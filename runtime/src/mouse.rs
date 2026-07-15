//! Port of `runtime/os/mouse.c` — Microsoft-compatible mouse driver (INT 33h).
//!
//! DOS games reach the mouse only through this software-interrupt API, so this
//! IS the device model. It owns cursor position, button state, show/hide
//! nesting, the coordinate clamp window, mickey counters and the fn-0x0C user
//! event handler. Register marshalling reads AX (function) from the live CPU
//! registers and writes results back to BX/CX/DX. ABI-identical to mouse.h.

use crate::cpu::*;
use core::ffi::{c_char, c_int};

extern "C" {
    fn shim_log_stdout(fmt: *const c_char, ...);
    fn shim_invoke_far_call(
        seg: u16,
        off: u16,
        r_ax: u16,
        r_bx: u16,
        r_cx: u16,
        r_dx: u16,
        r_si: u16,
        r_di: u16,
    );
}

const MOUSE_DEF_XMIN: i16 = 0;
const MOUSE_DEF_XMAX: i16 = 639;
const MOUSE_DEF_YMIN: i16 = 0;
const MOUSE_DEF_YMAX: i16 = 199;
const MOUSE_BUTTONS: u16 = 2;
const MOUSE_EV_QUEUE: usize = 64;

#[repr(C)]
#[derive(Clone, Copy)]
struct MouseEvent {
    mask: u16,
    buttons: u16,
    x: i16,
    y: i16,
    mdx: i16,
    mdy: i16,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct MouseState {
    x: i16,
    y: i16,
    buttons: u16,
    show_count: i16,
    xmin: i16,
    xmax: i16,
    ymin: i16,
    ymax: i16,
    mickey_x: i16,
    mickey_y: i16,
    press_count: [u16; 3],
    press_x: [i16; 3],
    press_y: [i16; 3],
    release_count: [u16; 3],
    release_x: [i16; 3],
    release_y: [i16; 3],
    handler_seg: u16,
    handler_off: u16,
    handler_mask: u16,
    ev: [MouseEvent; MOUSE_EV_QUEUE],
    ev_head: i32,
    ev_tail: i32,
    ev_count: i32,
    installed: u8,
}

const ZERO_EV: MouseEvent = MouseEvent {
    mask: 0,
    buttons: 0,
    x: 0,
    y: 0,
    mdx: 0,
    mdy: 0,
};
static mut M: MouseState = MouseState {
    x: 0,
    y: 0,
    buttons: 0,
    show_count: 0,
    xmin: 0,
    xmax: 0,
    ymin: 0,
    ymax: 0,
    mickey_x: 0,
    mickey_y: 0,
    press_count: [0; 3],
    press_x: [0; 3],
    press_y: [0; 3],
    release_count: [0; 3],
    release_x: [0; 3],
    release_y: [0; 3],
    handler_seg: 0,
    handler_off: 0,
    handler_mask: 0,
    ev: [ZERO_EV; MOUSE_EV_QUEUE],
    ev_head: 0,
    ev_tail: 0,
    ev_count: 0,
    installed: 0,
};
static mut MOUSE_DELIVERING: i32 = 0;

#[inline(always)]
fn mm() -> *mut MouseState {
    core::ptr::addr_of_mut!(M)
}
const EVQ: i32 = MOUSE_EV_QUEUE as i32;

fn clamp16(v: i32, lo: i16, hi: i16) -> i16 {
    if v < lo as i32 {
        lo
    } else if v > hi as i32 {
        hi
    } else {
        v as i16
    }
}

fn mouse_queue_event(mask: u16, mdx: i16, mdy: i16) {
    unsafe {
        let m = &mut *mm();
        if m.ev_count >= EVQ {
            m.ev_head = (m.ev_head + 1) % EVQ;
            m.ev_count -= 1;
        }
        let t = m.ev_tail as usize;
        m.ev[t] = MouseEvent {
            mask,
            buttons: m.buttons,
            x: m.x,
            y: m.y,
            mdx,
            mdy,
        };
        m.ev_tail = (m.ev_tail + 1) % EVQ;
        m.ev_count += 1;
    }
}

fn mouse_reset_state() {
    unsafe {
        core::ptr::write_bytes(mm() as *mut u8, 0, core::mem::size_of::<MouseState>());
        let m = &mut *mm();
        m.xmin = MOUSE_DEF_XMIN;
        m.xmax = MOUSE_DEF_XMAX;
        m.ymin = MOUSE_DEF_YMIN;
        m.ymax = MOUSE_DEF_YMAX;
        m.x = (m.xmin + m.xmax) / 2;
        m.y = (m.ymin + m.ymax) / 2;
        m.show_count = -1; // driver starts with the cursor hidden
        m.installed = 1;
        MOUSE_DELIVERING = 0;
    }
}

/// Snapshot capture/restore (see `devices.rs`).
///
/// The whole driver is one POD struct, so its bytes *are* its state. On a real
/// machine the mouse driver is a TSR: it sits in memory across a game's save and
/// load, and the game never re-initializes it. Ours lives in the host process,
/// which a restore replaces — so without this the driver came back at its
/// power-on zeros, and those zeros are lethal twice over. `installed` is 0, so
/// the fn-0x0C callback never fires; and the clamp window is 0x0 — every
/// position clamps to (0,0), so the pointer cannot move and fn 0x03 forever
/// reports the top-left corner. The mouse is not broken after a load, it is
/// *absent*.
pub unsafe fn state_capture(out: &mut Vec<u8>) {
    crate::devices::pod_capture(&*mm(), out);
}

pub unsafe fn state_restore(b: &[u8]) -> bool {
    match crate::devices::pod_restore::<MouseState>(b) {
        Some(s) => {
            *mm() = s;
            // Not a captured field: the guard is only ever set while we are
            // inside a callback, and a fresh process is not inside one.
            MOUSE_DELIVERING = 0;
            true
        }
        None => false,
    }
}

#[no_mangle]
pub extern "C" fn mouse_host_motion(x: c_int, y: c_int, w: c_int, h: c_int) {
    unsafe {
        let m = &mut *mm();
        let nx = if w > 1 {
            clamp16(
                m.xmin as i32 + (x as i64 * (m.xmax - m.xmin) as i64 / (w - 1) as i64) as i32,
                m.xmin,
                m.xmax,
            )
        } else {
            clamp16(x, m.xmin, m.xmax)
        };
        let ny = if h > 1 {
            clamp16(
                m.ymin as i32 + (y as i64 * (m.ymax - m.ymin) as i64 / (h - 1) as i64) as i32,
                m.ymin,
                m.ymax,
            )
        } else {
            clamp16(y, m.ymin, m.ymax)
        };
        if nx != m.x || ny != m.y {
            let ddx = nx - m.x;
            let ddy = ny - m.y;
            m.mickey_x = m.mickey_x.wrapping_add(ddx);
            m.mickey_y = m.mickey_y.wrapping_add(ddy);
            m.x = nx;
            m.y = ny;
            mouse_queue_event(0x0001, ddx, ddy);
        }
    }
}

#[no_mangle]
pub extern "C" fn mouse_host_button(button: c_int, pressed: c_int) {
    if button < 0 || button > 2 {
        return;
    }
    unsafe {
        let m = &mut *mm();
        let mask = 1u16 << button;
        let b = button as usize;
        if pressed != 0 {
            if m.buttons & mask != 0 {
                return;
            }
            m.buttons |= mask;
            m.press_count[b] += 1;
            m.press_x[b] = m.x;
            m.press_y[b] = m.y;
            mouse_queue_event(1u16 << (1 + 2 * button), 0, 0);
        } else {
            if m.buttons & mask == 0 {
                return;
            }
            m.buttons &= !mask;
            m.release_count[b] += 1;
            m.release_x[b] = m.x;
            m.release_y[b] = m.y;
            mouse_queue_event(1u16 << (2 + 2 * button), 0, 0);
        }
    }
}

#[no_mangle]
pub extern "C" fn mouse_host_inject(x: i16, y: i16, buttons: u16) {
    unsafe {
        let m = &mut *mm();
        let nx = clamp16(x as i32, m.xmin, m.xmax);
        let ny = clamp16(y as i32, m.ymin, m.ymax);
        if nx != m.x || ny != m.y {
            let ddx = nx - m.x;
            let ddy = ny - m.y;
            m.mickey_x = m.mickey_x.wrapping_add(ddx);
            m.mickey_y = m.mickey_y.wrapping_add(ddy);
            m.x = nx;
            m.y = ny;
            mouse_queue_event(0x0001, ddx, ddy);
        }
    }
    for b in 0..3 {
        mouse_host_button(b, ((buttons >> b) & 1) as c_int);
    }
}

#[no_mangle]
pub extern "C" fn mouse_deliver_pending_events() {
    unsafe {
        let m = &mut *mm();
        if m.installed == 0 {
            return;
        }
        if m.handler_seg == 0 && m.handler_off == 0 {
            m.ev_head = 0;
            m.ev_tail = 0;
            m.ev_count = 0;
            return;
        }
        if MOUSE_DELIVERING != 0 {
            return;
        }
        MOUSE_DELIVERING = 1;
        while m.ev_count > 0 {
            let e = m.ev[m.ev_head as usize];
            m.ev_head = (m.ev_head + 1) % EVQ;
            m.ev_count -= 1;
            let events = e.mask & m.handler_mask;
            if events == 0 {
                continue;
            }
            shim_invoke_far_call(
                m.handler_seg,
                m.handler_off,
                events,
                e.buttons,
                e.x as u16,
                e.y as u16,
                e.mdx as u16,
                e.mdy as u16,
            );
        }
        MOUSE_DELIVERING = 0;
    }
}

#[no_mangle]
pub extern "C" fn mouse_int33_impl(file: *const c_char, func: *const c_char, line: c_int) {
    unsafe {
        shim_log(
            c"mouse_int33_impl".as_ptr(),
            file,
            func,
            line,
            core::ptr::null(),
        );
        let m = &mut *mm();
        match ax() {
            0x0000 | 0x0021 => {
                mouse_reset_state();
                set_ax(0xFFFF);
                set_bx(MOUSE_BUTTONS);
            }
            0x0001 => {
                if m.show_count < 0 {
                    m.show_count += 1;
                }
            }
            0x0002 => {
                m.show_count -= 1;
            }
            0x0003 => {
                set_bx(m.buttons);
                set_cx(m.x as u16);
                set_dx(m.y as u16);
            }
            0x0004 => {
                m.x = clamp16(cx() as i16 as i32, m.xmin, m.xmax);
                m.y = clamp16(dx() as i16 as i32, m.ymin, m.ymax);
            }
            0x0005 => {
                let b = if bx() <= 2 { bx() as usize } else { 0 };
                set_ax(m.buttons);
                set_cx(m.press_x[b] as u16);
                set_dx(m.press_y[b] as u16);
                set_bx(m.press_count[b]);
                m.press_count[b] = 0;
            }
            0x0006 => {
                let b = if bx() <= 2 { bx() as usize } else { 0 };
                set_ax(m.buttons);
                set_cx(m.release_x[b] as u16);
                set_dx(m.release_y[b] as u16);
                set_bx(m.release_count[b]);
                m.release_count[b] = 0;
            }
            0x0007 => {
                if (cx() as i16) <= (dx() as i16) {
                    m.xmin = cx() as i16;
                    m.xmax = dx() as i16;
                } else {
                    m.xmin = dx() as i16;
                    m.xmax = cx() as i16;
                }
                m.x = clamp16(m.x as i32, m.xmin, m.xmax);
            }
            0x0008 => {
                if (cx() as i16) <= (dx() as i16) {
                    m.ymin = cx() as i16;
                    m.ymax = dx() as i16;
                } else {
                    m.ymin = dx() as i16;
                    m.ymax = cx() as i16;
                }
                m.y = clamp16(m.y as i32, m.ymin, m.ymax);
            }
            0x000B => {
                set_cx(m.mickey_x as u16);
                set_dx(m.mickey_y as u16);
                m.mickey_x = 0;
                m.mickey_y = 0;
            }
            0x000C => {
                m.handler_mask = cx();
                m.handler_off = dx();
                m.handler_seg = es();
            }
            0x0014 => {
                let (old_seg, old_off, old_mask) = (m.handler_seg, m.handler_off, m.handler_mask);
                m.handler_mask = cx();
                m.handler_off = dx();
                m.handler_seg = es();
                set_cx(old_mask);
                set_dx(old_off);
                set_es(old_seg);
            }
            0x0015 => {
                set_bx(core::mem::size_of::<MouseState>() as u16);
            }
            0x0016 => {
                core::ptr::copy_nonoverlapping(
                    mm() as *const u8,
                    seg_off(es(), dx()),
                    core::mem::size_of::<MouseState>(),
                );
            }
            0x0017 => {
                core::ptr::copy_nonoverlapping(
                    seg_off(es(), dx()) as *const u8,
                    mm() as *mut u8,
                    core::mem::size_of::<MouseState>(),
                );
            }
            0x001B => {
                set_bx(50);
                set_cx(50);
                set_dx(50);
            }
            0x001E => {
                set_bx(0);
            }
            0x0024 => {
                set_bx(0x0700);
                set_cx(0x0400);
            }
            0x0009 | 0x000A | 0x000D | 0x000E | 0x000F | 0x0010 | 0x0013 | 0x001A | 0x001C
            | 0x001D => {}
            // A function this driver does not implement — including the extension
            // magic numbers a game uses to *probe* for an optional TSR (Arena does
            // `mov ax,0x53C1; int 33h; cmp ax,1` to sniff for one). A real driver
            // that lacks the extension does not fault the machine; it returns with
            // the registers untouched, so the probe reads back its own AX (≠ the
            // success value it is testing for) and the game takes its not-installed
            // path. Aborting here was an abort-on-unknown stub that turned that
            // ordinary negative probe into a crash. Left as a trace for visibility:
            // a function the game genuinely *needs* will still surface as the game
            // misbehaving, not as a dead machine.
            other => {
                shim_log_stdout(
                    c"Trace: mouse INT33 unimplemented AX=0x%04X (bx=%04X cx=%04X dx=%04X) - returning unchanged\n"
                        .as_ptr(),
                    other as core::ffi::c_uint,
                    bx() as core::ffi::c_uint,
                    cx() as core::ffi::c_uint,
                    dx() as core::ffi::c_uint,
                );
            }
        }
    }
}
