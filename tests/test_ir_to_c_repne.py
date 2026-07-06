import sys
from pathlib import Path

sys.path.append(str(Path(__file__).resolve().parents[1]))

from compiler.ir_to_c import CCodeRenderer  # noqa: E402


def render_function(func, known):
    return CCodeRenderer().render_function(func, known)


def test_repne_scasb_calls_scanMemoryForAl():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "repne scasb",
                "op_str": "byte ptr es:[di]",
                "bytes": "F2AE",
            },
            {
                "address": 0x0002,
                "mnemonic": "ret",
                "op_str": "",
                "bytes": "C3",
            },
        ],
    }
    lines = render_function(func, set())
    src = "\n".join(lines)
    assert "int delta = DF ? -1 : 1;" in src
    assert "uint8_t last_byte = 0;" in src
    assert (
        "uint16_t index = scanMemoryForAl((const uint8_t *)seg_off(es, di), "
        "al, count, delta, &last_byte);"
        in src
    )
    assert (
        "uint16_t advance = (index < count) ? index + 1 : index;" in src
    )
    assert "if (advance > 0) {" in src
    assert "CF = left_val < right_val;" in src
    assert "SF = (result >> 7) & 1;" in src
    assert (
        "OF = ((left_val ^ right_val) & (left_val ^ result) & 0x80) != 0;"
        in src
    )
    assert "ZF = result == 0;" in src
    assert "cx = (count - advance) & 0xFFFF;" in src
    assert "di = (di + advance * delta) & 0xFFFF;" in src
    assert "// TODO ASM: repne scasb" not in src
