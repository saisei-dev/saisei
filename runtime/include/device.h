#ifndef RUNTIME_DEVICE_H
#define RUNTIME_DEVICE_H

#include <stdint.h>

/* ======================= generic hardware device bus =======================
 * The runtime hosts emulated hardware as pluggable devices instead of a
 * hardcoded inb/outb switch. Each device lives in its own translation unit
 * (scripts/runtime/hw/<dev>.c), owns its state, and declares the byte IO
 * ports it answers to. The IO-port dispatch (inb/outb in shims.c) routes a
 * port to whichever device claims it.
 *
 * Why: this tool reconstructs *many* DOS games, and games target different
 * hardware. Keeping devices behind a uniform interface means a game's
 * machine is *composed* from device modules — to support a game's sound or
 * video card you implement/​link its module, with no edits to the core. A
 * device self-registers at startup via a constructor, so linking its TU is
 * all it takes; per-game composition is just per-game linking (and, later,
 * a per-game device list in the game config).
 */
typedef struct {
  const char *name;
  /* Claimed byte IO ports, terminated by 0xFFFF. */
  const uint16_t *ports;
  uint8_t (*read8)(uint16_t port);
  void (*write8)(uint16_t port, uint8_t value);
} IoDevice;

/* Register a device. Call from a constructor in the device's TU:
 *   __attribute__((constructor)) static void foo_register(void) {
 *       io_bus_register(&foo_device);
 *   }
 */
void io_bus_register(const IoDevice *dev);

/* The device claiming `port`, or NULL if no device handles it (callers fall
 * back to the legacy in-core handling for not-yet-migrated ports). */
const IoDevice *io_bus_lookup(uint16_t port);

#endif /* RUNTIME_DEVICE_H */
