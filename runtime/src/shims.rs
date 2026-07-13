//! Port of `runtime/core/shims.c` — the runtime's integration core.
//!
//! Faithful, line-by-line translation of the C. Defines the runtime globals and
//! the big integration surface (memory access, file_mappings, the JIT registry,
//! dispatch, crash bundles, the function-patch registry). Exports the identical
//! C ABI (`#[no_mangle] extern "C"`) so the JIT chunks and the sibling modules
//! resolve against it unchanged.

#![allow(non_upper_case_globals)]
#![allow(non_camel_case_types)]
#![allow(non_snake_case)]
#![allow(unused_imports)]
#![allow(unused_parens)]
#![allow(dead_code)]

use core::ffi::{
    c_char, c_int, c_long, c_longlong, c_uint, c_ulong, c_ulonglong, c_ushort, c_void, VaList,
};
use core::ptr;
use libc::FILE;

use crate::cpu::*;

/// `cstr!("...")` → a NUL-terminated `*const c_char` (a static byte string).
macro_rules! cstr {
    ($s:expr) => {
        concat!($s, "\0").as_ptr() as *const c_char
    };
}

// ---- glibc stdio stream globals (not exported by the libc crate) ----
extern "C" {
    static mut stdin: *mut FILE;
    static mut stdout: *mut FILE;
    static mut stderr: *mut FILE;
    fn vsnprintf(s: *mut c_char, n: usize, fmt: *const c_char, ap: VaList) -> c_int;
    fn vfprintf(stream: *mut FILE, fmt: *const c_char, ap: VaList) -> c_int;
}

/// The C `__FILE__` for shims.c call sites (feeds diagnostic logging only).
const SHIMS_FILE: *const c_char = cstr!("core/shims.c");

#[repr(C)]
#[derive(Clone, Copy)]
pub struct PITState {
    pub reload: u32,
    pub temp_reload: u16,
    pub expect_high: u8,
    pub access_mode: u8,
}

// ---- symbols defined by the sibling Rust modules (resolved in-crate) ----
extern "C" {
    // timer.rs
    fn shim_virtual_now_ns() -> u64;
    static mut vclock_state: c_int;
    static mut vclock_frozen_virtual_ns: u64;
    static mut bios_timer_tick_preincremented: u8;
    static mut pit: PITState;
    static mut pit_cycle_accum: u64;
    static mut pit_cycle_fraction_accum: u64;
    static mut bios_timer_tick_backlog: u32;
    static mut jit_instr_budget: i64;
    static mut jit_budget_last_refill: i64;
    static mut jit_total_retired: u64;
    static mut jit_ns_per_instr: u64;
    static mut virtual_now_accum_ns: u64;
    fn vclock_advance_ns(ns: u64);
    fn vclock_service();
    fn vclock_halt();
    fn vclock_resume();
    fn vclock_step(ticks: u32);
    fn bios_timer_increment();
    // keyboard.rs
    fn shim_keyboard_enqueue_scancode_release(scancode: u8);
    fn shim_keyboard_enqueue_scancode_press(scancode: u8);
    fn shim_keyboard_enqueue_scancode_release_ext(scancode: u8);
    fn shim_keyboard_enqueue_scancode_press_ext(scancode: u8);
    fn kbd_enqueue(ascii: u8, scancode: u8);
    fn kbd_queue_push(ascii: u8, scancode: u8);
    // mouse.rs
    fn mouse_host_inject(x: i16, y: i16, buttons: u16);
    fn mouse_deliver_pending_events();
    // video.rs
    fn stage_and_present_current_buffer();
    // save_manager.rs
    fn save_manager_poll_pending();
    fn save_manager_request_save();
    // dos.rs
    fn log_last_sw_interrupt_snapshot();
    fn dos_set_current_psp_to_load();
    // video.rs
    static mut bios_video: BiosVideoState;
    static mut cga: CgaState;
    static mut vga: VgaState;
    fn video_invalidate_palette_cache();
    fn vga_dac_component(value: u8) -> u8;
    fn apply_video_mode_state(mode: u8);
    // keyboard.rs
    static mut kbd: KbdState;
    fn kbd_consume();
    // timer.rs (extra statics + fns)
    static mut pit_channel1: PITState;
    static mut pit_channel2: PITState;
    static mut pit_reload_value: u32;
    static mut pit_latched_value: u16;
    static mut pit_latch_valid: u8;
    static mut pit_read_buffer: u16;
    static mut pit_read_expect_high: u8;
    static mut pit_read_buffer_is_latch: u8;
    fn pit_current_count() -> u16;
    fn shim_scaled_monotonic_ns() -> u64;
    // snapshot.rs
    fn snapshot_on_key_consumed();
    // io_bus.rs
    fn io_bus_lookup(port: u16) -> *const IoDevice;
    // bios.rs (INT 10h/11h/16h services)
    fn bios_set_video_mode_impl(mode: u8, file: *const c_char, func: *const c_char, line: c_int);
    fn bios_set_cursor_position_impl(
        page: u8,
        row: u8,
        col: u8,
        file: *const c_char,
        func: *const c_char,
        line: c_int,
    );
    fn bios_get_cursor(page: u8);
    fn bios_read_char_attr() -> u16;
    fn bios_write_char_attr(glyph: u8, page: u8, attr: u8, count: u16);
    fn bios_write_char_only(glyph: u8, page: u8, count: u16);
    fn bios_scroll_window(
        lines: u8,
        attr: u8,
        top: u8,
        left: u8,
        bottom: u8,
        right: u8,
        down: c_int,
    );
    fn bios_set_cga_palette_impl(
        bh_val: u8,
        bl_val: u8,
        file: *const c_char,
        func: *const c_char,
        line: c_int,
    );
    fn bios_teletype_output_impl(
        ch_val: u8,
        page: u8,
        attr: u8,
        file: *const c_char,
        func: *const c_char,
        line: c_int,
    );
    fn bios_keyboard_impl(file: *const c_char, func: *const c_char, line: c_int);
    fn bios_current_video_mode() -> u8;
    fn bios_current_video_columns() -> u8;
    fn bios_current_active_page() -> u8;
    fn bios_display_combination_code() -> u8;
    fn bios_display_combination_alt_code() -> u8;
    fn bios_get_video_parameter_block(mode: u8, seg: *mut u16, off: *mut u16);
    fn bios_set_palette_impl(file: *const c_char, func: *const c_char, line: c_int);
    fn bios_video_alt_select_impl(file: *const c_char, func: *const c_char, line: c_int);
    // dos.rs
    fn dos_exit_impl(file: *const c_char, func: *const c_char, line: c_int);
    fn dos_api_impl(file: *const c_char, func: *const c_char, line: c_int) -> u8;
    // mouse.rs
    fn mouse_int33_impl(file: *const c_char, func: *const c_char, line: c_int);
    // keyboard.rs
    fn kbd_bios_deposit_from_isr();
    // video.rs
    fn shim_render_screenshot_png(path: *const c_char) -> c_int;
    // NOTE (for human integrator): stbi_write_png was instantiated by shims.c's
    // STB_IMAGE_WRITE_IMPLEMENTATION. With shims.c removed, no TU exports it.
    // shim_save_video_memory's palette-PNG dump calls it. This is an undefined
    // extern (OK for the staticlib build) that must be resolved at final link —
    // either keep a tiny stb TU, or export a stbi_write_png-compatible fn from
    // video.rs (it already has private encode_png/write_png_file). See SHIMS_PROGRESS.md.
    fn stbi_write_png(
        filename: *const c_char,
        w: c_int,
        h: c_int,
        comp: c_int,
        data: *const c_void,
        stride_bytes: c_int,
    ) -> c_int;
    // audio.rs
    static mut opl2: Opl2State;
    // snapshot.rs
    fn snapshot_init();
    fn snapshot_restore_and_resume(bundle_dir: *const c_char) -> c_int;
    // save_manager.rs (variadic)
    fn save_manager_sr_log(fmt: *const c_char, ...);
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct Opl2State {
    pub address: u8,
    pub registers: [u8; 256],
    pub status: u8,
    pub busy_until_us: u64,
    pub timer1_expire_us: u64,
    pub timer2_expire_us: u64,
}

const SHIM_RUNTIME_STATE_VERSION: u32 = 6;

#[repr(C)]
pub struct ShimRuntimeState {
    pub version: u32,
    pub bios_video: BiosVideoState,
    pub cga: CgaState,
    pub current_display_width: i32,
    pub current_display_height: i32,
    pub virtual_display_buffer: i32,
    pub vga: VgaState,
    pub opl2: Opl2State,
    pub pit: PITState,
    pub pit_reload_value: u32,
    pub pit_latched_value: u16,
    pub pit_latch_valid: u8,
    pub pit_read_buffer: u16,
    pub pit_read_expect_high: u8,
    pub pit_read_buffer_is_latch: u8,
    pub bios_timer_tick_backlog: u32,
    pub bios_timer_tick_preincremented: u8,
    pub pit_cycle_accum: u64,
    pub pit_cycle_fraction_accum: u64,
    pub next_free_seg: u16,
    pub program_min_block_paras: u16,
    pub null_guard_initial: [u8; 16],
    pub a20_enabled: u8,
    pub irq0_pending: u8,
    pub irq_pending: [u8; 256],
    pub last_int_no: u8,
}

#[repr(C)]
pub struct ShimTailDispatchState {
    pub pending: bool,
    pub addr: u32,
    pub expected: u16,
}

const SHIM_KBD_BUFFER_SIZE: usize = 64;
#[repr(C)]
pub struct ShimKbdState {
    pub q_ascii: [u8; SHIM_KBD_BUFFER_SIZE],
    pub q_scan: [u8; SHIM_KBD_BUFFER_SIZE],
    pub head: c_int,
    pub tail: c_int,
    pub count: c_int,
    pub cur_ascii: u8,
    pub cur_scan: u8,
    pub last_scan: u8,
    pub ready: u8,
}

#[repr(C)]
pub struct ShimFileMappingView {
    pub base: u32,
    pub len: usize,
    pub file_offset: usize,
    pub canonical_cs: u16,
    pub path: *const c_char,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct CgaState {
    pub crtc_index: u8,
    pub crtc_regs: [u8; 0x20],
    pub hsync_base: u8,
    pub horiz_scroll: i32,
    pub hsync_initialized: u8,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct VgaState {
    pub palette: [u8; 256 * 3],
    pub palette_write_index: u8,
    pub palette_component: u8,
    pub palette_read_index: u8,
    pub palette_mask: u8,
    pub attr_palette: [u8; 16],
    pub border_color: u8,
    pub blink_mode: u8,
    pub dac_paging_mode: u8,
    pub dac_current_page: u8,
    pub misc_output: u8,
    pub feature_control: u8,
    pub graphics_index: u8,
    pub graphics_regs: [u8; 16],
}

const KBD_BUFFER_SIZE: usize = 64;
#[repr(C)]
#[derive(Clone, Copy)]
pub struct KbdEntry {
    pub ascii: u8,
    pub scancode: u8,
}
#[repr(C)]
pub struct KbdState {
    pub queue: [KbdEntry; KBD_BUFFER_SIZE],
    pub queue_head: i32,
    pub queue_tail: i32,
    pub queue_count: i32,
    pub ascii: u8,
    pub scancode: u8,
    pub last_scancode: u8,
    pub scancode_ready: i32,
    pub bios_buf: [KbdEntry; KBD_BUFFER_SIZE],
    pub bios_head: i32,
    pub bios_tail: i32,
    pub bios_count: i32,
    pub pending_bios_ascii: u8,
    pub pending_bios_scancode: u8,
    pub pending_bios_valid: i32,
}

#[repr(C)]
pub struct IoDevice {
    pub name: *const c_char,
    pub ports: *const u16,
    pub read8: Option<extern "C" fn(port: u16) -> u8>,
    pub write8: Option<extern "C" fn(port: u16, value: u8)>,
}

// RCBField enum values (rcb_fields.h); passed across the ABI as `int`.
const FIELD_1: c_int = 0xFF00;
const PROGRAM_SEG: c_int = 0xFF02;
const PREV_TIMER_VECTOR_OFF: c_int = 0xFF04;
const PREV_TIMER_VECTOR_SEG: c_int = 0xFF06;
const FIELD_5: c_int = 0xFF08;
const FIELD_6: c_int = 0xFF09;
const JOYSTICK_FLAG: c_int = 0xFF0A;
const FIELD_8: c_int = 0xFF0B;
const DATA_BUF1_OFF: c_int = 0xFF0C;
const DATA_BUF1_SEG: c_int = 0xFF0E;
const DATA_BUF2_OFF: c_int = 0xFF10;
const DATA_BUF2_SEG: c_int = 0xFF12;
const VIDEO_DRIVER_INDEX: c_int = 0xFF14;
const MUSIC_DRIVER_FLAG: c_int = 0xFF15;
const FIELD_15: c_int = 0xFF16;
const FIELD_16: c_int = 0xFF17;
const FIELD_17: c_int = 0xFF18;
const FIELD_18: c_int = 0xFF1D;
const FIELD_19: c_int = 0xFF1E;
const FIELD_20: c_int = 0xFF1F;
const FIELD_21: c_int = 0xFF26;
const FIELD_22: c_int = 0xFF27;
const FIELD_23: c_int = 0xFF28;
const DATA_BASE_SEG: c_int = 0xFF2C;
const FIELD_25: c_int = 0xFF33;
const FIELD_26: c_int = 0xFF34;
const FIELD_27: c_int = 0xFF38;
const FIELD_28: c_int = 0xFF39;
const FIELD_29: c_int = 0xFF3A;
const FIELD_30: c_int = 0xFF3B;
const FIELD_31: c_int = 0xFF3C;
const FIELD_32: c_int = 0xFF40;
const FIELD_33: c_int = 0xFF42;
const FIELD_34: c_int = 0xFF43;
const FIELD_35: c_int = 0xFF74;
const FIELD_36: c_int = 0xFF75;
const FIELD_37: c_int = 0xFF78;
const PREV_KEYBOARD_VECTOR_OFF: c_int = 0xFF79;
const PREV_KEYBOARD_VECTOR_SEG: c_int = 0xFF7B;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct BiosVideoState {
    pub video_mode: u8,
    pub cursor_row: [u8; 8],
    pub cursor_col: [u8; 8],
    pub cursor_attr: [u8; 8],
    pub active_page: u8,
    pub cga_palette_select: u8,
    pub cga_border_color: u8,
}

const BIOS_VIDEO_PARAM_SEG: u16 = 0xF000;
const BIOS_VIDEO_PARAM_OFF: u16 = 0x0200;

// vclock_state_t enum values (mirror timer.h / timer.rs).
const VCLOCK_RUNNING: c_int = 0;
const VCLOCK_HALTED: c_int = 1;
const VCLOCK_STEPPING: c_int = 2;

pub type ShimDispatchFn = DispatchFn;

#[repr(C)]
pub struct InterruptSnapshot {
    pub valid: u8,
    pub int_no: u8,
    pub ax_before: u16,
    pub bx_before: u16,
    pub cx_before: u16,
    pub dx_before: u16,
    pub ds_before: u16,
    pub es_before: u16,
    pub ss_before: u16,
    pub sp_before: u16,
    pub cs_before: u16,
    pub ip_before: u16,
    pub ax_after: u16,
    pub bx_after: u16,
    pub cx_after: u16,
    pub dx_after: u16,
    pub ds_after: u16,
    pub es_after: u16,
    pub ss_after: u16,
    pub sp_after: u16,
    pub cs_after: u16,
    pub ip_after: u16,
    pub file: *const c_char,
    pub func: *const c_char,
    pub line: c_int,
}

// ============================================================================
// Game config + generated-code function-pointer types (game_config.h)
// ============================================================================

pub type GameFunc = Option<
    unsafe extern "C" fn(
        expected_retip: u16,
        file: *const c_char,
        func: *const c_char,
        line: c_int,
    ),
>;

#[repr(C)]
pub struct CallTarget {
    pub addr: u32,
    pub file: *const c_char,
    pub fn_: GameFunc,
}

#[repr(C)]
pub struct ProtectedSlot {
    pub lo: u32,
    pub hi: u32,
    pub name: *const c_char,
}

pub type DispatchFn = Option<
    unsafe extern "C" fn(
        pc: c_int,
        expected_retip: u16,
        file: *const c_char,
        func: *const c_char,
        line: c_int,
    ),
>;

#[repr(C)]
pub struct BinaryDispatch {
    pub file: *const c_char,
    pub module: *const c_char,
    pub cs_base: u16,
    pub fn_: DispatchFn,
}

pub const PATCH_DECLINED: c_int = 0;
pub const PATCH_HANDLED: c_int = 1;

pub type PatchFn = Option<
    unsafe extern "C" fn(
        expected_retip: u16,
        file: *const c_char,
        func: *const c_char,
        line: c_int,
    ) -> c_int,
>;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct GamePatch {
    pub file: *const c_char,
    pub file_off: u32,
    pub fn_: PatchFn,
    pub name: *const c_char,
    pub enabled: c_int,
}

#[repr(C)]
pub struct GameConfig {
    pub name: *const c_char,
    pub program_path: *const c_char,
    pub entry: GameFunc,
    pub call_targets: *const CallTarget,
    pub call_target_count: usize,
    pub binary_dispatch: *const BinaryDispatch,
    pub binary_dispatch_count: usize,
    pub protected_slots: *const ProtectedSlot,
    pub protected_slot_count: usize,
    pub init_cs: u16,
    pub psp_seg: u16,
    pub patches: *const GamePatch,
    pub patch_count: usize,
}

// Weak default `game_config` so the runtime links standalone (the shim-test
// harness dlopens the runtime .a with no per-game config). A *frozen* per-game
// binary provides a STRONG `game_config` that overrides this at link time —
// that path is what the dispatch tables below are shaped for. The player host
// links no such symbol and instead calls `saisei_set_game_config` at run time,
// so one binary can run any bundle without a per-game relink.
//
// Read it through `cfg()`, never directly: a direct read sees the weak default
// even when the host has installed one.
unsafe impl Sync for GameConfig {}
static EMPTY_CSTR: [u8; 1] = [0];
#[linkage = "weak"]
#[no_mangle]
pub static game_config: GameConfig = GameConfig {
    name: EMPTY_CSTR.as_ptr() as *const c_char,
    program_path: EMPTY_CSTR.as_ptr() as *const c_char,
    entry: None,
    call_targets: core::ptr::null(),
    call_target_count: 0,
    binary_dispatch: core::ptr::null(),
    binary_dispatch_count: 0,
    protected_slots: core::ptr::null(),
    protected_slot_count: 0,
    init_cs: 0,
    psp_seg: 0,
    patches: core::ptr::null(),
    patch_count: 0,
};

// The host's run-time config, when it installed one. Null => the linked-in
// `game_config` (weak default, or a frozen build's strong override) applies.
static mut GAME_CONFIG_OVERRIDE: *const GameConfig = core::ptr::null();

/// Install a `GameConfig` at run time. The player host calls this — with the
/// config it read from `games/<name>/<name>.json` — before `saisei_main`, which
/// is what lets one binary run any bundle. The pointee must outlive the process.
#[no_mangle]
pub unsafe extern "C" fn saisei_set_game_config(config: *const GameConfig) {
    GAME_CONFIG_OVERRIDE = config;
}

/// The active game config: the host's, if it installed one, else the linked-in
/// `game_config`. Every read of the config goes through here.
#[inline]
pub fn cfg() -> &'static GameConfig {
    unsafe {
        if GAME_CONFIG_OVERRIDE.is_null() {
            &game_config
        } else {
            &*GAME_CONFIG_OVERRIDE
        }
    }
}

extern "C" {
    // Provided by sdl.rs (strong definitions).
    fn virtual_display_init(width: c_int, height: c_int, scale: c_int);
    fn virtual_display_shutdown();
    fn virtual_display_present(
        vram: *const u8,
        pitch: c_int,
        w: c_int,
        h: c_int,
        palette: *const u8,
        palette_mask: u8,
    );
    fn virtual_display_poll_input();
    fn virtual_display_set_mode(mode: c_int);
    fn virtual_display_configure(width: c_int, height: c_int);
}

// ============================================================================
// Compile-time constants mirrored from shims.h
// ============================================================================

const DEFAULT_PSP_SEG: u16 = 0x1000;
const MEMORY_SIZE: usize = 1 << 21;
const MEMORY_MASK: u32 = (MEMORY_SIZE - 1) as u32;
const CONVENTIONAL_TOP_SEG: u16 = 0xA000;
const MAX_DOS_HANDLES: usize = 20;
const PATH_MAX: usize = 4096;
const NAME_MAX: usize = 255;

// ============================================================================
// Trace/diagnostic stdout gate + centralized stream flush
// ============================================================================

/// Trace/diagnostic stdout is OFF by default; `--verbose` turns it on.
static mut shim_stdout_enabled: c_int = 0;

static mut trace_file_fp: *mut FILE = ptr::null_mut();
static mut lifecycle_fp: *mut FILE = ptr::null_mut();

unsafe fn shim_flush_all_streams() {
    libc::fflush(stdout);
    libc::fflush(stderr);
    if !trace_file_fp.is_null() {
        libc::fflush(trace_file_fp);
        let tf = libc::fileno(trace_file_fp);
        if tf >= 0 {
            libc::fsync(tf);
        }
    }
    if !lifecycle_fp.is_null() {
        libc::fflush(lifecycle_fp);
        let lf = libc::fileno(lifecycle_fp);
        if lf >= 0 {
            libc::fsync(lf);
        }
    }
    let out_fd = libc::fileno(stdout);
    let err_fd = libc::fileno(stderr);
    if out_fd >= 0 {
        libc::fsync(out_fd);
    }
    if err_fd >= 0 && err_fd != out_fd {
        libc::fsync(err_fd);
    }
}

/// atexit trampoline (C-ABI, no args) forwarding to shim_flush_all_streams.
extern "C" fn shim_flush_all_streams_atexit() {
    unsafe { shim_flush_all_streams() };
}

// ============================================================================
// Async-signal-safe crash handler
// ============================================================================

unsafe fn emit_crash_marker(fd: c_int, signum: c_int, name: *const c_char) {
    if fd < 0 {
        return;
    }
    let binary_name = shim_active_binary();
    let linear: u32 = ((cs() as u32) << 4) + ((ip() as u32) & 0xFFFF);
    let is_fault = signum == libc::SIGSEGV
        || signum == libc::SIGBUS
        || signum == libc::SIGILL
        || signum == libc::SIGFPE
        || signum == libc::SIGABRT;
    let hint: *const c_char = if is_fault {
        cstr!("[CRASH]   Host-level fault inside the translated case body. Search the trace tail for the last Trace:/[BUG] line — the case it was in is where the out-of-bounds memory access happened.\n")
    } else {
        cstr!("[CRASH]   EXTERNAL termination (timeout / Ctrl-C / closed pipe), NOT a fault. The cs:ip above is just where execution was when the signal arrived; the program did not crash.\n")
    };
    let mut buf = [0u8; 640];
    let mut n = libc::snprintf(
        buf.as_mut_ptr() as *mut c_char,
        buf.len(),
        cstr!(
            "\n[CRASH] terminated by signal %d (%s)\n[CRASH]   cs:ip=%04X:%04X linear=0x%05X\n[CRASH]   active_binary=%s\n[CRASH]   ax=%04X bx=%04X cx=%04X dx=%04X si=%04X di=%04X bp=%04X ss:sp=%04X:%04X\n[CRASH]   ds=%04X es=%04X\n[CRASH]   depths: lcall=%u isr=%u dispatch=%u critical=%u\n%s"
        ),
        signum,
        name,
        cs() as c_uint,
        ip() as c_uint,
        linear as c_uint,
        if binary_name.is_null() { cstr!("<none>") } else { binary_name },
        ax() as c_uint,
        bx() as c_uint,
        cx() as c_uint,
        dx() as c_uint,
        si() as c_uint,
        di() as c_uint,
        bp() as c_uint,
        ss() as c_uint,
        sp() as c_uint,
        ds() as c_uint,
        es() as c_uint,
        lcall_depth as c_uint,
        isr_depth as c_uint,
        dispatch_depth as c_uint,
        critical_depth as c_uint,
        hint,
    );
    if n <= 0 {
        return;
    }
    if n > buf.len() as c_int {
        n = buf.len() as c_int;
    }
    let mut off: isize = 0;
    while off < n as isize {
        let w = libc::write(
            fd,
            buf.as_ptr().offset(off) as *const c_void,
            (n as isize - off) as usize,
        );
        if w < 0 {
            if *libc::__errno_location() == libc::EINTR {
                continue;
            }
            break;
        }
        off += w as isize;
    }
    libc::fsync(fd);
}

unsafe extern "C" fn crash_signal_handler(signum: c_int) {
    // Restore the terminal FIRST. The `.fini_array` `cleanup_keyboard` only runs
    // on a CLEAN exit — a Ctrl-C (SIGINT), `timeout`'s SIGTERM, or a crash would
    // otherwise leave the user's tty in raw mode (symptom: `^M` echoed on Enter,
    // no line submitted, in the NEXT command they run). tcsetattr is the
    // universal tty-restore-in-handler idiom; safe here since we re-raise below.
    cleanup_keyboard();
    let name: *const c_char = match signum {
        libc::SIGSEGV => cstr!("SIGSEGV"),
        libc::SIGABRT => cstr!("SIGABRT"),
        libc::SIGBUS => cstr!("SIGBUS"),
        libc::SIGFPE => cstr!("SIGFPE"),
        libc::SIGILL => cstr!("SIGILL"),
        libc::SIGINT => cstr!("SIGINT"),
        libc::SIGTERM => cstr!("SIGTERM"),
        libc::SIGPIPE => cstr!("SIGPIPE"),
        _ => cstr!("?"),
    };
    let out_fd = libc::fileno(stdout);
    let err_fd = libc::fileno(stderr);
    emit_crash_marker(out_fd, signum, name);
    if err_fd != out_fd {
        emit_crash_marker(err_fd, signum, name);
    }
    libc::signal(signum, libc::SIG_DFL);
    libc::raise(signum);
}

/// Sink for the freeze sampler: stderr (so a tap shows up live in the terminal)
/// AND an appended `freeze.log` in the game's run dir, next to lifecycle.log —
/// a terminal scrollback is not an artifact, and a wedged run is exactly when
/// the sample must survive. Both calls are async-signal-safe.
unsafe fn freeze_diag_write(buf: *const c_char, len: usize) {
    static mut log_fd: c_int = -2;
    let err_fd = libc::fileno(stderr);
    libc::write(err_fd, buf as *const c_void, len);
    libc::fsync(err_fd);
    if log_fd == -2 {
        log_fd = libc::open(
            cstr!("freeze.log"),
            libc::O_WRONLY | libc::O_CREAT | libc::O_APPEND | libc::O_CLOEXEC,
            0o644 as c_int,
        );
    }
    if log_fd >= 0 {
        libc::write(log_fd, buf as *const c_void, len);
        libc::fsync(log_fd);
    }
}

/// Repeatable, NON-fatal freeze sampler (SIGUSR1). Unlike the crash handler,
/// this returns and lets the guest keep running, so `kill -USR1 <pid>` can be
/// tapped several times against a wedged game to trace a spin loop. It reports
/// the live cs:ip, the last I/O port touched + the access counter (a spin on a
/// status port pins the port while the counter races between taps), and the
/// depth/flag state that distinguishes a spin from a blocked wait.
///
/// `retired`/`sp_visits` are the honest "is the guest running at all" signal:
/// cs:ip lives in a chunk-local `Regs` and only reaches the global cpu struct at
/// a spill (safepoint / FFI), so a frozen `ip` between taps can mean either a
/// wedged dispatcher or merely a spin whose safepoint keeps landing on the same
/// block. Retired instructions cannot lie: +0 across taps means no guest code
/// executed, which is OUR bug, not a guest spin.
unsafe extern "C" fn freeze_diag_handler(_signum: c_int) {
    static mut last_counter: u64 = 0;
    static mut last_ip_linear: u32 = 0;
    static mut last_retired: u64 = 0;
    static mut last_sp_visits: u64 = 0;
    let ip_lin = ((cs() as u32) << 4) + ip() as u32;
    let delta = io_access_counter.wrapping_sub(last_counter);
    let ip_moved = ip_lin != last_ip_linear;
    let retired_delta = jit_total_retired.wrapping_sub(last_retired);
    let sp_delta = perf_sp_visits.wrapping_sub(last_sp_visits);
    last_counter = io_access_counter;
    last_ip_linear = ip_lin;
    last_retired = jit_total_retired;
    last_sp_visits = perf_sp_visits;
    let mut buf = [0u8; 640];
    let n = libc::snprintf(
        buf.as_mut_ptr() as *mut c_char,
        buf.len(),
        cstr!("[FREEZE-DIAG] cs:ip=%04X:%04X lin=0x%05X  last_io=%s 0x%04X  io_count=%llu (+%llu since last tap)  retired=+%llu sp_visits=+%llu  ip_moved=%d  isr=%u disp=%u crit=%u lcall=%u IF=%d  ss:sp=%04X:%04X ds=%04X es=%04X ax=%04X bx=%04X\n"),
        cs() as c_uint,
        ip() as c_uint,
        ip_lin as c_uint,
        if last_io_was_read != 0 { cstr!("IN ") } else { cstr!("OUT") },
        last_io_port as c_uint,
        io_access_counter as c_ulonglong,
        delta as c_ulonglong,
        retired_delta as c_ulonglong,
        sp_delta as c_ulonglong,
        ip_moved as c_int,
        isr_depth as c_uint,
        dispatch_depth as c_uint,
        critical_depth as c_uint,
        lcall_depth as c_uint,
        IF() as c_int,
        ss() as c_uint,
        sp() as c_uint,
        ds() as c_uint,
        es() as c_uint,
        ax() as c_uint,
        bx() as c_uint,
    );
    if n > 0 {
        let len = if (n as usize) < buf.len() {
            n as usize
        } else {
            buf.len()
        };
        freeze_diag_write(buf.as_ptr() as *const c_char, len);
    }
    // Second line: the timer/clock/interrupt state that decides whether an
    // interrupt-driven wait can ever complete. A loop polling an ISR-set flag
    // hangs if virtual time is frozen (vnow +0), irq0 never latches, or the
    // vector it INT-calls points at nothing.
    static mut last_vnow: u64 = 0;
    let vnow = virtual_now_accum_ns;
    let vnow_delta = vnow.wrapping_sub(last_vnow);
    last_vnow = vnow;
    let bios_tick =
        memw_raw_read(0x40, 0x006C) as u32 | ((memw_raw_read(0x40, 0x006E) as u32) << 8);
    let v61_off = memw_raw_read(0, 0x61 * 4);
    let v61_seg = memw_raw_read(0, 0x61 * 4 + 2);
    let v08_off = memw_raw_read(0, 0x08 * 4);
    let v08_seg = memw_raw_read(0, 0x08 * 4 + 2);
    let ff1a = *seg_off(ds(), 0xFF1A);
    let ff1d = *seg_off(ds(), 0xFF1D);
    let ff1e = *seg_off(ds(), 0xFF1E);
    let mut b2 = [0u8; 768];
    let n2 = libc::snprintf(
        b2.as_mut_ptr() as *mut c_char,
        b2.len(),
        cstr!("[FREEZE-DIAG]   vclock=%d vnow=%llu (+%llu)  irq0_pending=%u last_int=0x%02X  pit0.reload=%u bios_tick=%u  INT61->%04X:%04X INT08->%04X:%04X  pic[imr=%02X isr=%02X base=%02X]  irq0: delivered=%llu blocked[shadow=%llu pic=%llu IF=%llu crit=%llu other=%llu]  RCB[FF1A]=%02X [FF1D]=%02X [FF1E]=%02X\n"),
        vclock_state as c_int,
        vnow as c_ulonglong,
        vnow_delta as c_ulonglong,
        irq0_pending as c_uint,
        last_int_no as c_uint,
        pit.reload as c_uint,
        bios_tick as c_uint,
        v61_seg as c_uint, v61_off as c_uint,
        v08_seg as c_uint, v08_off as c_uint,
        pic_imr as c_uint,
        pic_isr as c_uint,
        pic_vector_base as c_uint,
        perf_irq0_delivered as c_ulonglong,
        perf_irq0_blk_shadow as c_ulonglong,
        perf_irq0_blk_pic as c_ulonglong,
        perf_irq0_blk_if as c_ulonglong,
        perf_irq0_blk_crit as c_ulonglong,
        perf_irq0_blk_other as c_ulonglong,
        ff1a as c_uint, ff1d as c_uint, ff1e as c_uint,
    );
    if n2 > 0 {
        let len = if (n2 as usize) < b2.len() {
            n2 as usize
        } else {
            b2.len()
        };
        freeze_diag_write(b2.as_ptr() as *const c_char, len);
    }
}

/// Exposed so sdl.rs can re-install after SDL_Init overrides our handlers.
#[no_mangle]
pub unsafe extern "C" fn shim_reinstall_crash_handlers() {
    install_crash_handlers();
}

unsafe fn install_crash_handlers() {
    // 64K altstack (SIGSTKSZ<65536 ? 65536 : SIGSTKSZ); on Linux 65536 wins.
    static mut ALTSTACK: [u8; 65536] = [0; 65536];
    let mut alt: libc::stack_t = core::mem::zeroed();
    alt.ss_sp = ptr::addr_of_mut!(ALTSTACK) as *mut c_void;
    alt.ss_size = 65536;
    alt.ss_flags = 0;
    libc::sigaltstack(&alt, ptr::null_mut());

    let mut sa: libc::sigaction = core::mem::zeroed();
    sa.sa_sigaction = crash_signal_handler as *const () as usize;
    libc::sigemptyset(&mut sa.sa_mask);
    sa.sa_flags = libc::SA_RESETHAND | libc::SA_ONSTACK;
    libc::sigaction(libc::SIGSEGV, &sa, ptr::null_mut());
    libc::sigaction(libc::SIGBUS, &sa, ptr::null_mut());
    libc::sigaction(libc::SIGABRT, &sa, ptr::null_mut());
    libc::sigaction(libc::SIGFPE, &sa, ptr::null_mut());
    libc::sigaction(libc::SIGILL, &sa, ptr::null_mut());
    libc::sigaction(libc::SIGINT, &sa, ptr::null_mut());
    libc::sigaction(libc::SIGTERM, &sa, ptr::null_mut());
    libc::sigaction(libc::SIGPIPE, &sa, ptr::null_mut());

    // SIGUSR1: the repeatable, NON-fatal freeze sampler — its own handler, no
    // SA_RESETHAND (stays installed across taps), on the alt stack.
    let mut du: libc::sigaction = core::mem::zeroed();
    du.sa_sigaction = freeze_diag_handler as *const () as usize;
    libc::sigemptyset(&mut du.sa_mask);
    du.sa_flags = libc::SA_ONSTACK | libc::SA_RESTART;
    libc::sigaction(libc::SIGUSR1, &du, ptr::null_mut());
}

// ============================================================================
// Dispatch recursion guard
// ============================================================================

const DISPATCH_DEPTH_LIMIT: u16 = 2048;

unsafe fn dispatch_depth_guard(
    kind: *const c_char,
    addr: u32,
    file: *const c_char,
    func: *const c_char,
    line: c_int,
) {
    if dispatch_depth < DISPATCH_DEPTH_LIMIT {
        return;
    }
    let mut msg = [0u8; 2048];
    let n = libc::snprintf(
        msg.as_mut_ptr() as *mut c_char,
        msg.len(),
        cstr!(
            "[BUG] dispatch recursion limit hit: depth=%u (limit=%d)\n[BUG]   triggering site: %s addr=0x%05X (%s:%s:%d)\n[BUG]   cs:ip=%04X:%04X ss:sp=%04X:%04X active_binary=%s\n[BUG]   depths: lcall=%u isr=%u dispatch=%u critical=%u\n[BUG]   ++/-- per-site (inc/dec/leak):\n[BUG]     call_table_impl      inc=%llu  dec=%llu  leak=%lld\n[BUG]     dispatch_via_binary  inc=%llu  dec=%llu  leak=%lld\n[BUG]     try_dispatch_overlay inc=%llu  dec=%llu  leak=%lld\n[BUG]   diagnosis: the simulated stack has bogus return addresses (likely from a chunk-swap stack imbalance or a translator near-ret mismatch). Each dispatch pops a bad value and tail-dispatches, growing the C stack without bound. Walk back through the trace tail's `near_ret_tail`/`call_table` sequence to find where the expected_retip first diverged from the popped value.\n"
        ),
        dispatch_depth as c_uint,
        DISPATCH_DEPTH_LIMIT as c_int,
        kind,
        addr as c_uint,
        if file.is_null() { cstr!("?") } else { file },
        if func.is_null() { cstr!("?") } else { func },
        line,
        cs() as c_uint,
        ip() as c_uint,
        ss() as c_uint,
        sp() as c_uint,
        if shim_active_binary().is_null() { cstr!("<none>") } else { shim_active_binary() },
        lcall_depth as c_uint,
        isr_depth as c_uint,
        dispatch_depth as c_uint,
        critical_depth as c_uint,
        dd_inc_call_table as c_ulonglong,
        dd_dec_call_table as c_ulonglong,
        (dd_inc_call_table.wrapping_sub(dd_dec_call_table)) as c_longlong,
        dd_inc_via_binary as c_ulonglong,
        dd_dec_via_binary as c_ulonglong,
        (dd_inc_via_binary.wrapping_sub(dd_dec_via_binary)) as c_longlong,
        dd_inc_overlay_first as c_ulonglong,
        dd_dec_overlay_first as c_ulonglong,
        (dd_inc_overlay_first.wrapping_sub(dd_dec_overlay_first)) as c_longlong,
    );
    if n > 0 {
        shim_log_crash(cstr!("%s"), msg.as_ptr() as *const c_char);
        save_bug_bundle(
            cstr!("dispatch_recursion"),
            addr,
            msg.as_ptr() as *const c_char,
        );
    }
    shim_flush_all_streams();
    libc::abort();
}

// ============================================================================
// Stack-drift detector
// ============================================================================

#[no_mangle]
pub unsafe extern "C" fn shim_check_stack_drift(
    site: *const c_char,
    expected_sp: u16,
    actual_sp: u16,
    file: *const c_char,
    func: *const c_char,
    line: c_int,
) {
    let delta: i16 = actual_sp.wrapping_sub(expected_sp) as i16;
    let mut msg = [0u8; 1024];
    let n = libc::snprintf(
        msg.as_mut_ptr() as *mut c_char,
        msg.len(),
        cstr!(
            "[WARN] stack drift at %s boundary (non-fatal; continuing)\n  expected_sp=%04X  actual_sp=%04X  delta=%+d bytes\n  cs:ip=%04X:%04X  ss=%04X  source=%s:%s:%d\n  active_binary=%s  depths: lcall=%u isr=%u dispatch=%u\n  registers: ax=%04X bx=%04X cx=%04X dx=%04X si=%04X di=%04X bp=%04X\n  segments:  ds=%04X es=%04X\n  diagnosis: the body dispatched by this %s site had a net non-zero\n  stack effect across its setjmp/longjmp boundary. After matching\n  push/pop pairs the simulated 8086 stack pointer drifted by %+d.\n  Causes include: an unsupported instruction whose translator stub\n  skipped its push or pop; a translator emit that lost a push when\n  the matching pop ran (or vice versa); a shim with an unbalanced\n  manual sp adjustment. Inspect stack_writes.log in the bundle to\n  find the recent push/pop history around this sp range.\n"
        ),
        site,
        expected_sp as c_uint,
        actual_sp as c_uint,
        delta as c_int,
        cs() as c_uint,
        ip() as c_uint,
        ss() as c_uint,
        if file.is_null() { cstr!("?") } else { file },
        if func.is_null() { cstr!("?") } else { func },
        line,
        if shim_active_binary().is_null() { cstr!("<none>") } else { shim_active_binary() },
        lcall_depth as c_uint,
        isr_depth as c_uint,
        dispatch_depth as c_uint,
        ax() as c_uint,
        bx() as c_uint,
        cx() as c_uint,
        dx() as c_uint,
        si() as c_uint,
        di() as c_uint,
        bp() as c_uint,
        ds() as c_uint,
        es() as c_uint,
        site,
        delta as c_int,
    );
    static mut drift_reports: c_int = 0;
    if drift_reports < 3 {
        shim_log_crash(cstr!("%s"), msg.as_ptr() as *const c_char);
        if drift_reports == 0 && n > 0 {
            save_bug_bundle(
                cstr!("stack_drift"),
                actual_sp as u32,
                msg.as_ptr() as *const c_char,
            );
        }
        drift_reports += 1;
        if drift_reports == 3 {
            shim_log_stdout(cstr!("[WARN] further stack-drift reports suppressed\n"));
        }
    }
}

// ============================================================================
// Constructor: init_shim_logs
// ============================================================================

unsafe extern "C" fn init_shim_logs() {
    let verbose = libc::getenv(cstr!("SAISEI_VERBOSE"));
    if !verbose.is_null() && *verbose != 0 {
        shim_stdout_enabled = 1;
    }
    libc::setvbuf(stdout, ptr::null_mut(), libc::_IONBF, 0);
    libc::setvbuf(stderr, ptr::null_mut(), libc::_IONBF, 0);
    libc::fflush(stdout);
    libc::fflush(stderr);
    install_crash_handlers();
    libc::atexit(shim_flush_all_streams_atexit);
}

#[used]
#[link_section = ".init_array"]
static INIT_SHIM_LOGS_CTOR: unsafe extern "C" fn() = init_shim_logs;

// ============================================================================
// Trace ring buffer
// ============================================================================

const TRACE_RING_LINES: usize = 50000;
const TRACE_RING_LINE_MAX: usize = 384;
static mut trace_ring: [[u8; TRACE_RING_LINE_MAX]; TRACE_RING_LINES] =
    [[0; TRACE_RING_LINE_MAX]; TRACE_RING_LINES];
static mut trace_ring_len: [u16; TRACE_RING_LINES] = [0; TRACE_RING_LINES];
static mut trace_ring_pos: c_int = 0;
static mut trace_ring_filled: c_int = 0;

unsafe fn trace_ring_save(line: *const c_char, mut len: usize) {
    if len == 0 {
        return;
    }
    if len > TRACE_RING_LINE_MAX - 1 {
        len = TRACE_RING_LINE_MAX - 1;
    }
    let dst = trace_ring[trace_ring_pos as usize].as_mut_ptr();
    libc::memcpy(dst as *mut c_void, line as *const c_void, len);
    *dst.add(len) = 0;
    trace_ring_len[trace_ring_pos as usize] = len as u16;
    trace_ring_pos = (trace_ring_pos + 1) % TRACE_RING_LINES as c_int;
    if trace_ring_filled < TRACE_RING_LINES as c_int {
        trace_ring_filled += 1;
    }
}

unsafe fn trace_ring_dump(fd: c_int) {
    let n = trace_ring_filled;
    let start = (trace_ring_pos - n + TRACE_RING_LINES as c_int) % TRACE_RING_LINES as c_int;
    for i in 0..n {
        let idx = ((start + i) % TRACE_RING_LINES as c_int) as usize;
        let buf = trace_ring[idx].as_ptr();
        let len = trace_ring_len[idx] as usize;
        let mut off: usize = 0;
        while off < len {
            let w = libc::write(fd, buf.add(off) as *const c_void, len - off);
            if w < 0 {
                if *libc::__errno_location() == libc::EINTR {
                    continue;
                }
                break;
            }
            off += w as usize;
        }
    }
}

// ============================================================================
// Lifecycle log ring
// ============================================================================

const LIFECYCLE_RING_LINES: usize = 65536;
const LIFECYCLE_LINE_MAX: usize = 200;
static mut lifecycle_ring: [[u8; LIFECYCLE_LINE_MAX]; LIFECYCLE_RING_LINES] =
    [[0; LIFECYCLE_LINE_MAX]; LIFECYCLE_RING_LINES];
static mut lifecycle_ring_len: [u16; LIFECYCLE_RING_LINES] = [0; LIFECYCLE_RING_LINES];
static mut lifecycle_ring_pos: c_int = 0;
static mut lifecycle_ring_filled: c_int = 0;
// Per-entry payload tag: LC_TEXT = preformatted line (the general
// lifecycle_log path), LC_DISPATCH / LC_NRET = a binary LifecycleDispatchRec
// stored in the line buffer, formatted only when the ring is dumped. The
// dispatch-trace entries (CALL/LCALL/JMP/LJMP/NRET, several 100k/s on
// driver-call-heavy games) were the dominant per-transfer cost as text — two
// vsnprintfs and an alias lookup per far transfer. Dump output is identical.
const LC_TEXT: u8 = 0;
const LC_DISPATCH: u8 = 1;
const LC_NRET: u8 = 2;
static mut lifecycle_ring_kind: [u8; LIFECYCLE_RING_LINES] = [0; LIFECYCLE_RING_LINES];

/// A deferred dispatch-trace event. Everything dump-time formatting needs is
/// captured at record time: the mapping resolution (mappings change as
/// overlays load — resolving later would lie) and the registers (the alias
/// arg-spec renderer reads them). Alias names resolve at dump time from the
/// same registry the eager path consults.
#[repr(C)]
#[derive(Clone, Copy)]
struct LifecycleDispatchRec {
    t_us: u64,
    /// Static cstr ("CALL"/"LCALL"/"JMP"/"LJMP"); unused for LC_NRET.
    kind: *const c_char,
    addr: u32,
    popped: u16, // LC_NRET only
    has_path: u8,
    _pad: u8,
    off_in: u64,
    bn: [u8; 20],
    regs: RegSnap,
}

unsafe fn lifecycle_ring_save_rec(rec: &LifecycleDispatchRec, kind: u8) {
    let dst = lifecycle_ring[lifecycle_ring_pos as usize].as_mut_ptr() as *mut LifecycleDispatchRec;
    core::ptr::write_unaligned(dst, *rec);
    lifecycle_ring_len[lifecycle_ring_pos as usize] =
        core::mem::size_of::<LifecycleDispatchRec>() as u16;
    lifecycle_ring_kind[lifecycle_ring_pos as usize] = kind;
    lifecycle_ring_pos = (lifecycle_ring_pos + 1) % LIFECYCLE_RING_LINES as c_int;
    if lifecycle_ring_filled < LIFECYCLE_RING_LINES as c_int {
        lifecycle_ring_filled += 1;
    }
}

/// True when dispatch-trace events must be formatted at record time: the shim
/// trace is live on stdout (--verbose / trace file) or lifecycle events are
/// streaming to a file (--lifecycle-file). Otherwise they are recorded as
/// binary ring entries and formatted only if a dump ever happens.
unsafe fn lifecycle_eager() -> bool {
    lifecycle_fp_open_if_requested();
    shim_stdout_enabled != 0 || !lifecycle_fp.is_null()
}
static mut lifecycle_fp_buf: [u8; 1 << 15] = [0; 1 << 15];
unsafe fn lifecycle_elapsed_us() -> u64 {
    // VIRTUAL (instruction-driven) elapsed µs since machine init. Forensic
    // stamps in lifecycle.log / stack_writes.log follow game time: they are
    // deterministic across replays and cost a few loads instead of the
    // per-event host clock_gettime the old host-clock formula paid (which was
    // the single hottest line on the stack-op path — every push/pop stamped).
    shim_virtual_now_ns().saturating_sub(host_time_origin_ns) / 1000
}

unsafe fn lifecycle_fp_open_if_requested() {
    // Idempotent, and must NOT latch while the path is still unset. The path
    // comes from --lifecycle-file (parsed in saisei_main), but the very first
    // lifecycle event (the program LOAD) fires from the init_memory constructor,
    // which runs *before* main — env vars were readable there, argv is not.
    // Latching on that null-path call would permanently disable streaming, so we
    // only stop retrying once a real open attempt (non-null path) has failed.
    if !lifecycle_fp.is_null() {
        return;
    }
    let p = lifecycle_file_path_arg;
    if p.is_null() || *p == 0 {
        return;
    }
    static mut open_failed: c_int = 0;
    if open_failed != 0 {
        return;
    }
    lifecycle_fp = libc::fopen(p, cstr!("w"));
    if lifecycle_fp.is_null() {
        open_failed = 1;
        libc::fprintf(
            stderr,
            cstr!("--lifecycle-file: cannot open %s: %s\n"),
            p,
            libc::strerror(*libc::__errno_location()),
        );
        return;
    }
    libc::setvbuf(
        lifecycle_fp,
        ptr::addr_of_mut!(lifecycle_fp_buf) as *mut c_char,
        libc::_IOFBF,
        core::mem::size_of_val(&*ptr::addr_of!(lifecycle_fp_buf)),
    );
    libc::fprintf(
        lifecycle_fp,
        cstr!("# Focused lifecycle log. Columns: t=<elapsed_us> <kind> <details>\n# kinds: LOAD (file mapping registered), CALL/JMP/LJMP/LCALL/NRET\n"),
    );
}

unsafe fn lifecycle_ring_save(buf: *const c_char, mut len: usize) {
    if len > LIFECYCLE_LINE_MAX - 1 {
        len = LIFECYCLE_LINE_MAX - 1;
    }
    let dst = lifecycle_ring[lifecycle_ring_pos as usize].as_mut_ptr();
    libc::memcpy(dst as *mut c_void, buf as *const c_void, len);
    *dst.add(len) = 0;
    lifecycle_ring_len[lifecycle_ring_pos as usize] = len as u16;
    lifecycle_ring_kind[lifecycle_ring_pos as usize] = LC_TEXT;
    lifecycle_ring_pos = (lifecycle_ring_pos + 1) % LIFECYCLE_RING_LINES as c_int;
    if lifecycle_ring_filled < LIFECYCLE_RING_LINES as c_int {
        lifecycle_ring_filled += 1;
    }
}

unsafe extern "C" fn lifecycle_log(fmt: *const c_char, args: ...) {
    lifecycle_fp_open_if_requested();
    let mut buf = [0u8; LIFECYCLE_LINE_MAX];
    let t = lifecycle_elapsed_us();
    let mut prefix = libc::snprintf(
        buf.as_mut_ptr() as *mut c_char,
        buf.len(),
        cstr!("t=%llu "),
        t as c_ulonglong,
    );
    if prefix < 0 {
        prefix = 0;
    }
    if prefix > (buf.len() as c_int - 1) {
        prefix = buf.len() as c_int - 1;
    }
    let mut rest = vsnprintf(
        buf.as_mut_ptr().add(prefix as usize) as *mut c_char,
        buf.len() - prefix as usize,
        fmt,
        args,
    );
    if rest < 0 {
        rest = 0;
    }
    let mut total = prefix as usize + rest as usize;
    if total >= buf.len() {
        total = buf.len() - 1;
    }
    lifecycle_ring_save(buf.as_ptr() as *const c_char, total);
    if !lifecycle_fp.is_null() {
        libc::fwrite(buf.as_ptr() as *const c_void, 1, total, lifecycle_fp);
    }
}

unsafe fn lifecycle_dump_to_dir(dir: *const c_char) {
    let mut path = [0u8; 320];
    libc::snprintf(
        path.as_mut_ptr() as *mut c_char,
        path.len(),
        cstr!("%s/lifecycle.log"),
        dir,
    );
    let fd = libc::open(
        path.as_ptr() as *const c_char,
        libc::O_WRONLY | libc::O_CREAT | libc::O_TRUNC | libc::O_CLOEXEC,
        0o644,
    );
    if fd < 0 {
        return;
    }
    let header: &[u8] = b"# Focused lifecycle log (in-memory ring tail).\n# Columns: t=<elapsed_us> <kind> <details>\n# kinds: LOAD CALL JMP LJMP LCALL NRET\n";
    let _hw = libc::write(fd, header.as_ptr() as *const c_void, header.len());
    let n = lifecycle_ring_filled;
    let start =
        (lifecycle_ring_pos - n + LIFECYCLE_RING_LINES as c_int) % LIFECYCLE_RING_LINES as c_int;
    let mut fmt = [0u8; LIFECYCLE_LINE_MAX];
    for i in 0..n {
        let idx = ((start + i) % LIFECYCLE_RING_LINES as c_int) as usize;
        let kind = lifecycle_ring_kind[idx];
        let (mut line, mut len): (*const u8, usize) = (
            lifecycle_ring[idx].as_ptr(),
            lifecycle_ring_len[idx] as usize,
        );
        if kind != LC_TEXT {
            // Deferred dispatch-trace record: format now, exactly as the
            // eager path would have at record time.
            let rec = core::ptr::read_unaligned(
                lifecycle_ring[idx].as_ptr() as *const LifecycleDispatchRec
            );
            let w = lifecycle_format_rec(&rec, kind, fmt.as_mut_ptr() as *mut c_char, fmt.len());
            line = fmt.as_ptr();
            len = w;
        }
        let mut off: usize = 0;
        while off < len {
            let w = libc::write(fd, line.add(off) as *const c_void, len - off);
            if w < 0 {
                if *libc::__errno_location() == libc::EINTR {
                    continue;
                }
                break;
            }
            off += w as usize;
        }
    }
    libc::close(fd);
}

/// Render a deferred LC_DISPATCH / LC_NRET record to the exact text the eager
/// lifecycle_log path would have produced at record time. Returns the byte
/// length (truncated to cap-1, like lifecycle_log's vsnprintf).
unsafe fn lifecycle_format_rec(
    rec: &LifecycleDispatchRec,
    kind: u8,
    out: *mut c_char,
    cap: usize,
) -> usize {
    let bn = rec.bn.as_ptr() as *const c_char;
    let w: c_int;
    if kind == LC_NRET {
        w = libc::snprintf(
            out,
            cap,
            cstr!("t=%llu NRET 0x%05X popped=%04X -> %s+0x%zX\n"),
            rec.t_us as c_ulonglong,
            rec.addr as c_uint,
            rec.popped as c_uint,
            bn,
            rec.off_in as usize,
        );
    } else {
        let mut alias: *const c_char = ptr::null();
        let mut disp = [0u8; 256];
        if rec.has_path != 0 {
            let mut idbuf = [0u8; 160];
            libc::snprintf(
                idbuf.as_mut_ptr() as *mut c_char,
                idbuf.len(),
                cstr!("%s+0x%zX"),
                bn,
                rec.off_in as usize,
            );
            alias = aliasreg_alias(idbuf.as_ptr() as *const c_char, 0);
        }
        if !alias.is_null() {
            render_alias_with_args(
                alias,
                disp.as_mut_ptr() as *mut c_char,
                disp.len(),
                &rec.regs,
            );
            w = libc::snprintf(
                out,
                cap,
                cstr!("t=%llu %s 0x%05X -> %s (%s+0x%zX)  bx=%04X si=%04X ax=%04X ds=%04X cs=%04X ip=%04X\n"),
                rec.t_us as c_ulonglong,
                rec.kind,
                rec.addr as c_uint,
                disp.as_ptr() as *const c_char,
                bn,
                rec.off_in as usize,
                rec.regs.bx as c_uint,
                rec.regs.si as c_uint,
                rec.regs.ax as c_uint,
                rec.regs.ds as c_uint,
                rec.regs.cs as c_uint,
                rec.regs.ip as c_uint,
            );
        } else {
            w = libc::snprintf(
                out,
                cap,
                cstr!("t=%llu %s 0x%05X -> %s+0x%zX  bx=%04X si=%04X ax=%04X ds=%04X cs=%04X ip=%04X\n"),
                rec.t_us as c_ulonglong,
                rec.kind,
                rec.addr as c_uint,
                bn,
                rec.off_in as usize,
                rec.regs.bx as c_uint,
                rec.regs.si as c_uint,
                rec.regs.ax as c_uint,
                rec.regs.ds as c_uint,
                rec.regs.cs as c_uint,
                rec.regs.ip as c_uint,
            );
        }
    }
    if w < 0 {
        return 0;
    }
    if w as usize >= cap {
        return cap - 1;
    }
    w as usize
}

// ============================================================================
// Per-binary case-key sets (save_manager IP validation)  [C lines 552-615]
// ============================================================================

const CK_MAX_BINARIES: usize = 16;

#[repr(C)]
struct ShimCaseKeys {
    name: [u8; 16],
    keys: *mut u32,
    count: u32,
    loaded: c_int,
}
impl ShimCaseKeys {
    const ZERO: ShimCaseKeys = ShimCaseKeys {
        name: [0; 16],
        keys: ptr::null_mut(),
        count: 0,
        loaded: 0,
    };
}

static mut shim_ck_sets: [ShimCaseKeys; CK_MAX_BINARIES] = [ShimCaseKeys::ZERO; CK_MAX_BINARIES];
static mut shim_ck_count: c_int = 0;

unsafe fn shim_ck_find_or_load(module: *const c_char) -> *mut ShimCaseKeys {
    if module.is_null() {
        return ptr::null_mut();
    }
    for i in 0..shim_ck_count {
        if libc::strcmp(
            shim_ck_sets[i as usize].name.as_ptr() as *const c_char,
            module,
        ) == 0
        {
            return &mut shim_ck_sets[i as usize];
        }
    }
    if shim_ck_count >= CK_MAX_BINARIES as c_int {
        return ptr::null_mut();
    }
    let s = &mut shim_ck_sets[shim_ck_count as usize];
    shim_ck_count += 1;
    libc::strncpy(s.name.as_mut_ptr() as *mut c_char, module, s.name.len() - 1);
    s.name[s.name.len() - 1] = 0;
    s.loaded = 1;
    let mut path = [0u8; 256];
    libc::snprintf(
        path.as_mut_ptr() as *mut c_char,
        path.len(),
        cstr!("case_keys/%s.bin"),
        module,
    );
    let fp = libc::fopen(path.as_ptr() as *const c_char, cstr!("rb"));
    if fp.is_null() {
        return s;
    }
    let mut cnt: u32 = 0;
    if libc::fread(
        &mut cnt as *mut u32 as *mut c_void,
        core::mem::size_of::<u32>(),
        1,
        fp,
    ) != 1
        || cnt == 0
        || cnt > 0x100000
    {
        libc::fclose(fp);
        return s;
    }
    let keys = libc::malloc(cnt as usize * core::mem::size_of::<u32>()) as *mut u32;
    if keys.is_null() {
        libc::fclose(fp);
        return s;
    }
    if libc::fread(
        keys as *mut c_void,
        core::mem::size_of::<u32>(),
        cnt as usize,
        fp,
    ) != cnt as usize
    {
        libc::free(keys as *mut c_void);
        libc::fclose(fp);
        return s;
    }
    libc::fclose(fp);
    s.keys = keys;
    s.count = cnt;
    s
}

#[no_mangle]
pub unsafe extern "C" fn shim_pc_is_case_key(module: *const c_char, file_off: u32) -> c_int {
    let s = shim_ck_find_or_load(module);
    if s.is_null() || (*s).keys.is_null() || (*s).count == 0 {
        return 0;
    }
    let s = &*s;
    let mut lo: u32 = 0;
    let mut hi: u32 = s.count;
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        if *s.keys.add(mid as usize) < file_off {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    (lo < s.count && *s.keys.add(lo as usize) == file_off) as c_int
}

// ============================================================================
// Optional persistent trace file + logging entry points  [C lines 617-726]
// ============================================================================

static mut trace_file_buf: [u8; 1 << 18] = [0; 1 << 18];

// Paths from --trace-file / --lifecycle-file (parsed in saisei_main). They point
// into argv, which lives for the whole process, so we keep the pointer directly.
static mut trace_file_path_arg: *const c_char = ptr::null();
static mut lifecycle_file_path_arg: *const c_char = ptr::null();

unsafe fn trace_file_open_if_requested() {
    // Same rule as lifecycle: idempotent, and don't latch while the path is
    // unset (see lifecycle_fp_open_if_requested) — the path arrives from argv in
    // saisei_main, later than any constructor-time logging attempt.
    if !trace_file_fp.is_null() {
        return;
    }
    let p = trace_file_path_arg;
    if p.is_null() || *p == 0 {
        return;
    }
    static mut open_failed: c_int = 0;
    if open_failed != 0 {
        return;
    }
    trace_file_fp = libc::fopen(p, cstr!("w"));
    if trace_file_fp.is_null() {
        open_failed = 1;
        libc::fprintf(
            stderr,
            cstr!("--trace-file: cannot open %s: %s\n"),
            p,
            libc::strerror(*libc::__errno_location()),
        );
        return;
    }
    libc::setvbuf(
        trace_file_fp,
        ptr::addr_of_mut!(trace_file_buf) as *mut c_char,
        libc::_IOFBF,
        core::mem::size_of_val(&*ptr::addr_of!(trace_file_buf)),
    );
}

unsafe fn shim_log_stdout_impl(stream: *mut FILE, fmt: *const c_char, args: VaList) {
    let mut buf = [0u8; TRACE_RING_LINE_MAX];
    let n = vsnprintf(
        buf.as_mut_ptr() as *mut c_char,
        buf.len(),
        fmt,
        args.clone(),
    );
    if n < 0 {
        vfprintf(stream, fmt, args);
        return;
    }
    let emit: usize = if (n as usize) < buf.len() {
        n as usize
    } else {
        buf.len() - 1
    };
    trace_ring_save(buf.as_ptr() as *const c_char, emit);
    trace_file_open_if_requested();
    if !trace_file_fp.is_null() {
        libc::fwrite(buf.as_ptr() as *const c_void, 1, emit, trace_file_fp);
        return;
    }
    libc::fwrite(buf.as_ptr() as *const c_void, 1, emit, stream);
}

#[no_mangle]
pub unsafe extern "C" fn shim_set_stdout_logging_enabled(enabled: c_int) {
    shim_stdout_enabled = if enabled != 0 { 1 } else { 0 };
}

/// The `--verbose` gate, for diagnostics that write straight to stderr instead
/// of going through `shim_log_stdout`.
#[no_mangle]
pub unsafe extern "C" fn shim_stdout_logging_enabled() -> c_int {
    shim_stdout_enabled
}

#[no_mangle]
pub unsafe extern "C" fn shim_enable_stdout_logging() {
    shim_set_stdout_logging_enabled(1);
}

#[no_mangle]
pub unsafe extern "C" fn shim_disable_stdout_logging() {
    shim_set_stdout_logging_enabled(0);
}

#[no_mangle]
pub unsafe extern "C" fn shim_log_stdout(fmt: *const c_char, args: ...) {
    if shim_stdout_enabled == 0 {
        return;
    }
    shim_log_stdout_impl(stdout, fmt, args.clone());
    if libc::ferror(stdout) != 0 {
        libc::clearerr(stdout);
        shim_log_stdout_impl(stderr, fmt, args.clone());
    }
}

#[no_mangle]
pub unsafe extern "C" fn shim_log_crash(fmt: *const c_char, args: ...) {
    shim_log_stdout_impl(stdout, fmt, args.clone());
    if libc::ferror(stdout) != 0 {
        libc::clearerr(stdout);
        shim_log_stdout_impl(stderr, fmt, args.clone());
    }
}

#[no_mangle]
pub unsafe extern "C" fn shim_log_stderr(fmt: *const c_char, args: ...) {
    vfprintf(stderr, fmt, args);
    libc::fflush(stderr);
}

#[no_mangle]
pub unsafe extern "C" fn shim_exit_with_message(fmt: *const c_char, args: ...) {
    vfprintf(stderr, fmt, args);
    shim_flush_all_streams();
    libc::exit(1);
}

unsafe fn to_bcd(value: u8) -> u8 {
    ((value / 10) << 4) | (value % 10)
}

unsafe fn set_iret_carry(set: c_int) {
    let flags_off: u16 = ((sp() as u32).wrapping_add(4) & 0xFFFF) as u16;
    let mut ret_flags = memw_read_impl(ss(), flags_off, SHIMS_FILE, cstr!("set_iret_carry"), 734);
    if set != 0 {
        ret_flags |= 1u16;
    } else {
        ret_flags &= !1u16;
    }
    memw_write_impl(
        ss(),
        flags_off,
        ret_flags,
        SHIMS_FILE,
        cstr!("set_iret_carry"),
        740,
    );
    set_CF((set != 0) as u8);
}

unsafe fn io_port_error(func: *const c_char, port: u16) {
    shim_log_stderr(
        cstr!("Error: %s called with unsupported port 0x%04X\n"),
        func,
        port as c_uint,
    );
    shim_flush_all_streams();
    libc::exit(1);
}

/// Exit-time execution stats: retired guest instructions vs host wall time.
/// Under `--speedup N` (pacing mostly idle) host-MIPS approximates raw
/// execution capability; at speedup 1 it just re-states real-time pacing.
/// Developer diagnostics, so it is silent unless `--verbose` is on — a player
/// quitting a game should see nothing. (The FIFO 0x1E report is an explicit
/// console request and still prints either way.)
extern "C" fn report_retired_at_exit() {
    unsafe {
        if shim_stdout_enabled == 0 {
            return;
        }
        shim_perf_report(cstr!("exit"))
    }
}

#[cfg(feature = "force_exit_after_10s")]
unsafe extern "C" fn force_exit_handler(_signum: c_int) {
    shim_log_stderr(cstr!(
        "Error: Execution exceeded 10 seconds. Forcing exit.\n"
    ));
    shim_flush_all_streams();
    libc::exit(1);
}

#[cfg(feature = "force_exit_after_10s")]
unsafe fn setup_force_exit() {
    libc::signal(libc::SIGALRM, force_exit_handler as usize);
    libc::alarm(10);
}

// ============================================================================
// CPU state + flat memory model globals  [C lines 778-852]
// ============================================================================

#[repr(C)]
pub struct ExecParamBlock {
    pub saved_sp: u16,
    pub saved_ss: u16,
}

#[repr(C)]
pub struct PSP {
    pub raw: [u8; 256],
}

#[no_mangle]
pub static mut cpu: CpuState = CpuState {
    r_ax: 0,
    r_bx: 0,
    r_cx: 0,
    r_dx: 0,
    si: 0,
    di: 0,
    bp: 0,
    sp: 0,
    r_ip: 0,
    r_cs: 0,
    r_ds: 0,
    r_es: 0,
    r_ss: 0,
    flags: CpuFlags {
        CF: 0,
        PF: 0,
        ZF: 0,
        SF: 0,
        OF: 0,
        IF: 0,
        DF: 0,
    },
};

#[no_mangle]
pub static mut exec_params: ExecParamBlock = ExecParamBlock {
    saved_sp: 0,
    saved_ss: 0,
};

#[no_mangle]
pub static mut psp_seg: u16 = DEFAULT_PSP_SEG;

#[no_mangle]
pub static mut virtual_memory: *mut u8 = ptr::null_mut();
#[no_mangle]
pub static SHIM_MEMORY_SIZE: usize = MEMORY_SIZE;
#[no_mangle]
pub static mut a20_enabled: bool = false;
static mut psp: *mut PSP = ptr::null_mut();
static mut image_base: *mut u8 = ptr::null_mut();
static mut env_block: *mut u8 = ptr::null_mut();
#[no_mangle]
pub static mut dta_ptr: *mut c_void = ptr::null_mut();
#[no_mangle]
pub static mut next_free_seg: u16 = 0;
#[no_mangle]
pub static mut program_min_block_paras: u16 = 0;
#[no_mangle]
pub static mut null_guard_initial: [u8; 16] = [0; 16];
static mut screenshot_counter: c_int = 1;
static mut SCREENSHOT_INTERVAL_SECS: c_int = 0;
static mut last_present_time_ns: u64 = 0;
static mut last_screenshot_time_ns: u64 = 0;
#[no_mangle]
pub static mut headless_mode: c_int = 0;
#[no_mangle]
pub static mut emulation_speedup: f64 = 1.0;
#[no_mangle]
pub static mut host_time_origin_ns: u64 = 0;
#[no_mangle]
pub static mut virtual_display_buffer: c_int = 0;
#[no_mangle]
pub static mut current_display_width: c_int = 320;
#[no_mangle]
pub static mut current_display_height: c_int = 200;

static mut orig_termios: libc::termios = unsafe { core::mem::MaybeUninit::zeroed().assume_init() };
static mut keyboard_fd: c_int = -1;
static mut keyboard_initialized: c_int = 0;
static mut keyboard_input_enabled: c_int = 0;
static mut keyboard_blocking_enabled: c_int = 0;

const STD_HANDLE_COUNT: usize = 5;
#[no_mangle]
pub static mut handles: [*mut FILE; MAX_DOS_HANDLES] = [ptr::null_mut(); MAX_DOS_HANDLES];
#[no_mangle]
pub static mut handle_paths: [*mut c_char; MAX_DOS_HANDLES] = [ptr::null_mut(); MAX_DOS_HANDLES];
#[no_mangle]
pub static mut handle_paths_owned: [bool; MAX_DOS_HANDLES] = [false; MAX_DOS_HANDLES];

static mut std_handle_names: [*const c_char; STD_HANDLE_COUNT] = [
    cstr!("<stdin>"),
    cstr!("<stdout>"),
    cstr!("<stderr>"),
    cstr!("<stdprn>"),
    cstr!("<stdaux>"),
];

#[no_mangle]
pub unsafe extern "C" fn is_standard_handle(handle: u16) -> c_int {
    (handle < STD_HANDLE_COUNT as u16) as c_int
}

unsafe fn init_standard_handles() {
    handles[0] = stdin;
    handles[1] = stdout;
    handles[2] = stderr;
    handles[3] = stdout;
    handles[4] = stdout;
    for i in 0..STD_HANDLE_COUNT {
        handle_paths[i] = std_handle_names[i] as *mut c_char;
        handle_paths_owned[i] = false;
    }
}

// ============================================================================
// FileMapping table + interrupt-vector stubs  [C lines 854-1018]
// ============================================================================

#[repr(C)]
#[derive(Clone, Copy)]
struct FileMapping {
    path: *mut c_char,
    base: u32,
    len: usize,
    file_offset: usize,
    data: *mut u8,
    canonical_cs: u16,
    loader_cs: u16,
    loader_ip: u16,
    loader_ss: u16,
    loader_sp: u16,
    loader_stack: [u16; 8],
}
impl FileMapping {
    const ZERO: FileMapping = FileMapping {
        path: ptr::null_mut(),
        base: 0,
        len: 0,
        file_offset: 0,
        data: ptr::null_mut(),
        canonical_cs: 0,
        loader_cs: 0,
        loader_ip: 0,
        loader_ss: 0,
        loader_sp: 0,
        loader_stack: [0; 8],
    };
}

const MAX_FILE_MAPPINGS: usize = 1024;
static mut file_mappings: [FileMapping; MAX_FILE_MAPPINGS] = [FileMapping::ZERO; MAX_FILE_MAPPINGS];
static mut file_mapping_count: usize = 0;
#[no_mangle]
pub static mut last_int_no: u8 = 0;

const DEFAULT_ISR_LINEAR: u32 = 0x000F0000;
const DEFAULT_ISR_SEG: u16 = (DEFAULT_ISR_LINEAR >> 4) as u16;
const DEFAULT_ISR_OFF: u16 = (DEFAULT_ISR_LINEAR & 0xF) as u16;
const BIOS_VIDEO_ISR_LINEAR: u32 = 0x000F0100;
const BIOS_VIDEO_ISR_SEG: u16 = (BIOS_VIDEO_ISR_LINEAR >> 4) as u16;
const BIOS_VIDEO_ISR_OFF: u16 = (BIOS_VIDEO_ISR_LINEAR & 0xF) as u16;

static bios_video_parameter_table_mode6: [u8; 6] = [0x00, 0x00, 0x02, 0x00, 0x00, 0x00];

const BIOS_KBD_ISR_LINEAR: u32 = 0x000F0200;
const BIOS_KBD_ISR_SEG: u16 = (BIOS_KBD_ISR_LINEAR >> 4) as u16;
const BIOS_KBD_ISR_OFF: u16 = (BIOS_KBD_ISR_LINEAR & 0xF) as u16;
const DOS_TERM_ISR_LINEAR: u32 = 0x000F0300;
const DOS_TERM_ISR_SEG: u16 = (DOS_TERM_ISR_LINEAR >> 4) as u16;
const DOS_TERM_ISR_OFF: u16 = (DOS_TERM_ISR_LINEAR & 0xF) as u16;
const DOS_API_ISR_LINEAR: u32 = 0x000F0400;
const DOS_API_ISR_SEG: u16 = (DOS_API_ISR_LINEAR >> 4) as u16;
const DOS_API_ISR_OFF: u16 = (DOS_API_ISR_LINEAR & 0xF) as u16;
const BIOS_TIMER_ISR_LINEAR: u32 = 0x000F0500;
const BIOS_TIMER_ISR_SEG: u16 = (BIOS_TIMER_ISR_LINEAR >> 4) as u16;
const BIOS_TIMER_ISR_OFF: u16 = (BIOS_TIMER_ISR_LINEAR & 0xF) as u16;
const BIOS_IRQ0_ISR_LINEAR: u32 = 0x000F0600;
const BIOS_IRQ0_ISR_SEG: u16 = (BIOS_IRQ0_ISR_LINEAR >> 4) as u16;
const BIOS_IRQ0_ISR_OFF: u16 = (BIOS_IRQ0_ISR_LINEAR & 0xF) as u16;
const BIOS_IRQ1_ISR_LINEAR: u32 = 0x000F0900;
const BIOS_IRQ1_ISR_SEG: u16 = (BIOS_IRQ1_ISR_LINEAR >> 4) as u16;
const BIOS_IRQ1_ISR_OFF: u16 = (BIOS_IRQ1_ISR_LINEAR & 0xF) as u16;
const BIOS_EQUIPMENT_ISR_LINEAR: u32 = 0x000F0700;
const BIOS_EQUIPMENT_ISR_SEG: u16 = (BIOS_EQUIPMENT_ISR_LINEAR >> 4) as u16;
const BIOS_EQUIPMENT_ISR_OFF: u16 = (BIOS_EQUIPMENT_ISR_LINEAR & 0xF) as u16;
const BIOS_TIMER_TICK_ISR_LINEAR: u32 = 0x000F0800;
const BIOS_TIMER_TICK_ISR_SEG: u16 = (BIOS_TIMER_TICK_ISR_LINEAR >> 4) as u16;
const BIOS_TIMER_TICK_ISR_OFF: u16 = (BIOS_TIMER_TICK_ISR_LINEAR & 0xF) as u16;
const MOUSE_ISR_LINEAR: u32 = 0x000F0A00;
const MOUSE_ISR_SEG: u16 = (MOUSE_ISR_LINEAR >> 4) as u16;
const MOUSE_ISR_OFF: u16 = (MOUSE_ISR_LINEAR & 0xF) as u16;
const BIOS_EQUIPMENT_WORD: u16 = 0x0063;

unsafe extern "C" fn default_isr_impl(
    _expected_retip: u16,
    file: *const c_char,
    func: *const c_char,
    line: c_int,
) {
    let mut msg = [0u8; 256];
    libc::snprintf(
        msg.as_mut_ptr() as *mut c_char,
        msg.len(),
        cstr!("unhandled interrupt 0x%02X (%s:%s:%d)"),
        last_int_no as c_uint,
        file,
        func,
        line,
    );
    shim_log_crash(cstr!("%s\n"), msg.as_ptr() as *const c_char);
    save_bug_bundle(
        cstr!("unhandled_interrupt"),
        ((cs() as u32) << 4).wrapping_add(ip() as u32),
        msg.as_ptr() as *const c_char,
    );
    shim_flush_all_streams();
    libc::abort();
}

#[no_mangle]
pub static mut isr_depth: u8 = 0;
#[no_mangle]
pub static mut critical_depth: u8 = 0;
#[no_mangle]
pub static mut interrupt_shadow: u8 = 0;
#[no_mangle]
pub static mut irq0_pending: u8 = 0;
#[no_mangle]
pub static mut bios_tick_cycle_debt: u64 = 0;
#[no_mangle]
pub static mut irq_pending: [u8; 256] = [0; 256];
/// Count of non-zero entries in `irq_pending` (Fix 1 throughput). The safepoint
/// interrupt-delivery tail scanned all 256 slots on EVERY emulated instruction;
/// this lets it skip the scan when nothing is scheduled (the overwhelmingly
/// common case). Maintained in lockstep with `irq_pending` everywhere it changes.
#[no_mangle]
pub static mut irq_pending_count: u32 = 0;
#[no_mangle]
pub static mut last_host_time_ns: u64 = 0;

// ---------------------------------------------------------------------------
// 8259A PIC (master). Ports 0x20 (command) / 0x21 (IMR).
//
// This is the register that decides whether a raised IRQ ever reaches the CPU,
// and until now we modelled NONE of it: `isr_depth` stood in for the in-service
// register, the mask register did not exist at all (a game that masked the timer
// still got INT 08 from us), and EOI was a no-op. `isr_depth` is a bad proxy in
// both directions — it counts SOFTWARE interrupts (INT 21h/61h put nothing in
// service on a real 8259), and it misses the thing the hardware actually does:
// hold a level in service until the handler acknowledges it.
//
// `irq0_pending` / `irq_pending[]` are the request latch (IRR); these are the
// mask and in-service registers.
//
// Deliberately standalone statics, NOT fields of the snapshot structs: those are
// #[repr(C)] FROZEN and serialized byte-for-byte, so growing them would
// invalidate every existing save (same call as the PIT ch2 state).
/// Interrupt mask: 1 = that IRQ line is masked off. Power-on default is the
/// value the PC BIOS leaves behind — IRQ0 (timer), IRQ1 (keyboard), IRQ2
/// (cascade) and IRQ6 (floppy) enabled, everything else masked.
#[no_mangle]
pub static mut pic_imr: u8 = 0xB8;
/// In-service: bit n set from the INTA cycle that delivers IRQ n until the
/// handler EOIs. This is what stops a handler being re-entered by its own line,
/// and what blocks lower-priority lines while a higher one is being serviced.
#[no_mangle]
pub static mut pic_isr: u8 = 0;
/// OCW3 read select: a subsequent IN 0x20 returns the ISR (1) or the IRR (0).
static mut pic_read_isr: u8 = 0;
/// IRQ n is delivered as INT (base + n). The BIOS sets base = 0x08; a guest may
/// re-issue ICW1/ICW2 to move it, so don't hardcode the mapping.
#[no_mangle]
pub static mut pic_vector_base: u8 = 0x08;
/// Position in an ICW1..ICW4 initialization sequence (0 = not initializing).
/// Without this, the ICW2/3/4 bytes that follow ICW1 on port 0x21 would be
/// misread as mask writes.
static mut pic_icw_step: u8 = 0;
static mut pic_icw_needs_icw4: u8 = 0;
static mut pic_icw_single: u8 = 0;
/// Slave 8259 (ports 0xA0/0xA1). No device we emulate sits on IRQ8-15, so this
/// is state-only: it exists so a guest that reads back what it wrote sees its
/// own value instead of an open-bus guess.
static mut pic2_imr: u8 = 0xFF;
static mut pic2_isr: u8 = 0;
static mut pic2_read_isr: u8 = 0;

/// The master-PIC line an INT vector belongs to, or None if it is not one.
unsafe fn pic_irq_of_int(int_no: u8) -> Option<u8> {
    let base = pic_vector_base;
    if int_no >= base && int_no < base.wrapping_add(8) {
        Some(int_no - base)
    } else {
        None
    }
}

/// 8259 priority: IRQ0 is highest. A request is held off while any line of
/// EQUAL or HIGHER priority is still in service — equal included, which is what
/// keeps a handler from being re-entered by its own line before it EOIs.
unsafe fn pic_can_deliver(irq: u8) -> bool {
    if pic_imr & (1u8 << irq) != 0 {
        return false;
    }
    let equal_or_higher = (((1u16 << (irq + 1)) - 1) & 0xFF) as u8;
    pic_isr & equal_or_higher == 0
}

/// The INTA cycle: the delivered line goes in service until its handler EOIs.
unsafe fn pic_ack(int_no: u8) {
    if let Some(irq) = pic_irq_of_int(int_no) {
        pic_isr |= 1u8 << irq;
    }
}

/// The master's request latch, assembled from the pending flags for the lines we
/// actually drive (IRQ0 timer, and whatever has been marked pending).
unsafe fn pic_irr() -> u8 {
    let mut irr = 0u8;
    if irq0_pending != 0 {
        irr |= 1;
    }
    if irq_pending_count > 0 {
        for irq in 0..8u8 {
            if irq_pending[(pic_vector_base.wrapping_add(irq)) as usize] != 0 {
                irr |= 1u8 << irq;
            }
        }
    }
    irr
}

/// EOI (OCW2). Non-specific clears the highest-priority line in service;
/// specific clears the named one. Clearing a level can unblock a lower-priority
/// request, so this OPENS the delivery gate — arm a recognition point.
unsafe fn pic_eoi(value: u8) {
    if value & 0x40 != 0 {
        pic_isr &= !(1u8 << (value & 0x07));
    } else if pic_isr != 0 {
        pic_isr &= pic_isr - 1; // clear lowest set bit == highest priority
    }
    shim_irq_recheck();
}

static mut isr_expected_sp: [u16; 256] = [0; 256];
#[no_mangle]
pub static mut lcall_depth: u8 = 0;
static mut lcall_expected_sp: [u16; 256] = [0; 256];
static mut lcall_expected_ss: [u16; 256] = [0; 256];
static mut lcall_ret_ip: [u16; 256] = [0; 256];
static mut lcall_ret_cs: [u16; 256] = [0; 256];
static mut last_retf_pop_bytes: u16 = 0;

#[no_mangle]
pub static mut dispatch_depth: u16 = 0;
#[no_mangle]
pub static mut shim_input_phase_started: c_int = 0;
#[no_mangle]
pub static mut dd_inc_call_table: u64 = 0;
#[no_mangle]
pub static mut dd_dec_call_table: u64 = 0;
#[no_mangle]
pub static mut dd_inc_via_binary: u64 = 0;
#[no_mangle]
pub static mut dd_dec_via_binary: u64 = 0;
#[no_mangle]
pub static mut dd_inc_overlay_first: u64 = 0;
#[no_mangle]
pub static mut dd_dec_overlay_first: u64 = 0;

const SHIM_ACTIVE_BINARY_MAX: usize = 64;
// Exported (with the tail_dispatch_* statics) so the chunk prelude can inline
// enter/leave_binary and tail_dispatch_save/restore — the per-function wrapper
// bookkeeping — without an FFI hop; layout is part of the chunk ABI.
#[no_mangle]
pub static mut active_binary_stack: [*const c_char; SHIM_ACTIVE_BINARY_MAX] =
    [ptr::null(); SHIM_ACTIVE_BINARY_MAX];
#[no_mangle]
pub static mut active_binary_top: c_int = 0;

// ============================================================================
// Active-binary stack + dispatch-fn lookup  [C lines 1020-1047]
// ============================================================================

#[no_mangle]
pub unsafe extern "C" fn shim_enter_binary(name: *const c_char) {
    if active_binary_top < SHIM_ACTIVE_BINARY_MAX as c_int {
        active_binary_stack[active_binary_top as usize] = name;
    }
    active_binary_top += 1;
}

#[no_mangle]
pub unsafe extern "C" fn shim_leave_binary() {
    if active_binary_top > 0 {
        active_binary_top -= 1;
    }
}

#[no_mangle]
pub unsafe extern "C" fn shim_active_binary() -> *const c_char {
    if active_binary_top <= 0 {
        return ptr::null();
    }
    let mut top = active_binary_top - 1;
    if top >= SHIM_ACTIVE_BINARY_MAX as c_int {
        top = SHIM_ACTIVE_BINARY_MAX as c_int - 1;
    }
    active_binary_stack[top as usize]
}

#[no_mangle]
pub unsafe extern "C" fn shim_dispatch_fn_by_module(module: *const c_char) -> ShimDispatchFn {
    if module.is_null() || cfg().binary_dispatch.is_null() {
        return None;
    }
    for i in 0..cfg().binary_dispatch_count {
        let bd = &*cfg().binary_dispatch.add(i);
        if !bd.module.is_null() && bd.fn_.is_some() && libc::strcmp(bd.module, module) == 0 {
            return bd.fn_;
        }
    }
    None
}

// ============================================================================
// Critical sections + monotonic clock + session log  [C lines 1053-1178]
// ============================================================================

static mut critical_owner_name: *const c_char = ptr::null();
static mut critical_owner_file: *const c_char = ptr::null();
static mut critical_owner_func: *const c_char = ptr::null();
static mut critical_owner_line: c_int = 0;
const CRITICAL_MAX_DEPTH: usize = 16;
static mut critical_owner_name_stk: [*const c_char; CRITICAL_MAX_DEPTH] =
    [ptr::null(); CRITICAL_MAX_DEPTH];

#[no_mangle]
pub unsafe extern "C" fn shim_host_monotonic_ns() -> u64 {
    let mut ts: libc::timespec = core::mem::zeroed();
    libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts);
    ts.tv_sec as u64 * 1000000000u64 + ts.tv_nsec as u64
}

static mut session_log_fp: *mut FILE = ptr::null_mut();
static mut session_log_path: [u8; 256] = [0; 256];

unsafe fn session_log_init() {
    if !session_log_fp.is_null() {
        return;
    }
    let dir = cstr!("sessions");
    if libc::mkdir(dir, 0o755) != 0 && *libc::__errno_location() != libc::EEXIST {
        shim_log_stdout(
            cstr!("[SESSION] mkdir sessions: %s\n"),
            libc::strerror(*libc::__errno_location()),
        );
        return;
    }
    libc::snprintf(
        ptr::addr_of_mut!(session_log_path) as *mut c_char,
        (*ptr::addr_of!(session_log_path)).len(),
        cstr!("%s/session.log"),
        dir,
    );
    session_log_fp = libc::fopen(ptr::addr_of!(session_log_path) as *const c_char, cstr!("w"));
    if session_log_fp.is_null() {
        shim_log_stdout(
            cstr!("[SESSION] fopen %s: %s\n"),
            ptr::addr_of!(session_log_path) as *const c_char,
            libc::strerror(*libc::__errno_location()),
        );
        return;
    }
    libc::setvbuf(session_log_fp, ptr::null_mut(), libc::_IOLBF, 0);
    libc::fprintf(
        session_log_fp,
        cstr!("# session log, speedup=%g\n"),
        emulation_speedup,
    );
    shim_log_stdout(
        cstr!("[SESSION] logging stdin to %s\n"),
        ptr::addr_of!(session_log_path) as *const c_char,
    );
}

unsafe fn session_log_bytes(buf: *const u8, n: usize) {
    if session_log_fp.is_null() {
        session_log_init();
    }
    if session_log_fp.is_null() || n == 0 {
        return;
    }
    let vns = shim_virtual_now_ns();
    libc::fprintf(
        session_log_fp,
        cstr!("vns=%llu  bytes="),
        vns as c_ulonglong,
    );
    for i in 0..n {
        libc::fprintf(
            session_log_fp,
            cstr!("%02X%s"),
            *buf.add(i) as c_uint,
            if i + 1 < n { cstr!(" ") } else { cstr!("") },
        );
    }
    libc::fputc(b'\n' as c_int, session_log_fp);
}

unsafe fn session_logged_read(buf: *mut c_void, n: usize) -> isize {
    let r = libc::read(keyboard_fd, buf, n);
    if r > 0 {
        session_log_bytes(buf as *const u8, r as usize);
    }
    r
}

unsafe fn session_log_write_to_bundle(dir: *const c_char) {
    if session_log_fp.is_null() {
        return;
    }
    libc::fflush(session_log_fp);
    let src = libc::fopen(ptr::addr_of!(session_log_path) as *const c_char, cstr!("r"));
    if src.is_null() {
        return;
    }
    let mut path = [0u8; 512];
    libc::snprintf(
        path.as_mut_ptr() as *mut c_char,
        path.len(),
        cstr!("%s/session.log"),
        dir,
    );
    let dst = libc::fopen(path.as_ptr() as *const c_char, cstr!("w"));
    if dst.is_null() {
        libc::fclose(src);
        return;
    }
    let mut buf = [0u8; 4096];
    loop {
        let r = libc::fread(buf.as_mut_ptr() as *mut c_void, 1, buf.len(), src);
        if r == 0 {
            break;
        }
        libc::fwrite(buf.as_ptr() as *const c_void, 1, r, dst);
    }
    libc::fclose(src);
    libc::fclose(dst);
}

#[no_mangle]
pub static mut last_sw_interrupt: InterruptSnapshot = InterruptSnapshot {
    valid: 0,
    int_no: 0,
    ax_before: 0,
    bx_before: 0,
    cx_before: 0,
    dx_before: 0,
    ds_before: 0,
    es_before: 0,
    ss_before: 0,
    sp_before: 0,
    cs_before: 0,
    ip_before: 0,
    ax_after: 0,
    bx_after: 0,
    cx_after: 0,
    dx_after: 0,
    ds_after: 0,
    es_after: 0,
    ss_after: 0,
    sp_after: 0,
    cs_after: 0,
    ip_after: 0,
    file: ptr::null(),
    func: ptr::null(),
    line: 0,
};

unsafe fn critical_section_abort(
    reason: *const c_char,
    attempt_name: *const c_char,
    attempt_file: *const c_char,
    attempt_func: *const c_char,
    attempt_line: c_int,
) {
    shim_log_stderr(
        cstr!("Error: %s by %s (%s:%s:%d)\n"),
        reason,
        attempt_name,
        attempt_file,
        attempt_func,
        attempt_line,
    );
    if !critical_owner_name.is_null() {
        shim_log_stderr(
            cstr!("       Active critical section owned by %s (%s:%s:%d)\n"),
            critical_owner_name,
            critical_owner_file,
            critical_owner_func,
            critical_owner_line,
        );
    } else {
        shim_log_stderr(cstr!("       No active critical section owner recorded\n"));
    }
    shim_flush_all_streams();
    libc::exit(1);
}

#[no_mangle]
pub unsafe extern "C" fn critical_section_enter(
    name: *const c_char,
    file: *const c_char,
    func: *const c_char,
    line: c_int,
) {
    if critical_depth >= CRITICAL_MAX_DEPTH as u8 {
        critical_section_abort(
            cstr!("critical section nested too deep (runaway recursion)"),
            name,
            file,
            func,
            line,
        );
    }
    critical_owner_name_stk[critical_depth as usize] = name;
    critical_owner_name = name;
    critical_owner_file = file;
    critical_owner_func = func;
    critical_owner_line = line;
    critical_depth += 1;
}

#[no_mangle]
pub unsafe extern "C" fn critical_section_exit(
    name: *const c_char,
    file: *const c_char,
    func: *const c_char,
    line: c_int,
) {
    if critical_depth == 0 {
        critical_section_abort(
            cstr!("critical section exit without matching entry"),
            name,
            file,
            func,
            line,
        );
    }
    critical_depth -= 1;
    let expected = critical_owner_name_stk[critical_depth as usize];
    if !expected.is_null() && libc::strcmp(expected, name) != 0 {
        critical_section_abort(
            cstr!("critical section ownership mismatch on exit"),
            name,
            file,
            func,
            line,
        );
    }
    if critical_depth > 0 {
        critical_owner_name = critical_owner_name_stk[critical_depth as usize - 1];
    } else {
        critical_owner_name = ptr::null();
        critical_owner_file = ptr::null();
        critical_owner_func = ptr::null();
        critical_owner_line = 0;
        // Leaving the outermost critical section opens the delivery gate.
        shim_irq_recheck();
    }
}

#[no_mangle]
pub unsafe extern "C" fn shim_dos_input_wait_begin(saved_crit: *mut u8, saved_if: *mut u8) {
    *saved_crit = critical_depth;
    *saved_if = IF();
    critical_depth = 0;
    set_IF(1);
    // Dropping the critical section and forcing IF=1 opens the delivery gate —
    // the point of a DOS input wait is that interrupts keep flowing through it.
    shim_irq_recheck();
}

#[no_mangle]
pub unsafe extern "C" fn shim_dos_input_wait_end(saved_crit: u8, saved_if: u8) {
    critical_depth = saved_crit;
    set_IF(saved_if);
}

#[no_mangle]
pub unsafe extern "C" fn ascii_to_scan(c: u8) -> u8 {
    if c >= b'a' && c <= b'z' {
        static MAP: [u8; 26] = [
            0x1E, 0x30, 0x2E, 0x20, 0x12, 0x21, 0x22, 0x23, 0x17, 0x24, 0x25, 0x26, 0x32, 0x31,
            0x18, 0x19, 0x10, 0x13, 0x1F, 0x14, 0x16, 0x2F, 0x11, 0x2D, 0x15, 0x2C,
        ];
        return MAP[(c - b'a') as usize];
    }
    if c >= b'A' && c <= b'Z' {
        static MAP: [u8; 26] = [
            0x1E, 0x30, 0x2E, 0x20, 0x12, 0x21, 0x22, 0x23, 0x17, 0x24, 0x25, 0x26, 0x32, 0x31,
            0x18, 0x19, 0x10, 0x13, 0x1F, 0x14, 0x16, 0x2F, 0x11, 0x2D, 0x15, 0x2C,
        ];
        return MAP[(c - b'A') as usize];
    }
    if c >= b'0' && c <= b'9' {
        static MAP: [u8; 10] = [0x0B, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A];
        return MAP[(c - b'0') as usize];
    }
    match c {
        b'\r' | b'\n' => 0x1C,
        27 => 0x01,
        b' ' => 0x39,
        0x08 => 0x0E,
        b'\t' => 0x0F,
        _ => 0,
    }
}

// Slots 0x01..0x7F are plain (keypad) codes; slots 0x81..0xFF are the same
// 7-bit codes pressed as extended (grey, 0xE0-prefixed) keys — the scheduled
// release must emit the matching variant.
static mut pending_release_deadline_ns: [u64; 256] = [0; 256];

unsafe fn pending_release_tick() {
    let now = shim_virtual_now_ns();
    for i in 1..256 {
        if i == 0x80 {
            continue;
        }
        if pending_release_deadline_ns[i] != 0 && now >= pending_release_deadline_ns[i] {
            pending_release_deadline_ns[i] = 0;
            if i & 0x80 != 0 {
                shim_keyboard_enqueue_scancode_release_ext((i & 0x7F) as u8);
            } else {
                shim_keyboard_enqueue_scancode_release(i as u8);
            }
            shim_log_stdout(
                cstr!("[TAP] release sc=0x%02X ext=%d fired virtual_ns=%llu\n"),
                (i & 0x7F) as c_uint,
                ((i & 0x80) != 0) as c_int,
                now as c_ulonglong,
            );
        }
    }
}

unsafe fn init_virtual_display() {
    virtual_display_init(320, 200, 3);
    current_display_width = 320;
    current_display_height = 200;
}

unsafe fn quit_virtual_display() {
    virtual_display_shutdown();
}

// ============================================================================
// Keyboard raw-mode init/cleanup (constructor/destructor)  [C lines 1343-1395]
// ============================================================================

unsafe extern "C" fn init_keyboard() {
    // NOTE: --replay's initial vclock halt lives in saisei_main's argv parse
    // (runs before run_machine), not here — the flag isn't known at ctor time.
    keyboard_fd = libc::STDIN_FILENO;
    if libc::tcgetattr(keyboard_fd, ptr::addr_of_mut!(orig_termios)) != 0 {
        let tty = libc::open(cstr!("/dev/tty"), libc::O_RDONLY);
        if tty >= 0 && libc::tcgetattr(tty, ptr::addr_of_mut!(orig_termios)) == 0 {
            keyboard_fd = tty;
        } else {
            if tty >= 0 {
                libc::close(tty);
            }
            keyboard_fd = libc::STDIN_FILENO;
            if libc::fcntl(keyboard_fd, libc::F_SETFL, libc::O_NONBLOCK) == 0 {
                keyboard_input_enabled = 1;
            }
            keyboard_blocking_enabled = 0;
            return;
        }
    }
    let mut raw: libc::termios = orig_termios;
    libc::cfmakeraw(&mut raw);
    raw.c_oflag = orig_termios.c_oflag;
    libc::tcsetattr(keyboard_fd, libc::TCSANOW, &raw);
    libc::fcntl(keyboard_fd, libc::F_SETFL, libc::O_NONBLOCK);
    keyboard_input_enabled = 1;
    keyboard_blocking_enabled = 1;
    keyboard_initialized = 1;
}

unsafe extern "C" fn cleanup_keyboard() {
    if keyboard_initialized != 0 {
        libc::tcsetattr(keyboard_fd, libc::TCSANOW, ptr::addr_of!(orig_termios));
        if keyboard_fd != libc::STDIN_FILENO {
            libc::close(keyboard_fd);
        }
    }
}

#[used]
#[link_section = ".init_array"]
static INIT_KEYBOARD_CTOR: unsafe extern "C" fn() = init_keyboard;
#[used]
#[link_section = ".fini_array"]
static CLEANUP_KEYBOARD_DTOR: unsafe extern "C" fn() = cleanup_keyboard;

// ============================================================================
// Interrupt scheduling / injection  [C lines 1397-1650]
// ============================================================================

#[no_mangle]
pub unsafe extern "C" fn shim_set_timer_isr(segment: u16, offset: u16) {
    memw_raw_write(0, 0x08 * 4, offset);
    memw_raw_write(0, 0x08 * 4 + 2, segment);
}

#[no_mangle]
pub unsafe extern "C" fn schedule_interrupt_impl(
    int_no: u8,
    file: *const c_char,
    func: *const c_char,
    line: c_int,
) {
    shim_log(
        cstr!("schedule_interrupt_impl"),
        file,
        func,
        line,
        ptr::null(),
    );
    shim_mark_irq_pending(int_no);
}

/// The single entry point for raising a pending IRQ: sets `irq_pending[int_no]`
/// and keeps `irq_pending_count` in sync so the safepoint scan-skip (Fix 1)
/// stays correct. Used by `schedule_interrupt` AND the keyboard IRQ1 path
/// (runtime/src/keyboard.rs) — any code that marks an IRQ pending MUST go
/// through here, never write the array directly, or the scan will be skipped
/// while an interrupt is actually pending.
#[no_mangle]
pub unsafe extern "C" fn shim_mark_irq_pending(int_no: u8) {
    if irq_pending[int_no as usize] == 0 {
        irq_pending_count += 1;
    }
    irq_pending[int_no as usize] = 1;
}

#[no_mangle]
pub unsafe extern "C" fn schedule_interrupt(int_no: u8) {
    schedule_interrupt_impl(int_no, cstr!("<external>"), cstr!("schedule_interrupt"), 0);
}

#[no_mangle]
pub unsafe extern "C" fn shim_invoke_far_call(
    seg: u16,
    off: u16,
    r_ax: u16,
    r_bx: u16,
    r_cx: u16,
    r_dx: u16,
    r_si: u16,
    r_di: u16,
) {
    let s_cs = cs();
    let s_ip = ip();
    let s_ss = ss();
    let s_sp = sp();
    let s_ax = ax();
    let s_bx = bx();
    let s_cx = cx();
    let s_dx = dx();
    let s_si = si();
    let s_di = di();
    let s_bp = bp();
    let s_ds = ds();
    let s_es = es();
    let s_cf = CF();
    let s_pf = PF();
    let s_zf = ZF();
    let s_sf = SF();
    let s_of = OF();
    let s_if = IF();
    let s_df = DF();
    set_ax(r_ax);
    set_bx(r_bx);
    set_cx(r_cx);
    set_dx(r_dx);
    set_si(r_si);
    set_di(r_di);
    let sp_entry = sp();
    set_sp(sp().wrapping_sub(2));
    memw_write_impl(
        ss(),
        sp(),
        s_cs,
        SHIMS_FILE,
        cstr!("shim_invoke_far_call"),
        1429,
    );
    set_sp(sp().wrapping_sub(2));
    memw_write_impl(
        ss(),
        sp(),
        s_ip,
        SHIMS_FILE,
        cstr!("shim_invoke_far_call"),
        1430,
    );
    set_cs(seg);
    set_ip(off);
    isr_depth += 1;
    isr_expected_sp[isr_depth as usize] = s_sp;
    while machine_halted == 0 && (sp().wrapping_sub(sp_entry) as i16) < 0 {
        let addr = ((cs() as u32) << 4) + ip() as u32;
        if resolve_and_run_chunk(addr) == 0 {
            shim_log_crash(
                cstr!("[BUG] far callback reached unmapped cs:ip=%04X:%04X\n"),
                cs() as c_uint,
                ip() as c_uint,
            );
            shim_flush_all_streams();
            libc::exit(1);
        }
    }
    isr_depth -= 1;
    // The far callback returning drops isr_depth, which opens the delivery gate.
    shim_irq_recheck();
    set_cs(s_cs);
    set_ip(s_ip);
    set_ss(s_ss);
    set_sp(s_sp);
    set_ax(s_ax);
    set_bx(s_bx);
    set_cx(s_cx);
    set_dx(s_dx);
    set_si(s_si);
    set_di(s_di);
    set_bp(s_bp);
    set_ds(s_ds);
    set_es(s_es);
    set_CF(s_cf);
    set_PF(s_pf);
    set_ZF(s_zf);
    set_SF(s_sf);
    set_OF(s_of);
    set_IF(s_if);
    set_DF(s_df);
}

unsafe fn invoke_isr(
    int_no: u8,
    preserve_regs: c_int,
    preserve_stack: c_int,
    preserve_segments: c_int,
    ret_ip: u16,
    source: *const c_char,
    func: *const c_char,
    line: c_int,
) {
    jit_instr_budget -= 30; // int + iret ≈ 59+38 cycles on a 386
                            // Interrupts nest (that is the point of IF and the PIC's priority
                            // levels), and each level costs a frame of native recursion through
                            // resolve_and_run_chunk. `isr_expected_sp` is 256 deep and isr_depth is
                            // a u8, so runaway nesting would wrap the counter and corrupt the stack
                            // bookkeeping silently. A real machine would fault; fail loudly instead
                            // of limping. Legitimate depth is small — the 8259 holds a line in
                            // service until EOI, so a handler cannot be re-entered by its own line.
    if isr_depth >= 200 {
        let mut msg = [0u8; 256];
        libc::snprintf(
            msg.as_mut_ptr() as *mut c_char,
            msg.len(),
            cstr!("[BUG] interrupt nesting runaway: isr_depth=%u delivering int 0x%02X (a handler is not returning, or an IRQ is being re-delivered without an EOI)\n"),
            isr_depth as c_uint,
            int_no as c_uint,
        );
        shim_log_crash(cstr!("%s"), msg.as_ptr() as *const c_char);
        save_bug_bundle(
            cstr!("isr_nesting_runaway"),
            ((cs() as u32) << 4) + ip() as u32,
            msg.as_ptr() as *const c_char,
        );
        shim_flush_all_streams();
        libc::exit(1);
    }
    let saved_ss = ss();
    let saved_sp = sp();
    let mut saved_stack_word0: u16 = 0;
    let mut saved_stack_word1: u16 = 0;
    let mut saved_ax: u16 = 0;
    let mut saved_bx: u16 = 0;
    let mut saved_cx: u16 = 0;
    let mut saved_dx: u16 = 0;
    let mut saved_si: u16 = 0;
    let mut saved_di: u16 = 0;
    let mut saved_bp: u16 = 0;
    let mut saved_cf: u8 = 0;
    let mut saved_pf: u8 = 0;
    let mut saved_zf: u8 = 0;
    let mut saved_sf: u8 = 0;
    let mut saved_of: u8 = 0;
    let mut saved_if: u8 = 0;
    let mut saved_df: u8 = 0;
    let mut saved_ds: u16 = 0;
    let mut saved_es: u16 = 0;
    let saved_cs = cs();
    if preserve_stack != 0 {
        saved_stack_word0 =
            memw_read_impl(saved_ss, saved_sp, SHIMS_FILE, cstr!("invoke_isr"), 1478);
        saved_stack_word1 = memw_read_impl(
            saved_ss,
            saved_sp.wrapping_add(2),
            SHIMS_FILE,
            cstr!("invoke_isr"),
            1479,
        );
    }
    if preserve_regs != 0 || preserve_segments != 0 {
        saved_ds = ds();
        saved_es = es();
    }
    if preserve_regs != 0 {
        saved_ax = ax();
        saved_bx = bx();
        saved_cx = cx();
        saved_dx = dx();
        saved_si = si();
        saved_di = di();
        saved_bp = bp();
        saved_cf = CF();
        saved_pf = PF();
        saved_zf = ZF();
        saved_sf = SF();
        saved_of = OF();
        saved_if = IF();
        saved_df = DF();
    }

    let flags: u16 = 0x0002u16
        | (CF() as u16)
        | ((PF() as u16) << 2)
        | ((ZF() as u16) << 6)
        | ((SF() as u16) << 7)
        | ((IF() as u16) << 9)
        | ((DF() as u16) << 10)
        | ((OF() as u16) << 11);
    set_sp(sp().wrapping_sub(2) & 0xFFFF);
    memw_write_impl(ss(), sp(), flags, SHIMS_FILE, cstr!("invoke_isr"), 1506);
    set_sp(sp().wrapping_sub(2) & 0xFFFF);
    memw_write_impl(ss(), sp(), cs(), SHIMS_FILE, cstr!("invoke_isr"), 1508);
    set_sp(sp().wrapping_sub(2) & 0xFFFF);
    memw_write_impl(ss(), sp(), ret_ip, SHIMS_FILE, cstr!("invoke_isr"), 1510);
    shim_log_stdout(cstr!("Trace: isr_depth enter (IF->0)\n"));
    set_IF(0);
    let vector_off: u16 = (int_no as u16) * 4;
    let isr_ip = memw_raw_read(0, vector_off);
    let isr_cs = memw_raw_read(0, vector_off + 2);
    last_int_no = int_no;
    isr_depth += 1;
    isr_expected_sp[isr_depth as usize] = sp();
    let sp_at_invoke_isr_entry = saved_sp;
    set_cs(isr_cs);
    set_ip(isr_ip);
    record_binary_cs(((isr_cs as u32) << 4) + isr_ip as u32, isr_cs);
    shim_log_stdout(
        cstr!("Trace: isr_depth run: %d preserve=%d stack=%d seg=%d ret_ip=%04X target=%04X:%04X sp=%04X flags=0x%04X\n"),
        isr_depth as c_int,
        preserve_regs,
        preserve_stack,
        preserve_segments,
        ret_ip as c_uint,
        isr_cs as c_uint,
        isr_ip as c_uint,
        sp() as c_uint,
        flags as c_uint,
    );
    while machine_halted == 0 && (sp().wrapping_sub(sp_at_invoke_isr_entry) as i16) < 0 {
        let addr = ((cs() as u32) << 4) + ip() as u32;
        if resolve_and_run_chunk(addr) == 0 {
            let mut msg = [0u8; 512];
            let mn = libc::snprintf(
                msg.as_mut_ptr() as *mut c_char,
                msg.len(),
                cstr!("[BUG] ISR (int 0x%02X) reached unmapped cs:ip=%04X:%04X (linear 0x%05X) sp=%04X (%s:%s:%d)\n"),
                int_no as c_uint,
                cs() as c_uint,
                ip() as c_uint,
                addr as c_uint,
                sp() as c_uint,
                if source.is_null() { cstr!("?") } else { source },
                if func.is_null() { cstr!("?") } else { func },
                line,
            );
            shim_log_crash(cstr!("%s"), msg.as_ptr() as *const c_char);
            if mn > 0 {
                save_bug_bundle(cstr!("isr_unmapped"), addr, msg.as_ptr() as *const c_char);
            }
            shim_flush_all_streams();
            libc::exit(1);
        }
    }
    shim_log_stdout(
        cstr!("Trace: isr_depth resume: %d last_int=0x%02X return cs:ip=%04X:%04X sp=%04X IF=%d\n"),
        isr_depth as c_int,
        last_int_no as c_uint,
        cs() as c_uint,
        ip() as c_uint,
        sp() as c_uint,
        IF() as c_int,
    );
    isr_depth -= 1;
    shim_log_stdout(cstr!("Trace: isr_depth exit -> %d\n"), isr_depth as c_int);
    if preserve_stack != 0 {
        let after_word0 = memw_read_impl(saved_ss, saved_sp, SHIMS_FILE, cstr!("invoke_isr"), 1563);
        let after_word1 = memw_read_impl(
            saved_ss,
            saved_sp.wrapping_add(2),
            SHIMS_FILE,
            cstr!("invoke_isr"),
            1564,
        );
        if after_word0 != saved_stack_word0 || after_word1 != saved_stack_word1 {
            shim_log_stdout(
                cstr!("Trace: stack-top changed across int 0x%02X (%s:%s:%d) ss:sp=%04X:%04X [%04X %04X] -> [%04X %04X]\n"),
                int_no as c_uint,
                if source.is_null() { cstr!("<unknown>") } else { source },
                if func.is_null() { cstr!("<unknown>") } else { func },
                line,
                saved_ss as c_uint,
                saved_sp as c_uint,
                saved_stack_word0 as c_uint,
                saved_stack_word1 as c_uint,
                after_word0 as c_uint,
                after_word1 as c_uint,
            );
        }
        set_ss(saved_ss);
        set_sp(saved_sp);
    }
    if preserve_regs != 0 {
        set_cs(saved_cs);
        set_ip(ret_ip);
        set_ax(saved_ax);
        set_bx(saved_bx);
        set_cx(saved_cx);
        set_dx(saved_dx);
        set_si(saved_si);
        set_di(saved_di);
        set_bp(saved_bp);
        set_CF(saved_cf);
        set_PF(saved_pf);
        set_ZF(saved_zf);
        set_SF(saved_sf);
        set_OF(saved_of);
        set_IF(saved_if);
        set_DF(saved_df);
    }
    if preserve_regs != 0 || preserve_segments != 0 {
        set_ds(saved_ds);
        set_es(saved_es);
    }

    shim_irq_recheck();
}

/// Arm interrupt recognition at the next basic-block boundary.
///
/// THE INVARIANT: a waiting interrupt is taken at the first instruction boundary
/// where the delivery gate is open. The gate has exactly four inputs —
///
///     deliver ⟺ pending ∧ IF ∧ ¬shadow ∧ isr_depth==0 ∧ critical_depth==0
///
/// — but our only recognition point is a safepoint, and a safepoint happens
/// where the instruction BUDGET expires. Those are different boundaries, and the
/// difference is not benign, because `safe_point_impl` REFILLS the budget: if
/// the expiry lands in a region where the gate is shut, the fresh quantum is
/// spent there too, and the next expiry lands in the same kind of region again.
/// The recognition point gets *captured* by any guest shape that correlates with
/// a gate input, and the interrupt then starves FOREVER. Zeliard hit it via the
/// ISR (a ~237Hz PIT and an INT 61h service longer than one quantum: every
/// safepoint saw isr_depth>0, so INT 08 never ran again); a `cli`..`sti` loop
/// captures it identically via IF, and the shadow branch below via ¬shadow.
///
/// So this is called at EVERY transition that can open the gate — the closed set
/// of them, which is what makes the bug class gone rather than this one game
/// fixed:
///   • IF 0→1     — `sti`, `popf` (emitted inline: rt::irq_arm), `iret`
///   • isr_depth→0 — an ISR or far-callback returning
///   • critical_depth→0 — a runtime critical section ending
///   • ¬shadow    — the shadow being consumed at a safepoint
/// Retiring the budget makes the next block head a safepoint, so recognition
/// lands on the boundary the CPU would have used. Folding through
/// `shim_time_sync` first is what keeps it honest: the UNSPENT remainder is not
/// executed time and must not be billed to the virtual clock.
#[no_mangle]
pub unsafe extern "C" fn shim_irq_recheck() {
    if (irq0_pending != 0 || irq_pending_count > 0) && jit_instr_budget > 0 {
        shim_time_sync();
        jit_instr_budget = 0;
        jit_budget_last_refill = 0;
    }
}

#[no_mangle]
pub unsafe extern "C" fn run_interrupt_impl(
    int_no: u8,
    file: *const c_char,
    func: *const c_char,
    line: c_int,
) {
    shim_log(cstr!("run_interrupt_impl"), file, func, line, ptr::null());
    bios_timer_tick_preincremented = 0;
    let return_ip: u16 = ip().wrapping_add(2);
    last_sw_interrupt.valid = 1;
    last_sw_interrupt.int_no = int_no;
    last_sw_interrupt.file = file;
    last_sw_interrupt.func = func;
    last_sw_interrupt.line = line;
    last_sw_interrupt.ax_before = ax();
    last_sw_interrupt.bx_before = bx();
    last_sw_interrupt.cx_before = cx();
    last_sw_interrupt.dx_before = dx();
    last_sw_interrupt.ds_before = ds();
    last_sw_interrupt.es_before = es();
    last_sw_interrupt.ss_before = ss();
    last_sw_interrupt.sp_before = sp();
    last_sw_interrupt.cs_before = cs();
    last_sw_interrupt.ip_before = ip();

    invoke_isr(int_no, 0, 1, 0, return_ip, cstr!("<interrupt>"), func, line);

    last_sw_interrupt.ax_after = ax();
    last_sw_interrupt.bx_after = bx();
    last_sw_interrupt.cx_after = cx();
    last_sw_interrupt.dx_after = dx();
    last_sw_interrupt.ds_after = ds();
    last_sw_interrupt.es_after = es();
    last_sw_interrupt.ss_after = ss();
    last_sw_interrupt.sp_after = sp();
    last_sw_interrupt.cs_after = cs();
    last_sw_interrupt.ip_after = ip();
    if int_no == 0x60 {
        log_last_sw_interrupt_snapshot();
    }
}

#[no_mangle]
pub unsafe extern "C" fn run_interrupt(int_no: u8) {
    run_interrupt_impl(int_no, cstr!("<external>"), cstr!("run_interrupt"), 0);
}

// ============================================================================
// Linear-block copy + safe_point (frame pacing, stdin opcodes, IRQ delivery)
// [C lines 1654-2081]
// ============================================================================

#[no_mangle]
pub unsafe extern "C" fn copy_linear_from_segoff(seg: u16, off: u16, len: usize, dst: *mut u8) {
    let mut s = seg;
    let mut o = off;
    for i in 0..len {
        *dst.add(i) = memb_read_impl(s, o, SHIMS_FILE, cstr!("copy_linear_from_segoff"), 1659);
        let old = o;
        o = o.wrapping_add(1);
        if o < old {
            s = s.wrapping_add(0x1000);
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn shim_copy_linear_block(seg: u16, off: u16, len: usize, dst: *mut u8) {
    copy_linear_from_segoff(seg, off, len, dst);
}

/// Instructions handed to the chunks per budget refill (~102µs of game time at
/// the default 10 MIPS). Small enough for sub-ms IRQ latency and PIT fidelity,
/// large enough that the slow path is amortized to noise.
const JIT_BUDGET_QUANTUM: i64 = 1024;
const NS_PER_BIOS_TICK: u64 = 54_925_000;

// Real-time pacing anchors: virtual (instruction-driven) time is slaved to the
// host clock scaled by emulation_speedup, by sleeping in the slow path when
// virtual runs ahead. When the HOST stalls (a JIT compile, scheduler hiccup)
// we re-anchor instead of letting the game fast-forward to catch up.
static mut pacing_host_anchor_ns: u64 = 0;
static mut pacing_virtual_anchor_ns: u64 = 0;
static mut last_stdin_poll_host_ns: u64 = 0;
// perf counters (dumped by FIFO opcode 0x1E and the exit report)
static mut perf_sp_visits: u64 = 0;
// Why a latched IRQ0 did not get delivered at a safepoint. A stuck `irq0_pending`
// means the guest's timer handler never runs, which stalls anything the guest
// clocks off it; these partition the veto exhaustively (shadow / in-ISR / IF=0 /
// critical section / another interrupt ahead of it) so the culprit is read off a
// freeze sample instead of guessed at.
static mut perf_irq0_delivered: u64 = 0;
static mut perf_irq0_blk_shadow: u64 = 0;
static mut perf_irq0_blk_pic: u64 = 0;
static mut perf_irq0_blk_if: u64 = 0;
static mut perf_irq0_blk_crit: u64 = 0;
static mut perf_irq0_blk_other: u64 = 0;
static mut perf_sync_calls: u64 = 0;
static mut perf_pacing_sleeps: u64 = 0;
static mut perf_pacing_slept_ns: u64 = 0;
static mut perf_idle_waits: u64 = 0;
const PACING_SLACK_NS: u64 = 200_000; // ignore sub-slack drift
const PACING_MAX_SLEEP_NS: u64 = 2_000_000; // bounded slices: stay responsive
const PACING_FORGIVE_NS: u64 = 250_000_000; // host stall → re-anchor, don't fast-forward

unsafe fn pacing_service(virtual_now: u64, host_now: u64) {
    if vclock_state != VCLOCK_RUNNING {
        return; // halted/stepping: the control FIFO owns time, never sleep-sync
    }
    if pacing_host_anchor_ns == 0 {
        pacing_host_anchor_ns = host_now;
        pacing_virtual_anchor_ns = virtual_now;
        return;
    }
    let v_elapsed = virtual_now.saturating_sub(pacing_virtual_anchor_ns);
    let target_host =
        pacing_host_anchor_ns.wrapping_add((v_elapsed as f64 / emulation_speedup) as u64);
    if host_now.wrapping_add(PACING_SLACK_NS) < target_host {
        let mut d = target_host - host_now;
        if d > PACING_MAX_SLEEP_NS {
            d = PACING_MAX_SLEEP_NS;
        }
        perf_pacing_sleeps += 1;
        perf_pacing_slept_ns += d;
        let ts = libc::timespec {
            tv_sec: 0,
            tv_nsec: d as i64,
        };
        libc::nanosleep(&ts, ptr::null_mut());
    } else if host_now > target_host.wrapping_add(PACING_FORGIVE_NS) {
        pacing_host_anchor_ns = host_now;
        pacing_virtual_anchor_ns = virtual_now;
    }
}

/// Blocking-wait service for runtime-internal input waits (INT 16h AH=0, DOS
/// console reads) and the emitted `hlt`: the guest retires no instructions
/// while stalled, so the instruction-driven clock would freeze — but machine
/// time must keep flowing (PIT music, TAP release deadlines, BIOS ticks) at
/// real-time pace. Advance the virtual clock to track the host (scaled),
/// run the safepoint machinery, and yield a slice of host CPU.
#[no_mangle]
pub unsafe extern "C" fn shim_idle_wait() {
    perf_idle_waits += 1;
    if vclock_state == VCLOCK_RUNNING {
        if pacing_host_anchor_ns != 0 {
            let host_now = shim_host_monotonic_ns();
            let h_elapsed = host_now.saturating_sub(pacing_host_anchor_ns);
            let target_v = pacing_virtual_anchor_ns
                .wrapping_add((h_elapsed as f64 * emulation_speedup) as u64);
            if target_v > virtual_now_accum_ns {
                virtual_now_accum_ns = target_v;
            }
        }
    } else if vclock_state == VCLOCK_STEPPING {
        // A step progresses in bounded slices while the guest idles, so a
        // FIFO-driven step completes even when the program blocks on input.
        virtual_now_accum_ns = virtual_now_accum_ns.wrapping_add(1_000_000);
    }
    safe_point_impl(SHIMS_FILE, cstr!("shim_idle_wait"), 0);
    if vclock_state != VCLOCK_HALTED {
        let ts = libc::timespec {
            tv_sec: 0,
            tv_nsec: 1_000_000,
        };
        libc::nanosleep(&ts, ptr::null_mut());
    }
}

/// Fold the instructions the chunks retired since the last visit into the
/// virtual clock, refill the budget, and advance the PIT chain to "now".
/// Called from every safepoint slow-path visit AND from the PIT port
/// handlers: the guest must never observe a frozen counter — a calibration
/// loop reading the PIT twice within one budget quantum would compute a zero
/// delta and #DE on its division. Returns virtual now.
/// One-line execution snapshot to stderr (FIFO opcode 0x1E, exit report).
#[no_mangle]
pub unsafe extern "C" fn shim_perf_report(tag: *const c_char) {
    let host_elapsed_ns = shim_host_monotonic_ns().saturating_sub(host_time_origin_ns);
    let virt_elapsed_ns = shim_virtual_now_ns().saturating_sub(host_time_origin_ns);
    libc::fprintf(
        stderr,
        cstr!("[PERF %s] retired=%llu host=%.2fs virtual=%.2fs sp_visits=%llu syncs=%llu sleeps=%llu slept=%.2fs idle_waits=%llu isr_depth=%d cs:ip=%04X:%04X\n"),
        tag,
        jit_total_retired as c_ulonglong,
        host_elapsed_ns as f64 / 1e9,
        virt_elapsed_ns as f64 / 1e9,
        perf_sp_visits as c_ulonglong,
        perf_sync_calls as c_ulonglong,
        perf_pacing_sleeps as c_ulonglong,
        perf_pacing_slept_ns as f64 / 1e9,
        perf_idle_waits as c_ulonglong,
        isr_depth as c_int,
        cs() as c_uint,
        ip() as c_uint,
    );
    libc::fflush(stderr);
}

#[no_mangle]
pub unsafe extern "C" fn shim_time_sync() -> u64 {
    perf_sync_calls += 1;
    // Fold WITHOUT refilling: move the fold point to wherever the countdown
    // stands so the next fold sees only newer instructions, but leave the
    // countdown itself alone — a guest loop that polls the PIT every few
    // instructions must still hit the safepoint slow path (IRQ delivery,
    // pacing) when its budget runs out. Only safe_point_impl refills.
    let consumed = jit_budget_last_refill - jit_instr_budget;
    if consumed > 0 {
        jit_total_retired = jit_total_retired.wrapping_add(consumed as u64);
        vclock_advance_ns(consumed as u64 * jit_ns_per_instr);
    }
    jit_budget_last_refill = jit_instr_budget;

    vclock_service();
    let now_ns = shim_virtual_now_ns();
    // Virtual elapsed since the PIT chain last advanced. Capped so a stale
    // anchor (snapshot restore, clock mode change) can't dump an hour of
    // backlogged ticks at once.
    let mut elapsed_ns = now_ns.saturating_sub(last_host_time_ns);
    if elapsed_ns > 1_000_000_000 {
        elapsed_ns = 1_000_000_000;
    }
    last_host_time_ns = now_ns;

    // The 8254 is FREE-RUNNING: it counts continuously regardless of whether
    // the CPU is servicing an interrupt. Accumulating the PIT / BIOS tick only
    // at isr_depth==0 was unfaithful and — with the per-basic-block safepoint
    // model — a hard hang: a guest that spends most of its time inside an ISR
    // never advances the clock. Alley Cat's BIOS delay loop (`sub ah,ah;
    // int 1Ah` polling the tick) calls a software interrupt every iteration;
    // the per-block budget kept depleting *inside* that handler, so every
    // accumulating safepoint saw isr_depth>0, the tick froze, and the delay
    // never completed. So accumulate whenever time has elapsed. Only IRQ0
    // *delivery* stays gated on isr_depth==0 (in safe_point_impl) — a pending
    // tick set here is delivered once the ISR returns.
    if elapsed_ns > 0 {
        // The PIT counts VIRTUAL (instruction-driven) time directly;
        // emulation_speedup no longer scales here — it scales the pacing.
        pit_cycle_fraction_accum =
            pit_cycle_fraction_accum.wrapping_add(elapsed_ns.wrapping_mul(1193182u64));
        pit_cycle_accum = pit_cycle_accum.wrapping_add(pit_cycle_fraction_accum / 1000000000u64);
        pit_cycle_fraction_accum %= 1000000000u64;
        while pit_cycle_accum >= pit.reload as u64 {
            pit_cycle_accum -= pit.reload as u64;
            bios_tick_cycle_debt = bios_tick_cycle_debt.wrapping_add(pit.reload as u64);
            if bios_tick_cycle_debt >= 65536 {
                bios_tick_cycle_debt -= 65536;
                bios_timer_increment();
                bios_timer_tick_backlog = 1;
            }
            if irq0_pending == 0 {
                irq0_pending = 1;
                // Frame present / save-manager poll are the only side effects
                // that must NOT run re-entrantly inside an ISR; gate just them
                // on isr_depth==0. The clock itself keeps advancing above.
                if isr_depth == 0 && now_ns - last_present_time_ns >= 16000000u64 {
                    last_present_time_ns = now_ns;
                    if headless_mode == 0 {
                        // Top the audio queue up FIRST. Presenting a frame costs
                        // real time — converting VRAM, uploading the texture — and
                        // costs no *virtual* time at all, so not one sample is
                        // produced while it happens. Anything already owed must be
                        // in the queue before we go and spend that time, or the
                        // device plays the gap.
                        crate::audio::catchup();
                        stage_and_present_current_buffer();
                    } else {
                        save_manager_poll_pending();
                    }
                }
            }
        }
        if irq0_pending == 0 && bios_timer_tick_backlog > 0 {
            irq0_pending = 1;
        }
    }
    now_ns
}

/// Budget-gated safepoint for the dispatcher hot paths: the per-block polls
/// and the transfer-cost debits already bound IRQ latency, so a dispatcher
/// hop only takes the slow path when the budget is actually spent. (The
/// debits guarantee a resolver ping-pong still drains the budget and polls.)
#[inline(always)]
unsafe fn maybe_safe_point(file: *const c_char, func: *const c_char, line: c_int) {
    if jit_instr_budget <= 0 {
        safe_point_impl(file, func, line);
    }
}

// ============================================================================
// The in-game overlay
//
// The player presses F12 and the game stops dead while a menu is up. Nothing new
// is needed to stop it: this blocks inside the safepoint, exactly as
// `retire_splash` already blocks the machine loop to run the logo out. Virtual
// time is instruction-driven, so it simply does not advance while the guest is
// not running, and the pacer re-anchors rather than fast-forwarding after a stall
// (PACING_FORGIVE_NS) — so a menu left open for a minute costs the guest no time
// at all and delivers it no backlog of timer interrupts. The virtual clock is
// halted on top of that, so even the paths that advance time off the host clock
// (`shim_idle_wait`) stay still.
//
// The one thing that is NOT free is *where* we stop. A snapshot is only valid at
// a dispatcher resting point — `can_save_now()` wants zero lcall/isr depth and an
// `ip` the dispatcher can re-enter, and restore refuses a bundle without them. So
// arming the overlay does not stop the guest on the spot: it lets it run on to
// the next resting point and stops it there. A game's main loop passes through
// these constantly, so it is microseconds of guest time and reads as instant, and
// Save is then always available rather than mysteriously greyed out. If a game is
// wedged somewhere that never rests, we open anyway after a bounded wait, with
// Save off — a player must always be able to reach the menu and walk away.
// ============================================================================

/// The overlay the host draws. Told whether a save is possible at the point we
/// stopped; returns when the player is done with it.
pub type OverlayFn = Option<unsafe extern "C" fn(can_save: bool)>;

static mut OVERLAY_ENTRY: OverlayFn = None;
static mut OVERLAY_REQUESTED: bool = false;
static mut OVERLAY_ARMED_AT_NS: u64 = 0;

/// How long we let the guest run looking for a savable resting point before
/// giving up and opening the menu anyway. In *virtual* time, so a slow host
/// cannot turn it into a longer wait for the player.
const OVERLAY_SAVEPOINT_WAIT_NS: u64 = 250_000_000;

/// Install the player's overlay. With none installed, F12 does nothing.
#[no_mangle]
pub unsafe extern "C" fn saisei_set_overlay_entry(f: OverlayFn) {
    OVERLAY_ENTRY = f;
}

/// Ask for the overlay. Called from the SDL key handler on F12; the menu opens at
/// the next resting point, not here.
#[no_mangle]
pub extern "C" fn saisei_request_overlay() {
    unsafe {
        if core::ptr::addr_of!(OVERLAY_ENTRY).read().is_none() || OVERLAY_REQUESTED {
            return;
        }
        OVERLAY_REQUESTED = true;
        OVERLAY_ARMED_AT_NS = shim_virtual_now_ns();
    }
}

unsafe fn maybe_enter_overlay(now_ns: u64) {
    use crate::save_manager::save_manager_can_save_now;
    use crate::sdl::{
        saisei_ui_begin, saisei_ui_release, virtual_display_release_keys, virtual_display_repaint,
    };

    let savable = save_manager_can_save_now() != 0;
    if !savable && now_ns.saturating_sub(OVERLAY_ARMED_AT_NS) < OVERLAY_SAVEPOINT_WAIT_NS {
        return; // keep running; stop at the next point a save would be valid
    }
    let Some(overlay) = core::ptr::addr_of!(OVERLAY_ENTRY).read() else {
        OVERLAY_REQUESTED = false;
        return;
    };
    OVERLAY_REQUESTED = false;

    // The guest is frozen mid-keystroke as far as it knows. Let go of anything the
    // player was holding, or it will still be held when we resume — and F12 itself
    // must never reach the game.
    virtual_display_release_keys();
    // Hand the window to the menu *before* it sees a single event: SDL rewrites
    // mouse coordinates into the renderer's logical space, and the guest's is not
    // the one the menu is laid out in.
    saisei_ui_begin();
    // Fade the sound out into the queue before the clock stops. Virtual time is
    // instruction-driven, so it simply stops here — and an abrupt stop mid-
    // waveform is a click. Once the queue drains, SDL plays silence on its own;
    // there is no device to pause and no pause machinery to get wrong.
    crate::audio::shim_audio_suspend();
    vclock_halt();
    overlay(savable);
    vclock_resume();
    crate::audio::shim_audio_resume();
    // Hand the window back: drop the menu's texture and put the game's own frame
    // on screen now, rather than leaving the menu up until the game next presents.
    saisei_ui_release();
    virtual_display_repaint();
}

// Host-clock probe cache for the safepoint slow path: a fresh clock_gettime
// (and a pacing check) only when virtual time moved ≥50µs since the last
// probe. Rep-block debits drain the budget in large bites, so bursts of
// slow-path visits land within the same virtual instant; re-reading the host
// clock for each adds nothing (pacing slack is 200µs — 50µs of extra drift is
// invisible) but costs a vdso call per visit.
static mut last_host_probe_virtual_ns: u64 = 0;
static mut cached_host_now_ns: u64 = 0;
const HOST_PROBE_MIN_VIRTUAL_NS: u64 = 50_000;

#[no_mangle]
pub unsafe extern "C" fn safe_point_impl(_file: *const c_char, func: *const c_char, line: c_int) {
    perf_sp_visits += 1;
    let now_ns = shim_time_sync();
    jit_instr_budget = JIT_BUDGET_QUANTUM;
    jit_budget_last_refill = JIT_BUDGET_QUANTUM;

    // Render what the guest has just earned BEFORE the pacer puts it to sleep.
    //
    // This ordering is the whole game. Audio is produced out of *virtual* time,
    // and the pacer's sleep is real time in which virtual time does not move — so
    // a sleep produces no audio, it only drains the queue. Rendering after the
    // sleep (which is where this used to sit) meant every safepoint topped the
    // queue up and then immediately let the sleep eat into it, with nothing to
    // give back. Rendering before it means we go into the sleep with the queue as
    // full as it will ever be. A game holding a note touches no port, so this is
    // also the only thing feeding the mixer between register writes; it is
    // rate-limited internally to ~1ms of virtual time.
    if headless_mode == 0 {
        crate::audio::service();
    }

    let host_now_ns =
        if now_ns.wrapping_sub(last_host_probe_virtual_ns) >= HOST_PROBE_MIN_VIRTUAL_NS {
            last_host_probe_virtual_ns = now_ns;
            cached_host_now_ns = shim_host_monotonic_ns();
            pacing_service(now_ns, cached_host_now_ns);
            cached_host_now_ns
        } else {
            cached_host_now_ns
        };

    if headless_mode == 0 {
        virtual_display_poll_input();
        if OVERLAY_REQUESTED {
            maybe_enter_overlay(now_ns);
        }
    }
    if isr_depth == 0
        && headless_mode != 0
        && SCREENSHOT_INTERVAL_SECS > 0
        && now_ns - last_screenshot_time_ns >= SCREENSHOT_INTERVAL_SECS as u64 * 1000000000u64
    {
        last_screenshot_time_ns = now_ns;
        shim_save_video_memory();
    }

    if interrupt_shadow != 0 {
        // The shadow suppresses recognition for ONE instruction — not for a
        // whole fresh quantum. Consuming it here and returning on a refilled
        // budget means the next recognition point is up to a quantum away, and a
        // guest that STIs more often than that (a `cli`..`sti` loop) re-arms the
        // shadow before every safepoint and starves the interrupt forever. Arm
        // the next block head instead.
        interrupt_shadow = 0;
        if irq0_pending != 0 {
            perf_irq0_blk_shadow += 1;
        }
        shim_irq_recheck();
        return;
    }

    // Poll the control FIFO at least every 5ms of host time (the slow path
    // runs per budget quantum now, not per instruction, so a call-count gate
    // would be far too coarse).
    let force_stdin_poll = host_now_ns.saturating_sub(last_stdin_poll_host_ns) >= 5_000_000;
    if isr_depth == 0
        && keyboard_input_enabled != 0
        && (irq0_pending != 0 || vclock_state != VCLOCK_RUNNING || force_stdin_poll)
    {
        last_stdin_poll_host_ns = host_now_ns;
        let mut c: u8 = 0;
        loop {
            let r = session_logged_read(&mut c as *mut u8 as *mut c_void, 1);
            if r == 1 {
                // fall through to byte-processing below
            } else if r == 0 {
                let newfd = libc::open(cstr!("/proc/self/fd/0"), libc::O_RDONLY | libc::O_NONBLOCK);
                if newfd >= 0 && newfd != keyboard_fd {
                    libc::dup2(newfd, keyboard_fd);
                    libc::close(newfd);
                }
                break;
            } else {
                break;
            }
            if c == 0x14 {
                shim_save_video_memory();
                continue;
            }
            if c == 0x19 {
                save_manager_request_save();
                continue;
            }
            if c == 0x15 {
                vclock_halt();
                continue;
            }
            if c == 0x16 {
                vclock_resume();
                continue;
            }
            if c == 0x17 {
                let mut buf = [0u8; 2];
                let mut got: usize = 0;
                let mut tries = 0;
                while got < 2 && tries < 1000 {
                    let n2 = session_logged_read(buf.as_mut_ptr().add(got) as *mut c_void, 2 - got);
                    if n2 > 0 {
                        got += n2 as usize;
                    }
                    tries += 1;
                }
                if got == 2 {
                    let ticks: u16 = buf[0] as u16 | ((buf[1] as u16) << 8);
                    vclock_step(ticks as u32);
                } else {
                    shim_log_stdout(cstr!("[VCLOCK] step short read got=%zu\n"), got);
                }
                continue;
            }
            if c == 0x18 {
                let mut buf = [0u8; 5];
                let mut got: usize = 0;
                let mut tries = 0;
                while got < 5 && tries < 1000 {
                    let n2 = session_logged_read(buf.as_mut_ptr().add(got) as *mut c_void, 5 - got);
                    if n2 > 0 {
                        got += n2 as usize;
                    }
                    tries += 1;
                }
                if got == 5 {
                    let addr: u32 = buf[0] as u32
                        | ((buf[1] as u32) << 8)
                        | ((buf[2] as u32) << 16)
                        | ((buf[3] as u32) << 24);
                    let len = buf[4];
                    shim_read_memory_to_sidecar(addr, len);
                } else {
                    shim_log_stdout(cstr!("[READ] short read got=%zu\n"), got);
                }
                continue;
            }
            if c == 0x1D {
                let mut buf = [0u8; 8];
                let mut got: usize = 0;
                let mut tries = 0;
                while got < 8 && tries < 1000 {
                    let n2 = session_logged_read(buf.as_mut_ptr().add(got) as *mut c_void, 8 - got);
                    if n2 > 0 {
                        got += n2 as usize;
                    }
                    tries += 1;
                }
                if got == 8 {
                    let mut vns: u64 = 0;
                    for i in 0..8 {
                        vns |= (buf[i] as u64) << (8 * i);
                    }
                    vclock_frozen_virtual_ns = vns;
                    vclock_state = VCLOCK_HALTED;
                    shim_log_stdout(
                        cstr!("[VCLOCK] set_virtual_clock vns=%llu\n"),
                        vns as c_ulonglong,
                    );
                } else {
                    shim_log_stdout(cstr!("[VCLOCK] set_vc short read got=%zu\n"), got);
                }
                continue;
            }
            if c == 0x1A {
                shim_dump_ram_snapshot();
                continue;
            }
            if c == 0x1E {
                shim_perf_report(cstr!("fifo"));
                continue;
            }
            if c == 0x10 || c == 0x11 {
                let kind = c;
                let mut sc: u8 = 0;
                let n2 = session_logged_read(&mut sc as *mut u8 as *mut c_void, 1);
                if n2 == 1 && sc != 0 {
                    // Bit 7 of the scancode byte selects the extended (grey,
                    // 0xE0-prefixed) variant of the 7-bit code — e.g. 0xC8 =
                    // grey Up, 0x48 = keypad-8.
                    let ext = sc & 0x80 != 0;
                    let sc7 = sc & 0x7F;
                    if kind == 0x10 {
                        if ext {
                            shim_keyboard_enqueue_scancode_press_ext(sc7);
                        } else {
                            shim_keyboard_enqueue_scancode_press(sc7);
                        }
                    } else if ext {
                        shim_keyboard_enqueue_scancode_release_ext(sc7);
                    } else {
                        shim_keyboard_enqueue_scancode_release(sc7);
                    }
                }
                continue;
            }
            if c == 0x12 {
                let mut buf = [0u8; 3];
                let mut got: usize = 0;
                let mut tries = 0;
                while got < 3 && tries < 1000 {
                    let n2 = session_logged_read(buf.as_mut_ptr().add(got) as *mut c_void, 3 - got);
                    if n2 > 0 {
                        got += n2 as usize;
                    }
                    tries += 1;
                }
                if got == 3 && buf[0] != 0 {
                    // Bit 7 selects the extended (grey, 0xE0-prefixed) variant;
                    // the deadline slot keeps the bit so the scheduled release
                    // emits the matching E0-prefixed break.
                    let ext = buf[0] & 0x80 != 0;
                    let sc = buf[0] & 0x7F;
                    let slot = buf[0] as usize;
                    let mut ticks: u16 = buf[1] as u16 | ((buf[2] as u16) << 8);
                    if ticks == 0 {
                        ticks = 1;
                    }
                    if ext {
                        shim_keyboard_enqueue_scancode_press_ext(sc);
                    } else {
                        shim_keyboard_enqueue_scancode_press(sc);
                    }
                    // Deadline in VIRTUAL ns: a BIOS tick is 54.925ms of game
                    // time; emulation_speedup is a pacing concern, not a
                    // game-time one.
                    let ns_per_tick: u64 = NS_PER_BIOS_TICK;
                    let now_v = shim_virtual_now_ns();
                    pending_release_deadline_ns[slot] =
                        now_v.wrapping_add((ticks as u64).wrapping_mul(ns_per_tick));
                    shim_log_stdout(
                        cstr!("[TAP] sc=0x%02X ext=%d ticks=%u virtual_ns=%llu deadline=%llu\n"),
                        sc as c_uint,
                        ext as c_int,
                        ticks as c_uint,
                        now_v as c_ulonglong,
                        pending_release_deadline_ns[slot] as c_ulonglong,
                    );
                } else {
                    shim_log_stdout(
                        cstr!("[TAP] short read got=%zu buf=%02X%02X%02X\n"),
                        got,
                        buf[0] as c_uint,
                        buf[1] as c_uint,
                        buf[2] as c_uint,
                    );
                }
                continue;
            }
            if c == 0x13 {
                let mut mb = [0u8; 5];
                let mut got: usize = 0;
                let mut tries = 0;
                while got < 5 && tries < 1000 {
                    let n2 = session_logged_read(mb.as_mut_ptr().add(got) as *mut c_void, 5 - got);
                    if n2 > 0 {
                        got += n2 as usize;
                    }
                    tries += 1;
                }
                if got == 5 {
                    let mx: i16 = (mb[0] as u16 | ((mb[1] as u16) << 8)) as i16;
                    let my: i16 = (mb[2] as u16 | ((mb[3] as u16) << 8)) as i16;
                    mouse_host_inject(mx, my, mb[4] as u16);
                    shim_log_stdout(
                        cstr!("[MOUSE] inject x=%d y=%d buttons=0x%02X\n"),
                        mx as c_int,
                        my as c_int,
                        mb[4] as c_uint,
                    );
                }
                continue;
            }
            let mut ascii: u8 = 0;
            let scancode: u8;
            let mut extended = false;
            if c == 0x1B {
                let mut seq = [0u8; 2];
                let n2 = session_logged_read(seq.as_mut_ptr() as *mut c_void, 2);
                if n2 == 2 && seq[0] == b'[' {
                    // Terminal cursor sequences are the dedicated (grey) arrow
                    // keys: emit the faithful 0xE0-prefixed make/break.
                    match seq[1] {
                        b'A' => {
                            scancode = 0x48;
                            extended = true;
                        }
                        b'B' => {
                            scancode = 0x50;
                            extended = true;
                        }
                        b'C' => {
                            scancode = 0x4D;
                            extended = true;
                        }
                        b'D' => {
                            scancode = 0x4B;
                            extended = true;
                        }
                        _ => {
                            ascii = 0x1B;
                            scancode = 0x01;
                        }
                    }
                } else {
                    ascii = 0x1B;
                    scancode = 0x01;
                }
            } else {
                ascii = if c == b'\n' { b'\r' } else { c };
                scancode = ascii_to_scan(ascii);
            }
            if extended {
                shim_keyboard_enqueue_scancode_press_ext(scancode);
                shim_keyboard_enqueue_scancode_release_ext(scancode);
            } else {
                kbd_enqueue(ascii, scancode);
                if scancode != 0 {
                    kbd_queue_push(0, scancode | 0x80);
                }
            }
        }
    }

    // Pick the highest-priority request the 8259 would actually let through.
    // The timer is IRQ0 — the HIGHEST priority line — so it is considered first;
    // the old scan skipped 0x08 and preferred anything else, which inverted the
    // priority of the timer against the keyboard.
    let timer_int = pic_vector_base;
    let mut pending_int: u8 = 0xFF;
    let mut source: *const c_char = cstr!("<timer>");
    if irq0_pending != 0 && pic_can_deliver(0) {
        pending_int = timer_int;
    }
    // Skip the 256-slot scan entirely when nothing is scheduled (Fix 1): this
    // ran on every emulated instruction and was the dominant per-instruction cost.
    if pending_int == 0xFF && irq_pending_count > 0 {
        for i in 0..256 {
            if i == timer_int as usize || irq_pending[i] == 0 {
                continue;
            }
            // A hardware line only reaches the CPU if the PIC lets it: not
            // masked, and no equal-or-higher line still in service. Vectors that
            // are not PIC lines at all are software-scheduled and pass straight
            // through.
            if let Some(irq) = pic_irq_of_int(i as u8) {
                if !pic_can_deliver(irq) {
                    continue;
                }
            }
            pending_int = i as u8;
            source = cstr!("<interrupt>");
            break;
        }
    }
    // Delivery gate. NOTE there is no `isr_depth` term: being inside a handler
    // is not what the hardware gates on — a SOFTWARE interrupt (INT 21h/61h)
    // puts nothing in service on the 8259, and a hardware handler is protected
    // from its own line by IF=0 on entry plus its in-service bit until it EOIs.
    // Nested interrupts are legal and routine, and `isr_depth` blocking them was
    // what starved Zeliard's timer inside its INT 61h service.
    let gate_open = IF() != 0 && critical_depth == 0;
    if irq0_pending != 0 {
        // Exhaustive partition of the veto — exactly one bucket per skipped
        // safepoint, so a freeze sample names the culprit instead of guessing.
        if IF() == 0 {
            perf_irq0_blk_if += 1;
        } else if critical_depth != 0 {
            perf_irq0_blk_crit += 1;
        } else if !pic_can_deliver(0) {
            perf_irq0_blk_pic += 1;
        } else if pending_int != timer_int {
            perf_irq0_blk_other += 1;
        }
    }
    if gate_open && pending_int != 0xFF {
        // The INTA cycle: drop the request latch and put the line in service.
        if pending_int == timer_int {
            irq0_pending = 0;
            perf_irq0_delivered += 1;
            pending_release_tick();
            bios_timer_tick_preincremented = 1;
            if bios_timer_tick_backlog > 0 {
                bios_timer_tick_backlog -= 1;
            }
        } else {
            if irq_pending[pending_int as usize] != 0 {
                irq_pending_count = irq_pending_count.saturating_sub(1);
            }
            irq_pending[pending_int as usize] = 0;
            bios_timer_tick_preincremented = 0;
        }
        pic_ack(pending_int);
        let preserve_regs = 1;
        invoke_isr(
            pending_int,
            preserve_regs,
            preserve_regs,
            preserve_regs,
            ip(),
            source,
            func,
            line,
        );
        bios_timer_tick_preincremented = 0;
    }
    if IF() != 0 && isr_depth == 0 && critical_depth == 0 {
        mouse_deliver_pending_events();
    }
}

// ============================================================================
// Memory-offset helpers  [C lines 2098-2126]
// ============================================================================

unsafe fn try_memory_offset(addr: *const c_void, offset: *mut u32) -> bool {
    if virtual_memory.is_null() || addr.is_null() {
        return false;
    }
    let p = addr as *const u8;
    if p < virtual_memory || p >= virtual_memory.add(MEMORY_SIZE) {
        return false;
    }
    *offset = p.offset_from(virtual_memory) as u32;
    true
}

unsafe fn try_memory_range(addr: *const c_void, len: usize, base: *mut u32, end: *mut u32) -> bool {
    let mut offset: u32 = 0;
    if !try_memory_offset(addr, &mut offset) {
        return false;
    }
    if !base.is_null() {
        *base = offset;
    }
    if !end.is_null() {
        *end = offset + len as u32;
    }
    true
}

// ============================================================================
// File-mapping table management  [C lines 2146-2410]
// ============================================================================

unsafe fn evict_or_shrink_for_load(new_base: u32, new_len: usize) {
    if new_len == 0 {
        return;
    }
    let new_end = new_base + new_len as u32;
    let mut out: usize = 0;
    let mut splits_to_append: usize = 0;
    const MAX_SPLITS_PER_LOAD: usize = 8;
    let mut splits = [FileMapping::ZERO; MAX_SPLITS_PER_LOAD];
    for i in 0..file_mapping_count {
        let mut e = file_mappings[i];
        let e_end = e.base + e.len as u32;
        if e_end <= new_base || e.base >= new_end {
            file_mappings[out] = e;
            out += 1;
            continue;
        }
        if e.base >= new_base && e_end <= new_end {
            libc::free(e.path as *mut c_void);
            libc::free(e.data as *mut c_void);
            continue;
        }
        if e.base < new_base && e_end > new_end {
            if splits_to_append >= MAX_SPLITS_PER_LOAD {
                shim_log_crash(
                    cstr!("[BUG] evict_or_shrink_for_load: too many splits in one LOAD at 0x%05X len 0x%zX — raise MAX_SPLITS_PER_LOAD or audit chunk boundaries upstream\n"),
                    new_base as c_uint,
                    new_len,
                );
                shim_flush_all_streams();
                libc::abort();
            }
            let mut right = e;
            right.path = libc::strdup(e.path);
            right.base = new_end;
            right.len = (e_end - new_end) as usize;
            right.file_offset = e.file_offset + (new_end - e.base) as usize;
            right.data = ptr::null_mut();
            splits[splits_to_append] = right;
            splits_to_append += 1;
            e.len = (new_base - e.base) as usize;
            libc::free(e.data as *mut c_void);
            e.data = ptr::null_mut();
            file_mappings[out] = e;
            out += 1;
            continue;
        }
        if e.base >= new_base {
            let advance = (new_end - e.base) as usize;
            e.base = new_end;
            e.file_offset += advance;
            e.len -= advance;
            libc::free(e.data as *mut c_void);
            e.data = ptr::null_mut();
            file_mappings[out] = e;
            out += 1;
            continue;
        }
        e.len = (new_base - e.base) as usize;
        libc::free(e.data as *mut c_void);
        e.data = ptr::null_mut();
        file_mappings[out] = e;
        out += 1;
    }
    file_mapping_count = out;
    for i in 0..splits_to_append {
        if file_mapping_count >= MAX_FILE_MAPPINGS {
            shim_log_crash(cstr!("[BUG] evict_or_shrink_for_load: file_mappings full while appending split right-piece — raise MAX_FILE_MAPPINGS\n"));
            shim_flush_all_streams();
            libc::abort();
        }
        file_mappings[file_mapping_count] = splits[i];
        file_mapping_count += 1;
        mem_page_flags_recompute();
    }
}

unsafe fn register_file_mapping(
    path: *const c_char,
    file_offset: usize,
    addr: *const c_void,
    len: usize,
) {
    let p = addr as *const u8;
    if p < virtual_memory || p >= virtual_memory.add(MEMORY_SIZE) {
        shim_log_stdout(
            cstr!("Trace: register_file_mapping: ignoring %s at %p (outside virtual memory)\n"),
            path,
            addr,
        );
        return;
    }
    let base = p.offset_from(virtual_memory) as u32;
    evict_or_shrink_for_load(base, len);
    if file_mapping_count < MAX_FILE_MAPPINGS {
        let m = &mut file_mappings[file_mapping_count];
        m.path = libc::strdup(path);
        m.base = base;
        m.len = len;
        m.file_offset = file_offset;
        m.loader_cs = cs();
        m.loader_ip = ip();
        m.loader_ss = ss();
        m.loader_sp = sp();
        for i in 0..8 {
            let off: u16 = (sp().wrapping_add(2 * i as u16)) & 0xFFFF;
            m.loader_stack[i] = memw_raw_read(ss(), off);
        }
        m.data = libc::malloc(len) as *mut u8;
        if !m.data.is_null() {
            libc::memcpy(m.data as *mut c_void, addr, len);
        }
        shim_log_stdout(
            cstr!("Trace: register_file_mapping[%zu]: %s mapped at 0x%05X-0x%05X (file offset 0x%zX)\n"),
            file_mapping_count,
            path,
            base as c_uint,
            (base + len as u32) as c_uint,
            file_offset,
        );
        {
            let bn0 = libc::strrchr(path, b'/' as c_int);
            let bn = if !bn0.is_null() { bn0.add(1) } else { path };
            lifecycle_log(
                cstr!("LOAD %s+0x%zX @ 0x%05X-0x%05X (len 0x%zX) from cs:ip=%04X:%04X\n"),
                bn,
                file_offset,
                base as c_uint,
                (base + len as u32) as c_uint,
                len,
                cs() as c_uint,
                ip() as c_uint,
            );
        }
        file_mapping_count += 1;
        mem_page_flags_recompute();
    } else {
        libc::printf(cstr!(
            "Error: register_file_mapping: too many file mappings\n"
        ));
        shim_flush_all_streams();
        libc::exit(1);
    }
}

unsafe fn init_psp() {
    dos_set_current_psp_to_load();
    libc::memset(psp as *mut c_void, 0, core::mem::size_of::<PSP>());
    (*psp).raw[0] = 0xCD;
    (*psp).raw[1] = 0x20;
    memw_write_impl(
        psp_seg,
        0x02,
        CONVENTIONAL_TOP_SEG,
        SHIMS_FILE,
        cstr!("init_psp"),
        2293,
    );

    for i in 0..MAX_DOS_HANDLES {
        memb_write_impl(
            psp_seg,
            0x18 + i as u16,
            if i < 5 { i as u8 } else { 0xFF },
            SHIMS_FILE,
            cstr!("init_psp"),
            2296,
        );
    }

    let env_seg = psp_seg.wrapping_sub(0x10);
    env_block = seg_off(env_seg, 0);
    libc::memset(env_block as *mut c_void, 0, 0x100);
    let mut program_path = cfg().program_path;
    if program_path.is_null() {
        program_path = cstr!("program.exe");
    }
    let mut dos_path = [0u8; 128];
    let mut written = libc::snprintf(
        dos_path.as_mut_ptr() as *mut c_char,
        dos_path.len(),
        cstr!("C:\\%s"),
        program_path,
    );
    if written < 0 {
        dos_path[0] = 0;
        written = 0;
    }
    let mut path_len = written as usize;
    if path_len >= 0x100 - 2 {
        path_len = 0x100 - 5;
    }
    static mut env_vars: [*const c_char; 3] = [
        cstr!("COMSPEC=C:\\COMMAND.COM"),
        cstr!("PATH=C:\\"),
        cstr!("PROMPT=$p$g"),
    ];
    let mut off: usize = 0;
    for i in 0..(*ptr::addr_of!(env_vars)).len() {
        let n = libc::strlen(env_vars[i]);
        libc::memcpy(
            env_block.add(off) as *mut c_void,
            env_vars[i] as *const c_void,
            n,
        );
        off += n;
        *env_block.add(off) = 0;
        off += 1;
    }
    *env_block.add(off) = 0;
    off += 1;
    *env_block.add(off) = 0x01;
    off += 1;
    *env_block.add(off) = 0x00;
    off += 1;
    if off + path_len + 1 > 0x100 {
        path_len = 0x100 - off - 1;
    }
    libc::memcpy(
        env_block.add(off) as *mut c_void,
        dos_path.as_ptr() as *const c_void,
        path_len,
    );
    *env_block.add(off + path_len) = 0;
    memw_write_impl(psp_seg, 0x2C, env_seg, SHIMS_FILE, cstr!("init_psp"), 2343);
    dta_ptr = seg_off(psp_seg, 0x80) as *mut c_void;

    memb_write_impl(psp_seg, 0x80, 0, SHIMS_FILE, cstr!("init_psp"), 2350);
    memb_write_impl(psp_seg, 0x81, 0x0D, SHIMS_FILE, cstr!("init_psp"), 2351);
    libc::memset(seg_off(psp_seg, 0x5C) as *mut c_void, 0, 0x10);
    libc::memset(seg_off(psp_seg, 0x6C) as *mut c_void, 0, 0x10);
}

unsafe fn init_bios_data_area() {
    let bda = seg_off(0x40, 0);
    libc::memset(bda as *mut c_void, 0, 0x100);
    memw_raw_write(0x40, 0x0010, BIOS_EQUIPMENT_WORD);
    // Keyboard shift-flag byte: real AT BIOSes boot with NumLock ON (bit 5).
    // The extended-key senders consult this to emit the authentic fake-shift
    // framing (E0 2A / E0 AA) around grey-cluster make/break — several games
    // (e.g. DM's IBMIO) key their cursor handling on that exact byte stream.
    *seg_off(0x40, 0x17) = 0x20;
    *seg_off(0x40, 0x49) = bios_video.video_mode;
    *seg_off(0x40, 0x4A) = 80;
    memw_raw_write(0x40, 0x4C, 0x0FA0);
    memw_raw_write(0x40, 0x63, 0x3D4);
    memw_raw_write(0x40, 0x6C, 0);
    memw_raw_write(0x40, 0x6E, 0);
    *seg_off(0x40, 0x66) = 0;
    *seg_off(0x40, 0x62) = 0;
    bios_video.cga_palette_select = 0;
    bios_video.cga_border_color = 0;
    video_invalidate_palette_cache();
    for page in 0..8 {
        bios_video.cursor_row[page] = 0;
        bios_video.cursor_col[page] = 0;
        bios_video.cursor_attr[page] = 0x07;
        memw_raw_write(0x40, (0x50 + page * 2) as u16, 0);
    }
    bios_video.active_page = 0;
    for i in 0..bios_video_parameter_table_mode6.len() {
        *seg_off(BIOS_VIDEO_PARAM_SEG, BIOS_VIDEO_PARAM_OFF + i as u16) =
            bios_video_parameter_table_mode6[i];
    }
}

unsafe fn find_file_mapping(addr: u32) -> *const FileMapping {
    let mut i = file_mapping_count as isize - 1;
    while i >= 0 {
        let base = file_mappings[i as usize].base;
        if addr >= base && addr < base + file_mappings[i as usize].len as u32 {
            return &file_mappings[i as usize];
        }
        i -= 1;
    }
    if is_builtin_call_target(addr) != 0 {
        return ptr::null();
    }
    shim_log_stdout(
        cstr!("Trace: find_file_mapping: address 0x%05X not mapped\n"),
        addr as c_uint,
    );
    ptr::null()
}

unsafe fn find_file_mapping_mut(addr: u32) -> *mut FileMapping {
    let mut i = file_mapping_count as isize - 1;
    while i >= 0 {
        let base = file_mappings[i as usize].base;
        if addr >= base && addr < base + file_mappings[i as usize].len as u32 {
            return &mut file_mappings[i as usize];
        }
        i -= 1;
    }
    ptr::null_mut()
}

/// `linear_addr` matching shims.h's static inline (u32, a20-masked).
#[inline]
unsafe fn linear_addr(seg: u16, off: u16) -> u32 {
    let addr = ((seg as u32) << 4) + off as u32;
    if a20_enabled {
        addr & 0x1FFFFF
    } else {
        addr & 0xFFFFF
    }
}

// ============================================================================
// Unhandled-pc report + dispatch cs bookkeeping + region swaps  [2422-2751]
// ============================================================================

#[no_mangle]
pub unsafe extern "C" fn shim_unhandled_pc_report(
    module: *const c_char,
    pc: c_int,
    out: *mut c_char,
    cap: usize,
) -> c_int {
    if out.is_null() || cap == 0 {
        return 0;
    }
    let mut n: usize = 0;

    macro_rules! unhpc_append {
        ($($arg:tt)*) => {{
            if n < cap {
                let _w = libc::snprintf(out.add(n), cap - n, $($arg)*);
                if _w > 0 {
                    n += _w as usize;
                }
            }
        }};
    }

    let linear: u32 = ((cs() as u32) << 4) + ((ip() as u32) & 0xFFFF);

    unhpc_append!(
        cstr!("[BUG] Unhandled pc=%X in %s_dispatch\n"),
        pc as c_uint,
        module
    );
    unhpc_append!(
        cstr!("[BUG]   cs:ip=%04X:%04X  linear=0x%05X  active_binary=%s\n"),
        cs() as c_uint,
        ip() as c_uint,
        linear as c_uint,
        if shim_active_binary().is_null() {
            cstr!("<none>")
        } else {
            shim_active_binary()
        }
    );

    let picked = find_file_mapping(linear);
    if !picked.is_null() && !(*picked).path.is_null() {
        let pn0 = libc::strrchr((*picked).path, b'/' as c_int);
        let pn = if !pn0.is_null() {
            pn0.add(1)
        } else {
            (*picked).path
        };
        let target_file_off = (*picked).file_offset + (linear - (*picked).base) as usize;
        unhpc_append!(
            cstr!("[BUG]   primary mapping: %s base=0x%05X len=0x%zX chunk_file_off=0x%zX -> target file_off=0x%zX canonical_cs=0x%04X\n"),
            pn,
            (*picked).base as c_uint,
            (*picked).len,
            (*picked).file_offset,
            target_file_off,
            (*picked).canonical_cs as c_uint
        );
    } else {
        unhpc_append!(
            cstr!("[BUG]   primary mapping: NONE - linear 0x%05X is unmapped\n"),
            linear as c_uint
        );
    }

    let mut overlap_count: c_int = 0;
    for i in 0..file_mapping_count {
        let m = &file_mappings[i] as *const FileMapping;
        if m == picked {
            continue;
        }
        if linear < (*m).base || linear >= (*m).base + (*m).len as u32 {
            continue;
        }
        if overlap_count == 0 {
            unhpc_append!(cstr!(
                "[BUG]   overlapping mappings at same linear (chunk-swap candidates):\n"
            ));
        }
        overlap_count += 1;
        let pn0 = libc::strrchr((*m).path, b'/' as c_int);
        let pn = if !pn0.is_null() {
            pn0.add(1)
        } else {
            (*m).path
        };
        let alt_file_off = (*m).file_offset + (linear - (*m).base) as usize;
        unhpc_append!(
            cstr!("[BUG]     [%3zu] %s base=0x%05X len=0x%zX chunk_file_off=0x%zX -> ALT target file_off=0x%zX canonical_cs=0x%04X\n"),
            i,
            pn,
            (*m).base as c_uint,
            (*m).len,
            (*m).file_offset,
            alt_file_off,
            (*m).canonical_cs as c_uint
        );
    }

    if overlap_count > 0 {
        unhpc_append!(
            cstr!("[BUG]   diagnosis: %d overlapping mapping(s) - likely chunk-swap stale target. The runtime computed this address while one chunk was loaded at base 0x%05X, but a different chunk is loaded there now; the SAME stored ret/jump value resolves to a different file_offset in the currently-active chunk.\n"),
            overlap_count,
            if !picked.is_null() { (*picked).base } else { 0 } as c_uint
        );
    } else if !picked.is_null() {
        unhpc_append!(
            cstr!("[BUG]   diagnosis: pc=0x%04X is inside %s but no dispatch case matches it. Either the disassembler missed a basic-block boundary at this offset, or an upstream computation produced a target that's mid-block.\n"),
            pc as c_uint,
            module
        );
    }

    unhpc_append!(cstr!("[BUG]   diagnosis: with literal-emission translation, the dispatch case set covers every legitimate branch/call target. Landing here means either (a) a same-binary RET popped a mid-instruction IP (stack corruption upstream of the pop), or (b) a cross-binary RET that find_file_mapping couldn't route (overlay-swap timing or unmapped target). Trace lifecycle.log/trace.tail.log backward from this crash to identify the corrupting push or unmapped chunk.\n"));

    if overlap_count > 0 {
        unhpc_append!(
            cstr!("[BUG]   chunk-swap suspect: lifecycle.log LOAD events for base 0x%05X show which chunk was active when the target was computed. The fix is in the loader/chunk-attribution layer.\n"),
            if !picked.is_null() { (*picked).base } else { 0 } as c_uint
        );
    }

    (if n < cap { n } else { cap - 1 }) as c_int
}

unsafe fn record_binary_cs(addr: u32, seg: u16) {
    let fm = find_file_mapping_mut(addr);
    if !fm.is_null() {
        (*fm).canonical_cs = seg;
    }
}

unsafe fn set_dispatch_cs(fm: *const FileMapping, addr: u32) {
    if fm.is_null() || (*fm).canonical_cs == 0 {
        return;
    }
    let live_off = addr.wrapping_sub((cs() as u32) << 4);
    if live_off < 0x10000u32 {
        return;
    }
    set_cs((*fm).canonical_cs);
}

unsafe fn swap_file_mappings_in_regions(a_start: u32, b_start: u32, len: u32) {
    if len == 0 {
        return;
    }
    let a_end = a_start + len;
    let b_end = b_start + len;

    let cuts: [u32; 4] = [a_start, a_end, b_start, b_end];
    for ci in 0..4 {
        let cut = cuts[ci];
        let n = file_mapping_count;
        for i in 0..n {
            let base = file_mappings[i].base;
            let end = base + file_mappings[i].len as u32;
            if base < cut && cut < end {
                if file_mapping_count >= MAX_FILE_MAPPINGS {
                    shim_log_crash(
                        cstr!("[BUG] swap_file_mappings: file_mappings full while splitting a straddling mapping at boundary 0x%05X -- raise MAX_FILE_MAPPINGS\n"),
                        cut as c_uint,
                    );
                    shim_flush_all_streams();
                    libc::abort();
                }
                let e = file_mappings[i];
                let mut right = e;
                right.path = if !e.path.is_null() {
                    libc::strdup(e.path)
                } else {
                    ptr::null_mut()
                };
                right.base = cut;
                right.len = (end - cut) as usize;
                right.file_offset = e.file_offset + (cut - base) as usize;
                right.data = ptr::null_mut();
                libc::free(file_mappings[i].data as *mut c_void);
                file_mappings[i].data = ptr::null_mut();
                file_mappings[i].len = (cut - base) as usize;
                file_mappings[file_mapping_count] = right;
                file_mapping_count += 1;
                mem_page_flags_recompute();
            }
        }
    }

    let mut moved: usize = 0;
    for i in 0..file_mapping_count {
        let base = file_mappings[i].base;
        let end = base + file_mappings[i].len as u32;
        if base >= a_start && end <= a_end {
            file_mappings[i].base = base - a_start + b_start;
            moved += 1;
        } else if base >= b_start && end <= b_end {
            file_mappings[i].base = base - b_start + a_start;
            moved += 1;
        }
    }
    if moved != 0 {
        lifecycle_log(
            cstr!("MAPSWAP relocated %zu mappings between regions 0x%05X..0x%05X and 0x%05X..0x%05X (len 0x%X)\n"),
            moved,
            a_start as c_uint,
            a_end as c_uint,
            b_start as c_uint,
            b_end as c_uint,
            len as c_uint,
        );
    }
}

#[no_mangle]
pub unsafe extern "C" fn shim_swap_regions_w(
    es_seg: u16,
    di_off: u16,
    ds_seg: u16,
    si_off: u16,
    count: u16,
    df: c_int,
) {
    if count == 0 {
        return;
    }
    let es_lin = linear_addr(es_seg, di_off);
    let ds_lin = linear_addr(ds_seg, si_off);
    let bytes = count as u32 * 2;
    let es_start;
    let ds_start;
    if df == 0 {
        es_start = es_lin;
        ds_start = ds_lin;
    } else {
        es_start = (es_lin + 2 - bytes) & 0xFFFFF;
        ds_start = (ds_lin + 2 - bytes) & 0xFFFFF;
    }
    for i in 0..bytes {
        let ea = mask_addr(es_start + i);
        let da = mask_addr(ds_start + i);
        let tmp_a = *virtual_memory.add(ea as usize);
        let tmp_b = *virtual_memory.add(da as usize);
        write_watch_log(
            ea,
            1,
            tmp_b as u32,
            SHIMS_FILE,
            cstr!("shim_swap_regions_w"),
            2671,
        );
        write_watch_log(
            da,
            1,
            tmp_a as u32,
            SHIMS_FILE,
            cstr!("shim_swap_regions_w"),
            2672,
        );
        *virtual_memory.add(ea as usize) = tmp_b;
        *virtual_memory.add(da as usize) = tmp_a;
    }
    lifecycle_log(
        cstr!("SWAP_W A=0x%05X..0x%05X B=0x%05X..0x%05X bytes=0x%X df=%d\n"),
        ds_start as c_uint,
        (ds_start + bytes) as c_uint,
        es_start as c_uint,
        (es_start + bytes) as c_uint,
        bytes as c_uint,
        df,
    );
    swap_file_mappings_in_regions(ds_start, es_start, bytes);
    shim_jit_invalidate_code_range(ds_start, bytes);
    shim_jit_invalidate_code_range(es_start, bytes);
}

unsafe fn lifecycle_log_dispatch(kind: *const c_char, addr: u32) {
    let fm = find_file_mapping(addr);
    let mut bn: *const c_char = cstr!("<unmapped>");
    let mut off_in: usize = 0;
    let has_path = !fm.is_null() && !(*fm).path.is_null();
    if has_path {
        let bn0 = libc::strrchr((*fm).path, b'/' as c_int);
        bn = if !bn0.is_null() {
            bn0.add(1)
        } else {
            (*fm).path
        };
        off_in = (*fm).file_offset + (addr - (*fm).base) as usize;
    }
    let unmapped = fm.is_null();
    if isr_depth > 0 && !unmapped {
        return;
    }
    let call_like = *kind == b'C' as c_char || *kind == b'L' as c_char;
    // Callgraph accounting stays eager (a hash-probe + count on settled
    // edges). Alias self-seeding used to ride a per-transfer registry lookup;
    // a new callgraph edge fires at exactly the same first-call moments, so
    // seed only then — steady state does no registry work at all.
    let new_edge = if call_like {
        cg_record(((cs() as u32) << 4) + ip() as u32, addr)
    } else {
        false
    };
    if new_edge && has_path {
        let mut idbuf = [0u8; 160];
        libc::snprintf(
            idbuf.as_mut_ptr() as *mut c_char,
            idbuf.len(),
            cstr!("%s+0x%zX"),
            bn,
            off_in,
        );
        aliasreg_alias(idbuf.as_ptr() as *const c_char, 1);
    }
    if !lifecycle_eager() {
        // Silent run: capture a binary record; the text (identical to the
        // eager output) is produced only if the ring is ever dumped.
        let mut rec = LifecycleDispatchRec {
            t_us: lifecycle_elapsed_us(),
            kind,
            addr,
            popped: 0,
            has_path: has_path as u8,
            _pad: 0,
            off_in: off_in as u64,
            bn: [0; 20],
            regs: regsnap_now(),
        };
        libc::snprintf(
            rec.bn.as_mut_ptr() as *mut c_char,
            rec.bn.len(),
            cstr!("%s"),
            bn,
        );
        lifecycle_ring_save_rec(&rec, LC_DISPATCH);
        return;
    }
    let mut alias: *const c_char = ptr::null();
    let mut disp = [0u8; 256];
    if has_path {
        let mut idbuf = [0u8; 160];
        libc::snprintf(
            idbuf.as_mut_ptr() as *mut c_char,
            idbuf.len(),
            cstr!("%s+0x%zX"),
            bn,
            off_in,
        );
        alias = aliasreg_alias(idbuf.as_ptr() as *const c_char, call_like as c_int);
    }
    if !alias.is_null() {
        let snap = regsnap_now();
        render_alias_with_args(alias, disp.as_mut_ptr() as *mut c_char, disp.len(), &snap);
    }
    if call_like {
        if !alias.is_null() {
            shim_log_stdout(
                cstr!("Flow: %s 0x%05X -> %s (%s+0x%zX)  from=%04X:%04X\n"),
                kind,
                addr as c_uint,
                disp.as_ptr() as *const c_char,
                bn,
                off_in,
                cs() as c_uint,
                ip() as c_uint,
            );
        } else {
            shim_log_stdout(
                cstr!("Flow: %s 0x%05X -> %s+0x%zX  from=%04X:%04X\n"),
                kind,
                addr as c_uint,
                bn,
                off_in,
                cs() as c_uint,
                ip() as c_uint,
            );
        }
    }
    if !alias.is_null() {
        lifecycle_log(
            cstr!("%s 0x%05X -> %s (%s+0x%zX)  bx=%04X si=%04X ax=%04X ds=%04X cs=%04X ip=%04X\n"),
            kind,
            addr as c_uint,
            disp.as_ptr() as *const c_char,
            bn,
            off_in,
            bx() as c_uint,
            si() as c_uint,
            ax() as c_uint,
            ds() as c_uint,
            cs() as c_uint,
            ip() as c_uint,
        );
    } else {
        lifecycle_log(
            cstr!("%s 0x%05X -> %s+0x%zX  bx=%04X si=%04X ax=%04X ds=%04X cs=%04X ip=%04X\n"),
            kind,
            addr as c_uint,
            bn,
            off_in,
            bx() as c_uint,
            si() as c_uint,
            ax() as c_uint,
            ds() as c_uint,
            cs() as c_uint,
            ip() as c_uint,
        );
    }
}

// ============================================================================
// Function alias registry (reconstruction naming layer)  [C lines 2774-2954]
// ============================================================================

const ALIASREG_MAX_ENTRIES: usize = 8192;

#[repr(C)]
#[derive(Clone, Copy)]
struct AliasRegEntry {
    id: *mut c_char,
    alias: *mut c_char,
}
impl AliasRegEntry {
    const ZERO: AliasRegEntry = AliasRegEntry {
        id: ptr::null_mut(),
        alias: ptr::null_mut(),
    };
}
static mut aliasreg_entries: [AliasRegEntry; ALIASREG_MAX_ENTRIES] =
    [AliasRegEntry::ZERO; ALIASREG_MAX_ENTRIES];
static mut aliasreg_count: c_int = 0;
static mut aliasreg_loaded: c_int = 0;
static mut aliasreg_path: [u8; 1024] = [0; 1024];

unsafe fn annot_file_path(name: *const c_char, out: *mut c_char, cap: usize) {
    let jit = libc::getenv(cstr!("SAISEI_JIT_DIR"));
    let repo = libc::getenv(cstr!("SAISEI_REPO_ROOT"));
    if !repo.is_null() && *repo != 0 && !jit.is_null() && *jit != 0 {
        let mut tmp = [0u8; 1024];
        libc::snprintf(tmp.as_mut_ptr() as *mut c_char, tmp.len(), cstr!("%s"), jit);
        let s = libc::strrchr(tmp.as_mut_ptr() as *mut c_char, b'/' as c_int);
        if !s.is_null() {
            *s = 0;
            let key0 = libc::strrchr(tmp.as_ptr() as *const c_char, b'/' as c_int);
            let key = if !key0.is_null() {
                key0.add(1)
            } else {
                tmp.as_ptr() as *const c_char
            };
            let mut dir = [0u8; 1100];
            libc::snprintf(
                dir.as_mut_ptr() as *mut c_char,
                dir.len(),
                cstr!("%s/games/%s"),
                repo,
                key,
            );
            if libc::access(dir.as_ptr() as *const c_char, libc::F_OK) == 0 {
                libc::snprintf(
                    out,
                    cap,
                    cstr!("%s/%s"),
                    dir.as_ptr() as *const c_char,
                    name,
                );
                return;
            }
        }
    }
    if !jit.is_null() && *jit != 0 {
        libc::snprintf(out, cap, cstr!("%s/../%s"), jit, name);
    } else {
        libc::snprintf(out, cap, cstr!("%s"), name);
    }
}

unsafe fn aliasreg_compute_path() {
    annot_file_path(
        cstr!("aliases.json"),
        ptr::addr_of_mut!(aliasreg_path) as *mut c_char,
        (*ptr::addr_of!(aliasreg_path)).len(),
    );
}

unsafe fn aliasreg_find(id: *const c_char) -> *mut AliasRegEntry {
    for i in 0..aliasreg_count {
        if libc::strcmp(aliasreg_entries[i as usize].id, id) == 0 {
            return &mut aliasreg_entries[i as usize];
        }
    }
    ptr::null_mut()
}

unsafe fn aliasreg_read_string(
    mut p: *const c_char,
    out: *mut c_char,
    cap: usize,
) -> *const c_char {
    let mut n: usize = 0;
    if *p as u8 == b'"' {
        p = p.add(1);
    }
    while *p != 0 && *p as u8 != b'"' {
        let mut c = *p;
        p = p.add(1);
        if c as u8 == b'\\' && *p != 0 {
            c = *p;
            p = p.add(1);
        }
        if n + 1 < cap {
            *out.add(n) = c;
            n += 1;
        }
    }
    *out.add(n) = 0;
    if *p as u8 == b'"' {
        p = p.add(1);
    }
    p
}

unsafe fn aliasreg_read_value(
    mut p: *const c_char,
    name: *mut c_char,
    cap: usize,
) -> *const c_char {
    *name = 0;
    while *p as u8 == b' ' || *p as u8 == b'\t' || *p as u8 == b'\n' || *p as u8 == b'\r' {
        p = p.add(1);
    }
    if *p as u8 == b'"' {
        return aliasreg_read_string(p, name, cap);
    }
    if *p as u8 == b'{' {
        let mut q = p.add(1);
        let mut depth = 1;
        while *q != 0 && depth != 0 {
            if *q as u8 == b'{' {
                depth += 1;
            } else if *q as u8 == b'}' {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            }
            q = q.add(1);
        }
        let end = if *q as u8 == b'}' { q.add(1) } else { q };
        let mut r = p;
        while r.add(6) < end {
            if *r as u8 == b'"'
                && *r.add(1) as u8 == b'n'
                && *r.add(2) as u8 == b'a'
                && *r.add(3) as u8 == b'm'
                && *r.add(4) as u8 == b'e'
                && *r.add(5) as u8 == b'"'
            {
                let mut s = r.add(6);
                while s < end && *s as u8 != b'"' {
                    s = s.add(1);
                }
                if s < end {
                    aliasreg_read_string(s, name, cap);
                }
                break;
            }
            r = r.add(1);
        }
        return end;
    }
    while *p != 0 && *p as u8 != b',' && *p as u8 != b'}' {
        p = p.add(1);
    }
    p
}

unsafe fn aliasreg_load() {
    if aliasreg_loaded != 0 {
        return;
    }
    aliasreg_loaded = 1;
    aliasreg_compute_path();
    let fp = libc::fopen(ptr::addr_of!(aliasreg_path) as *const c_char, cstr!("rb"));
    if fp.is_null() {
        return;
    }
    libc::fseek(fp, 0, libc::SEEK_END);
    let sz = libc::ftell(fp);
    if sz <= 0 || sz > (16i64 << 20) {
        libc::fclose(fp);
        return;
    }
    libc::rewind(fp);
    let buf = libc::malloc(sz as usize + 1) as *mut c_char;
    if buf.is_null() {
        libc::fclose(fp);
        return;
    }
    let rd = libc::fread(buf as *mut c_void, 1, sz as usize, fp);
    libc::fclose(fp);
    *buf.add(rd) = 0;
    let mut p: *const c_char = buf;
    let mut key = [0u8; 160];
    let mut val = [0u8; 96];
    while *p != 0 && aliasreg_count < ALIASREG_MAX_ENTRIES as c_int {
        while *p != 0 && *p as u8 != b'"' {
            p = p.add(1);
        }
        if *p == 0 {
            break;
        }
        p = aliasreg_read_string(p, key.as_mut_ptr() as *mut c_char, key.len());
        while *p != 0 && *p as u8 != b':' && *p as u8 != b'"' {
            p = p.add(1);
        }
        if *p as u8 != b':' {
            continue;
        }
        p = p.add(1);
        p = aliasreg_read_value(p, val.as_mut_ptr() as *mut c_char, val.len());
        if key[0] != 0 && aliasreg_find(key.as_ptr() as *const c_char).is_null() {
            let kd = libc::strdup(key.as_ptr() as *const c_char);
            let vd = libc::strdup(val.as_ptr() as *const c_char);
            if !kd.is_null() && !vd.is_null() {
                aliasreg_entries[aliasreg_count as usize].id = kd;
                aliasreg_entries[aliasreg_count as usize].alias = vd;
                aliasreg_count += 1;
            } else {
                libc::free(kd as *mut c_void);
                libc::free(vd as *mut c_void);
            }
        }
    }
    libc::free(buf as *mut c_void);
}

unsafe fn aliasreg_write_escaped(fp: *mut FILE, mut s: *const c_char) {
    while !s.is_null() && *s != 0 {
        if *s as u8 == b'"' || *s as u8 == b'\\' {
            libc::fputc(b'\\' as c_int, fp);
        }
        libc::fputc(*s as c_int, fp);
        s = s.add(1);
    }
}

unsafe fn aliasreg_save() {
    let mut tmp = [0u8; 1100];
    libc::snprintf(
        tmp.as_mut_ptr() as *mut c_char,
        tmp.len(),
        cstr!("%s.tmp"),
        ptr::addr_of!(aliasreg_path) as *const c_char,
    );
    let fp = libc::fopen(tmp.as_ptr() as *const c_char, cstr!("wb"));
    if fp.is_null() {
        return;
    }
    libc::fputs(cstr!("{\n"), fp);
    for i in 0..aliasreg_count {
        libc::fputs(cstr!("  \""), fp);
        aliasreg_write_escaped(fp, aliasreg_entries[i as usize].id);
        libc::fputs(cstr!("\": \""), fp);
        aliasreg_write_escaped(fp, aliasreg_entries[i as usize].alias);
        libc::fputs(
            if i + 1 < aliasreg_count {
                cstr!("\",\n")
            } else {
                cstr!("\"\n")
            },
            fp,
        );
    }
    libc::fputs(cstr!("}\n"), fp);
    libc::fclose(fp);
    libc::rename(
        tmp.as_ptr() as *const c_char,
        ptr::addr_of!(aliasreg_path) as *const c_char,
    );
}

unsafe fn aliasreg_alias(id: *const c_char, seed: c_int) -> *const c_char {
    if id.is_null() || *id == 0 {
        return ptr::null();
    }
    aliasreg_load();
    let mut e = aliasreg_find(id);
    if e.is_null() && seed != 0 && isr_depth == 0 && aliasreg_count < ALIASREG_MAX_ENTRIES as c_int
    {
        let kd = libc::strdup(id);
        let vd = libc::strdup(cstr!(""));
        if !kd.is_null() && !vd.is_null() {
            e = &mut aliasreg_entries[aliasreg_count as usize];
            aliasreg_count += 1;
            (*e).id = kd;
            (*e).alias = vd;
            aliasreg_save();
        } else {
            libc::free(kd as *mut c_void);
            libc::free(vd as *mut c_void);
        }
    }
    if !e.is_null() && !(*e).alias.is_null() && *(*e).alias != 0 {
        (*e).alias
    } else {
        ptr::null()
    }
}

// ============================================================================
// Named memory regions (addresses -> region names)  [C lines 2963-3032]
// ============================================================================

const ALIASREG_MAX_REGIONS: usize = 64;

#[repr(C)]
#[derive(Clone, Copy)]
struct AliasRegRegion {
    lo: u32,
    hi: u32,
    name: *mut c_char,
}
impl AliasRegRegion {
    const ZERO: AliasRegRegion = AliasRegRegion {
        lo: 0,
        hi: 0,
        name: ptr::null_mut(),
    };
}
static mut aliasreg_regions: [AliasRegRegion; ALIASREG_MAX_REGIONS] =
    [AliasRegRegion::ZERO; ALIASREG_MAX_REGIONS];
static mut aliasreg_region_count: c_int = 0;
static mut aliasreg_regions_loaded: c_int = 0;

unsafe fn aliasreg_regions_load() {
    if aliasreg_regions_loaded != 0 {
        return;
    }
    aliasreg_regions_loaded = 1;
    let mut path = [0u8; 1100];
    annot_file_path(
        cstr!("regions.json"),
        path.as_mut_ptr() as *mut c_char,
        path.len(),
    );
    let fp = libc::fopen(path.as_ptr() as *const c_char, cstr!("rb"));
    if fp.is_null() {
        return;
    }
    libc::fseek(fp, 0, libc::SEEK_END);
    let sz = libc::ftell(fp);
    if sz <= 0 || sz > (1i64 << 20) {
        libc::fclose(fp);
        return;
    }
    libc::rewind(fp);
    let buf = libc::malloc(sz as usize + 1) as *mut c_char;
    if buf.is_null() {
        libc::fclose(fp);
        return;
    }
    let rd = libc::fread(buf as *mut c_void, 1, sz as usize, fp);
    libc::fclose(fp);
    *buf.add(rd) = 0;
    let mut p: *const c_char = buf;
    let mut key = [0u8; 64];
    let mut val = [0u8; 64];
    while *p != 0 && aliasreg_region_count < ALIASREG_MAX_REGIONS as c_int {
        while *p != 0 && *p as u8 != b'"' {
            p = p.add(1);
        }
        if *p == 0 {
            break;
        }
        p = aliasreg_read_string(p, key.as_mut_ptr() as *mut c_char, key.len());
        while *p != 0 && *p as u8 != b':' && *p as u8 != b'}' {
            p = p.add(1);
        }
        if *p as u8 != b':' {
            continue;
        }
        p = p.add(1);
        p = aliasreg_read_value(p, val.as_mut_ptr() as *mut c_char, val.len());
        let mut lo: c_ulong = 0;
        let mut hi: c_ulong = 0;
        if libc::sscanf(
            key.as_ptr() as *const c_char,
            cstr!("%lx-%lx"),
            &mut lo as *mut c_ulong,
            &mut hi as *mut c_ulong,
        ) == 2
            && hi >= lo
            && val[0] != 0
        {
            let nd = libc::strdup(val.as_ptr() as *const c_char);
            if !nd.is_null() {
                aliasreg_regions[aliasreg_region_count as usize].lo = lo as u32;
                aliasreg_regions[aliasreg_region_count as usize].hi = hi as u32;
                aliasreg_regions[aliasreg_region_count as usize].name = nd;
                aliasreg_region_count += 1;
            }
        }
    }
    libc::free(buf as *mut c_void);
}

unsafe fn name_addr(lin: u32, out: *mut c_char, cap: usize) -> *const c_char {
    aliasreg_regions_load();
    for i in 0..aliasreg_region_count {
        if lin >= aliasreg_regions[i as usize].lo && lin <= aliasreg_regions[i as usize].hi {
            libc::snprintf(
                out,
                cap,
                cstr!("%s+0x%X"),
                aliasreg_regions[i as usize].name,
                (lin - aliasreg_regions[i as usize].lo) as c_uint,
            );
            return out;
        }
    }
    let mut i = file_mapping_count as isize - 1;
    while i >= 0 {
        let base = file_mappings[i as usize].base;
        if lin >= base
            && lin < base + file_mappings[i as usize].len as u32
            && !file_mappings[i as usize].path.is_null()
        {
            let bn0 = libc::strrchr(file_mappings[i as usize].path, b'/' as c_int);
            let bn = if !bn0.is_null() {
                bn0.add(1)
            } else {
                file_mappings[i as usize].path
            };
            libc::snprintf(
                out,
                cap,
                cstr!("%s+0x%zX"),
                bn,
                (file_mappings[i as usize].file_offset + (lin - base) as usize),
            );
            return out;
        }
        i -= 1;
    }
    libc::snprintf(out, cap, cstr!("0x%05X"), lin as c_uint);
    out
}

// ============================================================================
// Named data variables (change-watch)  [C lines 3042-3172]
// ============================================================================

const ALIASREG_MAX_VARS: usize = 256;

#[repr(C)]
#[derive(Clone, Copy)]
struct AliasRegVar {
    addr: u32,
    size: u8,
    name: *mut c_char,
    last: u32,
    seen: c_int,
    reports: c_int,
    origin_bin: *mut c_char,
    origin_off: u32,
}
impl AliasRegVar {
    const ZERO: AliasRegVar = AliasRegVar {
        addr: 0,
        size: 0,
        name: ptr::null_mut(),
        last: 0,
        seen: 0,
        reports: 0,
        origin_bin: ptr::null_mut(),
        origin_off: 0,
    };
}
static mut aliasreg_vars: [AliasRegVar; ALIASREG_MAX_VARS] = [AliasRegVar::ZERO; ALIASREG_MAX_VARS];
static mut aliasreg_var_count: c_int = 0;
static mut aliasreg_vars_loaded: c_int = 0;
static mut aliasreg_has_origin_vars: c_int = 0;
static mut aliasreg_var_lo: u32 = 0xFFFFFFFF;
static mut aliasreg_var_hi: u32 = 0;

unsafe fn aliasreg_vars_load() {
    if aliasreg_vars_loaded != 0 {
        return;
    }
    aliasreg_vars_loaded = 1;
    let mut path = [0u8; 1100];
    annot_file_path(
        cstr!("vars.json"),
        path.as_mut_ptr() as *mut c_char,
        path.len(),
    );
    let fp = libc::fopen(path.as_ptr() as *const c_char, cstr!("rb"));
    if fp.is_null() {
        return;
    }
    libc::fseek(fp, 0, libc::SEEK_END);
    let sz = libc::ftell(fp);
    if sz <= 0 || sz > (1i64 << 20) {
        libc::fclose(fp);
        return;
    }
    libc::rewind(fp);
    let buf = libc::malloc(sz as usize + 1) as *mut c_char;
    if buf.is_null() {
        libc::fclose(fp);
        return;
    }
    let rd = libc::fread(buf as *mut c_void, 1, sz as usize, fp);
    libc::fclose(fp);
    *buf.add(rd) = 0;
    let mut p: *const c_char = buf;
    let mut key = [0u8; 64];
    let mut val = [0u8; 64];
    while *p != 0 && aliasreg_var_count < ALIASREG_MAX_VARS as c_int {
        while *p != 0 && *p as u8 != b'"' {
            p = p.add(1);
        }
        if *p == 0 {
            break;
        }
        p = aliasreg_read_string(p, key.as_mut_ptr() as *mut c_char, key.len());
        while *p != 0 && *p as u8 != b':' && *p as u8 != b'}' {
            p = p.add(1);
        }
        if *p as u8 != b':' {
            continue;
        }
        p = p.add(1);
        p = aliasreg_read_value(p, val.as_mut_ptr() as *mut c_char, val.len());
        if val[0] != 0 {
            let mut binbuf = [0u8; 48];
            let mut off: c_ulong = 0;
            let mut s: c_ulong = 1;
            let mut lin: u32 = 0;
            let mut is_origin = 0;
            let plus = libc::strstr(key.as_ptr() as *const c_char, cstr!("+0x"));
            if !plus.is_null() && !(key[0] == b'0' && (key[1] == b'x' || key[1] == b'X')) {
                let blen = plus.offset_from(key.as_ptr() as *const c_char) as usize;
                if blen > 0 && blen < binbuf.len() {
                    libc::memcpy(
                        binbuf.as_mut_ptr() as *mut c_void,
                        key.as_ptr() as *const c_void,
                        blen,
                    );
                    binbuf[blen] = 0;
                    if libc::sscanf(
                        plus.add(3),
                        cstr!("%lx:%lu"),
                        &mut off as *mut c_ulong,
                        &mut s as *mut c_ulong,
                    ) >= 1
                    {
                        is_origin = 1;
                    }
                }
            } else {
                let mut a: c_ulong = 0;
                if libc::sscanf(
                    key.as_ptr() as *const c_char,
                    cstr!("%lx:%lu"),
                    &mut a as *mut c_ulong,
                    &mut s as *mut c_ulong,
                ) >= 1
                {
                    lin = a as u32;
                }
            }
            if s != 1 && s != 2 {
                s = 1;
            }
            if is_origin != 0 || lin != 0 {
                let nd = libc::strdup(val.as_ptr() as *const c_char);
                if !nd.is_null() {
                    let v = &mut aliasreg_vars[aliasreg_var_count as usize];
                    aliasreg_var_count += 1;
                    v.size = s as u8;
                    v.name = nd;
                    v.last = 0;
                    v.seen = 0;
                    if is_origin != 0 {
                        v.origin_bin = libc::strdup(binbuf.as_ptr() as *const c_char);
                        v.origin_off = off as u32;
                        v.addr = 0;
                        aliasreg_has_origin_vars = 1;
                    } else {
                        v.origin_bin = ptr::null_mut();
                        v.origin_off = 0;
                        v.addr = lin;
                        if v.addr < aliasreg_var_lo {
                            aliasreg_var_lo = v.addr;
                        }
                        if v.addr + v.size as u32 - 1 > aliasreg_var_hi {
                            aliasreg_var_hi = v.addr + v.size as u32 - 1;
                        }
                    }
                }
            }
        }
    }
    libc::free(buf as *mut c_void);
}

unsafe fn resolve_origin_to_linear(bin: *const c_char, off: u32) -> u32 {
    let mut i = file_mapping_count as isize - 1;
    while i >= 0 {
        let m = &file_mappings[i as usize];
        if m.path.is_null() {
            i -= 1;
            continue;
        }
        let bn0 = libc::strrchr(m.path, b'/' as c_int);
        let bn = if !bn0.is_null() { bn0.add(1) } else { m.path };
        if libc::strcmp(bn, bin) != 0 {
            i -= 1;
            continue;
        }
        if off as usize >= m.file_offset && (off as usize) < m.file_offset + m.len {
            return m.base + (off as usize - m.file_offset) as u32;
        }
        i -= 1;
    }
    0
}

unsafe fn aliasreg_vars_resolve() {
    aliasreg_var_lo = 0xFFFFFFFF;
    aliasreg_var_hi = 0;
    for i in 0..aliasreg_var_count {
        let v = &mut aliasreg_vars[i as usize];
        if !v.origin_bin.is_null() {
            let lin = resolve_origin_to_linear(v.origin_bin, v.origin_off);
            if lin != 0 && lin != v.addr {
                v.addr = lin;
                v.seen = 0;
                v.reports = 0;
            }
            if lin == 0 {
                continue;
            }
        }
        if v.addr != 0 {
            if v.addr < aliasreg_var_lo {
                aliasreg_var_lo = v.addr;
            }
            if v.addr + v.size as u32 - 1 > aliasreg_var_hi {
                aliasreg_var_hi = v.addr + v.size as u32 - 1;
            }
        }
    }
}

unsafe fn aliasreg_var_write(addr: u32, _size: u8, value: u32) {
    for i in 0..aliasreg_var_count {
        let v = &mut aliasreg_vars[i as usize];
        if v.addr != addr {
            continue;
        }
        let nv: u32 = if v.size == 1 {
            value & 0xFF
        } else {
            value & 0xFFFF
        };
        if v.seen != 0 && nv == v.last {
            return;
        }
        if v.reports < 50 {
            if v.seen != 0 {
                shim_log_stdout(
                    cstr!("VAR %s: 0x%X -> 0x%X  (cs:ip=%04X:%04X)\n"),
                    v.name,
                    v.last as c_uint,
                    nv as c_uint,
                    cs() as c_uint,
                    ip() as c_uint,
                );
            } else {
                shim_log_stdout(
                    cstr!("VAR %s = 0x%X  (first seen, cs:ip=%04X:%04X)\n"),
                    v.name,
                    nv as c_uint,
                    cs() as c_uint,
                    ip() as c_uint,
                );
            }
            v.reports += 1;
            if v.reports == 50 {
                shim_log_stdout(cstr!("VAR %s: further changes suppressed (cap)\n"), v.name);
            }
        }
        v.last = nv;
        v.seen = 1;
        return;
    }
}

// ============================================================================
// Constants / enums on call args  [C lines 3181-3304]
// ============================================================================

const ALIASREG_MAX_ENUMS: usize = 2048;

#[repr(C)]
#[derive(Clone, Copy)]
struct AliasRegEnum {
    ename: *mut c_char,
    value: u32,
    label: *mut c_char,
}
impl AliasRegEnum {
    const ZERO: AliasRegEnum = AliasRegEnum {
        ename: ptr::null_mut(),
        value: 0,
        label: ptr::null_mut(),
    };
}
static mut aliasreg_enums: [AliasRegEnum; ALIASREG_MAX_ENUMS] =
    [AliasRegEnum::ZERO; ALIASREG_MAX_ENUMS];
static mut aliasreg_enum_count: c_int = 0;
static mut aliasreg_enums_loaded: c_int = 0;

unsafe fn aliasreg_enums_load() {
    if aliasreg_enums_loaded != 0 {
        return;
    }
    aliasreg_enums_loaded = 1;
    let mut path = [0u8; 1100];
    annot_file_path(
        cstr!("enums.json"),
        path.as_mut_ptr() as *mut c_char,
        path.len(),
    );
    let fp = libc::fopen(path.as_ptr() as *const c_char, cstr!("rb"));
    if fp.is_null() {
        return;
    }
    libc::fseek(fp, 0, libc::SEEK_END);
    let sz = libc::ftell(fp);
    if sz <= 0 || sz > (4i64 << 20) {
        libc::fclose(fp);
        return;
    }
    libc::rewind(fp);
    let buf = libc::malloc(sz as usize + 1) as *mut c_char;
    if buf.is_null() {
        libc::fclose(fp);
        return;
    }
    let rd = libc::fread(buf as *mut c_void, 1, sz as usize, fp);
    libc::fclose(fp);
    *buf.add(rd) = 0;
    let mut p: *const c_char = buf;
    let mut key = [0u8; 80];
    let mut val = [0u8; 64];
    while *p != 0 && aliasreg_enum_count < ALIASREG_MAX_ENUMS as c_int {
        while *p != 0 && *p as u8 != b'"' {
            p = p.add(1);
        }
        if *p == 0 {
            break;
        }
        p = aliasreg_read_string(p, key.as_mut_ptr() as *mut c_char, key.len());
        while *p != 0 && *p as u8 != b':' && *p as u8 != b'}' {
            p = p.add(1);
        }
        if *p as u8 != b':' {
            continue;
        }
        p = p.add(1);
        p = aliasreg_read_value(p, val.as_mut_ptr() as *mut c_char, val.len());
        let colon = libc::strrchr(key.as_mut_ptr() as *mut c_char, b':' as c_int);
        if !colon.is_null() && val[0] != 0 {
            *colon = 0;
            let v = libc::strtoul(colon.add(1), ptr::null_mut(), 0);
            let en = libc::strdup(key.as_ptr() as *const c_char);
            let lb = libc::strdup(val.as_ptr() as *const c_char);
            if !en.is_null() && !lb.is_null() {
                aliasreg_enums[aliasreg_enum_count as usize].ename = en;
                aliasreg_enums[aliasreg_enum_count as usize].value = v as u32;
                aliasreg_enums[aliasreg_enum_count as usize].label = lb;
                aliasreg_enum_count += 1;
            } else {
                libc::free(en as *mut c_void);
                libc::free(lb as *mut c_void);
            }
        }
    }
    libc::free(buf as *mut c_void);
}

unsafe fn aliasreg_enum_label(ename: *const c_char, value: u32) -> *const c_char {
    for i in 0..aliasreg_enum_count {
        if aliasreg_enums[i as usize].value == value
            && libc::strcmp(aliasreg_enums[i as usize].ename, ename) == 0
        {
            return aliasreg_enums[i as usize].label;
        }
    }
    ptr::null()
}

/// Register values captured at a lifecycle-event's record time, so deferred
/// ring entries can be formatted at dump time with the registers the event
/// actually saw (the live cpu has long since moved on).
#[repr(C)]
#[derive(Clone, Copy)]
struct RegSnap {
    ax: u16,
    bx: u16,
    cx: u16,
    dx: u16,
    si: u16,
    di: u16,
    bp: u16,
    sp: u16,
    ip: u16,
    cs: u16,
    ds: u16,
    es: u16,
    ss: u16,
}

unsafe fn regsnap_now() -> RegSnap {
    RegSnap {
        ax: ax(),
        bx: bx(),
        cx: cx(),
        dx: dx(),
        si: si(),
        di: di(),
        bp: bp(),
        sp: sp(),
        ip: ip(),
        cs: cs(),
        ds: ds(),
        es: es(),
        ss: ss(),
    }
}

unsafe fn aliasreg_reg_value(r: *const c_char, s: &RegSnap) -> u32 {
    if libc::strcmp(r, cstr!("ax")) == 0 {
        return s.ax as u32;
    }
    if libc::strcmp(r, cstr!("bx")) == 0 {
        return s.bx as u32;
    }
    if libc::strcmp(r, cstr!("cx")) == 0 {
        return s.cx as u32;
    }
    if libc::strcmp(r, cstr!("dx")) == 0 {
        return s.dx as u32;
    }
    if libc::strcmp(r, cstr!("si")) == 0 {
        return s.si as u32;
    }
    if libc::strcmp(r, cstr!("di")) == 0 {
        return s.di as u32;
    }
    if libc::strcmp(r, cstr!("bp")) == 0 {
        return s.bp as u32;
    }
    if libc::strcmp(r, cstr!("ds")) == 0 {
        return s.ds as u32;
    }
    if libc::strcmp(r, cstr!("es")) == 0 {
        return s.es as u32;
    }
    if libc::strcmp(r, cstr!("cs")) == 0 {
        return s.cs as u32;
    }
    if libc::strcmp(r, cstr!("al")) == 0 {
        return (s.ax & 0xFF) as u32;
    }
    if libc::strcmp(r, cstr!("ah")) == 0 {
        return ((s.ax >> 8) & 0xFF) as u32;
    }
    if libc::strcmp(r, cstr!("bl")) == 0 {
        return (s.bx & 0xFF) as u32;
    }
    if libc::strcmp(r, cstr!("bh")) == 0 {
        return ((s.bx >> 8) & 0xFF) as u32;
    }
    if libc::strcmp(r, cstr!("cl")) == 0 {
        return (s.cx & 0xFF) as u32;
    }
    if libc::strcmp(r, cstr!("ch")) == 0 {
        return ((s.cx >> 8) & 0xFF) as u32;
    }
    if libc::strcmp(r, cstr!("dl")) == 0 {
        return (s.dx & 0xFF) as u32;
    }
    if libc::strcmp(r, cstr!("dh")) == 0 {
        return ((s.dx >> 8) & 0xFF) as u32;
    }
    0
}

unsafe fn render_alias_with_args(
    alias: *const c_char,
    out: *mut c_char,
    cap: usize,
    regs: &RegSnap,
) -> *const c_char {
    let lp = libc::strchr(alias, b'(' as c_int);
    if lp.is_null() {
        libc::snprintf(out, cap, cstr!("%s"), alias);
        return out;
    }
    aliasreg_enums_load();
    let mut n: usize = 0;
    let mut c = alias;
    while c < lp && n + 1 < cap {
        *out.add(n) = *c;
        n += 1;
        c = c.add(1);
    }
    if n + 1 < cap {
        *out.add(n) = b'(' as c_char;
        n += 1;
    }
    let rp = libc::strchr(lp, b')' as c_int);
    let aend = if !rp.is_null() {
        rp
    } else {
        lp.add(libc::strlen(lp))
    };
    let mut a = lp.add(1);
    let mut first = 1;
    while a < aend {
        let mut comma = a;
        while comma < aend && *comma as u8 != b',' {
            comma = comma.add(1);
        }
        let mut reg = [0u8; 8];
        let mut argn = [0u8; 24];
        let mut en = [0u8; 24];
        let mut sep = a;
        while sep < comma && *sep as u8 != b':' && *sep as u8 != b'@' {
            sep = sep.add(1);
        }
        let mut k: usize = 0;
        let mut r = a;
        while r < sep && k < reg.len() - 1 {
            reg[k] = *r as u8;
            k += 1;
            r = r.add(1);
        }
        reg[k] = 0;
        if sep < comma && *sep as u8 == b':' {
            sep = sep.add(1);
            let mut at = sep;
            while at < comma && *at as u8 != b'@' {
                at = at.add(1);
            }
            k = 0;
            let mut r = sep;
            while r < at && k < argn.len() - 1 {
                argn[k] = *r as u8;
                k += 1;
                r = r.add(1);
            }
            argn[k] = 0;
            if at < comma && *at as u8 == b'@' {
                at = at.add(1);
                k = 0;
                let mut r = at;
                while r < comma && k < en.len() - 1 {
                    en[k] = *r as u8;
                    k += 1;
                    r = r.add(1);
                }
                en[k] = 0;
            }
        }
        let v = aliasreg_reg_value(reg.as_ptr() as *const c_char, regs);
        let lab = if en[0] != 0 {
            aliasreg_enum_label(en.as_ptr() as *const c_char, v)
        } else {
            ptr::null()
        };
        if first == 0 && n + 2 < cap {
            *out.add(n) = b',' as c_char;
            n += 1;
            *out.add(n) = b' ' as c_char;
            n += 1;
        }
        first = 0;
        if argn[0] != 0 {
            let w = libc::snprintf(
                out.add(n),
                if n < cap { cap - n } else { 0 },
                cstr!("%s="),
                argn.as_ptr() as *const c_char,
            );
            if w > 0 {
                n += w as usize;
            }
            if n >= cap {
                n = cap - 1;
            }
        }
        {
            let w = if !lab.is_null() {
                libc::snprintf(
                    out.add(n),
                    if n < cap { cap - n } else { 0 },
                    cstr!("%s"),
                    lab,
                )
            } else {
                libc::snprintf(
                    out.add(n),
                    if n < cap { cap - n } else { 0 },
                    cstr!("0x%X"),
                    v as c_uint,
                )
            };
            if w > 0 {
                n += w as usize;
            }
            if n >= cap {
                n = cap - 1;
            }
        }
        a = if comma < aend { comma.add(1) } else { aend };
    }
    if n + 1 < cap {
        *out.add(n) = b')' as c_char;
        n += 1;
    }
    *out.add(n) = 0;
    out
}

// ============================================================================
// Persistent call-graph  [C lines 3313-3414]
// ============================================================================

const CG_HASH: usize = 16384;

#[repr(C)]
#[derive(Clone, Copy)]
struct CGEdge {
    caller: u32,
    callee: u32,
    count: u32,
    line: *mut c_char,
    used: c_int,
}
impl CGEdge {
    const ZERO: CGEdge = CGEdge {
        caller: 0,
        callee: 0,
        count: 0,
        line: ptr::null_mut(),
        used: 0,
    };
}
static mut cg_edges: [CGEdge; CG_HASH] = [CGEdge::ZERO; CG_HASH];
static mut cg_new_since_save: c_int = 0;

unsafe fn cg_caller_func(caller_linear: u32, out: *mut c_char, cap: usize) {
    let mut site = [0u8; 64];
    name_addr(caller_linear, site.as_mut_ptr() as *mut c_char, site.len());
    let plus = libc::strstr(site.as_ptr() as *const c_char, cstr!("+0x"));
    if plus.is_null() {
        libc::snprintf(out, cap, cstr!("%s"), site.as_ptr() as *const c_char);
        return;
    }
    let blen = plus.offset_from(site.as_ptr() as *const c_char) as usize;
    let csoff = libc::strtoul(plus.add(3), ptr::null_mut(), 16);
    let mut best: *const c_char = ptr::null();
    let mut bestoff: c_ulong = 0;
    for i in 0..aliasreg_count {
        let id = aliasreg_entries[i as usize].id;
        let p2 = libc::strstr(id, cstr!("+0x"));
        if p2.is_null()
            || p2.offset_from(id) as usize != blen
            || libc::strncmp(id, site.as_ptr() as *const c_char, blen) != 0
        {
            continue;
        }
        let off2 = libc::strtoul(p2.add(3), ptr::null_mut(), 16);
        if off2 <= csoff && (best.is_null() || off2 > bestoff) {
            best = id;
            bestoff = off2;
        }
    }
    if best.is_null() {
        libc::snprintf(out, cap, cstr!("%s"), site.as_ptr() as *const c_char);
        return;
    }
    let nm = aliasreg_alias(best, 0);
    if !nm.is_null() {
        let mut k: usize = 0;
        let mut s = nm;
        while *s != 0 && *s as u8 != b'(' && k < cap - 1 {
            *out.add(k) = *s;
            k += 1;
            s = s.add(1);
        }
        *out.add(k) = 0;
    } else {
        libc::snprintf(out, cap, cstr!("%s"), best);
    }
}

const CG_MAX_LINES: usize = 4096;

unsafe fn cg_save() {
    let mut path = [0u8; 1100];
    annot_file_path(
        cstr!("callgraph.json"),
        path.as_mut_ptr() as *mut c_char,
        path.len(),
    );
    let mut tmp = [0u8; 1200];
    libc::snprintf(
        tmp.as_mut_ptr() as *mut c_char,
        tmp.len(),
        cstr!("%s.tmp"),
        path.as_ptr() as *const c_char,
    );
    let fp = libc::fopen(tmp.as_ptr() as *const c_char, cstr!("wb"));
    if fp.is_null() {
        return;
    }
    static mut lines: [*const c_char; CG_MAX_LINES] = [ptr::null(); CG_MAX_LINES];
    static mut counts: [u32; CG_MAX_LINES] = [0; CG_MAX_LINES];
    let mut nl: c_int = 0;
    for i in 0..CG_HASH {
        if cg_edges[i].used == 0 || cg_edges[i].line.is_null() {
            continue;
        }
        let mut f: c_int = -1;
        for j in 0..nl {
            if libc::strcmp(lines[j as usize], cg_edges[i].line) == 0 {
                f = j;
                break;
            }
        }
        if f >= 0 {
            counts[f as usize] += cg_edges[i].count;
        } else if nl < CG_MAX_LINES as c_int {
            lines[nl as usize] = cg_edges[i].line;
            counts[nl as usize] = cg_edges[i].count;
            nl += 1;
        }
    }
    libc::fputs(cstr!("{\n"), fp);
    for j in 0..nl {
        if j != 0 {
            libc::fputs(cstr!(",\n"), fp);
        }
        libc::fputs(cstr!("  \""), fp);
        let mut s = lines[j as usize];
        while *s != 0 {
            if *s as u8 == b'"' || *s as u8 == b'\\' {
                libc::fputc(b'\\' as c_int, fp);
            }
            libc::fputc(*s as c_int, fp);
            s = s.add(1);
        }
        libc::fprintf(fp, cstr!("\": \"%u\""), counts[j as usize] as c_uint);
    }
    libc::fputs(cstr!("\n}\n"), fp);
    libc::fclose(fp);
    libc::rename(
        tmp.as_ptr() as *const c_char,
        path.as_ptr() as *const c_char,
    );
}

/// Record a callgraph edge; returns true when the edge is NEW (first time
/// this caller→callee pair is seen) — the moment alias self-seeding fires.
unsafe fn cg_record(caller: u32, callee: u32) -> bool {
    if isr_depth != 0 {
        return false;
    }
    let h = caller
        .wrapping_mul(2654435761u32)
        .wrapping_add(callee.wrapping_mul(40503u32));
    for probe in 0..8u32 {
        let e = &mut cg_edges[((h + probe) & (CG_HASH as u32 - 1)) as usize];
        if e.used != 0 {
            if e.caller == caller && e.callee == callee {
                e.count += 1;
                return false;
            }
            continue;
        }
        let mut a = [0u8; 64];
        let mut b = [0u8; 64];
        let mut bname = [0u8; 64];
        cg_caller_func(caller, a.as_mut_ptr() as *mut c_char, a.len());
        name_addr(callee, b.as_mut_ptr() as *mut c_char, b.len());
        let nm = aliasreg_alias(b.as_ptr() as *const c_char, 0);
        let mut callee_disp: *const c_char = b.as_ptr() as *const c_char;
        if !nm.is_null() {
            let mut k: usize = 0;
            let mut s = nm;
            while *s != 0 && *s as u8 != b'(' && k < bname.len() - 1 {
                bname[k] = *s as u8;
                k += 1;
                s = s.add(1);
            }
            bname[k] = 0;
            callee_disp = bname.as_ptr() as *const c_char;
        }
        let mut line = [0u8; 160];
        libc::snprintf(
            line.as_mut_ptr() as *mut c_char,
            line.len(),
            cstr!("%s -> %s"),
            a.as_ptr() as *const c_char,
            callee_disp,
        );
        e.used = 1;
        e.caller = caller;
        e.callee = callee;
        e.count = 1;
        e.line = libc::strdup(line.as_ptr() as *const c_char);
        cg_new_since_save += 1;
        if cg_new_since_save >= 16 {
            cg_new_since_save = 0;
            cg_save();
        }
        return true;
    }
    false
}

// ============================================================================
// shim_log + case-insensitive path resolution + executable/overlay loading
// [C lines 3416-3953]
// ============================================================================

#[no_mangle]
pub unsafe extern "C" fn shim_log(
    func_name: *const c_char,
    file: *const c_char,
    func: *const c_char,
    line: c_int,
    path: *const c_char,
) {
    if !path.is_null() {
        shim_log_stdout(
            cstr!("Trace: %s: %s (%s:%s:%d)\n"),
            func_name,
            path,
            file,
            func,
            line,
        );
    } else {
        shim_log_stdout(cstr!("Trace: %s (%s:%s:%d)\n"), func_name, file, func, line);
    }
}

unsafe fn resolve_case_insensitive_path(
    path: *const c_char,
    resolved: *mut c_char,
    resolved_size: usize,
) -> bool {
    if path.is_null() || *path == 0 || resolved.is_null() || resolved_size == 0 {
        return false;
    }
    let mut normalized = [0u8; PATH_MAX];
    let mut len: usize = 0;
    while *path.add(len) != 0 && len < normalized.len() - 1 {
        let mut current = *path.add(len) as u8;
        if current == b'\\' {
            current = b'/';
        }
        normalized[len] = current;
        len += 1;
    }
    if *path.add(len) != 0 {
        return false;
    }
    normalized[len] = 0;

    let mut start: usize = 0;
    let mut absolute = false;
    if normalized[0] == b'/' {
        absolute = true;
        start = 1;
    } else if len >= 2 && normalized[1] == b':' {
        start = 2;
        if normalized[2] == b'/' {
            start = 3;
        }
    }

    let mut current_path = [0u8; PATH_MAX];
    if absolute {
        current_path[0] = b'/';
        current_path[1] = 0;
    } else {
        current_path[0] = 0;
    }

    let mut cursor = normalized.as_ptr().add(start) as *const c_char;
    while *cursor != 0 {
        let next = libc::strchr(cursor, b'/' as c_int);
        let comp_len = if !next.is_null() {
            next.offset_from(cursor) as usize
        } else {
            libc::strlen(cursor)
        };
        if comp_len == 0 {
            if next.is_null() {
                break;
            }
            cursor = next.add(1);
            continue;
        }
        if comp_len > NAME_MAX {
            return false;
        }

        let mut component = [0u8; NAME_MAX + 1];
        libc::memcpy(
            component.as_mut_ptr() as *mut c_void,
            cursor as *const c_void,
            comp_len,
        );
        component[comp_len] = 0;

        if libc::strcmp(component.as_ptr() as *const c_char, cstr!(".")) == 0 {
            // No-op for current directory.
        } else if libc::strcmp(component.as_ptr() as *const c_char, cstr!("..")) == 0 {
            let mut cur_len = libc::strlen(current_path.as_ptr() as *const c_char);
            if cur_len == 0 || (absolute && cur_len == 1 && current_path[0] == b'/') {
                // Already at base.
            } else {
                if current_path[cur_len - 1] == b'/' {
                    current_path[cur_len - 1] = 0;
                    cur_len -= 1;
                }
                while cur_len > 0 && current_path[cur_len - 1] != b'/' {
                    cur_len -= 1;
                }
                current_path[cur_len] = 0;
                if cur_len == 0 && absolute {
                    current_path[0] = b'/';
                    current_path[1] = 0;
                }
            }
        } else {
            let mut dir_path = [0u8; PATH_MAX];
            if current_path[0] == 0 {
                libc::strcpy(
                    dir_path.as_mut_ptr() as *mut c_char,
                    if absolute { cstr!("/") } else { cstr!(".") },
                );
            } else {
                libc::strcpy(
                    dir_path.as_mut_ptr() as *mut c_char,
                    current_path.as_ptr() as *const c_char,
                );
            }

            let dir = libc::opendir(dir_path.as_ptr() as *const c_char);
            if dir.is_null() {
                return false;
            }
            let mut matched: *const c_char = ptr::null();
            loop {
                let entry = libc::readdir(dir);
                if entry.is_null() {
                    break;
                }
                if libc::strcasecmp(
                    (*entry).d_name.as_ptr(),
                    component.as_ptr() as *const c_char,
                ) == 0
                {
                    matched = (*entry).d_name.as_ptr();
                    break;
                }
            }
            libc::closedir(dir);

            if matched.is_null() {
                return false;
            }

            let mut cur_len = libc::strlen(current_path.as_ptr() as *const c_char);
            if cur_len == 0 {
                if absolute {
                    if libc::snprintf(
                        current_path.as_mut_ptr() as *mut c_char,
                        current_path.len(),
                        cstr!("/%s"),
                        matched,
                    ) >= current_path.len() as c_int
                    {
                        return false;
                    }
                } else {
                    if libc::snprintf(
                        current_path.as_mut_ptr() as *mut c_char,
                        current_path.len(),
                        cstr!("%s"),
                        matched,
                    ) >= current_path.len() as c_int
                    {
                        return false;
                    }
                }
            } else if absolute && cur_len == 1 && current_path[0] == b'/' {
                let match_len = libc::strlen(matched);
                if cur_len + match_len >= current_path.len() {
                    return false;
                }
                libc::memcpy(
                    current_path.as_mut_ptr().add(cur_len) as *mut c_void,
                    matched as *const c_void,
                    match_len + 1,
                );
            } else {
                if current_path[cur_len - 1] != b'/' {
                    if cur_len + 1 >= current_path.len() {
                        return false;
                    }
                    current_path[cur_len] = b'/';
                    cur_len += 1;
                    current_path[cur_len] = 0;
                }
                let match_len = libc::strlen(matched);
                if cur_len + match_len >= current_path.len() {
                    return false;
                }
                libc::memcpy(
                    current_path.as_mut_ptr().add(cur_len) as *mut c_void,
                    matched as *const c_void,
                    match_len + 1,
                );
            }
        }

        if next.is_null() {
            break;
        }
        cursor = next.add(1);
    }

    if current_path[0] == 0 {
        return false;
    }

    libc::strncpy(
        resolved,
        current_path.as_ptr() as *const c_char,
        resolved_size,
    );
    *resolved.add(resolved_size - 1) = 0;
    true
}

#[no_mangle]
pub unsafe extern "C" fn fopen_case_insensitive(
    path: *const c_char,
    mode: *const c_char,
) -> *mut FILE {
    let f = libc::fopen(path, mode);
    if !f.is_null() {
        return f;
    }
    let saved_errno = *libc::__errno_location();
    if saved_errno != libc::ENOENT {
        return ptr::null_mut();
    }
    let mut resolved = [0u8; PATH_MAX];
    if !resolve_case_insensitive_path(path, resolved.as_mut_ptr() as *mut c_char, resolved.len()) {
        *libc::__errno_location() = saved_errno;
        return ptr::null_mut();
    }
    if libc::strcmp(resolved.as_ptr() as *const c_char, path) == 0 {
        *libc::__errno_location() = saved_errno;
        return ptr::null_mut();
    }
    let retry = libc::fopen(resolved.as_ptr() as *const c_char, mode);
    if !retry.is_null() {
        shim_log_stdout(
            cstr!("Trace: fopen_case_insensitive matched %s -> %s\n"),
            path,
            resolved.as_ptr() as *const c_char,
        );
        return retry;
    }
    if *libc::__errno_location() == libc::ENOENT {
        *libc::__errno_location() = saved_errno;
    }
    ptr::null_mut()
}

#[no_mangle]
pub unsafe extern "C" fn shim_log_file_load(
    path: *const c_char,
    addr: *const c_void,
    len: usize,
    file_offset: usize,
) {
    let mut offset_buf = [0u8; 32];
    let mut offset_text: *const c_char = cstr!("n/a");
    let mut offset: u32 = 0;
    let in_range = try_memory_offset(addr, &mut offset);
    if in_range {
        libc::snprintf(
            offset_buf.as_mut_ptr() as *mut c_char,
            offset_buf.len(),
            cstr!("0x%zX"),
            offset as usize,
        );
        offset_text = offset_buf.as_ptr() as *const c_char;
    }
    shim_log_stdout(
        cstr!("Trace: loaded %s at %p (mem offset %s, file offset 0x%zX) length %zu\n"),
        path,
        addr,
        offset_text,
        file_offset,
        len,
    );
    if in_range {
        warn_file_overlap(path, addr, len);
        warn_rcb_overlap(path, addr, len);
    }
    register_file_mapping(path, file_offset, addr, len);
}

#[no_mangle]
pub unsafe extern "C" fn load_executable(
    path: *const c_char,
    load_seg: u16,
    is_child: c_int,
    out_cs: *mut u16,
    out_ip: *mut u16,
    out_ss: *mut u16,
    out_sp: *mut u16,
) -> c_int {
    critical_section_enter(
        cstr!("load_executable"),
        SHIMS_FILE,
        cstr!("load_executable"),
        3643,
    );
    let f = fopen_case_insensitive(path, cstr!("rb"));
    if f.is_null() {
        shim_log_stdout(cstr!("Trace: load_executable failed to open %s\n"), path);
        critical_section_exit(
            cstr!("load_executable"),
            SHIMS_FILE,
            cstr!("load_executable"),
            3647,
        );
        return 1;
    }
    let mut header = [0u8; 28];
    let hdr_read = libc::fread(header.as_mut_ptr() as *mut c_void, 1, header.len(), f);
    let mut size: usize = 0;
    if hdr_read >= 6 {
        let e_cblp: u16 = header[2] as u16 | ((header[3] as u16) << 8);
        let e_cp: u16 = header[4] as u16 | ((header[5] as u16) << 8);
        if e_cp > 0 {
            size = ((e_cp - 1) as usize * 512) + (if e_cblp != 0 { e_cblp as usize } else { 512 });
        }
    }
    if size == 0 {
        if libc::fseek(f, 0, libc::SEEK_END) == 0 {
            let actual = libc::ftell(f);
            if actual > 0 {
                size = actual as usize;
            }
        }
    }
    if size == 0 {
        libc::fclose(f);
        shim_log_stdout(cstr!("Trace: load_executable invalid size for %s\n"), path);
        critical_section_exit(
            cstr!("load_executable"),
            SHIMS_FILE,
            cstr!("load_executable"),
            3671,
        );
        return 1;
    }
    let buf = libc::malloc(size) as *mut u8;
    if buf.is_null() {
        libc::fclose(f);
        shim_log_stdout(
            cstr!("Trace: load_executable failed to allocate buffer for %s\n"),
            path,
        );
        critical_section_exit(
            cstr!("load_executable"),
            SHIMS_FILE,
            cstr!("load_executable"),
            3679,
        );
        return 1;
    }
    if libc::fseek(f, 0, libc::SEEK_SET) != 0 || libc::fread(buf as *mut c_void, 1, size, f) != size
    {
        libc::free(buf as *mut c_void);
        libc::fclose(f);
        shim_log_stdout(cstr!("Trace: load_executable failed to read %s\n"), path);
        critical_section_exit(
            cstr!("load_executable"),
            SHIMS_FILE,
            cstr!("load_executable"),
            3686,
        );
        return 1;
    }
    let header_paras: u16 = if size >= 10 {
        *buf.add(8) as u16 | ((*buf.add(9) as u16) << 8)
    } else {
        0
    };
    let min_alloc: u16 = if size >= 12 {
        *buf.add(10) as u16 | ((*buf.add(11) as u16) << 8)
    } else {
        0
    };
    let e_ss: u16 = if size >= 16 {
        *buf.add(14) as u16 | ((*buf.add(15) as u16) << 8)
    } else {
        0
    };
    let e_sp: u16 = if size >= 18 {
        *buf.add(16) as u16 | ((*buf.add(17) as u16) << 8)
    } else {
        0
    };
    let e_ip: u16 = if size >= 22 {
        *buf.add(20) as u16 | ((*buf.add(21) as u16) << 8)
    } else {
        0
    };
    let e_cs: u16 = if size >= 24 {
        *buf.add(22) as u16 | ((*buf.add(23) as u16) << 8)
    } else {
        0
    };
    let reloc_count: u16 = if size >= 8 {
        *buf.add(6) as u16 | ((*buf.add(7) as u16) << 8)
    } else {
        0
    };
    let reloc_off: u16 = if size >= 26 {
        *buf.add(24) as u16 | ((*buf.add(25) as u16) << 8)
    } else {
        0
    };
    let header_size: usize = header_paras as usize * 16;
    if size <= header_size {
        libc::free(buf as *mut c_void);
        libc::fclose(f);
        shim_log_stdout(
            cstr!("Trace: load_executable header too small for %s\n"),
            path,
        );
        critical_section_exit(
            cstr!("load_executable"),
            SHIMS_FILE,
            cstr!("load_executable"),
            3702,
        );
        return 1;
    }
    let image_size: usize = size - header_size;
    let load_base = virtual_memory.add(((load_seg as u32) << 4) as usize);
    {
        let blk_paras = (image_size + 15) / 16 + min_alloc as usize;
        if load_seg as u32 + blk_paras as u32 > CONVENTIONAL_TOP_SEG as u32 {
            shim_log_stdout(
                cstr!(
                    "Trace: load_executable %s at seg 0x%04X crosses the 0xA000 ceiling; failing\n"
                ),
                path,
                load_seg as c_uint,
            );
            libc::free(buf as *mut c_void);
            libc::fclose(f);
            critical_section_exit(
                cstr!("load_executable"),
                SHIMS_FILE,
                cstr!("load_executable"),
                3715,
            );
            return 1;
        }
    }
    libc::memcpy(
        load_base as *mut c_void,
        buf.add(header_size) as *const c_void,
        image_size,
    );
    shim_log_file_load(path, load_base as *const c_void, image_size, 0);
    shim_jit_invalidate_code_range_force(((load_seg as u32) << 4) as u32, image_size as u32);

    let file_paras: usize = (image_size + 15) / 16;
    let total_paras: usize = file_paras + min_alloc as usize;
    let alloc_bytes: usize = total_paras * 16;
    if alloc_bytes > image_size {
        libc::memset(
            load_base.add(image_size) as *mut c_void,
            0,
            alloc_bytes - image_size,
        );
    }
    if is_child == 0 {
        let mut min_block_paras: u32 =
            (psp_seg.wrapping_add(0x10) as u32).wrapping_sub(psp_seg as u32);
        if file_paras > 0xFFFF {
            min_block_paras = 0xFFFF;
        } else {
            min_block_paras += file_paras as u32;
            if min_block_paras > 0xFFFF {
                min_block_paras = 0xFFFF;
            }
        }
        program_min_block_paras = min_block_paras as u16;
        next_free_seg = psp_seg.wrapping_add(0x10).wrapping_add(total_paras as u16);
    } else {
        next_free_seg = load_seg.wrapping_add(total_paras as u16);
    }

    for i in 0..reloc_count {
        let entry_off = reloc_off as usize + i as usize * 4;
        if entry_off + 3 < size {
            let off: u16 = *buf.add(entry_off) as u16 | ((*buf.add(entry_off + 1) as u16) << 8);
            let seg: u16 = *buf.add(entry_off + 2) as u16 | ((*buf.add(entry_off + 3) as u16) << 8);
            let addr: u32 = ((seg as u32) << 4) + off as u32;
            if addr + 2 <= alloc_bytes as u32 {
                let pp = load_base.add(addr as usize) as *mut u16;
                let v = pp.read_unaligned();
                pp.write_unaligned(v.wrapping_add(load_seg));
            }
        }
    }

    libc::free(buf as *mut c_void);
    libc::fclose(f);
    critical_section_exit(
        cstr!("load_executable"),
        SHIMS_FILE,
        cstr!("load_executable"),
        3775,
    );

    if !out_cs.is_null() {
        *out_cs = load_seg.wrapping_add(e_cs);
    }
    if !out_ip.is_null() {
        *out_ip = e_ip;
    }
    if !out_ss.is_null() {
        *out_ss = load_seg.wrapping_add(e_ss);
    }
    if !out_sp.is_null() {
        *out_sp = e_sp;
    }
    shim_log_stdout(cstr!("Trace: load_executable loaded %s\n"), path);
    0
}

#[no_mangle]
pub unsafe extern "C" fn load_overlay(
    path: *const c_char,
    load_seg: u16,
    reloc_factor: u16,
) -> c_int {
    critical_section_enter(
        cstr!("load_overlay"),
        SHIMS_FILE,
        cstr!("load_overlay"),
        3803,
    );
    let f = fopen_case_insensitive(path, cstr!("rb"));
    if f.is_null() {
        shim_log_stdout(cstr!("Trace: load_overlay failed to open %s\n"), path);
        critical_section_exit(
            cstr!("load_overlay"),
            SHIMS_FILE,
            cstr!("load_overlay"),
            3807,
        );
        return 1;
    }
    let mut header = [0u8; 28];
    let hdr_read = libc::fread(header.as_mut_ptr() as *mut c_void, 1, header.len(), f);
    let mut size: usize = 0;
    if hdr_read >= 6 {
        let e_cblp: u16 = header[2] as u16 | ((header[3] as u16) << 8);
        let e_cp: u16 = header[4] as u16 | ((header[5] as u16) << 8);
        if e_cp > 0 {
            size = ((e_cp - 1) as usize * 512) + (if e_cblp != 0 { e_cblp as usize } else { 512 });
        }
    }
    if size == 0 && libc::fseek(f, 0, libc::SEEK_END) == 0 {
        let actual = libc::ftell(f);
        if actual > 0 {
            size = actual as usize;
        }
    }
    let buf = if size != 0 {
        libc::malloc(size) as *mut u8
    } else {
        ptr::null_mut()
    };
    if buf.is_null()
        || libc::fseek(f, 0, libc::SEEK_SET) != 0
        || libc::fread(buf as *mut c_void, 1, size, f) != size
    {
        libc::free(buf as *mut c_void);
        libc::fclose(f);
        shim_log_stdout(cstr!("Trace: load_overlay failed to read %s\n"), path);
        critical_section_exit(
            cstr!("load_overlay"),
            SHIMS_FILE,
            cstr!("load_overlay"),
            3828,
        );
        return 1;
    }
    let header_paras: u16 = if size >= 10 {
        *buf.add(8) as u16 | ((*buf.add(9) as u16) << 8)
    } else {
        0
    };
    let reloc_count: u16 = if size >= 8 {
        *buf.add(6) as u16 | ((*buf.add(7) as u16) << 8)
    } else {
        0
    };
    let reloc_off: u16 = if size >= 26 {
        *buf.add(24) as u16 | ((*buf.add(25) as u16) << 8)
    } else {
        0
    };
    let header_size: usize = header_paras as usize * 16;
    if size <= header_size {
        libc::free(buf as *mut c_void);
        libc::fclose(f);
        critical_section_exit(
            cstr!("load_overlay"),
            SHIMS_FILE,
            cstr!("load_overlay"),
            3838,
        );
        return 1;
    }
    let image_size: usize = size - header_size;
    let base: u32 = (load_seg as u32) << 4;
    if base as usize + image_size > MEMORY_SIZE {
        libc::free(buf as *mut c_void);
        libc::fclose(f);
        critical_section_exit(
            cstr!("load_overlay"),
            SHIMS_FILE,
            cstr!("load_overlay"),
            3846,
        );
        return 1;
    }
    shim_jit_invalidate_code_range_force(base, image_size as u32);
    let dst = virtual_memory.add(base as usize);
    libc::memcpy(
        dst as *mut c_void,
        buf.add(header_size) as *const c_void,
        image_size,
    );
    for i in 0..reloc_count {
        let entry_off = reloc_off as usize + i as usize * 4;
        if entry_off + 3 < size {
            let off: u16 = *buf.add(entry_off) as u16 | ((*buf.add(entry_off + 1) as u16) << 8);
            let seg: u16 = *buf.add(entry_off + 2) as u16 | ((*buf.add(entry_off + 3) as u16) << 8);
            let addr: u32 = ((seg as u32) << 4) + off as u32;
            if addr + 2 <= image_size as u32 {
                let pp = dst.add(addr as usize) as *mut u16;
                let v = pp.read_unaligned();
                pp.write_unaligned(v.wrapping_add(reloc_factor));
            }
        }
    }
    libc::free(buf as *mut c_void);
    libc::fclose(f);
    shim_log_file_load(path, dst as *const c_void, image_size, 0);
    shim_log_stdout(
        cstr!("Trace: load_overlay %s at seg 0x%04X reloc 0x%04X (%zu bytes)\n"),
        path,
        load_seg as c_uint,
        reloc_factor as c_uint,
        image_size,
    );
    critical_section_exit(
        cstr!("load_overlay"),
        SHIMS_FILE,
        cstr!("load_overlay"),
        3874,
    );
    0
}

#[no_mangle]
pub unsafe extern "C" fn mask_addr(addr: u32) -> u32 {
    if a20_enabled {
        addr & MEMORY_MASK
    } else {
        addr & 0xFFFFF
    }
}

#[no_mangle]
pub unsafe extern "C" fn wrap_segoff_addr(base: u32, offset: u32) -> u32 {
    base + offset
}

#[no_mangle]
pub unsafe extern "C" fn file_mapping_swap_impl(
    seg_a: u16,
    off_a: u16,
    seg_b: u16,
    off_b: u16,
    len: usize,
    file: *const c_char,
    func: *const c_char,
    line: c_int,
) {
    let addr_a = linear_addr(seg_a, off_a);
    let addr_b = linear_addr(seg_b, off_b);
    shim_log_stdout(
        cstr!("Trace: file_mapping_swap 0x%05X-0x%05X <-> 0x%05X-0x%05X (%s:%s:%d)\n"),
        addr_a as c_uint,
        (addr_a + len as u32) as c_uint,
        addr_b as c_uint,
        (addr_b + len as u32) as c_uint,
        file,
        func,
        line,
    );

    let m_a = find_file_mapping_mut(addr_a);
    let m_b = find_file_mapping_mut(addr_b);
    let exact_a = !m_a.is_null() && (*m_a).base == addr_a && (*m_a).len == len;
    let exact_b = !m_b.is_null() && (*m_b).base == addr_b && (*m_b).len == len;

    if exact_a {
        (*m_a).base = addr_b;
    } else if !m_a.is_null() {
        shim_log_stdout(
            cstr!("Trace: file_mapping_swap skipped rebasing %s at 0x%05X (len 0x%zX); requested subrange 0x%05X-0x%05X\n"),
            if !(*m_a).path.is_null() { (*m_a).path } else { cstr!("<unknown>") },
            (*m_a).base as c_uint,
            (*m_a).len,
            addr_a as c_uint,
            (addr_a + len as u32) as c_uint,
        );
    }

    if exact_b {
        (*m_b).base = addr_a;
    } else if !m_b.is_null() {
        shim_log_stdout(
            cstr!("Trace: file_mapping_swap skipped rebasing %s at 0x%05X (len 0x%zX); requested subrange 0x%05X-0x%05X\n"),
            if !(*m_b).path.is_null() { (*m_b).path } else { cstr!("<unknown>") },
            (*m_b).base as c_uint,
            (*m_b).len,
            addr_b as c_uint,
            (addr_b + len as u32) as c_uint,
        );
    }
}

#[no_mangle]
/// A byte of guest physical memory. The DMA controller addresses memory linearly
/// — it has no segments and no CPU to go through — so it needs this rather than
/// the seg:off accessors everything else uses.
pub unsafe fn phys_read_byte(addr: u32) -> u8 {
    if virtual_memory.is_null() {
        return 0;
    }
    *virtual_memory.add(mask_addr(addr) as usize)
}

#[cfg(test)]
pub unsafe fn phys_write_byte(addr: u32, value: u8) {
    *virtual_memory.add(mask_addr(addr) as usize) = value;
}

/// Guest memory, for device tests that touch it (the DMA controller reads it) but
/// do not boot a machine to get it.
#[cfg(test)]
pub unsafe fn shim_test_init_memory() {
    if virtual_memory.is_null() {
        virtual_memory = libc::calloc(MEMORY_SIZE, 1) as *mut u8;
    }
}

#[no_mangle]
pub unsafe extern "C" fn memw_raw_read(seg: u16, off: u16) -> u16 {
    let addr = linear_addr(seg, off);
    (*virtual_memory.add(addr as usize) as u16)
        | ((*virtual_memory.add(mask_addr(addr + 1) as usize) as u16) << 8)
}

#[no_mangle]
pub unsafe extern "C" fn memw_raw_write(seg: u16, off: u16, value: u16) {
    let addr = linear_addr(seg, off);
    *virtual_memory.add(addr as usize) = (value & 0xFF) as u8;
    *virtual_memory.add(mask_addr(addr + 1) as usize) = (value >> 8) as u8;
}

// ============================================================================
// Stack-write watcher ring  [C lines 3965-4047]
// ============================================================================

const SWO_PUSH: u8 = 1;
const SWO_POP: u8 = 2;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct StackWriteEvent {
    t_us: u32,
    writer_cs: u16,
    writer_ip: u16,
    target_ss: u16,
    target_off: u16,
    value: u16,
    sp_at_op: u16,
    kind: u8,
    isr_depth_at: u8,
    lcall_depth_at: u8,
    reserved: u8,
    file: *const c_char,
    line: u32,
}
impl StackWriteEvent {
    const ZERO: StackWriteEvent = StackWriteEvent {
        t_us: 0,
        writer_cs: 0,
        writer_ip: 0,
        target_ss: 0,
        target_off: 0,
        value: 0,
        sp_at_op: 0,
        kind: 0,
        isr_depth_at: 0,
        lcall_depth_at: 0,
        reserved: 0,
        file: ptr::null(),
        line: 0,
    };
}
const STACK_WRITE_RING_BITS: u32 = 11;
const STACK_WRITE_RING_SIZE: u32 = 1u32 << STACK_WRITE_RING_BITS;
const STACK_WRITE_RING_MASK: u32 = STACK_WRITE_RING_SIZE - 1;
// Exported so the chunk prelude's SS-segment fast path can append push/pop
// events inline (saisei_rt.rs mirrors StackWriteEvent's #[repr(C)] layout and
// the [-16, +256]-of-SP filter; the two must be edited together).
#[no_mangle]
pub static mut stack_write_ring: [StackWriteEvent; STACK_WRITE_RING_SIZE as usize] =
    [StackWriteEvent::ZERO; STACK_WRITE_RING_SIZE as usize];
#[no_mangle]
pub static mut stack_write_ring_pos: u32 = 0;

unsafe fn stack_op_record(
    kind: u8,
    seg: u16,
    off: u16,
    value: u16,
    file: *const c_char,
    line: c_int,
) {
    if seg != ss() {
        return;
    }
    let rel: u16 = off.wrapping_sub(sp());
    let rel_s = rel as i16;
    if rel_s < -16 || rel_s > 256 {
        return;
    }
    let idx = (stack_write_ring_pos & STACK_WRITE_RING_MASK) as usize;
    stack_write_ring_pos = stack_write_ring_pos.wrapping_add(1);
    let e = &mut stack_write_ring[idx];
    e.t_us = lifecycle_elapsed_us() as u32;
    e.writer_cs = cs();
    e.writer_ip = ip();
    e.target_ss = seg;
    e.target_off = off;
    e.value = value;
    e.sp_at_op = sp();
    e.kind = kind;
    e.isr_depth_at = isr_depth;
    e.lcall_depth_at = lcall_depth;
    e.file = file;
    e.line = line as u32;
}

unsafe fn stack_writes_dump_to_dir(dir: *const c_char) {
    if dir.is_null() {
        return;
    }
    let mut path = [0u8; 320];
    libc::snprintf(
        path.as_mut_ptr() as *mut c_char,
        path.len(),
        cstr!("%s/stack_writes.log"),
        dir,
    );
    let f = libc::fopen(path.as_ptr() as *const c_char, cstr!("w"));
    if f.is_null() {
        return;
    }
    libc::fprintf(
        f,
        cstr!("# Per-stack-cell write/read ring. Newest last.\n# Columns: t_us kind cs:ip ss:off val sp_at_op rel(sp) isr lcall  source\n# Use to find the push that wrote the value a later ret popped.\n# Filter by 'off=XXXX' to see the history of a single slot.\n"),
    );
    let n = if stack_write_ring_pos < STACK_WRITE_RING_SIZE {
        stack_write_ring_pos
    } else {
        STACK_WRITE_RING_SIZE
    };
    let start = if stack_write_ring_pos >= STACK_WRITE_RING_SIZE {
        stack_write_ring_pos & STACK_WRITE_RING_MASK
    } else {
        0
    };
    for i in 0..n {
        let idx = ((start + i) & STACK_WRITE_RING_MASK) as usize;
        let e = &stack_write_ring[idx];
        let rel = e.target_off.wrapping_sub(e.sp_at_op) as i16;
        let kn: *const c_char = if e.kind == SWO_PUSH {
            cstr!("PUSH")
        } else if e.kind == SWO_POP {
            cstr!("POP ")
        } else {
            cstr!("?   ")
        };
        let fbase0 = if !e.file.is_null() {
            libc::strrchr(e.file, b'/' as c_int)
        } else {
            ptr::null_mut()
        };
        let fbase = if !fbase0.is_null() {
            fbase0.add(1) as *const c_char
        } else if !e.file.is_null() {
            e.file
        } else {
            cstr!("?")
        };
        libc::fprintf(
            f,
            cstr!("t=%-10u %s cs:ip=%04X:%04X ss:off=%04X:%04X val=%04X sp=%04X rel=%+5d isr=%u lcall=%u  %s:%u\n"),
            e.t_us as c_uint,
            kn,
            e.writer_cs as c_uint,
            e.writer_ip as c_uint,
            e.target_ss as c_uint,
            e.target_off as c_uint,
            e.value as c_uint,
            e.sp_at_op as c_uint,
            rel as c_int,
            e.isr_depth_at as c_uint,
            e.lcall_depth_at as c_uint,
            fbase,
            e.line as c_uint,
        );
    }
    libc::fclose(f);
}

// ============================================================================
// memw/memb read/write impls + write-watch + protected-slot tripwire + bookend
// [C lines 4049-4610]
// ============================================================================

#[no_mangle]
pub unsafe extern "C" fn memw_read_impl(
    seg: u16,
    off: u16,
    file: *const c_char,
    func: *const c_char,
    line: c_int,
) -> u16 {
    let addr = linear_addr(seg, off);
    let rcb_base = linear_addr(es(), 0xFF00);
    if seg == 0 && off < 0x10 {
        shim_log_stdout(
            cstr!("Warning: null pointer word write %04X:%04X (%s:%s:%d)\n"),
            seg as c_uint,
            off as c_uint,
            file,
            func,
            line,
        );
    }
    if seg == es() && addr >= rcb_base && addr < rcb_base + 0x100 {
        let field = (0xFF00 + (addr - rcb_base)) as c_int;
        return rcb_read16_impl(field, file, func, line);
    }
    let v = memw_raw_read(seg, off);
    stack_op_record(SWO_POP, seg, off, v, file, line);
    v
}

#[repr(C)]
#[derive(Clone, Copy)]
struct WriteWatch {
    lo: u32,
    hi: u32,
    name: *const c_char,
}
static mut write_watches: [WriteWatch; 1] = [WriteWatch {
    lo: 0xFFFFFFFF,
    hi: 0x0,
    name: ptr::null(),
}];
const write_watches_count: usize = 1;

static mut watchw_log_fp: *mut FILE = ptr::null_mut();

unsafe fn protected_slots_check_write(
    addr: u32,
    size: usize,
    value: u32,
    file: *const c_char,
    func: *const c_char,
    line: c_int,
) {
    if cfg().protected_slot_count == 0 {
        return;
    }
    if cs() == cfg().init_cs {
        return;
    }
    for i in 0..cfg().protected_slot_count {
        let slot_lo = (*cfg().protected_slots.add(i)).lo;
        let slot_hi = (*cfg().protected_slots.add(i)).hi;
        if addr + size as u32 <= slot_lo || addr > slot_hi {
            continue;
        }
        let mut msg = [0u8; 768];
        libc::snprintf(
            msg.as_mut_ptr() as *mut c_char,
            msg.len(),
            cstr!("[RCB OVERWRITE] post-init write into protected slot %s @ 0x%05X size=%zu val=0x%X\n  cs:ip=%04X:%04X active=%s ds=%04X es=%04X ax=%04X bx=%04X cx=%04X dx=%04X si=%04X di=%04X bp=%04X ss:sp=%04X:%04X\n  via %s:%s:%d\n  diagnosis: this slot holds an indirect ljmp/lcall target the game reads every timer tick. A write here from non-init code is a buffer overrun stomping the dispatch table; the next dispatch through the slot would land at a bogus target. The cs:ip above is the instruction whose write overflowed.\n"),
            (*cfg().protected_slots.add(i)).name,
            addr as c_uint,
            size,
            value as c_uint,
            cs() as c_uint,
            ip() as c_uint,
            if shim_active_binary().is_null() { cstr!("<none>") } else { shim_active_binary() },
            ds() as c_uint,
            es() as c_uint,
            ax() as c_uint,
            bx() as c_uint,
            cx() as c_uint,
            dx() as c_uint,
            si() as c_uint,
            di() as c_uint,
            bp() as c_uint,
            ss() as c_uint,
            sp() as c_uint,
            if file.is_null() { cstr!("?") } else { file },
            if func.is_null() { cstr!("?") } else { func },
            line,
        );
        libc::fprintf(stderr, cstr!("%s"), msg.as_ptr() as *const c_char);
        shim_log_crash(cstr!("%s"), msg.as_ptr() as *const c_char);
        save_bug_bundle(cstr!("rcb_overwrite"), addr, msg.as_ptr() as *const c_char);
        shim_flush_all_streams();
        libc::abort();
    }
}

#[no_mangle]
pub unsafe extern "C" fn shim_protected_slots_check() {}

static mut bookend_active: c_int = 0;
static mut bookend_log_fp: *mut FILE = ptr::null_mut();
static mut bookend_writes_logged: u64 = 0;
static mut bookend_writes_skipped: u64 = 0;

unsafe fn bookend_dump_snapshot(path: *const c_char) {
    let f = libc::fopen(path, cstr!("wb"));
    if f.is_null() {
        shim_log_stderr(
            cstr!("Bookend: failed to open %s: %s\n"),
            path,
            libc::strerror(*libc::__errno_location()),
        );
        return;
    }
    let w = libc::fwrite(virtual_memory as *const c_void, 1, MEMORY_SIZE, f);
    libc::fclose(f);
    shim_log_stdout(cstr!("Bookend: wrote %zu bytes to %s\n"), w, path);
}

#[no_mangle]
pub unsafe extern "C" fn shim_bookend_start() {
    if bookend_active != 0 {
        shim_log_stdout(cstr!("Bookend: already active, ignoring start\n"));
        return;
    }
    bookend_dump_snapshot(cstr!("/tmp/zbookend_snap1.bin"));
    bookend_log_fp = libc::fopen(cstr!("/tmp/zbookend.log"), cstr!("w"));
    if !bookend_log_fp.is_null() {
        libc::setvbuf(bookend_log_fp, ptr::null_mut(), libc::_IOLBF, 0);
        libc::fprintf(
            bookend_log_fp,
            cstr!("# bookend START cs:ip=%04X:%04X ds=%04X es=%04X ss:sp=%04X:%04X active=%s\n"),
            cs() as c_uint,
            ip() as c_uint,
            ds() as c_uint,
            es() as c_uint,
            ss() as c_uint,
            sp() as c_uint,
            if shim_active_binary().is_null() {
                cstr!("<none>")
            } else {
                shim_active_binary()
            },
        );
    } else {
        shim_log_stderr(
            cstr!("Bookend: failed to open /tmp/zbookend.log: %s\n"),
            libc::strerror(*libc::__errno_location()),
        );
    }
    bookend_writes_logged = 0;
    bookend_writes_skipped = 0;
    bookend_active = 1;
    mem_page_flags_recompute(); // capture mode: route every write through the impl
    shim_log_stdout(cstr!("Bookend: START\n"));
}

#[no_mangle]
pub unsafe extern "C" fn shim_bookend_stop() {
    if bookend_active == 0 {
        shim_log_stdout(cstr!("Bookend: not active, ignoring stop\n"));
        return;
    }
    bookend_active = 0;
    mem_page_flags_recompute();
    bookend_dump_snapshot(cstr!("/tmp/zbookend_snap2.bin"));
    if !bookend_log_fp.is_null() {
        libc::fprintf(
            bookend_log_fp,
            cstr!("# bookend STOP cs:ip=%04X:%04X ds=%04X es=%04X ss:sp=%04X:%04X logged=%llu skipped=%llu\n"),
            cs() as c_uint,
            ip() as c_uint,
            ds() as c_uint,
            es() as c_uint,
            ss() as c_uint,
            sp() as c_uint,
            bookend_writes_logged as c_ulonglong,
            bookend_writes_skipped as c_ulonglong,
        );
        libc::fclose(bookend_log_fp);
        bookend_log_fp = ptr::null_mut();
    }
    shim_log_stdout(
        cstr!("Bookend: STOP  logged=%llu skipped=%llu\n  diff: saisei zbookend-diff /tmp/zbookend_snap1.bin /tmp/zbookend_snap2.bin\n  log:  /tmp/zbookend.log\n"),
        bookend_writes_logged as c_ulonglong,
        bookend_writes_skipped as c_ulonglong,
    );
}

unsafe fn bookend_log_write(
    seg: u16,
    off: u16,
    addr: u32,
    size: usize,
    value: u32,
    file: *const c_char,
    func: *const c_char,
    line: c_int,
) {
    if bookend_active == 0 || bookend_log_fp.is_null() {
        return;
    }
    let is_push = seg == ss() && off == sp();
    if is_push || (addr >= 0xA0000 && addr < 0xC0000) {
        bookend_writes_skipped += 1;
        return;
    }
    libc::fprintf(
        bookend_log_fp,
        cstr!("W %05X size=%zu val=0x%X seg:off=%04X:%04X cs:ip=%04X:%04X ds=%04X es=%04X bx=%04X si=%04X di=%04X ax=%04X (%s:%s:%d)\n"),
        addr as c_uint,
        size,
        value as c_uint,
        seg as c_uint,
        off as c_uint,
        cs() as c_uint,
        ip() as c_uint,
        ds() as c_uint,
        es() as c_uint,
        bx() as c_uint,
        si() as c_uint,
        di() as c_uint,
        ax() as c_uint,
        if file.is_null() { cstr!("?") } else { file },
        if func.is_null() { cstr!("?") } else { func },
        line,
    );
    bookend_writes_logged += 1;
}

// ---- write fast-path page flags ---------------------------------------------
//
// One byte per 4KB page of guest memory. A zero flag means a chunk's inline
// `memb_write`/`memw_write` (saisei_rt.rs) may store directly; a nonzero flag
// routes the write through `mem*_write_impl` so every special behavior —
// JIT'd-code invalidation, write watches / annotation vars, protected slots,
// the null-page warning, .drv cross-binary overwrite detection, bookend
// capture — runs exactly as before. Flags only ever err toward the slow path:
// a stale flag costs a few ns, a missing one would drop a behavior, so every
// event that could grow the special set (chunk registration, file-mapping
// registration, bookend start) triggers a recompute.
#[no_mangle]
pub static mut mem_page_flags: [u8; MEMORY_SIZE >> 12] = [0; MEMORY_SIZE >> 12];

static mut watch_vars_init: c_int = 0;
static mut watch_vars_resolved_at: usize = usize::MAX;

unsafe fn watch_ranges_prepare() {
    if watch_vars_init == 0 {
        watch_vars_init = 1;
        aliasreg_vars_load();
    }
    if aliasreg_has_origin_vars != 0 && file_mapping_count != watch_vars_resolved_at {
        watch_vars_resolved_at = file_mapping_count;
        aliasreg_vars_resolve();
    }
}

unsafe fn mem_page_flag_range(lo: u32, hi_exclusive: u32) {
    if hi_exclusive <= lo {
        return;
    }
    let first = (lo >> 12) as usize;
    let last = ((hi_exclusive - 1) >> 12) as usize;
    for p in first..=last.min((MEMORY_SIZE >> 12) - 1) {
        mem_page_flags[p] = 1;
    }
}

#[no_mangle]
pub unsafe extern "C" fn mem_page_flags_recompute() {
    if bookend_active != 0 {
        // capture-everything mode: every write goes through the impl
        for p in (*ptr::addr_of_mut!(mem_page_flags)).iter_mut() {
            *p = 1;
        }
        return;
    }
    for p in (*ptr::addr_of_mut!(mem_page_flags)).iter_mut() {
        *p = 0;
    }
    // null-pointer page (the 0000:0000..000F warning lives in the impl)
    mem_page_flags[0] = 1;
    // live JIT chunk code ranges (self-modifying-code invalidation)
    for i in 0..jit_chunk_count {
        let c = &jit_chunks[i];
        if c.stale == 0 {
            mem_page_flag_range(c.seg_base + c.lo, c.seg_base + c.hi);
        }
    }
    // .drv file mappings (cross-binary overwrite abort in warn_on_mutation)
    for i in 0..file_mapping_count {
        let path = file_mappings[i].path;
        if !path.is_null() {
            let plen = libc::strlen(path);
            if plen >= 4 && libc::strcmp(path.add(plen - 4), cstr!(".drv")) == 0 {
                mem_page_flag_range(
                    file_mappings[i].base,
                    file_mappings[i].base + file_mappings[i].len as u32,
                );
            }
        }
    }
    // write watches + annotation vars
    watch_ranges_prepare();
    for i in 0..write_watches_count {
        if write_watches[i].lo <= write_watches[i].hi {
            mem_page_flag_range(write_watches[i].lo, write_watches[i].hi + 1);
        }
    }
    if aliasreg_var_lo <= aliasreg_var_hi {
        mem_page_flag_range(aliasreg_var_lo, aliasreg_var_hi.saturating_add(1));
    }
    // protected slots
    for i in 0..cfg().protected_slot_count {
        let lo = (*cfg().protected_slots.add(i)).lo;
        let hi = (*cfg().protected_slots.add(i)).hi;
        mem_page_flag_range(lo, hi.saturating_add(1));
    }
}

#[no_mangle]
pub unsafe extern "C" fn write_watch_log(
    addr: u32,
    size: usize,
    value: u32,
    file: *const c_char,
    func: *const c_char,
    line: c_int,
) {
    watch_ranges_prepare();
    if addr >= aliasreg_var_lo && addr <= aliasreg_var_hi {
        aliasreg_var_write(addr, size as u8, value);
    }
    for i in 0..write_watches_count {
        if addr + size as u32 > write_watches[i].lo && addr <= write_watches[i].hi {
            let mut tgtbuf = [0u8; 64];
            let mut srcbuf = [0u8; 64];
            name_addr(addr, tgtbuf.as_mut_ptr() as *mut c_char, tgtbuf.len());
            name_addr(
                ((ds() as u32) << 4) + si() as u32,
                srcbuf.as_mut_ptr() as *mut c_char,
                srcbuf.len(),
            );
            lifecycle_log(
                cstr!("WATCHW [%s] @ %s size=%zu val=0x%X  src=%s  cs:ip=%04X:%04X  es=%04X bx=%04X di=%04X ax=%04X (%s:%s:%d)\n"),
                write_watches[i].name,
                tgtbuf.as_ptr() as *const c_char,
                size,
                value as c_uint,
                srcbuf.as_ptr() as *const c_char,
                cs() as c_uint,
                ip() as c_uint,
                es() as c_uint,
                bx() as c_uint,
                di() as c_uint,
                ax() as c_uint,
                if file.is_null() { cstr!("?") } else { file },
                if func.is_null() { cstr!("?") } else { func },
                line,
            );
            if watchw_log_fp.is_null() {
                watchw_log_fp = libc::fopen(cstr!("watchw.log"), cstr!("w"));
                if !watchw_log_fp.is_null() {
                    libc::setvbuf(watchw_log_fp, ptr::null_mut(), libc::_IOLBF, 0);
                }
            }
            if !watchw_log_fp.is_null() {
                libc::fprintf(
                    watchw_log_fp,
                    cstr!("WATCHW [%s] @ %s size=%zu val=0x%X  src=%s  cs:ip=%04X:%04X  es=%04X bx=%04X di=%04X ax=%04X (%s:%s:%d)\n"),
                    write_watches[i].name,
                    tgtbuf.as_ptr() as *const c_char,
                    size,
                    value as c_uint,
                    srcbuf.as_ptr() as *const c_char,
                    cs() as c_uint,
                    ip() as c_uint,
                    es() as c_uint,
                    bx() as c_uint,
                    di() as c_uint,
                    ax() as c_uint,
                    if file.is_null() { cstr!("?") } else { file },
                    if func.is_null() { cstr!("?") } else { func },
                    line,
                );
            }
            protected_slots_check_write(addr, size, value, file, func, line);
            break;
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn memw_write_impl(
    seg: u16,
    off: u16,
    value: u16,
    file: *const c_char,
    func: *const c_char,
    line: c_int,
) {
    let addr = linear_addr(seg, off);
    let rcb_base = linear_addr(es(), 0xFF00);
    if seg == 0 && off < 0x10 {
        shim_log_stdout(
            cstr!("Warning: null pointer byte write %04X:%04X (%s:%s:%d)\n"),
            seg as c_uint,
            off as c_uint,
            file,
            func,
            line,
        );
    }
    if seg == es() && addr >= rcb_base && addr < rcb_base + 0x100 {
        let field = (0xFF00 + (addr - rcb_base)) as c_int;
        rcb_write16_impl(field, value, file, func, line);
        return;
    }
    stack_op_record(SWO_PUSH, seg, off, value, file, line);
    // The special write behaviors (bookend capture, watches/annotation vars,
    // .drv cross-binary tripwire, protected slots, JIT'd-code invalidation)
    // all live on flagged pages — mem_page_flags_recompute marks every range
    // they can match. An unflagged page write (the overwhelmingly common
    // case: runtime-internal stack pushes from lcall/call_table) is a plain
    // store, same as the chunk-inline fast path already treats byte writes.
    let addr_hi = mask_addr(addr.wrapping_add(1));
    if mem_page_flags[(addr >> 12) as usize] != 0 || mem_page_flags[(addr_hi >> 12) as usize] != 0 {
        bookend_log_write(seg, off, addr, 2, value as u32, file, func, line);
        write_watch_log(addr, 2, value as u32, file, func, line);
        warn_on_mutation(addr, 2, file, func, line);
        memw_raw_write(seg, off, value);
        shim_jit_invalidate_code_range(addr, 2);
        return;
    }
    memw_raw_write(seg, off, value);
}

#[no_mangle]
pub unsafe extern "C" fn memb_read_impl(
    seg: u16,
    off: u16,
    file: *const c_char,
    func: *const c_char,
    line: c_int,
) -> u8 {
    let addr = linear_addr(seg, off);
    let rcb_base = linear_addr(es(), 0xFF00);
    if seg == es() && addr >= rcb_base && addr < rcb_base + 0x100 {
        let field = (0xFF00 + (addr - rcb_base)) as c_int;
        return rcb_read8_impl(field, file, func, line);
    }
    *seg_off(seg, off)
}

#[no_mangle]
pub unsafe extern "C" fn memb_write_impl(
    seg: u16,
    off: u16,
    value: u8,
    file: *const c_char,
    func: *const c_char,
    line: c_int,
) {
    let addr = linear_addr(seg, off);
    let rcb_base = linear_addr(es(), 0xFF00);
    if seg == es() && addr >= rcb_base && addr < rcb_base + 0x100 {
        let field = (0xFF00 + (addr - rcb_base)) as c_int;
        rcb_write8_impl(field, value, file, func, line);
        return;
    }
    // Flag-gated like memw_write_impl: unflagged pages carry no special write
    // behavior (see the comment there).
    if mem_page_flags[(addr >> 12) as usize] != 0 {
        bookend_log_write(seg, off, addr, 1, value as u32, file, func, line);
        write_watch_log(addr, 1, value as u32, file, func, line);
        warn_on_mutation(addr, 1, file, func, line);
        *seg_off(seg, off) = value;
        shim_jit_invalidate_code_range(addr, 1);
        return;
    }
    *seg_off(seg, off) = value;
}

unsafe fn rep_range_touches_rcb(seg: u16, lo: u32, len: u32) -> c_int {
    if seg != es() {
        return 0;
    }
    let rcb_lo = ((es() as u32) << 4) + 0xFF00;
    let rcb_hi = rcb_lo + 0x100;
    (lo < rcb_hi && lo + len > rcb_lo) as c_int
}

unsafe fn rep_range_touches_watch(lo: u32, len: u32) -> c_int {
    for i in 0..write_watches_count {
        if lo < write_watches[i].hi + 1 && lo + len > write_watches[i].lo {
            return 1;
        }
    }
    0
}

unsafe fn rep_would_wrap(off: u16, count: u32, direction: c_int) -> c_int {
    if direction > 0 {
        (off as u32 + count > 0x10000) as c_int
    } else {
        (count > off as u32 + 1) as c_int
    }
}

unsafe fn maybe_register_relocation_shadow(src_lo: u32, dst_lo: u32, count: u32) {
    if count < 256 || src_lo == dst_lo {
        return;
    }
    let mut src_fm: *const FileMapping = ptr::null();
    let bd = find_binary_for_addr(src_lo, &mut src_fm);
    if bd.is_null() || src_fm.is_null() || (*src_fm).path.is_null() {
        return;
    }
    let fm_lo = (*src_fm).base;
    let fm_hi = (*src_fm).base + (*src_fm).len as u32;
    if src_lo < fm_lo || src_lo + count > fm_hi {
        return;
    }
    if dst_lo >= fm_hi || dst_lo + count <= fm_lo {
        return;
    }
    let new_file_off = (*src_fm).file_offset + (src_lo - (*src_fm).base) as usize;
    for i in 0..file_mapping_count {
        if file_mappings[i].base == dst_lo
            && file_mappings[i].len == count as usize
            && file_mappings[i].file_offset == new_file_off
        {
            return;
        }
    }
    if file_mapping_count >= MAX_FILE_MAPPINGS {
        return;
    }
    let idx = file_mapping_count;
    file_mapping_count += 1;
    mem_page_flags_recompute();
    file_mappings[idx] = FileMapping::ZERO;
    let src_path = (*src_fm).path;
    let fm = &mut file_mappings[idx];
    fm.path = libc::strdup(src_path);
    fm.base = dst_lo;
    fm.len = count as usize;
    fm.file_offset = new_file_off;
    fm.data = ptr::null_mut();
    let logpath = file_mappings[idx].path;
    shim_log_stdout(
        cstr!("Trace: relocation shadow: %s 0x%X bytes 0x%05X -> 0x%05X (dispatch now follows the copy)\n"),
        if !logpath.is_null() { logpath } else { cstr!("?") },
        count as c_uint,
        src_lo as c_uint,
        dst_lo as c_uint,
    );
    lifecycle_log(
        cstr!("RELOC 0x%05X->0x%05X len 0x%X\n"),
        src_lo as c_uint,
        dst_lo as c_uint,
        count as c_uint,
    );
}

#[no_mangle]
pub unsafe extern "C" fn rep_movsb_block_impl(
    dst_seg: u16,
    src_seg: u16,
    file: *const c_char,
    func: *const c_char,
    line: c_int,
) {
    if cx() == 0 {
        return;
    }
    let count: u32 = cx() as u32;
    // The block copy stands in for `count` iterations of rep movsb: debit the
    // instruction budget so bulk copies consume game time like real ones.
    jit_instr_budget -= count as i64;
    let delta: c_int = if DF() != 0 { -1 } else { 1 };

    if rep_would_wrap(si(), count, delta) != 0 || rep_would_wrap(di(), count, delta) != 0 {
        while cx() != 0 {
            let b = memb_read_impl(src_seg, si(), file, func, line);
            memb_write_impl(dst_seg, di(), b, file, func, line);
            set_si((si() as i32 + delta) as u16);
            set_di((di() as i32 + delta) as u16);
            set_cx(cx().wrapping_sub(1));
        }
        return;
    }

    let src_first_off: u16 = if DF() != 0 {
        (si() as i32 - count as i32 + 1) as u16
    } else {
        si()
    };
    let dst_first_off: u16 = if DF() != 0 {
        (di() as i32 - count as i32 + 1) as u16
    } else {
        di()
    };
    let src_lo = linear_addr(src_seg, src_first_off);
    let dst_lo = linear_addr(dst_seg, dst_first_off);

    if rep_range_touches_rcb(dst_seg, dst_lo, count) != 0
        || rep_range_touches_rcb(src_seg, src_lo, count) != 0
        || rep_range_touches_watch(dst_lo, count) != 0
    {
        while cx() != 0 {
            let b = memb_read_impl(src_seg, si(), file, func, line);
            memb_write_impl(dst_seg, di(), b, file, func, line);
            set_si((si() as i32 + delta) as u16);
            set_di((di() as i32 + delta) as u16);
            set_cx(cx().wrapping_sub(1));
        }
        maybe_register_relocation_shadow(src_lo, dst_lo, count);
        shim_jit_invalidate_code_range(dst_lo, count);
        return;
    }

    if src_lo < dst_lo + count && dst_lo < src_lo + count {
        let mut s = linear_addr(src_seg, si());
        let mut d = linear_addr(dst_seg, di());
        for _i in 0..count {
            *virtual_memory.add(mask_addr(d) as usize) = *virtual_memory.add(mask_addr(s) as usize);
            s = (s as i64 + delta as i64) as u32;
            d = (d as i64 + delta as i64) as u32;
        }
    } else {
        libc::memmove(
            virtual_memory.add(dst_lo as usize) as *mut c_void,
            virtual_memory.add(src_lo as usize) as *const c_void,
            count as usize,
        );
    }
    warn_on_mutation(dst_lo, count as usize, file, func, line);
    maybe_register_relocation_shadow(src_lo, dst_lo, count);
    shim_jit_invalidate_code_range(dst_lo, count);

    set_si((si() as i32 + count as i32 * delta) as u16);
    set_di((di() as i32 + count as i32 * delta) as u16);
    set_cx(0);
}

#[no_mangle]
pub unsafe extern "C" fn rep_stosb_block_impl(
    dst_seg: u16,
    file: *const c_char,
    func: *const c_char,
    line: c_int,
) {
    if cx() == 0 {
        return;
    }
    let count: u32 = cx() as u32;
    jit_instr_budget -= count as i64; // count iterations of rep stosb
    let delta: c_int = if DF() != 0 { -1 } else { 1 };

    if rep_would_wrap(di(), count, delta) != 0 {
        while cx() != 0 {
            memb_write_impl(dst_seg, di(), al(), file, func, line);
            set_di((di() as i32 + delta) as u16);
            set_cx(cx().wrapping_sub(1));
        }
        return;
    }

    let dst_first_off: u16 = if DF() != 0 {
        (di() as i32 - count as i32 + 1) as u16
    } else {
        di()
    };
    let dst_lo = linear_addr(dst_seg, dst_first_off);

    if rep_range_touches_rcb(dst_seg, dst_lo, count) != 0
        || rep_range_touches_watch(dst_lo, count) != 0
    {
        while cx() != 0 {
            memb_write_impl(dst_seg, di(), al(), file, func, line);
            set_di((di() as i32 + delta) as u16);
            set_cx(cx().wrapping_sub(1));
        }
        shim_jit_invalidate_code_range(dst_lo, count);
        return;
    }

    libc::memset(
        virtual_memory.add(dst_lo as usize) as *mut c_void,
        al() as c_int,
        count as usize,
    );
    warn_on_mutation(dst_lo, count as usize, file, func, line);
    shim_jit_invalidate_code_range(dst_lo, count);

    set_di((di() as i32 + count as i32 * delta) as u16);
    set_cx(0);
}

#[no_mangle]
pub unsafe extern "C" fn rep_movsw_block_impl(
    dst_seg: u16,
    src_seg: u16,
    file: *const c_char,
    func: *const c_char,
    line: c_int,
) {
    if cx() == 0 {
        return;
    }
    let count_words: u32 = cx() as u32;
    jit_instr_budget -= count_words as i64; // count iterations of rep movsw
    let count_bytes: u32 = count_words * 2;
    let delta: c_int = if DF() != 0 { -2 } else { 2 };

    if rep_would_wrap(si(), count_bytes, delta) != 0
        || rep_would_wrap(di(), count_bytes, delta) != 0
    {
        while cx() != 0 {
            let w = memw_read_impl(src_seg, si(), file, func, line);
            memw_write_impl(dst_seg, di(), w, file, func, line);
            set_si((si() as i32 + delta) as u16);
            set_di((di() as i32 + delta) as u16);
            set_cx(cx().wrapping_sub(1));
        }
        return;
    }

    let src_first_off: u16 = if DF() != 0 {
        (si() as i32 - count_bytes as i32 + 2) as u16
    } else {
        si()
    };
    let dst_first_off: u16 = if DF() != 0 {
        (di() as i32 - count_bytes as i32 + 2) as u16
    } else {
        di()
    };
    let src_lo = linear_addr(src_seg, src_first_off);
    let dst_lo = linear_addr(dst_seg, dst_first_off);

    if rep_range_touches_rcb(dst_seg, dst_lo, count_bytes) != 0
        || rep_range_touches_rcb(src_seg, src_lo, count_bytes) != 0
        || rep_range_touches_watch(dst_lo, count_bytes) != 0
    {
        while cx() != 0 {
            let w = memw_read_impl(src_seg, si(), file, func, line);
            memw_write_impl(dst_seg, di(), w, file, func, line);
            set_si((si() as i32 + delta) as u16);
            set_di((di() as i32 + delta) as u16);
            set_cx(cx().wrapping_sub(1));
        }
        shim_jit_invalidate_code_range(dst_lo, count_bytes);
        return;
    }

    if src_lo < dst_lo + count_bytes && dst_lo < src_lo + count_bytes {
        let mut s = linear_addr(src_seg, si());
        let mut d = linear_addr(dst_seg, di());
        for _i in 0..count_words {
            let b0 = *virtual_memory.add(mask_addr(s) as usize);
            let b1 = *virtual_memory.add(mask_addr(s + 1) as usize);
            *virtual_memory.add(mask_addr(d) as usize) = b0;
            *virtual_memory.add(mask_addr(d + 1) as usize) = b1;
            s = (s as i64 + delta as i64) as u32;
            d = (d as i64 + delta as i64) as u32;
        }
    } else {
        libc::memmove(
            virtual_memory.add(dst_lo as usize) as *mut c_void,
            virtual_memory.add(src_lo as usize) as *const c_void,
            count_bytes as usize,
        );
    }
    warn_on_mutation(dst_lo, count_bytes as usize, file, func, line);
    shim_jit_invalidate_code_range(dst_lo, count_bytes);

    let d1: c_int = if DF() != 0 { -1 } else { 1 };
    set_si((si() as i32 + count_bytes as i32 * d1) as u16);
    set_di((di() as i32 + count_bytes as i32 * d1) as u16);
    set_cx(0);
}

// ============================================================================
// RCB field naming + overlap/mutation warnings + RCB accessors  [4612-4913]
// ============================================================================

unsafe fn rcb_field_name(field: c_int) -> *const c_char {
    match field {
        FIELD_1 => cstr!("FIELD_1"),
        PROGRAM_SEG => cstr!("PROGRAM_SEG"),
        PREV_TIMER_VECTOR_OFF => cstr!("PREV_TIMER_VECTOR_OFF"),
        PREV_TIMER_VECTOR_SEG => cstr!("PREV_TIMER_VECTOR_SEG"),
        FIELD_5 => cstr!("FIELD_5"),
        FIELD_6 => cstr!("FIELD_6"),
        JOYSTICK_FLAG => cstr!("JOYSTICK_FLAG"),
        FIELD_8 => cstr!("FIELD_8"),
        DATA_BUF1_OFF => cstr!("DATA_BUF1_OFF"),
        DATA_BUF1_SEG => cstr!("DATA_BUF1_SEG"),
        DATA_BUF2_OFF => cstr!("DATA_BUF2_OFF"),
        DATA_BUF2_SEG => cstr!("DATA_BUF2_SEG"),
        VIDEO_DRIVER_INDEX => cstr!("VIDEO_DRIVER_INDEX"),
        MUSIC_DRIVER_FLAG => cstr!("MUSIC_DRIVER_FLAG"),
        FIELD_15 => cstr!("FIELD_15"),
        FIELD_16 => cstr!("FIELD_16"),
        FIELD_17 => cstr!("FIELD_17"),
        FIELD_18 => cstr!("FIELD_18"),
        FIELD_19 => cstr!("FIELD_19"),
        FIELD_20 => cstr!("FIELD_20"),
        FIELD_21 => cstr!("FIELD_21"),
        FIELD_22 => cstr!("FIELD_22"),
        FIELD_23 => cstr!("FIELD_23"),
        DATA_BASE_SEG => cstr!("DATA_BASE_SEG"),
        FIELD_25 => cstr!("FIELD_25"),
        FIELD_26 => cstr!("FIELD_26"),
        FIELD_27 => cstr!("FIELD_27"),
        FIELD_28 => cstr!("FIELD_28"),
        FIELD_29 => cstr!("FIELD_29"),
        FIELD_30 => cstr!("FIELD_30"),
        FIELD_31 => cstr!("FIELD_31"),
        FIELD_32 => cstr!("FIELD_32"),
        FIELD_33 => cstr!("FIELD_33"),
        FIELD_34 => cstr!("FIELD_34"),
        FIELD_35 => cstr!("FIELD_35"),
        FIELD_36 => cstr!("FIELD_36"),
        FIELD_37 => cstr!("FIELD_37"),
        PREV_KEYBOARD_VECTOR_OFF => cstr!("PREV_KEYBOARD_VECTOR_OFF"),
        PREV_KEYBOARD_VECTOR_SEG => cstr!("PREV_KEYBOARD_VECTOR_SEG"),
        _ => cstr!("UNKNOWN"),
    }
}

unsafe fn warn_rcb_overlap(path: *const c_char, addr: *const c_void, len: usize) {
    let mut base: u32 = 0;
    let mut end: u32 = 0;
    if !try_memory_range(addr, len, &mut base, &mut end) {
        return;
    }
    let rcb_base = ((es() as u32) << 4) + 0xFF00;
    let rcb_end = rcb_base + 0x100;
    if end <= rcb_base || base >= rcb_end {
        return;
    }
    let fields: [(c_int, usize); 39] = [
        (FIELD_1, 2),
        (PROGRAM_SEG, 2),
        (PREV_TIMER_VECTOR_OFF, 2),
        (PREV_TIMER_VECTOR_SEG, 2),
        (FIELD_5, 1),
        (FIELD_6, 1),
        (JOYSTICK_FLAG, 1),
        (FIELD_8, 1),
        (DATA_BUF1_OFF, 2),
        (DATA_BUF1_SEG, 2),
        (DATA_BUF2_OFF, 2),
        (DATA_BUF2_SEG, 2),
        (VIDEO_DRIVER_INDEX, 1),
        (MUSIC_DRIVER_FLAG, 1),
        (FIELD_15, 1),
        (FIELD_16, 1),
        (FIELD_17, 2),
        (FIELD_18, 1),
        (FIELD_19, 1),
        (FIELD_20, 2),
        (FIELD_21, 1),
        (FIELD_22, 1),
        (FIELD_23, 1),
        (DATA_BASE_SEG, 2),
        (FIELD_25, 1),
        (FIELD_26, 1),
        (FIELD_27, 1),
        (FIELD_28, 1),
        (FIELD_29, 1),
        (FIELD_30, 1),
        (FIELD_31, 1),
        (FIELD_32, 1),
        (FIELD_33, 1),
        (FIELD_34, 1),
        (FIELD_35, 1),
        (FIELD_36, 1),
        (FIELD_37, 1),
        (PREV_KEYBOARD_VECTOR_OFF, 2),
        (PREV_KEYBOARD_VECTOR_SEG, 2),
    ];
    for i in 0..fields.len() {
        let field_addr = rcb_base + (fields[i].0 - 0xFF00) as u32;
        let field_end = field_addr + fields[i].1 as u32;
        if base < field_end && end > field_addr {
            shim_log_stdout(
                cstr!("Warning: file %s overwrote RCB field %s\n"),
                path,
                rcb_field_name(fields[i].0),
            );
        }
    }
}

unsafe fn warn_file_overlap(path: *const c_char, addr: *const c_void, len: usize) {
    let mut base: u32 = 0;
    let mut end: u32 = 0;
    if !try_memory_range(addr, len, &mut base, &mut end) {
        return;
    }
    for i in 0..file_mapping_count {
        let f_base = file_mappings[i].base;
        let f_end = f_base + file_mappings[i].len as u32;
        if base < f_end && end > f_base {
            let overlap_start = if base > f_base { base } else { f_base };
            let overlap_end = if end < f_end { end } else { f_end };
            shim_log_stdout(
                cstr!("WARNING: file %s overwrote %s at 0x%05X-0x%05X\n"),
                path,
                file_mappings[i].path,
                overlap_start as c_uint,
                overlap_end as c_uint,
            );
            let overlap_len = (overlap_end - overlap_start) as usize;
            let dump_len = if overlap_len > 10 { 10 } else { overlap_len };
            if !file_mappings[i].data.is_null() {
                let old_bytes = file_mappings[i].data.add((overlap_start - f_base) as usize);
                let new_bytes = (addr as *const u8).add((overlap_start - base) as usize);
                shim_log_stdout(cstr!("         old bytes:"));
                for j in 0..dump_len {
                    shim_log_stdout(cstr!(" %02X"), *old_bytes.add(j) as c_uint);
                }
                shim_log_stdout(cstr!("\n         new bytes:"));
                for j in 0..dump_len {
                    shim_log_stdout(cstr!(" %02X"), *new_bytes.add(j) as c_uint);
                }
                shim_log_stdout(cstr!("\n"));
            }
        }
    }
}

unsafe fn warn_on_mutation(
    addr: u32,
    size: usize,
    file: *const c_char,
    func: *const c_char,
    line: c_int,
) {
    let mut src_name: *const c_char = ptr::null();
    let mut src_name_len: usize = 0;
    if !file.is_null() {
        let slash = libc::strrchr(file, b'/' as c_int);
        src_name = if !slash.is_null() { slash.add(1) } else { file };
        let dot = libc::strrchr(src_name, b'.' as c_int);
        src_name_len = if !dot.is_null() {
            dot.offset_from(src_name) as usize
        } else {
            libc::strlen(src_name)
        };
    }
    if shim_input_phase_started == 0 {
        return;
    }
    for i in 0..file_mapping_count {
        let f_base = file_mappings[i].base;
        let f_end = f_base + file_mappings[i].len as u32;
        if addr >= f_end || addr + size as u32 <= f_base {
            continue;
        }
        let path = file_mappings[i].path;
        shim_log_stdout(
            cstr!("Warning: mutation of %s at 0x%05X (%s:%s:%d)\n"),
            path,
            addr as c_uint,
            file,
            func,
            line,
        );
        if src_name.is_null() || path.is_null() {
            continue;
        }
        let tgt_base0 = libc::strrchr(path, b'/' as c_int);
        let tgt_base = if !tgt_base0.is_null() {
            tgt_base0.add(1)
        } else {
            path
        };
        let tgt_dot = libc::strrchr(tgt_base, b'.' as c_int);
        let tgt_len = if !tgt_dot.is_null() {
            tgt_dot.offset_from(tgt_base) as usize
        } else {
            libc::strlen(tgt_base)
        };
        if src_name_len == tgt_len && libc::strncmp(src_name, tgt_base, tgt_len) == 0 {
            continue;
        }
        if !src_name.is_null() && src_name_len > 4 && libc::strncmp(src_name, cstr!("jit_"), 4) == 0
        {
            let chunk_seg = libc::strtoul(src_name.add(4), ptr::null_mut(), 16) as u32;
            if addr >= chunk_seg && addr < chunk_seg + 0x10000 {
                continue;
            }
        }
        let plen = libc::strlen(path);
        if plen < 4 || libc::strcmp(path.add(plen - 4), cstr!(".drv")) != 0 {
            continue;
        }
        if file_mappings[i].len <= 16 {
            continue;
        }
        let mut msg = [0u8; 640];
        libc::snprintf(
            msg.as_mut_ptr() as *mut c_char,
            msg.len(),
            cstr!("[CROSS-BINARY OVERWRITE] %.*s code wrote into %s @ 0x%05X size=%zu\n  cs:ip=%04X:%04X ds=%04X es=%04X ax=%04X bx=%04X cx=%04X dx=%04X si=%04X di=%04X bp=%04X ss:sp=%04X:%04X\n  via %s:%s:%d\n  diagnosis: code translated from %.*s wrote into %s's loaded region. Loaded binary regions are read-only from the outside; only the owning binary may mutate its own bytes. A cross-binary write is a buffer overrun stomping the target's code or data — the next dispatch through whatever the target holds at the corrupted offset will land at a bogus target. The cs:ip above is the instruction whose write overflowed.\n"),
            src_name_len as c_int,
            src_name,
            path,
            addr as c_uint,
            size,
            cs() as c_uint,
            ip() as c_uint,
            ds() as c_uint,
            es() as c_uint,
            ax() as c_uint,
            bx() as c_uint,
            cx() as c_uint,
            dx() as c_uint,
            si() as c_uint,
            di() as c_uint,
            bp() as c_uint,
            ss() as c_uint,
            sp() as c_uint,
            if file.is_null() { cstr!("?") } else { file },
            if func.is_null() { cstr!("?") } else { func },
            line,
            src_name_len as c_int,
            src_name,
            path,
        );
        libc::fprintf(stderr, cstr!("%s"), msg.as_ptr() as *const c_char);
        shim_log_crash(cstr!("%s"), msg.as_ptr() as *const c_char);
        save_bug_bundle(
            cstr!("cross_binary_overwrite"),
            addr,
            msg.as_ptr() as *const c_char,
        );
        shim_flush_all_streams();
        libc::abort();
    }
}

#[no_mangle]
pub unsafe extern "C" fn rcb_read8_impl(
    field: c_int,
    file: *const c_char,
    func: *const c_char,
    line: c_int,
) -> u8 {
    let value = *seg_off(es(), field as u16);
    shim_log_stdout(
        cstr!("Trace: rcb_read8 %s=0x%02X (%s:%s:%d)\n"),
        rcb_field_name(field),
        value as c_uint,
        file,
        func,
        line,
    );
    value
}

#[no_mangle]
pub unsafe extern "C" fn rcb_write8_impl(
    field: c_int,
    value: u8,
    file: *const c_char,
    func: *const c_char,
    line: c_int,
) {
    shim_log_stdout(
        cstr!("Trace: rcb_write8 %s=0x%02X (%s:%s:%d)\n"),
        rcb_field_name(field),
        value as c_uint,
        file,
        func,
        line,
    );
    write_watch_log(
        linear_addr(es(), field as u16),
        1,
        value as u32,
        file,
        func,
        line,
    );
    *seg_off(es(), field as u16) = value;
}

#[no_mangle]
pub unsafe extern "C" fn rcb_read16_impl(
    field: c_int,
    file: *const c_char,
    func: *const c_char,
    line: c_int,
) -> u16 {
    let value = memw_raw_read(es(), field as u16);
    shim_log_stdout(
        cstr!("Trace: rcb_read16 %s=0x%04X (%s:%s:%d)\n"),
        rcb_field_name(field),
        value as c_uint,
        file,
        func,
        line,
    );
    value
}

#[no_mangle]
pub unsafe extern "C" fn rcb_write16_impl(
    field: c_int,
    value: u16,
    file: *const c_char,
    func: *const c_char,
    line: c_int,
) {
    shim_log_stdout(
        cstr!("Trace: rcb_write16 %s=0x%04X (%s:%s:%d)\n"),
        rcb_field_name(field),
        value as c_uint,
        file,
        func,
        line,
    );
    write_watch_log(
        linear_addr(es(), field as u16),
        2,
        value as u32,
        file,
        func,
        line,
    );
    memw_raw_write(es(), field as u16, value);
}

// ============================================================================
// Memory init (constructor) + Sound Blaster stub + A20 + PIT port helpers
// [C lines 4917-5140]
// ============================================================================

/// Boot the machine: RAM, the PSP, the interrupt-vector table and BIOS data
/// area, the virtual clock, and the program image the active `GameConfig` names
/// — everything that must be true before the first guest instruction runs.
///
/// This is machine boot, not process init; it runs from a constructor (below)
/// only because a frozen per-game binary carries its `GameConfig` as a link-time
/// symbol, so by the time constructors run the program to load is already known.
/// The player host is one binary for every game and only learns which one from
/// its arguments — which is after constructors have run. So it installs the
/// config with `saisei_set_game_config` and calls this again.
///
/// Booting twice is therefore expected, and this is written to be a *reset*
/// rather than an increment: guest RAM is reused and re-zeroed instead of
/// reallocated, the file mappings are dropped, and the exit report is registered
/// once. The first boot, with no config installed, finds an empty program path
/// and simply loads nothing.
#[no_mangle]
pub unsafe extern "C" fn shim_boot_machine() {
    if cfg().psp_seg != 0 {
        psp_seg = cfg().psp_seg;
    }

    if virtual_memory.is_null() {
        virtual_memory = libc::calloc(1, MEMORY_SIZE) as *mut u8;
        if virtual_memory.is_null() {
            shim_flush_all_streams();
            libc::exit(1);
        }
    } else {
        // Re-boot: the guest starts from cold RAM, and nothing the previous boot
        // mapped is still true.
        libc::memset(virtual_memory as *mut c_void, 0, MEMORY_SIZE);
        file_mapping_count = 0;
    }

    psp = seg_off(psp_seg, 0) as *mut PSP;
    image_base = virtual_memory.add(((psp_seg.wrapping_add(0x10) as u32) << 4) as usize);
    init_psp();
    init_standard_handles();
    init_bios_data_area();
    for i in 0..256 {
        let addr: u16 = i as u16 * 4;
        memw_raw_write(0, addr, DEFAULT_ISR_OFF);
        memw_raw_write(0, addr + 2, DEFAULT_ISR_SEG);
    }
    memw_raw_write(0, 0x08 * 4, BIOS_IRQ0_ISR_OFF);
    memw_raw_write(0, 0x08 * 4 + 2, BIOS_IRQ0_ISR_SEG);
    memw_raw_write(0, 0x09 * 4, BIOS_IRQ1_ISR_OFF);
    memw_raw_write(0, 0x09 * 4 + 2, BIOS_IRQ1_ISR_SEG);
    memw_raw_write(0, 0x10 * 4, BIOS_VIDEO_ISR_OFF);
    memw_raw_write(0, 0x10 * 4 + 2, BIOS_VIDEO_ISR_SEG);
    memw_raw_write(0, 0x11 * 4, BIOS_EQUIPMENT_ISR_OFF);
    memw_raw_write(0, 0x11 * 4 + 2, BIOS_EQUIPMENT_ISR_SEG);
    memw_raw_write(0, 0x16 * 4, BIOS_KBD_ISR_OFF);
    memw_raw_write(0, 0x16 * 4 + 2, BIOS_KBD_ISR_SEG);
    memw_raw_write(0, 0x20 * 4, DOS_TERM_ISR_OFF);
    memw_raw_write(0, 0x20 * 4 + 2, DOS_TERM_ISR_SEG);
    memw_raw_write(0, 0x21 * 4, DOS_API_ISR_OFF);
    memw_raw_write(0, 0x21 * 4 + 2, DOS_API_ISR_SEG);
    memw_raw_write(0, 0x1A * 4, BIOS_TIMER_ISR_OFF);
    memw_raw_write(0, 0x1A * 4 + 2, BIOS_TIMER_ISR_SEG);
    memw_raw_write(0, 0x1C * 4, BIOS_TIMER_TICK_ISR_OFF);
    memw_raw_write(0, 0x1C * 4 + 2, BIOS_TIMER_TICK_ISR_SEG);
    memw_raw_write(0, 0x33 * 4, MOUSE_ISR_OFF);
    memw_raw_write(0, 0x33 * 4 + 2, MOUSE_ISR_SEG);
    // Seed the instruction-driven virtual clock from the host monotonic clock
    // so virtual and host share an epoch, and hand the chunks their first
    // instruction budget.
    virtual_now_accum_ns = shim_host_monotonic_ns();
    jit_instr_budget = JIT_BUDGET_QUANTUM;
    jit_budget_last_refill = JIT_BUDGET_QUANTUM;
    mem_page_flags_recompute();
    // Once per process, not once per boot — a second registration would print
    // the retired-instruction report twice at exit.
    if !RETIRED_REPORT_REGISTERED {
        RETIRED_REPORT_REGISTERED = true;
        libc::atexit(report_retired_at_exit);
    }
    last_host_time_ns = shim_virtual_now_ns();
    host_time_origin_ns = last_host_time_ns;
    pit_cycle_accum = 0;
    pit_cycle_fraction_accum = 0;
    pit_reload_value = 0x10000;
    pit_latch_valid = 0;
    pit_read_expect_high = 0;
    pit_read_buffer_is_latch = 0;
    last_present_time_ns = last_host_time_ns;
    last_screenshot_time_ns = last_host_time_ns;

    cga.crtc_index = 0;
    libc::memset(ptr::addr_of_mut!(cga.crtc_regs) as *mut c_void, 0, 0x20);
    cga.hsync_base = 0;
    cga.horiz_scroll = 0;
    cga.hsync_initialized = 0;

    set_ds(psp_seg);
    set_es(psp_seg);
    set_cs(psp_seg.wrapping_add(0x10));
    set_ss(psp_seg.wrapping_add(0x10));
    set_DF(0);
    set_IF(1);
    next_free_seg = psp_seg.wrapping_add(0x10);
    program_min_block_paras = (psp_seg.wrapping_add(0x10)).wrapping_sub(psp_seg);
    let mut new_cs: u16 = 0;
    let mut new_ip: u16 = 0;
    let mut new_ss: u16 = 0;
    let mut new_sp: u16 = 0;
    let mut program_path = cfg().program_path;
    if program_path.is_null() {
        program_path = cstr!("program.exe");
    }
    if load_executable(
        program_path,
        psp_seg.wrapping_add(0x10),
        0,
        &mut new_cs,
        &mut new_ip,
        &mut new_ss,
        &mut new_sp,
    ) == 0
    {
        set_cs(new_cs);
        set_ip(new_ip);
        set_ss(new_ss);
        set_sp(new_sp);
    }
    for i in 0..16 {
        null_guard_initial[i] = *virtual_memory.add(i);
    }
}

static mut RETIRED_REPORT_REGISTERED: bool = false;

unsafe extern "C" fn init_memory() {
    shim_boot_machine();
}

#[used]
#[link_section = ".init_array"]
static INIT_MEMORY_CTOR: unsafe extern "C" fn() = init_memory;

/// PPI port B: bit 0 = channel-2 gate, bit 1 = speaker data enable. Read by the
/// audio mixer, which is the only thing outside this file that touches it.
pub static mut port61: u8 = 0;
static mut port92: u8 = 0;

// ---------------------------------------------------------------------------
// Snapshot blocks for the hardware modelled in this file (see devices.rs).
//
// None of this was in ShimRuntimeState, which stopped at the video regs, the
// OPL2 register file and PIT channel 0. Everything below is guest-programmable
// and was being dropped by a save/load: the 8259A's mask (so a restored game got
// back whichever IRQs *we* unmask at power-on, not the ones it had chosen), the
// speaker's gate and its channel-2 divisor (its pitch), and the A20 port.
// ---------------------------------------------------------------------------

#[repr(C)]
#[derive(Clone, Copy)]
struct PicSnap {
    imr: u8,
    isr: u8,
    read_isr: u8,
    vector_base: u8,
    icw_step: u8,
    icw_needs_icw4: u8,
    icw_single: u8,
    pic2_imr: u8,
    pic2_isr: u8,
    pic2_read_isr: u8,
}

pub(crate) unsafe fn pic_state_capture(out: &mut Vec<u8>) {
    let s = PicSnap {
        imr: pic_imr,
        isr: pic_isr,
        read_isr: pic_read_isr,
        vector_base: pic_vector_base,
        icw_step: pic_icw_step,
        icw_needs_icw4: pic_icw_needs_icw4,
        icw_single: pic_icw_single,
        pic2_imr,
        pic2_isr,
        pic2_read_isr,
    };
    crate::devices::pod_capture(&s, out);
}

pub(crate) unsafe fn pic_state_restore(b: &[u8]) -> bool {
    match crate::devices::pod_restore::<PicSnap>(b) {
        Some(s) => {
            pic_imr = s.imr;
            pic_isr = s.isr;
            pic_read_isr = s.read_isr;
            pic_vector_base = s.vector_base;
            pic_icw_step = s.icw_step;
            pic_icw_needs_icw4 = s.icw_needs_icw4;
            pic_icw_single = s.icw_single;
            pic2_imr = s.pic2_imr;
            pic2_isr = s.pic2_isr;
            pic2_read_isr = s.pic2_read_isr;
            true
        }
        None => false,
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
struct PortSnap {
    port61: u8,
    port92: u8,
}

pub(crate) unsafe fn port_state_capture(out: &mut Vec<u8>) {
    let s = PortSnap { port61, port92 };
    crate::devices::pod_capture(&s, out);
}

pub(crate) unsafe fn port_state_restore(b: &[u8]) -> bool {
    match crate::devices::pod_restore::<PortSnap>(b) {
        Some(s) => {
            port61 = s.port61;
            port92 = s.port92;
            true
        }
        None => false,
    }
}

/// PIT channels 1 and 2, and channel 2's mode/load time. ShimRuntimeState
/// carries channel 0 (the BIOS tick) and nothing else — but channel 2 *is* the
/// speaker's pitch, and a game sets a tone's divisor once and leaves it.
#[repr(C)]
#[derive(Clone, Copy)]
struct PitAuxSnap {
    channel1: PITState,
    channel2: PITState,
    ch2_mode: u8,
    ch2_load_ns: u64,
}

pub(crate) unsafe fn pit_aux_state_capture(out: &mut Vec<u8>) {
    let mut s: PitAuxSnap = core::mem::zeroed();
    s.channel1 = pit_channel1;
    s.channel2 = pit_channel2;
    s.ch2_mode = pit_ch2_mode;
    s.ch2_load_ns = pit_ch2_load_ns;
    crate::devices::pod_capture(&s, out);
}

pub(crate) unsafe fn pit_aux_state_restore(b: &[u8]) -> bool {
    match crate::devices::pod_restore::<PitAuxSnap>(b) {
        Some(s) => {
            pit_channel1 = s.channel1;
            pit_channel2 = s.channel2;
            pit_ch2_mode = s.ch2_mode;
            pit_ch2_load_ns = s.ch2_load_ns;
            true
        }
        None => false,
    }
}

// The Sound Blaster and the 8237 DMA controller used to be stubbed here — a DSP
// that answered only its reset handshake and dropped every command after it, and
// a DMA "controller" that was one latch for channel 3 while every other DMA port
// fell through to io_port_error() and killed the process. Both are now real
// devices on the io_bus: see audio/sb.rs and audio/dma.rs.

#[no_mangle]
pub unsafe extern "C" fn a20_set_enabled(enabled: bool) {
    a20_enabled = enabled;
    if enabled {
        port92 |= 0x02;
    } else {
        port92 &= !0x02;
    }
}

unsafe extern "C" fn init_a20() {
    a20_set_enabled(true);
}

#[used]
#[link_section = ".init_array"]
static INIT_A20_CTOR: unsafe extern "C" fn() = init_a20;

unsafe fn pit_state_for_channel(channel: u8) -> *mut PITState {
    match channel {
        0 => ptr::addr_of_mut!(pit),
        1 => ptr::addr_of_mut!(pit_channel1),
        2 => ptr::addr_of_mut!(pit_channel2),
        _ => ptr::null_mut(),
    }
}

// 8254 channel-2 OUTPUT state (port 0x61 bit 5) needs the operating mode
// (control-word bits 1-3) and the virtual time of the last counter load /
// gate rise — neither lives in the FROZEN PITState (snapshots serialize that
// layout byte-for-byte), so they are separate statics. After a snapshot
// restore they reset (a one-sound transient), which is benign.
pub static mut pit_ch2_mode: u8 = 3;
static mut pit_ch2_load_ns: u64 = 0;

/// PIT ticks (1 tick = 838.1ns) between `since_ns` and `now_ns`.
fn pit_ticks_between(since_ns: u64, now_ns: u64) -> u64 {
    let elapsed = now_ns.saturating_sub(since_ns);
    ((elapsed as u128 * 1193182u128) / 1_000_000_000u128) as u64
}

/// The 8254 channel-2 output pin AT a given virtual instant. Faithful for the
/// modes games use: mode 0 (one-shot: out low from load until the count expires
/// — the "wait for timer" poll idiom), mode 2 (rate generator: out high except
/// one tick at terminal count; forced high while the gate is low), mode 3
/// (square wave: high first half of each period; forced high while the gate is
/// low). The gate is port 0x61 bit 0.
///
/// Parameterised by time rather than reading the clock itself because the audio
/// mixer renders *past* intervals and needs the pin as a function of time. Both
/// callers — the guest's port 0x61 bit 5, and the speaker's PCM path — go
/// through this one model, so the sound a game makes and the bit it reads back
/// can never disagree.
pub unsafe fn pit_ch2_output_at(now_ns: u64) -> u8 {
    let gate = port61 & 1;
    let reload: u64 = if pit_channel2.reload != 0 {
        pit_channel2.reload as u64
    } else {
        0x10000
    };
    match pit_ch2_mode {
        0 => {
            // Gate low suspends counting: not expired yet (the poll idiom
            // loads the count with the gate already high).
            if gate == 0 {
                return 0;
            }
            (pit_ticks_between(pit_ch2_load_ns, now_ns) >= reload) as u8
        }
        2 => {
            if gate == 0 {
                return 1;
            }
            (pit_ticks_between(pit_ch2_load_ns, now_ns) % reload != reload - 1) as u8
        }
        _ => {
            if gate == 0 {
                return 1;
            }
            (pit_ticks_between(pit_ch2_load_ns, now_ns) % reload < (reload + 1) / 2) as u8
        }
    }
}

/// The channel-2 output pin now, as port 0x61 bit 5 reports it.
unsafe fn pit_ch2_output() -> u8 {
    pit_ch2_output_at(shim_virtual_now_ns())
}

/// The AT's DRAM refresh-detect bit (port 0x61 bit 4): toggles on each PIT
/// channel-1 terminal count. The BIOS programs channel 1 to 18 ticks
/// (15.085µs); our machine starts post-POST, so an unprogrammed channel 1
/// models that BIOS value. Period code busy-waits by COUNTING these toggles
/// — a static bit spins such a delay loop forever.
unsafe fn refresh_detect_bit() -> u8 {
    let reload: u64 = match pit_channel1.reload {
        0 | 0x10000 => 18,
        r => r as u64,
    };
    let period_ns = reload * 1_000_000_000 / 1193182;
    ((shim_virtual_now_ns() / period_ns) & 1) as u8
}

unsafe fn pit_commit_reload(state: *mut PITState, channel: u8) {
    let reload_value: u32 = if (*state).temp_reload != 0 {
        (*state).temp_reload as u32
    } else {
        0x10000
    };
    let reload_ticks: u32 = reload_value;
    (*state).reload = reload_ticks;
    if channel == 0 {
        pit_reload_value = (reload_value & 0xFFFF) as u32;
        pit_read_expect_high = 0;
        pit_latch_valid = 0;
    }
    if channel == 2 {
        // A data write (re)starts channel 2's count — the anchor the output
        // model (port 0x61 bit 5) measures from.
        pit_ch2_load_ns = shim_virtual_now_ns();
    }
}

unsafe fn pit_write_data(channel: u8, value: u8) {
    let state = pit_state_for_channel(channel);
    if state.is_null() {
        return;
    }
    match (*state).access_mode {
        0x1 => {
            (*state).temp_reload = ((*state).temp_reload & 0xFF00) | value as u16;
            pit_commit_reload(state, channel);
            (*state).expect_high = 0;
        }
        0x2 => {
            (*state).temp_reload = ((*state).temp_reload & 0x00FF) | ((value as u16) << 8);
            pit_commit_reload(state, channel);
            (*state).expect_high = 0;
        }
        0x3 => {
            if (*state).expect_high == 0 {
                (*state).temp_reload = ((*state).temp_reload & 0xFF00) | value as u16;
                (*state).expect_high = 1;
            } else {
                (*state).temp_reload = ((*state).temp_reload & 0x00FF) | ((value as u16) << 8);
                pit_commit_reload(state, channel);
                (*state).expect_high = 0;
            }
        }
        _ => {}
    }
}

// ============================================================================
// IO-port dispatch (inb/outb/inw/outw) + block scan helpers  [C lines 5142-5624]
// ============================================================================

// Last I/O port the guest touched + a monotonic access counter, sampled by the
// SIGUSR1 freeze diagnostic (a spin on a status port is named directly by
// watching last_io_port stay pinned while io_access_counter races).
static mut last_io_port: u16 = 0xFFFF;
static mut last_io_was_read: u8 = 0;
static mut io_access_counter: u64 = 0;

#[no_mangle]
pub unsafe extern "C" fn inb(port: u16) -> u8 {
    last_io_port = port;
    last_io_was_read = 1;
    io_access_counter = io_access_counter.wrapping_add(1);
    let dev = io_bus_lookup(port);
    if !dev.is_null() {
        return ((*dev).read8.unwrap())(port);
    }
    if port == 0x60 {
        if kbd.scancode_ready != 0 {
            let sc = kbd.scancode;
            let asc = kbd.ascii;
            // 0xE0 is the extended-key PREFIX byte (grey cursor/nav cluster),
            // not a key make, and E0-prefixed 2A/36 are the NumLock fake-shift
            // FRAMING around grey-key events: both pass through to the reader
            // untouched but must not stage a BIOS keystroke or count as
            // consumed input.
            let prev_was_e0 = kbd.last_scancode == 0xE0;
            kbd.last_scancode = sc;
            let fake_shift = prev_was_e0 && matches!(sc & 0x7F, 0x2A | 0x36);
            if (sc & 0x80) == 0 && sc != 0xE0 && !fake_shift {
                shim_input_phase_started = 1;
                snapshot_on_key_consumed();
                kbd.pending_bios_ascii = asc;
                kbd.pending_bios_scancode = sc;
                kbd.pending_bios_valid = 1;
            }
            kbd_consume();
            return sc;
        }
        return kbd.last_scancode;
    }
    if port == 0x61 {
        // Port B read: the low nibble echoes the written control bits
        // (speaker gate/data enable); bit 4 = DRAM refresh detect, bit 5 =
        // 8254 channel-2 output — LIVE hardware state derived from the
        // virtual clock. These bits read as static constants before, which is
        // a real hang class on this corpus's period: PC-speaker sound routines
        // and refresh-counting delay loops (INT 15h AH=86h style, and driver
        // BUSY-waits on the channel-2 one-shot) poll them and never advance if
        // they don't toggle. Faithful modelling, not a targeted band-aid.
        // Fold time first, same rule as the PIT data ports: a poll loop must
        // see time flowing between safepoints.
        shim_time_sync();
        let mut v = port61 & 0x0F;
        if refresh_detect_bit() != 0 {
            v |= 0x10;
        }
        if pit_ch2_output() != 0 {
            v |= 0x20;
        }
        return v;
    }
    if port == 0x40 {
        // Fold un-accounted budget into the clock first: the counter must
        // move between two reads inside one quantum (calibration loops #DE
        // on a zero delta otherwise).
        shim_time_sync();
        let ret: u8;
        if pit.access_mode == 0x3 {
            if pit_read_expect_high == 0 {
                if pit_latch_valid != 0 {
                    pit_read_buffer = pit_latched_value;
                    pit_read_buffer_is_latch = 1;
                } else {
                    pit_read_buffer = pit_current_count();
                    pit_read_buffer_is_latch = 0;
                }
                ret = (pit_read_buffer & 0xFF) as u8;
                pit_read_expect_high = 1;
                return ret;
            }
            ret = ((pit_read_buffer >> 8) & 0xFF) as u8;
            pit_read_expect_high = 0;
            if pit_read_buffer_is_latch != 0 {
                pit_latch_valid = 0;
                pit_read_buffer_is_latch = 0;
            }
            return ret;
        }
        let value: u16;
        if pit_latch_valid != 0 {
            value = pit_latched_value;
            pit_latch_valid = 0;
        } else {
            value = pit_current_count();
        }
        pit_read_expect_high = 0;
        pit_read_buffer_is_latch = 0;
        match pit.access_mode {
            0x1 => ret = (value & 0xFF) as u8,
            0x2 => ret = ((value >> 8) & 0xFF) as u8,
            _ => ret = (value & 0xFF) as u8,
        }
        return ret;
    }
    if port == 0x42 {
        shim_time_sync(); // live counter — see port 0x40
        let reload2: u16 = if pit_channel2.reload != 0 {
            pit_channel2.reload as u16
        } else {
            0
        };
        let ticks: u64 = (shim_scaled_monotonic_ns() * 1193182u64) / 1000000000u64;
        let count: u16;
        if reload2 == 0 {
            count = (0u16).wrapping_sub((ticks & 0xFFFF) as u16);
        } else {
            count = reload2.wrapping_sub((ticks % reload2 as u64) as u16);
        }
        if pit_channel2.access_mode == 0x1 {
            return (count & 0xFF) as u8;
        }
        if pit_channel2.access_mode == 0x2 {
            return ((count >> 8) & 0xFF) as u8;
        }
        if pit_channel2.expect_high == 0 {
            pit_channel2.expect_high = 1;
            return (count & 0xFF) as u8;
        }
        pit_channel2.expect_high = 0;
        return ((count >> 8) & 0xFF) as u8;
    }
    if port == 0x20 {
        // 8259A master: OCW3 selects whether this reads back the in-service or
        // the request register.
        return if pic_read_isr != 0 {
            pic_isr
        } else {
            pic_irr()
        };
    }
    if port == 0x21 {
        return pic_imr;
    }
    if port == 0xA0 {
        return if pic2_read_isr != 0 { pic2_isr } else { 0 };
    }
    if port == 0xA1 {
        return pic2_imr;
    }
    if port == 0x92 {
        return port92;
    }
    if port == 0x64 {
        return if kbd.scancode_ready != 0 { 0x01 } else { 0x00 };
    }
    if port == 0x201 {
        return 0xFF;
    }
    if port == 0x3C2 || port == 0x3CC {
        return vga.misc_output;
    }
    if port == 0x3CD {
        return vga.feature_control;
    }
    if port == 0x3C9 {
        let comp =
            vga.palette[vga.palette_read_index as usize * 3 + vga.palette_component as usize];
        vga.palette_component += 1;
        if vga.palette_component == 3 {
            vga.palette_component = 0;
            vga.palette_read_index = vga.palette_read_index.wrapping_add(1);
        }
        return comp & 0x3F;
    }
    if port == 0x3C8 {
        return vga.palette_write_index;
    }
    if port == 0x3C6 {
        return vga.palette_mask;
    }
    if port == 0x3BA || port == 0x3DA {
        // CGA/MDA status register, modeled from the 6845's real raster timing
        // on the shared 14.318MHz crystal (same time base as the PIT):
        //   bit 0 = display disabled (horizontal OR vertical blanking) — games
        //           write CGA VRAM snow-free one word per ~64µs hblank window;
        //   bit 3 = vertical sync pulse (MC6845: fixed 16 scan lines starting
        //           at the programmed vsync row — line 224 of 262 on CGA).
        // Both bits are set during vsync (display is disabled there too).
        // The old model was a 16ms half-period square wave with bit0 and bit3
        // mutually exclusive: a snow-avoiding redraw loop (wait-for-hblank per
        // word) took ~32ms per WORD instead of ≤64µs — Alley Cat's title cat
        // froze for ~30s per animation frame.
        //
        // Fold retired units into the virtual clock first, exactly like the
        // PIT port handlers: a polling loop must see time flowing between
        // safepoints, or the ~19µs hblank window aliases against the budget
        // quantum.
        let t = shim_time_sync();
        const LINE_NS: u64 = 63_695; // 15.70kHz horizontal rate
        const FRAME_LINES: u64 = 262; // 59.92Hz frame
        const VISIBLE_LINES: u64 = 200;
        const VSYNC_START_LINE: u64 = 224;
        const VSYNC_LINES: u64 = 16; // MC6845 vsync width is fixed
        const H_ACTIVE_NS: u64 = 44_700; // 80 of 114 char clocks visible
        let frame_pos = t % (LINE_NS * FRAME_LINES);
        let line = frame_pos / LINE_NS;
        let line_pos = frame_pos % LINE_NS;
        let mut status: u8 = 0;
        if line >= VISIBLE_LINES || line_pos >= H_ACTIVE_NS {
            status |= 0x01;
        }
        if line >= VSYNC_START_LINE && line < VSYNC_START_LINE + VSYNC_LINES {
            status |= 0x08;
        }
        return status;
    }
    if port == 0x3B4 || port == 0x3B5 {
        return 0xFF;
    }
    if port == 0x3D4 {
        return cga.crtc_index;
    }
    if port == 0x3D5 {
        return cga.crtc_regs[(cga.crtc_index & 0x1F) as usize];
    }
    io_port_error(cstr!("inb"), port);
    0
}

#[no_mangle]
pub unsafe extern "C" fn inw(port: u16) -> u16 {
    let lo = inb(port);
    let hi = inb(port.wrapping_add(1));
    lo as u16 | ((hi as u16) << 8)
}

#[no_mangle]
pub unsafe extern "C" fn outb(port: u16, value: u8) {
    last_io_port = port;
    last_io_was_read = 0;
    io_access_counter = io_access_counter.wrapping_add(1);
    let dev = io_bus_lookup(port);
    if !dev.is_null() {
        ((*dev).write8.unwrap())(port, value);
        return;
    }
    // The PC speaker's three ports. Render the audio owed up to this instant
    // BEFORE the write lands, so the samples that precede it are made from the
    // old state and the change takes effect at its own virtual timestamp. A PWM
    // speaker driver toggles these thousands of times a second; quantising those
    // edges to the next service tick would turn digitised speech into noise.
    // (0x388/0x389 do the same inside the OPL2 device, above.)
    let speaker_port = matches!(port, 0x42 | 0x43 | 0x61);
    if speaker_port {
        crate::audio::catchup();
    }
    match port {
        0x20 => {
            // 8259A master command port. An EOI clears the IN-SERVICE bit of the
            // acknowledged line; it does NOT discard pending requests (IRR). The
            // old shortcut `irq0_pending = 0` here silently destroyed any timer
            // tick that became pending while a handler ran (or whenever a driver
            // wrote defensive EOIs from its main loop — DM's IBMIO does this
            // every poll), starving INT8 and freezing game clocks.
            if value & 0x10 != 0 {
                // ICW1: begin initialization. The bytes that follow on 0x21 are
                // ICW2 (vector base), then ICW3 if cascaded, then ICW4 if asked
                // for — NOT mask writes.
                pic_icw_needs_icw4 = value & 0x01;
                pic_icw_single = (value >> 1) & 0x01;
                pic_icw_step = 1;
                pic_isr = 0;
                pic_read_isr = 0;
            } else if value & 0x08 != 0 {
                // OCW3: bit1 selects the read register for the next IN 0x20.
                if value & 0x02 != 0 {
                    pic_read_isr = value & 0x01;
                }
            } else if value & 0x20 != 0 {
                // OCW2 with the EOI bit.
                pic_eoi(value);
            }
        }
        0x21 => {
            if pic_icw_step != 0 {
                // Initialization sequence in progress (see ICW1 above).
                match pic_icw_step {
                    1 => {
                        // ICW2: the vector base. IRQ n becomes INT (base + n).
                        pic_vector_base = value & 0xF8;
                        pic_icw_step = if pic_icw_single != 0 {
                            if pic_icw_needs_icw4 != 0 {
                                3
                            } else {
                                0
                            }
                        } else {
                            2
                        };
                    }
                    2 => {
                        // ICW3: cascade wiring — nothing for us to model.
                        pic_icw_step = if pic_icw_needs_icw4 != 0 { 3 } else { 0 };
                    }
                    _ => {
                        // ICW4: 8086 mode / EOI mode — nothing for us to model.
                        pic_icw_step = 0;
                    }
                }
            } else {
                // OCW1: the interrupt mask. Unmasking a line can let a latched
                // request through, which OPENS the delivery gate.
                pic_imr = value;
                shim_irq_recheck();
            }
        }
        0xA0 => {
            // Slave command port. No emulated device sits on IRQ8-15, so only
            // the state a guest can read back is kept.
            if value & 0x10 != 0 {
                pic2_isr = 0;
                pic2_read_isr = 0;
            } else if value & 0x08 != 0 {
                if value & 0x02 != 0 {
                    pic2_read_isr = value & 0x01;
                }
            } else if value & 0x20 != 0 && pic2_isr != 0 {
                pic2_isr &= pic2_isr - 1;
            }
        }
        0xA1 => {
            pic2_imr = value;
        }
        0x201 => {}
        0x43 => {
            let channel = (value >> 6) & 0x03;
            let access = (value >> 4) & 0x03;
            let state = pit_state_for_channel(channel);
            if state.is_null() {
                // Read-back command (channel == 3) not yet implemented.
            } else if access == 0x00 {
                if channel == 0 {
                    shim_time_sync(); // latch a live counter — see inb 0x40
                    pit_latched_value = pit_current_count();
                    pit_latch_valid = 1;
                    pit_read_expect_high = 0;
                }
            } else {
                (*state).access_mode = access;
                (*state).expect_high = 0;
                if channel == 0 {
                    pit_latch_valid = 0;
                }
                if channel == 2 {
                    // Control-word bits 1-3 select the operating mode (the
                    // 8254 aliases 6/7 back to 2/3); mode 0 drops the output
                    // low immediately — anchor now, the data write re-anchors.
                    let mut mode = (value >> 1) & 0x07;
                    if mode >= 6 {
                        mode -= 4;
                    }
                    pit_ch2_mode = mode;
                    pit_ch2_load_ns = shim_virtual_now_ns();
                }
            }
        }
        0x40 => pit_write_data(0, value),
        0x41 => pit_write_data(1, value),
        0x42 => pit_write_data(2, value),
        0x61 => {
            // A gate rising edge (bit 0) reloads channel 2's counter in the
            // periodic modes — re-anchor the output model.
            if value & 1 != 0 && port61 & 1 == 0 {
                pit_ch2_load_ns = shim_virtual_now_ns();
            }
            port61 = value;
        }
        0x3B4 | 0x3B5 => {}
        0x3D4 => {
            cga.crtc_index = value & 0x1F;
        }
        0x3D5 => {
            let idx = (cga.crtc_index & 0x1F) as usize;
            cga.crtc_regs[idx] = value;
            if idx == 0x02 {
                if cga.hsync_initialized == 0 {
                    cga.hsync_initialized = 1;
                    cga.hsync_base = value;
                    cga.horiz_scroll = 0;
                } else {
                    let mut delta: c_int = value as c_int - cga.hsync_base as c_int;
                    if delta >= 8 || delta <= -8 {
                        delta %= 8;
                    }
                    cga.horiz_scroll = delta;
                }
            }
        }
        0x3D8 => {
            let new_mode;
            let new_palette;
            let graphics = (value >> 1) & 0x01;
            if graphics == 0 {
                let high_res_text = value & 0x01;
                let black_and_white = (value >> 2) & 0x01;
                if high_res_text != 0 {
                    new_mode = if black_and_white != 0 { 0x02 } else { 0x03 };
                } else {
                    new_mode = 0x00;
                }
                new_palette = 0;
            } else {
                let high_res_graphics = (value >> 4) & 0x01;
                let black_and_white = (value >> 2) & 0x01;
                if high_res_graphics != 0 {
                    new_mode = 0x06;
                    new_palette = 0x02;
                } else {
                    new_mode = if black_and_white != 0 { 0x05 } else { 0x04 };
                    new_palette = 0x00;
                }
            }
            if new_palette != bios_video.cga_palette_select {
                bios_video.cga_palette_select = new_palette;
                *seg_off(0x40, 0x0066) = new_palette;
            }
            video_invalidate_palette_cache();
            apply_video_mode_state(new_mode);
        }
        0x3D9 => {
            *seg_off(0x40, 0x0066) = value;
            bios_video.cga_border_color = value & 0x0F;
            vga.border_color = bios_video.cga_border_color;
            let mut palette_select: u8 = 0;
            if value & 0x10 != 0 {
                palette_select |= 0x01;
            }
            if value & 0x20 != 0 {
                palette_select |= 0x02;
            }
            if value & 0x08 != 0 {
                palette_select |= 0x04;
            }
            if palette_select != bios_video.cga_palette_select {
                bios_video.cga_palette_select = palette_select;
            }
            video_invalidate_palette_cache();
        }
        0x92 => {
            port92 = value;
            a20_set_enabled((value & 0x02) != 0);
        }
        0x3C2 | 0x3CC => {
            vga.misc_output = value;
        }
        0x3CD => {
            vga.feature_control = value;
        }
        0x3C8 => {
            vga.palette_write_index = value;
            vga.palette_component = 0;
        }
        0x3C9 => {
            vga.palette[vga.palette_write_index as usize * 3 + vga.palette_component as usize] =
                vga_dac_component(value);
            vga.palette_component += 1;
            if vga.palette_component == 3 {
                vga.palette_component = 0;
                vga.palette_write_index = vga.palette_write_index.wrapping_add(1);
            }
        }
        0x3C7 => {
            vga.palette_read_index = value;
            vga.palette_component = 0;
        }
        0x3C6 => {
            vga.palette_mask = value;
        }
        0x3CE => {
            vga.graphics_index = value & 0x0F;
        }
        0x3CF => {
            vga.graphics_regs[(vga.graphics_index & 0x0F) as usize] = value;
        }
        _ => io_port_error(cstr!("outb"), port),
    }
    if speaker_port {
        // The guest state is now current; hand the mixer the new gate/divisor as
        // a timestamped event.
        crate::audio::speaker::on_port_write();
    }
}

#[no_mangle]
pub unsafe extern "C" fn outw(port: u16, value: u16) {
    outb(port, (value & 0x00FF) as u8);
    outb(port.wrapping_add(1), ((value >> 8) & 0x00FF) as u8);
}

#[no_mangle]
pub unsafe extern "C" fn compareMemoryUntilMismatch(
    mut src: *const u8,
    mut dst: *const u8,
    count: u16,
    direction: c_int,
) -> u8 {
    critical_section_enter(
        cstr!("compareMemoryUntilMismatch"),
        SHIMS_FILE,
        cstr!("compareMemoryUntilMismatch"),
        5561,
    );
    let mut src_addr: u32 = src.offset_from(virtual_memory) as u32;
    let mut dst_addr: u32 = dst.offset_from(virtual_memory) as u32;
    for _i in 0..count {
        if *src != *dst {
            critical_section_exit(
                cstr!("compareMemoryUntilMismatch"),
                SHIMS_FILE,
                cstr!("compareMemoryUntilMismatch"),
                5566,
            );
            return 0;
        }
        src_addr = (src_addr & !0xFFFF) | (((src_addr as i64 + direction as i64) as u32) & 0xFFFF);
        dst_addr = (dst_addr & !0xFFFF) | (((dst_addr as i64 + direction as i64) as u32) & 0xFFFF);
        src = virtual_memory.add(src_addr as usize);
        dst = virtual_memory.add(dst_addr as usize);
    }
    critical_section_exit(
        cstr!("compareMemoryUntilMismatch"),
        SHIMS_FILE,
        cstr!("compareMemoryUntilMismatch"),
        5574,
    );
    1
}

#[no_mangle]
pub unsafe extern "C" fn scanMemoryForAl(
    mut dst: *const u8,
    value: u8,
    count: u16,
    direction: c_int,
    last_byte: *mut u8,
) -> u16 {
    critical_section_enter(
        cstr!("scanMemoryForAl"),
        SHIMS_FILE,
        cstr!("scanMemoryForAl"),
        5580,
    );
    let mut addr: u32 = dst.offset_from(virtual_memory) as u32;
    let mut i: u16 = 0;
    let mut byte: u8 = 0;
    static mut scan_log_count: c_int = 0;
    if scan_log_count < 50 {
        shim_log_stdout(
            cstr!("Trace: scanMemoryForAl dst=%04X:%04X value=0x%02X count=%u dir=%d"),
            (addr >> 4) as c_uint,
            (addr & 0xF) as c_uint,
            value as c_uint,
            count as c_uint,
            direction,
        );
        let mut preview_addr = addr;
        let mut preview_ptr = dst;
        shim_log_stdout(cstr!(" bytes:"));
        let mut j: u16 = 0;
        while j < count && j < 8 {
            shim_log_stdout(cstr!(" %02X"), *preview_ptr as c_uint);
            preview_addr = (preview_addr & !0xFFFF)
                | (((preview_addr as i64 + direction as i64) as u32) & 0xFFFF);
            preview_ptr = virtual_memory.add(preview_addr as usize);
            j += 1;
        }
        shim_log_stdout(cstr!("\n"));
        scan_log_count += 1;
    }
    if count > 0 {
        byte = *dst;
        while i < count && byte != value {
            addr = (addr & !0xFFFF) | (((addr as i64 + direction as i64) as u32) & 0xFFFF);
            dst = virtual_memory.add(addr as usize);
            i += 1;
            if i < count {
                byte = *dst;
            }
        }
    }
    if i >= count && count > 0 && byte != value {
        let seg = (addr >> 4) as u16;
        let off = (addr & 0xF) as u16;
        shim_log_stdout(
            cstr!("Warning: scanMemoryForAl miss value=0x%02X count=%u final=%04X:%04X\n"),
            value as c_uint,
            count as c_uint,
            seg as c_uint,
            off as c_uint,
        );
    }
    if !last_byte.is_null() {
        *last_byte = byte;
    }
    critical_section_exit(
        cstr!("scanMemoryForAl"),
        SHIMS_FILE,
        cstr!("scanMemoryForAl"),
        5622,
    );
    i
}

// ============================================================================
// Synthetic BIOS/DOS ISR stubs + call-target table + lookup  [C 5628-5913]
// ============================================================================

unsafe extern "C" fn int08h_impl(
    _expected_retip: u16,
    file: *const c_char,
    func: *const c_char,
    line: c_int,
) {
    shim_log(cstr!("int08h_impl"), file, func, line, ptr::null());
    let preincremented = bios_timer_tick_preincremented;
    bios_timer_tick_preincremented = 0;
    if preincremented == 0 {
        bios_timer_increment();
    }
    invoke_isr(0x1C, 1, 1, 1, ip(), cstr!("<int08>"), func, line);
    // The real BIOS timer handler ends with `mov al,20h; out 20h,al`. Now that
    // the in-service register is modelled, skipping the EOI would leave IRQ0 in
    // service forever and no timer tick would ever be delivered again.
    pic_eoi(0x20);
    iret_impl(file, func, line);
}

unsafe extern "C" fn int09h_impl(
    _expected_retip: u16,
    file: *const c_char,
    func: *const c_char,
    line: c_int,
) {
    shim_log(cstr!("int09h_impl"), file, func, line, ptr::null());
    kbd_bios_deposit_from_isr();
    // As with INT 08: the real BIOS keyboard handler EOIs before its IRET.
    pic_eoi(0x20);
    iret_impl(file, func, line);
}

unsafe extern "C" fn int11h_impl(
    _expected_retip: u16,
    file: *const c_char,
    func: *const c_char,
    line: c_int,
) {
    shim_log(cstr!("int11h_impl"), file, func, line, ptr::null());
    set_ax(memw_raw_read(0x40, 0x0010));
    iret_impl(file, func, line);
}

unsafe extern "C" fn int10h_impl(
    _expected_retip: u16,
    file: *const c_char,
    func: *const c_char,
    line: c_int,
) {
    shim_log(cstr!("int10h_impl"), file, func, line, ptr::null());
    if ah() == 0x00 {
        bios_set_video_mode_impl(al(), file, func, line);
    } else if ah() == 0x02 {
        bios_set_cursor_position_impl(bh(), dh(), dl(), file, func, line);
    } else if ah() == 0x03 {
        bios_get_cursor(bh());
    } else if ah() == 0x09 {
        bios_write_char_attr(al(), bh(), bl(), cx());
    } else if ah() == 0x0A {
        bios_write_char_only(al(), bh(), cx());
    } else if ah() == 0x0F {
        set_al(bios_current_video_mode());
        set_ah(bios_current_video_columns());
        set_bh(bios_current_active_page());
    } else if ah() == 0x06 {
        bios_scroll_window(al(), bh(), ch(), cl(), dh(), dl(), 0);
    } else if ah() == 0x07 {
        bios_scroll_window(al(), bh(), ch(), cl(), dh(), dl(), 1);
    } else if ah() == 0x0B {
        bios_set_cga_palette_impl(bh(), bl(), file, func, line);
    } else if ah() == 0x0E {
        bios_teletype_output_impl(al(), bh(), bl(), file, func, line);
    } else if ah() == 0x1A {
        set_al(0x1A);
        set_bl(bios_display_combination_code());
        set_bh(bios_display_combination_alt_code());
    } else if ah() == 0x10 {
        bios_set_palette_impl(file, func, line);
    } else if ah() == 0x12 {
        bios_video_alt_select_impl(file, func, line);
    } else if ah() == 0x08 {
        set_ax(bios_read_char_attr());
    } else if ah() == 0x30 {
        let mut seg: u16 = 0;
        let mut off: u16 = 0;
        bios_get_video_parameter_block(al(), &mut seg, &mut off);
        set_cx(seg);
        set_dx(off);
    } else {
        let mut msg = [0u8; 256];
        libc::snprintf(
            msg.as_mut_ptr() as *mut c_char,
            msg.len(),
            cstr!("unhandled BIOS video AH=0x%02X (%s:%s:%d)"),
            ah() as c_uint,
            file,
            func,
            line,
        );
        shim_log_crash(cstr!("%s\n"), msg.as_ptr() as *const c_char);
        save_bug_bundle(
            cstr!("unimplemented_bios"),
            ((cs() as u32) << 4) + ip() as u32,
            msg.as_ptr() as *const c_char,
        );
        shim_flush_all_streams();
        libc::abort();
    }
    iret_impl(file, func, line);
}

unsafe extern "C" fn int16h_impl(
    _expected_retip: u16,
    file: *const c_char,
    func: *const c_char,
    line: c_int,
) {
    shim_log(cstr!("int16h_impl"), file, func, line, ptr::null());
    bios_keyboard_impl(file, func, line);
    iret_impl(file, func, line);
}

unsafe extern "C" fn int20h_impl(
    _expected_retip: u16,
    file: *const c_char,
    func: *const c_char,
    line: c_int,
) {
    shim_log(cstr!("int20h_impl"), file, func, line, ptr::null());
    dos_exit_impl(file, func, line);
}

unsafe extern "C" fn int21h_impl(
    _expected_retip: u16,
    file: *const c_char,
    func: *const c_char,
    line: c_int,
) {
    shim_log(cstr!("int21h_impl"), file, func, line, ptr::null());
    dos_api_impl(file, func, line);
    iret_impl(file, func, line);
}

unsafe extern "C" fn int1Ah_impl(
    _expected_retip: u16,
    file: *const c_char,
    func: *const c_char,
    line: c_int,
) {
    shim_log(cstr!("int1Ah_impl"), file, func, line, ptr::null());
    match ah() {
        0x00 => {
            let mut ticks: u32 = memw_raw_read(0x40, 0x006C) as u32;
            ticks |= (memw_raw_read(0x40, 0x006E) as u32) << 16;
            set_cx((ticks >> 16) as u16);
            set_dx((ticks & 0xFFFF) as u16);
            set_al(*seg_off(0x40, 0x70));
            *seg_off(0x40, 0x70) = 0;
            set_iret_carry(0);
        }
        0x01 => {
            memw_raw_write(0x40, 0x006C, dx());
            memw_raw_write(0x40, 0x006E, cx());
            *seg_off(0x40, 0x70) = 0;
            set_iret_carry(0);
        }
        0x1C => {
            let mut ticks: u32 = memw_raw_read(0x40, 0x006C) as u32;
            ticks |= (memw_raw_read(0x40, 0x006E) as u32) << 16;
            set_cx((ticks >> 16) as u16);
            set_dx((ticks & 0xFFFF) as u16);
            set_iret_carry(0);
        }
        0x02 => {
            let now = libc::time(ptr::null_mut());
            let tm_ptr = libc::localtime(&now);
            let mut local_tm: libc::tm = core::mem::zeroed();
            if !tm_ptr.is_null() {
                local_tm = *tm_ptr;
            }
            set_ch(to_bcd(local_tm.tm_hour as u8));
            set_cl(to_bcd(local_tm.tm_min as u8));
            set_dh(to_bcd(local_tm.tm_sec as u8));
            set_dl(if local_tm.tm_isdst > 0 { 1 } else { 0 });
            set_iret_carry(0);
        }
        0x04 => {
            let now = libc::time(ptr::null_mut());
            let tm_ptr = libc::localtime(&now);
            let mut local_tm: libc::tm = core::mem::zeroed();
            if !tm_ptr.is_null() {
                local_tm = *tm_ptr;
            }
            let year: u16 = (local_tm.tm_year + 1900) as u16;
            set_ch(to_bcd((year / 100) as u8));
            set_cl(to_bcd((year % 100) as u8));
            set_dh(to_bcd((local_tm.tm_mon + 1) as u8));
            set_dl(to_bcd(local_tm.tm_mday as u8));
            set_al(0);
            set_iret_carry(0);
        }
        _ => {
            let mut msg = [0u8; 256];
            libc::snprintf(
                msg.as_mut_ptr() as *mut c_char,
                msg.len(),
                cstr!("unhandled BIOS timer AH=0x%02X (%s:%s:%d)"),
                ah() as c_uint,
                file,
                func,
                line,
            );
            shim_log_crash(cstr!("%s\n"), msg.as_ptr() as *const c_char);
            save_bug_bundle(
                cstr!("unimplemented_bios"),
                ((cs() as u32) << 4) + ip() as u32,
                msg.as_ptr() as *const c_char,
            );
            shim_flush_all_streams();
            libc::abort();
        }
    }
    iret_impl(file, func, line);
}

unsafe extern "C" fn int1Ch_impl(
    _expected_retip: u16,
    file: *const c_char,
    func: *const c_char,
    line: c_int,
) {
    shim_log(cstr!("int1Ch_impl"), file, func, line, ptr::null());
    iret_impl(file, func, line);
}

unsafe extern "C" fn int33h_impl(
    _expected_retip: u16,
    file: *const c_char,
    func: *const c_char,
    line: c_int,
) {
    shim_log(cstr!("int33h_impl"), file, func, line, ptr::null());
    mouse_int33_impl(file, func, line);
    iret_impl(file, func, line);
}

static mut base_call_targets: [CallTarget; 11] = [
    CallTarget {
        addr: DEFAULT_ISR_LINEAR,
        file: ptr::null(),
        fn_: Some(default_isr_impl),
    },
    CallTarget {
        addr: BIOS_IRQ0_ISR_LINEAR,
        file: ptr::null(),
        fn_: Some(int08h_impl),
    },
    CallTarget {
        addr: BIOS_IRQ1_ISR_LINEAR,
        file: ptr::null(),
        fn_: Some(int09h_impl),
    },
    CallTarget {
        addr: BIOS_VIDEO_ISR_LINEAR,
        file: ptr::null(),
        fn_: Some(int10h_impl),
    },
    CallTarget {
        addr: BIOS_EQUIPMENT_ISR_LINEAR,
        file: ptr::null(),
        fn_: Some(int11h_impl),
    },
    CallTarget {
        addr: BIOS_KBD_ISR_LINEAR,
        file: ptr::null(),
        fn_: Some(int16h_impl),
    },
    CallTarget {
        addr: DOS_TERM_ISR_LINEAR,
        file: ptr::null(),
        fn_: Some(int20h_impl),
    },
    CallTarget {
        addr: DOS_API_ISR_LINEAR,
        file: ptr::null(),
        fn_: Some(int21h_impl),
    },
    CallTarget {
        addr: BIOS_TIMER_ISR_LINEAR,
        file: ptr::null(),
        fn_: Some(int1Ah_impl),
    },
    CallTarget {
        addr: BIOS_TIMER_TICK_ISR_LINEAR,
        file: ptr::null(),
        fn_: Some(int1Ch_impl),
    },
    CallTarget {
        addr: MOUSE_ISR_LINEAR,
        file: ptr::null(),
        fn_: Some(int33h_impl),
    },
];
const base_call_target_count: usize = 11;

unsafe fn is_builtin_call_target(addr: u32) -> c_int {
    for i in 0..base_call_target_count {
        if base_call_targets[i].addr == addr {
            return 1;
        }
    }
    0
}

unsafe fn try_call_target(addr: u32) -> GameFunc {
    let m = find_file_mapping(addr);
    let mut mapped_file: *const c_char = ptr::null();
    if !m.is_null() && !(*m).path.is_null() {
        let slash = libc::strrchr((*m).path, b'/' as c_int);
        mapped_file = if !slash.is_null() {
            slash.add(1)
        } else {
            (*m).path
        };
    }
    for i in 0..base_call_target_count {
        if base_call_targets[i].addr == addr {
            if (base_call_targets[i].file.is_null() && mapped_file.is_null())
                || (!base_call_targets[i].file.is_null()
                    && !mapped_file.is_null()
                    && libc::strcmp(base_call_targets[i].file, mapped_file) == 0)
            {
                return base_call_targets[i].fn_;
            }
        }
    }
    if !cfg().call_targets.is_null() {
        for i in 0..cfg().call_target_count {
            let target = &*cfg().call_targets.add(i);
            if target.addr == addr {
                if (target.file.is_null() && mapped_file.is_null())
                    || (!target.file.is_null()
                        && !mapped_file.is_null()
                        && libc::strcmp(target.file, mapped_file) == 0)
                {
                    return target.fn_;
                }
            }
        }
    }
    None
}

// stb_image_write's stbiw__crc32 (standard zlib CRC-32) — reproduced so the
// lookup_call_target trace checksum matches the C build byte-for-byte.
unsafe fn stbiw__crc32(buffer: *const u8, len: c_int) -> c_uint {
    let mut crc_table = [0u32; 256];
    for n in 0..256u32 {
        let mut c = n;
        for _ in 0..8 {
            c = if c & 1 != 0 {
                0xedb88320u32 ^ (c >> 1)
            } else {
                c >> 1
            };
        }
        crc_table[n as usize] = c;
    }
    let mut crc: u32 = !0u32;
    for i in 0..len {
        let b = *buffer.add(i as usize);
        crc = (crc >> 8) ^ crc_table[((b as u32) ^ (crc & 0xff)) as usize];
    }
    !crc
}

unsafe fn lookup_call_target(
    addr: u32,
    kind: *const c_char,
    file: *const c_char,
    func: *const c_char,
    line: c_int,
) -> GameFunc {
    let mut sample = [0u8; 8];
    for i in 0..8 {
        sample[i] = *virtual_memory.add(mask_addr(addr + i as u32) as usize);
    }
    let checksum = stbiw__crc32(sample.as_ptr(), sample.len() as c_int);
    let m = find_file_mapping(addr);
    let mut mapped_file: *const c_char = ptr::null();
    let mut offset: u32 = 0;
    if !m.is_null() {
        let slash = libc::strrchr((*m).path, b'/' as c_int);
        mapped_file = if !slash.is_null() {
            slash.add(1)
        } else {
            (*m).path
        };
        offset = ((*m).file_offset + (addr - (*m).base) as usize) as u32;
    }
    shim_log_stdout(
        cstr!("Trace: lookup_call_target: 0x%08X checksum 0x%08X (%s+0x%X)\n"),
        addr as c_uint,
        checksum,
        if mapped_file.is_null() {
            cstr!("<no file>")
        } else {
            mapped_file
        },
        offset as c_uint,
    );
    let fn_ = try_call_target(addr);
    if fn_.is_some() {
        return fn_;
    }
    shim_log_stdout(
        cstr!("Trace: lookup_call_target: address 0x%08X (%s) not mapped (called from %s:%s:%d)\n"),
        addr as c_uint,
        if mapped_file.is_null() {
            cstr!("<no file>")
        } else {
            mapped_file
        },
        file,
        func,
        line,
    );
    report_unmapped(
        if kind.is_null() {
            cstr!("call target")
        } else {
            kind
        },
        addr,
        file,
        func,
        line,
    );
    None
}

// ============================================================================
// Crash bundle writer  [C lines 5931-6186]
// ============================================================================

static mut crash_bundle_dir_cache: [u8; 256] = [0; 256];

unsafe fn crash_bundle_mkdir_parents(dir: *const c_char) -> c_int {
    let mut buf = [0u8; 256];
    let n = libc::strlen(dir);
    if n >= buf.len() {
        return -1;
    }
    libc::memcpy(buf.as_mut_ptr() as *mut c_void, dir as *const c_void, n + 1);
    for i in 1..=n {
        if buf[i] == b'/' || buf[i] == 0 {
            let saved = buf[i];
            buf[i] = 0;
            if libc::mkdir(buf.as_ptr() as *const c_char, 0o755) != 0
                && *libc::__errno_location() != libc::EEXIST
            {
                return -1;
            }
            buf[i] = saved;
        }
    }
    0
}

unsafe fn crash_bundle_create_dir(kind: *const c_char, addr: u32) -> *const c_char {
    if crash_bundle_dir_cache[0] != 0 {
        return ptr::addr_of!(crash_bundle_dir_cache) as *const c_char;
    }
    let now = libc::time(ptr::null_mut());
    let mut tm_buf: libc::tm = core::mem::zeroed();
    libc::localtime_r(&now, &mut tm_buf);
    let mut kind_token = [0u8; 32];
    let mut kt: usize = 0;
    let mut p = if kind.is_null() { cstr!("crash") } else { kind };
    while *p != 0 && kt < kind_token.len() - 1 {
        let ch = *p as u8;
        kind_token[kt] = if ch == b' ' || ch == b'/' { b'_' } else { ch };
        kt += 1;
        p = p.add(1);
    }
    kind_token[kt] = 0;
    libc::snprintf(
        ptr::addr_of_mut!(crash_bundle_dir_cache) as *mut c_char,
        (*ptr::addr_of!(crash_bundle_dir_cache)).len(),
        cstr!("crashes/crash_%04d%02d%02d_%02d%02d%02d_%s_0x%08X"),
        tm_buf.tm_year + 1900,
        tm_buf.tm_mon + 1,
        tm_buf.tm_mday,
        tm_buf.tm_hour,
        tm_buf.tm_min,
        tm_buf.tm_sec,
        kind_token.as_ptr() as *const c_char,
        addr as c_uint,
    );
    if crash_bundle_mkdir_parents(ptr::addr_of!(crash_bundle_dir_cache) as *const c_char) != 0 {
        crash_bundle_dir_cache[0] = 0;
        return ptr::null();
    }
    ptr::addr_of!(crash_bundle_dir_cache) as *const c_char
}

unsafe fn crash_bundle_write_file(
    dir: *const c_char,
    name: *const c_char,
    contents: *const c_char,
    len: usize,
) {
    let mut path = [0u8; 320];
    libc::snprintf(
        path.as_mut_ptr() as *mut c_char,
        path.len(),
        cstr!("%s/%s"),
        dir,
        name,
    );
    let fd = libc::open(
        path.as_ptr() as *const c_char,
        libc::O_WRONLY | libc::O_CREAT | libc::O_TRUNC | libc::O_CLOEXEC,
        0o644,
    );
    if fd < 0 {
        return;
    }
    let mut off: usize = 0;
    while off < len {
        let w = libc::write(fd, contents.add(off) as *const c_void, len - off);
        if w < 0 {
            if *libc::__errno_location() == libc::EINTR {
                continue;
            }
            break;
        }
        off += w as usize;
    }
    libc::close(fd);
}

unsafe fn crash_bundle_write_trace_tail(dir: *const c_char) {
    let mut path = [0u8; 320];
    libc::snprintf(
        path.as_mut_ptr() as *mut c_char,
        path.len(),
        cstr!("%s/trace.tail.log"),
        dir,
    );
    let fd = libc::open(
        path.as_ptr() as *const c_char,
        libc::O_WRONLY | libc::O_CREAT | libc::O_TRUNC | libc::O_CLOEXEC,
        0o644,
    );
    if fd < 0 {
        return;
    }
    trace_ring_dump(fd);
    libc::close(fd);
}

unsafe fn crash_bundle_write_state(dir: *const c_char) {
    let mut buf = [0u8; 8192];
    let mut n: c_int = 0;
    n += libc::snprintf(
        buf.as_mut_ptr().add(n as usize) as *mut c_char,
        buf.len() - n as usize,
        cstr!("cpu: cs:ip=%04X:%04X ss:sp=%04X:%04X ds=%04X es=%04X\n     ax=%04X bx=%04X cx=%04X dx=%04X si=%04X di=%04X bp=%04X\n     flags: CF=%u PF=%u ZF=%u SF=%u OF=%u IF=%u DF=%u\n"),
        cs() as c_uint, ip() as c_uint, ss() as c_uint, sp() as c_uint, ds() as c_uint, es() as c_uint,
        ax() as c_uint, bx() as c_uint, cx() as c_uint, dx() as c_uint, si() as c_uint, di() as c_uint, bp() as c_uint,
        CF() as c_uint, PF() as c_uint, ZF() as c_uint, SF() as c_uint, OF() as c_uint, IF() as c_uint, DF() as c_uint,
    );
    n += libc::snprintf(
        buf.as_mut_ptr().add(n as usize) as *mut c_char,
        buf.len() - n as usize,
        cstr!("lcall_depth=%u\n"),
        lcall_depth as c_uint,
    );
    let mut d: u16 = 1;
    while d <= lcall_depth as u16 && n < buf.len() as c_int {
        n += libc::snprintf(
            buf.as_mut_ptr().add(n as usize) as *mut c_char,
            buf.len() - n as usize,
            cstr!("  [%u] expected_ss:sp=%04X:%04X\n"),
            d as c_uint,
            lcall_expected_ss[d as usize] as c_uint,
            lcall_expected_sp[d as usize] as c_uint,
        );
        d += 1;
    }
    n += libc::snprintf(
        buf.as_mut_ptr().add(n as usize) as *mut c_char,
        buf.len() - n as usize,
        cstr!("isr_depth=%u\n"),
        isr_depth as c_uint,
    );
    let mut d: u16 = 1;
    while d <= isr_depth as u16 && n < buf.len() as c_int {
        n += libc::snprintf(
            buf.as_mut_ptr().add(n as usize) as *mut c_char,
            buf.len() - n as usize,
            cstr!("  [%u] expected_sp=%04X\n"),
            d as c_uint,
            isr_expected_sp[d as usize] as c_uint,
        );
        d += 1;
    }
    n += libc::snprintf(
        buf.as_mut_ptr().add(n as usize) as *mut c_char,
        buf.len() - n as usize,
        cstr!("simulated_stack_top (ss=%04X):\n"),
        ss() as c_uint,
    );
    let mut i = 0;
    while i < 16 && n < buf.len() as c_int {
        let off: u16 = sp().wrapping_add((i * 2) as u16);
        let w = memw_read_impl(
            ss(),
            off,
            SHIMS_FILE,
            cstr!("crash_bundle_write_state"),
            6028,
        );
        n += libc::snprintf(
            buf.as_mut_ptr().add(n as usize) as *mut c_char,
            buf.len() - n as usize,
            cstr!("  ss:%04X = %04X\n"),
            off as c_uint,
            w as c_uint,
        );
        i += 1;
    }
    n += libc::snprintf(
        buf.as_mut_ptr().add(n as usize) as *mut c_char,
        buf.len() - n as usize,
        cstr!("file_mappings (%zu):\n"),
        file_mapping_count,
    );
    let mut i = 0;
    while i < file_mapping_count && n < buf.len() as c_int {
        n += libc::snprintf(
            buf.as_mut_ptr().add(n as usize) as *mut c_char,
            buf.len() - n as usize,
            cstr!("  [%3zu] 0x%05X-0x%05X (len 0x%05X, file_off 0x%X) %s\n"),
            i,
            file_mappings[i].base as c_uint,
            (file_mappings[i].base + file_mappings[i].len as u32) as c_uint,
            file_mappings[i].len as c_uint,
            file_mappings[i].file_offset as c_uint,
            file_mappings[i].path,
        );
        i += 1;
    }
    if n < 0 {
        n = 0;
    }
    if n > buf.len() as c_int {
        n = buf.len() as c_int;
    }
    crash_bundle_write_file(
        dir,
        cstr!("state.txt"),
        buf.as_ptr() as *const c_char,
        n as usize,
    );
}

unsafe fn crash_bundle_write_screenshot(dir: *const c_char) {
    let mut path = [0u8; 320];
    libc::snprintf(
        path.as_mut_ptr() as *mut c_char,
        path.len(),
        cstr!("%s/screenshot.png"),
        dir,
    );
    shim_render_screenshot_png(path.as_ptr() as *const c_char);
}

unsafe fn crash_bundle_write_mappings_json(dir: *const c_char) {
    let mut path = [0u8; 320];
    libc::snprintf(
        path.as_mut_ptr() as *mut c_char,
        path.len(),
        cstr!("%s/file_mappings.json"),
        dir,
    );
    let fd = libc::open(
        path.as_ptr() as *const c_char,
        libc::O_WRONLY | libc::O_CREAT | libc::O_TRUNC | libc::O_CLOEXEC,
        0o644,
    );
    if fd < 0 {
        return;
    }
    let mut line = [0u8; 768];
    let mut n = libc::snprintf(
        line.as_mut_ptr() as *mut c_char,
        line.len(),
        cstr!("{\n  \"cpu\": {\"cs\": \"0x%04X\", \"ip\": \"0x%04X\"},\n  \"file_mappings\": [\n"),
        cs() as c_uint,
        ip() as c_uint,
    );
    if n > 0 {
        let _w = libc::write(fd, line.as_ptr() as *const c_void, n as usize);
    }
    for i in 0..file_mapping_count {
        n = libc::snprintf(
            line.as_mut_ptr() as *mut c_char,
            line.len(),
            cstr!("%s    {\"index\": %zu, \"base\": \"0x%05X\", \"len\": \"0x%zX\", \"file_offset\": \"0x%zX\", \"canonical_cs\": \"0x%04X\", \"loader_cs\": \"0x%04X\", \"loader_ip\": \"0x%04X\", \"loader_ss\": \"0x%04X\", \"loader_sp\": \"0x%04X\", \"loader_stack\": [\"0x%04X\",\"0x%04X\",\"0x%04X\",\"0x%04X\",\"0x%04X\",\"0x%04X\",\"0x%04X\",\"0x%04X\"], \"path\": \"%s\"}"),
            if i != 0 { cstr!(",\n") } else { cstr!("") },
            i,
            file_mappings[i].base as c_uint,
            file_mappings[i].len,
            file_mappings[i].file_offset,
            file_mappings[i].canonical_cs as c_uint,
            file_mappings[i].loader_cs as c_uint,
            file_mappings[i].loader_ip as c_uint,
            file_mappings[i].loader_ss as c_uint,
            file_mappings[i].loader_sp as c_uint,
            file_mappings[i].loader_stack[0] as c_uint,
            file_mappings[i].loader_stack[1] as c_uint,
            file_mappings[i].loader_stack[2] as c_uint,
            file_mappings[i].loader_stack[3] as c_uint,
            file_mappings[i].loader_stack[4] as c_uint,
            file_mappings[i].loader_stack[5] as c_uint,
            file_mappings[i].loader_stack[6] as c_uint,
            file_mappings[i].loader_stack[7] as c_uint,
            if !file_mappings[i].path.is_null() { file_mappings[i].path } else { cstr!("") },
        );
        if n > 0 {
            let _w = libc::write(fd, line.as_ptr() as *const c_void, n as usize);
        }
    }
    let tail: &[u8] = b"\n  ]\n}\n";
    let _w = libc::write(fd, tail.as_ptr() as *const c_void, tail.len());
    libc::close(fd);
}

static mut bundle_extra_writer: Option<unsafe extern "C" fn(*const c_char)> = None;

#[no_mangle]
pub unsafe extern "C" fn shim_set_bundle_extra_writer(
    fn_: Option<unsafe extern "C" fn(*const c_char)>,
) {
    bundle_extra_writer = fn_;
}

/// The launcher exports the repo git hash as SAISEI_RUNTIME_VERSION at run
/// time (no per-commit rebuild); crash manifests fall back to "unknown".
unsafe fn runtime_version() -> *const c_char {
    let v = libc::getenv(cstr!("SAISEI_RUNTIME_VERSION"));
    if v.is_null() || *v == 0 {
        cstr!("unknown")
    } else {
        v
    }
}

unsafe fn crash_bundle_write_manifest(dir: *const c_char, kind: *const c_char, addr: u32) {
    let ab = shim_active_binary();
    let mut buf = [0u8; 1024];
    let mut n = libc::snprintf(
        buf.as_mut_ptr() as *mut c_char,
        buf.len(),
        cstr!("{\n  \"schema\": 1,\n  \"kind\": \"%s\",\n  \"fault_addr\": \"0x%05X\",\n  \"runtime_version\": \"%s\",\n  \"active_binary\": \"%s\",\n  \"cpu\": {\"cs\":\"0x%04X\",\"ip\":\"0x%04X\",\"ss\":\"0x%04X\",\"sp\":\"0x%04X\",\"ds\":\"0x%04X\",\"es\":\"0x%04X\"},\n  \"regs\": {\"ax\":\"0x%04X\",\"bx\":\"0x%04X\",\"cx\":\"0x%04X\",\"dx\":\"0x%04X\",\"si\":\"0x%04X\",\"di\":\"0x%04X\",\"bp\":\"0x%04X\"},\n  \"depths\": {\"lcall\":%u,\"isr\":%u,\"dispatch\":%u,\"critical\":%u}\n}\n"),
        kind,
        addr as c_uint,
        runtime_version(),
        if ab.is_null() { cstr!("<none>") } else { ab },
        cs() as c_uint, ip() as c_uint, ss() as c_uint, sp() as c_uint, ds() as c_uint, es() as c_uint,
        ax() as c_uint, bx() as c_uint, cx() as c_uint, dx() as c_uint, si() as c_uint, di() as c_uint, bp() as c_uint,
        lcall_depth as c_uint, isr_depth as c_uint, dispatch_depth as c_uint, critical_depth as c_uint,
    );
    if n < 0 {
        n = 0;
    }
    if n > buf.len() as c_int {
        n = buf.len() as c_int;
    }
    crash_bundle_write_file(
        dir,
        cstr!("manifest.json"),
        buf.as_ptr() as *const c_char,
        n as usize,
    );
}

unsafe fn save_crash_bundle(
    kind: *const c_char,
    addr: u32,
    crash_text: *const c_char,
    crash_len: usize,
) -> *const c_char {
    let dir = crash_bundle_create_dir(kind, addr);
    if dir.is_null() {
        return ptr::null();
    }
    crash_bundle_write_manifest(dir, kind, addr);
    crash_bundle_write_file(dir, cstr!("crash.txt"), crash_text, crash_len);
    crash_bundle_write_trace_tail(dir);
    crash_bundle_write_state(dir);
    crash_bundle_write_mappings_json(dir);
    lifecycle_dump_to_dir(dir);
    stack_writes_dump_to_dir(dir);
    crash_bundle_write_screenshot(dir);
    if let Some(f) = bundle_extra_writer {
        f(dir);
    }
    session_log_write_to_bundle(dir);
    dir
}

#[no_mangle]
pub unsafe extern "C" fn shim_crash_bundle_write_file(
    dir: *const c_char,
    name: *const c_char,
    contents: *const c_char,
    len: usize,
) {
    crash_bundle_write_file(dir, name, contents, len);
}
#[no_mangle]
pub unsafe extern "C" fn shim_crash_bundle_write_state(dir: *const c_char) {
    crash_bundle_write_state(dir);
}
#[no_mangle]
pub unsafe extern "C" fn shim_crash_bundle_write_trace_tail(dir: *const c_char) {
    crash_bundle_write_trace_tail(dir);
}
#[no_mangle]
pub unsafe extern "C" fn shim_lifecycle_dump_to_dir(dir: *const c_char) {
    lifecycle_dump_to_dir(dir);
}

// ============================================================================
// Snapshot module access (kbd queue + file_mappings)  [C lines 6193-6259]
// ============================================================================

#[no_mangle]
pub unsafe extern "C" fn shim_kbd_state_capture(out: *mut ShimKbdState) {
    for i in 0..SHIM_KBD_BUFFER_SIZE {
        (*out).q_ascii[i] = kbd.queue[i].ascii;
        (*out).q_scan[i] = kbd.queue[i].scancode;
    }
    (*out).head = kbd.queue_head;
    (*out).tail = kbd.queue_tail;
    (*out).count = kbd.queue_count;
    (*out).cur_ascii = kbd.ascii;
    (*out).cur_scan = kbd.scancode;
    (*out).last_scan = kbd.last_scancode;
    (*out).ready = kbd.scancode_ready as u8;
}

#[no_mangle]
pub unsafe extern "C" fn shim_kbd_state_restore(in_: *const ShimKbdState) {
    for i in 0..SHIM_KBD_BUFFER_SIZE {
        kbd.queue[i].ascii = (*in_).q_ascii[i];
        kbd.queue[i].scancode = (*in_).q_scan[i];
    }
    kbd.queue_head = (*in_).head;
    kbd.queue_tail = (*in_).tail;
    kbd.queue_count = (*in_).count;
    kbd.ascii = (*in_).cur_ascii;
    kbd.scancode = (*in_).cur_scan;
    kbd.last_scancode = (*in_).last_scan;
    kbd.scancode_ready = (*in_).ready as i32;
}

#[no_mangle]
pub unsafe extern "C" fn shim_file_mappings_count() -> usize {
    file_mapping_count
}

#[no_mangle]
pub unsafe extern "C" fn shim_file_mappings_get(i: usize, out: *mut ShimFileMappingView) {
    if i >= file_mapping_count {
        libc::memset(
            out as *mut c_void,
            0,
            core::mem::size_of::<ShimFileMappingView>(),
        );
        return;
    }
    (*out).base = file_mappings[i].base;
    (*out).len = file_mappings[i].len;
    (*out).file_offset = file_mappings[i].file_offset;
    (*out).canonical_cs = file_mappings[i].canonical_cs;
    (*out).path = file_mappings[i].path;
}

#[no_mangle]
pub unsafe extern "C" fn shim_file_mappings_reset() {
    file_mapping_count = 0;
}

#[no_mangle]
pub unsafe extern "C" fn shim_file_mappings_add_for_restore(
    path: *const c_char,
    base: u32,
    len: usize,
    file_offset: usize,
    canonical_cs: u16,
) -> c_int {
    evict_or_shrink_for_load(base, len);
    if file_mapping_count >= MAX_FILE_MAPPINGS {
        return -1;
    }
    file_mappings[file_mapping_count].path = libc::strdup(path);
    file_mappings[file_mapping_count].base = base;
    file_mappings[file_mapping_count].len = len;
    file_mappings[file_mapping_count].file_offset = file_offset;
    file_mappings[file_mapping_count].data = ptr::null_mut();
    file_mappings[file_mapping_count].canonical_cs = canonical_cs;
    file_mapping_count += 1;
    mem_page_flags_recompute();
    0
}

unsafe fn save_bug_bundle(kind: *const c_char, addr: u32, msg: *const c_char) {
    let dir = save_crash_bundle(kind, addr, msg, libc::strlen(msg));
    if !dir.is_null() {
        shim_log_crash(cstr!("Bundle: %s\n"), dir);
    }
}

#[no_mangle]
pub unsafe extern "C" fn shim_save_bug_bundle(kind: *const c_char, addr: u32, msg: *const c_char) {
    save_bug_bundle(kind, addr, msg);
}

// ---- Test seam: mockable fatal (unmapped dispatch) path ----
static mut shim_fatal_armed: c_int = 0;
#[no_mangle]
pub static mut shim_fatal_captured: c_int = 0;
#[no_mangle]
pub static mut shim_fatal_addr: u32 = 0;
#[no_mangle]
pub static mut shim_fatal_kind: [u8; 32] = [0; 32];

#[no_mangle]
pub unsafe extern "C" fn shim_arm_fatal_capture() {
    shim_fatal_armed = 1;
    shim_fatal_captured = 0;
    shim_fatal_addr = 0;
    shim_fatal_kind[0] = 0;
}

#[no_mangle]
pub unsafe extern "C" fn shim_disarm_fatal_capture() {
    shim_fatal_armed = 0;
}

unsafe fn report_unmapped(
    kind: *const c_char,
    addr: u32,
    caller_file: *const c_char,
    caller_func: *const c_char,
    line: c_int,
) {
    if shim_fatal_armed != 0 {
        // TODO: fatal-capture test path — the C longjmp'd back to the armed
        // entry wrapper; here we set the capture flags and return without
        // unwinding (test-only; no game path uses it).
        shim_fatal_captured = 1;
        shim_fatal_addr = addr;
        libc::snprintf(
            ptr::addr_of_mut!(shim_fatal_kind) as *mut c_char,
            (*ptr::addr_of!(shim_fatal_kind)).len(),
            cstr!("%s"),
            if kind.is_null() { cstr!("") } else { kind },
        );
        return;
    }
    let m = find_file_mapping(addr);
    let mut bytes = [0u8; 8];
    for i in 0..8 {
        bytes[i] = *seg_off((addr >> 4) as u16, ((addr & 0xF) + i as u32) as u16);
    }
    let mut hex = [0u8; 32];
    let mut hp: c_int = 0;
    for i in 0..8 {
        hp += libc::snprintf(
            hex.as_mut_ptr().add(hp as usize) as *mut c_char,
            hex.len() - hp as usize,
            cstr!("%s%02X"),
            if i != 0 { cstr!(" ") } else { cstr!("") },
            bytes[i] as c_uint,
        );
    }
    let mut ascii = [0u8; 16];
    for i in 0..8 {
        ascii[i] = if libc::isprint(bytes[i] as c_int) != 0 {
            bytes[i]
        } else {
            b'.'
        };
    }
    ascii[8] = 0;

    let mut block = [0u8; 1536];
    let mut n: c_int;
    if !m.is_null() {
        let offset: u32 = ((*m).file_offset + (addr - (*m).base) as usize) as u32;
        let mut mapped_file = (*m).path;
        let slash = libc::strrchr(mapped_file, b'/' as c_int);
        if !slash.is_null() {
            mapped_file = slash.add(1);
        }
        let mut stem = [0u8; 256];
        libc::strncpy(stem.as_mut_ptr() as *mut c_char, mapped_file, stem.len());
        stem[stem.len() - 1] = 0;
        let dot = libc::strrchr(stem.as_mut_ptr() as *mut c_char, b'.' as c_int);
        if !dot.is_null() {
            *dot = 0;
        }
        let game_name = if !cfg().name.is_null() {
            cfg().name
        } else {
            cstr!("game")
        };
        n = libc::snprintf(
            block.as_mut_ptr() as *mut c_char,
            block.len(),
            cstr!("======== CRASH ========\nCPU state at unmapped %s: cs:ip=%04X:%04X ss:sp=%04X:%04X ds=%04X es=%04X ax=%04X bx=%04X cx=%04X dx=%04X si=%04X di=%04X\nError: %s address 0x%08X is not mapped (called from %s:%s:%d; offset 0x%X in %s)\nBytes at 0x%08X: %s |%s|\nTo fix: verify offset 0x%X in %s looks like code. If so, add 0x%X to extra_entries in resources/%s.json and add an entry to call_targets in games/%s.json\n=======================\n"),
            kind, cs() as c_uint, ip() as c_uint, ss() as c_uint, sp() as c_uint, ds() as c_uint, es() as c_uint,
            ax() as c_uint, bx() as c_uint, cx() as c_uint, dx() as c_uint, si() as c_uint, di() as c_uint,
            kind, addr as c_uint, caller_file, caller_func, line, offset as c_uint, mapped_file,
            addr as c_uint, hex.as_ptr() as *const c_char, ascii.as_ptr() as *const c_char,
            offset as c_uint, mapped_file, offset as c_uint, stem.as_ptr() as *const c_char, game_name,
        );
    } else {
        let game_name = if !cfg().name.is_null() {
            cfg().name
        } else {
            cstr!("game")
        };
        n = libc::snprintf(
            block.as_mut_ptr() as *mut c_char,
            block.len(),
            cstr!("======== CRASH ========\nCPU state at unmapped %s: cs:ip=%04X:%04X ss:sp=%04X:%04X ds=%04X es=%04X ax=%04X bx=%04X cx=%04X dx=%04X si=%04X di=%04X\nError: %s address 0x%08X is not mapped (called from %s:%s:%d; no file loaded). Update call_targets in games/%s.json if this should map to translated code.\nBytes at 0x%08X: %s |%s|\n=======================\n"),
            kind, cs() as c_uint, ip() as c_uint, ss() as c_uint, sp() as c_uint, ds() as c_uint, es() as c_uint,
            ax() as c_uint, bx() as c_uint, cx() as c_uint, dx() as c_uint, si() as c_uint, di() as c_uint,
            kind, addr as c_uint, caller_file, caller_func, line, game_name,
            addr as c_uint, hex.as_ptr() as *const c_char, ascii.as_ptr() as *const c_char,
        );
    }

    if n <= 0 {
        let fallback = cstr!("[CRASH report could not be formatted]\n");
        n = libc::strlen(fallback) as c_int;
        libc::memcpy(
            block.as_mut_ptr() as *mut c_void,
            fallback as *const c_void,
            n as usize + 1,
        );
    } else if n >= block.len() as c_int {
        n = block.len() as c_int - 1;
    }
    let bundle_dir = save_crash_bundle(kind, addr, block.as_ptr() as *const c_char, n as usize);
    if !bundle_dir.is_null() && n + 64 < block.len() as c_int {
        let mut extra = [0u8; 256];
        let en = libc::snprintf(
            extra.as_mut_ptr() as *mut c_char,
            extra.len(),
            cstr!("Bundle: %s\n"),
            bundle_dir,
        );
        let closer = cstr!("=======================\n");
        let pos = libc::strstr(block.as_ptr() as *const c_char, closer);
        if !pos.is_null()
            && en > 0
            && (pos.offset_from(block.as_ptr() as *const c_char) as usize
                + en as usize
                + libc::strlen(closer)
                + 1)
                < block.len()
        {
            libc::memmove(
                pos.add(en as usize) as *mut c_void,
                pos as *const c_void,
                libc::strlen(closer) + 1,
            );
            libc::memcpy(
                pos as *mut c_void,
                extra.as_ptr() as *const c_void,
                en as usize,
            );
            n += en;
        }
    }
    shim_flush_all_streams();
    let target_fd: c_int;
    let mut tty_fd: c_int = -1;
    let out_fd = libc::fileno(stdout);
    if out_fd >= 0 && libc::isatty(out_fd) != 0 {
        tty_fd = libc::open(cstr!("/dev/tty"), libc::O_WRONLY | libc::O_CLOEXEC);
        target_fd = if tty_fd >= 0 { tty_fd } else { out_fd };
    } else {
        target_fd = out_fd;
    }
    if target_fd >= 0 {
        let mut off: c_int = 0;
        while off < n {
            let w = libc::write(
                target_fd,
                block.as_ptr().add(off as usize) as *const c_void,
                (n - off) as usize,
            );
            if w < 0 {
                if *libc::__errno_location() == libc::EINTR {
                    continue;
                }
                break;
            }
            off += w as c_int;
        }
        libc::fsync(target_fd);
    }
    if tty_fd >= 0 {
        libc::close(tty_fd);
    }
    libc::exit(1);
}

// ============================================================================
// Dispatch primitives: long_jump / retf / iret / lcall / call  [C 6464-6626]
// ============================================================================

#[no_mangle]
pub unsafe extern "C" fn long_jump_impl(
    seg: u16,
    off: u16,
    file: *const c_char,
    func: *const c_char,
    line: c_int,
) {
    let addr = ((seg as u32) << 4) + off as u32;
    shim_log_stdout(
        cstr!("Trace: long_jump to %04X:%04X (0x%08X) (%s:%s:%d)\n"),
        seg as c_uint,
        off as c_uint,
        addr as c_uint,
        file,
        func,
        line,
    );
    lifecycle_log_dispatch(cstr!("LJMP"), addr);
    set_cs(seg);
    set_ip(off);
    record_binary_cs(addr, seg);
}

unsafe fn retf_common_impl(file: *const c_char, func: *const c_char, line: c_int, pop_bytes: u16) {
    let frame_ss = ss();
    let sp_before = sp();
    let new_ip = memw_read_impl(
        frame_ss,
        sp_before,
        SHIMS_FILE,
        cstr!("retf_common_impl"),
        6485,
    );
    let seg = memw_read_impl(
        frame_ss,
        (sp_before as u32 + 2) as u16 & 0xFFFF,
        SHIMS_FILE,
        cstr!("retf_common_impl"),
        6486,
    );
    set_sp(((sp_before as u32 + 4 + pop_bytes as u32) & 0xFFFF) as u16);
    shim_log_stdout(
        cstr!("Trace: retf -> %04X:%04X sp=%04X pop=%u (%s:%s:%d)\n"),
        seg as c_uint,
        new_ip as c_uint,
        sp_before as c_uint,
        pop_bytes as c_uint,
        file,
        func,
        line,
    );
    set_cs(seg);
    set_ip(new_ip);
}

#[no_mangle]
pub unsafe extern "C" fn retf_impl(file: *const c_char, func: *const c_char, line: c_int) {
    retf_common_impl(file, func, line, 0);
}

#[no_mangle]
pub unsafe extern "C" fn retf_pop_impl(
    file: *const c_char,
    func: *const c_char,
    line: c_int,
    pop_bytes: u16,
) {
    retf_common_impl(file, func, line, pop_bytes);
}

#[no_mangle]
pub unsafe extern "C" fn retf() {
    retf_impl(cstr!("<external>"), cstr!("retf"), 0);
}

#[no_mangle]
pub unsafe extern "C" fn iret_impl(file: *const c_char, func: *const c_char, line: c_int) {
    let sp_before = sp();
    let old_if = IF();
    let new_ip = memw_read_impl(ss(), sp_before, SHIMS_FILE, cstr!("iret_impl"), 6512);
    let seg = memw_read_impl(
        ss(),
        (sp_before as u32 + 2) as u16 & 0xFFFF,
        SHIMS_FILE,
        cstr!("iret_impl"),
        6513,
    );
    let flags = memw_read_impl(
        ss(),
        (sp_before as u32 + 4) as u16 & 0xFFFF,
        SHIMS_FILE,
        cstr!("iret_impl"),
        6514,
    );
    set_sp(((sp_before as u32 + 6) & 0xFFFF) as u16);
    shim_log_stdout(
        cstr!("Trace: iret -> %04X:%04X flags=0x%04X depth=%d sp=%04X (%s:%s:%d)\n"),
        seg as c_uint,
        new_ip as c_uint,
        flags as c_uint,
        isr_depth as c_int,
        sp_before as c_uint,
        file,
        func,
        line,
    );
    set_CF((flags & 1) as u8);
    set_PF(((flags >> 2) & 1) as u8);
    set_ZF(((flags >> 6) & 1) as u8);
    set_SF(((flags >> 7) & 1) as u8);
    set_IF(((flags >> 9) & 1) as u8);
    set_DF(((flags >> 10) & 1) as u8);
    set_OF(((flags >> 11) & 1) as u8);
    // IRET does NOT create an interrupt shadow. Only STI, MOV SS and POP SS
    // inhibit interrupt recognition for the following instruction on x86; after
    // an IRET that re-enables IF, a pending maskable interrupt is recognized
    // immediately. Setting the shadow here was unfaithful, and with the
    // per-basic-block safepoint model it became a hard hang: a loop that calls
    // a software interrupt every iteration (Zeliard's shop music-poll: INT 61h,
    // whose handler returns with IF 0->1) re-armed this shadow between the rare
    // budget-gated safepoints, so every safepoint early-returned before
    // delivering the pending timer IRQ0 — the hooked INT8 music tick never ran,
    // the RCB flag it sets never changed, and the poll loop spun forever. (The
    // old per-instruction safepoint masked this: a safepoint ran the very next
    // instruction, consumed the shadow, and the one after delivered the IRQ.)
    set_cs(seg);
    set_ip(new_ip);
    // IRET can raise IF (0->1 from the popped flags), which opens the delivery
    // gate — so the next block boundary must be a recognition point.
    if old_if == 0 && IF() != 0 {
        shim_irq_recheck();
    }
}

#[no_mangle]
pub unsafe extern "C" fn iret() {
    iret_impl(cstr!("<external>"), cstr!("iret"), 0);
}

#[no_mangle]
pub unsafe extern "C" fn lcall_table_impl(
    ret_ip: u16,
    seg: u16,
    off: u16,
    file: *const c_char,
    func: *const c_char,
    line: c_int,
) {
    let addr = ((seg as u32) << 4) + off as u32;
    shim_log_stdout(
        cstr!("Trace: lcall_table to %04X:%04X (0x%08X) (%s:%s:%d)\n"),
        seg as c_uint,
        off as c_uint,
        addr as c_uint,
        file,
        func,
        line,
    );
    lifecycle_log_dispatch(cstr!("LCALL"), addr);
    let sp_before = sp();
    set_sp(((sp() as u32).wrapping_sub(2) & 0xFFFF) as u16);
    memw_write_impl(
        ss(),
        sp(),
        cs(),
        SHIMS_FILE,
        cstr!("lcall_table_impl"),
        6549,
    );
    set_sp(((sp() as u32).wrapping_sub(2) & 0xFFFF) as u16);
    memw_write_impl(
        ss(),
        sp(),
        ret_ip,
        SHIMS_FILE,
        cstr!("lcall_table_impl"),
        6551,
    );
    shim_log_stdout(
        cstr!("Trace: lcall push ret_ip=%04X saved_cs=%04X sp=%04X -> %04X\n"),
        ret_ip as c_uint,
        cs() as c_uint,
        sp_before as c_uint,
        sp() as c_uint,
    );
    set_cs(seg);
    set_ip(off);
    record_binary_cs(addr, seg);
}

#[no_mangle]
pub unsafe extern "C" fn call_table_impl(
    ret_ip: u16,
    addr: u32,
    file: *const c_char,
    func: *const c_char,
    line: c_int,
) {
    shim_log_stdout(
        cstr!("Trace: call_table 0x%08X (%s:%s:%d)\n"),
        addr as c_uint,
        file,
        func,
        line,
    );
    lifecycle_log_dispatch(cstr!("CALL"), addr);
    set_sp(((sp() as u32).wrapping_sub(2) & 0xFFFF) as u16);
    memw_write_impl(
        ss(),
        sp(),
        ret_ip,
        SHIMS_FILE,
        cstr!("call_table_impl"),
        6571,
    );
    set_ip((addr.wrapping_sub((cs() as u32) << 4)) as u16);
}

unsafe fn find_dispatch_by_source_file(file: *const c_char) -> *const BinaryDispatch {
    if file.is_null() || cfg().binary_dispatch.is_null() {
        return ptr::null();
    }
    let slash = libc::strrchr(file, b'/' as c_int);
    let base = if !slash.is_null() { slash.add(1) } else { file };
    let mut n = libc::strlen(base);
    if n > 2 && *base.add(n - 2) as u8 == b'.' && *base.add(n - 1) as u8 == b'c' {
        n -= 2;
    }
    for i in 0..cfg().binary_dispatch_count {
        let bd = &*cfg().binary_dispatch.add(i);
        if !bd.module.is_null()
            && bd.fn_.is_some()
            && libc::strlen(bd.module) == n
            && libc::strncmp(bd.module, base, n) == 0
        {
            return bd;
        }
    }
    ptr::null()
}

#[no_mangle]
pub static mut tail_dispatch_pending: bool = false;
#[no_mangle]
pub static mut tail_dispatch_addr: u32 = 0;
#[no_mangle]
pub static mut tail_dispatch_expected: u16 = 0;

unsafe fn find_binary_for_addr(
    addr: u32,
    out_fm: *mut *const FileMapping,
) -> *const BinaryDispatch {
    if cfg().binary_dispatch.is_null() {
        return ptr::null();
    }
    let fm = find_file_mapping(addr);
    if fm.is_null() || (*fm).path.is_null() {
        return ptr::null();
    }
    let bn0 = libc::strrchr((*fm).path, b'/' as c_int);
    let bn = if !bn0.is_null() {
        bn0.add(1)
    } else {
        (*fm).path
    };
    for i in 0..cfg().binary_dispatch_count {
        let bd = &*cfg().binary_dispatch.add(i);
        if !bd.file.is_null() && bd.fn_.is_some() && libc::strcmp(bd.file, bn) == 0 {
            if !out_fm.is_null() {
                *out_fm = fm;
            }
            return bd;
        }
    }
    ptr::null()
}

// ============================================================================
// JIT recompiler  [C lines 6662-7028]
// ============================================================================

#[repr(C)]
#[derive(Clone, Copy)]
struct JitChunk {
    seg_base: u32,
    lo: u32,
    hi: u32,
    keys: *mut u32,
    nkeys: usize,
    code: *mut u32,
    ncode: usize,
    fn_: DispatchFn,
    handle: *mut c_void,
    stale: c_int,
    /// Next chunk index with the same seg_base (newest-first bucket list of
    /// `jit_seg_heads`), -1 = end. Maintained by `jit_seg_index_relink`.
    next_same_seg: i32,
}
impl JitChunk {
    const ZERO: JitChunk = JitChunk {
        seg_base: 0,
        lo: 0,
        hi: 0,
        keys: ptr::null_mut(),
        nkeys: 0,
        code: ptr::null_mut(),
        ncode: 0,
        fn_: None,
        handle: ptr::null_mut(),
        stale: 0,
        next_same_seg: -1,
    };
}
const MAX_JIT_CHUNKS: usize = 1024;
static mut jit_chunks: [JitChunk; MAX_JIT_CHUNKS] = [JitChunk::ZERO; MAX_JIT_CHUNKS];
static mut jit_chunk_count: usize = 0;
static mut jit_code_lo: u32 = 0xFFFFFFFF;
static mut jit_code_hi: u32 = 0;

/// Per-segbase chunk index: head of a newest-first list of chunk indices for
/// each 16-byte-aligned seg base (encoded idx+1, 0 = empty; zero-init → BSS).
/// The dispatch hot path resolves the live cs's chunk in O(chunks-at-this-cs)
/// instead of scanning the whole registry newest-first.
static mut jit_seg_heads: [u16; MEMORY_SIZE >> 4] = [0; MEMORY_SIZE >> 4];

/// (Re-)insert chunk `idx` at the head of its seg_base bucket. A reused stale
/// slot is already in the list (same seg_base) — unlink it first so the bucket
/// stays newest-first and cycle-free.
unsafe fn jit_seg_index_relink(idx: usize) {
    let slot = (jit_chunks[idx].seg_base >> 4) as usize;
    let mut cur = jit_seg_heads[slot] as i32 - 1;
    if cur == idx as i32 {
        jit_seg_heads[slot] = (jit_chunks[idx].next_same_seg + 1) as u16;
    } else {
        while cur >= 0 {
            let nxt = jit_chunks[cur as usize].next_same_seg;
            if nxt == idx as i32 {
                jit_chunks[cur as usize].next_same_seg = jit_chunks[idx].next_same_seg;
                break;
            }
            cur = nxt;
        }
    }
    jit_chunks[idx].next_same_seg = jit_seg_heads[slot] as i32 - 1;
    jit_seg_heads[slot] = (idx + 1) as u16;
}

/// Newest-first lookup restricted to one seg base: the chunk that decodes
/// `seg_base:off` (off must be a decoded case key), or null. This is the
/// dispatch fast path — a far transfer lands at the live cs almost always.
unsafe fn jit_lookup_at_base(seg_base: u32, off: u32) -> *mut JitChunk {
    if off >= 0x10000 {
        return ptr::null_mut();
    }
    let mut cur = jit_seg_heads[(seg_base >> 4) as usize] as i32 - 1;
    while cur >= 0 {
        let c = &mut jit_chunks[cur as usize] as *mut JitChunk;
        if (*c).stale == 0 && off >= (*c).lo && off < (*c).hi && jit_chunk_has_key(c, off) != 0 {
            return c;
        }
        cur = (*c).next_same_seg;
    }
    ptr::null_mut()
}

unsafe fn jit_chunk_has_key(c: *const JitChunk, off: u32) -> c_int {
    if (*c).keys.is_null() || (*c).nkeys == 0 {
        return 1;
    }
    let mut lo: usize = 0;
    let mut hi: usize = (*c).nkeys;
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        if *(*c).keys.add(mid) < off {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    (lo < (*c).nkeys && *(*c).keys.add(lo) == off) as c_int
}

unsafe fn jit_load_keys(c: *mut JitChunk, so_path: *const c_char) {
    (*c).keys = ptr::null_mut();
    (*c).nkeys = 0;
    (*c).code = ptr::null_mut();
    (*c).ncode = 0;
    let n = libc::strlen(so_path);
    if n < 3 || n + 6 >= 1100 {
        return;
    }
    let mut kp = [0u8; 1104];
    libc::memcpy(
        kp.as_mut_ptr() as *mut c_void,
        so_path as *const c_void,
        n - 3,
    );
    libc::memcpy(
        kp.as_mut_ptr().add(n - 3) as *mut c_void,
        cstr!(".keys") as *const c_void,
        6,
    );
    let f = libc::fopen(kp.as_ptr() as *const c_char, cstr!("rb"));
    if !f.is_null() {
        let mut cnt: u32 = 0;
        if libc::fread(
            &mut cnt as *mut u32 as *mut c_void,
            core::mem::size_of::<u32>(),
            1,
            f,
        ) == 1
            && cnt > 0
            && cnt < (1u32 << 24)
        {
            let arr = libc::malloc(cnt as usize * core::mem::size_of::<u32>()) as *mut u32;
            if !arr.is_null()
                && libc::fread(
                    arr as *mut c_void,
                    core::mem::size_of::<u32>(),
                    cnt as usize,
                    f,
                ) == cnt as usize
            {
                (*c).keys = arr;
                (*c).nkeys = cnt as usize;
            } else {
                libc::free(arr as *mut c_void);
            }
        }
        libc::fclose(f);
    }
    libc::memcpy(
        kp.as_mut_ptr().add(n - 3) as *mut c_void,
        cstr!(".code") as *const c_void,
        6,
    );
    let f = libc::fopen(kp.as_ptr() as *const c_char, cstr!("rb"));
    if !f.is_null() {
        let mut cnt: u32 = 0;
        if libc::fread(
            &mut cnt as *mut u32 as *mut c_void,
            core::mem::size_of::<u32>(),
            1,
            f,
        ) == 1
            && cnt > 0
            && cnt < (1u32 << 24)
        {
            let arr = libc::malloc(cnt as usize * 2 * core::mem::size_of::<u32>()) as *mut u32;
            if !arr.is_null()
                && libc::fread(
                    arr as *mut c_void,
                    core::mem::size_of::<u32>(),
                    cnt as usize * 2,
                    f,
                ) == cnt as usize * 2
            {
                (*c).code = arr;
                (*c).ncode = cnt as usize;
            } else {
                libc::free(arr as *mut c_void);
            }
        }
        libc::fclose(f);
    }
}

unsafe fn jit_range_hits_code(c: *const JitChunk, k_lo: u32, k_hi: u32) -> c_int {
    let mut lo: usize = 0;
    let mut hi: usize = (*c).ncode;
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        if *(*c).code.add(2 * mid + 1) <= k_lo {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    (lo < (*c).ncode && *(*c).code.add(2 * lo) < k_hi) as c_int
}

unsafe fn jit_lookup(linear: u32) -> *mut JitChunk {
    let mut i = jit_chunk_count as isize - 1;
    while i >= 0 {
        let c = &mut jit_chunks[i as usize] as *mut JitChunk;
        if (*c).stale != 0 {
            i -= 1;
            continue;
        }
        if linear >= (*c).seg_base + (*c).lo
            && linear < (*c).seg_base + (*c).hi
            && jit_chunk_has_key(c, linear - (*c).seg_base) != 0
        {
            return c;
        }
        i -= 1;
    }
    ptr::null_mut()
}

#[no_mangle]
pub unsafe extern "C" fn shim_pc_is_jit_case_key(seg: u16, off: u16) -> c_int {
    (!jit_lookup_at_base((seg as u32) << 4, off as u32).is_null()) as c_int
}

unsafe fn jit_invalidate_range_impl(lin: u32, len: u32) {
    if jit_chunk_count == 0 || len == 0 {
        return;
    }
    if lin + len <= jit_code_lo || lin >= jit_code_hi {
        return;
    }
    let w_lo = lin;
    let w_hi = lin + len;
    for i in 0..jit_chunk_count {
        let c = &mut jit_chunks[i] as *mut JitChunk;
        if (*c).stale != 0 {
            continue;
        }
        let c_lo = (*c).seg_base + (*c).lo;
        let c_hi = (*c).seg_base + (*c).hi;
        if w_hi <= c_lo || w_lo >= c_hi {
            continue;
        }
        let k_lo = if w_lo > (*c).seg_base {
            w_lo - (*c).seg_base
        } else {
            0
        };
        let k_hi = w_hi - (*c).seg_base;
        if (*c).ncode != 0 && jit_range_hits_code(c, k_lo, k_hi) == 0 {
            continue;
        }
        (*c).stale = 1;
        shim_log_stdout(
            cstr!("JIT: invalidate chunk %05X:[%04X,%04X) -- write 0x%05X..0x%05X overwrote a decoded instruction\n"),
            (*c).seg_base as c_uint,
            (*c).lo as c_uint,
            (*c).hi as c_uint,
            w_lo as c_uint,
            w_hi as c_uint,
        );
    }
}

#[no_mangle]
pub unsafe extern "C" fn shim_jit_invalidate_code_range(lin: u32, len: u32) {
    jit_invalidate_range_impl(lin, len);
}

#[no_mangle]
pub unsafe extern "C" fn shim_jit_invalidate_code_range_force(lin: u32, len: u32) {
    jit_invalidate_range_impl(lin, len);
}

unsafe fn jit_dispatch(
    c: *mut JitChunk,
    linear: u32,
    expected_retip: u16,
    file: *const c_char,
    func: *const c_char,
    line: c_int,
) -> c_int {
    set_cs(((*c).seg_base >> 4) as u16);
    ((*c).fn_.unwrap())(
        (linear - (*c).seg_base) as c_int,
        expected_retip,
        file,
        func,
        line,
    );
    shim_drain_pending_tail_dispatch(file, func, line);
    1
}

unsafe fn jit_compile_or_get(seg: u16, off: u16) -> *mut JitChunk {
    let seg_base = (seg as u32) << 4;
    let existing = jit_lookup_at_base(seg_base, off as u32);
    if !existing.is_null() {
        return existing;
    }
    let repo = libc::getenv(cstr!("SAISEI_REPO_ROOT"));
    if repo.is_null()
        || libc::getenv(cstr!("SAISEI_JITC")).is_null()
        || jit_chunk_count >= MAX_JIT_CHUNKS
    {
        return ptr::null_mut();
    }
    if seg_base as usize + 0x10000 > MEMORY_SIZE {
        return ptr::null_mut();
    }

    vclock_halt();
    let mut dir = [0u8; 1024];
    let mut dump = [0u8; 1100];
    let mut cmd = [0u8; 4096];
    let mut jitcap = [0u8; 8192];
    let mut jitcaplen: usize = 0;
    jitcap[0] = 0;
    let jit_dir = libc::getenv(cstr!("SAISEI_JIT_DIR"));
    if !jit_dir.is_null() && *jit_dir != 0 {
        libc::snprintf(
            dir.as_mut_ptr() as *mut c_char,
            dir.len(),
            cstr!("%s"),
            jit_dir,
        );
    } else {
        libc::snprintf(
            dir.as_mut_ptr() as *mut c_char,
            dir.len(),
            cstr!("%s/build/jit"),
            repo,
        );
    }
    libc::mkdir(dir.as_ptr() as *const c_char, 0o755);
    libc::snprintf(
        dump.as_mut_ptr() as *mut c_char,
        dump.len(),
        cstr!("%s/seg_%05X.bin"),
        dir.as_ptr() as *const c_char,
        seg_base as c_uint,
    );
    let mut result: *mut JitChunk = ptr::null_mut();
    let fp = libc::fopen(dump.as_ptr() as *const c_char, cstr!("wb"));
    if !fp.is_null() {
        libc::fwrite(
            virtual_memory.add(seg_base as usize) as *const c_void,
            1,
            0x10000,
            fp,
        );
        libc::fclose(fp);
        let jitc = libc::getenv(cstr!("SAISEI_JITC"));
        libc::snprintf(
            cmd.as_mut_ptr() as *mut c_char,
            cmd.len(),
            cstr!("'%s' jit-compile --mem '%s' --entry 0x%X --name jit_%05x_%04x --image-base 0x%X --outdir '%s' 2>&1"),
            jitc,
            dump.as_ptr() as *const c_char,
            off as c_uint,
            seg_base as c_uint,
            off as c_uint,
            seg_base as c_uint,
            dir.as_ptr() as *const c_char,
        );
        let pp = libc::popen(cmd.as_ptr() as *const c_char, cstr!("r"));
        if !pp.is_null() {
            let mut so = [0u8; 1024];
            let mut sym = [0u8; 256];
            let mut line = [0u8; 1200];
            let mut lo: c_uint = 0;
            let mut hi: c_uint = 0;
            while !libc::fgets(line.as_mut_ptr() as *mut c_char, line.len() as c_int, pp).is_null()
            {
                if libc::strncmp(line.as_ptr() as *const c_char, cstr!("SO "), 3) == 0 {
                    libc::sscanf(
                        line.as_ptr().add(3) as *const c_char,
                        cstr!("%1023s"),
                        so.as_mut_ptr() as *mut c_char,
                    );
                } else if libc::strncmp(line.as_ptr() as *const c_char, cstr!("SYM "), 4) == 0 {
                    libc::sscanf(
                        line.as_ptr().add(4) as *const c_char,
                        cstr!("%255s"),
                        sym.as_mut_ptr() as *mut c_char,
                    );
                } else if libc::strncmp(line.as_ptr() as *const c_char, cstr!("RANGE "), 6) == 0 {
                    libc::sscanf(
                        line.as_ptr().add(6) as *const c_char,
                        cstr!("0x%x 0x%x"),
                        &mut lo as *mut c_uint,
                        &mut hi as *mut c_uint,
                    );
                }
                let ll = libc::strlen(line.as_ptr() as *const c_char);
                if ll >= jitcap.len() {
                    libc::memcpy(
                        jitcap.as_mut_ptr() as *mut c_void,
                        line.as_ptr().add(ll - (jitcap.len() - 1)) as *const c_void,
                        jitcap.len() - 1,
                    );
                    jitcaplen = jitcap.len() - 1;
                } else {
                    if jitcaplen + ll > jitcap.len() - 1 {
                        let drop = jitcaplen + ll - (jitcap.len() - 1);
                        libc::memmove(
                            jitcap.as_mut_ptr() as *mut c_void,
                            jitcap.as_ptr().add(drop) as *const c_void,
                            jitcaplen - drop,
                        );
                        jitcaplen -= drop;
                    }
                    libc::memcpy(
                        jitcap.as_mut_ptr().add(jitcaplen) as *mut c_void,
                        line.as_ptr() as *const c_void,
                        ll,
                    );
                    jitcaplen += ll;
                }
                jitcap[jitcaplen] = 0;
            }
            let rc = libc::pclose(pp);
            if rc == 0 && so[0] != 0 && sym[0] != 0 && hi > lo {
                let h = libc::dlopen(
                    so.as_ptr() as *const c_char,
                    libc::RTLD_NOW | libc::RTLD_GLOBAL,
                );
                if !h.is_null() {
                    let symp = libc::dlsym(h, sym.as_ptr() as *const c_char);
                    if !symp.is_null() {
                        let cfn: DispatchFn = core::mem::transmute(symp);
                        let mut c: *mut JitChunk = ptr::null_mut();
                        for i in 0..jit_chunk_count {
                            if jit_chunks[i].stale != 0 && jit_chunks[i].seg_base == seg_base {
                                c = &mut jit_chunks[i];
                                libc::free((*c).keys as *mut c_void);
                                libc::free((*c).code as *mut c_void);
                                break;
                            }
                        }
                        if c.is_null() {
                            c = &mut jit_chunks[jit_chunk_count];
                            jit_chunk_count += 1;
                        }
                        (*c).seg_base = seg_base;
                        (*c).lo = lo;
                        (*c).hi = hi;
                        (*c).fn_ = cfn;
                        (*c).handle = h;
                        (*c).stale = 0;
                        (*c).keys = ptr::null_mut();
                        (*c).nkeys = 0;
                        jit_seg_index_relink(
                            c.offset_from(ptr::addr_of!(jit_chunks) as *const JitChunk) as usize,
                        );
                        if seg_base + lo < jit_code_lo {
                            jit_code_lo = seg_base + lo;
                        }
                        if seg_base + hi > jit_code_hi {
                            jit_code_hi = seg_base + hi;
                        }
                        mem_page_flags_recompute();
                        // Sidecars (.keys/.code) are named by the CHUNK, not
                        // by the object file: batched speculative compiles
                        // share one .so between many chunks, so derive the
                        // sidecar base from the symbol name.
                        let mut sidecar = [0u8; 1100];
                        let symlen = libc::strlen(sym.as_ptr() as *const c_char);
                        let namelen = symlen.saturating_sub(9); // strip "_dispatch"
                        libc::snprintf(
                            sidecar.as_mut_ptr() as *mut c_char,
                            sidecar.len(),
                            cstr!("%s/%.*s.so"),
                            dir.as_ptr() as *const c_char,
                            namelen as c_int,
                            sym.as_ptr() as *const c_char,
                        );
                        jit_load_keys(c, sidecar.as_ptr() as *const c_char);
                        shim_log_stdout(
                            cstr!("JIT: chunk %04X:[%04X,%04X) (lin %05X) %s keys=%zu\n"),
                            seg as c_uint,
                            lo,
                            hi,
                            (seg_base + off as u32) as c_uint,
                            sym.as_ptr() as *const c_char,
                            (*c).nkeys,
                        );
                        result = c;
                    } else {
                        shim_log_stdout(
                            cstr!("JIT: dlsym %s failed: %s\n"),
                            sym.as_ptr() as *const c_char,
                            libc::dlerror(),
                        );
                        libc::dlclose(h);
                    }
                } else {
                    shim_log_stdout(cstr!("JIT: dlopen failed: %s\n"), libc::dlerror());
                }
            } else {
                shim_log_stdout(
                    cstr!("JIT: compile failed cs:ip=%04X:%04X (rc=%d)\n"),
                    seg as c_uint,
                    off as c_uint,
                    rc,
                );
            }
        }
    }
    if result.is_null() {
        let mut msg = [0u8; 640];
        libc::snprintf(
            msg.as_mut_ptr() as *mut c_char,
            msg.len(),
            cstr!("JIT compile/translate FAILED at cs:ip=%04X:%04X (linear 0x%05X): the live in-memory bytes did not become a runnable chunk (unsupported instruction or data decoded as code -- control reached a non-code address via a wrong transfer). Chunk artifacts in %s. This bundle is self-contained: jit_translate.log has the translator FATAL (mnemonic / file-offset / reached-from function), jit_segment.bin is the exact 64KB this decode saw, and the manifest carries cs:ip -- re-run 'saisei-jitc jit-compile --mem jit_segment.bin --entry 0x%X --image-base 0x%X --outdir .' to reproduce."),
            seg as c_uint,
            off as c_uint,
            (seg_base + off as u32) as c_uint,
            dir.as_ptr() as *const c_char,
            off as c_uint,
            seg_base as c_uint,
        );
        libc::fprintf(
            stderr,
            cstr!("\n[FATAL] %s\n  Halting at the JIT failure (prime directive: a failed data-decode is a hard failure, not a drop).\n\n"),
            msg.as_ptr() as *const c_char,
        );
        let bdir = save_crash_bundle(
            cstr!("jit_compile_failed"),
            seg_base + off as u32,
            msg.as_ptr() as *const c_char,
            libc::strlen(msg.as_ptr() as *const c_char),
        );
        if !bdir.is_null() {
            shim_log_crash(cstr!("Bundle: %s\n"), bdir);
            if jitcaplen != 0 {
                crash_bundle_write_file(
                    bdir,
                    cstr!("jit_translate.log"),
                    jitcap.as_ptr() as *const c_char,
                    jitcaplen,
                );
            }
            crash_bundle_write_file(
                bdir,
                cstr!("jit_segment.bin"),
                virtual_memory.add(seg_base as usize) as *const c_char,
                0x10000,
            );
        }
        shim_flush_all_streams();
        libc::exit(1);
    }
    vclock_resume();

    // Speculative background pre-compile of this segment's other entries: one
    // detached, niced `saisei-jitc speculate` per segbase per run. The dump is
    // re-written to a speculate-private file first (later compiles of this
    // segbase overwrite seg_<base>.bin), and everything the child produces is
    // plain cache content that later foreground jit-compiles resolve — the
    // game only ever blocks on the entry it actually reached.
    static mut speculated_segbases: [u32; 256] = [0; 256];
    static mut speculated_count: usize = 0;
    let mut already = false;
    for i in 0..speculated_count {
        if speculated_segbases[i] == seg_base {
            already = true;
            break;
        }
    }
    if !already && speculated_count < 256 {
        speculated_segbases[speculated_count] = seg_base;
        speculated_count += 1;
        let mut spec_in = [0u8; 1100];
        libc::snprintf(
            spec_in.as_mut_ptr() as *mut c_char,
            spec_in.len(),
            cstr!("%s/spec_in_%05X.bin"),
            dir.as_ptr() as *const c_char,
            seg_base as c_uint,
        );
        let sfp = libc::fopen(spec_in.as_ptr() as *const c_char, cstr!("wb"));
        if !sfp.is_null() {
            libc::fwrite(
                virtual_memory.add(seg_base as usize) as *const c_void,
                1,
                0x10000,
                sfp,
            );
            libc::fclose(sfp);
            let jitc = libc::getenv(cstr!("SAISEI_JITC"));
            libc::snprintf(
                cmd.as_mut_ptr() as *mut c_char,
                cmd.len(),
                cstr!("nice -n 10 '%s' speculate --mem '%s' --image-base 0x%X --outdir '%s' --exclude 0x%X --delete-input >> '%s/speculate.log' 2>&1 &"),
                jitc,
                spec_in.as_ptr() as *const c_char,
                seg_base as c_uint,
                dir.as_ptr() as *const c_char,
                off as c_uint,
                dir.as_ptr() as *const c_char,
            );
            let sp = libc::popen(cmd.as_ptr() as *const c_char, cstr!("r"));
            if !sp.is_null() {
                libc::pclose(sp);
            }
        }
    }
    result
}

// ============================================================================
// Faithful flat machine: run_machine + resolve_and_run_chunk  [C 7048-7177]
// ============================================================================

#[no_mangle]
pub static mut machine_halted: c_int = 0;

unsafe fn resolve_and_run_chunk(addr: u32) -> c_int {
    if try_patch_at(
        addr,
        0,
        cstr!("<run_machine>"),
        cstr!("resolve_and_run_chunk"),
        0,
    ) != 0
    {
        return 1;
    }
    // Fast path: the chunk decoding this address at the live cs (the common
    // case for every far transfer) — one bucket walk, no registry scan.
    let live_base = (cs() as u32) << 4;
    let fast = jit_lookup_at_base(live_base, addr.wrapping_sub(live_base));
    if !fast.is_null() {
        ((*fast).fn_.unwrap())(
            (addr - live_base) as c_int,
            0,
            cstr!("<run_machine>"),
            cstr!("resolve_and_run_chunk"),
            7077,
        );
        return 1;
    }
    let jc = jit_lookup(addr);
    if !jc.is_null() {
        if (*jc).seg_base == ((cs() as u32) << 4) {
            ((*jc).fn_.unwrap())(
                (addr - (*jc).seg_base) as c_int,
                0,
                cstr!("<run_machine>"),
                cstr!("resolve_and_run_chunk"),
                7077,
            );
            return 1;
        }
        let alias_off: u16 = (addr - ((cs() as u32) << 4)) as u16;
        let nc = jit_compile_or_get(cs(), alias_off);
        if !nc.is_null() && (*nc).seg_base == ((cs() as u32) << 4) {
            ((*nc).fn_.unwrap())(
                (addr - (*nc).seg_base) as c_int,
                0,
                cstr!("<run_machine>"),
                cstr!("resolve_and_run_chunk"),
                7092,
            );
            return 1;
        }
    }
    let bfn = try_call_target(addr);
    if bfn.is_some() && is_builtin_call_target(addr) != 0 {
        maybe_safe_point(SHIMS_FILE, cstr!("resolve_and_run_chunk"), 7101);
        (bfn.unwrap())(
            0,
            cstr!("<run_machine>"),
            cstr!("resolve_and_run_chunk"),
            7102,
        );
        return 1;
    }
    let mut fm: *const FileMapping = ptr::null();
    let bd = find_binary_for_addr(addr, &mut fm);
    if !bd.is_null() && !fm.is_null() {
        let file_off = (addr - (*fm).base) + (*fm).file_offset as u32;
        if shim_pc_is_case_key((*bd).module, file_off) == 0 {
            let joff: u16 = (addr - ((cs() as u32) << 4)) as u16;
            let nc = jit_compile_or_get(cs(), joff);
            if !nc.is_null() && (*nc).seg_base == ((cs() as u32) << 4) {
                ((*nc).fn_.unwrap())(
                    (addr - (*nc).seg_base) as c_int,
                    0,
                    cstr!("<run_machine>"),
                    cstr!("resolve_and_run_chunk"),
                    7118,
                );
                return 1;
            }
        }
        set_dispatch_cs(fm, addr);
        ((*bd).fn_.unwrap())(
            file_off as c_int,
            0,
            if !(*fm).path.is_null() {
                (*fm).path
            } else {
                cstr!("<run_machine>")
            },
            cstr!("resolve_and_run_chunk"),
            7124,
        );
        return 1;
    }
    if is_builtin_call_target(addr) == 0 {
        let joff: u16 = (addr - ((cs() as u32) << 4)) as u16;
        let nc = jit_compile_or_get(cs(), joff);
        if !nc.is_null() {
            set_cs(((*nc).seg_base >> 4) as u16);
            ((*nc).fn_.unwrap())(
                (addr - (*nc).seg_base) as c_int,
                0,
                cstr!("<run_machine>"),
                cstr!("resolve_and_run_chunk"),
                7135,
            );
            return 1;
        }
    }
    if bfn.is_some() {
        maybe_safe_point(SHIMS_FILE, cstr!("resolve_and_run_chunk"), 7142);
        (bfn.unwrap())(
            0,
            cstr!("<run_machine>"),
            cstr!("resolve_and_run_chunk"),
            7143,
        );
        return 1;
    }
    0
}

#[no_mangle]
pub unsafe extern "C" fn run_machine() {
    while machine_halted == 0 {
        let addr = ((cs() as u32) << 4) + ip() as u32;
        // Budget-gated like the dispatcher hops: far-return unwinds land here
        // once per transfer, and the transfer debits guarantee the budget
        // still drains to a real poll at bounded intervals.
        maybe_safe_point(SHIMS_FILE, cstr!("run_machine"), 7156);
        if machine_halted != 0 {
            break;
        }
        if resolve_and_run_chunk(addr) != 0 {
            if EXEC_CHILD_EXIT_PENDING {
                EXEC_CHILD_EXIT_PENDING = false;
                return;
            }
            continue;
        }
        let mut msg = [0u8; 1024];
        let n = libc::snprintf(
            msg.as_mut_ptr() as *mut c_char,
            msg.len(),
            cstr!("[BUG] run_machine: unmapped cs:ip=%04X:%04X (linear 0x%05X)\n  ss:sp=%04X:%04X active_binary=%s\n  ax=%04X bx=%04X cx=%04X dx=%04X si=%04X di=%04X bp=%04X ds=%04X es=%04X\n  diagnosis: control reached a linear address with no JIT chunk, no\n  file_mapping, and no static call_target. Almost always the emulated\n  8086 stack popped a bogus value as cs:ip (an upstream push/pop\n  imbalance), or a far/near transfer computed a wrong target. Search\n  the trace tail for the last push/pop/ret before this address.\n"),
            cs() as c_uint, ip() as c_uint, addr as c_uint, ss() as c_uint, sp() as c_uint,
            if shim_active_binary().is_null() { cstr!("<none>") } else { shim_active_binary() },
            ax() as c_uint, bx() as c_uint, cx() as c_uint, dx() as c_uint, si() as c_uint, di() as c_uint, bp() as c_uint, ds() as c_uint, es() as c_uint,
        );
        shim_log_crash(cstr!("%s"), msg.as_ptr() as *const c_char);
        if n > 0 {
            save_bug_bundle(
                cstr!("run_machine_unmapped"),
                addr,
                msg.as_ptr() as *const c_char,
            );
        }
        shim_flush_all_streams();
        libc::exit(1);
    }
}

// ============================================================================
// Synchronous EXEC (re-architected: no setjmp/longjmp)  [C 7187-7262]
// ============================================================================

const MAX_EXEC_NEST: usize = 8;
static mut exec_nest_depth: c_int = 0;
static mut exec_nest_status: [c_int; MAX_EXEC_NEST + 1] = [0; MAX_EXEC_NEST + 1];
/// Set by shim_exec_child_terminate when a nested EXEC'd child exits; run_machine
/// observes it, clears it, and returns to shim_exec_run_child (replaces longjmp).
static mut EXEC_CHILD_EXIT_PENDING: bool = false;

#[no_mangle]
pub unsafe extern "C" fn shim_exec_run_child(
    child_cs: u16,
    child_ip: u16,
    child_ss: u16,
    child_sp: u16,
    child_psp: u16,
) -> c_int {
    if exec_nest_depth >= MAX_EXEC_NEST as c_int {
        return -1;
    }
    let p_cs = cs();
    let p_ip = ip();
    let p_ss = ss();
    let p_sp = sp();
    let p_ds = ds();
    let p_es = es();
    let p_ax = ax();
    let p_bx = bx();
    let p_cx = cx();
    let p_dx = dx();
    let p_si = si();
    let p_di = di();
    let p_bp = bp();
    let p_crit = critical_depth;
    let p_if = IF();
    let mut p_owners: [*const c_char; CRITICAL_MAX_DEPTH] = [ptr::null(); CRITICAL_MAX_DEPTH];
    let mut i = 0;
    while i < p_crit as usize && i < CRITICAL_MAX_DEPTH {
        p_owners[i] = critical_owner_name_stk[i];
        i += 1;
    }
    exec_nest_depth += 1;
    let depth = exec_nest_depth;
    exec_nest_status[depth as usize] = 0;

    set_cs(child_cs);
    set_ip(child_ip);
    set_ss(child_ss);
    set_sp(child_sp);
    set_ds(child_psp);
    set_es(child_psp);
    critical_depth = 0;
    set_IF(1);
    run_machine(); // returns when the child exits (EXEC_CHILD_EXIT_PENDING) or machine_halted

    let status = exec_nest_status[depth as usize];
    exec_nest_depth -= 1;
    critical_depth = p_crit;
    set_IF(p_if);
    let mut i = 0;
    while i < p_crit as usize && i < CRITICAL_MAX_DEPTH {
        critical_owner_name_stk[i] = p_owners[i];
        i += 1;
    }
    critical_owner_name = if p_crit > 0 {
        critical_owner_name_stk[p_crit as usize - 1]
    } else {
        ptr::null()
    };
    set_cs(p_cs);
    set_ip(p_ip);
    set_ss(p_ss);
    set_sp(p_sp);
    set_ds(p_ds);
    set_es(p_es);
    set_ax(p_ax);
    set_bx(p_bx);
    set_cx(p_cx);
    set_dx(p_dx);
    set_si(p_si);
    set_di(p_di);
    set_bp(p_bp);
    status
}

#[no_mangle]
pub unsafe extern "C" fn shim_exec_child_terminate(status: c_int) -> c_int {
    if exec_nest_depth > 0 {
        exec_nest_status[exec_nest_depth as usize] = status;
        // Re-architected: instead of longjmp, flag the pending child exit so
        // run_machine unwinds back to shim_exec_run_child. Return nonzero so
        // dos_exit_impl returns (does not exit the process).
        EXEC_CHILD_EXIT_PENDING = true;
        return 1;
    }
    0
}

// ============================================================================
// Function-patch registry  [C lines 7278-7464]
// ============================================================================

const NO_ACTIVE_PATCH: u32 = 0xFFFFFFFF;
const MAX_PATCHES: usize = 2048;
impl GamePatch {
    const ZERO: GamePatch = GamePatch {
        file: ptr::null(),
        file_off: 0,
        fn_: None,
        name: ptr::null(),
        enabled: 0,
    };
}
static mut patch_reg: [GamePatch; MAX_PATCHES] = [GamePatch::ZERO; MAX_PATCHES];
static mut patch_reg_lin: [u32; MAX_PATCHES] = [0; MAX_PATCHES];
static mut patch_reg_count: usize = 0;
static mut patch_reg_inited: c_int = 0;
static mut patch_reg_lin_ready: c_int = 0;
/// file_mapping_count at the last linear-resolution attempt: unresolved
/// patches are only re-resolved when the mappings actually changed, not on
/// every transfer through the dispatcher.
static mut patch_reg_lin_stamp: usize = usize::MAX;
/// Resolved-patch early-out state for the dispatch hot path: min/max linear
/// bounds plus a 64-bit bloom over (lin >> 4). Rebuilt with patch_reg_lin.
static mut patch_lin_min: u32 = 0xFFFFFFFF;
static mut patch_lin_max: u32 = 0;
static mut patch_lin_bloom: u64 = 0;
static mut patch_active_addr: u32 = NO_ACTIVE_PATCH;
static mut patch_current_addr: u32 = 0;
static mut patch_current_retip: u16 = 0;
static mut patch_current_file: *const c_char = ptr::null();
static mut patch_current_func: *const c_char = ptr::null();
static mut patch_current_line: c_int = 0;

unsafe fn patch_path_basename(p: *const c_char) -> *const c_char {
    let mut s = p;
    let mut c = p;
    while *c != 0 {
        if *c as u8 == b'/' || *c as u8 == b'\\' {
            s = c.add(1);
        }
        c = c.add(1);
    }
    s
}

unsafe fn patch_resolve_linear(binary: *const c_char, file_off: u32) -> u32 {
    let mut j = file_mapping_count as isize - 1;
    while j >= 0 {
        let pth = file_mappings[j as usize].path;
        if pth.is_null() {
            j -= 1;
            continue;
        }
        if !binary.is_null() && libc::strcmp(patch_path_basename(pth), binary) != 0 {
            j -= 1;
            continue;
        }
        let fo = file_mappings[j as usize].file_offset as u32;
        if file_off >= fo && (file_off as usize) < fo as usize + file_mappings[j as usize].len {
            return file_mappings[j as usize].base + (file_off - fo);
        }
        j -= 1;
    }
    0
}

#[no_mangle]
pub unsafe extern "C" fn patch_register(arr: *const GamePatch, n: usize) {
    let mut i = 0;
    while i < n && patch_reg_count < MAX_PATCHES {
        patch_reg[patch_reg_count] = *arr.add(i);
        patch_reg_count += 1;
        i += 1;
    }
    patch_reg_lin_ready = 0;
    patch_reg_lin_stamp = usize::MAX;
    shim_log_stdout(
        cstr!("patch: registered %zu patch(es), %zu total\n"),
        n,
        patch_reg_count,
    );
}

unsafe fn ensure_patch_reg() {
    if patch_reg_inited != 0 {
        return;
    }
    patch_reg_inited = 1;
    if !cfg().patches.is_null() && cfg().patch_count != 0 {
        patch_register(cfg().patches, cfg().patch_count);
    }
}

#[no_mangle]
pub unsafe extern "C" fn patch_load_bundle(so_path: *const c_char) {
    ensure_patch_reg();
    let h = libc::dlopen(so_path, libc::RTLD_NOW | libc::RTLD_GLOBAL);
    if h.is_null() {
        shim_log_stderr(
            cstr!("patch: dlopen %s failed: %s\n"),
            so_path,
            libc::dlerror(),
        );
        return;
    }
    let arr = libc::dlsym(h, cstr!("bundle_patches")) as *const GamePatch;
    let cnt = libc::dlsym(h, cstr!("bundle_patch_count")) as *const usize;
    if arr.is_null() || cnt.is_null() {
        shim_log_stderr(
            cstr!("patch: bundle %s lacks bundle_patches/bundle_patch_count\n"),
            so_path,
        );
        return;
    }
    shim_log_stdout(cstr!("patch: bundle %s -> %zu patch(es)\n"), so_path, *cnt);
    patch_register(arr, *cnt);
}

unsafe fn ensure_patch_lin() {
    ensure_patch_reg();
    if patch_reg_lin_ready != 0 {
        return;
    }
    // Unresolved entries can only become resolvable when a new file mapping
    // appears; skip the per-transfer re-resolution walk until then.
    if patch_reg_lin_stamp == file_mapping_count {
        return;
    }
    patch_reg_lin_stamp = file_mapping_count;
    let mut all = 1;
    for i in 0..patch_reg_count {
        if patch_reg_lin[i] != 0 {
            continue;
        }
        let lin = patch_resolve_linear(patch_reg[i].file, patch_reg[i].file_off);
        if lin != 0 {
            patch_reg_lin[i] = lin;
        } else {
            all = 0;
        }
    }
    patch_reg_lin_ready = all;
    patch_lin_min = 0xFFFFFFFF;
    patch_lin_max = 0;
    patch_lin_bloom = 0;
    for i in 0..patch_reg_count {
        let lin = patch_reg_lin[i];
        if lin == 0 {
            continue;
        }
        if lin < patch_lin_min {
            patch_lin_min = lin;
        }
        if lin > patch_lin_max {
            patch_lin_max = lin;
        }
        patch_lin_bloom |= 1u64 << ((lin >> 4) & 63);
    }
}

unsafe fn try_patch_at(
    addr: u32,
    expected_retip: u16,
    file: *const c_char,
    func: *const c_char,
    line: c_int,
) -> c_int {
    ensure_patch_reg();
    if patch_reg_count == 0 {
        return 0;
    }
    if addr == patch_active_addr {
        return 0;
    }
    ensure_patch_lin();
    // Hot-path early-out: almost no transfer targets a patched function.
    // (addr == 0 keeps the full scan: an unresolved entry's lin is 0 and the
    // legacy loop would compare equal — preserve that behavior exactly.)
    if addr != 0
        && (addr < patch_lin_min
            || addr > patch_lin_max
            || patch_lin_bloom & (1u64 << ((addr >> 4) & 63)) == 0)
    {
        return 0;
    }
    for i in 0..patch_reg_count {
        let p = &patch_reg[i];
        if p.enabled == 0 || p.fn_.is_none() || patch_reg_lin[i] != addr {
            continue;
        }
        let s_addr = patch_current_addr;
        let s_retip = patch_current_retip;
        let s_file = patch_current_file;
        let s_func = patch_current_func;
        let s_line = patch_current_line;
        let s_active = patch_active_addr;
        patch_active_addr = addr;
        patch_current_addr = addr;
        patch_current_retip = expected_retip;
        patch_current_file = file;
        patch_current_func = func;
        patch_current_line = line;
        let r = (p.fn_.unwrap())(expected_retip, file, func, line);
        patch_active_addr = s_active;
        patch_current_addr = s_addr;
        patch_current_retip = s_retip;
        patch_current_file = s_file;
        patch_current_func = s_func;
        patch_current_line = s_line;
        if r == PATCH_HANDLED {
            shim_drain_pending_tail_dispatch(file, func, line);
            return 1;
        }
        return 0;
    }
    0
}

#[no_mangle]
pub unsafe extern "C" fn shim_patch_check(linear: u32, expected_retip: u16) -> c_int {
    try_patch_at(
        linear,
        expected_retip,
        cstr!("<chunk>"),
        cstr!("shim_patch_check"),
        0,
    )
}

#[no_mangle]
pub unsafe extern "C" fn patch_call_original() {
    dispatch_via_binary(
        patch_current_addr,
        patch_current_retip,
        if !patch_current_file.is_null() {
            patch_current_file
        } else {
            cstr!("patch")
        },
        if !patch_current_func.is_null() {
            patch_current_func
        } else {
            cstr!("patch")
        },
        patch_current_line,
    );
}

#[no_mangle]
pub unsafe extern "C" fn patch_self_offset(binary_out: *mut *const c_char) -> u32 {
    let fm = find_file_mapping(patch_current_addr);
    if !binary_out.is_null() {
        *binary_out = if !fm.is_null() && !(*fm).path.is_null() {
            patch_path_basename((*fm).path)
        } else {
            cstr!("?")
        };
    }
    if fm.is_null() {
        return patch_current_addr;
    }
    (patch_current_addr - (*fm).base) + (*fm).file_offset as u32
}

#[no_mangle]
pub unsafe extern "C" fn shim_resolve_addr(linear: u32, binary_out: *mut *const c_char) -> u32 {
    let fm = find_file_mapping(linear);
    if !binary_out.is_null() {
        *binary_out = if !fm.is_null() && !(*fm).path.is_null() {
            patch_path_basename((*fm).path)
        } else {
            cstr!("?")
        };
    }
    if fm.is_null() {
        return 0xFFFFFFFF;
    }
    (linear - (*fm).base) + (*fm).file_offset as u32
}

#[no_mangle]
pub unsafe extern "C" fn patch_call_function(binary: *const c_char, file_off: u32) {
    let lin = patch_resolve_linear(binary, file_off);
    if lin == 0 {
        shim_log_stderr(
            cstr!("patch_call_function: %s+0x%X not mapped\n"),
            if !binary.is_null() {
                binary
            } else {
                cstr!("?")
            },
            file_off as c_uint,
        );
        return;
    }
    dispatch_via_binary(lin, 0, cstr!("patch"), cstr!("patch_call_function"), 0);
}

#[no_mangle]
pub unsafe extern "C" fn patch_ret_near(expected_retip: u16) {
    let popped = memw_read_impl(ss(), sp(), SHIMS_FILE, cstr!("patch_ret_near"), 7461);
    set_sp(((sp() as u32 + 2) & 0xFFFF) as u16);
    near_ret_tail_impl(
        popped,
        expected_retip,
        cstr!("patch"),
        cstr!("patch_ret_near"),
        0,
    );
}

// ============================================================================
// dispatch_via_binary + overlay dispatch + jump/near-ret  [C 7466-7709]
// ============================================================================

unsafe fn dispatch_via_binary(
    mut addr: u32,
    mut expected_retip: u16,
    file: *const c_char,
    func: *const c_char,
    line: c_int,
) -> c_int {
    // A cross-chunk far transfer stands in for a real far-call + retf pair
    // (~52+32 cycles on a 386 ≈ 25 model instructions): charge the instruction
    // budget accordingly, or virtual time under-runs real time for code that
    // far-calls a driver per sprite/char (Zeliard's attract loop measured
    // 0.36× real time with transfers charged as a single instruction).
    jit_instr_budget -= 25;
    tail_dispatch_pending = false;
    if try_patch_at(addr, expected_retip, file, func, line) != 0 {
        return 1;
    }
    // Fast path: the chunk decoding this address at the live cs (the common
    // case for every far transfer) — one bucket walk, no registry scan.
    let live_base = (cs() as u32) << 4;
    let fast = jit_lookup_at_base(live_base, addr.wrapping_sub(live_base));
    if !fast.is_null() {
        return jit_dispatch(fast, addr, expected_retip, file, func, line);
    }
    let jc = jit_lookup(addr);
    if !jc.is_null() {
        let mut jc = jc;
        let alias_off = addr.wrapping_sub(live_base);
        if alias_off < 0x10000 && (*jc).seg_base != live_base {
            let nc = jit_compile_or_get(cs(), alias_off as u16);
            if !nc.is_null() && (*nc).seg_base == live_base {
                jc = nc;
            }
        }
        return jit_dispatch(jc, addr, expected_retip, file, func, line);
    }
    let mut fm: *const FileMapping = ptr::null();
    let mut bd = find_binary_for_addr(addr, &mut fm);
    if bd.is_null() {
        if is_builtin_call_target(addr) != 0 {
            return 0;
        }
        let joff: u16 = (addr - ((cs() as u32) << 4)) as u16;
        let nc = jit_compile_or_get(cs(), joff);
        if !nc.is_null() {
            return jit_dispatch(nc, addr, expected_retip, file, func, line);
        }
        return 0;
    }
    let entry_module = (*bd).module;
    let entry_off = (addr - (*fm).base) + (*fm).file_offset as u32;
    let entry_was_case_key = shim_pc_is_case_key(entry_module, entry_off);
    let entry_cs = cs();
    let entry_ip = ip();
    if entry_was_case_key == 0 {
        let joff: u16 = (addr - ((cs() as u32) << 4)) as u16;
        let nc = jit_compile_or_get(cs(), joff);
        if !nc.is_null() {
            return jit_dispatch(nc, addr, expected_retip, file, func, line);
        }
    }
    let mut unmapped_tail = 0;
    let saved_cs = cs();
    dispatch_depth += 1;
    dd_inc_via_binary += 1;
    loop {
        set_dispatch_cs(fm, addr);
        let file_off = (addr - (*fm).base) + (*fm).file_offset as u32;
        maybe_safe_point(SHIMS_FILE, cstr!("dispatch_via_binary"), 7562);
        ((*bd).fn_.unwrap())(file_off as c_int, expected_retip, file, func, line);
        maybe_safe_point(SHIMS_FILE, cstr!("dispatch_via_binary"), 7564);
        if !tail_dispatch_pending {
            break;
        }
        if tail_dispatch_addr == addr && shim_pc_is_case_key((*bd).module, file_off) == 0 {
            let exp = tail_dispatch_expected;
            tail_dispatch_pending = false;
            let jc = jit_compile_or_get(cs(), ip());
            dispatch_depth -= 1;
            dd_dec_via_binary += 1;
            set_cs(saved_cs);
            if !jc.is_null() {
                return jit_dispatch(jc, addr, exp, file, func, line);
            }
            return 1;
        }
        addr = tail_dispatch_addr;
        expected_retip = tail_dispatch_expected;
        tail_dispatch_pending = false;
        fm = ptr::null();
        bd = find_binary_for_addr(addr, &mut fm);
        if bd.is_null() {
            unmapped_tail = 1;
        } else if shim_pc_is_case_key((*bd).module, (addr - (*fm).base) + (*fm).file_offset as u32)
            == 0
        {
            let nc = jit_compile_or_get(cs(), ip());
            if !nc.is_null() {
                dispatch_depth -= 1;
                dd_dec_via_binary += 1;
                set_cs(saved_cs);
                return jit_dispatch(nc, addr, expected_retip, file, func, line);
            }
        }
        if bd.is_null() {
            break;
        }
    }
    dispatch_depth -= 1;
    dd_dec_via_binary += 1;
    set_cs(saved_cs);

    if unmapped_tail != 0 && entry_was_case_key == 0 {
        set_cs(entry_cs);
        set_ip(entry_ip);
        let entry_lin = ((entry_cs as u32) << 4) + entry_ip as u32;
        let nc = jit_compile_or_get(entry_cs, entry_ip);
        if !nc.is_null() {
            return jit_dispatch(nc, entry_lin, expected_retip, file, func, line);
        }
        let mut buf = [0u8; 2048];
        shim_unhandled_pc_report(
            entry_module,
            entry_off as c_int,
            buf.as_mut_ptr() as *mut c_char,
            buf.len(),
        );
        shim_log_crash(cstr!("%s"), buf.as_ptr() as *const c_char);
        save_bug_bundle(
            cstr!("unhandled_pc"),
            entry_lin,
            buf.as_ptr() as *const c_char,
        );
        shim_flush_all_streams();
        libc::exit(1);
    }
    1
}

unsafe fn try_dispatch_overlay_first(
    addr: u32,
    expected_retip: u16,
    file: *const c_char,
    func: *const c_char,
    line: c_int,
) -> c_int {
    if dispatch_via_binary(addr, expected_retip, file, func, line) != 0 {
        return 1;
    }
    let fn_ = try_call_target(addr);
    if fn_.is_none() {
        return 0;
    }
    let fm = find_file_mapping(addr);
    let saved_cs = cs();
    set_dispatch_cs(fm, addr);
    dispatch_depth += 1;
    dd_inc_overlay_first += 1;
    maybe_safe_point(SHIMS_FILE, cstr!("try_dispatch_overlay_first"), 7668);
    (fn_.unwrap())(expected_retip, file, func, line);
    maybe_safe_point(SHIMS_FILE, cstr!("try_dispatch_overlay_first"), 7670);
    dispatch_depth -= 1;
    dd_dec_overlay_first += 1;
    set_cs(saved_cs);
    1
}

#[no_mangle]
pub unsafe extern "C" fn jump_table_impl(
    addr: u32,
    _expected_retip: u16,
    file: *const c_char,
    func: *const c_char,
    line: c_int,
) {
    shim_log_stdout(
        cstr!("Trace: jump_table 0x%08X (%s:%s:%d)\n"),
        addr as c_uint,
        file,
        func,
        line,
    );
    lifecycle_log_dispatch(cstr!("JMP"), addr);
    set_ip((addr.wrapping_sub((cs() as u32) << 4)) as u16);
}

#[no_mangle]
pub unsafe extern "C" fn near_ret_tail_impl(
    popped_ip: u16,
    _expected_retip: u16,
    file: *const c_char,
    func: *const c_char,
    line: c_int,
) {
    jit_instr_budget -= 3; // near ret ≈ 10 cycles on a 386
    let addr = ((cs() as u32) << 4) + popped_ip as u32;
    shim_log_stdout(
        cstr!("Trace: near_ret_tail to %04X:%04X (0x%08X) (%s:%s:%d)\n"),
        cs() as c_uint,
        popped_ip as c_uint,
        addr as c_uint,
        file,
        func,
        line,
    );
    if isr_depth == 0 {
        let fm = find_file_mapping(addr);
        let bn: *const c_char = if !fm.is_null() && !(*fm).path.is_null() {
            let s = libc::strrchr((*fm).path, b'/' as c_int);
            if !s.is_null() {
                s.add(1)
            } else {
                (*fm).path
            }
        } else {
            cstr!("<unmapped>")
        };
        let off_in: usize = if !fm.is_null() && !(*fm).path.is_null() {
            (*fm).file_offset + (addr - (*fm).base) as usize
        } else {
            0
        };
        if lifecycle_eager() {
            lifecycle_log(
                cstr!("NRET 0x%05X popped=%04X -> %s+0x%zX\n"),
                addr as c_uint,
                popped_ip as c_uint,
                bn,
                off_in,
            );
        } else {
            let mut rec = LifecycleDispatchRec {
                t_us: lifecycle_elapsed_us(),
                kind: ptr::null(),
                addr,
                popped: popped_ip,
                has_path: 0,
                _pad: 0,
                off_in: off_in as u64,
                bn: [0; 20],
                regs: regsnap_now(),
            };
            libc::snprintf(
                rec.bn.as_mut_ptr() as *mut c_char,
                rec.bn.len(),
                cstr!("%s"),
                bn,
            );
            lifecycle_ring_save_rec(&rec, LC_NRET);
        }
    }
    set_ip(popped_ip);
}

// ============================================================================
// Memory dumps + RAM snapshot + screenshot  [C 7711-7822]
// ============================================================================

#[no_mangle]
pub unsafe extern "C" fn shim_dump_memory(offset: u32, mut length: usize) {
    if offset as usize >= MEMORY_SIZE {
        return;
    }
    if offset as usize + length > MEMORY_SIZE {
        length = MEMORY_SIZE - offset as usize;
    }
    let mut pos: usize = 0;
    while pos < length {
        let line_len = if length - pos > 16 { 16 } else { length - pos };
        shim_log_stdout(cstr!("%06X:"), (offset as usize + pos) as c_uint);
        for i in 0..16 {
            if i < line_len {
                shim_log_stdout(
                    cstr!(" %02X"),
                    *virtual_memory.add(offset as usize + pos + i) as c_uint,
                );
            } else {
                shim_log_stdout(cstr!("   "));
            }
        }
        shim_log_stdout(cstr!("  |"));
        for i in 0..line_len {
            let b = *virtual_memory.add(offset as usize + pos + i);
            shim_log_stdout(
                cstr!("%c"),
                (if b >= 32 && b <= 126 { b } else { b'.' }) as c_uint,
            );
        }
        shim_log_stdout(cstr!("|\n"));
        pos += 16;
    }
}

#[no_mangle]
pub unsafe extern "C" fn shim_dump_whole_memory() {
    shim_dump_memory(0, MEMORY_SIZE);
}

static mut ram_snapshot_counter: c_int = 1;

unsafe fn shim_dump_ram_snapshot() {
    let dir = cstr!("snapshots");
    if libc::mkdir(dir, 0o755) != 0 && *libc::__errno_location() != libc::EEXIST {
        libc::perror(cstr!("mkdir snapshots"));
        return;
    }
    let mut path = [0u8; 128];
    libc::snprintf(
        path.as_mut_ptr() as *mut c_char,
        path.len(),
        cstr!("%s/snap_%d.bin"),
        dir,
        ram_snapshot_counter,
    );
    let fd = libc::open(
        path.as_ptr() as *const c_char,
        libc::O_WRONLY | libc::O_CREAT | libc::O_TRUNC,
        0o644,
    );
    if fd < 0 {
        libc::perror(path.as_ptr() as *const c_char);
        return;
    }
    let mut off: usize = 0;
    while off < SHIM_MEMORY_SIZE {
        let w = libc::write(
            fd,
            virtual_memory.add(off) as *const c_void,
            SHIM_MEMORY_SIZE - off,
        );
        if w <= 0 {
            break;
        }
        off += w as usize;
    }
    libc::close(fd);
    shim_log_stdout(
        cstr!("[SNAP] ram → %s (%zu bytes, counter=%d)\n"),
        path.as_ptr() as *const c_char,
        off,
        ram_snapshot_counter,
    );
    ram_snapshot_counter += 1;
}

unsafe fn shim_read_memory_to_sidecar(addr: u32, len: u8) {
    if addr as usize + len as usize > SHIM_MEMORY_SIZE {
        shim_log_stdout(
            cstr!("[READ] out of bounds addr=0x%X len=%u\n"),
            addr as c_uint,
            len as c_uint,
        );
        return;
    }
    let dir = cstr!("snapshots");
    if libc::mkdir(dir, 0o755) != 0 && *libc::__errno_location() != libc::EEXIST {
        libc::perror(cstr!("mkdir snapshots"));
        return;
    }
    let mut path = [0u8; 128];
    libc::snprintf(
        path.as_mut_ptr() as *mut c_char,
        path.len(),
        cstr!("%s/last_read.bin"),
        dir,
    );
    let fd = libc::open(
        path.as_ptr() as *const c_char,
        libc::O_WRONLY | libc::O_CREAT | libc::O_TRUNC,
        0o644,
    );
    if fd < 0 {
        libc::perror(path.as_ptr() as *const c_char);
        return;
    }
    let w = libc::write(
        fd,
        virtual_memory.add(addr as usize) as *const c_void,
        len as usize,
    );
    libc::close(fd);
    shim_log_stdout(
        cstr!("[READ] addr=0x%X len=%u wrote=%zd → %s\n"),
        addr as c_uint,
        len as c_uint,
        w,
        path.as_ptr() as *const c_char,
    );
}

#[no_mangle]
pub unsafe extern "C" fn shim_save_video_memory() {
    let dir = cstr!("screenshots");
    if libc::mkdir(dir, 0o755) != 0 && *libc::__errno_location() != libc::EEXIST {
        libc::perror(cstr!("mkdir"));
        return;
    }
    let mut path = [0u8; 128];
    let sc = screenshot_counter;
    screenshot_counter += 1;
    libc::snprintf(
        path.as_mut_ptr() as *mut c_char,
        path.len(),
        cstr!("%s/screenshot%d.png"),
        dir,
        sc,
    );
    shim_render_screenshot_png(path.as_ptr() as *const c_char);

    let crc = stbiw__crc32(
        ptr::addr_of!(vga.palette) as *const u8,
        (*ptr::addr_of!(vga.palette)).len() as c_int,
    );
    libc::snprintf(
        path.as_mut_ptr() as *mut c_char,
        path.len(),
        cstr!("%s/pallet_%08x.png"),
        dir,
        crc,
    );

    let mut palette_img = [0u8; 16 * 16 * 3];
    for i in 0..256 {
        let mut r = vga.palette[i * 3];
        let mut g = vga.palette[i * 3 + 1];
        let mut b = vga.palette[i * 3 + 2];
        r = (r << 2) | (r >> 4);
        g = (g << 2) | (g >> 4);
        b = (b << 2) | (b >> 4);
        palette_img[i * 3] = r;
        palette_img[i * 3 + 1] = g;
        palette_img[i * 3 + 2] = b;
    }
    stbi_write_png(
        path.as_ptr() as *const c_char,
        16,
        16,
        3,
        palette_img.as_ptr() as *const c_void,
        16 * 3,
    );
}

// ---- location-free wrappers ----
#[no_mangle]
pub unsafe extern "C" fn safe_point() {
    safe_point_impl(cstr!("<external>"), cstr!("safe_point"), 0);
}
#[no_mangle]
pub unsafe extern "C" fn long_jump(seg: u16, off: u16) {
    long_jump_impl(seg, off, cstr!("<external>"), cstr!("long_jump"), 0);
}
#[no_mangle]
pub unsafe extern "C" fn lcall_table(ret_ip: u16, seg: u16, off: u16) {
    lcall_table_impl(
        ret_ip,
        seg,
        off,
        cstr!("<external>"),
        cstr!("lcall_table"),
        0,
    );
}

// ============================================================================
// Remaining wrappers + drain + main + runtime state capture  [C 7834-8172]
// ============================================================================

#[no_mangle]
pub unsafe extern "C" fn call_table(ret_ip: u16, addr: u32) {
    // Test seam: the C setjmp'd shim_fatal_env here so an armed unmapped target
    // unwound back. report_unmapped is re-architected to set the capture flags
    // and return (not longjmp), so control unwinds through call_table_impl
    // normally — no setjmp needed.
    call_table_impl(ret_ip, addr, cstr!("<external>"), cstr!("call_table"), 0);
}

#[no_mangle]
pub unsafe extern "C" fn jump_table(addr: u32, expected_retip: u16) {
    jump_table_impl(
        addr,
        expected_retip,
        cstr!("<external>"),
        cstr!("jump_table"),
        0,
    );
}

#[no_mangle]
pub unsafe extern "C" fn shim_dispatch_via_binary(
    addr: u32,
    expected_retip: u16,
    file: *const c_char,
    func: *const c_char,
    line: c_int,
) -> c_int {
    dispatch_via_binary(addr, expected_retip, file, func, line)
}

#[no_mangle]
pub unsafe extern "C" fn shim_tail_dispatch_save(out: *mut ShimTailDispatchState) {
    (*out).pending = tail_dispatch_pending;
    (*out).addr = tail_dispatch_addr;
    (*out).expected = tail_dispatch_expected;
    tail_dispatch_pending = false;
}

#[no_mangle]
pub unsafe extern "C" fn shim_tail_dispatch_restore(in_: *const ShimTailDispatchState) {
    tail_dispatch_pending = (*in_).pending;
    tail_dispatch_addr = (*in_).addr;
    tail_dispatch_expected = (*in_).expected;
}

#[no_mangle]
pub unsafe extern "C" fn shim_drain_pending_tail_dispatch(
    file: *const c_char,
    func: *const c_char,
    line: c_int,
) {
    while tail_dispatch_pending {
        let addr = tail_dispatch_addr;
        let expected = tail_dispatch_expected;
        if dispatch_via_binary(addr, expected, file, func, line) != 0 {
            continue;
        }
        let fn_ = try_call_target(addr);
        if fn_.is_some() {
            tail_dispatch_pending = false;
            dispatch_depth += 1;
            dd_inc_overlay_first += 1;
            (fn_.unwrap())(expected, file, func, line);
            dispatch_depth -= 1;
            dd_dec_overlay_first += 1;
            continue;
        }
        // DEAD "contained lcall fault recovery" band-aid: lcall_return_env is
        // never setjmp'd, so the C longjmp here can never validly execute.
        // Per the port plan, keep the same crash log and abort() instead.
        if lcall_depth > 0 {
            shim_log_crash(
                cstr!("[WARN] contained lcall fault: tail dead-end 0x%X at depth %d -- returning from lcall to %04X:%04X (callee likely mis-reconstructed)\n"),
                addr as c_uint,
                lcall_depth as c_int,
                lcall_ret_cs[lcall_depth as usize] as c_uint,
                lcall_ret_ip[lcall_depth as usize] as c_uint,
            );
            shim_flush_all_streams();
            libc::abort();
        }
        let mut msg = [0u8; 1024];
        let n = libc::snprintf(
            msg.as_mut_ptr() as *mut c_char,
            msg.len(),
            cstr!("[BUG] tail dispatch to unmapped target 0x%X (expected_retip=%04X)\n  caller: %s:%s:%d\n  cs:ip=%04X:%04X  ss:sp=%04X:%04X  active_binary=%s\n  ax=%04X bx=%04X cx=%04X dx=%04X si=%04X di=%04X bp=%04X\n  diagnosis: a near_ret_tail or cross-binary dispatch landed at a\n  linear address with NO file_mapping AND no static call_target.\n  Almost always the simulated 8086 stack popped a segment value\n  as IP (stack imbalance from a translator/shim bug), or the\n  saved snapshot was captured in an inconsistent state and the\n  first ret after restore consumed garbage. Search the trace\n  tail for the last push/pop pair before this dispatch.\n"),
            addr as c_uint,
            expected as c_uint,
            if file.is_null() { cstr!("?") } else { file },
            if func.is_null() { cstr!("?") } else { func },
            line,
            cs() as c_uint, ip() as c_uint, ss() as c_uint, sp() as c_uint,
            if shim_active_binary().is_null() { cstr!("<none>") } else { shim_active_binary() },
            ax() as c_uint, bx() as c_uint, cx() as c_uint, dx() as c_uint, si() as c_uint, di() as c_uint, bp() as c_uint,
        );
        shim_log_crash(cstr!("%s"), msg.as_ptr() as *const c_char);
        if n > 0 {
            save_bug_bundle(
                cstr!("tail_dispatch_unmapped"),
                addr,
                msg.as_ptr() as *const c_char,
            );
        }
        shim_flush_all_streams();
        libc::abort();
    }
}

extern "C" fn virtual_display_shutdown_atexit() {
    unsafe { virtual_display_shutdown() };
}

/// The runtime entry point. Named `saisei_main` (not `main`) so the crate can
/// be an ordinary rlib dependency of the `saisei-game` bin crate, whose tiny
/// Rust `main` rebuilds C argv and calls this.
#[no_mangle]
pub unsafe extern "C" fn saisei_main(argc: c_int, argv: *mut *mut c_char) -> c_int {
    #[cfg(feature = "force_exit_after_10s")]
    setup_force_exit();
    let mut restore_from: *const c_char = ptr::null();
    let mut i: c_int = 1;
    while i < argc {
        let arg = *argv.offset(i as isize) as *const c_char;
        if libc::strcmp(arg, cstr!("--headless")) == 0 {
            headless_mode = 1;
            i += 1;
            continue;
        }
        if libc::strncmp(arg, cstr!("--restore-from="), 15) == 0 {
            restore_from = arg.add(15);
            i += 1;
            continue;
        }
        if libc::strcmp(arg, cstr!("--restore-from")) == 0 {
            if i + 1 >= argc {
                libc::fprintf(stderr, cstr!("Missing value after --restore-from\n"));
                return 2;
            }
            i += 1;
            restore_from = *argv.offset(i as isize) as *const c_char;
            i += 1;
            continue;
        }
        if libc::strncmp(arg, cstr!("--speedup="), 10) == 0 {
            let mut end: *mut c_char = ptr::null_mut();
            let parsed = libc::strtod(arg.add(10), &mut end);
            if !end.is_null() && *end == 0 && parsed > 0.0 {
                emulation_speedup = parsed;
            } else {
                libc::fprintf(
                    stderr,
                    cstr!("Invalid --speedup value '%s' (expected positive number)\n"),
                    arg.add(10),
                );
                return 2;
            }
            i += 1;
            continue;
        }
        if libc::strcmp(arg, cstr!("--speedup")) == 0 {
            if i + 1 >= argc {
                libc::fprintf(stderr, cstr!("Missing value after --speedup\n"));
                return 2;
            }
            i += 1;
            let av = *argv.offset(i as isize) as *const c_char;
            let mut end: *mut c_char = ptr::null_mut();
            let parsed = libc::strtod(av, &mut end);
            if !end.is_null() && *end == 0 && parsed > 0.0 {
                emulation_speedup = parsed;
            } else {
                libc::fprintf(
                    stderr,
                    cstr!("Invalid --speedup value '%s' (expected positive number)\n"),
                    av,
                );
                return 2;
            }
            i += 1;
            continue;
        }
        if libc::strcmp(arg, cstr!("--patch-bundle")) == 0 {
            if i + 1 >= argc {
                libc::fprintf(stderr, cstr!("Missing value after --patch-bundle\n"));
                return 2;
            }
            i += 1;
            patch_load_bundle(*argv.offset(i as isize) as *const c_char);
            i += 1;
            continue;
        }
        if libc::strncmp(arg, cstr!("--patch-bundle="), 15) == 0 {
            patch_load_bundle(arg.add(15));
            i += 1;
            continue;
        }
        // Developer knobs (previously passed via env; now argv-only).
        if libc::strcmp(arg, cstr!("--verbose")) == 0 {
            shim_stdout_enabled = 1;
            i += 1;
            continue;
        }
        if libc::strncmp(arg, cstr!("--trace-file="), 13) == 0 {
            trace_file_path_arg = arg.add(13);
            i += 1;
            continue;
        }
        if libc::strcmp(arg, cstr!("--trace-file")) == 0 {
            if i + 1 >= argc {
                libc::fprintf(stderr, cstr!("Missing value after --trace-file\n"));
                return 2;
            }
            i += 1;
            trace_file_path_arg = *argv.offset(i as isize) as *const c_char;
            i += 1;
            continue;
        }
        if libc::strncmp(arg, cstr!("--lifecycle-file="), 17) == 0 {
            lifecycle_file_path_arg = arg.add(17);
            i += 1;
            continue;
        }
        if libc::strcmp(arg, cstr!("--lifecycle-file")) == 0 {
            if i + 1 >= argc {
                libc::fprintf(stderr, cstr!("Missing value after --lifecycle-file\n"));
                return 2;
            }
            i += 1;
            lifecycle_file_path_arg = *argv.offset(i as isize) as *const c_char;
            i += 1;
            continue;
        }
        // Drive knobs (previously passed via env; now argv-only).
        if libc::strncmp(arg, cstr!("--screenshot-secs="), 18) == 0 {
            SCREENSHOT_INTERVAL_SECS = libc::atoi(arg.add(18));
            i += 1;
            continue;
        }
        if libc::strcmp(arg, cstr!("--screenshot-secs")) == 0 {
            if i + 1 >= argc {
                libc::fprintf(stderr, cstr!("Missing value after --screenshot-secs\n"));
                return 2;
            }
            i += 1;
            SCREENSHOT_INTERVAL_SECS = libc::atoi(*argv.offset(i as isize) as *const c_char);
            i += 1;
            continue;
        }
        if libc::strcmp(arg, cstr!("--replay")) == 0 {
            // Replay determinism wants the virtual clock pinned at 0 before any
            // instruction runs; argv is parsed before run_machine, so this is
            // equivalent to the old constructor-time halt (was init_keyboard).
            vclock_state = VCLOCK_HALTED;
            vclock_frozen_virtual_ns = 0;
            shim_log_stdout(cstr!("[VCLOCK] --replay: initial halt at virtual_ns=0\n"));
            i += 1;
            continue;
        }
        i += 1;
    }
    // The --lifecycle-file path is known now (argv parsed above). Open the stream
    // eagerly so the file exists immediately, as it did when the path came from
    // the environment at constructor time. (The initial program LOAD emitted by
    // the init_memory constructor still lands in the in-memory ring, which is
    // dumped on exit; everything from here on streams live.)
    lifecycle_fp_open_if_requested();
    shim_log_stdout(
        cstr!("Emulation speedup multiplier: %.2fx\n"),
        emulation_speedup,
    );
    snapshot_init();
    if headless_mode == 0 {
        init_virtual_display();
        libc::atexit(virtual_display_shutdown_atexit);
    }
    if !restore_from.is_null() {
        if snapshot_restore_and_resume(restore_from) != 0 {
            libc::fprintf(stderr, cstr!("restore: failed; not resuming.\n"));
            return 3;
        }
        libc::fprintf(
            stderr,
            cstr!("\n[EXIT] restored dispatch returned without calling DOS terminate. cs:ip=%04X:%04X active_binary=%s\n[EXIT]   game's main loop exited via near ret instead of INT 21h AH=4Ch. No bundle written; check why the dispatch bubbled out (translator CFG issue or runtime ret-target mismatch).\n"),
            cs() as c_uint,
            ip() as c_uint,
            if shim_active_binary().is_null() { cstr!("<none>") } else { shim_active_binary() },
        );
        save_manager_sr_log(
            cstr!("exit RESTORE_DISPATCH_RETURNED cs:ip=%04X:%04X active=%s (game main bubbled out without DOS terminate after restore)"),
            cs() as c_uint,
            ip() as c_uint,
            if shim_active_binary().is_null() { cstr!("<none>") } else { shim_active_binary() },
        );
        libc::fflush(stderr);
    } else if !cfg().program_path.is_null() {
        run_machine();
        libc::fprintf(
            stderr,
            cstr!("\n[EXIT] run_machine returned without calling DOS terminate. cs:ip=%04X:%04X active_binary=%s\n[EXIT]   this is a translator/CFG bug: the game's main bubbled out via near ret instead of INT 21h AH=4Ch. No bundle written (it's not a runtime crash); investigate why the dispatch loop exited.\n"),
            cs() as c_uint,
            ip() as c_uint,
            if shim_active_binary().is_null() { cstr!("<none>") } else { shim_active_binary() },
        );
        save_manager_sr_log(
            cstr!("exit ENTRY_RETURNED cs:ip=%04X:%04X active=%s (translator bug: main bubbled out without DOS terminate)"),
            cs() as c_uint,
            ip() as c_uint,
            if shim_active_binary().is_null() { cstr!("<none>") } else { shim_active_binary() },
        );
        libc::fflush(stderr);
    } else {
        shim_log_stdout(cstr!(
            "Warning: no entry point configured; nothing to execute.\n"
        ));
    }
    0
}

#[no_mangle]
pub unsafe extern "C" fn shim_runtime_state_capture(out: *mut ShimRuntimeState) {
    if out.is_null() {
        return;
    }
    libc::memset(
        out as *mut c_void,
        0,
        core::mem::size_of::<ShimRuntimeState>(),
    );
    (*out).version = SHIM_RUNTIME_STATE_VERSION;

    (*out).bios_video.video_mode = bios_video.video_mode;
    (*out).bios_video.cursor_row = bios_video.cursor_row;
    (*out).bios_video.cursor_col = bios_video.cursor_col;
    (*out).bios_video.cursor_attr = bios_video.cursor_attr;
    (*out).bios_video.active_page = bios_video.active_page;
    (*out).bios_video.cga_palette_select = bios_video.cga_palette_select;
    (*out).bios_video.cga_border_color = bios_video.cga_border_color;
    (*out).cga = cga;
    (*out).current_display_width = current_display_width as i32;
    (*out).current_display_height = current_display_height as i32;
    (*out).virtual_display_buffer = virtual_display_buffer as i32;

    (*out).vga = vga;

    (*out).opl2 = opl2;

    (*out).pit = pit;
    (*out).pit_reload_value = pit_reload_value;
    (*out).pit_latched_value = pit_latched_value;
    (*out).pit_latch_valid = pit_latch_valid;
    (*out).pit_read_buffer = pit_read_buffer;
    (*out).pit_read_expect_high = pit_read_expect_high;
    (*out).pit_read_buffer_is_latch = pit_read_buffer_is_latch;
    (*out).bios_timer_tick_backlog = bios_timer_tick_backlog;
    (*out).bios_timer_tick_preincremented = bios_timer_tick_preincremented;
    (*out).pit_cycle_accum = pit_cycle_accum;
    (*out).pit_cycle_fraction_accum = pit_cycle_fraction_accum;

    (*out).next_free_seg = next_free_seg;
    (*out).program_min_block_paras = program_min_block_paras;
    (*out).null_guard_initial = null_guard_initial;
    (*out).a20_enabled = if a20_enabled { 1 } else { 0 };

    (*out).irq0_pending = irq0_pending;
    (*out).irq_pending = irq_pending;
    (*out).last_int_no = last_int_no;
}

#[no_mangle]
pub unsafe extern "C" fn shim_runtime_state_restore(in_: *const ShimRuntimeState) -> c_int {
    if in_.is_null() {
        return -1;
    }
    if (*in_).version != SHIM_RUNTIME_STATE_VERSION {
        libc::fprintf(
            stderr,
            cstr!("shim_runtime_state_restore: version mismatch — bundle has v%u, binary expects v%u. Re-capture the snapshot with the current build.\n"),
            (*in_).version as c_uint,
            SHIM_RUNTIME_STATE_VERSION as c_uint,
        );
        return -1;
    }

    apply_video_mode_state((*in_).bios_video.video_mode);
    bios_video.cursor_row = (*in_).bios_video.cursor_row;
    bios_video.cursor_col = (*in_).bios_video.cursor_col;
    bios_video.cursor_attr = (*in_).bios_video.cursor_attr;
    bios_video.active_page = (*in_).bios_video.active_page;
    bios_video.cga_palette_select = (*in_).bios_video.cga_palette_select;
    bios_video.cga_border_color = (*in_).bios_video.cga_border_color;
    cga = (*in_).cga;
    current_display_width = (*in_).current_display_width as c_int;
    current_display_height = (*in_).current_display_height as c_int;
    virtual_display_buffer = (*in_).virtual_display_buffer as c_int;

    vga = (*in_).vga;

    opl2 = (*in_).opl2;

    pit = (*in_).pit;
    pit_reload_value = (*in_).pit_reload_value;
    pit_latched_value = (*in_).pit_latched_value;
    pit_latch_valid = (*in_).pit_latch_valid;
    pit_read_buffer = (*in_).pit_read_buffer;
    pit_read_expect_high = (*in_).pit_read_expect_high;
    pit_read_buffer_is_latch = (*in_).pit_read_buffer_is_latch;
    bios_timer_tick_backlog = (*in_).bios_timer_tick_backlog;
    bios_timer_tick_preincremented = (*in_).bios_timer_tick_preincremented;
    pit_cycle_accum = (*in_).pit_cycle_accum;
    pit_cycle_fraction_accum = (*in_).pit_cycle_fraction_accum;

    next_free_seg = (*in_).next_free_seg;
    program_min_block_paras = (*in_).program_min_block_paras;
    null_guard_initial = (*in_).null_guard_initial;
    a20_set_enabled((*in_).a20_enabled != 0);

    irq0_pending = (*in_).irq0_pending;
    irq_pending = (*in_).irq_pending;
    irq_pending_count = (*ptr::addr_of!(irq_pending))
        .iter()
        .filter(|&&x| x != 0)
        .count() as u32;
    last_int_no = (*in_).last_int_no;
    0
}
