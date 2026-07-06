# flake8: noqa
import sys
from pathlib import Path

sys.path.append(str(Path(__file__).resolve().parents[1]))

from compiler.ir_to_c import CCodeRenderer  # noqa: E402


def render(func):
    return "\n".join(CCodeRenderer().render_function(func, set()))


def test_lcall_cs_indirect():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "lcall",
                "op_str": "cs:[0xff10]",
                "bytes": "2EFF1E10FF",
                "detail": {"mem_refs": [{"segment": "CS", "disp": 0xFF10, "access": "read"}]},
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
        "lcall_table((uint16_t)(0x00005U + 0x10100U - ((uint32_t)cs << 4)), memw(cs, 0xff12), memw(cs, 0xff10));"
        in src
    )
    assert "// ASM: lcall cs:[0xff10]" in src


def test_lcall_cs_indirect_other_offset():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "lcall",
                "op_str": "cs:[0xff0c]",
                "bytes": "2EFF1E0CFF",
                "detail": {"mem_refs": [{"segment": "CS", "disp": 0xFF0C, "access": "read"}]},
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
        "lcall_table((uint16_t)(0x00005U + 0x10100U - ((uint32_t)cs << 4)), memw(cs, 0xff0e), memw(cs, 0xff0c));"
        in src
    )
    assert "// ASM: lcall cs:[0xff0c]" in src


def test_lcall_indirect_register():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "lcall",
                "op_str": "[bx]",
                "bytes": "",
                "detail": {
                    "mem_refs": [
                        {
                            "segment": "DS",
                            "base": "BX",
                            "index": None,
                            "scale": 1,
                            "disp": 0,
                            "access": "read",
                        }
                    ]
                },
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
        "lcall_table((uint16_t)(0x00005U + 0x10100U - ((uint32_t)cs << 4)), memw(ds, bx + 0x0002), memw(ds, bx));"
        in src
    )
    assert "// ASM: lcall [bx]" in src
    assert "// TODO ASM: lcall [bx]" not in src


def test_call_after_push_cs_pop_ds_is_near():
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
                "op_str": "ds",
                "bytes": "1F",
            },
            {
                "address": 0x0002,
                "mnemonic": "call",
                "op_str": "word ptr cs:[0x1000]",
                "bytes": "2EFF161000",
                "detail": {
                    "mem_refs": [
                        {"segment": "CS", "disp": 0x1000, "access": "read"}
                    ]
                },
            },
            {
                "address": 0x0007,
                "mnemonic": "ret",
                "op_str": "",
                "bytes": "C3",
            },
        ],
    }
    src = render(func)
    assert "call_table((uint16_t)(0x00007U + 0x10100U - ((uint32_t)cs << 4)), (((uint32_t)cs << 4) + (uint16_t)(memw(cs, 0x1000))" in src
    assert "lcall_table" not in src


def test_call_after_push_cs_with_intervening_instruction_is_near():
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
                "mnemonic": "mov",
                "op_str": "ax, ax",
                "bytes": "89C0",
            },
            {
                "address": 0x0003,
                "mnemonic": "call",
                "op_str": "word ptr cs:[0x1000]",
                "bytes": "2EFF161000",
                "detail": {
                    "mem_refs": [
                        {"segment": "CS", "disp": 0x1000, "access": "read"}
                    ]
                },
            },
            {
                "address": 0x0008,
                "mnemonic": "ret",
                "op_str": "",
                "bytes": "C3",
            },
        ],
    }
    src = render(func)
    assert "call_table((uint16_t)(0x00008U + 0x10100U - ((uint32_t)cs << 4)), (((uint32_t)cs << 4) + (uint16_t)(memw(cs, 0x1000))" in src
    assert "lcall_table" not in src
