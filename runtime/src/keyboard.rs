//! Port of `runtime/hw/keyboard.c` — PS/2 keyboard controller.
//!
//! A scancode ring buffer fed by input sources (SDL, control FIFO) via
//! shim_keyboard_enqueue*, drained by the BIOS INT16h services + the port 0x60
//! read that stay in shims.c. Plus the separate BIOS type-ahead buffer (40:1E).
//! ABI-identical to keyboard.h; logic unchanged so input behaviour is preserved.

use core::ffi::c_char;

pub const KBD_BUFFER_SIZE: usize = 64;

extern "C" {
    static mut virtual_memory: *mut u8;
    static a20_enabled: u8;
    static mut shim_input_phase_started: i32;
    fn snapshot_on_key_consumed();
    fn snapshot_on_key_event(kind: c_char, scancode: u8);
    fn ascii_to_scan(ascii: u8) -> u8;
    // Raise a pending IRQ through the count-tracking helper (never write
    // `irq_pending[]` directly — see shim_mark_irq_pending in shims.rs).
    fn shim_mark_irq_pending(int_no: u8);
}

#[inline(always)]
fn linear_addr(seg: u16, off: u16) -> usize {
    let addr = ((seg as u32) << 4).wrapping_add(off as u32);
    (if unsafe { a20_enabled } != 0 {
        addr & 0x1FFFFF
    } else {
        addr & 0xFFFFF
    }) as usize
}
/// `&memb_raw(seg, off)` — a raw pointer to the byte in virtual memory.
#[inline(always)]
fn memb_raw_ptr(seg: u16, off: u16) -> *mut u8 {
    unsafe { virtual_memory.add(linear_addr(seg, off)) }
}

/// The canonical `KbdEntry` layout (was keyboard.h — FROZEN for snapshots).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct KbdEntry {
    pub ascii: u8,
    pub scancode: u8,
}

/// The canonical `KbdState` layout (was keyboard.h; snapshot reads it). `int` -> i32.
#[repr(C)]
pub struct KbdState {
    pub queue: [KbdEntry; KBD_BUFFER_SIZE],
    pub queue_head: i32,
    pub queue_tail: i32,
    pub queue_count: i32,
    pub ascii: u8,
    pub scancode: u8,
    pub last_scancode: u8,
    pub scancode_ready: i32,
    pub bios_buf: [KbdEntry; KBD_BUFFER_SIZE],
    pub bios_head: i32,
    pub bios_tail: i32,
    pub bios_count: i32,
    pub pending_bios_ascii: u8,
    pub pending_bios_scancode: u8,
    pub pending_bios_valid: i32,
}

#[no_mangle]
pub static mut kbd: KbdState = KbdState {
    queue: [KbdEntry {
        ascii: 0,
        scancode: 0,
    }; KBD_BUFFER_SIZE],
    queue_head: 0,
    queue_tail: 0,
    queue_count: 0,
    ascii: 0,
    scancode: 0,
    last_scancode: 0,
    scancode_ready: 0,
    bios_buf: [KbdEntry {
        ascii: 0,
        scancode: 0,
    }; KBD_BUFFER_SIZE],
    bios_head: 0,
    bios_tail: 0,
    bios_count: 0,
    pending_bios_ascii: 0,
    pending_bios_scancode: 0,
    pending_bios_valid: 0,
};

#[inline(always)]
fn k() -> *mut KbdState {
    core::ptr::addr_of_mut!(kbd)
}
const SZ: i32 = KBD_BUFFER_SIZE as i32;

#[no_mangle]
pub extern "C" fn kbd_queue_push(ascii: u8, scancode: u8) {
    unsafe {
        let st = &mut *k();
        if st.scancode_ready == 0 {
            st.ascii = ascii;
            st.scancode = scancode;
            st.scancode_ready = 1;
        } else {
            if st.queue_count >= SZ {
                if scancode & 0x80 == 0 {
                    return; // drop the new non-break event
                }
                let mut drop_idx: i32 = -1;
                for i in 0..st.queue_count {
                    let idx = (st.queue_head + i) % SZ;
                    if st.queue[idx as usize].scancode & 0x80 == 0 {
                        drop_idx = idx;
                        break;
                    }
                }
                if drop_idx < 0 {
                    return; // saturated with breaks; drop this new break
                }
                while drop_idx != st.queue_head {
                    let prev_idx = (drop_idx - 1 + SZ) % SZ;
                    st.queue[drop_idx as usize] = st.queue[prev_idx as usize];
                    drop_idx = prev_idx;
                }
                st.queue_head = (st.queue_head + 1) % SZ;
                st.queue_count -= 1;
            }
            let tail = st.queue_tail as usize;
            st.queue[tail].ascii = ascii;
            st.queue[tail].scancode = scancode;
            st.queue_tail = (st.queue_tail + 1) % SZ;
            st.queue_count += 1;
        }
        // Raise IRQ1 so games with their own INT 09h handler see keystrokes.
        shim_mark_irq_pending(0x09);
    }
}

#[no_mangle]
pub extern "C" fn kbd_is_break_scancode(scancode: u8) -> i32 {
    (scancode & 0x80 != 0) as i32
}

/// A queue byte that is not a key MAKE: break codes (bit 7) and the 0xE0
/// extended-key prefix byte (which precedes the make/break it qualifies).
#[inline(always)]
fn kbd_is_non_event_byte(scancode: u8) -> bool {
    scancode & 0x80 != 0 || scancode == 0xE0
}

#[no_mangle]
pub extern "C" fn kbd_peek_next_non_break(ascii: *mut u8, scancode: *mut u8) -> i32 {
    unsafe {
        let st = &*k();
        if st.scancode_ready != 0 && !kbd_is_non_event_byte(st.scancode) {
            if !ascii.is_null() {
                *ascii = st.ascii;
            }
            if !scancode.is_null() {
                *scancode = st.scancode;
            }
            return 1;
        }
        for i in 0..st.queue_count {
            let idx = ((st.queue_head + i) % SZ) as usize;
            let qsc = st.queue[idx].scancode;
            if !kbd_is_non_event_byte(qsc) {
                if !ascii.is_null() {
                    *ascii = st.queue[idx].ascii;
                }
                if !scancode.is_null() {
                    *scancode = qsc;
                }
                return 1;
            }
        }
        0
    }
}

#[no_mangle]
pub extern "C" fn kbd_pop_next_non_break(ascii: *mut u8, scancode: *mut u8) -> i32 {
    unsafe {
        while (*k()).scancode_ready != 0 {
            let cur_ascii = (*k()).ascii;
            let cur_scancode = (*k()).scancode;
            kbd_consume();
            if !kbd_is_non_event_byte(cur_scancode) {
                if !ascii.is_null() {
                    *ascii = cur_ascii;
                }
                if !scancode.is_null() {
                    *scancode = cur_scancode;
                }
                shim_input_phase_started = 1;
                snapshot_on_key_consumed();
                return 1;
            }
        }
        0
    }
}

#[no_mangle]
pub extern "C" fn kbd_has_non_break_event() -> i32 {
    unsafe {
        let st = &*k();
        if st.scancode_ready != 0 && !kbd_is_non_event_byte(st.scancode) {
            return 1;
        }
        for i in 0..st.queue_count {
            let idx = ((st.queue_head + i) % SZ) as usize;
            if !kbd_is_non_event_byte(st.queue[idx].scancode) {
                return 1;
            }
        }
        0
    }
}

#[no_mangle]
pub extern "C" fn kbd_enqueue(ascii: u8, scancode: u8) {
    kbd_queue_push(ascii, scancode);
}

#[no_mangle]
pub extern "C" fn kbd_consume() {
    unsafe {
        let st = &mut *k();
        if st.queue_count > 0 {
            let head = st.queue_head as usize;
            st.ascii = st.queue[head].ascii;
            st.scancode = st.queue[head].scancode;
            st.queue_head = (st.queue_head + 1) % SZ;
            st.queue_count -= 1;
            st.scancode_ready = 1;
            shim_mark_irq_pending(0x09);
        } else {
            st.ascii = 0;
            st.scancode = 0;
            st.scancode_ready = 0;
        }
    }
}

#[no_mangle]
pub extern "C" fn kbd_reset() {
    unsafe {
        let st = &mut *k();
        st.queue_head = 0;
        st.queue_tail = 0;
        st.queue_count = 0;
        st.ascii = 0;
        st.scancode = 0;
        st.last_scancode = 0;
        st.scancode_ready = 0;
        // bios_head/bios_tail/bios_count are vestigial: the type-ahead ring now
        // lives in the BDA, where the guest can see it. They stay in the struct
        // because its layout is frozen for snapshots.
        st.bios_head = 0;
        st.bios_tail = 0;
        st.bios_count = 0;
        st.pending_bios_ascii = 0;
        st.pending_bios_scancode = 0;
        st.pending_bios_valid = 0;
    }
    kbd_bios_buffer_init();
}

// ---- the BIOS type-ahead buffer, where the BIOS keeps it -------------------
//
// This is a ring of 16 two-byte entries (low = ASCII, high = scancode) that
// lives **in the BIOS data area**, with its head and tail at 40:1A and 40:1C and
// its bounds published at 40:80 and 40:82. It is not an implementation detail of
// INT 16h: reading it is a documented way to test for a keypress without one,
// and `head = tail` is *the* way to flush it. Games do both, constantly.
//
// It used to be a host-side struct instead — a buffer with the right contents in
// the wrong place. INT 16h worked, and a game that read the buffer directly saw
// head == tail == 0 and concluded, forever, that nobody had pressed anything.
// MechWarrior polls it at its prompt: it waits about fifteen timer ticks for a
// key, is told there are none, and quits. Nothing in a trace shows it happening,
// because reading a BDA word is just a memory read.
//
// Now the guest's buffer and ours are the same object, so they cannot disagree.
// It also snapshots for free: it is guest RAM.

const BDA_KBD_HEAD: u16 = 0x1A;
const BDA_KBD_TAIL: u16 = 0x1C;
pub const BDA_KBD_BUF_START: u16 = 0x1E;
pub const BDA_KBD_BUF_END: u16 = 0x3E; // one past the last entry

#[inline]
unsafe fn bda_word(off: u16) -> u16 {
    let p = memb_raw_ptr(0x40, off);
    (*p as u16) | ((*p.add(1) as u16) << 8)
}

#[inline]
unsafe fn bda_set_word(off: u16, v: u16) {
    let p = memb_raw_ptr(0x40, off);
    *p = (v & 0xFF) as u8;
    *p.add(1) = (v >> 8) as u8;
}

/// The ring's bounds as the BIOS publishes them. A program is allowed to move
/// the buffer by rewriting these (that is what they are for), so follow them
/// rather than assuming 1E/3E — but keep the ring inside the BDA page, since
/// that is the only memory the BIOS owns here.
#[inline]
unsafe fn ring_bounds() -> (u16, u16) {
    let start = bda_word(0x80);
    let end = bda_word(0x82);
    if start >= BDA_KBD_BUF_START && end > start && end <= 0x100 && (end - start) >= 4 {
        (start, end)
    } else {
        (BDA_KBD_BUF_START, BDA_KBD_BUF_END)
    }
}

#[inline]
unsafe fn ring_next(off: u16, start: u16, end: u16) -> u16 {
    let n = off.wrapping_add(2);
    if n >= end {
        start
    } else {
        n
    }
}

/// Head/tail as the guest left them, snapped back into the ring if they are not
/// in it (an uninitialized BDA would otherwise have us writing keys over the
/// COM1 port address at 40:00).
#[inline]
unsafe fn ring_ptrs(start: u16, end: u16) -> (u16, u16) {
    let fix = |p: u16| {
        if p >= start && p < end && (p - start) % 2 == 0 {
            p
        } else {
            start
        }
    };
    (fix(bda_word(BDA_KBD_HEAD)), fix(bda_word(BDA_KBD_TAIL)))
}

#[no_mangle]
pub extern "C" fn kbd_bios_push(ascii: u8, scancode: u8) {
    unsafe {
        if kbd_is_break_scancode(scancode) != 0 {
            return;
        }
        let (start, end) = ring_bounds();
        let (head, tail) = ring_ptrs(start, end);
        let next = ring_next(tail, start, end);
        if next == head {
            return; // full: the BIOS beeps and drops the key
        }
        let p = memb_raw_ptr(0x40, tail);
        *p = ascii;
        *p.add(1) = scancode;
        bda_set_word(BDA_KBD_TAIL, next);
    }
}

#[no_mangle]
pub extern "C" fn kbd_bios_peek(ascii: *mut u8, scancode: *mut u8) -> i32 {
    unsafe {
        let (start, end) = ring_bounds();
        let (head, tail) = ring_ptrs(start, end);
        if head == tail {
            return 0;
        }
        let p = memb_raw_ptr(0x40, head);
        if !ascii.is_null() {
            *ascii = *p;
        }
        if !scancode.is_null() {
            *scancode = *p.add(1);
        }
        1
    }
}

#[no_mangle]
pub extern "C" fn kbd_bios_pop(ascii: *mut u8, scancode: *mut u8) -> i32 {
    unsafe {
        let (start, end) = ring_bounds();
        let (head, tail) = ring_ptrs(start, end);
        if head == tail {
            return 0;
        }
        let p = memb_raw_ptr(0x40, head);
        if !ascii.is_null() {
            *ascii = *p;
        }
        if !scancode.is_null() {
            *scancode = *p.add(1);
        }
        bda_set_word(BDA_KBD_HEAD, ring_next(head, start, end));
        shim_input_phase_started = 1;
        snapshot_on_key_consumed();
        1
    }
}

#[no_mangle]
pub extern "C" fn kbd_bios_has_event() -> i32 {
    unsafe {
        let (start, end) = ring_bounds();
        let (head, tail) = ring_ptrs(start, end);
        (head != tail) as i32
    }
}

/// Put the ring back the way the BIOS leaves it at power-on. Called from
/// `init_bios_data_area` (which zeroes the page first) and from `kbd_reset`.
#[no_mangle]
pub extern "C" fn kbd_bios_buffer_init() {
    unsafe {
        bda_set_word(0x80, BDA_KBD_BUF_START);
        bda_set_word(0x82, BDA_KBD_BUF_END);
        bda_set_word(BDA_KBD_HEAD, BDA_KBD_BUF_START);
        bda_set_word(BDA_KBD_TAIL, BDA_KBD_BUF_START);
    }
}

const fn build_us() -> [u8; 0x40] {
    let mut a = [0u8; 0x40];
    a[0x01] = 0x1B;
    let m: &[(usize, u8)] = &[
        (0x02, b'1'),
        (0x03, b'2'),
        (0x04, b'3'),
        (0x05, b'4'),
        (0x06, b'5'),
        (0x07, b'6'),
        (0x08, b'7'),
        (0x09, b'8'),
        (0x0A, b'9'),
        (0x0B, b'0'),
        (0x0C, b'-'),
        (0x0D, b'='),
        (0x0E, 0x08),
        (0x0F, b'\t'),
        (0x10, b'q'),
        (0x11, b'w'),
        (0x12, b'e'),
        (0x13, b'r'),
        (0x14, b't'),
        (0x15, b'y'),
        (0x16, b'u'),
        (0x17, b'i'),
        (0x18, b'o'),
        (0x19, b'p'),
        (0x1A, b'['),
        (0x1B, b']'),
        (0x1C, b'\r'),
        (0x1E, b'a'),
        (0x1F, b's'),
        (0x20, b'd'),
        (0x21, b'f'),
        (0x22, b'g'),
        (0x23, b'h'),
        (0x24, b'j'),
        (0x25, b'k'),
        (0x26, b'l'),
        (0x27, b';'),
        (0x28, b'\''),
        (0x29, b'`'),
        (0x2B, b'\\'),
        (0x2C, b'z'),
        (0x2D, b'x'),
        (0x2E, b'c'),
        (0x2F, b'v'),
        (0x30, b'b'),
        (0x31, b'n'),
        (0x32, b'm'),
        (0x33, b','),
        (0x34, b'.'),
        (0x35, b'/'),
        (0x39, b' '),
    ];
    let mut i = 0;
    while i < m.len() {
        a[m[i].0] = m[i].1;
        i += 1;
    }
    a
}
const fn build_shift() -> [u8; 0x40] {
    let mut a = [0u8; 0x40];
    a[0x01] = 0x1B;
    let m: &[(usize, u8)] = &[
        (0x02, b'!'),
        (0x03, b'@'),
        (0x04, b'#'),
        (0x05, b'$'),
        (0x06, b'%'),
        (0x07, b'^'),
        (0x08, b'&'),
        (0x09, b'*'),
        (0x0A, b'('),
        (0x0B, b')'),
        (0x0C, b'_'),
        (0x0D, b'+'),
        (0x0E, 0x08),
        (0x0F, b'\t'),
        (0x10, b'Q'),
        (0x11, b'W'),
        (0x12, b'E'),
        (0x13, b'R'),
        (0x14, b'T'),
        (0x15, b'Y'),
        (0x16, b'U'),
        (0x17, b'I'),
        (0x18, b'O'),
        (0x19, b'P'),
        (0x1A, b'{'),
        (0x1B, b'}'),
        (0x1C, b'\r'),
        (0x1E, b'A'),
        (0x1F, b'S'),
        (0x20, b'D'),
        (0x21, b'F'),
        (0x22, b'G'),
        (0x23, b'H'),
        (0x24, b'J'),
        (0x25, b'K'),
        (0x26, b'L'),
        (0x27, b':'),
        (0x28, b'"'),
        (0x29, b'~'),
        (0x2B, b'|'),
        (0x2C, b'Z'),
        (0x2D, b'X'),
        (0x2E, b'C'),
        (0x2F, b'V'),
        (0x30, b'B'),
        (0x31, b'N'),
        (0x32, b'M'),
        (0x33, b'<'),
        (0x34, b'>'),
        (0x35, b'?'),
        (0x39, b' '),
    ];
    let mut i = 0;
    while i < m.len() {
        a[m[i].0] = m[i].1;
        i += 1;
    }
    a
}
static KBD_SCAN_TO_ASCII_US: [u8; 0x40] = build_us();
static KBD_SCAN_TO_ASCII_SHIFT: [u8; 0x40] = build_shift();

fn kbd_update_shift_flags(scancode: u8) {
    unsafe {
        let make = scancode & 0x80 == 0;
        let base = scancode & 0x7F;
        let flags = memb_raw_ptr(0x40, 0x17);
        match base {
            0x2A => {
                if make {
                    *flags |= 0x02
                } else {
                    *flags &= !0x02
                }
            } // LShift
            0x36 => {
                if make {
                    *flags |= 0x01
                } else {
                    *flags &= !0x01
                }
            } // RShift
            0x1D => {
                if make {
                    *flags |= 0x04
                } else {
                    *flags &= !0x04
                }
            } // Ctrl
            0x38 => {
                if make {
                    *flags |= 0x08
                } else {
                    *flags &= !0x08
                }
            } // Alt
            0x3A => {
                if make {
                    *flags ^= 0x40
                }
            } // CapsLock
            0x45 => {
                if make {
                    *flags ^= 0x20
                }
            } // NumLock
            0x46 => {
                if make {
                    *flags ^= 0x10
                }
            } // ScrollLock
            _ => {}
        }
    }
}

fn scancode_to_ascii(scancode: u8) -> u8 {
    if scancode >= 0x40 {
        return 0;
    }
    unsafe {
        let flags = *memb_raw_ptr(0x40, 0x17);
        let shift = flags & 0x03 != 0;
        let caps = flags & 0x40 != 0;
        let ctrl = flags & 0x04 != 0;
        let base = KBD_SCAN_TO_ASCII_US[scancode as usize];
        if ctrl {
            if base.is_ascii_lowercase() {
                return base - b'a' + 1;
            }
            return base;
        }
        if base.is_ascii_lowercase() {
            return if shift ^ caps {
                KBD_SCAN_TO_ASCII_SHIFT[scancode as usize]
            } else {
                base
            };
        }
        if shift {
            KBD_SCAN_TO_ASCII_SHIFT[scancode as usize]
        } else {
            base
        }
    }
}

#[no_mangle]
pub extern "C" fn kbd_bios_deposit_from_isr() {
    unsafe {
        let st = &mut *k();
        let mut ascii;
        let scancode;
        if st.pending_bios_valid != 0 {
            ascii = st.pending_bios_ascii;
            scancode = st.pending_bios_scancode;
            st.pending_bios_valid = 0;
        } else if st.scancode_ready != 0 {
            ascii = st.ascii;
            scancode = st.scancode;
            st.last_scancode = scancode;
            kbd_consume();
        } else {
            return;
        }
        // The 0xE0 extended-key prefix is not a keystroke: the real BIOS
        // remembers it and decodes the *following* byte in its light (same
        // scan code for the grey and keypad variants), so the prefix itself
        // is dropped here. E0-prefixed 2A/AA are the keyboard's NumLock
        // fake-shift FRAMING around grey-key events — pure wire protocol,
        // never a shift-state change and never a buffered keystroke.
        static mut PREV_WAS_E0: bool = false;
        if scancode == 0xE0 {
            PREV_WAS_E0 = true;
            return;
        }
        let was_ext = PREV_WAS_E0;
        PREV_WAS_E0 = false;
        if was_ext && matches!(scancode & 0x7F, 0x2A | 0x36) {
            return; // fake shift make/break: framing only
        }
        kbd_update_shift_flags(scancode);
        if ascii == 0 && kbd_is_break_scancode(scancode) == 0 {
            ascii = scancode_to_ascii(scancode);
        }
        kbd_bios_push(ascii, scancode);
    }
}

#[no_mangle]
pub extern "C" fn shim_keyboard_enqueue(ascii: u8) {
    let ascii = if ascii == b'\n' { b'\r' } else { ascii };
    unsafe {
        let sc = ascii_to_scan(ascii);
        if sc != 0 {
            snapshot_on_key_event(b'P' as c_char, sc);
        }
        kbd_enqueue(ascii, sc);
    }
}

#[no_mangle]
pub extern "C" fn shim_keyboard_enqueue_scancode_press(scancode: u8) {
    if scancode == 0 {
        return;
    }
    let scancode = scancode & 0x7F;
    unsafe {
        snapshot_on_key_event(b'P' as c_char, scancode);
    }
    kbd_queue_push(0, scancode);
}

#[no_mangle]
pub extern "C" fn shim_keyboard_enqueue_scancode_release(scancode: u8) {
    if scancode == 0 {
        return;
    }
    unsafe {
        snapshot_on_key_event(b'R' as c_char, scancode & 0x7F);
    }
    kbd_queue_push(0, scancode | 0x80);
}

// Extended (grey-cluster) keys — set-1 scancodes prefixed with 0xE0 on real
// hardware: the dedicated cursor arrows, Ins/Del/Home/End/PgUp/PgDn, grey
// Enter and grey '/'. The prefix byte and the code byte each arrive as their
// own IRQ1, exactly like a real 8042 stream; INT9 handlers that track E0
// (e.g. DM's IBMIO distinguishes grey-Up from keypad-8) see the distinction,
// and handlers that ignore unknown bytes still act on the plain code that
// follows — the same compatibility contract as real hardware.
//
// NumLock framing: with NumLock ON (the real AT boot default, and ours — see
// init_bios_data_area), the keyboard wraps grey-key events in a FAKE LEFT
// SHIFT so BIOS-era software decodes the key as a cursor key, not a keypad
// digit: make = `E0 2A, E0 sc`, break = `E0 sc|80, E0 AA`. Games key on this
// exact stream — DM's IBMIO ignores a bare `E0 48` (NumLock-off framing) but
// moves the party on the NumLock-on framing, verified live.

/// NumLock state from the BIOS keyboard flag byte (0040:0017 bit 5).
fn kbd_numlock_on() -> bool {
    unsafe { *memb_raw_ptr(0x40, 0x17) & 0x20 != 0 }
}

#[no_mangle]
pub extern "C" fn shim_keyboard_enqueue_scancode_press_ext(scancode: u8) {
    if scancode == 0 {
        return;
    }
    let scancode = scancode & 0x7F;
    unsafe {
        snapshot_on_key_event(b'P' as c_char, scancode);
    }
    if kbd_numlock_on() {
        kbd_queue_push(0, 0xE0);
        kbd_queue_push(0, 0x2A); // fake LShift make
    }
    kbd_queue_push(0, 0xE0);
    kbd_queue_push(0, scancode);
}

#[no_mangle]
pub extern "C" fn shim_keyboard_enqueue_scancode_release_ext(scancode: u8) {
    if scancode == 0 {
        return;
    }
    unsafe {
        snapshot_on_key_event(b'R' as c_char, scancode & 0x7F);
    }
    kbd_queue_push(0, 0xE0);
    kbd_queue_push(0, (scancode & 0x7F) | 0x80);
    if kbd_numlock_on() {
        kbd_queue_push(0, 0xE0);
        kbd_queue_push(0, 0xAA); // fake LShift break
    }
}

#[no_mangle]
pub extern "C" fn shim_keyboard_enqueue_scancode(scancode: u8) {
    shim_keyboard_enqueue_scancode_press(scancode);
    shim_keyboard_enqueue_scancode_release(scancode);
}
