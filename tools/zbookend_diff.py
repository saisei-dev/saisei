#!/usr/bin/env python3
"""Diff two bookend RAM snapshots produced by shim_bookend_start/stop.

Both files are raw `virtual_memory` dumps (MEMORY_SIZE = 2 MiB). Output is
a list of changed byte runs, sorted by address, with optional grep into
/tmp/zbookend.log to surface writers for each range.

Usage:
    python3 tools/zbookend_diff.py snap1.bin snap2.bin [--log /tmp/zbookend.log]
                                                         [--max N]
                                                         [--addr LO:HI]

Defaults: skip VGA (0xA0000-0xBFFFF) and BIOS data area (0x00400-0x004FF).
The stack window is unknown at diff time (ss may have moved) so it is NOT
filtered here — the write-log filter already skipped stack writes.
"""

import argparse
import os
import re
from collections import defaultdict


SKIP_RANGES_DEFAULT = [
    (0xA0000, 0xC0000),  # VGA pages
    (0x00400, 0x00500),  # BIOS data area (clock tick, etc.)
]


def load_snapshot(path):
    with open(path, 'rb') as f:
        data = f.read()
    return data


def find_runs(a, b, skip_ranges):
    runs = []  # list of (lo, hi_exclusive)
    n = min(len(a), len(b))
    i = 0
    while i < n:
        if any(lo <= i < hi for lo, hi in skip_ranges):
            # jump to end of nearest skip range
            for lo, hi in skip_ranges:
                if lo <= i < hi:
                    i = hi
                    break
            continue
        if a[i] != b[i]:
            j = i + 1
            # allow up to 8 bytes of equal padding inside a run (small
            # mixed-edit regions read better as one entry than as many)
            equal_streak = 0
            while j < n and equal_streak < 8:
                if any(lo <= j < hi for lo, hi in skip_ranges):
                    break
                if a[j] != b[j]:
                    equal_streak = 0
                else:
                    equal_streak += 1
                j += 1
            # trim trailing equal padding from run
            while j > i + 1 and a[j - 1] == b[j - 1]:
                j -= 1
            runs.append((i, j))
            i = j
        else:
            i += 1
    return runs


def parse_log(path):
    """Return: dict[addr_int] -> list of log lines that wrote to that address."""
    if not path or not os.path.exists(path):
        return {}
    pat = re.compile(r"^W ([0-9A-Fa-f]+) size=(\d+)")
    by_addr = defaultdict(list)
    with open(path, 'r', errors='replace') as f:
        for line in f:
            m = pat.match(line)
            if not m:
                continue
            addr = int(m.group(1), 16)
            size = int(m.group(2))
            for a in range(addr, addr + size):
                by_addr[a].append(line.rstrip())
    return by_addr


def fmt_run(a, b, lo, hi):
    length = hi - lo
    if length <= 16:
        old = ' '.join(f"{a[x]:02X}" for x in range(lo, hi))
        new = ' '.join(f"{b[x]:02X}" for x in range(lo, hi))
        return f"  {old}\n  -> {new}"
    return f"  ({length} bytes — too long to print inline)"


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('snap1')
    ap.add_argument('snap2')
    ap.add_argument('--log', default='/tmp/zbookend.log',
                    help='Path to bookend write log (default /tmp/zbookend.log).')
    ap.add_argument('--max', type=int, default=200,
                    help='Max number of diff runs to display.')
    ap.add_argument('--addr', default=None,
                    help='Restrict diff to LO:HI (hex). E.g., --addr 12B00:12C00')
    ap.add_argument('--no-default-skip', action='store_true',
                    help='Disable default VGA/BIOS skip ranges.')
    ap.add_argument('--max-writers', type=int, default=4,
                    help='Max log lines to show per diff run (default 4).')
    args = ap.parse_args()

    a = load_snapshot(args.snap1)
    b = load_snapshot(args.snap2)
    if len(a) != len(b):
        print(f"[warn] snapshot sizes differ: {len(a)} vs {len(b)} — comparing common prefix")

    skip = [] if args.no_default_skip else list(SKIP_RANGES_DEFAULT)
    if args.addr:
        lo_s, hi_s = args.addr.split(':')
        lo = int(lo_s, 16)
        hi = int(hi_s, 16)
        # convert "restrict to [lo,hi)" into "skip everything outside"
        skip.append((0, lo))
        skip.append((hi, max(len(a), len(b))))

    runs = find_runs(a, b, skip)
    by_addr = parse_log(args.log)

    print(f"snap1: {args.snap1}  ({len(a)} bytes)")
    print(f"snap2: {args.snap2}  ({len(b)} bytes)")
    print(f"log:   {args.log}  ({'present' if by_addr else 'missing/empty'})")
    print(f"runs:  {len(runs)} (showing up to {args.max})")
    print()

    for idx, (lo, hi) in enumerate(runs[:args.max]):
        print(f"#{idx + 1:>4}  {lo:05X}..{hi - 1:05X}  (+{hi - lo}B)")
        print(fmt_run(a, b, lo, hi))
        if by_addr:
            # collect unique writer lines for this run
            seen = []
            seen_set = set()
            for addr in range(lo, hi):
                for line in by_addr.get(addr, ()):
                    if line in seen_set:
                        continue
                    seen_set.add(line)
                    seen.append(line)
                    if len(seen) >= args.max_writers:
                        break
                if len(seen) >= args.max_writers:
                    break
            if seen:
                print("  writers:")
                for line in seen:
                    print(f"    {line}")
            else:
                print("  writers: (no log entries — write came from a "
                      "raw path or was filtered)")
        print()

    if len(runs) > args.max:
        print(f"... {len(runs) - args.max} more runs (use --max to see)")


if __name__ == '__main__':
    main()
