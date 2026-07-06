import sys
from pathlib import Path

sys.path.append(str(Path(__file__).resolve().parents[1]))

from compiler.ir_to_c import CCodeRenderer  # noqa: E402


def render_function(func, known):
    return CCodeRenderer().render_function(func, known)


def test_shr_sets_cf_of():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "shr",
                "op_str": "al, 1",
                "bytes": "",
            },
            {
                "address": 0x0002,
                "mnemonic": "shr",
                "op_str": "ax, 1",
                "bytes": "",
            },
            {
                "address": 0x0004,
                "mnemonic": "ret",
                "op_str": "",
                "bytes": "",
            },
        ],
    }
    lines = render_function(func, set())
    src = "\n".join(lines)
    assert src.count("unsigned count = 1 & 0x1F;") == 2
    assert src.count("unsigned orig_count = count;") == 2
    assert src.count("while (count--) {") == 2
    assert src.count("CF = tmp & 1;") == 2
    assert "unsigned orig_sign = (tmp >> 7) & 1;" in src
    assert "unsigned orig_sign = (tmp >> 15) & 1;" in src
    assert src.count("OF = (orig_count == 1) ? orig_sign : 0;") == 2


def test_shr_memory_writes_via_helper():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "shr",
                "op_str": "byte ptr ds:[0x1234], 1",
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
    assert "memb_write(ds, 0x1234, tmp);" in src
    assert "ZF = memb(ds, 0x1234) == 0;" in src
