#!/usr/bin/env python3
"""Run disassembly and C translation for one or more binaries.

Pipeline output is arranged as:

    build/
        <name>.c            # generated C for each input
        disassemble/
            <name>/        # disassembly artefacts for each input
"""

from __future__ import annotations

import argparse
import importlib.util
import json
import subprocess
import sys
import time
from pathlib import Path

try:
    from .utils import sanitize_identifier
except ImportError:  # pragma: no cover - allow running as a script
    from utils import sanitize_identifier

ROOT = Path(__file__).resolve().parent


def _ensure_dependencies() -> None:
    """Fail early when required modules are missing."""
    if importlib.util.find_spec("capstone") is not None:
        return
    raise SystemExit(
        "Missing Python dependency 'capstone'. "
        "Activate your virtualenv and run `python -m pip install "
        "-r requirements.txt`."
    )


def run(cmd: list[str], desc: str, timeout: int = 120) -> None:
    """Execute *cmd* ensuring success while reporting timing.

    The default timeout of 120 seconds accommodates slow CI environments
    where larger inputs (e.g. a large overlay archive) may take longer to process.
    """
    print(f"Starting {desc}...", flush=True)
    start = time.monotonic()
    try:
        # Explicitly detach subprocesses from the controlling terminal so they
        # cannot inadvertently read from it. Some tools in the build pipeline
        # may probe stdin which could leave the terminal in an unexpected state
        # and interfere with later keyboard handling. Feeding them ``DEVNULL``
        # prevents this.
        subprocess.run(
            cmd, check=True, timeout=timeout, stdin=subprocess.DEVNULL
        )
    except subprocess.TimeoutExpired:
        print(
            f"Error: {desc} was abnormally long. "
            "We force-killed it as it should not be as long.",
            file=sys.stderr,
        )
        raise
    duration = time.monotonic() - start
    print(f"Finished {desc} in {duration:.2f}s", flush=True)


def _newest_mtime(paths: list[Path]) -> float:
    """Newest mtime among existing files. Missing files are ignored (callers
    pass paths that should exist; non-existence is a different bug)."""
    best = 0.0
    for p in paths:
        try:
            m = p.stat().st_mtime
        except FileNotFoundError:
            continue
        if m > best:
            best = m
    return best


def _oldest_mtime(paths: list[Path]) -> float | None:
    """Oldest mtime among the listed files. Returns None if any is missing —
    a missing output means we MUST rebuild."""
    best: float | None = None
    for p in paths:
        try:
            m = p.stat().st_mtime
        except FileNotFoundError:
            return None
        if best is None or m < best:
            best = m
    return best


def main() -> None:
    _ensure_dependencies()

    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "inputs", nargs="+", help="Paths to executables or binaries"
    )
    parser.add_argument(
        "--artifacts-dir",
        default="build",
        help="Root directory for pipeline outputs",
    )
    parser.add_argument(
        "--disasm-only",
        action="store_true",
        help="Only perform disassembly without translating to C",
    )
    parser.add_argument(
        "--force",
        action="store_true",
        help="Bypass the mtime-based per-binary skip and reprocess every input",
    )
    args = parser.parse_args()

    artifacts_root = Path(args.artifacts_dir)
    disasm_root = artifacts_root / "disassemble"
    disasm_root.mkdir(parents=True, exist_ok=True)

    # Tools whose mtime invalidates every binary's disasm + translate.
    # Anything that affects the *output* of disassemble.py or ir_to_c.py
    # belongs here. utils.py is imported by both, build_pipeline.py wires
    # them together.
    tool_paths = [
        ROOT / "disassemble.py",
        ROOT / "ir_to_c.py",
        ROOT / "utils.py",
        ROOT / "build_pipeline.py",
    ]
    tool_mtime = _newest_mtime(tool_paths)

    seen_names: set[str] = set()
    for inp in args.inputs:
        bin_path = Path(inp)
        name = bin_path.stem
        safe_name = sanitize_identifier(name)
        if safe_name in seen_names:
            raise ValueError(
                f"Duplicate sanitized name '{safe_name}' for input {bin_path}"
            )
        seen_names.add(safe_name)
        outdir = disasm_root / safe_name
        outdir.mkdir(parents=True, exist_ok=True)

        meta = bin_path.with_suffix(".json")
        extra_entries: list[int] = []
        skip_entry_0000 = False
        cs_base: int | None = None
        if meta.exists():
            try:
                meta_data = json.loads(meta.read_text())
            except json.JSONDecodeError:
                meta_data = {}
            for entry in meta_data.get("extra_entries", []):
                try:
                    extra_entries.append(int(entry, 0))
                except (TypeError, ValueError):
                    continue
            skip_entry_0000 = bool(meta_data.get("skip_entry_0000", False))
            cs_base_raw = meta_data.get("cs_base")
            if cs_base_raw is not None:
                try:
                    cs_base = (
                        int(cs_base_raw, 0)
                        if isinstance(cs_base_raw, str)
                        else int(cs_base_raw)
                    )
                except (TypeError, ValueError):
                    cs_base = None

        # Per-binary mtime gate. Inputs that should invalidate this binary's
        # outputs: the binary itself, its meta JSON (entry list / cs_base /
        # skip flag), and the shared tools (disasm.py / ir_to_c.py /
        # utils.py / build_pipeline.py).
        input_paths = [bin_path, meta] if meta.exists() else [bin_path]
        newest_input = max(_newest_mtime(input_paths), tool_mtime)

        ir_path = outdir / "program.ir.json"
        c_out = artifacts_root / f"{safe_name}.c"
        # In disasm-only mode the .c file isn't expected to be written — gate
        # on the IR alone. Otherwise both must be present and current.
        outputs = [ir_path] if args.disasm_only else [ir_path, c_out]
        oldest_output = _oldest_mtime(outputs)
        if (
            not args.force
            and oldest_output is not None
            and oldest_output >= newest_input
        ):
            mode = "disasm" if args.disasm_only else "disasm+translate"
            print(
                f"Skipping {mode} for {safe_name}: outputs up to date "
                f"(use --force to override)",
                flush=True,
            )
            continue

        dis_cmd = [
            sys.executable,
            str(ROOT / "disassemble.py"),
            str(bin_path),
            "--outdir",
            str(outdir),
        ]
        for entry in extra_entries:
            dis_cmd.extend(["--entry", f"0x{entry:X}"])
        if skip_entry_0000:
            dis_cmd.append("--skip-entry-0000")
        if cs_base is not None:
            dis_cmd.extend(["--cs-base", f"0x{cs_base:X}"])

        # Fail-fast: if disasm or ir_to_c fails for any binary, abort the
        # whole pipeline. Continuing to translate other binaries when one
        # already failed wastes time and produces a confusing "Missing
        # generated sources" downstream error that hides the real cause.
        # ir_to_c.py prints its own clear [ir_to_c FATAL] block before
        # exiting; we just propagate the exit status.
        run(dis_cmd, desc=f"disassembling {safe_name}")
        if args.disasm_only:
            continue
        cmd = [
            sys.executable,
            str(ROOT / "ir_to_c.py"),
            str(ir_path),
            str(bin_path),
            "--out",
            str(c_out),
        ]
        cmd.extend(["--prefix", f"{safe_name}_"])
        if meta.exists():
            cmd.extend(["--metadata", str(meta)])
        run(cmd, desc=f"translating {name} to C")


if __name__ == "__main__":
    try:
        main()
    except subprocess.CalledProcessError as exc:
        # Subprocess (disasm or ir_to_c) failed; it has already printed its
        # own error block. Propagate the exit code without dumping a Python
        # traceback that obscures the real message.
        sys.exit(exc.returncode if exc.returncode else 1)
