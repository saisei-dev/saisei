# Runtime port notes (C → Rust)

The runtime (`runtime/` crate, formerly `runtime-rs/`) is a full port of the
original C runtime. The port is byte-exact at the ABI level; these notes record
the deliberate re-architectures and conventions that differ from the C source,
for anyone comparing behavior against the original design.

## Key re-architectures / deviations

1. **EXEC nest (no setjmp):** module static `EXEC_CHILD_EXIT_PENDING: bool` +
   `exec_nest_status[]`. `shim_exec_child_terminate`: when `exec_nest_depth>0`,
   stores status, sets `EXEC_CHILD_EXIT_PENDING=true`, returns **1**
   (dos.rs's dos_exit_impl already returns on nonzero). `run_machine`'s loop,
   after `resolve_and_run_chunk`, checks the flag, clears it, and `return`s to
   `shim_exec_run_child`, which then restores parent regs. Register
   save/restore + critical_depth/IF handling identical to the C.
2. **Fatal-capture test seam:** `report_unmapped` non-armed path = build crash
   bundle + `exit(1)` (faithful). Armed path (unit tests only, via
   `shim_arm_fatal_capture`) sets `shim_fatal_captured/addr/kind` and RETURNS
   (no unwind). The C `shim_fatal_env` jmp_buf and `call_table`'s setjmp were
   dropped (report_unmapped no longer longjmps).
3. **Dead longjmps dropped:** `irq_return_env` / `lcall_return_env` removed.
   The `lcall_depth>0` branch in `shim_drain_pending_tail_dispatch` keeps the
   same `[WARN] contained lcall fault` crash log then `abort()` (was an
   unreachable longjmp in the C).
4. **Entry point:** the C `main(argc, argv)` is `saisei_main` (still
   `#[no_mangle] extern "C"`); the `saisei-game` bin crate's Rust `main`
   rebuilds C argv and calls it. `atexit(virtual_display_shutdown)` uses a
   safe trampoline.
5. **`game_config`:** the runtime carries a `#[linkage = "weak"]` default (all
   zeros/nulls); the generated per-game config in the `saisei-game` bin is a
   strong symbol that overrides it. Unit tests dlopen the runtime cdylib alone
   and get the weak default.
6. **PNG output:** `stb_image_write` was replaced by video.rs's self-contained
   stored-deflate PNG encoder; emitted bytes differ from stb's, decoded pixels
   are identical.
7. **FORCE_EXIT_AFTER_10S:** was `-DFORCE_EXIT_AFTER_10S` via CFLAGS; now the
   cargo feature `force_exit_after_10s` (launcher: `--features
   force_exit_after_10s` on `saisei-cli run`).

## Cross-module structs mirrored in shims.rs (repr(C), byte-exact)

CpuState (via crate::cpu), PITState, BiosVideoState, CgaState, VgaState,
Opl2State, KbdEntry/KbdState, IoDevice, InterruptSnapshot, ShimKbdState,
ShimFileMappingView, ShimRuntimeState (v6), ShimTailDispatchState,
GameConfig/CallTarget/ProtectedSlot/BinaryDispatch/GamePatch,
FileMapping/JitChunk/ShimCaseKeys/Alias*/CGEdge/StackWriteEvent/WriteWatch,
PSP/ExecParamBlock.

These layouts are FROZEN: save/restore snapshots serialize them byte-for-byte
(`snapshot.rs` static-asserts the sizes), and the JIT chunk prelude
(`saisei-jitc/rt/saisei_rt.rs`) mirrors `CpuState` and `ShimTailDispatchState`
— edit the prelude and the runtime together.

## Conventions

- `cstr!("...")` macro → `*const c_char`. `SHIMS_FILE = "core/shims.c"` keeps
  the original call-site `__FILE__` tags stable in traces/logs.
- memw/memb/rcb macro sites call the `_impl(seg,off,SHIMS_FILE,cstr!("<fn>"),
  <original C line>)` forms so trace output matches the C runtime's.
- Variadic fns: `args: ...` is `VaList` (impl Clone = va_copy); forwarded via
  vsnprintf/vfprintf (nightly `c_variadic` — see rust-toolchain.toml).
- C `__attribute__((constructor))` → `#[link_section = ".init_array"]`;
  destructors → `.fini_array`.
