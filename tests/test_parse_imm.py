import pytest

from compiler.cfg import _parse_imm as cfg_parse
from compiler.ir_to_c import _parse_imm as ir_parse

parsers = [cfg_parse, ir_parse]


@pytest.mark.parametrize("parser", parsers)
@pytest.mark.parametrize("token,expected", [
    ("-42", -42),
    ("-0x2A", -42),
    ("-2Ah", -42),
])
def test_parse_negative_immediates(parser, token, expected):
    assert parser(token) == expected
