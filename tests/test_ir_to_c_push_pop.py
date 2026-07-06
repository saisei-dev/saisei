from pathlib import Path
import sys

sys.path.append(str(Path(__file__).resolve().parents[1]))

from compiler.ir_to_c import CCodeRenderer  # noqa: E402


def render(func):
    return "\n".join(CCodeRenderer().render_function(func, set()))


def test_push_register():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "push",
                "op_str": "ax",
                "bytes": "50",
            },
            {
                "address": 0x0001,
                "mnemonic": "ret",
                "op_str": "",
                "bytes": "C3",
            },
        ],
    }
    src = render(func)
    assert "sp = (sp - 2) & 0xFFFF;" in src
    assert "memw_write(ss, sp, ax);" in src


def test_push_immediate():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "push",
                "op_str": "0x1234",
                "bytes": "683412",
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
    assert "sp = (sp - 2) & 0xFFFF;" in src
    assert "memw_write(ss, sp, 0x1234);" in src


def test_push_sp_uses_pre_decrement_value():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "push",
                "op_str": "sp",
                "bytes": "54",
            },
            {
                "address": 0x0001,
                "mnemonic": "ret",
                "op_str": "",
                "bytes": "C3",
            },
        ],
    }
    src = render(func)
    assert "uint16_t push_value = sp;" in src
    assert "sp = (sp - 2) & 0xFFFF;" in src
    assert "memw_write(ss, sp, push_value);" in src


def test_push_memory():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "push",
                "op_str": "word ptr es:[di]",
                "bytes": "2EFF",
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
    assert "sp = (sp - 2) & 0xFFFF;" in src
    assert "memw_write(ss, sp, memw(es, di));" in src


def test_pop_register():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "pop",
                "op_str": "ax",
                "bytes": "58",
            },
            {
                "address": 0x0001,
                "mnemonic": "ret",
                "op_str": "",
                "bytes": "C3",
            },
        ],
    }
    src = render(func)
    assert "ax = memw(ss, sp);" in src
    assert "sp = (sp + 2) & 0xFFFF;" in src


def test_pop_memory():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "pop",
                "op_str": "word ptr [bp-2]",
                "bytes": "8F46FE",
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
    assert (
        "memw_write(ss, ((bp + 0xFFFE) & 0xFFFF), memw(ss, sp));" in src
    )
    assert "sp = (sp + 2) & 0xFFFF;" in src


def test_pop_memory_with_segment_override():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "pop",
                "op_str": "word ptr es:[di]",
                "bytes": "268F05",
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
    assert "memw_write(es, di, memw(ss, sp));" in src
    assert "sp = (sp + 2) & 0xFFFF;" in src


def test_push_pop_pair_preserves_stack_effect():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "push",
                "op_str": "cs",
                "bytes": "0E",
            },
            {
                "address": 0x0001,
                "mnemonic": "pop",
                "op_str": "es",
                "bytes": "07",
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
    assert "sp = (sp - 2) & 0xFFFF;" in src
    assert "memw_write(ss, sp, cs);" in src
    assert "es = memw(ss, sp);" in src
    assert "sp = (sp + 2) & 0xFFFF;" in src
