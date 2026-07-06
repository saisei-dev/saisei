from compiler.ir_to_c import jcc_condition


def test_jcc_condition_includes_address_for_unsupported():
    comment = jcc_condition("jfoo", address=0x1234)
    assert comment == "/* unsupported jcc at 0x1234 */"
