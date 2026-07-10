# Patch bundles

A **patch** replaces or augments a single game function without touching the
game image or the runtime. It is the supported way to change a game's behaviour
(fix, instrument, or reskin a function) on top of the faithful JIT — the
original binary stays the driver; a patch just intercepts one function.

This folder holds patch bundles, one per subdirectory (`patches/<name>/`). It
ships empty by design: bundles are project- and game-specific. The mechanism
below is permanent; the bundles are not.

## What a patch identifies

A patch is bound to a function by its **stable identity** — the pair
`(binary basename, file_off)`: the basename of the binary the function lives in
(e.g. `game.bin`) and the function's entry offset within that binary. The
dispatcher resolves every runtime address to this pair via the `file_mappings`
origin table, so **one patch applies across every `cs`-alias** the same physical
code is reached under, and across JIT recompiles. You never patch a live linear
address; you patch a function's identity.

## The ABI

A bundle is a shared object exporting two fixed symbols over the C ABI —
`bundle_patches` (an array of `GamePatch`) and `bundle_patch_count`. In Rust
terms (this is the layout the runtime's `game_config` types use):

```rust
type PatchFn = Option<
    unsafe extern "C" fn(
        expected_retip: u16,
        file: *const c_char,
        func: *const c_char,
        line: c_int,
    ) -> c_int,
>;

#[repr(C)]
pub struct GamePatch {
    pub file: *const c_char, // binary basename the patched function lives in
    pub file_off: u32,       // the function's entry offset within that binary
    pub fn_: PatchFn,        // the replacement/augment body
    pub name: *const c_char, // human label for logging
    pub enabled: c_int,      // 0 disables the entry without removing it
}

pub const PATCH_DECLINED: c_int = 0;
pub const PATCH_HANDLED: c_int = 1;
```

When control reaches a patched function, the codegen interception hook
(`shim_patch_check`, emitted at function entry) calls your `fn`:

- return **`PATCH_HANDLED`** to stand in for the original — the caller returns
  immediately, exactly as if a normal dispatch had completed;
- return **`PATCH_DECLINED`** to fall through and run the original body
  unchanged (an "augment", e.g. observe-and-continue).

## Author helper API

Inside a `PatchFn` you can reach back into the game (declare these
`extern "C"`; they resolve from the host game binary at dlopen, exactly like a
JIT chunk's shim calls):

- `patch_call_original()` — run the original function this patch replaced.
- `patch_call_function(binary, file_off)` — call any game function by identity.
- `patch_ret_near(expected_retip)` — perform the original's near `ret`
  (replace-style: end your patch as the original would have).
- `patch_self_offset(&binary)` — `(binary, file_off)` of the function that fired.
- `shim_resolve_addr(linear, &binary)` — map a linear address to its
  `(binary basename, file_off)` identity.

Plus the usual shim surface (`memb`/`memw` reads and writes, register access)
for reading arguments and writing results. The JIT chunk prelude
(`saisei-jitc/rt/saisei_rt.rs`) is the reference for these signatures — a patch
bundle binds the same ABI.

## Writing a bundle

Create `patches/<name>/` as a tiny cdylib crate (or a single `.rs` compiled
with `rustc --crate-type cdylib`). Export the two fixed symbols the loader
looks up:

```rust
use core::ffi::{c_char, c_int};

unsafe extern "C" fn my_fn(
    _expected_retip: u16,
    _file: *const c_char,
    _func: *const c_char,
    _line: c_int,
) -> c_int {
    // ... read args via memb/memw, do work, maybe patch_call_original() ...
    1 // PATCH_HANDLED (0 = PATCH_DECLINED to run the original)
}

#[no_mangle]
pub static bundle_patches: [GamePatch; 1] = [GamePatch {
    file: c"game.bin".as_ptr(),
    file_off: 0x0984,
    fn_: Some(my_fn),
    name: c"my_fn".as_ptr(),
    enabled: 1,
}];

#[no_mangle]
pub static bundle_patch_count: usize = 1;
```

```bash
rustc --edition 2021 --crate-type cdylib -C panic=abort \
      -o patches/<name>/<name>.so patches/<name>/<name>.rs
```

## Loading a bundle

The runtime loads bundles at startup and registers them into one patch registry
(`patch_register` in `runtime/src/shims.rs`). Two sources feed that registry:

- **Built-in:** entries listed in the per-game `GameConfig.patches` table.
- **Separately delivered:** a `.so` passed to the runtime binary as
  `--patch-bundle <path.so>` — `patch_load_bundle` `dlopen`s it, reads the
  `bundle_patches` / `bundle_patch_count` symbols, and registers them. This
  needs no rebuild of the game or the runtime.

Because bundles ride the same JIT dispatch, run the game binary with the JIT
environment the launcher normally sets (`SAISEI_REPO_ROOT`, `SAISEI_JITC`,
`SAISEI_JIT_DIR`) so first-reached code still compiles on demand.

## How it fits the whole

Patching is one of the platform's core capabilities, alongside play, save/load,
external control, and RE annotation. It is deliberately a **wrapping layer**: the
patch mirrors or intercepts one of the game's own operations while the unmodified
original keeps running the show — never a rewrite or a replay of the game.
