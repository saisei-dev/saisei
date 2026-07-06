import sys
from pathlib import Path

sys.path.append(str(Path(__file__).resolve().parents[1]))

from compiler.ir_to_c import CCodeRenderer  # noqa: E402


def render(func):
    return "\n".join(CCodeRenderer().render_function(func, set()))


def test_stick_style_loop():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "lodsb",
                "op_str": "",
                "bytes": "AC",
            },
            {
                "address": 0x0001,
                "mnemonic": "dec",
                "op_str": "dx",
                "bytes": "4A",
            },
            {
                "address": 0x0002,
                "mnemonic": "cmp",
                "op_str": "al, 0xff",
                "bytes": "3CFF",
            },
            {
                "address": 0x0004,
                "mnemonic": "jne",
                "op_str": "0x9",
                "bytes": "7503",
            },
            {
                "address": 0x0006,
                "mnemonic": "ret",
                "op_str": "",
                "bytes": "C3",
            },
            {
                "address": 0x0007,
                "mnemonic": "inc",
                "op_str": "si",
                "bytes": "46",
            },
            {
                "address": 0x0008,
                "mnemonic": "dec",
                "op_str": "dx",
                "bytes": "4A",
            },
            {
                "address": 0x0009,
                "mnemonic": "jmp",
                "op_str": "0x0",
                "bytes": "E9F5FF",
            },
        ],
    }
    src = render(func)
    assert "if (ZF != 0)" in src
    assert "int delta = DF ? -1 : 1;" in src
    assert src.count("si = (si + delta) & 0xFFFF;") == 1
    assert src.count("si = (si + 1) & 0xFFFF;") == 1
    assert "while (1)" in src
