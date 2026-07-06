from __future__ import annotations

from typing import Dict, Iterable, Set, List

from ..ast_nodes import ASTNode, DoWhileNode, LoopNode, ForLoopNode
from ..basic_block import BasicBlock
from ..cfg import build_cfg
from . import PatternContext, PatternResult


def _collect_starts(nodes: Iterable[ASTNode]) -> Set[int]:
    """Return set of ``start`` addresses referenced within *nodes*."""
    starts: Set[int] = set()
    for node in nodes:
        start = getattr(node, "start", None)
        if start is not None:
            starts.add(start)
        for attr in ("body", "then_body", "else_body"):
            child = getattr(node, attr, None)
            if isinstance(child, list):
                starts.update(_collect_starts(child))
    return starts


def detect_for_loop(start: int, ctx: PatternContext) -> PatternResult | None:
    """Return ``PatternResult`` if *start* begins a ``for`` loop."""

    if start not in ctx.blocks or not ctx.blocks[start].instructions:
        return None

    init_block = ctx.blocks[start]
    last = init_block.instructions[-1]

    # ------------------------------------------------------------------
    # Pattern: initialization followed by JCXZ and loop back-edge
    # ------------------------------------------------------------------
    if last.get("mnemonic") == "jcxz":
        exit_target = ctx.renderer.parse_imm(last.get("op_str", ""))
        succs = set(ctx.graph.successors(start))
        if exit_target in succs:
            succs.remove(exit_target)
        if len(succs) != 1:
            return None
        header = succs.pop()
        if header not in ctx.loop_map:
            return None

        body_nodes, exits = ctx.loop_map[header]
        if start in body_nodes:
            return None

        # Locate latch block containing the LOOP instruction
        latch_blocks = [
            addr
            for addr in body_nodes
            if ctx.blocks[addr].instructions
            and ctx.blocks[addr].instructions[-1].get("mnemonic") == "loop"
            and ctx.renderer.parse_imm(
                ctx.blocks[addr].instructions[-1].get("op_str", "")
            )
            == header
        ]
        if len(latch_blocks) != 1:
            return None
        latch_addr = latch_blocks[0]

        sub_blocks: Dict[int, BasicBlock] = {}
        for addr in body_nodes:
            orig = ctx.blocks[addr]
            new_block = BasicBlock(addr)
            new_block.instructions = orig.instructions[:]
            if addr == latch_addr:
                new_block.instructions = new_block.instructions[:-1]
            elif (
                new_block.instructions
                and new_block.instructions[-1].get("mnemonic") == "jmp"
                and ctx.renderer.parse_imm(
                    new_block.instructions[-1].get("op_str", "")
                )
                == header
            ):
                new_block.instructions = new_block.instructions[:-1]
            if new_block.instructions:
                sub_blocks[addr] = new_block

        sub_graph = build_cfg(sub_blocks)
        if header in sub_graph:
            for pred in list(sub_graph.predecessors(header)):
                sub_graph.remove_edge(pred, header)
        # Always begin structuring from the loop header.
        #
        # Using ``min(sub_blocks)`` here could select one of the loop's exit
        # blocks if its address happened to be lower than the header. In that
        # scenario the recursive ``structure`` call would never visit the
        # actual header block, leading to re-detection of the same loop and
        # eventually a recursion error.
        entry_block = header
        body_ast = ctx.renderer.structure(
            sub_blocks,
            sub_graph,
            ctx.known_funcs,
            entry_block,
            loop_exits=ctx.loop_exits | exits,
        )
        body_ast = [
            n for n in body_ast if getattr(n, "start", None) not in exits
        ]
        exit_blocks = exits & set(sub_blocks)
        referenced = _collect_starts(body_ast)

        step_inst = {"mnemonic": "dec", "op_str": "cx"}
        node = ForLoopNode(
            init_block.instructions[:-1],
            last["mnemonic"],
            step_inst,
            body_ast,
            last.get("cond_prev"),
        )
        # Include only exit blocks referenced inside the loop so they aren't
        # emitted twice after the structured ``for``.
        consumed = set(body_nodes) | {start} | (exit_blocks & referenced)
        return PatternResult(node, consumed)

    # ------------------------------------------------------------------
    # Pattern: classic init; jmp header; cond; step; jmp
    # ------------------------------------------------------------------
    if last.get("mnemonic") != "jmp":
        return None

    target = ctx.renderer.parse_imm(last.get("op_str", ""))
    if target not in ctx.loop_map:
        return None

    body_nodes, exits = ctx.loop_map[target]
    if start in body_nodes:
        return None

    body_only = body_nodes - {target}
    if not body_only:
        return None

    # Identify the block responsible for stepping and jumping back to header
    latch_blocks = [
        addr
        for addr in body_only
        if ctx.blocks[addr].instructions
        and ctx.blocks[addr].instructions[-1].get("mnemonic") == "jmp"
        and ctx.renderer.parse_imm(
            ctx.blocks[addr].instructions[-1].get("op_str", "")
        )
        == target
    ]
    if len(latch_blocks) != 1:
        return None
    latch_addr = latch_blocks[0]
    latch_block = ctx.blocks[latch_addr]
    if len(latch_block.instructions) < 2:
        return None

    step_inst = latch_block.instructions[-2]
    # If the step instruction operates on memory (e.g. ``word ptr es:[di+4]``)
    # rather than a register variable then the loop resembles a ``while``
    # construct more than a canonical ``for`` loop.  Treating such cases as
    # ``for`` loops places raw memory expressions like ``word ptr es:[di+4]``
    # in the step field of the generated C which is both awkward and often
    # incorrect.  By rejecting them here the generic loop detector will handle
    # the region instead, keeping the memory operation within the loop body.
    if "[" in step_inst.get("op_str", ""):
        return None

    # Build subgraph excluding header and init; strip back-edge and step
    sub_blocks: Dict[int, BasicBlock] = {}
    for addr in body_only:
        orig = ctx.blocks[addr]
        new_block = BasicBlock(addr)
        new_block.instructions = orig.instructions[:]
        if addr == latch_addr:
            new_block.instructions = new_block.instructions[:-2]
        elif (
            new_block.instructions
            and new_block.instructions[-1].get("mnemonic") == "jmp"
            and ctx.renderer.parse_imm(
                new_block.instructions[-1].get("op_str", "")
            )
            == target
        ):
            new_block.instructions = new_block.instructions[:-1]
        if new_block.instructions:
            sub_blocks[addr] = new_block

    sub_graph = build_cfg(sub_blocks)
    if target in sub_graph:
        for pred in list(sub_graph.predecessors(target)):
            sub_graph.remove_edge(pred, target)
    # Always begin structuring from the loop header.
    #
    # Using ``min(sub_blocks)`` here could select one of the loop's exit
    # blocks if its address happened to be lower than the header. In that
    # scenario the recursive ``structure`` call would never visit the
    # actual header block, leading to re-detection of the same loop and
    # eventually a recursion error.
    entry_block = target
    body_ast = ctx.renderer.structure(
        sub_blocks,
        sub_graph,
        ctx.known_funcs,
        entry_block,
        loop_exits=ctx.loop_exits | exits,
    )
    body_ast = [n for n in body_ast if getattr(n, "start", None) not in exits]
    exit_blocks = exits & set(sub_blocks)
    referenced = _collect_starts(body_ast)

    header_block = ctx.blocks[target]
    cond_inst = header_block.instructions[-1]

    node = ForLoopNode(
        init_block.instructions[:-1],
        cond_inst["mnemonic"],
        step_inst,
        body_ast,
        cond_inst.get("cond_prev"),
    )
    # Consume loop body and referenced terminal exit blocks to avoid
    # emitting them again after the loop.
    consumed = set(body_nodes) | {start} | (exit_blocks & referenced)
    return PatternResult(node, consumed)


def detect_loop(start: int, ctx: PatternContext) -> PatternResult | None:
    """Return ``PatternResult`` if *start* begins a loop."""

    if start not in ctx.loop_map:
        return None

    body_nodes, exits = ctx.loop_map[start]
    blocks = ctx.blocks

    sub_blocks: Dict[int, BasicBlock] = {}

    # Locate latch block providing the back-edge to the loop header
    latch_addr = None
    latch_inst = None
    for addr in body_nodes:
        if addr == start:
            continue
        insts = blocks[addr].instructions if addr in blocks else []
        if insts:
            last_inst = insts[-1]
            if ctx.renderer.parse_imm(last_inst.get("op_str", "")) == start:
                latch_addr = addr
                latch_inst = last_inst
                break

    header_block = blocks[start]
    header_last = header_block.instructions[-1]
    target = ctx.renderer.parse_imm(header_last.get("op_str", ""))
    latch_mnem = latch_inst.get("mnemonic") if latch_inst else None
    complement = {
        "ja": "jbe",
        "jbe": "ja",
        "jae": "jb",
        "jb": "jae",
        "jg": "jle",
        "jle": "jg",
        "jge": "jl",
        "jl": "jge",
        "je": "jne",
        "jne": "je",
        "jz": "jnz",
        "jnz": "jz",
        "jc": "jnc",
        "jnc": "jc",
        "js": "jns",
        "jns": "js",
    }
    treat_as_while = (
        latch_mnem is not None
        and complement.get(header_last["mnemonic"]) == latch_mnem
    )

    header_cond_into_body = (
        latch_inst
        and latch_inst.get("mnemonic") == "jmp"
        and header_last.get("mnemonic", "").startswith("j")
        and header_last.get("mnemonic") != "jmp"
        and target in body_nodes
    )

    if header_cond_into_body:
        header_copy = BasicBlock(start)
        header_copy.instructions = header_block.instructions[:]
        sub_blocks[start] = header_copy
    elif (
        latch_inst
        and latch_inst.get("mnemonic") != "jmp"
        and not treat_as_while
    ):
        header_copy = BasicBlock(start)
        header_copy.instructions = header_block.instructions[:]
        sub_blocks[start] = header_copy
    else:
        prefix = header_block.instructions[:-1]
        if (
            prefix
            and header_last.get("mnemonic", "").startswith("j")
            and header_last.get("mnemonic") != "jmp"
            and target not in body_nodes
            and latch_inst
            and latch_inst.get("mnemonic") == "jmp"
            and header_last.get("cond_prev") is None
        ):
            header_copy = BasicBlock(start)
            header_copy.instructions = header_block.instructions[:]
            sub_blocks[start] = header_copy
        elif prefix:
            header_copy = BasicBlock(start)
            header_copy.instructions = prefix
            sub_blocks[start] = header_copy
        elif (
            header_block.instructions
            and header_block.instructions[0]["mnemonic"]
            not in {"jmp", "ret", "retn", "retf"}
        ):
            header_copy = BasicBlock(start)
            header_copy.instructions = header_block.instructions[:]
            sub_blocks[start] = header_copy

    for addr in body_nodes:
        if addr == start:
            continue
        orig = blocks[addr]
        new_block = BasicBlock(addr)
        new_block.instructions = orig.instructions[:]
        if (
            new_block.instructions
            and ctx.renderer.parse_imm(
                new_block.instructions[-1].get("op_str", "")
            )
            == start
        ):
            if latch_addr is not None and addr == latch_addr:
                new_block.instructions = new_block.instructions[:-1]
            elif new_block.instructions[-1]["mnemonic"] in {"jmp", "loop"}:
                new_block.instructions = new_block.instructions[:-1]
        if new_block.instructions:
            sub_blocks[addr] = new_block

    for exit_addr in exits:
        if exit_addr in blocks:
            exit_block = blocks[exit_addr]
            insts = exit_block.instructions
            if insts:
                new_exit = BasicBlock(exit_addr)
                new_exit.instructions = insts[:]
                sub_blocks[exit_addr] = new_exit

    sub_graph = build_cfg(sub_blocks)
    if start in sub_graph:
        for pred in list(sub_graph.predecessors(start)):
            sub_graph.remove_edge(pred, start)
    # Always begin structuring from the loop header.
    #
    # Using ``min(sub_blocks)`` here could select one of the loop's exit
    # blocks if its address happened to be lower than the header.  In that
    # scenario the recursive ``structure`` call would never visit the actual
    # header block, leading to re-detection of the same loop and eventually a
    # recursion error.  Starting from ``start`` ensures the loop body is
    # analysed relative to its header and prevents re-entering ``detect_loop``
    # on the same region.
    entry_block = start
    body_ast = ctx.renderer.structure(
        sub_blocks,
        sub_graph,
        ctx.known_funcs,
        entry_block,
        loop_exits=ctx.loop_exits | exits,
    )
    body_ast = [
        n for n in body_ast if getattr(n, "start", None) not in exits
    ]
    exit_blocks = exits & set(sub_blocks)

    if header_cond_into_body:
        node = DoWhileNode(start, "jmp", body_ast, exits)
        early_ret_target = None
        early_ret_insts: List[dict] | None = None
    elif latch_inst and latch_inst.get("mnemonic") != "jmp":
        early_ret_target = None
        early_ret_insts = None
        if treat_as_while:
            node = LoopNode(
                start,
                latch_inst["mnemonic"],
                body_ast,
                exits,
                latch_inst.get("cond_prev"),
            )
        else:
            node = DoWhileNode(
                start,
                latch_inst["mnemonic"],
                body_ast,
                exits,
                latch_inst.get("cond_prev"),
            )
    else:
        last = header_last
        negate = target not in body_nodes
        early_ret_target = None
        early_ret_insts = None
        if target == start and last["mnemonic"] != "jmp":
            node = DoWhileNode(
                start,
                last["mnemonic"],
                body_ast,
                exits,
                last.get("cond_prev"),
            )
        else:
            prefix = header_block.instructions[:-1]
            trivial = all(
                inst.get("mnemonic") in {"cmp", "test", "or", "and"}
                for inst in prefix
            )
            special_break = (
                prefix
                and not trivial
                and header_last.get("mnemonic", "").startswith("j")
                and header_last.get("mnemonic") != "jmp"
                and target not in body_nodes
                and latch_inst
                and latch_inst.get("mnemonic") == "jmp"
                and header_last.get("cond_prev") is None
            )
            if special_break:
                node = LoopNode(start, "jmp", body_ast, exits)
            elif prefix and not trivial:
                cond_inst = last
                for inst in reversed(prefix):
                    if inst.get("mnemonic", "").startswith("j"):
                        cond_inst = inst
                        break
                node = DoWhileNode(
                    start,
                    cond_inst["mnemonic"],
                    body_ast,
                    exits,
                    cond_inst.get("cond_prev"),
                    negate=negate if cond_inst is last else False,
                )
            else:
                node = LoopNode(
                    start,
                    last["mnemonic"],
                    body_ast,
                    exits,
                    last.get("cond_prev"),
                    negate=negate,
                )
                if (
                    target not in body_nodes
                    and target in blocks
                    and blocks[target].instructions
                    and blocks[target].instructions[-1]["mnemonic"]
                    in {"ret", "retn", "retf"}
                ):
                    early_ret_target = target
                    early_ret_insts = blocks[target].instructions[:]

    referenced = _collect_starts(body_ast)
    # Consume loop body and any exit blocks referenced inside it so they aren't
    # duplicated after the loop.
    consumed = body_nodes | (exit_blocks & referenced)
    if early_ret_target is not None:
        consumed.add(early_ret_target)
        if isinstance(node, LoopNode):
            node.cond_target = early_ret_target
            node.cond_target_insts = early_ret_insts
    return PatternResult(node, consumed)


__all__ = ["detect_for_loop", "detect_loop"]
