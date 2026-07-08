# Faithful dispatch refactor

**Prime directive (see CLAUDE.md):** the JIT must model real x86 exactly. The
stack holds return addresses; `call` pushes, `ret`/`retf`/`iret` pop and jump.
A faithful 8086 never asks "is this a *genuine* far return?" Every heuristic
harness in the current engine exists because the engine doesn't run a single
faithful stack. This refactor removes the harnesses by making the model correct.

## Current (unfaithful) model

- `main()` calls `config.entry` — a per-function **C wrapper** `<mod>_func_<ip>`
  that calls `<mod>_dispatch(ip, expected_retip)`.
- `<mod>_dispatch`: `for(;;) switch(pc) { case X: …; pc = next; continue; … }`.
- **Intra-chunk `call`** (target is a case in this switch): already faithful —
  `push ret_ip; pc = target; continue;`.
- **Cross-chunk `call`** (extern target, no case here): unfaithful — pushes
  ret_ip then makes a **nested C call** to the target's wrapper, recursing the C
  stack. (`handle_call`, the `extern_labels` branch.)
- **Far `call` (lcall)**: `lcall_table_impl` — `setjmp` a return env, nested C
  dispatch, the callee's `retf` `longjmp`s back. Tracks `lcall_expected_sp/ss`,
  `lcall_ret_ip/cs` for drift detection.
- **Near `ret`**: `if (popped_ip == expected_retip) return;` (C-unwind to the
  wrapper's caller) else `pc = popped_ip` / `near_ret_tail` (more C-unwind +
  near-ret-escape drift detector).
- **`retf`**: `retf_is_genuine_far_return(ip,cs)` heuristic → `longjmp` vs
  `pc`/`long_jump`. Plus the windowed-recovery in `long_jump_impl` /
  `near_ret_tail_impl` (`flat & 0xFFFF` return-IP correction).
- **ISR**: `invoke_isr` with `setjmp`/`longjmp` and its own sp drift asserts.

Harnesses to delete: `retf_is_genuine_far_return`, `expected_retip` unwind,
`near_ret_tail` escape check, windowed-recovery, `shim_check_stack_drift` /
near-ret-escape, the `lcall_return_env`/`irq_return_env` longjmp machinery, the
per-function C wrappers.

## Faithful model

One flat C stack: a **top-level loop** drives chunk dispatch; chunks never call
each other in C and never longjmp. All control transfer goes through the
emulated `cpu.r_cs:cpu.r_ip` and the emulated stack.

```
// runtime
void run_machine(void) {
  while (!machine_halted) {
    GameFunc disp = resolve_chunk(cpu.r_cs, cpu.r_ip);  // file_mappings/overlay-aware
    disp(cpu.r_ip);          // runs until control leaves this chunk's pc-space
  }
}
```

```
// every chunk dispatch
void <mod>_dispatch(int pc) {
  for (;;) {
    switch (pc) {
      case X: …; pc = next; continue;       // intra-chunk step / near jmp / near call target
      …
      default:                              // pc not a case in THIS chunk
        cpu.r_ip = (uint16_t)pc;
        return;                             // -> top-level re-resolves to the owning chunk
    }
  }
}
```

**Emission rules (), same-segment stays in-loop, cross-segment exits:**

| op | faithful emit |
|---|---|
| near `call t` | `sp-=2; memw_write(ss,sp,ret_ip); pc=t; continue;` (one rule for intra AND extern — `default:` handles the exit) |
| near `jmp t` | `pc=t; continue;` |
| near `ret` / `ret N` | `pc = memw(ss,sp); sp+=2(+N); continue;` |
| far `call s:o` | `sp-=2; memw_write(ss,sp,cs); sp-=2; memw_write(ss,sp,ret_ip); cpu.r_cs=s; cpu.r_ip=o; return;` |
| far `jmp s:o` | `cpu.r_cs=s; cpu.r_ip=o; return;` |
| `retf` / `retf N` | `uint16_t i=memw(ss,sp),c=memw(ss,sp+2); sp+=4(+N); cpu.r_cs=c; cpu.r_ip=i; return;` |
| `iret` | pop ip,cs,flags; restore flags; `cpu.r_cs/ip=…; return;` |
| indirect call/jmp through reg/mem | compute target, then the matching near/far rule above |

No `cs_base` add on the pushed return IP — a JIT chunk's `pc` IS the segment
offset (cs_base 0), so the faithful return IP is just `next_ip`. This is what
deletes the windowed-recovery: the pushed value is already segment-relative, so
`cs<<4 + ip` reconstructs the right linear address under whatever `cs` is live.

**ISR (faithful far call injected at SAFEPOINT):** when an IRQ is pending and
IF=1, SAFEPOINT pushes flags+cs+ip, sets `cpu.r_cs:ip` = the IVT handler, and
**exits the chunk** (return to top-level, which dispatches the handler). `iret`
pops flags+cs+ip and exits. No setjmp/longjmp; the handler runs as ordinary code
on the same flat loop.

## Implementation order (each step must keep the regression program green)

Pick a packed/overlay program `<name>` as the regression gate — it exercises
cross-chunk + ISR transfers heavily. Run after every step:
`saisei run <name> --headless --screenshot-secs 3` (expect exit 124, no `Unhandled pc`/abort, screenshots).

1. **Scaffold top-level loop + chunk-exit `default:`** alongside the existing
   path (gated), so resolve_chunk + the for-loop exit protocol exist and are
   tested in isolation before flipping emission.
2. **Far transfers first** (`lcall`/`ljmp`/`retf`/`iret`): emit the exit form,
   delete `lcall_table`/`retf_is_genuine_far_return`/windowed-recovery, route
   through the top-level loop. Test the regression program.
3. **Cross-chunk near `call`/`ret`**: drop the `extern_labels` wrapper branch and
   `expected_retip`; emit the in-loop form; delete `near_ret_tail` + escape
   checks + the C wrappers. Test the regression program.
4. **ISR**: replace `invoke_isr` longjmp with the SAFEPOINT far-call injection +
   faithful `iret`. Delete the isr drift asserts. Test the regression program.
5. **Delete dead machinery**: `lcall_return_env`, `irq_return_env`,
   `lcall_depth`/`expected_*` arrays, `shim_check_stack_drift`,
   `dispatch_depth_guard`. Test the regression program + run the cargo test suite.
6. **Validate a config/setup program's device-selection path** writes its
   output file (the bug this unblocks): the config descriptor should now
   survive load→parse because the stack is faithful.

## Invariants / risks

- The emulated stack is the *only* return-address store; the C stack is one
  frame deep per chunk activation. A mistranslated push/pop now shows up as a
  real wrong jump (good — visible), not silently absorbed by a wrapper.
- `resolve_chunk` must be overlay/bank-switch aware (reuse `find_file_mapping` /
  `try_dispatch_overlay_first` logic) so on-the-fly-loaded code still resolves.
- Self-referential `cs` aliasing (same code under `1010` and `1A9B`): faithful
  because the pushed IP is segment-relative and the top-level re-resolves the
  chunk for the *current* `cs` after every cross-segment transfer.
- Performance: a `return`-to-top-level per cross-chunk transfer replaces a C
  call/longjmp; expected to be comparable or better, and far simpler.
