/* ============================ os/mouse ============================
 * Microsoft-compatible mouse driver (INT 33h).
 *
 * DOS games reach the mouse only through this software-interrupt API, so the
 * driver here IS the device model -- there is no separate BIOS mouse service
 * and these titles don't read raw PS/2 packets. It owns cursor position,
 * button state, the show/hide nesting counter, the coordinate clamp window,
 * the motion (mickey) counters and the optional user event handler.
 *
 * Register marshalling mirrors os/bios.c: read AX (the 16-bit function number)
 * from the live CPU registers, act, and write results back to BX/CX/DX/...
 * SHIMS_DISABLE_MACROS makes the *_impl wrappers compile as plain functions,
 * same as os/dos.c and os/bios.c. The host input layer (the SDL display)
 * pushes real movement/clicks via mouse_host_motion / mouse_host_button.
 */
#define SHIMS_DISABLE_MACROS
#include "shims.h"
#include "mouse.h"
#include <stdlib.h>
#include <string.h>

/* Default virtual-coordinate window of the MS driver (independent of the
 * active video mode; a game narrows it with functions 0x07/0x08). */
#define MOUSE_DEF_XMIN 0
#define MOUSE_DEF_XMAX 639
#define MOUSE_DEF_YMIN 0
#define MOUSE_DEF_YMAX 199
#define MOUSE_BUTTONS  2

/* One discrete INT 33h event as the real driver hands it to the fn-0x0C handler:
 * a SINGLE condition edge (a move, or one button press/release) carrying the
 * button state and cursor position AS OF that edge. The real driver fires the
 * handler once per edge; it does NOT coalesce a press and its release into one
 * callback (which would report the final button state -- button UP -- and lose
 * the press), so a UI that arms on press and acts on release, or that reads the
 * button state at the press edge, sees both edges faithfully. */
#define MOUSE_EV_QUEUE 64
typedef struct {
  uint16_t mask;     /* condition bits for THIS edge (1=move, press/release...) */
  uint16_t buttons;  /* button bitmask AS OF this edge */
  int16_t x, y;      /* cursor position at this edge */
  int16_t mdx, mdy;  /* mickey delta for this edge (0 for button edges) */
} MouseEvent;

typedef struct {
  int16_t x, y;                 /* cursor position (driver virtual pixels) */
  uint16_t buttons;             /* currently-down: bit0=L bit1=R bit2=M */
  int16_t show_count;           /* >=0 visible, <0 hidden (show/hide nesting) */
  int16_t xmin, xmax, ymin, ymax;
  int16_t mickey_x, mickey_y;   /* motion accumulators since last fn 0x0B */
  uint16_t press_count[3];
  int16_t press_x[3], press_y[3];
  uint16_t release_count[3];
  int16_t release_x[3], release_y[3];
  uint16_t handler_seg, handler_off, handler_mask; /* fn 0x0C user handler */
  /* FIFO of discrete edges awaiting delivery to the fn-0x0C handler. */
  MouseEvent ev[MOUSE_EV_QUEUE];
  int ev_head, ev_tail, ev_count;
  uint8_t installed;
} MouseState;

static MouseState m;
/* Re-entrancy guard: a queued edge is delivered via shim_invoke_far_call, which
 * runs the handler nested (and hits safepoints that re-enter delivery); do not
 * deliver the next edge nested inside the current one. */
static int mouse_delivering;

static int16_t clamp16(int v, int lo, int hi) {
  if (v < lo) return (int16_t)lo;
  if (v > hi) return (int16_t)hi;
  return (int16_t)v;
}

/* Append one discrete edge to the delivery FIFO, capturing the button state and
 * cursor position AS OF now (callers update m.buttons / m.x / m.y first). */
static void mouse_queue_event(uint16_t mask, int16_t mdx, int16_t mdy) {
  if (m.ev_count >= MOUSE_EV_QUEUE) {
    /* Bounded FIFO: drop the OLDEST undelivered edge (only reachable if nothing
     * is draining -- no handler yet; a live handler keeps the queue near-empty). */
    m.ev_head = (m.ev_head + 1) % MOUSE_EV_QUEUE;
    m.ev_count--;
  }
  MouseEvent *e = &m.ev[m.ev_tail];
  e->mask = mask;
  e->buttons = m.buttons;
  e->x = m.x;
  e->y = m.y;
  e->mdx = mdx;
  e->mdy = mdy;
  m.ev_tail = (m.ev_tail + 1) % MOUSE_EV_QUEUE;
  m.ev_count++;
}

static void mouse_reset_state(void) {
  /* A driver reset clears everything -- position, buttons, ranges, counters
   * and any installed event handler. */
  memset(&m, 0, sizeof(m));
  m.xmin = MOUSE_DEF_XMIN;
  m.xmax = MOUSE_DEF_XMAX;
  m.ymin = MOUSE_DEF_YMIN;
  m.ymax = MOUSE_DEF_YMAX;
  m.x = (int16_t)((m.xmin + m.xmax) / 2);
  m.y = (int16_t)((m.ymin + m.ymax) / 2);
  m.show_count = -1; /* the driver starts with the cursor hidden */
  m.installed = 1;
  mouse_delivering = 0;
}

/* ---- host input (display layer) -------------------------------------- */

void mouse_host_motion(int x, int y, int w, int h) {
  int16_t nx = (w > 1)
      ? clamp16(m.xmin + (int)((long)x * (m.xmax - m.xmin) / (w - 1)),
                m.xmin, m.xmax)
      : clamp16(x, m.xmin, m.xmax);
  int16_t ny = (h > 1)
      ? clamp16(m.ymin + (int)((long)y * (m.ymax - m.ymin) / (h - 1)),
                m.ymin, m.ymax)
      : clamp16(y, m.ymin, m.ymax);
  if (nx != m.x || ny != m.y) {
    int16_t ddx = (int16_t)(nx - m.x), ddy = (int16_t)(ny - m.y);
    m.mickey_x = (int16_t)(m.mickey_x + ddx);
    m.mickey_y = (int16_t)(m.mickey_y + ddy);
    m.x = nx;
    m.y = ny;
    mouse_queue_event(0x0001, ddx, ddy); /* condition bit 0: cursor moved */
  }
}

void mouse_host_button(int button, int pressed) {
  if (button < 0 || button > 2) return;
  uint16_t mask = (uint16_t)(1u << button);
  if (pressed) {
    if (m.buttons & mask) return; /* no edge */
    m.buttons |= mask;
    m.press_count[button]++;
    m.press_x[button] = m.x;
    m.press_y[button] = m.y;
    mouse_queue_event((uint16_t)(1u << (1 + 2 * button)), 0, 0); /* L/R/M press */
  } else {
    if (!(m.buttons & mask)) return; /* no edge */
    m.buttons &= (uint16_t)~mask;
    m.release_count[button]++;
    m.release_x[button] = m.x;
    m.release_y[button] = m.y;
    mouse_queue_event((uint16_t)(1u << (2 + 2 * button)), 0, 0); /* L/R/M release */
  }
}

/* Headless mouse injection (stdin opcode): set absolute driver-pixel position
 * and the button bitmask, synthesising the same motion/press/release events a
 * real SDL device would, so the event handler can be exercised without a GUI. */
void mouse_host_inject(int16_t x, int16_t y, uint16_t buttons) {
  int16_t nx = clamp16(x, m.xmin, m.xmax), ny = clamp16(y, m.ymin, m.ymax);
  if (nx != m.x || ny != m.y) {
    int16_t ddx = (int16_t)(nx - m.x), ddy = (int16_t)(ny - m.y);
    m.mickey_x = (int16_t)(m.mickey_x + ddx);
    m.mickey_y = (int16_t)(m.mickey_y + ddy);
    m.x = nx;
    m.y = ny;
    mouse_queue_event(0x0001, ddx, ddy);
  }
  for (int b = 0; b < 3; ++b) mouse_host_button(b, (buttons >> b) & 1);
}

/* Deliver any accrued events to the game's INT 33h fn-0x0C handler as a far
 * call (DM and most mouse games drive their UI entirely off this callback, not
 * polling). Called from safe_point at a delivery-safe point. */
void mouse_deliver_pending_events(void) {
  if (!m.installed) return;
  if (m.handler_seg == 0 && m.handler_off == 0) {
    /* No handler installed: the real driver delivers nothing. Drop queued edges
     * (fn 0x03/0x05/0x06 polling state is maintained separately in m). */
    m.ev_head = m.ev_tail = m.ev_count = 0;
    return;
  }
  if (mouse_delivering) return; /* don't deliver an edge nested inside a handler */
  mouse_delivering = 1;
  while (m.ev_count > 0) {
    MouseEvent e = m.ev[m.ev_head];
    m.ev_head = (m.ev_head + 1) % MOUSE_EV_QUEUE;
    m.ev_count--;
    uint16_t events = e.mask & m.handler_mask;
    if (events == 0) continue;
    /* AX=condition mask, BX=button state, CX=x, DX=y, SI/DI=mickeys -- all AS OF
     * this single edge (one callback per edge, the faithful MS driver model). */
    shim_invoke_far_call(m.handler_seg, m.handler_off, events, e.buttons,
                         (uint16_t)e.x, (uint16_t)e.y, (uint16_t)e.mdx,
                         (uint16_t)e.mdy);
  }
  mouse_delivering = 0;
}

/* ---- INT 33h dispatch ------------------------------------------------ */

void mouse_int33_impl(const char *file, const char *func, int line) {
  shim_log(__func__, file, func, line, NULL);
  switch (ax) {
  case 0x0000: /* Reset driver and request status */
  case 0x0021: /* Software reset */
    mouse_reset_state();
    ax = 0xFFFF;          /* mouse installed */
    bx = MOUSE_BUTTONS;   /* number of buttons */
    break;

  case 0x0001: /* Show cursor */
    if (m.show_count < 0) m.show_count++;
    break;

  case 0x0002: /* Hide cursor */
    m.show_count--;
    break;

  case 0x0003: /* Get position and button status */
    bx = m.buttons;
    cx = (uint16_t)m.x;
    dx = (uint16_t)m.y;
    break;

  case 0x0004: /* Set cursor position */
    m.x = clamp16((int16_t)cx, m.xmin, m.xmax);
    m.y = clamp16((int16_t)dx, m.ymin, m.ymax);
    break;

  case 0x0005: { /* Get button press data (BX selects the button) */
    int b = (bx <= 2) ? (int)bx : 0;
    ax = m.buttons;
    cx = (uint16_t)m.press_x[b];
    dx = (uint16_t)m.press_y[b];
    bx = m.press_count[b];
    m.press_count[b] = 0;
    break;
  }

  case 0x0006: { /* Get button release data (BX selects the button) */
    int b = (bx <= 2) ? (int)bx : 0;
    ax = m.buttons;
    cx = (uint16_t)m.release_x[b];
    dx = (uint16_t)m.release_y[b];
    bx = m.release_count[b];
    m.release_count[b] = 0;
    break;
  }

  case 0x0007: /* Set horizontal min/max */
    if ((int16_t)cx <= (int16_t)dx) { m.xmin = (int16_t)cx; m.xmax = (int16_t)dx; }
    else { m.xmin = (int16_t)dx; m.xmax = (int16_t)cx; }
    m.x = clamp16(m.x, m.xmin, m.xmax);
    break;

  case 0x0008: /* Set vertical min/max */
    if ((int16_t)cx <= (int16_t)dx) { m.ymin = (int16_t)cx; m.ymax = (int16_t)dx; }
    else { m.ymin = (int16_t)dx; m.ymax = (int16_t)cx; }
    m.y = clamp16(m.y, m.ymin, m.ymax);
    break;

  case 0x000B: /* Read motion counters (mickeys), then clear */
    cx = (uint16_t)m.mickey_x;
    dx = (uint16_t)m.mickey_y;
    m.mickey_x = 0;
    m.mickey_y = 0;
    break;

  case 0x000C: /* Set user event handler (ES:DX, mask in CX) */
    m.handler_mask = cx;
    m.handler_off = dx;
    m.handler_seg = es;
    break;

  case 0x0014: { /* Swap user event handler: return the previous one */
    uint16_t old_seg = m.handler_seg, old_off = m.handler_off,
             old_mask = m.handler_mask;
    m.handler_mask = cx;
    m.handler_off = dx;
    m.handler_seg = es;
    cx = old_mask;
    dx = old_off;
    es = old_seg;
    break;
  }

  case 0x0015: /* Get driver state storage size */
    bx = (uint16_t)sizeof(MouseState);
    break;

  case 0x0016: /* Save driver state to ES:DX */
    memcpy(seg_off(es, dx), &m, sizeof(MouseState));
    break;

  case 0x0017: /* Restore driver state from ES:DX */
    memcpy(&m, seg_off(es, dx), sizeof(MouseState));
    break;

  case 0x001B: /* Get sensitivity -- report the 1:1 / default profile */
    bx = 50;   /* horizontal mickeys per 8 pixels (default) */
    cx = 50;   /* vertical */
    dx = 50;   /* double-speed threshold */
    break;

  case 0x001E: /* Get cursor display page */
    bx = 0;
    break;

  case 0x0024: /* Get driver version, mouse type and IRQ */
    bx = 0x0700;  /* report version 7.00 */
    cx = 0x0400;  /* CH=0x04 PS/2 mouse, CL=0x00 IRQ (PS/2 has no PIC IRQ) */
    break;

  /* Configuration calls that have no observable effect in this model -- the
   * cursor is drawn by the game, motion comes from the host, and there is no
   * real serial/PS/2 sampling to retune. Accept and ignore (a real driver
   * likewise returns nothing for these). */
  case 0x0009: /* Set graphics cursor shape (BX hotspot, ES:DX masks) */
  case 0x000A: /* Set text cursor type */
  case 0x000D: /* Light-pen emulation on */
  case 0x000E: /* Light-pen emulation off */
  case 0x000F: /* Set mickey/pixel ratio */
  case 0x0010: /* Set cursor exclusion area */
  case 0x0013: /* Set double-speed threshold */
  case 0x001A: /* Set sensitivity */
  case 0x001C: /* Set interrupt rate */
  case 0x001D: /* Set cursor display page */
    break;

  default: {
    /* An INT 33h function we don't model yet. Per the prime directive a gap
     * is a hard failure surfaced loudly (with the live AX), not a silent
     * no-op -- so the next mouse function a game needs is obvious from the
     * bundle and gets implemented, rather than the game limping on wrong
     * results. */
    char msg[192];
    snprintf(msg, sizeof(msg),
             "unhandled INT 33h mouse function AX=0x%04X "
             "(bx=%04X cx=%04X dx=%04X) (%s:%s:%d)",
             ax, bx, cx, dx, file, func, line);
    shim_log_crash("%s\n", msg);
    shim_save_bug_bundle("unimplemented_mouse", ((uint32_t)cs << 4) + ip, msg);
    abort();
  }
  }
}
