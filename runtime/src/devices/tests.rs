//! The guardrail for device-state persistence.
//!
//! The bug these exist to prevent is not "the mouse stopped working". It is the
//! *class*: a device gets modelled, its state lives in a `static mut`, and
//! nobody remembers that a restore is a fresh process which will throw that
//! static away. It happened to the mouse, the 8259A, PIT channels 1 and 2, the
//! Tandy chip, the Sound Blaster and the DMA controller — every one of them
//! silently, because nothing failed, the hardware just came back blank.
//!
//! So these tests do not check any particular device. They check the property:
//!
//! * `hardware_state_survives_a_power_cycle` — program the machine, save,
//!   *scribble over every device*, restore, and require that both the captured
//!   bytes and what the guest can actually read back through its own ports come
//!   back identical. A device whose state is not in a block fails this: the
//!   scribble survives the restore, because nothing overwrote it.
//! * `every_io_bus_device_has_a_snapshot_block` — the structural half. A new
//!   sound card registers itself on the bus; this makes it impossible to land
//!   one without either giving it a block or writing down why it needs none.
//! * `opl2_sounds_the_same_after_a_restore` — rule 2. The OPL2's registers were
//!   always saved, and the chip still came back wrong, because the thing you
//!   *hear* is derived from those registers by the writes and nobody replayed
//!   them. Bytes matching is not the bar; the audio matching is.

use super::*;
use crate::cpu::{bx, cx, dx, es, set_ax, set_bx, set_cx, set_dx, set_es};
use crate::mouse::mouse_int33_impl;
use crate::shims::{inb, outb};
use std::sync::{Mutex, MutexGuard};

/// Device state is global — the same globals the guest drives — so these tests
/// must not run beside the per-chip tests that also drive them. Each chip module
/// owns a lock for its own tests; take all of them, always in this order, so no
/// cycle can form (every other test holds exactly one).
static DEVICES_LOCK: Mutex<()> = Mutex::new(());

struct Claim(
    #[allow(dead_code)] MutexGuard<'static, ()>,
    #[allow(dead_code)] MutexGuard<'static, ()>,
    #[allow(dead_code)] MutexGuard<'static, ()>,
    #[allow(dead_code)] MutexGuard<'static, ()>,
);

fn claim_machine() -> Claim {
    let a = DEVICES_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let b = crate::audio::opl2::tests::claim_chip();
    let c = crate::audio::sn76489::tests::claim();
    let d = crate::audio::sb::tests::claim();
    // A restore re-applies the video mode, which would otherwise reach for a
    // window that a unit test does not have.
    unsafe { crate::shims::headless_mode = 1 };
    Claim(a, b, c, d)
}

/// A whole machine snapshot, in the order `snapshot.rs` writes and reads one.
///
/// Device state is split across two files on disk — `ShimRuntimeState` carries
/// the video registers, PIT channel 0 and (the reason it matters here) the OPL2
/// register file, while `devices.bin` carries everything else. A test that
/// round-tripped only one of them would be testing a restore that never happens.
struct MachineSnap {
    rt: crate::shims::ShimRuntimeState,
    devices: Vec<u8>,
}

unsafe fn capture_machine() -> MachineSnap {
    let mut rt: crate::shims::ShimRuntimeState = core::mem::zeroed();
    crate::shims::shim_runtime_state_capture(&mut rt);
    MachineSnap {
        rt,
        devices: capture(),
    }
}

/// Split a container back into its blocks, so a failure can name the device that
/// broke rather than leaving whoever hits it to diff two anonymous byte strings.
fn split_blocks(buf: &[u8]) -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    let mut off = 12usize;
    while off + 8 <= buf.len() {
        let tag = String::from_utf8_lossy(&buf[off..off + 4]).into_owned();
        let len = u32::from_le_bytes(buf[off + 4..off + 8].try_into().unwrap()) as usize;
        off += 8;
        if off + len > buf.len() {
            break;
        }
        out.push((tag, buf[off..off + len].to_vec()));
        off += len;
    }
    out
}

/// The blocks that differ, by tag.
fn differing_blocks(a: &[u8], b: &[u8]) -> Vec<String> {
    let (a, b) = (split_blocks(a), split_blocks(b));
    a.iter()
        .zip(b.iter())
        .filter(|((_, x), (_, y))| x != y)
        .map(|((t, _), _)| t.clone())
        .collect()
}

unsafe fn restore_machine(s: &MachineSnap) {
    // Order is load-bearing and mirrors snapshot.rs: the OPL2's registers arrive
    // with the shim state, and `devices::restore` is what replays them into the
    // synth. Swap these and the synth is rebuilt from the *old* register file.
    assert_eq!(
        crate::shims::shim_runtime_state_restore(&s.rt),
        0,
        "shim_runtime_state_restore refused its own capture"
    );
    assert!(
        restore(&s.devices),
        "devices::restore refused its own capture"
    );
}

// ---------------------------------------------------------------------------
// Driving the machine exactly as a guest does: through ports and INT 33h. Never
// by poking the statics — a test that reaches behind the interface would pass
// even if the interface were the broken part.
// ---------------------------------------------------------------------------

unsafe fn fm(reg: u8, val: u8) {
    outb(0x388, reg);
    outb(0x389, val);
}

unsafe fn int33(func: u16) {
    set_ax(func);
    mouse_int33_impl(
        c"devices/tests.rs".as_ptr(),
        c"int33".as_ptr(),
        line!() as i32,
    );
}

/// Program every device we persist. Two different scripts, so that "restore put
/// it back" can be told apart from "it never changed".
unsafe fn program_machine(variant: u8) {
    let v = variant;

    // ---- 8259A: the interrupt mask a game chooses is *the* thing a restore was
    // handing back at power-on. Unmask a different set in each variant.
    outb(0x21, if v == 0 { 0xB8 } else { 0x3C });
    outb(0xA1, if v == 0 { 0xFF } else { 0x0F });

    // ---- PIT channel 2 (the speaker's pitch) + port 61h (its gate).
    outb(0x43, 0xB6); // ch2, lo/hi, mode 3, binary
    let div: u16 = if v == 0 { 0x04AA } else { 0x0912 };
    outb(0x42, (div & 0xFF) as u8);
    outb(0x42, (div >> 8) as u8);
    outb(0x61, if v == 0 { 0x03 } else { 0x00 });

    // ---- A20.
    outb(0x92, if v == 0 { 0x02 } else { 0x00 });

    // ---- OPL2: an instrument patch and a keyed note. This is the state a music
    // driver sets once and never rewrites, so losing it loses the timbre.
    fm(0x01, 0x20); // waveform select enable
    fm(0x20, if v == 0 { 0x01 } else { 0x21 }); // op0 mult / tremolo
    fm(0x40, if v == 0 { 0x10 } else { 0x2A }); // op0 level
    fm(0x60, if v == 0 { 0xF0 } else { 0x84 }); // op0 attack/decay
    fm(0x80, if v == 0 { 0x77 } else { 0x31 }); // op0 sustain/release
    fm(0x23, 0x01);
    fm(0x43, 0x00);
    fm(0x63, if v == 0 { 0xF0 } else { 0x95 });
    fm(0x83, if v == 0 { 0x77 } else { 0x22 });
    fm(0xE0, if v == 0 { 0x02 } else { 0x00 }); // waveform
    fm(0xA0, if v == 0 { 0x81 } else { 0x40 }); // F-number low
    fm(0xB0, if v == 0 { 0x2E } else { 0x21 }); // key-on + block + F-num high

    // ---- Tandy SN76489: write-only, so nothing ever re-sends this.
    outb(0xC0, if v == 0 { 0x8A } else { 0x93 }); // latch tone0 period lo
    outb(0xC0, if v == 0 { 0x0F } else { 0x21 }); // data: period hi
    outb(0xC0, if v == 0 { 0x92 } else { 0x9F }); // tone0 attenuation

    // ---- 8237 DMA channel 1: an in-flight sample transfer.
    outb(0x0C, 0); // clear the byte-pair flip-flop
    outb(0x0B, if v == 0 { 0x49 } else { 0x59 }); // mode: ch1, read, single/auto
    outb(0x02, if v == 0 { 0x34 } else { 0x78 }); // addr lo
    outb(0x02, if v == 0 { 0x12 } else { 0x56 }); // addr hi
    outb(0x03, if v == 0 { 0xFF } else { 0x40 }); // count lo
    outb(0x03, if v == 0 { 0x03 } else { 0x01 }); // count hi
    outb(0x83, if v == 0 { 0x02 } else { 0x05 }); // page
    outb(0x0A, if v == 0 { 0x01 } else { 0x05 }); // mask/unmask ch1

    // ---- Sound Blaster: mixer registers + a programmed rate.
    outb(0x224, 0x22); // mixer addr: master volume
    outb(0x225, if v == 0 { 0xCC } else { 0x44 });
    outb(0x22C, 0x40); // DSP: set time constant
    outb(0x22C, if v == 0 { 0xA6 } else { 0xD3 });

    // ---- Mouse (INT 33h). The driver is a TSR on a real machine: it survives a
    // game's save/load because it is not part of the game. Ours is not.
    int33(0x0000); // reset — installs the driver
    set_cx(if v == 0 { 10 } else { 100 });
    set_dx(if v == 0 { 300 } else { 500 });
    int33(0x0007); // horizontal window
    set_cx(if v == 0 { 20 } else { 40 });
    set_dx(if v == 0 { 150 } else { 180 });
    int33(0x0008); // vertical window
    set_cx(if v == 0 { 120 } else { 240 });
    set_dx(if v == 0 { 80 } else { 160 });
    int33(0x0004); // set position
    set_cx(if v == 0 { 0x000F } else { 0x0001 });
    set_dx(if v == 0 { 0x1234 } else { 0x4321 });
    set_es(if v == 0 { 0x2000 } else { 0x9000 });
    int33(0x000C); // install the fn-0x0C event handler
    if v == 0 {
        int33(0x0001); // show the cursor
    }
}

/// What the guest can actually see, read back through its own interfaces.
///
/// This is deliberately *not* the captured bytes — a block could faithfully
/// round-trip a field the device never reads, and prove nothing. Only ports
/// whose value is a function of guest-written state are probed: the refresh bit
/// of port 61h, the 6845 raster and the OPL2's timer flags are all functions of
/// *time*, and would differ between two reads of a machine that was working
/// perfectly.
unsafe fn observe() -> Vec<u8> {
    let mut o = Vec::new();

    // The legacy chipset — not on the io_bus, and not growing: this is a 1981
    // PC. New devices arrive on the bus, and the test below catches those.
    o.push(inb(0x21)); // master IMR
    o.push(inb(0xA1)); // slave IMR
    o.push(inb(0x61) & 0x0F); // gate/enable; bits 4-5 are time-driven
    o.push(inb(0x92)); // A20

    // The DMA controller reports its current address and count — mid-transfer,
    // that is how far through the sample the card has got.
    for p in [0x00u16, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07] {
        o.push(inb(p));
    }
    o.push(inb(0x08)); // status

    // Sound Blaster mixer read-back.
    outb(0x224, 0x22);
    o.push(inb(0x225));

    // The mouse, through the only interface it has.
    set_ax(0x0003);
    set_bx(0);
    set_cx(0);
    set_dx(0);
    int33(0x0003); // buttons + position
    o.push(bx() as u8);
    o.extend_from_slice(&cx().to_le_bytes());
    o.extend_from_slice(&dx().to_le_bytes());
    // fn 0x14 swaps the event handler in and hands the old one back — the only
    // way to read what fn 0x0C installed. Put it straight back afterwards.
    set_cx(0);
    set_dx(0);
    set_es(0);
    int33(0x0014);
    let (mask, off, seg) = (cx(), dx(), es());
    o.extend_from_slice(&mask.to_le_bytes());
    o.extend_from_slice(&off.to_le_bytes());
    o.extend_from_slice(&seg.to_le_bytes());
    // Put it back: fn 0x14 is a swap, and the probe must not be a write.
    set_cx(mask);
    set_dx(off);
    set_es(seg);
    int33(0x000C);

    o
}

// ---------------------------------------------------------------------------

#[test]
fn hardware_state_survives_a_power_cycle() {
    let _claim = claim_machine();
    unsafe {
        program_machine(0);

        // Capture *before* observing: reading a PIT counter or a DSP reply moves
        // the device on, and the snapshot must be of the machine as the guest
        // left it, not as the test found it.
        let saved = capture_machine();
        let seen = observe();

        // The power-cycle. Not a reset to defaults — something *different*, so a
        // device that restore forgets about cannot pass by accident just because
        // its default happened to match.
        program_machine(1);
        assert_ne!(
            observe(),
            seen,
            "the two programming scripts left the machine in the same observable \
             state — the test cannot tell a working restore from a no-op"
        );

        restore_machine(&saved);

        let recaptured = capture();
        assert!(
            recaptured == saved.devices,
            "these device blocks did not round-trip: {:?}. restore(capture(x)) \
             must re-capture to x — a field is being dropped, or reconstructed \
             differently, by that device's state_restore.",
            differing_blocks(&saved.devices, &recaptured)
        );
        assert_eq!(
            observe(),
            seen,
            "the guest can see that the hardware changed across a save/restore — \
             some device's state is not in a snapshot block"
        );
    }
}

#[test]
fn every_io_bus_device_has_a_snapshot_block() {
    // A device on the io_bus is, by definition, something the guest programs
    // through ports. If it holds any state at all, that state has to survive a
    // load. This list is the "why not" for the ones that do not carry a block of
    // their own — keeping it explicit is the point, so that adding a card is a
    // decision about persistence rather than an oversight.
    let covered: &[(&str, &str)] = &[
        (
            "opl2",
            "register file rides in ShimRuntimeState; the derived FM synth is \
             replayed from it by devices::post_restore",
        ),
        ("sn76489", "SN76 block"),
        ("sb", "SBLA block"),
        ("dma8237", "DMA8 block"),
        (
            "dma8237-page",
            "the page registers are part of the DMA8 block — same device, second \
             port range",
        ),
    ];

    let devices = crate::io_bus::registered_devices();
    assert!(
        !devices.is_empty(),
        "no devices registered — the .init_array ctors did not run, so this test \
         is not actually checking anything"
    );

    for (name, ports) in &devices {
        assert!(
            covered.iter().any(|(n, _)| n == name),
            "io_bus device '{name}' (ports {ports:02X?}) has no entry in the \
             snapshot-coverage list. Either give it a DeviceBlock in \
             devices::DEVICE_BLOCKS so its state survives a save/load, or add it \
             here with the reason it needs none. A device that is neither will \
             come back at power-on defaults after a restore, silently."
        );
    }
}

#[test]
fn opl2_sounds_the_same_after_a_restore() {
    let _claim = claim_machine();
    unsafe {
        // A chip programmed the way a game programs one: by writing its ports.
        program_machine(0);
        let saved = capture_machine();
        let mut expected = vec![0.0f32; 4096];
        crate::audio::opl2::render(&mut expected);

        // Now the restore path, from a chip holding a different patch.
        program_machine(1);
        restore_machine(&saved);

        let mut actual = vec![0.0f32; 4096];
        crate::audio::opl2::render(&mut actual);

        // Not a fuzzy comparison: the synth is deterministic, so a correct
        // re-derive reproduces the samples exactly. Any drift here means the FM
        // state the player hears was rebuilt from something other than the
        // registers that were saved.
        assert_eq!(
            expected, actual,
            "the OPL2 renders different audio after a restore. Its register file \
             is saved, but the synth those registers drive is built by the writes \
             — devices::post_restore has to replay them (see \
             opl2::resync_synth_from_registers)."
        );
        assert!(
            expected.iter().any(|s| *s != 0.0),
            "the reference render is silent, so this test would pass on a synth \
             that was never programmed at all"
        );
    }
}
