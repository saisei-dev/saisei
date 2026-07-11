//! Shared access to the 8086 CPU state + virtual memory, for the OS/hardware
//! handlers (mouse/bios/dos) that marshal INT arguments through the registers.
//!
//! `cpu`/`virtual_memory`/`a20_enabled` are still DEFINED in shims.c today, so
//! these are `extern` bindings; when shims.c is ported the definitions move here.
//! The canonical CPU state layout (was cpu_state.h; FROZEN — shared with the JIT
//! chunks' saisei_rt prelude).

use core::ffi::{c_char, c_int};

#[repr(C)]
#[allow(non_snake_case)] // x86 flag names (FROZEN ABI, shared with JIT prelude)
pub struct CpuFlags {
    pub CF: u8,
    pub PF: u8,
    pub ZF: u8,
    pub SF: u8,
    pub OF: u8,
    pub IF: u8,
    pub DF: u8,
}

#[repr(C)]
pub struct CpuState {
    pub r_ax: u16,
    pub r_bx: u16,
    pub r_cx: u16,
    pub r_dx: u16,
    pub si: u16,
    pub di: u16,
    pub bp: u16,
    pub sp: u16,
    pub r_ip: u16,
    pub r_cs: u16,
    pub r_ds: u16,
    pub r_es: u16,
    pub r_ss: u16,
    pub flags: CpuFlags,
}

extern "C" {
    static mut cpu: CpuState;
    static mut virtual_memory: *mut u8;
    static a20_enabled: u8;
    /// Instrumentation hook the C handlers open with (shim_log).
    pub fn shim_log(
        func_name: *const c_char,
        file: *const c_char,
        func: *const c_char,
        line: c_int,
        path: *const c_char,
    );
}

#[inline(always)]
pub fn cpu_ptr() -> *mut CpuState {
    core::ptr::addr_of_mut!(cpu)
}

macro_rules! word_reg {
    ($get:ident, $set:ident, $field:ident) => {
        #[inline(always)]
        pub fn $get() -> u16 {
            unsafe { (*cpu_ptr()).$field }
        }
        #[inline(always)]
        pub fn $set(v: u16) {
            unsafe {
                (*cpu_ptr()).$field = v;
            }
        }
    };
}
word_reg!(ax, set_ax, r_ax);
word_reg!(bx, set_bx, r_bx);
word_reg!(cx, set_cx, r_cx);
word_reg!(dx, set_dx, r_dx);
word_reg!(si, set_si, si);
word_reg!(di, set_di, di);
word_reg!(bp, set_bp, bp);
word_reg!(sp, set_sp, sp);
word_reg!(ip, set_ip, r_ip);
word_reg!(cs, set_cs, r_cs);
word_reg!(ds, set_ds, r_ds);
word_reg!(es, set_es, r_es);
word_reg!(ss, set_ss, r_ss);

macro_rules! byte_halves {
    ($lo:ident, $set_lo:ident, $hi:ident, $set_hi:ident, $w:ident, $sw:ident) => {
        #[inline(always)]
        pub fn $lo() -> u8 {
            $w() as u8
        }
        #[inline(always)]
        pub fn $set_lo(v: u8) {
            $sw(($w() & 0xFF00) | v as u16);
        }
        #[inline(always)]
        pub fn $hi() -> u8 {
            ($w() >> 8) as u8
        }
        #[inline(always)]
        pub fn $set_hi(v: u8) {
            $sw(($w() & 0x00FF) | ((v as u16) << 8));
        }
    };
}
byte_halves!(al, set_al, ah, set_ah, ax, set_ax);
byte_halves!(bl, set_bl, bh, set_bh, bx, set_bx);
byte_halves!(cl, set_cl, ch, set_ch, cx, set_cx);
byte_halves!(dl, set_dl, dh, set_dh, dx, set_dx);

macro_rules! flag {
    ($get:ident, $set:ident, $field:ident) => {
        #[inline(always)]
        #[allow(non_snake_case)] // x86 flag names (CF/ZF/…)
        pub fn $get() -> u8 {
            unsafe { (*cpu_ptr()).flags.$field }
        }
        #[inline(always)]
        #[allow(non_snake_case)]
        pub fn $set(v: u8) {
            unsafe {
                (*cpu_ptr()).flags.$field = v;
            }
        }
    };
}
flag!(CF, set_CF, CF);
flag!(PF, set_PF, PF);
flag!(ZF, set_ZF, ZF);
flag!(SF, set_SF, SF);
flag!(OF, set_OF, OF);
flag!(IF, set_IF, IF);
flag!(DF, set_DF, DF);

#[inline(always)]
fn linear_addr(seg: u16, off: u16) -> usize {
    let addr = ((seg as u32) << 4).wrapping_add(off as u32);
    (if unsafe { a20_enabled } != 0 {
        addr & 0x1FFFFF
    } else {
        addr & 0xFFFFF
    }) as usize
}
/// `seg_off(seg, off)` — a raw pointer into virtual memory.
#[inline(always)]
pub fn seg_off(seg: u16, off: u16) -> *mut u8 {
    unsafe { virtual_memory.add(linear_addr(seg, off)) }
}
