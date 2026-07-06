import sys
from pathlib import Path

# flake8: noqa
sys.path.append(str(Path(__file__).resolve().parents[1]))

from compiler.ir_to_c import CCodeRenderer  # noqa: E402


def render_function(func, known):
    return CCodeRenderer().render_function(func, known)


def test_lodsb_decrements_si_when_df_set():
    func = {
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "std", "op_str": "", "bytes": "FD"},
            {"address": 0x0001, "mnemonic": "lodsb", "op_str": "", "bytes": "AC"},
            {"address": 0x0002, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    }
    lines = render_function(func, set())
    src = "\n".join(lines)
    assert "DF = 1;" in src
    assert "int delta = DF ? -1 : 1;" in src
    assert "si = (si + delta) & 0xFFFF;" in src


def test_movsb_decrements_regs_when_df_set():
    func = {
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "std", "op_str": "", "bytes": "FD"},
            {
                "address": 0x0001,
                "mnemonic": "movsb",
                "op_str": "byte ptr es:[di], byte ptr [si]",
                "bytes": "A4",
            },
            {"address": 0x0002, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    }
    lines = render_function(func, set())
    src = "\n".join(lines)
    assert "DF = 1;" in src
    assert "int delta = DF ? -1 : 1;" in src
    assert "si = (si + delta) & 0xFFFF;" in src
    assert "di = (di + delta) & 0xFFFF;" in src


def test_rep_movsw_decrements_regs_when_df_set():
    func = {
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "std", "op_str": "", "bytes": "FD"},
            {
                "address": 0x0001,
                "mnemonic": "rep movsw",
                "op_str": "word ptr es:[di], word ptr [si]",
                "bytes": "F3A5",
            },
            {"address": 0x0003, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    }
    lines = render_function(func, set())
    src = "\n".join(lines)
    # std still sets DF; the rep movsw itself is now emitted as a call
    # to the rep_movsw_block shim, which reads DF for its step direction.
    assert "DF = 1;" in src
    assert "rep_movsw_block(es, ds);" in src
