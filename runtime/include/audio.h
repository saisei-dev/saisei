#ifndef RUNTIME_HW_AUDIO_H
#define RUNTIME_HW_AUDIO_H

#include <stdint.h>

/* ============================ hw/audio ============================
 * OPL2 (AdLib / Yamaha YM3812) FM sound card. The game reaches it only
 * through IO ports 0x388 (address/status) and 0x389 (data), so the whole
 * device is one struct + two port entry points; the IO-port dispatcher in
 * shims.c routes 0x388/0x389 here. State is grouped so save/restore can
 * serialise it as a single unit (see ShimRuntimeState.opl2). */
typedef struct {
  uint8_t address;
  uint8_t registers[256];
  uint8_t status;
  uint64_t busy_until_us;
  uint64_t timer1_expire_us;
  uint64_t timer2_expire_us;
} Opl2State;

extern Opl2State opl2;

/* The device registers itself on the IO-port bus (see device.h); its port
 * handlers are internal. Opl2State is exposed only so the snapshot
 * aggregate (ShimRuntimeState) can embed it. */

#endif /* RUNTIME_HW_AUDIO_H */
