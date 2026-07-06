import sys
from pathlib import Path

# flake8: noqa
sys.path.append(str(Path(__file__).resolve().parents[1]))

from compiler.ir_to_c import CCodeRenderer  # noqa: E402


def test_if_with_merge_as_target_preserves_merge_block():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "cmp",
                "op_str": "cx, 0xf",
                "bytes": "83f90f",
            },
            {
                "address": 0x0003,
                "mnemonic": "jb",
                "op_str": "0x0008",
                "bytes": "7203",
            },
            {
                "address": 0x0005,
                "mnemonic": "mov",
                "op_str": "cx, 0xf",
                "bytes": "b90f00",
            },
            {
                "address": 0x0008,
                "mnemonic": "mov",
                "op_str": "di, 0x88b",
                "bytes": "bf8b08",
            },
            {
                "address": 0x000b,
                "mnemonic": "ret",
                "op_str": "",
                "bytes": "c3",
            },
        ],
    }
    src = "\n".join(CCodeRenderer().render_function(func, set()))
    assert "if (CF != 1)" in src
    assert src.index("if (CF != 1)") < src.index("di = 0x88b;")
