import sys
from pathlib import Path

sys.path.append(str(Path(__file__).resolve().parents[1]))

from compiler.ir_to_c import CCodeRenderer  # noqa: E402


def test_loop_exit_block_consumed():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "mov",
                "op_str": "cx, 2",
                "bytes": "B90200",
            },
            {
                "address": 0x0003,
                "mnemonic": "cmp",
                "op_str": "al, 1",
                "bytes": "3C01",
            },
            {
                "address": 0x0005,
                "mnemonic": "je",
                "op_str": "0xF",
                "bytes": "7408",
            },
            {
                "address": 0x0007,
                "mnemonic": "inc",
                "op_str": "di",
                "bytes": "47",
            },
            {
                "address": 0x0008,
                "mnemonic": "loop",
                "op_str": "0x3",
                "bytes": "E2F9",
            },
            {
                "address": 0x000A,
                "mnemonic": "ret",
                "op_str": "",
                "bytes": "C3",
            },
            {
                "address": 0x000F,
                "mnemonic": "mov",
                "op_str": "bx, bx",
                "bytes": "89DB",
            },
            {
                "address": 0x0011,
                "mnemonic": "ret",
                "op_str": "",
                "bytes": "C3",
            },
        ],
    }
    src = "\n".join(CCodeRenderer().render_function(func, set()))
    # The exit block at 0x000F should appear once inside the conditional branch
    # while the fall-through ``ret`` after the loop remains.
    assert src.count("bx = bx;") == 1
    assert src.count("return;") == 2
