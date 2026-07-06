#!/usr/bin/env python3
"""control.py — drive an emulated game over its stdin control FIFO.

The FIFO is owned by the running game process (see docs/playing.md for
the launch recipe). This tool just attaches and writes the byte protocol
the shim reads in safe_point_impl. It does NOT start, stop, build, or
restart the game — that's intentional, so the same control CLI works
whether the game is running in the foreground (a terminal you can see)
or the background (CI / drive scripts).

Commands:
  tap <key> [ticks]      Press + scheduled release after `ticks` BIOS
                         IRQ0 ticks (default 10). Deterministic across
                         emulator speeds — see docs/playing.md.
  press <key>            Press only. Game sees the key as held until
                         a matching `release`.
  release <key>          Release a previously held key.
  enter [count=1]        Send the auto-paired Enter byte (0x0D) `count`
                         times, with a short gap between them so menus
                         can settle.
  space [count=1]        Same idea but for the in-game dialog-advance
                         Space tap (3 ticks each, per playing.md).
  shot [--out PATH]      Capture a screenshot. Resolves which file the
                         shim just wrote (highest-numbered screenshot
                         in the runtime dir) and prints its absolute
                         path. With --out, copies it there.
  raw <hex-bytes>        Write arbitrary hex (e.g. `10 4D` = press 0x4D).
                         Useful when iterating on a new opcode.
  halt                   Freeze the game's virtual clock. Wall time
                         continues; the game does not. Idempotent.
  resume                 Resume the virtual clock. Idempotent.
  step <ticks>           Advance virtual time by exactly N BIOS ticks
                         (default 1) and then halt. Deterministic
                         single-step primitive — closed-loop drivers
                         use `step N; shot; decide; step N; ...`.
  read <addr> [len]      Read `len` (default 1, max 255) bytes from
                         linear address `addr` in the game's RAM.
                         Writes them to snapshots/last_read.bin in the
                         runtime dir and prints the resolved path.
                         `addr` accepts decimal or 0xHEX.
  snapshot [--out PATH]  Dump full 2 MB RAM to a counter-named file in
                         snapshots/ inside the runtime dir. Prints the
                         resolved path; with --out, also copies there.
  status                 Print FIFO path + whether anything seems to
                         be reading it (best-effort via `fuser`).

Key names accepted by tap/press/release:
  up down left right space enter esc tab backspace
  a..z, 0..9
  0xHH                   raw 7-bit scancode

Globals:
  --fifo PATH            FIFO to write to. Default: /tmp/saisei_fifo.
                         Also honoured via $SAISEI_CONTROL_FIFO.
  --shots-dir PATH       Screenshot directory. Default: derived from
                         the FIFO name (`<runtime>/screenshots/`) or
                         the env $SAISEI_CONTROL_SHOTS.
  --gap MS               Inter-byte gap for repeated keys. Default 80.
"""

from __future__ import annotations

import argparse
import os
import shutil
import time
from pathlib import Path


# Scancode table — same mapping the shim uses internally.
SCANCODES = {
    "up": 0x48, "down": 0x50, "left": 0x4B, "right": 0x4D,
    "space": 0x39, "enter": 0x1C, "esc": 0x01,
    "tab": 0x0F, "backspace": 0x0E,
}
ASCII_TO_SCAN = {
    **{c: 0x1E + i for i, c in enumerate("abcdefghijklmnopqrstuvwxyz")},
    "0": 0x0B,
    **{str(d): 0x02 + d - 1 for d in range(1, 10)},
}


def resolve_key(name: str) -> int:
    """Map a friendly key name to a 7-bit DOS scancode."""
    n = name.lower()
    if n in SCANCODES:
        return SCANCODES[n]
    # Lowercase letter / digit.
    if n in ASCII_TO_SCAN:
        return ASCII_TO_SCAN[n]
    # Raw hex (0xHH or just HH).
    try:
        v = int(n, 16) if n.startswith("0x") else int(n, 16)
    except ValueError:
        raise SystemExit(
            f"control: unknown key {name!r}. Use up/down/left/right/"
            f"space/enter/esc/tab/backspace, a-z, 0-9, or 0xHH."
        )
    if not (1 <= v <= 0x7F):
        raise SystemExit(f"control: scancode 0x{v:X} out of 7-bit range")
    return v


def open_fifo(path: Path) -> int:
    if not path.exists():
        raise SystemExit(
            f"control: FIFO {path} doesn't exist. The game must be running "
            f"with stdin redirected from this FIFO — see docs/playing.md."
        )
    try:
        return os.open(str(path), os.O_WRONLY)
    except OSError as exc:
        raise SystemExit(f"control: cannot open {path} for write: {exc}")


def send_bytes(fifo: Path, payload: bytes) -> None:
    """Atomic write to the FIFO. Writes <= PIPE_BUF are atomic on Linux."""
    fd = open_fifo(fifo)
    try:
        os.write(fd, payload)
    finally:
        os.close(fd)


def cmd_tap(args, fifo: Path) -> None:
    sc = resolve_key(args.key)
    ticks = args.ticks if args.ticks > 0 else 1
    if ticks > 0xFFFF:
        raise SystemExit("control: ticks must fit in 16 bits")
    payload = bytes([0x12, sc, ticks & 0xFF, (ticks >> 8) & 0xFF])
    send_bytes(fifo, payload)


def cmd_press(args, fifo: Path) -> None:
    sc = resolve_key(args.key)
    send_bytes(fifo, bytes([0x10, sc]))


def cmd_release(args, fifo: Path) -> None:
    sc = resolve_key(args.key)
    send_bytes(fifo, bytes([0x11, sc]))


def cmd_enter(args, fifo: Path) -> None:
    gap = args.gap / 1000.0
    fd = open_fifo(fifo)
    try:
        for i in range(args.count):
            os.write(fd, b"\r")
            if i + 1 < args.count:
                time.sleep(gap)
    finally:
        os.close(fd)


def cmd_space(args, fifo: Path) -> None:
    """Space taps that the game's dialog driver expects (3 ticks)."""
    gap = args.gap / 1000.0
    fd = open_fifo(fifo)
    try:
        for i in range(args.count):
            os.write(fd, bytes([0x12, 0x39, 0x03, 0x00]))
            if i + 1 < args.count:
                time.sleep(gap)
    finally:
        os.close(fd)


def resolve_shots_dir(fifo: Path, override: str | None) -> Path:
    if override:
        return Path(override).resolve()
    env = os.environ.get("SAISEI_CONTROL_SHOTS")
    if env:
        return Path(env).resolve()
    # FIFO name is typically /tmp/<game>_fifo → assume <project>/build/<game>/screenshots/
    project = Path(__file__).resolve().parent.parent
    name = fifo.name.removesuffix("_fifo")
    if not name:
        name = "game"
    return (project / "build" / name / "screenshots").resolve()


def cmd_shot(args, fifo: Path) -> None:
    shots = resolve_shots_dir(fifo, args.shots_dir)
    shots.mkdir(parents=True, exist_ok=True)
    before = {p.name: p.stat().st_mtime
              for p in shots.glob("screenshot*.png") if p.is_file()}
    send_bytes(fifo, b"\x14")

    # Poll for a new or freshly-modified screenshot file.
    deadline = time.monotonic() + args.timeout
    new_path: Path | None = None
    while time.monotonic() < deadline:
        for p in shots.glob("screenshot*.png"):
            if not p.is_file():
                continue
            m = p.stat().st_mtime
            prev = before.get(p.name)
            if prev is None or m > prev + 0.001:
                new_path = p
                break
        if new_path:
            break
        time.sleep(0.05)

    if not new_path:
        raise SystemExit(
            f"control: no screenshot appeared in {shots} within "
            f"{args.timeout}s. The game may not be processing input "
            f"right now (paused on a SAFEPOINT-less code path, or "
            f"stdin gating is off)."
        )

    if args.out:
        dst = Path(args.out).resolve()
        dst.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(new_path, dst)
        print(str(dst))
    else:
        print(str(new_path.resolve()))


def cmd_halt(args, fifo: Path) -> None:
    send_bytes(fifo, b"\x15")


def cmd_resume(args, fifo: Path) -> None:
    send_bytes(fifo, b"\x16")


def cmd_step(args, fifo: Path) -> None:
    ticks = args.ticks if args.ticks > 0 else 1
    if ticks > 0xFFFF:
        raise SystemExit("control: ticks must fit in 16 bits")
    send_bytes(fifo, bytes([0x17, ticks & 0xFF, (ticks >> 8) & 0xFF]))


def resolve_snapshots_dir(fifo: Path, override: str | None) -> Path:
    """Sibling of the screenshots dir — same `build/<game>/` parent."""
    if override:
        return Path(override).resolve()
    env = os.environ.get("SAISEI_CONTROL_SNAPSHOTS")
    if env:
        return Path(env).resolve()
    shots = resolve_shots_dir(fifo, None)
    return (shots.parent / "snapshots").resolve()


def _parse_addr(s: str) -> int:
    s = s.strip()
    if s.startswith("0x") or s.startswith("0X"):
        return int(s, 16)
    return int(s, 0)


def cmd_read(args, fifo: Path) -> None:
    addr = _parse_addr(args.addr)
    length = args.len
    if not (1 <= length <= 255):
        raise SystemExit("control: read len must be 1..255")
    if not (0 <= addr <= 0xFFFFFFFF) or addr + length > (1 << 21):
        raise SystemExit(
            f"control: addr out of bounds (RAM is 0..0x{(1 << 21) - 1:X})")
    snaps = resolve_snapshots_dir(fifo, args.snapshots_dir)
    snaps.mkdir(parents=True, exist_ok=True)
    target = snaps / "last_read.bin"
    before_mtime = target.stat().st_mtime if target.exists() else None

    payload = bytes([0x18,
                     addr & 0xFF, (addr >> 8) & 0xFF,
                     (addr >> 16) & 0xFF, (addr >> 24) & 0xFF,
                     length])
    send_bytes(fifo, payload)

    deadline = time.monotonic() + args.timeout
    while time.monotonic() < deadline:
        if target.exists():
            m = target.stat().st_mtime
            if before_mtime is None or m > before_mtime + 0.001:
                break
        time.sleep(0.02)
    else:
        raise SystemExit(
            f"control: read did not produce {target} within {args.timeout}s. "
            f"The game may not be processing input (try `halt` first)."
        )

    data = target.read_bytes()
    if args.out:
        dst = Path(args.out).resolve()
        dst.parent.mkdir(parents=True, exist_ok=True)
        dst.write_bytes(data)
        print(str(dst))
    else:
        print(f"{target} ({len(data)} bytes: {data.hex()})")


def cmd_snapshot(args, fifo: Path) -> None:
    snaps = resolve_snapshots_dir(fifo, args.snapshots_dir)
    snaps.mkdir(parents=True, exist_ok=True)
    before = {p.name for p in snaps.glob("snap_*.bin") if p.is_file()}
    send_bytes(fifo, b"\x1A")

    deadline = time.monotonic() + args.timeout
    new_path: Path | None = None
    while time.monotonic() < deadline:
        for p in snaps.glob("snap_*.bin"):
            if p.name not in before and p.is_file() and p.stat().st_size > 0:
                new_path = p
                break
        if new_path:
            break
        time.sleep(0.02)

    if not new_path:
        raise SystemExit(
            f"control: no new snapshot appeared in {snaps} within "
            f"{args.timeout}s. The game may not be processing input "
            f"(try `halt` first)."
        )

    if args.out:
        dst = Path(args.out).resolve()
        dst.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(new_path, dst)
        print(str(dst))
    else:
        print(str(new_path.resolve()))


def cmd_raw(args, fifo: Path) -> None:
    hex_str = "".join(args.bytes).replace(" ", "").replace("0x", "")
    try:
        data = bytes.fromhex(hex_str)
    except ValueError:
        raise SystemExit(f"control: bad hex {hex_str!r}")
    send_bytes(fifo, data)


def cmd_status(args, fifo: Path) -> None:
    print(f"fifo: {fifo}")
    print(f"exists: {fifo.exists()}")
    if not fifo.exists():
        return
    # Best-effort: who has the FIFO open. `fuser` may not be installed;
    # fall back to /proc walk.
    found_reader = False
    try:
        import subprocess
        r = subprocess.run(
            ["fuser", str(fifo)], capture_output=True, text=True, check=False
        )
        if r.stdout.strip():
            print(f"holders: {r.stdout.strip()}")
            found_reader = True
    except FileNotFoundError:
        pass
    if not found_reader:
        # Cheaper than walking /proc — best effort only.
        print("holders: (run `lsof " + str(fifo) + "` to check)")
    shots = resolve_shots_dir(fifo, args.shots_dir)
    if shots.is_dir():
        latest = sorted(shots.glob("screenshot*.png"),
                        key=lambda p: p.stat().st_mtime, reverse=True)
        if latest:
            print(f"latest shot: {latest[0]}")


def main() -> None:
    parser = argparse.ArgumentParser(
        description=__doc__,
        formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument(
        "--fifo",
        default=os.environ.get("SAISEI_CONTROL_FIFO", "/tmp/saisei_fifo"),
        help="FIFO path (default: /tmp/saisei_fifo or $SAISEI_CONTROL_FIFO)")
    parser.add_argument(
        "--shots-dir", default=None,
        help="Screenshot dir (default: derived from --fifo or "
             "$SAISEI_CONTROL_SHOTS)")
    parser.add_argument(
        "--snapshots-dir", default=None,
        help="RAM snapshot dir (default: sibling of shots dir, or "
             "$SAISEI_CONTROL_SNAPSHOTS)")
    parser.add_argument(
        "--gap", type=int, default=80,
        help="Inter-byte gap in ms for repeated keys (default: 80)")
    sub = parser.add_subparsers(dest="command", required=True)

    sp = sub.add_parser("tap", help="press + auto-release after N ticks")
    sp.add_argument("key")
    sp.add_argument("ticks", type=int, nargs="?", default=10)

    sp = sub.add_parser("press", help="press without release")
    sp.add_argument("key")
    sp = sub.add_parser("release", help="release a held key")
    sp.add_argument("key")

    sp = sub.add_parser("enter", help="Enter key, optionally repeated")
    sp.add_argument("count", type=int, nargs="?", default=1)

    sp = sub.add_parser("space", help="Space dialog-advance tap (3 ticks each)")
    sp.add_argument("count", type=int, nargs="?", default=1)

    sp = sub.add_parser("shot", help="screenshot now, print path")
    sp.add_argument("--out", default=None, help="copy the new shot here")
    sp.add_argument(
        "--timeout", type=float, default=3.0,
        help="seconds to wait for the file to appear (default 3)")

    sp = sub.add_parser("raw", help="send arbitrary hex bytes")
    sp.add_argument("bytes", nargs="+")

    sub.add_parser("halt", help="freeze virtual clock (game pauses)")
    sub.add_parser("resume", help="resume virtual clock")
    sp = sub.add_parser("step", help="advance N BIOS ticks then halt")
    sp.add_argument("ticks", type=int, nargs="?", default=1)

    sp = sub.add_parser("read", help="read bytes from RAM at linear addr")
    sp.add_argument("addr")
    sp.add_argument("len", type=int, nargs="?", default=1)
    sp.add_argument("--out", default=None, help="copy the bytes here")
    sp.add_argument("--timeout", type=float, default=3.0)

    sp = sub.add_parser("snapshot", help="dump full RAM to snapshots/snap_<N>.bin")
    sp.add_argument("--out", default=None, help="copy the snapshot here")
    sp.add_argument("--timeout", type=float, default=5.0)

    sub.add_parser("status", help="show FIFO + shots-dir state")

    args = parser.parse_args()
    fifo = Path(args.fifo)

    handlers = {
        "tap": cmd_tap, "press": cmd_press, "release": cmd_release,
        "enter": cmd_enter, "space": cmd_space, "shot": cmd_shot,
        "raw": cmd_raw, "halt": cmd_halt, "resume": cmd_resume,
        "step": cmd_step, "read": cmd_read, "snapshot": cmd_snapshot,
        "status": cmd_status,
    }
    handlers[args.command](args, fifo)


if __name__ == "__main__":
    main()
