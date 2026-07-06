from compiler.ir_to_c import CCodeRenderer, build_basic_blocks
from compiler import cfg
from compiler.ast_nodes import LoopNode, IfElseNode, ReturnNode


def test_iterative_structure_handles_multiple_passes():
    instrs = [
        {"address": 0x0, "mnemonic": "jnz", "bytes": "0000", "op_str": "0x6"},
        {"address": 0x2, "mnemonic": "jmp", "bytes": "0000", "op_str": "0x0"},
        {"address": 0x6, "mnemonic": "jnz", "bytes": "0000", "op_str": "0xA"},
        {"address": 0x8, "mnemonic": "jmp", "bytes": "0000", "op_str": "0xC"},
        {"address": 0xA, "mnemonic": "jmp", "bytes": "0000", "op_str": "0xC"},
        {"address": 0xC, "mnemonic": "ret", "bytes": "00", "op_str": ""},
    ]
    blocks = build_basic_blocks(instrs)
    graph = cfg.build_cfg(blocks)
    renderer = CCodeRenderer()
    nodes = renderer.structure(blocks, graph, set(), min(blocks))
    types = [type(n) for n in nodes]
    assert types == [LoopNode, IfElseNode, ReturnNode]
