//! Port of `runtime/os/bios.c` — BIOS firmware services (INT 10h video, INT 16h
//! keyboard, video-data-area helpers).
//!
//! Sits between the OS/game and the hw device modules: reads/writes the BIOS
//! data area and drives hw/{video,keyboard,timer}. `vga`/`bios_video` are still
//! defined in the (C) video.c, bound here as extern with matching layouts;
//! marshals INT args through the shared `cpu` module. ABI-identical to shims.h.

use crate::cpu::*;
use crate::keyboard::{kbd_bios_peek, kbd_bios_pop};
use core::ffi::{c_char, c_int};

// ---- VGA / BIOS video state (defined in the still-C video.c) ----------------

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

extern "C" {
    static mut vga: VgaState;
    static mut bios_video: BiosVideoState;
    // video.c helpers
    fn apply_video_mode_state(mode: u8);
    fn video_invalidate_palette_cache();
    fn vga_dac_component(value: u8) -> u8;
    fn bios_cga_render_teletype(row: u8, col: u8, ch_val: u8, attr: u8, stored_attr: u8);
    fn is_text_mode(mode: u8) -> c_int;
    fn is_cga_graphics_mode(mode: u8) -> c_int;
    // shims.c internals
    fn memw_raw_read(seg: u16, off: u16) -> u16;
    fn memw_raw_write(seg: u16, off: u16, value: u16);
    fn memw_write_impl(
        seg: u16,
        off: u16,
        value: u16,
        file: *const c_char,
        func: *const c_char,
        line: c_int,
    );
    fn shim_idle_wait();
    fn shim_log_stdout(fmt: *const c_char, ...);
    fn shim_exit_with_message(fmt: *const c_char, ...) -> !;
}

const BIOS_VIDEO_PARAM_SEG: u16 = 0xF000;
const BIOS_VIDEO_PARAM_OFF: u16 = 0x0200;

#[inline(always)]
fn site() -> *const c_char {
    c"bios".as_ptr()
}
#[inline(always)]
fn v() -> *mut VgaState {
    core::ptr::addr_of_mut!(vga)
}
#[inline(always)]
fn bv() -> *mut BiosVideoState {
    core::ptr::addr_of_mut!(bios_video)
}
#[inline(always)]
fn memw_write(seg: u16, off: u16, value: u16) {
    unsafe { memw_write_impl(seg, off, value, site(), site(), 0) }
}
#[inline(always)]
fn log_open(f: *const c_char, fu: *const c_char, l: c_int) {
    unsafe { shim_log(c"bios".as_ptr(), f, fu, l, core::ptr::null()) }
}

// ---- BIOS-data-area video geometry ------------------------------------------

#[no_mangle]
pub extern "C" fn bios_video_columns() -> u16 {
    let cols = unsafe { memw_raw_read(0x40, 0x004A) };
    if cols != 0 {
        cols
    } else {
        80
    }
}

#[no_mangle]
pub extern "C" fn bios_page_stride() -> u16 {
    let mut stride = unsafe { memw_raw_read(0x40, 0x004C) };
    if stride == 0 {
        stride = bios_video_columns().wrapping_mul(25 * 2);
    }
    stride
}

#[no_mangle]
pub extern "C" fn bios_video_rows() -> u16 {
    let cols = bios_video_columns();
    let stride = bios_page_stride();
    let rows = stride / if cols != 0 { cols * 2 } else { 160 };
    if rows != 0 {
        rows
    } else {
        25
    }
}

fn bios_scroll_text_page(page: u8, attr: u8) {
    let cols = bios_video_columns();
    let rows = bios_video_rows();
    let stride = bios_page_stride();
    if rows <= 1 || cols == 0 {
        return;
    }
    let bytes_per_row = cols as usize * 2;
    let base = (page % 8) as u16 * stride;
    unsafe {
        let vram = seg_off(0xB800, base);
        core::ptr::copy(
            vram.add(bytes_per_row),
            vram,
            (rows as usize - 1) * bytes_per_row,
        );
        let last = vram.add((rows as usize - 1) * bytes_per_row);
        for i in 0..cols as usize {
            *last.add(i * 2) = b' ';
            *last.add(i * 2 + 1) = attr;
        }
    }
}

fn bios_store_cursor(page: u8, row: u8, col: u8) {
    let page = page % 8;
    unsafe {
        (*bv()).cursor_row[page as usize] = row;
        (*bv()).cursor_col[page as usize] = col;
        memw_raw_write(
            0x40,
            0x0050 + page as u16 * 2,
            ((row as u16) << 8) | col as u16,
        );
    }
}

fn bios_cursor_offset(page: u8) -> u32 {
    let cols = bios_video_columns() as u32;
    let stride = bios_page_stride() as u32;
    unsafe {
        let row = (*bv()).cursor_row[page as usize] as u32;
        let col = (*bv()).cursor_col[page as usize] as u32;
        page as u32 * stride + (row * cols + col) * 2
    }
}

// ---- INT 10h services -------------------------------------------------------

#[no_mangle]
pub extern "C" fn bios_set_video_mode(mode: u8) {
    bios_set_video_mode_impl(
        mode,
        c"<external>".as_ptr(),
        c"bios_set_video_mode".as_ptr(),
        0,
    );
}

#[no_mangle]
pub extern "C" fn bios_set_palette_impl(file: *const c_char, func: *const c_char, line: c_int) {
    log_open(file, func, line);
    unsafe {
        shim_log_stdout(
            c"Trace: bios_set_palette: AL=0x%02X\n".as_ptr(),
            al() as c_int,
        );
        let vs = &mut *v();
        match al() {
            0x00 => vs.attr_palette[(bl() & 0x0F) as usize] = bh(),
            0x01 => vs.border_color = bh(),
            0x02 => {
                let src = seg_off(es(), dx());
                for i in 0..16 {
                    vs.attr_palette[i] = *src.add(i);
                }
            }
            0x03 => vs.blink_mode = bh() & 1,
            0x07 => set_bh(vs.attr_palette[(bl() & 0x0F) as usize]),
            0x08 => set_bh(vs.border_color),
            0x09 => {
                let dst = seg_off(es(), dx());
                for i in 0..16 {
                    *dst.add(i) = vs.attr_palette[i];
                }
                *dst.add(16) = vs.border_color;
            }
            0x10 => {
                let idx = (bx() & 0xFF) as usize;
                vs.palette[idx * 3] = vga_dac_component(dh());
                vs.palette[idx * 3 + 1] = vga_dac_component(ch());
                vs.palette[idx * 3 + 2] = vga_dac_component(cl());
            }
            0x12 => {
                let src = seg_off(es(), dx());
                for i in 0..cx() {
                    let idx = ((bx() + i) & 0xFF) as usize;
                    vs.palette[idx * 3] = vga_dac_component(*src.add(i as usize * 3));
                    vs.palette[idx * 3 + 1] = vga_dac_component(*src.add(i as usize * 3 + 1));
                    vs.palette[idx * 3 + 2] = vga_dac_component(*src.add(i as usize * 3 + 2));
                }
            }
            0x13 => {
                if bl() == 0x00 {
                    vs.dac_paging_mode = bh();
                } else if bl() == 0x01 {
                    vs.dac_current_page = bh();
                }
            }
            0x15 => {
                let idx = bl() as usize;
                set_dh(vs.palette[idx * 3]);
                set_ch(vs.palette[idx * 3 + 1]);
                set_cl(vs.palette[idx * 3 + 2]);
            }
            0x17 => {
                let dst = seg_off(es(), dx());
                for i in 0..cx() {
                    let idx = ((bx() + i) & 0xFF) as usize;
                    *dst.add(i as usize * 3) = vs.palette[idx * 3];
                    *dst.add(i as usize * 3 + 1) = vs.palette[idx * 3 + 1];
                    *dst.add(i as usize * 3 + 2) = vs.palette[idx * 3 + 2];
                }
            }
            0x18 => vs.palette_mask = bl(),
            0x19 => set_bl(vs.palette_mask),
            0x1A => {
                set_bl(vs.dac_paging_mode);
                set_bh(vs.dac_current_page);
            }
            0x1B => {
                for i in 0..cx() {
                    let idx = ((bx() + i) & 0xFF) as usize;
                    let r = vs.palette[idx * 3] as u32;
                    let g = vs.palette[idx * 3 + 1] as u32;
                    let b = vs.palette[idx * 3 + 2] as u32;
                    let gray = ((r * 30 + g * 59 + b * 11) / 100) as u8;
                    vs.palette[idx * 3] = gray;
                    vs.palette[idx * 3 + 1] = gray;
                    vs.palette[idx * 3 + 2] = gray;
                }
            }
            _ => {}
        }
    }
    set_CF(0);
}

#[no_mangle]
pub extern "C" fn bios_set_palette() {
    bios_set_palette_impl(c"<external>".as_ptr(), c"bios_set_palette".as_ptr(), 0);
}

#[no_mangle]
pub extern "C" fn bios_set_cursor_position_impl(
    page: u8,
    mut row: u8,
    mut col: u8,
    file: *const c_char,
    func: *const c_char,
    line: c_int,
) {
    log_open(file, func, line);
    let cols = bios_video_columns();
    let rows = bios_video_rows();
    if cols != 0 {
        col %= cols as u8;
    }
    if rows != 0 {
        row %= rows as u8;
    }
    unsafe {
        (*bv()).active_page = page;
    }
    bios_store_cursor(page, row, col);
    set_CF(0);
}

#[no_mangle]
pub extern "C" fn bios_set_cursor_position(page: u8, row: u8, col: u8) {
    bios_set_cursor_position_impl(
        page,
        row,
        col,
        c"<external>".as_ptr(),
        c"bios_set_cursor_position".as_ptr(),
        0,
    );
}

#[no_mangle]
pub extern "C" fn bios_read_char_attr() -> u16 {
    unsafe {
        let page = (*bv()).active_page % 8;
        let cols = bios_video_columns();
        let stride = bios_page_stride();
        let row = (*bv()).cursor_row[page as usize] as u16;
        let col = (*bv()).cursor_col[page as usize] as u16;
        let off = page as u16 * stride + (row * cols + col) * 2;
        let vram = seg_off(0xB800, off);
        ((*vram.add(1) as u16) << 8) | *vram as u16
    }
}

#[no_mangle]
pub extern "C" fn bios_write_char_attr(glyph: u8, page: u8, attr: u8, count: u16) {
    let page = page % 8;
    let base = bios_cursor_offset(page);
    unsafe {
        for i in 0..count {
            let cell = seg_off(0xB800, (base + i as u32 * 2) as u16);
            *cell = glyph;
            *cell.add(1) = attr;
        }
    }
}

#[no_mangle]
pub extern "C" fn bios_write_char_only(glyph: u8, page: u8, count: u16) {
    let page = page % 8;
    let base = bios_cursor_offset(page);
    unsafe {
        for i in 0..count {
            let cell = seg_off(0xB800, (base + i as u32 * 2) as u16);
            *cell = glyph;
        }
    }
}

#[no_mangle]
pub extern "C" fn bios_get_cursor(page: u8) {
    let page = page % 8;
    unsafe {
        set_dx(
            ((*bv()).cursor_row[page as usize] as u16) << 8
                | (*bv()).cursor_col[page as usize] as u16,
        );
    }
    set_cx(0x0607);
}

#[no_mangle]
pub extern "C" fn bios_scroll_window(
    lines: u8,
    attr: u8,
    top: u8,
    left: u8,
    mut bottom: u8,
    mut right: u8,
    down: c_int,
) {
    let cols = bios_video_columns();
    let rows = bios_video_rows();
    if cols == 0 || rows == 0 {
        return;
    }
    if right as u16 >= cols {
        right = (cols - 1) as u8;
    }
    if bottom as u16 >= rows {
        bottom = (rows - 1) as u8;
    }
    if top > bottom || left > right {
        return;
    }
    let height = (bottom - top + 1) as u16;
    let cell = |r: u16, c: u16| -> *mut u8 { seg_off(0xB800, (r * cols + c) * 2) };
    unsafe {
        if lines == 0 || lines as u16 >= height {
            for r in top as u16..=bottom as u16 {
                for c in left as u16..=right as u16 {
                    let p = cell(r, c);
                    *p = 0x20;
                    *p.add(1) = attr;
                }
            }
            return;
        }
        if down == 0 {
            let mut r = top as u16;
            while r + lines as u16 <= bottom as u16 {
                for c in left as u16..=right as u16 {
                    let dst = cell(r, c);
                    let src = cell(r + lines as u16, c);
                    *dst = *src;
                    *dst.add(1) = *src.add(1);
                }
                r += 1;
            }
            for r in (bottom - lines + 1) as u16..=bottom as u16 {
                for c in left as u16..=right as u16 {
                    let p = cell(r, c);
                    *p = 0x20;
                    *p.add(1) = attr;
                }
            }
        } else {
            let mut r = bottom as u16;
            while r >= (top + lines) as u16 {
                for c in left as u16..=right as u16 {
                    let dst = cell(r, c);
                    let src = cell(r - lines as u16, c);
                    *dst = *src;
                    *dst.add(1) = *src.add(1);
                }
                if r == 0 {
                    break;
                }
                r -= 1;
            }
            for r in top as u16..(top + lines) as u16 {
                for c in left as u16..=right as u16 {
                    let p = cell(r, c);
                    *p = 0x20;
                    *p.add(1) = attr;
                }
            }
        }
    }
}

#[no_mangle]
pub extern "C" fn bios_set_cga_palette_impl(
    bh_val: u8,
    bl_val: u8,
    file: *const c_char,
    func: *const c_char,
    line: c_int,
) {
    log_open(file, func, line);
    unsafe {
        shim_log_stdout(
            c"Trace: bios_set_cga_palette: BH=0x%02X BL=0x%02X\n".as_ptr(),
            bh_val as c_int,
            bl_val as c_int,
        );
        *seg_off(0x40, 0x0066) = bl_val;
        if bh_val == 0x00 {
            (*bv()).cga_border_color = bl_val & 0x0F;
            (*v()).border_color = (*bv()).cga_border_color;
        } else if bh_val == 0x01 {
            (*bv()).cga_palette_select = bl_val & 0x07;
        }
        video_invalidate_palette_cache();
    }
    set_CF(0);
}

#[no_mangle]
pub extern "C" fn bios_set_cga_palette(bh_val: u8, bl_val: u8) {
    bios_set_cga_palette_impl(
        bh_val,
        bl_val,
        c"<external>".as_ptr(),
        c"bios_set_cga_palette".as_ptr(),
        0,
    );
}

#[no_mangle]
pub extern "C" fn bios_teletype_output_impl(
    ch_val: u8,
    page: u8,
    attr: u8,
    file: *const c_char,
    func: *const c_char,
    line: c_int,
) {
    log_open(file, func, line);
    let page = page % 8;
    unsafe {
        (*bv()).active_page = page;
    }
    let cols = bios_video_columns();
    let rows = bios_video_rows();
    let stride = bios_page_stride();
    let text_mode = unsafe { is_text_mode((*bv()).video_mode) } != 0;
    let cga_mode = unsafe { is_cga_graphics_mode((*bv()).video_mode) } != 0;
    let mut row = unsafe { (*bv()).cursor_row[page as usize] };
    let mut col = unsafe { (*bv()).cursor_col[page as usize] };
    let current_attr = unsafe { (*bv()).cursor_attr[page as usize] };

    match ch_val {
        b'\r' => col = 0,
        b'\n' => {
            if rows != 0 {
                row += 1;
            }
        }
        0x08 => {
            if col > 0 {
                col -= 1;
            } else if row > 0 && cols != 0 {
                row -= 1;
                col = (cols - 1) as u8;
            }
            if cols != 0 {
                if text_mode {
                    let cell = row as u32 * cols as u32 + col as u32;
                    let off = (page as u32 * stride as u32 + cell * 2) as u16;
                    let blank = b' ' as u16 | ((current_attr as u16) << 8);
                    memw_write(0xB800, off, blank);
                } else if cga_mode {
                    unsafe {
                        bios_cga_render_teletype(row, col, b' ', current_attr & 0x0F, current_attr)
                    };
                }
            }
        }
        _ => {
            if text_mode {
                unsafe {
                    (*bv()).cursor_attr[page as usize] = attr;
                }
                if rows != 0 && row as u16 >= rows {
                    bios_scroll_text_page(page, attr);
                    row = if rows > 0 { (rows - 1) as u8 } else { 0 };
                }
                if cols != 0 {
                    let cell = row as u32 * cols as u32 + col as u32;
                    let off = (page as u32 * stride as u32 + cell * 2) as u16;
                    memw_write(0xB800, off, ch_val as u16 | ((attr as u16) << 8));
                    col += 1;
                    if col as u16 >= cols {
                        col = 0;
                        if rows != 0 {
                            row += 1;
                        }
                    }
                }
            } else if cga_mode {
                let combined_attr = (current_attr & 0xF0) | (attr & 0x0F);
                unsafe {
                    (*bv()).cursor_attr[page as usize] = combined_attr;
                    bios_cga_render_teletype(row, col, ch_val, attr, combined_attr);
                }
                if cols != 0 {
                    col += 1;
                    if col as u16 >= cols {
                        col = 0;
                        if rows != 0 {
                            row += 1;
                        }
                    }
                }
            }
        }
    }

    if rows != 0 && row as u16 >= rows {
        if text_mode {
            bios_scroll_text_page(page, unsafe { (*bv()).cursor_attr[page as usize] });
        }
        row = if rows > 0 { (rows - 1) as u8 } else { 0 };
    }

    bios_store_cursor(page, row, col);
    set_al(ch_val);
    set_CF(0);
}

#[no_mangle]
pub extern "C" fn bios_teletype_output(ch_val: u8, page: u8, attr: u8) {
    bios_teletype_output_impl(
        ch_val,
        page,
        attr,
        c"<external>".as_ptr(),
        c"bios_teletype_output".as_ptr(),
        0,
    );
}

#[no_mangle]
pub extern "C" fn bios_keyboard_impl(file: *const c_char, func: *const c_char, line: c_int) {
    log_open(file, func, line);
    match ah() {
        0x00 | 0x10 => {
            let mut ascii: u8 = 0;
            let mut scancode: u8 = 0;
            while kbd_bios_pop(&mut ascii, &mut scancode) == 0 {
                // Blocking wait: no guest instructions retire, so let machine
                // time flow at host pace (music, TAP deadlines) while we wait.
                unsafe { shim_idle_wait() };
            }
            set_ax(ascii as u16 | ((scancode as u16) << 8));
            set_ZF(0);
        }
        0x01 | 0x11 => {
            let mut ascii: u8 = 0;
            let mut scancode: u8 = 0;
            if kbd_bios_peek(&mut ascii, &mut scancode) != 0 {
                set_ax(ascii as u16 | ((scancode as u16) << 8));
                set_ZF(0);
            } else {
                set_ax(0);
                set_ZF(1);
            }
        }
        0x02 => set_al(unsafe { *seg_off(0x40, 0x17) }),
        0x12 => {
            set_al(unsafe { *seg_off(0x40, 0x17) });
            set_ah(unsafe { *seg_off(0x40, 0x18) });
        }
        0x03 => {}
        other => unsafe {
            shim_exit_with_message(
                c"Error: unimplemented BIOS keyboard AH=0x%02X (%s:%s:%d)\n".as_ptr(),
                other as c_int,
                file,
                func,
                line,
            );
        },
    }
}

#[no_mangle]
pub extern "C" fn bios_keyboard() {
    bios_keyboard_impl(c"<external>".as_ptr(), c"bios_keyboard".as_ptr(), 0);
}

/// Where pixel (x, y) lives in CGA graphics memory, and how far to shift a pixel
/// down within its byte. The CGA stores even rows at B800:0000 and odd rows at
/// B800:2000, 80 bytes to a row either way — four 2-bit pixels per byte in modes
/// 4/5, eight 1-bit pixels per byte in mode 6, leftmost pixel in the high bits.
/// Returns None for a coordinate off the screen, which the BIOS simply drops.
fn cga_pixel_site(mode: u8, x: u16, y: u16) -> Option<(u16, u32, u8)> {
    const BYTES_PER_ROW: u32 = 80;
    let width: u32 = if mode == 0x06 { 640 } else { 320 };
    let (x, y) = (x as u32, y as u32);
    if x >= width || y >= 200 {
        return None;
    }
    let row_base = (y & 1) * 0x2000 + (y >> 1) * BYTES_PER_ROW;
    let (byte, shift, mask) = if mode == 0x06 {
        (x >> 3, 7 - (x & 7), 0x01u8)
    } else {
        (x >> 2, (3 - (x & 3)) * 2, 0x03u8)
    };
    Some((((row_base + byte) & 0x3FFF) as u16, shift, mask))
}

/// INT 10h AH=0Ch — write a graphics pixel. AL = colour, and its bit 7 means XOR
/// the colour into the pixel already there instead of replacing it (which is how
/// a game draws and erases a sprite with the same call). CX = column, DX = row.
/// The caller gates on a CGA graphics mode.
#[no_mangle]
pub extern "C" fn bios_write_pixel(color: u8, x: u16, y: u16) {
    let mode = unsafe { (*bv()).video_mode };
    if let Some((off, shift, mask)) = cga_pixel_site(mode, x, y) {
        unsafe {
            let p = seg_off(0xB800, off);
            let bits = (color & mask) << shift;
            if color & 0x80 != 0 {
                *p ^= bits;
            } else {
                *p = (*p & !(mask << shift)) | bits;
            }
        }
    }
}

/// INT 10h AH=0Dh — read a graphics pixel back into AL.
#[no_mangle]
pub extern "C" fn bios_read_pixel(x: u16, y: u16) {
    let mode = unsafe { (*bv()).video_mode };
    let color = match cga_pixel_site(mode, x, y) {
        Some((off, shift, mask)) => unsafe { (*seg_off(0xB800, off) >> shift) & mask },
        None => 0,
    };
    set_al(color);
}

#[no_mangle]
pub extern "C" fn bios_current_video_mode() -> u8 {
    unsafe { (*bv()).video_mode }
}
#[no_mangle]
pub extern "C" fn bios_current_video_columns() -> u8 {
    bios_video_columns() as u8
}
#[no_mangle]
pub extern "C" fn bios_current_active_page() -> u8 {
    unsafe { (*bv()).active_page }
}
#[no_mangle]
pub extern "C" fn bios_display_combination_code() -> u8 {
    0x07
}
#[no_mangle]
pub extern "C" fn bios_display_combination_alt_code() -> u8 {
    0x00
}

#[no_mangle]
pub extern "C" fn bios_get_video_parameter_block(mode: u8, seg: *mut u16, off: *mut u16) {
    if seg.is_null() || off.is_null() {
        return;
    }
    unsafe {
        if mode == 6 {
            *seg = BIOS_VIDEO_PARAM_SEG;
            *off = BIOS_VIDEO_PARAM_OFF;
        } else {
            *seg = 0;
            *off = 0;
        }
    }
}

#[no_mangle]
pub extern "C" fn bios_set_video_mode_impl(
    mode: u8,
    file: *const c_char,
    func: *const c_char,
    line: c_int,
) {
    log_open(file, func, line);
    unsafe {
        shim_log_stdout(
            c"Trace: bios_set_video_mode: mode=0x%02X (no real mode switch)\n".as_ptr(),
            mode as c_int,
        );
        apply_video_mode_state(mode);
    }
    set_al(mode);
    set_ah(0);
    set_CF(0);
}

#[no_mangle]
pub extern "C" fn bios_video_alt_select_impl(
    file: *const c_char,
    func: *const c_char,
    line: c_int,
) {
    log_open(file, func, line);
    if bl() == 0x10 {
        set_bh(0x00);
        set_bl(0x03);
        set_cx(0x0000);
        return;
    }
    if cx() != 0 {
        unsafe {
            let src = seg_off(es(), dx());
            let vs = &mut *v();
            for i in 0..cx() {
                let idx = ((bx() + i) & 0xFF) as usize;
                vs.palette[idx * 3] = vga_dac_component(*src.add(i as usize * 3));
                vs.palette[idx * 3 + 1] = vga_dac_component(*src.add(i as usize * 3 + 1));
                vs.palette[idx * 3 + 2] = vga_dac_component(*src.add(i as usize * 3 + 2));
            }
        }
    }
}
