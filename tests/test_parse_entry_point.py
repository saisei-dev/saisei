from compiler.disassemble import parse_entry_point


def test_parse_segment_offset_hex_without_prefix() -> None:
    assert parse_entry_point("0100:0010") == 0x1010


def test_parse_segment_offset_with_prefix() -> None:
    assert parse_entry_point("0x0100:0x0010") == 0x1010
