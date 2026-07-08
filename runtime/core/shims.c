#include <fcntl.h>
#include <dirent.h>
#include <fnmatch.h>
#include <limits.h>
#ifndef PATH_MAX
#define PATH_MAX 4096
#endif
#include <setjmp.h>
#include <signal.h>
#include <sys/stat.h>
#include <sys/time.h>
#include <sys/types.h>
#include <termios.h>
#include <time.h>
#include <unistd.h>
#include <dlfcn.h>
#define SHIMS_DISABLE_MACROS
#include "shims.h"
#undef SHIMS_DISABLE_MACROS
#include "snapshot.h"
#include <ctype.h>
#include <errno.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <strings.h>
#define STB_IMAGE_WRITE_IMPLEMENTATION
#include "game_config.h"
#include "mouse.h"
#include "stb_image_write.h"
#include "virtual_display.h"

__attribute__((weak)) const GameConfig game_config = {
    .name = "default",
    .program_path = NULL,
    .entry = NULL,
    .call_targets = NULL,
    .call_target_count = 0,
};

__attribute__((weak)) void virtual_display_init(int width, int height,
                                                int scale) {
  (void)width;
  (void)height;
  (void)scale;
}

__attribute__((weak)) void virtual_display_shutdown(void) {}

__attribute__((weak)) void virtual_display_present(const uint8_t *vram,
                                                   int pitch, int w, int h,
                                                   const uint8_t *palette,
                                                   uint8_t palette_mask) {
  (void)vram;
  (void)pitch;
  (void)w;
  (void)h;
  (void)palette;
  (void)palette_mask;
}

__attribute__((weak)) void virtual_display_poll_input(void) {}

__attribute__((weak)) void virtual_display_set_mode(int mode) { (void)mode; }

__attribute__((weak)) void virtual_display_configure(int width, int height) {
  (void)width;
  (void)height;
}

/* Trace/diagnostic stdout is OFF by default; `saisei run --verbose` (which sets
 * SAISEI_VERBOSE, read in the constructor below) turns it on. Crash/exit
 * diagnostics and real errors bypass this gate and always print. */
static int shim_stdout_enabled = 0;


/*
 * Centralized flush: fflush stdio + fsync the underlying FDs so any kernel
 * write-back to a redirected file completes before we hand control to the
 * runtime (or an atexit handler).  NOT async-signal-safe — call from
 * exit/abort paths only, never from a signal handler (the signal handler
 * uses raw write()/fsync()).
 *
 * Every explicit exit-like path in this file goes through this helper.  An
 * atexit registration also catches paths that go through `return from main`
 * or unfamiliar exit() callers.  Belt and suspenders, because the user has
 * repeatedly hit cases where the captured log was missing the final lines.
 */
static FILE *trace_file_fp;  /* forward — defined alongside the trace writer */
static FILE *lifecycle_fp;   /* forward — defined alongside the lifecycle writer */
/* Forward declarations for the per-site dispatch_depth counters (defined
 * alongside the dispatch_depth global). The counter values are read inside
 * dispatch_depth_guard, which is defined above the actual storage. */
extern uint64_t dd_inc_call_table, dd_dec_call_table;
extern uint64_t dd_inc_via_binary, dd_dec_via_binary;
extern uint64_t dd_inc_overlay_first, dd_dec_overlay_first;

/* Cross-binary trampoline state — set by near_ret_tail and consumed by the
 * dispatch_via_binary loop. Definitions live further down with the
 * dispatch_via_binary helpers; declared here so invoke_isr/lcall_table can
 * snapshot/restore them across setjmp boundaries. */
extern bool     tail_dispatch_pending;
extern uint32_t tail_dispatch_addr;
extern uint16_t tail_dispatch_expected;
static void shim_flush_all_streams(void) {
  fflush(stdout);
  fflush(stderr);
  if (trace_file_fp) {
    fflush(trace_file_fp);
    int tf = fileno(trace_file_fp);
    if (tf >= 0) fsync(tf);
  }
  if (lifecycle_fp) {
    fflush(lifecycle_fp);
    int lf = fileno(lifecycle_fp);
    if (lf >= 0) fsync(lf);
  }
  int out_fd = fileno(stdout);
  int err_fd = fileno(stderr);
  if (out_fd >= 0) fsync(out_fd);
  if (err_fd >= 0 && err_fd != out_fd) fsync(err_fd);
}

/*
 * Strictly async-signal-safe crash handler.  POSIX does NOT list fflush as
 * signal-safe — if main was mid-vfprintf holding the FILE* lock, calling
 * fflush from the handler can deadlock or corrupt state.  So we restrict
 * the handler to write(), fsync(), signal(), raise() — all on the
 * async-signal-safe whitelist — and we leave any in-flight vfprintf bytes
 * wherever the syscall happened to land.  The handler writes a [CRASH]
 * marker to BOTH stdout and stderr so a single redirect (`> debug.txt`,
 * with or without `2>&1`) captures the cause of death.
 */
static void emit_crash_marker(int fd, int signum, const char *name) {
  if (fd < 0) return;
  /* Build the message without stdio.  snprintf into a stack buffer is
   * async-signal-safe in practice on glibc (it doesn't touch FILE locks
   * or the malloc arena for fixed-width conversions).
   *
   * Include the simulated CPU state and currently-executing binary so the
   * user can locate the crash without inspecting the trace. cs:ip is the
   * primary clue — it tells you which translated case body was running
   * when the host SIGSEGV fired. */
  const char *binary_name = shim_active_binary();
  uint32_t linear = ((uint32_t)cs << 4) + ((uint32_t)ip & 0xFFFF);
  /* Only the genuine fault signals indicate a host-level error inside a
   * translated case body. SIGTERM/SIGINT/SIGPIPE are EXTERNAL termination
   * (timeout, Ctrl-C, closed pipe) — not a fault; the cs:ip below is just where
   * execution happened to be. Keep the hint honest so a timeout/interrupt isn't
   * misread as a crash (it was, repeatedly, before this distinction existed). */
  int is_fault = (signum == SIGSEGV || signum == SIGBUS || signum == SIGILL ||
                  signum == SIGFPE  || signum == SIGABRT);
  const char *hint = is_fault
      ? "[CRASH]   Host-level fault inside the translated case body. Search the "
        "trace tail for the last Trace:/[BUG] line — the case it was in is where "
        "the out-of-bounds memory access happened.\n"
      : "[CRASH]   EXTERNAL termination (timeout / Ctrl-C / closed pipe), NOT a "
        "fault. The cs:ip above is just where execution was when the signal "
        "arrived; the program did not crash.\n";
  char buf[640];
  int n = snprintf(buf, sizeof(buf),
                   "\n[CRASH] terminated by signal %d (%s)\n"
                   "[CRASH]   cs:ip=%04X:%04X linear=0x%05X\n"
                   "[CRASH]   active_binary=%s\n"
                   "[CRASH]   ax=%04X bx=%04X cx=%04X dx=%04X "
                   "si=%04X di=%04X bp=%04X ss:sp=%04X:%04X\n"
                   "[CRASH]   ds=%04X es=%04X\n"
                   "[CRASH]   depths: lcall=%u isr=%u dispatch=%u critical=%u\n"
                   "%s",
                   signum, name, cs, ip, linear,
                   binary_name ? binary_name : "<none>",
                   ax, bx, cx, dx, si, di, bp, ss, sp,
                   ds, es,
                   (unsigned)lcall_depth, (unsigned)isr_depth,
                   (unsigned)dispatch_depth, (unsigned)critical_depth,
                   hint);
  if (n <= 0) return;
  if (n > (int)sizeof(buf)) n = (int)sizeof(buf);
  ssize_t off = 0;
  while (off < n) {
    ssize_t w = write(fd, buf + off, (size_t)(n - off));
    if (w < 0) {
      if (errno == EINTR) continue;
      break;
    }
    off += w;
  }
  fsync(fd);
}

static void crash_signal_handler(int signum) {
  const char *name = "?";
  switch (signum) {
    case SIGSEGV: name = "SIGSEGV"; break;
    case SIGABRT: name = "SIGABRT"; break;
    case SIGBUS:  name = "SIGBUS";  break;
    case SIGFPE:  name = "SIGFPE";  break;
    case SIGILL:  name = "SIGILL";  break;
    case SIGINT:  name = "SIGINT";  break;
    case SIGTERM: name = "SIGTERM"; break;
    case SIGPIPE: name = "SIGPIPE"; break;
    default: break;
  }
  int out_fd = fileno(stdout);
  int err_fd = fileno(stderr);
  /* Write to BOTH streams: stdout for users who only redirect stdout,
   * stderr for tools/CI that watch stderr.  If both happen to be the same
   * underlying file (e.g. `2>&1` to a regular file), the marker appears
   * twice — small price for never missing it. */
  emit_crash_marker(out_fd, signum, name);
  if (err_fd != out_fd) {
    emit_crash_marker(err_fd, signum, name);
  }
  /* Restore default disposition and re-raise so the exit code / core dump
   * reflect the original signal. */
  signal(signum, SIG_DFL);
  raise(signum);
}

/* Expose so virtual_display_sdl.c can re-install after SDL_Init (SDL
 * installs its own signal handlers and silently overrides ours, which is
 * why SIGSEGV stopped producing [CRASH] markers). */
void shim_reinstall_crash_handlers(void);

static void install_crash_handlers(void) {
  /* Install a dedicated signal stack so the handler can run even if the
   * crash was caused by stack overflow on the main thread. Without this,
   * SIGSEGV from a blown stack would re-fault inside the handler and die
   * silently (no [CRASH] line). 64K is plenty for snprintf + write. */
  static char altstack[SIGSTKSZ < 65536 ? 65536 : SIGSTKSZ];
  stack_t alt;
  alt.ss_sp = altstack;
  alt.ss_size = sizeof(altstack);
  alt.ss_flags = 0;
  sigaltstack(&alt, NULL);

  struct sigaction sa;
  memset(&sa, 0, sizeof(sa));
  sa.sa_handler = crash_signal_handler;
  sigemptyset(&sa.sa_mask);
  /* SA_RESETHAND ensures that if our handler itself faults, the second
   * signal kills the process via the default handler rather than recursing.
   * SA_ONSTACK runs the handler on altstack so stack-overflow crashes can
   * still be reported. */
  sa.sa_flags = SA_RESETHAND | SA_ONSTACK;
  sigaction(SIGSEGV, &sa, NULL);
  sigaction(SIGBUS,  &sa, NULL);
  sigaction(SIGABRT, &sa, NULL);
  sigaction(SIGFPE,  &sa, NULL);
  sigaction(SIGILL,  &sa, NULL);
  sigaction(SIGINT,  &sa, NULL);
  sigaction(SIGTERM, &sa, NULL);
  sigaction(SIGPIPE, &sa, NULL);
}

void shim_reinstall_crash_handlers(void) { install_crash_handlers(); }

/* Recursion guard for the dispatch entry points (call_table, long_jump,
 * near_ret_tail). Real games rarely nest binary dispatches more than 3-5
 * deep; anything past ~32 is uncontrolled recursion from stack imbalance
 * (the stack has bogus return addresses and each dispatch tail-dispatches
 * to the next one). Without this, the C stack overflows around depth ~100
 * and the process SIGSEGVs with no bundle. With this we abort cleanly,
 * write a bundle, and the diagnostic surfaces the chunk-swap or stack-
 * balance bug that put the bogus address on the stack. */
#define DISPATCH_DEPTH_LIMIT 2048
/* 2048 chosen so the guard fires well before the host C stack overflows
 * (observed SIGSEGV at depth=3508 with 8MB default stack). Hitting the
 * guard writes a full crash bundle with the per-site ++/dec/leak counters
 * + stack_writes.log; SIGSEGV from host overflow does not (async-signal-
 * safe handler can only print [CRASH] markers). Lower than 32768 by design
 * for the +6/music-event leak diagnosis. */
static void save_bug_bundle(const char *kind, uint32_t addr, const char *msg);
static void dispatch_depth_guard(const char *kind, uint32_t addr,
                                 const char *file, const char *func,
                                 int line) {
  if (dispatch_depth < DISPATCH_DEPTH_LIMIT) return;
  char msg[2048];
  int n = snprintf(msg, sizeof(msg),
      "[BUG] dispatch recursion limit hit: depth=%u (limit=%d)\n"
      "[BUG]   triggering site: %s addr=0x%05X (%s:%s:%d)\n"
      "[BUG]   cs:ip=%04X:%04X ss:sp=%04X:%04X active_binary=%s\n"
      "[BUG]   depths: lcall=%u isr=%u dispatch=%u critical=%u\n"
      "[BUG]   ++/-- per-site (inc/dec/leak):\n"
      "[BUG]     call_table_impl      inc=%llu  dec=%llu  leak=%lld\n"
      "[BUG]     dispatch_via_binary  inc=%llu  dec=%llu  leak=%lld\n"
      "[BUG]     try_dispatch_overlay inc=%llu  dec=%llu  leak=%lld\n"
      "[BUG]   diagnosis: the simulated stack has bogus return addresses "
      "(likely from a chunk-swap stack imbalance or a translator near-ret "
      "mismatch). Each dispatch pops a bad value and tail-dispatches, "
      "growing the C stack without bound. Walk back through the trace "
      "tail's `near_ret_tail`/`call_table` sequence to find where the "
      "expected_retip first diverged from the popped value.\n",
      (unsigned)dispatch_depth, DISPATCH_DEPTH_LIMIT, kind, addr,
      file ? file : "?", func ? func : "?", line, cs, ip, ss, sp,
      shim_active_binary() ? shim_active_binary() : "<none>",
      (unsigned)lcall_depth, (unsigned)isr_depth,
      (unsigned)dispatch_depth, (unsigned)critical_depth,
      (unsigned long long)dd_inc_call_table, (unsigned long long)dd_dec_call_table,
      (long long)(dd_inc_call_table - dd_dec_call_table),
      (unsigned long long)dd_inc_via_binary, (unsigned long long)dd_dec_via_binary,
      (long long)(dd_inc_via_binary - dd_dec_via_binary),
      (unsigned long long)dd_inc_overlay_first, (unsigned long long)dd_dec_overlay_first,
      (long long)(dd_inc_overlay_first - dd_dec_overlay_first));
  if (n > 0) {
    shim_log_crash("%s", msg);
    save_bug_bundle("dispatch_recursion", addr, msg);
  }
  shim_flush_all_streams();
  abort();
}

/* Force-crash on stack drift detected at ISR/lcall boundaries. The
 * invariant: after an iret completes, sp must equal sp at invoke_isr
 * entry (flags+cs+ip pushed at entry, popped by iret = net 0). Likewise
 * after retf, sp must equal sp at lcall_table_impl entry (4 bytes
 * pushed, 4 bytes popped). Any difference is stack drift introduced by
 * an unbalanced push/pop somewhere in the dispatched body — typically
 * a translator bug or an unsupported instruction emit. */
void shim_check_stack_drift(const char *site, uint16_t expected_sp,
                            uint16_t actual_sp, const char *file,
                            const char *func, int line) {
  int16_t delta = (int16_t)(actual_sp - expected_sp);
  char msg[1024];
  int n = snprintf(msg, sizeof(msg),
      "[WARN] stack drift at %s boundary (non-fatal; continuing)\n"
      "  expected_sp=%04X  actual_sp=%04X  delta=%+d bytes\n"
      "  cs:ip=%04X:%04X  ss=%04X  source=%s:%s:%d\n"
      "  active_binary=%s  depths: lcall=%u isr=%u dispatch=%u\n"
      "  registers: ax=%04X bx=%04X cx=%04X dx=%04X si=%04X di=%04X bp=%04X\n"
      "  segments:  ds=%04X es=%04X\n"
      "  diagnosis: the body dispatched by this %s site had a net non-zero\n"
      "  stack effect across its setjmp/longjmp boundary. After matching\n"
      "  push/pop pairs the simulated 8086 stack pointer drifted by %+d.\n"
      "  Causes include: an unsupported instruction whose translator stub\n"
      "  skipped its push or pop; a translator emit that lost a push when\n"
      "  the matching pop ran (or vice versa); a shim with an unbalanced\n"
      "  manual sp adjustment. Inspect stack_writes.log in the bundle to\n"
      "  find the recent push/pop history around this sp range.\n",
      site, (unsigned)expected_sp, (unsigned)actual_sp, (int)delta,
      cs, ip, ss, file ? file : "?", func ? func : "?", line,
      shim_active_binary() ? shim_active_binary() : "<none>",
      (unsigned)lcall_depth, (unsigned)isr_depth, (unsigned)dispatch_depth,
      ax, bx, cx, dx, si, di, bp, ds, es, site, (int)delta);
  /* Non-fatal (see retf drift note): a callee can legitimately change sp.
   * Warn + continue instead of aborting; rate-limited, first drops a bundle. */
  static int drift_reports = 0;
  if (drift_reports < 3) {
    shim_log_crash("%s", msg);
    if (drift_reports == 0 && n > 0)
      save_bug_bundle("stack_drift", (uint32_t)actual_sp, msg);
    if (++drift_reports == 3)
      shim_log_stdout("[WARN] further stack-drift reports suppressed\n");
  }
}

static void init_shim_logs(void) __attribute__((constructor));
static void init_shim_logs(void) {
  const char *verbose = getenv("SAISEI_VERBOSE");
  if (verbose && verbose[0] != '\0') {
    shim_stdout_enabled = 1;
  }
  /* Disable buffering so traces are written immediately even if the process
   * terminates abnormally before stdio has a chance to flush. */
  setvbuf(stdout, NULL, _IONBF, 0);
  setvbuf(stderr, NULL, _IONBF, 0);
  fflush(stdout);
  fflush(stderr);
  /* Install signal handlers so that any asynchronous death (fault,
   * ctrl-c, broken pipe, our own abort()) drains stdio before exiting.
   * Without this, the final vfprintf can be cut mid-byte-stream and the
   * tail of the crash log silently disappears. */
  install_crash_handlers();
  /* atexit catches every path that doesn't go through our explicit
   * shim_flush_all_streams() (return from main, library teardown, exit()
   * called from a third-party source).  Runs in registration order
   * relative to other atexit handlers; we register first so we run last,
   * after stdio's own atexit flush. */
  atexit(shim_flush_all_streams);
}

/*
 * Trace ring buffer: keep the last TRACE_RING_LINES formatted log messages
 * in memory so we can write them out as part of a crash bundle.  Memory
 * cost: 1000 * 384 = 384 KB resident.  Per-trace cost: one vsnprintf into
 * a stack buffer + one memcpy into the ring, both cheap.
 */
#define TRACE_RING_LINES 50000
#define TRACE_RING_LINE_MAX 384
static char trace_ring[TRACE_RING_LINES][TRACE_RING_LINE_MAX];
static uint16_t trace_ring_len[TRACE_RING_LINES];
static int trace_ring_pos;     /* next slot to write */
static int trace_ring_filled;  /* total entries written, capped at TRACE_RING_LINES */

static void trace_ring_save(const char *line, size_t len) {
  if (len == 0) return;
  if (len > TRACE_RING_LINE_MAX - 1) len = TRACE_RING_LINE_MAX - 1;
  memcpy(trace_ring[trace_ring_pos], line, len);
  trace_ring[trace_ring_pos][len] = '\0';
  trace_ring_len[trace_ring_pos] = (uint16_t)len;
  trace_ring_pos = (trace_ring_pos + 1) % TRACE_RING_LINES;
  if (trace_ring_filled < TRACE_RING_LINES) ++trace_ring_filled;
}

static void trace_ring_dump(int fd) {
  int n = trace_ring_filled;
  int start = (trace_ring_pos - n + TRACE_RING_LINES) % TRACE_RING_LINES;
  for (int i = 0; i < n; ++i) {
    int idx = (start + i) % TRACE_RING_LINES;
    const char *buf = trace_ring[idx];
    size_t len = trace_ring_len[idx];
    size_t off = 0;
    while (off < len) {
      ssize_t w = write(fd, buf + off, len - off);
      if (w < 0) {
        if (errno == EINTR) continue;
        break;
      }
      off += (size_t)w;
    }
  }
}

/* Focused "lifecycle" log: chunk loads (overlay swaps) and indirect
 * dispatches (call_table, jump_table, long_jump, lcall_table,
 * near_ret_tail). Each event carries an elapsed-us timestamp.
 *
 * Always-on: events go to an in-memory ring so every crash bundle ships
 * the recent history without the user remembering to enable anything.
 * Optional: LIFECYCLE_FILE=<path> additionally streams to disk for long
 * sessions where the ring tail isn't enough. Lifecycle events are sparse
 * (handful per second vs millions for the full trace), so a few-thousand
 * entry ring carries many minutes at trivial memory cost (~1.6 MB). */
#define LIFECYCLE_RING_LINES 65536
#define LIFECYCLE_LINE_MAX   200
static char     lifecycle_ring[LIFECYCLE_RING_LINES][LIFECYCLE_LINE_MAX];
static uint16_t lifecycle_ring_len[LIFECYCLE_RING_LINES];
static int      lifecycle_ring_pos;
static int      lifecycle_ring_filled;
static char     lifecycle_fp_buf[1 << 15];  /* 32 KB stdio buffer */
static struct timespec lifecycle_start_ts;
static int      lifecycle_start_ts_set;

static uint64_t lifecycle_elapsed_us(void) {
  struct timespec now;
  clock_gettime(CLOCK_MONOTONIC, &now);
  if (!lifecycle_start_ts_set) {
    lifecycle_start_ts = now;
    lifecycle_start_ts_set = 1;
    return 0;
  }
  uint64_t s = (uint64_t)(now.tv_sec - lifecycle_start_ts.tv_sec);
  int64_t ns = (int64_t)now.tv_nsec - (int64_t)lifecycle_start_ts.tv_nsec;
  return s * 1000000ULL + (uint64_t)(ns / 1000);
}
static void lifecycle_fp_open_if_requested(void) {
  static int checked = 0;
  if (checked) return;
  checked = 1;
  const char *p = getenv("LIFECYCLE_FILE");
  if (!p || !*p) return;
  lifecycle_fp = fopen(p, "w");
  if (!lifecycle_fp) {
    fprintf(stderr, "LIFECYCLE_FILE: cannot open %s: %s\n", p, strerror(errno));
    return;
  }
  setvbuf(lifecycle_fp, lifecycle_fp_buf, _IOFBF, sizeof(lifecycle_fp_buf));
  fprintf(lifecycle_fp,
          "# Focused lifecycle log. Columns: t=<elapsed_us> <kind> <details>\n"
          "# kinds: LOAD (file mapping registered), CALL/JMP/LJMP/LCALL/NRET\n");
}
static void lifecycle_ring_save(const char *buf, size_t len) {
  if (len > LIFECYCLE_LINE_MAX - 1) len = LIFECYCLE_LINE_MAX - 1;
  memcpy(lifecycle_ring[lifecycle_ring_pos], buf, len);
  lifecycle_ring[lifecycle_ring_pos][len] = '\0';
  lifecycle_ring_len[lifecycle_ring_pos] = (uint16_t)len;
  lifecycle_ring_pos = (lifecycle_ring_pos + 1) % LIFECYCLE_RING_LINES;
  if (lifecycle_ring_filled < LIFECYCLE_RING_LINES) ++lifecycle_ring_filled;
}
static void lifecycle_log(const char *fmt, ...) {
  lifecycle_fp_open_if_requested();
  char buf[LIFECYCLE_LINE_MAX];
  uint64_t t = lifecycle_elapsed_us();
  int prefix = snprintf(buf, sizeof(buf), "t=%llu ", (unsigned long long)t);
  if (prefix < 0) prefix = 0;
  if (prefix > (int)sizeof(buf) - 1) prefix = (int)sizeof(buf) - 1;
  va_list args;
  va_start(args, fmt);
  int rest = vsnprintf(buf + prefix, sizeof(buf) - prefix, fmt, args);
  va_end(args);
  if (rest < 0) rest = 0;
  size_t total = (size_t)prefix + (size_t)rest;
  if (total >= sizeof(buf)) total = sizeof(buf) - 1;
  lifecycle_ring_save(buf, total);
  if (lifecycle_fp) {
    fwrite(buf, 1, total, lifecycle_fp);
  }
}

/* Dump the lifecycle ring tail to `dir/lifecycle.log`. Called from the
 * crash bundle writer and from the atexit snapshot. */
static void lifecycle_dump_to_dir(const char *dir) {
  char path[320];
  snprintf(path, sizeof(path), "%s/lifecycle.log", dir);
  int fd = open(path, O_WRONLY | O_CREAT | O_TRUNC | O_CLOEXEC, 0644);
  if (fd < 0) return;
  static const char header[] =
      "# Focused lifecycle log (in-memory ring tail).\n"
      "# Columns: t=<elapsed_us> <kind> <details>\n"
      "# kinds: LOAD CALL JMP LJMP LCALL NRET\n";
  ssize_t _hw = write(fd, header, sizeof(header) - 1); (void)_hw;
  int n = lifecycle_ring_filled;
  int start = (lifecycle_ring_pos - n + LIFECYCLE_RING_LINES) % LIFECYCLE_RING_LINES;
  for (int i = 0; i < n; ++i) {
    int idx = (start + i) % LIFECYCLE_RING_LINES;
    size_t len = lifecycle_ring_len[idx];
    size_t off = 0;
    while (off < len) {
      ssize_t w = write(fd, lifecycle_ring[idx] + off, len - off);
      if (w < 0) { if (errno == EINTR) continue; break; }
      off += (size_t)w;
    }
  }
  close(fd);
}

/* Resolve `addr` to "<binary>+0x<file_off>" via the live file_mapping and
 * log a one-line dispatch event. Defined further down once FileMapping +
 * find_file_mapping are in scope; declared here so call-sites can use it. */
static void lifecycle_log_dispatch(const char *kind, uint32_t addr);

/* Function alias registry (reconstruction naming layer): map a function's
 * stable "<binary>+0x<off>" identity to a user-assigned alias, seeding new
 * ones on first sight when `seed` is set. Defined far below; declared here so
 * lifecycle_log_dispatch can render aliases into the flow log. */
static const char *aliasreg_alias(const char *id, int seed);

/* Render an alias that may carry an argument spec ("name(reg:argname@enum,...)")
 * into "name(argname=LABEL|0xval, ...)" by reading the call-time registers and
 * the enums.json tables. A plain "name" passes through unchanged. Defined far
 * below; declared here so lifecycle_log_dispatch can use it. */
static const char *render_alias_with_args(const char *alias, char *out, size_t cap);

/* Persistent call-graph: record a unique call edge (caller call-site -> callee).
 * Defined far below; declared here so lifecycle_log_dispatch can call it. */
static void cg_record(uint32_t caller_linear, uint32_t callee_linear);

/* sp is `#define sp (cpu.sp)` via cpu_state.h; isr_depth + lcall_depth are
 * externed in shims.h. No additional declarations needed here. */

/* ===== Per-binary case-key sets (save_manager IP validation) =====
 *
 * Loaded lazily from case_keys/<module>.bin (uint32 count + sorted uint32
 * keys). The translated near-ret handler no longer consults this — the
 * dispatch switch itself is the case-key set at runtime. The sidecar
 * survives because save_manager uses shim_pc_is_case_key to refuse
 * captures whose cs:ip lies in the middle of a translated case body
 * (those would land at no-case on restore and trip unhandled_pc).
 */
#define CK_MAX_BINARIES 16
typedef struct {
  char       name[16];
  uint32_t  *keys;       /* sorted file_offsets */
  uint32_t   count;
  int        loaded;     /* 1 = load attempted (success or known-miss) */
} ShimCaseKeys;
static ShimCaseKeys shim_ck_sets[CK_MAX_BINARIES];
static int          shim_ck_count;

static ShimCaseKeys *shim_ck_find_or_load(const char *module) {
  if (!module) return NULL;
  for (int i = 0; i < shim_ck_count; ++i) {
    if (strcmp(shim_ck_sets[i].name, module) == 0) {
      return &shim_ck_sets[i];
    }
  }
  if (shim_ck_count >= CK_MAX_BINARIES) return NULL;
  ShimCaseKeys *s = &shim_ck_sets[shim_ck_count++];
  strncpy(s->name, module, sizeof(s->name) - 1);
  s->name[sizeof(s->name) - 1] = '\0';
  s->loaded = 1;
  char path[256];
  snprintf(path, sizeof(path), "case_keys/%s.bin", module);
  FILE *fp = fopen(path, "rb");
  if (!fp) return s;
  uint32_t cnt;
  if (fread(&cnt, sizeof(cnt), 1, fp) != 1 || cnt == 0 || cnt > 0x100000) {
    fclose(fp); return s;
  }
  uint32_t *keys = (uint32_t *)malloc((size_t)cnt * sizeof(uint32_t));
  if (!keys) { fclose(fp); return s; }
  if (fread(keys, sizeof(uint32_t), cnt, fp) != cnt) {
    free(keys); fclose(fp); return s;
  }
  fclose(fp);
  s->keys = keys;
  s->count = cnt;
  return s;
}

/* Binary-search lookup. Returns 1 if `file_off` is a case key in `module`'s
 * dispatch switch. Used by the translated near-ret handler to decide
 * between in-loop dispatch and fallback near_ret_tail. */
int shim_pc_is_case_key(const char *module, uint32_t file_off) {
  ShimCaseKeys *s = shim_ck_find_or_load(module);
  if (!s || !s->keys || s->count == 0) return 0;
  uint32_t lo = 0, hi = s->count;
  while (lo < hi) {
    uint32_t mid = lo + (hi - lo) / 2;
    if (s->keys[mid] < file_off) lo = mid + 1;
    else hi = mid;
  }
  return (lo < s->count && s->keys[lo] == file_off);
}

/* Optional persistent trace file. Enabled via TRACE_FILE=<path>. When set,
 * trace lines go to that file via a large stdio buffer (much faster than
 * per-line write() syscalls) and the per-line stdout write is skipped to
 * avoid duplicate I/O. The crash signal handler flushes this file before
 * re-raising so a fault doesn't lose the last buffered block. */
static char trace_file_buf[1 << 18];  /* 256 KB buffer, batches ~700 lines */
static void trace_file_open_if_requested(void) {
  static int checked = 0;
  if (checked) return;
  checked = 1;
  const char *p = getenv("TRACE_FILE");
  if (!p || !*p) return;
  trace_file_fp = fopen(p, "w");
  if (!trace_file_fp) {
    fprintf(stderr, "TRACE_FILE: cannot open %s: %s\n", p, strerror(errno));
    return;
  }
  /* Fully buffered with our own large buffer — minimises syscall count. */
  setvbuf(trace_file_fp, trace_file_buf, _IOFBF, sizeof(trace_file_buf));
}

static void shim_log_stdout_impl(FILE *stream, const char *fmt, va_list args) {
  /* Format once into a stack buffer, then save to ring + emit. Falls back to
   * plain vfprintf if formatting overflows the buffer — we'd rather print a
   * truncated trace line than drop it. */
  char buf[TRACE_RING_LINE_MAX];
  va_list args_copy;
  va_copy(args_copy, args);
  int n = vsnprintf(buf, sizeof(buf), fmt, args_copy);
  va_end(args_copy);
  if (n < 0) {
    vfprintf(stream, fmt, args);
    return;
  }
  size_t emit = (size_t)(n < (int)sizeof(buf) ? n : (int)sizeof(buf) - 1);
  trace_ring_save(buf, emit);
  trace_file_open_if_requested();
  if (trace_file_fp) {
    /* TRACE_FILE is the canonical sink — write only there, skip stdout to
     * save the per-line write() syscall on the terminal path. */
    fwrite(buf, 1, emit, trace_file_fp);
    return;
  }
  /* No TRACE_FILE — write to terminal stream (stdout is _IONBF, so this is
   * a direct write syscall per line; that's the legacy fast-feedback mode). */
  fwrite(buf, 1, emit, stream);
}

void shim_set_stdout_logging_enabled(int enabled) {
  shim_stdout_enabled = enabled ? 1 : 0;
}

void shim_enable_stdout_logging(void) { shim_set_stdout_logging_enabled(1); }

void shim_disable_stdout_logging(void) { shim_set_stdout_logging_enabled(0); }

void shim_log_stdout(const char *fmt, ...) {
  if (!shim_stdout_enabled) {
    return;
  }

  va_list args;
  va_start(args, fmt);
  shim_log_stdout_impl(stdout, fmt, args);
  va_end(args);

  /* If stdout is unavailable (for example, the DOS program closed it),
   * preserve tracing by mirroring the message to stderr. */
  if (ferror(stdout)) {
    clearerr(stdout);
    va_start(args, fmt);
    shim_log_stdout_impl(stderr, fmt, args);
    va_end(args);
  }
}

void shim_log_crash(const char *fmt, ...) {
  /* Crash diagnostics are written to stdout so they land in the same
   * stream as the traces above; a single `> debug.txt` redirect captures
   * the whole story.  We bypass the verbosity gate that suppresses
   * normal traces because a crash is always worth reporting.  ferror
   * fallback to stderr handles the case where the program closed stdout. */
  va_list args;
  va_start(args, fmt);
  shim_log_stdout_impl(stdout, fmt, args);
  va_end(args);
  if (ferror(stdout)) {
    clearerr(stdout);
    va_start(args, fmt);
    shim_log_stdout_impl(stderr, fmt, args);
    va_end(args);
  }
}

void shim_log_stderr(const char *fmt, ...) {
  va_list args;
  va_start(args, fmt);
  vfprintf(stderr, fmt, args);
  va_end(args);
  fflush(stderr);
}

void shim_exit_with_message(const char *fmt, ...) {
  va_list args;
  va_start(args, fmt);
  vfprintf(stderr, fmt, args);
  va_end(args);
  shim_flush_all_streams();
  exit(1);
}

static uint8_t to_bcd(uint8_t value) {
  return (uint8_t)(((value / 10) << 4) | (value % 10));
}

static void set_iret_carry(int set) {
  uint16_t flags_off = (uint16_t)((sp + 4) & 0xFFFF);
  uint16_t ret_flags = memw(ss, flags_off);
  if (set) {
    ret_flags |= 1u;
  } else {
    ret_flags &= (uint16_t)~1u;
  }
  memw_write(ss, flags_off, ret_flags);
  CF = (uint8_t)(set != 0);
}

static void io_port_error(const char *func, uint16_t port) {
  shim_log_stderr("Error: %s called with unsupported port 0x%04X\n", func,
                  port);
  shim_flush_all_streams();
  exit(1);
}

#ifdef FORCE_EXIT_AFTER_10S
static void force_exit_handler(int signum) {
  (void)signum;
  shim_log_stderr("Error: Execution exceeded 10 seconds. Forcing exit.\n");

  shim_flush_all_streams();
  exit(1);
}

static void setup_force_exit(void) {
  signal(SIGALRM, force_exit_handler);
  alarm(10);
}
#endif

/*
 * Provide global instances for the CPU register state and a handful of
 * utility structures used by the translated binaries.  Previously these were
 * defined in a separate compilation unit, but linking the generated code
 * without it caused unresolved symbol errors (e.g. `cpu`, `exec_params`,
 * `rcb`).  Defining them here keeps the build command simple:
 *
 *     gcc artifacts/program.c scripts/shims.c -Iscripts/include -o
 * artifacts/program
 *
 * which now succeeds without needing any additional source files.
 */
CPUState cpu;
ExecParamBlock exec_params;

/*
 * Simple flat memory model that reserves space for the program image.  The
 * translated code uses the ``memb``/``memw`` helpers which expect segment
 * values to be translated to linear addresses via ``seg_off``.  We keep a
 * contiguous block of virtual memory and have those macros index into it.
 */

#define ENV_SEG ((uint16_t)(PSP_SEG - 0x10))
#define LOAD_SEG ((uint16_t)(PSP_SEG + 0x10))
#define MEMORY_MASK (MEMORY_SIZE - 1)

/* Runtime PSP segment. Default mirrors the historical 0x1000 layout; the
 * per-game config can lower it (giving the program more conventional RAM) when
 * the game requires it, matching how a real minimal-DOS machine loaded that
 * title. Set from game_config in init_memory before any PSP/load use. */
uint16_t psp_seg = DEFAULT_PSP_SEG;

uint8_t *virtual_memory;
const size_t SHIM_MEMORY_SIZE = (size_t)MEMORY_SIZE;
bool a20_enabled;
static PSP *psp;
static uint8_t *image_base;
static uint8_t *env_block;
void *dta_ptr;
uint16_t next_free_seg;
uint16_t program_min_block_paras;
uint8_t null_guard_initial[16];
static int screenshot_counter = 1;
/* Auto-screenshot interval (seconds). 0 = on-demand only via the stdin Ctrl+T
 * (\x14) trigger -- avoids cluttering screenshots/ on every headless run.
 * Override with SAISEI_SCREENSHOT_SECS=N for unattended headless validation
 * (e.g. confirming a JIT-only build renders). */
static int SCREENSHOT_INTERVAL_SECS = 0;
static uint64_t last_present_time_ns;
static uint64_t last_screenshot_time_ns;
int headless_mode;
double emulation_speedup = 1.0;
uint64_t host_time_origin_ns;
int virtual_display_buffer = 0;
int current_display_width = 320;
int current_display_height = 200;

static struct termios orig_termios;
static int keyboard_fd = -1;
static int keyboard_initialized;
static int keyboard_input_enabled;
static int keyboard_blocking_enabled;

#define STD_HANDLE_COUNT 5
FILE *handles[MAX_DOS_HANDLES];
char *handle_paths[MAX_DOS_HANDLES];
bool handle_paths_owned[MAX_DOS_HANDLES];

static const char *const std_handle_names[STD_HANDLE_COUNT] = {
    "<stdin>", "<stdout>", "<stderr>", "<stdprn>", "<stdaux>"};

int is_standard_handle(uint16_t handle) {
  return handle < STD_HANDLE_COUNT;
}

static void init_standard_handles(void) {
  handles[0] = stdin;
  handles[1] = stdout;
  handles[2] = stderr;
  handles[3] = stdout;
  handles[4] = stdout;

  for (int i = 0; i < STD_HANDLE_COUNT; ++i) {
    handle_paths[i] = (char *)std_handle_names[i];
    handle_paths_owned[i] = false;
  }
}

typedef struct {
  char *path;
  uint32_t base;
  size_t len;
  size_t file_offset;
  uint8_t *data;
  /* Canonical CS for code in this mapping.  Populated lazily by
   * lcall_table_impl / long_jump_impl when an authoritative seg:off
   * transfer first lands in this mapping. Used by dispatch_via_binary to
   * set cpu.r_cs correctly when routing near-transfers (near_ret_tail,
   * jump_table) into the binary's translated code, so the callee's
   * ``cs:[disp]`` references resolve against its own segment instead of
   * whatever cs happened to be left over from a previous binary. */
  uint16_t canonical_cs;
  /* cs:ip of the game-side instruction that triggered this LOAD. Captured
   * from the CPU at register_file_mapping time. The lifecycle ring loses
   * the LOAD event after ~65k entries; this field preserves the trigger
   * for every chunk indefinitely, so a post-mortem can answer "which
   * game function decided to load chunk N here?" even for early loads. */
  uint16_t loader_cs;
  uint16_t loader_ip;
  /* Top 8 words of the simulated stack at LOAD time — this is the chain
   * of return IPs the loader will eventually unwind through, i.e. the
   * call stack that drove us into the loader. Lets us answer "which
   * higher-level game function asked the loader to load chunk N" even
   * when the loader itself is the same generic DOS-read shim for every
   * chunk. ss/sp at LOAD time included so a future tool can re-walk it. */
  uint16_t loader_ss;
  uint16_t loader_sp;
  uint16_t loader_stack[8];
} FileMapping;

#define MAX_FILE_MAPPINGS 1024
static FileMapping file_mappings[MAX_FILE_MAPPINGS];
static size_t file_mapping_count;
uint8_t last_int_no;

/*
 * Default interrupt handler stub.  Many DOS programs query existing vectors
 * and restore them later.  Our simulated interrupt table starts empty, which
 * meant those queries would read {0,0} and eventually reinstall a null
 * handler.  When the timer fired, the shim attempted to long jump to 0:0 and
 * crashed.  Initialize all vectors to a stub that simply returns via ``iret``.
 */
#define DEFAULT_ISR_LINEAR 0x000F0000
#define DEFAULT_ISR_SEG (DEFAULT_ISR_LINEAR >> 4)
#define DEFAULT_ISR_OFF (DEFAULT_ISR_LINEAR & 0xF)

#define BIOS_VIDEO_ISR_LINEAR 0x000F0100
#define BIOS_VIDEO_ISR_SEG (BIOS_VIDEO_ISR_LINEAR >> 4)
#define BIOS_VIDEO_ISR_OFF (BIOS_VIDEO_ISR_LINEAR & 0xF)

static const uint8_t bios_video_parameter_table_mode6[] = {0x00, 0x00, 0x02, 0x00,
                                                         0x00, 0x00};

#define BIOS_KBD_ISR_LINEAR 0x000F0200
#define BIOS_KBD_ISR_SEG (BIOS_KBD_ISR_LINEAR >> 4)
#define BIOS_KBD_ISR_OFF (BIOS_KBD_ISR_LINEAR & 0xF)

#define DOS_TERM_ISR_LINEAR 0x000F0300
#define DOS_TERM_ISR_SEG (DOS_TERM_ISR_LINEAR >> 4)
#define DOS_TERM_ISR_OFF (DOS_TERM_ISR_LINEAR & 0xF)

#define DOS_API_ISR_LINEAR 0x000F0400
#define DOS_API_ISR_SEG (DOS_API_ISR_LINEAR >> 4)
#define DOS_API_ISR_OFF (DOS_API_ISR_LINEAR & 0xF)

#define BIOS_TIMER_ISR_LINEAR 0x000F0500
#define BIOS_TIMER_ISR_SEG (BIOS_TIMER_ISR_LINEAR >> 4)
#define BIOS_TIMER_ISR_OFF (BIOS_TIMER_ISR_LINEAR & 0xF)

#define BIOS_IRQ0_ISR_LINEAR 0x000F0600
#define BIOS_IRQ0_ISR_SEG (BIOS_IRQ0_ISR_LINEAR >> 4)
#define BIOS_IRQ0_ISR_OFF (BIOS_IRQ0_ISR_LINEAR & 0xF)

#define BIOS_IRQ1_ISR_LINEAR 0x000F0900
#define BIOS_IRQ1_ISR_SEG (BIOS_IRQ1_ISR_LINEAR >> 4)
#define BIOS_IRQ1_ISR_OFF (BIOS_IRQ1_ISR_LINEAR & 0xF)

#define BIOS_EQUIPMENT_ISR_LINEAR 0x000F0700
#define BIOS_EQUIPMENT_ISR_SEG (BIOS_EQUIPMENT_ISR_LINEAR >> 4)
#define BIOS_EQUIPMENT_ISR_OFF (BIOS_EQUIPMENT_ISR_LINEAR & 0xF)

#define BIOS_TIMER_TICK_ISR_LINEAR 0x000F0800
#define BIOS_TIMER_TICK_ISR_SEG (BIOS_TIMER_TICK_ISR_LINEAR >> 4)
#define BIOS_TIMER_TICK_ISR_OFF (BIOS_TIMER_TICK_ISR_LINEAR & 0xF)

#define MOUSE_ISR_LINEAR 0x000F0A00
#define MOUSE_ISR_SEG (MOUSE_ISR_LINEAR >> 4)
#define MOUSE_ISR_OFF (MOUSE_ISR_LINEAR & 0xF)

#define BIOS_EQUIPMENT_WORD                                                   \
  0x0063 /* Bit 1 indicates the system has an 8087 math coprocessor installed */

/* Forward declaration for iret_impl so the stub can invoke it. */
void iret_impl(const char *file, const char *func, int line);

static void default_isr_impl(uint16_t expected_retip, const char *file, const char *func, int line) {
  (void)expected_retip;
  char msg[256];
  snprintf(msg, sizeof(msg), "unhandled interrupt 0x%02X (%s:%s:%d)",
           last_int_no, file, func, line);
  shim_log_crash("%s\n", msg);
  save_bug_bundle("unhandled_interrupt", ((uint32_t)cs << 4) + ip, msg);
  shim_flush_all_streams();
  abort();
}



uint8_t isr_depth;
uint8_t critical_depth;
uint8_t interrupt_shadow;
uint8_t irq0_pending;
uint64_t bios_tick_cycle_debt; /* PIT cycles accrued toward the next 18.2 Hz tick */
uint8_t irq_pending[256];
uint64_t last_host_time_ns;
static jmp_buf irq_return_env[256];
static uint16_t isr_expected_sp[256];
uint8_t lcall_depth;
static uint16_t lcall_expected_sp[256];
static uint16_t lcall_expected_ss[256];
/* Saved far-return (caller cs:ip) per lcall depth, so a callee that faults
 * (tail-dispatch dead-ends in undecodable/garbage memory) can be CONTAINED:
 * return from the lcall instead of aborting the whole program. */
static uint16_t lcall_ret_ip[256];
static uint16_t lcall_ret_cs[256];
static jmp_buf lcall_return_env[256];
/* Byte count the matching retf popped via `retf imm16` (callee argument
 * cleanup, Pascal/stdcall far calls). Set by retf_common_impl immediately
 * before its longjmp; read by lcall_table_impl right after the longjmp resume
 * so the stack-balance check accounts for args the callee removed. */
static uint16_t last_retf_pop_bytes;

/* Counts nested call_table / dispatch_via_binary / overlay-dispatch calls.
 * Tracked for diagnostics; the load-bearing save/restore correctness gate
 * is now active_binary_stack below. */
uint16_t dispatch_depth;
/* Set on first kbd scancode consumption; gates the cross-binary-write
 * tripwire so boot-time legitimate cross-binary writes (the main program →
 * music-driver tables, an overlay module → game setup) don't false-fire. Symptom of the
 * tripwire's target — buffer-overrun corruption from gameplay — only
 * happens post-input by construction. */
int shim_input_phase_started;
/* Per-site ++/-- counters to identify which dispatch_depth manipulation
 * path is leaking. Dumped at crash time. The leak shows as a per-site
 * imbalance of inc - dec; expected to be 0 if all ++s are paired. */
uint64_t dd_inc_call_table, dd_dec_call_table;
uint64_t dd_inc_via_binary, dd_dec_via_binary;
uint64_t dd_inc_overlay_first, dd_dec_overlay_first;

/* Stack of currently-executing binaries. Each translated _impl wrapper
 * pushes its binary's basename on entry and pops on exit (see
 * the source emission). save_manager refuses captures whenever this is
 * empty (we're outside any game-binary execution — shim setup, idle
 * boot, etc.). snapshot saves the top of the stack so restore can route
 * back into the right binary's dispatch instead of relying on cs:ip
 * arithmetic, which doesn't round-trip through file_mappings to the
 * active binary when ip is set to a file_offset value.
 *
 * The stack lives only at runtime — there's no need to save/restore the
 * whole stack, only the top element (the binary currently executing). */
#define SHIM_ACTIVE_BINARY_MAX 64
static const char *active_binary_stack[SHIM_ACTIVE_BINARY_MAX];
static int active_binary_top;

void shim_enter_binary(const char *name) {
  if (active_binary_top < SHIM_ACTIVE_BINARY_MAX) {
    active_binary_stack[active_binary_top] = name;
  }
  ++active_binary_top;
}

void shim_leave_binary(void) {
  if (active_binary_top > 0) --active_binary_top;
}

const char *shim_active_binary(void) {
  if (active_binary_top <= 0) return NULL;
  int top = active_binary_top - 1;
  if (top >= SHIM_ACTIVE_BINARY_MAX) top = SHIM_ACTIVE_BINARY_MAX - 1;
  return active_binary_stack[top];
}

ShimDispatchFn shim_dispatch_fn_by_module(const char *module) {
  if (!module || !game_config.binary_dispatch) return NULL;
  for (size_t i = 0; i < game_config.binary_dispatch_count; ++i) {
    const BinaryDispatch *bd = &game_config.binary_dispatch[i];
    if (bd->module && bd->fn && strcmp(bd->module, module) == 0) {
      return (ShimDispatchFn)bd->fn;
    }
  }
  return NULL;
}

/* shim_unhandled_pc_report is defined further down — see comment near
 * find_file_mapping. It depends on the FileMapping array which is
 * declared after this point. */

static const char *critical_owner_name;
static const char *critical_owner_file;
static const char *critical_owner_func;
static int critical_owner_line;
/* Critical sections nest legitimately: a critical operation can call a critical
 * sub-operation (e.g. INT 21h AH=4Bh EXEC runs inside dos_api's section and
 * then calls load_executable, which guards itself too). Track owners on a stack
 * so the exit ownership check pairs by nesting level; the depth is still capped
 * to catch genuine runaway recursion. */
#define CRITICAL_MAX_DEPTH 16
static const char *critical_owner_name_stk[CRITICAL_MAX_DEPTH];

uint64_t shim_host_monotonic_ns(void) {
  struct timespec ts;
  clock_gettime(CLOCK_MONOTONIC, &ts);
  return (uint64_t)ts.tv_sec * 1000000000ull + (uint64_t)ts.tv_nsec;
}

/* === VIRTUAL CLOCK ===
 *
 * The wall clock always advances; the *virtual* clock is what the game
 * perceives. Everything that affects game behavior (PIT catchup, tap
 * release deadlines, frame pacing, in-game time-of-day reads) reads from
 * shim_virtual_now_ns() rather than shim_host_monotonic_ns(). The wall
 * clock is reserved for logging, where real-time timestamps matter.
 *
 * States:
 *   RUNNING  — virtual time tracks wall time (offset constant). Identical
 *              to the pre-virtual-clock world; baseline.
 *   HALTED   — virtual time frozen at vclock_frozen_virtual_ns. No PIT
 *              ticks delivered, no IRQ0, no in-game animation. The
 *              emulator main loop keeps spinning but the game is paused.
 *   STEPPING — virtual time advances at wall rate, but clamped at
 *              vclock_step_deadline_virtual_ns. When it reaches the
 *              deadline, the next vclock_service() call transitions to
 *              HALTED with virtual frozen at the deadline. This delivers
 *              an exact, deterministic amount of game time per request.
 *
 * Driven from safe_point_impl, which is single-threaded with the rest of
 * the emulator — no locking needed. */

/* Defined alongside shim_save_video_memory below. Forward-declared here so
 * the stdin opcode handlers in safe_point_impl can reach them. */
static void shim_dump_ram_snapshot(void);
static void shim_read_memory_to_sidecar(uint32_t addr, uint8_t len);
/* Faithful flat machine helpers, defined later but used by invoke_isr. */
static int resolve_and_run_chunk(uint32_t addr);
static void record_binary_cs(uint32_t addr, uint16_t seg);

/* === SESSION LOG ===
 *
 * Every byte received from the stdin FIFO is written to a session log
 * (`sessions/session.log` in cwd, overwritten per launch) with the
 * virtual_ns at read time. The log is the input track of the run; paired
 * with a known starting state (fresh launch + matching --speedup) it is
 * sufficient to deterministically replay the run via scripts/the source.
 *
 * On crash/bug bundle, the session log is copied into the bundle dir
 * via the bundle_extra_writer hook so the inputs are preserved with
 * the rest of the state.
 *
 * Format (text, line-oriented for grep-ability):
 *   # session log, speedup=1
 *   vns=12345  bytes=12 4D 05 00
 *   vns=34567  bytes=17 05 00
 *   ... */
static FILE *session_log_fp;
static char  session_log_path[256];

static void session_log_init(void) {
  if (session_log_fp) return;
  const char *dir = "sessions";
  if (mkdir(dir, 0755) && errno != EEXIST) {
    shim_log_stdout("[SESSION] mkdir sessions: %s\n", strerror(errno));
    return;
  }
  snprintf(session_log_path, sizeof(session_log_path), "%s/session.log", dir);
  session_log_fp = fopen(session_log_path, "w");
  if (!session_log_fp) {
    shim_log_stdout("[SESSION] fopen %s: %s\n", session_log_path, strerror(errno));
    return;
  }
  setvbuf(session_log_fp, NULL, _IOLBF, 0);
  fprintf(session_log_fp, "# session log, speedup=%g\n", emulation_speedup);
  shim_log_stdout("[SESSION] logging stdin to %s\n", session_log_path);
}

static void session_log_bytes(const uint8_t *buf, size_t n) {
  if (!session_log_fp) session_log_init();
  if (!session_log_fp || n == 0) return;
  uint64_t vns = shim_virtual_now_ns();
  fprintf(session_log_fp, "vns=%llu  bytes=", (unsigned long long)vns);
  for (size_t i = 0; i < n; i++) {
    fprintf(session_log_fp, "%02X%s", buf[i], (i + 1 < n) ? " " : "");
  }
  fputc('\n', session_log_fp);
}

/* Drop-in replacement for read(keyboard_fd, ...) that records each
 * successfully-read chunk into the session log. All keyboard_fd reads in
 * safe_point_impl should go through this wrapper. */
static ssize_t session_logged_read(void *buf, size_t n) {
  ssize_t r = read(keyboard_fd, buf, n);
  if (r > 0) session_log_bytes((const uint8_t *)buf, (size_t)r);
  return r;
}

/* Bundle extra writer: copy the session log into the crash bundle dir so
 * the inputs that led to the crash are preserved alongside the state. */
static void session_log_write_to_bundle(const char *dir) {
  if (!session_log_fp) return;
  fflush(session_log_fp);
  FILE *src = fopen(session_log_path, "r");
  if (!src) return;
  char path[512];
  snprintf(path, sizeof(path), "%s/session.log", dir);
  FILE *dst = fopen(path, "w");
  if (!dst) { fclose(src); return; }
  char buf[4096];
  size_t r;
  while ((r = fread(buf, 1, sizeof(buf), src)) > 0) {
    fwrite(buf, 1, r, dst);
  }
  fclose(src);
  fclose(dst);
}



InterruptSnapshot last_sw_interrupt;

static void critical_section_abort(const char *reason, const char *attempt_name,
                                   const char *attempt_file,
                                   const char *attempt_func, int attempt_line) {
  shim_log_stderr("Error: %s by %s (%s:%s:%d)\n", reason, attempt_name,
                  attempt_file, attempt_func, attempt_line);
  if (critical_owner_name) {
    shim_log_stderr("       Active critical section owned by %s (%s:%s:%d)\n",
                    critical_owner_name, critical_owner_file,
                    critical_owner_func, critical_owner_line);
  } else {
    shim_log_stderr("       No active critical section owner recorded\n");
  }
  shim_flush_all_streams();
  exit(1);
}

void critical_section_enter(const char *name, const char *file,
                                   const char *func, int line) {
  if (critical_depth >= CRITICAL_MAX_DEPTH) {
    critical_section_abort("critical section nested too deep (runaway recursion)",
                           name, file, func, line);
  }
  critical_owner_name_stk[critical_depth] = name;
  critical_owner_name = name; /* top of stack, for the abort diagnostic */
  critical_owner_file = file;
  critical_owner_func = func;
  critical_owner_line = line;
  ++critical_depth;
}

void critical_section_exit(const char *name, const char *file,
                                  const char *func, int line) {
  if (critical_depth == 0) {
    critical_section_abort("critical section exit without matching entry", name,
                           file, func, line);
  }
  --critical_depth;
  /* Pair the exit with the entry at this nesting level (LIFO). */
  const char *expected = critical_owner_name_stk[critical_depth];
  if (expected && strcmp(expected, name) != 0) {
    critical_section_abort("critical section ownership mismatch on exit", name,
                           file, func, line);
  }
  if (critical_depth > 0) {
    critical_owner_name = critical_owner_name_stk[critical_depth - 1];
  } else {
    critical_owner_name = NULL;
    critical_owner_file = NULL;
    critical_owner_func = NULL;
    critical_owner_line = 0;
  }
}

/* DOS character I/O is interruptible. On real hardware INT 21h AH=01/06/07/08/
 * 0Ah execute STI and then spin waiting for a key; the keyboard IRQ (IRQ1)
 * fires NESTED inside the DOS call, runs INT 09h, and deposits the keystroke
 * into the BIOS type-ahead buffer the input service then reads. Our model
 * suppresses interrupt delivery while the dispatch re-entrancy guard is held
 * (dos_api wraps its whole switch in CRITICAL_ENTER -> critical_depth=1) and
 * while the caller left IF=0, so IRQ1 never fires, the buffer never fills, and
 * the wait deadlocks. A blocking DOS input primitive brackets its SAFEPOINT
 * wait with these: lift the suppression (as DOS's STI does) so the keyboard/
 * timer IRQs deliver during the wait, then restore the prior state on the way
 * out -- mirroring the caller's flags being restored by the closing IRET. */
void shim_dos_input_wait_begin(uint8_t *saved_crit, uint8_t *saved_if) {
  *saved_crit = critical_depth;
  *saved_if = IF;
  critical_depth = 0;
  IF = 1;
}

void shim_dos_input_wait_end(uint8_t saved_crit, uint8_t saved_if) {
  critical_depth = saved_crit;
  IF = saved_if;
}


uint8_t ascii_to_scan(uint8_t c) {
  if (c >= 'a' && c <= 'z') {
    static const uint8_t map[] = {0x1E, 0x30, 0x2E, 0x20, 0x12, 0x21, 0x22,
                                  0x23, 0x17, 0x24, 0x25, 0x26, 0x32, 0x31,
                                  0x18, 0x19, 0x10, 0x13, 0x1F, 0x14, 0x16,
                                  0x2F, 0x11, 0x2D, 0x15, 0x2C};
    return map[c - 'a'];
  }
  if (c >= 'A' && c <= 'Z') {
    static const uint8_t map[] = {0x1E, 0x30, 0x2E, 0x20, 0x12, 0x21, 0x22,
                                  0x23, 0x17, 0x24, 0x25, 0x26, 0x32, 0x31,
                                  0x18, 0x19, 0x10, 0x13, 0x1F, 0x14, 0x16,
                                  0x2F, 0x11, 0x2D, 0x15, 0x2C};
    return map[c - 'A'];
  }
  if (c >= '0' && c <= '9') {
    static const uint8_t map[] = {0x0B, 0x02, 0x03, 0x04, 0x05,
                                  0x06, 0x07, 0x08, 0x09, 0x0A};
    return map[c - '0'];
  }
  switch (c) {
  case '\r':
  case '\n':
    return 0x1C;
  case 27:
    return 0x01;
  case ' ':
    return 0x39;
  case 0x08:
    return 0x0E; /* backspace */
  case '\t':
    return 0x0F;
  default:
    return 0;
  }
}


/* Wall-clock-scheduled key releases for deterministic walking. Set by
 * the stdin "tap" opcode (0x12). pending_release_deadline_ns[sc] =
 * virtual-clock ns at which the matching break scancode should be
 * enqueued (0 = no pending release).
 *
 * Why virtual-clock and not raw tick count: the BIOS IRQ0 dispatch path
 * collapses catchup PIT cycles aggressively — many "ticks" can fire
 * across microseconds when the emulator catches up after a stutter. An
 * earlier version paced N ticks at the IRQ0 dispatch site and walked
 * nothing because the release fired in 4.5ms instead of 5.5s.
 *
 * Virtual-time pacing keeps the press held for a deterministic amount
 * of *game* time, which is what determines how much the game's main
 * loop runs between press and release. While RUNNING, virtual tracks
 * wall, so behavior matches the old wall-clock implementation. While
 * HALTED, virtual is frozen and the deadline can't fire. Speedup is
 * factored into ns_per_tick (a 2.0-speedup hold lasts half the wall
 * time for the same TICKS). */
static uint64_t pending_release_deadline_ns[128];

static void pending_release_tick(void) {
  uint64_t now = shim_virtual_now_ns();
  for (int i = 1; i < 128; ++i) {
    if (pending_release_deadline_ns[i] && now >= pending_release_deadline_ns[i]) {
      pending_release_deadline_ns[i] = 0;
      shim_keyboard_enqueue_scancode_release((uint8_t)i);
      shim_log_stdout("[TAP] release sc=0x%02X fired virtual_ns=%llu\n",
              i, (unsigned long long)now);
    }
  }
}

static void init_virtual_display(void) {
  // start with Mode 13h logical size (320x200). Scale 3x by default.
  virtual_display_init(320, 200, 3);
  current_display_width = 320;
  current_display_height = 200;
}

static void quit_virtual_display(void) { virtual_display_shutdown(); }

static void init_keyboard(void) __attribute__((constructor));
static void cleanup_keyboard(void) __attribute__((destructor));

static void init_keyboard(void) {
  /* SAISEI_REPLAY mode — start with the virtual clock halted at 0 so
   * the game does not free-run during launch. Combined with a replay
   * driver that steps the recorded virtual_ns deltas before each input
   * write, this gives deterministic baseline state across runs. Without
   * this, the seconds of wall-clock between launch and the first
   * replay byte give a different vclock value every run. */
  if (getenv("SAISEI_REPLAY")) {
    vclock_state = VCLOCK_HALTED;
    vclock_frozen_virtual_ns = 0;
    shim_log_stdout("[VCLOCK] SAISEI_REPLAY: initial halt at virtual_ns=0\n");
  }
  keyboard_fd = STDIN_FILENO;
  if (tcgetattr(keyboard_fd, &orig_termios) != 0) {
    int tty = open("/dev/tty", O_RDONLY);
    if (tty >= 0 && tcgetattr(tty, &orig_termios) == 0) {
      keyboard_fd = tty;
    } else {
      if (tty >= 0) {
        close(tty);
      }
      /*
       * In headless runs stdin is often a pipe (no TTY), which makes tcgetattr
       * fail. Keep stdin as a non-blocking key source anyway so callers can
       * drive the game loop by piping bytes into the process.
       */
      keyboard_fd = STDIN_FILENO;
      if (fcntl(keyboard_fd, F_SETFL, O_NONBLOCK) == 0) {
        keyboard_input_enabled = 1;
      }
      keyboard_blocking_enabled = 0;
      return;
    }
  }
  struct termios raw = orig_termios;
  cfmakeraw(&raw);
  // Preserve original output processing to keep console output working.
  raw.c_oflag = orig_termios.c_oflag;
  tcsetattr(keyboard_fd, TCSANOW, &raw);
  fcntl(keyboard_fd, F_SETFL, O_NONBLOCK);
  keyboard_input_enabled = 1;
  keyboard_blocking_enabled = 1;
  keyboard_initialized = 1;
}

static void cleanup_keyboard(void) {
  if (keyboard_initialized) {
    tcsetattr(keyboard_fd, TCSANOW, &orig_termios);
    if (keyboard_fd != STDIN_FILENO) {
      close(keyboard_fd);
    }
  }
}

void shim_set_timer_isr(uint16_t segment, uint16_t offset) {
  memw_raw_write(0, 0x08 * 4, offset);
  memw_raw_write(0, 0x08 * 4 + 2, segment);
}

void schedule_interrupt_impl(uint8_t int_no, const char *file, const char *func,
                             int line) {
  shim_log(__func__, file, func, line, NULL);
  irq_pending[int_no] = 1;
}

void schedule_interrupt(uint8_t int_no) {
  schedule_interrupt_impl(int_no, "<external>", __func__, 0);
}

/* Synchronously call a real-mode FAR procedure in game code (e.g. an INT 33h
 * mouse event handler) from the host side, returning once it RETFs. Models
 * invoke_isr's sp-based nested run, but pushes only a `call far` return frame
 * (cs:ip): the callee ends with RETF, whose net stack effect is zero, so the
 * loop terminates exactly when sp climbs back to its pre-call value. The
 * callback is asynchronous to the interrupted game code, so the full CPU state
 * is saved and restored around it. */
void shim_invoke_far_call(uint16_t seg, uint16_t off, uint16_t r_ax,
                          uint16_t r_bx, uint16_t r_cx, uint16_t r_dx,
                          uint16_t r_si, uint16_t r_di) {
  uint16_t s_cs = cs, s_ip = ip, s_ss = ss, s_sp = sp;
  uint16_t s_ax = ax, s_bx = bx, s_cx = cx, s_dx = dx;
  uint16_t s_si = si, s_di = di, s_bp = bp, s_ds = ds, s_es = es;
  uint8_t s_cf = CF, s_pf = PF, s_zf = ZF, s_sf = SF, s_of = OF, s_if = IF,
          s_df = DF;
  ax = r_ax; bx = r_bx; cx = r_cx; dx = r_dx; si = r_si; di = r_di;
  uint16_t sp_entry = sp;
  sp = (uint16_t)(sp - 2); memw_write(ss, sp, s_cs); /* push cs ... */
  sp = (uint16_t)(sp - 2); memw_write(ss, sp, s_ip); /* ... then ip (on top) */
  cs = seg; ip = off;
  ++isr_depth; /* gate re-entrant async (IRQ/callback) delivery while it runs */
  isr_expected_sp[isr_depth] = s_sp;
  while (!machine_halted && (int16_t)(sp - sp_entry) < 0) {
    uint32_t addr = ((uint32_t)cs << 4) + ip;
    if (!resolve_and_run_chunk(addr)) {
      shim_log_crash("[BUG] far callback reached unmapped cs:ip=%04X:%04X\n", cs,
                     ip);
      shim_flush_all_streams();
      exit(1);
    }
  }
  --isr_depth;
  cs = s_cs; ip = s_ip; ss = s_ss; sp = s_sp;
  ax = s_ax; bx = s_bx; cx = s_cx; dx = s_dx; si = s_si; di = s_di; bp = s_bp;
  ds = s_ds; es = s_es;
  CF = s_cf; PF = s_pf; ZF = s_zf; SF = s_sf; OF = s_of; IF = s_if; DF = s_df;
}

static void invoke_isr(uint8_t int_no, int preserve_regs, int preserve_stack,
                       int preserve_segments, uint16_t ret_ip,
                       const char *source, const char *func, int line) {
  /* All saved-state locals MUST be volatile. The ISR returns via iret_impl
   * → longjmp back to the setjmp below; C99 6.11.2.1 then says any
   * non-volatile auto whose value was changed between setjmp and longjmp
   * is indeterminate, and in practice clang/gcc keep these saves in
   * registers that get clobbered by the ISR body. Without volatile, the
   * `ax = saved_ax;` restore further down writes garbage to ax. Manifests
   * as key-mash crashes where the timer ISR fires mid file-read → push ax
   * → close → pop dx sequence in a single-segment binary: the push captures whatever
   * garbage `ax` was restored to, the pop loads garbage into `dx`, the
   * decompressor's outer-loop count is then bogus, and rep_stosb overruns
   * into the RCB. The comment author had already volatile-qualified
   * saved_dispatch_depth / sp_at_invoke_isr_entry / saved_tail_pending
   * below for exactly this reason; the register saves were a missed case.
   * (2026-05-28, key-mash investigation) */
  volatile uint16_t saved_ss = ss;
  volatile uint16_t saved_sp = sp;
  volatile uint16_t saved_stack_word0 = 0;
  volatile uint16_t saved_stack_word1 = 0;
  volatile uint16_t saved_ax = 0, saved_bx = 0, saved_cx = 0, saved_dx = 0;
  volatile uint16_t saved_si = 0, saved_di = 0, saved_bp = 0;
  volatile uint8_t saved_cf = 0, saved_pf = 0, saved_zf = 0;
  volatile uint8_t saved_sf = 0, saved_of = 0, saved_if = 0, saved_df = 0;
  volatile uint16_t saved_ds = 0, saved_es = 0;
  volatile uint16_t saved_cs = cs;
  if (preserve_stack) {
    saved_stack_word0 = memw(saved_ss, saved_sp);
    saved_stack_word1 = memw(saved_ss, (uint16_t)(saved_sp + 2));
  }
  if (preserve_regs || preserve_segments) {
    saved_ds = ds;
    saved_es = es;
  }
  if (preserve_regs) {
    saved_ax = ax;
    saved_bx = bx;
    saved_cx = cx;
    saved_dx = dx;
    saved_si = si;
    saved_di = di;
    saved_bp = bp;
    saved_cf = CF;
    saved_pf = PF;
    saved_zf = ZF;
    saved_sf = SF;
    saved_of = OF;
    saved_if = IF;
    saved_df = DF;
  }

  uint16_t flags =
      (uint16_t)(0x0002u | CF | (PF << 2) | (ZF << 6) | (SF << 7) |
                 (IF << 9) | (DF << 10) | (OF << 11));
  sp = (sp - 2) & 0xFFFF;
  memw_write(ss, sp, flags);
  sp = (sp - 2) & 0xFFFF;
  memw_write(ss, sp, cs);
  sp = (sp - 2) & 0xFFFF;
  memw_write(ss, sp, ret_ip);
  /*
   * Interrupt delivery clears IF until the ISR executes IRET.  This applies
   * to both hardware IRQs and software INT instructions.
   */
  shim_log_stdout("Trace: isr_depth enter (IF->0)\n");
  IF = 0;
  uint16_t vector_off = (uint16_t)int_no * 4;
  uint16_t isr_ip = memw_raw_read(0, vector_off);
  uint16_t isr_cs = memw_raw_read(0, vector_off + 2);
  last_int_no = int_no;
  ++isr_depth;
  isr_expected_sp[isr_depth] = sp;
  /* Faithful interrupt injection: cs:ip now point at the IVT handler and the
   * flags+cs+ip frame is on the emulated stack. Run the handler on the SAME
   * flat dispatch the top-level loop uses -- no setjmp/longjmp. The handler is
   * ordinary code (game ISR or a synthetic BIOS stub); its far calls/rets and
   * iret all flow through cpu.r_cs:cpu.r_ip on the emulated stack. The ISR's
   * net stack effect is zero (push flags+cs+ip at entry, iret pops them), so
   * the loop terminates when sp climbs back to the pre-injection value -- the
   * exact moment iret restored the interrupted cs:ip. */
  uint16_t sp_at_invoke_isr_entry = saved_sp;
  cpu.r_cs = isr_cs;
  cpu.r_ip = isr_ip;
  record_binary_cs(((uint32_t)isr_cs << 4) + isr_ip, isr_cs);
  shim_log_stdout(
      "Trace: isr_depth run: %d preserve=%d stack=%d seg=%d ret_ip=%04X "
      "target=%04X:%04X sp=%04X flags=0x%04X\n",
      isr_depth, preserve_regs, preserve_stack, preserve_segments, ret_ip,
      isr_cs, isr_ip, sp, flags);
  while (!machine_halted &&
         (int16_t)(sp - sp_at_invoke_isr_entry) < 0) {
    uint32_t addr = ((uint32_t)cpu.r_cs << 4) + cpu.r_ip;
    if (!resolve_and_run_chunk(addr)) {
      char msg[512];
      int mn = snprintf(msg, sizeof(msg),
          "[BUG] ISR (int 0x%02X) reached unmapped cs:ip=%04X:%04X "
          "(linear 0x%05X) sp=%04X (%s:%s:%d)\n",
          int_no, cs, ip, addr, sp, source ? source : "?",
          func ? func : "?", line);
      shim_log_crash("%s", msg);
      if (mn > 0) save_bug_bundle("isr_unmapped", addr, msg);
      shim_flush_all_streams();
      exit(1);
    }
  }
  shim_log_stdout(
      "Trace: isr_depth resume: %d last_int=0x%02X return cs:ip=%04X:%04X "
      "sp=%04X IF=%d\n",
      isr_depth, last_int_no, cs, ip, sp, IF);
  --isr_depth;
  shim_log_stdout("Trace: isr_depth exit -> %d\n", isr_depth);
  if (preserve_stack) {
    uint16_t after_word0 = memw(saved_ss, saved_sp);
    uint16_t after_word1 = memw(saved_ss, (uint16_t)(saved_sp + 2));
    if (after_word0 != saved_stack_word0 || after_word1 != saved_stack_word1) {
      shim_log_stdout(
          "Trace: stack-top changed across int 0x%02X (%s:%s:%d) ss:sp=%04X:%04X "
          "[%04X %04X] -> [%04X %04X]\n",
          int_no, source ? source : "<unknown>", func ? func : "<unknown>",
          line, saved_ss, saved_sp, saved_stack_word0, saved_stack_word1,
          after_word0, after_word1);
    }
    ss = saved_ss;
    sp = saved_sp;
  }
  if (preserve_regs) {
    /*
     * Async IRQ delivery should resume at the interrupted control-flow point
     * regardless of transient ISR stack corruption.
     */
    cpu.r_cs = saved_cs;
    cpu.r_ip = ret_ip;
    ax = saved_ax;
    bx = saved_bx;
    cx = saved_cx;
    dx = saved_dx;
    si = saved_si;
    di = saved_di;
    bp = saved_bp;
    CF = saved_cf;
    PF = saved_pf;
    ZF = saved_zf;
    SF = saved_sf;
    OF = saved_of;
    IF = saved_if;
    DF = saved_df;
  }
  if (preserve_regs || preserve_segments) {
    ds = saved_ds;
    es = saved_es;
  }
}

void run_interrupt_impl(uint8_t int_no, const char *file, const char *func,
                        int line) {
  shim_log(__func__, file, func, line, NULL);
  bios_timer_tick_preincremented = 0;
  /*
   * ``int imm8`` instructions are two bytes long.  The caller sets ``ip`` to
   * the interrupt's address prior to invoking this helper, so push the return
   * address for the instruction following the ``int`` opcode just like the CPU
   * would.
   */
  uint16_t return_ip = (uint16_t)(ip + 2);
  last_sw_interrupt.valid = 1;
  last_sw_interrupt.int_no = int_no;
  last_sw_interrupt.file = file;
  last_sw_interrupt.func = func;
  last_sw_interrupt.line = line;
  last_sw_interrupt.ax_before = ax;
  last_sw_interrupt.bx_before = bx;
  last_sw_interrupt.cx_before = cx;
  last_sw_interrupt.dx_before = dx;
  last_sw_interrupt.ds_before = ds;
  last_sw_interrupt.es_before = es;
  last_sw_interrupt.ss_before = ss;
  last_sw_interrupt.sp_before = sp;
  last_sw_interrupt.cs_before = cs;
  last_sw_interrupt.ip_before = ip;

  invoke_isr(int_no, 0, 1, 0, return_ip, "<interrupt>", func, line);

  last_sw_interrupt.ax_after = ax;
  last_sw_interrupt.bx_after = bx;
  last_sw_interrupt.cx_after = cx;
  last_sw_interrupt.dx_after = dx;
  last_sw_interrupt.ds_after = ds;
  last_sw_interrupt.es_after = es;
  last_sw_interrupt.ss_after = ss;
  last_sw_interrupt.sp_after = sp;
  last_sw_interrupt.cs_after = cs;
  last_sw_interrupt.ip_after = ip;
  if (int_no == 0x60) {
    log_last_sw_interrupt_snapshot();
  }
}

void run_interrupt(uint8_t int_no) {
  run_interrupt_impl(int_no, "<external>", __func__, 0);
}



void copy_linear_from_segoff(uint16_t seg, uint16_t off,
                                           size_t len, uint8_t *dst) {
  uint16_t s = seg;
  uint16_t o = off;
  for (size_t i = 0; i < len; ++i) {
    dst[i] = memb(s, o);
    uint16_t old = o;
    o = (uint16_t)(o + 1);
    if (o < old) {
      // 16-bit offset overflow -> carry into the segment component
      s = (uint16_t)(s + 0x1000);
    }
  }
}

void shim_copy_linear_block(uint16_t seg, uint16_t off, size_t len,
                            uint8_t *dst) {
  copy_linear_from_segoff(seg, off, len, dst);
}

void safe_point_impl(const char *file, const char *func, int line) {
  vclock_service();
  const uint64_t now_ns = shim_virtual_now_ns();
  const uint64_t elapsed_ns = now_ns - last_host_time_ns;
  last_host_time_ns = now_ns;

  if (!headless_mode) {
    /*
     * Even when we defer presenting a frame, keep SDL's input queue drained so
     * break scancodes are delivered promptly. Otherwise key releases that
     * occur during long CPU-bound sections can get stuck until the next render
     * pass.
     */
    virtual_display_poll_input();
  }
  if (!isr_depth) {
    uint64_t scaled_elapsed_ns =
        (uint64_t)((double)elapsed_ns * emulation_speedup);
    pit_cycle_fraction_accum += scaled_elapsed_ns * 1193182ull;
    pit_cycle_accum += pit_cycle_fraction_accum / 1000000000ull;
    pit_cycle_fraction_accum %= 1000000000ull;
    while (pit_cycle_accum >= pit.reload) {
      pit_cycle_accum -= pit.reload;
      /*
       * The BIOS time-of-day tick (0x46C) must advance at a FIXED 18.2 Hz
       * (every 65536 PIT cycles of REAL elapsed time), NOT once per channel-0
       * IRQ. A game may reprogram channel 0 to a short reload for a polled
       * delay (DM: 49/53); incrementing the tick per fire then made the wall
       * clock -- and every timer-paced loop -- race wildly fast. Accrue the
       * actual cycles consumed and step the tick only per real 18.2 Hz period.
       */
      bios_tick_cycle_debt += pit.reload;
      if (bios_tick_cycle_debt >= 65536) {
        bios_tick_cycle_debt -= 65536;
        bios_timer_increment();
        bios_timer_tick_backlog = 1;
      }
      /*
       * Collapsing multiple elapsed PIT ticks into a single pending IRQ keeps
       * the emulator responsive at high --speedup values, while avoiding long
       * IRQ0 backlogs that can otherwise dominate execution.
       */
      if (!irq0_pending) {
        irq0_pending = 1;
        if (!headless_mode) {
          if (now_ns - last_present_time_ns >= 16000000ull) {
            last_present_time_ns = now_ns;
            stage_and_present_current_buffer();
          }
        } else if (now_ns - last_present_time_ns >= 16000000ull) {
          /* Headless parity for the save state machine: SDL drives
           * save_manager_poll_pending() from the per-present path; do the same
           * here at the same cadence so a requested save (stdin 0x19) fires at
           * the next valid SAFEPOINT. */
          last_present_time_ns = now_ns;
          extern void save_manager_poll_pending(void);
          save_manager_poll_pending();
        }
      }
    }
    if (headless_mode && SCREENSHOT_INTERVAL_SECS > 0 &&
        now_ns - last_screenshot_time_ns >=
            (uint64_t)SCREENSHOT_INTERVAL_SECS * 1000000000ull) {
      last_screenshot_time_ns = now_ns;
      shim_save_video_memory();
    }
    if (!irq0_pending && bios_timer_tick_backlog > 0) {
      irq0_pending = 1;
    }
  }

  if (interrupt_shadow) {
    interrupt_shadow = 0;
    return;
  }

  /* Stdin-poll cadence fallback. The gate below normally reads stdin only when a
   * timer tick is pending (irq0_pending) or the vclock is idle -- which is fine
   * for code that HLTs or paces itself. But a pure busy-wait menu loop (PoP's
   * SETUP spins on AH=07 without HLT) calls safe_point millions of times between
   * two PIT ticks; since wall-clock barely advances per call, irq0_pending is
   * almost never set and the vclock stays RUNNING, so stdin is polled only a
   * few hundred times across the whole session. Keys typed after the menu
   * appears then never get read. Force a stdin poll every N calls so a tight
   * busy-wait still drains the input pipe promptly (N is large enough that
   * normal paced code -- which already passes the gate via irq0_pending -- pays
   * no measurable cost). */
  static uint32_t sp_poll_counter;
  int force_stdin_poll = (++sp_poll_counter & 0x3FF) == 0;
  if (!isr_depth && keyboard_input_enabled &&
      (irq0_pending || vclock_state != VCLOCK_RUNNING || force_stdin_poll)) {
    uint8_t c;
    for (;;) {
      ssize_t r = session_logged_read(&c, 1);
      if (r == 1) {
        /* fall through to byte-processing below */
      } else if (r == 0) {
        /* EOF — a previous writer closed the FIFO. Reopen via /proc/self/fd/0
         * (always refers to the original stdin connection) so future writers
         * can be seen again. Important for long-running headless drivers
         * where the controller process may restart while the game keeps
         * running. */
        int newfd = open("/proc/self/fd/0", O_RDONLY | O_NONBLOCK);
        if (newfd >= 0 && newfd != keyboard_fd) {
          dup2(newfd, keyboard_fd);
          close(newfd);
        }
        break;
      } else {
        /* -1: EAGAIN (no data) or real error — leave the loop. */
        break;
      }
      /* (Backspace/DEL early-exit removed — was triggering accidental
       * silent exits when keystrokes landed in the terminal pane instead
       * of the SDL window. Headless CI drivers can use SIGTERM or close
       * stdin to terminate the game cleanly.) */
      if (c == 0x14) {
        /* Ctrl+T = DC4 = on-demand screenshot trigger. Lets external drivers
         * grab a frame whenever they want one (the auto-interval still fires
         * separately). Doesn't enqueue any key event. */
        shim_save_video_memory();
        continue;
      }
      if (c == 0x19) {
        /* EM = request a manual save (headless analogue of SDL Cmd+F1). Arms
         * the same save_manager request the SDL key drives; the per-present
         * poll in safe_point then captures at the next valid SAFEPOINT. */
        extern void save_manager_request_save(void);
        save_manager_request_save();
        continue;
      }
      if (c == 0x15) {
        /* NAK = halt virtual clock. Game freezes; emulator main loop
         * keeps spinning so future opcodes (including resume) are still
         * read. Idempotent. */
        vclock_halt();
        continue;
      }
      if (c == 0x16) {
        /* SYN = resume virtual clock from halted/stepping state.
         * Idempotent if already running. */
        vclock_resume();
        continue;
      }
      if (c == 0x17) {
        /* ETB = step N ticks then halt. Next 2 bytes = ticks_lo, ticks_hi.
         * Same atomic-write semantics as the 0x12 tap opcode. After the
         * step, the clock is HALTED until a resume or another step. */
        uint8_t buf[2];
        size_t got = 0;
        for (int tries = 0; got < 2 && tries < 1000; ++tries) {
          ssize_t n2 = session_logged_read(buf + got, 2 - got);
          if (n2 > 0) got += (size_t)n2;
        }
        if (got == 2) {
          uint16_t ticks = (uint16_t)buf[0] | ((uint16_t)buf[1] << 8);
          vclock_step((uint32_t)ticks);
        } else {
          shim_log_stdout("[VCLOCK] step short read got=%zu\n", got);
        }
        continue;
      }
      if (c == 0x18) {
        /* CAN = read memory. Next 5 bytes: addr_lo addr_mid_lo addr_mid_hi
         * addr_hi (little-endian uint32), then len (uint8). Writes the
         * bytes to snapshots/last_read.bin. Caller reads that file. */
        uint8_t buf[5];
        size_t got = 0;
        for (int tries = 0; got < 5 && tries < 1000; ++tries) {
          ssize_t n2 = session_logged_read(buf + got, 5 - got);
          if (n2 > 0) got += (size_t)n2;
        }
        if (got == 5) {
          uint32_t addr = (uint32_t)buf[0] | ((uint32_t)buf[1] << 8) |
                          ((uint32_t)buf[2] << 16) | ((uint32_t)buf[3] << 24);
          uint8_t len = buf[4];
          shim_read_memory_to_sidecar(addr, len);
        } else {
          shim_log_stdout("[READ] short read got=%zu\n", got);
        }
        continue;
      }
      if (c == 0x1D) {
        /* GS = set_virtual_clock. Next 8 bytes: virtual_ns little-endian.
         * Forces vclock to HALTED at exactly this vns. Used by the replay
         * tool to override step rounding and land the vclock at the
         * recorded vns for each input byte. Going backward is allowed
         * but corrupts PIT state (next safepoint sees a huge negative
         * elapsed_ns); the replay tool guarantees monotonic forward
         * progression. */
        uint8_t buf[8];
        size_t got = 0;
        for (int tries = 0; got < 8 && tries < 1000; ++tries) {
          ssize_t n2 = session_logged_read(buf + got, 8 - got);
          if (n2 > 0) got += (size_t)n2;
        }
        if (got == 8) {
          uint64_t vns = 0;
          for (int i = 0; i < 8; i++) vns |= ((uint64_t)buf[i]) << (8 * i);
          vclock_frozen_virtual_ns = vns;
          vclock_state = VCLOCK_HALTED;
          shim_log_stdout("[VCLOCK] set_virtual_clock vns=%llu\n",
                  (unsigned long long)vns);
        } else {
          shim_log_stdout("[VCLOCK] set_vc short read got=%zu\n", got);
        }
        continue;
      }
      if (c == 0x1A) {
        /* SUB = snapshot full RAM to snapshots/snap_<N>.bin. The N is a
         * per-process counter — the writer prints the path it used to
         * stderr, callers can also list the dir to find the latest. */
        shim_dump_ram_snapshot();
        continue;
      }
      if (c == 0x10 || c == 0x11) {
        /* DLE (0x10) press, DC1 (0x11) release. The next byte is the raw
         * scancode (7-bit make code). Unlike the auto-pair ASCII path
         * below, these emit ONLY the make (or break), so external drivers
         * can simulate held keys (walking is impossible without holding,
         * since the game polls key state per tick). */
        uint8_t kind = c;
        uint8_t sc;
        ssize_t n2 = session_logged_read(&sc, 1);
        if (n2 == 1 && sc) {
          if (kind == 0x10) {
            shim_keyboard_enqueue_scancode_press(sc);
          } else {
            shim_keyboard_enqueue_scancode_release(sc);
          }
        }
        continue;
      }
      if (c == 0x12) {
        /* DC2 (0x12) tap: 3 bytes follow — scancode, then ticks_lo,
         * ticks_hi. Enqueues the press now and schedules the matching
         * release after exactly N BIOS IRQ0 ticks of game time (18.2 Hz,
         * fixed regardless of host load). Drivers use this for
         * deterministic walking — wall-clock holds vary with emulator
         * speed and per-frame work, so the same `\x10 4D / sleep / \x11
         * 4D` sequence walks different distances run-to-run. The tap
         * opcode collapses both events into one tick-counted unit so the
         * walked distance is reproducible.
         *
         * keyboard_fd is O_NONBLOCK and stdin is a pipe — a single
         * read() may return fewer bytes than requested even though the
         * writer sent all 4 atomically. Loop with a short busy-wait so
         * the rest of the payload arrives before we give up. */
        uint8_t buf[3];
        size_t got = 0;
        for (int tries = 0; got < 3 && tries < 1000; ++tries) {
          ssize_t n2 = session_logged_read(buf + got, 3 - got);
          if (n2 > 0) got += (size_t)n2;
        }
        if (got == 3 && buf[0]) {
          uint8_t sc = buf[0] & 0x7F;
          uint16_t ticks = (uint16_t)buf[1] | ((uint16_t)buf[2] << 8);
          if (ticks == 0) ticks = 1;
          shim_keyboard_enqueue_scancode_press(sc);
          /* 1 BIOS tick = ~54.925 ms at 18.2 Hz = 54_925_000 ns. Divide
           * by speedup so that a "100 tick" hold lasts the right amount
           * of *game* time — with speedup=2 we walk the same distance in
           * half the wall-clock seconds. */
          uint64_t ns_per_tick = (uint64_t)(54925000.0 / emulation_speedup);
          uint64_t now_v = shim_virtual_now_ns();
          pending_release_deadline_ns[sc] =
              now_v + (uint64_t)ticks * ns_per_tick;
          shim_log_stdout("[TAP] sc=0x%02X ticks=%u virtual_ns=%llu deadline=%llu\n",
                  sc, (unsigned)ticks, (unsigned long long)now_v,
                  (unsigned long long)pending_release_deadline_ns[sc]);
        } else {
          shim_log_stdout("[TAP] short read got=%zu buf=%02X%02X%02X\n",
                  got, buf[0], buf[1], buf[2]);
        }
        continue;
      }
      if (c == 0x13) {
        /* DC3 (0x13) mouse inject: 5 bytes follow — x_lo, x_hi, y_lo, y_hi,
         * buttons (bit0=L bit1=R bit2=M). Sets the absolute driver position and
         * button state, synthesising INT 33h motion/press/release events so the
         * game's fn-0x0C handler fires (headless has no SDL mouse). */
        uint8_t mb[5];
        size_t got = 0;
        for (int tries = 0; got < 5 && tries < 1000; ++tries) {
          ssize_t n2 = session_logged_read(mb + got, 5 - got);
          if (n2 > 0) got += (size_t)n2;
        }
        if (got == 5) {
          int16_t mx = (int16_t)((uint16_t)mb[0] | ((uint16_t)mb[1] << 8));
          int16_t my = (int16_t)((uint16_t)mb[2] | ((uint16_t)mb[3] << 8));
          mouse_host_inject(mx, my, mb[4]);
          shim_log_stdout("[MOUSE] inject x=%d y=%d buttons=0x%02X\n", mx, my,
                  mb[4]);
        }
        continue;
      }
      uint8_t ascii = 0;
      uint8_t scancode = 0;
      if (c == 0x1B) {
        uint8_t seq[2];
        ssize_t n2 = session_logged_read(seq, 2);
        if (n2 == 2 && seq[0] == '[') {
          switch (seq[1]) {
          case 'A': scancode = 0x48; break;
          case 'B': scancode = 0x50; break;
          case 'C': scancode = 0x4D; break;
          case 'D': scancode = 0x4B; break;
          default:  ascii = 0x1B; scancode = 0x01; break;
          }
        } else {
          ascii = 0x1B; scancode = 0x01;
        }
      } else {
        ascii = (c == '\n') ? '\r' : c;
        scancode = ascii_to_scan(ascii);
      }
      kbd_enqueue(ascii, scancode);
      /* Stdin only delivers key-press characters, but the game's IRQ1 ISR
       * tracks key state via make/break pairs. Without an automatic
       * release event the key appears stuck (e.g. Enter held through the
       * castle scene cycles menus). Inject the matching break scancode
       * right after the make so port-0x60 pollers see one full keystroke. */
      if (scancode) {
        kbd_queue_push(0, (uint8_t)(scancode | 0x80));
      }
    }
  }

  uint8_t pending_int = 0xFF;
  const char *source = "<timer>";
  for (int i = 0; i < 256; ++i) {
    if (i == 0x08) {
      continue;
    }
    if (irq_pending[i]) {
      pending_int = (uint8_t)i;
      source = "<interrupt>";
      break;
    }
  }
  if (pending_int == 0xFF && irq0_pending && IF && !isr_depth &&
      !critical_depth) {
    /*
     * Deliver the timer interrupt only after all other pending interrupts to
     * avoid starving them.  Only a single IRQ0 is serviced per safe_point
     * invocation.
     */
    pending_int = 0x08;
    irq0_pending = 0;
    /* Tap-opcode release scheduling: decrement once per actual IRQ0
     * delivery (the rate the game's main loop perceives), NOT per PIT
     * cycle. Catchup batches collapse many PIT cycles into one IRQ0, so
     * paced from PIT the release fires before the game can run a single
     * frame of "key held." */
    pending_release_tick();
  }
  if (IF && !isr_depth && !critical_depth && pending_int != 0xFF) {
    if (pending_int != 0x08) {
      irq_pending[pending_int] = 0;
    }
    /*
     * Asynchronous interrupts delivered at SAFEPOINT() boundaries should
     * preserve registers just like hardware IRQs, while synchronous ``int``
     * instructions (run via run_interrupt) are allowed to return values in
     * registers.
     */
    /*
     * SAFEPOINT() delivery is always asynchronous with respect to translated
     * game code, so preserve caller-visible register state for every pending
     * IRQ vector.  Synchronous ``int`` instructions still route through
     * run_interrupt_impl(), which keeps register-return semantics.
     */
    int preserve_regs = 1;
    /* Do NOT pre-set ax for INT 09. Real hardware: the keyboard ISR
     * reads scancode from port 0x60 (`in al, 0x60`) inside its own body.
     * a single-segment binary's INT 09 handler at file_off 0x01D1 does exactly that.
     * The previous pre-set here corrupted the interrupted code's `ax`
     * because preserve_regs=1 saved this synthetic value at invoke_isr
     * entry and restored it after the ISR returned — so whatever the
     * game was doing with ax (typically an indexed write `mov [es:di],
     * ax` or arithmetic) got clobbered. Manifested as visible white
     * pixels on screen that tracked every key event 1:1 — when the
     * interrupted code happened to be mid-VGA write, the scancode pair
     * landed in video memory. (2026-05-28) */
    if (pending_int == 0x08) {
      /* The time-of-day tick is advanced at a fixed 18.2 Hz by the debt-gated
       * loop in safe_point (so a game's short channel-0 reload for a polled
       * delay does not race the wall clock). The hardware IRQ0 handler must
       * therefore NOT also increment it -- always mark it pre-incremented. */
      bios_timer_tick_preincremented = 1;
      if (bios_timer_tick_backlog > 0) {
        --bios_timer_tick_backlog;
      }
    } else {
      bios_timer_tick_preincremented = 0;
    }
    invoke_isr(pending_int, preserve_regs, preserve_regs, preserve_regs,
               ip, source, func, line);
    bios_timer_tick_preincremented = 0;
  }
  /* Deliver accrued mouse events to the game's INT 33h fn-0x0C handler as a far
   * call -- only at a delivery-safe point (interrupts on, not nested in an ISR
   * or a shim critical section), exactly like a hardware IRQ. Mouse-driven UIs
   * (some programs) get NO input otherwise and stall / loop their attract. */
  if (IF && !isr_depth && !critical_depth) {
    mouse_deliver_pending_events();
  }
}

static void warn_rcb_overlap(const char *path, const void *addr, size_t len);
static void warn_file_overlap(const char *path, const void *addr, size_t len);
static void warn_on_mutation(uint32_t addr, size_t size, const char *file,
                             const char *func, int line);
static GameFunc lookup_call_target(uint32_t addr, const char *kind,
                                   const char *file, const char *func,
                                   int line);
static int try_dispatch_overlay_first(uint32_t addr, uint16_t expected_retip,
                                      const char *file, const char *func,
                                      int line);
static void report_unmapped(const char *kind, uint32_t addr,
                            const char *caller_file, const char *caller_func,
                            int line);


static bool try_memory_offset(const void *addr, uint32_t *offset) {
  if (!virtual_memory || !addr) {
    return false;
  }

  const uint8_t *p = (const uint8_t *)addr;
  if (p < virtual_memory || p >= virtual_memory + MEMORY_SIZE) {
    return false;
  }

  *offset = (uint32_t)(p - virtual_memory);
  return true;
}

static bool try_memory_range(const void *addr, size_t len, uint32_t *base,
                             uint32_t *end) {
  uint32_t offset;
  if (!try_memory_offset(addr, &offset)) {
    return false;
  }

  if (base) {
    *base = offset;
  }
  if (end) {
    *end = offset + (uint32_t)len;
  }
  return true;
}


/* Evict or shrink existing mappings that overlap [new_base, new_end). The
 * file_mappings table flat-mirrors real DOS memory: at any byte, at most one
 * chunk is live. When a new LOAD overwrites bytes covered by an older
 * mapping, the older mapping is shrunk to the surviving portion (or split
 * if the new range punches a hole through the middle). After this call,
 * no remaining entry overlaps [new_base, new_end).
 *
 * Split semantics: if old fully contains new, the old slot is reused for
 * the left piece (start..new_base); the right piece (new_end..old_end) is
 * appended as a fresh entry. Inserting the right piece at higher index is
 * fine — with no overlap remaining, find_file_mapping's reverse-walk
 * finds the unique covering entry either way.
 *
 * `.data` (the LOAD-time byte snapshot used by warn_file_overlap +
 * snapshot serialization) is freed for shrunk/split entries because we
 * can't subset it cheaply; that degrades diagnostic detail but doesn't
 * affect dispatch correctness. */
static void evict_or_shrink_for_load(uint32_t new_base, size_t new_len) {
  if (new_len == 0) return;
  uint32_t new_end = new_base + (uint32_t)new_len;
  size_t out = 0;
  size_t splits_to_append = 0;
  /* Reserve room at the end of file_mappings for split right-pieces. We
   * cap at a small number — a single LOAD physically can't split more
   * than a handful of distinct prior chunks. */
  enum { MAX_SPLITS_PER_LOAD = 8 };
  FileMapping splits[MAX_SPLITS_PER_LOAD];
  for (size_t i = 0; i < file_mapping_count; ++i) {
    FileMapping e = file_mappings[i];
    uint32_t e_end = e.base + (uint32_t)e.len;
    if (e_end <= new_base || e.base >= new_end) {
      /* No overlap — keep as-is. */
      file_mappings[out++] = e;
      continue;
    }
    if (e.base >= new_base && e_end <= new_end) {
      /* Fully covered — evict. */
      free(e.path);
      free(e.data);
      continue;
    }
    if (e.base < new_base && e_end > new_end) {
      /* New punches a hole — split into left (reuses slot) + right (queued). */
      if (splits_to_append >= MAX_SPLITS_PER_LOAD) {
        shim_log_crash(
            "[BUG] evict_or_shrink_for_load: too many splits in one LOAD "
            "at 0x%05X len 0x%zX — raise MAX_SPLITS_PER_LOAD or audit "
            "chunk boundaries upstream\n",
            new_base, new_len);
        shim_flush_all_streams();
        abort();
      }
      FileMapping right = e;
      right.path = strdup(e.path);
      right.base = new_end;
      right.len = (size_t)(e_end - new_end);
      right.file_offset = e.file_offset + (size_t)(new_end - e.base);
      right.data = NULL;
      splits[splits_to_append++] = right;
      /* Shrink left piece in place. */
      e.len = (size_t)(new_base - e.base);
      free(e.data);
      e.data = NULL;
      file_mappings[out++] = e;
      continue;
    }
    if (e.base >= new_base) {
      /* Left side of e is overlapped — advance e's start to new_end. */
      size_t advance = (size_t)(new_end - e.base);
      e.base = new_end;
      e.file_offset += advance;
      e.len -= advance;
      free(e.data);
      e.data = NULL;
      file_mappings[out++] = e;
      continue;
    }
    /* e.base < new_base && e_end <= new_end — right side of e is overlapped;
     * shrink e to end at new_base. */
    e.len = (size_t)(new_base - e.base);
    free(e.data);
    e.data = NULL;
    file_mappings[out++] = e;
  }
  file_mapping_count = out;
  for (size_t i = 0; i < splits_to_append; ++i) {
    if (file_mapping_count >= MAX_FILE_MAPPINGS) {
      shim_log_crash(
          "[BUG] evict_or_shrink_for_load: file_mappings full while appending "
          "split right-piece — raise MAX_FILE_MAPPINGS\n");
      shim_flush_all_streams();
      abort();
    }
    file_mappings[file_mapping_count++] = splits[i];
  }
}

static void register_file_mapping(const char *path, size_t file_offset,
                                  const void *addr, size_t len) {
  const uint8_t *p = addr;
  if (p < virtual_memory || p >= virtual_memory + MEMORY_SIZE) {
    shim_log_stdout(
        "Trace: register_file_mapping: ignoring %s at %p (outside virtual "
        "memory)\n",
        path, addr);

    return;
  }
  uint32_t base = (uint32_t)(p - virtual_memory);
  evict_or_shrink_for_load(base, len);
  if (file_mapping_count < MAX_FILE_MAPPINGS) {
    file_mappings[file_mapping_count].path = strdup(path);
    file_mappings[file_mapping_count].base = base;
    file_mappings[file_mapping_count].len = len;
    file_mappings[file_mapping_count].file_offset = file_offset;
    file_mappings[file_mapping_count].loader_cs = cs;
    file_mappings[file_mapping_count].loader_ip = ip;
    file_mappings[file_mapping_count].loader_ss = ss;
    file_mappings[file_mapping_count].loader_sp = sp;
    for (int i = 0; i < 8; ++i) {
      uint16_t off = (uint16_t)((sp + 2 * i) & 0xFFFF);
      file_mappings[file_mapping_count].loader_stack[i] = memw_raw_read(ss, off);
    }
    file_mappings[file_mapping_count].data = malloc(len);
    if (file_mappings[file_mapping_count].data) {
      memcpy(file_mappings[file_mapping_count].data, addr, len);
    }
    shim_log_stdout(
        "Trace: register_file_mapping[%zu]: %s mapped at 0x%05X-0x%05X "
        "(file offset 0x%zX)\n",
        file_mapping_count, path, base, base + (uint32_t)len, file_offset);
    {
      const char *bn = strrchr(path, '/');
      bn = bn ? bn + 1 : path;
      /* Include the game-side caller cs:ip so we can pin which game function
       * decided to load this chunk — answers "did real DOS load chunk X
       * here too, or did our translation route us into a wrong branch that
       * issued an extra load?". */
      lifecycle_log("LOAD %s+0x%zX @ 0x%05X-0x%05X (len 0x%zX) from cs:ip=%04X:%04X\n",
                    bn, file_offset, base, base + (uint32_t)len, len,
                    cs, ip);
    }

    file_mapping_count++;
  } else {
    printf("Error: register_file_mapping: too many file mappings\n");
    shim_flush_all_streams();
    exit(1);
  }
}

void dos_set_current_psp_to_load(void);

static void init_psp(void) {
  /* Rebind DOS's "current PSP" to the live load segment (a game config may have
   * lowered psp_seg from the default). */
  dos_set_current_psp_to_load();
  memset(psp, 0, sizeof(PSP));
  psp->raw[0] = 0xCD;
  psp->raw[1] = 0x20;
  /* PSP[0x02]: segment of the first paragraph BEYOND the program's memory
   * block (top of allocated memory). Real DOS fills this; programs read it to
   * size their stack/heap (e.g. PoP's SETUP does `mov si,[2]; sub si,<base>`).
   * Use the conventional-memory ceiling (640 KB = segment 0xA000). */
  memw_write(PSP_SEG, 0x02, CONVENTIONAL_TOP_SEG);

  for (int i = 0; i < MAX_DOS_HANDLES; ++i) {
    memb_write(PSP_SEG, 0x18 + i, (i < 5) ? i : 0xFF);
  }

  env_block = seg_off(ENV_SEG, 0);
  memset(env_block, 0, 0x100);
  const char *program_path = game_config.program_path;
  if (!program_path) {
    program_path = "program.exe";
  }
  char dos_path[128];
  int written = snprintf(dos_path, sizeof(dos_path), "C:\\%s", program_path);
  if (written < 0) {
    dos_path[0] = '\0';
    written = 0;
  }
  size_t path_len = (size_t)written;
  if (path_len >= 0x100 - 2) {
    path_len = 0x100 - 5;
  }
  /* Build a faithful DOS environment block. Real programs are launched from
   * COMMAND.COM, which always passes a non-empty environment (COMSPEC, PATH,
   * ...). A degenerate empty env (just the terminator) makes some C runtimes'
   * env-size walk compute a 2-byte size that then gets reused as a stale
   * pointer offset, derailing later env processing. Layout: a series of
   * "NAME=VALUE\0" strings, an empty-string \0 terminating the list, a WORD
   * count of the strings that follow (1: the program path), then the
   * fully-qualified program path. */
  static const char *const env_vars[] = {
      "COMSPEC=C:\\COMMAND.COM",
      "PATH=C:\\",
      "PROMPT=$p$g",
  };
  size_t off = 0;
  for (size_t i = 0; i < sizeof(env_vars) / sizeof(env_vars[0]); ++i) {
    size_t n = strlen(env_vars[i]);
    memcpy(env_block + off, env_vars[i], n);
    off += n;
    env_block[off++] = '\0';
  }
  env_block[off++] = '\0'; /* empty string terminates the variable list */
  env_block[off++] = 0x01; /* WORD count of following strings (the path) */
  env_block[off++] = 0x00;
  if (off + path_len + 1 > 0x100) {
    path_len = 0x100 - off - 1;
  }
  memcpy(env_block + off, dos_path, path_len);
  env_block[off + path_len] = '\0';
  memw_write(PSP_SEG, 0x2C, ENV_SEG);
  dta_ptr = seg_off(PSP_SEG, 0x80);

  /* Populate the PSP command tail with runtime arguments. DOS stores the
   * length in the first byte followed by the characters and a terminating
   * 0x0D. Our loader does not pass any arguments to the emulated program, so
   * the tail starts empty with only the terminator. */
  memb_write(PSP_SEG, 0x80, 0);
  memb_write(PSP_SEG, 0x81, 0x0D);
  memset(seg_off(PSP_SEG, 0x5C), 0, 0x10);
  memset(seg_off(PSP_SEG, 0x6C), 0, 0x10);
}

static void init_bios_data_area(void) {
  uint8_t *bda = seg_off(0x40, 0);
  memset(bda, 0, 0x100);
  memw_raw_write(0x40, 0x0010, BIOS_EQUIPMENT_WORD);
  memb_raw(0x40, 0x49) = bios_video.video_mode;
  memb_raw(0x40, 0x4A) = 80;
  memw_raw_write(0x40, 0x4C, 0x0FA0);
  memw_raw_write(0x40, 0x63, 0x3D4);
  memw_raw_write(0x40, 0x6C, 0);
  memw_raw_write(0x40, 0x6E, 0);
  memb_raw(0x40, 0x66) = 0;
  memb_raw(0x40, 0x62) = 0;
  bios_video.cga_palette_select = 0;
  bios_video.cga_border_color = 0;
  video_invalidate_palette_cache();
  for (int page = 0; page < 8; ++page) {
    bios_video.cursor_row[page] = 0;
    bios_video.cursor_col[page] = 0;
    bios_video.cursor_attr[page] = 0x07;
    memw_raw_write(0x40, (uint16_t)(0x50 + page * 2), 0);
  }
  bios_video.active_page = 0;
  for (size_t i = 0; i < sizeof(bios_video_parameter_table_mode6); ++i) {
    memb_raw(BIOS_VIDEO_PARAM_SEG, (uint16_t)(BIOS_VIDEO_PARAM_OFF + i)) =
        bios_video_parameter_table_mode6[i];
  }
}

static int is_builtin_call_target(uint32_t addr);

static const FileMapping *find_file_mapping(uint32_t addr) {
  for (ssize_t i = (ssize_t)file_mapping_count - 1; i >= 0; --i) {
    uint32_t base = file_mappings[i].base;
    if (addr >= base && addr < base + file_mappings[i].len) {
      return &file_mappings[i];
    }
  }
  if (is_builtin_call_target(addr)) {
    return NULL;
  }
  shim_log_stdout("Trace: find_file_mapping: address 0x%05X not mapped\n",
                  addr);

  return NULL;
}

static FileMapping *find_file_mapping_mut(uint32_t addr) {
  for (ssize_t i = (ssize_t)file_mapping_count - 1; i >= 0; --i) {
    uint32_t base = file_mappings[i].base;
    if (addr >= base && addr < base + file_mappings[i].len) {
      return &file_mappings[i];
    }
  }
  return NULL;
}

/* Helper for the snprintf-into-buffer pattern below. Always advances n by
 * the formatted length even on overflow, so the final return value is
 * clamped to cap - 1. */
#define UNHPC_APPEND(...) do {                                                \
  if (n < cap) {                                                              \
    int _w = snprintf(out + n, cap - n, __VA_ARGS__);                         \
    if (_w > 0) n += (size_t)_w;                                              \
  }                                                                           \
} while (0)

int shim_unhandled_pc_report(const char *module, int pc,
                             char *out, size_t cap) {
  if (!out || cap == 0) return 0;
  size_t n = 0;

  uint32_t linear = ((uint32_t)cs << 4) + ((uint32_t)ip & 0xFFFF);

  /* pc is printed FULL-WIDTH (no & 0xFFFF mask) because multi-chunk binaries
   * (overlay chunks) have case keys that exceed 16 bits — masking would
   * truncate it to a wrong address (e.g. 0x7997 instead of the real 0x17997),
   * misleading post-mortem tools. */
  UNHPC_APPEND("[BUG] Unhandled pc=%X in %s_dispatch\n",
               (unsigned)pc, module);
  UNHPC_APPEND("[BUG]   cs:ip=%04X:%04X  linear=0x%05X  active_binary=%s\n",
               cs, ip, linear,
               shim_active_binary() ? shim_active_binary() : "<none>");

  const FileMapping *picked = find_file_mapping(linear);
  if (picked && picked->path) {
    const char *pn = strrchr(picked->path, '/');
    pn = pn ? pn + 1 : picked->path;
    size_t target_file_off = picked->file_offset + (linear - picked->base);
    UNHPC_APPEND("[BUG]   primary mapping: %s base=0x%05X len=0x%zX "
                 "chunk_file_off=0x%zX -> target file_off=0x%zX "
                 "canonical_cs=0x%04X\n",
                 pn, picked->base, picked->len, picked->file_offset,
                 target_file_off, picked->canonical_cs);
  } else {
    UNHPC_APPEND("[BUG]   primary mapping: NONE - linear 0x%05X is unmapped\n",
                 linear);
  }

  /* All other mappings that ALSO cover this linear - chunk-swap suspects. */
  int overlap_count = 0;
  for (size_t i = 0; i < file_mapping_count; ++i) {
    const FileMapping *m = &file_mappings[i];
    if (m == picked) continue;
    if (linear < m->base || linear >= m->base + m->len) continue;
    if (overlap_count++ == 0) {
      UNHPC_APPEND("[BUG]   overlapping mappings at same linear "
                   "(chunk-swap candidates):\n");
    }
    const char *pn = strrchr(m->path, '/');
    pn = pn ? pn + 1 : m->path;
    size_t alt_file_off = m->file_offset + (linear - m->base);
    UNHPC_APPEND("[BUG]     [%3zu] %s base=0x%05X len=0x%zX "
                 "chunk_file_off=0x%zX -> ALT target file_off=0x%zX "
                 "canonical_cs=0x%04X\n",
                 i, pn, m->base, m->len, m->file_offset, alt_file_off,
                 m->canonical_cs);
  }

  if (overlap_count > 0) {
    UNHPC_APPEND("[BUG]   diagnosis: %d overlapping mapping(s) - likely "
                 "chunk-swap stale target. The runtime computed this address "
                 "while one chunk was loaded at base 0x%05X, but a different "
                 "chunk is loaded there now; the SAME stored ret/jump value "
                 "resolves to a different file_offset in the currently-active "
                 "chunk.\n",
                 overlap_count, picked ? picked->base : 0);
  } else if (picked) {
    UNHPC_APPEND("[BUG]   diagnosis: pc=0x%04X is inside %s but no dispatch "
                 "case matches it. Either the disassembler missed a basic-"
                 "block boundary at this offset, or an upstream computation "
                 "produced a target that's mid-block.\n",
                 (unsigned)pc, module);
  }

  UNHPC_APPEND("[BUG]   diagnosis: with literal-emission translation, the "
               "dispatch case set covers every legitimate branch/call "
               "target. Landing here means either (a) a same-binary RET "
               "popped a mid-instruction IP (stack corruption upstream of "
               "the pop), or (b) a cross-binary RET that find_file_mapping "
               "couldn't route (overlay-swap timing or unmapped target). "
               "Trace lifecycle.log/trace.tail.log backward from this "
               "crash to identify the corrupting push or unmapped chunk.\n");

  if (overlap_count > 0) {
    UNHPC_APPEND("[BUG]   chunk-swap suspect: lifecycle.log LOAD events "
                 "for base 0x%05X show which chunk was active when the "
                 "target was computed. The fix is in the loader/chunk-"
                 "attribution layer.\n",
                 picked ? picked->base : 0);
  }

  return (int)(n < cap ? n : cap - 1);
}

#undef UNHPC_APPEND

/* Cache the segment register value associated with the binary containing
 * ``addr``. Called from authoritative cs-setting transfer paths (lcall, far
 * jump, iret restoration). dispatch_via_binary later uses this to set
 * cpu.r_cs when routing near-transfers into the binary's translated code. */
static void record_binary_cs(uint32_t addr, uint16_t seg) {
  FileMapping *fm = find_file_mapping_mut(addr);
  if (fm) fm->canonical_cs = seg;
}

/* Choose the code segment to run a binary's translated code under when routing
 * a (linear addr -> file_off) dispatch.
 *
 * The LIVE cpu.r_cs, set by the transfer that produced this address (retf, far
 * jmp, iret), is authoritative whenever it already places the code in a valid
 * 16-bit IP window -- i.e. ``0 <= addr - (cs<<4) < 0x10000``. A multi-segment
 * binary (a relocated DOS EXE) legitimately runs the SAME
 * file mapping under several cs values; clobbering the live cs with a single
 * cached ``canonical_cs`` would force one segment's base onto code that the
 * program entered under a different, equally-valid segment, composing every
 * subsequent near-return IP (cs<<4 + ip) at the wrong linear address.
 *
 * canonical_cs is only consulted as a FALLBACK: when the live cs is stale
 * (out of range for this addr -- e.g. a near-ret left binary A's cs in place
 * while returning into binary B), use the cached segment the binary was last
 * authoritatively entered under. This keeps the single-segment overlay binaries
 * (overlay chunks) working while not corrupting multi-segment EXEs. */
static void set_dispatch_cs(const FileMapping *fm, uint32_t addr) {
  if (!fm || !fm->canonical_cs) return;
  uint32_t live_off = addr - ((uint32_t)cpu.r_cs << 4);
  if (live_off < 0x10000u) return; /* live cs already valid -- trust it */
  cpu.r_cs = fm->canonical_cs;
}


/* Move file_mappings entries that lie inside one of two swapped regions to the
 * corresponding position in the other region. Called by the shim that
 * implements game-side overlay/bank-switch loops — those move raw bytes between
 * memory regions, and our (linear_addr -> file_offset) lookup must follow the
 * bytes or dispatch_via_binary will use the wrong chunk's view (the original
 * "before swap" chunk) when looking up the destination address.
 *
 * A mapping that STRADDLES a swap-region boundary is a normal, faithful state:
 * a single file LOAD can be larger than the swap window (e.g. a 0x1B3D-byte
 * resource bank-switched a 0x1000 sub-window at a time), so the swap moves only
 * PART of that mapping. Model it the way the hardware does -- split the mapping
 * at the region boundaries so each fragment is wholly inside A, wholly inside B,
 * or wholly outside, then relocate the in-window fragments to the other region.
 * (Earlier this aborted on a straddle, assuming swaps were always chunk-aligned;
 * that invariant does not hold for sub-chunk bank-switches and surfaced as the
 * `mapswap_straddle` crash.) */
static void swap_file_mappings_in_regions(uint32_t a_start, uint32_t b_start,
                                          uint32_t len) {
  if (len == 0) return;
  uint32_t a_end = a_start + len;
  uint32_t b_end = b_start + len;

  /* Phase 1 -- cut every mapping that crosses a region boundary so no fragment
   * straddles. The four boundaries are processed in turn; right-pieces appended
   * for an earlier boundary start exactly AT that boundary, so they never need
   * re-cutting there but are reconsidered (via the refreshed count) at later
   * boundaries. Mirrors evict_or_shrink_for_load's in-place left + queued right
   * split, including dropping the (diagnostic-only) byte cache on the pieces. */
  const uint32_t cuts[4] = {a_start, a_end, b_start, b_end};
  for (int ci = 0; ci < 4; ++ci) {
    uint32_t cut = cuts[ci];
    size_t n = file_mapping_count;
    for (size_t i = 0; i < n; ++i) {
      uint32_t base = file_mappings[i].base;
      uint32_t end = base + (uint32_t)file_mappings[i].len;
      if (base < cut && cut < end) {
        if (file_mapping_count >= MAX_FILE_MAPPINGS) {
          shim_log_crash("[BUG] swap_file_mappings: file_mappings full while "
                         "splitting a straddling mapping at boundary 0x%05X "
                         "-- raise MAX_FILE_MAPPINGS\n", cut);
          shim_flush_all_streams();
          abort();
        }
        FileMapping *e = &file_mappings[i];
        FileMapping right = *e;
        right.path = e->path ? strdup(e->path) : NULL;
        right.base = cut;
        right.len = (size_t)(end - cut);
        right.file_offset = e->file_offset + (size_t)(cut - base);
        right.data = NULL;
        free(e->data);
        e->data = NULL;
        e->len = (size_t)(cut - base);
        file_mappings[file_mapping_count++] = right;
      }
    }
  }

  /* Phase 2 -- every fragment is now wholly inside a region or wholly outside.
   * Relocate in-A fragments to B and in-B fragments to A. Each entry is visited
   * once and moved at most once, so a just-moved A->B fragment is not re-seen. */
  size_t moved = 0;
  for (size_t i = 0; i < file_mapping_count; ++i) {
    uint32_t base = file_mappings[i].base;
    uint32_t end = base + (uint32_t)file_mappings[i].len;
    if (base >= a_start && end <= a_end) {
      file_mappings[i].base = base - a_start + b_start;
      moved++;
    } else if (base >= b_start && end <= b_end) {
      file_mappings[i].base = base - b_start + a_start;
      moved++;
    }
  }
  if (moved) {
    lifecycle_log(
        "MAPSWAP relocated %zu mappings between regions 0x%05X..0x%05X and "
        "0x%05X..0x%05X (len 0x%X)\n",
        moved, a_start, a_end, b_start, b_end, len);
  }
}

/* Word-sized memory swap loop shim — replaces the 5-instruction
 * lodsw/mov-es:[di]/stosw/mov-[si-2],dx/loop pattern in one shot.
 *
 * Game-side semantics: for ``count`` iterations, exchange word at
 * [ds:si] with word at [es:di], advancing both si and di by ``±2``
 * (sign per DF). Caller is responsible for updating si/di/cx/ZF to
 * post-loop state — see the IR-to-C swap-loop pattern emitter.
 *
 * Why a dedicated shim: the loop is the game's bank-switch primitive
 * that moves whole overlay regions between RAM windows without going
 * through a DOS read. Our file_mappings (the linear -> file lookup
 * dispatch_via_binary depends on) only updates at LOAD time, so after
 * a bank-switch the entry for the destination still points at the
 * pre-switch chunk's file_offset — and dispatch_via_binary then sends
 * CALLs into the wrong dispatcher, which has no case for what the
 * post-switch bytes correspond to. Calling swap_file_mappings here
 * keeps the lookup truthful. */
void shim_swap_regions_w(uint16_t es_seg, uint16_t di_off,
                         uint16_t ds_seg, uint16_t si_off,
                         uint16_t count, int df) {
  if (count == 0) return;
  uint32_t es_lin = linear_addr(es_seg, di_off);
  uint32_t ds_lin = linear_addr(ds_seg, si_off);
  uint32_t bytes = (uint32_t)count * 2;
  uint32_t es_start, ds_start;
  if (df == 0) {
    /* Forward: byte range is [start, start + count*2). */
    es_start = es_lin;
    ds_start = ds_lin;
  } else {
    /* Reverse (DF=1): si/di descend each iteration, so the first byte
     * accessed is at the high end of the window. Walk back to the low
     * end so the byte swap covers the same range either way. */
    es_start = (es_lin + 2 - bytes) & 0xFFFFF;
    ds_start = (ds_lin + 2 - bytes) & 0xFFFFF;
  }
  for (uint32_t i = 0; i < bytes; ++i) {
    uint32_t ea = mask_addr(es_start + i);
    uint32_t da = mask_addr(ds_start + i);
    uint8_t tmp_a = virtual_memory[ea];
    uint8_t tmp_b = virtual_memory[da];
    /* SWAP_W bypasses memb_write_impl, so route both directions through
     * write_watch_log explicitly — otherwise an overlay swap that crosses
     * a watched range corrupts it invisibly. */
    write_watch_log(ea, 1, tmp_b, __FILE__, __func__, __LINE__);
    write_watch_log(da, 1, tmp_a, __FILE__, __func__, __LINE__);
    virtual_memory[ea] = tmp_b;
    virtual_memory[da] = tmp_a;
  }
  lifecycle_log(
      "SWAP_W A=0x%05X..0x%05X B=0x%05X..0x%05X bytes=0x%X df=%d\n",
      ds_start, ds_start + bytes, es_start, es_start + bytes, bytes, df);
  swap_file_mappings_in_regions(ds_start, es_start, bytes);
  /* The bank-switch swapped the CODE in both windows. swap_file_mappings_in_
   * regions kept the dispatch file_mapping lookup truthful; do the same for the JIT:
   * drop any chunk decoded in either window, else a stale chunk keeps running
   * the pre-swap overlay's code (and reads the now-swapped-in data) -- the
   * recurring overlay-reshuffle crash (e.g. the entity-dispatch overlay + its
   * jump table getting bank-switched). The per-byte write_watch_log above does
   * NOT invalidate (it bypasses memb_write_impl), so it must be done here. */
  shim_jit_invalidate_code_range(ds_start, bytes);
  shim_jit_invalidate_code_range(es_start, bytes);
}

static void lifecycle_log_dispatch(const char *kind, uint32_t addr) {
  /* Suppress dispatch events that happen inside an ISR (timer chain etc.)
   * unless the target is unmapped — those are the actually-interesting
   * failure cases. The timer ISR alone fires 50k+ dispatch events/sec, which
   * crowds out everything else; skipping them gives the ring tail real
   * signal across the run. LOAD events bypass this and always log. */
  const FileMapping *fm = find_file_mapping(addr);
  const char *bn = "<unmapped>";
  size_t off_in = 0;
  if (fm && fm->path) {
    bn = strrchr(fm->path, '/');
    bn = bn ? bn + 1 : fm->path;
    off_in = fm->file_offset + (addr - fm->base);
  }
  int unmapped = (fm == NULL);
  if (isr_depth > 0 && !unmapped) return;
  /* Reconstruction naming layer: record this transfer's target in the alias
   * registry and render its user-assigned alias when present. Seed only on
   * call-like transfers — CALL/LCALL/LJMP land on routine entries, so they are
   * the "functions" worth naming; a near JMP is usually an intra-function
   * branch (rendered if already named, but not seeded). See aliasreg_alias. */
  const char *alias = NULL;
  char disp[256];                                      /* alias rendered with args */
  int call_like = (kind[0] == 'C' || kind[0] == 'L');  /* CALL / LCALL / LJMP */
  if (fm && fm->path) {
    char idbuf[160];
    snprintf(idbuf, sizeof(idbuf), "%s+0x%zX", bn, off_in);
    alias = aliasreg_alias(idbuf, call_like);
  }
  if (alias) render_alias_with_args(alias, disp, sizeof(disp));  /* "name(arg=LABEL,..)" */
  /* Live flow view: echo call-like transfers to the trace stream so a RUNNING
   * session shows the call flow in the assigned names (an empty alias renders
   * the bare stable identity, already clearer than nothing). Honors the
   * --verbose gate via shim_log_stdout; the ring line below keeps the full
   * register detail for
   * the post-mortem lifecycle.log. */
  if (call_like) {
    cg_record(((uint32_t)cs << 4) + ip, addr);  /* persistent call-graph edge */
    if (alias)
      shim_log_stdout("Flow: %s 0x%05X -> %s (%s+0x%zX)  from=%04X:%04X\n", kind,
                      addr, disp, bn, off_in, cs, ip);
    else
      shim_log_stdout("Flow: %s 0x%05X -> %s+0x%zX  from=%04X:%04X\n", kind, addr,
                      bn, off_in, cs, ip);
  }
  /* Include the indirect-source registers so post-mortem can recover what
   * the source instruction (e.g. ``call ax`` with ax loaded from a
   * function-pointer table at ds:[bx+disp]) actually read. State.txt
   * captures registers at abort time, but bx/si/ax may have been modified
   * by the dispatcher itself before abort — these are the values at the
   * moment of the indirect transfer. */
  if (alias) {
    lifecycle_log(
        "%s 0x%05X -> %s (%s+0x%zX)  bx=%04X si=%04X ax=%04X ds=%04X cs=%04X ip=%04X\n",
        kind, addr, disp, bn, off_in, bx, si, ax, ds, cs, ip);
  } else {
    lifecycle_log(
        "%s 0x%05X -> %s+0x%zX  bx=%04X si=%04X ax=%04X ds=%04X cs=%04X ip=%04X\n",
        kind, addr, bn, off_in, bx, si, ax, ds, cs, ip);
  }
}

/* ===== Function alias registry (reconstruction naming layer) ==============
 *
 * Lets the dispatch flow log read in human names instead of address-derived
 * ones, so a bottom-up reconstruction's control flow becomes legible. The hook
 * is lifecycle_log_dispatch (below): it already resolves every CALL/JMP target
 * to its STABLE "<binary>+0x<file_off>" origin (the form the user reverse-
 * engineers against, e.g. "an overlay archive+0xB198"), which is invariant across
 * cs-aliases and content-keyed chunk re-decodes because it keys on the origin
 * file, not the chunk's load address.
 *
 *  - Registry: build/<game>/aliases.json, a flat { "<id>": "<alias>" } map the
 *    user fills in over time. Loaded once. Every call-like target reached for
 *    the first time is appended with an empty alias -- a naming worklist that
 *    grows along the real flow. A non-empty alias is rendered in the flow log;
 *    an empty one renders the bare identity (unchanged from before). Editing the
 *    file and re-running shows the new names with NO rebuild: the alias is
 *    applied at log time, never baked into a chunk, so the JIT chunk cache and
 *    the committed artifact hashes are untouched (this is a runtime-only change).
 *
 * aliasreg_alias() is forward-declared up by the lifecycle_log_dispatch
 * prototype so the dispatch logger (defined earlier) can call it. */
#define ALIASREG_MAX_ENTRIES 8192
typedef struct {
  char *id;     /* "<binary>+0x<file_off>" */
  char *alias;  /* user-assigned; "" = seeded but unnamed */
} AliasRegEntry;
static AliasRegEntry aliasreg_entries[ALIASREG_MAX_ENTRIES];
static int  aliasreg_count;
static int  aliasreg_loaded;   /* 0 until first load attempt */
static char aliasreg_path[1024];

/* Annotation files (aliases/regions/vars.json) are durable METADATA, not build
 * artifacts -- they live in the committed games/<key>/ bundle (survives a
 * `build/` wipe, can be checked in), with a build/<key>/ fallback if that dir is
 * absent. Path = <SAISEI_REPO_ROOT>/games/<key>/<name>, where <key> is parsed
 * from SAISEI_JIT_DIR (".../build/<key>/jit"). Used for both read and auto-save,
 * so seeds accumulate in the committed bundle. */
static void annot_file_path(const char *name, char *out, size_t cap) {
  const char *jit = getenv("SAISEI_JIT_DIR");
  const char *repo = getenv("SAISEI_REPO_ROOT");
  if (repo && repo[0] && jit && jit[0]) {
    char tmp[1024];
    snprintf(tmp, sizeof(tmp), "%s", jit);
    char *s = strrchr(tmp, '/');           /* trailing "/jit" */
    if (s) {
      *s = '\0';                           /* now ".../build/<key>" */
      const char *key = strrchr(tmp, '/');
      key = key ? key + 1 : tmp;           /* "<key>" */
      char dir[1100];
      snprintf(dir, sizeof(dir), "%s/games/%s", repo, key);
      if (access(dir, F_OK) == 0) {        /* committed bundle exists -> durable home */
        snprintf(out, cap, "%s/%s", dir, name);
        return;
      }
    }
  }
  if (jit && jit[0]) snprintf(out, cap, "%s/../%s", jit, name);  /* build/ fallback */
  else snprintf(out, cap, "%s", name);
}

static void aliasreg_compute_path(void) {
  annot_file_path("aliases.json", aliasreg_path, sizeof(aliasreg_path));
}

static AliasRegEntry *aliasreg_find(const char *id) {
  for (int i = 0; i < aliasreg_count; ++i)
    if (strcmp(aliasreg_entries[i].id, id) == 0) return &aliasreg_entries[i];
  return NULL;
}

/* Read one JSON string starting at the opening quote `p`; returns the position
 * just past the closing quote. Handles \" and \\ -- all we ever emit. */
static const char *aliasreg_read_string(const char *p, char *out, size_t cap) {
  size_t n = 0;
  if (*p == '"') ++p;
  while (*p && *p != '"') {
    char c = *p++;
    if (c == '\\' && *p) c = *p++;
    if (n + 1 < cap) out[n++] = c;
  }
  out[n] = '\0';
  if (*p == '"') ++p;
  return p;
}

/* Read an annotation VALUE -- either a bare string "name", or an object
 * { ... "name": "X" ... } carrying extra fields (confidence, note, ...) -- into
 * `name`, and return the position just past the value. Lets every annotation
 * file be enriched with metadata while the runtime only needs the name.
 * NOTE: aliasreg_save (functions only) re-serializes as a bare string, so on a
 * later auto-seed an object entry's extra fields flatten to just the name; add
 * rich notes once the function set has stopped growing (or keep them in the
 * never-auto-saved regions.json / vars.json). */
static const char *aliasreg_read_value(const char *p, char *name, size_t cap) {
  name[0] = '\0';
  while (*p == ' ' || *p == '\t' || *p == '\n' || *p == '\r') ++p;
  if (*p == '"') return aliasreg_read_string(p, name, cap);
  if (*p == '{') {
    const char *q = p + 1;
    int depth = 1;
    while (*q && depth) { if (*q == '{') ++depth; else if (*q == '}') { --depth; if (!depth) break; } ++q; }
    const char *end = (*q == '}') ? q + 1 : q;
    for (const char *r = p; r + 6 < end; ++r) {
      if (r[0]=='"'&&r[1]=='n'&&r[2]=='a'&&r[3]=='m'&&r[4]=='e'&&r[5]=='"') {
        const char *s = r + 6;
        while (s < end && *s != '"') ++s;  /* skip to the value's open quote */
        if (s < end) aliasreg_read_string(s, name, cap);
        break;
      }
    }
    return end;
  }
  while (*p && *p != ',' && *p != '}') ++p;  /* bare token: skip */
  return p;
}

/* Parse the flat {"k":"v",...} map we own. Tolerant of layout; anything
 * malformed is skipped rather than aborting -- the user hand-edits this file. */
static void aliasreg_load(void) {
  if (aliasreg_loaded) return;
  aliasreg_loaded = 1;
  aliasreg_compute_path();
  FILE *fp = fopen(aliasreg_path, "rb");
  if (!fp) return;
  fseek(fp, 0, SEEK_END);
  long sz = ftell(fp);
  if (sz <= 0 || sz > (16L << 20)) { fclose(fp); return; }
  rewind(fp);
  char *buf = (char *)malloc((size_t)sz + 1);
  if (!buf) { fclose(fp); return; }
  size_t rd = fread(buf, 1, (size_t)sz, fp);
  fclose(fp);
  buf[rd] = '\0';
  const char *p = buf;
  char key[160], val[96];
  while (*p && aliasreg_count < ALIASREG_MAX_ENTRIES) {
    while (*p && *p != '"') ++p;            /* to key open-quote */
    if (!*p) break;
    p = aliasreg_read_string(p, key, sizeof(key));
    while (*p && *p != ':' && *p != '"') ++p;
    if (*p != ':') continue;                /* not a key:value pair */
    ++p;                                    /* past ':' */
    p = aliasreg_read_value(p, val, sizeof(val));  /* string | object{name} */
    if (key[0] && !aliasreg_find(key)) {
      char *kd = strdup(key), *vd = strdup(val);
      if (kd && vd) {
        aliasreg_entries[aliasreg_count].id = kd;
        aliasreg_entries[aliasreg_count].alias = vd;
        aliasreg_count++;
      } else { free(kd); free(vd); }
    }
  }
  free(buf);
}

static void aliasreg_write_escaped(FILE *fp, const char *s) {
  for (; s && *s; ++s) {
    if (*s == '"' || *s == '\\') fputc('\\', fp);
    fputc(*s, fp);
  }
}

/* Rewrite the registry from memory via a temp file + rename, so a crash mid-
 * write can never truncate the user's named entries. Called on each new seed;
 * the entry count is small (the reached-function set), so this stays cheap. */
static void aliasreg_save(void) {
  char tmp[1100];
  snprintf(tmp, sizeof(tmp), "%s.tmp", aliasreg_path);
  FILE *fp = fopen(tmp, "wb");
  if (!fp) return;
  fputs("{\n", fp);
  for (int i = 0; i < aliasreg_count; ++i) {
    fputs("  \"", fp);
    aliasreg_write_escaped(fp, aliasreg_entries[i].id);
    fputs("\": \"", fp);
    aliasreg_write_escaped(fp, aliasreg_entries[i].alias);
    fputs(i + 1 < aliasreg_count ? "\",\n" : "\"\n", fp);
  }
  fputs("}\n", fp);
  fclose(fp);
  rename(tmp, aliasreg_path);
}

/* Look up the alias for identity `id` ("<binary>+0x<off>"). When `seed` is set
 * and `id` is new, append it with an empty alias (a worklist entry) and persist.
 * Returns the alias once the user has named it, else NULL (the caller renders the
 * bare identity). Seeding is skipped inside an ISR -- file I/O there is unwelcome,
 * and the address will be seeded the next time it is reached at base level. */
static const char *aliasreg_alias(const char *id, int seed) {
  if (!id || !id[0]) return NULL;
  aliasreg_load();
  AliasRegEntry *e = aliasreg_find(id);
  if (!e && seed && isr_depth == 0 && aliasreg_count < ALIASREG_MAX_ENTRIES) {
    char *kd = strdup(id), *vd = strdup("");
    if (kd && vd) {
      e = &aliasreg_entries[aliasreg_count++];
      e->id = kd; e->alias = vd;
      aliasreg_save();
    } else { free(kd); free(vd); }
  }
  return (e && e->alias && e->alias[0]) ? e->alias : NULL;
}

/* ===== Named memory regions (annotation layer: addresses -> region names) ===
 * A flat { "0xLO-0xHI": "name" } map in build/<game>/regions.json names the
 * volatile/computed buffers that have NO useful file mapping -- the framebuffer,
 * the off-screen VRAM sprite cache, the image work-segment, the decode scratch.
 * name_addr() then renders any linear address as "region+0xoff" (named buffer)
 * | "binary+0xoff" (file-mapped code/resource) | raw, so the flow log and WATCHW
 * read in names instead of bare hex. Runtime-only; edit regions.json + rerun. */
#define ALIASREG_MAX_REGIONS 64
typedef struct { uint32_t lo, hi; char *name; } AliasRegRegion;
static AliasRegRegion aliasreg_regions[ALIASREG_MAX_REGIONS];
static int aliasreg_region_count;
static int aliasreg_regions_loaded;

static void aliasreg_regions_load(void) {
  if (aliasreg_regions_loaded) return;
  aliasreg_regions_loaded = 1;
  char path[1100];
  annot_file_path("regions.json", path, sizeof(path));
  FILE *fp = fopen(path, "rb");
  if (!fp) return;
  fseek(fp, 0, SEEK_END);
  long sz = ftell(fp);
  if (sz <= 0 || sz > (1L << 20)) { fclose(fp); return; }
  rewind(fp);
  char *buf = (char *)malloc((size_t)sz + 1);
  if (!buf) { fclose(fp); return; }
  size_t rd = fread(buf, 1, (size_t)sz, fp);
  fclose(fp);
  buf[rd] = '\0';
  const char *p = buf;
  char key[64], val[64];
  while (*p && aliasreg_region_count < ALIASREG_MAX_REGIONS) {
    while (*p && *p != '"') ++p;
    if (!*p) break;
    p = aliasreg_read_string(p, key, sizeof(key));
    while (*p && *p != ':' && *p != '}') ++p;
    if (*p != ':') continue;
    ++p;                                    /* past ':' */
    p = aliasreg_read_value(p, val, sizeof(val));
    unsigned long lo = 0, hi = 0;  /* key form: "0xLO-0xHI" */
    if (sscanf(key, "%lx-%lx", &lo, &hi) == 2 && hi >= lo && val[0]) {
      char *nd = strdup(val);
      if (nd) {
        aliasreg_regions[aliasreg_region_count].lo = (uint32_t)lo;
        aliasreg_regions[aliasreg_region_count].hi = (uint32_t)hi;
        aliasreg_regions[aliasreg_region_count].name = nd;
        aliasreg_region_count++;
      }
    }
  }
  free(buf);
}

/* Render `lin` as "region+0xoff" | "binary+0xoff" | "0xlin" into `out`.
 * Uses a QUIET file-mapping scan (no "not mapped" logging) so it is safe to call
 * on hot/arbitrary addresses (stack, etc.). */
static const char *name_addr(uint32_t lin, char *out, size_t cap) {
  aliasreg_regions_load();
  for (int i = 0; i < aliasreg_region_count; ++i)
    if (lin >= aliasreg_regions[i].lo && lin <= aliasreg_regions[i].hi) {
      snprintf(out, cap, "%s+0x%X", aliasreg_regions[i].name,
               lin - aliasreg_regions[i].lo);
      return out;
    }
  for (ssize_t i = (ssize_t)file_mapping_count - 1; i >= 0; --i) {
    uint32_t base = file_mappings[i].base;
    if (lin >= base && lin < base + file_mappings[i].len && file_mappings[i].path) {
      const char *bn = strrchr(file_mappings[i].path, '/');
      bn = bn ? bn + 1 : file_mappings[i].path;
      snprintf(out, cap, "%s+0x%zX", bn,
               (size_t)(file_mappings[i].file_offset + (lin - base)));
      return out;
    }
  }
  snprintf(out, cap, "0x%05X", lin);
  return out;
}

/* ===== Named data variables (change-watch) =================================
 * build/<game>/vars.json: flat { "0xADDR:SIZE": "name" } (SIZE = 1|2 bytes;
 * ":SIZE" optional, default 1). On a write to a named address whose value
 * CHANGED since last seen, emit "VAR name: old -> new" -- so a state machine's
 * variables read in names and you see only the meaningful transitions, not every
 * write. Write-path only (hooked in write_watch_log, the central per-write sink),
 * so NO hot read-path cost; an [lo,hi] window makes unwatched writes ~free.
 * Runtime-only; edit vars.json + rerun. */
#define ALIASREG_MAX_VARS 256
typedef struct {
  uint32_t addr; uint8_t size; char *name; uint32_t last; int seen; int reports;
  char *origin_bin; uint32_t origin_off;  /* "binary+off" vars: addr resolved at runtime */
} AliasRegVar;
static AliasRegVar aliasreg_vars[ALIASREG_MAX_VARS];
static int aliasreg_var_count;
static int aliasreg_vars_loaded;
static int aliasreg_has_origin_vars;
static uint32_t aliasreg_var_lo = 0xFFFFFFFFu, aliasreg_var_hi = 0;

static void aliasreg_vars_load(void) {
  if (aliasreg_vars_loaded) return;
  aliasreg_vars_loaded = 1;
  char path[1100];
  annot_file_path("vars.json", path, sizeof(path));
  FILE *fp = fopen(path, "rb");
  if (!fp) return;
  fseek(fp, 0, SEEK_END);
  long sz = ftell(fp);
  if (sz <= 0 || sz > (1L << 20)) { fclose(fp); return; }
  rewind(fp);
  char *buf = (char *)malloc((size_t)sz + 1);
  if (!buf) { fclose(fp); return; }
  size_t rd = fread(buf, 1, (size_t)sz, fp);
  fclose(fp);
  buf[rd] = '\0';
  const char *p = buf;
  char key[64], val[64];
  while (*p && aliasreg_var_count < ALIASREG_MAX_VARS) {
    while (*p && *p != '"') ++p;
    if (!*p) break;
    p = aliasreg_read_string(p, key, sizeof(key));
    while (*p && *p != ':' && *p != '}') ++p;
    if (*p != ':') continue;
    ++p;                                    /* past ':' */
    p = aliasreg_read_value(p, val, sizeof(val));
    if (val[0]) {
      /* key: "0xLINEAR:SIZE" OR "binary.ext+0xOFF:SIZE" (origin-relative, resolved
       * to a linear addr at runtime so it survives overlay remaps). SIZE optional. */
      char binbuf[48]; unsigned long off = 0, s = 1; uint32_t lin = 0; int is_origin = 0;
      const char *plus = strstr(key, "+0x");
      if (plus && !(key[0] == '0' && (key[1] == 'x' || key[1] == 'X'))) {
        size_t blen = (size_t)(plus - key);
        if (blen > 0 && blen < sizeof(binbuf)) {
          memcpy(binbuf, key, blen); binbuf[blen] = '\0';
          if (sscanf(plus + 3, "%lx:%lu", &off, &s) >= 1) is_origin = 1;
        }
      } else {
        unsigned long a = 0;
        if (sscanf(key, "%lx:%lu", &a, &s) >= 1) lin = (uint32_t)a;
      }
      if (s != 1 && s != 2) s = 1;
      if (is_origin || lin) {
        char *nd = strdup(val);
        if (nd) {
          AliasRegVar *v = &aliasreg_vars[aliasreg_var_count++];
          v->size = (uint8_t)s; v->name = nd; v->last = 0; v->seen = 0;
          if (is_origin) {
            v->origin_bin = strdup(binbuf); v->origin_off = (uint32_t)off; v->addr = 0;
            aliasreg_has_origin_vars = 1;
          } else {
            v->origin_bin = NULL; v->origin_off = 0; v->addr = lin;
            if (v->addr < aliasreg_var_lo) aliasreg_var_lo = v->addr;
            if (v->addr + v->size - 1 > aliasreg_var_hi) aliasreg_var_hi = v->addr + v->size - 1;
          }
        }
      }
    }
  }
  free(buf);
}

/* Resolve a "binary+off" origin var to its current linear address via
 * file_mappings (newest covering mapping wins), so overlay-relative vars track
 * remaps instead of a frozen linear address. */
static uint32_t resolve_origin_to_linear(const char *bin, uint32_t off) {
  for (int i = (int)file_mapping_count - 1; i >= 0; --i) {
    FileMapping *m = &file_mappings[i];
    if (!m->path) continue;
    const char *bn = strrchr(m->path, '/');
    bn = bn ? bn + 1 : m->path;
    if (strcmp(bn, bin) != 0) continue;
    if ((size_t)off >= m->file_offset && (size_t)off < m->file_offset + m->len)
      return m->base + (uint32_t)((size_t)off - m->file_offset);
  }
  return 0;  /* binary not mapped yet */
}

/* Re-resolve all origin vars and recompute the [lo,hi] window over every var.
 * Called when the mapping set changes; a moved resolution resets `seen`. */
static void aliasreg_vars_resolve(void) {
  aliasreg_var_lo = 0xFFFFFFFFu;
  aliasreg_var_hi = 0;
  for (int i = 0; i < aliasreg_var_count; ++i) {
    AliasRegVar *v = &aliasreg_vars[i];
    if (v->origin_bin) {
      uint32_t lin = resolve_origin_to_linear(v->origin_bin, v->origin_off);
      if (lin && lin != v->addr) { v->addr = lin; v->seen = 0; v->reports = 0; }  /* (re)mapped */
      if (!lin) continue;                                          /* not mapped yet */
    }
    if (v->addr) {
      if (v->addr < aliasreg_var_lo) aliasreg_var_lo = v->addr;
      if (v->addr + v->size - 1 > aliasreg_var_hi) aliasreg_var_hi = v->addr + v->size - 1;
    }
  }
}

/* Report a value change of a named var. Called from write_watch_log only for
 * addresses inside [aliasreg_var_lo, aliasreg_var_hi]. */
static void aliasreg_var_write(uint32_t addr, uint8_t size, uint32_t value) {
  (void)size;
  for (int i = 0; i < aliasreg_var_count; ++i) {
    AliasRegVar *v = &aliasreg_vars[i];
    if (v->addr != addr) continue;  /* match the var's start address */
    uint32_t nv = (v->size == 1) ? (value & 0xFF) : (value & 0xFFFF);
    if (v->seen && nv == v->last) return;  /* unchanged -> stay quiet */
    if (v->reports < 50) {  /* cap: high-freq vars sample rather than flood */
      if (v->seen)
        shim_log_stdout("VAR %s: 0x%X -> 0x%X  (cs:ip=%04X:%04X)\n", v->name,
                        v->last, nv, cs, ip);
      else
        shim_log_stdout("VAR %s = 0x%X  (first seen, cs:ip=%04X:%04X)\n", v->name,
                        nv, cs, ip);
      if (++v->reports == 50)
        shim_log_stdout("VAR %s: further changes suppressed (cap)\n", v->name);
    }
    v->last = nv; v->seen = 1;
    return;
  }
}

/* ===== Constants / enums on call args =====================================
 * An alias may carry an argument spec: "name(reg:argname@enum, reg:argname, ...)".
 * render_alias_with_args reads the call-time registers + the enums.json tables to
 * print "name(argname=LABEL|0xval, ...)" so a call reads like
 * load_resource(res_id=PLAYER) instead of CALL load_resource. A plain "name"
 * (no '(') passes through unchanged. enums.json: flat { "<enum>:0xVALUE":"LABEL" }.
 * Runtime-only; edit aliases.json/enums.json + rerun. */
#define ALIASREG_MAX_ENUMS 2048
typedef struct { char *ename; uint32_t value; char *label; } AliasRegEnum;
static AliasRegEnum aliasreg_enums[ALIASREG_MAX_ENUMS];
static int aliasreg_enum_count;
static int aliasreg_enums_loaded;

static void aliasreg_enums_load(void) {
  if (aliasreg_enums_loaded) return;
  aliasreg_enums_loaded = 1;
  char path[1100];
  annot_file_path("enums.json", path, sizeof(path));
  FILE *fp = fopen(path, "rb");
  if (!fp) return;
  fseek(fp, 0, SEEK_END);
  long sz = ftell(fp);
  if (sz <= 0 || sz > (4L << 20)) { fclose(fp); return; }
  rewind(fp);
  char *buf = (char *)malloc((size_t)sz + 1);
  if (!buf) { fclose(fp); return; }
  size_t rd = fread(buf, 1, (size_t)sz, fp);
  fclose(fp);
  buf[rd] = '\0';
  const char *p = buf;
  char key[80], val[64];
  while (*p && aliasreg_enum_count < ALIASREG_MAX_ENUMS) {
    while (*p && *p != '"') ++p;
    if (!*p) break;
    p = aliasreg_read_string(p, key, sizeof(key));
    while (*p && *p != ':' && *p != '}') ++p;
    if (*p != ':') continue;
    ++p;
    p = aliasreg_read_value(p, val, sizeof(val));
    char *colon = strrchr(key, ':');  /* key form "<enum>:0xVALUE" */
    if (colon && val[0]) {
      *colon = '\0';
      unsigned long v = strtoul(colon + 1, NULL, 0);
      char *en = strdup(key), *lb = strdup(val);
      if (en && lb) {
        aliasreg_enums[aliasreg_enum_count].ename = en;
        aliasreg_enums[aliasreg_enum_count].value = (uint32_t)v;
        aliasreg_enums[aliasreg_enum_count].label = lb;
        aliasreg_enum_count++;
      } else { free(en); free(lb); }
    }
  }
  free(buf);
}

static const char *aliasreg_enum_label(const char *ename, uint32_t value) {
  for (int i = 0; i < aliasreg_enum_count; ++i)
    if (aliasreg_enums[i].value == value &&
        strcmp(aliasreg_enums[i].ename, ename) == 0)
      return aliasreg_enums[i].label;
  return NULL;
}

/* Value of a named 8/16-bit register at the current call. */
static uint32_t aliasreg_reg_value(const char *r) {
  if (!strcmp(r,"ax")) return ax;  if (!strcmp(r,"bx")) return bx;
  if (!strcmp(r,"cx")) return cx;  if (!strcmp(r,"dx")) return dx;
  if (!strcmp(r,"si")) return si;  if (!strcmp(r,"di")) return di;
  if (!strcmp(r,"bp")) return bp;  if (!strcmp(r,"ds")) return ds;
  if (!strcmp(r,"es")) return es;  if (!strcmp(r,"cs")) return cs;
  if (!strcmp(r,"al")) return ax & 0xFF;  if (!strcmp(r,"ah")) return (ax >> 8) & 0xFF;
  if (!strcmp(r,"bl")) return bx & 0xFF;  if (!strcmp(r,"bh")) return (bx >> 8) & 0xFF;
  if (!strcmp(r,"cl")) return cx & 0xFF;  if (!strcmp(r,"ch")) return (cx >> 8) & 0xFF;
  if (!strcmp(r,"dl")) return dx & 0xFF;  if (!strcmp(r,"dh")) return (dx >> 8) & 0xFF;
  return 0;
}

static const char *render_alias_with_args(const char *alias, char *out, size_t cap) {
  const char *lp = strchr(alias, '(');
  if (!lp) { snprintf(out, cap, "%s", alias); return out; }
  aliasreg_enums_load();
  size_t n = 0;
  for (const char *c = alias; c < lp && n + 1 < cap; ++c) out[n++] = *c;  /* name */
  if (n + 1 < cap) out[n++] = '(';
  const char *rp = strchr(lp, ')');
  const char *aend = rp ? rp : lp + strlen(lp);
  const char *a = lp + 1;
  int first = 1;
  while (a < aend) {
    const char *comma = a;
    while (comma < aend && *comma != ',') ++comma;  /* arg token [a, comma) */
    char reg[8] = "", argn[24] = "", en[24] = "";
    const char *sep = a;
    while (sep < comma && *sep != ':' && *sep != '@') ++sep;
    size_t k = 0;
    for (const char *r = a; r < sep && k < sizeof(reg) - 1; ++r) reg[k++] = *r;
    reg[k] = '\0';
    if (sep < comma && *sep == ':') {
      ++sep;
      const char *at = sep;
      while (at < comma && *at != '@') ++at;
      k = 0;
      for (const char *r = sep; r < at && k < sizeof(argn) - 1; ++r) argn[k++] = *r;
      argn[k] = '\0';
      if (at < comma && *at == '@') {
        ++at; k = 0;
        for (const char *r = at; r < comma && k < sizeof(en) - 1; ++r) en[k++] = *r;
        en[k] = '\0';
      }
    }
    uint32_t v = aliasreg_reg_value(reg);
    const char *lab = en[0] ? aliasreg_enum_label(en, v) : NULL;
    if (!first && n + 2 < cap) { out[n++] = ','; out[n++] = ' '; }
    first = 0;
    if (argn[0]) {
      int w = snprintf(out + n, n < cap ? cap - n : 0, "%s=", argn);
      if (w > 0) n += (size_t)w;
      if (n >= cap) n = cap - 1;
    }
    {
      int w = lab ? snprintf(out + n, n < cap ? cap - n : 0, "%s", lab)
                  : snprintf(out + n, n < cap ? cap - n : 0, "0x%X", v);
      if (w > 0) n += (size_t)w;
      if (n >= cap) n = cap - 1;
    }
    a = (comma < aend) ? comma + 1 : aend;
  }
  if (n + 1 < cap) out[n++] = ')';
  out[n] = '\0';
  return out;
}

/* ===== Persistent call-graph (navigate structure without re-running) ========
 * Accumulates UNIQUE call-like edges (caller call-site -> callee) into
 * games/<key>/callgraph.json as { "<caller> -> <callee>": "<count>" }: "who
 * calls X" = grep "-> X", "what X+0x.. calls" = grep "X+0x.. ->". An O(1) hash
 * dedup keeps the hot dispatch path cheap (the main-loop edges are recorded once,
 * then all hits just ++count); save is throttled. The callee shows its alias name
 * when one exists. Single-run (overwrite). Runtime-only. */
#define CG_HASH 16384  /* power of two; >> distinct edge count */
typedef struct { uint32_t caller, callee, count; char *line; int used; } CGEdge;
static CGEdge cg_edges[CG_HASH];
static int cg_new_since_save;

/* Resolve a call-site to its CONTAINING function: the nearest aliased function
 * entry at or below the call-site offset in the same binary, rendered by its
 * name -- so the graph reads "game_director -> load_resource", not call-sites. */
static void cg_caller_func(uint32_t caller_linear, char *out, size_t cap) {
  char site[64];
  name_addr(caller_linear, site, sizeof(site));
  char *plus = strstr(site, "+0x");
  if (!plus) { snprintf(out, cap, "%s", site); return; }
  size_t blen = (size_t)(plus - site);
  unsigned long csoff = strtoul(plus + 3, NULL, 16);
  const char *best = NULL;
  unsigned long bestoff = 0;
  for (int i = 0; i < aliasreg_count; ++i) {
    const char *id = aliasreg_entries[i].id;
    char *p2 = strstr(id, "+0x");
    if (!p2 || (size_t)(p2 - id) != blen || strncmp(id, site, blen) != 0) continue;
    unsigned long off2 = strtoul(p2 + 3, NULL, 16);
    if (off2 <= csoff && (!best || off2 > bestoff)) { best = id; bestoff = off2; }
  }
  if (!best) { snprintf(out, cap, "%s", site); return; }  /* no entry below */
  const char *nm = aliasreg_alias(best, 0);
  if (nm) {
    size_t k = 0;
    for (const char *s = nm; *s && *s != '(' && k < cap - 1; ++s) out[k++] = *s;
    out[k] = '\0';
  } else snprintf(out, cap, "%s", best);
}

#define CG_MAX_LINES 4096
static void cg_save(void) {
  char path[1100];
  annot_file_path("callgraph.json", path, sizeof(path));
  char tmp[1200];
  snprintf(tmp, sizeof(tmp), "%s.tmp", path);
  FILE *fp = fopen(tmp, "wb");
  if (!fp) return;
  /* Dedup the raw (per-call-site) edges by their function-level line, summing
   * counts, so many call-sites in one function collapse to one edge. */
  static const char *lines[CG_MAX_LINES];
  static uint32_t counts[CG_MAX_LINES];
  int nl = 0;
  for (int i = 0; i < CG_HASH; ++i) {
    if (!cg_edges[i].used || !cg_edges[i].line) continue;
    int f = -1;
    for (int j = 0; j < nl; ++j)
      if (strcmp(lines[j], cg_edges[i].line) == 0) { f = j; break; }
    if (f >= 0) counts[f] += cg_edges[i].count;
    else if (nl < CG_MAX_LINES) {
      lines[nl] = cg_edges[i].line; counts[nl] = cg_edges[i].count; nl++;
    }
  }
  fputs("{\n", fp);
  for (int j = 0; j < nl; ++j) {
    if (j) fputs(",\n", fp);
    fputs("  \"", fp);
    for (const char *s = lines[j]; *s; ++s) {
      if (*s == '"' || *s == '\\') fputc('\\', fp);
      fputc(*s, fp);
    }
    fprintf(fp, "\": \"%u\"", counts[j]);
  }
  fputs("\n}\n", fp);
  fclose(fp);
  rename(tmp, path);
}

static void cg_record(uint32_t caller, uint32_t callee) {
  if (isr_depth) return;  /* ISR (timer-chain) edges are noise */
  uint32_t h = caller * 2654435761u + callee * 40503u;
  for (int probe = 0; probe < 8; ++probe) {
    CGEdge *e = &cg_edges[(h + (uint32_t)probe) & (CG_HASH - 1)];
    if (e->used) {
      if (e->caller == caller && e->callee == callee) { e->count++; return; }
      continue;  /* collision -> probe */
    }
    /* new edge: resolve caller (-> containing function) + callee (name) ONCE */
    char a[64], b[64], bname[64];
    cg_caller_func(caller, a, sizeof(a));
    name_addr(callee, b, sizeof(b));
    const char *nm = aliasreg_alias(b, 0);  /* callee alias, no seed */
    const char *callee_disp = b;
    if (nm) {  /* strip any "(...)" argspec for the node label */
      size_t k = 0;
      for (const char *s = nm; *s && *s != '(' && k < sizeof(bname) - 1; ++s)
        bname[k++] = *s;
      bname[k] = '\0';
      callee_disp = bname;
    }
    char line[160];
    snprintf(line, sizeof(line), "%s -> %s", a, callee_disp);
    e->used = 1; e->caller = caller; e->callee = callee; e->count = 1;
    e->line = strdup(line);
    if (++cg_new_since_save >= 16) { cg_new_since_save = 0; cg_save(); }
    return;
  }
  /* probe chain full: drop (bounded backstop; effectively never) */
}

void shim_log(const char *func_name, const char *file, const char *func,
              int line, const char *path) {
  if (path) {
    shim_log_stdout("Trace: %s: %s (%s:%s:%d)\n", func_name, path, file, func,
                    line);
  } else {
    shim_log_stdout("Trace: %s (%s:%s:%d)\n", func_name, file, func, line);
  }
}

static bool resolve_case_insensitive_path(const char *path, char *resolved,
                                          size_t resolved_size) {
  if (!path || !*path || !resolved || resolved_size == 0) {
    return false;
  }

  char normalized[PATH_MAX];
  size_t len = 0;
  while (path[len] != '\0' && len < sizeof(normalized) - 1) {
    char current = path[len];
    if (current == '\\') {
      current = '/';
    }
    normalized[len++] = current;
  }
  if (path[len] != '\0') {
    return false;
  }
  normalized[len] = '\0';

  size_t start = 0;
  bool absolute = false;
  if (normalized[0] == '/') {
    absolute = true;
    start = 1;
  } else if (len >= 2 && normalized[1] == ':') {
    start = 2;
    if (normalized[2] == '/') {
      start = 3;
    }
  }

  char current_path[PATH_MAX];
  if (absolute) {
    current_path[0] = '/';
    current_path[1] = '\0';
  } else {
    current_path[0] = '\0';
  }

  const char *cursor = normalized + start;
  while (*cursor != '\0') {
    const char *next = strchr(cursor, '/');
    size_t comp_len = next ? (size_t)(next - cursor) : strlen(cursor);
    if (comp_len == 0) {
      if (!next) {
        break;
      }
      cursor = next + 1;
      continue;
    }
    if (comp_len > NAME_MAX) {
      return false;
    }

    char component[NAME_MAX + 1];
    memcpy(component, cursor, comp_len);
    component[comp_len] = '\0';

    if (strcmp(component, ".") == 0) {
      // No-op for current directory.
    } else if (strcmp(component, "..") == 0) {
      size_t cur_len = strlen(current_path);
      if (cur_len == 0 || (absolute && cur_len == 1 && current_path[0] == '/')) {
        // Already at base; nothing to pop.
      } else {
        if (current_path[cur_len - 1] == '/') {
          current_path[cur_len - 1] = '\0';
          --cur_len;
        }
        while (cur_len > 0 && current_path[cur_len - 1] != '/') {
          --cur_len;
        }
        current_path[cur_len] = '\0';
        if (cur_len == 0 && absolute) {
          current_path[0] = '/';
          current_path[1] = '\0';
        }
      }
    } else {
      char dir_path[PATH_MAX];
      if (current_path[0] == '\0') {
        strcpy(dir_path, absolute ? "/" : ".");
      } else {
        strcpy(dir_path, current_path);
      }

      DIR *dir = opendir(dir_path);
      if (!dir) {
        return false;
      }

      struct dirent *entry;
      const char *match = NULL;
      while ((entry = readdir(dir)) != NULL) {
        if (strcasecmp(entry->d_name, component) == 0) {
          match = entry->d_name;
          break;
        }
      }
      closedir(dir);

      if (!match) {
        return false;
      }

      size_t cur_len = strlen(current_path);
      if (cur_len == 0) {
        if (absolute) {
          if (snprintf(current_path, sizeof(current_path), "/%s", match) >=
              (int)sizeof(current_path)) {
            return false;
          }
        } else {
          if (snprintf(current_path, sizeof(current_path), "%s", match) >=
              (int)sizeof(current_path)) {
            return false;
          }
        }
      } else if (absolute && cur_len == 1 && current_path[0] == '/') {
        size_t match_len = strlen(match);
        if (cur_len + match_len >= sizeof(current_path)) {
          return false;
        }
        memcpy(current_path + cur_len, match, match_len + 1);
      } else {
        if (current_path[cur_len - 1] != '/') {
          if (cur_len + 1 >= sizeof(current_path)) {
            return false;
          }
          current_path[cur_len++] = '/';
          current_path[cur_len] = '\0';
        }
        size_t match_len = strlen(match);
        if (cur_len + match_len >= sizeof(current_path)) {
          return false;
        }
        memcpy(current_path + cur_len, match, match_len + 1);
      }
    }

    if (!next) {
      break;
    }
    cursor = next + 1;
  }

  if (current_path[0] == '\0') {
    return false;
  }

  strncpy(resolved, current_path, resolved_size);
  resolved[resolved_size - 1] = '\0';
  return true;
}

FILE *fopen_case_insensitive(const char *path, const char *mode) {
  FILE *f = fopen(path, mode);
  if (f) {
    return f;
  }

  int saved_errno = errno;
  if (saved_errno != ENOENT) {
    return NULL;
  }

  char resolved[PATH_MAX];
  if (!resolve_case_insensitive_path(path, resolved, sizeof(resolved))) {
    errno = saved_errno;
    return NULL;
  }

  if (strcmp(resolved, path) == 0) {
    errno = saved_errno;
    return NULL;
  }

  FILE *retry = fopen(resolved, mode);
  if (retry) {
    shim_log_stdout("Trace: fopen_case_insensitive matched %s -> %s\n", path,
                    resolved);
    return retry;
  }

  if (errno == ENOENT) {
    errno = saved_errno;
  }
  return NULL;
}

void shim_log_file_load(const char *path, const void *addr, size_t len,
                        size_t file_offset) {
  char offset_buf[32];
  const char *offset_text = "n/a";
  uint32_t offset;
  bool in_range = try_memory_offset(addr, &offset);
  if (in_range) {
    snprintf(offset_buf, sizeof(offset_buf), "0x%zX", (size_t)offset);
    offset_text = offset_buf;
  }

  shim_log_stdout(
      "Trace: loaded %s at %p (mem offset %s, file offset 0x%zX) length "
      "%zu\n",
      path, addr, offset_text, file_offset, len);

  if (in_range) {
    warn_file_overlap(path, addr, len);
    warn_rcb_overlap(path, addr, len);
  }
  register_file_mapping(path, file_offset, addr, len);
}

int load_executable(const char *path, uint16_t load_seg, int is_child,
                           uint16_t *out_cs, uint16_t *out_ip,
                           uint16_t *out_ss, uint16_t *out_sp) {
  CRITICAL_ENTER();
  FILE *f = fopen_case_insensitive(path, "rb");
  if (!f) {
    shim_log_stdout("Trace: load_executable failed to open %s\n", path);
    CRITICAL_EXIT();
    return 1;
  }
  uint8_t header[28];
  size_t hdr_read = fread(header, 1, sizeof(header), f);
  size_t size = 0;
  if (hdr_read >= 6) {
    uint16_t e_cblp = header[2] | (header[3] << 8);
    uint16_t e_cp = header[4] | (header[5] << 8);
    if (e_cp > 0) {
      size = ((size_t)(e_cp - 1) * 512) + (e_cblp ? e_cblp : 512);
    }
  }
  if (size == 0) {
    if (fseek(f, 0, SEEK_END) == 0) {
      long actual = ftell(f);
      if (actual > 0) {
        size = (size_t)actual;
      }
    }
  }
  if (size == 0) {
    fclose(f);
    shim_log_stdout("Trace: load_executable invalid size for %s\n", path);
    CRITICAL_EXIT();
    return 1;
  }
  uint8_t *buf = malloc(size);
  if (!buf) {
    fclose(f);
    shim_log_stdout("Trace: load_executable failed to allocate buffer for %s\n",
                    path);
    CRITICAL_EXIT();
    return 1;
  }
  if (fseek(f, 0, SEEK_SET) != 0 || fread(buf, 1, size, f) != size) {
    free(buf);
    fclose(f);
    shim_log_stdout("Trace: load_executable failed to read %s\n", path);
    CRITICAL_EXIT();
    return 1;
  }
  uint16_t header_paras = size >= 10 ? (buf[8] | (buf[9] << 8)) : 0;
  uint16_t min_alloc = size >= 12 ? (buf[10] | (buf[11] << 8)) : 0;
  uint16_t e_ss = size >= 16 ? (buf[14] | (buf[15] << 8)) : 0;
  uint16_t e_sp = size >= 18 ? (buf[16] | (buf[17] << 8)) : 0;
  uint16_t e_ip = size >= 22 ? (buf[20] | (buf[21] << 8)) : 0;
  uint16_t e_cs = size >= 24 ? (buf[22] | (buf[23] << 8)) : 0;
  uint16_t reloc_count = size >= 8 ? (buf[6] | (buf[7] << 8)) : 0;
  uint16_t reloc_off = size >= 26 ? (buf[24] | (buf[25] << 8)) : 0;
  size_t header_size = (size_t)header_paras * 16;
  if (size <= header_size) {
    free(buf);
    fclose(f);
    shim_log_stdout("Trace: load_executable header too small for %s\n", path);
    CRITICAL_EXIT();
    return 1;
  }
  size_t image_size = size - header_size;
  uint8_t *load_base = virtual_memory + ((uint32_t)load_seg << 4);
  {
    size_t blk_paras = (image_size + 15) / 16 + (size_t)min_alloc;
    if ((uint32_t)load_seg + blk_paras > CONVENTIONAL_TOP_SEG) {
      shim_log_stdout(
          "Trace: load_executable %s at seg 0x%04X crosses the 0xA000 ceiling; failing\n",
          path, load_seg);
      free(buf);
      fclose(f);
      CRITICAL_EXIT();
      return 1;
    }
  }
  memcpy(load_base, buf + header_size, image_size);
  shim_log_file_load(path, load_base, image_size, 0);
  /* This image overwrites whatever program last occupied this segment --
   * EXEC'd children reuse the freed arena, so successive child overlays
   * keep reloading into the same address. Any JIT chunks decoded from the OLD
   * bytes here are now stale; drop them (force = don't skip the chunk we're
   * "in", we're in the loader, not the loaded code) so the dispatch re-decodes
   * the fresh image instead of re-running the previous program -- which made
   * the intro/menu loop and corrupted later renders. The re-decode reads memory
   * AFTER relocations are applied below, so invalidating here is correct. */
  shim_jit_invalidate_code_range_force((uint32_t)((uint32_t)load_seg << 4),
                                       (uint32_t)image_size);

  size_t file_paras = (image_size + 15) / 16;
  size_t total_paras = file_paras + min_alloc;
  size_t alloc_bytes = total_paras * 16;
  if (alloc_bytes > image_size) {
    memset(load_base + image_size, 0, alloc_bytes - image_size); /* zero BSS */
  }
  if (!is_child) {
    /* These globals describe the RESIDENT program's block. An EXEC'd child
     * lives in its own block above the parent and must not perturb them. */
    uint32_t min_block_paras = (uint32_t)(LOAD_SEG - PSP_SEG);
    if (file_paras > 0xFFFF) {
      min_block_paras = 0xFFFF;
    } else {
      min_block_paras += (uint32_t)file_paras;
      if (min_block_paras > 0xFFFF) {
        min_block_paras = 0xFFFF;
      }
    }
    program_min_block_paras = (uint16_t)min_block_paras;
    next_free_seg = LOAD_SEG + (uint16_t)total_paras;
  } else {
    /* For the duration of the child's run the bump arena must sit ABOVE the
     * child's own image, so the child's allocations don't collide with it.
     * dos_exec saved the parent's next_free_seg before this call and restores
     * it once the child exits -- modelling DOS freeing the child's block. */
    next_free_seg = load_seg + (uint16_t)total_paras;
  }

  for (uint16_t i = 0; i < reloc_count; ++i) {
    size_t entry_off = (size_t)reloc_off + i * 4;
    if (entry_off + 3 < size) {
      uint16_t off = buf[entry_off] | (buf[entry_off + 1] << 8);
      uint16_t seg = buf[entry_off + 2] | (buf[entry_off + 3] << 8);
      uint32_t addr = ((uint32_t)seg << 4) + off;
      if (addr + 2 <= alloc_bytes) {
        uint16_t *p = (uint16_t *)(load_base + addr);
        *p += load_seg;
      }
    }
  }

  free(buf);
  fclose(f);
  CRITICAL_EXIT();

  if (out_cs) {
    *out_cs = load_seg + e_cs;
  }
  if (out_ip) {
    *out_ip = e_ip;
  }
  if (out_ss) {
    /* DOS loads the module at PSP+0x10 and treats e_ss as relative to that */
    *out_ss = load_seg + e_ss;
  }
  if (out_sp) {
    *out_sp = e_sp;
  }
  shim_log_stdout("Trace: load_executable loaded %s\n", path);
  return 0;
}

/* INT 21h AH=4Bh AL=03h "Load Overlay". Unlike a full EXEC this does NOT create
 * a PSP, set up registers, or run the program: it loads the file's image at the
 * caller-chosen segment (param-block word 0) and adds the caller's relocation
 * factor (param-block word 2) to each relocatable item; the caller then far-
 * calls the overlay itself. Returns 0 on success. This is how a program's
 * launcher pulls in its C:\VGA graphics driver -- into a segment IT allocated
 * above its own memory, so (unlike loading at the fixed LOAD_SEG) it does not
 * clobber the parent. */
int load_overlay(const char *path, uint16_t load_seg, uint16_t reloc_factor) {
  CRITICAL_ENTER();
  FILE *f = fopen_case_insensitive(path, "rb");
  if (!f) {
    shim_log_stdout("Trace: load_overlay failed to open %s\n", path);
    CRITICAL_EXIT();
    return 1;
  }
  uint8_t header[28];
  size_t hdr_read = fread(header, 1, sizeof(header), f);
  size_t size = 0;
  if (hdr_read >= 6) {
    uint16_t e_cblp = header[2] | (header[3] << 8);
    uint16_t e_cp = header[4] | (header[5] << 8);
    if (e_cp > 0) size = ((size_t)(e_cp - 1) * 512) + (e_cblp ? e_cblp : 512);
  }
  if (size == 0 && fseek(f, 0, SEEK_END) == 0) {
    long actual = ftell(f);
    if (actual > 0) size = (size_t)actual;
  }
  uint8_t *buf = size ? malloc(size) : NULL;
  if (!buf || fseek(f, 0, SEEK_SET) != 0 || fread(buf, 1, size, f) != size) {
    free(buf);
    fclose(f);
    shim_log_stdout("Trace: load_overlay failed to read %s\n", path);
    CRITICAL_EXIT();
    return 1;
  }
  uint16_t header_paras = size >= 10 ? (buf[8] | (buf[9] << 8)) : 0;
  uint16_t reloc_count = size >= 8 ? (buf[6] | (buf[7] << 8)) : 0;
  uint16_t reloc_off = size >= 26 ? (buf[24] | (buf[25] << 8)) : 0;
  size_t header_size = (size_t)header_paras * 16;
  if (size <= header_size) {
    free(buf);
    fclose(f);
    CRITICAL_EXIT();
    return 1;
  }
  size_t image_size = size - header_size;
  uint32_t base = (uint32_t)load_seg << 4;
  if (base + image_size > MEMORY_SIZE) {
    free(buf);
    fclose(f);
    CRITICAL_EXIT();
    return 1;
  }
  /* This load overwrites whatever code/data sat in [base, base+image_size) --
   * drop any JIT chunk decoded from it so the next dispatch re-decodes. A
   * replace-self overlay (a self-replacing overlay EXE loads C:\VGA over its own segment) leaves the
   * loader's cs:ip INSIDE this range, so use the force variant: the resident
   * loader's own chunk (e.g. the env-walk startup at offset 0) MUST be dropped,
   * else the stub's later jump to the new entry runs the stale loader code. */
  shim_jit_invalidate_code_range_force(base, (uint32_t)image_size);
  uint8_t *dst = virtual_memory + base;
  memcpy(dst, buf + header_size, image_size);
  for (uint16_t i = 0; i < reloc_count; ++i) {
    size_t entry_off = (size_t)reloc_off + i * 4;
    if (entry_off + 3 < size) {
      uint16_t off = buf[entry_off] | (buf[entry_off + 1] << 8);
      uint16_t seg = buf[entry_off + 2] | (buf[entry_off + 3] << 8);
      uint32_t addr = ((uint32_t)seg << 4) + off;
      if (addr + 2 <= image_size) {
        uint16_t *p = (uint16_t *)(dst + addr);
        *p += reloc_factor;
      }
    }
  }
  free(buf);
  fclose(f);
  shim_log_file_load(path, dst, image_size, 0);
  shim_log_stdout("Trace: load_overlay %s at seg 0x%04X reloc 0x%04X (%zu bytes)\n",
                  path, load_seg, reloc_factor, image_size);
  CRITICAL_EXIT();
  return 0;
}

uint32_t mask_addr(uint32_t addr) {
  return a20_enabled ? (addr & MEMORY_MASK) : (addr & 0xFFFFF);
}

uint32_t wrap_segoff_addr(uint32_t base, uint32_t offset) {
  /* Used by dos_read_file / dos_write_file to compute the linear address
   * of byte N within a buffer at linear ``base``. Previous implementation
   *
   *   return (base & ~0xFFFFu) | ((base + offset) & 0xFFFFu);
   *
   * attempted to model the DOS segment-wrap semantics (offset within a
   * segment wraps at 0xFFFF) but used ``base & ~0xFFFFu`` as the segment
   * base — that's the 64KB-LINEAR-page boundary, not the segment base.
   * For any ``base`` with non-zero low 16 bits (i.e. virtually every load
   * because the caller's segment is rarely 0x?000) it would scatter bytes
   * to the wrong linear addresses when (base + offset) crossed a 64KB
   * linear boundary. Concrete bite: chunk-135 LOAD at base 0x3EB00, len
   * 0x3F2E — the first 0x1500 bytes landed correctly, the rest landed
   * 64KB lower at 0x30000+, leaving 0x3EB00+0x1500..0x42A2E zero. The
   * empty-path string the game later read from a swap-derived address
   * caused a "DISK read Error!!" game-side exit.
   *
   * Proper segment-wrap would need (seg, off) plumbed through the read/
   * write shims. No known program LOAD exceeds 64KB in a single syscall,
   * so plain linear addition is correct here. If a future game does need
   * segment-wrap, revisit by plumbing seg/off through dos_read_file_impl
   * and dos_write_file_impl (currently they take a void* buffer). */
  return base + offset;
}

void file_mapping_swap_impl(uint16_t seg_a, uint16_t off_a, uint16_t seg_b,
                            uint16_t off_b, size_t len, const char *file,
                            const char *func, int line) {
  uint32_t addr_a = linear_addr(seg_a, off_a);
  uint32_t addr_b = linear_addr(seg_b, off_b);
  shim_log_stdout(
      "Trace: file_mapping_swap 0x%05X-0x%05X <-> 0x%05X-0x%05X (%s:%s:%d)\n",
      addr_a, addr_a + (uint32_t)len, addr_b, addr_b + (uint32_t)len, file,
      func, line);

  FileMapping *m_a = find_file_mapping_mut(addr_a);
  FileMapping *m_b = find_file_mapping_mut(addr_b);
  bool exact_a = m_a && m_a->base == addr_a && m_a->len == len;
  bool exact_b = m_b && m_b->base == addr_b && m_b->len == len;

  if (exact_a) {
    m_a->base = addr_b;
  } else if (m_a) {
    shim_log_stdout(
        "Trace: file_mapping_swap skipped rebasing %s at 0x%05X (len 0x%zX); "
        "requested subrange 0x%05X-0x%05X\n",
        m_a->path ? m_a->path : "<unknown>", m_a->base, m_a->len, addr_a,
        addr_a + (uint32_t)len);
  }

  if (exact_b) {
    m_b->base = addr_a;
  } else if (m_b) {
    shim_log_stdout(
        "Trace: file_mapping_swap skipped rebasing %s at 0x%05X (len 0x%zX); "
        "requested subrange 0x%05X-0x%05X\n",
        m_b->path ? m_b->path : "<unknown>", m_b->base, m_b->len, addr_b,
        addr_b + (uint32_t)len);
  }
}

uint16_t memw_raw_read(uint16_t seg, uint16_t off) {
  uint32_t addr = linear_addr(seg, off);
  return virtual_memory[addr] | (virtual_memory[mask_addr(addr + 1)] << 8);
}

void memw_raw_write(uint16_t seg, uint16_t off, uint16_t value) {
  uint32_t addr = linear_addr(seg, off);
  virtual_memory[addr] = value & 0xFF;
  virtual_memory[mask_addr(addr + 1)] = value >> 8;
}


/* ===== Stack-write watcher ring =====
 *
 * Records every word read/write to (ss, *) within a tight window around the
 * current sp. Lets a post-mortem identify which push placed a given value
 * at the slot that a later ret popped — the canonical signature of the
 * "segment value popped as IP" stack imbalance.
 *
 * Lifecycle: dumped to stack_writes.log in every crash bundle.
 */
typedef enum { SWO_PUSH = 1, SWO_POP = 2 } StackOpKind;
typedef struct {
  uint32_t t_us;
  uint16_t writer_cs;
  uint16_t writer_ip;
  uint16_t target_ss;
  uint16_t target_off;
  uint16_t value;
  uint16_t sp_at_op;
  uint8_t  kind;          /* StackOpKind */
  uint8_t  isr_depth_at;
  uint8_t  lcall_depth_at;
  uint8_t  reserved;
  const char *file;
  uint32_t line;
} StackWriteEvent;
#define STACK_WRITE_RING_BITS 11   /* 2048 entries */
#define STACK_WRITE_RING_SIZE (1u << STACK_WRITE_RING_BITS)
#define STACK_WRITE_RING_MASK (STACK_WRITE_RING_SIZE - 1u)
static StackWriteEvent stack_write_ring[STACK_WRITE_RING_SIZE];
static uint32_t        stack_write_ring_pos;

static inline void stack_op_record(StackOpKind kind, uint16_t seg, uint16_t off,
                                   uint16_t value, const char *file, int line) {
  if (seg != ss) return;
  /* Only record writes/reads within the active stack frame window. The
   * window is generous (sp - 16 .. sp + 256) to catch lcall-saved frames
   * AND any push/pop that targets a slot a bit above sp. Anything far from
   * sp is unrelated data in the stack segment. */
  uint16_t rel = (uint16_t)(off - sp);
  /* rel as a signed 16-bit value: < 0 means below current sp, > 0 above. */
  int16_t rel_s = (int16_t)rel;
  if (rel_s < -16 || rel_s > 256) return;
  StackWriteEvent *e =
      &stack_write_ring[stack_write_ring_pos++ & STACK_WRITE_RING_MASK];
  e->t_us = (uint32_t)lifecycle_elapsed_us();
  e->writer_cs = cs;
  e->writer_ip = ip;
  e->target_ss = seg;
  e->target_off = off;
  e->value = value;
  e->sp_at_op = sp;
  e->kind = (uint8_t)kind;
  e->isr_depth_at = isr_depth;
  e->lcall_depth_at = lcall_depth;
  e->file = file;
  e->line = (uint32_t)line;
}

static void stack_writes_dump_to_dir(const char *dir) {
  if (!dir) return;
  char path[320];
  snprintf(path, sizeof(path), "%s/stack_writes.log", dir);
  FILE *f = fopen(path, "w");
  if (!f) return;
  fprintf(f,
          "# Per-stack-cell write/read ring. Newest last.\n"
          "# Columns: t_us kind cs:ip ss:off val sp_at_op rel(sp) "
          "isr lcall  source\n"
          "# Use to find the push that wrote the value a later ret popped.\n"
          "# Filter by 'off=XXXX' to see the history of a single slot.\n");
  uint32_t n = stack_write_ring_pos < STACK_WRITE_RING_SIZE
                   ? stack_write_ring_pos
                   : STACK_WRITE_RING_SIZE;
  uint32_t start = stack_write_ring_pos >= STACK_WRITE_RING_SIZE
                       ? (stack_write_ring_pos & STACK_WRITE_RING_MASK)
                       : 0u;
  for (uint32_t i = 0; i < n; ++i) {
    uint32_t idx = (start + i) & STACK_WRITE_RING_MASK;
    StackWriteEvent *e = &stack_write_ring[idx];
    int16_t rel = (int16_t)((uint16_t)(e->target_off - e->sp_at_op));
    const char *kn = e->kind == SWO_PUSH ? "PUSH" : (e->kind == SWO_POP ? "POP " : "?   ");
    const char *fbase = e->file ? strrchr(e->file, '/') : NULL;
    fbase = fbase ? fbase + 1 : (e->file ? e->file : "?");
    fprintf(f, "t=%-10u %s cs:ip=%04X:%04X ss:off=%04X:%04X val=%04X sp=%04X rel=%+5d "
               "isr=%u lcall=%u  %s:%u\n",
            (unsigned)e->t_us, kn, e->writer_cs, e->writer_ip, e->target_ss,
            e->target_off, e->value, e->sp_at_op, (int)rel,
            (unsigned)e->isr_depth_at, (unsigned)e->lcall_depth_at, fbase,
            (unsigned)e->line);
  }
  fclose(f);
}

uint16_t memw_read_impl(uint16_t seg, uint16_t off, const char *file,
                        const char *func, int line) {
  uint32_t addr = linear_addr(seg, off);
  uint32_t rcb_base = linear_addr(es, 0xFF00);
  if (seg == 0 && off < 0x10) {
    shim_log_stdout("Warning: null pointer word write %04X:%04X (%s:%s:%d)\n",
                    seg, off, file, func, line);
  }
  if (seg == es && addr >= rcb_base && addr < rcb_base + 0x100) {
    RCBField field = (RCBField)(0xFF00 + (addr - rcb_base));
    return rcb_read16_impl(field, file, func, line);
  }
  uint16_t v = memw_raw_read(seg, off);
  stack_op_record(SWO_POP, seg, off, v, file, line);
  return v;
}

/* Investigation watchlist — writes to these linear addresses are logged to
 * the lifecycle ring with full source context (cs:ip + value). Used to
 * answer "who wrote X to memory M between t0 and t1" when the trace tail
 * has rotated and the mutation we care about is no longer in trace.tail.
 * Lifecycle ring is 65k entries and survives much longer wall-clock than
 * the per-instruction trace, so a write here is very likely to still be
 * visible at crash time. Empty list = no overhead. */
typedef struct {
  uint32_t lo;
  uint32_t hi; /* inclusive */
  const char *name;
} WriteWatch;
static const WriteWatch write_watches[] = {
  /* No write-watches configured by default. To trace who writes to a linear
   * range, add {lo, hi, "name"} entries here (hi inclusive); every matching
   * write is mirrored to the lifecycle ring with full source context, which
   * answers "who wrote value X to memory M between t0 and t1" after the
   * per-instruction trace tail has rotated. The sentinel below never matches
   * a real write (lo > hi) and keeps this a valid non-empty array. */
  {0xFFFFFFFFu, 0x0u, NULL},
};
static const size_t write_watches_count =
    sizeof(write_watches) / sizeof(write_watches[0]);

/* Persistent WATCHW log. The lifecycle ring (65k entries) wraps every
 * few seconds under heavy LCALL/CALL activity, evicting the corrupting
 * write before the crash bundle is dumped. Mirror every WATCHW hit to
 * watchw.log so the writer is still recoverable even on a delayed crash. */
static FILE *watchw_log_fp;

/* ===== Write-time tripwire for RCB indirect-dispatch slots =====
 * The RCB slots at 0x22A0X hold ljmp/lcall pointers the game reads
 * every timer tick. They get written exactly once during the init
 * code in the main program (cs=0x1010) and are immutable for the lifetime
 * of the program. The game's main code at cs=0x12B0 / 0x22A0 should
 * NEVER write here; if it does, that's a buffer overrun stomping the
 * dispatch table, and the later ljmp/lcall will land somewhere bogus.
 *
 * Detect at write-time, not at use-time: every write that hits these
 * slots while we're past the init phase (cs != 0x1010) is the bug.
 * Abort there with the full caller context so the crash bundle's
 * cs:ip + lifecycle.log tail pinpoint the bad instruction. */
/* ProtectedSlot + the slot list are game-specific data (game_config.h /
 * the per-game config), not hardcoded here -- a game that declares no slots
 * gets no check. */
static void protected_slots_check_write(uint32_t addr, size_t size,
                                         uint32_t value, const char *file,
                                         const char *func, int line) {
  /* Init writes come from the game's load segment during startup; after the
   * main entry hands off to other segments these slots are read-only. The
   * config declares init_cs + the slots; count==0 disables the check. */
  if (game_config.protected_slot_count == 0) return;
  if (cs == game_config.init_cs) return;
  for (size_t i = 0; i < game_config.protected_slot_count; ++i) {
    uint32_t slot_lo = game_config.protected_slots[i].lo;
    uint32_t slot_hi = game_config.protected_slots[i].hi;
    if (addr + size <= slot_lo || addr > slot_hi) continue;
    char msg[768];
    int n = snprintf(msg, sizeof(msg),
        "[RCB OVERWRITE] post-init write into protected slot %s "
        "@ 0x%05X size=%zu val=0x%X\n"
        "  cs:ip=%04X:%04X active=%s ds=%04X es=%04X "
        "ax=%04X bx=%04X cx=%04X dx=%04X si=%04X di=%04X bp=%04X "
        "ss:sp=%04X:%04X\n"
        "  via %s:%s:%d\n"
        "  diagnosis: this slot holds an indirect ljmp/lcall target "
        "the game reads every timer tick. A write here from non-init "
        "code is a buffer overrun stomping the dispatch table; the "
        "next dispatch through the slot would land at a bogus target. "
        "The cs:ip above is the instruction whose write overflowed.\n",
        game_config.protected_slots[i].name, addr, size, value,
        cs, ip, shim_active_binary() ? shim_active_binary() : "<none>",
        ds, es, ax, bx, cx, dx, si, di, bp, ss, sp,
        file ? file : "?", func ? func : "?", line);
    fprintf(stderr, "%s", msg);
    shim_log_crash("%s", msg);
    save_bug_bundle("rcb_overwrite", (uint32_t)addr, msg);
    shim_flush_all_streams();
    abort();
  }
}

void shim_protected_slots_check(void) { /* legacy no-op for callers */ }

/* ===== Bookend recorder =====
 * User-driven before/after capture for symptoms without a crash anchor.
 * F9 (or shim_bookend_start) snapshots full RAM + opens a filtered write
 * log. F10 (or shim_bookend_stop) snapshots again and closes the log.
 * Filter: skip seg==ss (stack churn) and skip VGA pages (0xA0000-0xBFFFF).
 * Post-mortem: the source against the two snapshots names the
 * changed addresses; grep the log for those addresses to find the writer. */
static volatile int bookend_active;
static FILE *bookend_log_fp;
static uint64_t bookend_writes_logged;
static uint64_t bookend_writes_skipped;

static void bookend_dump_snapshot(const char *path) {
  FILE *f = fopen(path, "wb");
  if (!f) {
    shim_log_stderr("Bookend: failed to open %s: %s\n", path, strerror(errno));
    return;
  }
  size_t w = fwrite(virtual_memory, 1, MEMORY_SIZE, f);
  fclose(f);
  shim_log_stdout("Bookend: wrote %zu bytes to %s\n", w, path);
}

void shim_bookend_start(void) {
  if (bookend_active) {
    shim_log_stdout("Bookend: already active, ignoring start\n");
    return;
  }
  bookend_dump_snapshot("/tmp/zbookend_snap1.bin");
  bookend_log_fp = fopen("/tmp/zbookend.log", "w");
  if (bookend_log_fp) {
    setvbuf(bookend_log_fp, NULL, _IOLBF, 0);
    fprintf(bookend_log_fp,
            "# bookend START cs:ip=%04X:%04X ds=%04X es=%04X ss:sp=%04X:%04X "
            "active=%s\n",
            cs, ip, ds, es, ss, sp,
            shim_active_binary() ? shim_active_binary() : "<none>");
  } else {
    shim_log_stderr("Bookend: failed to open /tmp/zbookend.log: %s\n",
                    strerror(errno));
  }
  bookend_writes_logged = 0;
  bookend_writes_skipped = 0;
  bookend_active = 1;
  shim_log_stdout("Bookend: START\n");
}

void shim_bookend_stop(void) {
  if (!bookend_active) {
    shim_log_stdout("Bookend: not active, ignoring stop\n");
    return;
  }
  bookend_active = 0;
  bookend_dump_snapshot("/tmp/zbookend_snap2.bin");
  if (bookend_log_fp) {
    fprintf(bookend_log_fp,
            "# bookend STOP cs:ip=%04X:%04X ds=%04X es=%04X ss:sp=%04X:%04X "
            "logged=%llu skipped=%llu\n",
            cs, ip, ds, es, ss, sp,
            (unsigned long long)bookend_writes_logged,
            (unsigned long long)bookend_writes_skipped);
    fclose(bookend_log_fp);
    bookend_log_fp = NULL;
  }
  shim_log_stdout("Bookend: STOP  logged=%llu skipped=%llu\n"
                  "  diff: saisei zbookend-diff "
                  "/tmp/zbookend_snap1.bin /tmp/zbookend_snap2.bin\n"
                  "  log:  /tmp/zbookend.log\n",
                  (unsigned long long)bookend_writes_logged,
                  (unsigned long long)bookend_writes_skipped);
}

static inline void bookend_log_write(uint16_t seg, uint16_t off, uint32_t addr,
                                     size_t size, uint32_t value,
                                     const char *file, const char *func,
                                     int line) {
  if (!bookend_active || !bookend_log_fp) return;
  /* Filter: skip pushes only. A push is `sp -= 2; memw_write(ss, sp, val)`
   * so the write lands at off == sp. Writes to slots ABOVE sp (saved regs,
   * return addresses of outer frames) must be logged — that's where stack
   * overwrite corruption appears. Also skip VGA. */
  int is_push = (seg == ss) && (off == sp);
  if (is_push || (addr >= 0xA0000 && addr < 0xC0000)) {
    bookend_writes_skipped++;
    return;
  }
  fprintf(bookend_log_fp,
          "W %05X size=%zu val=0x%X seg:off=%04X:%04X cs:ip=%04X:%04X "
          "ds=%04X es=%04X bx=%04X si=%04X di=%04X ax=%04X (%s:%s:%d)\n",
          addr, size, value, seg, off, cs, ip,
          ds, es, bx, si, di, ax,
          file ? file : "?", func ? func : "?", line);
  bookend_writes_logged++;
}

void write_watch_log(uint32_t addr, size_t size, uint32_t value,
                            const char *file, const char *func, int line) {
  /* Named-var change-watch (vars.json): report value changes of named addresses.
   * Loaded once here (this is the central per-write sink); the [lo,hi] window
   * makes the common unwatched write a single comparison. */
  static int vars_init;
  static size_t vars_resolved_at = (size_t)-1;
  if (!vars_init) { vars_init = 1; aliasreg_vars_load(); }
  if (aliasreg_has_origin_vars && file_mapping_count != vars_resolved_at) {
    vars_resolved_at = file_mapping_count;
    aliasreg_vars_resolve();
  }
  if (addr >= aliasreg_var_lo && addr <= aliasreg_var_hi)
    aliasreg_var_write(addr, (uint8_t)size, value);
  for (size_t i = 0; i < write_watches_count; ++i) {
    if (addr + size > write_watches[i].lo && addr <= write_watches[i].hi) {
      /* Region-name the watched address and the ds:si source pointer so the
       * line reads "@ work_seg_images+0x.. src=scratch_decode+0x.." instead of
       * bare hex. (For a non-copy write, src= is just where ds:si points.) */
      char tgtbuf[64], srcbuf[64];
      name_addr(addr, tgtbuf, sizeof(tgtbuf));
      name_addr(((uint32_t)ds << 4) + si, srcbuf, sizeof(srcbuf));
      lifecycle_log(
          "WATCHW [%s] @ %s size=%zu val=0x%X  src=%s  cs:ip=%04X:%04X  "
          "es=%04X bx=%04X di=%04X ax=%04X (%s:%s:%d)\n",
          write_watches[i].name, tgtbuf, size, value, srcbuf, cs, ip,
          es, bx, di, ax,
          file ? file : "?", func ? func : "?", line);
      if (!watchw_log_fp) {
        watchw_log_fp = fopen("watchw.log", "w");
        if (watchw_log_fp) setvbuf(watchw_log_fp, NULL, _IOLBF, 0);
      }
      if (watchw_log_fp) {
        fprintf(watchw_log_fp,
                "WATCHW [%s] @ %s size=%zu val=0x%X  src=%s  cs:ip=%04X:%04X  "
                "es=%04X bx=%04X di=%04X ax=%04X (%s:%s:%d)\n",
                write_watches[i].name, tgtbuf, size, value, srcbuf, cs, ip,
                es, bx, di, ax,
                file ? file : "?", func ? func : "?", line);
      }
      protected_slots_check_write(addr, size, value, file, func, line);
      break;
    }
  }
}

void memw_write_impl(uint16_t seg, uint16_t off, uint16_t value,
                     const char *file, const char *func, int line) {
  uint32_t addr = linear_addr(seg, off);
  uint32_t rcb_base = linear_addr(es, 0xFF00);
  if (seg == 0 && off < 0x10) {
    shim_log_stdout("Warning: null pointer byte write %04X:%04X (%s:%s:%d)\n",
                    seg, off, file, func, line);
  }
  if (seg == es && addr >= rcb_base && addr < rcb_base + 0x100) {
    RCBField field = (RCBField)(0xFF00 + (addr - rcb_base));
    rcb_write16_impl(field, value, file, func, line);
    return;
  }
  bookend_log_write(seg, off, addr, 2, value, file, func, line);
  write_watch_log(addr, 2, value, file, func, line);
  warn_on_mutation(addr, 2, file, func, line);
  stack_op_record(SWO_PUSH, seg, off, value, file, line);
  memw_raw_write(seg, off, value);
  /* If this word overwrote decoded code (an overlay reshuffle clearing/moving a
   * region byte/word at a time, or self-modifying code), drop the stale chunk. */
  shim_jit_invalidate_code_range(addr, 2);
}

uint8_t memb_read_impl(uint16_t seg, uint16_t off, const char *file,
                       const char *func, int line) {
  uint32_t addr = linear_addr(seg, off);
  uint32_t rcb_base = linear_addr(es, 0xFF00);
  if (seg == es && addr >= rcb_base && addr < rcb_base + 0x100) {
    RCBField field = (RCBField)(0xFF00 + (addr - rcb_base));
    return rcb_read8_impl(field, file, func, line);
  }
  uint8_t v = (uint8_t)memb_raw(seg, off);
  return v;
}

void memb_write_impl(uint16_t seg, uint16_t off, uint8_t value,
                     const char *file, const char *func, int line) {
  uint32_t addr = linear_addr(seg, off);
  uint32_t rcb_base = linear_addr(es, 0xFF00);
  if (seg == es && addr >= rcb_base && addr < rcb_base + 0x100) {
    RCBField field = (RCBField)(0xFF00 + (addr - rcb_base));
    rcb_write8_impl(field, value, file, func, line);
    return;
  }
  bookend_log_write(seg, off, addr, 1, value, file, func, line);
  write_watch_log(addr, 1, value, file, func, line);
  warn_on_mutation(addr, 1, file, func, line);
  memb_raw(seg, off) = value;
  /* See memw_write_impl: drop any JIT chunk whose decoded code this byte
   * overwrote (overlay reshuffle clearing/moving a region, or self-mod code). */
  shim_jit_invalidate_code_range(addr, 1);
}

/* Returns 1 if [seg:lo, seg:lo+len) overlaps the RCB control window
 * at es:FF00..FFFF. RCB writes have side effects beyond plain memory,
 * so block helpers must fall back to the per-byte path for those. */
static int rep_range_touches_rcb(uint16_t seg, uint32_t lo, uint32_t len) {
  if (seg != es) return 0;
  uint32_t rcb_lo = ((uint32_t)es << 4) + 0xFF00;
  uint32_t rcb_hi = rcb_lo + 0x100;
  return lo < rcb_hi && lo + len > rcb_lo;
}

/* Returns 1 if [lo, lo+len) overlaps any address range under WATCHW
 * surveillance. The block-copy fast path uses raw memmove which would
 * silently bypass write_watch_log; routing through the per-byte/word
 * loop instead ensures the watched-slot writer is logged. */
static int rep_range_touches_watch(uint32_t lo, uint32_t len) {
  for (size_t i = 0; i < write_watches_count; ++i) {
    if (lo < write_watches[i].hi + 1u && lo + len > write_watches[i].lo) {
      return 1;
    }
  }
  return 0;
}

/* Returns 1 if a forward rep starting at ``off`` with byte count ``count``
 * (after element-size multiplication) would wrap the 16-bit offset within
 * its segment. Real-mode rep instructions wrap si/di at 0xFFFF, jumping
 * back to the start of the segment; a memmove on linear memory would not
 * model that, so we fall back to the byte loop. Backward reps don't wrap
 * unless the starting offset is too small for the count, handled the
 * same way. */
static int rep_would_wrap(uint16_t off, uint32_t count, int direction) {
  if (direction > 0) {
    return (uint32_t)off + count > 0x10000u;
  }
  /* direction < 0 */
  return count > (uint32_t)off + 1;
}

static const BinaryDispatch *find_binary_for_addr(uint32_t addr,
                                                  const FileMapping **out_fm);

/* Relocation-aware dispatch: some DOS programs copy their own code image to a
 * different segment at runtime (a `rep movsb` self-relocator) and then execute
 * the copy -- e.g. a relocated DOS EXE's SETUP.EXE. Static dispatch is keyed on
 * linear address via file_mappings (the ORIGINAL load image), so a far-jump
 * into the copy would decode whatever was originally at that linear address.
 *
 * When a block copy moves a WHOLE code mapping (copy starts exactly at the
 * mapping base, large enough) to fresh memory that overlaps nothing, register
 * a SHADOW mapping for the destination pointing at the same file offsets as
 * the source -- so a later far-jump into the copy dispatches the correct
 * decoded code. Tightly guarded so ordinary data copies never trigger it. */
static void maybe_register_relocation_shadow(uint32_t src_lo, uint32_t dst_lo,
                                             uint32_t count) {
  if (count < 256 || src_lo == dst_lo) return;
  const FileMapping *src_fm = NULL;
  const BinaryDispatch *bd = find_binary_for_addr(src_lo, &src_fm);
  if (!bd || !src_fm || !src_fm->path) return;   /* source isn't code */
  /* In-place self-relocation: the SOURCE is wholly code in this mapping and the
   * DESTINATION overlaps the image (the program shifts its own code and runs
   * from the new spot -- the dst may extend a little past the load end). This
   * excludes ordinary data copies OUT to VGA/heap (dst disjoint from the
   * image), which we'd never dispatch into. The shadow reflects the bytes the
   * memmove actually placed, so dispatch through it is correct. */
  uint32_t fm_lo = src_fm->base, fm_hi = src_fm->base + (uint32_t)src_fm->len;
  if (src_lo < fm_lo || src_lo + count > fm_hi) return;    /* src wholly code */
  if (dst_lo >= fm_hi || dst_lo + count <= fm_lo) return;  /* dst overlaps image */
  uint32_t new_file_off = src_fm->file_offset + (src_lo - src_fm->base);
  /* Idempotent: skip if an identical shadow is already the top mapping here. */
  for (size_t i = 0; i < file_mapping_count; ++i)
    if (file_mappings[i].base == dst_lo && file_mappings[i].len == count &&
        file_mappings[i].file_offset == new_file_off)
      return;
  if (file_mapping_count >= MAX_FILE_MAPPINGS) return;
  FileMapping *fm = &file_mappings[file_mapping_count++];
  memset(fm, 0, sizeof(*fm));
  fm->path = strdup(src_fm->path);
  fm->base = dst_lo;
  fm->len = count;
  /* find_file_mapping scans last-registered first, so this overrides the
   * original mapping for [dst_lo, dst_lo+count) -- exactly where the moved
   * code now lives. */
  fm->file_offset = new_file_off;
  fm->data = NULL;
  shim_log_stdout(
      "Trace: relocation shadow: %s 0x%X bytes 0x%05X -> 0x%05X "
      "(dispatch now follows the copy)\n",
      fm->path ? fm->path : "?", count, src_lo, dst_lo);
  lifecycle_log("RELOC 0x%05X->0x%05X len 0x%X\n", src_lo, dst_lo, count);
}

void rep_movsb_block_impl(uint16_t dst_seg, uint16_t src_seg,
                          const char *file, const char *func, int line) {
  if (cx == 0) return;
  uint32_t count = cx;
  int delta = DF ? -1 : 1;

  if (rep_would_wrap(si, count, delta) || rep_would_wrap(di, count, delta)) {
    while (cx) {
      memb_write_impl(dst_seg, di,
                      memb_read_impl(src_seg, si, file, func, line),
                      file, func, line);
      si = (uint16_t)(si + delta);
      di = (uint16_t)(di + delta);
      cx = (uint16_t)(cx - 1);
    }
    return;
  }

  /* Linear address of the leftmost byte of each range. */
  uint16_t src_first_off = DF ? (uint16_t)(si - count + 1) : si;
  uint16_t dst_first_off = DF ? (uint16_t)(di - count + 1) : di;
  uint32_t src_lo = linear_addr(src_seg, src_first_off);
  uint32_t dst_lo = linear_addr(dst_seg, dst_first_off);

  if (rep_range_touches_rcb(dst_seg, dst_lo, count) ||
      rep_range_touches_rcb(src_seg, src_lo, count) ||
      rep_range_touches_watch(dst_lo, count)) {
    while (cx) {
      memb_write_impl(dst_seg, di,
                      memb_read_impl(src_seg, si, file, func, line),
                      file, func, line);
      si = (uint16_t)(si + delta);
      di = (uint16_t)(di + delta);
      cx = (uint16_t)(cx - 1);
    }
    maybe_register_relocation_shadow(src_lo, dst_lo, count);
    shim_jit_invalidate_code_range(dst_lo, count);
    return;
  }

  /* memmove copies "safely" for overlapping ranges -- which is WRONG for the
   * x86 rep-movs OVERLAP-REPLICATION idiom (forward dst>src / backward dst<src,
   * i.e. LZ77/RLE back-references where each copied byte re-reads a byte just
   * written this same rep). When the byte ranges overlap, replicate faithfully
   * byte-by-byte in the DF direction so the propagation happens. */
  if (src_lo < dst_lo + count && dst_lo < src_lo + count) {
    uint32_t s = linear_addr(src_seg, si);
    uint32_t d = linear_addr(dst_seg, di);
    for (uint32_t i = 0; i < count; ++i) {
      virtual_memory[mask_addr(d)] = virtual_memory[mask_addr(s)];
      s = (uint32_t)(s + delta);
      d = (uint32_t)(d + delta);
    }
  } else {
    memmove(virtual_memory + dst_lo, virtual_memory + src_lo, count);
  }
  warn_on_mutation(dst_lo, count, file, func, line);
  maybe_register_relocation_shadow(src_lo, dst_lo, count);
  shim_jit_invalidate_code_range(dst_lo, count);

  si = (uint16_t)(si + (int)count * delta);
  di = (uint16_t)(di + (int)count * delta);
  cx = 0;
}

/* `rep stosb` shim. Mirrors rep_movsb_block_impl's fast/slow split:
 * memset on virtual_memory only when the destination range doesn't
 * cross RCB or a watched range. The earlier codegen emitted the
 * memset() inline, which silently corrupted RCB indirect-dispatch
 * slots when a stosb overflowed into them (the host memset bypasses
 * memb_write_impl / rcb_write*, so the RCB write semantics + WATCHW
 * never saw it). Falls back to per-byte memb_write_impl so the
 * affected slot reads and our slot tripwire stay consistent. */
void rep_stosb_block_impl(uint16_t dst_seg, const char *file,
                          const char *func, int line) {
  if (cx == 0) return;
  uint32_t count = cx;
  int delta = DF ? -1 : 1;

  if (rep_would_wrap(di, count, delta)) {
    while (cx) {
      memb_write_impl(dst_seg, di, al, file, func, line);
      di = (uint16_t)(di + delta);
      cx = (uint16_t)(cx - 1);
    }
    return;
  }

  uint16_t dst_first_off = DF ? (uint16_t)(di - count + 1) : di;
  uint32_t dst_lo = linear_addr(dst_seg, dst_first_off);

  if (rep_range_touches_rcb(dst_seg, dst_lo, count) ||
      rep_range_touches_watch(dst_lo, count)) {
    while (cx) {
      memb_write_impl(dst_seg, di, al, file, func, line);
      di = (uint16_t)(di + delta);
      cx = (uint16_t)(cx - 1);
    }
    shim_jit_invalidate_code_range(dst_lo, count);
    return;
  }

  memset(virtual_memory + dst_lo, al, count);
  warn_on_mutation(dst_lo, count, file, func, line);
  /* A clear over a code region (overlay reshuffle vacating an old slot) makes
   * any JIT chunk decoded there stale -- drop it like rep movs does, else the
   * next dispatch runs the old overlay's code on now-cleared memory. */
  shim_jit_invalidate_code_range(dst_lo, count);

  di = (uint16_t)(di + (int)count * delta);
  cx = 0;
}

void rep_movsw_block_impl(uint16_t dst_seg, uint16_t src_seg,
                          const char *file, const char *func, int line) {
  if (cx == 0) return;
  uint32_t count_words = cx;
  uint32_t count_bytes = count_words * 2;
  int delta = DF ? -2 : 2;

  if (rep_would_wrap(si, count_bytes, delta) ||
      rep_would_wrap(di, count_bytes, delta)) {
    while (cx) {
      memw_write_impl(dst_seg, di,
                      memw_read_impl(src_seg, si, file, func, line),
                      file, func, line);
      si = (uint16_t)(si + delta);
      di = (uint16_t)(di + delta);
      cx = (uint16_t)(cx - 1);
    }
    return;
  }

  uint16_t src_first_off = DF ? (uint16_t)(si - count_bytes + 2) : si;
  uint16_t dst_first_off = DF ? (uint16_t)(di - count_bytes + 2) : di;
  uint32_t src_lo = linear_addr(src_seg, src_first_off);
  uint32_t dst_lo = linear_addr(dst_seg, dst_first_off);

  if (rep_range_touches_rcb(dst_seg, dst_lo, count_bytes) ||
      rep_range_touches_rcb(src_seg, src_lo, count_bytes) ||
      rep_range_touches_watch(dst_lo, count_bytes)) {
    while (cx) {
      memw_write_impl(dst_seg, di,
                      memw_read_impl(src_seg, si, file, func, line),
                      file, func, line);
      si = (uint16_t)(si + delta);
      di = (uint16_t)(di + delta);
      cx = (uint16_t)(cx - 1);
    }
    shim_jit_invalidate_code_range(dst_lo, count_bytes);
    return;
  }

  /* See rep_movsb_block_impl: memmove breaks the x86 overlap-replication
   * idiom. Replicate word-by-word in the DF direction when ranges overlap. */
  if (src_lo < dst_lo + count_bytes && dst_lo < src_lo + count_bytes) {
    uint32_t s = linear_addr(src_seg, si);
    uint32_t d = linear_addr(dst_seg, di);
    for (uint32_t i = 0; i < count_words; ++i) {
      uint8_t b0 = virtual_memory[mask_addr(s)];
      uint8_t b1 = virtual_memory[mask_addr((uint32_t)(s + 1))];
      virtual_memory[mask_addr(d)] = b0;
      virtual_memory[mask_addr((uint32_t)(d + 1))] = b1;
      s = (uint32_t)(s + delta);
      d = (uint32_t)(d + delta);
    }
  } else {
    memmove(virtual_memory + dst_lo, virtual_memory + src_lo, count_bytes);
  }
  warn_on_mutation(dst_lo, count_bytes, file, func, line);
  shim_jit_invalidate_code_range(dst_lo, count_bytes);

  si = (uint16_t)(si + (int)count_bytes * (DF ? -1 : 1));
  di = (uint16_t)(di + (int)count_bytes * (DF ? -1 : 1));
  cx = 0;
}

static const char *rcb_field_name(RCBField field) {
  switch (field) {
#define RCB_FIELD_CASE(name)                                                   \
  case name:                                                                   \
    return #name;
    RCB_FIELD_CASE(FIELD_1)
    RCB_FIELD_CASE(PROGRAM_SEG)
    RCB_FIELD_CASE(PREV_TIMER_VECTOR_OFF)
    RCB_FIELD_CASE(PREV_TIMER_VECTOR_SEG)
    RCB_FIELD_CASE(FIELD_5)
    RCB_FIELD_CASE(FIELD_6)
    RCB_FIELD_CASE(JOYSTICK_FLAG)
    RCB_FIELD_CASE(FIELD_8)
    RCB_FIELD_CASE(DATA_BUF1_OFF)
    RCB_FIELD_CASE(DATA_BUF1_SEG)
    RCB_FIELD_CASE(DATA_BUF2_OFF)
    RCB_FIELD_CASE(DATA_BUF2_SEG)
    RCB_FIELD_CASE(VIDEO_DRIVER_INDEX)
    RCB_FIELD_CASE(MUSIC_DRIVER_FLAG)
    RCB_FIELD_CASE(FIELD_15)
    RCB_FIELD_CASE(FIELD_16)
    RCB_FIELD_CASE(FIELD_17)
    RCB_FIELD_CASE(FIELD_18)
    RCB_FIELD_CASE(FIELD_19)
    RCB_FIELD_CASE(FIELD_20)
    RCB_FIELD_CASE(FIELD_21)
    RCB_FIELD_CASE(FIELD_22)
    RCB_FIELD_CASE(FIELD_23)
    RCB_FIELD_CASE(DATA_BASE_SEG)
    RCB_FIELD_CASE(FIELD_25)
    RCB_FIELD_CASE(FIELD_26)
    RCB_FIELD_CASE(FIELD_27)
    RCB_FIELD_CASE(FIELD_28)
    RCB_FIELD_CASE(FIELD_29)
    RCB_FIELD_CASE(FIELD_30)
    RCB_FIELD_CASE(FIELD_31)
    RCB_FIELD_CASE(FIELD_32)
    RCB_FIELD_CASE(FIELD_33)
    RCB_FIELD_CASE(FIELD_34)
    RCB_FIELD_CASE(FIELD_35)
    RCB_FIELD_CASE(FIELD_36)
    RCB_FIELD_CASE(FIELD_37)
    RCB_FIELD_CASE(PREV_KEYBOARD_VECTOR_OFF)
    RCB_FIELD_CASE(PREV_KEYBOARD_VECTOR_SEG)
#undef RCB_FIELD_CASE
  default:
    return "UNKNOWN";
  }
}

static void warn_rcb_overlap(const char *path, const void *addr, size_t len) {
  uint32_t base;
  uint32_t end;
  if (!try_memory_range(addr, len, &base, &end)) {
    return;
  }
  uint32_t rcb_base = ((uint32_t)es << 4) + 0xFF00;
  uint32_t rcb_end = rcb_base + 0x100;

  if (end <= rcb_base || base >= rcb_end) {
    return;
  }

  static const struct {
    RCBField field;
    size_t size;
  } fields[] = {
      {FIELD_1, 2},
      {PROGRAM_SEG, 2},
      {PREV_TIMER_VECTOR_OFF, 2},
      {PREV_TIMER_VECTOR_SEG, 2},
      {FIELD_5, 1},
      {FIELD_6, 1},
      {JOYSTICK_FLAG, 1},
      {FIELD_8, 1},
      {DATA_BUF1_OFF, 2},
      {DATA_BUF1_SEG, 2},
      {DATA_BUF2_OFF, 2},
      {DATA_BUF2_SEG, 2},
      {VIDEO_DRIVER_INDEX, 1},
      {MUSIC_DRIVER_FLAG, 1},
      {FIELD_15, 1},
      {FIELD_16, 1},
      {FIELD_17, 2},
      {FIELD_18, 1},
      {FIELD_19, 1},
      {FIELD_20, 2},
      {FIELD_21, 1},
      {FIELD_22, 1},
      {FIELD_23, 1},
      {DATA_BASE_SEG, 2},
      {FIELD_25, 1},
      {FIELD_26, 1},
      {FIELD_27, 1},
      {FIELD_28, 1},
      {FIELD_29, 1},
      {FIELD_30, 1},
      {FIELD_31, 1},
      {FIELD_32, 1},
      {FIELD_33, 1},
      {FIELD_34, 1},
      {FIELD_35, 1},
      {FIELD_36, 1},
      {FIELD_37, 1},
      {PREV_KEYBOARD_VECTOR_OFF, 2},
      {PREV_KEYBOARD_VECTOR_SEG, 2},
  };

  for (size_t i = 0; i < sizeof(fields) / sizeof(fields[0]); ++i) {
    uint32_t field_addr = rcb_base + (uint32_t)(fields[i].field - 0xFF00);
    uint32_t field_end = field_addr + (uint32_t)fields[i].size;
    if (base < field_end && end > field_addr) {
      shim_log_stdout("Warning: file %s overwrote RCB field %s\n", path,
                      rcb_field_name(fields[i].field));
    }
  }
}

static void warn_file_overlap(const char *path, const void *addr, size_t len) {
  uint32_t base;
  uint32_t end;
  if (!try_memory_range(addr, len, &base, &end)) {
    return;
  }
  for (size_t i = 0; i < file_mapping_count; ++i) {
    uint32_t f_base = file_mappings[i].base;
    uint32_t f_end = f_base + (uint32_t)file_mappings[i].len;
    if (base < f_end && end > f_base) {
      uint32_t overlap_start = base > f_base ? base : f_base;
      uint32_t overlap_end = end < f_end ? end : f_end;
      shim_log_stdout("WARNING: file %s overwrote %s at 0x%05X-0x%05X\n", path,
                      file_mappings[i].path, overlap_start, overlap_end);
      size_t overlap_len = overlap_end - overlap_start;
      size_t dump_len = overlap_len > 10 ? 10 : overlap_len;
      if (file_mappings[i].data) {
        const uint8_t *old_bytes =
            file_mappings[i].data + (overlap_start - f_base);
        const uint8_t *new_bytes =
            (const uint8_t *)addr + (overlap_start - base);
        shim_log_stdout("         old bytes:");
        for (size_t j = 0; j < dump_len; ++j) {
          shim_log_stdout(" %02X", old_bytes[j]);
        }
        shim_log_stdout("\n         new bytes:");
        for (size_t j = 0; j < dump_len; ++j) {
          shim_log_stdout(" %02X", new_bytes[j]);
        }
        shim_log_stdout("\n");
      }
    }
  }
}

static void warn_on_mutation(uint32_t addr, size_t size, const char *file,
                             const char *func, int line) {
  /* Cross-binary write detector. A loaded binary's region (.bin/.drv/
   * .sar/.exe) is read-only from the outside — only the binary's own
   * code may mutate it (self-modifying code, working buffers in the
   * data area). A write from binary X targeting binary Y's region is
   * a buffer overrun by X stomping Y's code/data; the next dispatch
   * through whatever Y holds at the corrupted offset will land at a
   * bogus target.
   *
   * "Source" binary is derived from cs:ip's file_mapping, NOT from
   * shim_active_binary(). The active-binary stack tracks _impl wrapper
   * entries — but ISRs (delivered via invoke_isr → game's IVT) jump
   * straight to the target binary's dispatch with no _impl wrapper, so
   * the stack still reflects whatever was running before the ISR.
   * cs:ip resolves to the actually-executing code. */
  /* Source binary derived from the C source `file` of the write
   * callsite, e.g. "/.../sounddrv.c". cs:ip-based attribution
   * doesn't work here: multiple binaries share canonical_cs (e.g.
   * two audio-driver modules both at 0x22A0), so linear cs*16+ip lands
   * in the first-mapped binary even when the actually-executing
   * code belongs to a later-mapped one. `__FILE__` is unambiguous. */
  const char *src_name = NULL;
  size_t src_name_len = 0;
  if (file) {
    const char *slash = strrchr(file, '/');
    src_name = slash ? slash + 1 : file;
    const char *dot = strrchr(src_name, '.');
    src_name_len = dot ? (size_t)(dot - src_name) : strlen(src_name);
  }
  /* The boot/init phase legitimately cross-binary-writes: the main
   * program installs the RCB pointer slots and the music driver's jump
   * table; an overlay module & friends rewrite parts of game.bin before
   * the main loop starts.
   * Enforce only AFTER first user input, since the symptom we want to
   * catch is corruption during gameplay (a single-segment binary's stosb overruns
   * post-input). shim_input_phase_started is set by the keyboard-
   * consumption hooks below. */
  if (!shim_input_phase_started) {
    return;
  }
  for (size_t i = 0; i < file_mapping_count; ++i) {
    uint32_t f_base = file_mappings[i].base;
    uint32_t f_end = f_base + (uint32_t)file_mappings[i].len;
    if (addr >= f_end || addr + size <= f_base) continue;
    const char *path = file_mappings[i].path;
    shim_log_stdout("Warning: mutation of %s at 0x%05X (%s:%s:%d)\n",
                    path, addr, file, func, line);
    if (!src_name || !path) continue;
    /* Compare source-file stem to target path basename without ext.
     * src_name = "sounddrv.c" → stem = "sounddrv".
     * path     = "sounddrv.drv" → basename-no-ext = "sounddrv".
     * Match = same binary, self-modifying code permitted. */
    const char *tgt_base = strrchr(path, '/');
    tgt_base = tgt_base ? tgt_base + 1 : path;
    const char *tgt_dot = strrchr(tgt_base, '.');
    size_t tgt_len = tgt_dot ? (size_t)(tgt_dot - tgt_base) : strlen(tgt_base);
    if (src_name_len == tgt_len &&
        strncmp(src_name, tgt_base, tgt_len) == 0) {
      continue;
    }
    /* A JIT chunk (name "jit_<seg5>_<off4>") writing within its own decode
     * segment is the same self-modification the named-binary case allows: the
     * chunk IS that segment's code (e.g. a JIT'd resident driver writing its
     * own data/jump-table). Without this, the chunk's synthetic name never
     * matches the .drv basename and legitimate driver self-writes abort. */
    if (src_name && src_name_len > 4 &&
        strncmp(src_name, "jit_", 4) == 0) {
      uint32_t chunk_seg = (uint32_t)strtoul(src_name + 4, NULL, 16);
      if (addr >= chunk_seg && addr < chunk_seg + 0x10000u) {
        continue;
      }
    }
    /* Scope: only treat .drv targets as protected. Music/sound driver
     * binaries (the .drv audio drivers) install function-pointer
     * tables that the timer ISR reads every tick; corruption of those
     * tables is the symptom we're chasing. Resource archives (.sar) and
     * game binaries (.bin) commonly contain mutable data regions —
     * sprite buffers, decompression output, save-state — where
     * cross-binary writes are part of normal play. */
    size_t plen = strlen(path);
    if (plen < 4 || strcmp(path + plen - 4, ".drv") != 0) {
      continue;
    }
    /* Small overlay chunks embedded inside another binary's region are
     * game-coded indirect pointers. Even inside .drv, a 4-byte slot is
     * a legitimate pointer cell, not a buffer the game would overrun. */
    if (file_mappings[i].len <= 16) {
      continue;
    }
    char msg[640];
    int n = snprintf(msg, sizeof(msg),
        "[CROSS-BINARY OVERWRITE] %.*s code wrote into %s @ 0x%05X "
        "size=%zu\n"
        "  cs:ip=%04X:%04X ds=%04X es=%04X ax=%04X bx=%04X cx=%04X "
        "dx=%04X si=%04X di=%04X bp=%04X ss:sp=%04X:%04X\n"
        "  via %s:%s:%d\n"
        "  diagnosis: code translated from %.*s wrote into %s's loaded "
        "region. Loaded binary regions are read-only from the outside; "
        "only the owning binary may mutate its own bytes. A cross-"
        "binary write is a buffer overrun stomping the target's code "
        "or data — the next dispatch through whatever the target holds "
        "at the corrupted offset will land at a bogus target. The "
        "cs:ip above is the instruction whose write overflowed.\n",
        (int)src_name_len, src_name, path, addr, size,
        cs, ip, ds, es, ax, bx, cx, dx, si, di, bp, ss, sp,
        file ? file : "?", func ? func : "?", line,
        (int)src_name_len, src_name, path);
    fprintf(stderr, "%s", msg);
    shim_log_crash("%s", msg);
    save_bug_bundle("cross_binary_overwrite", (uint32_t)addr, msg);
    shim_flush_all_streams();
    abort();
  }
}

uint8_t rcb_read8_impl(RCBField field, const char *file, const char *func,
                       int line) {
  uint8_t value = memb_raw(es, field);
  shim_log_stdout("Trace: rcb_read8 %s=0x%02X (%s:%s:%d)\n",
                  rcb_field_name(field), value, file, func, line);

  return value;
}

void rcb_write8_impl(RCBField field, uint8_t value, const char *file,
                     const char *func, int line) {
  shim_log_stdout("Trace: rcb_write8 %s=0x%02X (%s:%s:%d)\n",
                  rcb_field_name(field), value, file, func, line);
  write_watch_log(linear_addr(es, field), 1, value, file, func, line);
  memb_raw(es, field) = value;
}

uint16_t rcb_read16_impl(RCBField field, const char *file, const char *func,
                         int line) {
  uint16_t value = memw_raw_read(es, field);
  shim_log_stdout("Trace: rcb_read16 %s=0x%04X (%s:%s:%d)\n",
                  rcb_field_name(field), value, file, func, line);

  return value;
}

void rcb_write16_impl(RCBField field, uint16_t value, const char *file,
                      const char *func, int line) {
  shim_log_stdout("Trace: rcb_write16 %s=0x%04X (%s:%s:%d)\n",
                  rcb_field_name(field), value, file, func, line);
  write_watch_log(linear_addr(es, field), 2, value, file, func, line);
  memw_raw_write(es, field, value);
}

static void init_memory(void) __attribute__((constructor));

static void init_memory(void) {
  /* Honour a game-specific PSP load segment before anything derives PSP_SEG /
   * LOAD_SEG / ENV_SEG. 0 means "use the default layout". */
  if (game_config.psp_seg != 0) {
    psp_seg = game_config.psp_seg;
  }

  virtual_memory = calloc(1, MEMORY_SIZE);
  if (!virtual_memory) {
    shim_flush_all_streams();
    exit(1);
  }

  psp = (PSP *)seg_off(PSP_SEG, 0);
  image_base = virtual_memory + (LOAD_SEG << 4);
  init_psp();
  init_standard_handles();
  init_bios_data_area();
  for (int i = 0; i < 256; ++i) {
    uint16_t addr = (uint16_t)i * 4;
    memw_raw_write(0, addr, DEFAULT_ISR_OFF);
    memw_raw_write(0, addr + 2, DEFAULT_ISR_SEG);
  }
  /* Install basic DOS and BIOS ISRs. */
  memw_raw_write(0, 0x08 * 4, BIOS_IRQ0_ISR_OFF);
  memw_raw_write(0, 0x08 * 4 + 2, BIOS_IRQ0_ISR_SEG);
  memw_raw_write(0, 0x09 * 4, BIOS_IRQ1_ISR_OFF);
  memw_raw_write(0, 0x09 * 4 + 2, BIOS_IRQ1_ISR_SEG);
  memw_raw_write(0, 0x10 * 4, BIOS_VIDEO_ISR_OFF);
  memw_raw_write(0, 0x10 * 4 + 2, BIOS_VIDEO_ISR_SEG);
  memw_raw_write(0, 0x11 * 4, BIOS_EQUIPMENT_ISR_OFF);
  memw_raw_write(0, 0x11 * 4 + 2, BIOS_EQUIPMENT_ISR_SEG);
  memw_raw_write(0, 0x16 * 4, BIOS_KBD_ISR_OFF);
  memw_raw_write(0, 0x16 * 4 + 2, BIOS_KBD_ISR_SEG);
  memw_raw_write(0, 0x20 * 4, DOS_TERM_ISR_OFF);
  memw_raw_write(0, 0x20 * 4 + 2, DOS_TERM_ISR_SEG);
  memw_raw_write(0, 0x21 * 4, DOS_API_ISR_OFF);
  memw_raw_write(0, 0x21 * 4 + 2, DOS_API_ISR_SEG);
  memw_raw_write(0, 0x1A * 4, BIOS_TIMER_ISR_OFF);
  memw_raw_write(0, 0x1A * 4 + 2, BIOS_TIMER_ISR_SEG);
  memw_raw_write(0, 0x1C * 4, BIOS_TIMER_TICK_ISR_OFF);
  memw_raw_write(0, 0x1C * 4 + 2, BIOS_TIMER_TICK_ISR_SEG);
  memw_raw_write(0, 0x33 * 4, MOUSE_ISR_OFF);
  memw_raw_write(0, 0x33 * 4 + 2, MOUSE_ISR_SEG);
  last_host_time_ns = shim_virtual_now_ns();
  host_time_origin_ns = last_host_time_ns;
  pit_cycle_accum = 0;
  pit_cycle_fraction_accum = 0;
  pit_reload_value = 0x10000;
  pit_latch_valid = 0;
  pit_read_expect_high = 0;
  pit_read_buffer_is_latch = 0;
  last_present_time_ns = last_host_time_ns;
  last_screenshot_time_ns = last_host_time_ns;

  cga.crtc_index = 0;
  memset(cga.crtc_regs, 0, sizeof(cga.crtc_regs));
  cga.hsync_base = 0;
  cga.horiz_scroll = 0;
  cga.hsync_initialized = 0;

  /* Set up initial register state. */
  cpu.r_ds = PSP_SEG;
  cpu.r_es = PSP_SEG;
  cpu.r_cs = LOAD_SEG;
  cpu.r_ss = LOAD_SEG;
  DF = 0;
  IF = 1;
  next_free_seg = LOAD_SEG;
  program_min_block_paras = (uint16_t)(LOAD_SEG - PSP_SEG);
  uint16_t new_cs, new_ip, new_ss, new_sp;
  const char *program_path = game_config.program_path;
  if (!program_path) {
    program_path = "program.exe";
  }
  if (load_executable(program_path, LOAD_SEG, 0, &new_cs, &new_ip, &new_ss,
                      &new_sp) == 0) {
    cpu.r_cs = new_cs;
    ip = new_ip;
    cpu.r_ss = new_ss;
    sp = new_sp;
  }
  for (int i = 0; i < 16; ++i) {
    null_guard_initial[i] = virtual_memory[i];
  }
}

static uint8_t port61;
static uint8_t port92;
static uint16_t dma3_addr;
static uint8_t dma_ff; /* DMA address register flip-flop */
/* OPL2 (AdLib) sound card now lives in hw/audio.c (Opl2State opl2 +
 * opl2_port_read/opl2_port_write); the IO-port dispatch below forwards
 * ports 0x388/0x389 there. */

/* Sound Blaster DSP (base 0x220). Only the detection handshake is modeled:
 * a real DSP, after the reset pulse (write 1 then 0 to 2x6), places 0xAA in
 * its read-data buffer; software polls 2xE bit 7 for "data available" and
 * reads the byte from 2xA, treating 0xAA as "card present". Command writes
 * (2xC) are accepted; the write-status (2xC) bit 7 is "DSP busy" and is held
 * clear so software never spins. This is the faithful presence response of a
 * Sound Blaster whose digital playback path is otherwise idle -- some
 * programs' audio init probes it during startup before drawing the title. */
static uint8_t sb_dsp_read_data = 0xFF; /* last byte the DSP made readable */
static uint8_t sb_dsp_read_ready;       /* 1 => 2xA has a byte to read */
static uint8_t sb_dsp_reset_state;      /* tracks the 1->0 reset pulse */

static int sb_dsp_port(uint16_t port) {
  return port >= 0x220 && port <= 0x22F;
}

static void sb_dsp_write(uint16_t port, uint8_t value) {
  switch (port & 0x00F) {
  case 0x6: /* DSP reset */
    if (value & 1) {
      sb_dsp_reset_state = 1;
    } else if (sb_dsp_reset_state) {
      /* Falling edge completes the reset: DSP signals readiness with 0xAA. */
      sb_dsp_reset_state = 0;
      sb_dsp_read_data = 0xAA;
      sb_dsp_read_ready = 1;
    }
    break;
  case 0xC: /* DSP write command/data -- accept (no playback path modeled) */
  default:
    break;
  }
}

static uint8_t sb_dsp_read(uint16_t port) {
  switch (port & 0x00F) {
  case 0xA: { /* DSP read data */
    uint8_t v = sb_dsp_read_data;
    sb_dsp_read_ready = 0;
    return v;
  }
  case 0xC: /* DSP write-buffer status: bit 7 = busy (held clear = ready) */
    return 0x00;
  case 0xE: /* DSP read-buffer status: bit 7 = data available */
    return sb_dsp_read_ready ? 0x80 : 0x00;
  default:
    return 0xFF;
  }
}

/*
 * Control A20 gate state.  Keep the cached port 0x92 value in sync so reads
 * accurately reflect the current state regardless of how it was toggled.
 */
void a20_set_enabled(bool enabled) {
  a20_enabled = enabled;
  if (enabled)
    port92 |= 0x02;
  else
    port92 &= ~0x02;
}

static void init_a20(void) __attribute__((constructor));
static void init_a20(void) { a20_set_enabled(true); }


static PITState *pit_state_for_channel(uint8_t channel) {
  switch (channel) {
  case 0:
    return &pit;
  case 1:
    return &pit_channel1;
  case 2:
    return &pit_channel2;
  default:
    return NULL;
  }
}

static void pit_commit_reload(PITState *state, uint8_t channel) {
  uint32_t reload_value = state->temp_reload ? state->temp_reload : 0x10000;
  /* The reload is the channel's count in PIT cycles (0 == 65536). It must be
   * used FAITHFULLY: games set a short channel-0 reload to time precise polled
   * delays off the latched counter (some programs use 49/53). The old
   * `/ 200` scaling shrank that to ~1, making the counter -- and any delay
   * read from it -- ~50x too fast. The BIOS time-of-day tick is kept at 18.2 Hz
   * separately (see bios_tick_cycle_debt), so a fast reload no longer races the
   * wall clock. */
  uint32_t reload_ticks = reload_value;

  state->reload = reload_ticks;

  if (channel == 0) {
    pit_reload_value = (uint16_t)(reload_value & 0xFFFF);
    pit_read_expect_high = 0;
    pit_latch_valid = 0;
  }
}

static void pit_write_data(uint8_t channel, uint8_t value) {
  PITState *state = pit_state_for_channel(channel);
  if (!state)
    return;

  switch (state->access_mode) {
  case 0x1: /* lobyte only */
    state->temp_reload = (state->temp_reload & 0xFF00) | value;
    pit_commit_reload(state, channel);
    state->expect_high = 0;
    break;
  case 0x2: /* hibyte only */
    state->temp_reload = (state->temp_reload & 0x00FF) | ((uint16_t)value << 8);
    pit_commit_reload(state, channel);
    state->expect_high = 0;
    break;
  case 0x3: /* lobyte/hibyte */
    if (!state->expect_high) {
      state->temp_reload = (state->temp_reload & 0xFF00) | value;
      state->expect_high = 1;
    } else {
      state->temp_reload = (state->temp_reload & 0x00FF) | ((uint16_t)value << 8);
      pit_commit_reload(state, channel);
      state->expect_high = 0;
    }
    break;
  default:
    break;
  }
}

uint8_t inb(uint16_t port) {
  const IoDevice *dev = io_bus_lookup(port);
  if (dev != NULL) {
    return dev->read8(port);
  }
  if (port == 0x60) {
    if (kbd.scancode_ready) {
      uint8_t sc = kbd.scancode;
      uint8_t asc = kbd.ascii;
      kbd.last_scancode = sc;
      /* Port 0x60 delivery path (game's INT 09h ISR reads). Snapshot only
       * on make scancodes (high bit clear) — break events are frequent and
       * aren't a useful resume point. */
      if ((sc & 0x80) == 0) {
        shim_input_phase_started = 1;
        snapshot_on_key_consumed();
        /*
         * Remember the make keystroke for the chained BIOS INT 09h handler.
         * On a real PC the same scancode stays latched at port 0x60 until the
         * controller loads the next one, so the BIOS handler reads the same
         * value the game's ISR just read. Our kbd_consume() advances the
         * queue, so stash it here for kbd_bios_deposit_from_isr() to translate
         * into the BIOS type-ahead buffer.
         */
        kbd.pending_bios_ascii = asc;
        kbd.pending_bios_scancode = sc;
        kbd.pending_bios_valid = 1;
      }
      kbd_consume();
      return sc;
    }
    return kbd.last_scancode;
  }
  if (port == 0x61)
    return port61;
  if (port == 0x40) {
    uint8_t ret = 0;
    if (pit.access_mode == 0x3) {
      if (!pit_read_expect_high) {
        if (pit_latch_valid) {
          pit_read_buffer = pit_latched_value;
          pit_read_buffer_is_latch = 1;
        } else {
          pit_read_buffer = pit_current_count();
          pit_read_buffer_is_latch = 0;
        }
        ret = (uint8_t)(pit_read_buffer & 0xFF);
        pit_read_expect_high = 1;
        return ret;
      }
      ret = (uint8_t)((pit_read_buffer >> 8) & 0xFF);
      pit_read_expect_high = 0;
      if (pit_read_buffer_is_latch) {
        pit_latch_valid = 0;
        pit_read_buffer_is_latch = 0;
      }
      return ret;
    }

    uint16_t value;
    if (pit_latch_valid) {
      value = pit_latched_value;
      pit_latch_valid = 0;
    } else {
      value = pit_current_count();
    }
    pit_read_expect_high = 0;
    pit_read_buffer_is_latch = 0;

    switch (pit.access_mode) {
    case 0x1:
      ret = (uint8_t)(value & 0xFF);
      break;
    case 0x2:
      ret = (uint8_t)((value >> 8) & 0xFF);
      break;
    default:
      ret = (uint8_t)(value & 0xFF);
      break;
    }
    return ret;
  }
  if (port == 0x42) {
    /* PIT channel 2 counter read. Channel 2 free-runs at 1.193182 MHz; many
     * programs read it purely as a fast-changing timing/entropy source (some
     * programs' audio init seeds from it). Derive the current count down from
     * the scaled monotonic clock and sequence lo/hi bytes per the channel's
     * latched access mode, exactly as the 8254 presents them. */
    uint16_t reload2 = pit_channel2.reload ? (uint16_t)pit_channel2.reload : 0;
    uint64_t ticks = (shim_scaled_monotonic_ns() * 1193182ull) / 1000000000ull;
    uint16_t count;
    if (reload2 == 0) {
      count = (uint16_t)(0 - (uint16_t)(ticks & 0xFFFF)); /* full 16-bit wrap */
    } else {
      count = (uint16_t)(reload2 - (uint16_t)(ticks % reload2));
    }
    if (pit_channel2.access_mode == 0x1)
      return (uint8_t)(count & 0xFF);
    if (pit_channel2.access_mode == 0x2)
      return (uint8_t)((count >> 8) & 0xFF);
    /* mode 3: lo byte then hi byte on successive reads */
    if (!pit_channel2.expect_high) {
      pit_channel2.expect_high = 1;
      return (uint8_t)(count & 0xFF);
    }
    pit_channel2.expect_high = 0;
    return (uint8_t)((count >> 8) & 0xFF);
  }
  if (port == 0x92)
    return port92;
  if (port == 0x06) {
    /* DMA channel 3 address register */
    uint8_t val = dma_ff ? (dma3_addr >> 8) & 0xFF : dma3_addr & 0xFF;
    dma_ff ^= 1;
    return val;
  }
  if (port == 0x64) {
    return kbd.scancode_ready ? 0x01 : 0x00;
  }
  if (port == 0x201) {
    /* Joystick port - no joystick connected */
    return 0xFF;
  }
  if (port == 0x3C2 || port == 0x3CC)
    return vga.misc_output;
  if (port == 0x3CD)
    return vga.feature_control;
  if (port == 0x3C9) {
    /* PEL Data read: return the stored 6-bit DAC component at the read index
     * and advance R->G->B then to the next palette entry, mirroring the write
     * path. The read index was set via 0x3C7. (Some programs read back the
     * palette during their colour fades.) */
    uint8_t comp = vga.palette[vga.palette_read_index * 3 + vga.palette_component];
    if (++vga.palette_component == 3) {
      vga.palette_component = 0;
      ++vga.palette_read_index;
    }
    return (uint8_t)(comp & 0x3F);
  }
  if (port == 0x3C8)
    return vga.palette_write_index; /* PEL address register read-back */
  if (port == 0x3C6)
    return vga.palette_mask;
  if (port == 0x3BA || port == 0x3DA) {
    /*
     * VGA input status register 1. Many programs poll bit 3 for the start of
     * the vertical retrace and will busy-wait if it never changes. Toggle the
     * bit at roughly 60 Hz to keep such loops moving.
     */
    uint64_t now_ms = shim_scaled_monotonic_ns() / 1000000ull;
    static uint64_t last_toggle_ms;
    static uint8_t in_vsync;
    if (now_ms - last_toggle_ms >= 16) {
      in_vsync ^= 1;
      last_toggle_ms = now_ms;
    }
    uint8_t status = 0;
    if (in_vsync) {
      status |= 0x08; /* vertical retrace */
    } else {
      status |= 0x01; /* display enable */
    }
    return status;
  }
  if (port == 0x3B4 || port == 0x3B5) {
    /* Monochrome (MDA/Hercules) CRTC ports. We emulate a VGA/colour machine
     * (matching the game), which has NO adapter decoding these -- so reads
     * float to 0xFF. This deliberately FAILS the setup's 6845 presence probe
     * (write 0x66 to reg 0x0F, read back, compare): a phantom mono card would
     * make setup render its menu to the B000 mono buffer instead of B800. */
    return 0xFF;
  }
  if (port == 0x3D4) {
    /* Colour (CGA/EGA/VGA) CRTC address register: reads back the index. */
    return cga.crtc_index;
  }
  if (port == 0x3D5) {
    /* Colour CRTC data register: reads back the value last written to the
     * selected index (consistent read-back, as a real 6845 provides). */
    return cga.crtc_regs[cga.crtc_index & 0x1F];
  }
  if (sb_dsp_port(port))
    return sb_dsp_read(port);
  io_port_error(__func__, port);
  return 0;
}

uint16_t inw(uint16_t port) {
  /* A 16-bit IN on the ISA bus is two byte cycles: the low byte from `port`
   * and the high byte from `port+1` (an 8-bit card decodes each port
   * separately). Compose from inb so any port pair a game word-reads is
   * served by the same per-port handlers -- e.g. the AdLib status register at
   * 0x388 (low) and the OPL2 data port at 0x389 (high). */
  uint8_t lo = inb(port);
  uint8_t hi = inb((uint16_t)(port + 1));
  return (uint16_t)(lo | ((uint16_t)hi << 8));
}

void outb(uint16_t port, uint8_t value) {
  const IoDevice *dev = io_bus_lookup(port);
  if (dev != NULL) {
    dev->write8(port, value);
    return;
  }
  switch (port) {
  case 0x06:
    /* DMA channel 3 address register */
    if (dma_ff)
      dma3_addr = (dma3_addr & 0x00FF) | ((uint16_t)value << 8);
    else
      dma3_addr = (dma3_addr & 0xFF00) | value;
    dma_ff ^= 1;
    break;
  case 0x0C:
    /* DMA clear first/last flip-flop */
    dma_ff = 0;
    break;
  case 0x20:
    /* PIC1 command port - acknowledge interrupt */
    if (value == 0x20) {
      irq0_pending = 0;
    }
    break;
  case 0x201:
    /* Joystick strobe; currently ignored in the null joystick backend. */
    break;
  case 0x43: {
    uint8_t channel = (value >> 6) & 0x03;
    uint8_t access = (value >> 4) & 0x03;
    PITState *state = pit_state_for_channel(channel);
    if (!state) {
      /* Read-back command (channel == 3) not yet implemented. */
      break;
    }
    if (access == 0x00) {
      if (channel == 0) {
        pit_latched_value = pit_current_count();
        pit_latch_valid = 1;
        pit_read_expect_high = 0;
      }
    } else {
      state->access_mode = access;
      state->expect_high = 0;
      if (channel == 0)
        pit_latch_valid = 0;
    }
    break;
  }
  case 0x40:
    pit_write_data(0, value);
    break;
  case 0x41:
    pit_write_data(1, value);
    break;
  case 0x42:
    pit_write_data(2, value);
    break;
  case 0x61: {
    uint8_t old = port61;
    port61 = value;
    // Bit 7 pulse emulates keyboard controller reset
    if ((old & 0x80) == 0 && (value & 0x80)) {
      // rising edge: optional—nothing required
    }
    if ((old & 0x80) && !(value & 0x80)) {
      /* Falling edge is the standard PC keyboard IRQ ACK pulse — the game
       * tells the 8042 controller it has consumed the current scancode.
       * On real hardware this does NOT clear the keyboard buffer; the next
       * pending scancode (if any) is presented immediately. Clearing the
       * whole queue here used to drop the key-release events for stdin-
       * driven input, making keys appear stuck. */
    }
    break;
  }
  case 0x3B4:
  case 0x3B5:
    /* Monochrome (MDA/Hercules) CRTC -- no such adapter on this emulated
     * VGA/colour machine, so writes are discarded (matches inb's 0xFF). */
    break;
  case 0x3D4:
    cga.crtc_index = (uint8_t)(value & 0x1F);
    break;
  case 0x3D5: {
    uint8_t idx = (uint8_t)(cga.crtc_index & 0x1F);
    cga.crtc_regs[idx] = value;
    if (idx == 0x02) {
      if (!cga.hsync_initialized) {
        cga.hsync_initialized = 1;
        cga.hsync_base = value;
        cga.horiz_scroll = 0;
      } else {
        int delta = (int)value - (int)cga.hsync_base;
        if (delta >= 8 || delta <= -8) {
          delta %= 8;
        }
        cga.horiz_scroll = delta;
      }
    }
    break;
  }
  case 0x3D8: {
    /* CGA mode control register */
    uint8_t new_mode = bios_video.video_mode;
    uint8_t new_palette = bios_video.cga_palette_select;
    int graphics = (value >> 1) & 0x01;
    if (!graphics) {
      int high_res_text = value & 0x01;
      int black_and_white = (value >> 2) & 0x01;
      if (high_res_text) {
        new_mode = black_and_white ? 0x02 : 0x03;
      } else {
        new_mode = 0x00;
      }
      new_palette = 0;
    } else {
      int high_res_graphics = (value >> 4) & 0x01;
      int black_and_white = (value >> 2) & 0x01;
      if (high_res_graphics) {
        new_mode = 0x06;
        new_palette = 0x02;
      } else {
        new_mode = black_and_white ? 0x05 : 0x04;
        new_palette = 0x00;
      }
    }

    if (new_palette != bios_video.cga_palette_select) {
      bios_video.cga_palette_select = new_palette;
      memb_raw(0x40, 0x0066) = new_palette;
    }
    video_invalidate_palette_cache();
    apply_video_mode_state(new_mode);
    break;
  }
  case 0x3D9: {
    /* CGA color select register */
    memb_raw(0x40, 0x0066) = value;
    bios_video.cga_border_color = value & 0x0F;
    vga.border_color = bios_video.cga_border_color;

    uint8_t palette_select = 0;
    if (value & 0x10)
      palette_select |= 0x01; /* palette select */
    if (value & 0x20)
      palette_select |= 0x02; /* intensity */
    if (value & 0x08)
      palette_select |= 0x04; /* background intensity */

    if (palette_select != bios_video.cga_palette_select) {
      bios_video.cga_palette_select = palette_select;
    }
    video_invalidate_palette_cache();
    break;
  }
  case 0x92:
    port92 = value;
    a20_set_enabled((value & 0x02) != 0);
    break;
  case 0x3C2:
  case 0x3CC:
    /* Miscellaneous Output Register.  Some binaries address the read mirror */
    /* at 0x3CC when writing, so accept both aliases. */
    vga.misc_output = value;
    break;
  case 0x3CD:
    /* Feature Control Register (color emulation). */
    vga.feature_control = value;
    break;
  case 0x3C8:
    /* PEL Write Address */
    vga.palette_write_index = value;
    vga.palette_component = 0;
    break;
  case 0x3C9:
    /* PEL Data */
    vga.palette[vga.palette_write_index * 3 + vga.palette_component] =
        vga_dac_component(value);
    if (++vga.palette_component == 3) {
      vga.palette_component = 0;
      ++vga.palette_write_index;
    }
    break;
  case 0x3C7:
    /* PEL Read Address */
    vga.palette_read_index = value;
    vga.palette_component = 0;
    break;
  case 0x3C6:
    /* PEL Mask */
    vga.palette_mask = value;
    break;
  case 0x3CE:
    /* VGA Graphics Controller index */
    vga.graphics_index = (uint8_t)(value & 0x0F);
    break;
  case 0x3CF:
    /* VGA Graphics Controller data */
    vga.graphics_regs[vga.graphics_index & 0x0F] = value;
    break;
  default:
    if (sb_dsp_port(port)) {
      sb_dsp_write(port, value);
      break;
    }
    io_port_error(__func__, port);
  }
}

void outw(uint16_t port, uint16_t value) {
  /* Symmetric with inw: a 16-bit OUT is two byte cycles -- low byte to `port`,
   * high byte to `port+1`. This is exactly the VGA index/data pair idiom (e.g.
   * 0x3CE/0x3CF) and works for any device whose registers sit on consecutive
   * ports, routed through the same per-port outb handlers. */
  outb(port, (uint8_t)(value & 0x00FF));
  outb((uint16_t)(port + 1), (uint8_t)((value >> 8) & 0x00FF));
}

uint8_t compareMemoryUntilMismatch(const uint8_t *src, const uint8_t *dst,
                                   uint16_t count, int direction) {
  CRITICAL_ENTER();
  uint32_t src_addr = (uint32_t)(src - virtual_memory);
  uint32_t dst_addr = (uint32_t)(dst - virtual_memory);
  for (uint16_t i = 0; i < count; ++i) {
    if (*src != *dst) {
      CRITICAL_EXIT();
      return 0;
    }
    src_addr = (src_addr & ~0xFFFF) | ((src_addr + direction) & 0xFFFF);
    dst_addr = (dst_addr & ~0xFFFF) | ((dst_addr + direction) & 0xFFFF);
    src = virtual_memory + src_addr;
    dst = virtual_memory + dst_addr;
  }
  CRITICAL_EXIT();
  return 1;
}

uint16_t scanMemoryForAl(const uint8_t *dst, uint8_t value, uint16_t count,
                         int direction, uint8_t *last_byte) {
  CRITICAL_ENTER();
  uint32_t addr = (uint32_t)(dst - virtual_memory);
  uint16_t i = 0;
  uint8_t byte = 0;
  static int scan_log_count;
  if (scan_log_count < 50) {
    shim_log_stdout(
        "Trace: scanMemoryForAl dst=%04X:%04X value=0x%02X count=%u dir=%d",
        (uint16_t)(addr >> 4), (uint16_t)(addr & 0xF), value, count, direction);
    uint32_t preview_addr = addr;
    const uint8_t *preview_ptr = dst;
    shim_log_stdout(" bytes:");
    for (uint16_t j = 0; j < count && j < 8; ++j) {
      shim_log_stdout(" %02X", *preview_ptr);
      preview_addr = (preview_addr & ~0xFFFF) |
                     ((preview_addr + direction) & 0xFFFF);
      preview_ptr = virtual_memory + preview_addr;
    }
    shim_log_stdout("\n");
    ++scan_log_count;
  }
  if (count > 0) {
    byte = *dst;
    while (i < count && byte != value) {
      addr = (addr & ~0xFFFF) | ((addr + direction) & 0xFFFF);
      dst = virtual_memory + addr;
      ++i;
      if (i < count) {
        byte = *dst;
      }
    }
  }
  if (i >= count && count > 0 && byte != value) {
    uint16_t seg = (uint16_t)(addr >> 4);
    uint16_t off = (uint16_t)(addr & 0xF);
    shim_log_stdout(
        "Warning: scanMemoryForAl miss value=0x%02X count=%u final=%04X:%04X\n",
        value, count, seg, off);
  }
  if (last_byte) {
    *last_byte = byte;
  }
  CRITICAL_EXIT();
  return i;
}



static void int08h_impl(uint16_t expected_retip, const char *file, const char *func, int line) {
  shim_log(__func__, file, func, line, NULL);
  uint8_t preincremented = bios_timer_tick_preincremented;
  bios_timer_tick_preincremented = 0;
  if (!preincremented) {
    bios_timer_increment();
  }
  invoke_isr(0x1C, 1, 1, 1, ip, "<int08>", func, line);
  iret_impl(file, func, line);
}

static void int09h_impl(uint16_t expected_retip, const char *file, const char *func, int line) {
  shim_log(__func__, file, func, line, NULL);
  /*
   * Faithful BIOS IRQ1 handler: translate the scancode the keyboard controller
   * presented at port 0x60 and store the ASCII+scancode word into the BIOS
   * type-ahead buffer (40:1E) that INT 16h and the DOS console services read.
   *
   * This runs in two situations, both handled by kbd_bios_deposit_from_isr():
   *   1. A game installed its own INT 09h ISR which read port 0x60 and then
   *      chained here (e.g. some programs). The make keystroke is waiting in
   *      the pending-deposit latch.
   *   2. No game ISR is installed, so this is the default IRQ1 handler. It then
   *      reads the scancode off the hardware queue itself.
   * Without this the game's own ISR would drain the keystroke from port 0x60
   * and the BIOS buffer would stay empty, so AH=06/INT 16h never see the key.
   */
  kbd_bios_deposit_from_isr();
  iret_impl(file, func, line);
}

static void int11h_impl(uint16_t expected_retip, const char *file, const char *func, int line) {
  shim_log(__func__, file, func, line, NULL);
  ax = memw_raw_read(0x40, 0x0010);
  iret_impl(file, func, line);
}

static void int10h_impl(uint16_t expected_retip, const char *file, const char *func, int line) {
  shim_log(__func__, file, func, line, NULL);
  if (ah == 0x00) {
    bios_set_video_mode_impl(al, file, func, line);
  } else if (ah == 0x02) {
    bios_set_cursor_position_impl(bh, dh, dl, file, func, line);
  } else if (ah == 0x03) {
    bios_get_cursor(bh);
  } else if (ah == 0x09) {
    bios_write_char_attr(al, bh, bl, cx);
  } else if (ah == 0x0A) {
    bios_write_char_only(al, bh, cx);
  } else if (ah == 0x0F) {
    al = bios_current_video_mode();
    ah = bios_current_video_columns();
    bh = bios_current_active_page();
  } else if (ah == 0x06) {
    bios_scroll_window(al, bh, ch, cl, dh, dl, 0);
  } else if (ah == 0x07) {
    bios_scroll_window(al, bh, ch, cl, dh, dl, 1);
  } else if (ah == 0x0B) {
    bios_set_cga_palette_impl(bh, bl, file, func, line);
  } else if (ah == 0x0E) {
    bios_teletype_output_impl(al, bh, bl, file, func, line);
  } else if (ah == 0x1A) {
    al = 0x1A;
    bl = bios_display_combination_code();
    bh = bios_display_combination_alt_code();
  } else if (ah == 0x10) {
    bios_set_palette_impl(file, func, line);
  } else if (ah == 0x12) {
    bios_video_alt_select_impl(file, func, line);
  } else if (ah == 0x08) {
    ax = bios_read_char_attr();
  } else if (ah == 0x30) {
    uint16_t seg, off;
    bios_get_video_parameter_block(al, &seg, &off);
    cx = seg;
    dx = off;
  } else {
    char msg[256];
    snprintf(msg, sizeof(msg),
             "unhandled BIOS video AH=0x%02X (%s:%s:%d)", ah, file, func, line);
    shim_log_crash("%s\n", msg);
    save_bug_bundle("unimplemented_bios", ((uint32_t)cs << 4) + ip, msg);
    shim_flush_all_streams();
    abort();
  }
  iret_impl(file, func, line);
}

static void int16h_impl(uint16_t expected_retip, const char *file, const char *func, int line) {
  shim_log(__func__, file, func, line, NULL);
  bios_keyboard_impl(file, func, line);
  iret_impl(file, func, line);
}

static void int20h_impl(uint16_t expected_retip, const char *file, const char *func, int line) {
  shim_log(__func__, file, func, line, NULL);
  dos_exit_impl(file, func, line);
}

static void int21h_impl(uint16_t expected_retip, const char *file, const char *func, int line) {
  shim_log(__func__, file, func, line, NULL);
  dos_api_impl(file, func, line);
  iret_impl(file, func, line);
}

static void int1Ah_impl(uint16_t expected_retip, const char *file, const char *func, int line) {
  shim_log(__func__, file, func, line, NULL);
  switch (ah) {
  case 0x00: {
    /* Read system-timer tick count. Real BIOS returns AL = the midnight
     * day-rollover flag from 0040:0070 and then CLEARS it. */
    uint32_t ticks = memw_raw_read(0x40, 0x006C);
    ticks |= (uint32_t)memw_raw_read(0x40, 0x006E) << 16;
    cx = (uint16_t)(ticks >> 16);
    dx = (uint16_t)(ticks & 0xFFFF);
    al = memb_raw(0x40, 0x70);
    memb_raw(0x40, 0x70) = 0;
    set_iret_carry(0);
    break;
  }
  case 0x01: {
    /* Set system-timer tick count: CX:DX -> 0040:006C/006E, clear rollover. */
    memw_raw_write(0x40, 0x006C, dx);
    memw_raw_write(0x40, 0x006E, cx);
    memb_raw(0x40, 0x70) = 0;
    set_iret_carry(0);
    break;
  }
  case 0x1C: {
    uint32_t ticks = memw_raw_read(0x40, 0x006C);
    ticks |= (uint32_t)memw_raw_read(0x40, 0x006E) << 16;
    cx = (uint16_t)(ticks >> 16);
    dx = (uint16_t)(ticks & 0xFFFF);
    set_iret_carry(0);
    break;
  }
  case 0x02: {
    time_t now = time(NULL);
    struct tm local_tm;
    struct tm *tm_ptr = localtime(&now);
    if (tm_ptr) {
      local_tm = *tm_ptr;
    } else {
      memset(&local_tm, 0, sizeof(local_tm));
    }
    ch = to_bcd((uint8_t)local_tm.tm_hour);
    cl = to_bcd((uint8_t)local_tm.tm_min);
    dh = to_bcd((uint8_t)local_tm.tm_sec);
    dl = (uint8_t)((local_tm.tm_isdst > 0) ? 1 : 0);
    set_iret_carry(0);
    break;
  }
  case 0x04: {
    time_t now = time(NULL);
    struct tm local_tm;
    struct tm *tm_ptr = localtime(&now);
    if (tm_ptr) {
      local_tm = *tm_ptr;
    } else {
      memset(&local_tm, 0, sizeof(local_tm));
    }
    uint16_t year = (uint16_t)(local_tm.tm_year + 1900);
    ch = to_bcd((uint8_t)(year / 100));
    cl = to_bcd((uint8_t)(year % 100));
    dh = to_bcd((uint8_t)(local_tm.tm_mon + 1));
    dl = to_bcd((uint8_t)local_tm.tm_mday);
    al = 0;
    set_iret_carry(0);
    break;
  }
  default: {
    char msg[256];
    snprintf(msg, sizeof(msg),
             "unhandled BIOS timer AH=0x%02X (%s:%s:%d)", ah, file, func, line);
    shim_log_crash("%s\n", msg);
    save_bug_bundle("unimplemented_bios", ((uint32_t)cs << 4) + ip, msg);
    shim_flush_all_streams();
    abort();
  }
  }
  iret_impl(file, func, line);
}

static void int1Ch_impl(uint16_t expected_retip, const char *file, const char *func, int line) {
  shim_log(__func__, file, func, line, NULL);
  iret_impl(file, func, line);
}

static void int33h_impl(uint16_t expected_retip, const char *file, const char *func, int line) {
  shim_log(__func__, file, func, line, NULL);
  mouse_int33_impl(file, func, line);
  iret_impl(file, func, line);
}

static const CallTarget base_call_targets[] = {
    {DEFAULT_ISR_LINEAR, NULL, default_isr_impl},
    {BIOS_IRQ0_ISR_LINEAR, NULL, int08h_impl},
    {BIOS_IRQ1_ISR_LINEAR, NULL, int09h_impl},
    {BIOS_VIDEO_ISR_LINEAR, NULL, int10h_impl},
    {BIOS_EQUIPMENT_ISR_LINEAR, NULL, int11h_impl},
    {BIOS_KBD_ISR_LINEAR, NULL, int16h_impl},
    {DOS_TERM_ISR_LINEAR, NULL, int20h_impl},
    {DOS_API_ISR_LINEAR, NULL, int21h_impl},
    {BIOS_TIMER_ISR_LINEAR, NULL, int1Ah_impl},
    {BIOS_TIMER_TICK_ISR_LINEAR, NULL, int1Ch_impl},
    {MOUSE_ISR_LINEAR, NULL, int33h_impl},
};

static const size_t base_call_target_count =
    sizeof(base_call_targets) / sizeof(base_call_targets[0]);

static int is_builtin_call_target(uint32_t addr) {
  for (size_t i = 0; i < base_call_target_count; ++i) {
    if (base_call_targets[i].addr == addr) {
      return 1;
    }
  }
  return 0;
}

/* Try to resolve ``addr`` to a registered CallTarget. Returns NULL on miss
 * (without aborting). Used by callers that want to try dispatch routing as
 * a fallback before crashing. */
static GameFunc try_call_target(uint32_t addr) {
  const FileMapping *m = find_file_mapping(addr);
  const char *mapped_file = NULL;
  if (m && m->path) {
    const char *slash = strrchr(m->path, '/');
    mapped_file = slash ? slash + 1 : m->path;
  }
  for (size_t i = 0; i < base_call_target_count; ++i) {
    if (base_call_targets[i].addr == addr) {
      if ((base_call_targets[i].file == NULL && mapped_file == NULL) ||
          (base_call_targets[i].file && mapped_file &&
           strcmp(base_call_targets[i].file, mapped_file) == 0)) {
        return base_call_targets[i].fn;
      }
    }
  }
  if (game_config.call_targets) {
    for (size_t i = 0; i < game_config.call_target_count; ++i) {
      const CallTarget *target = &game_config.call_targets[i];
      if (target->addr == addr) {
        if ((target->file == NULL && mapped_file == NULL) ||
            (target->file && mapped_file &&
             strcmp(target->file, mapped_file) == 0)) {
          return target->fn;
        }
      }
    }
  }
  return NULL;
}

static GameFunc lookup_call_target(uint32_t addr, const char *kind,
                                   const char *file, const char *func,
                                   int line) {
  // Log the linear address with a checksum of the next 8 bytes so we can
  // verify that the target function has not changed between runs.
  uint8_t sample[8];
  for (int i = 0; i < 8; ++i) {
    sample[i] = virtual_memory[mask_addr(addr + i)];
  }
  unsigned int checksum = stbiw__crc32(sample, sizeof(sample));
  const FileMapping *m = find_file_mapping(addr);
  const char *mapped_file = NULL;
  uint32_t offset = 0;
  if (m) {
    const char *slash = strrchr(m->path, '/');
    mapped_file = slash ? slash + 1 : m->path;
    offset = (uint32_t)(m->file_offset + (addr - m->base));
  }

  shim_log_stdout(
      "Trace: lookup_call_target: 0x%08X checksum 0x%08X (%s+0x%X)\n", addr,
      checksum, mapped_file ? mapped_file : "<no file>", offset);

  GameFunc fn = try_call_target(addr);
  if (fn) return fn;

  shim_log_stdout(
      "Trace: lookup_call_target: address 0x%08X (%s) not mapped (called from %s:%s:%d)\n",
      addr, mapped_file ? mapped_file : "<no file>", file, func, line);
  report_unmapped(kind ? kind : "call target", addr, file, func, line);
  return NULL;
}

/* ---- Crash bundle ------------------------------------------------------
 *
 * Every crash creates a folder under crashes/ named with a timestamp + the
 * crash kind + the involved address.  Bundle contents:
 *
 *   crash.txt        — same banner block the user sees on the console
 *   trace.tail.log   — last ~1000 trace lines from the in-memory ring
 *   state.txt        — CPU regs, lcall/isr depths + their saved sp/ss
 *                       stacks, file_mappings, top of simulated stack
 *   screenshot.png   — current video memory rendered as PNG
 *
 * The folder name is computed once per crash and cached so multiple
 * shim_log_crash banners in the same crash (rare but possible) share one
 * bundle.  Returns the relative dir path so the caller can mention it in
 * the on-screen banner.
 */
static char crash_bundle_dir_cache[256];

static int crash_bundle_mkdir_parents(const char *dir) {
  /* Create each path component in turn so we don't depend on `mkdir -p`. */
  char buf[256];
  size_t n = strlen(dir);
  if (n >= sizeof(buf)) return -1;
  memcpy(buf, dir, n + 1);
  for (size_t i = 1; i <= n; ++i) {
    if (buf[i] == '/' || buf[i] == '\0') {
      char saved = buf[i];
      buf[i] = '\0';
      if (mkdir(buf, 0755) && errno != EEXIST) return -1;
      buf[i] = saved;
    }
  }
  return 0;
}

static const char *crash_bundle_create_dir(const char *kind, uint32_t addr) {
  if (crash_bundle_dir_cache[0]) return crash_bundle_dir_cache;
  time_t now = time(NULL);
  struct tm tm_buf;
  localtime_r(&now, &tm_buf);
  /* Sanitize kind into a filesystem-friendly token (replace spaces). */
  char kind_token[32];
  int kt = 0;
  for (const char *p = kind ? kind : "crash"; *p && kt < (int)sizeof(kind_token) - 1; ++p) {
    kind_token[kt++] = (*p == ' ' || *p == '/') ? '_' : *p;
  }
  kind_token[kt] = '\0';
  snprintf(crash_bundle_dir_cache, sizeof(crash_bundle_dir_cache),
           "crashes/crash_%04d%02d%02d_%02d%02d%02d_%s_0x%08X",
           tm_buf.tm_year + 1900, tm_buf.tm_mon + 1, tm_buf.tm_mday,
           tm_buf.tm_hour, tm_buf.tm_min, tm_buf.tm_sec, kind_token, addr);
  if (crash_bundle_mkdir_parents(crash_bundle_dir_cache) != 0) {
    crash_bundle_dir_cache[0] = '\0';
    return NULL;
  }
  return crash_bundle_dir_cache;
}

static void crash_bundle_write_file(const char *dir, const char *name,
                                    const char *contents, size_t len) {
  char path[320];
  snprintf(path, sizeof(path), "%s/%s", dir, name);
  int fd = open(path, O_WRONLY | O_CREAT | O_TRUNC | O_CLOEXEC, 0644);
  if (fd < 0) return;
  size_t off = 0;
  while (off < len) {
    ssize_t w = write(fd, contents + off, len - off);
    if (w < 0) {
      if (errno == EINTR) continue;
      break;
    }
    off += (size_t)w;
  }
  close(fd);
}

static void crash_bundle_write_trace_tail(const char *dir) {
  char path[320];
  snprintf(path, sizeof(path), "%s/trace.tail.log", dir);
  int fd = open(path, O_WRONLY | O_CREAT | O_TRUNC | O_CLOEXEC, 0644);
  if (fd < 0) return;
  trace_ring_dump(fd);
  close(fd);
}

static void crash_bundle_write_state(const char *dir) {
  /* State snapshot: anything that helps post-mortem analysis but isn't
   * already in crash.txt.  Keep it grep-friendly. */
  char buf[8192];
  int n = 0;
  n += snprintf(buf + n, sizeof(buf) - n,
                "cpu: cs:ip=%04X:%04X ss:sp=%04X:%04X ds=%04X es=%04X\n"
                "     ax=%04X bx=%04X cx=%04X dx=%04X si=%04X di=%04X bp=%04X\n"
                "     flags: CF=%u PF=%u ZF=%u SF=%u OF=%u IF=%u DF=%u\n",
                cs, ip, ss, sp, ds, es, ax, bx, cx, dx, si, di, bp,
                (unsigned)CF, (unsigned)PF, (unsigned)ZF, (unsigned)SF,
                (unsigned)OF, (unsigned)IF, (unsigned)DF);
  n += snprintf(buf + n, sizeof(buf) - n,
                "lcall_depth=%u\n", (unsigned)lcall_depth);
  for (uint16_t d = 1; d <= lcall_depth && n < (int)sizeof(buf); ++d) {
    n += snprintf(buf + n, sizeof(buf) - n,
                  "  [%u] expected_ss:sp=%04X:%04X\n",
                  d, lcall_expected_ss[d], lcall_expected_sp[d]);
  }
  n += snprintf(buf + n, sizeof(buf) - n,
                "isr_depth=%u\n", (unsigned)isr_depth);
  for (uint16_t d = 1; d <= isr_depth && n < (int)sizeof(buf); ++d) {
    n += snprintf(buf + n, sizeof(buf) - n,
                  "  [%u] expected_sp=%04X\n", d, isr_expected_sp[d]);
  }
  n += snprintf(buf + n, sizeof(buf) - n, "simulated_stack_top (ss=%04X):\n", ss);
  for (int i = 0; i < 16 && n < (int)sizeof(buf); ++i) {
    uint16_t off = (uint16_t)(sp + i * 2);
    uint16_t w = memw(ss, off);
    n += snprintf(buf + n, sizeof(buf) - n, "  ss:%04X = %04X\n", off, w);
  }
  n += snprintf(buf + n, sizeof(buf) - n,
                "file_mappings (%zu):\n", file_mapping_count);
  for (size_t i = 0; i < file_mapping_count && n < (int)sizeof(buf); ++i) {
    n += snprintf(buf + n, sizeof(buf) - n,
                  "  [%3zu] 0x%05X-0x%05X (len 0x%05X, file_off 0x%X) %s\n",
                  i, file_mappings[i].base,
                  file_mappings[i].base + (uint32_t)file_mappings[i].len,
                  (uint32_t)file_mappings[i].len,
                  (uint32_t)file_mappings[i].file_offset,
                  file_mappings[i].path);
  }
  if (n < 0) n = 0;
  if (n > (int)sizeof(buf)) n = (int)sizeof(buf);
  crash_bundle_write_file(dir, "state.txt", buf, (size_t)n);
}

static void crash_bundle_write_screenshot(const char *dir) {
  char path[320];
  snprintf(path, sizeof(path), "%s/screenshot.png", dir);
  shim_render_screenshot_png(path);
}

/* Machine-readable dump of the live overlay layout (file_mappings) plus the
 * cs:ip of the failure. Lets post-mortem tools reason about which
 * chunk was loaded at the failing linear address — important for multi-chunk
 * binaries (overlay archives) where the same linear address can
 * correspond to different functions depending on what's loaded. state.txt
 * has the same info in text form; this file is the structured version. */
static void crash_bundle_write_mappings_json(const char *dir) {
  char path[320];
  snprintf(path, sizeof(path), "%s/file_mappings.json", dir);
  int fd = open(path, O_WRONLY | O_CREAT | O_TRUNC | O_CLOEXEC, 0644);
  if (fd < 0) return;
  char line[768];
  int n = snprintf(line, sizeof(line),
                   "{\n"
                   "  \"cpu\": {\"cs\": \"0x%04X\", \"ip\": \"0x%04X\"},\n"
                   "  \"file_mappings\": [\n",
                   cs, ip);
  if (n > 0) { ssize_t _w = write(fd, line, (size_t)n); (void)_w; }
  for (size_t i = 0; i < file_mapping_count; ++i) {
    n = snprintf(line, sizeof(line),
                 "%s    {\"index\": %zu, \"base\": \"0x%05X\", "
                 "\"len\": \"0x%zX\", \"file_offset\": \"0x%zX\", "
                 "\"canonical_cs\": \"0x%04X\", "
                 "\"loader_cs\": \"0x%04X\", \"loader_ip\": \"0x%04X\", "
                 "\"loader_ss\": \"0x%04X\", \"loader_sp\": \"0x%04X\", "
                 "\"loader_stack\": [\"0x%04X\",\"0x%04X\",\"0x%04X\",\"0x%04X\","
                 "\"0x%04X\",\"0x%04X\",\"0x%04X\",\"0x%04X\"], "
                 "\"path\": \"%s\"}",
                 i ? ",\n" : "",
                 i,
                 file_mappings[i].base,
                 file_mappings[i].len,
                 file_mappings[i].file_offset,
                 (unsigned)file_mappings[i].canonical_cs,
                 (unsigned)file_mappings[i].loader_cs,
                 (unsigned)file_mappings[i].loader_ip,
                 (unsigned)file_mappings[i].loader_ss,
                 (unsigned)file_mappings[i].loader_sp,
                 (unsigned)file_mappings[i].loader_stack[0],
                 (unsigned)file_mappings[i].loader_stack[1],
                 (unsigned)file_mappings[i].loader_stack[2],
                 (unsigned)file_mappings[i].loader_stack[3],
                 (unsigned)file_mappings[i].loader_stack[4],
                 (unsigned)file_mappings[i].loader_stack[5],
                 (unsigned)file_mappings[i].loader_stack[6],
                 (unsigned)file_mappings[i].loader_stack[7],
                 file_mappings[i].path ? file_mappings[i].path : "");
    if (n > 0) { ssize_t _w = write(fd, line, (size_t)n); (void)_w; }
  }
  static const char tail[] = "\n  ]\n}\n";
  ssize_t _w = write(fd, tail, sizeof(tail) - 1); (void)_w;
  close(fd);
}

/* Snapshot/keylog writers live in scripts/snapshot.c (the SDL-tier layer).
 * They register themselves via shim_set_bundle_extra_writer() at startup
 * and are invoked from save_crash_bundle below. */
static void (*bundle_extra_writer)(const char *dir);
void shim_set_bundle_extra_writer(void (*fn)(const char *dir)) {
  bundle_extra_writer = fn;
}

/* Write a complete crash bundle.  ``crash_text`` is the same banner block
 * the user sees on the console — included as crash.txt so the bundle is
 * self-contained. */
/* Build/runtime version, stamped into every crash bundle so a submitted
 * report maps to an exact code revision. Overridden at compile time with
 * -DRUNTIME_VERSION=\"<git sha>\" (see the source). */
#ifndef RUNTIME_VERSION
#define RUNTIME_VERSION "unknown"
#endif

/* Machine-readable bundle header for triage / user-submitted reports. Keep it
 * small, stable, and JSON so the source (and future tooling) can classify
 * a bundle without parsing the human-readable crash.txt. */
static void crash_bundle_write_manifest(const char *dir, const char *kind,
                                        uint32_t addr) {
  const char *ab = shim_active_binary();
  char buf[1024];
  int n = snprintf(buf, sizeof(buf),
      "{\n"
      "  \"schema\": 1,\n"
      "  \"kind\": \"%s\",\n"
      "  \"fault_addr\": \"0x%05X\",\n"
      "  \"runtime_version\": \"%s\",\n"
      "  \"active_binary\": \"%s\",\n"
      "  \"cpu\": {\"cs\":\"0x%04X\",\"ip\":\"0x%04X\",\"ss\":\"0x%04X\","
      "\"sp\":\"0x%04X\",\"ds\":\"0x%04X\",\"es\":\"0x%04X\"},\n"
      "  \"regs\": {\"ax\":\"0x%04X\",\"bx\":\"0x%04X\",\"cx\":\"0x%04X\","
      "\"dx\":\"0x%04X\",\"si\":\"0x%04X\",\"di\":\"0x%04X\",\"bp\":\"0x%04X\"},\n"
      "  \"depths\": {\"lcall\":%u,\"isr\":%u,\"dispatch\":%u,\"critical\":%u}\n"
      "}\n",
      kind, addr, RUNTIME_VERSION, ab ? ab : "<none>",
      cs, ip, ss, sp, ds, es, ax, bx, cx, dx, si, di, bp,
      (unsigned)lcall_depth, (unsigned)isr_depth,
      (unsigned)dispatch_depth, (unsigned)critical_depth);
  if (n < 0) n = 0;
  if (n > (int)sizeof(buf)) n = (int)sizeof(buf);
  crash_bundle_write_file(dir, "manifest.json", buf, (size_t)n);
}

static const char *save_crash_bundle(const char *kind, uint32_t addr,
                                     const char *crash_text, size_t crash_len) {
  const char *dir = crash_bundle_create_dir(kind, addr);
  if (!dir) return NULL;
  crash_bundle_write_manifest(dir, kind, addr);
  crash_bundle_write_file(dir, "crash.txt", crash_text, crash_len);
  crash_bundle_write_trace_tail(dir);
  crash_bundle_write_state(dir);
  crash_bundle_write_mappings_json(dir);
  lifecycle_dump_to_dir(dir);
  stack_writes_dump_to_dir(dir);
  crash_bundle_write_screenshot(dir);
  if (bundle_extra_writer) bundle_extra_writer(dir);
  session_log_write_to_bundle(dir);
  return dir;
}

/* Public wrappers around the static crash_bundle_write_* helpers so the
 * snapshot module can write into a bundle directory using the same path
 * conventions. */
void shim_crash_bundle_write_file(const char *dir, const char *name,
                                  const char *contents, size_t len) {
  crash_bundle_write_file(dir, name, contents, len);
}
void shim_crash_bundle_write_state(const char *dir) {
  crash_bundle_write_state(dir);
}
void shim_crash_bundle_write_trace_tail(const char *dir) {
  crash_bundle_write_trace_tail(dir);
}
void shim_lifecycle_dump_to_dir(const char *dir) {
  lifecycle_dump_to_dir(dir);
}

/* ===== Snapshot module access to keyboard queue + file_mappings =====
 *
 * These wrap the static globals so the snapshot module can capture/restore
 * without depending on shims internals. */

void shim_kbd_state_capture(ShimKbdState *out) {
  for (int i = 0; i < SHIM_KBD_BUFFER_SIZE; ++i) {
    out->q_ascii[i] = kbd.queue[i].ascii;
    out->q_scan[i]  = kbd.queue[i].scancode;
  }
  out->head = kbd.queue_head;
  out->tail = kbd.queue_tail;
  out->count = kbd.queue_count;
  out->cur_ascii = kbd.ascii;
  out->cur_scan  = kbd.scancode;
  out->last_scan = kbd.last_scancode;
  out->ready     = (uint8_t)kbd.scancode_ready;
}

void shim_kbd_state_restore(const ShimKbdState *in) {
  for (int i = 0; i < SHIM_KBD_BUFFER_SIZE; ++i) {
    kbd.queue[i].ascii = in->q_ascii[i];
    kbd.queue[i].scancode = in->q_scan[i];
  }
  kbd.queue_head = in->head;
  kbd.queue_tail = in->tail;
  kbd.queue_count = in->count;
  kbd.ascii = in->cur_ascii;
  kbd.scancode = in->cur_scan;
  kbd.last_scancode = in->last_scan;
  kbd.scancode_ready = in->ready;
}

size_t shim_file_mappings_count(void) { return file_mapping_count; }

void shim_file_mappings_get(size_t i, ShimFileMappingView *out) {
  if (i >= file_mapping_count) {
    memset(out, 0, sizeof(*out));
    return;
  }
  out->base = file_mappings[i].base;
  out->len  = file_mappings[i].len;
  out->file_offset = file_mappings[i].file_offset;
  out->canonical_cs = file_mappings[i].canonical_cs;
  out->path = file_mappings[i].path;
}

void shim_file_mappings_reset(void) {
  /* Keep allocated path/data so we don't double-free; just clear the count.
   * Restore appends to the freshly cleared slot range. Acceptable for a
   * one-shot restore path. */
  file_mapping_count = 0;
}

int shim_file_mappings_add_for_restore(const char *path, uint32_t base,
                                       size_t len, size_t file_offset,
                                       uint16_t canonical_cs) {
  /* Apply the same flat-schema invariant as register_file_mapping: any
   * older entry overlapping the restored range gets shrunk/split/evicted.
   * Snapshots taken before evict_or_shrink_for_load existed may contain
   * historical overlaps that we collapse here. */
  evict_or_shrink_for_load(base, len);
  if (file_mapping_count >= MAX_FILE_MAPPINGS) return -1;
  file_mappings[file_mapping_count].path = strdup(path);
  file_mappings[file_mapping_count].base = base;
  file_mappings[file_mapping_count].len = len;
  file_mappings[file_mapping_count].file_offset = file_offset;
  file_mappings[file_mapping_count].data = NULL;
  file_mappings[file_mapping_count].canonical_cs = canonical_cs;
  ++file_mapping_count;
  return 0;
}

/* Convenience for [BUG] abort sites: format a short message, dump it as
 * crash.txt, and also log to the on-screen / file channel so the user
 * sees both the BUG message and the bundle path before the abort. */
static void save_bug_bundle(const char *kind, uint32_t addr,
                            const char *msg) {
  const char *dir = save_crash_bundle(kind, addr, msg, strlen(msg));
  if (dir) {
    shim_log_crash("Bundle: %s\n", dir);
  }
}

/* Public entry point for translator-emitted [BUG] aborts (the dispatcher
 * default case for an unhandled pc).  Forwards to save_bug_bundle. */
void shim_save_bug_bundle(const char *kind, uint32_t addr, const char *msg) {
  save_bug_bundle(kind, addr, msg);
}

/* ---- Test seam: mockable fatal (unmapped dispatch) path ------------------
 * In the real product an unmapped call/jmp target is a hard failure:
 * report_unmapped writes a crash bundle and exit(1)s. Tests need to assert
 * that behaviour WITHOUT the process dying (and without spraying crash
 * bundles into the tree). When a test arms this seam, report_unmapped
 * instead records the (kind, addr) and longjmps back to the setjmp guard in
 * the entry wrapper that armed it. Disarmed -- the default, i.e. every
 * non-test run -- behaviour is exactly as before (real exit(1)). */
static volatile int shim_fatal_armed = 0;
static jmp_buf shim_fatal_env;
volatile int shim_fatal_captured = 0;
uint32_t shim_fatal_addr = 0;
char shim_fatal_kind[32] = {0};

void shim_arm_fatal_capture(void) {
  shim_fatal_armed = 1;
  shim_fatal_captured = 0;
  shim_fatal_addr = 0;
  shim_fatal_kind[0] = '\0';
}

void shim_disarm_fatal_capture(void) { shim_fatal_armed = 0; }

static void report_unmapped(const char *kind, uint32_t addr,
                            const char *caller_file, const char *caller_func,
                            int line) {
  if (shim_fatal_armed) {
    /* Test mode: capture and unwind to the armed entry wrapper instead of
     * terminating. Short-circuit before any I/O so no crash bundle is
     * written. */
    shim_fatal_captured = 1;
    shim_fatal_addr = addr;
    snprintf(shim_fatal_kind, sizeof(shim_fatal_kind), "%s",
             kind ? kind : "");
    longjmp(shim_fatal_env, 1);
  }
  /* Build the entire crash diagnostic into one stack buffer and emit it in
   * a single shim_log_crash call.  Multiple small writes get dropped /
   * coalesced by tmux and other terminal emulators under high trace
   * throughput — even though the writes are unbuffered, the terminal-side
   * render can't keep up and the visible pane ends up missing characters.
   * The file capture is fine (we already fsync per stream), but for the
   * interactive use case we want the diagnostic to land atomically.
   *
   * The buffer is sized to hold a generous worst case (~1.5 KB).  We bail
   * to per-line emission only if snprintf overflows. */
  const FileMapping *m = find_file_mapping(addr);
  uint8_t bytes[8];
  for (int i = 0; i < 8; i++) {
    bytes[i] = memb_raw(addr >> 4, (addr & 0xF) + i);
  }
  char hex[32];
  int hp = 0;
  for (int i = 0; i < 8; i++) {
    hp += snprintf(hex + hp, sizeof(hex) - hp, "%s%02X", i ? " " : "", bytes[i]);
  }
  char ascii[16];
  for (int i = 0; i < 8; i++) {
    ascii[i] = isprint(bytes[i]) ? (char)bytes[i] : '.';
  }
  ascii[8] = '\0';

  char block[1536];
  int n;
  if (m) {
    uint32_t offset = (uint32_t)(m->file_offset + (addr - m->base));
    const char *mapped_file = m->path;
    const char *slash = strrchr(mapped_file, '/');
    if (slash) mapped_file = slash + 1;
    char stem[256];
    strncpy(stem, mapped_file, sizeof(stem));
    stem[sizeof(stem) - 1] = '\0';
    char *dot = strrchr(stem, '.');
    if (dot) *dot = '\0';
    const char *game_name = game_config.name ? game_config.name : "game";

    n = snprintf(block, sizeof(block),
        "======== CRASH ========\n"
        "CPU state at unmapped %s: cs:ip=%04X:%04X ss:sp=%04X:%04X "
        "ds=%04X es=%04X ax=%04X bx=%04X cx=%04X dx=%04X si=%04X di=%04X\n"
        "Error: %s address 0x%08X is not mapped (called from %s:%s:%d; "
        "offset 0x%X in %s)\n"
        "Bytes at 0x%08X: %s |%s|\n"
        "To fix: verify offset 0x%X in %s looks like code. If so, add 0x%X "
        "to extra_entries in resources/%s.json and add an entry to "
        "call_targets in games/%s.json\n"
        "=======================\n",
        kind, cs, ip, ss, sp, ds, es, ax, bx, cx, dx, si, di,
        kind, addr, caller_file, caller_func, line, offset, mapped_file,
        addr, hex, ascii,
        offset, mapped_file, offset, stem, game_name);
  } else {
    const char *game_name = game_config.name ? game_config.name : "game";
    n = snprintf(block, sizeof(block),
        "======== CRASH ========\n"
        "CPU state at unmapped %s: cs:ip=%04X:%04X ss:sp=%04X:%04X "
        "ds=%04X es=%04X ax=%04X bx=%04X cx=%04X dx=%04X si=%04X di=%04X\n"
        "Error: %s address 0x%08X is not mapped (called from %s:%s:%d; no "
        "file loaded). Update call_targets in games/%s.json if this should "
        "map to translated code.\n"
        "Bytes at 0x%08X: %s |%s|\n"
        "=======================\n",
        kind, cs, ip, ss, sp, ds, es, ax, bx, cx, dx, si, di,
        kind, addr, caller_file, caller_func, line, game_name,
        addr, hex, ascii);
  }

  /* Skip stdio for the crash block: glibc's vfprintf on an _IONBF stream
   * flushes per character via __overflow, which under tmux + high trace
   * throughput can lose chunks at the terminal-render layer.  A direct
   * write() of the full block in one syscall survives. */
  if (n <= 0) {
    const char *fallback = "[CRASH report could not be formatted]\n";
    n = (int)strlen(fallback);
    memcpy(block, fallback, (size_t)n + 1);
  } else if (n >= (int)sizeof(block)) {
    n = (int)sizeof(block) - 1;
  }
  /* Build a crash bundle on disk and append its path to the on-screen
   * banner so the user knows where to find screenshot / trace tail /
   * state for post-mortem. */
  const char *bundle_dir = save_crash_bundle(kind, addr, block, (size_t)n);
  if (bundle_dir && n + 64 < (int)sizeof(block)) {
    /* Insert "Bundle: <path>\n" right before the closing banner so the
     * crash.txt we already wrote stays minimal but the on-screen output
     * tells the user where to look. */
    char extra[256];
    int en = snprintf(extra, sizeof(extra), "Bundle: %s\n", bundle_dir);
    /* Splice extra before the closing "=======" line. */
    const char *closer = "=======================\n";
    char *pos = strstr(block, closer);
    if (pos && en > 0 && (size_t)(pos - block) + en + strlen(closer) + 1
        < sizeof(block)) {
      memmove(pos + en, pos, strlen(closer) + 1);
      memcpy(pos, extra, en);
      n += en;
    }
  }
  shim_flush_all_streams(); /* drain prior traces before our atomic block */
  /* Pick the single channel where the user is actually watching output:
   *
   * - If stdout is a terminal (interactive run, no redirection), write
   *   directly to /dev/tty.  Writes to fd 1 in this case get dropped by
   *   the terminal renderer when the pty is saturated with millions of
   *   buffered trace bytes; /dev/tty bypasses that.
   * - If stdout is redirected (`> debug.txt`, `| tee`, etc.), write to
   *   fd 1 so the block lands in the capture file.
   *
   * Exactly one copy lands in the user's chosen channel. */
  int target_fd = -1;
  int tty_fd = -1;
  int out_fd = fileno(stdout);
  if (out_fd >= 0 && isatty(out_fd)) {
    tty_fd = open("/dev/tty", O_WRONLY | O_CLOEXEC);
    target_fd = tty_fd >= 0 ? tty_fd : out_fd;
  } else {
    target_fd = out_fd;
  }
  if (target_fd >= 0) {
    int off = 0;
    while (off < n) {
      ssize_t w = write(target_fd, block + off, (size_t)(n - off));
      if (w < 0) {
        if (errno == EINTR) continue;
        break;
      }
      off += w;
    }
    fsync(target_fd);
  }
  if (tty_fd >= 0) close(tty_fd);
  /* Unmapped target is a failure, not a successful run.  Exit non-zero so
   * the harness / shell reports it as one. */
  exit(1);
}

// Shim for ljmp seg:off
//
// A far jump is not a call: nothing new is pushed onto the stack.  The
// caller's retIP (if any) is whatever already sits on top of the simulated
// stack; the dispatched routine will own that pop on its own ``ret``.
/* Faithful far jmp `jmp s:o`: set cpu.r_cs:cpu.r_ip and return to the
 * top-level loop, which re-resolves the owning chunk for the new segment.
 * No nested dispatch, no windowed-recovery -- the pushed/popped values are
 * segment-relative offsets so cs<<4+ip reconstructs the right linear address
 * under whatever cs is live. */
void long_jump_impl(uint16_t seg, uint16_t off, const char *file,
                    const char *func, int line) {
  uint32_t addr = ((uint32_t)seg << 4) + off;
  shim_log_stdout("Trace: long_jump to %04X:%04X (0x%08X) (%s:%s:%d)\n", seg,
                  off, addr, file, func, line);
  lifecycle_log_dispatch("LJMP", addr);
  cpu.r_cs = seg;
  cpu.r_ip = off;
  record_binary_cs(addr, seg);
}

/* Faithful `retf` / `retf imm16`: pop ip then cs (then imm16 argument bytes),
 * set cpu.r_cs:cpu.r_ip, and return to the top-level loop. The emulated stack
 * is the only return-address store, so there is no lcall frame to match and no
 * longjmp. Works identically for a genuine far-call return, the
 * `push cs; call near; retf` idiom, and `push far ptr; retf` used as a jump --
 * all three just pop two words and continue at the popped cs:ip. */
static void retf_common_impl(const char *file, const char *func, int line,
                             uint16_t pop_bytes) {
  uint16_t frame_ss = ss;
  uint16_t sp_before = sp;
  uint16_t new_ip = memw(frame_ss, sp_before);
  uint16_t seg = memw(frame_ss, (sp_before + 2) & 0xFFFF);
  sp = (sp_before + 4 + pop_bytes) & 0xFFFF;
  shim_log_stdout("Trace: retf -> %04X:%04X sp=%04X pop=%u (%s:%s:%d)\n", seg,
                  new_ip, sp_before, pop_bytes, file, func, line);
  cpu.r_cs = seg;
  cpu.r_ip = new_ip;
}

void retf_impl(const char *file, const char *func, int line) {
  retf_common_impl(file, func, line, 0);
}

void retf_pop_impl(const char *file, const char *func, int line,
                   uint16_t pop_bytes) {
  retf_common_impl(file, func, line, pop_bytes);
}

void retf(void) { retf_impl("<external>", __func__, 0); }

/* Faithful `iret`: pop ip, cs, flags; restore flags; set cpu.r_cs:cpu.r_ip;
 * return to the top-level loop (or, when servicing an injected hardware IRQ,
 * to the nested run-loop in invoke_isr, which detects completion by sp). No
 * setjmp/longjmp, no drift assert -- the stack is the source of truth. */
void iret_impl(const char *file, const char *func, int line) {
  uint16_t sp_before = sp;
  uint8_t old_if = IF;
  uint16_t new_ip = memw(ss, sp_before);
  uint16_t seg = memw(ss, (sp_before + 2) & 0xFFFF);
  uint16_t flags = memw(ss, (sp_before + 4) & 0xFFFF);
  sp = (sp_before + 6) & 0xFFFF;
  shim_log_stdout(
      "Trace: iret -> %04X:%04X flags=0x%04X depth=%d sp=%04X (%s:%s:%d)\n",
      seg, new_ip, flags, isr_depth, sp_before, file, func, line);

  CF = flags & 1;
  PF = (flags >> 2) & 1;
  ZF = (flags >> 6) & 1;
  SF = (flags >> 7) & 1;
  IF = (flags >> 9) & 1;
  DF = (flags >> 10) & 1;
  OF = (flags >> 11) & 1;
  if (!old_if && IF) {
    interrupt_shadow = 1;
  }
  cpu.r_cs = seg;
  cpu.r_ip = new_ip;
}

void iret(void) { iret_impl("<external>", __func__, 0); }

/* Faithful far call `call s:o`: push cs then ret_ip on the emulated stack,
 * set cpu.r_cs:cpu.r_ip = s:o, and return to the top-level loop. The matching
 * retf pops the two words. No setjmp/longjmp, no lcall depth/drift bookkeeping
 * -- the stack is the only return-address store. */
void lcall_table_impl(uint16_t ret_ip, uint16_t seg, uint16_t off,
                      const char *file, const char *func, int line) {
  uint32_t addr = ((uint32_t)seg << 4) + off;
  shim_log_stdout("Trace: lcall_table to %04X:%04X (0x%08X) (%s:%s:%d)\n", seg,
                  off, addr, file, func, line);
  lifecycle_log_dispatch("LCALL", addr);

  uint16_t sp_before = sp;
  sp = (sp - 2) & 0xFFFF;
  memw_write(ss, sp, cs);
  sp = (sp - 2) & 0xFFFF;
  memw_write(ss, sp, ret_ip);
  shim_log_stdout(
      "Trace: lcall push ret_ip=%04X saved_cs=%04X sp=%04X -> %04X\n", ret_ip,
      cs, sp_before, sp);
  cpu.r_cs = seg;
  cpu.r_ip = off;
  record_binary_cs(addr, seg);
}

/* Faithful near indirect call `call [mem]` / `call reg`: the target is a
 * 16-bit offset within the CURRENT code segment (the caller computed `addr =
 * cs<<4 + target_off`). Push the near return IP, set cpu.r_ip to the target
 * offset, and return to the top-level loop. The matching near `ret` pops the
 * pushed IP. cs is unchanged (near call). */
void call_table_impl(uint16_t ret_ip, uint32_t addr, const char *file,
                     const char *func, int line) {
  shim_log_stdout("Trace: call_table 0x%08X (%s:%s:%d)\n", addr, file, func,
                  line);
  lifecycle_log_dispatch("CALL", addr);
  sp = (sp - 2) & 0xFFFF;
  memw_write(ss, sp, ret_ip);
  cpu.r_ip = (uint16_t)(addr - ((uint32_t)cs << 4));
}

/* Look up the BinaryDispatch entry for the binary whose translated C source
 * is ``file`` (e.g. ``/.../overlay.c`` → matches ``module="overlay"``).
 *
 * Semantic near ret / jump_table from translated code can use this to route
 * directly into the caller's own dispatch without needing the cs-derived
 * linear address to resolve cleanly. The caller's binary is statically
 * known at compile time (it's the file emitting the call), so dispatching
 * by caller binary is correct regardless of cs corruption. */
static const BinaryDispatch *find_dispatch_by_source_file(const char *file) {
  if (!file || !game_config.binary_dispatch) return NULL;
  /* Extract basename without ``.c`` extension. */
  const char *slash = strrchr(file, '/');
  const char *base = slash ? slash + 1 : file;
  size_t n = strlen(base);
  if (n > 2 && base[n - 2] == '.' && base[n - 1] == 'c') n -= 2;
  for (size_t i = 0; i < game_config.binary_dispatch_count; ++i) {
    const BinaryDispatch *bd = &game_config.binary_dispatch[i];
    if (bd->module && bd->fn && strlen(bd->module) == n &&
        strncmp(bd->module, base, n) == 0) {
      return bd;
    }
  }
  return NULL;
}

/* Cross-binary trampoline state. Set by `near_ret_tail_impl` (and other
 * cross-binary tail dispatchers) instead of recursively calling the target
 * binary's dispatch function. The enclosing `dispatch_via_binary` loop
 * picks this up after its bd->fn returns and dispatches the new
 * (addr, expected_retip) IN THE SAME C FRAME — no stack growth across
 * cross-binary tail transitions. This is what eliminates the music
 * sequencer's `overlay ↔ module ↔ overlay via 0x7353` recursion. */
bool     tail_dispatch_pending;
uint32_t tail_dispatch_addr;
uint16_t tail_dispatch_expected;

static const BinaryDispatch *find_binary_for_addr(uint32_t addr,
                                                  const FileMapping **out_fm) {
  if (!game_config.binary_dispatch) return NULL;
  const FileMapping *fm = find_file_mapping(addr);
  if (!fm || !fm->path) return NULL;
  const char *bn = strrchr(fm->path, '/');
  bn = bn ? bn + 1 : fm->path;
  for (size_t i = 0; i < game_config.binary_dispatch_count; ++i) {
    const BinaryDispatch *bd = &game_config.binary_dispatch[i];
    if (bd->file && bd->fn && strcmp(bd->file, bn) == 0) {
      if (out_fm) *out_fm = fm;
      return bd;
    }
  }
  return NULL;
}

/* Per-binary dispatch routing.  Resolves a linear address into
 * (binary, file_offset) via file_mappings + BinaryDispatch and invokes
 * ``<binary>_dispatch(file_offset, ...)``.  Returns 1 if a dispatch was
 * invoked, 0 if no matching binary dispatch was found (caller should fall
 * back to per-symbol lookup_call_target).
 *
 * Sets cpu.r_cs around the call to the binary's recorded canonical cs (if
 * known from an earlier lcall/long_jump). Without this the target's
 * translated code would use whatever cs happened to be left over from
 * previous execution and references like ``cs:[disp]`` would read from
 * the wrong segment.
 *
 * Trampoline loop: after bd->fn returns, if `tail_dispatch_pending` is set
 * (a near_ret_tail or similar inside fn requested a cross-binary tail
 * dispatch), re-enter with the new (addr, expected_retip) in the SAME C
 * frame instead of recursing. Eliminates the dispatch_via_binary stack
 * growth from cross-binary tail chains. */
/* ===================== JIT recompiler =====================
 * This is the whole pipeline: nothing is decoded ahead of time. When the
 * program transfers control to code with no compiled chunk -- the entry itself,
 * or code decompressed by a runtime unpacker, loaded from an overlay, or self-
 * modified -- the JIT decodes the REAL in-memory bytes, compiles them to a .so,
 * dlopen()s it, and dispatches into it, all without restarting. Registered
 * chunks take precedence in dispatch_via_binary for their decoded ranges. It
 * requires the launcher to export SAISEI_REPO_ROOT + SAISEI_JITC (the `saisei`
 * launcher does). The static dispatch-by-binary path (find_binary_for_addr /
 * GameConfig.binary_dispatch) is retained NULL-but-shaped for a future "freeze
 * the chunks into a static native build"; today it is empty and every address
 * routes through here. */
/* Chunks are decoded PER SEGMENT (base = cs<<4) and keyed on IP, matching the
 * x86 segment model: the code's ret_ip / near-ret / far-jump offsets are IP
 * values, so the chunk must be keyed on IP (not linear) or those offsets come
 * out wrong by (cs<<4)&0xFFFF. seg_base is the segment's linear base; [lo,hi)
 * is the decoded IP range within it. */
typedef struct {
  uint32_t seg_base; /* cs << 4 */
  uint32_t lo, hi;   /* decoded IP range within the segment */
  uint32_t *keys;    /* sorted IP case-keys the chunk actually decoded */
  size_t nkeys;
  /* Exact decoded-instruction byte coverage: ncode merged [start,end) IP
   * intervals stored flat (code[2*i]=start, code[2*i+1]=end), sorted &
   * non-overlapping. A write invalidates this chunk iff it overlaps one of these
   * intervals -- the faithful x86 self-modifying-code rule (any instruction byte,
   * not just a data byte in a [lo,hi] gap, and not a window around a case-key). */
  uint32_t *code;
  size_t ncode;
  DispatchFn fn;
  void *handle;
  int stale;         /* 1 = code bytes overwritten by an overlay load; skip in
                        lookup so the next dispatch re-decodes from live memory */
} JitChunk;
#define MAX_JIT_CHUNKS 1024
static JitChunk jit_chunks[MAX_JIT_CHUNKS];
static size_t jit_chunk_count;
/* Union of all live chunks' decoded linear ranges -- a cheap reject so the
 * per-write invalidation hook is free for the (overwhelming majority of) writes
 * that land outside any decoded code region (VGA, stack, data segments). */
static uint32_t jit_code_lo = 0xFFFFFFFFu, jit_code_hi = 0;

/* True if IP `off` is a real dispatch case in the chunk. Without this a chunk
 * whose [lo,hi] range spans gaps (mid-instruction bytes, or addresses it never
 * decoded) would shadow real decoded code at those gaps -- dispatching there
 * hits the chunk's default case, which near-rets and corrupts the stack. No key
 * data loaded -> fall back to range-only (1). */
static int jit_chunk_has_key(const JitChunk *c, uint32_t off) {
  if (!c->keys || c->nkeys == 0) return 1;
  size_t lo = 0, hi = c->nkeys;
  while (lo < hi) {
    size_t mid = lo + (hi - lo) / 2;
    if (c->keys[mid] < off) lo = mid + 1;
    else hi = mid;
  }
  return lo < c->nkeys && c->keys[lo] == off;
}

/* Load the chunk's case-keys sidecar (<so without .so>.keys): <uint32 count>
 * then count*uint32 sorted keys (same format as the source case_keys), and
 * the exact code-byte-coverage sidecar (.code): <uint32 count> then
 * count*(uint32 start, uint32 end) merged, sorted intervals. */
static void jit_load_keys(JitChunk *c, const char *so_path) {
  c->keys = NULL;
  c->nkeys = 0;
  c->code = NULL;
  c->ncode = 0;
  size_t n = strlen(so_path);
  if (n < 3 || n + 6 >= 1100) return;
  char kp[1104];
  memcpy(kp, so_path, n - 3); /* strip ".so" */
  memcpy(kp + (n - 3), ".keys", 6);
  FILE *f = fopen(kp, "rb");
  if (f) {
    uint32_t cnt = 0;
    if (fread(&cnt, sizeof(cnt), 1, f) == 1 && cnt > 0 && cnt < (1u << 24)) {
      uint32_t *arr = (uint32_t *)malloc((size_t)cnt * sizeof(uint32_t));
      if (arr && fread(arr, sizeof(uint32_t), cnt, f) == cnt) {
        c->keys = arr;
        c->nkeys = cnt;
      } else {
        free(arr);
      }
    }
    fclose(f);
  }
  memcpy(kp + (n - 3), ".code", 6);
  f = fopen(kp, "rb");
  if (f) {
    uint32_t cnt = 0;
    if (fread(&cnt, sizeof(cnt), 1, f) == 1 && cnt > 0 && cnt < (1u << 24)) {
      uint32_t *arr = (uint32_t *)malloc((size_t)cnt * 2 * sizeof(uint32_t));
      if (arr && fread(arr, sizeof(uint32_t), (size_t)cnt * 2, f) == (size_t)cnt * 2) {
        c->code = arr;
        c->ncode = cnt;
      } else {
        free(arr);
      }
    }
    fclose(f);
  }
}

/* True iff the write range [k_lo,k_hi) (chunk-relative IP offsets) overlaps any
 * decoded-instruction byte interval. Intervals are sorted & non-overlapping, so
 * a binary search for the first interval whose end > k_lo and then a single
 * start < k_hi test is exact. */
static int jit_range_hits_code(const JitChunk *c, uint32_t k_lo, uint32_t k_hi) {
  size_t lo = 0, hi = c->ncode;
  while (lo < hi) {
    size_t mid = lo + (hi - lo) / 2;
    if (c->code[2 * mid + 1] <= k_lo) lo = mid + 1; /* interval ends at/before write */
    else hi = mid;
  }
  return lo < c->ncode && c->code[2 * lo] < k_hi;   /* its start is before write end */
}

static JitChunk *jit_lookup(uint32_t linear) {
  for (ssize_t i = (ssize_t)jit_chunk_count - 1; i >= 0; --i) {
    JitChunk *c = &jit_chunks[i];
    if (c->stale) continue;
    if (linear >= c->seg_base + c->lo && linear < c->seg_base + c->hi &&
        jit_chunk_has_key(c, linear - c->seg_base))
      return c;
  }
  return NULL;
}

/* True iff seg:off is a dispatch case-key in a live JIT chunk decoded at THIS
 * segment base (cs<<4) -- i.e. a position the game can be running at inside a
 * JIT chunk and that a restore can re-enter via dispatch_via_binary. Lets the
 * save_manager treat a JIT-chunk resting point as savable, the same way a
 * static dispatch-by-binary case-key would be. */
int shim_pc_is_jit_case_key(uint16_t seg, uint16_t off) {
  uint32_t seg_base = (uint32_t)seg << 4;
  for (size_t i = 0; i < jit_chunk_count; ++i) {
    JitChunk *c = &jit_chunks[i];
    if (c->stale || c->seg_base != seg_base) continue;
    if (off >= c->lo && off < c->hi && jit_chunk_has_key(c, off)) return 1;
  }
  return 0;
}

/* Mark stale any JIT chunk a write to [lin, lin+len) just invalidated, by the
 * faithful x86 self-modifying-code rule: a chunk is stale iff the write
 * overwrote a byte the chunk decoded as part of an INSTRUCTION. jit_lookup skips
 * stale chunks, so the next control transfer there re-decodes from live memory;
 * the still-mapped .so keeps running the active frame safely until then (marking
 * stale never unloads under the frame), so there is no need to special-case a
 * write that hits the writer's own currently-executing code.
 *
 * The exact per-instruction extents (.code intervals) make this precise and
 * generic: a write to a data/buffer byte that merely sits inside the chunk's
 * [lo,hi] span -- a gap between/after instructions, e.g. the intro's overlay
 * graphics frames, or a resident routine's nearby counter -- does NOT invalidate
 * (no churn-recompile), while a patch to ANY instruction byte (opcode OR
 * operand/immediate/displacement) DOES (no stale execution). No byte window, no
 * skip-self heuristic. No-op when no chunks exist. */
static void jit_invalidate_range_impl(uint32_t lin, uint32_t len) {
  if (jit_chunk_count == 0 || len == 0) return;
  /* Cheap reject: outside the union of all decoded code ranges -> nothing to do.
   * Makes the per-write hook free for VGA/stack/data writes. */
  if (lin + len <= jit_code_lo || lin >= jit_code_hi) return;
  uint32_t w_lo = lin, w_hi = lin + len;
  for (size_t i = 0; i < jit_chunk_count; ++i) {
    JitChunk *c = &jit_chunks[i];
    if (c->stale) continue;
    uint32_t c_lo = c->seg_base + c->lo, c_hi = c->seg_base + c->hi;
    if (w_hi <= c_lo || w_lo >= c_hi) continue;          /* no [lo,hi] overlap */
    uint32_t k_lo = (w_lo > c->seg_base) ? w_lo - c->seg_base : 0;
    uint32_t k_hi = w_hi - c->seg_base;
    /* Exact code-byte overlap. A chunk that predates the .code sidecar (old
     * cache) has ncode==0 -> conservatively treat any [lo,hi] overlap as code. */
    if (c->ncode != 0 && !jit_range_hits_code(c, k_lo, k_hi)) continue;
    c->stale = 1;
    shim_log_stdout("JIT: invalidate chunk %05X:[%04X,%04X) -- write "
                    "0x%05X..0x%05X overwrote a decoded instruction\n",
                    c->seg_base, c->lo, c->hi, w_lo, w_hi);
  }
}

/* Per-write self-modification hook (memb/memw/string-store). With exact
 * per-instruction extents the invalidation is precise, so this and the _force
 * variant below are now identical -- both kept for call-site clarity. */
void shim_jit_invalidate_code_range(uint32_t lin, uint32_t len) {
  jit_invalidate_range_impl(lin, len);
}

/* Full code-swap invalidation (a replace-self overlay/EXEC that loads a new
 * program OVER the loader's own segment). Marking stale is deferred (the .so
 * stays mapped for the live frame; the next dispatch to that address re-decodes
 * the new bytes), so invalidating the loader's own running chunk is safe. */
void shim_jit_invalidate_code_range_force(uint32_t lin, uint32_t len) {
  jit_invalidate_range_impl(lin, len);
}

/* Dispatch a linear address into a JIT chunk: run it at the chunk's segment
 * (so near-ret composition uses the right cs) with pc = IP = linear-seg_base. */
static int jit_dispatch(JitChunk *c, uint32_t linear, uint16_t expected_retip,
                        const char *file, const char *func, int line) {
  cpu.r_cs = (uint16_t)(c->seg_base >> 4);
  c->fn((int)(linear - c->seg_base), expected_retip, file, func, line);
  /* The chunk's raw _dispatch does not drain a pending tail dispatch, so a
   * near-ret/tail-call OUT of the chunk would leak tail_dispatch_pending to our
   * caller and corrupt the next return. Drain it here -- the same drain
   * dispatch_via_binary's trampoline loop runs. */
  shim_drain_pending_tail_dispatch(file, func, line);
  return 1;
}

/* Decode+compile+load the per-segment chunk covering cs:ip; return the chunk
 * (or NULL if JIT is disabled / any stage fails -- caller then takes the static-dispatch
 * unhandled-pc path). Decodes the 64KB segment at cs<<4 from offset ip, keyed
 * on IP. Game time is frozen across the compile. */
static JitChunk *jit_compile_or_get(uint16_t seg, uint16_t off) {
  uint32_t seg_base = (uint32_t)seg << 4;
  JitChunk *existing = jit_lookup(seg_base + off);
  /* Only a cache hit if the existing chunk is based at THIS seg_base. The same
   * linear address is also covered by chunks decoded under other cs aliases
   * (e.g. the canonical 1010-base chunk covers 1844:06A6 = linear 0x18AE6); we
   * must NOT hand that back here, or the caller runs it under the wrong cs and
   * its pc-relative IPs land off-segment. A foreign-base hit means decode this
   * code afresh at our own seg_base. */
  if (existing && existing->seg_base == seg_base) return existing;
  const char *repo = getenv("SAISEI_REPO_ROOT");
  /* The Rust translator (SAISEI_JITC) is the only JIT backend; the runtime needs
   * no the reference at all. */
  if (!repo || !getenv("SAISEI_JITC") || jit_chunk_count >= MAX_JIT_CHUNKS)
    return NULL;
  if (seg_base + 0x10000u > MEMORY_SIZE) return NULL;

  vclock_halt();
  char dir[1024], dump[1100], cmd[4096];
  /* Capture the JIT subprocess's combined stdout+stderr so a translate FATAL
   * (ir_to_c's mnemonic / file-offset / reached-from block, written to stderr)
   * lands IN the crash bundle instead of only on the terminal. The diagnostic
   * is printed last and is ~1.5 KB, so on overflow we keep the TAIL. */
  char jitcap[8192];
  size_t jitcaplen = 0;
  jitcap[0] = '\0';
  /* Per-game chunk dir (SAISEI_JIT_DIR) so bundles sharing seg:ip but decoding
   * to different bytes don't clobber each other's .so. Falls back to the
   * shared build/jit when unset (direct binary invocation outside tools.game). */
  const char *jit_dir = getenv("SAISEI_JIT_DIR");
  if (jit_dir && jit_dir[0])
    snprintf(dir, sizeof(dir), "%s", jit_dir);
  else
    snprintf(dir, sizeof(dir), "%s/build/jit", repo);
  mkdir(dir, 0755);
  snprintf(dump, sizeof(dump), "%s/seg_%05X.bin", dir, seg_base);
  JitChunk *result = NULL;
  FILE *fp = fopen(dump, "wb");
  if (fp) {
    fwrite(virtual_memory + seg_base, 1, 0x10000u, fp);
    fclose(fp);
    /* The JIT translator is the Rust `saisei-jitc jit-compile` (SAISEI_JITC). It
     * speaks the SO/SYM/RANGE stdout protocol + .keys/.code sidecars the loader
     * below parses. */
    const char *jitc = getenv("SAISEI_JITC");
    snprintf(cmd, sizeof(cmd),
             "'%s' jit-compile --mem '%s' --entry 0x%X "
             "--name jit_%05x_%04x --image-base 0x%X --outdir '%s' 2>&1",
             jitc, dump, off, seg_base, off, seg_base, dir);
    FILE *pp = popen(cmd, "r");
    if (pp) {
      char so[1024] = "", sym[256] = "", line[1200];
      unsigned lo = 0, hi = 0;
      while (fgets(line, sizeof(line), pp)) {
        if (!strncmp(line, "SO ", 3)) sscanf(line + 3, "%1023s", so);
        else if (!strncmp(line, "SYM ", 4)) sscanf(line + 4, "%255s", sym);
        else if (!strncmp(line, "RANGE ", 6))
          sscanf(line + 6, "0x%x 0x%x", &lo, &hi);
        /* Tail-keep the combined output for the bundle (see jitcap decl). */
        size_t ll = strlen(line);
        if (ll >= sizeof(jitcap)) {
          memcpy(jitcap, line + ll - (sizeof(jitcap) - 1), sizeof(jitcap) - 1);
          jitcaplen = sizeof(jitcap) - 1;
        } else {
          if (jitcaplen + ll > sizeof(jitcap) - 1) {
            size_t drop = jitcaplen + ll - (sizeof(jitcap) - 1);
            memmove(jitcap, jitcap + drop, jitcaplen - drop);
            jitcaplen -= drop;
          }
          memcpy(jitcap + jitcaplen, line, ll);
          jitcaplen += ll;
        }
        jitcap[jitcaplen] = '\0';
      }
      int rc = pclose(pp);
      if (rc == 0 && so[0] && sym[0] && hi > lo) {
        void *h = dlopen(so, RTLD_NOW | RTLD_GLOBAL);
        if (h) {
          DispatchFn cfn = (DispatchFn)dlsym(h, sym);
          if (cfn) {
            /* Reuse a stale slot for this same segment (an overlay re-decode)
             * so repeated overlay swaps don't grow the table unbounded. Free the
             * old keys but do NOT dlclose the old .so: an invalidated chunk can
             * still be on the call stack (a sibling chunk in the same segment
             * triggered the overlay load), and unloading code under an active
             * frame would SIGSEGV. Leaking the handle is bounded by the overlay-
             * swap count and harmless. */
            JitChunk *c = NULL;
            for (size_t i = 0; i < jit_chunk_count; ++i) {
              if (jit_chunks[i].stale && jit_chunks[i].seg_base == seg_base) {
                c = &jit_chunks[i];
                free(c->keys);
                free(c->code);
                break;
              }
            }
            if (!c) c = &jit_chunks[jit_chunk_count++];
            c->seg_base = seg_base;
            c->lo = lo;
            c->hi = hi;
            c->fn = cfn;
            c->handle = h;
            c->stale = 0;
            c->keys = NULL;
            c->nkeys = 0;
            if (seg_base + lo < jit_code_lo) jit_code_lo = seg_base + lo;
            if (seg_base + hi > jit_code_hi) jit_code_hi = seg_base + hi;
            jit_load_keys(c, so);
            shim_log_stdout("JIT: chunk %04X:[%04X,%04X) (lin %05X) %s keys=%zu\n",
                            seg, lo, hi, seg_base + off, sym, c->nkeys);
            result = c;
          } else {
            shim_log_stdout("JIT: dlsym %s failed: %s\n", sym, dlerror());
            dlclose(h);
          }
        } else {
          shim_log_stdout("JIT: dlopen failed: %s\n", dlerror());
        }
      } else {
        shim_log_stdout("JIT: compile failed cs:ip=%04X:%04X (rc=%d)\n",
                        seg, off, rc);
      }
    }
  }
  if (!result) {
    /* We dumped the live segment and tried to JIT it but produced no runnable
     * chunk: the bytes at cs:ip did not translate -- an unsupported instruction
     * or data decoded as code, i.e. control almost certainly reached a non-code
     * address via a wrong transfer/return. Per the prime directive a failed
     * data-decode is a HARD failure, not a drop: halt HERE, at the offending
     * address, instead of returning NULL and limping into a later, far less
     * informative unmapped crash. (The early returns above -- no SAISEI_PYTHON,
     * table full, out of range -- are legitimate "JIT unavailable" and already
     * returned; this point is only reached after a real compile attempt.) */
    char msg[640];
    snprintf(msg, sizeof(msg),
        "JIT compile/translate FAILED at cs:ip=%04X:%04X (linear 0x%05X): the "
        "live in-memory bytes did not become a runnable chunk (unsupported "
        "instruction or data decoded as code -- control reached a non-code "
        "address via a wrong transfer). Chunk artifacts in %s. This bundle is "
        "self-contained: jit_translate.log has the translator FATAL "
        "(mnemonic / file-offset / reached-from function), jit_segment.bin is "
        "the exact 64KB this decode saw, and the manifest carries cs:ip -- "
        "re-run 'saisei-jitc jit-compile --mem jit_segment.bin --entry 0x%X "
        "--image-base 0x%X --outdir .' to reproduce.",
        seg, off, seg_base + off, dir, off, seg_base);
    fprintf(stderr,
        "\n[FATAL] %s\n  Halting at the JIT failure (prime directive: a failed "
        "data-decode is a hard failure, not a drop).\n\n", msg);
    /* save_crash_bundle (not save_bug_bundle) so we get the dir back and can
     * attach the translator output + the exact segment bytes -- bringing a JIT
     * failure to parity with a native-crash bundle. */
    const char *bdir = save_crash_bundle("jit_compile_failed", seg_base + off,
                                         msg, strlen(msg));
    if (bdir) {
      shim_log_crash("Bundle: %s\n", bdir);
      if (jitcaplen)
        crash_bundle_write_file(bdir, "jit_translate.log", jitcap, jitcaplen);
      /* The precise bytes the failing decode compiled (still live in memory),
       * independent of any later self-modification the snapshot might show. */
      crash_bundle_write_file(bdir, "jit_segment.bin",
                              (const char *)(virtual_memory + seg_base),
                              0x10000u);
    }
    shim_flush_all_streams();
    exit(1);
  }
  vclock_resume();
  return result;
}

/* ===================== Faithful flat machine =====================
 * The emulated stack (ss:sp, memw_write) is the ONLY return-address store. A
 * single top-level loop (run_machine) drives chunk dispatch; chunks never call
 * each other in C and never longjmp. Every control transfer goes through the
 * emulated cpu.r_cs:cpu.r_ip:
 *
 *   - a chunk runs its `for(;;) switch(pc)` until pc leaves its pc-space; the
 *     `default:` sets cpu.r_ip = pc + cs_base and returns to run_machine, which
 *     re-resolves the owning chunk for the (possibly new) cs:ip.
 *   - far call/jmp/ret/iret set cpu.r_cs:cpu.r_ip directly and return.
 *
 * resolve_and_run_chunk() resolves a linear address to the chunk that owns it
 * (JIT chunk / static-dispatch binary / builtin BIOS stub), runs it exactly once, and
 * returns 1. It returns 0 only when the address maps to nothing dispatchable
 * (genuinely bogus cs:ip -- surfaced by the caller). */

/* Set by dos_exit_impl right before exit(); also lets the nested-ISR loop and
 * run_machine terminate deterministically if exit() is ever bypassed. */
volatile int machine_halted;

/* Run the single chunk that owns linear `addr` (cs:ip already reflect it).
 * On return cpu.r_cs:cpu.r_ip have been updated by the chunk to wherever
 * control flowed (an in-chunk default exit, a far transfer, or a ret/iret).
 * Returns 1 if a chunk/handler was dispatched, 0 if `addr` is unmapped. */
static int try_patch_at(uint32_t addr, uint16_t expected_retip,
                        const char *file, const char *func, int line);

static int resolve_and_run_chunk(uint32_t addr) {
  /* Function-patch hook on the PRIMARY dispatch path (run_machine drives the
   * game by calling this for each cs:ip). Same (file,file_off) key as the
   * dispatch_via_binary hook; chunks here run with expected_retip 0, so a
   * replace-style patch uses patch_ret_near(0) like the chunk it stands in for.
   * No-op when the game declares no patches. */
  if (try_patch_at(addr, 0, "<run_machine>", "resolve_and_run_chunk", 0))
    return 1;
  /* JIT chunks own their decoded linear ranges -- consult them first, but ONLY
   * when the chunk's seg_base matches the live cs (seg_base == cpu.r_cs<<4).
   * A chunk's pc is its offset from seg_base; running it under the cs it was
   * decoded for makes that pc the TRUE cs-relative IP, so near-call return IPs
   * pushed on the stack are segment-relative to the live cpu.r_cs and round-
   * trip correctly through retf / far-jmp. The same physical code reached under
   * a different cs alias (seg_base != cs<<4) must NOT reuse the foreign-base
   * chunk and flip cs underfoot -- it falls through and is re-JITted at the
   * alias's own seg_base below, the faithful "separate chunks per seg base". */
  JitChunk *jc = jit_lookup(addr);
  if (jc) {
    if (jc->seg_base == ((uint32_t)cpu.r_cs << 4)) {
      jc->fn((int)(addr - jc->seg_base), 0, "<run_machine>", __func__, __LINE__);
      return 1;
    }
    /* jit_lookup found this address already decoded in a chunk at a DIFFERENT
     * seg_base -- i.e. the same physical JIT'd code is now reached under a cs
     * alias. Run it in a chunk based at the LIVE cs so its pc == the true
     * cs-relative IP and pushed return IPs round-trip through retf/far-jmp.
     * Crucially do NOT fall through to the binary/file-mapping path: an aliased
     * mapping there dispatches the canonical-base chunk under the alias cs and
     * pushes wrong-relative IPs (the func_89E6 -> 1844:89E6 garbage). If the
     * bytes don't translate, jit_compile_or_get hard-fails (a true wrong
     * transfer halts loudly rather than silently mis-running). */
    uint16_t alias_off = (uint16_t)(addr - ((uint32_t)cpu.r_cs << 4));
    JitChunk *nc = jit_compile_or_get(cpu.r_cs, alias_off);
    if (nc && nc->seg_base == ((uint32_t)cpu.r_cs << 4)) {
      nc->fn((int)(addr - nc->seg_base), 0, "<run_machine>", __func__, __LINE__);
      return 1;
    }
  }
  /* Builtin BIOS/DOS handler (int08h_impl etc.) at a synthetic unmapped
   * address (F060:0000 ...). It runs its emulated firmware and exits via
   * iret_impl/dos_exit, both of which set cpu.r_cs:ip (or terminate). */
  GameFunc bfn = try_call_target(addr);
  if (bfn && is_builtin_call_target(addr)) {
    SAFEPOINT();
    bfn(0, "<run_machine>", __func__, __LINE__);
    return 1;
  }
  const FileMapping *fm = NULL;
  const BinaryDispatch *bd = find_binary_for_addr(addr, &fm);
  if (bd && fm) {
    uint32_t file_off = (addr - fm->base) + (uint32_t)fm->file_offset;
    /* Mapped real code no chunk has decoded yet (overlay / incomplete seeds):
     * JIT the live in-memory bytes at the live cs's own base instead of hitting
     * the binary's default case. (Aliased JIT'd code is already handled in the
     * jit_lookup branch above; this path is reached only when no JIT chunk owns
     * the address yet -- genuine mapped code or a fresh overlay.) */
    if (!shim_pc_is_case_key(bd->module, file_off)) {
      uint16_t joff = (uint16_t)(addr - ((uint32_t)cpu.r_cs << 4));
      JitChunk *nc = jit_compile_or_get(cpu.r_cs, joff);
      if (nc && nc->seg_base == ((uint32_t)cpu.r_cs << 4)) {
        nc->fn((int)(addr - nc->seg_base), 0, "<run_machine>", __func__,
               __LINE__);
        return 1;
      }
    }
    set_dispatch_cs(fm, addr);
    bd->fn((int)file_off, 0, fm->path ? fm->path : "<run_machine>", __func__,
           __LINE__);
    return 1;
  }
  /* Not static code, not a builtin, not yet JIT'd: it may be code an
   * unpacker/overlay just wrote outside any file mapping. JIT it. */
  if (!is_builtin_call_target(addr)) {
    uint16_t joff = (uint16_t)(addr - ((uint32_t)cpu.r_cs << 4));
    JitChunk *nc = jit_compile_or_get(cpu.r_cs, joff);
    if (nc) {
      cpu.r_cs = (uint16_t)(nc->seg_base >> 4);
      nc->fn((int)(addr - nc->seg_base), 0, "<run_machine>", __func__,
             __LINE__);
      return 1;
    }
  }
  /* Last resort: a hand-curated static call_target with no file mapping. */
  if (bfn) {
    SAFEPOINT();
    bfn(0, "<run_machine>", __func__, __LINE__);
    return 1;
  }
  return 0;
}

/* The faithful top-level loop. Resolves cs:ip to its owning chunk and runs it,
 * forever, until the program terminates (dos_exit_impl -> exit(), which never
 * returns; machine_halted is a belt-and-suspenders backstop). An unmapped cs:ip
 * is a real wrong jump (stack imbalance upstream) -- surface it loudly. */
void run_machine(void) {
  while (!machine_halted) {
    uint32_t addr = ((uint32_t)cpu.r_cs << 4) + cpu.r_ip;
    SAFEPOINT();
    if (machine_halted) break;
    if (resolve_and_run_chunk(addr)) continue;
    char msg[1024];
    int n = snprintf(msg, sizeof(msg),
        "[BUG] run_machine: unmapped cs:ip=%04X:%04X (linear 0x%05X)\n"
        "  ss:sp=%04X:%04X active_binary=%s\n"
        "  ax=%04X bx=%04X cx=%04X dx=%04X si=%04X di=%04X bp=%04X ds=%04X es=%04X\n"
        "  diagnosis: control reached a linear address with no JIT chunk, no\n"
        "  file_mapping, and no static call_target. Almost always the emulated\n"
        "  8086 stack popped a bogus value as cs:ip (an upstream push/pop\n"
        "  imbalance), or a far/near transfer computed a wrong target. Search\n"
        "  the trace tail for the last push/pop/ret before this address.\n",
        cs, ip, addr, ss, sp,
        shim_active_binary() ? shim_active_binary() : "<none>",
        ax, bx, cx, dx, si, di, bp, ds, es);
    shim_log_crash("%s", msg);
    if (n > 0) save_bug_bundle("run_machine_unmapped", addr, msg);
    shim_flush_all_streams();
    exit(1);
  }
}

/* ===================== Synchronous EXEC (INT 21h AH=4Bh) =====================
 * Real DOS EXEC loads a child program and RUNS IT TO COMPLETION, returning to
 * the parent after its INT 21h when the child terminates (AH=4Ch). A long_jump
 * is wrong: it sets the child's cs:ip but the parent chunk's switch keeps
 * running its own code. Run the child on a nested machine loop (the invoke_isr
 * pattern): save the parent's registers, dispatch the child until it exits
 * (dos_exit -> shim_exec_child_terminate longjmps back here), then restore the
 * parent so its chunk resumes after the INT. */
#define MAX_EXEC_NEST 8
static int exec_nest_depth;
static jmp_buf exec_nest_env[MAX_EXEC_NEST + 1];
static int exec_nest_status[MAX_EXEC_NEST + 1];

int shim_exec_run_child(uint16_t child_cs, uint16_t child_ip,
                        uint16_t child_ss, uint16_t child_sp,
                        uint16_t child_psp) {
  if (exec_nest_depth >= MAX_EXEC_NEST) {
    return -1; /* refuse runaway EXEC recursion */
  }
  /* Volatile so the post-longjmp restore reads the saved values, not registers
   * the nested run_machine clobbered (C99 6.11.2.1; same rule as invoke_isr). */
  volatile uint16_t p_cs = cs, p_ip = ip, p_ss = ss, p_sp = sp;
  volatile uint16_t p_ds = ds, p_es = es;
  volatile uint16_t p_ax = ax, p_bx = bx, p_cx = cx, p_dx = dx;
  volatile uint16_t p_si = si, p_di = di, p_bp = bp;
  /* The child runs as a normal program with hardware interrupts LIVE (real DOS
   * is not "inside" a call during an EXEC'd child). Our synchronous EXEC runs
   * it nested inside the PARENT's INT 21h critical section; left in place that
   * critical_depth gates off IRQ0 for the child's whole life, freezing the BIOS
   * timer tick and hanging any timer-paced loop (e.g. a child process's intro,
   * which spins waiting for ticks that never arrive). Lift the critical section
   * and enable IF for the child; restore the parent's exact state below. */
  volatile uint8_t p_crit = critical_depth, p_if = IF;
  const char *p_owners[CRITICAL_MAX_DEPTH];
  for (int i = 0; i < (int)p_crit && i < CRITICAL_MAX_DEPTH; ++i) {
    p_owners[i] = critical_owner_name_stk[i];
  }
  int depth = ++exec_nest_depth;
  exec_nest_status[depth] = 0;
  if (setjmp(exec_nest_env[depth]) == 0) {
    /* Hand the CPU to the child. DOS sets DS=ES=PSP; the child reloads them. */
    cpu.r_cs = child_cs;
    cpu.r_ip = child_ip;
    cpu.r_ss = child_ss;
    sp = child_sp;
    cpu.r_ds = child_psp; /* DOS sets DS=ES=child PSP (its own, above the parent) */
    cpu.r_es = child_psp;
    critical_depth = 0; /* child runs with IRQs live (see note above) */
    IF = 1;
    run_machine(); /* returns only if machine_halted; child exit longjmps out */
  }
  int status = exec_nest_status[depth];
  --exec_nest_depth;
  /* Restore the parent's critical-section state (the child's own shim calls
   * churned critical_depth/owner stack while it ran) and IF. */
  critical_depth = p_crit;
  IF = p_if;
  for (int i = 0; i < (int)p_crit && i < CRITICAL_MAX_DEPTH; ++i) {
    critical_owner_name_stk[i] = p_owners[i];
  }
  critical_owner_name = p_crit > 0 ? critical_owner_name_stk[p_crit - 1] : NULL;
  /* Restore the parent so its chunk's switch continues after the INT 21h. */
  cpu.r_cs = p_cs;
  cpu.r_ip = p_ip;
  cpu.r_ss = p_ss;
  sp = p_sp;
  cpu.r_ds = p_ds;
  cpu.r_es = p_es;
  ax = p_ax; bx = p_bx; cx = p_cx; dx = p_dx;
  si = p_si; di = p_di; bp = p_bp;
  return status;
}

/* Called from dos_exit_impl. If a parent EXEC is active, stash the child's exit
 * code and longjmp back to shim_exec_run_child (the child terminates, the
 * parent resumes). Returns 0 when NOT nested, so the top-level program exit
 * (real process exit) proceeds. */
int shim_exec_child_terminate(int status) {
  if (exec_nest_depth > 0) {
    exec_nest_status[exec_nest_depth] = status;
    longjmp(exec_nest_env[exec_nest_depth], 1);
  }
  return 0;
}

/* ===================== Function-patch registry =======================
 * A patch deterministically REPLACES a game function: when control reaches the
 * function's entry, the runtime runs the patch instead of the original. The
 * relationship is bidirectional — the patch can run the original it replaced
 * (patch_call_original) or call any other game function (patch_call_function),
 * and the game reaches the patch through the normal dispatch.
 *
 * Patches are keyed on (binary basename, file_off) — the stable identity the
 * dispatcher resolves addresses to — so one patch works for static-dispatch and JIT code
 * and across cs-aliases. They are registered at startup, either from the
 * built-in game_config.patches table or, separately deliverable, from bundle
 * .so's loaded via patch_load_bundle (see the source --patch). Interception
 * happens at the codegen hook (shim_patch_check, emitted at each function entry)
 * and the dispatch arms below; both consult this one registry. */
#define NO_ACTIVE_PATCH 0xFFFFFFFFu
#define MAX_PATCHES 2048
static GamePatch patch_reg[MAX_PATCHES];     /* the active registry */
static uint32_t patch_reg_lin[MAX_PATCHES];  /* resolved linear addr (0=pending) */
static size_t patch_reg_count;
static int patch_reg_inited;
static int patch_reg_lin_ready;

/* The function currently executing inside its patch (linear addr), or
 * NO_ACTIVE_PATCH. A re-entry to the SAME addr while it is patched is
 * patch_call_original asking for the original body — the hook skips it, which
 * also prevents infinite recursion. */
static uint32_t patch_active_addr = NO_ACTIVE_PATCH;
static uint32_t patch_current_addr = 0;
static uint16_t patch_current_retip = 0;
static const char *patch_current_file = NULL;
static const char *patch_current_func = NULL;
static int patch_current_line = 0;

static int dispatch_via_binary(uint32_t addr, uint16_t expected_retip,
                               const char *file, const char *func, int line);

static const char *patch_path_basename(const char *p) {
  const char *s = p;
  for (const char *c = p; *c; ++c)
    if (*c == '/' || *c == '\\') s = c + 1;
  return s;
}

/* Resolve a (binary basename, file_off) to a live linear address via the file
 * mappings (newest covering entry wins). 0 if not currently mapped. */
static uint32_t patch_resolve_linear(const char *binary, uint32_t file_off) {
  for (ssize_t j = (ssize_t)file_mapping_count - 1; j >= 0; --j) {
    const char *pth = file_mappings[j].path;
    if (!pth) continue;
    if (binary && strcmp(patch_path_basename(pth), binary) != 0) continue;
    uint32_t fo = (uint32_t)file_mappings[j].file_offset;
    if (file_off >= fo && file_off < fo + file_mappings[j].len)
      return file_mappings[j].base + (file_off - fo);
  }
  return 0;
}

/* Append patches to the registry (from game_config or a bundle). */
void patch_register(const GamePatch *arr, size_t n) {
  for (size_t i = 0; i < n && patch_reg_count < MAX_PATCHES; ++i)
    patch_reg[patch_reg_count++] = arr[i];
  patch_reg_lin_ready = 0; /* force re-resolve to include new entries */
  shim_log_stdout("patch: registered %zu patch(es), %zu total\n",
                  n, patch_reg_count);
}

/* Seed the registry from the built-in game_config table (once). */
static void ensure_patch_reg(void) {
  if (patch_reg_inited) return;
  patch_reg_inited = 1;
  if (game_config.patches && game_config.patch_count)
    patch_register(game_config.patches, game_config.patch_count);
}

/* Load a separately-delivered patch bundle: a .so exporting
 * `const GamePatch bundle_patches[]` and `const size_t bundle_patch_count`.
 * The bundle's patch fns resolve runtime symbols via the host's -rdynamic
 * exports, exactly like a JIT chunk. */
void patch_load_bundle(const char *so_path) {
  ensure_patch_reg();
  void *h = dlopen(so_path, RTLD_NOW | RTLD_GLOBAL);
  if (!h) {
    shim_log_stderr("patch: dlopen %s failed: %s\n", so_path, dlerror());
    return;
  }
  const GamePatch *arr = (const GamePatch *)dlsym(h, "bundle_patches");
  const size_t *cnt = (const size_t *)dlsym(h, "bundle_patch_count");
  if (!arr || !cnt) {
    shim_log_stderr("patch: bundle %s lacks bundle_patches/bundle_patch_count\n",
                    so_path);
    return;
  }
  shim_log_stdout("patch: bundle %s -> %zu patch(es)\n", so_path, *cnt);
  patch_register(arr, *cnt);
}

static void ensure_patch_lin(void) {
  ensure_patch_reg();
  if (patch_reg_lin_ready) return;
  int all = 1;
  for (size_t i = 0; i < patch_reg_count; ++i) {
    if (patch_reg_lin[i]) continue;
    uint32_t lin = patch_resolve_linear(patch_reg[i].file, patch_reg[i].file_off);
    if (lin) patch_reg_lin[i] = lin; else all = 0;
  }
  patch_reg_lin_ready = all;
}

/* Run the registered patch for linear address `addr`, if any. Returns 1 if the
 * patch HANDLED the call (caller returns immediately, like a completed
 * dispatch), 0 to run the original. Cheap (a few integer compares) when there
 * are patches; an immediate return when there are none. */
static int try_patch_at(uint32_t addr, uint16_t expected_retip,
                        const char *file, const char *func, int line) {
  ensure_patch_reg();
  if (!patch_reg_count) return 0;
  if (addr == patch_active_addr) return 0; /* call_original re-entry / recursion */
  ensure_patch_lin();
  for (size_t i = 0; i < patch_reg_count; ++i) {
    const GamePatch *p = &patch_reg[i];
    if (!p->enabled || !p->fn || patch_reg_lin[i] != addr) continue;
    uint32_t s_addr = patch_current_addr; uint16_t s_retip = patch_current_retip;
    const char *s_file = patch_current_file; const char *s_func = patch_current_func;
    int s_line = patch_current_line; uint32_t s_active = patch_active_addr;
    patch_active_addr = addr;
    patch_current_addr = addr; patch_current_retip = expected_retip;
    patch_current_file = file; patch_current_func = func; patch_current_line = line;
    int r = p->fn(expected_retip, file, func, line);
    patch_active_addr = s_active;
    patch_current_addr = s_addr; patch_current_retip = s_retip;
    patch_current_file = s_file; patch_current_func = s_func;
    patch_current_line = s_line;
    if (r == PATCH_HANDLED) {
      shim_drain_pending_tail_dispatch(file, func, line);
      return 1;
    }
    return 0; /* declined -> run the original */
  }
  return 0;
}

/* Codegen hook: emitted at each function-entry case of JIT/static dispatch (linear
 * = (cs<<4)+pc). The single deterministic interception point for replacing a
 * function — fires however the entry is reached (chunk entry, cross-chunk
 * dispatch, or intra-chunk near call). `expected_retip` is the caller-pushed
 * return IP, threaded through so patch_call_original can re-run the original
 * with the correct return convention. */
int shim_patch_check(uint32_t linear, uint16_t expected_retip) {
  return try_patch_at(linear, expected_retip, "<chunk>", "shim_patch_check", 0);
}

/* patch -> game: run the ORIGINAL function this patch replaced (do-original-
 * then-enhance). Re-enters dispatch for the same function; try_patch_at skips
 * it because patch_active_addr still equals this addr, so the original body runs
 * and performs its own return back here. */
void patch_call_original(void) {
  dispatch_via_binary(patch_current_addr, patch_current_retip,
                      patch_current_file ? patch_current_file : "patch",
                      patch_current_func ? patch_current_func : "patch",
                      patch_current_line);
}

/* The (binary, file_off) of the function the running patch is replacing — lets a
 * single shared patch fn registered on many functions know which one fired.
 * Returns the file_off; *binary_out (if non-NULL) gets the binary basename. */
uint32_t patch_self_offset(const char **binary_out) {
  const FileMapping *fm = find_file_mapping(patch_current_addr);
  if (binary_out) *binary_out = (fm && fm->path) ? patch_path_basename(fm->path) : "?";
  if (!fm) return patch_current_addr;
  return (patch_current_addr - fm->base) + (uint32_t)fm->file_offset;
}

/* Resolve a linear address to its origin (binary basename, file_off) via the
 * live file mappings — the inverse of patch_resolve_linear. For diagnosing
 * which binary owns a given cs:ip (e.g. a WATCHW writer). */
uint32_t shim_resolve_addr(uint32_t linear, const char **binary_out) {
  const FileMapping *fm = find_file_mapping(linear);
  if (binary_out) *binary_out = (fm && fm->path) ? patch_path_basename(fm->path) : "?";
  if (!fm) return 0xFFFFFFFFu;
  return (linear - fm->base) + (uint32_t)fm->file_offset;
}

/* patch -> game: call ANY game function by (binary basename, file_off). Lets a
 * patch invoke game routines beyond the one it replaced. */
void patch_call_function(const char *binary, uint32_t file_off) {
  uint32_t lin = patch_resolve_linear(binary, file_off);
  if (!lin) {
    shim_log_stderr("patch_call_function: %s+0x%X not mapped\n",
                    binary ? binary : "?", file_off);
    return;
  }
  dispatch_via_binary(lin, 0, "patch", "patch_call_function", 0);
}

/* Perform a near `ret` from inside a PATCH_HANDLED patch (replace style): pop
 * the return IP and route it exactly like the generated handle_ret. */
void patch_ret_near(uint16_t expected_retip) {
  uint16_t popped = memw(ss, sp);
  sp = (uint16_t)((sp + 2) & 0xFFFF);
  near_ret_tail_impl(popped, expected_retip, "patch", "patch_ret_near", 0);
}

static int dispatch_via_binary(uint32_t addr, uint16_t expected_retip,
                               const char *file, const char *func, int line) {
  /* Suppress any stale tail dispatch from an interrupted/just-returned caller
   * BEFORE the JIT-chunk fast-path below. The drain (and other callers) pass
   * addr/expected_retip explicitly and have already captured them, so the
   * pending flag is purely a leftover signal that must not survive into the
   * chunk we are about to run. If it did, a chunk that returns via an
   * expected_retip match (clean near ret, doesn't touch pending) would let
   * jit_dispatch's own drain re-fire the stale pending and pop an extra stack
   * word -- which split the lcall far-return in the music driver and
   * surfaced as the `call_table 0x32901` crash. (Previously this reset lived
   * after the `if (jc) return jit_dispatch(...)` fast-path, so JIT chunks --
   * i.e. the entire JIT dispatch -- never cleared it.) */
  tail_dispatch_pending = false;
  /* Function-patch hook: keyed on (file, file_off) resolved live, this is the
   * single point every cross-binary / cross-chunk CALL funnels through, so it
   * must precede ALL dispatch arms below (the jit_lookup fast path, the
   * first-compile jit_compile_or_get paths, and the static-dispatch trampoline) — a patched
   * function reached by any of them is intercepted. No-op when the game
   * declares no patches. */
  if (try_patch_at(addr, expected_retip, file, func, line)) return 1;
  /* JIT chunks own their decoded linear ranges -- consult them first so a
   * control transfer into already-JIT'd (decompressed) code dispatches
   * directly. No-op (single bounds check on an empty table) when the static path is empty. */
  JitChunk *jc = jit_lookup(addr);
  if (jc) {
    uint32_t live_base = (uint32_t)cpu.r_cs << 4;
    uint32_t alias_off = addr - live_base;
    if (alias_off < 0x10000u && jc->seg_base != live_base) {
      JitChunk *nc = jit_compile_or_get(cpu.r_cs, (uint16_t)alias_off);
      if (nc && nc->seg_base == live_base) jc = nc;
    }
    return jit_dispatch(jc, addr, expected_retip, file, func, line);
  }
  const FileMapping *fm = NULL;
  const BinaryDispatch *bd = find_binary_for_addr(addr, &fm);
  if (!bd) {
    /* A builtin BIOS/DOS handler (int08h_impl etc.) lives at a synthetic
     * unmapped address (F060:0000 ...) and is dispatched via the call_targets
     * table by our caller -- NOT real code. Return "not handled" so that
     * fallthrough runs it, instead of JIT-decoding the empty BIOS area. */
    if (is_builtin_call_target(addr)) return 0;
    /* Not static code and not yet JIT'd -- try to JIT it (it may be code an
     * unpacker/overlay just wrote outside any file mapping). Derive the IP from
     * `addr` (the dispatch target) -- NOT cpu.r_ip, which on a jump_table /
     * call_table dispatch is still the SOURCE instruction, not the target. */
    uint16_t joff = (uint16_t)(addr - ((uint32_t)cpu.r_cs << 4));
    JitChunk *nc = jit_compile_or_get(cpu.r_cs, joff);
    if (nc) return jit_dispatch(nc, addr, expected_retip, file, func, line);
    return 0;
  }
  /* Reconstruction self-discovery (see the unmapped-tail check after the
   * loop): remember the ORIGINAL transfer target and whether it was already
   * a decoded case. A first target that is NOT a case key is normally fine
   * -- the near-ret default re-composes cs:popped_ip into a valid case in
   * this or another binary (cross-binary tail-call). It is only a genuine
   * UNDECODED ENTRY when that re-composition then dead-ends on an unmapped
   * address, which we detect below and JIT from the live bytes. */
  const char *entry_module = bd->module;
  uint32_t entry_off = (addr - fm->base) + (uint32_t)fm->file_offset;
  int entry_was_case_key = shim_pc_is_case_key(entry_module, entry_off);
  uint16_t entry_cs = cpu.r_cs, entry_ip = cpu.r_ip;
  /* Genuine transfer (call/jump/iret -- fresh entries; near-ret trampolines are
   * the loop below, never here) into real code that IS mapped but no chunk has
   * decoded yet: an overlay, or freshly-loaded code. JIT the TARGET now, before the undecoded
   * binary's default case near-rets and walks the stack into garbage. The IP is
   * derived from `addr` (the target) -- NOT cpu.r_ip, which on a jump_table /
   * call_table dispatch is still the SOURCE instruction. A failed compile (NULL)
   * falls through to the legacy trampoline path unchanged. */
  if (!entry_was_case_key) {
    uint16_t joff = (uint16_t)(addr - ((uint32_t)cpu.r_cs << 4));
    JitChunk *nc = jit_compile_or_get(cpu.r_cs, joff);
    if (nc) return jit_dispatch(nc, addr, expected_retip, file, func, line);
  }
  int unmapped_tail = 0;
  uint16_t saved_cs = cs;
  /* tail_dispatch_pending was already reset at function entry (above) so the
   * JIT-chunk fast-path clears it too. */
  ++dispatch_depth; ++dd_inc_via_binary;
  /* The trampoline loop processes cross-binary tail-dispatches set by
   * near_ret_tail. Each iteration: dispatch into the binary at `addr`,
   * the binary's case body runs (possibly making cross-binary calls of
   * its own), eventually a near-ret either matches expected_retip (loop
   * exits) or sets tail_dispatch_pending with a new addr (loop iterates).
   *
   * No "stuck" detector here: legitimate game loops (music sequencer,
   * a menu / dialog input wait) trampoline through the SAME addr
   * repeatedly by design — the case body at that addr does real work
   * each iteration. A "same addr N times = stuck" heuristic would
   * abort/skip those loops. If something genuinely IS stuck (missing
   * extra_entries seed, mid-instruction IP), the game hangs and the
   * user can Ctrl+C; the crash bundle taxonomy then catches the
   * underlying cause via a different code path. */
  do {
    set_dispatch_cs(fm, addr);
    uint32_t file_off = (addr - fm->base) + (uint32_t)fm->file_offset;
    SAFEPOINT();
    bd->fn((int)file_off, expected_retip, file, func, line);
    SAFEPOINT();
    if (!tail_dispatch_pending) break;
    /* Self-loop on undecoded code: the near-ret composed back to the SAME
     * address and that address is not a decoded case -- the static-dispatch
     * default near-rets pc->pc forever (e.g. returning into a region the unpacker
     * decompressed over the packed image, which no chunk has decoded).
     * JIT the in-memory code there instead of spinning. A legitimate game loop
     * trampolines a DECODED case, so the case-key check excludes it. */
    if (tail_dispatch_addr == addr &&
        !shim_pc_is_case_key(bd->module, file_off)) {
      uint16_t exp = tail_dispatch_expected;
      tail_dispatch_pending = false;
      JitChunk *jc = jit_compile_or_get(cpu.r_cs, cpu.r_ip);
      --dispatch_depth; ++dd_dec_via_binary;
      cpu.r_cs = saved_cs;
      if (jc) return jit_dispatch(jc, addr, exp, file, func, line);
      return 1; /* JIT unavailable/failed -- stop spinning */
    }
    addr = tail_dispatch_addr;
    expected_retip = tail_dispatch_expected;
    tail_dispatch_pending = false;
    fm = NULL;
    bd = find_binary_for_addr(addr, &fm);
    if (!bd) {
      unmapped_tail = 1;
    } else if (!shim_pc_is_case_key(
                   bd->module,
                   (addr - fm->base) + (uint32_t)fm->file_offset)) {
      /* The near-ret trampolined into code that IS mapped but not yet
       * decoded (e.g. freshly-loaded overlay code). Without
       * this, its dispatch hits the default case, which near-rets again and
       * walks the stack word-by-word into garbage (the descending 0984/0884/...
       * chain) instead of running the real return target. JIT the target now;
       * cpu.r_cs:r_ip == addr here (near_ret_tail set them). A failed compile
       * (garbage, not real code) returns NULL and falls through to the legacy
       * walk -- so genuine returns get JIT'd, stray stack words don't wedge. */
      JitChunk *nc = jit_compile_or_get(cpu.r_cs, cpu.r_ip);
      if (nc) {
        --dispatch_depth; ++dd_dec_via_binary;
        cpu.r_cs = saved_cs;
        return jit_dispatch(nc, addr, expected_retip, file, func, line);
      }
    }
  } while (bd);
  --dispatch_depth; ++dd_dec_via_binary;
  cpu.r_cs = saved_cs;

  /* Reconstruction self-discovery: the trampoline dead-ended on an unmapped
   * address AND the ORIGINAL control transfer landed on a code address that
   * was never decoded into its dispatch switch. That first target is a real
   * UNDECODED ENTRY (e.g. a far-jump into a relocated/self-modified code
   * region the JIT never reached) -- not a near-ret. JIT it from the live
   * bytes below instead of silently bubbling out of the dispatch loop. (When
   * the first target WAS a case key, an unmapped tail is a genuine return-to-
   * DOS / overlay-not-loaded case -- left as-is.) */
  if (unmapped_tail && !entry_was_case_key) {
    cpu.r_cs = entry_cs;
    cpu.r_ip = entry_ip;
    /* JIT the original transfer target: real but not-yet-decoded code
     * (decompressed/overlay/self-modified). Decode the in-memory bytes,
     * compile+load a chunk, and dispatch into it instead of aborting. Only the
     * static-only path (JIT disabled) falls through to the unhandled-pc bundle
     * + exit. */
    uint32_t entry_lin = ((uint32_t)entry_cs << 4) + entry_ip;
    JitChunk *nc = jit_compile_or_get(entry_cs, entry_ip);
    if (nc) return jit_dispatch(nc, entry_lin, expected_retip, file, func, line);
    char buf[2048];
    shim_unhandled_pc_report(entry_module, (int)entry_off, buf, sizeof(buf));
    shim_log_crash("%s", buf);
    save_bug_bundle("unhandled_pc", entry_lin, buf);
    shim_flush_all_streams();
    exit(1);
  }
  return 1;
}

/* Overlay-aware indirect dispatch — prefer the live-mapping path over the
 * static call_targets table.
 *
 * Rationale: linear addresses in multi-chunk overlay archives can
 * correspond to different functions in different chunks.
 * Static `games/<game>.json` `call_targets` entries bind a linear address
 * to ONE symbol, picked from one observation; if a later overlay reuses
 * the same linear address for different code, the static binding silently
 * runs the wrong function. `dispatch_via_binary` looks up the currently
 * loaded chunk via `file_mappings` and computes the file_off live — always
 * matches whatever chunk is actually executing. This mirrors real-DOS
 * semantics: a far/indirect call jumps to whatever bytes are at the
 * computed address right now, no static "function table" exists.
 *
 * The static `try_call_target` is kept as a fallback for linear addresses
 * that no file_mapping covers (BIOS stubs, external code with hand-curated
 * call_targets entries). Returns 1 on dispatch, 0 if the caller should
 * abort via `lookup_call_target` (which logs and aborts on miss). */
static int try_dispatch_overlay_first(uint32_t addr, uint16_t expected_retip,
                                      const char *file, const char *func,
                                      int line) {
  if (dispatch_via_binary(addr, expected_retip, file, func, line)) return 1;
  GameFunc fn = try_call_target(addr);
  if (!fn) return 0;
  const FileMapping *fm = find_file_mapping(addr);
  uint16_t saved_cs = cs;
  set_dispatch_cs(fm, addr);
  ++dispatch_depth; ++dd_inc_overlay_first;
  SAFEPOINT();
  fn(expected_retip, file, func, line);
  SAFEPOINT();
  --dispatch_depth; ++dd_dec_overlay_first;
  cpu.r_cs = saved_cs;
  return 1;
}

/* Faithful near indirect jmp `jmp [mem]` / `jmp reg`: the target is a 16-bit
 * offset within the current code segment (caller passed addr = cs<<4 +
 * target_off). No stack change; set cpu.r_ip and return to the top-level loop,
 * which re-resolves the owning chunk. */
void jump_table_impl(uint32_t addr, uint16_t expected_retip, const char *file,
                     const char *func, int line) {
  (void)expected_retip;
  shim_log_stdout("Trace: jump_table 0x%08X (%s:%s:%d)\n", addr, file, func,
                  line);
  lifecycle_log_dispatch("JMP", addr);
  cpu.r_ip = (uint16_t)(addr - ((uint32_t)cs << 4));
}

/* Faithful cross-chunk near `ret`: the chunk popped an IP that is not a case in
 * its own switch (the `default:` arm), so control leaves this chunk's pc-space.
 * Set cpu.r_ip to the popped offset (cs unchanged -- near ret) and return to
 * the top-level loop, which re-resolves the owning chunk. No drift checks, no
 * windowed recovery, no tail-dispatch trampoline -- the popped value is the
 * faithful return IP. */
void near_ret_tail_impl(uint16_t popped_ip, uint16_t expected_retip,
                        const char *file, const char *func, int line) {
  (void)expected_retip;
  uint32_t addr = ((uint32_t)cs << 4) + popped_ip;
  shim_log_stdout("Trace: near_ret_tail to %04X:%04X (0x%08X) (%s:%s:%d)\n", cs,
                  popped_ip, addr, file, func, line);
  if (isr_depth == 0) {
    const FileMapping *fm = find_file_mapping(addr);
    const char *bn = fm && fm->path ? (strrchr(fm->path, '/') ? strrchr(fm->path, '/') + 1 : fm->path) : "<unmapped>";
    size_t off_in = (fm && fm->path) ? fm->file_offset + (addr - fm->base) : 0;
    lifecycle_log("NRET 0x%05X popped=%04X -> %s+0x%zX\n", addr, popped_ip, bn,
                  off_in);
  }
  cpu.r_ip = popped_ip;
}

void shim_dump_memory(uint32_t offset, size_t length) {
  if (offset >= MEMORY_SIZE) {
    return;
  }
  if (offset + length > MEMORY_SIZE) {
    length = MEMORY_SIZE - offset;
  }
  for (size_t pos = 0; pos < length; pos += 16) {
    size_t line_len = (length - pos > 16) ? 16 : (length - pos);
    shim_log_stdout("%06X:", offset + pos);
    for (size_t i = 0; i < 16; ++i) {
      if (i < line_len) {
        shim_log_stdout(" %02X", virtual_memory[offset + pos + i]);
      } else {
        shim_log_stdout("   ");
      }
    }
    shim_log_stdout("  |");
    for (size_t i = 0; i < line_len; ++i) {
      uint8_t b = virtual_memory[offset + pos + i];
      shim_log_stdout("%c", (b >= 32 && b <= 126) ? b : '.');
    }
    shim_log_stdout("|\n");
  }
}

void shim_dump_whole_memory(void) { shim_dump_memory(0, MEMORY_SIZE); }

/* Render the current video memory into a PNG written to ``path``.  Returns
 * 0 on success, -1 on failure.  Pure rendering — no directory creation. */

/* Counter for RAM snapshot files, per-process. Matches the screenshot
 * counter pattern (snapshots/snap_<N>.bin). */
static int ram_snapshot_counter = 1;

static void shim_dump_ram_snapshot(void) {
  const char *dir = "snapshots";
  if (mkdir(dir, 0755) && errno != EEXIST) {
    perror("mkdir snapshots");
    return;
  }
  char path[128];
  snprintf(path, sizeof(path), "%s/snap_%d.bin", dir, ram_snapshot_counter);
  int fd = open(path, O_WRONLY | O_CREAT | O_TRUNC, 0644);
  if (fd < 0) {
    perror(path);
    return;
  }
  size_t off = 0;
  while (off < SHIM_MEMORY_SIZE) {
    ssize_t w = write(fd, virtual_memory + off, SHIM_MEMORY_SIZE - off);
    if (w <= 0) break;
    off += (size_t)w;
  }
  close(fd);
  shim_log_stdout("[SNAP] ram → %s (%zu bytes, counter=%d)\n", path, off,
          ram_snapshot_counter);
  ram_snapshot_counter++;
}

/* Stable path — overwrites each call. Caller polls/reads it immediately. */
static void shim_read_memory_to_sidecar(uint32_t addr, uint8_t len) {
  if ((size_t)addr + (size_t)len > SHIM_MEMORY_SIZE) {
    shim_log_stdout("[READ] out of bounds addr=0x%X len=%u\n", addr, len);
    return;
  }
  const char *dir = "snapshots";
  if (mkdir(dir, 0755) && errno != EEXIST) {
    perror("mkdir snapshots");
    return;
  }
  char path[128];
  snprintf(path, sizeof(path), "%s/last_read.bin", dir);
  int fd = open(path, O_WRONLY | O_CREAT | O_TRUNC, 0644);
  if (fd < 0) {
    perror(path);
    return;
  }
  ssize_t w = write(fd, virtual_memory + addr, len);
  close(fd);
  shim_log_stdout("[READ] addr=0x%X len=%u wrote=%zd → %s\n", addr, len, w, path);
}

void shim_save_video_memory(void) {
  const char *dir = "screenshots";
  if (mkdir(dir, 0755) && errno != EEXIST) {
    perror("mkdir");
    return;
  }
  char path[128];
  snprintf(path, sizeof(path), "%s/screenshot%d.png", dir,
           screenshot_counter++);
  shim_render_screenshot_png(path);

  // Save the current VGA palette as a 16x16 image identified by its CRC.
  unsigned int crc = stbiw__crc32(vga.palette, sizeof(vga.palette));
  snprintf(path, sizeof(path), "%s/pallet_%08x.png", dir, crc);

  uint8_t palette_img[16 * 16 * 3];
  for (int i = 0; i < 256; ++i) {
    uint8_t r = vga.palette[i * 3];
    uint8_t g = vga.palette[i * 3 + 1];
    uint8_t b = vga.palette[i * 3 + 2];
    r = (r << 2) | (r >> 4);
    g = (g << 2) | (g >> 4);
    b = (b << 2) | (b >> 4);
    palette_img[i * 3] = r;
    palette_img[i * 3 + 1] = g;
    palette_img[i * 3 + 2] = b;
  }
  stbi_write_png(path, 16, 16, 3, palette_img, 16 * 3);
}

// Wrappers exporting the original symbols without location information
void safe_point(void) { safe_point_impl("<external>", __func__, 0); }
void long_jump(uint16_t seg, uint16_t off) {
  long_jump_impl(seg, off, "<external>", __func__, 0);
}

void lcall_table(uint16_t ret_ip, uint16_t seg, uint16_t off) {
  lcall_table_impl(ret_ip, seg, off, "<external>", __func__, 0);
}

void call_table(uint16_t ret_ip, uint32_t addr) {
  /* Test seam: when armed, an unmapped target unwinds back here (see
   * report_unmapped) instead of exit(1)ing. No-op in production. */
  if (shim_fatal_armed) {
    if (setjmp(shim_fatal_env)) return;
  }
  call_table_impl(ret_ip, addr, "<external>", __func__, 0);
}

void jump_table(uint32_t addr, uint16_t expected_retip) {
  jump_table_impl(addr, expected_retip, "<external>", __func__, 0);
}

/* Public wrapper around the static dispatch_via_binary so the snapshot
 * module can re-enter dispatch at a saved cs:ip after --restore-from. */
int shim_dispatch_via_binary(uint32_t addr, uint16_t expected_retip,
                             const char *file, const char *func, int line) {
  return dispatch_via_binary(addr, expected_retip, file, func, line);
}

void shim_tail_dispatch_save(ShimTailDispatchState *out) {
  out->pending = tail_dispatch_pending;
  out->addr = tail_dispatch_addr;
  out->expected = tail_dispatch_expected;
  tail_dispatch_pending = false;
}

void shim_tail_dispatch_restore(const ShimTailDispatchState *in) {
  tail_dispatch_pending = in->pending;
  tail_dispatch_addr = in->addr;
  tail_dispatch_expected = in->expected;
}

void shim_drain_pending_tail_dispatch(const char *file, const char *func,
                                      int line) {
  while (tail_dispatch_pending) {
    uint32_t addr = tail_dispatch_addr;
    uint16_t expected = tail_dispatch_expected;
    /* Try the trampoline loop first (handles all the normal cases:
     * overlay-aware dispatch + further cross-binary tail chains). */
    if (dispatch_via_binary(addr, expected, file, func, line)) continue;
    /* No binary mapping for this linear address. Try the static
     * call_targets table (BIOS handlers like int08h_impl). This mirrors
     * the original near_ret_tail fallback chain that existed before
     * the trampoline refactor. */
    GameFunc fn = try_call_target(addr);
    if (fn) {
      tail_dispatch_pending = false;
      ++dispatch_depth; ++dd_inc_overlay_first;
      fn(expected, file, func, line);
      --dispatch_depth; ++dd_dec_overlay_first;
      continue;
    }
    /* Contained-fault recovery: the dead-end happened while inside an lcall'd
     * routine (a callee that walked its stack into garbage -- e.g. an
     * imperfectly reconstructed driver). Rather than abort the whole program,
     * return from that innermost lcall (restore the saved far-return + the
     * 4-byte frame) so the caller continues. The fault is logged loudly. */
    if (lcall_depth > 0) {
      shim_log_crash(
          "[WARN] contained lcall fault: tail dead-end 0x%X at depth %d -- "
          "returning from lcall to %04X:%04X (callee likely mis-reconstructed)\n",
          (unsigned)addr, lcall_depth, lcall_ret_cs[lcall_depth],
          lcall_ret_ip[lcall_depth]);
      cpu.r_cs = lcall_ret_cs[lcall_depth];
      cpu.r_ip = lcall_ret_ip[lcall_depth];
      cpu.r_ss = lcall_expected_ss[lcall_depth];
      sp = (uint16_t)(lcall_expected_sp[lcall_depth] + 4);
      last_retf_pop_bytes = 0;
      tail_dispatch_pending = false;
      longjmp(lcall_return_env[lcall_depth], 1);
    }
    /* Truly unmapped target — this is the "segment popped as IP" /
     * stack-imbalance signature reaching daylight. Abort loudly with a
     * crash bundle rather than letting the game silently "exit cleanly"
     * — that was the bug behind the strange post-restore EXIT_CLEAN. */
    char msg[1024];
    int n = snprintf(msg, sizeof(msg),
        "[BUG] tail dispatch to unmapped target 0x%X (expected_retip=%04X)\n"
        "  caller: %s:%s:%d\n"
        "  cs:ip=%04X:%04X  ss:sp=%04X:%04X  active_binary=%s\n"
        "  ax=%04X bx=%04X cx=%04X dx=%04X si=%04X di=%04X bp=%04X\n"
        "  diagnosis: a near_ret_tail or cross-binary dispatch landed at a\n"
        "  linear address with NO file_mapping AND no static call_target.\n"
        "  Almost always the simulated 8086 stack popped a segment value\n"
        "  as IP (stack imbalance from a translator/shim bug), or the\n"
        "  saved snapshot was captured in an inconsistent state and the\n"
        "  first ret after restore consumed garbage. Search the trace\n"
        "  tail for the last push/pop pair before this dispatch.\n",
        (unsigned)addr, (unsigned)expected,
        file ? file : "?", func ? func : "?", line,
        cs, ip, ss, sp,
        shim_active_binary() ? shim_active_binary() : "<none>",
        ax, bx, cx, dx, si, di, bp);
    shim_log_crash("%s", msg);
    if (n > 0) save_bug_bundle("tail_dispatch_unmapped", addr, msg);
    shim_flush_all_streams();
    abort();
  }
}

int main(int argc, char **argv) {
#ifdef FORCE_EXIT_AFTER_10S
  setup_force_exit();
#endif
  const char *restore_from = NULL;
  for (int i = 1; i < argc; ++i) {
    if (strcmp(argv[i], "--headless") == 0) {
      headless_mode = 1;
      const char *shot_secs = getenv("SAISEI_SCREENSHOT_SECS");
      if (shot_secs && *shot_secs) SCREENSHOT_INTERVAL_SECS = atoi(shot_secs);
      continue;
    }
    if (strncmp(argv[i], "--restore-from=", 15) == 0) {
      restore_from = argv[i] + 15;
      continue;
    }
    if (strcmp(argv[i], "--restore-from") == 0) {
      if (i + 1 >= argc) {
        fprintf(stderr, "Missing value after --restore-from\n");
        return 2;
      }
      restore_from = argv[++i];
      continue;
    }
    if (strncmp(argv[i], "--speedup=", 10) == 0) {
      char *end = NULL;
      const double parsed = strtod(argv[i] + 10, &end);
      if (end && *end == '\0' && parsed > 0.0) {
        emulation_speedup = parsed;
      } else {
        fprintf(stderr,
                "Invalid --speedup value '%s' (expected positive number)\n",
                argv[i] + 10);
        return 2;
      }
      continue;
    }
    if (strcmp(argv[i], "--speedup") == 0) {
      if (i + 1 >= argc) {
        fprintf(stderr, "Missing value after --speedup\n");
        return 2;
      }
      char *end = NULL;
      const double parsed = strtod(argv[++i], &end);
      if (end && *end == '\0' && parsed > 0.0) {
        emulation_speedup = parsed;
      } else {
        fprintf(stderr,
                "Invalid --speedup value '%s' (expected positive number)\n",
                argv[i]);
        return 2;
      }
      continue;
    }
    /* Load a separately-delivered patch bundle (.so). Repeatable. */
    if (strcmp(argv[i], "--patch-bundle") == 0) {
      if (i + 1 >= argc) {
        fprintf(stderr, "Missing value after --patch-bundle\n");
        return 2;
      }
      patch_load_bundle(argv[++i]);
      continue;
    }
    if (strncmp(argv[i], "--patch-bundle=", 15) == 0) {
      patch_load_bundle(argv[i] + 15);
      continue;
    }
  }
  shim_log_stdout("Emulation speedup multiplier: %.2fx\n", emulation_speedup);
  /* Snapshot/keylog/atexit/restore live in scripts/snapshot.c. */
  snapshot_init();
  if (!headless_mode) {
    init_virtual_display();
    atexit(virtual_display_shutdown);
  }
  if (restore_from) {
    if (snapshot_restore_and_resume(restore_from) != 0) {
      fprintf(stderr, "restore: failed; not resuming.\n");
      return 3;
    }
    /* Same as the entry-returned case below: if the restored dispatch
     * returns cleanly, the game's main loop has exited without calling
     * DOS terminate. Mark it so the silent exit is visible. */
    fprintf(stderr,
            "\n[EXIT] restored dispatch returned without calling DOS terminate. "
            "cs:ip=%04X:%04X active_binary=%s\n"
            "[EXIT]   game's main loop exited via near ret instead of INT "
            "21h AH=4Ch. No bundle written; check why the dispatch bubbled "
            "out (translator CFG issue or runtime ret-target mismatch).\n",
            cs, ip, shim_active_binary() ? shim_active_binary() : "<none>");
    extern void save_manager_sr_log(const char *fmt, ...);
    save_manager_sr_log("exit RESTORE_DISPATCH_RETURNED cs:ip=%04X:%04X "
                        "active=%s (game main bubbled out without DOS "
                        "terminate after restore)", cs, ip,
                        shim_active_binary() ? shim_active_binary() : "<none>");
    fflush(stderr);
  } else if (game_config.program_path) {
    /* Faithful flat machine: the loader (init_memory/load_executable) left
     * cpu.r_cs:cpu.r_ip at the program entry taken from the MZ header.
     * run_machine resolves cs:ip to its owning chunk -- JIT-compiling the live
     * segment on first reach -- runs it, then re-resolves wherever control
     * flowed, forever, until DOS terminate calls exit(). The chunks never call
     * each other in C and never longjmp; the emulated stack is the only
     * return-address store. (program_path being set means a game config was
     * linked in; a non-loaded config errors out below.) */
    run_machine();
    /* run_machine only returns if machine_halted was set without exit() -- i.e.
     * the game's main loop bubbled out without a real DOS terminate. */
    fprintf(stderr,
            "\n[EXIT] run_machine returned without calling DOS terminate. "
            "cs:ip=%04X:%04X active_binary=%s\n"
            "[EXIT]   this is a translator/CFG bug: the game's main bubbled "
            "out via near ret instead of INT 21h AH=4Ch. No bundle written "
            "(it's not a runtime crash); investigate why the dispatch loop "
            "exited.\n",
            cs, ip, shim_active_binary() ? shim_active_binary() : "<none>");
    extern void save_manager_sr_log(const char *fmt, ...);
    save_manager_sr_log("exit ENTRY_RETURNED cs:ip=%04X:%04X active=%s "
                        "(translator bug: main bubbled out without DOS "
                        "terminate)", cs, ip,
                        shim_active_binary() ? shim_active_binary() : "<none>");
    fflush(stderr);
  } else {
    shim_log_stdout("Warning: no entry point configured; nothing to execute.\n");
  }
  return 0;
}

/* ===== Shim runtime state capture/restore for snapshots =====
 *
 * Snapshots previously only saved the simulated memory + CPU regs +
 * keyboard. After restore, the BIOS video mode, OPL2 audio chip state,
 * PIT timer state, DOS heap pointer, and pending IRQs were all reset to
 * boot defaults — so the visible game state (graphics mode, music,
 * timing, allocations) didn't match what the simulated memory expected.
 * Restored game ran in a broken state.
 *
 * These helpers serialize/deserialize all of that to the bundle. The
 * struct is versioned in shims.h so adding more fields later is safe.
 */
void shim_runtime_state_capture(ShimRuntimeState *out) {
  if (!out) return;
  memset(out, 0, sizeof(*out));
  out->version = SHIM_RUNTIME_STATE_VERSION;

  out->bios_video.video_mode = bios_video.video_mode;
  memcpy(out->bios_video.cursor_row, bios_video.cursor_row, sizeof(out->bios_video.cursor_row));
  memcpy(out->bios_video.cursor_col, bios_video.cursor_col, sizeof(out->bios_video.cursor_col));
  memcpy(out->bios_video.cursor_attr, bios_video.cursor_attr, sizeof(out->bios_video.cursor_attr));
  out->bios_video.active_page = bios_video.active_page;
  out->bios_video.cga_palette_select = bios_video.cga_palette_select;
  out->bios_video.cga_border_color = bios_video.cga_border_color;
  out->cga = cga;
  out->current_display_width = (int32_t)current_display_width;
  out->current_display_height = (int32_t)current_display_height;
  out->virtual_display_buffer = (int32_t)virtual_display_buffer;

  out->vga = vga;

  out->opl2 = opl2;

  out->pit = pit;
  out->pit_reload_value = pit_reload_value;
  out->pit_latched_value = pit_latched_value;
  out->pit_latch_valid = pit_latch_valid;
  out->pit_read_buffer = pit_read_buffer;
  out->pit_read_expect_high = pit_read_expect_high;
  out->pit_read_buffer_is_latch = pit_read_buffer_is_latch;
  out->bios_timer_tick_backlog = bios_timer_tick_backlog;
  out->bios_timer_tick_preincremented = bios_timer_tick_preincremented;
  out->pit_cycle_accum = pit_cycle_accum;
  out->pit_cycle_fraction_accum = pit_cycle_fraction_accum;

  out->next_free_seg = next_free_seg;
  out->program_min_block_paras = program_min_block_paras;
  memcpy(out->null_guard_initial, null_guard_initial, sizeof(out->null_guard_initial));
  out->a20_enabled = (uint8_t)(a20_enabled ? 1 : 0);

  out->irq0_pending = irq0_pending;
  memcpy(out->irq_pending, irq_pending, sizeof(out->irq_pending));
  out->last_int_no = last_int_no;
}

int shim_runtime_state_restore(const ShimRuntimeState *in) {
  if (!in) return -1;
  if (in->version != SHIM_RUNTIME_STATE_VERSION) {
    fprintf(stderr,
            "shim_runtime_state_restore: version mismatch — bundle has v%u, "
            "binary expects v%u. Re-capture the snapshot with the current "
            "build.\n",
            (unsigned)in->version, SHIM_RUNTIME_STATE_VERSION);
    return -1;
  }

  /* Video first — apply_video_mode_state has side effects (display
   * geometry, BIOS data area writes) we want to happen before later
   * cursor/CRTC overrides land. */
  apply_video_mode_state(in->bios_video.video_mode);
  memcpy(bios_video.cursor_row, in->bios_video.cursor_row, sizeof(bios_video.cursor_row));
  memcpy(bios_video.cursor_col, in->bios_video.cursor_col, sizeof(bios_video.cursor_col));
  memcpy(bios_video.cursor_attr, in->bios_video.cursor_attr, sizeof(bios_video.cursor_attr));
  bios_video.active_page = in->bios_video.active_page;
  bios_video.cga_palette_select = in->bios_video.cga_palette_select;
  bios_video.cga_border_color = in->bios_video.cga_border_color;
  cga = in->cga;
  current_display_width = (int)in->current_display_width;
  current_display_height = (int)in->current_display_height;
  virtual_display_buffer = (int)in->virtual_display_buffer;

  /* VGA state — apply AFTER apply_video_mode_state above so it isn't
   * stomped (apply_video_mode_state currently leaves vga.palette alone
   * but other VGA regs could be reset by future mode-switch logic). */
  vga = in->vga;

  opl2 = in->opl2;

  pit = in->pit;
  pit_reload_value = in->pit_reload_value;
  pit_latched_value = in->pit_latched_value;
  pit_latch_valid = in->pit_latch_valid;
  pit_read_buffer = in->pit_read_buffer;
  pit_read_expect_high = in->pit_read_expect_high;
  pit_read_buffer_is_latch = in->pit_read_buffer_is_latch;
  bios_timer_tick_backlog = in->bios_timer_tick_backlog;
  bios_timer_tick_preincremented = in->bios_timer_tick_preincremented;
  pit_cycle_accum = in->pit_cycle_accum;
  pit_cycle_fraction_accum = in->pit_cycle_fraction_accum;

  next_free_seg = in->next_free_seg;
  program_min_block_paras = in->program_min_block_paras;
  memcpy(null_guard_initial, in->null_guard_initial, sizeof(null_guard_initial));
  a20_set_enabled(in->a20_enabled != 0);

  irq0_pending = in->irq0_pending;
  memcpy(irq_pending, in->irq_pending, sizeof(irq_pending));
  last_int_no = in->last_int_no;
  return 0;
}
