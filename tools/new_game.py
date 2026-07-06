#!/usr/bin/env python3
"""Bootstrap a new game bundle from an archive.

The platform's front door: turn *a link to a game archive* plus *one choice
(which executable starts the game)* into a ready-to-reconstruct bundle under
games/<name>/.

Why only one human input is needed -- the JIT-only config has just two kinds
of fields:

  * INPUT     program_path ... the one semantic choice (the entry exe)
  * DERIVED   name, runtime ... from the archive

There is no binaries list, entry symbol, or call-target table: the runtime takes
the entry cs:ip from the MZ header and JIT-compiles every reached segment on
demand. So this tool fetches + extracts the archive, detects the executables,
asks which one boots the game (only when it can't tell), writes the seed config,
and runs one probe build so you can see it link.

Usage:
    python -m tools.new_game <url|zip|dir> [--exe NAME] [--name NAME] [--no-probe]
"""
from __future__ import annotations

import argparse
import shutil
import sys
import tempfile
import urllib.parse
import urllib.request
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
GAMES_DIR = ROOT / "games"

try:
    from compiler.utils import sanitize_identifier
except ImportError:  # pragma: no cover - allow running as a script
    sys.path.insert(0, str(ROOT))
    from compiler.utils import sanitize_identifier

# Files that are never game runtime assets (docs / installer cruft).
_SKIP_EXT = {".txt", ".md", ".bat", ".diz", ".nfo"}

# Archive-packaging cruft that should never count as game content. These defeat
# the "single wrapping directory" heuristic (a __MACOSX/ sibling makes the
# top level look like it has two entries) and pollute the runtime file list.
_JUNK_NAMES = {"__macosx", ".ds_store", "thumbs.db", ".directory"}


def _is_junk(p: Path) -> bool:
    n = p.name.lower()
    return n in _JUNK_NAMES or n.startswith("._")


def _prune_junk(root: Path) -> None:
    """Delete archive-packaging cruft anywhere under *root*."""
    # Deepest-first so directories are emptied before removal.
    for p in sorted(root.rglob("*"), key=lambda x: len(x.parts), reverse=True):
        if not _is_junk(p):
            continue
        if p.is_dir():
            shutil.rmtree(p, ignore_errors=True)
        elif p.exists():
            p.unlink()


def fetch(src: str, workdir: Path) -> Path:
    """Resolve *src* (http(s) URL, local .zip, or local dir) to a local path."""
    if src.startswith(("http://", "https://")):
        name = Path(urllib.parse.urlparse(src).path).name or "archive.zip"
        dest = workdir / name
        print(f"fetch: downloading {src}")
        urllib.request.urlretrieve(src, dest)  # noqa: S310 - user-provided URL
        return dest
    p = Path(src).expanduser()
    if not p.exists():
        raise SystemExit(f"new-game: not found: {src}")
    return p


def _flatten_wrappers(dest: Path) -> None:
    """Drop a top-level wrapper directory when the archive holds nothing else.

    The common ``archive.zip -> archive/ -> files`` layout (and nested chains of
    such single-folder wrappers) leaves the game one or more directories deep,
    so the executable scan finds only an empty top level. Collapse it: while the
    top level's only real entry is a single directory, move that directory's
    contents up and remove it. Junk has already been pruned, so a ``__MACOSX/``
    sibling no longer makes the level look non-empty. Stops as soon as the top
    level holds a file or more than one entry -- we only ever drop a *pure*
    wrapper, never restructure real game content."""
    while True:
        entries = [e for e in dest.iterdir() if not _is_junk(e)]
        if len(entries) != 1 or not entries[0].is_dir():
            break
        sub = entries[0]
        # Rename first so a child sharing the wrapper's name can't collide.
        tmp = dest / (sub.name + ".__unwrap__")
        sub.rename(tmp)
        for item in list(tmp.iterdir()):
            shutil.move(str(item), str(dest / item.name))
        tmp.rmdir()


def extract_into(archive: Path, dest: Path) -> None:
    """Extract *archive* (zip), copy (dir), or place (single file) into *dest*.

    Prunes packaging cruft (__MACOSX/, .DS_Store, ...) then drops a pure
    top-level wrapper directory so the game's files sit directly in *dest*."""
    dest.mkdir(parents=True, exist_ok=True)
    if archive.is_dir():
        for item in archive.iterdir():
            target = dest / item.name
            if item.is_dir():
                shutil.copytree(item, target, dirs_exist_ok=True)
            else:
                shutil.copy2(item, target)
    elif zipfile.is_zipfile(archive):
        with zipfile.ZipFile(archive) as z:
            z.extractall(dest)
    else:
        shutil.copy2(archive, dest / archive.name)

    _prune_junk(dest)
    _flatten_wrappers(dest)


def is_executable(p: Path) -> bool:
    if p.suffix.lower() in (".exe", ".com"):
        return True
    try:
        with open(p, "rb") as f:
            return f.read(2) == b"MZ"
    except OSError:
        return False


class _cooked_terminal:
    """Force the controlling terminal into canonical ("cooked") mode for the
    duration of a prompt, then restore it.

    Earlier commands (the game, the FIFO control tools) can leave the tty in
    raw mode, where Enter sends a bare CR that input() never sees as a line
    end -- the user types a number, presses Enter, and just gets a literal
    ^M. Re-enabling ICANON/ECHO/ICRNL around the prompt makes it work no
    matter what state the terminal was left in. No-op when stdin is not a tty
    or termios is unavailable (e.g. Windows)."""

    def __enter__(self):
        self._saved = None
        try:
            import termios

            self._termios = termios
            fd = sys.stdin.fileno()
            self._fd = fd
            self._saved = termios.tcgetattr(fd)
            attrs = termios.tcgetattr(fd)
            attrs[0] |= termios.ICRNL          # map CR -> NL on input
            attrs[3] |= termios.ICANON | termios.ECHO | termios.ECHOE
            termios.tcsetattr(fd, termios.TCSANOW, attrs)
        except Exception:
            self._saved = None
        return self

    def __exit__(self, *exc):
        if self._saved is not None:
            try:
                self._termios.tcsetattr(
                    self._fd, self._termios.TCSANOW, self._saved
                )
            except Exception:
                pass
        return False


def choose_entry(execs: list[Path], requested: str | None) -> Path:
    """Pick the entry executable: --exe, the only one, or an interactive ask."""
    by_name = {e.name.lower(): e for e in execs}
    if requested:
        e = by_name.get(requested.lower())
        if not e:
            raise SystemExit(
                f"new-game: --exe {requested!r} not among executables: "
                + ", ".join(e.name for e in execs)
            )
        return e
    if len(execs) == 1:
        return execs[0]
    if not execs:
        raise SystemExit("new-game: no executables (.exe/.com/MZ) found in archive")
    if not sys.stdin.isatty():
        listing = ", ".join(e.name for e in execs)
        raise SystemExit(
            "new-game: multiple executables found; rerun with --exe NAME.\n"
            f"  candidates: {listing}"
        )
    print("\nWhich executable starts the game?")
    for i, e in enumerate(execs, 1):
        print(f"  {i}. {e.name}  ({e.stat().st_size} bytes)")
    # Accept either the list number or the executable name (case-insensitive),
    # and tolerate a stray CR/whitespace from a misbehaving terminal.
    with _cooked_terminal():
        while True:
            try:
                raw = input("Enter number (or exe name): ").strip().strip("\r")
            except EOFError:
                raise SystemExit(
                    "new-game: no input; rerun with --exe NAME to pick "
                    "non-interactively."
                )
            if raw.isdigit() and 1 <= int(raw) <= len(execs):
                return execs[int(raw) - 1]
            if raw.lower() in by_name:
                return by_name[raw.lower()]
            print(f"  (invalid: type 1-{len(execs)} or one of: "
                  + ", ".join(e.name for e in execs) + ")")


def build_seed_config(name: str, exe: Path, game_dir: Path) -> dict:
    runtime = []
    # Recurse: a DOS game opens its assets by path (e.g. fopen("DATA\\GFX.DAT")),
    # so subdirectories are part of the data layout, not packaging. Preserve each
    # asset's path relative to the bundle as its dest -- the runtime resolves the
    # backslash path to that same subdir (resolve_case_insensitive_path), and the
    # copy step recreates the tree under build/<game>/.
    for f in sorted(game_dir.rglob("*"), key=lambda p: str(p).lower()):
        if not f.is_file() or _is_junk(f):
            continue
        if f.suffix.lower() in _SKIP_EXT:
            continue
        rel = f.relative_to(game_dir).as_posix()
        if rel == f"{name}.json":
            continue
        runtime.append({"source": f"games/{name}/{rel}", "dest": rel})
    # JIT-only: the config just names the program image and the data files to
    # copy. The runtime takes the entry cs:ip from the MZ header and JIT-compiles
    # every reached segment on demand -- no binaries list, no entry symbol, no
    # call-target table to grow. A game needing a non-default PSP load segment
    # sets "psp_seg" here later.
    return {
        "name": name,
        "program_path": exe.name,
        "runtime": runtime,
    }


def probe(name: str) -> int:
    """Run one game-agnostic build pass and report how far it got."""
    import subprocess

    print(f"\nprobe: building bundle '{name}' (emit config + link runtime)...")
    proc = subprocess.run(
        [sys.executable, "-m", "tools.game", "build", name],
        cwd=ROOT,
    )
    if proc.returncode == 0:
        print(
            f"\nprobe: OK -- bundle '{name}' builds.\n"
            f"  next: python -m tools.game run {name} --headless   to see it boot.\n"
            f"  The JIT compiles the game's code on demand as it runs; watch the\n"
            f"  screenshots + trace to see how far it gets."
        )
    else:
        print(
            f"\nprobe: build stopped (exit {proc.returncode}) -- inspect the output\n"
            f"  above. A build failure here is a config/link error (the build only\n"
            f"  emits the config and links the runtime; no game code is decoded)."
        )
    return proc.returncode


def main() -> None:
    ap = argparse.ArgumentParser(
        prog="new-game", description="Bootstrap a game bundle from an archive."
    )
    ap.add_argument("archive", help="http(s) URL, local .zip, or local directory")
    ap.add_argument("--exe", help="entry executable name (skips the prompt)")
    ap.add_argument("--name", help="bundle name (default: archive stem)")
    ap.add_argument(
        "--no-probe", action="store_true", help="scaffold only; skip the build probe"
    )
    ap.add_argument(
        "--force", action="store_true", help="overwrite games/<name>/ if it exists"
    )
    args = ap.parse_args()

    name = sanitize_identifier(args.name or Path(args.archive.rstrip("/")).stem)
    game_dir = GAMES_DIR / name
    if game_dir.exists():
        if not args.force:
            raise SystemExit(
                f"new-game: games/{name}/ already exists (use --force to overwrite)"
            )
        shutil.rmtree(game_dir)

    with tempfile.TemporaryDirectory() as tmp:
        archive = fetch(args.archive, Path(tmp))
        print(f"extract: -> games/{name}/")
        extract_into(archive, game_dir)

    execs = sorted(
        (f for f in game_dir.iterdir() if f.is_file() and is_executable(f)),
        key=lambda p: p.name.lower(),
    )
    print(f"detect: {len(execs)} executable(s): " + ", ".join(e.name for e in execs))
    exe = choose_entry(execs, args.exe)

    config = build_seed_config(name, exe, game_dir)
    config_path = game_dir / f"{name}.json"
    import json

    config_path.write_text(json.dumps(config, indent=2) + "\n")
    print(
        f"scaffold: wrote {config_path.relative_to(ROOT)}\n"
        f"  program_path  = {config['program_path']}\n"
        f"  entry_symbol  = {config['entry_symbol']}\n"
        f"  binaries      = {len(config['binaries'])} (entry exe; loop adds more)\n"
        f"  runtime files = {len(config['runtime'])}"
    )

    if args.no_probe:
        return
    sys.exit(probe(name))


if __name__ == "__main__":
    main()
