//! Re-execing the player.
//!
//! Two things need it. Loading a save restores into clean host state, so the
//! runtime re-execs itself with `--restore-from` (see `save_manager`); it needs
//! an argv that names the running game, which a GUI launch's bare `saisei`
//! cmdline does not carry. And leaving a game for the library cannot simply
//! return: a game that has run has left the guest's memory, the DOS file table
//! and the JIT registry populated, and there is no honest way to tear all of
//! that down in place — so we start over as a fresh process.

use std::ffi::{c_char, CString};
use std::ptr;

use crate::host::LaunchSpec;

fn exe() -> String {
    std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "saisei".to_string())
}

/// Tell the runtime what argv reproduces this launch, so its save-load re-exec
/// comes back into the same game rather than into the library.
pub fn arm(spec: &LaunchSpec) {
    let args = spec.relaunch_argv(&exe());
    let cstrs: Vec<CString> = args
        .iter()
        .map(|a| CString::new(a.as_str()).expect("nul in argv"))
        .collect();
    let ptrs: Vec<*const c_char> = cstrs.iter().map(|c| c.as_ptr()).collect();
    let argc = ptrs.len() as i32;
    // Both outlive the process: the runtime reads them at re-exec time, which is
    // arbitrarily far from now.
    let ptrs: &'static [*const c_char] = Box::leak(ptrs.into_boxed_slice());
    std::mem::forget(cstrs);
    unsafe { saisei_runtime::save_manager::saisei_set_relaunch_argv(ptrs.as_ptr(), argc) };
}

/// Replace this process with a fresh player. Never returns on success.
///
/// `args` are the player's own arguments (no argv[0]) — empty means "open the
/// library".
pub fn exec_player(args: &[String]) -> ! {
    let exe_path = exe();
    let mut argv_s = vec![exe_path.clone()];
    argv_s.extend(args.iter().cloned());

    let cstrs: Vec<CString> = argv_s
        .iter()
        .map(|a| CString::new(a.as_str()).expect("nul in argv"))
        .collect();
    let mut argv: Vec<*const c_char> = cstrs.iter().map(|c| c.as_ptr()).collect();
    argv.push(ptr::null());

    // The window belongs to this process image; the next one opens its own.
    // Flush first — the re-exec discards our buffered stdio.
    unsafe {
        libc::fflush(ptr::null_mut());
        let path = CString::new(exe_path.as_str()).unwrap();
        libc::execv(path.as_ptr(), argv.as_ptr());
    }
    eprintln!(
        "saisei: could not restart ({})",
        std::io::Error::last_os_error()
    );
    std::process::exit(1);
}
