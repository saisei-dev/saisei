"""Contract test for the translator <-> runtime ABI seam.

runtime/include/runtime_abi.h declares the fixed surface that generated
code (build/*.c) may call into the runtime through. This test fails if
the translator (compiler/ir_to_c.py) emits a call to anything NOT listed in
that manifest -- i.e. it catches drift between the two sides of the seam
before it reaches a (much later) link error.
"""

import re
from pathlib import Path
import sys

sys.path.append(str(Path(__file__).resolve().parents[1]))

from compiler.ir_to_c import CCodeRenderer  # noqa: E402

REPO = Path(__file__).resolve().parents[1]
ABI_HEADER = REPO / "runtime" / "include" / "runtime_abi.h"

# Identifiers that look like calls but are C control flow, libc, or names
# the generated code defines itself -- none of these are part of the
# runtime ABI surface and must not be required to appear in the manifest.
_C_KEYWORDS = {
    "if", "while", "for", "switch", "sizeof", "return", "do", "else",
}
_LIBC = {
    "memcpy", "memset", "memmove", "printf", "fprintf", "snprintf",
    "strlen", "strcmp", "abort", "exit", "malloc", "free",
}

# Symbols the translator coins for the binary under translation take the form
# "<module>_func_<ip>" / "<module>_dispatch" / "<module>_impl" (module = the
# binary's basename). Calls to these are generated-code -> generated-code, not
# part of the runtime ABI surface.
_GEN_PREFIX = re.compile(
    r"^[A-Za-z][A-Za-z0-9]*_(func_[0-9A-Fa-f]+|dispatch|impl)"
)


def _strip_comments(src: str) -> str:
    src = re.sub(r"/\*.*?\*/", " ", src, flags=re.S)
    src = re.sub(r"//[^\n]*", " ", src)
    return src


def load_abi_manifest() -> set[str]:
    text = ABI_HEADER.read_text(encoding="utf-8")
    m = re.search(
        r"RUNTIME_ABI_SYMBOLS_BEGIN(.*?)RUNTIME_ABI_SYMBOLS_END",
        text,
        re.S,
    )
    assert m, "manifest markers missing from runtime_abi.h"
    body = m.group(1)
    # Strip comment leaders and inline '--- section' annotations.
    body = re.sub(r"^\s*\*", " ", body, flags=re.M)
    body = re.sub(r"---[^\n]*", " ", body)
    return {tok for tok in body.split() if re.fullmatch(r"[A-Za-z_]\w*", tok)}


def _runtime_calls(src: str) -> set[str]:
    """Runtime symbols *called* in rendered C, minus locally-defined and
    library/keyword names."""
    src = _strip_comments(src)
    called = set(re.findall(r"\b([A-Za-z_]\w*)\s*\(", src))
    local = set(re.findall(r"\bvoid\s+([A-Za-z_]\w*)\s*\(", src))
    out = set()
    for name in called:
        if name in _C_KEYWORDS or name in _LIBC or name in local:
            continue
        # Names the translator coins for the binary under translation.
        if name.endswith("_impl") or "_func_" in name:
            continue
        if name.endswith("_dispatch") or name == "dispatch":
            continue
        if name.startswith("func_") or _GEN_PREFIX.match(name):
            continue
        out.add(name)
    return out


def _render_broad_spread() -> str:
    instrs = [
        ("mov", "ah, 0x30"), ("int", "21"), ("int", "1a"),
        ("cmp", "al, 2"), ("jb", "0010"), ("add", "ax, bx"),
        ("xor", "ax, ax"), ("xor", "al, al"), ("mov", "ax, bx"),
        ("rep movsb", "byte ptr es:[di], byte ptr ds:[si]"),
        ("rep movsw", "word ptr es:[di], word ptr ds:[si]"),
        ("rep stosb", "byte ptr es:[di]"), ("jmp", "ax"),
        ("in", "al, dx"), ("out", "dx, al"),
        ("ret", ""), ("retf", ""), ("iret", ""),
    ]
    func = {
        "start": 0x0000,
        "instructions": [
            {"address": i * 2, "mnemonic": m, "op_str": o, "bytes": "90"}
            for i, (m, o) in enumerate(instrs)
        ],
    }
    return "\n".join(
        CCodeRenderer(name_prefix="app_").render_function(func, set())
    )


def test_translator_emits_only_declared_abi():
    allowed = load_abi_manifest()
    emitted = _runtime_calls(_render_broad_spread())
    unexpected = emitted - allowed
    assert not unexpected, (
        "translator emits runtime calls not declared in runtime_abi.h: "
        f"{sorted(unexpected)}. Either add them to the manifest (and "
        "shims.h) on purpose, or they are a bug."
    )


def test_generated_artifacts_respect_abi_if_present():
    """Stronger check when a build exists: every runtime symbol used by the
    real generated C must be in the contract. Skipped in environments
    (e.g. CI lint/test stage) where artifacts have not been built."""
    gen = sorted((REPO / "build").glob("*.c"))
    if not gen:
        import pytest

        pytest.skip("no generated artifacts present")
    allowed = load_abi_manifest()
    offenders: dict[str, set[str]] = {}
    for f in gen:
        unexpected = _runtime_calls(f.read_text(errors="ignore")) - allowed
        if unexpected:
            offenders[f.name] = unexpected
    assert not offenders, (
        f"artifacts call undeclared runtime symbols: {offenders}"
    )


def test_shims_includes_runtime_abi():
    shims_h = (REPO / "runtime" / "include" / "shims.h").read_text()
    assert '#include "runtime_abi.h"' in shims_h
