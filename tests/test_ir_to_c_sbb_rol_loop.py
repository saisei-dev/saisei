import sys
from pathlib import Path

# flake8: noqa
sys.path.append(str(Path(__file__).resolve().parents[1]))

from compiler.ir_to_c import CCodeRenderer  # noqa: E402


def render_function(func, known):
    return CCodeRenderer().render_function(func, known)


def test_rol_translates_to_rotate_left():
    func = {
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "rol", "op_str": "al, 1", "bytes": ""},
            {"address": 0x0002, "mnemonic": "ret", "op_str": "", "bytes": ""},
        ],
    }
    lines = render_function(func, set())
    src = "\n".join(lines)
    assert "unsigned count = 1 & 7;" in src
    assert "if (count) {" in src
    assert "al = (al << count) | (al >> (8 - count));" in src
    assert "CF = al & 1;" in src
    assert "OF = ((al >> 7) & 1) ^ CF;" in src
    assert "ZF = al == 0;" not in src
    assert "PF = parity8((uint8_t)al);" not in src
    assert "// TODO ASM: rol" not in src


def test_sbb_subtracts_with_carry():
    func = {
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "sbb", "op_str": "al, 0x1", "bytes": ""},
            {"address": 0x0002, "mnemonic": "ret", "op_str": "", "bytes": ""},
        ],
    }
    lines = render_function(func, set())
    src = "\n".join(lines)
    assert "uint32_t old = al;" in src
    assert "uint32_t src = 0x1 + CF;" in src
    assert "uint32_t tmp = old - src;" in src
    assert "CF = old < src;" in src
    assert "al = tmp & 0xFF;" in src
    assert "OF = ((old ^ src) & (old ^ tmp) & 0x80) != 0;" in src
    assert "// TODO ASM: sbb" not in src


def test_loopne_decrements_cx_and_branches():
    func = {
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "loopne", "op_str": "0x0010", "bytes": ""},
            {"address": 0x0002, "mnemonic": "ret", "op_str": "", "bytes": ""},
        ],
    }
    lines = CCodeRenderer().render_function(func, set())
    src = "\n".join(lines)
    assert "cx--;" in src
    assert "pc = 0x0010;" in src
    assert "continue;" in src
    assert "goto" not in src
    assert "// TODO ASM: loopne" not in src
