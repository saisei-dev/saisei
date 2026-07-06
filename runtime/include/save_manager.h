#ifndef SAVE_MANAGER_H
#define SAVE_MANAGER_H

#include <stddef.h>

/* Gameplay save-state ring. Peer to snapshot.c but distinct purpose:
 * snapshot.c writes crash investigation artifacts (pre_last_key snapshots,
 * keys.log, atexit dumps); save_manager writes resume-in-place gameplay
 * saves that --restore-from can resume in place.
 *
 * Layout:
 *   <runtime_dir>/saves/slot_0/   newest, target age ~5s
 *                 slot_1/         ~15s
 *                 slot_2/         ~45s
 *                 slot_3/         ~135s
 *                 slot_4/         ~180s  (3-minute cap)
 * Each slot contains: pre_last_key.{bin,state.bin,maps.tsv} + meta.json.
 * No keys.log — gameplay saves are for resume, not replay.
 *
 * Writes are skipped when lcall_depth != 0 || isr_depth != 0 (the only
 * states snapshot_restore_and_resume can safely re-enter), and throttled
 * to >= 5s between successful writes.
 *
 * Zero coupling to shims.c: driven from virtual_display_sdl.c's poll loop
 * (time) and snapshot.c::snapshot_on_key_consumed (key). All shim state
 * is read via the accessors declared in shims.h. */

void save_manager_init(void);

/* Wall-clock periodic trigger. Safe to call at any rate; internally
 * throttled. Call from the SDL poll loop. */
void save_manager_tick(void);

/* Key-consumed trigger. Call from snapshot_on_key_consumed after the
 * existing snapshot capture work has run. */
void save_manager_on_key_consumed(void);

/* Manual user-driven save trigger (Cmd+F1). Queues a save request and
 * returns immediately. The request is serviced from save_manager_poll_pending
 * at the next safe SAFEPOINT *after* the game has consumed a keyboard
 * event (BIOS INT 16h or port 0x60 ISR read). Without that anchor, the
 * save could land mid-function-call with transient register state that
 * the dispatch entry can't reproduce on restore. So after Cmd+F1, the
 * user typically also needs to press a regular game key for the save to
 * actually land. Bypasses the 5s throttle. */
void save_manager_request_save(void);

/* Manual user-driven restore trigger (Cmd+F2). Finds the freshest valid
 * slot (slot_0 first, falling back to higher-numbered slots) and re-execs
 * the current binary with --restore-from <slot_dir>. Does NOT return on
 * success. Logs and returns if no valid slot exists. */
void save_manager_request_load_latest(void);

/* Drive any queued user-driven save request. Cheap no-op when nothing
 * is pending. Call from the SDL poll loop (which itself fires from
 * SAFEPOINTs in the translated game code). */
void save_manager_poll_pending(void);

/* Append a human-readable line to <runtime_dir>/save_restore.log with
 * an ISO timestamp prefix. Used by save_manager and snapshot.c so
 * save/restore activity lives in one easy-to-read file. */
void save_manager_sr_log(const char *fmt, ...)
    __attribute__((format(printf, 1, 2)));

#endif /* SAVE_MANAGER_H */
