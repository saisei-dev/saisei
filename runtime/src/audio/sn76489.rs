//! The SN76489 — the Tandy 1000 / PCjr three-voice sound chip.
//!
//! Three square-wave tone generators and one noise generator, each with a 4-bit
//! attenuator, behind a single write-only port (0xC0). Dungeon Master ships a
//! TANDY driver blob and Zeliard ships `mscjr.drv` / `sndjr.drv`, so a bundle
//! configured for Tandy lands here.
//!
//! Note that claiming the port is not optional once we model the chip at all:
//! `io_port_error` calls `exit(1)` on any unclaimed port, so a Tandy-configured
//! game would take the process down on its first note rather than merely play
//! silently.

use super::dsp::OnePole;
use super::SAMPLE_RATE;
use crate::io_bus::{io_bus_register, IoDevice};
use core::ffi::c_char;

/// The chip's clock on a Tandy/PCjr: the same 3.579545MHz colourburst the CGA
/// uses. Internally divided by 16 before it reaches the tone counters.
const CHIP_HZ: f64 = 3_579_545.0;
const TICK_HZ: f64 = CHIP_HZ / 16.0;

/// Ticks of the divided clock per output sample (~4.66).
const TICKS_PER_SAMPLE: f64 = TICK_HZ / SAMPLE_RATE as f64;

/// Attenuation is 2dB per step; 15 is not "very quiet", it is off.
fn attenuation_gain(att: u16) -> f32 {
    if att >= 15 {
        0.0
    } else {
        10f32.powf(-0.1 * att as f32)
    }
}

pub struct Sn76489 {
    /// Even indices are tone/noise periods, odd are attenuations:
    /// 0,1 = tone 0; 2,3 = tone 1; 4,5 = tone 2; 6 = noise control; 7 = noise att.
    regs: [u16; 8],
    latch: usize,

    counters: [i32; 4],
    /// The square-wave flip-flop of each tone channel.
    flip: [bool; 3],

    /// 15-bit LFSR. White noise taps bits 0 and 1; periodic noise feeds bit 0
    /// straight back, turning it into a buzzy pitched tone.
    lfsr: u16,
    noise_out: bool,

    tick_acc: f64,
    lpf: OnePole,
}

static mut CHIP: Option<Sn76489> = None;

// ---- snapshot block (see devices.rs) ---------------------------------------
//
// The SN76489 is a write-only chip: there is no port to read a register back
// from, so nothing in the guest ever rewrites what it has already set. Its
// register file *is* the voice — drop it across a load and a game that
// programmed its tones at the start of a level comes back to a silent chip and
// never says another word to it. The oscillator state (counters, flip-flops,
// LFSR) rides along because it is cheap and it keeps a held note continuous
// across the load rather than restarting its phase.

#[repr(C)]
#[derive(Clone, Copy)]
struct SnSnap {
    regs: [u16; 8],
    latch: u32,
    counters: [i32; 4],
    flip: [u8; 3],
    lfsr: u16,
    noise_out: u8,
}

pub(crate) unsafe fn state_capture(out: &mut Vec<u8>) {
    let c = chip();
    let mut s: SnSnap = core::mem::zeroed();
    s.regs = c.regs;
    s.latch = c.latch as u32;
    s.counters = c.counters;
    s.flip = [c.flip[0] as u8, c.flip[1] as u8, c.flip[2] as u8];
    s.lfsr = c.lfsr;
    s.noise_out = c.noise_out as u8;
    crate::devices::pod_capture(&s, out);
}

pub(crate) unsafe fn state_restore(b: &[u8]) -> bool {
    let s = match crate::devices::pod_restore::<SnSnap>(b) {
        Some(s) => s,
        None => return false,
    };
    // `reset()` first: it is what builds the chip (and its filter) in a process
    // that has not seen a register write yet.
    reset();
    let c = chip();
    c.regs = s.regs;
    c.latch = (s.latch as usize).min(7);
    c.counters = s.counters;
    c.flip = [s.flip[0] != 0, s.flip[1] != 0, s.flip[2] != 0];
    c.lfsr = s.lfsr;
    c.noise_out = s.noise_out != 0;
    true
}

/// The chip exists from its first register write, not from audio init: a game
/// may program it before (or without) a device being open, and its register file
/// *is* its state — dropping those writes would lose the voice. Rendering is
/// what depends on audio being up, not bookkeeping.
unsafe fn chip() -> &'static mut Sn76489 {
    if (*core::ptr::addr_of!(CHIP)).is_none() {
        reset();
    }
    (*core::ptr::addr_of_mut!(CHIP)).as_mut().unwrap()
}

pub unsafe fn reset() {
    CHIP = Some(Sn76489 {
        // Attenuators power up silent; a chip that came up at full volume would
        // scream until the game got round to programming it.
        regs: [0, 15, 0, 15, 0, 15, 0, 15],
        latch: 0,
        counters: [0; 4],
        flip: [false; 3],
        lfsr: 1 << 14,
        noise_out: false,
        tick_acc: 0.0,
        lpf: OnePole::new(9000.0),
    });
}

/// A byte arrived on the chip's port. Two framings share it: a LATCH byte (bit
/// 7 set) names a register and carries the low 4 bits of the value; a DATA byte
/// (bit 7 clear) carries the upper 6 bits of whatever was latched last.
pub unsafe fn write(value: u8) {
    super::catchup();
    let c = chip();
    if value & 0x80 != 0 {
        c.latch = ((value >> 4) & 0x07) as usize;
        let data = (value & 0x0F) as u16;
        if c.latch & 1 == 1 {
            c.regs[c.latch] = data; // attenuation: 4 bits, complete in one byte
        } else if c.latch == 6 {
            c.regs[6] = data & 0x07;
            c.lfsr = 1 << 14; // a noise-control write restarts the sequence
        } else {
            c.regs[c.latch] = (c.regs[c.latch] & 0x3F0) | data;
        }
    } else {
        let data = (value & 0x3F) as u16;
        if c.latch & 1 == 1 {
            c.regs[c.latch] = data & 0x0F;
        } else if c.latch == 6 {
            c.regs[6] = data & 0x07;
            c.lfsr = 1 << 14;
        } else {
            c.regs[c.latch] = (c.regs[c.latch] & 0x00F) | (data << 4);
        }
    }
}

/// The noise generator's clock divisor, in ticks of the divided clock. Mode 3
/// takes its rate from tone channel 2 instead — that is how a game slides the
/// noise pitch, and it is why the noise counter has to be able to follow a tone.
fn noise_period(c: &Sn76489) -> i32 {
    match c.regs[6] & 0x03 {
        0 => 32,
        1 => 64,
        2 => 128,
        _ => (c.regs[4] & 0x3FF).max(1) as i32,
    }
}

impl Sn76489 {
    /// Advance the chip one tick of its divided clock.
    fn tick(&mut self) {
        for ch in 0..3 {
            self.counters[ch] -= 1;
            if self.counters[ch] <= 0 {
                // A period of 0 means "no division" — the output sits still
                // rather than toggling at an absurd rate. Games use it to park a
                // channel; treating it as 1 would emit an ultrasonic squeal.
                let period = (self.regs[ch * 2] & 0x3FF) as i32;
                self.counters[ch] = period.max(1);
                if period > 0 {
                    self.flip[ch] = !self.flip[ch];
                }
            }
        }

        self.counters[3] -= 1;
        if self.counters[3] <= 0 {
            self.counters[3] = noise_period(self);
            // The LFSR advances on every *rising* edge, so it steps at half the
            // counter rate — the same relationship a tone channel's flip-flop has
            // to its counter.
            let white = self.regs[6] & 0x04 != 0;
            let feedback = if white {
                (self.lfsr ^ (self.lfsr >> 1)) & 1
            } else {
                self.lfsr & 1
            };
            self.lfsr = (self.lfsr >> 1) | (feedback << 14);
            self.noise_out = self.lfsr & 1 != 0;
        }
    }

    fn amplitude(&self) -> f32 {
        let mut v = 0.0;
        for ch in 0..3 {
            let g = attenuation_gain(self.regs[ch * 2 + 1]);
            if g > 0.0 {
                v += if self.flip[ch] { g } else { -g };
            }
        }
        let g = attenuation_gain(self.regs[7]);
        if g > 0.0 {
            v += if self.noise_out { g } else { -g };
        }
        v
    }
}

/// Add the chip's contribution. Each output sample box-filters every chip tick
/// that falls inside it: the tone flip-flops run at up to ~110kHz, and point-
/// sampling those at 48kHz would fold them straight back into the audible band.
pub unsafe fn render(buf: &mut [f32]) {
    // Nothing has ever touched the chip: it is not in this game's build.
    if (*core::ptr::addr_of!(CHIP)).is_none() {
        return;
    }
    let c = chip();
    let frames = buf.len() / 2;
    for i in 0..frames {
        c.tick_acc += TICKS_PER_SAMPLE;
        let n = c.tick_acc as u32;
        c.tick_acc -= n as f64;

        let mut acc = 0.0f32;
        let mut count = 0u32;
        for _ in 0..n {
            c.tick();
            acc += c.amplitude();
            count += 1;
        }
        let v = if count > 0 {
            acc / count as f32
        } else {
            c.amplitude()
        };
        let s = c.lpf.tick(v) * 0.12;
        buf[i * 2] += s;
        buf[i * 2 + 1] += s;
    }
}

// ---- the port ---------------------------------------------------------------

extern "C" fn sn_port_write(_port: u16, value: u8) {
    unsafe { write(value) }
}

/// The chip is write-only; a read floats. (Returning anything else would invent
/// a status register the hardware does not have, which is exactly the kind of
/// convenient fiction a detection routine would then branch on.)
extern "C" fn sn_port_read(_port: u16) -> u8 {
    0xFF
}

static SN_PORTS: [u16; 2] = [0xC0, 0xFFFF];
static SN_DEVICE: IoDevice = IoDevice {
    name: b"sn76489\0".as_ptr() as *const c_char,
    ports: SN_PORTS.as_ptr(),
    read8: Some(sn_port_read),
    write8: Some(sn_port_write),
};

extern "C" fn sn_register() {
    io_bus_register(core::ptr::addr_of!(SN_DEVICE));
}

#[used]
#[cfg_attr(not(target_os = "macos"), link_section = ".init_array")]
#[cfg_attr(target_os = "macos", link_section = "__DATA,__mod_init_func")]
static SN_CTOR: extern "C" fn() = sn_register;

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::{Mutex, MutexGuard};

    static LOCK: Mutex<()> = Mutex::new(());
    pub(crate) fn claim() -> MutexGuard<'static, ()> {
        LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn tone_at(buf: &[f32], freq: f64) -> f64 {
        let n = buf.len() / 2;
        let k = 2.0 * (core::f64::consts::TAU * freq / SAMPLE_RATE as f64).cos();
        let (mut s1, mut s2) = (0.0f64, 0.0f64);
        for i in 0..n {
            let s0 = buf[i * 2] as f64 + k * s1 - s2;
            s2 = s1;
            s1 = s0;
        }
        (s1 * s1 + s2 * s2 - k * s1 * s2).max(0.0).sqrt() / n as f64
    }

    fn render_frames(n: usize) -> Vec<f32> {
        let mut buf = vec![0.0f32; n * 2];
        unsafe { render(&mut buf) };
        buf
    }

    /// Program tone channel 0 for `div`, full volume. The protocol is a LATCH byte
    /// carrying the low 4 bits, then a DATA byte carrying the upper 6.
    unsafe fn play(div: u16) {
        reset();
        write(0x80 | (div & 0x0F) as u8); // latch ch0 tone, low nibble
        write(((div >> 4) & 0x3F) as u8); // data: upper 6 bits
        write(0x90); // ch0 attenuation 0 = loudest
    }

    #[test]
    fn silent_until_programmed() {
        let _g = claim();
        unsafe { reset() };
        let buf = render_frames(4096);
        let peak = buf.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        assert!(peak < 1.0e-6, "chip should power up silent, got {peak}");
    }

    #[test]
    fn a_tone_channel_sounds_at_its_divisor() {
        let _g = claim();
        // f = 3579545 / (32 * n). n = 254 -> 440.5Hz.
        unsafe { play(254) };
        let buf = render_frames(16384);

        let peak = buf.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        assert!(peak > 0.02, "programmed tone is inaudible ({peak})");

        let f0 = tone_at(&buf, 440.5);
        for other in [300.0, 370.0, 550.0, 660.0] {
            assert!(
                f0 > tone_at(&buf, other) * 4.0,
                "440Hz should dominate {other}Hz"
            );
        }
    }

    #[test]
    fn attenuation_15_is_off_not_merely_quiet() {
        let _g = claim();
        unsafe { play(254) };
        let loud = render_frames(8192)
            .iter()
            .fold(0.0f32, |m, v| m.max(v.abs()));

        unsafe { write(0x9F) }; // ch0 attenuation 15
        let after = render_frames(8192);
        // Skip the head: the output filter is still ringing down from the note
        // that was playing a sample ago, and that decay is the filter doing its
        // job, not the channel refusing to stop.
        let off = after[1024..].iter().fold(0.0f32, |m, v| m.max(v.abs()));

        assert!(loud > 0.02, "the note never sounded ({loud})");
        assert!(
            off < 1.0e-4,
            "attenuation 15 must silence the channel, not merely quieten it (got {off})"
        );
    }

    #[test]
    fn the_noise_channel_is_noise_and_not_a_tone() {
        let _g = claim();
        unsafe {
            reset();
            write(0xE4); // noise: white, clock/512
            write(0xF0); // noise attenuation 0
        }
        let buf = render_frames(16384);
        let peak = buf.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        assert!(peak > 0.01, "noise channel produced nothing");

        // Noise spreads its energy; no single bin should dominate the way a tone's
        // fundamental does.
        let bins: Vec<f64> = (1..24).map(|k| tone_at(&buf, 200.0 * k as f64)).collect();
        let max = bins.iter().cloned().fold(0.0f64, f64::max);
        let mean = bins.iter().sum::<f64>() / bins.len() as f64;
        assert!(
            max < mean * 8.0,
            "noise should not have a dominant partial (peak/mean = {:.1})",
            max / mean
        );
    }
}
