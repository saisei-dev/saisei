#ifndef SHIMS_H
#define SHIMS_H

#include "cpu_state.h"
#include "rcb_fields.h"
/* runtime_abi.h is the explicit contract for what generated code (the
 * translated artifacts) may call into here. The call-surface
 * declarations themselves still live below in this file; runtime_abi.h
 * carries the authoritative symbol manifest, enforced by
 * tests/the source. */
#include "runtime_abi.h"
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <stdbool.h>
#include <setjmp.h>

extern uint8_t *virtual_memory;
extern const size_t SHIM_MEMORY_SIZE;
extern uint8_t interrupt_shadow;
extern uint8_t isr_depth;
extern uint8_t lcall_depth;
extern uint16_t dispatch_depth;
extern uint8_t critical_depth;

/* Active-binary stack — see shims.c. Push name on entering an _impl
 * wrapper, pop on exit. save_manager refuses captures while empty;
 * snapshot saves the top for restore-time routing. */
void shim_enter_binary(const char *name);
void shim_leave_binary(void);
const char *shim_active_binary(void);

/* Fast case-key membership test. Used by save_manager to validate that
 * a captured cs:ip lies on an instruction boundary in its binary's
 * dispatch switch — restore would otherwise land in the middle of a
 * case body and trip unhandled_pc. The translated near-ret handler no
 * longer consults this (the switch IS the case-key set at runtime).
 * Returns 1 if file_off is a case key in module's dispatch switch. */
int shim_pc_is_case_key(const char *module, uint32_t file_off);

/* True iff seg:off is a dispatch case-key in a live JIT chunk at this segment
 * base -- a JIT-execution resting point the save_manager can snapshot and a
 * restore can re-enter via dispatch_via_binary (makes the JIT build saveable). */
int shim_pc_is_jit_case_key(uint16_t seg, uint16_t off);

/* Find a binary's dispatch function pointer by module name (e.g. an overlay module's basename).
 * Used by snapshot_restore_and_resume to route into the right binary
 * directly, without going through cs:ip-arithmetic file_mapping lookup
 * (which doesn't round-trip during canonical_cs swaps or inline _impl
 * calls). Returns NULL if no binary matches. */
typedef void (*ShimDispatchFn)(int pc, uint16_t expected_retip,
                               const char *file, const char *func, int line);
ShimDispatchFn shim_dispatch_fn_by_module(const char *module);

/* Function-patch author API (see the patch registry in shims.c and the
 * GamePatch type in game_config.h). A patch bundle's fn calls these to defer to
 * or reach beyond the original game function it stands in for:
 *   - patch_call_original: run the ORIGINAL function this patch replaced.
 *   - patch_call_function: call any game function by (binary basename, file_off).
 *   - patch_ret_near:      perform the original's near `ret` (replace style).
 *   - patch_self_offset:   (binary, file_off) of the function that fired.
 *   - shim_resolve_addr:   linear addr -> (binary basename, file_off).
 * shim_patch_check is the codegen interception hook; patch_load_bundle loads a
 * .so exporting `bundle_patches[]` / `bundle_patch_count`. */
int  shim_patch_check(uint32_t linear, uint16_t expected_retip);
void patch_call_original(void);
void patch_call_function(const char *binary, uint32_t file_off);
void patch_ret_near(uint16_t expected_retip);
uint32_t patch_self_offset(const char **binary_out);
uint32_t shim_resolve_addr(uint32_t linear, const char **binary_out);
void patch_load_bundle(const char *so_path);

/* Build a rich diagnostic for a binary's dispatch fall-through (the
 * default case in <module>_dispatch). With literal-emission translation
 * the dispatch case set is complete by construction, so landing here
 * means stack corruption upstream of the RET (popped a mid-instruction
 * IP) or an unmapped chunk-swap target — not a missing case key.
 * Appends:
 *   - cs:ip and computed linear at the moment of failure
 *   - the file_mapping currently picked for that linear (which binary
 *     and chunk file_offset is "live")
 *   - any OTHER overlapping mappings at the same linear (chunk-swap
 *     candidates with their alternative file_offsets — these are the
 *     usual culprits when a runtime-computed target resolves into the
 *     "wrong" chunk after an overlay swap)
 * Returns the number of bytes written (clamped to cap). */
int shim_unhandled_pc_report(const char *module, int pc,
                             char *out, size_t cap);

/* Stack-drift detector. Called at the ISR/lcall setjmp resume sites in
 * shims.c when sp doesn't equal sp at boundary entry. Aborts with a
 * stack_drift bundle containing the recent push/pop history. */
void shim_check_stack_drift(const char *site, uint16_t expected_sp,
                            uint16_t actual_sp, const char *file,
                            const char *func, int line);
extern uint8_t irq0_pending;
extern uint64_t last_host_time_ns;
extern uint64_t pit_cycle_accum;
/* Virtual/scaled monotonic clock in ns (speedup-adjusted). Shared with the
 * hardware-device layers (audio timers, etc.); will move into hw/timer. */
uint64_t shim_scaled_monotonic_ns(void);
uint64_t shim_host_monotonic_ns(void);
extern double emulation_speedup;
extern uint64_t host_time_origin_ns;

/* Last software interrupt snapshot (interrupt-trace diagnostic, shared
 * with the DOS layer's logging). */
typedef struct {
  uint8_t valid;
  uint8_t int_no;
  uint16_t ax_before, bx_before, cx_before, dx_before;
  uint16_t ds_before, es_before, ss_before, sp_before, cs_before, ip_before;
  uint16_t ax_after, bx_after, cx_after, dx_after;
  uint16_t ds_after, es_after, ss_after, sp_after, cs_after, ip_after;
  const char *file;
  const char *func;
  int line;
} InterruptSnapshot;
extern InterruptSnapshot last_sw_interrupt;
void log_last_sw_interrupt_snapshot(void);
extern void *dta_ptr;  /* DOS Disk Transfer Area pointer */

/* DOS layer shared state/macros (os/dos.c owns the services; the handle
 * table + critical-section + memory size are core facilities it uses). */
#define BIOS_VIDEO_PARAM_SEG 0xF000
#define BIOS_VIDEO_PARAM_OFF 0x0200
/* Segment where the program's PSP loads. On real DOS this depends on how much
 * low conventional RAM the resident kernel + drivers consume, so it is not a
 * universal constant: a minimal-config machine loads programs lower (leaving
 * more conventional RAM free) than a machine with many TSRs/drivers. It is a
 * runtime value (default 0x1000) the game config can override per game.
 * LOAD_SEG/ENV_SEG derive from it. */
extern uint16_t psp_seg;
#define PSP_SEG (psp_seg)
#define DEFAULT_PSP_SEG 0x1000
#define MEMORY_SIZE (1 << 21)
/* Top of conventional memory (640 KB = segment 0xA000). Segments at/above this
 * are video RAM (0xA000+) and the upper-memory/ROM area, not DOS-allocatable
 * conventional RAM. DOS hands out blocks only below this ceiling, so the DOS
 * memory services (AH=48h/49h/4Ah, INT 12h) and the PSP "top of memory"
 * paragraph all clamp here. */
#define CONVENTIONAL_TOP_SEG 0xA000
#define MAX_DOS_HANDLES 20 /* mirrors DOS 5 default limit */
#define CRITICAL_ENTER() \
  critical_section_enter(__func__, __FILE__, __func__, __LINE__)
#define CRITICAL_EXIT() \
  critical_section_exit(__func__, __FILE__, __func__, __LINE__)
extern FILE *handles[MAX_DOS_HANDLES];
extern char *handle_paths[MAX_DOS_HANDLES];
extern bool handle_paths_owned[MAX_DOS_HANDLES];
int is_standard_handle(uint16_t handle);
FILE *fopen_case_insensitive(const char *path, const char *mode);
extern uint16_t next_free_seg;
extern uint16_t program_min_block_paras;
extern uint8_t null_guard_initial[16];
void shim_exit_with_message(const char *fmt, ...);
/* Load an MZ EXE image at `load_seg` (the resident program uses LOAD_SEG with
 * is_child=0; an EXEC'd child uses a segment ABOVE the parent with is_child=1
 * so it does not clobber the parent or perturb the resident-block globals). */
int load_executable(const char *path, uint16_t load_seg, int is_child,
                    uint16_t *out_cs, uint16_t *out_ip,
                    uint16_t *out_ss, uint16_t *out_sp);
/* INT 21h AH=4Bh AL=03h Load Overlay: load `path` at `load_seg`, add
 * `reloc_factor` to relocatable items, no PSP/run. Returns 0 on success. */
int load_overlay(const char *path, uint16_t load_seg, uint16_t reloc_factor);
void critical_section_enter(const char *name, const char *file,
                            const char *func, int line);
void critical_section_exit(const char *name, const char *file,
                           const char *func, int line);
uint32_t wrap_segoff_addr(uint32_t base, uint32_t offset);
uint32_t mask_addr(uint32_t addr);
void write_watch_log(uint32_t addr, size_t size, uint32_t value,
                     const char *file, const char *func, int line);

/* Mark stale any runtime-JIT chunk whose decoded code was just overwritten by
 * an overlay LOAD over [lin, lin+len) (DOS file read / block relocation) so the
 * next dispatch re-decodes from live memory. No-op when no JIT chunks exist. */
void shim_jit_invalidate_code_range(uint32_t lin, uint32_t len);
/* Same, but ALSO drops the chunk the writer is executing inside -- for a
 * replace-self overlay/EXEC that loads a new program over the loader's own
 * code and then transfers control to it. */
void shim_jit_invalidate_code_range_force(uint32_t lin, uint32_t len);

/* Internal helpers shared with the hw/video layer: BIOS-data-area video
 * geometry, raw word memory access, and a seg:off linear copy. (These will
 * partition further as the OS/BIOS layer is split out.) */
extern int headless_mode;
extern int current_display_width;
extern int current_display_height;
extern int virtual_display_buffer;
extern uint8_t irq_pending[256];
extern int shim_input_phase_started;
uint8_t ascii_to_scan(uint8_t c);  /* keyboard helper */
uint16_t memw_raw_read(uint16_t seg, uint16_t off);
void memw_raw_write(uint16_t seg, uint16_t off, uint16_t value);
void copy_linear_from_segoff(uint16_t seg, uint16_t off, size_t len,
                             uint8_t *dst);
uint16_t bios_video_columns(void);
uint16_t bios_page_stride(void);
uint16_t bios_video_rows(void);

/* Generic hardware-device bus (IO-port routing). inb/outb dispatch ports
 * to registered devices before the legacy in-core handling. */
#include "device.h"

/* OPL2 (AdLib) sound card — hw/audio. Opl2State + the device live there. */
#include "audio.h"

/* Video card (VGA/CGA/BIOS-video) — hw/video. Structs + device there. */
#include "video.h"

/* Keyboard controller — hw/keyboard. KbdState + the device there. */
#include "keyboard.h"
extern bool a20_enabled;
void a20_set_enabled(bool enabled);

/* PIT (8254) + virtual clock — hw/timer. PITState + state there. */
#include "timer.h"

/* Shim runtime state. Everything in the simulator that lives in C globals
 * (i.e., NOT in the simulated 8086 memory image) which a clean snapshot
 * needs to restore for the game to continue without visible breakage.
 * Versioned because adding fields is expected as more shim state surfaces.
 * Captured/restored by shim_runtime_state_capture / shim_runtime_state_restore.
 *
 * Intentionally NOT included (because they're always zero at a clean
 * idle snapshot point — save_manager enforces this):
 *   - dispatch_depth, lcall_depth, isr_depth, critical_depth, interrupt_shadow
 *   - tail_dispatch_pending / tail_dispatch_addr / tail_dispatch_expected
 *   - isr_expected_sp[], lcall_expected_sp[], lcall_expected_ss[]
 *
 * Intentionally NOT included (because they're diagnostic-only or
 * recomputed at runtime):
 *   - trace_ring, lifecycle_ring, key_event_log, stack_write_ring
 *   - dd_inc/dec_* counters
 *   - last_host_time_ns, last_present_time_ns, host_time_origin_ns
 *   - screenshot_counter, headless_mode, shim_stdout_enabled
 *   - handles[], handle_paths[] (file handles cannot snapshot across process boundary)
 */
#define SHIM_RUNTIME_STATE_VERSION 6
typedef struct {
  uint32_t version;

  /* BIOS video state */
  BiosVideoState bios_video;
  CgaState cga;
  int32_t  current_display_width;
  int32_t  current_display_height;
  int32_t  virtual_display_buffer;

  /* VGA state — Mode 13h games program the DAC palette
   * directly; without restoring it the framebuffer indices render as
   * black after a re-exec. Added in v2; v1 snapshots wouldn't carry these
   * and would restore to a black screen on Mode 13h games. */
  VgaState vga;

  /* OPL2 audio chip state (music engine reads/writes these). Captured as
   * one Opl2State as of v3; v2 stored address/registers/status flat and
   * did not snapshot the busy/timer deadlines. */
  Opl2State opl2;

  /* PIT (timer) state */
  PITState pit;
  uint32_t pit_reload_value;
  uint16_t pit_latched_value;
  uint8_t  pit_latch_valid;
  uint16_t pit_read_buffer;
  uint8_t  pit_read_expect_high;
  uint8_t  pit_read_buffer_is_latch;
  uint32_t bios_timer_tick_backlog;
  uint8_t  bios_timer_tick_preincremented;
  uint64_t pit_cycle_accum;
  uint64_t pit_cycle_fraction_accum;

  /* DOS memory state */
  uint16_t next_free_seg;
  uint16_t program_min_block_paras;
  uint8_t  null_guard_initial[16];
  uint8_t  a20_enabled;

  /* Pending IRQs at capture time */
  uint8_t  irq0_pending;
  uint8_t  irq_pending[256];
  uint8_t  last_int_no;
} ShimRuntimeState;

/* Capture every C-global runtime state into *out. Always succeeds. */
void shim_runtime_state_capture(ShimRuntimeState *out);
/* Restore from *in. Returns 0 on success, -1 if version mismatch. */
int  shim_runtime_state_restore(const ShimRuntimeState *in);

/* Drain any pending cross-binary tail dispatch left over after a direct
 * call to a binary's dispatch function (e.g. snapshot restore's
 * re-entry, which bypasses dispatch_via_binary's trampoline loop). If
 * near_ret_tail set tail_dispatch_pending inside the direct fn() call
 * and there was no enclosing trampoline loop to consume it, the request
 * would otherwise be silently dropped — the game appears to "exit
 * cleanly" mid-execution. This drain runs the trampoline loop until the
 * pending flag is clear. */
void shim_drain_pending_tail_dispatch(const char *file, const char *func,
                                      int line);

/* Snapshot/restore the tail-dispatch state around a wrapper call. The
 * wrapper invokes its own dispatch loop, which may itself set a cross-
 * binary tail-dispatch and return; without snapshot/restore the inner
 * pending leaks into (or destroys) the outer caller's pending. Used by
 * the per-binary wrapper generated by the source's PCSwitchRenderer. */
typedef struct {
  bool pending;
  uint32_t addr;
  uint16_t expected;
} ShimTailDispatchState;
void shim_tail_dispatch_save(ShimTailDispatchState *out);
void shim_tail_dispatch_restore(const ShimTailDispatchState *in);

void shim_set_timer_isr(uint16_t segment, uint16_t offset);

/* Test seam for the otherwise-fatal unmapped-dispatch path. After
 * shim_arm_fatal_capture(), a would-be-fatal unmapped target reached via an
 * armed entry wrapper (e.g. call_table) records (shim_fatal_captured,
 * shim_fatal_addr, shim_fatal_kind) and unwinds back to the wrapper instead
 * of writing a crash bundle and exit(1)ing. Lets tests assert the hard-fail
 * behaviour in-process. Disarmed by default (real product behaviour). */
void shim_arm_fatal_capture(void);
void shim_disarm_fatal_capture(void);
extern volatile int shim_fatal_captured;
extern uint32_t shim_fatal_addr;
extern char shim_fatal_kind[32];

static inline uint32_t linear_addr(uint16_t seg, uint16_t off) {
  uint32_t addr = ((uint32_t)seg << 4) + off;
  return a20_enabled ? (addr & 0x1FFFFF) : (addr & 0xFFFFF);
}

#define seg_off(seg, off) (virtual_memory + linear_addr((seg), (off)))

/*
 * Raw byte access helper that bypasses any logging.  This is used by the RCB
 * read/write implementations to avoid recursing back into the logging layer.
 */
#define memb_raw(seg, off) (*(uint8_t *)seg_off((seg), (off)))

uint16_t memw_read_impl(uint16_t seg, uint16_t off, const char *file,
                        const char *func, int line);
void memw_write_impl(uint16_t seg, uint16_t off, uint16_t value,
                     const char *file, const char *func, int line);
uint8_t memb_read_impl(uint16_t seg, uint16_t off, const char *file,
                       const char *func, int line);
void memb_write_impl(uint16_t seg, uint16_t off, uint8_t value,
                     const char *file, const char *func, int line);
#define memw(seg, off)                                                         \
  memw_read_impl((seg), (off), __FILE__, __func__, __LINE__)
#define memw_write(seg, off, value)                                            \
  memw_write_impl((seg), (off), (value), __FILE__, __func__, __LINE__)
#define memb(seg, off)                                                         \
  ((uint8_t)memb_read_impl((seg), (off), __FILE__, __func__, __LINE__))
#define memb_write(seg, off, value)                                            \
  memb_write_impl((seg), (off), (value), __FILE__, __func__, __LINE__)

/* Block memory-move helpers used by translated ``rep movsb`` / ``rep movsw``.
 * On real hardware these are single-instruction block copies running at
 * memory bandwidth (the CRT can't catch them mid-transfer within a vsync
 * window). Translating them to byte-at-a-time C loops takes long enough
 * that screen blits show row-by-row tearing; these helpers do the copy via
 * memmove on the underlying virtual memory and only fall back to a slow
 * loop when either segment range overlaps the RCB window where per-byte
 * shim semantics matter. They consume cx (count), si (source offset), di
 * (destination offset), and DF (direction flag), and leave cx=0 with
 * si/di advanced (or decremented) by cx*delta. */
void rep_movsb_block_impl(uint16_t dst_seg, uint16_t src_seg,
                          const char *file, const char *func, int line);
void rep_movsw_block_impl(uint16_t dst_seg, uint16_t src_seg,
                          const char *file, const char *func, int line);
void rep_stosb_block_impl(uint16_t dst_seg,
                          const char *file, const char *func, int line);
#define rep_movsb_block(dst_seg, src_seg)                                      \
  rep_movsb_block_impl((dst_seg), (src_seg), __FILE__, __func__, __LINE__)
#define rep_movsw_block(dst_seg, src_seg)                                      \
  rep_movsw_block_impl((dst_seg), (src_seg), __FILE__, __func__, __LINE__)
#define rep_stosb_block(dst_seg)                                               \
  rep_stosb_block_impl((dst_seg), __FILE__, __func__, __LINE__)

static inline uint8_t parity8(uint8_t value) {
  value ^= value >> 4;
  value &= 0xF;
  /* x86 PF is set when the low byte has even parity. */
  return (uint8_t)(!((0x6996 >> value) & 1));
}

void shim_log(const char *func_name, const char *file, const char *func,
              int line, const char *path);
void shim_log_file_load(const char *path, const void *addr, size_t len,
                        size_t file_offset);
/* Replaces a 5-instruction word swap-loop (lodsw / mov dx,es:[di] / stosw /
 * mov [si-2],dx / loop) with a single call. Exchanges count words between
 * [ds_seg:si_off..] and [es_seg:di_off..] AND updates file_mappings so the
 * (linear -> file/offset) lookup follows the moved bytes. See definition
 * in shims.c for rationale. df=0 forward, df=1 reverse. */
void shim_swap_regions_w(uint16_t es_seg, uint16_t di_off,
                         uint16_t ds_seg, uint16_t si_off,
                         uint16_t count, int df);
void shim_set_stdout_logging_enabled(int enabled);
void shim_enable_stdout_logging(void);
void shim_disable_stdout_logging(void);
void shim_log_stdout(const char *fmt, ...);
void shim_log_stderr(const char *fmt, ...);
/* shim_log_crash: always writes (ignores the --verbose gate) and lands on stdout
 * so crash diagnostics share the same stream as the trace log.  Use this
 * for unrecoverable failure reports — every line is must-see. */
void shim_log_crash(const char *fmt, ...);

/* shim_save_bug_bundle: create a crashes/<...> bundle (crash.txt,
 * trace.tail.log, state.txt, screenshot.png) for translator-emitted
 * [BUG] aborts (e.g. dispatcher default case for an unhandled pc).
 * Also emits "Bundle: <path>" via shim_log_crash so the user sees where
 * to look on the on-screen banner. */
void shim_save_bug_bundle(const char *kind, uint32_t addr, const char *msg);
/* Make a blocking DOS console-input wait interruptible (DOS does STI; the
 * keyboard IRQ must fire nested to fill the type-ahead buffer). Bracket the
 * SAFEPOINT wait: begin saves+lifts critical_depth/IF, end restores them. */
void shim_dos_input_wait_begin(uint8_t *saved_crit, uint8_t *saved_if);
void shim_dos_input_wait_end(uint8_t saved_crit, uint8_t saved_if);
/* Synchronous INT 21h AH=4Bh EXEC: run a freshly-loaded child to completion on
 * a nested machine loop, returning its exit code; the parent resumes after its
 * INT. dos_exit calls shim_exec_child_terminate, which longjmps back into the
 * run when nested (returns 0 at top level so the real process exit proceeds). */
int shim_exec_run_child(uint16_t child_cs, uint16_t child_ip, uint16_t child_ss,
                        uint16_t child_sp, uint16_t child_psp);
int shim_exec_child_terminate(int status);
/* Run a real-mode FAR procedure in game code to completion (RETF), saving and
 * restoring the interrupted CPU state. Used to deliver INT 33h mouse events. */
void shim_invoke_far_call(uint16_t seg, uint16_t off, uint16_t r_ax,
                          uint16_t r_bx, uint16_t r_cx, uint16_t r_dx,
                          uint16_t r_si, uint16_t r_di);
void shim_save_video_memory(void);
void shim_dump_memory(uint32_t offset, size_t length);
void shim_dump_whole_memory(void);

/* Bookend recorder for symptom-only bugs (no crash anchor). Caller marks
 * a window of "the moment" so post-mortem can diff full-RAM snapshots
 * and grep the write log between the bookends. See shims.c for filter. */
void shim_bookend_start(void);
void shim_bookend_stop(void);
void shim_present_current_buffer(void);
void safe_point_impl(const char *file, const char *func, int line);
void safe_point(void);
#define SAFEPOINT() safe_point_impl(__FILE__, __func__, __LINE__)

void schedule_interrupt_impl(uint8_t int_no, const char *file,
                             const char *func, int line);
void schedule_interrupt(uint8_t int_no);
#ifndef SHIMS_DISABLE_MACROS
#define schedule_interrupt(int_no)                                             \
  schedule_interrupt_impl(int_no, __FILE__, __func__, __LINE__)
#endif

void run_interrupt_impl(uint8_t int_no, const char *file, const char *func,
                        int line);
void run_interrupt(uint8_t int_no);
#ifndef SHIMS_DISABLE_MACROS
#define run_interrupt(int_no)                                                  \
  run_interrupt_impl(int_no, __FILE__, __func__, __LINE__)
#endif


void file_mapping_swap_impl(uint16_t seg_a, uint16_t off_a, uint16_t seg_b,
                            uint16_t off_b, size_t len, const char *file,
                            const char *func, int line);
#ifndef SHIMS_DISABLE_MACROS
#define file_mapping_swap(seg_a, off_a, seg_b, off_b, len)                     \
  file_mapping_swap_impl(seg_a, off_a, seg_b, off_b, len, __FILE__, __func__,  \
                         __LINE__)
#endif

uint8_t rcb_read8_impl(RCBField field, const char *file, const char *func,
                       int line);
void rcb_write8_impl(RCBField field, uint8_t value, const char *file,
                     const char *func, int line);
uint16_t rcb_read16_impl(RCBField field, const char *file, const char *func,
                         int line);
void rcb_write16_impl(RCBField field, uint16_t value, const char *file,
                      const char *func, int line);
#ifndef SHIMS_DISABLE_MACROS
#define rcb_read8(field) rcb_read8_impl(field, __FILE__, __func__, __LINE__)
#define rcb_write8(field, value)                                               \
  rcb_write8_impl(field, value, __FILE__, __func__, __LINE__)
#define rcb_read16(field) rcb_read16_impl(field, __FILE__, __func__, __LINE__)
#define rcb_write16(field, value)                                              \
  rcb_write16_impl(field, value, __FILE__, __func__, __LINE__)
#endif

uint8_t inb(uint16_t port);
uint16_t inw(uint16_t port);
void outb(uint16_t port, uint8_t value);
void outw(uint16_t port, uint16_t value);
uint8_t compareMemoryUntilMismatch(const uint8_t *src, const uint8_t *dst,
                                   uint16_t count, int direction);
uint16_t scanMemoryForAl(const uint8_t *dst, uint8_t value, uint16_t count,
                         int direction, uint8_t *last_byte);

static inline void xor8(uint8_t *dest, uint8_t src) {
  uint8_t res = *dest ^ src;
  *dest = res;
  CF = 0;
  OF = 0;
  ZF = res == 0;
  SF = (res >> 7) & 1;
  PF = parity8(res);
}

static inline void xor16(uint16_t *dest, uint16_t src) {
  uint16_t res = *dest ^ src;
  *dest = res;
  CF = 0;
  OF = 0;
  ZF = res == 0;
  SF = (res >> 15) & 1;
  PF = parity8((uint8_t)res);
}

uint8_t dos_read_char_impl(const char *file, const char *func, int line);
uint8_t dos_read_char(void);
#ifndef SHIMS_DISABLE_MACROS
#define dos_read_char() dos_read_char_impl(__FILE__, __func__, __LINE__)
#endif
uint8_t dos_write_char_impl(uint8_t ch_val, const char *file, const char *func,
                            int line);
uint8_t dos_write_char(uint8_t ch_val);
#ifndef SHIMS_DISABLE_MACROS
#define dos_write_char(ch_val)                                                 \
  dos_write_char_impl(ch_val, __FILE__, __func__, __LINE__)
#endif
uint8_t dos_direct_console_io_impl(uint8_t dl_val, const char *file,
                                   const char *func, int line);
uint8_t dos_direct_console_io(uint8_t dl_val);
#ifndef SHIMS_DISABLE_MACROS
#define dos_direct_console_io(dl_val)                                          \
  dos_direct_console_io_impl(dl_val, __FILE__, __func__, __LINE__)
#endif
uint8_t dos_check_keyboard_status_impl(const char *file, const char *func,
                                       int line);
uint8_t dos_check_keyboard_status(void);
#ifndef SHIMS_DISABLE_MACROS
#define dos_check_keyboard_status()                                            \
  dos_check_keyboard_status_impl(__FILE__, __func__, __LINE__)
#endif
uint8_t dos_buffered_input_impl(const char *file, const char *func, int line);
uint8_t dos_buffered_input(void);
#ifndef SHIMS_DISABLE_MACROS
#define dos_buffered_input()                                                   \
  dos_buffered_input_impl(__FILE__, __func__, __LINE__)
#endif
uint8_t dos_parse_filename_impl(const char *file, const char *func, int line);
uint8_t dos_parse_filename(void);
#ifndef SHIMS_DISABLE_MACROS
#define dos_parse_filename()                                                   \
  dos_parse_filename_impl(__FILE__, __func__, __LINE__)
#endif
uint8_t dos_reset_disk_impl(const char *file, const char *func, int line);
uint8_t dos_reset_disk(void);
#ifndef SHIMS_DISABLE_MACROS
#define dos_reset_disk() dos_reset_disk_impl(__FILE__, __func__, __LINE__)
#endif
uint8_t dos_select_drive_impl(uint8_t dl_val, const char *file,
                              const char *func, int line);
uint8_t dos_select_drive(uint8_t dl_val);
#ifndef SHIMS_DISABLE_MACROS
#define dos_select_drive(dl_val)                                               \
  dos_select_drive_impl(dl_val, __FILE__, __func__, __LINE__)
#endif
uint8_t dos_print_string_impl(const char *str, const char *file,
                              const char *func, int line);
uint8_t dos_print_string(const char *str);
#ifndef SHIMS_DISABLE_MACROS
#define dos_print_string(str)                                                  \
  dos_print_string_impl(str, __FILE__, __func__, __LINE__)
#endif
uint8_t dos_get_current_drive_impl(const char *file, const char *func,
                                   int line);
uint8_t dos_get_current_drive(void);
#ifndef SHIMS_DISABLE_MACROS
#define dos_get_current_drive()                                                \
  dos_get_current_drive_impl(__FILE__, __func__, __LINE__)
#endif
uint8_t dos_set_dta_impl(void *dta, const char *file, const char *func,
                         int line);
uint8_t dos_set_dta(void *dta);
#ifndef SHIMS_DISABLE_MACROS
#define dos_set_dta(dta) dos_set_dta_impl(dta, __FILE__, __func__, __LINE__)
#endif
uint8_t dos_set_interrupt_vector_impl(uint8_t int_no, uint16_t segment,
                                      uint16_t offset, const char *file,
                                      const char *func, int line);
uint8_t dos_set_interrupt_vector(uint8_t int_no, uint16_t segment,
                                 uint16_t offset);
#ifndef SHIMS_DISABLE_MACROS
#define dos_set_interrupt_vector(int_no, segment, offset)                      \
  dos_set_interrupt_vector_impl(int_no, segment, offset, __FILE__, __func__,   \
                                __LINE__)
#endif
uint8_t dos_get_disk_free_space_impl(uint8_t dl_val, const char *file,
                                     const char *func, int line);
uint8_t dos_get_disk_free_space(uint8_t dl_val);
#ifndef SHIMS_DISABLE_MACROS
#define dos_get_disk_free_space(dl_val)                                        \
  dos_get_disk_free_space_impl(dl_val, __FILE__, __func__, __LINE__)
#endif
uint8_t dos_get_version_impl(const char *file, const char *func, int line);
uint8_t dos_get_version(void);
#ifndef SHIMS_DISABLE_MACROS
#define dos_get_version() dos_get_version_impl(__FILE__, __func__, __LINE__)
#endif
uint8_t dos_get_time_impl(const char *file, const char *func, int line);
uint8_t dos_get_time(void);
uint8_t dos_get_date_impl(const char *file, const char *func, int line);
uint8_t dos_get_date(void);
uint8_t dos_console_input_no_echo_impl(const char *file, const char *func,
                                       int line);
uint8_t dos_console_input_no_echo(void);
#ifndef SHIMS_DISABLE_MACROS
#define dos_get_time() dos_get_time_impl(__FILE__, __func__, __LINE__)
#define dos_get_date() dos_get_date_impl(__FILE__, __func__, __LINE__)
#define dos_console_input_no_echo() \
  dos_console_input_no_echo_impl(__FILE__, __func__, __LINE__)
#endif
uint8_t dos_get_interrupt_vector_impl(const char *file, const char *func,
                                      int line);
uint8_t dos_get_interrupt_vector(void);
#ifndef SHIMS_DISABLE_MACROS
#define dos_get_interrupt_vector()                                             \
  dos_get_interrupt_vector_impl(__FILE__, __func__, __LINE__)
#endif
uint32_t interrupt_vector_addr_impl(uint8_t int_no, const char *file,
                                    const char *func, int line);
uint32_t interrupt_vector_addr(uint8_t int_no);
#ifndef SHIMS_DISABLE_MACROS
#define interrupt_vector_addr(int_no)                                          \
  interrupt_vector_addr_impl(int_no, __FILE__, __func__, __LINE__)
#endif
uint8_t dos_make_dir_impl(const char *path, const char *file, const char *func,
                          int line);
uint8_t dos_make_dir(const char *path);
#ifndef SHIMS_DISABLE_MACROS
#define dos_make_dir(path) dos_make_dir_impl(path, __FILE__, __func__, __LINE__)
#endif
uint8_t dos_change_dir_impl(const char *path, const char *file,
                            const char *func, int line);
uint8_t dos_change_dir(const char *path);
#ifndef SHIMS_DISABLE_MACROS
#define dos_change_dir(path)                                                   \
  dos_change_dir_impl(path, __FILE__, __func__, __LINE__)
#endif
uint8_t dos_open_file_impl(const char *path, const char *file, const char *func,
                           int line);
uint8_t dos_open_file(const char *path);
#ifndef SHIMS_DISABLE_MACROS
#define dos_open_file(path)                                                    \
  dos_open_file_impl(path, __FILE__, __func__, __LINE__)
#endif
uint8_t dos_close_file_impl(uint16_t handle, const char *file, const char *func,
                            int line);
uint8_t dos_close_file(uint16_t handle);
#ifndef SHIMS_DISABLE_MACROS
#define dos_close_file(handle)                                                 \
  dos_close_file_impl(handle, __FILE__, __func__, __LINE__)
#endif
uint8_t dos_read_file_impl(uint16_t handle, void *buf, uint16_t len,
                           const char *file, const char *func, int line);
uint8_t dos_read_file(uint16_t handle, void *buf, uint16_t len);
#ifndef SHIMS_DISABLE_MACROS
#define dos_read_file(handle, buf, len)                                        \
  dos_read_file_impl(handle, buf, len, __FILE__, __func__, __LINE__)
#endif
uint8_t dos_write_file_impl(uint16_t handle, const void *buf, uint16_t len,
                            const char *file, const char *func, int line);
uint8_t dos_write_file(uint16_t handle, const void *buf, uint16_t len);
#ifndef SHIMS_DISABLE_MACROS
#define dos_write_file(handle, buf, len)                                       \
  dos_write_file_impl(handle, buf, len, __FILE__, __func__, __LINE__)
#endif
uint8_t dos_get_file_attributes_impl(const char *path, uint16_t *attr,
                                     const char *file, const char *func,
                                     int line);
uint8_t dos_get_file_attributes(const char *path, uint16_t *attr);
#ifndef SHIMS_DISABLE_MACROS
#define dos_get_file_attributes(path, attr)                                    \
  dos_get_file_attributes_impl(path, attr, __FILE__, __func__, __LINE__)
#endif
uint8_t dos_delete_file_impl(const char *path, const char *file,
                             const char *func, int line);
uint8_t dos_delete_file(const char *path);
#ifndef SHIMS_DISABLE_MACROS
#define dos_delete_file(path)                                                  \
  dos_delete_file_impl(path, __FILE__, __func__, __LINE__)
#endif
uint8_t dos_rename_impl(const char *old_path, const char *new_path,
                        const char *file, const char *func, int line);
uint8_t dos_rename(const char *old_path, const char *new_path);
#ifndef SHIMS_DISABLE_MACROS
#define dos_rename(old_path, new_path)                                         \
  dos_rename_impl(old_path, new_path, __FILE__, __func__, __LINE__)
#endif
uint8_t dos_ioctl_impl(uint8_t subfunc, uint16_t bx_handle, const char *file,
                       const char *func, int line);
uint8_t dos_ioctl(uint8_t subfunc, uint16_t bx_handle);
#ifndef SHIMS_DISABLE_MACROS
#define dos_ioctl(subfunc, bx_handle)                                          \
  dos_ioctl_impl(subfunc, bx_handle, __FILE__, __func__, __LINE__)
#endif
uint8_t dos_get_current_dir_impl(char *buf, const char *file, const char *func,
                                 int line);
uint8_t dos_get_current_dir(char *buf);
#ifndef SHIMS_DISABLE_MACROS
#define dos_get_current_dir(buf)                                               \
  dos_get_current_dir_impl(buf, __FILE__, __func__, __LINE__)
#endif
uint8_t dos_resize_mem_impl(uint16_t segment, uint16_t parags,
                            const char *file, const char *func, int line);
uint8_t dos_create_file_impl(const char *path, uint16_t attr, const char *file,
                             const char *func, int line);
uint8_t dos_create_file(const char *path, uint16_t attr);
#ifndef SHIMS_DISABLE_MACROS
#define dos_create_file(path, attr)                                            \
  dos_create_file_impl(path, attr, __FILE__, __func__, __LINE__)
#endif
uint8_t dos_get_dta_impl(const char *file, const char *func, int line);
uint8_t dos_get_dta(void);
#ifndef SHIMS_DISABLE_MACROS
#define dos_get_dta() dos_get_dta_impl(__FILE__, __func__, __LINE__)
#endif
uint8_t dos_find_next_impl(const char *file, const char *func, int line);
uint8_t dos_find_next(void);
#ifndef SHIMS_DISABLE_MACROS
#define dos_find_next() dos_find_next_impl(__FILE__, __func__, __LINE__)
#endif
uint8_t dos_flush_and_read_impl(uint8_t subfunc, const char *file,
                                const char *func, int line);
uint8_t dos_flush_and_read(uint8_t subfunc);
#ifndef SHIMS_DISABLE_MACROS
#define dos_flush_and_read(subfunc)                                            \
  dos_flush_and_read_impl(subfunc, __FILE__, __func__, __LINE__)
#endif
uint8_t dos_resize_mem(uint16_t segment, uint16_t parags);
#ifndef SHIMS_DISABLE_MACROS
#define dos_resize_mem(segment, parags)                                        \
  dos_resize_mem_impl(segment, parags, __FILE__, __func__, __LINE__)
#endif
uint8_t dos_lseek_impl(uint16_t handle, uint16_t off_hi, uint16_t off_lo,
                       uint8_t origin, const char *file, const char *func,
                       int line);
uint8_t dos_lseek(uint16_t handle, uint16_t off_hi, uint16_t off_lo,
                  uint8_t origin);
#ifndef SHIMS_DISABLE_MACROS
#define dos_lseek(handle, off_hi, off_lo, origin)                              \
  dos_lseek_impl(handle, off_hi, off_lo, origin, __FILE__, __func__, __LINE__)
#endif
uint8_t dos_find_first_impl(const char *path, uint16_t attr, const char *file,
                            const char *func, int line);
uint8_t dos_find_first(const char *path, uint16_t attr);
#ifndef SHIMS_DISABLE_MACROS
#define dos_find_first(path, attr)                                             \
  dos_find_first_impl(path, attr, __FILE__, __func__, __LINE__)
#endif
uint8_t dos_alloc_mem_impl(uint16_t parags, const char *file, const char *func,
                           int line);
uint8_t dos_alloc_mem(uint16_t parags);
#ifndef SHIMS_DISABLE_MACROS
#define dos_alloc_mem(parags)                                                  \
  dos_alloc_mem_impl(parags, __FILE__, __func__, __LINE__)
#endif
uint8_t dos_free_mem_impl(void *ptr, const char *file, const char *func,
                          int line);
uint8_t dos_free_mem(void *ptr);
#ifndef SHIMS_DISABLE_MACROS
#define dos_free_mem(ptr) dos_free_mem_impl(ptr, __FILE__, __func__, __LINE__)
#endif
uint8_t dos_exec_impl(void *param_block, const char *cmd, const char *file,
                      const char *func, int line);
uint8_t dos_exec(void *param_block, const char *cmd);
#ifndef SHIMS_DISABLE_MACROS
#define dos_exec(param_block, cmd)                                             \
  dos_exec_impl(param_block, cmd, __FILE__, __func__, __LINE__)
#endif
void dos_exit_impl(const char *file, const char *func, int line)
    __attribute__((noreturn));
void dos_exit(void) __attribute__((noreturn));
#ifndef SHIMS_DISABLE_MACROS
#define dos_exit() dos_exit_impl(__FILE__, __func__, __LINE__)
#endif

/* INT 21h AH=4Dh: Get Return Code of Subprocess (AX = term type << 8 | code). */
uint8_t dos_get_return_code_impl(const char *file, const char *func, int line);
#ifndef SHIMS_DISABLE_MACROS
#define dos_get_return_code()                                                  \
  dos_get_return_code_impl(__FILE__, __func__, __LINE__)
#endif
/* DOS API AH=0x10: Close file using FCB */
uint8_t dos_close_fcb_impl(uint16_t dx_val, const char *file, const char *func,
                           int line);
uint8_t dos_close_fcb(uint16_t dx_val);
#ifndef SHIMS_DISABLE_MACROS
#define dos_close_fcb(dx_val)                                                  \
  dos_close_fcb_impl(dx_val, __FILE__, __func__, __LINE__)
#endif
uint8_t dos_api_impl(const char *file, const char *func, int line);
uint8_t dos_api(void);
#ifndef SHIMS_DISABLE_MACROS
#define dos_api() dos_api_impl(__FILE__, __func__, __LINE__)
#endif
void bios_set_video_mode_impl(uint8_t mode, const char *file, const char *func,
                              int line);
void bios_set_video_mode(uint8_t mode);
#ifndef SHIMS_DISABLE_MACROS
#define bios_set_video_mode(mode)                                              \
  bios_set_video_mode_impl(mode, __FILE__, __func__, __LINE__)
#endif
void bios_set_cursor_position_impl(uint8_t page, uint8_t row, uint8_t col,
                                   const char *file, const char *func,
                                   int line);
void bios_set_cursor_position(uint8_t page, uint8_t row, uint8_t col);
#ifndef SHIMS_DISABLE_MACROS
#define bios_set_cursor_position(page, row, col)                                \
  bios_set_cursor_position_impl(page, row, col, __FILE__, __func__, __LINE__)
#endif
void bios_set_cga_palette_impl(uint8_t bh_val, uint8_t bl_val, const char *file,
                               const char *func, int line);
void bios_set_cga_palette(uint8_t bh_val, uint8_t bl_val);
#ifndef SHIMS_DISABLE_MACROS
#define bios_set_cga_palette(bh_val, bl_val)                                    \
  bios_set_cga_palette_impl(bh_val, bl_val, __FILE__, __func__, __LINE__)
#endif
void bios_set_palette_impl(const char *file, const char *func, int line);
void bios_video_alt_select_impl(const char *file, const char *func, int line);
void bios_set_palette(void);
#ifndef SHIMS_DISABLE_MACROS
#define bios_set_palette() bios_set_palette_impl(__FILE__, __func__, __LINE__)
#endif
void bios_video_alt_select(void);
#ifndef SHIMS_DISABLE_MACROS
#define bios_video_alt_select()                                                \
  bios_video_alt_select_impl(__FILE__, __func__, __LINE__)
#endif
void bios_teletype_output_impl(uint8_t ch_val, uint8_t page, uint8_t attr,
                               const char *file, const char *func, int line);
void bios_teletype_output(uint8_t ch_val, uint8_t page, uint8_t attr);
#ifndef SHIMS_DISABLE_MACROS
#define bios_teletype_output(ch_value, page, attr)                               \
  bios_teletype_output_impl(ch_value, page, attr, __FILE__, __func__, __LINE__)
#endif
void bios_keyboard_impl(const char *file, const char *func, int line);
void bios_keyboard(void);
#ifndef SHIMS_DISABLE_MACROS
#define bios_keyboard() bios_keyboard_impl(__FILE__, __func__, __LINE__)
#endif
uint8_t bios_current_video_mode(void);
uint16_t bios_read_char_attr(void);
void bios_write_char_attr(uint8_t glyph, uint8_t page, uint8_t attr, uint16_t count);
void bios_write_char_only(uint8_t glyph, uint8_t page, uint16_t count);
void bios_scroll_window(uint8_t lines, uint8_t attr, uint8_t top, uint8_t left,
                        uint8_t bottom, uint8_t right, int down);
void bios_get_cursor(uint8_t page);
uint8_t bios_current_video_columns(void);
uint8_t bios_current_active_page(void);
uint8_t bios_display_combination_code(void);
uint8_t bios_display_combination_alt_code(void);
void bios_get_video_parameter_block(uint8_t mode, uint16_t *seg, uint16_t *off);

// Shim for ljmp seg:off
void long_jump_impl(uint16_t seg, uint16_t off, const char *file,
                    const char *func, int line);
void long_jump(uint16_t seg, uint16_t off);
#ifndef SHIMS_DISABLE_MACROS
#define long_jump(seg, off)                                                    \
  long_jump_impl(seg, off, __FILE__, __func__, __LINE__)
#endif
void retf_impl(const char *file, const char *func, int line);
void retf(void);
void retf_pop_impl(const char *file, const char *func, int line,
                   uint16_t pop_bytes);
#ifndef SHIMS_DISABLE_MACROS
#define retf() retf_impl(__FILE__, __func__, __LINE__)
#define retf_pop(pop_bytes)                                                    \
  retf_pop_impl(__FILE__, __func__, __LINE__, pop_bytes)
#endif
void iret_impl(const char *file, const char *func, int line);
void iret(void);
#ifndef SHIMS_DISABLE_MACROS
#define iret() iret_impl(__FILE__, __func__, __LINE__)
#endif
// Shim for lcall seg:off
void lcall_table_impl(uint16_t ret_ip, uint16_t seg, uint16_t off,
                      const char *file, const char *func, int line);
void lcall_table(uint16_t ret_ip, uint16_t seg, uint16_t off);
#ifndef SHIMS_DISABLE_MACROS
#define lcall_table(ret_ip, seg, off)                                          \
  lcall_table_impl(ret_ip, seg, off, __FILE__, __func__, __LINE__)
#endif
// Shim for call [addr]
void call_table_impl(uint16_t ret_ip, uint32_t addr, const char *file,
                     const char *func, int line);
void call_table(uint16_t ret_ip, uint32_t addr);
#ifndef SHIMS_DISABLE_MACROS
#define call_table(ret_ip, addr)                                               \
  call_table_impl(ret_ip, addr, __FILE__, __func__, __LINE__)
#endif
// Shim for jmp [addr]
//
// A near jmp is a tail-dispatch: the current expected_retip is propagated
// to the target so any ret in the dispatched chain that matches it returns
// to the original caller (not silently consumed by a stack-inferred match).
void jump_table_impl(uint32_t addr, uint16_t expected_retip, const char *file,
                     const char *func, int line);
void jump_table(uint32_t addr, uint16_t expected_retip);
#ifndef SHIMS_DISABLE_MACROS
#define jump_table(addr, expected_retip)                                       \
  jump_table_impl(addr, expected_retip, __FILE__, __func__, __LINE__)
#endif
// Semantic near `ret` tail dispatch.
//
// Emitted by the IR-to-C translator at every near `ret`: if the popped IP
// matches the caller-pushed retIP (``expected_retip`` parameter of the
// translated function) the function returns to C normally; otherwise this
// shim is invoked to tail-dispatch to the popped IP.  ``expected_retip`` is
// propagated to the dispatched routine so that the eventual ret in the
// chain that matches it unwinds back to the original caller cleanly,
// rather than being short-circuited by a stack-inferred match.
//
// The popped IP must have already been removed from the stack by the
// caller before invoking this shim.
void near_ret_tail_impl(uint16_t popped_ip, uint16_t expected_retip,
                        const char *file, const char *func, int line);
void near_ret_tail(uint16_t popped_ip, uint16_t expected_retip);
#ifndef SHIMS_DISABLE_MACROS
#define near_ret_tail(popped_ip, expected_retip)                               \
  near_ret_tail_impl(popped_ip, expected_retip, __FILE__, __func__, __LINE__)
#endif

typedef struct {
  unsigned char raw[256];
} PSP;

typedef struct {
  uint16_t saved_sp;
  uint16_t saved_ss;
} ExecParamBlock;

typedef struct {
  uint16_t field_ff00;
  uint16_t field_ff02;
  uint16_t field_ff04;
  uint16_t field_ff06;
  uint8_t field_ff08;
  uint8_t field_ff09;
  uint8_t field_ff0A;
  uint8_t field_ff0B;
  uint16_t field_ff0C;
  uint16_t field_ff0E;
  uint16_t field_ff10;
  uint16_t field_ff12;
  uint8_t field_ff14;
  uint8_t field_ff15;
  uint8_t field_ff16;
  uint8_t field_ff17;
  uint16_t field_ff18;
  uint8_t field_ff1D;
  uint8_t field_ff1E;
  uint16_t field_ff1F;
  uint8_t field_ff26;
  uint8_t field_ff27;
  uint8_t field_ff28;
  uint16_t field_ff2C;
  uint8_t field_ff33;
  uint8_t field_ff34;
  uint8_t field_ff38;
  uint8_t field_ff39;
  uint8_t field_ff3A;
  uint8_t field_ff3B;
  uint8_t field_ff3C;
  uint8_t field_ff40;
  uint8_t field_ff42;
  uint8_t field_ff43;
  uint8_t field_ff74;
  uint8_t field_ff75;
  uint8_t field_ff78;
  uint16_t field_ff79;
  uint16_t field_ff7B;
} ResidentControlBlock;

extern ExecParamBlock exec_params;

/* ===== Internal API for sibling modules (e.g. scripts/snapshot.c) =====
 *
 * These expose just enough of the shims runtime so the snapshot/keylog/restore
 * layer can capture and restore state without poking at static globals. Not
 * intended for use by translated game code. */

#define SHIM_KBD_BUFFER_SIZE 64

typedef struct {
  uint8_t  q_ascii[SHIM_KBD_BUFFER_SIZE];
  uint8_t  q_scan[SHIM_KBD_BUFFER_SIZE];
  int      head, tail, count;
  uint8_t  cur_ascii, cur_scan, last_scan, ready;
} ShimKbdState;
void shim_kbd_state_capture(ShimKbdState *out);
void shim_kbd_state_restore(const ShimKbdState *in);

typedef struct {
  uint32_t base;
  size_t   len;
  size_t   file_offset;
  uint16_t canonical_cs;
  const char *path;   /* not owned by caller; valid for process lifetime */
} ShimFileMappingView;
size_t shim_file_mappings_count(void);
void   shim_file_mappings_get(size_t i, ShimFileMappingView *out);
/* Restore path: clear all mappings, then add them back from snapshot. */
void   shim_file_mappings_reset(void);
int    shim_file_mappings_add_for_restore(const char *path, uint32_t base,
                                          size_t len, size_t file_offset,
                                          uint16_t canonical_cs);

/* Dispatch into the binary that owns the given linear address. Used by the
 * snapshot module to re-enter at a saved cs:ip after --restore-from. */
int shim_dispatch_via_binary(uint32_t addr, uint16_t expected_retip,
                             const char *file, const char *func, int line);

/* Faithful flat top-level loop: resolve cpu.r_cs:cpu.r_ip to its owning chunk,
 * run it until control leaves its pc-space, then re-resolve -- forever, until
 * the program terminates (dos_exit -> exit()). main() sets the entry cs:ip and
 * calls this. */
void run_machine(void);
/* Set by dos_exit_impl before exit(); backstop for run_machine / the nested
 * ISR loop to terminate deterministically. */
extern volatile int machine_halted;

/* Bundle writer helpers (used by both crash bundles and the snapshot atexit
 * dump). Path is "<dir>/<name>"; len bytes of contents are written. */
void shim_crash_bundle_write_file(const char *dir, const char *name,
                                  const char *contents, size_t len);
void shim_crash_bundle_write_state(const char *dir);
void shim_crash_bundle_write_trace_tail(const char *dir);
void shim_lifecycle_dump_to_dir(const char *dir);

/* Snapshot module installs a "write extra files into this bundle dir" hook
 * here. Called from save_crash_bundle() after the standard files are
 * written. Optional — NULL means no extra files. */
void shim_set_bundle_extra_writer(void (*fn)(const char *dir));

#endif /* SHIMS_H */
