# Saisei

*Saisei* (再生 — "regeneration / playback") is a JIT binary recompiler for DOS MZ
executables. The runtime loads a program image, takes the entry point from its
MZ header, and — as control reaches each code segment — decodes the live bytes
to a lossless IR, translates that to C, compiles it with clang, and dlopens it.
Nothing is decoded ahead of time; the translated C *is* the program, and
packed / overlay / self-modifying code just works because it is compiled on
demand as it executes.

## Bring your own program

No programs are bundled with this repository. Point Saisei at any DOS MZ
executable you have the right to use — bootstrap a bundle from a URL, `.zip`, or
local directory:

```bash
saisei new-game <archive-url-or-path> --exe YOURGAME.EXE
```

This creates `games/<name>/` with a `<name>.json` config. See
[Driving a program](docs/playing.md) for details.

## Licensing

This repository is released under the MIT License. See [LICENSE](LICENSE). The
license covers the recompiler and runtime only — the DOS programs you run
through it are yours and are never included here.

## Prerequisites

System packages (not pip-installable):

- Python 3.10 or later
- `clang` — the runtime compiles generated C, at build time and on the fly (JIT)
- **SDL2** (dev headers) — the runtime links against it for the viewer window

```bash
# Debian/Ubuntu
sudo apt install python3-venv clang libsdl2-dev pkg-config
# macOS (Homebrew)
brew install llvm sdl2 pkg-config
```

## Install

Create a virtual environment and install the project editable. This gives you
the `saisei` command and puts the packages on the path, so you no longer need to
`export PYTHONPATH` or invoke `tools/game.py` by file:

```bash
python3 -m venv .venv
source .venv/bin/activate
pip install -e .            # core (capstone, networkx)
pip install -e ".[dev]"     # + pytest, flake8, pillow — to work on the pipeline
make hooks                  # enable the flake8 pre-commit hook (contributors)
```

## Linting

`flake8` is required and enforced — CI fails the build on any violation, and
`make hooks` installs a pre-commit hook that blocks commits that don't lint.
Rules live in [`.flake8`](.flake8) (max line length 100). Run it any time with:

```bash
make lint
```

## Using the toolchain

Bundles live under `games/<name>/` with a `<name>.json` config. The `saisei`
command has one entry point with subcommands:

```bash
saisei new-game <archive> --exe FOO.EXE   # bootstrap a bundle
saisei build <name>                       # emit the config + link the runtime
saisei run   <name> --headless            # build + run (headless = no window)
saisei play  <name>                       # build + run in the SDL window
saisei run   <name> --program setup       # multi-program bundles
```

The unpackaged form still works without installing — `export PYTHONPATH=$PWD`
then `python3 tools/game.py <command>` — and the `Makefile` targets below need
no install at all.

`build` does not decode anything ahead of time — it emits the per-program config
and links the runtime. All program code is JIT-compiled at run time into
`build/<name>/jit/` (cached by segment-bytes SHA + toolchain hash). Useful `run`
flags: `--silent`, `--trace-file <path>`, `--lifecycle-file <path>`. Set
`SAISEI_SCREENSHOT_SECS=N` to auto-dump PNG screenshots to
`build/<name>/screenshots/`.

## Make targets

The `Makefile` is a thin JIT-only wrapper over `tools/game.py`. Pass the bundle
name with `GAME=<name>`:

```bash
make new-game ARGS="<archive-url> --exe FOO.EXE"   # bootstrap a bundle
make build GAME=<name>       # build
make run   GAME=<name>       # build + run headless
make play  GAME=<name>       # SDL viewer
```

## Further reading

- [Architecture and implementation overview](docs/architecture.md)
- [Runtime memory model](docs/runtime_memory_model.md)
- [Driving a program programmatically](docs/playing.md)
- [Writing patch bundles](patches/README.md)
