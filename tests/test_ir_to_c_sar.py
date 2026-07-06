import sys
from pathlib import Path

sys.path.append(str(Path(__file__).resolve().parents[1]))

from compiler.ir_to_c import CCodeRenderer  # noqa: E402


def render_function(func, known):
    return CCodeRenderer().render_function(func, known)


def test_sar_uses_arithmetic_shift():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "sar",
                "op_str": "al, 1",
                "bytes": "D0F8",
            },
            {
                "address": 0x0002,
                "mnemonic": "sar",
                "op_str": "ax, 1",
                "bytes": "D1F8",
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
    assert src.count("unsigned count = 1 & 0x1F;") == 2
    assert src.count("unsigned orig_count = count;") == 2
    assert "int8_t tmp = (int8_t)al;" in src
    assert "int16_t tmp = (int16_t)ax;" in src
    assert src.count("CF = tmp & 1;") == 2
    assert "al = (uint8_t)tmp;" in src
    assert "ax = (uint16_t)tmp;" in src
    assert src.count("OF = 0;") == 2
