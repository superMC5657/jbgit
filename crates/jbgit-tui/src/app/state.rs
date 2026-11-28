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
    pub(crate) tree_syntax: RequestSlot<jbgit_core::TreeBlobKey>,
    pub(crate) next_cli_id: u64,
    pub(crate) loading_snapshot: bool,
    pub(crate) loading_more_commits: bool,
    pub(crate) commit_page_size: usize,
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
    /// Number of in-flight Git operations that may change `HEAD`.
    pub(crate) head_switch_ops_running: usize,
    /// Waiting for `RepoSnapshotLoaded` after a `HEAD`-changing op succeeded.
    pub(crate) awaiting_head_snapshot: bool,
    /// Global `P` (`PromptPushCurrentBranch`) pressed while switching `HEAD`.
    pub(crate) pending_push_after_head_switch: bool,
    /// Outgoing commit count visible when the user confirmed the last Push dialog.
    pub(crate) last_push_expected_commits: Option<usize>,
    /// Commit queued while configuring missing `user.name` / `user.email`.
    pub(crate) pending_commit_after_identity: Option<jbgit_engine::CommitRequest>,
    /// Last output directory used by `Create Patch…` (defaults to `<repo>/..`).
    pub(crate) last_patch_dir: Option<PathBuf>,
    /// Update Project stopped on conflicts with local changes still stashed:
    /// pop them once the ongoing operation is continued / aborted.
    pub(crate) pending_update_stash: bool,
    /// Set once the ongoing operation finishes so the next clean
    /// `RepoSnapshotLoaded` restores `pending_update_stash`.
    pub(crate) pending_update_stash_restore: bool,
    /// Askpass prompts waiting for the credential dialog (FIFO).
    pub(crate) credentials: crate::app::CredentialQueue,
    /// Remote to select in the Remotes modal once the snapshot lists it.
    pub(crate) pending_remote_selection: Option<String>,
    /// Repository session generation (Spec 12 Issue 09): bumped on every
    /// rebind; engine / async results stamped with an older generation are
    /// dropped by [`crate::session::TuiSession`].
    pub(crate) session_generation: u64,
    /// Monotonic ID generator for async lifecycle tasks (`init`/`clone`/`worktree_setup`).
    pub(crate) next_async_task_id: u64,
    /// Currently active `git init` task ID (if any).
    pub(crate) active_init_task_id: Option<u64>,
    /// Currently active `git clone` task ID (if any).
    pub(crate) active_clone_task_id: Option<u64>,
    /// Currently active `[worktree] setup_command` task IDs.
    pub(crate) active_worktree_setup_ids: rustc_hash::FxHashSet<u64>,
    /// Confirmed repository switch the host must perform (unbind → bind).
    pub(crate) pending_repo_switch: Option<PathBuf>,
    /// Confirmed `git init` directory request the host must execute and bind.
    pub(crate) pending_init_repo: Option<PathBuf>,
    /// Confirmed `git clone` request the host must execute and bind on success.
    pub(crate) pending_clone_repo: Option<jbgit_engine::CloneRequest>,
    /// Set when the user cancels an active `git clone` task.
    pub(crate) pending_clone_cancel: bool,
    /// `(branch_name, tip_oid)` list from an in-flight Branch Cleanup batch delete
    /// so partial failures still offer Undo/Restore for already-deleted branches.
    pub(crate) pending_batch_branch_deletes: Vec<(String, jbgit_core::Oid)>,
    /// Confirmed `[worktree] setup_command` the host should run in the new worktree.
    pub(crate) pending_worktree_setup: Option<(PathBuf, String)>,
    /// Task ID paired with `pending_worktree_setup`.
    pub(crate) pending_worktree_setup_task_id: Option<u64>,
    /// User-level most-recently-opened repositories.
    pub(crate) recent_repos: crate::repo_switcher::RecentRepos,
    /// True when `recent_repos` was already recorded off-thread during bind.
    pub(crate) recent_repos_pre_recorded: bool,
    /// Cached remembered options for Merge/Pull/Rebase dialogs so opening them
    /// never blocks the UI event loop with disk reads.
    pub(crate) remembered_merge_options: Option<jbgit_core::MergeOptions>,
    pub(crate) remembered_pull_options: Option<jbgit_core::PullOptions>,
    pub(crate) remembered_rebase_options: Option<jbgit_core::RebaseOptions>,
    /// Detected `git --version` (`None` = unknown; worktree gate assumes support).
    pub(crate) git_version: Option<jbgit_core::GitVersion>,
    /// Resolved `git` executable path & version banner (Spec 13 Issue 13).
    pub(crate) git_info: Option<jbgit_engine::GitExecutableInfo>,
    /// Error message when `[git] executable` is invalid or `git` cannot be found.
    pub(crate) git_info_error: Option<String>,
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
        let diff_whitespace = config.jbgit.diff.whitespace.to_core();
        let diff_soft_wrap = config.jbgit.diff.soft_wrap;

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

        let log = LogWorkbench::new(
            diff_context,
            diff_whitespace,
            diff_soft_wrap,
            compact_refs,
            &config.jbgit.log,
            graph_mode,
        );
        let stage_workbench =
            CommitWorkbench::with_diff_prefs(diff_context, diff_whitespace, diff_soft_wrap);
        let stash = StashWorkbench::with_diff_prefs(diff_context, diff_whitespace, diff_soft_wrap);

        Self {
            theme,
            glyph_mode,
            branch_colors,
            diff_caches: DiffCacheStore::new(),
            log,
            active_tab,
            focused_pane,
            splitters: SplitterState::with_default_ratios(config.jbgit.layout.splitter_defaults()),
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
            tree_syntax: RequestSlot::new(),
            console: ConsoleWorkbench::new(),
            next_cli_id: 1,
            loading_snapshot: false,
            loading_more_commits: false,
            commit_page_size: jbgit_engine::DEFAULT_COMMIT_PAGE_SIZE,
            async_graph_enabled: false,
            status_notice: None,
            spinner_frame: 0,
            needs_redraw: true,
            should_quit: false,
            notification: None,
            menu_hover: None,
            git_ops_running: 0,
            head_switch_ops_running: 0,
            awaiting_head_snapshot: false,
            pending_push_after_head_switch: false,
            last_push_expected_commits: None,
            pending_commit_after_identity: None,
            last_patch_dir: None,
            pending_update_stash: false,
            pending_update_stash_restore: false,
            credentials: crate::app::CredentialQueue::default(),
            pending_remote_selection: None,
            session_generation: 0,
            next_async_task_id: 1,
            active_init_task_id: None,
            active_clone_task_id: None,
            active_worktree_setup_ids: rustc_hash::FxHashSet::default(),
            pending_repo_switch: None,
            pending_init_repo: None,
            pending_clone_repo: None,
            pending_clone_cancel: false,
            pending_batch_branch_deletes: Vec::new(),
            pending_worktree_setup: None,
            pending_worktree_setup_task_id: None,
            recent_repos: crate::repo_switcher::RecentRepos::in_memory(),
            recent_repos_pre_recorded: false,
            remembered_merge_options: None,
            remembered_pull_options: None,
            remembered_rebase_options: None,
            git_version: None,
            git_info: None,
            git_info_error: None,
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

    /// Formats a compact one-line summary of the resolved `git` executable
    /// (or any configured `[git] executable` error) for Console startup and
    /// Help/About overlays.
    pub fn git_executable_summary(&self) -> Option<(String, bool)> {
        if let Some(err) = &self.git_info_error {
            return Some((
                self.i18n
                    .t_fmt("git.executable_error", &[("err", err.as_str())]),
                true,
            ));
        }
        let info = self.git_info.as_ref()?;
        let path = info.path.display().to_string();
        let ver = if info.version_string.trim().is_empty() {
            "git"
        } else {
            info.version_string.trim()
        };
        Some((
            self.i18n.t_fmt(
                "git.executable_summary",
                &[("version", ver), ("path", &path)],
            ),
            false,
        ))
    }

    /// Unified focus/tab transition hook: cleanly blurs any inline editing state
    /// on the pane being left before activating `(tab, pane)`.
    pub(crate) fn set_focus(&mut self, tab: WorkspaceTab, pane: PaneId) {
        let leaving_tab = self.active_tab != tab;
        let leaving_pane = self.focused_pane != pane;
        if leaving_tab || leaving_pane {
            self.blur_active_pane(leaving_tab);
        }
        if leaving_tab {
            self.hover_target = None;
            self.hover_pos = None;
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
        self.menu_hover = None;
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

    pub(crate) fn allocate_async_task_id(&mut self) -> u64 {
        let id = self.next_async_task_id;
        self.next_async_task_id = self.next_async_task_id.wrapping_add(1).max(1);
        id
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
            // Fallbacks below are lazy: once a frame has bound `max_scroll`, the
            // whole-content measurement is skipped. `ScrollViewport` is `Copy`, so
            // it is copied out where the fallback borrows its owner.
            ScrollPane::FileHistoryDiff => {
                let caches = &self.diff_caches;
                if let Some(fh) = self.modal_stack.file_history_mut() {
                    let mut scroll = fh.diff.scroll;
                    scroll.scroll_by_with(delta, || fh.fallback_diff_max_scroll(caches));
                    fh.diff.scroll = scroll;
                }
            }
            ScrollPane::FileHistoryBlob => {
                if let Some(fh) = self.modal_stack.file_history_mut() {
                    let mut scroll = fh.repo_version_scroll;
                    scroll.scroll_by_with(delta, || fh.repo_version_line_count().saturating_sub(1));
                    fh.repo_version_scroll = scroll;
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
                let caches = &self.diff_caches;
                if let Some(ann) = self.modal_stack.annotate_mut() {
                    let mut scroll = ann.diff.scroll;
                    scroll.scroll_by_with(delta, || ann.fallback_diff_max_scroll(head_oid, caches));
                    ann.diff.scroll = scroll;
                }
            }
            ScrollPane::PushDiff => {
                let caches = &self.diff_caches;
                if let Some(pd) = self.modal_stack.push_dialog_mut() {
                    let mut scroll = pd.diff.scroll;
                    scroll.scroll_by_with(delta, || {
                        pd.desired_diff_key(caches)
                            .and_then(|key| caches.peek_diff(&key))
                            .map_or(usize::MAX, |d| {
                                d.hunks
                                    .iter()
                                    .map(|h| h.lines.len() + 1)
                                    .sum::<usize>()
                                    .saturating_sub(1)
                            })
                    });
                    pd.diff.scroll = scroll;
                }
            }
            ScrollPane::ConflictMerge => {
                if let Some(cm) = self.modal_stack.conflict_merge_mut() {
                    cm.scroll.scroll_by_with(delta, || {
                        cm.doc
                            .as_ref()
                            .map_or(0, |d| d.total_pane_rows().saturating_sub(1))
                    });
                }
            }
            ScrollPane::ConflictQueue => {
                if let Some(cq) = self.modal_stack.conflict_queue_mut() {
                    let fallback = cq.entries.len().saturating_sub(1);
                    cq.scroll.scroll_by(delta, fallback);
                }
            }
            ScrollPane::TreeBrowserPreview => {
                if let Some(tb) = self.modal_stack.tree_browser_mut() {
                    let mut scroll = tb.preview_scroll;
                    scroll.scroll_by_with(delta, || tb.preview_line_count().saturating_sub(1));
                    tb.preview_scroll = scroll;
                }
            }
            ScrollPane::ApplyPatch => {
                if let Some(ap) = self.modal_stack.apply_patch_mut() {
                    let fallback = ap.known_files.len().saturating_sub(1);
                    ap.scroll.scroll_by(delta, fallback);
                }
            }
            ScrollPane::RebaseDiff => {
                if let Some(ModalLayer::Dialog(d)) = self.modal_stack.last_mut()
                    && let crate::dialogs::Dialog::RebaseTodo(r) = d.as_mut()
                    && r.show_diff
                {
                    r.diff_scroll.scroll_by(delta, usize::MAX);
                }
            }
            ScrollPane::CommitMessageModal => {
                if let Some(scroll) = self.modal_stack.commit_message_scroll_mut() {
                    scroll.scroll_by(delta, usize::MAX);
                }
            }
            ScrollPane::ReflogDiff => {
                let caches = &self.diff_caches;
                if let Some(rf) = self.modal_stack.reflog_mut() {
                    let mut scroll = rf.diff.scroll;
                    scroll.scroll_by_with(delta, || {
                        rf.desired_diff_key(caches)
                            .and_then(|k| caches.peek_diff(&k))
                            .map_or(usize::MAX, |d| {
                                d.hunks
                                    .iter()
                                    .map(|h| h.lines.len() + 1)
                                    .sum::<usize>()
                                    .saturating_sub(1)
                            })
                    });
                    rf.diff.scroll = scroll;
                }
            }
            ScrollPane::ApplyPatchPreview => {
                if let Some(ap) = self.modal_stack.apply_patch_mut() {
                    let mut scroll = ap.preview_scroll;
                    scroll.scroll_by_with(delta, || ap.preview_line_count().saturating_sub(1));
                    ap.preview_scroll = scroll;
                }
            }
            ScrollPane::BranchCleanup => {
                if let Some(bc) = self.modal_stack.branch_cleanup_mut() {
                    let fallback = bc.rows.len().saturating_sub(1);
                    bc.scroll.scroll_by(delta, fallback);
                }
            }
            ScrollPane::TreeBrowserList
            | ScrollPane::TreeBrowserSearch
            | ScrollPane::PushCommits
            | ScrollPane::PushFiles
            | ScrollPane::Worktrees
            | ScrollPane::RepoSwitcher
            | ScrollPane::Remotes => {}
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
                    self.log
                        .details
                        .diff
                        .scroll
                        .bind_bound(viewport_h, max_scroll);
                }
                ScrollPane::LogDetails => {
                    self.log
                        .details
                        .details_scroll
                        .bind_bound(viewport_h, max_scroll);
                }
                ScrollPane::StashDiff => {
                    self.stash.bind_diff_viewport(viewport_h, max_scroll);
                }
                ScrollPane::StageDiff => {
                    self.stage_workbench
                        .bind_diff_viewport(viewport_h, max_scroll);
                }
                ScrollPane::FileHistoryDiff => {
                    if let Some(fh) = self.modal_stack.file_history_mut() {
                        fh.diff.scroll.bind_bound(viewport_h, max_scroll);
                    }
                }
                ScrollPane::CommitMessageModal => {
                    if let Some(scroll) = self.modal_stack.commit_message_scroll_mut() {
                        scroll.bind_bound(viewport_h, max_scroll);
                    }
                }
                ScrollPane::ReflogDiff => {
                    if let Some(rf) = self.modal_stack.reflog_mut() {
                        rf.diff.scroll.bind_bound(viewport_h, max_scroll);
                    }
                }
                ScrollPane::FileHistoryBlob => {
                    if let Some(fh) = self.modal_stack.file_history_mut() {
                        fh.repo_version_scroll.bind_bound(viewport_h, max_scroll);
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
                ScrollPane::PushCommits => {
                    if let Some(pd) = self.modal_stack.push_dialog_mut() {
                        let total = pd.visible_commits().len();
                        pd.commit_scroll
                            .ensure_visible(pd.selected_commit, total, viewport_h);
                    }
                }
                ScrollPane::PushFiles => {
                    let total = self
                        .modal_stack
                        .push_dialog()
                        .map_or(0, |pd| pd.selected_file_changes(&self.diff_caches).len());
                    if let Some(pd) = self.modal_stack.push_dialog_mut() {
                        pd.file_scroll
                            .ensure_visible(pd.selected_file, total, viewport_h);
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
                ScrollPane::ConflictQueue => {
                    if let Some(cq) = self.modal_stack.conflict_queue_mut() {
                        let total = cq.entries.len();
                        cq.scroll.ensure_visible(cq.selected, total, viewport_h);
                    }
                }
                ScrollPane::TreeBrowserList => {
                    if let Some(tb) = self.modal_stack.tree_browser_mut() {
                        let total = tb.rows.len();
                        tb.scroll.ensure_visible(tb.selected, total, viewport_h);
                    }
                }
                ScrollPane::TreeBrowserPreview => {
                    if let Some(tb) = self.modal_stack.tree_browser_mut() {
                        tb.preview_scroll.bind_bound(viewport_h, max_scroll);
                    }
                }
                ScrollPane::TreeBrowserSearch => {
                    if let Some(tb) = self.modal_stack.tree_browser_mut() {
                        let total = tb.search_matches.len();
                        tb.search_scroll
                            .ensure_visible(tb.search_selected, total, viewport_h);
                    }
                }
                ScrollPane::ApplyPatch => {
                    if let Some(ap) = self.modal_stack.apply_patch_mut() {
                        ap.scroll.bind_bound(viewport_h, max_scroll);
                    }
                }
                ScrollPane::ApplyPatchPreview => {
                    if let Some(ap) = self.modal_stack.apply_patch_mut() {
                        ap.preview_scroll.bind_bound(viewport_h, max_scroll);
                    }
                }
                ScrollPane::Worktrees => {
                    let total = self
                        .repo_snapshot
                        .as_deref()
                        .map_or(0, |s| s.worktrees.len());
                    if let Some(ws) = self.modal_stack.worktrees_mut() {
                        ws.scroll.ensure_visible(ws.selected, total, viewport_h);
                    }
                }
                ScrollPane::RepoSwitcher => {
                    if let Some(rs) = self.modal_stack.repo_switcher_mut() {
                        let vis = rs.visible();
                        let (sel_display, total) = rs.display_selection_and_len(&vis);
                        rs.scroll.ensure_visible(sel_display, total, viewport_h);
                    }
                }
                ScrollPane::BranchCleanup => {
                    if let Some(bc) = self.modal_stack.branch_cleanup_mut() {
                        bc.scroll.bind_bound(viewport_h, max_scroll);
                    }
                }
                ScrollPane::Remotes => {
                    let total = self.repo_snapshot.as_deref().map_or(0, |s| s.remotes.len());
                    if let Some(rs) = self.modal_stack.remotes_mut() {
                        rs.scroll.ensure_visible(rs.selected, total, viewport_h);
                    }
                }
                ScrollPane::RebaseDiff => {
                    for layer in self.modal_stack.iter_mut().rev() {
                        if let ModalLayer::Dialog(d) = layer
                            && let crate::dialogs::Dialog::RebaseTodo(r) = d.as_mut()
                        {
                            r.diff_scroll.bind_bound(viewport_h, max_scroll);
                            break;
                        }
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
            || self.loading_more_commits
            || self.log.graph.rebuild_in_progress
            || self.stage_workbench.commit_in_progress()
            || self.stage_workbench.loading_stage_status()
            || self.git_ops_running > 0
            || self.head_switch_ops_running > 0
            || self.awaiting_head_snapshot
            || self.pending_push_after_head_switch
            || self.modal_stack.file_history().is_some_and(|fh| fh.loading)
            || self.modal_stack.annotate().is_some_and(|ann| ann.loading)
            || self.modal_stack.push_dialog().is_some_and(|pd| pd.loading)
            || self.modal_stack.reflog().is_some_and(|rf| rf.loading)
            || self.console.has_running()
            || !self.credentials.is_empty()
            || self.modal_stack.iter().any(|l| match l {
                ModalLayer::Welcome(ws) => ws.initializing,
                ModalLayer::CloneRepo(cd) => cd.cloning,
                _ => false,
            })
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
            self.modal_stack
                .reflog()
                .and_then(crate::log_view::ReflogState::next_deadline),
            self.modal_stack
                .apply_patch()
                .and_then(crate::patch_view::ApplyPatchState::next_deadline),
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
            && let Some(key) = fh.diff.flush_due(&self.diff_caches, now)
        {
            effects.push(Effect::RequestFileDiff {
                slot: RequestSlotId::FileHistoryDiff,
                key,
            });
        }
        if let Some(ann) = self.modal_stack.annotate_mut()
            && ann
                .pending_g
                .is_some_and(|t| now.saturating_duration_since(t) >= FILE_HISTORY_G_WINDOW)
        {
            ann.pending_g = None;
        }
        if let Some(ann) = self.modal_stack.annotate_mut()
            && let Some(key) = ann.diff.flush_due(&self.diff_caches, now)
        {
            effects.push(Effect::RequestFileDiff {
                slot: RequestSlotId::AnnotateDiff,
                key,
            });
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
        if let Some(ap) = self.modal_stack.apply_patch_mut()
            && let Some(key) = ap.flush_due(now)
        {
            effects.push(Effect::RequestPatchCheck(key));
        }
        if let Some(pd) = self.modal_stack.push_dialog_mut()
            && let Some(key) = pd.diff.flush_due(&self.diff_caches, now)
        {
            effects.push(Effect::RequestFileDiff {
                slot: RequestSlotId::PushFileDiff,
                key,
            });
        }
        if let Some(rf) = self.modal_stack.reflog_mut()
            && let Some(key) = rf.diff.flush_due(&self.diff_caches, now)
        {
            effects.push(Effect::RequestFileDiff {
                slot: RequestSlotId::ReflogFileDiff,
                key,
            });
        }
    }

    pub(crate) fn schedule_file_syntax(&mut self, diff: Arc<FileDiff>, effects: &mut Vec<Effect>) {
        if !self.config.jbgit.diff.syntax_highlight
            || diff.is_binary
            || diff.is_too_large
            || diff.lfs.is_some()
            || diff.hunks.is_empty()
        {
            return;
        }
        if let Some((epoch, cancel)) = self.file_syntax.begin_cancelable(diff.key.clone()) {
            effects.push(Effect::RequestSyntaxHighlight(SyntaxJob {
                epoch,
                target: SyntaxTarget::File {
                    key: SyntaxJobKey::File(diff.key.clone()),
                    diff,
                },
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
            || diff.lfs.is_some()
            || diff.hunks.is_empty()
        {
            return;
        }
        let slot_key = (oid, diff.key.path.clone(), range);
        if let Some((epoch, cancel)) = self.range_syntax.begin_cancelable(slot_key) {
            let path = diff.key.path.clone();
            effects.push(Effect::RequestSyntaxHighlight(SyntaxJob {
                epoch,
                target: SyntaxTarget::File {
                    key: SyntaxJobKey::RangePatch { oid, path, range },
                    diff,
                },
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

    /// True when the workspace is not attached to a repository (startup error or
    /// onboarding modal without a loaded repository), so background panes should
    /// not render misleading "Loading…" placeholders.
    pub fn is_unattached_workspace(&self) -> bool {
        self.repo_error.is_some()
            || self.modal_stack.iter().any(|l| {
                matches!(l, ModalLayer::Welcome(_))
                    || (self.repo_snapshot.is_none() && matches!(l, ModalLayer::CloneRepo(_)))
            })
    }
}
