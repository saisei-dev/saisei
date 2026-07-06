from compiler.ir_to_c import jcc_condition


def test_parity_conditions():
    assert jcc_condition("jpo") == "PF == 0"
    assert jcc_condition("jpe") == "PF == 1"
