# flake8: noqa
import sys
from pathlib import Path

import pytest

sys.path.append(str(Path(__file__).resolve().parents[1]))

from compiler.ir_to_c import CCodeRenderer, PCSwitchRenderer  # noqa: E402

REGS = ["ax", "bx", "cx", "dx", "si", "di", "bp", "sp"]


def _render_ccode(func):
    return "\n".join(CCodeRenderer().render_function(func, set()))


def _render_pc_switch(func):
    renderer = PCSwitchRenderer()
    renderer.render_function(func, set())
    return "\n".join(renderer.render_dispatch())


@pytest.mark.parametrize("reg", REGS)
def test_jmp_reg_uses_jump_table(reg):
    func = {
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "jmp", "op_str": reg, "bytes": ""},
            {"address": 0x0002, "mnemonic": "ret", "op_str": "", "bytes": ""},
        ],
    }
    src = _render_ccode(func)
    assert (
        f"jump_table((((uint32_t)cs << 4) + (uint16_t)({reg})) & 0xFFFFF, expected_retip);" in src
    )


@pytest.mark.parametrize("reg", REGS)
def test_pc_switch_jmp_reg_uses_jump_table(reg):
    func = {
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "jmp", "op_str": reg, "bytes": ""}
        ],
    }
    src = _render_pc_switch(func)
    assert (
        f"jump_table((((uint32_t)cs << 4) + (uint16_t)({reg})) & 0xFFFFF, expected_retip);" in src
    )
