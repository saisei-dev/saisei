#ifndef VIRTUAL_DISPLAY_H
#define VIRTUAL_DISPLAY_H

#include <stdint.h>

void virtual_display_init(int width, int height, int scale);
void virtual_display_shutdown(void);
void virtual_display_present(const uint8_t *vram, int pitch, int w, int h,
                             const uint8_t *palette, uint8_t palette_mask);
// Process any pending host input without presenting a frame. Useful when the
// emulator loop needs to advance input state during long CPU-bound sections.
void virtual_display_poll_input(void);
void virtual_display_set_mode(int mode);
void virtual_display_configure(int width, int height);
extern int virtual_display_buffer;

#endif /* VIRTUAL_DISPLAY_H */
