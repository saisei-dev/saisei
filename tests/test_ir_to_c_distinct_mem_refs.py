from pathlib import Path
import sys

sys.path.append(str(Path(__file__).resolve().parents[1]))

from compiler.ir_to_c import normalize_flags  # noqa: E402


def test_distinct_mem_refs_do_not_clobber_condition():
    instrs = [
        {
            "address": 0,
            "mnemonic": "cmp",
            "op_str": "byte ptr [bx], 0",
            "bytes": "",
            "detail": {
                "regs_read": ["BX"],
                "regs_write": [],
                "mem_refs": [
                    {
                        "segment": "DS",
                        "base": "BX",
                        "index": None,
                        "scale": 1,
                        "disp": 0,
                        "access": "read",
                    }
                ],
            },
        },
        {
            "address": 1,
            "mnemonic": "mov",
            "op_str": "byte ptr [si], 1",
            "bytes": "",
            "detail": {
                "regs_read": ["SI"],
                "regs_write": [],
                "mem_refs": [
                    {
                        "segment": "DS",
                        "base": "SI",
                        "index": None,
                        "scale": 1,
                        "disp": 0,
                        "access": "write",
                    }
                ],
            },
        },
        {
            "address": 2,
            "mnemonic": "je",
            "op_str": "0005",
            "bytes": "",
        },
    ]
    result = normalize_flags(instrs)
    assert result[2]["cond_prev"]["mnemonic"] == "cmp"
