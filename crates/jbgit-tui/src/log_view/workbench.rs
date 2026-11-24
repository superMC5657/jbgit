//! Self-contained `[2 Log]` three-pane workbench module encapsulating the
//! commit graph, Branches Dashboard, filter bar, presentation settings,
//! graph renderer, and right-hand details/changes/diff panes.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Instant;

use jbgit_core::graph::SortType;
use jbgit_core::{
    BranchCompareDirection, BranchFilter, BranchNodeKind, CherryPickedKey, CommitSummary,
    DetailsKey, DetailsMode, DiffContext, Oid, PaneId, PathFilter, RangeFilter, RefKind,
    RepoSnapshot, RequestSlot, RequestSlotId, WhitespaceMode,
};

use crate::app::{
    Action, ChangesHost, ContainingBranchesJob, DiffScope, Effect, ScrollPane, SyntaxJob,
    SyntaxJobKey, SyntaxTarget,
};
use crate::diff_session::DiffCacheStore;
use crate::graph::state::{ASYNC_GRAPH_THRESHOLD, FilterInputs};
use crate::graph::{GraphRenderer, GraphRendererMode, LogGraphState};
use crate::i18n::I18n;
use crate::theme::BranchColorManager;

use super::{
    BranchesPaneState, DETAILS_DEBOUNCE, DetailsPaneState, FilterBarState, FilterField,
    LogPresentation, LogSpeedSearchState, MAX_AGGREGATED_COMMITS, PathRequest, PresentationToggle,
};

pub struct LogDispatchCtx<'a> {
    pub snap: Option<&'a Arc<RepoSnapshot>>,
    pub branch_colors: &'a BranchColorManager,
    pub diff_caches: &'a DiffCacheStore,
    pub i18n: &'a I18n,
    pub now: Instant,
    pub width: u16,
    pub async_graph_enabled: bool,
    pub focused_pane: &'a mut PaneId,
    pub status_notice: &'a mut Option<String>,
    pub effects: &'a mut Vec<Effect>,
}

pub struct LogReconcileCtx<'a> {
    pub snap: Option<&'a Arc<RepoSnapshot>>,
    pub branch_colors: &'a BranchColorManager,
    pub diff_caches: &'a mut DiffCacheStore,
    pub file_syntax: &'a mut RequestSlot<jbgit_core::FileDiffKey>,
    pub syntax_enabled: bool,
    pub i18n: &'a I18n,
    pub async_graph_enabled: bool,
    pub status_notice: &'a mut Option<String>,
    pub effects: &'a mut Vec<Effect>,
}

/// Active two-branch symmetric comparison state (`Compare with…` / `Compare with Current`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchCompareState {
    pub left: String,
    pub right: String,
    pub left_oid: Oid,
    pub right_oid: Oid,
    pub direction: BranchCompareDirection,
    pub only_left: usize,
    pub only_right: usize,
    pub tree_diff: bool,
}

impl BranchCompareState {
    pub fn range_label(&self) -> String {
        self.direction.range_label(&self.left, &self.right)
    }

    pub fn diff_pair(&self) -> (Oid, Oid) {
        match self.direction {
            BranchCompareDirection::OnlyLeft => (self.right_oid, self.left_oid),
            BranchCompareDirection::OnlyRight | BranchCompareDirection::Symmetric => {
                (self.left_oid, self.right_oid)
            }
        }
    }
}

pub struct LogWorkbench {
    pub(crate) graph: LogGraphState,
    pub(crate) branches: BranchesPaneState,
    pub(crate) filter: FilterBarState,
    pub(crate) speed_search: LogSpeedSearchState,
    pub(crate) presentation: LogPresentation,
    pub(crate) graph_renderer: GraphRenderer,
    pub(crate) details: DetailsPaneState,
    pub(crate) sig_requested: Vec<Oid>,
    /// Loaded `git cherry <head> <branch>` patch-equivalent OIDs for the active compared branch.
    pub(crate) cherry_picked: Option<(CherryPickedKey, Arc<HashSet<Oid>>)>,
    /// Key of the currently in-flight `git cherry` request, if any.
    pub(crate) cherry_picked_requested: Option<CherryPickedKey>,
    pub(crate) branch_compare: Option<BranchCompareState>,
}

impl LogWorkbench {
    pub fn new(
        diff_context: DiffContext,
        diff_whitespace: WhitespaceMode,
        diff_soft_wrap: bool,
        compact_refs: bool,
        log_cfg: &crate::config::jbgit_config::LogSection,
        graph_mode: GraphRendererMode,
    ) -> Self {
        let mut details = DetailsPaneState::default();
        details.diff.context = diff_context;
        details.diff.whitespace = diff_whitespace;
        details.diff.soft_wrap = diff_soft_wrap;
        let mut graph = LogGraphState::default();
        graph.options.linear_bek = log_cfg.linear_bek;
        let presentation = LogPresentation {
            compact_refs,
            show_only_affected_changes: log_cfg.show_only_affected_changes,
            highlight_my_commits: log_cfg.highlight_my_commits,
            highlight_cherry_picked: log_cfg.highlight_cherry_picked,
            show_author_column: log_cfg.show_author_column,
            show_date_column: log_cfg.show_date_column,
            align_labels_right: log_cfg.align_labels_right,
            ..Default::default()
        };
        Self {
            graph,
            branches: BranchesPaneState::default(),
            filter: FilterBarState::default(),
            speed_search: LogSpeedSearchState::default(),
            presentation,
            graph_renderer: GraphRenderer::new(
                graph_mode,
                crate::graph::GraphicsCapabilities::default(),
            ),
            details,
            sig_requested: Vec::new(),
            cherry_picked: None,
            cherry_picked_requested: None,
            branch_compare: None,
        }
    }

    pub fn speed_search(&self) -> &LogSpeedSearchState {
        &self.speed_search
    }

    /// True when Current Branch background tinting should be drawn.
    /// Auto-suppressed when `BranchFilter` restricts solely to `HEAD` or current branch.
    pub fn should_highlight_current_branch(&self, snap: &RepoSnapshot) -> bool {
        self.presentation.highlight_current_branch
            && !self
                .filter
                .spec
                .branch
                .is_only_current_branch(snap.head_branch.as_deref())
    }

    /// True when My Commits bolding should be drawn.
    /// Auto-suppressed for single-author repos or when `UserFilter(@me)` is active.
    pub fn should_highlight_my_commits(&self) -> bool {
        self.presentation.highlight_my_commits
            && !self.filter.spec.user.has_me()
            && self
                .graph
                .cached
                .as_ref()
                .is_some_and(|c| c.has_multiple_authors)
    }

    /// Active `CherryPickedKey` when the `Already Cherry-Picked` highlighter applies.
    pub fn desired_cherry_picked_key(&self, snap: &RepoSnapshot) -> Option<CherryPickedKey> {
        if !self.presentation.highlight_cherry_picked {
            return None;
        }
        let head_oid = snap.head_oid?;
        let branch = self
            .filter
            .spec
            .branch
            .single_compared_branch(snap.head_branch.as_deref())
            .or_else(|| {
                let bc = self.branch_compare.as_ref()?;
                let head = snap.head_branch.as_deref();
                if head == Some(bc.left.as_str()) {
                    Some(bc.right.as_str())
                } else if head == Some(bc.right.as_str()) {
                    Some(bc.left.as_str())
                } else {
                    Some(bc.right.as_str())
                }
            })?;
        Some(CherryPickedKey {
            head_oid,
            branch: branch.to_string(),
        })
    }

    /// True if `oid` is dimmed by the Already Cherry-Picked highlighter for the current filter.
    pub fn is_commit_cherry_picked(&self, snap: &RepoSnapshot, oid: &Oid) -> bool {
        let Some(desired) = self.desired_cherry_picked_key(snap) else {
            return false;
        };
        self.cherry_picked
            .as_ref()
            .is_some_and(|(key, set)| *key == desired && set.contains(oid))
    }

    pub fn apply_cherry_picked_loaded(&mut self, key: CherryPickedKey, oids: Arc<HashSet<Oid>>) {
        if self.cherry_picked_requested.as_ref() == Some(&key) {
            self.cherry_picked_requested = None;
        }
        self.cherry_picked = Some((key, oids));
    }

    pub fn favorite_branch_names(&self, snap: Option<&Arc<RepoSnapshot>>) -> Vec<String> {
        let Some(s) = snap else {
            return Vec::new();
        };
        s.refs
            .iter()
            .filter(|r| {
                matches!(r.kind, RefKind::LocalBranch | RefKind::RemoteBranch)
                    && self
                        .branches
                        .settings
                        .favorites
                        .is_favorite(&r.full_name, &r.name)
            })
            .map(|r| r.name.clone())
            .collect()
    }

    pub fn graph(&self) -> &LogGraphState {
        &self.graph
    }

    pub fn branches(&self) -> &BranchesPaneState {
        &self.branches
    }

    pub fn filter(&self) -> &FilterBarState {
        &self.filter
    }

    pub fn presentation(&self) -> &LogPresentation {
        &self.presentation
    }

    pub fn graph_renderer(&self) -> &GraphRenderer {
        &self.graph_renderer
    }

    pub fn details(&self) -> &DetailsPaneState {
        &self.details
    }

    pub fn commit_at<'a>(
        &self,
        snap: Option<&'a Arc<RepoSnapshot>>,
        visible_row: usize,
    ) -> Option<&'a CommitSummary> {
        self.graph.commit_at(snap, visible_row)
    }

    pub fn uses_async_graph(&self, snap: Option<&Arc<RepoSnapshot>>, async_enabled: bool) -> bool {
        async_enabled && snap.is_some_and(|s| s.commits.len() >= ASYNC_GRAPH_THRESHOLD)
    }

    pub fn ensure_graph_cache(
        &mut self,
        snap: Option<&Arc<RepoSnapshot>>,
        branch_colors: &BranchColorManager,
        async_enabled: bool,
    ) {
        if !self.uses_async_graph(snap, async_enabled) {
            let path_matches = self.filter.current_path_matches(snap);
            let favorites = self.favorite_branch_names(snap);
            self.graph.ensure_cache(
                snap,
                branch_colors,
                &FilterInputs {
                    spec: &self.filter.spec,
                    path_matches,
                    favorites: &favorites,
                },
            );
        }
        self.branches.sync_model(snap, false);
        self.sync_speed_search(snap);
    }

    /// Recomputes cached in-table speed search row indices whenever the visible
    /// graph projection has changed since the last computation.
    pub fn sync_speed_search(&mut self, snap: Option<&Arc<RepoSnapshot>>) {
        if !self.speed_search.is_active() {
            return;
        }
        if self.speed_search.graph_revision == self.graph.visible_revision {
            return;
        }
        self.speed_search.graph_revision = self.graph.visible_revision;
        if let (Some(cached), Some(snap)) = (&self.graph.cached, snap) {
            let _ = self
                .speed_search
                .recompute(cached, snap, self.graph.selected_idx);
        } else {
            self.speed_search.matches.clear();
            self.speed_search.current = None;
        }
    }

    pub fn on_blur(&mut self, leaving_tab: bool) {
        if leaving_tab {
            self.details.diff.zoomed = false;
        }
        self.details.diff.search.editing = false;
        self.filter.editing_text = false;
        self.speed_search.editing = false;
        self.branches.searching = false;
    }

    pub fn next_deadline(&self) -> Option<Instant> {
        [self.details.next_deadline(), self.filter.next_deadline()]
            .into_iter()
            .flatten()
            .min()
    }

    pub fn scroll_pane(&mut self, pane: ScrollPane, delta: isize, diff_caches: &DiffCacheStore) {
        match pane {
            ScrollPane::LogDiff => {
                let fallback = self.details.fallback_diff_max_scroll(diff_caches);
                self.details.diff.scroll.scroll_by(delta, fallback);
            }
            ScrollPane::LogDetails => {
                self.details.details_scroll.scroll_by(delta, usize::MAX);
            }
            _ => {}
        }
    }

    pub fn sync_affected_filter(&mut self) {
        let next = if self.presentation.show_only_affected_changes
            && !self.filter.spec.paths.is_empty()
        {
            Some(self.filter.spec.paths.clone())
        } else {
            None
        };
        if self.details.changes.affected_filter != next {
            self.details.changes.affected_filter = next;
            self.details.changes.selected = 0;
            self.details.diff.scroll.reset();
        }
    }

    pub fn on_filter_changed(
        &mut self,
        snap: Option<&Arc<RepoSnapshot>>,
        branch_colors: &BranchColorManager,
        async_enabled: bool,
        i18n: &I18n,
        status_notice: &mut Option<String>,
    ) {
        // Any explicit re-filter also covers a pending debounced text edit.
        self.filter.clear_text_debounce();
        self.sync_affected_filter();
        self.graph.filter_generation = self.graph.filter_generation.wrapping_add(1);
        self.ensure_graph_cache(snap, branch_colors, async_enabled);
        if let Some(err) = &self.graph.filter_error {
            *status_notice = Some(i18n.t_fmt("toast.filter_error", &[("err", &err.to_string())]));
        }
    }

    pub fn navigate_to_branch_node(
        &mut self,
        snap: Option<&Arc<RepoSnapshot>>,
        i18n: &I18n,
        status_notice: &mut Option<String>,
    ) {
        let Some(node) = self.branches.selected_node() else {
            return;
        };
        let Some(oid) = node.target_oid else {
            return;
        };
        let label = node.ref_name.clone().unwrap_or_default();
        if !self.graph.jump_to_oid(snap, &oid) {
            *status_notice = Some(i18n.t_fmt("toast.tip_hidden_by_filter", &[("label", &label)]));
        }
    }

    pub fn start_compare_with_local(
        &mut self,
        target: Oid,
        select_path: Option<String>,
        path_prefix: Option<String>,
        i18n: &I18n,
        status_notice: &mut Option<String>,
    ) {
        self.details.diff_preview = true;
        self.details.pending_select_path = select_path;
        let key = DetailsKey::compare_with_local_path(target, path_prefix);
        let t_s = target.short();
        *status_notice = Some(match key.path_prefix.as_deref() {
            Some(dir) => i18n.t_fmt("toast.comparing_local_dir", &[("target", &t_s), ("dir", dir)]),
            None => i18n.t_fmt("toast.comparing_local", &[("target", &t_s)]),
        });
        self.details.compare_override = Some(key);
    }

    pub fn start_compare_versions(
        &mut self,
        base: Oid,
        target: Oid,
        i18n: &I18n,
        status_notice: &mut Option<String>,
    ) {
        self.details.diff_preview = true;
        self.details.pending_select_path = None;
        self.details.compare_override = Some(DetailsKey::compare_versions(base, target));
        let b_s = base.short();
        let t_s = target.short();
        *status_notice = Some(i18n.t_fmt(
            "toast.comparing_versions",
            &[("base", &b_s), ("target", &t_s)],
        ));
    }

    pub fn compare_with_current(
        &mut self,
        branch: &str,
        snap: Option<&Arc<RepoSnapshot>>,
        branch_colors: &BranchColorManager,
        async_enabled: bool,
        i18n: &I18n,
        status_notice: &mut Option<String>,
    ) -> bool {
        let Some(snap_ref) = snap else {
            return false;
        };
        let current = snap_ref
            .head_branch
            .clone()
            .unwrap_or_else(|| "HEAD".to_string());
        self.compare_branches(
            &current,
            branch,
            snap,
            branch_colors,
            async_enabled,
            i18n,
            status_notice,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn compare_branches(
        &mut self,
        left: &str,
        right: &str,
        snap: Option<&Arc<RepoSnapshot>>,
        branch_colors: &BranchColorManager,
        async_enabled: bool,
        i18n: &I18n,
        status_notice: &mut Option<String>,
    ) -> bool {
        let Some(snap_ref) = snap else {
            return false;
        };
        let (Some(left_oid), Some(right_oid)) = (
            snap_ref.resolve_revision(left),
            snap_ref.resolve_revision(right),
        ) else {
            *status_notice = Some(i18n.t_fmt("toast.cannot_compare_branch", &[("name", right)]));
            return false;
        };
        let only_left = snap_ref.count_exclusive(left_oid, right_oid);
        let only_right = snap_ref.count_exclusive(right_oid, left_oid);
        let state = BranchCompareState {
            left: left.to_string(),
            right: right.to_string(),
            left_oid,
            right_oid,
            direction: BranchCompareDirection::Symmetric,
            only_left,
            only_right,
            tree_diff: false,
        };
        let oids = snap_ref.compare_branch_oids(left_oid, right_oid, state.direction);
        let label = state.range_label();
        self.branch_compare = Some(state);
        self.filter.spec.branch = BranchFilter::default();
        self.filter.spec.range = Some(RangeFilter::new(label, oids));
        self.graph.filter_generation = self.graph.filter_generation.wrapping_add(1);
        self.ensure_graph_cache(snap, branch_colors, async_enabled);
        self.graph.jump_to_oid(snap, &right_oid);
        let oa = only_right.to_string();
        let ob = only_left.to_string();
        *status_notice = Some(i18n.t_fmt(
            "toast.compare_summary",
            &[
                ("branch", right),
                ("current", left),
                ("only_a", &oa),
                ("only_b", &ob),
            ],
        ));
        true
    }

    pub fn cycle_branch_compare_direction(
        &mut self,
        snap: Option<&Arc<RepoSnapshot>>,
        branch_colors: &BranchColorManager,
        async_enabled: bool,
        i18n: &I18n,
        status_notice: &mut Option<String>,
    ) -> bool {
        let (Some(snap_ref), Some(bc)) = (snap, self.branch_compare.as_mut()) else {
            return false;
        };
        bc.direction = bc.direction.cycle();
        let oids = snap_ref.compare_branch_oids(bc.left_oid, bc.right_oid, bc.direction);
        let label = bc.range_label();
        let tree_diff = bc.tree_diff;
        let (base, target) = bc.diff_pair();
        self.filter.spec.range = Some(RangeFilter::new(label.clone(), oids));
        if tree_diff {
            self.details.compare_override = Some(DetailsKey::compare_versions(base, target));
        }
        self.graph.filter_generation = self.graph.filter_generation.wrapping_add(1);
        self.ensure_graph_cache(snap, branch_colors, async_enabled);
        *status_notice = Some(i18n.t_fmt("toast.compare_direction", &[("range", &label)]));
        true
    }

    pub fn toggle_branch_compare_tree_diff(
        &mut self,
        i18n: &I18n,
        status_notice: &mut Option<String>,
    ) -> bool {
        let Some(bc) = self.branch_compare.as_mut() else {
            return false;
        };
        bc.tree_diff = !bc.tree_diff;
        if bc.tree_diff {
            let (base, target) = bc.diff_pair();
            self.details.diff_preview = true;
            self.details.pending_select_path = None;
            self.details.compare_override = Some(DetailsKey::compare_versions(base, target));
            let label = bc.range_label();
            *status_notice = Some(i18n.t_fmt("toast.compare_tree_diff", &[("range", &label)]));
        } else {
            self.details.compare_override = None;
        }
        true
    }

    pub fn close_branch_compare(
        &mut self,
        snap: Option<&Arc<RepoSnapshot>>,
        branch_colors: &BranchColorManager,
        async_enabled: bool,
    ) {
        self.branch_compare = None;
        self.filter.spec.range = None;
        self.details.compare_override = None;
        self.graph.filter_generation = self.graph.filter_generation.wrapping_add(1);
        self.ensure_graph_cache(snap, branch_colors, async_enabled);
    }

    pub fn flush_pending(&mut self, ctx: &mut LogDispatchCtx<'_>) {
        if self.filter.take_due_text_apply(ctx.now) {
            self.on_filter_changed(
                ctx.snap,
                ctx.branch_colors,
                ctx.async_graph_enabled,
                ctx.i18n,
                ctx.status_notice,
            );
        }
        if let Some((key, at)) = self.details.pending.take() {
            if at <= ctx.now {
                if !ctx.diff_caches.details.contains(&key) {
                    ctx.effects.push(Effect::RequestCommitDetails {
                        slot: RequestSlotId::LogCommitDetails,
                        key,
                    });
                }
            } else {
                self.details.pending = Some((key, at));
            }
        }
    }

    pub fn dispatch(&mut self, action: &Action, ctx: &mut LogDispatchCtx<'_>) -> bool {
        match action {
            Action::SetGraphRenderer(mode) => {
                self.graph_renderer.set_mode(*mode);
                return true;
            }
            Action::CycleGraphRenderer => {
                let next = self.graph_renderer.mode().next();
                self.graph_renderer.set_mode(next);
                return true;
            }
            Action::TogglePresentation(t) => {
                self.toggle_presentation(*t, ctx);
                return true;
            }
            _ => {}
        }

        self.dispatch_branches_action(action, ctx)
            || self.dispatch_filter_action(action, ctx)
            || self.dispatch_log_action(action, ctx)
            || self.dispatch_details_action(action, ctx)
    }

    /// Status toast after a collapse / expand: `N commits hidden / shown`.
    fn notify_fold(&self, before: usize, ctx: &mut LogDispatchCtx<'_>) {
        let after = self.graph.visible_count();
        let n = before.abs_diff(after).to_string();
        let key = if after < before {
            "toast.graph_collapsed"
        } else {
            "toast.graph_expanded"
        };
        *ctx.status_notice = Some(ctx.i18n.t_fmt(key, &[("n", &n)]));
    }

    fn toggle_presentation(&mut self, t: PresentationToggle, ctx: &mut LogDispatchCtx<'_>) {
        let p = &mut self.presentation;
        let o = &mut self.graph.options;
        let (label_key, on) = match t {
            PresentationToggle::CompactRefs => {
                p.compact_refs = !p.compact_refs;
                ("presentation.compact_refs", p.compact_refs)
            }
            PresentationToggle::ShowTagNames => {
                p.show_tag_names = !p.show_tag_names;
                ("presentation.show_tag_names", p.show_tag_names)
            }
            PresentationToggle::AlignLabelsRight => {
                p.align_labels_right = !p.align_labels_right;
                ("presentation.align_labels_right", p.align_labels_right)
            }
            PresentationToggle::ShowLongEdges => {
                o.show_long_edges = !o.show_long_edges;
                ("presentation.show_long_edges", o.show_long_edges)
            }
            PresentationToggle::IntelliSort => {
                o.sort_type = match o.sort_type {
                    SortType::IntelliSort => SortType::Topo,
                    SortType::Topo => SortType::IntelliSort,
                };
                (
                    "presentation.intelli_sort",
                    o.sort_type == SortType::IntelliSort,
                )
            }
            PresentationToggle::LinearBek => {
                o.linear_bek = !o.linear_bek;
                if o.linear_bek {
                    self.graph.linear_bek.collapse_all();
                }
                ("presentation.linear_bek", o.linear_bek)
            }
            PresentationToggle::FirstParentOnly => {
                o.first_parent_only = !o.first_parent_only;
                ("presentation.first_parent_only", o.first_parent_only)
            }
            PresentationToggle::HighlightCurrentBranch => {
                p.highlight_current_branch = !p.highlight_current_branch;
                (
                    "presentation.highlight_current_branch",
                    p.highlight_current_branch,
                )
            }
            PresentationToggle::HighlightMyCommits => {
                p.highlight_my_commits = !p.highlight_my_commits;
                ("presentation.highlight_my_commits", p.highlight_my_commits)
            }
            PresentationToggle::HighlightMerges => {
                p.highlight_merges = !p.highlight_merges;
                ("presentation.highlight_merges", p.highlight_merges)
            }
            PresentationToggle::HighlightCherryPicked => {
                p.highlight_cherry_picked = !p.highlight_cherry_picked;
                if !p.highlight_cherry_picked {
                    self.cherry_picked_requested = None;
                }
                (
                    "presentation.highlight_cherry_picked",
                    p.highlight_cherry_picked,
                )
            }
            PresentationToggle::ShowAuthorColumn => {
                p.show_author_column = !p.show_author_column;
                ("presentation.show_author_column", p.show_author_column)
            }
            PresentationToggle::ShowDateColumn => {
                p.show_date_column = !p.show_date_column;
                ("presentation.show_date_column", p.show_date_column)
            }
            PresentationToggle::ShowHashColumn => {
                p.show_hash_column = !p.show_hash_column;
                ("presentation.show_hash_column", p.show_hash_column)
            }
            PresentationToggle::ShowSignatureColumn => {
                p.show_signature_column = !p.show_signature_column;
                if p.show_signature_column {
                    self.sig_requested.clear();
                }
                ("presentation.show_signature_column", p.show_signature_column)
            }
            PresentationToggle::RelativeDates => {
                p.relative_dates = !p.relative_dates;
                ("presentation.relative_dates", p.relative_dates)
            }
            PresentationToggle::UseAuthorDate => {
                p.use_author_date = !p.use_author_date;
                ("presentation.use_author_date", p.use_author_date)
            }
            PresentationToggle::ShowOnlyAffectedChanges => {
                p.show_only_affected_changes = !p.show_only_affected_changes;
                (
                    "presentation.show_only_affected_changes",
                    p.show_only_affected_changes,
                )
            }
        };
        let label = ctx.i18n.t(label_key);
        let state = ctx.i18n.on_off(on);
        *ctx.status_notice = Some(
            ctx.i18n
                .t_fmt("toast.presentation_toggle", &[("label", label), ("state", state)]),
        );
        self.sync_affected_filter();
        self.ensure_graph_cache(ctx.snap, ctx.branch_colors, ctx.async_graph_enabled);
    }

    fn dispatch_branches_action(&mut self, action: &Action, ctx: &mut LogDispatchCtx<'_>) -> bool {
        match action {
            Action::ToggleBranchesPane => {
                self.branches.toggle_collapsed(ctx.width);
                if self.branches.is_collapsed(ctx.width) {
                    if *ctx.focused_pane == PaneId::LogBranchesTree {
                        *ctx.focused_pane = PaneId::LogCommitTable;
                    }
                } else {
                    *ctx.focused_pane = PaneId::LogBranchesTree;
                }
            }
            Action::SelectBranchRow(idx) => {
                if !self.branches.rows.is_empty() {
                    self.branches.selected = (*idx).min(self.branches.rows.len() - 1);
                    if self.branches.settings.navigate_on_select {
                        self.navigate_to_branch_node(ctx.snap, ctx.i18n, ctx.status_notice);
                    }
                }
            }
            Action::ScrollBranches(delta) => {
                self.branches.move_selection(*delta);
                if self.branches.settings.navigate_on_select {
                    self.navigate_to_branch_node(ctx.snap, ctx.i18n, ctx.status_notice);
                }
            }
            Action::BranchesExpand(expand) => {
                let expand = *expand;
                if let Some(row) = self.branches.selected_row() {
                    let node = &self.branches.model.nodes[row.node];
                    match row.expanded {
                        Some(is_open) if is_open != expand => {
                            let key = node.key.clone();
                            self.branches.settings.expansion.set(&key, expand);
                            self.branches.save_settings();
                            self.branches.refresh_rows();
                        }
                        Some(true) if expand => self.branches.move_selection(1),
                        _ if !expand => {
                            if let Some(parent) = node.parent {
                                let key = self.branches.model.nodes[parent].key.clone();
                                self.branches.select_key(&key);
                            }
                        }
                        _ => {}
                    }
                }
            }
            Action::BranchesToggleRow(idx) => {
                if let Some(row) = self.branches.rows.get(*idx).copied()
                    && row.expanded.is_some()
                {
                    let key = self.branches.model.nodes[row.node].key.clone();
                    self.branches.settings.expansion.toggle(&key);
                    self.branches.save_settings();
                    self.branches.refresh_rows();
                    self.branches.select_key(&key);
                }
            }
            Action::BranchesToggleMulti(idx) => {
                if let Some(row) = self.branches.rows.get(*idx).copied() {
                    let key = self.branches.model.nodes[row.node].key.clone();
                    if self.branches.multi.is_empty()
                        && let Some(cur) = self.branches.selected_node()
                        && cur.key != key
                    {
                        let cur_key = cur.key.clone();
                        self.branches.toggle_multi(&cur_key);
                    }
                    self.branches.toggle_multi(&key);
                    self.branches.selected = *idx;
                }
            }
            Action::BranchesApplyFilter => {
                let names = self.branches.filter_ref_names();
                if !names.is_empty() {
                    self.filter.spec.branch = BranchFilter::from_names(names);
                    self.branches.multi.clear();
                    let label = self.filter.spec.branch.label();
                    *ctx.status_notice = Some(
                        ctx.i18n
                            .t_fmt("toast.branch_filter_set", &[("label", &label)]),
                    );
                    self.on_filter_changed(
                        ctx.snap,
                        ctx.branch_colors,
                        ctx.async_graph_enabled,
                        ctx.i18n,
                        ctx.status_notice,
                    );
                }
            }
            Action::BranchesNavigate => {
                self.navigate_to_branch_node(ctx.snap, ctx.i18n, ctx.status_notice);
            }
            Action::BranchesToggleFavorite => {
                if let Some(node) = self.branches.selected_node().cloned()
                    && let BranchNodeKind::Leaf(kind) = node.kind
                    && kind != RefKind::Tag
                {
                    let (full, short) = if kind == RefKind::Head {
                        match ctx.snap.and_then(|s| s.head_branch.clone()) {
                            Some(b) => (format!("refs/heads/{b}"), b),
                            None => return true,
                        }
                    } else {
                        let name = node.ref_name.clone().unwrap_or_default();
                        let short = if kind == RefKind::RemoteBranch {
                            ctx.snap
                                .and_then(|s| s.split_tracking(&name).map(|(_, b)| b.to_string()))
                                .unwrap_or_else(|| {
                                    name.split_once('/')
                                        .map_or(name.clone(), |(_, s)| s.to_string())
                                })
                        } else {
                            name
                        };
                        (node.key.clone(), short)
                    };
                    let on = self.branches.settings.favorites.toggle(&full, &short);
                    self.branches.save_settings();
                    self.branches.sync_model(ctx.snap, true);
                    self.branches.select_key(&node.key);
                    *ctx.status_notice = Some(ctx.i18n.t_fmt(
                        if on {
                            "toast.favorite_added"
                        } else {
                            "toast.favorite_removed"
                        },
                        &[("name", &short)],
                    ));
                }
            }
            Action::BranchesSearchStart => {
                *ctx.focused_pane = PaneId::LogBranchesTree;
                self.branches.searching = true;
            }
            Action::BranchesSearchDone => {
                self.branches.searching = false;
            }
            Action::BranchesSearchClear => {
                self.branches.query.clear();
                self.branches.refresh_rows();
            }
            Action::BranchesSearchInput(ch) => {
                self.branches.query.push(*ch);
                self.branches.refresh_rows();
                if let Some(pos) = self.branches.rows.iter().position(|r| r.expanded.is_none()) {
                    self.branches.selected = pos;
                }
            }
            Action::BranchesSearchBackspace => {
                self.branches.query.pop();
                self.branches.refresh_rows();
            }
            Action::BranchesEscape => {
                self.branches.searching = false;
                if !self.branches.query.is_empty() {
                    let key = self.branches.selected_node().map(|n| n.key.clone());
                    self.branches.query.clear();
                    self.branches.refresh_rows();
                    if let Some(k) = key {
                        self.branches.select_key(&k);
                    }
                } else if !self.branches.multi.is_empty() {
                    self.branches.multi.clear();
                } else if !self.filter.spec.branch.is_empty() {
                    self.filter.spec.branch = BranchFilter::default();
                    *ctx.status_notice =
                        Some(ctx.i18n.t("toast.branch_filter_cleared").to_string());
                    self.on_filter_changed(
                        ctx.snap,
                        ctx.branch_colors,
                        ctx.async_graph_enabled,
                        ctx.i18n,
                        ctx.status_notice,
                    );
                }
            }
            Action::BranchesToggleGroupByDir => {
                let o = &mut self.branches.settings.options;
                o.group_by_directory = !o.group_by_directory;
                self.branches.save_settings();
                self.branches.sync_model(ctx.snap, true);
            }
            Action::BranchesToggleShowTags => {
                let o = &mut self.branches.settings.options;
                o.show_tags = !o.show_tags;
                self.branches.save_settings();
                self.branches.sync_model(ctx.snap, true);
            }
            Action::BranchesToggleNavigateOnSelect => {
                let s = &mut self.branches.settings;
                s.navigate_on_select = !s.navigate_on_select;
                self.branches.save_settings();
                let st = ctx.i18n.on_off(self.branches.settings.navigate_on_select);
                *ctx.status_notice = Some(ctx.i18n.t_fmt("toast.nav_on_select", &[("state", st)]));
            }
            _ => return false,
        }
        true
    }

    fn dispatch_filter_action(&mut self, action: &Action, ctx: &mut LogDispatchCtx<'_>) -> bool {
        match action {
            Action::FocusFilterText => {
                *ctx.focused_pane = PaneId::LogCommitTable;
                self.filter.editing_text = true;
            }
            Action::FilterTextInput(ch) => {
                self.filter.push_text_char(*ch, ctx.now);
            }
            Action::FilterTextBackspace => {
                self.filter.pop_text_char(ctx.now);
            }
            Action::FilterTextDone => {
                if self.filter.finish_text_edit() {
                    self.on_filter_changed(
                        ctx.snap,
                        ctx.branch_colors,
                        ctx.async_graph_enabled,
                        ctx.i18n,
                        ctx.status_notice,
                    );
                }
            }
            Action::FilterTextClear => {
                self.filter.spec.text.query.clear();
                self.on_filter_changed(
                    ctx.snap,
                    ctx.branch_colors,
                    ctx.async_graph_enabled,
                    ctx.i18n,
                    ctx.status_notice,
                );
            }
            Action::ToggleFilterRegex => {
                self.filter.spec.text.regex = !self.filter.spec.text.regex;
                self.on_filter_changed(
                    ctx.snap,
                    ctx.branch_colors,
                    ctx.async_graph_enabled,
                    ctx.i18n,
                    ctx.status_notice,
                );
            }
            Action::ToggleFilterCase => {
                self.filter.spec.text.match_case = !self.filter.spec.text.match_case;
                self.on_filter_changed(
                    ctx.snap,
                    ctx.branch_colors,
                    ctx.async_graph_enabled,
                    ctx.i18n,
                    ctx.status_notice,
                );
            }
            Action::ToggleNoMergesFilter => {
                self.filter.spec.no_merges = !self.filter.spec.no_merges;
                let state = ctx.i18n.on_off(self.filter.spec.no_merges);
                *ctx.status_notice = Some(ctx.i18n.t_fmt(
                    "toast.presentation_toggle",
                    &[("label", ctx.i18n.t("filter.no_merges")), ("state", state)],
                ));
                self.on_filter_changed(
                    ctx.snap,
                    ctx.branch_colors,
                    ctx.async_graph_enabled,
                    ctx.i18n,
                    ctx.status_notice,
                );
            }
            Action::LogSpeedSearchOpen => {
                self.filter.editing_text = false;
                self.speed_search.open();
                *ctx.focused_pane = PaneId::LogCommitTable;
                self.speed_search.graph_revision = self.graph.visible_revision;
                if let (Some(cached), Some(snap)) = (&self.graph.cached, ctx.snap)
                    && let Some(row) =
                        self.speed_search
                            .recompute(cached, snap, self.graph.selected_idx)
                {
                    self.graph.select_row(row);
                }
            }
            Action::LogSpeedSearchInput(ch) => {
                self.speed_search.active = true;
                self.speed_search.editing = true;
                self.speed_search.query.push(*ch);
                self.speed_search.graph_revision = self.graph.visible_revision;
                if let (Some(cached), Some(snap)) = (&self.graph.cached, ctx.snap)
                    && let Some(row) =
                        self.speed_search
                            .recompute(cached, snap, self.graph.selected_idx)
                {
                    self.graph.select_row(row);
                }
            }
            Action::LogSpeedSearchBackspace => {
                self.speed_search.query.pop();
                self.speed_search.graph_revision = self.graph.visible_revision;
                if let (Some(cached), Some(snap)) = (&self.graph.cached, ctx.snap)
                    && let Some(row) =
                        self.speed_search
                            .recompute(cached, snap, self.graph.selected_idx)
                {
                    self.graph.select_row(row);
                }
            }
            Action::LogSpeedSearchConfirm => {
                if self.speed_search.query.trim().is_empty() {
                    self.speed_search.close();
                } else {
                    self.speed_search.editing = false;
                }
            }
            Action::LogSpeedSearchClose => {
                self.speed_search.close();
            }
            Action::LogSpeedSearchStep(forward) => {
                self.sync_speed_search(ctx.snap);
                if self.speed_search.is_active()
                    && let Some(row) = self.speed_search.step(*forward)
                {
                    self.graph.jump_to(ctx.snap, row, true);
                }
            }
            Action::ApplyFilterField { field, expr } => {
                if let Err(err) = field.apply_i18n(
                    &mut self.filter.spec,
                    expr,
                    chrono::Local::now().timestamp(),
                    ctx.i18n,
                ) {
                    *ctx.status_notice = Some(err);
                    return true;
                }
                self.filter.record_history(*field, expr);
                self.on_filter_changed(
                    ctx.snap,
                    ctx.branch_colors,
                    ctx.async_graph_enabled,
                    ctx.i18n,
                    ctx.status_notice,
                );
            }
            Action::ClearFilterField(field) => {
                if *field == FilterField::Paths
                    && let Some(saved) = self.filter.saved_before_dir_history.take()
                {
                    self.filter.spec = saved;
                    self.filter.path_matches = None;
                    self.filter.path_requested = None;
                    self.on_filter_changed(
                        ctx.snap,
                        ctx.branch_colors,
                        ctx.async_graph_enabled,
                        ctx.i18n,
                        ctx.status_notice,
                    );
                    *ctx.status_notice = Some(ctx.i18n.t("toast.dir_history_restored").to_string());
                } else {
                    field.clear(&mut self.filter.spec);
                    self.on_filter_changed(
                        ctx.snap,
                        ctx.branch_colors,
                        ctx.async_graph_enabled,
                        ctx.i18n,
                        ctx.status_notice,
                    );
                }
            }
            Action::FilterLogByPath(path) => {
                self.filter.spec.paths = PathFilter::single(path);
                self.filter.path_matches = None;
                self.filter.path_requested = None;
                self.on_filter_changed(
                    ctx.snap,
                    ctx.branch_colors,
                    ctx.async_graph_enabled,
                    ctx.i18n,
                    ctx.status_notice,
                );
                if let Some(p) = self.filter.spec.paths.paths.first() {
                    *ctx.status_notice =
                        Some(ctx.i18n.t_fmt("toast.path_filter_set", &[("path", p)]));
                }
            }
            Action::ShowDirectoryHistory(path) => {
                if self.filter.saved_before_dir_history.is_none() {
                    self.filter.saved_before_dir_history = Some(self.filter.spec.clone());
                }
                self.filter.spec.paths = PathFilter::single(path);
                self.filter.path_matches = None;
                self.filter.path_requested = None;
                self.on_filter_changed(
                    ctx.snap,
                    ctx.branch_colors,
                    ctx.async_graph_enabled,
                    ctx.i18n,
                    ctx.status_notice,
                );
                if let Some(p) = self.filter.spec.paths.paths.first() {
                    *ctx.status_notice =
                        Some(ctx.i18n.t_fmt("toast.dir_history_entered", &[("path", p)]));
                }
            }
            Action::RestorePreDirHistoryFilter => {
                if let Some(saved) = self.filter.saved_before_dir_history.take() {
                    self.filter.spec = saved;
                    self.filter.path_matches = None;
                    self.filter.path_requested = None;
                    self.on_filter_changed(
                        ctx.snap,
                        ctx.branch_colors,
                        ctx.async_graph_enabled,
                        ctx.i18n,
                        ctx.status_notice,
                    );
                    *ctx.status_notice = Some(ctx.i18n.t("toast.dir_history_restored").to_string());
                }
            }
            Action::FilterLogByRange { label, oids } => {
                self.filter.spec.range =
                    Some(jbgit_core::RangeFilter::new(label.clone(), oids.iter().copied()));
                self.on_filter_changed(
                    ctx.snap,
                    ctx.branch_colors,
                    ctx.async_graph_enabled,
                    ctx.i18n,
                    ctx.status_notice,
                );
                let n_s = oids.len().to_string();
                *ctx.status_notice = Some(
                    ctx.i18n
                        .t_fmt("toast.range_filter_set", &[("label", label), ("count", &n_s)]),
                );
            }
            Action::ClearRangeFilter => {
                let had_cmp = self.branch_compare.take().is_some();
                if had_cmp {
                    self.details.compare_override = None;
                }
                if self.filter.spec.range.take().is_some() || had_cmp {
                    self.on_filter_changed(
                        ctx.snap,
                        ctx.branch_colors,
                        ctx.async_graph_enabled,
                        ctx.i18n,
                        ctx.status_notice,
                    );
                }
            }
            Action::ClearAllFilters => {
                self.filter.saved_before_dir_history = None;
                self.filter.spec = Default::default();
                self.on_filter_changed(
                    ctx.snap,
                    ctx.branch_colors,
                    ctx.async_graph_enabled,
                    ctx.i18n,
                    ctx.status_notice,
                );
            }
            _ => return false,
        }
        true
    }

    fn dispatch_log_action(&mut self, action: &Action, ctx: &mut LogDispatchCtx<'_>) -> bool {
        match action {
            Action::GraphCollapseSegment(id) => {
                let before = self.graph.visible_count();
                self.graph.collapse_segment_moving_cursor(*id);
                self.notify_fold(before, ctx);
            }
            Action::GraphExpandSegments(ids) => {
                let before = self.graph.visible_count();
                self.graph.set_segments_collapsed(ids, false);
                self.notify_fold(before, ctx);
            }
            Action::GraphExpandBekMerge(oid) => {
                if self.graph.expand_bek_merge(*oid) {
                    let short = oid.short();
                    *ctx.status_notice = Some(
                        ctx.i18n
                            .t_fmt("toast.linear_bek_expanded", &[("short", &short)]),
                    );
                }
            }
            Action::GraphExpandEdge { up_row, down_row } => {
                if let Some(bek_oid) = self
                    .graph
                    .cached
                    .as_ref()
                    .and_then(|c| c.visible.bek_merge_at_edge(*up_row, *down_row))
                {
                    if self.graph.expand_bek_merge(bek_oid) {
                        let short = bek_oid.short();
                        *ctx.status_notice = Some(
                            ctx.i18n
                                .t_fmt("toast.linear_bek_expanded", &[("short", &short)]),
                        );
                    }
                    return true;
                }
                let collapsed_ids = self
                    .graph
                    .cached
                    .as_ref()
                    .map(|c| c.collapsed_segments_at_edge(*up_row, *down_row))
                    .unwrap_or_default();
                if !collapsed_ids.is_empty() {
                    let before = self.graph.visible_count();
                    self.graph.set_segments_collapsed(&collapsed_ids, false);
                    self.notify_fold(before, ctx);
                } else if let Some(seg_id) = self
                    .graph
                    .cached
                    .as_ref()
                    .filter(|c| c.visible.can_collapse)
                    .and_then(|c| c.expanded_segment_at_edge(*up_row, *down_row))
                {
                    let before = self.graph.visible_count();
                    self.graph.collapse_segment_moving_cursor(seg_id);
                    self.notify_fold(before, ctx);
                }
            }
            Action::GraphExpandBelow(row) => {
                let ids = self
                    .graph
                    .cached
                    .as_ref()
                    .map(|c| c.collapsed_segments_below(*row))
                    .unwrap_or_default();
                if !ids.is_empty() {
                    let before = self.graph.visible_count();
                    self.graph.set_segments_collapsed(&ids, false);
                    self.notify_fold(before, ctx);
                } else if let Some(bek_oid) = self
                    .graph
                    .cached
                    .as_ref()
                    .and_then(|c| c.visible.bek_merge_at_row(*row))
                    && self.graph.expand_bek_merge(bek_oid)
                {
                    let short = bek_oid.short();
                    *ctx.status_notice = Some(
                        ctx.i18n
                            .t_fmt("toast.linear_bek_expanded", &[("short", &short)]),
                    );
                }
            }
            Action::GraphCollapseAll => {
                let before = self.graph.visible_count();
                self.graph.collapse_all();
                self.notify_fold(before, ctx);
            }
            Action::GraphExpandAll => {
                let before = self.graph.visible_count();
                self.graph.expand_all();
                self.notify_fold(before, ctx);
            }
            Action::SelectLogRow(idx) => {
                self.graph.select_row(*idx);
            }
            Action::ExtendLogSelectionTo(idx) => {
                self.graph.extend_selection_to(*idx);
            }
            Action::AddLogSelectionRangeTo(idx) => {
                self.graph.add_range_selection_to(*idx);
            }
            Action::ToggleLogRow(idx) => {
                self.graph.toggle_row_selection(*idx, false);
            }
            Action::ToggleLogRowSelection => {
                let idx = self.graph.selected_idx;
                self.graph.toggle_row_selection(idx, true);
            }
            Action::MoveLogCursorOnly(delta) => {
                self.graph.move_cursor_only(*delta);
            }
            Action::ClearLogMultiSelection => {
                self.graph.clear_multi_selection();
            }
            Action::ScrollLog(delta) => {
                self.graph.scroll_by(*delta, false);
            }
            Action::ExtendLogSelection(delta) => {
                self.graph.scroll_by(*delta, true);
            }
            Action::LogPage { half, down } => {
                let vh = self.graph.last_viewport_h.max(2);
                let step = if *half { vh / 2 } else { vh.saturating_sub(1) }.max(1) as isize;
                self.graph.scroll_by(if *down { step } else { -step }, false);
            }
            Action::LogEdge { bottom } => {
                let len = self.graph.navigable_row_count();
                self.graph.jump_to(
                    ctx.snap,
                    if *bottom { len.saturating_sub(1) } else { 0 },
                    false,
                );
            }
            Action::JumpToLogRow { row, center } => {
                self.graph.jump_to(ctx.snap, *row, *center);
            }
            Action::GoToParent(idx) => match self.graph.parent_row(ctx.snap, *idx) {
                Some(row) => self.graph.jump_to(ctx.snap, row, false),
                None => {
                    *ctx.status_notice = Some(if *idx == 0 {
                        ctx.i18n.t("toast.no_visible_parent").to_string()
                    } else {
                        let n_s = (*idx + 1).to_string();
                        ctx.i18n.t_fmt("toast.no_visible_parent_n", &[("n", &n_s)])
                    });
                }
            },
            Action::GoToChild => match self.graph.child_row() {
                Some(row) => self.graph.jump_to(ctx.snap, row, false),
                None => {
                    *ctx.status_notice = Some(ctx.i18n.t("toast.no_visible_child").to_string());
                }
            },
            Action::GoToChildCandidate(idx) => {
                let candidates = self.graph.child_candidates(ctx.snap);
                match candidates.get(*idx) {
                    Some(&(row, _)) => self.graph.jump_to(ctx.snap, row, false),
                    None => {
                        *ctx.status_notice =
                            Some(ctx.i18n.t("toast.no_visible_child").to_string());
                    }
                }
            }
            Action::GoToRevision(expr) => {
                let expr = expr.trim().to_string();
                match ctx.snap.and_then(|s| s.resolve_revision(&expr)) {
                    Some(oid) => {
                        let short = oid.short();
                        if self.graph.jump_to_oid(ctx.snap, &oid) {
                            *ctx.focused_pane = PaneId::LogCommitTable;
                            *ctx.status_notice = Some(
                                ctx.i18n
                                    .t_fmt("toast.goto_ok", &[("expr", &expr), ("short", &short)]),
                            );
                        } else {
                            *ctx.status_notice = Some(
                                ctx.i18n
                                    .t_fmt("toast.goto_hidden", &[("expr", &expr), ("short", &short)]),
                            );
                        }
                    }
                    None => {
                        *ctx.status_notice =
                            Some(ctx.i18n.t_fmt("toast.goto_not_found", &[("expr", &expr)]));
                    }
                }
            }
            Action::JumpToCommit(oid) => {
                if self.graph.jump_to_oid(ctx.snap, oid) {
                    *ctx.focused_pane = PaneId::LogCommitTable;
                } else {
                    let short = oid.short();
                    *ctx.status_notice = Some(
                        ctx.i18n
                            .t_fmt("toast.commit_not_visible", &[("short", &short)]),
                    );
                }
            }
            Action::NavLogBack => {
                self.graph.nav_back(ctx.snap);
            }
            Action::NavLogForward => {
                self.graph.nav_forward(ctx.snap);
            }
            _ => return false,
        }
        // Single commit-navigation exit: clear transient comparison / pending path overrides.
        self.details.compare_override = None;
        self.details.pending_select_path = None;
        if let Some(bc) = self.branch_compare.as_mut() {
            bc.tree_diff = false;
        }
        true
    }

    fn dispatch_details_action(&mut self, action: &Action, ctx: &mut LogDispatchCtx<'_>) -> bool {
        match action {
            Action::ChangesMove(ChangesHost::Log, delta) => {
                self.details.changes.move_selection(*delta, ctx.diff_caches);
                self.details.diff.reset_view_offsets();
            }
            Action::ChangesSelect(ChangesHost::Log, idx) => {
                self.details.changes.select_row(*idx, ctx.diff_caches);
                self.details.diff.reset_view_offsets();
            }
            Action::ChangesExpand(ChangesHost::Log, expand) => {
                self.details
                    .changes
                    .expand_or_collapse(*expand, ctx.diff_caches);
            }
            Action::ChangesToggleGroupByDir(ChangesHost::Log) => {
                self.details.changes.toggle_group_by_dir();
            }
            Action::ChangesExpandAll(ChangesHost::Log, expand) => {
                self.details.changes.expand_all(*expand, ctx.diff_caches);
            }
            Action::ConfirmLoadManyChanges => {
                if let Some(key) = self.details.many_changes_blocked.take() {
                    self.details.many_changes_confirmed = Some(key.clone());
                    self.details.changes.current_key = Some(key.clone());
                    self.details.changes.selected = 0;
                    self.details.details_scroll.reset();
                    self.details.diff.scroll.reset();
                    self.details.pending = None;
                    if !ctx.diff_caches.details.contains(&key) {
                        ctx.effects.push(Effect::RequestCommitDetails {
                            slot: RequestSlotId::LogCommitDetails,
                            key,
                        });
                    }
                }
            }
            Action::SwapCompareDirection => {
                let swapped = self
                    .details
                    .compare_override
                    .as_ref()
                    .or(self.details.changes.current_key.as_ref())
                    .and_then(|k| {
                        (k.mode == DetailsMode::CompareVersions && k.oids.len() >= 2)
                            .then_some((k.oids[1], k.oids[0]))
                    });
                if let Some((new_base, new_target)) = swapped {
                    let keep_path = self
                        .details
                        .selected_change(ctx.diff_caches)
                        .map(|c| c.path.clone());
                    self.start_compare_versions(
                        new_base,
                        new_target,
                        ctx.i18n,
                        ctx.status_notice,
                    );
                    self.details.pending_select_path = keep_path;
                } else if self.branch_compare.is_some() {
                    let _ = self.cycle_branch_compare_direction(
                        ctx.snap,
                        ctx.branch_colors,
                        ctx.async_graph_enabled,
                        ctx.i18n,
                        ctx.status_notice,
                    );
                }
            }
            Action::SwitchMergeParent => {
                let oids = self.graph.selected_oids(ctx.snap);
                let parents = match oids.as_slice() {
                    [one] => self
                        .graph
                        .commit_at(ctx.snap, self.graph.selected_idx)
                        .filter(|c| c.oid == *one)
                        .map_or(0, |c| c.parents.len()),
                    _ => 0,
                };
                if parents >= 2 {
                    self.details.merge_parent = (self.details.merge_parent + 1) % parents;
                    let cur_s = (self.details.merge_parent + 1).to_string();
                    let tot_s = parents.to_string();
                    *ctx.status_notice = Some(ctx.i18n.t_fmt(
                        "toast.merge_parent_switched",
                        &[("cur", &cur_s), ("total", &tot_s)],
                    ));
                } else {
                    *ctx.status_notice = Some(ctx.i18n.t("toast.not_merge_commit").to_string());
                }
            }
            Action::ToggleDiffPreview => {
                self.details.diff_preview = !self.details.diff_preview;
                self.details.diff.scroll.reset();
            }
            Action::DiffCycleDock => {
                self.details.diff_dock = self.details.diff_dock.next();
                let dock_lbl = ctx.i18n.diff_dock_label(self.details.diff_dock);
                *ctx.status_notice =
                    Some(ctx.i18n.t_fmt("toast.diff_dock_set", &[("dock", dock_lbl)]));
            }
            Action::ZoomDiff(on) => {
                self.details.diff.zoomed =
                    *on && self.details.selected_change(ctx.diff_caches).is_some();
                self.details.diff.scroll.reset();
            }
            Action::DiffNextFile(DiffScope::Log, delta) => {
                self.details.select_next_file(*delta, ctx.diff_caches);
            }
            Action::DiffJumpHunk(DiffScope::Log, delta) => {
                self.details.jump_diff_hunk(*delta, ctx.diff_caches);
            }
            Action::DiffToggleSideBySide(DiffScope::Log) => {
                self.details.diff.toggle_side_by_side();
            }
            Action::DiffCycleContext(DiffScope::Log) => {
                self.details.diff.cycle_context();
                let ctx_lbl = ctx.i18n.diff_context_label(self.details.diff.context);
                *ctx.status_notice = Some(
                    ctx.i18n
                        .t_fmt("toast.diff_context_set", &[("ctx", ctx_lbl)]),
                );
            }
            Action::DiffCycleWhitespace(DiffScope::Log) => {
                self.details.diff.cycle_whitespace();
                let ws_lbl = ctx.i18n.diff_whitespace_label(self.details.diff.whitespace);
                *ctx.status_notice = Some(
                    ctx.i18n
                        .t_fmt("toast.diff_whitespace_set", &[("ws", ws_lbl)]),
                );
            }
            Action::DiffToggleSoftWrap(DiffScope::Log) => {
                self.details.diff.toggle_soft_wrap();
                let on_off = ctx.i18n.on_off(self.details.diff.soft_wrap);
                *ctx.status_notice = Some(
                    ctx.i18n
                        .t_fmt("toast.diff_soft_wrap_set", &[("state", on_off)]),
                );
            }
            Action::DiffScrollHoriz(DiffScope::Log, delta) => {
                self.details.diff.scroll_horiz(*delta);
            }
            Action::DiffSearchOpen(DiffScope::Log) => {
                self.details.diff.search.open();
            }
            Action::DiffSearchChar(DiffScope::Log, ch) => {
                self.details.diff.search.push_char(*ch);
                let diff = self.details.current_diff(ctx.diff_caches).cloned();
                let tab = self.details.diff.tab_size();
                self.details.diff.refresh_search(diff.as_ref(), tab);
            }
            Action::DiffSearchBackspace(DiffScope::Log) => {
                self.details.diff.search.backspace();
                let diff = self.details.current_diff(ctx.diff_caches).cloned();
                let tab = self.details.diff.tab_size();
                self.details.diff.refresh_search(diff.as_ref(), tab);
            }
            Action::DiffSearchClear(DiffScope::Log) => {
                self.details.diff.search.clear_query();
            }
            Action::DiffSearchConfirm(DiffScope::Log) => {
                self.details.diff.search.finish_editing();
            }
            Action::DiffSearchClose(DiffScope::Log) => {
                self.details.diff.search.close();
            }
            Action::DiffSearchJump(DiffScope::Log, delta) => {
                let diff = self.details.current_diff(ctx.diff_caches).cloned();
                let tab = self.details.diff.tab_size();
                self.details.diff.jump_search(*delta, diff.as_ref(), tab);
            }
            Action::DiffScroll(DiffScope::Log, delta) => {
                self.scroll_pane(ScrollPane::LogDiff, *delta, ctx.diff_caches);
            }
            Action::ScrollDetails(delta) => {
                self.scroll_pane(ScrollPane::LogDetails, *delta, ctx.diff_caches);
            }
            _ => return false,
        }
        true
    }

    pub fn reconcile_requests(&mut self, now: Instant, ctx: &mut LogReconcileCtx<'_>) {
        self.ensure_graph_cache(ctx.snap, ctx.branch_colors, ctx.async_graph_enabled);
        if self.uses_async_graph(ctx.snap, ctx.async_graph_enabled) {
            let path_matches = self
                .filter
                .path_matches
                .as_ref()
                .filter(|pm| {
                    ctx.snap.is_some_and(|s| {
                        pm.paths == self.filter.spec.paths.paths && Arc::ptr_eq(&pm.snapshot, s)
                    })
                })
                .map(|pm| Arc::clone(&pm.oids));
            let favorites = self.favorite_branch_names(ctx.snap);
            if let Some(job) = self.graph.take_async_rebuild_job(
                ctx.snap,
                ctx.branch_colors,
                &self.filter.spec,
                favorites,
                path_matches,
            ) {
                ctx.effects.push(Effect::RebuildLogGraph(Box::new(job)));
            }
        }

        // Reconcile async `Already Cherry-Picked` query on its own RequestSlot.
        if let Some(snap) = ctx.snap {
            if let Some(desired_key) = self.desired_cherry_picked_key(snap) {
                let already_loaded = self
                    .cherry_picked
                    .as_ref()
                    .is_some_and(|(k, _)| *k == desired_key);
                let already_requested = self.cherry_picked_requested.as_ref() == Some(&desired_key);
                if !already_loaded && !already_requested {
                    self.cherry_picked_requested = Some(desired_key.clone());
                    ctx.effects.push(Effect::RequestCherryPicked(desired_key));
                }
            } else {
                self.cherry_picked_requested = None;
            }
        }

        if let Some(s) = ctx.snap
            && self.filter.path_matches_pending(Some(s))
        {
            let paths = self.filter.spec.paths.paths.clone();
            let already = self
                .filter
                .path_requested
                .as_ref()
                .is_some_and(|req| req.paths == paths && Arc::ptr_eq(&req.snapshot, s));
            if !already {
                self.filter.path_requested = Some(PathRequest {
                    paths: paths.clone(),
                    snapshot: Arc::clone(s),
                });
                ctx.effects
                    .push(Effect::RequestPathMatches(paths, Arc::clone(s)));
            }
        }

        self.sync_affected_filter();

        let desired = if let Some(cmp) = &self.details.compare_override {
            Some(cmp.clone())
        } else {
            let oids = self.graph.selected_oids(ctx.snap);
            if self.details.changes.current_key.as_ref().map(|k| &k.oids) != Some(&oids) {
                self.details.merge_parent = 0;
            }
            let parent_index = if oids.len() == 1 {
                self.details.merge_parent
            } else {
                0
            };
            (!oids.is_empty()).then_some(DetailsKey::commits(oids, parent_index))
        };
        let many_blocked = desired.as_ref().is_some_and(|k| {
            k.mode == DetailsMode::Commits
                && k.oids.len() > MAX_AGGREGATED_COMMITS
                && self.details.many_changes_confirmed.as_ref() != Some(k)
                && !ctx.diff_caches.details.contains(k)
        });
        if many_blocked {
            self.details.many_changes_blocked = desired;
            self.details.changes.current_key = None;
            self.details.changes.selected = 0;
            self.details.details_scroll.reset();
            self.details.diff.scroll.reset();
            self.details.pending = None;
            self.details.diff.zoomed = false;
        } else {
            self.details.many_changes_blocked = None;
            if self.details.many_changes_confirmed.as_ref() != desired.as_ref() {
                self.details.many_changes_confirmed = None;
            }
            if desired != self.details.changes.current_key {
                self.details.changes.current_key = desired.clone();
                self.details.changes.selected = 0;
                self.details.details_scroll.reset();
                self.details.diff.reset_view_offsets();
                self.details.pending = match desired {
                    Some(k) if ctx.diff_caches.details.get(&k).is_some() => None,
                    Some(k) if self.details.compare_override.is_some() => {
                        ctx.effects.push(Effect::RequestCommitDetails {
                            slot: RequestSlotId::LogCommitDetails,
                            key: k,
                        });
                        None
                    }
                    Some(k) => Some((k, now + DETAILS_DEBOUNCE)),
                    None => None,
                };
                if self.details.current_bundle(ctx.diff_caches).is_none() {
                    self.details.diff.zoomed = false;
                }
            }
        }

        if let Some((path, found)) = self.details.try_select_pending_path(ctx.diff_caches)
            && !found
        {
            *ctx.status_notice = Some(
                ctx.i18n
                    .t_fmt("toast.compare_local_no_diff", &[("path", &path)]),
            );
        }

        if self.presentation.show_signature_column
            && let (Some(snap), Some(cached)) = (ctx.snap, &self.graph.cached)
        {
            let count = cached.visible_count();
            if count > 0 {
                let vis_start = self.graph.scroll_offset.min(count);
                let vis_end = vis_start.saturating_add(48).min(count);
                let pad_start = vis_start.saturating_sub(8);
                let pad_end = vis_end.saturating_add(8).min(count);
                let mut candidates = Vec::with_capacity(64);
                let mut push_row = |row: usize| {
                    if candidates.len() >= 64 {
                        return;
                    }
                    if let Some(c) = cached.commit_at_visible_row(snap, row)
                        && ctx.diff_caches.signatures.peek(&c.oid).is_none()
                        && !candidates.contains(&c.oid)
                    {
                        candidates.push(c.oid);
                    }
                };
                for row in vis_start..vis_end {
                    push_row(row);
                }
                push_row(self.graph.selected_idx);
                for row in pad_start..vis_start {
                    push_row(row);
                }
                for row in vis_end..pad_end {
                    push_row(row);
                }
                if !candidates.is_empty()
                    && candidates.iter().any(|oid| !self.sig_requested.contains(oid))
                {
                    self.sig_requested = candidates.clone();
                    ctx.effects.push(Effect::RequestCommitSignatures(candidates));
                }
            }
        }

        if let Some(key) = &self.details.changes.current_key
            && let [oid] = key.oids.as_slice()
            && let (Some(s), Some(cached)) = (ctx.snap, &self.graph.cached)
            && let Some(node) = cached.permanent.linear.node_of_oid(oid)
        {
            let oid = *oid;
            if let Some(token) = self.details.request_containing_branches(oid) {
                ctx.effects
                    .push(Effect::ComputeContainingBranches(ContainingBranchesJob {
                        oid,
                        node,
                        token,
                        graph: Arc::clone(&cached.permanent),
                        snapshot: Arc::clone(s),
                    }));
            }
        }

        if self.details.diff_visible()
            && let Some(key) = self.details.desired_diff_key(ctx.diff_caches)
        {
            if let Some(cached) = ctx.diff_caches.get_diff(&key) {
                if ctx.diff_caches.get_syntax(&key).is_none()
                    && ctx.syntax_enabled
                    && !cached.is_binary
                    && !cached.is_too_large
                    && cached.lfs.is_none()
                    && !cached.hunks.is_empty()
                    && let Some((epoch, cancel)) = ctx.file_syntax.begin_cancelable(cached.key.clone())
                {
                    ctx.effects.push(Effect::RequestSyntaxHighlight(SyntaxJob {
                        epoch,
                        target: SyntaxTarget::File {
                            key: SyntaxJobKey::File(cached.key.clone()),
                            diff: cached,
                        },
                        cancel,
                    }));
                }
            } else if self.details.diff.requested.as_ref() != Some(&key) {
                if ctx.file_syntax.is_busy() {
                    ctx.file_syntax.invalidate();
                }
                self.details.diff.requested = Some(key.clone());
                ctx.effects.push(Effect::RequestFileDiff {
                    slot: RequestSlotId::LogFileDiff,
                    key,
                });
            }
        }
    }
}
