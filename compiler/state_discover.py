#!/usr/bin/env python3
"""state_discover.py — find game-state addresses by snapshot diffing.

Workflow:
  1. control.py halt
  2. control.py snapshot                          # S0 = baseline
  3. <perform some action>                        # tap+step, or input the
                                                  # player makes only when a
                                                  # specific variable should
                                                  # change
  4. control.py halt; control.py snapshot         # S1 = after action
  5. state_discover.py diff S0 S1                 # see all bytes that changed

Refinement subcommands:
  changed S0 S1 [S2 S3 ...]
      Addresses where every adjacent pair differs. Use when an action is
      repeated and the variable is expected to change each time (e.g. tick
      counter, walk-right repeated 3x).
  reverted S0 S1 [S2 ...]
      Addresses that differ from S0 in S1 but match S0 again in some later
      Sn — i.e. the action was un-done. The bread-and-butter filter for
      "player_x: walked right then walked back".
  constant S0 S1 [S2 ...]
      Addresses that are identical across every snapshot. Useful as a
      sanity check ("did the snapshots actually come from a paused game?")
      and to subtract from candidate sets.
  monotonic S0 S1 S2 [S3 ...]
      Addresses where the (little-endian, width-typed) value strictly
      increases at every step, OR strictly decreases at every step. The
      strongest filter for finding coordinates and counters under
      repeated same-direction input. Pair with `reverted` (using a
      reverse-action sequence) to fully confirm.

All addresses are linear (0..0x1FFFFF for the 2 MB virtual RAM). Output is
one candidate per line, sorted, with the byte values from each snapshot in
the order given. Pass --max N to cap output (default 200) and --width
{1,2,4} to group bytes as 8/16/32-bit values for readability.
"""
from __future__ import annotations

import argparse
import sys
from pathlib import Path


RAM_SIZE = 1 << 21  # 2 MB, matches SHIM_MEMORY_SIZE


def _load(path: str) -> bytes:
    data = Path(path).read_bytes()
    if len(data) != RAM_SIZE:
        raise SystemExit(
            f"state_discover: {path} is {len(data)} bytes, expected {RAM_SIZE}"
        )
    return data


def _format_value(snap: bytes, addr: int, width: int) -> str:
    chunk = snap[addr : addr + width]
    if width == 1:
        return f"{chunk[0]:02X}"
    if width == 2:
        v = chunk[0] | (chunk[1] << 8)
        return f"{v:04X}"
    if width == 4:
        v = chunk[0] | (chunk[1] << 8) | (chunk[2] << 16) | (chunk[3] << 24)
        return f"{v:08X}"
    raise ValueError(width)


def _addresses(width: int) -> range:
    return range(0, RAM_SIZE, width)


def _emit(candidates: list[int], snaps: list[bytes], width: int, limit: int) -> None:
    if not candidates:
        print("(no candidates)")
        return
    shown = candidates[:limit]
    for addr in shown:
        seg = (addr >> 4) & 0xFFFF
        off = addr & 0xF
        vals = " → ".join(_format_value(s, addr, width) for s in snaps)
        print(f"0x{addr:06X}  {seg:04X}:{off:04X}  {vals}")
    if len(candidates) > limit:
        print(f"... {len(candidates) - limit} more (use --max to widen)")
    print(f"# {len(candidates)} total candidate{'s' if len(candidates) != 1 else ''}")


def cmd_diff(args: argparse.Namespace) -> None:
    a = _load(args.snap_a)
    b = _load(args.snap_b)
    out: list[int] = []
    for addr in _addresses(args.width):
        if a[addr : addr + args.width] != b[addr : addr + args.width]:
            out.append(addr)
    _emit(out, [a, b], args.width, args.max)


def cmd_changed(args: argparse.Namespace) -> None:
    snaps = [_load(p) for p in args.snaps]
    if len(snaps) < 2:
        raise SystemExit("changed needs >=2 snapshots")
    out: list[int] = []
    for addr in _addresses(args.width):
        if all(
            snaps[i][addr : addr + args.width]
            != snaps[i + 1][addr : addr + args.width]
            for i in range(len(snaps) - 1)
        ):
            out.append(addr)
    _emit(out, snaps, args.width, args.max)


def cmd_reverted(args: argparse.Namespace) -> None:
    snaps = [_load(p) for p in args.snaps]
    if len(snaps) < 3:
        raise SystemExit("reverted needs >=3 snapshots (baseline, after, back)")
    baseline = snaps[0]
    after = snaps[1]
    laters = snaps[2:]
    out: list[int] = []
    for addr in _addresses(args.width):
        b = baseline[addr : addr + args.width]
        a = after[addr : addr + args.width]
        if a == b:
            continue
        if any(later[addr : addr + args.width] == b for later in laters):
            out.append(addr)
    _emit(out, snaps, args.width, args.max)


def _value_at(snap: bytes, addr: int, width: int) -> int:
    chunk = snap[addr : addr + width]
    v = 0
    for i, b in enumerate(chunk):
        v |= b << (8 * i)
    return v


def cmd_monotonic(args: argparse.Namespace) -> None:
    snaps = [_load(p) for p in args.snaps]
    if len(snaps) < 3:
        raise SystemExit("monotonic needs >=3 snapshots")
    out: list[int] = []
    for addr in _addresses(args.width):
        vals = [_value_at(s, addr, args.width) for s in snaps]
        if all(vals[i] < vals[i + 1] for i in range(len(vals) - 1)):
            out.append(addr)
            continue
        if all(vals[i] > vals[i + 1] for i in range(len(vals) - 1)):
            out.append(addr)
    _emit(out, snaps, args.width, args.max)


def cmd_constant(args: argparse.Namespace) -> None:
    snaps = [_load(p) for p in args.snaps]
    if len(snaps) < 2:
        raise SystemExit("constant needs >=2 snapshots")
    first = snaps[0]
    out: list[int] = []
    for addr in _addresses(args.width):
        chunk = first[addr : addr + args.width]
        if all(s[addr : addr + args.width] == chunk for s in snaps[1:]):
            out.append(addr)
    _emit(out, snaps, args.width, args.max)


def main() -> None:
    parser = argparse.ArgumentParser(
        description=__doc__,
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    parser.add_argument(
        "--max", type=int, default=200,
        help="cap rows printed (default 200)")
    parser.add_argument(
        "--width", type=int, choices=(1, 2, 4), default=1,
        help="group bytes as 8/16/32-bit values (default 1)")
    sub = parser.add_subparsers(dest="command", required=True)

    sp = sub.add_parser("diff", help="byte-level diff of two snapshots")
    sp.add_argument("snap_a")
    sp.add_argument("snap_b")

    sp = sub.add_parser("changed", help="addresses that differ across every step")
    sp.add_argument("snaps", nargs="+")

    sp = sub.add_parser("reverted", help="changed in S1 then returned to S0 in some later Sn")
    sp.add_argument("snaps", nargs="+")

    sp = sub.add_parser("constant", help="addresses identical across all snapshots")
    sp.add_argument("snaps", nargs="+")

    sp = sub.add_parser(
        "monotonic",
        help="value strictly increases or strictly decreases at every step")
    sp.add_argument("snaps", nargs="+")

    args = parser.parse_args()
    handlers = {
        "diff": cmd_diff, "changed": cmd_changed,
        "reverted": cmd_reverted, "constant": cmd_constant,
        "monotonic": cmd_monotonic,
    }
    handlers[args.command](args)


if __name__ == "__main__":
    # `head` and pipes close stdout early; don't backtrace.
    try:
        main()
    except BrokenPipeError:
        sys.stderr.close()
