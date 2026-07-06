import sys
from pathlib import Path

# flake8: noqa
sys.path.append(str(Path(__file__).resolve().parents[1]))

from compiler.ir_to_c import CCodeRenderer  # noqa: E402


def render_function(func, known):
    return CCodeRenderer().render_function(func, known)


def test_dos_write_char_passes_argument():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "mov",
                "op_str": "dl, 0x41",
                "bytes": "B241",
            },
            {
                "address": 0x0002,
                "mnemonic": "mov",
                "op_str": "ah, 0x02",
                "bytes": "B402",
            },
            {
                "address": 0x0004,
                "mnemonic": "int",
                "op_str": "0x21",
                "bytes": "CD21",
            },
            {
                "address": 0x0006,
                "mnemonic": "ret",
                "op_str": "",
                "bytes": "C3",
            },
        ],
    }
    lines = render_function(func, set())
    src = "\n".join(lines)
    assert "CF = dos_write_char(0x41);" in src


def test_dos_direct_console_io_uses_dl():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "mov",
                "op_str": "dl, 0x41",
                "bytes": "B241",
            },
            {
                "address": 0x0002,
                "mnemonic": "mov",
                "op_str": "ah, 0x06",
                "bytes": "B406",
            },
            {
                "address": 0x0004,
                "mnemonic": "int",
                "op_str": "0x21",
                "bytes": "CD21",
            },
            {
                "address": 0x0006,
                "mnemonic": "ret",
                "op_str": "",
                "bytes": "C3",
            },
        ],
    }
    lines = render_function(func, set())
    src = "\n".join(lines)
    assert "dl = 0x41;" in src
    assert "dos_direct_console_io(dl);" in src


def test_dos_close_file_passes_bx():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "mov",
                "op_str": "bx, 0x0005",
                "bytes": "BB0500",
            },
            {
                "address": 0x0003,
                "mnemonic": "mov",
                "op_str": "ah, 0x3E",
                "bytes": "B43E",
            },
            {
                "address": 0x0005,
                "mnemonic": "int",
                "op_str": "0x21",
                "bytes": "CD21",
            },
            {
                "address": 0x0007,
                "mnemonic": "ret",
                "op_str": "",
                "bytes": "C3",
            },
        ],
    }
    lines = render_function(func, set())
    src = "\n".join(lines)
    assert "CF = dos_close_file(0x0005);" in src


def test_dos_exit_preserves_ah_after_al_write():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "mov",
                "op_str": "ah, 0x4C",
                "bytes": "B44C",
            },
            {
                "address": 0x0002,
                "mnemonic": "mov",
                "op_str": "al, 0x00",
                "bytes": "B000",
            },
            {
                "address": 0x0004,
                "mnemonic": "int",
                "op_str": "0x21",
                "bytes": "CD21",
            },
            {
                "address": 0x0006,
                "mnemonic": "ret",
                "op_str": "",
                "bytes": "C3",
            },
        ],
    }
    lines = render_function(func, set())
    src = "\n".join(lines)
    assert "dos_exit();" in src


def test_dos_string_pointer_does_not_use_stale_dx_after_dl_write():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "mov",
                "op_str": "dx, 0x1200",
                "bytes": "BA0012",
            },
            {
                "address": 0x0003,
                "mnemonic": "mov",
                "op_str": "dl, 0x41",
                "bytes": "B241",
            },
            {
                "address": 0x0005,
                "mnemonic": "mov",
                "op_str": "ah, 0x09",
                "bytes": "B409",
            },
            {
                "address": 0x0007,
                "mnemonic": "int",
                "op_str": "0x21",
                "bytes": "CD21",
            },
            {
                "address": 0x0009,
                "mnemonic": "ret",
                "op_str": "",
                "bytes": "C3",
            },
        ],
    }
    lines = render_function(func, set())
    src = "\n".join(lines)
    assert "dx = 0x1200;" in src
    assert "dl = 0x41;" in src
    assert "CF = dos_print_string((const char *)seg_off(ds, dx));" in src
    assert "CF = dos_print_string((const char *)seg_off(ds, 0x1200));" not in src
