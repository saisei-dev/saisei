import sys
from pathlib import Path

# flake8: noqa
sys.path.append(str(Path(__file__).resolve().parents[1]))

from compiler.ir_to_c import CCodeRenderer  # noqa: E402


def render_function(func, known):
    return CCodeRenderer().render_function(func, known)


def test_movsw_copies_word_and_increments():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "movsw",
                "op_str": "word ptr es:[di], word ptr [si]",
                "bytes": "A5",
            },
            {"address": 0x0001, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    }
    lines = render_function(func, set())
    src = "\n".join(lines)
    assert "int delta = DF ? -2 : 2;" in src
    assert "memw_write(es, di, memw(ds, si));" in src
    assert "si = (si + delta) & 0xFFFF;" in src
    assert "di = (di + delta) & 0xFFFF;" in src


def test_rep_movsw_loops_and_updates_regs():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "rep movsw",
                "op_str": "word ptr es:[di], word ptr [si]",
                "bytes": "F3A5",
            },
            {"address": 0x0002, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    }
    lines = render_function(func, set())
    src = "\n".join(lines)
    # rep movsw is now emitted as a call to the rep_movsw_block shim,
    # which performs the DF-aware copy + si/di/cx updates in C.
    assert "rep_movsw_block(es, ds);" in src
