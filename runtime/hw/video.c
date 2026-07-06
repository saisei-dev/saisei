/* ============================ hw/video ============================
 * Video card emulation (VGA/CGA/BIOS-video) + present pipeline, extracted
 * from shims.c behind the video.h interface. Owns the CGA palette cache,
 * the font, and staging. VGA/CGA register *ports* stay in shims.c's
 * inb/outb (they only touch the extern register structs + vga_dac_component);
 * the derived cache here is invalidated via video_invalidate_cga_palette().
 */
#include "video.h"
#include "shims.h"
#include "virtual_display.h"
#include "stb_image_write.h"
#include <string.h>
#include <stdlib.h>

/* Invalidate the derived CGA palette cache (see video.h). Implemented near
 * the cache + ensure_cga_palette below via a forward declaration. */
VgaState vga = { .palette_mask = 0xFF };
CgaState cga;
BiosVideoState bios_video = { .video_mode = 0x03 };

uint8_t vga_dac_component(uint8_t value) {
  /* VGA DAC channels are 6-bit; writes clamp to 0..63. */
  return (uint8_t)(value & 0x3F);
}

static uint8_t cga_palette[256 * 3];
static uint8_t cga_palette_last_select = 0xFF;
static uint8_t cga_palette_last_border = 0xFF;
static int cga_palette_initialized;

static const uint8_t text_mode_palette_6bit[16][3] = {
    {0x00, 0x00, 0x00}, {0x00, 0x00, 0x2A}, {0x00, 0x2A, 0x00},
    {0x00, 0x2A, 0x2A}, {0x2A, 0x00, 0x00}, {0x2A, 0x00, 0x2A},
    {0x2A, 0x15, 0x00}, {0x2A, 0x2A, 0x2A}, {0x15, 0x15, 0x15},
    {0x15, 0x15, 0x3F}, {0x15, 0x3F, 0x15}, {0x15, 0x3F, 0x3F},
    {0x3F, 0x15, 0x15}, {0x3F, 0x15, 0x3F}, {0x3F, 0x3F, 0x15},
    {0x3F, 0x3F, 0x3F},
};
static uint8_t text_mode_palette[256 * 3];
static int text_mode_palette_initialized;

static void ensure_text_mode_palette(void) {
  if (text_mode_palette_initialized) {
    return;
  }
  memset(text_mode_palette, 0, sizeof(text_mode_palette));
  for (size_t i = 0; i < 16; ++i) {
    text_mode_palette[i * 3 + 0] = text_mode_palette_6bit[i][0];
    text_mode_palette[i * 3 + 1] = text_mode_palette_6bit[i][1];
    text_mode_palette[i * 3 + 2] = text_mode_palette_6bit[i][2];
  }
  text_mode_palette_initialized = 1;
}

#include "font8x8_basic.h"


static void ensure_display_geometry(int width, int height) {
  if (headless_mode) {
    return;
  }
  if (width <= 0 || height <= 0) {
    return;
  }
  if (width == current_display_width && height == current_display_height) {
    return;
  }
  current_display_width = width;
  current_display_height = height;
  virtual_display_configure(width, height);
}


int is_text_mode(uint8_t mode) {
  switch (mode) {
  case 0x00:
  case 0x01:
  case 0x02:
  case 0x03:
  case 0x07:
    return 1;
  default:
    return 0;
  }
}

int is_cga_graphics_mode(uint8_t mode) {
  switch (mode) {
  case 0x04:
  case 0x05:
  case 0x06:
    return 1;
  default:
    return 0;
  }
}

static inline int is_tandy_graphics_mode(uint8_t mode) {
  switch (mode) {
  case 0x08:
  case 0x09:
  case 0x0A:
    return 1;
  default:
    return 0;
  }
}

static inline int is_planar_graphics_mode(uint8_t mode) {
  switch (mode) {
  case 0x0D:
  case 0x0E:
    return 1;
  default:
    return 0;
  }
}

static inline int planar_mode_width(uint8_t mode) {
  switch (mode) {
  case 0x0E:
    return 640;
  case 0x0D:
  default:
    return 320;
  }
}

static inline int planar_mode_height(uint8_t mode) {
  (void)mode;
  return 200;
}

static inline int cga_mode_width(uint8_t mode) {
  return (mode == 0x06) ? 640 : 320;
}

static inline int tandy_mode_width(uint8_t mode) {
  switch (mode) {
  case 0x08:
    return 160;
  case 0x0A:
    return 640;
  case 0x09:
  default:
    return 320;
  }
}

static inline int tandy_mode_height(uint8_t mode) {
  (void)mode;
  return 200;
}

static void ensure_cga_palette(void) {
  if (cga_palette_initialized &&
      cga_palette_last_select == bios_video.cga_palette_select &&
      cga_palette_last_border == bios_video.cga_border_color) {
    return;
  }

  memset(cga_palette, 0, sizeof(cga_palette));

  uint8_t background = (uint8_t)(bios_video.cga_border_color & 0x0F);
  uint8_t indices[4];

  if (bios_video.video_mode == 0x06) {
    /*
     * 640x200 monochrome: background from color register, foreground obeys
     * the intensity bit. Only two palette entries are meaningful; duplicate
     * the foreground into the remaining slots so existing lookup logic stays
     * valid.
     */
    uint8_t foreground = (uint8_t)((bios_video.cga_palette_select & 0x02) ? 0x0F : 0x07);
    indices[0] = background;
    indices[1] = foreground;
    indices[2] = foreground;
    indices[3] = foreground;
  } else {
    uint8_t palette_index = (uint8_t)(bios_video.cga_palette_select & 0x01);
    uint8_t intensity = (uint8_t)((bios_video.cga_palette_select >> 1) & 0x01);

    static const uint8_t palette0[3] = {0x02, 0x04, 0x06};
    static const uint8_t palette0_hi[3] = {0x0A, 0x0C, 0x0E};
    static const uint8_t palette1[3] = {0x03, 0x05, 0x07};
    static const uint8_t palette1_hi[3] = {0x0B, 0x0D, 0x0F};

    const uint8_t *src;
    if (palette_index) {
      src = intensity ? palette1_hi : palette1;
    } else {
      src = intensity ? palette0_hi : palette0;
    }

    indices[0] = background;
    indices[1] = src[0];
    indices[2] = src[1];
    indices[3] = src[2];
  }

  for (size_t i = 0; i < 4; ++i) {
    uint8_t idx = (uint8_t)(indices[i] & 0x0F);
    cga_palette[i * 3 + 0] = text_mode_palette_6bit[idx][0];
    cga_palette[i * 3 + 1] = text_mode_palette_6bit[idx][1];
    cga_palette[i * 3 + 2] = text_mode_palette_6bit[idx][2];
  }

  cga_palette_initialized = 1;
  cga_palette_last_select = bios_video.cga_palette_select;
  cga_palette_last_border = bios_video.cga_border_color;
}

static void stage_and_present_text_mode(void) {
  enum {
    CELL_W = 8,
    CELL_H = 8,
    MAX_COLS = 80,
    MAX_ROWS = 50,
    MAX_WIDTH = MAX_COLS * CELL_W,
    MAX_HEIGHT = MAX_ROWS * CELL_H,
  };

  uint16_t cols = bios_video_columns();
  if (!cols) {
    cols = 80;
  }
  if (cols > MAX_COLS) {
    cols = MAX_COLS;
  }
  uint16_t rows = bios_video_rows();
  if (!rows) {
    rows = 25;
  }
  if (rows > MAX_ROWS) {
    rows = MAX_ROWS;
  }

  uint8_t page = (uint8_t)(memb_raw(0x40, 0x62) & 0x07);
  uint16_t stride = bios_page_stride();
  uint32_t base = (uint32_t)(page % 8) * stride;
  uint16_t segment = (bios_video.video_mode == 0x07) ? 0xB000 : 0xB800;
  const uint8_t *src = seg_off(segment, (uint16_t)base);

  static uint8_t staging[MAX_WIDTH * MAX_HEIGHT];
  int width = cols * CELL_W;
  int height = rows * CELL_H;

  for (uint16_t row = 0; row < rows; ++row) {
    for (uint16_t col = 0; col < cols; ++col) {
      size_t cell_index = (size_t)row * cols + col;
      uint8_t glyph_code = src[cell_index * 2];
      uint8_t attr = src[cell_index * 2 + 1];
      const uint8_t *glyph = font8x8_basic[glyph_code & 0x7F];
      uint8_t fg = (uint8_t)(attr & 0x0F);
      uint8_t bg = (uint8_t)((attr >> 4) & 0x07);
      if (attr & 0x80) {
        bg |= 0x08;
      }
      for (int gy = 0; gy < CELL_H; ++gy) {
        uint8_t bits = glyph[gy];
        uint8_t *dst = staging + ((row * CELL_H + gy) * width) + col * CELL_W;
        for (int gx = 0; gx < CELL_W; ++gx) {
          uint8_t mask = (uint8_t)(1u << gx);
          dst[gx] = (bits & mask) ? fg : bg;
        }
      }
    }
  }

  ensure_text_mode_palette();
  ensure_display_geometry(width, height);
  virtual_display_present(staging, width, width, height, text_mode_palette, 0x3F);
}

static void decode_cga_mode(uint8_t mode, uint8_t *dst) {
  enum {
    H = 200,
    BYTES_PER_ROW = 80,
    CGA_VRAM_SIZE = 0x4000,
    CGA_VRAM_MASK = CGA_VRAM_SIZE - 1
  };
  const uint8_t *vram = seg_off(0xB800, 0);

  const int width = cga_mode_width(mode);
  uint32_t start_offset =
      (((uint32_t)cga.crtc_regs[0x0C] << 8) | (uint32_t)cga.crtc_regs[0x0D]) &
      CGA_VRAM_MASK;
  int scroll = cga.hsync_initialized ? cga.horiz_scroll : 0;
  if (scroll >= width || scroll <= -width) {
    scroll %= width;
  }

  for (int y = 0; y < H; ++y) {
    uint32_t plane_offset = (uint32_t)(y & 1) * 0x2000u +
                            (uint32_t)(y >> 1) * BYTES_PER_ROW;
    uint32_t base_offset = (plane_offset + start_offset) & CGA_VRAM_MASK;
    uint8_t *row = dst + y * width;
    if (mode == 0x06) {
      for (int x = 0; x < width; ++x) {
        int src_x = x + scroll;
        if (src_x < 0) {
          src_x += width;
        } else if (src_x >= width) {
          src_x -= width;
        }
        int byte = src_x >> 3;
        int bit = 7 - (src_x & 7);
        uint32_t addr = (base_offset + (uint32_t)byte) & CGA_VRAM_MASK;
        uint8_t packed = vram[addr];
        row[x] = (uint8_t)((packed >> bit) & 0x01);
      }
    } else {
      for (int x = 0; x < width; ++x) {
        int src_x = x + scroll;
        if (src_x < 0) {
          src_x += width;
        } else if (src_x >= width) {
          src_x -= width;
        }
        int byte = src_x >> 2;
        int sub = src_x & 3;
        uint32_t addr = (base_offset + (uint32_t)byte) & CGA_VRAM_MASK;
        uint8_t packed = vram[addr];
        // In 320x200 4-color mode the 2-bit pixels are packed sequentially
        // with the leftmost pixel in bits 7-6.  Extract the desired pair.
        int shift = (3 - sub) * 2;
        row[x] = (uint8_t)((packed >> shift) & 0x03);
      }
    }
  }
}

static enum StagePresentBranch last_stage_present_branch =
    STAGE_PRESENT_BRANCH_UNKNOWN;

static void stage_and_present_cga_mode(void) {
  enum { H = 200, MAX_W = 640 };
  static uint8_t staging[MAX_W * H];

  const uint8_t mode = bios_video.video_mode;
  const int width = cga_mode_width(mode);

  decode_cga_mode(mode, staging);
  ensure_cga_palette();
  ensure_display_geometry(width, H);
  virtual_display_present(staging, width, width, H, cga_palette, 0x3F);
}

static uint8_t tandy_palette[16 * 3];
static int tandy_palette_initialized;

static void ensure_tandy_palette(void) {
  if (tandy_palette_initialized) {
    return;
  }

  for (int i = 0; i < 16; ++i) {
    tandy_palette[i * 3 + 0] = text_mode_palette_6bit[i][0];
    tandy_palette[i * 3 + 1] = text_mode_palette_6bit[i][1];
    tandy_palette[i * 3 + 2] = text_mode_palette_6bit[i][2];
  }

  tandy_palette_initialized = 1;
}

static void decode_tandy_mode(uint8_t mode, uint8_t *dst) {
  enum {
    TANDY_VRAM_SIZE = 0x8000,
    TANDY_VRAM_MASK = TANDY_VRAM_SIZE - 1
  };

  const uint8_t *vram = seg_off(0xB800, 0);
  const int width = tandy_mode_width(mode);
  const int height = tandy_mode_height(mode);
  const int bytes_per_row = width / 2;

  uint32_t start_offset =
      (((uint32_t)cga.crtc_regs[0x0C] << 8) | (uint32_t)cga.crtc_regs[0x0D]) &
      TANDY_VRAM_MASK;

  for (int y = 0; y < height; ++y) {
    uint32_t base_offset =
        (start_offset + (uint32_t)y * (uint32_t)bytes_per_row) &
        TANDY_VRAM_MASK;
    uint8_t *row = dst + y * width;
    for (int byte = 0; byte < bytes_per_row; ++byte) {
      uint32_t addr = (base_offset + (uint32_t)byte) & TANDY_VRAM_MASK;
      uint8_t packed = vram[addr];
      row[byte * 2 + 0] = (uint8_t)(packed >> 4);
      row[byte * 2 + 1] = (uint8_t)(packed & 0x0F);
    }
  }
}

static void stage_and_present_tandy_mode(void) {
  enum { MAX_W = 640, MAX_H = 200 };
  static uint8_t staging[MAX_W * MAX_H];

  const uint8_t mode = bios_video.video_mode;
  const int width = tandy_mode_width(mode);
  const int height = tandy_mode_height(mode);

  if (width > MAX_W || height > MAX_H) {
    return;
  }

  decode_tandy_mode(mode, staging);
  ensure_tandy_palette();
  ensure_display_geometry(width, height);
  virtual_display_present(staging, width, width, height, tandy_palette, 0x3F);
}

static void decode_planar_mode(uint8_t mode, uint8_t *dst) {
  const int width = planar_mode_width(mode);
  const int height = planar_mode_height(mode);
  const int bytes_per_row = width / 8;

  const uint8_t *plane0 = seg_off(0xA000, 0x0000);
  const uint8_t *plane1 = seg_off(0xA000, 0x2000);
  const uint8_t *plane2 = seg_off(0xA000, 0x4000);
  const uint8_t *plane3 = seg_off(0xA000, 0x6000);

  for (int y = 0; y < height; ++y) {
    const uint8_t *row0 = plane0 + y * bytes_per_row;
    const uint8_t *row1 = plane1 + y * bytes_per_row;
    const uint8_t *row2 = plane2 + y * bytes_per_row;
    const uint8_t *row3 = plane3 + y * bytes_per_row;
    uint8_t *row = dst + y * width;

    for (int byte = 0; byte < bytes_per_row; ++byte) {
      const uint8_t b0 = row0[byte];
      const uint8_t b1 = row1[byte];
      const uint8_t b2 = row2[byte];
      const uint8_t b3 = row3[byte];

      for (int bit = 0; bit < 8; ++bit) {
        const uint8_t mask = (uint8_t)(0x80 >> bit);
        uint8_t color = 0;
        if (b0 & mask)
          color |= 0x01;
        if (b1 & mask)
          color |= 0x02;
        if (b2 & mask)
          color |= 0x04;
        if (b3 & mask)
          color |= 0x08;
        row[byte * 8 + bit] = vga.attr_palette[color & 0x0F];
      }
    }
  }
}

static void stage_and_present_planar_mode(void) {
  enum { MAX_W = 640, MAX_H = 200 };
  static uint8_t staging[MAX_W * MAX_H];

  const uint8_t mode = bios_video.video_mode;
  const int width = planar_mode_width(mode);
  const int height = planar_mode_height(mode);

  if (width > MAX_W || height > MAX_H) {
    return;
  }

  decode_planar_mode(mode, staging);
  ensure_display_geometry(width, height);
  virtual_display_present(staging, width, width, height, vga.palette,
                          vga.palette_mask);
}

void stage_and_present_current_buffer(void) {
  if (is_text_mode(bios_video.video_mode)) {
    last_stage_present_branch = STAGE_PRESENT_BRANCH_TEXT;
    stage_and_present_text_mode();
    return;
  }

  if (is_cga_graphics_mode(bios_video.video_mode)) {
    last_stage_present_branch = STAGE_PRESENT_BRANCH_CGA;
    stage_and_present_cga_mode();
    return;
  }

  if (is_tandy_graphics_mode(bios_video.video_mode)) {
    last_stage_present_branch = STAGE_PRESENT_BRANCH_CGA;
    stage_and_present_tandy_mode();
    return;
  }

  if (is_planar_graphics_mode(bios_video.video_mode)) {
    last_stage_present_branch = STAGE_PRESENT_BRANCH_PLANAR;
    stage_and_present_planar_mode();
    return;
  }

  last_stage_present_branch = STAGE_PRESENT_BRANCH_OTHER;

  enum { W = 320, H = 200, BYTES = W * H };

  // Decide which source to show: 0=A000, 1=BB0, 2=BB1
  const int which = virtual_display_buffer;

  uint16_t seg;
  uint16_t off;
  if (which == 0) {
    seg = 0xA000; // VGA memory
    off = 0x0000;
  } else {
    seg = memw_raw_read(es, DATA_BASE_SEG); // game's backbuffer segment (RCB)
    off = (which == 1) ? 0x0000 : 0x4000;
  }

  static uint8_t staging[BYTES];
  copy_linear_from_segoff(seg, off, BYTES, staging);

  ensure_display_geometry(W, H);
  // Present as a contiguous index buffer; pitch == width
  virtual_display_present(staging, W, W, H, vga.palette, vga.palette_mask);
}

enum StagePresentBranch shim_last_stage_present_branch(void) {
  return last_stage_present_branch;
}

void shim_stage_and_present_current_buffer(void) {
  stage_and_present_current_buffer();
}

void shim_present_current_buffer(void) { stage_and_present_current_buffer(); }

void apply_video_mode_state(uint8_t mode) {
  bios_video.video_mode = mode;
  cga.hsync_initialized = 0;
  cga.horiz_scroll = 0;
  cga.crtc_index = 0;
  memset(cga.crtc_regs, 0, sizeof(cga.crtc_regs));
  memb_raw(0x40, 0x49) = mode;
  uint16_t crtc_port = (mode == 0x07) ? 0x3B4 : 0x3D4;
  memw_raw_write(0x40, 0x0063, crtc_port);
  if (!headless_mode) {
    virtual_display_set_mode(mode);
    if (is_text_mode(mode)) {
      uint16_t cols = bios_video_columns();
      if (!cols) {
        cols = (mode == 0x00 || mode == 0x01) ? 40 : 80;
      }
      uint16_t rows = bios_video_rows();
      if (!rows) {
        rows = 25;
      }
      ensure_display_geometry(cols * 8, rows * 8);
    } else if (is_cga_graphics_mode(mode) || mode == 0x13) {
      ensure_display_geometry(320, 200);
    } else if (is_tandy_graphics_mode(mode)) {
      ensure_display_geometry(tandy_mode_width(mode),
                              tandy_mode_height(mode));
    } else if (is_planar_graphics_mode(mode)) {
      ensure_display_geometry(planar_mode_width(mode),
                              planar_mode_height(mode));
    }
  }
  for (int page = 0; page < 8; ++page) {
    bios_video.cursor_row[page] = 0;
    bios_video.cursor_col[page] = 0;
    bios_video.cursor_attr[page] = 0x07;
    memw_raw_write(0x40, (uint16_t)(0x50 + page * 2), 0);
  }
  bios_video.active_page = 0;
}

int shim_render_screenshot_png(const char *path) {
  const uint8_t mode = bios_video.video_mode;
  int width;
  int height;
  if (is_cga_graphics_mode(mode)) {
    width = cga_mode_width(mode);
    height = 200;
  } else if (is_tandy_graphics_mode(mode)) {
    width = tandy_mode_width(mode);
    height = tandy_mode_height(mode);
  } else {
    width = 320;
    height = 200;
  }
  uint8_t *indices = malloc(width * height);
  if (!indices) {
    return -1;
  }

  const uint8_t *palette;
  uint8_t palette_mask;

  if (is_cga_graphics_mode(mode)) {
    decode_cga_mode(mode, indices);
    ensure_cga_palette();
    palette = cga_palette;
    palette_mask = 0x3F;
  } else if (is_tandy_graphics_mode(mode)) {
    decode_tandy_mode(mode, indices);
    ensure_tandy_palette();
    palette = tandy_palette;
    palette_mask = 0x3F;
  } else {
    uint8_t *src = seg_off(0xA000, 0);
    memcpy(indices, src, width * height);
    palette = vga.palette;
    palette_mask = vga.palette_mask;
  }

  uint8_t *img = malloc(width * height * 3);
  if (!img) {
    free(indices);
    return -1;
  }
  for (int y = 0; y < height; ++y) {
    uint8_t *row = indices + y * width;
    for (int x = 0; x < width; ++x) {
      uint8_t idx = row[x];
      uint8_t r = (uint8_t)(palette[idx * 3] & palette_mask);
      uint8_t g = (uint8_t)(palette[idx * 3 + 1] & palette_mask);
      uint8_t b = (uint8_t)(palette[idx * 3 + 2] & palette_mask);
      r = (r << 2) | (r >> 4);
      g = (g << 2) | (g >> 4);
      b = (b << 2) | (b >> 4);
      int out = (y * width + x) * 3;
      img[out] = r;
      img[out + 1] = g;
      img[out + 2] = b;
    }
  }
  int ok = stbi_write_png(path, width, height, 3, img, width * 3);
  free(indices);
  free(img);
  return ok ? 0 : -1;
}

void video_invalidate_palette_cache(void) {
  cga_palette_initialized = 0;
  cga_palette_last_select = 0xFF;
  cga_palette_last_border = 0xFF;
  tandy_palette_initialized = 0;
}

void bios_cga_render_teletype(uint8_t row, uint8_t col, uint8_t ch_val,
                                     uint8_t attr, uint8_t stored_attr) {
  const uint8_t mode = bios_video.video_mode;
  if (!is_cga_graphics_mode(mode)) {
    return;
  }

  enum {
    CELL_W = 8,
    CELL_H = 8,
    HEIGHT = 200,
    BYTES_PER_ROW = 80,
    CGA_VRAM_SIZE = 0x4000,
    CGA_VRAM_MASK = CGA_VRAM_SIZE - 1,
  };

  const int width = cga_mode_width(mode);
  if (width <= 0) {
    return;
  }

  int pixel_x_base = (int)col * CELL_W;
  int pixel_y_base = (int)row * CELL_H;
  if (pixel_x_base >= width || pixel_y_base >= HEIGHT) {
    return;
  }

  const uint8_t *glyph = font8x8_basic[ch_val & 0x7F];
  uint8_t fg = (uint8_t)(attr & 0x0F);
  uint8_t bg = (uint8_t)((stored_attr >> 4) & 0x0F);
  if (mode == 0x06) {
    fg &= 0x01;
    bg &= 0x01;
  } else {
    fg &= 0x03;
    bg &= 0x03;
  }

  uint8_t *vram = seg_off(0xB800, 0);
  uint32_t start_offset =
      (((uint32_t)cga.crtc_regs[0x0C] << 8) | (uint32_t)cga.crtc_regs[0x0D]) &
      CGA_VRAM_MASK;
  int scroll = cga.hsync_initialized ? cga.horiz_scroll : 0;
  if (scroll >= width || scroll <= -width) {
    scroll %= width;
  }

  for (int gy = 0; gy < CELL_H; ++gy) {
    int y = pixel_y_base + gy;
    if (y >= HEIGHT) {
      break;
    }
    uint8_t bits = glyph[gy];
    uint32_t plane_offset =
        (uint32_t)(y & 1) * 0x2000u + (uint32_t)(y >> 1) * BYTES_PER_ROW;
    uint32_t base_offset = (plane_offset + start_offset) & CGA_VRAM_MASK;

    for (int gx = 0; gx < CELL_W; ++gx) {
      int x = pixel_x_base + gx;
      if (x >= width) {
        break;
      }

      int mem_x = x + scroll;
      if (mem_x < 0) {
        mem_x += width;
      } else if (mem_x >= width) {
        mem_x -= width;
      }

      uint8_t mask = (uint8_t)(1u << gx);
      uint8_t color = (bits & mask) ? fg : bg;
      if (mode == 0x06) {
        int byte = mem_x >> 3;
        int bit = 7 - (mem_x & 7);
        uint32_t addr = (base_offset + (uint32_t)byte) & CGA_VRAM_MASK;
        uint8_t existing = vram[addr];
        if (color & 0x01) {
          existing |= (uint8_t)(1u << bit);
        } else {
          existing &= (uint8_t)~(1u << bit);
        }
        vram[addr] = existing;
      } else {
        int byte = mem_x >> 2;
        int sub = mem_x & 3;
        int shift = (3 - sub) * 2;
        uint32_t addr = (base_offset + (uint32_t)byte) & CGA_VRAM_MASK;
        uint8_t existing = vram[addr];
        existing &= (uint8_t)~(0x03u << shift);
        existing |= (uint8_t)((color & 0x03u) << shift);
        vram[addr] = existing;
      }
    }
  }
}
