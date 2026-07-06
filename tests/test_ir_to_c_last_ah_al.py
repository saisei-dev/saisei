# flake8: noqa
from pathlib import Path
import sys

sys.path.append(str(Path(__file__).resolve().parents[1]))

from compiler.ir_to_c import CCodeRenderer  # noqa: E402


def render_function(func, known):
    return CCodeRenderer().render_function(func, known)


def test_mov_ah_invalidates_al():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "mov",
                "op_str": "al, 0x02",
                "bytes": "B002",
            },
            {
                "address": 0x0002,
                "mnemonic": "mov",
                "op_str": "ah, 0x42",
                "bytes": "B442",
            },
            {
                "address": 0x0004,
                "mnemonic": "int",
                "op_str": "0x21",
                "bytes": "CD21",
            },
            {
                "address": 0x0006,
                "mnemonic": "ret",
                "op_str": "",
                "bytes": "C3",
            },
        ],
    }
    lines = render_function(func, set())
    src = "\n".join(lines)
    assert "CF = dos_lseek(bx, cx, dx, al);" in src
