import sys
from pathlib import Path

# flake8: noqa
sys.path.append(str(Path(__file__).resolve().parents[1]))

from compiler.ir_to_c import CCodeRenderer  # noqa: E402


def test_bp_relative_defaults_to_ss():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "mov",
                "op_str": "ax, word ptr [bp + 4]",
                "bytes": "",
            },
            {"address": 0x0003, "mnemonic": "ret", "op_str": "", "bytes": ""},
        ],
    }
    src = "\n".join(CCodeRenderer().render_function(func, {0x0000}))
    assert "ax = memw(ss, (bp + 4) & 0xFFFF);" in src
