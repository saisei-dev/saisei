# flake8: noqa
import sys
from pathlib import Path

sys.path.append(str(Path(__file__).resolve().parents[1]))

from compiler.ir_to_c import CCodeRenderer  # noqa: E402


def test_nop_instruction_is_ignored():
    func = {
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "nop", "op_str": "", "bytes": "90"},
            {"address": 0x0001, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    }
    renderer = CCodeRenderer()
    lines = renderer.render_function(func, set())
    src = "\n".join(lines)
    assert "// TODO ASM: nop" not in src
