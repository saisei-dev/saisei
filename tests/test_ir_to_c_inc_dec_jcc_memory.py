from pathlib import Path
import sys
import pytest

sys.path.append(str(Path(__file__).resolve().parents[1]))

from compiler.ir_to_c import CCodeRenderer  # noqa: E402


@pytest.mark.parametrize(
    "mnemonic,jcc,bytes_jcc,op",
    [
        ("inc", "jz", "7403", "=="),
        ("dec", "jnz", "7503", "!="),
    ],
)
def test_memory_inc_dec_conditional_jump(mnemonic, jcc, bytes_jcc, op):
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": mnemonic,
                "op_str": "byte ptr [bp-4]",
                "bytes": "0000",
            },
            {
                "address": 0x0002,
                "mnemonic": jcc,
                "op_str": "0007",
                "target": 0x0007,
                "bytes": bytes_jcc,
            },
            {
                "address": 0x0004,
                "mnemonic": "ret",
                "op_str": "",
                "bytes": "C3",
            },
            {
                "address": 0x0007,
                "mnemonic": "ret",
                "op_str": "",
                "bytes": "C3",
            },
        ],
    }
    lines = CCodeRenderer().render_function(func, set())
    src = "\n".join(lines)
    assert "ZF" in src
    assert "byte ptr" not in src
