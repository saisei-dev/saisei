import sys
from pathlib import Path

# flake8: noqa
sys.path.append(str(Path(__file__).resolve().parents[1]))

from compiler.ir_to_c import CCodeRenderer  # noqa: E402


def render_function(func, known):
    return CCodeRenderer().render_function(func, known)


def test_rep_lodsb_loads_last_byte_and_updates_regs():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "rep lodsb",
                "op_str": "",
                "bytes": "F3AC",
            },
            {"address": 0x0002, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    }
    lines = render_function(func, set())
    src = "\n".join(lines)
    assert "if (cx) {" in src
    assert "int delta = DF ? -1 : 1;" in src
    assert "al = memb(ds, si + (cx - 1) * delta);" in src
    assert "si = (si + cx * delta) & 0xFFFF;" in src
    assert "cx = 0;" in src


def test_lodsb_respects_source_segment_override():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "lodsb",
                "op_str": "",
                "bytes": "2EAC",
                "detail": {
                    "mem_refs": [
                        {"segment": "CS", "disp": 0, "access": "read"}
                    ]
                },
            },
            {"address": 0x0002, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    }
    lines = render_function(func, set())
    src = "\n".join(lines)
    assert "al = memb(cs, si);" in src
