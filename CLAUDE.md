# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this project is

A **JIT binary recompiler** that turns DOS MZ executables into native Rust and runs them. The runtime loads a game's program image, takes the entry `cs:ip` from its MZ header, and — the first time control reaches any code segment — dumps the live 64KB, decodes it to a lossless JSON IR, emits it as Rust, compiles it with rustc, and `dlopen`s the result. The translated Rust *is* the game; there is **no interpreter, no external emulator, and no ahead-of-time whole-image decode**. Code is discovered and compiled on demand as the program executes, which is exactly what makes packed / overlay / self-modifying games work with no special-casing.

Everything is Rust, workspace at the repo root: `saisei-jitc/` (the translator +
JIT, exposed as the `saisei-jitc` binary + library), `saisei-player/` (the
**`saisei`** binary — the player app, and the one host that runs every game),
`saisei-ui/` (its interface, as a pure software compositor), `saisei/` (the
**`saisei-cli`** binary + the launcher library), `runtime/` (the runtime crate,
`saisei-runtime`), and `saisei-game/` (the thin per-game bin crate, now built
only for the future freeze — nothing in the play path compiles per game). No
clang and no C build system (make/cmake): the only C in the tree is the vendored
capstone disassembler, which the cc crate compiles from inside cargo
(`vendor/capstone-sys`) using the same system C compiler rustc already needs as
its linker. The toolchain is pinned to a *dated* nightly (`rust-toolchain.toml`)
for `c_variadic`/`linkage` in the runtime — the date is deliberate, since a
floating nightly can break those unstable features out from under a fresh clone.

The system prerequisites are exactly two — a C compiler/linker and SDL2 — plus
rustup. Nothing shells out: `new-game` downloads over HTTPS in-process (ureq) and
extracts zips in-process (the zip crate), `control status` reads /proc, the build
revision is baked in by `saisei/build.rs`, and glyphs are rasterized by fontdue
against a bundled font. Don't reintroduce a `curl`/`unzip`/`git`/`fuser`
subprocess or a system font library — a missing tool becomes a first-run failure
for someone who just wants to play a game.

## Commands

Build once with `cargo build --release`. That produces two binaries in
`target/release/` (put it on your PATH):

**`saisei` — the player.** With no arguments it opens its window: the logo, then
your library, then the game you pick, then the in-game overlay, all in the *same*
window and the *same* process. Games are added by dropping a zip on the window or
pasting a link. F12 pauses the running game and brings up its menu (save / load /
settings / the library / back into the game — and going to the library does *not*
end the game: it is a screen over the pause). `saisei --play <name>` boots
straight into a game, which is what the CLI and the save-load re-exec both use.

**`saisei-cli` — everything else.** The dev/automation surface:

```bash
saisei-cli new-game <archive> --exe FOO.EXE   # create games/<name>/<name>.json
saisei-cli run   <name> --headless            # run without a window (scripting/CI)
saisei-cli play  <name>                       # run in the window
saisei-cli run   <name> --program setup       # a bundle may define multiple programs
saisei-cli build <name>                       # emit the per-game GameConfig + binary
```

`run`/`play` do **not** compile anything per game: they build the player and hand
it the game by name, and the player reads the bundle's `<name>.json` at run time
(`saisei_set_game_config`, then `shim_boot_machine`). `build` is what still emits
the per-program GameConfig (Rust) and cargo-builds the `saisei-game` bin (runtime
rlib + config, linked `-rdynamic`) to `build/<name>/<program>` — that artifact is
what a *frozen* build will be made of; it is not on the play path. All program
code is JIT-compiled at run time either way.

Useful `run` flags: `--verbose` (runs are silent by default; this prints the shim trace to stdout), `--trace-file <path>` (write execution trace), `--lifecycle-file <path>` (stream LOAD/CALL/JMP/… events), `--patch-bundle <path>` (load a game-function patch `.so`), `--features <list>` (cargo features for the player build — e.g. `--features force_exit_after_10s` for a self-terminating smoke run). Screenshots: pass `--screenshot-secs N` (headless runs) to auto-dump PNGs to `build/<name>/screenshots/`. All user config is passed as CLI flags (forwarded to the host's argv); the only env vars left are internal plumbing the launcher sets itself — `SAISEI_REPO_ROOT`/`SAISEI_JITC`/`SAISEI_JIT_DIR` (the runtime JIT needs them to invoke the `saisei-jitc` translator and cache chunks). Run `saisei-cli help` for the full command surface (Player / Developer / Drive tiers) and `saisei-cli control help` for the drive console; `docs/console.md` is the written reference.

Tests are Rust `cargo test` — the translator unit tests (`saisei-jitc/tests/`,
asserting on the chunk emitter's Rust output and the shared front-half) and the
runtime shim tests (dlopen'd isolated copies of the runtime cdylib, driven via
FFI), plus launcher tests in `saisei/tests/`. Capstone (5.0.7, x86-only) is
vendored and built statically by `vendor/capstone-sys` — no env vars needed;
setting `SAISEI_CAPSTONE_LIB_DIR` optionally links an external libcapstone
dylib instead.

```bash
cargo test                                              # whole workspace
cargo test -p saisei-jitc --test ported_disasm          # one test file
cargo test -p saisei-jitc --test ported_disasm disassemble_retf__retf  # one test
```

## Architecture

**Translator (`saisei-jitc/src/`, shared by every JIT compile):**
- `disassemble.rs` — Capstone-decodes a byte image into `program.ir.json` + header/reloc/xref metadata. Computes per-operand default segment (BP/SP→SS, else DS), entry points, basic-block boundaries.
- `translate.rs` — the shared front-half: IR instruction utilities, operand rewriting (`rewrite_mem_op` → `memb`/`memw` accessors, RCB/exec_params named fields, stack vars), flag normalization (`normalize_flags`), basic blocks (`build_basic_blocks`) and CFG successors. The RCB field table (`RCB_FIELDS`) lives here, kept in sync with the chunk prelude by unit tests.
- `codegen.rs` — the chunk emitter: IR → a flat pc-state-machine (`loop { pc = match pc { … } }`, one arm per basic block calling a small per-block `fn … -> c_int` that returns the next pc, -1 to leave the dispatcher; `set_ip`/`SAFEPOINT()` per instruction) plus per-function `_impl` wrappers. Blocks are separate small fns on purpose: rustc's per-body analyses (borrowck) are superlinear, and one giant dispatch fn made JIT compiles ~3× slower. **This is where most translation bugs live.** A construct it can't express is a hard `Unsupported` error — there is no fallback backend; extend `codegen.rs` (repro with `saisei-jitc emit` or the `gap_sweep` test).
- `generate_game_config` (in `saisei/`) — emits the per-game `GameConfig` Rust data file (program image path, PSP load segment, protected slots) from the one `<name>.json`; the `saisei-game` build.rs `include!`s it. Carries no dispatch table and no entry symbol: the runtime takes the entry `cs:ip` from the MZ header and JITs from there.

**JIT (`saisei-jitc jit-compile` + runtime):** When control reaches an address with no compiled chunk, `jit_compile_or_get` (`runtime/src/shims.rs`) dumps the live 64KB segment to `SAISEI_JIT_DIR/seg_<base>.bin`, runs the `saisei-jitc jit-compile` translator (via `SAISEI_JITC`) on it, compiles a `.so` with rustc, and `dlopen`s it — no restart. Chunks live at `build/<game>/jit/jit_<segbase>_<offset>_<rssha>.{rs,so,sha,keys,code,funcs}`, content-addressed by SHA of the emitted Rust (name-normalized) + toolchain hash — the same seg:ip decoding different *code* bytes gets distinct chunks, while dumps differing only in data bytes emit identical Rust and reuse one compiled `.so` (rustc dominates; it is compiled `-C opt-level=0` deliberately — the emulated CPU is throttled with ample host idle, so compile latency, not chunk speed, is what the player feels). **Function-level dedup:** each chunk's `.funcs` sidecar lists the function starts it emitted; a later compile at the same segbase drops functions another chunk already owns (keeping only its own entry function plus not-yet-owned ones), and transfers to a dropped function route through the normal inter-chunk dispatch (`call_table_` / `near_ret_tail_`) to the chunk that owns it. This cut measured block-level recompile redundancy from ~2.2× to ~1.35×. A `jit_<segbase>_<offset>_<blobsha>.alias` sidecar per distinct 64KB dump names its chunk so an identical dump resolves without even re-decoding. Every chunk links the precompiled `saisei_rt` rlib (`--extern saisei_rt=libsaisei_rt_<toolchainhash>.rlib`; the 941-line prelude is built once per toolchain from the `saisei_rt.rs` dropped beside the chunks, not re-`include!`d and reparsed per chunk — it was ~86% of a small chunk's rustc time) — it binds the shared `cpu` global and the runtime ABI; its `#[repr(C)]` layouts and the runtime's must be edited together. The prelude lives upstream of the chunk, so each chunk hands its own name back through a `#[no_mangle] saisei_site_name()` fn that `rt::site()` calls. Each chunk's dispatch match is keyed on IP; the chunk runs based at the live `cs`, so pushed near-call return IPs are true cs-relative offsets and round-trip through retf/far-jmp. The same physical code reached under a different `cs` alias becomes a *separate* chunk at that alias's seg base — segment/return handling must stay faithful to the x86 model across that seam. To hand-instrument a chunk: edit its `.rs` and recompile the `.so` in place (`rustc --edition 2021 --crate-type cdylib -C opt-level=0 -C overflow-checks=off -C debug-assertions=off -C panic=abort --extern saisei_rt=libsaisei_rt_<hash>.rlib -o <chunk>.so <chunk>.rs`, run in the jit dir); the `.sha` stays valid so the cache hits.

*Freezing:* the end goal is to collect the JIT-discovered chunks and link them into a fully static native build (no runtime compiler at all). The runtime dispatch tables (`GameConfig.binary_dispatch`, the `DispatchFn` ABI) are retained NULL-but-shaped for that future freeze to populate; today they are empty and every address routes through the JIT.

**Runtime (`runtime/`, crate `saisei-runtime`; port notes in `docs/runtime_port_notes.md`):**
- `shims.rs` — the big integration surface: the machine loop (`run_machine` → `resolve_and_run_chunk`), `memb`/`memw`, `file_mappings`, the JIT registry + `dispatch_via_binary`, IRQ delivery, WATCHW tripwires, crash bundles, the function-patch registry. Entry point `saisei_main`.
- `dos.rs` (INT 21h: file I/O, memory alloc, console), `bios.rs` (INT 10h/16h, …), `mouse.rs` (INT 33h).
- device emulation: `io_bus.rs`, `video.rs`, `keyboard.rs`, `timer.rs`; display: `sdl.rs`; persistence: `snapshot.rs`, `save_manager.rs` (their `#[repr(C)]` layouts are FROZEN — snapshots serialize them byte-for-byte), `devices.rs` (device state; see below).
- **Device state across a save/load (`devices.rs`)**: a restore is a **fresh process** (`save_manager` re-execs with `--restore-from`), so every host-side `static mut` returns to its power-on initializer. Guest RAM and the CPU come back from the bundle; anything the guest programmed into a *device* comes back from `devices.bin`, a tagged block container (`MOUS`/`PIC8`/`PORT`/`PIT2`/`DOSS`/`DOSF`/`SN76`/`SBLA`/`DMA8`) written beside the other snapshot files. It is tagged rather than folded into `ShimRuntimeState` because that struct is a frozen v6 layout whose restore *hard-fails* on a version mismatch — extending it would make every existing save unloadable, whereas an unknown tag is skipped and a missing one only costs that device. **Two rules, both enforced by tests in `devices/tests.rs`:** (1) every guest-programmable register is captured — if a game can write it and read it back, or *hear* it, it needs a block; (2) every derived cache is re-derived from those registers on restore (`devices::post_restore`) — a device's cooked render-side state (the OPL2's FM `Synth`, the speaker's segment classifier) is built up by the *writes*, so restoring the register bytes alone leaves the guest reading a correctly-programmed chip while the one it is heard through was never programmed at all. **A DOS file handle is one of those registers**, and the least device-looking of
  them: the guest holds the handle number and reads/writes/seeks through it, while
  the `FILE*`, the path and the seek offset behind it are all host state. Without
  the `DOSF` block a restore handed the guest back a handle onto nothing — and it
  did not *notice*, because what it had already read came back in RAM. It only died
  the next time it reached for the disk. Dungeon Master keeps `DATA\GRAPHICS.DAT`
  open and streams levels off it, so: load a save, walk down the stairs, the read
  fails, "SYSTEM ERROR", INT 21h AH=4Ch. `DOSF` captures (handle, path, access mode,
  offset) and reopens each on restore — never with a truncating mode, whatever the
  guest opened it with, or the create-mode handle would erase the file the save
  exists to preserve. A save written before `DOSF` existed simply has no such tag,
  and restores as it always did (without its handles).
  `hardware_state_survives_a_power_cycle` programs the machine through its real interfaces (ports, INT 33h), saves, scribbles over every device, restores, and requires both the captured bytes and the guest-visible read-backs to match; `every_io_bus_device_has_a_snapshot_block` fails for a new bus device with no block; `opl2_sounds_the_same_after_a_restore` compares *rendered samples*, not bytes. Note for anyone adding a block: build the `#[repr(C)]` snap struct from `mem::zeroed()` and assign fields — a struct literal leaves padding undefined, and `pod_capture` copies it.
- **sound (`audio/`, see `docs/audio.md`)**: OPL2/AdLib FM (`opl2.rs`), PC speaker (`speaker.rs`), Tandy SN76489 (`sn76489.rs`), Sound Blaster + 8237 DMA (`sb.rs`, `dma.rs`), the mixer (`mod.rs`) and SDL sink (`out.rs`). All rendering happens **on the guest thread in virtual time** — sound-port writes force a catch-up *before* the write lands, so register timing is sample-accurate; `safe_point_impl` pumps the rest. Three non-obvious invariants keep the device queue fed, and each one was independently enough to make it crackle: the queue must be **primed** (we only render the audio virtual time has *earned*, so there is nothing spare to build a buffer out of), audio must render **before** the pacer sleeps and before a frame presents (both spend real time producing no virtual time, and so only drain it), and the rate controller must act **over seconds** (tuned tighter it does not correct the drift, it becomes it). The PC speaker is deliberately **re-voiced** rather than reproduced — see below.
- Built as rlib (linked into `saisei-game`) and cdylib (`libsaisei_runtime.so`, dlopen'd by the shim unit tests).

**Runtime memory model (`docs/runtime_memory_model.md`):** every linear byte has an *origin* (file + offset), tracked in `file_mappings[]` (newest covering entry wins). The top-level loop `run_machine` → `resolve_and_run_chunk` resolves the live `cs:ip` to its owning JIT chunk (compiling it on first reach); cross-binary/indirect transfers go through `dispatch_via_binary`. This origin tracking is what makes on-the-fly-loaded (unpacked/overlay) code dispatchable and savable.

**Function patches (`runtime/src/shims.rs`):** a `GamePatch` replaces or augments a game function identified by `(binary basename, file_off)` — the stable identity the dispatcher resolves addresses to, so one patch applies across cs-aliases. Patches register at startup or from separately-delivered `.so` bundles (`patch_load_bundle`, `--patch-bundle`); a patch fn returns `PATCH_HANDLED`/`PATCH_DECLINED` and can call `patch_call_original`/`patch_call_function`/`patch_ret_near`.

**Staging the guest's disk (`copy_runtime`, `saisei/src/lib.rs`):** `build/<game>/`
is the guest's C: drive; the bundle under `games/<game>/` is what it is *seeded*
from. Those are two different things, and the difference only shows once a game
writes to its own disk — which is the entire point of a setup program. POP's
SETUP.CFG and Zeliard's resource.cfg ship *in* the bundle and are rewritten *by*
the guest, so re-copying every bundle file over the drive on each launch (which is
what this used to do) silently undid every setting the player had just chosen.
Never re-copying is wrong too — a corrected bundle file would then never reach the
drive. So staging **writes down what it staged** (`build/<game>_stage.json`: dest →
size+mtime, kept beside the drive, not on it, so a DOS `FindFirst` never sees a
file we invented): a drive file still exactly as we left it is ours to refresh; one
that differs is the guest's and is left alone. The record must always hold *what we
put there*, never what is there now — record the guest's own file as if we had
staged it and the next launch finds a "match", calls it ours, and copies the bundle
straight over the writes it just preserved. No record at all = a drive that
predates the record: seed it as before, and adopt it. Consequence, and it is the
DOS one: the disk is the disk, so a data file a game corrupts is not repaired by
relaunching — remove `build/<game>/` to reseed.

**Per-game config (`games/<name>/<name>.json`):** `name`, `program_path` (the MZ image to load), optional `programs` (multi-executable bundles, each with its own `program_path`/`psp_seg`), `psp_seg`/`init_cs` (machine params), `protected_slots` (runtime memory-protection ranges), and `runtime` (files copied into `build/<game>/` at run). Both consumers read it through `saisei::game_config_values` — `generate_game_config` bakes those values into a frozen per-game binary, and the player installs them at run time — so the two cannot drift. The per-binary `<binary>.json` sidecars and the `aliases`/`callgraph`/`regions`/`vars`/`enums` files are reverse-engineering annotations (function names, comments, discovered entries) — not part of the JIT run path. Diagnosis artifacts land in `build/<game>/` (`lifecycle.log`, `watchw.log`) and `crashes/`.

**Player (`saisei-player/` = the `saisei` binary, `saisei-ui/` = its interface):**
one window and one process for a whole session — logo, library, the game itself,
the pause menu. `saisei-ui` is a *pure* software compositor (RGBA in, actions
out): no SDL, no runtime, no machine, so the whole interface unit-tests with no
window on screen, and the same code paints the library standing alone and the
same library over a paused game. The runtime owns the SDL surface it paints onto
(`saisei_ui_*` in `runtime/src/sdl.rs`).

- **One page, one size, one place.** Every screen is laid out in the same rect —
  the window, less a margin — and wears the same bar: a **drawn** back button and
  a title saying where you are. Between a screen with a game paused behind it and
  the same screen without, the *only* difference is the backdrop (`t::SCRIM` over
  the frozen frame, vs the page's own gradient). This is what killed the old
  floating overlay panel: a panel is a different size and shape from the library,
  so every step between them resized the thing under the player's cursor. Two
  tests hold the line — `the_frame_never_moves_between_screens` and
  `every_screen_but_the_root_library_draws_a_way_back`. Escape and F12 are
  shortcuts for the back button, never the only way out: the line of grey hints
  that used to sit at the foot of each page ("Arrows move  Enter choose  Esc
  back") *was* the exits, printed for anyone still reading.
- **Nothing irreversible happens without an answer** (`Ui::request_launch`,
  `offer_delete` → one `Confirm`, which carries the `Action` it is asking about so
  a yes cannot drift from the words on screen). Every confirmation opens with the
  cursor on Cancel.

- **Hosting.** The player installs the bundle's `GameConfig` at run time
  (`saisei_set_game_config`) and calls `shim_boot_machine` + `saisei_main`
  in-process. `init_memory` is an `.init_array` ctor that loads the program image
  *before main*, which is fine for a per-game binary (it knows its game at link
  time) and useless for a host that learns its game from argv — hence
  `shim_boot_machine`, written as a **reset**, not an increment.
- **The pause menu pauses by blocking in the safepoint**, exactly as
  `retire_splash` already does. No new pause machinery: virtual time is
  instruction-driven, the vclock is halted, and the pacer re-anchors instead of
  fast-forwarding — so a menu left open costs the guest no time and no interrupt
  backlog. F12 is swallowed in the key handler before any guest mapping; held keys
  are released on entry.
- **The library is a screen inside that pause, not the way out of it.** It is
  reached and left without the guest running an instruction (`overlay_entry` keeps
  its loop; `Screen::Library` with `Ui::running` set). It used to be an
  `Action::ToLibrary` that re-execed the player — which threw a running game away
  for the crime of wanting to look at the list. There it is **browse-only**
  (`Ui::can_edit_library`): no Add, no "…", no Delete, no drop, because adding or
  removing a game renumbers the very list the paused game is identified by, and
  none of it is work that cannot wait until the game has been left. `Ui::running`
  is an *index*, not a flag, precisely because the page on screen need not be the
  game you are in — Resume/Save belong to the paused game's page and nowhere else.
- **It opens at a *savable* point, not on the spot.** A snapshot is only valid at
  a dispatcher resting point (`save_manager_can_save_now`: zero lcall/isr depth,
  `ip` a case key — restore refuses anything else). So F12 arms, the guest runs
  on to the next resting point, and *there* it stops. That is why Save is always
  live rather than mysteriously grey.
- **Starting a game re-execs** (`relaunch.rs`) — and with one already paused, that
  is the *only* thing left in the interface that ends it, whether it is called
  loading a save, starting over, or switching games. The guest has run — memory,
  DOS file table, JIT registry all populated — and there is no honest way to unwind
  that in place. Hence one gate for all three: while `Ui::running` is set, no
  `Action::Launch` leaves the UI until the player has answered for it. The
  runtime's own save-load path re-execs for the same reason, and takes its argv
  from the host (`saisei_set_relaunch_argv`), since a GUI launch's bare `saisei`
  cmdline names no game to come back into.
- **Player saves** live under XDG (`~/.local/share/saisei/saves/<game>/`), never
  overwrite, and carry a thumbnail of the *presented* frame — **not**
  `shim_render_screenshot_png`, which re-reads guest VRAM as a linear 320x200 at
  0xA000 with no planar branch and yields garbage for an EGA game. They are a
  different thing from the runtime's `saves/slot_N` rewind ring, which is
  untouched.
- **A game's card has a "…" menu**: Run a file, and Remove game. **Run a file**
  boots one of the *other* programs on the game's disk — its setup, its installer
  — once, on the same drive, instead of the game (`--exe FILE.EXE`,
  `LaunchSpec.exe`; the game's `programs[]`/`--program` is a different thing and
  is untouched). Three things make it actually work, and each was independently
  enough to make it useless: the drive must **keep what the setup wrote** (see
  staging, below) or the game reads back the shipped default; the **splash must be
  off** (`virtual_display_set_splash(false)`) because the pre-game hold only ends
  on the first *graphics* frame and a setup is text-mode start to finish, so the
  window would stay blank forever; and the program must **come back to the
  library** when it terminates (a `libc::atexit` hook that re-execs, armed only for
  a windowed one-off and only on a *clean* DOS terminate — `machine_halted` — so a
  crash stays a crash). The pause menu does not offer **Save** during one: a
  snapshot there would be a picture of SETUP.EXE filed under the game's name.
  Only `.EXE` MZ images are offered, which is DOS's own rule for what is runnable —
  it is what keeps DM's EGA/VGA/ANIM *overlays* (real MZ images, loaded by DM into
  its own memory, dead on a fresh PSP) off the list. `.COM` is off it too, and that
  one is a real gap: `load_executable` is **MZ-only** — it reads the size, entry
  cs:ip and relocations out of an MZ header — while a `.COM` is headerless (DOS
  lays it at PSP:0100, cs=ds=es=ss=PSP, ip=0100). Teaching the loader that protocol
  is what would put Kings Bounty's `KB!.COM` back on the list.

## Working principles (non-obvious, enforced)

- **The PC speaker is re-voiced on purpose, and this is not a violation of the directive below.** A game's beeper *tones* are extracted as a note stream and played through a soft synth; its PWM/digitised output is reproduced faithfully as PCM. The guest still sees a bit-exact 8254 and port 61h — the audio path and the guest's port-0x61 reads go through the *same* `pit_ch2_output_at()`, so what a game hears and what it reads back cannot disagree. The prime directive is about the machine the program runs on; what happens to the pin waveform downstream of the emulation, on its way to the speakers, is a rendering choice the guest cannot observe or branch on. The tone/PWM split is not a guess either: the speaker renders 3ms **behind** the mixer, so by the time a segment is classified the renderer can already see what came after it and *knows* whether the divisor was held (a note) or overwritten (a PWM step). Deciding late beats predicting. See `docs/audio.md`.
- **Emulate faithfully — no heuristic harnesses. This is the prime directive.** The JIT and shims must model real x86 / BIOS / hardware / DOS behavior *exactly*; when they do, the game just runs, with no special-casing. Do **NOT** add fallbacks, recoveries, or "is-this-really-X?" detectors that guess based on heuristics — e.g. windowed address-recovery, `retf_is_genuine_far_return`, stack-drift fixups, "redirect if the window-corrected address happens to be a decoded case-key." Every such harness papers over an *unfaithful model* and makes wrong behavior masquerade as working. When a transfer / return / address / register comes out wrong, the bug is that the translation or shim isn't faithful — fix the model so the band-aid becomes unnecessary, then delete the band-aid.
- **Don't treat one of our own outputs as a known-good oracle.** A program that has never run correctly end-to-end (e.g. SETUP.EXE) has no trustworthy reference chunk. "This JIT chunk is byte-identical to that one" proves nothing — both are our output and can be equally wrong. A packed EXE's on-disk image is just the unpacker stub; the real program is *all* runtime-JIT'd. Validate against the real x86/DOS/BIOS contract, not against our own translation.
- **Bugs are in our shims or our generated code — never in the game; find them by reading OUR code against the x86/DOS/BIOS spec, not by studying the game's.** The games are real, correct, shipping binaries. The trap is tracing the game's own code (loader, decompressor, allocator) *to localize which shim is unfaithful* — still studying the game, still a waste: the game is not the variable, our model of x86/DOS/BIOS/HW is, and tracing the game only re-derives the faithful behavior you already know from the spec. Instead, name the corruption's signature, list the OUR-code surfaces (`runtime/src/` shims, `saisei-jitc/src/` codegen) that implement that operation, and read them against the spec for an unfaithful implementation or simplification — a library call standing in for an instruction's exact semantics, uncomputed flags, an "approximate"/"good enough" shortcut, an abort-on-unknown stub. Decode the game only to *confirm* a hypothesis already pinned to a named `file:function` in our code — never to discover the bug, and not via the generated game (`program.ir.json`, `build/<game>/jit/*.rs` are the game re-expressed in Rust; reading them to follow what the game computes is game-tracing with extra steps, though reading a chunk to fix a codegen defect visible in the emitted Rust is fine). (Example: `handle_les`/`handle_lds` hardcoded the mem-operand segment to DS, but `[bp+…]` defaults to SS — a codegen bug; likewise a block-copy shim's `memmove` silently broke `rep movs` overlap-replication semantics — a shim bug. Both found by reading our code, not the game.)
- **No external emulator as an oracle.** Never reference/diff against DOSBox/QEMU/dosemu; that tooling was deliberately removed. Saisei is self-contained.
- **No fabricated artifacts or address-guessing.** Don't hand-write output files (e.g. a program's config/save file) or guess addresses to get past a gate — produce them by running the real user journey. Stop and ask when in doubt.
- **No debug-toggle cruft.** Don't add `SAISEI_*` env vars or ad-hoc debug flags; use existing crash-bundle data (`lifecycle.log` already carries `ds`/registers) and WATCHW write-watchpoints (`write_watches[]` in `runtime/src/shims.rs`) to localize corruption.
- **Validate by running the real program (`run`), not just by tests.** A change is proven when the program reaches its known scene (screenshot it). The `cargo test` suite exercises translation/shim units but is not the acceptance bar for a runtime behavior change.
