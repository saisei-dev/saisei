from compiler.ir_to_c import CCodeRenderer  # noqa: E402


def render(func):
    return "\n".join(CCodeRenderer().render_function(func, set()))


def test_ljmp_uses_long_jump():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "ljmp",
                "op_str": "0x2000:0x1000",
                "bytes": "EA00100020",
            },
            {
                "address": 0x0005,
                "mnemonic": "ret",
                "op_str": "",
                "bytes": "C3",
            },
        ],
    }
    src = render(func)
    assert "long_jump(0x2000, 0x1000);\n    return;" in src
    assert "// ASM: ljmp 0x2000:0x1000" in src


def test_ljmp_memory_operand():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "ljmp",
                "op_str": "cs:[0x8ad]",
                "bytes": "2EFF2EAD08",
            },
            {
                "address": 0x0005,
                "mnemonic": "ret",
                "op_str": "",
                "bytes": "C3",
            },
        ],
    }
    src = render(func)
    assert (
        "long_jump(memw(cs, 0x08AF), memw(cs, 0x08AD));\n    return;" in src
    )
    assert "// ASM: ljmp cs:[0x8ad]" in src


def test_ljmp_memory_operand_no_segment_prefix():
    # FF /5: far indirect jump through memory with the default segment (DS).
    # some programs use this at 0x1612E (`ljmp [0x2ffc]`). The far pointer
    # is offset@[disp], segment@[disp+2].
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "ljmp",
                "op_str": "[0x2ffc]",
                "bytes": "FF2EFC2F",
                "detail": {"mem_refs": [{"segment": "DS", "disp": 0x2FFC}]},
            },
            {
                "address": 0x0004,
                "mnemonic": "ret",
                "op_str": "",
                "bytes": "C3",
            },
        ],
    }
    src = render(func)
    assert (
        "long_jump(memw(ds, 0x2FFE), memw(ds, 0x2FFC));\n    return;" in src
    )
    assert "// ASM: ljmp [0x2ffc]" in src
