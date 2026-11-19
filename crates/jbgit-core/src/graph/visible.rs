use std::cmp::Reverse;
use std::collections::BinaryHeap;

use fixedbitset::FixedBitSet;

use crate::Oid;
use crate::graph::permanent::{LayoutIndex, PermNodeId, PermanentGraph, VisibleRowIdx};

/// Row ordering strategy for [`VisibleGraph`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SortType {
    /// Standard reverse topological order (`Normal` in IDEA).
    Topo,
    /// Interleaves concurrent branches by timestamp while strictly preserving
    /// topological parent-below-child constraints (`BekSorter` / IntelliSort).
    #[default]
    IntelliSort,
}

/// View configuration controlling projection and rendering of a [`VisibleGraph`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GraphViewOptions {
    pub sort_type: SortType,
    pub first_parent_only: bool,
    pub show_long_edges: bool,
}

impl Default for GraphViewOptions {
    fn default() -> Self {
        Self {
            sort_type: SortType::IntelliSort,
            first_parent_only: false,
            show_long_edges: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GraphEdgeType {
    Normal,
    Dotted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GraphEdge {
    pub up: VisibleRowIdx,
    pub down: VisibleRowIdx,
    pub edge_type: GraphEdgeType,
}

/// Projected visible commit graph with CSR adjacency and bidirectional row/node mappings.
#[derive(Debug, Clone)]
pub struct VisibleLinearGraph {
    pub visible_count: u32,
    pub visible_to_perm: Vec<PermNodeId>,
    pub perm_to_visible: Vec<Option<VisibleRowIdx>>,
    pub down_offsets: Vec<u32>,
    pub down_edges_data: Vec<GraphEdge>,
    pub up_offsets: Vec<u32>,
    pub up_edges_data: Vec<GraphEdge>,
    pub unloaded_down: Vec<Option<Oid>>,
}

impl VisibleLinearGraph {
    #[inline]
    pub fn visible_count(&self) -> u32 {
        self.visible_count
    }

    #[inline]
    pub fn perm_id(&self, row: VisibleRowIdx) -> PermNodeId {
        self.visible_to_perm[row as usize]
    }

    #[inline]
    pub fn visible_row(&self, perm_id: PermNodeId) -> Option<VisibleRowIdx> {
        self.perm_to_visible
            .get(perm_id as usize)
            .copied()
            .flatten()
    }

    #[inline]
    pub fn down_edges(&self, row: VisibleRowIdx) -> &[GraphEdge] {
        let idx = row as usize;
        let start = self.down_offsets[idx] as usize;
        let end = self.down_offsets[idx + 1] as usize;
        &self.down_edges_data[start..end]
    }

    #[inline]
    pub fn up_edges(&self, row: VisibleRowIdx) -> &[GraphEdge] {
        let idx = row as usize;
        let start = self.up_offsets[idx] as usize;
        let end = self.up_offsets[idx + 1] as usize;
        &self.up_edges_data[start..end]
    }
}

/// Complete visible graph state ready for `EdgesInRowGenerator` and `PrintElementGenerator`.
#[derive(Debug, Clone)]
pub struct VisibleGraph {
    pub linear: VisibleLinearGraph,
    pub options: GraphViewOptions,
}

impl VisibleGraph {
    /// Builds a visible projection from `permanent` using `options` and an optional
    /// matching-node filter bitset.
    pub fn build(
        permanent: &PermanentGraph,
        options: GraphViewOptions,
        filter: Option<&FixedBitSet>,
    ) -> Self {
        let linear = build_visible_linear(permanent, options, filter);
        Self { linear, options }
    }

    #[inline]
    pub fn layout_index(&self, permanent: &PermanentGraph, row: VisibleRowIdx) -> LayoutIndex {
        permanent.layout_index(self.linear.perm_id(row))
    }
}

fn build_visible_linear(
    permanent: &PermanentGraph,
    options: GraphViewOptions,
    filter: Option<&FixedBitSet>,
) -> VisibleLinearGraph {
    let pg = &permanent.linear;
    let total = pg.nodes_count as usize;
    if total == 0 {
        return VisibleLinearGraph {
            visible_count: 0,
            visible_to_perm: Vec::new(),
            perm_to_visible: Vec::new(),
            down_offsets: vec![0],
            down_edges_data: Vec::new(),
            up_offsets: vec![0],
            up_edges_data: Vec::new(),
            unloaded_down: Vec::new(),
        };
    }

    // 1. Determine visible node mask (combining first_parent_only and filter if present).
    let fp_mask = if options.first_parent_only {
        Some(compute_first_parent_mask(permanent))
    } else {
        None
    };

    let is_visible = |node: usize| -> bool {
        let in_filter = filter.is_none_or(|f| f.contains(node));
        let in_fp = fp_mask.as_ref().is_none_or(|fp| fp.contains(node));
        in_filter && in_fp
    };

    let mut visible_perms: Vec<PermNodeId> = Vec::with_capacity(total);
    let mut is_vis_vec = vec![false; total];
    for (node, vis_slot) in is_vis_vec.iter_mut().enumerate() {
        if is_visible(node) {
            *vis_slot = true;
            visible_perms.push(node as PermNodeId);
        }
    }

    let m = visible_perms.len();
    if m == 0 {
        return VisibleLinearGraph {
            visible_count: 0,
            visible_to_perm: Vec::new(),
            perm_to_visible: vec![None; total],
            down_offsets: vec![0],
            down_edges_data: Vec::new(),
            up_offsets: vec![0],
            up_edges_data: Vec::new(),
            unloaded_down: Vec::new(),
        };
    }

    // 2. Compute edges in PermNodeId space (handling compressed dotted edges when filtered).
    let all_visible = m == total && !options.first_parent_only;
    let mut perm_edges: Vec<Vec<(PermNodeId, GraphEdgeType)>> = vec![Vec::new(); total];

    if all_visible {
        for (u, edges_u) in perm_edges.iter_mut().enumerate() {
            for p in pg.loaded_down_nodes(u as PermNodeId) {
                edges_u.push((p, GraphEdgeType::Normal));
            }
        }
    } else if options.first_parent_only && filter.is_none() {
        for &u in &visible_perms {
            if let Some(p0) = pg.loaded_down_nodes(u).next()
                && is_vis_vec[p0 as usize]
            {
                perm_edges[u as usize].push((p0, GraphEdgeType::Normal));
            }
        }
    } else {
        // General FilteredController compression: bottom-up DP for nearest visible descendants.
        let mut nearest_down: Vec<Vec<PermNodeId>> = vec![Vec::new(); total];
        for node in (0..total).rev() {
            if is_vis_vec[node] {
                nearest_down[node].push(node as PermNodeId);
            } else {
                let mut merged: Vec<PermNodeId> = Vec::new();
                let mut parent_iter = pg.loaded_down_nodes(node as PermNodeId);
                if options.first_parent_only {
                    if let Some(p0) = parent_iter.next() {
                        merged.extend_from_slice(&nearest_down[p0 as usize]);
                    }
                } else {
                    for p in parent_iter {
                        merged.extend_from_slice(&nearest_down[p as usize]);
                    }
                }
                merged.sort_unstable();
                merged.dedup();
                merged.truncate(32);
                nearest_down[node] = merged;
            }
        }

        for &u in &visible_perms {
            let mut candidates: Vec<(PermNodeId, GraphEdgeType)> = Vec::new();
            let mut parent_iter = pg.loaded_down_nodes(u);
            let mut push_parent = |p: PermNodeId| {
                if is_vis_vec[p as usize] {
                    candidates.push((p, GraphEdgeType::Normal));
                } else {
                    for &v in &nearest_down[p as usize] {
                        candidates.push((v, GraphEdgeType::Dotted));
                    }
                }
            };
            if options.first_parent_only {
                if let Some(p0) = parent_iter.next() {
                    push_parent(p0);
                }
            } else {
                for p in parent_iter {
                    push_parent(p);
                }
            }
            // Deduplicate by target PermNodeId, preferring Normal over Dotted.
            candidates.sort_by_key(|&(target, kind)| {
                (
                    target,
                    if kind == GraphEdgeType::Normal {
                        0u8
                    } else {
                        1u8
                    },
                )
            });
            candidates.dedup_by_key(|e| e.0);
            perm_edges[u as usize] = candidates;
        }
    }

    // 3. Order visible nodes according to SortType.
    let visible_to_perm: Vec<PermNodeId> = match options.sort_type {
        SortType::Topo => visible_perms,
        SortType::IntelliSort => bek_sort(permanent, &visible_perms, &perm_edges),
    };

    let mut perm_to_visible = vec![None; total];
    for (row, &perm_id) in visible_to_perm.iter().enumerate() {
        perm_to_visible[perm_id as usize] = Some(row as VisibleRowIdx);
    }

    // 4. Build CSR down_edges & up_edges in VisibleRowIdx space.
    let mut down_offsets = Vec::with_capacity(m + 1);
    let mut down_edges_data = Vec::with_capacity(m.saturating_mul(2));
    let mut up_counts = vec![0u32; m];
    let mut unloaded_down = Vec::with_capacity(m);

    down_offsets.push(0);
    for (row, &perm_id) in visible_to_perm.iter().enumerate() {
        let up_row = row as VisibleRowIdx;
        let mut row_edges: Vec<GraphEdge> = perm_edges[perm_id as usize]
            .iter()
            .filter_map(|&(target_perm, edge_type)| {
                let down_row = perm_to_visible[target_perm as usize]?;
                debug_assert!(
                    up_row < down_row,
                    "topological invariant violated: {up_row} >= {down_row}"
                );
                Some(GraphEdge {
                    up: up_row,
                    down: down_row,
                    edge_type,
                })
            })
            .collect();

        // Keep down edges sorted by (down, edge_type) deterministically, preserving parent[0] if desired.
        for e in &row_edges {
            up_counts[e.down as usize] += 1;
        }
        down_edges_data.append(&mut row_edges);
        down_offsets.push(down_edges_data.len() as u32);

        let unloaded = pg
            .down_nodes(perm_id)
            .iter()
            .copied()
            .find_map(|t| pg.unloaded_oid(t));
        unloaded_down.push(unloaded);
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

    VisibleLinearGraph {
        visible_count: m as u32,
        visible_to_perm,
        perm_to_visible,
        down_offsets,
        down_edges_data,
        up_offsets,
        up_edges_data,
        unloaded_down,
    }
}

fn compute_first_parent_mask(permanent: &PermanentGraph) -> FixedBitSet {
    let pg = &permanent.linear;
    let total = pg.nodes_count as usize;
    let mut mask = FixedBitSet::with_capacity(total);

    for &head in &permanent.sorted_heads {
        let mut curr = head;
        while !mask.put(curr as usize) {
            if let Some(p0) = pg.loaded_down_nodes(curr).next() {
                curr = p0;
            } else {
                break;
            }
        }
    }
    mask
}

/// Topological timestamp-interleaving sort (`BekSorter` equivalent).
///
/// Uses Kahn's algorithm with a max-heap keyed by `(timestamp, Reverse(layout_index), Reverse(perm_id))`
/// so concurrent branches interleave chronologically while strictly guaranteeing `row(child) < row(parent)`.
fn bek_sort(
    permanent: &PermanentGraph,
    visible_perms: &[PermNodeId],
    perm_edges: &[Vec<(PermNodeId, GraphEdgeType)>],
) -> Vec<PermNodeId> {
    let total = permanent.linear.nodes_count as usize;
    let mut in_degree = vec![0u32; total];
    for &u in visible_perms {
        for &(v, _) in &perm_edges[u as usize] {
            in_degree[v as usize] += 1;
        }
    }

    #[derive(Copy, Clone, Eq, PartialEq)]
    struct HeapItem {
        timestamp: i64,
        layout_index: Reverse<LayoutIndex>,
        perm_id: Reverse<PermNodeId>,
    }

    impl Ord for HeapItem {
        fn cmp(&self, other: &Self) -> std::cmp::Ordering {
            self.timestamp
                .cmp(&other.timestamp)
                .then_with(|| self.layout_index.cmp(&other.layout_index))
                .then_with(|| self.perm_id.cmp(&other.perm_id))
        }
    }

    impl PartialOrd for HeapItem {
        fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
            Some(self.cmp(other))
        }
    }

    let mut heap = BinaryHeap::with_capacity(visible_perms.len().min(1024));
    for &u in visible_perms {
        if in_degree[u as usize] == 0 {
            heap.push(HeapItem {
                timestamp: permanent.linear.commit(u).timestamp,
                layout_index: Reverse(permanent.layout_index(u)),
                perm_id: Reverse(u),
            });
        }
    }

    let mut ordered = Vec::with_capacity(visible_perms.len());
    while let Some(item) = heap.pop() {
        let u = item.perm_id.0;
        ordered.push(u);
        for &(v, _) in &perm_edges[u as usize] {
            let deg = &mut in_degree[v as usize];
            *deg -= 1;
            if *deg == 0 {
                heap.push(HeapItem {
                    timestamp: permanent.linear.commit(v).timestamp,
                    layout_index: Reverse(permanent.layout_index(v)),
                    perm_id: Reverse(v),
                });
            }
        }
    }

    ordered
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RefInfo;
    use crate::RefKind;
    use crate::graph::permanent::{RawCommitNode, ReachableNodes};

    fn oid(n: u64) -> Oid {
        let mut bytes = [0u8; 20];
        bytes[..8].copy_from_slice(&n.to_le_bytes());
        Oid::from_bytes(bytes)
    }

    fn raw(id: u64, parents: &[u64], ts: i64) -> RawCommitNode {
        RawCommitNode {
            oid: oid(id),
            parents: parents.iter().copied().map(oid).collect(),
            timestamp: ts,
        }
    }

    fn branch_ref(name: &str, target: u64) -> RefInfo {
        RefInfo {
            name: name.to_string(),
            full_name: format!("refs/heads/{name}"),
            target_oid: oid(target),
            kind: RefKind::LocalBranch,
            upstream: None,
            ahead_behind: None,
            is_head: name == "main",
        }
    }

    #[test]
    fn bek_sort_interleaves_concurrent_branches_by_time_and_preserves_topo() {
        // main: 0 (ts 100) -> 1 (ts 50) -> 4 (ts 10)
        // feat: 2 (ts 90) -> 3 (ts 70) -> 4 (ts 10)
        // Input order places main chain before feat chain.
        let commits = vec![
            raw(0, &[1], 100),
            raw(1, &[4], 50),
            raw(2, &[3], 90),
            raw(3, &[4], 70),
            raw(4, &[], 10),
        ];
        let refs = vec![branch_ref("main", 0), branch_ref("feat", 2)];
        let pg = PermanentGraph::build(&commits, &refs, Some(oid(0)));

        let vis = VisibleGraph::build(
            &pg,
            GraphViewOptions {
                sort_type: SortType::IntelliSort,
                ..Default::default()
            },
            None,
        );
        let oids: Vec<Oid> = (0..vis.linear.visible_count())
            .map(|r| pg.linear.commit(vis.linear.perm_id(r)).oid)
            .collect();
        assert_eq!(oids, vec![oid(0), oid(2), oid(3), oid(1), oid(4)]);

        for r in 0..vis.linear.visible_count() {
            for e in vis.linear.down_edges(r) {
                assert!(e.up < e.down);
            }
        }
    }

    #[test]
    fn filtered_controller_compresses_gaps_with_dotted_edges() {
        // 0 -> 1 -> 2 -> 3 -> 4, filter keeps {0, 3, 4}
        let commits = vec![
            raw(0, &[1], 50),
            raw(1, &[2], 40),
            raw(2, &[3], 30),
            raw(3, &[4], 20),
            raw(4, &[], 10),
        ];
        let refs = vec![branch_ref("main", 0)];
        let pg = PermanentGraph::build(&commits, &refs, Some(oid(0)));

        let mut filter = FixedBitSet::with_capacity(5);
        filter.insert(0);
        filter.insert(3);
        filter.insert(4);

        let vis = VisibleGraph::build(&pg, GraphViewOptions::default(), Some(&filter));
        assert_eq!(vis.linear.visible_count(), 3);
        assert_eq!(
            vis.linear.down_edges(0),
            &[GraphEdge {
                up: 0,
                down: 1,
                edge_type: GraphEdgeType::Dotted
            }]
        );
        assert_eq!(
            vis.linear.down_edges(1),
            &[GraphEdge {
                up: 1,
                down: 2,
                edge_type: GraphEdgeType::Normal
            }]
        );
        assert!(vis.linear.down_edges(2).is_empty());
    }

    #[test]
    fn first_parent_only_hides_merged_side_branch() {
        // 0 (merge [1, 2]) -> 1 -> 3, 2 -> 3
        let commits = vec![
            raw(0, &[1, 2], 40),
            raw(1, &[3], 30),
            raw(2, &[3], 20),
            raw(3, &[], 10),
        ];
        let refs = vec![branch_ref("main", 0)];
        let pg = PermanentGraph::build(&commits, &refs, Some(oid(0)));

        let vis = VisibleGraph::build(
            &pg,
            GraphViewOptions {
                first_parent_only: true,
                ..Default::default()
            },
            None,
        );
        let perm_ids: Vec<_> = (0..vis.linear.visible_count())
            .map(|r| vis.linear.perm_id(r))
            .collect();
        assert_eq!(perm_ids, vec![0, 1, 3]);
    }

    #[test]
    fn branch_filter_retains_reachable_subgraph() {
        let commits = vec![
            raw(0, &[1], 50),
            raw(1, &[3], 40),
            raw(2, &[3], 30),
            raw(3, &[], 10),
        ];
        let refs = vec![branch_ref("main", 0), branch_ref("feat", 2)];
        let pg = PermanentGraph::build(&commits, &refs, Some(oid(0)));
        let feat_mask = ReachableNodes::contained_in_branch(&pg.linear, &[2]);

        let vis = VisibleGraph::build(&pg, GraphViewOptions::default(), Some(&feat_mask));
        let perm_ids: Vec<_> = (0..vis.linear.visible_count())
            .map(|r| vis.linear.perm_id(r))
            .collect();
        assert_eq!(perm_ids, vec![2, 3]);
        assert_eq!(vis.linear.down_edges(0)[0].edge_type, GraphEdgeType::Normal);
    }

    #[test]
    fn first_parent_with_filter_preserves_matching_ancestors() {
        // 0 (main, merge [1, 2]) -> 1 -> 3 -> 4; side branch 2 -> 3
        // Filter keeps {3, 4} (hides head 0, intermediate 1, and side 2).
        // First-parent + filter must still retain {3, 4} and hide side branch {2}.
        let commits = vec![
            raw(0, &[1, 2], 50),
            raw(1, &[3], 40),
            raw(2, &[3], 35),
            raw(3, &[4], 20),
            raw(4, &[], 10),
        ];
        let refs = vec![branch_ref("main", 0)];
        let pg = PermanentGraph::build(&commits, &refs, Some(oid(0)));

        let mut filter = FixedBitSet::with_capacity(5);
        filter.insert(2);
        filter.insert(3);
        filter.insert(4);

        let vis = VisibleGraph::build(
            &pg,
            GraphViewOptions {
                first_parent_only: true,
                ..Default::default()
            },
            Some(&filter),
        );
        let perm_ids: Vec<_> = (0..vis.linear.visible_count())
            .map(|r| vis.linear.perm_id(r))
            .collect();
        assert_eq!(perm_ids, vec![3, 4]);
        assert_eq!(
            vis.linear.down_edges(0),
            &[GraphEdge {
                up: 0,
                down: 1,
                edge_type: GraphEdgeType::Normal,
            }]
        );

        // Also test keeping {0, 3, 4} with intermediate p0=1 filtered out -> Dotted edge 0 -> 3.
        let mut filter2 = FixedBitSet::with_capacity(5);
        filter2.insert(0);
        filter2.insert(2);
        filter2.insert(3);
        filter2.insert(4);

        let vis2 = VisibleGraph::build(
            &pg,
            GraphViewOptions {
                first_parent_only: true,
                ..Default::default()
            },
            Some(&filter2),
        );
        let perm_ids2: Vec<_> = (0..vis2.linear.visible_count())
            .map(|r| vis2.linear.perm_id(r))
            .collect();
        assert_eq!(perm_ids2, vec![0, 3, 4]);
        assert_eq!(
            vis2.linear.down_edges(0),
            &[GraphEdge {
                up: 0,
                down: 1,
                edge_type: GraphEdgeType::Dotted,
            }]
        );
    }
}
