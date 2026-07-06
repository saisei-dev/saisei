import sys
from pathlib import Path

# flake8: noqa
sys.path.append(str(Path(__file__).resolve().parents[1]))

from compiler.ir_to_c import CCodeRenderer  # noqa: E402


def render_function(func, known):
    return CCodeRenderer().render_function(func, known)


def test_cmpsb_compares_memory_and_increments_si_di():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "cmpsb",
                "op_str": "byte ptr [si], byte ptr es:[di]",
                "bytes": "A6",
            },
            {"address": 0x0001, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    }
    lines = render_function(func, set())
    src = "\n".join(lines)
    cf_index = src.index("CF = left_val < right_val;")
    tmp_index = src.index("uint32_t tmp = left_val - right_val;")
    assert cf_index < tmp_index
    assert "uint32_t left_val = memb(ds, si);" in src
    assert "uint32_t right_val = memb(es, di);" in src
    assert "si = (si + delta) & 0xFFFF;" in src
    assert "di = (di + delta) & 0xFFFF;" in src
    assert "// TODO ASM: cmpsb" not in src


def test_cmpsb_respects_source_segment_override():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "cmpsb",
                "op_str": "byte ptr [si], byte ptr es:[di]",
                "bytes": "2EA6",
                "detail": {
                    "mem_refs": [
                        {"segment": "CS", "disp": 0, "access": "read"},
                        {"segment": "ES", "disp": 0, "access": "read"},
                    ]
                },
            },
            {"address": 0x0002, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    }
    lines = render_function(func, set())
    src = "\n".join(lines)
    assert "uint32_t left_val = memb(cs, si);" in src
