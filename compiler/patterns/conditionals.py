from __future__ import annotations

from ..ast_nodes import (
    BreakNode,
    ContinueNode,
    IfElseNode,
    ReturnNode,
    BasicBlockNode,
    GotoNode,
)
from ..cfg import (
    build_cfg,
    collect_branch,
    nearest_common_postdom,
    reduce_region,
)
from . import PatternContext, PatternResult


def detect_if_else(start: int, ctx: PatternContext) -> PatternResult | None:
    """Return ``PatternResult`` if *start* begins an if/else region."""

    blocks = ctx.blocks
    graph = ctx.graph
    ipost = ctx.ipost

    # ``ctx.loop_exits`` may include edges that leave the function entirely
    # (e.g. via ``ret`` or an unconditional jump).  Such exits do not
    # correspond to ``break`` statements from the current loop and should not
    # influence break detection.  Filter them out up-front so later checks only
    # consider exits that actually rejoin execution after the loop.
    loop_breaks = set()
    for ex in ctx.loop_exits:
        blk = blocks.get(ex)
        if not blk or not blk.instructions:
            continue
        last = blk.instructions[-1]
        mnem = last.get("mnemonic")
        if mnem in {"ret", "retn", "retf"}:
            continue
        if mnem == "jmp":
            continue
        loop_breaks.add(ex)

    block = blocks.get(start)
    if not block or not block.instructions:
        return None

    last = block.instructions[-1]
    if not (last["mnemonic"].startswith("j") and last["mnemonic"] != "jmp"):
        return None

    target = ctx.renderer.parse_imm(last.get("op_str", ""))
    size = len(last["bytes"]) // 2
    fall = last["address"] + size
    if target is None:
        return None
    if fall not in blocks and fall not in loop_breaks:
        return None

    if fall not in blocks and fall in loop_breaks:
        prefix = block.instructions[:-1]
        exit_node = (
            ContinueNode() if fall in ctx.loop_map else ReturnNode()
        )
        node = IfElseNode(
            prefix,
            last["mnemonic"],
            last.get("cond_prev"),
            [exit_node],
            None,
            target,
        )
        setattr(node, "start", start)
        return PatternResult(node, {start})

    target_block = blocks.get(target)
    if (
        target_block
        and graph.in_degree(target) == 1
        and target_block.instructions
        and target_block.instructions[-1]["mnemonic"]
        in {"ret", "retn", "retf"}
    ):
        if start in collect_branch(graph, fall, None, None):
            return None
        prefix = block.instructions[:-1]
        body_instrs = target_block.instructions[:-1]
        body_nodes = []
        if body_instrs:
            body_nodes.append(BasicBlockNode(target, body_instrs))
        ret_mnem = target_block.instructions[-1]["mnemonic"]
        body_nodes.append(ReturnNode(target, ret_mnem))
        node = IfElseNode(
            prefix,
            last["mnemonic"],
            last.get("cond_prev"),
            body_nodes,
            None,
            target,
            negate=False,
        )
        setattr(node, "start", start)
        return PatternResult(node, {start, target})

    # Conditional branch acting as a break from an enclosing loop
    if target in loop_breaks:
        prefix = block.instructions[:-1]

        if len(loop_breaks) == 1 or target == min(loop_breaks):
            node = IfElseNode(
                prefix,
                last["mnemonic"],
                last.get("cond_prev"),
                [BreakNode()],
                None,
                target,
                negate=False,
            )
            setattr(node, "start", start)
            consumed = {start}
            if target in blocks:
                consumed.add(target)
            return PatternResult(node, consumed)

        goto_insn = {
            "address": last["address"],
            "mnemonic": "jmp",
            "op_str": f"0x{target:04X}",
            "bytes": "",
            "force_pc": True,
        }
        if target in blocks:
            ctx.renderer.func_names.setdefault(target, f"label_{target:04X}")
            ctx.known_funcs.discard(target)
        node = IfElseNode(
            prefix,
            last["mnemonic"],
            last.get("cond_prev"),
            [GotoNode(last["address"], goto_insn)],
            None,
            target,
            negate=False,
        )
        setattr(node, "start", start)
        consumed = {start}
        return PatternResult(node, consumed)

    if target in blocks:
        merge = nearest_common_postdom(ipost, fall, target)
        then_nodes = collect_branch(graph, fall, merge, target)
        else_nodes = collect_branch(graph, target, merge, fall)
    else:
        merge = target
        then_nodes = collect_branch(graph, fall, merge, None)
        else_nodes = set()

    consumed = {start} | then_nodes | else_nodes

    then_blocks, _ = reduce_region(blocks, graph, fall, merge)
    else_blocks, _ = reduce_region(blocks, graph, target, merge)

    # Guard against degenerate regions that would cause recursive structuring
    # without consuming any blocks.  This can happen when a branch flows back
    # to the starting block or when region reduction fails to shrink the CFG.
    if (
        start in then_blocks
        or start in else_blocks
        or set(then_blocks) == set(blocks)
        or set(else_blocks) == set(blocks)
    ):
        return None

    if merge == target:
        target_block = blocks.get(target)
        if (
            target_block
            and target_block.instructions
            and not target_block.instructions[0]["mnemonic"].startswith("j")
        ):
            then_blocks.pop(target, None)
        else:
            consumed.add(target)
            then_blocks.pop(target, None)

    then_ast = (
        ctx.renderer.structure(
            then_blocks,
            build_cfg(then_blocks),
            ctx.known_funcs,
            fall,
            loop_exits=loop_breaks,
        )
        if then_blocks
        else []
    )
    else_ast = (
        ctx.renderer.structure(
            else_blocks,
            build_cfg(else_blocks),
            ctx.known_funcs,
            target,
            loop_exits=loop_breaks,
        )
        if else_blocks
        else None
    )

    prefix = block.instructions[:-1]
    node = IfElseNode(
        prefix,
        last["mnemonic"],
        last.get("cond_prev"),
        then_ast,
        else_ast,
        target,
    )
    setattr(node, "start", start)
    return PatternResult(node, consumed)


__all__ = ["detect_if_else"]
