#!/usr/bin/env python3
"""JIT chunk translator.

Decode an in-memory code region -- the REAL bytes the program is about to
execute (decompressed by a runtime unpacker, loaded from an overlay, or
self-modified) -- into a compilable C chunk that exports ``<name>_dispatch``.

This is the translator entry point for the JIT (the whole pipeline): when the
running program transfers control to code with no compiled chunk, the runtime
dumps memory and calls this to produce a chunk it then compiles to a .so and
dlopen()s. It runs the same disassemble + ir_to_c pipeline used everywhere --
the input is a live memory dump (decoded from a chosen entry) rather than an
on-disk EXE.

The dump is a single 64KB SEGMENT (the runtime dumps memory at cs<<4) and the
entry is an IP within it, so the chunk's dispatch switch is keyed on IP
(file_off == IP, cs_base 0). The runtime registers it with the segment base and
dispatches with pc = linear - seg_base = IP. This matches the x86 segment model
so the code's ret_ip / near-ret / far-jump offsets come out correct.
"""
from __future__ import annotations

import argparse
import json
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--mem", required=True,
                    help="raw 64KB segment dump (offset 0 == cs<<4)")
    ap.add_argument("--entry", required=True, type=lambda v: int(v, 0),
                    help="IP (offset within the segment dump) to decode from")
    ap.add_argument("--name", required=True,
                    help="chunk module name (a C identifier, e.g. jit_1A9CE)")
    ap.add_argument("--outdir", default="build/jit",
                    help="output directory for the chunk .c/.h/.bin")
    args = ap.parse_args()

    outdir = Path(args.outdir)
    outdir.mkdir(parents=True, exist_ok=True)

    blob = Path(args.mem).read_bytes()
    if not (0 <= args.entry < len(blob)):
        print(f"jit_chunk: entry 0x{args.entry:X} outside dump "
              f"[0,0x{len(blob):X})", file=sys.stderr)
        sys.exit(2)

    # Decode ONLY code reachable by real control flow from the entry. We do NOT
    # enable cs_base push-imm/jump-table discovery here: that speculatively
    # follows every table word as a code target, and in overlay/resource segments
    # many words are data -- the disassembler then decodes data as code and
    # ir_to_c "drops" it. A dropped data-decode means we walked into the wrong
    # part of the image (a corrupted decode), so it must NOT happen silently.
    # Indirect targets (jump tables, pushed pointers) are resolved ON DEMAND at
    # runtime instead: when the program actually transfers there, the runtime
    # JITs that address with the REAL target value, so we never decode data.
    bin_path = outdir / f"{args.name}.bin"
    bin_path.write_bytes(blob)
    sidecar = {
        "extra_entries": [f"0x{args.entry:X}"],
        "skip_entry_0000": True,
    }
    bin_path.with_suffix(".json").write_text(json.dumps(sidecar) + "\n")

    # Run the translator: build_pipeline runs disassemble then ir_to_c
    # with --prefix <name>_, emitting <outdir>/<name>.c and <name>.h.
    subprocess.run(
        [sys.executable, str(ROOT / "build_pipeline.py"), str(bin_path),
         "--artifacts-dir", str(outdir), "--force"],
        check=True, stdin=subprocess.DEVNULL,
    )

    c_path = outdir / f"{args.name}.c"
    if not c_path.exists():
        print(f"jit_chunk: pipeline produced no {c_path}", file=sys.stderr)
        sys.exit(3)
    print(str(c_path))


if __name__ == "__main__":
    main()
