#ifndef RUNTIME_HW_VIDEO_H
#define RUNTIME_HW_VIDEO_H

#include <stdint.h>

/* ============================ hw/video ============================
 * Video card: VGA/CGA/BIOS-video registers, mode handling, and the present
 * pipeline (VRAM -> staged pixels -> SDL). Lives in scripts/runtime/hw/
 * video.c and owns its internals -- the CGA palette derivation cache, the
 * 8x8 font, and the staging buffers. The runtime talks to it through the
 * interface below; the VGA/CGA register state is shared with the IO-port
 * dispatch in shims.c (extern structs) but the derived palette cache is
 * private, so port writes that change CGA palette inputs call
 * video_invalidate_cga_palette() rather than poke the cache.
 *
 * The three register-state structs are exposed so the IO-port dispatch can
 * write registers and the snapshot aggregate (ShimRuntimeState) can embed
 * them; their derived caches stay inside video.c. */
typedef struct {
  uint8_t palette[256 * 3];
  uint8_t palette_write_index;
  uint8_t palette_component;
  uint8_t palette_read_index;
  uint8_t palette_mask;
  uint8_t attr_palette[16];
  uint8_t border_color;
  uint8_t blink_mode;
  uint8_t dac_paging_mode;
  uint8_t dac_current_page;
  uint8_t misc_output;
  uint8_t feature_control;
  uint8_t graphics_index;
  uint8_t graphics_regs[16];
} VgaState;

typedef struct {
  uint8_t crtc_index;
  uint8_t crtc_regs[0x20];
  uint8_t hsync_base;
  int32_t horiz_scroll;
  uint8_t hsync_initialized;
} CgaState;

typedef struct {
  uint8_t video_mode;
  uint8_t cursor_row[8];
  uint8_t cursor_col[8];
  uint8_t cursor_attr[8];
  uint8_t active_page;
  uint8_t cga_palette_select;
  uint8_t cga_border_color;
} BiosVideoState;

extern VgaState vga;
extern CgaState cga;
extern BiosVideoState bios_video;

/* Which staging path the last present took (for tests/diagnostics). */
enum StagePresentBranch {
  STAGE_PRESENT_BRANCH_UNKNOWN = 0,
  STAGE_PRESENT_BRANCH_TEXT,
  STAGE_PRESENT_BRANCH_CGA,
  STAGE_PRESENT_BRANCH_PLANAR,
  STAGE_PRESENT_BRANCH_OTHER,
};

/* ---- the runtime's interface to the video card ---- */
void apply_video_mode_state(uint8_t mode);        /* BIOS INT10h mode set */
void stage_and_present_current_buffer(void);      /* present from main loop */
void shim_stage_and_present_current_buffer(void);
void shim_present_current_buffer(void);
enum StagePresentBranch shim_last_stage_present_branch(void);
int shim_render_screenshot_png(const char *path); /* screenshot */
uint8_t vga_dac_component(uint8_t value);         /* DAC 6-bit clamp (ports) */
/* Drop the derived CGA palette cache; call when CGA palette/border inputs
 * change (port writes, mode reset). The cache lazily re-derives on present. */
void video_invalidate_palette_cache(void);
/* Mode predicates used by the BIOS teletype path in shims.c. */
int is_text_mode(uint8_t mode);
int is_cga_graphics_mode(uint8_t mode);
/* CGA-graphics-mode glyph blit used by the BIOS teletype path. */
void bios_cga_render_teletype(uint8_t row, uint8_t col, uint8_t ch_val,
                              uint8_t attr, uint8_t stored_attr);

#endif /* RUNTIME_HW_VIDEO_H */
