//! Verifies the shim FFI foundation (build+dlopen+globals+stdout capture) by
//! Run single-threaded: SAISEI_VERBOSE is process-global env state.
mod shim_common;
use shim_common::*;
use std::ffi::c_void;
use std::os::raw::c_char;

#[test]
fn bios_equipment_reports_math_coprocessor() {
    let _g = guard();
    let lib = ShimLib::load();
    unsafe {
        let equipment_addr = (0x40usize << 4) + 0x0010;
        let equipment = lib.read_u16(equipment_addr);
        assert!(
            equipment & 0x0002 != 0,
            "expected BIOS equipment word to report an 8087"
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
