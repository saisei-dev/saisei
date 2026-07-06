from compiler.ir_to_c import build_basic_blocks, normalize_indirect_jumps
from compiler import cfg


def test_indirect_jump_ignored():
    instrs = [
        {
            "address": 0x0,
            "mnemonic": "jmp",
            "bytes": "00",
            "op_str": "word ptr cs:[bx + 0x588]",
        },
        {"address": 0x100, "mnemonic": "nop", "bytes": "00", "op_str": ""},
        {"address": 0x588, "mnemonic": "ret", "bytes": "00", "op_str": ""},
    ]
    instrs = normalize_indirect_jumps(instrs)
    blocks = build_basic_blocks(instrs)
    assert list(blocks) == [0x0]
    assert blocks[0].instructions[0]["op"] == "INDIRECT_NEAR_JMP"
    graph = cfg.build_cfg(blocks)
    assert graph.out_degree(0x0) == 0
