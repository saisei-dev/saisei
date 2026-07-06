from compiler.ir_to_c import CCodeRenderer  # noqa: E402


def render(func):
    return "\n".join(CCodeRenderer().render_function(func, set()))


def test_iret_invokes_helper():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "iret",
                "op_str": "",
                "bytes": "CF",
            },
        ],
    }
    src = render(func)
    assert "iret();" in src
    assert "/* iret */" not in src
