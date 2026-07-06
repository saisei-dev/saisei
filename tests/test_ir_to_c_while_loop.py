import sys
from pathlib import Path

# flake8: noqa
sys.path.append(str(Path(__file__).resolve().parents[1]))

from compiler.ir_to_c import CCodeRenderer  # noqa: E402


def render_function(func, known):
    return CCodeRenderer().render_function(func, known)
def test_top_checked_while_loop():
    func = {
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "cmp", "op_str": "cx, 3", "bytes": "83F9"},
            {"address": 0x0002, "mnemonic": "jge", "op_str": "000C", "bytes": "7D08"},
            {"address": 0x0004, "mnemonic": "mov", "op_str": "ax, bx", "bytes": "89D8"},
            {"address": 0x0006, "mnemonic": "inc", "op_str": "cx", "bytes": "41"},
            {"address": 0x0007, "mnemonic": "jmp", "op_str": "0000", "bytes": "E9F9FF"},
            {"address": 0x000C, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    }
    lines = render_function(func, set())
    src = "\n".join(lines)
    assert "if (SF == OF)" in src
    assert "ax = bx;" in src


def test_loop_instruction_is_structured_as_while_loop():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "cmp",
                "op_str": "byte ptr [si], 0x20",
                "bytes": "803C20",
            },
            {
                "address": 0x0003,
                "mnemonic": "jne",
                "op_str": "000A",
                "bytes": "7505",
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
            {"address": 0x0008, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
            {"address": 0x000A, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    }
    lines = render_function(func, set())
    src = "\n".join(lines)
    assert "while (" in src
    assert "si = (si + 1) & 0xFFFF;" in src
    assert "// TODO ASM: loop" not in src


def test_while_loop_with_nested_if():
    func = {
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "cmp", "op_str": "cx, 3", "bytes": "83F9"},
            {"address": 0x0002, "mnemonic": "jge", "op_str": "0x10", "bytes": "7D0C"},
            {"address": 0x0004, "mnemonic": "cmp", "op_str": "ax, 0", "bytes": "3D0000"},
            {"address": 0x0007, "mnemonic": "je", "op_str": "0xB", "bytes": "7402"},
            {"address": 0x0009, "mnemonic": "inc", "op_str": "bx", "bytes": "43"},
            {"address": 0x000A, "mnemonic": "jmp", "op_str": "0xB", "bytes": "EBFF"},
            {"address": 0x000B, "mnemonic": "inc", "op_str": "cx", "bytes": "41"},
            {"address": 0x000C, "mnemonic": "jmp", "op_str": "0x0", "bytes": "E9F1FF"},
            {"address": 0x0010, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    }
    lines = render_function(func, set())
    src = "\n".join(lines)
    assert "while (SF != OF)" in src
    assert "if (ZF != 1)" in src
    assert "bx = (bx + 1) & 0xFFFF;" in src


def test_initialization_before_loop_rendered_first():
    func = {
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "mov", "op_str": "cx, 0", "bytes": "00"},
            {"address": 0x0001, "mnemonic": "jmp", "op_str": "0x4", "bytes": "00"},
            {"address": 0x0004, "mnemonic": "cmp", "op_str": "cx, 3", "bytes": "00"},
            {"address": 0x0005, "mnemonic": "jge", "op_str": "0x8", "bytes": "00"},
            {"address": 0x0006, "mnemonic": "inc", "op_str": "cx", "bytes": "00"},
            {"address": 0x0007, "mnemonic": "jmp", "op_str": "0x4", "bytes": "00"},
            {"address": 0x0008, "mnemonic": "ret", "op_str": "", "bytes": "00"},
        ],
    }
    lines = render_function(func, set())
    src = "\n".join(lines)
    assert "cx = 0;" in src
    assert "for (cx = 0; SF != OF; cx++)" in src


def test_do_while_loop():
    func = {
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "mov", "op_str": "dl, 0xff", "bytes": "B2FF"},
            {"address": 0x0002, "mnemonic": "int", "op_str": "0x21", "bytes": "CD21"},
            {"address": 0x0004, "mnemonic": "jne", "op_str": "0000", "bytes": "75FA"},
            {"address": 0x0006, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    }
    lines = render_function(func, set())
    src = "\n".join(lines)
    assert src.count("do {") == 1
    assert "while (ZF == 0)" in src


def test_conditional_back_edge_structured_as_while():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "cmp",
                "op_str": "byte ptr [si], 0x20",
                "bytes": "803C20",
            },
            {
                "address": 0x0003,
                "mnemonic": "jb",
                "op_str": "0010",
                "bytes": "720B",
            },
            {
                "address": 0x0005,
                "mnemonic": "inc",
                "op_str": "dx",
                "bytes": "42",
            },
            {
                "address": 0x0006,
                "mnemonic": "cmp",
                "op_str": "byte ptr [si], 0x20",
                "bytes": "803C20",
            },
            {
                "address": 0x0009,
                "mnemonic": "jae",
                "op_str": "0000",
                "bytes": "7300",
            },
            {"address": 0x000B, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
            {"address": 0x0010, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    }
    lines = render_function(func, set())
    src = "\n".join(lines)
    assert "while (" in src
    assert "dx = (dx + 1) & 0xFFFF;" in src
    assert "// TODO ASM: jae" not in src
    assert "break;" not in src


def test_initialisation_inside_loop_triggers_do_while():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "mov",
                "op_str": "al, byte ptr [bx]",
                "bytes": "8A07",
            },
            {
                "address": 0x0002,
                "mnemonic": "or",
                "op_str": "al, al",
                "bytes": "08C0",
            },
            {
                "address": 0x0004,
                "mnemonic": "jz",
                "op_str": "000A",
                "bytes": "7404",
                "cond_prev": {"mnemonic": "or", "op_str": "al, al"},
            },
            {"address": 0x0006, "mnemonic": "inc", "op_str": "bx", "bytes": "43"},
            {"address": 0x0007, "mnemonic": "jmp", "op_str": "0000", "bytes": "E9F6FF"},
            {"address": 0x000A, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    }
    lines = render_function(func, set())
    src = "\n".join(lines)
    assert src.count("do {") == 1
    assert "bx = (bx + 1) & 0xFFFF;" in src
    assert "while (ZF != 1)" in src


def test_dec_dx_jne_generates_dx_condition():
    func = {
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "cmp", "op_str": "ax, 0xffff", "bytes": "3DFFFF"},
            {"address": 0x0003, "mnemonic": "mov", "op_str": "al, 0", "bytes": "B000"},
            {"address": 0x0005, "mnemonic": "dec", "op_str": "dx", "bytes": "4A"},
            {"address": 0x0006, "mnemonic": "jne", "op_str": "0x5", "bytes": "75FD"},
            {"address": 0x0008, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    }
    lines = render_function(func, set())
    src = "\n".join(lines)
    assert "while (ZF == 0)" in src

