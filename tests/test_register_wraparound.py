import sys
from pathlib import Path

sys.path.append(str(Path(__file__).resolve().parents[1]))

from compiler.ir_to_c import CCodeRenderer  # noqa: E402


def render(func):
    return "\n".join(CCodeRenderer().render_function(func, set()))


def test_register_arithmetic_masks_results():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "add",
                "op_str": "ax, bx",
                "bytes": "",
            },
            {
                "address": 0x0002,
                "mnemonic": "sub",
                "op_str": "ax, bx",
                "bytes": "",
            },
            {"address": 0x0004, "mnemonic": "ret", "op_str": "", "bytes": ""},
        ],
    }
    src = render(func)
    assert src.count("ax = tmp & 0xFFFF;") == 2
