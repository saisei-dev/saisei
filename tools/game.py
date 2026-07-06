#!/usr/bin/env python3
"""Orchestrate disassembly, compilation and execution for configured games."""
from __future__ import annotations

import argparse
import datetime
import json
import os
import shutil
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import List, Tuple

# Must match SLOT_COUNT in runtime/core/save_manager.c.

try:
    from compiler.utils import sanitize_identifier
except ImportError:  # pragma: no cover - allow running as a script
    from utils import sanitize_identifier

ROOT = Path(__file__).resolve().parent.parent
GAMES_DIR = ROOT / "games"
ARTIFACTS_DIR = ROOT / "build"


@dataclass
class GameDefinition:
    name: str           # bundle name
    key: str            # bundle key = shared working dir build/<key>/
    config_path: Path
    runtime: List[Tuple[str, str]]
    program_path: str
    program: str        # selected program name (e.g. "game" / "setup")
    program_key: str    # binary name within the bundle dir


def _load_json(path: Path) -> dict:
    with path.open("r", encoding="utf-8") as fh:
        return json.load(fh)


def _programs(data: dict) -> list[dict]:
    """Normalize a config into a list of program dicts.

    A bundle is one or more cooperating executables sharing the runtime data
    files and working dir (e.g. SETUP.EXE writes setup.dat that a relocated DOS EXE
    reads). A flat single-program config (no "programs") is treated as one
    implicit program named after the bundle."""
    progs = data.get("programs")
    if progs:
        return progs
    return [{
        "name": data.get("name") or "main",
        "program_path": data.get("program_path"),
    }]


def load_game_definition(name: str, program: str | None = None) -> GameDefinition:
    config_path = GAMES_DIR / name / f"{name}.json"
    if not config_path.exists():
        raise SystemExit(f"Unknown game '{name}'. Expected config at {config_path}")
    data = _load_json(config_path)

    progs = _programs(data)
    want = program or data.get("default_program") or progs[0].get("name")
    prog = next((p for p in progs if p.get("name") == want), None)
    if prog is None:
        names = ", ".join(p.get("name", "?") for p in progs)
        raise SystemExit(f"Game '{name}' has no program '{want}'. Available: {names}")

    runtime_entries = []
    for item in data.get("runtime", []):
        if not isinstance(item, dict) or "source" not in item:
            raise SystemExit(f"Invalid runtime entry in {config_path}: {item!r}")
        runtime_entries.append((item["source"],
                                item.get("dest") or Path(item["source"]).name))

    program_path = prog.get("program_path")
    if not program_path:
        raise SystemExit(f"Program '{want}' in '{name}' missing program_path")

    game_name = data.get("name") or config_path.stem
    return GameDefinition(
        name=game_name,
        key=sanitize_identifier(game_name),
        config_path=config_path,
        runtime=runtime_entries,
        program_path=program_path,
        program=want,
        program_key=sanitize_identifier(want),
    )


def run_subprocess(cmd: list[str], *, cwd: Path | None = None, env: dict | None = None) -> None:
    subprocess.run(cmd, cwd=cwd, env=env, check=True)


def generate_game_config(game: GameDefinition) -> Path:
    out_path = ARTIFACTS_DIR / f"{game.program_key}_game_config.c"
    cmd = [
        sys.executable,
        str(ROOT / "compiler" / "generate_game_config.py"),
        str(game.config_path),
        "--program", game.program,
        "--out",
        str(out_path),
    ]
    run_subprocess(cmd)
    return out_path


def _pkg_config(*args: str) -> list[str] | None:
    for tool in ("pkg-config", "pkgconf"):
        proc = subprocess.run(
            [tool, *args],
            check=False,
            capture_output=True,
            text=True,
        )
        if proc.returncode == 0:
            result = proc.stdout.strip()
            if result:
                return result.split()
            return []
    return None


def resolve_sdl_flags() -> tuple[list[str], list[str]]:
    cflags = _pkg_config("--cflags", "sdl2")
    libs = _pkg_config("--libs", "sdl2")
    if cflags is None:
        cflags = ["-I/opt/homebrew/include/SDL2", "-D_THREAD_SAFE"]
    if libs is None:
        libs = ["-L/opt/homebrew/lib", "-lSDL2"]
    return cflags, libs


def compile_game(game: GameDefinition, config_source: Path) -> Path:
    output_dir = ARTIFACTS_DIR / game.key
    output_dir.mkdir(parents=True, exist_ok=True)
    binary_path = output_dir / game.program_key
    obj_dir = output_dir / f"obj_{game.program_key}"
    obj_dir.mkdir(parents=True, exist_ok=True)

    # JIT-only: no binary is translated ahead of time, so there are no
    # per-module <module>.c sources to compile. The runtime loads the program
    # image and JIT-compiles every reached code segment on demand.
    sources: list[Path] = []

    cflags, libs = resolve_sdl_flags()
    # Stamp the runtime build with the current git revision so crash bundles
    # (manifest.json) map to an exact code version for submitted reports.
    try:
        version = subprocess.run(
            ["git", "rev-parse", "--short", "HEAD"], cwd=ROOT,
            capture_output=True, text=True, check=False,
        ).stdout.strip() or "unknown"
    except OSError:
        version = "unknown"
    # Honor a CFLAGS env var so callers/CI can inject extra defines
    # (e.g. CFLAGS=-DFORCE_EXIT_AFTER_10S for a self-terminating smoke run).
    extra_cflags = os.environ.get("CFLAGS", "").split()
    cc_flags = (
        ["-Iruntime/include", f"-I{output_dir}", "-O2", f'-DRUNTIME_VERSION="{version}"']
        + cflags
        + ["-pthread", "-DSDL_MAIN_HANDLED"]
        + extra_cflags
    )

    # Per-translation-unit compile to obj_dir/<name>.o, then link.
    # Common-headers staleness: any change under runtime/include/*.h
    # conservatively invalidates every .o. Refining to clang -MMD/-MF
    # (per-.o .d files with the exact include set) would be more surgical
    # but adds parsing — this coarse rule is fine while the generated .c
    # files all share the same small set of runtime headers.
    header_dirs = [ROOT / "runtime" / "include"]
    header_paths: list[Path] = []
    for d in header_dirs:
        if d.is_dir():
            header_paths.extend(sorted(d.glob("*.h")))
    headers_newest = max(
        (p.stat().st_mtime for p in header_paths if p.exists()),
        default=0.0,
    )

    # The runtime, by layer (mirrors RUNTIME_CSRCS in the Makefile).
    shim_sources = [
        ROOT / "runtime" / "core" / "shims.c",
        ROOT / "runtime" / "core" / "snapshot.c",
        ROOT / "runtime" / "core" / "save_manager.c",
        ROOT / "runtime" / "display" / "virtual_display_sdl.c",
        ROOT / "runtime" / "hw" / "io_bus.c",
        ROOT / "runtime" / "hw" / "audio.c",
        ROOT / "runtime" / "hw" / "video.c",
        ROOT / "runtime" / "hw" / "keyboard.c",
        ROOT / "runtime" / "hw" / "timer.c",
        ROOT / "runtime" / "os" / "dos.c",
        ROOT / "runtime" / "os" / "bios.c",
        ROOT / "runtime" / "os" / "mouse.c",
    ]
    all_srcs: list[Path] = list(sources) + shim_sources + [config_source]

    obj_paths: list[Path] = []
    for src in all_srcs:
        obj = obj_dir / (src.stem + ".o")
        obj_paths.append(obj)
        need_rebuild = (
            not obj.exists()
            or obj.stat().st_mtime < src.stat().st_mtime
            or obj.stat().st_mtime < headers_newest
        )
        if not need_rebuild:
            continue
        run_subprocess(
            ["clang", "-c", str(src), *cc_flags, "-o", str(obj)]
        )

    # Link — cheap relative to compiles, always re-do so the binary
    # picks up whichever .o's were just refreshed (and so a renamed
    # symbol elsewhere doesn't go undetected).
    #
    # -rdynamic exports the runtime's symbols (cpu, virtual_memory, the shim
    # functions) into the dynamic symbol table so JIT-compiled chunk .so's
    # (dlopen'd at runtime by the JIT fallback) resolve them to the one live
    # copy -- shared state, seamless continuation. -ldl for dlopen/dlsym.
    run_subprocess(
        ["clang", *[str(o) for o in obj_paths], *cc_flags,
         "-o", str(binary_path), *libs, "-pthread", "-rdynamic", "-ldl"]
    )
    return binary_path


def copy_runtime(game: GameDefinition) -> Path:
    output_dir = ARTIFACTS_DIR / game.key
    output_dir.mkdir(parents=True, exist_ok=True)
    for source, dest in game.runtime:
        src_path = ROOT / source
        if not src_path.exists():
            raise SystemExit(f"Runtime file not found: {src_path}")
        dest_path = output_dir / dest
        if src_path.is_dir():
            shutil.copytree(src_path, dest_path, dirs_exist_ok=True)
        else:
            dest_path.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(src_path, dest_path)
    return output_dir


def build(game: GameDefinition) -> Path:
    # JIT-only: nothing is decoded at build time. `build` just emits the
    # per-game config and links the runtime; the entry cs:ip comes from the
    # program image's MZ header and code is JIT-compiled on first reach.
    config_source = generate_game_config(game)
    binary_path = compile_game(game, config_source)
    return binary_path


def _sr_log(runtime_dir: Path, msg: str) -> None:
    """Append a one-line event to <runtime_dir>/save_restore.log. Mirrors
    save_manager_sr_log on the C side so save/restore activity from both
    sides shares one file."""
    try:
        ts = datetime.datetime.now(datetime.timezone.utc).strftime(
            "%Y-%m-%dT%H:%M:%SZ"
        )
        with (runtime_dir / "save_restore.log").open("a", encoding="utf-8") as fh:
            fh.write(f"[{ts}] {msg}\n")
    except OSError:
        pass


def run_game(
    game: GameDefinition,
    *,
    headless: bool,
    silent: bool,
    speedup: float,
    restore_from: str | None = None,
    trace_file: str | None = None,
    lifecycle_file: str | None = None,
) -> None:
    binary_path = build(game)
    runtime_dir = copy_runtime(game)

    cmd = [f"./{binary_path.name}"]
    if headless:
        cmd.append("--headless")
    cmd.extend(["--speedup", f"{speedup}"])
    if restore_from:
        # Resolve to an absolute path so the binary (running from
        # runtime_dir) can find the bundle regardless of where the user
        # passed it from.
        cmd.extend(["--restore-from", str(Path(restore_from).resolve())])

    env = os.environ.copy()
    # Enable the JIT (the whole pipeline): on a dispatch into any not-yet-
    # compiled code (the entry itself, or decompressed/overlay/self-modified
    # code), the runtime invokes compiler/jit_compile.py and clang to
    # build+dlopen a chunk. It needs the repo root (to find the compiler +
    # write build/<key>/jit) and a Python.
    env["SAISEI_REPO_ROOT"] = str(ROOT)
    env["SAISEI_PYTHON"] = sys.executable
    # Per-game JIT chunk dir so two bundles (e.g. two related program variants) that decode
    # the same seg:ip to DIFFERENT bytes never overwrite each other's chunk .so.
    env["SAISEI_JIT_DIR"] = str(ARTIFACTS_DIR / game.key / "jit")
    if silent:
        env["RUN_SILENT"] = "1"
    else:
        env.pop("RUN_SILENT", None)
    if trace_file:
        # Pass an absolute path so the binary (running from runtime_dir) finds
        # it regardless of where the user invoked the command.
        env["TRACE_FILE"] = str(Path(trace_file).resolve())
        # Auto-enable shim-side targeted instrumentation when collecting a
        # full-session trace. The hooks only fire on narrow addresses/lines,
        # so the noise cost is small and we save the user from having to
        # remember a second env var.
        env.setdefault("TRACE_DIGIT_HUNT", "1")
    if lifecycle_file:
        env["LIFECYCLE_FILE"] = str(Path(lifecycle_file).resolve())

    rc = 0
    _sr_log(runtime_dir,
            f"binary START cmd={' '.join(cmd)} restore_from={restore_from!r}")
    try:
        run_subprocess(cmd, cwd=runtime_dir, env=env)
    except subprocess.CalledProcessError as exc:
        rc = exc.returncode

    # Always log how the binary exited so silent terminations are visible.
    # Negative rc => killed by signal abs(rc). Positive => exit() with that
    # code. Zero => clean exit. This catches the cases the in-binary
    # [EXIT] markers can't (SIGKILL/SIGSTOP from OOM-killer or external).
    if rc == 0:
        _sr_log(runtime_dir, "binary EXIT_CLEAN rc=0 (exit code 0)")
    elif rc > 0:
        _sr_log(runtime_dir,
                f"binary EXIT_NONZERO rc={rc} (process called exit({rc}) — "
                f"check stderr for [EXIT] / [BUG] markers)")
    else:
        import signal as _signal
        try:
            sig_name = _signal.Signals(-rc).name
        except (ValueError, AttributeError):
            sig_name = f"signal_{-rc}"
        _sr_log(runtime_dir,
                f"binary KILLED_BY_SIGNAL rc={rc} signal={-rc} ({sig_name}) — "
                f"NO catchable handler ran. If SIGKILL/SIGSTOP, the process "
                f"was killed externally (OOM-killer, shell, or kernel). "
                f"Check `dmesg | tail` for OOM. If SIGSEGV/SIGABRT in the "
                f"signal handler itself, the second signal bypassed "
                f"SA_RESETHAND-restored default disposition.")

    # Exit cleanly with the child's status instead of raising — the
    # Python traceback would otherwise scroll the child's crash
    # diagnostic out of the visible tmux pane.
    sys.exit(rc)


def _parse_run_unknown_args(
    parser: argparse.ArgumentParser,
    args: argparse.Namespace,
    unknown: list[str],
) -> None:
    """Handle backward-compatible run flags that may arrive as unknown args."""
    if args.command != "run":
        if unknown:
            parser.error(f"unrecognized arguments: {' '.join(unknown)}")
        return

    idx = 0
    while idx < len(unknown):
        token = unknown[idx]
        if token.startswith("--speedup="):
            value = token.split("=", 1)[1]
        elif token == "--speedup":
            if idx + 1 >= len(unknown):
                parser.error("argument --speedup: expected one argument")
            idx += 1
            value = unknown[idx]
        else:
            parser.error(f"unrecognized arguments: {' '.join(unknown[idx:])}")

        try:
            args.speedup = float(value)
        except ValueError:
            parser.error(f"argument --speedup: invalid float value: '{value}'")
        idx += 1


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)

    build_p = sub.add_parser("build", help="Emit the config and link the runtime")
    build_p.add_argument("game", help="Game configuration name")
    build_p.add_argument(
        "--program", default=None,
        help="Program (sub-executable) in a multi-program "
        "bundle, e.g. setup; default: default_program")

    def _add_run_flags(p: argparse.ArgumentParser, *, with_headless: bool) -> None:
        p.add_argument(
            "--program", default=None,
            help="Program (sub-executable) in a multi-program "
            "bundle, e.g. setup; default: default_program")
        if with_headless:
            p.add_argument(
                "--headless", action="store_true",
                help="Run without opening the SDL window",
            )
        p.add_argument(
            "--silent", action="store_true",
            help="Suppress shim stdout logging",
        )
        p.add_argument(
            "--speedup", type=float, default=1.0,
            help="Emulation speed multiplier "
                 "(e.g. 2.0 runs game logic twice as fast)",
        )
        p.add_argument(
            "--restore-from", metavar="BUNDLE_DIR", default=None,
            help="Resume from the pre_last_key snapshot in BUNDLE_DIR (path "
                 "to a crashes/crash_* directory). Requires the snapshot to "
                 "have lcall_depth=0 and isr_depth=0.",
        )
        p.add_argument(
            "--trace-file", metavar="PATH", default=None,
            help="Stream every trace line to PATH (full session, not just "
                 "the ring tail). Useful for investigating long flows.",
        )
        p.add_argument(
            "--lifecycle-file", metavar="PATH", default=None,
            help="Stream lifecycle events (LOAD/CALL/JMP/LJMP/LCALL/NRET) to "
                 "PATH for long sessions. Lifecycle is always captured to an "
                 "in-memory ring and dumped to every crash bundle as "
                 "lifecycle.log automatically; this flag only adds disk "
                 "streaming for runs that outlast the ring.",
        )

    run_p = sub.add_parser("run", help="Build and run a game (headless-capable)")
    run_p.add_argument("game", help="Game configuration name")
    _add_run_flags(run_p, with_headless=True)

    play_p = sub.add_parser("play", help="Build and run a game in the SDL window")
    play_p.add_argument("game", help="Game configuration name")
    _add_run_flags(play_p, with_headless=False)

    copy_p = sub.add_parser("copy-runtime", help="Copy runtime assets for a game")
    copy_p.add_argument("game", help="Game configuration name")
    copy_p.add_argument(
        "--program", default=None,
        help="Program (sub-executable) in a multi-program "
        "bundle, e.g. setup; default: default_program")

    new_p = sub.add_parser(
        "new-game",
        help="Bootstrap a game bundle from an archive (URL, .zip, or dir)")
    new_p.add_argument(
        "new_game_args", nargs=argparse.REMAINDER,
        help="Arguments forwarded to the bundler, e.g. <archive> --exe FOO.EXE")

    args, unknown = parser.parse_known_args()

    if args.command == "new-game":
        from tools import new_game
        sys.argv = ["saisei new-game", *args.new_game_args]
        new_game.main()
        return

    _parse_run_unknown_args(parser, args, unknown)
    game = load_game_definition(args.game, program=getattr(args, "program", None))

    if args.command == "build":
        build(game)
    elif args.command in ("run", "play"):
        if args.speedup <= 0:
            raise SystemExit("--speedup must be a positive number")
        run_game(
            game,
            headless=getattr(args, "headless", False),
            silent=args.silent,
            speedup=args.speedup,
            restore_from=getattr(args, "restore_from", None),
            trace_file=getattr(args, "trace_file", None),
            lifecycle_file=getattr(args, "lifecycle_file", None),
        )
    elif args.command == "copy-runtime":
        copy_runtime(game)
    else:
        raise SystemExit(f"Unsupported command {args.command}")


def _cli() -> None:
    """Console-script entry point (the ``saisei`` command).

    Wraps ``main`` so the installed command behaves exactly like
    ``python3 tools/game.py`` — same clean handling of a failed subprocess.
    """
    try:
        main()
    except subprocess.CalledProcessError as exc:
        # A subprocess (disasm, ir_to_c, clang, or the game binary itself)
        # already printed its own error block. Don't dump a Python traceback
        # on top of it — that just buries the real message.
        sys.exit(exc.returncode if exc.returncode else 1)


if __name__ == "__main__":
    _cli()
