import sys
from pathlib import Path

# flake8: noqa
sys.path.append(str(Path(__file__).resolve().parents[1]))

from compiler.ir_to_c import CCodeRenderer  # noqa: E402


def test_basic_block_starts_with_safepoint():
    func = {
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "mov", "op_str": "ax, bx", "bytes": "89D8"},
            {"address": 0x0002, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    }
    lines = CCodeRenderer().render_function(func, set())
    assert lines[2].strip() == "ip = 0x0000;"
    safepoints = [i for i, line in enumerate(lines) if line.strip() == "SAFEPOINT();"]
    assert safepoints == [3, 5]


def test_loop_bottom_has_safepoint():
    func = {
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "mov", "op_str": "cx, 0", "bytes": "B90000"},
            {"address": 0x0003, "mnemonic": "cmp", "op_str": "cx, 3", "bytes": "83F9"},
            {"address": 0x0006, "mnemonic": "jge", "op_str": "0x10", "bytes": "7D08"},
            {"address": 0x0008, "mnemonic": "inc", "op_str": "cx", "bytes": "41"},
            {"address": 0x0009, "mnemonic": "jmp", "op_str": "0x3", "bytes": "E9F9FF"},
            {"address": 0x0010, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    }
    lines = CCodeRenderer().render_function(func, set())
    start = next(i for i, line in enumerate(lines) if line.strip().startswith("while"))
    depth = 0
    end = None
    for i in range(start, len(lines)):
        stripped = lines[i].strip()
        if stripped.endswith("{"):
            depth += 1
        if stripped == "}":
            depth -= 1
            if depth == 0:
                end = i
                break
    assert end is not None
    assert lines[end - 1].strip() == "SAFEPOINT();"
