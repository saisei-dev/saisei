#ifndef SNAPSHOT_H
#define SNAPSHOT_H

#include <stddef.h>
#include <stdint.h>

/* SDL-tier "save state" layer that sits on top of the shims runtime.
 *
 * Concerns owned by this module:
 *   - key event log (every press/release + hold duration → keys.log)
 *   - pre-key memory + CPU snapshot (→ pre_last_key.{bin,state.bin,json,maps.tsv})
 *   - atexit dump of the latest snapshot into last_exit_snapshot/
 *   - --restore-from <dir>: load snapshot and resume dispatch
 *
 * The shims runtime invokes the on_* hooks at the appropriate moments
 * (keypress arrival, key consumption, crash bundle write) and the main entry
 * calls snapshot_init() once at startup. Otherwise shims has no knowledge of
 * this module. */

/* Register atexit handler that writes last_exit_snapshot/ on any clean
 * exit. Idempotent. */
void snapshot_init(void);

/* SDL/stdin → shim_keyboard_enqueue_scancode_press/_release invokes this
 * with action='P' or 'R' (per scancode). */
void snapshot_on_key_event(char action, uint8_t scancode);

/* shims invokes this when a make scancode is delivered to the game
 * (port 0x60 read, INT 16h pop). Snapshots memory + CPU + keyboard queue. */
void snapshot_on_key_consumed(void);

/* Append our files (keys.log, pre_last_key.*) to an existing crash bundle
 * directory. Called from the shim's save_crash_bundle path. */
void snapshot_write_to_bundle(const char *dir);

/* --restore-from <bundle> entry. Reads pre_last_key.* + maps.tsv, restores
 * cpu+memory+queue+file_mappings, then re-enters dispatch at saved cs:ip.
 * Returns 0 on success (dispatch returned), nonzero if restore failed. */
int snapshot_restore_and_resume(const char *bundle_dir);

/* Capture current CPU + memory + keyboard queue + file mappings and write
 * pre_last_key.{bin,state.bin,maps.tsv,json} into <dir>. Used by the
 * save_manager gameplay-save ring. Returns 0 on success, nonzero on I/O
 * failure. Caller is responsible for ensuring lcall_depth == 0 &&
 * isr_depth == 0; this function does not validate. */
int snapshot_capture_and_write(const char *dir);

#endif /* SNAPSHOT_H */
