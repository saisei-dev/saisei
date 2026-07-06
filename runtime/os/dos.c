/* ============================ os/dos ============================
 * DOS (INT 21h) services + their helpers, extracted from shims.c. This is
 * the OS layer the translated game runs on: file I/O over the host fs,
 * memory allocation, console I/O, date/time, the interrupt-vector table,
 * etc. The functions are declared in runtime_abi.h / shims.h (generated
 * game code calls them directly, and shims.c's INT21h dispatcher routes
 * here); this TU implements them over shared core state (the DOS file
 * handle table, PSP/next_free_seg) exposed from shims.c.
 */
/* Define the *_impl wrappers as functions, not the convenience
 * macros (same as shims.c does). */
#define SHIMS_DISABLE_MACROS
#include "shims.h"
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <errno.h>
#include <limits.h>
#include <dirent.h>
#include <ctype.h>
#include <sys/stat.h>
#include <unistd.h>
#include <fnmatch.h>
#include <sys/time.h>

static uint16_t errno_to_dos(int err);

/* All DOS logical drives map onto the single host working directory. Strip a
 * leading "X:" drive specifier (and any following root "\" / "/" separators) so
 * paths like "K:\MIDI.DRV", "C:\GAMEDIR\X" or the doubled "C:\\DIGI.DRV" that
 * SETUP builds resolve into cwd. This matters for CREATE/WRITE opens: fopen()
 * of a literal "K:\..." (or "\DIGI.DRV") name would otherwise create a weird
 * host file with a backslash IN ITS NAME instead of writing the intended file
 * in the working dir -- and a later read of the bare "DIGI.DRV" would then miss
 * it. (Read opens also fall back to resolve_case_insensitive_path, which strips
 * the prefix; this makes create/open consistent.)
 *
 * SETUP's driver-copy formats the destination as "C:\\NAME" (a DOUBLED root
 * backslash). Dropping only ONE separator left "\NAME", so the copy created
 * "\DIGI.DRV" on the host while the dialog renderer opened the bare "DIGI.DRV"
 * -> the read failed, the fread returned an uninitialised buffer, and the
 * dialog's interpolation descriptors (driver header offset 0x9C/0xB2) were
 * garbage, which ran the glyph-fill loop off the end of the stack. Strip the
 * WHOLE leading run of separators so both halves agree on "DIGI.DRV". */
static const char *dos_strip_drive_prefix(const char *path) {
  if (!path) {
    return path;
  }
  if (path[0] != '\0' && path[1] == ':') {
    path += 2;
  }
  /* Drop the entire leading root-separator run so "\DIGI.DRV", "C:\DIGI.DRV"
   * and "C:\\DIGI.DRV" all resolve relative to the host working dir. (Interior
   * "A\B" separators are left intact -- only the leading run is collapsed.) */
  while (path[0] == '\\' || path[0] == '/') {
    path += 1;
  }
  return path;
}

/* DOS "current PSP" (INT 21h AH=50h set / 51h,62h get). Single process, so it
 * defaults to our one PSP; programs set/restore it during their own startup.
 * Initialised to the default layout; init_psp() rebinds it to the live psp_seg
 * (which a game config may have lowered). */
static uint16_t dos_current_psp = DEFAULT_PSP_SEG;
void dos_set_current_psp_to_load(void) { dos_current_psp = PSP_SEG; }

/* DOS console input returns EXTENDED keys (arrows, function keys, etc.) as a
 * two-byte sequence: a 0x00 lead byte on the first read, then the BIOS
 * scancode on the NEXT read. Our keyboard delivers such a key as a single
 * (ascii==0, scancode) event, so we split it across two console reads here.
 * Shared by all the AH=01/06/07/08 console-input paths so the lead byte and
 * its scancode are never interleaved with another key. */
static uint8_t dos_ext_scancode_pending; /* 0 = none queued */

uint8_t dos_read_char_impl(const char *file, const char *func, int line) {
  shim_log(__func__, file, func, line, NULL);
  if (dos_ext_scancode_pending) {
    al = dos_ext_scancode_pending;
    dos_ext_scancode_pending = 0;
    dos_write_char_impl(al, file, func, line);
    return 0;
  }
  uint8_t saved_crit, saved_if;
  shim_dos_input_wait_begin(&saved_crit, &saved_if);
  while (1) {
    uint8_t ascii, scan;
    /* Block until the BIOS INT 09h handler has placed a key in the type-ahead
     * buffer. SAFEPOINT() delivers IRQ1 which drains port 0x60 and fills it. */
    if (kbd_bios_pop(&ascii, &scan)) {
      if (ascii == 0)
        dos_ext_scancode_pending = scan;
      dos_write_char_impl(ascii, file, func, line);
      al = ascii;
      shim_dos_input_wait_end(saved_crit, saved_if);
      return 0;
    }
    SAFEPOINT();
  }
}

uint8_t dos_read_char(void) {
  return dos_read_char_impl("<external>", __func__, 0);
}

uint8_t dos_write_char_impl(uint8_t ch_val, const char *file, const char *func,
                            int line) {
  unsigned char ch_str[2];
  ch_str[0] = ch_val;
  ch_str[1] = '\0';
  shim_log(__func__, file, func, line, (char *)ch_str);
  fputc(ch_val, stdout);
  fflush(stdout);
  return 0;
}

uint8_t dos_write_char(uint8_t ch_val) {
  return dos_write_char_impl(ch_val, "<external>", __func__, 0);
}

uint8_t dos_direct_console_io_impl(uint8_t dl_val, const char *file,
                                   const char *func, int line) {
  shim_log(__func__, file, func, line, NULL);
  if (dl_val == 0xFF) {
    SAFEPOINT();
    uint8_t ascii;
    if (kbd_bios_pop(&ascii, NULL)) {
      al = ascii;
      ZF = 0;
    } else {
      ZF = 1;
    }
  } else {
    dos_write_char_impl(dl_val, file, func, line);
    al = dl_val;
    ZF = 0;
  }
  return 0;
}

uint8_t dos_direct_console_io(uint8_t dl_val) {
  return dos_direct_console_io_impl(dl_val, "<external>", __func__, 0);
}

uint8_t dos_check_keyboard_status_impl(const char *file, const char *func,
                                       int line) {
  shim_log(__func__, file, func, line, NULL);
  if (kbd_bios_has_event()) {
    al = 0xFF;
    ZF = 0;
  } else {
    al = 0x00;
    ZF = 1;
  }
  CF = 0;
  return 0;
}

uint8_t dos_check_keyboard_status(void) {
  return dos_check_keyboard_status_impl("<external>", __func__, 0);
}

/* INT 21h AH=0Ah: buffered keyboard input into DS:DX. The buffer is
 *   [0] = max length (set by caller; counts the trailing CR)
 *   [1] = chars entered (filled by DOS, excluding the CR)
 *   [2..] = the typed characters, terminated by a 0x0D
 * DOS reads a whole line with on-screen echo and Backspace editing, blocking
 * until Enter. Mirrors dos_read_char_impl's blocking idiom: SAFEPOINT()
 * delivers IRQ1, the BIOS INT 09h handler fills the type-ahead buffer, and
 * kbd_bios_pop drains it. */
uint8_t dos_buffered_input_impl(const char *file, const char *func, int line) {
  shim_log(__func__, file, func, line, NULL);
  uint8_t *buf = seg_off(ds, dx);
  uint8_t maxlen = buf[0];
  if (maxlen == 0) {
    return 0; /* no room even for the CR */
  }
  uint8_t count = 0;
  uint8_t saved_crit, saved_if;
  shim_dos_input_wait_begin(&saved_crit, &saved_if);
  while (1) {
    uint8_t ascii, scan;
    if (!kbd_bios_pop(&ascii, &scan)) {
      SAFEPOINT();
      continue;
    }
    if (ascii == 0x0D) { /* Enter: terminate the line */
      buf[1] = count;
      buf[2 + count] = 0x0D;
      dos_write_char_impl(0x0D, file, func, line);
      dos_write_char_impl(0x0A, file, func, line);
      shim_dos_input_wait_end(saved_crit, saved_if);
      return 0;
    }
    if (ascii == 0x08) { /* Backspace: erase the last char on screen + buffer */
      if (count > 0) {
        count--;
        dos_write_char_impl(0x08, file, func, line);
        dos_write_char_impl(0x20, file, func, line);
        dos_write_char_impl(0x08, file, func, line);
      }
      continue;
    }
    /* Printable char: store while a slot remains before the reserved CR. */
    if (ascii >= 0x20 && count < (uint8_t)(maxlen - 1)) {
      buf[2 + count] = ascii;
      count++;
      dos_write_char_impl(ascii, file, func, line);
    }
    /* Otherwise (buffer full or a non-printable control) ignore the key, as
     * the DOS line editor does. */
  }
}

uint8_t dos_buffered_input(void) {
  return dos_buffered_input_impl("<external>", __func__, 0);
}

uint8_t dos_reset_disk_impl(const char *file, const char *func, int line) {
  shim_log(__func__, file, func, line, NULL);
  return 0;
}

uint8_t dos_reset_disk(void) {
  return dos_reset_disk_impl("<external>", __func__, 0);
}

static uint8_t dos_current_drive = 0; /* 0 = A: */

uint8_t dos_select_drive_impl(uint8_t dl_val, const char *file,
                              const char *func, int line) {
  shim_log(__func__, file, func, line, NULL);
  /* AH=0Eh Select default drive (DL = drive index, 0=A). We map the whole DOS
   * filesystem onto the single host working dir, so any drive is "present".
   * Track the selection so AH=19h reads it back (the installer's drive-probe
   * does select(dl)+get and compares; a no-op get always returned 0, which
   * made every drive but A: look invalid). AL returns the number of drives. */
  dos_current_drive = dl_val;
  al = 26; /* number of logical drives */
  return 0;
}

uint8_t dos_select_drive(uint8_t dl_val) {
  return dos_select_drive_impl(dl_val, "<external>", __func__, 0);
}

uint8_t dos_print_string_impl(const char *str, const char *file,
                              const char *func, int line) {
  shim_log(__func__, file, func, line, NULL);
  /* DOS strings are '$' terminated. */
  size_t len = 0;
  while (str[len] && str[len] != '$') {
    ++len;
  }
  if (len > 0) {
    /* This is the GAME's console output (AH=09h), not diagnostic tracing --
     * write it straight to stdout like dos_write_char does. Routing it through
     * shim_log_stdout would let --silent (which mutes the trace/log channel)
     * wrongly swallow the program's own text. */
    fwrite(str, 1, len, stdout);
    fflush(stdout);
  }

  return 0;
}

uint8_t dos_print_string(const char *str) {
  return dos_print_string_impl(str, "<external>", __func__, 0);
}

uint8_t dos_get_current_drive_impl(const char *file, const char *func,
                                   int line) {
  shim_log(__func__, file, func, line, NULL);
  al = dos_current_drive;
  return 0;
}

uint8_t dos_get_current_drive(void) {
  return dos_get_current_drive_impl("<external>", __func__, 0);
}

uint8_t dos_set_dta_impl(void *dta, const char *file, const char *func,
                         int line) {
  shim_log(__func__, file, func, line, NULL);
  dta_ptr = dta;
  return 0;
}

uint8_t dos_set_dta(void *dta) {
  return dos_set_dta_impl(dta, "<external>", __func__, 0);
}

uint8_t dos_set_interrupt_vector_impl(uint8_t int_no, uint16_t segment,
                                      uint16_t offset, const char *file,
                                      const char *func, int line) {
  shim_log(__func__, file, func, line, NULL);
  uint16_t off = (uint16_t)int_no * 4;
  memw_raw_write(0, off, offset);
  memw_raw_write(0, off + 2, segment);
  return 0;
}

uint8_t dos_set_interrupt_vector(uint8_t int_no, uint16_t segment,
                                 uint16_t offset) {
  return dos_set_interrupt_vector_impl(int_no, segment, offset, "<external>",
                                       __func__, 0);
}

uint8_t dos_get_disk_free_space_impl(uint8_t dl_val, const char *file,
                                     const char *func, int line) {
  shim_log(__func__, file, func, line, NULL);
  (void)dl_val;
  return 0;
}

uint8_t dos_get_disk_free_space(uint8_t dl_val) {
  return dos_get_disk_free_space_impl(dl_val, "<external>", __func__, 0);
}

uint8_t dos_get_version_impl(const char *file, const char *func, int line) {
  shim_log(__func__, file, func, line, NULL);
  /* DOS function 0x30 returns the version number with the major component in
   * AL and the minor component in AH.  Some translated programs (e.g. Av)
   * refuse to run if the reported DOS version is below 3.x.  The previous
   * implementation always returned 2.0, causing Av to exit immediately. */
  al = 3; /* Major version */
  ah = 0; /* Minor version */
  bh = 0; /* OEM number */
  bl = 0; /* Reserved */
  CF = 0;
  return 0;
}

uint8_t dos_get_version(void) {
  return dos_get_version_impl("<external>", __func__, 0);
}

/* AH=2Ch Get time: CH=hour, CL=min, DH=sec, DL=hundredths. */
uint8_t dos_get_time_impl(const char *file, const char *func, int line) {
  shim_log(__func__, file, func, line, NULL);
  struct timeval tv;
  gettimeofday(&tv, NULL);
  struct tm *tm = localtime(&tv.tv_sec);
  ch = (uint8_t)tm->tm_hour;
  cl = (uint8_t)tm->tm_min;
  dh = (uint8_t)tm->tm_sec;
  dl = (uint8_t)(tv.tv_usec / 10000);
  CF = 0;
  return 0;
}

uint8_t dos_get_time(void) {
  return dos_get_time_impl("<external>", __func__, 0);
}

/* AH=2Ah Get date: CX=year, DH=month, DL=day, AL=weekday (0=Sunday). */
uint8_t dos_get_date_impl(const char *file, const char *func, int line) {
  shim_log(__func__, file, func, line, NULL);
  struct timeval tv;
  gettimeofday(&tv, NULL);
  struct tm *tm = localtime(&tv.tv_sec);
  cx = (uint16_t)(tm->tm_year + 1900);
  dh = (uint8_t)(tm->tm_mon + 1);
  dl = (uint8_t)tm->tm_mday;
  al = (uint8_t)tm->tm_wday;
  CF = 0;
  return 0;
}

uint8_t dos_get_date(void) {
  return dos_get_date_impl("<external>", __func__, 0);
}

/* AH=07h Direct console input without echo: block for a key, return AL=char. */
uint8_t dos_console_input_no_echo_impl(const char *file, const char *func,
                                       int line) {
  shim_log(__func__, file, func, line, NULL);
  if (dos_ext_scancode_pending) {
    al = dos_ext_scancode_pending;
    dos_ext_scancode_pending = 0;
    return 0;
  }
  uint8_t saved_crit, saved_if;
  shim_dos_input_wait_begin(&saved_crit, &saved_if);
  while (1) {
    uint8_t ascii, scan;
    /* Block until the BIOS INT 09h handler has placed a key in the type-ahead
     * buffer (filled via IRQ1/port 0x60 at SAFEPOINT). */
    if (kbd_bios_pop(&ascii, &scan)) {
      if (ascii == 0)
        dos_ext_scancode_pending = scan;
      al = ascii;
      shim_dos_input_wait_end(saved_crit, saved_if);
      return 0;
    }
    SAFEPOINT();
  }
}

uint8_t dos_console_input_no_echo(void) {
  return dos_console_input_no_echo_impl("<external>", __func__, 0);
}

uint8_t dos_get_interrupt_vector_impl(const char *file, const char *func,
                                      int line) {
  shim_log(__func__, file, func, line, NULL);
  uint8_t int_no = al;
  uint16_t off = (uint16_t)int_no * 4;
  bx = memw_raw_read(0, off);
  es = memw_raw_read(0, off + 2);
  return 0;
}

uint8_t dos_get_interrupt_vector(void) {
  return dos_get_interrupt_vector_impl("<external>", __func__, 0);
}

uint32_t interrupt_vector_addr_impl(uint8_t int_no, const char *file,
                                    const char *func, int line) {
  shim_log(__func__, file, func, line, NULL);
  uint16_t off = (uint16_t)int_no * 4;
  uint16_t offset = memw_raw_read(0, off);
  uint16_t segment = memw_raw_read(0, off + 2);
  return ((uint32_t)segment << 4) + offset;
}

uint32_t interrupt_vector_addr(uint8_t int_no) {
  return interrupt_vector_addr_impl(int_no, "<external>", __func__, 0);
}

uint8_t dos_make_dir_impl(const char *path, const char *file, const char *func,
                          int line) {
  shim_log(__func__, file, func, line, path);
  (void)path;
  return 0;
}

uint8_t dos_make_dir(const char *path) {
  return dos_make_dir_impl(path, "<external>", __func__, 0);
}

uint8_t dos_change_dir_impl(const char *path, const char *file,
                            const char *func, int line) {
  shim_log(__func__, file, func, line, path);
  (void)path;
  return 0;
}

uint8_t dos_change_dir(const char *path) {
  return dos_change_dir_impl(path, "<external>", __func__, 0);
}

void log_last_sw_interrupt_snapshot(void) {
  if (!last_sw_interrupt.valid) {
    shim_log_stdout("Trace: last_sw_interrupt: <none>\n");
    return;
  }
  shim_log_stdout(
      "Trace: last_sw_interrupt int=0x%02X at %s:%s:%d\n",
      last_sw_interrupt.int_no,
      last_sw_interrupt.file ? last_sw_interrupt.file : "<unknown>",
      last_sw_interrupt.func ? last_sw_interrupt.func : "<unknown>",
      last_sw_interrupt.line);
  shim_log_stdout(
      "Trace:   before cs:ip=%04X:%04X ss:sp=%04X:%04X ds:es=%04X:%04X ax=%04X bx=%04X cx=%04X dx=%04X\n",
      last_sw_interrupt.cs_before, last_sw_interrupt.ip_before,
      last_sw_interrupt.ss_before, last_sw_interrupt.sp_before,
      last_sw_interrupt.ds_before, last_sw_interrupt.es_before,
      last_sw_interrupt.ax_before, last_sw_interrupt.bx_before,
      last_sw_interrupt.cx_before, last_sw_interrupt.dx_before);
  shim_log_stdout(
      "Trace:   after  cs:ip=%04X:%04X ss:sp=%04X:%04X ds:es=%04X:%04X ax=%04X bx=%04X cx=%04X dx=%04X\n",
      last_sw_interrupt.cs_after, last_sw_interrupt.ip_after,
      last_sw_interrupt.ss_after, last_sw_interrupt.sp_after,
      last_sw_interrupt.ds_after, last_sw_interrupt.es_after,
      last_sw_interrupt.ax_after, last_sw_interrupt.bx_after,
      last_sw_interrupt.cx_after, last_sw_interrupt.dx_after);
}


uint8_t dos_open_file_impl(const char *path, const char *file, const char *func,
                           int line) {
  const uint8_t *ptr = (const uint8_t *)path;
  const char *p = path;
  if (ptr >= virtual_memory && ptr < virtual_memory + MEMORY_SIZE) {
    uintptr_t addr = (uintptr_t)(ptr - virtual_memory);
    uint16_t seg = addr >> 4;
    uint16_t off = addr & 0xF;
    shim_log_stdout("Trace: dos_open_file ds:dx=%04X:%04X\n", seg, off);
  } else {
    shim_log_stdout("Trace: dos_open_file ds:dx=%04X:%04X\n", ds, dx);
  }
  if (!p || p[0] == '\0') {
    shim_log(__func__, file, func, line, "<empty>");
    shim_log_stdout("Error: dos_open_file: empty path\n");
    ax = 3; /* path not found */
    return 1;
  }
  char buf[260];
  size_t i = 0;
  const char *p_stripped = dos_strip_drive_prefix(p);
  while (i < sizeof(buf) - 1 && p_stripped[i] != '\0' && p_stripped[i] != '\r') {
    buf[i] = p_stripped[i];
    ++i;
  }
  buf[i] = '\0';

  const char *open_path = buf;
  shim_log(__func__, file, func, line, open_path);

  shim_log_stdout("Trace: dos_open_file: %s\n", open_path);

  shim_log_stdout("Trace: %s path bytes:", __func__);
  for (size_t j = 0; open_path[j] != '\0'; ++j) {
    shim_log_stdout(" %02X", (unsigned char)open_path[j]);
  }
  shim_log_stdout("\n");

  /* DOS INT 21h AH=3Dh passes the access mode in AL bits 0-2:
   *   0 = read-only, 1 = write-only, 2 = read/write.
   * The setup's driver-copy opens the destination file for WRITE (mode 1/2)
   * and then issues AH=40h writes to it. Honoring the mode is essential — an
   * unconditional "rb" makes those writes fail with EBADF, which aborts the
   * driver copy and prevents SETUP.DAT from ever being written. Use "r+b" for
   * write/RW so an existing file is opened without truncation (DOS open does
   * not truncate; that's what AH=3Ch create is for). */
  uint8_t dos_access_mode = al & 0x07;
  const char *fopen_mode = (dos_access_mode == 0) ? "rb" : "r+b";

  for (int i = 0; i < MAX_DOS_HANDLES; ++i) {
    if (handles[i] == NULL) {
      handles[i] = fopen_case_insensitive(open_path, fopen_mode);
      if (handles[i] != NULL) {
        handle_paths[i] = strdup(open_path);
        handle_paths_owned[i] = true;
        ax = i;
        bx = i;
        return 0;
      }
      shim_log_stdout("Error: dos_open_file: failed to open %s: %s\n",
                      open_path, strerror(errno));
      ax = errno_to_dos(errno); /* AX = DOS error code (2 = not found) */
      return 1;
    }
  }
  shim_log_stdout("Error: dos_open_file: no handles available for %s\n",
                  open_path);
  ax = 4; /* too many open files */
  return 1;
}

uint8_t dos_open_file(const char *path) {
  CRITICAL_ENTER();
  uint8_t result = dos_open_file_impl(path, "<external>", __func__, 0);
  CRITICAL_EXIT();
  return result;
}

uint8_t dos_close_file_impl(uint16_t handle, const char *file, const char *func,
                            int line) {
  shim_log(__func__, file, func, line, NULL);
  if (handle < MAX_DOS_HANDLES && handles[handle] != NULL) {
    if (is_standard_handle(handle)) {
      return 0;
    }
    fclose(handles[handle]);
    handles[handle] = NULL;
    if (handle_paths_owned[handle]) {
      free(handle_paths[handle]);
      handle_paths_owned[handle] = false;
    }
    handle_paths[handle] = NULL;
    return 0;
  }
  return 1;
}

uint8_t dos_close_file(uint16_t handle) {
  CRITICAL_ENTER();
  uint8_t result = dos_close_file_impl(handle, "<external>", __func__, 0);
  CRITICAL_EXIT();
  return result;
}

uint8_t dos_read_file_impl(uint16_t handle, void *buf, uint16_t len,
                           const char *file, const char *func, int line) {
  if (handle < MAX_DOS_HANDLES && handles[handle] != NULL) {
    const char *path = handle_paths[handle];
    shim_log(__func__, file, func, line, path);
    if (path) {
      shim_log_stdout("Trace: dos_read_file: %s -> %p\n", path, buf);
    }
    long pos = ftell(handles[handle]);
    uint8_t *tmp = malloc(len);
    if (tmp == NULL) {
      shim_log_stdout("Trace: dos_read_file allocation failure\n");
      ax = 0;
      return 1;
    }
    size_t r = fread(tmp, 1, len, handles[handle]);
    ax = (uint16_t)r;
    uint8_t *p = buf;
    bool p_in_virtual_memory =
        (p >= virtual_memory && p < virtual_memory + MEMORY_SIZE);
    if (path != NULL) {
      shim_log_file_load(path, p, r, pos >= 0 ? (size_t)pos : 0);
    }
    if (p_in_virtual_memory) {
      uint32_t base = (uint32_t)(p - virtual_memory);
      /* This read loads (possibly new overlay) code over [base, base+r). Any
       * runtime-JIT chunk decoded from the bytes previously there is now stale
       * -- drop it so the next control transfer re-decodes the freshly loaded
       * code instead of running the old overlay's dispatch. */
      shim_jit_invalidate_code_range(base, (uint32_t)r);
      for (size_t i = 0; i < r; ++i) {
        uint32_t addr = wrap_segoff_addr(base, (uint32_t)i);
        /* No warn_on_mutation here. dos_read_file is an authorized disk
         * load: shim_log_file_load above already evicted any overlapping
         * mapping and registered the new one covering exactly these
         * bytes. The per-byte writes ARE the load, not arbitrary
         * cross-binary writes. warn_on_mutation here false-positives on
         * every overlay reload — concretely, the game's loadFile at
         * case 0x0500 reads (up to 0xFFFF bytes) into a buffer
         * that overlaps the music driver's region. Same address in real DOS,
         * because the game allocated one ~256KB block via DOS AH=48h and
         * uses it as both binary-load area and scratch. RCB protected-
         * slot writes are still caught via write_watch_log →
         * protected_slots_check_write. */
        /* DOS file reads bypass memb_write_impl, so any read whose buffer
         * crosses a watched range needs explicit WATCHW logging — catches
         * a load that overflows or mis-targets into RCB slots. */
        write_watch_log(mask_addr(addr), 1, tmp[i], file, func, line);
        virtual_memory[mask_addr(addr)] = tmp[i];
      }
    } else {
      shim_log_stdout("Trace: dos_read_file buffer %p outside virtual memory\n",
                      buf);
      memcpy(buf, tmp, r);
    }
    if (r > 0) {
      shim_log_stdout("Trace: dos_read_file data:");
      size_t to_dump = r < 16 ? r : 16;
      for (size_t i = 0; i < to_dump; ++i) {
        shim_log_stdout(" %02X", tmp[i]);
      }
      if (r > to_dump) {
        shim_log_stdout(" ...");
      }
      shim_log_stdout("\n");
    }
    free(tmp);
    return 0;
  }
  shim_log(__func__, file, func, line, NULL);
  ax = 0;
  return 1;
}

uint8_t dos_read_file(uint16_t handle, void *buf, uint16_t len) {
  CRITICAL_ENTER();
  uint8_t result =
      dos_read_file_impl(handle, buf, len, "<external>", __func__, 0);
  CRITICAL_EXIT();
  return result;
}


uint8_t dos_write_file_impl(uint16_t handle, const void *buf, uint16_t len,
                            const char *file, const char *func, int line) {
  shim_log(__func__, file, func, line, NULL);
  if (handle < MAX_DOS_HANDLES && handles[handle] != NULL) {
    const char *path = handle_paths[handle] ? handle_paths[handle] : "<unnamed>";
    shim_log_stdout("Trace: dos_write_file: handle=%u path=%s len=%u\n", handle,
                    path, (unsigned)len);
    
    if (len > 0 && len <= 128 && buf != NULL) {
      shim_log_stdout("         data: ");
      const unsigned char *bytes = buf;
      for (uint16_t i = 0; i < len; ++i) {
        unsigned char byte_val = bytes[i];
        if (byte_val >= 0x20 && byte_val <= 0x7E) {
          shim_log_stdout("%c", byte_val);
        } else if (byte_val == '\n') {
          shim_log_stdout("\\n");
        } else if (byte_val == '\r') {
          shim_log_stdout("\\r");
        } else {
          shim_log_stdout("\\x%02X", byte_val);
        }
      }
      shim_log_stdout("\n");
    }
    const uint8_t *src = (const uint8_t *)buf;
    uint8_t *tmp = NULL;
    bool src_in_virtual_memory =
        (src >= virtual_memory && src < virtual_memory + MEMORY_SIZE);
    if (src_in_virtual_memory && len > 0) {
      uint32_t base = (uint32_t)(src - virtual_memory);
      tmp = malloc(len);
      if (tmp == NULL) {
        ax = errno_to_dos(ENOMEM);
        shim_log_stdout("Trace: dos_write_file allocation failure\n");
        return 1;
      }
      for (uint16_t i = 0; i < len; ++i) {
        uint32_t addr = wrap_segoff_addr(base, (uint32_t)i);
        tmp[i] = virtual_memory[mask_addr(addr)];
      }
      src = tmp;
    }

    size_t r = fwrite(src, 1, len, handles[handle]);
    free(tmp);
    if (r == len) {
      ax = (uint16_t)r;
      return 0;
    }
    int err = ferror(handles[handle]) ? errno : EIO;
    ax = errno_to_dos(err);
    shim_log_stdout(
        "Trace: dos_write_file: handle=%u wrote %zu/%u bytes (errno=%d)\n",
        handle, r, (unsigned)len, err);
    return 1;
  }
  ax = errno_to_dos(EBADF);
  shim_log_stdout("Trace: dos_write_file: invalid handle %u\n", handle);
  return 1;
}

uint8_t dos_write_file(uint16_t handle, const void *buf, uint16_t len) {
  return dos_write_file_impl(handle, buf, len, "<external>", __func__, 0);
}

uint8_t dos_get_file_attributes_impl(const char *path, uint16_t *attr,
                                     const char *file, const char *func,
                                     int line) {
  shim_log(__func__, file, func, line, path);
  (void)path;
  (void)attr;
  return 0;
}

uint8_t dos_get_file_attributes(const char *path, uint16_t *attr) {
  return dos_get_file_attributes_impl(path, attr, "<external>", __func__, 0);
}

/* AH=41h Delete File (DS:DX -> ASCIZ path). Like make_dir/change_dir this is
 * a no-op-success stub: the game's delete is acknowledged (CF clear) without
 * touching the host filesystem. */
uint8_t dos_delete_file_impl(const char *path, const char *file,
                             const char *func, int line) {
  shim_log(__func__, file, func, line, path);
  (void)path;
  CF = 0;
  return 0;
}

uint8_t dos_delete_file(const char *path) {
  return dos_delete_file_impl(path, "<external>", __func__, 0);
}

/* AH=56h Rename File: DS:DX = existing name, ES:DI = new name. SETUP uses this
 * to shuffle its temporary work files (and may rename the freshly-written
 * config into place). Strip the DOS drive/root prefix from both names (same as
 * open/create) so a "C:\\X" source/target maps onto the host working dir, then
 * rename() the host file. Returns CF=0 / AX=0 on success; on a missing source
 * we report success (CF=0) rather than abort -- SETUP often renames optional
 * scratch files that may not exist, and a hard failure here would derail the
 * config write. */
uint8_t dos_rename_impl(const char *old_path, const char *new_path,
                        const char *file, const char *func, int line) {
  shim_log(__func__, file, func, line, old_path);
  if (!old_path || !new_path) {
    CF = 1;
    ax = 3; /* path not found */
    return 1;
  }
  char old_buf[260], new_buf[260];
  const char *o = dos_strip_drive_prefix(old_path);
  const char *n = dos_strip_drive_prefix(new_path);
  size_t i = 0;
  for (; i < sizeof(old_buf) - 1 && o[i] != '\0' && o[i] != '\r'; ++i)
    old_buf[i] = o[i];
  old_buf[i] = '\0';
  for (i = 0; i < sizeof(new_buf) - 1 && n[i] != '\0' && n[i] != '\r'; ++i)
    new_buf[i] = n[i];
  new_buf[i] = '\0';
  shim_log_stdout("Trace: dos_rename: %s -> %s\n", old_buf, new_buf);
  if (rename(old_buf, new_buf) == 0) {
    CF = 0;
    ax = 0;
    return 0;
  }
  if (errno == ENOENT) {
    /* Source absent: treat as a no-op success (optional scratch file). */
    CF = 0;
    ax = 0;
    return 0;
  }
  shim_log_stdout("Error: dos_rename: %s -> %s: %s\n", old_buf, new_buf,
                  strerror(errno));
  CF = 1;
  ax = errno_to_dos(errno);
  return 1;
}

uint8_t dos_rename(const char *old_path, const char *new_path) {
  return dos_rename_impl(old_path, new_path, "<external>", __func__, 0);
}

/* AH=44h IOCTL. AL selects the subfunction.
 *  AL=00 (get device info): report a regular disk file (ISDEV bit clear).
 *  AL=0Dh (generic IOCTL): CX = category(CH):minor(CL). CL=0x60 "Get Device
 *    Parameters" fills the Device Parameters block at DS:DX. DM's launcher reads
 *    the device-TYPE field (+1) and dispatches a per-device handler through a
 *    jump table indexed by device_type*2 -- so this MUST return a real block, or
 *    the garbage type lands on a garbage handler. We back the virtual C: drive
 *    with a fixed-disk device type + a standard 512-byte-sector FAT BPB. */
uint8_t dos_ioctl_impl(uint8_t subfunc, uint16_t bx_handle, const char *file,
                       const char *func, int line) {
  shim_log(__func__, file, func, line, NULL);
  (void)bx_handle;
  if (subfunc == 0x00) {
    dx = 0; /* bit 7 (ISDEV) clear => regular file; low byte = drive A */
  } else if (subfunc == 0x0D && (cx & 0xFF) == 0x60) {
    uint32_t b = (((uint32_t)ds << 4) + dx) & 0xFFFFF;
#define DPB(off, v) virtual_memory[(b + (off)) & 0xFFFFF] = (uint8_t)(v)
#define DPW(off, v) do { DPB((off), (v) & 0xFF); DPB((off) + 1, ((v) >> 8) & 0xFF); } while (0)
    DPB(0x00, 0x00);       /* special functions: use the BPB below */
    DPB(0x01, 0x05);       /* device type: 0x05 = fixed disk */
    DPW(0x02, 0x0001);     /* device attributes: bit0=1 non-removable */
    DPW(0x04, 0x0130);     /* number of cylinders */
    DPB(0x06, 0x00);       /* media type */
    /* device BPB at +7 */
    DPW(0x07, 512);        /* bytes per sector */
    DPB(0x09, 0x04);       /* sectors per cluster */
    DPW(0x0A, 1);          /* reserved sectors */
    DPB(0x0C, 2);          /* number of FATs */
    DPW(0x0D, 512);        /* root directory entries */
    DPW(0x0F, 0);          /* total sectors (small; 0 => use large at +0x1C) */
    DPB(0x11, 0xF8);       /* media descriptor: fixed disk */
    DPW(0x12, 0x0080);     /* sectors per FAT */
    DPW(0x14, 0x0011);     /* sectors per track */
    DPW(0x16, 0x0004);     /* heads */
    DPW(0x18, 0); DPW(0x1A, 0);     /* hidden sectors (dword) */
    DPW(0x1C, 0x8000); DPW(0x1E, 0); /* large total sectors (dword) */
#undef DPB
#undef DPW
  }
  CF = 0;
  return 0;
}

uint8_t dos_ioctl(uint8_t subfunc, uint16_t bx_handle) {
  return dos_ioctl_impl(subfunc, bx_handle, "<external>", __func__, 0);
}

/* AH=47h Get Current Directory (DS:SI <- 64-byte buffer, DL=drive). The path
 * is returned without the drive letter or leading backslash; the current
 * directory is the root, i.e. the empty string. */
uint8_t dos_get_current_dir_impl(char *buf, const char *file,
                                 const char *func, int line) {
  shim_log(__func__, file, func, line, NULL);
  if (buf) {
    buf[0] = '\0';
  }
  CF = 0;
  return 0;
}

uint8_t dos_get_current_dir(char *buf) {
  return dos_get_current_dir_impl(buf, "<external>", __func__, 0);
}

static uint16_t errno_to_dos(int err) {
  switch (err) {
  case ENOENT:
    return 2; /* file not found -- games check for this to fall back to
                 defaults (e.g. missing config), so it must be exact */
  case ENOTDIR:
    return 3; /* path not found */
  case EACCES:
  case EPERM:
    return 5; /* access denied */
  case EMFILE:
  case ENFILE:
    return 4; /* too many open files */
  case EBADF:
    return 6; /* invalid handle */
  case EINVAL:
    return 0x1F; /* invalid parameter */
  default:
    return 1; /* general failure */
  }
}

uint8_t dos_lseek_impl(uint16_t handle, uint16_t off_hi, uint16_t off_lo,
                       uint8_t origin, const char *file, const char *func,
                       int line) {
  shim_log(__func__, file, func, line, NULL);
  if (handle < MAX_DOS_HANDLES && handles[handle] != NULL) {
    off_t off;
    if (origin == 0) {
      uint32_t off32 = ((uint32_t)off_hi << 16) | (uint32_t)off_lo;
      off = (off_t)off32;
    } else {
      uint32_t off32 = ((uint32_t)off_hi << 16) | (uint32_t)off_lo;
      int32_t s_off = (int32_t)off32;
      off = (off_t)s_off;
    }
    int whence;
    switch (origin) {
    case 0:
      whence = SEEK_SET;
      break;
    case 1:
      whence = SEEK_CUR;
      break;
    case 2:
      whence = SEEK_END;
      break;
    default:
      ax = errno_to_dos(EINVAL);
      shim_log_stdout("Trace: dos_lseek: invalid origin %u for handle %u\n",
                      origin, handle);

      return 1;
    }
    if (fseeko(handles[handle], off, whence) == 0) {
      off_t pos = ftello(handles[handle]);
      if (pos != (off_t)-1) {
        uint32_t u_pos = (uint32_t)pos;
        ax = (uint16_t)(u_pos & 0xFFFF);
        dx = (uint16_t)(u_pos >> 16);
        shim_log_stdout("Trace: dos_lseek: handle=%u -> 0x%08X\n", handle,
                        u_pos);

        return 0;
      }
    }
    int err = errno;
    ax = errno_to_dos(err);
    shim_log_stdout("Trace: dos_lseek: handle=%u failed (errno=%d)\n", handle,
                    err);

    return 1;
  }
  ax = errno_to_dos(EBADF);
  shim_log_stdout("Trace: dos_lseek: invalid handle %u\n", handle);

  return 1;
}

uint8_t dos_lseek(uint16_t handle, uint16_t off_hi, uint16_t off_lo,
                  uint8_t origin) {
  CRITICAL_ENTER();
  uint8_t result =
      dos_lseek_impl(handle, off_hi, off_lo, origin, "<external>", __func__,
                     0);
  CRITICAL_EXIT();
  return result;
}

static void format_search_template(uint8_t dest[11], const char *spec) {
  if (!spec || *spec == '\0') {
    spec = "*.*";
  }
  for (int j = 0; j < 11; ++j) {
    dest[j] = ' ';
  }
  const char *dot = strrchr(spec, '.');
  size_t name_len = dot ? (size_t)(dot - spec) : strlen(spec);
  if (name_len > 8) {
    name_len = 8;
  }
  for (size_t j = 0; j < name_len; ++j) {
    dest[j] = (uint8_t)toupper((unsigned char)spec[j]);
  }
  if (dot && dot[1] != '\0') {
    ++dot;
    size_t ext_len = strlen(dot);
    if (ext_len > 3) {
      ext_len = 3;
    }
    for (size_t j = 0; j < ext_len; ++j) {
      dest[8 + j] = (uint8_t)toupper((unsigned char)dot[j]);
    }
  }
}

static void format_dos_filename(char dest[13], const char *src) {
  if (!src) {
    dest[0] = '\0';
    return;
  }
  const char *dot = strrchr(src, '.');
  size_t name_len = dot ? (size_t)(dot - src) : strlen(src);
  while (name_len > 0 && src[name_len - 1] == ' ') {
    --name_len;
  }
  if (name_len > 8) {
    name_len = 8;
  }
  char base[9];
  for (size_t j = 0; j < name_len; ++j) {
    base[j] = (char)toupper((unsigned char)src[j]);
  }
  base[name_len] = '\0';
  char ext[4] = "";
  if (dot && dot[1] != '\0') {
    ++dot;
    size_t ext_len = strlen(dot);
    if (ext_len > 3) {
      ext_len = 3;
    }
    for (size_t j = 0; j < ext_len; ++j) {
      ext[j] = (char)toupper((unsigned char)dot[j]);
    }
    ext[ext_len] = '\0';
  }
  if (ext[0] != '\0') {
    snprintf(dest, 13, "%s.%s", base, ext);
  } else {
    snprintf(dest, 13, "%s", base);
  }
}

static uint16_t dos_encode_time(const struct tm *tm_ptr) {
  int hour = tm_ptr ? tm_ptr->tm_hour : 0;
  int minute = tm_ptr ? tm_ptr->tm_min : 0;
  int second = tm_ptr ? tm_ptr->tm_sec : 0;
  return (uint16_t)(((hour & 0x1F) << 11) | ((minute & 0x3F) << 5) |
                    ((second / 2) & 0x1F));
}

static uint16_t dos_encode_date(const struct tm *tm_ptr) {
  int year = tm_ptr ? tm_ptr->tm_year + 1900 : 1980;
  int month = tm_ptr ? tm_ptr->tm_mon + 1 : 1;
  int day = tm_ptr ? tm_ptr->tm_mday : 1;
  if (year < 1980) {
    year = 1980;
    month = 1;
    day = 1;
  }
  return (uint16_t)((((year - 1980) & 0x7F) << 9) |
                    ((month & 0x0F) << 5) |
                    (day & 0x1F));
}

uint8_t dos_find_first_impl(const char *path, uint16_t attr, const char *file,
                            const char *func, int line) {
  shim_log(__func__, file, func, line, path);
  if (!dta_ptr) {
    CF = 1;
    ax = 0x1F; /* invalid parameter */
    return 1;
  }

  char buf[PATH_MAX];
  size_t i = 0;
  if (path) {
    while (i < sizeof(buf) - 1 && path[i] != '\0' && path[i] != '\r') {
      char current = path[i];
      if (current == '\\') {
        current = '/';
      }
      buf[i++] = current;
    }
  }
  buf[i] = '\0';

  char *filespec = buf;
  char *colon = strchr(filespec, ':');
  if (colon) {
    filespec = colon + 1;
  }

  while (*filespec == '/') {
    ++filespec;
  }

  if (*filespec == '\0') {
    CF = 1;
    ax = 0x03; /* path not found */
    return 1;
  }

  char *pattern = filespec;
  const char *dirpath = ".";
  char *last_sep = strrchr(filespec, '/');
  if (last_sep) {
    *last_sep = '\0';
    pattern = last_sep + 1;
    dirpath = (*filespec != '\0') ? filespec : "/";
  }

  if (*pattern == '\0') {
    pattern = "*.*";
  }

  DIR *dir = opendir(dirpath);
  if (!dir) {
    int err = errno;
    CF = 1;
    ax = (err == ENOENT || err == ENOTDIR) ? 0x03 : 0x05; /* access denied */
    return 1;
  }

  typedef struct __attribute__((packed)) {
    uint8_t reserved[21];
    uint8_t attr;
    uint16_t time;
    uint16_t date;
    uint32_t size;
    char name[13];
  } FindFirstBlock;

  FindFirstBlock *dta = (FindFirstBlock *)dta_ptr;

  int fnm_flags = 0;
#ifdef FNM_CASEFOLD
  fnm_flags |= FNM_CASEFOLD;
#endif

  struct dirent *ent;
  int found = 0;
  while ((ent = readdir(dir)) != NULL) {
    const char *name = ent->d_name;
    if (strcmp(name, ".") == 0 || strcmp(name, "..") == 0) {
      continue;
    }
    if (fnmatch(pattern, name, fnm_flags) != 0) {
      continue;
    }

    char full_path[PATH_MAX];
    if (strcmp(dirpath, ".") == 0) {
      snprintf(full_path, sizeof(full_path), "%s", name);
    } else if (strcmp(dirpath, "/") == 0) {
      snprintf(full_path, sizeof(full_path), "/%s", name);
    } else {
      size_t len = strlen(dirpath);
      const char *fmt = (len > 0 && dirpath[len - 1] == '/') ? "%s%s" : "%s/%s";
      snprintf(full_path, sizeof(full_path), fmt, dirpath, name);
    }

    struct stat st;
    if (stat(full_path, &st) != 0) {
      continue;
    }

    uint8_t file_attr = 0;
    if (S_ISDIR(st.st_mode)) {
      file_attr |= 0x10;
    } else if (S_ISREG(st.st_mode)) {
      file_attr |= 0x20;
    }
    if (access(full_path, W_OK) != 0) {
      file_attr |= 0x01;
    }
    if (name[0] == '.') {
      file_attr |= 0x02;
    }

    if ((file_attr & 0x02) && !(attr & 0x02)) {
      continue;
    }
    if ((file_attr & 0x04) && !(attr & 0x04)) {
      continue;
    }
    if ((file_attr & 0x10) && !(attr & 0x10)) {
      continue;
    }
    if ((file_attr & 0x08) && !(attr & 0x08)) {
      continue;
    }

    memset(dta, 0, sizeof(*dta));
    dta->reserved[0] = 0; /* default drive */
    format_search_template(&dta->reserved[1], pattern);
    dta->reserved[12] = (uint8_t)attr;
    dta->attr = file_attr;

    time_t mtime = st.st_mtime;
    struct tm tm_buf;
    struct tm *tm_ptr = localtime(&mtime);
    if (tm_ptr) {
      tm_buf = *tm_ptr;
      tm_ptr = &tm_buf;
    } else {
      memset(&tm_buf, 0, sizeof(tm_buf));
      tm_buf.tm_year = 80;
      tm_buf.tm_mon = 0;
      tm_buf.tm_mday = 1;
      tm_ptr = &tm_buf;
    }
    dta->time = dos_encode_time(tm_ptr);
    dta->date = dos_encode_date(tm_ptr);
    dta->size = S_ISDIR(st.st_mode) ? 0 : (uint32_t)st.st_size;
    format_dos_filename(dta->name, name);

    found = 1;
    break;
  }

  closedir(dir);

  if (!found) {
    CF = 1;
    ax = 0x02; /* file not found */
    return 1;
  }

  CF = 0;
  ax = 0;
  return 0;
}

uint8_t dos_find_first(const char *path, uint16_t attr) {
  return dos_find_first_impl(path, attr, "<external>", __func__, 0);
}

/* Segment of the most recent dos_alloc_mem block. dos_resize_mem uses it to
 * tell which block currently owns the top of the bump arena (so a grow/shrink
 * of that block can move next_free_seg). */
static uint16_t dos_last_alloc_seg;

uint8_t dos_alloc_mem_impl(uint16_t parags, const char *file, const char *func,
                           int line) {
  shim_log(__func__, file, func, line, NULL);
  /* Faithful DOS AH=48h: blocks come out of conventional RAM, which ends at
   * segment 0xA000 (640 KB) -- everything above is video/UMB/ROM and is not
   * DOS-allocatable. Compute the request in paragraphs (32-bit, so a 1 MB
   * BX=0xFFFF probe can't wrap next_free_seg) and fail when it would cross the
   * ceiling. On failure DOS returns CF=1, AX=8 (insufficient memory) and
   * BX=largest free block (paragraphs) -- which is exactly how a program's
   * "how much memory is free" idiom (request 0xFFFF, read the returned BX)
   * reads available conventional RAM. */
  uint32_t end_seg = (uint32_t)next_free_seg + (uint32_t)parags;
  if (end_seg > CONVENTIONAL_TOP_SEG) {
    CF = 1;
    ax = 8;
    uint32_t avail = (next_free_seg < CONVENTIONAL_TOP_SEG)
                         ? (CONVENTIONAL_TOP_SEG - next_free_seg)
                         : 0;
    bx = (uint16_t)avail;
    return 1;
  }
  ax = next_free_seg;
  dos_last_alloc_seg = next_free_seg;
  next_free_seg = (uint16_t)end_seg;
  CF = 0;
  return 0;
}

uint8_t dos_alloc_mem(uint16_t parags) {
  CRITICAL_ENTER();
  uint8_t result = dos_alloc_mem_impl(parags, "<external>", __func__, 0);
  CRITICAL_EXIT();
  return result;
}

uint8_t dos_free_mem_impl(void *ptr, const char *file, const char *func,
                          int line) {
  shim_log(__func__, file, func, line, NULL);
  (void)ptr;
  CF = 0;
  return 0;
}

uint8_t dos_free_mem(void *ptr) {
  return dos_free_mem_impl(ptr, "<external>", __func__, 0);
}

uint8_t dos_resize_mem_impl(uint16_t segment, uint16_t parags,
                            const char *file, const char *func, int line) {
  shim_log(__func__, file, func, line, NULL);
  shim_log_stdout(
      "Trace: dos_resize_mem: segment=0x%04X parags=0x%04X (min=0x%04X)\n",
      segment, parags, program_min_block_paras);
  /* Resize (AH=4Ah) can only grow a block up to the conventional ceiling
   * (0xA000); the host arena above that is not DOS-allocatable RAM. */
  uint32_t max_paras = CONVENTIONAL_TOP_SEG;
  /* A valid owned block is the program block (PSP_SEG) or any block previously
   * handed out by dos_alloc_mem -- the bump allocator places those in
   * [PSP_SEG, next_free_seg). Real DOS (INT 21h AH=4Ah) resizes ANY owned
   * block, not just the program block; rejecting allocated blocks with AX=9
   * here made games that "allocate then grow/shrink to fit" (some programs'
   * startup heap setup) mis-track their pool and miscompute free memory. */
  if (segment < PSP_SEG || (uint32_t)segment >= next_free_seg) {
    CF = 1;
    ax = 9; /* invalid memory block */
    bx = 0;
    return 1;
  }
  if ((uint32_t)segment >= max_paras) {
    CF = 1;
    ax = 8;
    bx = 0;
    return 1;
  }
  uint32_t requested_top = (uint32_t)segment + (uint32_t)parags;
  if (requested_top > max_paras) {
    CF = 1;
    ax = 8; /* insufficient memory */
    uint32_t avail = max_paras - segment;
    if (avail > 0xFFFF) {
      avail = 0xFFFF;
    }
    bx = (uint16_t)avail;
    return 1;
  }
  if (segment == PSP_SEG) {
    /* The program block has a minimum size (PSP + resident image) and anchors
     * the bottom of the bump arena, so its resize sets where the free area
     * begins -- exactly the original behaviour. */
    /* Real DOS AH=4Ah shrinks the program block to whatever the program asks
     * for -- it owns the block and knows what it still needs; there is no
     * "minimum loaded-image" floor. Our old floor (program_min_block_paras)
     * was derived from the COMPRESSED on-disk paragraph count of an LZEXE
     * program, so it wrongly rejected a packed program shrinking to its real
     * (smaller) decompressed size. That left the block huge, so an EXEC'd child
     * had nowhere below the VGA ceiling to load and clobbered video memory.
     * Floor only at the PSP itself. */
    uint16_t min_required = 0x10; /* the PSP itself (LOAD_SEG = PSP_SEG + 0x10) */
    if (parags < min_required) {
      CF = 1;
      ax = 8; /* insufficient memory for requested block */
      bx = min_required;
      return 1;
    }
    program_min_block_paras = (uint16_t)parags;
    next_free_seg = (uint16_t)requested_top;
    CF = 0;
    ax = 0;
    return 0;
  }
  /* Resize of an allocated block: only the block that currently owns the arena
   * top (the most recently allocated one) moves the bump pointer; resizing an
   * interior block succeeds but cannot reclaim its hole (no free list -- matches
   * a DOS heap that does not coalesce an interior block). */
  if (segment == dos_last_alloc_seg) {
    next_free_seg = (uint16_t)requested_top;
  }
  CF = 0;
  ax = 0;
  return 0;
}

uint8_t dos_resize_mem(uint16_t segment, uint16_t parags) {
  return dos_resize_mem_impl(segment, parags, "<external>", __func__, 0);
}

/* AH=3Ch Create File (DS:DX -> path, CX = attributes). Creates/truncates the
 * file and returns a handle in AX (mirrored in BX), CF clear. Mirrors
 * dos_open_file's handle-table allocation. */
uint8_t dos_create_file_impl(const char *path, uint16_t attr, const char *file,
                             const char *func, int line) {
  shim_log(__func__, file, func, line, path);
  (void)attr;
  if (!path || path[0] == '\0' || path[0] == '\r') {
    CF = 1;
    ax = 3; /* path not found */
    return 1;
  }
  char buf[260];
  size_t i = 0;
  const char *src_path = dos_strip_drive_prefix(path);
  while (i < sizeof(buf) - 1 && src_path[i] != '\0' && src_path[i] != '\r') {
    buf[i] = src_path[i];
    ++i;
  }
  buf[i] = '\0';
  if (buf[0] == '\0') {
    CF = 1;
    ax = 3; /* path not found (drive-only spec) */
    return 1;
  }
  for (int h = 0; h < MAX_DOS_HANDLES; ++h) {
    if (handles[h] == NULL) {
      handles[h] = fopen_case_insensitive(buf, "wb+");
      if (handles[h] == NULL) {
        shim_log_stdout("Error: dos_create_file: %s: %s\n", buf,
                        strerror(errno));
        CF = 1;
        ax = 5; /* access denied */
        return 1;
      }
      handle_paths[h] = strdup(buf);
      handle_paths_owned[h] = true;
      ax = (uint16_t)h;
      bx = (uint16_t)h;
      CF = 0;
      return 0;
    }
  }
  shim_log_stdout("Error: dos_create_file: no handles available for %s\n", buf);
  CF = 1;
  ax = 4; /* too many open files */
  return 1;
}

uint8_t dos_create_file(const char *path, uint16_t attr) {
  return dos_create_file_impl(path, attr, "<external>", __func__, 0);
}

/* AH=2Fh Get DTA (ES:BX <- current Disk Transfer Area). Convert the stored
 * host pointer back to the seg:off the caller expects. */
uint8_t dos_get_dta_impl(const char *file, const char *func, int line) {
  shim_log(__func__, file, func, line, NULL);
  const uint8_t *p = (const uint8_t *)dta_ptr;
  if (p >= virtual_memory && p < virtual_memory + MEMORY_SIZE) {
    uint32_t a = (uint32_t)(p - virtual_memory);
    es = (uint16_t)(a >> 4);
    bx = (uint16_t)(a & 0xF);
  } else {
    es = 0;
    bx = 0;
  }
  CF = 0;
  return 0;
}

uint8_t dos_get_dta(void) {
  return dos_get_dta_impl("<external>", __func__, 0);
}

/* AH=4Fh Find Next. Continues a Find First search; find_first currently
 * matches nothing, so report "no more files" (CF set, AX=0x12). */
uint8_t dos_find_next_impl(const char *file, const char *func, int line) {
  shim_log(__func__, file, func, line, NULL);
  CF = 1;
  ax = 0x12; /* no more files */
  return 1;
}

uint8_t dos_find_next(void) {
  return dos_find_next_impl("<external>", __func__, 0);
}

/* AH=0Ch Flush keyboard buffer then invoke input function AL. The flush is a
 * no-op here; report success with no input processed. */
uint8_t dos_flush_and_read_impl(uint8_t subfunc, const char *file,
                                const char *func, int line) {
  shim_log(__func__, file, func, line, NULL);
  (void)subfunc;
  CF = 0;
  return 0;
}

uint8_t dos_flush_and_read(uint8_t subfunc) {
  return dos_flush_and_read_impl(subfunc, "<external>", __func__, 0);
}

uint8_t dos_exec_impl(void *param_block, const char *cmd, const char *file,
                      const char *func, int line) {
  shim_log(__func__, file, func, line, cmd);
  /* DOS loads an EXEC'd child into the FREE memory ABOVE the parent's block --
   * NOT over the parent. The parent shrinks its block first (INT 21h AH=4Ah),
   * which sets next_free_seg to the top of its block; that paragraph is where
   * the child's PSP goes (image at +0x10). Loading at the fixed LOAD_SEG
   * instead let a child (e.g. an intro overlay) overwrite the
   * parent's resident code/data -- corrupting driver objects the parent uses
   * after the child returns. Give the child its own PSP (copy the parent's so
   * its env segment / top-of-memory stay valid) above the parent. */
  uint16_t parent_next_free = next_free_seg;
  uint16_t child_psp = next_free_seg;
  uint16_t child_load = (uint16_t)(child_psp + 0x10);
  memcpy(virtual_memory + ((uint32_t)child_psp << 4),
         virtual_memory + ((uint32_t)PSP_SEG << 4), 0x100);
  /* Copy the AH=4Bh parameter block's COMMAND TAIL into the child PSP at 0x80
   * (a length byte, the CR-terminated argument string). DOS passes the child's
   * arguments this way -- offset +2 of the param block is a far pointer to the
   * tail. Ignoring it left the child reading the PARENT's copied tail, so a
   * launcher that EXECs a worker WITH a command line had its arguments silently
   * dropped and the worker ran its no-argument default. (One program's
   * launcher EXECs a worker with "@1" to start play; with the tail dropped,
   * the worker saw no command and re-ran the attract -- the title->attract loop.) */
  if (param_block) {
    const uint8_t *pb = (const uint8_t *)param_block;
    uint16_t tail_off = (uint16_t)(pb[2] | (pb[3] << 8));
    uint16_t tail_seg = (uint16_t)(pb[4] | (pb[5] << 8));
    uint32_t tail_lin = (((uint32_t)tail_seg << 4) + tail_off) & 0xFFFFF;
    uint8_t len = virtual_memory[tail_lin]; /* char count, excl. len byte + CR */
    if (len > 0x7E) len = 0x7E;             /* PSP tail area is 0x80..0xFF */
    uint32_t dst = ((uint32_t)child_psp << 4) + 0x80;
    virtual_memory[dst] = len;
    for (uint16_t i = 0; i < len; ++i)
      virtual_memory[(dst + 1 + i) & 0xFFFFF] =
          virtual_memory[(tail_lin + 1 + i) & 0xFFFFF];
    virtual_memory[(dst + 1 + len) & 0xFFFFF] = 0x0D; /* CR terminator */
  }
  uint16_t new_cs, new_ip, new_ss, new_sp;
  if (load_executable(cmd, child_load, 1, &new_cs, &new_ip, &new_ss, &new_sp) !=
      0) {
    CF = 1;
    ax = 2;
    return 1;
  }

  /* EXEC runs the child to completion and returns here (its parent's INT 21h);
   * shim_exec_run_child saves/restores our registers around the nested run. A
   * plain long_jump would only set the child's cs:ip while this chunk kept
   * executing the parent's code. */
  shim_exec_run_child(new_cs, new_ip, new_ss, new_sp, child_psp);
  /* DOS frees the child's block when it exits: reclaim the arena so the next
   * EXEC/alloc reuses the space instead of climbing toward the 0xA000 ceiling
   * (which would eventually load a child into video memory). */
  next_free_seg = parent_next_free;
  CF = 0;
  return 0;
}

uint8_t dos_exec(void *param_block, const char *cmd) {
  return dos_exec_impl(param_block, cmd, "<external>", __func__, 0);
}

/* INT 21h AH=4Dh (Get Return Code of Subprocess) reads the code of the most
 * recently terminated child here: high byte = termination type (0 normal exit,
 * 1 Ctrl-C, 2 critical error, 3 TSR), low byte = the AH=4Ch exit code. DOS
 * returns it ONCE then clears it, so two consecutive AH=4Dh reads differ. */
static uint16_t dos_child_return_code = 0;

void dos_exit_impl(const char *file, const char *func, int line) {
  shim_log(__func__, file, func, line, NULL);
  for (int i = 0; i < 16; ++i) {
    uint8_t cur = virtual_memory[i];
    if (cur != null_guard_initial[i]) {
      shim_log_stdout(
          "Warning: null guard changed at 0x%02X initial=0x%02X current=0x%02X\n",
          i, null_guard_initial[i], cur);
    }
  }
  /* DOS function 0x4C places the exit code in AL.  Propagate this value so
   * callers can observe meaningful failure codes instead of always seeing 0.
   * When the program terminates via an older INT 0x20 style call the AL
   * register is typically zero, which still results in a successful exit. */
  int status = (int)(uint8_t)al;

  /* Record it for a later AH=4Dh from the parent (normal termination => AH=0). */
  dos_child_return_code = (uint16_t)(uint8_t)status;

  /* If this program was EXEC'd by a parent (INT 21h AH=4Bh), its termination
   * returns to the parent rather than ending the process. shim_exec_child_
   * terminate longjmps back into shim_exec_run_child; it returns 0 only at the
   * top level, where we fall through to the real process exit below. */
  if (shim_exec_child_terminate(status) != 0) {
    return; /* not reached: longjmp'd to the parent's EXEC */
  }

  /* Visible exit marker so a clean DOS termination doesn't look like a
   * silent disappearance. The game itself called INT 21h AH=4C (or INT
   * 20h) — usually from a "Quit to DOS" menu action or a fatal-error
   * handler that issued a clean exit instead of crashing. */
  fprintf(stderr,
          "\n[EXIT] DOS terminate (INT 21h AH=4Ch / INT 20h) called from "
          "cs:ip=%04X:%04X active_binary=%s exit_status=%d\n"
          "[EXIT]   triggering C site: %s:%s:%d\n"
          "[EXIT]   this is a CLEAN game-initiated exit, not a crash. "
          "No bundle written.\n",
          cs, ip, shim_active_binary() ? shim_active_binary() : "<none>",
          status, file ? file : "?", func ? func : "?", line);
  extern void save_manager_sr_log(const char *fmt, ...);
  save_manager_sr_log("exit DOS_TERMINATE cs:ip=%04X:%04X active=%s "
                      "status=%d from %s:%s:%d (clean exit, not a crash)",
                      cs, ip,
                      shim_active_binary() ? shim_active_binary() : "<none>",
                      status, file ? file : "?", func ? func : "?", line);

  machine_halted = 1;
  fflush(stdout);
  fflush(stderr);
  exit(status);
}

void dos_exit(void) { dos_exit_impl("<external>", __func__, 0); }

/* INT 21h AH=4Dh: Get Return Code of Subprocess. Returns AX = (termination
 * type << 8) | exit code of the last terminated child, then clears it so a
 * second call reads 0 -- the documented DOS "read once" behaviour. */
uint8_t dos_get_return_code_impl(const char *file, const char *func, int line) {
  shim_log(__func__, file, func, line, NULL);
  ax = dos_child_return_code;
  dos_child_return_code = 0;
  return 0; /* CF clear */
}

/* DOS API AH=0x10: close file using an FCB */
uint8_t dos_close_fcb_impl(uint16_t dx_val, const char *file, const char *func,
                           int line) {
  shim_log(__func__, file, func, line, NULL);
  (void)dx_val; /* DS:DX -> FCB */
  return 0;
}

uint8_t dos_close_fcb(uint16_t dx_val) {
  return dos_close_fcb_impl(dx_val, "<external>", __func__, 0);
}

/* Characters that terminate a filename/extension component when parsing into
 * an FCB (the DOS "filename separators"), plus any control char. '.' (the
 * extension separator) and ':' (the drive separator) are in the set too; the
 * parser consumes those explicitly where they're meaningful. */
static int dos_fcb_is_separator(uint8_t c) {
  if (c < 0x20) return 1;
  switch (c) {
  case ' ': case '.': case ':': case ';': case ',': case '=': case '+':
  case '/': case '\\': case '"': case '[': case ']': case '|':
  case '<': case '>': case 0x7F:
    return 1;
  default:
    return 0;
  }
}

/* INT 21h AH=29h: Parse a filename at DS:SI into the FCB at ES:DI.
 *   AL bit0 = scan off leading separators; bit1/2/3 = only set the FCB
 *   drive/name/extension field when one is actually present in the string.
 * Returns AL=0 (no wildcard) or 1 (the name held '*'/'?'); DS:SI is advanced
 * past the parsed name, and ES:DI holds drive(1) + 8-char name + 3-char ext,
 * blank-padded and upper-cased. */
uint8_t dos_parse_filename_impl(const char *file, const char *func, int line) {
  shim_log(__func__, file, func, line, NULL);
  uint8_t ctrl = al;
  const uint8_t *s = (const uint8_t *)seg_off(ds, si);
  uint8_t *fcb = (uint8_t *)seg_off(es, di);
  uint16_t pos = 0;
  int wildcard = 0;

  if (ctrl & 0x01) {
    while (s[pos] == ' ' || s[pos] == '\t') pos++;
  }

  /* Optional "X:" drive. */
  uint8_t drive = 0; /* 0 = default/current */
  int drive_present = 0;
  if (s[pos] != '\0' && s[pos + 1] == ':') {
    uint8_t c = s[pos];
    if (c >= 'a' && c <= 'z') c = (uint8_t)(c - 0x20);
    if (c >= 'A' && c <= 'Z') {
      drive = (uint8_t)(c - 'A' + 1);
      drive_present = 1;
      pos += 2;
    }
  }
  if (drive_present || !(ctrl & 0x02)) {
    fcb[0] = drive;
  }

  uint8_t name[8], ext[3];
  for (int i = 0; i < 8; i++) name[i] = ' ';
  for (int i = 0; i < 3; i++) ext[i] = ' ';

  int fi = 0, name_present = 0;
  while (s[pos] != '\0' && !dos_fcb_is_separator(s[pos])) {
    uint8_t c = s[pos];
    name_present = 1;
    if (c == '*') { while (fi < 8) name[fi++] = '?'; wildcard = 1; pos++; break; }
    if (c == '?') wildcard = 1;
    if (c >= 'a' && c <= 'z') c = (uint8_t)(c - 0x20);
    if (fi < 8) name[fi++] = c;
    pos++;
  }
  while (s[pos] != '\0' && s[pos] != '.' && !dos_fcb_is_separator(s[pos])) pos++;

  int ext_present = 0;
  if (s[pos] == '.') {
    pos++;
    int ei = 0;
    while (s[pos] != '\0' && !dos_fcb_is_separator(s[pos])) {
      uint8_t c = s[pos];
      ext_present = 1;
      if (c == '*') { while (ei < 3) ext[ei++] = '?'; wildcard = 1; pos++; break; }
      if (c == '?') wildcard = 1;
      if (c >= 'a' && c <= 'z') c = (uint8_t)(c - 0x20);
      if (ei < 3) ext[ei++] = c;
      pos++;
    }
    while (s[pos] != '\0' && !dos_fcb_is_separator(s[pos])) pos++;
  }

  if (name_present || !(ctrl & 0x04)) {
    for (int i = 0; i < 8; i++) fcb[1 + i] = name[i];
  }
  if (ext_present || !(ctrl & 0x08)) {
    for (int i = 0; i < 3; i++) fcb[9 + i] = ext[i];
  }

  si = (uint16_t)(si + pos);
  al = (uint8_t)(wildcard ? 1 : 0);
  return 0;
}

uint8_t dos_parse_filename(void) {
  return dos_parse_filename_impl("<external>", __func__, 0);
}

uint8_t dos_api_impl(const char *file, const char *func, int line) {
  CRITICAL_ENTER();
  shim_log(__func__, file, func, line, NULL);
  uint8_t result = 0;
  switch (ah) {
  case 0x09: /* Display string */
    result = dos_print_string_impl((const char *)seg_off(ds, dx), file, func,
                                   line);
    CF = result;
    break;
  case 0x25: /* Set interrupt vector */
    result = dos_set_interrupt_vector_impl(al, ds, dx, file, func, line);
    CF = result;
    break;
  case 0x29: /* Parse filename (DS:SI) into FCB (ES:DI) */
    result = dos_parse_filename_impl(file, func, line);
    break;
  case 0x2C: { /* Get time */
    struct timeval tv;
    gettimeofday(&tv, NULL);
    struct tm *tm = localtime(&tv.tv_sec);
    ch = (uint8_t)tm->tm_hour;
    cl = (uint8_t)tm->tm_min;
    dh = (uint8_t)tm->tm_sec;
    dl = (uint8_t)(tv.tv_usec / 10000);
    CF = 0;
    break;
  }
  case 0x30: /* Get DOS version */
    ax = 0x0003; /* DOS 3.00 (AL=major, AH=minor) */
    bh = 0x00;   /* OEM number (IBM/MS-DOS) */
    bl = 0x00;   /* Version flags */
    cx = 0;
    CF = 0;
    break;
  case 0x50: /* Set current PSP (BX = PSP segment) */
    dos_current_psp = bx;
    CF = 0;
    break;
  case 0x51: /* Get current PSP -> BX */
  case 0x62: /* Get PSP address (DOS 3.0+) -> BX */
    bx = dos_current_psp;
    CF = 0;
    break;
  /* The remaining services are also reachable here via a runtime-dynamic AH
   * (the JIT decodes code whose AH isn't a translate-time constant, so it can't
   * be routed to the named helper statically). Route them to the same _impl
   * functions the static path uses, with the same register-derived arguments. */
  case 0x01: /* Read char (echo) -> AL */
    CF = dos_read_char_impl(file, func, line);
    break;
  case 0x02: /* Write char (DL) */
    CF = dos_write_char_impl(dl, file, func, line);
    break;
  case 0x06: /* Direct console I/O (DL) */
    CF = dos_direct_console_io_impl(dl, file, func, line);
    break;
  case 0x07: /* Direct console input, no echo -> AL */
    CF = dos_console_input_no_echo_impl(file, func, line);
    break;
  case 0x0A: /* Buffered keyboard input into DS:DX */
    CF = dos_buffered_input_impl(file, func, line);
    break;
  case 0x0B: /* Check keyboard status -> AL */
    CF = dos_check_keyboard_status_impl(file, func, line);
    break;
  case 0x0C: /* Flush buffer + read (AL = function) */
    CF = dos_flush_and_read_impl(al, file, func, line);
    break;
  case 0x0D: /* Reset disk */
    CF = dos_reset_disk_impl(file, func, line);
    break;
  case 0x0E: /* Select drive (DL) */
    CF = dos_select_drive_impl(dl, file, func, line);
    break;
  case 0x10: /* Close FCB (DS:DX) */
    CF = dos_close_fcb_impl(dx, file, func, line);
    break;
  case 0x19: /* Get current drive -> AL */
    CF = dos_get_current_drive_impl(file, func, line);
    break;
  case 0x1A: /* Set DTA (DS:DX) */
    CF = dos_set_dta_impl(seg_off(ds, dx), file, func, line);
    break;
  case 0x2A: /* Get date */
    CF = dos_get_date_impl(file, func, line);
    break;
  case 0x2F: /* Get DTA -> ES:BX */
    CF = dos_get_dta_impl(file, func, line);
    break;
  case 0x35: /* Get interrupt vector (AL) -> ES:BX */
    CF = dos_get_interrupt_vector_impl(file, func, line);
    break;
  case 0x36: /* Get disk free space (DL) */
    CF = dos_get_disk_free_space_impl(dl, file, func, line);
    break;
  case 0x39: /* Make dir (DS:DX) */
    CF = dos_make_dir_impl((const char *)seg_off(ds, dx), file, func, line);
    break;
  case 0x3B: /* Change dir (DS:DX) */
    CF = dos_change_dir_impl((const char *)seg_off(ds, dx), file, func, line);
    break;
  case 0x3C: /* Create file (DS:DX, CX=attr) */
    CF = dos_create_file_impl((const char *)seg_off(ds, dx), cx, file, func,
                              line);
    break;
  case 0x41: /* Delete file (DS:DX) */
    CF = dos_delete_file_impl((const char *)seg_off(ds, dx), file, func, line);
    break;
  case 0x43: /* Get/set file attributes (DS:DX path, CX attr) */
    CF = dos_get_file_attributes_impl((const char *)seg_off(ds, dx), &cx, file,
                                      func, line);
    break;
  case 0x44: /* IOCTL (AL = subfunction) */
    CF = dos_ioctl_impl(al, bx, file, func, line);
    break;
  case 0x47: /* Get current dir (DL drive, DS:SI buffer) */
    CF = dos_get_current_dir_impl((char *)seg_off(ds, si), file, func, line);
    break;
  case 0x48: /* Allocate memory (BX paragraphs) */
    CF = dos_alloc_mem_impl(bx, file, func, line);
    break;
  case 0x49: /* Free memory (ES) */
    CF = dos_free_mem_impl((void *)seg_off(es, 0), file, func, line);
    break;
  case 0x4B: /* Exec / load (DS:DX path, ES:BX param block) */
    if (al == 0x03) {
      /* AL=03h Load Overlay: param block is {word load_seg, word reloc_factor};
       * load the image there and return -- the caller far-calls it itself. */
      const uint16_t *pb = (const uint16_t *)seg_off(es, bx);
      result = load_overlay((const char *)seg_off(ds, dx), pb[0], pb[1]);
      CF = result;
      if (result == 0) {
        ax = 0;
      }
    } else {
      /* AL=00h load+execute (AL=01h load, no execute, is treated the same). */
      CF = dos_exec_impl((void *)seg_off(es, bx),
                         (const char *)seg_off(ds, dx), file, func, line);
    }
    break;
  case 0x4E: /* Find first (DS:DX, CX=attr) */
    CF = dos_find_first_impl((const char *)seg_off(ds, dx), cx, file, func,
                             line);
    break;
  case 0x4F: /* Find next */
    CF = dos_find_next_impl(file, func, line);
    break;
  case 0x3D: /* Open file */
    result = dos_open_file_impl((const char *)seg_off(ds, dx), file, func,
                                line);
    CF = result;
    break;
  case 0x3E: /* Close file */
    result = dos_close_file_impl(bx, file, func, line);
    CF = result;
    break;
  case 0x3F: /* Read file */
    result = dos_read_file_impl(bx, seg_off(ds, dx), cx, file, func, line);
    CF = result;
    break;
  case 0x40: /* Write file */
    result = dos_write_file_impl(bx, seg_off(ds, dx), cx, file, func, line);
    CF = result;
    break;
  case 0x42: /* LSeek */
    result = dos_lseek_impl(bx, cx, dx, al, file, func, line);
    CF = result;
    break;
  case 0x4A: /* Resize memory block */
    result = dos_resize_mem_impl(es, bx, file, func, line);
    CF = result;
    break;
  case 0x4C: /* Exit */
    CRITICAL_EXIT();
    dos_exit_impl(file, func, line);
    return 0; /* dos_exit_impl does not return */
  default: {
    /* Hard-crash on an unimplemented service so the gap is loud and leaves a
     * crash bundle to inspect (CPU state + which AH), rather than a quiet exit
     * that's easy to miss. */
    char msg[256];
    snprintf(msg, sizeof(msg),
             "unimplemented DOS function AH=0x%02X (%s:%s:%d)", ah, file, func,
             line);
    shim_log_crash("%s\n", msg);
    shim_save_bug_bundle("unimplemented_dos", ((uint32_t)cs << 4) + ip, msg);
    abort();
  }
  }
  CRITICAL_EXIT();
  return result;
}

uint8_t dos_api(void) { return dos_api_impl("<external>", __func__, 0); }

/*
 * Emulate BIOS video mode switch (INT 0x10, AH=0x00).
 * Only logs the requested mode and does not perform a real VGA mode change.
 * Mimics the BIOS side effects by updating CPU registers and tracking the
 * requested mode.
 */

