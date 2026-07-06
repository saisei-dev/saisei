import sys
from pathlib import Path

import pytest

sys.path.append(str(Path(__file__).resolve().parents[1]))

from compiler.ir_to_c import (  # noqa: E402
    CCodeRenderer,
    UnsupportedInstructionError,
)


def render_function(func, known):
    return CCodeRenderer().render_function(func, known)


def test_repe_cmpsb_translates_faithful_inline():
    # repe cmpsb is emitted as an explicit inline compare-while-equal loop:
    # it steps si/di by delta, stops on the first mismatch (ZF->0 ends repe) or
    # cx exhaustion, sets cx to the remaining count, and computes the flags from
    # the LAST byte pair compared (the faithful CMPSB effect).
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "repe cmpsb",
                "op_str": "byte ptr [si], byte ptr es:[di]",
                "bytes": "F3A6",
            },
            {
                "address": 0x0002,
                "mnemonic": "ret",
                "op_str": "",
                "bytes": "C3",
            },
        ],
    }
    lines = render_function(func, set())
    src = "\n".join(lines)
    assert "int delta = DF ? -1 : 1;" in src
    assert "lb = memb(ds, si);" in src
    assert "rb = memb(es, di);" in src
    assert "if (lb != rb) break;" in src        # ZF->0 ends repe
    assert "cx = (count - i) & 0xFFFF;" in src   # remaining count
    assert "ZF = res == 0;" in src
    assert "CF = lb < rb;" in src


def test_repe_scasb_and_cmpsw_translate():
    # ``repe cmpsb``, ``repe cmpsw`` and ``repe scasb`` all have dedicated
    # handlers now (scan/compare-while-equal). They must translate, not abort.
    for mnem, b in (("repe cmpsw", "F3A7"), ("repe scasb", "F3AE")):
        func = {
            "start": 0x0000,
            "instructions": [
                {"address": 0x0000, "mnemonic": mnem,
                 "op_str": "byte ptr es:[di]", "bytes": b},
                {"address": 0x0002, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
            ],
        }
        body = "\n".join(render_function(func, set()))
        assert "abort" not in body.lower()


def test_other_repe_instructions_abort_translation():
    # repe cmpsb/cmpsw/scasb have dedicated handlers; other repe-prefixed
    # string ops (e.g. ``repe scasw``) still hard-abort rather than emit
    # partial code.
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "repe scasw",
                "op_str": "word ptr es:[di]",
                "bytes": "F3AF",
            },
            {
                "address": 0x0002,
                "mnemonic": "ret",
                "op_str": "",
                "bytes": "C3",
            },
        ],
    }
    with pytest.raises(UnsupportedInstructionError) as exc:
        render_function(func, set())
    assert "scasw" in str(exc.value)
