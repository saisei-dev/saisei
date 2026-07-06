from pathlib import Path
import sys

# flake8: noqa
sys.path.append(str(Path(__file__).resolve().parents[1]))

from compiler.ir_to_c import CCodeRenderer  # noqa: E402


def render_function(func, known):
    return CCodeRenderer().render_function(func, known)


def test_int_clobbers_previous_cmp_flag():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "cmp",
                "op_str": "al, 1",
                "bytes": "3C01",
            },
            {
                "address": 0x0002,
                "mnemonic": "mov",
                "op_str": "ah, 0x3d",
                "bytes": "B43D",
            },
            {
                "address": 0x0004,
                "mnemonic": "int",
                "op_str": "21",
                "bytes": "CD21",
            },
            {
                "address": 0x0006,
                "mnemonic": "jmp",
                "op_str": "000A",
                "bytes": "E90300",
            },
            {
                "address": 0x000A,
                "mnemonic": "jb",
                "op_str": "000E",
                "bytes": "7202",
            },
            {
                "address": 0x000C,
                "mnemonic": "ret",
                "op_str": "",
                "bytes": "C3",
            },
            {
                "address": 0x000E,
                "mnemonic": "ret",
                "op_str": "",
                "bytes": "C3",
            },
        ],
    }
    lines = render_function(func, set())
    src = "\n".join(lines)
    assert "CF = dos_open_file((const char *)seg_off(ds, dx));" in src
    assert "if (CF == 1)" in src
    assert "if (al < 1)" not in src

def test_non_dos_int_does_not_preadvance_ip_before_run_interrupt():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "int",
                "op_str": "60",
                "bytes": "CD60",
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

    assert "run_interrupt(0x60);" in src
    assert "ip = 0x0002;" not in src
