# flake8: noqa
import sys
from pathlib import Path

sys.path.append(str(Path(__file__).resolve().parents[1]))

from compiler.ir_to_c import CCodeRenderer  # noqa: E402


def render_function(func, known):
    return CCodeRenderer().render_function(func, known)


def test_adc_adds_with_carry():
    func = {
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "adc", "op_str": "ax, bx", "bytes": ""},
            {"address": 0x0002, "mnemonic": "ret", "op_str": "", "bytes": ""},
        ],
    }
    lines = render_function(func, set())
    src = "\n".join(lines)
    assert "uint32_t old = ax;" in src
    assert "uint32_t src = bx;" in src
    assert "uint32_t tmp = old + src + CF;" in src
    assert "CF = tmp > 0xFFFF;" in src
    assert "ax = tmp & 0xFFFF;" in src
    assert "OF = (~(old ^ src) & (old ^ tmp) & 0x8000) != 0;" in src
    assert "// TODO ASM: adc" not in src

