//! The per-game binary: the generated GameConfig data + a thin argv shim over
//! the runtime's C-style entry point.

// The generated per-game config defines `#[no_mangle] pub static game_config`
// (a strong symbol overriding the runtime's weak default). The module wrapper
// keeps its helper types out of this crate's namespace.
#[allow(non_upper_case_globals, dead_code)]
mod game_config {
    include!(concat!(env!("OUT_DIR"), "/game_config.rs"));
}

fn main() {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let args: Vec<CString> = std::env::args_os()
        .map(|a| CString::new(a.as_bytes()).expect("nul byte in argv"))
        .collect();
    let mut argv: Vec<*mut std::ffi::c_char> = args.iter().map(|c| c.as_ptr() as *mut _).collect();
    argv.push(std::ptr::null_mut());
    let rc =
        unsafe { saisei_runtime::shims::saisei_main((argv.len() - 1) as i32, argv.as_mut_ptr()) };
    std::process::exit(rc);
}
