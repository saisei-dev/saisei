import sys
from pathlib import Path

# flake8: noqa
sys.path.append(str(Path(__file__).resolve().parents[1]))

from compiler.ir_to_c import CCodeRenderer  # noqa: E402


def render_function(func, known):
    return CCodeRenderer().render_function(func, known)
def test_switch_pattern_emits_switch_statement():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "mov",
                "op_str": "bl, byte ptr cs:[0x8E7]",
                "bytes": "8A1E8708",
            },
            {"address": 0x0004, "mnemonic": "mov", "op_str": "bh, 0", "bytes": "B700"},
            {"address": 0x0006, "mnemonic": "add", "op_str": "bx, bx", "bytes": "01DB"},
            {
                "address": 0x0008,
                "mnemonic": "jmp",
                "op_str": "word ptr cs:[bx + 0x100]",
                "bytes": "FF27",
            },
        ],
    }
    renderer = CCodeRenderer()
    renderer.code_bytes = b"\x00" * 0x100 + b"\x00\x01\x00\x02"
    known = {0x0000, 0x0100, 0x0200}
    lines = renderer.render_function(func, known)
    src = "\n".join(lines)
    assert "switch (memb(cs, 0x8E7))" in src
    assert "case 0: func_0100();" in src
    assert "case 1: func_0200();" in src


def test_switch_structures_case_bodies():
    func = {
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "jmp", "op_str": "0010", "bytes": "E90000"},
            {"address": 0x0003, "mnemonic": "jmp", "op_str": "0016", "bytes": "E90000"},
            {
                "address": 0x0006,
                "mnemonic": "mov",
                "op_str": "bl, byte ptr cs:[0x8E7]",
                "bytes": "8A1E8708",
            },
            {"address": 0x000A, "mnemonic": "mov", "op_str": "bh, 0", "bytes": "B700"},
            {"address": 0x000C, "mnemonic": "add", "op_str": "bx, bx", "bytes": "01DB"},
            {
                "address": 0x000E,
                "mnemonic": "jmp",
                "op_str": "word ptr cs:[bx + 0x100]",
                "bytes": "FF27",
            },
            {"address": 0x0010, "mnemonic": "mov", "op_str": "ax, 1", "bytes": "B80100"},
            {"address": 0x0013, "mnemonic": "jmp", "op_str": "0020", "bytes": "E90000"},
            {"address": 0x0016, "mnemonic": "mov", "op_str": "ax, 2", "bytes": "B80200"},
            {"address": 0x0019, "mnemonic": "jmp", "op_str": "0020", "bytes": "E90000"},
            {"address": 0x0020, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    }
    renderer = CCodeRenderer()
    renderer.code_bytes = b"\x00" * 0x100 + b"\x10\x00\x16\x00"
    lines = renderer.render_function(func, {0x0000})
    src = "\n".join(lines)
    assert "switch (memb(cs, 0x8E7))" in src
    assert "case 0:" in src
    assert "case 1:" in src
    assert "ax = 1;" in src
    assert "ax = 2;" in src



def test_switch_pattern_detects_xor_zeroing_bh():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "mov",
                "op_str": "bl, al",
                "bytes": "88C3",
            },
            {"address": 0x0002, "mnemonic": "xor", "op_str": "bh, bh", "bytes": "30FF"},
            {"address": 0x0004, "mnemonic": "add", "op_str": "bx, bx", "bytes": "01DB"},
            {
                "address": 0x0006,
                "mnemonic": "jmp",
                "op_str": "word ptr es:[bx + 0x100]",
                "bytes": "26FFA70001",
            },
        ],
    }
    renderer = CCodeRenderer()
    renderer.code_bytes = b"\x00" * 0x100 + b"\x00\x01\x00\x02"
    known = {0x0000, 0x0100, 0x0200}
    src = "\n".join(renderer.render_function(func, known))

    assert "switch (al)" in src
    assert "case 0: func_0100();" in src
    assert "case 1: func_0200();" in src

def test_switch_pattern_uses_current_function_addresses_for_cases():
    func = {
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "mov", "op_str": "bl, al", "bytes": "88C3"},
            {"address": 0x0002, "mnemonic": "xor", "op_str": "bh, bh", "bytes": "30FF"},
            {"address": 0x0004, "mnemonic": "add", "op_str": "bx, bx", "bytes": "01DB"},
            {
                "address": 0x0006,
                "mnemonic": "jmp",
                "op_str": "word ptr cs:[bx + 0x0100]",
                "bytes": "FFA70001",
            },
            {"address": 0x000A, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
            {"address": 0x000B, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    }

    renderer = CCodeRenderer()
    renderer.code_bytes = b"\x00" * 0x100 + b"\x0A\x00\x0B\x00"

    src = "\n".join(renderer.render_function(func, {0x0000}))

    assert "switch (al)" in src
    assert "case 0:" in src
    assert "case 1:" in src
    assert "case 1: /* 0x000B */" in src
    assert "jump_table(" not in src
