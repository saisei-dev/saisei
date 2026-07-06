import sys
from pathlib import Path

# flake8: noqa
sys.path.append(str(Path(__file__).resolve().parents[1]))

import pytest

from compiler.ir_to_c import CCodeRenderer, UnsupportedInstructionError


def render(func):
    return "\n".join(CCodeRenderer().render_function(func, set()))


def test_basic_block_comment_omitted_when_instructions_handled():
    func = {
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    }
    src = render(func)
    assert "// Basic block" not in src


def test_unhandled_instruction_raises():
    # Unsupported mnemonics now hard-fail at IR-to-C time instead of
    # silently emitting a "// Basic block" + "// TODO ASM" comment, so a
    # broken translation can never slip through to the C output.
    func = {
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "foo", "op_str": "", "bytes": "00"},
            {"address": 0x0001, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    }
    with pytest.raises(UnsupportedInstructionError):
        render(func)
