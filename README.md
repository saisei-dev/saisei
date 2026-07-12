# Saisei

**再生 — "rebirth."** Take a DOS game you loved, and bring it back to life as real, native code.

The classics you grew up with are frozen in old binaries — playable only through an emulator, and a black box even then. Saisei thaws them out. As a game runs, Saisei decompiles it into readable Rust, compiles that with rustc, and runs *it*. The translated Rust **is** the game. No emulator sits in the middle — and nothing stays a black box.

So a game stops being a sealed artifact and becomes something you can open up:

- ▶️ **Play** — run the classics as fast, native programs.
- 🔍 **Explore** — read the Rust Saisei generates and finally see how your favorite game actually works.
- 🎨 **Remix** *(WIP)* — swap the art, rescore the music, rewrite the gameplay, and ship your own cut.

Packed, overlay-swapped, and self-modifying games all just work, because Saisei only ever compiles the bytes that are about to run. Old games don't have to stay frozen — give them a second life.

## Start playing

Two things to install — SDL2, for the window, and Rust. Then build:

```bash
# Debian/Ubuntu:  sudo apt install build-essential libsdl2-dev
# macOS:          xcode-select --install && brew install sdl2
# Rust:           https://rustup.rs

cargo build --release                      # fetches the pinned toolchain itself
export PATH="$PWD/target/release:$PATH"    # or just run target/release/saisei
```

Then bring a game. Grab one from an abandonware archive like [My Abandonware](https://www.myabandonware.com/) and hand Saisei the link:

```bash
saisei new-game "https://.../coolgame.zip"
saisei play coolgame
```

`new-game` downloads and unpacks the zip — and asks which executable to run if there's more than one. `play` opens the game in a window, compiling each part to native code the moment control reaches it. No config files, no flags to weigh.

Two more commands for when you need them: `saisei run <name> --headless` runs without a window (for scripting and CI), and `saisei build <name>` compiles without running. Run `saisei help` for the full command list. To drive a game from a script — keystrokes, screenshots, deterministic replay — see [Driving a program](docs/playing.md).

## Where Saisei is going

Playing is the front door. The point is to make these games **open to tinker with** again — that's the direction:

- 🤖 **Explore a game with an AI agent** *(WIP)* — turn the generated Rust into a map of how a game works. *(guide coming soon)*
- 🩹 **Write patches** *(WIP)* — small, shareable mods that hook a game's own functions with no source changes. → [patch bundles](patches/README.md)
- 🎨 **Replace art, music, and gameplay** *(WIP)* — swap a game's resources and behavior while it runs. *(coming soon)*
- ❄️ **Freeze to a standalone binary** *(WIP)* — collect the compiled pieces into one native executable for the target of your choice: desktop (Linux, macOS, Windows) and eventually mobile (Android, iOS), with no compiler required to run it. Your childhood DOS game, native on your phone. *(coming soon)*

*(WIP = work in progress. These are the direction, not promises with dates — links land here as each one ships.)*

## How it works

At run time Saisei loads the program image, takes the entry point from the MZ header, and JIT-compiles each code segment the first time control reaches it: decode → lossless IR → Rust → rustc → `dlopen`. Nothing is decoded ahead of time, and compiled chunks are keyed by the bytes that are actually live — so a decompressed or overlaid region is recompiled from whatever is really there. The full design is in the [architecture overview](docs/architecture.md).

## Tested games

Saisei's x86, BIOS, and DOS emulation is **still being built out, and many games do not work yet** — they hang, glitch, or stop on something we haven't implemented. These are the ones known to play:

- **Zeliard**
- **Prince of Persia**
- **Dungeon Master**
- **Alley Cat**

Every game you try is a test case, so please tell us either way: if one works, report it and we'll add it to this list; if one breaks, [open an issue](https://github.com/saisei-dev/saisei/issues) with the game and how far it got. A game that stops on an unimplemented port or an unfaithful instruction is exactly the signal that pushes the emulation forward.

## Contributing

Saisei is early and there's plenty to build — the dev setup, the design docs, and how to report a game are in [CONTRIBUTING.md](CONTRIBUTING.md).

## License

Saisei is released under the MIT License — see [LICENSE](LICENSE). The license covers the recompiler and runtime only. The DOS games you run through it are yours; you are responsible for having the right to use them, and none are ever included here.
