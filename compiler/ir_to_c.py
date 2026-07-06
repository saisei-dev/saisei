#!/usr/bin/env python3
"""Translate program IR into very rough C source.

This utility performs a best-effort structuring pass over the JSON IR produced
by :mod:`compiler.disassemble` and emits C code with high level control flow
constructs where possible.  The translation is intentionally conservative –
instructions that cannot be mapped cleanly are preserved in the output as
comments so that no behaviour is lost.

The goal is not to produce perfect C, but to bootstrap the decompilation
process by identifying obvious ``while`` loops and ``if`` statements.  More
advanced reconstruction (switch statements, nested flow, variable recovery,
etc.) can build on top of this foundation.
"""

from __future__ import annotations

import argparse
import json
import logging
import os
import re
import sys
from collections import Counter
from pathlib import Path
from typing import Callable, Dict, Iterable, List, Set, Tuple, TYPE_CHECKING


logger = logging.getLogger(__name__)


# ``ir_to_c.py`` can be executed both as a module and as a standalone script.
# Import ``BasicBlock`` in a way that works for both invocation styles.
if __package__ is None:  # pragma: no cover - allow running as a script
    sys.path.append(str(Path(__file__).resolve().parent.parent))
    from basic_block import BasicBlock  # type: ignore
else:  # pragma: no cover - imported as part of package
    from .basic_block import BasicBlock
# ---------------------------------------------------------------------------
# Memory structure metadata
# ---------------------------------------------------------------------------

# Addresses within the code segment used to store the parent stack state when
# spawning a child process via ``dos_exec``.
_EXEC_PARAM_FIELDS: Dict[int, str] = {
    0x8C2: "exec_params.saved_sp",
    0x8C4: "exec_params.saved_ss",
}

# Resident control block located at ``es:0xFF00``.  The keys here are absolute
# offsets within the segment and the values give the field types inferred from
# how each offset is accessed.  Friendly field names are resolved separately
# when generating references.
_RCB_FIELD_TYPES: Dict[int, str] = {
    0xFF00: "uint16_t",
    0xFF02: "uint16_t",
    0xFF04: "uint16_t",
    0xFF06: "uint16_t",
    0xFF08: "uint8_t",
    0xFF09: "uint8_t",
    0xFF0A: "uint8_t",
    0xFF0B: "uint8_t",
    0xFF0C: "uint16_t",
    0xFF0E: "uint16_t",
    0xFF10: "uint16_t",
    0xFF12: "uint16_t",
    0xFF14: "uint8_t",
    0xFF15: "uint8_t",
    0xFF16: "uint8_t",
    0xFF17: "uint8_t",
    0xFF18: "uint16_t",
    0xFF1D: "uint8_t",
    0xFF1E: "uint8_t",
    0xFF1F: "uint16_t",
    0xFF26: "uint8_t",
    0xFF27: "uint8_t",
    0xFF28: "uint8_t",
    0xFF2C: "uint16_t",
    0xFF33: "uint8_t",
    0xFF34: "uint8_t",
    0xFF38: "uint8_t",
    0xFF39: "uint8_t",
    0xFF3A: "uint8_t",
    0xFF3B: "uint8_t",
    0xFF3C: "uint8_t",
    0xFF40: "uint8_t",
    0xFF42: "uint8_t",
    0xFF43: "uint8_t",
    0xFF74: "uint8_t",
    0xFF75: "uint8_t",
    0xFF78: "uint8_t",
    0xFF79: "uint16_t",
    0xFF7B: "uint16_t",
}

# RCB field names come from scripts/include/rcb_fields.h — the single
# hand-maintained source for the Resident Control Block layout (it is also
# compiled into the runtime). Parse its enum so generated code can name RCB
# accesses (e.g. rcb_read16(DATA_BUF1_OFF)) instead of raw offsets.
try:
    _rcb_header = Path(__file__).resolve().parent.parent / "runtime" / "include" / "rcb_fields.h"
    _rcb_text = _rcb_header.read_text()
    _RCB_FIELD_ALIASES: Dict[int, str] = {
        int(m.group(2), 16): m.group(1)
        for m in re.finditer(r"(\w+)\s*=\s*(0x[0-9A-Fa-f]+)", _rcb_text)
    }
    _RCB_FIELD_NAMES: Set[str] = set(_RCB_FIELD_ALIASES.values())
except FileNotFoundError:
    _RCB_FIELD_ALIASES = {}
    _RCB_FIELD_NAMES = set()

# Combined lookup table mapping ``(segment, offset)`` pairs to the friendly
# field reference used in output.  Only the exec parameter slots are expanded
# to named fields.  Resident control block accesses are handled separately to
# emit direct ``memb``/``memw`` helpers against ``es:0xFF00``.
_MEMORY_FIELD_MAP: Dict[tuple[str, int], str] = {
    ("cs", off): name for off, name in _EXEC_PARAM_FIELDS.items()
}

# Regex for parsing operands like ``word ptr es:[di+4]``.  Captures the access
# size (``byte``/``word``), optional segment prefix and the inner address
# expression.
# Regex for ``word ptr var_4`` style stack-local references.
_MEM_PTR_RE = re.compile(
    r"^(byte|word) ptr (?:(cs|ds|es|ss):)?\[(.+)\]$",
    re.IGNORECASE,
)
_VAR_RE = re.compile(r"^var_([0-9a-f]+)$", re.IGNORECASE)

# Set of general-purpose 16-bit registers used for register-based jumps.
_JMP_REGS = {"ax", "bx", "cx", "dx", "si", "di", "bp", "sp"}


def _match_rcb_access(op: str) -> tuple[str, str] | None:
    """Return (size, field) if ``op`` references an RCB field via ``es``."""

    m = re.match(
        r"mem([bw])\(es, (0x[0-9a-f]+|[a-z0-9_]+)\)$",
        op,
        re.IGNORECASE,
    )
    if not m:
        return None

    size, field = m.groups()
    field_upper = field.upper()
    if field_upper in _RCB_FIELD_NAMES:
        return size, field_upper

    try:
        base = 16 if field.lower().startswith("0x") else 10
        off = int(field, base)
    except ValueError:
        return None

    alias = _RCB_FIELD_ALIASES.get(off)
    if alias:
        return size, alias
    return None


def _rewrite_mem_op(op: str, seg: str | None = None) -> str:
    """Rewrite ``byte/word ptr`` memory operands into C helpers.

    Operands referencing known struct fields are replaced with the field name.
    Any other memory reference is emitted via ``memb``/``memw`` helper macros
    using ``ds`` as the default segment when none is specified.  Plain
    ``byte/word ptr var`` expressions (produced by BP-relative decoding) are
    reduced to the variable name without additional decoration.  When no
    segment is provided, ``bp``/``sp`` based addressing defaults to ``ss``.
    """

    match = _MEM_PTR_RE.match(op)
    if match:
        size, seg_prefix, expr = match.groups()
        expr_l = expr.lower()
        if seg_prefix:
            seg = seg_prefix.lower()
        elif seg:
            seg = seg.lower()
        elif re.search(r"\b(?:bp|sp)\b", expr_l):
            seg = "ss"
        else:
            seg = "ds"
        # Only map constant expressions to named fields
        try:
            base = 16 if expr_l.startswith(("0x", "-0x")) else 10
            off = int(expr_l, base)
            off &= 0xFFFF
        except ValueError:
            off = None
        if off is not None:
            # Expand exec parameter slots to their named fields when the
            # access lands within the ``cs`` segment table.
            mapped = _MEMORY_FIELD_MAP.get((seg, off))
            if mapped:
                return mapped
            # The resident control block resides at ``es:0xFF00``.  Translate
            # these accesses into direct ``memb``/``memw`` helper invocations
            # so the generated code writes straight into emulated memory.
            if seg == "es" and off in _RCB_FIELD_TYPES:
                macro = (
                    "memb" if _RCB_FIELD_TYPES[off] == "uint8_t" else "memw"
                )
                alias = _RCB_FIELD_ALIASES.get(off)
                addr = alias if alias else f"0x{off:04X}"
                return f"{macro}(es, {addr})"
        macro = "memb" if size.lower() == "byte" else "memw"
        # Real-mode effective addresses are 16-bit and wrap within the segment:
        # the offset is (base + index + disp) mod 0x10000. Mask any COMPUTED
        # address so a 16-bit overflow or underflow wraps inside the segment
        # instead of spilling into the neighbouring one. Without this a
        # sign-extended negative distance in an LZEXE back-reference
        # (`mov al, es:[bx+di]` with bx=0xFF.., bx+di>0xFFFF) reads the next
        # segment, and `[di-0x75]` with di<0x75 reads a negative offset --
        # both corrupt the decode. A bare register or constant can't wrap, so
        # it's left alone to keep the common case clean.
        if re.search(r"[+\-]", expr):
            return f"{macro}({seg}, ({expr}) & 0xFFFF)"
        return f"{macro}({seg}, {expr})"

    # ``word ptr var_4`` style operands
    simple = re.match(r"^(?:byte|word) ptr (.+)$", op)
    if simple:
        return simple.group(1)
    return op


def _operand_width_from_str(expr: str | None) -> int | None:
    """Return the operand width in bits when it can be inferred from *expr*."""

    if not expr:
        return None
    expr_l = expr.strip().lower()
    if expr_l in {"al", "ah", "bl", "bh", "cl", "ch", "dl", "dh"}:
        return 8
    if expr_l in {
        "ax",
        "bx",
        "cx",
        "dx",
        "si",
        "di",
        "bp",
        "sp",
    }:
        return 16
    if expr_l.startswith("memb("):
        return 8
    if expr_l.startswith("memw("):
        return 16
    return None


def _split_mem_accessor(expr: str) -> tuple[str, str] | None:
    """Return the helper macro and inner expression for memory accesses.

    ``expr`` is expected to be of the form ``memb(seg, off)`` or
    ``memw(seg, off)``.  When the expression does not match this pattern the
    function returns ``None``.
    """

    if expr.startswith("memw(") and expr.endswith(")"):
        return "memw", expr[5:-1]
    if expr.startswith("memb(") and expr.endswith(")"):
        return "memb", expr[5:-1]
    return None


def _emit_store(lines: List[str], dest: str, value: str, *, indent: str = "") -> None:
    """Append an assignment to ``dest`` using helper macros when needed."""

    mem_access = _split_mem_accessor(dest)
    if mem_access:
        helper, inner = mem_access
        lines.append(f"{indent}{helper}_write({inner}, {value});")
    else:
        lines.append(f"{indent}{dest} = {value};")


def _value_expr(dest: str) -> str:
    """Return an expression that reads the current value of ``dest``."""

    mem_access = _split_mem_accessor(dest)
    if mem_access:
        helper, inner = mem_access
        return f"{helper}({inner})"
    return dest


def _emit_struct_include() -> List[str]:
    """Return include directives for shared definitions.

    The generated C files rely on ``NULL`` for placeholder pointers when
    emitting shim log calls.  ``NULL`` lives in ``stddef.h`` so ensure it's
    always included alongside ``shims.h``.
    """

    return [
        "#include <stddef.h>",
        "#include <string.h>",
        "#include <stdio.h>",
        "#include <stdlib.h>",
        "#include \"shims.h\"",
        "#include \"rcb_fields.h\"",
        "",
    ]


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("ir", help="Path to program.ir.json")
    parser.add_argument("exe", help="Path to original executable")
    parser.add_argument(
        "--out",
        default="program.c",
        help="Output path for generated C code",
    )
    parser.add_argument(
        "--metadata",
        help=(
            "Optional JSON file mapping function addresses to friendly names"
        ),
    )
    parser.add_argument(
        "--prefix",
        default="",
        help="Prefix to apply to generated symbol names",
    )
    parser.add_argument(
        "--image-base",
        default=None,
        help=(
            "Linear base address of file offset 0 in the loaded image "
            "(hex/dec). For a runtime JIT chunk this is the segment base "
            "(cs<<4) the dump was taken at, so the near-call IP conversion "
            "collapses to the identity. Defaults to SAISEI_DISASM_IMAGE_BASE "
            "when set, else the load segment << 4."
        ),
    )
    return parser.parse_args()


def build_basic_blocks(
    instrs: List[dict],
    extra_leaders: Set[int] | None = None,
    func_start: int | None = None,
) -> Dict[int, BasicBlock]:
    """Partition instruction stream into basic blocks.

    ``func_start`` is the function's own entry address. It is, by definition, a
    direct call/jmp target (that is why it is a function), so it must be
    protected from the overlapping-instruction cleaner and forced as a block
    leader -- even when it lands inside the byte range of an instruction from a
    different, overlapping instruction stream (x86 permits overlapping code).
    Without this, an entry that overlaps a preceding instruction is dropped and
    the function's first real instructions vanish, leaving a body that starts
    mid-routine (observed: ``call 0x151ba`` -> the get-version prologue at
    0x51BA-0x51BD silently disappearing).
    """

    if not instrs:
        return {}

    # ``program.ir.json`` now guarantees instructions are provided in ascending
    # address order as the disassembler sorts them prior to emission.  Earlier
    # versions defensively sorted the stream here to handle out-of-order input,
    # but this is no longer necessary.

    # Some binaries contain spurious instructions whose start address falls
    # within the byte range of a previous instruction.  These usually stem
    # from decoding errors around control flow instructions (e.g. the second
    # byte of a short jump being interpreted as an opcode).  Such overlap
    # confuses later passes which expect a linear, non-overlapping stream and
    # can prevent loops and conditionals from being recognised correctly.
    #
    # Filter out any instruction that overlaps with its predecessor before
    # computing leaders/blocks so the resulting CFG reflects the actual
    # execution order.
    cleaned: List[dict] = []
    prev_addr = None
    prev_size = 0
    targets: Set[int] = set()
    if func_start is not None:
        # The function entry is an external direct-call/jmp target; it must
        # survive the overlap cleaner even though no instruction WITHIN this
        # function references it.
        targets.add(func_start)
    for insn in instrs:
        mnem = insn.get("mnemonic", "")
        if (
            mnem.startswith("j")
            or mnem == "call"
            or mnem == "lcall"
            or mnem.startswith("loop")
            or mnem == "ljmp"
        ):
            tgt = insn.get("target")
            if tgt is None:
                tgt = _parse_imm(insn.get("op_str", ""))
            if tgt is not None:
                targets.add(tgt)
    for insn in instrs:
        addr = insn["address"]
        size = len(insn.get("bytes", "")) // 2
        if (
            prev_addr is not None
            and prev_size > 1
            and addr < prev_addr + prev_size
            and addr not in targets
        ):
            continue
        cleaned.append(insn)
        prev_addr = addr
        prev_size = size
    instrs = cleaned

    for prev, curr in zip(instrs, instrs[1:]):
        prev["_next_addr"] = curr["address"]

    addr_set = {insn["address"] for insn in instrs}
    leaders = {instrs[0]["address"]}
    if func_start is not None and func_start in addr_set:
        leaders.add(func_start)
    if extra_leaders:
        leaders.update(extra_leaders)

    for insn in instrs:
        mnem = insn["mnemonic"]
        size = len(insn["bytes"]) // 2
        next_addr = insn["address"] + size

        if (
            mnem.startswith("j")
            or mnem == "call"
            or mnem == "lcall"
            or mnem.startswith("loop")
            or mnem == "ljmp"
        ):
            target = insn.get("target")
            if target is None:
                target = _parse_imm(insn["op_str"])
            if target is not None and target in addr_set:
                leaders.add(target)
            if next_addr in addr_set:
                leaders.add(next_addr)
        elif mnem in {"ret", "retn", "retf", "hlt", "iret"}:
            continue

    leaders = sorted(leaders)
    blocks: Dict[int, BasicBlock] = {}

    for i, start in enumerate(leaders):
        end = leaders[i + 1] if i + 1 < len(leaders) else None
        block = BasicBlock(start)
        for insn in instrs:
            addr = insn["address"]
            if addr < start:
                continue
            if end is not None and addr >= end:
                break
            block.instructions.append(insn)
        if block.instructions:
            blocks[start] = block

    return blocks


def normalize_flags(instrs: List[dict]) -> List[dict]:
    """Fold flag-setting ops followed by Jcc into the jump instruction.

    Sequences like ``cmp/test/or`` immediately followed by a conditional
    jump are common for implementing simple boolean tests.  For the purposes
    of structuring, it's easier if the condition-setting instruction is
    attached directly to the subsequent jump so later passes don't need to
    reason about trailing ``cmp``/``test`` operations.  This function marks
    the flag-setting instruction to be skipped and stores it on the Jcc under
    ``cond_prev``.
    """

    if not instrs:
        return []

    normalized: List[dict] = []
    last_cond: dict | None = None

    def _should_skip_flag_set(insn: dict) -> bool:
        """Do not elide flag-setting instructions before conditional jumps.

        Earlier versions skipped ``cmp``/``test``/``or`` sequences and rebuilt
        the branch condition from operand expressions.  That optimisation could
        misbehave when later code modified the compared values.  By always
        keeping the original instruction we allow subsequent branches to read
        the explicit CPU flags, ensuring correctness."""

        return False

    _REG_ALIASES = {
        "al": {"al", "ax"},
        "ah": {"ah", "ax"},
        "ax": {"al", "ah", "ax"},
        "bl": {"bl", "bx"},
        "bh": {"bh", "bx"},
        "bx": {"bl", "bh", "bx"},
        "cl": {"cl", "cx"},
        "ch": {"ch", "cx"},
        "cx": {"cl", "ch", "cx"},
        "dl": {"dl", "dx"},
        "dh": {"dh", "dx"},
        "dx": {"dl", "dh", "dx"},
    }

    def _expand_reg_aliases(regs: Set[str]) -> Set[str]:
        expanded: Set[str] = set()
        for reg in regs:
            expanded.update(_REG_ALIASES.get(reg, {reg}))
        return expanded

    def _clobbers_last_cond(insn: dict, last: dict | None) -> bool:
        if not last:
            return False
        cond_detail = last.get("detail", {})
        insn_detail = insn.get("detail", {})
        cond_regs = _expand_reg_aliases(set(cond_detail.get("regs_read", [])))
        insn_regs = _expand_reg_aliases(set(insn_detail.get("regs_write", [])))
        if cond_regs & insn_regs:
            return True

        def _mem_key(mr: dict) -> tuple:
            return (
                mr.get("segment"),
                mr.get("base"),
                mr.get("index"),
                mr.get("scale"),
                mr.get("disp"),
            )

        cond_mem = {
            _mem_key(mr)
            for mr in cond_detail.get("mem_refs", [])
            if mr.get("access") in {"read", "readwrite"}
        }
        insn_mem = {
            _mem_key(mr)
            for mr in insn_detail.get("mem_refs", [])
            if mr.get("access") in {"write", "readwrite"}
        }
        return bool(cond_mem & insn_mem)

    flag_setters = {
        "cmp",
        "test",
        "add",
        "sub",
        "xor",
        "adc",
        "sbb",
        "shl",
        "shr",
        "sal",
        "sar",
        "rol",
        "ror",
        "rcl",
        "rcr",
        "or",
        "and",
        "stc",
        "clc",
        "cmc",
        "repe cmpsb",
        "scasb",
        "dec",
        "inc",
    }

    for insn in instrs:
        mnem = insn.get("mnemonic", "")
        if mnem in flag_setters:
            last_cond = insn
            normalized.append(insn)
            continue

        if (
            (mnem.startswith("j") or mnem.startswith("loop"))
            and mnem != "jmp"
            and last_cond is not None
        ):
            if _should_skip_flag_set(last_cond):
                last_cond["skip"] = True
            new_insn = dict(insn)
            new_insn["cond_prev"] = last_cond
            normalized.append(new_insn)
            continue

        normalized.append(insn)
        if _clobbers_last_cond(insn, last_cond) or mnem in {"int", "popf"}:
            last_cond = None
    return normalized


def normalize_indirect_jumps(instrs: List[dict]) -> List[dict]:
    """Annotate indirect near jumps with explicit IR fields."""

    if not instrs:
        return []

    normalized: List[dict] = []
    for insn in instrs:
        if insn.get("mnemonic") == "jmp":
            op = insn.get("op_str", "").lower().strip()
            match = _MEM_PTR_RE.match(op)
            if match:
                size, seg_prefix, expr = match.groups()
                mem_refs = insn.get("detail", {}).get("mem_refs", [])
                seg = (
                    seg_prefix.lower()
                    if seg_prefix
                    else mem_refs[0].get("segment", "ds").lower()
                    if mem_refs
                    else "ds"
                )
                width = 8 if size.lower() == "byte" else 16
                new_insn = dict(insn)
                new_insn["op"] = "INDIRECT_NEAR_JMP"
                new_insn["seg"] = seg
                new_insn["ea"] = expr
                new_insn["width"] = width
                normalized.append(new_insn)
                continue
            if op in _JMP_REGS:
                new_insn = dict(insn)
                new_insn["op"] = "INDIRECT_NEAR_JMP"
                new_insn["reg"] = op
                normalized.append(new_insn)
                continue
        normalized.append(insn)
    return normalized


def jcc_condition(
    mnemonic: str, prev: dict | None = None, address: int | None = None
) -> str:
    """Return a human readable condition for a Jcc mnemonic."""
    mnemonic = mnemonic.lower()

    if mnemonic == "jmp":
        return "1"

    if mnemonic == "loop":
        return "--cx != 0"
    if mnemonic in {"loopne", "loopnz"}:
        cond = jcc_condition("jnz", prev)
        return f"--cx != 0 && {cond}"
    if mnemonic in {"loope", "loopz"}:
        cond = jcc_condition("jz", prev)
        return f"--cx != 0 && {cond}"

    if mnemonic in {"jcxz", "jecxz"}:
        reg = "cx" if mnemonic == "jcxz" else "ecx"
        return f"{reg} == 0"

    mapping = {
        "jz": "ZF == 1",
        "je": "ZF == 1",
        "jnz": "ZF == 0",
        "jne": "ZF == 0",
        "jc": "CF == 1",
        "jnc": "CF == 0",
        "js": "SF == 1",
        "jns": "SF == 0",
        "jo": "OF == 1",
        "jno": "OF == 0",
        "jpo": "PF == 0",
        "jpe": "PF == 1",
        "jnp": "PF == 0",
        "jp": "PF == 1",
        "jg": "ZF == 0 && SF == OF",
        "jge": "SF == OF",
        "jl": "SF != OF",
        "jle": "ZF == 1 || SF != OF",
        "ja": "CF == 0 && ZF == 0",
        "jae": "CF == 0",
        "jb": "CF == 1",
        "jbe": "CF == 1 || ZF == 1",
        "jna": "CF == 1 || ZF == 1",
        "jnbe": "CF == 0 && ZF == 0",
        "jnb": "CF == 0",
        "jnae": "CF == 1",
        "jnge": "SF != OF",
        "jng": "ZF == 1 || SF != OF",
        "jnl": "SF == OF",
        "jnle": "ZF == 0 && SF == OF",
    }
    cond = mapping.get(mnemonic)
    if cond is not None:
        return cond
    if address is not None:
        return f"/* unsupported jcc at 0x{address:04X} */"
    return "/* unsupported jcc */"


def _negate_condition(cond: str) -> str:
    match = re.match(r"^(.*)\s(==|!=|<=|>=|<|>)\s(.*)$", cond)
    if match:
        left, op, right = match.groups()
        inverse = {
            "==": "!=",
            "!=": "==",
            "<": ">=",
            ">": "<=",
            "<=": ">",
            ">=": "<",
        }[op]
        return f"{left} {inverse} {right}"
    return f"!({cond})"


def _parse_imm(value: str) -> int | None:
    """Best-effort parse of an immediate value from assembly syntax."""

    value = value.strip()
    # Ignore complex expressions such as memory references (``[bx+0x10]``) or
    # far pointers.  Only plain immediates are supported.
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
    elif token.isdigit():
        # Bare decimal immediates from disassembly (for example ``retf 12``)
        # should keep decimal semantics. Treating these as hexadecimal can
        # over-adjust the emulated stack and cause return-frame drift.
        pass
    else:
        token = f"0x{token}"
    try:
        return int(f"{sign}{token}", 0)
    except ValueError:
        return None


def _operand_width(op: str) -> int:
    """Return operand width in bits inferred from *op*."""

    op_l = op.lower()
    if op_l.startswith("memb("):
        return 8
    if op_l.startswith("memw("):
        return 16
    if op_l in {"al", "ah", "bl", "bh", "cl", "ch", "dl", "dh"}:
        return 8
    return 16


def _fix_negative_imm(op: str, other: str) -> str:
    """Rewrite negative immediates to unsigned hex based on *other* width."""

    imm = _parse_imm(op)
    if imm is None or imm >= 0:
        return op
    width = _operand_width(other)
    mask = (1 << width) - 1
    imm &= mask
    return f"0x{imm:0{width // 4}X}"


_BP_VAR_RE = re.compile(
    r"\[(?:[bs]p)\s*([+-])\s*(0x[0-9a-fA-F]+|\d+)\]",
    re.IGNORECASE,
)

_BP_INNER_RE = re.compile(
    r"^(?:[bs]p)\s*([+-])\s*(0x[0-9a-fA-F]+|\d+)$",
    re.IGNORECASE,
)

_DOS_INT_CALLS = {
    0x20: "dos_exit",
}

# Map AH register values for INT 21h to higher level DOS APIs.  Each entry
# provides the friendly function name.  Unknown functions fall back to
# ``dos_api_ah_XX``.
_DOS_API_FUNCS: Dict[int, str] = {
    0x01: "dos_read_char",
    0x02: "dos_write_char",
    0x06: "dos_direct_console_io",
    0x07: "dos_console_input_no_echo",
    0x2A: "dos_get_date",
    0x2C: "dos_get_time",
    0x0B: "dos_check_keyboard_status",
    0x0D: "dos_reset_disk",
    0x0E: "dos_select_drive",
    0x10: "dos_close_fcb",
    0x09: "dos_print_string",
    0x19: "dos_get_current_drive",
    0x1A: "dos_set_dta",
    0x25: "dos_set_interrupt_vector",
    0x36: "dos_get_disk_free_space",
    0x30: "dos_get_version",
    0x35: "dos_get_interrupt_vector",
    0x39: "dos_make_dir",
    0x3B: "dos_change_dir",
    0x0C: "dos_flush_and_read",
    0x2F: "dos_get_dta",
    0x3C: "dos_create_file",
    0x3D: "dos_open_file",
    0x3E: "dos_close_file",
    0x4F: "dos_find_next",
    0x3F: "dos_read_file",
    0x40: "dos_write_file",
    0x41: "dos_delete_file",
    0x43: "dos_get_file_attributes",
    0x44: "dos_ioctl",
    0x47: "dos_get_current_dir",
    0x42: "dos_lseek",
    0x4A: "dos_resize_mem",
    0x4E: "dos_find_first",
    0x48: "dos_alloc_mem",
    0x49: "dos_free_mem",
    0x4B: "dos_exec",
    0x4C: "dos_exit",
    0x4D: "dos_get_return_code",
    0x56: "dos_rename",
}

# Calls that do not return and therefore do not propagate CF back to the
# caller.
_DOS_NORET = {0x20, 0x4C}

# Function names corresponding to non-returning DOS calls.  These are derived
# from ``_DOS_INT_CALLS`` and ``_DOS_API_FUNCS`` so that dispatch blocks can
# identify terminating calls like ``dos_exit``.
_NORETURN_FUNCS = {
    name
    for code, name in _DOS_INT_CALLS.items()
    if code in _DOS_NORET
} | {
    name
    for ah, name in _DOS_API_FUNCS.items()
    if ah in _DOS_NORET
} | {
    # Faithful flat model: each of these sets cpu.r_cs:cpu.r_ip (and adjusts the
    # emulated stack) then returns; control has left this chunk's pc-space, so
    # the chunk must exit to the top-level loop. Treat them as terminating so a
    # `return;` is emitted after each.
    "retf",
    "retf_pop",
    "long_jump",
    "iret",
    "lcall_table",
    "call_table",
    "jump_table",
}


# Map BIOS interrupt numbers to helper functions that do not rely on AH.
_BIOS_INT_CALLS = {
    0x16: "bios_keyboard",
}

# Map AH values for BIOS interrupt calls to higher level helpers.  Each entry
# is keyed by the interrupt number and contains a mapping of AH values to
# function names.  Unknown functions fall back to ``bios_int_XX_ah_YY``.
_BIOS_API_FUNCS: Dict[int, Dict[int, str]] = {
    0x10: {
        0x00: "bios_set_video_mode",
        0x02: "bios_set_cursor_position",
        0x09: "bios_write_char_attr",
        0x0A: "bios_write_char_only",
        0x0B: "bios_set_cga_palette",
        0x0E: "bios_teletype_output",
        0x10: "bios_set_palette",
        0x12: "bios_video_alt_select",
    },
}

# Match a function name followed directly by '(' with no preceding backslash.
_CALL_RE = re.compile(r'^([a-zA-Z_][a-zA-Z0-9_]*)\(')


def _decode_variables(op_str: str) -> str:
    """Replace BP-relative memory operands with pseudo variable names.

    This function walks the operand string and rewrites any occurrences of
    ``bp`` or ``sp`` relative addressing into pseudo variable names.  Nested
    expressions such as ``[si + [bp - 4]]`` are handled recursively so that
    all variables are decoded correctly.
    """

    def bp_name(sign: str, num: str) -> str:
        try:
            offset = int(num, 16 if num.lower().startswith("0x") else 10)
        except ValueError:
            return f"bp {sign} {num}"
        return f"var_{offset:X}"

    def recurse(expr: str) -> str:
        """Recursively decode any BP-relative expressions within *expr*."""

        result = ""
        i = 0
        while i < len(expr):
            ch = expr[i]
            if ch == "[":
                depth = 1
                j = i + 1
                while j < len(expr) and depth:
                    if expr[j] == "[":
                        depth += 1
                    elif expr[j] == "]":
                        depth -= 1
                    j += 1
                segment = expr[i:j]
                match = _BP_VAR_RE.fullmatch(segment)
                if match:
                    sign = match.group(1)
                    if sign == "-":
                        result += bp_name(sign, match.group(2))
                    else:
                        result += segment
                else:
                    inner = recurse(segment[1:-1])
                    result += f"[{inner}]"
                i = j
            else:
                result += ch
                i += 1

        # Replace any remaining standalone bp/sp occurrences that appear
        # outside of simple bracketed expressions.
        match = _BP_INNER_RE.fullmatch(result.strip())
        if match:
            if match.group(1) == "-":
                return bp_name(match.group(1), match.group(2))
            return f"bp + {match.group(2)}"
        return result

    return recurse(op_str)


def propagate_flag_conditions(
    blocks: Dict[int, BasicBlock], graph: "nx.DiGraph"
) -> None:
    """Attach prior flag-setting instruction to leading Jcc in a block."""

    for addr, block in blocks.items():
        if not block.instructions:
            continue
        first = block.instructions[0]
        mnem = first.get("mnemonic", "")
        if not (mnem.startswith("j") and mnem != "jmp"):
            continue
        if "cond_prev" in first:
            continue
        preds = list(graph.predecessors(addr))
        if len(preds) != 1:
            continue
        pred_block = blocks.get(preds[0])
        if not pred_block or not pred_block.instructions:
            continue
        prev = None
        for candidate in reversed(pred_block.instructions):
            mnem_cand = candidate.get("mnemonic")
            if mnem_cand in {
                "cmp",
                "test",
                "add",
                "sub",
                "xor",
                "adc",
                "sbb",
                "shl",
                "shr",
                "sal",
                "sar",
                "rol",
                "ror",
                "rcl",
                "rcr",
                "or",
                "and",
                "stc",
                "clc",
                "cmc",
            }:
                prev = candidate
                break
            if mnem_cand not in {
                "mov",
                "lea",
                "nop",
                "push",
                "pop",
                "jmp",
            }:
                break
        if prev is None:
            continue
        first["cond_prev"] = prev


# Pattern detection strategies
from compiler import cfg  # noqa: E402
from compiler.ast_nodes import (  # type: ignore  # noqa: E402
    ASTNode,
    BasicBlockNode,
    CommentNode,
    CallNode,
    GotoNode,
    IfElseNode,
    ReturnNode,
    DoWhileNode,
)
from compiler.patterns import (  # noqa: E402
    PatternContext,
    detect_switch,
    detect_for_loop,
    detect_loop,
    detect_if_else,
)

if TYPE_CHECKING:  # pragma: no cover
    import networkx as nx


class UnsupportedInstructionError(Exception):
    """Raised by the translator when it encounters a mnemonic it has no
    handler for. Carries enough context (module, file_off, mnemonic +
    operands) for the user to either implement the translator handler or
    identify the bogus extra_entry that put unreachable data in the code
    path being analyzed."""

    def __init__(self, module: str, addr: int, asm: str, insn: dict) -> None:
        self.module = module
        self.addr = addr
        self.asm = asm
        self.insn = insn
        super().__init__(
            f"unsupported instruction at {module}+0x{addr:X}: {asm}"
        )


class CCodeRenderer:
    """Render IR instructions into rough C code for a single function."""

    def __init__(
        self,
        *,
        code_bytes: bytes | None = None,
        func_names: Dict[int, str] | None = None,
        comments: Dict[int, dict | str] | None = None,
        extern_labels: Set[int] | None = None,
        name_prefix: str = "",
        program_addrs: Set[int] | None = None,
        relocations: Iterable[dict] | None = None,
        load_segment: int = 0x1010,
        cs_base: int = 0,
        load_base: int | None = None,
    ) -> None:
        self.last_ah: int | None = None
        self.last_al: int | None = None
        self.last_dx: int | None = None
        self.last_bx: int | None = None
        self.last_dl: int | None = None
        self.last_cx: int | None = None
        self.last_cl: int | None = None
        self.last_ch: int | None = None
        self.pending_dos: List[str] = []
        self.stack_var_sizes: Dict[tuple[str | None, str], int] = {}
        self.current_func_name: str | None = None
        self.handlers: Dict[
            str, Callable[[dict, Set[int]], List[str] | None]
        ] = {
            "call": self.handle_call,
            "lcall": self.handle_lcall,
            "int": self.handle_interrupt,
            "mov": self.handle_mov,
            "add": self.handle_arithmetic,
            "sub": self.handle_arithmetic,
            "sbb": self.handle_arithmetic,
            "adc": self.handle_arithmetic,
            "and": self.handle_arithmetic,
            "xor": self.handle_arithmetic,
            "or": self.handle_arithmetic,
            "inc": self.handle_arithmetic,
            "dec": self.handle_arithmetic,
            "aaa": self.handle_aaa,
            "aas": self.handle_aas,
            "daa": self.handle_daa,
            "das": self.handle_das,
            "mul": self.handle_mul,
            "imul": self.handle_imul,
            "div": self.handle_div,
            "idiv": self.handle_idiv,
            "not": self.handle_not,
            "neg": self.handle_neg,
            "cmp": self.handle_cmp,
            "test": self.handle_test,
            "push": self.handle_push,
            "pop": self.handle_pop,
            "pushf": self.handle_pushf,
            "popf": self.handle_popf,
            "pushaw": self.handle_pushaw,
            "popaw": self.handle_popaw,
            "leave": self.handle_leave,
            "enter": self.handle_enter,
            "shl": self.handle_shift,
            "shr": self.handle_shift,
            "sar": self.handle_shift,
            "sal": self.handle_shift,
            "rol": self.handle_rotate,
            "ror": self.handle_ror,
            "rcl": self.handle_rcl,
            "xchg": self.handle_xchg,
            "xlatb": self.handle_xlatb,
            "lodsb": self.handle_lodsb,
            "lodsw": self.handle_lodsw,
            "movsb": self.handle_movsb,
            "movsw": self.handle_movsw,
            "cmpsb": self.handle_cmpsb,
            "scasb": self.handle_scasb,
            "scasw": self.handle_scasw,
            "stosb": self.handle_stosb,
            "stosw": self.handle_stosw,
            "rep": self.handle_rep,
            "repe": self.handle_repe_cmpsb,
            "repne": self.handle_repne_scasb,
            "bound": self.handle_bound,
            "loop": self.handle_loop,
            "loopne": self.handle_loop,
            "loopnz": self.handle_loop,
            "loope": self.handle_loop,
            "loopz": self.handle_loop,
            "lahf": self.handle_lahf,
            "sahf": self.handle_sahf,
            "salc": self.handle_salc,
            "aam": self.handle_aam,
            "aad": self.handle_aad,
            "cld": self.handle_cld,
            "std": self.handle_std,
            "cli": self.handle_cli,
            "sti": self.handle_sti,
            "in": self.handle_io,
            "out": self.handle_io,
            "lds": self.handle_lds,
            "les": self.handle_les,
            "jmp": self.handle_jmp,
            "ljmp": self.handle_ljmp,
            "cwde": self.handle_cwde,
            "cdq": self.handle_cdq,
            "clc": self.handle_clc,
            "cmc": self.handle_cmc,
            "stc": self.handle_stc,
            "rcr": self.handle_rcr,
            "nop": self.handle_noop,
            # `db` ("define byte") is what disassemble.py emits when capstone
            # can't decode the next byte — usually a 1-byte tail at EOF, or
            # an alignment-padding null between functions. Treat as a noop:
            # no semantic effect, no runtime work. Disassemble.py only emits
            # `db` for bytes that are NOT a valid instruction encoding, so
            # nothing of value is being skipped.
            "db": self.handle_noop,
            "ret": self.handle_ret,
            "retn": self.handle_ret,
            "retf": self.handle_ret,
            "iret": self.handle_iret,
            "lea": self.handle_lea,
        }
        self.code_bytes = code_bytes or b""
        self.func_names = func_names or {}
        self.comments = comments or {}
        self.extern_labels: Set[int] = set()
        if extern_labels:
            self.extern_labels.update(extern_labels)
        self.name_prefix = name_prefix
        self.cs_base = cs_base & 0xFFFF
        # Rendered lines for entry points that are lifted into separate
        # helper functions. Each key maps an entry address to a list of
        # pre-rendered C statements comprising the function body.
        self.extra_func_blocks: Dict[int, List[str]] = {}
        self.program_addrs: Set[int] = program_addrs or set()
        relocation_entries = relocations or []
        self._reloc_offsets: Set[int] = {
            (entry.get("segment", 0) << 4) + entry.get("offset", 0)
            for entry in relocation_entries
        }
        self._load_segment = load_segment & 0xFFFF
        # Linear base address of file offset 0 in the loaded image. The dispatch
        # switch is keyed on FILE OFFSETS (= linear - load_base), but the
        # emulated x86 stack stores true segment-relative IPs. The two only
        # coincide when a code segment's base equals load_base; a binary whose
        # entry segment is relocated above its load base (a DOS EXE with a
        # non-zero header cs, e.g. a relocated DOS EXE) runs the same `<name>_dispatch`
        # switch under several cs values where cs<<4 != load_base. For those,
        # the near-call return IP and near-ret reconstruction must convert
        # between file-offset (pc) and true IP via the LIVE cs:
        #   IP = (uint16_t)(file_off + load_base - (cs<<4))
        #   file_off = (cs<<4) + IP - load_base
        # so the round-trip is exact across the segment seam and a far return /
        # far pointer composed from cs<<4+IP lands at the right linear address.
        # For a runtime JIT chunk the dump is based at seg_base (= cs<<4 at run
        # time) and load_base is set to that base, so the conversion collapses
        # to the identity (pc == IP) -- the chunk's existing behavior unchanged.
        if load_base is None:
            load_base = self._load_segment << 4
        self._load_base = load_base & 0xFFFFF
        self.current_func_addrs: Set[int] = set()
        self.current_func_all_addrs: Set[int] = set()
        self.current_blocks: Dict[int, BasicBlock] = {}
        self.current_func_name: str = ""
        # Track loop headers to allow translating back-edges into
        # ``continue`` statements rather than emitting ``goto`` targets that
        # no longer exist after structuring.
        self.loop_headers: Set[int] = set()

    def func_name(self, addr: int) -> str:
        """Return friendly name for function or label at *addr*.

        Addresses listed in ``extern_labels`` are treated as labels rather than
        functions and receive a ``label_`` prefix unless explicitly named in
        ``func_names``.
        """
        if addr in self.func_names:
            return f"{self.name_prefix}{self.func_names[addr]}"
        if addr in self.extern_labels:
            return f"{self.name_prefix}label_{addr:04X}"
        return f"{self.name_prefix}func_{addr:04X}"

    def comment_lines(self, addr: int) -> List[str]:
        """Return comment lines for instruction at *addr* if configured."""
        comment = self.comments.get(addr)
        if comment is None:
            return []
        if isinstance(comment, str):
            lines = comment.splitlines()
            return [f"// {line}" for line in lines]
        if isinstance(comment, dict):
            text = comment.get("text", "")
            lines = text.splitlines()
            if comment.get("multiline"):
                return ["/*"] + lines + ["*/"]
            return [f"// {line}" for line in lines]
        return []

    # helper functions used by AST nodes and structuring utilities
    def parse_imm(self, value: str) -> int | None:
        return _parse_imm(value)

    def jcc_condition(
        self,
        mnemonic: str,
        prev: dict | None = None,
        address: int | None = None,
    ) -> str:
        return jcc_condition(mnemonic, prev, address)

    def negate_condition(self, cond: str) -> str:
        return _negate_condition(cond)

    def is_terminator(self, insn: dict, prev: dict | None = None) -> bool:
        if insn["mnemonic"] in {"ret", "retn", "retf", "hlt", "iret"}:
            return True
        if insn["mnemonic"] == "int" and prev is not None:
            imm = _parse_imm(insn.get("op_str", ""))
            if imm == 0x21 and prev.get("mnemonic") == "mov":
                op_str = _decode_variables(prev.get("op_str", ""))
                parts = [p.strip() for p in op_str.split(",", 1)]
                if len(parts) == 2:
                    dest, src = parts
                    dest_l = dest.lower()
                    try:
                        val = int(src.rstrip("hH"), 16)
                    except ValueError:
                        return False
                    if dest_l == "ah" and val == 0x4C:
                        return True
                    if dest_l == "ax" and ((val >> 8) & 0xFF) == 0x4C:
                        return True
        return False

    def flush_pending_dos(self, lines: List[str], indent: str = "") -> None:
        if self.pending_dos:
            lines.extend(f"{indent}{entry}" for entry in self.pending_dos)
            self.pending_dos = []
            self.last_ah = None
            self.last_al = None
            self.last_dx = None
            self.last_bx = None
            self.last_dl = None
            self.last_cx = None
            self.last_cl = None
            self.last_ch = None

    def _invalidate_register(self, dest: str) -> None:
        """Clear cached values for a register modified by an instruction."""
        reg = dest.lower()
        if reg in {"cx", "cl", "ch"}:
            self.last_cx = None
            self.last_cl = None
            self.last_ch = None
        elif reg == "dx":
            self.last_dx = None
            self.last_dl = None
        elif reg == "dh":
            self.last_dx = None
        elif reg == "dl":
            self.last_dx = None
            self.last_dl = None
        elif reg in {"bx", "bl", "bh"}:
            self.last_bx = None
        elif reg == "ax":
            self.last_ah = None
            self.last_al = None
        elif reg == "ah":
            self.last_ah = None
        elif reg == "al":
            self.last_al = None

    def _stack_var_key(self, var_name: str) -> tuple[str | None, str]:
        return (self.current_func_name, var_name.upper())

    def _fallthrough_ip(self, insn: dict) -> int | None:
        next_addr = insn.get("_next_addr")
        if next_addr is not None:
            return next_addr & 0xFFFF
        bytes_str = insn.get("bytes")
        if not bytes_str:
            return None
        size = len(bytes_str) // 2
        if size <= 0:
            return None
        addr = insn.get("address")
        if addr is None:
            return None
        return (addr + size) & 0xFFFF

    def _fallthrough_fileoff(self, insn: dict) -> int | None:
        """FULL (untruncated) file offset of the instruction after *insn*.

        The dispatch switch is keyed on file offsets, which for a binary larger
        than 64 KB exceed 16 bits, so the high bits matter when converting to a
        true segment-relative return IP. The truncation to 16 bits happens after
        the live cs base has been folded in (a real 8086 IP is 16-bit)."""
        next_addr = insn.get("_next_addr")
        if next_addr is not None:
            return next_addr
        bytes_str = insn.get("bytes")
        if not bytes_str:
            return None
        size = len(bytes_str) // 2
        if size <= 0:
            return None
        addr = insn.get("address")
        if addr is None:
            return None
        return addr + size

    def _call_return_arg(self, insn: dict) -> str:
        """The true x86 segment-relative return IP pushed on the emulated stack
        at a near call site.

        For a cs_base-anchored single-segment binary (e.g. a single-segment
        driver module) the stack holds IP values (= file_off + cs_base) and the
        case-key dispatch in handle_ret subtracts cs_base; keep that literal.

        Otherwise the pushed value is what the real 8086 pushes: the offset of
        the next instruction within the LIVE code segment,
        ``(uint16_t)(next_file_off + load_base - (cs<<4))``. The matching near
        return reconstructs the file-offset case key with the inverse, so the
        round-trip is exact even across a binary's multiple code segments. This
        also keeps the DOS ``mov reg,imm; push reg; ret`` tail-jump correct: the
        immediate is already a true IP, and for binaries where cs<<4 == load_base
        the expression collapses to that immediate."""
        if self.cs_base:
            next_ip = self._fallthrough_ip(insn)
            if next_ip is None:
                return "ip"
            return f"0x{(next_ip + self.cs_base) & 0xFFFF:04X}"
        next_off = self._fallthrough_fileoff(insn)
        if next_off is None:
            return "ip"
        return (
            f"(uint16_t)(0x{next_off:05X}U + 0x{self._load_base:05X}U "
            "- ((uint32_t)cs << 4))"
        )

    def _stack_var_width(
        self,
        var_name: str,
        *,
        size_prefix: str | None,
        insn: dict,
        operands: List[str],
        index: int,
    ) -> int:
        key = self._stack_var_key(var_name)
        if size_prefix == "byte":
            width = 8
        elif size_prefix == "word":
            width = 16
        else:
            width = self.stack_var_sizes.get(key)
            if width is None:
                width = self._infer_stack_var_width(insn, operands, index)
        self.stack_var_sizes[key] = width
        return width

    def _infer_stack_var_width(
        self, insn: dict, operands: List[str], index: int
    ) -> int:
        # Prefer widths inferred from sibling operands when present.
        for i, other in enumerate(operands):
            if i == index:
                continue
            width = _operand_width_from_str(other)
            if width is not None:
                return width
        # Default to word-sized locals when no better information is available.
        return 16

    def _rewrite_operands(self, insn: dict, operands: List[str]) -> List[str]:
        """Rewrite operands using instruction metadata and stack var tracking."""

        mem_refs = iter(insn.get("detail", {}).get("mem_refs", []))
        result: List[str] = []
        for index, original in enumerate(operands):
            size_prefix: str | None = None
            simple = re.match(r"^(byte|word) ptr (.+)$", original, re.IGNORECASE)
            op = simple.group(2) if simple else original
            if simple:
                size_prefix = simple.group(1).lower()
            op_l = op.lower()
            match = _VAR_RE.fullmatch(op_l)
            if match:
                mem_ref = next(mem_refs, None)
                if mem_ref:
                    seg = mem_ref.get("segment", "ss").lower()
                    disp = mem_ref.get("disp", 0)
                else:
                    seg = "ss"
                    disp = -int(match.group(1), 16)
                addr_expr = f"((bp + 0x{(disp & 0xFFFF):04X}) & 0xFFFF)"
                width = self._stack_var_width(
                    match.group(0),
                    size_prefix=size_prefix,
                    insn=insn,
                    operands=operands,
                    index=index,
                )
                macro = "memb" if width == 8 else "memw"
                result.append(f"{macro}({seg}, {addr_expr})")
                continue
            if _MEM_PTR_RE.match(original.lower()):
                mem_ref = next(mem_refs, None)
                seg = mem_ref.get("segment") if mem_ref else None
                result.append(_rewrite_mem_op(original, seg=seg))
                continue
            result.append(op)
        return result

    def _emit_with_comment(
        self, lines: List[str], insn: dict, code: str, indent: str = ""
    ) -> None:
        asm = f"{insn['mnemonic']} {insn.get('op_str', '')}".strip()
        lines.append(f"{indent}// ASM: {asm}")
        lines.append(f"{indent}{code}")

    def _emit_unsupported_abort(
        self, lines: List[str], insn: dict, asm: str | None = None
    ) -> None:
        """Hard-fail at IR-to-C compile time for any unsupported instruction.

        Replaces the silent ``// TODO ASM: ...`` comment that used to let
        partial code through (e.g. an unmatched ``push cs`` whose matching
        pop was the skipped TODO — caused the music driver's 0x0F20 leak). Aborts
        the translation immediately so unsupported mnemonics surface at
        build time, before any broken C is produced, before the game ever
        runs. If you see this fail and the instruction is in unreachable
        data (a bogus extra_entry), remove that entry. If it's reachable,
        implement the translator handler.

        The ``lines`` parameter is preserved for call-site compatibility
        but is not appended to — we raise instead.
        """
        if asm is None:
            asm = f"{insn['mnemonic']} {insn.get('op_str', '')}".strip()
        addr = int(insn.get("address", 0))
        module = self.name_prefix.rstrip("_") if self.name_prefix else "?"
        del lines  # explicitly: do not emit, raise instead
        raise UnsupportedInstructionError(module, addr, asm, insn)

    def _string_source_segment(self, insn: dict) -> str:
        """Resolve the source segment for string instructions."""

        detail = insn.get("detail")
        if isinstance(detail, dict):
            mem_refs = detail.get("mem_refs")
            if isinstance(mem_refs, list):
                for ref in mem_refs:
                    if not isinstance(ref, dict):
                        continue
                    access = str(ref.get("access", "")).lower()
                    if access not in {"read", "readwrite"}:
                        continue
                    seg = ref.get("segment")
                    if isinstance(seg, str) and seg:
                        return seg.lower()
        return "ds"

    def handle_dos_interrupt_prep(
        self, dest: str, src: str, lines: List[str]
    ) -> bool:
        # A REGISTER source (e.g. `mov ah, bh`) is not a constant. Crucially,
        # int("bh".rstrip("hH"), 16) == int("b", 16) == 0x0B would misparse the
        # register `bh` as the immediate 0x0B (and ah/ch/dh as 0x0A/0x0C/0x0D),
        # so the following INT 21h would dispatch to the AH=0x0B shim instead of
        # the real runtime AH (e.g. AH=0x3C create). Reject register sources so
        # AH is treated as unknown and the interrupt dispatches dynamically.
        if src.lower() in {
            "ah", "al", "ax", "bh", "bl", "bx", "ch", "cl", "cx", "dh", "dl",
            "dx", "si", "di", "bp", "sp", "cs", "ds", "es", "ss",
        }:
            self.flush_pending_dos(lines)
            return False
        try:
            val = int(src.rstrip("hH"), 16)
        except ValueError:
            self.flush_pending_dos(lines)
            return False
        dest_l = dest.lower()
        if dest_l == "ax":
            flush_prefixes: tuple[str, ...] = ("ax",)
        elif dest_l == "ah":
            flush_prefixes = ("ah", "ax")
        elif dest_l == "al":
            flush_prefixes = ("al", "ax")
        else:
            flush_prefixes = ()

        if flush_prefixes and any(
            line.lower().startswith(flush_prefixes) for line in self.pending_dos
        ):
            self.flush_pending_dos(lines)

        if dest_l == "ax":
            self.last_ah = (val >> 8) & 0xFF
            self.last_al = val & 0xFF
            self.pending_dos.append(f"{dest} = {src};")
            return True
        if dest_l == "ah":
            self.last_ah = val
            self.last_al = None
            self.pending_dos.append(f"{dest} = {src};")
            return True
        if dest_l == "al":
            self.last_al = val
            self.pending_dos = [
                entry
                for entry in self.pending_dos
                if not entry.lower().startswith("al =")
            ]
            self.pending_dos.append(f"{dest} = {src};")
            return True
        return False

    def handle_dos_interrupt_call(self) -> List[str] | None:
        if self.last_ah is None:
            return None
        ah = self.last_ah
        func = _DOS_API_FUNCS.get(ah, f"dos_api_ah_{ah:02X}")
        args: List[str] = []

        if ah == 0x48:
            if self.last_bx is not None:
                args.append(f"0x{self.last_bx:04X}")
            else:
                args.append("bx")
        elif ah == 0x49:
            args.append("(void *)seg_off(es, 0)")
        elif ah == 0x4B:
            if self.last_bx is not None:
                args.append(f"(void *)0x{self.last_bx:04X}")
            else:
                args.append("(void *)bx")
            if self.last_dx is not None:
                args.append(f"(const char *)seg_off(cs, 0x{self.last_dx:04X})")
            else:
                args.append("(const char *)seg_off(ds, dx)")
        elif ah == 0x25:
            if self.last_al is not None:
                args.append(f"0x{self.last_al:02X}")
            else:
                args.append("al")
            args.append("ds")
            if self.last_dx is not None:
                args.append(f"0x{self.last_dx:04X}")
            else:
                args.append("dx")
        elif ah == 0x42:
            if self.last_bx is not None:
                args.append(f"0x{self.last_bx:04X}")
            else:
                args.append("bx")
            if self.last_cx is not None:
                args.append(f"0x{self.last_cx:04X}")
            else:
                args.append("cx")
            if self.last_dx is not None:
                args.append(f"0x{self.last_dx:04X}")
            else:
                args.append("dx")
            if self.last_al is not None:
                args.append(f"0x{self.last_al:02X}")
            else:
                args.append("al")
        elif ah == 0x3E:
            if self.last_bx is not None:
                args.append(f"0x{self.last_bx:04X}")
            else:
                args.append("bx")
        elif ah == 0x3F:
            if self.last_bx is not None:
                args.append(f"0x{self.last_bx:04X}")
            else:
                args.append("bx")
            if self.last_dx is not None:
                args.append(f"(void *)seg_off(cs, 0x{self.last_dx:04X})")
            else:
                args.append("(void *)seg_off(ds, dx)")
            if self.last_cx is not None:
                args.append(f"0x{self.last_cx:04X}")
            else:
                args.append("cx")
        elif ah == 0x40:
            if self.last_bx is not None:
                args.append(f"0x{self.last_bx:04X}")
            else:
                args.append("bx")
            if self.last_dx is not None:
                args.append(f"(const void *)seg_off(cs, 0x{self.last_dx:04X})")
            else:
                args.append("(const void *)seg_off(ds, dx)")
            if self.last_cx is not None:
                args.append(f"0x{self.last_cx:04X}")
            else:
                args.append("cx")
        elif ah in {0x39, 0x3B, 0x41}:
            if self.last_dx is not None:
                args.append(f"(const char *)seg_off(cs, 0x{self.last_dx:04X})")
            else:
                args.append("(const char *)seg_off(ds, dx)")
        elif ah == 0x56:
            # AH=56h Rename File: DS:DX = old name, ES:DI = new name. dx/di are
            # set from registers (not folded immediates) on this path, so always
            # read them live rather than via last_dx/last_di.
            args.append("(const char *)seg_off(ds, dx)")
            args.append("(const char *)seg_off(es, di)")
        elif ah == 0x44:
            # IOCTL: AL = subfunction, BX = handle.
            if self.last_al is not None:
                args.append(f"0x{self.last_al:02X}")
            else:
                args.append("al")
            if self.last_bx is not None:
                args.append(f"0x{self.last_bx:04X}")
            else:
                args.append("bx")
        elif ah == 0x47:
            # Get current directory: DS:SI -> caller's buffer.
            args.append("(char *)seg_off(ds, si)")
        elif ah == 0x4A:
            # Resize memory block: ES = segment, BX = new size in paragraphs.
            args.append("es")
            if self.last_bx is not None:
                args.append(f"0x{self.last_bx:04X}")
            else:
                args.append("bx")
        elif ah == 0x43:
            # Get/set file attributes: DS:DX -> path; attribute word in CX.
            if self.last_dx is not None:
                args.append(f"(const char *)seg_off(cs, 0x{self.last_dx:04X})")
            else:
                args.append("(const char *)seg_off(ds, dx)")
            args.append("&cx")
        elif ah == 0x3C:
            # Create file: DS:DX -> path, CX = attributes.
            if self.last_dx is not None:
                args.append(f"(const char *)seg_off(cs, 0x{self.last_dx:04X})")
            else:
                args.append("(const char *)seg_off(ds, dx)")
            if self.last_cx is not None:
                args.append(f"0x{self.last_cx:04X}")
            else:
                args.append("cx")
        elif ah == 0x0C:
            # Flush keyboard buffer and invoke input function AL.
            if self.last_al is not None:
                args.append(f"0x{self.last_al:02X}")
            else:
                args.append("al")
        elif ah in {0x2F, 0x4F}:
            # Get DTA / Find Next: no input register arguments.
            pass
        elif ah == 0x4E:
            if self.last_dx is not None:
                args.append(f"(const char *)seg_off(cs, 0x{self.last_dx:04X})")
            else:
                args.append("(const char *)seg_off(ds, dx)")
            if self.last_cx is not None:
                args.append(f"0x{self.last_cx:04X}")
            else:
                args.append("cx")
        elif ah == 0x1A:
            if self.last_dx is not None:
                args.append(f"(void *)seg_off(cs, 0x{self.last_dx:04X})")
            else:
                args.append("(void *)seg_off(ds, dx)")
        elif ah == 0x06:
            args.append("dl")
        elif ah in {0x02, 0x0E, 0x36}:
            if self.last_dl is not None:
                args.append(f"0x{self.last_dl:02X}")
            else:
                args.append("dl")
        elif ah == 0x09:
            if self.last_dx is not None:
                args.append(f"(const char *)seg_off(cs, 0x{self.last_dx:04X})")
            else:
                args.append("(const char *)seg_off(ds, dx)")
        elif ah == 0x3D:
            if self.last_dx is not None:
                args.append(f"(const char *)seg_off(cs, 0x{self.last_dx:04X})")
            else:
                args.append("(const char *)seg_off(ds, dx)")
        elif self.last_dx is not None:
            args.append("dx")

        # Preserve all pending register assignments; DOS interrupts may not
        # consume every register, and values can remain live after the call.
        lines = self.pending_dos
        self.pending_dos = []
        self.last_ah = None
        self.last_al = None
        self.last_dx = None
        self.last_bx = None
        self.last_dl = None
        self.last_cx = None
        self.last_cl = None
        self.last_ch = None

        call = f"{func}({', '.join(args)})" if args else f"{func}()"
        if ah in _DOS_NORET:
            lines.append(f"{call};")
        elif ah == 0x06:
            lines.append(f"{call};")
        else:
            lines.append(f"CF = {call};")
        return lines

    def handle_bios_interrupt_call(self, int_num: int) -> List[str] | None:
        funcs = _BIOS_API_FUNCS.get(int_num)
        if funcs is None or self.last_ah is None:
            return None

        ah = self.last_ah
        func = funcs.get(ah)

        if int_num == 0x10 and ah == 0x0F:
            lines = self.pending_dos
            self.pending_dos = []
            regs_to_clear = {"ax", "ah", "al", "bx", "bh"}
            lines = [
                entry
                for entry in lines
                if entry.split("=", 1)[0].strip().lower() not in regs_to_clear
            ]
            self.last_ah = None
            self.last_al = None
            self.last_dx = None
            self.last_bx = None
            self.last_dl = None
            self.last_cx = None
            self.last_cl = None
            self.last_ch = None
            lines.append("CF = 0;")
            lines.append("al = bios_current_video_mode();")
            lines.append("ah = bios_current_video_columns();")
            lines.append("bh = bios_current_active_page();")
            return lines

        if int_num == 0x10 and ah == 0x1A:
            lines = self.pending_dos
            self.pending_dos = []
            regs_to_clear = {"ax", "ah", "al", "bx", "bh"}
            lines = [
                entry
                for entry in lines
                if entry.split("=", 1)[0].strip().lower() not in regs_to_clear
            ]
            self.last_ah = None
            self.last_al = None
            self.last_dx = None
            self.last_bx = None
            self.last_dl = None
            self.last_cx = None
            self.last_cl = None
            self.last_ch = None
            lines.append("CF = 0;")
            lines.append("al = 0x1A;")
            lines.append("bl = bios_display_combination_code();")
            lines.append("bh = bios_display_combination_alt_code();")
            return lines

        if int_num == 0x10 and ah == 0x03:
            # Get cursor position (DX) + shape (CX) for page BH.
            lines = self.pending_dos
            self.pending_dos = []
            lines = [
                entry for entry in lines
                if entry.split("=", 1)[0].strip().lower() not in {"dx", "cx"}
            ]
            page = (f"0x{(self.last_bx >> 8) & 0xFF:02X}"
                    if self.last_bx is not None else "bh")
            for attr in ("last_ah", "last_al", "last_dx", "last_bx",
                         "last_dl", "last_cx", "last_cl", "last_ch"):
                setattr(self, attr, None)
            lines.append(f"bios_get_cursor({page});")
            return lines

        if int_num == 0x10 and ah == 0x08:
            # Read character + attribute at the cursor: AL=char, AH=attribute.
            lines = self.pending_dos
            self.pending_dos = []
            drop = {"ax", "ah", "al"}
            lines = [
                e for e in lines
                if e.split("=", 1)[0].strip().lower() not in drop
            ]
            self.last_ah = None
            self.last_al = None
            self.last_dx = None
            self.last_bx = None
            self.last_dl = None
            self.last_cx = None
            self.last_cl = None
            self.last_ch = None
            lines.append("ax = bios_read_char_attr();")
            lines.append("CF = 0;")
            return lines

        if int_num == 0x10 and ah == 0x30:
            lines = self.pending_dos
            self.pending_dos = []
            regs_to_clear = {"cx", "dx"}
            lines = [
                entry
                for entry in lines
                if entry.split("=", 1)[0].strip().lower() not in regs_to_clear
            ]
            self.last_ah = None
            self.last_al = None
            self.last_dx = None
            self.last_bx = None
            self.last_dl = None
            self.last_cx = None
            self.last_cl = None
            self.last_ch = None
            lines.append("{")
            lines.append("    uint16_t seg;")
            lines.append("    uint16_t off;")
            lines.append("    bios_get_video_parameter_block(al, &seg, &off);")
            lines.append("    cx = seg;")
            lines.append("    dx = off;")
            lines.append("}")
            lines.append("CF = 0;")
            return lines

        if int_num == 0x10 and ah in (0x06, 0x07):
            # Scroll window up (06) / down (07). No register return values; this
            # mirrors int10h_impl's runtime handler so a constant-AH static call
            # to scroll lifts instead of hitting _emit_unsupported_abort (the two
            # INT 10h dispatch paths must cover the same set).
            lines = self.pending_dos
            self.pending_dos = []
            self.last_ah = None
            self.last_al = None
            self.last_dx = None
            self.last_bx = None
            self.last_dl = None
            self.last_cx = None
            self.last_cl = None
            self.last_ch = None
            _dir = 0 if ah == 0x06 else 1
            lines.append(
                f"bios_scroll_window(al, bh, ch, cl, dh, dl, {_dir});")
            lines.append("CF = 0;")
            return lines

        if func is None:
            self.pending_dos = []
            self.last_ah = None
            self.last_al = None
            self.last_dx = None
            self.last_bx = None
            self.last_dl = None
            self.last_cx = None
            self.last_cl = None
            self.last_ch = None
            _abort_lines: List[str] = []
            self._emit_unsupported_abort(
                _abort_lines, {"address": 0, "mnemonic": "int", "op_str": ""},
                f"int 0x{int_num:02X} AH=0x{ah:02X} (unsupported BIOS)"
            )
            return _abort_lines
        lines = self.pending_dos
        self.pending_dos = []
        self.last_ah = None

        call_args: List[str] = []
        regs_to_clear: Set[str] = set()

        if int_num == 0x10:
            if ah == 0x00:
                mode = (
                    f"0x{self.last_al:02X}" if self.last_al is not None else "al"
                )
                regs = {"ax", "ah", "al"}
                lines = [
                    entry
                    for entry in lines
                    if entry.split("=", 1)[0].strip().lower() not in regs
                ]
                self.last_dx = None
                self.last_bx = None
                self.last_dl = None
                self.last_cx = None
                self.last_cl = None
                self.last_ch = None
                self.last_al = None
                lines.append(f"{func}({mode});")
                return lines
            if ah == 0x02:
                if self.last_bx is not None:
                    page = (self.last_bx >> 8) & 0xFF
                    call_args.append(f"0x{page:02X}")
                else:
                    call_args.append("bh")
                if self.last_dx is not None:
                    row = (self.last_dx >> 8) & 0xFF
                    col = self.last_dx & 0xFF
                    call_args.append(f"0x{row:02X}")
                    call_args.append(f"0x{col:02X}")
                else:
                    call_args.append("dh")
                    call_args.append("dl")
                regs_to_clear = {"ax", "ah", "al", "bx", "dx"}
            elif ah == 0x0B:
                if self.last_bx is not None:
                    bh_val = (self.last_bx >> 8) & 0xFF
                    bl_val = self.last_bx & 0xFF
                    call_args.append(f"0x{bh_val:02X}")
                    call_args.append(f"0x{bl_val:02X}")
                else:
                    call_args.append("bh")
                    call_args.append("bl")
                regs_to_clear = {"ax", "ah", "al", "bx"}
            elif ah == 0x0E:
                if self.last_al is not None:
                    call_args.append(f"0x{self.last_al:02X}")
                else:
                    call_args.append("al")
                if self.last_bx is not None:
                    page = (self.last_bx >> 8) & 0xFF
                    attr = self.last_bx & 0xFF
                    call_args.append(f"0x{page:02X}")
                    call_args.append(f"0x{attr:02X}")
                else:
                    call_args.append("bh")
                    call_args.append("bl")
                regs_to_clear = {"ax", "ah", "al", "bx"}
            elif ah == 0x09:
                # write char+attr CX times: char=AL page=BH attr=BL count=CX
                al_a = (f"0x{self.last_al:02X}"
                        if self.last_al is not None else "al")
                cx_a = (f"0x{self.last_cx:04X}"
                        if self.last_cx is not None else "cx")
                call_args.append(al_a)
                if self.last_bx is not None:
                    call_args.append(f"0x{(self.last_bx >> 8) & 0xFF:02X}")
                    call_args.append(f"0x{self.last_bx & 0xFF:02X}")
                else:
                    call_args.append("bh")
                    call_args.append("bl")
                call_args.append(cx_a)
                regs_to_clear = {"ax", "ah", "al"}
            elif ah == 0x0A:
                # write char CX times keeping attr: char=AL page=BH count=CX
                al_a = (f"0x{self.last_al:02X}"
                        if self.last_al is not None else "al")
                bh_a = (f"0x{(self.last_bx >> 8) & 0xFF:02X}"
                        if self.last_bx is not None else "bh")
                cx_a = (f"0x{self.last_cx:04X}"
                        if self.last_cx is not None else "cx")
                call_args.append(al_a)
                call_args.append(bh_a)
                call_args.append(cx_a)
                regs_to_clear = {"ax", "ah", "al"}

        self.last_dx = None
        self.last_bx = None
        self.last_dl = None
        self.last_cx = None
        self.last_cl = None
        self.last_ch = None
        self.last_al = None
        if regs_to_clear:
            lines = [
                entry
                for entry in lines
                if entry.split("=", 1)[0].strip().lower() not in regs_to_clear
            ]
        call = f"{func}({', '.join(call_args)})" if call_args else f"{func}()"
        lines.append(f"{call};")
        return lines

    # -- Instruction handlers -------------------------------------------------

    def handle_call(self, insn: dict, known_funcs: Set[int]) -> List[str]:
        lines: List[str] = []
        self.flush_pending_dos(lines)
        target = insn.get("target")
        if target is not None:
            bytes_str = insn.get("bytes", "")
            if len(bytes_str) == 6 and target & 0xFFFF0000 == 0xFFFF0000:
                target &= 0xFFFF
        ret_arg = self._call_return_arg(insn)
        if target is None:
            target = _parse_imm(insn["op_str"])

        if target is not None:
            # Translate-to-the-letter: any direct call with a known target
            # gets the literal asm semantics — push retIP on the simulated
            # stack, set pc, continue the dispatch. The dispatch switch
            # routes to the matching case if it exists, otherwise default:
            # falls through to near_ret_tail which resolves cross-binary
            # via file_mappings or aborts on a genuinely bogus target.
            #
            # Exception: extern_labels targets live in another translation
            # unit, so we don't have a case label for them in our switch.
            # Those go through the per-binary C wrapper — the wrapper's
            # expected_retip-based unwind provides per-call drift isolation
            # that the in-loop pattern can't (MIGRATION.md "future work").
            if target in self.extern_labels:
                name = self.func_name(target)
                self._emit_with_comment(lines, insn, "{")
                lines.append("    sp = (sp - 2) & 0xFFFF;")
                lines.append(f"    memw_write(ss, sp, {ret_arg});")
                lines.append(f"    {name}({ret_arg});")
                lines.append("}")
            elif self.name_prefix:
                self._emit_with_comment(lines, insn, "{")
                lines.append("    sp = (sp - 2) & 0xFFFF;")
                lines.append(f"    memw_write(ss, sp, {ret_arg});")
                lines.append(f"    pc = 0x{target:04X};")
                lines.append("    continue;")
                lines.append("}")
            else:
                asm = f"{insn['mnemonic']} {insn.get('op_str', '')}".strip()
                self._emit_unsupported_abort(lines, insn, asm)
            return lines
        op_str = _decode_variables(insn.get("op_str", ""))
        op = op_str.lower().strip()
        ret_arg = self._call_return_arg(insn)
        # Register operand: treat as an indirect call within the current code
        # segment.  The register holds the 16-bit offset from ``cs``.
        if op in {"ax", "bx", "cx", "dx", "si", "di", "bp", "sp"}:
            expr = (
                f"call_table({ret_arg}, (((uint32_t)cs << 4) + (uint16_t)({op})) & 0xFFFFF);"
            )
            self._emit_with_comment(lines, insn, expr)
            return lines

        match = _MEM_PTR_RE.match(op)
        if match:
            mem_refs = insn.get("detail", {}).get("mem_refs", [])
            seg = (
                mem_refs[0].get("segment", "ds").lower() if mem_refs else "ds"
            )
            mem_expr = _rewrite_mem_op(op, seg=seg)
            # A ``call`` mnemonic is ALWAYS a near indirect call (opcode FF /2,
            # ``call word ptr [mem]``): it reads a 16-bit offset, pushes only the
            # near return IP, and leaves cs unchanged. Capstone emits ``lcall``
            # (-> handle_lcall) for genuine far indirect calls (FF /3,
            # ``call dword ptr [mem]``). A preceding ``push cs`` is the game's
            # own ``push cs; call near; retf`` idiom -- it explicitly stacks the
            # return cs so the callee can ``retf``; promoting the near call to a
            # far lcall_table would push a SECOND cs and drift the stack by 2.
            expr = (
                f"call_table({ret_arg}, (((uint32_t)cs << 4) + (uint16_t)({mem_expr}))"
                " & 0xFFFFF);"
            )
            self._emit_with_comment(lines, insn, expr)
            return lines

        # Unknown target: emit as a comment.
        asm = f"{insn['mnemonic']} {insn.get('op_str', '')}".strip()
        self._emit_unsupported_abort(lines, insn, asm)
        return lines

    def handle_lcall(self, insn: dict, known_funcs: Set[int]) -> List[str]:
        lines: List[str] = []
        self.flush_pending_dos(lines)
        op_str = _decode_variables(insn.get("op_str", ""))
        op = op_str.lower().strip()
        ret_arg = self._call_return_arg(insn)
        if ":" in op:
            op = op.split(":", 1)[1].strip()
        if op.startswith("[") and op.endswith("]"):
            inner = op[1:-1].strip()
            mem_refs = insn.get("detail", {}).get("mem_refs", [])
            seg_reg = (
                mem_refs[0].get("segment", "ds").lower() if mem_refs else "ds"
            )
            try:
                base = 16 if inner.lower().startswith(("0x", "-0x")) else 10
                off = int(inner, base) & 0xFFFF
            except ValueError:
                m = re.fullmatch(
                    r"(bx|bp|si|di)(?:\s*([+-])\s*(0x[0-9a-f]+|\d+))?",
                    inner,
                    re.IGNORECASE,
                )
                if m:
                    reg = m.group(1).lower()
                    sign = m.group(2)
                    off_str = m.group(3)
                    if off_str:
                        base = 16 if off_str.lower().startswith("0x") else 10
                        imm = int(off_str, base)
                        if sign == "-":
                            imm = (-imm) & 0xFFFF
                        off = imm & 0xFFFF
                    else:
                        off = 0
                    reg_expr = reg if off == 0 else f"{reg} + 0x{off:04X}"
                    off2 = (off + 2) & 0xFFFF
                    seg_expr = reg if off2 == 0 else f"{reg} + 0x{off2:04X}"
                    expr = (
                        f"lcall_table({ret_arg}, "
                        "memw({seg_reg}, {seg_expr}), "
                        "memw({seg_reg}, {reg_expr}));"
                    ).format(
                        seg_reg=seg_reg,
                        seg_expr=seg_expr,
                        reg_expr=reg_expr,
                    )
                    self._emit_with_comment(lines, insn, expr)
                    return lines
            else:
                off2 = (off + 2) & 0xFFFF
                expr = (
                    f"lcall_table({ret_arg}, "
                    "memw({seg_reg}, 0x{off2:04x}), "
                    "memw({seg_reg}, 0x{off:04x}));"
                ).format(seg_reg=seg_reg, off2=off2, off=off)
                self._emit_with_comment(lines, insn, expr)
                return lines

        imm_match = re.fullmatch(
            r"(0x[0-9a-f]+|\d+)\s*,\s*(0x[0-9a-f]+|\d+)", op, re.IGNORECASE
        )
        if imm_match:
            seg_val = int(
                imm_match.group(1),
                16 if imm_match.group(1).lower().startswith("0x") else 10,
            ) & 0xFFFF
            off_val = int(
                imm_match.group(2),
                16 if imm_match.group(2).lower().startswith("0x") else 10,
            ) & 0xFFFF
            # Apply the load-time relocation to the segment immediate. If the
            # EXE's relocation table fixes up this far call's segment word, the
            # DOS loader adds the load segment to it. Without this a `lcall 0:0`
            # whose segment is relocated calls 0000:0000 (null) -- e.g. a relocated
            # DOS EXE's C startup. The segment is the instruction's last word.
            size_bytes = len(insn.get("bytes", "")) // 2
            if (insn["address"] + size_bytes - 2) in self._reloc_offsets:
                seg_val = (seg_val + self._load_segment) & 0xFFFF
            expr = f"lcall_table({ret_arg}, 0x{seg_val:04X}, 0x{off_val:04X});"
            self._emit_with_comment(lines, insn, expr)
            return lines

        var_match = _VAR_RE.fullmatch(op)
        if var_match:
            mem_refs = insn.get("detail", {}).get("mem_refs", [])
            mem_ref = mem_refs[0] if mem_refs else None
            if mem_ref:
                seg_reg = mem_ref.get("segment", "ss").lower()
                disp = mem_ref.get("disp", 0)
            else:
                seg_reg = "ss"
                disp = -int(var_match.group(1), 16)
            off_expr = f"((bp + 0x{(disp & 0xFFFF):04X}) & 0xFFFF)"
            seg_expr = f"((bp + 0x{((disp + 2) & 0xFFFF):04X}) & 0xFFFF)"
            expr = (
                f"lcall_table({ret_arg}, "
                f"memw({seg_reg}, {seg_expr}), "
                f"memw({seg_reg}, {off_expr}));"
            )
            self._emit_with_comment(lines, insn, expr)
            return lines

        asm = f"{insn['mnemonic']} {insn.get('op_str', '')}".strip()
        self._emit_unsupported_abort(lines, insn, asm)
        return lines

    def handle_ret(self, insn: dict, known_funcs: Set[int]) -> List[str]:
        lines: List[str] = []
        self.flush_pending_dos(lines)
        mnem = insn["mnemonic"]
        imm = _parse_imm(insn.get("op_str", ""))
        if mnem == "retf":
            # Faithful far return: pop ip then cs (then imm16 caller-cleanup
            # bytes), set cpu.r_cs:cpu.r_ip, and exit the chunk. The top-level
            # loop re-resolves the owning chunk for the popped cs:ip. One rule
            # for every retf -- genuine far-call return, `push cs; call near;
            # retf` idiom, and `push far ptr; retf` far jump all just pop two
            # words and continue at the popped cs:ip.
            if imm is not None:
                lines.append(f"retf_pop({imm});")
            else:
                lines.append("retf();")
            lines.append("return;")
            return lines
        # Faithful near return: pop the IP off the emulated stack (+imm16 for
        # `ret N` caller-cleanup) and resume there. The pushed value is the true
        # x86 segment-relative IP. The dispatch switch is keyed on FILE OFFSET
        # (= linear - load_base), so the case key is reconstructed from the LIVE
        # cs: file_off = (cs<<4) + popped_ip - load_base. A binary that spans
        # more than one code segment (file > 64 KB) reaches different segments
        # under different cs values; using the live cs keeps the near call/ret
        # round-trip faithful instead of truncating to 16 bits (which collides a
        # high-segment return with a different low-segment case -- the
        # relocated-DOS-EXE constructor-walker `push cs; call near; retf` bug). If the
        # reconstructed key isn't a case here, the `default:` arm inverts the
        # reconstruction back to a segment-relative IP and routes the cross-
        # binary return through near_ret_tail. No expected_retip early-exit --
        # the emulated stack is the only return-address store.
        lines.append("{")
        lines.append("    uint16_t popped_ip = memw(ss, sp);")
        lines.append("    sp = (sp + 2) & 0xFFFF;")
        if imm is not None:
            lines.append(f"    sp = (sp + {imm}) & 0xFFFF;")
        if self.name_prefix:
            if self.cs_base:
                # cs_base-anchored single-segment binary: cs is constant, so the
                # static offset indexes the switch directly. Keep the faithful
                # constant subtraction.
                lines.append(
                    f"    pc = (int)((popped_ip - 0x{self.cs_base:04X}) & 0xFFFF);"
                )
            else:
                # Reconstruct the file-offset case key from the LIVE cs.
                lines.append(
                    "    pc = (int)(((uint32_t)cs << 4) + popped_ip - "
                    f"0x{self._load_base:05X}U);"
                )
            lines.append("    continue;")
        else:
            lines.append("    near_ret_tail(popped_ip, expected_retip);")
            lines.append("    return;")
        lines.append("}")
        return lines

    def handle_iret(self, insn: dict, known_funcs: Set[int]) -> List[str]:
        lines: List[str] = []
        self.flush_pending_dos(lines)
        # Faithful iret: pop ip, cs, flags; restore flags; set cpu.r_cs:cpu.r_ip;
        # exit the chunk so the top-level loop resumes at the interrupted point.
        lines.append("iret();")
        lines.append("return;")
        return lines

    def _indirect_jump_target(self, insn: dict) -> Tuple[str, str]:
        """Return the type and expression for an indirect near jump target."""
        reg = insn.get("reg")
        if reg:
            return "uint16_t", reg

        seg = insn.get("seg", "ds")
        expr = insn.get("ea", "0")
        width = insn.get("width", 16)
        size_tok = "byte" if width == 8 else "word"
        mem_expr = _rewrite_mem_op(f"{size_tok} ptr [{expr}]", seg=seg)
        rcb = _match_rcb_access(mem_expr)
        c_type = "uint8_t" if width == 8 else "uint16_t"
        if rcb:
            size, field = rcb
            func = "rcb_read16" if size == "w" else "rcb_read8"
            expr = f"{func}({field})"
        else:
            expr = mem_expr
        return c_type, expr

    def handle_jmp(self, insn: dict, known_funcs: Set[int]) -> List[str]:
        lines: List[str] = []
        self.flush_pending_dos(lines)
        op_str = _decode_variables(insn.get("op_str", ""))
        op = op_str.lower().strip()
        if insn.get("op") == "INDIRECT_NEAR_JMP":
            _, expr = self._indirect_jump_target(insn)
            self._emit_with_comment(
                lines,
                insn,
                (
                    f"jump_table((((uint32_t)cs << 4) + (uint16_t)({expr}))"
                    " & 0xFFFFF, expected_retip);"
                ),
            )
            lines.append("return;")
            return lines
        if op in _JMP_REGS:
            self._emit_with_comment(
                lines,
                insn,
                (
                    f"jump_table((((uint32_t)cs << 4) + (uint16_t)({op}))"
                    " & 0xFFFFF, expected_retip);",
                ),
            )
            lines.append("return;")
            return lines

        match = _MEM_PTR_RE.match(op)
        if match:
            mem_refs = insn.get("detail", {}).get("mem_refs", [])
            seg = (
                mem_refs[0].get("segment", "ds").lower() if mem_refs else "ds"
            )
            mem_expr = _rewrite_mem_op(op, seg=seg)
            rcb = _match_rcb_access(mem_expr)
            if rcb:
                size, field = rcb
                func = "rcb_read16" if size == "w" else "rcb_read8"
                expr = f"{func}({field})"
            else:
                expr = mem_expr
            self._emit_with_comment(
                lines,
                insn,
                (
                    f"jump_table((((uint32_t)cs << 4) + (uint16_t)({expr}))"
                    " & 0xFFFFF, expected_retip);"
                ),
            )
            lines.append("return;")
            return lines

        try:
            target = int(op_str, 16)
        except ValueError:
            asm = f"jmp {op_str}".strip()
            self._emit_unsupported_abort(lines, insn, asm)
            return lines

        if target in self.loop_headers and target in self.current_blocks:
            lines.append("continue;")
            return lines
        block = self.current_blocks.get(target)
        if target in known_funcs:
            # Faithful near jmp to a known function start: set pc and continue.
            # If `target` is a case in this chunk the switch routes to it;
            # otherwise the `default:` arm exits to the top-level loop (a
            # same-segment cross-chunk jmp). No C tail-call recursion.
            self._emit_with_comment(lines, insn, f"pc = 0x{target:04X};")
            lines.append("continue;")
            return lines
        if (
            block
            and len(block.instructions) == 1
            and block.instructions[0]["mnemonic"] in {"ret", "retn", "retf"}
            and target not in self.func_names
        ):
            lines.append("return;")
            return lines
        if (
            block
            and block.instructions
            and target in self.current_func_addrs
            and target in self.program_addrs
        ):
            self._emit_with_comment(
                lines,
                insn,
                f"pc = 0x{target:04X};",
            )
            lines.append("continue;")
            return lines

        if insn.get("force_pc"):
            self._emit_with_comment(
                lines,
                insn,
                f"pc = 0x{target:04X};",
            )
            lines.append("continue;")
            return lines
        # Faithful near jmp: set pc and continue. Whether or not a block exists
        # in this chunk, the switch routes to the case if present, else the
        # `default:` arm exits to the top-level loop.
        self._emit_with_comment(
            lines,
            insn,
            f"pc = 0x{target:04X};",
        )
        lines.append("continue;")
        return lines

    def handle_ljmp(self, insn: dict, known_funcs: Set[int]) -> List[str]:
        lines: List[str] = []
        self.flush_pending_dos(lines)
        op_str = _decode_variables(insn.get("op_str", ""))
        parts = [p.strip() for p in op_str.split(":", 1)]
        if len(parts) == 2:
            try:
                seg = int(parts[0], 16)
                off = int(parts[1], 16)
            except ValueError:
                seg_reg = parts[0].lower()
                mem = parts[1].strip()
                if (
                    mem.startswith("[")
                    and mem.endswith("]")
                    and seg_reg in {"cs", "ds", "es", "ss"}
                ):
                    inner = mem[1:-1]
                    try:
                        base = (
                            16
                            if inner.lower().startswith(("0x", "-0x"))
                            else 10
                        )
                        off = int(inner, base) & 0xFFFF
                    except ValueError:
                        # Register/expression operand with a segment override,
                        # e.g. `ljmp cs:[bx]`: read the far pointer (offset at
                        # [EA], segment at [EA+2]) via the standard memory-
                        # operand rewriter using the explicit segment.
                        off_mem = _rewrite_mem_op(
                            f"word ptr [{inner}]", seg=seg_reg)
                        seg_mem = _rewrite_mem_op(
                            f"word ptr [{inner}+2]", seg=seg_reg)
                        expr = f"long_jump({seg_mem}, {off_mem});"
                        self._emit_with_comment(lines, insn, expr)
                        lines.append("return;")
                        return lines
                    else:
                        off2 = (off + 2) & 0xFFFF
                        seg_mem = f"memw({seg_reg}, 0x{off2:04X})"
                        off_mem = f"memw({seg_reg}, 0x{off:04X})"
                        seg_rcb = _match_rcb_access(seg_mem)
                        off_rcb = _match_rcb_access(off_mem)
                        if seg_rcb:
                            s_size, s_field = seg_rcb
                            s_func = (
                                "rcb_read16" if s_size == "w" else "rcb_read8"
                            )
                            seg_mem = f"{s_func}({s_field})"
                        if off_rcb:
                            o_size, o_field = off_rcb
                            o_func = (
                                "rcb_read16" if o_size == "w" else "rcb_read8"
                            )
                            off_mem = f"{o_func}({o_field})"
                        expr = f"long_jump({seg_mem}, {off_mem});"
                        self._emit_with_comment(lines, insn, expr)
                        lines.append("return;")
                        return lines
            else:
                # Apply the load-time relocation to the segment immediate, same
                # as the far-call path: a relocated `ljmp seg:off` segment word
                # gets the load segment added by the DOS loader.
                size_bytes = len(insn.get("bytes", "")) // 2
                if (insn["address"] + size_bytes - 2) in self._reloc_offsets:
                    seg = (seg + self._load_segment) & 0xFFFF
                expr = f"long_jump(0x{seg:04X}, 0x{off:04X});"
                self._emit_with_comment(lines, insn, expr)
                lines.append("return;")
                return lines

        # Memory-indirect far jump with no segment prefix: `ljmp [mem]`
        # (FF /5). The 4-byte far pointer at the effective address holds the
        # offset (low word) then the segment (high word); read both and jump.
        # The default segment comes from the operand's mem_ref (DS unless a
        # bp/sp base makes it SS).
        mem = op_str.strip()
        if mem.startswith("[") and mem.endswith("]"):
            inner = mem[1:-1].strip()
            mem_refs = insn.get("detail", {}).get("mem_refs", [])
            seg_reg = (
                mem_refs[0].get("segment", "ds").lower() if mem_refs else "ds"
            )

            def _rcb_read(expr: str) -> str:
                rcb = _match_rcb_access(expr)
                if not rcb:
                    return expr
                size, field = rcb
                return f"{'rcb_read16' if size == 'w' else 'rcb_read8'}({field})"

            try:
                base = 16 if inner.lower().startswith(("0x", "-0x")) else 10
                off = int(inner, base) & 0xFFFF
            except ValueError:
                # Register/expression-based address: read offset at the EA and
                # segment at EA+2 via the standard memory-operand rewriter.
                off_mem = _rewrite_mem_op(f"word ptr [{inner}]", seg=seg_reg)
                seg_mem = _rewrite_mem_op(f"word ptr [{inner}+2]", seg=seg_reg)
            else:
                off2 = (off + 2) & 0xFFFF
                off_mem = f"memw({seg_reg}, 0x{off:04X})"
                seg_mem = f"memw({seg_reg}, 0x{off2:04X})"
            expr = f"long_jump({_rcb_read(seg_mem)}, {_rcb_read(off_mem)});"
            self._emit_with_comment(lines, insn, expr)
            lines.append("return;")
            return lines

        asm = f"ljmp {op_str}".strip()
        self._emit_unsupported_abort(lines, insn, asm)
        return lines

    def handle_interrupt(self, insn: dict, known_funcs: Set[int]) -> List[str]:
        lines: List[str] = []
        op = insn["op_str"].lower().strip().rstrip("h")
        try:
            int_num = int(op, 16)
        except ValueError:
            int_num = None
        if int_num == 0x21:
            result = self.handle_dos_interrupt_call()
            if result is not None:
                return result
        result = self.handle_bios_interrupt_call(int_num)
        if result is not None:
            return result
        self.flush_pending_dos(lines)
        if int_num == 0x21:
            lines.append("dos_api();")
            return lines
        if int_num in _DOS_INT_CALLS:
            lines.append(f"{_DOS_INT_CALLS[int_num]}();")
            return lines
        if int_num in _BIOS_INT_CALLS:
            lines.append(f"{_BIOS_INT_CALLS[int_num]}();")
            return lines
        if int_num is not None:
            # Dispatchers set ``ip`` to the current opcode address before
            # invoking instruction handlers. ``run_interrupt`` is responsible
            # for pushing the correct post-INT return address, so pre-advancing
            # ``ip`` here would double-advance and corrupt the software
            # interrupt return frame.
            lines.append(f"run_interrupt(0x{int_num:02X});")
            return lines
        asm = f"int {insn['op_str']}"
        self._emit_unsupported_abort(lines, insn, asm)
        return lines

    def handle_io(self, insn: dict, known_funcs: Set[int]) -> List[str]:
        lines: List[str] = []
        self.flush_pending_dos(lines)
        op_str = _decode_variables(insn.get("op_str", ""))
        parts = [p.strip() for p in op_str.split(",", 1)]
        mnem = insn["mnemonic"]
        if mnem == "in" and len(parts) == 2:
            dest, port = parts
            dest_l = dest.lower()
            if dest_l in {"al", "ax"}:
                func = "inb" if dest_l == "al" else "inw"
                lines.append(f"{dest} = {func}({port});")
                return lines
        if mnem == "out" and len(parts) == 2:
            port, src = parts
            src_l = src.lower()
            if src_l in {"al", "ax"}:
                func = "outb" if src_l == "al" else "outw"
                lines.append(f"{func}({port}, {src});")
                return lines
        asm = f"{mnem} {op_str}".strip()
        self._emit_unsupported_abort(lines, insn, asm)
        return lines

    def handle_push(self, insn: dict, known_funcs: Set[int]) -> List[str]:
        lines: List[str] = []
        self.flush_pending_dos(lines)
        op_str = _decode_variables(insn.get("op_str", ""))
        op = self._rewrite_operands(insn, [op_str])[0]
        # x86 PUSH SP stores the pre-decrement SP value.
        if op.lower() == "sp":
            lines.append("uint16_t push_value = sp;")
            lines.append("sp = (sp - 2) & 0xFFFF;")
            lines.append("memw_write(ss, sp, push_value);")
            return lines
        lines.append("sp = (sp - 2) & 0xFFFF;")
        lines.append(f"memw_write(ss, sp, {op});")
        return lines

    def handle_pop(self, insn: dict, known_funcs: Set[int]) -> List[str]:
        lines: List[str] = []
        self.flush_pending_dos(lines)
        op_str = _decode_variables(insn.get("op_str", ""))
        dest = self._rewrite_operands(insn, [op_str])[0]
        dest_l = dest.lower()
        if dest_l == "dx":
            self.last_dx = None
            self.last_dl = None
        elif dest_l == "dl":
            self.last_dl = None
        elif dest_l == "bx":
            self.last_bx = None
        elif dest_l in {"ax", "ah", "al"}:
            self.last_ah = None
            self.last_al = None
        if dest_l == "sp":
            lines.append("{")
            lines.append("    uint16_t tmp = memw(ss, sp);")
            lines.append("    sp = (sp + 2) & 0xFFFF;")
            lines.append("    sp = tmp;")
            lines.append("}")
            return lines
        if dest.startswith("memw(") or dest.startswith("memb("):
            inner = dest[5:-1]
            writer = "memw_write" if dest.startswith("memw(") else "memb_write"
            lines.append(f"{writer}({inner}, memw(ss, sp));")
        else:
            lines.append(f"{dest} = memw(ss, sp);")
        if dest_l == "ss":
            lines.append("interrupt_shadow = 1;")
        lines.append("sp = (sp + 2) & 0xFFFF;")
        return lines

    def handle_pushf(self, insn: dict, known_funcs: Set[int]) -> List[str]:
        lines: List[str] = []
        self.flush_pending_dos(lines)
        lines.append("sp = (sp - 2) & 0xFFFF;")
        lines.append(
            "memw_write(ss, sp, (uint16_t)(0x0002u | CF | (PF << 2) | (ZF << 6) | "
            "(SF << 7) | (IF << 9) | (DF << 10) | (OF << 11)));"
        )
        return lines

    def handle_popf(self, insn: dict, known_funcs: Set[int]) -> List[str]:
        lines: List[str] = []
        self.flush_pending_dos(lines)
        lines.append("uint16_t oldIF = IF;")
        lines.append("uint16_t flags = memw(ss, sp);")
        lines.append("CF = flags & 0x0001;")
        lines.append("PF = (flags >> 2) & 1;")
        lines.append("ZF = (flags >> 6) & 1;")
        lines.append("SF = (flags >> 7) & 1;")
        lines.append("IF = (flags >> 9) & 1;")
        lines.append("DF = (flags >> 10) & 1;")
        lines.append("OF = (flags >> 11) & 1;")
        lines.append("sp = (sp + 2) & 0xFFFF;")
        lines.append("if (!oldIF && IF) interrupt_shadow = 1;")
        return lines

    def handle_pushaw(self, insn: dict, known_funcs: Set[int]) -> List[str]:
        lines: List[str] = []
        self.flush_pending_dos(lines)
        lines.append("{")
        lines.append("    uint16_t orig_sp = sp;")
        for reg in ("ax", "cx", "dx", "bx"):
            lines.append("    sp = (sp - 2) & 0xFFFF;")
            lines.append(f"    memw_write(ss, sp, {reg});")
        lines.append("    sp = (sp - 2) & 0xFFFF;")
        lines.append("    memw_write(ss, sp, orig_sp);")
        for reg in ("bp", "si", "di"):
            lines.append("    sp = (sp - 2) & 0xFFFF;")
            lines.append(f"    memw_write(ss, sp, {reg});")
        lines.append("}")
        return lines

    def handle_popaw(self, insn: dict, known_funcs: Set[int]) -> List[str]:
        # POPA (80186) -- exact inverse of PUSHA (handle_pushaw). Pop DI, SI, BP,
        # then DISCARD the saved-SP word (POPA does NOT restore SP from the
        # stack; SP is advanced by the pops themselves), then BX, DX, CX, AX.
        lines: List[str] = []
        self.flush_pending_dos(lines)
        lines.append("{")
        for reg in ("di", "si", "bp"):
            lines.append(f"    {reg} = memw(ss, sp);")
            lines.append("    sp = (sp + 2) & 0xFFFF;")
        lines.append("    sp = (sp + 2) & 0xFFFF;  /* saved SP slot: popped, discarded */")
        for reg in ("bx", "dx", "cx", "ax"):
            lines.append(f"    {reg} = memw(ss, sp);")
            lines.append("    sp = (sp + 2) & 0xFFFF;")
        lines.append("}")
        # The pops overwrite registers whose DOS-call immediate caches are now
        # stale; drop them so later interrupt lifting doesn't reuse old values.
        for reg in ("ax", "bx", "cx", "dx"):
            self._invalidate_register(reg)
        return lines

    def handle_leave(self, insn: dict, known_funcs: Set[int]) -> List[str]:
        # LEAVE (0xC9, 80186) -- tear down the current stack frame. It is
        # exactly ``mov sp, bp`` followed by ``pop bp`` at 16-bit operand size:
        #   SP <- BP ; BP <- [SS:SP] ; SP <- SP + 2
        # Compilers emit it as the epilogue counterpart to ``push bp;
        # mov bp, sp`` (or ENTER). Not a guess -- the exact instruction model.
        lines: List[str] = []
        self.flush_pending_dos(lines)
        lines.append("sp = bp;")
        lines.append("bp = memw(ss, sp);")
        lines.append("sp = (sp + 2) & 0xFFFF;")
        return lines

    def handle_enter(self, insn: dict, known_funcs: Set[int]) -> List[str]:
        # ENTER imm16, imm8 (0xC8, 80186) -- build a stack frame. Per the Intel
        # SDM (16-bit operand size): push BP; FrameTemp = SP; for the nesting
        # level copy the enclosing frame pointers; BP = FrameTemp; SP -= alloc.
        # The level is a compile-time immediate, so the copy loop is unrolled.
        # Level 0 (the common case a compiler emits) reduces to
        # ``push bp; mov bp, sp; sub sp, alloc``.
        lines: List[str] = []
        self.flush_pending_dos(lines)
        parts = [p.strip() for p in insn.get("op_str", "").split(",")]
        alloc = (int(parts[0], 0) if parts and parts[0] else 0) & 0xFFFF
        level = ((int(parts[1], 0) if len(parts) > 1 and parts[1] else 0)
                 & 0xFF) % 32
        lines.append(f"/* enter 0x{alloc:X}, {level} */")
        lines.append("sp = (sp - 2) & 0xFFFF;")
        lines.append("memw_write(ss, sp, bp);")
        lines.append("{")
        lines.append("    uint16_t frame_temp = sp;")
        for _ in range(1, level):
            lines.append("    bp = (bp - 2) & 0xFFFF;")
            lines.append("    sp = (sp - 2) & 0xFFFF;")
            lines.append("    memw_write(ss, sp, memw(ss, bp));")
        if level > 0:
            lines.append("    sp = (sp - 2) & 0xFFFF;")
            lines.append("    memw_write(ss, sp, frame_temp);")
        lines.append("    bp = frame_temp;")
        lines.append("}")
        if alloc:
            lines.append(f"sp = (sp - 0x{alloc:X}) & 0xFFFF;")
        return lines

    def handle_xchg(
        self, insn: dict, known_funcs: Set[int]
    ) -> List[str] | None:
        lines: List[str] = []
        self.flush_pending_dos(lines)
        op_str = _decode_variables(insn.get("op_str", ""))
        parts = [p.strip() for p in op_str.split(",", 1)]
        if len(parts) == 2:
            dest, src = self._rewrite_operands(insn, parts)
            if dest != src:
                # xchg mutates both operands; drop any cached DOS argument
                # values so subsequent interrupt lifting does not reuse stale
                # register immediates.
                self._invalidate_register(dest)
                self._invalidate_register(src)
                width = (
                    _operand_width_from_str(dest)
                    or _operand_width_from_str(src)
                    or 16
                )
                ctype = "uint8_t" if width == 8 else "uint16_t"
                dest_val = _value_expr(dest)
                src_val = _value_expr(src)
                lines.append("{")
                lines.append(f"    {ctype} tmp = {dest_val};")
                _emit_store(lines, dest, src_val, indent="    ")
                _emit_store(lines, src, "tmp", indent="    ")
                lines.append("}")
                return lines
        asm = f"xchg {op_str}".strip()
        self._emit_unsupported_abort(lines, insn, asm)
        return lines

    def _emit_load_far_ptr(
        self, dest: str, seg_reg: str, mem_seg: str, off_expr: str, seg_expr: str
    ) -> List[str]:
        """Emit a faithful far-pointer load (``lds``/``les``): set ``dest`` from
        ``[mem_seg:off_expr]`` and ``seg_reg`` from ``[mem_seg:seg_expr]``.

        Both words are read from the ORIGINAL effective address. When the
        destination register also appears in the address expression -- e.g.
        ``lds di, [di]`` -- assigning ``dest`` first would corrupt the offset
        used for the segment word. Read the segment word into a temporary BEFORE
        mutating ``dest`` so the original offset is always used for both reads.
        """
        return [
            "{",
            f"    uint16_t _far_seg = memw({mem_seg}, {seg_expr});",
            f"    {dest} = memw({mem_seg}, {off_expr});",
            f"    {seg_reg} = _far_seg;",
            "}",
        ]

    def handle_lds(self, insn: dict, known_funcs: Set[int]) -> List[str]:
        lines: List[str] = []
        # `mov bx,...`/`mov dx,...` are deferred into pending_dos (DOS-call
        # register batching). lds reads its base register (e.g. `lds si,[bx+N]`)
        # so any pending write to that register MUST be materialised first --
        # otherwise the load uses the stale value. Flush before emitting.
        self.flush_pending_dos(lines)
        op_str = _decode_variables(insn.get("op_str", ""))
        parts = [p.strip() for p in op_str.split(",", 1)]
        if len(parts) == 2:
            dest, src = parts
            dest_l = dest.lower()

            # ``ptr`` operands omit the size; normalise for parsing
            if src.lower().startswith("ptr "):
                src = src[4:].strip()

            match = _MEM_PTR_RE.match(f"word ptr {src}")
            if match:
                _, seg, expr = match.groups()
                # x86: a `[bp+...]`/`[sp+...]` effective address defaults to the
                # STACK segment (SS), not DS. The disassembler emits the bare
                # operand with no explicit segment, so honour the BP/SP default
                # here -- otherwise les/lds reads its FAR POINTER from the wrong
                # segment whenever ds != ss (e.g. the SETUP driver-install path),
                # yielding a garbage pointer whose copy overruns the stack and
                # zeroes the return address. (In ds==ss paths it was dormant.)
                if seg:
                    seg = seg.lower()
                else:
                    seg = "ss" if re.search(r"\bbp\b", expr.lower()) else "ds"
                try:
                    base = 16 if expr.lower().startswith(("0x", "-0x")) else 10
                    off = int(expr, base) & 0xFFFF
                    expr1 = f"0x{off:04X}"
                    expr2 = f"0x{(off + 2) & 0xFFFF:04X}"
                except ValueError:
                    expr1 = expr
                    expr2 = f"{expr} + 2"

                # Overwrite any pending assignment to the destination register
                self.pending_dos = [
                    entry
                    for entry in self.pending_dos
                    if not entry.lower().startswith(f"{dest_l} =")
                ]
                if dest_l == "dx":
                    self.last_dx = None
                    self.last_dl = None
                lines.extend(self._emit_load_far_ptr(dest, "ds", seg, expr1, expr2))
                return lines

            src_l = src.lower()
            match = _VAR_RE.fullmatch(src_l)
            if match:
                mem_refs = insn.get("detail", {}).get("mem_refs", [])
                mem_ref = mem_refs[0] if mem_refs else None
                if mem_ref:
                    seg = mem_ref.get("segment", "ss").lower()
                    disp = mem_ref.get("disp", 0)
                else:
                    seg = "ss"
                    disp = -int(match.group(1), 16)
                expr1 = f"((bp + 0x{(disp & 0xFFFF):04X}) & 0xFFFF)"
                expr2 = f"((bp + 0x{((disp + 2) & 0xFFFF):04X}) & 0xFFFF)"
                self.pending_dos = [
                    entry
                    for entry in self.pending_dos
                    if not entry.lower().startswith(f"{dest_l}=")
                    and not entry.lower().startswith(f"{dest_l} =")
                ]
                if dest_l == "dx":
                    self.last_dx = None
                    self.last_dl = None
                lines.extend(self._emit_load_far_ptr(dest, "ds", seg, expr1, expr2))
                return lines

        self.flush_pending_dos(lines)
        asm = f"lds {op_str}".strip()
        self._emit_unsupported_abort(lines, insn, asm)
        return lines

    def handle_les(self, insn: dict, known_funcs: Set[int]) -> List[str]:
        lines: List[str] = []
        # `mov bx,...`/`mov dx,...` are deferred into pending_dos (DOS-call
        # register batching). les reads its base register (e.g. `les si,[bx+N]`)
        # so any pending write to that register MUST be materialised first --
        # otherwise the load uses the stale value. Flush before emitting.
        self.flush_pending_dos(lines)
        op_str = _decode_variables(insn.get("op_str", ""))
        parts = [p.strip() for p in op_str.split(",", 1)]
        if len(parts) == 2:
            dest, src = parts
            dest_l = dest.lower()

            # ``ptr`` operands omit the size; normalise for parsing
            if src.lower().startswith("ptr "):
                src = src[4:].strip()

            match = _MEM_PTR_RE.match(f"word ptr {src}")
            if match:
                _, seg, expr = match.groups()
                # x86: a `[bp+...]`/`[sp+...]` effective address defaults to the
                # STACK segment (SS), not DS. The disassembler emits the bare
                # operand with no explicit segment, so honour the BP/SP default
                # here -- otherwise les/lds reads its FAR POINTER from the wrong
                # segment whenever ds != ss (e.g. the SETUP driver-install path),
                # yielding a garbage pointer whose copy overruns the stack and
                # zeroes the return address. (In ds==ss paths it was dormant.)
                if seg:
                    seg = seg.lower()
                else:
                    seg = "ss" if re.search(r"\bbp\b", expr.lower()) else "ds"
                try:
                    base = 16 if expr.lower().startswith(("0x", "-0x")) else 10
                    off = int(expr, base) & 0xFFFF
                    expr1 = f"0x{off:04X}"
                    expr2 = f"0x{(off + 2) & 0xFFFF:04X}"
                except ValueError:
                    expr1 = expr
                    expr2 = f"{expr} + 2"

                # Overwrite any pending assignment to the destination register
                self.pending_dos = [
                    entry
                    for entry in self.pending_dos
                    if not entry.lower().startswith(f"{dest_l} =")
                ]
                if dest_l == "dx":
                    self.last_dx = None
                    self.last_dl = None
                lines.extend(self._emit_load_far_ptr(dest, "es", seg, expr1, expr2))
                return lines

            src_l = src.lower()
            match = _VAR_RE.fullmatch(src_l)
            if match:
                mem_refs = insn.get("detail", {}).get("mem_refs", [])
                mem_ref = mem_refs[0] if mem_refs else None
                if mem_ref:
                    seg = mem_ref.get("segment", "ss").lower()
                    disp = mem_ref.get("disp", 0)
                else:
                    seg = "ss"
                    disp = -int(match.group(1), 16)
                expr1 = f"((bp + 0x{(disp & 0xFFFF):04X}) & 0xFFFF)"
                expr2 = f"((bp + 0x{((disp + 2) & 0xFFFF):04X}) & 0xFFFF)"
                self.pending_dos = [
                    entry
                    for entry in self.pending_dos
                    if not entry.lower().startswith(f"{dest_l}=")
                    and not entry.lower().startswith(f"{dest_l} =")
                ]
                if dest_l == "dx":
                    self.last_dx = None
                    self.last_dl = None
                lines.extend(self._emit_load_far_ptr(dest, "es", seg, expr1, expr2))
                return lines

        self.flush_pending_dos(lines)
        asm = f"les {op_str}".strip()
        self._emit_unsupported_abort(lines, insn, asm)
        return lines

    def handle_mov(self, insn: dict, known_funcs: Set[int]) -> List[str]:
        lines: List[str] = []
        op_str = _decode_variables(insn.get("op_str", ""))
        parts = [p.strip() for p in op_str.split(",", 1)]
        if len(parts) == 2:
            dest, src = self._rewrite_operands(insn, parts)

            dest_rcb = _match_rcb_access(dest)
            src_rcb = _match_rcb_access(src)
            if dest_rcb:
                size, field = dest_rcb
                func = "rcb_write16" if size == "w" else "rcb_write8"
                lines.append(f"{func}({field}, {src});")
                return lines
            if src_rcb:
                size, field = src_rcb
                func = "rcb_read16" if size == "w" else "rcb_read8"
                lines.append(f"{dest} = {func}({field});")
                return lines

            imm_value = _parse_imm(src)
            if imm_value is not None:
                size_bytes = len(insn.get("bytes", "")) // 2
                if size_bytes >= 3:
                    imm_offset = insn["address"] + size_bytes - 2
                    if imm_offset in self._reloc_offsets:
                        imm_value = (imm_value + self._load_segment) & 0xFFFF
                        src = f"0x{imm_value:04X}"

            dest_l = dest.lower()
            if dest_l == "ss":
                self.flush_pending_dos(lines)
                lines.append(f"{dest} = {src};")
                lines.append("interrupt_shadow = 1;")
                return lines
            if dest_l in {"ah", "ax", "al"}:
                self._invalidate_register(dest)
                if self.handle_dos_interrupt_prep(dest, src, lines):
                    return lines
                self.flush_pending_dos(lines)
                lines.append(f"{dest} = {src};")
                return lines
            # The bx/dx/dl deferral below queues the assignment into
            # pending_dos (so a following INT 21h/10h can reconstruct literal
            # arguments) AND dedup-drops any earlier pending write to the same
            # register, assuming that earlier write is now dead. That assumption
            # only holds for IMMEDIATE sources. For a memory/register source the
            # value is live: e.g. `mov bx, [bp+6]; mov bx, [bx]` — the second
            # load DEPENDS on the first, so dropping the first and keeping the
            # second makes `bx = [bx]` read a stale bx. Emit non-immediate
            # writes immediately, in order, and clear the literal tracker.
            if dest_l in {"dx", "dl", "bx"} and _parse_imm(src) is None:
                self.flush_pending_dos(lines)
                if dest_l in {"dx", "dl"}:
                    self.last_dx = None
                    self.last_dl = None
                else:
                    self.last_bx = None
                lines.append(f"{dest} = {src};")
                return lines
            if dest_l == "dx":
                val = _parse_imm(src)
                self.last_dx = val
                self.last_dl = val & 0xFF if val is not None else None
                self.pending_dos = [
                    entry
                    for entry in self.pending_dos
                    if not entry.lower().startswith("dx =")
                ]
                self.pending_dos.append(f"{dest} = {src};")
                return lines
            if dest_l == "dl":
                self.last_dx = None
                self.last_dl = _parse_imm(src)
                self.pending_dos = [
                    entry
                    for entry in self.pending_dos
                    if not entry.lower().startswith("dl =")
                ]
                self.pending_dos.append(f"{dest} = {src};")
                return lines
            if dest_l == "bx":
                self.last_bx = _parse_imm(src)
                self.pending_dos = [
                    entry
                    for entry in self.pending_dos
                    if not entry.lower().startswith("bx =")
                ]
                self.pending_dos.append(f"{dest} = {src};")
                return lines
            if dest_l == "cx":
                self.flush_pending_dos(lines)
                val = _parse_imm(src)
                self.last_cx = val
                self.last_cl = val & 0xFF if val is not None else None
                self.last_ch = (val >> 8) & 0xFF if val is not None else None
                lines.append(f"{dest} = {src};")
                return lines
            if dest_l == "cl":
                self.flush_pending_dos(lines)
                self.last_cx = None
                self.last_ch = None
                self.last_cl = _parse_imm(src)
                lines.append(f"{dest} = {src};")
                return lines
            if dest_l == "ch":
                self.flush_pending_dos(lines)
                self.last_cx = None
                self.last_cl = None
                self.last_ch = _parse_imm(src)
                lines.append(f"{dest} = {src};")
                return lines
            self.flush_pending_dos(lines)
            if dest.startswith("memw(") or dest.startswith("memb("):
                inner = dest[5:-1]
                writer = (
                    "memw_write" if dest.startswith("memw(") else "memb_write"
                )
                lines.append(f"{writer}({inner}, {src});")
            else:
                lines.append(f"{dest} = {src};")
            return lines
        self.flush_pending_dos(lines)
        asm = f"mov {op_str}".strip()
        self._emit_unsupported_abort(lines, insn, asm)
        return lines

    def handle_lea(self, insn: dict, known_funcs: Set[int]) -> List[str]:
        lines: List[str] = []
        self.flush_pending_dos(lines)
        op_str = _decode_variables(insn.get("op_str", ""))
        parts = [p.strip() for p in op_str.split(",", 1)]
        if len(parts) != 2:
            asm = f"lea {op_str}".strip()
            self._emit_unsupported_abort(lines, insn, asm)
            return lines
        dest, src = parts
        self._invalidate_register(dest)
        src_l = src.lower()
        mem_refs = insn.get("detail", {}).get("mem_refs", [])
        match = _VAR_RE.fullmatch(src_l)
        if match:
            disp = mem_refs[0].get("disp", 0) if mem_refs else -int(match.group(1), 16)
            expr = f"((bp + 0x{(disp & 0xFFFF):04X}) & 0xFFFF)"
            lines.append(f"{dest} = {expr};")
            return lines
        if src.startswith("[") and src.endswith("]"):
            inner = src[1:-1].strip()
            lines.append(f"{dest} = ({inner}) & 0xFFFF;")
            return lines
        try:
            base = 16 if src_l.startswith(("0x", "-0x")) else 10
            value = int(src, base)
        except ValueError:
            asm = f"lea {insn.get('op_str', '')}".strip()
            self._emit_unsupported_abort(lines, insn, asm)
            return lines
        lines.append(f"{dest} = 0x{value & 0xFFFF:04X};")
        return lines

    def handle_arithmetic(
        self, insn: dict, known_funcs: Set[int]
    ) -> List[str] | None:
        lines: List[str] = []
        self.flush_pending_dos(lines)
        op_str = _decode_variables(insn.get("op_str", ""))
        parts = [p.strip() for p in op_str.split(",", 1)]
        mnem = insn["mnemonic"]
        byte_regs = {"al", "ah", "bl", "bh", "cl", "ch", "dl", "dh"}
        if mnem == "add" and len(parts) == 2:
            dest, src = self._rewrite_operands(insn, parts)
            dest_rcb = _match_rcb_access(dest)
            src_rcb = _match_rcb_access(src)
            self._invalidate_register(dest)
            src_expr = src
            if src_rcb:
                size, field = src_rcb
                read = "rcb_read16" if size == "w" else "rcb_read8"
                src_expr = f"{read}({field})"
            if dest_rcb:
                size, field = dest_rcb
                read = "rcb_read16" if size == "w" else "rcb_read8"
                write = "rcb_write16" if size == "w" else "rcb_write8"
                mask = "0xFFFF" if size == "w" else "0xFF"
                sign_bit = "0x8000" if size == "w" else "0x80"
                lines.append("{")
                lines.append(f"    uint32_t old = {read}({field});")
                lines.append(f"    uint32_t src = {src_expr};")
                lines.append("    uint32_t tmp = old + src;")
                lines.append(f"    CF = tmp > {mask};")
                lines.append(f"    {write}({field}, tmp & {mask});")
                lines.append(
                    f"    OF = (~(old ^ src) & (old ^ tmp) & "
                    f"{sign_bit}) != 0;"
                )
                lines.append("}")
                lines.append(f"ZF = {read}({field}) == 0;")
                lines.append(
                    f"PF = parity8((uint8_t){read}({field}));"
                )
                shift = 15 if size == "w" else 7
                lines.append(f"SF = ({read}({field}) >> {shift}) & 1;")
            else:
                if dest.startswith("memw(") or dest.startswith("memb("):
                    inner = dest[5:-1]
                    acc = "memw" if dest.startswith("memw(") else "memb"
                    writer = acc + "_write"
                    mask = "0xFFFF" if acc == "memw" else "0xFF"
                    sign_bit = "0x8000" if acc == "memw" else "0x80"
                    lines.append("{")
                    lines.append(f"    uint32_t old = {acc}({inner});")
                    lines.append(f"    uint32_t src = {src_expr};")
                    lines.append("    uint32_t tmp = old + src;")
                    lines.append(f"    CF = tmp > {mask};")
                    lines.append(f"    {writer}({inner}, tmp & {mask});")
                    lines.append(
                        f"    OF = (~(old ^ src) & (old ^ tmp) & "
                        f"{sign_bit}) != 0;"
                    )
                    lines.append("}")
                    lines.append(f"ZF = {acc}({inner}) == 0;")
                    lines.append(
                        f"PF = parity8((uint8_t){acc}({inner}));"
                    )
                    shift = 7 if acc == "memb" else 15
                    lines.append(f"SF = ({acc}({inner}) >> {shift}) & 1;")
                else:
                    is_byte = dest.lower() in byte_regs
                    mask = "0xFF" if is_byte else "0xFFFF"
                    sign_bit = "0x80" if is_byte else "0x8000"
                    lines.append("{")
                    lines.append(f"    uint32_t old = {dest};")
                    lines.append(f"    uint32_t src = {src_expr};")
                    lines.append("    uint32_t tmp = old + src;")
                    lines.append(f"    CF = tmp > {mask};")
                    lines.append(f"    {dest} = tmp & {mask};")
                    lines.append(
                        f"    OF = (~(old ^ src) & (old ^ tmp) & "
                        f"{sign_bit}) != 0;"
                    )
                    lines.append("}")
                    lines.append(f"ZF = {dest} == 0;")
                    lines.append(f"PF = parity8((uint8_t){dest});")
                    shift = 7 if is_byte else 15
                    lines.append(f"SF = ({dest} >> {shift}) & 1;")
            return lines
        if mnem == "adc" and len(parts) == 2:
            dest, src = self._rewrite_operands(insn, parts)
            dest_rcb = _match_rcb_access(dest)
            src_rcb = _match_rcb_access(src)
            self._invalidate_register(dest)
            src_expr = src
            if src_rcb:
                size, field = src_rcb
                read = "rcb_read16" if size == "w" else "rcb_read8"
                src_expr = f"{read}({field})"
            if dest_rcb:
                size, field = dest_rcb
                read = "rcb_read16" if size == "w" else "rcb_read8"
                write = "rcb_write16" if size == "w" else "rcb_write8"
                mask = "0xFFFF" if size == "w" else "0xFF"
                sign_bit = "0x8000" if size == "w" else "0x80"
                lines.append("{")
                lines.append(f"    uint32_t old = {read}({field});")
                lines.append(f"    uint32_t src = {src_expr};")
                lines.append("    uint32_t tmp = old + src + CF;")
                lines.append(f"    CF = tmp > {mask};")
                lines.append(f"    {write}({field}, tmp & {mask});")
                lines.append(
                    f"    OF = (~(old ^ src) & (old ^ tmp) & "
                    f"{sign_bit}) != 0;"
                )
                lines.append("}")
                lines.append(f"ZF = {read}({field}) == 0;")
                lines.append(
                    f"PF = parity8((uint8_t){read}({field}));"
                )
                shift = 15 if size == "w" else 7
                lines.append(f"SF = ({read}({field}) >> {shift}) & 1;")
            elif dest.startswith("memw(") or dest.startswith("memb("):
                inner = dest[5:-1]
                acc = "memw" if dest.startswith("memw(") else "memb"
                writer = acc + "_write"
                mask = "0xFFFF" if acc == "memw" else "0xFF"
                sign_bit = "0x8000" if acc == "memw" else "0x80"
                lines.append("{")
                lines.append(f"    uint32_t old = {acc}({inner});")
                lines.append(f"    uint32_t src = {src_expr};")
                lines.append("    uint32_t tmp = old + src + CF;")
                lines.append(f"    CF = tmp > {mask};")
                lines.append(f"    {writer}({inner}, tmp & {mask});")
                lines.append(
                    f"    OF = (~(old ^ src) & (old ^ tmp) & {sign_bit}) != 0;"
                )
                lines.append("}")
                lines.append(f"ZF = {acc}({inner}) == 0;")
                lines.append(
                    f"PF = parity8((uint8_t){acc}({inner}));"
                )
                is_byte = dest.startswith("memb(")
                shift = 7 if is_byte else 15
                lines.append(f"SF = ({acc}({inner}) >> {shift}) & 1;")
            else:
                is_byte = dest.lower() in byte_regs
                mask = "0xFF" if is_byte else "0xFFFF"
                sign_bit = "0x80" if is_byte else "0x8000"
                lines.append("{")
                lines.append(f"    uint32_t old = {dest};")
                lines.append(f"    uint32_t src = {src_expr};")
                lines.append("    uint32_t tmp = old + src + CF;")
                lines.append(f"    CF = tmp > {mask};")
                lines.append(f"    {dest} = tmp & {mask};")
                lines.append(
                    f"    OF = (~(old ^ src) & (old ^ tmp) & {sign_bit}) != 0;"
                )
                lines.append("}")
                lines.append(f"ZF = {dest} == 0;")
                lines.append(f"PF = parity8((uint8_t){dest});")
                shift = 7 if is_byte else 15
                lines.append(f"SF = ({dest} >> {shift}) & 1;")
            return lines
        if mnem == "sub" and len(parts) == 2:
            dest, src = self._rewrite_operands(insn, parts)
            dest_rcb = _match_rcb_access(dest)
            src_rcb = _match_rcb_access(src)
            self._invalidate_register(dest)
            src_expr = src
            if src_rcb:
                size, field = src_rcb
                read = "rcb_read16" if size == "w" else "rcb_read8"
                src_expr = f"{read}({field})"
            if dest_rcb:
                size, field = dest_rcb
                read = "rcb_read16" if size == "w" else "rcb_read8"
                write = "rcb_write16" if size == "w" else "rcb_write8"
                mask = "0xFFFF" if size == "w" else "0xFF"
                sign_bit = "0x8000" if size == "w" else "0x80"
                lines.append("{")
                lines.append(f"    uint32_t old = {read}({field});")
                lines.append(f"    uint32_t src = {src_expr};")
                lines.append("    CF = old < src;")
                lines.append("    uint32_t tmp = old - src;")
                lines.append(f"    {write}({field}, tmp & {mask});")
                lines.append(
                    f"    OF = ((old ^ src) & (old ^ tmp) & "
                    f"{sign_bit}) != 0;"
                )
                lines.append("}")
                lines.append(f"ZF = {read}({field}) == 0;")
                lines.append(
                    f"PF = parity8((uint8_t){read}({field}));"
                )
                shift = 15 if size == "w" else 7
                lines.append(f"SF = ({read}({field}) >> {shift}) & 1;")
            else:
                if dest.startswith("memw(") or dest.startswith("memb("):
                    inner = dest[5:-1]
                    acc = "memw" if dest.startswith("memw(") else "memb"
                    writer = acc + "_write"
                    mask = "0xFFFF" if acc == "memw" else "0xFF"
                    sign_bit = "0x8000" if acc == "memw" else "0x80"
                    lines.append("{")
                    lines.append(f"    uint32_t old = {acc}({inner});")
                    lines.append(f"    uint32_t src = {src_expr};")
                    lines.append("    CF = old < src;")
                    lines.append("    uint32_t tmp = old - src;")
                    lines.append(f"    {writer}({inner}, tmp & {mask});")
                    lines.append(
                        f"    OF = ((old ^ src) & (old ^ tmp) & "
                        f"{sign_bit}) != 0;"
                    )
                    lines.append("}")
                    lines.append(f"ZF = {acc}({inner}) == 0;")
                    lines.append(
                        f"PF = parity8((uint8_t){acc}({inner}));"
                    )
                    shift = 7 if acc == "memb" else 15
                    lines.append(f"SF = ({acc}({inner}) >> {shift}) & 1;")
                else:
                    is_byte = dest.lower() in byte_regs
                    mask = "0xFF" if is_byte else "0xFFFF"
                    sign_bit = "0x80" if is_byte else "0x8000"
                    lines.append("{")
                    lines.append(f"    uint32_t old = {dest};")
                    lines.append(f"    uint32_t src = {src_expr};")
                    lines.append("    CF = old < src;")
                    lines.append("    uint32_t tmp = old - src;")
                    lines.append(f"    {dest} = tmp & {mask};")
                    lines.append(
                        f"    OF = ((old ^ src) & (old ^ tmp) & "
                        f"{sign_bit}) != 0;"
                    )
                    lines.append("}")
                    lines.append(f"ZF = {dest} == 0;")
                    lines.append(f"PF = parity8((uint8_t){dest});")
                    shift = 7 if is_byte else 15
                    lines.append(f"SF = ({dest} >> {shift}) & 1;")
            return lines
        if mnem == "sbb" and len(parts) == 2:
            dest, src = self._rewrite_operands(insn, parts)
            dest_rcb = _match_rcb_access(dest)
            src_rcb = _match_rcb_access(src)
            self._invalidate_register(dest)
            src_expr = src
            if src_rcb:
                size, field = src_rcb
                read = "rcb_read16" if size == "w" else "rcb_read8"
                src_expr = f"{read}({field})"
            if dest_rcb:
                size, field = dest_rcb
                read = "rcb_read16" if size == "w" else "rcb_read8"
                write = "rcb_write16" if size == "w" else "rcb_write8"
                mask = "0xFFFF" if size == "w" else "0xFF"
                sign_bit = "0x8000" if size == "w" else "0x80"
                lines.append("{")
                lines.append(f"    uint32_t old = {read}({field});")
                lines.append(f"    uint32_t src = {src_expr} + CF;")
                lines.append("    CF = old < src;")
                lines.append("    uint32_t tmp = old - src;")
                lines.append(f"    {write}({field}, tmp & {mask});")
                lines.append(
                    f"    OF = ((old ^ src) & (old ^ tmp) & "
                    f"{sign_bit}) != 0;"
                )
                lines.append("}")
                lines.append(f"ZF = {read}({field}) == 0;")
                lines.append(
                    f"PF = parity8((uint8_t){read}({field}));"
                )
                shift = 15 if size == "w" else 7
                lines.append(f"SF = ({read}({field}) >> {shift}) & 1;")
            else:
                if dest.startswith("memw(") or dest.startswith("memb("):
                    inner = dest[5:-1]
                    acc = "memw" if dest.startswith("memw(") else "memb"
                    writer = acc + "_write"
                    mask = "0xFFFF" if acc == "memw" else "0xFF"
                    sign_bit = "0x8000" if acc == "memw" else "0x80"
                    lines.append("{")
                    lines.append(f"    uint32_t old = {acc}({inner});")
                    lines.append(f"    uint32_t src = {src_expr} + CF;")
                    lines.append("    CF = old < src;")
                    lines.append("    uint32_t tmp = old - src;")
                    lines.append(f"    {writer}({inner}, tmp & {mask});")
                    lines.append(
                        f"    OF = ((old ^ src) & (old ^ tmp) & "
                        f"{sign_bit}) != 0;"
                    )
                    lines.append("}")
                    lines.append(f"ZF = {acc}({inner}) == 0;")
                    lines.append(
                        f"PF = parity8((uint8_t){acc}({inner}));"
                    )
                    shift = 7 if acc == "memb" else 15
                    lines.append(f"SF = ({acc}({inner}) >> {shift}) & 1;")
                else:
                    is_byte = dest.lower() in byte_regs
                    mask = "0xFF" if is_byte else "0xFFFF"
                    sign_bit = "0x80" if is_byte else "0x8000"
                    lines.append("{")
                    lines.append(f"    uint32_t old = {dest};")
                    lines.append(f"    uint32_t src = {src_expr} + CF;")
                    lines.append("    CF = old < src;")
                    lines.append("    uint32_t tmp = old - src;")
                    lines.append(f"    {dest} = tmp & {mask};")
                    lines.append(
                        f"    OF = ((old ^ src) & (old ^ tmp) & "
                        f"{sign_bit}) != 0;"
                    )
                    lines.append("}")
                    lines.append(f"ZF = {dest} == 0;")
                    lines.append(f"PF = parity8((uint8_t){dest});")
                    shift = 7 if is_byte else 15
                    lines.append(f"SF = ({dest} >> {shift}) & 1;")
            return lines
        if mnem == "and" and len(parts) == 2:
            dest, src = self._rewrite_operands(insn, parts)
            dest_rcb = _match_rcb_access(dest)
            src_rcb = _match_rcb_access(src)
            self._invalidate_register(dest)
            if dest_rcb:
                size, field = dest_rcb
                read = "rcb_read16" if size == "w" else "rcb_read8"
                write = "rcb_write16" if size == "w" else "rcb_write8"
                mask = "0xFFFF" if size == "w" else "0xFF"
                lines.append(
                    f"{write}({field}, ({read}({field}) & {src}) & {mask});"
                )
                lines.append("CF = 0;")
                lines.append("OF = 0;")
                lines.append(f"ZF = {read}({field}) == 0;")
                lines.append(
                    f"PF = parity8((uint8_t){read}({field}));"
                )
                shift = 15 if size == "w" else 7
                lines.append(f"SF = ({read}({field}) >> {shift}) & 1;")
            else:
                src_expr = src
                if src_rcb:
                    size, field = src_rcb
                    read = "rcb_read16" if size == "w" else "rcb_read8"
                    src_expr = f"{read}({field})"
                if dest.startswith("memw(") or dest.startswith("memb("):
                    inner = dest[5:-1]
                    acc = "memw" if dest.startswith("memw(") else "memb"
                    writer = acc + "_write"
                    mask = "0xFFFF" if acc == "memw" else "0xFF"
                    lines.append("{")
                    lines.append(f"    uint32_t old = {acc}({inner});")
                    lines.append(f"    uint32_t src = {src_expr};")
                    lines.append(f"    uint32_t tmp = (old & src) & {mask};")
                    lines.append(f"    {writer}({inner}, tmp);")
                    lines.append("    CF = 0;")
                    lines.append("    OF = 0;")
                    shift = 7 if acc == "memb" else 15
                    lines.append("    ZF = tmp == 0;")
                    lines.append("    PF = parity8((uint8_t)tmp);")
                    lines.append(f"    SF = (tmp >> {shift}) & 1;")
                    lines.append("}")
                    return lines
                else:
                    is_byte = dest.lower() in byte_regs
                    mask = "0xFF" if is_byte else "0xFFFF"
                    lines.append(f"{dest} = ({dest} & {src_expr}) & {mask};")
                lines.append("CF = 0;")
                lines.append("OF = 0;")
                is_byte = dest.startswith("memb(") or dest.lower() in byte_regs
                if dest.startswith("memw(") or dest.startswith("memb("):
                    lines.append("ZF = tmp == 0;")
                    lines.append("PF = parity8((uint8_t)tmp);")
                    lines.append(f"SF = (tmp >> {shift}) & 1;")
                else:
                    lines.append(f"ZF = {dest} == 0;")
                    lines.append(f"PF = parity8((uint8_t){dest});")
                    shift = 7 if is_byte else 15
                    lines.append(f"SF = ({dest} >> {shift}) & 1;")
            return lines
        if mnem == "xor" and len(parts) == 2:
            dest, src = self._rewrite_operands(insn, parts)
            dest_rcb = _match_rcb_access(dest)
            src_rcb = _match_rcb_access(src)
            self._invalidate_register(dest)
            if dest == src:
                if dest_rcb:
                    size, field = dest_rcb
                    write = "rcb_write16" if size == "w" else "rcb_write8"
                    lines.append(f"{write}({field}, 0);")
                    lines.append("CF = 0;")
                    lines.append("OF = 0;")
                    lines.append("ZF = 1;")
                    lines.append("PF = 1;")
                    lines.append("SF = 0;")
                elif dest.startswith("memw(") or dest.startswith("memb("):
                    inner = dest[5:-1]
                    writer = (
                        "memw_write" if dest.startswith("memw(") else
                        "memb_write"
                    )
                    lines.append(f"{writer}({inner}, 0);")
                    lines.append("CF = 0;")
                    lines.append("OF = 0;")
                    lines.append("ZF = 1;")
                    lines.append("PF = 1;")
                    lines.append("SF = 0;")
                else:
                    is_byte = dest.lower() in byte_regs
                    helper = "xor8" if is_byte else "xor16"
                    lines.append(f"{helper}(&{dest}, {src});")
            else:
                src_expr = src
                if src_rcb:
                    size, field = src_rcb
                    read = "rcb_read16" if size == "w" else "rcb_read8"
                    src_expr = f"{read}({field})"
                if dest_rcb:
                    size, field = dest_rcb
                    read = "rcb_read16" if size == "w" else "rcb_read8"
                    write = "rcb_write16" if size == "w" else "rcb_write8"
                    mask = "0xFFFF" if size == "w" else "0xFF"
                    lines.append(
                        f"{write}({field}, ({read}({field}) ^ {src_expr}) & "
                        f"{mask});"
                    )
                    lines.append("CF = 0;")
                    lines.append("OF = 0;")
                    lines.append(f"ZF = {read}({field}) == 0;")
                    lines.append(
                        f"PF = parity8((uint8_t){read}({field}));"
                    )
                    shift = 15 if size == "w" else 7
                    lines.append(f"SF = ({read}({field}) >> {shift}) & 1;")
                elif dest.startswith("memw(") or dest.startswith("memb("):
                    inner = dest[5:-1]
                    acc = "memw" if dest.startswith("memw(") else "memb"
                    writer = acc + "_write"
                    mask = "0xFFFF" if acc == "memw" else "0xFF"
                    lines.append(
                        f"{writer}({inner}, ({acc}({inner}) ^ {src_expr}) & "
                        f"{mask});"
                    )
                    lines.append("CF = 0;")
                    lines.append("OF = 0;")
                    lines.append(f"ZF = {dest} == 0;")
                    lines.append(f"PF = parity8((uint8_t){dest});")
                    shift = 7 if acc == "memb" else 15
                    lines.append(f"SF = ({dest} >> {shift}) & 1;")
                else:
                    is_byte = dest.lower() in byte_regs
                    mask = "0xFF" if is_byte else "0xFFFF"
                    lines.append(f"{dest} = ({dest} ^ {src_expr}) & {mask};")
                    lines.append("CF = 0;")
                    lines.append("OF = 0;")
                    lines.append(f"ZF = {dest} == 0;")
                    lines.append(f"PF = parity8((uint8_t){dest});")
                    shift = 7 if dest.lower() in byte_regs else 15
                    lines.append(f"SF = ({dest} >> {shift}) & 1;")
            return lines
        if mnem == "or" and len(parts) == 2:
            dest, src = self._rewrite_operands(insn, parts)
            dest_rcb = _match_rcb_access(dest)
            src_rcb = _match_rcb_access(src)
            self._invalidate_register(dest)
            if dest_rcb:
                size, field = dest_rcb
                read = "rcb_read16" if size == "w" else "rcb_read8"
                write = "rcb_write16" if size == "w" else "rcb_write8"
                mask = "0xFFFF" if size == "w" else "0xFF"
                lines.append(
                    f"{write}({field}, ({read}({field}) | {src}) & {mask});"
                )
                lines.append("CF = 0;")
                lines.append("OF = 0;")
                lines.append(f"ZF = {read}({field}) == 0;")
                lines.append(
                    f"PF = parity8((uint8_t){read}({field}));"
                )
                shift = 15 if size == "w" else 7
                lines.append(f"SF = ({read}({field}) >> {shift}) & 1;")
            else:
                src_expr = src
                if src_rcb:
                    size, field = src_rcb
                    read = "rcb_read16" if size == "w" else "rcb_read8"
                    src_expr = f"{read}({field})"
                if dest.startswith("memw(") or dest.startswith("memb("):
                    inner = dest[5:-1]
                    acc = "memw" if dest.startswith("memw(") else "memb"
                    writer = acc + "_write"
                    mask = "0xFFFF" if acc == "memw" else "0xFF"
                    lines.append("{")
                    lines.append(f"    uint32_t old = {acc}({inner});")
                    lines.append(f"    uint32_t src = {src_expr};")
                    lines.append(f"    uint32_t tmp = (old | src) & {mask};")
                    lines.append(f"    {writer}({inner}, tmp);")
                    lines.append("    CF = 0;")
                    lines.append("    OF = 0;")
                    shift = 7 if acc == "memb" else 15
                    lines.append("    ZF = tmp == 0;")
                    lines.append("    PF = parity8((uint8_t)tmp);")
                    lines.append(f"    SF = (tmp >> {shift}) & 1;")
                    lines.append("}")
                    return lines
                else:
                    is_byte = dest.lower() in byte_regs
                    mask = "0xFF" if is_byte else "0xFFFF"
                    lines.append(f"{dest} = ({dest} | {src_expr}) & {mask};")
                lines.append("CF = 0;")
                lines.append("OF = 0;")
                is_byte = dest.startswith("memb(") or dest.lower() in byte_regs
                if dest.startswith("memw(") or dest.startswith("memb("):
                    lines.append("ZF = tmp == 0;")
                    lines.append("PF = parity8((uint8_t)tmp);")
                    lines.append(f"SF = (tmp >> {shift}) & 1;")
                else:
                    lines.append(f"ZF = {dest} == 0;")
                    lines.append(f"PF = parity8((uint8_t){dest});")
                    shift = 7 if is_byte else 15
                    lines.append(f"SF = ({dest} >> {shift}) & 1;")
            return lines
        if mnem == "inc":
            operand = self._rewrite_operands(insn, [op_str.strip()])[0]
            operand_rcb = _match_rcb_access(operand)
            self._invalidate_register(operand)
            if operand_rcb:
                size, field = operand_rcb
                read = "rcb_read16" if size == "w" else "rcb_read8"
                write = "rcb_write16" if size == "w" else "rcb_write8"
                mask = "0xFFFF" if size == "w" else "0xFF"
                limit_minus1 = "0x7FFF" if size == "w" else "0x7F"
                shift = 15 if size == "w" else 7
                lines.append("{")
                lines.append(f"    uint32_t old = {read}({field});")
                lines.append(f"    {write}({field}, (old + 1) & {mask});")
                lines.append(f"    ZF = {read}({field}) == 0;")
                lines.append(
                    f"    PF = parity8((uint8_t){read}({field}));"
                )
                lines.append(f"    SF = ({read}({field}) >> {shift}) & 1;")
                lines.append(f"    OF = old == {limit_minus1};")
                lines.append("}")
            elif operand.startswith("memw(") or operand.startswith("memb("):
                inner = operand[5:-1]
                acc = "memw" if operand.startswith("memw(") else "memb"
                writer = acc + "_write"
                mask = "0xFFFF" if acc == "memw" else "0xFF"
                limit_minus1 = "0x7FFF" if acc == "memw" else "0x7F"
                shift = 15 if acc == "memw" else 7
                lines.append("{")
                lines.append(f"    uint32_t old = {acc}({inner});")
                lines.append(f"    {writer}({inner}, (old + 1) & {mask});")
                lines.append(f"    ZF = {acc}({inner}) == 0;")
                lines.append(
                    f"    PF = parity8((uint8_t){acc}({inner}));"
                )
                lines.append(f"    SF = ({acc}({inner}) >> {shift}) & 1;")
                lines.append(f"    OF = old == {limit_minus1};")
                lines.append("}")
            else:
                is_byte = operand.lower() in byte_regs
                limit_minus1 = "0x7F" if is_byte else "0x7FFF"
                shift = 7 if is_byte else 15
                lines.append("{")
                lines.append(f"    uint32_t old = {operand};")
                mask = "0xFF" if is_byte else "0xFFFF"
                lines.append(f"    {operand} = ({operand} + 1) & {mask};")
                lines.append(f"    ZF = {operand} == 0;")
                lines.append(
                    f"    PF = parity8((uint8_t){operand});"
                )
                lines.append(f"    SF = ({operand} >> {shift}) & 1;")
                lines.append(f"    OF = old == {limit_minus1};")
                lines.append("}")
            return lines
        if mnem == "dec":
            operand = self._rewrite_operands(insn, [op_str.strip()])[0]
            operand_rcb = _match_rcb_access(operand)
            self._invalidate_register(operand)
            if operand_rcb:
                size, field = operand_rcb
                read = "rcb_read16" if size == "w" else "rcb_read8"
                write = "rcb_write16" if size == "w" else "rcb_write8"
                mask = "0xFFFF" if size == "w" else "0xFF"
                sign_bit = "0x8000" if size == "w" else "0x80"
                shift = 15 if size == "w" else 7
                lines.append("{")
                lines.append(f"    uint32_t old = {read}({field});")
                lines.append(f"    {write}({field}, (old - 1) & {mask});")
                lines.append(f"    ZF = {read}({field}) == 0;")
                lines.append(
                    f"    PF = parity8((uint8_t){read}({field}));"
                )
                lines.append(f"    SF = ({read}({field}) >> {shift}) & 1;")
                lines.append(f"    OF = old == {sign_bit};")
                lines.append("}")
            elif operand.startswith("memw(") or operand.startswith("memb("):
                inner = operand[5:-1]
                acc = "memw" if operand.startswith("memw(") else "memb"
                writer = acc + "_write"
                mask = "0xFFFF" if acc == "memw" else "0xFF"
                sign_bit = "0x8000" if acc == "memw" else "0x80"
                shift = 15 if acc == "memw" else 7
                lines.append("{")
                lines.append(f"    uint32_t old = {acc}({inner});")
                lines.append(f"    {writer}({inner}, (old - 1) & {mask});")
                lines.append(f"    ZF = {acc}({inner}) == 0;")
                lines.append(
                    f"    PF = parity8((uint8_t){acc}({inner}));"
                )
                lines.append(f"    SF = ({acc}({inner}) >> {shift}) & 1;")
                lines.append(f"    OF = old == {sign_bit};")
                lines.append("}")
            else:
                is_byte = operand.lower() in byte_regs
                sign_bit = "0x80" if is_byte else "0x8000"
                shift = 7 if is_byte else 15
                lines.append("{")
                lines.append(f"    uint32_t old = {operand};")
                mask = "0xFF" if is_byte else "0xFFFF"
                lines.append(f"    {operand} = ({operand} - 1) & {mask};")
                lines.append(f"    ZF = {operand} == 0;")
                lines.append(
                    f"    PF = parity8((uint8_t){operand});"
                )
                lines.append(f"    SF = ({operand} >> {shift}) & 1;")
                lines.append(f"    OF = old == {sign_bit};")
                lines.append("}")
            return lines
        return None

    def handle_mul(self, insn: dict, known_funcs: Set[int]) -> List[str]:
        lines: List[str] = []
        self.flush_pending_dos(lines)
        op_str = _decode_variables(insn.get("op_str", "")).strip()
        operand = self._rewrite_operands(insn, [op_str])[0]
        # ``mul`` always clobbers AX and may update DX for 16-bit operands
        self._invalidate_register("dx")
        self._invalidate_register("ax")
        regs_write = {
            reg.lower() for reg in insn.get("detail", {}).get("regs_write", [])
        }
        if "dx" in regs_write:
            lines.append("{")
            lines.append(f"    uint32_t tmp = (uint32_t)ax * {operand};")
            lines.append("    dx = (tmp >> 16) & 0xFFFF;")
            lines.append("    ax = tmp & 0xFFFF;")
            lines.append("}")
            lines.append("CF = OF = dx != 0;")
        else:
            lines.append(f"ax = (al * {operand}) & 0xFFFF;")
            lines.append("CF = OF = ah != 0;")
        return lines

    def handle_imul(self, insn: dict, known_funcs: Set[int]) -> List[str]:
        lines: List[str] = []
        self.flush_pending_dos(lines)
        op_str = _decode_variables(insn.get("op_str", "")).strip()
        regs_write = {
            reg.lower() for reg in insn.get("detail", {}).get("regs_write", [])
        }
        # Intel-syntax operands are comma-separated; memory operands "[a + b]"
        # never contain a top-level comma, so a plain split is safe.
        parts = [p.strip() for p in op_str.split(",")] if op_str else []
        if len(parts) >= 2:
            # 2-operand:  imul dest, src        -> dest = dest * src
            # 3-operand:  imul dest, src, imm   -> dest = src  * imm
            # The destination is always a 16-bit register; the result is
            # truncated to it and CF=OF set when the full product doesn't fit
            # the signed destination. (No implicit DX:AX for these forms.)
            dest = self._rewrite_operands(insn, [parts[0]])[0]
            if len(parts) >= 3:
                fa = self._rewrite_operands(insn, [parts[1]])[0]
                fb = self._rewrite_operands(insn, [parts[2]])[0]
            else:
                fa = dest
                fb = self._rewrite_operands(insn, [parts[1]])[0]
            dest_reg = parts[0].strip().lower()
            self._invalidate_register(dest_reg)
            lines.append("{")
            lines.append(
                f"    int32_t tmp = (int16_t)({fa}) * (int16_t)({fb});"
            )
            lines.append(f"    {dest} = tmp & 0xFFFF;")
            lines.append("    CF = OF = (tmp != (int32_t)(int16_t)tmp);")
            lines.append("}")
            return lines
        operand = self._rewrite_operands(insn, [op_str])[0]
        width = _operand_width_from_str(operand)
        if width == 8 and "dx" not in regs_write:
            self._invalidate_register("ax")
            lines.append("{")
            lines.append(f"    int16_t tmp = (int8_t)al * (int8_t){operand};")
            lines.append("    ax = (uint16_t)tmp & 0xFFFF;")
            lines.append(
                "    CF = OF = (tmp < -128) || (tmp > 127);"
            )
            lines.append("}")
        else:
            self._invalidate_register("dx")
            self._invalidate_register("ax")
            lines.append("{")
            lines.append(f"    int32_t tmp = (int16_t)ax * (int16_t){operand};")
            lines.append("    dx = (tmp >> 16) & 0xFFFF;")
            lines.append("    ax = tmp & 0xFFFF;")
            lines.append("    CF = OF = tmp != (int32_t)(int16_t)ax;")
            lines.append("}")
        return lines

    def handle_div(self, insn: dict, known_funcs: Set[int]) -> List[str]:
        lines: List[str] = []
        self.flush_pending_dos(lines)
        op_str = _decode_variables(insn.get("op_str", "")).strip()
        operand = self._rewrite_operands(insn, [op_str])[0]
        # ``div`` always clobbers AX and may update DX for 16-bit operands
        self._invalidate_register("dx")
        self._invalidate_register("ax")
        regs_write = {
            reg.lower() for reg in insn.get("detail", {}).get("regs_write", [])
        }
        if "dx" in regs_write:
            lines.append("{")
            lines.append("    uint32_t tmp = ((uint32_t)dx << 16) | ax;")
            lines.append(f"    uint16_t divisor = {operand};")
            lines.append("    ax = (tmp / divisor) & 0xFFFF;")
            lines.append("    dx = (tmp % divisor) & 0xFFFF;")
            lines.append("}")
        else:
            lines.append("{")
            lines.append("    uint16_t tmp = ax;")
            lines.append(f"    uint8_t divisor = {operand};")
            lines.append("    al = (tmp / divisor) & 0xFF;")
            lines.append("    ah = (tmp % divisor) & 0xFF;")
            lines.append("}")
        return lines

    def handle_idiv(self, insn: dict, known_funcs: Set[int]) -> List[str]:
        lines: List[str] = []
        self.flush_pending_dos(lines)
        op_str = _decode_variables(insn.get("op_str", "")).strip()
        operand = self._rewrite_operands(insn, [op_str])[0]
        regs_write = {
            reg.lower() for reg in insn.get("detail", {}).get("regs_write", [])
        }
        if "dx" in regs_write:
            self._invalidate_register("dx")
        if "ax" in regs_write or "dx" in regs_write:
            self._invalidate_register("ax")
        if "dx" in regs_write:
            lines.append("{")
            lines.append("    int32_t dividend = ((int32_t)(int16_t)dx << 16) | ax;")
            lines.append(f"    int16_t divisor = (int16_t){operand};")
            lines.append("    ax = (uint16_t)(dividend / divisor);")
            lines.append("    dx = (uint16_t)(dividend % divisor);")
            lines.append("}")
        else:
            lines.append("{")
            lines.append("    int16_t dividend = (int16_t)ax;")
            lines.append(f"    int8_t divisor = (int8_t){operand};")
            lines.append("    al = (uint8_t)((dividend / divisor) & 0xFF);")
            lines.append("    ah = (uint8_t)((dividend % divisor) & 0xFF);")
            lines.append("}")
        return lines

    def handle_not(
        self, insn: dict, known_funcs: Set[int]
    ) -> List[str] | None:
        lines: List[str] = []
        self.flush_pending_dos(lines)
        op_str = _decode_variables(insn.get("op_str", "")).strip()
        operand = self._rewrite_operands(insn, [op_str])[0]
        operand_rcb = _match_rcb_access(operand)
        self._invalidate_register(operand)
        if operand_rcb:
            size, field = operand_rcb
            read = "rcb_read16" if size == "w" else "rcb_read8"
            write = "rcb_write16" if size == "w" else "rcb_write8"
            mask = "0xFFFF" if size == "w" else "0xFF"
            lines.append(f"{write}({field}, (~{read}({field})) & {mask});")
            lines.append(f"ZF = {read}({field}) == 0;")
        elif operand.startswith("memw(") or operand.startswith("memb("):
            inner = operand[5:-1]
            acc = "memw" if operand.startswith("memw(") else "memb"
            writer = acc + "_write"
            mask = "0xFFFF" if acc == "memw" else "0xFF"
            lines.append(f"{writer}({inner}, (~{acc}({inner})) & {mask});")
            lines.append(f"ZF = {acc}({inner}) == 0;")
        else:
            byte_regs = {"al", "ah", "bl", "bh", "cl", "ch", "dl", "dh"}
            mask = "0xFF" if operand.lower() in byte_regs else "0xFFFF"
            lines.append(f"{operand} = (~{operand}) & {mask};")
            lines.append(f"ZF = {operand} == 0;")
        return lines

    def handle_neg(
        self, insn: dict, known_funcs: Set[int]
    ) -> List[str] | None:
        lines: List[str] = []
        self.flush_pending_dos(lines)
        op_str = _decode_variables(insn.get("op_str", "")).strip()
        operand = self._rewrite_operands(insn, [op_str])[0]
        self._invalidate_register(operand)
        byte_regs = {"al", "ah", "bl", "bh", "cl", "ch", "dl", "dh"}
        is_mem = operand.startswith("memw(") or operand.startswith("memb(")
        is_byte = operand.startswith("memb(") or operand.lower() in byte_regs
        ctype = "uint8_t" if is_byte else "uint16_t"
        shift = 7 if is_byte else 15
        sign_bit = "0x80" if is_byte else "0x8000"
        mask = "0xFF" if is_byte else "0xFFFF"
        if is_mem:
            inner = operand[5:-1]
            acc = "memb" if operand.startswith("memb(") else "memw"
            writer = acc + "_write"
            lines.append("{")
            lines.append(f"    {ctype} tmp = {acc}({inner});")
            lines.append(f"    {writer}({inner}, (0 - tmp) & {mask});")
            lines.append("    CF = tmp != 0;")
            lines.append(f"    ZF = {acc}({inner}) == 0;")
            lines.append(f"    PF = parity8((uint8_t){acc}({inner}));")
            lines.append(f"    SF = ({acc}({inner}) >> {shift}) & 1;")
            lines.append(f"    OF = tmp == {sign_bit};")
            lines.append("}")
        else:
            lines.append("{")
            lines.append(f"    {ctype} tmp = {operand};")
            lines.append(f"    {operand} = (0 - tmp) & {mask};")
            lines.append("    CF = tmp != 0;")
            lines.append(f"    ZF = {operand} == 0;")
            lines.append(f"    PF = parity8((uint8_t){operand});")
            lines.append(f"    SF = ({operand} >> {shift}) & 1;")
            lines.append(f"    OF = tmp == {sign_bit};")
            lines.append("}")
        return lines

    def handle_aaa(self, insn: dict, known_funcs: Set[int]) -> List[str]:
        lines: List[str] = []
        self.flush_pending_dos(lines)
        self._invalidate_register("ax")
        lines.append("{")
        lines.append("    uint8_t tmp = al;")
        # AAA also adjusts when AF=1, but AF is currently not modelled.
        # Avoid approximating AF by checking AL bit 4, which is not equivalent
        # and can trigger incorrect decimal adjustments.
        lines.append("    if ((tmp & 0x0F) > 9) {")
        lines.append("        al = (uint8_t)((tmp + 6) & 0x0F);")
        lines.append("        ah = (uint8_t)((ah + 1) & 0xFF);")
        lines.append("        CF = 1;")
        lines.append("    } else {")
        lines.append("        al = tmp & 0x0F;")
        lines.append("        CF = 0;")
        lines.append("    }")
        lines.append("}")
        return lines

    def handle_aas(self, insn: dict, known_funcs: Set[int]) -> List[str]:
        # ASCII Adjust AL After Subtraction — the subtraction counterpart of
        # AAA. Modelled the same way (AF not tracked, so use the low-nibble>9
        # test, matching handle_aaa).
        lines: List[str] = []
        self.flush_pending_dos(lines)
        self._invalidate_register("ax")
        lines.append("{")
        lines.append("    uint8_t tmp = al;")
        lines.append("    if ((tmp & 0x0F) > 9) {")
        lines.append("        al = (uint8_t)((tmp - 6) & 0x0F);")
        lines.append("        ah = (uint8_t)((ah - 1) & 0xFF);")
        lines.append("        CF = 1;")
        lines.append("    } else {")
        lines.append("        al = tmp & 0x0F;")
        lines.append("        CF = 0;")
        lines.append("    }")
        lines.append("}")
        return lines

    def handle_daa(self, insn: dict, known_funcs: Set[int]) -> List[str]:
        # Decimal Adjust AL After Addition. Packed-BCD correction on the FULL
        # AL byte (both nibbles), unlike AAA which only touches the low nibble.
        # AF is not modelled, so the low-nibble adjust keys off (AL & 0x0F) > 9
        # (the same approximation as handle_aaa); the high-nibble adjust keys
        # off the modelled CF plus the post-low-adjust AL > 0x99 test. This is
        # the canonical DAA algorithm; flags ZF/SF/PF are set from the result.
        lines: List[str] = []
        self.flush_pending_dos(lines)
        self._invalidate_register("ax")
        lines.append("{")
        lines.append("    uint8_t old_al = al;")
        lines.append("    uint8_t old_cf = (uint8_t)CF;")
        lines.append("    uint8_t new_cf = 0;")
        lines.append("    if (((old_al & 0x0F) > 9)) {")
        lines.append("        al = (uint8_t)(al + 6);")
        lines.append("    }")
        lines.append("    if (old_cf || old_al > 0x99) {")
        lines.append("        al = (uint8_t)(al + 0x60);")
        lines.append("        new_cf = 1;")
        lines.append("    }")
        lines.append("    CF = new_cf;")
        lines.append("    ZF = (al == 0);")
        lines.append("    SF = (al >> 7) & 1;")
        lines.append("    PF = parity8(al);")
        lines.append("}")
        return lines

    def handle_das(self, insn: dict, known_funcs: Set[int]) -> List[str]:
        # Decimal Adjust AL After Subtraction — the subtraction counterpart of
        # DAA. Same structure; AF unmodelled so low-nibble adjust keys off
        # (AL & 0x0F) > 9, high-nibble off CF / AL > 0x99.
        lines: List[str] = []
        self.flush_pending_dos(lines)
        self._invalidate_register("ax")
        lines.append("{")
        lines.append("    uint8_t old_al = al;")
        lines.append("    uint8_t old_cf = (uint8_t)CF;")
        lines.append("    uint8_t new_cf = 0;")
        lines.append("    if (((old_al & 0x0F) > 9)) {")
        lines.append("        al = (uint8_t)(al - 6);")
        lines.append("    }")
        lines.append("    if (old_cf || old_al > 0x99) {")
        lines.append("        al = (uint8_t)(al - 0x60);")
        lines.append("        new_cf = 1;")
        lines.append("    }")
        lines.append("    CF = new_cf;")
        lines.append("    ZF = (al == 0);")
        lines.append("    SF = (al >> 7) & 1;")
        lines.append("    PF = parity8(al);")
        lines.append("}")
        return lines

    def handle_cmp(
        self, insn: dict, known_funcs: Set[int]
    ) -> List[str]:
        lines: List[str] = []
        self.flush_pending_dos(lines)
        op_str = _decode_variables(insn.get("op_str", ""))
        parts = [p.strip() for p in op_str.split(",", 1)]
        if len(parts) == 2:
            left, right = self._rewrite_operands(insn, parts)
            left = _fix_negative_imm(left, right)
            right = _fix_negative_imm(right, left)
            byte_regs = {"al", "ah", "bl", "bh", "cl", "ch", "dl", "dh"}
            left_rcb = _match_rcb_access(left)
            if left_rcb:
                is_byte = left_rcb[0] == "b"
            else:
                is_byte = left.startswith("memb(") or left.lower() in byte_regs
            ctype = "uint8_t" if is_byte else "uint16_t"
            sign_bit = "0x80" if is_byte else "0x8000"
            shift = 7 if is_byte else 15
            lines.append("{")
            lines.append(f"    uint32_t left_val = {left};")
            lines.append(f"    uint32_t right_val = {right};")
            lines.append("    CF = left_val < right_val;")
            lines.append("    uint32_t tmp = left_val - right_val;")
            lines.append(f"    {ctype} result = ({ctype})tmp;")
            lines.append("    ZF = result == 0;")
            lines.append(f"    SF = (result >> {shift}) & 1;")
            lines.append("    PF = parity8((uint8_t)result);")
            lines.append(
                "    OF = ((left_val ^ right_val) & (left_val ^ result) & "
                f"{sign_bit}) != 0;"
            )
            lines.append("}")
        else:
            asm = f"{insn['mnemonic']} {op_str}".strip()
            self._emit_unsupported_abort(lines, insn, asm)
        return lines

    def handle_test(
        self, insn: dict, known_funcs: Set[int]
    ) -> List[str]:
        lines: List[str] = []
        self.flush_pending_dos(lines)
        op_str = _decode_variables(insn.get("op_str", ""))
        parts = [p.strip() for p in op_str.split(",", 1)]
        if len(parts) == 2:
            left, right = self._rewrite_operands(insn, parts)
            left = _fix_negative_imm(left, right)
            right = _fix_negative_imm(right, left)
            byte_regs = {"al", "ah", "bl", "bh", "cl", "ch", "dl", "dh"}
            is_byte = left.startswith("memb(") or left.lower() in byte_regs
            ctype = "uint8_t" if is_byte else "uint16_t"
            shift = 7 if is_byte else 15
            lines.append("{")
            lines.append(f"    uint32_t left_val = {left};")
            lines.append(f"    uint32_t right_val = {right};")
            lines.append(
                f"    {ctype} tmp = ({ctype})(left_val & right_val);")
            lines.append("    CF = 0;")
            lines.append("    OF = 0;")
            lines.append("    ZF = tmp == 0;")
            lines.append(f"    SF = (tmp >> {shift}) & 1;")
            lines.append("    PF = parity8((uint8_t)tmp);")
            lines.append("}")
        else:
            asm = f"{insn['mnemonic']} {op_str}".strip()
            self._emit_unsupported_abort(lines, insn, asm)
        return lines

    def handle_shift(
        self, insn: dict, known_funcs: Set[int]
    ) -> List[str] | None:
        lines: List[str] = []
        self.flush_pending_dos(lines)
        op_str = _decode_variables(insn.get("op_str", ""))
        parts = [p.strip() for p in op_str.split(",", 1)]
        if len(parts) == 2:
            dest, src = self._rewrite_operands(insn, parts)
            reg = dest.lower()
            mem_access = _split_mem_accessor(dest)
            width = 16
            if mem_access:
                width = 8 if mem_access[0] == "memb" else 16
            elif reg in {
                "al",
                "ah",
                "bl",
                "bh",
                "cl",
                "ch",
                "dl",
                "dh",
            }:
                width = 8
            mask = "0xFF" if width == 8 else "0xFFFF"
            shift = width - 1
            ctype = "uint8_t" if width == 8 else "uint16_t"
            dest_value = _value_expr(dest)
            self._invalidate_register(dest)
            lines.append("{")
            lines.append(f"    unsigned count = {src} & 0x1F;")
            lines.append("    if (count) {")
            lines.append("        unsigned orig_count = count;")
            if insn["mnemonic"] == "sar":
                ctype_u = "uint8_t" if width == 8 else "uint16_t"
                ctype_s = "int8_t" if width == 8 else "int16_t"
                lines.append(f"        {ctype_s} tmp = ({ctype_s}){dest_value};")
                lines.append("        while (count--) {")
                lines.append("            CF = tmp & 1;")
                lines.append("            tmp >>= 1;")
                lines.append("        }")
                _emit_store(lines, dest, f"({ctype_u})tmp", indent="        ")
                lines.append("        OF = 0;")
            elif insn["mnemonic"] == "shr":
                lines.append(f"        {ctype} tmp = {dest_value};")
                lines.append(
                    f"        unsigned orig_sign = (tmp >> {shift}) & 1;",
                )
                lines.append("        while (count--) {")
                lines.append("            CF = tmp & 1;")
                lines.append("            tmp >>= 1;")
                lines.append("        }")
                _emit_store(lines, dest, "tmp", indent="        ")
                lines.append("        OF = (orig_count == 1) ? orig_sign : 0;")
            else:
                lines.append(f"        {ctype} tmp = {dest_value};")
                lines.append("        while (count--) {")
                lines.append(f"            CF = (tmp >> {shift}) & 1;")
                lines.append(
                    f"            tmp = ({ctype})((tmp << 1) & {mask});",
                )
                lines.append("        }")
                _emit_store(lines, dest, "tmp", indent="        ")
                lines.append(
                    f"        OF = (orig_count == 1) ? "
                    f"(CF ^ ((tmp >> {shift}) & 1)) : 0;",
                )
            result_expr = _value_expr(dest)
            lines.append(f"        ZF = {result_expr} == 0;")
            lines.append(f"        PF = parity8((uint8_t){result_expr});")
            lines.append(f"        SF = ({result_expr} >> {shift}) & 1;")
            lines.append("    }")
            lines.append("}")
            return lines
        asm = f"{insn['mnemonic']} {op_str}".strip()
        self._emit_unsupported_abort(lines, insn, asm)
        return lines

    def handle_repne_scasb(
        self, insn: dict, known_funcs: Set[int]
    ) -> List[str] | None:
        lines: List[str] = []
        self.flush_pending_dos(lines)
        op_str = _decode_variables(insn.get("op_str", ""))
        if op_str.startswith("scasb"):
            lines.append("{")
            lines.append("int delta = DF ? -1 : 1;")
            lines.append("uint16_t count = cx;")
            lines.append("uint8_t last_byte = 0;")
            lines.append(
                "uint16_t index = scanMemoryForAl("
                "(const uint8_t *)seg_off(es, di), "
                "al, count, delta, &last_byte);"
            )
            lines.append(
                "uint16_t advance = (index < count) ? index + 1 : index;"
            )
            lines.append("if (advance > 0) {")
            lines.append("    uint32_t left_val = al;")
            lines.append("    uint32_t right_val = last_byte;")
            lines.append("    uint32_t tmp = left_val - right_val;")
            lines.append("    uint8_t result = (uint8_t)tmp;")
            lines.append("    CF = left_val < right_val;")
            lines.append("    ZF = result == 0;")
            lines.append("    PF = parity8(result);")
            lines.append("    SF = (result >> 7) & 1;")
            lines.append(
                "    OF = ((left_val ^ right_val) & "
                "(left_val ^ result) & 0x80) != 0;"
            )
            lines.append("}")
            lines.append("cx = (count - advance) & 0xFFFF;")
            lines.append("di = (di + advance * delta) & 0xFFFF;")
            lines.append("}")
            return lines
        asm = f"{insn['mnemonic']} {op_str}".strip()
        self._emit_unsupported_abort(lines, insn, asm)
        return lines

    def handle_rotate(
        self, insn: dict, known_funcs: Set[int]
    ) -> List[str] | None:
        lines: List[str] = []
        self.flush_pending_dos(lines)
        op_str = _decode_variables(insn.get("op_str", ""))
        parts = [p.strip() for p in op_str.split(",", 1)]
        if len(parts) == 2:
            dest, src = self._rewrite_operands(insn, parts)
            reg = dest.lower()
            self._invalidate_register(dest)
            width = 16
            if dest.startswith("memb(") or reg in {
                "al",
                "ah",
                "bl",
                "bh",
                "cl",
                "ch",
                "dl",
                "dh",
            }:
                width = 8
            is_mem = dest.startswith("memw(") or dest.startswith("memb(")
            if is_mem:
                inner = dest[5:-1]
                acc = "memw" if dest.startswith("memw(") else "memb"
                writer = acc + "_write"
            lines.append("{")
            lines.append(f"    unsigned count = {src} & {width - 1};")
            lines.append("    if (count) {")
            if is_mem:
                lines.append(
                    f"        {writer}({inner}, ({acc}({inner}) << count) | "
                    f"({acc}({inner}) >> ({width} - count)));"
                )
                lines.append(f"        CF = {acc}({inner}) & 1;")
                lines.append("        if (count == 1) {")
                lines.append(
                    f"            OF = (({acc}({inner}) >> {width - 1}) & 1) ^ CF;"
                )
                lines.append("        } else {")
                lines.append("            OF = 0;")
                lines.append("        }")
            else:
                lines.append(
                    f"        {dest} = ({dest} << count) | "
                    f"({dest} >> ({width} - count));"
                )
                lines.append(f"        CF = {dest} & 1;")
                lines.append("        if (count == 1) {")
                lines.append(
                    f"            OF = (({dest} >> {width - 1}) & 1) ^ CF;"
                )
                lines.append("        } else {")
                lines.append("            OF = 0;")
                lines.append("        }")
            lines.append("    }")
            lines.append("}")
            return lines
        asm = f"{insn['mnemonic']} {op_str}".strip()
        self._emit_unsupported_abort(lines, insn, asm)
        return lines

    def handle_ror(
        self, insn: dict, known_funcs: Set[int]
    ) -> List[str] | None:
        lines: List[str] = []
        self.flush_pending_dos(lines)
        op_str = _decode_variables(insn.get("op_str", ""))
        parts = [p.strip() for p in op_str.split(",", 1)]
        if len(parts) == 2:
            dest, src = self._rewrite_operands(insn, parts)
            reg = dest.lower()
            self._invalidate_register(dest)
            width = 16
            if dest.startswith("memb(") or reg in {
                "al",
                "ah",
                "bl",
                "bh",
                "cl",
                "ch",
                "dl",
                "dh",
            }:
                width = 8
            is_mem = dest.startswith("memw(") or dest.startswith("memb(")
            if is_mem:
                inner = dest[5:-1]
                acc = "memw" if dest.startswith("memw(") else "memb"
                writer = acc + "_write"
            lines.append("{")
            lines.append(f"    unsigned count = {src} & {width - 1};")
            lines.append("    if (count) {")
            if is_mem:
                lines.append(
                    f"        {writer}({inner}, ({acc}({inner}) >> count) | "
                    f"({acc}({inner}) << ({width} - count)));"
                )
                lines.append(f"        CF = {acc}({inner}) >> {width - 1};")
                lines.append("        if (count == 1) {")
                lines.append(
                    f"            OF = (({acc}({inner}) >> {width - 1}) & 1) "
                    f"^ (({acc}({inner}) >> {width - 2}) & 1);"
                )
                lines.append("        } else {")
                lines.append("            OF = 0;")
                lines.append("        }")
            else:
                lines.append(
                    f"        {dest} = ({dest} >> count) | "
                    f"({dest} << ({width} - count));"
                )
                lines.append(f"        CF = {dest} >> {width - 1};")
                lines.append("        if (count == 1) {")
                lines.append(
                    f"            OF = (({dest} >> {width - 1}) & 1) "
                    f"^ (({dest} >> {width - 2}) & 1);"
                )
                lines.append("        } else {")
                lines.append("            OF = 0;")
                lines.append("        }")
            lines.append("    }")
            lines.append("}")
            return lines
        asm = f"{insn['mnemonic']} {op_str}".strip()
        self._emit_unsupported_abort(lines, insn, asm)
        return lines

    def handle_rcl(
        self, insn: dict, known_funcs: Set[int]
    ) -> List[str] | None:
        lines: List[str] = []
        self.flush_pending_dos(lines)
        op_str = _decode_variables(insn.get("op_str", ""))
        parts = [p.strip() for p in op_str.split(",", 1)]
        dest = self._rewrite_operands(insn, [parts[0]])[0]
        count = parts[1] if len(parts) == 2 else "1"
        self._invalidate_register(dest)
        byte_regs = {"al", "ah", "bl", "bh", "cl", "ch", "dl", "dh"}
        is_mem = dest.startswith("memw(") or dest.startswith("memb(")
        is_byte = dest.startswith("memb(") or dest.lower() in byte_regs
        width = 8 if is_byte else 16
        mask = "0xFF" if is_byte else "0xFFFF"
        if is_mem:
            inner = dest[5:-1]
            acc = "memb" if dest.startswith("memb(") else "memw"
            writer = acc + "_write"
            ctype = "uint8_t" if acc == "memb" else "uint16_t"
            lines.append("{")
            lines.append(f"    unsigned count = ({count} & 0x1F) % {width + 1};")
            lines.append("    unsigned orig_count = count;")
            lines.append(f"    {ctype} tmp = {acc}({inner});")
            lines.append("    while (count--) {")
            lines.append(
                f"        unsigned new_cf = (tmp >> {width - 1}) & 1;"
            )
            lines.append(
                f"        tmp = ((tmp << 1) | CF) & {mask};"
            )
            lines.append("        CF = new_cf;")
            lines.append("    }")
            lines.append(f"    {writer}({inner}, tmp & {mask});")
            lines.append("    if (orig_count == 1) {")
            lines.append(
                f"        OF = ((tmp >> {width - 1}) & 1) ^ CF;",
            )
            lines.append("    }")
            lines.append("}")
        else:
            lines.append("{")
            lines.append(f"    unsigned count = ({count} & 0x1F) % {width + 1};")
            lines.append("    unsigned orig_count = count;")
            lines.append("    while (count--) {")
            lines.append(
                f"        unsigned new_cf = ({dest} >> {width - 1}) & 1;"
            )
            lines.append(
                f"        {dest} = (({dest} << 1) | CF) & {mask};"
            )
            lines.append("        CF = new_cf;")
            lines.append("    }")
            lines.append("    if (orig_count == 1) {")
            lines.append(
                f"        OF = (({dest} >> {width - 1}) & 1) ^ CF;",
            )
            lines.append("    }")
            lines.append("}")
        return lines

    def handle_rcr(
        self, insn: dict, known_funcs: Set[int]
    ) -> List[str] | None:
        lines: List[str] = []
        self.flush_pending_dos(lines)
        op_str = _decode_variables(insn.get("op_str", ""))
        parts = [p.strip() for p in op_str.split(",", 1)]
        dest = self._rewrite_operands(insn, [parts[0]])[0]
        count = parts[1] if len(parts) == 2 else "1"
        self._invalidate_register(dest)
        byte_regs = {"al", "ah", "bl", "bh", "cl", "ch", "dl", "dh"}
        is_mem = dest.startswith("memw(") or dest.startswith("memb(")
        is_byte = dest.startswith("memb(") or dest.lower() in byte_regs
        width = 8 if is_byte else 16
        mask = "0xFF" if is_byte else "0xFFFF"
        if is_mem:
            inner = dest[5:-1]
            acc = "memb" if dest.startswith("memb(") else "memw"
            writer = acc + "_write"
            ctype = "uint8_t" if acc == "memb" else "uint16_t"
            lines.append("{")
            lines.append(f"    unsigned count = ({count} & 0x1F) % {width + 1};")
            lines.append("    unsigned orig_count = count;")
            lines.append(f"    {ctype} tmp = {acc}({inner});")
            lines.append("    while (count--) {")
            lines.append("        unsigned new_cf = tmp & 1;")
            lines.append(
                f"        tmp = ((tmp >> 1) | (CF << {width - 1})) & {mask};"
            )
            lines.append("        CF = new_cf;")
            lines.append("    }")
            lines.append(f"    {writer}({inner}, tmp & {mask});")
            lines.append("    if (orig_count == 1) {")
            lines.append(
                f"        OF = ((tmp >> {width - 1}) & 1) ^ ((tmp >> {width - 2}) & 1);"
            )
            lines.append("    }")
            lines.append("}")
        else:
            lines.append("{")
            lines.append(f"    unsigned count = ({count} & 0x1F) % {width + 1};")
            lines.append("    unsigned orig_count = count;")
            lines.append("    while (count--) {")
            lines.append(f"        unsigned new_cf = {dest} & 1;")
            lines.append(
                f"        {dest} = (({dest} >> 1) | (CF << {width - 1})) & {mask};"
            )
            lines.append("        CF = new_cf;")
            lines.append("    }")
            lines.append("    if (orig_count == 1) {")
            lines.append(
                f"        OF = (({dest} >> {width - 1}) & 1) ^ (({dest} >> {width - 2}) & 1);"
            )
            lines.append("    }")
            lines.append("}")
        return lines

    def handle_bound(self, insn: dict, known_funcs: Set[int]) -> List[str]:
        lines: List[str] = []
        self.flush_pending_dos(lines)
        op_str = _decode_variables(insn.get("op_str", ""))
        parts = [p.strip() for p in op_str.split(",", 1)]
        if len(parts) != 2:
            self._emit_unsupported_abort(lines, insn, f"bound {op_str}")
            return lines
        dest = self._rewrite_operands(insn, [parts[0]])[0]
        match = re.match(r"dword ptr \[(.+)\]$", parts[1], re.IGNORECASE)
        if not match:
            self._emit_unsupported_abort(lines, insn, f"bound {op_str}")
            return lines
        inner = match.group(1).strip()
        lower_access = _rewrite_mem_op(f"word ptr [{inner}]")
        mem_access = _split_mem_accessor(lower_access)
        if not mem_access:
            self._emit_unsupported_abort(lines, insn, f"bound {op_str}")
            return lines
        helper, inner_args = mem_access
        try:
            seg_expr, addr_expr = inner_args.split(",", 1)
        except ValueError:
            self._emit_unsupported_abort(lines, insn, f"bound {op_str}")
            return lines
        seg_expr = seg_expr.strip()
        addr_expr = addr_expr.strip()
        upper_inner = f"{seg_expr}, ((({addr_expr}) + 2) & 0xFFFF)"
        lines.append("{")
        lines.append(f"    uint16_t lower = {helper}({inner_args});")
        lines.append(f"    uint16_t upper = {helper}({upper_inner});")
        lines.append(f"    uint16_t value = {dest};")
        lines.append("    if (value < lower || value > upper) {")
        lines.append(
            "        shim_log_crash(\"[BOUND] %04X not in [%04X, %04X]\\n\","
            " value, lower, upper);"
        )
        lines.append("        abort();")
        lines.append("    }")
        lines.append("}")
        return lines

    def handle_loop(
        self, insn: dict, known_funcs: Set[int]
    ) -> List[str] | None:
        lines: List[str] = []
        self.flush_pending_dos(lines)
        self._invalidate_register("cx")
        target = self.parse_imm(insn.get("op_str", ""))
        mnem = insn["mnemonic"]
        lines.append("cx--;")
        cond = "cx != 0"
        if mnem in {"loopne", "loopnz"}:
            cond += " && ZF == 0"
        elif mnem in {"loope", "loopz"}:
            cond += " && ZF == 1"
        if target is not None:
            block = self.current_blocks.get(target)
            if (
                block
                and len(block.instructions) == 1
                and block.instructions[0]["mnemonic"] in {
                    "ret", "retn", "retf"}
            ):
                lines.append(f"if ({cond}) return;")
            elif target in self.current_blocks:
                lines.append(f"if ({cond}) {{")
                lines.append(f"    pc = 0x{target:04X};")
                lines.append("    continue;")
                lines.append("}")
            elif target in known_funcs:
                lines.append(f"if ({cond}) {{")
                lines.append(f"    pc = 0x{target:04X};")
                lines.append("    continue;")
                lines.append("}")
            else:
                lines.append(f"if ({cond}) {{")
                lines.append(f"    pc = 0x{target:04X};")
                lines.append("    continue;")
                lines.append("}")
        else:
            op_str = insn.get("op_str", "")
            asm = f"{mnem} {op_str}".strip()
            self._emit_unsupported_abort(lines, insn, asm)
        return lines

    def handle_lahf(self, insn: dict, known_funcs: Set[int]) -> List[str]:
        lines: List[str] = []
        self.flush_pending_dos(lines)
        self.last_ah = None
        lines.append("ah = (uint8_t)((SF << 7) | (ZF << 6) | (PF << 2) | 0x02 | CF);")
        return lines

    def _aad_aam_base(self, insn: dict) -> int:
        # AAD/AAM carry an immediate base (default 10 when the encoding omits it).
        op = _decode_variables(insn.get("op_str", "")).strip()
        base = _parse_imm(op) if op else None
        return 10 if base is None else base

    def handle_aam(self, insn: dict, known_funcs: Set[int]) -> List[str]:
        # AAM (0xD4 ib): AH = AL / base; AL = AL % base. SF/ZF/PF from AL.
        base = self._aad_aam_base(insn)
        lines: List[str] = []
        self.flush_pending_dos(lines)
        self.last_al = None
        self.last_ah = None
        if base != 0:
            lines.append(f"ah = (uint8_t)(al / {base}); al = (uint8_t)(al % {base});")
        lines.append("ZF = al == 0; SF = (al >> 7) & 1; PF = parity8(al);")
        return lines

    def handle_aad(self, insn: dict, known_funcs: Set[int]) -> List[str]:
        # AAD (0xD5 ib): AL = (AL + AH*base) & 0xFF; AH = 0. SF/ZF/PF from AL.
        base = self._aad_aam_base(insn)
        lines: List[str] = []
        self.flush_pending_dos(lines)
        self.last_al = None
        self.last_ah = None
        lines.append(f"al = (uint8_t)((al + ah * {base}) & 0xFF); ah = 0;")
        lines.append("ZF = al == 0; SF = (al >> 7) & 1; PF = parity8(al);")
        return lines

    def handle_salc(self, insn: dict, known_funcs: Set[int]) -> List[str]:
        # SALC (0xD6, undocumented): AL = CF ? 0xFF : 0x00. Reads CF, no flag
        # effect.
        lines: List[str] = []
        self.flush_pending_dos(lines)
        self.last_al = None
        lines.append("al = CF ? 0xFF : 0x00;")
        return lines

    def handle_sahf(self, insn: dict, known_funcs: Set[int]) -> List[str]:
        lines: List[str] = []
        self.flush_pending_dos(lines)
        lines.append("SF = (ah >> 7) & 1;")
        lines.append("ZF = (ah >> 6) & 1;")
        lines.append("PF = (ah >> 2) & 1;")
        lines.append("CF = ah & 1;")
        return lines

    def handle_cwde(self, insn: dict, known_funcs: Set[int]) -> List[str]:
        lines: List[str] = []
        self.flush_pending_dos(lines)
        lines.append("ax = ((int8_t)al) & 0xFFFF;")
        return lines

    def handle_cdq(self, insn: dict, known_funcs: Set[int]) -> List[str]:
        lines: List[str] = []
        self.flush_pending_dos(lines)
        self._invalidate_register("dx")
        lines.append("dx = (ax & 0x8000) ? 0xFFFF : 0;")
        return lines

    def handle_cld(self, insn: dict, known_funcs: Set[int]) -> List[str]:
        lines: List[str] = []
        self.flush_pending_dos(lines)
        lines.append("DF = 0;")
        return lines

    def handle_std(self, insn: dict, known_funcs: Set[int]) -> List[str]:
        lines: List[str] = []
        self.flush_pending_dos(lines)
        lines.append("DF = 1;")
        return lines

    def handle_clc(self, insn: dict, known_funcs: Set[int]) -> List[str]:
        lines: List[str] = []
        self.flush_pending_dos(lines)
        lines.append("CF = 0;")
        return lines

    def handle_cmc(self, insn: dict, known_funcs: Set[int]) -> List[str]:
        lines: List[str] = []
        self.flush_pending_dos(lines)
        lines.append("CF ^= 1;")
        return lines

    def handle_stc(self, insn: dict, known_funcs: Set[int]) -> List[str]:
        lines: List[str] = []
        self.flush_pending_dos(lines)
        lines.append("CF = 1;")
        return lines

    def handle_cli(self, insn: dict, known_funcs: Set[int]) -> List[str]:
        lines: List[str] = []
        self.flush_pending_dos(lines)
        lines.append("IF = 0;")
        return lines

    def handle_sti(self, insn: dict, known_funcs: Set[int]) -> List[str]:
        lines: List[str] = []
        self.flush_pending_dos(lines)
        lines.append("IF = 1;")
        lines.append("interrupt_shadow = 1;")
        return lines

    def handle_lodsb(
        self, insn: dict, known_funcs: Set[int]
    ) -> List[str] | None:
        lines: List[str] = []
        self.flush_pending_dos(lines)
        src_seg = self._string_source_segment(insn)
        lines.append("{")
        lines.append("int delta = DF ? -1 : 1;")
        lines.append(f"al = memb({src_seg}, si);")
        lines.append("si = (si + delta) & 0xFFFF;")
        lines.append("}")
        return lines

    def handle_lodsw(
        self, insn: dict, known_funcs: Set[int]
    ) -> List[str] | None:
        lines: List[str] = []
        self.flush_pending_dos(lines)
        src_seg = self._string_source_segment(insn)
        lines.append("{")
        lines.append("int delta = DF ? -2 : 2;")
        lines.append(f"ax = memw({src_seg}, si);")
        lines.append("si = (si + delta) & 0xFFFF;")
        lines.append("}")
        return lines

    def handle_xlatb(
        self, insn: dict, known_funcs: Set[int]
    ) -> List[str] | None:
        lines: List[str] = []
        self.flush_pending_dos(lines)
        seg = "ds"
        detail = insn.get("detail")
        if isinstance(detail, dict):
            seg_override = detail.get("seg_override")
            if isinstance(seg_override, str) and seg_override:
                seg = seg_override.lower()
        lines.append(f"al = memb({seg}, (bx + al) & 0xFFFF);")
        return lines

    def handle_movsb(
        self, insn: dict, known_funcs: Set[int]
    ) -> List[str] | None:
        lines: List[str] = []
        self.flush_pending_dos(lines)
        src_seg = self._string_source_segment(insn)
        lines.append("{")
        lines.append("int delta = DF ? -1 : 1;")
        lines.append(f"memb_write(es, di, memb({src_seg}, si));")
        lines.append("si = (si + delta) & 0xFFFF;")
        lines.append("di = (di + delta) & 0xFFFF;")
        lines.append("}")
        return lines

    def handle_movsw(
        self, insn: dict, known_funcs: Set[int]
    ) -> List[str] | None:
        lines: List[str] = []
        self.flush_pending_dos(lines)
        src_seg = self._string_source_segment(insn)
        lines.append("{")
        lines.append("int delta = DF ? -2 : 2;")
        lines.append(f"memw_write(es, di, memw({src_seg}, si));")
        lines.append("si = (si + delta) & 0xFFFF;")
        lines.append("di = (di + delta) & 0xFFFF;")
        lines.append("}")
        return lines

    def handle_cmpsb(
        self, insn: dict, known_funcs: Set[int]
    ) -> List[str] | None:
        lines: List[str] = []
        self.flush_pending_dos(lines)
        src_seg = self._string_source_segment(insn)
        lines.append("int delta = DF ? -1 : 1;")
        lines.append("{")
        lines.append(f"    uint32_t left_val = memb({src_seg}, si);")
        lines.append("    uint32_t right_val = memb(es, di);")
        lines.append("    CF = left_val < right_val;")
        lines.append("    uint32_t tmp = left_val - right_val;")
        lines.append("    uint8_t result = (uint8_t)tmp;")
        lines.append("    ZF = result == 0;")
        lines.append("    SF = (result >> 7) & 1;")
        lines.append(
            "    OF = ((left_val ^ right_val) & (left_val ^ result) "
            "& 0x80) != 0;"
        )
        lines.append("}")
        lines.append("si = (si + delta) & 0xFFFF;")
        lines.append("di = (di + delta) & 0xFFFF;")
        return lines

    def handle_scasb(
        self, insn: dict, known_funcs: Set[int]
    ) -> List[str] | None:
        lines: List[str] = []
        self.flush_pending_dos(lines)
        lines.append("int delta = DF ? -1 : 1;")
        lines.append("{")
        lines.append("    uint32_t left_val = al;")
        lines.append("    uint32_t right_val = memb(es, di);")
        lines.append("    CF = left_val < right_val;")
        lines.append("    uint32_t tmp = left_val - right_val;")
        lines.append("    uint8_t result = (uint8_t)tmp;")
        lines.append("    ZF = result == 0;")
        lines.append("    SF = (result >> 7) & 1;")
        lines.append(
            "    OF = ((left_val ^ right_val) & (left_val ^ result) "
            "& 0x80) != 0;"
        )
        lines.append("}")
        lines.append("di = (di + delta) & 0xFFFF;")
        return lines

    def handle_scasw(
        self, insn: dict, known_funcs: Set[int]
    ) -> List[str] | None:
        lines: List[str] = []
        self.flush_pending_dos(lines)
        lines.append("int delta = DF ? -2 : 2;")
        lines.append("{")
        lines.append("    uint32_t left_val = ax;")
        lines.append("    uint32_t right_val = memw(es, di);")
        lines.append("    CF = left_val < right_val;")
        lines.append("    uint32_t tmp = left_val - right_val;")
        lines.append("    uint16_t result = (uint16_t)tmp;")
        lines.append("    ZF = result == 0;")
        lines.append("    SF = (result >> 15) & 1;")
        lines.append("    PF = parity8((uint8_t)result);")
        lines.append(
            "    OF = ((left_val ^ right_val) & (left_val ^ result) "
            "& 0x8000) != 0;"
        )
        lines.append("}")
        lines.append("di = (di + delta) & 0xFFFF;")
        return lines

    def handle_stosb(
        self, insn: dict, known_funcs: Set[int]
    ) -> List[str] | None:
        lines: List[str] = []
        self.flush_pending_dos(lines)
        lines.append("{")
        lines.append("int delta = DF ? -1 : 1;")
        lines.append("memb_write(es, di, al);")
        lines.append("di = (di + delta) & 0xFFFF;")
        lines.append("}")
        return lines

    def handle_stosw(
        self, insn: dict, known_funcs: Set[int]
    ) -> List[str] | None:
        lines: List[str] = []
        self.flush_pending_dos(lines)
        lines.append("{")
        lines.append("int delta = DF ? -2 : 2;")
        lines.append("memw_write(es, di, ax);")
        lines.append("di = (di + delta) & 0xFFFF;")
        lines.append("}")
        return lines

    def handle_rep(
        self, insn: dict, known_funcs: Set[int]
    ) -> List[str] | None:
        lines: List[str] = []
        self.flush_pending_dos(lines)
        src_seg = self._string_source_segment(insn)
        op_str = _decode_variables(insn.get("op_str", ""))
        if op_str.startswith("movsb"):
            # On real hardware ``rep movsb`` is a single block-copy
            # instruction that runs at memory bandwidth; the CRT raster
            # cannot catch it mid-copy within a vsync window. A
            # byte-by-byte C loop, in contrast, takes long enough that
            # screen blits show row-by-row tearing. Emit the block copy
            # directly via ``rep_movsb_block``, which uses memmove on
            # the underlying virtual memory and falls back to the
            # original loop if either segment is RCB-mapped (where
            # per-byte semantics actually matter).
            lines.append(
                f"rep_movsb_block(es, {src_seg});"
            )
            return lines
        if op_str.startswith("movsw"):
            lines.append(
                f"rep_movsw_block(es, {src_seg});"
            )
            return lines
        if op_str.startswith("stosb"):
            # The previous inline `memset(seg_off(es, di + ...), al, cx)`
            # bypassed memb_write_impl / rcb_write*, so when a stosb
            # crossed into the RCB indirect-dispatch region (es=12B0,
            # di in 0xFF00..0xFFFF) it corrupted ljmp/lcall target
            # slots invisibly. rep_stosb_block falls back to per-byte
            # memb_write_impl when the range touches RCB or a WATCHW
            # watch so semantics stay correct; uses host memset only
            # on safe ranges. Same structure as rep_movsb_block.
            lines.append("rep_stosb_block(es);")
            return lines
        if op_str.startswith("stosw"):
            lines.append("{")
            lines.append("    int delta = DF ? -2 : 2;")
            lines.append("    for (; cx; cx--) {")
            lines.append("        memw_write(es, di, ax);")
            lines.append("        di = (di + delta) & 0xFFFF;")
            lines.append("    }")
            lines.append("    cx = 0;")
            lines.append("}")
            return lines
        if op_str.startswith("lodsb"):
            lines.append("if (cx) {")
            lines.append("    int delta = DF ? -1 : 1;")
            lines.append(f"    al = memb({src_seg}, si + (cx - 1) * delta);")
            lines.append("    si = (si + cx * delta) & 0xFFFF;")
            lines.append("    cx = 0;")
            lines.append("}")
            return lines
        asm = f"rep {op_str}".strip()
        self._emit_unsupported_abort(lines, insn, asm)
        return lines

    def handle_repe_cmpsb(
        self, insn: dict, known_funcs: Set[int]
    ) -> List[str] | None:
        lines: List[str] = []
        self.flush_pending_dos(lines)
        src_seg = self._string_source_segment(insn)
        op_str = _decode_variables(insn.get("op_str", ""))
        if op_str.startswith("cmpsb"):
            # repe cmpsb: compare [si] vs es:[di] while equal, up to cx times,
            # stopping at the FIRST mismatch. si/di/cx must reflect where it
            # actually stopped (partial), not the full count -- the old
            # compareMemoryUntilMismatch fast path advanced by the full cx and
            # zeroed cx, corrupting si/di/cx on an early mismatch. Mirror the
            # cmpsw path's per-element form (byte width).
            lines.append("{")
            lines.append("    int delta = DF ? -1 : 1;")
            lines.append("    uint16_t count = cx, i = 0;")
            lines.append("    uint8_t lb = 0, rb = 0;")
            lines.append("    while (i < count) {")
            lines.append(f"        lb = memb({src_seg}, si);")
            lines.append("        rb = memb(es, di);")
            lines.append("        si = (si + delta) & 0xFFFF;")
            lines.append("        di = (di + delta) & 0xFFFF;")
            lines.append("        ++i;")
            lines.append("        if (lb != rb) break;  /* ZF->0 ends repe */")
            lines.append("    }")
            lines.append("    cx = (count - i) & 0xFFFF;")
            lines.append("    if (i > 0) {")
            lines.append("        uint32_t t = (uint32_t)lb - (uint32_t)rb;")
            lines.append("        uint8_t res = (uint8_t)t;")
            lines.append("        CF = lb < rb;")
            lines.append("        ZF = res == 0;")
            lines.append("        PF = parity8(res);")
            lines.append("        SF = (res >> 7) & 1;")
            lines.append("        OF = ((lb ^ rb) & (lb ^ res) & 0x80) != 0;")
            lines.append("    }")
            lines.append("}")
            return lines
        if op_str.startswith("cmpsw"):
            # repe cmpsw: compare words [si] vs es:[di] while equal, cx times.
            lines.append("{")
            lines.append("    int delta = DF ? -2 : 2;")
            lines.append("    uint16_t count = cx, i = 0, lw = 0, rw = 0;")
            lines.append("    while (i < count) {")
            lines.append(f"        lw = memw({src_seg}, si);")
            lines.append("        rw = memw(es, di);")
            lines.append("        si = (si + delta) & 0xFFFF;")
            lines.append("        di = (di + delta) & 0xFFFF;")
            lines.append("        ++i;")
            lines.append("        if (lw != rw) break;  /* ZF->0 ends repe */")
            lines.append("    }")
            lines.append("    cx = (count - i) & 0xFFFF;")
            lines.append("    if (i > 0) {")
            lines.append("        uint32_t t = (uint32_t)lw - (uint32_t)rw;")
            lines.append("        uint16_t res = (uint16_t)t;")
            lines.append("        CF = lw < rw;")
            lines.append("        ZF = res == 0;")
            lines.append("        PF = parity8((uint8_t)res);")
            lines.append("        SF = (res >> 15) & 1;")
            lines.append(
                "        OF = ((lw ^ rw) & (lw ^ res) & 0x8000) != 0;")
            lines.append("    }")
            lines.append("}")
            return lines
        if op_str.startswith("scasb"):
            # repe scasb: scan es:[di] while AL == byte, stopping at the first
            # mismatch or when cx hits 0 (the scan-while-equal counterpart of
            # repne scasb). Per-byte; flags reflect the last CMP AL, byte.
            lines.append("{")
            lines.append("    int delta = DF ? -1 : 1;")
            lines.append("    uint16_t count = cx, i = 0;")
            lines.append("    uint8_t last_byte = 0;")
            lines.append("    while (i < count) {")
            lines.append("        last_byte = memb(es, di);")
            lines.append("        di = (di + delta) & 0xFFFF;")
            lines.append("        ++i;")
            lines.append("        if (al != last_byte) break;  /* ZF->0 ends repe */")
            lines.append("    }")
            lines.append("    cx = (count - i) & 0xFFFF;")
            lines.append("    if (i > 0) {")
            lines.append("        uint32_t l = al, r = last_byte, t = l - r;")
            lines.append("        uint8_t res = (uint8_t)t;")
            lines.append("        CF = l < r;")
            lines.append("        ZF = res == 0;")
            lines.append("        PF = parity8(res);")
            lines.append("        SF = (res >> 7) & 1;")
            lines.append("        OF = ((l ^ r) & (l ^ res) & 0x80) != 0;")
            lines.append("    }")
            lines.append("}")
            return lines
        asm = f"{insn['mnemonic']} {op_str}".strip()
        self._emit_unsupported_abort(lines, insn, asm)
        return lines

    def handle_jcc(
        self, insn: dict, known_funcs: Set[int]
    ) -> List[str] | None:
        lines: List[str] = []
        self.flush_pending_dos(lines)
        op_str = _decode_variables(insn.get("op_str", ""))
        try:
            target = int(op_str, 16)
        except ValueError:
            asm = f"{insn['mnemonic']} {op_str}".strip()
            self._emit_unsupported_abort(lines, insn, asm)
            return lines

        cond = self.jcc_condition(
            insn["mnemonic"], insn.get("cond_prev"), insn.get("address")
        )
        self._emit_with_comment(lines, insn, f"if ({cond}) {{")
        if target in known_funcs or target in self.current_func_addrs:
            lines.append(f"    pc = 0x{target:04X};")
            lines.append("    continue;")
        else:
            name = self.func_name(target)
            lines.append(f"    {name}();")
            lines.append("    return;")
        lines.append("}")
        return lines

    def handle_noop(
        self, insn: dict, known_funcs: Set[int]
    ) -> List[str]:
        # ``nop`` has no effect; leave pending register tracking intact so
        # subsequent interrupt translation can still observe cached values.
        return []

    def format_instruction(
        self, insn: dict, known_funcs: Set[int]
    ) -> List[str]:
        """Render a single instruction by delegating to mnemonic handlers."""

        handler = self.handlers.get(insn["mnemonic"])
        if not handler and " " in insn["mnemonic"]:
            prefix, base = insn["mnemonic"].split(" ", 1)
            unknown_prefix = False
            prefixed = dict(insn)
            prefixed["mnemonic"] = prefix
            prefixed["op_str"] = f"{base} {insn.get('op_str', '')}".strip()
            handler = self.handlers.get(prefix)
            if handler:
                result = handler(prefixed, known_funcs)
                if result is not None:
                    return result
            else:
                unknown_prefix = True
            base_insn = dict(insn)
            base_insn["mnemonic"] = base
            handler = self.handlers.get(base)
            if handler:
                result = handler(base_insn, known_funcs)
                if result is not None:
                    if unknown_prefix:
                        op_str = _decode_variables(insn.get("op_str", ""))
                        asm = f"{insn['mnemonic']} {op_str}".strip()
                        _abort_lines: List[str] = []
                        self._emit_unsupported_abort(_abort_lines, insn, asm)
                        return [*_abort_lines, *result]
                    return result
        elif handler:
            result = handler(insn, known_funcs)
            if result is not None:
                return result

        if insn["mnemonic"].startswith("j") and insn["mnemonic"] != "jmp":
            result = self.handle_jcc(insn, known_funcs)
            if result is not None:
                return result

        lines: List[str] = []
        self.flush_pending_dos(lines)
        op_str = _decode_variables(insn.get("op_str", ""))
        asm = f"{insn['mnemonic']} {op_str}".strip()
        self._emit_unsupported_abort(lines, insn, asm)
        return lines

    # -- Region reduction -------------------------------------------------

    def structure(
        self,
        blocks: Dict[int, BasicBlock],
        graph: "nx.DiGraph",
        known_funcs: Set[int],
        entry: int,
        loop_exits: Set[int] | None = None,
    ) -> List[ASTNode]:
        """Structure *blocks* using CFG utilities and return AST nodes.

        The algorithm runs iteratively: after identifying a high level region
        (loop, switch or conditional) the participating basic blocks are
        removed from the working CFG.  Dominator and post-dominator information
        is recomputed for the pruned graph on each pass so that subsequent
        pattern matches operate on a simplified structure mirroring the
        "iterative control-flow structuring" approach described in
        decompilation literature.
        """

        # Operate on copies so callers retain the original CFG.
        graph = cfg.merge_shared_tails(blocks, graph)
        pending_blocks: Dict[int, BasicBlock] = dict(blocks)
        pending_graph = graph.copy()
        nodes_with_addr: List[tuple[int, ASTNode]] = []
        order = cfg.traversal_order(graph, entry)
        ipdom_all = cfg.compute_immediate_postdominators(graph)

        # Identify blocks that are shared/join points and shouldn't be consumed
        protected_blocks: Set[int] = set()
        for addr in pending_graph.nodes:
            if pending_graph.in_degree(addr) > 1:
                protected_blocks.add(addr)
        postdom_joins = set(ipdom_all.values()) - {None}
        protected_blocks.update(postdom_joins)

        # Track blocks that have been logically processed even if still present
        processed_blocks: Set[int] = set()

        while pending_blocks:
            start_size = len(pending_blocks)
            logger.debug(
                "Structuring pass starting with %d pending blocks", start_size
            )
            loop_map = cfg.find_loops(pending_blocks, pending_graph)
            ipost = cfg.compute_immediate_postdominators(pending_graph)
            ctx = PatternContext(
                pending_blocks,
                pending_graph,
                known_funcs,
                self,
                loop_map,
                ipost,
                loop_exits or set(),
            )

            progress = False
            for start in order:
                if start not in pending_blocks or start in processed_blocks:
                    continue
                for_res = detect_for_loop(start, ctx)
                if for_res is not None:
                    nodes_with_addr.append((start, for_res.node))
                    if hasattr(for_res.node, "header"):
                        self.loop_headers.add(for_res.node.header)
                    for b in for_res.consumed_blocks:
                        processed_blocks.add(b)
                        if b not in protected_blocks:
                            pending_blocks.pop(b, None)
                            if b in pending_graph:
                                pending_graph.remove_node(b)
                    progress = True
                    break

                loop_res = detect_loop(start, ctx)
                if loop_res is not None:
                    if self.comment_lines(start):
                        nodes_with_addr.append((start, CommentNode(start)))
                    nodes_with_addr.append((start, loop_res.node))
                    if hasattr(loop_res.node, "header"):
                        self.loop_headers.add(loop_res.node.header)
                    for b in loop_res.consumed_blocks:
                        processed_blocks.add(b)
                        if b not in protected_blocks:
                            pending_blocks.pop(b, None)
                            if b in pending_graph:
                                pending_graph.remove_node(b)
                    progress = True
                    break

                switch_res = detect_switch(start, ctx)
                if switch_res is not None:
                    if self.comment_lines(start):
                        nodes_with_addr.append((start, CommentNode(start)))
                    nodes_with_addr.append((start, switch_res.node))
                    for b in switch_res.consumed_blocks:
                        processed_blocks.add(b)
                        if b not in protected_blocks:
                            pending_blocks.pop(b, None)
                            if b in pending_graph:
                                pending_graph.remove_node(b)
                    progress = True
                    break

                if_else_res = detect_if_else(start, ctx)
                if if_else_res is not None:
                    if self.comment_lines(start):
                        nodes_with_addr.append((start, CommentNode(start)))
                    nodes_with_addr.append((start, if_else_res.node))
                    for b in if_else_res.consumed_blocks:
                        processed_blocks.add(b)
                        if b not in protected_blocks:
                            pending_blocks.pop(b, None)
                            if b in pending_graph:
                                pending_graph.remove_node(b)
                    progress = True
                    break

            if not progress:
                for start in order:
                    if (
                        start in pending_blocks
                        and start not in processed_blocks
                    ):
                        block = pending_blocks[start]
                        insts = block.instructions
                        node: ASTNode
                        if len(insts) == 1:
                            mnem = insts[0]["mnemonic"]
                            if mnem in {"ret", "retn", "retf"}:
                                pop_bytes = _parse_imm(insts[0].get("op_str", ""))
                                node = ReturnNode(start, mnem, pop_bytes)
                            elif mnem == "jmp":
                                node = GotoNode(start, insts[0])
                            elif mnem == "call":
                                node = CallNode(start, insts[0])
                            else:
                                node = BasicBlockNode(start, insts)
                        else:
                            node = BasicBlockNode(start, insts)
                        if self.comment_lines(start):
                            nodes_with_addr.append((start, CommentNode(start)))
                        nodes_with_addr.append((start, node))
                        processed_blocks.add(start)
                        if start not in protected_blocks:
                            pending_blocks.pop(start, None)
                            if start in pending_graph:
                                pending_graph.remove_node(start)
                        break
                else:
                    break

            if (
                len(pending_blocks) == start_size
                and not progress
                and all(b in processed_blocks for b in pending_blocks)
            ):
                break
            logger.debug(
                "Structuring pass completed with %d pending blocks",
                len(pending_blocks),
            )

        order_index = {addr: i for i, addr in enumerate(order)}
        nodes_with_addr.sort(
            key=lambda item: order_index.get(item[0], len(order_index))
        )
        nodes_with_addr = self._extract_shared_postdom_blocks(
            nodes_with_addr, pending_graph
        )

        return [node for _addr, node in nodes_with_addr]

    def _merge_adjacent_empty_if(self, nodes: List[ASTNode]) -> None:
        """Merge a trailing instruction into a preceding empty ``if`` block.

        Recursively walks *nodes* so nested control-flow is also adjusted."""
        i = 0
        while i < len(nodes):
            node = nodes[i]
            # Recurse into child node lists
            if isinstance(node, IfElseNode):
                self._merge_adjacent_empty_if(node.then_body)
                if node.else_body is not None:
                    self._merge_adjacent_empty_if(node.else_body)
            elif hasattr(node, "body"):
                self._merge_adjacent_empty_if(getattr(node, "body"))
            elif hasattr(node, "cases"):
                for _val, body in getattr(node, "cases"):
                    if isinstance(body, list):
                        self._merge_adjacent_empty_if(body)

            if (
                i < len(nodes) - 1
                and isinstance(node, IfElseNode)
                and not node.then_body
                and node.else_body is None
            ):
                next_node = nodes[i + 1]
                if (
                    isinstance(next_node, BasicBlockNode)
                    and next_node.instructions
                ):
                    first = next_node.instructions[0]
                    if (
                        first["mnemonic"] == "mov"
                        and first["op_str"].replace(" ", "")
                        in {"al,1", "al,0x1"}
                    ):
                        node.then_body = [
                            BasicBlockNode(first["address"], [first])
                        ]
                        next_node.instructions = next_node.instructions[1:]
                        if not next_node.instructions:
                            nodes.pop(i + 1)
                            continue
            i += 1

    def _rewrite_returning_do_while(self, nodes: List[ASTNode]) -> None:
        """Rewrite ``do { ... } while (...)`` blocks that end in ``return``.

        These structures arise from misidentified loops where the body
        terminates unconditionally before the condition is evaluated.  When
        encountered, convert them into an ``if`` statement with the remaining
        statements becoming the ``else`` branch so that the condition is tested
        prior to executing either path."""

        i = 0
        while i < len(nodes):
            node = nodes[i]
            # Recurse into nested bodies first
            if isinstance(node, IfElseNode):
                self._rewrite_returning_do_while(node.then_body)
                if node.else_body is not None:
                    self._rewrite_returning_do_while(node.else_body)
            elif isinstance(node, DoWhileNode):
                self._rewrite_returning_do_while(node.body)
                if node.body:
                    tail = node.body[-1]
                    is_ret = isinstance(tail, ReturnNode) or (
                        isinstance(tail, BasicBlockNode)
                        and getattr(tail, "instructions", [])
                        and tail.instructions[-1].get("mnemonic")
                        in {"ret", "retn", "retf"}
                    )
                else:
                    is_ret = False
                if is_ret:
                    else_body = nodes[i + 1:]
                    if_else = IfElseNode(
                        [],
                        node.cond_mnem,
                        node.prev,
                        node.body,
                        else_body or None,
                        node.header,
                        negate=node.negate,
                    )
                    nodes[i:] = [if_else]
                    return
            i += 1

    def render_function(
        self, func: dict, known_funcs: Set[int]
    ) -> List[str]:
        self.last_ah = None
        self.last_al = None
        self.last_dx = None
        self.last_bx = None
        self.last_dl = None
        self.pending_dos = []
        self.current_func_name = self.func_name(func["start"])
        instrs = normalize_flags(func["instructions"])
        instrs = normalize_indirect_jumps(instrs)
        self.current_func_all_addrs = {insn["address"] for insn in instrs}
        self.current_func_addrs = set(self.current_func_all_addrs)
        label_addrs = {
            insn["address"]
            for insn in instrs
            if insn["address"] in self.extern_labels
            or (
                insn["address"] in self.func_names
                and insn["address"] not in known_funcs
            )
        }
        blocks = build_basic_blocks(
            instrs, label_addrs, func_start=func["start"]
        )
        graph = cfg.build_cfg(blocks)
        propagate_flag_conditions(blocks, graph)
        for block in blocks.values():
            block.instructions = [
                i for i in block.instructions if not i.get("skip")
            ]
        self.current_func_addrs = {
            i["address"] for b in blocks.values() for i in b.instructions
        }
        for addr in list(self.func_names):
            if (
                addr not in self.current_func_addrs
                and self.func_names[addr].startswith("label_")
            ):
                self.func_names.pop(addr)
        graph = cfg.build_cfg(blocks)
        indent = "    "
        for addr in sorted(label_addrs):
            region = cfg.collect_branch(
                graph, addr, None, label_addrs - {addr}
            )
            # Only keep nodes at or after the entry address. Without this,
            # backward edges from the region can pull preceding blocks into the
            # dispatch region, causing the originating jump to vanish from the
            # parent function and leaving a self-recursive dispatch block.
            region = {a for a in region if a >= addr}
            region_blocks = {
                a: blocks.pop(a) for a in region if a in blocks
            }
            if region_blocks:
                region_graph = cfg.build_cfg(region_blocks)
                propagate_flag_conditions(region_blocks, region_graph)
                prev_blocks = self.current_blocks
                self.current_blocks = region_blocks
                nodes = self.structure(
                    region_blocks, region_graph, known_funcs, addr
                )
                lines: List[str] = []
                for node in nodes:
                    lines.extend(node.render(self, indent, known_funcs))
                if self.pending_dos:
                    lines.extend(self.pending_dos)
                    self.pending_dos.clear()
                name = self.func_name(addr)
                label_line = f"{name}:"
                seen_labels: Set[str] = set()
                filtered: List[str] = []
                for line in lines:
                    stripped = line.strip()
                    if stripped.endswith(":"):
                        if stripped == label_line:
                            continue
                        if stripped in seen_labels:
                            continue
                        seen_labels.add(stripped)
                    filtered.append(line)
                lines = []
                for line in filtered:
                    m = re.match(r"(\s*)goto ([A-Za-z0-9_]+);", line)
                    if m and f"{m.group(2)}:" not in seen_labels:
                        try:
                            target = int(m.group(2).rsplit("_", 1)[1], 16)
                            indent = m.group(1)
                            if target in self.current_blocks:
                                lines.append(f"{indent}pc = 0x{target:04X};")
                                lines.append(f"{indent}continue;")
                            else:
                                name = self.func_name(target)
                                lines.append(f"{indent}{name}();")
                                lines.append(f"{indent}return;")
                        except (ValueError, IndexError):
                            lines.append(line)
                        continue
                    m = re.match(
                        r"(\s*)if \((.+)\) goto ([A-Za-z0-9_]+);",
                        line,
                    )
                    if m and f"{m.group(3)}:" not in seen_labels:
                        try:
                            indent, cond, label = m.groups()
                            target = int(label.rsplit("_", 1)[1], 16)
                            if target in self.current_blocks:
                                lines.append(f"{indent}if ({cond}) {{")
                                lines.append(
                                    f"{indent}    pc = 0x{target:04X};"
                                )
                                lines.append(f"{indent}    continue;")
                                lines.append(f"{indent}}}")
                            else:
                                name = self.func_name(target)
                                lines.append(f"{indent}if ({cond}) {{")
                                lines.append(f"{indent}    {name}();")
                                lines.append(f"{indent}    return;")
                                lines.append(f"{indent}}}")
                        except (ValueError, IndexError):
                            lines.append(line)
                    else:
                        lines.append(line)
                if addr == 0x0B01:
                    for i, code in enumerate(lines):
                        if "pc =" in code:
                            swap_call = (
                                "    file_mapping_swap(cs + 0x2000, "
                                "0x9000, es, 0x3000, 0x7000);"
                            )
                            lines.insert(i, swap_call)
                            break
                self.extra_func_blocks[addr] = lines
                graph.remove_nodes_from(region)
                self.last_ah = None
                self.last_al = None
                self.last_dx = None
                self.last_bx = None
                self.last_dl = None
                self.pending_dos = []
                self.current_blocks = prev_blocks
        self.current_func_addrs = {
            i["address"] for b in blocks.values() for i in b.instructions
        }
        if label_addrs:
            graph = cfg.build_cfg(blocks)
            propagate_flag_conditions(blocks, graph)

        params = "const char *file, const char *func, int line"
        if func["start"] in graph:
            reachable = cfg.collect_branch(graph, func["start"], None, None)
            reachable.add(func["start"])
        else:
            reachable = set()
        self.current_blocks = {a: blocks[a] for a in reachable if a in blocks}
        # ``indent`` already defined above
        nodes = self.structure(
            blocks, graph, known_funcs, func["start"]
        )
        self._rewrite_returning_do_while(nodes)
        self._merge_adjacent_empty_if(nodes)

        orig_name = f"func_{func['start']:04X}"
        name = self.func_name(func["start"])
        impl = f"{name}_impl"
        if name != orig_name:
            lines = [f"// {orig_name}", f"void {impl}({params}) {{"]
        else:
            lines = [f"void {impl}({params}) {{"]
        lines.append(f"{indent}shim_log(__func__, file, func, line, NULL);")
        if not nodes or not isinstance(nodes[0], BasicBlockNode):
            lines.append(f"{indent}SAFEPOINT();")
        for node in nodes:
            lines.extend(node.render(self, indent, known_funcs))
        if self.pending_dos:
            lines.extend(self.pending_dos)
            self.pending_dos.clear()
        # Replace gotos to undefined labels with function calls
        seen_labels = {
            lbl.strip() for lbl in lines if lbl.strip().endswith(":")
        }
        fixed: List[str] = []
        for line in lines:
            m = re.match(r"(\s*)goto ([A-Za-z0-9_]+);", line)
            if m and f"{m.group(2)}:" not in seen_labels:
                try:
                    target = int(m.group(2).rsplit("_", 1)[1], 16)
                    indent = m.group(1)
                    if target in self.current_blocks:
                        fixed.append(f"{indent}pc = 0x{target:04X};")
                        fixed.append(f"{indent}continue;")
                    else:
                        name = self.func_name(target)
                        fixed.append(f"{indent}{name}();")
                        fixed.append(f"{indent}return;")
                except (ValueError, IndexError):
                    fixed.append(line)
                continue
            m = re.match(r"(\s*)if \((.+)\) goto ([A-Za-z0-9_]+);", line)
            if m and f"{m.group(3)}:" not in seen_labels:
                try:
                    indent, cond, label = m.groups()
                    target = int(label.rsplit("_", 1)[1], 16)
                    if target in self.current_blocks:
                        fixed.append(f"{indent}if ({cond}) {{")
                        fixed.append(f"{indent}    pc = 0x{target:04X};")
                        fixed.append(f"{indent}    continue;")
                        fixed.append(f"{indent}}}")
                    else:
                        name = self.func_name(target)
                        fixed.append(f"{indent}if ({cond}) {{")
                        fixed.append(f"{indent}    {name}();")
                        fixed.append(f"{indent}    return;")
                        fixed.append(f"{indent}}}")
                except (ValueError, IndexError):
                    fixed.append(line)
            else:
                fixed.append(line)
        lines = fixed

        def _node_returns(n: ASTNode) -> bool:
            if isinstance(n, ReturnNode):
                return True
            if isinstance(n, BasicBlockNode):
                return (
                    n.instructions
                    and n.instructions[-1].get("mnemonic")
                    in {"ret", "retn", "retf"}
                )
            if isinstance(n, IfElseNode) and n.then_body and n.else_body:
                return (
                    _node_returns(n.then_body[-1])
                    and _node_returns(n.else_body[-1])
                )
            return False

        if nodes and _node_returns(nodes[-1]):
            last = "return;"
        else:
            last = next(
                (line.strip() for line in reversed(lines) if line.strip()),
                "",
            )
        if not self._terminates(last):
            if instrs and instrs[-1]["mnemonic"] in {"ret", "retn", "retf"}:
                if (
                    len(instrs) >= 2
                    and instrs[-2]["mnemonic"] == "pop"
                    and instrs[-2]["op_str"].lower() == "bp"
                ):
                    lines.append(f"{indent}bp = memw(ss, sp);")
                    lines.append(f"{indent}sp = (sp + 2) & 0xFFFF;")
                lines.append(f"{indent}return;")
        lines.append("}")
        self.current_func_addrs = set()
        self.current_blocks = {}
        return lines

    def _terminates(self, line: str) -> bool:
        line = line.strip()
        if line.startswith("return") or line.startswith("goto"):
            return True
        match = _CALL_RE.match(line)
        if match and match.group(1) in _NORETURN_FUNCS:
            return True
        return False

    # -- Helper methods ------------------------------------------------------
    def _extract_shared_postdom_blocks(
        self,
        nodes_with_addr: List[tuple[int, ASTNode]],
        graph: "nx.DiGraph",
    ) -> List[tuple[int, ASTNode]]:
        ipdom = cfg.compute_immediate_postdominators(graph)
        counts = Counter(ipdom.values())
        shared = {addr for addr, c in counts.items() if c > 1}
        shared.update(n for n in graph.nodes if graph.in_degree(n) > 1)
        if not shared:
            return nodes_with_addr

        seen: Set[int] = set()
        new_nodes: List[tuple[int, ASTNode]] = []
        for addr, node in nodes_with_addr:
            if isinstance(node, CommentNode):
                if addr in seen:
                    continue
                new_nodes.append((addr, node))
                continue
            if isinstance(
                node, (BasicBlockNode, GotoNode, CallNode, ReturnNode)
            ) and addr in shared:
                if addr in seen:
                    continue
                seen.add(addr)
            new_nodes.append((addr, node))

        return new_nodes


class PCSwitchRenderer(CCodeRenderer):
    """Render a module as a single PC-driven dispatcher."""

    def __init__(self, *args, **kwargs) -> None:  # pragma: no cover
        super().__init__(*args, **kwargs)
        self.dispatch_name = f"{self.name_prefix}dispatch"
        self.dispatch_cases: List[str] = []
        self._seen_cases: Set[int] = set()

    def render_function(self, func: dict, known_funcs: Set[int]) -> List[str]:
        instrs = normalize_flags(func["instructions"])
        instrs = normalize_indirect_jumps(instrs)
        blocks = build_basic_blocks(
            instrs, {func["start"]}, func_start=func["start"]
        )
        graph = cfg.build_cfg(blocks)

        name = self.func_name(func["start"])
        self.current_func_name = name
        impl = f"{name}_impl"
        params = (
            "uint16_t expected_retip, const char *file, const char *func, "
            "int line"
        )

        block_addrs = sorted(blocks)
        cases = sorted({func["start"], *block_addrs})
        first_real = block_addrs[0] if block_addrs else func["start"]

        indent = "            "
        for addr in cases:
            if addr in self._seen_cases:
                continue
            self._seen_cases.add(addr)
            self.dispatch_cases.append(
                f"        case 0x{addr:04X}: {{ /* {impl}@{addr:04X} */"
            )
            block = blocks.get(addr)
            if block is None:
                if block_addrs:
                    self.dispatch_cases.append(
                        f"{indent}pc = 0x{first_real:04X};"
                    )
                    self.dispatch_cases.append(f"{indent}continue;")
                else:
                    self.dispatch_cases.append(f"{indent}return;")
            else:
                self.dispatch_cases.extend(
                    self._render_block_state_machine(block, graph, known_funcs)
                )
            self.dispatch_cases.append("        }")
            self.dispatch_cases.append("")

        # Strip trailing underscore from the name_prefix (e.g. "module_" → "module").
        # save_manager / snapshot use this name to identify the active binary so
        # restore can route through the right dispatch instead of relying on
        # cs:ip arithmetic (which doesn't round-trip via file_mappings when
        # case bodies set ip to file_offset values).
        # The wrapper used to call shim_sp_track_func_enter/leave around
        # the dispatch to detect translator-side stack drift. With the
        # literal-emission model in place, the simulated SP IS the truth
        # — every push and pop is exactly what the asm says — so there's
        # nothing for a side channel to verify. shim_enter_binary stays
        # because save_manager/snapshot rely on knowing the active binary.
        binary_name = self.name_prefix.rstrip("_")
        lines: List[str] = [f"void {impl}({params}) {{"]
        lines.append(f"    shim_enter_binary(\"{binary_name}\");")
        # Save the outer-caller's tail-dispatch state so the inner dispatch
        # doesn't see/clobber it. After the inner dispatch returns, drain
        # ITS pending request (if any) and restore the outer's. Without
        # this, a wrapper invoked directly from another case body
        # (<name>_func_XXX(retIP) call site) leaves its inner pending to
        # silently die when control returns to the caller's case which
        # unconditionally does pc=fallthrough — the cross-binary jump
        # intent is lost, the simulated stack drifts, and eventually a
        # later near-ret pops uninitialized memory (cave-music repro).
        lines.append("    ShimTailDispatchState saved_tail;")
        lines.append("    shim_tail_dispatch_save(&saved_tail);")
        lines.append(
            f"    {self.dispatch_name}(0x{func['start']:04X}, expected_retip, "
            "file, func, line);"
        )
        lines.append("    shim_drain_pending_tail_dispatch(file, func, line);")
        lines.append("    shim_tail_dispatch_restore(&saved_tail);")
        lines.append("    shim_leave_binary();")
        lines.append("}")
        return lines

    def _try_match_swap_loop_w(
        self, block: BasicBlock
    ) -> List[str] | None:
        """Detect a word swap-loop (game's overlay bank-switch primitive)
        and replace with a single ``shim_swap_regions_w`` call.

        Two equivalent forms are recognised. Both exchange one word per
        iteration between [ds:si] and [es:di], then advance si and di by
        ``±2`` (per DF) and ``loop`` until cx hits zero.

        Form A (5 insn, used by a single-segment module):
            lodsw                          ; ax = [ds:si]; si += d
            mov   dx, word ptr es:[di]     ; dx = [es:di]
            stosw                          ; [es:di] = ax; di += d
            mov   word ptr [si - 2], dx    ; [ds:si - 2] = dx
            loop  <block.start>

        Form B (4 insn, used by an overlay archive):
            mov   ax, word ptr es:[di]     ; ax = [es:di] (save dest)
            movsw                          ; [es:di] = [ds:si]; si,di += d
            mov   word ptr [si - 2], ax    ; [ds:si - 2] = ax
            loop  <block.start>

        Without the shim, dispatch_via_binary uses the pre-swap chunk for
        the destination addresses because file_mappings isn't updated.
        The shim does the byte swap AND relocates file_mappings so the
        (linear -> file/offset) lookup follows the moved bytes.
        """
        insns = block.instructions
        if len(insns) == 5:
            ok = (
                insns[0].get("mnemonic") == "lodsw"
                and insns[1].get("mnemonic") == "mov"
                and "es:[di]" in insns[1].get("op_str", "").lower().replace(" ", "")
                and insns[1].get("op_str", "").lower().replace(" ", "").startswith("dx,")
                and insns[2].get("mnemonic") == "stosw"
                and insns[3].get("mnemonic") == "mov"
                and "[si-2]" in insns[3].get("op_str", "").lower().replace(" ", "")
                and insns[3].get("op_str", "").lower().replace(" ", "").endswith(",dx")
                and insns[4].get("mnemonic") == "loop"
            )
            if not ok:
                return None
            form_name = "lodsw/mov-dx,es:[di]/stosw/mov-[si-2],dx/loop"
            loop_insn = insns[4]
            start_insn = insns[0]
        elif len(insns) == 4:
            op0 = insns[0].get("op_str", "").lower().replace(" ", "")
            op2 = insns[2].get("op_str", "").lower().replace(" ", "")
            ok = (
                insns[0].get("mnemonic") == "mov"
                and op0.startswith("ax,")
                and "es:[di]" in op0
                and insns[1].get("mnemonic") == "movsw"
                and insns[2].get("mnemonic") == "mov"
                and "[si-2]" in op2
                and op2.endswith(",ax")
                and insns[3].get("mnemonic") == "loop"
            )
            if not ok:
                return None
            form_name = "mov-ax,es:[di]/movsw/mov-[si-2],ax/loop"
            loop_insn = insns[3]
            start_insn = insns[0]
        else:
            return None

        target_str = loop_insn.get("op_str", "").strip()
        try:
            loop_target = int(target_str, 16)
        except ValueError:
            return None
        if loop_target != block.start:
            return None
        loop_bytes = loop_insn.get("bytes", "")
        try:
            loop_len = len(bytes.fromhex(loop_bytes))
        except ValueError:
            return None
        succ_addr = (loop_insn["address"] + loop_len) & 0xFFFF

        indent = "            "
        lines: List[str] = []
        for comment in self.comment_lines(start_insn["address"]):
            lines.append(f"{indent}{comment}")
        lines.append(
            f"{indent}// Swap-loop pattern ({form_name}) -> shim_swap_regions_w"
        )
        lines.append(f"{indent}ip = 0x{(start_insn['address'] + self.cs_base) & 0xFFFF:04X};")
        lines.append(f"{indent}SAFEPOINT();")
        # Snapshot count before the call — the shim relies on the live cx.
        lines.append(f"{indent}shim_swap_regions_w(es, di, ds, si, cx, DF);")
        # Bring si/di/cx/ZF to post-loop state (as if the loop had run cx times).
        lines.append(f"{indent}{{")
        lines.append(f"{indent}    int _swap_delta = DF ? -2 : 2;")
        lines.append(
            f"{indent}    si = (uint16_t)((int)si + _swap_delta * (int)cx);"
        )
        lines.append(
            f"{indent}    di = (uint16_t)((int)di + _swap_delta * (int)cx);"
        )
        lines.append(f"{indent}    cx = 0;")
        lines.append(f"{indent}    ZF = 1;")
        lines.append(f"{indent}}}")
        lines.append(f"{indent}pc = 0x{succ_addr:04X};")
        lines.append(f"{indent}continue;")
        return lines

    def _render_block_state_machine(
        self, block: BasicBlock, graph: "nx.DiGraph", known_funcs: Set[int]
    ) -> List[str]:
        swap_lines = self._try_match_swap_loop_w(block)
        if swap_lines is not None:
            return swap_lines
        lines: List[str] = []
        indent = "            "
        insns = block.instructions
        for idx, insn in enumerate(insns):
            for comment in self.comment_lines(insn["address"]):
                lines.append(f"{indent}{comment}")
            lines.append(
                f"{indent}ip = 0x{(insn['address'] + self.cs_base) & 0xFFFF:04X};"
            )
            lines.append(f"{indent}SAFEPOINT();")

            is_last = idx == len(insns) - 1
            mnem = insn["mnemonic"]
            op_str = _decode_variables(insn.get("op_str", ""))
            op = op_str.lower().strip()

            if is_last and insn.get("op") == "INDIRECT_NEAR_JMP":
                self.flush_pending_dos(lines, indent)
                _, expr = self._indirect_jump_target(insn)
                self._emit_with_comment(
                    lines,
                    insn,
                    (
                        f"jump_table((((uint32_t)cs << 4) + (uint16_t)({expr}))"
                        " & 0xFFFFF, expected_retip);"
                    ),
                    indent,
                )
                lines.append(f"{indent}return;")
                continue

            if is_last and mnem == "jmp" and op in _JMP_REGS:
                self.flush_pending_dos(lines, indent)
                self._emit_with_comment(
                    lines,
                    insn,
                    (
                        f"jump_table((((uint32_t)cs << 4) + (uint16_t)({op}))"
                        " & 0xFFFFF, expected_retip);",
                    ),
                    indent,
                )
                lines.append(f"{indent}return;")
                continue

            if is_last and mnem == "jmp":
                self.flush_pending_dos(lines, indent)
                try:
                    target = int(op_str, 16)
                    lines.append(f"{indent}pc = 0x{target:04X};")
                    lines.append(f"{indent}continue;")
                except ValueError:
                    _abort_inner: List[str] = []
                    self._emit_unsupported_abort(_abort_inner, insn, f"jmp {op_str}")
                    for _ln in _abort_inner:
                        lines.append(f"{indent}{_ln}")
                continue

            if is_last and mnem.startswith("j") and mnem != "jmp":
                self.flush_pending_dos(lines, indent)
                try:
                    target = int(op_str, 16)
                except ValueError:
                    _abort_inner: List[str] = []
                    self._emit_unsupported_abort(_abort_inner, insn, f"{mnem} {op_str}")
                    for _ln in _abort_inner:
                        lines.append(f"{indent}{_ln}")
                    continue
                cond = self.jcc_condition(
                    mnem, insn.get("cond_prev"), insn.get("address")
                )
                succs = list(graph.successors(block.start))
                fall = next((s for s in succs if s != target), None)
                if fall is not None:
                    lines.append(f"{indent}if ({cond}) {{")
                    lines.append(f"{indent}    pc = 0x{target:04X};")
                    lines.append(f"{indent}}} else {{")
                    lines.append(f"{indent}    pc = 0x{fall:04X};")
                    lines.append(f"{indent}}}")
                    lines.append(f"{indent}continue;")
                else:
                    lines.append(f"{indent}if ({cond}) {{")
                    lines.append(f"{indent}    pc = 0x{target:04X};")
                    lines.append(f"{indent}    continue;")
                    lines.append(f"{indent}}}")
                    lines.append(f"{indent}return;")
                continue

            if is_last and mnem in {"ret", "retn", "retf"}:
                self.flush_pending_dos(lines, indent)
                formatted = self.format_instruction(insn, known_funcs)
                last_code: str | None = None
                for code in formatted:
                    lines.append(f"{indent}{code}")
                    last_code = code.strip()
                if not last_code or not last_code.startswith("return"):
                    lines.append(f"{indent}return;")
                continue

            formatted = self.format_instruction(insn, known_funcs)
            last_code: str | None = None
            for code in formatted:
                lines.append(f"{indent}{code}")
                last_code = code.strip()
            if last_code and self._terminates(last_code):
                if not last_code.startswith("return"):
                    lines.append(f"{indent}return;")
                break

            if is_last:
                # Flush any deferred DOS setup before emitting the fallthrough
                # edge so pending register updates appear in the correct
                # sequence.
                self.flush_pending_dos(lines, indent)
                if last_code in {"return;", "continue;"}:
                    pass
                else:
                    succs = list(graph.successors(block.start))
                    if succs:
                        lines.append(f"{indent}pc = 0x{succs[0]:04X};")
                        lines.append(f"{indent}continue;")
                    else:
                        lines.append(f"{indent}return;")

        self.flush_pending_dos(lines, indent)
        return lines

    def render_dispatch(self) -> List[str]:
        # ``expected_retip`` is the caller-pushed retIP; the dispatcher passes
        # it through to ``handle_ret`` (see _render_block_state_machine) for
        # the popped-IP comparison that decides return-vs-tail-dispatch.
        lines: List[str] = [
            f"void {self.dispatch_name}(int pc, uint16_t expected_retip, "
            "const char *file, const char *func, int line) {",
            "    (void)expected_retip;",
            "    for (;;) {",
            "        switch (pc) {",
        ]
        lines.extend(self.dispatch_cases)
        if self.cs_base:
            # cs_base-anchored single-segment binary: pc is already a
            # cs_base-relative file offset, so the static add recovers the IP.
            default_popped = (
                "            uint16_t popped_ip = "
                f"(uint16_t)((pc + 0x{self.cs_base:04X}) & 0xFFFF);"
            )
        else:
            # pc is a FILE OFFSET (= linear - load_base); invert it back to the
            # live segment's relative IP using the live cs so near_ret_tail
            # recomposes the correct linear address (the same reconstruction
            # handle_ret uses, inverted).
            default_popped = (
                "            uint16_t popped_ip = "
                "(uint16_t)((uint32_t)pc + "
                f"0x{self._load_base:05X}U - ((uint32_t)cs << 4));"
            )
        lines.extend(
            [
                "        default:",
                "        {",
                "            /* pc isn't a case in this binary's switch — most "
                "commonly a cross-binary RET / jmp. near_ret_tail composes "
                "cs:popped_ip into a linear address and resolves the right "
                "binary via file_mappings. Bogus IPs end up in its "
                "unhandled_pc abort path. */",
                default_popped,
                "            near_ret_tail(popped_ip, expected_retip);",
                "            return;",
                "        }",
                "        }",
                "    }",
                "}",
            ]
        )
        return lines

    # Override call handling to avoid dispatch functions.

    def handle_call(self, insn: dict, known_funcs: Set[int]) -> List[str]:
        lines: List[str] = []
        self.flush_pending_dos(lines)
        target = insn.get("target")
        if target is not None:
            bytes_str = insn.get("bytes", "")
            if len(bytes_str) == 6 and target & 0xFFFF0000 == 0xFFFF0000:
                target &= 0xFFFF
        ret_arg = self._call_return_arg(insn)
        if target is None:
            target = _parse_imm(insn["op_str"])

        if target is not None:
            if (
                target in known_funcs
                or target in self.func_names
                or target in self.extern_labels
            ):
                # Faithful near direct call: push the return IP on the emulated
                # stack and set pc to the target. ONE rule for both intra-chunk
                # (target is a case here -> the switch routes to it) and extern
                # (target lives in another chunk -> the `default:` arm exits to
                # the top-level loop, which re-resolves). No C recursion, no
                # per-function wrapper, no expected_retip.
                self._emit_with_comment(lines, insn, "{")
                lines.append("    sp = (sp - 2) & 0xFFFF;")
                lines.append(f"    memw_write(ss, sp, {ret_arg});")
                lines.append(f"    pc = 0x{target:04X};")
                lines.append("    continue;")
                lines.append("}")
            else:
                asm = f"{insn['mnemonic']} {insn.get('op_str', '')}".strip()
                self._emit_unsupported_abort(lines, insn, asm)
            return lines

        op_str = _decode_variables(insn.get("op_str", ""))
        op = op_str.lower().strip()
        if op in {"ax", "bx", "cx", "dx", "si", "di", "bp", "sp"}:
            expr = (
                f"call_table({ret_arg}, (((uint32_t)cs << 4) + (uint16_t)({op})) & 0xFFFFF);"
            )
            self._emit_with_comment(lines, insn, expr)
            return lines

        match = _MEM_PTR_RE.match(op)
        if match:
            mem_refs = insn.get("detail", {}).get("mem_refs", [])
            seg = (
                mem_refs[0].get("segment", "ds").lower() if mem_refs else "ds"
            )
            mem_expr = _rewrite_mem_op(op, seg=seg)
            # A ``call`` mnemonic is ALWAYS a near indirect call (opcode FF /2,
            # ``call word ptr [mem]``): it reads a 16-bit offset, pushes only the
            # near return IP, and leaves cs unchanged. Capstone emits ``lcall``
            # (-> handle_lcall) for genuine far indirect calls (FF /3,
            # ``call dword ptr [mem]``). A preceding ``push cs`` is the game's
            # own ``push cs; call near; retf`` idiom -- it explicitly stacks the
            # return cs so the callee can ``retf``; promoting the near call to a
            # far lcall_table would push a SECOND cs and drift the stack by 2.
            expr = (
                f"call_table({ret_arg}, (((uint32_t)cs << 4) + (uint16_t)({mem_expr}))"
                " & 0xFFFFF);"
            )
            self._emit_with_comment(lines, insn, expr)
            return lines

        asm = f"{insn['mnemonic']} {insn.get('op_str', '')}".strip()
        self._emit_unsupported_abort(lines, insn, asm)
        return lines


def main() -> None:
    args = parse_args()
    ir = json.loads(Path(args.ir).read_text())
    code_bytes = Path(args.exe).read_bytes()
    out_lines: List[str] = []
    out_lines.extend(_emit_struct_include())
    header_path = Path(args.out).with_suffix(".h")
    out_lines.append(f'#include "{header_path.name}"')
    out_lines.append("")

    functions = ir.get("functions", [])
    program_addrs = {
        insn["address"]
        for f in functions
        for insn in f.get("instructions", [])
    }
    known_funcs = {f["start"] for f in functions}
    extern_labels = set(ir.get("extern_labels", []))
    # Filter out any extern labels that fall within the body of a known
    # function.  Some disassembly passes conservatively record internal jump
    # targets as ``extern_labels`` even when they are only referenced from a
    # single routine.  Treat these as normal in-function labels so the
    # corresponding code remains part of the enclosing function instead of
    # being lifted into the dispatcher.
    addr_to_func: Dict[int, int] = {
        insn["address"]: func["start"]
        for func in functions
        for insn in func.get("instructions", [])
    }
    extern_labels = {
        addr
        for addr in extern_labels
        if addr_to_func.get(addr, addr) == addr
    }
    func_map: Dict[int, str] = {}
    comment_map: Dict[int, dict | str] = {}
    cs_base_val: int = 0
    # Default DOS load segment (LOAD_SEG = PSP_SEG + 0x10 = 0x1000 + 0x10). A
    # per-binary "load_segment" in the metadata overrides it so a game that
    # loads its program lower (more conventional RAM free) gets matching
    # absolute segment references in the generated C. Must equal the runtime's
    # LOAD_SEG (game_config.psp_seg + 0x10).
    load_segment_val: int = 0x1010
    if args.metadata:
        meta = json.loads(Path(args.metadata).read_text())
        cs_base_raw = meta.get("cs_base") if isinstance(meta, dict) else None
        if cs_base_raw is not None:
            try:
                cs_base_val = (
                    int(cs_base_raw, 0)
                    if isinstance(cs_base_raw, str)
                    else int(cs_base_raw)
                )
            except (TypeError, ValueError):
                cs_base_val = 0
        load_seg_raw = meta.get("load_segment") if isinstance(meta, dict) else None
        if load_seg_raw is not None:
            try:
                load_segment_val = (
                    int(load_seg_raw, 0)
                    if isinstance(load_seg_raw, str)
                    else int(load_seg_raw)
                )
            except (TypeError, ValueError):
                load_segment_val = 0x1010
        if isinstance(meta, dict) and (
            "functions" in meta or "comments" in meta
        ):
            funcs_meta = meta.get("functions", {})
            comments_meta = meta.get("comments", {})
        else:
            funcs_meta = meta
            comments_meta = {}
        for key, name in funcs_meta.items():
            if key.startswith("func_"):
                key = key[5:]
            elif key.startswith("func"):
                key = key[4:]
            try:
                addr = int(key, 16)
            except ValueError:
                continue
            func_map[addr] = name
        for key, comment in comments_meta.items():
            if key.startswith("0x") or key.startswith("0X"):
                key = key[2:]
            try:
                addr = int(key, 16)
            except ValueError:
                continue
            comment_map[addr] = comment

    # Linear base of file offset 0 in the loaded image. Explicit --image-base
    # wins; otherwise the runtime JIT path exports SAISEI_DISASM_IMAGE_BASE (the
    # dumped segment base = cs<<4), which makes the near-call IP conversion an
    # identity for the chunk. When neither is set (a whole-image decode) the
    # renderer defaults to load_segment<<4 (= the DOS LOAD_SEG paragraph).
    load_base_val: int | None = None
    base_raw = args.image_base
    if base_raw is None:
        base_raw = os.environ.get("SAISEI_DISASM_IMAGE_BASE")
    if base_raw is not None:
        try:
            load_base_val = (
                int(base_raw, 0) if isinstance(base_raw, str) else int(base_raw)
            )
        except (TypeError, ValueError):
            load_base_val = None

    renderer = PCSwitchRenderer(
        code_bytes=code_bytes,
        func_names=func_map,
        comments=comment_map,
        extern_labels=extern_labels,
        name_prefix=args.prefix,
        program_addrs=program_addrs,
        relocations=ir.get("relocations", []),
        cs_base=cs_base_val,
        load_segment=load_segment_val,
        load_base=load_base_val,
    )

    header_lines: List[str] = ["#pragma once", "", "#include <stdint.h>", ""]
    header_lines.append(
        f"void {renderer.dispatch_name}(int pc, uint16_t expected_retip, "
        "const char *file, const char *func, int line);"
    )
    header_lines.append("")
    for func in functions:
        if func["start"] in extern_labels:
            continue
        name = renderer.func_name(func["start"])
        impl = f"{name}_impl"
        header_lines.append(
            f"void {impl}(uint16_t expected_retip, const char *file, "
            "const char *func, int line);"
        )
        header_lines.append("#ifndef SHIMS_DISABLE_MACROS")
        # The short-form macro is only used by hand-written code that wants to
        # invoke a translated function without caring about retIP plumbing.
        # Pass 0 — the callee will not match expected_retip, fall into the
        # tail-dispatch path, and any pushed value will route correctly.
        header_lines.append(
            f"#define {name}(retip) {impl}((retip), __FILE__, __func__, __LINE__)"
        )
        header_lines.append("#endif")
    header_lines.append("")

    # JIT mode (SAISEI_JIT_DROP_DATA, set by jit_compile.py): a function that
    # fails to translate is DATA the per-segment discovery wandered into and
    # decoded as code -- it is not reachable real code. DROP it cleanly (emit
    # nothing, roll back any partial dispatch cases) rather than failing the
    # whole chunk. NOT a stub: the address just isn't a case, so if it ever IS
    # reached at runtime the JIT re-decodes from there. A whole-image decode still
    # hard-fails (a translate failure there is a real bug to fix).
    jit_drop = bool(os.environ.get("SAISEI_JIT_DROP_DATA"))
    wrapper_lines: List[str] = []
    kept_functions: List[dict] = []
    for func in functions:
        snap = len(renderer.dispatch_cases)
        try:
            lines = renderer.render_function(func, known_funcs)
        except UnsupportedInstructionError as e:
            if jit_drop:
                del renderer.dispatch_cases[snap:]
                sys.stderr.write(
                    f"[ir_to_c] dropped data-decoded function @0x"
                    f"{func['start']:X} (hit {e.asm})\n")
                continue
            sys.stderr.write(
                f"\n[ir_to_c FATAL] unsupported instruction blocked translation\n"
                f"  module:       {e.module}\n"
                f"  file_off:     0x{e.addr:X}\n"
                f"  mnemonic:     {e.asm}\n"
                f"  reached from: function {renderer.func_name(func['start'])} "
                f"(start=0x{func['start']:X})\n"
                f"  diagnosis: either implement a translator handler for this\n"
                f"  mnemonic in compiler/ir_to_c.py, or — if this code is\n"
                f"  unreachable garbage from a bogus extra_entry — remove the\n"
                f"  entry whose disassembly walks into this offset.\n"
            )
            sys.exit(2)
        kept_functions.append(func)
        if func["start"] in extern_labels:
            continue
        wrapper_lines.extend(lines)
        wrapper_lines.append("")
    functions = kept_functions

    dispatch_lines = renderer.render_dispatch()
    out_lines.extend(dispatch_lines)
    out_lines.append("")
    out_lines.extend(wrapper_lines)

    Path(args.out).write_text("\n".join(out_lines))
    header_path.write_text("\n".join(header_lines))


if __name__ == "__main__":
    main()
