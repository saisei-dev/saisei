# flake8: noqa
from compiler.ir_to_c import CCodeRenderer, _match_rcb_access  # noqa: E402


def render(func):
    return "\n".join(CCodeRenderer().render_function(func, set()))


def assert_jump_table(src: str) -> None:
    assert "jump_table((((uint32_t)cs << 4) +" in src
    assert "& 0xFFFFF, expected_retip);" in src
    assert "return;" in src
    assert "pc =" not in src
    assert " t = " not in src


def test_jmp_word_ptr_es_rcb_field_uses_rcb_read16():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "jmp",
                "op_str": "word ptr es:[0xFF0C]",
                "bytes": "",
                "detail": {"mem_refs": [{"segment": "ES", "disp": 0xFF0C, "access": "read"}]},
            },
            {
                "address": 0x0005,
                "mnemonic": "ret",
                "op_str": "",
                "bytes": "",
            },
        ],
    }
    src = render(func)
    assert "// ASM: jmp word ptr es:[0xFF0C]" in src
    assert (
        "jump_table((((uint32_t)cs << 4) + (uint16_t)(rcb_read16(DATA_BUF1_OFF))) & 0xFFFFF, expected_retip);"
        in src
    )
    assert_jump_table(src)


def test_ljmp_es_rcb_fields_use_rcb_read16():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "ljmp",
                "op_str": "es:[0xFF04]",
                "bytes": "",
                "detail": {"mem_refs": [{"segment": "ES", "disp": 0xFF04, "access": "read"}]},
            },
            {
                "address": 0x0005,
                "mnemonic": "ret",
                "op_str": "",
                "bytes": "",
            },
        ],
    }
    src = render(func)
    assert (
        "long_jump(rcb_read16(PREV_TIMER_VECTOR_SEG), rcb_read16(PREV_TIMER_VECTOR_OFF));" in src
    )
    assert "// ASM: ljmp es:[0xFF04]" in src


def test_jmp_word_ptr_es_lowercase_hex():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "jmp",
                "op_str": "word ptr es:[0xff0c]",
                "bytes": "",
                "detail": {"mem_refs": [{"segment": "ES", "disp": 0xFF0C, "access": "read"}]},
            },
            {
                "address": 0x0005,
                "mnemonic": "ret",
                "op_str": "",
                "bytes": "",
            },
        ],
    }
    src = render(func)
    assert "// ASM: jmp word ptr es:[0xff0c]" in src
    assert (
        "jump_table((((uint32_t)cs << 4) + (uint16_t)(rcb_read16(DATA_BUF1_OFF))) & 0xFFFFF, expected_retip);"
        in src
    )
    assert_jump_table(src)


def test_match_rcb_access_lowercase_name():
    assert _match_rcb_access("memw(es, data_buf1_off)") == ("w", "DATA_BUF1_OFF")


def test_match_rcb_access_lowercase_hex():
    assert _match_rcb_access("memw(es, 0xff0c)") == ("w", "DATA_BUF1_OFF")
