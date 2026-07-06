from compiler.ir_to_c import CCodeRenderer  # noqa: E402

# flake8: noqa


def render(func):
    return "\n".join(CCodeRenderer().render_function(func, set()))


def test_ret_near():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "mov",
                "op_str": "ax, ax",
                "bytes": "89C0",
            },
            {"address": 0x0002, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    }
    src = render(func)
    assert "return;" in src
    assert "retf();" not in src


def test_ret_far_invokes_helper():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "mov",
                "op_str": "ax, ax",
                "bytes": "89C0",
            },
            {"address": 0x0002, "mnemonic": "retf", "op_str": "", "bytes": "CB"},
        ],
    }
    src = render(func)
    assert "retf();" in src
    # retf is now followed by `return;` so the dispatch case exits cleanly when
    # a depth-0 far-jump retf returns from long_jump_impl (dead code on the
    # depth>0 longjmp path). See handle_ret / retf_common_impl.
    assert "retf();\n    return;" in src


def test_ret_far_only():
    func = {
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "retf", "op_str": "", "bytes": "CB"},
        ],
    }
    src = render(func)
    assert "retf();" in src
    # When retf is the function's last statement the trailing `return;` is
    # elided as redundant (falling off a void function == return). The explicit
    # `return;` only appears in the dispatch form where other cases follow
    # (see test_ret_far_invokes_helper).
    assert src.rstrip().endswith("retf();\n}")


def test_ret_immediate_adjusts_sp():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "mov",
                "op_str": "ax, ax",
                "bytes": "89C0",
            },
            {"address": 0x0002, "mnemonic": "ret", "op_str": "4", "bytes": "C20400"},
        ],
    }
    src = render(func)
    assert "sp = (sp + 4) & 0xFFFF;" in src
    assert "return;" in src




def test_ret_far_immediate_uses_retf_pop():
    func = {
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "retf", "op_str": "4", "bytes": "CA0400"},
        ],
    }
    src = render(func)
    assert "retf_pop(4);" in src
    assert "sp = (sp + 4) & 0xFFFF;" not in src


def test_ret_far_decimal_immediate_preserves_decimal_value():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "retf",
                "op_str": "12",
                "bytes": "CA0C00",
            },
        ],
    }
    src = render(func)
    assert "retf_pop(12);" in src
    assert "retf_pop(18);" not in src

def test_ret_far_in_if_else():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "cmp",
                "op_str": "ax, 0",
                "bytes": "3D0000",
            },
            {
                "address": 0x0003,
                "mnemonic": "jnz",
                "op_str": "0x0008",
                "bytes": "7503",
            },
            {
                "address": 0x0005,
                "mnemonic": "mov",
                "op_str": "ax, ax",
                "bytes": "89C0",
            },
            {"address": 0x0007, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
            {"address": 0x0008, "mnemonic": "retf", "op_str": "", "bytes": "CB"},
        ],
    }
    src = render(func)
    assert "retf();" in src
