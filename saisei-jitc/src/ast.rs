//! the structured-renderer AST + render methods.
//! Used only by the base (readable-C) CCodeRenderer.render_function path.

use crate::ir_to_c::{jcc_condition, negate_condition, Insn, Renderer};
use indexmap::IndexSet;
use serde_json::Value;
use std::collections::BTreeSet;

fn ms<'a>(i: &'a Insn, k: &str) -> &'a str {
    i.get(k).and_then(Value::as_str).unwrap_or("")
}
fn maddr(i: &Insn) -> i64 {
    i.get("address").and_then(Value::as_i64).unwrap_or(0)
}

pub enum CaseBody {
    Int(i64),
    Nodes(Vec<AstNode>),
}

pub enum AstNode {
    Comment {
        start: i64,
    },
    BasicBlock {
        start: i64,
        instructions: Vec<Insn>,
    },
    ForLoop {
        init_insts: Vec<Insn>,
        cond_mnem: String,
        step_inst: Option<Insn>,
        body: Vec<AstNode>,
        cond_prev: Option<Insn>,
    },
    Loop {
        header: i64,
        cond_mnem: String,
        body: Vec<AstNode>,
        exit_nodes: IndexSet<i64>,
        prev: Option<Insn>,
        negate: bool,
        cond_target: Option<i64>,
        cond_target_insts: Option<Vec<Insn>>,
    },
    DoWhile {
        header: i64,
        cond_mnem: String,
        body: Vec<AstNode>,
        exit_nodes: IndexSet<i64>,
        prev: Option<Insn>,
        negate: bool,
    },
    IfElse {
        prefix_insts: Vec<Insn>,
        mnem: String,
        prev: Option<Insn>,
        then_body: Vec<AstNode>,
        else_body: Option<Vec<AstNode>>,
        target: Option<i64>,
        negate: bool,
        start: i64,
    },
    Break,
    Continue,
    Return {
        start: Option<i64>,
        mnemonic: String,
        pop_bytes: Option<i64>,
    },
    Goto {
        start: i64,
        insn: Insn,
    },
    Call {
        start: i64,
        insn: Insn,
    },
    Switch {
        expr: String,
        cases: Vec<(i64, CaseBody)>,
    },
}

impl AstNode {
    /// getattr(node, "start", None) equivalent (for the source body filtering).
    /// CommentNode/BasicBlockNode/ReturnNode/GotoNode/CallNode carry a `.start`
    /// field; IfElseNode gets one via `setattr(node, "start", start)` in
    /// detect_if_else. ForLoop/Loop/DoWhile/Switch do NOT
    /// (they use `.header`/none), so getattr returns None for those.
    pub fn start(&self) -> Option<i64> {
        match self {
            AstNode::Comment { start } => Some(*start),
            AstNode::BasicBlock { start, .. } => Some(*start),
            AstNode::IfElse { start, .. } => Some(*start),
            AstNode::Goto { start, .. } => Some(*start),
            AstNode::Call { start, .. } => Some(*start),
            AstNode::Return { start, .. } => *start,
            _ => None,
        }
    }

    /// Whether this node is a non-fallthrough terminator (IfElseNode else-merge).
    fn non_fallthrough(nodes: &[AstNode]) -> bool {
        nodes.len() == 1
            && matches!(
                nodes[0],
                AstNode::Break | AstNode::Continue | AstNode::Return { .. } | AstNode::Goto { .. }
            )
    }

    pub fn render(&self, r: &mut Renderer, indent: &str, known: &BTreeSet<i64>) -> Vec<String> {
        match self {
            AstNode::Comment { start } => r
                .comment_lines(*start)
                .into_iter()
                .map(|c| format!("{indent}{c}"))
                .collect(),
            AstNode::BasicBlock {
                start,
                instructions,
            } => {
                let mut body: Vec<String> = Vec::new();
                let mut needs_comment = false;
                for (idx, insn) in instructions.iter().enumerate() {
                    let a = maddr(insn);
                    let label = if r.func_names.contains_key(&a) || r.extern_labels.contains(&a) {
                        Some(r.func_name(a))
                    } else {
                        None
                    };
                    if let Some(l) = &label {
                        if !known.contains(&a) {
                            body.push(format!("{l}:"));
                        }
                    }
                    for c in r.comment_lines(a) {
                        body.push(format!("{indent}{c}"));
                    }
                    if idx == 0 {
                        body.push(format!("{indent}ip = 0x{:04X};", a & 0xFFFF));
                    }
                    body.push(format!("{indent}SAFEPOINT();"));
                    let rendered = r.format_instruction(insn, known);
                    if !needs_comment
                        && rendered.iter().any(|line| {
                            let t = line.trim_start();
                            t.starts_with("/*")
                                || t.starts_with("// TODO ASM:")
                                || t.starts_with("// ASM:")
                        })
                    {
                        needs_comment = true;
                    }
                    for code in rendered {
                        body.push(format!("{indent}{code}"));
                    }
                }
                r.flush_pending_dos(&mut body, indent);
                if needs_comment {
                    let mut out = vec![format!("{indent}// Basic block 0x{start:04X}")];
                    out.extend(body);
                    out
                } else {
                    body
                }
            }
            AstNode::ForLoop {
                init_insts,
                cond_mnem,
                step_inst,
                body,
                cond_prev,
            } => {
                let mut init_parts: Vec<String> = Vec::new();
                for inst in init_insts {
                    for code in r.format_instruction(inst, known) {
                        init_parts.push(code.trim_end_matches(';').to_string());
                    }
                }
                let init_code = init_parts.join(", ");
                let cond = negate_condition(&jcc_condition(cond_mnem, cond_prev.as_ref(), None));
                let mut step_code = String::new();
                if let Some(si) = step_inst {
                    let mnem = ms(si, "mnemonic");
                    let op = ms(si, "op_str");
                    const REGS: &[&str] = &["ax", "bx", "cx", "dx", "si", "di", "bp", "sp"];
                    if (mnem == "inc" || mnem == "dec") && REGS.contains(&op) {
                        step_code = format!("{op}{}", if mnem == "inc" { "++" } else { "--" });
                    } else {
                        let raw = r.format_instruction(si, known);
                        step_code = raw
                            .first()
                            .map(|c| c.trim_end_matches(';').to_string())
                            .unwrap_or_default();
                    }
                }
                let mut lines = vec![format!("{indent}for ({init_code}; {cond}; {step_code}) {{")];
                let inner = format!("{indent}    ");
                for n in body {
                    lines.extend(n.render(r, &inner, known));
                }
                lines.push(format!("{indent}    SAFEPOINT();"));
                lines.push(format!("{indent}}}"));
                lines
            }
            AstNode::Loop {
                cond_mnem,
                body,
                prev,
                negate,
                cond_target_insts,
                ..
            } => {
                let mut cond = jcc_condition(cond_mnem, prev.as_ref(), None);
                if cond.contains("unsupported jcc") {
                    if cond_mnem == "jmp" || cond_mnem == "ljmp" || !cond_mnem.starts_with('j') {
                        cond = "1".into();
                    } else {
                        cond = "1 /* unsupported jcc */".into();
                    }
                } else if *negate {
                    cond = negate_condition(&cond);
                }
                let mut lines = vec![format!("{indent}while ({cond}) {{")];
                let inner = format!("{indent}    ");
                for n in body {
                    lines.extend(n.render(r, &inner, known));
                }
                lines.push(format!("{indent}    SAFEPOINT();"));
                lines.push(format!("{indent}}}"));
                if let Some(insts) = cond_target_insts {
                    let exit_cond = negate_condition(&cond);
                    lines.push(format!("{indent}if ({exit_cond}) {{"));
                    for insn in insts {
                        for code in r.format_instruction(insn, known) {
                            lines.push(format!("{indent}    {code}"));
                        }
                    }
                    lines.push(format!("{indent}}}"));
                }
                lines
            }
            AstNode::DoWhile {
                cond_mnem,
                body,
                prev,
                negate,
                ..
            } => {
                let mut cond = jcc_condition(cond_mnem, prev.as_ref(), None);
                if cond.contains("unsupported jcc") {
                    if cond_mnem == "jmp" || cond_mnem == "ljmp" || !cond_mnem.starts_with('j') {
                        cond = "1".into();
                    } else {
                        cond = "1 /* unsupported jcc */".into();
                    }
                } else if *negate {
                    cond = negate_condition(&cond);
                }
                let mut lines = vec![format!("{indent}do {{")];
                let inner = format!("{indent}    ");
                for n in body {
                    lines.extend(n.render(r, &inner, known));
                }
                lines.push(format!("{indent}    SAFEPOINT();"));
                lines.push(format!("{indent}}} while ({cond});"));
                lines
            }
            AstNode::IfElse {
                prefix_insts,
                mnem,
                prev,
                then_body,
                else_body,
                negate,
                ..
            } => {
                let mut lines: Vec<String> = Vec::new();
                for insn in prefix_insts {
                    for code in r.format_instruction(insn, known) {
                        lines.push(format!("{indent}{code}"));
                    }
                }
                let mut cond = jcc_condition(mnem, prev.as_ref(), None);
                if *negate {
                    cond = negate_condition(&cond);
                }
                lines.push(format!("{indent}if ({cond}) {{"));
                let branch = format!("{indent}    ");
                for n in then_body {
                    lines.extend(n.render(r, &branch, known));
                }
                r.flush_pending_dos(&mut lines, &branch);
                lines.push(format!("{indent}}}"));
                if let Some(eb) = else_body {
                    if Self::non_fallthrough(then_body) {
                        for n in eb {
                            lines.extend(n.render(r, indent, known));
                        }
                        r.flush_pending_dos(&mut lines, indent);
                    } else {
                        let last = lines.len() - 1;
                        lines[last] = format!("{indent}}} else {{");
                        for n in eb {
                            lines.extend(n.render(r, &branch, known));
                        }
                        r.flush_pending_dos(&mut lines, &branch);
                        lines.push(format!("{indent}}}"));
                    }
                }
                lines
            }
            AstNode::Break => vec![format!("{indent}break;")],
            AstNode::Continue => vec![format!("{indent}continue;")],
            AstNode::Return {
                mnemonic,
                pop_bytes,
                ..
            } => {
                let mut lines = Vec::new();
                if mnemonic == "retf" {
                    match pop_bytes {
                        Some(p) => lines.push(format!("{indent}retf_pop({p});")),
                        None => lines.push(format!("{indent}retf();")),
                    }
                    return lines;
                }
                if let Some(p) = pop_bytes {
                    lines.push(format!("{indent}sp = (sp + {p}) & 0xFFFF;"));
                }
                lines.push(format!("{indent}return;"));
                lines
            }
            AstNode::Goto { start, insn } | AstNode::Call { start, insn } => {
                let mut lines = Vec::new();
                let mut label = r
                    .func_names
                    .get(start)
                    .map(|n| format!("{}{}", r.prefix, n));
                if label.is_none() && r.extern_labels.contains(start) {
                    label = Some(r.func_name(*start));
                }
                if let Some(l) = label {
                    if !known.contains(start) {
                        lines.push(format!("{l}:"));
                    }
                }
                for code in r.format_instruction(insn, known) {
                    lines.push(format!("{indent}{code}"));
                }
                lines
            }
            AstNode::Switch { expr, cases } => {
                let mut lines = vec![format!("{indent}switch ({expr}) {{")];
                for (val, body) in cases {
                    match body {
                        CaseBody::Int(b) => {
                            let target = if known.contains(b) {
                                format!("func_{b:04X}();")
                            } else {
                                format!("/* 0x{b:04X} */")
                            };
                            lines.push(format!("{indent}    case {val}: {target} break;"));
                        }
                        CaseBody::Nodes(nodes) => {
                            lines.push(format!("{indent}    case {val}:"));
                            let inner = format!("{indent}        ");
                            for n in nodes {
                                lines.extend(n.render(r, &inner, known));
                            }
                            lines.push(format!("{indent}        break;"));
                        }
                    }
                }
                lines.push(format!("{indent}}}"));
                lines
            }
        }
    }
}
