#ifndef RUNTIME_HW_KEYBOARD_H
#define RUNTIME_HW_KEYBOARD_H

#include <stdint.h>

/* ============================ hw/keyboard ============================
 * PS/2-style keyboard controller: a scancode ring buffer fed by input
 * sources (SDL, the control FIFO) through shim_keyboard_enqueue*, and
 * drained by the BIOS INT 16h services + the port 0x60 read that remain in
 * shims.c. The queue logic lives in keyboard.c; the state is shared (extern
 * kbd) so those consumers and the snapshot layer can read it. */
#define KBD_BUFFER_SIZE 64

typedef struct {
  uint8_t ascii;
  uint8_t scancode;
} KbdEntry;

typedef struct {
  KbdEntry queue[KBD_BUFFER_SIZE];
  int queue_head, queue_tail, queue_count;
  uint8_t ascii;
  uint8_t scancode;
  uint8_t last_scancode;
  int scancode_ready;

  /*
   * BIOS type-ahead buffer (the real PC's 16-word ring at 40:1E, head 40:1A,
   * tail 40:1C). This is DISTINCT from the hardware scancode queue above: the
   * keyboard controller's data port (0x60) and the BIOS buffer are separate on
   * real hardware. A game's INT 09h ISR reads port 0x60 (draining `queue`);
   * the BIOS INT 09h handler translates the scancode and deposits the
   * (ascii, scancode) word HERE; INT 16h and the DOS console-input services
   * (INT 21h AH=01/06/07/08/0A) read from HERE. Conflating the two made a
   * game's own ISR (which reads 0x60) steal the key from the BIOS buffer that
   * AH=06 then polled -- exactly a program's "press any key" dialog. */
  KbdEntry bios_buf[KBD_BUFFER_SIZE];
  int bios_head, bios_tail, bios_count;

  /*
   * Pending BIOS deposit, captured by the port 0x60 read at the instant it
   * consumes a scancode. The chained BIOS INT 09h handler (int09h_impl) flushes
   * it into bios_buf. This preserves the *make* scancode that the game's ISR
   * read, even though `scancode`/`ascii` have since advanced to the next queue
   * entry by the time the BIOS handler runs.
   */
  uint8_t pending_bios_ascii;
  uint8_t pending_bios_scancode;
  int pending_bios_valid;
} KbdState;

extern KbdState kbd;

/* Queue operations used by the BIOS/port consumers in shims.c. */
void kbd_consume(void);
int kbd_has_non_break_event(void);
int kbd_peek_next_non_break(uint8_t *ascii, uint8_t *scancode);
int kbd_pop_next_non_break(uint8_t *ascii, uint8_t *scancode);
void kbd_reset(void);
void kbd_queue_push(uint8_t ascii, uint8_t scancode);
void kbd_enqueue(uint8_t ascii, uint8_t scancode);
int kbd_is_break_scancode(uint8_t scancode);

/* BIOS type-ahead buffer (40:1E) operations. The BIOS INT 09h handler fills
 * it; INT 16h / DOS console input drain it. */
void kbd_bios_push(uint8_t ascii, uint8_t scancode);
int kbd_bios_peek(uint8_t *ascii, uint8_t *scancode);
int kbd_bios_pop(uint8_t *ascii, uint8_t *scancode);
int kbd_bios_has_event(void);
/* Deposit the keystroke a game's INT 09h ISR just read off port 0x60 (or, in
 * the no-ISR default case, read one straight from the hardware queue) into the
 * BIOS type-ahead buffer. Called by the BIOS INT 09h handler. */
void kbd_bios_deposit_from_isr(void);

/* Public input API: SDL + the control FIFO push events here. */
void shim_keyboard_enqueue(uint8_t ascii);
void shim_keyboard_enqueue_scancode_press(uint8_t scancode);
void shim_keyboard_enqueue_scancode_release(uint8_t scancode);
void shim_keyboard_enqueue_scancode(uint8_t scancode);

#endif /* RUNTIME_HW_KEYBOARD_H */
