#!/usr/bin/env python3
"""Triage a crash bundle -- the entry point for processing user-submitted bug
reports.

A crash bundle (build/<game>/crashes/crash_*/) is the platform's bug-report
unit. This reads one and prints a maintainer-facing summary: what failed, the
runtime version it happened on, and -- crucially -- which *class* of problem it
is and where the fix lives. The three classes (see the reconstruction model):

  1. operator      a translated instruction's effect is wrong (stack drift,
                   unsupported opcode)            -> fix in compiler/runtime
  2. bad-transfer  control reached a wrong/unmapped code address (unhandled pc,
                   unmapped call). Real undecoded code is JIT'd on demand, so
                   this = a GENUINELY wrong target -> read OUR shims/codegen
                   (stack corruption or an unfaithful transfer upstream)
  3. missing-file  the game opened a file not in the bundle
                   (dos_open failure)             -> add to games/<name>.json

Usage:
    python -m tools.triage <bundle_dir>
    python -m tools.triage --latest [<game>]      # newest bundle for a game
"""
from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

# kind (from manifest/dir name) -> (class, one-line meaning, where to fix)
_CLASS = {
    "stack_drift": ("operator", "simulated stack pointer drifted across a call",
                    "a translated instruction dropped a push/pop, or a "
                    "callee-cleanup (retf N) the runtime didn't expect"),
    "retf_drift": ("operator", "far return saw an unexpected stack pointer",
                   "the far-called function's body left the stack unbalanced "
                   "(translator stack-effect bug)"),
    "dispatch_recursion": ("operator/disasm",
                           "dispatch recursed without bound",
                           "a near-ret popped a bogus return IP; trace the "
                           "near_ret_tail/call_table chain for the first "
                           "expected_retip divergence"),
    "unhandled_pc": ("bad-transfer", "dispatched to an address that won't decode",
                     "real code is JIT'd automatically, so this is a genuinely "
                     "wrong address -- stack corruption or an unfaithful "
                     "transfer upstream"),
    "lcall_table": ("bad-transfer", "far call to an unmapped/garbage target",
                    "check relocation of the call's segment; a 0x0 target is "
                    "often an un-relocated imm"),
    "call_table": ("bad-transfer", "near/indirect call to an unmapped target",
                   "the computed target is garbage (wrong ds/register upstream)"),
    "mapswap_straddle": ("memory", "overlay chunk swap straddled a region",
                         "overlay mapping issue; inspect file_mappings.json"),
    "rcb_overwrite": ("memory", "a resident-control-block field was overwritten",
                      "a write aliased an RCB field; inspect the write site"),
    "cross_binary_overwrite": ("memory", "a write crossed into another binary",
                               "segment/pointer bug; inspect the write site"),
}

# Heuristic: a failed file open in the logs => missing-file class (class 3),
# which currently has no dedicated bundle kind.
_FILE_FAIL_RE = re.compile(
    r"dos_(open|find)[^\n]*?(?:failed|not found|No such)", re.IGNORECASE)
_OPEN_PATH_RE = re.compile(r"dos_open_file:\s*([^\s]+)", re.IGNORECASE)


def _read(p: Path) -> str:
    try:
        return p.read_text(errors="ignore")
    except OSError:
        return ""


def find_latest(game: str | None) -> Path | None:
    roots = []
    if game:
        roots = [ROOT / "build" / game / "crashes"]
    else:
        bd = ROOT / "build"
        roots = [p / "crashes" for p in bd.iterdir() if p.is_dir()] if bd.is_dir() else []
    bundles = [b for r in roots if r.is_dir() for b in r.glob("crash_*") if b.is_dir()]
    return max(bundles, key=lambda b: b.stat().st_mtime, default=None)


def triage(bundle: Path) -> int:
    if not bundle.is_dir():
        print(f"triage: not a bundle dir: {bundle}", file=sys.stderr)
        return 2

    manifest = {}
    mp = bundle / "manifest.json"
    if mp.exists():
        try:
            manifest = json.loads(mp.read_text())
        except (OSError, json.JSONDecodeError):
            pass

    # kind: prefer manifest, else parse the dir name (crash_<ts>_<kind>_<addr>)
    kind = manifest.get("kind")
    if not kind:
        m = re.match(r"crash_\d+_\d+_(.+?)_0x[0-9A-Fa-f]+$", bundle.name)
        kind = m.group(1) if m else "unknown"

    crash = _read(bundle / "crash.txt")
    trace = _read(bundle / "trace.tail.log")

    klass, meaning, fix = _CLASS.get(
        kind, ("unknown", "unrecognized failure kind", "inspect crash.txt"))

    # Class-3 override: a failed file open anywhere in the logs.
    missing_file = None
    if _FILE_FAIL_RE.search(crash + trace):
        klass = "missing-file"
        meaning = "the game tried to open a file that isn't bundled"
        fix = "add the file to games/<name>.json (binaries + runtime)"
        mo = _OPEN_PATH_RE.search(crash + trace)
        missing_file = mo.group(1) if mo else None

    print(f"=== crash bundle triage: {bundle.name} ===")
    print(f"  kind            : {kind}")
    print(f"  class           : {klass}")
    print(f"  what happened   : {meaning}")
    if manifest:
        print(f"  runtime version : {manifest.get('runtime_version', '?')}")
        print(f"  active binary   : {manifest.get('active_binary', '?')}")
        print(f"  fault addr      : {manifest.get('fault_addr', '?')}")
        cpu = manifest.get("cpu", {})
        if cpu:
            print(f"  cpu             : cs:ip={cpu.get('cs')}:{cpu.get('ip')} "
                  f"ss:sp={cpu.get('ss')}:{cpu.get('sp')}")
        d = manifest.get("depths", {})
        if d:
            print(f"  depths          : lcall={d.get('lcall')} isr={d.get('isr')} "
                  f"dispatch={d.get('dispatch')}")
    else:
        print("  (no manifest.json -- older bundle; facts from crash.txt only)")
    if missing_file:
        print(f"  missing file    : {missing_file}")

    # First [BUG]/[FATAL]/Error line is usually the one-line cause.
    for line in crash.splitlines():
        s = line.strip()
        if s.startswith(("[BUG]", "[FATAL]", "Error:")):
            print(f"  message         : {s}")
            break

    print(f"\n  >> suggested fix ({klass}): {fix}")
    files = sorted(p.name for p in bundle.iterdir() if p.is_file())
    print(f"  bundle files    : {', '.join(files)}")
    return 0


def main() -> None:
    ap = argparse.ArgumentParser(prog="triage", description=__doc__)
    ap.add_argument("bundle", nargs="?", help="path to a crash_* bundle dir")
    ap.add_argument("--latest", action="store_true",
                    help="triage the newest bundle (optionally for a game)")
    ap.add_argument("--game", help="with --latest, restrict to this game")
    args = ap.parse_args()

    if args.latest or not args.bundle:
        b = find_latest(args.game or (args.bundle if not args.latest else None))
        if not b:
            raise SystemExit("triage: no crash bundles found under build/*/crashes/")
        sys.exit(triage(b))
    sys.exit(triage(Path(args.bundle)))


if __name__ == "__main__":
    main()
