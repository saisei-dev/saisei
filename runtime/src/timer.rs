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

// ---- instruction-driven virtual clock ---------------------------------------
//
// Virtual time advances with RETIRED GUEST WORK, not the host clock:
// `virtual_now_accum_ns += consumed × jit_ns_per_instr` at every safepoint
// slow-path visit. The unit of `consumed` is a budget unit: each basic block
// debits its summed per-class instruction weights (≈ 386 cycle count / 3.3;
// insn_weight in codegen.rs — mul/div/string/memory ops cost multiples of a
// register mov), and rep iterations/transfer shims debit their real costs.
// This models a fixed-speed CPU with per-class cycle costs: a calibration
// loop always measures the same work-per-PIT-tick ratio regardless of host
// stalls (JIT compiles, scheduler hiccups), and the PIT / BIOS tick chain
// derives from the same flow of time the program itself experiences. Pacing
// back to real time happens in the safepoint slow path (sleep while virtual
// is ahead of host × emulation_speedup) — never here.
//
// The emulated CPU speed: 40ns/unit ≈ a 486-class machine (2.5× the original
// 33MHz-386DX model of 100ns/unit), period-correct for the late end of this
// corpus and 4×-faster load/decompress phases. CONSTRAINT: the modeled speed
// must not exceed the raw sustained host throughput of dense translated code,
// or virtual time — and every PIT-paced game timer with it — falls behind
// real time in gameplay-heavy loops (pacing only ever sleeps when virtual
// runs AHEAD; it cannot speed up a lagging guest). Measured 2026-07-10 after
// the dispatcher/SS-ring/lifecycle/reg-cache rework: worst-case raw (Zeliard
// attract, the historical 0.80× phase) sustains ~37M units/s, so 25M units/s
// keeps a ~32% margin; 25ns/unit (40M units/s) would exceed it. Re-measure
// that phase before lowering this further.
#[no_mangle]
pub static mut jit_ns_per_instr: u64 = 40;
/// Instructions the running chunks may retire before the next safepoint
/// slow-path visit. Decremented by JIT_BUDGET() in every emitted basic block;
/// refilled (and the consumed count folded into virtual time) by
/// `safe_point_impl`.
#[no_mangle]
pub static mut jit_instr_budget: i64 = 0;
/// What `jit_instr_budget` was set to at the last refill (so consumed =
/// refill − budget).
#[no_mangle]
pub static mut jit_budget_last_refill: i64 = 0;
/// Total retired-instruction count (diagnostics / MIPS measurement).
#[no_mangle]
pub static mut jit_total_retired: u64 = 0;
/// The instruction-driven virtual clock. Seeded from the host monotonic clock
/// at startup so virtual and host share an epoch; advanced only by
/// `vclock_advance_ns` below.
#[no_mangle]
pub static mut virtual_now_accum_ns: u64 = 0;

const BIOS_TICKS_PER_DAY: u32 = 0x1800B0;
pub const NS_PER_BIOS_TICK: u64 = 54_925_000;

/// Advance the instruction-driven clock (RUNNING/STEPPING only; while HALTED
/// the flow of virtual time is frozen even though instructions may retire).
#[no_mangle]
pub extern "C" fn vclock_advance_ns(ns: u64) {
    unsafe {
        if vclock_state != VCLOCK_HALTED {
            virtual_now_accum_ns += ns;
        }
    }
}

#[no_mangle]
pub extern "C" fn shim_virtual_now_ns() -> u64 {
    unsafe {
        match vclock_state {
            VCLOCK_HALTED => vclock_frozen_virtual_ns,
            VCLOCK_STEPPING if virtual_now_accum_ns > vclock_step_deadline_virtual_ns => {
                vclock_step_deadline_virtual_ns
            }
            _ => virtual_now_accum_ns,
        }
    }
}

#[no_mangle]
pub extern "C" fn vclock_service() {
    unsafe {
        if vclock_state != VCLOCK_STEPPING {
            return;
        }
        if virtual_now_accum_ns >= vclock_step_deadline_virtual_ns {
            vclock_frozen_virtual_ns = vclock_step_deadline_virtual_ns;
            virtual_now_accum_ns = vclock_step_deadline_virtual_ns;
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
        let anchor = if vclock_state == VCLOCK_HALTED {
            vclock_frozen_virtual_ns
        } else {
            vclock_step_deadline_virtual_ns
        };
        virtual_now_accum_ns = anchor;
        vclock_state = VCLOCK_RUNNING;
        shim_log_stdout(
            c"[VCLOCK] resumed virtual_ns=%llu wall_ns=%llu\n".as_ptr(),
            anchor,
            shim_host_monotonic_ns(),
        );
    }
}

#[no_mangle]
pub extern "C" fn vclock_step(ticks: u32) {
    unsafe {
        let ticks = if ticks == 0 { 1 } else { ticks };
        // Deadline is in VIRTUAL ns: a BIOS tick is always 54.925ms of game
        // time (emulation_speedup only changes how fast virtual time passes
        // relative to the host, which the pacing sleep handles).
        let ns = ticks as u64 * NS_PER_BIOS_TICK;
        let base_virtual = if vclock_state == VCLOCK_HALTED {
            let b = vclock_frozen_virtual_ns;
            virtual_now_accum_ns = b;
            b
        } else {
            virtual_now_accum_ns
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
