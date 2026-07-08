# Architecture and implementation overview

This document captures the intent, goals and technical design of Saisei so new
contributors can build on top of it with confidence.

## What Saisei is

A **JIT binary recompiler** for DOS MZ executables. The runtime loads a
program image, takes the entry `cs:ip` from its MZ header, and — the first time
control reaches any code segment — dumps the live 64 KB, decodes it to a
lossless JSON IR, structures that into C, compiles it with clang, and `dlopen`s
the result. The translated C *is* the program.

There is **no interpreter, no external emulator, and no ahead-of-time
whole-image decode**. Code is discovered and compiled on demand as the program
executes, which is exactly what makes packed / overlay / self-modifying
programs run with no special-casing: whatever bytes are live at a `cs:ip` when
control reaches it are the bytes that get compiled.

## The run loop (compile-on-demand)

The runtime's top-level loop is `run_machine` → `resolve_and_run_chunk`
(`runtime/core/shims.c`):

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
`build/<name>/jit/jit_<segbase>_<offset>_<contentsha>.{c,so,sha,...}`, keyed on
the **segment bytes' SHA plus a toolchain hash**. The same `seg:ip` decoding
*different* bytes (an overlay reshuffle, a decompressed payload) therefore gets
a distinct chunk, and identical bytes reuse one.

## The translator (`compiler/`)

The same translator runs on every JIT compile. Given a byte image it produces
one `<name>.c` exporting `<name>_dispatch`:

- **** — Capstone-decodes the image into `program.ir.json` plus
  header/reloc/xref metadata. Computes each operand's default segment
  (`BP`/`SP` → `SS`, else `DS`), entry points, and basic-block boundaries. A
  `--max-insns` cap bounds runaway decodes; `--image-base` maps a single
  segment's far targets to dump offsets; `--cs-base` sets the IP at file
  offset 0.
- **** — the translator proper. IR → basic blocks
  () → CFG () → structured regions
  (`patterns/`, ) → C. Memory operands become `memb`/`memw`
  helper calls; instrumentation points (`SAFEPOINT()`) are preserved. **This is
  where most translation bugs live.** An instruction the translator can't
  emit **hard-fails loudly** (`[ir_to_c FATAL]`, exit non-zero) rather than
  being silently dropped or stubbed — a failed translate means the decoder
  walked into data (a wrong entry / bad transfer target), which is a bug to fix,
  not to paper over.
- **** — runs  then  on one
  input, threading per-segment parameters (entries, `cs_base`, `image_base`,
  `max_insns`) it reads from a `.json` sidecar. The JIT calls exactly this on
  each live segment.
- **** — the JIT's entry into the translator: writes the segment
  dump + its sidecar and invokes .
- **** — invoked by the runtime: runs , then
  `clang` to build the `.so`, with the content-keyed cross-run cache.
- **** — emits the per-program `GameConfig` C (image
  path, PSP load segment, protected slots) from the one `<name>.json`. It
  carries no dispatch table and no entry symbol — the runtime takes the entry
  `cs:ip` from the MZ header and JITs from there.

## The runtime (`runtime/`), layered

- **`core/`** — `shims.c` (the big integration surface: `memb`/`memw` writes,
  `file_mappings`, the JIT registry + `dispatch_via_binary`, WATCHW
  write-watchpoints, crash bundles, the function-patch registry, the trace/log
  channel), `snapshot.c`, `save_manager.c`.
- **`os/`** — `dos.c` (INT 21h: file I/O, memory alloc, console), `bios.c`
  (INT 10h/16h, …), `mouse.c` (INT 33h).
- **`hw/`** — device emulation split out of the shims: `io_bus.c`, `audio.c`,
  `video.c`, `keyboard.c`, `timer.c`.
- **`display/virtual_display_sdl.c`**, and the headers `include/shims.h` (the
  helper macros the generated C calls) and `include/game_config.h`.

The generated C only ever calls into the fixed runtime ABI declared in
`runtime/include/runtime_abi.h`; a contract test
(`tests/the source`) fails if the translator emits a call to
anything outside it.

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
  cross-reference files decouple decoding from structuring, so translator
  improvements reuse the same IR without repeating the Capstone pass.
- **Graph-driven structuring.** `the DiGraph` dominator/post-dominator analysis
  drives region reduction into `if`/`else`, `switch` and loops, rather than
  brittle textual pattern matching.

## Freezing (future)

The end goal is to collect the JIT-discovered chunks and link them into a fully
static native build with no runtime the reference or clang. The runtime dispatch tables
(`GameConfig.binary_dispatch`, the `DispatchFn` ABI in
`runtime/include/game_config.h`) are retained NULL-but-shaped for that freeze to
populate; today they are empty and every address routes through the JIT.

## Where things live

The toolchain is Rust (ported from the former `compiler/*.py`,
validated byte-identical). The `saisei-jitc` crate is the translator + JIT; the
`saisei` crate is the launcher.

- `saisei-jitc/src/disassemble.rs` – byte image → IR + metadata.
- `saisei-jitc/src/ir_to_c.rs` – IR → CFG → structured C.
- `saisei-jitc/src/cfg.rs` + `graph.rs` – insertion-ordered DiGraph helpers for CFG assembly and dominators.
- `saisei-jitc/src/ast.rs` – typed AST nodes that render structured control flow.
- `saisei-jitc/src/patterns.rs` – region detectors (loops, conditionals, switch).
- `saisei-jitc` subcommands `disasm` / `emit-c` / `jit-compile` – the JIT's translate → compile path.
- `saisei/` – the `saisei` launcher (build/run/play/new-game/…) + `generate_game_config`.
- `runtime/` – the C runtime (`core/`, `os/`, `hw/`, `display/`, `include/`).
- `saisei-jitc/tests/` + `saisei/tests/` – `cargo test` over the
  translator and the runtime shims (the latter compiled per-test and driven via
  dlopen/FFI).

## Building on the pipeline

- **Adding translation coverage:** implement the missing instruction/handler in
   (an unsupported op hard-fails with the exact
  mnemonic + address), or add a structuring pattern under `compiler/patterns/`.
- **Extending the runtime:** implement the missing DOS/BIOS/hardware behaviour
  in `runtime/os/` or `runtime/hw/`; the next run re-JITs affected chunks
  automatically (the toolchain hash invalidates stale ones).
- **Diagnosing corruption:** add an address to `write_watches[]` in `shims.c`
  and the next crash bundle's `lifecycle.log` names the writer's `cs:ip` +
  registers. A change is validated by the real program reaching its known scene
  — screenshot it (`--screenshot-secs N`), don't rely on unit tests alone.
