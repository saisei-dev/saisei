/* ============================ os/bios ============================
 * BIOS firmware services: the INT 10h video, INT 16h keyboard and the
 * video-data-area helpers the game and the ISR dispatch in shims.c call
 * into. This is the firmware layer that sits between the OS/game and the
 * hardware device modules -- it reads/writes the BIOS data area and drives
 * the hw/{video,keyboard,timer} layers (apply_video_mode_state, the kbd
 * queue, bios_timer_increment) rather than touching device internals.
 *
 * Declared in runtime_abi.h / shims.h (generated game code + the int*_impl
 * ISR handlers in shims.c call these); SHIMS_DISABLE_MACROS makes the *_impl
 * wrappers compile as functions, same as os/dos.c.
 */
#define SHIMS_DISABLE_MACROS
#include "shims.h"
#include <string.h>

uint16_t bios_video_columns(void) {
  uint16_t cols = memw_raw_read(0x40, 0x004A);
  return cols ? cols : 80;
}

uint16_t bios_page_stride(void) {
  uint16_t stride = memw_raw_read(0x40, 0x004C);
  if (!stride) {
    stride = (uint16_t)(bios_video_columns() * 25 * 2);
  }
  return stride;
}

uint16_t bios_video_rows(void) {
  uint16_t cols = bios_video_columns();
  uint16_t stride = bios_page_stride();
  uint16_t rows = (uint16_t)(stride / (cols ? (cols * 2) : 160));
  return rows ? rows : 25;
}

static void bios_scroll_text_page(uint8_t page, uint8_t attr) {
  uint16_t cols = bios_video_columns();
  uint16_t rows = bios_video_rows();
  uint16_t stride = bios_page_stride();
  if (rows <= 1 || cols == 0) {
    return;
  }
  size_t bytes_per_row = (size_t)cols * 2;
  uint16_t base = (uint16_t)(page % 8) * stride;
  uint8_t *vram = seg_off(0xB800, base);
  memmove(vram, vram + bytes_per_row, (rows - 1) * bytes_per_row);
  uint8_t *last = vram + (rows - 1) * bytes_per_row;
  for (uint16_t i = 0; i < cols; ++i) {
    last[i * 2] = ' ';
    last[i * 2 + 1] = attr;
  }
}

static void bios_store_cursor(uint8_t page, uint8_t row, uint8_t col) {
  page %= 8;
  bios_video.cursor_row[page] = row;
  bios_video.cursor_col[page] = col;
  memw_raw_write(0x40, (uint16_t)(0x0050 + page * 2),
                 (uint16_t)((row << 8) | col));
}

void bios_set_video_mode(uint8_t mode) {
  bios_set_video_mode_impl(mode, "<external>", __func__, 0);
}

/*
 * BIOS palette services (INT 0x10, AH=0x10).
 * Implements a subset of EGA/VGA palette and DAC functions.  The subcommand is
 * selected by ``al`` and parameters are supplied via global registers.
 */
void bios_set_palette_impl(const char *file, const char *func, int line) {
  shim_log(__func__, file, func, line, NULL);
  shim_log_stdout("Trace: bios_set_palette: AL=0x%02X\n", al);
  switch (al) {
  case 0x00: /* Set single palette register */
    vga.attr_palette[bl & 0x0F] = bh;
    break;
  case 0x01: /* Set border color */
    vga.border_color = bh;
    break;
  case 0x02: /* Set all palette registers */ {
    uint8_t *src = seg_off(es, dx);
    for (int i = 0; i < 16; ++i)
      vga.attr_palette[i] = src[i];
    break;
  }
  case 0x03: /* Toggle intensity/blinking bit */
    vga.blink_mode = bh & 1; /* 0=intensity,1=blinking */
    break;
  case 0x07: /* Get individual palette register */
    bh = vga.attr_palette[bl & 0x0F];
    break;
  case 0x08: /* Read overscan register */
    bh = vga.border_color;
    break;
  case 0x09: /* Read all palette registers and overscan */ {
    uint8_t *dst = seg_off(es, dx);
    for (int i = 0; i < 16; ++i)
      dst[i] = vga.attr_palette[i];
    dst[16] = vga.border_color;
    break;
  }
  case 0x10: /* Set individual DAC register */ {
    /* The VGA PEL address is an 8-bit register that wraps mod 256; mask so a
     * caller passing a >255 index can't index past vga.palette[768]. */
    uint16_t idx = bx & 0xFF;
    vga.palette[idx * 3 + 0] = vga_dac_component(dh);
    vga.palette[idx * 3 + 1] = vga_dac_component(ch);
    vga.palette[idx * 3 + 2] = vga_dac_component(cl);
    break;
  }
  case 0x12: /* Set block of DAC registers */ {
    uint8_t *src = seg_off(es, dx);
    for (uint16_t i = 0; i < cx; ++i) {
      uint16_t idx = (uint16_t)((bx + i) & 0xFF); /* 8-bit PEL index wrap */
      vga.palette[idx * 3 + 0] = vga_dac_component(src[i * 3 + 0]);
      vga.palette[idx * 3 + 1] = vga_dac_component(src[i * 3 + 1]);
      vga.palette[idx * 3 + 2] = vga_dac_component(src[i * 3 + 2]);
    }
    break;
  }
  case 0x13: /* Select video DAC color page */
    if (bl == 0x00) {
      vga.dac_paging_mode = bh;
    } else if (bl == 0x01) {
      vga.dac_current_page = bh;
    }
    break;
  case 0x15: /* Read individual DAC register */ {
    uint16_t idx = bl;
    dh = vga.palette[idx * 3 + 0];
    ch = vga.palette[idx * 3 + 1];
    cl = vga.palette[idx * 3 + 2];
    break;
  }
  case 0x17: /* Read block of DAC registers */ {
    uint8_t *dst = seg_off(es, dx);
    for (uint16_t i = 0; i < cx; ++i) {
      uint16_t idx = (uint16_t)((bx + i) & 0xFF); /* 8-bit PEL index wrap */
      dst[i * 3 + 0] = vga.palette[idx * 3 + 0];
      dst[i * 3 + 1] = vga.palette[idx * 3 + 1];
      dst[i * 3 + 2] = vga.palette[idx * 3 + 2];
    }
    break;
  }
  case 0x18: /* Set pel mask */
    vga.palette_mask = bl;
    break;
  case 0x19: /* Read pel mask */
    bl = vga.palette_mask;
    break;
  case 0x1A: /* Get video DAC color-page state */
    bl = vga.dac_paging_mode;
    bh = vga.dac_current_page;
    break;
  case 0x1B: /* Perform gray-scale summing */ {
    for (uint16_t i = 0; i < cx; ++i) {
      uint16_t idx = (uint16_t)((bx + i) & 0xFF); /* 8-bit PEL index wrap */
      uint8_t r = vga.palette[idx * 3 + 0];
      uint8_t g = vga.palette[idx * 3 + 1];
      uint8_t b = vga.palette[idx * 3 + 2];
      uint8_t gray = (uint8_t)((r * 30 + g * 59 + b * 11) / 100);
      vga.palette[idx * 3 + 0] = gray;
      vga.palette[idx * 3 + 1] = gray;
      vga.palette[idx * 3 + 2] = gray;
    }
    break;
  }
  default:
    /* Unhandled subcommand */
    break;
  }
  CF = 0;
}

void bios_set_palette(void) {
  bios_set_palette_impl("<external>", __func__, 0);
}

void bios_set_cursor_position_impl(uint8_t page, uint8_t row, uint8_t col,
                                   const char *file, const char *func,
                                   int line) {
  shim_log(__func__, file, func, line, NULL);
  uint16_t cols = bios_video_columns();
  uint16_t rows = bios_video_rows();
  if (cols) {
    col = (uint8_t)(col % cols);
  }
  if (rows) {
    row = (uint8_t)(row % rows);
  }
  bios_video.active_page = page;
  bios_store_cursor(page, row, col);
  CF = 0;
}

void bios_set_cursor_position(uint8_t page, uint8_t row, uint8_t col) {
  bios_set_cursor_position_impl(page, row, col, "<external>", __func__, 0);
}

/* INT 10h AH=08h: read the character + attribute at the cursor on the active
 * page from text video memory. Returns (attr << 8) | char (AH=attr, AL=char). */
uint16_t bios_read_char_attr(void) {
  uint8_t page = bios_video.active_page % 8;
  uint16_t cols = bios_video_columns();
  uint16_t stride = bios_page_stride();
  uint8_t row = bios_video.cursor_row[page];
  uint8_t col = bios_video.cursor_col[page];
  uint16_t off =
      (uint16_t)(page * stride + ((uint16_t)row * cols + col) * 2);
  const uint8_t *vram = seg_off(0xB800, off);
  return (uint16_t)((vram[1] << 8) | vram[0]);
}

/* Base text-video offset of the cursor on a page. */
static uint32_t bios_cursor_offset(uint8_t page) {
  uint16_t cols = bios_video_columns();
  uint16_t stride = bios_page_stride();
  uint8_t row = bios_video.cursor_row[page];
  uint8_t col = bios_video.cursor_col[page];
  return (uint32_t)page * stride + ((uint32_t)row * cols + col) * 2;
}

/* INT 10h AH=09h: write char+attr to CX cells starting at the cursor (the
 * cursor is not advanced). */
void bios_write_char_attr(uint8_t glyph, uint8_t page, uint8_t attr,
                          uint16_t count) {
  page %= 8;
  uint32_t base = bios_cursor_offset(page);
  for (uint16_t i = 0; i < count; ++i) {
    uint8_t *cell = seg_off(0xB800, (uint16_t)(base + (uint32_t)i * 2));
    cell[0] = glyph;
    cell[1] = attr;
  }
}

/* INT 10h AH=0Ah: write char to CX cells at the cursor, keeping each cell's
 * existing attribute (cursor not advanced). */
void bios_write_char_only(uint8_t glyph, uint8_t page, uint16_t count) {
  page %= 8;
  uint32_t base = bios_cursor_offset(page);
  for (uint16_t i = 0; i < count; ++i) {
    uint8_t *cell = seg_off(0xB800, (uint16_t)(base + (uint32_t)i * 2));
    cell[0] = glyph;
  }
}

/* INT 10h AH=03h: get cursor position (DH=row, DL=col) and shape (CX) for a
 * page. */
void bios_get_cursor(uint8_t page) {
  page %= 8;
  dx = (uint16_t)((bios_video.cursor_row[page] << 8) |
                  bios_video.cursor_col[page]);
  cx = 0x0607; /* conventional text cursor scanline shape */
}

/* INT 10h AH=06h (up) / AH=07h (down): scroll a text window. `lines`==0 (or >=
 * window height) clears the whole window. Vacated rows are filled with spaces of
 * attribute `attr`. Window corners are CH/CL (top/left) and DH/DL (bottom/right);
 * page 0 text buffer at B800:0. */
void bios_scroll_window(uint8_t lines, uint8_t attr, uint8_t top, uint8_t left,
                        uint8_t bottom, uint8_t right, int down) {
  uint16_t cols = bios_video_columns();
  uint16_t rows = bios_video_rows();
  if (cols == 0 || rows == 0) return;
  if (right >= cols) right = (uint8_t)(cols - 1);
  if (bottom >= rows) bottom = (uint8_t)(rows - 1);
  if (top > bottom || left > right) return;
  uint16_t height = (uint16_t)(bottom - top + 1);
#define SCROLL_CELL(r, c) seg_off(0xB800, (uint16_t)(((r) * cols + (c)) * 2))
  if (lines == 0 || lines >= height) {
    for (uint16_t r = top; r <= bottom; ++r)
      for (uint16_t c = left; c <= right; ++c) {
        uint8_t *cell = SCROLL_CELL(r, c);
        cell[0] = 0x20;
        cell[1] = attr;
      }
    return;
  }
  if (!down) { /* scroll up: row r <- row r+lines, blank the bottom `lines` */
    for (uint16_t r = top; (uint16_t)(r + lines) <= bottom; ++r)
      for (uint16_t c = left; c <= right; ++c) {
        uint8_t *dst = SCROLL_CELL(r, c);
        uint8_t *src = SCROLL_CELL((uint16_t)(r + lines), c);
        dst[0] = src[0];
        dst[1] = src[1];
      }
    for (uint16_t r = (uint16_t)(bottom - lines + 1); r <= bottom; ++r)
      for (uint16_t c = left; c <= right; ++c) {
        uint8_t *cell = SCROLL_CELL(r, c);
        cell[0] = 0x20;
        cell[1] = attr;
      }
  } else { /* scroll down: row r <- row r-lines, blank the top `lines` */
    for (uint16_t r = bottom; r >= (uint16_t)(top + lines); --r)
      for (uint16_t c = left; c <= right; ++c) {
        uint8_t *dst = SCROLL_CELL(r, c);
        uint8_t *src = SCROLL_CELL((uint16_t)(r - lines), c);
        dst[0] = src[0];
        dst[1] = src[1];
      }
    for (uint16_t r = top; r < (uint16_t)(top + lines); ++r)
      for (uint16_t c = left; c <= right; ++c) {
        uint8_t *cell = SCROLL_CELL(r, c);
        cell[0] = 0x20;
        cell[1] = attr;
      }
  }
#undef SCROLL_CELL
}

void bios_set_cga_palette_impl(uint8_t bh_val, uint8_t bl_val,
                               const char *file, const char *func, int line) {
  shim_log(__func__, file, func, line, NULL);
  shim_log_stdout("Trace: bios_set_cga_palette: BH=0x%02X BL=0x%02X\n", bh_val,
                  bl_val);
  memb_raw(0x40, 0x0066) = bl_val;
  if (bh_val == 0x00) {
    bios_video.cga_border_color = bl_val & 0x0F;
    vga.border_color = bios_video.cga_border_color;
  } else if (bh_val == 0x01) {
    bios_video.cga_palette_select = bl_val & 0x07;
  }
  video_invalidate_palette_cache();
  CF = 0;
}

void bios_set_cga_palette(uint8_t bh_val, uint8_t bl_val) {
  bios_set_cga_palette_impl(bh_val, bl_val, "<external>", __func__, 0);
}


void bios_teletype_output_impl(uint8_t ch_val, uint8_t page, uint8_t attr,
                               const char *file, const char *func, int line) {
  shim_log(__func__, file, func, line, NULL);
  page %= 8;
  bios_video.active_page = page;
  uint16_t cols = bios_video_columns();
  uint16_t rows = bios_video_rows();
  uint16_t stride = bios_page_stride();
  int text_mode = is_text_mode(bios_video.video_mode);
  int cga_mode = is_cga_graphics_mode(bios_video.video_mode);
  uint8_t row = bios_video.cursor_row[page];
  uint8_t col = bios_video.cursor_col[page];
  uint8_t current_attr = bios_video.cursor_attr[page];

  switch (ch_val) {
  case '\r':
    col = 0;
    break;
  case '\n':
    if (rows) {
      ++row;
    }
    break;
  case '\b':
    if (col > 0) {
      --col;
    } else if (row > 0 && cols) {
      --row;
      col = (uint8_t)(cols - 1);
    }
    if (cols) {
      if (text_mode) {
        uint32_t cell = (uint32_t)row * cols + col;
        uint16_t off = (uint16_t)(page * stride + (cell * 2));
        uint16_t blank = (uint16_t)(' ' | ((uint16_t)current_attr << 8));
        memw_write(0xB800, off, blank);
      } else if (cga_mode) {
        bios_cga_render_teletype(row, col, ' ',
                                 (uint8_t)(current_attr & 0x0F), current_attr);
      }
    }
    break;
  default: {
    if (text_mode) {
      bios_video.cursor_attr[page] = attr;
      if (rows && row >= rows) {
        bios_scroll_text_page(page, attr);
        row = (uint8_t)((rows > 0) ? (rows - 1) : 0);
      }
      if (cols) {
        uint32_t cell = (uint32_t)row * cols + col;
        uint16_t off = (uint16_t)(page * stride + (cell * 2));
        memw_write(0xB800, off,
                   (uint16_t)(ch_val | ((uint16_t)attr << 8)));
        ++col;
        if (col >= cols) {
          col = 0;
          if (rows) {
            ++row;
          }
        }
      }
    } else if (cga_mode) {
      uint8_t combined_attr =
          (uint8_t)((current_attr & 0xF0u) | (attr & 0x0Fu));
      bios_video.cursor_attr[page] = combined_attr;
      bios_cga_render_teletype(row, col, ch_val, attr, combined_attr);
      if (cols) {
        ++col;
        if (col >= cols) {
          col = 0;
          if (rows) {
            ++row;
          }
        }
      }
    }
    break;
  }
  }

  if (rows && row >= rows) {
    if (text_mode) {
      bios_scroll_text_page(page, bios_video.cursor_attr[page]);
    }
    row = (uint8_t)((rows > 0) ? (rows - 1) : 0);
  }

  bios_store_cursor(page, row, col);
  al = ch_val;
  CF = 0;
}

void bios_teletype_output(uint8_t ch_val, uint8_t page, uint8_t attr) {
  bios_teletype_output_impl(ch_val, page, attr, "<external>", __func__, 0);
}

void bios_keyboard_impl(const char *file, const char *func, int line) {
  shim_log(__func__, file, func, line, NULL);
  switch (ah) {
  case 0x00: /* read key (blocking) */
  case 0x10: { /* read key, extended (we have no 101-key extended set, alias) */
    uint8_t ascii = 0;
    uint8_t scancode = 0;
    /*
     * BIOS INT 16h/AH=00h is a blocking read. Returning AX=0 when stdin is
     * temporarily empty makes key handling timing-dependent and allows callers
     * to advance state machines with a fake keypress. Keep polling until a key
     * is available in the BIOS type-ahead buffer. SAFEPOINT() delivers the
     * IRQ1 (INT 09h) that drains port 0x60 and fills that buffer, so this is
     * the faithful "wait for the BIOS handler to enqueue a key" loop.
     */
    while (!kbd_bios_pop(&ascii, &scancode)) {
      SAFEPOINT();
    }
    ax = (uint16_t)(ascii | ((uint16_t)scancode << 8));
    ZF = 0;
    break;
  }
  case 0x01:   /* peek key (non-blocking) */
  case 0x11: { /* peek key, extended (alias) */
    uint8_t ascii, scancode;
    if (kbd_bios_peek(&ascii, &scancode)) {
      ax = (uint16_t)(ascii | ((uint16_t)scancode << 8));
      ZF = 0;
    } else {
      ax = 0;
      ZF = 1;
    }
    break;
  }
  case 0x02: /* get shift-flags byte 1 (BDA 0040:0017) */
    al = memb_raw(0x40, 0x17);
    break;
  case 0x12: /* get extended shift flags: AL=byte1 (40:17), AH=byte2 (40:18) */
    al = memb_raw(0x40, 0x17);
    ah = memb_raw(0x40, 0x18);
    break;
  case 0x03: /* set typematic rate/delay -- no-op (we don't model autorepeat) */
    break;
  default:
    shim_exit_with_message(
        "Error: unimplemented BIOS keyboard AH=0x%02X (%s:%s:%d)\n", ah, file,
        func, line);
  }
}

void bios_keyboard(void) { bios_keyboard_impl("<external>", __func__, 0); }

uint8_t bios_current_video_mode(void) { return bios_video.video_mode; }

uint8_t bios_current_video_columns(void) {
  return (uint8_t)bios_video_columns();
}

uint8_t bios_current_active_page(void) { return bios_video.active_page; }

uint8_t bios_display_combination_code(void) { return 0x07; }

uint8_t bios_display_combination_alt_code(void) { return 0x00; }

void bios_get_video_parameter_block(uint8_t mode, uint16_t *seg, uint16_t *off) {
  if (!seg || !off) {
    return;
  }
  if (mode == 6) {
    *seg = BIOS_VIDEO_PARAM_SEG;
    *off = BIOS_VIDEO_PARAM_OFF;
  } else {
    *seg = 0;
    *off = 0;
  }
}

void bios_set_video_mode_impl(uint8_t mode, const char *file, const char *func,
                              int line) {
  shim_log(__func__, file, func, line, NULL);
  shim_log_stdout(
      "Trace: bios_set_video_mode: mode=0x%02X (no real mode switch)\n", mode);
  apply_video_mode_state(mode);
  al = mode;
  ah = 0;
  CF = 0;
}

/* INT 10h AH=12h -- EGA/VGA "Alternate Function Select". The subfunction is
 * normally chosen by BL, but a program may also drive the color-register set
 * through this entry: some programs' video init issues AX=1210h with
 * ES:DX -> a 16-entry RGB DAC table, BX = first register, CX = count, which is
 * the same effect as the AH=10h/AL=12h "set block of DAC registers" call (the
 * game discards the return). Program the DAC block faithfully from ES:DX, and
 * answer the standard BL=10h "return video configuration" query so callers that
 * probe the adapter get sane VGA values. */
void bios_video_alt_select_impl(const char *file, const char *func, int line) {
  shim_log(__func__, file, func, line, NULL);
  if (bl == 0x10) {
    /* Return EGA/VGA configuration information. */
    bh = 0x00;       /* color mode in effect */
    bl = 0x03;       /* 256 KB of display memory */
    cx = 0x0000;     /* feature connector / switch settings */
    return;
  }
  /* DAC color-register block load (ES:DX = RGB triples, BX = start, CX = n). */
  if (cx) {
    uint8_t *src = seg_off(es, dx);
    for (uint16_t i = 0; i < cx; ++i) {
      uint16_t idx = (uint16_t)((bx + i) & 0xFF); /* 8-bit PEL index wrap */
      vga.palette[idx * 3 + 0] = vga_dac_component(src[i * 3 + 0]);
      vga.palette[idx * 3 + 1] = vga_dac_component(src[i * 3 + 1]);
      vga.palette[idx * 3 + 2] = vga_dac_component(src[i * 3 + 2]);
    }
  }
}
