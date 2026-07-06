import sys
from pathlib import Path

sys.path.append(str(Path(__file__).resolve().parents[1]))

from compiler.ir_to_c import CCodeRenderer  # noqa: E402


def render(func):
    return "\n".join(CCodeRenderer().render_function(func, set()))


def test_dx_assignment_inside_if():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "test",
                "op_str": "byte ptr cs:[0xff77], 0xff",
                "bytes": "2ef60677ffff",
            },
            {
                "address": 0x0006,
                "mnemonic": "je",
                "op_str": "000b",
                "bytes": "7403",
            },
            {
                "address": 0x0008,
                "mnemonic": "mov",
                "op_str": "dx, 0xffff",
                "bytes": "baffff",
            },
            {
                "address": 0x000b,
                "mnemonic": "ret",
                "op_str": "",
                "bytes": "c3",
            },
        ],
    }
    src = render(func)
    assert "if (ZF != 1) {" in src
    body = src.split("if (ZF != 1) {")[1]
    body = body.split("}\n", 1)[0]
    assert "dx = 0xffff;" in body
