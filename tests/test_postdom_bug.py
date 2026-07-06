from compiler.ir_to_c import CCodeRenderer, build_basic_blocks
from compiler import cfg


def test_structure_handles_missing_postdom():
    instrs = [
        {"address": 0, "mnemonic": "jnz", "bytes": "00" * 20, "op_str": "0xA"},
        {"address": 0xA, "mnemonic": "jmp", "bytes": "00", "op_str": "0xA"},
        {"address": 0x14, "mnemonic": "ret", "bytes": "00", "op_str": ""},
    ]
    blocks = build_basic_blocks(instrs)
    graph = cfg.build_cfg(blocks)
    renderer = CCodeRenderer()
    renderer.structure(blocks, graph, set(), min(blocks))
