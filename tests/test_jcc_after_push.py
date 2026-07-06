from pathlib import Path
import sys

sys.path.append(str(Path(__file__).resolve().parents[1]))

from compiler.ir_to_c import CCodeRenderer  # noqa: E402


def render(func):
    # Direct calls now render as the dispatch-loop continue pattern, which
    # requires a per-binary name prefix on the renderer.
    return "\n".join(
        CCodeRenderer(name_prefix="app_").render_function(func, set())
    )


def test_jcc_condition_survives_push():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "or",
                "op_str": "al, al",
                "bytes": "08C0",
            },
            {
                "address": 0x0002,
                "mnemonic": "push",
                "op_str": "ax",
                "bytes": "50",
            },
            {
                "address": 0x0003,
                "mnemonic": "je",
                "op_str": "0x0008",
                "bytes": "7403",
            },
            {
                "address": 0x0005,
                "mnemonic": "call",
                "op_str": "loadFile",
                "bytes": "E80000",
                "target": 0x0008,
            },
            {
                "address": 0x0008,
                "mnemonic": "ret",
                "op_str": "",
                "bytes": "C3",
            },
        ],
    }
    src = render(func)
    assert "if (ZF != 1)" in src
    assert "// TODO ASM: je" not in src
