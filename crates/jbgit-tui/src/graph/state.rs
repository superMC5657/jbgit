use std::cell::RefCell;
use std::collections::HashSet;
use std::sync::Arc;

use jbgit_core::graph::{
    EdgesInRowCache, FixedBitSet, GraphViewOptions, PermanentGraph, PermanentLinearGraph,
    PrintElementGenerator, RawCommitNode, ReachableNodes, VisibleGraph, VisibleRowIdx,
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
    pub visible: VisibleGraph,
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
        Self::build_with(snap, options, branch_colors, |_| filter.cloned())
    }

    /// Builds the permanent graph, then asks `select` for the visible-node
    /// filter (which usually needs the permanent node ids) and projects the
    /// visible graph exactly once.
    pub fn build_with<F>(
        snap: &Arc<RepoSnapshot>,
        options: GraphViewOptions,
        branch_colors: &BranchColorManager,
        select: F,
    ) -> Self
    where
        F: FnOnce(&PermanentLinearGraph) -> Option<FixedBitSet>,
    {
        let raw_commits: Vec<RawCommitNode> =
            snap.commits.iter().map(RawCommitNode::from).collect();
        let permanent = Arc::new(PermanentGraph::build(&raw_commits, &snap.refs, snap.head_oid));
        let color_map = build_graph_color_map(&permanent, branch_colors);
        let head_nodes: Vec<_> = snap
            .head_oid
            .and_then(|oid| permanent.linear.node_of_oid(&oid))
            .into_iter()
            .collect();
        let current_branch_reachable =
            ReachableNodes::contained_in_branch(&permanent.linear, &head_nodes);
        let filter = select(&permanent.linear);
        let visible = VisibleGraph::build(&permanent, options, filter.as_ref());
        let mut edges_cache = EdgesInRowCache::new(&visible.linear);
        let recommended_tracks =
            PrintElementGenerator::with_cache(&permanent, &visible, snap.head_oid, &mut edges_cache)
                .recommended_track_width();
        let (refs_by_oid, ref_kind_by_name) = build_ref_indices(&snap.refs);

        Self {
            snapshot: Arc::clone(snap),
            permanent,
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

    fn rebuild_visible(&mut self, options: GraphViewOptions, filter: Option<&FixedBitSet>) {
        self.visible = VisibleGraph::build(&self.permanent, options, filter);
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
}

fn build_ref_indices(
    refs: &[RefInfo],
) -> (FxHashMap<Oid, Vec<usize>>, FxHashMap<String, RefKind>) {
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
    pub filter_spec: LogFilterSpec,
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
                let raw_commits: Vec<RawCommitNode> =
                    self.snapshot.commits.iter().map(RawCommitNode::from).collect();
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
                CachedGraphBase {
                    permanent,
                    color_map,
                    current_branch_reachable,
                    refs_by_oid,
                    ref_kind_by_name,
                }
            }
        };

        let match_res = self.filter_spec.compute_matches_cancellable(
            &base.permanent.linear,
            &self.snapshot,
            self.path_matches.as_deref(),
            || self.token.is_stale(),
        )?;
        let (bits, filter_error) = match match_res {
            Ok(bits) => (bits, None),
            Err(e) => (None, Some(e)),
        };
        if self.token.is_stale() {
            return None;
        }
        let visible = VisibleGraph::build(&base.permanent, self.options, bits.as_ref());
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

/// Log-tab graph projection cache, viewport scroll, multi-selection and
/// back/forward navigation history (`MainFrame.myHistory`).
pub struct LogGraphState {
    pub options: GraphViewOptions,
    pub cached: Option<CachedLogGraph>,
    pub selected_idx: usize,
    /// Shift-selection anchor; the selection is `anchor..=selected_idx` (either order).
    pub anchor: Option<usize>,
    pub scroll_offset: usize,
    pub last_viewport_h: usize,
    pub back_history: Vec<Oid>,
    pub forward_history: Vec<Oid>,
    /// Bumped whenever filter inputs change; forces a visible-graph rebuild.
    pub filter_generation: u64,
    pub filter_error: Option<FilterError>,
    graph_slot: RequestSlot<GraphRebuildKey>,
    pub rebuild_in_progress: bool,
}

impl Default for LogGraphState {
    fn default() -> Self {
        Self {
            options: GraphViewOptions::default(),
            cached: None,
            selected_idx: 0,
            anchor: None,
            scroll_offset: 0,
            last_viewport_h: 20,
            back_history: Vec::new(),
            forward_history: Vec::new(),
            filter_generation: 0,
            filter_error: None,
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

    fn preserve_selection_after_rebuild(
        &mut self,
        selected_oid: Option<Oid>,
        anchor_oid: Option<Oid>,
    ) {
        let Some(cached) = self.cached.as_ref() else {
            return;
        };
        let len = cached.visible_count();
        match selected_oid.and_then(|oid| cached.row_of_oid(&oid)) {
            Some(row) => self.selected_idx = row,
            None => {
                self.selected_idx = 0;
                self.scroll_offset = 0;
            }
        }
        self.anchor = anchor_oid.and_then(|oid| cached.row_of_oid(&oid));
        if len == 0 {
            self.selected_idx = 0;
            self.anchor = None;
        }
        self.adjust_scroll(len, false);
    }

    fn capture_selection_oids(&self) -> (Option<Oid>, Option<Oid>) {
        let Some(cached) = self.cached.as_ref() else {
            return (None, None);
        };
        let snap = &cached.snapshot;
        let selected_oid = cached
            .commit_at_visible_row(snap, self.selected_idx)
            .map(|c| c.oid);
        let anchor_oid = self
            .anchor
            .and_then(|row| cached.commit_at_visible_row(snap, row).map(|c| c.oid));
        (selected_oid, anchor_oid)
    }

    /// Ensures the permanent graph matches `repo_snapshot` and the visible
    /// projection matches the current options / filter generation, keeping the
    /// selected commit selected when it survives the re-projection.
    pub fn ensure_cache(
        &mut self,
        repo_snapshot: Option<&Arc<RepoSnapshot>>,
        branch_colors: &BranchColorManager,
        filter: &FilterInputs<'_>,
    ) {
        let Some(snap) = repo_snapshot else {
            self.cached = None;
            self.graph_slot.invalidate();
            self.rebuild_in_progress = false;
            return;
        };
        let (snapshot_stale, visible_stale) = self.is_stale_for(snap);
        if !snapshot_stale && !visible_stale {
            return;
        }

        let (selected_oid, anchor_oid) = self.capture_selection_oids();

        if snapshot_stale {
            // Fresh snapshot: compute the filter against the new permanent node ids
            // and project the visible graph once (no throw-away unfiltered build).
            let mut filter_result = None;
            self.cached = Some(CachedLogGraph::build_with(
                snap,
                self.options,
                branch_colors,
                |linear| {
                    let res = filter.spec.compute_matches(linear, snap, filter.path_matches);
                    let bits = res.as_ref().ok().cloned().flatten();
                    filter_result = Some(res);
                    bits
                },
            ));
            self.filter_error = filter_result.and_then(Result::err);
        } else {
            let cached = self.cached.as_mut().expect("cached graph exists");
            match filter
                .spec
                .compute_matches(&cached.permanent.linear, snap, filter.path_matches)
            {
                Ok(bits) => {
                    self.filter_error = None;
                    cached.rebuild_visible(self.options, bits.as_ref());
                }
                Err(e) => {
                    self.filter_error = Some(e);
                    cached.rebuild_visible(self.options, None);
                }
            }
        }
        let cached = self.cached.as_mut().expect("cache built above");
        cached.filter_generation = self.filter_generation;
        self.graph_slot.invalidate();
        self.rebuild_in_progress = false;

        self.preserve_selection_after_rebuild(selected_oid, anchor_oid);
    }

    /// Issues an off-thread [`LogGraphJob`] when the snapshot has at least
    /// [`ASYNC_GRAPH_THRESHOLD`] commits and the cache is stale relative to
    /// what is currently in flight.
    pub fn take_async_rebuild_job(
        &mut self,
        repo_snapshot: Option<&Arc<RepoSnapshot>>,
        branch_colors: &BranchColorManager,
        spec: &LogFilterSpec,
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
            filter_spec: spec.clone(),
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
        let (selected_oid, anchor_oid) = self.capture_selection_oids();
        self.cached = Some(rebuilt.cached);
        self.filter_error = rebuilt.filter_error;
        self.preserve_selection_after_rebuild(selected_oid, anchor_oid);
        true
    }

    pub fn on_snapshot_loaded(&mut self) {
        // Keep selection by oid (handled in ensure_cache); drop row-based state.
        self.anchor = None;
    }

    pub fn visible_count(&self) -> usize {
        self.cached
            .as_ref()
            .map_or(0, CachedLogGraph::visible_count)
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

    /// Selected visible rows, top to bottom.
    pub fn selected_range(&self) -> std::ops::RangeInclusive<usize> {
        match self.anchor {
            Some(a) => a.min(self.selected_idx)..=a.max(self.selected_idx),
            None => self.selected_idx..=self.selected_idx,
        }
    }

    pub fn is_row_selected(&self, row: usize) -> bool {
        self.selected_range().contains(&row)
    }

    /// Selected commit oids in visible (top-to-bottom) order.
    pub fn selected_oids(&self, repo_snapshot: Option<&Arc<RepoSnapshot>>) -> Vec<Oid> {
        if self.visible_count() == 0 {
            return Vec::new();
        }
        self.selected_range()
            .filter_map(|row| self.commit_at(repo_snapshot, row).map(|c| c.oid))
            .collect()
    }

    pub fn select_row(&mut self, idx: usize) {
        let len = self.visible_count();
        if len == 0 {
            return;
        }
        self.anchor = None;
        self.selected_idx = idx.min(len - 1);
        self.adjust_scroll(len, false);
    }

    /// Extends the selection to `idx` keeping (or creating) the anchor.
    pub fn extend_selection_to(&mut self, idx: usize) {
        let len = self.visible_count();
        if len == 0 {
            return;
        }
        if self.anchor.is_none() {
            self.anchor = Some(self.selected_idx);
        }
        self.selected_idx = idx.min(len - 1);
        self.adjust_scroll(len, false);
    }

    pub fn scroll_by(&mut self, delta: isize, extend: bool) {
        let len = self.visible_count();
        if len == 0 {
            return;
        }
        let next = (self.selected_idx as isize + delta).clamp(0, (len - 1) as isize) as usize;
        if extend {
            self.extend_selection_to(next);
        } else {
            self.select_row(next);
        }
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
        let len = self.visible_count();
        if len == 0 {
            return;
        }
        let target = row.min(len - 1);
        if target != self.selected_idx {
            self.push_history(repo_snapshot);
            self.forward_history.clear();
        }
        self.anchor = None;
        self.selected_idx = target;
        self.adjust_scroll(len, center);
    }

    /// Jumps to `oid` if it is visible; returns `false` when filtered out / unknown.
    pub fn jump_to_oid(&mut self, repo_snapshot: Option<&Arc<RepoSnapshot>>, oid: &Oid) -> bool {
        let Some(row) = self.cached.as_ref().and_then(|c| c.row_of_oid(oid)) else {
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
            let Some(row) = self.cached.as_ref().and_then(|c| c.row_of_oid(&oid)) else {
                continue; // hidden by current filters; skip
            };
            if let Some(cur) = self.selected_oid(repo_snapshot) {
                if back {
                    self.forward_history.push(cur);
                } else {
                    self.back_history.push(cur);
                }
            }
            self.anchor = None;
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
        if total == 0 {
            self.scroll_offset = 0;
            return;
        }
        let max_off = total.saturating_sub(vh);
        if center {
            self.scroll_offset = self.selected_idx.saturating_sub(vh / 2).min(max_off);
        } else {
            if self.selected_idx < self.scroll_offset {
                self.scroll_offset = self.selected_idx;
            } else if self.selected_idx >= self.scroll_offset.saturating_add(vh) {
                self.scroll_offset = self.selected_idx + 1 - vh;
            }
            self.scroll_offset = self.scroll_offset.min(max_off);
        }
    }
}
