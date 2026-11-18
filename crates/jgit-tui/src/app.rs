use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use jgit_core::graph::SortType;
use jgit_core::{
    BranchFilter, BranchNodeKind, ChangeRowKind, CommitSummary, DetailsKey, FileDiffKey,
    GitConsoleEntry, ModalButton, Oid, PaneId, RefKind, RefreshScope, RepoSnapshot, SplitAxis,
    SplitterId, StageStatusSnapshot, StageTarget, StageTreeRowKind, WorkspaceTab,
};
use jgit_engine::{
    CommitRequest, EngineEvent, EngineHandle, StageDiffKey, TaskRequest,
};
use ratatui::layout::Rect;
use rustc_hash::FxHashMap;

use crate::graph::LogGraphState;
use crate::graph::state::FilterInputs;
use crate::hit_test::{DiffHunkActionKind, HitTarget, HitTestRegistry};
use crate::log_view::{
    BranchesPaneState, DETAILS_DEBOUNCE, DetailsPaneState, FilterBarState, FilterField,
    LogPresentation, PathMatches, PathRequest, PresentationToggle,
};
use crate::stage_view::StageWorkbenchState;
use crate::syntax::HighlightedDiff;
use crate::theme::{BranchColorManager, GlyphMode, ThemePalette};
use crate::widgets::SplitterState;

pub struct RenderContext<'a> {
    pub theme: &'a ThemePalette,
    pub glyph_mode: GlyphMode,
    pub branch_colors: &'a BranchColorManager,
    pub hit_registry: &'a mut HitTestRegistry,
    pub focused_pane: PaneId,
    pub hover_target: Option<&'a HitTarget>,
    pub dragging_splitter: Option<SplitterId>,
}

#[derive(Debug, Clone)]
pub enum ContextMenuAction {
    SwitchTab(WorkspaceTab),
    RefreshRepo,
    RunReadOnlyProbe(Vec<String>),
    ToggleGlyphMode,
    OpenHelp,
    OpenPalettePreview,
    TogglePresentation(PresentationToggle),
    BranchesApplyFilter,
    BranchesNavigate,
    BranchesToggleFavorite,
    BranchesToggleGroupByDir,
    BranchesToggleShowTags,
    BranchesToggleNavigateOnSelect,
    OpenGoTo,
    StageSelected,
    UnstageSelected,
    StageAllTracked,
    DiscardSelected,
    StageToggleGroupByDir,
    StageToggleShowIgnored,
    StageExpandAll(bool),
    OpenExternalEditorFile(String),
}

#[derive(Debug, Clone)]
pub enum ContextMenuEntryKind {
    Action(ContextMenuAction),
    Submenu(Vec<ContextMenuItem>),
}

#[derive(Debug, Clone)]
pub struct ContextMenuItem {
    pub label: String,
    pub mnemonic: Option<char>,
    pub shortcut: Option<String>,
    pub kind: ContextMenuEntryKind,
}

impl ContextMenuItem {
    pub fn action(
        label: impl Into<String>,
        mnemonic: Option<char>,
        shortcut: Option<&str>,
        action: ContextMenuAction,
    ) -> Self {
        Self {
            label: label.into(),
            mnemonic,
            shortcut: shortcut.map(ToString::to_string),
            kind: ContextMenuEntryKind::Action(action),
        }
    }

    pub fn submenu(
        label: impl Into<String>,
        mnemonic: Option<char>,
        items: Vec<ContextMenuItem>,
    ) -> Self {
        Self {
            label: label.into(),
            mnemonic,
            shortcut: None,
            kind: ContextMenuEntryKind::Submenu(items),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PopupKind {
    ShortcutsHelp,
    BranchesQuickView,
    /// Debug view of the Darcula palette, 14 track colors and glyph widths.
    PalettePreview,
    /// Inline editor for one filter-bar dimension.
    FilterEditor(FilterField),
    /// `Ctrl+G` Go to Hash / Branch / Tag / `HEAD~N`.
    GoToRevision,
    /// Parent chooser for merge commits with ≥ 2 parents.
    ChooseParent,
    /// Commit message history picker (`Ctrl+M` / `[🕘]`).
    CommitHistory,
    /// Three-way comparison card (`HEAD / Staged / Worktree`) for mixed `StagedRange`.
    ThreeWayInspector,
    /// Hook or commit failure output dialog.
    HookError,
}

#[derive(Debug, Clone)]
pub enum ModalLayer {
    /// Layer 2: Floating Popup (Branches Speed-Search, Shortcuts Help).
    FloatingPopup {
        kind: PopupKind,
        query: String,
        selected: usize,
    },
    /// Layer 3: Right-click Context Menu (supports cascading submenus via `depth`).
    ContextMenu {
        anchor: (u16, u16),
        items: Vec<ContextMenuItem>,
        selected: usize,
        depth: u8,
    },
    /// Layer 4: Critical Modal Dialog.
    CriticalDialog {
        title: String,
        body: String,
        confirm_label: String,
        cancel_label: String,
        selected_button: ModalButton,
        confirm_effect: Option<Effect>,
    },
}

impl ModalLayer {
    pub const fn z_level(&self) -> u8 {
        match self {
            Self::FloatingPopup { .. } => 20,
            Self::ContextMenu { depth, .. } => 30u8.saturating_add(depth.saturating_mul(2)),
            Self::CriticalDialog { .. } => 40,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum DragState {
    Splitter {
        splitter_id: SplitterId,
        axis: SplitAxis,
        container: Rect,
    },
    LogSelection,
}

#[derive(Debug, Clone, Copy)]
pub struct ClickRecord {
    pub at: Instant,
    pub col: u16,
    pub row: u16,
}

impl ClickRecord {
    pub fn is_double_click(&self, col: u16, row: u16) -> bool {
        self.at.elapsed() <= Duration::from_millis(300)
            && self.col.abs_diff(col) <= 1
            && self.row.abs_diff(row) <= 1
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditorMoveKind {
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
}

#[derive(Debug, Clone)]
pub enum Effect {
    RefreshRepoAndStage,
    RefreshScope(RefreshScope),
    RequestCommitDetails(DetailsKey),
    RequestFileDiff(FileDiffKey),
    RequestPathMatches(Vec<String>, Arc<RepoSnapshot>),
    RequestStageFileDiff(StageDiffKey),
    RequestAmendPreview,
    RequestCommitPrecheck,
    ApplyStageBuffer {
        path: String,
        new_staged_bytes: Vec<u8>,
        file_mode: u32,
        remove_from_index: bool,
    },
    WriteWorktreeBuffer {
        path: String,
        new_local_bytes: Vec<u8>,
    },
    StageTargets(Vec<StageTarget>),
    StageAllTracked,
    UnstageTargets(Vec<StageTarget>),
    DiscardTargets(Vec<StageTarget>),
    ExecuteCommit(CommitRequest),
    UndoLastCommit,
    OpenExternalEditorMessage,
    OpenExternalEditorFile(String),
    CopyToClipboard(String),
    RunCli {
        args: Vec<String>,
        stdin_payload: Option<Vec<u8>>,
        is_read_only: bool,
        refresh_after: RefreshScope,
    },
    Quit,
}

#[derive(Debug, Clone)]
pub enum Action {
    Quit,
    SwitchTab(WorkspaceTab),
    CycleFocus {
        reverse: bool,
    },
    FocusPane(PaneId),
    ToggleGlyphMode,
    ToggleCompactRefs,
    ToggleTagNames,
    RefreshAll,
    OpenPopup {
        kind: PopupKind,
        initial_query: String,
    },
    OpenContextMenu {
        anchor: (u16, u16),
        items: Vec<ContextMenuItem>,
        depth: u8,
    },
    PopModal,
    PopContextMenus,
    ConfirmCriticalDialog,
    ResetSplitter(SplitterId),

    // ── Branches Dashboard (left pane) ──
    ToggleBranchesPane,
    SelectBranchRow(usize),
    ScrollBranches(isize),
    BranchesExpand(bool),
    BranchesToggleRow(usize),
    BranchesToggleMulti(usize),
    BranchesApplyFilter,
    BranchesNavigate,
    BranchesToggleFavorite,
    BranchesSearchInput(char),
    BranchesSearchBackspace,
    BranchesEscape,
    BranchesToggleGroupByDir,
    BranchesToggleShowTags,
    BranchesToggleNavigateOnSelect,

    // ── Filter bar ──
    FocusFilterText,
    FilterTextInput(char),
    FilterTextBackspace,
    FilterTextDone,
    FilterTextClear,
    ToggleFilterRegex,
    ToggleFilterCase,
    ApplyFilterField {
        field: FilterField,
        expr: String,
    },
    ClearFilterField(FilterField),
    #[cfg_attr(not(test), allow(dead_code))]
    ClearAllFilters,
    TogglePresentation(PresentationToggle),

    // ── Commit table (center pane) ──
    SelectLogRow(usize),
    ExtendLogSelectionTo(usize),
    ScrollLog(isize),
    ExtendLogSelection(isize),
    LogPage {
        half: bool,
        down: bool,
    },
    LogEdge {
        bottom: bool,
    },
    JumpToLogRow {
        row: usize,
        center: bool,
    },
    GoToParent(usize),
    GoToChild,
    GoToRevision(String),
    JumpToCommit(Oid),
    NavLogBack,
    NavLogForward,

    // ── Changes / details / diff (right pane) ──
    ChangesMove(isize),
    ChangesSelectRow(usize),
    ChangesExpand(bool),
    ChangesToggleGroupByDir,
    ChangesExpandAll(bool),
    SwitchMergeParent,
    ToggleDiffPreview,
    DiffCycleDock,
    ZoomDiff(bool),
    DiffNextFile(isize),
    DiffToggleSideBySide,
    DiffCycleContext,
    ScrollDiff(isize),
    ScrollDetails(isize),
    CopyText(String),
    FlushPendingDetails,

    SelectConsoleRow(usize),
    ScrollConsoleList(isize),
    ScrollConsoleOutput(isize),
    SelectCommitStageRow(usize),
    ScrollCommitStage(isize),
    StageExpand(bool),
    StageToggleRow(usize),
    StageToggleMulti(usize),
    StageSelectAll,
    StageToggleGroupByDir,
    StageExpandAll(bool),
    StageToggleShowIgnored,
    StageToggleDiffCollapsed,
    StageActionSelected,
    UnstageActionSelected,
    StageAllTracked,
    DiscardPromptSelected,
    StashAll,
    ResolveConflictsNotice,
    StageDiffMoveCursor(isize),
    StageDiffScroll(isize),
    StageDiffJumpHunk(isize),
    StageDiffToggleVisual,
    StageDiffToggleSideBySide,
    StageDiffCycleContext,
    StageDiffSwitchSide,
    StageDiffStageSelection,
    StageDiffUnstageSelection,
    StageDiffRollbackSelection,
    StageDiffUndoRollback,
    OpenThreeWayInspector {
        row_idx: Option<usize>,
        range_idx: Option<usize>,
    },
    ApplyStageDiffHunk(usize, DiffHunkActionKind),
    CommitEditorChar(char),
    CommitEditorNewline,
    CommitEditorPaste(String),
    CommitEditorBackspace,
    CommitEditorDelete,
    CommitEditorDeleteWord,
    CommitEditorMove(EditorMoveKind),
    CommitEditorUndo,
    CommitEditorRedo,
    CommitToggleAmend,
    CommitToggleSignoff,
    CommitToggleRunHooks,
    CommitToggleAuthorInput,
    CommitFocusAuthorInput,
    CommitAuthorChar(char),
    CommitAuthorBackspace,
    CommitAuthorDone,
    CommitApplyHistory(usize),
    SubmitCommit {
        and_push: bool,
    },
    UndoLastCommit,
    OpenExternalEditorMessage,
    OpenExternalEditorFile(Option<String>),
    RunCliProbe(Vec<String>),
    Resize,
    TickSpinner,
}

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
    pub latest_path_epoch: u64,

    pub commit_stage_selected_idx: usize,
    pub hook_error_output: Option<String>,

    pub console_entries: Vec<GitConsoleEntry>,
    pub console_selected_idx: usize,
    pub console_output_scroll: usize,

    pub next_cli_id: u64,
    pub loading_snapshot: bool,
    pub status_notice: Option<String>,
    pub spinner_frame: usize,
    pub needs_redraw: bool,
    pub should_quit: bool,
}

impl AppState {
    pub fn new(repo_path_display: PathBuf, repo_error: Option<String>) -> Self {
        let theme = ThemePalette::detect();
        let branch_colors = theme.branch_color_manager();
        Self {
            theme,
            glyph_mode: GlyphMode::Unicode,
            branch_colors,
            compact_refs: false,
            show_tag_names: true,
            log_graph: LogGraphState::default(),
            branches: BranchesPaneState::default(),
            log_filter: FilterBarState::default(),
            presentation: LogPresentation::default(),
            details: DetailsPaneState::default(),
            active_tab: WorkspaceTab::Log,
            focused_pane: WorkspaceTab::Log.default_pane(),
            splitters: SplitterState::default(),
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
            stage_workbench: StageWorkbenchState::default(),
            latest_repo_epoch: 0,
            latest_stage_epoch: 0,
            latest_stage_diff_epoch: 0,
            latest_commit_details_epoch: 0,
            latest_file_diff_epoch: 0,
            latest_path_epoch: 0,
            commit_stage_selected_idx: 0,
            hook_error_output: None,
            console_entries: Vec::new(),
            console_selected_idx: 0,
            console_output_scroll: 0,
            next_cli_id: 1,
            loading_snapshot: false,
            status_notice: None,
            spinner_frame: 0,
            needs_redraw: true,
            should_quit: false,
        }
    }

    pub fn ensure_graph_cache(&mut self) {
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
        self.branches.sync_model(self.repo_snapshot.as_ref(), false);
    }

    pub fn log_commit_at(&self, visible_row: usize) -> Option<&CommitSummary> {
        self.log_graph
            .commit_at(self.repo_snapshot.as_ref(), visible_row)
    }

    pub fn has_active_tasks(&self) -> bool {
        self.loading_snapshot
            || self.stage_workbench.commit_in_progress
            || self.console_entries.iter().any(GitConsoleEntry::is_running)
    }

    /// Next instant the main loop must wake up for (details debounce).
    pub fn next_deadline(&self) -> Option<Instant> {
        self.details.next_deadline()
    }

    pub fn spinner_char(&self) -> &'static str {
        const FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
        FRAMES[self.spinner_frame % FRAMES.len()]
    }

    pub fn stage_item_count(&self) -> usize {
        self.stage_workbench.rows.len()
    }

    pub fn stage_entry_at(&self, idx: usize) -> Option<jgit_core::StageEntry> {
        let row = self.stage_workbench.rows.get(idx)?;
        match &row.kind {
            StageTreeRowKind::File {
                path,
                old_path,
                status_kind,
                ..
            } => Some(jgit_core::StageEntry {
                path: path.clone(),
                old_path: old_path.clone(),
                kind: *status_kind,
            }),
            StageTreeRowKind::AmendFile {
                path,
                old_path,
                status_kind,
            } => Some(jgit_core::StageEntry {
                path: path.clone(),
                old_path: old_path.clone(),
                kind: *status_kind,
            }),
            _ => None,
        }
    }

    fn effective_focus_ring(&self) -> Vec<PaneId> {
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

    fn on_filter_changed(&mut self) {
        self.log_graph.filter_generation = self.log_graph.filter_generation.wrapping_add(1);
        self.ensure_graph_cache();
        if let Some(err) = &self.log_graph.filter_error {
            self.status_notice = Some(format!("Filter: {err}"));
        }
    }

    fn navigate_to_branch_node(&mut self) {
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
            self.status_notice = Some(format!("{label}: tip commit is hidden by current filters"));
        }
    }

    pub fn dispatch(&mut self, action: Action) -> Vec<Effect> {
        let mut effects = Vec::new();
        self.needs_redraw = true;
        self.ensure_graph_cache();
        let snap = self.repo_snapshot.clone();
        let width = self.terminal_area.width;

        match action {
            Action::Quit => {
                self.should_quit = true;
                effects.push(Effect::Quit);
            }
            Action::SwitchTab(tab) => {
                self.active_tab = tab;
                self.focused_pane = tab.default_pane();
                if tab == WorkspaceTab::Commit {
                    effects.push(Effect::RequestCommitPrecheck);
                }
            }
            Action::CycleFocus { reverse } => {
                let ring = self.effective_focus_ring();
                if let Some(pos) = ring.iter().position(|&p| p == self.focused_pane) {
                    let next = if reverse {
                        if pos == 0 { ring.len() - 1 } else { pos - 1 }
                    } else {
                        (pos + 1) % ring.len()
                    };
                    self.focused_pane = ring[next];
                } else if let Some(&first) = ring.first() {
                    self.focused_pane = first;
                }
                self.log_filter.editing_text = false;
            }
            Action::FocusPane(pane) => {
                self.focused_pane = pane;
                self.log_filter.editing_text = false;
            }
            Action::ToggleGlyphMode => {
                self.glyph_mode = self.glyph_mode.toggle();
                self.status_notice = Some(format!("GlyphMode: {}", self.glyph_mode.label()));
            }
            Action::ToggleCompactRefs => {
                return self.dispatch(Action::TogglePresentation(PresentationToggle::CompactRefs));
            }
            Action::ToggleTagNames => {
                return self.dispatch(Action::TogglePresentation(PresentationToggle::ShowTagNames));
            }
            Action::RefreshAll => {
                self.loading_snapshot = true;
                effects.push(Effect::RefreshRepoAndStage);
            }
            Action::OpenPopup {
                kind,
                initial_query,
            } => {
                self.modal_stack.push(ModalLayer::FloatingPopup {
                    kind,
                    query: initial_query,
                    selected: 0,
                });
            }
            Action::OpenContextMenu {
                anchor,
                items,
                depth,
            } => {
                if !items.is_empty() {
                    self.modal_stack.push(ModalLayer::ContextMenu {
                        anchor,
                        items,
                        selected: 0,
                        depth,
                    });
                }
            }
            Action::PopModal => {
                if let Some(ModalLayer::FloatingPopup {
                    kind: PopupKind::ThreeWayInspector,
                    ..
                }) = self.modal_stack.pop()
                {
                    self.stage_workbench.three_way_range_idx = None;
                }
            }
            Action::PopContextMenus => {
                while matches!(
                    self.modal_stack.last(),
                    Some(ModalLayer::ContextMenu { .. })
                ) {
                    self.modal_stack.pop();
                }
            }
            Action::ConfirmCriticalDialog => {
                if let Some(ModalLayer::CriticalDialog { confirm_effect, .. }) =
                    self.modal_stack.pop()
                {
                    if let Some(eff) = confirm_effect {
                        effects.push(eff);
                    } else {
                        self.loading_snapshot = true;
                        effects.push(Effect::RefreshRepoAndStage);
                    }
                }
            }
            Action::ResetSplitter(id) => {
                self.splitters.reset(id);
            }

            // ── Branches Dashboard ──
            Action::ToggleBranchesPane => {
                self.branches.toggle_collapsed(width);
                if self.branches.is_collapsed(width) {
                    if self.focused_pane == PaneId::LogBranchesTree {
                        self.focused_pane = PaneId::LogCommitTable;
                    }
                } else {
                    self.focused_pane = PaneId::LogBranchesTree;
                }
            }
            Action::SelectBranchRow(idx) => {
                if !self.branches.rows.is_empty() {
                    self.branches.selected = idx.min(self.branches.rows.len() - 1);
                    if self.branches.settings.navigate_on_select {
                        self.navigate_to_branch_node();
                    }
                }
            }
            Action::ScrollBranches(delta) => {
                self.branches.move_selection(delta);
                if self.branches.settings.navigate_on_select {
                    self.navigate_to_branch_node();
                }
            }
            Action::BranchesExpand(expand) => {
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
                if let Some(row) = self.branches.rows.get(idx).copied()
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
                if let Some(row) = self.branches.rows.get(idx).copied() {
                    let key = self.branches.model.nodes[row.node].key.clone();
                    if self.branches.multi.is_empty()
                        && let Some(cur) = self.branches.selected_node()
                        && cur.key != key
                    {
                        let cur_key = cur.key.clone();
                        self.branches.toggle_multi(&cur_key);
                    }
                    self.branches.toggle_multi(&key);
                    self.branches.selected = idx;
                }
            }
            Action::BranchesApplyFilter => {
                let names = self.branches.filter_ref_names();
                if !names.is_empty() {
                    self.log_filter.spec.branch = BranchFilter::from_names(names);
                    self.branches.multi.clear();
                    self.status_notice = Some(format!(
                        "Branch filter: {}",
                        self.log_filter.spec.branch.label()
                    ));
                    self.on_filter_changed();
                }
            }
            Action::BranchesNavigate => self.navigate_to_branch_node(),
            Action::BranchesToggleFavorite => {
                if let Some(node) = self.branches.selected_node().cloned()
                    && let BranchNodeKind::Leaf(kind) = node.kind
                    && kind != RefKind::Tag
                {
                    let (full, short) = if kind == RefKind::Head {
                        match snap.as_ref().and_then(|s| s.head_branch.clone()) {
                            Some(b) => (format!("refs/heads/{b}"), b),
                            None => return effects,
                        }
                    } else {
                        let name = node.ref_name.clone().unwrap_or_default();
                        let short = if kind == RefKind::RemoteBranch {
                            name.split_once('/')
                                .map_or(name.clone(), |(_, s)| s.to_string())
                        } else {
                            name
                        };
                        (node.key.clone(), short)
                    };
                    let on = self.branches.settings.favorites.toggle(&full, &short);
                    self.branches.save_settings();
                    self.branches.sync_model(snap.as_ref(), true);
                    self.branches.select_key(&node.key);
                    self.status_notice = Some(format!(
                        "{} {}",
                        if on { "★ Favorite:" } else { "Unfavorited:" },
                        short
                    ));
                }
            }
            Action::BranchesSearchInput(ch) => {
                self.branches.query.push(ch);
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
                if !self.branches.query.is_empty() {
                    let key = self.branches.selected_node().map(|n| n.key.clone());
                    self.branches.query.clear();
                    self.branches.refresh_rows();
                    if let Some(k) = key {
                        self.branches.select_key(&k);
                    }
                } else if !self.branches.multi.is_empty() {
                    self.branches.multi.clear();
                } else if !self.log_filter.spec.branch.is_empty() {
                    self.log_filter.spec.branch = BranchFilter::default();
                    self.status_notice = Some("Branch filter cleared".to_string());
                    self.on_filter_changed();
                }
            }
            Action::BranchesToggleGroupByDir => {
                let o = &mut self.branches.settings.options;
                o.group_by_directory = !o.group_by_directory;
                self.branches.save_settings();
                self.branches.sync_model(snap.as_ref(), true);
            }
            Action::BranchesToggleShowTags => {
                let o = &mut self.branches.settings.options;
                o.show_tags = !o.show_tags;
                self.branches.save_settings();
                self.branches.sync_model(snap.as_ref(), true);
            }
            Action::BranchesToggleNavigateOnSelect => {
                let s = &mut self.branches.settings;
                s.navigate_on_select = !s.navigate_on_select;
                self.branches.save_settings();
                self.status_notice = Some(format!(
                    "Navigate Log to Branch on Selection: {}",
                    on_off(self.branches.settings.navigate_on_select)
                ));
            }

            // ── Filter bar ──
            Action::FocusFilterText => {
                self.focused_pane = PaneId::LogCommitTable;
                self.log_filter.editing_text = true;
            }
            Action::FilterTextInput(ch) => {
                self.log_filter.spec.text.query.push(ch);
                self.on_filter_changed();
            }
            Action::FilterTextBackspace => {
                if self.log_filter.spec.text.query.pop().is_some() {
                    self.on_filter_changed();
                }
            }
            Action::FilterTextDone => {
                self.log_filter.editing_text = false;
            }
            Action::FilterTextClear => {
                self.log_filter.spec.text.query.clear();
                self.on_filter_changed();
            }
            Action::ToggleFilterRegex => {
                self.log_filter.spec.text.regex = !self.log_filter.spec.text.regex;
                self.on_filter_changed();
            }
            Action::ToggleFilterCase => {
                self.log_filter.spec.text.match_case = !self.log_filter.spec.text.match_case;
                self.on_filter_changed();
            }
            Action::ApplyFilterField { field, expr } => {
                if let Err(err) = field.apply(
                    &mut self.log_filter.spec,
                    &expr,
                    chrono::Local::now().timestamp(),
                ) {
                    self.status_notice = Some(err);
                    return effects;
                }
                self.on_filter_changed();
            }
            Action::ClearFilterField(field) => {
                field.clear(&mut self.log_filter.spec);
                self.on_filter_changed();
            }
            Action::ClearAllFilters => {
                self.log_filter.spec = Default::default();
                self.on_filter_changed();
            }
            Action::TogglePresentation(t) => {
                let p = &mut self.presentation;
                let o = &mut self.log_graph.options;
                let (label, on) = match t {
                    PresentationToggle::CompactRefs => {
                        self.compact_refs = !self.compact_refs;
                        ("Compact References View", self.compact_refs)
                    }
                    PresentationToggle::ShowTagNames => {
                        self.show_tag_names = !self.show_tag_names;
                        ("Show Tag Names", self.show_tag_names)
                    }
                    PresentationToggle::ShowLongEdges => {
                        o.show_long_edges = !o.show_long_edges;
                        ("Show Long Edges", o.show_long_edges)
                    }
                    PresentationToggle::IntelliSort => {
                        o.sort_type = match o.sort_type {
                            SortType::IntelliSort => SortType::Topo,
                            SortType::Topo => SortType::IntelliSort,
                        };
                        ("IntelliSort", o.sort_type == SortType::IntelliSort)
                    }
                    PresentationToggle::FirstParentOnly => {
                        o.first_parent_only = !o.first_parent_only;
                        ("First Parent Only", o.first_parent_only)
                    }
                    PresentationToggle::HighlightCurrentBranch => {
                        p.highlight_current_branch = !p.highlight_current_branch;
                        ("Highlight: Current Branch", p.highlight_current_branch)
                    }
                    PresentationToggle::HighlightMerges => {
                        p.highlight_merges = !p.highlight_merges;
                        ("Highlight: Merge Commits", p.highlight_merges)
                    }
                    PresentationToggle::ShowHashColumn => {
                        p.show_hash_column = !p.show_hash_column;
                        ("Hash column", p.show_hash_column)
                    }
                    PresentationToggle::RelativeDates => {
                        p.relative_dates = !p.relative_dates;
                        ("Relative dates", p.relative_dates)
                    }
                    PresentationToggle::UseAuthorDate => {
                        p.use_author_date = !p.use_author_date;
                        ("Author date (vs commit date)", p.use_author_date)
                    }
                };
                self.status_notice = Some(format!("{label}: {}", on_off(on)));
                self.ensure_graph_cache();
            }

            // ── Commit table ──
            Action::SelectLogRow(idx) => self.log_graph.select_row(idx),
            Action::ExtendLogSelectionTo(idx) => self.log_graph.extend_selection_to(idx),
            Action::ScrollLog(delta) => self.log_graph.scroll_by(delta, false),
            Action::ExtendLogSelection(delta) => self.log_graph.scroll_by(delta, true),
            Action::LogPage { half, down } => {
                let vh = self.log_graph.last_viewport_h.max(2);
                let step = if half { vh / 2 } else { vh.saturating_sub(1) }.max(1) as isize;
                self.log_graph
                    .scroll_by(if down { step } else { -step }, false);
            }
            Action::LogEdge { bottom } => {
                let len = self.log_graph.visible_count();
                self.log_graph.jump_to(
                    snap.as_ref(),
                    if bottom { len.saturating_sub(1) } else { 0 },
                    false,
                );
            }
            Action::JumpToLogRow { row, center } => {
                self.log_graph.jump_to(snap.as_ref(), row, center);
            }
            Action::GoToParent(idx) => match self.log_graph.parent_row(snap.as_ref(), idx) {
                Some(row) => self.log_graph.jump_to(snap.as_ref(), row, false),
                None => {
                    self.status_notice = Some(if idx == 0 {
                        "No visible parent commit".to_string()
                    } else {
                        format!("No visible parent #{}", idx + 1)
                    });
                }
            },
            Action::GoToChild => match self.log_graph.child_row() {
                Some(row) => self.log_graph.jump_to(snap.as_ref(), row, false),
                None => self.status_notice = Some("No visible child commit".to_string()),
            },
            Action::GoToRevision(expr) => {
                let expr = expr.trim().to_string();
                match snap.as_ref().and_then(|s| s.resolve_revision(&expr)) {
                    Some(oid) => {
                        if self.log_graph.jump_to_oid(snap.as_ref(), &oid) {
                            self.focused_pane = PaneId::LogCommitTable;
                            self.status_notice = Some(format!("Go to {expr} → {}", oid.short()));
                        } else {
                            self.status_notice = Some(format!(
                                "{expr} ({}) is hidden by current filters",
                                oid.short()
                            ));
                        }
                    }
                    None => self.status_notice = Some(format!("Revision '{expr}' not found")),
                }
            }
            Action::JumpToCommit(oid) => {
                if self.log_graph.jump_to_oid(snap.as_ref(), &oid) {
                    self.focused_pane = PaneId::LogCommitTable;
                } else {
                    self.status_notice =
                        Some(format!("Commit {} is not visible in the log", oid.short()));
                }
            }
            Action::NavLogBack => {
                self.log_graph.nav_back(snap.as_ref());
            }
            Action::NavLogForward => {
                self.log_graph.nav_forward(snap.as_ref());
            }

            // ── Right pane ──
            Action::ChangesMove(delta) => {
                let len = self.details.change_rows().len();
                if len > 0 {
                    self.details.changes_selected = (self.details.changes_selected as isize + delta)
                        .clamp(0, len as isize - 1)
                        as usize;
                    self.details.diff_scroll = 0;
                }
            }
            Action::ChangesSelectRow(idx) => {
                let len = self.details.change_rows().len();
                if len > 0 {
                    self.details.changes_selected = idx.min(len - 1);
                    self.details.diff_scroll = 0;
                }
            }
            Action::ChangesExpand(expand) => {
                let rows = self.details.change_rows();
                if let Some(row) = rows.get(self.details.changes_selected)
                    && matches!(row.kind, ChangeRowKind::Dir { .. })
                {
                    if expand {
                        self.details.changes_collapsed.remove(&row.key);
                    } else {
                        self.details.changes_collapsed.insert(row.key.clone());
                    }
                }
            }
            Action::ChangesToggleGroupByDir => {
                self.details.changes_group_by_dir = !self.details.changes_group_by_dir;
                self.details.changes_selected = 0;
            }
            Action::ChangesExpandAll(expand) => {
                if expand {
                    self.details.changes_collapsed.clear();
                } else if let Some(bundle) = self.details.current_bundle() {
                    let all = jgit_core::build_change_rows(
                        &bundle.changes,
                        true,
                        &rustc_hash::FxHashSet::default(),
                    );
                    self.details.changes_collapsed = all
                        .into_iter()
                        .filter(|r| r.depth == 0 && matches!(r.kind, ChangeRowKind::Dir { .. }))
                        .map(|r| r.key)
                        .collect();
                    self.details.changes_selected = 0;
                }
            }
            Action::SwitchMergeParent => {
                let oids = self.log_graph.selected_oids(snap.as_ref());
                let parents = match oids.as_slice() {
                    [one] => self
                        .log_graph
                        .commit_at(snap.as_ref(), self.log_graph.selected_idx)
                        .filter(|c| c.oid == *one)
                        .map_or(0, |c| c.parents.len()),
                    _ => 0,
                };
                if parents >= 2 {
                    self.details.merge_parent = (self.details.merge_parent + 1) % parents;
                    self.status_notice = Some(format!(
                        "Showing changes against parent #{} of {parents}",
                        self.details.merge_parent + 1
                    ));
                } else {
                    self.status_notice = Some("Not a merge commit".to_string());
                }
            }
            Action::ToggleDiffPreview => {
                self.details.diff_preview = !self.details.diff_preview;
                self.details.diff_scroll = 0;
            }
            Action::DiffCycleDock => {
                self.details.diff_dock = self.details.diff_dock.next();
                self.status_notice = Some(format!(
                    "Diff preview dock: {}",
                    self.details.diff_dock.label()
                ));
            }
            Action::ZoomDiff(on) => {
                self.details.diff_zoomed = on && self.details.selected_change().is_some();
                self.details.diff_scroll = 0;
            }
            Action::DiffNextFile(delta) => {
                let files = self.details.file_row_positions();
                if !files.is_empty() {
                    let cur = files
                        .iter()
                        .position(|&p| p == self.details.changes_selected)
                        .map_or(0, |i| i as isize + delta);
                    let next = cur.clamp(0, files.len() as isize - 1) as usize;
                    self.details.changes_selected = files[next];
                    self.details.diff_scroll = 0;
                }
            }
            Action::DiffToggleSideBySide => {
                self.details.diff_side_by_side = !self.details.diff_side_by_side;
            }
            Action::DiffCycleContext => {
                self.details.diff_context = self.details.diff_context.next();
                self.status_notice = Some(format!(
                    "Diff context: {}",
                    self.details.diff_context.label()
                ));
            }
            Action::ScrollDiff(delta) => {
                self.details.diff_scroll =
                    (self.details.diff_scroll as isize + delta).max(0) as usize;
            }
            Action::ScrollDetails(delta) => {
                self.details.details_scroll =
                    (self.details.details_scroll as isize + delta).max(0) as usize;
            }
            Action::CopyText(text) => {
                self.status_notice = Some(format!("Copied {text}"));
                effects.push(Effect::CopyToClipboard(text));
            }
            Action::FlushPendingDetails => {
                if let Some((key, _)) = self.details.pending.take()
                    && !self.details.cache.contains(&key)
                {
                    effects.push(Effect::RequestCommitDetails(key));
                }
            }

            Action::SelectConsoleRow(idx) => {
                if !self.console_entries.is_empty() {
                    self.console_selected_idx = idx.min(self.console_entries.len() - 1);
                    self.console_output_scroll = 0;
                }
            }
            Action::ScrollConsoleList(delta) => {
                let len = self.console_entries.len();
                if len > 0 {
                    let next = (self.console_selected_idx as isize + delta)
                        .clamp(0, (len - 1) as isize) as usize;
                    if next != self.console_selected_idx {
                        self.console_selected_idx = next;
                        self.console_output_scroll = 0;
                    }
                }
            }
            Action::ScrollConsoleOutput(delta) => {
                let next = (self.console_output_scroll as isize + delta).max(0) as usize;
                self.console_output_scroll = next;
            }
            Action::RunCliProbe(args) => {
                effects.push(Effect::RunCli {
                    args,
                    stdin_payload: None,
                    is_read_only: true,
                    refresh_after: RefreshScope::None,
                });
            }
            Action::Resize | Action::TickSpinner => {
                if matches!(action, Action::TickSpinner) {
                    self.spinner_frame = self.spinner_frame.wrapping_add(1);
                }
            }
            other => {
                let _ = self.dispatch_stage_action(&other, &mut effects);
            }
        }

        self.reconcile_log_requests(Instant::now(), &mut effects);
        self.reconcile_stage_requests(&mut effects);
        effects
    }

    /// Derives background requests from the current Log selection:
    /// debounced details (LRU hit → 0 ms), containing refs, diff preview and
    /// path-filter matches.
    pub fn reconcile_log_requests(&mut self, now: Instant, effects: &mut Vec<Effect>) {
        self.ensure_graph_cache();
        let snap = self.repo_snapshot.clone();

        // Path filter matches.
        if let Some(s) = &snap
            && self.log_filter.path_matches_pending(Some(s))
        {
            let paths = self.log_filter.spec.paths.paths.clone();
            let already = self
                .log_filter
                .path_requested
                .as_ref()
                .is_some_and(|req| req.paths == paths && Arc::ptr_eq(&req.snapshot, s));
            if !already {
                self.log_filter.path_requested = Some(PathRequest {
                    paths: paths.clone(),
                    snapshot: Arc::clone(s),
                });
                effects.push(Effect::RequestPathMatches(paths, Arc::clone(s)));
            }
        }

        // Commit details (debounced).
        let oids = self.log_graph.selected_oids(snap.as_ref());
        if self.details.current_key.as_ref().map(|k| &k.oids) != Some(&oids) {
            self.details.merge_parent = 0;
        }
        let desired = (!oids.is_empty()).then_some(DetailsKey {
            parent_index: if oids.len() == 1 {
                self.details.merge_parent
            } else {
                0
            },
            oids,
        });
        if desired != self.details.current_key {
            self.details.current_key = desired.clone();
            self.details.changes_selected = 0;
            self.details.details_scroll = 0;
            self.details.diff_scroll = 0;
            self.details.pending = match desired {
                Some(k) if self.details.cache.get(&k).is_some() => None,
                Some(k) => Some((k, now + DETAILS_DEBOUNCE)),
                None => None,
            };
            if self.details.current_bundle().is_none() {
                self.details.diff_zoomed = false;
            }
        }

        // Containing branches / tags (once the selection settles).
        if self.details.pending.is_none()
            && let Some(key) = &self.details.current_key
            && let [oid] = key.oids.as_slice()
            && self
                .details
                .containing
                .as_ref()
                .is_none_or(|(o, _)| o != oid)
            && let (Some(s), Some(cached)) = (&snap, &self.log_graph.cached)
            && let Some(node) = cached.permanent.linear.node_of_oid(oid)
        {
            let names = jgit_core::graph::ReachableNodes::containing_branches(
                &cached.permanent.linear,
                node,
                &s.refs,
            )
            .into_iter()
            .map(|r| r.name.clone())
            .collect();
            self.details.containing = Some((*oid, names));
        }

        // Diff preview.
        if self.details.diff_visible()
            && let Some(key) = self.details.desired_diff_key()
        {
            if self.details.diff_cache.get(&key).is_some() {
                let _ = self.details.diff_syntax.get(&key);
            } else if self.details.requested_diff.as_ref() != Some(&key) {
                self.details.requested_diff = Some(key.clone());
                effects.push(Effect::RequestFileDiff(key));
            }
        }
    }

    pub fn apply_engine_event(&mut self, event: EngineEvent) -> Vec<Effect> {
        let mut effects = Vec::new();
        self.needs_redraw = true;

        let event = match self.apply_stage_engine_event(event, &mut effects) {
            None => {
                self.reconcile_log_requests(Instant::now(), &mut effects);
                self.reconcile_stage_requests(&mut effects);
                return effects;
            }
            Some(ev) => ev,
        };

        match event {
            EngineEvent::RepoSnapshotLoaded { snapshot, epoch } => {
                if epoch < self.latest_repo_epoch {
                    return effects;
                }
                self.latest_repo_epoch = epoch;
                self.loading_snapshot = false;
                if self.branches.settings_path.is_none()
                    && !snapshot.common_dir.as_os_str().is_empty()
                {
                    self.branches
                        .load_settings(snapshot.common_dir.join("jgit-branches-panel"));
                    self.stage_workbench
                        .load_history(snapshot.common_dir.join("jgit-commit-history"));
                    self.stage_workbench
                        .load_draft(snapshot.common_dir.join("jgit-commit-draft"));
                }
                let prev_head = self.repo_snapshot.as_ref().and_then(|s| s.head_oid);
                let head_changed = prev_head.is_some() && prev_head != snapshot.head_oid;
                if head_changed && !self.stage_workbench.amend {
                    self.stage_workbench.amend_preview = None;
                }
                self.log_graph.on_snapshot_loaded();
                self.details.containing = None;
                self.repo_snapshot = Some(snapshot);
                if self.stage_workbench.amend {
                    effects.push(Effect::RequestAmendPreview);
                }
            }
            EngineEvent::CommitDetailsLoaded { bundle, epoch } => {
                // Cache every result; only the current key is rendered, so
                // superseded epochs can never paint stale details.
                self.latest_commit_details_epoch = self.latest_commit_details_epoch.max(epoch);
                self.details.cache.insert(bundle.key.clone(), bundle);
            }
            EngineEvent::FileDiffLoaded { diff, epoch } => {
                self.latest_file_diff_epoch = self.latest_file_diff_epoch.max(epoch);
                if self.details.requested_diff.as_ref() == Some(&diff.key) {
                    self.details.requested_diff = None;
                }
                if !diff.is_binary && !diff.hunks.is_empty() {
                    let hl = Arc::new(HighlightedDiff::build(&diff));
                    self.details.diff_syntax.insert(diff.key.clone(), hl);
                }
                self.details.diff_cache.insert(diff.key.clone(), diff);
            }
            EngineEvent::PathMatchesLoaded {
                paths,
                matches,
                epoch,
            } => {
                if epoch < self.latest_path_epoch {
                    return effects;
                }
                self.latest_path_epoch = epoch;
                if let Some(req) = self.log_filter.path_requested.take()
                    && req.paths == paths
                {
                    self.log_filter.path_matches = Some(PathMatches {
                        paths,
                        snapshot: req.snapshot,
                        oids: matches,
                    });
                    self.on_filter_changed();
                }
            }
            EngineEvent::StageStatusLoaded { .. }
            | EngineEvent::StageFileDiffLoaded { .. }
            | EngineEvent::AmendPreviewLoaded { .. }
            | EngineEvent::CommitPrecheckLoaded { .. }
            | EngineEvent::CommitCompleted(_)
            | EngineEvent::UndoCommitCompleted { .. } => {
                unreachable!("handled by apply_stage_engine_event")
            }
            EngineEvent::ConsoleStarted(entry) => {
                self.console_entries.insert(0, entry);
                self.console_selected_idx = 0;
                self.console_output_scroll = 0;
            }
            EngineEvent::ConsoleOutput {
                id,
                is_stderr,
                line,
            } => {
                if let Some(entry) = self.console_entries.iter_mut().find(|e| e.id == id) {
                    if is_stderr {
                        entry.stderr_lines.push(line);
                    } else {
                        entry.stdout_lines.push(line);
                    }
                }
            }
            EngineEvent::ConsoleFinished {
                id,
                exit_code,
                duration_ms,
            } => {
                if let Some(entry) = self.console_entries.iter_mut().find(|e| e.id == id) {
                    entry.exit_code = Some(exit_code);
                    entry.duration_ms = Some(duration_ms);
                }
            }
            EngineEvent::IndexLockDetected {
                lock_path,
                message,
                retry_args,
                retry_stdin,
                retry_is_read_only,
                retry_refresh_after,
            } => {
                self.modal_stack.push(ModalLayer::CriticalDialog {
                    title: "Git Index Lock Conflict".to_string(),
                    body: format!(
                        "Another Git process appears to hold:\n{}\n\n{}",
                        lock_path.display(),
                        message
                    ),
                    confirm_label: "Retry".to_string(),
                    cancel_label: "Dismiss".to_string(),
                    selected_button: ModalButton::Confirm,
                    confirm_effect: Some(Effect::RunCli {
                        args: retry_args,
                        stdin_payload: retry_stdin,
                        is_read_only: retry_is_read_only,
                        refresh_after: retry_refresh_after,
                    }),
                });
            }
            EngineEvent::ExternalFsChanged(scope) => {
                effects.push(Effect::RefreshScope(scope));
            }
            EngineEvent::Error(msg) => {
                self.loading_snapshot = false;
                self.details.requested_diff = None;
                self.stage_workbench.requested_diff = None;
                self.stage_workbench.commit_in_progress = false;
                self.status_notice = Some(format!("Engine: {msg}"));
            }
        }

        self.reconcile_log_requests(Instant::now(), &mut effects);
        self.reconcile_stage_requests(&mut effects);
        effects
    }
}

fn on_off(on: bool) -> &'static str {
    if on { "on" } else { "off" }
}

pub fn execute_effects(effects: Vec<Effect>, state: &mut AppState, engine: Option<&EngineHandle>) {
    for effect in effects {
        if let Effect::CopyToClipboard(text) = &effect {
            crate::terminal::copy_to_clipboard(text);
            continue;
        }
        let Some(handle) = engine else {
            continue;
        };
        match effect {
            Effect::RefreshRepoAndStage => {
                let show_ignored = state.stage_workbench.tree_opts.show_ignored;
                state.latest_repo_epoch =
                    state.latest_repo_epoch.max(handle.request_repo_snapshot());
                state.latest_stage_epoch = state
                    .latest_stage_epoch
                    .max(handle.request_stage_status(show_ignored));
            }
            Effect::RefreshScope(scope) => {
                let show_ignored = state.stage_workbench.tree_opts.show_ignored;
                if scope.needs_graph_refresh() {
                    state.latest_repo_epoch =
                        state.latest_repo_epoch.max(handle.request_repo_snapshot());
                }
                if scope.needs_stage_refresh() {
                    state.latest_stage_epoch = state
                        .latest_stage_epoch
                        .max(handle.request_stage_status(show_ignored));
                }
            }
            Effect::RequestCommitDetails(key) => {
                state.latest_commit_details_epoch = state
                    .latest_commit_details_epoch
                    .max(handle.request_commit_details(key));
            }
            Effect::RequestFileDiff(key) => {
                state.latest_file_diff_epoch = state
                    .latest_file_diff_epoch
                    .max(handle.request_file_diff(key));
            }
            Effect::RequestPathMatches(paths, snapshot) => {
                state.latest_path_epoch = state
                    .latest_path_epoch
                    .max(handle.request_path_matches(paths, snapshot));
            }
            Effect::RequestStageFileDiff(key) => {
                state.latest_stage_diff_epoch = state
                    .latest_stage_diff_epoch
                    .max(handle.request_stage_file_diff(key));
            }
            Effect::RequestAmendPreview => {
                handle.request_amend_preview();
            }
            Effect::RequestCommitPrecheck => {
                handle.request_commit_precheck();
            }
            Effect::ApplyStageBuffer {
                path,
                new_staged_bytes,
                file_mode,
                remove_from_index,
            } => {
                handle.send(TaskRequest::ApplyStageBuffer {
                    path,
                    new_staged_bytes,
                    file_mode,
                    remove_from_index,
                });
            }
            Effect::WriteWorktreeBuffer {
                path,
                new_local_bytes,
            } => {
                handle.send(TaskRequest::WriteWorktreeBuffer {
                    path,
                    new_local_bytes,
                });
            }
            Effect::StageTargets(targets) => {
                handle.send(TaskRequest::StageTargets { targets });
            }
            Effect::StageAllTracked => {
                handle.send(TaskRequest::StageAllTracked);
            }
            Effect::UnstageTargets(targets) => {
                handle.send(TaskRequest::UnstageTargets { targets });
            }
            Effect::DiscardTargets(targets) => {
                handle.send(TaskRequest::DiscardTargets { targets });
            }
            Effect::ExecuteCommit(request) => {
                state.stage_workbench.commit_in_progress = true;
                let id = state.next_cli_id;
                state.next_cli_id += 1;
                handle.send(TaskRequest::ExecuteCommit { id, request });
            }
            Effect::UndoLastCommit => {
                handle.send(TaskRequest::UndoLastCommit);
            }
            Effect::OpenExternalEditorMessage | Effect::OpenExternalEditorFile(_) => {
                // Handled in main loop where TerminalGuard can pause raw mode
            }
            Effect::RunCli {
                args,
                stdin_payload,
                is_read_only,
                refresh_after,
            } => {
                let id = state.next_cli_id;
                state.next_cli_id += 1;
                handle.send(TaskRequest::RunGitCli {
                    id,
                    args,
                    stdin_payload,
                    is_read_only,
                    refresh_after,
                });
            }
            Effect::CopyToClipboard(_) | Effect::Quit => {}
        }
    }
}
