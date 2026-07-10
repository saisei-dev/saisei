//! Port of `runtime/hw/audio.c` — OPL2 (AdLib / YM3812) FM sound card.
//!
//! Driven purely through IO ports 0x388/0x389; shims.c's inb/outb dispatcher
//! forwards those here. Only the register file + timer/busy bookkeeping are
//! modelled (games poll the timer/busy status bits — no actual FM synthesis).
//! Timing reads the runtime's scaled monotonic clock. ABI-identical to audio.h.

use crate::io_bus::{io_bus_register, IoDevice};
use core::ffi::c_char;

extern "C" {
    fn shim_scaled_monotonic_ns() -> u64;
}

/// The canonical `Opl2State` layout (was audio.h; snapshot embeds it — FROZEN).
#[repr(C)]
pub struct Opl2State {
    pub address: u8,
    pub registers: [u8; 256],
    pub status: u8,
    pub busy_until_us: u64,
    pub timer1_expire_us: u64,
    pub timer2_expire_us: u64,
}

#[no_mangle]
pub static mut opl2: Opl2State = Opl2State {
    address: 0,
    registers: [0; 256],
    status: 0,
    busy_until_us: 0,
    timer1_expire_us: 0,
    timer2_expire_us: 0,
};

const OPL2_BUSY_DURATION_US: u64 = 50;
const OPL2_TIMER1_TICK_US: u64 = 80;
const OPL2_TIMER2_TICK_US: u64 = 320;

#[inline(always)]
fn s() -> *mut Opl2State {
    core::ptr::addr_of_mut!(opl2)
}

extern "C" fn opl2_port_read(port: u16) -> u8 {
    unsafe {
        let o = &mut *s();
        if port == 0x388 {
            let now_us = shim_scaled_monotonic_ns() / 1000;
            if o.timer1_expire_us != 0 && now_us >= o.timer1_expire_us {
                o.status |= 0x80;
                o.timer1_expire_us = 0;
            }
            if o.timer2_expire_us != 0 && now_us >= o.timer2_expire_us {
                o.status |= 0x40;
                o.timer2_expire_us = 0;
            }
            let mut status = o.status;
            if now_us < o.busy_until_us {
                status |= 0x01; // busy flag
            }
            return status;
        }
        // port == 0x389
        o.registers[o.address as usize]
    }
}

extern "C" fn opl2_port_write(port: u16, value: u8) {
    unsafe {
        let o = &mut *s();
        if port == 0x388 {
            o.address = value;
            o.busy_until_us = shim_scaled_monotonic_ns() / 1000 + OPL2_BUSY_DURATION_US;
            return;
        }
        // port == 0x389
        o.registers[o.address as usize] = value;
        let now_us = shim_scaled_monotonic_ns() / 1000;
        o.busy_until_us = now_us + OPL2_BUSY_DURATION_US;
        if o.address == 0x04 {
            if value & 0x80 != 0 {
                o.status &= !0xC0; // reset timer overflow flags
                o.timer1_expire_us = 0;
                o.timer2_expire_us = 0;
            }
            if value & 0x20 != 0 {
                o.status &= !0x80; // mask timer 1 flag
            }
            if value & 0x40 != 0 {
                o.status &= !0x40; // mask timer 2 flag
            }
            if value & 0x01 != 0 {
                let timer_val = o.registers[0x02];
                let mut delay = (256 - timer_val as u64) * OPL2_TIMER1_TICK_US;
                if delay == 0 {
                    delay = OPL2_TIMER1_TICK_US;
                }
                o.timer1_expire_us = now_us + delay;
            } else {
                o.timer1_expire_us = 0;
            }
            if value & 0x02 != 0 {
                let timer_val = o.registers[0x03];
                let mut delay = (256 - timer_val as u64) * OPL2_TIMER2_TICK_US;
                if delay == 0 {
                    delay = OPL2_TIMER2_TICK_US;
                }
                o.timer2_expire_us = now_us + delay;
            } else {
                o.timer2_expire_us = 0;
            }
        } else if o.address == 0x02 {
            if o.registers[0x04] & 0x01 != 0 {
                let mut delay = (256 - value as u64) * OPL2_TIMER1_TICK_US;
                if delay == 0 {
                    delay = OPL2_TIMER1_TICK_US;
                }
                o.timer1_expire_us = now_us + delay;
            }
        } else if o.address == 0x03 {
            if o.registers[0x04] & 0x02 != 0 {
                let mut delay = (256 - value as u64) * OPL2_TIMER2_TICK_US;
                if delay == 0 {
                    delay = OPL2_TIMER2_TICK_US;
                }
                o.timer2_expire_us = now_us + delay;
            }
        }
    }
}

static OPL2_PORTS: [u16; 3] = [0x388, 0x389, 0xFFFF];
static OPL2_DEVICE: IoDevice = IoDevice {
    name: b"opl2\0".as_ptr() as *const c_char,
    ports: OPL2_PORTS.as_ptr(),
    read8: Some(opl2_port_read),
    write8: Some(opl2_port_write),
};

extern "C" fn opl2_register() {
    io_bus_register(core::ptr::addr_of!(OPL2_DEVICE));
}

// Run opl2_register at process start (the C `__attribute__((constructor))`).
#[used]
#[link_section = ".init_array"]
static OPL2_CTOR: extern "C" fn() = opl2_register;
