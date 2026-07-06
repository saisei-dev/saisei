#pragma once

#include <stddef.h>
#include <stdint.h>

/* Type for generated functions from translated binaries.
 *
 * ``expected_retip`` is the IP value the caller pushed onto the simulated
 * stack as the return address.  Every function pops on its own ``ret`` and
 * compares the popped value against ``expected_retip``: on a match it returns
 * to the C caller, otherwise it tail-dispatches to the popped IP (the DOS
 * ``push imm16; ret`` idiom).  The caller therefore does NOT pop after the
 * call; the callee owns the retIP cleanup. */
typedef void (*GameFunc)(uint16_t expected_retip, const char *file,
                         const char *func, int line);

typedef struct {
  uint32_t addr;
  const char *file;
  GameFunc fn;
} CallTarget;

/* A memory range holding an indirect ljmp/lcall dispatch target the game reads
 * at runtime. See GameConfig.protected_slots. */
typedef struct {
  uint32_t lo, hi;
  const char *name;
} ProtectedSlot;

/* Per-binary dispatch function. Each translated binary has one big
 * ``<prefix>_dispatch(pc, ...)`` switch with a case for every decoded
 * address. The runtime resolves a linear address into (file, file_offset)
 * via the file_mappings table, looks up the matching DispatchFn here, and
 * invokes it with ``pc = file_offset``. This eliminates the need to
 * pre-register a CallTarget for every intra-binary entry point. */
typedef void (*DispatchFn)(int pc, uint16_t expected_retip, const char *file,
                           const char *func, int line);

typedef struct {
  const char *file;    /* basename, e.g. "sound.drv" */
  const char *module;  /* C source basename without extension, e.g. "sound" */
  uint16_t cs_base;    /* IP at file_off 0; subtract from popped IP to get pc */
  DispatchFn fn;
} BinaryDispatch;

/* Function-patch API. A patch replaces or augments a game function identified
 * by (binary basename, file_off) -- the stable identity the dispatcher resolves
 * addresses to, so one patch applies to JIT-compiled code across cs-aliases.
 * The fn returns PATCH_HANDLED to stand in for the original (the caller returns
 * immediately, like a completed dispatch) or PATCH_DECLINED to fall through and
 * run the original body. Patches register at startup from the built-in
 * game_config.patches table, or from separately-delivered .so bundles loaded via
 * patch_load_bundle (see the patch registry in shims.c). */
enum { PATCH_DECLINED = 0, PATCH_HANDLED = 1 };

typedef int (*PatchFn)(uint16_t expected_retip, const char *file,
                       const char *func, int line);

typedef struct {
  const char *file;    /* binary basename the patched function lives in */
  uint32_t file_off;   /* the function's entry offset within that binary */
  PatchFn fn;          /* the replacement/augment body */
  const char *name;    /* human label for logging */
  int enabled;         /* 0 disables the entry without removing it */
} GamePatch;

typedef struct {
  const char *name;
  const char *program_path;
  GameFunc entry;
  const CallTarget *call_targets;
  size_t call_target_count;
  const BinaryDispatch *binary_dispatch;
  size_t binary_dispatch_count;
  /* Protected dispatch slots + the load segment during which init writes to
   * them are allowed (a write from any other cs after init is the bug). These
   * are GAME-SPECIFIC, so they live in the per-game config, not the runtime; a
   * game that declares none (count==0) gets no check. */
  const ProtectedSlot *protected_slots;
  size_t protected_slot_count;
  uint16_t init_cs;
  /* PSP load segment for this game. 0 = use the runtime default (0x1000).
   * A game that needs more conventional RAM than the default layout leaves
   * free sets this lower, mirroring a real minimal-DOS machine that loaded the
   * program closer to the bottom of conventional memory. The runtime's
   * LOAD_SEG = psp_seg + 0x10; the loader maps the program image there and the
   * JIT decodes each reached segment at its live base. */
  uint16_t psp_seg;
  /* Built-in function patches registered at startup (NULL/0 = none). Bundles
   * delivered separately via patch_load_bundle add to the same registry. */
  const GamePatch *patches;
  size_t patch_count;
} GameConfig;

extern const GameConfig game_config;
