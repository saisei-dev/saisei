from pathlib import Path
import sys

sys.path.append(str(Path(__file__).resolve().parents[1]))

from compiler.ir_to_c import CCodeRenderer  # noqa: E402


def test_conditional_jump_rewrites_memory_operand():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "cmp",
                "op_str": "byte ptr cs:[0x8e8], 0",
                "bytes": "0000",
            },
            {
                "address": 0x0004,
                "mnemonic": "jnz",
                "op_str": "0009",
                "target": 0x0009,
                "bytes": "7503",
            },
            {
                "address": 0x0006,
                "mnemonic": "ret",
                "op_str": "",
                "bytes": "C3",
            },
            {
                "address": 0x0009,
                "mnemonic": "ret",
                "op_str": "",
                "bytes": "C3",
            },
        ],
    }
    lines = CCodeRenderer().render_function(func, set())
    src = "\n".join(lines)
    assert "memb(cs, 0x8e8)" in src
    assert "byte ptr cs:[0x8e8]" not in src
