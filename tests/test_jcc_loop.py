from compiler.ir_to_c import jcc_condition


def test_loopne_condition_uses_zero_flag():
    prev = {"mnemonic": "cmp", "op_str": "ax, bx"}
    assert jcc_condition("loopne", prev) == "--cx != 0 && ZF == 0"


def test_loope_condition_uses_zero_flag():
    prev = {"mnemonic": "cmp", "op_str": "ax, bx"}
    assert jcc_condition("loope", prev) == "--cx != 0 && ZF == 1"
