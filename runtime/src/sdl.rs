//! Port of `runtime/display/virtual_display_sdl.c` — the SDL2 window / present
//! pipeline + host input (keyboard/mouse) forwarding.
//!
//! In headless runs the display is never created (window/renderer stay null and
//! every entry point early-returns), so the SDL paths are inert; validated by
//! compile + boot. SDL2 constants/struct layouts below are taken from the system
//! <SDL2/*.h> ABI (stable). Feeds input to the Rust keyboard/mouse modules.

use crate::keyboard::{
    shim_keyboard_enqueue, shim_keyboard_enqueue_scancode_press,
    shim_keyboard_enqueue_scancode_press_ext, shim_keyboard_enqueue_scancode_release,
    shim_keyboard_enqueue_scancode_release_ext,
};
use crate::mouse::{mouse_host_button, mouse_host_motion};
use core::ffi::{c_char, c_int, c_void};

// ---- SDL2 opaque types + event union (ABI-stable, from <SDL2/*.h>) -----------

#[repr(C)]
#[derive(Clone, Copy)]
struct SdlKeysym {
    scancode: i32,
    sym: i32,
    r#mod: u16,
    unused: u32,
}
#[repr(C)]
#[derive(Clone, Copy)]
struct SdlKeyboardEvent {
    type_: u32,
    timestamp: u32,
    window_id: u32,
    state: u8,
    repeat: u8,
    padding2: u8,
    padding3: u8,
    keysym: SdlKeysym,
}
#[repr(C)]
#[derive(Clone, Copy)]
struct SdlWindowEvent {
    type_: u32,
    timestamp: u32,
    window_id: u32,
    event: u8,
    padding1: u8,
    padding2: u8,
    padding3: u8,
    data1: i32,
    data2: i32,
}
#[repr(C)]
#[derive(Clone, Copy)]
struct SdlMouseMotionEvent {
    type_: u32,
    timestamp: u32,
    window_id: u32,
    which: u32,
    state: u32,
    x: i32,
    y: i32,
    xrel: i32,
    yrel: i32,
}
#[repr(C)]
#[derive(Clone, Copy)]
struct SdlMouseButtonEvent {
    type_: u32,
    timestamp: u32,
    window_id: u32,
    which: u32,
    button: u8,
    state: u8,
    clicks: u8,
    padding1: u8,
    x: i32,
    y: i32,
}
#[repr(C)]
union SdlEvent {
    type_: u32,
    window: SdlWindowEvent,
    key: SdlKeyboardEvent,
    motion: SdlMouseMotionEvent,
    button: SdlMouseButtonEvent,
    padding: [u8; 56],
}

extern "C" {
    fn SDL_SetHint(name: *const c_char, value: *const c_char) -> c_int;
    fn SDL_Init(flags: u32) -> c_int;
    fn SDL_Quit();
    fn SDL_WasInit(flags: u32) -> u32;
    fn SDL_GetTicks() -> u32;
    fn SDL_Delay(ms: u32);
    fn SDL_CreateWindow(
        title: *const c_char,
        x: c_int,
        y: c_int,
        w: c_int,
        h: c_int,
        flags: u32,
    ) -> *mut c_void;
    fn SDL_DestroyWindow(w: *mut c_void);
    fn SDL_SetWindowSize(w: *mut c_void, width: c_int, height: c_int);
    fn SDL_SetWindowTitle(w: *mut c_void, title: *const c_char);
    fn SDL_CreateRenderer(w: *mut c_void, index: c_int, flags: u32) -> *mut c_void;
    fn SDL_DestroyRenderer(r: *mut c_void);
    fn SDL_RenderSetLogicalSize(r: *mut c_void, w: c_int, h: c_int) -> c_int;
    fn SDL_CreateTexture(
        r: *mut c_void,
        format: u32,
        access: c_int,
        w: c_int,
        h: c_int,
    ) -> *mut c_void;
    fn SDL_DestroyTexture(t: *mut c_void);
    fn SDL_SetTextureScaleMode(t: *mut c_void, mode: c_int) -> c_int;
    fn SDL_SetTextureColorMod(t: *mut c_void, r: u8, g: u8, b: u8) -> c_int;
    fn SDL_UpdateTexture(
        t: *mut c_void,
        rect: *const c_void,
        pixels: *const c_void,
        pitch: c_int,
    ) -> c_int;
    fn SDL_RenderClear(r: *mut c_void) -> c_int;
    fn SDL_RenderCopy(
        r: *mut c_void,
        t: *mut c_void,
        src: *const c_void,
        dst: *const c_void,
    ) -> c_int;
    fn SDL_RenderPresent(r: *mut c_void);
    fn SDL_PumpEvents();
    fn SDL_PollEvent(e: *mut SdlEvent) -> c_int;
    fn SDL_GetKeyboardState(numkeys: *mut c_int) -> *const u8;
    // rest of the C runtime + save layer
    fn shim_save_video_memory();
    fn shim_bookend_start();
    fn shim_bookend_stop();
    fn shim_reinstall_crash_handlers();
    fn save_manager_sr_log(msg: *const c_char);
    fn save_manager_request_save();
    fn save_manager_request_load_latest();
    fn save_manager_poll_pending();
    static mut virtual_display_buffer: c_int;
}

// C stdlib bits used directly.
extern "C" {
    fn realloc(ptr: *mut c_void, size: usize) -> *mut c_void;
    fn free(ptr: *mut c_void);
    fn exit(code: c_int) -> !;
    fn fflush(stream: *mut c_void) -> c_int;
}

// ---- SDL2 constants (from <SDL2/*.h>) ---------------------------------------

const SDL_INIT_VIDEO: u32 = 0x20;
const SDL_WINDOW_RESIZABLE: u32 = 0x20;
const SDL_RENDERER_ACCELERATED: u32 = 0x02;
const SDL_WINDOWPOS_UNDEFINED: c_int = 0x1FFF0000;
const SDL_TEXTUREACCESS_STATIC: c_int = 0;
const SDL_TEXTUREACCESS_STREAMING: c_int = 1;
const SDL_SCALEMODE_LINEAR: c_int = 1;
const SDL_PIXELFORMAT_RGB24: u32 = 0x17101803;

const SDL_QUIT: u32 = 0x100;
const SDL_WINDOWEVENT: u32 = 0x200;
const SDL_KEYDOWN: u32 = 0x300;
const SDL_KEYUP: u32 = 0x301;
const SDL_MOUSEMOTION: u32 = 0x400;
const SDL_MOUSEBUTTONDOWN: u32 = 0x401;
const SDL_MOUSEBUTTONUP: u32 = 0x402;

const SDL_WINDOWEVENT_HIDDEN: u8 = 2;
const SDL_WINDOWEVENT_SIZE_CHANGED: u8 = 6;
const SDL_WINDOWEVENT_MINIMIZED: u8 = 7;
const SDL_WINDOWEVENT_LEAVE: u8 = 11;
const SDL_WINDOWEVENT_FOCUS_LOST: u8 = 13;

const KMOD_CTRL: u16 = 0x0040 | 0x0080;
const KMOD_GUI: u16 = 0x0400 | 0x0800;
const SDL_BUTTON_MIDDLE: u8 = 2;
const SDL_BUTTON_RIGHT: u8 = 3;

const MASK: i32 = 1 << 30; // SDLK_SCANCODE_MASK
const SDL_SCANCODE_P: i32 = 19;
const SDL_SCANCODE_F1: i32 = 58;
const SDL_SCANCODE_F9: i32 = 66;
const SDL_SCANCODE_RIGHT: i32 = 79;
const SDL_SCANCODE_LEFT: i32 = 80;
const SDL_SCANCODE_DOWN: i32 = 81;
const SDL_SCANCODE_UP: i32 = 82;
// SDLK_* keycodes
const SDLK_RETURN: i32 = b'\r' as i32;
const SDLK_ESCAPE: i32 = 27;
const SDLK_BACKSPACE: i32 = 8;
const SDLK_TAB: i32 = 9;
const SDLK_1: i32 = b'1' as i32;
const SDLK_2: i32 = b'2' as i32;
const SDLK_3: i32 = b'3' as i32;
const SDLK_UP: i32 = SDL_SCANCODE_UP | MASK;
const SDLK_DOWN: i32 = SDL_SCANCODE_DOWN | MASK;
const SDLK_LEFT: i32 = SDL_SCANCODE_LEFT | MASK;
const SDLK_RIGHT: i32 = SDL_SCANCODE_RIGHT | MASK;
const SDLK_LCTRL: i32 = 224 | MASK;
const SDLK_RCTRL: i32 = 228 | MASK;
const SDLK_F1: i32 = SDL_SCANCODE_F1 | MASK;
const SDLK_F2: i32 = (SDL_SCANCODE_F1 + 1) | MASK;
const SDLK_F9: i32 = SDL_SCANCODE_F9 | MASK;
const SDLK_F10: i32 = (SDL_SCANCODE_F9 + 1) | MASK;

/// Host numeric keypad (SDL scancodes 89..98 = KP_1..KP_9, KP_0) → the plain
/// (non-E0) DOS keypad make codes. Games address the physical keypad by these
/// codes regardless of NumLock — e.g. DM's movement grid is keypad
/// 4/5/6 (turn-left / FORWARD / turn-right) and 1/2/3 (strafe-l / back /
/// strafe-r), exactly as on a real 1992 keyboard.
const SDL_SCANCODE_KP_1: i32 = 89;
fn sdl_kp_to_dos(sdl_scancode: i32) -> u8 {
    match sdl_scancode - SDL_SCANCODE_KP_1 {
        0 => 0x4F, // KP_1
        1 => 0x50, // KP_2
        2 => 0x51, // KP_3
        3 => 0x4B, // KP_4
        4 => 0x4C, // KP_5
        5 => 0x4D, // KP_6
        6 => 0x47, // KP_7
        7 => 0x48, // KP_8
        8 => 0x49, // KP_9
        9 => 0x52, // KP_0
        _ => 0,
    }
}

// ---- state (file-static in the C) -------------------------------------------

static mut WINDOW: *mut c_void = core::ptr::null_mut();
static mut RENDERER: *mut c_void = core::ptr::null_mut();
static mut TEXTURE: *mut c_void = core::ptr::null_mut();
static mut DISPLAY_READY: bool = false;
// Cleared until the game's first real frame has *earned* the window: while false
// the window shows the logo splash (see `draw_splash`) instead of a blank buffer.
// Set only by `retire_splash`, i.e. after the logo has had its full hold + fade.
static mut GAME_HAS_PRESENTED: bool = false;
// SDL ticks at which the logo first reached the window (0 = never presented, as
// in a headless run). The minimum-hold clock runs from here, not from the first
// game frame — time the game spends JIT-compiling counts toward the hold.
static mut SPLASH_SHOWN_AT: u32 = 0;
// The splash's own texture (see `ensure_splash_texture`), destroyed once the
// logo has been run out. Composited once per geometry, not per frame.
static mut SPLASH_TEXTURE: *mut c_void = core::ptr::null_mut();
static mut SPLASH_TEX_W: c_int = 0;
static mut SPLASH_TEX_H: c_int = 0;
static mut RGB_BUFFER: *mut u8 = core::ptr::null_mut();
static mut RGB_BUFFER_SIZE: i32 = 0;
static mut WIN_W: c_int = 0;
static mut WIN_H: c_int = 0;
static mut SCALE_HINT: c_int = 3;
static mut TEXTURE_W: c_int = 0;
static mut TEXTURE_H: c_int = 0;
static mut PRESSED_SCANCODES: [bool; 128] = [false; 128];
static mut PRESSED_EXT: [bool; 128] = [false; 128];
static mut CTRL_PRESSED: bool = false;
static mut LAST_POLL_TICKS: u32 = 0;

#[inline(always)]
fn vga6_to_8(v6: u8) -> u8 {
    (v6 << 2) | (v6 >> 4)
}

// US ASCII->scancode map used by the press/release pairing.
fn ascii_to_scancode(ascii: u8) -> u8 {
    match ascii {
        b'\r' | b'\n' => 0x1C,
        27 => 0x01,
        b' ' => 0x39,
        0x08 => 0x0E,
        b'\t' => 0x0F,
        b'a'..=b'z' => {
            const MAP: [u8; 26] = [
                0x1E, 0x30, 0x2E, 0x20, 0x12, 0x21, 0x22, 0x23, 0x17, 0x24, 0x25, 0x26, 0x32, 0x31,
                0x18, 0x19, 0x10, 0x13, 0x1F, 0x14, 0x16, 0x2F, 0x11, 0x2D, 0x15, 0x2C,
            ];
            MAP[(ascii - b'a') as usize]
        }
        b'0'..=b'9' => {
            const MAP: [u8; 10] = [0x0B, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A];
            MAP[(ascii - b'0') as usize]
        }
        _ => 0,
    }
}

fn ensure_rgb_buffer(w: i32, h: i32) {
    let need = w * h * 3;
    unsafe {
        if RGB_BUFFER_SIZE >= need {
            return;
        }
        let tmp = realloc(RGB_BUFFER as *mut c_void, need as usize);
        if tmp.is_null() {
            return;
        }
        RGB_BUFFER = tmp as *mut u8;
        RGB_BUFFER_SIZE = need;
    }
}

// ---- splash (the Saisei logo, shown before the game's first present) --------

// LOGO_W / LOGO_H / LOGO_RGB: the logo art as raw RGB24, decoded and downscaled
// from assets/saisei_logo.png at build time (see build.rs).
include!(concat!(env!("OUT_DIR"), "/logo.rs"));

// The logo owns the window for at least this long from the moment it first
// appears, then dissolves over SPLASH_FADE_MS. The game's first frame waits out
// both (see `retire_splash`), so the logo is never cut short and no game pixel
// is ever shown underneath it.
const SPLASH_MIN_MS: u32 = 1500;
const SPLASH_FADE_MS: u32 = 500;
// Frame pacing for the hold/fade loops: ~120Hz, fast enough that the dissolve is
// smooth on any refresh rate without spinning the CPU.
const SPLASH_FRAME_MS: u32 = 8;
// Long edge the splash texture must reach (see `ensure_splash_texture`): enough
// that the logo lands on a typical window at native detail instead of inheriting
// the guest's 320x200. The multiple is capped so an already-large guest mode
// (640x480) can't blow the texture up to something absurd.
const SPLASH_MIN_EDGE: c_int = 1024;
const SPLASH_MAX_MULT: c_int = 8;

// Box-filter the logo into an RGB24 buffer, aspect preserved and centered. The
// art is black-backed, so the letterbox bars the fit leaves are invisible.
// Averaging the source box per destination pixel (rather than point-sampling)
// keeps the blossom dither from aliasing into noise on the way down.
fn blit_logo(buf: &mut [u8], w: usize, h: usize) {
    if w == 0 || h == 0 {
        return;
    }
    let scale = (w as f32 / LOGO_W as f32).min(h as f32 / LOGO_H as f32);
    let dw = ((LOGO_W as f32 * scale) as usize).clamp(1, w);
    let dh = ((LOGO_H as f32 * scale) as usize).clamp(1, h);
    let x0 = (w - dw) / 2;
    let y0 = (h - dh) / 2;

    for dy in 0..dh {
        let sy0 = dy * LOGO_H / dh;
        let sy1 = (((dy + 1) * LOGO_H).div_ceil(dh)).clamp(sy0 + 1, LOGO_H);
        for dx in 0..dw {
            let sx0 = dx * LOGO_W / dw;
            let sx1 = (((dx + 1) * LOGO_W).div_ceil(dw)).clamp(sx0 + 1, LOGO_W);
            let (mut r, mut g, mut b, mut n) = (0u32, 0u32, 0u32, 0u32);
            for sy in sy0..sy1 {
                for sx in sx0..sx1 {
                    let s = (sy * LOGO_W + sx) * 3;
                    r += LOGO_RGB[s] as u32;
                    g += LOGO_RGB[s + 1] as u32;
                    b += LOGO_RGB[s + 2] as u32;
                    n += 1;
                }
            }
            let o = ((y0 + dy) * w + x0 + dx) * 3;
            buf[o] = (r / n) as u8;
            buf[o + 1] = (g / n) as u8;
            buf[o + 2] = (b / n) as u8;
        }
    }
}

// Build the splash image: the logo, centered on black. Split out from the present
// path so its pixels are unit-testable without SDL.
fn build_splash_rgb(w: usize, h: usize) -> Vec<u8> {
    let mut buf = vec![0u8; w * h * 3];
    blit_logo(&mut buf, w, h);
    buf
}

/// True while the pre-game logo is still showing: the display is up and the game
/// hasn't taken the window yet. The present path uses this to hold the logo
/// through the game's text-mode console/setup screens (e.g. Dungeon Master's
/// drive prompt) instead of flashing the text buffer.
pub(crate) fn splash_is_up() -> bool {
    unsafe { DISPLAY_READY && !GAME_HAS_PRESENTED }
}

/// (Re)draw the splash at full brightness. No-op unless the display is up.
pub(crate) fn show_splash() {
    draw_splash_at(255);
}

// Create (or resize) the splash's texture and upload the composited logo.
//
// The splash deliberately does NOT draw through the game's texture: that one is
// the guest's framebuffer geometry (320x200 for a VGA game), which would cap the
// logo at 320x200 of detail no matter how large the window is. This texture is a
// whole multiple of the game's — same aspect, so a full-viewport RenderCopy still
// fills the window undistorted — scaled up until its long edge clears
// SPLASH_MIN_EDGE, which is what lets the art land at native sharpness.
//
// It samples LINEAR, unlike the game's nearest: the logo is smooth art being
// resampled to an arbitrary window size, not a pixel grid whose blocks we must
// preserve exactly.
unsafe fn ensure_splash_texture() -> bool {
    if RENDERER.is_null() || TEXTURE_W <= 0 || TEXTURE_H <= 0 {
        return false;
    }
    let edge = TEXTURE_W.max(TEXTURE_H);
    let mult = ((SPLASH_MIN_EDGE + edge - 1) / edge).clamp(1, SPLASH_MAX_MULT);
    let (w, h) = (TEXTURE_W * mult, TEXTURE_H * mult);
    if !SPLASH_TEXTURE.is_null() && SPLASH_TEX_W == w && SPLASH_TEX_H == h {
        return true;
    }
    destroy_splash_texture();

    let tex = SDL_CreateTexture(
        RENDERER,
        SDL_PIXELFORMAT_RGB24,
        SDL_TEXTUREACCESS_STATIC,
        w,
        h,
    );
    if tex.is_null() {
        return false;
    }
    SDL_SetTextureScaleMode(tex, SDL_SCALEMODE_LINEAR);
    let buf = build_splash_rgb(w as usize, h as usize);
    SDL_UpdateTexture(tex, core::ptr::null(), buf.as_ptr() as *const c_void, w * 3);
    SPLASH_TEXTURE = tex;
    SPLASH_TEX_W = w;
    SPLASH_TEX_H = h;
    true
}

unsafe fn destroy_splash_texture() {
    if !SPLASH_TEXTURE.is_null() {
        SDL_DestroyTexture(SPLASH_TEXTURE);
        SPLASH_TEXTURE = core::ptr::null_mut();
    }
    SPLASH_TEX_W = 0;
    SPLASH_TEX_H = 0;
}

// Present the splash at brightness `level` (255 = full, 0 = black). The dissolve
// is a texture color-mod, so fading costs no per-frame pixel work and the logo is
// composited exactly once. Purely host-side chrome; the game's own frames take
// over once `retire_splash` has run the logo out.
fn draw_splash_at(level: u8) {
    unsafe {
        if !ensure_splash_texture() {
            return;
        }
        SDL_SetTextureColorMod(SPLASH_TEXTURE, level, level, level);
        SDL_RenderClear(RENDERER);
        SDL_RenderCopy(
            RENDERER,
            SPLASH_TEXTURE,
            core::ptr::null(),
            core::ptr::null(),
        );
        SDL_RenderPresent(RENDERER);

        // First frame on screen starts the minimum-hold clock. (max(1) keeps 0
        // meaning "never shown" — the headless case, where this all no-ops.)
        if SPLASH_SHOWN_AT == 0 {
            SPLASH_SHOWN_AT = SDL_GetTicks().max(1);
        }
    }
}

/// Run the logo out and hand the window to the game: hold it at full brightness
/// until it has had its SPLASH_MIN_MS on screen, dissolve it to black over
/// SPLASH_FADE_MS, and only then let `GAME_HAS_PRESENTED` go true.
///
/// This blocks the machine loop for the remainder of the hold + the fade, which
/// is exactly the intent — the game must not draw over a logo that is still up.
/// The stall is safe for emulated time: the virtual clock is instruction-driven,
/// so it simply doesn't advance while the guest isn't running, and the pacer
/// re-anchors rather than fast-forwarding after a host stall this long
/// (`PACING_FORGIVE_NS`, shims.rs). Events keep pumping throughout, so the
/// window stays responsive and a quit during the fade still lands.
fn retire_splash() {
    unsafe {
        if GAME_HAS_PRESENTED {
            return;
        }
        // Claim the window first: nothing (a mode-set redraw, a text-mode
        // present) may re-arm the splash from under the dissolve.
        GAME_HAS_PRESENTED = true;
        if SPLASH_SHOWN_AT == 0 || RENDERER.is_null() {
            return; // headless, or the logo never made it to a window: nothing to run out
        }

        while SDL_GetTicks().wrapping_sub(SPLASH_SHOWN_AT) < SPLASH_MIN_MS {
            handle_events();
            draw_splash_at(255);
            SDL_Delay(SPLASH_FRAME_MS);
        }

        let fade_start = SDL_GetTicks();
        loop {
            let t = SDL_GetTicks().wrapping_sub(fade_start);
            if t >= SPLASH_FADE_MS {
                break;
            }
            handle_events();
            // Smoothstep: linger near full, then fall away.
            let k = 1.0 - t as f32 / SPLASH_FADE_MS as f32;
            let k = k * k * (3.0 - 2.0 * k);
            draw_splash_at((k * 255.0) as u8);
            SDL_Delay(SPLASH_FRAME_MS);
        }
        draw_splash_at(0); // black; the caller's first game frame lands next
        destroy_splash_texture();
    }
}

fn recreate_texture(w: c_int, h: c_int) {
    unsafe {
        if RENDERER.is_null() || w <= 0 || h <= 0 {
            return;
        }
        if !TEXTURE.is_null() && w == TEXTURE_W && h == TEXTURE_H {
            return;
        }
        if !TEXTURE.is_null() {
            SDL_DestroyTexture(TEXTURE);
            TEXTURE = core::ptr::null_mut();
        }
        TEXTURE = SDL_CreateTexture(
            RENDERER,
            SDL_PIXELFORMAT_RGB24,
            SDL_TEXTUREACCESS_STREAMING,
            w,
            h,
        );
        if TEXTURE.is_null() {
            TEXTURE_W = 0;
            TEXTURE_H = 0;
            DISPLAY_READY = false;
            return;
        }
        SDL_RenderSetLogicalSize(RENDERER, w, h);
        if !WINDOW.is_null() {
            SDL_SetWindowSize(WINDOW, w * SCALE_HINT, h * SCALE_HINT);
        }
        TEXTURE_W = w;
        TEXTURE_H = h;
        DISPLAY_READY = true;
        // A fresh texture is blank; keep the logo up (through the game's mode-init
        // window resizes) until it has been run out. The splash keeps its own
        // texture, re-fitted to the new geometry on the next draw.
        if !GAME_HAS_PRESENTED {
            draw_splash_at(255);
        }
    }
}

// Host cursor keys (dedicated arrows AND the numeric keypad) both map to the
// PLAIN keypad scancodes — the XT/84-key cursor model; see the SDLK_UP note in
// handle_events for why (stream-sampling games need single-byte make/break).
// The grey-cluster emission (0xE0 + NumLock fake-shift framing, keyboard.rs)
// stays available via `ext` for callers that need the 101-key variant.
// `ext` records which variant was pressed so the release matches it.
fn release_pressed_scancodes() {
    unsafe {
        for i in 0..128 {
            if PRESSED_SCANCODES[i] {
                PRESSED_SCANCODES[i] = false;
                if PRESSED_EXT[i] {
                    shim_keyboard_enqueue_scancode_release_ext(i as u8);
                } else {
                    shim_keyboard_enqueue_scancode_release(i as u8);
                }
            }
        }
    }
}
fn mark_scancode_pressed(scancode: u8, ext: bool) {
    let scancode = scancode & 0x7F;
    if scancode == 0 {
        return;
    }
    unsafe {
        if !PRESSED_SCANCODES[scancode as usize] {
            PRESSED_SCANCODES[scancode as usize] = true;
            PRESSED_EXT[scancode as usize] = ext;
            if ext {
                shim_keyboard_enqueue_scancode_press_ext(scancode);
            } else {
                shim_keyboard_enqueue_scancode_press(scancode);
            }
        }
    }
}
fn mark_scancode_released(scancode: u8) {
    let scancode = scancode & 0x7F;
    if scancode == 0 {
        return;
    }
    unsafe {
        if PRESSED_SCANCODES[scancode as usize] {
            PRESSED_SCANCODES[scancode as usize] = false;
            if PRESSED_EXT[scancode as usize] {
                shim_keyboard_enqueue_scancode_release_ext(scancode);
            } else {
                shim_keyboard_enqueue_scancode_release(scancode);
            }
        }
    }
}
fn mark_ascii_pressed(ascii: u8) {
    if ascii == 0 {
        return;
    }
    let scancode = ascii_to_scancode(ascii);
    if scancode == 0 {
        return;
    }
    unsafe {
        if PRESSED_SCANCODES[scancode as usize] {
            return;
        }
        PRESSED_SCANCODES[scancode as usize] = true;
        shim_keyboard_enqueue(ascii);
    }
}

fn sync_pressed_scancodes_with_keyboard_state() {
    unsafe {
        SDL_PumpEvents();
        let mut num_keys: c_int = 0;
        let state = SDL_GetKeyboardState(&mut num_keys);
        for (sdl_sc, dos_sc) in [
            (SDL_SCANCODE_UP, 0x48u8),
            (SDL_SCANCODE_DOWN, 0x50),
            (SDL_SCANCODE_LEFT, 0x4B),
            (SDL_SCANCODE_RIGHT, 0x4D),
        ] {
            let is_down = sdl_sc < num_keys && *state.add(sdl_sc as usize) != 0;
            if is_down {
                // plain keypad cursor codes — see the SDLK_UP note in handle_events
                mark_scancode_pressed(dos_sc, false);
            } else {
                mark_scancode_released(dos_sc);
            }
        }
    }
}

/// Turn a `game_config` bundle id into a display name. The id is lowercase and
/// often carries platform/locale tags (`alleycat`, `kings_bounty_dos_en`); we
/// split on `_`/`-`, drop trailing bundle tokens, and title-case so the window
/// reads like the game ("Kings Bounty", not "kings_bounty_dos_en"). Returns
/// `None` for an empty/all-noise id.
fn prettify_name(raw: &str) -> Option<String> {
    let mut words: Vec<&str> = raw
        .split(|c: char| c == '_' || c == '-' || c == ' ')
        .filter(|w| !w.is_empty())
        .collect();
    // Trailing tokens that describe the bundle, not the game.
    const NOISE: &[&str] = &[
        "dos", "cd", "floppy", "disk", "en", "fr", "de", "es", "it", "nl", "pt", "us", "uk", "eu",
        "jp", "v1", "v2",
    ];
    while words.len() > 1 && NOISE.contains(&words.last().unwrap().to_ascii_lowercase().as_str()) {
        words.pop();
    }
    if words.is_empty() {
        return None;
    }
    Some(
        words
            .iter()
            .map(|w| {
                let mut chars = w.chars();
                let first = chars.next().unwrap().to_ascii_uppercase();
                first.to_string() + &chars.as_str().to_ascii_lowercase()
            })
            .collect::<Vec<_>>()
            .join(" "),
    )
}

/// The running game's display name from the linked-in `game_config` (`None`
/// when no game is linked — weak-default config / shim tests).
fn pretty_game_name() -> Option<String> {
    let ptr = crate::shims::game_config.name;
    if ptr.is_null() {
        return None;
    }
    let raw = unsafe { core::ffi::CStr::from_ptr(ptr) }.to_string_lossy();
    prettify_name(&raw)
}

/// Assemble the window title: "Saisei - <Game>" (plain "Saisei" when no game is
/// known), with an optional trailing suffix like a buffer label.
///
/// ASCII only, on purpose. X11 window managers render the titlebar from their
/// own font and legacy `WM_NAME`; non-ASCII there is unreliable — a color emoji
/// or even a BMP symbol/em-dash shows as a tofu box, and a WM that reads
/// `WM_NAME` as Latin-1 turns the UTF-8 bytes into mojibake. Sticking to ASCII
/// makes the title render identically across every WM.
fn format_title(game: Option<&str>, suffix: Option<&str>) -> String {
    let mut title = match game {
        Some(game) => format!("Saisei - {game}"),
        None => "Saisei".to_string(),
    };
    if let Some(suffix) = suffix {
        title.push_str(" (");
        title.push_str(suffix);
        title.push(')');
    }
    title
}

/// The title as a `CString`. SDL copies the string, so it need only outlive the
/// set/create call.
fn window_title(suffix: Option<&str>) -> std::ffi::CString {
    let title = format_title(pretty_game_name().as_deref(), suffix);
    // The format above never produces an interior NUL, but fall back defensively
    // rather than unwrap-panic in the display path.
    std::ffi::CString::new(title).unwrap_or_else(|_| std::ffi::CString::new("Saisei").unwrap())
}

unsafe fn set_window_title(suffix: Option<&str>) {
    SDL_SetWindowTitle(WINDOW, window_title(suffix).as_ptr());
}

fn handle_events() {
    unsafe {
        SDL_PumpEvents();
        let mut e: SdlEvent = SdlEvent { padding: [0; 56] };
        while SDL_PollEvent(&mut e) != 0 {
            match e.type_ {
                SDL_QUIT => {
                    save_manager_sr_log(c"exit SDL_QUIT window_close clean_exit".as_ptr());
                    fflush(core::ptr::null_mut());
                    SDL_Quit();
                    exit(0);
                }
                SDL_WINDOWEVENT => match e.window.event {
                    SDL_WINDOWEVENT_SIZE_CHANGED => {}
                    SDL_WINDOWEVENT_FOCUS_LOST
                    | SDL_WINDOWEVENT_MINIMIZED
                    | SDL_WINDOWEVENT_HIDDEN
                    | SDL_WINDOWEVENT_LEAVE => release_pressed_scancodes(),
                    _ => {}
                },
                SDL_KEYDOWN => {
                    let k = &e.key;
                    let sym = k.keysym.sym;
                    if sym == SDLK_LCTRL || sym == SDLK_RCTRL {
                        CTRL_PRESSED = true;
                        continue;
                    }
                    let mods = k.keysym.r#mod;
                    let ctrl_down = CTRL_PRESSED || (mods & KMOD_CTRL) != 0;
                    if ctrl_down && k.keysym.scancode == SDL_SCANCODE_P {
                        if k.repeat == 0 {
                            shim_save_video_memory();
                        }
                        continue;
                    }
                    if mods & KMOD_GUI != 0 {
                        match sym {
                            SDLK_1 => {
                                virtual_display_buffer = 0;
                                set_window_title(Some("Main Buffer"));
                            }
                            SDLK_2 => {
                                virtual_display_buffer = 1;
                                set_window_title(Some("First Buffer"));
                            }
                            SDLK_3 => {
                                virtual_display_buffer = 2;
                                set_window_title(Some("Second Buffer"));
                            }
                            SDLK_F1 => {
                                if k.repeat == 0 {
                                    save_manager_request_save()
                                }
                            }
                            SDLK_F2 => {
                                if k.repeat == 0 {
                                    save_manager_request_load_latest()
                                }
                            }
                            _ => {}
                        }
                        continue;
                    }
                    if sym == SDLK_F9 {
                        if k.repeat == 0 {
                            shim_bookend_start();
                        }
                        continue;
                    }
                    if sym == SDLK_F10 {
                        if k.repeat == 0 {
                            shim_bookend_stop();
                        }
                        continue;
                    }
                    let ksc = k.keysym.scancode;
                    if (SDL_SCANCODE_KP_1..=SDL_SCANCODE_KP_1 + 9).contains(&ksc) {
                        let sc = sdl_kp_to_dos(ksc);
                        if sc != 0 {
                            mark_scancode_pressed(sc, false);
                        }
                        continue;
                    }
                    match sym {
                        // Dedicated arrows emit the PLAIN keypad cursor codes
                        // (XT/84-key model, single byte per make/break), NOT the
                        // grey-cluster E0(+NumLock fake-shift) framing: games
                        // that sample the port-0x60 stream without hooking
                        // INT 9 (e.g. Zeliard) key on the burst's final byte,
                        // and only the single-byte form ends every event on the
                        // true make/break code. Verified live: with grey
                        // framing, Zeliard never sees the arrow break (the
                        // burst ends on the fake-shift 0xAA) and the character
                        // runs forever; with plain codes press/release track
                        // correctly.
                        SDLK_UP => mark_scancode_pressed(0x48, false),
                        SDLK_DOWN => mark_scancode_pressed(0x50, false),
                        SDLK_LEFT => mark_scancode_pressed(0x4B, false),
                        SDLK_RIGHT => mark_scancode_pressed(0x4D, false),
                        SDLK_RETURN => mark_ascii_pressed(b'\r'),
                        SDLK_ESCAPE => mark_ascii_pressed(27),
                        SDLK_BACKSPACE => mark_ascii_pressed(0x08),
                        SDLK_TAB => mark_ascii_pressed(b'\t'),
                        _ => {
                            if sym > 0 && sym < 0x80 {
                                mark_ascii_pressed(sym as u8);
                            }
                        }
                    }
                }
                SDL_KEYUP => {
                    let k = &e.key;
                    let sym = k.keysym.sym;
                    if sym == SDLK_LCTRL || sym == SDLK_RCTRL {
                        CTRL_PRESSED = false;
                        continue;
                    }
                    let ksc = k.keysym.scancode;
                    if (SDL_SCANCODE_KP_1..=SDL_SCANCODE_KP_1 + 9).contains(&ksc) {
                        let sc = sdl_kp_to_dos(ksc);
                        if sc != 0 {
                            mark_scancode_released(sc);
                        }
                        continue;
                    }
                    match sym {
                        SDLK_UP => mark_scancode_released(0x48),
                        SDLK_DOWN => mark_scancode_released(0x50),
                        SDLK_LEFT => mark_scancode_released(0x4B),
                        SDLK_RIGHT => mark_scancode_released(0x4D),
                        SDLK_RETURN => mark_scancode_released(0x1C),
                        SDLK_ESCAPE => mark_scancode_released(0x01),
                        SDLK_BACKSPACE => mark_scancode_released(0x0E),
                        SDLK_TAB => mark_scancode_released(0x0F),
                        _ => {
                            if sym > 0 && sym < 0x80 {
                                let sc = ascii_to_scancode(sym as u8);
                                if sc != 0 {
                                    mark_scancode_released(sc);
                                }
                            }
                        }
                    }
                }
                SDL_MOUSEMOTION => {
                    mouse_host_motion(e.motion.x, e.motion.y, WIN_W, WIN_H);
                }
                SDL_MOUSEBUTTONDOWN | SDL_MOUSEBUTTONUP => {
                    mouse_host_motion(e.button.x, e.button.y, WIN_W, WIN_H);
                    let b = if e.button.button == SDL_BUTTON_RIGHT {
                        1
                    } else if e.button.button == SDL_BUTTON_MIDDLE {
                        2
                    } else {
                        0
                    };
                    mouse_host_button(b, (e.type_ == SDL_MOUSEBUTTONDOWN) as c_int);
                }
                _ => {}
            }
        }
        sync_pressed_scancodes_with_keyboard_state();
    }
}

// ---- public API -------------------------------------------------------------

#[no_mangle]
pub extern "C" fn virtual_display_poll_input() {
    unsafe {
        if SDL_WasInit(SDL_INIT_VIDEO) == 0 || !DISPLAY_READY {
            return;
        }
        let now = SDL_GetTicks();
        // SDL_TICKS_PASSED(now, last+4): (last+4 - now) <= 0 as i32
        if (LAST_POLL_TICKS.wrapping_add(4).wrapping_sub(now)) as i32 > 0 {
            return;
        }
        LAST_POLL_TICKS = now;
        handle_events();
        save_manager_poll_pending();
    }
}

#[no_mangle]
pub extern "C" fn virtual_display_init(width: c_int, height: c_int, scale: c_int) {
    unsafe {
        DISPLAY_READY = false;
        if scale > 0 {
            SCALE_HINT = scale;
        }
        WIN_W = width;
        WIN_H = height;
        SDL_SetHint(c"SDL_NO_SIGNAL_HANDLERS".as_ptr(), c"1".as_ptr());
        if SDL_Init(SDL_INIT_VIDEO) != 0 {
            return;
        }
        shim_reinstall_crash_handlers();
        SDL_SetHint(c"SDL_RENDER_SCALE_QUALITY".as_ptr(), c"0".as_ptr());
        let title = window_title(None);
        WINDOW = SDL_CreateWindow(
            title.as_ptr(),
            SDL_WINDOWPOS_UNDEFINED,
            SDL_WINDOWPOS_UNDEFINED,
            width * SCALE_HINT,
            height * SCALE_HINT,
            SDL_WINDOW_RESIZABLE,
        );
        if WINDOW.is_null() {
            SDL_Quit();
            return;
        }
        RENDERER = SDL_CreateRenderer(WINDOW, -1, SDL_RENDERER_ACCELERATED);
        if RENDERER.is_null() {
            SDL_DestroyWindow(WINDOW);
            WINDOW = core::ptr::null_mut();
            SDL_Quit();
            return;
        }
        recreate_texture(width, height);
    }
}

#[no_mangle]
pub extern "C" fn virtual_display_shutdown() {
    unsafe {
        free(RGB_BUFFER as *mut c_void);
        RGB_BUFFER = core::ptr::null_mut();
        RGB_BUFFER_SIZE = 0;
        destroy_splash_texture();
        if !TEXTURE.is_null() {
            SDL_DestroyTexture(TEXTURE);
            TEXTURE = core::ptr::null_mut();
        }
        if !RENDERER.is_null() {
            SDL_DestroyRenderer(RENDERER);
            RENDERER = core::ptr::null_mut();
        }
        if !WINDOW.is_null() {
            SDL_DestroyWindow(WINDOW);
            WINDOW = core::ptr::null_mut();
        }
        DISPLAY_READY = false;
        TEXTURE_W = 0;
        TEXTURE_H = 0;
        SDL_Quit();
    }
}

#[no_mangle]
pub extern "C" fn virtual_display_present(
    vram: *const u8,
    pitch: c_int,
    w: c_int,
    h: c_int,
    palette: *const u8,
    palette_mask: u8,
) {
    handle_events();
    unsafe {
        if TEXTURE.is_null() || RENDERER.is_null() || vram.is_null() || palette.is_null() {
            return;
        }
        // The logo owns the window until it has had its full run. The game's
        // first real frame waits out the hold + dissolve here, so it can never
        // appear under a logo that is still fading.
        retire_splash();
        ensure_rgb_buffer(w, h);
        for y in 0..h {
            let src = vram.add((y * pitch) as usize);
            let dst = RGB_BUFFER.add((y * w * 3) as usize);
            for x in 0..w {
                let idx = *src.add(x as usize) as usize;
                let r6 = *palette.add(idx * 3) & palette_mask;
                let g6 = *palette.add(idx * 3 + 1) & palette_mask;
                let b6 = *palette.add(idx * 3 + 2) & palette_mask;
                *dst.add((x * 3) as usize) = vga6_to_8(r6);
                *dst.add((x * 3 + 1) as usize) = vga6_to_8(g6);
                *dst.add((x * 3 + 2) as usize) = vga6_to_8(b6);
            }
        }
        SDL_UpdateTexture(
            TEXTURE,
            core::ptr::null(),
            RGB_BUFFER as *const c_void,
            w * 3,
        );
        SDL_RenderClear(RENDERER);
        SDL_RenderCopy(RENDERER, TEXTURE, core::ptr::null(), core::ptr::null());
        SDL_RenderPresent(RENDERER);
    }
}

#[no_mangle]
pub extern "C" fn virtual_display_set_mode(_mode: c_int) {}

#[no_mangle]
pub extern "C" fn virtual_display_configure(width: c_int, height: c_int) {
    unsafe {
        WIN_W = width;
        WIN_H = height;
    }
    recreate_texture(width, height);
}

#[cfg(test)]
mod splash_tests {
    use super::{build_splash_rgb, LOGO_H, LOGO_W};

    // Lit art (the tree, the wordmark) vs the black field it sits on.
    fn is_lit(rgb: &[u8]) -> bool {
        rgb[0] > 0x30 || rgb[1] > 0x30 || rgb[2] > 0x30
    }

    #[test]
    fn splash_paints_the_logo_centered_on_black() {
        // The size the splash texture actually gets for a 320x200 game (4x).
        let (w, h) = (1280usize, 800usize);
        let buf = build_splash_rgb(w, h);
        assert_eq!(buf.len(), w * h * 3);

        let lit = |x: usize, y: usize| is_lit(&buf[(y * w + x) * 3..][..3]);

        // The art is black-backed and aspect-fitted, so the corners stay black —
        // the letterbox bars are indistinguishable from the logo's own field.
        for &(x, y) in &[(0usize, 0usize), (w - 1, 0), (0, h - 1), (w - 1, h - 1)] {
            assert!(!lit(x, y), "corner ({x},{y}) should be black");
        }

        // Aspect preserved: the logo is height-bound here, so the columns outside
        // the fitted width are bars, and nothing is drawn in them.
        let dw = LOGO_W * h / LOGO_H;
        let x0 = (w - dw) / 2;
        for y in (0..h).step_by(7) {
            for x in (0..x0.saturating_sub(1)).step_by(7) {
                assert!(!lit(x, y), "art bled into the letterbox bar at ({x},{y})");
                assert!(!lit(w - 1 - x, y), "art bled into the right bar at row {y}");
            }
        }

        let total = buf.chunks_exact(3).filter(|p| is_lit(p)).count();
        assert!(
            total > w * h / 20,
            "expected the logo to cover a substantial area, got {total} lit px"
        );
    }

    // Dump a PNG of the splash for eyeball validation:
    //   cargo test -p saisei-runtime --lib splash_dump_png -- --ignored --nocapture
    #[test]
    #[ignore]
    fn splash_dump_png() {
        let (w, h) = (1280usize, 800usize);
        let buf = build_splash_rgb(w, h);
        let path = std::env::temp_dir().join("saisei_splash.png");
        crate::video::write_png_for_test(path.to_str().unwrap(), w, h, &buf);
        eprintln!("wrote {}", path.display());
    }
}

#[cfg(test)]
mod title_tests {
    use super::{format_title, prettify_name};

    #[test]
    fn prettify_strips_bundle_tags_and_title_cases() {
        assert_eq!(prettify_name("zeliard").as_deref(), Some("Zeliard"));
        assert_eq!(prettify_name("alleycat").as_deref(), Some("Alleycat"));
        assert_eq!(
            prettify_name("kings_bounty_dos_en").as_deref(),
            Some("Kings Bounty")
        );
        assert_eq!(prettify_name("popcorn_dos_fr").as_deref(), Some("Popcorn"));
        assert_eq!(
            prettify_name("dungeon-master").as_deref(),
            Some("Dungeon Master")
        );
    }

    #[test]
    fn prettify_keeps_at_least_one_word() {
        // An id that is *all* bundle tokens must not vanish entirely.
        assert_eq!(prettify_name("dos").as_deref(), Some("Dos"));
        assert_eq!(prettify_name("").as_deref(), None);
        assert_eq!(prettify_name("___").as_deref(), None);
    }

    #[test]
    fn format_matches_chosen_style() {
        assert_eq!(format_title(Some("Zeliard"), None), "Saisei - Zeliard");
        assert_eq!(
            format_title(Some("Zeliard"), Some("Main Buffer")),
            "Saisei - Zeliard (Main Buffer)"
        );
        assert_eq!(format_title(None, None), "Saisei");
        // Title must be pure ASCII so every X11 WM titlebar renders it verbatim.
        assert!(format_title(Some("Zeliard"), Some("Main Buffer")).is_ascii());
    }
}
