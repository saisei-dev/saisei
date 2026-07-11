# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this project is

A **JIT binary recompiler** that turns DOS MZ executables into native Rust and runs them. The runtime loads a game's program image, takes the entry `cs:ip` from its MZ header, and — the first time control reaches any code segment — dumps the live 64KB, decodes it to a lossless JSON IR, emits it as Rust, compiles it with rustc, and `dlopen`s the result. The translated Rust *is* the game; there is **no interpreter, no external emulator, and no ahead-of-time whole-image decode**. Code is discovered and compiled on demand as the program executes, which is exactly what makes packed / overlay / self-modifying games work with no special-casing.

Everything is Rust, workspace at the repo root: `saisei-jitc/` (the translator +
JIT, exposed as the `saisei-jitc` binary + library), `saisei/` (the `saisei`
launcher), `runtime/` (the runtime crate, `saisei-runtime`), and `saisei-game/`
(the thin per-game bin crate). No C is compiled anywhere and no C toolchain is
invoked; the only C in the tree is the vendored capstone disassembler
(`vendor/capstone-sys`, built by the cc crate inside cargo). The toolchain is
pinned to nightly (`rust-toolchain.toml`) for `c_variadic` in the runtime.

## Commands

Build the toolchain once with `cargo build --release`; the driver is
the `saisei` binary at `target/release/saisei` (put it on your PATH, or run
it directly / via `cargo run --release -p saisei --`). No programs ship with the
repo — bootstrap a bundle with `saisei new-game <archive> --exe FOO.EXE`, which
creates `games/<name>/` with a `<name>.json` config. Then:

```bash
saisei build <name>                 # generate the config + build the game binary
saisei run   <name> --headless      # build + run (headless = no SDL window)
saisei play  <name>                 # build + run in the SDL window
saisei run   <name> --program setup # a bundle may define multiple programs
```

`build` does **not** decode anything ahead of time — it generates the per-program GameConfig (Rust) and has cargo build the `saisei-game` bin (runtime rlib + config, linked `-rdynamic`), copied to `build/<name>/<program>`. All program code is JIT-compiled at run time.

Useful `run` flags: `--verbose` (runs are silent by default; this prints the shim trace to stdout), `--trace-file <path>` (write execution trace), `--lifecycle-file <path>` (stream LOAD/CALL/JMP/… events), `--features <list>` (cargo features for the game build — e.g. `--features force_exit_after_10s` for a self-terminating smoke run). The launcher sets `SAISEI_REPO_ROOT`/`SAISEI_JITC`/`SAISEI_JIT_DIR` itself (the runtime JIT needs them to invoke the `saisei-jitc` translator and cache chunks). Screenshots: pass `--screenshot-secs N` (headless runs) to auto-dump PNGs to `build/<name>/screenshots/`.

Tests are Rust `cargo test` — the translator unit tests (`saisei-jitc/tests/`,
asserting on the chunk emitter's Rust output and the shared front-half) and the
runtime shim tests (dlopen'd isolated copies of the runtime cdylib, driven via
FFI), plus launcher tests in `saisei/tests/`. Capstone (5.0.7, x86-only) is
vendored and built statically by `vendor/capstone-sys` — no env vars needed;
setting `SAISEI_CAPSTONE_LIB_DIR` optionally links an external libcapstone
dylib instead.

```bash
cargo test                                              # whole workspace
cargo test -p saisei-jitc --test ported_disasm          # one test file
cargo test -p saisei-jitc --test ported_disasm disassemble_retf__retf  # one test
```

## Architecture

**Translator (`saisei-jitc/src/`, shared by every JIT compile):**
- `disassemble.rs` — Capstone-decodes a byte image into `program.ir.json` + header/reloc/xref metadata. Computes per-operand default segment (BP/SP→SS, else DS), entry points, basic-block boundaries.
- `translate.rs` — the shared front-half: IR instruction utilities, operand rewriting (`rewrite_mem_op` → `memb`/`memw` accessors, RCB/exec_params named fields, stack vars), flag normalization (`normalize_flags`), basic blocks (`build_basic_blocks`) and CFG successors. The RCB field table (`RCB_FIELDS`) lives here, kept in sync with the chunk prelude by unit tests.
- `codegen.rs` — the chunk emitter: IR → a flat pc-state-machine (`loop { pc = match pc { … } }`, one arm per basic block calling a small per-block `fn … -> c_int` that returns the next pc, -1 to leave the dispatcher; `set_ip`/`SAFEPOINT()` per instruction) plus per-function `_impl` wrappers. Blocks are separate small fns on purpose: rustc's per-body analyses (borrowck) are superlinear, and one giant dispatch fn made JIT compiles ~3× slower. **This is where most translation bugs live.** A construct it can't express is a hard `Unsupported` error — there is no fallback backend; extend `codegen.rs` (repro with `saisei-jitc emit` or the `gap_sweep` test).
- `generate_game_config` (in `saisei/`) — emits the per-game `GameConfig` Rust data file (program image path, PSP load segment, protected slots) from the one `<name>.json`; the `saisei-game` build.rs `include!`s it. Carries no dispatch table and no entry symbol: the runtime takes the entry `cs:ip` from the MZ header and JITs from there.

**JIT (`saisei-jitc jit-compile` + runtime):** When control reaches an address with no compiled chunk, `jit_compile_or_get` (`runtime/src/shims.rs`) dumps the live 64KB segment to `SAISEI_JIT_DIR/seg_<base>.bin`, runs the `saisei-jitc jit-compile` translator (via `SAISEI_JITC`) on it, compiles a `.so` with rustc, and `dlopen`s it — no restart. Chunks live at `build/<game>/jit/jit_<segbase>_<offset>_<rssha>.{rs,so,sha,keys,code}`, content-addressed by SHA of the emitted Rust (name-normalized) + toolchain hash — the same seg:ip decoding different *code* bytes gets distinct chunks, while dumps differing only in data bytes emit identical Rust and reuse one compiled `.so` (rustc is ~25× the cost of decode+emit). A `jit_<segbase>_<offset>_<blobsha>.alias` sidecar per distinct 64KB dump names its chunk so an identical dump resolves without even re-decoding. Every chunk `include!`s the prelude `saisei_rt.rs` (embedded in the `saisei-jitc` binary, dropped beside the chunks) — it binds the shared `cpu` global and the runtime ABI; its `#[repr(C)]` layouts and the runtime's must be edited together. Each chunk's dispatch match is keyed on IP; the chunk runs based at the live `cs`, so pushed near-call return IPs are true cs-relative offsets and round-trip through retf/far-jmp. The same physical code reached under a different `cs` alias becomes a *separate* chunk at that alias's seg base — segment/return handling must stay faithful to the x86 model across that seam. To hand-instrument a chunk: edit its `.rs` and recompile the `.so` in place (`rustc --edition 2021 --crate-type cdylib -C opt-level=1 -C overflow-checks=off -C debug-assertions=off -C panic=abort -o <chunk>.so <chunk>.rs`, run in the jit dir beside `saisei_rt.rs`); the `.sha` stays valid so the cache hits.

*Freezing:* the end goal is to collect the JIT-discovered chunks and link them into a fully static native build (no runtime compiler at all). The runtime dispatch tables (`GameConfig.binary_dispatch`, the `DispatchFn` ABI) are retained NULL-but-shaped for that future freeze to populate; today they are empty and every address routes through the JIT.

**Runtime (`runtime/`, crate `saisei-runtime`; port notes in `docs/runtime_port_notes.md`):**
- `shims.rs` — the big integration surface: the machine loop (`run_machine` → `resolve_and_run_chunk`), `memb`/`memw`, `file_mappings`, the JIT registry + `dispatch_via_binary`, IRQ delivery, WATCHW tripwires, crash bundles, the function-patch registry. Entry point `saisei_main`.
- `dos.rs` (INT 21h: file I/O, memory alloc, console), `bios.rs` (INT 10h/16h, …), `mouse.rs` (INT 33h).
- device emulation: `io_bus.rs`, `audio.rs`, `video.rs`, `keyboard.rs`, `timer.rs`; display: `sdl.rs`; persistence: `snapshot.rs`, `save_manager.rs` (their `#[repr(C)]` layouts are FROZEN — snapshots serialize them byte-for-byte).
- Built as rlib (linked into `saisei-game`) and cdylib (`libsaisei_runtime.so`, dlopen'd by the shim unit tests).

**Runtime memory model (`docs/runtime_memory_model.md`):** every linear byte has an *origin* (file + offset), tracked in `file_mappings[]` (newest covering entry wins). The top-level loop `run_machine` → `resolve_and_run_chunk` resolves the live `cs:ip` to its owning JIT chunk (compiling it on first reach); cross-binary/indirect transfers go through `dispatch_via_binary`. This origin tracking is what makes on-the-fly-loaded (unpacked/overlay) code dispatchable and savable.

**Function patches (`runtime/src/shims.rs`):** a `GamePatch` replaces or augments a game function identified by `(binary basename, file_off)` — the stable identity the dispatcher resolves addresses to, so one patch applies across cs-aliases. Patches register at startup or from separately-delivered `.so` bundles (`patch_load_bundle`, `--patch-bundle`); a patch fn returns `PATCH_HANDLED`/`PATCH_DECLINED` and can call `patch_call_original`/`patch_call_function`/`patch_ret_near`.

**Per-game config (`games/<name>/<name>.json`):** `name`, `program_path` (the MZ image to load), optional `programs` (multi-executable bundles, each with its own `program_path`/`psp_seg`), `psp_seg`/`init_cs` (machine params), `protected_slots` (runtime memory-protection ranges), and `runtime` (files copied into `build/<game>/` at run). The per-binary `<binary>.json` sidecars and the `aliases`/`callgraph`/`regions`/`vars`/`enums` files are reverse-engineering annotations (function names, comments, discovered entries) — not part of the JIT run path. Diagnosis artifacts land in `build/<game>/` (`lifecycle.log`, `watchw.log`) and `crashes/`.

## Working principles (non-obvious, enforced)

- **Emulate faithfully — no heuristic harnesses. This is the prime directive.** The JIT and shims must model real x86 / BIOS / hardware / DOS behavior *exactly*; when they do, the game just runs, with no special-casing. Do **NOT** add fallbacks, recoveries, or "is-this-really-X?" detectors that guess based on heuristics — e.g. windowed address-recovery, `retf_is_genuine_far_return`, stack-drift fixups, "redirect if the window-corrected address happens to be a decoded case-key." Every such harness papers over an *unfaithful model* and makes wrong behavior masquerade as working. When a transfer / return / address / register comes out wrong, the bug is that the translation or shim isn't faithful — fix the model so the band-aid becomes unnecessary, then delete the band-aid.
- **Don't treat one of our own outputs as a known-good oracle.** A program that has never run correctly end-to-end (e.g. SETUP.EXE) has no trustworthy reference chunk. "This JIT chunk is byte-identical to that one" proves nothing — both are our output and can be equally wrong. A packed EXE's on-disk image is just the unpacker stub; the real program is *all* runtime-JIT'd. Validate against the real x86/DOS/BIOS contract, not against our own translation.
- **Bugs are in our shims or our generated code — never in the game; find them by reading OUR code against the x86/DOS/BIOS spec, not by studying the game's.** The games are real, correct, shipping binaries. The trap is tracing the game's own code (loader, decompressor, allocator) *to localize which shim is unfaithful* — still studying the game, still a waste: the game is not the variable, our model of x86/DOS/BIOS/HW is, and tracing the game only re-derives the faithful behavior you already know from the spec. Instead, name the corruption's signature, list the OUR-code surfaces (`runtime/src/` shims, `saisei-jitc/src/` codegen) that implement that operation, and read them against the spec for an unfaithful implementation or simplification — a library call standing in for an instruction's exact semantics, uncomputed flags, an "approximate"/"good enough" shortcut, an abort-on-unknown stub. Decode the game only to *confirm* a hypothesis already pinned to a named `file:function` in our code — never to discover the bug, and not via the generated game (`program.ir.json`, `build/<game>/jit/*.rs` are the game re-expressed in Rust; reading them to follow what the game computes is game-tracing with extra steps, though reading a chunk to fix a codegen defect visible in the emitted Rust is fine). (Example: `handle_les`/`handle_lds` hardcoded the mem-operand segment to DS, but `[bp+…]` defaults to SS — a codegen bug; likewise a block-copy shim's `memmove` silently broke `rep movs` overlap-replication semantics — a shim bug. Both found by reading our code, not the game.)
- **No external emulator as an oracle.** Never reference/diff against DOSBox/QEMU/dosemu; that tooling was deliberately removed. Saisei is self-contained.
- **No fabricated artifacts or address-guessing.** Don't hand-write output files (e.g. a program's config/save file) or guess addresses to get past a gate — produce them by running the real user journey. Stop and ask when in doubt.
- **No debug-toggle cruft.** Don't add `SAISEI_*` env vars or ad-hoc debug flags; use existing crash-bundle data (`lifecycle.log` already carries `ds`/registers) and WATCHW write-watchpoints (`write_watches[]` in `runtime/src/shims.rs`) to localize corruption.
- **Validate by running the real program (`run`), not just by tests.** A change is proven when the program reaches its known scene (screenshot it). The `cargo test` suite exercises translation/shim units but is not the acceptance bar for a runtime behavior change.
