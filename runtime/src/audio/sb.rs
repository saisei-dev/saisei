//! The Sound Blaster DSP.
//!
//! What was here before answered exactly one thing — the reset handshake, so a
//! game's detection routine would find a card — and silently dropped every
//! command after it. A game would detect a Sound Blaster, configure itself for
//! one, and then play nothing at all through it for the rest of the session.
//!
//! The card has two halves and only one of them is new. The FM half *is* the
//! OPL2, at the card's own base address as well as AdLib's (see `opl2.rs`, which
//! claims 0x228/0x229 for exactly that reason). This file is the digitised half:
//! a DSP that takes commands on a byte port, and plays 8-bit samples that the
//! DMA controller feeds it out of guest memory.
//!
//! Playback is modelled as the card pulling: at its programmed sample rate it
//! asks the DMA controller for the next byte (`dma::pull`). When the block runs
//! out the card raises its interrupt — which is the *entire* mechanism by which
//! a game knows to queue the next one, and so the one thing that absolutely must
//! be right or a sound effect plays once and never again.

use super::dsp::OnePole;
use super::SAMPLE_RATE;
use crate::io_bus::{io_bus_register, IoDevice};
use core::ffi::c_char;

extern "C" {
    fn shim_mark_irq_pending(int_no: u8);
}

/// The card's DMA channel. 1 is the default on every Sound Blaster, and the only
/// one the 8-bit DSP can use.
const DMA_CHANNEL: usize = 1;

/// The card's interrupt. IRQ 7 is the factory default; a game that wants another
/// asks the mixer for it (SB Pro, mixer register 0x80).
const DEFAULT_IRQ: u8 = 7;

#[derive(Clone, Copy, PartialEq, Debug)]
enum Mode {
    Idle,
    /// A single block, then an interrupt and stop.
    Single,
    /// Blocks forever, an interrupt at the end of each — the DMA controller's
    /// auto-init reloads underneath us.
    AutoInit,
}

pub struct Sb {
    /// The command byte we are collecting arguments for, and the arguments.
    cmd: Option<u8>,
    args: Vec<u8>,

    reset_latch: bool,
    /// The byte a read of the data port returns, and whether one is waiting.
    out: u8,
    out_ready: bool,
    /// 0xE1 returns two bytes, so one pending byte is not enough.
    out_queue: Vec<u8>,

    rate_hz: u32,
    block_len: u16,
    mode: Mode,
    paused: bool,
    speaker_on: bool,
    irq: u8,
    /// Raised at end-of-block, cleared when the game acknowledges by reading the
    /// status port (0x22E). A real card holds the line until it is read.
    irq_pending: bool,

    /// Sub-sample accumulator: how much of the next sample period has elapsed.
    acc: f64,
    /// The sample currently being held between DMA pulls.
    level: f32,
    lpf: OnePole,

    mixer_addr: u8,
    mixer_regs: [u8; 256],
}

static mut SB: Option<Sb> = None;

// ---- snapshot block (see devices.rs) ---------------------------------------
//
// The mixer registers and the sample rate are set once, when the game finds the
// card; the playback mode and the pending IRQ are what a sound in progress
// consists of. `acc`/`level`/`lpf` are the render side — a fraction of one
// sample period and the DAC's own filter — and are deliberately left out: they
// re-derive within a sample of resuming, and they are not state the guest can
// name. The command being assembled (`cmd`/`args`) travels because a save can
// land between a DSP command byte and its arguments.

/// Bounds for the two Vec fields. A DSP command takes at most a handful of
/// argument bytes and `0xE1` is the longest reply; anything past this is a bug,
/// not a state we need to carry.
const SNAP_ARGS_MAX: usize = 8;

#[repr(C)]
#[derive(Clone, Copy)]
struct SbSnap {
    /// 0 = no command in progress, else `cmd + 1`.
    cmd_plus1: u16,
    args_len: u8,
    args: [u8; SNAP_ARGS_MAX],
    reset_latch: u8,
    out: u8,
    out_ready: u8,
    out_queue_len: u8,
    out_queue: [u8; SNAP_ARGS_MAX],
    rate_hz: u32,
    block_len: u16,
    /// 0 = Idle, 1 = Single, 2 = AutoInit.
    mode: u8,
    paused: u8,
    speaker_on: u8,
    irq: u8,
    irq_pending: u8,
    mixer_addr: u8,
    mixer_regs: [u8; 256],
}

fn mode_to_u8(m: Mode) -> u8 {
    match m {
        Mode::Idle => 0,
        Mode::Single => 1,
        Mode::AutoInit => 2,
    }
}

fn mode_from_u8(v: u8) -> Mode {
    match v {
        1 => Mode::Single,
        2 => Mode::AutoInit,
        _ => Mode::Idle,
    }
}

pub(crate) unsafe fn state_capture(out_buf: &mut Vec<u8>) {
    let c = sb();
    // Zeroed, not a struct literal: `pod_capture` copies the struct's bytes and
    // a literal leaves any padding undefined, so two captures of the same card
    // would not compare equal. See devices::pod_capture.
    let mut s: SbSnap = core::mem::zeroed();
    s.cmd_plus1 = c.cmd.map_or(0, |b| b as u16 + 1);
    s.reset_latch = c.reset_latch as u8;
    s.out = c.out;
    s.out_ready = c.out_ready as u8;
    s.rate_hz = c.rate_hz;
    s.block_len = c.block_len;
    s.mode = mode_to_u8(c.mode);
    s.paused = c.paused as u8;
    s.speaker_on = c.speaker_on as u8;
    s.irq = c.irq;
    s.irq_pending = c.irq_pending as u8;
    s.mixer_addr = c.mixer_addr;
    s.mixer_regs = c.mixer_regs;
    let n = c.args.len().min(SNAP_ARGS_MAX);
    s.args_len = n as u8;
    s.args[..n].copy_from_slice(&c.args[..n]);
    let n = c.out_queue.len().min(SNAP_ARGS_MAX);
    s.out_queue_len = n as u8;
    s.out_queue[..n].copy_from_slice(&c.out_queue[..n]);
    crate::devices::pod_capture(&s, out_buf);
}

pub(crate) unsafe fn state_restore(b: &[u8]) -> bool {
    let s = match crate::devices::pod_restore::<SbSnap>(b) {
        Some(s) => s,
        None => return false,
    };
    reset();
    let c = sb();
    c.cmd = if s.cmd_plus1 == 0 {
        None
    } else {
        Some((s.cmd_plus1 - 1) as u8)
    };
    c.args = s.args[..(s.args_len as usize).min(SNAP_ARGS_MAX)].to_vec();
    c.reset_latch = s.reset_latch != 0;
    c.out = s.out;
    c.out_ready = s.out_ready != 0;
    c.out_queue = s.out_queue[..(s.out_queue_len as usize).min(SNAP_ARGS_MAX)].to_vec();
    c.rate_hz = s.rate_hz;
    c.block_len = s.block_len;
    c.mode = mode_from_u8(s.mode);
    c.paused = s.paused != 0;
    c.speaker_on = s.speaker_on != 0;
    c.irq = s.irq;
    c.irq_pending = s.irq_pending != 0;
    c.mixer_addr = s.mixer_addr;
    c.mixer_regs = s.mixer_regs;
    true
}

unsafe fn sb() -> &'static mut Sb {
    if (*core::ptr::addr_of!(SB)).is_none() {
        reset();
    }
    (*core::ptr::addr_of_mut!(SB)).as_mut().unwrap()
}

pub unsafe fn reset() {
    SB = Some(Sb {
        cmd: None,
        args: Vec::new(),
        reset_latch: false,
        out: 0xAA,
        out_ready: false,
        out_queue: Vec::new(),
        rate_hz: 11025,
        block_len: 0,
        mode: Mode::Idle,
        paused: false,
        speaker_on: false,
        irq: DEFAULT_IRQ,
        irq_pending: false,
        acc: 0.0,
        level: 0.0,
        // The card's own output filter. A real SB is not a clean DAC and its
        // samples are 8-bit at 11kHz; without this they arrive as hiss.
        lpf: OnePole::new(7000.0),
        mixer_addr: 0,
        mixer_regs: [0; 256],
    });
}

impl Sb {
    fn push_out(&mut self, bytes: &[u8]) {
        self.out_queue.extend_from_slice(bytes);
        if let Some(b) = self.out_queue.first().copied() {
            self.out = b;
            self.out_ready = true;
        }
    }

    fn take_out(&mut self) -> u8 {
        let v = self.out;
        if !self.out_queue.is_empty() {
            self.out_queue.remove(0);
        }
        match self.out_queue.first().copied() {
            Some(b) => {
                self.out = b;
                self.out_ready = true;
            }
            None => self.out_ready = false,
        }
        v
    }

    /// How many argument bytes a command still needs before it can run.
    fn arg_count(cmd: u8) -> usize {
        match cmd {
            0x10 => 1,               // direct DAC: the sample
            0x14 | 0x16 | 0x48 => 2, // block length - 1, low then high
            0x40 => 1,               // time constant
            0x41 => 2,               // sample rate, high then low
            0x80 => 2,               // silence period
            _ => 0,
        }
    }
}

unsafe fn run_command(s: &mut Sb, cmd: u8, args: &[u8]) {
    match cmd {
        // Direct DAC: the CPU hands over one sample. Slow, but some games do it.
        0x10 => {
            s.level = (args[0] as f32 - 128.0) / 128.0;
        }
        // 8-bit DMA playback, single block.
        0x14 | 0x16 => {
            s.block_len = args[0] as u16 | ((args[1] as u16) << 8);
            s.mode = Mode::Single;
            s.paused = false;
        }
        // 8-bit DMA playback, auto-init: the block size came earlier, via 0x48.
        0x1C | 0x1F => {
            s.mode = Mode::AutoInit;
            s.paused = false;
        }
        // The time constant is not a rate — it is what you divide 1MHz by.
        0x40 => {
            let tc = args[0] as u32;
            s.rate_hz = (1_000_000 / (256 - tc.min(255))).clamp(4000, 48000);
        }
        // SB16 sets the rate directly, big-endian.
        0x41 | 0x42 => {
            let r = ((args[0] as u32) << 8) | args[1] as u32;
            s.rate_hz = r.clamp(4000, 48000);
        }
        0x48 => {
            s.block_len = args[0] as u16 | ((args[1] as u16) << 8);
        }
        // Silence for a while. We have nothing to play, so this is just a stop.
        0x80 => {
            s.level = 0.0;
        }
        0xD0 => s.paused = true,
        0xD1 => s.speaker_on = true,
        0xD3 => s.speaker_on = false,
        0xD4 => s.paused = false,
        // Stop auto-init at the end of the current block.
        0xDA => s.mode = Mode::Idle,
        0xD8 => s.push_out(&[if s.speaker_on { 0xFF } else { 0x00 }]),
        // DSP version. 3.01 — a Sound Blaster Pro, which is what the games in
        // this corpus that offer a Blaster at all were written for.
        0xE1 => s.push_out(&[0x03, 0x01]),
        // Identification: the card echoes the complement.
        0xE0 => {
            if let Some(&a) = args.first() {
                s.push_out(&[!a]);
            }
        }
        // Force an interrupt. This is how a driver finds out which IRQ it is on:
        // it points a handler at each candidate and asks the card to fire.
        0xF2 => raise_irq(s),
        _ => {}
    }
}

unsafe fn raise_irq(s: &mut Sb) {
    s.irq_pending = true;
    // Vector = 8 + IRQ for the master PIC.
    shim_mark_irq_pending(0x08 + s.irq);
}

// ---- the ports ---------------------------------------------------------------

extern "C" fn sb_write(port: u16, value: u8) {
    unsafe {
        super::catchup();
        let s = sb();
        match port & 0x0F {
            // Mixer.
            0x4 => s.mixer_addr = value,
            0x5 => {
                s.mixer_regs[s.mixer_addr as usize] = value;
                // Mixer register 0x80 selects the interrupt: bit0=IRQ2, bit1=IRQ5,
                // bit2=IRQ7, bit3=IRQ10. A game that reconfigures the card and then
                // gets its interrupts on the old line hears one buffer and stops.
                if s.mixer_addr == 0x80 {
                    s.irq = match value & 0x0F {
                        0x01 => 2,
                        0x02 => 5,
                        0x08 => 10,
                        _ => 7,
                    };
                }
            }
            // Reset: 1 then 0. The card answers 0xAA to say it is there.
            0x6 => {
                if value & 1 != 0 {
                    s.reset_latch = true;
                } else if s.reset_latch {
                    let irq = s.irq;
                    reset();
                    let s = sb();
                    s.irq = irq;
                    s.push_out(&[0xAA]);
                }
            }
            // Command / data.
            0xC => match s.cmd {
                Some(cmd) => {
                    s.args.push(value);
                    if s.args.len() >= Sb::arg_count(cmd) {
                        let args = core::mem::take(&mut s.args);
                        s.cmd = None;
                        run_command(s, cmd, &args);
                    }
                }
                None => {
                    if Sb::arg_count(value) == 0 {
                        run_command(s, value, &[]);
                    } else {
                        s.cmd = Some(value);
                        s.args.clear();
                    }
                }
            },
            _ => {}
        }
    }
}

extern "C" fn sb_read(port: u16) -> u8 {
    unsafe {
        let s = sb();
        match port & 0x0F {
            0x5 => s.mixer_regs[s.mixer_addr as usize],
            // Read data.
            0xA => s.take_out(),
            // Write-buffer status: bit 7 clear means "ready for a command". We are
            // always ready.
            0xC => 0x00,
            // Read-buffer status: bit 7 set means a byte is waiting.
            0xE => {
                // Reading this port is also how a game acknowledges the card's
                // interrupt. Hold the line until it does, exactly as the hardware
                // does — acknowledge it early and a driver that polls here to find
                // out whether the block finished never sees that it did.
                s.irq_pending = false;
                if s.out_ready {
                    0x80
                } else {
                    0x00
                }
            }
            _ => 0xFF,
        }
    }
}

/// The card's whole decode range, because an unclaimed port is not "ignored" on
/// this bus — it reaches `io_port_error`, which calls `exit(1)`. The ports we do
/// not model must still be *answered*, and answered the way an absent register
/// answers: 0xFF on a read, nothing on a write.
///
/// 0x228/0x229 are deliberately absent from this list: they are the OPL2 at the
/// card's base address, and `opl2.rs` claims them.
static SB_PORTS: [u16; 15] = [
    0x220, 0x221, 0x222, 0x223, 0x224, 0x225, 0x226, 0x227, 0x22A, 0x22B, 0x22C, 0x22D, 0x22E,
    0x22F, 0xFFFF,
];
static SB_DEVICE: IoDevice = IoDevice {
    name: b"sb\0".as_ptr() as *const c_char,
    ports: SB_PORTS.as_ptr(),
    read8: Some(sb_read),
    write8: Some(sb_write),
};

extern "C" fn sb_register() {
    io_bus_register(core::ptr::addr_of!(SB_DEVICE));
}

#[used]
#[link_section = ".init_array"]
static SB_CTOR: extern "C" fn() = sb_register;

// ---- rendering ---------------------------------------------------------------

/// Add the card's digitised output.
///
/// The card pulls a byte from the DMA controller every `1/rate_hz` seconds and
/// holds it until the next one. That hold is not laziness — it is what an 8-bit
/// DAC does, and it is why the output needs the lowpass afterwards rather than
/// arriving as a clean band-limited signal.
pub unsafe fn render(buf: &mut [f32]) {
    if (*core::ptr::addr_of!(SB)).is_none() {
        return;
    }
    let s = sb();
    let frames = buf.len() / 2;
    let step = s.rate_hz as f64 / SAMPLE_RATE as f64;

    for i in 0..frames {
        let playing = s.mode != Mode::Idle && !s.paused && super::dma::armed(DMA_CHANNEL);
        if playing {
            s.acc += step;
            while s.acc >= 1.0 {
                s.acc -= 1.0;
                match super::dma::pull(DMA_CHANNEL) {
                    Some((byte, tc)) => {
                        // 8-bit DMA samples are *unsigned*: 128 is silence.
                        s.level = (byte as f32 - 128.0) / 128.0;
                        if tc {
                            raise_irq(s);
                            if s.mode == Mode::Single {
                                s.mode = Mode::Idle;
                            }
                        }
                    }
                    None => {
                        s.mode = Mode::Idle;
                        s.level = 0.0;
                        break;
                    }
                }
            }
        }

        // The speaker-enable gate is the card's output switch. With it off the DAC
        // still runs; you just cannot hear it.
        let v = if s.speaker_on || s.mode != Mode::Idle {
            s.lpf.tick(s.level)
        } else {
            s.lpf.tick(0.0)
        };
        let out = v * 0.6;
        buf[i * 2] += out;
        buf[i * 2 + 1] += out;
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::{Mutex, MutexGuard};

    static LOCK: Mutex<()> = Mutex::new(());
    pub(crate) fn claim() -> MutexGuard<'static, ()> {
        LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    unsafe fn dsp(v: u8) {
        sb_write(0x22C, v);
    }

    /// Program DMA channel 1 for a block at `phys`, `len` bytes, auto-init off.
    unsafe fn arm_dma(phys: u32, len: u16, auto: bool) {
        let dw = |p: u16, v: u8| crate::audio::dma::test_write(p, v);
        dw(0x0C, 0); // clear the flip-flop
        dw(0x0B, if auto { 0x59 } else { 0x49 }); // ch1, read from memory
        dw(0x02, (phys & 0xFF) as u8);
        dw(0x02, ((phys >> 8) & 0xFF) as u8);
        dw(0x83, ((phys >> 16) & 0xFF) as u8); // page register for channel 1
        dw(0x03, ((len - 1) & 0xFF) as u8);
        dw(0x03, (((len - 1) >> 8) & 0xFF) as u8);
        dw(0x0A, 0x01); // unmask channel 1
    }

    /// Put a ramp in guest memory and return where it is.
    unsafe fn write_guest(phys: u32, bytes: &[u8]) {
        for (i, b) in bytes.iter().enumerate() {
            crate::shims::phys_write_byte(phys + i as u32, *b);
        }
    }

    #[test]
    fn reset_handshake_identifies_the_card() {
        let _g = claim();
        unsafe {
            reset();
            sb_write(0x226, 1);
            sb_write(0x226, 0);
            assert_eq!(sb_read(0x22E) & 0x80, 0x80, "a byte should be waiting");
            assert_eq!(sb_read(0x22A), 0xAA, "the card must answer 0xAA");
        }
    }

    #[test]
    fn the_time_constant_sets_the_sample_rate() {
        let _g = claim();
        unsafe {
            reset();
            dsp(0x40);
            dsp(165); // 1_000_000 / (256 - 165) = 10989
            assert_eq!(sb().rate_hz, 10989);
        }
    }

    #[test]
    fn dsp_version_reports_a_sound_blaster_pro() {
        let _g = claim();
        unsafe {
            reset();
            dsp(0xE1);
            assert_eq!(sb_read(0x22A), 0x03);
            assert_eq!(sb_read(0x22A), 0x01);
        }
    }

    /// The whole point of the card: a block of memory becomes sound, with the CPU
    /// uninvolved, and an interrupt at the end so the game knows to send more.
    #[test]
    fn a_dma_block_plays_and_then_raises_the_interrupt() {
        let _g = claim();
        unsafe {
            crate::shims::shim_test_init_memory();
            reset();
            crate::audio::dma::reset();

            // A full-scale square, so the output is unmistakable.
            let phys = 0x2000u32;
            let block: Vec<u8> = (0..64)
                .map(|i| if (i / 8) % 2 == 0 { 255u8 } else { 0u8 })
                .collect();
            write_guest(phys, &block);
            arm_dma(phys, block.len() as u16, false);

            dsp(0xD1); // speaker on
            dsp(0x40);
            dsp(0xE7); // ~4kHz, so 64 samples span a good stretch of buffer
            dsp(0x14);
            dsp(((block.len() - 1) & 0xFF) as u8);
            dsp((((block.len() - 1) >> 8) & 0xFF) as u8);

            let mut buf = vec![0.0f32; 4096 * 2];
            render(&mut buf);

            let peak = buf.iter().fold(0.0f32, |m, v| m.max(v.abs()));
            assert!(peak > 0.1, "the DMA block produced no sound (peak {peak})");

            // Terminal count must have fired the card's interrupt — without it a
            // game plays one buffer and never queues another.
            assert!(sb().irq_pending, "end of block did not raise the IRQ");
            assert_eq!(sb().mode, Mode::Idle, "a single block must stop at the end");
        }
    }

    #[test]
    fn auto_init_keeps_playing_past_the_end_of_the_block() {
        let _g = claim();
        unsafe {
            crate::shims::shim_test_init_memory();
            reset();
            crate::audio::dma::reset();

            let phys = 0x3000u32;
            let block: Vec<u8> = (0..32).map(|i| if i % 2 == 0 { 255 } else { 0 }).collect();
            write_guest(phys, &block);
            arm_dma(phys, block.len() as u16, true);

            dsp(0xD1);
            dsp(0x40);
            dsp(0xE7);
            dsp(0x48);
            dsp(((block.len() - 1) & 0xFF) as u8);
            dsp((((block.len() - 1) >> 8) & 0xFF) as u8);
            dsp(0x1C); // auto-init

            let mut buf = vec![0.0f32; 8192 * 2];
            render(&mut buf);
            let peak = buf.iter().fold(0.0f32, |m, v| m.max(v.abs()));

            assert!(peak > 0.1, "auto-init produced no sound");
            // Still going: the DMA controller reloaded from base at terminal count
            // rather than stopping.
            assert_eq!(sb().mode, Mode::AutoInit, "auto-init must not stop itself");
            assert!(
                crate::audio::dma::armed(DMA_CHANNEL),
                "channel should still be armed"
            );
        }
    }

    #[test]
    fn the_mixer_can_move_the_card_to_another_irq() {
        let _g = claim();
        unsafe {
            reset();
            assert_eq!(sb().irq, 7);
            sb_write(0x224, 0x80); // mixer register: IRQ select
            sb_write(0x225, 0x02); // -> IRQ 5
            assert_eq!(sb().irq, 5);
        }
    }
}
