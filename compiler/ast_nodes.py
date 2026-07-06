from __future__ import annotations

from dataclasses import dataclass
from typing import List, Set, Optional, TYPE_CHECKING

if TYPE_CHECKING:  # pragma: no cover - for type checking only
    from .ir_to_c import CCodeRenderer


class ASTNode:
    """Base class for IR to C abstract syntax tree nodes."""

    def render(
        self, renderer: "CCodeRenderer", indent: str, known_funcs: Set[int]
    ) -> List[str]:
        raise NotImplementedError


@dataclass
class CommentNode(ASTNode):
    """AST node representing a source comment."""

    start: int

    def render(
        self, renderer: "CCodeRenderer", indent: str, known_funcs: Set[int]
    ) -> List[str]:
        return [f"{indent}{c}" for c in renderer.comment_lines(self.start)]


@dataclass
class BasicBlockNode(ASTNode):
    start: int
    instructions: List[dict]

    def render(
        self, renderer: "CCodeRenderer", indent: str, known_funcs: Set[int]
    ) -> List[str]:
        body: List[str] = []
        needs_comment = False
        for idx, insn in enumerate(self.instructions):
            if (
                insn["address"] in renderer.func_names
                or insn["address"] in renderer.extern_labels
            ):
                label = renderer.func_name(insn["address"])
            else:
                label = None
            if label and insn["address"] not in known_funcs:
                body.append(f"{label}:")
            for comment in renderer.comment_lines(insn["address"]):
                body.append(f"{indent}{comment}")
            if idx == 0:
                body.append(
                    f"{indent}ip = 0x{insn['address'] & 0xFFFF:04X};"
                )
            body.append(f"{indent}SAFEPOINT();")
            rendered = renderer.format_instruction(insn, known_funcs)
            if (
                not needs_comment
                and any(
                    line.lstrip().startswith("/*")
                    or line.lstrip().startswith("// TODO ASM:")
                    or line.lstrip().startswith("// ASM:")
                    for line in rendered
                )
            ):
                needs_comment = True
            for code in rendered:
                body.append(f"{indent}{code}")

        renderer.flush_pending_dos(body, indent)

        if needs_comment:
            return [f"{indent}// Basic block 0x{self.start:04X}"] + body
        return body


@dataclass
class ForLoopNode(ASTNode):
    """AST node representing a C-style ``for`` loop."""

    init_insts: List[dict]
    cond_mnem: str
    step_inst: dict | None
    body: List[ASTNode]
    cond_prev: dict | None = None

    def render(
        self, renderer: "CCodeRenderer", indent: str, known_funcs: Set[int]
    ) -> List[str]:
        init_parts: List[str] = []
        for inst in self.init_insts:
            for code in renderer.format_instruction(inst, known_funcs):
                init_parts.append(code.rstrip(";"))
        init_code = ", ".join(init_parts)

        cond_expr = renderer.jcc_condition(self.cond_mnem, self.cond_prev)
        cond_expr = renderer.negate_condition(cond_expr)

        step_code = ""
        if self.step_inst is not None:
            mnem = self.step_inst.get("mnemonic")
            op = self.step_inst.get("op_str", "")
            if mnem in {"inc", "dec"} and op in {
                "ax",
                "bx",
                "cx",
                "dx",
                "si",
                "di",
                "bp",
                "sp",
            }:
                step_code = f"{op}{'++' if mnem == 'inc' else '--'}"
            else:
                step_raw = renderer.format_instruction(
                    self.step_inst, known_funcs
                )
                step_code = step_raw[0].rstrip(";") if step_raw else ""

        lines = [f"{indent}for ({init_code}; {cond_expr}; {step_code}) {{"]
        for node in self.body:
            lines.extend(node.render(renderer, indent + "    ", known_funcs))
        lines.append(f"{indent}    SAFEPOINT();")
        lines.append(f"{indent}}}")
        return lines


@dataclass
class LoopNode(ASTNode):
    """AST node representing a generic ``while`` style loop."""

    header: int
    cond_mnem: str
    body: List[ASTNode]
    exit_nodes: Set[int]
    prev: dict | None = None
    negate: bool = False
    cond_target: int | None = None
    cond_target_insts: List[dict] | None = None

    def render(
        self, renderer: "CCodeRenderer", indent: str, known_funcs: Set[int]
    ) -> List[str]:
        cond = renderer.jcc_condition(self.cond_mnem, self.prev)
        if "unsupported jcc" in cond:
            if (
                self.cond_mnem in {"jmp", "ljmp"}
                or not self.cond_mnem.startswith("j")
            ):
                cond = "1"
            else:
                cond = "1 /* unsupported jcc */"
        elif self.negate:
            cond = renderer.negate_condition(cond)
        lines = [f"{indent}while ({cond}) {{"]
        for node in self.body:
            lines.extend(node.render(renderer, indent + "    ", known_funcs))
        lines.append(f"{indent}    SAFEPOINT();")
        lines.append(f"{indent}}}")
        if self.cond_target_insts is not None:
            exit_cond = renderer.negate_condition(cond)
            lines.append(f"{indent}if ({exit_cond}) {{")
            for insn in self.cond_target_insts:
                for code in renderer.format_instruction(insn, known_funcs):
                    lines.append(f"{indent}    {code}")
            lines.append(f"{indent}}}")
        return lines


@dataclass
class DoWhileNode(ASTNode):
    """AST node representing a ``do { ... } while (...)`` loop."""

    header: int
    cond_mnem: str
    body: List[ASTNode]
    exit_nodes: Set[int]
    prev: dict | None = None
    negate: bool = False

    def render(
        self, renderer: "CCodeRenderer", indent: str, known_funcs: Set[int]
    ) -> List[str]:
        cond = renderer.jcc_condition(self.cond_mnem, self.prev)
        if "unsupported jcc" in cond:
            if (
                self.cond_mnem in {"jmp", "ljmp"}
                or not self.cond_mnem.startswith("j")
            ):
                cond = "1"
            else:
                cond = "1 /* unsupported jcc */"
        elif self.negate:
            cond = renderer.negate_condition(cond)
        lines = [f"{indent}do {{"]
        for node in self.body:
            lines.extend(node.render(renderer, indent + "    ", known_funcs))
        lines.append(f"{indent}    SAFEPOINT();")
        lines.append(f"{indent}}} while ({cond});")
        return lines


@dataclass
class IfElseNode(ASTNode):
    prefix_insts: List[dict]
    mnem: str
    prev: Optional[dict]
    then_body: List[ASTNode]
    else_body: Optional[List[ASTNode]] = None
    target: int | None = None
    negate: bool = True

    def render(
        self, renderer: "CCodeRenderer", indent: str, known_funcs: Set[int]
    ) -> List[str]:
        lines: List[str] = []
        for insn in self.prefix_insts:
            for code in renderer.format_instruction(insn, known_funcs):
                lines.append(f"{indent}{code}")
        cond = renderer.jcc_condition(self.mnem, self.prev)
        if self.negate:
            cond = renderer.negate_condition(cond)
        lines.append(f"{indent}if ({cond}) {{")
        branch_indent = indent + "    "
        for node in self.then_body:
            lines.extend(node.render(renderer, branch_indent, known_funcs))
        renderer.flush_pending_dos(lines, branch_indent)
        lines.append(f"{indent}}}")
        if self.else_body is not None:
            def _non_fallthrough(nodes: List[ASTNode]) -> bool:
                if len(nodes) != 1:
                    return False
                node = nodes[0]
                if isinstance(
                    node,
                    (BreakNode, ContinueNode, ReturnNode, GotoNode),
                ):
                    return True
                return False

            if _non_fallthrough(self.then_body):
                for node in self.else_body:
                    lines.extend(node.render(renderer, indent, known_funcs))
                renderer.flush_pending_dos(lines, indent)
            else:
                lines[-1] = f"{indent}}} else {{"
                for node in self.else_body:
                    lines.extend(
                        node.render(renderer, branch_indent, known_funcs)
                    )
                renderer.flush_pending_dos(lines, branch_indent)
                lines.append(f"{indent}}}")
        return lines


@dataclass
class BreakNode(ASTNode):
    """AST node representing a ``break`` statement."""
    def render(
        self, renderer: "CCodeRenderer", indent: str, known_funcs: Set[int]
    ) -> List[str]:
        return [f"{indent}break;"]


@dataclass
class ContinueNode(ASTNode):
    """AST node representing a ``continue`` statement."""
    def render(
        self, renderer: "CCodeRenderer", indent: str, known_funcs: Set[int]
    ) -> List[str]:
        return [f"{indent}continue;"]


@dataclass
class ReturnNode(ASTNode):
    """AST node representing a ``return`` or ``retf`` statement."""

    start: int | None = None
    mnemonic: str = "ret"
    pop_bytes: int | None = None

    def render(
        self, renderer: "CCodeRenderer", indent: str, known_funcs: Set[int]
    ) -> List[str]:
        lines: List[str] = []
        if self.mnemonic == "retf":
            if self.pop_bytes is not None:
                lines.append(f"{indent}retf_pop({self.pop_bytes});")
            else:
                lines.append(f"{indent}retf();")
            return lines
        if self.pop_bytes is not None:
            lines.append(f"{indent}sp = (sp + {self.pop_bytes}) & 0xFFFF;")
        lines.append(f"{indent}return;")
        return lines


@dataclass
class GotoNode(ASTNode):
    """AST node representing an unconditional ``goto``."""

    start: int
    insn: dict

    def render(
        self, renderer: "CCodeRenderer", indent: str, known_funcs: Set[int]
    ) -> List[str]:
        lines: List[str] = []
        label = renderer.func_names.get(self.start)
        if label is None and self.start in renderer.extern_labels:
            label = renderer.func_name(self.start)
        if label and self.start not in known_funcs:
            lines.append(f"{label}:")
        for code in renderer.format_instruction(self.insn, known_funcs):
            lines.append(f"{indent}{code}")
        return lines


@dataclass
class CallNode(ASTNode):
    """AST node representing a function ``call``."""

    start: int
    insn: dict

    def render(
        self, renderer: "CCodeRenderer", indent: str, known_funcs: Set[int]
    ) -> List[str]:
        lines: List[str] = []
        label = renderer.func_names.get(self.start)
        if label is None and self.start in renderer.extern_labels:
            label = renderer.func_name(self.start)
        if label and self.start not in known_funcs:
            lines.append(f"{label}:")
        for code in renderer.format_instruction(self.insn, known_funcs):
            lines.append(f"{indent}{code}")
        return lines


@dataclass
class SwitchNode(ASTNode):
    """AST node representing a ``switch`` statement."""

    expr: str
    cases: List[tuple[int, int | List[ASTNode]]]

    def render(
        self, renderer: "CCodeRenderer", indent: str, known_funcs: Set[int]
    ) -> List[str]:
        lines = [f"{indent}switch ({self.expr}) {{"]
        for val, body in self.cases:
            if isinstance(body, int):
                if body in known_funcs:
                    target = f"func_{body:04X}();"
                else:
                    target = f"/* 0x{body:04X} */"
                lines.append(f"{indent}    case {val}: {target} break;")
            else:
                lines.append(f"{indent}    case {val}:")
                for node in body:
                    lines.extend(
                        node.render(renderer, indent + "        ", known_funcs)
                    )
                lines.append(f"{indent}        break;")
        lines.append(f"{indent}}}")
        return lines
