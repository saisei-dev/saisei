import sys
from pathlib import Path

sys.path.append(str(Path(__file__).resolve().parents[1]))

from compiler.ir_to_c import CCodeRenderer  # noqa: E402


def render(func):
    return "\n".join(CCodeRenderer().render_function(func, set()))


def test_mov_ss_sets_interrupt_shadow():
    func = {
        "start": 0,
        "instructions": [
            {
                "address": 0,
                "mnemonic": "mov",
                "op_str": "ss, ax",
                "bytes": "8E D0",
            },
            {"address": 2, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    }
    src = render(func)
    assert "ss = ax;" in src
    assert src.count("interrupt_shadow = 1;") == 1


def test_pop_ss_sets_interrupt_shadow():
    func = {
        "start": 0,
        "instructions": [
            {
                "address": 0,
                "mnemonic": "pop",
                "op_str": "ss",
                "bytes": "17",
            },
            {"address": 1, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    }
    src = render(func)
    assert "ss = memw(ss, sp);" in src
    assert src.count("interrupt_shadow = 1;") == 1
