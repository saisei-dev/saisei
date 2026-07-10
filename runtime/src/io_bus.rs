//! Port of `runtime/hw/io_bus.c` — the generic IO-port device bus.
//!
//! A small registry of devices; the IO-port dispatch (inb/outb in shims.c) looks
//! a port up here before falling back to legacy in-core handling. Devices
//! self-register from constructors at process start. ABI-identical to device.h.

use core::ffi::c_char;

/// The canonical `IoDevice` layout (was device.h; must stay layout-identical:
/// devices are declared as static `IoDevice` in C TUs and passed by pointer).
#[repr(C)]
pub struct IoDevice {
    pub name: *const c_char,
    /// Claimed byte IO ports, terminated by 0xFFFF.
    pub ports: *const u16,
    pub read8: Option<extern "C" fn(port: u16) -> u8>,
    pub write8: Option<extern "C" fn(port: u16, value: u8)>,
}

// Devices are registered once at startup and only read thereafter; the raw
// pointers/fn pointers make it !Sync by default, so promise Sync for statics.
unsafe impl Sync for IoDevice {}

const IO_BUS_MAX_DEVICES: usize = 32;

static mut IO_DEVICES: [*const IoDevice; IO_BUS_MAX_DEVICES] =
    [core::ptr::null(); IO_BUS_MAX_DEVICES];
static mut IO_DEVICE_COUNT: usize = 0;

/// Register a device (called from a C device TU's constructor).
#[no_mangle]
pub extern "C" fn io_bus_register(dev: *const IoDevice) {
    unsafe {
        let count = core::ptr::addr_of!(IO_DEVICE_COUNT).read();
        if !dev.is_null() && count < IO_BUS_MAX_DEVICES {
            let devs = core::ptr::addr_of_mut!(IO_DEVICES) as *mut *const IoDevice;
            devs.add(count).write(dev);
            core::ptr::addr_of_mut!(IO_DEVICE_COUNT).write(count + 1);
        }
    }
}

/// The device claiming `port`, or NULL if none (caller falls back to legacy).
#[no_mangle]
pub extern "C" fn io_bus_lookup(port: u16) -> *const IoDevice {
    unsafe {
        let count = core::ptr::addr_of!(IO_DEVICE_COUNT).read();
        let devs = core::ptr::addr_of!(IO_DEVICES) as *const *const IoDevice;
        for i in 0..count {
            let dev = devs.add(i).read();
            if dev.is_null() {
                continue;
            }
            let mut p = (*dev).ports;
            while !p.is_null() && *p != 0xFFFF {
                if *p == port {
                    return dev;
                }
                p = p.add(1);
            }
        }
        core::ptr::null()
    }
}
