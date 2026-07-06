from pathlib import Path
import sys

sys.path.append(str(Path(__file__).resolve().parents[1]))

import networkx as nx  # noqa: E402
from compiler.basic_block import BasicBlock  # noqa: E402
from compiler import cfg  # noqa: E402


def _block(addr: int, mnemonic: str) -> BasicBlock:
    b = BasicBlock(addr)
    b.instructions = [
        {"address": addr, "mnemonic": mnemonic, "op_str": "", "bytes": ""}
    ]
    return b


def test_merge_shared_tails_merges_identical(monkeypatch):
    blocks = {
        0: _block(0, "nop"),
        1: _block(1, "ret"),
        2: _block(2, "ret"),
        3: _block(3, "nop"),
        4: _block(4, "nop"),
    }
    g = nx.DiGraph()
    g.add_edges_from(
        [(0, 1), (0, 2), (1, 3), (2, 3), (4, 1), (4, 2)]
    )
    ipdom = {0: 1, 1: 2, 2: 3, 3: None, 4: 1}
    monkeypatch.setattr(
        cfg, "compute_immediate_postdominators", lambda graph: ipdom
    )
    merged = cfg.merge_shared_tails(blocks, g)
    assert 2 not in merged.nodes
    assert merged.number_of_nodes() == 4
    assert merged.has_edge(0, 1)
    assert merged.has_edge(1, 3)


def test_merge_shared_tails_requires_matching_successors(monkeypatch):
    blocks = {
        0: _block(0, "nop"),
        1: _block(1, "ret"),
        2: _block(2, "ret"),
        3: _block(3, "nop"),
        4: _block(4, "nop"),
        5: _block(5, "nop"),
    }
    g = nx.DiGraph()
    g.add_edges_from(
        [(0, 1), (0, 2), (1, 3), (2, 4), (4, 3), (5, 1), (5, 2)]
    )
    ipdom = {0: 1, 1: 2, 2: 3, 3: None, 4: 3, 5: 1}
    monkeypatch.setattr(
        cfg, "compute_immediate_postdominators", lambda graph: ipdom
    )
    merged = cfg.merge_shared_tails(blocks, g)
    assert 2 in merged.nodes
    assert merged.has_edge(2, 4)
