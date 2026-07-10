# Runtime memory model: file ownership, bank-switching, and what's next

Many DOS programs load code on the fly: the executable calls `DOS read`
to bring chunk after chunk of binary content into a specific memory region, and
the new bytes are then executed in place. This is a JIT recompiler — the runtime
compiles each reached code segment on first execution and dispatch resolves a
live `cs:ip` to its owning JIT chunk. To decode the right bytes for a segment,
invalidate a chunk when the bytes under it change, and route a save/restore, the
shim has to answer:

> "Which file's content is at memory address `A` right now, and at what
> file_offset?"

That's the **runtime memory model**: every linear byte has an *origin* (file
+ offset). The model is what makes on-the-fly-loaded (unpacked/overlay) code
dispatchable and savable. This document explains how we maintain that origin
today, what's incomplete, and the planned extensions.

## How file_mappings works today

`runtime/core/shims.c` keeps a `file_mappings[]` array. Each entry records:

```
base, len     — linear range [base, base+len)
path          — source file (e.g. an overlay archive)
file_offset   — byte offset within the source file
canonical_cs  — segment the binary's translated code expects in cs:
loader_cs/ip  — cs:ip of the game-side instruction that triggered the LOAD
loader_ss/sp + loader_stack[8] — simulated-stack snapshot at LOAD time
data[]        — the bytes that were loaded (for mutation comparison)
```

Every DOS read shim (`dos_read_file_impl`) calls `register_file_mapping`
after the bytes land in memory. Entries are append-only; **newest entry
covering an address wins** (`find_file_mapping` iterates from end to start
and returns the first range hit).

`resolve_and_run_chunk`/`dispatch_via_binary` use this lookup to resolve an
address to its owning JIT chunk (or to compute `(file, file_off)` for stale-chunk
invalidation and save/restore routing). When no chunk covers the address, the
runtime JIT-compiles the live 64KB segment at `cs<<4` and dispatches into it.

This works perfectly **as long as `file_mappings` actually describes what's
in memory**. The problem is that games modify memory in ways the LOAD path
never sees — and a JIT chunk decoded from the *old* bytes must be invalidated
when the bytes under it move.

## The bank-switching problem

Some programs use a word-swap loop as their overlay bank-switch primitive:
every iteration exchanges one word between `[ds:si]` and `[es:di]`, advancing
both. After enough iterations the two memory regions have **swapped
contents**. No DOS read involved; no `register_file_mapping` triggered;
`file_mappings` is stale immediately.

Concrete failure: two overlay chunks A and B get swapped. A subsequent CALL
into the region looks up `file_mappings` → still says chunk A owns that range
→ dispatches into chunk A's decoding → hits zero padding (chunk A's tail) →
abort. Real DOS would execute the bytes that **are now** there (chunk B's
content), which is real code.

## Current stop-gap: pattern-matched swap loops

 (`_try_match_swap_loop_w`) recognises two specific
4–5-instruction loop bodies that implement this exact swap and replaces
each with one `shim_swap_regions_w(es, di, ds, si, cx, DF)` call.

The shim does two things:

1. **Byte swap**: exchange the actual bytes between the two regions (one
   `for` loop, no dispatch overhead).
2. **Ownership update**: walk `file_mappings`; any entry whose
   `[base, base+len)` lies entirely inside one swap region gets its `base`
   relocated to the matching offset in the other region
   (`swap_file_mappings_in_regions`). Entries that straddle a swap
   boundary warn instead of silently splitting.

After the call: bytes have moved, `file_mappings` correctly says the new
chunk owns each address, dispatch routes through the right dispatcher.

### Limitations of the stop-gap

Pattern matching is brittle:

- Only recognises the two exact instruction sequences (for example
  `lodsw / mov dx,es:[di] / stosw / mov [si-2],dx / loop` and
  `mov ax,es:[di] / movsw / mov [si-2],ax / loop`). Another program
  (or a recompilation of the same one) using different temp
  registers, byte-sized variants, `rep movsb`, or any instruction-order
  reshuffle is invisible to the matcher.
- Doesn't handle copies (one-way moves of bytes between regions), only
  swaps. A game-side `memcpy` from a chunk-mapped source to a chunk-
  mapped dest would leave the dest's mapping stale.
- Doesn't handle scattered writes — code that writes one byte at a time
  to assemble a function in place would never trigger the pattern.

## What's planned

The pattern detector covers one program's bank-switch primitive but
doesn't generalise. Three escalating strategies, listed in order of
increasing scope:

### (A) Extend the pattern catalogue — current

Add new entries to `_try_match_swap_loop_w` for each new
swap/copy idiom we encounter. Cheap per-pattern, no runtime cost, fails
loud (compile error on unmatched bytes used as code). The right move for
"a small set of well-known loops in one program" — which is where we are.

### (B) Per-byte ownership tags + register taint

The principled solution. Maintain a `mem_tags[MEMORY_SIZE]` shadow array
where each byte stores its source `(file_id, file_offset)`. Augment every
register with a shadow `r_tag_<reg>` that records "this value came from
linear address X". Generate `memw_tagged_read(seg, off, &r_tag_dst)` and
`memw_write_tagged(seg, off, val, r_tag_src)` from the chunk emitter so the tag
propagates through every register-mediated copy. Arithmetic ops clear
the tag. `dispatch_via_binary` queries `mem_tags[addr]` instead of
`find_file_mapping(addr)`.

Pros: handles any move pattern uniformly (rep movs, manual lods/stos,
manual swap, decryptor loops, byte-at-a-time writes — anything that
flows a value from one address to another through a register). No
pattern matching needed.

Cons: invasive codegen change (every emitted `memw()` / `memw_write()`
call site must thread the tag); ~8 MB shadow memory for 2 MB virtual
memory; care needed for arithmetic-on-pointers cases where the tag
"should" propagate but the value technically went through ALU.

### (C) JIT of synthesized code — done

Some games (decryptors, packers, self-modifying anti-tampering) generate
bytes that don't trace back to any file. This is exactly what the JIT
handles today: when control reaches an address with no compiled chunk, the
runtime dumps the live 64KB segment and recompiles it to native (not an
interpreter — a recompiler), so procedurally-generated code runs at native
speed. Origin tracking (A)/(B) still matters to invalidate a chunk when the
bytes under it move; the JIT then re-decodes from the new live bytes.

## Entry discovery — resolved on demand by the JIT

Under the JIT this problem largely dissolves. Indirect targets (function-
pointer tables in game data, pushed return addresses, computed jumps) don't
need to be pre-seeded as decoder entries: when the game transfers to one, the
runtime JIT-compiles the segment at the **actual target value it computed at
runtime**, so the decoder never has to guess an address from static data and
never walks data as code. There is no `extra_entries` seeding and no autoheal.
A transfer to a genuinely wrong address (stack corruption upstream) fails loud —
an unmapped `cs:ip`, or a chunk whose bytes won't translate — which is the signal
to find the unfaithful shim/codegen, per the project's hard rule: **no hand-
curated address discovery and no heuristics that guess.**

## Diagnostic infrastructure

The tools for tracing memory corruption and dispatch decisions live in
`runtime/core/shims.c`: the WATCHW write-watcher (`write_watches[]`),
`loader_stack` serialization, and lifecycle dispatch enrichment. Short
version: when a bug looks like "memory at address X silently changed", add X
to the `write_watches[]` table in `shims.c` and the next crash bundle's
`lifecycle.log` will identify the writer's cs:ip + register state.
