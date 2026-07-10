- This is a JIT recompiler: no program code is decoded ahead of time. `saisei build <name>` only generates the per-program GameConfig (Rust) and builds the game binary via cargo; all program code is JIT-compiled at run time into `build/<name>/jit/`. The whole toolchain is Rust; build it with `cargo build --release`.
- `build/` is build output and is NOT tracked in git (JIT chunks are regenerated on demand, cached by segment-bytes SHA + toolchain hash).
- To change how program code is translated, adjust the translator (`saisei-jitc/src/disassemble.rs`, `saisei-jitc/src/translate.rs`, `saisei-jitc/src/codegen.rs`) or the runtime shims (`runtime/src/`); rebuild `saisei-jitc` and the next run re-JITs affected chunks automatically (the toolchain hash invalidates stale chunks).

## Debugging

- Use `saisei run <name> --headless` to build and execute a program. A change is validated by the program reaching its known scene — screenshot it (`--screenshot-secs N`), don't rely on unit tests alone.
- Localize corruption with existing crash-bundle data (`build/<name>/crashes/`, `lifecycle.log` carries `ds`/registers) and WATCHW write-watchpoints (`write_watches[]` in `runtime/src/shims.rs`) — not ad-hoc `SAISEI_*` debug toggles.
