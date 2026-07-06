# flake8: noqa
from compiler.ir_to_c import CCodeRenderer  # noqa: E402


def render(func):
    return "\n".join(CCodeRenderer().render_function(func, set()))


def test_call_word_ptr_cs_uses_call_table():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "call",
                "op_str": "word ptr cs:[0x10c]",
                "bytes": "FF1E0C01",
                "detail": {"mem_refs": [{"segment": "CS", "disp": 0x10C, "access": "read"}]},
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
        "call_table((uint16_t)(0x00005U + 0x10100U - ((uint32_t)cs << 4)), (((uint32_t)cs << 4) + (uint16_t)(memw(cs, 0x10c))) & 0xFFFFF);"
        in src
    )
    assert "// ASM: call word ptr cs:[0x10c]" in src


def test_call_word_ptr_cs_with_bp_index_uses_call_table():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "call",
                "op_str": "word ptr cs:[bp + 0x10c]",
                "bytes": "FF9E0C01",
                "detail": {"mem_refs": [{"segment": "CS", "disp": 0x10C, "access": "read"}]},
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
        "call_table((uint16_t)(0x00005U + 0x10100U - ((uint32_t)cs << 4)), (((uint32_t)cs << 4) + (uint16_t)(memw(cs, (bp + 0x10c) & 0xFFFF))) & 0xFFFFF);"
        in src
    )
    assert "// ASM: call word ptr cs:[bp + 0x10c]" in src


def test_call_word_ptr_without_segment_uses_cs_for_call_table():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "call",
                "op_str": "word ptr [0x010C]",
                "bytes": "FF160C01",
                "detail": {"mem_refs": [{"segment": "DS", "disp": 0x10C, "access": "read"}]},
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
        "call_table((uint16_t)(0x00005U + 0x10100U - ((uint32_t)cs << 4)), (((uint32_t)cs << 4) + (uint16_t)(memw(ds, 0x010c))) & 0xFFFFF);"
        in src
    )
    assert "// ASM: call word ptr [0x010C]" in src


def test_call_word_ptr_bp_defaults_to_ss_for_mem_and_cs_for_call_table():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "call",
                "op_str": "word ptr [bp + 0x10c]",
                "bytes": "FF960C01",
                "detail": {"mem_refs": [{"segment": "SS", "disp": 0x10C, "access": "read"}]},
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
        "call_table((uint16_t)(0x00005U + 0x10100U - ((uint32_t)cs << 4)), (((uint32_t)cs << 4) + (uint16_t)(memw(ss, (bp + 0x10c) & 0xFFFF))) & 0xFFFFF);"
        in src
    )
    assert "// ASM: call word ptr [bp + 0x10c]" in src


def test_call_register_uses_call_table():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "call",
                "op_str": "ax",
                "bytes": "FFD0",
            },
            {
                "address": 0x0002,
                "mnemonic": "ret",
                "op_str": "",
                "bytes": "C3",
            },
        ],
    }
    src = render(func)
    assert "call_table((uint16_t)(0x00002U + 0x10100U - ((uint32_t)cs << 4)), (((uint32_t)cs << 4) + (uint16_t)(ax)) & 0xFFFFF);" in src
    assert "// ASM: call ax" in src
    assert "// TODO ASM: call ax" not in src
