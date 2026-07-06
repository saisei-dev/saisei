import sys
from pathlib import Path

# flake8: noqa
sys.path.append(str(Path(__file__).resolve().parents[1]))

from compiler.ir_to_c import CCodeRenderer  # noqa: E402


def render_function(func, known):
    return CCodeRenderer().render_function(func, known)


def test_movsb_copies_byte_and_increments():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "movsb",
                "op_str": "byte ptr es:[di], byte ptr [si]",
                "bytes": "A4",
            },
            {"address": 0x0001, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    }
    lines = render_function(func, set())
    src = "\n".join(lines)
    assert "int delta = DF ? -1 : 1;" in src
    assert "memb_write(es, di, memb(ds, si));" in src
    assert "si = (si + delta) & 0xFFFF;" in src
    assert "di = (di + delta) & 0xFFFF;" in src


def test_rep_movsb_loops_and_updates_regs():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "rep movsb",
                "op_str": "byte ptr es:[di], byte ptr [si]",
                "bytes": "F3A4",
            },
            {"address": 0x0002, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    }
    lines = render_function(func, set())
    src = "\n".join(lines)
    # rep movsb is now emitted as a call to the rep_movsb_block shim,
    # which performs the DF-aware copy + si/di/cx updates in C.
    assert "rep_movsb_block(es, ds);" in src


def test_stosb_stores_byte_and_increments():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "stosb",
                "op_str": "byte ptr es:[di], al",
                "bytes": "AA",
            },
            {"address": 0x0001, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    }
    lines = render_function(func, set())
    src = "\n".join(lines)
    assert "int delta = DF ? -1 : 1;" in src
    assert "memb_write(es, di, al);" in src
    assert "di = (di + delta) & 0xFFFF;" in src


def test_rep_stosb_uses_memset_and_updates_regs():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "rep stosb",
                "op_str": "byte ptr es:[di], al",
                "bytes": "F3AA",
            },
            {"address": 0x0002, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    }
    lines = render_function(func, set())
    src = "\n".join(lines)
    # rep stosb is now emitted as a call to the rep_stosb_block shim,
    # which performs the DF-aware fill + di/cx updates in C.
    assert "rep_stosb_block(es);" in src


def test_movsb_respects_source_segment_override():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "movsb",
                "op_str": "byte ptr es:[di], byte ptr [si]",
                "bytes": "2EA4",
                "detail": {
                    "mem_refs": [
                        {"segment": "CS", "disp": 0, "access": "read"},
                        {"segment": "ES", "disp": 0, "access": "write"},
                    ]
                },
            },
            {"address": 0x0002, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    }
    lines = render_function(func, set())
    src = "\n".join(lines)
    assert "memb_write(es, di, memb(cs, si));" in src
