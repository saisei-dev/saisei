/* The generic IO-port device bus (see scripts/include/device.h). A small
 * registry of devices; the IO-port dispatch looks a port up here before
 * falling back to the legacy in-core handling. Registration happens from
 * device constructors at process start; the registry is a plain
 * zero-initialised array so there is no constructor-ordering dependency. */
#include "device.h"
#include <stddef.h>

#define IO_BUS_MAX_DEVICES 32

static const IoDevice *io_devices[IO_BUS_MAX_DEVICES];
static int io_device_count;

void io_bus_register(const IoDevice *dev) {
  if (dev != NULL && io_device_count < IO_BUS_MAX_DEVICES) {
    io_devices[io_device_count++] = dev;
  }
}

const IoDevice *io_bus_lookup(uint16_t port) {
  for (int i = 0; i < io_device_count; ++i) {
    const IoDevice *dev = io_devices[i];
    for (const uint16_t *p = dev->ports; *p != 0xFFFF; ++p) {
      if (*p == port) {
        return dev;
      }
    }
  }
  return NULL;
}
