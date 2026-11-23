//! Linear-branch collapsing projection (`CollapsedGraph` /
//! `LinearFragmentGenerator` in IDEA's vcs-log).
//!
//! A **linear segment** is a maximal chain of commits that each have exactly
//! one loaded child and exactly one loaded parent, and that carry no ref and
//! are not `HEAD` (the *pinned* nodes). Collapsing a segment hides its middle
//! commits in the [`super::VisibleGraph`] projection and joins the two visible
//! ends (`up` = the child above the chain, `down` = the parent below it) with a
//! single [`super::GraphEdgeType::Collapsed`] edge. The permanent graph never
//! changes; only the visible projection and its row mapping do.

use std::collections::HashSet;

use fixedbitset::FixedBitSet;

use crate::Oid;
use crate::graph::permanent::{PermNodeId, PermanentLinearGraph};

/// Segments shorter than this are not worth a dotted edge (hiding a single
/// commit saves one row but costs the reader a click).
pub const MIN_SEGMENT_LEN: u32 = 2;

const NO_SEGMENT: u32 = u32::MAX;

/// Stable identity of a linear segment: the oid of its topmost hidden commit.
///
/// Oid-based (not [`PermNodeId`]) so collapsed state survives snapshot
/// reloads, which renumber permanent nodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SegmentId(pub Oid);

/// One collapsible chain of commits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LinearSegment {
    pub id: SegmentId,
    /// Visible child end (stays visible while collapsed).
    pub up: PermNodeId,
    /// Visible parent end (stays visible while collapsed).
    pub down: PermNodeId,
    /// Topmost middle (hidden) node.
    pub first: PermNodeId,
    /// Number of middle (hidden) nodes.
    pub len: u32,
}

/// All linear segments of a permanent graph plus a node → segment index.
#[derive(Debug, Clone, Default)]
pub struct LinearSegments {
    segments: Vec<LinearSegment>,
    segment_of: Vec<u32>,
}

impl LinearSegments {
    /// Pinned nodes for [`Self::compute`]: every ref target plus `head_oid`.
    pub fn pinned_nodes(
        linear: &PermanentLinearGraph,
        ref_targets: impl IntoIterator<Item = Oid>,
        head_oid: Option<Oid>,
    ) -> FixedBitSet {
        let mut pinned = FixedBitSet::with_capacity(linear.nodes_count as usize);
        for oid in ref_targets.into_iter().chain(head_oid) {
            if let Some(node) = linear.node_of_oid(&oid) {
                pinned.insert(node as usize);
            }
        }
        pinned
    }

    /// Finds every linear segment in `linear` in `O(nodes)`.
    pub fn compute(linear: &PermanentLinearGraph, pinned: &FixedBitSet) -> Self {
        let total = linear.nodes_count as usize;
        let mut segment_of = vec![NO_SEGMENT; total];
        let mut segments = Vec::new();

        let single_parent = |n: PermNodeId| -> Option<PermNodeId> {
            match linear.down_nodes(n) {
                [only] => only.as_loaded(),
                _ => None,
            }
        };
        let interior = |n: PermNodeId| -> bool {
            !pinned.contains(n as usize)
                && linear.up_nodes(n).len() == 1
                && single_parent(n).is_some()
        };

        for start in 0..total as PermNodeId {
            if !interior(start) {
                continue;
            }
            let up = linear.up_nodes(start)[0];
            if interior(up) {
                // Not the top of its chain; reached from the top instead.
                continue;
            }
            let mut len = 1u32;
            let mut cur = start;
            let down = loop {
                let parent = single_parent(cur).expect("interior node has one loaded parent");
                if interior(parent) {
                    cur = parent;
                    len += 1;
                } else {
                    break parent;
                }
            };
            // A segment whose ends are also directly linked would collapse into
            // a duplicate of that edge; keep it expanded.
            let directly_linked = linear.loaded_down_nodes(up).any(|p| p == down);
            if len < MIN_SEGMENT_LEN || directly_linked {
                continue;
            }
            let idx = segments.len() as u32;
            let mut node = start;
            for _ in 0..len {
                segment_of[node as usize] = idx;
                node = single_parent(node).unwrap_or(node);
            }
            segments.push(LinearSegment {
                id: SegmentId(linear.commit(start).oid),
                up,
                down,
                first: start,
                len,
            });
        }

        Self {
            segments,
            segment_of,
        }
    }

    pub fn len(&self) -> usize {
        self.segments.len()
    }

    pub fn is_empty(&self) -> bool {
        self.segments.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &LinearSegment> {
        self.segments.iter()
    }

    pub fn get(&self, idx: usize) -> Option<&LinearSegment> {
        self.segments.get(idx)
    }

    /// Index of the segment whose *middle* contains `node`.
    #[inline]
    pub fn segment_index_of(&self, node: PermNodeId) -> Option<usize> {
        match self.segment_of.get(node as usize).copied() {
            None | Some(NO_SEGMENT) => None,
            Some(idx) => Some(idx as usize),
        }
    }

    /// The segment whose middle contains `node`.
    pub fn segment_of(&self, node: PermNodeId) -> Option<&LinearSegment> {
        self.segment_index_of(node).map(|i| &self.segments[i])
    }

    /// Segments joining visible ends `up` → `down`. Several parallel chains
    /// (diamond / octopus arms) share one collapsed edge.
    pub fn segments_between(
        &self,
        up: PermNodeId,
        down: PermNodeId,
    ) -> impl Iterator<Item = &LinearSegment> {
        self.segments
            .iter()
            .filter(move |s| s.up == up && s.down == down)
    }

    /// Segment for an edge from `up` to its parent `parent` (the first middle node).
    pub fn segment_below(&self, up: PermNodeId, parent: PermNodeId) -> Option<&LinearSegment> {
        self.segment_of(parent)
            .filter(|s| s.up == up && s.first == parent)
    }

    pub fn by_id(&self, id: SegmentId) -> Option<&LinearSegment> {
        self.segments.iter().find(|s| s.id == id)
    }

    /// Middle nodes of `segment`, top to bottom.
    pub fn middle_nodes<'a>(
        &self,
        linear: &'a PermanentLinearGraph,
        segment: &LinearSegment,
    ) -> impl Iterator<Item = PermNodeId> + 'a {
        let mut next = Some(segment.first);
        let mut remaining = segment.len;
        std::iter::from_fn(move || {
            if remaining == 0 {
                return None;
            }
            remaining -= 1;
            let cur = next?;
            next = linear.loaded_down_nodes(cur).next();
            Some(cur)
        })
    }
}

/// How [`CollapseState::ids`] is interpreted (part of `GraphViewOptions`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum CollapseMode {
    /// Every segment expanded.
    #[default]
    None,
    /// Every segment collapsed except the ids in [`CollapseState::ids`].
    All,
    /// Only the ids in [`CollapseState::ids`] are collapsed.
    Custom,
}

/// User-chosen collapsed set (`collapsed: HashSet<SegmentId>`), robust to
/// snapshot reloads: `All` keeps collapsing segments created later.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CollapseState {
    pub mode: CollapseMode,
    /// Exceptions in `All` mode, collapsed segments in `Custom` mode.
    pub ids: HashSet<SegmentId>,
}

impl CollapseState {
    pub fn is_collapsed(&self, id: SegmentId) -> bool {
        match self.mode {
            CollapseMode::None => false,
            CollapseMode::All => !self.ids.contains(&id),
            CollapseMode::Custom => self.ids.contains(&id),
        }
    }

    /// Whether any segment could be collapsed under this state.
    pub fn is_active(&self) -> bool {
        self.mode != CollapseMode::None
    }

    pub fn collapse_all(&mut self) {
        self.mode = CollapseMode::All;
        self.ids.clear();
    }

    pub fn expand_all(&mut self) {
        self.mode = CollapseMode::None;
        self.ids.clear();
    }

    /// Collapses (`true`) or expands (`false`) one segment.
    pub fn set_collapsed(&mut self, id: SegmentId, collapsed: bool) {
        match (self.mode, collapsed) {
            (CollapseMode::None, false) => {}
            (CollapseMode::None, true) => {
                self.mode = CollapseMode::Custom;
                self.ids.clear();
                self.ids.insert(id);
            }
            (CollapseMode::All, true) => {
                self.ids.remove(&id);
            }
            (CollapseMode::All, false) => {
                self.ids.insert(id);
            }
            (CollapseMode::Custom, true) => {
                self.ids.insert(id);
            }
            (CollapseMode::Custom, false) => {
                self.ids.remove(&id);
                if self.ids.is_empty() {
                    self.mode = CollapseMode::None;
                }
            }
        }
    }

    /// Per-segment collapsed flags for `segments`.
    pub fn collapsed_flags(&self, segments: &LinearSegments) -> Vec<bool> {
        segments.iter().map(|s| self.is_collapsed(s.id)).collect()
    }
}

/// Collapse inputs for [`super::VisibleGraph::build_collapsed`].
#[derive(Debug, Clone, Copy)]
pub struct CollapseInput<'a> {
    pub segments: &'a LinearSegments,
    pub state: &'a CollapseState,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::permanent::{PermanentGraph, RawCommitNode};
    use crate::graph::visible::{
        GraphEdge, GraphEdgeType, GraphViewOptions, SortType, VisibleGraph,
    };

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

    fn graph(commits: &[RawCommitNode], pinned: &[u64]) -> (PermanentGraph, LinearSegments) {
        let pg = PermanentGraph::build(commits, &[], None);
        let pins = LinearSegments::pinned_nodes(&pg.linear, pinned.iter().copied().map(oid), None);
        let segs = LinearSegments::compute(&pg.linear, &pins);
        (pg, segs)
    }

    fn topo() -> GraphViewOptions {
        GraphViewOptions {
            sort_type: SortType::Topo,
            ..Default::default()
        }
    }

    fn visible_oids(pg: &PermanentGraph, vis: &VisibleGraph) -> Vec<Oid> {
        (0..vis.linear.visible_count())
            .map(|r| pg.linear.commit(vis.linear.perm_id(r)).oid)
            .collect()
    }

    fn all_collapsed() -> CollapseState {
        let mut s = CollapseState::default();
        s.collapse_all();
        s
    }

    fn assert_row_mapping_consistent(vis: &VisibleGraph) {
        for r in 0..vis.linear.visible_count() {
            let perm = vis.linear.perm_id(r);
            assert_eq!(vis.linear.visible_row(perm), Some(r));
        }
        let mapped = vis
            .linear
            .perm_to_visible
            .iter()
            .filter(|r| r.is_some())
            .count();
        assert_eq!(mapped as u32, vis.linear.visible_count());
    }

    #[test]
    fn simple_chain_collapses_middle_into_one_collapsed_edge() {
        // 0 (pinned head) -> 1 -> 2 -> 3 -> 4 (root)
        let commits = vec![
            raw(0, &[1], 50),
            raw(1, &[2], 40),
            raw(2, &[3], 30),
            raw(3, &[4], 20),
            raw(4, &[], 10),
        ];
        let (pg, segs) = graph(&commits, &[0]);
        assert_eq!(segs.len(), 1);
        let s = segs.get(0).copied().unwrap();
        assert_eq!((s.up, s.first, s.down, s.len), (0, 1, 4, 3));
        assert_eq!(s.id, SegmentId(oid(1)));
        assert_eq!(
            segs.middle_nodes(&pg.linear, &s).collect::<Vec<_>>(),
            vec![1, 2, 3]
        );

        let state = all_collapsed();
        let vis = VisibleGraph::build_collapsed(
            &pg,
            topo(),
            None,
            Some(CollapseInput {
                segments: &segs,
                state: &state,
            }),
        );
        assert!(vis.collapse_applied);
        assert_eq!(visible_oids(&pg, &vis), vec![oid(0), oid(4)]);
        assert_eq!(
            vis.linear.down_edges(0),
            &[GraphEdge {
                up: 0,
                down: 1,
                edge_type: GraphEdgeType::Collapsed,
            }]
        );
        assert_row_mapping_consistent(&vis);

        // Expanding restores the full projection.
        let mut state = state;
        state.set_collapsed(s.id, false);
        let vis = VisibleGraph::build_collapsed(
            &pg,
            topo(),
            None,
            Some(CollapseInput {
                segments: &segs,
                state: &state,
            }),
        );
        assert_eq!(vis.linear.visible_count(), 5);
    }

    #[test]
    fn single_hidden_commit_is_not_a_segment() {
        let commits = vec![raw(0, &[1], 30), raw(1, &[2], 20), raw(2, &[], 10)];
        let (_, segs) = graph(&commits, &[0]);
        assert!(segs.is_empty());
    }

    #[test]
    fn diamond_merge_collapses_side_chain_and_keeps_fork_and_merge() {
        // 0 merge [1, 3]; 1 -> 2 -> 5; 3 -> 4 -> 5; 5 root. Head 0 pinned.
        let commits = vec![
            raw(0, &[1, 3], 60),
            raw(1, &[2], 50),
            raw(2, &[5], 40),
            raw(3, &[4], 35),
            raw(4, &[5], 30),
            raw(5, &[], 10),
        ];
        let (pg, segs) = graph(&commits, &[0]);
        assert_eq!(segs.len(), 2);
        let ids: Vec<_> = segs.iter().map(|s| s.id).collect();
        assert_eq!(ids, vec![SegmentId(oid(1)), SegmentId(oid(3))]);
        for s in segs.iter() {
            assert_eq!((s.up, s.down, s.len), (0, 5, 2));
        }

        // Collapse only the second-parent side chain.
        let mut state = CollapseState::default();
        state.set_collapsed(SegmentId(oid(3)), true);
        assert_eq!(state.mode, CollapseMode::Custom);
        let vis = VisibleGraph::build_collapsed(
            &pg,
            topo(),
            None,
            Some(CollapseInput {
                segments: &segs,
                state: &state,
            }),
        );
        assert_eq!(
            visible_oids(&pg, &vis),
            vec![oid(0), oid(1), oid(2), oid(5)]
        );
        // Parent order is preserved: Normal to 1, Collapsed to 5.
        let edges = vis.linear.down_edges(0);
        assert_eq!(edges.len(), 2);
        assert_eq!(edges[0].edge_type, GraphEdgeType::Normal);
        assert_eq!(edges[1].edge_type, GraphEdgeType::Collapsed);
        assert_eq!(vis.linear.perm_id(edges[1].down), 5);
        assert_row_mapping_consistent(&vis);
    }

    #[test]
    fn ref_pinned_middle_commit_splits_the_chain() {
        // 0 -> 1 -> 2 -> 3(tag) -> 4 -> 5 -> 6 root
        let commits: Vec<_> = (0..7u64)
            .map(|i| {
                let parents: Vec<u64> = if i < 6 { vec![i + 1] } else { vec![] };
                raw(i, &parents, 100 - i as i64)
            })
            .collect();
        let (pg, segs) = graph(&commits, &[0, 3]);
        let shape: Vec<_> = segs
            .iter()
            .map(|s| (s.up, s.first, s.down, s.len))
            .collect();
        assert_eq!(shape, vec![(0, 1, 3, 2), (3, 4, 6, 2)]);
        assert!(segs.segment_of(3).is_none());

        let vis = VisibleGraph::build_collapsed(
            &pg,
            GraphViewOptions::default(),
            None,
            Some(CollapseInput {
                segments: &segs,
                state: &all_collapsed(),
            }),
        );
        assert_eq!(visible_oids(&pg, &vis), vec![oid(0), oid(3), oid(6)]);
        assert_row_mapping_consistent(&vis);
    }

    #[test]
    fn octopus_merge_arms_collapse_independently() {
        // 0 octopus [1, 3, 5]; each arm two commits deep onto root 7.
        let commits = vec![
            raw(0, &[1, 3, 5], 90),
            raw(1, &[2], 80),
            raw(2, &[7], 70),
            raw(3, &[4], 60),
            raw(4, &[7], 50),
            raw(5, &[6], 40),
            raw(6, &[7], 30),
            raw(7, &[], 10),
        ];
        let (pg, segs) = graph(&commits, &[0]);
        assert_eq!(segs.len(), 3);
        let vis = VisibleGraph::build_collapsed(
            &pg,
            topo(),
            None,
            Some(CollapseInput {
                segments: &segs,
                state: &all_collapsed(),
            }),
        );
        assert_eq!(visible_oids(&pg, &vis), vec![oid(0), oid(7)]);
        // The three arms share one collapsed edge to the root.
        assert_eq!(
            vis.linear.down_edges(0),
            &[GraphEdge {
                up: 0,
                down: 1,
                edge_type: GraphEdgeType::Collapsed,
            }]
        );
        assert_eq!(segs.segments_between(0, 7).count(), 3);
        assert_row_mapping_consistent(&vis);
    }

    #[test]
    fn directly_linked_ends_are_not_collapsible() {
        // 0 merge [1, 3]; 1 -> 2 -> 3; i.e. 0 is directly linked to 3.
        let commits = vec![
            raw(0, &[1, 3], 40),
            raw(1, &[2], 30),
            raw(2, &[3], 20),
            raw(3, &[], 10),
        ];
        let (_, segs) = graph(&commits, &[0]);
        assert!(segs.is_empty());
    }

    #[test]
    fn collapse_is_ignored_under_filter_or_first_parent() {
        let commits = vec![
            raw(0, &[1], 50),
            raw(1, &[2], 40),
            raw(2, &[3], 30),
            raw(3, &[], 20),
        ];
        let (pg, segs) = graph(&commits, &[0]);
        let state = all_collapsed();
        let input = Some(CollapseInput {
            segments: &segs,
            state: &state,
        });
        let fp = VisibleGraph::build_collapsed(
            &pg,
            GraphViewOptions {
                first_parent_only: true,
                ..Default::default()
            },
            None,
            input,
        );
        assert!(!fp.collapse_applied);
        assert_eq!(fp.linear.visible_count(), 4);

        let mut all = FixedBitSet::with_capacity(4);
        all.insert_range(..);
        let filtered = VisibleGraph::build_collapsed(&pg, topo(), Some(&all), input);
        assert!(!filtered.collapse_applied);
        assert_eq!(filtered.linear.visible_count(), 4);
    }

    #[test]
    fn collapse_state_transitions() {
        let a = SegmentId(oid(1));
        let b = SegmentId(oid(2));
        let mut s = CollapseState::default();
        assert!(!s.is_collapsed(a));
        s.set_collapsed(a, false);
        assert_eq!(s.mode, CollapseMode::None);
        s.set_collapsed(a, true);
        assert!(s.is_collapsed(a) && !s.is_collapsed(b));
        s.set_collapsed(a, false);
        assert_eq!(s.mode, CollapseMode::None);

        s.collapse_all();
        assert!(s.is_collapsed(a) && s.is_collapsed(b));
        s.set_collapsed(b, false);
        assert!(s.is_collapsed(a) && !s.is_collapsed(b));
        s.set_collapsed(b, true);
        assert!(s.is_collapsed(b));
        s.expand_all();
        assert!(!s.is_active());
    }

    /// Spec 10 method: 50k commits, collapse-all / expand-all projection
    /// rebuild must stay under 50ms (release; debug builds get 10x slack).
    #[test]
    fn collapse_all_projection_rebuild_is_fast_on_50k_commits() {
        use std::time::Instant;
        // Mainline with a two-commit side branch (forked 5 commits lower)
        // merged every 10 commits.
        let mut commits = Vec::new();
        let n_main = 40_000u64;
        let side_base = 1_000_000u64;
        for i in 0..n_main {
            let mut parents = Vec::new();
            if i + 1 < n_main {
                parents.push(i + 1);
            }
            if i % 10 == 0 && i + 5 < n_main {
                let s = side_base + i * 2;
                parents.push(s);
                commits.push((i, parents));
                commits.push((s, vec![s + 1]));
                commits.push((s + 1, vec![i + 5]));
                continue;
            }
            commits.push((i, parents));
        }
        let raws: Vec<_> = commits
            .iter()
            .enumerate()
            .map(|(idx, (id, parents))| raw(*id, parents, 10_000_000 - idx as i64))
            .collect();
        assert!(raws.len() >= 48_000);
        let pins: Vec<u64> = (0..n_main).step_by(500).collect();
        let (pg, segs) = graph(&raws, &pins);
        assert!(!segs.is_empty());

        let budget_ms = if cfg!(debug_assertions) { 500 } else { 50 };
        let state = all_collapsed();
        let t = Instant::now();
        let collapsed = VisibleGraph::build_collapsed(
            &pg,
            GraphViewOptions::default(),
            None,
            Some(CollapseInput {
                segments: &segs,
                state: &state,
            }),
        );
        let collapse_ms = t.elapsed().as_millis();
        assert!(collapsed.linear.visible_count() < pg.linear.nodes_count / 2);

        let expanded_state = CollapseState::default();
        let t = Instant::now();
        let expanded = VisibleGraph::build_collapsed(
            &pg,
            GraphViewOptions::default(),
            None,
            Some(CollapseInput {
                segments: &segs,
                state: &expanded_state,
            }),
        );
        let expand_ms = t.elapsed().as_millis();
        assert_eq!(expanded.linear.visible_count(), pg.linear.nodes_count);
        assert!(
            collapse_ms < budget_ms && expand_ms < budget_ms,
            "collapse {collapse_ms}ms / expand {expand_ms}ms (budget {budget_ms}ms)"
        );
    }
}
