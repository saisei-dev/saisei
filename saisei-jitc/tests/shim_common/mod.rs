//! Foundation for the ported C-shim tests (Rust equivalents of the FFI-driven
//! once, then hands each test an isolated dlopen'd copy so global C state (`cpu`,
//! `virtual_memory`, …) doesn't leak between tests.
#![allow(dead_code, non_snake_case)]

use libloading::{Library, Symbol};
use std::io::Read;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;

/// Repo root (…/saisei), derived from this crate's manifest dir.
pub fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .canonicalize()
        .unwrap()
}

use std::sync::{Mutex, MutexGuard};
static SHIM_LOCK: Mutex<()> = Mutex::new(());

/// Serialize shim tests within a test binary: SAISEI_VERBOSE (process env,
/// read at dlopen) and stdout fd (capture_stdout) are process-global, so every
/// shim test must hold this guard for its whole body. Poison-tolerant (an
/// assert-panic in another test must not wedge the rest).
/// #[test] fn my_test() { let _g = shim_common::guard(); ... }
pub fn guard() -> MutexGuard<'static, ()> {
    SHIM_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// 8086 register (union { u16 x; struct{u8 l,h;} }). Same 2-byte layout as u16.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Register16 {
    pub x: u16,
}
impl Register16 {
    pub fn l(&self) -> u8 {
        (self.x & 0xff) as u8
    }
    pub fn h(&self) -> u8 {
        (self.x >> 8) as u8
    }
}

/// cpu.flags — matches runtime/include/cpu_state.h EXACTLY (includes PF, which
/// the old the original test struct omitted).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Flags {
    pub CF: u8,
    pub PF: u8,
    pub ZF: u8,
    pub SF: u8,
    pub OF: u8,
    pub IF: u8,
    pub DF: u8,
}

/// The `cpu` global — mirrors CPUState in runtime/include/cpu_state.h.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct CpuState {
    pub r_ax: Register16,
    pub r_bx: Register16,
    pub r_cx: Register16,
    pub r_dx: Register16,
    pub si: u16,
    pub di: u16,
    pub bp: u16,
    pub sp: u16,
    pub r_ip: u16,
    pub r_cs: u16,
    pub r_ds: u16,
    pub r_es: u16,
    pub r_ss: u16,
    pub flags: Flags,
}

/// Build the runtime shim .so once; return the cached path. Uses the exact gcc
fn cached_so() -> &'static PathBuf {
    static SO: OnceLock<PathBuf> = OnceLock::new();
    SO.get_or_init(|| {
        let root = repo_root();
        let out = std::env::temp_dir().join(format!("saisei_shims_base_{}.so", std::process::id()));
        let status = std::process::Command::new("gcc")
            .current_dir(&root)
            .args([
                "-shared",
                "-fPIC",
                "-Iruntime/include",
                "runtime/core/shims.c",
                "runtime/core/save_manager.c",
                "runtime/core/snapshot.c",
                "runtime/hw/io_bus.c",
                "runtime/hw/audio.c",
                "runtime/hw/video.c",
                "runtime/hw/keyboard.c",
                "runtime/hw/timer.c",
                "runtime/os/dos.c",
                "runtime/os/bios.c",
                "runtime/os/mouse.c",
                "-o",
            ])
            .arg(&out)
            .status()
            .expect("spawn gcc");
        assert!(status.success(), "gcc failed to build runtime shims");
        out
    })
}

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// A dlopen'd, isolated copy of the runtime shims.
pub struct ShimLib {
    pub lib: Library,
    path: PathBuf,
}

impl ShimLib {
    /// Load a fresh, isolated shim instance. Set SAISEI_VERBOSE before this if
    /// the test asserts on the diagnostic stdout log (the .so reads it in an
    /// init constructor at dlopen). Returns (repo_root, ShimLib) to mirror the
    /// the original `_build_shims` return shape.
    pub fn load() -> ShimLib {
        let base = cached_so();
        let id = COUNTER.fetch_add(1, Ordering::SeqCst);
        let path =
            std::env::temp_dir().join(format!("saisei_shims_{}_{}.so", std::process::id(), id));
        std::fs::copy(base, &path).expect("copy shim .so");
        // A distinct file path => a distinct mapping => isolated C globals.
        let lib = unsafe { Library::new(&path).expect("dlopen shim .so") };
        ShimLib { lib, path }
    }

    /// Writable pointer to the ADDRESS of a data symbol (scalar OR struct):
    /// works for `uint16_t psp_seg;`, `CPUState cpu;`, etc. libloading refuses a
    /// non-pointer `Symbol<T>`, so we request `Symbol<*mut T>` and take the
    /// symbol's own raw address (dlsym result), which for a data symbol is
    /// `&variable`. For a pointer-valued global (`uint8_t *virtual_memory;`),
    /// this yields `&virtual_memory` — use `virtual_memory()` to get the value.
    pub unsafe fn global_ptr<T>(&self, name: &str) -> *mut T {
        let sym: Symbol<*mut T> = self
            .lib
            .get(name.as_bytes())
            .unwrap_or_else(|e| panic!("symbol {name}: {e}"));
        sym.try_as_raw_ptr().expect("symbol has a raw address") as *mut T
    }

    /// Read a Copy scalar/struct data global by value.
    pub unsafe fn read_global<T: Copy>(&self, name: &str) -> T {
        *self.global_ptr::<T>(name)
    }

    /// Pointer to the `cpu` CPUState global.
    pub unsafe fn cpu(&self) -> *mut CpuState {
        self.global_ptr::<CpuState>("cpu")
    }

    /// Base pointer of the `virtual_memory` global (value of the `uint8_t *`):
    /// dereference the pointer variable at `&virtual_memory`.
    pub unsafe fn virtual_memory(&self) -> *mut u8 {
        *self.global_ptr::<*mut u8>("virtual_memory")
    }

    /// Read `count` bytes of linear memory starting at `lin`.
    pub unsafe fn read_mem(&self, lin: usize, count: usize) -> Vec<u8> {
        let base = self.virtual_memory();
        (0..count).map(|i| *base.add(lin + i)).collect()
    }
    pub unsafe fn read_u16(&self, lin: usize) -> u16 {
        let b = self.read_mem(lin, 2);
        u16::from_le_bytes([b[0], b[1]])
    }
    pub unsafe fn write_mem(&self, lin: usize, bytes: &[u8]) {
        let base = self.virtual_memory();
        for (i, &b) in bytes.iter().enumerate() {
            *base.add(lin + i) = b;
        }
    }

    /// Fetch a C function symbol as a typed fn pointer.
    /// Usage: `let f = lib.func::<unsafe extern "C" fn(u16, u16)>("outb");`
    pub unsafe fn func<T: Copy>(&self, name: &str) -> T {
        let sym: Symbol<T> = self
            .lib
            .get(name.as_bytes())
            .unwrap_or_else(|e| panic!("fn {name}: {e}"));
        *sym
    }
}

impl Drop for ShimLib {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Capture everything the C side writes to fd 1 (stdout) while `f` runs. Used by
/// tests that assert on the SAISEI_VERBOSE diagnostic log.
pub fn capture_stdout<F: FnOnce()>(f: F) -> String {
    unsafe {
        libc::fflush(std::ptr::null_mut());
    }
    let id = COUNTER.fetch_add(1, Ordering::SeqCst);
    let path = std::env::temp_dir().join(format!("saisei_cap_{}_{}.log", std::process::id(), id));
    let file = std::fs::File::create(&path).expect("create capture file");
    let saved = unsafe { libc::dup(1) };
    unsafe {
        libc::dup2(file.as_raw_fd(), 1);
    }
    f();
    unsafe {
        libc::fflush(std::ptr::null_mut());
        libc::dup2(saved, 1);
        libc::close(saved);
    }
    drop(file);
    let mut s = String::new();
    std::fs::File::open(&path)
        .unwrap()
        .read_to_string(&mut s)
        .unwrap();
    let _ = std::fs::remove_file(&path);
    s
}

/// Set SAISEI_VERBOSE=1 for the duration of a closure that loads a shim. Env
/// mutation is process-global, so shim tests must run single-threaded
/// (`--test-threads=1`), which they do (see the ported shim test files).
pub fn with_verbose<T, F: FnOnce() -> T>(f: F) -> T {
    std::env::set_var("SAISEI_VERBOSE", "1");
    let r = f();
    std::env::remove_var("SAISEI_VERBOSE");
    r
}
