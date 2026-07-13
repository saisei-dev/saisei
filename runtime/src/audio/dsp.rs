//! Shared DSP primitives for the synth voices and the mixer.
//!
//! Small, allocation-free (except the reverb's delay lines, sized once at
//! construction) and sample-rate-agnostic — everything takes the rate it was
//! built at. Nothing here knows about the guest; these are pure signal blocks.

use super::SAMPLE_RATE;

const TAU: f32 = core::f32::consts::TAU;

// ---- envelopes --------------------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
enum Stage {
    Idle,
    Attack,
    Decay,
    Sustain,
    Release,
}

/// A linear-attack / exponential-decay ADSR. Exponential decay and release are
/// what make a note sound like it was struck rather than switched off; the
/// linear attack keeps transients crisp without an audible ramp.
#[derive(Clone, Copy)]
pub struct Adsr {
    stage: Stage,
    level: f32,
    attack_step: f32,
    decay_coef: f32,
    sustain: f32,
    release_coef: f32,
}

/// Per-sample multiplier that decays to `ratio` of the starting value in
/// `secs`. Used for the exponential segments.
fn decay_coef(secs: f32, ratio: f32) -> f32 {
    if secs <= 0.0 {
        return 0.0;
    }
    let n = secs * SAMPLE_RATE as f32;
    ratio.powf(1.0 / n)
}

impl Adsr {
    pub fn new(attack_s: f32, decay_s: f32, sustain: f32, release_s: f32) -> Self {
        Adsr {
            stage: Stage::Idle,
            level: 0.0,
            attack_step: if attack_s <= 0.0 {
                1.0
            } else {
                1.0 / (attack_s * SAMPLE_RATE as f32)
            },
            // Decay heads *towards* the sustain level, so the time constant is
            // measured against the distance it has to travel, not against zero.
            decay_coef: decay_coef(decay_s, 0.05),
            sustain,
            release_coef: decay_coef(release_s, 0.001),
        }
    }

    pub fn gate_on(&mut self) {
        self.stage = Stage::Attack;
    }

    pub fn gate_off(&mut self) {
        if self.stage != Stage::Idle {
            self.stage = Stage::Release;
        }
    }

    /// Restart the envelope from its *current* level rather than from zero — a
    /// retrigger mid-note must not punch a hole in the waveform.
    pub fn retrigger(&mut self) {
        self.stage = Stage::Attack;
    }

    pub fn is_idle(&self) -> bool {
        self.stage == Stage::Idle
    }

    pub fn level(&self) -> f32 {
        self.level
    }

    pub fn tick(&mut self) -> f32 {
        match self.stage {
            Stage::Idle => self.level = 0.0,
            Stage::Attack => {
                self.level += self.attack_step;
                if self.level >= 1.0 {
                    self.level = 1.0;
                    self.stage = Stage::Decay;
                }
            }
            Stage::Decay => {
                self.level = self.sustain + (self.level - self.sustain) * self.decay_coef;
                if (self.level - self.sustain).abs() < 1.0e-4 {
                    self.level = self.sustain;
                    self.stage = Stage::Sustain;
                }
            }
            Stage::Sustain => self.level = self.sustain,
            Stage::Release => {
                self.level *= self.release_coef;
                if self.level < 1.0e-4 {
                    self.level = 0.0;
                    self.stage = Stage::Idle;
                }
            }
        }
        self.level
    }
}

// ---- filters ----------------------------------------------------------------

/// One-pole lowpass. The workhorse: de-fizzes the speaker's PCM path and tames
/// the synth voice's upper partials.
#[derive(Clone, Copy)]
pub struct OnePole {
    a: f32,
    z: f32,
}

impl OnePole {
    pub fn new(cutoff_hz: f32) -> Self {
        let mut f = OnePole { a: 0.0, z: 0.0 };
        f.set_cutoff(cutoff_hz);
        f
    }

    pub fn set_cutoff(&mut self, cutoff_hz: f32) {
        let x = (-TAU * cutoff_hz / SAMPLE_RATE as f32).exp();
        self.a = 1.0 - x;
    }

    pub fn tick(&mut self, input: f32) -> f32 {
        self.z += self.a * (input - self.z);
        self.z
    }
}

/// A DC blocker. The speaker's direct path sits at a DC level whenever the
/// gate is held without the timer running; without this that offset would
/// thump the output every time a game parked the pin high.
#[derive(Clone, Copy)]
pub struct DcBlock {
    x1: f32,
    y1: f32,
}

impl DcBlock {
    pub const fn new() -> Self {
        DcBlock { x1: 0.0, y1: 0.0 }
    }

    pub fn tick(&mut self, x: f32) -> f32 {
        let y = x - self.x1 + 0.9975 * self.y1;
        self.x1 = x;
        self.y1 = y;
        y
    }
}

// ---- oscillators ------------------------------------------------------------

/// Sine from a phase in turns (0..1). `f32::sin` is plenty fast at 48kHz for
/// the handful of voices we run, and a table would only add aliasing of its own.
#[inline]
pub fn sine(phase: f32) -> f32 {
    (phase * TAU).sin()
}

/// PolyBLEP correction for a hard edge at `phase` with step `dt` per sample.
/// This is what keeps the *faithful* speaker path (the raw square wave, used
/// for PWM/digitised output that cannot be turned into notes) from aliasing
/// into a mess of inharmonic whistles when its pitch climbs.
#[inline]
pub fn poly_blep(phase: f32, dt: f32) -> f32 {
    if phase < dt {
        let t = phase / dt;
        t + t - t * t - 1.0
    } else if phase > 1.0 - dt {
        let t = (phase - 1.0) / dt;
        t * t + t + t + 1.0
    } else {
        0.0
    }
}

/// A band-limited square at `phase`, duty 50%.
#[inline]
pub fn blep_square(phase: f32, dt: f32) -> f32 {
    let mut v = if phase < 0.5 { 1.0 } else { -1.0 };
    v += poly_blep(phase, dt);
    v -= poly_blep((phase + 0.5) % 1.0, dt);
    v
}

// ---- reverb -----------------------------------------------------------------

struct Comb {
    buf: Vec<f32>,
    idx: usize,
    store: f32,
    feedback: f32,
    damp: f32,
}

impl Comb {
    fn new(len: usize, feedback: f32, damp: f32) -> Self {
        Comb {
            buf: vec![0.0; len],
            idx: 0,
            store: 0.0,
            feedback,
            damp,
        }
    }

    fn tick(&mut self, input: f32) -> f32 {
        let out = self.buf[self.idx];
        self.store = out * (1.0 - self.damp) + self.store * self.damp;
        self.buf[self.idx] = input + self.store * self.feedback;
        self.idx = (self.idx + 1) % self.buf.len();
        out
    }
}

struct AllPass {
    buf: Vec<f32>,
    idx: usize,
}

impl AllPass {
    fn new(len: usize) -> Self {
        AllPass {
            buf: vec![0.0; len],
            idx: 0,
        }
    }

    fn tick(&mut self, input: f32) -> f32 {
        let buffered = self.buf[self.idx];
        let out = -input + buffered;
        self.buf[self.idx] = input + buffered * 0.5;
        self.idx = (self.idx + 1) % self.buf.len();
        out
    }
}

/// A compact Schroeder/Freeverb-style stereo reverb: four damped combs in
/// parallel into two allpasses, per channel, with the right channel's delay
/// lines offset so the two decorrelate into a stereo image.
///
/// This is the single biggest reason the beautified speaker stops sounding like
/// a beeper: a bare square wave in an anechoic void reads as a buzzer no matter
/// how nicely it is shaped, and a little room around it reads as an instrument.
pub struct Reverb {
    combs_l: Vec<Comb>,
    combs_r: Vec<Comb>,
    aps_l: Vec<AllPass>,
    aps_r: Vec<AllPass>,
    wet: f32,
}

/// Freeverb's tunings, scaled from its native 44.1kHz to our rate.
const COMB_LENS: [usize; 4] = [1116, 1188, 1277, 1356];
const AP_LENS: [usize; 2] = [556, 441];
/// The classic stereo spread, in samples at 44.1kHz.
const STEREO_SPREAD: usize = 23;

fn scaled(n: usize) -> usize {
    (n * SAMPLE_RATE as usize / 44100).max(1)
}

impl Reverb {
    pub fn new(room: f32, damp: f32, wet: f32) -> Self {
        Reverb {
            combs_l: COMB_LENS
                .iter()
                .map(|&n| Comb::new(scaled(n), room, damp))
                .collect(),
            combs_r: COMB_LENS
                .iter()
                .map(|&n| Comb::new(scaled(n + STEREO_SPREAD), room, damp))
                .collect(),
            aps_l: AP_LENS.iter().map(|&n| AllPass::new(scaled(n))).collect(),
            aps_r: AP_LENS
                .iter()
                .map(|&n| AllPass::new(scaled(n + STEREO_SPREAD)))
                .collect(),
            wet,
        }
    }

    /// Mono in, stereo (wet-only) out — the caller sums it against its dry path.
    pub fn tick(&mut self, input: f32) -> (f32, f32) {
        let x = input * 0.015;
        let mut l = 0.0;
        let mut r = 0.0;
        for c in self.combs_l.iter_mut() {
            l += c.tick(x);
        }
        for c in self.combs_r.iter_mut() {
            r += c.tick(x);
        }
        for a in self.aps_l.iter_mut() {
            l = a.tick(l);
        }
        for a in self.aps_r.iter_mut() {
            r = a.tick(r);
        }
        (l * self.wet, r * self.wet)
    }
}
