import sys
from pathlib import Path

sys.path.append(str(Path(__file__).resolve().parents[1]))

from compiler.ir_to_c import CCodeRenderer  # noqa: E402


def render_function(func, known):
    return CCodeRenderer().render_function(func, known)


def test_loop_with_conditional_break():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "lodsb",
                "op_str": "",
                "bytes": "AC",
            },
            {
                "address": 0x0001,
                "mnemonic": "cmp",
                "op_str": "al, 0x2e",
                "bytes": "3C2E",
            },
            {
                "address": 0x0003,
                "mnemonic": "je",
                "op_str": "000A",
                "bytes": "7405",
            },
            {
                "address": 0x0005,
                "mnemonic": "inc",
                "op_str": "si",
                "bytes": "46",
            },
            {
                "address": 0x0006,
                "mnemonic": "loop",
                "op_str": "0000",
                "bytes": "E2F8",
            },
            {
                "address": 0x0008,
                "mnemonic": "ret",
                "op_str": "",
                "bytes": "C3",
            },
            {
                "address": 0x000A,
                "mnemonic": "ret",
                "op_str": "",
                "bytes": "C3",
            },
        ],
    }
    lines = render_function(func, set())
    src = "\n".join(lines)
    assert "return;" in src
    assert "while (--cx != 0)" in src or "do {" in src


def test_loop_conditional_return():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "nop",
                "op_str": "",
                "bytes": "90",
            },
            {
                "address": 0x0001,
                "mnemonic": "jmp",
                "op_str": "0006",
                "bytes": "E90400",
            },
            {
                "address": 0x0006,
                "mnemonic": "cmp",
                "op_str": "cx, 3",
                "bytes": "83F9",
            },
            {
                "address": 0x0008,
                "mnemonic": "jne",
                "op_str": "000E",
                "bytes": "7504",
            },
            {
                "address": 0x000A,
                "mnemonic": "ret",
                "op_str": "",
                "bytes": "C3",
            },
            {
                "address": 0x000E,
                "mnemonic": "dec",
                "op_str": "cx",
                "bytes": "49",
            },
            {
                "address": 0x000F,
                "mnemonic": "jmp",
                "op_str": "0000",
                "bytes": "E9F1FF",
            },
        ],
    }
    lines = render_function(func, set())
    src = "\n".join(lines)
    assert "if" in src and "return;" in src
    assert "// TODO ASM: jne" not in src


def test_xor_self_clears_with_flags():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "xor",
                "op_str": "ah, ah",
                "bytes": "32E4",
            },
            {
                "address": 0x0002,
                "mnemonic": "ret",
                "op_str": "",
                "bytes": "C3",
            },
        ],
    }
    src = "\n".join(render_function(func, set()))
    assert "xor8(&ah, ah);" in src
    assert "ZF" not in src


def test_loop_invalidates_cx_before_dos_int():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "mov",
                "op_str": "cx, 5",
                "bytes": "B90500",
            },
            {
                "address": 0x0003,
                "mnemonic": "loop",
                "op_str": "0x3",
                "bytes": "E2FE",
            },
            {
                "address": 0x0005,
                "mnemonic": "mov",
                "op_str": "ah, 0x40",
                "bytes": "B440",
            },
            {
                "address": 0x0007,
                "mnemonic": "mov",
                "op_str": "bx, 1",
                "bytes": "BB0100",
            },
            {
                "address": 0x000A,
                "mnemonic": "mov",
                "op_str": "dx, 0x1000",
                "bytes": "BA0010",
            },
            {
                "address": 0x000D,
                "mnemonic": "int",
                "op_str": "21",
                "bytes": "CD21",
            },
            {
                "address": 0x000F,
                "mnemonic": "ret",
                "op_str": "",
                "bytes": "C3",
            },
        ],
    }
    src = "\n".join(render_function(func, set()))
    assert "cx = 5;" in src
    assert "cx--;" in src
    assert "dos_write_file(0x0001, (const void *)seg_off(cs, 0x1000), cx);" in src
