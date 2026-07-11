# Performance backlog

State after the 2026-07-10 throughput rework — **all six backlog items are
done**. For the timing-model invariants (JIT_BUDGET units, shim_time_sync,
shim_idle_wait, pacing) see the comments in `runtime/src/timer.rs` and
`safe_point_impl` in `runtime/src/shims.rs`.

## Where we are (measured 2026-07-10, after completion)

- The emulated CPU is a fixed-speed model with **per-class instruction
  costs**: each basic block debits its summed instruction weights (≈ 386
  cycles / 3.3 — `insn_weight` in `saisei-jitc/src/codegen.rs`), rep
  iterations and transfer shims debit their real costs, and
  `jit_ns_per_instr` (`runtime/src/timer.rs`) is **40ns/unit ≈ a 486-class
  machine** (was 100ns ≈ 386DX-33 with flat 1-unit costs).
- Worst case measured (Zeliard's attract loop, historically 0.80× and the
  gate for everything): **1.00× real time with ~63–75% host idle** at the
  486-class speed. Raw sustained throughput in that phase is ~37M units/s
  against the modeled 25M units/s. Before this rework the same phase ran
  0.65× with the host flat out (~6.5 MIPS equivalent).
- DM boot→dungeon verified end-to-end (screenshot of the dungeon entrance);
  title screen paces exactly 1.0×.
- JIT compile speed: the block-local register cache made chunk `.rs` bodies
  much cheaper for rustc — the worst DM chunk (10.9k lines) dropped from
  ~5.4s to **~1.0s**; a cold DM boot reaches the title screen inside the
  first 10s run.
- `saisei build <name> --warm [--warm-secs N]` drives the real program
  headless once and ships the hot cache with the bundle (60s default). No
  ahead-of-time decode, no heuristics — the cache is the byproduct of a real
  run. First `saisei play` after a warm build starts hot.

## What was done (by backlog item)

1. **Dispatcher cost per cross-chunk far transfer** — DONE.
   - Per-segbase chunk index (`jit_seg_heads` + `JitChunk.next_same_seg`,
     newest-first bucket lists): the dispatch hot path resolves the live-cs
     chunk in O(chunks-at-this-cs); the full registry scan survives only as
     the cs-alias fallback.
   - `try_patch_at` early-out: min/max + 64-bit bloom over resolved patch
     addresses; unresolved patches re-resolve only when file mappings change
     (`patch_reg_lin_stamp`), not per transfer.
   - Wrapper bookkeeping (enter/leave_binary, tail_dispatch_save/restore,
     the drain gate) inlined into the chunk prelude via exported statics —
     the per-function wrapper makes no FFI call unless a tail dispatch is
     actually pending.
   - Dispatch-trace lifecycle entries (CALL/LCALL/JMP/LJMP/NRET — several
     100k/s) are recorded as **binary ring records** in silent runs and
     formatted only at dump time, byte-identical to the eager output
     (`LifecycleDispatchRec`, `lifecycle_format_rec`). Verbose /
     `--lifecycle-file` runs keep the eager path. Alias self-seeding now
     fires on new callgraph edges (same first-call moments) instead of a
     per-transfer registry scan + save.
   - Payoff taken: `jit_ns_per_instr` 100 → 40 (486-class; see constraint
     note in timer.rs before lowering further — 25ns would exceed the
     measured worst-case raw).

2. **Per-class instruction costs in codegen** — DONE. `insn_weight`
   (mul 5, div 12, string/push/pop/mem-operand 2, int 10, etc.); block debit
   is the compile-time sum; inline rep loops (`rep stosw`/`lodsb`,
   `repe`/`repne`) debit their dynamic iteration counts; the rep block shims
   already debited count. The virtual clock is faithful to real 386 cycle
   ratios and heavy instructions buy proportionally more virtual time per
   host cycle.

3. **SS-segment fast path** — DONE. The stack-op forensics ring
   (`stack_write_ring`) is exported and appended **inline** from the chunk
   prelude (same `[-16,+256]`-of-SP filter, same fields); SS words no longer
   cross the FFI boundary, and `memw_write_impl`/`memb_write_impl` are
   page-flag-gated so runtime-internal pushes (lcall/call_table) skip the
   watch/warn walks on unflagged pages too. Forensic timestamps (ring +
   lifecycle `t=`) moved from per-event host `clock_gettime` to the virtual
   clock — deterministic and ~free. This was the single largest steady-state
   win (~40% of host time in the attract loop).

4. **Block-local register caching in codegen** — DONE. The dispatch loop
   threads a `&mut Regs` (prelude struct: 8 GP regs + 7 flags cached; ip and
   segment registers write-through) through every block fn; emitted code
   uses `r.`-prefixed accessors (`localize_regs` post-pass). The global
   `cpu` is coherent at every runtime call and at dispatch exit: mem/rcb/io
   methods spill before entering the runtime (impls never write guest
   registers — no reload), budget/transfer/DOS/BIOS/string-shim methods
   spill + reload. Bonus: the noalias local made rustc ~5× faster per chunk.

5. **First-ever-run entry discovery** — DONE, deterministically (no decode
   heuristics, per the prime directive): `saisei build --warm` runs the real
   boot path headless once, waits for the speculative compiles to quiesce,
   and reports the shipped cache size.

6. **Safepoint slow-path micro-costs** — DONE. The host clock read + pacing
   check is skipped when virtual time moved <50µs since the last probe
   (rep-debit bursts land in the same virtual instant); pacing slack is
   200µs so the added drift is invisible. The PIT div/mod by 1e9 was left
   exact — the divisor is a compile-time constant and LLVM already
   strength-reduces it; determinism is worth more than the last cycle.

## Non-goals / settled

- **25ns/unit (40M units/s)**: exceeds the measured worst-case raw
  throughput (~37M units/s in Zeliard's attract loop). Re-measure that phase
  after any further throughput work before touching `jit_ns_per_instr`.
- **opt-level for chunks**: opt1 stays; compiles are ~1s worst-case now and
  batching+speculation+`--warm` cover the first-run feel. opt2/3 belongs to
  the future frozen static build.
- **DM's "Divide error" ×2 at startup**: the game's own EGA driver catches
  its own #DE speed probe (string lives in `games/dm/EGA`) and continues.
  Saturates at any playable speed; avoiding it would need sub-1-MIPS 8086
  pacing. Benign, permanent.
- **Host-clock-driven virtual time**: gone by design. It made calibration
  loops nondeterministic and let JIT compile stalls dump timer-tick backlogs
  into the game. Do not reintroduce host time into the PIT chain; pacing is
  the only place host and virtual time meet. (Forensic timestamps now use
  the virtual clock for the same reason.)
