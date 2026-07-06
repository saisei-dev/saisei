import sys
from pathlib import Path

# flake8: noqa
sys.path.append(str(Path(__file__).resolve().parents[1]))

from compiler.ir_to_c import CCodeRenderer  # noqa: E402


def render_function(func, known):
    # Direct calls now render as the dispatch-loop continue pattern, which
    # requires a per-binary name prefix on the renderer.
    return CCodeRenderer(name_prefix="app_").render_function(func, known)


def test_jnc_to_ret_not_negated():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "call",
                "op_str": "0x0010",
                "bytes": "E81000",
                "target": 0x0010,
            },
            {
                "address": 0x0003,
                "mnemonic": "jnc",
                "op_str": "0x0008",
                "bytes": "7303",
            },
            {
                "address": 0x0005,
                "mnemonic": "mov",
                "op_str": "ax, bx",
                "bytes": "89D8",
            },
            {
                "address": 0x0007,
                "mnemonic": "ret",
                "op_str": "",
                "bytes": "C3",
            },
            {
                "address": 0x0008,
                "mnemonic": "ret",
                "op_str": "",
                "bytes": "C3",
            },
        ],
    }
    lines = render_function(func, {0x0010})
    src = "\n".join(lines)
    assert "if (CF == 0)" in src
