import sys
from pathlib import Path

# flake8: noqa
sys.path.append(str(Path(__file__).resolve().parents[1]))

from compiler.ir_to_c import CCodeRenderer  # noqa: E402


def render_function(func, known):
    return CCodeRenderer().render_function(func, known)


def test_in_instruction_is_rendered():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "in",
                "op_str": "al, 0x60",
                "bytes": "E460",
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
    assert "al = inb(0x60);" in src
    assert "// TODO ASM: in al, 0x60" not in src


def test_out_instruction_is_rendered():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "mov",
                "op_str": "al, 1",
                "bytes": "B001",
            },
            {
                "address": 0x0002,
                "mnemonic": "out",
                "op_str": "0x61, al",
                "bytes": "E661",
            },
            {
                "address": 0x0004,
                "mnemonic": "ret",
                "op_str": "",
                "bytes": "C3",
            },
        ],
    }
    lines = render_function(func, set())
    src = "\n".join(lines)
    assert "outb(0x61, al);" in src
    assert "// TODO ASM: out 0x61, al" not in src
