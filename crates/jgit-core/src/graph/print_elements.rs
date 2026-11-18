use std::cmp::Ordering;

use crate::Oid;
use crate::graph::edges_in_row::EdgesInRowGenerator;
use crate::graph::permanent::{LayoutIndex, PermNodeId, PermanentGraph, VisibleRowIdx};
use crate::graph::visible::{GraphEdge, GraphEdgeType, VisibleGraph};
use crate::refs::BranchColorId;

pub const LONG_EDGE_SIZE: u32 = 30;
pub const VISIBLE_PART_SIZE: u32 = 1;
pub const VERY_LONG_EDGE_SIZE: u32 = 1000;
pub const VERY_LONG_EDGE_PART_SIZE: u32 = 250;
pub const RECOMMENDED_WIDTH_SAMPLE_SIZE: usize = 20_000;
pub const RECOMMENDED_WIDTH_K: f64 = 0.1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphElement {
    Node(VisibleRowIdx),
    Edge(GraphEdge),
}

/// Total-order projection key for [`GraphElement`] ensuring strict weak ordering (`Ord`)
/// while preserving non-crossing track positions across adjacent rows:
/// `(effective_layout_index, up_row, Reverse(down_row))`.
#[inline]
pub fn element_sort_key(
    el: &GraphElement,
    layout_idx_of: impl Fn(VisibleRowIdx) -> LayoutIndex,
) -> (LayoutIndex, VisibleRowIdx, std::cmp::Reverse<VisibleRowIdx>) {
    match *el {
        GraphElement::Node(r) => (layout_idx_of(r), r, std::cmp::Reverse(r)),
        GraphElement::Edge(e) => {
            let edge_li = layout_idx_of(e.up).max(layout_idx_of(e.down));
            (edge_li, e.up, std::cmp::Reverse(e.down))
        }
    }
}

/// Strict weak ordering comparator for elements within a row (`GraphElementComparatorByLayoutIndex`).
#[inline]
pub fn compare_edge_and_node(
    edge: GraphEdge,
    node_row: VisibleRowIdx,
    layout_idx_of: impl Fn(VisibleRowIdx) -> LayoutIndex,
) -> Ordering {
    let edge_li = layout_idx_of(edge.up).max(layout_idx_of(edge.down));
    let node_li = layout_idx_of(node_row);
    if edge_li != node_li {
        edge_li.cmp(&node_li)
    } else {
        edge.up.cmp(&node_row)
    }
}

#[inline]
pub fn compare_elements(
    a: &GraphElement,
    b: &GraphElement,
    layout_idx_of: impl Fn(VisibleRowIdx) -> LayoutIndex,
) -> Ordering {
    element_sort_key(a, &layout_idx_of).cmp(&element_sort_key(b, &layout_idx_of))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EdgeDirection {
    Up,
    Down,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LineStyle {
    Solid,
    Dashed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NodeKind {
    Normal,
    Head,
    Merge,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum GraphPrintElement {
    Node {
        row: VisibleRowIdx,
        perm_id: PermNodeId,
        pos: usize,
        color_id: BranchColorId,
        kind: NodeKind,
    },
    Edge {
        row: VisibleRowIdx,
        from_pos: usize,
        to_pos: usize,
        direction: EdgeDirection,
        style: LineStyle,
        up_row: VisibleRowIdx,
        down_row: VisibleRowIdx,
        color_id: BranchColorId,
    },
    TerminalEdge {
        row: VisibleRowIdx,
        pos: usize,
        direction: EdgeDirection,
        style: LineStyle,
        up_row: VisibleRowIdx,
        down_row: VisibleRowIdx,
        target_row: VisibleRowIdx,
        target_perm_id: PermNodeId,
        color_id: BranchColorId,
    },
    /// Dangling boundary indicator for a shallow/unloaded parent outside the loaded DAG.
    ShallowParent {
        row: VisibleRowIdx,
        pos: usize,
        unloaded_oid: Oid,
        color_id: BranchColorId,
    },
}

impl GraphPrintElement {
    pub fn row(&self) -> VisibleRowIdx {
        match *self {
            Self::Node { row, .. }
            | Self::Edge { row, .. }
            | Self::TerminalEdge { row, .. }
            | Self::ShallowParent { row, .. } => row,
        }
    }

    pub fn color_id(&self) -> BranchColorId {
        match *self {
            Self::Node { color_id, .. }
            | Self::Edge { color_id, .. }
            | Self::TerminalEdge { color_id, .. }
            | Self::ShallowParent { color_id, .. } => color_id,
        }
    }

    pub fn max_pos(&self) -> usize {
        match *self {
            Self::Node { pos, .. }
            | Self::TerminalEdge { pos, .. }
            | Self::ShallowParent { pos, .. } => pos,
            Self::Edge {
                from_pos, to_pos, ..
            } => from_pos.max(to_pos),
        }
    }
}

/// Presentation helper computing color ownership (`PrintElementPresentationManagerImpl`).
pub struct PrintElementPresentationManager;

impl PrintElementPresentationManager {
    /// Fork and merge edges belong to the higher `layout_index` (the side/feature branch),
    /// keeping the main trunk track color visually pure.
    #[inline]
    pub fn edge_color_id(
        permanent: &PermanentGraph,
        visible: &VisibleGraph,
        edge: GraphEdge,
    ) -> BranchColorId {
        let up_perm = visible.linear.perm_id(edge.up);
        let down_perm = visible.linear.perm_id(edge.down);
        let up_li = permanent.layout_index(up_perm);
        let down_li = permanent.layout_index(down_perm);
        let owner_perm = if up_li >= down_li { up_perm } else { down_perm };
        permanent.node_color(owner_perm)
    }

    #[inline]
    pub fn node_color_id(
        permanent: &PermanentGraph,
        visible: &VisibleGraph,
        row: VisibleRowIdx,
    ) -> BranchColorId {
        permanent.node_color(visible.linear.perm_id(row))
    }
}

#[inline]
pub fn edge_is_visible_in_gap(
    edge: GraphEdge,
    gap_row: VisibleRowIdx,
    show_long_edges: bool,
) -> bool {
    if gap_row < edge.up || gap_row >= edge.down {
        return false;
    }
    let span = edge.down - edge.up;
    let (limit, part) = if show_long_edges {
        (VERY_LONG_EDGE_SIZE, VERY_LONG_EDGE_PART_SIZE)
    } else {
        (LONG_EDGE_SIZE, VISIBLE_PART_SIZE)
    };
    if span < limit {
        return true;
    }
    (gap_row - edge.up) < part || (edge.down - 1 - gap_row) < part
}

struct RowSlotLayout {
    elements: Vec<GraphElement>,
    node_pos: usize,
}

/// On-demand generator producing non-crossing [`GraphPrintElement`] slices for visible rows.
pub struct PrintElementGenerator<'a> {
    permanent: &'a PermanentGraph,
    visible: &'a VisibleGraph,
    head_oid: Option<Oid>,
    edges_gen: EdgesInRowGenerator<'a>,
}

impl<'a> PrintElementGenerator<'a> {
    pub fn new(
        permanent: &'a PermanentGraph,
        visible: &'a VisibleGraph,
        head_oid: Option<Oid>,
    ) -> Self {
        let edges_gen = EdgesInRowGenerator::new(&visible.linear);
        Self {
            permanent,
            visible,
            head_oid,
            edges_gen,
        }
    }

    fn visible_edges_in_gap(&mut self, gap_row: VisibleRowIdx) -> Vec<GraphEdge> {
        let show_long = self.visible.options.show_long_edges;
        self.edges_gen
            .edges_in_row(gap_row)
            .into_iter()
            .filter(|&e| edge_is_visible_in_gap(e, gap_row, show_long))
            .collect()
    }

    pub fn print_elements(&mut self, row: VisibleRowIdx) -> Vec<GraphPrintElement> {
        let n = self.visible.linear.visible_count();
        if row >= n {
            return Vec::new();
        }

        let show_long = self.visible.options.show_long_edges;
        let upper_edges = if row > 0 {
            self.visible_edges_in_gap(row - 1)
        } else {
            Vec::new()
        };
        let lower_edges = self.visible_edges_in_gap(row);

        let curr_slots = self.build_row_slots(row, &upper_edges, &lower_edges);
        let prev_slots = if row > 0 {
            let prev_up = if row > 1 {
                self.visible_edges_in_gap(row - 2)
            } else {
                Vec::new()
            };
            Some(self.build_row_slots(row - 1, &prev_up, &upper_edges))
        } else {
            None
        };
        let next_slots = if row + 1 < n {
            let next_down = self.visible_edges_in_gap(row + 1);
            Some(self.build_row_slots(row + 1, &lower_edges, &next_down))
        } else {
            None
        };

        let mut out = Vec::with_capacity(upper_edges.len() + lower_edges.len() + 2);

        // 1. Incoming edges from row - 1 (and DOWN terminal arrows if hidden below row)
        if let Some(prev) = &prev_slots {
            for &e in &upper_edges {
                let from_pos = slot_of_edge(&curr_slots, row, e);
                let to_pos = slot_of_edge(prev, row - 1, e);
                let color_id =
                    PrintElementPresentationManager::edge_color_id(self.permanent, self.visible, e);
                let style = edge_style(e.edge_type);

                out.push(GraphPrintElement::Edge {
                    row,
                    from_pos,
                    to_pos,
                    direction: EdgeDirection::Up,
                    style,
                    up_row: e.up,
                    down_row: e.down,
                    color_id,
                });

                if e.up < row && row < e.down && !edge_is_visible_in_gap(e, row, show_long) {
                    out.push(GraphPrintElement::TerminalEdge {
                        row,
                        pos: from_pos,
                        direction: EdgeDirection::Down,
                        style,
                        up_row: e.up,
                        down_row: e.down,
                        target_row: e.down,
                        target_perm_id: self.visible.linear.perm_id(e.down),
                        color_id,
                    });
                }
            }
        }

        // 2. Outgoing edges to row + 1 (and UP terminal arrows if hidden above row)
        if let Some(next) = &next_slots {
            for &e in &lower_edges {
                let from_pos = slot_of_edge(&curr_slots, row, e);
                let to_pos = slot_of_edge(next, row + 1, e);
                let color_id =
                    PrintElementPresentationManager::edge_color_id(self.permanent, self.visible, e);
                let style = edge_style(e.edge_type);

                if e.up < row
                    && row < e.down
                    && row > 0
                    && !edge_is_visible_in_gap(e, row - 1, show_long)
                {
                    out.push(GraphPrintElement::TerminalEdge {
                        row,
                        pos: from_pos,
                        direction: EdgeDirection::Up,
                        style,
                        up_row: e.up,
                        down_row: e.down,
                        target_row: e.up,
                        target_perm_id: self.visible.linear.perm_id(e.up),
                        color_id,
                    });
                }

                out.push(GraphPrintElement::Edge {
                    row,
                    from_pos,
                    to_pos,
                    direction: EdgeDirection::Down,
                    style,
                    up_row: e.up,
                    down_row: e.down,
                    color_id,
                });
            }
        }

        // 3. Shallow unloaded parent indicator if any parent is beyond loaded history
        let perm_id = self.visible.linear.perm_id(row);
        let node_color =
            PrintElementPresentationManager::node_color_id(self.permanent, self.visible, row);
        if let Some(unloaded_oid) = self.visible.linear.unloaded_down[row as usize] {
            out.push(GraphPrintElement::ShallowParent {
                row,
                pos: curr_slots.node_pos,
                unloaded_oid,
                color_id: node_color,
            });
        }

        // 4. Commit node itself
        let commit_oid = self.permanent.linear.commit(perm_id).oid;
        let is_head = self.head_oid == Some(commit_oid);
        let is_merge = self.permanent.linear.down_nodes(perm_id).len() >= 2;
        let kind = if is_head {
            NodeKind::Head
        } else if is_merge {
            NodeKind::Merge
        } else {
            NodeKind::Normal
        };

        out.push(GraphPrintElement::Node {
            row,
            perm_id,
            pos: curr_slots.node_pos,
            color_id: node_color,
            kind,
        });

        out
    }

    /// Computes recommended track slot count (in graph tracks, not terminal columns)
    /// using IDEA's `avg + K * std_dev` formula over up to `RECOMMENDED_WIDTH_SAMPLE_SIZE` rows.
    pub fn recommended_track_width(&mut self) -> usize {
        let n = self.visible.linear.visible_count() as usize;
        if n == 0 {
            return 1;
        }
        let sample = n.min(RECOMMENDED_WIDTH_SAMPLE_SIZE);
        let step = (n / sample).max(1);

        let mut count = 0usize;
        let mut sum = 0.0f64;
        let mut sum_sq = 0.0f64;
        let mut prev_vis: Vec<GraphEdge> = Vec::new();
        let mut r = 0usize;

        while r < n && count < sample {
            let row = r as VisibleRowIdx;
            let upper = if step == 1 && r > 0 {
                prev_vis.clone()
            } else if r > 0 {
                self.visible_edges_in_gap(row - 1)
            } else {
                Vec::new()
            };
            let lower = self.visible_edges_in_gap(row);

            let slots = self.count_row_slots(row, &upper, &lower) as f64;
            sum += slots;
            sum_sq += slots * slots;
            count += 1;
            prev_vis = lower;
            r += step;
        }

        if count == 0 {
            return 1;
        }
        let avg = sum / count as f64;
        let var = (sum_sq / count as f64 - avg * avg).max(0.0);
        let std_dev = var.sqrt();
        (avg + RECOMMENDED_WIDTH_K * std_dev).round().max(1.0) as usize
    }

    /// Convenience helper returning the sorted [`GraphElement`]s at `row` for testing
    /// comparator strict weak ordering.
    pub fn sorted_row_elements(&mut self, row: VisibleRowIdx) -> Vec<GraphElement> {
        let upper = if row > 0 {
            self.visible_edges_in_gap(row - 1)
        } else {
            Vec::new()
        };
        let lower = self.visible_edges_in_gap(row);
        self.build_row_slots(row, &upper, &lower).elements
    }

    fn count_row_slots(
        &self,
        row: VisibleRowIdx,
        upper: &[GraphEdge],
        lower: &[GraphEdge],
    ) -> usize {
        let mut passing: Vec<GraphEdge> = Vec::with_capacity(upper.len() + lower.len());
        for &e in upper.iter().chain(lower.iter()) {
            if e.up < row && row < e.down && !passing.contains(&e) {
                passing.push(e);
            }
        }
        1 + passing.len()
    }

    fn build_row_slots(
        &self,
        row: VisibleRowIdx,
        upper: &[GraphEdge],
        lower: &[GraphEdge],
    ) -> RowSlotLayout {
        let mut elements: Vec<GraphElement> = Vec::with_capacity(upper.len() + lower.len() + 1);
        elements.push(GraphElement::Node(row));
        for &e in upper.iter().chain(lower.iter()) {
            if e.up < row && row < e.down {
                let ge = GraphElement::Edge(e);
                if !elements.contains(&ge) {
                    elements.push(ge);
                }
            }
        }

        let permanent = self.permanent;
        let visible = self.visible;
        elements.sort_by(|a, b| compare_elements(a, b, |r| visible.layout_index(permanent, r)));

        let node_pos = elements
            .iter()
            .position(|el| *el == GraphElement::Node(row))
            .unwrap_or(0);
        RowSlotLayout { elements, node_pos }
    }
}

#[inline]
fn slot_of_edge(slots: &RowSlotLayout, row: VisibleRowIdx, edge: GraphEdge) -> usize {
    if edge.up == row || edge.down == row {
        slots.node_pos
    } else {
        slots
            .elements
            .iter()
            .position(|el| *el == GraphElement::Edge(edge))
            .unwrap_or(slots.node_pos)
    }
}

#[inline]
fn edge_style(t: GraphEdgeType) -> LineStyle {
    match t {
        GraphEdgeType::Normal => LineStyle::Solid,
        GraphEdgeType::Dotted => LineStyle::Dashed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::permanent::{PermanentGraph, RawCommitNode};
    use crate::graph::visible::{GraphViewOptions, SortType, VisibleGraph};
    use crate::refs::{MAIN_BRANCH_COLOR_ID, branch_color_id};
    use crate::{Oid, RefInfo, RefKind};
    use fixedbitset::FixedBitSet;

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

    fn branch_ref(name: &str, target: u64, is_head: bool) -> RefInfo {
        RefInfo {
            name: name.to_string(),
            full_name: format!("refs/heads/{name}"),
            target_oid: oid(target),
            kind: RefKind::LocalBranch,
            upstream: None,
            ahead_behind: None,
            is_head,
        }
    }

    #[test]
    fn fork_and_merge_edges_keep_feature_branch_color() {
        // 0: main merge [1, 2]
        // 1: main~1 -> [3]
        // 2: feat -> [3]
        // 3: root -> []
        let commits = vec![
            raw(0, &[1, 2], 40),
            raw(1, &[3], 30),
            raw(2, &[3], 20),
            raw(3, &[], 10),
        ];
        let refs = vec![
            branch_ref("main", 0, true),
            branch_ref("feat/auth", 2, false),
        ];
        let pg = PermanentGraph::build(&commits, &refs, Some(oid(0)));
        let vis = VisibleGraph::build(
            &pg,
            GraphViewOptions {
                sort_type: SortType::Topo,
                ..Default::default()
            },
            None,
        );

        let feat_color = branch_color_id("feat/auth");
        assert_ne!(feat_color, MAIN_BRANCH_COLOR_ID);

        // Merge edge (0 -> 2) and fork edge (2 -> 3) must both belong to feat/auth.
        let merge_edge = GraphEdge {
            up: 0,
            down: 2,
            edge_type: GraphEdgeType::Normal,
        };
        let fork_edge = GraphEdge {
            up: 2,
            down: 3,
            edge_type: GraphEdgeType::Normal,
        };
        let main_edge = GraphEdge {
            up: 0,
            down: 1,
            edge_type: GraphEdgeType::Normal,
        };

        assert_eq!(
            PrintElementPresentationManager::edge_color_id(&pg, &vis, merge_edge),
            feat_color
        );
        assert_eq!(
            PrintElementPresentationManager::edge_color_id(&pg, &vis, fork_edge),
            feat_color
        );
        assert_eq!(
            PrintElementPresentationManager::edge_color_id(&pg, &vis, main_edge),
            MAIN_BRANCH_COLOR_ID
        );
    }

    #[test]
    fn long_edge_50_rows_emits_arrows_at_ends_and_hides_middle() {
        // 0 (main) -> [1, 50] (long edge spanning 50 rows!)
        // 1..50 linear chain
        let mut commits = Vec::new();
        for i in 0..=50u64 {
            let parents = if i == 0 {
                vec![oid(1), oid(50)]
            } else if i < 50 {
                vec![oid(i + 1)]
            } else {
                vec![]
            };
            commits.push(RawCommitNode {
                oid: oid(i),
                parents,
                timestamp: (100 - i) as i64,
            });
        }
        let refs = vec![branch_ref("main", 0, true)];
        let pg = PermanentGraph::build(&commits, &refs, Some(oid(0)));
        let vis = VisibleGraph::build(
            &pg,
            GraphViewOptions {
                sort_type: SortType::Topo,
                show_long_edges: false,
                ..Default::default()
            },
            None,
        );
        let mut elem_gen = PrintElementGenerator::new(&pg, &vis, Some(oid(0)));

        // Row 1 (up + 1): must contain TerminalEdge Down targeting row 50
        let r1 = elem_gen.print_elements(1);
        assert!(
            r1.iter().any(|el| matches!(
                el,
                GraphPrintElement::TerminalEdge {
                    direction: EdgeDirection::Down,
                    up_row: 0,
                    down_row: 50,
                    target_row: 50,
                    ..
                }
            )),
            "row 1 missing DOWN arrow: {r1:?}"
        );

        // Middle rows 2..=48: long edge is hidden; all nodes return to slot 0 and rows 3..=47 use only slot 0
        for r in 2..=48 {
            let els = elem_gen.print_elements(r);
            assert!(
                !els.iter()
                    .any(|el| matches!(el, GraphPrintElement::TerminalEdge { .. })),
                "row {r} should not have terminal arrow: {els:?}"
            );
            assert!(
                !els.iter().any(|el| matches!(
                    el,
                    GraphPrintElement::Edge {
                        up_row: 0,
                        down_row: 50,
                        ..
                    }
                )),
                "row {r} should hide long edge 0->50: {els:?}"
            );
            for el in &els {
                if (3..=47).contains(&r) {
                    assert_eq!(el.max_pos(), 0, "row {r} did not free column: {els:?}");
                } else if let GraphPrintElement::Node { pos, .. } = el {
                    assert_eq!(*pos, 0, "row {r} node did not return to slot 0: {els:?}");
                }
            }
        }

        // Row 49 (down - 1): must contain TerminalEdge Up targeting row 0
        let r49 = elem_gen.print_elements(49);
        assert!(
            r49.iter().any(|el| matches!(
                el,
                GraphPrintElement::TerminalEdge {
                    direction: EdgeDirection::Up,
                    up_row: 0,
                    down_row: 50,
                    target_row: 0,
                    ..
                }
            )),
            "row 49 missing UP arrow: {r49:?}"
        );
    }

    #[test]
    fn criss_cross_merge_and_octopus_merge_render_valid_slots() {
        // Criss-cross: 0 -> [2, 3], 1 -> [3, 2], 2 -> [4], 3 -> [4], 4 -> []
        let commits = vec![
            raw(0, &[2, 3], 50),
            raw(1, &[3, 2], 45),
            raw(2, &[4], 30),
            raw(3, &[4], 25),
            raw(4, &[], 10),
        ];
        let refs = vec![branch_ref("main", 0, true), branch_ref("dev", 1, false)];
        let pg = PermanentGraph::build(&commits, &refs, Some(oid(0)));
        let vis = VisibleGraph::build(&pg, GraphViewOptions::default(), None);
        let mut elem_gen = PrintElementGenerator::new(&pg, &vis, Some(oid(0)));

        for r in 0..vis.linear.visible_count() {
            let els = elem_gen.print_elements(r);
            assert!(
                els.iter()
                    .any(|e| matches!(e, GraphPrintElement::Node { .. }))
            );
        }
    }

    #[test]
    fn unloaded_shallow_parent_emits_on_busy_row_and_partial_merge() {
        // Branch A: 0 -> [1] -> [2 (shallow, unloaded 999)]
        // Branch B: 3 (orphan/concurrent) -> [4] (crosses row 1 where commit 2 sits, and 1 merges [2, 4])
        // Commit 1 merges [2, 4] and also has unloaded 888
        let commits = vec![
            raw(0, &[1], 60),
            raw(3, &[4], 50),
            raw(1, &[2, 888], 40),
            raw(2, &[999], 30),
            raw(4, &[], 10),
        ];
        let refs = vec![branch_ref("main", 0, true), branch_ref("side", 3, false)];
        let pg = PermanentGraph::build(&commits, &refs, Some(oid(0)));
        let vis = VisibleGraph::build(
            &pg,
            GraphViewOptions {
                sort_type: SortType::Topo,
                ..Default::default()
            },
            None,
        );
        let mut elem_gen = PrintElementGenerator::new(&pg, &vis, Some(oid(0)));

        // Commit 1 (row 2) has loaded parent 2 AND unloaded parent 888 -> must emit ShallowParent!
        let r_merge = elem_gen.print_elements(2);
        assert!(
            r_merge.iter().any(|el| matches!(
                el,
                GraphPrintElement::ShallowParent { unloaded_oid, .. } if *unloaded_oid == oid(888)
            )),
            "partial merge missing ShallowParent: {r_merge:?}"
        );

        // Commit 2 (row 3) has unloaded parent 999 while side branch edge (3 -> 4) passes through row 3 -> must still emit ShallowParent!
        let r_busy = elem_gen.print_elements(3);
        assert!(
            r_busy.iter().any(|el| matches!(
                el,
                GraphPrintElement::ShallowParent { unloaded_oid, .. } if *unloaded_oid == oid(999)
            )),
            "busy row missing ShallowParent: {r_busy:?}"
        );
    }

    proptest::proptest! {
        #![proptest_config(proptest::prelude::ProptestConfig::with_cases(100))]
        #[test]
        fn exhaustive_triple_transitivity_across_random_dags_topo_bek_and_filtered(
            n in 3usize..=35,
            raw_nodes in proptest::collection::vec((proptest::collection::vec(0usize..64, 1..=3), 0i64..1000, proptest::bool::ANY), 35),
        ) {
            let mut commits = Vec::with_capacity(n);
            for (i, (parent_rolls, ts, _)) in raw_nodes.iter().take(n).enumerate() {
                let mut parents = Vec::new();
                if i + 1 < n {
                    let remaining = n - 1 - i;
                    for &roll in parent_rolls {
                        let p = i + 1 + (roll % remaining);
                        parents.push(oid(p as u64));
                    }
                }
                commits.push(RawCommitNode {
                    oid: oid(i as u64),
                    parents,
                    timestamp: *ts,
                });
            }
            let refs = vec![branch_ref("main", 0, true)];
            let pg = PermanentGraph::build(&commits, &refs, Some(oid(0)));

            let mut filter = FixedBitSet::with_capacity(n);
            for (i, (_, _, keep)) in raw_nodes.iter().take(n).enumerate() {
                if *keep {
                    filter.insert(i);
                }
            }

            let configs = [
                (SortType::Topo, None),
                (SortType::IntelliSort, None),
                (SortType::IntelliSort, Some(&filter)),
            ];

            for (sort_type, flt) in configs {
                let vis = VisibleGraph::build(
                    &pg,
                    GraphViewOptions {
                        sort_type,
                        ..Default::default()
                    },
                    flt,
                );
                let mut elem_gen = PrintElementGenerator::new(&pg, &vis, Some(oid(0)));
                for r in 0..vis.linear.visible_count() {
                    let els = elem_gen.sorted_row_elements(r);
                    let cmp = |x: &GraphElement, y: &GraphElement| {
                        compare_elements(x, y, |row| vis.layout_index(&pg, row))
                    };

                    // Verify strict weak ordering on all triples (a, b, c)
                    for a in &els {
                        proptest::prop_assert_eq!(cmp(a, a), Ordering::Equal);
                        for b in &els {
                            proptest::prop_assert_eq!(cmp(a, b), cmp(b, a).reverse());
                            for c in &els {
                                let ab = cmp(a, b);
                                let bc = cmp(b, c);
                                let ac = cmp(a, c);
                                if ab == Ordering::Less && bc == Ordering::Less {
                                    proptest::prop_assert_eq!(ac, Ordering::Less);
                                }
                                if ab == Ordering::Equal && bc == Ordering::Equal {
                                    proptest::prop_assert_eq!(ac, Ordering::Equal);
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
