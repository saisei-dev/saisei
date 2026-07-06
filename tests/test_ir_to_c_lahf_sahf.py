import sys
from pathlib import Path

# flake8: noqa
sys.path.append(str(Path(__file__).resolve().parents[1]))

from compiler.ir_to_c import CCodeRenderer  # noqa: E402


def render_function(func, known):
    return CCodeRenderer().render_function(func, known)


def test_lahf_sahf_translate_flags():
    func = {
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "lahf", "op_str": "", "bytes": ""},
            {"address": 0x0001, "mnemonic": "sahf", "op_str": "", "bytes": ""},
            {"address": 0x0002, "mnemonic": "ret", "op_str": "", "bytes": ""},
        ],
    }
    src = "\n".join(render_function(func, set()))
    assert (
        "ah = (uint8_t)((SF << 7) | (ZF << 6) | (PF << 2) | 0x02 | CF);" in src
    )
    assert "SF = (ah >> 7) & 1;" in src
    assert "ZF = (ah >> 6) & 1;" in src
    assert "PF = (ah >> 2) & 1;" in src
    assert "CF = ah & 1;" in src
    assert "// TODO ASM: lahf" not in src
    assert "// TODO ASM: sahf" not in src
