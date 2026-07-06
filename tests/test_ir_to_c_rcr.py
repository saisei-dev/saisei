import sys
from pathlib import Path

# flake8: noqa
sys.path.append(str(Path(__file__).resolve().parents[1]))

from compiler.ir_to_c import CCodeRenderer  # noqa: E402


def render_function(func, known):
    return CCodeRenderer().render_function(func, known)


def test_rcr_register_translate():
    func = {
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "rcr", "op_str": "ax, 1", "bytes": ""},
            {"address": 0x0002, "mnemonic": "ret", "op_str": "", "bytes": ""},
        ],
    }
    src = "\n".join(render_function(func, set()))
    assert "unsigned count = (1 & 0x1F) % 17;" in src
    assert "unsigned orig_count = count;" in src
    assert "unsigned new_cf = ax & 1;" in src
    assert "ax = ((ax >> 1) | (CF << 15)) & 0xFFFF;" in src
    assert "ZF = ax == 0;" not in src
    assert "SF = (ax >> 15) & 1;" not in src
    assert "PF = parity8((uint8_t)ax);" not in src
    assert "OF = ((ax >> 15) & 1) ^ ((ax >> 14) & 1);" in src
    assert "OF = 0;" not in src
    assert "// TODO ASM: rcr" not in src
