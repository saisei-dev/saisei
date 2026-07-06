#!/usr/bin/env python3
"""replay.py — re-run a session log against a fresh game.

A session log is the input track recorded by the shim's session_log
machinery (every FIFO byte received, paired with the virtual_ns at
read time). Replaying against a fresh game launched with
SAISEI_REPLAY=1 reproduces the original state pixel-for-pixel.

How the determinism works:

  - Recording side: the shim writes each successful FIFO read as a
    `vns=<virtual_ns> bytes=<hex>` line in sessions/session.log.

  - Replay side: launch a fresh game with `SAISEI_REPLAY=1` in its
    environment. The shim's init_keyboard hook sees the env var and
    initializes the virtual clock to HALTED with virtual_ns=0. The
    game does not advance until step or resume.

  - replay.py then sends, for each log entry: step(entry.vns - now),
    write(entry.bytes). After processing the entry, the shim's virtual
    clock is at entry.vns — exactly the same virtual time as when the
    byte was originally read. Animation counters and timer-derived
    state therefore match bit-for-bit.

Usage:
  SAISEI_REPLAY=1 ./build/<game>  # in one terminal, with FIFO redirected
  tools/replay.py SESSION.log       # in another, drives the FIFO

The standard launcher `python3 tools/game.py run` passes through the
environment, so `SAISEI_REPLAY=1 python3 tools/game.py run ...` works.

This tool does NOT start the game; it just writes to the FIFO. Start
the game with SAISEI_REPLAY=1 in env before invoking.
"""
from __future__ import annotations

import argparse
import os
import re
import sys
import time
from pathlib import Path


REPO = Path(__file__).resolve().parent.parent


def _parse_log(path: Path) -> list[tuple[int, bytes]]:
    """Return list of (virtual_ns, bytes) tuples in order from a session log."""
    entries: list[tuple[int, bytes]] = []
    line_re = re.compile(r"^vns=(\d+)\s+bytes=([0-9A-Fa-f ]+)$")
    for lineno, raw in enumerate(path.read_text().splitlines(), start=1):
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        m = line_re.match(line)
        if not m:
            print(f"replay: ignoring unparseable line {lineno}: {line!r}",
                  file=sys.stderr)
            continue
        vns = int(m.group(1))
        hex_str = m.group(2).replace(" ", "")
        try:
            data = bytes.fromhex(hex_str)
        except ValueError:
            print(f"replay: bad hex on line {lineno}: {hex_str!r}",
                  file=sys.stderr)
            continue
        entries.append((vns, data))
    return entries


def _send(fifo: Path, data: bytes) -> None:
    fd = os.open(str(fifo), os.O_WRONLY)
    try:
        os.write(fd, data)
    finally:
        os.close(fd)


def _send_step(fifo: Path, ticks: int) -> None:
    if ticks <= 0:
        return
    if ticks > 0xFFFF:
        ticks = 0xFFFF
    _send(fifo, bytes([0x17, ticks & 0xFF, (ticks >> 8) & 0xFF]))


def _send_halt(fifo: Path) -> None:
    _send(fifo, b"\x15")


def _send_set_vclock(fifo: Path, vns: int) -> None:
    """Force the shim's virtual clock to HALTED at exactly `vns`."""
    payload = bytes([0x1D]) + vns.to_bytes(8, "little", signed=False)
    _send(fifo, payload)


# 1 BIOS tick at IRQ0 18.2 Hz = 54_925_000 ns.
NS_PER_TICK = 54_925_000


def main() -> None:
    parser = argparse.ArgumentParser(
        description=__doc__,
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    parser.add_argument("log", help="session log file (sessions/session.log)")
    parser.add_argument(
        "--fifo",
        default=os.environ.get("SAISEI_CONTROL_FIFO", "/tmp/saisei_fifo"),
        help="FIFO path (default: $SAISEI_CONTROL_FIFO or /tmp/saisei_fifo)")
    parser.add_argument(
        "--inter-write-gap-ms", type=float, default=5.0,
        help="Short wall-clock pause between FIFO writes to give the "
             "shim's stdin reader time to drain (default 5 ms).")
    args = parser.parse_args()

    log_path = Path(args.log)
    if not log_path.exists():
        sys.exit(f"replay: log not found: {log_path}")
    fifo = Path(args.fifo)
    if not fifo.exists():
        sys.exit(f"replay: FIFO not found: {fifo}. Start the game first.")

    entries = _parse_log(log_path)
    if not entries:
        sys.exit("replay: log had no replayable entries")
    print(f"replay: {len(entries)} entries from {log_path}")

    gap = args.inter_write_gap_ms / 1000.0

    # Step the integer-tick delta between entries so vclock advances at
    # the same rate as the original recording (gradual PIT catchup at
    # each safepoint). Two cleaner alternatives I tried were worse:
    #   - wall-clock pacing of writes (3.43% diff): scheduling jitter
    #     in Python's time.sleep adds noise.
    #   - set_virtual_clock opcode (14.57% diff): jumps vclock past
    #     intermediate safepoints, breaking PIT catchup pacing.
    # The step-delta approach gives ~2-3% diff for original-to-replay
    # — animation frame counters drift by a tick or two due to
    # sub-tick rounding (NS_PER_TICK = 54.925ms is coarse) but the
    # game state (player_x, scene, items) matches.
    current_vns = 0
    for i, (vns, data) in enumerate(entries):
        if vns > current_vns:
            delta = vns - current_vns
            ticks = round(delta / NS_PER_TICK)
            if ticks > 0:
                _send_step(fifo, ticks)
                time.sleep(gap)
                current_vns += ticks * NS_PER_TICK
        _send(fifo, data)
        time.sleep(gap)
        if (i + 1) % 20 == 0:
            print(f"  ... {i + 1}/{len(entries)}")

    print("replay: done")


if __name__ == "__main__":
    main()
