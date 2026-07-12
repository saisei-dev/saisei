# Contributing to Saisei

Saisei is early and there's plenty to build. The most valuable thing you can do
right now is **run a game and tell us what happened** — see [Reporting a
game](#reporting-a-game) below. If you want to go further and change the code,
read on.

## Dev setup

The build prerequisites are the same as for playing (a C compiler, SDL2, and
rustup — see the [README](README.md#start-playing)); the pinned toolchain is
fetched by cargo itself. Set up the commit hook once:

```bash
git config core.hooksPath .githooks   # runs `cargo fmt --check` before each commit
```

Run `cargo fmt` and `cargo test` before you push.

```bash
cargo build --release                                   # the toolchain + launcher
cargo test                                              # whole workspace
cargo test -p saisei-jitc --test ported_disasm          # a single test file
```

## Finding your way around

Start with the [architecture overview](docs/architecture.md), then the
[runtime memory model](docs/runtime_memory_model.md) — the origin-tracking
scheme that makes unpacked and overlaid code dispatchable is the idea most worth
understanding before you change anything.

The workspace is four crates: `saisei-jitc/` (the translator and JIT),
`runtime/` (the runtime — shims, DOS/BIOS/hardware emulation, video, audio),
`saisei/` (the launcher), and `saisei-game/` (the thin per-game binary). Most
translation bugs live in `saisei-jitc/src/codegen.rs`; most fidelity bugs live in
`runtime/src/`.

## The prime directive: emulate faithfully

Saisei models real x86, BIOS, DOS, and hardware behavior *exactly*. When the
model is faithful, a game simply runs, with no special-casing — so a patch that
adds a heuristic ("detect this case and recover", "fix up the stack if it looks
wrong") will not be accepted. Those harnesses paper over an unfaithful model and
make wrong behavior masquerade as working.

The games are real, correct, shipping binaries. When something breaks, the bug is
in *our* code — read the relevant shim or the code generator against the x86 /
DOS / BIOS spec, and fix the model.

## Verifying a change

A change is proven when a real game reaches its known scene, not when the tests
pass. `cargo test` covers translation and shim units; it is not the acceptance
bar for a runtime behavior change. Run the game (`saisei play <name>`, or
`saisei run <name> --headless --screenshot-secs N` to capture PNGs) and say in
your PR which game you ran and what you saw.

## Reporting a game

Every game is a test case, and we want both outcomes:

- **It works** — tell us, and we'll add it to the tested list in the
  [README](README.md#tested-games). Include the release you used.
- **It doesn't** — [open an issue](https://github.com/saisei-dev/saisei/issues)
  with the game, where it got to, and what you saw. If the run produced a crash
  bundle (`crashes/`) or a `build/<game>/lifecycle.log`, attach it. A game that
  hangs on an unimplemented port or an unfaithful instruction is exactly the
  signal that moves the emulation forward.

## License

By contributing you agree that your contributions are licensed under the MIT
License, the same as the rest of the project.
