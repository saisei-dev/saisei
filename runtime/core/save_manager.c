/* scripts/save_manager.c
 *
 * Gameplay save-state ring. See save_manager.h for design notes.
 *
 * No coupling to shims.c: triggers come from virtual_display_sdl.c (time)
 * and snapshot.c::snapshot_on_key_consumed (key). All shim state is read
 * through the accessors declared in shims.h; the on-disk snapshot format
 * is owned by snapshot.c via snapshot_capture_and_write().
 */

#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <stdarg.h>
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

#define SLOT_COUNT 5

/* Target ages per slot in microseconds. slot_0 is the freshest; slot_4
 * caps at ~3 minutes back. Promotion rule: slot[i-1] is moved to slot[i]
 * once its age reaches SLOT_TARGET_US[i]. */
static const uint64_t SLOT_TARGET_US[SLOT_COUNT] = {
    5ULL   * 1000000ULL,   /* slot_0:  ~5s    */
    15ULL  * 1000000ULL,   /* slot_1:  ~15s   */
    45ULL  * 1000000ULL,   /* slot_2:  ~45s   */
    135ULL * 1000000ULL,   /* slot_3:  ~2.25 min */
    180ULL * 1000000ULL,   /* slot_4:  3 min cap */
};

typedef struct {
  uint64_t write_us;  /* monotonic timestamp (this process's clock) */
  int      valid;
} SlotInfo;

static SlotInfo slots[SLOT_COUNT];
static int      initialized;
static int      initial_cascade_pending;  /* one-shot shift on first save */
static const char SAVES_ROOT[] = "saves";
static const char SR_LOG[] = "save_restore.log";

/* Human-readable activity log. Appended from save_manager (save events,
 * rotations), snapshot.c (restore events), and the source (binary
 * start/exit lines). One file at runtime_dir/save_restore.log;
 * readable by `tail -f`. Each line: ISO-timestamp + event + key=value
 * details. */
void save_manager_sr_log(const char *event_fmt, ...) {
  FILE *fp = fopen(SR_LOG, "a");
  if (!fp) return;
  time_t now = time(NULL);
  struct tm tm_; gmtime_r(&now, &tm_);
  fprintf(fp, "[%04d-%02d-%02dT%02d:%02d:%02dZ] ",
          tm_.tm_year + 1900, tm_.tm_mon + 1, tm_.tm_mday,
          tm_.tm_hour, tm_.tm_min, tm_.tm_sec);
  va_list ap;
  va_start(ap, event_fmt);
  vfprintf(fp, event_fmt, ap);
  va_end(ap);
  fputc('\n', fp);
  fclose(fp);
}

#define sr_log save_manager_sr_log

static struct timespec start_ts;

static uint64_t now_us(void) {
  struct timespec t;
  clock_gettime(CLOCK_MONOTONIC, &t);
  uint64_t s  = (uint64_t)(t.tv_sec  - start_ts.tv_sec);
  int64_t  ns = (int64_t)t.tv_nsec - (int64_t)start_ts.tv_nsec;
  return s * 1000000ULL + (uint64_t)(ns / 1000);
}

static int rmtree(const char *path) {
  DIR *d = opendir(path);
  if (!d) return (errno == ENOENT) ? 0 : -1;
  struct dirent *e;
  char child[512];
  while ((e = readdir(d))) {
    if (strcmp(e->d_name, ".") == 0 || strcmp(e->d_name, "..") == 0) continue;
    snprintf(child, sizeof(child), "%s/%s", path, e->d_name);
    struct stat st;
    if (lstat(child, &st) == 0 && S_ISDIR(st.st_mode)) {
      rmtree(child);
    } else {
      unlink(child);
    }
  }
  closedir(d);
  return rmdir(path);
}

static void slot_dir(int i, char *out, size_t cap) {
  snprintf(out, cap, "%s/slot_%d", SAVES_ROOT, i);
}

static void write_meta(const char *dir, uint64_t elapsed_us, int slot,
                       const char *reason) {
  char path[512];
  snprintf(path, sizeof(path), "%s/meta.json", dir);
  int fd = open(path, O_WRONLY | O_CREAT | O_TRUNC | O_CLOEXEC, 0644);
  if (fd < 0) return;
  char buf[320];
  time_t now = time(NULL);
  struct tm tm_;
  gmtime_r(&now, &tm_);
  int n = snprintf(buf, sizeof(buf),
                   "{\n"
                   "  \"slot\": %d,\n"
                   "  \"elapsed_us\": %llu,\n"
                   "  \"written_at_utc\": \"%04d-%02d-%02dT%02d:%02d:%02dZ\",\n"
                   "  \"trigger\": \"%s\"\n"
                   "}\n",
                   slot, (unsigned long long)elapsed_us,
                   tm_.tm_year + 1900, tm_.tm_mon + 1, tm_.tm_mday,
                   tm_.tm_hour, tm_.tm_min, tm_.tm_sec,
                   reason ? reason : "");
  if (n > 0) { ssize_t _w = write(fd, buf, (size_t)n); (void)_w; }
  close(fd);
}

/* Walk saves/ and reconstruct slots[] from disk. Inherited slots get
 * write_us = 0 (their effective age starts at "process start"); if any
 * exist, the first save in this process force-cascades them down by one
 * position to preserve their relative order, sacrificing only slot_4. */
static void rediscover_slots(void) {
  int any = 0;
  for (int i = 0; i < SLOT_COUNT; ++i) {
    char d[256];
    slot_dir(i, d, sizeof(d));
    char probe[512];
    snprintf(probe, sizeof(probe), "%s/pre_last_key.bin", d);
    struct stat st;
    if (stat(probe, &st) != 0) { slots[i].valid = 0; continue; }
    slots[i].valid    = 1;
    slots[i].write_us = 0;
    any = 1;
  }
  initial_cascade_pending = any;
}

/* Check whether the active binary's dispatch has a SINGLE case keyed at
 * `ip` (treated as a 32-bit file_offset). Restore calls fn(snap_cpu.r_ip),
 * which routes to the dispatch case keyed at exactly that value. For
 * overlay binaries the same `ip` low-16 can correspond to MULTIPLE case
 * keys (e.g. an overlay archive has both `case 0x387B` AND `case 0x1387B` — both
 * bodies do `ip = 0x387B`). Without knowing the live dispatch pc we
 * can't tell which body the game is in, and restore would land in the
 * wrong one. Refuse those saves; wait for a SAFEPOINT whose case key
 * has no low-16 collision.
 *
 * The walk caps `h` at 0x10 (covers file_offsets up to 1MB). Empirically
 * across all current binaries the highest case-key upper-16 bits is 0x4
 * (an overlay archive max 0x41D90); 0x10 leaves headroom without much cost. */
static int ip_is_case_key(const char *module, uint16_t ip_val) {
  if (!module) return 0;
  if (!shim_pc_is_case_key(module, (uint32_t)ip_val)) return 0;
  for (uint32_t h = 1; h <= 0x10; ++h) {
    if (shim_pc_is_case_key(module, (h << 16) | (uint32_t)ip_val)) {
      return 0;  /* overlay collision — can't restore unambiguously */
    }
  }
  return 1;
}

void save_manager_init(void) {
  if (initialized) return;
  initialized = 1;
  mkdir(SAVES_ROOT, 0755);
  clock_gettime(CLOCK_MONOTONIC, &start_ts);
  rediscover_slots();
}

/* Walk oldest → newest, promote slot[i-1] → slot[i] when:
 *   - destination slot[i] is empty (don't overwrite older history with
 *     younger content), AND
 *   - source slot[i-1] has aged past its OWN target (SLOT_TARGET[i-1]).
 * Iterating high → low means a slot vacated by a higher-index promotion
 * is available to receive the next promotion in this same pass. */
static void rotate(uint64_t now) {
  for (int i = SLOT_COUNT - 1; i >= 1; --i) {
    if (slots[i].valid) continue;            /* destination occupied */
    if (!slots[i - 1].valid) continue;       /* source empty */
    uint64_t age = now - slots[i - 1].write_us;
    if (age < SLOT_TARGET_US[i - 1]) continue;  /* source not ripe */

    char src[256], dst[256];
    slot_dir(i - 1, src, sizeof(src));
    slot_dir(i,     dst, sizeof(dst));
    rmtree(dst);
    if (rename(src, dst) == 0) {
      slots[i]            = slots[i - 1];
      slots[i - 1].valid  = 0;
    }
  }
}

static int can_save_now(void) {
  /* snapshot_restore_and_resume refuses to load a state with nonzero
   * lcall/isr depths; never write a slot we can't load. */
  if (lcall_depth != 0 || isr_depth != 0) return 0;
  /* JIT resting point: ip is a case-key in a live JIT chunk at the current cs.
   * Restore re-enters this via dispatch_via_binary (cs:ip arithmetic), which
   * re-JITs from the restored memory and does NOT need the active-binary name,
   * so a JIT position is savable even when no static-dispatch binary is active
   * (always, in the JIT-only pipeline). This is what makes the JIT build
   * savable at all. */
  if (shim_pc_is_jit_case_key(cs, ip)) return 1;
  /* Static-dispatch path (retained NULL-but-shaped; unused while every address
   * routes through the JIT): restore routes by binary name (cs:ip arithmetic doesn't
   * round-trip during canonical_cs swaps), so require a named active binary and
   * an ip that is a dispatch case-key in it -- otherwise restore would call
   * <binary>_dispatch(ip) at a non-case ip → [BUG] Unhandled pc → abort. */
  const char *binary = shim_active_binary();
  if (binary == NULL) return 0;
  if (!ip_is_case_key(binary, ip)) return 0;
  return 1;
}

/* One-shot full shift-down used on the first save after rediscovery.
 * Preserves the entire inherited ring at slot positions [1..N-1] and
 * sacrifices whatever was at slot_{N-1}. */
static void initial_cascade(void) {
  for (int i = SLOT_COUNT - 1; i >= 1; --i) {
    if (!slots[i - 1].valid) continue;
    char src[256], dst[256];
    slot_dir(i - 1, src, sizeof(src));
    slot_dir(i,     dst, sizeof(dst));
    rmtree(dst);
    if (rename(src, dst) == 0) {
      slots[i]            = slots[i - 1];
      slots[i - 1].valid  = 0;
    }
  }
}

static void try_save(const char *reason, int bypass_throttle) {
  if (!initialized) save_manager_init();
  if (!can_save_now()) {
    /* Spammy if logged unconditionally; only log refusals worth seeing.
     * We use whichever sub-condition failed to construct a specific
     * reason — depths, no active binary, or non-case-key ip. */
    const char *binary = shim_active_binary();
    if (binary != NULL) {
      const char *why;
      if (lcall_depth != 0 || isr_depth != 0) {
        why = "depth";
      } else if (!shim_pc_is_case_key(binary, (uint32_t)ip)) {
        why = "ip_not_case_key";
      } else if (!ip_is_case_key(binary, ip)) {
        /* Strict check refused because overlay collision (some other
         * case_key with the same low-16 exists; restore can't pick
         * unambiguously). */
        why = "ip_ambiguous_overlay";
      } else {
        why = "unknown";
      }
      sr_log("save SKIP reason=%s trigger=%s lcall_d=%u isr_d=%u "
             "active=%s cs:ip=%04X:%04X",
             why, reason, lcall_depth, isr_depth, binary, cs, ip);
    }
    return;
  }

  uint64_t now = now_us();
  uint64_t ref_us = slots[0].valid ? slots[0].write_us : 0;
  if (!bypass_throttle && (now - ref_us) < SLOT_TARGET_US[0]) {
    return;  /* throttled, silent */
  }

  if (initial_cascade_pending) {
    initial_cascade();
    initial_cascade_pending = 0;
    sr_log("save ROTATE initial_cascade (inherited ring shifted down)");
  } else {
    /* Track which rotations actually happened for the log. */
    int before_valid[SLOT_COUNT];
    for (int i = 0; i < SLOT_COUNT; ++i) before_valid[i] = slots[i].valid;
    rotate(now);
    for (int i = 1; i < SLOT_COUNT; ++i) {
      if (slots[i].valid && !before_valid[i]) {
        sr_log("save ROTATE slot_%d -> slot_%d (slot_%d aged past %llu us)",
               i - 1, i, i - 1,
               (unsigned long long)SLOT_TARGET_US[i - 1]);
      }
    }
  }

  char tmp[256], dst[256];
  snprintf(tmp, sizeof(tmp), "%s/slot_0_tmp", SAVES_ROOT);
  slot_dir(0, dst, sizeof(dst));

  rmtree(tmp);
  if (mkdir(tmp, 0755) != 0 && errno != EEXIST) {
    sr_log("save FAIL slot_0 reason=mkdir errno=%d", errno);
    return;
  }

  if (snapshot_capture_and_write(tmp) != 0) {
    sr_log("save FAIL slot_0 reason=capture");
    rmtree(tmp);
    return;
  }
  write_meta(tmp, now, 0, reason);

  rmtree(dst);
  if (rename(tmp, dst) != 0) {
    sr_log("save FAIL slot_0 reason=rename errno=%d", errno);
    rmtree(tmp);
    return;
  }

  slots[0].write_us = now;
  slots[0].valid    = 1;
  sr_log("save OK slot_0 trigger=%s active=%s cs:ip=%04X:%04X "
         "elapsed_us=%llu", reason,
         shim_active_binary() ? shim_active_binary() : "<none>",
         cs, ip, (unsigned long long)now);
}

void save_manager_tick(void)             { try_save("tick", 0); }

/* ===== Manual user-driven save / load (Cmd+F1, Cmd+F2) ===== */

/* Save anchor — set whenever the game consumes a keyboard event (via INT
 * 16h or port 0x60 ISR read). At those moments the game has just finished
 * a kbd wait and the next safe SAFEPOINT is naturally at a stable resting
 * point of the main loop (registers reflect "about to handle input", not
 * mid-function transients like dx=<port> set by a caller). Without this
 * gate, save fired at the next safe-looking SAFEPOINT regardless of game
 * state — which could land mid-music-tick with dx holding a transient
 * port (e.g. dx=0x97 mid an overlay archive's func_37A6), unrestoreable because the
 * dispatch entry can't reproduce the caller chain that set dx. */
static int save_request_pending;
static int save_anchor_armed;

void save_manager_request_save(void) {
  if (!save_request_pending) {
    sr_log("save REQUEST (manual via Cmd+F1; armed — waiting for next "
           "kbd-consumption anchor then a safe SAFEPOINT)");
  }
  save_request_pending = 1;
  save_anchor_armed = 0;  /* require a fresh post-request kbd event */
}

/* Wired from snapshot.c::snapshot_on_key_consumed (BIOS INT 16h path
 * and port 0x60 ISR read). Arms the save anchor; the actual capture
 * still waits for the next depth-0 SAFEPOINT with a unique case-key ip. */
void save_manager_on_key_consumed(void) {
  if (save_request_pending) {
    save_anchor_armed = 1;
  }
}

void save_manager_poll_pending(void) {
  if (!save_request_pending) return;
  if (!save_anchor_armed) return;
  if (!initialized) save_manager_init();
  /* Snapshot slot_0's write_us before so we can detect whether try_save
   * actually wrote a slot (vs. silently skipping for depth/ip reasons). */
  uint64_t before_us = slots[0].valid ? slots[0].write_us : 0;
  int before_valid   = slots[0].valid;
  try_save("manual", /*bypass_throttle=*/1);
  if (slots[0].valid &&
      (!before_valid || slots[0].write_us != before_us)) {
    save_request_pending = 0;
    save_anchor_armed = 0;
  }
}

/* Find the freshest valid slot dir. Prefers slot_0; otherwise the
 * lowest-numbered slot that has a pre_last_key.bin on disk. Returns 0
 * on success and fills `out`; returns -1 if nothing valid was found. */
static int find_latest_slot_dir(char *out, size_t cap) {
  for (int i = 0; i < SLOT_COUNT; ++i) {
    char d[256];
    slot_dir(i, d, sizeof(d));
    char probe[512];
    snprintf(probe, sizeof(probe), "%s/pre_last_key.bin", d);
    struct stat st;
    if (stat(probe, &st) == 0) {
      snprintf(out, cap, "%s", d);
      return 0;
    }
  }
  return -1;
}

/* Slurp /proc/self/cmdline (NUL-separated argv) into a heap array.
 * Caller frees both the strings and the returned argv. */
static char **read_proc_cmdline(int *argc_out) {
  *argc_out = 0;
  int fd = open("/proc/self/cmdline", O_RDONLY | O_CLOEXEC);
  if (fd < 0) return NULL;
  char  *buf = NULL;
  size_t cap = 0, len = 0;
  for (;;) {
    if (len + 4096 > cap) {
      size_t ncap = cap ? cap * 2 : 4096;
      char *nbuf = realloc(buf, ncap);
      if (!nbuf) { free(buf); close(fd); return NULL; }
      buf = nbuf; cap = ncap;
    }
    ssize_t r = read(fd, buf + len, cap - len);
    if (r < 0) { if (errno == EINTR) continue; free(buf); close(fd); return NULL; }
    if (r == 0) break;
    len += (size_t)r;
  }
  close(fd);
  int n = 0;
  for (size_t i = 0; i < len; ++i) if (buf[i] == '\0') ++n;
  if (n == 0) { free(buf); return NULL; }
  char **argv = calloc((size_t)n + 1, sizeof(char *));
  if (!argv) { free(buf); return NULL; }
  int idx = 0;
  size_t start = 0;
  for (size_t i = 0; i < len && idx < n; ++i) {
    if (buf[i] == '\0') {
      argv[idx++] = strdup(buf + start);
      start = i + 1;
    }
  }
  free(buf);
  *argc_out = idx;
  return argv;
}

void save_manager_request_load_latest(void) {
  if (!initialized) save_manager_init();
  char dir[256];
  if (find_latest_slot_dir(dir, sizeof(dir)) != 0) {
    sr_log("load REQUEST FAIL reason=no_valid_slot (Cmd+F2 pressed with empty ring)");
    fprintf(stderr,
            "[Cmd+F2] load: no save slots exist yet. Press Cmd+F1 first.\n");
    return;
  }
  /* Resolve to absolute so the re-execed binary doesn't depend on cwd
   * being preserved. /proc/self/exe is already absolute. */
  char abs_dir[PATH_MAX];
  if (realpath(dir, abs_dir) == NULL) {
    snprintf(abs_dir, sizeof(abs_dir), "%s", dir);
  }

  int orig_argc = 0;
  char **orig_argv = read_proc_cmdline(&orig_argc);

  /* Build new argv: orig argv with any existing --restore-from stripped,
   * then append --restore-from <abs_dir>. */
  int max_new = orig_argc + 3;
  char **new_argv = calloc((size_t)max_new, sizeof(char *));
  if (!new_argv) {
    sr_log("load REQUEST FAIL reason=oom");
    if (orig_argv) {
      for (int i = 0; i < orig_argc; ++i) free(orig_argv[i]);
      free(orig_argv);
    }
    return;
  }
  int n = 0;
  if (orig_argv && orig_argc > 0) {
    new_argv[n++] = orig_argv[0];
    for (int i = 1; i < orig_argc; ++i) {
      const char *a = orig_argv[i];
      if (strcmp(a, "--restore-from") == 0) {
        ++i;  /* skip its value too */
        continue;
      }
      if (strncmp(a, "--restore-from=", 15) == 0) continue;
      new_argv[n++] = orig_argv[i];
    }
  } else {
    /* Fallback if /proc/self/cmdline was unavailable. */
    new_argv[n++] = strdup("program");
  }
  new_argv[n++] = strdup("--restore-from");
  new_argv[n++] = strdup(abs_dir);
  new_argv[n]   = NULL;

  sr_log("load REQUEST OK bundle=%s (Cmd+F2 re-exec /proc/self/exe argc=%d)",
         abs_dir, n);
  fprintf(stderr, "[Cmd+F2] load: re-exec'ing with --restore-from %s\n", abs_dir);
  fflush(stderr);

  execv("/proc/self/exe", new_argv);
  /* execv only returns on failure. */
  int err = errno;
  sr_log("load REQUEST FAIL reason=execv errno=%d (%s)", err, strerror(err));
  fprintf(stderr, "[Cmd+F2] load: execv failed: %s\n", strerror(err));
  /* Best-effort cleanup; the process is in a degraded state but should
   * still be playable. */
  for (int i = 0; i < n; ++i) free(new_argv[i]);
  free(new_argv);
  free(orig_argv);
}
