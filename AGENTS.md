- This is a JIT recompiler: no game code is decoded ahead of time. `tools/game.py build <game>` only emits the per-game config C and links the runtime; all game code is JIT-compiled at run time into `build/<game>/jit/`.
- `build/` is build output and is NOT tracked in git (JIT chunks are regenerated on demand, cached by segment-bytes SHA + toolchain hash).
- To change how game code is translated, adjust the shared translator (`compiler/disassemble.py`, `compiler/ir_to_c.py`) or the runtime shims (`runtime/`); the next run re-JITs affected chunks automatically (the toolchain hash invalidates stale chunks).

## Debugging

- Use `make run` (or `python3 tools/game.py run <game> --headless`) to build and execute a game. A change is validated by the reconstructed game reaching its known scene — screenshot it (`SAISEI_SCREENSHOT_SECS=N`), don't rely on unit tests alone.
- Localize corruption with existing crash-bundle data (`build/<game>/crashes/`, `lifecycle.log` carries `ds`/registers) and WATCHW write-watchpoints (`write_watches[]` in `runtime/core/shims.c`) — not ad-hoc `SAISEI_*` debug toggles.
