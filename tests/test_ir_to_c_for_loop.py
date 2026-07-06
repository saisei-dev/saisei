import sys
from pathlib import Path

# flake8: noqa
sys.path.append(str(Path(__file__).resolve().parents[1]))

from compiler.ir_to_c import CCodeRenderer  # noqa: E402


def render_function(func, known, name_prefix=""):
    return CCodeRenderer(name_prefix=name_prefix).render_function(func, known)


def test_loop_with_jcxz_is_structured_as_for_loop():
    func = {
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "mov", "op_str": "cx, 3", "bytes": "B90300"},
            {"address": 0x0003, "mnemonic": "jcxz", "op_str": "0x9", "bytes": "E304"},
            {"address": 0x0005, "mnemonic": "int", "op_str": "0x10", "bytes": "CD10"},
            {"address": 0x0007, "mnemonic": "loop", "op_str": "0x5", "bytes": "E2FC"},
            {"address": 0x0009, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    }
    lines = render_function(func, set())
    src = "\n".join(lines)
    assert "for (cx = 3; cx != 0; cx--)" in src
    assert "cx--" in src
    assert "while" not in src
def test_simple_for_loop_is_structured():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "mov",
                "op_str": "cx, 0",
                "bytes": "B90000",
            },
            {
                "address": 0x0004,
                "mnemonic": "jmp",
                "op_str": "0x6",
                "bytes": "E90200",
            },
            {
                "address": 0x0006,
                "mnemonic": "jnz",
                "op_str": "0x12",
                "bytes": "750A",
            },
            {
                "address": 0x0008,
                "mnemonic": "call",
                "op_str": "0x1000",
                "bytes": "E80000",
            },
            {
                "address": 0x000C,
                "mnemonic": "inc",
                "op_str": "cx",
                "bytes": "41",
            },
            {
                "address": 0x000E,
                "mnemonic": "jmp",
                "op_str": "0x6",
                "bytes": "E9F9FF",
            },
            {
                "address": 0x0012,
                "mnemonic": "ret",
                "op_str": "",
                "bytes": "C3",
            },
        ],
    }
    known = {0x0000, 0x1000}
    lines = render_function(func, known, name_prefix="g_")
    src = "\n".join(lines)
    assert "for (cx = 0; ZF != 0; cx++)" in src
    assert "while (" not in src
    assert "pc = 0x1000;" in src


def test_basic_arithmetic_is_translated_to_c():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "mov",
                "op_str": "ax, bx",
                "bytes": "89D8",
            },
            {
                "address": 0x0002,
                "mnemonic": "add",
                "op_str": "ax, 1",
                "bytes": "83C001",
            },
            {
                "address": 0x0005,
                "mnemonic": "sub",
                "op_str": "ax, 2",
                "bytes": "83E002",
            },
            {
                "address": 0x0008,
                "mnemonic": "inc",
                "op_str": "ax",
                "bytes": "40",
            },
            {
                "address": 0x0009,
                "mnemonic": "dec",
                "op_str": "ax",
                "bytes": "48",
            },
            {
                "address": 0x000A,
                "mnemonic": "xor",
                "op_str": "ax, ax",
                "bytes": "31C0",
            },
            {
                "address": 0x000C,
                "mnemonic": "ret",
                "op_str": "",
                "bytes": "C3",
            },
        ],
    }
    lines = render_function(func, set())
    src = "\n".join(lines)
    assert "ax = bx;" in src
    assert "uint32_t old = ax;" in src
    assert "uint32_t src = 1;" in src
    assert "uint32_t tmp = old + src;" in src
    assert "ax = tmp & 0xFFFF;" in src
    assert "uint32_t src = 2;" in src
    assert "uint32_t tmp = old - src;" in src
    assert "CF = old < src;" in src
    assert "ax = (ax + 1) & 0xFFFF;" in src
    assert "ax = (ax - 1) & 0xFFFF;" in src
    assert "xor16(&ax, ax);" in src
    assert "// TODO ASM: mov ax, bx" not in src


def test_cmp_followed_by_jcc_uses_high_level_condition():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "cmp",
                "op_str": "al, 2",
                "bytes": "3C02",
            },
            {
                "address": 0x0002,
                "mnemonic": "jnc",
                "op_str": "0x6",
                "bytes": "7302",
            },
            {
                "address": 0x0004,
                "mnemonic": "int",
                "op_str": "0x20",
                "bytes": "CD20",
            },
            {
                "address": 0x0006,
                "mnemonic": "ret",
                "op_str": "",
                "bytes": "C3",
            },
        ],
    }
    lines = render_function(func, set())
    src = "\n".join(lines)
    assert "if (CF == 0)" in src
    assert "dos_exit();" in src


def test_dos_call_before_conditional_jump_is_preserved():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "mov",
                "op_str": "dx, 7E2h",
                "bytes": "BAE207",
            },
            {
                "address": 0x0003,
                "mnemonic": "mov",
                "op_str": "ax, 3D00h",
                "bytes": "B8003D",
            },
            {
                "address": 0x0006,
                "mnemonic": "int",
                "op_str": "21",
                "bytes": "CD21",
            },
            {
                "address": 0x0008,
                "mnemonic": "jnc",
                "op_str": "000C",
                "bytes": "7302",
            },
            {
                "address": 0x000A,
                "mnemonic": "ret",
                "op_str": "",
                "bytes": "C3",
            },
            {
                "address": 0x000C,
                "mnemonic": "ret",
                "op_str": "",
                "bytes": "C3",
            },
        ],
    }
    lines = render_function(func, set())
    src = "\n".join(lines)
    assert "CF = dos_open_file(" in src
    assert "if (CF == 0)" in src


def test_for_loop_with_cmp_condition():
    func = {
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "mov", "op_str": "cx, 0", "bytes": "B90000"},
            {"address": 0x0003, "mnemonic": "jmp", "op_str": "0x8", "bytes": "E904"},
            {"address": 0x0008, "mnemonic": "cmp", "op_str": "cx, 3", "bytes": "83F9"},
            {"address": 0x000A, "mnemonic": "jge", "op_str": "0x15", "bytes": "7D09"},
            {"address": 0x000C, "mnemonic": "mov", "op_str": "ax, bx", "bytes": "89D8"},
            {"address": 0x000E, "mnemonic": "inc", "op_str": "cx", "bytes": "41"},
            {"address": 0x000F, "mnemonic": "jmp", "op_str": "0x8", "bytes": "E9F8FF"},
            {"address": 0x0015, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    }
    lines = render_function(func, set())
    src = "\n".join(lines)
    assert "for (cx = 0; SF != OF; cx++)" in src
    assert "ax = bx;" in src


def test_loop_with_memory_step_rendered_as_while():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "mov",
                "op_str": "word ptr es:[di+4], 0",
                "bytes": "00",
            },
            {
                "address": 0x0001,
                "mnemonic": "jmp",
                "op_str": "0x4",
                "bytes": "00",
            },
            {
                "address": 0x0004,
                "mnemonic": "cmp",
                "op_str": "word ptr es:[di+4], 16",
                "bytes": "00",
            },
            {
                "address": 0x0005,
                "mnemonic": "jge",
                "op_str": "0x9",
                "bytes": "00",
            },
            {
                "address": 0x0006,
                "mnemonic": "call",
                "op_str": "0x1000",
                "bytes": "00",
            },
            {
                "address": 0x0007,
                "mnemonic": "add",
                "op_str": "word ptr es:[di+4], di",
                "bytes": "00",
            },
            {
                "address": 0x0008,
                "mnemonic": "jmp",
                "op_str": "0x4",
                "bytes": "00",
            },
            {"address": 0x0009, "mnemonic": "ret", "op_str": "", "bytes": "00"},
        ],
    }
    lines = render_function(func, {0x1000}, name_prefix="g_")
    src = "\n".join(lines)
    assert "for (" not in src
    assert "while (" in src
    assert "uint32_t old = memw(es, (di+4) & 0xFFFF);" in src
    assert "uint32_t src = di;" in src
    assert "uint32_t tmp = old + src;" in src
    assert "memw_write(es, (di+4) & 0xFFFF, tmp & 0xFFFF);" in src


