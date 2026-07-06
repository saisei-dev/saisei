import sys
from pathlib import Path

sys.path.append(str(Path(__file__).resolve().parents[1]))

from compiler.ir_to_c import CCodeRenderer  # noqa: E402


def render(func):
    return "\n".join(CCodeRenderer().render_function(func, set()))


def test_not_inverts_and_sets_zero_flag():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "not",
                "op_str": "al",
                "bytes": "F6D0",
            },
            {
                "address": 0x0002,
                "mnemonic": "ret",
                "op_str": "",
                "bytes": "C3",
            },
        ],
    }
    src = render(func)
    assert "al = (~al) & 0xFF;" in src
    assert "ZF = al == 0;" in src
    assert "// TODO ASM: not al" not in src


def test_not_memory_uses_write_helper():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "not",
                "op_str": "byte ptr cs:[0xff27]",
                "bytes": "F61627FF",
            },
            {
                "address": 0x0004,
                "mnemonic": "ret",
                "op_str": "",
                "bytes": "C3",
            },
        ],
    }
    src = render(func)
    assert "memb_write(cs, 0xff27, (~memb(cs, 0xff27)) & 0xFF);" in src
    assert "ZF = memb(cs, 0xff27) == 0;" in src
    assert "// TODO ASM: not" not in src
