import sys
from pathlib import Path

# flake8: noqa
sys.path.append(str(Path(__file__).resolve().parents[1]))

from compiler.ir_to_c import CCodeRenderer  # noqa: E402


def render_function(func, known):
    return CCodeRenderer().render_function(func, known)


def test_ror_translates_to_rotate_right():
    func = {
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "ror", "op_str": "al, 1", "bytes": ""},
            {"address": 0x0002, "mnemonic": "ret", "op_str": "", "bytes": ""},
        ],
    }
    lines = render_function(func, set())
    src = "\n".join(lines)
    assert "unsigned count = 1 & 7;" in src
    assert "if (count) {" in src
    assert "al = (al >> count) | (al << (8 - count));" in src
    assert "CF = al >> 7;" in src
    assert "OF = ((al >> 7) & 1) ^ ((al >> 6) & 1);" in src
    assert "ZF = al == 0;" not in src
    assert "PF = parity8((uint8_t)al);" not in src
    assert "// TODO ASM: ror" not in src
