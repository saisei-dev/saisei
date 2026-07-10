// saisei_rt.rs — the Rust view of the Saisei C runtime ABI (runtime_abi.h).
//
// This file is `include!`d verbatim into every generated Rust chunk, exactly
// as a C chunk `#include`s "shims.h". It binds the shared `cpu` global and the
// runtime call surface the translated code is allowed to use, and provides
// value accessors + faithful reimplementations of the header's `static inline`
// helpers (parity8/xor8/xor16/linear_addr/seg_off) so the emitted chunk code
// reads naturally.
//
// The chunk is compiled to a cdylib and dlopen'd into the -rdynamic host, so
// `cpu`, `virtual_memory` and every extern fn below resolve at load time
// against the (still C) runtime — the same way the C chunks link today.

// Wrapped in a module so this file can be `include!`d at item position: module
// inner attributes are legal, and the private `use core::ffi` stays scoped.
pub mod rt {
#![allow(dead_code, non_snake_case, non_upper_case_globals, non_camel_case_types)]
#![allow(unused_unsafe, unused_parens, unused_mut, unused_assignments, unused_imports)]

use core::ffi::{c_char, c_int, c_void};

// ---- shared CPU state (cpu_state.h; layout must match byte-for-byte) --------

#[repr(C)]
pub struct CpuFlags {
    pub CF: u8, pub PF: u8, pub ZF: u8, pub SF: u8, pub OF: u8, pub IF: u8, pub DF: u8,
}

#[repr(C)]
pub struct CpuState {
    pub r_ax: u16, pub r_bx: u16, pub r_cx: u16, pub r_dx: u16,
    pub si: u16, pub di: u16, pub bp: u16, pub sp: u16, pub r_ip: u16,
    pub r_cs: u16, pub r_ds: u16, pub r_es: u16, pub r_ss: u16,
    pub flags: CpuFlags,
}

#[repr(C)]
pub struct ShimTailDispatchState {
    pub pending: u8,
    pub addr: u32,
    pub expected: u16,
}

extern "C" {
    static mut cpu: CpuState;
    static mut virtual_memory: *mut u8;
    static a20_enabled: u8;
    static mut interrupt_shadow: u8;
}

#[inline(always)]
pub fn set_interrupt_shadow(v: u8) {
    unsafe { interrupt_shadow = v; }
}

// DOS EXEC (INT 21h AH=4Bh) saved stack — cs:0x8C2/0x8C4 map to these fields.
#[repr(C)]
pub struct ExecParamBlock {
    pub saved_sp: u16,
    pub saved_ss: u16,
}
extern "C" {
    static mut exec_params: ExecParamBlock;
}
#[inline(always)]
fn exec_ptr() -> *mut ExecParamBlock {
    unsafe { core::ptr::addr_of_mut!(exec_params) }
}
#[inline(always)] pub fn exec_saved_sp() -> u16 { unsafe { (*exec_ptr()).saved_sp } }
#[inline(always)] pub fn set_exec_saved_sp(v: u16) { unsafe { (*exec_ptr()).saved_sp = v; } }
#[inline(always)] pub fn exec_saved_ss() -> u16 { unsafe { (*exec_ptr()).saved_ss } }
#[inline(always)] pub fn set_exec_saved_ss(v: u16) { unsafe { (*exec_ptr()).saved_ss = v; } }

#[inline(always)]
fn cpu_ptr() -> *mut CpuState {
    unsafe { core::ptr::addr_of_mut!(cpu) }
}

#[inline(always)]
fn site() -> *const c_char {
    // The chunk's real name ("jit_<segbase5>_<off4>_<sha>"), defined at the
    // chunk crate root by ir_to_rust. Mirrors the C backend's `__FILE__` so the
    // runtime's cross-binary-write tripwire recognizes a chunk writing within
    // its own decode segment as legitimate self-modification (a generic "jit"
    // tag defeats that check and false-fires on driver self-writes).
    crate::SAISEI_SITE.as_ptr()
}

// ---- register accessors -----------------------------------------------------
//
// Word registers are stored fields; byte halves are computed views of the word
// (little-endian: low byte = bits 0..7, high byte = bits 8..15), matching the
// C `Register16` union. `*_ptr` helpers hand a raw pointer to the register cell
// for the pointer-taking helpers (xor8/xor16), exactly like C's `&reg`.

macro_rules! word_reg {
    ($get:ident, $set:ident, $ptr:ident, $field:ident) => {
        #[inline(always)] pub fn $get() -> u16 { unsafe { (*cpu_ptr()).$field } }
        #[inline(always)] pub fn $set(v: u16) { unsafe { (*cpu_ptr()).$field = v; } }
        #[inline(always)] pub fn $ptr() -> *mut u16 { unsafe { core::ptr::addr_of_mut!((*cpu_ptr()).$field) } }
    };
}

word_reg!(ax, set_ax, ax_ptr, r_ax);
word_reg!(bx, set_bx, bx_ptr, r_bx);
word_reg!(cx, set_cx, cx_ptr, r_cx);
word_reg!(dx, set_dx, dx_ptr, r_dx);
word_reg!(si, set_si, si_ptr, si);
word_reg!(di, set_di, di_ptr, di);
word_reg!(bp, set_bp, bp_ptr, bp);
word_reg!(sp, set_sp, sp_ptr, sp);
word_reg!(ip, set_ip, ip_ptr, r_ip);
word_reg!(cs, set_cs, cs_ptr, r_cs);
word_reg!(ds, set_ds, ds_ptr, r_ds);
word_reg!(es, set_es, es_ptr, r_es);
word_reg!(ss, set_ss, ss_ptr, r_ss);

macro_rules! byte_regs {
    ($lo:ident, $set_lo:ident, $lo_ptr:ident, $hi:ident, $set_hi:ident, $hi_ptr:ident, $word:ident, $set_word:ident, $wptr:ident) => {
        #[inline(always)] pub fn $lo() -> u8 { $word() as u8 }
        #[inline(always)] pub fn $set_lo(v: u8) { $set_word(($word() & 0xFF00) | v as u16); }
        #[inline(always)] pub fn $lo_ptr() -> *mut u8 { $wptr() as *mut u8 }
        #[inline(always)] pub fn $hi() -> u8 { ($word() >> 8) as u8 }
        #[inline(always)] pub fn $set_hi(v: u8) { $set_word(($word() & 0x00FF) | ((v as u16) << 8)); }
        #[inline(always)] pub fn $hi_ptr() -> *mut u8 { unsafe { ($wptr() as *mut u8).add(1) } }
    };
}

byte_regs!(al, set_al, al_ptr, ah, set_ah, ah_ptr, ax, set_ax, ax_ptr);
byte_regs!(bl, set_bl, bl_ptr, bh, set_bh, bh_ptr, bx, set_bx, bx_ptr);
byte_regs!(cl, set_cl, cl_ptr, ch, set_ch, ch_ptr, cx, set_cx, cx_ptr);
byte_regs!(dl, set_dl, dl_ptr, dh, set_dh, dh_ptr, dx, set_dx, dx_ptr);

// ---- flags ------------------------------------------------------------------

macro_rules! flag {
    ($get:ident, $set:ident, $field:ident) => {
        #[inline(always)] pub fn $get() -> u8 { unsafe { (*cpu_ptr()).flags.$field } }
        #[inline(always)] pub fn $set(v: u8) { unsafe { (*cpu_ptr()).flags.$field = v; } }
    };
}
flag!(CF, set_CF, CF);
flag!(PF, set_PF, PF);
flag!(ZF, set_ZF, ZF);
flag!(SF, set_SF, SF);
flag!(OF, set_OF, OF);
flag!(IF, set_IF, IF);
flag!(DF, set_DF, DF);

// ---- static-inline helpers (reimplemented; not dynamic symbols) -------------

#[inline(always)]
pub fn linear_addr(seg: u16, off: u16) -> u32 {
    let addr = ((seg as u32) << 4).wrapping_add(off as u32);
    if unsafe { a20_enabled } != 0 { addr & 0x1FFFFF } else { addr & 0xFFFFF }
}

#[inline(always)]
pub fn seg_off(seg: u16, off: u16) -> *mut u8 {
    unsafe { virtual_memory.add(linear_addr(seg, off) as usize) }
}

#[inline(always)]
pub fn memb_raw(seg: u16, off: u16) -> u8 {
    unsafe { *seg_off(seg, off) }
}

#[inline(always)]
fn mask_addr_rt(addr: u32) -> u32 {
    if unsafe { a20_enabled } != 0 { addr & 0x1FFFFF } else { addr & 0xFFFFF }
}

/// Byte-identical to the runtime's `memw_raw_read`: little-endian word with the
/// high byte's linear address re-masked to the A20 boundary (so a word straddling
/// the 1MB/2MB wrap reads the same bytes the shim would).
#[inline(always)]
pub fn memw_raw_read_inline(seg: u16, off: u16) -> u16 {
    unsafe {
        let addr = linear_addr(seg, off);
        (*virtual_memory.add(addr as usize) as u16)
            | ((*virtual_memory.add(mask_addr_rt(addr.wrapping_add(1)) as usize) as u16) << 8)
    }
}

#[inline(always)]
pub fn parity8(v: u8) -> u8 {
    let mut value = v;
    value ^= value >> 4;
    value &= 0xF;
    // x86 PF is set when the low byte has even parity.
    (((0x6996u32 >> value) & 1) == 0) as u8
}

#[inline(always)]
pub unsafe fn xor8(dest: *mut u8, src: u8) {
    let res = *dest ^ src;
    *dest = res;
    set_CF(0);
    set_OF(0);
    set_ZF((res == 0) as u8);
    set_SF((res >> 7) & 1);
    set_PF(parity8(res));
}

#[inline(always)]
pub unsafe fn xor16(dest: *mut u16, src: u16) {
    let res = *dest ^ src;
    *dest = res;
    set_CF(0);
    set_OF(0);
    set_ZF((res == 0) as u8);
    set_SF(((res >> 15) & 1) as u8);
    set_PF(parity8(res as u8));
}

// ---- runtime call surface (extern "C"; resolved at dlopen) ------------------

extern "C" {
    fn memb_read_impl(seg: u16, off: u16, file: *const c_char, func: *const c_char, line: c_int) -> u8;
    fn memw_read_impl(seg: u16, off: u16, file: *const c_char, func: *const c_char, line: c_int) -> u16;
    fn memb_write_impl(seg: u16, off: u16, value: u8, file: *const c_char, func: *const c_char, line: c_int);
    fn memw_write_impl(seg: u16, off: u16, value: u16, file: *const c_char, func: *const c_char, line: c_int);
    fn safe_point_impl(file: *const c_char, func: *const c_char, line: c_int);

    // Control transfers are macros in shims.h -> `*_impl(..., file, func, line)`;
    // the plain names aren't all exported, so bind the `_impl` symbols directly.
    fn call_table_impl(ret_ip: u16, addr: u32, file: *const c_char, func: *const c_char, line: c_int);
    fn lcall_table_impl(ret_ip: u16, seg: u16, off: u16, file: *const c_char, func: *const c_char, line: c_int);
    fn jump_table_impl(addr: u32, expected_retip: u16, file: *const c_char, func: *const c_char, line: c_int);
    fn near_ret_tail_impl(popped_ip: u16, expected_retip: u16, file: *const c_char, func: *const c_char, line: c_int);
    fn long_jump_impl(seg: u16, off: u16, file: *const c_char, func: *const c_char, line: c_int);
    fn iret_impl(file: *const c_char, func: *const c_char, line: c_int);
    fn retf_impl(file: *const c_char, func: *const c_char, line: c_int);
    fn retf_pop_impl(file: *const c_char, func: *const c_char, line: c_int, pop_bytes: u16);
    fn run_interrupt_impl(int_no: u8, file: *const c_char, func: *const c_char, line: c_int);
    fn schedule_interrupt_impl(int_no: u8, file: *const c_char, func: *const c_char, line: c_int);

    fn shim_enter_binary(name: *const c_char);
    fn shim_leave_binary();
    fn shim_tail_dispatch_save(out: *mut ShimTailDispatchState);
    fn shim_tail_dispatch_restore(inp: *const ShimTailDispatchState);
    fn shim_drain_pending_tail_dispatch(file: *const c_char, func: *const c_char, line: c_int);
    fn shim_swap_regions_w(es_seg: u16, di_off: u16, ds_seg: u16, si_off: u16, count: u16, df: c_int);

    fn rep_movsb_block_impl(dst_seg: u16, src_seg: u16, file: *const c_char, func: *const c_char, line: c_int);
    fn rep_movsw_block_impl(dst_seg: u16, src_seg: u16, file: *const c_char, func: *const c_char, line: c_int);
    fn rep_stosb_block_impl(dst_seg: u16, file: *const c_char, func: *const c_char, line: c_int);
    pub fn scanMemoryForAl(dst: *const u8, value: u8, count: u16, direction: c_int, last_byte: *mut u8) -> u16;
    pub fn compareMemoryUntilMismatch(src: *const u8, dst: *const u8, count: u16, direction: c_int) -> u8;

    fn rcb_read8_impl(field: c_int, file: *const c_char, func: *const c_char, line: c_int) -> u8;
    fn rcb_write8_impl(field: c_int, value: u8, file: *const c_char, func: *const c_char, line: c_int);
    fn rcb_read16_impl(field: c_int, file: *const c_char, func: *const c_char, line: c_int) -> u16;
    fn rcb_write16_impl(field: c_int, value: u16, file: *const c_char, func: *const c_char, line: c_int);

    // pub so generated chunks can call them directly (they carry no file/func/line).
    pub fn inb(port: u16) -> u8;
    pub fn inw(port: u16) -> u16;
    pub fn outb(port: u16, value: u8);
    pub fn outw(port: u16, value: u16);

    // DOS (INT 21h) services used by the chunk corpus.
    fn dos_api_impl(file: *const c_char, func: *const c_char, line: c_int) -> u8;
    fn dos_exit_impl(file: *const c_char, func: *const c_char, line: c_int);
    fn dos_get_version_impl(file: *const c_char, func: *const c_char, line: c_int) -> u8;
    fn dos_print_string_impl(s: *const c_char, file: *const c_char, func: *const c_char, line: c_int) -> u8;
    fn dos_write_char_impl(ch_val: u8, file: *const c_char, func: *const c_char, line: c_int) -> u8;
    fn dos_direct_console_io_impl(dl_val: u8, file: *const c_char, func: *const c_char, line: c_int) -> u8;
    fn dos_read_file_impl(handle: u16, buf: *mut c_void, len: u16, file: *const c_char, func: *const c_char, line: c_int) -> u8;
    fn dos_write_file_impl(handle: u16, buf: *const c_void, len: u16, file: *const c_char, func: *const c_char, line: c_int) -> u8;
    fn dos_open_file_impl(path: *const c_char, file: *const c_char, func: *const c_char, line: c_int) -> u8;
    fn dos_close_file_impl(handle: u16, file: *const c_char, func: *const c_char, line: c_int) -> u8;
    fn dos_close_fcb_impl(dx_val: u16, file: *const c_char, func: *const c_char, line: c_int) -> u8;
    fn dos_lseek_impl(handle: u16, off_hi: u16, off_lo: u16, origin: u8, file: *const c_char, func: *const c_char, line: c_int) -> u8;
    fn dos_reset_disk_impl(file: *const c_char, func: *const c_char, line: c_int) -> u8;
    fn dos_set_interrupt_vector_impl(int_no: u8, segment: u16, offset: u16, file: *const c_char, func: *const c_char, line: c_int) -> u8;
    fn dos_get_interrupt_vector_impl(file: *const c_char, func: *const c_char, line: c_int) -> u8;
    fn dos_alloc_mem_impl(parags: u16, file: *const c_char, func: *const c_char, line: c_int) -> u8;
    fn dos_free_mem_impl(ptr: *mut c_void, file: *const c_char, func: *const c_char, line: c_int) -> u8;
    fn dos_exec_impl(param_block: *mut c_void, cmd: *const c_char, file: *const c_char, func: *const c_char, line: c_int) -> u8;

    // BIOS services used by the chunk corpus.
    fn bios_set_video_mode_impl(mode: u8, file: *const c_char, func: *const c_char, line: c_int);
    fn bios_set_palette_impl(file: *const c_char, func: *const c_char, line: c_int);
    fn bios_keyboard_impl(file: *const c_char, func: *const c_char, line: c_int);
}

// ---- thin safe wrappers the emitter targets (inject file/func/line = jit,0) --

// Read fast path (Fix 2): the common case — a normal RAM/VRAM read — is inlined
// into the chunk (no cross-.so FFI). Fall back to the *_impl only for the
// address classes the impl treats specially, so behavior is byte-identical:
//   memb: the es:FF00 RCB register-window (memory-mapped CPU registers).
//   memw: the RCB window, the stack segment (stack-op forensics ring), and the
//         null page 0000:0000..000F (null-pointer warning).
#[inline(always)] pub fn memb(seg: u16, off: u16) -> u8 {
    unsafe {
        if seg == es() && off >= 0xFF00 {
            return memb_read_impl(seg, off, site(), site(), 0);
        }
        *seg_off(seg, off)
    }
}
#[inline(always)] pub fn memw(seg: u16, off: u16) -> u16 {
    unsafe {
        if (seg == es() && off >= 0xFF00) || seg == ss() || (seg == 0 && off < 0x10) {
            return memw_read_impl(seg, off, site(), site(), 0);
        }
        memw_raw_read_inline(seg, off)
    }
}
#[inline(always)] pub fn memb_write(seg: u16, off: u16, v: u8) { unsafe { memb_write_impl(seg, off, v, site(), site(), 0) } }
#[inline(always)] pub fn memw_write(seg: u16, off: u16, v: u16) { unsafe { memw_write_impl(seg, off, v, site(), site(), 0) } }
#[inline(always)] pub fn SAFEPOINT() { unsafe { safe_point_impl(site(), site(), 0) } }

#[inline(always)] pub fn run_interrupt(int_no: u8) { unsafe { run_interrupt_impl(int_no, site(), site(), 0) } }
#[inline(always)] pub fn schedule_interrupt(int_no: u8) { unsafe { schedule_interrupt_impl(int_no, site(), site(), 0) } }

#[inline(always)] pub fn rep_movsb_block(dst_seg: u16, src_seg: u16) { unsafe { rep_movsb_block_impl(dst_seg, src_seg, site(), site(), 0) } }
#[inline(always)] pub fn rep_movsw_block(dst_seg: u16, src_seg: u16) { unsafe { rep_movsw_block_impl(dst_seg, src_seg, site(), site(), 0) } }
#[inline(always)] pub fn rep_stosb_block(dst_seg: u16) { unsafe { rep_stosb_block_impl(dst_seg, site(), site(), 0) } }

#[inline(always)] pub fn rcb_read8(field: c_int) -> u8 { unsafe { rcb_read8_impl(field, site(), site(), 0) } }
#[inline(always)] pub fn rcb_write8(field: c_int, v: u8) { unsafe { rcb_write8_impl(field, v, site(), site(), 0) } }
#[inline(always)] pub fn rcb_read16(field: c_int) -> u16 { unsafe { rcb_read16_impl(field, site(), site(), 0) } }
#[inline(always)] pub fn rcb_write16(field: c_int, v: u16) { unsafe { rcb_write16_impl(field, v, site(), site(), 0) } }

#[inline(always)] pub fn dos_api() -> u8 { unsafe { dos_api_impl(site(), site(), 0) } }
#[inline(always)] pub fn dos_exit() { unsafe { dos_exit_impl(site(), site(), 0) } }
#[inline(always)] pub fn dos_get_version() -> u8 { unsafe { dos_get_version_impl(site(), site(), 0) } }
#[inline(always)] pub fn dos_print_string(s: *const c_char) -> u8 { unsafe { dos_print_string_impl(s, site(), site(), 0) } }
#[inline(always)] pub fn dos_write_char(ch_val: u8) -> u8 { unsafe { dos_write_char_impl(ch_val, site(), site(), 0) } }
#[inline(always)] pub fn dos_direct_console_io(dl_val: u8) -> u8 { unsafe { dos_direct_console_io_impl(dl_val, site(), site(), 0) } }
#[inline(always)] pub fn dos_read_file(handle: u16, buf: *mut c_void, len: u16) -> u8 { unsafe { dos_read_file_impl(handle, buf, len, site(), site(), 0) } }
#[inline(always)] pub fn dos_write_file(handle: u16, buf: *const c_void, len: u16) -> u8 { unsafe { dos_write_file_impl(handle, buf, len, site(), site(), 0) } }
#[inline(always)] pub fn dos_open_file(path: *const c_char) -> u8 { unsafe { dos_open_file_impl(path, site(), site(), 0) } }
#[inline(always)] pub fn dos_close_file(handle: u16) -> u8 { unsafe { dos_close_file_impl(handle, site(), site(), 0) } }
#[inline(always)] pub fn dos_close_fcb(dx_val: u16) -> u8 { unsafe { dos_close_fcb_impl(dx_val, site(), site(), 0) } }
#[inline(always)] pub fn dos_lseek(handle: u16, off_hi: u16, off_lo: u16, origin: u8) -> u8 { unsafe { dos_lseek_impl(handle, off_hi, off_lo, origin, site(), site(), 0) } }
#[inline(always)] pub fn dos_reset_disk() -> u8 { unsafe { dos_reset_disk_impl(site(), site(), 0) } }
#[inline(always)] pub fn dos_set_interrupt_vector(int_no: u8, segment: u16, offset: u16) -> u8 { unsafe { dos_set_interrupt_vector_impl(int_no, segment, offset, site(), site(), 0) } }
#[inline(always)] pub fn dos_get_interrupt_vector() -> u8 { unsafe { dos_get_interrupt_vector_impl(site(), site(), 0) } }
#[inline(always)] pub fn dos_alloc_mem(parags: u16) -> u8 { unsafe { dos_alloc_mem_impl(parags, site(), site(), 0) } }
#[inline(always)] pub fn dos_free_mem(ptr: *mut c_void) -> u8 { unsafe { dos_free_mem_impl(ptr, site(), site(), 0) } }
#[inline(always)] pub fn dos_exec(param_block: *mut c_void, cmd: *const c_char) -> u8 { unsafe { dos_exec_impl(param_block, cmd, site(), site(), 0) } }

#[inline(always)] pub fn bios_set_video_mode(mode: u8) { unsafe { bios_set_video_mode_impl(mode, site(), site(), 0) } }
#[inline(always)] pub fn bios_set_palette() { unsafe { bios_set_palette_impl(site(), site(), 0) } }
#[inline(always)] pub fn bios_keyboard() { unsafe { bios_keyboard_impl(site(), site(), 0) } }

#[inline(always)] pub fn enter_binary(name: *const c_char) { unsafe { shim_enter_binary(name) } }
#[inline(always)] pub fn leave_binary() { unsafe { shim_leave_binary() } }
#[inline(always)] pub fn tail_dispatch_save(out: *mut ShimTailDispatchState) { unsafe { shim_tail_dispatch_save(out) } }
#[inline(always)] pub fn tail_dispatch_restore(inp: *const ShimTailDispatchState) { unsafe { shim_tail_dispatch_restore(inp) } }
#[inline(always)] pub fn drain_pending_tail_dispatch() { unsafe { shim_drain_pending_tail_dispatch(site(), site(), 0) } }
#[inline(always)] pub fn swap_regions_w(es_seg: u16, di_off: u16, ds_seg: u16, si_off: u16, count: u16, df: u8) { unsafe { shim_swap_regions_w(es_seg, di_off, ds_seg, si_off, count, df as c_int) } }

#[inline(always)] pub fn call_table_(ret_ip: u16, addr: u32) { unsafe { call_table_impl(ret_ip, addr, site(), site(), 0) } }
#[inline(always)] pub fn lcall_table_(ret_ip: u16, seg: u16, off: u16) { unsafe { lcall_table_impl(ret_ip, seg, off, site(), site(), 0) } }
#[inline(always)] pub fn jump_table_(addr: u32, expected_retip: u16) { unsafe { jump_table_impl(addr, expected_retip, site(), site(), 0) } }
#[inline(always)] pub fn near_ret_tail_(popped_ip: u16, expected_retip: u16) { unsafe { near_ret_tail_impl(popped_ip, expected_retip, site(), site(), 0) } }
#[inline(always)] pub fn long_jump_(seg: u16, off: u16) { unsafe { long_jump_impl(seg, off, site(), site(), 0) } }
#[inline(always)] pub fn iret_() { unsafe { iret_impl(site(), site(), 0) } }
#[inline(always)] pub fn retf_() { unsafe { retf_impl(site(), site(), 0) } }
#[inline(always)] pub fn retf_pop_(pop_bytes: u16) { unsafe { retf_pop_impl(site(), site(), 0, pop_bytes) } }

// ---- RCB field ids (rcb_fields.h) -------------------------------------------

pub const FIELD_1: c_int = 0xFF00;
pub const PROGRAM_SEG: c_int = 0xFF02;
pub const PREV_TIMER_VECTOR_OFF: c_int = 0xFF04;
pub const PREV_TIMER_VECTOR_SEG: c_int = 0xFF06;
pub const FIELD_5: c_int = 0xFF08;
pub const FIELD_6: c_int = 0xFF09;
pub const JOYSTICK_FLAG: c_int = 0xFF0A;
pub const FIELD_8: c_int = 0xFF0B;
pub const DATA_BUF1_OFF: c_int = 0xFF0C;
pub const DATA_BUF1_SEG: c_int = 0xFF0E;
pub const DATA_BUF2_OFF: c_int = 0xFF10;
pub const DATA_BUF2_SEG: c_int = 0xFF12;
pub const VIDEO_DRIVER_INDEX: c_int = 0xFF14;
pub const MUSIC_DRIVER_FLAG: c_int = 0xFF15;
pub const FIELD_15: c_int = 0xFF16;
pub const FIELD_16: c_int = 0xFF17;
pub const FIELD_17: c_int = 0xFF18;
pub const FIELD_18: c_int = 0xFF1D;
pub const FIELD_19: c_int = 0xFF1E;
pub const FIELD_20: c_int = 0xFF1F;
pub const FIELD_21: c_int = 0xFF26;
pub const FIELD_22: c_int = 0xFF27;
pub const FIELD_23: c_int = 0xFF28;
pub const DATA_BASE_SEG: c_int = 0xFF2C;
pub const FIELD_25: c_int = 0xFF33;
pub const FIELD_26: c_int = 0xFF34;
pub const FIELD_27: c_int = 0xFF38;
pub const FIELD_28: c_int = 0xFF39;
pub const FIELD_29: c_int = 0xFF3A;
pub const FIELD_30: c_int = 0xFF3B;
pub const FIELD_31: c_int = 0xFF3C;
pub const FIELD_32: c_int = 0xFF40;
pub const FIELD_33: c_int = 0xFF42;
pub const FIELD_34: c_int = 0xFF43;
pub const FIELD_35: c_int = 0xFF74;
pub const FIELD_36: c_int = 0xFF75;
pub const FIELD_37: c_int = 0xFF78;
pub const PREV_KEYBOARD_VECTOR_OFF: c_int = 0xFF79;
pub const PREV_KEYBOARD_VECTOR_SEG: c_int = 0xFF7B;
} // mod rt

pub use rt::*;

// no_std chunk support: chunks need only `core` + the runtime ABI, so they are
// compiled `#![no_std]` (tiny, fast .so — no libstd link). That requires a
// panic handler; chunks are also compiled `-C panic=abort`, and the translated
// code never panics in normal operation, so route any panic to libc abort().
extern "C" {
    fn abort() -> !;
}
#[panic_handler]
fn saisei_rt_panic(_info: &core::panic::PanicInfo) -> ! {
    unsafe { abort() }
}

// no_std cdylibs still emit a reference to the unwind personality (e.g. the
// div-by-zero landing pad) even under `-C panic=abort`; provide a stub. Chunks
// never unwind (panic=abort -> the handler aborts), so it is never called.
#[no_mangle]
pub extern "C" fn rust_eh_personality() {}
