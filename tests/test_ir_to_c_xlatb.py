import sys
from pathlib import Path

# flake8: noqa
sys.path.append(str(Path(__file__).resolve().parents[1]))

from compiler.ir_to_c import CCodeRenderer  # noqa: E402


def render_function(func, known):
    return CCodeRenderer().render_function(func, known)


def test_xlatb_loads_table_byte():
    func = {
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "xlatb", "op_str": "", "bytes": "D7"},
            {"address": 0x0001, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    }
    lines = render_function(func, set())
    src = "\n".join(lines)
    assert "al = memb(ds, (bx + al) & 0xFFFF);" in src
    assert "// TODO ASM: xlatb" not in src


def test_xlatb_respects_segment_override():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "xlatb",
                "op_str": "",
                "bytes": "2ED7",
                "detail": {"seg_override": "CS"},
            },
            {"address": 0x0002, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    }
    lines = render_function(func, set())
    src = "\n".join(lines)
    assert "al = memb(cs, (bx + al) & 0xFFFF);" in src
