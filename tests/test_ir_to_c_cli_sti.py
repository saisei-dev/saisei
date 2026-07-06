import sys
from pathlib import Path

sys.path.append(str(Path(__file__).resolve().parents[1]))

from compiler.ir_to_c import CCodeRenderer  # noqa: E402


def render(func):
    return "\n".join(CCodeRenderer().render_function(func, set()))


def test_cli_sti_translation():
    func = {
        "start": 0,
        "instructions": [
            {"address": 0, "mnemonic": "cli", "op_str": "", "bytes": "FA"},
            {"address": 1, "mnemonic": "sti", "op_str": "", "bytes": "FB"},
            {"address": 2, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    }
    src = render(func)
    assert "IF = 0;" in src
    assert "IF = 1;" in src
    assert "interrupt_shadow = 1;" in src
    assert src.count("SAFEPOINT();") == 3
