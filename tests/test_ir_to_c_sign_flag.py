from compiler import ir_to_c


def render(insn):
    renderer = ir_to_c.CCodeRenderer()
    return renderer.handle_arithmetic(insn, set())


def test_add_sets_sf():
    lines = render({'mnemonic': 'add', 'op_str': 'al, 1'})
    assert 'SF = (al >> 7) & 1;' in lines


def test_sub_sets_sf():
    lines = render({'mnemonic': 'sub', 'op_str': 'ax, bx'})
    assert 'SF = (ax >> 15) & 1;' in lines


def test_or_sets_sf():
    lines = render({'mnemonic': 'or', 'op_str': 'al, al'})
    assert 'SF = (al >> 7) & 1;' in lines


def test_and_sets_sf():
    lines = render({'mnemonic': 'and', 'op_str': 'ax, bx'})
    assert 'SF = (ax >> 15) & 1;' in lines


def test_xor_sets_sf():
    lines = render({'mnemonic': 'xor', 'op_str': 'al, ah'})
    assert 'SF = (al >> 7) & 1;' in lines
