from compiler.ir_to_c import build_basic_blocks
from compiler import cfg


def test_lcall_creates_fallthrough_block() -> None:
    instrs = [
        {
            "address": 0x0,
            "mnemonic": "lcall",
            "op_str": "0x2000:0x1000",
            "bytes": "9A00100020",
        },
        {"address": 0x5, "mnemonic": "nop", "op_str": "", "bytes": "90"},
    ]
    blocks = build_basic_blocks(instrs)
    assert list(blocks) == [0x0, 0x5]
    graph = cfg.build_cfg(blocks)
    assert graph.out_degree(0x0) == 1
