use std::cell::RefCell;
use std::collections::{BTreeSet, HashSet};
use std::sync::Arc;

use jbgit_core::graph::{
    CollapseInput, CollapseState, EdgesInRowCache, FixedBitSet, GraphEdgeType, GraphViewOptions,
    LinearBekState, LinearSegments, PermNodeId, PermanentGraph, PermanentLinearGraph,
    PrintElementGenerator, RawCommitNode, ReachableNodes, SegmentId, VisibleGraph,
    VisibleGraphBuildArgs, VisibleRowIdx,
};
use jbgit_core::{
    BranchColorId, CommitSummary, EpochToken, FilterError, LogFilterSpec, Oid, RefInfo, RefKind,
    RepoSnapshot, RequestSlot,
};
use ratatui::style::Color;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::theme::BranchColorManager;

/// Snapshots with at least this many commits offload `VisibleGraph` and filter
/// rebuilds to a blocking worker thread when the async runtime is active.
pub const ASYNC_GRAPH_THRESHOLD: usize = 2_000;

/// Per-snapshot permanent graph plus the current filtered/sorted projection.
pub struct CachedLogGraph {
    pub snapshot: Arc<RepoSnapshot>,
    /// Shared so background jobs (containing-branch BFS) can walk it off the UI thread.
    pub permanent: Arc<PermanentGraph>,
    /// Collapsible linear segments of `permanent` (refs / HEAD pinned).
    pub segments: Arc<LinearSegments>,
    pub visible: VisibleGraph,
    /// Whether `visible` was projected through a filter bitset.
    pub filtered: bool,
    /// Cached filter inputs for fast synchronous collapse / LinearBek re-projections.
    pub last_filter: Option<FixedBitSet>,
    pub last_branch_heads: Option<Vec<PermNodeId>>,
    pub branch_only_filter: bool,
    /// True when the snapshot contains commits from at least 2 distinct authors
    /// (used to auto-suppress `My Commits` highlighting in single-author repos).
    pub has_multiple_authors: bool,
    pub color_map: FxHashMap<BranchColorId, Color>,
    pub current_branch_reachable: FixedBitSet,
    pub recommended_tracks: usize,
    pub edges_cache: RefCell<EdgesInRowCache>,
    pub refs_by_oid: FxHashMap<Oid, Vec<usize>>,
    pub ref_kind_by_name: FxHashMap<String, RefKind>,
    /// `LogGraphState::filter_generation` the visible projection was built for.
    pub filter_generation: u64,
    /// Number of `VisibleGraph` projections built for this snapshot (tests).
    #[cfg(test)]
    pub visible_builds: u32,
}

impl std::fmt::Debug for CachedLogGraph {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CachedLogGraph")
            .field("visible_count", &self.visible.linear.visible_count())
            .field("filter_generation", &self.filter_generation)
            .finish_non_exhaustive()
    }
}

impl CachedLogGraph {
    pub fn build(
        snap: &Arc<RepoSnapshot>,
        options: GraphViewOptions,
        filter: Option<&FixedBitSet>,
        branch_colors: &BranchColorManager,
    ) -> Self {
        Self::build_with(
            snap,
            options,
            &CollapseState::default(),
            &LinearBekState::default(),
            false,
            branch_colors,
            |_| (filter.cloned(), None),
        )
    }

    /// Builds the permanent graph, then asks `select` for the visible-node
    /// filter (which usually needs the permanent node ids) and projects the
    /// visible graph exactly once.
    pub fn build_with<F>(
        snap: &Arc<RepoSnapshot>,
        options: GraphViewOptions,
        collapse: &CollapseState,
        linear_bek: &LinearBekState,
        branch_only_filter: bool,
        branch_colors: &BranchColorManager,
        select: F,
    ) -> Self
    where
        F: FnOnce(&PermanentLinearGraph) -> (Option<FixedBitSet>, Option<Vec<PermNodeId>>),
    {
        let raw_commits: Vec<RawCommitNode> =
            snap.commits.iter().map(RawCommitNode::from).collect();
        let permanent = Arc::new(PermanentGraph::build(
            &raw_commits,
            &snap.refs,
            snap.head_oid,
        ));
        let color_map = build_graph_color_map(&permanent, branch_colors);
        let head_nodes: Vec<_> = snap
            .head_oid
            .and_then(|oid| permanent.linear.node_of_oid(&oid))
            .into_iter()
            .collect();
        let current_branch_reachable =
            ReachableNodes::contained_in_branch(&permanent.linear, &head_nodes);
        let segments = Arc::new(build_segments(&permanent, snap));
        let has_multiple_authors = compute_has_multiple_authors(snap);
        let (filter, branch_heads) = select(&permanent.linear);
        let visible = VisibleGraph::build_projected(
            &permanent,
            options,
            VisibleGraphBuildArgs {
                filter: filter.as_ref(),
                branch_heads: branch_heads.as_deref(),
                branch_only_filter,
                collapse: Some(CollapseInput {
                    segments: &segments,
                    state: collapse,
                }),
                linear_bek: Some(linear_bek),
            },
        );
        let mut edges_cache = EdgesInRowCache::new(&visible.linear);
        let recommended_tracks = PrintElementGenerator::with_cache(
            &permanent,
            &visible,
            snap.head_oid,
            &mut edges_cache,
        )
        .recommended_track_width();
        let (refs_by_oid, ref_kind_by_name) = build_ref_indices(&snap.refs);

        Self {
            snapshot: Arc::clone(snap),
            permanent,
            segments,
            filtered: filter.is_some(),
            last_filter: filter,
            last_branch_heads: branch_heads,
            branch_only_filter,
            has_multiple_authors,
            visible,
            color_map,
            current_branch_reachable,
            recommended_tracks,
            edges_cache: RefCell::new(edges_cache),
            refs_by_oid,
            ref_kind_by_name,
            filter_generation: 0,
            #[cfg(test)]
            visible_builds: 1,
        }
    }

    pub fn refs_for_oid<'a>(
        &self,
        snap: &'a RepoSnapshot,
        oid: Oid,
    ) -> impl Iterator<Item = &'a RefInfo> {
        self.refs_by_oid
            .get(&oid)
            .into_iter()
            .flat_map(|indices| indices.iter().filter_map(|&i| snap.refs.get(i)))
    }

    fn rebuild_visible(
        &mut self,
        options: GraphViewOptions,
        filter: Option<FixedBitSet>,
        branch_heads: Option<Vec<PermNodeId>>,
        branch_only_filter: bool,
        collapse: &CollapseState,
        linear_bek: &LinearBekState,
    ) {
        self.filtered = filter.is_some();
        self.last_filter = filter;
        self.last_branch_heads = branch_heads;
        self.branch_only_filter = branch_only_filter;
        self.visible = VisibleGraph::build_projected(
            &self.permanent,
            options,
            VisibleGraphBuildArgs {
                filter: self.last_filter.as_ref(),
                branch_heads: self.last_branch_heads.as_deref(),
                branch_only_filter,
                collapse: Some(CollapseInput {
                    segments: &self.segments,
                    state: collapse,
                }),
                linear_bek: Some(linear_bek),
            },
        );
        let mut edges_cache = EdgesInRowCache::new(&self.visible.linear);
        self.recommended_tracks = PrintElementGenerator::with_cache(
            &self.permanent,
            &self.visible,
            self.snapshot.head_oid,
            &mut edges_cache,
        )
        .recommended_track_width();
        self.edges_cache = RefCell::new(edges_cache);
        #[cfg(test)]
        {
            self.visible_builds = self.visible_builds.saturating_add(1);
        }
    }

    fn rebuild_visible_keeping_filter(
        &mut self,
        options: GraphViewOptions,
        collapse: &CollapseState,
        linear_bek: &LinearBekState,
    ) {
        self.visible = VisibleGraph::build_projected(
            &self.permanent,
            options,
            VisibleGraphBuildArgs {
                filter: self.last_filter.as_ref(),
                branch_heads: self.last_branch_heads.as_deref(),
                branch_only_filter: self.branch_only_filter,
                collapse: Some(CollapseInput {
                    segments: &self.segments,
                    state: collapse,
                }),
                linear_bek: Some(linear_bek),
            },
        );
        let mut edges_cache = EdgesInRowCache::new(&self.visible.linear);
        self.recommended_tracks = PrintElementGenerator::with_cache(
            &self.permanent,
            &self.visible,
            self.snapshot.head_oid,
            &mut edges_cache,
        )
        .recommended_track_width();
        self.edges_cache = RefCell::new(edges_cache);
        #[cfg(test)]
        {
            self.visible_builds = self.visible_builds.saturating_add(1);
        }
    }

    pub fn visible_count(&self) -> usize {
        self.visible.linear.visible_count() as usize
    }

    pub fn commit_at_visible_row<'a>(
        &self,
        snap: &'a RepoSnapshot,
        visible_row: usize,
    ) -> Option<&'a CommitSummary> {
        let row = u32::try_from(visible_row).ok()?;
        if row >= self.visible.linear.visible_count() {
            return None;
        }
        let perm_id = self.visible.linear.perm_id(row);
        let orig_idx = *self.permanent.linear.perm_to_input.get(perm_id as usize)?;
        snap.commits.get(orig_idx)
    }

    pub fn row_of_oid(&self, oid: &Oid) -> Option<usize> {
        let node = self.permanent.linear.node_of_oid(oid)?;
        self.visible.linear.visible_row(node).map(|r| r as usize)
    }

    /// Collapsed segment hiding `oid` in the current projection, if any.
    pub fn collapsed_segment_hiding(&self, oid: &Oid) -> Option<SegmentId> {
        if !self.visible.collapse_applied {
            return None;
        }
        let node = self.permanent.linear.node_of_oid(oid)?;
        if self.visible.linear.visible_row(node).is_some() {
            return None;
        }
        self.segments.segment_of(node).map(|s| s.id)
    }

    /// Segments drawn as the collapsed edge `up_row` → `down_row`.
    pub fn collapsed_segments_at_edge(&self, up_row: usize, down_row: usize) -> Vec<SegmentId> {
        let linear = &self.visible.linear;
        let (Ok(up), Ok(down)) = (
            VisibleRowIdx::try_from(up_row),
            VisibleRowIdx::try_from(down_row),
        ) else {
            return Vec::new();
        };
        if up >= linear.visible_count() || down >= linear.visible_count() {
            return Vec::new();
        }
        let is_collapsed_edge = linear
            .down_edges(up)
            .iter()
            .any(|e| e.down == down && e.edge_type == GraphEdgeType::Collapsed);
        if !is_collapsed_edge {
            return Vec::new();
        }
        self.segments
            .segments_between(linear.perm_id(up), linear.perm_id(down))
            .map(|s| s.id)
            .collect()
    }

    /// Expanded collapsible segment that owns the visible edge `up_row` → `down_row`, if any.
    pub fn expanded_segment_at_edge(&self, up_row: usize, down_row: usize) -> Option<SegmentId> {
        let linear = &self.visible.linear;
        let (Ok(up), Ok(down)) = (
            VisibleRowIdx::try_from(up_row),
            VisibleRowIdx::try_from(down_row),
        ) else {
            return None;
        };
        if up >= linear.visible_count() || down >= linear.visible_count() {
            return None;
        }
        let is_expanded_edge = linear
            .down_edges(up)
            .iter()
            .any(|e| e.down == down && e.edge_type != GraphEdgeType::Collapsed);
        if !is_expanded_edge {
            return None;
        }
        let up_perm = linear.perm_id(up);
        let down_perm = linear.perm_id(down);
        if let Some(seg) = self.segments.segment_of(up_perm)
            && (self
                .segments
                .segment_of(down_perm)
                .is_some_and(|d| d.id == seg.id)
                || seg.down == down_perm)
        {
            return Some(seg.id);
        }
        if let Some(seg) = self.segments.segment_of(down_perm)
            && seg.up == up_perm
        {
            return Some(seg.id);
        }
        None
    }

    /// Collapsed segments hanging directly below visible `row`.
    pub fn collapsed_segments_below(&self, row: usize) -> Vec<SegmentId> {
        let Ok(r) = VisibleRowIdx::try_from(row) else {
            return Vec::new();
        };
        if r >= self.visible.linear.visible_count() {
            return Vec::new();
        }
        let downs: Vec<usize> = self
            .visible
            .linear
            .down_edges(r)
            .iter()
            .filter(|e| e.edge_type == GraphEdgeType::Collapsed)
            .map(|e| e.down as usize)
            .collect();
        downs
            .into_iter()
            .flat_map(|d| self.collapsed_segments_at_edge(row, d))
            .collect()
    }

    /// Expanded segments `row` belongs to (as middle) or hangs above (as `up`),
    /// in that priority order; `row` is a visible row.
    pub fn expanded_segments_at(&self, row: usize) -> Vec<SegmentId> {
        let Ok(r) = VisibleRowIdx::try_from(row) else {
            return Vec::new();
        };
        if r >= self.visible.linear.visible_count() {
            return Vec::new();
        }
        let node: PermNodeId = self.visible.linear.perm_id(r);
        if let Some(seg) = self.segments.segment_of(node) {
            return vec![seg.id];
        }
        self.permanent
            .linear
            .loaded_down_nodes(node)
            .filter_map(|p| self.segments.segment_below(node, p))
            .filter(|s| self.visible.linear.visible_row(s.first).is_some())
            .map(|s| s.id)
            .collect()
    }

    /// Up end of `id` (where the cursor lands after folding it).
    pub fn segment_up_oid(&self, id: SegmentId) -> Option<Oid> {
        let seg = self.segments.by_id(id)?;
        Some(self.permanent.linear.commit(seg.up).oid)
    }

    /// Expanded collapsible segment containing visible `row` as a middle node, if any.
    pub fn expanded_segment_of_row(&self, row: usize) -> Option<SegmentId> {
        if !self.visible.can_collapse {
            return None;
        }
        let r = VisibleRowIdx::try_from(row).ok()?;
        if r >= self.visible.linear.visible_count() {
            return None;
        }
        let perm = self.visible.linear.perm_id(r);
        self.segments.segment_of(perm).map(|s| s.id)
    }

    /// True if visible `row` is a middle node of expanded collapsible segment `seg_id`.
    pub fn is_row_in_expanded_segment(&self, row: usize, seg_id: SegmentId) -> bool {
        self.expanded_segment_of_row(row) == Some(seg_id)
    }
}

fn compute_has_multiple_authors(snap: &RepoSnapshot) -> bool {
    let mut first: Option<(&str, &str)> = None;
    for c in snap.commits.iter() {
        let key = (c.author_name.as_str(), c.author_email.as_str());
        match first {
            None => first = Some(key),
            Some((n, e)) => {
                if !(e.eq_ignore_ascii_case(key.1) && n == key.0) {
                    return true;
                }
            }
        }
    }
    false
}

/// Linear segments of `permanent`, pinning every ref target and `HEAD`.
fn build_segments(permanent: &PermanentGraph, snap: &RepoSnapshot) -> LinearSegments {
    let pinned = LinearSegments::pinned_nodes(
        &permanent.linear,
        snap.refs.iter().map(|r| r.target_oid),
        snap.head_oid,
    );
    LinearSegments::compute(&permanent.linear, &pinned)
}

fn build_ref_indices(refs: &[RefInfo]) -> (FxHashMap<Oid, Vec<usize>>, FxHashMap<String, RefKind>) {
    let mut by_oid: FxHashMap<Oid, Vec<usize>> = FxHashMap::default();
    let mut by_name: FxHashMap<String, RefKind> = FxHashMap::default();
    by_name.reserve(refs.len());
    for (idx, r) in refs.iter().enumerate() {
        by_oid.entry(r.target_oid).or_default().push(idx);
        by_name.entry(r.name.clone()).or_insert(r.kind);
    }
    (by_oid, by_name)
}

pub fn build_graph_color_map(
    pg: &PermanentGraph,
    mgr: &BranchColorManager,
) -> FxHashMap<BranchColorId, Color> {
    let mut ordered_ids: Vec<BranchColorId> = Vec::new();
    let mut seen: FxHashSet<BranchColorId> = FxHashSet::default();
    let heads = pg.sorted_heads.iter().map(|&head| pg.node_color(head));
    for cid in heads.chain(pg.node_colors.iter().copied()) {
        if seen.insert(cid) {
            ordered_ids.push(cid);
        }
    }
    let colors = mgr.assign_visible(&ordered_ids);
    ordered_ids.into_iter().zip(colors).collect()
}

/// Filter inputs needed to (re)project the visible graph.
pub struct FilterInputs<'a> {
    pub spec: &'a LogFilterSpec,
    pub path_matches: Option<&'a HashSet<Oid>>,
    pub favorites: &'a [String],
}

const HISTORY_LIMIT: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
struct GraphRebuildKey {
    snapshot_ptr: usize,
    filter_generation: u64,
    options: GraphViewOptions,
}

#[derive(Debug, Clone)]
pub struct CachedGraphBase {
    pub permanent: Arc<PermanentGraph>,
    pub segments: Arc<LinearSegments>,
    pub has_multiple_authors: bool,
    pub color_map: FxHashMap<BranchColorId, Color>,
    pub current_branch_reachable: FixedBitSet,
    pub refs_by_oid: FxHashMap<Oid, Vec<usize>>,
    pub ref_kind_by_name: FxHashMap<String, RefKind>,
}

/// Off-UI-thread graph filter + `VisibleGraph` (and, on snapshot change,
/// `PermanentGraph`) build job.
#[derive(Debug, Clone)]
pub struct LogGraphJob {
    pub token: EpochToken,
    pub snapshot: Arc<RepoSnapshot>,
    pub base: Option<CachedGraphBase>,
    pub options: GraphViewOptions,
    pub collapse: CollapseState,
    pub linear_bek: LinearBekState,
    pub filter_spec: LogFilterSpec,
    pub favorites: Vec<String>,
    pub path_matches: Option<Arc<HashSet<Oid>>>,
    pub branch_colors: BranchColorManager,
    pub filter_generation: u64,
}

#[derive(Debug)]
pub struct LogGraphRebuilt {
    pub token: EpochToken,
    pub cached: CachedLogGraph,
    pub filter_error: Option<FilterError>,
}

impl LogGraphJob {
    pub fn run(self) -> Option<LogGraphRebuilt> {
        if self.token.is_stale() {
            return None;
        }
        let base = match self.base {
            Some(b) => b,
            None => {
                let raw_commits: Vec<RawCommitNode> = self
                    .snapshot
                    .commits
                    .iter()
                    .map(RawCommitNode::from)
                    .collect();
                if self.token.is_stale() {
                    return None;
                }
                let permanent = Arc::new(PermanentGraph::build(
                    &raw_commits,
                    &self.snapshot.refs,
                    self.snapshot.head_oid,
                ));
                if self.token.is_stale() {
                    return None;
                }
                let color_map = build_graph_color_map(&permanent, &self.branch_colors);
                let head_nodes: Vec<_> = self
                    .snapshot
                    .head_oid
                    .and_then(|oid| permanent.linear.node_of_oid(&oid))
                    .into_iter()
                    .collect();
                let current_branch_reachable =
                    ReachableNodes::contained_in_branch(&permanent.linear, &head_nodes);
                let (refs_by_oid, ref_kind_by_name) = build_ref_indices(&self.snapshot.refs);
                let segments = Arc::new(build_segments(&permanent, &self.snapshot));
                let has_multiple_authors = compute_has_multiple_authors(&self.snapshot);
                CachedGraphBase {
                    permanent,
                    segments,
                    has_multiple_authors,
                    color_map,
                    current_branch_reachable,
                    refs_by_oid,
                    ref_kind_by_name,
                }
            }
        };

        let branch_only_filter = self.filter_spec.is_branch_only();
        let match_res = self.filter_spec.compute_matches_with_favorites(
            &base.permanent.linear,
            &self.snapshot,
            self.path_matches.as_deref(),
            &self.favorites,
            || self.token.is_stale(),
        )?;
        let (bits, branch_heads, filter_error) = match match_res {
            Ok((bits, heads)) => (bits, heads, None),
            Err(e) => (None, None, Some(e)),
        };
        if self.token.is_stale() {
            return None;
        }
        let visible = VisibleGraph::build_projected(
            &base.permanent,
            self.options,
            VisibleGraphBuildArgs {
                filter: bits.as_ref(),
                branch_heads: branch_heads.as_deref(),
                branch_only_filter: branch_only_filter && filter_error.is_none(),
                collapse: Some(CollapseInput {
                    segments: &base.segments,
                    state: &self.collapse,
                }),
                linear_bek: Some(&self.linear_bek),
            },
        );
        if self.token.is_stale() {
            return None;
        }
        let mut edges_cache = EdgesInRowCache::new(&visible.linear);
        let recommended_tracks = PrintElementGenerator::with_cache(
            &base.permanent,
            &visible,
            self.snapshot.head_oid,
            &mut edges_cache,
        )
        .recommended_track_width();

        Some(LogGraphRebuilt {
            token: self.token,
            cached: CachedLogGraph {
                snapshot: self.snapshot,
                permanent: base.permanent,
                segments: base.segments,
                filtered: bits.is_some(),
                last_filter: bits,
                last_branch_heads: branch_heads,
                branch_only_filter: branch_only_filter && filter_error.is_none(),
                has_multiple_authors: base.has_multiple_authors,
                visible,
                color_map: base.color_map,
                current_branch_reachable: base.current_branch_reachable,
                recommended_tracks,
                edges_cache: RefCell::new(edges_cache),
                refs_by_oid: base.refs_by_oid,
                ref_kind_by_name: base.ref_kind_by_name,
                filter_generation: self.filter_generation,
                #[cfg(test)]
                visible_builds: 1,
            },
            filter_error,
        })
    }
}

/// Selection snapshot captured by OID before a graph rebuild or reload.
struct CapturedSelection {
    cursor_oid: Option<Oid>,
    anchor_oid: Option<Oid>,
    selected_oids: Vec<Oid>,
    mark_mode: bool,
    was_on_load_more: bool,
    prev_visible_count: usize,
    prev_scroll_offset: usize,
}

/// Log-tab graph projection cache, viewport scroll, multi-selection and
/// back/forward navigation history (`MainFrame.myHistory`).
pub struct LogGraphState {
    pub options: GraphViewOptions,
    /// Collapsed linear segments; `options.collapse` mirrors `collapse.mode`.
    pub collapse: CollapseState,
    /// Explicitly expanded merges when `options.linear_bek` is enabled.
    pub linear_bek: LinearBekState,
    pub cached: Option<CachedLogGraph>,
    /// Focused cursor row (`cursor_idx`).
    pub selected_idx: usize,
    /// Explicit multi-selection set (non-contiguous `Ctrl+Click` / `Space` or
    /// contiguous `Shift` range). Empty in implicit single-cursor selection.
    pub selected_rows: BTreeSet<usize>,
    /// Shift-selection anchor (`range_anchor`).
    pub anchor: Option<usize>,
    /// True after keyboard `Space` toggle so `j`/`k`/`Up`/`Down` move the
    /// cursor without clearing marked rows until `Esc` or a plain click.
    mark_mode: bool,
    pub scroll_offset: usize,
    pub last_viewport_h: usize,
    pub back_history: Vec<Oid>,
    pub forward_history: Vec<Oid>,
    /// Bumped whenever filter or collapse inputs change; forces a
    /// visible-graph rebuild.
    pub filter_generation: u64,
    pub filter_error: Option<FilterError>,
    /// Monotonic counter bumped whenever the visible row projection changes.
    pub visible_revision: u64,
    graph_slot: RequestSlot<GraphRebuildKey>,
    pub rebuild_in_progress: bool,
}

impl Default for LogGraphState {
    fn default() -> Self {
        Self {
            options: GraphViewOptions::default(),
            collapse: CollapseState::default(),
            linear_bek: LinearBekState::default(),
            cached: None,
            selected_idx: 0,
            selected_rows: BTreeSet::new(),
            anchor: None,
            mark_mode: false,
            scroll_offset: 0,
            last_viewport_h: 20,
            back_history: Vec::new(),
            forward_history: Vec::new(),
            filter_generation: 0,
            filter_error: None,
            visible_revision: 0,
            graph_slot: RequestSlot::new(),
            rebuild_in_progress: false,
        }
    }
}

impl LogGraphState {
    fn is_stale_for(&self, snap: &Arc<RepoSnapshot>) -> (bool, bool) {
        let snapshot_stale = self
            .cached
            .as_ref()
            .is_none_or(|c| !Arc::ptr_eq(&c.snapshot, snap));
        let visible_stale = self.cached.as_ref().is_none_or(|c| {
            c.filter_generation != self.filter_generation || c.visible.options != self.options
        });
        (snapshot_stale, visible_stale)
    }

    fn preserve_selection_after_rebuild(&mut self, captured: CapturedSelection) {
        self.visible_revision = self.visible_revision.wrapping_add(1);
        let Some(cached) = self.cached.as_ref() else {
            return;
        };
        // A reload can fold a new segment over the selected commit (e.g. the
        // ref that pinned it was deleted): keep it visible.
        if let Some(oid) = captured.cursor_oid
            && let Some(id) = cached.collapsed_segment_hiding(&oid)
        {
            self.collapse.set_collapsed(id, false);
            self.rebuild_collapsed_now();
        }
        let Some(cached) = self.cached.as_ref() else {
            return;
        };
        let len = cached.visible_count();
        let nav_len = self.navigable_row_count();
        if captured.was_on_load_more && len > 0 {
            self.selected_idx = if len > captured.prev_visible_count {
                captured.prev_visible_count.min(len - 1)
            } else {
                nav_len.saturating_sub(1)
            };
            self.scroll_offset = captured.prev_scroll_offset;
        } else {
            match captured.cursor_oid.and_then(|oid| cached.row_of_oid(&oid)) {
                Some(row) => {
                    self.selected_idx = row;
                    self.scroll_offset = captured.prev_scroll_offset;
                }
                None => {
                    self.selected_idx = 0;
                    self.scroll_offset = 0;
                }
            }
        }
        self.anchor = captured.anchor_oid.and_then(|oid| cached.row_of_oid(&oid));
        self.selected_rows.clear();
        for oid in &captured.selected_oids {
            if let Some(row) = cached.row_of_oid(oid) {
                self.selected_rows.insert(row);
            }
        }
        if len == 0 {
            self.selected_idx = 0;
            self.selected_rows.clear();
            self.anchor = None;
            self.mark_mode = false;
        } else if captured.selected_oids.is_empty() {
            self.mark_mode = false;
        } else if self.selected_rows.is_empty() {
            // Every explicitly selected commit disappeared (filtered/rebased out).
            self.mark_mode = false;
            self.anchor = None;
        } else if self.selected_rows.len() == 1
            && !captured.mark_mode
            && self.selected_rows.contains(&self.selected_idx)
        {
            // Non-mark selection shrank to just the cursor row: return to implicit single-select.
            self.selected_rows.clear();
            self.mark_mode = false;
            self.anchor = None;
        } else {
            self.mark_mode = captured.mark_mode;
        }
        self.adjust_scroll(nav_len, false);
    }

    fn capture_selection_oids(&self) -> CapturedSelection {
        let Some(cached) = self.cached.as_ref() else {
            return CapturedSelection {
                cursor_oid: None,
                anchor_oid: None,
                selected_oids: Vec::new(),
                mark_mode: false,
                was_on_load_more: false,
                prev_visible_count: 0,
                prev_scroll_offset: self.scroll_offset,
            };
        };
        let snap = &cached.snapshot;
        let was_on_load_more = self.is_cursor_on_load_more();
        let prev_visible_count = cached.visible_count();
        let cursor_oid = cached
            .commit_at_visible_row(snap, self.selected_idx)
            .map(|c| c.oid);
        let anchor_oid = self
            .anchor
            .and_then(|row| cached.commit_at_visible_row(snap, row).map(|c| c.oid));
        let selected_oids = self
            .selected_rows
            .iter()
            .filter_map(|&row| cached.commit_at_visible_row(snap, row).map(|c| c.oid))
            .collect();
        CapturedSelection {
            cursor_oid,
            anchor_oid,
            selected_oids,
            mark_mode: self.mark_mode,
            was_on_load_more,
            prev_visible_count,
            prev_scroll_offset: self.scroll_offset,
        }
    }

    /// Ensures the permanent graph matches `repo_snapshot` and the visible
    /// projection matches the current options / filter generation, keeping the
    /// selected commit(s) selected when they survive the re-projection.
    pub fn ensure_cache(
        &mut self,
        repo_snapshot: Option<&Arc<RepoSnapshot>>,
        branch_colors: &BranchColorManager,
        filter: &FilterInputs<'_>,
    ) {
        let Some(snap) = repo_snapshot else {
            if self.cached.is_some() {
                self.visible_revision = self.visible_revision.wrapping_add(1);
            }
            self.cached = None;
            self.graph_slot.invalidate();
            self.rebuild_in_progress = false;
            return;
        };
        let (snapshot_stale, visible_stale) = self.is_stale_for(snap);
        if !snapshot_stale && !visible_stale {
            return;
        }

        let captured = self.capture_selection_oids();

        let branch_only_filter = filter.spec.is_branch_only();

        if snapshot_stale {
            // Fresh snapshot: compute the filter against the new permanent node ids
            // and project the visible graph once (no throw-away unfiltered build).
            let mut filter_result = None;
            self.cached = Some(CachedLogGraph::build_with(
                snap,
                self.options,
                &self.collapse,
                &self.linear_bek,
                branch_only_filter,
                branch_colors,
                |linear| {
                    let res = filter
                        .spec
                        .compute_matches_with_favorites(
                            linear,
                            snap,
                            filter.path_matches,
                            filter.favorites,
                            || false,
                        )
                        .expect("uncancellable sync match");
                    let (bits, heads) = match &res {
                        Ok((b, h)) => (b.clone(), h.clone()),
                        Err(_) => (None, None),
                    };
                    filter_result = Some(res);
                    (bits, heads)
                },
            ));
            self.filter_error = filter_result.and_then(Result::err);
        } else {
            let cached = self.cached.as_mut().expect("cached graph exists");
            match filter
                .spec
                .compute_matches_with_favorites(
                    &cached.permanent.linear,
                    snap,
                    filter.path_matches,
                    filter.favorites,
                    || false,
                )
                .expect("uncancellable sync match")
            {
                Ok((bits, heads)) => {
                    self.filter_error = None;
                    cached.rebuild_visible(
                        self.options,
                        bits,
                        heads,
                        branch_only_filter,
                        &self.collapse,
                        &self.linear_bek,
                    );
                }
                Err(e) => {
                    self.filter_error = Some(e);
                    cached.rebuild_visible(
                        self.options,
                        None,
                        None,
                        false,
                        &self.collapse,
                        &self.linear_bek,
                    );
                }
            }
        }
        let cached = self.cached.as_mut().expect("cache built above");
        cached.filter_generation = self.filter_generation;
        self.graph_slot.invalidate();
        self.rebuild_in_progress = false;

        self.preserve_selection_after_rebuild(captured);
    }

    /// Issues an off-thread [`LogGraphJob`] when the snapshot has at least
    /// [`ASYNC_GRAPH_THRESHOLD`] commits and the cache is stale relative to
    /// what is currently in flight.
    pub fn take_async_rebuild_job(
        &mut self,
        repo_snapshot: Option<&Arc<RepoSnapshot>>,
        branch_colors: &BranchColorManager,
        spec: &LogFilterSpec,
        favorites: Vec<String>,
        path_matches: Option<Arc<HashSet<Oid>>>,
    ) -> Option<LogGraphJob> {
        let snap = repo_snapshot?;
        let (snapshot_stale, visible_stale) = self.is_stale_for(snap);
        if !snapshot_stale && !visible_stale {
            return None;
        }
        let key = GraphRebuildKey {
            snapshot_ptr: Arc::as_ptr(snap) as usize,
            filter_generation: self.filter_generation,
            options: self.options,
        };
        let token = self.graph_slot.begin_token(key)?;
        let base = if !snapshot_stale {
            self.cached.as_ref().map(|c| CachedGraphBase {
                permanent: Arc::clone(&c.permanent),
                segments: Arc::clone(&c.segments),
                has_multiple_authors: c.has_multiple_authors,
                color_map: c.color_map.clone(),
                current_branch_reachable: c.current_branch_reachable.clone(),
                refs_by_oid: c.refs_by_oid.clone(),
                ref_kind_by_name: c.ref_kind_by_name.clone(),
            })
        } else {
            None
        };
        self.rebuild_in_progress = true;
        Some(LogGraphJob {
            token,
            snapshot: Arc::clone(snap),
            base,
            options: self.options,
            collapse: self.collapse.clone(),
            linear_bek: self.linear_bek.clone(),
            filter_spec: spec.clone(),
            favorites,
            path_matches,
            branch_colors: branch_colors.clone(),
            filter_generation: self.filter_generation,
        })
    }

    /// Installs an off-thread [`LogGraphRebuilt`] if its token has not been superseded.
    pub fn apply_rebuilt(&mut self, rebuilt: LogGraphRebuilt) -> bool {
        if rebuilt.token.is_stale() {
            return false;
        }
        self.graph_slot.clear_in_flight();
        self.rebuild_in_progress = false;
        let captured = self.capture_selection_oids();
        self.cached = Some(rebuilt.cached);
        self.filter_error = rebuilt.filter_error;
        self.preserve_selection_after_rebuild(captured);
        true
    }

    pub fn on_snapshot_loaded(&mut self) {
        // Keep selection by oid (captured from the previous `cached` during
        // `ensure_cache` / `apply_rebuilt`); only drop row-based state when no
        // previous cache exists.
        if self.cached.is_none() {
            self.selected_rows.clear();
            self.anchor = None;
            self.mark_mode = false;
        }
    }

    pub fn visible_count(&self) -> usize {
        self.cached
            .as_ref()
            .map_or(0, CachedLogGraph::visible_count)
    }

    /// True when the Log table renders a navigable virtual `[Load More]` tail row
    /// immediately below the last visible commit row.
    pub fn has_load_more_row(&self) -> bool {
        self.cached
            .as_ref()
            .is_some_and(|c| c.snapshot.has_more_commits && c.visible_count() > 0)
    }

    /// True when `row` refers to the virtual `[Load More]` tail row.
    pub fn is_load_more_row(&self, row: usize) -> bool {
        self.has_load_more_row() && row == self.visible_count()
    }

    /// True when the Log table cursor currently sits on the virtual `[Load More]` row.
    pub fn is_cursor_on_load_more(&self) -> bool {
        self.is_load_more_row(self.selected_idx)
    }

    /// Total keyboard-navigable rows (real visible commits + optional virtual tail row).
    pub fn navigable_row_count(&self) -> usize {
        self.visible_count() + usize::from(self.has_load_more_row())
    }

    pub fn commit_at<'a>(
        &self,
        repo_snapshot: Option<&'a Arc<RepoSnapshot>>,
        visible_row: usize,
    ) -> Option<&'a CommitSummary> {
        let snap = repo_snapshot?;
        match &self.cached {
            Some(cached) => cached.commit_at_visible_row(snap, visible_row),
            None => snap.commits.get(visible_row),
        }
    }

    pub fn selected_oid(&self, repo_snapshot: Option<&Arc<RepoSnapshot>>) -> Option<Oid> {
        self.commit_at(repo_snapshot, self.selected_idx)
            .map(|c| c.oid)
    }

    /// Bounding range of selected visible rows (for contiguous ranges and legacy checks).
    pub fn selected_range(&self) -> std::ops::RangeInclusive<usize> {
        if let (Some(&first), Some(&last)) = (self.selected_rows.first(), self.selected_rows.last())
        {
            first..=last
        } else {
            self.selected_idx..=self.selected_idx
        }
    }

    /// Selected visible rows in ascending (top-to-bottom) order.
    pub fn selected_rows_vec(&self) -> Vec<usize> {
        let len = self.visible_count();
        if len == 0 {
            return Vec::new();
        }
        if self.selected_rows.is_empty() {
            if self.is_cursor_on_load_more() {
                Vec::new()
            } else {
                vec![self.selected_idx.min(len - 1)]
            }
        } else {
            self.selected_rows
                .iter()
                .copied()
                .filter(|&r| r < len)
                .collect()
        }
    }

    pub fn selection_count(&self) -> usize {
        let len = self.visible_count();
        if len == 0 {
            0
        } else if self.selected_rows.is_empty() {
            usize::from(!self.is_cursor_on_load_more())
        } else {
            self.selected_rows.iter().filter(|&&r| r < len).count()
        }
    }

    /// True when explicit multi-selection (more than one row or keyboard mark mode) is active.
    pub fn has_multi_selection(&self) -> bool {
        self.selection_count() > 1 || self.mark_mode || !self.selected_rows.is_empty()
    }

    pub fn is_mark_mode(&self) -> bool {
        self.mark_mode
    }

    pub fn is_row_selected(&self, row: usize) -> bool {
        let len = self.visible_count();
        if len == 0 || row >= len {
            return false;
        }
        if self.selected_rows.is_empty() {
            !self.is_cursor_on_load_more() && row == self.selected_idx
        } else {
            self.selected_rows.contains(&row)
        }
    }

    /// Selected commit oids in visible (top-to-bottom) order.
    pub fn selected_oids(&self, repo_snapshot: Option<&Arc<RepoSnapshot>>) -> Vec<Oid> {
        self.selected_rows_vec()
            .into_iter()
            .filter_map(|row| self.commit_at(repo_snapshot, row).map(|c| c.oid))
            .collect()
    }

    /// Collapses any multi-selection back to single-selecting the current cursor row.
    pub fn clear_multi_selection(&mut self) {
        self.selected_rows.clear();
        self.anchor = None;
        self.mark_mode = false;
    }

    pub fn select_row(&mut self, idx: usize) {
        let nav_len = self.navigable_row_count();
        if nav_len == 0 {
            return;
        }
        self.selected_rows.clear();
        self.anchor = None;
        self.mark_mode = false;
        self.selected_idx = idx.min(nav_len - 1);
        self.adjust_scroll(nav_len, false);
    }

    /// Extends a contiguous range selection from `anchor` to `idx`, replacing prior selection.
    pub fn extend_selection_to(&mut self, idx: usize) {
        let len = self.visible_count();
        if len == 0 {
            return;
        }
        let target = idx.min(len - 1);
        let anchor = *self.anchor.get_or_insert(self.selected_idx.min(len - 1));
        self.selected_idx = target;
        self.mark_mode = false;
        self.selected_rows.clear();
        for r in anchor.min(target)..=anchor.max(target) {
            self.selected_rows.insert(r);
        }
        self.adjust_scroll(len, false);
    }

    /// Adds a contiguous range `anchor..=idx` into the existing selection (`Ctrl+Shift+Click`).
    pub fn add_range_selection_to(&mut self, idx: usize) {
        let len = self.visible_count();
        if len == 0 {
            return;
        }
        let target = idx.min(len - 1);
        let anchor = *self.anchor.get_or_insert(self.selected_idx.min(len - 1));
        if self.selected_rows.is_empty() {
            self.selected_rows.insert(self.selected_idx.min(len - 1));
        }
        for r in anchor.min(target)..=anchor.max(target) {
            self.selected_rows.insert(r);
        }
        self.selected_idx = target;
        self.adjust_scroll(len, false);
    }

    /// Toggles `idx` in the non-contiguous selection set (`Ctrl+Click` when
    /// `sticky == false`, keyboard `Space` when `sticky == true`).
    pub fn toggle_row_selection(&mut self, idx: usize, sticky: bool) {
        let len = self.visible_count();
        if len == 0 || idx >= len {
            return;
        }
        let target = idx;
        let on_load_more = self.is_cursor_on_load_more();
        let cur = self.selected_idx.min(len - 1);
        if self.selected_rows.is_empty() {
            if !sticky && !on_load_more && target == cur {
                // Ctrl+Click on the already-focused single row keeps clean single selection.
                return;
            }
            if !on_load_more && target != cur {
                self.selected_rows.insert(cur);
            }
            self.selected_rows.insert(target);
            self.mark_mode = sticky;
        } else {
            if !self.selected_rows.remove(&target) {
                self.selected_rows.insert(target);
            }
            if self.selected_rows.is_empty() {
                self.mark_mode = false;
                self.anchor = None;
            } else if sticky {
                self.mark_mode = true;
            }
        }
        self.selected_idx = target;
        if !self.selected_rows.is_empty() {
            self.anchor = Some(target);
        }
        self.adjust_scroll(len, false);
    }

    /// Moves only the focused cursor row without clearing multi-selection (`Ctrl+Up/Down`).
    pub fn move_cursor_only(&mut self, delta: isize) {
        let len = self.visible_count();
        if len == 0 {
            return;
        }
        self.selected_idx =
            (self.selected_idx as isize + delta).clamp(0, (len - 1) as isize) as usize;
        self.adjust_scroll(len, false);
    }

    pub fn scroll_by(&mut self, delta: isize, extend: bool) {
        if extend {
            let len = self.visible_count();
            if len == 0 {
                return;
            }
            let cur = self.selected_idx.min(len - 1);
            let next = (cur as isize + delta).clamp(0, (len - 1) as isize) as usize;
            self.extend_selection_to(next);
            return;
        }
        let nav_len = self.navigable_row_count();
        if nav_len == 0 {
            return;
        }
        let next = (self.selected_idx as isize + delta).clamp(0, (nav_len - 1) as isize) as usize;
        if self.mark_mode {
            let len = self.visible_count();
            self.selected_idx = next.min(len.saturating_sub(1));
            self.adjust_scroll(nav_len, false);
        } else {
            self.select_row(next);
        }
    }

    /// Steps single-commit selection by `delta`, clamped to real visible commits
    /// (never landing on the virtual `[Load More]` tail row).
    /// Returns `true` when the active commit selection changed.
    pub fn step_commit(&mut self, delta: isize) -> bool {
        let len = self.visible_count();
        if len == 0 || (delta >= 0 && self.is_cursor_on_load_more()) {
            return false;
        }
        let next = (self.selected_idx as isize + delta).clamp(0, (len - 1) as isize) as usize;
        if next == self.selected_idx && !self.has_multi_selection() {
            return false;
        }
        self.select_row(next);
        true
    }

    fn push_history(&mut self, repo_snapshot: Option<&Arc<RepoSnapshot>>) {
        if let Some(oid) = self.selected_oid(repo_snapshot) {
            if self.back_history.last() != Some(&oid) {
                self.back_history.push(oid);
            }
            if self.back_history.len() > HISTORY_LIMIT {
                self.back_history.remove(0);
            }
        }
    }

    /// Navigation jump (records history, optional centering).
    pub fn jump_to(&mut self, repo_snapshot: Option<&Arc<RepoSnapshot>>, row: usize, center: bool) {
        let nav_len = self.navigable_row_count();
        if nav_len == 0 {
            return;
        }
        let target = row.min(nav_len - 1);
        if target != self.selected_idx {
            self.push_history(repo_snapshot);
            self.forward_history.clear();
        }
        self.selected_rows.clear();
        self.anchor = None;
        self.mark_mode = false;
        self.selected_idx = target;
        self.adjust_scroll(nav_len, center);
    }

    /// Visible row of `oid`, first expanding a collapsed segment that hides it.
    fn row_of_oid_revealing(&mut self, oid: &Oid) -> Option<usize> {
        if let Some(id) = self
            .cached
            .as_ref()
            .and_then(|c| c.collapsed_segment_hiding(oid))
        {
            self.set_segments_collapsed(&[id], false);
        }
        self.cached.as_ref().and_then(|c| c.row_of_oid(oid))
    }

    /// Re-projects the cached graph synchronously after a collapse or LinearBek change.
    fn rebuild_collapsed_now(&mut self) {
        self.options.collapse = self.collapse.mode;
        let previous = self.filter_generation;
        self.filter_generation = self.filter_generation.wrapping_add(1);
        let Some(cached) = self.cached.as_mut() else {
            return;
        };
        if cached.filter_generation != previous {
            return;
        }
        cached.rebuild_visible_keeping_filter(self.options, &self.collapse, &self.linear_bek);
        cached.filter_generation = self.filter_generation;
        self.graph_slot.invalidate();
        self.rebuild_in_progress = false;
    }

    /// Expands a single folded `LinearBek` merge commit, restoring its original merge edge.
    pub fn expand_bek_merge(&mut self, merge_oid: Oid) -> bool {
        if !self.options.linear_bek {
            return false;
        }
        let captured = self.capture_selection_oids();
        if !self.linear_bek.expand(merge_oid) {
            return false;
        }
        self.rebuild_collapsed_now();
        self.preserve_selection_after_rebuild(captured);
        true
    }

    /// Collapses (`true`) or expands (`false`) `ids`, keeping the selected
    /// commit(s) (and the shift anchor) selected and visible.
    pub fn set_segments_collapsed(&mut self, ids: &[SegmentId], collapsed: bool) {
        if ids.is_empty() {
            return;
        }
        let captured = self.capture_selection_oids();
        for &id in ids {
            self.collapse.set_collapsed(id, collapsed);
        }
        self.rebuild_collapsed_now();
        self.preserve_selection_after_rebuild(captured);
    }

    /// Folds `id` and moves the cursor to its up end when the selected commit
    /// sits inside it (vim `zc` semantics).
    pub fn collapse_segment_moving_cursor(&mut self, id: SegmentId) {
        let mut captured = self.capture_selection_oids();
        let inside = captured.cursor_oid.is_some_and(|oid| {
            self.cached.as_ref().is_some_and(|c| {
                c.permanent
                    .linear
                    .node_of_oid(&oid)
                    .and_then(|n| c.segments.segment_of(n))
                    .is_some_and(|s| s.id == id)
            })
        });
        let up = self.cached.as_ref().and_then(|c| c.segment_up_oid(id));
        self.collapse.set_collapsed(id, true);
        self.rebuild_collapsed_now();
        if inside {
            captured.cursor_oid = up;
        }
        captured.anchor_oid = None;
        self.preserve_selection_after_rebuild(captured);
    }

    /// Collapse All Linear Branches (and re-fold all LinearBek merges when active).
    pub fn collapse_all(&mut self) {
        let captured = self.capture_selection_oids();
        if self.options.linear_bek {
            self.linear_bek.collapse_all();
        }
        self.collapse.collapse_all();
        if let Some(cached) = self.cached.as_ref() {
            for oid in [captured.cursor_oid, captured.anchor_oid]
                .into_iter()
                .flatten()
                .chain(captured.selected_oids.iter().copied())
            {
                if let Some(seg) = cached
                    .permanent
                    .linear
                    .node_of_oid(&oid)
                    .and_then(|n| cached.segments.segment_of(n))
                {
                    self.collapse.set_collapsed(seg.id, false);
                }
            }
        }
        self.rebuild_collapsed_now();
        self.preserve_selection_after_rebuild(captured);
    }

    /// Expand All Linear Branches (and expand all LinearBek merges when active).
    pub fn expand_all(&mut self) {
        let captured = self.capture_selection_oids();
        if self.options.linear_bek
            && let Some(cached) = self.cached.as_ref()
        {
            self.linear_bek.expand_all(&cached.visible.bek_merges);
        }
        self.collapse.expand_all();
        self.rebuild_collapsed_now();
        self.preserve_selection_after_rebuild(captured);
    }

    /// Jumps to `oid` if it is visible; returns `false` when filtered out / unknown.
    /// A collapsed segment hiding `oid` is expanded first.
    pub fn jump_to_oid(&mut self, repo_snapshot: Option<&Arc<RepoSnapshot>>, oid: &Oid) -> bool {
        let Some(row) = self.row_of_oid_revealing(oid) else {
            return false;
        };
        self.jump_to(repo_snapshot, row, true);
        true
    }

    pub fn nav_back(&mut self, repo_snapshot: Option<&Arc<RepoSnapshot>>) -> bool {
        self.nav_history_step(repo_snapshot, true)
    }

    pub fn nav_forward(&mut self, repo_snapshot: Option<&Arc<RepoSnapshot>>) -> bool {
        self.nav_history_step(repo_snapshot, false)
    }

    fn nav_history_step(&mut self, repo_snapshot: Option<&Arc<RepoSnapshot>>, back: bool) -> bool {
        let len = self.visible_count();
        loop {
            let popped = if back {
                self.back_history.pop()
            } else {
                self.forward_history.pop()
            };
            let Some(oid) = popped else {
                return false;
            };
            let Some(row) = self.row_of_oid_revealing(&oid) else {
                continue; // hidden by current filters; skip
            };
            if let Some(cur) = self.selected_oid(repo_snapshot) {
                if back {
                    self.forward_history.push(cur);
                } else {
                    self.back_history.push(cur);
                }
            }
            self.selected_rows.clear();
            self.anchor = None;
            self.mark_mode = false;
            self.selected_idx = row;
            self.adjust_scroll(len, true);
            return true;
        }
    }

    /// `GoToParentRow`: visible row of `parent[parent_idx]` of the selected
    /// commit, falling back to the nearest visible down-edge when filtered.
    pub fn parent_row(
        &self,
        repo_snapshot: Option<&Arc<RepoSnapshot>>,
        parent_idx: usize,
    ) -> Option<usize> {
        let cached = self.cached.as_ref()?;
        let commit = self.commit_at(repo_snapshot, self.selected_idx)?;
        if let Some(row) = commit
            .parents
            .get(parent_idx)
            .and_then(|p| cached.row_of_oid(p))
        {
            return Some(row);
        }
        if parent_idx > 0 {
            return None;
        }
        let row = VisibleRowIdx::try_from(self.selected_idx).ok()?;
        cached
            .visible
            .linear
            .down_edges(row)
            .iter()
            .map(|e| e.down as usize)
            .min()
    }

    /// Visible parent candidates of the currently selected commit in parent order
    /// (falling back to visible down-edges when filtered).
    pub fn parent_candidates(
        &self,
        repo_snapshot: Option<&Arc<RepoSnapshot>>,
    ) -> Vec<(usize, Oid)> {
        let Some(cached) = self.cached.as_ref() else {
            return Vec::new();
        };
        let Some(snap) = repo_snapshot else {
            return Vec::new();
        };
        let Some(commit) = self.commit_at(repo_snapshot, self.selected_idx) else {
            return Vec::new();
        };
        let mut out: Vec<(usize, Oid)> = Vec::new();
        for &poid in &commit.parents {
            if let Some(row) = cached.row_of_oid(&poid)
                && !out.iter().any(|&(r, _)| r == row)
            {
                out.push((row, poid));
            }
        }
        if out.is_empty()
            && let Ok(row) = VisibleRowIdx::try_from(self.selected_idx)
            && row < cached.visible.linear.visible_count()
        {
            for e in cached.visible.linear.down_edges(row) {
                let r = e.down as usize;
                if let Some(c) = cached.commit_at_visible_row(snap, r)
                    && !out.iter().any(|&(er, _)| er == r)
                {
                    out.push((r, c.oid));
                }
            }
        }
        out
    }

    /// Visible child candidates above the currently selected commit, ordered closest-first.
    pub fn child_candidates(&self, repo_snapshot: Option<&Arc<RepoSnapshot>>) -> Vec<(usize, Oid)> {
        let Some(cached) = self.cached.as_ref() else {
            return Vec::new();
        };
        let Some(snap) = repo_snapshot else {
            return Vec::new();
        };
        let Ok(row) = VisibleRowIdx::try_from(self.selected_idx) else {
            return Vec::new();
        };
        if row >= cached.visible.linear.visible_count() {
            return Vec::new();
        }
        let mut rows: Vec<usize> = cached
            .visible
            .linear
            .up_edges(row)
            .iter()
            .map(|e| e.up as usize)
            .collect();
        // Closest visible child (largest row index below cursor) first.
        rows.sort_unstable_by(|a, b| b.cmp(a));
        rows.dedup();
        rows.into_iter()
            .filter_map(|r| cached.commit_at_visible_row(snap, r).map(|c| (r, c.oid)))
            .collect()
    }

    /// `GoToChildRow`: the closest visible child above the selected row.
    pub fn child_row(&self) -> Option<usize> {
        let cached = self.cached.as_ref()?;
        let row = VisibleRowIdx::try_from(self.selected_idx).ok()?;
        if row >= cached.visible.linear.visible_count() {
            return None;
        }
        cached
            .visible
            .linear
            .up_edges(row)
            .iter()
            .map(|e| e.up as usize)
            .max()
    }

    pub fn adjust_scroll(&mut self, total: usize, center: bool) {
        let vh = self.last_viewport_h.max(1);
        let total = total.max(self.navigable_row_count());
        let mut vp = crate::app::ScrollViewport::new();
        vp.set_offset(self.scroll_offset);
        if total == 0 {
            self.scroll_offset = 0;
            return;
        }
        if center {
            vp.bind_geometry(total, vh);
            vp.set_offset(self.selected_idx.saturating_sub(vh / 2));
        } else {
            vp.ensure_visible(self.selected_idx, total, vh);
        }
        self.scroll_offset = vp.offset();
    }
}
