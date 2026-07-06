import sys
from pathlib import Path

sys.path.append(str(Path(__file__).resolve().parents[1]))

from compiler.ir_to_c import CCodeRenderer  # noqa: E402


def render_function(func, known):
    return CCodeRenderer().render_function(func, known)


def test_and_followed_by_lodsb_and_jcc():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "and",
                "op_str": "al, 0x5f",
                "bytes": "245f",
            },
            {
                "address": 0x0002,
                "mnemonic": "lodsb",
                "op_str": "",
                "bytes": "AC",
            },
            {
                "address": 0x0003,
                "mnemonic": "je",
                "op_str": "0008",
                "bytes": "7403",
            },
            {
                "address": 0x0005,
                "mnemonic": "ret",
                "op_str": "",
                "bytes": "C3",
            },
            {
                "address": 0x0008,
                "mnemonic": "ret",
                "op_str": "",
                "bytes": "C3",
            },
        ],
    }
    lines = render_function(func, set())
    src = "\n".join(lines)
    assert "al = (al & 0x5f) & 0xFF;" in src
    assert "if" in src and "// TODO ASM: je" not in src
