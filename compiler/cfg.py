from __future__ import annotations

"""Control-flow graph utilities.

This module builds and analyses a control-flow graph (CFG) over basic blocks
emitted by the translator. Dominator computations rely on the classic
Lengauer-Tarjan algorithm via :mod:`networkx` and mirror techniques used in
decompilation literature such as *Native x86 Decompilation using
Semantics-Preserving Structural Analysis and Iterative Control-Flow
Structuring*.
"""

from typing import Dict, Iterable, Set  # noqa: E402

import networkx as nx  # noqa: E402

from .basic_block import BasicBlock


def _parse_imm(value: str) -> int | None:
    """Best-effort parse of an immediate value from assembly syntax."""

    import re

    value = value.strip()
    # Skip values that look like memory references (e.g. ``[bx+0x10]``) or
    # other complex expressions.  Only bare immediates are supported.
    if "[" in value or "]" in value or ":" in value:
        return None
    match = re.fullmatch(
        r"(?i)(?:short|near)?\s*([+-]?(?:0x[0-9a-f]+|[0-9a-f]+h|[0-9a-f]+))",
        value,
    )
    if not match:
        return None
    token = match.group(1)
    sign = ""
    if token[0] in "+-":
        sign, token = token[0], token[1:]
    if token[-1] in "hH":
        token = token[:-1]
        token = f"0x{token}"
    elif token.lower().startswith("0x"):
        pass
    elif token.isdigit() and sign:
        pass
    else:
        token = f"0x{token}"
    try:
        return int(f"{sign}{token}", 0)
    except ValueError:
        return None


def build_cfg(blocks: Dict[int, "BasicBlock"]) -> nx.DiGraph:
    """Return a directed graph representing control flow between *blocks*."""

    graph = nx.DiGraph()
    for addr in blocks:
        graph.add_node(addr)
    for addr, block in blocks.items():
        if not block.instructions:
            continue
        last = block.instructions[-1]
        mnem = last["mnemonic"]
        size = len(last["bytes"]) // 2
        cur_end = last["address"] + size
        if mnem.startswith("j") or mnem.startswith("loop") or mnem == "ljmp":
            target = _parse_imm(last.get("op_str", ""))
            # Add fallthrough edge before the explicit target so DFS traverses
            # natural control flow first.  This ensures subsequent ordering of
            # basic blocks follows the layout of the original code and avoids
            # emitting early returns before their associated body blocks.
            if mnem not in {"jmp", "ljmp"} and cur_end in blocks:
                graph.add_edge(addr, cur_end)
            if target is not None and target in blocks:
                graph.add_edge(addr, target)
        else:
            is_exit = mnem in {"ret", "retn", "retf", "hlt", "iret"}
            if mnem == "int":
                op = last.get("op_str", "")
                if op == "0x20":
                    is_exit = True
                elif op == "0x21":
                    prev = (
                        block.instructions[-2]
                        if len(block.instructions) >= 2
                        else None
                    )
                    if (
                        prev
                        and prev.get("mnemonic") == "mov"
                        and prev.get("op_str") in {"ax, 0x4c00", "ah, 0x4c"}
                    ):
                        is_exit = True
            if not is_exit and cur_end in blocks:
                graph.add_edge(addr, cur_end)
    return graph


def compute_dominators(graph: nx.DiGraph, entry: int) -> Dict[int, Set[int]]:
    """Compute dominator sets for ``graph`` starting at ``entry``.

    Nodes unreachable from ``entry`` are ignored to avoid ``KeyError`` when the
    graph has been pruned during iterative structuring.
    """

    reachable = {entry} | nx.descendants(graph, entry)
    subgraph = graph.subgraph(reachable)
    idom = nx.immediate_dominators(subgraph, entry)
    dominators: Dict[int, Set[int]] = {}
    for node in subgraph.nodes:
        cur = node
        dom: Set[int] = {cur}
        while cur != entry:
            cur = idom[cur]
            dom.add(cur)
        dominators[node] = dom
    return dominators


def compute_postdominators(
    graph: nx.DiGraph, exit: int
) -> Dict[int, Set[int]]:
    """Compute post-dominator sets for ``graph`` ending at ``exit``."""

    rev = graph.reverse(copy=False)
    idom = nx.immediate_dominators(rev, exit)
    post: Dict[int, Set[int]] = {}
    for node in rev.nodes:
        cur = node
        dom: Set[int] = {cur}
        while cur != exit:
            cur = idom[cur]
            dom.add(cur)
        post[node] = dom
    return post


def compute_immediate_postdominators(graph: nx.DiGraph) -> Dict[int, int]:
    """Return mapping of nodes to their immediate postdominators."""

    if not graph:
        return {}

    g = graph.copy()
    exit_node = max(g.nodes) + 1
    g.add_node(exit_node)
    for node in list(graph.nodes):
        if graph.out_degree(node) == 0:
            g.add_edge(node, exit_node)

    rev = g.reverse(copy=False)
    ipdom = nx.immediate_dominators(rev, exit_node)
    ipdom.pop(exit_node, None)
    return ipdom


def _postdominates(ipdom: Dict[int, int], node: int, cand: int) -> bool:
    """Return True if ``cand`` postdominates ``node`` via ``ipdom`` chain."""

    cur = node
    seen: Set[int] = set()
    while cur in ipdom:
        if cur in seen:
            return False
        seen.add(cur)
        cur = ipdom[cur]
        if cur == cand:
            return True
    return False


def merge_shared_tails(
    blocks: Dict[int, BasicBlock], graph: nx.DiGraph
) -> nx.DiGraph:
    """Merge duplicate tail blocks that postdominate their predecessors.

    For any node whose successors all postdominate it and each have an
    in-degree greater than one, edges are redirected to a single shared
    successor and the redundant nodes are removed.  Candidates are only merged
    when their instruction streams and successor sets are identical.  The
    original graph is not modified; a pruned copy is returned instead.
    """

    g = graph.copy()
    changed = True
    while changed:
        changed = False
        ipdom = compute_immediate_postdominators(g)
        for node in list(g.nodes):
            succs = list(g.successors(node))
            if len(succs) <= 1:
                continue
            cand = [
                s
                for s in succs
                if g.in_degree(s) > 1 and _postdominates(ipdom, node, s)
            ]
            if len(cand) > 1:
                keep = cand[0]
                keep_block = blocks.get(keep)
                keep_succs = set(g.successors(keep))
                if not keep_block:
                    continue
                keep_ins = [
                    (i.get("mnemonic"), i.get("op_str"), i.get("bytes"))
                    for i in keep_block.instructions
                ]
                identical = True
                for dup in cand[1:]:
                    dup_block = blocks.get(dup)
                    if not dup_block:
                        identical = False
                        break
                    dup_ins = [
                        (i.get("mnemonic"), i.get("op_str"), i.get("bytes"))
                        for i in dup_block.instructions
                    ]
                    if dup_ins != keep_ins:
                        identical = False
                        break
                    if set(g.successors(dup)) != keep_succs:
                        identical = False
                        break
                if not identical:
                    continue
                for dup in cand[1:]:
                    for pred in list(g.predecessors(dup)):
                        g.add_edge(pred, keep)
                    g.remove_node(dup)
                changed = True
                break
    return g


def traversal_order(graph: nx.DiGraph, entry: int) -> list[int]:
    """Return nodes visited from ``entry`` following CFG edges.

    A depth-first walk is performed starting at ``entry``.  Any nodes not
    reachable from the entry block are appended in ascending address order to
    ensure they are still processed deterministically.
    """

    order: list[int] = []
    if entry in graph:
        order.extend(nx.dfs_preorder_nodes(graph, entry))
    order.extend(n for n in sorted(graph.nodes) if n not in order)
    return order


def find_loops(
    blocks: Dict[int, "BasicBlock"],
    graph: nx.DiGraph,
) -> Dict[int, tuple[Set[int], Set[int]]]:
    """Return mapping of loop headers to their body and exit nodes."""

    if not blocks:
        return {}

    loops: Dict[int, tuple[Set[int], Set[int]]] = {}

    for component in nx.weakly_connected_components(graph):
        entry = min(component)
        sub = graph.subgraph(component)
        dominators = compute_dominators(sub, entry)
        for tail, head in sub.edges():
            if head not in dominators.get(tail, set()):
                continue
            loop_nodes: Set[int] = {head, tail}
            stack = [tail]
            while stack:
                node = stack.pop()
                for pred in sub.predecessors(node):
                    if pred in loop_nodes:
                        continue
                    if head in dominators.get(pred, set()):
                        loop_nodes.add(pred)
                        stack.append(pred)
            exits: Set[int] = set()
            for n in loop_nodes:
                for succ in sub.successors(n):
                    if succ not in loop_nodes:
                        exits.add(succ)
            if head in loops:
                prev_nodes, prev_exits = loops[head]
                loop_nodes |= prev_nodes
                exits |= prev_exits
            loops[head] = (loop_nodes, exits)
    return loops


def nearest_common_postdom(
    ipost: Dict[int, int], a: int, b: int
) -> int:
    """Return the nearest common post-dominator for *a* and *b*."""

    seen: Set[int] = set()
    cur = a
    visited: Set[int] = set()
    while True:
        if cur in visited:
            return a
        visited.add(cur)
        seen.add(cur)
        if cur not in ipost:
            break
        nxt = ipost[cur]
        if nxt == cur:
            break
        cur = nxt
    cur = b
    visited.clear()
    while cur not in seen:
        if cur in visited:
            return b
        visited.add(cur)
        if cur not in ipost:
            break
        nxt = ipost[cur]
        if nxt == cur:
            break
        cur = nxt
    return cur


def collect_branch(
    graph: nx.DiGraph,
    start: int,
    stop: int | None,
    other: Iterable[int] | int | None,
) -> Set[int]:
    """Return nodes reachable from ``start`` until ``stop`` or members of
    ``other``.

    ``other`` may be a single address or an iterable of addresses that should
    terminate the walk when encountered.
    """

    if other is None:
        others: Set[int] = set()
    elif isinstance(other, int):
        others = {other}
    else:
        others = set(other)

    nodes: Set[int] = set()
    stack = [start]
    while stack:
        node = stack.pop()
        if node == stop or node in others or node in nodes:
            continue
        nodes.add(node)
        for succ in graph.successors(node):
            stack.append(succ)
    return nodes


def reduce_region(
    blocks: Dict[int, "BasicBlock"],
    graph: nx.DiGraph,
    entry: int,
    exit: int,
) -> tuple[Dict[int, "BasicBlock"], Set[int]]:
    """Return subgraph blocks between *entry* and *exit*."""

    if entry == exit:
        return {}, set()

    region_nodes = collect_branch(graph, entry, exit, None)
    sub: Dict[int, "BasicBlock"] = {}
    for addr in region_nodes:
        orig = blocks[addr]
        new_block = BasicBlock(addr)
        new_block.instructions = orig.instructions[:]
        if (
            new_block.instructions
            and new_block.instructions[-1]["mnemonic"] == "jmp"
            and _parse_imm(
                new_block.instructions[-1].get("op_str", "")
            )
            == exit
        ):
            new_block.instructions = new_block.instructions[:-1]
        sub[addr] = new_block
    return sub, region_nodes
