#!/usr/bin/env python3
"""Disassemble a DOS MZ executable or raw binary and produce analysis
artifacts.

The workflow captures metadata, disassembles code, and assembles a lossless
JSON intermediate representation (program.ir.json).
"""

import argparse
import json
import os
import struct
import base64
import hashlib
from pathlib import Path

from capstone import Cs, CS_ARCH_X86, CS_MODE_16, CS_AC_READ, CS_AC_WRITE
import capstone.x86_const as x86_const


def _parse_imm(value: str) -> int | None:
    """Best-effort parse of an immediate value from assembly syntax."""

    import re

    value = value.strip()
    # Skip values that look like memory references (e.g. ``[bx+0x10]``) or
    # other complex expressions.  Only bare immediates are supported.
    if "[" in value or "]" in value or ":" in value:
        return None
    match = re.fullmatch(
        r"(?i)(?:short|near)?\s*([+-]?(?:0x[0-9a-f]+|[0-9a-f]+h|[0-9a-f]+))",
        value,
    )
    if not match:
        return None
    token = match.group(1)
    sign = ""
    if token[0] in "+-":
        sign, token = token[0], token[1:]
    if token[-1] in "hH":
        token = token[:-1]
        token = f"0x{token}"
    elif token.lower().startswith("0x"):
        pass
    elif token.isdigit() and sign:
        pass
    else:
        token = f"0x{token}"
    try:
        return int(f"{sign}{token}", 0)
    except ValueError:
        return None


def parse_entry_point(arg: str) -> int:
    """Parse a user supplied entry point string into an offset."""
    if ":" in arg:
        seg, off = arg.split(":", 1)
        # Segment:offset pairs are typically hexadecimal with optional leading
        # zeros and no prefix. Using ``int(..., 0)`` rejects values such as
        # ``"0100"``. Parse both parts explicitly as base-16 to accept common
        # forms like ``0100:0010`` or ``0x100:0x10``.
        return int(seg, 16) * 16 + int(off, 16)
    try:
        return int(arg, 0)
    except ValueError:
        return int(arg, 16)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "input", help="Path to DOS MZ executable or raw binary"
    )
    parser.add_argument(
        "--outdir",
        default="build/disassemble",
        help="Output directory for artifacts",
    )
    parser.add_argument(
        "--entry",
        dest="entry",
        action="append",
        default=[],
        type=parse_entry_point,
        help=(
            "Additional entry point to decode (linear or SEG:OFF). "
            "Can be specified multiple times"
        ),
    )
    parser.add_argument(
        "--skip-entry-0000",
        dest="skip_entry_0000",
        action="store_true",
        help="Do not decode the default 0x0000 entry point",
    )
    parser.add_argument(
        "--cs-base",
        dest="cs_base",
        type=lambda v: int(v, 0),
        default=None,
        help=(
            "Segment offset (IP value at file_off 0). Enables auto-discovery "
            "of push-imm and jump-table targets — without this flag discovery "
            "is off, since data-heavy binaries (resource archives) decode "
            "garbage into spurious patterns. Typical values are small, "
            "e.g. 0x100 or 0x1100."
        ),
    )
    return parser.parse_args()


def ensure_dir(path: Path) -> None:
    path.mkdir(parents=True, exist_ok=True)


def stage1(
    data: bytes,
    outdir: Path,
    extra_entries: list[int] | None = None,
) -> dict:
    """Perform binary intake and metadata capture."""
    if data[:2] != b"MZ":
        header_fields = {
            "e_magic": "BIN",
            "e_cblp": 0,
            "e_cp": 0,
            "e_crlc": 0,
            "e_cparhdr": 0,
            "e_minalloc": 0,
            "e_maxalloc": 0,
            "e_ss": 0,
            "e_sp": 0,
            "e_csum": 0,
            "e_ip": 0,
            "e_cs": 0,
            "e_lfarlc": 0,
            "e_ovno": 0,
            "e_res": [0] * 4,
            "e_oemid": 0,
            "e_oeminfo": 0,
            "e_res2": [0] * 10,
            "e_lfanew": 0,
        }

        raw_dump_path = outdir / "raw_dump.bin"
        raw_dump_path.write_bytes(data)

        reloc_entries: list = []
        seg_dir = outdir / "segments"
        ensure_dir(seg_dir)
        code_path = seg_dir / "code.bin"
        code_path.write_bytes(data)

        segments = [
            {
                "name": "code",
                "base": 0,
                "size": len(data),
                "bytes_b64": base64.b64encode(data).decode(),
                "type": "code",
                "permissions": "r-x",
            }
        ]

        (outdir / "header.json").write_text(
            json.dumps(header_fields, indent=2)
        )

        blobs = []

        def add_blob(path: Path) -> None:
            blob_bytes = path.read_bytes()
            blobs.append(
                {
                    "path": str(path.relative_to(outdir)),
                    "sha256": hashlib.sha256(blob_bytes).hexdigest(),
                    "needs_manual_review": False,
                }
            )

        add_blob(raw_dump_path)
        add_blob(code_path)

        manifest = {
            "entry": {"CS:IP": "0000:0000", "SS:SP": "0000:0000"},
            "blobs": blobs,
        }
        if extra_entries:
            manifest["extra_entries"] = [f"0x{e:X}" for e in extra_entries]
        (outdir / "manifest.json").write_text(json.dumps(manifest, indent=2))

        return {
            "load_module": data,
            "header": header_fields,
            "relocations": reloc_entries,
            "segment_map": segments,
            "manifest": manifest,
        }

    header_fields = {}
    (
        e_magic,
        e_cblp,
        e_cp,
        e_crlc,
        e_cparhdr,
        e_minalloc,
        e_maxalloc,
        e_ss,
        e_sp,
        e_csum,
        e_ip,
        e_cs,
        e_lfarlc,
        e_ovno,
    ) = struct.unpack_from("<2s13H", data, 0)
    header_fields.update(
        {
            "e_magic": e_magic.decode(),
            "e_cblp": e_cblp,
            "e_cp": e_cp,
            "e_crlc": e_crlc,
            "e_cparhdr": e_cparhdr,
            "e_minalloc": e_minalloc,
            "e_maxalloc": e_maxalloc,
            "e_ss": e_ss,
            "e_sp": e_sp,
            "e_csum": e_csum,
            "e_ip": e_ip,
            "e_cs": e_cs,
            "e_lfarlc": e_lfarlc,
            "e_ovno": e_ovno,
        }
    )
    offset = struct.calcsize("<2s13H")
    e_res = struct.unpack_from("<4H", data, offset)
    offset += 8
    e_oemid, e_oeminfo = struct.unpack_from("<HH", data, offset)
    offset += 4
    e_res2 = struct.unpack_from("<10H", data, offset)
    offset += 20
    e_lfanew = struct.unpack_from("<I", data, offset)[0]

    header_fields.update(
        {
            "e_res": list(e_res),
            "e_oemid": e_oemid,
            "e_oeminfo": e_oeminfo,
            "e_res2": list(e_res2),
            "e_lfanew": e_lfanew,
        }
    )

    # Dump entire binary
    raw_dump_path = outdir / "raw_dump.bin"
    raw_dump_path.write_bytes(data)

    # Relocation entries
    reloc_entries = []
    for i in range(header_fields["e_crlc"]):
        entry_off = header_fields["e_lfarlc"] + i * 4
        off, seg = struct.unpack_from("<HH", data, entry_off)
        reloc_entries.append({"offset": off, "segment": seg})
    (outdir / "reloc.json").write_text(json.dumps(reloc_entries, indent=2))

    # Load module and segments
    header_size = header_fields["e_cparhdr"] * 16
    file_size = len(data)
    e_cp = header_fields["e_cp"]
    e_cblp = header_fields["e_cblp"]
    expected_file_size = (
        e_cp * 512 if e_cblp == 0 else (e_cp - 1) * 512 + e_cblp
    )
    module_size = max(min(file_size, expected_file_size) - header_size, 0)
    load_module = data[header_size:header_size + module_size]
    overlay = b""
    if file_size > expected_file_size:
        overlay = data[expected_file_size:]

    seg_dir = outdir / "segments"
    ensure_dir(seg_dir)
    code_path = seg_dir / "code.bin"
    code_path.write_bytes(load_module)
    # Segment map entries record type and permissions for downstream analysis
    segments = [
        {
            "name": "code",
            "base": 0x1000,
            "size": len(load_module),
            "bytes_b64": base64.b64encode(load_module).decode(),
            "type": "code",
            "permissions": "r-x",
        }
    ]
    overlay_path = None
    if overlay:
        overlay_path = seg_dir / "overlay.bin"
        overlay_path.write_bytes(overlay)
        segments.append(
            {
                "name": "overlay",
                "base": 0x1000 + len(load_module),
                "size": len(overlay),
                "bytes_b64": base64.b64encode(overlay).decode(),
                "type": "overlay",
                "permissions": "r-x",
            }
        )

    # Header JSON
    (outdir / "header.json").write_text(json.dumps(header_fields, indent=2))

    # Manifest generation
    blobs = []

    def add_blob(path: Path, needs_review: bool = False) -> None:
        blob_bytes = path.read_bytes()
        blobs.append(
            {
                "path": str(path.relative_to(outdir)),
                "sha256": hashlib.sha256(blob_bytes).hexdigest(),
                "needs_manual_review": needs_review,
            }
        )

    add_blob(raw_dump_path)
    add_blob(code_path)
    if overlay_path:
        add_blob(overlay_path, needs_review=True)

    manifest = {
        "entry": {
            "CS:IP": (
                f"{header_fields['e_cs']:04X}:{header_fields['e_ip']:04X}"
            ),
            "SS:SP": (
                f"{header_fields['e_ss']:04X}:{header_fields['e_sp']:04X}"
            ),
        },
        "blobs": blobs,
    }
    if extra_entries:
        manifest["extra_entries"] = [f"0x{e:X}" for e in extra_entries]
    (outdir / "manifest.json").write_text(json.dumps(manifest, indent=2))

    return {
        "load_module": load_module,
        "header": header_fields,
        "relocations": reloc_entries,
        "segment_map": segments,
        "manifest": manifest,
    }


_INDIRECT_JMP_RE = None  # lazily compiled in _discover_jump_table_targets

_PUSH_IMM_REG_NAMES = {"ax", "bx", "cx", "dx", "si", "di", "bp"}


def _discover_push_imm_targets(
    decoded: dict, cs_base: int, code_bytes: bytes
) -> set[int]:
    """Tail-call return targets from ``push imm16`` / ``mov reg, imm16; push reg``.

    The DOS code uses ``push <future-ret-IP>; jmp/fall-through; ret`` as a tail
    call: the eventual ``ret`` pops the pushed IP and resumes there. Each
    immediate is a cs-relative IP; the corresponding file offset is
    ``imm - cs_base``.

    Strict adjacency: ``mov reg, imm16`` must immediately precede ``push reg``.
    Tracking the register cache across non-adjacent instructions produces false
    positives (e.g. a far-upstream ``mov di, 0xca1`` paired with an unrelated
    ``push di`` for register save/restore).
    """
    targets: set[int] = set()
    sorted_addrs = sorted(decoded)
    for i, addr in enumerate(sorted_addrs):
        ins = decoded[addr]
        if ins["mnemonic"] != "push":
            continue
        op = ins.get("op_str", "").strip().lower()
        imm = _parse_imm(op)
        if imm is not None:
            fo = imm - cs_base
            if 0 <= fo < len(code_bytes):
                targets.add(fo)
            continue
        if op not in _PUSH_IMM_REG_NAMES or i == 0:
            continue
        prev = decoded[sorted_addrs[i - 1]]
        if prev["mnemonic"] != "mov":
            continue
        pop = prev.get("op_str", "").strip().lower()
        if "," not in pop:
            continue
        dst, src = (s.strip() for s in pop.split(",", 1))
        if dst != op:
            continue
        imm = _parse_imm(src)
        if imm is None:
            continue
        fo = imm - cs_base
        if 0 <= fo < len(code_bytes):
            targets.add(fo)
    return targets


def _discover_jump_table_targets(
    decoded: dict, cs_base: int, code_bytes: bytes
) -> set[int]:
    """Indirect-jump table targets.

    Matches ``jmp word ptr [reg + disp]`` (any segment override). Looks back
    ~12 instructions for ``and reg, MASK`` to bound the table size; reads
    ``MASK + 1`` little-endian words from the binary at file offset
    ``disp - cs_base``. Each word is a cs-relative IP; returned as file
    offsets (``entry - cs_base``).
    """
    import re

    global _INDIRECT_JMP_RE
    if _INDIRECT_JMP_RE is None:
        _INDIRECT_JMP_RE = re.compile(
            r"\[\s*(?:[a-z]+\s*:\s*)?[a-z]+\s*\+\s*"
            r"(0x[0-9a-fA-F]+|[0-9a-fA-F]+h|\d+)\s*\]",
            re.IGNORECASE,
        )

    targets: set[int] = set()
    sorted_addrs = sorted(decoded)
    for i, addr in enumerate(sorted_addrs):
        ins = decoded[addr]
        if ins["mnemonic"] != "jmp":
            continue
        op = ins.get("op_str", "")
        m = _INDIRECT_JMP_RE.search(op)
        if not m:
            continue
        disp = _parse_imm(m.group(1))
        if disp is None:
            continue
        mask: int | None = None
        for j in range(i - 1, max(-1, i - 13), -1):
            prev = decoded[sorted_addrs[j]]
            if prev["mnemonic"] != "and":
                continue
            pop = prev.get("op_str", "").lower()
            parts = [p.strip() for p in pop.split(",", 1)]
            if len(parts) != 2:
                continue
            val = _parse_imm(parts[1])
            if val is not None and 0 < val < 0x100:
                mask = val
                break
        if mask is None:
            continue
        table_off = disp - cs_base
        n = mask + 1
        if table_off < 0 or table_off + n * 2 > len(code_bytes):
            continue
        for k in range(n):
            o = table_off + k * 2
            entry = code_bytes[o] | (code_bytes[o + 1] << 8)
            fo = entry - cs_base
            if 0 <= fo < len(code_bytes):
                targets.add(fo)
    return targets


def stage2(
    load_module: bytes,
    header: dict,
    outdir: Path,
    extra_entries: list[int] | None = None,
    skip_entry_0000: bool = False,
    cs_base: int | None = None,
) -> tuple[list, list, list, list, list]:
    """Instruction-level disassembly using control-flow analysis."""

    md = Cs(CS_ARCH_X86, CS_MODE_16)
    md.detail = True

    written_bytes: set[int] = set()

    def decode_eflags(mask: int) -> list:
        flags = set()
        for name, value in x86_const.__dict__.items():
            if name.startswith("X86_EFLAGS_") and mask & value:
                flags.add(name.rsplit("_", 1)[-1])
        return sorted(flags)

    prefix_seg = {
        0x2E: "CS",
        0x36: "SS",
        0x3E: "DS",
        0x26: "ES",
        0x64: "FS",
        0x65: "GS",
    }

    def default_segment(insn, op) -> str:
        """Return the segment register for a memory operand."""
        seg = md.reg_name(op.mem.segment)
        if seg:
            return seg.upper()

        for prefix in insn.prefix:
            seg_override = prefix_seg.get(prefix)
            if seg_override:
                return seg_override

        if op.mem.base in (x86_const.X86_REG_BP, x86_const.X86_REG_SP) or (
            op.mem.index in (x86_const.X86_REG_BP, x86_const.X86_REG_SP)
        ):
            return "SS"
        return "DS"

    # Entry point offset within load_module
    entry_off = header["e_cs"] * 16 + header["e_ip"]

    # ``visited`` tracks which functions have explored a given address while
    # ``decoded`` caches instructions so they can be shared across multiple
    # functions (e.g. when the same block is reachable from different entry
    # points).
    visited: dict[int, set[int]] = {}
    decoded: dict[int, dict] = {}
    decoded_owner: dict[int, int] = {}
    functions: dict[int, dict] = {}
    extern_labels: set[int] = set()

    # Seed the worklist with explicit entry points. When ``skip_entry_0000`` is
    # requested the default ``CS:IP`` pair is omitted, allowing callers to
    # decode only the supplied ``extra_entries``. This prevents a linear sweep
    # of the entire binary when the caller is interested in specific offsets.
    entry_points: list[int] = []
    if not skip_entry_0000:
        entry_points.append(entry_off)
    if extra_entries:
        out_of_range = [e for e in extra_entries if e >= len(load_module)]
        if out_of_range:
            raise SystemExit(
                "Refusing to seed disassembly with entries past end-of-file "
                f"(binary size 0x{len(load_module):X}): "
                + ", ".join(f"0x{e:X}" for e in out_of_range)
                + ". These are heuristic addresses with no backing bytes; "
                "remove them from the per-binary JSON or fix the address."
            )
        entry_points.extend(extra_entries)
    # The decode loop consumes ``worklist`` with ``pop()``, so seed entries in
    # reverse order to process the first declared entry first. This keeps
    # canonical starts (e.g. 0x0000) ahead of potentially noisy forced entries
    # and allows mid-instruction points to be rejected once ownership is known.
    worklist: list[tuple[int, int]] = [(off, off) for off in reversed(entry_points)]

    # Optional hard cap on total decoded instructions (env-driven). The JIT
    # sets this: decoding from a runtime
    # entry that turns out to point at zeros/data (a bad transfer target) would
    # otherwise follow fall-through through the whole image, generating an
    # enormous chunk. A cap bounds that to a finite, compilable chunk rather
    # than hanging the JIT compile. 0 == unlimited.
    max_insns = int(os.environ.get("SAISEI_DISASM_MAX_INSNS", "0") or "0")
    total_insns = 0
    # Linear base of the image being decoded. The JIT fallback dumps a single
    # 64KB segment based at cs<<4, so a far call/jump seg:off whose seg is THIS
    # segment must map to dump offset off (= seg*16+off - image_base), not
    # the absolute linear (which falls outside the 64KB dump and would be
    # skipped, leaving it undecoded). 0 == flat whole-image (no adjustment).
    image_base = int(os.environ.get("SAISEI_DISASM_IMAGE_BASE", "0") or "0", 0)

    def far_target_in_segment(seg: int) -> bool:
        """Whether a far ``lcall``/``ljmp`` ``seg:off`` should be decoded inline.

        A whole-image decode reads the relocated image as one flat space
        (``image_base == 0``): far transfers resolve to their absolute linear
        target and are followed inline -- keep that behavior.

        A JIT chunk is a SINGLE 64KB segment based at ``image_base``. A far
        transfer to a DIFFERENT segment runs under a different ``cs`` and is
        therefore a SEPARATE chunk, resolved on demand at runtime by
        ``lcall_table``/``dispatch_via_binary``. Real-mode segments overlap, so
        another segment's ``seg:off`` often has a linear address that lands
        inside THIS segment's 64KB window -- but decoding those bytes here
        mis-attributes another segment's code (frequently data at this cs) to
        the current chunk and walks the decoder into garbage. Only a far
        transfer back into THIS segment (``seg == image_base >> 4``) is
        genuinely in-chunk and gets an inline dispatch case.
        """
        if not image_base:
            return True
        return seg == (image_base >> 4)

    while worklist:
        addr, func_start = worklist.pop()
        func = functions.setdefault(
            func_start, {"start": func_start, "instructions": []}
        )
        cur = addr
        # An authoritative direct call/jmp target (the function's own entry)
        # may land inside a byte range already decoded by an overlapping
        # instruction stream. x86 explicitly permits overlapping code, so the
        # direct transfer is the source of truth: decode a fresh stream from
        # here even though those bytes are owned by another instruction. Once
        # committed, the stream's own fallthrough bytes are equally authoritative
        # (they belong to THIS instruction-stream alignment), so the overlap
        # guard is disabled for the rest of this inner decode loop.
        overlap_stream = (
            addr == func_start and (decoded_owner.get(addr) or addr) < addr
        )
        while True:
            if (
                func_start in visited.get(cur, set())
                or cur >= len(load_module)
            ):
                break

            owner = decoded_owner.get(cur)
            if owner is not None and owner < cur and not overlap_stream:
                # ``cur`` points into the middle of an already-decoded
                # instruction and this is NOT an authoritative overlapping
                # stream -- it's an off-by-one stray (e.g. a jump-table entry
                # pointing mid-instruction). Do not decode from the middle byte,
                # which would mint overlapping fake instructions (commonly ``db``
                # markers). (When ``overlap_stream`` is set, the leading
                # authoritative direct target deliberately decodes through the
                # overlap; leaving its function empty would lower to a bare
                # ``return;`` that never advances ip and spins at the call site.)
                break

            insn = decoded.get(cur)
            if insn is None:
                insn_list = list(md.disasm(load_module[cur:], cur, count=1))
                if not insn_list:
                    # Capstone failed to decode the next byte. Record it as a
                    # raw data byte so that later stages emit a "TODO ASM"
                    # comment instead of silently dropping it.
                    byte = load_module[cur]
                    insn = {
                        "address": cur,
                        "bytes": f"{byte:02x}",
                        "mnemonic": "db",
                        "op_str": f"0x{byte:02x}",
                        "detail": {},
                    }
                    decoded[cur] = insn
                else:
                    cs_insn = insn_list[0]
                    detail = {
                        "regs_read": [
                            md.reg_name(r).upper() for r in cs_insn.regs_read
                        ],
                        "regs_write": [
                            md.reg_name(r).upper() for r in cs_insn.regs_write
                        ],
                        "groups": [
                            md.group_name(g).upper() for g in cs_insn.groups
                        ],
                        "eflags": decode_eflags(cs_insn.eflags),
                    }
                    for prefix in cs_insn.prefix:
                        seg_override = prefix_seg.get(prefix)
                        if seg_override:
                            detail["seg_override"] = seg_override
                            break

                    mem_refs = []
                    for op in cs_insn.operands:
                        if op.type == x86_const.X86_OP_MEM:
                            seg = default_segment(cs_insn, op)
                            has_read = bool(op.access & CS_AC_READ)
                            has_write = bool(op.access & CS_AC_WRITE)
                            if has_read and has_write:
                                access = "readwrite"
                            elif has_write:
                                access = "write"
                            else:
                                access = "read"
                            disp = op.mem.disp
                            if op.mem.base == 0 and op.mem.index == 0:
                                disp &= 0xFFFF
                            mem_refs.append(
                                {
                                    "segment": seg,
                                    "disp": disp,
                                    "access": access,
                                }
                            )
                            if (
                                op.access & CS_AC_WRITE
                                and op.mem.base == 0
                                and op.mem.index == 0
                            ):
                                width = op.size or 1
                                for i in range(disp, disp + width):
                                    written_bytes.add(i)
                    if mem_refs:
                        detail["mem_refs"] = mem_refs

                    insn = {
                        "address": cs_insn.address,
                        "bytes": cs_insn.bytes.hex(),
                        # Normalise mnemonic to lowercase to simplify
                        # downstream analysis which performs
                        # case-sensitive comparisons.
                        "mnemonic": cs_insn.mnemonic.lower(),
                        "op_str": cs_insn.op_str,
                        "detail": detail,
                    }
                    decoded[cur] = insn

                insn_size = len(insn["bytes"]) // 2
                for off in range(cur, cur + insn_size):
                    prev_owner = decoded_owner.get(off)
                    if prev_owner is None or prev_owner > cur:
                        decoded_owner[off] = cur
                # An authoritative overlapping stream (a direct call/jmp target
                # that lands inside another instruction's byte range) must OWN
                # its own START byte, otherwise the IR-assembly filter -- which
                # keeps only instructions whose start byte they own -- drops it,
                # re-creating the empty ``return;`` function. We claim only the
                # leading byte: interior bytes stay owned by the lower-addressed
                # stream so its instructions remain intact (both overlapping
                # streams are real and both must appear in the flat IR).
                if overlap_stream and decoded_owner.get(cur) != cur:
                    decoded_owner[cur] = cur

            func["instructions"].append(insn)
            visited.setdefault(cur, set()).add(func_start)
            total_insns += 1
            if max_insns and total_insns >= max_insns:
                # Cap hit -- stop all further decoding (drop the rest of the
                # worklist) so the run produces a bounded chunk.
                worklist.clear()
                break

            size = len(insn["bytes"]) // 2
            mnemonic = insn["mnemonic"].lower()
            op_str = insn.get("op_str", "")

            # Treat DOS terminate sequence as function end.
            # ``int 0x20`` unconditionally terminates while ``int 0x21``
            # terminates (AH=4Ch) when preceded by ``mov ah, 0x4c`` or
            # ``mov ax, 0x4cXX`` -- AL is the exit code and may be ANY value,
            # not just 0x00 (e.g. SETUP exits with `mov ax, 0x4cff`). Missing
            # the nonzero-exit-code form makes the scan run past the int into
            # the error-string data, decoding garbage.
            if mnemonic == "int":
                if op_str == "0x20":
                    break
                if op_str == "0x21":
                    prev = (
                        func["instructions"][-2]
                        if len(func["instructions"]) >= 2
                        else None
                    )
                    terminate = False
                    if prev and prev["mnemonic"] == "mov":
                        pop = prev["op_str"]
                        if pop == "ah, 0x4c":
                            terminate = True
                        elif pop.startswith("ax, "):
                            v = _parse_imm(pop.split(",", 1)[1].strip())
                            if v is not None and (v >> 8) == 0x4C:
                                terminate = True
                    if terminate:
                        break

            # Helper to enqueue addresses
            def enqueue(target: int, new_func: bool = False) -> None:
                if not (0 <= target < len(load_module)):
                    return
                fs = target if new_func else func_start
                if fs not in visited.get(target, set()):
                    worklist.append((target, fs))
                    if new_func:
                        functions.setdefault(
                            target, {"start": target, "instructions": []}
                        )

            if mnemonic == "call":
                target = _parse_imm(op_str)
                if target is not None:
                    insn["target"] = target
                    enqueue(target, new_func=True)
                cur = insn["address"] + size
                continue
            if mnemonic == "lcall":
                seg_off = [
                    p.strip() for p in op_str.replace(":", ",").split(",")
                ]
                if len(seg_off) == 2:
                    seg = _parse_imm(seg_off[0])
                    off = _parse_imm(seg_off[1])
                    if seg is not None and off is not None:
                        target = seg * 16 + off - image_base
                        if far_target_in_segment(seg):
                            insn["target"] = target
                            enqueue(target, new_func=True)
                cur = insn["address"] + size
                continue
            if mnemonic == "jmp":
                target = _parse_imm(op_str)
                if target is not None:
                    insn["target"] = target
                    enqueue(target)
                else:
                    # Operand is not an immediate address (e.g. indirect jump).
                    # We can't determine the target, so no new work items are
                    # enqueued. The instruction itself is still recorded and
                    # the IR-to-C stage will emit a `// TODO ASM` comment to
                    # surface the unresolved jump in generated sources.
                    pass
                break
            if mnemonic == "ljmp":
                try:
                    seg_off = [
                        p.strip() for p in op_str.replace(":", ",").split(",")
                    ]
                    if len(seg_off) == 2:
                        seg, off = (int(seg_off[0], 16), int(seg_off[1], 16))
                        target = seg * 16 + off - image_base
                        if far_target_in_segment(seg):
                            insn["target"] = target
                            extern_labels.add(target)
                            enqueue(target, new_func=True)
                except ValueError:
                    pass
                break
            if mnemonic in {"ret", "retn", "retf", "hlt", "iret"}:
                break
            if mnemonic.startswith(("j", "loop")):
                try:
                    enqueue(int(op_str, 16))
                except ValueError:
                    pass
                fallthrough = insn["address"] + size
                enqueue(fallthrough)
                break
            cur = insn["address"] + size

        if not worklist and cs_base is not None:
            # Auto-discover targets reachable only by push-imm tail-calls or
            # indirect jump-table dispatch. Re-runs until fixed-point because
            # newly decoded code may expose more discovery sites. Gated on
            # cs_base being explicitly provided — without it we'd treat data
            # bytes as code addresses in resource archives.
            new_seeds = _discover_push_imm_targets(decoded, cs_base, load_module)
            new_seeds |= _discover_jump_table_targets(decoded, cs_base, load_module)
            new_seeds -= set(functions)
            for s in sorted(new_seeds):
                worklist.append((s, s))
                functions.setdefault(s, {"start": s, "instructions": []})

    # Sort instructions within each function and expose globally sorted streams
    for func in functions.values():
        func["instructions"] = [
            insn
            for insn in func["instructions"]
            if decoded_owner.get(insn["address"]) == insn["address"]
        ]
        func["instructions"].sort(key=lambda insn: insn["address"])

    functions_list = sorted(functions.values(), key=lambda f: f["start"])

    # Deduplicate instructions across functions by address. Some functions may
    # share basic blocks which previously resulted in the same instruction
    # appearing multiple times. Consolidate them into a mapping keyed by
    # address and then expose a globally sorted list.
    instructions_by_addr: dict[int, dict] = {}
    for func in functions_list:
        for insn in func["instructions"]:
            if decoded_owner.get(insn["address"]) != insn["address"]:
                continue
            instructions_by_addr.setdefault(insn["address"], insn)
    instructions = [
        instructions_by_addr[addr] for addr in sorted(instructions_by_addr)
    ]

    (outdir / "disasm.json").write_text(json.dumps(instructions, indent=2))
    (outdir / "functions.json").write_text(
        json.dumps(functions_list, indent=2)
    )

    # Determine data regions (bytes not part of any instruction)
    code_bytes = set()
    for insn in instructions:
        addr = insn["address"]
        size = len(insn["bytes"]) // 2
        for i in range(addr, addr + size):
            code_bytes.add(i)
    data_regions = []
    start = None
    for i in range(len(load_module)):
        if i not in code_bytes:
            if start is None:
                start = i
        elif start is not None:
            data_regions.append({"start": start, "end": i})
            start = None
    if start is not None:
        data_regions.append({"start": start, "end": len(load_module)})
    (outdir / "data_regions.json").write_text(
        json.dumps(data_regions, indent=2)
    )

    return (
        instructions,
        functions_list,
        data_regions,
        sorted(written_bytes),
        sorted(extern_labels),
    )


def compute_xrefs(instructions: list) -> list:
    """Derive simple cross references from instruction stream.

    ``instructions`` is expected to contain at most one entry per address. The
    sequence may originate from a mapping keyed by address, but this function
    accepts any iterable and performs a final deduplication/sort to guard
    against callers that provide an unsanitised list.
    """
    by_addr = {insn["address"]: insn for insn in instructions}
    sorted_addrs = sorted(by_addr)
    xrefs: list[str] = []
    for idx, addr in enumerate(sorted_addrs):
        insn = by_addr[addr]
        size = len(insn["bytes"]) // 2
        mnemonic = insn["mnemonic"].lower()
        op_str = insn["op_str"]
        if mnemonic in {"call", "lcall"}:
            target = insn.get("target")
            if target is None:
                try:
                    if mnemonic == "lcall":
                        parts = op_str.replace(":", ",").split(",")
                        seg_off = [p.strip() for p in parts]
                        if len(seg_off) == 2:
                            seg, off = (
                                int(seg_off[0], 16),
                                int(seg_off[1], 16),
                            )
                            target = seg * 16 + off
                        else:
                            raise ValueError
                    else:
                        target = int(op_str, 16)
                except ValueError:
                    target = None
            if target is not None:
                xrefs.append(f"call@0x{target:X}")
        if (
            mnemonic in {"jmp", "ljmp"}
            or mnemonic.startswith("j")
            or mnemonic.startswith("loop")
        ):
            target = insn.get("target")
            if target is None:
                try:
                    if mnemonic == "ljmp":
                        parts = op_str.replace(":", ",").split(",")
                        seg_off = [p.strip() for p in parts]
                        if len(seg_off) == 2:
                            seg, off = (
                                int(seg_off[0], 16),
                                int(seg_off[1], 16),
                            )
                            target = seg * 16 + off
                        else:
                            raise ValueError
                    else:
                        target = int(op_str, 16)
                except ValueError:
                    target = None
            if target is not None:
                xrefs.append(f"jump@0x{target:X}")
        for mem in insn.get("detail", {}).get("mem_refs", []):
            disp = mem["disp"]
            if disp < 0:
                disp_str = f"-0x{-disp:X}"
            else:
                disp_str = f"0x{disp:X}"
            xrefs.append(f"mem@{mem['segment']}:{disp_str}")
        if (
            mnemonic not in {"jmp", "ljmp", "hlt", "iret"}
            and mnemonic not in {"ret", "retn", "retf"}
        ):
            terminate = False
            if mnemonic == "int" and op_str == "0x20":
                terminate = True
            elif mnemonic == "int" and op_str == "0x21" and idx > 0:
                prev = by_addr[sorted_addrs[idx - 1]]
                if prev["mnemonic"].lower() == "mov":
                    prev_op = prev["op_str"].lower().replace(" ", "")
                    if prev_op in {"ax,0x4c00", "ah,0x4c"}:
                        terminate = True
            if not terminate:
                next_addr = insn["address"] + size
                xrefs.append(f"fallthrough@0x{next_addr:X}")
    return xrefs


def stage3(
    metadata: dict,
    instructions: list,
    functions: list,
    data_regions: list,
    xrefs: list,
    written: list,
    extern_labels: list,
    outdir: Path,
) -> None:
    """Merge metadata, disassembly, and xrefs into a single JSON IR.

    ``instructions`` is expected to have at most one entry per address but may
    arrive in any order. Normalise by collapsing to a mapping keyed by address
    and then rebuild a sorted list for emission.
    """
    by_addr = {insn["address"]: insn for insn in instructions}
    instructions_list = [by_addr[a] for a in sorted(by_addr)]

    program_ir = {
        "header": metadata["header"],
        "code": instructions_list,
        "functions": functions,
        "data_regions": data_regions,
        "data": metadata["load_module"].hex(),
        "relocations": metadata["relocations"],
        "segment_map": metadata["segment_map"],
        "xrefs": xrefs,
        "extern_labels": extern_labels,
    }
    (outdir / "program.ir.json").write_text(json.dumps(program_ir, indent=2))
    (outdir / "xrefs.json").write_text(json.dumps({"xrefs": xrefs}, indent=2))


def main() -> None:
    args = parse_args()
    outdir = Path(args.outdir)
    ensure_dir(outdir)

    data = Path(args.input).read_bytes()
    res = stage1(data, outdir, args.entry)
    instructions, functions, data_regions, writes, extern_labels = stage2(
        res["load_module"],
        res["header"],
        outdir,
        args.entry,
        args.skip_entry_0000,
        cs_base=args.cs_base,
    )
    xrefs = compute_xrefs(instructions)
    stage3(
        res,
        instructions,
        functions,
        data_regions,
        xrefs,
        writes,
        extern_labels,
        outdir,
    )


if __name__ == "__main__":
    main()
