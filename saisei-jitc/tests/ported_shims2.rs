//! Ported C-shim tests (batch `ported_shims2`). 1:1 Rust FFI ports of
//! test_inb_mda_status_alias_supported .. test_bios_timer_interrupt_updates_ticks_and_calls_int1c
//! Run single-threaded: SAISEI_VERBOSE, the captured stdout fd, and the process
//! cwd are all process-global state serialized by `shim_common::guard()`.
mod shim_common;
use shim_common::*;
use std::ffi::c_void;
use std::os::raw::{c_char, c_int};
use std::sync::atomic::{AtomicU64, Ordering};

static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Create a fresh unique temp directory (mirrors's `tmp_path`).
fn unique_tmp_dir() -> std::path::PathBuf {
    let id = TMP_COUNTER.fetch_add(1, Ordering::SeqCst);
    let d = std::env::temp_dir().join(format!("saisei_ported2_{}_{}", std::process::id(), id));
    std::fs::create_dir_all(&d).expect("create temp dir");
    d
}

// port of test_inb_mda_status_alias_supported
#[test]
fn test_inb_mda_status_alias_supported() {
    let _g = guard();
    let lib = ShimLib::load();
    unsafe {
        let inb: unsafe extern "C" fn(u16) -> u8 = lib.func("inb");
        let value = inb(0x3BA);
        assert!(value == 0x01 || value == 0x08, "value={value:#x}");
    }
}

// port of test_dos_read_file_logs_buffer
#[test]
fn test_dos_read_file_logs_buffer() {
    let _g = guard();
    std::env::set_var("SAISEI_VERBOSE", "1");
    let lib = ShimLib::load();
    std::env::remove_var("SAISEI_VERBOSE");

    let dir = unique_tmp_dir();
    let prev_cwd = std::env::current_dir().expect("cwd");
    std::env::set_current_dir(&dir).expect("chdir");
    std::fs::write(dir.join("file.txt"), b"hi").expect("write file.txt");

    unsafe {
        let dos_open_file: unsafe extern "C" fn(*const c_char) -> u8 = lib.func("dos_open_file");
        let dos_read_file: unsafe extern "C" fn(u16, *mut c_void, u16) -> u8 =
            lib.func("dos_read_file");
        let dos_close_file: unsafe extern "C" fn(u16) -> u8 = lib.func("dos_close_file");

        let mut open_ret = 1u8;
        let mut handle = 0u16;
        let out = capture_stdout(|| unsafe {
            open_ret = dos_open_file(b"file.txt\0".as_ptr() as *const c_char);
            handle = (*lib.cpu()).r_ax.x;
            let mut buf = [0u8; 2];
            dos_read_file(handle, buf.as_mut_ptr() as *mut c_void, 2);
        });
        let close_ret = dos_close_file(handle);

        std::env::set_current_dir(&prev_cwd).expect("restore cwd");
        let _ = std::fs::remove_dir_all(&dir);

        assert_eq!(open_ret, 0);
        assert!(out.contains("Trace: dos_read_file data: 68 69"), "{out}");
        assert!(
            out.contains("Trace: loaded ") && out.contains("file.txt"),
            "{out}"
        );
        assert!(out.contains("length 2"), "{out}");
        assert_eq!(close_ret, 0);
    }
}

// port of test_dos_read_file_wraps_at_segment_boundary
// NOTE(port-divergence): the original test is `an upstream skip`ped — a known
// faithfulness gap, not staleness: dos_read_file writes to the raw buffer pointer
// linearly, so a read crossing a 64KB boundary does NOT wrap the offset the way a
// real 8086 wraps DX. No game is known to rely on a segment-wrapping file read.
// Kept 1:1 and marked #[ignore] to match the original skip.
#[test]
#[ignore = "known faithfulness gap (matches the original an upstream skip): dos_read_file does not segment-wrap the buffer offset"]
fn test_dos_read_file_wraps_at_segment_boundary() {
    let _g = guard();
    let lib = ShimLib::load();

    let dir = unique_tmp_dir();
    let prev_cwd = std::env::current_dir().expect("cwd");
    std::env::set_current_dir(&dir).expect("chdir");
    std::fs::write(dir.join("wrap.bin"), b"abcd").expect("write wrap.bin");

    unsafe {
        let dos_open_file: unsafe extern "C" fn(*const c_char) -> u8 = lib.func("dos_open_file");
        let dos_read_file: unsafe extern "C" fn(u16, *mut c_void, u16) -> u8 =
            lib.func("dos_read_file");
        let dos_close_file: unsafe extern "C" fn(u16) -> u8 = lib.func("dos_close_file");
        let vm = lib.virtual_memory();

        assert_eq!(dos_open_file(b"wrap.bin\0".as_ptr() as *const c_char), 0);
        let handle = (*lib.cpu()).r_ax.x;

        let seg = (*lib.cpu()).r_ds as usize;
        (*lib.cpu()).r_dx.x = 0xFFFE;
        let buf_addr = vm.add((seg << 4) + 0xFFFE) as *mut c_void;
        dos_read_file(handle, buf_addr, 4);

        let mem1 = lib.read_mem((seg << 4) + 0xFFFE, 2);
        let mem2 = lib.read_mem(seg << 4, 2);

        std::env::set_current_dir(&prev_cwd).expect("restore cwd");
        let _ = std::fs::remove_dir_all(&dir);

        assert_eq!(&mem1, b"ab");
        assert_eq!(&mem2, b"cd");
        assert_eq!(dos_close_file(handle), 0);
    }
}

// port of test_copy_linear_from_segoff_handles_offset_wrap
#[test]
fn test_copy_linear_from_segoff_handles_offset_wrap() {
    let _g = guard();
    let lib = ShimLib::load();
    unsafe {
        let copy: unsafe extern "C" fn(u16, u16, usize, *mut c_void) =
            lib.func("shim_copy_linear_block");

        let seg = 0x9000usize;
        let off = 0x4000usize;
        let length = 0xFA00usize;

        let pattern: Vec<u8> = (0..length).map(|i| (i % 256) as u8).collect();
        lib.write_mem((seg << 4) + off, &pattern);

        let mut buffer = vec![0u8; length];
        copy(
            seg as u16,
            off as u16,
            length,
            buffer.as_mut_ptr() as *mut c_void,
        );

        assert_eq!(buffer, pattern);
    }
}

// port of test_dos_open_file_accepts_carriage_return
#[test]
fn test_dos_open_file_accepts_carriage_return() {
    let _g = guard();
    let lib = ShimLib::load();

    let dir = unique_tmp_dir();
    let prev_cwd = std::env::current_dir().expect("cwd");
    std::env::set_current_dir(&dir).expect("chdir");
    std::fs::write(dir.join("file.txt"), b"hi").expect("write file.txt");

    unsafe {
        let dos_open_file: unsafe extern "C" fn(*const c_char) -> u8 = lib.func("dos_open_file");
        let dos_close_file: unsafe extern "C" fn(u16) -> u8 = lib.func("dos_close_file");

        // A CR in the DOS path string terminates it: "file.txt\rtrash" opens file.txt.
        let open_ret = dos_open_file(b"file.txt\rtrash\0".as_ptr() as *const c_char);
        let handle = (*lib.cpu()).r_ax.x;
        let close_ret = dos_close_file(handle);

        std::env::set_current_dir(&prev_cwd).expect("restore cwd");
        let _ = std::fs::remove_dir_all(&dir);

        assert_eq!(open_ret, 0);
        assert_eq!(close_ret, 0);
    }
}

// port of test_dos_open_file_empty_path_fails
#[test]
fn test_dos_open_file_empty_path_fails() {
    let _g = guard();
    let lib = ShimLib::load();
    unsafe {
        let dos_open_file: unsafe extern "C" fn(*const c_char) -> u8 = lib.func("dos_open_file");
        assert_eq!(dos_open_file(b"\0".as_ptr() as *const c_char), 1);
    }
}

// port of test_long_jump_unmapped_address
// The the original test spawned a subprocess only to isolate any crash side-effects and
// assert returncode==0 (long_jump does NOT abort — it logs a Trace line and sets
// cpu.r_cs:r_ip). We call it directly under captured stdout.
#[test]
fn test_long_jump_unmapped_address() {
    let _g = guard();
    std::env::set_var("SAISEI_VERBOSE", "1");
    let lib = ShimLib::load();
    std::env::remove_var("SAISEI_VERBOSE");

    let (seg, off) = (0xDEADu16, 0xBEEFu16);
    let expected = format!("0x{:08X}", ((seg as u32) << 4) + off as u32);
    let out = capture_stdout(|| unsafe {
        let long_jump: unsafe extern "C" fn(u16, u16) = lib.func("long_jump");
        long_jump(seg, off);
    });
    assert!(out.contains(&expected), "{out}");
}

// port of test_call_table_unmapped_address
#[test]
fn test_call_table_unmapped_address() {
    let _g = guard();
    std::env::set_var("SAISEI_VERBOSE", "1");
    let lib = ShimLib::load();
    std::env::remove_var("SAISEI_VERBOSE");

    let out = capture_stdout(|| unsafe {
        let call_table: unsafe extern "C" fn(u16, u32) = lib.func("call_table");
        call_table(0x1234, 0xDEADBEEF);
    });
    assert!(out.contains("0xDEADBEEF"), "{out}");
}

// port of test_call_table_unmapped_target_is_captured_not_fatal
#[test]
fn test_call_table_unmapped_target_is_captured_not_fatal() {
    let _g = guard();
    let lib = ShimLib::load();
    unsafe {
        let call_table: unsafe extern "C" fn(u16, u32) = lib.func("call_table");
        let cpu = lib.cpu();
        (*cpu).r_cs = 0x1000;
        (*cpu).r_ss = 0x1000;
        (*cpu).sp = 0;

        // Faithful near indirect call: call_table only pushes the return IP and
        // sets cpu.r_ip to the target offset within the current segment. The
        // target offset is `addr - cs<<4`.
        let target_off: u32 = 0x4321; // within the cs=0x1000 segment
        let addr = (((*cpu).r_cs as u32) << 4) + target_off;
        let ret_ip: u16 = 0xBEEF;
        let orig_sp = (*cpu).sp;
        call_table(ret_ip, addr);
        // ret_ip pushed onto the emulated stack; cpu.r_ip = target offset; cs same.
        assert_eq!((*cpu).sp, orig_sp.wrapping_sub(2));
        assert_eq!((*cpu).r_cs, 0x1000);
        assert_eq!((*cpu).r_ip, target_off as u16);
    }
}

// port of test_lcall_table_unmapped_address
#[test]
fn test_lcall_table_unmapped_address() {
    let _g = guard();
    std::env::set_var("SAISEI_VERBOSE", "1");
    let lib = ShimLib::load();
    std::env::remove_var("SAISEI_VERBOSE");

    let (seg, off) = (0xDEADu16, 0xBEEFu16);
    let expected = format!("{seg:04X}:{off:04X}");
    let out = capture_stdout(|| unsafe {
        let lcall_table: unsafe extern "C" fn(u16, u16, u16) = lib.func("lcall_table");
        lcall_table(0x1234, seg, off);
    });
    assert!(out.contains(&expected), "{out}");
}

// port of test_lcall_table_pushes_frame_and_sets_target
#[test]
fn test_lcall_table_pushes_frame_and_sets_target() {
    let _g = guard();
    let lib = ShimLib::load();
    unsafe {
        let lcall_table: unsafe extern "C" fn(u16, u16, u16) = lib.func("lcall_table");
        let cpu = lib.cpu();
        let (_orig_cs, orig_ip, orig_sp) = ((*cpu).r_cs, (*cpu).r_ip, (*cpu).sp);
        // Faithful far call: push the caller cs + ret_ip on the emulated stack and
        // set cpu.r_cs:cpu.r_ip to the target.
        lcall_table(orig_ip, 0x106A, 0x0006);
        assert_eq!((*cpu).r_cs, 0x106A);
        assert_eq!((*cpu).r_ip, 0x0006);
        assert_eq!((*cpu).sp, orig_sp.wrapping_sub(4));
    }
}

// port of test_lcall_table_then_retf_round_trips
#[test]
fn test_lcall_table_then_retf_round_trips() {
    let _g = guard();
    let lib = ShimLib::load();
    unsafe {
        let lcall_table: unsafe extern "C" fn(u16, u16, u16) = lib.func("lcall_table");
        let retf: unsafe extern "C" fn() = lib.func("retf");
        let cpu = lib.cpu();
        let (orig_cs, orig_ip, orig_sp) = ((*cpu).r_cs, (*cpu).r_ip, (*cpu).sp);
        // Faithful round-trip: lcall pushes cs+ret_ip and sets the target; retf
        // pops them and restores the caller cs:ip:sp exactly.
        lcall_table(orig_ip, 0x106A, 0x0006);
        assert_eq!((*cpu).r_cs, 0x106A);
        assert_eq!((*cpu).r_ip, 0x0006);
        assert_eq!((*cpu).sp, orig_sp.wrapping_sub(4));
        retf();
        assert_eq!((*cpu).r_cs, orig_cs);
        assert_eq!((*cpu).r_ip, orig_ip);
        assert_eq!((*cpu).sp, orig_sp);
    }
}

// port of test_lcall_table_retf_pop_restores_stack
#[test]
fn test_lcall_table_retf_pop_restores_stack() {
    let _g = guard();
    let lib = ShimLib::load();
    unsafe {
        let lcall_table: unsafe extern "C" fn(u16, u16, u16) = lib.func("lcall_table");
        let retf_pop_impl: unsafe extern "C" fn(*const c_char, *const c_char, c_int, u16) =
            lib.func("retf_pop_impl");
        let cpu = lib.cpu();
        let (orig_cs, orig_ip, orig_sp) = ((*cpu).r_cs, (*cpu).r_ip, (*cpu).sp);
        // `retf imm16` (callee argument cleanup): pop the 4-byte far-return frame
        // PLUS imm16 caller-pushed argument bytes. lcall pushed 4 bytes (sp-=4);
        // retf_pop(6) pops 4+6 = 10, leaving sp = orig_sp + 6.
        lcall_table(orig_ip, 0x106A, 0x0006);
        assert_eq!((*cpu).sp, orig_sp.wrapping_sub(4));
        retf_pop_impl(
            b"<test>\0".as_ptr() as *const c_char,
            b"<test>\0".as_ptr() as *const c_char,
            0,
            6,
        );
        assert_eq!((*cpu).r_cs, orig_cs);
        assert_eq!((*cpu).r_ip, orig_ip);
        assert_eq!((*cpu).sp, orig_sp.wrapping_add(6));
    }
}

// port of test_lcall_table_retf_with_stack_drift_is_faithful
#[test]
fn test_lcall_table_retf_with_stack_drift_is_faithful() {
    let _g = guard();
    let lib = ShimLib::load();
    unsafe {
        let lcall_table: unsafe extern "C" fn(u16, u16, u16) = lib.func("lcall_table");
        let retf: unsafe extern "C" fn() = lib.func("retf");
        let cpu = lib.cpu();
        let (_orig_cs, orig_ip, orig_sp) = ((*cpu).r_cs, (*cpu).r_ip, (*cpu).sp);
        // Faithful model: there is no stack-drift "recovery". retf pops from the
        // CURRENT ss:sp -- exactly what a real 8086 does.
        lcall_table(orig_ip, 0x106A, 0x0006);
        assert_eq!((*cpu).sp, orig_sp.wrapping_sub(4));
        (*cpu).sp = (*cpu).sp.wrapping_sub(0x1A);
        let drifted_sp = (*cpu).sp;
        retf();
        // retf advanced sp by 4 from wherever it was (no recovery to orig_sp).
        assert_eq!((*cpu).sp, drifted_sp.wrapping_add(4));
    }
}

// port of test_lcall_table_retf_segment_change_is_faithful
#[test]
fn test_lcall_table_retf_segment_change_is_faithful() {
    let _g = guard();
    let lib = ShimLib::load();
    unsafe {
        let lcall_table: unsafe extern "C" fn(u16, u16, u16) = lib.func("lcall_table");
        let retf: unsafe extern "C" fn() = lib.func("retf");
        let cpu = lib.cpu();
        let (orig_cs, orig_ip, orig_sp, orig_ss) =
            ((*cpu).r_cs, (*cpu).r_ip, (*cpu).sp, (*cpu).r_ss);
        // Faithful model: retf reads the far-return frame from the CURRENT ss:sp
        // and never modifies ss. ss is left untouched (the caller owns ss).
        lcall_table(orig_ip, 0x106A, 0x0006);
        assert_eq!((*cpu).r_ss, orig_ss);
        retf();
        assert_eq!((*cpu).r_cs, orig_cs);
        assert_eq!((*cpu).r_ip, orig_ip);
        assert_eq!((*cpu).sp, orig_sp);
        assert_eq!((*cpu).r_ss, orig_ss);
    }
}

// port of test_jump_table_unmapped_address
#[test]
fn test_jump_table_unmapped_address() {
    let _g = guard();
    std::env::set_var("SAISEI_VERBOSE", "1");
    let lib = ShimLib::load();
    std::env::remove_var("SAISEI_VERBOSE");

    let out = capture_stdout(|| unsafe {
        // the original called jump_table(0xDEADBEEF) with a single arg; the real symbol
        // takes (uint32_t addr, uint16_t expected_retip). expected_retip is
        // ignored by jump_table_impl.
        let jump_table: unsafe extern "C" fn(u32, u16) = lib.func("jump_table");
        jump_table(0xDEADBEEF, 0);
    });
    assert!(out.contains("0xDEADBEEF"), "{out}");
}

// port of test_dos_set_interrupt_vector_unmapped_handler
#[test]
fn test_dos_set_interrupt_vector_unmapped_handler() {
    let _g = guard();
    std::env::set_var("SAISEI_VERBOSE", "1");
    let lib = ShimLib::load();
    std::env::remove_var("SAISEI_VERBOSE");

    let out = capture_stdout(|| unsafe {
        let dos_set_interrupt_vector: unsafe extern "C" fn(u8, u16, u16) -> u8 =
            lib.func("dos_set_interrupt_vector");
        dos_set_interrupt_vector(0x21, 0x0100, 0x0000);
    });
    assert!(!out.contains("0x00001000"), "{out}");
    assert!(!out.contains("not mapped"), "{out}");
}

// port of test_dos_get_interrupt_vector_has_default
#[test]
fn test_dos_get_interrupt_vector_has_default() {
    let _g = guard();
    let lib = ShimLib::load();
    unsafe {
        let dos_get_interrupt_vector: unsafe extern "C" fn() -> u8 =
            lib.func("dos_get_interrupt_vector");
        let cpu = lib.cpu();
        // AL = 0x08 (query the INT 08h vector).
        (*cpu).r_ax.x = ((*cpu).r_ax.x & 0xFF00) | 0x08;
        let result = dos_get_interrupt_vector();
        assert_eq!(result, 0);
        // The default (unset) IVT entry points at the BIOS handler stub F060:0000.
        assert_eq!((*cpu).r_es, 0xF060);
        assert_eq!((*cpu).r_bx.x, 0x0000);
    }
}

// port of test_bios_equipment_interrupt_returns_equipment_word
#[test]
fn test_bios_equipment_interrupt_returns_equipment_word() {
    let _g = guard();
    let lib = ShimLib::load();
    unsafe {
        let run_interrupt: unsafe extern "C" fn(u8) = lib.func("run_interrupt");
        let cpu = lib.cpu();

        let equipment = lib.read_u16(0x410);
        assert_eq!(equipment, 0x0063);

        (*cpu).r_ax.x = 0;
        run_interrupt(0x11);
        assert_eq!((*cpu).r_ax.x, 0x0063);
    }
}

// port of test_bios_timer_interrupt_updates_ticks_and_calls_int1c
#[test]
fn test_bios_timer_interrupt_updates_ticks_and_calls_int1c() {
    let _g = guard();
    let lib = ShimLib::load();
    unsafe {
        let run_interrupt: unsafe extern "C" fn(u8) = lib.func("run_interrupt");
        let vm = lib.virtual_memory();
        let tick = vm.add(0x46C) as *mut u32;
        let midnight = vm.add(0x470) as *mut u16;
        let last_int = lib.global_ptr::<u8>("last_int_no");

        std::ptr::write_unaligned(tick, 0);
        std::ptr::write_unaligned(midnight, 0);
        *last_int = 0;

        run_interrupt(0x08);

        assert_eq!(std::ptr::read_unaligned(tick), 1);
        assert_eq!(std::ptr::read_unaligned(midnight), 0);
        assert_eq!(*last_int, 0x1C);
    }
}
