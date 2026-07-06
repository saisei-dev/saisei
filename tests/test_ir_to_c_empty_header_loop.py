# flake8: noqa
import sys
from pathlib import Path

sys.path.append(str(Path(__file__).resolve().parents[1]))

from compiler.ir_to_c import CCodeRenderer  # noqa: E402


def render_function(func, known):
    return CCodeRenderer().render_function(func, known)


def test_loop_with_header_only_jump():
    func = {
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "jmp", "op_str": "0x2", "bytes": "E90200"},
            {"address": 0x0002, "mnemonic": "nop", "op_str": "", "bytes": "90"},
            {"address": 0x0003, "mnemonic": "jmp", "op_str": "0x0", "bytes": "E9FAFF"},
            {"address": 0x0005, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    }
    lines = render_function(func, set())
    src = "\n".join(lines)
    assert "while (1)" in src
    assert "// TODO ASM: nop" not in src
