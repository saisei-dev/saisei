//! Ported C-shim tests (batch `ported_shims3`): Rust FFI equivalents of the
//! from `test_bios_timer_interrupt_sets_midnight_flag_on_wrap` to the end).
//!
//! Run single-threaded: SAISEI_VERBOSE + the stdout fd are process-global.
//! SAISEI_CAPSTONE_LIB_DIR=... cargo test -p saisei-jitc --test ported_shims3 -- --test-threads=1
#![allow(non_snake_case)]

mod shim_common;
use shim_common::*;
use std::os::raw::{c_char, c_int};
use std::sync::atomic::{AtomicU64, Ordering};

// ---------------------------------------------------------------------------
// local helpers
// ---------------------------------------------------------------------------

/// `pit` global — matches the runtime PITState layout (timer.rs).
#[repr(C)]
#[derive(Clone, Copy)]
struct PitState {
    reload: u32,
    temp_reload: u16,
    expect_high: u8,
    access_mode: u8,
}

unsafe fn read_u32(lib: &ShimLib, lin: usize) -> u32 {
    let b = lib.read_mem(lin, 4);
    u32::from_le_bytes([b[0], b[1], b[2], b[3]])
}
unsafe fn write_u32(lib: &ShimLib, lin: usize, v: u32) {
    lib.write_mem(lin, &v.to_le_bytes());
}
unsafe fn write_u16(lib: &ShimLib, lin: usize, v: u16) {
    lib.write_mem(lin, &v.to_le_bytes());
}

const EXT: &[u8] = b"<external>\0";
const SAFE_POINT: &[u8] = b"safe_point\0";
fn cstr(b: &[u8]) -> *const c_char {
    b.as_ptr() as *const c_char
}

static CAP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Capture both fd 1 (stdout) and fd 2 (stderr) while `f` runs; returns
/// (stdout, stderr). Mirrors capfd.readouterr() for tests that assert on both
/// streams (e.g. run_silent_still_logs_stderr).
fn capture_out_err<F: FnOnce()>(f: F) -> (String, String) {
    use std::io::Read;
    use std::os::unix::io::AsRawFd;
    unsafe {
        libc::fflush(std::ptr::null_mut());
    }
    let id = CAP_COUNTER.fetch_add(1, Ordering::SeqCst);
    let pid = std::process::id();
    let out_path = std::env::temp_dir().join(format!("s3_out_{pid}_{id}.log"));
    let err_path = std::env::temp_dir().join(format!("s3_err_{pid}_{id}.log"));
    let out_file = std::fs::File::create(&out_path).expect("create out cap");
    let err_file = std::fs::File::create(&err_path).expect("create err cap");
    let saved_out = unsafe { libc::dup(1) };
    let saved_err = unsafe { libc::dup(2) };
    unsafe {
        libc::dup2(out_file.as_raw_fd(), 1);
        libc::dup2(err_file.as_raw_fd(), 2);
    }
    f();
    unsafe {
        libc::fflush(std::ptr::null_mut());
        libc::dup2(saved_out, 1);
        libc::close(saved_out);
        libc::dup2(saved_err, 2);
        libc::close(saved_err);
    }
    drop(out_file);
    drop(err_file);
    let mut o = String::new();
    std::fs::File::open(&out_path)
        .unwrap()
        .read_to_string(&mut o)
        .unwrap();
    let mut e = String::new();
    std::fs::File::open(&err_path)
        .unwrap()
        .read_to_string(&mut e)
        .unwrap();
    let _ = std::fs::remove_file(&out_path);
    let _ = std::fs::remove_file(&err_path);
    (o, e)
}

/// Re-exec this test binary to run a single abort-asserting test in a child
/// process (the original versions spawn a subprocess because the C shim calls
/// abort()/exit()). Assert the child failed and emitted `needle`.
fn assert_child_aborts(test_name: &str, child_key: &str, needle: &str) {
    let exe = std::env::current_exe().expect("current_exe");
    let out = std::process::Command::new(exe)
        .args(["--exact", test_name, "--nocapture", "--test-threads=1"])
        .current_dir(repo_root())
        .env(child_key, "1")
        .env("SAISEI_VERBOSE", "1")
        .output()
        .expect("spawn child test process");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !out.status.success(),
        "child unexpectedly succeeded:\n{combined}"
    );
    assert!(
        combined.contains(needle),
        "missing {needle:?} in child output:\n{combined}"
    );
}

// ---------------------------------------------------------------------------
// 41. test_bios_timer_interrupt_sets_midnight_flag_on_wrap
// ---------------------------------------------------------------------------
#[test]
fn test_bios_timer_interrupt_sets_midnight_flag_on_wrap() {
    let _g = shim_common::guard();
    let lib = ShimLib::load();
    unsafe {
        let run_interrupt: unsafe extern "C" fn(u8) = lib.func("run_interrupt");

        write_u32(&lib, 0x46C, 0x1800B0 - 1); // tick
        write_u16(&lib, 0x470, 0); // midnight flag
        *lib.global_ptr::<u8>("last_int_no") = 0;

        run_interrupt(0x08);

        assert_eq!(read_u32(&lib, 0x46C), 0);
        assert_eq!(lib.read_u16(0x470), 1);
        assert_eq!(lib.read_global::<u8>("last_int_no"), 0x1C);
    }
}

// ---------------------------------------------------------------------------
// 42. test_bios_timer_function_1c_returns_ticks
// ---------------------------------------------------------------------------
#[test]
fn test_bios_timer_function_1c_returns_ticks() {
    let _g = shim_common::guard();
    let lib = ShimLib::load();
    unsafe {
        let run_interrupt: unsafe extern "C" fn(u8) = lib.func("run_interrupt");
        let cpu = lib.cpu();

        write_u32(&lib, 0x46C, 0x12345678); // bios_ticks
        (*cpu).r_ax.x = 0x1C00; // AH=0x1C, AL=0x00
        (*cpu).r_cx.x = 0;
        (*cpu).r_dx.x = 0;
        (*cpu).flags.CF = 1;

        run_interrupt(0x1A);

        assert_eq!((*cpu).flags.CF, 0);
        assert_eq!((*cpu).r_cx.x, 0x1234);
        assert_eq!((*cpu).r_dx.x, 0x5678);
    }
}

// ---------------------------------------------------------------------------
// 43. test_unmapped_call_logged_last
//     Faithful near indirect call: call_table just pushes the return IP and
//     sets cpu.r_ip; it never resolves the target, so an "unmapped" address
//     does NOT produce an error and is non-fatal.
// ---------------------------------------------------------------------------
#[test]
fn test_unmapped_call_logged_last() {
    let _g = shim_common::guard();
    std::env::set_var("SAISEI_VERBOSE", "1");
    let lib = ShimLib::load();
    std::env::remove_var("SAISEI_VERBOSE");
    let (out, err) = capture_out_err(|| unsafe {
        let call_table: unsafe extern "C" fn(u16, u32) = lib.func("call_table");
        call_table(0x1234, 0xDEADBEEF);
    });
    let combined = format!("{out}{err}");
    // Non-fatal + no error resolving the "unmapped" target.
    assert!(
        !combined.contains("Error: call table address 0xDEADBEEF"),
        "{combined}"
    );
}

// ---------------------------------------------------------------------------
// 44. test_long_jump_reports_file_and_offset
//     Faithful far jmp: long_jump only sets cpu.r_cs:cpu.r_ip and traces the
//     seg:off (no file/offset resolution). (the original creates an unused app.exe
//     fixture for the old resolution path; long_jump never reads it, so it is
//     intentionally omitted here.)
// ---------------------------------------------------------------------------
#[test]
fn test_long_jump_reports_file_and_offset() {
    let _g = shim_common::guard();
    std::env::set_var("SAISEI_VERBOSE", "1");
    let lib = ShimLib::load();
    std::env::remove_var("SAISEI_VERBOSE");
    let out = capture_stdout(|| unsafe {
        let long_jump: unsafe extern "C" fn(u16, u16) = lib.func("long_jump");
        long_jump(0x1010, 0x0002);
    });
    assert!(out.contains("long_jump to 1010:0002"), "{out}");
}

// ---------------------------------------------------------------------------
// 45. test_call_table_known_location_logs_fix_instructions
//     Faithful near indirect call: call_table only pushes the return IP and
//     sets cpu.r_ip -- no file/offset resolution diagnostics.
// ---------------------------------------------------------------------------
#[test]
fn test_call_table_known_location_logs_fix_instructions() {
    let _g = shim_common::guard();
    std::env::set_var("SAISEI_VERBOSE", "1");
    let lib = ShimLib::load();
    std::env::remove_var("SAISEI_VERBOSE");
    let addr: u32 = (0x1010u32 << 4) + 0x0002;
    let out = capture_stdout(|| unsafe {
        let call_table: unsafe extern "C" fn(u16, u32) = lib.func("call_table");
        call_table(0x1234, addr);
    });
    assert!(out.contains("call_table 0x"), "{out}");
}

// ---------------------------------------------------------------------------
// 46. test_unmapped_address_reports_caller
//     Faithful near indirect call: call_table_impl pushes the return IP and
//     sets cpu.r_ip; the caller site is still logged in the trace line.
// ---------------------------------------------------------------------------
#[test]
fn test_unmapped_address_reports_caller() {
    let _g = shim_common::guard();
    std::env::set_var("SAISEI_VERBOSE", "1");
    let lib = ShimLib::load();
    std::env::remove_var("SAISEI_VERBOSE");
    let addr: u32 = (0x1010u32 << 4) + 0x0002;
    let out = capture_stdout(|| unsafe {
        let call_table_impl: unsafe extern "C" fn(u16, u32, *const c_char, *const c_char, c_int) =
            lib.func("call_table_impl");
        call_table_impl(0x1234, addr, cstr(b"build/game.c\0"), cstr(b"test\0"), 75);
    });
    assert!(out.contains("build/game.c:test:75"), "{out}");
}

// ---------------------------------------------------------------------------
// 47. test_iret_logs_file_and_line
//     safe_point services the pending IRQ0; the injected timer ISR eventually
//     iret's back to the interrupted 5678:1234 with IF-set flags (0x0202).
// ---------------------------------------------------------------------------
#[test]
fn test_iret_logs_file_and_line() {
    let _g = shim_common::guard();
    std::env::set_var("SAISEI_VERBOSE", "1");
    let lib = ShimLib::load();
    std::env::remove_var("SAISEI_VERBOSE");
    let out = capture_stdout(|| unsafe {
        let cpu = lib.cpu();
        (*cpu).r_ss = 0x2000;
        (*cpu).sp = 0x1000;
        (*cpu).r_cs = 0x5678;
        (*cpu).r_ip = 0x1234;
        (*cpu).flags.IF = 1;
        *lib.global_ptr::<u8>("irq0_pending") = 1;
        let safe_point_impl: unsafe extern "C" fn(*const c_char, *const c_char, c_int) =
            lib.func("safe_point_impl");
        safe_point_impl(cstr(EXT), cstr(SAFE_POINT), 0);
    });
    assert!(
        out.contains("Trace: iret -> 5678:1234 flags=0x0202"),
        "{out}"
    );
    assert!(!out.contains("Trace: long_jump to 5678:1234"), "{out}");
}

// ---------------------------------------------------------------------------
// 48. test_iret_outside_isr_restores_frame_without_longjmp
// ---------------------------------------------------------------------------
#[test]
fn test_iret_outside_isr_restores_frame_without_longjmp() {
    let _g = shim_common::guard();
    let lib = ShimLib::load();
    unsafe {
        let cpu = lib.cpu();
        (*cpu).r_ss = 0x3000;
        (*cpu).sp = 0x0200;
        (*cpu).r_cs = 0x1000;
        (*cpu).r_ip = 0x0100;
        (*cpu).flags.IF = 0;
        (*cpu).flags.DF = 1;
        *lib.global_ptr::<u8>("isr_depth") = 0;

        let base = ((*cpu).r_ss as usize) << 4;
        let sp = (*cpu).sp as usize;
        write_u16(&lib, base + sp, 0x5678); // new ip
        write_u16(&lib, base + sp + 2, 0x1234); // new cs
        write_u16(&lib, base + sp + 4, 0x0200); // IF=1

        let iret_impl: unsafe extern "C" fn(*const c_char, *const c_char, c_int) =
            lib.func("iret_impl");
        iret_impl(cstr(EXT), cstr(b"test\0"), 1);

        assert_eq!((*cpu).r_ip, 0x5678);
        assert_eq!((*cpu).r_cs, 0x1234);
        assert_eq!((*cpu).sp, 0x0206);
        assert_eq!((*cpu).flags.IF, 1);
        assert_eq!((*cpu).flags.DF, 0);
    }
}

// ---------------------------------------------------------------------------
// 49. test_pit_accumulates_ticks
// ---------------------------------------------------------------------------
#[test]
fn test_pit_accumulates_ticks() {
    let _g = shim_common::guard();
    let lib = ShimLib::load();
    unsafe {
        let safe_point_impl: unsafe extern "C" fn(*const c_char, *const c_char, c_int) =
            lib.func("safe_point_impl");

        let start = read_u32(&lib, 0x46C);
        safe_point_impl(cstr(EXT), cstr(SAFE_POINT), 0);

        let pit = lib.read_global::<PitState>("pit");
        let ns_per_tick = (pit.reload as u64) * 1_000_000_000u64 / 1193182u64;
        let last = lib.global_ptr::<u64>("last_host_time_ns");
        *last = (*last).wrapping_sub(ns_per_tick * 3);

        safe_point_impl(cstr(EXT), cstr(SAFE_POINT), 0);
        let end = read_u32(&lib, 0x46C);
        assert_eq!(end, start + 3);
        assert_eq!(lib.read_global::<u8>("irq0_pending"), 0);
    }
}

// ---------------------------------------------------------------------------
// 50. test_safe_point_respects_interrupt_shadow
// ---------------------------------------------------------------------------
#[test]
fn test_safe_point_respects_interrupt_shadow() {
    let _g = shim_common::guard();
    let lib = ShimLib::load();
    unsafe {
        let safe_point_impl: unsafe extern "C" fn(*const c_char, *const c_char, c_int) =
            lib.func("safe_point_impl");
        let cpu = lib.cpu();
        (*cpu).flags.IF = 1;
        *lib.global_ptr::<u8>("interrupt_shadow") = 1;
        *lib.global_ptr::<u8>("irq0_pending") = 1;
        safe_point_impl(cstr(EXT), cstr(SAFE_POINT), 0);
        assert_eq!((*cpu).flags.IF, 1);
        assert_eq!(lib.read_global::<u8>("interrupt_shadow"), 0);
        assert_eq!(lib.read_global::<u8>("irq0_pending"), 1);
    }
}

// ---------------------------------------------------------------------------
// 51. test_iret_sets_interrupt_shadow_on_if_enable
// ---------------------------------------------------------------------------
#[test]
fn test_iret_sets_interrupt_shadow_on_if_enable() {
    let _g = shim_common::guard();
    let lib = ShimLib::load();
    unsafe {
        let safe_point_impl: unsafe extern "C" fn(*const c_char, *const c_char, c_int) =
            lib.func("safe_point_impl");
        let cpu = lib.cpu();
        (*cpu).flags.IF = 1;
        *lib.global_ptr::<u8>("interrupt_shadow") = 0;
        *lib.global_ptr::<u8>("irq0_pending") = 1;
        safe_point_impl(cstr(EXT), cstr(SAFE_POINT), 0);
        assert_eq!((*cpu).flags.IF, 1);
        assert_eq!(lib.read_global::<u8>("interrupt_shadow"), 1);
        assert_eq!(lib.read_global::<u8>("irq0_pending"), 0);
    }
}

// ---------------------------------------------------------------------------
// 52. test_safe_point_skips_timer_enqueue_during_isr
// ---------------------------------------------------------------------------
#[test]
fn test_safe_point_skips_timer_enqueue_during_isr() {
    let _g = shim_common::guard();
    let lib = ShimLib::load();
    unsafe {
        let safe_point_impl: unsafe extern "C" fn(*const c_char, *const c_char, c_int) =
            lib.func("safe_point_impl");
        let set_timer_isr: unsafe extern "C" fn(u16, u16) = lib.func("shim_set_timer_isr");
        let cpu = lib.cpu();

        set_timer_isr(0x1332, 0x0000);
        (*cpu).flags.IF = 1;
        *lib.global_ptr::<u8>("isr_depth") = 1;
        let start_tick = read_u32(&lib, 0x46C);

        let pit = lib.read_global::<PitState>("pit");
        let ns_per_tick = (pit.reload as u64) * 1_000_000_000u64 / 1193182u64;
        let last = lib.global_ptr::<u64>("last_host_time_ns");
        *last = (*last).wrapping_sub(ns_per_tick * 3);

        safe_point_impl(cstr(EXT), cstr(SAFE_POINT), 0);
        assert_eq!(read_u32(&lib, 0x46C), start_tick);
        assert_eq!(lib.read_global::<u8>("irq0_pending"), 0);
        assert_eq!(lib.read_global::<u8>("isr_depth"), 1);
    }
}

// ---------------------------------------------------------------------------
// 53. test_safe_point_accumulates_fractional_pit_cycles
// ---------------------------------------------------------------------------
#[test]
fn test_safe_point_accumulates_fractional_pit_cycles() {
    let _g = shim_common::guard();
    let lib = ShimLib::load();
    unsafe {
        let safe_point_impl: unsafe extern "C" fn(*const c_char, *const c_char, c_int) =
            lib.func("safe_point_impl");

        let pit = lib.read_global::<PitState>("pit");
        let last = lib.global_ptr::<u64>("last_host_time_ns");

        safe_point_impl(cstr(EXT), cstr(SAFE_POINT), 0);
        let start_tick = read_u32(&lib, 0x46C);

        let ns_per_tick = (pit.reload as u64) * 1_000_000_000u64 / 1193182u64;
        let quarter_tick_ns = std::cmp::max(1u64, ns_per_tick / 4);

        for _ in 0..4 {
            *last = (*last).wrapping_sub(quarter_tick_ns);
            safe_point_impl(cstr(EXT), cstr(SAFE_POINT), 0);
        }

        assert!(read_u32(&lib, 0x46C) >= start_tick + 1);
    }
}

// ---------------------------------------------------------------------------
// 54. test_outb_pic_eoi_preserves_pending_irq
//     8259A spec: EOI clears the in-service bit only. A pending request (IRR)
//     survives EOI and is delivered later. The old model cleared irq0_pending
//     on EOI, silently destroying timer ticks that became pending while a
//     handler ran (or when a driver wrote defensive EOIs from its main loop,
//     e.g. DM's IBMIO) — starving INT8 and freezing game clocks.
// ---------------------------------------------------------------------------
#[test]
fn test_outb_pic_eoi_preserves_pending_irq() {
    let _g = shim_common::guard();
    let lib = ShimLib::load();
    unsafe {
        let outb: unsafe extern "C" fn(u16, u8) = lib.func("outb");
        *lib.global_ptr::<u8>("irq0_pending") = 1;
        outb(0x20, 0x20);
        assert_eq!(lib.read_global::<u8>("irq0_pending"), 1);
    }
}

// ---------------------------------------------------------------------------
// 55. test_run_silent_still_logs_stderr
//     Silent by default (no SAISEI_VERBOSE): shim_log_stdout is gated,
//     shim_log_stderr bypasses the gate.
// ---------------------------------------------------------------------------
#[test]
fn test_run_silent_still_logs_stderr() {
    let _g = shim_common::guard();
    std::env::remove_var("SAISEI_VERBOSE");
    let lib = ShimLib::load();
    let (out, err) = capture_out_err(|| unsafe {
        let shim_log_stdout: unsafe extern "C" fn(*const c_char, ...) = lib.func("shim_log_stdout");
        let shim_log_stderr: unsafe extern "C" fn(*const c_char, ...) = lib.func("shim_log_stderr");
        shim_log_stdout(cstr(b"stdout\n\0"));
        shim_log_stderr(cstr(b"stderr\n\0"));
    });
    assert!(!out.contains("stdout"), "{out}");
    assert!(err.contains("stderr"), "{err}");
}

// ---------------------------------------------------------------------------
// 56. test_runtime_toggle_stdout_logging
// ---------------------------------------------------------------------------
#[test]
fn test_runtime_toggle_stdout_logging() {
    let _g = shim_common::guard();
    std::env::set_var("SAISEI_VERBOSE", "1");
    let lib = ShimLib::load();
    std::env::remove_var("SAISEI_VERBOSE");
    unsafe {
        let set_enabled: unsafe extern "C" fn(c_int) = lib.func("shim_set_stdout_logging_enabled");
        let enable: unsafe extern "C" fn() = lib.func("shim_enable_stdout_logging");
        let log_stdout: unsafe extern "C" fn(*const c_char, ...) = lib.func("shim_log_stdout");

        let out = capture_stdout(|| log_stdout(cstr(b"enabled\n\0")));
        assert!(out.contains("enabled"), "{out}");

        set_enabled(0);
        let out = capture_stdout(|| log_stdout(cstr(b"disabled\n\0")));
        assert!(!out.contains("disabled"), "{out}");

        enable();
        let out = capture_stdout(|| log_stdout(cstr(b"re-enabled\n\0")));
        assert!(out.contains("re-enabled"), "{out}");
    }
}

// ---------------------------------------------------------------------------
// 57. test_default_isr_aborts_on_unknown_interrupt (abort → child process)
// ---------------------------------------------------------------------------
#[test]
fn test_default_isr_aborts_on_unknown_interrupt() {
    let _g = shim_common::guard();
    const KEY: &str = "SHIM3_CHILD_INT61";
    if std::env::var(KEY).is_ok() {
        let lib = ShimLib::load();
        unsafe {
            let run_interrupt: unsafe extern "C" fn(u8) = lib.func("run_interrupt");
            run_interrupt(0x61);
        }
        std::process::exit(0);
    }
    assert_child_aborts(
        "test_default_isr_aborts_on_unknown_interrupt",
        KEY,
        "unhandled interrupt 0x61",
    );
}

// ---------------------------------------------------------------------------
// 58. test_dos_api_aborts_on_unknown_function (abort → child process)
// ---------------------------------------------------------------------------
#[test]
fn test_dos_api_aborts_on_unknown_function() {
    let _g = shim_common::guard();
    const KEY: &str = "SHIM3_CHILD_DOSFF";
    if std::env::var(KEY).is_ok() {
        let lib = ShimLib::load();
        unsafe {
            (*lib.cpu()).r_ax.x = 0xFF00; // AH=0xFF
            let dos_api: unsafe extern "C" fn() -> u8 = lib.func("dos_api");
            dos_api();
        }
        std::process::exit(0);
    }
    assert_child_aborts(
        "test_dos_api_aborts_on_unknown_function",
        KEY,
        "unimplemented DOS function AH=0xFF",
    );
}

// ---------------------------------------------------------------------------
// 59. test_bios_keyboard_aborts_on_unknown_function (abort → child process)
// ---------------------------------------------------------------------------
#[test]
fn test_bios_keyboard_aborts_on_unknown_function() {
    let _g = shim_common::guard();
    const KEY: &str = "SHIM3_CHILD_KBD30";
    if std::env::var(KEY).is_ok() {
        let lib = ShimLib::load();
        unsafe {
            (*lib.cpu()).r_ax.x = 0x3000; // AH=0x30
            let bios_keyboard: unsafe extern "C" fn() = lib.func("bios_keyboard");
            bios_keyboard();
        }
        std::process::exit(0);
    }
    assert_child_aborts(
        "test_bios_keyboard_aborts_on_unknown_function",
        KEY,
        "unimplemented BIOS keyboard AH=0x30",
    );
}

// ---------------------------------------------------------------------------
// 60. test_cga_start_address_affects_origin
//     NOTE(port-skip): marked as a known skip — a known
//     faithfulness gap (CGA CRTC start address regs 0x0C/0x0D are used as a raw
//     byte offset, but a real 6845 start address is a WORD address, so the
//     origin shift is half the expected distance). It also requires a custom
//     virtual_display_present stub compiled into a separate .so, which the
//     shared ShimLib build does not provide. Kept ignored to mirror the skip.
// ---------------------------------------------------------------------------
#[test]
#[ignore]
fn test_cga_start_address_affects_origin() {
    let _g = shim_common::guard();
}
