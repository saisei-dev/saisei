# Architecture and implementation overview

This document captures the intent, goals and technical design of Saisei so new
contributors can build on top of it with confidence.

## Purpose and goals

- **Reproduce classic DOS titles** by transforming MZ executables and binary
  overlays into readable, compilable C code.
- **Preserve behaviour** by emitting a lossless JSON intermediate
  representation (IR) plus metadata that records every segment, relocation and
  entry point discovered during disassembly.
- **Enable iteration** so that newly recovered insights can be folded back into
  the pipeline without rerunning manual steps. Regenerating artefacts should be
  deterministic and safe.
- **Support live experimentation** via runtime shims that allow the generated C
  to be compiled and executed with tracing and instrumentation hooks.

## High-level pipeline

The workflow is split across two primary stages that exchange data through the
IR files stored under `build/disassemble/<name>/`.

### Stage 1 – Binary intake and analysis (`compiler/disassemble.py`)

`disassemble.py` processes a DOS executable or raw binary in three passes:

1. **Stage 1:** Load the binary, emit header metadata, relocation tables and
   segment dumps, and record hashes in `manifest.json` for reproducibility.
2. **Stage 2:** Decode machine code with Capstone, identify entry points (both
   the implicit reset vector and any supplied with `--entry`) and capture flow
   metadata such as cross references.
3. **Stage 3:** Combine the gathered information into `program.ir.json`, a
   structured representation of instructions and operands that remains faithful
   to the source binary.

Throughout these passes the tool records side artefacts including
`header.json`, `reloc.json`, and `xrefs.json`, ensuring every decision made by
later stages can be traced back to the original bytes.

### Stage 2 – IR to C translation (`compiler/ir_to_c.py`)

`ir_to_c.py` consumes `program.ir.json` along with the source binary to
structure control flow and emit C code:

- IR instructions are grouped into basic blocks (`BasicBlock`) and assembled
  into a control-flow graph using `networkx` utilities from `compiler/cfg.py`.
- Dominator and post-dominator analysis guide an iterative region-reduction
  pass implemented in `compiler/patterns/`, recognising `if`/`else`, `switch`
  statements and loops. These constructs are represented by the AST nodes in
  `compiler/ast_nodes.py` before being rendered back into C.
- Memory references are rewritten through helper macros such as `memw` and
  `memb` provided by `runtime/include/shims.h`, preserving instrumentation
  points like `SAFEPOINT()` while keeping the generated code compilable.
- DOS interrupt preparations are coalesced into descriptive helper calls and
  optional metadata files (passed with `--metadata`) inform naming of resident
  control block fields.

The translator prioritises correctness: unsupported instructions remain in the
output as comments so the generated program mirrors the original behaviour even
when manual follow-up is required.

## Key architecture decisions

- **JSON IR as the contract.** By emitting `program.ir.json` plus manifest and
  cross-reference files, the disassembler decouples binary decoding from higher
  level structuring. Subsequent improvements to the translator can reuse the
  same IR without repeating the expensive Capstone pass.
- **Graph-driven structuring.** Using `networkx` for dominator analysis and loop
  detection allows the translator to make algorithmic decisions instead of
  relying on brittle pattern heuristics.
- **Runtime shims with logging hooks.** The generated C expects the helpers in
  `runtime/include/shims.h` and `runtime/core/shims.c`. These wrappers centralise
  DOS environment emulation, log memory accesses, and gate safe points so new
  experiments can observe behaviour without modifying the lifted code.
- **Manifest-guided automation.** `compiler/build_pipeline.py` batches work for
  multiple binaries, reads `.json` sidecars for extra entry points, and enforces
  timeouts so the pipeline remains usable in CI and local development.
- **Comprehensive regression suite.** The `tests/` directory focuses on
  individual translator capabilities (flag handling, loop formation, metadata
  usage, etc.) and disassembler edge cases, catching regressions quickly.

## Implementation map

The repository is organised so responsibilities stay focused:

- `compiler/disassemble.py` – orchestrates the three analysis stages and writes
  all artefacts needed for later steps.
- `compiler/ir_to_c.py` – performs CFG construction, structuring, instruction
  rendering and metadata integration when producing C output.
- `compiler/cfg.py` – `networkx` backed helpers for CFG assembly, dominator
  computation and loop detection.
- `compiler/ast_nodes.py` – typed AST nodes (`BasicBlockNode`, `LoopNode`,
  `ForLoopNode`, etc.) that render structured control flow.
- `compiler/patterns/` – pattern detectors for loops, conditionals and switch
  statements used during region reduction.
- `runtime/include/shims.h`, `runtime/core/shims.c`,
  `runtime/display/virtual_display_sdl.c` – runtime surface exposing hardware
  abstractions, logging helpers and SDL output so the generated C can run.
- `compiler/build_pipeline.py` – batch front-end that mirrors CI behaviour.
- `tests/` – pytest suite covering both disassembly and translation logic.

## Building on the pipeline

- **Adding new analysis:** Extend `compiler/disassemble.py` to emit additional
  metadata and expose it through `program.ir.json`. Downstream consumers will
  automatically pick up the new fields.
- **Improving translation quality:** Introduce new structuring patterns in
  `compiler/patterns/` or new AST node renderers to produce cleaner C without
  sacrificing fidelity.
- **Augmenting runtime behaviour:** Update the shim implementations to log more
  state, inject debugging hooks or emulate additional hardware features. Because
  generated programs call into these helpers, enhancements remain isolated from
  the lifted code.

This separation between decoding, structuring and runtime support keeps the
system adaptable while ensuring regenerated artefacts remain reproducible.
