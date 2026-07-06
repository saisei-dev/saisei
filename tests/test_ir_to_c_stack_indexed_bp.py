import sys
from pathlib import Path

# flake8: noqa
sys.path.append(str(Path(__file__).resolve().parents[1]))

from compiler.ir_to_c import CCodeRenderer  # noqa: E402


def render_function(func, known):
    return CCodeRenderer().render_function(func, known)


def test_bp_indexed_stack_access_keeps_bp_expression():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "mov",
                "op_str": "ax, word ptr [bp + si - 4]",
                "bytes": "8B42FC",
            },
            {"address": 0x0003, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    }
    lines = render_function(func, set())
    src = "\n".join(lines)
    assert "memw(ss, (bp + si - 4) & 0xFFFF)" in src
    assert "var_4" not in src
