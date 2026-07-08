//! Ported C-shim tests (batch: ported_shims_small_a). Rust FFI equivalents of
//! the FFI-driven the original tests under tests/:
//! Run single-threaded: SAISEI_VERBOSE / stdout fd are process-global state.
mod shim_common;
use shim_common::*;
use std::ffi::c_void;
use std::os::raw::{c_char, c_int};

// Empty C string for the diagnostic (file, func) args (the original passed b"").
const EMPTY: *const c_char = b"\0".as_ptr() as *const c_char;

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------
#[test]
fn a20_default__default_enabled() {
    let _g = guard();
    let lib = ShimLib::load();
    unsafe {
        let inb: unsafe extern "C" fn(u16) -> u8 = lib.func("inb");
        let memb_write_impl: unsafe extern "C" fn(
            u16,
            u16,
            u8,
            *const c_char,
            *const c_char,
            c_int,
        ) = lib.func("memb_write_impl");
        let vm = lib.virtual_memory();

        // A20 should be enabled by default
        memb_write_impl(0xFFFF, 0x0010, 0xAA, EMPTY, EMPTY, 0);
        assert_eq!(*vm.add(0x100000), 0xAA);
        assert_eq!(*vm.add(0), 0x00);
        assert_eq!(inb(0x92) & 0x02, 0x02);
    }
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------
#[test]
fn a20_toggle__toggle() {
    let _g = guard();
    let lib = ShimLib::load();
    unsafe {
        let outb: unsafe extern "C" fn(u16, u8) = lib.func("outb");
        let memb_write_impl: unsafe extern "C" fn(
            u16,
            u16,
            u8,
            *const c_char,
            *const c_char,
            c_int,
        ) = lib.func("memb_write_impl");
        let vm = lib.virtual_memory();

        // ensure A20 disabled
        outb(0x92, 0x00);
        memb_write_impl(0xFFFF, 0x0010, 0xAA, EMPTY, EMPTY, 0);
        assert_eq!(*vm.add(0), 0xAA);
        assert_eq!(*vm.add(0x100000), 0x00);

        // enable A20
        outb(0x92, 0x02);
        memb_write_impl(0xFFFF, 0x0010, 0x55, EMPTY, EMPTY, 0);
        assert_eq!(*vm.add(0x100000), 0x55);
        assert_eq!(*vm.add(0), 0xAA);
    }
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------
#[test]
fn boot_regions__dos_boot_regions_initialised() {
    let _g = guard();
    let lib = ShimLib::load();
    unsafe {
        let vm = lib.virtual_memory();

        // BIOS Data Area clock ticks
        let ticks_off = (0x40usize << 4) + 0x6C;
        let t = lib.read_mem(ticks_off, 4);
        let ticks = u32::from_le_bytes([t[0], t[1], t[2], t[3]]);
        assert_eq!(ticks, 0);

        // PSP command tail
        let psp_cmd = (0x1000usize << 4) + 0x80;
        assert_eq!(*vm.add(psp_cmd), 0);
        assert_eq!(*vm.add(psp_cmd + 1), 0x0D);

        // Environment block pointer
        let env_ptr_off = (0x1000usize << 4) + 0x2C;
        let env_seg = (*vm.add(env_ptr_off) as u16) | ((*vm.add(env_ptr_off + 1) as u16) << 8);
        assert_eq!(env_seg, 0x0FF0);
        let env_off = (env_seg as usize) << 4;
        // A faithful DOS environment block is non-empty: COMMAND.COM always
        // launches a program with COMSPEC / PATH / PROMPT set, so it starts
        // with the COMSPEC entry, not two zero bytes.
        assert_ne!(*vm.add(env_off), 0);
        assert_eq!(&lib.read_mem(env_off, 8)[..], b"COMSPEC=");
    }
}

// ---------------------------------------------------------------------------
// (the original spawned a subprocess to capture stdout; the shim's dump uses
//  shim_log_stdout, gated on SAISEI_VERBOSE, so set verbose + capture fd 1.)
// ---------------------------------------------------------------------------
#[test]
fn dump_memory__dump_memory() {
    let _g = guard();
    std::env::set_var("SAISEI_VERBOSE", "1");
    let lib = ShimLib::load();
    std::env::remove_var("SAISEI_VERBOSE");
    let out = capture_stdout(|| unsafe {
        let vm = lib.virtual_memory();
        for i in 0..32u8 {
            *vm.add(0x200 + i as usize) = i;
        }
        let shim_dump_memory: unsafe extern "C" fn(u32, usize) = lib.func("shim_dump_memory");
        shim_dump_memory(0x200, 32);
    });
    assert!(
        out.contains(
            "000200: 00 01 02 03 04 05 06 07 08 09 0A 0B 0C 0D 0E 0F  |................|\n"
        ),
        "{out}"
    );
    assert!(
        out.contains(
            "000210: 10 11 12 13 14 15 16 17 18 19 1A 1B 1C 1D 1E 1F  |................|\n"
        ),
        "{out}"
    );
}

// ---------------------------------------------------------------------------
// (warn_file_overlap logs via shim_log_stdout — verbose-gated.)
// ---------------------------------------------------------------------------
#[test]
fn file_overwrite_warning__warn_on_file_overwrite() {
    let _g = guard();
    std::env::set_var("SAISEI_VERBOSE", "1");
    let lib = ShimLib::load();
    std::env::remove_var("SAISEI_VERBOSE");
    let out = capture_stdout(|| unsafe {
        let shim_log_file_load: unsafe extern "C" fn(*const c_char, *const c_void, usize, usize) =
            lib.func("shim_log_file_load");
        let vm = lib.virtual_memory();
        let addr1 = vm.add(0x3000) as *const c_void;
        let addr2 = vm.add(0x3002) as *const c_void;

        lib.write_mem(0x3000, &[0x01, 0x02, 0x03, 0x04]);
        shim_log_file_load(b"a.bin\0".as_ptr() as *const c_char, addr1, 4, 0);
        lib.write_mem(0x3002, &[0xFF, 0xEE, 0xDD, 0xCC]);
        shim_log_file_load(b"b.bin\0".as_ptr() as *const c_char, addr2, 4, 0);
    });
    assert!(
        out.contains("WARNING: file b.bin overwrote a.bin at 0x03002-0x03004"),
        "{out}"
    );
    assert!(out.contains("old bytes: 03 04"), "{out}");
    assert!(out.contains("new bytes: FF EE"), "{out}");
}

// ---------------------------------------------------------------------------
// The the original compiles a helper.c wrapping the memb / memb_write MACROS, which
// expand to memb_read_impl / memb_write_impl with __FILE__/__func__/__LINE__.
// We call those exported impl functions directly — the faithful equivalent.
// ---------------------------------------------------------------------------
#[test]
fn memb_unsigned__returns_unsigned() {
    let _g = guard();
    let lib = ShimLib::load();
    unsafe {
        let memb_write_impl: unsafe extern "C" fn(
            u16,
            u16,
            u8,
            *const c_char,
            *const c_char,
            c_int,
        ) = lib.func("memb_write_impl");
        let memb_read_impl: unsafe extern "C" fn(
            u16,
            u16,
            *const c_char,
            *const c_char,
            c_int,
        ) -> u8 = lib.func("memb_read_impl");

        let seg: u16 = 0x2000;
        let off: u16 = 0x0000;
        let value: u8 = 0xAB;

        memb_write_impl(seg, off, value, EMPTY, EMPTY, 0);
        assert_eq!(memb_read_impl(seg, off, EMPTY, EMPTY, 0), value);
    }
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------
fn wrap_addr(base: usize, offset: usize) -> usize {
    (base & !0xFFFF) | ((base + offset) & 0xFFFF)
}

// ::test_scan_forward_crosses_boundary
#[test]
fn scan_memory_for_al_boundary__scan_forward_crosses_boundary() {
    let _g = guard();
    let lib = ShimLib::load();
    unsafe {
        let scan_memory_for_al: unsafe extern "C" fn(*const u8, u8, u16, c_int, *mut u8) -> u16 =
            lib.func("scanMemoryForAl");
        let vm = lib.virtual_memory();

        let base = 0x1FFFEusize;
        *vm.add(wrap_addr(base, 0)) = 1;
        *vm.add(wrap_addr(base, 1)) = 2;
        *vm.add(wrap_addr(base, 2)) = 7;

        let mut last_byte: u8 = 0;
        let result = scan_memory_for_al(vm.add(base), 7, 3, 1, &mut last_byte);
        assert_eq!(result, 2);
    }
}

// ::test_scan_backward_crosses_boundary
#[test]
fn scan_memory_for_al_boundary__scan_backward_crosses_boundary() {
    let _g = guard();
    let lib = ShimLib::load();
    unsafe {
        let scan_memory_for_al: unsafe extern "C" fn(*const u8, u8, u16, c_int, *mut u8) -> u16 =
            lib.func("scanMemoryForAl");
        let vm = lib.virtual_memory();

        let base = 0x1FFFEusize;
        *vm.add(wrap_addr(base, 0)) = 7;
        *vm.add(wrap_addr(base, 1)) = 2;
        *vm.add(wrap_addr(base, 2)) = 1;

        let start = wrap_addr(base, 2);
        let mut last_byte: u8 = 0;
        let result = scan_memory_for_al(vm.add(start), 7, 3, -1, &mut last_byte);
        assert_eq!(result, 2);
    }
}
