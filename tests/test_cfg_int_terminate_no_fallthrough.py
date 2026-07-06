from pathlib import Path
import sys

# Ensure repository root is on sys.path
sys.path.append(str(Path(__file__).resolve().parents[1]))

from compiler.ir_to_c import build_basic_blocks  # noqa: E402
from compiler import cfg  # noqa: E402


def test_cfg_no_fallthrough_after_int_20():
    instrs = [
        {
            "address": 0x0,
            "mnemonic": "int",
            "op_str": "0x20",
            "bytes": "CD20",
        },
        {"address": 0x2, "mnemonic": "nop", "op_str": "", "bytes": "90"},
    ]
    blocks = build_basic_blocks(instrs, {0x2})
    graph = cfg.build_cfg(blocks)
    assert graph.out_degree(0x0) == 0


def test_cfg_no_fallthrough_after_mov_ax_4c00_int_21():
    instrs = [
        {
            "address": 0x0,
            "mnemonic": "mov",
            "op_str": "ax, 0x4c00",
            "bytes": "B8004C",
        },
        {
            "address": 0x3,
            "mnemonic": "int",
            "op_str": "0x21",
            "bytes": "CD21",
        },
        {"address": 0x5, "mnemonic": "nop", "op_str": "", "bytes": "90"},
    ]
    blocks = build_basic_blocks(instrs, {0x5})
    graph = cfg.build_cfg(blocks)
    assert graph.out_degree(0x0) == 0


def test_cfg_no_fallthrough_after_mov_ah_4c_int_21():
    instrs = [
        {
            "address": 0x0,
            "mnemonic": "mov",
            "op_str": "ah, 0x4c",
            "bytes": "B44C",
        },
        {
            "address": 0x2,
            "mnemonic": "int",
            "op_str": "0x21",
            "bytes": "CD21",
        },
        {"address": 0x4, "mnemonic": "nop", "op_str": "", "bytes": "90"},
    ]
    blocks = build_basic_blocks(instrs, {0x4})
    graph = cfg.build_cfg(blocks)
    assert graph.out_degree(0x0) == 0
