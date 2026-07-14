//! Ported C-shim tests (batch: ported_shims1). Rust FFI equivalents of the
//! functions (test_bios_equipment_reports_math_coprocessor through
//! test_outw_vga_graphics_controller_pair_supported).
//!
//! Run single-threaded: SAISEI_VERBOSE / stdout fd are process-global state.
//! SAISEI_CAPSTONE_LIB_DIR=... cargo test -p saisei-jitc --test ported_shims1 \
//! -- --test-threads=1
mod shim_common;
use shim_common::*;
use std::ffi::c_void;
use std::io::Read as _;
use std::os::raw::{c_char, c_int};
use std::os::unix::io::AsRawFd;
use std::sync::atomic::{AtomicU64, Ordering};

// ---------------------------------------------------------------------------
// Struct globals read by these tests (verified against runtime/ headers).
// ---------------------------------------------------------------------------

/// runtime/src/audio.rs :: Opl2State.
#[repr(C)]
#[derive(Clone, Copy)]
struct Opl2State {
    address: u8,
    registers: [u8; 256],
    status: u8,
    busy_until_us: u64,
    timer1_expire_us: u64,
    timer2_expire_us: u64,
}

/// runtime/src/timer.rs :: PITState.
#[repr(C)]
#[derive(Clone, Copy)]
struct PitState {
    reload: u32,
    temp_reload: u16,
    expect_high: u8,
    access_mode: u8,
}

// enum StagePresentBranch (runtime/src/video.rs): UNKNOWN=0, TEXT=1, CGA=2.
const STAGE_BRANCH_TEXT: c_int = 1;
const STAGE_BRANCH_CGA: c_int = 2;

// ---------------------------------------------------------------------------
// Fork helper: some tests need a child process (assert on its exit/output);
// subprocess returncode/stderr because the shim calls exit(1) on an unhandled
// IO port. We fork, redirect the child's stderr (fd 2) to a temp file, run the
// C call in the child, and read back (exit_code, stderr_text) in the parent.
// ---------------------------------------------------------------------------
static FORK_ID: AtomicU64 = AtomicU64::new(0);

unsafe fn fork_run<F: FnOnce()>(f: F) -> (i32, String) {
    libc::fflush(std::ptr::null_mut());
    let path = std::env::temp_dir().join(format!(
        "saisei_forkerr_{}_{}.log",
        std::process::id(),
        FORK_ID.fetch_add(1, Ordering::SeqCst)
    ));
    let file = std::fs::File::create(&path).expect("create fork stderr file");
    let fd = file.as_raw_fd();
    let pid = libc::fork();
    if pid == 0 {
        // Child: point stderr at the file, run the call. If it returns without
        // exiting, exit 0; if it calls exit(1) we inherit that code.
        libc::dup2(fd, 2);
        f();
        libc::fflush(std::ptr::null_mut());
        libc::_exit(0);
    }
    // Parent.
    drop(file);
    let mut status: c_int = 0;
    libc::waitpid(pid, &mut status, 0);
    let code = if libc::WIFEXITED(status) {
        libc::WEXITSTATUS(status)
    } else {
        -1
    };
    let mut s = String::new();
    std::fs::File::open(&path)
        .unwrap()
        .read_to_string(&mut s)
        .unwrap();
    let _ = std::fs::remove_file(&path);
    (code, s)
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------
/// The BIOS equipment word (BDA 0040:0010) must report **no** maths coprocessor.
///
/// This test used to assert the opposite — that bit 1 was set — and it was wrong
/// to. There is no x87 in this machine, and a program that is told there is one
/// takes its floating-point path and issues instructions the CPU cannot execute.
/// Advertising hardware we do not emulate is not generosity, it is a lie the guest
/// acts on. (Changed with the equipment word in "The EGA is a chip, not a byte
/// array", which also cleared bits 5-4 so the word stops claiming a CGA.)
#[test]
fn test_bios_equipment_reports_no_math_coprocessor() {
    let _g = guard();
    let lib = ShimLib::load();
    unsafe {
        let equipment = lib.read_u16((0x40usize << 4) + 0x0010);
        assert_eq!(
            equipment & 0x0002,
            0,
            "equipment word claims an 8087 this machine does not have"
        );
    }
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------
#[test]
fn test_shim_log_file_load_handles_host_pointer() {
    let _g = guard();
    std::env::set_var("SAISEI_VERBOSE", "1");
    let lib = ShimLib::load();
    std::env::remove_var("SAISEI_VERBOSE");
    let out = capture_stdout(|| unsafe {
        let f: unsafe extern "C" fn(*const c_char, *const c_void, usize, usize) =
            lib.func("shim_log_file_load");
        f(
            b"host.bin\0".as_ptr() as *const c_char,
            0x12345678usize as *const c_void,
            16,
            0,
        );
    });
    assert!(out.contains("mem offset n/a"), "{out}");
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------
#[test]
fn test_dos_get_version_sets_al() {
    let _g = guard();
    let lib = ShimLib::load();
    unsafe {
        let dos_get_version: unsafe extern "C" fn() -> u8 = lib.func("dos_get_version");
        let cpu = lib.cpu();
        // cpu.r_ax.byte.l = 0
        (*cpu).r_ax.x &= 0xFF00;
        let result = dos_get_version();
        assert_eq!(result, 0);
        assert_eq!((*cpu).r_ax.l(), 3);
    }
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------
#[test]
fn test_dos_write_file_fails_on_read_only_handle() {
    let _g = guard();
    let lib = ShimLib::load();
    unsafe {
        let dos_open_file: unsafe extern "C" fn(*const c_char) -> u8 = lib.func("dos_open_file");
        let dos_write_file: unsafe extern "C" fn(u16, *const c_void, u16) -> u8 =
            lib.func("dos_write_file");
        let dos_api: unsafe extern "C" fn() -> u8 = lib.func("dos_api");
        let dos_close_file: unsafe extern "C" fn(u16) -> u8 = lib.func("dos_close_file");
        let cpu = lib.cpu();

        // dos_open resolves DOS paths within the working dir (stripping leading
        // drive/root separators), so run from a temp dir and open by name.
        let dir = std::env::temp_dir().join(format!(
            "saisei_rotest_{}_{}",
            std::process::id(),
            FORK_ID.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("readonly.bin"), b"content").unwrap();
        let prev_cwd = std::env::current_dir().unwrap();
        std::env::set_current_dir(&dir).unwrap();

        let open_result = dos_open_file(b"readonly.bin\0".as_ptr() as *const c_char);
        assert_eq!(open_result, 0);
        let handle = (*cpu).r_ax.x;

        let buf: [u8; 1] = [b'!'];
        let write_result = dos_write_file(handle, buf.as_ptr() as *const c_void, 1);
        assert_eq!(write_result, 1);
        assert!(
            (*cpu).r_ax.x == 1 || (*cpu).r_ax.x == 6,
            "ax={}",
            (*cpu).r_ax.x
        );

        let seg: usize = 0x200;
        let off: usize = 0x0010;
        lib.write_mem((seg << 4) + off, b"!");

        // cpu.r_ax.byte.h = 0x40; cpu.r_ax.byte.l = 0  ==> ax = 0x4000
        (*cpu).r_ax.x = 0x4000;
        (*cpu).r_bx.x = handle;
        (*cpu).r_cx.x = 1;
        (*cpu).r_ds = seg as u16;
        (*cpu).r_dx.x = off as u16;
        (*cpu).flags.CF = 0;

        let api_result = dos_api();
        assert_eq!(api_result, 1);
        assert_eq!((*cpu).flags.CF, 1);
        assert!(
            (*cpu).r_ax.x == 1 || (*cpu).r_ax.x == 6,
            "ax={}",
            (*cpu).r_ax.x
        );

        dos_close_file(handle);

        // Restore cwd for sibling tests.
        std::env::set_current_dir(&prev_cwd).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------
#[test]
fn test_register_aliasing() {
    let _g = guard();
    let lib = ShimLib::load();
    unsafe {
        let cpu = lib.cpu();
        (*cpu).r_ax.x = 0;
        // cpu.r_ax.byte.l = 0x34
        (*cpu).r_ax.x = ((*cpu).r_ax.x & 0xFF00) | 0x34;
        assert_eq!((*cpu).r_ax.x, 0x0034);
        // cpu.r_ax.byte.h = 0x12
        (*cpu).r_ax.x = ((*cpu).r_ax.x & 0x00FF) | (0x12 << 8);
        assert_eq!((*cpu).r_ax.x, 0x1234);
        (*cpu).r_ax.x = 0xABCD;
        assert_eq!((*cpu).r_ax.l(), 0xCD);
        assert_eq!((*cpu).r_ax.h(), 0xAB);
    }
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------
#[test]
fn test_outb_3d8_updates_stage_branch() {
    let _g = guard();
    let lib = ShimLib::load();
    unsafe {
        let outb: unsafe extern "C" fn(u16, u8) = lib.func("outb");
        let shim_stage_and_present_current_buffer: unsafe extern "C" fn() =
            lib.func("shim_stage_and_present_current_buffer");
        let shim_last_stage_present_branch: unsafe extern "C" fn() -> c_int =
            lib.func("shim_last_stage_present_branch");

        outb(0x3D8, 0x0A);
        shim_stage_and_present_current_buffer();
        assert_eq!(shim_last_stage_present_branch(), STAGE_BRANCH_CGA);

        outb(0x3D8, 0x09);
        shim_stage_and_present_current_buffer();
        assert_eq!(shim_last_stage_present_branch(), STAGE_BRANCH_TEXT);
    }
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------
#[test]
fn test_dos_direct_console_io_sets_zf() {
    let _g = guard();
    let lib = ShimLib::load();
    unsafe {
        let dos_direct_console_io: unsafe extern "C" fn(u8) -> u8 =
            lib.func("dos_direct_console_io");
        let cpu = lib.cpu();
        (*cpu).flags.ZF = 0;
        let result = dos_direct_console_io(0xFF);
        assert_eq!(result, 0);
        assert_eq!((*cpu).flags.ZF, 1);
    }
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------
#[test]
fn test_dos_read_char_uses_keyboard_interrupt() {
    let _g = guard();
    std::env::set_var("SAISEI_VERBOSE", "1");
    let lib = ShimLib::load();
    std::env::remove_var("SAISEI_VERBOSE");
    let mut result: u8 = 0xFF;
    let out = capture_stdout(|| unsafe {
        let dos_read_char: unsafe extern "C" fn() -> u8 = lib.func("dos_read_char");
        let shim_keyboard_enqueue: unsafe extern "C" fn(u8) = lib.func("shim_keyboard_enqueue");
        let kbd_bios_deposit_from_isr: unsafe extern "C" fn() =
            lib.func("kbd_bios_deposit_from_isr");

        shim_keyboard_enqueue(0x41);
        // The keystroke only becomes visible to DOS console services once the
        // BIOS INT 09h handler has read it off port 0x60 into the type-ahead
        // buffer (40:1E). Drive that handler so the buffer is populated first.
        kbd_bios_deposit_from_isr();
        result = dos_read_char();
    });
    assert_eq!(result, 0);
    unsafe {
        assert_eq!((*lib.cpu()).r_ax.l(), 0x41);
    }
    assert!(out.ends_with('A'), "{out}");
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------
#[test]
fn test_dos_direct_console_io_reads_key() {
    let _g = guard();
    let lib = ShimLib::load();
    unsafe {
        let dos_direct_console_io: unsafe extern "C" fn(u8) -> u8 =
            lib.func("dos_direct_console_io");
        let shim_keyboard_enqueue: unsafe extern "C" fn(u8) = lib.func("shim_keyboard_enqueue");
        let kbd_bios_deposit_from_isr: unsafe extern "C" fn() =
            lib.func("kbd_bios_deposit_from_isr");
        let cpu = lib.cpu();

        shim_keyboard_enqueue(0x42);
        // Drive the BIOS INT 09h handler so the key reaches the type-ahead
        // buffer (40:1E) that AH=06 reads.
        kbd_bios_deposit_from_isr();
        (*cpu).flags.ZF = 1;
        let result = dos_direct_console_io(0xFF);
        assert_eq!(result, 0);
        assert_eq!((*cpu).flags.ZF, 0);
        assert_eq!((*cpu).r_ax.l(), 0x42);
        (*cpu).flags.ZF = 0;
        let _result = dos_direct_console_io(0xFF);
        assert_eq!((*cpu).flags.ZF, 1);
    }
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------
#[test]
fn test_inb_joystick_port_returns_ff() {
    let _g = guard();
    let lib = ShimLib::load();
    unsafe {
        let inb: unsafe extern "C" fn(u16) -> u8 = lib.func("inb");
        assert_eq!(inb(0x201), 0xFF);
    }
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------
#[test]
fn test_inb_vga_status_follows_cga_raster() {
    let _g = guard();
    let lib = ShimLib::load();
    unsafe {
        let inb: unsafe extern "C" fn(u16) -> u8 = lib.func("inb");
        // The status register is a pure function of VIRTUAL time (the 6845
        // raster position on the shared 14.318MHz crystal): line period
        // 63,695ns × 262 lines/frame; 200 visible lines with a ~44.7µs
        // active region; vsync pulse = lines 224..240. Park the clock at
        // known raster positions and read the bits.
        let vnow = lib.global_ptr::<u64>("virtual_now_accum_ns");
        const LINE_NS: u64 = 63_695;
        const FRAME_NS: u64 = LINE_NS * 262;
        let base = (*vnow / FRAME_NS + 2) * FRAME_NS;
        // visible line, active display: no retrace bits
        *vnow = base + 100 * LINE_NS + 10_000;
        assert_eq!(inb(0x3DA) & 0x09, 0x00);
        // visible line, horizontal blank: display-disable only
        *vnow = base + 100 * LINE_NS + 50_000;
        assert_eq!(inb(0x3DA) & 0x09, 0x01);
        // vertical blank before the sync pulse: display-disable only
        *vnow = base + 210 * LINE_NS + 10_000;
        assert_eq!(inb(0x3DA) & 0x09, 0x01);
        // vsync pulse: BOTH bits (display is disabled during vsync too —
        // the old square-wave model wrongly made them mutually exclusive)
        *vnow = base + 230 * LINE_NS + 10_000;
        assert_eq!(inb(0x3DA) & 0x09, 0x09);
    }
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------
#[test]
fn test_inb_dma_port_0006_returns_programmed_address() {
    let _g = guard();
    let lib = ShimLib::load();
    unsafe {
        let inb: unsafe extern "C" fn(u16) -> u8 = lib.func("inb");
        let outb: unsafe extern "C" fn(u16, u8) = lib.func("outb");
        outb(0x0C, 0);
        outb(0x06, 0x34);
        outb(0x06, 0x12);
        outb(0x0C, 0);
        assert_eq!(inb(0x06), 0x34);
        assert_eq!(inb(0x06), 0x12);
        assert_eq!(inb(0x06), 0x34);
    }
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------
#[test]
fn test_opl2_ports_store_register() {
    let _g = guard();
    let lib = ShimLib::load();
    unsafe {
        let outb: unsafe extern "C" fn(u16, u8) = lib.func("outb");
        let inb: unsafe extern "C" fn(u16) -> u8 = lib.func("inb");
        let opl2 = lib.global_ptr::<Opl2State>("opl2");

        outb(0x388, 0x20);
        outb(0x389, 0x7F);
        assert_eq!((*opl2).address, 0x20);
        assert_eq!((*opl2).registers[0x20], 0x7F);
        assert_eq!(inb(0x389), 0x7F);
        // YM3812 status: D4-D0 read as 0 — there is no "busy" status bit
        // (the write delay is a bus-timing constraint, not a flag).
        assert_eq!(inb(0x388) & 0x1F, 0);

        // The AdLib presence check, as the official driver performs it:
        // mask both timers + IRQ-reset -> flags clear; latch timer 1 = 0xFF,
        // start it unmasked; ≥80µs later (status & 0xE0) == 0xC0 (IRQ | T1).
        // Timer expiry follows VIRTUAL time — advance the clock.
        outb(0x388, 0x04);
        outb(0x389, 0x60);
        outb(0x388, 0x04);
        outb(0x389, 0x80);
        assert_eq!(inb(0x388) & 0xE0, 0);
        outb(0x388, 0x02);
        outb(0x389, 0xFF);
        outb(0x388, 0x04);
        outb(0x389, 0x21);
        let vnow = lib.global_ptr::<u64>("virtual_now_accum_ns");
        *vnow += 2_000_000;
        assert_eq!(inb(0x388) & 0xE0, 0xC0);
        // Cleanup mask + reset clears the flags (and ONLY the reset write's
        // bit 7 acts — the register keeps the 0x60 mask value).
        outb(0x388, 0x04);
        outb(0x389, 0x60);
        outb(0x388, 0x04);
        outb(0x389, 0x80);
        assert_eq!(inb(0x388) & 0xE0, 0);
        assert_eq!((*opl2).registers[0x04], 0x60);
    }
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------
#[test]
fn test_outb_pit_control_port_modes() {
    let _g = guard();
    let lib = ShimLib::load();
    unsafe {
        let outb: unsafe extern "C" fn(u16, u8) = lib.func("outb");
        let pit = lib.global_ptr::<PitState>("pit");

        outb(0x43, 0x30);
        outb(0x40, 0x34);
        outb(0x40, 0x12);
        assert_eq!((*pit).temp_reload, 0x1234);

        outb(0x43, 0x20);
        outb(0x40, 0xAB);
        assert_eq!((*pit).temp_reload, 0xAB34);

        outb(0x43, 0x10);
        outb(0x40, 0xCD);
        assert_eq!((*pit).temp_reload, 0xABCD);
    }
}

// ---------------------------------------------------------------------------
// (the original ran a subprocess and asserted returncode == 0; we run the calls in a
//  forked child and assert the child did not exit(1).)
// ---------------------------------------------------------------------------
#[test]
fn test_outb_pit_channel_2_accepts_data() {
    let _g = guard();
    let lib = ShimLib::load();
    unsafe {
        let outb: unsafe extern "C" fn(u16, u8) = lib.func("outb");
        let (code, _err) = fork_run(|| {
            outb(0x43, 0xB6);
            outb(0x42, 0x34);
            outb(0x42, 0x12);
        });
        assert_eq!(code, 0);
    }
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------
#[test]
fn test_outb_unhandled_port_exits() {
    let _g = guard();
    let lib = ShimLib::load();
    unsafe {
        let outb: unsafe extern "C" fn(u16, u8) = lib.func("outb");
        let (code, err) = fork_run(|| outb(0x1234, 0x56));
        assert_eq!(code, 1);
        assert!(
            err.contains("Error: outb called with unsupported port 0x1234"),
            "{err}"
        );
    }
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------
#[test]
fn test_inb_unhandled_port_exits() {
    let _g = guard();
    let lib = ShimLib::load();
    unsafe {
        let inb: unsafe extern "C" fn(u16) -> u8 = lib.func("inb");
        let (code, err) = fork_run(|| {
            let _ = inb(0x1234);
        });
        assert_eq!(code, 1);
        assert!(
            err.contains("Error: inb called with unsupported port 0x1234"),
            "{err}"
        );
    }
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------
#[test]
fn test_inw_unhandled_port_exits() {
    let _g = guard();
    let lib = ShimLib::load();
    unsafe {
        let inw: unsafe extern "C" fn(u16) -> u16 = lib.func("inw");
        let (code, err) = fork_run(|| {
            let _ = inw(0x1234);
        });
        assert_eq!(code, 1);
        // inw on an unhandled port decomposes to byte access, so the error
        // names inb.
        assert!(err.contains("unsupported port 0x1234"), "{err}");
    }
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------
#[test]
fn test_outw_unhandled_port_exits() {
    let _g = guard();
    let lib = ShimLib::load();
    unsafe {
        let outw: unsafe extern "C" fn(u16, u16) = lib.func("outw");
        let (code, err) = fork_run(|| outw(0x1234, 0xABCD));
        assert_eq!(code, 1);
        // outw on an unhandled port decomposes to byte access, so the error
        // names outb.
        assert!(err.contains("unsupported port 0x1234"), "{err}");
    }
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------
#[test]
fn test_outw_vga_graphics_controller_pair_supported() {
    let _g = guard();
    let lib = ShimLib::load();
    unsafe {
        let outw: unsafe extern "C" fn(u16, u16) = lib.func("outw");
        let (code, _err) = fork_run(|| outw(0x3CE, 0xFF08));
        assert_eq!(code, 0);
    }
}
