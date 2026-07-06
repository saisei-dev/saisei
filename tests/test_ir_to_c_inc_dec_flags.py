from compiler import ir_to_c


def render(insn):
    renderer = ir_to_c.CCodeRenderer()
    return renderer.handle_arithmetic(insn, set())


def test_inc_sets_sf_and_of():
    lines = render({"mnemonic": "inc", "op_str": "al"})
    src = "\n".join(lines)
    assert "SF = (al >> 7) & 1;" in src
    assert "OF = old == 0x7F;" in src


def test_dec_sets_sf_and_of():
    lines = render({"mnemonic": "dec", "op_str": "ax"})
    src = "\n".join(lines)
    assert "SF = (ax >> 15) & 1;" in src
    assert "OF = old == 0x8000;" in src
