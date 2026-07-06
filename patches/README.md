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

## The ABI (`runtime/include/game_config.h`)

```c
typedef int (*PatchFn)(uint16_t expected_retip, const char *file,
                       const char *func, int line);

typedef struct {
  const char *file;    /* binary basename the patched function lives in */
  uint32_t    file_off;/* the function's entry offset within that binary */
  PatchFn     fn;      /* the replacement/augment body */
  const char *name;    /* human label for logging */
  int         enabled; /* 0 disables the entry without removing it */
} GamePatch;

enum { PATCH_DECLINED = 0, PATCH_HANDLED = 1 };
```

When control reaches a patched function, the codegen interception hook
(`shim_patch_check`, emitted at function entry) calls your `fn`:

- return **`PATCH_HANDLED`** to stand in for the original — the caller returns
  immediately, exactly as if a normal dispatch had completed;
- return **`PATCH_DECLINED`** to fall through and run the original body
  unchanged (an "augment", e.g. observe-and-continue).

## Author helper API (`runtime/include/shims.h`)

Inside a `PatchFn` you can reach back into the game:

- `patch_call_original()` — run the original function this patch replaced.
- `patch_call_function(binary, file_off)` — call any game function by identity.
- `patch_ret_near(expected_retip)` — perform the original's near `ret`
  (replace-style: end your patch as the original would have).
- `patch_self_offset(&binary)` — `(binary, file_off)` of the function that fired.
- `shim_resolve_addr(linear, &binary)` — map a linear address to its
  `(binary basename, file_off)` identity.

Plus the usual shim surface (`memb`/`memw` reads and writes, register access)
for reading arguments and writing results.

## Writing a bundle

Create `patches/<name>/<name>.c`. Export an array of `GamePatch` entries and its
count under the two fixed symbol names the loader looks up:

```c
#include "shims.h"
#include "game_config.h"

static int my_fn(uint16_t expected_retip, const char *f, const char *fn, int l) {
  (void)f; (void)fn; (void)l;
  /* ... read args via memb/memw, do work, maybe patch_call_original() ... */
  return PATCH_HANDLED;     /* or PATCH_DECLINED to run the original */
}

const GamePatch bundle_patches[] = {
  { "game.bin", 0x0984, my_fn, "my_fn", 1 },
};
const size_t bundle_patch_count = sizeof(bundle_patches) / sizeof(bundle_patches[0]);
```

Compile it to a shared object against the runtime headers:

```bash
clang -shared -fPIC -O1 patches/<name>/<name>.c \
      -I runtime/include -o patches/<name>/<name>.so
```

## Loading a bundle

The runtime loads bundles at startup and registers them into one patch registry
(`patch_register` in `runtime/core/shims.c`). Two sources feed that registry:

- **Built-in:** entries listed in the per-game `GameConfig.patches` table.
- **Separately delivered:** a `.so` passed to the runtime binary as
  `--patch-bundle <path.so>` — `patch_load_bundle` `dlopen`s it, reads the
  `bundle_patches` / `bundle_patch_count` symbols, and registers them. This
  needs no rebuild of the game or the runtime.

Because bundles ride the same JIT dispatch, run the game binary with the JIT
environment `tools/game.py` normally sets (`SAISEI_REPO_ROOT`, `SAISEI_PYTHON`,
`SAISEI_JIT_DIR`) so first-reached code still compiles on demand.

## How it fits the whole

Patching is one of the platform's core capabilities, alongside play, save/load,
external control, and RE annotation. It is deliberately a **wrapping layer**: the
patch mirrors or intercepts one of the game's own operations while the unmodified
original keeps running the show — never a rewrite or a replay of the game.
