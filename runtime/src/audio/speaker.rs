//! The PC speaker — and the one place in this runtime where we deliberately do
//! not reproduce the hardware's *sound*.
//!
//! # The guest still sees a real 8254 and a real port 61h
//!
//! Nothing here is visible to the game. The gate, the speaker-data bit, the
//! channel-2 mode and divisor, and the output pin the guest reads back through
//! port 0x61 bit 5 all keep their exact hardware semantics, in `shims.rs`, and
//! this module only *reads* them. The prime directive — emulate faithfully — is
//! about the machine the program runs on. What we do with the resulting pin
//! waveform on the way to the speakers is a rendering choice, downstream of the
//! emulation, and it changes nothing the guest can observe or branch on.
//!
//! # Two things a game does with one pin
//!
//! The beeper carries audio two entirely different ways, and Prince of Persia
//! does both at once — its SETUP.CFG offers "Standard PC Internal Speaker" for
//! MIDI *and* for DIGITAL:
//!
//! * **Tones.** The CPU programs channel 2's divisor once and leaves the gate
//!   open. The pitch is `1193182 / divisor` and the *hardware* generates the
//!   waveform. This is music: note data, played on a square-wave oscillator.
//! * **PWM.** The CPU carries the waveform itself, toggling the speaker-data bit
//!   (or re-arming the counter) thousands of times a second. This is digitised
//!   sound — speech, samples — and the divisor means nothing.
//!
//! Tones we re-voice: the note stream is extracted and played on a soft synth
//! (below), so the game's melody comes out as an instrument instead of a buzzer.
//! PWM cannot be turned into notes — there are no notes — so it is reproduced
//! faithfully as PCM, band-limited and filtered so it is not harsh.
//!
//! # Telling them apart without guessing
//!
//! The distinction is not a heuristic about what the game "meant". It is
//! structural: **PWM must modulate something at the sample rate, and a held note
//! must not.** So the test is stability — has this exact (gate, enable, mode,
//! divisor) tuple been left alone long enough to *be* a note?
//!
//! Answering that at the moment of the write would require predicting the
//! future, and guessing is exactly what we refuse to do. So we don't: the
//! speaker renders `LOOKAHEAD_NS` **behind** the rest of the mixer. Writes land
//! in a timestamped queue, and by the time the renderer reaches a segment it can
//! already see what came after it, and *knows* whether the tuple survived. A
//! note is recognised because it was held; a PWM step is recognised because the
//! next write was already on its way. No prediction, no detector, no fallback —
//! just a decision made late enough to be certain.
//!
//! The cost is 3ms of latency on the speaker alone, which is inaudible, and
//! which buys a note onset with no square-wave blip in front of it.

use super::dsp::{sine, Adsr, DcBlock, OnePole, Reverb};
use super::SAMPLE_RATE;
use crate::shims::{pit_ch2_mode, pit_ch2_output_at, port61};
use crate::timer::pit_channel2;

/// The 8254's input clock.
const PIT_HZ: f64 = 1_193_182.0;

/// How long a tuple must survive to be a note rather than a PWM step. Three
/// milliseconds is longer than any PWM step (a 7kHz sample stream steps every
/// 140µs) and far shorter than any note.
const CONFIRM_NS: u64 = 3_000_000;

/// The renderer runs this far behind the mixer so that `CONFIRM_NS` of future is
/// already known when a segment is classified.
const LOOKAHEAD_NS: u64 = CONFIRM_NS;

/// The audible range. A "tone" outside it is not a note — it is a sub-sonic
/// rumble or an ultrasonic carrier — and makes no sound worth voicing.
const MIN_TONE_HZ: f64 = 20.0;
const MAX_TONE_HZ: f64 = 10_000.0;

/// Sub-samples per output frame for the PCM path. The pin is a 1-bit signal
/// whose edges land wherever they land; point-sampling it at 48kHz would fold
/// every edge into the audible band as hash. Box-filtering 8 sub-samples is the
/// cheap, correct answer, and it costs nothing when the speaker is off (the
/// common case short-circuits).
const OVERSAMPLE: usize = 8;

/// Voices in the pool. The speaker is monophonic — only one note ever sounds —
/// but a release tail has to be allowed to ring under the note that replaced it.
const VOICES: usize = 6;

// ---- what the guest has done to the speaker ---------------------------------

/// The speaker's complete input state. Two of these being equal means the pin's
/// behaviour is unchanged, which is the whole basis for the stability test.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Tuple {
    /// Port 61h bit 1 — the speaker-data enable. With this low the pin is dead.
    enable: bool,
    /// Port 61h bit 0 — channel 2's gate.
    gate: bool,
    mode: u8,
    reload: u32,
}

impl Tuple {
    unsafe fn live() -> Tuple {
        Tuple {
            enable: port61 & 2 != 0,
            gate: port61 & 1 != 0,
            mode: pit_ch2_mode,
            reload: if pit_channel2.reload != 0 {
                pit_channel2.reload
            } else {
                0x10000
            },
        }
    }

    /// The pitch this tuple would sound at, if it is a note at all.
    fn tone_hz(&self) -> Option<f64> {
        // Modes 2 and 3 both free-run the counter and so both put a periodic
        // waveform on the pin — mode 3 a square, mode 2 a narrow pulse — at the
        // same pitch. Mode 0 is a one-shot: it makes a single edge (the "wait
        // for the counter" idiom), which is a click, not a note.
        if !self.enable || !self.gate || (self.mode != 2 && self.mode != 3) {
            return None;
        }
        let hz = PIT_HZ / self.reload as f64;
        (MIN_TONE_HZ..=MAX_TONE_HZ).contains(&hz).then_some(hz)
    }
}

/// A write happened at `at_ns`, leaving the speaker in `tuple`.
#[derive(Clone, Copy)]
struct Event {
    at_ns: u64,
    tuple: Tuple,
}

/// Ring capacity. Only `LOOKAHEAD_NS` worth is ever in flight — even a 44kHz PWM
/// carrier puts fewer than 150 events in that window.
const RING: usize = 1024;

// ---- the beautified voice ---------------------------------------------------

/// One note of the soft synth the extracted note stream plays on.
struct Voice {
    phase: f32,
    /// The pitch we are sounding, and the pitch we are heading for. A small
    /// pitch change inside a held note glides rather than retriggering, which is
    /// what keeps a vibrato or a portamento sounding like one.
    freq: f32,
    target_freq: f32,
    env: Adsr,
    lpf: OnePole,
    /// Vibrato is delayed and faded in — applied from the first sample it would
    /// make every note sound seasick.
    age: f32,
    vib_phase: f32,
    gated: bool,
}

impl Voice {
    fn new() -> Self {
        Voice {
            phase: 0.0,
            freq: 440.0,
            target_freq: 440.0,
            env: Adsr::new(0.005, 0.22, 0.62, 0.16),
            lpf: OnePole::new(4000.0),
            age: 0.0,
            vib_phase: 0.0,
            gated: false,
        }
    }

    fn note_on(&mut self, hz: f32) {
        self.freq = hz;
        self.target_freq = hz;
        self.age = 0.0;
        self.vib_phase = 0.0;
        self.gated = true;
        // Brighter as the note climbs, so the top octave does not go dull and
        // the bottom does not turn to sand.
        self.lpf.set_cutoff((hz * 4.0 + 800.0).min(9000.0));
        self.env.retrigger();
    }

    fn glide_to(&mut self, hz: f32) {
        self.target_freq = hz;
        self.lpf.set_cutoff((hz * 4.0 + 800.0).min(9000.0));
    }

    fn note_off(&mut self) {
        self.gated = false;
        self.env.gate_off();
    }

    fn is_free(&self) -> bool {
        !self.gated && self.env.is_idle()
    }

    fn tick(&mut self) -> f32 {
        let amp = self.env.tick();
        if amp <= 0.0 && !self.gated {
            return 0.0;
        }
        let sr = SAMPLE_RATE as f32;

        // Glide: ~12ms to close the gap. Fast enough to read as the same note
        // bending, slow enough not to click.
        self.freq += (self.target_freq - self.freq) * 0.006;

        self.age += 1.0 / sr;
        let vib_depth = ((self.age - 0.14) / 0.12).clamp(0.0, 1.0) * 0.0035;
        self.vib_phase = (self.vib_phase + 5.5 / sr).fract();
        let f = self.freq * (1.0 + vib_depth * sine(self.vib_phase));

        self.phase = (self.phase + f / sr).fract();
        let p = self.phase;
        // A warm, slightly reedy tone: a fundamental with a few decaying
        // partials. Additive, so it is band-limited by construction — no
        // aliasing however high the melody goes.
        let mut v = 0.65 * sine(p);
        v += 0.22 * sine((p * 2.0).fract());
        v += 0.10 * sine((p * 3.0).fract());
        v += 0.05 * sine((p * 4.0).fract());

        self.lpf.tick(v) * amp
    }
}

// ---- module state -----------------------------------------------------------

struct Speaker {
    events: [Event; RING],
    head: usize,
    tail: usize,

    /// The segment the renderer is currently inside, and when it began.
    cur: Tuple,
    cur_since_ns: u64,
    /// Where the renderer has got to. Trails the mixer by `LOOKAHEAD_NS`.
    render_ns: u64,
    primed: bool,

    voices: [Voice; VOICES],
    /// The voice holding the note that is sounding now, if any.
    active: Option<usize>,
    sounding_hz: f32,

    /// The faithful pin path, for PWM/digitised output.
    pcm_dc: DcBlock,
    pcm_lpf: OnePole,
    /// Crossfades the PCM path out from under the synth when a note takes over.
    pcm_gain: f32,

    reverb: Reverb,
}

static mut SPK: Option<Speaker> = None;

unsafe fn spk() -> Option<&'static mut Speaker> {
    (*core::ptr::addr_of_mut!(SPK)).as_mut()
}

/// Build (or rebuild) the speaker's host-side state. Called when audio comes up
/// and after a snapshot restore — everything here is derived, so it is simply
/// re-derived from whatever the guest state now says.
pub unsafe fn reset() {
    let tuple = Tuple::live();
    let at = shim_now();
    SPK = Some(Speaker {
        events: [Event { at_ns: at, tuple }; RING],
        head: 0,
        tail: 0,
        cur: tuple,
        cur_since_ns: at,
        render_ns: at.saturating_sub(LOOKAHEAD_NS),
        primed: true,
        voices: [(); VOICES].map(|_| Voice::new()),
        active: None,
        sounding_hz: 0.0,
        pcm_dc: DcBlock::new(),
        pcm_lpf: OnePole::new(7000.0),
        pcm_gain: 1.0,
        reverb: Reverb::new(0.72, 0.35, 0.85),
    });
}

extern "C" {
    fn shim_virtual_now_ns() -> u64;
}

#[inline]
unsafe fn shim_now() -> u64 {
    shim_virtual_now_ns()
}

/// A write to 0x42 / 0x43 / 0x61 has just landed. The mixer has already caught
/// up to this instant (the port handler calls `audio::catchup()` first), so the
/// samples before the write are rendered against the old state and this event
/// takes effect exactly at its own timestamp.
pub unsafe fn on_port_write() {
    let Some(s) = spk() else { return };
    let tuple = Tuple::live();
    // Only a *change* is an event. Drivers re-write the same value constantly
    // (re-arming a gate that is already open); recording those would make every
    // held note look like it was being modulated.
    let last = if s.head == s.tail {
        s.cur
    } else {
        s.events[(s.head + RING - 1) % RING].tuple
    };
    if tuple == last {
        return;
    }
    let at = shim_now();
    let next = (s.head + 1) % RING;
    if next == s.tail {
        // Ring full: the renderer has fallen a long way behind (a JIT stall).
        // Drop the oldest rather than the newest — the newest is the one that
        // still matters.
        s.tail = (s.tail + 1) % RING;
    }
    s.events[s.head] = Event { at_ns: at, tuple };
    s.head = next;
}

/// The timestamp of the next queued event after the current segment, if the
/// renderer has already seen it.
fn peek_next(s: &Speaker) -> Option<u64> {
    (s.tail != s.head).then(|| s.events[s.tail].at_ns)
}

// ---- rendering --------------------------------------------------------------

/// Add the speaker's contribution to `buf` (interleaved stereo). Frame `i`
/// covers virtual time `t0 + i * dt_ns`; we evaluate `LOOKAHEAD_NS` behind that.
pub unsafe fn render(buf: &mut [f32], t0: u64, dt_ns: f64) {
    let Some(s) = spk() else { return };
    if !s.primed {
        return;
    }
    let frames = buf.len() / 2;
    for i in 0..frames {
        let ts = (t0 + (i as f64 * dt_ns) as u64).saturating_sub(LOOKAHEAD_NS);

        // The segment machinery only advances when virtual time does. It is
        // deliberately separate from the voices below, which tick on *sample*
        // time: while the guest is stopped (the overlay's fade-out tail) the
        // clock is frozen, and a note still has to be allowed to ring out.
        if ts >= s.render_ns {
            s.render_ns = ts;

            while let Some(at) = peek_next(s) {
                if at > ts {
                    break;
                }
                let ev = s.events[s.tail];
                s.tail = (s.tail + 1) % RING;
                s.cur = ev.tuple;
                s.cur_since_ns = ev.at_ns;
            }

            // Classify. Because the renderer is LOOKAHEAD_NS behind, every event
            // inside [cur_since, cur_since + CONFIRM_NS] has already been
            // recorded — so "no next event yet" *means* the tuple was held. It
            // is a fact about the past, not a prediction about the future.
            let held = match peek_next(s) {
                Some(next_at) => next_at.saturating_sub(s.cur_since_ns) >= CONFIRM_NS,
                None => true,
            };
            let note = if held { s.cur.tone_hz() } else { None };
            drive_notes(s, note);

            // The raw pin carries audio in exactly one case: an *unstable*
            // segment, where the guest is modulating the pin itself. Every
            // stable segment is either a note — which the synth voice has — or a
            // parked pin level, which on real hardware makes no sound once the
            // cone has settled. All a parked level contributes is the click of
            // getting there, and that click is precisely what we are here not to
            // reproduce.
            //
            // Keying this off `held` rather than off "is there a note" is also
            // what keeps note edges clean. Silence and notes are *both* stable,
            // so the pin path is already muted before a note starts and stays
            // muted through it — it never gets the chance to leak a slice of raw
            // square into the head or tail of a note.
            let want_pcm = if held { 0.0 } else { 1.0 };
            s.pcm_gain += (want_pcm - s.pcm_gain) * 0.25;
        }

        let mut voices = 0.0;
        for v in s.voices.iter_mut() {
            voices += v.tick();
        }
        // Only one note ever sounds; the rest of the pool is release tails. The
        // level is set so a note peaks around -8dBFS, leaving room for the FM
        // chip and the digitised path to sit alongside it without clipping.
        voices *= 0.42;

        // Always ticked, muted or not: a filter that is skipped while silent
        // keeps whatever state it last had, and the next sample it sees steps
        // away from it. That is a click at the end of every note.
        let pcm = pin_sample(s, ts, dt_ns);

        // Only the voices go to the room. Digitised speech through a reverb
        // sounds like digitised speech in a bathroom.
        let (wl, wr) = s.reverb.tick(voices);
        buf[i * 2] += voices + pcm + wl;
        buf[i * 2 + 1] += voices + pcm + wr;
    }
}

/// Turn the segment classification into note events on the voice pool.
fn drive_notes(s: &mut Speaker, note: Option<f64>) {
    match note {
        Some(hz) => {
            let hz = hz as f32;
            match s.active {
                Some(v) => {
                    // A small pitch move inside a sounding note is a bend, not a
                    // new note — that is what a vibrato is. A real interval is a
                    // new note, and gets a fresh attack.
                    let semitones = (hz / s.sounding_hz).log2().abs() * 12.0;
                    if semitones < 1.0 {
                        s.voices[v].glide_to(hz);
                    } else {
                        s.voices[v].note_off();
                        let n = alloc_voice(s);
                        s.voices[n].note_on(hz);
                        s.active = Some(n);
                    }
                }
                None => {
                    let n = alloc_voice(s);
                    s.voices[n].note_on(hz);
                    s.active = Some(n);
                }
            }
            s.sounding_hz = hz;
        }
        None => {
            if let Some(v) = s.active.take() {
                s.voices[v].note_off();
            }
        }
    }
}

/// Take a free voice, or steal the one furthest into its release.
fn alloc_voice(s: &mut Speaker) -> usize {
    if let Some(i) = s.voices.iter().position(|v| v.is_free()) {
        return i;
    }
    let mut worst = 0;
    let mut worst_level = f32::MAX;
    for (i, v) in s.voices.iter().enumerate() {
        if Some(i) == s.active {
            continue;
        }
        if v.env.level() < worst_level {
            worst_level = v.env.level();
            worst = i;
        }
    }
    worst
}

/// The faithful pin, box-filtered over the frame. This is the PWM/digitised
/// path: the audio lives in *when* the guest toggled the pin, so the waveform is
/// reproduced as-is — only band-limited, DC-blocked and gently lowpassed so that
/// it is not the ice-pick the bare hardware was.
unsafe fn pin_sample(s: &mut Speaker, ts: u64, dt_ns: f64) -> f32 {
    // The pin stays at 0..1 rather than being centred here: a gate parked high
    // is a DC level, not a sound, and it is the DC blocker's job to say so. Map
    // it bipolar instead and every enable/disable would thump.
    //
    // Skipping the oversample while muted is safe — and only because the gate is
    // applied to the filter *input* below, so a muted pin and a zero pin are the
    // same signal. It is not an optimisation that can drift.
    let level = if s.cur.enable && s.pcm_gain > 5.0e-4 {
        let step = dt_ns / OVERSAMPLE as f64;
        let mut acc = 0.0f32;
        for k in 0..OVERSAMPLE {
            acc += pit_ch2_output_at(ts + (k as f64 * step) as u64) as f32;
        }
        acc / OVERSAMPLE as f32
    } else {
        0.0
    };
    // Gate BEFORE the filters. Applied after them, a gain change would step
    // their *output* while their state stayed put; applied here it lands on
    // their input and they roll it off, which is what a filter is for.
    s.pcm_lpf.tick(s.pcm_dc.tick(level * s.pcm_gain)) * 0.9
}
