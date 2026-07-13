//! Sound: the emulated cards, and the one mixer that turns them into samples.
//!
//! # Where the samples come from
//!
//! Every source renders on the **guest thread, in virtual time**. The virtual
//! clock is instruction-driven (see `timer.rs`), so "how much audio does this
//! interval owe" is `elapsed_virtual_ns * rate`, and the mixer simply catches
//! up to `shim_virtual_now_ns()` whenever it is asked to. There are two kinds of
//! catch-up and the distinction matters:
//!
//! * **Forced** — every write to a sound port calls `catchup()` *before* the
//!   write lands. So the samples for the interval that just elapsed are rendered
//!   against the *old* register state, and the write takes effect exactly at its
//!   own virtual timestamp. That is what makes register timing sample-accurate,
//!   and it is not a nicety: a PC-speaker PWM driver toggles the gate at several
//!   kHz, and quantising those edges to a service tick would turn digitised
//!   speech into noise.
//! * **Periodic** — `safe_point_impl` calls `service()` about every millisecond
//!   of virtual time, so a game that is playing a held note without touching a
//!   port still gets its audio produced.
//!
//! # Why the clock cannot simply be trusted
//!
//! Virtual time is paced to real time but is not *made* of it. It stops dead
//! while the F12 overlay is up, it stalls for as long as rustc takes when the
//! JIT meets a new chunk, and it re-anchors rather than fast-forwarding after
//! either. So the mixer:
//!
//! * treats a catch-up delta larger than `REANCHOR_NS` as "the guest was not
//!   running" and drops it instead of rendering a quarter-second burst;
//! * steers its output rate off the device queue depth (`RATE_RATIO`), which is
//!   the only feedback available about virtual-vs-real drift. The authority is
//!   ±2%, well under the ~1% pitch shift an ear starts to catch, and it is the
//!   difference between "plays" and "crackles";
//! * ramps the master gain rather than switching it, and re-ramps from zero
//!   after any underrun, so a stall costs a moment of silence and not a click.

pub mod dma;
pub mod dsp;
pub mod opl2;
mod out;
pub mod sb;
pub mod sn76489;
pub mod speaker;

// The FROZEN OPL2 register-file layout that `snapshot.rs` serialises. It lives
// with the chip that owns it; re-exported here so the module path is unchanged.
pub use opl2::Opl2State;

extern "C" {
    fn shim_virtual_now_ns() -> u64;
    static mut headless_mode: core::ffi::c_int;
}

/// The mixer's rate. Everything in `dsp` is built against it.
pub const SAMPLE_RATE: u32 = 48_000;

/// How much audio we try to keep queued ahead of the device.
///
/// This has to cover the longest stretch of *real* time in which no *virtual*
/// time passes, because that is a stretch in which no samples are produced but
/// the device keeps consuming them: a frame present, a pacing sleep, an input
/// poll. Measured on Zeliard, 60ms was not enough — the queue ran dry dozens of
/// times a minute and every one of those was an audible crack. It is otherwise
/// pure latency, so it is as small as it can be and no smaller.
const TARGET_QUEUE_FRAMES: u32 = SAMPLE_RATE * 110 / 1000;

/// A catch-up gap this large means the guest was not running (overlay, JIT
/// compile, snapshot restore). Render nothing and re-anchor.
const REANCHOR_NS: u64 = 120_000_000;

/// Periodic service granularity — don't bother rendering a handful of frames.
const SERVICE_INTERVAL_NS: u64 = 1_000_000;

/// Per-sample master-gain slew (~5ms). Declicks mute, unmute, pause and
/// underrun alike, without any of them needing to know about the others.
const GAIN_SLEW: f32 = 0.004;

/// The fade rendered into the queue when the guest is about to stop, so the
/// waveform lands on zero instead of being guillotined.
const FADE_MS: u64 = 20;

static mut ENABLED: bool = false;
static mut MUTED: bool = false;
/// Master volume. The player overrides this per game from its settings file
/// before the guest makes a sound; this is only the value a game starts at the
/// first time it is ever run.
static mut VOLUME: f32 = 0.6;
static mut GAIN: f32 = 0.0;
static mut SILENT: bool = false;

static mut LAST_NS: u64 = 0;
static mut FRAME_FRAC: f64 = 0.0;
static mut RATE_RATIO: f64 = 1.0;
/// The integral term of the rate controller: it absorbs the slow stuff a
/// feed-forward cannot know about, chiefly the device's clock not being exactly
/// 48000Hz.
static mut RATE_BIAS: f64 = 0.0;
static mut LAST_RATE_CHECK_NS: u64 = 0;
static mut LAST_SERVICE_NS: u64 = 0;

/// How fast the emulated machine is actually running, as a fraction of real time,
/// smoothed. **This is the input the rate controller most needs.**
///
/// The guest is not a 1.00x machine. In a heavy scene it drops — Zeliard sits at
/// 0.82x coming out of a shop — and when it does, one virtual second buys only
/// 0.82 seconds of samples while the device still eats a full second of them. A
/// controller that only watches the queue has to *discover* that by running dry
/// first, and then chase it with an integral that takes seconds to wind up. So it
/// doesn't have to: the speed is measurable directly, and it is fed forward.
static mut GUEST_SPEED: f64 = 1.0;
static mut SPEED_LAST_V: u64 = 0;
static mut SPEED_LAST_H: u64 = 0;

/// Interleaved-stereo scratch. Heap-backed and reused; never copied out of the
/// static (a `ptr::read` of a static holding heap would hand out a second owner
/// of the same allocation).
static mut SCRATCH: Vec<f32> = Vec::new();

// ---- lifecycle --------------------------------------------------------------

/// Bring audio up. Called once the host has an SDL context (i.e. never in
/// headless mode). Failure is not fatal — the game runs silent.
#[no_mangle]
pub unsafe extern "C" fn saisei_audio_init() -> bool {
    if ENABLED {
        return true;
    }
    if headless_mode != 0 || !out::open(SAMPLE_RATE) {
        return false;
    }
    opl2::synth_reset();
    speaker::reset();
    GAIN = 0.0;
    SILENT = false;
    RATE_RATIO = 1.0;
    reanchor();
    ENABLED = true;
    true
}

#[no_mangle]
pub unsafe extern "C" fn saisei_audio_shutdown() {
    ENABLED = false;
    out::close();
}

/// Master volume, 0.0..1.0. Slewed in, so this is safe to call from the
/// overlay while a note is sounding.
#[no_mangle]
pub unsafe extern "C" fn saisei_audio_set_volume(v: f32) {
    VOLUME = v.clamp(0.0, 1.0);
}

#[no_mangle]
pub unsafe extern "C" fn saisei_audio_get_volume() -> f32 {
    VOLUME
}

#[no_mangle]
pub unsafe extern "C" fn saisei_audio_set_muted(muted: bool) {
    MUTED = muted;
}

#[no_mangle]
pub unsafe extern "C" fn saisei_audio_is_muted() -> bool {
    MUTED
}

/// A short reference note at the current volume.
///
/// The overlay is the one place a player sets the volume, and while it is up the
/// guest is frozen — so there is nothing making a sound to judge the setting by.
/// Without this the slider would be a number you could only hear the consequences
/// of after closing the menu, which is not a volume control, it is a guess.
#[no_mangle]
pub unsafe extern "C" fn saisei_audio_preview() {
    if !ENABLED {
        return;
    }
    // A dragged slider fires this on every mouse-move. Don't stack a queue of
    // blips that keeps playing long after the drag stopped.
    if out::queued_frames() > SAMPLE_RATE / 6 {
        return;
    }

    // The suspend that froze the guest also muted the mixer; the preview is
    // exactly the sound that is allowed through anyway.
    let was_silent = SILENT;
    SILENT = false;

    let n = (SAMPLE_RATE as usize * 160) / 1000;
    let buf = &mut *core::ptr::addr_of_mut!(SCRATCH);
    buf.clear();
    buf.resize(n * out::CHANNELS, 0.0);

    let sr = SAMPLE_RATE as f32;
    for i in 0..n {
        let t = i as f32 / sr;
        // A fifth, so it reads as a tone and not as a test beep, under a fast
        // attack and an exponential decay.
        let a = (t * 587.33 * core::f32::consts::TAU).sin() * 0.55
            + (t * 880.0 * core::f32::consts::TAU).sin() * 0.18;
        let env = (t / 0.004).min(1.0) * (-t * 14.0).exp();
        let v = a * env * 0.5;
        buf[i * 2] = v;
        buf[i * 2 + 1] = v;
    }

    let target = if MUTED { 0.0 } else { VOLUME };
    for frame in buf.chunks_exact_mut(out::CHANNELS) {
        GAIN += (target - GAIN) * GAIN_SLEW;
        frame[0] = (frame[0] * GAIN).clamp(-1.0, 1.0);
        frame[1] = (frame[1] * GAIN).clamp(-1.0, 1.0);
    }
    out::queue(buf);
    SILENT = was_silent;
}

/// True once a device is open and rendering.
pub unsafe fn is_active() -> bool {
    ENABLED
}

// ---- the catch-up ------------------------------------------------------------

/// Forget the elapsed interval and restart from now. Used at init and whenever
/// the guest has been stopped for long enough that the backlog is meaningless.
unsafe fn reanchor() {
    LAST_NS = shim_virtual_now_ns();
    LAST_SERVICE_NS = LAST_NS;
    FRAME_FRAC = 0.0;
    prime();
}

/// Put a cushion of silence in front of the device.
///
/// Without this the queue starts empty and *stays* empty. We only ever render the
/// audio that virtual time has actually earned — one second of guest time is one
/// second of samples — so there is nothing spare to build a buffer out of. The
/// queue level simply random-walks around wherever it started, and if it started
/// at zero then every frame present, every pacing sleep and every input poll
/// takes it straight through the floor. That was Zeliard: dozens of 15ms holes a
/// minute, each one an audible crack.
///
/// The rate control cannot dig it out either. It has 3% of authority, so refilling
/// 110ms of buffer from empty would take nearly four seconds — by which time the
/// next stall has already happened. The cushion has to be *given*, once, and then
/// defended.
unsafe fn prime() {
    let have = out::queued_frames();
    if have >= TARGET_QUEUE_FRAMES {
        return;
    }
    let n = (TARGET_QUEUE_FRAMES - have) as usize;
    let buf = &mut *core::ptr::addr_of_mut!(SCRATCH);
    buf.clear();
    buf.resize(n * out::CHANNELS, 0.0);
    out::queue(buf);
}

/// Render everything owed up to the current virtual instant. Call this from a
/// sound port's write handler *before* applying the write.
#[inline]
pub unsafe fn catchup() {
    if !ENABLED {
        return;
    }
    render_to(shim_virtual_now_ns());
}

/// The periodic pump, from `safe_point_impl`. Rate-limited: a held note needs
/// its samples produced, but not in 40-microsecond slivers.
#[inline]
pub unsafe fn service() {
    if !ENABLED {
        return;
    }
    let now = shim_virtual_now_ns();
    if now.saturating_sub(LAST_SERVICE_NS) < SERVICE_INTERVAL_NS {
        return;
    }
    LAST_SERVICE_NS = now;
    render_to(now);
}

/// The guest is about to stop for an unbounded time (the F12 overlay). Fade the
/// tail into the queue so it lands on zero, then let the queue drain: SDL plays
/// silence on an empty queue, which is precisely the behaviour we want, so there
/// is no device to pause and no pause machinery to get wrong.
#[no_mangle]
pub unsafe extern "C" fn shim_audio_suspend() {
    if !ENABLED || SILENT {
        return;
    }
    catchup();
    SILENT = true;
    // The clock is about to freeze, so this block does not advance virtual time
    // — it just lets the voices ring out under a falling gain. The sources tick
    // on sample time for exactly this reason.
    let frames = (SAMPLE_RATE as u64 * FADE_MS / 1000) as usize;
    render_frames(frames, LAST_NS, 1.0e9 / SAMPLE_RATE as f64);
}

/// The guest is running again. The gain is at zero, so the next block ramps it
/// back up on its own.
#[no_mangle]
pub unsafe extern "C" fn shim_audio_resume() {
    if !ENABLED {
        return;
    }
    SILENT = false;
    reanchor();
}

unsafe fn render_to(now: u64) {
    let delta = now.saturating_sub(LAST_NS);
    if delta == 0 {
        return;
    }
    if delta > REANCHOR_NS {
        reanchor();
        return;
    }
    update_rate_ratio(now);

    let t0 = LAST_NS;
    let owed = FRAME_FRAC + delta as f64 * (SAMPLE_RATE as f64 * RATE_RATIO) / 1.0e9;
    let n = owed as usize;
    FRAME_FRAC = owed - n as f64;
    LAST_NS = now;
    if n > 0 {
        // The n frames *cover* the interval, whatever the rate ratio did to
        // their count — so a frame's virtual timestamp is just its share of it.
        render_frames(n, t0, delta as f64 / n as f64);
    }
}

/// How many output frames to emit per second of *virtual* time.
///
/// # The thing this exists for
///
/// The emulated machine does not run at 1.00x real time. In a heavy scene it
/// drops — Zeliard sits at **0.82x** coming out of a shop and stays there. One
/// virtual second then buys only 0.82 seconds of samples, while the device keeps
/// eating a full second of them. Two things follow, and they are the same bug:
///
/// * the queue bleeds dry and the sound crackles; and
/// * the chips *advance* one sample per output frame, so they run at 0.82x of
///   their nominal rate in real time — and the music comes out roughly two
///   semitones flat. That is what "stretched" sounds like.
///
/// The correction for both is the same number, `1 / speed`, and that is not a
/// coincidence: the ratio that keeps the queue level is exactly the ratio that
/// makes the chip advance at its nominal rate in real time. Rate-correcting a slow
/// guest does not detune it — *not* correcting it is what detunes it.
///
/// # Feed it forward; do not chase it
///
/// The guest's speed is directly measurable, so it is measured (`GUEST_SPEED`)
/// and fed forward. A controller that watched only the queue would have to
/// *discover* an 18% shortfall by first running dry, then chase it with an
/// integral — and the integral was clamped at +10%, which could not reach the
/// +22% needed, so it pinned at its limit and the queue simply stayed empty.
/// That was the bug: not the gains, the fact that it was guessing at something it
/// could have known.
///
/// The PI loop stays, demoted to what it is good at: trimming the residual (the
/// device's clock is not exactly 48000Hz either) and holding the queue at target.
unsafe fn update_rate_ratio(now: u64) {
    // Slowly. The queue holds a tenth of a second and the residual being trimmed
    // is a drift, so the loop's business is with seconds — poll it at millisecond
    // rates with gains to match and it does not correct the drift, it *becomes*
    // the drift, hunting between overfull and empty and cracking on every
    // downswing. That is also what it did.
    if now.saturating_sub(LAST_RATE_CHECK_NS) < 50_000_000 {
        return;
    }
    LAST_RATE_CHECK_NS = now;
    measure_guest_speed(now);
    let queued = out::queued_frames();
    QUEUED_CACHE = queued;
    if queued == 0 {
        // Underrun. The device has already played silence, and whatever we queue
        // next starts at an arbitrary point in the waveform: drop the gain and let
        // it ramp back in rather than stitching a click on top of the gap. Then
        // rebuild the cushion by hand — the controller corrects a rate, and no
        // rate correction refills an empty buffer fast enough to matter.
        GAIN = 0.0;
        prime();
    }
    let target = TARGET_QUEUE_FRAMES as f64;
    let err = ((target - queued as f64) / target).clamp(-1.0, 1.0);
    // The integral moves at most 4% of rate per second, so it settles a residual
    // within a couple of seconds and cannot slam. The proportional term is
    // deliberately small: it is here to damp, not to correct.
    RATE_BIAS = (RATE_BIAS + err * 0.002).clamp(-0.10, 0.10);

    // Feed-forward first, trim second. The clamp is wide because the thing it has
    // to cover is the guest's real speed, and that is not ours to bound — a scene
    // that halves the emulated machine's speed needs twice the frames per virtual
    // second, or the queue starves and the music plays an octave-ish flat.
    let ff = 1.0 / GUEST_SPEED;
    RATE_RATIO = (ff * (1.0 + 0.02 * err + RATE_BIAS)).clamp(0.5, 2.5);
}

/// Measure how fast the guest is running, as a fraction of real time.
unsafe fn measure_guest_speed(now: u64) {
    extern "C" {
        fn shim_host_monotonic_ns() -> u64;
    }
    let h = shim_host_monotonic_ns();
    let dh = h.saturating_sub(SPEED_LAST_H);
    let dv = now.saturating_sub(SPEED_LAST_V);
    let first = SPEED_LAST_H == 0;
    SPEED_LAST_H = h;
    SPEED_LAST_V = now;
    if first || dh == 0 {
        return;
    }

    // This runs every 50ms of *virtual* time, so a healthy window is ~50ms of host
    // time too. A much longer one means the guest was not running at all — a JIT
    // compile, a blocking read, the overlay — and the "speed" of a window the guest
    // spent stopped is not a speed. Reading it as one would send the feed-forward
    // chasing a machine that is not there.
    if dh > 400_000_000 {
        return;
    }

    let speed = (dv as f64 / dh as f64).clamp(0.4, 2.0);
    // Smoothed hard enough that a single odd window cannot move the pitch, loose
    // enough to follow a scene change within about a second.
    GUEST_SPEED += (speed - GUEST_SPEED) * 0.12;
    GUEST_SPEED = GUEST_SPEED.clamp(0.4, 2.0);
}

/// The queue is a latency budget, not a backlog. Past this, samples are ones the
/// player would hear long after the thing that made them, so overshooting is not
/// something to store — it is something to stop doing.
const MAX_QUEUE_FRAMES: u32 = TARGET_QUEUE_FRAMES * 3;
/// The last queue depth we asked SDL for. Read every 50ms by the rate controller;
/// cached because the render path runs thousands of times a second and must not
/// ask again on each one.
static mut QUEUED_CACHE: u32 = 0;

unsafe fn render_frames(n: usize, t0: u64, dt_ns: f64) {
    let buf = &mut *core::ptr::addr_of_mut!(SCRATCH);
    buf.clear();
    buf.resize(n * out::CHANNELS, 0.0);

    // Each source *adds* into the buffer at its own level.
    opl2::render(buf);
    speaker::render(buf, t0, dt_ns);
    sn76489::render(buf);
    sb::render(buf);

    let target = if MUTED || SILENT { 0.0 } else { VOLUME };
    for frame in buf.chunks_exact_mut(out::CHANNELS) {
        GAIN += (target - GAIN) * GAIN_SLEW;
        frame[0] = soft_clip(frame[0] * GAIN);
        frame[1] = soft_clip(frame[1] * GAIN);
    }

    // The synths were advanced either way — their state has to stay in step with
    // virtual time. It is only the *queueing* that is dropped, and only when the
    // queue is already so deep that keeping these would mean playing them a third
    // of a second late.
    if QUEUED_CACHE > MAX_QUEUE_FRAMES {
        return;
    }
    out::queue(buf);
    QUEUED_CACHE += n as u32;
}

/// Saturate rather than clip.
///
/// Nine FM channels, a speaker voice and a digitised stream can all be sounding
/// at once, and the sum of them has no upper bound we get to choose. Truncating
/// that at +-1.0 folds the excess into harsh broadband hash — the loud passages
/// would be exactly the ones that turned to gravel. This bends instead: linear
/// where the signal actually lives, and asymptotic to full scale above it, so a
/// peak sounds loud rather than broken.
#[inline]
fn soft_clip(x: f32) -> f32 {
    const KNEE: f32 = 0.7;
    let a = x.abs();
    if a <= KNEE {
        return x;
    }
    let over = a - KNEE;
    let y = KNEE + (1.0 - KNEE) * (over / (1.0 - KNEE + over));
    if x < 0.0 {
        -y
    } else {
        y
    }
}
