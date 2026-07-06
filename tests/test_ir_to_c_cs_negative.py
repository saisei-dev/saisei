import sys
from pathlib import Path

# flake8: noqa
sys.path.append(str(Path(__file__).resolve().parents[1]))

from compiler.ir_to_c import CCodeRenderer, _MEMORY_FIELD_MAP  # noqa: E402


def test_cs_negative_offset_uses_named_table():
    _MEMORY_FIELD_MAP[("cs", 0xFF00)] = "table_cs_ff00"
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "mov",
                "op_str": "word ptr cs:[-0x100], ax",
                "bytes": "0000",
            },
            {
                "address": 0x0002,
                "mnemonic": "ret",
                "op_str": "",
                "bytes": "C3",
            },
        ],
    }
    lines = CCodeRenderer().render_function(func, set())
    src = "\n".join(lines)
    assert "table_cs_ff00 = ax;" in src
    del _MEMORY_FIELD_MAP[("cs", 0xFF00)]
