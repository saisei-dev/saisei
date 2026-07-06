"""Regression tests for dispatch helper rendering."""

# flake8: noqa

import sys
from pathlib import Path

import pytest


sys.path.append(str(Path(__file__).resolve().parents[1]))

from compiler.ir_to_c import CCodeRenderer  # noqa: E402


@pytest.fixture
def renderer_with_dispatch_helper():
    """Render a function that emits a dispatch helper with multiple statements."""

    func = {
        "start": 0x0100,
        "instructions": [
            {"address": 0x0100, "mnemonic": "jmp", "op_str": "0200", "bytes": "E9FD01"},
            {"address": 0x0200, "mnemonic": "mov", "op_str": "ax, bx", "bytes": "89D8"},
            {"address": 0x0203, "mnemonic": "inc", "op_str": "ax", "bytes": "40"},
            {"address": 0x0204, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    }

    renderer = CCodeRenderer(func_names={0x0200: "label_0200"})
    renderer.render_function(func, {0x0100})
    return renderer


def test_dispatch_helper_preserves_multiple_statements(renderer_with_dispatch_helper):
    helper_lines = renderer_with_dispatch_helper.extra_func_blocks.get(0x0200)
    assert helper_lines is not None
    assert helper_lines[0] == "    ip = 0x0200;"
    assert "    ax = bx;" in helper_lines
    assert "        ax = (ax + 1) & 0xFFFF;" in helper_lines
    # The ret now lowers to the near_ret_tail epilogue block, whose
    # closing brace is the final line of the helper.
    assert "        near_ret_tail(popped_ip, expected_retip);" in helper_lines
    assert helper_lines[-1] == "    }"
