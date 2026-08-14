use std::path::Path;

fn main() {
    // The generated per-game GameConfig (a .rs data file the launcher writes to
    // build/<game>/). Copied into OUT_DIR so main.rs can include! it; with no
    // config set, the runtime's weak `game_config` default applies.
    println!("cargo:rerun-if-env-changed=SAISEI_GAME_CONFIG");
    let out = Path::new(&std::env::var("OUT_DIR").unwrap()).join("game_config.rs");
    match std::env::var("SAISEI_GAME_CONFIG") {
        Ok(p) if !p.is_empty() => {
            println!("cargo:rerun-if-changed={p}");
            std::fs::copy(&p, &out).unwrap_or_else(|e| panic!("copy {p} to OUT_DIR: {e}"));
        }
        _ => {
            std::fs::write(
                &out,
                "// no per-game config: the runtime weak default applies\n",
            )
            .expect("write empty game_config.rs");
        }
    }
    // JIT chunk .so files resolve cpu/virtual_memory/every shim from the host
    // binary at dlopen — its dynamic symbol table must carry them all.
    // -rdynamic is the ELF spelling. On macOS the equivalent is
    // -export_dynamic, and it is NOT optional: Xcode 15's linker stopped
    // exporting a main executable's symbols by default, so without it the
    // first chunk dlopen dies with "symbol not found" on a host global.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!("cargo:rustc-link-arg-bins=-Wl,-export_dynamic");
    } else {
        println!("cargo:rustc-link-arg-bins=-rdynamic");
    }
    if let Some(arg) = macos_host_version_min() {
        println!("cargo:rustc-link-arg-bins={arg}");
    }
}

/// On macOS, target the running Mac's own OS version at link time, so ld64
/// stops warning that brew's SDL2 "was built for newer macOS version than
/// being linked" (rustc's default deployment target is 11.0 on Apple Silicon).
/// This machine's build is this machine's player — the host version is always
/// right. An explicit MACOSX_DEPLOYMENT_TARGET wins; this then emits nothing.
/// Kept in step with the same helper in runtime/build.rs and saisei-player/build.rs.
fn macos_host_version_min() -> Option<String> {
    println!("cargo:rerun-if-env-changed=MACOSX_DEPLOYMENT_TARGET");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos")
        || std::env::var("MACOSX_DEPLOYMENT_TARGET").is_ok()
    {
        return None;
    }
    let out = std::process::Command::new("sw_vers")
        .arg("-productVersion")
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    let v = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!v.is_empty()).then(|| format!("-mmacosx-version-min={v}"))
}
