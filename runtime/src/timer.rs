//! Port of `runtime/hw/timer.c` — PIT (8254) state + the virtual clock.
//!
//! The clock primitive (scaled monotonic / virtual-now) and the vclock state
//! machine (RUNNING/HALTED/STEPPING, driven by the control-FIFO for deterministic
//! replay) live here, plus the BIOS timer-tick increment (IRQ0). PIT register
//! ports 0x40-0x43 stay in shims.c's inb/outb and use the extern state defined
//! here. Timing logic is unchanged so replay determinism is preserved.

use core::ffi::c_char;

extern "C" {
    fn shim_host_monotonic_ns() -> u64;
    fn memw_raw_read(seg: u16, off: u16) -> u16;
    fn memw_raw_write(seg: u16, off: u16, value: u16);
    fn shim_log_stdout(fmt: *const c_char, ...);
    static emulation_speedup: f64;
    static host_time_origin_ns: u64;
}

/// The canonical `PITState` layout (was timer.h — FROZEN for snapshots).
#[repr(C)]
pub struct PITState {
    pub reload: u32,
    pub temp_reload: u16,
    pub expect_high: u8,
    pub access_mode: u8,
}

const fn pit_default() -> PITState {
    PITState {
        reload: 65536,
        temp_reload: 0,
        expect_high: 0,
        access_mode: 3,
    }
}

#[no_mangle]
pub static mut pit_cycle_accum: u64 = 0;
#[no_mangle]
pub static mut pit_cycle_fraction_accum: u64 = 0;
#[no_mangle]
pub static mut pit: PITState = pit_default();
#[no_mangle]
pub static mut pit_channel1: PITState = pit_default();
#[no_mangle]
pub static mut pit_channel2: PITState = pit_default();
#[no_mangle]
pub static mut pit_reload_value: u32 = 0x10000;
#[no_mangle]
pub static mut pit_latched_value: u16 = 0;
#[no_mangle]
pub static mut pit_latch_valid: u8 = 0;
#[no_mangle]
pub static mut pit_read_buffer: u16 = 0;
#[no_mangle]
pub static mut pit_read_expect_high: u8 = 0;
#[no_mangle]
pub static mut pit_read_buffer_is_latch: u8 = 0;
#[no_mangle]
pub static mut bios_timer_tick_backlog: u32 = 0;
#[no_mangle]
pub static mut bios_timer_tick_preincremented: u8 = 0;

// vclock_state_t enum values (int-sized, matching timer.h).
const VCLOCK_RUNNING: i32 = 0;
const VCLOCK_HALTED: i32 = 1;
const VCLOCK_STEPPING: i32 = 2;

#[no_mangle]
pub static mut vclock_state: i32 = VCLOCK_RUNNING;
#[no_mangle]
pub static mut vclock_paused_offset_ns: u64 = 0;
#[no_mangle]
pub static mut vclock_frozen_virtual_ns: u64 = 0;
#[no_mangle]
pub static mut vclock_step_deadline_virtual_ns: u64 = 0;

const BIOS_TICKS_PER_DAY: u32 = 0x1800B0;

#[no_mangle]
pub extern "C" fn shim_virtual_now_ns() -> u64 {
    unsafe {
        if vclock_state == VCLOCK_HALTED {
            return vclock_frozen_virtual_ns;
        }
        let wall = shim_host_monotonic_ns();
        let mut virt = wall - vclock_paused_offset_ns;
        if vclock_state == VCLOCK_STEPPING && virt > vclock_step_deadline_virtual_ns {
            virt = vclock_step_deadline_virtual_ns;
        }
        virt
    }
}

#[no_mangle]
pub extern "C" fn vclock_service() {
    unsafe {
        if vclock_state != VCLOCK_STEPPING {
            return;
        }
        let wall = shim_host_monotonic_ns();
        let virt = wall - vclock_paused_offset_ns;
        if virt >= vclock_step_deadline_virtual_ns {
            vclock_frozen_virtual_ns = vclock_step_deadline_virtual_ns;
            vclock_state = VCLOCK_HALTED;
            shim_log_stdout(
                c"[VCLOCK] step complete, halted virtual_ns=%llu\n".as_ptr(),
                vclock_frozen_virtual_ns,
            );
        }
    }
}

#[no_mangle]
pub extern "C" fn vclock_halt() {
    unsafe {
        if vclock_state == VCLOCK_HALTED {
            return;
        }
        vclock_frozen_virtual_ns = shim_virtual_now_ns();
        vclock_state = VCLOCK_HALTED;
        shim_log_stdout(
            c"[VCLOCK] halted virtual_ns=%llu wall_ns=%llu\n".as_ptr(),
            vclock_frozen_virtual_ns,
            shim_host_monotonic_ns(),
        );
    }
}

#[no_mangle]
pub extern "C" fn vclock_resume() {
    unsafe {
        if vclock_state == VCLOCK_RUNNING {
            return;
        }
        let wall = shim_host_monotonic_ns();
        let anchor = if vclock_state == VCLOCK_HALTED {
            vclock_frozen_virtual_ns
        } else {
            vclock_step_deadline_virtual_ns
        };
        vclock_paused_offset_ns = wall - anchor;
        vclock_state = VCLOCK_RUNNING;
        shim_log_stdout(
            c"[VCLOCK] resumed virtual_ns=%llu wall_ns=%llu\n".as_ptr(),
            anchor,
            wall,
        );
    }
}

#[no_mangle]
pub extern "C" fn vclock_step(ticks: u32) {
    unsafe {
        let ticks = if ticks == 0 { 1 } else { ticks };
        let ns_per_tick = (54925000.0 / emulation_speedup) as u64;
        let ns = ticks as u64 * ns_per_tick;
        let wall = shim_host_monotonic_ns();
        let base_virtual = if vclock_state == VCLOCK_HALTED {
            let b = vclock_frozen_virtual_ns;
            vclock_paused_offset_ns = wall - b;
            b
        } else {
            wall - vclock_paused_offset_ns
        };
        vclock_step_deadline_virtual_ns = base_virtual + ns;
        vclock_state = VCLOCK_STEPPING;
        shim_log_stdout(
            c"[VCLOCK] step ticks=%u deadline_virtual_ns=%llu\n".as_ptr(),
            ticks,
            vclock_step_deadline_virtual_ns,
        );
    }
}

#[no_mangle]
pub extern "C" fn shim_scaled_monotonic_ns() -> u64 {
    unsafe {
        let now_ns = shim_virtual_now_ns();
        let elapsed_ns = now_ns - host_time_origin_ns;
        let scaled_elapsed_ns = (elapsed_ns as f64 * emulation_speedup) as u64;
        host_time_origin_ns + scaled_elapsed_ns
    }
}

#[no_mangle]
pub extern "C" fn bios_timer_increment() {
    unsafe {
        let mut ticks: u32 = memw_raw_read(0x40, 0x006C) as u32;
        ticks |= (memw_raw_read(0x40, 0x006E) as u32) << 16;
        ticks += 1;
        if ticks >= BIOS_TICKS_PER_DAY {
            ticks -= BIOS_TICKS_PER_DAY;
            memw_raw_write(0x40, 0x0070, 1);
        }
        memw_raw_write(0x40, 0x006C, (ticks & 0xFFFF) as u16);
        memw_raw_write(0x40, 0x006E, (ticks >> 16) as u16);
    }
}

#[no_mangle]
pub extern "C" fn pit_current_count() -> u16 {
    unsafe {
        let raw_reload: u32 = if pit_reload_value != 0 {
            pit_reload_value
        } else {
            0x10000
        };
        let effective_reload: u32 = if pit.reload != 0 { pit.reload } else { 1 };

        let mut cycles: u64 = pit_cycle_accum;
        if cycles >= effective_reload as u64 {
            cycles %= effective_reload as u64;
        }

        let mut raw_cycles: u64 = cycles;
        if raw_reload != effective_reload {
            raw_cycles = (cycles * raw_reload as u64) / effective_reload as u64;
        }
        if raw_cycles >= raw_reload as u64 {
            raw_cycles %= raw_reload as u64;
        }

        if raw_reload == 0 {
            return 0;
        }

        let remaining: u32 = if raw_reload as u64 > raw_cycles {
            raw_reload - raw_cycles as u32
        } else {
            0
        };
        (remaining & 0xFFFF) as u16
    }
}
