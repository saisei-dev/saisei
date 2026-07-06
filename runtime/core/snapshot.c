/* scripts/snapshot.c
 *
 * SDL-tier save-state layer. Sits on top of the shims runtime and owns:
 *   - keys.log (timestamped press/release events with hold duration)
 *   - pre_last_key.{bin,state.bin,json,maps.tsv} snapshots
 *   - atexit dump into last_exit_snapshot/
 *   - --restore-from <dir>: load snapshot and re-enter dispatch
 *
 * The shims runtime invokes our on_* hooks at the right boundaries; we never
 * poke at shims static globals — capture/restore goes through the
 * shim_kbd_state_* and shim_file_mappings_* accessors declared in shims.h. */

#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <time.h>
#include <unistd.h>

#define SHIMS_DISABLE_MACROS
#include "shims.h"
#undef SHIMS_DISABLE_MACROS
#include "snapshot.h"
#include "save_manager.h"

/* ===== Elapsed-microseconds clock ===== */

static struct timespec proc_start_ts;
static int proc_start_ts_set;

static uint64_t snapshot_elapsed_us(void) {
  struct timespec now;
  clock_gettime(CLOCK_MONOTONIC, &now);
  if (!proc_start_ts_set) {
    proc_start_ts = now;
    proc_start_ts_set = 1;
    return 0;
  }
  uint64_t s = (uint64_t)(now.tv_sec - proc_start_ts.tv_sec);
  int64_t ns = (int64_t)now.tv_nsec - (int64_t)proc_start_ts.tv_nsec;
  return s * 1000000ULL + (uint64_t)(ns / 1000);
}

/* ===== Key event log ===== */

#define KEY_EVENT_LOG_CAP 4096
typedef struct {
  uint64_t elapsed_us;
  uint32_t held_us;
  char     action;     /* 'P' or 'R' */
  uint8_t  scancode;
} KeyEvent;
static KeyEvent key_event_log[KEY_EVENT_LOG_CAP];
static size_t key_event_log_n;
static uint64_t kbd_press_ts_us[128];

void snapshot_on_key_event(char action, uint8_t scancode) {
  uint64_t t = snapshot_elapsed_us();
  uint32_t held = 0;
  uint8_t sc7 = (uint8_t)(scancode & 0x7F);
  if (action == 'P') {
    kbd_press_ts_us[sc7] = t;
  } else if (action == 'R' && kbd_press_ts_us[sc7]) {
    held = (uint32_t)(t - kbd_press_ts_us[sc7]);
    kbd_press_ts_us[sc7] = 0;
  }
  if (key_event_log_n >= KEY_EVENT_LOG_CAP) {
    /* Drop oldest half, keep tail. */
    memmove(key_event_log, key_event_log + KEY_EVENT_LOG_CAP / 2,
            (KEY_EVENT_LOG_CAP / 2) * sizeof(KeyEvent));
    key_event_log_n = KEY_EVENT_LOG_CAP / 2;
  }
  key_event_log[key_event_log_n].elapsed_us = t;
  key_event_log[key_event_log_n].held_us = held;
  key_event_log[key_event_log_n].action = action;
  key_event_log[key_event_log_n].scancode = sc7;
  ++key_event_log_n;
}

static void write_keys_log(const char *dir) {
  if (key_event_log_n == 0) return;
  char path[320];
  snprintf(path, sizeof(path), "%s/keys.log", dir);
  int fd = open(path, O_WRONLY | O_CREAT | O_TRUNC | O_CLOEXEC, 0644);
  if (fd < 0) return;
  static const char header[] =
      "# columns: elapsed_us action scancode held_us\n"
      "# action: P=press R=release; scancode is 7-bit DOS make code\n";
  ssize_t _hw = write(fd, header, sizeof(header) - 1); (void)_hw;
  char line[64];
  for (size_t i = 0; i < key_event_log_n; ++i) {
    int n = snprintf(line, sizeof(line),
                     "%llu %c 0x%02X %u\n",
                     (unsigned long long)key_event_log[i].elapsed_us,
                     key_event_log[i].action ? key_event_log[i].action : '?',
                     (unsigned)key_event_log[i].scancode,
                     (unsigned)key_event_log[i].held_us);
    if (n > 0) { ssize_t _w = write(fd, line, (size_t)n); (void)_w; }
  }
  close(fd);
}

/* ===== Pre-key snapshot ===== */

typedef struct {
  uint16_t r_cs, r_ip, r_ss, r_sp, r_ds, r_es;
  uint16_t r_ax, r_bx, r_cx, r_dx, r_si, r_di, r_bp;
  uint8_t  f_CF, f_PF, f_ZF, f_SF, f_OF, f_IF, f_DF;
  uint16_t lcall_d, isr_d;
  uint8_t  last_scancode;
  uint64_t elapsed_us;
  ShimKbdState kbd;
  /* Top of the active-binary stack at capture time. Restore uses this
   * to route into the right binary's dispatch instead of resolving cs:ip
   * arithmetically (which doesn't round-trip for canonical_cs-swap
   * binaries or for inline _impl calls). Fixed-size string so the layout
   * is stable on disk. */
  char     active_binary[16];
} SnapCPU;

static uint8_t *snap_memory;
static SnapCPU  snap_cpu;
static int      snap_present;

/* Capture current runtime state into snap_memory/snap_cpu. Shared by the
 * per-key crash-investigation path and the gameplay-save path. */
static int capture_into_snap(void) {
  if (!snap_memory) {
    snap_memory = (uint8_t *)malloc(SHIM_MEMORY_SIZE);
    if (!snap_memory) return -1;
  }
  memcpy(snap_memory, virtual_memory, SHIM_MEMORY_SIZE);
  snap_cpu.r_cs = cs; snap_cpu.r_ip = ip; snap_cpu.r_ss = ss; snap_cpu.r_sp = sp;
  snap_cpu.r_ds = ds; snap_cpu.r_es = es;
  snap_cpu.r_ax = ax; snap_cpu.r_bx = bx; snap_cpu.r_cx = cx; snap_cpu.r_dx = dx;
  snap_cpu.r_si = si; snap_cpu.r_di = di; snap_cpu.r_bp = bp;
  snap_cpu.f_CF = CF; snap_cpu.f_PF = PF; snap_cpu.f_ZF = ZF; snap_cpu.f_SF = SF;
  snap_cpu.f_OF = OF; snap_cpu.f_IF = IF; snap_cpu.f_DF = DF;
  snap_cpu.lcall_d = lcall_depth;
  snap_cpu.isr_d = isr_depth;
  shim_kbd_state_capture(&snap_cpu.kbd);
  snap_cpu.last_scancode = snap_cpu.kbd.last_scan;
  snap_cpu.elapsed_us = snapshot_elapsed_us();
  const char *binary = shim_active_binary();
  memset(snap_cpu.active_binary, 0, sizeof(snap_cpu.active_binary));
  if (binary) {
    strncpy(snap_cpu.active_binary, binary, sizeof(snap_cpu.active_binary) - 1);
  }
  snap_present = 1;
  return 0;
}

void snapshot_on_key_consumed(void) {
  /* Capture into the in-memory snap so crash bundles get a pre_last_key.*
   * snapshot for forensics. */
  (void)capture_into_snap();
  /* Arm the gameplay-save anchor: if the user pressed Cmd+F1 since the
   * last save, this kbd-consumption boundary is when we want the save
   * to actually land — the game just returned from a kbd wait and is
   * about to re-enter its main loop at a stable resting state.
   * save_manager itself still requires depth=0 and a unique case-key ip
   * at the actual SAFEPOINT before writing. */
  save_manager_on_key_consumed();
}

static void write_pre_key_snapshot(const char *dir) {
  if (!snap_present || !snap_memory) return;
  char path[320];

  /* 1. raw memory */
  snprintf(path, sizeof(path), "%s/pre_last_key.bin", dir);
  int fd = open(path, O_WRONLY | O_CREAT | O_TRUNC | O_CLOEXEC, 0644);
  if (fd >= 0) {
    size_t off = 0;
    while (off < SHIM_MEMORY_SIZE) {
      ssize_t w = write(fd, snap_memory + off, SHIM_MEMORY_SIZE - off);
      if (w < 0) { if (errno == EINTR) continue; break; }
      off += (size_t)w;
    }
    close(fd);
  }

  /* 2. JSON (human-readable) */
  char buf[8192];
  int n = snprintf(buf, sizeof(buf),
                   "{\n"
                   "  \"elapsed_us\": %llu,\n"
                   "  \"last_scancode\": \"0x%02X\",\n"
                   "  \"cpu\": {\n"
                   "    \"cs\": \"0x%04X\", \"ip\": \"0x%04X\",\n"
                   "    \"ss\": \"0x%04X\", \"sp\": \"0x%04X\",\n"
                   "    \"ds\": \"0x%04X\", \"es\": \"0x%04X\",\n"
                   "    \"ax\": \"0x%04X\", \"bx\": \"0x%04X\",\n"
                   "    \"cx\": \"0x%04X\", \"dx\": \"0x%04X\",\n"
                   "    \"si\": \"0x%04X\", \"di\": \"0x%04X\",\n"
                   "    \"bp\": \"0x%04X\"\n"
                   "  },\n"
                   "  \"flags\": {\"CF\":%u,\"PF\":%u,\"ZF\":%u,\"SF\":%u,"
                   "\"OF\":%u,\"IF\":%u,\"DF\":%u},\n"
                   "  \"lcall_depth\": %u,\n"
                   "  \"isr_depth\": %u,\n"
                   "  \"memory_size\": %zu,\n"
                   "  \"keyboard\": {\n"
                   "    \"head\": %d, \"tail\": %d, \"count\": %d,\n"
                   "    \"ascii\": \"0x%02X\", \"scancode\": \"0x%02X\",\n"
                   "    \"last_scancode\": \"0x%02X\", \"ready\": %u\n"
                   "  }\n}\n",
                   (unsigned long long)snap_cpu.elapsed_us,
                   (unsigned)snap_cpu.last_scancode,
                   snap_cpu.r_cs, snap_cpu.r_ip, snap_cpu.r_ss, snap_cpu.r_sp,
                   snap_cpu.r_ds, snap_cpu.r_es,
                   snap_cpu.r_ax, snap_cpu.r_bx, snap_cpu.r_cx, snap_cpu.r_dx,
                   snap_cpu.r_si, snap_cpu.r_di, snap_cpu.r_bp,
                   (unsigned)snap_cpu.f_CF, (unsigned)snap_cpu.f_PF,
                   (unsigned)snap_cpu.f_ZF, (unsigned)snap_cpu.f_SF,
                   (unsigned)snap_cpu.f_OF, (unsigned)snap_cpu.f_IF,
                   (unsigned)snap_cpu.f_DF,
                   (unsigned)snap_cpu.lcall_d,
                   (unsigned)snap_cpu.isr_d,
                   SHIM_MEMORY_SIZE,
                   snap_cpu.kbd.head, snap_cpu.kbd.tail, snap_cpu.kbd.count,
                   (unsigned)snap_cpu.kbd.cur_ascii,
                   (unsigned)snap_cpu.kbd.cur_scan,
                   (unsigned)snap_cpu.kbd.last_scan,
                   (unsigned)snap_cpu.kbd.ready);
  if (n > 0 && n < (int)sizeof(buf)) {
    shim_crash_bundle_write_file(dir, "pre_last_key.json", buf, (size_t)n);
  }

  /* 3. Binary mirror of snap_cpu for restore (JSON parsing in C is fragile;
   * the bundle is consumed by the same binary that wrote it). */
  shim_crash_bundle_write_file(dir, "pre_last_key.state.bin",
                               (const char *)&snap_cpu, sizeof(snap_cpu));

  /* 3b. Shim runtime state — video mode, OPL2 regs, PIT, DOS heap pointer,
   * pending IRQs. Restore depends on this to put the simulator back in
   * the same C-global state the game expected. */
  ShimRuntimeState rt;
  shim_runtime_state_capture(&rt);
  shim_crash_bundle_write_file(dir, "pre_last_key.shim_state.bin",
                               (const char *)&rt, sizeof(rt));

  /* 4. file_mappings TSV */
  snprintf(path, sizeof(path), "%s/pre_last_key.maps.tsv", dir);
  fd = open(path, O_WRONLY | O_CREAT | O_TRUNC | O_CLOEXEC, 0644);
  if (fd >= 0) {
    static const char hdr[] = "# base\tlen\tfile_offset\tcanonical_cs\tpath\n";
    ssize_t _hw = write(fd, hdr, sizeof(hdr) - 1); (void)_hw;
    char line[512];
    size_t m = shim_file_mappings_count();
    for (size_t i = 0; i < m; ++i) {
      ShimFileMappingView v;
      shim_file_mappings_get(i, &v);
      int ln = snprintf(line, sizeof(line),
                        "0x%X\t0x%zX\t0x%zX\t0x%04X\t%s\n",
                        v.base, v.len, v.file_offset,
                        (unsigned)v.canonical_cs, v.path ? v.path : "");
      if (ln > 0) { ssize_t _w = write(fd, line, (size_t)ln); (void)_w; }
    }
    close(fd);
  }
}

void snapshot_write_to_bundle(const char *dir) {
  write_keys_log(dir);
  write_pre_key_snapshot(dir);
}

/* ===== atexit dump into last_exit_snapshot/ ===== */

static void dump_exit_snapshot(void) {
  /* Markers via raw write() so they appear even if stdout is in a weird
   * state — helps prove whether atexit actually ran. */
  const char *enter = "[ATEXIT] snapshot dump_exit_snapshot enter\n";
  ssize_t _w1 = write(2, enter, strlen(enter)); (void)_w1;
  if (!snap_present) {
    const char *skip =
        "[ATEXIT] snapshot: snap_present=0 (no key was consumed); skipping\n";
    ssize_t _w2 = write(2, skip, strlen(skip)); (void)_w2;
    return;
  }
  const char *dir = "last_exit_snapshot";
  mkdir(dir, 0755);
  write_pre_key_snapshot(dir);
  write_keys_log(dir);
  shim_crash_bundle_write_state(dir);
  shim_crash_bundle_write_trace_tail(dir);
  shim_lifecycle_dump_to_dir(dir);
  char tail[160];
  int n = snprintf(tail, sizeof(tail),
                   "[ATEXIT] snapshot wrote %s/ (snap from t=%llu us)\n",
                   dir, (unsigned long long)snap_cpu.elapsed_us);
  if (n > 0) { ssize_t _w3 = write(2, tail, (size_t)n); (void)_w3; }
}

void snapshot_init(void) {
  /* Register so save_crash_bundle calls our writer alongside the standard
   * crash files. */
  shim_set_bundle_extra_writer(snapshot_write_to_bundle);
  /* And on clean/abrupt exits, dump the latest snapshot to a fixed dir. */
  atexit(dump_exit_snapshot);
  /* save_manager_init is intentionally NOT called from here — save_manager
   * self-initialises lazily on its first save/load call. */
}

int snapshot_capture_and_write(const char *dir) {
  if (capture_into_snap() != 0) return -1;
  write_pre_key_snapshot(dir);
  return 0;
}

/* ===== Restore from snapshot ===== */

int snapshot_restore_and_resume(const char *bundle_dir) {
  char path[512];
  save_manager_sr_log("restore TRY bundle=%s", bundle_dir);

  /* 1. memory */
  snprintf(path, sizeof(path), "%s/pre_last_key.bin", bundle_dir);
  int fd = open(path, O_RDONLY);
  if (fd < 0) {
    fprintf(stderr, "restore: cannot open %s: %s\n", path, strerror(errno));
    save_manager_sr_log("restore FAIL bundle=%s reason=open_memory errno=%d",
                        bundle_dir, errno);
    return -1;
  }
  size_t off = 0;
  while (off < SHIM_MEMORY_SIZE) {
    ssize_t r = read(fd, virtual_memory + off, SHIM_MEMORY_SIZE - off);
    if (r <= 0) { close(fd); fprintf(stderr, "restore: short read on %s\n", path); return -1; }
    off += (size_t)r;
  }
  close(fd);

  /* 2. cpu state struct */
  snprintf(path, sizeof(path), "%s/pre_last_key.state.bin", bundle_dir);
  fd = open(path, O_RDONLY);
  if (fd < 0) {
    fprintf(stderr, "restore: cannot open %s: %s\n", path, strerror(errno));
    return -1;
  }
  off = 0;
  while (off < sizeof(snap_cpu)) {
    ssize_t r = read(fd, ((char *)&snap_cpu) + off, sizeof(snap_cpu) - off);
    if (r <= 0) { close(fd); fprintf(stderr, "restore: short read on %s\n", path); return -1; }
    off += (size_t)r;
  }
  close(fd);
  if (snap_cpu.lcall_d != 0 || snap_cpu.isr_d != 0) {
    fprintf(stderr,
            "restore: refusing — snapshot has lcall_depth=%u isr_depth=%u "
            "(non-zero stacks not serialized; pick a snapshot taken while "
            "the game was at a clean keyboard wait)\n",
            (unsigned)snap_cpu.lcall_d, (unsigned)snap_cpu.isr_d);
    return -1;
  }

  /* 2b. Shim runtime state — video mode, OPL2 regs, PIT, DOS heap pointer,
   * pending IRQs. Missing this file on older bundles is non-fatal (we'll
   * just restore with current C-globals, the legacy lossy behavior); but
   * a version mismatch on the struct IS fatal, signaling the bundle was
   * captured with a different binary. */
  snprintf(path, sizeof(path), "%s/pre_last_key.shim_state.bin", bundle_dir);
  fd = open(path, O_RDONLY);
  if (fd >= 0) {
    ShimRuntimeState rt;
    off = 0;
    while (off < sizeof(rt)) {
      ssize_t r = read(fd, ((char *)&rt) + off, sizeof(rt) - off);
      if (r <= 0) break;
      off += (size_t)r;
    }
    close(fd);
    if (off != sizeof(rt)) {
      fprintf(stderr,
              "restore: short read on %s (%zu of %zu bytes) — refusing\n",
              path, off, sizeof(rt));
      return -1;
    }
    if (shim_runtime_state_restore(&rt) != 0) {
      /* shim_runtime_state_restore already printed a version-mismatch
       * message; bail rather than continue with mismatched layout. */
      return -1;
    }
  } else {
    fprintf(stderr,
            "restore: no shim_state.bin in bundle — restoring with "
            "current C-globals (legacy lossy behavior). Re-capture the "
            "snapshot with the current binary for a stable restore.\n");
  }

  /* 3. file_mappings */
  snprintf(path, sizeof(path), "%s/pre_last_key.maps.tsv", bundle_dir);
  FILE *mf = fopen(path, "r");
  if (!mf) {
    fprintf(stderr, "restore: cannot open %s: %s\n", path, strerror(errno));
    return -1;
  }
  shim_file_mappings_reset();
  char line[1024];
  while (fgets(line, sizeof(line), mf)) {
    if (line[0] == '#' || line[0] == '\n' || line[0] == '\0') continue;
    unsigned base, canonical;
    size_t len, file_off;
    char tab_path[768];
    if (sscanf(line, "0x%x\t0x%zx\t0x%zx\t0x%x\t%767[^\n]",
               &base, &len, &file_off, &canonical, tab_path) != 5) continue;
    shim_file_mappings_add_for_restore(tab_path, base, len, file_off,
                                       (uint16_t)canonical);
  }
  fclose(mf);
  fprintf(stderr, "restore: loaded %zu file mappings\n",
          shim_file_mappings_count());

  /* 4. cpu registers + flags */
  cs = snap_cpu.r_cs; ip = snap_cpu.r_ip;
  ss = snap_cpu.r_ss; sp = snap_cpu.r_sp;
  ds = snap_cpu.r_ds; es = snap_cpu.r_es;
  ax = snap_cpu.r_ax; bx = snap_cpu.r_bx;
  cx = snap_cpu.r_cx; dx = snap_cpu.r_dx;
  si = snap_cpu.r_si; di = snap_cpu.r_di; bp = snap_cpu.r_bp;
  CF = snap_cpu.f_CF; PF = snap_cpu.f_PF; ZF = snap_cpu.f_ZF; SF = snap_cpu.f_SF;
  OF = snap_cpu.f_OF; IF = snap_cpu.f_IF; DF = snap_cpu.f_DF;

  /* 5. keyboard queue */
  shim_kbd_state_restore(&snap_cpu.kbd);

  /* 6. re-enter the binary that was executing at capture time. The active
   * binary name was saved alongside cs:ip because cs:ip arithmetic doesn't
   * round-trip to the active binary during canonical_cs swaps or inline
   * _impl calls. expected_retip=0xFFFF is a sentinel that won't match
   * anything on the simulated stack, so the first ret in the resumed
   * function dispatches via near_ret_tail to the real address that was on
   * the stack. */
  fprintf(stderr, "restore: resuming at cs:ip=%04X:%04X in binary '%s' pc=0x%04X\n",
          cs, ip, snap_cpu.active_binary[0] ? snap_cpu.active_binary : "<none>",
          snap_cpu.r_ip);
  /* Static-dispatch resume (retained NULL-but-shaped; a JIT-only run never
   * takes this branch since there are no static case-keys): the saved position
   * is a case-key in the active binary's own dispatch switch, so route by
   * binary name (cs:ip arithmetic doesn't
   * round-trip during canonical_cs swaps / inline _impl calls). A JIT position
   * -- ip is NOT a case-key in the active binary because the game was executing
   * inside a JIT chunk -- falls through to the dispatch_via_binary path below,
   * which re-JITs from the restored memory and re-enters the chunk. */
  if (snap_cpu.active_binary[0] &&
      shim_pc_is_case_key(snap_cpu.active_binary, (uint32_t)ip)) {
    ShimDispatchFn fn = shim_dispatch_fn_by_module(snap_cpu.active_binary);
    if (!fn) {
      fprintf(stderr, "restore: no dispatch function for binary '%s'\n",
              snap_cpu.active_binary);
      save_manager_sr_log("restore FAIL bundle=%s reason=no_dispatch_fn "
                          "active_binary=%s",
                          bundle_dir, snap_cpu.active_binary);
      return -1;
    }
    save_manager_sr_log("restore OK bundle=%s active_binary=%s cs:ip=%04X:%04X "
                        "pc=0x%04X (dispatched via active_binary)",
                        bundle_dir, snap_cpu.active_binary, cs, ip,
                        snap_cpu.r_ip);
    shim_enter_binary(snap_cpu.active_binary);
    fn((int)snap_cpu.r_ip, 0xFFFF, "<restore>", __func__, 0);
    /* fn() bypasses dispatch_via_binary's trampoline loop. If a
     * cross-binary near_ret_tail inside fn() set tail_dispatch_pending
     * and returned, the request would be silently dropped here and the
     * game would appear to "exit cleanly" right after restore. Drain
     * any pending request through the trampoline loop. */
    shim_drain_pending_tail_dispatch("<restore>", __func__, 0);
    shim_leave_binary();
    return 0;
  }
  /* JIT resume (ip inside a JIT chunk) or legacy fallback (no active_binary
   * tracked). Route via cs:ip arithmetic: dispatch_via_binary resolves the
   * linear address to a JIT chunk -- re-JITing it from the just-restored memory
   * if the chunk table is empty in this fresh process -- or to the right
   * static-dispatch binary. Enter the saved active binary first (if any) so the resumed JIT
   * code runs under the correct active-binary / canonical_cs context, the same
   * as it did at capture time. */
  if (snap_cpu.active_binary[0]) shim_enter_binary(snap_cpu.active_binary);
  save_manager_sr_log("restore OK bundle=%s cs:ip=%04X:%04X active_binary=%s "
                      "(resuming via run_machine)",
                      bundle_dir, cs, ip,
                      snap_cpu.active_binary[0] ? snap_cpu.active_binary
                                                : "<none>");
  /* Resume by re-entering the normal top-level loop from the restored cs:ip.
   * run_machine() re-resolves cs:ip to its owning chunk after EVERY chunk --
   * re-JITing from the restored memory as needed and following the restored
   * stack's return addresses across chunk boundaries. A single
   * dispatch_via_binary (the static-dispatch path) bubbles out at the first near-ret to a
   * not-yet-JIT'd target, which is why a JIT-position resume must use the loop.
   * run_machine returns only if the game bubbles out without a DOS terminate;
   * otherwise it ends the process via exit() inside the game's INT 21h AH=4Ch. */
  run_machine();
  if (snap_cpu.active_binary[0]) shim_leave_binary();
  return 0;
}
