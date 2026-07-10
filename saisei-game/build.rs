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
    println!("cargo:rustc-link-arg-bins=-rdynamic");
}
