//! Device state persistence — the hardware half of a snapshot.
//!
//! A restore is a **fresh process** (`save_manager` re-execs with
//! `--restore-from`), so every host-side `static mut` comes back at its
//! power-on initializer. Guest RAM and the CPU are restored from the bundle;
//! anything the guest programmed into a *device* is restored from here, or not
//! at all. That gap is what this file exists to close: the INT 33h mouse driver,
//! the 8259A's mask, the PIT's channel-2 divisor, the Tandy's tone registers and
//! the Sound Blaster's mixer were all simply dropped on the floor by a load, and
//! the game came back to hardware it had never programmed.
//!
//! Two rules, and the round-trip test at the bottom of this file enforces both:
//!
//! 1. **Every guest-programmable register is captured.** If a game can write it
//!    and later read it back — or *hear* it — it belongs in a block here.
//! 2. **Every derived cache is re-derived from those registers on restore.**
//!    A device may keep a cooked, render-side copy of its state (the OPL2's FM
//!    `Synth`, the speaker's segment classifier). Those are built up by the
//!    *writes*, not by the register file, so restoring the bytes alone leaves
//!    them at ctor defaults: the guest reads back a correctly-programmed chip
//!    while the one it is actually heard through was never programmed at all.
//!    Restoring a device therefore means replaying its registers into whatever
//!    it derives from them — see `post_restore`.
//!
//! The bundle file is `devices.bin`, a tagged container:
//!
//! ```text
//! magic:u32  version:u32  count:u32
//! [ tag:[u8;4]  len:u32  payload:[u8; len] ] * count
//! ```
//!
//! Tagged, so it can be evolved without the all-or-nothing versioning that makes
//! `ShimRuntimeState` unextendable in practice: an unknown tag is skipped, a
//! missing tag leaves that device at power-on and says so. A bundle written
//! before this file existed has no `devices.bin` at all, and still loads.

use core::ffi::c_void;
use core::mem::size_of;

use crate::audio;
use crate::dos;
use crate::mouse;
use crate::shims;

const DEVICES_MAGIC: u32 = 0x5341_4456; // "SADV"
const DEVICES_VERSION: u32 = 1;

/// One device's guest state. `capture` appends its payload; `restore` takes the
/// payload back and returns false if it could not use it (a length change from a
/// different build — refuse rather than reinterpret bytes as the wrong struct).
pub struct DeviceBlock {
    pub tag: [u8; 4],
    pub capture: unsafe fn(&mut Vec<u8>),
    pub restore: unsafe fn(&[u8]) -> bool,
}

/// The devices whose state a snapshot carries.
///
/// This is the list to extend when a new device is added — but forgetting to is
/// not left to anyone's memory. `every_io_bus_device_has_a_snapshot_block` fails
/// for a device on the bus that has no block here, and
/// `hardware_state_survives_a_power_cycle` fails if a device's state can be seen
/// to change across a save/restore. Both are in `devices/tests.rs`.
pub static DEVICE_BLOCKS: &[DeviceBlock] = &[
    DeviceBlock {
        tag: *b"MOUS",
        capture: mouse::state_capture,
        restore: mouse::state_restore,
    },
    DeviceBlock {
        tag: *b"PIC8",
        capture: shims::pic_state_capture,
        restore: shims::pic_state_restore,
    },
    DeviceBlock {
        tag: *b"PORT",
        capture: shims::port_state_capture,
        restore: shims::port_state_restore,
    },
    DeviceBlock {
        tag: *b"PIT2",
        capture: shims::pit_aux_state_capture,
        restore: shims::pit_aux_state_restore,
    },
    DeviceBlock {
        tag: *b"DOSS",
        capture: dos::state_capture,
        restore: dos::state_restore,
    },
    DeviceBlock {
        tag: *b"SN76",
        capture: audio::sn76489::state_capture,
        restore: audio::sn76489::state_restore,
    },
    DeviceBlock {
        tag: *b"SBLA",
        capture: audio::sb::state_capture,
        restore: audio::sb::state_restore,
    },
    DeviceBlock {
        tag: *b"DMA8",
        capture: audio::dma::state_capture,
        restore: audio::dma::state_restore,
    },
];

// ---------------------------------------------------------------------------
// POD helpers. Every block payload is a `#[repr(C)]` plain-old-data struct; the
// length is written alongside it, so a struct that changed shape is refused on
// restore rather than being reinterpreted.
// ---------------------------------------------------------------------------

/// Append `v`'s bytes to the block payload.
///
/// **Build the value from `core::mem::zeroed()` and assign its fields — never
/// from a struct literal.** This copies the struct's raw bytes, *including any
/// padding* `#[repr(C)]` inserted between fields, and a struct literal leaves
/// that padding undefined. Two captures of an identical device would then
/// differ in the padding alone, and a save would carry stack garbage. Assigning
/// into a zeroed value writes only the fields and leaves the padding zero.
/// (`hardware_state_survives_a_power_cycle` catches this: it re-captures after a
/// restore and requires the bytes to match.)
pub(crate) unsafe fn pod_capture<T: Copy>(v: &T, out: &mut Vec<u8>) {
    let bytes = core::slice::from_raw_parts(v as *const T as *const u8, size_of::<T>());
    out.extend_from_slice(bytes);
}

pub(crate) unsafe fn pod_restore<T: Copy>(b: &[u8]) -> Option<T> {
    if b.len() != size_of::<T>() {
        return None;
    }
    let mut v: T = core::mem::zeroed();
    core::ptr::copy_nonoverlapping(b.as_ptr(), &mut v as *mut T as *mut u8, size_of::<T>());
    Some(v)
}

// ---------------------------------------------------------------------------
// Container
// ---------------------------------------------------------------------------

/// Serialize every device's state into the tagged container.
pub unsafe fn capture() -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&DEVICES_MAGIC.to_le_bytes());
    out.extend_from_slice(&DEVICES_VERSION.to_le_bytes());
    out.extend_from_slice(&(DEVICE_BLOCKS.len() as u32).to_le_bytes());
    for blk in DEVICE_BLOCKS {
        let mut payload = Vec::new();
        (blk.capture)(&mut payload);
        out.extend_from_slice(&blk.tag);
        out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        out.extend_from_slice(&payload);
    }
    out
}

/// Restore every device from the container, then re-derive the caches that are
/// built from writes rather than from the register file (rule 2).
///
/// Returns false only if the container itself is unusable. A block that is
/// missing, unknown or the wrong size leaves *that* device at power-on and warns
/// — one stale device should not cost the player the whole save.
pub unsafe fn restore(buf: &[u8]) -> bool {
    if buf.len() < 12 {
        warn("devices.bin: truncated header");
        return false;
    }
    let magic = u32::from_le_bytes(buf[0..4].try_into().unwrap());
    let version = u32::from_le_bytes(buf[4..8].try_into().unwrap());
    if magic != DEVICES_MAGIC {
        warn("devices.bin: bad magic");
        return false;
    }
    if version != DEVICES_VERSION {
        warn("devices.bin: unknown version — devices left at power-on");
        return false;
    }
    let count = u32::from_le_bytes(buf[8..12].try_into().unwrap()) as usize;

    let mut seen = [false; 64];
    let mut off = 12usize;
    for _ in 0..count {
        if off + 8 > buf.len() {
            warn("devices.bin: truncated block header");
            break;
        }
        let tag: [u8; 4] = buf[off..off + 4].try_into().unwrap();
        let len = u32::from_le_bytes(buf[off + 4..off + 8].try_into().unwrap()) as usize;
        off += 8;
        if off + len > buf.len() {
            warn("devices.bin: truncated block payload");
            break;
        }
        let payload = &buf[off..off + len];
        off += len;

        match DEVICE_BLOCKS.iter().position(|b| b.tag == tag) {
            Some(i) => {
                if (DEVICE_BLOCKS[i].restore)(payload) {
                    seen[i] = true;
                } else {
                    warn_tag("devices.bin: block refused (size changed?)", &tag);
                }
            }
            // A bundle from a build that knew about a device this one does not.
            None => warn_tag("devices.bin: unknown block, skipped", &tag),
        }
    }
    for (i, blk) in DEVICE_BLOCKS.iter().enumerate() {
        if !seen[i] {
            warn_tag("devices.bin: no block — device left at power-on", &blk.tag);
        }
    }

    post_restore();
    true
}

/// Rule 2: rebuild everything that is *derived* from the register files we just
/// restored. These caches are normally built up a write at a time as the guest
/// programs the chip; a restore hands them the finished register file instead,
/// so each one has to be re-derived from it explicitly.
pub unsafe fn post_restore() {
    // The FM synth is built by `synth_write` on each OPL2 register write. The
    // register file is restored by ShimRuntimeState, but nothing ever replayed
    // it into the synth — so the game heard a chip it had never programmed.
    audio::opl2::resync_synth_from_registers();
    // The speaker's segment classifier is a pure function of port 61h and PIT
    // channel 2, both of which are only now back. Its own doc comment always
    // said it was to be called "after a snapshot restore"; nothing called it.
    audio::speaker::reset();
}

fn warn(msg: &str) {
    eprintln!("restore: {msg}");
}

fn warn_tag(msg: &str, tag: &[u8; 4]) {
    eprintln!("restore: {msg} [{}]", String::from_utf8_lossy(tag));
}

// ---------------------------------------------------------------------------
// File IO (called from snapshot.rs, which owns the bundle directory).
// ---------------------------------------------------------------------------

/// Write `devices.bin` into a bundle directory. Returns false on IO failure.
pub unsafe fn write_to_bundle(dir: *const core::ffi::c_char) -> bool {
    let dir = match cstr_to_string(dir) {
        Some(d) => d,
        None => return false,
    };
    let path = format!("{dir}/devices.bin");
    std::fs::write(&path, capture()).is_ok()
}

/// Read `devices.bin` back from a bundle directory.
///
/// A bundle written before this existed simply has no such file. That is not an
/// error — it is every save the player already had — but the hardware in it is
/// lost, so say so once, plainly, rather than letting the mouse quietly not work.
pub unsafe fn read_from_bundle(dir: *const core::ffi::c_char) -> bool {
    let dir = match cstr_to_string(dir) {
        Some(d) => d,
        None => return false,
    };
    let path = format!("{dir}/devices.bin");
    match std::fs::read(&path) {
        Ok(buf) => restore(&buf),
        Err(_) => {
            warn(
                "no devices.bin in bundle — mouse, PIC, PIT and sound devices \
                 will come back at power-on defaults. Re-save with this build \
                 for a save that restores them.",
            );
            // Still re-derive: the OPL2 register file *is* in ShimRuntimeState,
            // so an old bundle can at least come back with its FM voices.
            post_restore();
            false
        }
    }
}

unsafe fn cstr_to_string(p: *const core::ffi::c_char) -> Option<String> {
    if p.is_null() {
        return None;
    }
    core::ffi::CStr::from_ptr(p)
        .to_str()
        .ok()
        .map(str::to_owned)
}

/// The guest memory base, for turning host pointers (the DOS DTA) into the
/// linear offsets a snapshot can actually carry.
pub(crate) unsafe fn mem_base() -> *mut u8 {
    shims::virtual_memory
}

/// A host pointer into guest RAM → its linear address. `u32::MAX` means null.
pub(crate) unsafe fn ptr_to_linear(p: *mut c_void) -> u32 {
    let base = mem_base();
    if p.is_null() || base.is_null() {
        return u32::MAX;
    }
    ((p as usize) - (base as usize)) as u32
}

/// The inverse. Out-of-range (or the null marker) restores as null.
pub(crate) unsafe fn linear_to_ptr(lin: u32) -> *mut c_void {
    let base = mem_base();
    if lin == u32::MAX || base.is_null() || lin as usize >= shims::SHIM_MEMORY_SIZE {
        return core::ptr::null_mut();
    }
    base.add(lin as usize) as *mut c_void
}

#[cfg(test)]
mod tests;
