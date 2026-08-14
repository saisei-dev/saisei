fn main() {
    // The player binary hosts the game, so it is what the JIT chunk .so files
    // dlopen against: cpu / virtual_memory / every shim symbol must be in its
    // dynamic symbol table. Same reason saisei-game/build.rs does this.
    // -rdynamic is the ELF spelling. On macOS the equivalent is
    // -export_dynamic, and it is NOT optional: Xcode 15's linker stopped
    // exporting a main executable's symbols by default, so without it the
    // first chunk dlopen dies with "symbol not found" on a host global
    // (exec_params was the one that caught it).
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
/// Kept in step with the same helper in runtime/build.rs and saisei-game/build.rs.
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
