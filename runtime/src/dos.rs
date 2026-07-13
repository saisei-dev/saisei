//! Port of `runtime/os/dos.c` — DOS (INT 21h) services + their helpers.
//!
//! The OS layer the translated game runs on: file I/O over the host fs, memory
//! allocation, console I/O, date/time, the interrupt-vector table, etc. The
//! functions are the same ABI the generated game code / shims.c INT21h dispatcher
//! call; they operate over shared core state (the DOS handle table, PSP /
//! next_free_seg) still owned by shims.c. ABI-identical to shims.h; a faithful,
//! line-for-line translation of dos.c (no behaviour changes).

#![allow(non_upper_case_globals)]
#![allow(static_mut_refs)]

use crate::cpu::*;
use crate::keyboard::{kbd_bios_has_event, kbd_bios_pop};
use core::ffi::{c_char, c_int, c_void};

// ---- constants (from shims.h macros) ---------------------------------------

const MAX_DOS_HANDLES: usize = 20;
const MEMORY_SIZE: usize = 1 << 21;
const CONVENTIONAL_TOP_SEG: u32 = 0xA000;
const DEFAULT_PSP_SEG: u16 = 0x1000;
const PATH_MAX_USIZE: usize = libc::PATH_MAX as usize;

// ---- shims.c-owned globals dos.c touches -----------------------------------

extern "C" {
    static mut virtual_memory: *mut u8;
    static mut handles: [*mut libc::FILE; MAX_DOS_HANDLES];
    static mut handle_paths: [*mut c_char; MAX_DOS_HANDLES];
    static mut handle_paths_owned: [bool; MAX_DOS_HANDLES];
    static mut dta_ptr: *mut c_void;
    static mut next_free_seg: u16;
    static mut program_min_block_paras: u16;
    static mut psp_seg: u16; // the PSP_SEG macro
    static mut null_guard_initial: [u8; 16];
    static mut machine_halted: c_int;
    static mut last_sw_interrupt: InterruptSnapshot;
}

// ---- shims.c functions dos.c calls -----------------------------------------

// NOTE: `save_manager_sr_log` is variadic in C (`void save_manager_sr_log(const
// char*, ...)`); sdl.rs imports the same symbol with a simplified non-variadic
// signature, so the two extern declarations clash (silenced crate-wide in lib.rs).
// Ours is the faithful one dos.c's variadic call needs.
extern "C" {
    fn fopen_case_insensitive(path: *const c_char, mode: *const c_char) -> *mut libc::FILE;
    fn is_standard_handle(handle: u16) -> c_int;
    fn memw_raw_read(seg: u16, off: u16) -> u16;
    fn memw_raw_write(seg: u16, off: u16, value: u16);
    fn wrap_segoff_addr(base: u32, offset: u32) -> u32;
    fn mask_addr(addr: u32) -> u32;
    fn write_watch_log(
        addr: u32,
        size: usize,
        value: u32,
        file: *const c_char,
        func: *const c_char,
        line: c_int,
    );
    fn shim_log_stdout(fmt: *const c_char, ...);
    fn shim_log_crash(fmt: *const c_char, ...);
    fn shim_log_file_load(path: *const c_char, addr: *const c_void, len: usize, file_offset: usize);
    fn shim_jit_invalidate_code_range(lin: u32, len: u32);
    fn shim_dos_input_wait_begin(saved_crit: *mut u8, saved_if: *mut u8);
    fn shim_dos_input_wait_end(saved_crit: u8, saved_if: u8);
    fn shim_active_binary() -> *const c_char;
    fn shim_exec_run_child(
        child_cs: u16,
        child_ip: u16,
        child_ss: u16,
        child_sp: u16,
        child_psp: u16,
    ) -> c_int;
    fn shim_exec_child_terminate(status: c_int) -> c_int;
    fn shim_save_bug_bundle(kind: *const c_char, addr: u32, msg: *const c_char);
    fn load_executable(
        path: *const c_char,
        load_seg: u16,
        is_child: c_int,
        out_cs: *mut u16,
        out_ip: *mut u16,
        out_ss: *mut u16,
        out_sp: *mut u16,
    ) -> c_int;
    fn load_overlay(path: *const c_char, load_seg: u16, reloc_factor: u16) -> c_int;
    fn critical_section_enter(
        name: *const c_char,
        file: *const c_char,
        func: *const c_char,
        line: c_int,
    );
    fn critical_section_exit(
        name: *const c_char,
        file: *const c_char,
        func: *const c_char,
        line: c_int,
    );
    fn safe_point_impl(file: *const c_char, func: *const c_char, line: c_int);
    fn shim_idle_wait();
    fn save_manager_sr_log(fmt: *const c_char, ...);
    // C stdio stream globals (not exposed by the libc crate; glibc exports them).
    static mut stdout: *mut libc::FILE;
    static mut stderr: *mut libc::FILE;
}

// ---- mirror of shims.h InterruptSnapshot -----------------------------------

#[repr(C)]
pub struct InterruptSnapshot {
    pub valid: u8,
    pub int_no: u8,
    pub ax_before: u16,
    pub bx_before: u16,
    pub cx_before: u16,
    pub dx_before: u16,
    pub ds_before: u16,
    pub es_before: u16,
    pub ss_before: u16,
    pub sp_before: u16,
    pub cs_before: u16,
    pub ip_before: u16,
    pub ax_after: u16,
    pub bx_after: u16,
    pub cx_after: u16,
    pub dx_after: u16,
    pub ds_after: u16,
    pub es_after: u16,
    pub ss_after: u16,
    pub sp_after: u16,
    pub cs_after: u16,
    pub ip_after: u16,
    pub file: *const c_char,
    pub func: *const c_char,
    pub line: c_int,
}

// ---- module-private statics (file-local in dos.c) --------------------------

static mut dos_current_psp: u16 = DEFAULT_PSP_SEG;
static mut dos_ext_scancode_pending: u8 = 0; // 0 = none queued

// The drive the guest is running on. `build/<game>/` is its C: drive — the
// program image is loaded from there and every path resolves into it — so the
// only truthful answer to AH=19h is 2. Answering 0 told a game it had been
// started from a floppy, and a game that believes it is on a floppy goes
// looking for one: Popcorn asked for the current drive, was told A:, and went
// straight to an absolute sector read (INT 25h) of it, which no directory-backed
// drive can serve. The drive letter itself never reaches the file layer
// (`dos_strip_drive_prefix`); this is purely what DOS says about itself.
static mut dos_current_drive: u8 = 2; // 2 = C:
static mut dos_last_alloc_seg: u16 = 0;
static mut dos_child_return_code: u16 = 0;

/// How each handle was opened: 0 = read-only, 1 = writable. The `FILE*` knows,
/// but will not say, and a restore has to open the file the same way the guest
/// did — see `files_capture`.
static mut handle_modes: [u8; MAX_DOS_HANDLES] = [0; MAX_DOS_HANDLES];

const HANDLE_MODE_READ: u8 = 0;
const HANDLE_MODE_WRITE: u8 = 1;

fn hmset(i: usize, mode: u8) {
    unsafe {
        if i < MAX_DOS_HANDLES {
            core::ptr::addr_of_mut!(handle_modes)
                .cast::<u8>()
                .add(i)
                .write(mode);
        }
    }
}

fn hmget(i: usize) -> u8 {
    unsafe {
        if i < MAX_DOS_HANDLES {
            core::ptr::addr_of!(handle_modes).cast::<u8>().add(i).read()
        } else {
            HANDLE_MODE_READ
        }
    }
}

// ---- snapshot block (see devices.rs) ---------------------------------------
//
// DOS is a device too, as far as a save is concerned: these are the values INT
// 21h reports back, and none of them was in ShimRuntimeState. The DTA is the one
// that needs a conversion — it is held as a host pointer into guest RAM, which
// means nothing in the next process, so it travels as the linear address it
// actually is.

#[repr(C)]
#[derive(Clone, Copy)]
struct DosSnap {
    current_psp: u16,
    ext_scancode_pending: u8,
    current_drive: u8,
    last_alloc_seg: u16,
    child_return_code: u16,
    dta_linear: u32,
}

// ---- the guest's open files (snapshot block "DOSF") -------------------------
//
// A DOS handle is guest state: the game holds the number, and reads, writes and
// seeks through it. Everything *behind* it is host state — a `FILE*`, a strdup'd
// path, and a seek offset that belongs to the C library. A restore is a fresh
// process, so all of that comes back NULL while guest RAM comes back still
// holding the handle numbers. The guest does not notice until it next touches the
// disk; for a game that streams its levels off a file it keeps open, that is the
// moment you walk down the stairs. The read fails, and the game does what any
// program does when its data file has vanished from under it: it reports the
// error and terminates. (Dungeon Master: "SYSTEM ERROR", then INT 21h AH=4Ch.)
//
// So the table is captured exactly as a device's registers are — what the guest
// can see (the handle number) plus what it takes to make the host side mean that
// again: the path, how it was opened, and where the file was positioned.
//
// Not a fixed POD, so not `pod_capture`: the paths are variable-length.
//
//   count:u32
//   [ handle:u16  mode:u8  pad:u8  offset:i64  path_len:u16  path:[u8; path_len] ] * count
//
// The standard handles are left out: a fresh process brings its own stdin and
// stdout, and they are not the guest's to restore.

pub(crate) unsafe fn files_capture(out: &mut Vec<u8>) {
    let mut body: Vec<u8> = Vec::new();
    let mut count: u32 = 0;

    for i in 0..MAX_DOS_HANDLES {
        let fp = hget(i);
        if fp.is_null() || is_standard_handle(i as u16) != 0 {
            continue;
        }
        let path = hpget(i);
        if path.is_null() {
            continue;
        }
        // Where the guest left the file. A handle whose position we cannot read is
        // one we could not put back faithfully, and a file silently rewound to 0
        // is worse than one that is honestly missing.
        let off = libc::ftell(fp);
        if off < 0 {
            continue;
        }
        let bytes = core::ffi::CStr::from_ptr(path).to_bytes();
        if bytes.is_empty() || bytes.len() > u16::MAX as usize {
            continue;
        }

        body.extend_from_slice(&(i as u16).to_le_bytes());
        body.push(hmget(i));
        body.push(0);
        body.extend_from_slice(&(off as i64).to_le_bytes());
        body.extend_from_slice(&(bytes.len() as u16).to_le_bytes());
        body.extend_from_slice(bytes);
        count += 1;
    }

    out.extend_from_slice(&count.to_le_bytes());
    out.extend_from_slice(&body);
}

pub(crate) unsafe fn files_restore(b: &[u8]) -> bool {
    if b.len() < 4 {
        return false;
    }
    let count = u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
    let mut p = 4usize;

    for _ in 0..count {
        if p + 14 > b.len() {
            return false;
        }
        let h = u16::from_le_bytes([b[p], b[p + 1]]) as usize;
        let mode = b[p + 2];
        let off = i64::from_le_bytes([
            b[p + 4],
            b[p + 5],
            b[p + 6],
            b[p + 7],
            b[p + 8],
            b[p + 9],
            b[p + 10],
            b[p + 11],
        ]);
        let len = u16::from_le_bytes([b[p + 12], b[p + 13]]) as usize;
        p += 14;
        if p + len > b.len() {
            return false;
        }
        let path = &b[p..p + len];
        p += len;

        if h >= MAX_DOS_HANDLES || is_standard_handle(h as u16) != 0 {
            continue;
        }
        let Ok(cpath) = std::ffi::CString::new(path) else {
            continue;
        };

        // Never a truncating mode, whatever the guest opened it with. A handle the
        // guest got from AH=3Ch was opened "wb+" — reopening it that way here would
        // erase the very file the save exists to preserve.
        let fmode = if mode == HANDLE_MODE_READ {
            c"rb".as_ptr()
        } else {
            c"r+b".as_ptr()
        };
        let fp = fopen_case_insensitive(cpath.as_ptr(), fmode);
        if fp.is_null() {
            shim_log_stdout(
                c"restore: could not reopen DOS handle %d on %s\n".as_ptr(),
                h as c_int,
                cpath.as_ptr(),
            );
            continue;
        }
        libc::fseek(fp, off as libc::c_long, libc::SEEK_SET);

        if !hget(h).is_null() {
            libc::fclose(hget(h));
        }
        hset(h, fp);
        hpset(h, libc::strdup(cpath.as_ptr()));
        hoset(h, true);
        hmset(h, mode);
    }
    true
}

/// Throw the open-file table away, as the fresh process a restore runs in does.
///
/// Deliberately leaks the `FILE*` and the path rather than closing them: a process
/// that has died did not close its files either, and a test that tidied up first
/// would be testing something gentler than the thing that actually happens.
#[cfg(test)]
pub(crate) unsafe fn forget_open_files_for_test() {
    for i in 0..MAX_DOS_HANDLES {
        if is_standard_handle(i as u16) == 0 {
            hset(i, core::ptr::null_mut());
            hpset(i, core::ptr::null_mut());
            hoset(i, false);
            hmset(i, HANDLE_MODE_READ);
        }
    }
}

pub(crate) unsafe fn state_capture(out: &mut Vec<u8>) {
    let s = DosSnap {
        current_psp: dos_current_psp,
        ext_scancode_pending: dos_ext_scancode_pending,
        current_drive: dos_current_drive,
        last_alloc_seg: dos_last_alloc_seg,
        child_return_code: dos_child_return_code,
        dta_linear: crate::devices::ptr_to_linear(dta_ptr),
    };
    crate::devices::pod_capture(&s, out);
}

pub(crate) unsafe fn state_restore(b: &[u8]) -> bool {
    match crate::devices::pod_restore::<DosSnap>(b) {
        Some(s) => {
            dos_current_psp = s.current_psp;
            dos_ext_scancode_pending = s.ext_scancode_pending;
            dos_current_drive = s.current_drive;
            dos_last_alloc_seg = s.last_alloc_seg;
            dos_child_return_code = s.child_return_code;
            dta_ptr = crate::devices::linear_to_ptr(s.dta_linear);
            true
        }
        None => false,
    }
}

// ---- small helpers ---------------------------------------------------------

#[inline(always)]
fn errno() -> c_int {
    unsafe { *libc::__errno_location() }
}
#[inline(always)]
fn vm() -> *mut u8 {
    unsafe { virtual_memory }
}
#[inline(always)]
fn safepoint() {
    unsafe { safe_point_impl(c"dos.rs".as_ptr(), c"dos".as_ptr(), 0) }
}
#[inline(always)]
fn crit_enter(name: *const c_char) {
    unsafe { critical_section_enter(name, c"dos.rs".as_ptr(), name, 0) }
}
#[inline(always)]
fn crit_exit(name: *const c_char) {
    unsafe { critical_section_exit(name, c"dos.rs".as_ptr(), name, 0) }
}
#[inline(always)]
fn hget(i: usize) -> *mut libc::FILE {
    unsafe {
        core::ptr::addr_of!(handles)
            .cast::<*mut libc::FILE>()
            .add(i)
            .read()
    }
}
#[inline(always)]
fn hset(i: usize, v: *mut libc::FILE) {
    unsafe {
        core::ptr::addr_of_mut!(handles)
            .cast::<*mut libc::FILE>()
            .add(i)
            .write(v)
    }
}
#[inline(always)]
fn hpget(i: usize) -> *mut c_char {
    unsafe {
        core::ptr::addr_of!(handle_paths)
            .cast::<*mut c_char>()
            .add(i)
            .read()
    }
}
#[inline(always)]
fn hpset(i: usize, v: *mut c_char) {
    unsafe {
        core::ptr::addr_of_mut!(handle_paths)
            .cast::<*mut c_char>()
            .add(i)
            .write(v)
    }
}
#[inline(always)]
fn hoget(i: usize) -> bool {
    unsafe {
        core::ptr::addr_of!(handle_paths_owned)
            .cast::<bool>()
            .add(i)
            .read()
    }
}
#[inline(always)]
fn hoset(i: usize, v: bool) {
    unsafe {
        core::ptr::addr_of_mut!(handle_paths_owned)
            .cast::<bool>()
            .add(i)
            .write(v)
    }
}

/// Strip a leading "X:" drive specifier and the whole leading root-separator run.
fn dos_strip_drive_prefix(path: *const c_char) -> *const c_char {
    if path.is_null() {
        return path;
    }
    let mut p = path as *const u8;
    unsafe {
        if *p != 0 && *p.add(1) == b':' {
            p = p.add(2);
        }
        while *p == b'\\' || *p == b'/' {
            p = p.add(1);
        }
    }
    p as *const c_char
}

#[no_mangle]
pub extern "C" fn dos_set_current_psp_to_load() {
    unsafe {
        dos_current_psp = psp_seg;
    }
}

// ---- console input / output ------------------------------------------------

#[no_mangle]
pub extern "C" fn dos_read_char_impl(file: *const c_char, func: *const c_char, line: c_int) -> u8 {
    unsafe {
        shim_log(
            c"dos_read_char_impl".as_ptr(),
            file,
            func,
            line,
            core::ptr::null(),
        );
    }
    if unsafe { dos_ext_scancode_pending } != 0 {
        set_al(unsafe { dos_ext_scancode_pending });
        unsafe {
            dos_ext_scancode_pending = 0;
        }
        dos_write_char_impl(al(), file, func, line);
        return 0;
    }
    let mut saved_crit: u8 = 0;
    let mut saved_if: u8 = 0;
    unsafe {
        shim_dos_input_wait_begin(&mut saved_crit, &mut saved_if);
    }
    loop {
        let mut ascii: u8 = 0;
        let mut scan: u8 = 0;
        if kbd_bios_pop(&mut ascii, &mut scan) != 0 {
            if ascii == 0 {
                unsafe {
                    dos_ext_scancode_pending = scan;
                }
            }
            dos_write_char_impl(ascii, file, func, line);
            set_al(ascii);
            unsafe {
                shim_dos_input_wait_end(saved_crit, saved_if);
            }
            return 0;
        }
        unsafe { shim_idle_wait() };
    }
}

#[no_mangle]
pub extern "C" fn dos_read_char() -> u8 {
    dos_read_char_impl(c"<external>".as_ptr(), c"dos_read_char".as_ptr(), 0)
}

#[no_mangle]
pub extern "C" fn dos_write_char_impl(
    ch_val: u8,
    file: *const c_char,
    func: *const c_char,
    line: c_int,
) -> u8 {
    let ch_str = [ch_val, 0u8];
    unsafe {
        shim_log(
            c"dos_write_char_impl".as_ptr(),
            file,
            func,
            line,
            ch_str.as_ptr() as *const c_char,
        );
        libc::fputc(ch_val as c_int, stdout);
        libc::fflush(stdout);
    }
    0
}

#[no_mangle]
pub extern "C" fn dos_write_char(ch_val: u8) -> u8 {
    dos_write_char_impl(
        ch_val,
        c"<external>".as_ptr(),
        c"dos_write_char".as_ptr(),
        0,
    )
}

#[no_mangle]
pub extern "C" fn dos_direct_console_io_impl(
    dl_val: u8,
    file: *const c_char,
    func: *const c_char,
    line: c_int,
) -> u8 {
    unsafe {
        shim_log(
            c"dos_direct_console_io_impl".as_ptr(),
            file,
            func,
            line,
            core::ptr::null(),
        );
    }
    if dl_val == 0xFF {
        safepoint();
        let mut ascii: u8 = 0;
        if kbd_bios_pop(&mut ascii, core::ptr::null_mut()) != 0 {
            set_al(ascii);
            set_ZF(0);
        } else {
            set_ZF(1);
        }
    } else {
        dos_write_char_impl(dl_val, file, func, line);
        set_al(dl_val);
        set_ZF(0);
    }
    0
}

#[no_mangle]
pub extern "C" fn dos_direct_console_io(dl_val: u8) -> u8 {
    dos_direct_console_io_impl(
        dl_val,
        c"<external>".as_ptr(),
        c"dos_direct_console_io".as_ptr(),
        0,
    )
}

#[no_mangle]
pub extern "C" fn dos_check_keyboard_status_impl(
    file: *const c_char,
    func: *const c_char,
    line: c_int,
) -> u8 {
    unsafe {
        shim_log(
            c"dos_check_keyboard_status_impl".as_ptr(),
            file,
            func,
            line,
            core::ptr::null(),
        );
    }
    if kbd_bios_has_event() != 0 {
        set_al(0xFF);
        set_ZF(0);
    } else {
        set_al(0x00);
        set_ZF(1);
    }
    set_CF(0);
    0
}

#[no_mangle]
pub extern "C" fn dos_check_keyboard_status() -> u8 {
    dos_check_keyboard_status_impl(
        c"<external>".as_ptr(),
        c"dos_check_keyboard_status".as_ptr(),
        0,
    )
}

#[no_mangle]
pub extern "C" fn dos_buffered_input_impl(
    file: *const c_char,
    func: *const c_char,
    line: c_int,
) -> u8 {
    unsafe {
        shim_log(
            c"dos_buffered_input_impl".as_ptr(),
            file,
            func,
            line,
            core::ptr::null(),
        );
    }
    let buf = seg_off(ds(), dx());
    let maxlen = unsafe { *buf };
    if maxlen == 0 {
        return 0; // no room even for the CR
    }
    let mut count: u8 = 0;
    let mut saved_crit: u8 = 0;
    let mut saved_if: u8 = 0;
    unsafe {
        shim_dos_input_wait_begin(&mut saved_crit, &mut saved_if);
    }
    loop {
        let mut ascii: u8 = 0;
        let mut scan: u8 = 0;
        if kbd_bios_pop(&mut ascii, &mut scan) == 0 {
            unsafe { shim_idle_wait() };
            continue;
        }
        if ascii == 0x0D {
            // Enter: terminate the line
            unsafe {
                *buf.add(1) = count;
                *buf.add(2 + count as usize) = 0x0D;
            }
            dos_write_char_impl(0x0D, file, func, line);
            dos_write_char_impl(0x0A, file, func, line);
            unsafe {
                shim_dos_input_wait_end(saved_crit, saved_if);
            }
            return 0;
        }
        if ascii == 0x08 {
            // Backspace: erase the last char on screen + buffer
            if count > 0 {
                count -= 1;
                dos_write_char_impl(0x08, file, func, line);
                dos_write_char_impl(0x20, file, func, line);
                dos_write_char_impl(0x08, file, func, line);
            }
            continue;
        }
        // Printable char: store while a slot remains before the reserved CR.
        if ascii >= 0x20 && count < maxlen.wrapping_sub(1) {
            unsafe {
                *buf.add(2 + count as usize) = ascii;
            }
            count += 1;
            dos_write_char_impl(ascii, file, func, line);
        }
    }
}

#[no_mangle]
pub extern "C" fn dos_buffered_input() -> u8 {
    dos_buffered_input_impl(c"<external>".as_ptr(), c"dos_buffered_input".as_ptr(), 0)
}

#[no_mangle]
pub extern "C" fn dos_reset_disk_impl(file: *const c_char, func: *const c_char, line: c_int) -> u8 {
    unsafe {
        shim_log(
            c"dos_reset_disk_impl".as_ptr(),
            file,
            func,
            line,
            core::ptr::null(),
        );
    }
    0
}

#[no_mangle]
pub extern "C" fn dos_reset_disk() -> u8 {
    dos_reset_disk_impl(c"<external>".as_ptr(), c"dos_reset_disk".as_ptr(), 0)
}

#[no_mangle]
pub extern "C" fn dos_select_drive_impl(
    dl_val: u8,
    file: *const c_char,
    func: *const c_char,
    line: c_int,
) -> u8 {
    unsafe {
        shim_log(
            c"dos_select_drive_impl".as_ptr(),
            file,
            func,
            line,
            core::ptr::null(),
        );
    }
    unsafe {
        dos_current_drive = dl_val;
    }
    set_al(26); // number of logical drives
    0
}

#[no_mangle]
pub extern "C" fn dos_select_drive(dl_val: u8) -> u8 {
    dos_select_drive_impl(
        dl_val,
        c"<external>".as_ptr(),
        c"dos_select_drive".as_ptr(),
        0,
    )
}

#[no_mangle]
pub extern "C" fn dos_print_string_impl(
    str: *const c_char,
    file: *const c_char,
    func: *const c_char,
    line: c_int,
) -> u8 {
    unsafe {
        shim_log(
            c"dos_print_string_impl".as_ptr(),
            file,
            func,
            line,
            core::ptr::null(),
        );
    }
    // DOS strings are '$' terminated.
    let sp = str as *const u8;
    let mut len: usize = 0;
    unsafe {
        while *sp.add(len) != 0 && *sp.add(len) != b'$' {
            len += 1;
        }
        if len > 0 {
            libc::fwrite(str as *const c_void, 1, len, stdout);
            libc::fflush(stdout);
        }
    }
    0
}

#[no_mangle]
pub extern "C" fn dos_print_string(str: *const c_char) -> u8 {
    dos_print_string_impl(str, c"<external>".as_ptr(), c"dos_print_string".as_ptr(), 0)
}

#[no_mangle]
pub extern "C" fn dos_get_current_drive_impl(
    file: *const c_char,
    func: *const c_char,
    line: c_int,
) -> u8 {
    unsafe {
        shim_log(
            c"dos_get_current_drive_impl".as_ptr(),
            file,
            func,
            line,
            core::ptr::null(),
        );
    }
    set_al(unsafe { dos_current_drive });
    0
}

#[no_mangle]
pub extern "C" fn dos_get_current_drive() -> u8 {
    dos_get_current_drive_impl(c"<external>".as_ptr(), c"dos_get_current_drive".as_ptr(), 0)
}

#[no_mangle]
pub extern "C" fn dos_set_dta_impl(
    dta: *mut c_void,
    file: *const c_char,
    func: *const c_char,
    line: c_int,
) -> u8 {
    unsafe {
        shim_log(
            c"dos_set_dta_impl".as_ptr(),
            file,
            func,
            line,
            core::ptr::null(),
        );
    }
    unsafe {
        dta_ptr = dta;
    }
    0
}

#[no_mangle]
pub extern "C" fn dos_set_dta(dta: *mut c_void) -> u8 {
    dos_set_dta_impl(dta, c"<external>".as_ptr(), c"dos_set_dta".as_ptr(), 0)
}

#[no_mangle]
pub extern "C" fn dos_set_interrupt_vector_impl(
    int_no: u8,
    segment: u16,
    offset: u16,
    file: *const c_char,
    func: *const c_char,
    line: c_int,
) -> u8 {
    unsafe {
        shim_log(
            c"dos_set_interrupt_vector_impl".as_ptr(),
            file,
            func,
            line,
            core::ptr::null(),
        );
    }
    let off = (int_no as u16).wrapping_mul(4);
    unsafe {
        memw_raw_write(0, off, offset);
        memw_raw_write(0, off + 2, segment);
    }
    0
}

#[no_mangle]
pub extern "C" fn dos_set_interrupt_vector(int_no: u8, segment: u16, offset: u16) -> u8 {
    dos_set_interrupt_vector_impl(
        int_no,
        segment,
        offset,
        c"<external>".as_ptr(),
        c"dos_set_interrupt_vector".as_ptr(),
        0,
    )
}

#[no_mangle]
pub extern "C" fn dos_get_disk_free_space_impl(
    dl_val: u8,
    file: *const c_char,
    func: *const c_char,
    line: c_int,
) -> u8 {
    unsafe {
        shim_log(
            c"dos_get_disk_free_space_impl".as_ptr(),
            file,
            func,
            line,
            core::ptr::null(),
        );
    }
    let _ = dl_val;
    0
}

#[no_mangle]
pub extern "C" fn dos_get_disk_free_space(dl_val: u8) -> u8 {
    dos_get_disk_free_space_impl(
        dl_val,
        c"<external>".as_ptr(),
        c"dos_get_disk_free_space".as_ptr(),
        0,
    )
}

#[no_mangle]
pub extern "C" fn dos_get_version_impl(
    file: *const c_char,
    func: *const c_char,
    line: c_int,
) -> u8 {
    unsafe {
        shim_log(
            c"dos_get_version_impl".as_ptr(),
            file,
            func,
            line,
            core::ptr::null(),
        );
    }
    set_al(3); // Major version
    set_ah(0); // Minor version
    set_bh(0); // OEM number
    set_bl(0); // Reserved
    set_CF(0);
    0
}

#[no_mangle]
pub extern "C" fn dos_get_version() -> u8 {
    dos_get_version_impl(c"<external>".as_ptr(), c"dos_get_version".as_ptr(), 0)
}

// AH=2Ch Get time: CH=hour, CL=min, DH=sec, DL=hundredths.
#[no_mangle]
pub extern "C" fn dos_get_time_impl(file: *const c_char, func: *const c_char, line: c_int) -> u8 {
    unsafe {
        shim_log(
            c"dos_get_time_impl".as_ptr(),
            file,
            func,
            line,
            core::ptr::null(),
        );
    }
    unsafe {
        let mut tv: libc::timeval = core::mem::zeroed();
        libc::gettimeofday(&mut tv, core::ptr::null_mut());
        let tm = libc::localtime(&tv.tv_sec);
        set_ch((*tm).tm_hour as u8);
        set_cl((*tm).tm_min as u8);
        set_dh((*tm).tm_sec as u8);
        set_dl((tv.tv_usec / 10000) as u8);
    }
    set_CF(0);
    0
}

#[no_mangle]
pub extern "C" fn dos_get_time() -> u8 {
    dos_get_time_impl(c"<external>".as_ptr(), c"dos_get_time".as_ptr(), 0)
}

// AH=2Ah Get date: CX=year, DH=month, DL=day, AL=weekday (0=Sunday).
#[no_mangle]
pub extern "C" fn dos_get_date_impl(file: *const c_char, func: *const c_char, line: c_int) -> u8 {
    unsafe {
        shim_log(
            c"dos_get_date_impl".as_ptr(),
            file,
            func,
            line,
            core::ptr::null(),
        );
    }
    unsafe {
        let mut tv: libc::timeval = core::mem::zeroed();
        libc::gettimeofday(&mut tv, core::ptr::null_mut());
        let tm = libc::localtime(&tv.tv_sec);
        set_cx(((*tm).tm_year + 1900) as u16);
        set_dh(((*tm).tm_mon + 1) as u8);
        set_dl((*tm).tm_mday as u8);
        set_al((*tm).tm_wday as u8);
    }
    set_CF(0);
    0
}

#[no_mangle]
pub extern "C" fn dos_get_date() -> u8 {
    dos_get_date_impl(c"<external>".as_ptr(), c"dos_get_date".as_ptr(), 0)
}

// AH=07h Direct console input without echo: block for a key, return AL=char.
#[no_mangle]
pub extern "C" fn dos_console_input_no_echo_impl(
    file: *const c_char,
    func: *const c_char,
    line: c_int,
) -> u8 {
    unsafe {
        shim_log(
            c"dos_console_input_no_echo_impl".as_ptr(),
            file,
            func,
            line,
            core::ptr::null(),
        );
    }
    if unsafe { dos_ext_scancode_pending } != 0 {
        set_al(unsafe { dos_ext_scancode_pending });
        unsafe {
            dos_ext_scancode_pending = 0;
        }
        return 0;
    }
    let mut saved_crit: u8 = 0;
    let mut saved_if: u8 = 0;
    unsafe {
        shim_dos_input_wait_begin(&mut saved_crit, &mut saved_if);
    }
    loop {
        let mut ascii: u8 = 0;
        let mut scan: u8 = 0;
        if kbd_bios_pop(&mut ascii, &mut scan) != 0 {
            if ascii == 0 {
                unsafe {
                    dos_ext_scancode_pending = scan;
                }
            }
            set_al(ascii);
            unsafe {
                shim_dos_input_wait_end(saved_crit, saved_if);
            }
            return 0;
        }
        unsafe { shim_idle_wait() };
    }
}

#[no_mangle]
pub extern "C" fn dos_console_input_no_echo() -> u8 {
    dos_console_input_no_echo_impl(
        c"<external>".as_ptr(),
        c"dos_console_input_no_echo".as_ptr(),
        0,
    )
}

#[no_mangle]
pub extern "C" fn dos_get_interrupt_vector_impl(
    file: *const c_char,
    func: *const c_char,
    line: c_int,
) -> u8 {
    unsafe {
        shim_log(
            c"dos_get_interrupt_vector_impl".as_ptr(),
            file,
            func,
            line,
            core::ptr::null(),
        );
    }
    let int_no = al();
    let off = (int_no as u16).wrapping_mul(4);
    unsafe {
        set_bx(memw_raw_read(0, off));
        set_es(memw_raw_read(0, off + 2));
    }
    0
}

#[no_mangle]
pub extern "C" fn dos_get_interrupt_vector() -> u8 {
    dos_get_interrupt_vector_impl(
        c"<external>".as_ptr(),
        c"dos_get_interrupt_vector".as_ptr(),
        0,
    )
}

#[no_mangle]
pub extern "C" fn interrupt_vector_addr_impl(
    int_no: u8,
    file: *const c_char,
    func: *const c_char,
    line: c_int,
) -> u32 {
    unsafe {
        shim_log(
            c"interrupt_vector_addr_impl".as_ptr(),
            file,
            func,
            line,
            core::ptr::null(),
        );
    }
    let off = (int_no as u16).wrapping_mul(4);
    let offset = unsafe { memw_raw_read(0, off) };
    let segment = unsafe { memw_raw_read(0, off + 2) };
    ((segment as u32) << 4) + offset as u32
}

#[no_mangle]
pub extern "C" fn interrupt_vector_addr(int_no: u8) -> u32 {
    interrupt_vector_addr_impl(
        int_no,
        c"<external>".as_ptr(),
        c"interrupt_vector_addr".as_ptr(),
        0,
    )
}

#[no_mangle]
pub extern "C" fn dos_make_dir_impl(
    path: *const c_char,
    file: *const c_char,
    func: *const c_char,
    line: c_int,
) -> u8 {
    unsafe {
        shim_log(c"dos_make_dir_impl".as_ptr(), file, func, line, path);
    }
    let _ = path;
    0
}

#[no_mangle]
pub extern "C" fn dos_make_dir(path: *const c_char) -> u8 {
    dos_make_dir_impl(path, c"<external>".as_ptr(), c"dos_make_dir".as_ptr(), 0)
}

#[no_mangle]
pub extern "C" fn dos_change_dir_impl(
    path: *const c_char,
    file: *const c_char,
    func: *const c_char,
    line: c_int,
) -> u8 {
    unsafe {
        shim_log(c"dos_change_dir_impl".as_ptr(), file, func, line, path);
    }
    let _ = path;
    0
}

#[no_mangle]
pub extern "C" fn dos_change_dir(path: *const c_char) -> u8 {
    dos_change_dir_impl(path, c"<external>".as_ptr(), c"dos_change_dir".as_ptr(), 0)
}

#[no_mangle]
pub extern "C" fn log_last_sw_interrupt_snapshot() {
    let ls = unsafe { &*core::ptr::addr_of!(last_sw_interrupt) };
    if ls.valid == 0 {
        unsafe {
            shim_log_stdout(c"Trace: last_sw_interrupt: <none>\n".as_ptr());
        }
        return;
    }
    let file_or = if !ls.file.is_null() {
        ls.file
    } else {
        c"<unknown>".as_ptr()
    };
    let func_or = if !ls.func.is_null() {
        ls.func
    } else {
        c"<unknown>".as_ptr()
    };
    unsafe {
        shim_log_stdout(
            c"Trace: last_sw_interrupt int=0x%02X at %s:%s:%d\n".as_ptr(),
            ls.int_no as c_int,
            file_or,
            func_or,
            ls.line,
        );
        shim_log_stdout(
            c"Trace:   before cs:ip=%04X:%04X ss:sp=%04X:%04X ds:es=%04X:%04X ax=%04X bx=%04X cx=%04X dx=%04X\n".as_ptr(),
            ls.cs_before as c_int, ls.ip_before as c_int,
            ls.ss_before as c_int, ls.sp_before as c_int,
            ls.ds_before as c_int, ls.es_before as c_int,
            ls.ax_before as c_int, ls.bx_before as c_int,
            ls.cx_before as c_int, ls.dx_before as c_int,
        );
        shim_log_stdout(
            c"Trace:   after  cs:ip=%04X:%04X ss:sp=%04X:%04X ds:es=%04X:%04X ax=%04X bx=%04X cx=%04X dx=%04X\n".as_ptr(),
            ls.cs_after as c_int, ls.ip_after as c_int,
            ls.ss_after as c_int, ls.sp_after as c_int,
            ls.ds_after as c_int, ls.es_after as c_int,
            ls.ax_after as c_int, ls.bx_after as c_int,
            ls.cx_after as c_int, ls.dx_after as c_int,
        );
    }
}

// ---- file I/O --------------------------------------------------------------

#[no_mangle]
pub extern "C" fn dos_open_file_impl(
    path: *const c_char,
    file: *const c_char,
    func: *const c_char,
    line: c_int,
) -> u8 {
    let ptr = path as usize;
    let p = path;
    let vmb = vm() as usize;
    if ptr >= vmb && ptr < vmb + MEMORY_SIZE {
        let addr = ptr - vmb;
        let seg = (addr >> 4) as u16;
        let off = (addr & 0xF) as u16;
        unsafe {
            shim_log_stdout(
                c"Trace: dos_open_file ds:dx=%04X:%04X\n".as_ptr(),
                seg as c_int,
                off as c_int,
            );
        }
    } else {
        unsafe {
            shim_log_stdout(
                c"Trace: dos_open_file ds:dx=%04X:%04X\n".as_ptr(),
                ds() as c_int,
                dx() as c_int,
            );
        }
    }
    if p.is_null() || unsafe { *(p as *const u8) } == 0 {
        unsafe {
            shim_log(
                c"dos_open_file_impl".as_ptr(),
                file,
                func,
                line,
                c"<empty>".as_ptr(),
            );
            shim_log_stdout(c"Error: dos_open_file: empty path\n".as_ptr());
        }
        set_ax(3); // path not found
        return 1;
    }
    let mut buf = [0u8; 260];
    let mut i: usize = 0;
    let p_stripped = dos_strip_drive_prefix(p) as *const u8;
    unsafe {
        while i < buf.len() - 1 && *p_stripped.add(i) != 0 && *p_stripped.add(i) != b'\r' {
            buf[i] = *p_stripped.add(i);
            i += 1;
        }
    }
    buf[i] = 0;

    let open_path = buf.as_ptr() as *const c_char;
    unsafe {
        shim_log(c"dos_open_file_impl".as_ptr(), file, func, line, open_path);
        shim_log_stdout(c"Trace: dos_open_file: %s\n".as_ptr(), open_path);
        shim_log_stdout(
            c"Trace: %s path bytes:".as_ptr(),
            c"dos_open_file_impl".as_ptr(),
        );
        let mut j: usize = 0;
        while buf[j] != 0 {
            shim_log_stdout(c" %02X".as_ptr(), buf[j] as c_int);
            j += 1;
        }
        shim_log_stdout(c"\n".as_ptr());
    }

    let dos_access_mode = al() & 0x07;
    let fopen_mode = if dos_access_mode == 0 {
        c"rb".as_ptr()
    } else {
        c"r+b".as_ptr()
    };

    for i in 0..MAX_DOS_HANDLES {
        if hget(i).is_null() {
            let fp = unsafe { fopen_case_insensitive(open_path, fopen_mode) };
            hset(i, fp);
            // Remembered for the snapshot: a restore has to reopen the file the
            // same way the guest did, and the FILE* will not tell us.
            hmset(
                i,
                if dos_access_mode == 0 {
                    HANDLE_MODE_READ
                } else {
                    HANDLE_MODE_WRITE
                },
            );
            if !fp.is_null() {
                hpset(i, unsafe { libc::strdup(open_path) });
                hoset(i, true);
                set_ax(i as u16);
                set_bx(i as u16);
                return 0;
            }
            unsafe {
                shim_log_stdout(
                    c"Error: dos_open_file: failed to open %s: %s\n".as_ptr(),
                    open_path,
                    libc::strerror(errno()),
                );
            }
            set_ax(errno_to_dos(errno())); // AX = DOS error code (2 = not found)
            return 1;
        }
    }
    unsafe {
        shim_log_stdout(
            c"Error: dos_open_file: no handles available for %s\n".as_ptr(),
            open_path,
        );
    }
    set_ax(4); // too many open files
    1
}

#[no_mangle]
pub extern "C" fn dos_open_file(path: *const c_char) -> u8 {
    crit_enter(c"dos_open_file".as_ptr());
    let result = dos_open_file_impl(path, c"<external>".as_ptr(), c"dos_open_file".as_ptr(), 0);
    crit_exit(c"dos_open_file".as_ptr());
    result
}

#[no_mangle]
pub extern "C" fn dos_close_file_impl(
    handle: u16,
    file: *const c_char,
    func: *const c_char,
    line: c_int,
) -> u8 {
    unsafe {
        shim_log(
            c"dos_close_file_impl".as_ptr(),
            file,
            func,
            line,
            core::ptr::null(),
        );
    }
    if handle < MAX_DOS_HANDLES as u16 && !hget(handle as usize).is_null() {
        if unsafe { is_standard_handle(handle) } != 0 {
            return 0;
        }
        unsafe {
            libc::fclose(hget(handle as usize));
        }
        hset(handle as usize, core::ptr::null_mut());
        if hoget(handle as usize) {
            unsafe {
                libc::free(hpget(handle as usize) as *mut c_void);
            }
            hoset(handle as usize, false);
        }
        hpset(handle as usize, core::ptr::null_mut());
        return 0;
    }
    1
}

#[no_mangle]
pub extern "C" fn dos_close_file(handle: u16) -> u8 {
    crit_enter(c"dos_close_file".as_ptr());
    let result = dos_close_file_impl(
        handle,
        c"<external>".as_ptr(),
        c"dos_close_file".as_ptr(),
        0,
    );
    crit_exit(c"dos_close_file".as_ptr());
    result
}

#[no_mangle]
pub extern "C" fn dos_read_file_impl(
    handle: u16,
    buf: *mut c_void,
    len: u16,
    file: *const c_char,
    func: *const c_char,
    line: c_int,
) -> u8 {
    if handle < MAX_DOS_HANDLES as u16 && !hget(handle as usize).is_null() {
        let path = hpget(handle as usize);
        unsafe {
            shim_log(c"dos_read_file_impl".as_ptr(), file, func, line, path);
        }
        if !path.is_null() {
            unsafe {
                shim_log_stdout(c"Trace: dos_read_file: %s -> %p\n".as_ptr(), path, buf);
            }
        }
        let pos = unsafe { libc::ftell(hget(handle as usize)) };
        let tmp = unsafe { libc::malloc(len as usize) as *mut u8 };
        if tmp.is_null() {
            unsafe {
                shim_log_stdout(c"Trace: dos_read_file allocation failure\n".as_ptr());
            }
            set_ax(0);
            return 1;
        }
        let r = unsafe { libc::fread(tmp as *mut c_void, 1, len as usize, hget(handle as usize)) };
        set_ax(r as u16);
        let p = buf as *mut u8;
        let p_in_virtual_memory =
            (p as usize) >= (vm() as usize) && (p as usize) < (vm() as usize) + MEMORY_SIZE;
        if !path.is_null() {
            unsafe {
                shim_log_file_load(
                    path,
                    p as *const c_void,
                    r,
                    if pos >= 0 { pos as usize } else { 0 },
                );
            }
        }
        if p_in_virtual_memory {
            let base = (p as usize - vm() as usize) as u32;
            // This read loads (possibly new overlay) code; drop any stale JIT chunk.
            unsafe {
                shim_jit_invalidate_code_range(base, r as u32);
            }
            let mut idx: usize = 0;
            while idx < r {
                let addr = unsafe { wrap_segoff_addr(base, idx as u32) };
                let masked = unsafe { mask_addr(addr) };
                let byte = unsafe { *tmp.add(idx) };
                unsafe {
                    write_watch_log(masked, 1, byte as u32, file, func, line);
                    *vm().add(masked as usize) = byte;
                }
                idx += 1;
            }
        } else {
            unsafe {
                shim_log_stdout(
                    c"Trace: dos_read_file buffer %p outside virtual memory\n".as_ptr(),
                    buf,
                );
                libc::memcpy(buf, tmp as *const c_void, r);
            }
        }
        if r > 0 {
            unsafe {
                shim_log_stdout(c"Trace: dos_read_file data:".as_ptr());
                let to_dump = if r < 16 { r } else { 16 };
                let mut k: usize = 0;
                while k < to_dump {
                    shim_log_stdout(c" %02X".as_ptr(), *tmp.add(k) as c_int);
                    k += 1;
                }
                if r > to_dump {
                    shim_log_stdout(c" ...".as_ptr());
                }
                shim_log_stdout(c"\n".as_ptr());
            }
        }
        unsafe {
            libc::free(tmp as *mut c_void);
        }
        return 0;
    }
    unsafe {
        shim_log(
            c"dos_read_file_impl".as_ptr(),
            file,
            func,
            line,
            core::ptr::null(),
        );
    }
    set_ax(0);
    1
}

#[no_mangle]
pub extern "C" fn dos_read_file(handle: u16, buf: *mut c_void, len: u16) -> u8 {
    crit_enter(c"dos_read_file".as_ptr());
    let result = dos_read_file_impl(
        handle,
        buf,
        len,
        c"<external>".as_ptr(),
        c"dos_read_file".as_ptr(),
        0,
    );
    crit_exit(c"dos_read_file".as_ptr());
    result
}

#[no_mangle]
pub extern "C" fn dos_write_file_impl(
    handle: u16,
    buf: *const c_void,
    len: u16,
    file: *const c_char,
    func: *const c_char,
    line: c_int,
) -> u8 {
    unsafe {
        shim_log(
            c"dos_write_file_impl".as_ptr(),
            file,
            func,
            line,
            core::ptr::null(),
        );
    }
    if handle < MAX_DOS_HANDLES as u16 && !hget(handle as usize).is_null() {
        let path = if !hpget(handle as usize).is_null() {
            hpget(handle as usize) as *const c_char
        } else {
            c"<unnamed>".as_ptr()
        };
        unsafe {
            shim_log_stdout(
                c"Trace: dos_write_file: handle=%u path=%s len=%u\n".as_ptr(),
                handle as c_int,
                path,
                len as c_int,
            );
        }

        if len > 0 && len <= 128 && !buf.is_null() {
            unsafe {
                shim_log_stdout(c"         data: ".as_ptr());
                let bytes = buf as *const u8;
                let mut i: u16 = 0;
                while i < len {
                    let byte_val = *bytes.add(i as usize);
                    if byte_val >= 0x20 && byte_val <= 0x7E {
                        shim_log_stdout(c"%c".as_ptr(), byte_val as c_int);
                    } else if byte_val == b'\n' {
                        shim_log_stdout(c"\\n".as_ptr());
                    } else if byte_val == b'\r' {
                        shim_log_stdout(c"\\r".as_ptr());
                    } else {
                        shim_log_stdout(c"\\x%02X".as_ptr(), byte_val as c_int);
                    }
                    i += 1;
                }
                shim_log_stdout(c"\n".as_ptr());
            }
        }
        let mut src = buf as *const u8;
        let mut tmp: *mut u8 = core::ptr::null_mut();
        let src_in_virtual_memory =
            (src as usize) >= (vm() as usize) && (src as usize) < (vm() as usize) + MEMORY_SIZE;
        if src_in_virtual_memory && len > 0 {
            let base = (src as usize - vm() as usize) as u32;
            tmp = unsafe { libc::malloc(len as usize) as *mut u8 };
            if tmp.is_null() {
                set_ax(errno_to_dos(libc::ENOMEM));
                unsafe {
                    shim_log_stdout(c"Trace: dos_write_file allocation failure\n".as_ptr());
                }
                return 1;
            }
            let mut i: u16 = 0;
            while i < len {
                let addr = unsafe { wrap_segoff_addr(base, i as u32) };
                unsafe {
                    *tmp.add(i as usize) = *vm().add(mask_addr(addr) as usize);
                }
                i += 1;
            }
            src = tmp;
        }

        let r =
            unsafe { libc::fwrite(src as *const c_void, 1, len as usize, hget(handle as usize)) };
        unsafe {
            libc::free(tmp as *mut c_void);
        }
        if r == len as usize {
            set_ax(r as u16);
            return 0;
        }
        let err = if unsafe { libc::ferror(hget(handle as usize)) } != 0 {
            errno()
        } else {
            libc::EIO
        };
        set_ax(errno_to_dos(err));
        unsafe {
            shim_log_stdout(
                c"Trace: dos_write_file: handle=%u wrote %zu/%u bytes (errno=%d)\n".as_ptr(),
                handle as c_int,
                r,
                len as c_int,
                err,
            );
        }
        return 1;
    }
    set_ax(errno_to_dos(libc::EBADF));
    unsafe {
        shim_log_stdout(
            c"Trace: dos_write_file: invalid handle %u\n".as_ptr(),
            handle as c_int,
        );
    }
    1
}

#[no_mangle]
pub extern "C" fn dos_write_file(handle: u16, buf: *const c_void, len: u16) -> u8 {
    dos_write_file_impl(
        handle,
        buf,
        len,
        c"<external>".as_ptr(),
        c"dos_write_file".as_ptr(),
        0,
    )
}

#[no_mangle]
pub extern "C" fn dos_get_file_attributes_impl(
    path: *const c_char,
    attr: *mut u16,
    file: *const c_char,
    func: *const c_char,
    line: c_int,
) -> u8 {
    unsafe {
        shim_log(
            c"dos_get_file_attributes_impl".as_ptr(),
            file,
            func,
            line,
            path,
        );
    }
    let _ = path;
    let _ = attr;
    0
}

#[no_mangle]
pub extern "C" fn dos_get_file_attributes(path: *const c_char, attr: *mut u16) -> u8 {
    dos_get_file_attributes_impl(
        path,
        attr,
        c"<external>".as_ptr(),
        c"dos_get_file_attributes".as_ptr(),
        0,
    )
}

// AH=41h Delete File: no-op-success stub.
#[no_mangle]
pub extern "C" fn dos_delete_file_impl(
    path: *const c_char,
    file: *const c_char,
    func: *const c_char,
    line: c_int,
) -> u8 {
    unsafe {
        shim_log(c"dos_delete_file_impl".as_ptr(), file, func, line, path);
    }
    let _ = path;
    set_CF(0);
    0
}

#[no_mangle]
pub extern "C" fn dos_delete_file(path: *const c_char) -> u8 {
    dos_delete_file_impl(path, c"<external>".as_ptr(), c"dos_delete_file".as_ptr(), 0)
}

// AH=56h Rename File.
#[no_mangle]
pub extern "C" fn dos_rename_impl(
    old_path: *const c_char,
    new_path: *const c_char,
    file: *const c_char,
    func: *const c_char,
    line: c_int,
) -> u8 {
    unsafe {
        shim_log(c"dos_rename_impl".as_ptr(), file, func, line, old_path);
    }
    if old_path.is_null() || new_path.is_null() {
        set_CF(1);
        set_ax(3); // path not found
        return 1;
    }
    let mut old_buf = [0u8; 260];
    let mut new_buf = [0u8; 260];
    let o = dos_strip_drive_prefix(old_path) as *const u8;
    let n = dos_strip_drive_prefix(new_path) as *const u8;
    let mut i: usize = 0;
    unsafe {
        while i < old_buf.len() - 1 && *o.add(i) != 0 && *o.add(i) != b'\r' {
            old_buf[i] = *o.add(i);
            i += 1;
        }
        old_buf[i] = 0;
        i = 0;
        while i < new_buf.len() - 1 && *n.add(i) != 0 && *n.add(i) != b'\r' {
            new_buf[i] = *n.add(i);
            i += 1;
        }
        new_buf[i] = 0;
        shim_log_stdout(
            c"Trace: dos_rename: %s -> %s\n".as_ptr(),
            old_buf.as_ptr(),
            new_buf.as_ptr(),
        );
        if libc::rename(
            old_buf.as_ptr() as *const c_char,
            new_buf.as_ptr() as *const c_char,
        ) == 0
        {
            set_CF(0);
            set_ax(0);
            return 0;
        }
        if errno() == libc::ENOENT {
            // Source absent: treat as a no-op success (optional scratch file).
            set_CF(0);
            set_ax(0);
            return 0;
        }
        shim_log_stdout(
            c"Error: dos_rename: %s -> %s: %s\n".as_ptr(),
            old_buf.as_ptr(),
            new_buf.as_ptr(),
            libc::strerror(errno()),
        );
    }
    set_CF(1);
    set_ax(errno_to_dos(errno()));
    1
}

#[no_mangle]
pub extern "C" fn dos_rename(old_path: *const c_char, new_path: *const c_char) -> u8 {
    dos_rename_impl(
        old_path,
        new_path,
        c"<external>".as_ptr(),
        c"dos_rename".as_ptr(),
        0,
    )
}

// AH=44h IOCTL.
#[no_mangle]
pub extern "C" fn dos_ioctl_impl(
    subfunc: u8,
    bx_handle: u16,
    file: *const c_char,
    func: *const c_char,
    line: c_int,
) -> u8 {
    unsafe {
        shim_log(
            c"dos_ioctl_impl".as_ptr(),
            file,
            func,
            line,
            core::ptr::null(),
        );
    }
    let _ = bx_handle;
    if subfunc == 0x00 {
        set_dx(0); // bit 7 (ISDEV) clear => regular file; low byte = drive A
    } else if subfunc == 0x0D && (cx() & 0xFF) == 0x60 {
        let b = ((((ds() as u32) << 4) + dx() as u32) & 0xFFFFF) as usize;
        let dpb = |off: usize, v: u8| unsafe {
            *vm().add((b + off) & 0xFFFFF) = v;
        };
        let dpw = |off: usize, v: u16| {
            dpb(off, (v & 0xFF) as u8);
            dpb(off + 1, ((v >> 8) & 0xFF) as u8);
        };
        dpb(0x00, 0x00); // special functions: use the BPB below
        dpb(0x01, 0x05); // device type: 0x05 = fixed disk
        dpw(0x02, 0x0001); // device attributes: bit0=1 non-removable
        dpw(0x04, 0x0130); // number of cylinders
        dpb(0x06, 0x00); // media type
                         // device BPB at +7
        dpw(0x07, 512); // bytes per sector
        dpb(0x09, 0x04); // sectors per cluster
        dpw(0x0A, 1); // reserved sectors
        dpb(0x0C, 2); // number of FATs
        dpw(0x0D, 512); // root directory entries
        dpw(0x0F, 0); // total sectors (small; 0 => use large at +0x1C)
        dpb(0x11, 0xF8); // media descriptor: fixed disk
        dpw(0x12, 0x0080); // sectors per FAT
        dpw(0x14, 0x0011); // sectors per track
        dpw(0x16, 0x0004); // heads
        dpw(0x18, 0);
        dpw(0x1A, 0); // hidden sectors (dword)
        dpw(0x1C, 0x8000);
        dpw(0x1E, 0); // large total sectors (dword)
    }
    set_CF(0);
    0
}

#[no_mangle]
pub extern "C" fn dos_ioctl(subfunc: u8, bx_handle: u16) -> u8 {
    dos_ioctl_impl(
        subfunc,
        bx_handle,
        c"<external>".as_ptr(),
        c"dos_ioctl".as_ptr(),
        0,
    )
}

// AH=47h Get Current Directory: current directory is root, i.e. empty string.
#[no_mangle]
pub extern "C" fn dos_get_current_dir_impl(
    buf: *mut c_char,
    file: *const c_char,
    func: *const c_char,
    line: c_int,
) -> u8 {
    unsafe {
        shim_log(
            c"dos_get_current_dir_impl".as_ptr(),
            file,
            func,
            line,
            core::ptr::null(),
        );
    }
    if !buf.is_null() {
        unsafe {
            *buf = 0;
        }
    }
    set_CF(0);
    0
}

#[no_mangle]
pub extern "C" fn dos_get_current_dir(buf: *mut c_char) -> u8 {
    dos_get_current_dir_impl(
        buf,
        c"<external>".as_ptr(),
        c"dos_get_current_dir".as_ptr(),
        0,
    )
}

fn errno_to_dos(err: c_int) -> u16 {
    match err {
        libc::ENOENT => 2,                // file not found
        libc::ENOTDIR => 3,               // path not found
        libc::EACCES | libc::EPERM => 5,  // access denied
        libc::EMFILE | libc::ENFILE => 4, // too many open files
        libc::EBADF => 6,                 // invalid handle
        libc::EINVAL => 0x1F,             // invalid parameter
        _ => 1,                           // general failure
    }
}

#[no_mangle]
pub extern "C" fn dos_lseek_impl(
    handle: u16,
    off_hi: u16,
    off_lo: u16,
    origin: u8,
    file: *const c_char,
    func: *const c_char,
    line: c_int,
) -> u8 {
    unsafe {
        shim_log(
            c"dos_lseek_impl".as_ptr(),
            file,
            func,
            line,
            core::ptr::null(),
        );
    }
    if handle < MAX_DOS_HANDLES as u16 && !hget(handle as usize).is_null() {
        let off: libc::off_t;
        if origin == 0 {
            let off32 = ((off_hi as u32) << 16) | (off_lo as u32);
            off = off32 as libc::off_t;
        } else {
            let off32 = ((off_hi as u32) << 16) | (off_lo as u32);
            let s_off = off32 as i32;
            off = s_off as libc::off_t;
        }
        let whence: c_int = match origin {
            0 => libc::SEEK_SET,
            1 => libc::SEEK_CUR,
            2 => libc::SEEK_END,
            _ => {
                set_ax(errno_to_dos(libc::EINVAL));
                unsafe {
                    shim_log_stdout(
                        c"Trace: dos_lseek: invalid origin %u for handle %u\n".as_ptr(),
                        origin as c_int,
                        handle as c_int,
                    );
                }
                return 1;
            }
        };
        if unsafe { libc::fseeko(hget(handle as usize), off, whence) } == 0 {
            let pos = unsafe { libc::ftello(hget(handle as usize)) };
            if pos != -1 {
                let u_pos = pos as u32;
                set_ax((u_pos & 0xFFFF) as u16);
                set_dx((u_pos >> 16) as u16);
                unsafe {
                    shim_log_stdout(
                        c"Trace: dos_lseek: handle=%u -> 0x%08X\n".as_ptr(),
                        handle as c_int,
                        u_pos as c_int,
                    );
                }
                return 0;
            }
        }
        let err = errno();
        set_ax(errno_to_dos(err));
        unsafe {
            shim_log_stdout(
                c"Trace: dos_lseek: handle=%u failed (errno=%d)\n".as_ptr(),
                handle as c_int,
                err,
            );
        }
        return 1;
    }
    set_ax(errno_to_dos(libc::EBADF));
    unsafe {
        shim_log_stdout(
            c"Trace: dos_lseek: invalid handle %u\n".as_ptr(),
            handle as c_int,
        );
    }
    1
}

#[no_mangle]
pub extern "C" fn dos_lseek(handle: u16, off_hi: u16, off_lo: u16, origin: u8) -> u8 {
    crit_enter(c"dos_lseek".as_ptr());
    let result = dos_lseek_impl(
        handle,
        off_hi,
        off_lo,
        origin,
        c"<external>".as_ptr(),
        c"dos_lseek".as_ptr(),
        0,
    );
    crit_exit(c"dos_lseek".as_ptr());
    result
}

// ---- find-first helpers ----------------------------------------------------

fn format_search_template(dest: *mut u8, spec: *const c_char) {
    let spec = if spec.is_null() || unsafe { *(spec as *const u8) } == 0 {
        c"*.*".as_ptr()
    } else {
        spec
    };
    unsafe {
        for j in 0..11 {
            *dest.add(j) = b' ';
        }
        let dot = libc::strrchr(spec, b'.' as c_int);
        let name_len_full = if !dot.is_null() {
            (dot as usize) - (spec as usize)
        } else {
            libc::strlen(spec)
        };
        let name_len = if name_len_full > 8 { 8 } else { name_len_full };
        let sp = spec as *const u8;
        for j in 0..name_len {
            *dest.add(j) = libc::toupper(*sp.add(j) as c_int) as u8;
        }
        if !dot.is_null() && *(dot as *const u8).add(1) != 0 {
            let dot = dot.add(1);
            let ext_len_full = libc::strlen(dot);
            let ext_len = if ext_len_full > 3 { 3 } else { ext_len_full };
            let dp = dot as *const u8;
            for j in 0..ext_len {
                *dest.add(8 + j) = libc::toupper(*dp.add(j) as c_int) as u8;
            }
        }
    }
}

fn format_dos_filename(dest: *mut c_char, src: *const c_char) {
    if src.is_null() {
        unsafe {
            *dest = 0;
        }
        return;
    }
    unsafe {
        let dot = libc::strrchr(src, b'.' as c_int);
        let mut name_len = if !dot.is_null() {
            (dot as usize) - (src as usize)
        } else {
            libc::strlen(src)
        };
        let sp = src as *const u8;
        while name_len > 0 && *sp.add(name_len - 1) == b' ' {
            name_len -= 1;
        }
        if name_len > 8 {
            name_len = 8;
        }
        let mut base = [0 as c_char; 9];
        for j in 0..name_len {
            base[j] = libc::toupper(*sp.add(j) as c_int) as c_char;
        }
        base[name_len] = 0;
        let mut ext = [0 as c_char; 4];
        if !dot.is_null() && *(dot as *const u8).add(1) != 0 {
            let dot = dot.add(1);
            let ext_len_full = libc::strlen(dot);
            let ext_len = if ext_len_full > 3 { 3 } else { ext_len_full };
            let dp = dot as *const u8;
            for j in 0..ext_len {
                ext[j] = libc::toupper(*dp.add(j) as c_int) as c_char;
            }
            ext[ext_len] = 0;
        }
        if ext[0] != 0 {
            libc::snprintf(dest, 13, c"%s.%s".as_ptr(), base.as_ptr(), ext.as_ptr());
        } else {
            libc::snprintf(dest, 13, c"%s".as_ptr(), base.as_ptr());
        }
    }
}

fn dos_encode_time(tm_ptr: *const libc::tm) -> u16 {
    let (hour, minute, second) = if tm_ptr.is_null() {
        (0, 0, 0)
    } else {
        unsafe { ((*tm_ptr).tm_hour, (*tm_ptr).tm_min, (*tm_ptr).tm_sec) }
    };
    (((hour & 0x1F) << 11) | ((minute & 0x3F) << 5) | ((second / 2) & 0x1F)) as u16
}

fn dos_encode_date(tm_ptr: *const libc::tm) -> u16 {
    let mut year = if tm_ptr.is_null() {
        1980
    } else {
        unsafe { (*tm_ptr).tm_year + 1900 }
    };
    let mut month = if tm_ptr.is_null() {
        1
    } else {
        unsafe { (*tm_ptr).tm_mon + 1 }
    };
    let mut day = if tm_ptr.is_null() {
        1
    } else {
        unsafe { (*tm_ptr).tm_mday }
    };
    if year < 1980 {
        year = 1980;
        month = 1;
        day = 1;
    }
    ((((year - 1980) & 0x7F) << 9) | ((month & 0x0F) << 5) | (day & 0x1F)) as u16
}

#[repr(C, packed)]
struct FindFirstBlock {
    reserved: [u8; 21],
    attr: u8,
    time: u16,
    date: u16,
    size: u32,
    name: [c_char; 13],
}

#[inline(always)]
fn s_isdir(mode: libc::mode_t) -> bool {
    (mode & libc::S_IFMT) == libc::S_IFDIR
}
#[inline(always)]
fn s_isreg(mode: libc::mode_t) -> bool {
    (mode & libc::S_IFMT) == libc::S_IFREG
}

#[no_mangle]
pub extern "C" fn dos_find_first_impl(
    path: *const c_char,
    attr: u16,
    file: *const c_char,
    func: *const c_char,
    line: c_int,
) -> u8 {
    unsafe {
        shim_log(c"dos_find_first_impl".as_ptr(), file, func, line, path);
    }
    if unsafe { dta_ptr }.is_null() {
        set_CF(1);
        set_ax(0x1F); // invalid parameter
        return 1;
    }

    let mut buf = [0u8; PATH_MAX_USIZE];
    let mut i: usize = 0;
    if !path.is_null() {
        let pp = path as *const u8;
        unsafe {
            while i < buf.len() - 1 && *pp.add(i) != 0 && *pp.add(i) != b'\r' {
                let mut current = *pp.add(i);
                if current == b'\\' {
                    current = b'/';
                }
                buf[i] = current;
                i += 1;
            }
        }
    }
    buf[i] = 0;

    let buf_ptr = buf.as_mut_ptr();
    let mut filespec = buf_ptr;
    let colon = unsafe { libc::strchr(filespec as *const c_char, b':' as c_int) };
    if !colon.is_null() {
        filespec = unsafe { (colon as *mut u8).add(1) };
    }

    unsafe {
        while *filespec == b'/' {
            filespec = filespec.add(1);
        }
    }

    if unsafe { *filespec } == 0 {
        set_CF(1);
        set_ax(0x03); // path not found
        return 1;
    }

    let mut pattern = filespec as *const u8;
    let mut dirpath = c".".as_ptr();
    let last_sep = unsafe { libc::strrchr(filespec as *const c_char, b'/' as c_int) };
    if !last_sep.is_null() {
        unsafe {
            *(last_sep as *mut u8) = 0;
        }
        pattern = unsafe { (last_sep as *const u8).add(1) };
        dirpath = if unsafe { *filespec } != 0 {
            filespec as *const c_char
        } else {
            c"/".as_ptr()
        };
    }

    if unsafe { *pattern } == 0 {
        pattern = c"*.*".as_ptr() as *const u8;
    }

    let dir = unsafe { libc::opendir(dirpath) };
    if dir.is_null() {
        let err = errno();
        set_CF(1);
        set_ax(if err == libc::ENOENT || err == libc::ENOTDIR {
            0x03
        } else {
            0x05
        }); // access denied
        return 1;
    }

    let dta = unsafe { dta_ptr } as *mut FindFirstBlock;

    let fnm_flags: c_int = libc::FNM_CASEFOLD;

    let mut found = 0;
    loop {
        let ent = unsafe { libc::readdir(dir) };
        if ent.is_null() {
            break;
        }
        let name = unsafe { (*ent).d_name.as_ptr() };
        if unsafe { libc::strcmp(name, c".".as_ptr()) } == 0
            || unsafe { libc::strcmp(name, c"..".as_ptr()) } == 0
        {
            continue;
        }
        if unsafe { libc::fnmatch(pattern as *const c_char, name, fnm_flags) } != 0 {
            continue;
        }

        let mut full_path = [0u8; PATH_MAX_USIZE];
        let fp = full_path.as_mut_ptr() as *mut c_char;
        unsafe {
            if libc::strcmp(dirpath, c".".as_ptr()) == 0 {
                libc::snprintf(fp, full_path.len(), c"%s".as_ptr(), name);
            } else if libc::strcmp(dirpath, c"/".as_ptr()) == 0 {
                libc::snprintf(fp, full_path.len(), c"/%s".as_ptr(), name);
            } else {
                let len = libc::strlen(dirpath);
                let fmt = if len > 0 && *(dirpath as *const u8).add(len - 1) == b'/' {
                    c"%s%s".as_ptr()
                } else {
                    c"%s/%s".as_ptr()
                };
                libc::snprintf(fp, full_path.len(), fmt, dirpath, name);
            }
        }

        let mut st: libc::stat = unsafe { core::mem::zeroed() };
        if unsafe { libc::stat(fp as *const c_char, &mut st) } != 0 {
            continue;
        }

        let mut file_attr: u8 = 0;
        if s_isdir(st.st_mode) {
            file_attr |= 0x10;
        } else if s_isreg(st.st_mode) {
            file_attr |= 0x20;
        }
        if unsafe { libc::access(fp as *const c_char, libc::W_OK) } != 0 {
            file_attr |= 0x01;
        }
        if unsafe { *(name as *const u8) } == b'.' {
            file_attr |= 0x02;
        }

        if (file_attr & 0x02) != 0 && (attr & 0x02) == 0 {
            continue;
        }
        if (file_attr & 0x04) != 0 && (attr & 0x04) == 0 {
            continue;
        }
        if (file_attr & 0x10) != 0 && (attr & 0x10) == 0 {
            continue;
        }
        if (file_attr & 0x08) != 0 && (attr & 0x08) == 0 {
            continue;
        }

        unsafe {
            core::ptr::write_bytes(dta as *mut u8, 0, core::mem::size_of::<FindFirstBlock>());
            let reserved = core::ptr::addr_of_mut!((*dta).reserved) as *mut u8;
            *reserved.add(0) = 0; // default drive
            format_search_template(reserved.add(1), pattern as *const c_char);
            *reserved.add(12) = attr as u8;
            core::ptr::addr_of_mut!((*dta).attr).write(file_attr);

            let mtime: libc::time_t = st.st_mtime;
            let mut tm_buf: libc::tm = core::mem::zeroed();
            let tm_ptr = libc::localtime(&mtime);
            if !tm_ptr.is_null() {
                tm_buf = *tm_ptr;
            } else {
                tm_buf.tm_year = 80;
                tm_buf.tm_mon = 0;
                tm_buf.tm_mday = 1;
            }
            core::ptr::addr_of_mut!((*dta).time).write_unaligned(dos_encode_time(&tm_buf));
            core::ptr::addr_of_mut!((*dta).date).write_unaligned(dos_encode_date(&tm_buf));
            let size_val: u32 = if s_isdir(st.st_mode) {
                0
            } else {
                st.st_size as u32
            };
            core::ptr::addr_of_mut!((*dta).size).write_unaligned(size_val);
            format_dos_filename(core::ptr::addr_of_mut!((*dta).name) as *mut c_char, name);
        }

        found = 1;
        break;
    }

    unsafe {
        libc::closedir(dir);
    }

    if found == 0 {
        set_CF(1);
        set_ax(0x02); // file not found
        return 1;
    }

    set_CF(0);
    set_ax(0);
    0
}

#[no_mangle]
pub extern "C" fn dos_find_first(path: *const c_char, attr: u16) -> u8 {
    dos_find_first_impl(
        path,
        attr,
        c"<external>".as_ptr(),
        c"dos_find_first".as_ptr(),
        0,
    )
}

// ---- memory services -------------------------------------------------------

#[no_mangle]
pub extern "C" fn dos_alloc_mem_impl(
    parags: u16,
    file: *const c_char,
    func: *const c_char,
    line: c_int,
) -> u8 {
    unsafe {
        shim_log(
            c"dos_alloc_mem_impl".as_ptr(),
            file,
            func,
            line,
            core::ptr::null(),
        );
    }
    let end_seg = unsafe { next_free_seg } as u32 + parags as u32;
    if end_seg > CONVENTIONAL_TOP_SEG {
        set_CF(1);
        set_ax(8);
        let nfs = unsafe { next_free_seg } as u32;
        let avail = if nfs < CONVENTIONAL_TOP_SEG {
            CONVENTIONAL_TOP_SEG - nfs
        } else {
            0
        };
        set_bx(avail as u16);
        return 1;
    }
    set_ax(unsafe { next_free_seg });
    unsafe {
        dos_last_alloc_seg = next_free_seg;
        next_free_seg = end_seg as u16;
    }
    set_CF(0);
    0
}

#[no_mangle]
pub extern "C" fn dos_alloc_mem(parags: u16) -> u8 {
    crit_enter(c"dos_alloc_mem".as_ptr());
    let result = dos_alloc_mem_impl(parags, c"<external>".as_ptr(), c"dos_alloc_mem".as_ptr(), 0);
    crit_exit(c"dos_alloc_mem".as_ptr());
    result
}

#[no_mangle]
pub extern "C" fn dos_free_mem_impl(
    ptr: *mut c_void,
    file: *const c_char,
    func: *const c_char,
    line: c_int,
) -> u8 {
    unsafe {
        shim_log(
            c"dos_free_mem_impl".as_ptr(),
            file,
            func,
            line,
            core::ptr::null(),
        );
    }
    let _ = ptr;
    set_CF(0);
    0
}

#[no_mangle]
pub extern "C" fn dos_free_mem(ptr: *mut c_void) -> u8 {
    dos_free_mem_impl(ptr, c"<external>".as_ptr(), c"dos_free_mem".as_ptr(), 0)
}

#[no_mangle]
pub extern "C" fn dos_resize_mem_impl(
    segment: u16,
    parags: u16,
    file: *const c_char,
    func: *const c_char,
    line: c_int,
) -> u8 {
    unsafe {
        shim_log(
            c"dos_resize_mem_impl".as_ptr(),
            file,
            func,
            line,
            core::ptr::null(),
        );
        shim_log_stdout(
            c"Trace: dos_resize_mem: segment=0x%04X parags=0x%04X (min=0x%04X)\n".as_ptr(),
            segment as c_int,
            parags as c_int,
            program_min_block_paras as c_int,
        );
    }
    let max_paras: u32 = CONVENTIONAL_TOP_SEG;
    if segment < unsafe { psp_seg } || (segment as u32) >= unsafe { next_free_seg } as u32 {
        set_CF(1);
        set_ax(9); // invalid memory block
        set_bx(0);
        return 1;
    }
    if (segment as u32) >= max_paras {
        set_CF(1);
        set_ax(8);
        set_bx(0);
        return 1;
    }
    let requested_top = (segment as u32) + (parags as u32);
    if requested_top > max_paras {
        set_CF(1);
        set_ax(8); // insufficient memory
        let mut avail = max_paras - segment as u32;
        if avail > 0xFFFF {
            avail = 0xFFFF;
        }
        set_bx(avail as u16);
        return 1;
    }
    if segment == unsafe { psp_seg } {
        let min_required: u16 = 0x10; // the PSP itself (LOAD_SEG = PSP_SEG + 0x10)
        if parags < min_required {
            set_CF(1);
            set_ax(8); // insufficient memory for requested block
            set_bx(min_required);
            return 1;
        }
        unsafe {
            program_min_block_paras = parags;
            next_free_seg = requested_top as u16;
        }
        set_CF(0);
        set_ax(0);
        return 0;
    }
    if segment == unsafe { dos_last_alloc_seg } {
        unsafe {
            next_free_seg = requested_top as u16;
        }
    }
    set_CF(0);
    set_ax(0);
    0
}

#[no_mangle]
pub extern "C" fn dos_resize_mem(segment: u16, parags: u16) -> u8 {
    dos_resize_mem_impl(
        segment,
        parags,
        c"<external>".as_ptr(),
        c"dos_resize_mem".as_ptr(),
        0,
    )
}

// AH=3Ch Create File.
#[no_mangle]
pub extern "C" fn dos_create_file_impl(
    path: *const c_char,
    attr: u16,
    file: *const c_char,
    func: *const c_char,
    line: c_int,
) -> u8 {
    unsafe {
        shim_log(c"dos_create_file_impl".as_ptr(), file, func, line, path);
    }
    let _ = attr;
    if path.is_null()
        || unsafe { *(path as *const u8) } == 0
        || unsafe { *(path as *const u8) } == b'\r'
    {
        set_CF(1);
        set_ax(3); // path not found
        return 1;
    }
    let mut buf = [0u8; 260];
    let mut i: usize = 0;
    let src_path = dos_strip_drive_prefix(path) as *const u8;
    unsafe {
        while i < buf.len() - 1 && *src_path.add(i) != 0 && *src_path.add(i) != b'\r' {
            buf[i] = *src_path.add(i);
            i += 1;
        }
    }
    buf[i] = 0;
    if buf[0] == 0 {
        set_CF(1);
        set_ax(3); // path not found (drive-only spec)
        return 1;
    }
    for h in 0..MAX_DOS_HANDLES {
        if hget(h).is_null() {
            let fp =
                unsafe { fopen_case_insensitive(buf.as_ptr() as *const c_char, c"wb+".as_ptr()) };
            hset(h, fp);
            hmset(h, HANDLE_MODE_WRITE);
            if fp.is_null() {
                unsafe {
                    shim_log_stdout(
                        c"Error: dos_create_file: %s: %s\n".as_ptr(),
                        buf.as_ptr(),
                        libc::strerror(errno()),
                    );
                }
                set_CF(1);
                set_ax(5); // access denied
                return 1;
            }
            hpset(h, unsafe { libc::strdup(buf.as_ptr() as *const c_char) });
            hoset(h, true);
            set_ax(h as u16);
            set_bx(h as u16);
            set_CF(0);
            return 0;
        }
    }
    unsafe {
        shim_log_stdout(
            c"Error: dos_create_file: no handles available for %s\n".as_ptr(),
            buf.as_ptr(),
        );
    }
    set_CF(1);
    set_ax(4); // too many open files
    1
}

#[no_mangle]
pub extern "C" fn dos_create_file(path: *const c_char, attr: u16) -> u8 {
    dos_create_file_impl(
        path,
        attr,
        c"<external>".as_ptr(),
        c"dos_create_file".as_ptr(),
        0,
    )
}

// AH=2Fh Get DTA.
#[no_mangle]
pub extern "C" fn dos_get_dta_impl(file: *const c_char, func: *const c_char, line: c_int) -> u8 {
    unsafe {
        shim_log(
            c"dos_get_dta_impl".as_ptr(),
            file,
            func,
            line,
            core::ptr::null(),
        );
    }
    let p = unsafe { dta_ptr } as usize;
    let vmb = vm() as usize;
    if p >= vmb && p < vmb + MEMORY_SIZE {
        let a = (p - vmb) as u32;
        set_es((a >> 4) as u16);
        set_bx((a & 0xF) as u16);
    } else {
        set_es(0);
        set_bx(0);
    }
    set_CF(0);
    0
}

#[no_mangle]
pub extern "C" fn dos_get_dta() -> u8 {
    dos_get_dta_impl(c"<external>".as_ptr(), c"dos_get_dta".as_ptr(), 0)
}

// AH=4Fh Find Next. find_first matches nothing more here: report "no more files".
#[no_mangle]
pub extern "C" fn dos_find_next_impl(file: *const c_char, func: *const c_char, line: c_int) -> u8 {
    unsafe {
        shim_log(
            c"dos_find_next_impl".as_ptr(),
            file,
            func,
            line,
            core::ptr::null(),
        );
    }
    set_CF(1);
    set_ax(0x12); // no more files
    1
}

#[no_mangle]
pub extern "C" fn dos_find_next() -> u8 {
    dos_find_next_impl(c"<external>".as_ptr(), c"dos_find_next".as_ptr(), 0)
}

// AH=0Ch Flush keyboard buffer then invoke input function AL.
#[no_mangle]
pub extern "C" fn dos_flush_and_read_impl(
    subfunc: u8,
    file: *const c_char,
    func: *const c_char,
    line: c_int,
) -> u8 {
    unsafe {
        shim_log(
            c"dos_flush_and_read_impl".as_ptr(),
            file,
            func,
            line,
            core::ptr::null(),
        );
    }
    let _ = subfunc;
    set_CF(0);
    0
}

#[no_mangle]
pub extern "C" fn dos_flush_and_read(subfunc: u8) -> u8 {
    dos_flush_and_read_impl(
        subfunc,
        c"<external>".as_ptr(),
        c"dos_flush_and_read".as_ptr(),
        0,
    )
}

#[no_mangle]
pub extern "C" fn dos_exec_impl(
    param_block: *mut c_void,
    cmd: *const c_char,
    file: *const c_char,
    func: *const c_char,
    line: c_int,
) -> u8 {
    unsafe {
        shim_log(c"dos_exec_impl".as_ptr(), file, func, line, cmd);
    }
    let parent_next_free = unsafe { next_free_seg };
    let child_psp = unsafe { next_free_seg };
    let child_load = child_psp.wrapping_add(0x10);
    unsafe {
        libc::memcpy(
            vm().add((child_psp as usize) << 4) as *mut c_void,
            vm().add((psp_seg as usize) << 4) as *const c_void,
            0x100,
        );
    }
    if !param_block.is_null() {
        let pb = param_block as *const u8;
        unsafe {
            let tail_off = (*pb.add(2) as u16) | ((*pb.add(3) as u16) << 8);
            let tail_seg = (*pb.add(4) as u16) | ((*pb.add(5) as u16) << 8);
            let tail_lin = ((((tail_seg as u32) << 4) + tail_off as u32) & 0xFFFFF) as usize;
            let mut len = *vm().add(tail_lin); // char count, excl. len byte + CR
            if len > 0x7E {
                len = 0x7E; // PSP tail area is 0x80..0xFF
            }
            let dst = ((child_psp as usize) << 4) + 0x80;
            *vm().add(dst) = len;
            let mut i: u16 = 0;
            while i < len as u16 {
                *vm().add((dst + 1 + i as usize) & 0xFFFFF) =
                    *vm().add((tail_lin + 1 + i as usize) & 0xFFFFF);
                i += 1;
            }
            *vm().add((dst + 1 + len as usize) & 0xFFFFF) = 0x0D; // CR terminator
        }
    }
    let mut new_cs: u16 = 0;
    let mut new_ip: u16 = 0;
    let mut new_ss: u16 = 0;
    let mut new_sp: u16 = 0;
    if unsafe {
        load_executable(
            cmd,
            child_load,
            1,
            &mut new_cs,
            &mut new_ip,
            &mut new_ss,
            &mut new_sp,
        )
    } != 0
    {
        set_CF(1);
        set_ax(2);
        return 1;
    }

    unsafe {
        shim_exec_run_child(new_cs, new_ip, new_ss, new_sp, child_psp);
    }
    // DOS frees the child's block when it exits: reclaim the arena.
    unsafe {
        next_free_seg = parent_next_free;
    }
    set_CF(0);
    0
}

#[no_mangle]
pub extern "C" fn dos_exec(param_block: *mut c_void, cmd: *const c_char) -> u8 {
    dos_exec_impl(
        param_block,
        cmd,
        c"<external>".as_ptr(),
        c"dos_exec".as_ptr(),
        0,
    )
}

#[no_mangle]
pub extern "C" fn dos_exit_impl(file: *const c_char, func: *const c_char, line: c_int) {
    unsafe {
        shim_log(
            c"dos_exit_impl".as_ptr(),
            file,
            func,
            line,
            core::ptr::null(),
        );
    }
    for i in 0..16 {
        let cur = unsafe { *vm().add(i) };
        let initial = unsafe {
            core::ptr::addr_of!(null_guard_initial)
                .cast::<u8>()
                .add(i)
                .read()
        };
        if cur != initial {
            unsafe {
                shim_log_stdout(
                    c"Warning: null guard changed at 0x%02X initial=0x%02X current=0x%02X\n"
                        .as_ptr(),
                    i as c_int,
                    initial as c_int,
                    cur as c_int,
                );
            }
        }
    }
    // DOS function 0x4C places the exit code in AL.
    let status = al() as c_int;

    // Record it for a later AH=4Dh from the parent (normal termination => AH=0).
    unsafe {
        dos_child_return_code = status as u8 as u16;
    }

    // If this program was EXEC'd by a parent, its termination returns to the
    // parent rather than ending the process.
    if unsafe { shim_exec_child_terminate(status) } != 0 {
        return; // not reached: longjmp'd to the parent's EXEC
    }

    let ab = unsafe { shim_active_binary() };
    let ab_or = if !ab.is_null() {
        ab
    } else {
        c"<none>".as_ptr()
    };
    let file_or = if !file.is_null() { file } else { c"?".as_ptr() };
    let func_or = if !func.is_null() { func } else { c"?".as_ptr() };
    unsafe {
        libc::fprintf(
            stderr,
            c"\n[EXIT] DOS terminate (INT 21h AH=4Ch / INT 20h) called from cs:ip=%04X:%04X active_binary=%s exit_status=%d\n[EXIT]   triggering C site: %s:%s:%d\n[EXIT]   this is a CLEAN game-initiated exit, not a crash. No bundle written.\n".as_ptr(),
            cs() as c_int, ip() as c_int, ab_or, status, file_or, func_or, line,
        );
        save_manager_sr_log(
            c"exit DOS_TERMINATE cs:ip=%04X:%04X active=%s status=%d from %s:%s:%d (clean exit, not a crash)".as_ptr(),
            cs() as c_int, ip() as c_int, ab_or, status, file_or, func_or, line,
        );

        machine_halted = 1;
        libc::fflush(stdout);
        libc::fflush(stderr);
        libc::exit(status);
    }
}

#[no_mangle]
pub extern "C" fn dos_exit() {
    dos_exit_impl(c"<external>".as_ptr(), c"dos_exit".as_ptr(), 0)
}

// INT 21h AH=4Dh: Get Return Code of Subprocess (read-once).
#[no_mangle]
pub extern "C" fn dos_get_return_code_impl(
    file: *const c_char,
    func: *const c_char,
    line: c_int,
) -> u8 {
    unsafe {
        shim_log(
            c"dos_get_return_code_impl".as_ptr(),
            file,
            func,
            line,
            core::ptr::null(),
        );
    }
    set_ax(unsafe { dos_child_return_code });
    unsafe {
        dos_child_return_code = 0;
    }
    0 // CF clear
}

// DOS API AH=0x10: close file using an FCB.
#[no_mangle]
pub extern "C" fn dos_close_fcb_impl(
    dx_val: u16,
    file: *const c_char,
    func: *const c_char,
    line: c_int,
) -> u8 {
    unsafe {
        shim_log(
            c"dos_close_fcb_impl".as_ptr(),
            file,
            func,
            line,
            core::ptr::null(),
        );
    }
    let _ = dx_val; // DS:DX -> FCB
    0
}

#[no_mangle]
pub extern "C" fn dos_close_fcb(dx_val: u16) -> u8 {
    dos_close_fcb_impl(dx_val, c"<external>".as_ptr(), c"dos_close_fcb".as_ptr(), 0)
}

fn dos_fcb_is_separator(c: u8) -> c_int {
    if c < 0x20 {
        return 1;
    }
    match c {
        b' ' | b'.' | b':' | b';' | b',' | b'=' | b'+' | b'/' | b'\\' | b'"' | b'[' | b']'
        | b'|' | b'<' | b'>' | 0x7F => 1,
        _ => 0,
    }
}

// INT 21h AH=29h: Parse a filename at DS:SI into the FCB at ES:DI.
#[no_mangle]
pub extern "C" fn dos_parse_filename_impl(
    file: *const c_char,
    func: *const c_char,
    line: c_int,
) -> u8 {
    unsafe {
        shim_log(
            c"dos_parse_filename_impl".as_ptr(),
            file,
            func,
            line,
            core::ptr::null(),
        );
    }
    let ctrl = al();
    let s = seg_off(ds(), si()) as *const u8;
    let fcb = seg_off(es(), di()) as *mut u8;
    let mut pos: u16 = 0;
    let mut wildcard = 0;

    unsafe {
        if ctrl & 0x01 != 0 {
            while *s.add(pos as usize) == b' ' || *s.add(pos as usize) == b'\t' {
                pos += 1;
            }
        }

        // Optional "X:" drive.
        let mut drive: u8 = 0; // 0 = default/current
        let mut drive_present = 0;
        if *s.add(pos as usize) != 0 && *s.add(pos as usize + 1) == b':' {
            let mut c = *s.add(pos as usize);
            if c >= b'a' && c <= b'z' {
                c = c - 0x20;
            }
            if c >= b'A' && c <= b'Z' {
                drive = c - b'A' + 1;
                drive_present = 1;
                pos += 2;
            }
        }
        if drive_present != 0 || (ctrl & 0x02) == 0 {
            *fcb.add(0) = drive;
        }

        let mut name = [b' '; 8];
        let mut ext = [b' '; 3];

        let mut fi = 0;
        let mut name_present = 0;
        while *s.add(pos as usize) != 0 && dos_fcb_is_separator(*s.add(pos as usize)) == 0 {
            let mut c = *s.add(pos as usize);
            name_present = 1;
            if c == b'*' {
                while fi < 8 {
                    name[fi] = b'?';
                    fi += 1;
                }
                wildcard = 1;
                pos += 1;
                break;
            }
            if c == b'?' {
                wildcard = 1;
            }
            if c >= b'a' && c <= b'z' {
                c = c - 0x20;
            }
            if fi < 8 {
                name[fi] = c;
                fi += 1;
            }
            pos += 1;
        }
        while *s.add(pos as usize) != 0
            && *s.add(pos as usize) != b'.'
            && dos_fcb_is_separator(*s.add(pos as usize)) == 0
        {
            pos += 1;
        }

        let mut ext_present = 0;
        if *s.add(pos as usize) == b'.' {
            pos += 1;
            let mut ei = 0;
            while *s.add(pos as usize) != 0 && dos_fcb_is_separator(*s.add(pos as usize)) == 0 {
                let mut c = *s.add(pos as usize);
                ext_present = 1;
                if c == b'*' {
                    while ei < 3 {
                        ext[ei] = b'?';
                        ei += 1;
                    }
                    wildcard = 1;
                    pos += 1;
                    break;
                }
                if c == b'?' {
                    wildcard = 1;
                }
                if c >= b'a' && c <= b'z' {
                    c = c - 0x20;
                }
                if ei < 3 {
                    ext[ei] = c;
                    ei += 1;
                }
                pos += 1;
            }
            while *s.add(pos as usize) != 0 && dos_fcb_is_separator(*s.add(pos as usize)) == 0 {
                pos += 1;
            }
        }

        if name_present != 0 || (ctrl & 0x04) == 0 {
            for i in 0..8 {
                *fcb.add(1 + i) = name[i];
            }
        }
        if ext_present != 0 || (ctrl & 0x08) == 0 {
            for i in 0..3 {
                *fcb.add(9 + i) = ext[i];
            }
        }
    }

    set_si(si().wrapping_add(pos));
    set_al(if wildcard != 0 { 1 } else { 0 });
    0
}

#[no_mangle]
pub extern "C" fn dos_parse_filename() -> u8 {
    dos_parse_filename_impl(c"<external>".as_ptr(), c"dos_parse_filename".as_ptr(), 0)
}

// ---- INT 21h dispatcher for runtime-dynamic AH -----------------------------

#[no_mangle]
pub extern "C" fn dos_api_impl(file: *const c_char, func: *const c_char, line: c_int) -> u8 {
    crit_enter(c"dos_api_impl".as_ptr());
    unsafe {
        shim_log(
            c"dos_api_impl".as_ptr(),
            file,
            func,
            line,
            core::ptr::null(),
        );
    }
    let mut result: u8 = 0;
    match ah() {
        0x09 => {
            result = dos_print_string_impl(seg_off(ds(), dx()) as *const c_char, file, func, line);
            set_CF(result);
        }
        0x25 => {
            result = dos_set_interrupt_vector_impl(al(), ds(), dx(), file, func, line);
            set_CF(result);
        }
        0x29 => {
            result = dos_parse_filename_impl(file, func, line);
        }
        0x2C => {
            unsafe {
                let mut tv: libc::timeval = core::mem::zeroed();
                libc::gettimeofday(&mut tv, core::ptr::null_mut());
                let tm = libc::localtime(&tv.tv_sec);
                set_ch((*tm).tm_hour as u8);
                set_cl((*tm).tm_min as u8);
                set_dh((*tm).tm_sec as u8);
                set_dl((tv.tv_usec / 10000) as u8);
            }
            set_CF(0);
        }
        0x30 => {
            set_ax(0x0003); // DOS 3.00 (AL=major, AH=minor)
            set_bh(0x00); // OEM number (IBM/MS-DOS)
            set_bl(0x00); // Version flags
            set_cx(0);
            set_CF(0);
        }
        0x50 => {
            unsafe {
                dos_current_psp = bx();
            }
            set_CF(0);
        }
        0x51 | 0x62 => {
            set_bx(unsafe { dos_current_psp });
            set_CF(0);
        }
        0x01 => set_CF(dos_read_char_impl(file, func, line)),
        0x02 => set_CF(dos_write_char_impl(dl(), file, func, line)),
        0x06 => set_CF(dos_direct_console_io_impl(dl(), file, func, line)),
        0x07 => set_CF(dos_console_input_no_echo_impl(file, func, line)),
        0x0A => set_CF(dos_buffered_input_impl(file, func, line)),
        0x0B => set_CF(dos_check_keyboard_status_impl(file, func, line)),
        0x0C => set_CF(dos_flush_and_read_impl(al(), file, func, line)),
        0x0D => set_CF(dos_reset_disk_impl(file, func, line)),
        0x0E => set_CF(dos_select_drive_impl(dl(), file, func, line)),
        0x10 => set_CF(dos_close_fcb_impl(dx(), file, func, line)),
        0x19 => set_CF(dos_get_current_drive_impl(file, func, line)),
        0x1A => set_CF(dos_set_dta_impl(
            seg_off(ds(), dx()) as *mut c_void,
            file,
            func,
            line,
        )),
        0x2A => set_CF(dos_get_date_impl(file, func, line)),
        0x2F => set_CF(dos_get_dta_impl(file, func, line)),
        0x35 => set_CF(dos_get_interrupt_vector_impl(file, func, line)),
        0x36 => set_CF(dos_get_disk_free_space_impl(dl(), file, func, line)),
        0x39 => set_CF(dos_make_dir_impl(
            seg_off(ds(), dx()) as *const c_char,
            file,
            func,
            line,
        )),
        0x3B => set_CF(dos_change_dir_impl(
            seg_off(ds(), dx()) as *const c_char,
            file,
            func,
            line,
        )),
        0x3C => set_CF(dos_create_file_impl(
            seg_off(ds(), dx()) as *const c_char,
            cx(),
            file,
            func,
            line,
        )),
        0x41 => set_CF(dos_delete_file_impl(
            seg_off(ds(), dx()) as *const c_char,
            file,
            func,
            line,
        )),
        0x43 => {
            let cx_ptr = unsafe { core::ptr::addr_of_mut!((*cpu_ptr()).r_cx) };
            set_CF(dos_get_file_attributes_impl(
                seg_off(ds(), dx()) as *const c_char,
                cx_ptr,
                file,
                func,
                line,
            ));
        }
        0x44 => set_CF(dos_ioctl_impl(al(), bx(), file, func, line)),
        0x47 => set_CF(dos_get_current_dir_impl(
            seg_off(ds(), si()) as *mut c_char,
            file,
            func,
            line,
        )),
        0x48 => set_CF(dos_alloc_mem_impl(bx(), file, func, line)),
        0x49 => set_CF(dos_free_mem_impl(
            seg_off(es(), 0) as *mut c_void,
            file,
            func,
            line,
        )),
        0x4B => {
            if al() == 0x03 {
                // AL=03h Load Overlay: param block is {word load_seg, word reloc_factor}.
                let pb = seg_off(es(), bx()) as *const u16;
                let load_seg = unsafe { core::ptr::read_unaligned(pb) };
                let reloc = unsafe { core::ptr::read_unaligned(pb.add(1)) };
                result =
                    unsafe { load_overlay(seg_off(ds(), dx()) as *const c_char, load_seg, reloc) }
                        as u8;
                set_CF(result);
                if result == 0 {
                    set_ax(0);
                }
            } else {
                // AL=00h load+execute (AL=01h load, no execute, is treated the same).
                set_CF(dos_exec_impl(
                    seg_off(es(), bx()) as *mut c_void,
                    seg_off(ds(), dx()) as *const c_char,
                    file,
                    func,
                    line,
                ));
            }
        }
        // AH=4Dh: Get Return Code of Subprocess (WAIT) — returns the exit code of
        // the last EXEC'd child in AX. The handler existed but was never wired into
        // the dispatch (same omission in the original C's dos_api switch); DM's
        // SELECTOR reaches it (EXEC the game, then read its return code).
        0x4D => set_CF(dos_get_return_code_impl(file, func, line)),
        0x4E => set_CF(dos_find_first_impl(
            seg_off(ds(), dx()) as *const c_char,
            cx(),
            file,
            func,
            line,
        )),
        0x4F => set_CF(dos_find_next_impl(file, func, line)),
        0x3D => {
            result = dos_open_file_impl(seg_off(ds(), dx()) as *const c_char, file, func, line);
            set_CF(result);
        }
        0x3E => {
            result = dos_close_file_impl(bx(), file, func, line);
            set_CF(result);
        }
        0x3F => {
            result = dos_read_file_impl(
                bx(),
                seg_off(ds(), dx()) as *mut c_void,
                cx(),
                file,
                func,
                line,
            );
            set_CF(result);
        }
        0x40 => {
            result = dos_write_file_impl(
                bx(),
                seg_off(ds(), dx()) as *const c_void,
                cx(),
                file,
                func,
                line,
            );
            set_CF(result);
        }
        0x42 => {
            result = dos_lseek_impl(bx(), cx(), dx(), al(), file, func, line);
            set_CF(result);
        }
        0x4A => {
            result = dos_resize_mem_impl(es(), bx(), file, func, line);
            set_CF(result);
        }
        0x4C => {
            crit_exit(c"dos_api_impl".as_ptr());
            dos_exit_impl(file, func, line);
            return 0; // dos_exit_impl does not return
        }
        _ => {
            // Hard-crash on an unimplemented service so the gap is loud.
            let mut msg = [0 as c_char; 256];
            unsafe {
                libc::snprintf(
                    msg.as_mut_ptr(),
                    256,
                    c"unimplemented DOS function AH=0x%02X (%s:%s:%d)".as_ptr(),
                    ah() as c_int,
                    file,
                    func,
                    line,
                );
                shim_log_crash(c"%s\n".as_ptr(), msg.as_ptr());
                shim_save_bug_bundle(
                    c"unimplemented_dos".as_ptr(),
                    ((cs() as u32) << 4) + ip() as u32,
                    msg.as_ptr(),
                );
                libc::abort();
            }
        }
    }
    crit_exit(c"dos_api_impl".as_ptr());
    result
}

#[no_mangle]
pub extern "C" fn dos_api() -> u8 {
    dos_api_impl(c"<external>".as_ptr(), c"dos_api".as_ptr(), 0)
}
