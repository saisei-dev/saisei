//! Minimal insertion-ordered directed graph for the structured renderer.
//!
//! The  / patterns/ must produce BYTE-IDENTICAL structured C, which
//! means iteration order has to match the DiGraph exactly: nodes and per-node
//! adjacency are insertion-ordered, and DFS visits neighbors in adjacency order.
//! Dominators are a unique property, so any correct algorithm matches the DiGraph's
//! result. Node ids are i64 (block addresses).

use indexmap::{IndexMap, IndexSet};

#[derive(Clone, Default)]
pub struct DiGraph {
    nodes: IndexSet<i64>,
    succ: IndexMap<i64, IndexSet<i64>>,
    pred: IndexMap<i64, IndexSet<i64>>,
}

impl DiGraph {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_node(&mut self, n: i64) {
        self.nodes.insert(n);
        self.succ.entry(n).or_default();
        self.pred.entry(n).or_default();
    }

    pub fn add_edge(&mut self, u: i64, v: i64) {
        self.add_node(u);
        self.add_node(v);
        self.succ.get_mut(&u).unwrap().insert(v);
        self.pred.get_mut(&v).unwrap().insert(u);
    }

    pub fn contains(&self, n: i64) -> bool {
        self.nodes.contains(&n)
    }
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Nodes in insertion order.
    pub fn nodes(&self) -> Vec<i64> {
        self.nodes.iter().copied().collect()
    }

    /// Successors in edge-insertion order (the DiGraph G.successors / G[n]).
    pub fn successors(&self, n: i64) -> Vec<i64> {
        self.succ
            .get(&n)
            .map(|s| s.iter().copied().collect())
            .unwrap_or_default()
    }
    pub fn predecessors(&self, n: i64) -> Vec<i64> {
        self.pred
            .get(&n)
            .map(|s| s.iter().copied().collect())
            .unwrap_or_default()
    }
    pub fn out_degree(&self, n: i64) -> usize {
        self.succ.get(&n).map(|s| s.len()).unwrap_or(0)
    }
    pub fn in_degree(&self, n: i64) -> usize {
        self.pred.get(&n).map(|s| s.len()).unwrap_or(0)
    }

    /// (u, v) edges in the order the DiGraph yields G.edges(): outer over nodes in
    /// insertion order, inner over each node's successors in insertion order.
    pub fn edges(&self) -> Vec<(i64, i64)> {
        let mut out = Vec::new();
        for &u in &self.nodes {
            if let Some(vs) = self.succ.get(&u) {
                for &v in vs {
                    out.push((u, v));
                }
            }
        }
        out
    }

    pub fn remove_node(&mut self, n: i64) {
        if !self.nodes.shift_remove(&n) {
            return;
        }
        if let Some(succs) = self.succ.shift_remove(&n) {
            for v in succs {
                if let Some(p) = self.pred.get_mut(&v) {
                    p.shift_remove(&n);
                }
            }
        }
        if let Some(preds) = self.pred.shift_remove(&n) {
            for u in preds {
                if let Some(s) = self.succ.get_mut(&u) {
                    s.shift_remove(&n);
                }
            }
        }
    }
    pub fn remove_nodes_from(&mut self, ns: &[i64]) {
        for &n in ns {
            self.remove_node(n);
        }
    }

    pub fn remove_edge(&mut self, u: i64, v: i64) {
        if let Some(s) = self.succ.get_mut(&u) {
            s.shift_remove(&v);
        }
        if let Some(p) = self.pred.get_mut(&v) {
            p.shift_remove(&u);
        }
    }

    /// Subgraph induced on `keep` (node insertion order = self's order filtered).
    pub fn subgraph(&self, keep: &IndexSet<i64>) -> DiGraph {
        let mut g = DiGraph::new();
        for &n in &self.nodes {
            if keep.contains(&n) {
                g.add_node(n);
            }
        }
        for (u, v) in self.edges() {
            if keep.contains(&u) && keep.contains(&v) {
                g.add_edge(u, v);
            }
        }
        g
    }

    /// Reversed graph (edges flipped), preserving node insertion order.
    pub fn reverse(&self) -> DiGraph {
        let mut g = DiGraph::new();
        for &n in &self.nodes {
            g.add_node(n);
        }
        for (u, v) in self.edges() {
            g.add_edge(v, u);
        }
        g
    }

    /// nx.descendants(G, source): nodes reachable from source, EXCLUDING source.
    pub fn descendants(&self, source: i64) -> IndexSet<i64> {
        let mut seen = IndexSet::new();
        let mut stack = vec![source];
        let mut visited = IndexSet::new();
        visited.insert(source);
        while let Some(n) = stack.pop() {
            for v in self.successors(n) {
                if visited.insert(v) {
                    seen.insert(v);
                    stack.push(v);
                }
            }
        }
        seen
    }

    /// nx.dfs_preorder_nodes(G, source): DFS preorder following adjacency order.
    /// Matches the DiGraph's stack-based dfs_edges (visits first unvisited child).
    pub fn dfs_preorder(&self, source: i64) -> Vec<i64> {
        let mut order = Vec::new();
        if !self.contains(source) {
            return order;
        }
        let mut visited = IndexSet::new();
        visited.insert(source);
        order.push(source);
        // stack of (node, child-cursor into its successor list)
        let succ_of: Vec<(i64, Vec<i64>)> = Vec::new();
        let _ = succ_of;
        let mut stack: Vec<(i64, Vec<i64>, usize)> = vec![(source, self.successors(source), 0)];
        while let Some((_node, children, idx)) = stack.last_mut() {
            let mut advanced = false;
            while *idx < children.len() {
                let child = children[*idx];
                *idx += 1;
                if visited.insert(child) {
                    order.push(child);
                    let cs = self.successors(child);
                    stack.push((child, cs, 0));
                    advanced = true;
                    break;
                }
            }
            if !advanced {
                stack.pop();
            }
        }
        order
    }

    /// nx.weakly_connected_components(G): components as node sets. Component
    /// iteration order follows the node insertion order of their first-seen node.
    pub fn weakly_connected_components(&self) -> Vec<IndexSet<i64>> {
        let mut seen = IndexSet::new();
        let mut comps = Vec::new();
        for &start in &self.nodes {
            if seen.contains(&start) {
                continue;
            }
            let mut comp = IndexSet::new();
            let mut stack = vec![start];
            seen.insert(start);
            comp.insert(start);
            while let Some(n) = stack.pop() {
                for v in self.successors(n).into_iter().chain(self.predecessors(n)) {
                    if seen.insert(v) {
                        comp.insert(v);
                        stack.push(v);
                    }
                }
            }
            comps.push(comp);
        }
        comps
    }

    /// nx.immediate_dominators(G, entry). Result is the unique idom tree, so it
    /// matches the DiGraph regardless of algorithm (Cooper-Harvey-Kennedy here).
    /// Only nodes reachable from `entry` are included; idom[entry] = entry.
    pub fn immediate_dominators(&self, entry: i64) -> IndexMap<i64, i64> {
        // Reverse-postorder from entry.
        let mut post = Vec::new();
        let mut visited = IndexSet::new();
        self.dfs_postorder(entry, &mut visited, &mut post);
        let rpo: Vec<i64> = post.iter().rev().copied().collect();
        let mut rpo_num: IndexMap<i64, usize> = IndexMap::new();
        for (i, &n) in rpo.iter().enumerate() {
            rpo_num.insert(n, i);
        }

        let mut idom: IndexMap<i64, i64> = IndexMap::new();
        idom.insert(entry, entry);
        let intersect = |idom: &IndexMap<i64, i64>,
                         rpo_num: &IndexMap<i64, usize>,
                         mut a: i64,
                         mut b: i64|
         -> i64 {
            while a != b {
                while rpo_num[&a] > rpo_num[&b] {
                    a = idom[&a];
                }
                while rpo_num[&b] > rpo_num[&a] {
                    b = idom[&b];
                }
            }
            a
        };
        let mut changed = true;
        while changed {
            changed = false;
            for &n in &rpo {
                if n == entry {
                    continue;
                }
                let mut new_idom: Option<i64> = None;
                for p in self.predecessors(n) {
                    if !rpo_num.contains_key(&p) {
                        continue; // unreachable predecessor
                    }
                    if idom.contains_key(&p) {
                        new_idom = Some(match new_idom {
                            None => p,
                            Some(cur) => intersect(&idom, &rpo_num, p, cur),
                        });
                    }
                }
                if let Some(ni) = new_idom {
                    if idom.get(&n) != Some(&ni) {
                        idom.insert(n, ni);
                        changed = true;
                    }
                }
            }
        }
        idom
    }

    fn dfs_postorder(&self, n: i64, visited: &mut IndexSet<i64>, out: &mut Vec<i64>) {
        if !visited.insert(n) {
            return;
        }
        for v in self.successors(n) {
            self.dfs_postorder(v, visited, out);
        }
        out.push(n);
    }
}
