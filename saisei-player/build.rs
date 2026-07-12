fn main() {
    // The player binary hosts the game, so it is what the JIT chunk .so files
    // dlopen against: cpu / virtual_memory / every shim symbol must be in its
    // dynamic symbol table. Same reason saisei-game/build.rs does this.
    println!("cargo:rustc-link-arg-bins=-rdynamic");
}
