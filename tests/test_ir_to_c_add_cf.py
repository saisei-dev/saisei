import sys
from pathlib import Path

sys.path.append(str(Path(__file__).resolve().parents[1]))

from compiler.ir_to_c import CCodeRenderer  # noqa: E402


def render_function(func, known):
    return CCodeRenderer().render_function(func, known)


def test_add_sets_cf_flag():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "add",
                "op_str": "al, al",
                "bytes": "00C0",
            },
            {
                "address": 0x0002,
                "mnemonic": "jb",
                "op_str": "0000",
                "bytes": "7200",
            },
            {
                "address": 0x0004,
                "mnemonic": "ret",
                "op_str": "",
                "bytes": "C3",
            },
        ],
    }
    lines = render_function(func, set())
    src = "\n".join(lines)
    assert "uint32_t old = al;" in src
    assert "uint32_t src = al;" in src
    assert "uint32_t tmp = old + src;" in src
    assert "CF = tmp > 0xFF;" in src
    assert "while (CF == 1)" in src
