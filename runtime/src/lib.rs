//! saisei-runtime — the game runtime: machine loop, JIT registry, DOS/BIOS/HW
//! shims, SDL display.
//!
//! Built as an rlib (linked into the `saisei-game` binary) and a cdylib (the
//! shim unit tests dlopen isolated copies). Everything crosses module and JIT
//! boundaries over a C ABI (`#[no_mangle] extern "C"`): the rustc-compiled JIT
//! chunks resolve these symbols from the -rdynamic host binary at dlopen.
//! Ported from the original C runtime — see docs/runtime_port_notes.md for the
//! deliberate deviations.

#![feature(c_variadic)]
// `#[linkage = "weak"]` for the default `game_config` (shims.rs): the runtime is
// self-contained (the shim-test harness dlopens it alone), and the generated
// per-game config's STRONG `game_config` overrides the weak default at the real
// game link. Mirrors the original C runtime's weak default.
#![feature(linkage)]
#![allow(clippy::missing_safety_doc)]
// dos.rs imports the variadic C `save_manager_sr_log` (its true signature); sdl.rs
// imports the same symbol with a simplified non-variadic signature. The two extern
// declarations legitimately clash — silence the cosmetic cross-module lint here.
#![allow(clashing_extern_declarations)]

/// Where C keeps `errno`: glibc spells the accessor `__errno_location`, macOS
/// spells it `__error`. Every module reads and writes errno through this name.
#[cfg(target_os = "macos")]
#[inline(always)]
pub fn errno_loc() -> *mut core::ffi::c_int {
    unsafe { libc::__error() }
}
#[cfg(not(target_os = "macos"))]
#[inline(always)]
pub fn errno_loc() -> *mut core::ffi::c_int {
    unsafe { libc::__errno_location() }
}

pub mod audio;
pub mod bios;
pub mod cpu;
pub mod devices;
pub mod dos;
pub mod ems;
pub mod io_bus;
pub mod keyboard;
pub mod mouse;
pub mod save_manager;
pub mod sdl;
pub mod shims;
pub mod snapshot;
pub mod timer;
pub mod vga_mem;
pub mod video;
pub mod xms;
