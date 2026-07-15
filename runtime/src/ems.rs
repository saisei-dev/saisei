//! The EMS (LIM Expanded Memory) driver — the bank-switched page frame this
//! machine boots with, alongside the flat XMS pool (see `xms.rs`).
//!
//! A 1993/94 DOS game that wants memory beyond the 640K asks for it one of two
//! ways: XMS, the flat extended memory above the 1MB line, or **EMS**, the
//! bank-switched memory of an expansion card seen through a 64K *page frame* in
//! the upper memory area. The Elder Scrolls: Arena requires EMS — with none it
//! prints "Not enough EMS" and quits — and it is the only faithful way to give
//! it what it needs, because unlike extended memory (which is just the RAM that
//! is already here), expanded memory is genuinely *off-CPU*: the guest cannot
//! address it directly, only the four 16K pages currently *mapped* into the
//! frame. That off-CPU-ness is the model, not an inconvenience:
//!
//! * The EMS pool is a separate host buffer, invisible to the guest except
//!   through the page frame at `FRAME_SEG`. Mapping a logical page copies it into
//!   the frame; the next remap of that physical slot copies whatever the guest
//!   wrote back out first. The guest sees exactly the bank-switch hardware would
//!   show it — the mapped page's bytes, and its own writes preserved across a
//!   remap — and nothing it cannot reach.
//! * A guest detects it exactly as it detects EMM386: INT 67h's vector points at
//!   a device header whose name field (offset 0x0A) reads "EMMXXXX0", and the
//!   handler answers the LIM control functions in AH with status back in AH
//!   (0 = success). Both are installed at boot in `shims.rs`.

use crate::cpu::*;
use crate::devices::{pod_capture, pod_restore};
use crate::shims::virtual_memory;

/// One EMS page is 16 KB — the LIM unit for every count and every map.
const PAGE_SIZE: u32 = 0x4000;
/// The page frame: four contiguous physical pages (64K) in the upper memory
/// area, the window through which the guest reaches expanded memory. `shims.rs`
/// reports this same segment from INT 67h AH=41h.
pub const FRAME_SEG: u16 = 0xE000;
const FRAME_LINEAR: u32 = (FRAME_SEG as u32) << 4;
const PHYS_PAGES: usize = 4;
/// Total expanded memory this board carries: 512 pages × 16K = 8 MB, plenty for
/// Arena's world data and comfortably inside the 8 MB guest RAM the frame lives
/// in without touching the conventional/XMS regions (the pool is a separate host
/// allocation).
const TOTAL_PAGES: u16 = 512;
/// Handles a guest may hold at once. Handle 0 is the OS handle in the LIM model
/// and is never handed out; index i names handle i.
const MAX_HANDLES: usize = 64;

// EMS status codes (returned in AH), as the LIM spec names them.
const EMS_OK: u8 = 0x00;
const EMS_ERR_NO_HANDLES: u8 = 0x85;
const EMS_ERR_INVALID_HANDLE: u8 = 0x83;
const EMS_ERR_UNDEFINED_FN: u8 = 0x84;
const EMS_ERR_TOTAL_EXCEEDED: u8 = 0x87;
const EMS_ERR_NOT_ENOUGH: u8 = 0x88;
const EMS_ERR_LOGICAL_OUT_OF_RANGE: u8 = 0x8A;
const EMS_ERR_ILLEGAL_PHYSICAL: u8 = 0x8B;

#[derive(Clone, Copy)]
struct Handle {
    /// 0 = free. index i (1..) is the guest's name for this handle.
    used: u8,
    /// Number of logical pages this handle owns.
    count: u16,
    /// logical page -> pool page index. Only `count` entries are meaningful.
    pages: [u16; TOTAL_PAGES as usize],
    /// The saved page-map for INT 67h AH=47h (restored by AH=48h). -1 = empty.
    saved_map: [i32; PHYS_PAGES],
    saved_valid: u8,
}

struct EmsState {
    handles: [Handle; MAX_HANDLES],
    /// Which handle owns each pool page (0 = free — handle 0 is never a real
    /// owner, so 0 doubles as "free").
    page_used: [u8; TOTAL_PAGES as usize],
    /// What each physical frame page currently holds: -1 = unmapped, else
    /// ((handle as i32) << 16) | (logical as i32).
    frame_map: [i32; PHYS_PAGES],
    /// The off-CPU expanded-memory pool, malloc'd on first use.
    pool: *mut u8,
}

const EMPTY_HANDLE: Handle = Handle {
    used: 0,
    count: 0,
    pages: [0; TOTAL_PAGES as usize],
    saved_map: [-1; PHYS_PAGES],
    saved_valid: 0,
};

static mut EMS: EmsState = EmsState {
    handles: [EMPTY_HANDLE; MAX_HANDLES],
    page_used: [0; TOTAL_PAGES as usize],
    frame_map: [-1; PHYS_PAGES],
    pool: core::ptr::null_mut(),
};

#[inline]
unsafe fn st() -> *mut EmsState {
    core::ptr::addr_of_mut!(EMS)
}

/// Ensure the pool buffer exists (8 MB, zeroed). Lazy so a game that never
/// touches EMS pays nothing.
unsafe fn ensure_pool() {
    if (*st()).pool.is_null() {
        let bytes = TOTAL_PAGES as usize * PAGE_SIZE as usize;
        let p = libc::calloc(1, bytes) as *mut u8;
        (*st()).pool = p;
    }
}

#[inline]
unsafe fn pool_page_ptr(idx: u16) -> *mut u8 {
    (*st()).pool.add(idx as usize * PAGE_SIZE as usize)
}

#[inline]
unsafe fn frame_page_ptr(phys: usize) -> *mut u8 {
    virtual_memory.add((FRAME_LINEAR + phys as u32 * PAGE_SIZE) as usize)
}

#[inline]
fn encode_map(handle: u16, logical: u16) -> i32 {
    ((handle as i32) << 16) | (logical as i32)
}

/// Count of unallocated pool pages.
unsafe fn free_pages() -> u16 {
    let mut n = 0u16;
    for i in 0..TOTAL_PAGES as usize {
        if (*st()).page_used[i] == 0 {
            n += 1;
        }
    }
    n
}

/// Copy whatever the guest wrote into physical frame page `phys` back to the
/// pool page it currently holds, so a remap does not lose it.
unsafe fn copy_back(phys: usize) {
    let m = (*st()).frame_map[phys];
    if m < 0 {
        return;
    }
    let handle = (m >> 16) as u16;
    let logical = (m & 0xFFFF) as u16;
    if let Some(pool_idx) = pool_index_of(handle, logical) {
        libc::memcpy(
            pool_page_ptr(pool_idx) as *mut libc::c_void,
            frame_page_ptr(phys) as *const libc::c_void,
            PAGE_SIZE as usize,
        );
    }
}

/// The pool page backing (handle, logical), or None if out of range.
unsafe fn pool_index_of(handle: u16, logical: u16) -> Option<u16> {
    let hi = handle as usize;
    if hi == 0 || hi >= MAX_HANDLES || (*st()).handles[hi].used == 0 {
        return None;
    }
    if logical >= (*st()).handles[hi].count {
        return None;
    }
    Some((*st()).handles[hi].pages[logical as usize])
}

/// Map logical page of handle into physical frame page `phys` (or unmap when
/// logical == 0xFFFF, a LIM 4.0 convenience). Returns a status code.
unsafe fn map_page(phys: usize, logical: u16, handle: u16) -> u8 {
    if phys >= PHYS_PAGES {
        return EMS_ERR_ILLEGAL_PHYSICAL;
    }
    if logical == 0xFFFF {
        copy_back(phys);
        (*st()).frame_map[phys] = -1;
        return EMS_OK;
    }
    let hi = handle as usize;
    if hi == 0 || hi >= MAX_HANDLES || (*st()).handles[hi].used == 0 {
        return EMS_ERR_INVALID_HANDLE;
    }
    if logical >= (*st()).handles[hi].count {
        return EMS_ERR_LOGICAL_OUT_OF_RANGE;
    }
    // Already mapped here? Nothing to do (avoids a needless copy-back/in).
    let want = encode_map(handle, logical);
    if (*st()).frame_map[phys] == want {
        return EMS_OK;
    }
    copy_back(phys);
    let pool_idx = (*st()).handles[hi].pages[logical as usize];
    libc::memcpy(
        frame_page_ptr(phys) as *mut libc::c_void,
        pool_page_ptr(pool_idx) as *const libc::c_void,
        PAGE_SIZE as usize,
    );
    (*st()).frame_map[phys] = want;
    EMS_OK
}

/// Allocate `count` pages to a fresh handle. Returns (handle, status).
unsafe fn alloc_handle(count: u16) -> (u16, u8) {
    if count > TOTAL_PAGES {
        return (0, EMS_ERR_TOTAL_EXCEEDED);
    }
    if count > free_pages() {
        return (0, EMS_ERR_NOT_ENOUGH);
    }
    // Find a free handle slot (1..).
    let mut hi = 0usize;
    for i in 1..MAX_HANDLES {
        if (*st()).handles[i].used == 0 {
            hi = i;
            break;
        }
    }
    if hi == 0 {
        return (0, EMS_ERR_NO_HANDLES);
    }
    // Claim `count` free pool pages, zeroing each (a fresh allocation reads back
    // as zero, and this keeps a save deterministic).
    let mut assigned = 0u16;
    for p in 0..TOTAL_PAGES {
        if assigned == count {
            break;
        }
        if (*st()).page_used[p as usize] == 0 {
            (*st()).page_used[p as usize] = hi as u8;
            (*st()).handles[hi].pages[assigned as usize] = p;
            libc::memset(pool_page_ptr(p) as *mut libc::c_void, 0, PAGE_SIZE as usize);
            assigned += 1;
        }
    }
    (*st()).handles[hi].used = 1;
    (*st()).handles[hi].count = count;
    (*st()).handles[hi].saved_valid = 0;
    (hi as u16, EMS_OK)
}

/// Free a handle: unmap any frame page holding it, return its pool pages.
unsafe fn free_handle(handle: u16) -> u8 {
    let hi = handle as usize;
    if hi == 0 || hi >= MAX_HANDLES || (*st()).handles[hi].used == 0 {
        return EMS_ERR_INVALID_HANDLE;
    }
    // Drop mappings pointing at this handle (no copy-back — the memory is gone).
    for phys in 0..PHYS_PAGES {
        let m = (*st()).frame_map[phys];
        if m >= 0 && (m >> 16) as u16 == handle {
            (*st()).frame_map[phys] = -1;
        }
    }
    let count = (*st()).handles[hi].count;
    for l in 0..count {
        let p = (*st()).handles[hi].pages[l as usize];
        (*st()).page_used[p as usize] = 0;
    }
    (*st()).handles[hi] = EMPTY_HANDLE;
    EMS_OK
}

unsafe fn used_handle_count() -> u16 {
    let mut n = 0u16;
    for i in 1..MAX_HANDLES {
        if (*st()).handles[i].used != 0 {
            n += 1;
        }
    }
    n
}

/// INT 67h entry. Reads the function from AH, leaves status in AH.
pub unsafe fn control() {
    ensure_pool();
    let fn_no = ah();
    match fn_no {
        // 40h — Get manager status.
        0x40 => set_ah(EMS_OK),
        // 41h — Get page frame segment.
        0x41 => {
            set_bx(FRAME_SEG);
            set_ah(EMS_OK);
        }
        // 42h — Get number of pages: BX = free, DX = total.
        0x42 => {
            set_bx(free_pages());
            set_dx(TOTAL_PAGES);
            set_ah(EMS_OK);
        }
        // 43h — Allocate BX pages, hand back a handle in DX.
        0x43 => {
            let (handle, status) = alloc_handle(bx());
            if status == EMS_OK {
                set_dx(handle);
            }
            set_ah(status);
        }
        // 44h — Map handle DX's logical page BX into physical page AL.
        0x44 => {
            let status = map_page(al() as usize, bx(), dx());
            set_ah(status);
        }
        // 45h — Deallocate handle DX.
        0x45 => set_ah(free_handle(dx())),
        // 46h — Get EMM version (BCD). 0x32 = LIM 3.2.
        0x46 => {
            set_al(0x32);
            set_ah(EMS_OK);
        }
        // 47h — Save the current page map, tagged by handle DX.
        0x47 => {
            let hi = dx() as usize;
            if hi == 0 || hi >= MAX_HANDLES || (*st()).handles[hi].used == 0 {
                set_ah(EMS_ERR_INVALID_HANDLE);
            } else {
                (*st()).handles[hi].saved_map = (*st()).frame_map;
                (*st()).handles[hi].saved_valid = 1;
                set_ah(EMS_OK);
            }
        }
        // 48h — Restore the page map saved for handle DX. Re-mapping through
        // map_page keeps the frame's contents consistent with the registers.
        0x48 => {
            let hi = dx() as usize;
            if hi == 0 || hi >= MAX_HANDLES || (*st()).handles[hi].used == 0 {
                set_ah(EMS_ERR_INVALID_HANDLE);
            } else {
                let saved = (*st()).handles[hi].saved_map;
                for phys in 0..PHYS_PAGES {
                    let m = saved[phys];
                    if m < 0 {
                        copy_back(phys);
                        (*st()).frame_map[phys] = -1;
                    } else {
                        map_page(phys, (m & 0xFFFF) as u16, (m >> 16) as u16);
                    }
                }
                set_ah(EMS_OK);
            }
        }
        // 4Bh — Get number of open handles in BX.
        0x4B => {
            set_bx(used_handle_count());
            set_ah(EMS_OK);
        }
        // 4Ch — Get pages owned by handle DX.
        0x4C => {
            let hi = dx() as usize;
            if hi == 0 || hi >= MAX_HANDLES || (*st()).handles[hi].used == 0 {
                set_ah(EMS_ERR_INVALID_HANDLE);
            } else {
                set_bx((*st()).handles[hi].count);
                set_ah(EMS_OK);
            }
        }
        // 4Dh — Get pages owned by all handles: write (handle, pages) pairs to
        // ES:DI, count in BX.
        0x4D => {
            let mut di_off = di();
            let mut n = 0u16;
            for i in 1..MAX_HANDLES {
                if (*st()).handles[i].used != 0 {
                    crate::shims::memw_raw_write(es(), di_off, i as u16);
                    crate::shims::memw_raw_write(
                        es(),
                        di_off.wrapping_add(2),
                        (*st()).handles[i].count,
                    );
                    di_off = di_off.wrapping_add(4);
                    n += 1;
                }
            }
            set_bx(n);
            set_ah(EMS_OK);
        }
        // 4Eh — Get/set page map (context save/restore across an interrupt). The
        // "map" here is the four frame registers, serialized to/from a guest
        // buffer. Subfunction in AL.
        0x4E => {
            match al() {
                // 00h — save the mapping registers to ES:DI.
                0x00 => {
                    save_map_regs(es(), di());
                    set_ah(EMS_OK);
                }
                // 01h — restore from DS:SI.
                0x01 => {
                    restore_map_regs(ds(), si());
                    set_ah(EMS_OK);
                }
                // 02h — save current to ES:DI, then restore from DS:SI.
                0x02 => {
                    save_map_regs(es(), di());
                    restore_map_regs(ds(), si());
                    set_ah(EMS_OK);
                }
                // 03h — size of the save area, in AL.
                0x03 => {
                    set_al((PHYS_PAGES * 4) as u8);
                    set_ah(EMS_OK);
                }
                _ => set_ah(EMS_ERR_UNDEFINED_FN),
            }
        }
        // 51h — Reallocate handle DX to BX pages (grow or shrink).
        0x51 => {
            let status = realloc_handle(dx(), bx());
            if status == EMS_OK {
                set_bx((*st()).handles[dx() as usize].count);
            }
            set_ah(status);
        }
        // Anything else: an EMS function this board does not implement. Report it
        // honestly rather than pretending success.
        _ => set_ah(EMS_ERR_UNDEFINED_FN),
    }
    crate::shims::shim_log_stdout(
        c"Trace: ems INT67 AH=0x%02X -> ah=0x%02X bx=0x%04X dx=0x%04X\n".as_ptr(),
        fn_no as core::ffi::c_uint,
        ah() as core::ffi::c_uint,
        bx() as core::ffi::c_uint,
        dx() as core::ffi::c_uint,
    );
}

/// Serialize the four frame map registers (handle, logical) to guest memory.
unsafe fn save_map_regs(seg: u16, off: u16) {
    for phys in 0..PHYS_PAGES {
        let m = (*st()).frame_map[phys];
        let (handle, logical) = if m < 0 {
            (0xFFFFu16, 0xFFFFu16)
        } else {
            ((m >> 16) as u16, (m & 0xFFFF) as u16)
        };
        let base = off.wrapping_add((phys * 4) as u16);
        crate::shims::memw_raw_write(seg, base, handle);
        crate::shims::memw_raw_write(seg, base.wrapping_add(2), logical);
    }
}

/// Restore the four frame map registers from guest memory, re-mapping so the
/// frame contents follow.
unsafe fn restore_map_regs(seg: u16, off: u16) {
    for phys in 0..PHYS_PAGES {
        let base = off.wrapping_add((phys * 4) as u16);
        let handle = crate::shims::memw_raw_read(seg, base);
        let logical = crate::shims::memw_raw_read(seg, base.wrapping_add(2));
        if handle == 0xFFFF || logical == 0xFFFF {
            copy_back(phys);
            (*st()).frame_map[phys] = -1;
        } else {
            map_page(phys, logical, handle);
        }
    }
}

/// Grow or shrink a handle to `new_count` pages, preserving existing content.
unsafe fn realloc_handle(handle: u16, new_count: u16) -> u8 {
    let hi = handle as usize;
    if hi == 0 || hi >= MAX_HANDLES || (*st()).handles[hi].used == 0 {
        return EMS_ERR_INVALID_HANDLE;
    }
    if new_count > TOTAL_PAGES {
        return EMS_ERR_TOTAL_EXCEEDED;
    }
    let cur = (*st()).handles[hi].count;
    if new_count == cur {
        return EMS_OK;
    }
    if new_count < cur {
        // Release the tail pages.
        for l in new_count..cur {
            let p = (*st()).handles[hi].pages[l as usize];
            (*st()).page_used[p as usize] = 0;
        }
        (*st()).handles[hi].count = new_count;
        return EMS_OK;
    }
    // Grow: need (new_count - cur) more free pages.
    let need = new_count - cur;
    if need > free_pages() {
        return EMS_ERR_NOT_ENOUGH;
    }
    let mut added = 0u16;
    for p in 0..TOTAL_PAGES {
        if added == need {
            break;
        }
        if (*st()).page_used[p as usize] == 0 {
            (*st()).page_used[p as usize] = hi as u8;
            (*st()).handles[hi].pages[(cur + added) as usize] = p;
            libc::memset(pool_page_ptr(p) as *mut libc::c_void, 0, PAGE_SIZE as usize);
            added += 1;
        }
    }
    (*st()).handles[hi].count = new_count;
    EMS_OK
}

/// True once any EMS allocation exists (unused by detection — the guest checks
/// the device header — but handy for diagnostics).
pub unsafe fn installed() -> bool {
    true
}

// ---------------------------------------------------------------------------
// Snapshot. Expanded memory is off-CPU, so unlike XMS its *contents* are NOT in
// the linear image — the pool, the handle map and the frame registers all live
// here and a fresh restore process would forget them. The block carries the
// bookkeeping plus the bytes of every allocated pool page (the mapped-in frame
// pages ride in the linear image and are copied back on restore's first remap).
// ---------------------------------------------------------------------------

#[repr(C)]
#[derive(Clone, Copy)]
struct EmsSnapHandle {
    used: u8,
    count: u16,
    pages: [u16; TOTAL_PAGES as usize],
    saved_map: [i32; PHYS_PAGES],
    saved_valid: u8,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct EmsSnapHead {
    frame_map: [i32; PHYS_PAGES],
    handle_count: u16,
    _pad: u16,
}

pub unsafe fn state_capture(out: &mut Vec<u8>) {
    // Nothing allocated and nothing mapped → no block worth writing.
    if (*st()).pool.is_null() && (*st()).frame_map == [-1; PHYS_PAGES] && used_handle_count() == 0 {
        return;
    }
    let mut head: EmsSnapHead = core::mem::zeroed();
    head.frame_map = (*st()).frame_map;
    head.handle_count = MAX_HANDLES as u16;
    pod_capture(&head, out);
    for i in 0..MAX_HANDLES {
        let mut h: EmsSnapHandle = core::mem::zeroed();
        h.used = (*st()).handles[i].used;
        h.count = (*st()).handles[i].count;
        h.pages = (*st()).handles[i].pages;
        h.saved_map = (*st()).handles[i].saved_map;
        h.saved_valid = (*st()).handles[i].saved_valid;
        pod_capture(&h, out);
    }
    // page_used table, then the bytes of every allocated pool page.
    out.extend_from_slice(&(*st()).page_used);
    if !(*st()).pool.is_null() {
        for p in 0..TOTAL_PAGES {
            if (*st()).page_used[p as usize] != 0 {
                let bytes =
                    core::slice::from_raw_parts(pool_page_ptr(p) as *const u8, PAGE_SIZE as usize);
                out.extend_from_slice(bytes);
            }
        }
    }
}

pub unsafe fn state_restore(b: &[u8]) -> bool {
    let head_len = size_of::<EmsSnapHead>();
    let handle_len = size_of::<EmsSnapHandle>();
    if b.len() < head_len + MAX_HANDLES * handle_len + TOTAL_PAGES as usize {
        return false;
    }
    let head: EmsSnapHead = match pod_restore(&b[..head_len]) {
        Some(v) => v,
        None => return false,
    };
    if head.handle_count as usize != MAX_HANDLES {
        return false;
    }
    ensure_pool();
    (*st()).frame_map = head.frame_map;
    let mut off = head_len;
    for i in 0..MAX_HANDLES {
        let h: EmsSnapHandle = match pod_restore(&b[off..off + handle_len]) {
            Some(v) => v,
            None => return false,
        };
        off += handle_len;
        (*st()).handles[i].used = h.used;
        (*st()).handles[i].count = h.count;
        (*st()).handles[i].pages = h.pages;
        (*st()).handles[i].saved_map = h.saved_map;
        (*st()).handles[i].saved_valid = h.saved_valid;
    }
    (*st())
        .page_used
        .copy_from_slice(&b[off..off + TOTAL_PAGES as usize]);
    off += TOTAL_PAGES as usize;
    for p in 0..TOTAL_PAGES {
        if (*st()).page_used[p as usize] != 0 {
            if off + PAGE_SIZE as usize > b.len() {
                return false;
            }
            libc::memcpy(
                pool_page_ptr(p) as *mut libc::c_void,
                b[off..].as_ptr() as *const libc::c_void,
                PAGE_SIZE as usize,
            );
            off += PAGE_SIZE as usize;
        }
    }
    true
}
