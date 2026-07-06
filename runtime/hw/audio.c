/* ============================ hw/audio ============================
 * OPL2 (AdLib) FM sound card emulation. Extracted from shims.c as part of
 * the hardware-layer separation. The device is driven purely through IO
 * ports 0x388/0x389; shims.c's inb/outb dispatcher forwards those here.
 *
 * Only the register file and timer/busy bookkeeping are modelled — there
 * is no actual FM synthesis (the games poll the timer/busy status bits,
 * which is what this reproduces). Timing is read from the runtime's scaled
 * monotonic clock so it stays consistent with the virtual clock / replay.
 */
#include "audio.h"
#include "device.h"
#include "shims.h"

Opl2State opl2;

#define OPL2_BUSY_DURATION_US 50
#define OPL2_TIMER1_TICK_US 80
#define OPL2_TIMER2_TICK_US 320

static uint8_t opl2_port_read(uint16_t port) {
  if (port == 0x388) {
    uint64_t now_us = shim_scaled_monotonic_ns() / 1000ull;
    if (opl2.timer1_expire_us && now_us >= opl2.timer1_expire_us) {
      opl2.status |= 0x80;
      opl2.timer1_expire_us = 0;
    }
    if (opl2.timer2_expire_us && now_us >= opl2.timer2_expire_us) {
      opl2.status |= 0x40;
      opl2.timer2_expire_us = 0;
    }
    uint8_t status = opl2.status;
    if (now_us < opl2.busy_until_us)
      status |= 0x01; /* busy flag */
    return status;
  }
  /* port == 0x389 */
  return opl2.registers[opl2.address];
}

static void opl2_port_write(uint16_t port, uint8_t value) {
  if (port == 0x388) {
    opl2.address = value;
    opl2.busy_until_us =
        shim_scaled_monotonic_ns() / 1000ull + OPL2_BUSY_DURATION_US;
    return;
  }
  /* port == 0x389 */
  opl2.registers[opl2.address] = value;
  uint64_t now_us = shim_scaled_monotonic_ns() / 1000ull;
  opl2.busy_until_us = now_us + OPL2_BUSY_DURATION_US;
  if (opl2.address == 0x04) {
    if (value & 0x80) {
      opl2.status &= ~0xC0; /* reset timer overflow flags */
      opl2.timer1_expire_us = 0;
      opl2.timer2_expire_us = 0;
    }
    if (value & 0x20)
      opl2.status &= (uint8_t)~0x80; /* mask timer 1 flag */
    if (value & 0x40)
      opl2.status &= (uint8_t)~0x40; /* mask timer 2 flag */
    if (value & 0x01) {
      uint8_t timer_val = opl2.registers[0x02];
      uint64_t delay = (uint64_t)(256 - timer_val) * OPL2_TIMER1_TICK_US;
      if (delay == 0)
        delay = OPL2_TIMER1_TICK_US;
      opl2.timer1_expire_us = now_us + delay;
    } else {
      opl2.timer1_expire_us = 0;
    }
    if (value & 0x02) {
      uint8_t timer_val = opl2.registers[0x03];
      uint64_t delay = (uint64_t)(256 - timer_val) * OPL2_TIMER2_TICK_US;
      if (delay == 0)
        delay = OPL2_TIMER2_TICK_US;
      opl2.timer2_expire_us = now_us + delay;
    } else {
      opl2.timer2_expire_us = 0;
    }
  } else if (opl2.address == 0x02) {
    if (opl2.registers[0x04] & 0x01) {
      uint64_t delay = (uint64_t)(256 - value) * OPL2_TIMER1_TICK_US;
      if (delay == 0)
        delay = OPL2_TIMER1_TICK_US;
      opl2.timer1_expire_us = now_us + delay;
    }
  } else if (opl2.address == 0x03) {
    if (opl2.registers[0x04] & 0x02) {
      uint64_t delay = (uint64_t)(256 - value) * OPL2_TIMER2_TICK_US;
      if (delay == 0)
        delay = OPL2_TIMER2_TICK_US;
      opl2.timer2_expire_us = now_us + delay;
    }
  }
}

/* Register the OPL2 on the IO-port bus. The game reaches the sound card
 * only through these two ports, so this is its whole public surface. */
static const uint16_t opl2_ports[] = {0x388, 0x389, 0xFFFF};
static const IoDevice opl2_device = {
    .name = "opl2",
    .ports = opl2_ports,
    .read8 = opl2_port_read,
    .write8 = opl2_port_write,
};

__attribute__((constructor)) static void opl2_register(void) {
  io_bus_register(&opl2_device);
}
