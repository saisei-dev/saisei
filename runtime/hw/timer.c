/* ============================ hw/timer ============================
 * PIT (8254) + the virtual clock. The clock primitive (scaled monotonic /
 * virtual-now) and the vclock state machine (RUNNING/HALTED/STEPPING, driven
 * by the control-FIFO halt/resume/step opcodes for deterministic replay)
 * live here, along with the BIOS timer-tick increment (IRQ0). PIT register
 * ports 0x40-0x43 stay in shims.c's inb/outb and use the extern state here.
 * This is a faithful extraction -- timing logic is unchanged so replay
 * determinism is preserved. */
#include "timer.h"
#include "shims.h"
#include <stdio.h>

uint64_t pit_cycle_accum;
uint64_t pit_cycle_fraction_accum;
PITState pit = {.reload = 65536, .temp_reload = 0, .expect_high = 0, .access_mode = 3};
PITState pit_channel1 = {
    .reload = 65536, .temp_reload = 0, .expect_high = 0, .access_mode = 3};
PITState pit_channel2 = {
    .reload = 65536, .temp_reload = 0, .expect_high = 0, .access_mode = 3};
uint32_t pit_reload_value = 0x10000;
uint16_t pit_latched_value;
uint8_t pit_latch_valid;
uint16_t pit_read_buffer;
uint8_t pit_read_expect_high;
uint8_t pit_read_buffer_is_latch;
uint32_t bios_timer_tick_backlog;
uint8_t bios_timer_tick_preincremented;

vclock_state_t vclock_state = VCLOCK_RUNNING;
uint64_t vclock_paused_offset_ns;          /* virtual = wall - offset (RUNNING/STEPPING) */
uint64_t vclock_frozen_virtual_ns;         /* virtual time when HALTED */
uint64_t vclock_step_deadline_virtual_ns;  /* clamp target while STEPPING */

uint64_t shim_virtual_now_ns(void) {
  if (vclock_state == VCLOCK_HALTED) {
    return vclock_frozen_virtual_ns;
  }
  uint64_t wall = shim_host_monotonic_ns();
  uint64_t virtual = wall - vclock_paused_offset_ns;
  if (vclock_state == VCLOCK_STEPPING &&
      virtual > vclock_step_deadline_virtual_ns) {
    virtual = vclock_step_deadline_virtual_ns;
  }
  return virtual;
}

/* Transition STEPPING→HALTED when the step deadline is reached. Called
 * from safe_point_impl before any virtual-time reads in that pass. */
void vclock_service(void) {
  if (vclock_state != VCLOCK_STEPPING) return;
  uint64_t wall = shim_host_monotonic_ns();
  uint64_t virtual = wall - vclock_paused_offset_ns;
  if (virtual >= vclock_step_deadline_virtual_ns) {
    vclock_frozen_virtual_ns = vclock_step_deadline_virtual_ns;
    vclock_state = VCLOCK_HALTED;
    shim_log_stdout("[VCLOCK] step complete, halted virtual_ns=%llu\n",
            (unsigned long long)vclock_frozen_virtual_ns);
  }
}

void vclock_halt(void) {
  if (vclock_state == VCLOCK_HALTED) return;
  vclock_frozen_virtual_ns = shim_virtual_now_ns();
  vclock_state = VCLOCK_HALTED;
  shim_log_stdout("[VCLOCK] halted virtual_ns=%llu wall_ns=%llu\n",
          (unsigned long long)vclock_frozen_virtual_ns,
          (unsigned long long)shim_host_monotonic_ns());
}

void vclock_resume(void) {
  if (vclock_state == VCLOCK_RUNNING) return;
  /* Re-anchor offset so virtual_now stays continuous across the
   * transition: after resume, wall - offset must equal the virtual time
   * we were frozen/clamped at. */
  uint64_t wall = shim_host_monotonic_ns();
  uint64_t anchor = (vclock_state == VCLOCK_HALTED)
                        ? vclock_frozen_virtual_ns
                        : vclock_step_deadline_virtual_ns;
  vclock_paused_offset_ns = wall - anchor;
  vclock_state = VCLOCK_RUNNING;
  shim_log_stdout("[VCLOCK] resumed virtual_ns=%llu wall_ns=%llu\n",
          (unsigned long long)anchor, (unsigned long long)wall);
}

void vclock_step(uint32_t ticks) {
  if (ticks == 0) ticks = 1;
  uint64_t ns_per_tick = (uint64_t)(54925000.0 / emulation_speedup);
  uint64_t ns = (uint64_t)ticks * ns_per_tick;
  uint64_t wall = shim_host_monotonic_ns();
  uint64_t base_virtual;
  if (vclock_state == VCLOCK_HALTED) {
    base_virtual = vclock_frozen_virtual_ns;
    vclock_paused_offset_ns = wall - base_virtual;
  } else {
    base_virtual = wall - vclock_paused_offset_ns;
  }
  vclock_step_deadline_virtual_ns = base_virtual + ns;
  vclock_state = VCLOCK_STEPPING;
  shim_log_stdout("[VCLOCK] step ticks=%u deadline_virtual_ns=%llu\n",
          (unsigned)ticks,
          (unsigned long long)vclock_step_deadline_virtual_ns);
}

uint64_t shim_scaled_monotonic_ns(void) {
  const uint64_t now_ns = shim_virtual_now_ns();
  const uint64_t elapsed_ns = now_ns - host_time_origin_ns;
  const uint64_t scaled_elapsed_ns =
      (uint64_t)((double)elapsed_ns * emulation_speedup);
  return host_time_origin_ns + scaled_elapsed_ns;
}

void bios_timer_increment(void) {
  uint32_t ticks = memw_raw_read(0x40, 0x006C);
  ticks |= (uint32_t)memw_raw_read(0x40, 0x006E) << 16;
  ++ticks;
  if (ticks >= BIOS_TICKS_PER_DAY) {
    ticks -= BIOS_TICKS_PER_DAY;
    memw_raw_write(0x40, 0x0070, 1);
  }
  memw_raw_write(0x40, 0x006C, (uint16_t)(ticks & 0xFFFF));
  memw_raw_write(0x40, 0x006E, (uint16_t)(ticks >> 16));
}

uint16_t pit_current_count(void) {
  uint32_t raw_reload = pit_reload_value ? pit_reload_value : 0x10000;
  uint32_t effective_reload = pit.reload ? pit.reload : 1;

  uint64_t cycles = pit_cycle_accum;
  if (cycles >= effective_reload) {
    cycles %= effective_reload;
  }

  uint64_t raw_cycles = cycles;
  if (raw_reload != effective_reload) {
    raw_cycles = (cycles * raw_reload) / effective_reload;
  }
  if (raw_cycles >= raw_reload) {
    raw_cycles %= raw_reload;
  }

  if (raw_reload == 0) {
    return 0;
  }

  uint32_t remaining = (raw_reload > raw_cycles) ?
                            (raw_reload - (uint32_t)raw_cycles) :
                            0;
  return (uint16_t)(remaining & 0xFFFF);
}
