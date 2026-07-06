import sys
from pathlib import Path

sys.path.append(str(Path(__file__).resolve().parents[1]))

from compiler.ir_to_c import CCodeRenderer, normalize_flags  # noqa: E402


def render(func):
    return "\n".join(CCodeRenderer().render_function(func, set()))


def test_or_not_skipped_after_popf():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "or",
                "op_str": "ax, bx",
                "bytes": "0BC3",
            },
            {
                "address": 0x0002,
                "mnemonic": "push",
                "op_str": "ax",
                "bytes": "50",
            },
            {
                "address": 0x0003,
                "mnemonic": "popf",
                "op_str": "",
                "bytes": "9D",
            },
            {
                "address": 0x0004,
                "mnemonic": "ret",
                "op_str": "",
                "bytes": "C3",
            },
            {
                "address": 0x0010,
                "mnemonic": "loop",
                "op_str": "0x0010",
                "bytes": "E2FE",
            },
        ],
    }
    src = render(func)
    assert "ax = (ax | bx) & 0xFFFF;" in src


def test_readwrite_mem_access_clobbers_cmp_before_jcc():
    instrs = [
        {
            "address": 0x0000,
            "mnemonic": "cmp",
            "op_str": "ax, word ptr [bx]",
            "bytes": "3B07",
            "detail": {
                "mem_refs": [
                    {
                        "segment": "DS",
                        "base": "BX",
                        "index": "",
                        "scale": 1,
                        "disp": 0,
                        "access": "read",
                    }
                ]
            },
        },
        {
            "address": 0x0002,
            "mnemonic": "xchg",
            "op_str": "word ptr [bx], ax",
            "bytes": "8707",
            "detail": {
                "mem_refs": [
                    {
                        "segment": "DS",
                        "base": "BX",
                        "index": "",
                        "scale": 1,
                        "disp": 0,
                        "access": "readwrite",
                    }
                ]
            },
        },
        {
            "address": 0x0004,
            "mnemonic": "jnz",
            "op_str": "0x10",
            "bytes": "750A",
        },
    ]
    normalized = normalize_flags(instrs)
    assert "cond_prev" not in normalized[-1]
