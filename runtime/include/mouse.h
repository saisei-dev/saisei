#ifndef RUNTIME_OS_MOUSE_H
#define RUNTIME_OS_MOUSE_H

#include <stdint.h>

/* ============================ os/mouse ============================
 * Microsoft-compatible mouse driver (INT 33h). DOS games talk to the mouse
 * exclusively through this software-interrupt API -- there is no BIOS mouse
 * service and (for these titles) no raw PS/2 packet stream -- so the driver
 * IS the device model: it owns the cursor position, the button state, the
 * show/hide nesting counter, the coordinate clamp window, the motion (mickey)
 * counters and the optional user event handler.
 *
 * The INT 33h AX dispatch lives in mouse.c and is register-marshalled exactly
 * like os/bios.c (read AX from the live CPU registers, write results back to
 * BX/CX/DX/...). The host input layer (display/virtual_display_sdl.c) feeds
 * real movement and clicks through mouse_host_motion / mouse_host_button. */

/* INT 33h entry. Reads AX (function number) from the live CPU registers,
 * services it, writes results back. Invoked from int33h_impl in shims.c. */
void mouse_int33_impl(const char *file, const char *func, int line);

/* Host input from the display layer. (x, y) are in logical/game pixels and
 * (w, h) the logical framebuffer size SDL_RenderSetLogicalSize established
 * (e.g. 320x200 for mode 13h); the driver maps them into its current clamp
 * window so the reported position is right whatever range the game set. */
void mouse_host_motion(int x, int y, int w, int h);
void mouse_host_button(int button, int pressed); /* button: 0=L 1=R 2=M */

/* Headless mouse injection (stdin opcode 0x13): absolute driver-pixel x,y plus
 * a button bitmask (bit0=L bit1=R bit2=M), synthesising the same events SDL
 * would so the fn-0x0C handler can be exercised without a GUI. */
void mouse_host_inject(int16_t x, int16_t y, uint16_t buttons);

/* Deliver accrued motion/button events to the game's INT 33h fn-0x0C event
 * handler (far call). Called from safe_point at a delivery-safe point. */
void mouse_deliver_pending_events(void);

#endif
