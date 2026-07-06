#!/usr/bin/env python3
"""JIT compile orchestrator (runtime-invoked).

Given a memory-image dump and a linear entry address, produce a loadable chunk
shared object and tell the runtime how to wire it in. Steps:

  1. translate  -- jit_chunk.py decodes the in-memory bytes from `entry` into C
  2. compile    -- clang builds the chunk .c into a position-independent .so
                   (runtime globals/functions stay undefined; they resolve at
                   dlopen against the -rdynamic main binary)
  3. report     -- print the .so path, the exported dispatch symbol, and the
                   linear address range the chunk covers, for the runtime to
                   dlopen + dlsym + register in its dynamic dispatch table

Output (stdout, one item per line) for the runtime to parse:
  SO   <path to .so>
  SYM  <exported dispatch symbol name>
  RANGE <lo hex> <hi hex>
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import struct
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent
REPO = ROOT.parent


def _toolchain_hash() -> str:
    """Hash the pieces whose change must invalidate a cached chunk .so: the
    translator (a different .c would be emitted) and the runtime headers (a
    struct-layout change would make the .so's compiled-in offsets stale). A
    pure runtime .c change does NOT invalidate -- the .so resolves runtime
    symbols at dlopen, so it stays valid against the rebuilt main binary."""
    h = hashlib.sha256()
    files = [ROOT / "ir_to_c.py", ROOT / "disassemble.py",
             ROOT / "jit_chunk.py", ROOT / "build_pipeline.py"]
    files += sorted((REPO / "runtime" / "include").glob("*.h"))
    for f in files:
        try:
            h.update(f.read_bytes())
        except OSError:
            pass
    return h.hexdigest()[:16]


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--mem", required=True,
                    help="raw memory-image dump (base 0)")
    ap.add_argument("--entry", required=True, type=lambda v: int(v, 0),
                    help="offset within the dump to decode from (== IP for a "
                         "per-segment dump based at cs<<4)")
    ap.add_argument("--name", default=None,
                    help="chunk module name (unique per segment+entry); "
                         "default jit_<entry>")
    ap.add_argument("--image-base", type=lambda v: int(v, 0), default=0,
                    help="linear base of dump (cs<<4 per-segment); "
                         "far targets mapped relative to it")
    ap.add_argument("--outdir", default=str(REPO / "build" / "jit"),
                    help="directory for chunk artifacts")
    args = ap.parse_args()

    outdir = Path(args.outdir)
    outdir.mkdir(parents=True, exist_ok=True)
    # Bound the decode so a bad transfer target (one pointing at zeros/data)
    # can't make the disassembler follow fall-through through the whole image
    # into a giant chunk that hangs clang. Inherited by the disassemble.py
    # subprocess. Generous vs a real function graph (the largest observed
    # decompressed chunk was ~7k instructions).
    os.environ.setdefault("SAISEI_DISASM_MAX_INSNS", "30000")
    # Map same-segment far targets to dump offsets (per-segment dump).
    os.environ["SAISEI_DISASM_IMAGE_BASE"] = f"0x{args.image_base:X}"
    # NO drop-data: a function that fails to translate means the disassembler
    # walked into data (decoded data as code) -- i.e. we decoded the WRONG part
    # of the image. That is a corrupted decode, not something to silently drop
    # and proceed. Let ir_to_c FATAL out; the chunk compile fails, the runtime's
    # jit_compile_or_get returns NULL, and the dispatch aborts loudly with the
    # offending mnemonic+address, so the real bug (a bad entry / wrong target)
    # surfaces instead of detonating later. With discovery off (jit_chunk.py)
    # real control-flow decode should never reach data.
    # lowercase: build_pipeline sanitizes the stem to lowercase, so the
    # emitted <name>.c and <name>_dispatch symbol are lowercase -- match it.
    name = (args.name or f"jit_{args.entry:x}").lower()

    # Fold the segment-content hash into the chunk name. The SAME seg_base+entry
    # is re-decoded with DIFFERENT bytes whenever an overlay/unpacker overwrites
    # the code there (e.g. a self-replacing overlay EXE's launcher then a
    # second program both run at 1010:0000).
    # If the .so path stayed constant, the runtime's dlopen would hand back the
    # OLD already-mapped handle (and the duplicate `<name>_dispatch` symbol would
    # interpose under RTLD_GLOBAL), so the dispatch would run the STALE code with
    # the new chunk's metadata -- exactly the seg/return-faithfulness seam called
    # out in the prime directive. A content-keyed name gives each distinct decode
    # its own .so + dispatch symbol; identical bytes reuse one name so the
    # cross-run cache below still hits.
    blob = Path(args.mem).read_bytes()
    content_sha = hashlib.sha256(blob).hexdigest()[:16]
    name = f"{name}_{content_sha}"

    so_path = outdir / f"{name}.so"
    sha_path = outdir / f"{name}.sha"
    range_path = outdir / f"{name}.range"
    keys_path = outdir / f"{name}.keys"
    code_path = outdir / f"{name}.code"

    # Cross-run cache: skip the (slow) decode+clang when this exact segment
    # content + toolchain was already compiled. The clang step dominates JIT
    # cost; a hit drops a chunk to a stat + file read. The content hash is now in
    # the name; the .sha still carries the toolchain hash so a toolchain change
    # (same bytes) re-decodes. SAISEI_JIT_NOCACHE=1 forces a rebuild.
    key = content_sha + ":" + _toolchain_hash()
    if (not os.environ.get("SAISEI_JIT_NOCACHE")
            and so_path.exists() and sha_path.exists() and range_path.exists()
            and keys_path.exists() and code_path.exists()
            and sha_path.read_text().strip() == key):
        lo_s, hi_s = range_path.read_text().split()[:2]
        print(f"SO {so_path}")
        print(f"SYM {name}_dispatch")
        print(f"RANGE {lo_s} {hi_s}")
        return

    # 1. translate the in-memory region into a C chunk
    subprocess.run(
        [sys.executable, str(ROOT / "jit_chunk.py"),
         "--mem", args.mem, "--entry", hex(args.entry),
         "--name", name, "--outdir", str(outdir)],
        check=True, stdin=subprocess.DEVNULL,
    )

    c_path = outdir / f"{name}.c"

    # 2. compile to a .so; undefined runtime symbols resolve at dlopen
    subprocess.run(
        ["clang", "-shared", "-fPIC", "-O1", str(c_path),
         "-I", str(REPO / "runtime" / "include"), "-I", str(outdir),
         "-o", str(so_path)],
        check=True, stdin=subprocess.DEVNULL,
    )

    # 3. the linear address range the chunk decoded (its case-key span)
    fj = outdir / "disassemble" / name / "functions.json"
    addrs: list[int] = []
    spans: list[tuple[int, int]] = []  # [start,end) byte extent of every decoded instruction
    try:
        for f in json.loads(fj.read_text()):
            for ins in f.get("instructions", []) or []:
                a = ins.get("address")
                if a is not None:
                    blen = len(ins.get("bytes", "")) // 2
                    addrs.append(a)
                    addrs.append(a + blen)
                    if blen > 0:
                        spans.append((a, a + blen))
    except (OSError, json.JSONDecodeError):
        pass
    lo, hi = ((min(addrs), max(addrs)) if addrs
              else (args.entry, args.entry + 1))

    # Case-key set: the dispatch `case 0xXXX:` labels (the addresses the chunk
    # actually decoded an entry for). The runtime uses this so jit_lookup matches
    # a chunk ONLY on a real case -- otherwise the chunk's [lo,hi] range would
    # shadow another chunk's real code at the in-range gaps (mid-instruction /
    # undecoded), whose default case near-rets and corrupts the stack.
    # Format: <uint32 count><count * uint32 keys>, sorted.
    case_re = re.compile(r"case 0x([0-9A-Fa-f]+):")
    keys = sorted({int(m, 16) for m in case_re.findall(
        c_path.read_text(encoding="utf-8", errors="ignore"))})
    with keys_path.open("wb") as fh:
        fh.write(struct.pack("<I", len(keys)))
        for k in keys:
            fh.write(struct.pack("<I", k))

    # Exact decoded-instruction byte coverage: merged [start,end) intervals over
    # EVERY decoded instruction (not just the dispatch case-keys). The runtime
    # invalidates a JIT chunk iff a write overwrites a real code byte -- the
    # faithful x86 self-modifying-code rule. This is exact: a write to a data /
    # buffer byte that merely sits inside the chunk's [lo,hi] span (a gap between
    # or after instructions) does NOT invalidate (so a resident routine churning a
    # nearby counter never recompiles), while a patch to ANY byte of an
    # instruction (opcode OR operand/immediate/displacement) DOES. Replaces the
    # old case-key-start window heuristic in jit_invalidate_range_impl. Same IP
    # coordinate as keys/lo/hi. Format: <uint32 count> then count*(uint32 start,
    # uint32 end), sorted, non-overlapping.
    spans.sort()
    merged: list[list[int]] = []
    for s, e in spans:
        if merged and s <= merged[-1][1]:
            if e > merged[-1][1]:
                merged[-1][1] = e
        else:
            merged.append([s, e])
    with code_path.open("wb") as fh:
        fh.write(struct.pack("<I", len(merged)))
        for s, e in merged:
            fh.write(struct.pack("<II", s, e))

    # Record the cache sidecars so a later run with the same segment+toolchain
    # reuses this .so instead of recompiling.
    sha_path.write_text(key + "\n")
    range_path.write_text(f"0x{lo:X} 0x{hi:X}\n")

    print(f"SO {so_path}")
    print(f"SYM {name}_dispatch")
    print(f"RANGE 0x{lo:X} 0x{hi:X}")


if __name__ == "__main__":
    main()
