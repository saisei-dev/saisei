fn main() {
    // The player binary hosts the game, so it is what the JIT chunk .so files
    // dlopen against: cpu / virtual_memory / every shim symbol must be in its
    // dynamic symbol table. Same reason saisei-game/build.rs does this.
    // -rdynamic is the ELF spelling; a Mach-O executable exports its global
    // symbols to dlopen'd images by default, so macOS needs (and has) no flag.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        println!("cargo:rustc-link-arg-bins=-rdynamic");
    }
}
