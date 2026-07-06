# flake8: noqa
from compiler.ir_to_c import CCodeRenderer  # noqa: E402


def render(func, known):
    return "\n".join(CCodeRenderer().render_function(func, known))


def test_jmp_known_function_entry_without_block_sets_pc():
    func = {
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "jmp", "op_str": "0100", "bytes": ""},
            {"address": 0x0003, "mnemonic": "ret", "op_str": "", "bytes": ""},
        ],
    }
    src = render(func, {0x0000, 0x0100})
    # Faithful flat model: a near jmp to a known function start sets pc and
    # continues the dispatch loop -- no C tail-call into func_0100().
    assert "pc = 0x0100;" in src
    assert "func_0100();" not in src


def test_jmp_known_function_entry_with_block_sets_pc():
    func = {
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "jmp", "op_str": "0006", "bytes": "", "target": 0x0006},
            {"address": 0x0003, "mnemonic": "ret", "op_str": "", "bytes": ""},
            {"address": 0x0006, "mnemonic": "ret", "op_str": "", "bytes": ""},
        ],
    }
    src = render(func, {0x0000, 0x0006})
    assert "pc = 0x0006;" in src
    assert "func_0006();" not in src
