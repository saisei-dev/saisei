# Command & configuration reference

Everything you can tell Saisei to do, grouped by who needs it. Run `saisei-cli help`
for the same list in the terminal, and `saisei-cli control help` for the console.

The surface is deliberately tiered:

- **Player** — what you need just to play a game.
- **Developer** — debugging and reverse-engineering the runtime/translator.
- **Drive** — automating a running game, often from an AI/agent.

Configuration is passed as **command-line flags**, not environment variables. The
only environment variables left are internal plumbing the launcher sets for you
(see the last section) — you never set them by hand.

## Commands

### Player

| Command | What it does |
| --- | --- |
| `saisei-cli play <game>` | Build and run in the SDL window. |
| `saisei-cli run <game>` | Build and run; headless-capable (good for automation). |
| `saisei-cli build <game>` | Build the game binary. `run`/`play` do this for you. |
| `saisei-cli new-game <archive>` | Create a game bundle from a zip / directory / URL. |

### Developer

| Command | What it does |
| --- | --- |
| `saisei-cli triage` | Inspect the newest crash bundle. |
| `saisei-cli state-discover …` | Discover memory-state predicates across snapshots. |
| `saisei-cli zbookend-diff <a> <b>` | Diff two snapshots to find who wrote an address. |
| `saisei-cli zoom <img> <col> <row>` | Pixel-zoom a screenshot tile. |

### Drive (automation / AI)

| Command | What it does |
| --- | --- |
| `saisei-cli control <cmd>` | Drive a running game through its FIFO (see below). |
| `saisei-cli replay <log>` | Replay a recorded session against a `--replay` run. |
| `saisei-cli run-with-pty <cmd>` | Run a command under a pseudo-terminal. |

## `run` / `play` options

| Flag | Tier | Meaning |
| --- | --- | --- |
| `--program <name>` | player | Pick a program in a multi-executable bundle. |
| `--restore-from <save>` | player | Resume from a snapshot. |
| `--speedup <n>` | player | Emulation speed multiplier (default 1). |
| `--headless` | drive | Run without an SDL window (`run` only). |
| `--screenshot-secs <n>` | drive | Auto-dump PNGs every *n* seconds (headless). |
| `--replay` | drive | Record inputs for later replay. |
| `--verbose` | dev | Stream the shim trace to stdout. |
| `--trace-file <path>` | dev | Write the execution trace to a file. |
| `--lifecycle-file <path>` | dev | Write `LOAD`/`CALL`/`JMP` lifecycle events. |
| `--patch-bundle <path>` | dev | Load a game-function patch `.so`. |
| `--features <list>` | dev | cargo features for the game build (e.g. `force_exit_after_10s`). |

`build` additionally accepts `--warm` / `--warm-secs <n>` to warm the JIT cache
after building.

> Note: `--verbose` output is redirected *into* the trace file when
> `--trace-file` is also given — so stdout will look quiet, and the trace file
> holds everything.

## The `control` console

`saisei-cli control [global options] <command> [args]` drives a running game through
its control FIFO. Global options: `--fifo <path>` (default `/tmp/saisei_fifo`),
`--shots-dir <path>`, `--snapshots-dir <path>`, `--gap <ms>` (default 80).

Keys accept names (`up`, `enter`, `esc`, `f1`, `a`) or a hex scancode (`0x39`).

**Input:** `tap <key> [ticks]` · `press <key>` · `release <key>` ·
`enter [count]` · `space [count]` · `raw <hex>…`

**Execution:** `halt` (freeze the virtual clock) · `resume` · `step [ticks]`

**Observe:** `shot [--out P] [--timeout S]` · `read <addr> [len] [--out P]` ·
`snapshot [name] [--out P]` · `status`

Malformed input (unknown command, bad key, out-of-range address, wrong argument
count) fails with a specific message; `saisei-cli control help` lists everything.

## Environment variables (internal only)

You do not set these — the launcher sets them for the game process, and two are
consumed by the build:

| Variable | Set by | Purpose |
| --- | --- | --- |
| `SAISEI_REPO_ROOT` | launcher | Repo root, so the runtime can find the translator + caches. |
| `SAISEI_JITC` | launcher | Path to the `saisei-jitc` translator binary. |
| `SAISEI_JIT_DIR` | launcher | Per-game JIT chunk cache directory. |
| `SAISEI_RUNTIME_VERSION` | launcher | Git short-hash stamped into crash manifests. |
| `SAISEI_GAME_CONFIG` | build | Path to the generated `game_config.rs` (read by `build.rs`). |
| `SAISEI_CAPSTONE_LIB_DIR` | you (optional) | Link an external libcapstone instead of the vendored static one. |
| `SAISEI_VERBOSE` | test harness | Internal: enables the shim trace at library-load time; used only by the shim unit tests (which have no argv). Prefer `--verbose`. |
