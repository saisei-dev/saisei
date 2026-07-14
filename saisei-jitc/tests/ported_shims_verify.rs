//! Verifies the shim FFI foundation (build+dlopen+globals+stdout capture) by
//! Run single-threaded: SAISEI_VERBOSE is process-global env state.
mod shim_common;
use shim_common::*;
use std::ffi::c_void;
use std::os::raw::c_char;

/// See the note on the same assertion in `ported_shims1.rs`: there is no x87 in
/// this machine, so the equipment word must not claim one — a guest told otherwise
/// takes its floating-point path and issues instructions the CPU cannot execute.
#[test]
fn bios_equipment_reports_no_math_coprocessor() {
    let _g = guard();
    let lib = ShimLib::load();
    unsafe {
        let equipment_addr = (0x40usize << 4) + 0x0010;
        let equipment = lib.read_u16(equipment_addr);
        assert_eq!(
            equipment & 0x0002,
            0,
            "equipment word claims an 8087 this machine does not have"
        );
    }
}

#[test]
fn shim_log_file_load_handles_host_pointer() {
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
