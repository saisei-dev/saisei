from __future__ import annotations

from typing import List


class BasicBlock:
    """Simple representation of a basic block."""

    def __init__(self, start: int) -> None:
        self.start = start
        self.instructions: List[dict] = []
