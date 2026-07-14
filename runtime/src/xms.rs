//! The XMS driver — the extended-memory manager this machine boots with.
//!
//! A 1993 DOS game that wants memory beyond the 640K asks for it one of two ways:
//! EMS, the bank-switched page frame of an expansion card, or XMS, the flat
//! extended memory above the 1MB line that a 286+ has by virtue of being a 286+.
//! There is no EMS board plugged into this machine and saying so is honest. But
//! extended memory is not a board — it is the RAM that is *already here*
//! (see `MEMORY_SIZE`, and `mask_addr` already gates the 1MB line on A20) — and
//! the only reason a program could not reach it was that nothing was installed to
//! hand it out. That is what a memory manager is, and this is it.
//!
//! So this is not a shim standing in for a driver: it is the driver, implementing
//! the XMS 3.0 control function against the memory the machine actually has. A
//! guest detects it exactly as it detects HIMEM.SYS — INT 2Fh AX=4300h answers
//! AL=80h, AX=4310h hands back a far entry point — and calls it exactly as it
//! calls HIMEM.SYS: a far call with the function in AH, AX=1 back for success and
//! AX=0 with an error code in BL for failure.
//!
//! Two consequences of "it is the real RAM" are worth stating, because both are
//! what make this faithful rather than convenient:
//!
//! * A locked block's linear address (function 0Ch) is a **true** linear address
//!   into that RAM. A guest that locks a block, turns A20 on and reads it through
//!   a 32-bit offset finds its bytes there, because they *are* there. Nothing is
//!   translated behind its back.
//! * The block contents live in guest RAM, which the snapshot already carries. So
//!   only the *bookkeeping* — who owns which paragraphs, and the HMA and A20
//!   claims — needs a device block of its own (`XMSM`), for the same reason a DOS
//!   file handle needed one: the guest holds a handle number, and the thing the
//!   number means is host state that a fresh process would otherwise forget.

use crate::cpu::*;
use crate::devices::{pod_capture, pod_restore};
use crate::shims::{a20_set_enabled, virtual_memory, SHIM_MEMORY_SIZE};

/// The High Memory Area: the first 64K above the 1MB line, which is reachable
/// from real mode as FFFF:0010..FFFF:FFFF once A20 is open. It is claimed as a
/// unit by function 01h and is *not* part of the block pool — handing the same
/// paragraphs out twice is precisely what a memory manager exists to prevent.
const HMA_BASE: u32 = 0x0010_0000;
const HMA_SIZE: u32 = 0x0001_0000;

/// Extended-memory blocks come from the RAM above the HMA.
const EMB_BASE: u32 = HMA_BASE + HMA_SIZE;

/// HIMEM.SYS defaults to 32 handles (/NUMHANDLES), and a guest can exhaust them:
/// running out is a real, reportable condition (error 0xA1), not an impossibility.
const MAX_HANDLES: usize = 32;

// XMS error codes, as the spec names them.
const XMS_ERR_HMA_IN_USE: u8 = 0x91;
const XMS_ERR_HMA_NOT_ALLOCATED: u8 = 0x93;
const XMS_ERR_NO_HANDLES: u8 = 0xA1;
const XMS_ERR_INVALID_HANDLE: u8 = 0xA2;
const XMS_ERR_INVALID_SRC_HANDLE: u8 = 0xA3;
const XMS_ERR_INVALID_SRC_OFFSET: u8 = 0xA4;
const XMS_ERR_INVALID_DST_HANDLE: u8 = 0xA5;
const XMS_ERR_INVALID_DST_OFFSET: u8 = 0xA6;
const XMS_ERR_INVALID_LENGTH: u8 = 0xA7;
const XMS_ERR_BLOCK_NOT_LOCKED: u8 = 0xAA;
const XMS_ERR_BLOCK_LOCKED: u8 = 0xAB;
const XMS_ERR_LOCK_COUNT_OVERFLOW: u8 = 0xAC;
const XMS_ERR_OUT_OF_MEMORY: u8 = 0xA0;
const XMS_ERR_NO_UMBS: u8 = 0xB1;
const XMS_ERR_INVALID_UMB_SEG: u8 = 0xB2;

#[repr(C)]
#[derive(Clone, Copy)]
struct Handle {
    /// 0 = free. A handle is the guest's name for a block; index+1 is the name.
    used: u8,
    lock_count: u8,
    /// Linear base of the block. Meaningful only while `used`.
    base: u32,
    size_kb: u16,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct XmsState {
    handles: [Handle; MAX_HANDLES],
    hma_allocated: u8,
    /// Function 05h/06h nest: A20 stays open until the last local enable is
    /// matched by its disable, which is why they are counted and not a flag.
    a20_local_count: u8,
}

static mut XMS: XmsState = XmsState {
    handles: [Handle {
        used: 0,
        lock_count: 0,
        base: 0,
        size_kb: 0,
    }; MAX_HANDLES],
    hma_allocated: 0,
    a20_local_count: 0,
};

#[inline]
fn st() -> *mut XmsState {
    core::ptr::addr_of_mut!(XMS)
}

/// The block pool: every byte of RAM above the HMA.
fn pool_size_kb() -> u32 {
    let top = SHIM_MEMORY_SIZE as u32;
    if top <= EMB_BASE {
        return 0;
    }
    (top - EMB_BASE) / 1024
}

// ---------------------------------------------------------------------------
// Allocation. First fit over the gaps between live blocks, which is what a
// handle table without a free list can honestly offer.
// ---------------------------------------------------------------------------

/// Find a base for `size_kb` KB, or None if the pool cannot seat it.
unsafe fn find_gap(size_kb: u32) -> Option<u32> {
    // The live blocks, by base. MAX_HANDLES is 32, so an insertion sort here is
    // cheaper than any structure that would avoid it.
    let mut live: [(u32, u32); MAX_HANDLES] = [(0, 0); MAX_HANDLES];
    let mut n = 0usize;
    for h in (*st()).handles.iter() {
        if h.used != 0 {
            live[n] = (h.base, h.size_kb as u32 * 1024);
            n += 1;
        }
    }
    live[..n].sort_unstable_by_key(|e| e.0);

    let want = size_kb * 1024;
    let top = SHIM_MEMORY_SIZE as u32;
    let mut cursor = EMB_BASE;
    for &(base, len) in live[..n].iter() {
        if base >= cursor && base - cursor >= want {
            return Some(cursor);
        }
        let end = base + len;
        if end > cursor {
            cursor = end;
        }
    }
    if top >= cursor && top - cursor >= want {
        return Some(cursor);
    }
    None
}

/// Largest free run, and the total free — the two numbers function 08h reports,
/// and they are not the same number whenever the pool is fragmented.
unsafe fn free_kb() -> (u32, u32) {
    let mut live: [(u32, u32); MAX_HANDLES] = [(0, 0); MAX_HANDLES];
    let mut n = 0usize;
    for h in (*st()).handles.iter() {
        if h.used != 0 {
            live[n] = (h.base, h.size_kb as u32 * 1024);
            n += 1;
        }
    }
    live[..n].sort_unstable_by_key(|e| e.0);

    let top = SHIM_MEMORY_SIZE as u32;
    let mut largest = 0u32;
    let mut total = 0u32;
    let mut cursor = EMB_BASE;
    for &(base, len) in live[..n].iter() {
        if base > cursor {
            let gap = base - cursor;
            total += gap;
            if gap > largest {
                largest = gap;
            }
        }
        let end = base + len;
        if end > cursor {
            cursor = end;
        }
    }
    if top > cursor {
        let gap = top - cursor;
        total += gap;
        if gap > largest {
            largest = gap;
        }
    }
    (largest / 1024, total / 1024)
}

unsafe fn handle_mut(h: u16) -> Option<&'static mut Handle> {
    if h == 0 || h as usize > MAX_HANDLES {
        return None;
    }
    let e = &mut (*st()).handles[h as usize - 1];
    if e.used == 0 {
        return None;
    }
    Some(e)
}

unsafe fn free_handle_count() -> u8 {
    (*st())
        .handles
        .iter()
        .filter(|h| h.used == 0)
        .count()
        .min(255) as u8
}

// ---------------------------------------------------------------------------
// Guest memory, read as the guest laid it out.
// ---------------------------------------------------------------------------

unsafe fn read_u16(seg: u16, off: u16) -> u16 {
    let lo = *seg_off(seg, off) as u16;
    let hi = *seg_off(seg, off.wrapping_add(1)) as u16;
    lo | (hi << 8)
}

unsafe fn read_u32(seg: u16, off: u16) -> u32 {
    read_u16(seg, off) as u32 | ((read_u16(seg, off.wrapping_add(2)) as u32) << 16)
}

fn ok() {
    set_ax(1);
    set_bl(0);
}

fn fail(err: u8) {
    set_ax(0);
    set_bl(err);
}

// ---------------------------------------------------------------------------
// Function 0Bh — move an extended memory block.
//
// The workhorse: it is how a real-mode program gets at extended memory at all,
// since it cannot address it. A handle of 0 means the offset field is not an
// offset but a far pointer into conventional memory, which is the whole reason
// the field is 32 bits wide.
// ---------------------------------------------------------------------------

/// Resolve one side of the move to a linear address, or the error that says why
/// it could not be. `bad_handle`/`bad_offset` differ per side, so they come in.
unsafe fn move_side(
    handle: u16,
    offset: u32,
    len: u32,
    bad_handle: u8,
    bad_offset: u8,
) -> Result<u32, u8> {
    let base = if handle == 0 {
        // A far pointer: segment in the high word, offset in the low. This is a
        // real-mode address computed the real-mode way, and it may legitimately
        // reach above the 1MB line (FFFF:0010 and up is the HMA).
        let seg = (offset >> 16) as u32;
        let off = offset & 0xFFFF;
        (seg << 4) + off
    } else {
        let h = match handle_mut(handle) {
            Some(h) => h,
            None => return Err(bad_handle),
        };
        if offset > h.size_kb as u32 * 1024 {
            return Err(bad_offset);
        }
        h.base + offset
    };
    // The move must land inside the RAM this machine has, whichever side it is.
    let end = base as u64 + len as u64;
    if end > SHIM_MEMORY_SIZE as u64 {
        return Err(bad_offset);
    }
    Ok(base)
}

unsafe fn move_block() {
    let seg = ds();
    let off = si();

    let len = read_u32(seg, off);
    let src_handle = read_u16(seg, off.wrapping_add(4));
    let src_offset = read_u32(seg, off.wrapping_add(6));
    let dst_handle = read_u16(seg, off.wrapping_add(10));
    let dst_offset = read_u32(seg, off.wrapping_add(12));

    // The spec is explicit that the length is even, and HIMEM refuses an odd one.
    // A guest that passes one has a bug we would be hiding by rounding it.
    if len % 2 != 0 {
        fail(XMS_ERR_INVALID_LENGTH);
        return;
    }
    if len == 0 {
        // A zero-length move is legal and does nothing.
        ok();
        return;
    }

    let src = match move_side(
        src_handle,
        src_offset,
        len,
        XMS_ERR_INVALID_SRC_HANDLE,
        XMS_ERR_INVALID_SRC_OFFSET,
    ) {
        Ok(v) => v,
        Err(e) => return fail(e),
    };
    let dst = match move_side(
        dst_handle,
        dst_offset,
        len,
        XMS_ERR_INVALID_DST_HANDLE,
        XMS_ERR_INVALID_DST_OFFSET,
    ) {
        Ok(v) => v,
        Err(e) => return fail(e),
    };

    // A block move, and specified as one: overlap is resolved as if the bytes went
    // through a temporary, so this is `memmove` and never `rep movs` — there is no
    // forward-replication semantics to reproduce here.
    //
    // Note the copy is made against the linear RAM directly and so is indifferent
    // to the A20 gate, exactly as HIMEM's is: the driver opens A20 for the
    // duration of the move and closes it after, and a guest with A20 shut still
    // gets its bytes.
    libc::memmove(
        virtual_memory.add(dst as usize) as *mut libc::c_void,
        virtual_memory.add(src as usize) as *const libc::c_void,
        len as usize,
    );
    ok();
}

// ---------------------------------------------------------------------------
// The control function. Entered by a FAR CALL with the function in AH.
// ---------------------------------------------------------------------------

pub unsafe fn control() {
    let fn_no = ah();
    let arg = dx();
    control_dispatch();
    crate::shims::shim_log_stdout(
        c"Trace: xms AH=0x%02X (dx=0x%04X) -> ax=0x%04X bx=0x%04X dx=0x%04X bl=0x%02X\n".as_ptr(),
        fn_no as core::ffi::c_uint,
        arg as core::ffi::c_uint,
        ax() as core::ffi::c_uint,
        bx() as core::ffi::c_uint,
        dx() as core::ffi::c_uint,
        bl() as core::ffi::c_uint,
    );
}

unsafe fn control_dispatch() {
    match ah() {
        // 00h — Get version. AX = XMS version BCD, BX = driver revision, DX = 1
        // when an HMA exists. It does not use the AX=1/AX=0 convention.
        0x00 => {
            set_ax(0x0300);
            set_bx(0x0001);
            set_dx(1);
        }
        // 01h — Request the HMA.
        0x01 => {
            if (*st()).hma_allocated != 0 {
                fail(XMS_ERR_HMA_IN_USE);
            } else {
                (*st()).hma_allocated = 1;
                ok();
            }
        }
        // 02h — Release the HMA.
        0x02 => {
            if (*st()).hma_allocated == 0 {
                fail(XMS_ERR_HMA_NOT_ALLOCATED);
            } else {
                (*st()).hma_allocated = 0;
                ok();
            }
        }
        // 03h/04h — Global A20. The gate is the machine's, and already modelled.
        0x03 => {
            a20_set_enabled(true);
            ok();
        }
        0x04 => {
            a20_set_enabled(false);
            ok();
        }
        // 05h/06h — Local A20, which nests: the gate closes on the last release.
        0x05 => {
            (*st()).a20_local_count = (*st()).a20_local_count.saturating_add(1);
            a20_set_enabled(true);
            ok();
        }
        0x06 => {
            if (*st()).a20_local_count > 0 {
                (*st()).a20_local_count -= 1;
            }
            if (*st()).a20_local_count == 0 {
                a20_set_enabled(false);
            }
            ok();
        }
        // 07h — Query A20. AX carries the answer, and AX=0 here means "closed",
        // not "failed" — hence BL=0 either way.
        0x07 => {
            set_ax(if crate::shims::a20_enabled { 1 } else { 0 });
            set_bl(0);
        }
        // 08h — Query free extended memory, in KB. Reports the largest run and the
        // total, which differ once the pool is fragmented.
        0x08 => {
            let (largest, total) = free_kb();
            set_ax(largest.min(0xFFFF) as u16);
            set_dx(total.min(0xFFFF) as u16);
            set_bl(if total == 0 { XMS_ERR_OUT_OF_MEMORY } else { 0 });
        }
        // 09h — Allocate an extended memory block of DX KB.
        0x09 => {
            let want = dx() as u32;
            let slot = (*st()).handles.iter().position(|h| h.used == 0);
            let slot = match slot {
                Some(s) => s,
                None => return fail(XMS_ERR_NO_HANDLES),
            };
            let base = match find_gap(want) {
                Some(b) => b,
                None => return fail(XMS_ERR_OUT_OF_MEMORY),
            };
            let h = &mut (*st()).handles[slot];
            h.used = 1;
            h.lock_count = 0;
            h.base = base;
            h.size_kb = want as u16;
            ok();
            set_dx(slot as u16 + 1);
        }
        // 0Ah — Free a block. A locked block is still in use by someone.
        0x0A => {
            let hn = dx();
            match handle_mut(hn) {
                None => fail(XMS_ERR_INVALID_HANDLE),
                Some(h) => {
                    if h.lock_count != 0 {
                        fail(XMS_ERR_BLOCK_LOCKED);
                    } else {
                        h.used = 0;
                        h.base = 0;
                        h.size_kb = 0;
                        ok();
                    }
                }
            }
        }
        // 0Bh — Move.
        0x0B => move_block(),
        // 0Ch — Lock, handing back the block's true 32-bit linear address.
        0x0C => {
            let hn = dx();
            match handle_mut(hn) {
                None => fail(XMS_ERR_INVALID_HANDLE),
                Some(h) => {
                    if h.lock_count == 0xFF {
                        fail(XMS_ERR_LOCK_COUNT_OVERFLOW);
                    } else {
                        h.lock_count += 1;
                        let base = h.base;
                        ok();
                        set_dx((base >> 16) as u16);
                        set_bx((base & 0xFFFF) as u16);
                    }
                }
            }
        }
        // 0Dh — Unlock.
        0x0D => {
            let hn = dx();
            match handle_mut(hn) {
                None => fail(XMS_ERR_INVALID_HANDLE),
                Some(h) => {
                    if h.lock_count == 0 {
                        fail(XMS_ERR_BLOCK_NOT_LOCKED);
                    } else {
                        h.lock_count -= 1;
                        ok();
                    }
                }
            }
        }
        // 0Eh — Get handle information.
        0x0E => {
            let hn = dx();
            let free = free_handle_count();
            match handle_mut(hn) {
                None => fail(XMS_ERR_INVALID_HANDLE),
                Some(h) => {
                    let lock = h.lock_count;
                    let size = h.size_kb;
                    ok();
                    set_bh(lock);
                    set_bl(free);
                    set_dx(size);
                }
            }
        }
        // 0Fh — Reallocate a block to BX KB. Growing it in place is only possible
        // when the run above it is free, so this moves the bytes when it must.
        0x0F => {
            let hn = dx();
            let want = bx() as u32;
            let (old_base, old_kb, locked) = match handle_mut(hn) {
                None => return fail(XMS_ERR_INVALID_HANDLE),
                Some(h) => (h.base, h.size_kb as u32, h.lock_count),
            };
            if locked != 0 {
                return fail(XMS_ERR_BLOCK_LOCKED);
            }
            if want == old_kb {
                return ok();
            }
            if want < old_kb {
                // Shrinking never needs to move anything.
                if let Some(h) = handle_mut(hn) {
                    h.size_kb = want as u16;
                }
                return ok();
            }
            // Free it first so the gap search can consider its own paragraphs,
            // then take a new run and carry the bytes over.
            if let Some(h) = handle_mut(hn) {
                h.used = 0;
            }
            let base = match find_gap(want) {
                Some(b) => b,
                None => {
                    if let Some(h) = handle_mut(hn) {
                        h.used = 1;
                    } else {
                        (*st()).handles[hn as usize - 1].used = 1;
                    }
                    return fail(XMS_ERR_OUT_OF_MEMORY);
                }
            };
            let e = &mut (*st()).handles[hn as usize - 1];
            e.used = 1;
            e.base = base;
            e.size_kb = want as u16;
            if base != old_base && old_kb > 0 {
                libc::memmove(
                    virtual_memory.add(base as usize) as *mut libc::c_void,
                    virtual_memory.add(old_base as usize) as *const libc::c_void,
                    (old_kb * 1024) as usize,
                );
            }
            ok();
        }
        // 10h/11h — Upper memory blocks. There is no UMB provider on this machine
        // (nothing is filling the gaps between the adapter ROMs), and a guest that
        // is told so falls back to conventional memory, which is what it should do.
        0x10 => {
            set_ax(0);
            set_bl(XMS_ERR_NO_UMBS);
            set_dx(0);
        }
        0x11 => {
            set_ax(0);
            set_bl(XMS_ERR_INVALID_UMB_SEG);
        }
        _ => {
            // An unimplemented function is reported as one. XMS says so with
            // AX=0/BL=80h ("function not implemented"), and a guest that asks for
            // something exotic is entitled to hear no and take its other path.
            crate::shims::shim_log_stdout(
                c"Trace: XMS function AH=0x%02X is not implemented by this driver\n".as_ptr(),
                ah() as core::ffi::c_uint,
            );
            set_ax(0);
            set_bl(0x80);
        }
    }
}

/// Reported by INT 2Fh AX=4300h. A driver is here.
pub fn installed() -> bool {
    true
}

/// Total extended memory this machine can hand out, in KB — what a guest sees
/// through function 08h on a machine with nothing allocated yet.
pub fn total_kb() -> u32 {
    pool_size_kb()
}

// ---------------------------------------------------------------------------
// Snapshot. The blocks' *contents* are guest RAM and ride in the linear image;
// what would be lost is who owns which paragraphs — see the module note.
// ---------------------------------------------------------------------------

pub unsafe fn state_capture(out: &mut Vec<u8>) {
    let mut snap: XmsState = core::mem::zeroed();
    snap.handles = (*st()).handles;
    snap.hma_allocated = (*st()).hma_allocated;
    snap.a20_local_count = (*st()).a20_local_count;
    pod_capture(&snap, out);
}

pub unsafe fn state_restore(b: &[u8]) -> bool {
    match pod_restore::<XmsState>(b) {
        Some(v) => {
            (*st()).handles = v.handles;
            (*st()).hma_allocated = v.hma_allocated;
            (*st()).a20_local_count = v.a20_local_count;
            true
        }
        None => false,
    }
}

#[cfg(test)]
pub(crate) unsafe fn reset_for_test() {
    (*st()).handles = [Handle {
        used: 0,
        lock_count: 0,
        base: 0,
        size_kb: 0,
    }; MAX_HANDLES];
    (*st()).hma_allocated = 0;
    (*st()).a20_local_count = 0;
}
