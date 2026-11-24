use std::cmp::Reverse;
use std::collections::BinaryHeap;

use fixedbitset::FixedBitSet;

use crate::Oid;
use crate::graph::collapse::{CollapseInput, CollapseMode};
use crate::graph::linear_bek::{FoldedBekMerge, LinearBekState, apply_linear_bek};
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
    /// Linear-branch collapsing (`CollapsedGraph`); allowed unfiltered or when
    /// only `BranchFilter` is active (ignored under other filters or `first_parent_only`).
    pub collapse: CollapseMode,
    /// `LinearBek` view mode: fold merge edges into dotted edges from merged
    /// branch tails to the merge's deeper parent.
    pub linear_bek: bool,
}

impl Default for GraphViewOptions {
    fn default() -> Self {
        Self {
            sort_type: SortType::IntelliSort,
            first_parent_only: false,
            show_long_edges: false,
            collapse: CollapseMode::None,
            linear_bek: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GraphEdgeType {
    Normal,
    /// Filter compression edge (hidden non-matching commits in between).
    Dotted,
    /// Collapsed linear segment (see [`crate::graph::collapse`]).
    Collapsed,
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

/// Optional contextual inputs for [`VisibleGraph::build_projected`].
#[derive(Debug, Clone, Copy, Default)]
pub struct VisibleGraphBuildArgs<'a> {
    pub filter: Option<&'a FixedBitSet>,
    /// Explicit starting heads from `BranchFilter` (used to scope `first_parent_only`
    /// walks so unrelated side-branch tips/tags do not leak into the projection).
    pub branch_heads: Option<&'a [PermNodeId]>,
    /// True when `filter` originates exclusively from `BranchFilter` (no text,
    /// user, date, path, range, or `--no-merges` predicates), allowing linear
    /// segment collapsing to remain active.
    pub branch_only_filter: bool,
    pub collapse: Option<CollapseInput<'a>>,
    pub linear_bek: Option<&'a LinearBekState>,
}

/// Complete visible graph state ready for `EdgesInRowGenerator` and `PrintElementGenerator`.
#[derive(Debug, Clone)]
pub struct VisibleGraph {
    pub linear: VisibleLinearGraph,
    pub options: GraphViewOptions,
    /// Whether linear-segment collapsing was applied to this projection.
    pub collapse_applied: bool,
    /// Whether linear-segment collapsing is permitted for the active filter/view mode.
    pub can_collapse: bool,
    /// Merges folded by `LinearBek` in this projection (for hit-testing & expand).
    pub bek_merges: Vec<FoldedBekMerge>,
}

impl VisibleGraph {
    /// Builds a visible projection from `permanent` using `options` and an optional
    /// matching-node filter bitset.
    pub fn build(
        permanent: &PermanentGraph,
        options: GraphViewOptions,
        filter: Option<&FixedBitSet>,
    ) -> Self {
        Self::build_collapsed(permanent, options, filter, None)
    }

    /// [`Self::build`] plus linear-segment collapsing. Collapsing is skipped
    /// (`collapse_applied == false`) while a non-branch filter or `first_parent_only`
    /// is active.
    pub fn build_collapsed(
        permanent: &PermanentGraph,
        options: GraphViewOptions,
        filter: Option<&FixedBitSet>,
        collapse: Option<CollapseInput<'_>>,
    ) -> Self {
        Self::build_projected(
            permanent,
            options,
            VisibleGraphBuildArgs {
                filter,
                branch_heads: None,
                branch_only_filter: false,
                collapse,
                linear_bek: None,
            },
        )
    }

    /// Full projection entry point supporting branch-scoped `FirstParentOnly`,
    /// branch-filter-compatible linear collapsing, and `LinearBek`.
    pub fn build_projected(
        permanent: &PermanentGraph,
        options: GraphViewOptions,
        args: VisibleGraphBuildArgs<'_>,
    ) -> Self {
        let can_collapse =
            !options.first_parent_only && (args.filter.is_none() || args.branch_only_filter);
        let collapse = args.collapse.filter(|c| {
            can_collapse && c.state.is_active() && !c.segments.is_empty()
        });
        let flags = collapse.map(|c| {
            let mut f = c.state.collapsed_flags(c.segments);
            if let Some(filter) = args.filter {
                // Only collapse a segment if both endpoints and every middle node are in the branch filter.
                for (idx, seg) in c.segments.iter().enumerate() {
                    if f[idx]
                        && (!filter.contains(seg.up as usize)
                            || !filter.contains(seg.down as usize)
                            || !c
                                .segments
                                .middle_nodes(&permanent.linear, seg)
                                .all(|n| filter.contains(n as usize)))
                    {
                        f[idx] = false;
                    }
                }
            }
            f
        });
        let linear = build_visible_linear(
            permanent,
            options,
            args.filter,
            args.branch_heads,
            collapse,
            flags.as_deref(),
        );
        let (linear, bek_merges) = if options.linear_bek && !options.first_parent_only {
            let default_bek = LinearBekState::default();
            apply_linear_bek(permanent, linear, args.linear_bek.unwrap_or(&default_bek))
        } else {
            (linear, Vec::new())
        };
        Self {
            linear,
            options,
            collapse_applied: collapse.is_some(),
            can_collapse,
            bek_merges,
        }
    }

    #[inline]
    pub fn layout_index(&self, permanent: &PermanentGraph, row: VisibleRowIdx) -> LayoutIndex {
        permanent.layout_index(self.linear.perm_id(row))
    }

    /// Returns the folded `LinearBek` merge OID owning dotted edge `(up_row, down_row)`.
    pub fn bek_merge_at_edge(&self, up_row: usize, down_row: usize) -> Option<Oid> {
        let (Ok(u), Ok(d)) = (
            VisibleRowIdx::try_from(up_row),
            VisibleRowIdx::try_from(down_row),
        ) else {
            return None;
        };
        self.bek_merges
            .iter()
            .find(|m| m.dotted_edges.contains(&(u, d)))
            .map(|m| m.merge_oid)
    }

    /// Returns the folded `LinearBek` merge OID whose merge row or tail row is `row`.
    pub fn bek_merge_at_row(&self, row: usize) -> Option<Oid> {
        let Ok(r) = VisibleRowIdx::try_from(row) else {
            return None;
        };
        self.bek_merges
            .iter()
            .find(|m| m.merge_row == r || m.dotted_edges.iter().any(|&(u, _)| u == r))
            .map(|m| m.merge_oid)
    }
}

fn build_visible_linear(
    permanent: &PermanentGraph,
    options: GraphViewOptions,
    filter: Option<&FixedBitSet>,
    branch_heads: Option<&[PermNodeId]>,
    collapse: Option<CollapseInput<'_>>,
    collapsed_flags: Option<&[bool]>,
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
        Some(compute_first_parent_mask(permanent, branch_heads))
    } else {
        None
    };

    // Segment index of a node hidden by a collapsed segment.
    let collapsed_segment = |node: usize| -> Option<usize> {
        let c = collapse?;
        let idx = c.segments.segment_index_of(node as PermNodeId)?;
        collapsed_flags
            .and_then(|f| f.get(idx).copied())
            .unwrap_or(false)
            .then_some(idx)
    };

    let is_visible = |node: usize| -> bool {
        let in_filter = filter.is_none_or(|f| f.contains(node));
        let in_fp = fp_mask.as_ref().is_none_or(|fp| fp.contains(node));
        in_filter && in_fp && collapsed_segment(node).is_none()
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

    if let (false, Some(c)) = (all_visible, collapse) {
        // Collapsed segments (either unfiltered or pure branch filter):
        // a hidden parent that belongs to a collapsed segment jumps straight to `seg.down`.
        // Parents excluded by a branch range cutoff (not in `filter`) are simply omitted.
        for &u in &visible_perms {
            let edges_u = &mut perm_edges[u as usize];
            for p in pg.loaded_down_nodes(u) {
                let edge = match collapsed_segment(p as usize) {
                    Some(idx) => {
                        let seg = c.segments.get(idx).expect("segment index in range");
                        Some((seg.down, GraphEdgeType::Collapsed))
                    }
                    None if is_vis_vec[p as usize] => Some((p, GraphEdgeType::Normal)),
                    None => None,
                };
                if let Some(edge) = edge
                    && !edges_u.contains(&edge)
                {
                    edges_u.push(edge);
                }
            }
        }
    } else if all_visible {
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

fn compute_first_parent_mask(
    permanent: &PermanentGraph,
    branch_heads: Option<&[PermNodeId]>,
) -> FixedBitSet {
    let pg = &permanent.linear;
    let total = pg.nodes_count as usize;
    let mut mask = FixedBitSet::with_capacity(total);

    let start_heads = branch_heads.unwrap_or(&permanent.sorted_heads);
    for &head in start_heads {
        if (head as usize) >= total {
            continue;
        }
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

    #[test]
    fn first_parent_with_branch_filter_does_not_leak_side_branch_tip_or_tag() {
        // 0 (main, merge [1, 2]) -> 1 -> 3; side branch 2 (also tagged/branched as `feat`) -> 3.
        // Without branch_heads scoping, `permanent.sorted_heads` contains both 0 and 2,
        // leaking side-branch tip 2 into `Branch=main + FirstParentOnly`.
        let commits = vec![
            raw(0, &[1, 2], 50),
            raw(1, &[3], 40),
            raw(2, &[3], 35),
            raw(3, &[], 10),
        ];
        let refs = vec![branch_ref("main", 0), branch_ref("feat", 2)];
        let pg = PermanentGraph::build(&commits, &refs, Some(oid(0)));
        let main_node = pg.linear.node_of_oid(&oid(0)).unwrap();
        let main_mask = ReachableNodes::contained_in_branch(&pg.linear, &[main_node]);

        let vis = VisibleGraph::build_projected(
            &pg,
            GraphViewOptions {
                first_parent_only: true,
                ..Default::default()
            },
            VisibleGraphBuildArgs {
                filter: Some(&main_mask),
                branch_heads: Some(&[main_node]),
                branch_only_filter: true,
                collapse: None,
                linear_bek: None,
            },
        );
        let visible_oids: Vec<Oid> = (0..vis.linear.visible_count())
            .map(|r| pg.linear.commit(vis.linear.perm_id(r)).oid)
            .collect();
        assert_eq!(visible_oids, vec![oid(0), oid(1), oid(3)]);
    }

    #[test]
    fn linear_bek_folds_merge_into_dotted_edge_and_restores_on_expand() {
        // 0 (main merge [1, 2]) -> 1 (main parent, ts 40) -> 4 (base, ts 10)
        //                       -> 2 (feat tip, ts 45) -> 3 (feat base, ts 42) -> 4
        // In IntelliSort order: 0 (ts 50), 2 (ts 45), 3 (ts 42), 1 (ts 40), 4 (ts 10).
        // Merge 0 has right_child = 2 (row 1) and left_child = 1 (row 3).
        // LinearBek folds (0 -> 1) and replaces (3 -> 4) with dotted (3 -> 1).
        let commits = vec![
            raw(0, &[1, 2], 50),
            raw(2, &[3], 45),
            raw(3, &[4], 42),
            raw(1, &[4], 40),
            raw(4, &[], 10),
        ];
        let refs = vec![branch_ref("main", 0)];
        let pg = PermanentGraph::build(&commits, &refs, Some(oid(0)));

        let bek_state = LinearBekState::default();
        let vis = VisibleGraph::build_projected(
            &pg,
            GraphViewOptions {
                sort_type: SortType::IntelliSort,
                linear_bek: true,
                ..Default::default()
            },
            VisibleGraphBuildArgs {
                linear_bek: Some(&bek_state),
                ..Default::default()
            },
        );
        assert_eq!(vis.bek_merges.len(), 1);
        assert_eq!(vis.bek_merges[0].merge_oid, oid(0));
        // Merge row 0 now has only a single down edge (to feat tip row 1), no direct edge to row 3.
        assert_eq!(vis.linear.down_edges(0).len(), 1);
        assert_eq!(vis.linear.down_edges(0)[0].down, 1);
        // Tail row 2 (commit 3) now has a Dotted edge to left_child row 3 (commit 1).
        assert_eq!(
            vis.linear.down_edges(2),
            &[GraphEdge {
                up: 2,
                down: 3,
                edge_type: GraphEdgeType::Dotted,
            }]
        );
        assert_eq!(vis.bek_merge_at_edge(2, 3), Some(oid(0)));

        // Expanding oid(0) restores the original 2-parent merge edges.
        let mut expanded = LinearBekState::default();
        expanded.expand(oid(0));
        let vis_restored = VisibleGraph::build_projected(
            &pg,
            GraphViewOptions {
                sort_type: SortType::IntelliSort,
                linear_bek: true,
                ..Default::default()
            },
            VisibleGraphBuildArgs {
                linear_bek: Some(&expanded),
                ..Default::default()
            },
        );
        assert!(vis_restored.bek_merges.is_empty());
        assert_eq!(vis_restored.linear.down_edges(0).len(), 2);
    }

    #[test]
    fn collapse_remains_active_under_branch_only_filter() {
        use crate::graph::collapse::{CollapseState, LinearSegments};
        // main: 0 -> 1 -> 2 -> 3 -> 4 (segment [1,2,3] collapsible between 0 and 4)
        // side: 5 -> 4
        let commits = vec![
            raw(0, &[1], 60),
            raw(1, &[2], 50),
            raw(2, &[3], 40),
            raw(3, &[4], 30),
            raw(5, &[4], 25),
            raw(4, &[], 10),
        ];
        let refs = vec![branch_ref("main", 0), branch_ref("side", 5)];
        let pg = PermanentGraph::build(&commits, &refs, Some(oid(0)));
        let pinned = LinearSegments::pinned_nodes(&pg.linear, [oid(0), oid(5)], Some(oid(0)));
        let segments = LinearSegments::compute(&pg.linear, &pinned);
        assert_eq!(segments.len(), 1);

        let mut collapse = CollapseState::default();
        collapse.collapse_all();
        let main_node = pg.linear.node_of_oid(&oid(0)).unwrap();
        let main_mask = ReachableNodes::contained_in_branch(&pg.linear, &[main_node]);

        let vis = VisibleGraph::build_projected(
            &pg,
            GraphViewOptions {
                collapse: collapse.mode,
                ..Default::default()
            },
            VisibleGraphBuildArgs {
                filter: Some(&main_mask),
                branch_heads: Some(&[main_node]),
                branch_only_filter: true,
                collapse: Some(CollapseInput {
                    segments: &segments,
                    state: &collapse,
                }),
                linear_bek: None,
            },
        );
        assert!(vis.collapse_applied);
        assert!(vis.can_collapse);
        // Only 0 and 4 remain visible on `main`, linked by a Collapsed edge.
        let oids: Vec<Oid> = (0..vis.linear.visible_count())
            .map(|r| pg.linear.commit(vis.linear.perm_id(r)).oid)
            .collect();
        assert_eq!(oids, vec![oid(0), oid(4)]);
        assert_eq!(vis.linear.down_edges(0)[0].edge_type, GraphEdgeType::Collapsed);
    }
}
