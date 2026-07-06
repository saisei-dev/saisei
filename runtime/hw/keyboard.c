/* ============================ hw/keyboard ============================
 * Keyboard controller: scancode ring buffer + enqueue/consume logic.
 * Extracted from shims.c behind keyboard.h. Input sources (SDL, control
 * FIFO) push via shim_keyboard_enqueue*; the BIOS INT16h services and the
 * port 0x60 read in shims.c drain it through the queue ops here. */
#include "keyboard.h"
#include "shims.h"
#include "snapshot.h"

KbdState kbd;

void kbd_queue_push(uint8_t ascii, uint8_t scancode) {
  if (!kbd.scancode_ready) {
    kbd.ascii = ascii;
    kbd.scancode = scancode;
    kbd.scancode_ready = 1;
  } else {
    if (kbd.queue_count >= KBD_BUFFER_SIZE) {
      if ((scancode & 0x80) == 0) {
        // Drop the new non-break event. Keeping the older entries means
        // we still deliver the eventual release scancode and avoid keys
        // getting "stuck" because their break never makes it to the game.
        return;
      }
      // For break scancodes, evict the oldest queued make scancode when
      // possible. This preserves already-queued break events so releases for
      // previously pressed keys are less likely to be lost under heavy input.
      int drop_idx = -1;
      for (int i = 0; i < kbd.queue_count; ++i) {
        int idx = (kbd.queue_head + i) % KBD_BUFFER_SIZE;
        if ((kbd.queue[idx].scancode & 0x80) == 0) {
          drop_idx = idx;
          break;
        }
      }

      if (drop_idx < 0) {
        // Queue is saturated with break events already; drop this new break.
        return;
      }

      while (drop_idx != kbd.queue_head) {
        int prev_idx = (drop_idx - 1 + KBD_BUFFER_SIZE) % KBD_BUFFER_SIZE;
        kbd.queue[drop_idx] = kbd.queue[prev_idx];
        drop_idx = prev_idx;
      }
      kbd.queue_head = (kbd.queue_head + 1) % KBD_BUFFER_SIZE;
      --kbd.queue_count;
    }
    kbd.queue[kbd.queue_tail].ascii = ascii;
    kbd.queue[kbd.queue_tail].scancode = scancode;
    kbd.queue_tail = (kbd.queue_tail + 1) % KBD_BUFFER_SIZE;
    ++kbd.queue_count;
  }
  /*
   * Queueing keyboard input should raise IRQ1 so games that install their own
   * INT 09h handlers continue to observe keystrokes. Keep this here (instead
   * of kbd_consume()) to avoid duplicate IRQ scheduling when an event is read.
   */
  irq_pending[0x09] = 1;
}

int kbd_is_break_scancode(uint8_t scancode) {
  return (scancode & 0x80) != 0;
}

int kbd_peek_next_non_break(uint8_t *ascii, uint8_t *scancode) {
  if (kbd.scancode_ready && !kbd_is_break_scancode(kbd.scancode)) {
    if (ascii)
      *ascii = kbd.ascii;
    if (scancode)
      *scancode = kbd.scancode;
    return 1;
  }
  for (int i = 0; i < kbd.queue_count; ++i) {
    int idx = (kbd.queue_head + i) % KBD_BUFFER_SIZE;
    uint8_t queued_scancode = kbd.queue[idx].scancode;
    if (!kbd_is_break_scancode(queued_scancode)) {
      if (ascii)
        *ascii = kbd.queue[idx].ascii;
      if (scancode)
        *scancode = queued_scancode;
      return 1;
    }
  }
  return 0;
}

int kbd_pop_next_non_break(uint8_t *ascii, uint8_t *scancode) {
  while (kbd.scancode_ready) {
    uint8_t cur_ascii = kbd.ascii;
    uint8_t cur_scancode = kbd.scancode;
    kbd_consume();
    if (!kbd_is_break_scancode(cur_scancode)) {
      if (ascii)
        *ascii = cur_ascii;
      if (scancode)
        *scancode = cur_scancode;
      /* INT 16h delivery path — game just received a make scancode. */
      shim_input_phase_started = 1;
      snapshot_on_key_consumed();
      return 1;
    }
  }
  return 0;
}

int kbd_has_non_break_event(void) {
  if (kbd.scancode_ready && !kbd_is_break_scancode(kbd.scancode)) {
    return 1;
  }
  for (int i = 0; i < kbd.queue_count; ++i) {
    int idx = (kbd.queue_head + i) % KBD_BUFFER_SIZE;
    if (!kbd_is_break_scancode(kbd.queue[idx].scancode)) {
      return 1;
    }
  }
  return 0;
}

void kbd_enqueue(uint8_t ascii, uint8_t scancode) {
  /*
   * BIOS keyboard services deliver key-make events. Automatically injecting
   * break scancodes for every typed character can desynchronize game state
   * machines that poll INT 16h and do not expect releases in that queue.
   */
  kbd_queue_push(ascii, scancode);
}

void kbd_consume(void) {
  if (kbd.queue_count > 0) {
    kbd.ascii = kbd.queue[kbd.queue_head].ascii;
    kbd.scancode = kbd.queue[kbd.queue_head].scancode;
    kbd.queue_head = (kbd.queue_head + 1) % KBD_BUFFER_SIZE;
    --kbd.queue_count;
    kbd.scancode_ready = 1;
    /* Mirror real keyboard controller: each scancode loaded into the data
     * port re-asserts IRQ1 so games that install their own INT 09h handler
     * see one IRQ per scancode (make AND break). */
    irq_pending[0x09] = 1;
  } else {
    kbd.ascii = 0;
    kbd.scancode = 0;
    kbd.scancode_ready = 0;
  }
}

void kbd_reset(void) {
  kbd.queue_head = kbd.queue_tail = kbd.queue_count = 0;
  kbd.ascii = 0;
  kbd.scancode = 0;
  kbd.last_scancode = 0;
  kbd.scancode_ready = 0;
  kbd.bios_head = kbd.bios_tail = kbd.bios_count = 0;
  kbd.pending_bios_ascii = 0;
  kbd.pending_bios_scancode = 0;
  kbd.pending_bios_valid = 0;
}

/* ===================== BIOS type-ahead buffer (40:1E) =====================
 * Distinct from the hardware scancode queue. Filled by the BIOS INT 09h
 * handler (which reads port 0x60, translates, and stores the ASCII+scancode
 * word); drained by INT 16h and the DOS AH=01/06/07/08 console services. Only
 * key-make events are stored: the BIOS does not place key releases in this
 * buffer, so consumers never see break scancodes. */
void kbd_bios_push(uint8_t ascii, uint8_t scancode) {
  if (kbd_is_break_scancode(scancode)) {
    return;
  }
  if (kbd.bios_count >= KBD_BUFFER_SIZE) {
    /* Real BIOS drops the keystroke (and beeps) when the 16-word buffer is
     * full; we just drop it. */
    return;
  }
  kbd.bios_buf[kbd.bios_tail].ascii = ascii;
  kbd.bios_buf[kbd.bios_tail].scancode = scancode;
  kbd.bios_tail = (kbd.bios_tail + 1) % KBD_BUFFER_SIZE;
  ++kbd.bios_count;
}

int kbd_bios_peek(uint8_t *ascii, uint8_t *scancode) {
  if (kbd.bios_count <= 0) {
    return 0;
  }
  if (ascii)
    *ascii = kbd.bios_buf[kbd.bios_head].ascii;
  if (scancode)
    *scancode = kbd.bios_buf[kbd.bios_head].scancode;
  return 1;
}

int kbd_bios_pop(uint8_t *ascii, uint8_t *scancode) {
  if (kbd.bios_count <= 0) {
    return 0;
  }
  if (ascii)
    *ascii = kbd.bios_buf[kbd.bios_head].ascii;
  if (scancode)
    *scancode = kbd.bios_buf[kbd.bios_head].scancode;
  kbd.bios_head = (kbd.bios_head + 1) % KBD_BUFFER_SIZE;
  --kbd.bios_count;
  shim_input_phase_started = 1;
  snapshot_on_key_consumed();
  return 1;
}

int kbd_bios_has_event(void) { return kbd.bios_count > 0; }

/* US-layout scancode (set 1 make code) -> ASCII. Inverse of ascii_to_scan
 * (shims.c); modifier/function/keypad keys have no ASCII and map to 0. The
 * real BIOS INT 09h handler folds in the 0040:0017 shift/caps/ctrl state, so we
 * keep an unshifted AND a shifted layer and select per the live flags. */
static const uint8_t kbd_scan_to_ascii_us[0x40] = {
    [0x01] = 0x1B,
    [0x02] = '1', [0x03] = '2', [0x04] = '3', [0x05] = '4', [0x06] = '5',
    [0x07] = '6', [0x08] = '7', [0x09] = '8', [0x0A] = '9', [0x0B] = '0',
    [0x0C] = '-', [0x0D] = '=', [0x0E] = 0x08, [0x0F] = '\t',
    [0x10] = 'q', [0x11] = 'w', [0x12] = 'e', [0x13] = 'r', [0x14] = 't',
    [0x15] = 'y', [0x16] = 'u', [0x17] = 'i', [0x18] = 'o', [0x19] = 'p',
    [0x1A] = '[', [0x1B] = ']', [0x1C] = '\r',
    [0x1E] = 'a', [0x1F] = 's', [0x20] = 'd', [0x21] = 'f', [0x22] = 'g',
    [0x23] = 'h', [0x24] = 'j', [0x25] = 'k', [0x26] = 'l', [0x27] = ';',
    [0x28] = '\'', [0x29] = '`', [0x2B] = '\\',
    [0x2C] = 'z', [0x2D] = 'x', [0x2E] = 'c', [0x2F] = 'v', [0x30] = 'b',
    [0x31] = 'n', [0x32] = 'm', [0x33] = ',', [0x34] = '.', [0x35] = '/',
    [0x39] = ' ',
};

static const uint8_t kbd_scan_to_ascii_shift[0x40] = {
    [0x01] = 0x1B,
    [0x02] = '!', [0x03] = '@', [0x04] = '#', [0x05] = '$', [0x06] = '%',
    [0x07] = '^', [0x08] = '&', [0x09] = '*', [0x0A] = '(', [0x0B] = ')',
    [0x0C] = '_', [0x0D] = '+', [0x0E] = 0x08, [0x0F] = '\t',
    [0x10] = 'Q', [0x11] = 'W', [0x12] = 'E', [0x13] = 'R', [0x14] = 'T',
    [0x15] = 'Y', [0x16] = 'U', [0x17] = 'I', [0x18] = 'O', [0x19] = 'P',
    [0x1A] = '{', [0x1B] = '}', [0x1C] = '\r',
    [0x1E] = 'A', [0x1F] = 'S', [0x20] = 'D', [0x21] = 'F', [0x22] = 'G',
    [0x23] = 'H', [0x24] = 'J', [0x25] = 'K', [0x26] = 'L', [0x27] = ':',
    [0x28] = '"', [0x29] = '~', [0x2B] = '|',
    [0x2C] = 'Z', [0x2D] = 'X', [0x2E] = 'C', [0x2F] = 'V', [0x30] = 'B',
    [0x31] = 'N', [0x32] = 'M', [0x33] = '<', [0x34] = '>', [0x35] = '?',
    [0x39] = ' ',
};

/* BIOS shift-flags byte 1 lives at BDA 0040:0017 (the real BIOS keeps it there;
 * games read it via INT 16h AH=02 or directly). Maintain it from modifier
 * make/break so the translation and INT 16h see the live state.
 *  bit0=RShift bit1=LShift bit2=Ctrl bit3=Alt
 *  bit4=ScrollLock bit5=NumLock bit6=CapsLock bit7=Insert (toggles). */
static void kbd_update_shift_flags(uint8_t scancode) {
  int make = (scancode & 0x80) == 0;
  uint8_t base = scancode & 0x7F;
  uint8_t *flags = &memb_raw(0x40, 0x17);
  switch (base) {
  case 0x2A: if (make) *flags |= 0x02; else *flags &= (uint8_t)~0x02; break; /*LShift*/
  case 0x36: if (make) *flags |= 0x01; else *flags &= (uint8_t)~0x01; break; /*RShift*/
  case 0x1D: if (make) *flags |= 0x04; else *flags &= (uint8_t)~0x04; break; /*Ctrl*/
  case 0x38: if (make) *flags |= 0x08; else *flags &= (uint8_t)~0x08; break; /*Alt*/
  case 0x3A: if (make) *flags ^= 0x40; break; /*CapsLock toggle on make*/
  case 0x45: if (make) *flags ^= 0x20; break; /*NumLock*/
  case 0x46: if (make) *flags ^= 0x10; break; /*ScrollLock*/
  default: break;
  }
}

static uint8_t scancode_to_ascii(uint8_t scancode) {
  if (scancode >= 0x40) return 0;
  uint8_t flags = memb_raw(0x40, 0x17);
  int shift = (flags & 0x03) != 0;
  int caps = (flags & 0x40) != 0;
  int ctrl = (flags & 0x04) != 0;
  uint8_t base = kbd_scan_to_ascii_us[scancode];
  if (ctrl) { /* Ctrl+letter -> control code 1..26; otherwise base */
    if (base >= 'a' && base <= 'z') return (uint8_t)(base - 'a' + 1);
    return base;
  }
  if (base >= 'a' && base <= 'z') /* letters: Shift XOR CapsLock = upper */
    return (shift ^ caps) ? kbd_scan_to_ascii_shift[scancode] : base;
  return shift ? kbd_scan_to_ascii_shift[scancode] : base; /* CapsLock ignores non-letters */
}

void kbd_bios_deposit_from_isr(void) {
  /*
   * The BIOS INT 09h handler runs either chained-to by a game's own INT 09h
   * ISR (which has already read port 0x60, leaving the make keystroke in the
   * pending latch) or as the default handler (nothing has read 0x60 yet, so we
   * must consume one keystroke from the hardware queue ourselves -- exactly
   * what the real BIOS handler does).
   */
  uint8_t ascii, scancode;
  if (kbd.pending_bios_valid) {
    ascii = kbd.pending_bios_ascii;
    scancode = kbd.pending_bios_scancode;
    kbd.pending_bios_valid = 0;
  } else if (kbd.scancode_ready) {
    /* Default-handler path: read the scancode the controller is presenting and
     * advance the hardware queue, just like an `in al, 0x60`. */
    ascii = kbd.ascii;
    scancode = kbd.scancode;
    kbd.last_scancode = scancode;
    kbd_consume();
  } else {
    return;
  }
  /* Update the BDA shift-flags byte from any modifier make/break (this also
   * runs for break codes, which kbd_bios_push then drops from the buffer). */
  kbd_update_shift_flags(scancode);
  /* The real BIOS derives the ASCII code from the scancode. Our ASCII input
   * paths precompute it (ascii != 0); a raw-scancode source -- the stdin tap
   * opcode, or a game injecting scancodes -- leaves it 0, so do the BIOS
   * translation here for make codes. */
  if (ascii == 0 && !kbd_is_break_scancode(scancode)) {
    ascii = scancode_to_ascii(scancode);
  }
  kbd_bios_push(ascii, scancode);
}

void shim_keyboard_enqueue(uint8_t ascii) {
  if (ascii == '\n') {
    ascii = '\r';
  }
  uint8_t sc = ascii_to_scan(ascii);
  if (sc) snapshot_on_key_event('P', sc);
  kbd_enqueue(ascii, sc);
}

void shim_keyboard_enqueue_scancode_press(uint8_t scancode) {
  if (!scancode) {
    return;
  }
  scancode &= 0x7F;
  snapshot_on_key_event('P', scancode);
  kbd_queue_push(0, scancode);
}

void shim_keyboard_enqueue_scancode_release(uint8_t scancode) {
  if (!scancode) {
    return;
  }
  snapshot_on_key_event('R', (uint8_t)(scancode & 0x7F));
  uint8_t break_scancode = (uint8_t)(scancode | 0x80);
  kbd_queue_push(0, break_scancode);
}

void shim_keyboard_enqueue_scancode(uint8_t scancode) {
  shim_keyboard_enqueue_scancode_press(scancode);
  shim_keyboard_enqueue_scancode_release(scancode);
}
