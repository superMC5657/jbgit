//! LinearBek graph projection (`LinearBekGraphBuilder` / `LinearBekController`
//! in IDEA's `vcs-log`).
//!
//! Folds 2-parent merge fragments into dotted edges from the merged branch's
//! tail(s) to the merge's deeper parent (`left_child`), removing the spanning
//! merge edge `(merge, left_child)` so parallel merge tracks collapse into a
//! linear vertical column. Individual folded merges can be expanded back by
//! inserting their commit OID into [`LinearBekState::expanded`].

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, HashSet};

use crate::Oid;
use crate::graph::permanent::{PermanentGraph, VisibleRowIdx};
use crate::graph::visible::{GraphEdge, GraphEdgeType, VisibleLinearGraph};

/// Maximum number of commits inside a single foldable merge fragment (matches IDEA).
pub const LINEAR_BEK_MAX_BLOCK_SIZE: usize = 20;
/// Maximum descendant search distance for the magic-set ancestor check (matches IDEA).
pub const LINEAR_BEK_MAGIC_SET_SIZE: usize = 10;

/// User-controlled expansion state for `LinearBek` mode.
///
/// While [`crate::graph::GraphViewOptions::linear_bek`] is enabled, every
/// eligible merge fragment is folded unless its merge commit OID is listed in
/// [`Self::expanded`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LinearBekState {
    /// Merge commit OIDs explicitly expanded by the user.
    pub expanded: HashSet<Oid>,
}

impl LinearBekState {
    pub fn is_expanded(&self, oid: &Oid) -> bool {
        self.expanded.contains(oid)
    }

    pub fn expand(&mut self, oid: Oid) -> bool {
        self.expanded.insert(oid)
    }

    pub fn collapse(&mut self, oid: &Oid) -> bool {
        self.expanded.remove(oid)
    }

    pub fn collapse_all(&mut self) {
        self.expanded.clear();
    }

    pub fn expand_all(&mut self, merges: &[FoldedBekMerge]) {
        for m in merges {
            self.expanded.insert(m.merge_oid);
        }
    }
}

/// Metadata recorded for each merge folded by [`apply_linear_bek`], enabling
/// click/Enter hit-testing on dotted edges or merge rows to expand that merge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FoldedBekMerge {
    pub merge_oid: Oid,
    pub merge_row: VisibleRowIdx,
    pub right_child_row: VisibleRowIdx,
    pub left_child_row: VisibleRowIdx,
    pub dotted_edges: Vec<(VisibleRowIdx, VisibleRowIdx)>,
}

struct MergeFragment {
    parent: VisibleRowIdx,
    left_child: VisibleRowIdx,
    right_child: VisibleRowIdx,
    merge_with_old_commit: bool,
    tail_edges: HashMap<VisibleRowIdx, Vec<VisibleRowIdx>>,
    body: HashSet<VisibleRowIdx>,
    tails: Vec<VisibleRowIdx>,
    tails_set: HashSet<VisibleRowIdx>,
}

impl MergeFragment {
    fn new(parent: VisibleRowIdx, left_child: VisibleRowIdx, right_child: VisibleRowIdx) -> Self {
        Self {
            parent,
            left_child,
            right_child,
            merge_with_old_commit: false,
            tail_edges: HashMap::new(),
            body: HashSet::new(),
            tails: Vec::new(),
            tails_set: HashSet::new(),
        }
    }

    fn add_tail(&mut self, tail: VisibleRowIdx) {
        if !self.body.contains(&tail) && self.tails_set.insert(tail) {
            self.tails.push(tail);
        }
    }

    fn add_tail_edge(&mut self, up: VisibleRowIdx, down: VisibleRowIdx) {
        if !self.body.contains(&up) {
            if self.tails_set.insert(up) {
                self.tails.push(up);
            }
            let entry = self.tail_edges.entry(up).or_default();
            if !entry.contains(&down) {
                entry.push(down);
            }
        }
    }

    fn add_body(&mut self, node: VisibleRowIdx) {
        self.body.insert(node);
    }

    fn has_tail_edge(&self, node: VisibleRowIdx) -> bool {
        self.tail_edges.get(&node).is_some_and(|v| !v.is_empty())
    }
}

/// Applies the `LinearBek` transformation to `linear`, folding eligible 2-parent
/// merge fragments whose merge commit is not in `state.expanded`.
///
/// Strictly preserves the topological invariant (`edge.up < edge.down`) for
/// every edge in the resulting [`VisibleLinearGraph`].
pub fn apply_linear_bek(
    permanent: &PermanentGraph,
    linear: VisibleLinearGraph,
    state: &LinearBekState,
) -> (VisibleLinearGraph, Vec<FoldedBekMerge>) {
    let m = linear.visible_count as usize;
    if m < 3 {
        return (linear, Vec::new());
    }

    let mut down_adj: Vec<Vec<(VisibleRowIdx, GraphEdgeType)>> = (0..m)
        .map(|row| {
            linear
                .down_edges(row as VisibleRowIdx)
                .iter()
                .map(|e| (e.down, e.edge_type))
                .collect()
        })
        .collect();

    let layout_of = |row: VisibleRowIdx| -> u32 {
        permanent.layout_index(linear.visible_to_perm[row as usize])
    };

    let mut folded = Vec::new();

    for merge_row_usize in 0..m {
        let merge_row = merge_row_usize as VisibleRowIdx;
        let merge_oid = permanent
            .linear
            .commit(linear.visible_to_perm[merge_row_usize])
            .oid;
        if state.is_expanded(&merge_oid) {
            continue;
        }

        let mut down_nodes: Vec<VisibleRowIdx> = down_adj[merge_row_usize]
            .iter()
            .map(|&(d, _)| d)
            .collect();
        down_nodes.sort_unstable();
        down_nodes.dedup();
        if down_nodes.len() != 2 {
            continue;
        }

        // Smaller visible row index is closer to mergeCommit (`rightChild`),
        // larger visible row index is deeper (`leftChild`).
        let right_child = down_nodes[0];
        let left_child = down_nodes[1];

        let Some(fragment) = find_fragment(
            merge_row,
            left_child,
            right_child,
            &down_adj,
            &layout_of,
        ) else {
            continue;
        };

        // Apply fragment collapse to mutable `down_adj`.
        for (&up, downs) in &fragment.tail_edges {
            down_adj[up as usize].retain(|&(d, _)| !downs.contains(&d));
        }

        let mut dotted_edges = Vec::new();
        for &tail in &fragment.tails {
            if tail >= fragment.left_child {
                continue;
            }
            let edges = &mut down_adj[tail as usize];
            if let Some(existing) = edges.iter_mut().find(|(d, _)| *d == fragment.left_child) {
                existing.1 = GraphEdgeType::Dotted;
            } else {
                edges.push((fragment.left_child, GraphEdgeType::Dotted));
            }
            dotted_edges.push((tail, fragment.left_child));
        }

        // Remove spanning merge edge `(merge_row, left_child)`.
        down_adj[fragment.parent as usize].retain(|&(d, _)| d != fragment.left_child);

        folded.push(FoldedBekMerge {
            merge_oid,
            merge_row: fragment.parent,
            right_child_row: fragment.right_child,
            left_child_row: fragment.left_child,
            dotted_edges,
        });
    }

    if folded.is_empty() {
        return (linear, folded);
    }

    // Rebuild CSR `down_edges` & `up_edges` from `down_adj`.
    let mut down_offsets = Vec::with_capacity(m + 1);
    let mut down_edges_data = Vec::with_capacity(m.saturating_mul(2));
    let mut up_counts = vec![0u32; m];
    down_offsets.push(0);

    for (row_usize, edges) in down_adj.into_iter().enumerate() {
        let up = row_usize as VisibleRowIdx;
        for (down, edge_type) in edges {
            debug_assert!(up < down, "LinearBek violated topo invariant: {up} >= {down}");
            up_counts[down as usize] += 1;
            down_edges_data.push(GraphEdge {
                up,
                down,
                edge_type,
            });
        }
        down_offsets.push(down_edges_data.len() as u32);
    }

    let mut up_offsets = Vec::with_capacity(m + 1);
    up_offsets.push(0);
    let mut running = 0u32;
    for &c in &up_counts {
        running += c;
        up_offsets.push(running);
    }
    let mut up_cursors = up_offsets[..m].to_vec();
    let mut up_edges_data = vec![
        GraphEdge {
            up: 0,
            down: 0,
            edge_type: GraphEdgeType::Normal,
        };
        running as usize
    ];
    for edge in &down_edges_data {
        let d = edge.down as usize;
        let slot = up_cursors[d] as usize;
        up_edges_data[slot] = *edge;
        up_cursors[d] += 1;
    }

    let rebuilt = VisibleLinearGraph {
        visible_count: linear.visible_count,
        visible_to_perm: linear.visible_to_perm,
        perm_to_visible: linear.perm_to_visible,
        down_offsets,
        down_edges_data,
        up_offsets,
        up_edges_data,
        unloaded_down: linear.unloaded_down,
    };
    (rebuilt, folded)
}

fn find_fragment(
    parent: VisibleRowIdx,
    left_child: VisibleRowIdx,
    right_child: VisibleRowIdx,
    down_adj: &[Vec<(VisibleRowIdx, GraphEdgeType)>],
    layout_of: &impl Fn(VisibleRowIdx) -> u32,
) -> Option<MergeFragment> {
    let mut fragment = MergeFragment::new(parent, left_child, right_child);
    let left_li = layout_of(left_child);
    let right_li = layout_of(right_child);
    let mut rows_count: u32 = 1;
    let mut block_size: usize = 1;

    // Min-heap ordered by `(down_row, up_row)`.
    let mut queue: BinaryHeap<Reverse<(VisibleRowIdx, VisibleRowIdx)>> =
        BinaryHeap::with_capacity(LINEAR_BEK_MAX_BLOCK_SIZE);
    for &(down, _) in &down_adj[right_child as usize] {
        queue.push(Reverse((down, right_child)));
    }
    // Root commit directly merged (right_child has no down edges): fold into tail.
    if queue.is_empty() {
        fragment.add_tail(right_child);
        return Some(fragment);
    }

    let mut magic_set: Option<HashSet<VisibleRowIdx>> = None;

    while let Some(Reverse((next, up_node))) = queue.pop() {
        if next == left_child {
            fragment.add_tail(up_node);
            fragment.merge_with_old_commit = true;
        } else if next == right_child + rows_count {
            rows_count += 1;
            block_size += 1;
            for &(d, _) in &down_adj[next as usize] {
                queue.push(Reverse((d, next)));
            }
            fragment.add_body(up_node);
        } else if next > right_child + rows_count && next < left_child {
            rows_count = next - right_child + 1;
            block_size += 1;
            for &(d, _) in &down_adj[next as usize] {
                queue.push(Reverse((d, next)));
            }
            fragment.add_body(up_node);
        } else if next > left_child {
            let li = layout_of(next);
            if left_li > right_li && !fragment.merge_with_old_commit {
                if next > left_child + LINEAR_BEK_MAGIC_SET_SIZE as u32 {
                    return None;
                }
                let ms = magic_set
                    .get_or_insert_with(|| calculate_magic_set(left_child, down_adj));
                if ms.contains(&next) {
                    fragment.add_tail_edge(up_node, next);
                } else {
                    return None;
                }
            } else if (li > left_li && li < right_li) || li == left_li {
                fragment.add_tail_edge(up_node, next);
            } else if li >= right_li {
                return None;
            } else if next > left_child + LINEAR_BEK_MAGIC_SET_SIZE as u32 {
                if !fragment.has_tail_edge(up_node) && !fragment.body.contains(&up_node) {
                    return None;
                }
            } else {
                let ms = magic_set
                    .get_or_insert_with(|| calculate_magic_set(left_child, down_adj));
                if ms.contains(&next) {
                    fragment.add_tail_edge(up_node, next);
                } else {
                    return None;
                }
            }
        }

        if block_size >= LINEAR_BEK_MAX_BLOCK_SIZE {
            return None;
        }
    }

    if fragment.tails.is_empty() {
        return None;
    }
    Some(fragment)
}

fn calculate_magic_set(
    node: VisibleRowIdx,
    down_adj: &[Vec<(VisibleRowIdx, GraphEdgeType)>],
) -> HashSet<VisibleRowIdx> {
    let mut magic = HashSet::with_capacity(LINEAR_BEK_MAGIC_SET_SIZE);
    let mut queue: BinaryHeap<Reverse<VisibleRowIdx>> =
        BinaryHeap::with_capacity(LINEAR_BEK_MAGIC_SET_SIZE);
    for &(d, _) in &down_adj[node as usize] {
        queue.push(Reverse(d));
    }
    let limit = node.saturating_add(LINEAR_BEK_MAGIC_SET_SIZE as u32);
    while let Some(Reverse(i)) = queue.pop() {
        if i > limit {
            break;
        }
        if magic.insert(i) {
            for &(d, _) in &down_adj[i as usize] {
                queue.push(Reverse(d));
            }
        }
    }
    magic
}
