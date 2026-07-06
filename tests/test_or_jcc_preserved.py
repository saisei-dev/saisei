import sys
from pathlib import Path

sys.path.append(str(Path(__file__).resolve().parents[1]))

from compiler.ir_to_c import CCodeRenderer  # noqa: E402


def render_function(func):
    return CCodeRenderer().render_function(func, set())


def test_or_followed_by_jcc_preserved():
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
                "mnemonic": "je",
                "op_str": "0005",
                "bytes": "7401",
            },
            {
                "address": 0x0004,
                "mnemonic": "ret",
                "op_str": "",
                "bytes": "C3",
            },
            {
                "address": 0x0005,
                "mnemonic": "ret",
                "op_str": "",
                "bytes": "C3",
            },
        ],
    }
    src = "\n".join(render_function(func))
    assert "ax = (ax | bx) & 0xFFFF;" in src
    assert "if" in src and "// TODO ASM: je" not in src
