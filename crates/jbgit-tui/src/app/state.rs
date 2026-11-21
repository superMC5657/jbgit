use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use jbgit_core::{
    CommitSummary, FileDiff, FileDiffKey, GitConsoleEntry, PaneId, RepoSnapshot, SplitAxis,
    SplitterId, StageStatusSnapshot, StageTreeRowKind, WorkspaceTab,
};
use jbgit_engine::{StageDiffKey, ThreeDocStageDiff};
use ratatui::layout::Rect;
use rustc_hash::FxHashMap;

use crate::graph::LogGraphState;
use crate::graph::state::{ASYNC_GRAPH_THRESHOLD, FilterInputs};
use crate::hit_test::{HitTarget, HitTestRegistry};
use crate::log_view::{
    BranchesPaneState, DetailsPaneState, FILE_HISTORY_G_WINDOW, FileHistoryState, FilterBarState,
    LogPresentation,
};
use crate::stage_view::StageWorkbenchState;
use crate::stash_view::StashTabState;
use crate::theme::{BranchColorManager, GlyphMode, ThemePalette};
use crate::widgets::SplitterState;

use super::{
    ClickRecord, ConflictMergeState, DragState, Effect, MenuHover, ModalLayer, Notification,
    ScrollPane, SyntaxJob, SyntaxJobKey, SyntaxJobSlot, SyntaxTarget, ViewportScrollMetrics,
    apply_scroll_delta,
};

pub struct AppState {
    pub theme: ThemePalette,
    pub glyph_mode: GlyphMode,
    pub branch_colors: BranchColorManager,
    /// Compact References View: only the first ref keeps its text.
    pub compact_refs: bool,
    /// `Show Tag Names`: when off, tags collapse to icons.
    pub show_tag_names: bool,
    pub log_graph: LogGraphState,
    pub branches: BranchesPaneState,
    pub log_filter: FilterBarState,
    pub presentation: LogPresentation,
    /// `Graph Renderer: Auto | Vector | Character Grid` + terminal graphics capabilities.
    pub graph_renderer: crate::graph::GraphRenderer,
    pub details: DetailsPaneState,
    pub active_tab: WorkspaceTab,
    pub focused_pane: PaneId,
    pub splitters: SplitterState,
    pub splitter_containers: FxHashMap<SplitterId, (SplitAxis, Rect)>,
    pub terminal_area: Rect,
    pub modal_stack: Vec<ModalLayer>,
    pub hit_registry: HitTestRegistry,
    pub drag_state: Option<DragState>,
    pub last_click: Option<ClickRecord>,
    pub hover_pos: Option<(u16, u16)>,
    pub hover_target: Option<HitTarget>,

    pub repo_path_display: PathBuf,
    pub repo_error: Option<String>,
    pub repo_snapshot: Option<Arc<RepoSnapshot>>,
    pub stage_status: Option<Arc<StageStatusSnapshot>>,
    pub stage_workbench: StageWorkbenchState,

    pub latest_repo_epoch: u64,
    pub latest_stage_epoch: u64,
    pub latest_stage_diff_epoch: u64,
    pub latest_commit_details_epoch: u64,
    pub latest_file_diff_epoch: u64,
    pub latest_file_history_epoch: u64,
    pub latest_path_epoch: u64,

    pub file_syntax: SyntaxJobSlot<FileDiffKey>,
    pub stage_syntax: SyntaxJobSlot<StageDiffKey>,

    pub commit_stage_selected_idx: usize,
    pub hook_error_output: Option<String>,

    pub console_entries: Vec<GitConsoleEntry>,
    pub console_selected_idx: usize,
    pub console_output_scroll: usize,
    pub scroll_metrics: ViewportScrollMetrics,

    pub next_cli_id: u64,
    pub loading_snapshot: bool,
    pub async_graph_enabled: bool,
    pub status_notice: Option<String>,
    pub spinner_frame: usize,
    pub needs_redraw: bool,
    pub should_quit: bool,

    /// Footer notification (`[Restore Branch]`, operation errors).
    pub notification: Option<Notification>,
    /// Hover-to-open timer of cascading submenus.
    pub menu_hover: Option<MenuHover>,
    /// Number of Git write operations in flight.
    pub git_ops_running: usize,
    pub stash: StashTabState,
    pub conflict_merge: Option<ConflictMergeState>,
    pub file_history: Option<FileHistoryState>,
    #[allow(dead_code)]
    pub config: crate::config::AppConfig,
    pub i18n: std::sync::Arc<crate::i18n::I18n>,
    pub pending_keys: Vec<crossterm::event::KeyEvent>,
    pub pending_key_at: Option<std::time::Instant>,
}

impl AppState {
    pub fn new(repo_path_display: PathBuf, repo_error: Option<String>) -> Self {
        let config = crate::config::AppConfig::load(Some(&repo_path_display));
        Self::new_with_config(repo_path_display, repo_error, config)
    }

    pub fn new_with_config(
        repo_path_display: PathBuf,
        repo_error: Option<String>,
        config: crate::config::AppConfig,
    ) -> Self {
        use crate::config::{DefaultTab, GlyphModePref, GraphBackendPref};

        let theme = ThemePalette::from_config(&config.theme);
        let branch_colors = theme.branch_color_manager();
        let (active_tab, focused_pane) = match config.jbgit.general.default_tab {
            DefaultTab::Commit => (WorkspaceTab::Commit, WorkspaceTab::Commit.default_pane()),
            DefaultTab::Branches => (WorkspaceTab::Log, PaneId::LogBranchesTree),
            DefaultTab::Stash => (WorkspaceTab::Stash, WorkspaceTab::Stash.default_pane()),
            DefaultTab::Console => (WorkspaceTab::Console, WorkspaceTab::Console.default_pane()),
            DefaultTab::Log => (WorkspaceTab::Log, WorkspaceTab::Log.default_pane()),
        };
        let glyph_mode = match config.theme.glyphs.mode {
            GlyphModePref::Unicode => GlyphMode::Unicode,
            GlyphModePref::NerdFont => GlyphMode::NerdFont,
        };
        let compact_refs = config.jbgit.graph.compact_refs;
        let graph_mode = match config.jbgit.graph.backend {
            GraphBackendPref::Auto => crate::graph::GraphRendererMode::Auto,
            GraphBackendPref::Vector => crate::graph::GraphRendererMode::Vector,
            GraphBackendPref::Cell => crate::graph::GraphRendererMode::CharacterGrid,
        };
        let diff_context = jbgit_core::DiffContext::from_lines(config.jbgit.diff.context_lines);

        let locales_dir =
            crate::config::discovery::find_config_file("locales", Some(&repo_path_display))
                .or_else(|| {
                    let global_loc = crate::config::discovery::global_config_dir().join("locales");
                    if global_loc.is_dir() {
                        Some(global_loc)
                    } else {
                        None
                    }
                });
        let i18n = std::sync::Arc::new(crate::i18n::I18n::load(
            config.jbgit.general.language.as_str(),
            locales_dir.as_deref(),
        ));

        let details = DetailsPaneState {
            diff_context,
            ..Default::default()
        };
        let stage_workbench = StageWorkbenchState {
            diff_context,
            ..Default::default()
        };
        let mut stash = StashTabState::default();
        stash.details.diff_context = diff_context;

        Self {
            theme,
            glyph_mode,
            branch_colors,
            compact_refs,
            show_tag_names: true,
            log_graph: LogGraphState::default(),
            branches: BranchesPaneState::default(),
            log_filter: FilterBarState::default(),
            presentation: LogPresentation::default(),
            graph_renderer: crate::graph::GraphRenderer::new(
                graph_mode,
                crate::graph::GraphicsCapabilities::default(),
            ),
            details,
            active_tab,
            focused_pane,
            splitters: SplitterState::with_default_ratios(
                config.jbgit.layout.splitter_defaults(),
            ),
            splitter_containers: FxHashMap::default(),
            terminal_area: Rect::ZERO,
            modal_stack: Vec::new(),
            hit_registry: HitTestRegistry::new(),
            drag_state: None,
            last_click: None,
            hover_pos: None,
            hover_target: None,
            repo_path_display,
            repo_error,
            repo_snapshot: None,
            stage_status: None,
            stage_workbench,
            latest_repo_epoch: 0,
            latest_stage_epoch: 0,
            latest_stage_diff_epoch: 0,
            latest_commit_details_epoch: 0,
            latest_file_diff_epoch: 0,
            latest_file_history_epoch: 0,
            latest_path_epoch: 0,
            file_syntax: SyntaxJobSlot::default(),
            stage_syntax: SyntaxJobSlot::default(),
            commit_stage_selected_idx: 0,
            hook_error_output: None,
            console_entries: Vec::new(),
            console_selected_idx: 0,
            console_output_scroll: 0,
            scroll_metrics: ViewportScrollMetrics::default(),
            next_cli_id: 1,
            loading_snapshot: false,
            async_graph_enabled: false,
            status_notice: None,
            spinner_frame: 0,
            needs_redraw: true,
            should_quit: false,
            notification: None,
            menu_hover: None,
            git_ops_running: 0,
            stash,
            conflict_merge: None,
            file_history: None,
            config,
            i18n,
            pending_keys: Vec::new(),
            pending_key_at: None,
        }
    }

    pub(crate) fn uses_async_graph(&self) -> bool {
        self.async_graph_enabled
            && self
                .repo_snapshot
                .as_ref()
                .is_some_and(|s| s.commits.len() >= ASYNC_GRAPH_THRESHOLD)
    }

    pub fn ensure_graph_cache(&mut self) {
        if !self.uses_async_graph() {
            let path_matches = self
                .log_filter
                .current_path_matches(self.repo_snapshot.as_ref());
            self.log_graph.ensure_cache(
                self.repo_snapshot.as_ref(),
                &self.branch_colors,
                &FilterInputs {
                    spec: &self.log_filter.spec,
                    path_matches,
                },
            );
        }
        self.branches.sync_model(self.repo_snapshot.as_ref(), false);
    }

    pub fn log_commit_at(&self, visible_row: usize) -> Option<&CommitSummary> {
        self.log_graph
            .commit_at(self.repo_snapshot.as_ref(), visible_row)
    }

    pub fn console_output_max_scroll(&self) -> usize {
        let Some(entry) = self.console_entries.get(self.console_selected_idx) else {
            return 0;
        };
        let total_lines = crate::views::console::console_entry_total_lines(entry);
        match self.scroll_metrics.console_output {
            Some(bound) if bound.viewport_h > 0 => total_lines.saturating_sub(bound.viewport_h),
            _ => total_lines.saturating_sub(1),
        }
    }

    pub fn console_page_step(&self) -> isize {
        self.scroll_metrics
            .console_output
            .filter(|b| b.viewport_h > 0)
            .map(|b| b.viewport_h.saturating_sub(1).max(1) as isize)
            .unwrap_or(10)
    }

    pub fn console_half_page_step(&self) -> isize {
        self.scroll_metrics
            .console_output
            .filter(|b| b.viewport_h > 0)
            .map(|b| (b.viewport_h / 2).max(1) as isize)
            .unwrap_or(10)
    }

    /// Scrolls `pane` by signed `delta`, clamped to its recorded or fallback upper bound.
    pub fn scroll_pane(&mut self, pane: ScrollPane, delta: isize) {
        let recorded = self.scroll_metrics.bound(pane).map(|b| b.max_scroll);
        match pane {
            ScrollPane::ConsoleOutput => {
                let max_scroll = self.console_output_max_scroll();
                self.console_output_scroll =
                    apply_scroll_delta(self.console_output_scroll, delta, max_scroll);
            }
            ScrollPane::LogDiff => {
                let max_scroll =
                    recorded.unwrap_or_else(|| self.details.fallback_diff_max_scroll());
                self.details.diff_scroll =
                    apply_scroll_delta(self.details.diff_scroll, delta, max_scroll);
            }
            ScrollPane::LogDetails => {
                let max_scroll = recorded.unwrap_or(usize::MAX);
                self.details.details_scroll =
                    apply_scroll_delta(self.details.details_scroll, delta, max_scroll);
            }
            ScrollPane::StashDiff => {
                let max_scroll =
                    recorded.unwrap_or_else(|| self.stash.details.fallback_diff_max_scroll());
                self.stash.details.diff_scroll =
                    apply_scroll_delta(self.stash.details.diff_scroll, delta, max_scroll);
            }
            ScrollPane::StageDiff => {
                let max_scroll = recorded
                    .unwrap_or_else(|| self.stage_workbench.diff_rows().len().saturating_sub(1));
                self.stage_workbench.diff_scroll =
                    apply_scroll_delta(self.stage_workbench.diff_scroll, delta, max_scroll);
            }
            ScrollPane::FileHistoryDiff => {
                let max_scroll = recorded.unwrap_or_else(|| {
                    self.file_history
                        .as_ref()
                        .map_or(usize::MAX, |fh| fh.fallback_diff_max_scroll(&self.details.diff_cache))
                });
                if let Some(fh) = &mut self.file_history {
                    fh.diff_scroll = apply_scroll_delta(fh.diff_scroll, delta, max_scroll);
                }
            }
        }
    }

    /// Stores the latest frame's `metrics` and clamps all tracked scroll offsets.
    pub fn clamp_scroll_metrics(&mut self, metrics: ViewportScrollMetrics) {
        self.scroll_metrics = metrics;
        if let Some(b) = metrics.console_output {
            self.console_output_scroll = self.console_output_scroll.min(b.max_scroll);
        }
        if let Some(b) = metrics.log_diff {
            self.details.diff_scroll = self.details.diff_scroll.min(b.max_scroll);
        }
        if let Some(b) = metrics.log_details {
            self.details.details_scroll = self.details.details_scroll.min(b.max_scroll);
        }
        if let Some(b) = metrics.stash_diff {
            self.stash.details.diff_scroll = self.stash.details.diff_scroll.min(b.max_scroll);
        }
        if let Some(b) = metrics.stage_diff {
            self.stage_workbench.diff_scroll = self.stage_workbench.diff_scroll.min(b.max_scroll);
        }
        if let Some(b) = metrics.file_history_diff
            && let Some(fh) = &mut self.file_history
        {
            fh.diff_scroll = fh.diff_scroll.min(b.max_scroll);
        }
    }

    pub fn has_active_tasks(&self) -> bool {
        self.loading_snapshot
            || self.log_graph.rebuild_in_progress
            || self.stage_workbench.commit_in_progress
            || self.git_ops_running > 0
            || self.file_history.as_ref().is_some_and(|fh| fh.loading)
            || self.console_entries.iter().any(GitConsoleEntry::is_running)
    }

    /// Next instant the main loop must wake up for (details debounce, menu hover, or key chord timeout).
    pub fn next_deadline(&self) -> Option<Instant> {
        let chord = self
            .pending_key_at
            .map(|t| t + crate::keymap::KEY_CHORD_WINDOW);
        [
            self.details.next_deadline(),
            self.stage_workbench.next_deadline(),
            self.file_history
                .as_ref()
                .and_then(FileHistoryState::next_deadline),
            self.menu_hover.map(|h| h.due),
            self.log_filter.next_deadline(),
            chord,
        ]
        .into_iter()
        .flatten()
        .min()
    }

    pub fn spinner_char(&self) -> &'static str {
        const FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
        FRAMES[self.spinner_frame % FRAMES.len()]
    }

    pub fn stage_item_count(&self) -> usize {
        self.stage_workbench.rows.len()
    }

    pub fn stage_entry_at(&self, idx: usize) -> Option<jbgit_core::StageEntry> {
        let row = self.stage_workbench.rows.get(idx)?;
        match &row.kind {
            StageTreeRowKind::File {
                path,
                old_path,
                status_kind,
                ..
            } => Some(jbgit_core::StageEntry {
                path: path.clone(),
                old_path: old_path.clone(),
                kind: *status_kind,
            }),
            StageTreeRowKind::AmendFile {
                path,
                old_path,
                status_kind,
            } => Some(jbgit_core::StageEntry {
                path: path.clone(),
                old_path: old_path.clone(),
                kind: *status_kind,
            }),
            _ => None,
        }
    }

    pub(crate) fn effective_focus_ring(&self) -> Vec<PaneId> {
        self.active_tab
            .focus_ring()
            .iter()
            .copied()
            .filter(|p| {
                *p != PaneId::LogBranchesTree
                    || !self.branches.is_collapsed(self.terminal_area.width)
            })
            .collect()
    }

    pub(crate) fn on_filter_changed(&mut self) {
        // Any explicit re-filter also covers a pending debounced text edit.
        self.log_filter.clear_text_debounce();
        self.log_graph.filter_generation = self.log_graph.filter_generation.wrapping_add(1);
        self.ensure_graph_cache();
        if let Some(err) = &self.log_graph.filter_error {
            self.status_notice = Some(
                self.i18n
                    .t_fmt("toast.filter_error", &[("err", &err.to_string())]),
            );
        }
    }

    pub(crate) fn navigate_to_branch_node(&mut self) {
        let Some(node) = self.branches.selected_node() else {
            return;
        };
        let Some(oid) = node.target_oid else {
            return;
        };
        let label = node.ref_name.clone().unwrap_or_default();
        if !self
            .log_graph
            .jump_to_oid(self.repo_snapshot.as_ref(), &oid)
        {
            self.status_notice = Some(
                self.i18n
                    .t_fmt("toast.tip_hidden_by_filter", &[("label", &label)]),
            );
        }
    }

    pub fn flush_pending_at(&mut self, now: Instant, effects: &mut Vec<Effect>) {
        if self
            .pending_key_at
            .is_some_and(|t| now.saturating_duration_since(t) >= crate::keymap::KEY_CHORD_WINDOW)
        {
            self.pending_keys.clear();
            self.pending_key_at = None;
        }
        crate::input_menus::flush_menu_hover(self, now);
        self.stage_workbench.flush_draft_due(now);
        if self.log_filter.take_due_text_apply(now) {
            self.on_filter_changed();
        }
        if let Some((key, _)) = self.details.pending.take()
            && !self.details.cache.contains(&key)
        {
            effects.push(Effect::RequestCommitDetails(key));
        }
        if let Some((key, at)) = self.stage_workbench.pending_diff.take() {
            if at <= now {
                if let Some(cached) = self.stage_workbench.diff_cache.get(&key).cloned() {
                    if self.stage_workbench.current_diff_key.as_ref() == Some(&key) {
                        self.stage_workbench.diff_syntax =
                            self.stage_workbench.diff_syntax_cache.get(&key).cloned();
                        self.stage_workbench.current_diff = Some(Arc::clone(&cached));
                        if self.stage_workbench.diff_syntax.is_none() {
                            self.schedule_stage_syntax(cached, effects);
                        }
                    }
                } else if self.stage_workbench.requested_diff.as_ref() != Some(&key) {
                    self.stage_workbench.requested_diff = Some(key.clone());
                    effects.push(Effect::RequestStageFileDiff(key));
                }
            } else {
                self.stage_workbench.pending_diff = Some((key, at));
            }
        }
        if let Some(fh) = &mut self.file_history
            && fh
                .pending_g
                .is_some_and(|t| now.saturating_duration_since(t) >= FILE_HISTORY_G_WINDOW)
        {
            fh.pending_g = None;
            self.jump_file_history_to_log(effects);
        }
        if let Some(fh) = &mut self.file_history
            && let Some((key, at)) = fh.pending_diff.take()
        {
            if at <= now {
                if !self.details.diff_cache.contains(&key)
                    && fh.requested_diff.as_ref() != Some(&key)
                {
                    fh.requested_diff = Some(key.clone());
                    effects.push(Effect::RequestFileDiff(key));
                }
            } else {
                fh.pending_diff = Some((key, at));
            }
        }
    }

    pub(crate) fn schedule_file_syntax(
        &mut self,
        diff: Arc<FileDiff>,
        effects: &mut Vec<Effect>,
    ) {
        if !self.config.jbgit.diff.syntax_highlight
            || diff.is_binary
            || diff.is_too_large
            || diff.hunks.is_empty()
        {
            return;
        }
        if let Some((epoch, cancel)) = self.file_syntax.begin(&diff.key) {
            effects.push(Effect::RequestSyntaxHighlight(SyntaxJob {
                key: SyntaxJobKey::File(diff.key.clone()),
                epoch,
                target: SyntaxTarget::File(diff),
                cancel,
            }));
        }
    }

    pub(crate) fn schedule_stage_syntax(
        &mut self,
        diff: Arc<ThreeDocStageDiff>,
        effects: &mut Vec<Effect>,
    ) {
        if !self.config.jbgit.diff.syntax_highlight
            || diff.is_binary
            || diff.is_too_large
            || diff.hunks.is_empty()
        {
            return;
        }
        if let Some((epoch, cancel)) = self.stage_syntax.begin(&diff.key) {
            effects.push(Effect::RequestSyntaxHighlight(SyntaxJob {
                key: SyntaxJobKey::Stage(diff.key.clone()),
                epoch,
                target: SyntaxTarget::Stage(diff),
                cancel,
            }));
        }
    }

    pub(crate) fn reconcile_stash_syntax(&mut self, effects: &mut Vec<Effect>) {
        if self.active_tab != WorkspaceTab::Stash {
            return;
        }
        let Some(dk) = self.stash.diff_key() else {
            return;
        };
        if self.stash.details.diff_syntax.get(&dk).is_none()
            && let Some(diff) = self.stash.details.diff_cache.peek(&dk).cloned()
        {
            self.schedule_file_syntax(diff, effects);
        }
    }
}
