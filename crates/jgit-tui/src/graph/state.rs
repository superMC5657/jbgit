use std::sync::Arc;

use jgit_core::graph::{
    FixedBitSet, GraphViewOptions, PermanentGraph, PrintElementGenerator, RawCommitNode,
    ReachableNodes, VisibleGraph,
};
use jgit_core::{BranchColorId, CommitSummary, Oid, RepoSnapshot};
use ratatui::style::Color;
use rustc_hash::FxHashMap;

use crate::theme::BranchColorManager;

pub struct CachedLogGraph {
    pub snapshot: Arc<RepoSnapshot>,
    pub permanent: PermanentGraph,
    pub visible: VisibleGraph,
    pub color_map: FxHashMap<BranchColorId, Color>,
    pub current_branch_reachable: FixedBitSet,
    pub recommended_tracks: usize,
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
        let visible = VisibleGraph::build(&permanent, options, filter);
        let color_map = build_graph_color_map(&permanent, branch_colors);
        let head_nodes: Vec<_> = snap
            .head_oid
            .and_then(|oid| permanent.linear.node_of_oid(&oid))
            .into_iter()
            .collect();
        let current_branch_reachable =
            ReachableNodes::contained_in_branch(&permanent.linear, &head_nodes);
        let recommended_tracks = PrintElementGenerator::new(&permanent, &visible, snap.head_oid)
            .recommended_track_width();

        Self {
            snapshot: Arc::clone(snap),
            permanent,
            visible,
            color_map,
            current_branch_reachable,
            recommended_tracks,
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

/// Encapsulates log-tab graph projection cache, viewport scroll, and jump back-history.
pub struct LogGraphState {
    pub options: GraphViewOptions,
    pub cached: Option<CachedLogGraph>,
    pub selected_idx: usize,
    pub scroll_offset: usize,
    pub last_viewport_h: usize,
    pub nav_history: Vec<usize>,
}

impl Default for LogGraphState {
    fn default() -> Self {
        Self {
            options: GraphViewOptions::default(),
            cached: None,
            selected_idx: 0,
            scroll_offset: 0,
            last_viewport_h: 20,
            nav_history: Vec::new(),
        }
    }
}

impl LogGraphState {
    pub fn ensure_cache(
        &mut self,
        repo_snapshot: Option<&Arc<RepoSnapshot>>,
        branch_colors: &BranchColorManager,
    ) {
        let Some(snap) = repo_snapshot else {
            self.cached = None;
            return;
        };
        let stale = self
            .cached
            .as_ref()
            .is_none_or(|c| !Arc::ptr_eq(&c.snapshot, snap));
        if stale {
            self.cached = Some(CachedLogGraph::build(
                snap,
                self.options,
                None,
                branch_colors,
            ));
            let vis_len = self.visible_count(repo_snapshot);
            if self.selected_idx >= vis_len {
                self.selected_idx = vis_len.saturating_sub(1);
            }
        }
    }

    pub fn on_snapshot_loaded(
        &mut self,
        snapshot: &Arc<RepoSnapshot>,
        branch_colors: &BranchColorManager,
    ) {
        let commits_len = snapshot.commits.len();
        if self.selected_idx >= commits_len {
            self.selected_idx = commits_len.saturating_sub(1);
        }
        self.cached = None;
        self.ensure_cache(Some(snapshot), branch_colors);
        self.nav_history.clear();
        self.scroll_offset = 0;
    }

    pub fn visible_count(&self, repo_snapshot: Option<&Arc<RepoSnapshot>>) -> usize {
        if let Some(cached) = &self.cached {
            cached.visible_count()
        } else {
            repo_snapshot.map_or(0, |s| s.commits.len())
        }
    }

    pub fn commit_at<'a>(
        &self,
        repo_snapshot: Option<&'a Arc<RepoSnapshot>>,
        visible_row: usize,
    ) -> Option<&'a CommitSummary> {
        let snap = repo_snapshot?;
        if let Some(cached) = &self.cached {
            cached.commit_at_visible_row(snap, visible_row)
        } else {
            snap.commits.get(visible_row)
        }
    }

    pub fn select_row(
        &mut self,
        repo_snapshot: Option<&Arc<RepoSnapshot>>,
        branch_colors: &BranchColorManager,
        idx: usize,
    ) -> Option<Oid> {
        self.ensure_cache(repo_snapshot, branch_colors);
        let len = self.visible_count(repo_snapshot);
        if len == 0 {
            return None;
        }
        self.selected_idx = idx.min(len - 1);
        self.adjust_scroll(repo_snapshot, false);
        self.commit_at(repo_snapshot, self.selected_idx)
            .map(|c| c.oid)
    }

    pub fn scroll_by(
        &mut self,
        repo_snapshot: Option<&Arc<RepoSnapshot>>,
        branch_colors: &BranchColorManager,
        delta: isize,
    ) -> Option<Oid> {
        self.ensure_cache(repo_snapshot, branch_colors);
        let len = self.visible_count(repo_snapshot);
        if len == 0 {
            return None;
        }
        let cur = self.selected_idx as isize;
        let next = (cur + delta).clamp(0, (len - 1) as isize) as usize;
        if next != self.selected_idx {
            self.selected_idx = next;
            self.adjust_scroll(repo_snapshot, false);
            return self.commit_at(repo_snapshot, next).map(|c| c.oid);
        }
        None
    }

    pub fn jump_to(
        &mut self,
        repo_snapshot: Option<&Arc<RepoSnapshot>>,
        branch_colors: &BranchColorManager,
        row: usize,
        center: bool,
    ) -> Option<Oid> {
        self.ensure_cache(repo_snapshot, branch_colors);
        let len = self.visible_count(repo_snapshot);
        if len == 0 {
            return None;
        }
        let target = row.min(len - 1);
        if target != self.selected_idx {
            self.nav_history.push(self.selected_idx);
            if self.nav_history.len() > 64 {
                self.nav_history.remove(0);
            }
            self.selected_idx = target;
        }
        self.adjust_scroll(repo_snapshot, center);
        self.commit_at(repo_snapshot, self.selected_idx)
            .map(|c| c.oid)
    }

    pub fn nav_back(
        &mut self,
        repo_snapshot: Option<&Arc<RepoSnapshot>>,
        branch_colors: &BranchColorManager,
    ) -> Option<Oid> {
        self.ensure_cache(repo_snapshot, branch_colors);
        let len = self.visible_count(repo_snapshot);
        if len == 0 {
            return None;
        }
        let prev = self.nav_history.pop()?;
        self.selected_idx = prev.min(len - 1);
        self.adjust_scroll(repo_snapshot, true);
        self.commit_at(repo_snapshot, self.selected_idx)
            .map(|c| c.oid)
    }

    fn adjust_scroll(&mut self, repo_snapshot: Option<&Arc<RepoSnapshot>>, center: bool) {
        let total = self.visible_count(repo_snapshot);
        let vh = self.last_viewport_h.max(1);
        if total == 0 {
            self.scroll_offset = 0;
            return;
        }
        if center {
            self.scroll_offset = self
                .selected_idx
                .saturating_sub(vh / 2)
                .min(total.saturating_sub(vh));
        } else {
            let max_off = total.saturating_sub(vh);
            if self.selected_idx < self.scroll_offset {
                self.scroll_offset = self.selected_idx;
            } else if self.selected_idx >= self.scroll_offset.saturating_add(vh) {
                self.scroll_offset = self.selected_idx + 1 - vh;
            }
            self.scroll_offset = self.scroll_offset.min(max_off);
        }
    }
}
