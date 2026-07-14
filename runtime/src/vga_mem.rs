//! The EGA/VGA planar memory controller: the thing that sits between a CPU
//! access to the 0xA0000 window and the four display planes.
//!
//! It is not addressable memory. A byte the CPU writes there is not the byte
//! that lands, and the byte it reads back is not the byte in any plane: writes
//! are steered by the Sequencer Map Mask, recoloured by Set/Reset, merged with
//! the four latches through an ALU and a bit mask; reads load all four latches
//! as a side effect and hand back either one selected plane or a colour compare.
//! Every EGA game drives the chip through those registers — that *is* the EGA
//! drawing idiom — so modelling the window as plain RAM (which is what we used
//! to do) means the four planes are never separately written at all. A game
//! draws its whole title screen and the display decodes near-black.
//!
//! Routing is gated on the BIOS mode being a planar one (0x0D/0x0E/0x10/0x12),
//! which is the same predicate the display decode keys on, so what the guest
//! writes and what the screen shows can never disagree about where the pixels
//! live. Mode 13h is chained and stays linear guest RAM.
//!
//! Registers (all live in `vga`, so they snapshot and port-round-trip with the
//! rest of the chip):
//!   SR2  map mask          — which planes a write lands in
//!   GR0  set/reset         — the colour written when enable-set/reset is on
//!   GR1  enable set/reset  — per plane: take the colour from GR0, not the CPU
//!   GR2  colour compare    — read mode 1 reference colour
//!   GR3  data rotate / fn  — rotate count (bits 0-2), ALU op (bits 3-4)
//!   GR4  read map select   — which plane read mode 0 returns
//!   GR5  mode              — write mode (bits 0-1), read mode (bit 3)
//!   GR7  colour don't care — planes excluded from the read mode 1 compare
//!   GR8  bit mask          — per bit: take the new value, else keep the latch
#![allow(static_mut_refs)]

use crate::video::{is_planar_graphics_mode_pub, vga};

pub const PLANE_SIZE: usize = 0x10000;
pub const VRAM_BASE: u32 = 0xA0000;
pub const VRAM_END: u32 = 0xB0000;

/// The four display planes. Every planar pixel the guest has ever drawn lives
/// here and nowhere else — the 0xA0000 page of linear guest RAM is not it.
#[no_mangle]
pub static mut vga_planes: [u8; 4 * PLANE_SIZE] = [0; 4 * PLANE_SIZE];

/// The four latches, loaded from all four planes on *every* CPU read of the
/// window. A latch-based blit (read a byte to load them, write to copy them) is
/// the standard EGA fast path, so dropping this makes copies write garbage.
#[no_mangle]
pub static mut vga_latches: [u8; 4] = [0; 4];

/// Cached `is_planar_graphics_mode(bios video mode)`, refreshed on every mode
/// set. On the memory hot path — in the runtime *and* inside every JIT chunk,
/// which binds it by name — so it is a flag rather than a call.
#[no_mangle]
pub static mut vga_planar_active: u8 = 0;

/// Back to power-on: blank planes, empty latches, no planar mode. Called from
/// `shim_boot_machine`, which is a reset — the planes are guest-readable memory
/// and would otherwise survive into the next program booted in this process.
pub unsafe fn power_on_reset() {
    libc::memset(
        core::ptr::addr_of_mut!(vga_planes) as *mut core::ffi::c_void,
        0,
        4 * PLANE_SIZE,
    );
    vga_latches = [0; 4];
    vga_planar_active = 0;
}

/// Whether the 0xA0000 window is steered through the planes right now.
///
/// Two ways in, and the second is not a mode at all: an EGA planar mode
/// (0x0D/0x0E/0x10/0x12), or **mode 13h with chain-4 switched off** — Mode X. The
/// chain is a Sequencer bit a game clears at will, long after the BIOS set the
/// mode, so this cannot be answered from the mode number alone and
/// `refresh_planar_active` has to be re-run when SR4 is written as well as when
/// the mode changes.
unsafe fn planes_are_live(mode: u8) -> bool {
    is_planar_graphics_mode_pub(mode) || crate::video::is_unchained_256(mode)
}

pub unsafe fn refresh_planar_active(mode: u8) {
    let now = planes_are_live(mode) as u8;
    if now == vga_planar_active {
        return;
    }
    vga_planar_active = now;
    // Writes are routed by the page-flag table (which the chunks' inline write
    // fast path already consults), so the window's flags must follow the mode.
    crate::shims::mem_page_flags_recompute();
}

/// The Sequencer's Memory Mode register was written, and chain-4 lives in it: the
/// window may have just become planar (or stopped being). Called from the port
/// write, because a game enters Mode X *while already in* mode 13h — nothing else
/// would notice.
pub unsafe fn sequencer_memory_mode_written() {
    refresh_planar_active(crate::video::current_video_mode());
}

/// Is this linear address steered through the planes right now?
#[inline(always)]
pub unsafe fn routed(addr: u32) -> bool {
    vga_planar_active != 0 && addr >= VRAM_BASE && addr < VRAM_END
}

/// Does a block access of `len` bytes from `lo` touch the planar window? Used by
/// the rep-string block shims to drop to the per-byte path, which routes.
#[inline]
pub unsafe fn range_routed(lo: u32, len: u32) -> bool {
    vga_planar_active != 0 && lo < VRAM_END && lo.saturating_add(len) > VRAM_BASE
}

#[inline]
unsafe fn plane(p: usize, off: usize) -> *mut u8 {
    vga_planes
        .as_mut_ptr()
        .add(p * PLANE_SIZE + (off & (PLANE_SIZE - 1)))
}

#[inline]
fn alu(func: u8, value: u8, latch: u8) -> u8 {
    match func & 3 {
        1 => value & latch,
        2 => value | latch,
        3 => value ^ latch,
        _ => value,
    }
}

/// A CPU read of the planar window. Loads the latches from all four planes, then
/// returns what the current read mode says the CPU sees.
pub unsafe fn read(addr: u32) -> u8 {
    let off = (addr - VRAM_BASE) as usize;
    let gr = &vga.graphics_regs;

    for p in 0..4 {
        vga_latches[p] = *plane(p, off);
    }

    if gr[5] & 0x08 == 0 {
        // Read mode 0: one plane, chosen by Read Map Select.
        vga_latches[(gr[4] & 3) as usize]
    } else {
        // Read mode 1: each result bit says "at this pixel, do the planes I care
        // about all match the compare colour?"
        let compare = gr[2] & 0x0F;
        let care = gr[7] & 0x0F;
        let mut out = 0u8;
        for bit in 0..8 {
            let mask = 0x80u8 >> bit;
            let mut pixel = 0u8;
            for p in 0..4 {
                if vga_latches[p] & mask != 0 {
                    pixel |= 1 << p;
                }
            }
            if (pixel & care) == (compare & care) {
                out |= mask;
            }
        }
        out
    }
}

/// A CPU write to the planar window, through the write mode the guest selected.
pub unsafe fn write(addr: u32, value: u8) {
    let off = (addr - VRAM_BASE) as usize;
    let gr = &vga.graphics_regs;
    let map_mask = vga.sequencer_regs[2] & 0x0F;

    let write_mode = gr[5] & 0x03;
    let func = (gr[3] >> 3) & 0x03;
    let rotate = gr[3] & 0x07;
    let bit_mask = gr[8];
    let set_reset = gr[0] & 0x0F;
    let enable_sr = gr[1] & 0x0F;

    // Write mode 1 copies the latches straight through — no ALU, no bit mask.
    if write_mode == 1 {
        for p in 0..4 {
            if map_mask & (1 << p) != 0 {
                *plane(p, off) = vga_latches[p];
            }
        }
        return;
    }

    // Write mode 3: the CPU byte is the bit mask (ANDed with GR8), and the
    // colour always comes from Set/Reset.
    if write_mode == 3 {
        let rotated = value.rotate_right(rotate as u32);
        let mask = rotated & bit_mask;
        for p in 0..4 {
            if map_mask & (1 << p) == 0 {
                continue;
            }
            let color = if set_reset & (1 << p) != 0 {
                0xFF
            } else {
                0x00
            };
            *plane(p, off) = (color & mask) | (vga_latches[p] & !mask);
        }
        return;
    }

    for p in 0..4 {
        if map_mask & (1 << p) == 0 {
            continue;
        }
        let src = if write_mode == 2 {
            // Write mode 2: the low nibble of the CPU byte *is* the colour.
            if value & (1 << p) != 0 {
                0xFF
            } else {
                0x00
            }
        } else if enable_sr & (1 << p) != 0 {
            // Write mode 0 with Set/Reset enabled for this plane.
            if set_reset & (1 << p) != 0 {
                0xFF
            } else {
                0x00
            }
        } else {
            value.rotate_right(rotate as u32)
        };
        let merged = alu(func, src, vga_latches[p]);
        *plane(p, off) = (merged & bit_mask) | (vga_latches[p] & !bit_mask);
    }
}

/// Read a plane byte for the display decode. The renderer sees the planes, not
/// the CPU's view of them, so it must not go through `read` (which would clobber
/// the latches out from under the guest).
#[inline]
pub unsafe fn plane_byte(p: usize, off: usize) -> u8 {
    *plane(p, off)
}

// ---- snapshot (devices.bin "VGAM") -----------------------------------------
//
// The planes are guest RAM in every sense that matters — they are just not in
// the linear address space, so the snapshot's memory image does not carry them.
// Without this block a restored EGA game comes back to a black screen it never
// cleared. The latches go with them (a blit in flight reads them), and so does
// the Sequencer register file, which is a guest-programmable register like any
// other: the game writes the Map Mask and reads it back.
//
// `vga_planar_active` is deliberately NOT captured: it is derived, and
// `post_restore` re-derives it from the restored BIOS video mode (rule 2).

#[repr(C)]
#[derive(Clone, Copy)]
struct VgaMemSnap {
    planes: [u8; 4 * PLANE_SIZE],
    latches: [u8; 4],
    sequencer_index: u8,
    sequencer_regs: [u8; 8],
}

pub unsafe fn state_capture(out: &mut Vec<u8>) {
    let mut s: VgaMemSnap = core::mem::zeroed();
    s.planes = vga_planes;
    s.latches = vga_latches;
    s.sequencer_index = vga.sequencer_index;
    s.sequencer_regs = vga.sequencer_regs;
    crate::devices::pod_capture(&s, out);
}

pub unsafe fn state_restore(b: &[u8]) -> bool {
    let s: VgaMemSnap = match crate::devices::pod_restore(b) {
        Some(s) => s,
        None => return false,
    };
    vga_planes = s.planes;
    vga_latches = s.latches;
    vga.sequencer_index = s.sequencer_index;
    vga.sequencer_regs = s.sequencer_regs;
    true
}

/// Re-derive the routing flag from the mode the guest is actually in. A restore
/// is a fresh process, so this static is back at its power-on zero — and a game
/// restored mid-EGA would have its writes going to linear RAM while the display
/// read the planes.
pub unsafe fn post_restore(mode: u8) {
    vga_planar_active = is_planar_graphics_mode_pub(mode) as u8;
    crate::shims::mem_page_flags_recompute();
}
