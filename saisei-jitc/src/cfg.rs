//! CFG construction + dominator/loop analysis over the
//! insertion-ordered DiGraph. Used only by the structured (readable-C) renderer.

use crate::disassemble::parse_imm; // the source's _parse_imm == disassemble's (bare-decimal+sign)
use crate::graph::DiGraph;
use crate::ir_to_c::BasicBlock;
use indexmap::{IndexMap, IndexSet};
use serde_json::Value;
use std::collections::BTreeMap;

fn s<'a>(i: &'a serde_json::Map<String, Value>, k: &str) -> &'a str {
    i.get(k).and_then(Value::as_str).unwrap_or("")
}
fn addr(i: &serde_json::Map<String, Value>) -> i64 {
    i.get("address").and_then(Value::as_i64).unwrap_or(0)
}
fn size(i: &serde_json::Map<String, Value>) -> i64 {
    (s(i, "bytes").len() / 2) as i64
}

/// build_cfg.
pub fn build_cfg(blocks: &BTreeMap<i64, BasicBlock>) -> DiGraph {
    let mut g = DiGraph::new();
    for &a in blocks.keys() {
        g.add_node(a);
    }
    for (&a, block) in blocks {
        let last = match block.instructions.last() {
            Some(l) => l,
            None => continue,
        };
        let mnem = s(last, "mnemonic");
        let cur_end = addr(last) + size(last);
        if mnem.starts_with('j') || mnem.starts_with("loop") || mnem == "ljmp" {
            let target = parse_imm(s(last, "op_str"));
            if mnem != "jmp" && mnem != "ljmp" && blocks.contains_key(&cur_end) {
                g.add_edge(a, cur_end);
            }
            if let Some(t) = target {
                if blocks.contains_key(&t) {
                    g.add_edge(a, t);
                }
            }
        } else {
            let mut is_exit = matches!(mnem, "ret" | "retn" | "retf" | "hlt" | "iret");
            if mnem == "int" {
                let op = s(last, "op_str");
                if op == "0x20" {
                    is_exit = true;
                } else if op == "0x21" && block.instructions.len() >= 2 {
                    let prev = &block.instructions[block.instructions.len() - 2];
                    if s(prev, "mnemonic") == "mov"
                        && matches!(s(prev, "op_str"), "ax, 0x4c00" | "ah, 0x4c")
                    {
                        is_exit = true;
                    }
                }
            }
            if !is_exit && blocks.contains_key(&cur_end) {
                g.add_edge(a, cur_end);
            }
        }
    }
    g
}

/// compute_dominators.
pub fn compute_dominators(graph: &DiGraph, entry: i64) -> IndexMap<i64, IndexSet<i64>> {
    let mut reachable = IndexSet::new();
    reachable.insert(entry);
    for d in graph.descendants(entry) {
        reachable.insert(d);
    }
    let sub = graph.subgraph(&reachable);
    let idom = sub.immediate_dominators(entry);
    let mut dominators: IndexMap<i64, IndexSet<i64>> = IndexMap::new();
    for n in sub.nodes() {
        let mut cur = n;
        let mut dom = IndexSet::new();
        dom.insert(cur);
        while cur != entry {
            cur = idom[&cur];
            dom.insert(cur);
        }
        dominators.insert(n, dom);
    }
    dominators
}

/// compute_postdominators.
pub fn compute_postdominators(graph: &DiGraph, exit: i64) -> IndexMap<i64, IndexSet<i64>> {
    let rev = graph.reverse();
    let idom = rev.immediate_dominators(exit);
    let mut post: IndexMap<i64, IndexSet<i64>> = IndexMap::new();
    for n in rev.nodes() {
        let mut cur = n;
        let mut dom = IndexSet::new();
        dom.insert(cur);
        while cur != exit {
            cur = idom[&cur];
            dom.insert(cur);
        }
        post.insert(n, dom);
    }
    post
}

/// compute_immediate_postdominators.
pub fn compute_immediate_postdominators(graph: &DiGraph) -> IndexMap<i64, i64> {
    if graph.is_empty() {
        return IndexMap::new();
    }
    let mut g = graph.clone();
    let exit_node = graph.nodes().iter().copied().max().unwrap() + 1;
    g.add_node(exit_node);
    for n in graph.nodes() {
        if graph.out_degree(n) == 0 {
            g.add_edge(n, exit_node);
        }
    }
    let rev = g.reverse();
    let mut ipdom = rev.immediate_dominators(exit_node);
    ipdom.shift_remove(&exit_node);
    ipdom
}

/// _postdominates.
pub fn postdominates(ipdom: &IndexMap<i64, i64>, node: i64, cand: i64) -> bool {
    let mut cur = node;
    let mut seen = IndexSet::new();
    while let Some(&next) = ipdom.get(&cur) {
        if !seen.insert(cur) {
            return false;
        }
        cur = next;
        if cur == cand {
            return true;
        }
    }
    false
}

/// merge_shared_tails.
pub fn merge_shared_tails(blocks: &BTreeMap<i64, BasicBlock>, graph: &DiGraph) -> DiGraph {
    let mut g = graph.clone();
    let ins_key = |b: &BasicBlock| -> Vec<(String, String, String)> {
        b.instructions
            .iter()
            .map(|i| {
                (
                    s(i, "mnemonic").into(),
                    s(i, "op_str").into(),
                    s(i, "bytes").into(),
                )
            })
            .collect()
    };
    let mut changed = true;
    while changed {
        changed = false;
        let ipdom = compute_immediate_postdominators(&g);
        for node in g.nodes() {
            let succs = g.successors(node);
            if succs.len() <= 1 {
                continue;
            }
            let cand: Vec<i64> = succs
                .iter()
                .copied()
                .filter(|&sx| g.in_degree(sx) > 1 && postdominates(&ipdom, node, sx))
                .collect();
            if cand.len() <= 1 {
                continue;
            }
            let keep = cand[0];
            let keep_block = match blocks.get(&keep) {
                Some(b) => b,
                None => continue,
            };
            let keep_succs: IndexSet<i64> = g.successors(keep).into_iter().collect();
            let keep_ins = ins_key(keep_block);
            let mut identical = true;
            for &dup in &cand[1..] {
                match blocks.get(&dup) {
                    None => {
                        identical = false;
                        break;
                    }
                    Some(db) => {
                        if ins_key(db) != keep_ins {
                            identical = false;
                            break;
                        }
                        let ds: IndexSet<i64> = g.successors(dup).into_iter().collect();
                        if ds != keep_succs {
                            identical = false;
                            break;
                        }
                    }
                }
            }
            if !identical {
                continue;
            }
            for &dup in &cand[1..] {
                for pred in g.predecessors(dup) {
                    g.add_edge(pred, keep);
                }
                g.remove_node(dup);
            }
            changed = true;
            break;
        }
    }
    g
}

/// traversal_order.
pub fn traversal_order(graph: &DiGraph, entry: i64) -> Vec<i64> {
    let mut order = Vec::new();
    if graph.contains(entry) {
        order = graph.dfs_preorder(entry);
    }
    let seen: IndexSet<i64> = order.iter().copied().collect();
    let mut rest: Vec<i64> = graph
        .nodes()
        .into_iter()
        .filter(|n| !seen.contains(n))
        .collect();
    rest.sort_unstable();
    order.extend(rest);
    order
}

/// find_loops -> header -> (loop_nodes, exits).
pub fn find_loops(
    blocks: &BTreeMap<i64, BasicBlock>,
    graph: &DiGraph,
) -> IndexMap<i64, (IndexSet<i64>, IndexSet<i64>)> {
    let mut loops: IndexMap<i64, (IndexSet<i64>, IndexSet<i64>)> = IndexMap::new();
    if blocks.is_empty() {
        return loops;
    }
    for component in graph.weakly_connected_components() {
        let entry = component.iter().copied().min().unwrap();
        let sub = graph.subgraph(&component);
        let dominators = compute_dominators(&sub, entry);
        for (tail, head) in sub.edges() {
            let dom_tail = dominators.get(&tail);
            if !dom_tail.map_or(false, |d| d.contains(&head)) {
                continue;
            }
            let mut loop_nodes = IndexSet::new();
            loop_nodes.insert(head);
            loop_nodes.insert(tail);
            let mut stack = vec![tail];
            while let Some(node) = stack.pop() {
                for pred in sub.predecessors(node) {
                    if loop_nodes.contains(&pred) {
                        continue;
                    }
                    if dominators.get(&pred).map_or(false, |d| d.contains(&head)) {
                        loop_nodes.insert(pred);
                        stack.push(pred);
                    }
                }
            }
            let mut exits = IndexSet::new();
            for &n in &loop_nodes {
                for succ in sub.successors(n) {
                    if !loop_nodes.contains(&succ) {
                        exits.insert(succ);
                    }
                }
            }
            if let Some((prev_nodes, prev_exits)) = loops.get(&head) {
                for x in prev_nodes {
                    loop_nodes.insert(*x);
                }
                for x in prev_exits {
                    exits.insert(*x);
                }
            }
            loops.insert(head, (loop_nodes, exits));
        }
    }
    // detect_loop iterates body_nodes to pick the latch (first block whose last
    // insn jumps to the header) and breaks on the first hit — so iteration order
    // is output-affecting when a loop has >1 back-edge. the reference holds these as a
    // plain `set`, whose iteration over the small in-range int addresses here is
    // ascending; a DFS-insertion IndexSet is not. Sort so the latch (and every
    // other set-iteration) matches the reference. See jit_12b00_6002 @0x6CC6.
    for (_h, (nodes, exits)) in loops.iter_mut() {
        let mut n: Vec<i64> = nodes.iter().copied().collect();
        n.sort_unstable();
        *nodes = n.into_iter().collect();
        let mut e: Vec<i64> = exits.iter().copied().collect();
        e.sort_unstable();
        *exits = e.into_iter().collect();
    }
    loops
}

/// nearest_common_postdom.
pub fn nearest_common_postdom(ipost: &IndexMap<i64, i64>, a: i64, b: i64) -> i64 {
    let mut seen = IndexSet::new();
    let mut cur = a;
    let mut visited = IndexSet::new();
    loop {
        if !visited.insert(cur) {
            return a;
        }
        seen.insert(cur);
        match ipost.get(&cur) {
            None => break,
            Some(&nxt) => {
                if nxt == cur {
                    break;
                }
                cur = nxt;
            }
        }
    }
    cur = b;
    visited.clear();
    while !seen.contains(&cur) {
        if !visited.insert(cur) {
            return b;
        }
        match ipost.get(&cur) {
            None => break,
            Some(&nxt) => {
                if nxt == cur {
                    break;
                }
                cur = nxt;
            }
        }
    }
    cur
}

/// reduce_region -> (region blocks with trailing jmp-to-exit stripped, region nodes).
pub fn reduce_region(
    blocks: &BTreeMap<i64, BasicBlock>,
    graph: &DiGraph,
    entry: i64,
    exit: i64,
) -> (BTreeMap<i64, BasicBlock>, IndexSet<i64>) {
    if entry == exit {
        return (BTreeMap::new(), IndexSet::new());
    }
    let region_nodes = collect_branch(graph, entry, Some(exit), &IndexSet::new());
    let mut sub = BTreeMap::new();
    for &a in &region_nodes {
        let orig = &blocks[&a];
        let mut instrs = orig.instructions.clone();
        if let Some(last) = instrs.last() {
            if s(last, "mnemonic") == "jmp" && parse_imm(s(last, "op_str")) == Some(exit) {
                instrs.pop();
            }
        }
        sub.insert(
            a,
            BasicBlock {
                start: a,
                instructions: instrs,
            },
        );
    }
    (sub, region_nodes)
}

/// collect_branch.
pub fn collect_branch(
    graph: &DiGraph,
    start: i64,
    stop: Option<i64>,
    other: &IndexSet<i64>,
) -> IndexSet<i64> {
    let mut nodes = IndexSet::new();
    let mut stack = vec![start];
    while let Some(node) = stack.pop() {
        if Some(node) == stop || other.contains(&node) || nodes.contains(&node) {
            continue;
        }
        nodes.insert(node);
        for succ in graph.successors(node) {
            stack.push(succ);
        }
    }
    nodes
}
