//! Port of compiler/patterns/{conditionals,switch,loops}.py — structuring pattern
//! detectors for the base (readable-C) renderer. Each returns (AstNode, consumed).

use crate::ast::{AstNode, CaseBody};
use crate::cfg::{build_cfg, collect_branch, nearest_common_postdom, reduce_region};
use crate::graph::DiGraph;
use crate::ir_to_c::{decode_variables, rewrite_mem_op, BasicBlock, Insn, Renderer};
use indexmap::{IndexMap, IndexSet};
use regex::Regex;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;

type LoopMap = IndexMap<i64, (IndexSet<i64>, IndexSet<i64>)>;
type IPost = IndexMap<i64, i64>;

fn ms<'a>(i: &'a Insn, k: &str) -> &'a str {
    i.get(k).and_then(Value::as_str).unwrap_or("")
}
fn maddr(i: &Insn) -> i64 {
    i.get("address").and_then(Value::as_i64).unwrap_or(0)
}
fn msize(i: &Insn) -> i64 {
    (ms(i, "bytes").len() / 2) as i64
}
fn cond_prev(i: &Insn) -> Option<Insn> {
    i.get("cond_prev").and_then(|v| v.as_object()).cloned()
}
fn ret_node() -> AstNode {
    AstNode::Return {
        start: None,
        mnemonic: "ret".into(),
        pop_bytes: None,
    }
}
fn prefix_of(block: &BasicBlock) -> Vec<Insn> {
    let n = block.instructions.len();
    if n == 0 {
        Vec::new()
    } else {
        block.instructions[..n - 1].to_vec()
    }
}

/// the source _collect_starts.
fn collect_starts(nodes: &[AstNode], out: &mut IndexSet<i64>) {
    for node in nodes {
        if let Some(s) = node.start() {
            out.insert(s);
        }
        match node {
            AstNode::ForLoop { body, .. }
            | AstNode::Loop { body, .. }
            | AstNode::DoWhile { body, .. } => collect_starts(body, out),
            AstNode::IfElse {
                then_body,
                else_body,
                ..
            } => {
                collect_starts(then_body, out);
                if let Some(eb) = else_body {
                    collect_starts(eb, out);
                }
            }
            _ => {}
        }
    }
}

// ===================== the source: detect_if_else =====================

#[allow(clippy::too_many_arguments)]
pub fn detect_if_else(
    r: &mut Renderer,
    start: i64,
    blocks: &BTreeMap<i64, BasicBlock>,
    graph: &DiGraph,
    loop_map: &LoopMap,
    ipost: &IPost,
    loop_exits: &IndexSet<i64>,
    known: &mut BTreeSet<i64>,
) -> Option<(AstNode, IndexSet<i64>)> {
    let mut loop_breaks: IndexSet<i64> = IndexSet::new();
    for &ex in loop_exits {
        match blocks.get(&ex) {
            Some(b) if !b.instructions.is_empty() => {
                let last = b.instructions.last().unwrap();
                let m = ms(last, "mnemonic");
                if matches!(m, "ret" | "retn" | "retf") || m == "jmp" {
                    continue;
                }
                loop_breaks.insert(ex);
            }
            _ => continue,
        }
    }

    let block = match blocks.get(&start) {
        Some(b) if !b.instructions.is_empty() => b,
        _ => return None,
    };
    let last = block.instructions.last().unwrap();
    let lm = ms(last, "mnemonic").to_string();
    if !(lm.starts_with('j') && lm != "jmp") {
        return None;
    }
    let target = r.parse_imm(ms(last, "op_str"));
    let fall = maddr(last) + msize(last);
    let target = target?;
    if !blocks.contains_key(&fall) && !loop_breaks.contains(&fall) {
        return None;
    }
    let cp = cond_prev(last);

    // fall is a loop break
    if !blocks.contains_key(&fall) && loop_breaks.contains(&fall) {
        let exit_node = if loop_map.contains_key(&fall) {
            AstNode::Continue
        } else {
            ret_node()
        };
        let node = AstNode::IfElse {
            prefix_insts: prefix_of(block),
            mnem: lm,
            prev: cp,
            then_body: vec![exit_node],
            else_body: None,
            target: Some(target),
            negate: true,
            start,
        };
        return Some((node, IndexSet::from([start])));
    }

    // target is a single-pred block ending in ret
    if let Some(tb) = blocks.get(&target) {
        if graph.in_degree(target) == 1
            && !tb.instructions.is_empty()
            && matches!(
                ms(tb.instructions.last().unwrap(), "mnemonic"),
                "ret" | "retn" | "retf"
            )
        {
            if collect_branch(graph, fall, None, &IndexSet::new()).contains(&start) {
                return None;
            }
            let mut body_nodes = Vec::new();
            let body_instrs: Vec<Insn> = tb.instructions[..tb.instructions.len() - 1].to_vec();
            if !body_instrs.is_empty() {
                body_nodes.push(AstNode::BasicBlock {
                    start: target,
                    instructions: body_instrs,
                });
            }
            let ret_mnem = ms(tb.instructions.last().unwrap(), "mnemonic").to_string();
            body_nodes.push(AstNode::Return {
                start: Some(target),
                mnemonic: ret_mnem,
                pop_bytes: None,
            });
            let node = AstNode::IfElse {
                prefix_insts: prefix_of(block),
                mnem: lm,
                prev: cp,
                then_body: body_nodes,
                else_body: None,
                target: Some(target),
                negate: false,
                start,
            };
            return Some((node, IndexSet::from([start, target])));
        }
    }

    // conditional break from enclosing loop
    if loop_breaks.contains(&target) {
        let prefix = prefix_of(block);
        let min_break = loop_breaks.iter().copied().min();
        if loop_breaks.len() == 1 || Some(target) == min_break {
            let node = AstNode::IfElse {
                prefix_insts: prefix,
                mnem: lm,
                prev: cp,
                then_body: vec![AstNode::Break],
                else_body: None,
                target: Some(target),
                negate: false,
                start,
            };
            let mut consumed = IndexSet::from([start]);
            if blocks.contains_key(&target) {
                consumed.insert(target);
            }
            return Some((node, consumed));
        }
        let mut goto_insn = serde_json::Map::new();
        goto_insn.insert("address".into(), Value::from(maddr(last)));
        goto_insn.insert("mnemonic".into(), Value::from("jmp"));
        goto_insn.insert("op_str".into(), Value::from(format!("0x{target:04X}")));
        goto_insn.insert("bytes".into(), Value::from(""));
        goto_insn.insert("force_pc".into(), Value::from(true));
        if blocks.contains_key(&target) {
            r.func_names
                .entry(target)
                .or_insert_with(|| format!("label_{target:04X}"));
            known.remove(&target);
        }
        let node = AstNode::IfElse {
            prefix_insts: prefix,
            mnem: lm,
            prev: cp,
            then_body: vec![AstNode::Goto {
                start: maddr(last),
                insn: goto_insn,
            }],
            else_body: None,
            target: Some(target),
            negate: false,
            start,
        };
        return Some((node, IndexSet::from([start])));
    }

    // general if/else
    let (merge, then_nodes, else_nodes): (i64, IndexSet<i64>, IndexSet<i64>);
    if blocks.contains_key(&target) {
        let m = nearest_common_postdom(ipost, fall, target);
        merge = m;
        then_nodes = collect_branch(graph, fall, Some(m), &IndexSet::from([target]));
        else_nodes = collect_branch(graph, target, Some(m), &IndexSet::from([fall]));
    } else {
        merge = target;
        then_nodes = collect_branch(graph, fall, Some(target), &IndexSet::new());
        else_nodes = IndexSet::new();
    }
    let mut consumed = IndexSet::from([start]);
    consumed.extend(then_nodes.iter().copied());
    consumed.extend(else_nodes.iter().copied());

    let (mut then_blocks, _) = reduce_region(blocks, graph, fall, merge);
    let (else_blocks, _) = reduce_region(blocks, graph, target, merge);

    let all_keys: BTreeSet<i64> = blocks.keys().copied().collect();
    let then_keys: BTreeSet<i64> = then_blocks.keys().copied().collect();
    let else_keys: BTreeSet<i64> = else_blocks.keys().copied().collect();
    if then_blocks.contains_key(&start)
        || else_blocks.contains_key(&start)
        || then_keys == all_keys
        || else_keys == all_keys
    {
        return None;
    }

    if merge == target {
        let tb0_is_jump = blocks
            .get(&target)
            .and_then(|b| b.instructions.first())
            .map(|i| ms(i, "mnemonic").starts_with('j'))
            .unwrap_or(false);
        let has_insns = blocks
            .get(&target)
            .map(|b| !b.instructions.is_empty())
            .unwrap_or(false);
        if has_insns && !tb0_is_jump {
            then_blocks.remove(&target);
        } else {
            consumed.insert(target);
            then_blocks.remove(&target);
        }
    }

    let then_ast = if !then_blocks.is_empty() {
        let g = build_cfg(&then_blocks);
        r.structure(&then_blocks, &g, known, fall, &loop_breaks)
    } else {
        Vec::new()
    };
    let else_ast = if !else_blocks.is_empty() {
        let g = build_cfg(&else_blocks);
        Some(r.structure(&else_blocks, &g, known, target, &loop_breaks))
    } else {
        None
    };

    let node = AstNode::IfElse {
        prefix_insts: prefix_of(block),
        mnem: lm,
        prev: cp,
        then_body: then_ast,
        else_body: else_ast,
        target: Some(target),
        negate: true,
        start,
    };
    Some((node, consumed))
}

// ===================== the source: detect_switch =====================

fn switch_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)\[bx\s*\+\s*(0x[0-9a-fA-F]+|[0-9a-fA-F]+h|\d+)\]").unwrap())
}
fn bh0_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)^bh,\s*0").unwrap())
}
fn bhbh_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)^bh\s*,\s*bh").unwrap())
}

#[allow(clippy::too_many_arguments)]
pub fn detect_switch(
    r: &mut Renderer,
    start: i64,
    blocks: &BTreeMap<i64, BasicBlock>,
    graph: &DiGraph,
    _loop_map: &LoopMap,
    ipost: &IPost,
    loop_exits: &IndexSet<i64>,
    known: &mut BTreeSet<i64>,
) -> Option<(AstNode, IndexSet<i64>)> {
    let block = blocks.get(&start)?;
    if block.instructions.len() < 4 {
        return None;
    }
    let n = block.instructions.len();
    let mov1 = &block.instructions[n - 4];
    let mov2 = &block.instructions[n - 3];
    let scale = &block.instructions[n - 2];
    let jmp = &block.instructions[n - 1];
    if ms(jmp, "mnemonic") != "jmp" {
        return None;
    }
    let op_str = ms(jmp, "op_str");
    if !op_str.contains('[') {
        return None;
    }
    let m = switch_re().captures(op_str)?;
    let table_off = crate::disassemble::parse_imm(m.get(1).unwrap().as_str())?;
    if !(ms(mov1, "mnemonic") == "mov" && ms(mov1, "op_str").to_lowercase().starts_with("bl,")) {
        return None;
    }
    let mov2_op = ms(mov2, "op_str");
    let bh_zeroed = (ms(mov2, "mnemonic") == "mov" && bh0_re().is_match(mov2_op))
        || (ms(mov2, "mnemonic") == "xor" && bhbh_re().is_match(mov2_op));
    if !bh_zeroed {
        return None;
    }
    if !(matches!(ms(scale, "mnemonic"), "add" | "shl")
        && matches!(
            ms(scale, "op_str").to_lowercase().as_str(),
            "bx, bx" | "bx, 1"
        ))
    {
        return None;
    }
    let expr_raw = ms(mov1, "op_str")
        .splitn(2, ',')
        .nth(1)
        .unwrap_or("")
        .trim()
        .to_string();
    let expr = rewrite_mem_op(&decode_variables(&expr_raw), None);

    let mut cases: Vec<(i64, i64)> = Vec::new();
    let mut index = 0i64;
    let code = &r.code_bytes;
    loop {
        let off = (table_off + index * 2) as usize;
        if off + 2 > code.len() {
            break;
        }
        let addr = code[off] as i64 | ((code[off + 1] as i64) << 8);
        if !known.contains(&addr)
            && !blocks.contains_key(&addr)
            && !r.current_func_all_addrs.contains(&addr)
        {
            break;
        }
        cases.push((index, addr));
        index += 1;
        if index > 256 {
            break;
        }
    }
    if cases.is_empty() {
        return None;
    }

    let _ = loop_exits;
    let internal: Vec<i64> = cases
        .iter()
        .filter(|(_, a)| blocks.contains_key(a))
        .map(|(_, a)| *a)
        .collect();
    if !internal.is_empty() {
        let mut merge = internal[0];
        for &a in &internal[1..] {
            merge = nearest_common_postdom(ipost, merge, a);
        }
        if blocks.contains_key(&merge) {
            let mut consumed = IndexSet::from([start]);
            let mut new_cases: Vec<(i64, CaseBody)> = Vec::new();
            for (val, addr) in &cases {
                if blocks.contains_key(addr) {
                    let (sub_blocks, region_nodes) = reduce_region(blocks, graph, *addr, merge);
                    let sub_graph = build_cfg(&sub_blocks);
                    let entry_block = sub_blocks.keys().copied().min().unwrap_or(*addr);
                    let body = r.structure(&sub_blocks, &sub_graph, known, entry_block, loop_exits);
                    new_cases.push((*val, CaseBody::Nodes(body)));
                    consumed.extend(region_nodes);
                } else {
                    new_cases.push((*val, CaseBody::Int(*addr)));
                }
            }
            return Some((
                AstNode::Switch {
                    expr,
                    cases: new_cases,
                },
                consumed,
            ));
        }
    }
    let node = AstNode::Switch {
        expr,
        cases: cases
            .into_iter()
            .map(|(v, a)| (v, CaseBody::Int(a)))
            .collect(),
    };
    Some((node, IndexSet::from([start])))
}

// ===================== the source: detect_for_loop / detect_loop =====================

fn mk_dec_cx() -> Insn {
    let mut m = serde_json::Map::new();
    m.insert("mnemonic".into(), Value::from("dec"));
    m.insert("op_str".into(), Value::from("cx"));
    m
}

#[allow(clippy::too_many_arguments)]
pub fn detect_for_loop(
    r: &mut Renderer,
    start: i64,
    blocks: &BTreeMap<i64, BasicBlock>,
    graph: &DiGraph,
    loop_map: &LoopMap,
    _ipost: &IPost,
    loop_exits: &IndexSet<i64>,
    known: &mut BTreeSet<i64>,
) -> Option<(AstNode, IndexSet<i64>)> {
    let init_block = match blocks.get(&start) {
        Some(b) if !b.instructions.is_empty() => b,
        _ => return None,
    };
    let last = init_block.instructions.last().unwrap();

    // Pattern: init; JCXZ; loop back-edge.
    if ms(last, "mnemonic") == "jcxz" {
        let exit_target = r.parse_imm(ms(last, "op_str"));
        let mut succs: IndexSet<i64> = graph.successors(start).into_iter().collect();
        if let Some(et) = exit_target {
            succs.shift_remove(&et);
        }
        if succs.len() != 1 {
            return None;
        }
        let header = *succs.iter().next().unwrap();
        let (body_nodes, exits) = loop_map.get(&header)?.clone();
        if body_nodes.contains(&start) {
            return None;
        }
        let latch: Vec<i64> = body_nodes
            .iter()
            .copied()
            .filter(|&a| {
                blocks
                    .get(&a)
                    .and_then(|b| b.instructions.last())
                    .map_or(false, |li| {
                        ms(li, "mnemonic") == "loop"
                            && r.parse_imm(ms(li, "op_str")) == Some(header)
                    })
            })
            .collect();
        if latch.len() != 1 {
            return None;
        }
        let latch_addr = latch[0];
        let mut sub_blocks: BTreeMap<i64, BasicBlock> = BTreeMap::new();
        for &a in &body_nodes {
            let mut instrs = blocks[&a].instructions.clone();
            if a == latch_addr {
                instrs.pop();
            } else if instrs.last().map_or(false, |li| {
                ms(li, "mnemonic") == "jmp" && r.parse_imm(ms(li, "op_str")) == Some(header)
            }) {
                instrs.pop();
            }
            if !instrs.is_empty() {
                sub_blocks.insert(
                    a,
                    BasicBlock {
                        start: a,
                        instructions: instrs,
                    },
                );
            }
        }
        let mut sub_graph = build_cfg(&sub_blocks);
        if sub_graph.contains(header) {
            for pred in sub_graph.predecessors(header) {
                sub_graph.remove_edge(pred, header);
            }
        }
        let mut new_exits = loop_exits.clone();
        new_exits.extend(exits.iter().copied());
        let mut body_ast = r.structure(&sub_blocks, &sub_graph, known, header, &new_exits);
        body_ast.retain(|nd| !nd.start().map_or(false, |sx| exits.contains(&sx)));
        let exit_blocks: IndexSet<i64> = exits
            .iter()
            .copied()
            .filter(|e| sub_blocks.contains_key(e))
            .collect();
        let mut referenced = IndexSet::new();
        collect_starts(&body_ast, &mut referenced);
        let node = AstNode::ForLoop {
            init_insts: prefix_of(init_block),
            cond_mnem: ms(last, "mnemonic").into(),
            step_inst: Some(mk_dec_cx()),
            body: body_ast,
            cond_prev: cond_prev(last),
        };
        let mut consumed: IndexSet<i64> = body_nodes.iter().copied().collect();
        consumed.insert(start);
        for e in &exit_blocks {
            if referenced.contains(e) {
                consumed.insert(*e);
            }
        }
        return Some((node, consumed));
    }

    // Pattern: init; jmp header; cond; step; jmp.
    if ms(last, "mnemonic") != "jmp" {
        return None;
    }
    let target = r.parse_imm(ms(last, "op_str"));
    let target = match target {
        Some(t) if loop_map.contains_key(&t) => t,
        _ => return None,
    };
    let (body_nodes, exits) = loop_map.get(&target)?.clone();
    if body_nodes.contains(&start) {
        return None;
    }
    let body_only: IndexSet<i64> = body_nodes
        .iter()
        .copied()
        .filter(|&x| x != target)
        .collect();
    if body_only.is_empty() {
        return None;
    }
    let latch: Vec<i64> = body_only
        .iter()
        .copied()
        .filter(|&a| {
            blocks
                .get(&a)
                .and_then(|b| b.instructions.last())
                .map_or(false, |li| {
                    ms(li, "mnemonic") == "jmp" && r.parse_imm(ms(li, "op_str")) == Some(target)
                })
        })
        .collect();
    if latch.len() != 1 {
        return None;
    }
    let latch_addr = latch[0];
    let latch_block = &blocks[&latch_addr];
    if latch_block.instructions.len() < 2 {
        return None;
    }
    let step_inst = latch_block.instructions[latch_block.instructions.len() - 2].clone();
    if ms(&step_inst, "op_str").contains('[') {
        return None;
    }
    let mut sub_blocks: BTreeMap<i64, BasicBlock> = BTreeMap::new();
    for &a in &body_only {
        let mut instrs = blocks[&a].instructions.clone();
        if a == latch_addr {
            let keep = instrs.len().saturating_sub(2);
            instrs.truncate(keep);
        } else if instrs.last().map_or(false, |li| {
            ms(li, "mnemonic") == "jmp" && r.parse_imm(ms(li, "op_str")) == Some(target)
        }) {
            instrs.pop();
        }
        if !instrs.is_empty() {
            sub_blocks.insert(
                a,
                BasicBlock {
                    start: a,
                    instructions: instrs,
                },
            );
        }
    }
    let mut sub_graph = build_cfg(&sub_blocks);
    if sub_graph.contains(target) {
        for pred in sub_graph.predecessors(target) {
            sub_graph.remove_edge(pred, target);
        }
    }
    let mut new_exits = loop_exits.clone();
    new_exits.extend(exits.iter().copied());
    let mut body_ast = r.structure(&sub_blocks, &sub_graph, known, target, &new_exits);
    body_ast.retain(|nd| !nd.start().map_or(false, |sx| exits.contains(&sx)));
    let exit_blocks: IndexSet<i64> = exits
        .iter()
        .copied()
        .filter(|e| sub_blocks.contains_key(e))
        .collect();
    let mut referenced = IndexSet::new();
    collect_starts(&body_ast, &mut referenced);
    let cond_inst = blocks[&target].instructions.last().unwrap();
    let node = AstNode::ForLoop {
        init_insts: prefix_of(init_block),
        cond_mnem: ms(cond_inst, "mnemonic").into(),
        step_inst: Some(step_inst),
        body: body_ast,
        cond_prev: cond_prev(cond_inst),
    };
    let mut consumed: IndexSet<i64> = body_nodes.iter().copied().collect();
    consumed.insert(start);
    for e in &exit_blocks {
        if referenced.contains(e) {
            consumed.insert(*e);
        }
    }
    Some((node, consumed))
}

fn complement(m: &str) -> Option<&'static str> {
    Some(match m {
        "ja" => "jbe",
        "jbe" => "ja",
        "jae" => "jb",
        "jb" => "jae",
        "jg" => "jle",
        "jle" => "jg",
        "jge" => "jl",
        "jl" => "jge",
        "je" => "jne",
        "jne" => "je",
        "jz" => "jnz",
        "jnz" => "jz",
        "jc" => "jnc",
        "jnc" => "jc",
        "js" => "jns",
        "jns" => "js",
        _ => return None,
    })
}

#[allow(clippy::too_many_arguments)]
pub fn detect_loop(
    r: &mut Renderer,
    start: i64,
    blocks: &BTreeMap<i64, BasicBlock>,
    _graph: &DiGraph,
    loop_map: &LoopMap,
    _ipost: &IPost,
    loop_exits: &IndexSet<i64>,
    known: &mut BTreeSet<i64>,
) -> Option<(AstNode, IndexSet<i64>)> {
    let (body_nodes, exits) = loop_map.get(&start)?.clone();

    // latch: a body block (!= header) whose last insn jumps back to start.
    let mut latch_addr: Option<i64> = None;
    let mut latch_inst: Option<Insn> = None;
    for &a in &body_nodes {
        if a == start {
            continue;
        }
        if let Some(b) = blocks.get(&a) {
            if let Some(li) = b.instructions.last() {
                if r.parse_imm(ms(li, "op_str")) == Some(start) {
                    latch_addr = Some(a);
                    latch_inst = Some(li.clone());
                    break;
                }
            }
        }
    }

    let header_block = &blocks[&start];
    let header_last = header_block.instructions.last().unwrap().clone();
    let target = r.parse_imm(ms(&header_last, "op_str"));
    let latch_mnem = latch_inst.as_ref().map(|i| ms(i, "mnemonic").to_string());
    let treat_as_while = latch_mnem.as_deref().is_some()
        && complement(ms(&header_last, "mnemonic")) == latch_mnem.as_deref();

    let hlm = ms(&header_last, "mnemonic").to_string();
    let header_cond_into_body = latch_inst
        .as_ref()
        .map_or(false, |li| ms(li, "mnemonic") == "jmp")
        && hlm.starts_with('j')
        && hlm != "jmp"
        && target.map_or(false, |t| body_nodes.contains(&t));

    let mut sub_blocks: BTreeMap<i64, BasicBlock> = BTreeMap::new();
    let full_header = || BasicBlock {
        start,
        instructions: header_block.instructions.clone(),
    };
    let header_first_mnem = header_block
        .instructions
        .first()
        .map(|i| ms(i, "mnemonic").to_string());

    if header_cond_into_body {
        sub_blocks.insert(start, full_header());
    } else if latch_inst
        .as_ref()
        .map_or(false, |li| ms(li, "mnemonic") != "jmp")
        && !treat_as_while
    {
        sub_blocks.insert(start, full_header());
    } else {
        let prefix = prefix_of(header_block);
        let cond_none = header_last.get("cond_prev").map_or(true, |v| v.is_null());
        if !prefix.is_empty()
            && hlm.starts_with('j')
            && hlm != "jmp"
            && target.map_or(true, |t| !body_nodes.contains(&t))
            && latch_inst
                .as_ref()
                .map_or(false, |li| ms(li, "mnemonic") == "jmp")
            && cond_none
        {
            sub_blocks.insert(start, full_header());
        } else if !prefix.is_empty() {
            sub_blocks.insert(
                start,
                BasicBlock {
                    start,
                    instructions: prefix,
                },
            );
        } else if header_first_mnem
            .as_deref()
            .map_or(false, |m| !matches!(m, "jmp" | "ret" | "retn" | "retf"))
        {
            sub_blocks.insert(start, full_header());
        }
    }

    for &a in &body_nodes {
        if a == start {
            continue;
        }
        let mut instrs = blocks[&a].instructions.clone();
        if instrs
            .last()
            .map_or(false, |li| r.parse_imm(ms(li, "op_str")) == Some(start))
        {
            if latch_addr == Some(a) {
                instrs.pop();
            } else if matches!(ms(instrs.last().unwrap(), "mnemonic"), "jmp" | "loop") {
                instrs.pop();
            }
        }
        if !instrs.is_empty() {
            sub_blocks.insert(
                a,
                BasicBlock {
                    start: a,
                    instructions: instrs,
                },
            );
        }
    }
    for &e in &exits {
        if let Some(eb) = blocks.get(&e) {
            if !eb.instructions.is_empty() {
                sub_blocks.insert(
                    e,
                    BasicBlock {
                        start: e,
                        instructions: eb.instructions.clone(),
                    },
                );
            }
        }
    }

    let mut sub_graph = build_cfg(&sub_blocks);
    if sub_graph.contains(start) {
        for pred in sub_graph.predecessors(start) {
            sub_graph.remove_edge(pred, start);
        }
    }
    let mut new_exits = loop_exits.clone();
    new_exits.extend(exits.iter().copied());
    let mut body_ast = r.structure(&sub_blocks, &sub_graph, known, start, &new_exits);
    body_ast.retain(|nd| !nd.start().map_or(false, |sx| exits.contains(&sx)));
    let exit_blocks: IndexSet<i64> = exits
        .iter()
        .copied()
        .filter(|e| sub_blocks.contains_key(e))
        .collect();

    let mut early_ret_target: Option<i64> = None;
    let mut early_ret_insts: Option<Vec<Insn>> = None;

    let node: AstNode = if header_cond_into_body {
        AstNode::DoWhile {
            header: start,
            cond_mnem: "jmp".into(),
            body: body_ast,
            exit_nodes: exits.clone(),
            prev: None,
            negate: false,
        }
    } else if latch_inst
        .as_ref()
        .map_or(false, |li| ms(li, "mnemonic") != "jmp")
    {
        let li = latch_inst.as_ref().unwrap();
        if treat_as_while {
            AstNode::Loop {
                header: start,
                cond_mnem: ms(li, "mnemonic").into(),
                body: body_ast,
                exit_nodes: exits.clone(),
                prev: cond_prev(li),
                negate: false,
                cond_target: None,
                cond_target_insts: None,
            }
        } else {
            AstNode::DoWhile {
                header: start,
                cond_mnem: ms(li, "mnemonic").into(),
                body: body_ast,
                exit_nodes: exits.clone(),
                prev: cond_prev(li),
                negate: false,
            }
        }
    } else {
        let negate = target.map_or(true, |t| !body_nodes.contains(&t));
        if target == Some(start) && hlm != "jmp" {
            AstNode::DoWhile {
                header: start,
                cond_mnem: hlm.clone(),
                body: body_ast,
                exit_nodes: exits.clone(),
                prev: cond_prev(&header_last),
                negate: false,
            }
        } else {
            let prefix = prefix_of(header_block);
            let trivial = prefix
                .iter()
                .all(|inst| matches!(ms(inst, "mnemonic"), "cmp" | "test" | "or" | "and"));
            let cond_none = header_last.get("cond_prev").map_or(true, |v| v.is_null());
            let special_break = !prefix.is_empty()
                && !trivial
                && hlm.starts_with('j')
                && hlm != "jmp"
                && target.map_or(true, |t| !body_nodes.contains(&t))
                && latch_inst
                    .as_ref()
                    .map_or(false, |li| ms(li, "mnemonic") == "jmp")
                && cond_none;
            if special_break {
                AstNode::Loop {
                    header: start,
                    cond_mnem: "jmp".into(),
                    body: body_ast,
                    exit_nodes: exits.clone(),
                    prev: None,
                    negate: false,
                    cond_target: None,
                    cond_target_insts: None,
                }
            } else if !prefix.is_empty() && !trivial {
                let mut cond_inst = header_last.clone();
                for inst in prefix.iter().rev() {
                    if ms(inst, "mnemonic").starts_with('j') {
                        cond_inst = inst.clone();
                        break;
                    }
                }
                let is_last = ms(&cond_inst, "address") == ms(&header_last, "address")
                    && maddr(&cond_inst) == maddr(&header_last)
                    && ms(&cond_inst, "mnemonic") == ms(&header_last, "mnemonic")
                    && ms(&cond_inst, "op_str") == ms(&header_last, "op_str");
                AstNode::DoWhile {
                    header: start,
                    cond_mnem: ms(&cond_inst, "mnemonic").into(),
                    body: body_ast,
                    exit_nodes: exits.clone(),
                    prev: cond_prev(&cond_inst),
                    negate: if is_last { negate } else { false },
                }
            } else {
                if target.map_or(false, |t| !body_nodes.contains(&t))
                    && target.map_or(false, |t| blocks.contains_key(&t))
                    && target.and_then(|t| blocks.get(&t)).map_or(false, |b| {
                        b.instructions.last().map_or(false, |li| {
                            matches!(ms(li, "mnemonic"), "ret" | "retn" | "retf")
                        })
                    })
                {
                    let t = target.unwrap();
                    early_ret_target = Some(t);
                    early_ret_insts = Some(blocks[&t].instructions.clone());
                }
                AstNode::Loop {
                    header: start,
                    cond_mnem: hlm.clone(),
                    body: body_ast,
                    exit_nodes: exits.clone(),
                    prev: cond_prev(&header_last),
                    negate,
                    cond_target: None,
                    cond_target_insts: None,
                }
            }
        }
    };

    let mut referenced = IndexSet::new();
    // collect_starts needs body from `node`; re-derive from node's body.
    if let Some(b) = node_body(&node) {
        collect_starts(b, &mut referenced);
    }
    let mut consumed: IndexSet<i64> = body_nodes.iter().copied().collect();
    for e in &exit_blocks {
        if referenced.contains(e) {
            consumed.insert(*e);
        }
    }
    let mut node = node;
    if let Some(t) = early_ret_target {
        consumed.insert(t);
        if let AstNode::Loop {
            cond_target,
            cond_target_insts,
            ..
        } = &mut node
        {
            *cond_target = Some(t);
            *cond_target_insts = early_ret_insts;
        }
    }
    Some((node, consumed))
}

fn node_body(node: &AstNode) -> Option<&Vec<AstNode>> {
    match node {
        AstNode::Loop { body, .. } | AstNode::DoWhile { body, .. } => Some(body),
        _ => None,
    }
}
