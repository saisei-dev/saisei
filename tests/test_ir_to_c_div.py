import sys
from pathlib import Path

sys.path.append(str(Path(__file__).resolve().parents[1]))

from compiler.ir_to_c import CCodeRenderer  # noqa: E402


def render(func):
    return "\n".join(CCodeRenderer().render_function(func, set()))


def test_div_cl_generates_code_without_todo():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "div",
                "op_str": "cl",
                "bytes": "F6F1",
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
    assert "al = (tmp / divisor) & 0xFF;" in src
    assert "ah = (tmp % divisor) & 0xFF;" in src
    assert "ZF = ax == 0;" not in src
    assert "// TODO ASM: div cl" not in src


def test_div_cx_generates_quotient_and_remainder_without_flag_writes():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "div",
                "op_str": "cx",
                "bytes": "F7F1",
                "detail": {"regs_write": ["AX", "DX"]},
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
    assert "uint32_t tmp = ((uint32_t)dx << 16) | ax;" in src
    assert "ax = (tmp / divisor) & 0xFFFF;" in src
    assert "dx = (tmp % divisor) & 0xFFFF;" in src
    assert "ZF =" not in src
    assert "// TODO ASM: div cx" not in src


def test_idiv_cx_does_not_emit_flag_updates():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "idiv",
                "op_str": "cx",
                "bytes": "F7F9",
                "detail": {"regs_write": ["AX", "DX"]},
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
    assert "int32_t dividend = ((int32_t)(int16_t)dx << 16) | ax;" in src
    assert "ax = (uint16_t)(dividend / divisor);" in src
    assert "dx = (uint16_t)(dividend % divisor);" in src
    assert "ZF =" not in src
