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
    fn SDL_GetError() -> *const c_char;
    fn SDL_GetTicks() -> u32;
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
const SDL_TEXTUREACCESS_STREAMING: c_int = 1;
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
                                SDL_SetWindowTitle(WINDOW, c"Saisei - Main Buffer".as_ptr());
                            }
                            SDLK_2 => {
                                virtual_display_buffer = 1;
                                SDL_SetWindowTitle(WINDOW, c"Saisei - First Buffer".as_ptr());
                            }
                            SDLK_3 => {
                                virtual_display_buffer = 2;
                                SDL_SetWindowTitle(WINDOW, c"Saisei - Second Buffer".as_ptr());
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
        WINDOW = SDL_CreateWindow(
            c"Saisei".as_ptr(),
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
