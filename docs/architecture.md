# Architecture and implementation overview

This document captures the intent, goals and technical design of Saisei so new
contributors can build on top of it with confidence.

## What Saisei is

A **JIT binary recompiler** for DOS MZ executables. The runtime loads a
program image, takes the entry `cs:ip` from its MZ header, and — the first time
control reaches any code segment — dumps the live 64 KB, decodes it to a
lossless JSON IR, emits it as Rust, compiles it with rustc, and `dlopen`s
the result. The translated Rust *is* the program.

There is **no interpreter, no external emulator, and no ahead-of-time
whole-image decode**. Code is discovered and compiled on demand as the program
executes, which is exactly what makes packed / overlay / self-modifying
programs run with no special-casing: whatever bytes are live at a `cs:ip` when
control reaches it are the bytes that get compiled.

## The run loop (compile-on-demand)

The runtime's top-level loop is `run_machine` → `resolve_and_run_chunk`
(`runtime/src/shims.rs`):

1. Resolve the live `cs:ip` to the JIT chunk that owns it.
2. If no chunk covers it, dump the live 64 KB segment (based at `cs<<4`) to
   `SAISEI_JIT_DIR/seg_<base>.bin` and translate + compile it (below) — no
   restart. The new `.so` is `dlopen`ed and registered.
3. Dispatch into the chunk; it runs until control leaves its `pc`-space, then
   returns to the loop, which re-resolves the new `cs:ip`.

Cross-binary and indirect transfers route through `dispatch_via_binary`. Which
bytes belong to which chunk is answered by the **runtime memory model** — every
linear byte has an origin `(file, file_offset)` tracked in `file_mappings[]` —
described in [runtime_memory_model.md](runtime_memory_model.md).

Chunks are cached on disk at
`build/<name>/jit/jit_<segbase>_<offset>_<contentsha>.{rs,so,sha,...}`, keyed on
the **segment bytes' SHA plus a toolchain hash**. The same `seg:ip` decoding
*different* bytes (an overlay reshuffle, a decompressed payload) therefore gets
a distinct chunk, and identical bytes reuse one.

## The translator (`saisei-jitc/`)

The same translator runs on every JIT compile. Given a byte image it produces
one chunk `.rs` exporting `<name>_dispatch`:

- **`disassemble.rs`** — Capstone-decodes the image into `program.ir.json` plus
  header/reloc/xref metadata. Computes each operand's default segment
  (`BP`/`SP` → `SS`, else `DS`), entry points, and basic-block boundaries. A
  `--max-insns` cap bounds runaway decodes; `--image-base` maps a single
  segment's far targets to dump offsets; `--cs-base` sets the IP at file
  offset 0.
- **`translate.rs`** — the shared translation front-half: IR instruction
  utilities, operand rewriting (memory operands become `memb`/`memw` accessor
  calls; RCB and exec_params fields get their names; `[bp±N]` becomes stack
  vars), flag normalization, basic blocks and CFG successors.
- **`codegen.rs`** — the chunk emitter. IR → a flat pc-state-machine
  (`loop { match pc { … } }`, one match arm per basic block; every instruction
  is preceded by `set_ip(…)` + `SAFEPOINT()`), plus a `#[no_mangle]` `_impl`
  wrapper per function. **This is where most translation bugs live.** An
  instruction the emitter can't express **hard-fails loudly** (`Unsupported`,
  a fatal JIT error) rather than being silently dropped or stubbed — a failed
  translate means either a missing handler (extend `codegen.rs`) or the decoder
  walked into data (a wrong entry / bad transfer target), a bug to fix, not to
  paper over.
- **`jit-compile` subcommand** — invoked by the runtime: disassembles the
  segment dump, emits the chunk Rust, drops the `saisei_rt.rs` prelude beside
  it, and compiles the `.so` with rustc, with the content-keyed cross-run
  cache. (`disasm` and `emit` run the two halves standalone.)
- **`generate_game_config`** (in `saisei/`) — emits the per-program
  `GameConfig` Rust data file (image path, PSP load segment, protected slots)
  from the one `<name>.json`; the `saisei-game` bin crate `include!`s it. It
  carries no dispatch table and no entry symbol — the runtime takes the entry
  `cs:ip` from the MZ header and JITs from there.

Every chunk `include!`s the prelude **`saisei-jitc/rt/saisei_rt.rs`** — the
Rust view of the runtime ABI: it binds the shared `cpu` global, the
`memb`/`memw`/shim call surface, and faithful inline helpers (`parity8`,
`xor8`/`xor16`, `linear_addr`). The prelude's `#[repr(C)]` layouts and the
runtime's struct definitions must be edited together; the prelude is embedded
in the `saisei-jitc` binary, so the toolchain hash covers it.

## The runtime (`runtime/`, crate `saisei-runtime`)

- **`shims.rs`** — the big integration surface: the machine loop, `memb`/`memw`
  writes, `file_mappings`, the JIT registry + `dispatch_via_binary`, IRQ
  delivery, WATCHW write-watchpoints, crash bundles, the function-patch
  registry, the trace/log channel. Entry point `saisei_main` (the `saisei-game`
  bin's Rust `main` calls it with C argv).
- **`dos.rs`** (INT 21h: file I/O, memory alloc, console), **`bios.rs`**
  (INT 10h/16h, …), **`mouse.rs`** (INT 33h).
- Device emulation: **`io_bus.rs`**, **`audio.rs`**, **`video.rs`**,
  **`keyboard.rs`**, **`timer.rs`**; display: **`sdl.rs`**; persistence:
  **`snapshot.rs`**, **`save_manager.rs`** (their `#[repr(C)]` layouts are
  frozen — snapshots serialize them byte-for-byte).
- Built as an rlib (linked into the `saisei-game` binary, `-rdynamic` so chunk
  `.so`s resolve shims from the host) and as a cdylib (`libsaisei_runtime.so`,
  dlopen'd in isolated copies by the shim unit tests). Port history and
  deliberate deviations from the original C runtime:
  [runtime_port_notes.md](runtime_port_notes.md).

The generated Rust only ever calls the fixed runtime ABI the prelude declares;
a contract test (`saisei-jitc/tests/ported_base_e.rs`, `runtime_abi_contract`)
fails if the emitter produces a call to anything outside it.

## Key design decisions

- **JIT-only, compile-on-demand.** No byte is decoded until control reaches it
  with the bytes that are actually live there. This is what makes packed /
  overlay / self-modifying code work without special-casing.
- **Faithful emulation, no heuristic harnesses.** The JIT and shims model real
  x86 / BIOS / hardware / DOS behaviour *exactly*; a wrong transfer/return/value
  is a bug in our model to fix, not something to recover from with a guess. See
  the working principles in `CLAUDE.md` and the in-progress
  [faithful_dispatch_refactor.md](faithful_dispatch_refactor.md).
- **Lossless JSON IR as the contract.** `program.ir.json` plus manifest and
  cross-reference files decouple decoding from emission, so translator
  improvements reuse the same IR without repeating the Capstone pass.
- **Flat pc-state-machine chunks.** Each basic block is a match arm keyed on
  its cs-relative address; control flow is explicit `pc = …; continue;`
  transfers. No structuring pass sits between the IR and the emitted code, so
  the chunk's control flow is exactly the program's.

## Freezing (future)

The end goal is to collect the JIT-discovered chunks and link them into a fully
static native build with no runtime compiler at all. The runtime dispatch
tables (`GameConfig.binary_dispatch`, the `DispatchFn` ABI) are retained
NULL-but-shaped for that freeze to populate; today they are empty and every
address routes through the JIT.

## Where things live

Everything is Rust, on a *dated* nightly pinned by `rust-toolchain.toml` —
`c_variadic` and `linkage` are unstable, so a floating nightly could break a
fresh clone out from under it. The only C in the tree is the vendored capstone
disassembler (`vendor/capstone-sys`), compiled statically inside cargo by the cc
crate, using the same system C compiler rustc already needs as its linker.

So the system prerequisites are exactly two — that C compiler/linker and SDL2 —
plus rustup. The launcher spawns nothing else: `new-game` downloads over HTTPS
and extracts zips in-process, and the build revision is baked in at compile
time. (`cargo` and `rustc` are still invoked at *run* time — that is the JIT.)

- `saisei-jitc/src/disassemble.rs` – byte image → IR + metadata.
- `saisei-jitc/src/translate.rs` – the shared translation front-half.
- `saisei-jitc/src/codegen.rs` – IR → chunk Rust (the pc-state-machine emitter).
- `saisei-jitc/rt/saisei_rt.rs` – the chunk prelude (runtime ABI + `cpu` binding).
- `saisei-jitc` subcommands `disasm` / `emit` / `jit-compile` – the JIT's translate → compile path.
- `saisei/` – the `saisei` launcher (build/run/play/new-game/…) + `generate_game_config`.
- `saisei-game/` – the thin per-game bin crate (runtime rlib + generated config).
- `runtime/` – the runtime crate (`saisei-runtime`).
- `saisei-jitc/tests/` + `saisei/tests/` – `cargo test` over the translator and
  the runtime shims (the latter driven via dlopen/FFI on the runtime cdylib).

## Building on the pipeline

- **Adding translation coverage:** implement the missing instruction handler in
  `saisei-jitc/src/codegen.rs` (an unsupported op hard-fails with the exact
  mnemonic; reproduce offline with `saisei-jitc emit` or the `gap_sweep` test).
- **Extending the runtime:** implement the missing DOS/BIOS/hardware behaviour
  in `runtime/src/`; the next run re-JITs affected chunks automatically (the
  toolchain hash invalidates stale ones).
- **Diagnosing corruption:** add an address to `write_watches[]` in
  `runtime/src/shims.rs` and the next crash bundle's `lifecycle.log` names the
  writer's `cs:ip` + registers. A change is validated by the real program
  reaching its known scene — screenshot it (`--screenshot-secs N`), don't rely
  on unit tests alone.
