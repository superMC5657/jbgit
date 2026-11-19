use std::collections::HashSet;
use std::sync::Arc;

use jbgit_core::graph::{
    FixedBitSet, GraphViewOptions, PermanentGraph, PrintElementGenerator, RawCommitNode,
    ReachableNodes, VisibleGraph, VisibleRowIdx,
};
use jbgit_core::{BranchColorId, CommitSummary, FilterError, LogFilterSpec, Oid, RepoSnapshot};
use ratatui::style::Color;
use rustc_hash::FxHashMap;

use crate::theme::BranchColorManager;

/// Per-snapshot permanent graph plus the current filtered/sorted projection.
pub struct CachedLogGraph {
    pub snapshot: Arc<RepoSnapshot>,
    pub permanent: PermanentGraph,
    pub visible: VisibleGraph,
    pub color_map: FxHashMap<BranchColorId, Color>,
    pub current_branch_reachable: FixedBitSet,
    pub recommended_tracks: usize,
    /// `LogGraphState::filter_generation` the visible projection was built for.
    pub filter_generation: u64,
}

impl CachedLogGraph {
    pub fn build(
        snap: &Arc<RepoSnapshot>,
        options: GraphViewOptions,
        filter: Option<&FixedBitSet>,
        branch_colors: &BranchColorManager,
    ) -> Self {
        let raw_commits: Vec<RawCommitNode> =
            snap.commits.iter().map(RawCommitNode::from).collect();
        let permanent = PermanentGraph::build(&raw_commits, &snap.refs, snap.head_oid);
        let color_map = build_graph_color_map(&permanent, branch_colors);
        let head_nodes: Vec<_> = snap
            .head_oid
            .and_then(|oid| permanent.linear.node_of_oid(&oid))
            .into_iter()
            .collect();
        let current_branch_reachable =
            ReachableNodes::contained_in_branch(&permanent.linear, &head_nodes);
        let visible = VisibleGraph::build(&permanent, options, filter);
        let recommended_tracks = PrintElementGenerator::new(&permanent, &visible, snap.head_oid)
            .recommended_track_width();

        Self {
            snapshot: Arc::clone(snap),
            permanent,
            visible,
            color_map,
            current_branch_reachable,
            recommended_tracks,
            filter_generation: 0,
        }
    }

    fn rebuild_visible(&mut self, options: GraphViewOptions, filter: Option<&FixedBitSet>) {
        self.visible = VisibleGraph::build(&self.permanent, options, filter);
        self.recommended_tracks =
            PrintElementGenerator::new(&self.permanent, &self.visible, self.snapshot.head_oid)
                .recommended_track_width();
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

pub fn build_graph_color_map(
    pg: &PermanentGraph,
    mgr: &BranchColorManager,
) -> FxHashMap<BranchColorId, Color> {
    let mut ordered_ids: Vec<BranchColorId> = Vec::new();
    for &head in &pg.sorted_heads {
        let cid = pg.node_color(head);
        if !ordered_ids.contains(&cid) {
            ordered_ids.push(cid);
        }
    }
    for &cid in &pg.node_colors {
        if !ordered_ids.contains(&cid) {
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
        }
    }
}

impl LogGraphState {
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
            return;
        };
        let snapshot_stale = self
            .cached
            .as_ref()
            .is_none_or(|c| !Arc::ptr_eq(&c.snapshot, snap));
        let visible_stale = self.cached.as_ref().is_none_or(|c| {
            c.filter_generation != self.filter_generation || c.visible.options != self.options
        });
        if !snapshot_stale && !visible_stale {
            return;
        }

        // Carry the selection over only from a previous projection (by oid);
        // a first build always starts at the top row.
        let prev_snap = self.cached.as_ref().map(|c| Arc::clone(&c.snapshot));
        let oid_at = |row: usize| {
            let cached = self.cached.as_ref()?;
            let prev = prev_snap.as_ref()?;
            cached.commit_at_visible_row(prev, row).map(|c| c.oid)
        };
        let selected_oid = oid_at(self.selected_idx);
        let anchor_oid = self.anchor.and_then(oid_at);

        if snapshot_stale {
            self.cached = Some(CachedLogGraph::build(
                snap,
                self.options,
                None,
                branch_colors,
            ));
        }
        let cached = self.cached.as_mut().expect("cache built above");
        let matches =
            filter
                .spec
                .compute_matches(&cached.permanent.linear, snap, filter.path_matches);
        let bits = match matches {
            Ok(bits) => {
                self.filter_error = None;
                bits
            }
            Err(e) => {
                self.filter_error = Some(e);
                None
            }
        };
        if bits.is_some() || !snapshot_stale || cached.visible.options != self.options {
            cached.rebuild_visible(self.options, bits.as_ref());
        }
        cached.filter_generation = self.filter_generation;

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
