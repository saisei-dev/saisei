import sys
from pathlib import Path

sys.path.append(str(Path(__file__).resolve().parents[1]))

from compiler.ir_to_c import CCodeRenderer  # noqa: E402


def render_function(func, known):
    return CCodeRenderer().render_function(func, known)


def test_lodsw_stosw():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "lodsw",
                "op_str": "",
                "bytes": "AD",
            },
            {
                "address": 0x0001,
                "mnemonic": "stosw",
                "op_str": "",
                "bytes": "AB",
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
    assert "int delta = DF ? -2 : 2;" in src
    assert "ax = memw(ds, si);" in src
    assert "si = (si + delta) & 0xFFFF;" in src
    assert "memw_write(es, di, ax);" in src
    assert "di = (di + delta) & 0xFFFF;" in src


def test_xchg():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "xchg",
                "op_str": "ax, cx",
                "bytes": "91",
            },
            {
                "address": 0x0001,
                "mnemonic": "ret",
                "op_str": "",
                "bytes": "C3",
            },
        ],
    }
    lines = render_function(func, set())
    src = "\n".join(lines)
    assert "uint16_t tmp = ax;" in src
    assert "ax = cx;" in src
    assert "cx = tmp;" in src


def test_pushf_popf():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "pushf",
                "op_str": "",
                "bytes": "9C",
            },
            {
                "address": 0x0001,
                "mnemonic": "popf",
                "op_str": "",
                "bytes": "9D",
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
    assert (
        "memw_write(ss, sp, (uint16_t)(0x0002u | CF | (PF << 2) | (ZF << 6) | "
        "(SF << 7) | (IF << 9) | (DF << 10) | (OF << 11)));" in src
    )
    assert "uint16_t oldIF = IF;" in src
    assert "uint16_t flags = memw(ss, sp);" in src
    assert "CF = flags & 0x0001;" in src
    assert "PF = (flags >> 2) & 1;" in src
    assert "IF = (flags >> 9) & 1;" in src
    assert "sp = (sp + 2) & 0xFFFF;" in src


def test_int_16():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "int",
                "op_str": "16",
                "bytes": "CD16",
            },
            {
                "address": 0x0001,
                "mnemonic": "ret",
                "op_str": "",
                "bytes": "C3",
            },
        ],
    }
    lines = render_function(func, set())
    src = "\n".join(lines)
    assert "bios_keyboard();" in src
