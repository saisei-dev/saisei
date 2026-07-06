import sys
from pathlib import Path

sys.path.append(str(Path(__file__).resolve().parents[1]))

from compiler.ir_to_c import CCodeRenderer  # noqa: E402


def test_jmp_over_lodsb_drops_prev_condition():
    func = {
        "start": 0,
        "instructions": [
            {
                "address": 0,
                "mnemonic": "cmp",
                "op_str": "al, 0x5f",
                "bytes": "3c5f",
                "detail": {
                    "regs_read": ["AL"],
                    "regs_write": [],
                    "mem_refs": [],
                },
            },
            {
                "address": 2,
                "mnemonic": "lodsb",
                "op_str": "al, byte ptr [si]",
                "bytes": "ac",
                "detail": {
                    "regs_read": ["SI", "FLAGS"],
                    "regs_write": ["AL", "SI"],
                    "mem_refs": [
                        {"segment": "DS", "disp": 0, "access": "read"}
                    ],
                },
            },
            {
                "address": 3,
                "mnemonic": "jmp",
                "op_str": "0x0005",
                "bytes": "e90000",
            },
            {
                "address": 5,
                "mnemonic": "jne",
                "op_str": "0x0008",
                "bytes": "7501",
            },
            {"address": 7, "mnemonic": "ret", "op_str": "", "bytes": "c3"},
            {"address": 8, "mnemonic": "ret", "op_str": "", "bytes": "c3"},
        ],
    }
    lines = CCodeRenderer().render_function(func, set())
    src = "\n".join(lines)
    assert "if (ZF == 0)" in src
    assert "unsupported jcc" not in src
