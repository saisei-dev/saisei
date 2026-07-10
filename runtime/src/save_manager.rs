//! Port of `runtime/core/save_manager.c` — the gameplay save-state ring.
//!
//! A faithful, line-for-line translation of save_manager.c. Owns the
//! `saves/slot_N/` rotation ring, the manual Cmd+F1/Cmd+F2 save/load flow
//! (re-exec via `execv` for --restore-from), and the human-readable
//! `save_restore.log`. All shim state is read through the accessors declared in
//! shims.h; the on-disk snapshot format is owned by snapshot.rs via
//! `snapshot_capture_and_write()`.
//!
//! NOTE ON `save_manager_sr_log`: the C symbol is variadic
//! (`void save_manager_sr_log(const char*, ...)`). Defining a C-variadic
//! function needs the unstable `c_variadic` feature, which is unavailable on the
//! pinned stable toolchain. In-crate log sites (this file + snapshot.rs) format
//! byte-identically via the private `sr_log!` macro, which calls C `fprintf`
//! directly (equivalent to the original's `vfprintf`). The exported non-variadic
//! symbol writes its single string argument verbatim (used by the still-C
//! callers' single-string / exit-line logging).

#![allow(non_upper_case_globals)]
#![allow(static_mut_refs)]
// The `sr_log!` macro wraps its `libc::fprintf` in `unsafe` so it can be used
// from safe contexts; some call sites already sit inside an `unsafe` block.
#![allow(unused_unsafe)]

use crate::cpu::{cs, ip};
use core::ffi::{c_char, c_int, c_uint, c_ulonglong, c_void};

// ---- shims.c-owned globals / functions save_manager.c touches --------------

extern "C" {
    static lcall_depth: u8;
    static isr_depth: u8;
    static mut stderr: *mut libc::FILE;

    fn shim_pc_is_case_key(module: *const c_char, file_off: u32) -> c_int;
    fn shim_pc_is_jit_case_key(seg: u16, off: u16) -> c_int;
    fn shim_active_binary() -> *const c_char;
}

const SLOT_COUNT: usize = 5;

/* Target ages per slot in microseconds. slot_0 is the freshest; slot_4
 * caps at ~3 minutes back. Promotion rule: slot[i-1] is moved to slot[i]
 * once its age reaches SLOT_TARGET_US[i]. */
static SLOT_TARGET_US: [u64; SLOT_COUNT] = [
    5u64 * 1000000,   /* slot_0:  ~5s    */
    15u64 * 1000000,  /* slot_1:  ~15s   */
    45u64 * 1000000,  /* slot_2:  ~45s   */
    135u64 * 1000000, /* slot_3:  ~2.25 min */
    180u64 * 1000000, /* slot_4:  3 min cap */
];

#[derive(Clone, Copy)]
struct SlotInfo {
    write_us: u64, /* monotonic timestamp (this process's clock) */
    valid: c_int,
}

static mut SLOTS: [SlotInfo; SLOT_COUNT] = [SlotInfo {
    write_us: 0,
    valid: 0,
}; SLOT_COUNT];
static mut INITIALIZED: c_int = 0;
static mut INITIAL_CASCADE_PENDING: c_int = 0; /* one-shot shift on first save */

#[inline(always)]
fn errno() -> c_int {
    unsafe { *libc::__errno_location() }
}

#[inline(always)]
fn saves_root() -> *const c_char {
    c"saves".as_ptr()
}

// ---- slots[] accessors (static-mut safe place ops) -------------------------

#[inline(always)]
unsafe fn slot_valid(i: usize) -> c_int {
    core::ptr::addr_of!(SLOTS[i].valid).read()
}
#[inline(always)]
unsafe fn slot_write_us(i: usize) -> u64 {
    core::ptr::addr_of!(SLOTS[i].write_us).read()
}
#[inline(always)]
unsafe fn set_slot_valid(i: usize, v: c_int) {
    core::ptr::addr_of_mut!(SLOTS[i].valid).write(v);
}
#[inline(always)]
unsafe fn set_slot_write_us(i: usize, v: u64) {
    core::ptr::addr_of_mut!(SLOTS[i].write_us).write(v);
}
#[inline(always)]
unsafe fn slot_copy(dst: usize, src: usize) {
    let v = core::ptr::addr_of!(SLOTS[src]).read();
    core::ptr::addr_of_mut!(SLOTS[dst]).write(v);
}

// ---- save_restore.log helpers ----------------------------------------------

/* Human-readable activity log. Appended from save_manager (save events,
 * rotations), snapshot.rs (restore events), and the source (binary
 * start/exit lines). One file at runtime_dir/save_restore.log;
 * readable by `tail -f`. Each line: ISO-timestamp + event + key=value
 * details. */
pub(crate) fn sr_log_begin() -> *mut libc::FILE {
    let fp = unsafe { libc::fopen(c"save_restore.log".as_ptr(), c"a".as_ptr()) };
    if fp.is_null() {
        return fp;
    }
    let now: libc::time_t = unsafe { libc::time(core::ptr::null_mut()) };
    let mut tm_: libc::tm = unsafe { core::mem::zeroed() };
    unsafe {
        libc::gmtime_r(&now, &mut tm_);
        libc::fprintf(
            fp,
            c"[%04d-%02d-%02dT%02d:%02d:%02dZ] ".as_ptr(),
            tm_.tm_year + 1900,
            tm_.tm_mon + 1,
            tm_.tm_mday,
            tm_.tm_hour,
            tm_.tm_min,
            tm_.tm_sec,
        );
    }
    fp
}

pub(crate) fn sr_log_end(fp: *mut libc::FILE) {
    unsafe {
        libc::fputc(b'\n' as c_int, fp);
        libc::fclose(fp);
    }
}

/// In-crate formatted logging: byte-identical to the C `vfprintf` path.
macro_rules! sr_log {
    ($($arg:tt)*) => {{
        let __fp = $crate::save_manager::sr_log_begin();
        if !__fp.is_null() {
            unsafe { libc::fprintf(__fp, $($arg)*); }
            $crate::save_manager::sr_log_end(__fp);
        }
    }};
}
pub(crate) use sr_log;

/// Exported C ABI. See the module note: the still-C callers pass varargs we
/// cannot consume on stable Rust, so this writes the format string verbatim.
#[no_mangle]
pub extern "C" fn save_manager_sr_log(event_fmt: *const c_char) {
    let fp = sr_log_begin();
    if fp.is_null() {
        return;
    }
    unsafe {
        libc::fputs(event_fmt, fp);
    }
    sr_log_end(fp);
}

// ---- monotonic clock -------------------------------------------------------

static mut START_TS: libc::timespec = libc::timespec {
    tv_sec: 0,
    tv_nsec: 0,
};

fn now_us() -> u64 {
    let mut t: libc::timespec = unsafe { core::mem::zeroed() };
    unsafe {
        libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut t);
        let s = (t.tv_sec - START_TS.tv_sec) as u64;
        let ns = t.tv_nsec as i64 - START_TS.tv_nsec as i64;
        s.wrapping_mul(1000000).wrapping_add((ns / 1000) as u64)
    }
}

// ---- filesystem helpers ----------------------------------------------------

#[inline(always)]
fn s_isdir(mode: libc::mode_t) -> bool {
    (mode & libc::S_IFMT) == libc::S_IFDIR
}

fn rmtree(path: *const c_char) -> c_int {
    let d = unsafe { libc::opendir(path) };
    if d.is_null() {
        return if errno() == libc::ENOENT { 0 } else { -1 };
    }
    let mut child = [0 as c_char; 512];
    unsafe {
        loop {
            let e = libc::readdir(d);
            if e.is_null() {
                break;
            }
            let name = core::ptr::addr_of!((*e).d_name) as *const c_char;
            if libc::strcmp(name, c".".as_ptr()) == 0 || libc::strcmp(name, c"..".as_ptr()) == 0 {
                continue;
            }
            libc::snprintf(child.as_mut_ptr(), 512, c"%s/%s".as_ptr(), path, name);
            let mut st: libc::stat = core::mem::zeroed();
            if libc::lstat(child.as_ptr(), &mut st) == 0 && s_isdir(st.st_mode) {
                rmtree(child.as_ptr());
            } else {
                libc::unlink(child.as_ptr());
            }
        }
        libc::closedir(d);
        libc::rmdir(path)
    }
}

fn slot_dir(i: c_int, out: *mut c_char, cap: usize) {
    unsafe {
        libc::snprintf(out, cap, c"%s/slot_%d".as_ptr(), saves_root(), i);
    }
}

fn write_meta(dir: *const c_char, elapsed_us: u64, slot: c_int, reason: *const c_char) {
    let mut path = [0 as c_char; 512];
    unsafe {
        libc::snprintf(path.as_mut_ptr(), 512, c"%s/meta.json".as_ptr(), dir);
        let fd = libc::open(
            path.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_TRUNC | libc::O_CLOEXEC,
            0o644 as c_int,
        );
        if fd < 0 {
            return;
        }
        let mut buf = [0 as c_char; 320];
        let now: libc::time_t = libc::time(core::ptr::null_mut());
        let mut tm_: libc::tm = core::mem::zeroed();
        libc::gmtime_r(&now, &mut tm_);
        let n = libc::snprintf(
            buf.as_mut_ptr(),
            320,
            c"{\n  \"slot\": %d,\n  \"elapsed_us\": %llu,\n  \"written_at_utc\": \"%04d-%02d-%02dT%02d:%02d:%02dZ\",\n  \"trigger\": \"%s\"\n}\n".as_ptr(),
            slot,
            elapsed_us as c_ulonglong,
            tm_.tm_year + 1900,
            tm_.tm_mon + 1,
            tm_.tm_mday,
            tm_.tm_hour,
            tm_.tm_min,
            tm_.tm_sec,
            if reason.is_null() { c"".as_ptr() } else { reason },
        );
        if n > 0 {
            let _ = libc::write(fd, buf.as_ptr() as *const c_void, n as usize);
        }
        libc::close(fd);
    }
}

/* Walk saves/ and reconstruct slots[] from disk. Inherited slots get
 * write_us = 0 (their effective age starts at "process start"); if any
 * exist, the first save in this process force-cascades them down by one
 * position to preserve their relative order, sacrificing only slot_4. */
fn rediscover_slots() {
    let mut any: c_int = 0;
    for i in 0..SLOT_COUNT {
        let mut d = [0 as c_char; 256];
        slot_dir(i as c_int, d.as_mut_ptr(), 256);
        let mut probe = [0 as c_char; 512];
        unsafe {
            libc::snprintf(
                probe.as_mut_ptr(),
                512,
                c"%s/pre_last_key.bin".as_ptr(),
                d.as_ptr(),
            );
            let mut st: libc::stat = core::mem::zeroed();
            if libc::stat(probe.as_ptr(), &mut st) != 0 {
                set_slot_valid(i, 0);
                continue;
            }
            set_slot_valid(i, 1);
            set_slot_write_us(i, 0);
            any = 1;
        }
    }
    unsafe {
        INITIAL_CASCADE_PENDING = any;
    }
}

/* Check whether the active binary's dispatch has a SINGLE case keyed at
 * `ip` (treated as a 32-bit file_offset). Refuse overlay collisions where
 * the same low-16 maps to multiple case keys. */
fn ip_is_case_key(module: *const c_char, ip_val: u16) -> c_int {
    if module.is_null() {
        return 0;
    }
    if unsafe { shim_pc_is_case_key(module, ip_val as u32) } == 0 {
        return 0;
    }
    let mut h: u32 = 1;
    while h <= 0x10 {
        if unsafe { shim_pc_is_case_key(module, (h << 16) | (ip_val as u32)) } != 0 {
            return 0; /* overlay collision — can't restore unambiguously */
        }
        h += 1;
    }
    1
}

#[no_mangle]
pub extern "C" fn save_manager_init() {
    unsafe {
        if INITIALIZED != 0 {
            return;
        }
        INITIALIZED = 1;
        libc::mkdir(saves_root(), 0o755);
        libc::clock_gettime(libc::CLOCK_MONOTONIC, core::ptr::addr_of_mut!(START_TS));
    }
    rediscover_slots();
}

/* Walk oldest → newest, promote slot[i-1] → slot[i] when the destination is
 * empty and the source has aged past its OWN target. Iterating high → low. */
fn rotate(now: u64) {
    for i in (1..SLOT_COUNT).rev() {
        unsafe {
            if slot_valid(i) != 0 {
                continue;
            } /* destination occupied */
            if slot_valid(i - 1) == 0 {
                continue;
            } /* source empty */
            let age = now.wrapping_sub(slot_write_us(i - 1));
            if age < SLOT_TARGET_US[i - 1] {
                continue;
            } /* source not ripe */

            let mut src = [0 as c_char; 256];
            let mut dst = [0 as c_char; 256];
            slot_dir((i - 1) as c_int, src.as_mut_ptr(), 256);
            slot_dir(i as c_int, dst.as_mut_ptr(), 256);
            rmtree(dst.as_ptr());
            if libc::rename(src.as_ptr(), dst.as_ptr()) == 0 {
                slot_copy(i, i - 1);
                set_slot_valid(i - 1, 0);
            }
        }
    }
}

fn can_save_now() -> c_int {
    /* refuse nonzero lcall/isr depths */
    if unsafe { lcall_depth } != 0 || unsafe { isr_depth } != 0 {
        return 0;
    }
    /* JIT resting point: ip is a case-key in a live JIT chunk at the current cs. */
    if unsafe { shim_pc_is_jit_case_key(cs(), ip()) } != 0 {
        return 1;
    }
    /* Static-dispatch path: require a named active binary and a case-key ip. */
    let binary = unsafe { shim_active_binary() };
    if binary.is_null() {
        return 0;
    }
    if ip_is_case_key(binary, ip()) == 0 {
        return 0;
    }
    1
}

/* One-shot full shift-down used on the first save after rediscovery. */
fn initial_cascade() {
    for i in (1..SLOT_COUNT).rev() {
        unsafe {
            if slot_valid(i - 1) == 0 {
                continue;
            }
            let mut src = [0 as c_char; 256];
            let mut dst = [0 as c_char; 256];
            slot_dir((i - 1) as c_int, src.as_mut_ptr(), 256);
            slot_dir(i as c_int, dst.as_mut_ptr(), 256);
            rmtree(dst.as_ptr());
            if libc::rename(src.as_ptr(), dst.as_ptr()) == 0 {
                slot_copy(i, i - 1);
                set_slot_valid(i - 1, 0);
            }
        }
    }
}

fn try_save(reason: *const c_char, bypass_throttle: c_int) {
    if unsafe { INITIALIZED } == 0 {
        save_manager_init();
    }
    if can_save_now() == 0 {
        let binary = unsafe { shim_active_binary() };
        if !binary.is_null() {
            let why: *const c_char = if unsafe { lcall_depth } != 0 || unsafe { isr_depth } != 0 {
                c"depth".as_ptr()
            } else if unsafe { shim_pc_is_case_key(binary, ip() as u32) } == 0 {
                c"ip_not_case_key".as_ptr()
            } else if ip_is_case_key(binary, ip()) == 0 {
                /* overlay collision */
                c"ip_ambiguous_overlay".as_ptr()
            } else {
                c"unknown".as_ptr()
            };
            sr_log!(
                c"save SKIP reason=%s trigger=%s lcall_d=%u isr_d=%u active=%s cs:ip=%04X:%04X"
                    .as_ptr(),
                why,
                reason,
                unsafe { lcall_depth } as c_uint,
                unsafe { isr_depth } as c_uint,
                binary,
                cs() as c_uint,
                ip() as c_uint
            );
        }
        return;
    }

    let now = now_us();
    let ref_us = if unsafe { slot_valid(0) } != 0 {
        unsafe { slot_write_us(0) }
    } else {
        0
    };
    if bypass_throttle == 0 && now.wrapping_sub(ref_us) < SLOT_TARGET_US[0] {
        return; /* throttled, silent */
    }

    if unsafe { INITIAL_CASCADE_PENDING } != 0 {
        initial_cascade();
        unsafe {
            INITIAL_CASCADE_PENDING = 0;
        }
        sr_log!(c"save ROTATE initial_cascade (inherited ring shifted down)".as_ptr());
    } else {
        /* Track which rotations actually happened for the log. */
        let mut before_valid = [0 as c_int; SLOT_COUNT];
        for i in 0..SLOT_COUNT {
            before_valid[i] = unsafe { slot_valid(i) };
        }
        rotate(now);
        for i in 1..SLOT_COUNT {
            if unsafe { slot_valid(i) } != 0 && before_valid[i] == 0 {
                sr_log!(
                    c"save ROTATE slot_%d -> slot_%d (slot_%d aged past %llu us)".as_ptr(),
                    (i - 1) as c_int,
                    i as c_int,
                    (i - 1) as c_int,
                    SLOT_TARGET_US[i - 1] as c_ulonglong
                );
            }
        }
    }

    let mut tmp = [0 as c_char; 256];
    let mut dst = [0 as c_char; 256];
    unsafe {
        libc::snprintf(
            tmp.as_mut_ptr(),
            256,
            c"%s/slot_0_tmp".as_ptr(),
            saves_root(),
        );
    }
    slot_dir(0, dst.as_mut_ptr(), 256);

    rmtree(tmp.as_ptr());
    if unsafe { libc::mkdir(tmp.as_ptr(), 0o755) } != 0 && errno() != libc::EEXIST {
        let e = errno();
        sr_log!(c"save FAIL slot_0 reason=mkdir errno=%d".as_ptr(), e);
        return;
    }

    if crate::snapshot::snapshot_capture_and_write(tmp.as_ptr()) != 0 {
        sr_log!(c"save FAIL slot_0 reason=capture".as_ptr());
        rmtree(tmp.as_ptr());
        return;
    }
    write_meta(tmp.as_ptr(), now, 0, reason);

    rmtree(dst.as_ptr());
    if unsafe { libc::rename(tmp.as_ptr(), dst.as_ptr()) } != 0 {
        let e = errno();
        sr_log!(c"save FAIL slot_0 reason=rename errno=%d".as_ptr(), e);
        rmtree(tmp.as_ptr());
        return;
    }

    unsafe {
        set_slot_write_us(0, now);
        set_slot_valid(0, 1);
    }
    let active = unsafe { shim_active_binary() };
    sr_log!(
        c"save OK slot_0 trigger=%s active=%s cs:ip=%04X:%04X elapsed_us=%llu".as_ptr(),
        reason,
        if active.is_null() {
            c"<none>".as_ptr()
        } else {
            active
        },
        cs() as c_uint,
        ip() as c_uint,
        now as c_ulonglong
    );
}

#[no_mangle]
pub extern "C" fn save_manager_tick() {
    try_save(c"tick".as_ptr(), 0);
}

// ===== Manual user-driven save / load (Cmd+F1, Cmd+F2) =====

/* Save anchor — set whenever the game consumes a keyboard event. */
static mut SAVE_REQUEST_PENDING: c_int = 0;
static mut SAVE_ANCHOR_ARMED: c_int = 0;

#[no_mangle]
pub extern "C" fn save_manager_request_save() {
    if unsafe { SAVE_REQUEST_PENDING } == 0 {
        sr_log!(c"save REQUEST (manual via Cmd+F1; armed — waiting for next kbd-consumption anchor then a safe SAFEPOINT)".as_ptr());
    }
    unsafe {
        SAVE_REQUEST_PENDING = 1;
        SAVE_ANCHOR_ARMED = 0; /* require a fresh post-request kbd event */
    }
}

/* Wired from snapshot.rs::snapshot_on_key_consumed. Arms the save anchor. */
#[no_mangle]
pub extern "C" fn save_manager_on_key_consumed() {
    if unsafe { SAVE_REQUEST_PENDING } != 0 {
        unsafe {
            SAVE_ANCHOR_ARMED = 1;
        }
    }
}

#[no_mangle]
pub extern "C" fn save_manager_poll_pending() {
    if unsafe { SAVE_REQUEST_PENDING } == 0 {
        return;
    }
    if unsafe { SAVE_ANCHOR_ARMED } == 0 {
        return;
    }
    if unsafe { INITIALIZED } == 0 {
        save_manager_init();
    }
    /* Snapshot slot_0's write_us before so we can detect whether try_save
     * actually wrote a slot (vs. silently skipping). */
    let before_us = if unsafe { slot_valid(0) } != 0 {
        unsafe { slot_write_us(0) }
    } else {
        0
    };
    let before_valid = unsafe { slot_valid(0) };
    try_save(c"manual".as_ptr(), 1);
    if unsafe { slot_valid(0) } != 0
        && (before_valid == 0 || unsafe { slot_write_us(0) } != before_us)
    {
        unsafe {
            SAVE_REQUEST_PENDING = 0;
            SAVE_ANCHOR_ARMED = 0;
        }
    }
}

/* Find the freshest valid slot dir. Prefers slot_0; otherwise the
 * lowest-numbered slot that has a pre_last_key.bin on disk. */
fn find_latest_slot_dir(out: *mut c_char, cap: usize) -> c_int {
    for i in 0..SLOT_COUNT {
        let mut d = [0 as c_char; 256];
        slot_dir(i as c_int, d.as_mut_ptr(), 256);
        let mut probe = [0 as c_char; 512];
        unsafe {
            libc::snprintf(
                probe.as_mut_ptr(),
                512,
                c"%s/pre_last_key.bin".as_ptr(),
                d.as_ptr(),
            );
            let mut st: libc::stat = core::mem::zeroed();
            if libc::stat(probe.as_ptr(), &mut st) == 0 {
                libc::snprintf(out, cap, c"%s".as_ptr(), d.as_ptr());
                return 0;
            }
        }
    }
    -1
}

/* Slurp /proc/self/cmdline (NUL-separated argv) into a heap array.
 * Caller frees both the strings and the returned argv. */
fn read_proc_cmdline(argc_out: *mut c_int) -> *mut *mut c_char {
    unsafe {
        *argc_out = 0;
        let fd = libc::open(
            c"/proc/self/cmdline".as_ptr(),
            libc::O_RDONLY | libc::O_CLOEXEC,
        );
        if fd < 0 {
            return core::ptr::null_mut();
        }
        let mut buf: *mut c_char = core::ptr::null_mut();
        let mut cap: usize = 0;
        let mut len: usize = 0;
        loop {
            if len + 4096 > cap {
                let ncap = if cap != 0 { cap * 2 } else { 4096 };
                let nbuf = libc::realloc(buf as *mut c_void, ncap) as *mut c_char;
                if nbuf.is_null() {
                    libc::free(buf as *mut c_void);
                    libc::close(fd);
                    return core::ptr::null_mut();
                }
                buf = nbuf;
                cap = ncap;
            }
            let r = libc::read(fd, buf.add(len) as *mut c_void, cap - len);
            if r < 0 {
                if errno() == libc::EINTR {
                    continue;
                }
                libc::free(buf as *mut c_void);
                libc::close(fd);
                return core::ptr::null_mut();
            }
            if r == 0 {
                break;
            }
            len += r as usize;
        }
        libc::close(fd);
        let mut n: c_int = 0;
        for i in 0..len {
            if *buf.add(i) == 0 {
                n += 1;
            }
        }
        if n == 0 {
            libc::free(buf as *mut c_void);
            return core::ptr::null_mut();
        }
        let argv =
            libc::calloc((n as usize) + 1, core::mem::size_of::<*mut c_char>()) as *mut *mut c_char;
        if argv.is_null() {
            libc::free(buf as *mut c_void);
            return core::ptr::null_mut();
        }
        let mut idx: c_int = 0;
        let mut start: usize = 0;
        let mut i: usize = 0;
        while i < len && idx < n {
            if *buf.add(i) == 0 {
                *argv.add(idx as usize) = libc::strdup(buf.add(start));
                idx += 1;
                start = i + 1;
            }
            i += 1;
        }
        libc::free(buf as *mut c_void);
        *argc_out = idx;
        argv
    }
}

#[no_mangle]
pub extern "C" fn save_manager_request_load_latest() {
    if unsafe { INITIALIZED } == 0 {
        save_manager_init();
    }
    let mut dir = [0 as c_char; 256];
    if find_latest_slot_dir(dir.as_mut_ptr(), 256) != 0 {
        sr_log!(
            c"load REQUEST FAIL reason=no_valid_slot (Cmd+F2 pressed with empty ring)".as_ptr()
        );
        unsafe {
            libc::fprintf(
                stderr,
                c"[Cmd+F2] load: no save slots exist yet. Press Cmd+F1 first.\n".as_ptr(),
            );
        }
        return;
    }
    /* Resolve to absolute so the re-execed binary doesn't depend on cwd. */
    let mut abs_dir = [0 as c_char; libc::PATH_MAX as usize];
    unsafe {
        if libc::realpath(dir.as_ptr(), abs_dir.as_mut_ptr()).is_null() {
            libc::snprintf(
                abs_dir.as_mut_ptr(),
                libc::PATH_MAX as usize,
                c"%s".as_ptr(),
                dir.as_ptr(),
            );
        }
    }

    let mut orig_argc: c_int = 0;
    let orig_argv = read_proc_cmdline(&mut orig_argc);

    /* Build new argv: orig argv with any existing --restore-from stripped,
     * then append --restore-from <abs_dir>. */
    let max_new = orig_argc + 3;
    let new_argv = unsafe { libc::calloc(max_new as usize, core::mem::size_of::<*mut c_char>()) }
        as *mut *mut c_char;
    if new_argv.is_null() {
        sr_log!(c"load REQUEST FAIL reason=oom".as_ptr());
        if !orig_argv.is_null() {
            unsafe {
                for i in 0..orig_argc {
                    libc::free(*orig_argv.add(i as usize) as *mut c_void);
                }
                libc::free(orig_argv as *mut c_void);
            }
        }
        return;
    }
    let mut n: c_int = 0;
    unsafe {
        if !orig_argv.is_null() && orig_argc > 0 {
            *new_argv.add(n as usize) = *orig_argv.add(0);
            n += 1;
            let mut i: c_int = 1;
            while i < orig_argc {
                let a = *orig_argv.add(i as usize);
                if libc::strcmp(a, c"--restore-from".as_ptr()) == 0 {
                    i += 1; /* skip its value too */
                    i += 1;
                    continue;
                }
                if libc::strncmp(a, c"--restore-from=".as_ptr(), 15) == 0 {
                    i += 1;
                    continue;
                }
                *new_argv.add(n as usize) = *orig_argv.add(i as usize);
                n += 1;
                i += 1;
            }
        } else {
            /* Fallback if /proc/self/cmdline was unavailable. */
            *new_argv.add(n as usize) = libc::strdup(c"program".as_ptr());
            n += 1;
        }
        *new_argv.add(n as usize) = libc::strdup(c"--restore-from".as_ptr());
        n += 1;
        *new_argv.add(n as usize) = libc::strdup(abs_dir.as_ptr());
        n += 1;
        *new_argv.add(n as usize) = core::ptr::null_mut();
    }

    sr_log!(
        c"load REQUEST OK bundle=%s (Cmd+F2 re-exec /proc/self/exe argc=%d)".as_ptr(),
        abs_dir.as_ptr(),
        n
    );
    unsafe {
        libc::fprintf(
            stderr,
            c"[Cmd+F2] load: re-exec'ing with --restore-from %s\n".as_ptr(),
            abs_dir.as_ptr(),
        );
        libc::fflush(stderr);

        libc::execv(c"/proc/self/exe".as_ptr(), new_argv as *const *const c_char);
        /* execv only returns on failure. */
        let err = errno();
        sr_log!(
            c"load REQUEST FAIL reason=execv errno=%d (%s)".as_ptr(),
            err,
            libc::strerror(err)
        );
        libc::fprintf(
            stderr,
            c"[Cmd+F2] load: execv failed: %s\n".as_ptr(),
            libc::strerror(err),
        );
        /* Best-effort cleanup. */
        for i in 0..n {
            libc::free(*new_argv.add(i as usize) as *mut c_void);
        }
        libc::free(new_argv as *mut c_void);
        libc::free(orig_argv as *mut c_void);
    }
}
