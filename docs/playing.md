# Driving a program from a script

This doc covers the interface for **driving a bundled program programmatically** —
sending keystrokes, taking screenshots, and getting deterministic results across
runs. It's written for an agent or automation script that needs to navigate a
program without a human at the keyboard. For the human-facing build/run flow, see
`README.md`.

The mechanism is the same whether you run with or without the SDL window. The
two modes differ only in whether the program also accepts SDL keyboard events.

Throughout, `<name>` is the bundle name under `games/<name>/`; its build output
(and runtime working directory) is `build/<name>/`. Examples use
`python3 tools/game.py run <name>`; where the `saisei` wrapper is installed,
`saisei run <name>` is the equivalent shorthand.

## Starting the program with a control FIFO

Open a FIFO and redirect it to the program's stdin. The program inherits both
ends through the subshell so the writer never sees EOF when the controller exits
and re-opens the pipe:

```bash
rm -f /tmp/saisei_fifo
mkfifo /tmp/saisei_fifo
(exec 9<>/tmp/saisei_fifo;
 PYTHONPATH=$PWD python3 tools/game.py run <name> --headless --silent <&9 \
   > /tmp/saisei_stdout.log 2>&1) &
```

Flags worth knowing:

- `--headless` — no SDL window. Useful for batch runs / CI / parallel
  experiments. Without this flag the SDL window opens and SDL also delivers
  keystrokes alongside the FIFO.
- `--silent` — suppresses shim stdout logging. Stderr (including `[TAP]`
  trace lines, `[BUG]` reports, and crash banners) is unaffected.
- `--speedup N` — multiplies emulation speed (game logic runs N× as fast).

Run from the repo root so the pipeline can find the bundle under
`games/<name>/` and write runtime artifacts under `build/<name>/`.

## The stdin control protocol

All bytes you write to the FIFO are interpreted by the shim's stdin reader
(`runtime/core/shims.c`, the `safe_point_impl` keyboard block). The opcodes:

| Bytes | Meaning |
|-------|---------|
| `\x10 <sc>` | Press scancode `sc` (no auto-release). The program sees the key as held until you send the matching release. |
| `\x11 <sc>` | Release scancode `sc`. |
| `\x12 <sc> <ticks_lo> <ticks_hi>` | **Tap**: press + schedule release after exactly N BIOS IRQ0 ticks of game time. The deterministic-input primitive — see below. |
| `\x14` | Save a screenshot to `build/<name>/screenshots/screenshot<N>.png`. The counter is per-process. |
| `\r` | Press Enter (auto-paired make/break, like real BIOS keyboard input). |
| printable ASCII | Same as `\r` for the corresponding ASCII character. |
| escape sequences `\x1B[A`/B/C/D | Cursor up/down/right/left. |

Scancode quick reference (7-bit make codes):

| Key | Scancode (hex) |
|-----|----|
| Up | `48` |
| Down | `50` |
| Left | `4B` |
| Right | `4D` |
| Space | `39` |
| Enter | `1C` |
| Esc | `01` |

## Deterministic input: the tap opcode

Wall-clock holds (`\x10 4D`, sleep N seconds, `\x11 4D`) advance a *different
amount every run*. The emulator pauses and catches up depending on what the
program is doing during the sleep, and your driver's wall clock isn't the
program's clock. The tap opcode (`\x12`) fixes this by scheduling the release
inside the shim against a wall-clock deadline that's computed from a game-time
tick count:

- 1 BIOS tick = 54.925 ms (the 18.2 Hz timer) on the host clock.
- The deadline is divided by `--speedup`, so a 100-tick tap takes ~5.5 s of
  *game* time regardless of emulator speed: ~5.5 s wall-clock at `speedup=1`,
  ~2.75 s at `speedup=2`.
- The release fires from `pending_release_tick()`, called at the IRQ0 delivery
  site in `safe_point_impl`. (Earlier attempts paced this off raw PIT cycles
  and fired in 4.5 ms because PIT catchup batches collapse many "ticks" into
  one safepoint — don't repeat that mistake if you refactor.)

The control CLI wraps this as `tap` (see below). To hit it directly, write the
4 raw bytes atomically:

```bash
# press Right and release it after 100 BIOS ticks (~5.5 s of game time)
python3 -c "open('/tmp/saisei_fifo','wb',buffering=0).write(bytes([0x12,0x4D,100,0]))"
```

Each tap logs to stderr:

```
[TAP] sc=0x4D ticks=100 host_ns=109880237997260 deadline=109885730496990
[TAP] release sc=0x4D fired host_ns=109885730545593
```

The delta between deadline and release fire is normally microseconds.

## Screenshots

Auto-screenshots are off by default (`SCREENSHOT_INTERVAL_SECS` in
`runtime/core/shims.c` defaults to 0). Set `SAISEI_SCREENSHOT_SECS=N` in the
environment before launching to dump a PNG every N seconds — handy for
unattended headless validation. On demand, the triggers are:

1. `\x14` from stdin (Ctrl+T).
2. The SDL window's screenshot hotkey (windowed mode only).
3. Crash bundles (one shot per crash, written to the bundle dir).

Screenshots land in `build/<name>/screenshots/`. Listing that directory by
mtime (`ls -t`) gives you exactly the shots *you* took. To eliminate any
ambiguity across long sessions, clear the directory before each screenshot:

```bash
rm -f build/<name>/screenshots/*.png
printf '\x14' > /tmp/saisei_fifo
sleep 1.5  # let the IRQ0 path actually process the byte
ls build/<name>/screenshots/screenshot*.png | head -1
```

The screenshot counter is per-process and resets between launches — so don't
assume monotonically increasing numbers across crashes.

## Discovering a program's keys

Which keys do what is program-specific — the bundle's original documentation is
the authoritative source. When driving blind, mash Enter (`\r`) to advance
title/menu screens, then probe the arrow keys and Space with short taps,
screenshotting after each, to learn the mapping. For a held direction, note that
"aligned with an on-screen target" is often more precise than it looks: if an
action doesn't fire, nudge by a small tap (5–10 ticks) and retry.

## Crash bundles

When the program crashes (most commonly `unhandled_pc` or one of the
`*_unmapped` variants), the shim emits a `[BUG]` report to stderr and
writes a bundle under
`build/<name>/crashes/crash_<timestamp>_<reason>_<addr>/`. The bundle
contains `crash.txt`, `lifecycle.log` (a ring of CALL/JMP/LCALL/LJMP/NRET
events plus WATCHW lines), `file_mappings.json`, a full memory snapshot,
`screenshot.png`, and stack write history.

Because the translator emits a `case` for every basic-block boundary the
disassembler found, the dispatch case set is complete by construction. So
`unhandled_pc` landings mean **stack corruption upstream of the RET** (a
popped mid-instruction IP) or an unmapped chunk-swap target — investigation
usually starts with the lifecycle ring's last few NRET/CALL events.

`jump_table` / `lcall_table` / `long_jump` to unmapped targets usually
indicate **memory corruption** of an indirect pointer slot. See the WATCHW
section below.

## WATCHW: catching memory corruption

The shim has a per-address write watchlist (`write_watches[]` in `shims.c`).
When any code writes to a watched range, a `WATCHW` line lands in
`lifecycle.log` recording the writer's `cs:ip`, value, source location, and a
few registers. Each entry is a `{lo, hi, "name"}` linear-address range — for
example a function-pointer slot, a saved interrupt vector, or an indirect-call
jump table you suspect is being clobbered.

Two hookup points ensure writes via every path are caught:

1. `rcb_write8_impl` / `rcb_write16_impl` call `write_watch_log` after their
   normal trace — catches writes that come through the canonical segment
   shortcut.
2. `rep_movsb_block_impl` / `rep_movsw_block_impl` consult
   `rep_range_touches_watch()` and route through the per-byte/per-word
   memb/memw path when overlapping a watched range, so `rep movs` block
   copies are visible.

To add a new watch: append a `{lo, hi, "name"}` entry to `write_watches[]`.
Watches add a few comparisons per byte/word write — keep the list short
(<20 entries).

## Driver CLI: `tools/control.py`

Program-agnostic wrapper around the stdin protocol. Attaches to whatever
process owns the FIFO — it does **not** start, stop, or rebuild the
program, so the same CLI works with both foreground and background
launches.

```bash
# Launch the program (any way; here, background with FIFO-redirected stdin)
mkfifo /tmp/saisei_fifo
(exec 9<>/tmp/saisei_fifo;
 PYTHONPATH=$PWD python3 tools/game.py run <name> --headless --speedup 1.0 <&9 \
   > /tmp/saisei_stdout.log 2> /tmp/saisei_stderr.log) &

# Drive it
python3 tools/control.py shot                # capture frame; prints PNG path
python3 tools/control.py space 5             # 5 Space taps
python3 tools/control.py tap right 30        # right-arrow held for 30 BIOS ticks
python3 tools/control.py press up            # held key (no auto-release)
python3 tools/control.py release up
python3 tools/control.py enter 3             # 3 Enters (auto-paired make/break)
python3 tools/control.py raw 12 4D 14 00     # raw protocol bytes (escape hatch)
python3 tools/control.py status              # FIFO + latest screenshot path
```

Keys accepted by `tap`/`press`/`release`: `up down left right space
enter esc tab backspace`, lowercase letters `a..z`, digits `0..9`, or
raw `0xHH`.

Configuration:
- `--fifo PATH` / `$SAISEI_CONTROL_FIFO` — defaults to `/tmp/saisei_fifo`.
- `--shots-dir PATH` / `$SAISEI_CONTROL_SHOTS` — defaults to the
  `screenshots/` directory derived from the FIFO's runtime directory.

The CLI is the durable contract for driving. The byte protocol (press/release/
tap opcodes, `\x14` screenshot, `\r` Enter) is the underlying contract;
`tools/control.py` is just a friendly skin over it. If you need something the
CLI doesn't expose, `control.py raw …` writes arbitrary hex.

## Closed-loop driving via the virtual clock

The shim has a **virtual clock** that the program's perception of time goes
through, independent of wall-clock. Three opcodes manipulate it:

| Bytes | Meaning |
|-------|---------|
| `\x15` | **halt** — freeze virtual time. Wall time advances; the program does not. Idempotent. |
| `\x16` | **resume** — virtual time resumes continuously from the frozen value. |
| `\x17 <ticks_lo> <ticks_hi>` | **step** — advance virtual time by exactly N BIOS ticks, then halt. Deterministic single-step. |

Together with the read (`\x18 <addr_4B> <len_1B>`) and snapshot (`\x1A`)
opcodes, this turns the program into a turn-by-turn engine: `step → read →
decide → step → …`. Driver latency drops out of the program's perception of
time because the world only advances when a step says so. CLI wrappers
in `control.py`: `halt`, `resume`, `step <ticks>`, `read <addr> [len]`,
`snapshot [--out PATH]`.

The same primitives also make `--speedup` the single knob that sets
speed across hosts — virtual time advances per real time at a
fixed ratio regardless of host capacity.

## Inspecting program state (no symbols required)

`control.py read <addr> [len]` and `control.py snapshot` dump live RAM (to
`build/<name>/snapshots/`) without pausing or rebuilding. A common loop for
locating a state variable with no symbols: halt, snapshot, perform a known
input, halt, snapshot again, and diff the two dumps — bytes that changed only
when you acted are candidates. Combine filters across several trials to narrow
them: a value that reverts after a reversible action, or moves monotonically as
you repeat one, is more likely to be real state than an animation artifact.

**The filter that catches sampling artifacts** is the sub-tick test: take many
snapshots one BIOS tick apart (`step 1`), with and without input. A real
coordinate must be (a) identical across all idle ticks and (b) responsive to
held input at the tick scale. Coarser-step comparisons phase-lock onto
animation cycles and mis-identify them as state.

`tools/zoom.py SRC COL ROW` crops a fixed 4×4 grid cell (80×50 pixels) out of a
screenshot when you need to inspect a region more closely than the full 320×200
frame allows.

## The tap/step timing gotcha

`tap right N` schedules the release at "virtual-now when the tap opcode was
processed + N ticks". A following `step N` schedules its own halt at
"virtual-now when the step opcode was processed + N ticks". Those two
`virtual_now()` reads happen at slightly different moments (control.py issues
them as two separate FIFO writes), so the release deadline can land just before
or just after the step's halt. When it lands after, the key stays "pressed" past
the end of the step, and the next action behaves as if the previous direction is
still held. Workaround between actions:

```bash
python3 tools/control.py release left
python3 tools/control.py release right
python3 tools/control.py release up
python3 tools/control.py step 3       # let the releases land
```

The proper fix is a fused press-step-release opcode that does all three inside
one safepoint pass; the workaround is fine for now.

## Failure modes worth knowing

- **Tap fires but nothing moves** → check `grep TAP /tmp/saisei_stdout.log` for
  the release timing. If release fires in milliseconds rather than seconds,
  you're hitting the PIT-catchup bug (shouldn't happen with current code since
  pending_release uses wall-clock deadlines, but worth checking after any pacing
  refactor).
- **FIFO writes block forever** → the program died and the FIFO has no
  reader. `pgrep -f build/<name>/<name>` shows nothing. Restart it.
- **Tap opcode short-read** → the `read(keyboard_fd, buf, 3)` inside the
  `0x12` handler busy-loops up to 1000 times to assemble the 3-byte payload.
  If you see `[TAP] short read got=N` lines, the writer isn't using unbuffered
  writes. Atomic 4-byte writes via
  `python3 -c "open(fifo,'wb',buffering=0).write(bytes(...))"` always succeed.
- **Unhandled pc lands at a chunk-swap region** → check whether the
  `[BUG]` report diagnoses `overlapping mappings at same linear`. If yes,
  the upstream bug is stale ret-target attribution — the chunk that owned
  the address at call-time isn't the chunk loaded now. Fix is in the
  loader/chunk-attribution layer.
