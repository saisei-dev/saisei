# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this project is

A **JIT binary recompiler** that turns DOS MZ executables into native C and runs them. The runtime loads a game's program image, takes the entry `cs:ip` from its MZ header, and — the first time control reaches any code segment — dumps the live 64KB, decodes it to a lossless JSON IR, structures that into C, compiles it with clang, and `dlopen`s the result. The translated C *is* the game; there is **no interpreter, no external emulator, and no ahead-of-time whole-image decode**. Code is discovered and compiled on demand as the program executes, which is exactly what makes packed / overlay / self-modifying games work with no special-casing.

> NOTE: the pipeline code lives in `compiler/` (translator + JIT) and `tools/` (orchestration) — there is no `scripts/` dir despite occasional stale references in older docs.

## Commands

The primary driver is the `saisei` command (`tools/game.py`; installed via `pip install -e .`). No programs ship with the repo — bootstrap a bundle with `saisei new-game <archive> --exe FOO.EXE`, which creates `games/<name>/` with a `<name>.json` config. Then:

```bash
saisei build <name>                 # emit the config + link the runtime
saisei run   <name> --headless      # build + run (headless = no SDL window)
saisei play  <name>                 # build + run in the SDL window
saisei run   <name> --program setup # a bundle may define multiple programs
```

The unpackaged form works without installing: `export PYTHONPATH=$PWD` then `python3 tools/game.py <command>`.

`build` does **not** decode anything ahead of time — it just emits the per-program config C and links the runtime into `build/<name>/<program>`. All program code is JIT-compiled at run time.

Useful `run` flags: `--silent` (suppress trace to stdout), `--trace-file <path>` (write execution trace), `--lifecycle-file <path>` (stream LOAD/CALL/JMP/… events). `game.py` sets `SAISEI_REPO_ROOT`/`SAISEI_PYTHON`/`SAISEI_JIT_DIR` itself (the runtime JIT needs them to invoke the compiler and cache chunks). Screenshots: set `SAISEI_SCREENSHOT_SECS=N` to auto-dump PNGs to `build/<game>/screenshots/`.

The `Makefile` is a thin JIT-only convenience wrapper over `tools/game.py` (pass the bundle as `GAME=<name>`): `make build GAME=<name>`, `make run GAME=<name>`, `make play GAME=<name>` (SDL viewer). Inject compiler defines with `CFLAGS=…` (e.g. `CFLAGS=-DFORCE_EXIT_AFTER_10S make run GAME=<name>` for a self-terminating smoke run).

Tests are pytest in `tests/`, mostly exercising `compiler/` translation logic and the `runtime/` shims (compiled per-test and driven via `ctypes`):

```bash
python3 -m pytest tests/                         # whole suite
python3 -m pytest tests/test_disassemble_default_segment.py            # one file
python3 -m pytest tests/test_disassemble_default_segment.py::test_name # one test
```

## Architecture

**Translator (`compiler/`, shared by every JIT compile):**
- `disassemble.py` — Capstone-decodes a byte image into `program.ir.json` + header/reloc/xref metadata. Computes per-operand default segment (BP/SP→SS, else DS), entry points, basic-block boundaries.
- `ir_to_c.py` — the translator. IR → basic blocks (`basic_block.py`) → CFG (`cfg.py`, networkx) → structured regions (`patterns/`, `ast_nodes.py`) → C. Memory ops become `memb`/`memw` helpers; instrumentation points (`SAFEPOINT()`) are preserved. **This is where most translation bugs live.**
- `build_pipeline.py` — runs `disassemble.py` then `ir_to_c.py` on one input, emitting `<name>.c`. The JIT calls exactly this on each live memory segment.
- `generate_game_config.py` — emits the per-game `GameConfig` C (program image path, PSP load segment, protected slots) from the one `<name>.json`. Carries no dispatch table and no entry symbol: the runtime takes the entry `cs:ip` from the MZ header and JITs from there.

**JIT (`compiler/jit_chunk.py`, `jit_compile.py` + runtime):** When control reaches an address with no compiled chunk, `jit_compile_or_get` (`runtime/core/shims.c`) dumps the live 64KB segment to `SAISEI_JIT_DIR/seg_<base>.bin`, runs the *same* `build_pipeline.py` on it, compiles a `.so`, and `dlopen`s it — no restart. Chunks live at `build/<game>/jit/jit_<segbase>_<offset>_<contentsha>.{c,so,sha,keys,code}`, cached by segment-bytes SHA + toolchain hash (so the same seg:ip decoding *different* bytes gets distinct chunks). Each chunk's dispatch switch is keyed on IP; the chunk runs based at the live `cs`, so pushed near-call return IPs are true cs-relative offsets and round-trip through retf/far-jmp. The same physical code reached under a different `cs` alias becomes a *separate* chunk at that alias's seg base — segment/return handling must stay faithful to the x86 model across that seam. To hand-instrument a chunk: edit its `.c`, recompile the `.so` (`clang -shared -fPIC -O1 chunk.c -I runtime/include -I <dir> -o chunk.so`); the `.sha` stays valid so the cache hits.

*Freezing:* the end goal is to collect the JIT-discovered chunks and link them into a fully static native build (no runtime Python/clang). The runtime dispatch tables (`GameConfig.binary_dispatch`, the `DispatchFn` ABI in `runtime/include/game_config.h`) are retained NULL-but-shaped for that future freeze to populate; today they are empty and every address routes through the JIT.

**Runtime (`runtime/`), layered:**
- `core/` — `shims.c` (the big integration surface: `memb`/`memw` writes, `file_mappings`, the JIT registry + `dispatch_via_binary`, WATCHW tripwires, crash bundles, the function-patch registry), `snapshot.c`, `save_manager.c`.
- `os/` — `dos.c` (INT 21h: file I/O, memory alloc, console), `bios.c` (INT 10h/16h, etc.), `mouse.c` (INT 33h).
- `hw/` — device emulation split out of shims: `io_bus.c`, `audio.c`, `video.c`, `keyboard.c`, `timer.c`.
- `display/virtual_display_sdl.c`, `include/shims.h` (the helper macros the generated C calls), `include/game_config.h`.

**Runtime memory model (`docs/runtime_memory_model.md`):** every linear byte has an *origin* (file + offset), tracked in `file_mappings[]` (newest covering entry wins). The top-level loop `run_machine` → `resolve_and_run_chunk` resolves the live `cs:ip` to its owning JIT chunk (compiling it on first reach); cross-binary/indirect transfers go through `dispatch_via_binary`. This origin tracking is what makes on-the-fly-loaded (unpacked/overlay) code dispatchable and savable.

**Function patches (`runtime/core/shims.c`, `runtime/include/game_config.h`):** a `GamePatch` replaces or augments a game function identified by `(binary basename, file_off)` — the stable identity the dispatcher resolves addresses to, so one patch applies across cs-aliases. Patches register at startup or from separately-delivered `.so` bundles (`patch_load_bundle`, `--patch-bundle`); a patch fn returns `PATCH_HANDLED`/`PATCH_DECLINED` and can call `patch_call_original`/`patch_call_function`/`patch_ret_near`.

**Per-game config (`games/<name>/<name>.json`):** `name`, `program_path` (the MZ image to load), optional `programs` (multi-executable bundles, each with its own `program_path`/`psp_seg`), `psp_seg`/`init_cs` (machine params), `protected_slots` (runtime memory-protection ranges), and `runtime` (files copied into `build/<game>/` at run). The per-binary `<binary>.json` sidecars and the `aliases`/`callgraph`/`regions`/`vars`/`enums` files are reverse-engineering annotations (function names, comments, discovered entries) — not part of the JIT run path. Diagnosis artifacts land in `build/<game>/` (`lifecycle.log`, `watchw.log`) and `crashes/`.

## Working principles (non-obvious, enforced)

- **Emulate faithfully — no heuristic harnesses. This is the prime directive.** The JIT and shims must model real x86 / BIOS / hardware / DOS behavior *exactly*; when they do, the game just runs, with no special-casing. Do **NOT** add fallbacks, recoveries, or "is-this-really-X?" detectors that guess based on heuristics — e.g. windowed address-recovery, `retf_is_genuine_far_return`, stack-drift fixups, "redirect if the window-corrected address happens to be a decoded case-key." Every such harness papers over an *unfaithful model* and makes wrong behavior masquerade as working. When a transfer / return / address / register comes out wrong, the bug is that the translation or shim isn't faithful — fix the model so the band-aid becomes unnecessary, then delete the band-aid.
- **Don't treat one of our own outputs as a known-good oracle.** A program that has never run correctly end-to-end (e.g. SETUP.EXE) has no trustworthy reference chunk. "This JIT chunk is byte-identical to that one" proves nothing — both are our output and can be equally wrong. A packed EXE's on-disk image is just the unpacker stub; the real program is *all* runtime-JIT'd. Validate against the real x86/DOS/BIOS contract, not against our own translation.
- **Bugs are in our shims or our generated C — never in the game; find them by reading OUR code against the x86/DOS/BIOS spec, not by studying the game's.** The games are real, correct, shipping binaries. The trap is tracing the game's own code (loader, decompressor, allocator) *to localize which shim is unfaithful* — still studying the game, still a waste: the game is not the variable, our model of x86/DOS/BIOS/HW is, and tracing the game only re-derives the faithful behavior you already know from the spec. Instead, name the corruption's signature, list the OUR-code surfaces (`runtime/` shims, `compiler/` codegen) that implement that operation, and read them against the spec for an unfaithful implementation or simplification — a library call standing in for an instruction's exact semantics, uncomputed flags, an "approximate"/"good enough" shortcut, an abort-on-unknown stub. Decode the game only to *confirm* a hypothesis already pinned to a named `file:function` in our code — never to discover the bug, and not via the generated game (`program.ir.json`, `build/<game>/jit/*.c` are the game re-expressed in C; reading them to follow what the game computes is game-tracing with extra steps, though reading a chunk to fix a codegen defect visible in the C is fine). (Example: `handle_les`/`handle_lds` hardcoded the mem-operand segment to DS, but `[bp+…]` defaults to SS — a codegen bug; likewise a block-copy shim's `memmove` silently broke `rep movs` overlap-replication semantics — a shim bug. Both found by reading our code, not the game.)
- **No external emulator as an oracle.** Never reference/diff against DOSBox/QEMU/dosemu; that tooling was deliberately removed. The reconstruction is self-contained.
- **No fabricated artifacts or address-guessing.** Don't hand-write output files (e.g. a game's config/save file) or guess addresses to get past a gate — produce them by running the real reconstructed user journey. Stop and ask when in doubt.
- **No debug-toggle cruft.** Don't add `SAISEI_*` env vars or ad-hoc debug flags; use existing crash-bundle data (`lifecycle.log` already carries `ds`/registers) and WATCHW write-watchpoints (`write_watches[]` in `shims.c`) to localize corruption.
- **Validate by running the real game (`run`), not just by tests.** A change is proven when the reconstructed game reaches its known scene (screenshot it). The pytest suite exercises translation/shim units but is not the acceptance bar for a runtime behavior change.
