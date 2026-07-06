import sys
from pathlib import Path

sys.path.append(str(Path(__file__).resolve().parents[1]))

from compiler.ir_to_c import CCodeRenderer  # noqa: E402


def render_function(func, known):
    return CCodeRenderer().render_function(func, known)


def test_cwde_sign_extends_al_into_ax():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "mov",
                "op_str": "al, 0x80",
                "bytes": "B080",
            },
            {
                "address": 0x0002,
                "mnemonic": "cwde",
                "op_str": "",
                "bytes": "98",
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
    assert "ax = ((int8_t)al) & 0xFFFF;" in src
    assert "// TODO ASM: cwde" not in src


def test_stc_sets_carry_flag():
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
                "mnemonic": "ret",
                "op_str": "",
                "bytes": "C3",
            },
        ],
    }
    lines = render_function(func, set())
    src = "\n".join(lines)
    assert "CF = 1;" in src
    assert "// TODO ASM: stc" not in src


def test_clc_clears_carry_flag():
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
                "mnemonic": "ret",
                "op_str": "",
                "bytes": "C3",
            },
        ],
    }
    lines = render_function(func, set())
    src = "\n".join(lines)
    assert "CF = 0;" in src
    assert "// TODO ASM: clc" not in src


def test_cmc_complements_carry_flag():
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
                "mnemonic": "cmc",
                "op_str": "",
                "bytes": "F5",
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
    assert "CF ^= 1;" in src
    assert "// TODO ASM: cmc" not in src


def test_cld_clears_direction_flag():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "cld",
                "op_str": "",
                "bytes": "FC",
            },
            {
                "address": 0x0001,
                "mnemonic": "ret",
                "op_str": "",
                "bytes": "C3",
            },
        ],
    }
    lines = render_function(func, set())
    src = "\n".join(lines)
    assert "DF = 0;" in src
    assert "// TODO ASM: cld" not in src


def test_std_sets_direction_flag():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "std",
                "op_str": "",
                "bytes": "FD",
            },
            {
                "address": 0x0001,
                "mnemonic": "ret",
                "op_str": "",
                "bytes": "C3",
            },
        ],
    }
    lines = render_function(func, set())
    src = "\n".join(lines)
    assert "DF = 1;" in src
    assert "// TODO ASM: std" not in src
