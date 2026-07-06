# flake8: noqa
from compiler.ir_to_c import CCodeRenderer  # noqa: E402



def render(func):
    return "\n".join(CCodeRenderer().render_function(func, set()))


def assert_jump_table(src: str) -> None:
    assert "jump_table((((uint32_t)cs << 4) +" in src
    assert "& 0xFFFFF, expected_retip);" in src
    assert "return;" in src
    assert "pc =" not in src
    assert " t = " not in src


def test_jmp_word_ptr_cs_uses_jump_table():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "jmp",
                "op_str": "word ptr cs:[0x10c]",
                "bytes": "FF2E0C01",
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
    assert "// ASM: jmp word ptr cs:[0x10c]" in src
    assert (
        "jump_table((((uint32_t)cs << 4) + (uint16_t)(memw(cs, 0x10c))) & 0xFFFFF, expected_retip);" in src
    )
    assert_jump_table(src)


def test_jmp_word_ptr_cs_with_bp_index_uses_jump_table():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "jmp",
                "op_str": "word ptr cs:[bp + 0x10c]",
                "bytes": "FFAE0C01",
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
    assert "// ASM: jmp word ptr cs:[bp + 0x10c]" in src
    assert (
        "jump_table((((uint32_t)cs << 4) + (uint16_t)(memw(cs, (bp + 0x10c) & 0xFFFF))) & 0xFFFFF, expected_retip);" in src
    )
    assert_jump_table(src)


def test_jmp_word_ptr_cs_with_bx_register_uses_jump_table():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "jmp",
                "op_str": "word ptr cs:[bx]",
                "bytes": "FF27",
                "detail": {"mem_refs": [{"segment": "CS", "disp": 0x0, "access": "read"}]},
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
    assert "// ASM: jmp word ptr cs:[bx]" in src
    assert (
        "jump_table((((uint32_t)cs << 4) + (uint16_t)(memw(cs, bx))) & 0xFFFFF, expected_retip);" in src
    )
    assert_jump_table(src)


def test_jmp_known_function_sets_pc():
    func = {
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "jmp", "op_str": "0100", "bytes": ""},
            {"address": 0x0003, "mnemonic": "ret", "op_str": "", "bytes": ""},
        ],
    }
    renderer = CCodeRenderer()
    src = "\n".join(renderer.render_function(func, {0x0000, 0x0100}))
    # Faithful flat model: a near jmp to a known function start sets pc and
    # continues -- no C tail-call into func_0100().
    assert "pc = 0x0100;" in src
    assert "func_0100();" not in src
    assert "// ASM: jmp 0100" in src


def test_jmp_word_ptr_es_with_bx_uses_jump_table():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "jmp",
                "op_str": "word ptr es:[bx]",
                "bytes": "26FF27",
                "detail": {"mem_refs": [{"segment": "ES", "disp": 0x0, "access": "read"}]},
            },
            {
                "address": 0x0003,
                "mnemonic": "ret",
                "op_str": "",
                "bytes": "C3",
            },
        ],
    }
    src = render(func)
    assert "// ASM: jmp word ptr es:[bx]" in src
    assert (
        "jump_table((((uint32_t)cs << 4) + (uint16_t)(memw(es, bx))) & 0xFFFFF, expected_retip);" in src
    )
    assert_jump_table(src)


def test_jmp_word_ptr_es_with_offset_uses_jump_table():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "jmp",
                "op_str": "word ptr es:[0x010C]",
                "bytes": "26FF2E0C01",
                "detail": {"mem_refs": [{"segment": "ES", "disp": 0x10C, "access": "read"}]},
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
    assert "// ASM: jmp word ptr es:[0x010C]" in src
    assert (
        "jump_table((((uint32_t)cs << 4) + (uint16_t)(memw(es, 0x010c))) & 0xFFFFF, expected_retip);" in src
    )
    assert_jump_table(src)


def test_jmp_word_ptr_without_segment_uses_ds():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "jmp",
                "op_str": "word ptr [0x010C]",
                "bytes": "FF260C01",
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
    assert "// ASM: jmp word ptr [0x010C]" in src
    assert (
        "jump_table((((uint32_t)cs << 4) + (uint16_t)(memw(ds, 0x010c))) & 0xFFFFF, expected_retip);" in src
    )
    assert_jump_table(src)


def test_jmp_word_ptr_bp_defaults_to_ss():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "jmp",
                "op_str": "word ptr [bp + 0x10c]",
                "bytes": "FF660C01",
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
    assert "// ASM: jmp word ptr [bp + 0x10c]" in src
    assert (
        "jump_table((((uint32_t)cs << 4) + (uint16_t)(memw(ss, (bp + 0x10c) & 0xFFFF))) & 0xFFFFF, expected_retip);" in src
    )
    assert_jump_table(src)


def test_jmp_word_ptr_cs_with_negative_offset_uses_jump_table():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "jmp",
                "op_str": "word ptr cs:[bx - 0x10]",
                "bytes": "",
                "detail": {"mem_refs": [{"segment": "CS", "disp": -0x10, "access": "read"}]},
            },
            {
                "address": 0x0003,
                "mnemonic": "ret",
                "op_str": "",
                "bytes": "",
            },
        ],
    }
    src = render(func)
    assert "// ASM: jmp word ptr cs:[bx - 0x10]" in src
    assert (
        "jump_table((((uint32_t)cs << 4) + (uint16_t)(memw(cs, (bx - 0x10) & 0xFFFF))) & 0xFFFFF, expected_retip);" in src
    )
    assert_jump_table(src)
