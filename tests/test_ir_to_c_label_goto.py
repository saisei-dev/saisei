import sys
from pathlib import Path

# flake8: noqa
sys.path.append(str(Path(__file__).resolve().parents[1]))

from compiler.ir_to_c import CCodeRenderer  # noqa: E402


def test_jmp_to_label_sets_pc():
    func = {
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "jmp", "op_str": "0x340", "bytes": ""},
            {"address": 0x0340, "mnemonic": "ret", "op_str": "", "bytes": ""},
        ],
    }
    renderer = CCodeRenderer(func_names={0x0340: "label_0340"})
    body_lines = renderer.render_function(func, {0x0000})
    src = "\n".join(body_lines)
    # Faithful flat model: a near jmp sets pc and continues; it never emits a C
    # tail-call into the target's function.
    assert "pc = 0x0340;" in src
    assert "label_0340();" not in src
    assert "dispatch(" not in src
    assert "label_0340:" not in src
