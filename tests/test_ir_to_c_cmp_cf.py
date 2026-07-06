from pathlib import Path
import sys

sys.path.append(str(Path(__file__).resolve().parents[1]))

from compiler.ir_to_c import CCodeRenderer  # noqa: E402


def render_function(func, known):
    return CCodeRenderer().render_function(func, known)


def test_cmp_sets_cf_before_subtraction():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "cmp",
                "op_str": "ax, bx",
                "bytes": "",
            },
            {
                "address": 0x0002,
                "mnemonic": "ret",
                "op_str": "",
                "bytes": "",
            },
        ],
    }
    lines = render_function(func, set())
    src = "\n".join(lines)
    cf_index = src.index("CF = left_val < right_val;")
    tmp_index = src.index("uint32_t tmp = left_val - right_val;")
    assert cf_index < tmp_index
    assert "// TODO ASM: cmp" not in src
