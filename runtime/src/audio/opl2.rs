//! OPL2 (AdLib / YM3812) FM sound card.
//!
//! Driven purely through IO ports 0x388/0x389; the inb/outb dispatcher forwards
//! those here. This file owns two things that must not be confused:
//!
//! * The **register file and timers** (`Opl2State`) — the part the *guest*
//!   observes. Its layout is FROZEN: snapshots serialise it byte-for-byte, so it
//!   may not gain a field. Games poll the timer/overflow status bits to detect
//!   the card, and that handshake has to be right whether or not anyone is
//!   listening.
//! * The **FM synthesis** that turns those registers into sound. That state is
//!   derived, host-side only, and lives in separate statics precisely *because*
//!   the snapshot layout is frozen — on restore it is reset and re-derived from
//!   the restored register file rather than carried across.

use crate::io_bus::{io_bus_register, IoDevice};
use core::ffi::c_char;

extern "C" {
    fn shim_scaled_monotonic_ns() -> u64;
    fn shim_time_sync() -> u64;
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
        // Fold un-accounted budget into the clock first, exactly as every other
        // polled status port does (0x40, 0x42, 0x61, 0x3DA). Without it a driver
        // spinning on the timer-overflow bits sees virtual time frozen until the
        // next safepoint, and the timer it is waiting for can never expire.
        shim_time_sync();
        let o = &mut *s();
        // The chip answers at both its own base and the SB's; the low bit is what
        // selects address vs data, not the absolute port.
        let is_addr = port & 1 == 0;
        if is_addr {
            // YM3812 status: bit7 = IRQ (set when EITHER unmasked timer
            // overflows), bit6 = timer-1 overflow, bit5 = timer-2 overflow;
            // D4-D0 read as 0 (there is no "busy" status bit — the required
            // write delay is a bus-timing constraint, not a flag). The
            // canonical AdLib timer/presence check starts timer 1 and waits
            // for (status & 0xE0) == 0xC0. The old model set the WRONG bits —
            // timer-1 raised bit7 (0x80) alone and timer-2 raised bit6 (0x40),
            // with no combined-IRQ bit — so that handshake never matched its
            // expected 0xC0 and a driver polling for it could spin. Zeliard is
            // configured for AdLib (resource.cfg: MSCADLIB.DRV/SNDADLIB.DRV),
            // so every shop's music-init runs this path; entering a building
            // (verified: the church) now works. bit0's phantom "busy" flag is
            // also removed — no such YM3812 status bit exists.
            let now_us = shim_scaled_monotonic_ns() / 1000;
            let mask = o.registers[0x04];
            if o.timer1_expire_us != 0 && now_us >= o.timer1_expire_us {
                if mask & 0x40 == 0 {
                    o.status |= 0xC0; // IRQ | T1
                }
                o.timer1_expire_us = 0;
            }
            if o.timer2_expire_us != 0 && now_us >= o.timer2_expire_us {
                if mask & 0x20 == 0 {
                    o.status |= 0xA0; // IRQ | T2
                }
                o.timer2_expire_us = 0;
            }
            return o.status;
        }
        // data port
        o.registers[o.address as usize]
    }
}

extern "C" fn opl2_port_write(port: u16, value: u8) {
    unsafe {
        // Render everything owed up to *this instant* before the register
        // changes, so the change is heard at its own virtual timestamp rather
        // than smeared back over the samples that preceded it.
        super::catchup();
        let o = &mut *s();
        if port & 1 == 0 {
            o.address = value;
            o.busy_until_us = shim_scaled_monotonic_ns() / 1000 + OPL2_BUSY_DURATION_US;
            return;
        }
        // data port
        let now_us = shim_scaled_monotonic_ns() / 1000;
        o.busy_until_us = now_us + OPL2_BUSY_DURATION_US;
        if o.address == 0x04 && value & 0x80 != 0 {
            // IRQ-RESET write: clears the status flag bits; every other bit
            // of this write is IGNORED (the register keeps its old value —
            // the mask/start bits do not change). YM3812 datasheet semantics.
            o.status &= !0xE0;
            return;
        }
        o.registers[o.address as usize] = value;
        // AFTER the store: a key-on has to see the F-Number and block it is being
        // keyed with, and those live in the register file it just landed in.
        synth_write(o.address, value);
        if o.address == 0x04 {
            // bit0/bit1 start (or stop) timer 1/2; the mask bits (bit6/bit5)
            // gate flag SETTING and are consulted live at expiry.
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

/// 0x388/0x389 is the AdLib card. 0x228/0x229 is the *same chip* at the Sound
/// Blaster's base address — an SB has an OPL2 on board and decodes it there too,
/// and a game that found an SB will happily drive FM through it. Those two ports
/// used to fall inside the SB DSP's range and get answered with 0xFF, so FM
/// through an SB went nowhere at all.
static OPL2_PORTS: [u16; 5] = [0x388, 0x389, 0x228, 0x229, 0xFFFF];
static OPL2_DEVICE: IoDevice = IoDevice {
    name: b"opl2\0".as_ptr() as *const c_char,
    ports: OPL2_PORTS.as_ptr(),
    read8: Some(opl2_port_read),
    write8: Some(opl2_port_write),
};

// ============================================================================
// FM synthesis
//
// Host-side only: nothing below is visible to the guest, and nothing below may
// be added to `Opl2State` (frozen layout, see the module header). On a snapshot
// restore the register file comes back and this is re-derived from it.
//
// The chip is 9 channels of 2 operators. An operator is a sine oscillator whose
// output is an *attenuation* (the whole chip works in the log domain, which is
// why a multiply — the FM depth, the envelope, the total level — is an add).
// Its phase can be modulated by another operator's output: that is the "FM".
//
// Everything is computed at the chip's own 49716 Hz (14.318MHz / 288) and
// resampled to the mixer's rate, because the envelope rates, the LFOs and the
// phase increments are all defined against that clock.
// ============================================================================

/// The YM3812's sample rate: the 14.318MHz master clock divided by 288.
const CHIP_HZ: f64 = 14_318_180.0 / 288.0;

const NUM_CH: usize = 9;
const NUM_OP: usize = 18;

/// Register offset of each operator. The chip's operator space has holes — 0x06,
/// 0x07, 0x0E, 0x0F are not operators — so the offsets are not contiguous.
const OP_OFFSET: [usize; NUM_OP] = [
    0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x08, 0x09, 0x0A, 0x0B, 0x0C, 0x0D, 0x10, 0x11, 0x12, 0x13,
    0x14, 0x15,
];

/// Each channel's (modulator, carrier) operator indices.
const CH_OPS: [(usize, usize); NUM_CH] = [
    (0, 3),
    (1, 4),
    (2, 5),
    (6, 9),
    (7, 10),
    (8, 11),
    (12, 15),
    (13, 16),
    (14, 17),
];

/// MULT, doubled so the 0 -> 0.5 case stays an integer. Note 11/13 and 14/15 are
/// duplicates — the chip really does have no 11x or 13x.
const MULT2: [u32; 16] = [1, 2, 4, 6, 8, 10, 12, 14, 16, 18, 20, 20, 24, 24, 30, 30];

/// Key-scale-level attenuation at the top of each octave, in units of 0.75dB —
/// the same units as Total Level, which is why both are shifted by 2 into the
/// envelope's 0.1875dB units below.
const KSL_ROM: [i32; 16] = [
    0, 32, 40, 45, 48, 51, 53, 55, 56, 58, 59, 60, 61, 62, 63, 64,
];
/// The KSL field is not in increasing order: 0 = off, 1 = 3dB/oct, 2 = 1.5dB/oct,
/// 3 = 6dB/oct. A shift of 8 is how "off" is spelled.
const KSL_SHIFT: [i32; 4] = [8, 1, 2, 0];

/// The envelope's increment pattern. Within each group of 8 counter steps the
/// low two bits of the effective rate buy 4, 5, 6 or 7 steps out of the 8 (rows
/// 0-3) — that is how the chip gets four distinct speeds out of every doubling.
/// Once the rate is fast enough that it steps *every* sample, it cannot step
/// more often, so it steps harder instead: rows 4-11 raise the increment itself
/// (rate groups 13 and 14), row 12 is flat-out, and row 13 is the attack's
/// special top gear. Row 14 is a rate register of 0 — a note held forever.
const EG_INC: [[i32; 8]; 15] = [
    [0, 1, 0, 1, 0, 1, 0, 1], // rate group <= 12, lo = 0
    [0, 1, 0, 1, 1, 1, 0, 1], // lo = 1
    [0, 1, 1, 1, 0, 1, 1, 1], // lo = 2
    [0, 1, 1, 1, 1, 1, 1, 1], // lo = 3
    [1, 1, 1, 1, 1, 1, 1, 1], // rate group 13, lo = 0
    [1, 1, 1, 2, 1, 1, 1, 2], // lo = 1
    [1, 2, 1, 2, 1, 2, 1, 2], // lo = 2
    [1, 2, 2, 2, 1, 2, 2, 2], // lo = 3
    [2, 2, 2, 2, 2, 2, 2, 2], // rate group 14, lo = 0
    [2, 2, 2, 4, 2, 2, 2, 4], // lo = 1
    [2, 4, 2, 4, 2, 4, 2, 4], // lo = 2
    [2, 4, 4, 4, 2, 4, 4, 4], // lo = 3
    [4, 4, 4, 4, 4, 4, 4, 4], // rate group 15
    [8, 8, 8, 8, 8, 8, 8, 8], // attack, rate >= 62
    [0, 0, 0, 0, 0, 0, 0, 0], // rate register 0: never advances
];

#[derive(Clone, Copy, PartialEq)]
enum Eg {
    Off,
    Attack,
    Decay,
    Sustain,
    Release,
}

#[derive(Clone, Copy)]
struct Op {
    /// 20 bits of phase; one cycle is 2^20, so the 10-bit sine index is bits
    /// 19..10.
    phase: u32,
    /// Attenuation, 0 (loudest) .. 511 (silent), in units of 0.1875dB — a 96dB
    /// range, which is the chip's whole dynamic range.
    att: i32,
    eg: Eg,
    keyed: bool,
    /// The last two outputs, for operator 1's self-feedback.
    out: [i32; 2],
}

impl Op {
    const fn new() -> Self {
        Op {
            phase: 0,
            att: 511,
            eg: Eg::Off,
            keyed: false,
            out: [0, 0],
        }
    }
}

struct Synth {
    ops: [Op; NUM_OP],
    /// Log-sine (quarter wave) and exp tables — the chip's two ROMs.
    logsin: [u16; 256],
    exp: [u16; 256],

    eg_counter: u64,
    /// 23-bit LFSR feeding the rhythm section's noise.
    noise: u32,

    /// Tremolo: a 210-step triangle stepped every 64 samples -> 3.70Hz.
    am_pos: u32,
    am_sub: u32,
    /// Vibrato: an 8-step LFO advanced every 1024 samples -> 6.07Hz.
    vib_pos: u32,
    vib_sub: u32,

    /// Resampler: the chip runs at 49716 Hz, the mixer at 48000.
    resample_acc: f64,
    prev: f32,
    cur: f32,

    dc: super::dsp::DcBlock,
}

static mut SYNTH: Option<Synth> = None;

unsafe fn synth() -> &'static mut Synth {
    if (*core::ptr::addr_of!(SYNTH)).is_none() {
        synth_reset();
    }
    (*core::ptr::addr_of_mut!(SYNTH)).as_mut().unwrap()
}

/// Reset the derived synth state. Called when audio comes up and after a
/// snapshot restore — the register file is restored, this is re-derived.
pub unsafe fn synth_reset() {
    let mut logsin = [0u16; 256];
    let mut exp = [0u16; 256];
    for i in 0..256 {
        // The quarter-wave sine, stored as attenuation: -log2(sin) scaled so 256
        // units is one octave of amplitude. Sampled at the half-step so it never
        // has to represent log(0).
        let s = (((i as f64) + 0.5) * core::f64::consts::PI / 512.0).sin();
        logsin[i] = (-s.log2() * 256.0).round() as u16;
        // The inverse: 2^x, so that adding attenuations multiplies amplitudes.
        exp[i] = (2f64.powf((255.0 - i as f64) / 256.0) * 1024.0).round() as u16;
    }
    SYNTH = Some(Synth {
        ops: [Op::new(); NUM_OP],
        logsin,
        exp,
        eg_counter: 0,
        noise: 1,
        am_pos: 0,
        am_sub: 0,
        vib_pos: 0,
        vib_sub: 0,
        resample_acc: 0.0,
        prev: 0.0,
        cur: 0.0,
        dc: super::dsp::DcBlock::new(),
    });
}

/// Rebuild the FM synth from the restored register file (see `devices.rs`).
///
/// The OPL2's register file is the one piece of audio state a snapshot always
/// carried — it rides in `ShimRuntimeState`. But the chip you *hear* is not that
/// byte array: it is `SYNTH`, and `SYNTH` is built up by `synth_write` on each
/// port write, not from the array. So a restore used to hand the guest a
/// perfectly programmed register file attached to a synth that had never been
/// programmed at all: every instrument patch, the waveform-select enable and
/// rhythm mode were back to ctor defaults, and the game — which sets its patches
/// once and then only sends notes — had no reason to ever write them again.
///
/// Replaying the whole file in ascending order is exactly the sequence of writes
/// that would have produced it. Order is not delicate: `synth_write` reads every
/// register it consults (0xBD for rhythm, 0xB0-0xB8 for the keys) out of the
/// restored file rather than out of the replay, so each write already sees the
/// finished chip. A note that was sounding re-attacks rather than resuming
/// mid-envelope — the envelope phase is derived state we do not carry — which
/// costs one note's attack on load and nothing else.
pub unsafe fn resync_synth_from_registers() {
    synth_reset();
    for r in 0..=0xFFusize {
        synth_write(r as u8, reg(r));
    }
}

// ---- register access ---------------------------------------------------------

#[inline]
unsafe fn reg(r: usize) -> u8 {
    (*s()).registers[r & 0xFF]
}

/// Waveform select is *disabled* by default: until a game sets reg 0x01 bit 5,
/// every operator is a plain sine no matter what 0xE0 says. Miss this and an
/// early AdLib title that leaves junk in the waveform registers plays through
/// the wrong oscillators.
#[inline]
unsafe fn wave_enabled() -> bool {
    reg(0x01) & 0x20 != 0
}

#[inline]
unsafe fn rhythm_enabled() -> bool {
    reg(0xBD) & 0x20 != 0
}

/// The channel an operator belongs to, and whether it is the carrier.
fn op_channel(op: usize) -> (usize, bool) {
    for (ch, &(m, c)) in CH_OPS.iter().enumerate() {
        if op == m {
            return (ch, false);
        }
        if op == c {
            return (ch, true);
        }
    }
    (0, false)
}

unsafe fn fnum(ch: usize) -> u32 {
    let lo = reg(0xA0 + ch) as u32;
    let hi = reg(0xB0 + ch) as u32;
    ((hi & 0x03) << 8) | lo
}

unsafe fn block(ch: usize) -> u32 {
    (reg(0xB0 + ch) as u32 >> 2) & 0x07
}

/// The key-scale number: how high the note is, which is what makes envelopes
/// shorten and quieten as a voice climbs — the chip's model of a real
/// instrument's top octave.
unsafe fn key_scale(ch: usize) -> i32 {
    let f = fnum(ch);
    // NTS (reg 0x08 bit 6) picks which F-Num bit joins the block.
    let nts = reg(0x08) & 0x40 != 0;
    let bit = if nts { (f >> 8) & 1 } else { (f >> 9) & 1 };
    ((block(ch) << 1) | bit) as i32
}

// ---- the envelope ------------------------------------------------------------

/// How much the envelope moves this sample. `reg_rate` is the raw 4-bit register
/// value (AR/DR/RR) and `ksv` the key-scale value; a register rate of 0 never
/// moves at all, which is how a game holds a note forever.
fn eg_step(counter: u64, reg_rate: i32, ksv: i32) -> i32 {
    if reg_rate == 0 {
        return 0; // row 14
    }
    let rate = reg_rate * 4 + ksv;
    let hi = (rate >> 2).min(15);
    let lo = (rate & 3) as usize;
    let row = match hi {
        0..=12 => lo,
        13 => 4 + lo,
        14 => 8 + lo,
        _ => 12,
    };
    // The counter is the chip's own sample clock. A slow rate steps only when the
    // low `shift` bits of it are zero — i.e. once every 2^shift samples.
    let shift = (12 - hi).clamp(0, 12) as u64;
    if shift > 0 && (counter & ((1u64 << shift) - 1)) != 0 {
        return 0;
    }
    EG_INC[row][((counter >> shift) & 7) as usize]
}

impl Synth {
    /// How much the envelope's rate is scaled by how high the note is — and
    /// whether this operator asked to be scaled at all (the KSR bit). This is the
    /// chip's model of a real instrument, whose top octave decays faster.
    unsafe fn ksv_of(&self, op: usize) -> i32 {
        let (ch, _) = op_channel(op);
        let off = OP_OFFSET[op];
        let ksn = key_scale(ch);
        if reg(0x20 + off) & 0x10 != 0 {
            ksn
        } else {
            ksn >> 2
        }
    }

    unsafe fn eg_advance(&mut self, op: usize) {
        let off = OP_OFFSET[op];
        let ad = reg(0x60 + off);
        let sr = reg(0x80 + off);
        let ar = (ad >> 4) as i32;
        let dr = (ad & 0x0F) as i32;
        let sl = (sr >> 4) as i32;
        let rr = (sr & 0x0F) as i32;
        // EGT: set = hold at the sustain level while keyed (an organ); clear =
        // keep falling straight through it (a plucked string).
        let sustaining = reg(0x20 + off) & 0x20 != 0;

        // Sustain level: 3dB per step, and 15 does not mean 45dB — it means off.
        let sl_units = if sl == 15 { 496 } else { sl * 16 };
        let counter = self.eg_counter;
        let ksv = self.ksv_of(op);

        match self.ops[op].eg {
            Eg::Off => {}
            Eg::Attack => {
                let att = self.ops[op].att;
                // The attack's own top gear: at an effective rate of 62 or more
                // it is not merely fast, it is a step function.
                let inc = if ar * 4 + ksv >= 62 {
                    EG_INC[13][((counter >> 0) & 7) as usize]
                } else {
                    eg_step(counter, ar, ksv)
                };
                if inc > 0 {
                    // Attack is exponential in *attenuation*: it closes a fraction
                    // of the remaining distance to 0 each step. `!att` is
                    // -(att + 1), so this subtracts.
                    self.ops[op].att = att + (((!att) * inc) >> 3);
                    if self.ops[op].att <= 0 {
                        self.ops[op].att = 0;
                        self.ops[op].eg = Eg::Decay;
                    }
                }
            }
            Eg::Decay => {
                self.ops[op].att += eg_step(counter, dr, ksv);
                if self.ops[op].att >= sl_units {
                    self.ops[op].att = sl_units;
                    self.ops[op].eg = Eg::Sustain;
                }
            }
            Eg::Sustain => {
                if !sustaining {
                    self.ops[op].att += eg_step(counter, rr, ksv);
                }
            }
            Eg::Release => {
                self.ops[op].att += eg_step(counter, rr, ksv);
            }
        }
        // The silent floor: once the top bits are all set the chip pins it.
        if self.ops[op].att & 0x1F8 == 0x1F8 {
            self.ops[op].att = 511;
            if matches!(self.ops[op].eg, Eg::Release | Eg::Sustain) {
                self.ops[op].eg = Eg::Off;
            }
        }
        self.ops[op].att = self.ops[op].att.clamp(0, 511);
    }

    /// The operator's total attenuation this sample: its envelope, plus the level
    /// the game set, plus the key-scale roll-off, plus tremolo. All in the
    /// envelope's 0.1875dB units — attenuations add, which in the linear domain
    /// is the multiply this whole chip is built to avoid.
    unsafe fn total_att(&self, op: usize, am_units: i32) -> i32 {
        let off = OP_OFFSET[op];
        let (ch, _) = op_channel(op);
        let ksltl = reg(0x40 + off);
        // TL is 0.75dB per step: four envelope units each.
        let tl = (ksltl & 0x3F) as i32 * 4;
        let ksl_field = (ksltl >> 6) as usize;

        // Key-scale level: how much quieter this operator gets as the note
        // climbs. Scaled into envelope units *before* the KSL field's shift —
        // shifting the coarser value first would throw away its low bits.
        let b = block(ch) as i32;
        let f = fnum(ch);
        let mut ksl = (KSL_ROM[(f >> 6) as usize] << 2) - ((8 - b) << 5);
        if ksl < 0 {
            ksl = 0;
        }
        ksl >>= KSL_SHIFT[ksl_field];

        let am = if reg(0x20 + off) & 0x80 != 0 {
            am_units
        } else {
            0
        };
        self.ops[op].att + tl + ksl + am
    }

    /// One operator's sample at an explicit 10-bit phase index. The rhythm
    /// section needs to drive the index directly, so the index — not a phase
    /// modulation — is the primitive here.
    unsafe fn op_sample_at(&self, op: usize, idx: u32, am_units: i32) -> i32 {
        let off = OP_OFFSET[op];
        let att = self.total_att(op, am_units);
        let idx = idx & 0x3FF;

        // Waveform select does nothing until the game turns it on (reg 0x01 bit
        // 5). An early AdLib title that leaves junk in 0xE0-0xF5 must still play
        // sines.
        let wave = if wave_enabled() {
            reg(0xE0 + off) & 0x03
        } else {
            0
        };

        // The chip stores only a quarter of a sine and folds the rest out of it;
        // each waveform is a different fold. A muted half is an *exact* zero, so
        // it gets an attenuation big enough to shift the exp lookup to nothing.
        const MUTE: i32 = 0x1000;
        let quadrant = (idx >> 8) & 3;
        let pos = (idx & 0xFF) as usize;
        let mirrored = if quadrant & 1 == 1 { 255 - pos } else { pos };
        let (logsin_v, negative) = match wave {
            0 => (self.logsin[mirrored] as i32, quadrant >= 2),
            // Half sine: the negative lobe is simply gone.
            1 if quadrant >= 2 => (MUTE, false),
            1 => (self.logsin[mirrored] as i32, false),
            // Absolute sine: the negative lobe is folded up.
            2 => (self.logsin[mirrored] as i32, false),
            // Pulse sine: only the rising quarter of each half survives.
            _ if quadrant & 1 == 1 => (MUTE, false),
            _ => (self.logsin[pos] as i32, false),
        };

        // Both terms are attenuations, so this add is a multiply in the linear
        // domain — the envelope scaling the oscillator. 0x1FFF is silence.
        let total = (logsin_v + (att << 3)).clamp(0, 0x1FFF) as u32;
        let v = ((self.exp[(total & 0xFF) as usize] as i32) << 1) >> (total >> 8);
        if negative {
            -v
        } else {
            v
        }
    }

    /// One operator's sample, phase-modulated. `modulation` shifts its phase —
    /// that is the whole trick — in the same 1024-per-cycle units as the index.
    unsafe fn op_sample(&self, op: usize, modulation: i32, am_units: i32) -> i32 {
        let idx = ((self.ops[op].phase >> 10) as i32).wrapping_add(modulation) as u32;
        self.op_sample_at(op, idx, am_units)
    }

    /// Advance an operator's phase by one chip sample.
    unsafe fn phase_advance(&mut self, op: usize) {
        let off = OP_OFFSET[op];
        let (ch, _) = op_channel(op);
        let mult = MULT2[(reg(0x20 + off) & 0x0F) as usize];
        let mut f = fnum(ch) as i32;
        let b = block(ch);

        if reg(0x20 + off) & 0x40 != 0 {
            // Vibrato modulates the F-Number itself, *before* the block shift —
            // so the depth is a constant number of cents at every octave, which
            // is what makes it sound like a vibrato and not a wobble that gets
            // worse as you go up. Depth is 7 cents, or 14 with reg 0xBD bit 6.
            let mut range = (f >> 7) & 7;
            if self.vib_pos & 3 == 0 {
                range = 0;
            } else if self.vib_pos & 1 != 0 {
                range >>= 1;
            }
            range >>= if reg(0xBD) & 0x40 != 0 { 0 } else { 1 };
            if self.vib_pos & 4 != 0 {
                range = -range;
            }
            f += range;
        }

        // One cycle is 2^20 of phase, and f / 2^(20-block) cycles pass per sample
        // — the datasheet's F-Number formula, rearranged.
        let inc = (((f.max(0) as u32) << b) * mult) >> 1;
        self.ops[op].phase = self.ops[op].phase.wrapping_add(inc) & 0xFFFFF;
    }

    /// Step the two chip-wide LFOs and the noise register.
    unsafe fn tick_globals(&mut self) {
        self.eg_counter = self.eg_counter.wrapping_add(1);

        // 23-bit LFSR, tapped at bits 0 and 14.
        let bit = ((self.noise ^ (self.noise >> 14)) & 1) << 22;
        self.noise = (self.noise >> 1) | bit;

        self.am_sub += 1;
        if self.am_sub >= 64 {
            self.am_sub = 0;
            self.am_pos = (self.am_pos + 1) % 210;
        }
        self.vib_sub += 1;
        if self.vib_sub >= 1024 {
            self.vib_sub = 0;
            self.vib_pos = (self.vib_pos + 1) & 7;
        }
    }

    /// The tremolo LFO's current depth, in envelope units. A 210-step triangle;
    /// reg 0xBD bit 7 picks 4.8dB rather than 1.0dB.
    unsafe fn am_units(&self) -> i32 {
        let shift = if reg(0xBD) & 0x80 != 0 { 2 } else { 4 };
        let pos = self.am_pos as i32;
        if pos < 105 {
            pos >> shift
        } else {
            (210 - pos) >> shift
        }
    }
}

// ---- keying ------------------------------------------------------------------

unsafe fn key_on(sy: &mut Synth, op: usize) {
    if sy.ops[op].keyed {
        return;
    }
    sy.ops[op].keyed = true;
    sy.ops[op].eg = Eg::Attack;
    // The phase generator resets, which is why two notes keyed together on this
    // chip are always in phase.
    sy.ops[op].phase = 0;
    sy.ops[op].out = [0, 0];
}

unsafe fn key_off(sy: &mut Synth, op: usize) {
    if !sy.ops[op].keyed {
        return;
    }
    sy.ops[op].keyed = false;
    if sy.ops[op].eg != Eg::Off {
        sy.ops[op].eg = Eg::Release;
    }
}

/// Rhythm mode steals the last three channels: channel 6 becomes the bass drum
/// (still a normal 2-operator voice), and channels 7 and 8 are broken up into
/// four one-operator percussion voices, each keyed by its own bit of reg 0xBD.
const RHYTHM_OPS: [(u8, usize); 5] = [
    (0x10, 12), // bass drum   — channel 6, both operators
    (0x08, 16), // snare       — channel 7 carrier
    (0x04, 14), // tom-tom     — channel 8 modulator
    (0x02, 17), // cymbal      — channel 8 carrier
    (0x01, 13), // hi-hat      — channel 7 modulator
];

/// A register write landed. `catchup()` has already run, so the synth is
/// rendered up to this exact instant and may act on the write immediately.
unsafe fn synth_write(r: u8, value: u8) {
    let sy = synth();
    let r = r as usize;

    if (0xB0..=0xB8).contains(&r) {
        let ch = r - 0xB0;
        // In rhythm mode the last three channels take their keys from 0xBD
        // instead, and this bit means nothing to them.
        if rhythm_enabled() && ch >= 6 {
            return;
        }
        let (m, c) = CH_OPS[ch];
        if value & 0x20 != 0 {
            key_on(sy, m);
            key_on(sy, c);
        } else {
            key_off(sy, m);
            key_off(sy, c);
        }
        return;
    }

    if r == 0xBD {
        // 0xBD is TWO registers wearing one address: the rhythm section's enable
        // and its five keys, *and* the chip-wide tremolo and vibrato depths. A
        // purely melodic driver writes it for the depth bits alone and means
        // nothing by the rest — so a write here may only touch the percussion
        // keys when rhythm mode is actually on. Treating every 0xBD write as a
        // key update silences channels 6, 7 and 8 every time a driver sets its
        // vibrato depth, which is exactly what a driver does when it restarts
        // its music.
        if value & 0x20 == 0 {
            // Not rhythm mode: channels 6-8 are ordinary melodic channels, and
            // their keys live in 0xB6-0xB8 where they always did. Hand them back
            // rather than assuming they are silent — coming *out* of rhythm mode
            // must restore whatever those channels were being told to play.
            for ch in 6..NUM_CH {
                let (m, c) = CH_OPS[ch];
                if reg(0xB0 + ch) & 0x20 != 0 {
                    key_on(sy, m);
                    key_on(sy, c);
                } else {
                    key_off(sy, m);
                    key_off(sy, c);
                }
            }
            return;
        }
        for &(bit, op) in RHYTHM_OPS.iter() {
            if value & bit != 0 {
                key_on(sy, op);
                if bit == 0x10 {
                    key_on(sy, CH_OPS[6].1); // the bass drum's carrier
                }
            } else {
                key_off(sy, op);
                if bit == 0x10 {
                    key_off(sy, CH_OPS[6].1);
                }
            }
        }
    }
}

// ---- rendering ---------------------------------------------------------------

/// Run one 2-operator melodic channel and return its output.
unsafe fn melodic_channel(sy: &mut Synth, ch: usize, am_units: i32, drop_op1: bool) -> i32 {
    let (m, c) = CH_OPS[ch];
    let cn = reg(0xC0 + ch);
    let fb = (cn >> 1) & 0x07;
    let additive = cn & 0x01 != 0;

    sy.eg_advance(m);
    sy.eg_advance(c);
    sy.phase_advance(m);
    sy.phase_advance(c);

    // Operator 1 modulates itself with the *average* of its last two outputs.
    // Averaging is not a detail — it is a one-pole lowpass inside the feedback
    // loop, and without it the loop just screams.
    let fbmod = if fb != 0 {
        (sy.ops[m].out[0] + sy.ops[m].out[1]) >> (9 - fb)
    } else {
        0
    };
    let mo = sy.op_sample(m, fbmod, am_units);
    sy.ops[m].out[1] = sy.ops[m].out[0];
    sy.ops[m].out[0] = mo;

    if additive {
        // Both operators reach the output; op 1 modulates nothing. Except for the
        // bass drum, where op 1 is silent even in additive mode.
        let co = sy.op_sample(c, 0, am_units);
        if drop_op1 {
            co
        } else {
            mo + co
        }
    } else {
        // Op 1's output *is* op 2's phase offset. This is the FM.
        sy.op_sample(c, mo, am_units)
    }
}

/// One chip sample, summed across all nine channels.
unsafe fn chip_sample(sy: &mut Synth) -> f32 {
    sy.tick_globals();
    let am_units = sy.am_units();
    let rhythm = rhythm_enabled();
    let mut acc = 0i32;

    for ch in 0..NUM_CH {
        // In rhythm mode the last three channels are not channels any more.
        if rhythm && ch >= 6 {
            continue;
        }
        acc += melodic_channel(sy, ch, am_units, false);
    }

    if rhythm {
        acc += rhythm_sample(sy, am_units);
    }

    // Nine channels of +-4084. Scale so one loud voice sits near unity.
    let v = acc as f32 / 8192.0;
    sy.dc.tick(v)
}

/// The five percussion voices.
///
/// The bass drum is still an ordinary 2-operator channel and the tom-tom an
/// ordinary single operator — both are simply pitched drums. The hi-hat, snare
/// and cymbal are the chip's *noise* voices: their phase index is not their own
/// accumulator at all but a value mangled together out of bits of the hi-hat's
/// and the cymbal's accumulators and the noise LFSR. That mangling is what gives
/// them their metallic character, and it is why they cannot be approximated with
/// a noise generator bolted onto a sine.
unsafe fn rhythm_sample(sy: &mut Synth, am_units: i32) -> i32 {
    // Bass drum: channel 6, ordinary FM — except that its operator 1 is silent
    // even in additive mode.
    let mut acc = melodic_channel(sy, 6, am_units, true);

    let hh = CH_OPS[7].0;
    let sd = CH_OPS[7].1;
    let tt = CH_OPS[8].0;
    let cy = CH_OPS[8].1;
    for &op in &[hh, sd, tt, cy] {
        sy.eg_advance(op);
        sy.phase_advance(op);
    }

    // None of the four take a modulation input, and each is summed twice.
    let noise = (sy.noise & 1) as i32;
    let h = (sy.ops[hh].phase >> 10) as i32;
    let t = (sy.ops[cy].phase >> 10) as i32;
    let bit = |v: i32, n: u32| (v >> n) & 1;

    let rm_xor = (bit(h, 2) ^ bit(h, 7)) | (bit(h, 3) ^ bit(t, 5)) | (bit(t, 3) ^ bit(t, 5));

    let hh_idx = ((rm_xor << 9) | if rm_xor ^ noise != 0 { 0xD0 } else { 0x34 }) as u32;
    let sd_idx = ((bit(h, 8) << 9) | ((bit(h, 8) ^ noise) << 8)) as u32;
    let cy_idx = ((rm_xor << 9) | 0x80) as u32;

    acc += 2 * sy.op_sample_at(hh, hh_idx, am_units);
    acc += 2 * sy.op_sample_at(sd, sd_idx, am_units);
    acc += 2 * sy.op_sample(tt, 0, am_units);
    acc += 2 * sy.op_sample_at(cy, cy_idx, am_units);
    acc
}

/// Add this chip's contribution to an interleaved-stereo buffer. The OPL2 is
/// mono — there is one output pin — so both channels get the same signal.
pub unsafe fn render(buf: &mut [f32]) {
    if (*core::ptr::addr_of!(SYNTH)).is_none() {
        return;
    }
    let sy = synth();
    let step = CHIP_HZ / super::SAMPLE_RATE as f64;
    let frames = buf.len() / 2;
    for i in 0..frames {
        // Resample 49716 -> 48000 by running the chip forward and interpolating.
        // The ratio is close to 1, so this is a fraction of a sample of error and
        // never more than one chip step per output frame.
        sy.resample_acc += step;
        while sy.resample_acc >= 1.0 {
            sy.resample_acc -= 1.0;
            sy.prev = sy.cur;
            sy.cur = chip_sample(sy);
        }
        let t = sy.resample_acc as f32;
        let v = sy.prev + (sy.cur - sy.prev) * (1.0 - t);
        let out = v * 0.55;
        buf[i * 2] += out;
        buf[i * 2 + 1] += out;
    }
}

extern "C" fn opl2_register() {
    io_bus_register(core::ptr::addr_of!(OPL2_DEVICE));
}

// Run opl2_register at process start (the C `__attribute__((constructor))`).
#[used]
#[link_section = ".init_array"]
static OPL2_CTOR: extern "C" fn() = opl2_register;

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::{Mutex, MutexGuard};

    /// There is exactly one chip, and it is a global — the same one the guest
    /// drives. Rust runs tests in parallel threads, so they have to take turns
    /// with it or they simply overwrite each other's notes.
    static CHIP_LOCK: Mutex<()> = Mutex::new(());

    pub(crate) fn claim_chip() -> MutexGuard<'static, ()> {
        // A panicking test poisons the lock; the next one still wants the chip.
        CHIP_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Drive the chip exactly as a game does: address byte, then data byte.
    pub(super) unsafe fn w(r: u8, v: u8) {
        opl2_port_write(0x388, r);
        opl2_port_write(0x389, v);
    }

    /// A chip fresh out of reset: no registers, no derived state.
    pub(super) unsafe fn fresh() {
        (*s()).registers = [0; 256];
        (*s()).address = 0;
        synth_reset();
    }

    /// Magnitude at `freq`, by Goertzel. Enough to ask "is the note there".
    fn tone_at(buf: &[f32], freq: f64) -> f64 {
        let n = buf.len() / 2;
        let k = 2.0 * (core::f64::consts::TAU * freq / super::super::SAMPLE_RATE as f64).cos();
        let (mut s1, mut s2) = (0.0f64, 0.0f64);
        for i in 0..n {
            let s0 = buf[i * 2] as f64 + k * s1 - s2;
            s2 = s1;
            s1 = s0;
        }
        (s1 * s1 + s2 * s2 - k * s1 * s2).max(0.0).sqrt() / n as f64
    }

    /// Program channel 0 as a single audible sine: additive connection with
    /// operator 1 fully attenuated, so only the carrier is heard and it is
    /// unmodulated. F-Num 580 at block 4 is concert A — see the datasheet's
    /// F = fnum * 49716 / 2^(20-block).
    unsafe fn key_a440() {
        fresh();
        w(0x01, 0x20); // waveform select enabled
        w(0x20, 0x21); // op1: sustaining, mult = 1
        w(0x23, 0x21); // op2: sustaining, mult = 1
        w(0x40, 0x3F); // op1: fully attenuated -> silent
        w(0x43, 0x00); // op2: loudest
        w(0x60, 0xF0); // op1: fastest attack, no decay
        w(0x63, 0xF0); // op2: same
        w(0x80, 0x00); // op1: sustain at full, slowest release
        w(0x83, 0x00); // op2: same
        w(0xC0, 0x01); // additive, no feedback
        w(0xA0, 0x44); // F-Num low  (580 = 0x244)
        w(0xB0, 0x32); // key on | block 4 | F-Num high
    }

    pub(super) fn render_frames(n: usize) -> Vec<f32> {
        let mut buf = vec![0.0f32; n * 2];
        unsafe { render(&mut buf) };
        buf
    }

    /// All nine channels sounding, FM, with feedback — the worst case a game can
    /// actually put the chip in.
    unsafe fn nine_voices() {
        fresh();
        w(0x01, 0x20);
        for ch in 0..NUM_CH {
            let (m, c) = CH_OPS[ch];
            for op in [m, c] {
                let off = OP_OFFSET[op] as u8;
                w(0x20 + off, 0x21); // sustaining, mult 1
                w(0x40 + off, 0x08);
                w(0x60 + off, 0xF4);
                w(0x80 + off, 0x24);
                w(0xE0 + off, (op & 3) as u8); // exercise all four waveforms
            }
            w(0xC0 + ch as u8, 0x0E); // feedback 7, FM
            w(0xA0 + ch as u8, 0x44 + ch as u8);
            w(0xB0 + ch as u8, 0x32);
        }
    }

    /// The chip renders on the *guest thread*, out of the time the guest is not
    /// using. Every microsecond spent here is one the emulated CPU does not get,
    /// and when the guest falls behind real time the audio queue starves and the
    /// sound crackles. So this is a correctness bound, not a nicety.
    ///
    /// Only meaningful against optimised code — an unoptimised chip is slow and
    /// says nothing about the one that ships. `cargo test --release`.
    #[test]
    #[cfg_attr(
        debug_assertions,
        ignore = "measures optimised code; run with --release"
    )]
    fn a_second_of_audio_costs_a_small_fraction_of_a_second() {
        let _chip = claim_chip();
        unsafe { nine_voices() };
        let mut buf = vec![0.0f32; super::super::SAMPLE_RATE as usize * 2];

        unsafe { render(&mut buf) }; // warm
        let t = std::time::Instant::now();
        unsafe { render(&mut buf) };
        let secs = t.elapsed().as_secs_f64();

        eprintln!(
            "OPL2: 1.0s of audio in {:.1}ms = {:.1}% of realtime",
            secs * 1000.0,
            secs * 100.0
        );
        assert!(
            secs < 0.06,
            "OPL2 render costs {:.1}% of realtime; that is taken from the guest, \
             which then falls behind and starves the audio queue",
            secs * 100.0
        );
    }

    #[test]
    fn silent_until_keyed() {
        let _chip = claim_chip();
        unsafe { fresh() };
        let buf = render_frames(2048);
        let peak = buf.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        assert!(peak < 1.0e-6, "chip made sound with nothing keyed: {peak}");
    }

    #[test]
    fn keyed_note_sounds_at_its_programmed_pitch() {
        let _chip = claim_chip();
        unsafe { key_a440() };
        let buf = render_frames(8192);

        let peak = buf.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        assert!(peak > 0.05, "keyed note is inaudible (peak {peak})");

        // The fundamental must dominate its neighbours by a wide margin — that
        // is what distinguishes a note from noise or from a mistuned one.
        let f0 = tone_at(&buf, 440.0);
        for off in [300.0, 370.0, 520.0, 660.0] {
            let other = tone_at(&buf, off);
            assert!(
                f0 > other * 4.0,
                "440Hz ({f0:.5}) does not dominate {off}Hz ({other:.5})"
            );
        }
    }

    #[test]
    fn block_is_an_octave() {
        let _chip = claim_chip();
        // The same F-Num one block up is the same note one octave up: the block
        // is a power-of-two shift on the phase increment, nothing more.
        unsafe {
            key_a440();
            w(0xB0, 0x36); // block 5, still keyed
        }
        let buf = render_frames(8192);
        let up = tone_at(&buf, 880.0);
        let orig = tone_at(&buf, 440.0);
        assert!(
            up > orig * 4.0,
            "block+1 should be an octave up: {up} vs {orig}"
        );
    }

    #[test]
    fn key_off_releases_the_envelope() {
        let _chip = claim_chip();
        unsafe { key_a440() };
        let held = render_frames(4096);
        let held_peak = held.iter().fold(0.0f32, |m, v| m.max(v.abs()));

        unsafe {
            w(0x83, 0x0F); // op2: fastest release
            w(0xB0, 0x12); // key off (block 4 retained)
        }
        // Skip the release itself, then measure what is left.
        let _ = render_frames(4096);
        let after = render_frames(4096);
        let after_peak = after.iter().fold(0.0f32, |m, v| m.max(v.abs()));

        assert!(held_peak > 0.05, "note never sounded ({held_peak})");
        assert!(
            after_peak < held_peak * 0.02,
            "note still ringing long after key-off: {after_peak} vs {held_peak}"
        );
    }

    #[test]
    fn total_level_attenuates() {
        let _chip = claim_chip();
        unsafe { key_a440() };
        let loud = tone_at(&render_frames(8192), 440.0);
        unsafe { w(0x43, 0x10) }; // TL = 16 -> 12dB down
        let quiet = tone_at(&render_frames(8192), 440.0);
        let ratio = quiet / loud;
        // 12dB is a factor of ~4. Allow generous slack for the envelope's own
        // quantisation; the point is that it is a large, monotone drop.
        assert!(
            ratio > 0.15 && ratio < 0.35,
            "TL=16 should be ~12dB down (0.25x), got {ratio:.3}"
        );
    }

    #[test]
    fn fm_adds_harmonics_that_additive_does_not() {
        let _chip = claim_chip();
        // The whole point of the chip: route op1 into op2's phase and the carrier
        // sprouts sidebands. Same operators, same note — only the connection bit
        // and op1's level change.
        unsafe {
            key_a440();
            w(0x40, 0x00); // op1 audible now
            w(0xC0, 0x00); // FM: op1 modulates op2
        }
        let fm = render_frames(8192);
        let fm_h2 = tone_at(&fm, 880.0) / tone_at(&fm, 440.0);

        unsafe {
            key_a440(); // back to additive with op1 silent -> a pure sine
        }
        let clean = render_frames(8192);
        let clean_h2 = tone_at(&clean, 880.0) / tone_at(&clean, 440.0);

        assert!(
            fm_h2 > clean_h2 * 5.0,
            "FM should be far richer than a bare sine: {fm_h2:.4} vs {clean_h2:.4}"
        );
    }
}

#[cfg(test)]
mod bd_register_tests {
    use super::tests::{claim_chip, fresh, render_frames, w};

    /// Register 0xBD is two registers sharing one address: the rhythm section's
    /// enable and keys, and the chip-wide tremolo/vibrato depths. A melodic driver
    /// writes it for the depth bits alone — and must not lose its notes for it.
    ///
    /// This is not hypothetical: a driver writes 0xBD when it restarts its music,
    /// and treating that as a key update silenced a third of the chip.
    #[test]
    fn setting_the_vibrato_depth_does_not_silence_channels_6_to_8() {
        let _chip = claim_chip();
        unsafe {
            fresh();
            w(0x01, 0x20);
            // A sustaining note on channel 6 — a melodic channel while rhythm is off.
            for off in [0x10u8, 0x13] {
                w(0x20 + off, 0x21);
                w(0x40 + off, 0x00);
                w(0x60 + off, 0xF0);
                w(0x80 + off, 0x00);
            }
            w(0xC0 + 6, 0x01); // additive
            w(0xA6, 0x44);
            w(0xB6, 0x32); // key on, block 4
        }
        let before = render_frames(4096)
            .iter()
            .fold(0.0f32, |m, v| m.max(v.abs()));
        assert!(before > 0.02, "channel 6 never sounded ({before})");

        // Deep vibrato + deep tremolo, rhythm mode OFF. This is a depth setting,
        // not a key-off.
        unsafe { w(0xBD, 0xC0) };
        let after = render_frames(4096)
            .iter()
            .fold(0.0f32, |m, v| m.max(v.abs()));
        assert!(
            after > before * 0.5,
            "a vibrato-depth write killed the note on channel 6: {before} -> {after}"
        );
    }

    /// And leaving rhythm mode must hand channels 6-8 back to whatever their own
    /// key bits say, rather than leaving them stuck silent.
    #[test]
    fn leaving_rhythm_mode_restores_the_melodic_channels() {
        let _chip = claim_chip();
        unsafe {
            fresh();
            w(0x01, 0x20);
            for off in [0x10u8, 0x13] {
                w(0x20 + off, 0x21);
                w(0x40 + off, 0x00);
                w(0x60 + off, 0xF0);
                w(0x80 + off, 0x00);
            }
            w(0xC0 + 6, 0x01);
            w(0xA6, 0x44);
            w(0xBD, 0x20); // rhythm mode ON: channel 6 is the bass drum now
            w(0xB6, 0x32); // key bit set, but rhythm owns the channel
            w(0xBD, 0x00); // rhythm OFF again -> channel 6 is melodic and keyed
        }
        let peak = render_frames(4096)
            .iter()
            .fold(0.0f32, |m, v| m.max(v.abs()));
        assert!(
            peak > 0.02,
            "channel 6 stayed silent after leaving rhythm mode ({peak})"
        );
    }
}
