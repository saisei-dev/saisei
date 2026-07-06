from compiler.ir_to_c import CCodeRenderer, _NORETURN_FUNCS


def test_terminates_on_noreturn_funcs():
    renderer = CCodeRenderer()
    for func in _NORETURN_FUNCS:
        assert renderer._terminates(f"{func}();")
