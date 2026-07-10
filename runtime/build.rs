//! Native link flags for the runtime crate's linkable artifacts: the cdylib
//! (`libsaisei_runtime.so`, dlopen'd by the shim unit tests) and any binary
//! that depends on the rlib. SDL2 comes from pkg-config (with a homebrew
//! fallback), plus the system libs the runtime calls directly.

use std::process::Command;

fn main() {
    let pkg = Command::new("pkg-config")
        .args(["--libs", "sdl2"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());

    match pkg {
        Some(flags) => {
            for tok in flags.split_whitespace() {
                if let Some(dir) = tok.strip_prefix("-L") {
                    println!("cargo:rustc-link-search=native={dir}");
                } else if let Some(lib) = tok.strip_prefix("-l") {
                    println!("cargo:rustc-link-lib={lib}");
                }
            }
        }
        None => {
            // Homebrew fallback (mirrors the launcher's resolve_sdl_flags).
            println!("cargo:rustc-link-search=native=/opt/homebrew/lib");
            println!("cargo:rustc-link-lib=SDL2");
        }
    }
    println!("cargo:rustc-link-lib=dl");
    println!("cargo:rustc-link-lib=pthread");
    println!("cargo:rustc-link-lib=m");
}
