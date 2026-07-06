import sys
from pathlib import Path

# flake8: noqa
sys.path.append(str(Path(__file__).resolve().parents[1]))

from compiler.ir_to_c import CCodeRenderer  # noqa: E402


def render_function(func, known):
    return CCodeRenderer().render_function(func, known)


def test_single_header_instruction_loop():
    func = {
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "mov", "op_str": "byte ptr [0xff1a], 0", "bytes": "c6061aff00"},
            {"address": 0x0005, "mnemonic": "jmp", "op_str": "0000", "bytes": "e9faff"},
        ],
    }
    lines = render_function(func, set())
    loop_idx = next(i for i, l in enumerate(lines) if l.strip().startswith("do"))
    memb_idx = next(i for i, l in enumerate(lines) if "memb_write" in l)
    assert memb_idx > loop_idx
    assert any("memb_write(ds, 0xff1a, 0" in l for l in lines)
