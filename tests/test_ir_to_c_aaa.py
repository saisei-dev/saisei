import sys
from pathlib import Path

# flake8: noqa
sys.path.append(str(Path(__file__).resolve().parents[1]))

from compiler.ir_to_c import CCodeRenderer  # noqa: E402


def test_aaa_translates_adjust():
    func = {
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "aaa", "op_str": "", "bytes": ""},
            {"address": 0x0001, "mnemonic": "ret", "op_str": "", "bytes": ""},
        ],
    }
    src = "\n".join(CCodeRenderer().render_function(func, set()))
    assert "uint8_t tmp = al;" in src
    assert "(tmp & 0x10)" not in src
    assert "al = (uint8_t)((tmp + 6) & 0x0F);" in src
    assert "ah = (uint8_t)((ah + 1) & 0xFF);" in src
    assert "CF = 1;" in src
    assert "al = tmp & 0x0F;" in src
    assert "CF = 0;" in src
    assert "// TODO ASM: aaa" not in src
