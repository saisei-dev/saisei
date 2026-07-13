//! The host audio sink: an SDL2 audio device fed by `SDL_QueueAudio`.
//!
//! Deliberately the *push* API and not a callback. A callback device would run
//! the mixer on SDL's audio thread, which would mean every synth reading guest
//! state (the OPL2 register file, the PIT channel-2 divisor, the DMA window)
//! across a thread boundary — a lock or a lock-free ring around state the guest
//! is concurrently writing. With `SDL_QueueAudio` the mixer runs on the guest
//! thread at its own catch-up points, reads that state directly, and pushes
//! finished frames; SDL plays silence whenever the queue runs dry, which is
//! exactly the behaviour we want while the guest is stopped (the F12 overlay)
//! anyway. No second thread, no locks, no torn reads.
//!
//! Bindings are hand-written `extern "C"` in the same style as `sdl.rs` — SDL2
//! is already one of the project's two system prerequisites, so audio adds no
//! new dependency.

use core::ffi::{c_char, c_int, c_void};

/// The canonical `SDL_AudioSpec` layout (from `<SDL2/SDL_audio.h>`).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct SdlAudioSpec {
    pub freq: c_int,
    pub format: u16,
    pub channels: u8,
    pub silence: u8,
    pub samples: u16,
    pub padding: u16,
    pub size: u32,
    pub callback: *const c_void,
    pub userdata: *mut c_void,
}

extern "C" {
    fn SDL_InitSubSystem(flags: u32) -> c_int;
    fn SDL_QuitSubSystem(flags: u32);
    fn SDL_WasInit(flags: u32) -> u32;
    fn SDL_OpenAudioDevice(
        device: *const c_char,
        iscapture: c_int,
        desired: *const SdlAudioSpec,
        obtained: *mut SdlAudioSpec,
        allowed_changes: c_int,
    ) -> u32;
    fn SDL_CloseAudioDevice(dev: u32);
    fn SDL_PauseAudioDevice(dev: u32, pause_on: c_int);
    fn SDL_QueueAudio(dev: u32, data: *const c_void, len: u32) -> c_int;
    fn SDL_GetQueuedAudioSize(dev: u32) -> u32;
    fn SDL_ClearQueuedAudio(dev: u32);
}

const SDL_INIT_AUDIO: u32 = 0x10;
/// `AUDIO_F32SYS` on a little-endian host: signed | float | 32 bits.
const AUDIO_F32LSB: u16 = 0x8120;

/// Device handle; 0 is SDL's "no device" sentinel.
static mut DEVICE: u32 = 0;
/// Whether *we* were the ones to bring the audio subsystem up.
static mut OWNS_SUBSYSTEM: bool = false;

pub const CHANNELS: usize = 2;
const BYTES_PER_FRAME: u32 = (CHANNELS * core::mem::size_of::<f32>()) as u32;

/// Open the device at `rate` Hz, stereo f32. Returns false if audio is
/// unavailable — a machine with no sound card, a container with no ALSA/Pulse
/// socket. That is not fatal: the game runs silent rather than refusing to
/// start, which is the same posture `virtual_display_open_window` takes.
pub unsafe fn open(rate: u32) -> bool {
    if DEVICE != 0 {
        return true;
    }
    if SDL_WasInit(SDL_INIT_AUDIO) == 0 {
        if SDL_InitSubSystem(SDL_INIT_AUDIO) != 0 {
            return false;
        }
        OWNS_SUBSYSTEM = true;
    }
    let desired = SdlAudioSpec {
        freq: rate as c_int,
        format: AUDIO_F32LSB,
        channels: CHANNELS as u8,
        silence: 0,
        // ~10ms at 48kHz. The device buffer is the floor on output latency;
        // our own queue depth (see `mod.rs`) rides on top of it.
        samples: 512,
        padding: 0,
        size: 0,
        callback: core::ptr::null(),
        userdata: core::ptr::null_mut(),
    };
    let mut obtained = desired;
    // 0 allowed changes: we want exactly this format, and SDL will convert
    // internally if the hardware disagrees. Anything else would make the
    // mixer's frame arithmetic a lie.
    DEVICE = SDL_OpenAudioDevice(
        core::ptr::null(),
        0,
        &desired as *const SdlAudioSpec,
        &mut obtained as *mut SdlAudioSpec,
        0,
    );
    if DEVICE == 0 {
        if OWNS_SUBSYSTEM {
            SDL_QuitSubSystem(SDL_INIT_AUDIO);
            OWNS_SUBSYSTEM = false;
        }
        return false;
    }
    SDL_PauseAudioDevice(DEVICE, 0);
    true
}

pub unsafe fn close() {
    if DEVICE != 0 {
        SDL_ClearQueuedAudio(DEVICE);
        SDL_CloseAudioDevice(DEVICE);
        DEVICE = 0;
    }
    if OWNS_SUBSYSTEM {
        SDL_QuitSubSystem(SDL_INIT_AUDIO);
        OWNS_SUBSYSTEM = false;
    }
}

/// Hand finished interleaved-stereo frames to the device.
pub unsafe fn queue(frames: &[f32]) {
    if DEVICE == 0 || frames.is_empty() {
        return;
    }
    SDL_QueueAudio(
        DEVICE,
        frames.as_ptr() as *const c_void,
        core::mem::size_of_val(frames) as u32,
    );
}

/// Frames still waiting to be played. The mixer steers its output rate off
/// this: it is the only feedback we get about virtual-vs-real clock drift.
pub unsafe fn queued_frames() -> u32 {
    if DEVICE == 0 {
        return 0;
    }
    SDL_GetQueuedAudioSize(DEVICE) / BYTES_PER_FRAME
}
