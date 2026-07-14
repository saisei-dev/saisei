//! Port of `runtime/core/snapshot.c` — the SDL-tier save-state layer.
//!
//! A faithful, line-for-line translation of snapshot.c. Sits on top of the
//! shims runtime and owns keys.log, the pre_last_key.* snapshots, the atexit
//! dump into last_exit_snapshot/, and the --restore-from resume path. It never
//! pokes at shims static globals — capture/restore goes through the
//! shim_kbd_state_* / shim_file_mappings_* accessors declared in shims.h.
//!
//! Formatted logging into save_restore.log goes through save_manager's
//! `sr_log!` macro (byte-identical to the original's variadic
//! save_manager_sr_log; see the note in save_manager.rs).

#![allow(non_upper_case_globals)]
#![allow(non_snake_case)]
#![allow(static_mut_refs)]
// The `sr_log!` macro wraps its `libc::fprintf` in `unsafe`; several restore
// call sites already sit inside an `unsafe` block.
#![allow(unused_unsafe)]

use crate::cpu::*;
use crate::save_manager::sr_log;
use core::ffi::{c_char, c_int, c_uint, c_ulonglong, c_void};

// ============================================================================
// The structs the snapshot serializes.
//
// These are shims' own types, used directly — NOT re-declared here. snapshot.c
// mirrored them because it was a separate translation unit reading shims.h; in
// one Rust crate a mirror buys nothing and costs correctness. It drifted, and
// silently: `shim_runtime_state_capture` writes shims' struct through a `*mut`
// into a buffer this module sized from its own copy, so when shims' `VgaState`
// grew a Sequencer register file (the EGA is a chip, not a byte array) and the
// mirror did not, the write ran 16 bytes off the end of the buffer and the read
// took 16 bytes of stack garbage back — landing in the tail of `irq_pending[]`,
// which is a table of *interrupt vectors to deliver*. A restored game raised a
// vector nobody had scheduled (0xFA), the IVT sent it to the unhandled-interrupt
// ISR, and every save died on load. One definition, so it cannot happen again.
// ============================================================================
use crate::shims::{
    BiosVideoState, CgaState, Opl2State, PITState, ShimFileMappingView, ShimKbdState,
    ShimRuntimeState, VgaState, SHIM_KBD_BUFFER_SIZE,
};

// ---- shims.h: ShimDispatchFn -----------------------------------------------
type ShimDispatchFn = Option<
    extern "C" fn(
        pc: c_int,
        expected_retip: u16,
        file: *const c_char,
        func: *const c_char,
        line: c_int,
    ),
>;

// ---- snapshot.c file-local SnapCPU (fwrite'd as pre_last_key.state.bin) -----
#[repr(C)]
struct SnapCpu {
    r_cs: u16,
    r_ip: u16,
    r_ss: u16,
    r_sp: u16,
    r_ds: u16,
    r_es: u16,
    r_ax: u16,
    r_bx: u16,
    r_cx: u16,
    r_dx: u16,
    r_si: u16,
    r_di: u16,
    r_bp: u16,
    f_CF: u8,
    f_PF: u8,
    f_ZF: u8,
    f_SF: u8,
    f_OF: u8,
    f_IF: u8,
    f_DF: u8,
    lcall_d: u16,
    isr_d: u16,
    last_scancode: u8,
    elapsed_us: u64,
    kbd: ShimKbdState,
    /* Top of the active-binary stack at capture time. Fixed-size so the
     * layout is stable on disk. */
    active_binary: [c_char; 16],
}

// The on-disk layout, pinned. These are the sizes the snapshot files are written
// at, so a struct that changes shape trips this at COMPILE time — which is the
// point: the failure it replaces was silent. Growing one of these is allowed, but
// it is a format change: update the number here AND bump
// `SHIM_RUNTIME_STATE_VERSION`, so a bundle written by an older binary is refused
// on sight instead of being reinterpreted through the new shape.
const _: () = assert!(core::mem::size_of::<VgaState>() == 820);
const _: () = assert!(core::mem::size_of::<CgaState>() == 44);
const _: () = assert!(core::mem::size_of::<BiosVideoState>() == 28);
const _: () = assert!(core::mem::size_of::<Opl2State>() == 288);
const _: () = assert!(core::mem::size_of::<PITState>() == 8);
const _: () = assert!(core::mem::size_of::<ShimRuntimeState>() == 1528);
const _: () = assert!(core::mem::size_of::<ShimKbdState>() == 144);
const _: () = assert!(core::mem::size_of::<ShimFileMappingView>() == 40);
const _: () = assert!(core::mem::size_of::<SnapCpu>() == 208);

// ============================================================================
// extern: shims.c-owned globals + functions snapshot.c calls.
// ============================================================================

extern "C" {
    static mut virtual_memory: *mut u8;
    static SHIM_MEMORY_SIZE: usize;
    static lcall_depth: u8;
    static isr_depth: u8;
    static mut stderr: *mut libc::FILE;

    fn shim_stdout_logging_enabled() -> c_int;
    fn shim_kbd_state_capture(out: *mut ShimKbdState);
    fn shim_kbd_state_restore(inp: *const ShimKbdState);
    fn shim_active_binary() -> *const c_char;
    fn shim_crash_bundle_write_file(
        dir: *const c_char,
        name: *const c_char,
        contents: *const c_char,
        len: usize,
    );
    fn shim_crash_bundle_write_state(dir: *const c_char);
    fn shim_crash_bundle_write_trace_tail(dir: *const c_char);
    fn shim_lifecycle_dump_to_dir(dir: *const c_char);
    fn shim_runtime_state_capture(out: *mut ShimRuntimeState);
    fn shim_runtime_state_restore(inp: *const ShimRuntimeState) -> c_int;
    fn shim_file_mappings_count() -> usize;
    fn shim_file_mappings_get(i: usize, out: *mut ShimFileMappingView);
    fn shim_file_mappings_reset();
    fn shim_file_mappings_add_for_restore(
        path: *const c_char,
        base: u32,
        len: usize,
        file_offset: usize,
        canonical_cs: u16,
    ) -> c_int;
    fn shim_set_bundle_extra_writer(f: Option<extern "C" fn(*const c_char)>);
    fn shim_pc_is_case_key(module: *const c_char, file_off: u32) -> c_int;
    fn shim_dispatch_fn_by_module(module: *const c_char) -> ShimDispatchFn;
    fn shim_enter_binary(name: *const c_char);
    fn shim_leave_binary();
    fn shim_drain_pending_tail_dispatch(file: *const c_char, func: *const c_char, line: c_int);
    fn run_machine();
}

#[inline(always)]
fn errno() -> c_int {
    unsafe { *libc::__errno_location() }
}
#[inline(always)]
fn shim_mem_size() -> usize {
    unsafe { SHIM_MEMORY_SIZE }
}

// ============================================================================
// Elapsed-microseconds clock
// ============================================================================

static mut PROC_START_TS: libc::timespec = libc::timespec {
    tv_sec: 0,
    tv_nsec: 0,
};
static mut PROC_START_TS_SET: c_int = 0;

fn snapshot_elapsed_us() -> u64 {
    let mut now: libc::timespec = unsafe { core::mem::zeroed() };
    unsafe {
        libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut now);
        if PROC_START_TS_SET == 0 {
            PROC_START_TS = now;
            PROC_START_TS_SET = 1;
            return 0;
        }
        let s = (now.tv_sec - PROC_START_TS.tv_sec) as u64;
        let ns = now.tv_nsec as i64 - PROC_START_TS.tv_nsec as i64;
        s.wrapping_mul(1000000).wrapping_add((ns / 1000) as u64)
    }
}

// ============================================================================
// Key event log
// ============================================================================

const KEY_EVENT_LOG_CAP: usize = 4096;

#[derive(Clone, Copy)]
struct KeyEvent {
    elapsed_us: u64,
    held_us: u32,
    action: c_char, /* 'P' or 'R' */
    scancode: u8,
}

const KEYEVENT_INIT: KeyEvent = KeyEvent {
    elapsed_us: 0,
    held_us: 0,
    action: 0,
    scancode: 0,
};

static mut KEY_EVENT_LOG: [KeyEvent; KEY_EVENT_LOG_CAP] = [KEYEVENT_INIT; KEY_EVENT_LOG_CAP];
static mut KEY_EVENT_LOG_N: usize = 0;
static mut KBD_PRESS_TS_US: [u64; 128] = [0; 128];

#[no_mangle]
pub extern "C" fn snapshot_on_key_event(action: c_char, scancode: u8) {
    let t = snapshot_elapsed_us();
    let mut held: u32 = 0;
    let sc7 = (scancode & 0x7F) as usize;
    unsafe {
        if action == b'P' as c_char {
            KBD_PRESS_TS_US[sc7] = t;
        } else if action == b'R' as c_char && KBD_PRESS_TS_US[sc7] != 0 {
            held = t.wrapping_sub(KBD_PRESS_TS_US[sc7]) as u32;
            KBD_PRESS_TS_US[sc7] = 0;
        }
        if KEY_EVENT_LOG_N >= KEY_EVENT_LOG_CAP {
            /* Drop oldest half, keep tail. */
            libc::memmove(
                core::ptr::addr_of_mut!(KEY_EVENT_LOG[0]) as *mut c_void,
                core::ptr::addr_of!(KEY_EVENT_LOG[KEY_EVENT_LOG_CAP / 2]) as *const c_void,
                (KEY_EVENT_LOG_CAP / 2) * core::mem::size_of::<KeyEvent>(),
            );
            KEY_EVENT_LOG_N = KEY_EVENT_LOG_CAP / 2;
        }
        let idx = KEY_EVENT_LOG_N;
        core::ptr::addr_of_mut!(KEY_EVENT_LOG[idx].elapsed_us).write(t);
        core::ptr::addr_of_mut!(KEY_EVENT_LOG[idx].held_us).write(held);
        core::ptr::addr_of_mut!(KEY_EVENT_LOG[idx].action).write(action);
        core::ptr::addr_of_mut!(KEY_EVENT_LOG[idx].scancode).write(sc7 as u8);
        KEY_EVENT_LOG_N += 1;
    }
}

fn write_keys_log(dir: *const c_char) {
    unsafe {
        if KEY_EVENT_LOG_N == 0 {
            return;
        }
        let mut path = [0 as c_char; 320];
        libc::snprintf(path.as_mut_ptr(), 320, c"%s/keys.log".as_ptr(), dir);
        let fd = libc::open(
            path.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_TRUNC | libc::O_CLOEXEC,
            0o644 as c_int,
        );
        if fd < 0 {
            return;
        }
        const HEADER: &[u8] = b"# columns: elapsed_us action scancode held_us\n# action: P=press R=release; scancode is 7-bit DOS make code\n";
        let _ = libc::write(fd, HEADER.as_ptr() as *const c_void, HEADER.len());
        let mut line = [0 as c_char; 64];
        for i in 0..KEY_EVENT_LOG_N {
            let ev_elapsed = core::ptr::addr_of!(KEY_EVENT_LOG[i].elapsed_us).read();
            let ev_action = core::ptr::addr_of!(KEY_EVENT_LOG[i].action).read();
            let ev_scancode = core::ptr::addr_of!(KEY_EVENT_LOG[i].scancode).read();
            let ev_held = core::ptr::addr_of!(KEY_EVENT_LOG[i].held_us).read();
            let action_or_q: c_int = if ev_action != 0 {
                ev_action as c_int
            } else {
                '?' as c_int
            };
            let n = libc::snprintf(
                line.as_mut_ptr(),
                64,
                c"%llu %c 0x%02X %u\n".as_ptr(),
                ev_elapsed as c_ulonglong,
                action_or_q,
                ev_scancode as c_uint,
                ev_held as c_uint,
            );
            if n > 0 {
                let _ = libc::write(fd, line.as_ptr() as *const c_void, n as usize);
            }
        }
        libc::close(fd);
    }
}

// ============================================================================
// Pre-key snapshot
// ============================================================================

const SHIMKBD_INIT: ShimKbdState = ShimKbdState {
    q_ascii: [0; SHIM_KBD_BUFFER_SIZE],
    q_scan: [0; SHIM_KBD_BUFFER_SIZE],
    head: 0,
    tail: 0,
    count: 0,
    cur_ascii: 0,
    cur_scan: 0,
    last_scan: 0,
    ready: 0,
};

const SNAPCPU_INIT: SnapCpu = SnapCpu {
    r_cs: 0,
    r_ip: 0,
    r_ss: 0,
    r_sp: 0,
    r_ds: 0,
    r_es: 0,
    r_ax: 0,
    r_bx: 0,
    r_cx: 0,
    r_dx: 0,
    r_si: 0,
    r_di: 0,
    r_bp: 0,
    f_CF: 0,
    f_PF: 0,
    f_ZF: 0,
    f_SF: 0,
    f_OF: 0,
    f_IF: 0,
    f_DF: 0,
    lcall_d: 0,
    isr_d: 0,
    last_scancode: 0,
    elapsed_us: 0,
    kbd: SHIMKBD_INIT,
    active_binary: [0; 16],
};

static mut SNAP_MEMORY: *mut u8 = core::ptr::null_mut();
static mut SNAP_CPU: SnapCpu = SNAPCPU_INIT;
static mut SNAP_PRESENT: c_int = 0;

/* Capture current runtime state into snap_memory/snap_cpu. */
fn capture_into_snap() -> c_int {
    unsafe {
        if SNAP_MEMORY.is_null() {
            SNAP_MEMORY = libc::malloc(shim_mem_size()) as *mut u8;
            if SNAP_MEMORY.is_null() {
                return -1;
            }
        }
        libc::memcpy(
            SNAP_MEMORY as *mut c_void,
            virtual_memory as *const c_void,
            shim_mem_size(),
        );
        SNAP_CPU.r_cs = cs();
        SNAP_CPU.r_ip = ip();
        SNAP_CPU.r_ss = ss();
        SNAP_CPU.r_sp = sp();
        SNAP_CPU.r_ds = ds();
        SNAP_CPU.r_es = es();
        SNAP_CPU.r_ax = ax();
        SNAP_CPU.r_bx = bx();
        SNAP_CPU.r_cx = cx();
        SNAP_CPU.r_dx = dx();
        SNAP_CPU.r_si = si();
        SNAP_CPU.r_di = di();
        SNAP_CPU.r_bp = bp();
        SNAP_CPU.f_CF = CF();
        SNAP_CPU.f_PF = PF();
        SNAP_CPU.f_ZF = ZF();
        SNAP_CPU.f_SF = SF();
        SNAP_CPU.f_OF = OF();
        SNAP_CPU.f_IF = IF();
        SNAP_CPU.f_DF = DF();
        SNAP_CPU.lcall_d = lcall_depth as u16;
        SNAP_CPU.isr_d = isr_depth as u16;
        shim_kbd_state_capture(core::ptr::addr_of_mut!(SNAP_CPU.kbd));
        SNAP_CPU.last_scancode = SNAP_CPU.kbd.last_scan;
        SNAP_CPU.elapsed_us = snapshot_elapsed_us();
        let binary = shim_active_binary();
        libc::memset(
            core::ptr::addr_of_mut!(SNAP_CPU.active_binary) as *mut c_void,
            0,
            core::mem::size_of_val(&SNAP_CPU.active_binary),
        );
        if !binary.is_null() {
            libc::strncpy(
                core::ptr::addr_of_mut!(SNAP_CPU.active_binary) as *mut c_char,
                binary,
                core::mem::size_of_val(&SNAP_CPU.active_binary) - 1,
            );
        }
        SNAP_PRESENT = 1;
    }
    0
}

#[no_mangle]
pub extern "C" fn snapshot_on_key_consumed() {
    /* Capture into the in-memory snap so crash bundles get a pre_last_key.*
     * snapshot for forensics. */
    let _ = capture_into_snap();
    /* Arm the gameplay-save anchor. */
    crate::save_manager::save_manager_on_key_consumed();
}

fn write_pre_key_snapshot(dir: *const c_char) {
    unsafe {
        if SNAP_PRESENT == 0 || SNAP_MEMORY.is_null() {
            return;
        }
        let mut path = [0 as c_char; 320];

        /* 1. raw memory */
        libc::snprintf(path.as_mut_ptr(), 320, c"%s/pre_last_key.bin".as_ptr(), dir);
        let mut fd = libc::open(
            path.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_TRUNC | libc::O_CLOEXEC,
            0o644 as c_int,
        );
        if fd >= 0 {
            let msize = shim_mem_size();
            let mut off: usize = 0;
            while off < msize {
                let w = libc::write(fd, SNAP_MEMORY.add(off) as *const c_void, msize - off);
                if w < 0 {
                    if errno() == libc::EINTR {
                        continue;
                    }
                    break;
                }
                off += w as usize;
            }
            libc::close(fd);
        }

        /* 2. JSON (human-readable) */
        let mut buf = [0 as c_char; 8192];
        let n = libc::snprintf(
            buf.as_mut_ptr(),
            8192,
            c"{\n  \"elapsed_us\": %llu,\n  \"last_scancode\": \"0x%02X\",\n  \"cpu\": {\n    \"cs\": \"0x%04X\", \"ip\": \"0x%04X\",\n    \"ss\": \"0x%04X\", \"sp\": \"0x%04X\",\n    \"ds\": \"0x%04X\", \"es\": \"0x%04X\",\n    \"ax\": \"0x%04X\", \"bx\": \"0x%04X\",\n    \"cx\": \"0x%04X\", \"dx\": \"0x%04X\",\n    \"si\": \"0x%04X\", \"di\": \"0x%04X\",\n    \"bp\": \"0x%04X\"\n  },\n  \"flags\": {\"CF\":%u,\"PF\":%u,\"ZF\":%u,\"SF\":%u,\"OF\":%u,\"IF\":%u,\"DF\":%u},\n  \"lcall_depth\": %u,\n  \"isr_depth\": %u,\n  \"memory_size\": %zu,\n  \"keyboard\": {\n    \"head\": %d, \"tail\": %d, \"count\": %d,\n    \"ascii\": \"0x%02X\", \"scancode\": \"0x%02X\",\n    \"last_scancode\": \"0x%02X\", \"ready\": %u\n  }\n}\n".as_ptr(),
            SNAP_CPU.elapsed_us as c_ulonglong,
            SNAP_CPU.last_scancode as c_uint,
            SNAP_CPU.r_cs as c_int, SNAP_CPU.r_ip as c_int, SNAP_CPU.r_ss as c_int, SNAP_CPU.r_sp as c_int,
            SNAP_CPU.r_ds as c_int, SNAP_CPU.r_es as c_int,
            SNAP_CPU.r_ax as c_int, SNAP_CPU.r_bx as c_int, SNAP_CPU.r_cx as c_int, SNAP_CPU.r_dx as c_int,
            SNAP_CPU.r_si as c_int, SNAP_CPU.r_di as c_int, SNAP_CPU.r_bp as c_int,
            SNAP_CPU.f_CF as c_uint, SNAP_CPU.f_PF as c_uint, SNAP_CPU.f_ZF as c_uint, SNAP_CPU.f_SF as c_uint,
            SNAP_CPU.f_OF as c_uint, SNAP_CPU.f_IF as c_uint, SNAP_CPU.f_DF as c_uint,
            SNAP_CPU.lcall_d as c_uint,
            SNAP_CPU.isr_d as c_uint,
            shim_mem_size(),
            SNAP_CPU.kbd.head, SNAP_CPU.kbd.tail, SNAP_CPU.kbd.count,
            SNAP_CPU.kbd.cur_ascii as c_uint,
            SNAP_CPU.kbd.cur_scan as c_uint,
            SNAP_CPU.kbd.last_scan as c_uint,
            SNAP_CPU.kbd.ready as c_uint,
        );
        if n > 0 && n < 8192 {
            shim_crash_bundle_write_file(
                dir,
                c"pre_last_key.json".as_ptr(),
                buf.as_ptr(),
                n as usize,
            );
        }

        /* 3. Binary mirror of snap_cpu for restore. */
        shim_crash_bundle_write_file(
            dir,
            c"pre_last_key.state.bin".as_ptr(),
            core::ptr::addr_of!(SNAP_CPU) as *const c_char,
            core::mem::size_of::<SnapCpu>(),
        );

        /* 3b. Shim runtime state. */
        let mut rt: ShimRuntimeState = core::mem::zeroed();
        shim_runtime_state_capture(&mut rt);
        shim_crash_bundle_write_file(
            dir,
            c"pre_last_key.shim_state.bin".as_ptr(),
            core::ptr::addr_of!(rt) as *const c_char,
            core::mem::size_of::<ShimRuntimeState>(),
        );

        /* 3c. Device state — the mouse, the PIC, PIT channels 1/2, port 61h/92h,
         * DOS, and the sound chips. ShimRuntimeState is a frozen v6 layout that
         * restore refuses on a version mismatch, so extending it would make every
         * save the player already has unloadable. The devices go in their own
         * tagged block file instead. */
        if !crate::devices::write_to_bundle(dir) {
            sr_log!(c"save: failed to write devices.bin\n".as_ptr());
        }

        /* 4. file_mappings TSV */
        libc::snprintf(
            path.as_mut_ptr(),
            320,
            c"%s/pre_last_key.maps.tsv".as_ptr(),
            dir,
        );
        fd = libc::open(
            path.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_TRUNC | libc::O_CLOEXEC,
            0o644 as c_int,
        );
        if fd >= 0 {
            const HDR: &[u8] = b"# base\tlen\tfile_offset\tcanonical_cs\tpath\n";
            let _ = libc::write(fd, HDR.as_ptr() as *const c_void, HDR.len());
            let mut line = [0 as c_char; 512];
            let m = shim_file_mappings_count();
            for i in 0..m {
                let mut v: ShimFileMappingView = core::mem::zeroed();
                shim_file_mappings_get(i, &mut v);
                let ln = libc::snprintf(
                    line.as_mut_ptr(),
                    512,
                    c"0x%X\t0x%zX\t0x%zX\t0x%04X\t%s\n".as_ptr(),
                    v.base as c_uint,
                    v.len,
                    v.file_offset,
                    v.canonical_cs as c_uint,
                    if v.path.is_null() {
                        c"".as_ptr()
                    } else {
                        v.path
                    },
                );
                if ln > 0 {
                    let _ = libc::write(fd, line.as_ptr() as *const c_void, ln as usize);
                }
            }
            libc::close(fd);
        }
    }
}

#[no_mangle]
pub extern "C" fn snapshot_write_to_bundle(dir: *const c_char) {
    write_keys_log(dir);
    write_pre_key_snapshot(dir);
}

// ============================================================================
// atexit dump into last_exit_snapshot/
// ============================================================================

extern "C" fn dump_exit_snapshot() {
    unsafe {
        /* Markers via raw write() so they appear even if stdout is weird, but
         * only under --verbose: they are developer diagnostics, and a player
         * quitting a game should see nothing. */
        let verbose = shim_stdout_logging_enabled() != 0;
        if verbose {
            const ENTER: &[u8] = b"[ATEXIT] snapshot dump_exit_snapshot enter\n";
            let _ = libc::write(2, ENTER.as_ptr() as *const c_void, ENTER.len());
        }
        if SNAP_PRESENT == 0 {
            if verbose {
                const SKIP: &[u8] =
                    b"[ATEXIT] snapshot: snap_present=0 (no key was consumed); skipping\n";
                let _ = libc::write(2, SKIP.as_ptr() as *const c_void, SKIP.len());
            }
            return;
        }
        let dir = c"last_exit_snapshot".as_ptr();
        libc::mkdir(dir, 0o755);
        write_pre_key_snapshot(dir);
        write_keys_log(dir);
        shim_crash_bundle_write_state(dir);
        shim_crash_bundle_write_trace_tail(dir);
        shim_lifecycle_dump_to_dir(dir);
        if verbose {
            let mut tail = [0 as c_char; 160];
            let n = libc::snprintf(
                tail.as_mut_ptr(),
                160,
                c"[ATEXIT] snapshot wrote %s/ (snap from t=%llu us)\n".as_ptr(),
                dir,
                SNAP_CPU.elapsed_us as c_ulonglong,
            );
            if n > 0 {
                let _ = libc::write(2, tail.as_ptr() as *const c_void, n as usize);
            }
        }
    }
}

#[no_mangle]
pub extern "C" fn snapshot_init() {
    unsafe {
        /* Register so save_crash_bundle calls our writer alongside the
         * standard crash files. */
        shim_set_bundle_extra_writer(Some(snapshot_write_to_bundle));
        /* And on clean/abrupt exits, dump the latest snapshot. */
        libc::atexit(dump_exit_snapshot);
    }
    /* save_manager_init is intentionally NOT called from here. */
}

#[no_mangle]
pub extern "C" fn snapshot_capture_and_write(dir: *const c_char) -> c_int {
    if capture_into_snap() != 0 {
        return -1;
    }
    write_pre_key_snapshot(dir);
    0
}

// ============================================================================
// Restore from snapshot
// ============================================================================

#[no_mangle]
pub extern "C" fn snapshot_restore_and_resume(bundle_dir: *const c_char) -> c_int {
    unsafe {
        let mut path = [0 as c_char; 512];
        sr_log!(c"restore TRY bundle=%s".as_ptr(), bundle_dir);

        /* 1. memory */
        libc::snprintf(
            path.as_mut_ptr(),
            512,
            c"%s/pre_last_key.bin".as_ptr(),
            bundle_dir,
        );
        let mut fd = libc::open(path.as_ptr(), libc::O_RDONLY);
        if fd < 0 {
            let e = errno();
            libc::fprintf(
                stderr,
                c"restore: cannot open %s: %s\n".as_ptr(),
                path.as_ptr(),
                libc::strerror(e),
            );
            sr_log!(
                c"restore FAIL bundle=%s reason=open_memory errno=%d".as_ptr(),
                bundle_dir,
                e
            );
            return -1;
        }
        let msize = shim_mem_size();
        let mut off: usize = 0;
        while off < msize {
            let r = libc::read(fd, virtual_memory.add(off) as *mut c_void, msize - off);
            if r <= 0 {
                libc::close(fd);
                libc::fprintf(
                    stderr,
                    c"restore: short read on %s\n".as_ptr(),
                    path.as_ptr(),
                );
                return -1;
            }
            off += r as usize;
        }
        libc::close(fd);

        /* 2. cpu state struct */
        libc::snprintf(
            path.as_mut_ptr(),
            512,
            c"%s/pre_last_key.state.bin".as_ptr(),
            bundle_dir,
        );
        fd = libc::open(path.as_ptr(), libc::O_RDONLY);
        if fd < 0 {
            libc::fprintf(
                stderr,
                c"restore: cannot open %s: %s\n".as_ptr(),
                path.as_ptr(),
                libc::strerror(errno()),
            );
            return -1;
        }
        off = 0;
        let snap_sz = core::mem::size_of::<SnapCpu>();
        while off < snap_sz {
            let r = libc::read(
                fd,
                (core::ptr::addr_of_mut!(SNAP_CPU) as *mut u8).add(off) as *mut c_void,
                snap_sz - off,
            );
            if r <= 0 {
                libc::close(fd);
                libc::fprintf(
                    stderr,
                    c"restore: short read on %s\n".as_ptr(),
                    path.as_ptr(),
                );
                return -1;
            }
            off += r as usize;
        }
        libc::close(fd);
        if SNAP_CPU.lcall_d != 0 || SNAP_CPU.isr_d != 0 {
            libc::fprintf(
                stderr,
                c"restore: refusing — snapshot has lcall_depth=%u isr_depth=%u (non-zero stacks not serialized; pick a snapshot taken while the game was at a clean keyboard wait)\n".as_ptr(),
                SNAP_CPU.lcall_d as c_uint,
                SNAP_CPU.isr_d as c_uint,
            );
            return -1;
        }

        /* 2b. Shim runtime state — missing on older bundles is non-fatal;
         * a version/size mismatch IS fatal. */
        libc::snprintf(
            path.as_mut_ptr(),
            512,
            c"%s/pre_last_key.shim_state.bin".as_ptr(),
            bundle_dir,
        );
        fd = libc::open(path.as_ptr(), libc::O_RDONLY);
        if fd >= 0 {
            let mut rt: ShimRuntimeState = core::mem::zeroed();
            let rt_sz = core::mem::size_of::<ShimRuntimeState>();
            off = 0;
            while off < rt_sz {
                let r = libc::read(
                    fd,
                    (core::ptr::addr_of_mut!(rt) as *mut u8).add(off) as *mut c_void,
                    rt_sz - off,
                );
                if r <= 0 {
                    break;
                }
                off += r as usize;
            }
            libc::close(fd);
            if off != rt_sz {
                libc::fprintf(
                    stderr,
                    c"restore: short read on %s (%zu of %zu bytes) — refusing\n".as_ptr(),
                    path.as_ptr(),
                    off,
                    rt_sz,
                );
                return -1;
            }
            if shim_runtime_state_restore(&rt) != 0 {
                /* version-mismatch already reported; bail. */
                return -1;
            }
        } else {
            libc::fprintf(
                stderr,
                c"restore: no shim_state.bin in bundle — restoring with current C-globals (legacy lossy behavior). Re-capture the snapshot with the current binary for a stable restore.\n".as_ptr(),
            );
        }

        /* 3. file_mappings */
        libc::snprintf(
            path.as_mut_ptr(),
            512,
            c"%s/pre_last_key.maps.tsv".as_ptr(),
            bundle_dir,
        );
        let mf = libc::fopen(path.as_ptr(), c"r".as_ptr());
        if mf.is_null() {
            libc::fprintf(
                stderr,
                c"restore: cannot open %s: %s\n".as_ptr(),
                path.as_ptr(),
                libc::strerror(errno()),
            );
            return -1;
        }
        shim_file_mappings_reset();
        let mut line = [0 as c_char; 1024];
        while !libc::fgets(line.as_mut_ptr(), 1024, mf).is_null() {
            let c0 = line[0];
            if c0 == b'#' as c_char || c0 == b'\n' as c_char || c0 == 0 {
                continue;
            }
            let mut base: c_uint = 0;
            let mut canonical: c_uint = 0;
            let mut len: usize = 0;
            let mut file_off: usize = 0;
            let mut tab_path = [0 as c_char; 768];
            if libc::sscanf(
                line.as_ptr(),
                c"0x%x\t0x%zx\t0x%zx\t0x%x\t%767[^\n]".as_ptr(),
                &mut base,
                &mut len,
                &mut file_off,
                &mut canonical,
                tab_path.as_mut_ptr(),
            ) != 5
            {
                continue;
            }
            shim_file_mappings_add_for_restore(
                tab_path.as_ptr(),
                base,
                len,
                file_off,
                canonical as u16,
            );
        }
        libc::fclose(mf);
        libc::fprintf(
            stderr,
            c"restore: loaded %zu file mappings\n".as_ptr(),
            shim_file_mappings_count(),
        );

        /* 4. cpu registers + flags */
        set_cs(SNAP_CPU.r_cs);
        set_ip(SNAP_CPU.r_ip);
        set_ss(SNAP_CPU.r_ss);
        set_sp(SNAP_CPU.r_sp);
        set_ds(SNAP_CPU.r_ds);
        set_es(SNAP_CPU.r_es);
        set_ax(SNAP_CPU.r_ax);
        set_bx(SNAP_CPU.r_bx);
        set_cx(SNAP_CPU.r_cx);
        set_dx(SNAP_CPU.r_dx);
        set_si(SNAP_CPU.r_si);
        set_di(SNAP_CPU.r_di);
        set_bp(SNAP_CPU.r_bp);
        set_CF(SNAP_CPU.f_CF);
        set_PF(SNAP_CPU.f_PF);
        set_ZF(SNAP_CPU.f_ZF);
        set_SF(SNAP_CPU.f_SF);
        set_OF(SNAP_CPU.f_OF);
        set_IF(SNAP_CPU.f_IF);
        set_DF(SNAP_CPU.f_DF);

        /* 5. keyboard queue */
        shim_kbd_state_restore(core::ptr::addr_of!(SNAP_CPU.kbd));

        /* 5b. Device state. Must come after the shim_state restore above: the
         * OPL2's register file arrives with *that*, and this is what replays it
         * into the FM synth the guest is actually heard through. */
        crate::devices::read_from_bundle(bundle_dir);

        /* 6. re-enter the binary that was executing at capture time. */
        let ab_ptr = core::ptr::addr_of!(SNAP_CPU.active_binary) as *const c_char;
        let ab_present = SNAP_CPU.active_binary[0] != 0;
        let name_or_none = if ab_present {
            ab_ptr
        } else {
            c"<none>".as_ptr()
        };
        libc::fprintf(
            stderr,
            c"restore: resuming at cs:ip=%04X:%04X in binary '%s' pc=0x%04X\n".as_ptr(),
            cs() as c_uint,
            ip() as c_uint,
            name_or_none,
            SNAP_CPU.r_ip as c_uint,
        );
        /* Static-dispatch resume. */
        if ab_present && shim_pc_is_case_key(ab_ptr, ip() as u32) != 0 {
            let fn_opt = shim_dispatch_fn_by_module(ab_ptr);
            if fn_opt.is_none() {
                libc::fprintf(
                    stderr,
                    c"restore: no dispatch function for binary '%s'\n".as_ptr(),
                    ab_ptr,
                );
                sr_log!(
                    c"restore FAIL bundle=%s reason=no_dispatch_fn active_binary=%s".as_ptr(),
                    bundle_dir,
                    ab_ptr
                );
                return -1;
            }
            sr_log!(
                c"restore OK bundle=%s active_binary=%s cs:ip=%04X:%04X pc=0x%04X (dispatched via active_binary)".as_ptr(),
                bundle_dir,
                ab_ptr,
                cs() as c_uint,
                ip() as c_uint,
                SNAP_CPU.r_ip as c_uint
            );
            shim_enter_binary(ab_ptr);
            (fn_opt.unwrap())(
                SNAP_CPU.r_ip as c_int,
                0xFFFF,
                c"<restore>".as_ptr(),
                c"snapshot_restore_and_resume".as_ptr(),
                0,
            );
            /* Drain any pending cross-binary tail dispatch. */
            shim_drain_pending_tail_dispatch(
                c"<restore>".as_ptr(),
                c"snapshot_restore_and_resume".as_ptr(),
                0,
            );
            shim_leave_binary();
            return 0;
        }
        /* JIT resume or legacy fallback. */
        if ab_present {
            shim_enter_binary(ab_ptr);
        }
        sr_log!(
            c"restore OK bundle=%s cs:ip=%04X:%04X active_binary=%s (resuming via run_machine)"
                .as_ptr(),
            bundle_dir,
            cs() as c_uint,
            ip() as c_uint,
            name_or_none
        );
        run_machine();
        if ab_present {
            shim_leave_binary();
        }
        0
    }
}
