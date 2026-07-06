from __future__ import annotations

import re

from ..ast_nodes import ASTNode, SwitchNode
from ..cfg import _parse_imm, build_cfg, nearest_common_postdom, reduce_region
from . import PatternContext, PatternResult


def detect_switch(start: int, ctx: PatternContext) -> PatternResult | None:
    """Return ``PatternResult`` if *start* contains a jump table pattern."""

    blocks = ctx.blocks
    if start not in blocks:
        return None
    block = blocks[start]
    if len(block.instructions) < 4:
        return None
    mov1, mov2, scale, jmp = block.instructions[-4:]
    if jmp["mnemonic"] != "jmp":
        return None
    op_str = jmp.get("op_str", "")
    if "[" not in op_str:
        return None
    m = re.search(
        r"\[bx\s*\+\s*(0x[0-9a-fA-F]+|[0-9a-fA-F]+h|\d+)\]",
        op_str,
        re.IGNORECASE,
    )
    if not m:
        return None
    table_off = _parse_imm(m.group(1))
    if table_off is None:
        return None
    if not (
        mov1["mnemonic"] == "mov"
        and mov1.get("op_str", "").lower().startswith("bl,")
    ):
        return None
    mov2_op = mov2.get("op_str", "")
    bh_zeroed = (
        mov2["mnemonic"] == "mov"
        and re.match(r"bh,\s*0", mov2_op, re.IGNORECASE)
    ) or (
        mov2["mnemonic"] == "xor"
        and re.match(r"bh\s*,\s*bh", mov2_op, re.IGNORECASE)
    )
    if not bh_zeroed:
        return None
    if not (
        scale["mnemonic"] in {"add", "shl"}
        and scale.get("op_str", "").lower() in {"bx, bx", "bx, 1"}
    ):
        return None
    expr = mov1.get("op_str", "").split(",", 1)[1].strip()
    from ..ir_to_c import _decode_variables  # avoid circular import

    from ..ir_to_c import _rewrite_mem_op  # avoid circular import

    expr = _rewrite_mem_op(_decode_variables(expr))

    cases: list[tuple[int, int]] = []
    index = 0
    code_bytes = ctx.renderer.code_bytes
    while True:
        off = table_off + index * 2
        if off + 2 > len(code_bytes):
            break
        addr = int.from_bytes(code_bytes[off: off + 2], "little")
        if (
            addr not in ctx.known_funcs
            and addr not in ctx.blocks
            and addr not in ctx.renderer.current_func_all_addrs
        ):
            break
        cases.append((index, addr))
        index += 1
        if index > 256:
            break
    if not cases:
        return None

    internal = [addr for _, addr in cases if addr in ctx.blocks]
    if internal:
        merge = internal[0]
        for addr in internal[1:]:
            merge = nearest_common_postdom(ctx.ipost, merge, addr)
        if merge in ctx.blocks:
            consumed = {start}
            new_cases: list[tuple[int, int | list[ASTNode]]] = []
            for val, addr in cases:
                if addr in ctx.blocks:
                    sub_blocks, region_nodes = reduce_region(
                        ctx.blocks, ctx.graph, addr, merge
                    )
                    sub_graph = build_cfg(sub_blocks)
                    entry_block = min(sub_blocks) if sub_blocks else addr
                    body = ctx.renderer.structure(
                        sub_blocks,
                        sub_graph,
                        ctx.known_funcs,
                        entry_block,
                        loop_exits=ctx.loop_exits,
                    )
                    new_cases.append((val, body))
                    consumed |= region_nodes
                else:
                    new_cases.append((val, addr))
            node = SwitchNode(expr, new_cases)
            return PatternResult(node, consumed)

    node = SwitchNode(expr, cases)
    return PatternResult(node, {start})


__all__ = ["detect_switch"]
