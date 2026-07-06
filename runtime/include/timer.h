#ifndef RUNTIME_HW_TIMER_H
#define RUNTIME_HW_TIMER_H

#include <stdint.h>

/* ============================ hw/timer ============================
 * PIT (8254) state + the virtual clock. The PIT register ports 0x40-0x43
 * stay in shims.c's inb/outb and use the extern state here; the clock
 * primitive + vclock state machine + IRQ0 tick live in timer.c. */
typedef struct {
  uint32_t reload;
  uint16_t temp_reload;
  uint8_t expect_high;
  uint8_t access_mode;
} PITState;

extern PITState pit;
extern PITState pit_channel1;
extern PITState pit_channel2;
extern uint32_t pit_reload_value;
extern uint16_t pit_latched_value;
extern uint8_t pit_latch_valid;
extern uint16_t pit_read_buffer;
extern uint8_t pit_read_expect_high;
extern uint8_t pit_read_buffer_is_latch;
extern uint32_t bios_timer_tick_backlog;
extern uint8_t bios_timer_tick_preincremented;

typedef enum {
  VCLOCK_RUNNING = 0,
  VCLOCK_HALTED,
  VCLOCK_STEPPING,
} vclock_state_t;
extern vclock_state_t vclock_state;
extern uint64_t vclock_paused_offset_ns;
extern uint64_t vclock_frozen_virtual_ns;
extern uint64_t vclock_step_deadline_virtual_ns;
extern uint64_t pit_cycle_fraction_accum;
#define BIOS_TICKS_PER_DAY 0x1800B0u

/* Clock + PIT interface (control loop / IRQ0 / PIT-port consumers). */
uint64_t shim_virtual_now_ns(void);
void vclock_service(void);
void vclock_halt(void);
void vclock_resume(void);
void vclock_step(uint32_t ticks);
void bios_timer_increment(void);
uint16_t pit_current_count(void);

#endif /* RUNTIME_HW_TIMER_H */
