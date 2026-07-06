from __future__ import annotations

from dataclasses import dataclass
from typing import Dict, Set, TYPE_CHECKING

import networkx as nx

if TYPE_CHECKING:  # pragma: no cover - type checking only
    from ..basic_block import BasicBlock
    from ..ir_to_c import CCodeRenderer
    from ..ast_nodes import ASTNode


@dataclass
class PatternContext:
    """Context provided to pattern matching functions."""

    blocks: Dict[int, "BasicBlock"]
    graph: nx.DiGraph
    known_funcs: Set[int]
    renderer: "CCodeRenderer"
    loop_map: Dict[int, tuple[Set[int], Set[int]]]
    ipost: Dict[int, int]
    loop_exits: Set[int]


@dataclass
class PatternResult:
    """Result of a pattern match operation."""

    node: "ASTNode"
    consumed_blocks: Set[int]


from .switch import detect_switch  # noqa: E402
from .loops import detect_for_loop, detect_loop  # noqa: E402
from .conditionals import detect_if_else  # noqa: E402
__all__ = [
    "PatternContext",
    "PatternResult",
    "detect_switch",
    "detect_for_loop",
    "detect_loop",
    "detect_if_else",
]
