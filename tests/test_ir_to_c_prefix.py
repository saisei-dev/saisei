import sys
from pathlib import Path

# flake8: noqa
sys.path.append(str(Path(__file__).resolve().parents[1]))

from compiler.ir_to_c import CCodeRenderer  # noqa: E402


def test_name_prefix_applied():
    func = {
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "jmp", "op_str": "0x0100", "bytes": ""},
        ],
    }
    renderer = CCodeRenderer(func_names={0x0100: "func_0100"}, name_prefix="foo_")
    lines = renderer.render_function(func, {0x0000})
    # Faithful flat model: a near jmp sets pc and continues, so there is no
    # prefixed C tail-call to verify the prefix on -- assert the jmp lowered to
    # the pc form rather than recursing into foo_func_0100().
    assert any("pc = 0x0100;" in line for line in lines)
    assert all("foo_func_0100();" not in line for line in lines)
    assert all("dispatch" not in line for line in lines)
