import sys
from pathlib import Path

sys.path.append(str(Path(__file__).resolve().parents[1]))

from compiler.ir_to_c import CCodeRenderer  # noqa: E402


def render_function(func, known):
    return CCodeRenderer().render_function(func, known)


def test_stc_followed_by_jb_checks_carry_flag():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "stc",
                "op_str": "",
                "bytes": "F9",
            },
            {
                "address": 0x0001,
                "mnemonic": "jb",
                "op_str": "0000",
                "bytes": "7200",
            },
            {
                "address": 0x0003,
                "mnemonic": "ret",
                "op_str": "",
                "bytes": "C3",
            },
        ],
    }
    lines = render_function(func, set())
    src = "\n".join(lines)
    assert "do {" in src
    assert "while (CF == 1)" in src


def test_clc_followed_by_jnb_checks_carry_flag():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "clc",
                "op_str": "",
                "bytes": "F8",
            },
            {
                "address": 0x0001,
                "mnemonic": "jnb",
                "op_str": "0000",
                "bytes": "7300",
            },
            {
                "address": 0x0003,
                "mnemonic": "ret",
                "op_str": "",
                "bytes": "C3",
            },
        ],
    }
    lines = render_function(func, set())
    src = "\n".join(lines)
    assert "do {" in src
    assert "while (CF == 0)" in src
