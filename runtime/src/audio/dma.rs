//! The 8237A DMA controller.
//!
//! It lives under `audio/` because sound is the only thing in this machine that
//! actually uses it — a Sound Blaster plays digitised audio by handing the DMA
//! controller a block of memory and letting it feed the card a byte at a time,
//! with the CPU uninvolved. That is the whole point of the part, and it is why
//! digitised sound was possible at all on a 286.
//!
//! What was here before was two bytes: channel 3's address latch, and a
//! flip-flop. Every other DMA port fell through to `io_port_error`, which calls
//! `exit(1)` — so a game that tried to play a sample did not play it badly, it
//! took the process down.
//!
//! The model is a *pull*: nothing is transferred on a schedule, because there is
//! no bus to arbitrate. The card asks for its next byte when it needs one (see
//! `sb.rs`), and the controller answers out of the channel's current address,
//! advancing it and counting down exactly as the hardware would. Auto-init
//! reloads from the base registers at terminal count, which is what lets a game
//! queue one buffer and get continuous playback out of it.

use crate::io_bus::{io_bus_register, IoDevice};
use core::ffi::c_char;

const CHANNELS: usize = 4;

#[derive(Clone, Copy, Default)]
pub struct Channel {
    base_addr: u16,
    base_count: u16,
    cur_addr: u16,
    cur_count: u16,
    /// The high 8 bits of the physical address: DMA on this machine cannot cross
    /// a 64K boundary, and the page register is why.
    page: u8,
    mode: u8,
    masked: bool,
    /// Terminal count reached since the last status read.
    tc: bool,
}

impl Channel {
    fn auto_init(&self) -> bool {
        self.mode & 0x10 != 0
    }
    /// Bit 5: address decrements rather than increments. Rare, but free.
    fn descending(&self) -> bool {
        self.mode & 0x20 != 0
    }
    fn physical(&self) -> u32 {
        ((self.page as u32) << 16) | self.cur_addr as u32
    }
}

pub struct Dma {
    ch: [Channel; CHANNELS],
    /// The address/count registers are 16-bit but the bus is 8 — so every one of
    /// them is written low byte then high byte, and this bit says which is next.
    /// It is shared by all four channels, which is exactly why a driver clears it
    /// (port 0x0C) before it starts.
    flip: bool,
}

static mut DMA: Option<Dma> = None;

// ---- snapshot block (see devices.rs) ---------------------------------------
//
// A Sound Blaster playing a sample is a DMA transfer in flight: the channel's
// *current* address and count are how far through the buffer the card has got.
// Lose them and a save taken during a digitised sound comes back with the
// channel masked at power-on — the transfer does not resume, and (worse) an
// auto-init block that the game expects to keep looping simply stops.

#[repr(C)]
#[derive(Clone, Copy)]
struct ChannelSnap {
    base_addr: u16,
    base_count: u16,
    cur_addr: u16,
    cur_count: u16,
    page: u8,
    mode: u8,
    masked: u8,
    tc: u8,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct DmaSnap {
    ch: [ChannelSnap; CHANNELS],
    flip: u8,
}

pub(crate) unsafe fn state_capture(out: &mut Vec<u8>) {
    let d = dma();
    // Zeroed, not a struct literal: `pod_capture` copies the struct's bytes and
    // a literal leaves any padding undefined, so two captures of the same chip
    // would not compare equal. See devices::pod_capture.
    let mut s: DmaSnap = core::mem::zeroed();
    s.flip = d.flip as u8;
    for i in 0..CHANNELS {
        let c = &d.ch[i];
        s.ch[i].base_addr = c.base_addr;
        s.ch[i].base_count = c.base_count;
        s.ch[i].cur_addr = c.cur_addr;
        s.ch[i].cur_count = c.cur_count;
        s.ch[i].page = c.page;
        s.ch[i].mode = c.mode;
        s.ch[i].masked = c.masked as u8;
        s.ch[i].tc = c.tc as u8;
    }
    crate::devices::pod_capture(&s, out);
}

pub(crate) unsafe fn state_restore(b: &[u8]) -> bool {
    let s = match crate::devices::pod_restore::<DmaSnap>(b) {
        Some(s) => s,
        None => return false,
    };
    reset();
    let d = dma();
    d.flip = s.flip != 0;
    for i in 0..CHANNELS {
        let c = &s.ch[i];
        d.ch[i] = Channel {
            base_addr: c.base_addr,
            base_count: c.base_count,
            cur_addr: c.cur_addr,
            cur_count: c.cur_count,
            page: c.page,
            mode: c.mode,
            masked: c.masked != 0,
            tc: c.tc != 0,
        };
    }
    true
}

unsafe fn dma() -> &'static mut Dma {
    if (*core::ptr::addr_of!(DMA)).is_none() {
        reset();
    }
    (*core::ptr::addr_of_mut!(DMA)).as_mut().unwrap()
}

pub unsafe fn reset() {
    DMA = Some(Dma {
        // Masked at power-on: a channel that transferred before anyone programmed
        // it would be reading whatever happened to be in memory.
        ch: [Channel {
            masked: true,
            ..Default::default()
        }; CHANNELS],
        flip: false,
    });
}

/// Take the next byte of channel `n`, as the sound card does when it needs one.
///
/// Returns the byte and whether this was the last of the block. `None` means the
/// channel is masked or was never programmed — there is nothing to play.
pub unsafe fn pull(n: usize) -> Option<(u8, bool)> {
    let d = dma();
    let c = &mut d.ch[n];
    if c.masked {
        return None;
    }
    let byte = crate::shims::phys_read_byte(c.physical());

    if c.descending() {
        c.cur_addr = c.cur_addr.wrapping_sub(1);
    } else {
        c.cur_addr = c.cur_addr.wrapping_add(1);
    }

    // The count register holds "length - 1", so terminal count is the borrow out
    // of zero — not a compare against it.
    let (next, borrowed) = c.cur_count.overflowing_sub(1);
    c.cur_count = next;
    if !borrowed {
        return Some((byte, false));
    }

    c.tc = true;
    if c.auto_init() {
        // Reload and keep going: this is how one buffer becomes continuous sound.
        c.cur_addr = c.base_addr;
        c.cur_count = c.base_count;
    } else {
        c.masked = true;
    }
    Some((byte, true))
}

/// True if the channel has been programmed and armed — i.e. there is a block to
/// play. The card asks before it starts pulling.
pub unsafe fn armed(n: usize) -> bool {
    !dma().ch[n].masked
}

// ---- the ports ---------------------------------------------------------------

/// Page registers. Not contiguous, and not in channel order — an accident of the
/// PC/AT's wiring that every DMA driver has hard-coded ever since.
fn page_port_channel(port: u16) -> Option<usize> {
    match port {
        0x87 => Some(0),
        0x83 => Some(1),
        0x81 => Some(2),
        0x82 => Some(3),
        _ => None,
    }
}

extern "C" fn dma_write(port: u16, value: u8) {
    unsafe {
        let d = dma();
        if let Some(n) = page_port_channel(port) {
            d.ch[n].page = value;
            return;
        }
        match port {
            // Address and count, low byte then high, per the shared flip-flop.
            0x00..=0x07 => {
                let n = (port >> 1) as usize;
                let is_count = port & 1 != 0;
                let c = &mut d.ch[n];
                let reg = if is_count {
                    &mut c.base_count
                } else {
                    &mut c.base_addr
                };
                if d.flip {
                    *reg = (*reg & 0x00FF) | ((value as u16) << 8);
                } else {
                    *reg = (*reg & 0xFF00) | value as u16;
                }
                d.flip = !d.flip;
                // Writing a base register also loads the current one: the channel
                // is programmed and ready, not merely described.
                if is_count {
                    c.cur_count = c.base_count;
                } else {
                    c.cur_addr = c.base_addr;
                }
            }
            0x08 => {} // command register: nothing we model differs on it
            0x09 => {} // software DRQ
            0x0A => {
                // Single mask bit: bit 2 sets, bits 0-1 pick the channel.
                let n = (value & 0x03) as usize;
                d.ch[n].masked = value & 0x04 != 0;
            }
            0x0B => {
                let n = (value & 0x03) as usize;
                d.ch[n].mode = value;
            }
            0x0C => d.flip = false,
            0x0D => {
                // Master clear: everything back to power-on.
                let pages = [d.ch[0].page, d.ch[1].page, d.ch[2].page, d.ch[3].page];
                reset();
                let d = dma();
                for (n, p) in pages.iter().enumerate() {
                    d.ch[n].page = *p;
                }
            }
            0x0E => {
                for c in d.ch.iter_mut() {
                    c.masked = false;
                }
            }
            0x0F => {
                for (n, c) in d.ch.iter_mut().enumerate() {
                    c.masked = value & (1 << n) != 0;
                }
            }
            _ => {}
        }
    }
}

extern "C" fn dma_read(port: u16) -> u8 {
    unsafe {
        let d = dma();
        if let Some(n) = page_port_channel(port) {
            return d.ch[n].page;
        }
        match port {
            0x00..=0x07 => {
                let n = (port >> 1) as usize;
                let c = &mut d.ch[n];
                let v = if port & 1 != 0 {
                    c.cur_count
                } else {
                    c.cur_addr
                };
                let byte = if d.flip {
                    (v >> 8) as u8
                } else {
                    (v & 0xFF) as u8
                };
                d.flip = !d.flip;
                byte
            }
            0x08 => {
                // Status: low nibble is terminal-count-since-last-read (and
                // reading it clears them), high nibble is which channels are
                // requesting. We have no bus requests to report.
                let mut v = 0u8;
                for (n, c) in d.ch.iter_mut().enumerate() {
                    if c.tc {
                        v |= 1 << n;
                        c.tc = false;
                    }
                }
                v
            }
            _ => 0xFF,
        }
    }
}

static DMA_PORTS: [u16; 15] = [
    0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A, 0x0B, 0x0C, 0x0D, 0xFFFF,
];
static DMA_DEVICE: IoDevice = IoDevice {
    name: b"dma8237\0".as_ptr() as *const c_char,
    ports: DMA_PORTS.as_ptr(),
    read8: Some(dma_read),
    write8: Some(dma_write),
};

// 0x0E/0x0F are write-only mask registers, and the page registers are scattered;
// a second device keeps the port list honest rather than pretending they are one
// contiguous block.
static DMA_PAGE_PORTS: [u16; 7] = [0x0E, 0x0F, 0x81, 0x82, 0x83, 0x87, 0xFFFF];
static DMA_PAGE_DEVICE: IoDevice = IoDevice {
    name: b"dma8237-page\0".as_ptr() as *const c_char,
    ports: DMA_PAGE_PORTS.as_ptr(),
    read8: Some(dma_read),
    write8: Some(dma_write),
};

/// Program the controller as a driver would, for tests that need a channel armed.
#[cfg(test)]
pub unsafe fn test_write(port: u16, value: u8) {
    dma_write(port, value)
}

extern "C" fn dma_register() {
    io_bus_register(core::ptr::addr_of!(DMA_DEVICE));
    io_bus_register(core::ptr::addr_of!(DMA_PAGE_DEVICE));
}

#[used]
#[link_section = ".init_array"]
static DMA_CTOR: extern "C" fn() = dma_register;
