import sys
from pathlib import Path

# flake8: noqa
sys.path.append(str(Path(__file__).resolve().parents[1]))

from compiler.ir_to_c import CCodeRenderer  # noqa: E402


def render_function(func, known):
    return CCodeRenderer().render_function(func, known)


def test_scasb_compares_al_with_memory_and_increments_di():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "scasb",
                "op_str": "al, byte ptr es:[di]",
                "bytes": "AE",
            },
            {"address": 0x0001, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    }
    lines = render_function(func, set())
    src = "\n".join(lines)
    cf_index = src.index("CF = left_val < right_val;")
    tmp_index = src.index("uint32_t tmp = left_val - right_val;")
    assert cf_index < tmp_index
    assert "uint32_t left_val = al;" in src
    assert "uint32_t right_val = memb(es, di);" in src
    assert "int delta = DF ? -1 : 1;" in src
    assert "di = (di + delta) & 0xFFFF;" in src
    assert "// TODO ASM: scasb" not in src

