import sys
from pathlib import Path

# flake8: noqa
sys.path.append(str(Path(__file__).resolve().parents[1]))

from compiler.ir_to_c import CCodeRenderer, build_basic_blocks
from compiler import cfg
from compiler.ast_nodes import CommentNode, GotoNode, ReturnNode


def test_blocks_follow_cfg_traversal_order():
    instrs = [
        {"address": 0x0, "mnemonic": "jmp", "bytes": "00", "op_str": "0x100"},
        {"address": 0x2, "mnemonic": "ret", "bytes": "00", "op_str": ""},
        {"address": 0x100, "mnemonic": "jmp", "bytes": "00", "op_str": "0x2"},
    ]
    blocks = build_basic_blocks(instrs)
    graph = cfg.build_cfg(blocks)
    renderer = CCodeRenderer()
    nodes = renderer.structure(blocks, graph, set(), 0x0)
    starts = [node.start for node in nodes]
    assert starts == [0x0, 0x100, 0x2]
    types = [type(n) for n in nodes]
    assert types == [GotoNode, GotoNode, ReturnNode]


def test_comment_node_precedes_instruction():
    instrs = [
        {"address": 0x0, "mnemonic": "ret", "bytes": "00", "op_str": ""},
    ]
    blocks = build_basic_blocks(instrs)
    graph = cfg.build_cfg(blocks)
    renderer = CCodeRenderer(comments={0x0: "note"})
    nodes = renderer.structure(blocks, graph, set(), 0x0)
    types = [type(n) for n in nodes]
    assert types == [CommentNode, ReturnNode]
    lines: list[str] = []
    for node in nodes:
        lines.extend(node.render(renderer, "", set()))
    assert lines == ["// note", "return;"]
