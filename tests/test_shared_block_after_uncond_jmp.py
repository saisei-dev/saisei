import sys
from pathlib import Path

sys.path.append(str(Path(__file__).resolve().parents[1]))

from compiler.ir_to_c import CCodeRenderer  # noqa: E402


def test_shared_block_after_unconditional_jump_rendered_once():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "jmp",
                "op_str": "0008",
                "bytes": "EB06",
                "target": 0x0008,
            },
            {
                "address": 0x0002,
                "mnemonic": "nop",
                "op_str": "",
                "bytes": "90",
            },
            {
                "address": 0x0003,
                "mnemonic": "ret",
                "op_str": "",
                "bytes": "C3",
            },
            {
                "address": 0x0008,
                "mnemonic": "jmp",
                "op_str": "0002",
                "bytes": "E9F5FF",
                "target": 0x0002,
            },
            {
                "address": 0x000B,
                "mnemonic": "jmp",
                "op_str": "0002",
                "bytes": "E9F2FF",
                "target": 0x0002,
            },
        ],
    }
    lines = CCodeRenderer().render_function(func, set())
    src = "\n".join(lines)
    assert src.count("ip = 0x0002;") == 1
