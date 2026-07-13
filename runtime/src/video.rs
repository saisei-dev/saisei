//! Port of `runtime/hw/video.c` — video card emulation (VGA/CGA/BIOS-video) +
//! the present pipeline (VRAM -> staged indices -> SDL / PNG). Owns the CGA
//! palette derivation cache, the 8x8 font, and the staging buffers.
//!
//! `vga`/`cga`/`bios_video` register state is DEFINED here (`#[no_mangle]`) and
//! shared with the still-C IO-port dispatch in shims.c and with bios.rs, which
//! bind it extern with matching `#[repr(C)]` layouts. ABI-identical to video.h.
//!
//! The screenshot path replaces stb_image_write with a self-contained PNG
//! encoder (stored-deflate zlib): the emitted bytes differ from stb's, but the
//! decoded pixels are identical — validated pixel-for-pixel against the C build.
#![allow(non_upper_case_globals)]
#![allow(static_mut_refs)]

use crate::cpu::{es, seg_off};
use core::ffi::{c_char, c_int};

// ---- register-state structs (video.h) --------------------------------------

#[repr(C)]
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
    /// VGA Sequencer (0x3C4 index / 0x3C5 data). SR2 is the Map Mask — which of
    /// the four planes a write to video memory lands in — and SR4 the memory
    /// mode. A game writes these before it writes a pixel, and reads them back to
    /// decide a VGA is there at all, so they must round-trip exactly.
    pub sequencer_index: u8,
    pub sequencer_regs: [u8; 8],
}

#[repr(C)]
pub struct CgaState {
    pub crtc_index: u8,
    pub crtc_regs: [u8; 0x20],
    pub hsync_base: u8,
    pub horiz_scroll: i32,
    pub hsync_initialized: u8,
}

#[repr(C)]
pub struct BiosVideoState {
    pub video_mode: u8,
    pub cursor_row: [u8; 8],
    pub cursor_col: [u8; 8],
    pub cursor_attr: [u8; 8],
    pub active_page: u8,
    pub cga_palette_select: u8,
    pub cga_border_color: u8,
}

#[no_mangle]
pub static mut vga: VgaState = VgaState {
    palette: [0; 256 * 3],
    palette_write_index: 0,
    palette_component: 0,
    palette_read_index: 0,
    palette_mask: 0xFF,
    attr_palette: [0; 16],
    border_color: 0,
    blink_mode: 0,
    dac_paging_mode: 0,
    dac_current_page: 0,
    misc_output: 0,
    feature_control: 0,
    graphics_index: 0,
    graphics_regs: [0; 16],
    sequencer_index: 0,
    sequencer_regs: [0; 8],
};

#[no_mangle]
pub static mut cga: CgaState = CgaState {
    crtc_index: 0,
    crtc_regs: [0; 0x20],
    hsync_base: 0,
    horiz_scroll: 0,
    hsync_initialized: 0,
};

#[no_mangle]
pub static mut bios_video: BiosVideoState = BiosVideoState {
    video_mode: 0x03,
    cursor_row: [0; 8],
    cursor_col: [0; 8],
    cursor_attr: [0; 8],
    active_page: 0,
    cga_palette_select: 0,
    cga_border_color: 0,
};

#[inline]
fn vs() -> *mut VgaState {
    core::ptr::addr_of_mut!(vga)
}
#[inline]
fn cs() -> *mut CgaState {
    core::ptr::addr_of_mut!(cga)
}
#[inline]
fn bv() -> *mut BiosVideoState {
    core::ptr::addr_of_mut!(bios_video)
}

// ---- still-C shims.c internals + globals -----------------------------------

extern "C" {
    static mut headless_mode: c_int;
    static mut current_display_width: c_int;
    static mut current_display_height: c_int;
    static mut virtual_display_buffer: c_int;
    fn memw_raw_read(seg: u16, off: u16) -> u16;
    fn memw_raw_write(seg: u16, off: u16, value: u16);
    fn copy_linear_from_segoff(seg: u16, off: u16, len: usize, dst: *mut u8);
}

const DATA_BASE_SEG: u16 = 0xFF2C; // rcb_fields.h

// ---- staging buffers (function-local statics in C) -------------------------

static mut STAGING_TEXT: [u8; 640 * 400] = [0; 640 * 400];
static mut STAGING_CGA: [u8; 640 * 200] = [0; 640 * 200];
static mut STAGING_TANDY: [u8; 640 * 200] = [0; 640 * 200];
static mut STAGING_PLANAR: [u8; 640 * 480] = [0; 640 * 480];
static mut STAGING_OTHER: [u8; 320 * 200] = [0; 320 * 200];

#[inline]
fn staging_text() -> *mut u8 {
    core::ptr::addr_of_mut!(STAGING_TEXT) as *mut u8
}
#[inline]
fn staging_cga() -> *mut u8 {
    core::ptr::addr_of_mut!(STAGING_CGA) as *mut u8
}
#[inline]
fn staging_tandy() -> *mut u8 {
    core::ptr::addr_of_mut!(STAGING_TANDY) as *mut u8
}
#[inline]
fn staging_planar() -> *mut u8 {
    core::ptr::addr_of_mut!(STAGING_PLANAR) as *mut u8
}
#[inline]
fn staging_other() -> *mut u8 {
    core::ptr::addr_of_mut!(STAGING_OTHER) as *mut u8
}

// ---- palette caches --------------------------------------------------------

static TEXT_MODE_PALETTE_6BIT: [[u8; 3]; 16] = [
    [0x00, 0x00, 0x00],
    [0x00, 0x00, 0x2A],
    [0x00, 0x2A, 0x00],
    [0x00, 0x2A, 0x2A],
    [0x2A, 0x00, 0x00],
    [0x2A, 0x00, 0x2A],
    [0x2A, 0x15, 0x00],
    [0x2A, 0x2A, 0x2A],
    [0x15, 0x15, 0x15],
    [0x15, 0x15, 0x3F],
    [0x15, 0x3F, 0x15],
    [0x15, 0x3F, 0x3F],
    [0x3F, 0x15, 0x15],
    [0x3F, 0x15, 0x3F],
    [0x3F, 0x3F, 0x15],
    [0x3F, 0x3F, 0x3F],
];

static mut TEXT_MODE_PALETTE: [u8; 256 * 3] = [0; 256 * 3];
static mut TEXT_MODE_PALETTE_INITIALIZED: bool = false;

static mut CGA_PALETTE: [u8; 256 * 3] = [0; 256 * 3];
static mut CGA_PALETTE_LAST_SELECT: u8 = 0xFF;
static mut CGA_PALETTE_LAST_BORDER: u8 = 0xFF;
static mut CGA_PALETTE_INITIALIZED: bool = false;

static mut TANDY_PALETTE: [u8; 16 * 3] = [0; 16 * 3];
static mut TANDY_PALETTE_INITIALIZED: bool = false;

#[inline]
fn text_pal() -> *mut u8 {
    core::ptr::addr_of_mut!(TEXT_MODE_PALETTE) as *mut u8
}
#[inline]
fn cga_pal() -> *mut u8 {
    core::ptr::addr_of_mut!(CGA_PALETTE) as *mut u8
}
#[inline]
fn tandy_pal() -> *mut u8 {
    core::ptr::addr_of_mut!(TANDY_PALETTE) as *mut u8
}

// ---- last-present branch (STAGE_PRESENT_BRANCH_*) --------------------------

const STAGE_PRESENT_BRANCH_UNKNOWN: c_int = 0;
const STAGE_PRESENT_BRANCH_TEXT: c_int = 1;
const STAGE_PRESENT_BRANCH_CGA: c_int = 2;
const STAGE_PRESENT_BRANCH_PLANAR: c_int = 3;
const STAGE_PRESENT_BRANCH_OTHER: c_int = 4;

static mut LAST_STAGE_PRESENT_BRANCH: c_int = STAGE_PRESENT_BRANCH_UNKNOWN;

// ---- byte helpers ----------------------------------------------------------

#[inline]
unsafe fn memb_get(seg: u16, off: u16) -> u8 {
    *seg_off(seg, off)
}
#[inline]
unsafe fn memb_set(seg: u16, off: u16, v: u8) {
    *seg_off(seg, off) = v;
}

// ---- DAC clamp -------------------------------------------------------------

#[no_mangle]
pub extern "C" fn vga_dac_component(value: u8) -> u8 {
    // VGA DAC channels are 6-bit; writes clamp to 0..63.
    value & 0x3F
}

// ---- mode predicates -------------------------------------------------------

#[no_mangle]
pub extern "C" fn is_text_mode(mode: u8) -> c_int {
    matches!(mode, 0x00 | 0x01 | 0x02 | 0x03 | 0x07) as c_int
}

#[no_mangle]
pub extern "C" fn is_cga_graphics_mode(mode: u8) -> c_int {
    matches!(mode, 0x04 | 0x05 | 0x06) as c_int
}

fn is_tandy_graphics_mode(mode: u8) -> bool {
    matches!(mode, 0x08 | 0x09 | 0x0A)
}
fn is_planar_graphics_mode(mode: u8) -> bool {
    matches!(mode, 0x0D | 0x0E | 0x10 | 0x12)
}

/// Same predicate, for `vga_mem` — the memory controller and the display decode
/// must agree on which modes put the pixels in the planes.
pub fn is_planar_graphics_mode_pub(mode: u8) -> bool {
    is_planar_graphics_mode(mode)
}

fn planar_mode_width(mode: u8) -> i32 {
    match mode {
        0x0E | 0x10 | 0x12 => 640,
        _ => 320,
    }
}
fn planar_mode_height(mode: u8) -> i32 {
    match mode {
        0x10 => 350,
        0x12 => 480,
        _ => 200,
    }
}
fn cga_mode_width(mode: u8) -> i32 {
    if mode == 0x06 {
        640
    } else {
        320
    }
}
fn tandy_mode_width(mode: u8) -> i32 {
    match mode {
        0x08 => 160,
        0x0A => 640,
        _ => 320,
    }
}
fn tandy_mode_height(_mode: u8) -> i32 {
    200
}

// ---- geometry --------------------------------------------------------------

unsafe fn ensure_display_geometry(width: c_int, height: c_int) {
    if headless_mode != 0 {
        return;
    }
    if width <= 0 || height <= 0 {
        return;
    }
    if width == current_display_width && height == current_display_height {
        return;
    }
    current_display_width = width;
    current_display_height = height;
    crate::sdl::virtual_display_configure(width, height);
}

// ---- BIOS default palette (INT 10h AH=00h) ---------------------------------

/// The 16 attribute-palette registers as the BIOS leaves them after a mode set.
/// These are EGA colour *values*, not indices: 6 bits `rgbRGB`, low three the
/// primary R/G/B and high three the secondary. Colour 6 is 0x14, not 0x06 —
/// that is what makes CGA brown brown instead of dark yellow.
const EGA_DEFAULT_ATTR_PALETTE: [u8; 16] = [
    0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x14, 0x07, 0x38, 0x39, 0x3A, 0x3B, 0x3C, 0x3D, 0x3E, 0x3F,
];

/// Decode one 6-bit EGA colour value to a 6-bit DAC triple. Each channel is a
/// 2-bit level (primary bit is the high one, secondary the low) scaled over the
/// full range: 0, 0x15, 0x2A, 0x3F.
fn ega_color_to_dac(value: u8) -> [u8; 3] {
    let level = |primary: u8, secondary: u8| -> u8 {
        let v = ((value >> primary) & 1) * 2 + ((value >> secondary) & 1);
        v * 0x15
    };
    [level(2, 5), level(1, 4), level(0, 3)]
}

/// What a real BIOS does on every INT 10h AH=00h and we did not: load the
/// default palette. Without it `vga.palette` stays at its power-on zeros, so a
/// game that programs the *attribute* registers (AH=10h) and then draws — which
/// is the whole EGA idiom — has every pixel index a DAC entry of black. The
/// screen is then not blank because nothing was drawn; it is blank because
/// everything was drawn in black.
///
/// Scope is modes 0x00-0x0E, whose default is the 64-colour EGA table. Mode 13h
/// has its own 256-entry default DAC, which is NOT loaded here.
unsafe fn load_default_palette(mode: u8) {
    if mode > 0x0E {
        return;
    }
    let vs = &mut *vs();
    vs.attr_palette = EGA_DEFAULT_ATTR_PALETTE;
    for i in 0..64usize {
        let rgb = ega_color_to_dac(i as u8);
        vs.palette[i * 3] = rgb[0];
        vs.palette[i * 3 + 1] = rgb[1];
        vs.palette[i * 3 + 2] = rgb[2];
    }
    vs.palette_mask = 0xFF;
}

/// The other half of what INT 10h AH=00h does and we did not: leave the
/// Sequencer and Graphics Controller in the state the BIOS leaves them.
///
/// It matters most for the two registers that are *masks*. From their power-on
/// zeros, a Map Mask of 0 enables no plane, so every write the guest makes is
/// discarded; and a Bit Mask of 0 protects every bit, so every merge just writes
/// the latch back. A game that programs neither (because a real BIOS already
/// did) draws its whole screen into a chip that is throwing all of it away.
unsafe fn load_default_chip_state(mode: u8) {
    let vs = &mut *vs();
    vs.sequencer_regs = [0; 8];
    vs.sequencer_regs[2] = 0x0F; // map mask: all four planes enabled
    vs.sequencer_regs[4] = if mode == 0x13 { 0x0E } else { 0x06 };
    vs.graphics_regs = [0; 16];
    vs.graphics_regs[5] = if mode == 0x13 { 0x40 } else { 0x00 }; // write mode 0
    vs.graphics_regs[6] = if is_text_mode(mode) != 0 { 0x0E } else { 0x05 };
    vs.graphics_regs[7] = 0x0F; // colour don't care: compare against all planes
    vs.graphics_regs[8] = 0xFF; // bit mask: no bit protected
}

// ---- palette derivation ----------------------------------------------------

unsafe fn ensure_text_mode_palette() {
    if TEXT_MODE_PALETTE_INITIALIZED {
        return;
    }
    let pal = text_pal();
    core::ptr::write_bytes(pal, 0, 256 * 3);
    for i in 0..16 {
        *pal.add(i * 3) = TEXT_MODE_PALETTE_6BIT[i][0];
        *pal.add(i * 3 + 1) = TEXT_MODE_PALETTE_6BIT[i][1];
        *pal.add(i * 3 + 2) = TEXT_MODE_PALETTE_6BIT[i][2];
    }
    TEXT_MODE_PALETTE_INITIALIZED = true;
}

unsafe fn ensure_cga_palette() {
    if CGA_PALETTE_INITIALIZED
        && CGA_PALETTE_LAST_SELECT == (*bv()).cga_palette_select
        && CGA_PALETTE_LAST_BORDER == (*bv()).cga_border_color
    {
        return;
    }

    let pal = cga_pal();
    core::ptr::write_bytes(pal, 0, 256 * 3);

    let background = (*bv()).cga_border_color & 0x0F;
    let indices: [u8; 4];

    if (*bv()).video_mode == 0x06 {
        // 640x200 monochrome: background from color register, foreground obeys
        // the intensity bit. Duplicate the foreground into the spare slots.
        let foreground = if (*bv()).cga_palette_select & 0x02 != 0 {
            0x0F
        } else {
            0x07
        };
        indices = [background, foreground, foreground, foreground];
    } else {
        let palette_index = (*bv()).cga_palette_select & 0x01;
        let intensity = ((*bv()).cga_palette_select >> 1) & 0x01;

        const PALETTE0: [u8; 3] = [0x02, 0x04, 0x06];
        const PALETTE0_HI: [u8; 3] = [0x0A, 0x0C, 0x0E];
        const PALETTE1: [u8; 3] = [0x03, 0x05, 0x07];
        const PALETTE1_HI: [u8; 3] = [0x0B, 0x0D, 0x0F];

        let src = if palette_index != 0 {
            if intensity != 0 {
                PALETTE1_HI
            } else {
                PALETTE1
            }
        } else if intensity != 0 {
            PALETTE0_HI
        } else {
            PALETTE0
        };

        indices = [background, src[0], src[1], src[2]];
    }

    for i in 0..4 {
        let idx = (indices[i] & 0x0F) as usize;
        *pal.add(i * 3) = TEXT_MODE_PALETTE_6BIT[idx][0];
        *pal.add(i * 3 + 1) = TEXT_MODE_PALETTE_6BIT[idx][1];
        *pal.add(i * 3 + 2) = TEXT_MODE_PALETTE_6BIT[idx][2];
    }

    CGA_PALETTE_INITIALIZED = true;
    CGA_PALETTE_LAST_SELECT = (*bv()).cga_palette_select;
    CGA_PALETTE_LAST_BORDER = (*bv()).cga_border_color;
}

unsafe fn ensure_tandy_palette() {
    if TANDY_PALETTE_INITIALIZED {
        return;
    }
    let pal = tandy_pal();
    for i in 0..16 {
        *pal.add(i * 3) = TEXT_MODE_PALETTE_6BIT[i][0];
        *pal.add(i * 3 + 1) = TEXT_MODE_PALETTE_6BIT[i][1];
        *pal.add(i * 3 + 2) = TEXT_MODE_PALETTE_6BIT[i][2];
    }
    TANDY_PALETTE_INITIALIZED = true;
}

// ---- staging ---------------------------------------------------------------

/// Rasterize the live text page into `dst` as palette indices, returning its
/// pixel geometry. Shared by the presenter and the screenshot so a captured
/// frame is the frame that was shown.
unsafe fn decode_text_mode(dst: *mut u8) -> (i32, i32) {
    const CELL_W: i32 = 8;
    const CELL_H: i32 = 8;
    const MAX_COLS: u16 = 80;
    const MAX_ROWS: u16 = 50;

    let mut cols = crate::bios::bios_video_columns();
    if cols == 0 {
        cols = 80;
    }
    if cols > MAX_COLS {
        cols = MAX_COLS;
    }
    let mut rows = crate::bios::bios_video_rows();
    if rows == 0 {
        rows = 25;
    }
    if rows > MAX_ROWS {
        rows = MAX_ROWS;
    }

    let page = memb_get(0x40, 0x62) & 0x07;
    let stride = crate::bios::bios_page_stride();
    let base = (page as u32 % 8) * stride as u32;
    let segment = if (*bv()).video_mode == 0x07 {
        0xB000u16
    } else {
        0xB800u16
    };
    let src = seg_off(segment, base as u16);

    let staging = dst;
    let width = cols as i32 * CELL_W;
    let height = rows as i32 * CELL_H;

    for row in 0..rows {
        for col in 0..cols {
            let cell_index = row as usize * cols as usize + col as usize;
            let glyph_code = *src.add(cell_index * 2);
            let attr = *src.add(cell_index * 2 + 1);
            let glyph = &FONT8X8_BASIC[(glyph_code & 0x7F) as usize];
            let fg = attr & 0x0F;
            let mut bg = (attr >> 4) & 0x07;
            if attr & 0x80 != 0 {
                bg |= 0x08;
            }
            for gy in 0..CELL_H {
                let bits = glyph[gy as usize];
                let dst = staging
                    .add(((row as i32 * CELL_H + gy) * width + col as i32 * CELL_W) as usize);
                for gx in 0..8 {
                    let mask = 1u8 << gx;
                    *dst.add(gx as usize) = if bits & mask != 0 { fg } else { bg };
                }
            }
        }
    }

    (width, height)
}

unsafe fn stage_and_present_text_mode() {
    let staging = staging_text();
    let (width, height) = decode_text_mode(staging);
    ensure_text_mode_palette();
    ensure_display_geometry(width, height);
    crate::sdl::virtual_display_present(staging, width, width, height, text_pal(), 0x3F);
}

unsafe fn decode_cga_mode(mode: u8, dst: *mut u8) {
    const H: i32 = 200;
    const BYTES_PER_ROW: u32 = 80;
    const CGA_VRAM_MASK: u32 = 0x4000 - 1;

    let vram = seg_off(0xB800, 0);
    let width = cga_mode_width(mode);
    let start_offset =
        (((*cs()).crtc_regs[0x0C] as u32) << 8 | (*cs()).crtc_regs[0x0D] as u32) & CGA_VRAM_MASK;
    let mut scroll = if (*cs()).hsync_initialized != 0 {
        (*cs()).horiz_scroll
    } else {
        0
    };
    if scroll >= width || scroll <= -width {
        scroll %= width;
    }

    for y in 0..H {
        let plane_offset = (y as u32 & 1) * 0x2000 + (y as u32 >> 1) * BYTES_PER_ROW;
        let base_offset = (plane_offset + start_offset) & CGA_VRAM_MASK;
        let row = dst.add((y * width) as usize);
        if mode == 0x06 {
            for x in 0..width {
                let mut src_x = x + scroll;
                if src_x < 0 {
                    src_x += width;
                } else if src_x >= width {
                    src_x -= width;
                }
                let byte = src_x >> 3;
                let bit = 7 - (src_x & 7);
                let addr = (base_offset + byte as u32) & CGA_VRAM_MASK;
                let packed = *vram.add(addr as usize);
                *row.add(x as usize) = (packed >> bit) & 0x01;
            }
        } else {
            for x in 0..width {
                let mut src_x = x + scroll;
                if src_x < 0 {
                    src_x += width;
                } else if src_x >= width {
                    src_x -= width;
                }
                let byte = src_x >> 2;
                let sub = src_x & 3;
                let addr = (base_offset + byte as u32) & CGA_VRAM_MASK;
                let packed = *vram.add(addr as usize);
                // 320x200 4-color: 2-bit pixels packed with leftmost in bits 7-6.
                let shift = (3 - sub) * 2;
                *row.add(x as usize) = (packed >> shift) & 0x03;
            }
        }
    }
}

unsafe fn stage_and_present_cga_mode() {
    const H: i32 = 200;
    let mode = (*bv()).video_mode;
    let width = cga_mode_width(mode);
    let staging = staging_cga();
    decode_cga_mode(mode, staging);
    ensure_cga_palette();
    ensure_display_geometry(width, H);
    crate::sdl::virtual_display_present(staging, width, width, H, cga_pal(), 0x3F);
}

unsafe fn decode_tandy_mode(mode: u8, dst: *mut u8) {
    const TANDY_VRAM_MASK: u32 = 0x8000 - 1;

    let vram = seg_off(0xB800, 0);
    let width = tandy_mode_width(mode);
    let height = tandy_mode_height(mode);
    let bytes_per_row = width / 2;

    let start_offset =
        (((*cs()).crtc_regs[0x0C] as u32) << 8 | (*cs()).crtc_regs[0x0D] as u32) & TANDY_VRAM_MASK;

    for y in 0..height {
        let base_offset = (start_offset + y as u32 * bytes_per_row as u32) & TANDY_VRAM_MASK;
        let row = dst.add((y * width) as usize);
        for byte in 0..bytes_per_row {
            let addr = (base_offset + byte as u32) & TANDY_VRAM_MASK;
            let packed = *vram.add(addr as usize);
            *row.add((byte * 2) as usize) = packed >> 4;
            *row.add((byte * 2 + 1) as usize) = packed & 0x0F;
        }
    }
}

unsafe fn stage_and_present_tandy_mode() {
    const MAX_W: i32 = 640;
    const MAX_H: i32 = 200;
    let mode = (*bv()).video_mode;
    let width = tandy_mode_width(mode);
    let height = tandy_mode_height(mode);
    if width > MAX_W || height > MAX_H {
        return;
    }
    let staging = staging_tandy();
    decode_tandy_mode(mode, staging);
    ensure_tandy_palette();
    ensure_display_geometry(width, height);
    crate::sdl::virtual_display_present(staging, width, width, height, tandy_pal(), 0x3F);
}

unsafe fn decode_planar_mode(mode: u8, dst: *mut u8) {
    let width = planar_mode_width(mode);
    let height = planar_mode_height(mode);
    let bytes_per_row = width / 8;

    // The pixels are in the four planes, not in the 0xA0000 page of guest RAM:
    // all four planes answer to the same addresses, and which one a CPU write
    // reached was decided by the Map Mask (see vga_mem).
    let attr_palette = core::ptr::addr_of!((*vs()).attr_palette) as *const u8;

    for y in 0..height {
        let row_off = (y * bytes_per_row) as usize;
        let row = dst.add((y * width) as usize);

        for byte in 0..bytes_per_row {
            let b0 = crate::vga_mem::plane_byte(0, row_off + byte as usize);
            let b1 = crate::vga_mem::plane_byte(1, row_off + byte as usize);
            let b2 = crate::vga_mem::plane_byte(2, row_off + byte as usize);
            let b3 = crate::vga_mem::plane_byte(3, row_off + byte as usize);

            for bit in 0..8 {
                let mask = 0x80u8 >> bit;
                let mut color = 0u8;
                if b0 & mask != 0 {
                    color |= 0x01;
                }
                if b1 & mask != 0 {
                    color |= 0x02;
                }
                if b2 & mask != 0 {
                    color |= 0x04;
                }
                if b3 & mask != 0 {
                    color |= 0x08;
                }
                *row.add((byte * 8 + bit) as usize) = *attr_palette.add((color & 0x0F) as usize);
            }
        }
    }
}

unsafe fn stage_and_present_planar_mode() {
    const MAX_W: i32 = 640;
    const MAX_H: i32 = 480;
    let mode = (*bv()).video_mode;
    let width = planar_mode_width(mode);
    let height = planar_mode_height(mode);
    if width > MAX_W || height > MAX_H {
        return;
    }
    let staging = staging_planar();
    decode_planar_mode(mode, staging);
    ensure_display_geometry(width, height);
    crate::sdl::virtual_display_present(
        staging,
        width,
        width,
        height,
        (*vs()).palette.as_ptr(),
        (*vs()).palette_mask,
    );
}

#[no_mangle]
pub extern "C" fn stage_and_present_current_buffer() {
    unsafe {
        let mode = (*bv()).video_mode;
        // Pre-game phase: hold the Saisei logo through the game's text-mode
        // console/setup screens (e.g. Dungeon Master's drive prompt) rather than
        // presenting the text buffer. Only real graphics output retires the logo
        // (see sdl::splash_is_up); input still flows via virtual_display_poll_input.
        if headless_mode == 0 && is_text_mode(mode) != 0 && crate::sdl::splash_is_up() {
            LAST_STAGE_PRESENT_BRANCH = STAGE_PRESENT_BRANCH_TEXT;
            crate::sdl::show_splash();
            return;
        }
        if is_text_mode(mode) != 0 {
            LAST_STAGE_PRESENT_BRANCH = STAGE_PRESENT_BRANCH_TEXT;
            stage_and_present_text_mode();
            return;
        }
        if is_cga_graphics_mode(mode) != 0 {
            LAST_STAGE_PRESENT_BRANCH = STAGE_PRESENT_BRANCH_CGA;
            stage_and_present_cga_mode();
            return;
        }
        if is_tandy_graphics_mode(mode) {
            LAST_STAGE_PRESENT_BRANCH = STAGE_PRESENT_BRANCH_CGA;
            stage_and_present_tandy_mode();
            return;
        }
        if is_planar_graphics_mode(mode) {
            LAST_STAGE_PRESENT_BRANCH = STAGE_PRESENT_BRANCH_PLANAR;
            stage_and_present_planar_mode();
            return;
        }

        LAST_STAGE_PRESENT_BRANCH = STAGE_PRESENT_BRANCH_OTHER;

        const W: i32 = 320;
        const H: i32 = 200;
        const BYTES: usize = (W * H) as usize;

        // Decide which source to show: 0=A000, 1=BB0, 2=BB1
        let which = virtual_display_buffer;
        let seg: u16;
        let off: u16;
        if which == 0 {
            seg = 0xA000; // VGA memory
            off = 0x0000;
        } else {
            seg = memw_raw_read(es(), DATA_BASE_SEG); // game's backbuffer segment (RCB)
            off = if which == 1 { 0x0000 } else { 0x4000 };
        }

        let staging = staging_other();
        copy_linear_from_segoff(seg, off, BYTES, staging);
        ensure_display_geometry(W, H);
        crate::sdl::virtual_display_present(
            staging,
            W,
            W,
            H,
            (*vs()).palette.as_ptr(),
            (*vs()).palette_mask,
        );
    }
}

#[no_mangle]
pub extern "C" fn shim_last_stage_present_branch() -> c_int {
    unsafe { LAST_STAGE_PRESENT_BRANCH }
}

#[no_mangle]
pub extern "C" fn shim_stage_and_present_current_buffer() {
    stage_and_present_current_buffer();
}

#[no_mangle]
pub extern "C" fn shim_present_current_buffer() {
    stage_and_present_current_buffer();
}

#[no_mangle]
pub extern "C" fn apply_video_mode_state(mode: u8) {
    unsafe {
        (*bv()).video_mode = mode;
        (*cs()).hsync_initialized = 0;
        (*cs()).horiz_scroll = 0;
        (*cs()).crtc_index = 0;
        core::ptr::write_bytes(
            core::ptr::addr_of_mut!((*cs()).crtc_regs) as *mut u8,
            0,
            0x20,
        );
        memb_set(0x40, 0x49, mode);
        let crtc_port = if mode == 0x07 { 0x3B4u16 } else { 0x3D4u16 };
        memw_raw_write(0x40, 0x0063, crtc_port);
        load_default_palette(mode);
        load_default_chip_state(mode);
        crate::vga_mem::refresh_planar_active(mode);
        if headless_mode == 0 {
            crate::sdl::virtual_display_set_mode(mode as c_int);
            if is_text_mode(mode) != 0 {
                // Keep the fixed splash-logo window while it's still up; a
                // text-console mode-set must not resize it to 80x25 geometry
                // (that resize is the "reset to odd setup" during DM's prompt).
                if !crate::sdl::splash_is_up() {
                    let mut cols = crate::bios::bios_video_columns();
                    if cols == 0 {
                        cols = if mode == 0x00 || mode == 0x01 { 40 } else { 80 };
                    }
                    let mut rows = crate::bios::bios_video_rows();
                    if rows == 0 {
                        rows = 25;
                    }
                    ensure_display_geometry(cols as i32 * 8, rows as i32 * 8);
                }
            } else if is_cga_graphics_mode(mode) != 0 || mode == 0x13 {
                ensure_display_geometry(320, 200);
            } else if is_tandy_graphics_mode(mode) {
                ensure_display_geometry(tandy_mode_width(mode), tandy_mode_height(mode));
            } else if is_planar_graphics_mode(mode) {
                ensure_display_geometry(planar_mode_width(mode), planar_mode_height(mode));
            }
        }
        for page in 0..8u16 {
            (*bv()).cursor_row[page as usize] = 0;
            (*bv()).cursor_col[page as usize] = 0;
            (*bv()).cursor_attr[page as usize] = 0x07;
            memw_raw_write(0x40, 0x50 + page * 2, 0);
        }
        (*bv()).active_page = 0;
    }
}

// ---- screenshot ------------------------------------------------------------

#[no_mangle]
pub extern "C" fn shim_render_screenshot_png(path: *const c_char) -> c_int {
    unsafe {
        // Decode exactly the way the display presents (stage_and_present_current
        // _buffer): a screenshot that re-derives the frame its own way is a
        // picture of a machine nobody is running. A planar EGA mode read as a
        // linear 0xA000 byte array through the (never-programmed) VGA DAC is how
        // this used to hand back a black frame for a game that was drawing.
        let mode = (*bv()).video_mode;
        let (width, height): (i32, i32) = if is_text_mode(mode) != 0 {
            (640, 400) // upper bound; decode_text_mode returns the real geometry
        } else if is_cga_graphics_mode(mode) != 0 {
            (cga_mode_width(mode), 200)
        } else if is_tandy_graphics_mode(mode) {
            (tandy_mode_width(mode), tandy_mode_height(mode))
        } else if is_planar_graphics_mode(mode) {
            (planar_mode_width(mode), planar_mode_height(mode))
        } else {
            (320, 200)
        };
        let mut indices = vec![0u8; (width * height) as usize];

        let palette: *const u8;
        let palette_mask: u8;
        let (width, height) = if is_text_mode(mode) != 0 {
            let geom = decode_text_mode(indices.as_mut_ptr());
            ensure_text_mode_palette();
            palette = text_pal();
            palette_mask = 0x3F;
            geom
        } else {
            if is_cga_graphics_mode(mode) != 0 {
                decode_cga_mode(mode, indices.as_mut_ptr());
                ensure_cga_palette();
                palette = cga_pal();
                palette_mask = 0x3F;
            } else if is_tandy_graphics_mode(mode) {
                decode_tandy_mode(mode, indices.as_mut_ptr());
                ensure_tandy_palette();
                palette = tandy_pal();
                palette_mask = 0x3F;
            } else {
                if is_planar_graphics_mode(mode) {
                    decode_planar_mode(mode, indices.as_mut_ptr());
                } else {
                    let src = seg_off(0xA000, 0);
                    core::ptr::copy_nonoverlapping(
                        src,
                        indices.as_mut_ptr(),
                        (width * height) as usize,
                    );
                }
                palette = (*vs()).palette.as_ptr();
                palette_mask = (*vs()).palette_mask;
            }
            (width, height)
        };
        let n = (width * height) as usize;

        let mut img = vec![0u8; n * 3];
        for y in 0..height {
            for x in 0..width {
                let idx = indices[(y * width + x) as usize] as usize;
                let mut r = *palette.add(idx * 3) & palette_mask;
                let mut g = *palette.add(idx * 3 + 1) & palette_mask;
                let mut b = *palette.add(idx * 3 + 2) & palette_mask;
                r = (r << 2) | (r >> 4);
                g = (g << 2) | (g >> 4);
                b = (b << 2) | (b >> 4);
                let out = ((y * width + x) * 3) as usize;
                img[out] = r;
                img[out + 1] = g;
                img[out + 2] = b;
            }
        }

        let cpath = core::ffi::CStr::from_ptr(path);
        match write_png_file(cpath.to_bytes(), width as usize, height as usize, &img) {
            Ok(()) => 0,
            Err(_) => -1,
        }
    }
}

#[no_mangle]
pub extern "C" fn video_invalidate_palette_cache() {
    unsafe {
        CGA_PALETTE_INITIALIZED = false;
        CGA_PALETTE_LAST_SELECT = 0xFF;
        CGA_PALETTE_LAST_BORDER = 0xFF;
        TANDY_PALETTE_INITIALIZED = false;
    }
}

#[no_mangle]
pub extern "C" fn bios_cga_render_teletype(
    row: u8,
    col: u8,
    ch_val: u8,
    attr: u8,
    stored_attr: u8,
) {
    unsafe {
        let mode = (*bv()).video_mode;
        if is_cga_graphics_mode(mode) == 0 {
            return;
        }

        const CELL_W: i32 = 8;
        const CELL_H: i32 = 8;
        const HEIGHT: i32 = 200;
        const BYTES_PER_ROW: u32 = 80;
        const CGA_VRAM_MASK: u32 = 0x4000 - 1;

        let width = cga_mode_width(mode);
        if width <= 0 {
            return;
        }

        let pixel_x_base = col as i32 * CELL_W;
        let pixel_y_base = row as i32 * CELL_H;
        if pixel_x_base >= width || pixel_y_base >= HEIGHT {
            return;
        }

        let glyph = &FONT8X8_BASIC[(ch_val & 0x7F) as usize];
        let mut fg = attr & 0x0F;
        let mut bg = (stored_attr >> 4) & 0x0F;
        if mode == 0x06 {
            fg &= 0x01;
            bg &= 0x01;
        } else {
            fg &= 0x03;
            bg &= 0x03;
        }

        let vram = seg_off(0xB800, 0);
        let start_offset = (((*cs()).crtc_regs[0x0C] as u32) << 8 | (*cs()).crtc_regs[0x0D] as u32)
            & CGA_VRAM_MASK;
        let mut scroll = if (*cs()).hsync_initialized != 0 {
            (*cs()).horiz_scroll
        } else {
            0
        };
        if scroll >= width || scroll <= -width {
            scroll %= width;
        }

        for gy in 0..CELL_H {
            let y = pixel_y_base + gy;
            if y >= HEIGHT {
                break;
            }
            let bits = glyph[gy as usize];
            let plane_offset = (y as u32 & 1) * 0x2000 + (y as u32 >> 1) * BYTES_PER_ROW;
            let base_offset = (plane_offset + start_offset) & CGA_VRAM_MASK;

            for gx in 0..CELL_W {
                let x = pixel_x_base + gx;
                if x >= width {
                    break;
                }

                let mut mem_x = x + scroll;
                if mem_x < 0 {
                    mem_x += width;
                } else if mem_x >= width {
                    mem_x -= width;
                }

                let mask = 1u8 << gx;
                let color = if bits & mask != 0 { fg } else { bg };
                if mode == 0x06 {
                    let byte = mem_x >> 3;
                    let bit = 7 - (mem_x & 7);
                    let addr = (base_offset + byte as u32) & CGA_VRAM_MASK;
                    let mut existing = *vram.add(addr as usize);
                    if color & 0x01 != 0 {
                        existing |= 1u8 << bit;
                    } else {
                        existing &= !(1u8 << bit);
                    }
                    *vram.add(addr as usize) = existing;
                } else {
                    let byte = mem_x >> 2;
                    let sub = mem_x & 3;
                    let shift = (3 - sub) * 2;
                    let addr = (base_offset + byte as u32) & CGA_VRAM_MASK;
                    let mut existing = *vram.add(addr as usize);
                    existing &= !(0x03u8 << shift);
                    existing |= (color & 0x03) << shift;
                    *vram.add(addr as usize) = existing;
                }
            }
        }
    }
}

/// `stbi_write_png`-compatible export. shims.c's `shim_save_video_memory`
/// (palette dump) used stb_image_write's `stbi_write_png`; with shims.c ported,
/// that symbol has no C provider. Reuse this module's PNG encoder. Only RGB
/// (`comp == 3`) is exercised. Returns non-zero on success (stb convention).
#[no_mangle]
pub extern "C" fn stbi_write_png(
    filename: *const c_char,
    w: c_int,
    h: c_int,
    comp: c_int,
    data: *const core::ffi::c_void,
    stride_bytes: c_int,
) -> c_int {
    if filename.is_null() || data.is_null() || w <= 0 || h <= 0 || comp != 3 {
        return 0;
    }
    let (w, h) = (w as usize, h as usize);
    let stride = if stride_bytes != 0 {
        stride_bytes as usize
    } else {
        w * 3
    };
    let src = data as *const u8;
    let mut rgb = vec![0u8; w * h * 3];
    unsafe {
        for y in 0..h {
            core::ptr::copy_nonoverlapping(
                src.add(y * stride),
                rgb.as_mut_ptr().add(y * w * 3),
                w * 3,
            );
        }
        let cpath = core::ffi::CStr::from_ptr(filename);
        match write_png_file(cpath.to_bytes(), w, h, &rgb) {
            Ok(()) => 1,
            Err(_) => 0,
        }
    }
}

// ---- minimal PNG encoder (stored-deflate zlib) -----------------------------

fn write_png_file(path_bytes: &[u8], w: usize, h: usize, rgb: &[u8]) -> std::io::Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let path = std::path::Path::new(std::ffi::OsStr::from_bytes(path_bytes));
    let png = encode_png(w, h, rgb);
    std::fs::write(path, &png)
}

/// Test-only: dump an RGB24 buffer to a PNG using this module's encoder.
#[cfg(test)]
pub(crate) fn write_png_for_test(path: &str, w: usize, h: usize, rgb: &[u8]) {
    let _ = write_png_file(path.as_bytes(), w, h, rgb);
}

fn encode_png(w: usize, h: usize, rgb: &[u8]) -> Vec<u8> {
    // Raw = filtered scanlines: each row prefixed with filter byte 0 (none).
    let mut raw = Vec::with_capacity(h * (1 + w * 3));
    for y in 0..h {
        raw.push(0);
        raw.extend_from_slice(&rgb[y * w * 3..y * w * 3 + w * 3]);
    }

    // zlib stream: 0x78 0x01 header + stored deflate blocks + adler32.
    let mut z = Vec::with_capacity(raw.len() + raw.len() / 65535 * 5 + 16);
    z.push(0x78);
    z.push(0x01);
    if raw.is_empty() {
        z.push(1);
        z.extend_from_slice(&0u16.to_le_bytes());
        z.extend_from_slice(&0xFFFFu16.to_le_bytes());
    } else {
        let mut i = 0;
        while i < raw.len() {
            let n = core::cmp::min(65535, raw.len() - i);
            let last = i + n >= raw.len();
            z.push(if last { 1 } else { 0 });
            z.extend_from_slice(&(n as u16).to_le_bytes());
            z.extend_from_slice(&(!(n as u16)).to_le_bytes());
            z.extend_from_slice(&raw[i..i + n]);
            i += n;
        }
    }
    z.extend_from_slice(&adler32(&raw).to_be_bytes());

    let mut out = Vec::new();
    out.extend_from_slice(&[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]);
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&(w as u32).to_be_bytes());
    ihdr.extend_from_slice(&(h as u32).to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]); // 8-bit, colortype 2 (RGB), no interlace
    push_chunk(&mut out, b"IHDR", &ihdr);
    push_chunk(&mut out, b"IDAT", &z);
    push_chunk(&mut out, b"IEND", &[]);
    out
}

fn push_chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    let start = out.len();
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let crc = crc32(&out[start..]);
    out.extend_from_slice(&crc.to_be_bytes());
}

fn adler32(data: &[u8]) -> u32 {
    const MOD: u32 = 65521;
    let mut a = 1u32;
    let mut b = 0u32;
    for &x in data {
        a = (a + x as u32) % MOD;
        b = (b + a) % MOD;
    }
    (b << 16) | a
}

fn crc32(data: &[u8]) -> u32 {
    let mut c = 0xFFFF_FFFFu32;
    for &byte in data {
        c ^= byte as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 {
                (c >> 1) ^ 0xEDB8_8320
            } else {
                c >> 1
            };
        }
    }
    c ^ 0xFFFF_FFFF
}

// ---- 8x8 font (public domain, from font8x8_basic.h) ------------------------

static FONT8X8_BASIC: [[u8; 8]; 128] = [
    [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
    [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
    [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
    [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
    [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
    [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
    [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
    [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
    [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
    [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
    [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
    [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
    [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
    [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
    [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
    [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
    [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
    [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
    [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
    [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
    [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
    [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
    [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
    [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
    [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
    [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
    [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
    [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
    [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
    [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
    [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
    [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
    [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
    [0x18, 0x3C, 0x3C, 0x18, 0x18, 0x00, 0x18, 0x00],
    [0x36, 0x36, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
    [0x36, 0x36, 0x7F, 0x36, 0x7F, 0x36, 0x36, 0x00],
    [0x0C, 0x3E, 0x03, 0x1E, 0x30, 0x1F, 0x0C, 0x00],
    [0x00, 0x63, 0x33, 0x18, 0x0C, 0x66, 0x63, 0x00],
    [0x1C, 0x36, 0x1C, 0x6E, 0x3B, 0x33, 0x6E, 0x00],
    [0x06, 0x06, 0x03, 0x00, 0x00, 0x00, 0x00, 0x00],
    [0x18, 0x0C, 0x06, 0x06, 0x06, 0x0C, 0x18, 0x00],
    [0x06, 0x0C, 0x18, 0x18, 0x18, 0x0C, 0x06, 0x00],
    [0x00, 0x66, 0x3C, 0xFF, 0x3C, 0x66, 0x00, 0x00],
    [0x00, 0x0C, 0x0C, 0x3F, 0x0C, 0x0C, 0x00, 0x00],
    [0x00, 0x00, 0x00, 0x00, 0x00, 0x0C, 0x0C, 0x06],
    [0x00, 0x00, 0x00, 0x3F, 0x00, 0x00, 0x00, 0x00],
    [0x00, 0x00, 0x00, 0x00, 0x00, 0x0C, 0x0C, 0x00],
    [0x60, 0x30, 0x18, 0x0C, 0x06, 0x03, 0x01, 0x00],
    [0x3E, 0x63, 0x73, 0x7B, 0x6F, 0x67, 0x3E, 0x00],
    [0x0C, 0x0E, 0x0C, 0x0C, 0x0C, 0x0C, 0x3F, 0x00],
    [0x1E, 0x33, 0x30, 0x1C, 0x06, 0x33, 0x3F, 0x00],
    [0x1E, 0x33, 0x30, 0x1C, 0x30, 0x33, 0x1E, 0x00],
    [0x38, 0x3C, 0x36, 0x33, 0x7F, 0x30, 0x78, 0x00],
    [0x3F, 0x03, 0x1F, 0x30, 0x30, 0x33, 0x1E, 0x00],
    [0x1C, 0x06, 0x03, 0x1F, 0x33, 0x33, 0x1E, 0x00],
    [0x3F, 0x33, 0x30, 0x18, 0x0C, 0x0C, 0x0C, 0x00],
    [0x1E, 0x33, 0x33, 0x1E, 0x33, 0x33, 0x1E, 0x00],
    [0x1E, 0x33, 0x33, 0x3E, 0x30, 0x18, 0x0E, 0x00],
    [0x00, 0x0C, 0x0C, 0x00, 0x00, 0x0C, 0x0C, 0x00],
    [0x00, 0x0C, 0x0C, 0x00, 0x00, 0x0C, 0x0C, 0x06],
    [0x18, 0x0C, 0x06, 0x03, 0x06, 0x0C, 0x18, 0x00],
    [0x00, 0x00, 0x3F, 0x00, 0x00, 0x3F, 0x00, 0x00],
    [0x06, 0x0C, 0x18, 0x30, 0x18, 0x0C, 0x06, 0x00],
    [0x1E, 0x33, 0x30, 0x18, 0x0C, 0x00, 0x0C, 0x00],
    [0x3E, 0x63, 0x7B, 0x7B, 0x7B, 0x03, 0x1E, 0x00],
    [0x0C, 0x1E, 0x33, 0x33, 0x3F, 0x33, 0x33, 0x00],
    [0x3F, 0x66, 0x66, 0x3E, 0x66, 0x66, 0x3F, 0x00],
    [0x3C, 0x66, 0x03, 0x03, 0x03, 0x66, 0x3C, 0x00],
    [0x1F, 0x36, 0x66, 0x66, 0x66, 0x36, 0x1F, 0x00],
    [0x7F, 0x46, 0x16, 0x1E, 0x16, 0x46, 0x7F, 0x00],
    [0x7F, 0x46, 0x16, 0x1E, 0x16, 0x06, 0x0F, 0x00],
    [0x3C, 0x66, 0x03, 0x03, 0x73, 0x66, 0x7C, 0x00],
    [0x33, 0x33, 0x33, 0x3F, 0x33, 0x33, 0x33, 0x00],
    [0x1E, 0x0C, 0x0C, 0x0C, 0x0C, 0x0C, 0x1E, 0x00],
    [0x78, 0x30, 0x30, 0x30, 0x33, 0x33, 0x1E, 0x00],
    [0x67, 0x66, 0x36, 0x1E, 0x36, 0x66, 0x67, 0x00],
    [0x0F, 0x06, 0x06, 0x06, 0x46, 0x66, 0x7F, 0x00],
    [0x63, 0x77, 0x7F, 0x7F, 0x6B, 0x63, 0x63, 0x00],
    [0x63, 0x67, 0x6F, 0x7B, 0x73, 0x63, 0x63, 0x00],
    [0x1C, 0x36, 0x63, 0x63, 0x63, 0x36, 0x1C, 0x00],
    [0x3F, 0x66, 0x66, 0x3E, 0x06, 0x06, 0x0F, 0x00],
    [0x1E, 0x33, 0x33, 0x33, 0x3B, 0x1E, 0x38, 0x00],
    [0x3F, 0x66, 0x66, 0x3E, 0x36, 0x66, 0x67, 0x00],
    [0x1E, 0x33, 0x07, 0x0E, 0x38, 0x33, 0x1E, 0x00],
    [0x3F, 0x2D, 0x0C, 0x0C, 0x0C, 0x0C, 0x1E, 0x00],
    [0x33, 0x33, 0x33, 0x33, 0x33, 0x33, 0x3F, 0x00],
    [0x33, 0x33, 0x33, 0x33, 0x33, 0x1E, 0x0C, 0x00],
    [0x63, 0x63, 0x63, 0x6B, 0x7F, 0x77, 0x63, 0x00],
    [0x63, 0x63, 0x36, 0x1C, 0x1C, 0x36, 0x63, 0x00],
    [0x33, 0x33, 0x33, 0x1E, 0x0C, 0x0C, 0x1E, 0x00],
    [0x7F, 0x63, 0x31, 0x18, 0x4C, 0x66, 0x7F, 0x00],
    [0x1E, 0x06, 0x06, 0x06, 0x06, 0x06, 0x1E, 0x00],
    [0x03, 0x06, 0x0C, 0x18, 0x30, 0x60, 0x40, 0x00],
    [0x1E, 0x18, 0x18, 0x18, 0x18, 0x18, 0x1E, 0x00],
    [0x08, 0x1C, 0x36, 0x63, 0x00, 0x00, 0x00, 0x00],
    [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xFF],
    [0x0C, 0x0C, 0x18, 0x00, 0x00, 0x00, 0x00, 0x00],
    [0x00, 0x00, 0x1E, 0x30, 0x3E, 0x33, 0x6E, 0x00],
    [0x07, 0x06, 0x06, 0x3E, 0x66, 0x66, 0x3B, 0x00],
    [0x00, 0x00, 0x1E, 0x33, 0x03, 0x33, 0x1E, 0x00],
    [0x38, 0x30, 0x30, 0x3e, 0x33, 0x33, 0x6E, 0x00],
    [0x00, 0x00, 0x1E, 0x33, 0x3f, 0x03, 0x1E, 0x00],
    [0x1C, 0x36, 0x06, 0x0f, 0x06, 0x06, 0x0F, 0x00],
    [0x00, 0x00, 0x6E, 0x33, 0x33, 0x3E, 0x30, 0x1F],
    [0x07, 0x06, 0x36, 0x6E, 0x66, 0x66, 0x67, 0x00],
    [0x0C, 0x00, 0x0E, 0x0C, 0x0C, 0x0C, 0x1E, 0x00],
    [0x30, 0x00, 0x30, 0x30, 0x30, 0x33, 0x33, 0x1E],
    [0x07, 0x06, 0x66, 0x36, 0x1E, 0x36, 0x67, 0x00],
    [0x0E, 0x0C, 0x0C, 0x0C, 0x0C, 0x0C, 0x1E, 0x00],
    [0x00, 0x00, 0x33, 0x7F, 0x7F, 0x6B, 0x63, 0x00],
    [0x00, 0x00, 0x1F, 0x33, 0x33, 0x33, 0x33, 0x00],
    [0x00, 0x00, 0x1E, 0x33, 0x33, 0x33, 0x1E, 0x00],
    [0x00, 0x00, 0x3B, 0x66, 0x66, 0x3E, 0x06, 0x0F],
    [0x00, 0x00, 0x6E, 0x33, 0x33, 0x3E, 0x30, 0x78],
    [0x00, 0x00, 0x3B, 0x6E, 0x66, 0x06, 0x0F, 0x00],
    [0x00, 0x00, 0x3E, 0x03, 0x1E, 0x30, 0x1F, 0x00],
    [0x08, 0x0C, 0x3E, 0x0C, 0x0C, 0x2C, 0x18, 0x00],
    [0x00, 0x00, 0x33, 0x33, 0x33, 0x33, 0x6E, 0x00],
    [0x00, 0x00, 0x33, 0x33, 0x33, 0x1E, 0x0C, 0x00],
    [0x00, 0x00, 0x63, 0x6B, 0x7F, 0x7F, 0x36, 0x00],
    [0x00, 0x00, 0x63, 0x36, 0x1C, 0x36, 0x63, 0x00],
    [0x00, 0x00, 0x33, 0x33, 0x33, 0x3E, 0x30, 0x1F],
    [0x00, 0x00, 0x3F, 0x19, 0x0C, 0x26, 0x3F, 0x00],
    [0x38, 0x0C, 0x0C, 0x07, 0x0C, 0x0C, 0x38, 0x00],
    [0x18, 0x18, 0x18, 0x00, 0x18, 0x18, 0x18, 0x00],
    [0x07, 0x0C, 0x0C, 0x38, 0x0C, 0x0C, 0x07, 0x00],
    [0x6E, 0x3B, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
    [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
];
