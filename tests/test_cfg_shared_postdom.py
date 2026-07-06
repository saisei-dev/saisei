from pathlib import Path
import sys

sys.path.append(str(Path(__file__).resolve().parents[1]))

from compiler.ir_to_c import CCodeRenderer  # noqa: E402
from compiler.ast_nodes import BasicBlockNode  # noqa: E402
import networkx as nx  # noqa: E402


def test_extract_shared_postdom_keeps_original_block():
    renderer = CCodeRenderer()
    block_a = BasicBlockNode(0x0000, [])
    shared1 = BasicBlockNode(0x0010, [])
    shared2 = BasicBlockNode(0x0010, [])
    nodes = [(0x0000, block_a), (0x0010, shared1), (0x0010, shared2)]
    graph = nx.DiGraph()
    graph.add_edge(0x0000, 0x0010)
    graph.add_edge(0x0005, 0x0010)
    result = renderer._extract_shared_postdom_blocks(nodes, graph)
    assert len(result) == 2
    assert isinstance(result[1][1], BasicBlockNode)
