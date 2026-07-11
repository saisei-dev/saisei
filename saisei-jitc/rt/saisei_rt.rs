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
    static mut jit_instr_budget: i64;
    // One byte per 4KB page of guest memory (MEMORY_SIZE = 1<<21 → 512 pages;
    // must match runtime/src/shims.rs). Nonzero = this page has a special
    // write behavior (JIT'd code, watches, protected slots, null page, .drv
    // mapping, bookend capture) — route the write through the *_impl.
    static mut mem_page_flags: [u8; 512];
    fn shim_idle_wait();

    // Stack-op forensics ring (runtime/src/shims.rs) — appended INLINE by the
    // SS-segment fast path below; layout/filter must match stack_op_record.
    static mut stack_write_ring: [StackWriteEvent; STACK_WRITE_RING_SIZE];
    static mut stack_write_ring_pos: u32;
    static mut isr_depth: u8;
    static mut lcall_depth: u8;
    // Virtual-clock state (runtime/src/timer.rs) for the inline forensic
    // timestamp; mirrors shim_virtual_now_ns exactly.
    static vclock_state: i32;
    static vclock_frozen_virtual_ns: u64;
    static vclock_step_deadline_virtual_ns: u64;
    static virtual_now_accum_ns: u64;
    static host_time_origin_ns: u64;
}

// ---- stack-op forensics ring: inline SS fast path ---------------------------
//
// Mirrors runtime/src/shims.rs `StackWriteEvent` (#[repr(C)], FROZEN together)
// and `stack_op_record` byte-for-byte: same [-16, +256]-of-SP filter, same
// fields, virtual-clock t_us. Inlining this append is what lets every
// `[bp+x]` local access and push/pop stay inside the chunk — the ring was the
// only reason SS-segment words crossed the FFI boundary.

#[repr(C)]
pub struct StackWriteEvent {
    pub t_us: u32,
    pub writer_cs: u16,
    pub writer_ip: u16,
    pub target_ss: u16,
    pub target_off: u16,
    pub value: u16,
    pub sp_at_op: u16,
    pub kind: u8,
    pub isr_depth_at: u8,
    pub lcall_depth_at: u8,
    pub reserved: u8,
    pub file: *const c_char,
    pub line: u32,
}
const STACK_WRITE_RING_SIZE: usize = 2048;
const SWO_PUSH: u8 = 1;
const SWO_POP: u8 = 2;

/// shim_virtual_now_ns, reimplemented inline (runtime/src/timer.rs).
#[inline(always)]
fn virtual_now_ns_inline() -> u64 {
    unsafe {
        match vclock_state {
            1 => vclock_frozen_virtual_ns, // HALTED
            2 if virtual_now_accum_ns > vclock_step_deadline_virtual_ns => {
                vclock_step_deadline_virtual_ns // STEPPING past deadline
            }
            _ => virtual_now_accum_ns,
        }
    }
}

/// The read path for everything the inline `memw` fast path excludes: the RCB
/// window and null page go to the impl; a stack-segment read records its POP
/// inline (no FFI). `#[inline(never)]`: one copy per chunk — inlining this (or
/// even a bare extra call edge) at every memw site measurably blows up rustc
/// time, and JIT compile latency is a first-class constraint.
#[inline(never)]
fn memw_read_slow(seg: u16, off: u16) -> u16 {
    unsafe {
        if (seg == es() && off >= 0xFF00) || (seg == 0 && off < 0x10) {
            return memw_read_impl(seg, off, site(), site(), 0);
        }
        let v = memw_raw_read_inline(seg, off);
        stack_ring_record(SWO_POP, seg, off, v);
        v
    }
}

/// The write path for the RCB window and the stack segment (see
/// memw_read_slow). Flagged pages still route to the impl so every special
/// write behavior runs; a plain stack write records its PUSH and stores.
#[inline(never)]
fn memw_write_slow(seg: u16, off: u16, v: u16) {
    unsafe {
        if seg == es() && off >= 0xFF00 {
            return memw_write_impl(seg, off, v, site(), site(), 0);
        }
        let addr = linear_addr(seg, off);
        let addr_hi = mask_addr_rt(addr.wrapping_add(1));
        if mem_page_flags[(addr >> 12) as usize] != 0
            || mem_page_flags[(addr_hi >> 12) as usize] != 0
        {
            return memw_write_impl(seg, off, v, site(), site(), 0);
        }
        stack_ring_record(SWO_PUSH, seg, off, v);
        *virtual_memory.add(addr as usize) = (v & 0xFF) as u8;
        *virtual_memory.add(addr_hi as usize) = (v >> 8) as u8;
    }
}

/// stack_op_record, reimplemented inline. `seg == ss()` established by caller.
#[inline(always)]
fn stack_ring_record(kind: u8, seg: u16, off: u16, value: u16) {
    unsafe {
        let rel = off.wrapping_sub(sp()) as i16;
        if rel < -16 || rel > 256 {
            return;
        }
        let idx = (stack_write_ring_pos & (STACK_WRITE_RING_SIZE as u32 - 1)) as usize;
        stack_write_ring_pos = stack_write_ring_pos.wrapping_add(1);
        let e = &mut stack_write_ring[idx];
        e.t_us = (virtual_now_ns_inline().saturating_sub(host_time_origin_ns) / 1000) as u32;
        e.writer_cs = cs();
        e.writer_ip = ip();
        e.target_ss = seg;
        e.target_off = off;
        e.value = value;
        e.sp_at_op = sp();
        e.kind = kind;
        e.isr_depth_at = isr_depth;
        e.lcall_depth_at = lcall_depth;
        e.file = site();
        e.line = 0;
    }
}

#[inline(always)]
pub fn set_interrupt_shadow(v: u8) {
    unsafe { interrupt_shadow = v; }
}

/// Per-basic-block safepoint poll: debit the block's summed per-class
/// instruction weights (budget units ≈ 386 cycles / 3.3; see insn_weight in
/// codegen.rs) from the budget; only a depleted budget takes the slow path
/// (IRQ delivery, PIT, input, pacing — safe_point_impl). The budget is the
/// flow of virtual time: the runtime folds consumed units into the virtual
/// clock at each refill, which is what makes calibration loops see a
/// fixed-speed CPU. Inline rep loops also call this with their dynamic
/// iteration count.
#[inline(always)]
pub fn JIT_BUDGET(n: u32) {
    unsafe {
        jit_instr_budget -= n as i64;
        if jit_instr_budget <= 0 {
            safe_point_impl(site(), site(), 0);
        }
    }
}

/// Emitted for `hlt`: wait for the next interrupt with machine time flowing
/// at host pace (the guest retires nothing while halted).
#[inline(always)]
pub fn HLT_WAIT() {
    unsafe { shim_idle_wait() }
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

    fn shim_drain_pending_tail_dispatch(file: *const c_char, func: *const c_char, line: c_int);
    fn shim_swap_regions_w(es_seg: u16, di_off: u16, ds_seg: u16, si_off: u16, count: u16, df: c_int);

    // Wrapper-bookkeeping state, exported by the runtime so the per-function
    // wrapper prologue/epilogue (enter/leave_binary, tail_dispatch_save/
    // restore, the drain gate) is pure inline data access — the wrapper makes
    // no FFI call unless a tail dispatch is actually pending.
    static mut tail_dispatch_pending: bool;
    static mut tail_dispatch_addr: u32;
    static mut tail_dispatch_expected: u16;
    static mut active_binary_stack: [*const c_char; 64];
    static mut active_binary_top: c_int;

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
//   memw: the RCB window and the null page 0000:0000..000F (null-pointer
//         warning). Stack-segment reads record their POP event inline.
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
            return memw_read_slow(seg, off);
        }
        memw_raw_read_inline(seg, off)
    }
}
// Write fast path: the common case — a plain RAM/VRAM store to a page with no
// special behavior — is inlined (no cross-.so FFI). The page-flag table routes
// everything special (JIT'd-code invalidation, watches, protected slots, null
// page, .drv mappings, bookend capture) through the impl, byte-identically.
// memw additionally appends stack-segment PUSH events to the forensics ring —
// inline (stack_ring_record), so SS words no longer cross the FFI boundary.
#[inline(always)] pub fn memb_write(seg: u16, off: u16, v: u8) {
    unsafe {
        if seg == es() && off >= 0xFF00 {
            return memb_write_impl(seg, off, v, site(), site(), 0);
        }
        let addr = linear_addr(seg, off);
        if mem_page_flags[(addr >> 12) as usize] != 0 {
            return memb_write_impl(seg, off, v, site(), site(), 0);
        }
        *virtual_memory.add(addr as usize) = v;
    }
}
#[inline(always)] pub fn memw_write(seg: u16, off: u16, v: u16) {
    unsafe {
        if (seg == es() && off >= 0xFF00) || seg == ss() {
            return memw_write_slow(seg, off, v);
        }
        let addr = linear_addr(seg, off);
        // High byte's address is A20-remasked, exactly like memw_raw_read_inline;
        // a word can also straddle a page boundary: both pages must be plain.
        let addr_hi = mask_addr_rt(addr.wrapping_add(1));
        if mem_page_flags[(addr >> 12) as usize] != 0
            || mem_page_flags[(addr_hi >> 12) as usize] != 0
        {
            return memw_write_impl(seg, off, v, site(), site(), 0);
        }
        *virtual_memory.add(addr as usize) = (v & 0xFF) as u8;
        *virtual_memory.add(addr_hi as usize) = (v >> 8) as u8;
    }
}
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

// Inline mirrors of the runtime's shim_enter_binary/shim_leave_binary/
// shim_tail_dispatch_save/shim_tail_dispatch_restore (runtime/src/shims.rs) —
// same state, no FFI hop. Behavior must stay byte-identical to the shims.
#[inline(always)] pub fn enter_binary(name: *const c_char) {
    unsafe {
        if active_binary_top < 64 { active_binary_stack[active_binary_top as usize] = name; }
        active_binary_top += 1;
    }
}
#[inline(always)] pub fn leave_binary() {
    unsafe { if active_binary_top > 0 { active_binary_top -= 1; } }
}
#[inline(always)] pub fn tail_dispatch_save(out: *mut ShimTailDispatchState) {
    unsafe {
        (*out).pending = tail_dispatch_pending as u8;
        (*out).addr = tail_dispatch_addr;
        (*out).expected = tail_dispatch_expected;
        tail_dispatch_pending = false;
    }
}
#[inline(always)] pub fn tail_dispatch_restore(inp: *const ShimTailDispatchState) {
    unsafe {
        tail_dispatch_pending = (*inp).pending != 0;
        tail_dispatch_addr = (*inp).addr;
        tail_dispatch_expected = (*inp).expected;
    }
}
#[inline(always)] pub fn drain_pending_tail_dispatch() {
    // Gate the FFI call on the pending flag — false on the common path.
    unsafe {
        if tail_dispatch_pending {
            shim_drain_pending_tail_dispatch(site(), site(), 0);
        }
    }
}
#[inline(always)] pub fn swap_regions_w(es_seg: u16, di_off: u16, ds_seg: u16, si_off: u16, count: u16, df: u8) { unsafe { shim_swap_regions_w(es_seg, di_off, ds_seg, si_off, count, df as c_int) } }

#[inline(always)] pub fn call_table_(ret_ip: u16, addr: u32) { unsafe { call_table_impl(ret_ip, addr, site(), site(), 0) } }
#[inline(always)] pub fn lcall_table_(ret_ip: u16, seg: u16, off: u16) { unsafe { lcall_table_impl(ret_ip, seg, off, site(), site(), 0) } }
#[inline(always)] pub fn jump_table_(addr: u32, expected_retip: u16) { unsafe { jump_table_impl(addr, expected_retip, site(), site(), 0) } }
#[inline(always)] pub fn near_ret_tail_(popped_ip: u16, expected_retip: u16) { unsafe { near_ret_tail_impl(popped_ip, expected_retip, site(), site(), 0) } }
#[inline(always)] pub fn long_jump_(seg: u16, off: u16) { unsafe { long_jump_impl(seg, off, site(), site(), 0) } }
#[inline(always)] pub fn iret_() { unsafe { iret_impl(site(), site(), 0) } }
#[inline(always)] pub fn retf_() { unsafe { retf_impl(site(), site(), 0) } }
#[inline(always)] pub fn retf_pop_(pop_bytes: u16) { unsafe { retf_pop_impl(site(), site(), 0, pop_bytes) } }

// ---- block-local register cache ---------------------------------------------
//
// `Regs` caches the guest CPU state in a local the dispatch loop threads
// through every block fn (`r: &mut Regs`). Emitted code reads/writes registers
// and flags through `r.` methods (same names as the free accessors), so rustc
// can keep them in host registers across instructions: the `&mut Regs` param
// is `noalias`, which raw stores through `virtual_memory` and the shared `cpu`
// global are not. The invariant: the global `cpu` is coherent at every extern
// (runtime) call and at dispatch exit — methods that call the runtime spill
// first (and reload after, when the callee can mutate guest registers: IRQ
// delivery, DOS/BIOS services, transfers, string-block shims). cs/ds/es/ss/ip
// are write-through (setters update local AND global) so runtime forensics
// that sample them between spills stay exact.

pub struct Regs {
    pub ax: u16, pub bx: u16, pub cx: u16, pub dx: u16,
    pub si_: u16, pub di_: u16, pub bp_: u16, pub sp_: u16,
    pub ip_: u16, pub cs_: u16, pub ds_: u16, pub es_: u16, pub ss_: u16,
    pub cf: u8, pub pf: u8, pub zf: u8, pub sf: u8, pub of: u8, pub if_: u8, pub df: u8,
}

macro_rules! regs_word {
    ($get:ident, $set:ident, $field:ident) => {
        #[inline(always)] pub fn $get(&self) -> u16 { self.$field }
        #[inline(always)] pub fn $set(&mut self, v: u16) { self.$field = v; }
    };
}
macro_rules! regs_seg {
    ($get:ident, $set:ident, $field:ident, $cpu_field:ident) => {
        #[inline(always)] pub fn $get(&self) -> u16 { self.$field }
        #[inline(always)] pub fn $set(&mut self, v: u16) {
            self.$field = v;
            unsafe { (*cpu_ptr()).$cpu_field = v; } // write-through
        }
    };
}
macro_rules! regs_byte {
    ($lo:ident, $set_lo:ident, $hi:ident, $set_hi:ident, $field:ident) => {
        #[inline(always)] pub fn $lo(&self) -> u8 { self.$field as u8 }
        #[inline(always)] pub fn $set_lo(&mut self, v: u8) { self.$field = (self.$field & 0xFF00) | v as u16; }
        #[inline(always)] pub fn $hi(&self) -> u8 { (self.$field >> 8) as u8 }
        #[inline(always)] pub fn $set_hi(&mut self, v: u8) { self.$field = (self.$field & 0x00FF) | ((v as u16) << 8); }
    };
}
macro_rules! regs_flag {
    ($get:ident, $set:ident, $field:ident, $cpu_field:ident) => {
        #[inline(always)] pub fn $get(&self) -> u8 { self.$field }
        #[inline(always)] pub fn $set(&mut self, v: u8) { self.$field = v; }
    };
}
/// &mut runtime-call method: spill, call the free wrapper, reload (the callee
/// may mutate guest registers — IRQ delivery, DOS services, transfers).
macro_rules! regs_ffi_mut {
    ($name:ident($($a:ident: $t:ty),*) $(-> $r:ty)?) => {
        #[inline(always)] pub fn $name(&mut self, $($a: $t),*) $(-> $r)? {
            self.spill();
            let out = $name($($a),*);
            self.reload();
            out
        }
    };
}

impl Regs {
    #[inline(always)]
    pub fn load() -> Regs {
        unsafe {
            let c = cpu_ptr();
            Regs {
                ax: (*c).r_ax, bx: (*c).r_bx, cx: (*c).r_cx, dx: (*c).r_dx,
                si_: (*c).si, di_: (*c).di, bp_: (*c).bp, sp_: (*c).sp,
                ip_: (*c).r_ip, cs_: (*c).r_cs, ds_: (*c).r_ds, es_: (*c).r_es, ss_: (*c).r_ss,
                cf: (*c).flags.CF, pf: (*c).flags.PF, zf: (*c).flags.ZF,
                sf: (*c).flags.SF, of: (*c).flags.OF, if_: (*c).flags.IF, df: (*c).flags.DF,
            }
        }
    }
    #[inline(always)]
    pub fn spill(&self) {
        unsafe {
            let c = cpu_ptr();
            (*c).r_ax = self.ax; (*c).r_bx = self.bx; (*c).r_cx = self.cx; (*c).r_dx = self.dx;
            (*c).si = self.si_; (*c).di = self.di_; (*c).bp = self.bp_; (*c).sp = self.sp_;
            (*c).r_ip = self.ip_; (*c).r_cs = self.cs_; (*c).r_ds = self.ds_;
            (*c).r_es = self.es_; (*c).r_ss = self.ss_;
            (*c).flags.CF = self.cf; (*c).flags.PF = self.pf; (*c).flags.ZF = self.zf;
            (*c).flags.SF = self.sf; (*c).flags.OF = self.of; (*c).flags.IF = self.if_;
            (*c).flags.DF = self.df;
        }
    }
    #[inline(always)]
    pub fn reload(&mut self) {
        *self = Regs::load();
    }

    regs_word!(ax, set_ax, ax);
    regs_word!(bx, set_bx, bx);
    regs_word!(cx, set_cx, cx);
    regs_word!(dx, set_dx, dx);
    regs_word!(si, set_si, si_);
    regs_word!(di, set_di, di_);
    regs_word!(bp, set_bp, bp_);
    regs_word!(sp, set_sp, sp_);
    regs_seg!(ip, set_ip, ip_, r_ip);
    regs_seg!(cs, set_cs, cs_, r_cs);
    regs_seg!(ds, set_ds, ds_, r_ds);
    regs_seg!(es, set_es, es_, r_es);
    regs_seg!(ss, set_ss, ss_, r_ss);
    regs_byte!(al, set_al, ah, set_ah, ax);
    regs_byte!(bl, set_bl, bh, set_bh, bx);
    regs_byte!(cl, set_cl, ch, set_ch, cx);
    regs_byte!(dl, set_dl, dh, set_dh, dx);
    regs_flag!(CF, set_CF, cf, CF);
    regs_flag!(PF, set_PF, pf, PF);
    regs_flag!(ZF, set_ZF, zf, ZF);
    regs_flag!(SF, set_SF, sf, SF);
    regs_flag!(OF, set_OF, of, OF);
    regs_flag!(IF, set_IF, if_, IF);
    regs_flag!(DF, set_DF, df, DF);

    // -- memory: same fast/slow shape as the free fns; the slow helpers spill
    //    before entering the runtime so forensics see exact registers, and
    //    record stack ops from the LOCAL sp/cs/ip. The impls never write guest
    //    registers, so &self + no reload.
    #[inline(always)]
    pub fn memb(&self, seg: u16, off: u16) -> u8 {
        unsafe {
            if seg == self.es_ && off >= 0xFF00 {
                return self.memb_rcb_read(seg, off);
            }
            *seg_off(seg, off)
        }
    }
    #[inline(never)]
    fn memb_rcb_read(&self, seg: u16, off: u16) -> u8 {
        self.spill();
        unsafe { memb_read_impl(seg, off, site(), site(), 0) }
    }
    #[inline(always)]
    pub fn memw(&self, seg: u16, off: u16) -> u16 {
        unsafe {
            if (seg == self.es_ && off >= 0xFF00) || seg == self.ss_ || (seg == 0 && off < 0x10) {
                return self.memw_read_slow(seg, off);
            }
            memw_raw_read_inline(seg, off)
        }
    }
    #[inline(never)]
    fn memw_read_slow(&self, seg: u16, off: u16) -> u16 {
        unsafe {
            if (seg == self.es_ && off >= 0xFF00) || (seg == 0 && off < 0x10) {
                self.spill();
                return memw_read_impl(seg, off, site(), site(), 0);
            }
            let v = memw_raw_read_inline(seg, off);
            self.ring_record(SWO_POP, seg, off, v);
            v
        }
    }
    #[inline(always)]
    pub fn memb_write(&self, seg: u16, off: u16, v: u8) {
        unsafe {
            let addr = linear_addr(seg, off);
            if (seg == self.es_ && off >= 0xFF00) || mem_page_flags[(addr >> 12) as usize] != 0 {
                return self.memb_write_slow(seg, off, v);
            }
            *virtual_memory.add(addr as usize) = v;
        }
    }
    #[inline(never)]
    fn memb_write_slow(&self, seg: u16, off: u16, v: u8) {
        self.spill();
        unsafe { memb_write_impl(seg, off, v, site(), site(), 0) }
    }
    #[inline(always)]
    pub fn memw_write(&self, seg: u16, off: u16, v: u16) {
        unsafe {
            if (seg == self.es_ && off >= 0xFF00) || seg == self.ss_ {
                return self.memw_write_slow(seg, off, v);
            }
            let addr = linear_addr(seg, off);
            let addr_hi = mask_addr_rt(addr.wrapping_add(1));
            if mem_page_flags[(addr >> 12) as usize] != 0
                || mem_page_flags[(addr_hi >> 12) as usize] != 0
            {
                return self.memw_write_flagged(seg, off, v);
            }
            *virtual_memory.add(addr as usize) = (v & 0xFF) as u8;
            *virtual_memory.add(addr_hi as usize) = (v >> 8) as u8;
        }
    }
    #[inline(never)]
    fn memw_write_flagged(&self, seg: u16, off: u16, v: u16) {
        self.spill();
        unsafe { memw_write_impl(seg, off, v, site(), site(), 0) }
    }
    #[inline(never)]
    fn memw_write_slow(&self, seg: u16, off: u16, v: u16) {
        unsafe {
            if seg == self.es_ && off >= 0xFF00 {
                self.spill();
                return memw_write_impl(seg, off, v, site(), site(), 0);
            }
            let addr = linear_addr(seg, off);
            let addr_hi = mask_addr_rt(addr.wrapping_add(1));
            if mem_page_flags[(addr >> 12) as usize] != 0
                || mem_page_flags[(addr_hi >> 12) as usize] != 0
            {
                self.spill();
                return memw_write_impl(seg, off, v, site(), site(), 0);
            }
            self.ring_record(SWO_PUSH, seg, off, v);
            *virtual_memory.add(addr as usize) = (v & 0xFF) as u8;
            *virtual_memory.add(addr_hi as usize) = (v >> 8) as u8;
        }
    }
    /// stack_op_record from the LOCAL register cache (see stack_ring_record).
    #[inline(always)]
    fn ring_record(&self, kind: u8, seg: u16, off: u16, value: u16) {
        unsafe {
            let rel = off.wrapping_sub(self.sp_) as i16;
            if rel < -16 || rel > 256 {
                return;
            }
            let idx = (stack_write_ring_pos & (STACK_WRITE_RING_SIZE as u32 - 1)) as usize;
            stack_write_ring_pos = stack_write_ring_pos.wrapping_add(1);
            let e = &mut stack_write_ring[idx];
            e.t_us = (virtual_now_ns_inline().saturating_sub(host_time_origin_ns) / 1000) as u32;
            e.writer_cs = self.cs_;
            e.writer_ip = self.ip_;
            e.target_ss = seg;
            e.target_off = off;
            e.value = value;
            e.sp_at_op = self.sp_;
            e.kind = kind;
            e.isr_depth_at = isr_depth;
            e.lcall_depth_at = lcall_depth;
            e.file = site();
            e.line = 0;
        }
    }

    // -- RCB / port I/O: runtime calls that never write guest registers.
    #[inline(never)] pub fn rcb_read8(&self, field: c_int) -> u8 { self.spill(); rcb_read8(field) }
    #[inline(never)] pub fn rcb_write8(&self, field: c_int, v: u8) { self.spill(); rcb_write8(field, v) }
    #[inline(never)] pub fn rcb_read16(&self, field: c_int) -> u16 { self.spill(); rcb_read16(field) }
    #[inline(never)] pub fn rcb_write16(&self, field: c_int, v: u16) { self.spill(); rcb_write16(field, v) }
    #[inline(never)] pub fn inb(&self, port: u16) -> u8 { self.spill(); unsafe { inb(port) } }
    #[inline(never)] pub fn inw(&self, port: u16) -> u16 { self.spill(); unsafe { inw(port) } }
    #[inline(never)] pub fn outb(&self, port: u16, v: u8) { self.spill(); unsafe { outb(port, v) } }
    #[inline(never)] pub fn outw(&self, port: u16, v: u16) { self.spill(); unsafe { outw(port, v) } }

    /// Per-block budget poll (see JIT_BUDGET): only a depleted budget takes
    /// the slow path, which can deliver IRQs — spill/reload there only.
    #[inline(always)]
    pub fn budget(&mut self, n: u32) {
        unsafe {
            jit_instr_budget -= n as i64;
            if jit_instr_budget <= 0 {
                self.budget_slow();
            }
        }
    }
    #[inline(never)]
    fn budget_slow(&mut self) {
        self.spill();
        unsafe { safe_point_impl(site(), site(), 0) };
        self.reload();
    }

    // -- runtime calls that can mutate guest registers: spill + reload.
    regs_ffi_mut!(HLT_WAIT());
    regs_ffi_mut!(run_interrupt(int_no: u8));
    regs_ffi_mut!(schedule_interrupt(int_no: u8));
    regs_ffi_mut!(dos_api() -> u8);
    regs_ffi_mut!(dos_exit());
    regs_ffi_mut!(bios_keyboard());
    regs_ffi_mut!(rep_movsb_block(dst_seg: u16, src_seg: u16));
    regs_ffi_mut!(rep_movsw_block(dst_seg: u16, src_seg: u16));
    regs_ffi_mut!(rep_stosb_block(dst_seg: u16));
    regs_ffi_mut!(call_table_(ret_ip: u16, addr: u32));
    regs_ffi_mut!(lcall_table_(ret_ip: u16, seg: u16, off: u16));
    regs_ffi_mut!(jump_table_(addr: u32, expected_retip: u16));
    regs_ffi_mut!(near_ret_tail_(popped_ip: u16, expected_retip: u16));
    regs_ffi_mut!(long_jump_(seg: u16, off: u16));
    regs_ffi_mut!(iret_());
    regs_ffi_mut!(retf_());
    regs_ffi_mut!(retf_pop_(pop_bytes: u16));
}

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
