#![allow(non_snake_case)] // fn_under_test__scenario naming convention
//! Ported C-shim tests (batch `ported_shims_small_b`). 1:1 ports of the
//! FFI-driven files:
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
    let d = std::env::temp_dir().join(format!("saisei_ported_b_{}_{}", std::process::id(), id));
    std::fs::create_dir_all(&d).expect("create temp dir");
    d
}

// port of test_lookup_call_target_logs_file_and_offset
// Faithful far jmp: long_jump_impl just sets cpu.r_cs:cpu.r_ip and logs a
// "Trace: long_jump to ..." line (no lookup/dispatch, no exit(1)). The the original
// test ran this in a subprocess only to isolate any crashes-bundle side effects;
// here we call it directly and assert on the captured stdout trace + cpu state.
#[test]
fn test_lookup_call_target_logging__logs_file_and_offset() {
    let _g = guard();
    std::env::set_var("SAISEI_VERBOSE", "1");
    let lib = ShimLib::load();
    std::env::remove_var("SAISEI_VERBOSE");
    let out = capture_stdout(|| unsafe {
        let long_jump_impl: unsafe extern "C" fn(u16, u16, *const c_char, *const c_char, c_int) =
            lib.func("long_jump_impl");
        long_jump_impl(
            0x100,
            0x2,
            b"test.c\0".as_ptr() as *const c_char,
            b"test_func\0".as_ptr() as *const c_char,
            1,
        );
    });
    unsafe {
        assert_eq!((*lib.cpu()).r_cs, 0x100, "r_cs={:#x}", (*lib.cpu()).r_cs);
        assert_eq!((*lib.cpu()).r_ip, 0x2, "r_ip={:#x}", (*lib.cpu()).r_ip);
    }
    assert!(out.contains("Trace: long_jump to 0100:0002"), "{out}");
}

// port of test_dos_exec_missing_file
#[test]
fn test_dos_exec__missing_file() {
    let _g = guard();
    let lib = ShimLib::load();
    unsafe {
        let dos_exec: unsafe extern "C" fn(*mut c_void, *const c_char) -> u8 = lib.func("dos_exec");
        let result = dos_exec(
            std::ptr::null_mut(),
            b"no_such_file.exe\0".as_ptr() as *const c_char,
        );
        assert_eq!(result, 1);
        assert_eq!((*lib.cpu()).flags.CF, 1);
    }
}

// port of test_file_read_warns_on_rcb_overlap
#[test]
fn test_rcb_file_overlap__file_read_warns_on_rcb_overlap() {
    let _g = guard();
    let dir = unique_tmp_dir();
    let prev_cwd = std::env::current_dir().expect("cwd");
    // dos_open resolves DOS paths within the process working dir, so run from
    // the temp dir and open by name.
    std::env::set_current_dir(&dir).expect("chdir");
    std::fs::write(dir.join("file.bin"), b"abcdef").expect("write file.bin");

    std::env::set_var("SAISEI_VERBOSE", "1");
    let lib = ShimLib::load();
    std::env::remove_var("SAISEI_VERBOSE");

    unsafe {
        let dos_open_file: unsafe extern "C" fn(*const c_char) -> u8 = lib.func("dos_open_file");
        let dos_read_file: unsafe extern "C" fn(u16, *mut c_void, u16) -> u8 =
            lib.func("dos_read_file");
        let dos_close_file: unsafe extern "C" fn(u16) -> u8 = lib.func("dos_close_file");

        assert_eq!(dos_open_file(b"file.bin\0".as_ptr() as *const c_char), 0);
        let handle = (*lib.cpu()).r_ax.x;

        let es = (*lib.cpu()).r_es;
        let rcb_base = ((es as usize) << 4) + 0xFF00;
        let buf_addr = lib.virtual_memory().add(rcb_base - 1) as *mut c_void;
        (*lib.cpu()).r_ds = es;
        (*lib.cpu()).r_dx.x = 0xFF00 - 1;

        let out = capture_stdout(|| {
            dos_read_file(handle, buf_addr, 6);
        });
        assert!(out.contains("Warning: file"), "{out}");
        assert!(out.contains("FIELD_1"), "{out}");
        assert!(out.contains("PROGRAM_SEG"), "{out}");
        assert!(out.contains("PREV_TIMER_VECTOR_OFF"), "{out}");
        assert_eq!(dos_close_file(handle), 0);
    }

    std::env::set_current_dir(&prev_cwd).expect("restore cwd");
    let _ = std::fs::remove_dir_all(&dir);
}

// port of test_dos_read_file_uses_buffer_pointer_not_ds_dx
#[test]
fn test_dos_read_file_buffer_pointer__uses_buffer_pointer_not_ds_dx() {
    let _g = guard();
    let dir = unique_tmp_dir();
    let prev_cwd = std::env::current_dir().expect("cwd");
    std::env::set_current_dir(&dir).expect("chdir");

    let lib = ShimLib::load();
    unsafe {
        (*lib.cpu()).r_ds = 0;
        (*lib.cpu()).r_dx.x = 0;

        let dos_open_file: unsafe extern "C" fn(*const c_char) -> u8 = lib.func("dos_open_file");
        let dos_read_file: unsafe extern "C" fn(u16, *mut c_void, u16) -> u8 =
            lib.func("dos_read_file");
        let dos_close_file: unsafe extern "C" fn(u16) -> u8 = lib.func("dos_close_file");

        let payload: &[u8] = b"ABCD";
        std::fs::write(dir.join("sample.bin"), payload).expect("write sample.bin");

        assert_eq!(dos_open_file(b"sample.bin\0".as_ptr() as *const c_char), 0);
        let handle = (*lib.cpu()).r_ax.x;

        let target_addr = lib.virtual_memory().add(0x4000);
        for i in 0..payload.len() {
            *target_addr.add(i) = 0;
        }

        // Reading into vm+0x4000 must NOT disturb the IVT at linear 0.
        let ivt_before = lib.read_mem(0, payload.len());

        assert_eq!(
            dos_read_file(handle, target_addr as *mut c_void, payload.len() as u16),
            0
        );
        let got = lib.read_mem(0x4000, payload.len());
        assert_eq!(&got[..], payload);

        let ivt_after = lib.read_mem(0, payload.len());
        assert_eq!(ivt_after, ivt_before);

        assert_eq!(dos_close_file(handle), 0);
    }

    std::env::set_current_dir(&prev_cwd).expect("restore cwd");
    let _ = std::fs::remove_dir_all(&dir);
}

// port of test_file_mapping_swap
#[test]
fn test_file_mapping_swap__swap() {
    let _g = guard();
    std::env::set_var("SAISEI_VERBOSE", "1");
    let lib = ShimLib::load();
    std::env::remove_var("SAISEI_VERBOSE");
    unsafe {
        let shim_log_file_load: unsafe extern "C" fn(*const c_char, *const c_void, usize, usize) =
            lib.func("shim_log_file_load");
        let file_mapping_swap: unsafe extern "C" fn(
            u16,
            u16,
            u16,
            u16,
            usize,
            *const c_char,
            *const c_char,
            c_int,
        ) = lib.func("file_mapping_swap_impl");

        let addr1 = lib.virtual_memory().add(0x4000) as *const c_void;
        let addr2 = lib.virtual_memory().add(0x5000) as *const c_void;

        let out = capture_stdout(|| {
            shim_log_file_load(b"a.bin\0".as_ptr() as *const c_char, addr1, 4, 0);
            shim_log_file_load(b"b.bin\0".as_ptr() as *const c_char, addr2, 4, 0);
            file_mapping_swap(
                0x4000 >> 4,
                0x4000 & 0xF,
                0x5000 >> 4,
                0x5000 & 0xF,
                4,
                b"t\0".as_ptr() as *const c_char,
                b"f\0".as_ptr() as *const c_char,
                1,
            );
        });
        assert!(out.contains("file_mapping_swap"), "{out}");
    }
}

// port of test_file_mapping_swap_skips_partial_rebase
#[test]
fn test_file_mapping_swap__skips_partial_rebase() {
    let _g = guard();
    std::env::set_var("SAISEI_VERBOSE", "1");
    let lib = ShimLib::load();
    std::env::remove_var("SAISEI_VERBOSE");
    unsafe {
        let shim_log_file_load: unsafe extern "C" fn(*const c_char, *const c_void, usize, usize) =
            lib.func("shim_log_file_load");
        let file_mapping_swap: unsafe extern "C" fn(
            u16,
            u16,
            u16,
            u16,
            usize,
            *const c_char,
            *const c_char,
            c_int,
        ) = lib.func("file_mapping_swap_impl");

        let addr1 = lib.virtual_memory().add(0x4000) as *const c_void;
        let addr2 = lib.virtual_memory().add(0x5000) as *const c_void;

        let out = capture_stdout(|| {
            // Register mappings larger than the swap range so rebasing is skipped.
            shim_log_file_load(b"a.bin\0".as_ptr() as *const c_char, addr1, 8, 0);
            shim_log_file_load(b"b.bin\0".as_ptr() as *const c_char, addr2, 8, 0);
            file_mapping_swap(
                0x4000 >> 4,
                0x4000 & 0xF,
                0x5000 >> 4,
                0x5000 & 0xF,
                4,
                b"t\0".as_ptr() as *const c_char,
                b"f\0".as_ptr() as *const c_char,
                1,
            );
        });
        assert!(out.contains("skipped rebasing"), "{out}");
    }
}

fn wrap_addr(base: usize, offset: usize) -> usize {
    (base & !0xFFFF) | ((base + offset) & 0xFFFF)
}

// port of test_compare_forward_and_backward
#[test]
fn test_compare_memory_until_mismatch_df__forward_and_backward() {
    let _g = guard();
    let lib = ShimLib::load();
    unsafe {
        let compare: unsafe extern "C" fn(*const u8, *const u8, u16, c_int) -> u8 =
            lib.func("compareMemoryUntilMismatch");
        let vm = lib.virtual_memory();

        let src_addr = 0x200usize;
        let dst_ok_addr = 0x400usize;
        let dst_bad_addr = 0x600usize;

        let values = [1u8, 2, 3, 4];
        for (i, &val) in values.iter().enumerate() {
            *vm.add(src_addr + i) = val;
            *vm.add(dst_ok_addr + i) = val;
            *vm.add(dst_bad_addr + i) = if i < 3 { val } else { 5 };
        }

        assert_eq!(compare(vm.add(src_addr), vm.add(dst_ok_addr), 4, 1), 1);
        assert_eq!(compare(vm.add(src_addr), vm.add(dst_bad_addr), 4, 1), 0);

        let src_end = src_addr + 3;
        let dst_ok_end = dst_ok_addr + 3;
        let dst_bad_end = dst_bad_addr + 3;
        assert_eq!(compare(vm.add(src_end), vm.add(dst_ok_end), 4, -1), 1);
        assert_eq!(compare(vm.add(src_end), vm.add(dst_bad_end), 4, -1), 0);
    }
}

// port of test_compare_crosses_boundary
#[test]
fn test_compare_memory_until_mismatch_df__crosses_boundary() {
    let _g = guard();
    let lib = ShimLib::load();
    unsafe {
        let compare: unsafe extern "C" fn(*const u8, *const u8, u16, c_int) -> u8 =
            lib.func("compareMemoryUntilMismatch");
        let vm = lib.virtual_memory();

        let src_addr = 0x1FFFEusize;
        let dst_ok_addr = 0x20FFEusize;
        let dst_bad_addr = 0x21FFEusize;
        let values = [1u8, 2, 3];
        for (i, &val) in values.iter().enumerate() {
            *vm.add(wrap_addr(src_addr, i)) = val;
            *vm.add(wrap_addr(dst_ok_addr, i)) = val;
            *vm.add(wrap_addr(dst_bad_addr, i)) = if i < 2 { val } else { 9 };
        }

        assert_eq!(compare(vm.add(src_addr), vm.add(dst_ok_addr), 3, 1), 1);
        assert_eq!(compare(vm.add(src_addr), vm.add(dst_bad_addr), 3, 1), 0);

        let src_end = wrap_addr(src_addr, 2);
        let dst_ok_end = wrap_addr(dst_ok_addr, 2);
        let dst_bad_end = wrap_addr(dst_bad_addr, 2);
        assert_eq!(compare(vm.add(src_end), vm.add(dst_ok_end), 3, -1), 1);
        assert_eq!(compare(vm.add(src_end), vm.add(dst_bad_end), 3, -1), 0);
    }
}
