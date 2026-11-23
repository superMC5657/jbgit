use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use jbgit_core::{
    BlameBundle, BlameKey, CommitSummary, FileDiff, FileDiffKey, Oid, PaneId, RepoSnapshot,
    RequestSlot, RequestSlotId, RequestSlotTable, SplitAxis, SplitterId, WorkspaceTab,
};
use jbgit_engine::StageDiffKey;
use ratatui::layout::Rect;
use rustc_hash::FxHashMap;

use crate::console_workbench::ConsoleWorkbench;
use crate::diff_session::DiffCacheStore;
use crate::hit_test::{HitTarget, HitTestRegistry};
use crate::log_view::{
    AnnotateState, FILE_HISTORY_G_WINDOW, FileHistoryState, LogWorkbench, PushDialogState,
};
use crate::stage_view::CommitWorkbench;
use crate::stash_view::StashWorkbench;
use crate::theme::{BranchColorManager, GlyphMode, ThemePalette};
use crate::widgets::SplitterState;

use super::{
    ClickRecord, DragState, Effect, MenuHover, ModalLayer, ModalStack, Notification, ScrollPane,
    SyntaxJob, SyntaxJobKey, SyntaxTarget,
};

pub struct AppState {
    pub theme: ThemePalette,
    pub glyph_mode: GlyphMode,
    pub branch_colors: BranchColorManager,
    pub active_tab: WorkspaceTab,
    pub focused_pane: PaneId,
    pub splitters: SplitterState,
    pub terminal_area: Rect,
    pub modal_stack: ModalStack,
    pub hit_registry: HitTestRegistry,
    pub diff_caches: DiffCacheStore,
    pub repo_snapshot: Option<Arc<RepoSnapshot>>,
    pub log: LogWorkbench,
    pub stage_workbench: CommitWorkbench,
    pub stash: StashWorkbench,
    pub console: ConsoleWorkbench,
    #[allow(dead_code)]
    pub config: crate::config::AppConfig,
    pub i18n: std::sync::Arc<crate::i18n::I18n>,

    // ── Crate-internal coordination state ──
    pub(crate) splitter_containers: FxHashMap<SplitterId, (SplitAxis, Rect)>,
    pub(crate) drag_state: Option<DragState>,
    pub(crate) last_click: Option<ClickRecord>,
    pub(crate) hover_pos: Option<(u16, u16)>,
    pub(crate) hover_target: Option<HitTarget>,
    pub(crate) repo_path_display: PathBuf,
    pub(crate) repo_error: Option<String>,
    pub(crate) request_slots: RequestSlotTable,
    pub(crate) file_syntax: RequestSlot<FileDiffKey>,
    pub(crate) stage_syntax: RequestSlot<StageDiffKey>,
    pub(crate) blame_syntax: RequestSlot<BlameKey>,
    pub(crate) range_syntax: RequestSlot<(Oid, String, (u32, u32))>,
    pub(crate) next_cli_id: u64,
    pub(crate) loading_snapshot: bool,
    pub(crate) async_graph_enabled: bool,
    pub(crate) status_notice: Option<String>,
    pub(crate) spinner_frame: usize,
    pub(crate) needs_redraw: bool,
    pub(crate) should_quit: bool,
    /// Footer notification (`[Restore Branch]`, operation errors).
    pub(crate) notification: Option<Notification>,
    /// Hover-to-open timer of cascading submenus.
    pub(crate) menu_hover: Option<MenuHover>,
    /// Number of Git write operations in flight.
    pub(crate) git_ops_running: usize,
    /// Outgoing commit count visible when the user confirmed the last Push dialog.
    pub(crate) last_push_expected_commits: Option<usize>,
    pub(crate) pending_keys: Vec<crossterm::event::KeyEvent>,
    pub(crate) pending_key_at: Option<std::time::Instant>,
    /// Virtual monotonic clock injected by [`crate::session::TuiSession`]
    /// during deterministic runs (`None` falls back to `Instant::now()`).
    pub(crate) clock: Option<Instant>,
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

        let log = LogWorkbench::new(diff_context, compact_refs, graph_mode);
        let stage_workbench = CommitWorkbench::with_diff_context(diff_context);
        let stash = StashWorkbench::with_diff_context(diff_context);

        Self {
            theme,
            glyph_mode,
            branch_colors,
            diff_caches: DiffCacheStore::new(),
            log,
            active_tab,
            focused_pane,
            splitters: SplitterState::with_default_ratios(
                config.jbgit.layout.splitter_defaults(),
            ),
            splitter_containers: FxHashMap::default(),
            terminal_area: Rect::ZERO,
            modal_stack: ModalStack::new(),
            hit_registry: HitTestRegistry::new(),
            drag_state: None,
            last_click: None,
            hover_pos: None,
            hover_target: None,
            repo_path_display,
            repo_error,
            repo_snapshot: None,
            stage_workbench,
            request_slots: RequestSlotTable::new(),
            file_syntax: RequestSlot::new(),
            stage_syntax: RequestSlot::new(),
            blame_syntax: RequestSlot::new(),
            range_syntax: RequestSlot::new(),
            console: ConsoleWorkbench::new(),
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
            last_push_expected_commits: None,
            stash,
            config,
            i18n,
            pending_keys: Vec::new(),
            pending_key_at: None,
            clock: None,
        }
    }

    /// Returns the session's current monotonic instant (virtual clock when driven
    /// through [`crate::session::TuiSession`], real `Instant::now()` otherwise).
    pub(crate) fn now(&self) -> Instant {
        self.clock.unwrap_or_else(Instant::now)
    }

    /// Unified focus/tab transition hook: cleanly blurs any inline editing state
    /// on the pane being left before activating `(tab, pane)`.
    pub(crate) fn set_focus(&mut self, tab: WorkspaceTab, pane: PaneId) {
        let leaving_tab = self.active_tab != tab;
        let leaving_pane = self.focused_pane != pane;
        if leaving_tab || leaving_pane {
            self.blur_active_pane(leaving_tab);
        }
        self.active_tab = tab;
        self.focused_pane = pane;
    }

    /// Blurs inline editors / visual selection on the currently focused pane
    /// (and clears Log zoomed diff when leaving the Log tab).
    pub(crate) fn blur_active_pane(&mut self, leaving_tab: bool) {
        self.log.on_blur(leaving_tab);
        self.stage_workbench.on_blur();
    }

    /// Pushes a modal layer onto `modal_stack` and blurs inline editing state
    /// on the underlying pane (preserving diff visual selection for context
    /// menus / three-way inspection).
    pub(crate) fn push_modal(&mut self, layer: ModalLayer) {
        let preserve_visual = matches!(
            layer,
            ModalLayer::ContextMenu { .. } | ModalLayer::ThreeWayInspector { .. }
        );
        if preserve_visual {
            self.log.on_blur(false);
            self.stage_workbench.blur_text_inputs();
        } else {
            self.blur_active_pane(false);
        }
        self.modal_stack.push(layer);
    }

    pub fn ensure_graph_cache(&mut self) {
        self.log.ensure_graph_cache(
            self.repo_snapshot.as_ref(),
            &self.branch_colors,
            self.async_graph_enabled,
        );
    }

    pub fn log_commit_at(&self, visible_row: usize) -> Option<&CommitSummary> {
        self.log.commit_at(self.repo_snapshot.as_ref(), visible_row)
    }

    pub fn console_output_max_scroll(&self) -> usize {
        self.console.output_max_scroll()
    }

    pub fn console_page_step(&self) -> isize {
        self.console.page_step()
    }

    pub fn console_half_page_step(&self) -> isize {
        self.console.half_page_step()
    }

    /// Scrolls `pane` by signed `delta`, clamped to its bound or fallback upper bound.
    pub fn scroll_pane(&mut self, pane: ScrollPane, delta: isize) {
        match pane {
            ScrollPane::ConsoleOutput => {
                self.console.scroll_output(delta);
            }
            ScrollPane::LogDiff | ScrollPane::LogDetails => {
                self.log.scroll_pane(pane, delta, &self.diff_caches);
            }
            ScrollPane::StashDiff => {
                self.stash.scroll_diff(delta, &self.diff_caches);
            }
            ScrollPane::StageDiff => {
                self.stage_workbench.scroll_diff(delta);
            }
            ScrollPane::FileHistoryDiff => {
                let fallback = self
                    .modal_stack
                    .file_history()
                    .map_or(usize::MAX, |fh| fh.fallback_diff_max_scroll(&self.diff_caches));
                if let Some(fh) = self.modal_stack.file_history_mut() {
                    fh.diff.scroll.scroll_by(delta, fallback);
                }
            }
            ScrollPane::Annotate => {
                if let Some(ann) = self.modal_stack.annotate_mut() {
                    let fallback = ann.line_count().saturating_sub(1);
                    ann.scroll.scroll_by(delta, fallback);
                }
            }
            ScrollPane::AnnotateDiff => {
                let head_oid = self.repo_snapshot.as_ref().and_then(|s| s.head_oid);
                let fallback = self.modal_stack.annotate().map_or(usize::MAX, |ann| {
                    ann.fallback_diff_max_scroll(head_oid, &self.diff_caches)
                });
                if let Some(ann) = self.modal_stack.annotate_mut() {
                    ann.diff.scroll.scroll_by(delta, fallback);
                }
            }
            ScrollPane::PushDiff => {
                let fallback = self
                    .modal_stack
                    .push_dialog()
                    .and_then(|pd| pd.desired_diff_key(&self.diff_caches))
                    .and_then(|key| self.diff_caches.peek_diff(&key))
                    .map_or(usize::MAX, |d| {
                        d.hunks
                            .iter()
                            .map(|h| h.lines.len() + 1)
                            .sum::<usize>()
                            .saturating_sub(1)
                    });
                if let Some(pd) = self.modal_stack.push_dialog_mut() {
                    pd.diff.scroll.scroll_by(delta, fallback);
                }
            }
            ScrollPane::ConflictMerge => {
                if let Some(cm) = self.modal_stack.conflict_merge_mut() {
                    let fallback = cm
                        .doc
                        .as_ref()
                        .map_or(0, |d| d.three_pane_rows().len().saturating_sub(1));
                    cm.scroll.scroll_by(delta, fallback);
                }
            }
        }
    }

    /// Binds rendered viewport geometries directly to each [`ScrollViewport`] so
    /// scroll offsets remain clamped without a separate `ViewportScrollMetrics` cache.
    pub fn bind_rendered_scroll_bounds(
        &mut self,
        bounds: &[(ScrollPane, usize, usize)],
        commit_editor_viewport: Option<(usize, usize)>,
    ) {
        for &(pane, viewport_h, max_scroll) in bounds {
            match pane {
                ScrollPane::ConsoleOutput => {
                    self.console.bind_output_bound(viewport_h, max_scroll);
                }
                ScrollPane::LogDiff => {
                    self.log.details.diff.scroll.bind_bound(viewport_h, max_scroll);
                }
                ScrollPane::LogDetails => {
                    self.log.details.details_scroll.bind_bound(viewport_h, max_scroll);
                }
                ScrollPane::StashDiff => {
                    self.stash.bind_diff_viewport(viewport_h, max_scroll);
                }
                ScrollPane::StageDiff => {
                    self.stage_workbench.bind_diff_viewport(viewport_h, max_scroll);
                }
                ScrollPane::FileHistoryDiff => {
                    if let Some(fh) = self.modal_stack.file_history_mut() {
                        fh.diff.scroll.bind_bound(viewport_h, max_scroll);
                    }
                }
                ScrollPane::Annotate => {
                    if let Some(ann) = self.modal_stack.annotate_mut() {
                        ann.scroll.bind_bound(viewport_h, max_scroll);
                    }
                }
                ScrollPane::AnnotateDiff => {
                    if let Some(ann) = self.modal_stack.annotate_mut() {
                        ann.diff.scroll.bind_bound(viewport_h, max_scroll);
                    }
                }
                ScrollPane::PushDiff => {
                    if let Some(pd) = self.modal_stack.push_dialog_mut() {
                        pd.diff.scroll.bind_bound(viewport_h, max_scroll);
                    }
                }
                ScrollPane::ConflictMerge => {
                    if let Some(cm) = self.modal_stack.conflict_merge_mut() {
                        cm.scroll.bind_bound(viewport_h, max_scroll);
                    }
                }
            }
        }
        if let Some((inner_w, inner_h)) = commit_editor_viewport {
            self.stage_workbench.bind_editor_viewport(inner_w, inner_h);
        }
    }

    pub fn has_active_tasks(&self) -> bool {
        self.loading_snapshot
            || self.log.graph.rebuild_in_progress
            || self.stage_workbench.commit_in_progress()
            || self.git_ops_running > 0
            || self.modal_stack.file_history().is_some_and(|fh| fh.loading)
            || self.modal_stack.annotate().is_some_and(|ann| ann.loading)
            || self.modal_stack.push_dialog().is_some_and(|pd| pd.loading)
            || self.console.has_running()
    }

    /// Next instant the main loop must wake up for (details debounce, menu hover, or key chord timeout).
    pub fn next_deadline(&self) -> Option<Instant> {
        let chord = self
            .pending_key_at
            .map(|t| t + crate::keymap::KEY_CHORD_WINDOW);
        [
            self.log.next_deadline(),
            self.stage_workbench.next_deadline(),
            self.modal_stack
                .file_history()
                .and_then(FileHistoryState::next_deadline),
            self.modal_stack
                .annotate()
                .and_then(AnnotateState::next_deadline),
            self.modal_stack
                .push_dialog()
                .and_then(PushDialogState::next_deadline),
            self.menu_hover.map(|h| h.due),
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
        self.stage_workbench.rows().len()
    }

    pub fn stage_entry_at(&self, idx: usize) -> Option<jbgit_core::StageEntry> {
        self.stage_workbench.entry_at(idx)
    }

    pub(crate) fn effective_focus_ring(&self) -> Vec<PaneId> {
        self.active_tab
            .focus_ring()
            .iter()
            .copied()
            .filter(|p| {
                *p != PaneId::LogBranchesTree
                    || !self.log.branches.is_collapsed(self.terminal_area.width)
            })
            .collect()
    }

    pub fn flush_pending_at(&mut self, now: Instant, effects: &mut Vec<Effect>) {
        if self
            .pending_key_at
            .is_some_and(|t| now.saturating_duration_since(t) >= crate::keymap::KEY_CHORD_WINDOW)
        {
            self.pending_keys.clear();
            self.pending_key_at = None;
        }
        let now_hover = now;
        let screen = self.terminal_area;
        let (hover_redraw, hover_action) =
            self.modal_stack
                .take_due_menu_hover_action(now_hover, screen, &mut self.menu_hover);
        if hover_redraw {
            self.needs_redraw = true;
        }
        if let Some(action) = hover_action {
            effects.extend(self.dispatch(action));
        }
        let mut log_ctx = crate::log_view::LogDispatchCtx {
            snap: self.repo_snapshot.as_ref(),
            branch_colors: &self.branch_colors,
            diff_caches: &self.diff_caches,
            i18n: &self.i18n,
            now,
            width: self.terminal_area.width,
            async_graph_enabled: self.async_graph_enabled,
            focused_pane: &mut self.focused_pane,
            status_notice: &mut self.status_notice,
            effects,
        };
        self.log.flush_pending(&mut log_ctx);
        let syntax_enabled = self.config.jbgit.diff.syntax_highlight;
        self.stage_workbench
            .flush_pending(now, &mut self.stage_syntax, syntax_enabled, effects);
        if let Some(fh) = self.modal_stack.file_history_mut()
            && fh
                .pending_g
                .is_some_and(|t| now.saturating_duration_since(t) >= FILE_HISTORY_G_WINDOW)
        {
            fh.pending_g = None;
            self.jump_file_history_to_log(effects);
        }
        if let Some(fh) = self.modal_stack.file_history_mut()
            && let Some((key, at)) = fh.diff.pending.take()
        {
            if at <= now {
                if !self.diff_caches.diffs.contains(&key)
                    && fh.diff.requested.as_ref() != Some(&key)
                {
                    fh.diff.requested = Some(key.clone());
                    effects.push(Effect::RequestFileDiff {
                        slot: RequestSlotId::FileHistoryDiff,
                        key,
                    });
                }
            } else {
                fh.diff.pending = Some((key, at));
            }
        }
        if let Some(ann) = self.modal_stack.annotate_mut()
            && ann
                .pending_g
                .is_some_and(|t| now.saturating_duration_since(t) >= FILE_HISTORY_G_WINDOW)
        {
            ann.pending_g = None;
        }
        if let Some(ann) = self.modal_stack.annotate_mut()
            && let Some((key, at)) = ann.diff.pending.take()
        {
            if at <= now {
                if !self.diff_caches.diffs.contains(&key)
                    && ann.diff.requested.as_ref() != Some(&key)
                {
                    ann.diff.requested = Some(key.clone());
                    effects.push(Effect::RequestFileDiff {
                        slot: RequestSlotId::AnnotateDiff,
                        key,
                    });
                }
            } else {
                ann.diff.pending = Some((key, at));
            }
        }
        if let Some(pd) = self.modal_stack.push_dialog_mut()
            && let Some((key, at)) = pd.details_pending.take()
        {
            if at <= now {
                if !self.diff_caches.details.contains(&key)
                    && pd.details_requested.as_ref() != Some(&key)
                {
                    pd.details_requested = Some(key.clone());
                    effects.push(Effect::RequestCommitDetails {
                        slot: RequestSlotId::PushCommitDetails,
                        key,
                    });
                }
            } else {
                pd.details_pending = Some((key, at));
            }
        }
        if let Some(pd) = self.modal_stack.push_dialog_mut()
            && let Some((key, at)) = pd.diff.pending.take()
        {
            if at <= now {
                if !self.diff_caches.diffs.contains(&key)
                    && pd.diff.requested.as_ref() != Some(&key)
                {
                    pd.diff.requested = Some(key.clone());
                    effects.push(Effect::RequestFileDiff {
                        slot: RequestSlotId::PushFileDiff,
                        key,
                    });
                }
            } else {
                pd.diff.pending = Some((key, at));
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
        if let Some((epoch, cancel)) = self.file_syntax.begin_cancelable(diff.key.clone()) {
            effects.push(Effect::RequestSyntaxHighlight(SyntaxJob {
                key: SyntaxJobKey::File(diff.key.clone()),
                epoch,
                target: SyntaxTarget::File(diff),
                cancel,
            }));
        }
    }

    pub(crate) fn schedule_blame_syntax(
        &mut self,
        bundle: Arc<BlameBundle>,
        effects: &mut Vec<Effect>,
    ) {
        if !self.config.jbgit.diff.syntax_highlight
            || bundle.lines.is_empty()
            || bundle.content.len() > jbgit_core::MAX_DIFF_FILE_BYTES
        {
            return;
        }
        if let Some((epoch, cancel)) = self.blame_syntax.begin_cancelable(bundle.key.clone()) {
            effects.push(Effect::RequestSyntaxHighlight(SyntaxJob {
                key: SyntaxJobKey::Blame(bundle.key.clone()),
                epoch,
                target: SyntaxTarget::Blame(bundle),
                cancel,
            }));
        }
    }

    pub(crate) fn schedule_range_syntax(
        &mut self,
        oid: Oid,
        range: (u32, u32),
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
        let slot_key = (oid, diff.key.path.clone(), range);
        if let Some((epoch, cancel)) = self.range_syntax.begin_cancelable(slot_key) {
            effects.push(Effect::RequestSyntaxHighlight(SyntaxJob {
                key: SyntaxJobKey::RangePatch {
                    oid,
                    path: diff.key.path.clone(),
                    range,
                },
                epoch,
                target: SyntaxTarget::File(diff),
                cancel,
            }));
        }
    }

    pub(crate) fn reconcile_stash_syntax(&mut self, effects: &mut Vec<Effect>) {
        if self.active_tab != WorkspaceTab::Stash {
            return;
        }
        if let Some(diff) = self.stash.pending_syntax_diff(&self.diff_caches) {
            self.schedule_file_syntax(diff, effects);
        }
    }
}
