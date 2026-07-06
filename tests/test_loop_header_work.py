import sys
from pathlib import Path

# flake8: noqa
sys.path.append(str(Path(__file__).resolve().parents[1]))

from compiler.ir_to_c import CCodeRenderer  # noqa: E402


def render_function(func, known):
    return CCodeRenderer().render_function(func, known)


def test_header_instructions_preserved():
    func = {
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "lodsb", "op_str": "", "bytes": "AC"},
            {"address": 0x0001, "mnemonic": "cmp", "op_str": "al, 0", "bytes": "3C00"},
            {"address": 0x0003, "mnemonic": "je", "op_str": "000A", "bytes": "7405"},
            {"address": 0x0005, "mnemonic": "inc", "op_str": "cx", "bytes": "41"},
            {"address": 0x0006, "mnemonic": "jmp", "op_str": "0000", "bytes": "E9F9FF"},
            {"address": 0x000A, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    }
    lines = render_function(func, set())
    src = "\n".join(lines)
    assert "while (" in src
    assert "int delta = DF ? -1 : 1;" in src
    assert "al = memb(ds, si);" in src
    assert "si = (si + delta) & 0xFFFF;" in src
    assert "cx = (cx + 1) & 0xFFFF;" in src

