use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use jbgit_core::graph::SortType;
use jbgit_core::{
    BranchFilter, BranchNodeKind, ChangeRowKind, CommitSummary, ConflictChoice, ConflictDocument,
    DetailsKey, FileDiffKey, GitConsoleEntry, GitOp, ModalButton, Oid, PaneId, RefKind,
    RefreshScope, RepoSnapshot, SplitAxis, SplitterId, StageStatusSnapshot, StageTarget,
    StageTreeRowKind, WorkspaceTab,
};
use jbgit_engine::{
    CommitRequest, EngineEvent, EngineHandle, StageDiffKey, TaskRequest,
};
use ratatui::layout::Rect;
use rustc_hash::FxHashMap;

use crate::dialogs::Dialog;
use crate::graph::LogGraphState;
use crate::graph::state::FilterInputs;
use crate::hit_test::{DiffHunkActionKind, HitTarget, HitTestRegistry};
use crate::log_view::{
    BranchesPaneState, DETAILS_DEBOUNCE, DetailsPaneState, DiffPreviewDock, FilterBarState,
    FilterField, LogPresentation, PathMatches, PathRequest, PresentationToggle,
};
use crate::stage_view::StageWorkbenchState;
use crate::stash_view::StashTabState;
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
    /// Graph backend selector; `None` renders with a plain [`crate::graph::CellGridGraphBackend`].
    pub graph_renderer: Option<&'a mut crate::graph::GraphRenderer>,
    pub i18n: Option<&'a crate::i18n::I18n>,
    pub config: Option<&'a crate::config::AppConfig>,
}

impl<'a> RenderContext<'a> {
    pub fn minimal(
        theme: &'a ThemePalette,
        glyph_mode: GlyphMode,
        branch_colors: &'a BranchColorManager,
        hit_registry: &'a mut HitTestRegistry,
    ) -> Self {
        Self {
            theme,
            glyph_mode,
            branch_colors,
            hit_registry,
            focused_pane: PaneId::LogCommitTable,
            hover_target: None,
            dragging_splitter: None,
            graph_renderer: None,
            i18n: None,
            config: None,
        }
    }

    pub fn rebind_hits<'b>(&'a self, hit_registry: &'b mut HitTestRegistry) -> RenderContext<'b>
    where
        'a: 'b,
    {
        RenderContext {
            theme: self.theme,
            glyph_mode: self.glyph_mode,
            branch_colors: self.branch_colors,
            hit_registry,
            focused_pane: self.focused_pane,
            hover_target: self.hover_target,
            dragging_splitter: self.dragging_splitter,
            graph_renderer: None,
            i18n: self.i18n,
            config: self.config,
        }
    }

    pub fn ok(&self) -> &str {
        self.i18n.map_or("OK", |i| i.ok())
    }

    pub fn cancel(&self) -> &str {
        self.i18n.map_or("Cancel", |i| i.cancel())
    }

    pub fn t<'s>(&'s self, key: &'s str) -> &'s str {
        self.i18n.map_or(key, |i| i.t(key))
    }

    pub fn t_fmt(&self, key: &str, args: &[(&str, &str)]) -> String {
        match self.i18n {
            Some(i) => i.t_fmt(key, args),
            None => key.to_string(),
        }
    }

    pub fn pane_title(&self, pane: PaneId) -> &str {
        self.i18n.map_or(pane.label(), |i| i.pane_title(pane))
    }

    pub fn branch_category_title(&self, cat: jbgit_core::BranchCategory) -> &str {
        self.i18n
            .map_or(cat.title(), |i| i.branch_category_title(cat))
    }

    pub fn diff_context_label(&self, ctx: jbgit_core::DiffContext) -> &str {
        self.i18n
            .map_or(ctx.label(), |i| i.diff_context_label(ctx))
    }

    pub fn diff_dock_label(&self, dock: DiffPreviewDock) -> &str {
        self.i18n.map_or(dock.label(), |i| i.diff_dock_label(dock))
    }

    pub fn todo_action_label(&self, act: jbgit_core::TodoAction) -> &str {
        self.i18n.map_or(act.label(), |i| i.todo_action_label(act))
    }

    pub fn format_relative_time(&self, ts: i64, now: i64) -> String {
        match self.i18n {
            Some(i) => i.format_relative_time(ts, now),
            None => crate::log_view::format_relative_time(ts, now),
        }
    }

    pub fn diff_syntax_enabled(&self) -> bool {
        self.config
            .is_none_or(|c| c.jbgit.diff.syntax_highlight)
    }

    pub fn diff_tab_size(&self) -> usize {
        self.config.map_or(4, |c| c.jbgit.diff.tab_size.max(1))
    }
}

#[derive(Debug, Clone)]
pub enum ContextMenuAction {
    RefreshRepo,
    RunReadOnlyProbe(Vec<String>),
    TogglePresentation(PresentationToggle),
    SetGraphRenderer(crate::graph::GraphRendererMode),
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
    /// Arbitrary follow-up actions (Spec 05 Git workflows).
    Run(Vec<Action>),
}

#[derive(Debug, Clone)]
pub enum ContextMenuEntryKind {
    Action(ContextMenuAction),
    Submenu(Vec<ContextMenuItem>),
    /// `├─────┤` group divider (never selectable).
    Separator,
}

#[derive(Debug, Clone)]
pub struct ContextMenuItem {
    pub label: String,
    pub mnemonic: Option<char>,
    pub shortcut: Option<String>,
    /// 1-cell icon in the left gutter.
    pub icon: Option<&'static str>,
    /// Greyed-out but visible; the reason is shown in the footer when focused.
    pub disabled_reason: Option<String>,
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
            icon: None,
            disabled_reason: None,
            kind: ContextMenuEntryKind::Action(action),
        }
    }

    /// Item running arbitrary [`Action`]s.
    pub fn run(
        label: impl Into<String>,
        mnemonic: Option<char>,
        shortcut: Option<&str>,
        actions: Vec<Action>,
    ) -> Self {
        Self::action(label, mnemonic, shortcut, ContextMenuAction::Run(actions))
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
            icon: None,
            disabled_reason: None,
            kind: ContextMenuEntryKind::Submenu(items),
        }
    }

    pub fn separator() -> Self {
        Self {
            label: String::new(),
            mnemonic: None,
            shortcut: None,
            icon: None,
            disabled_reason: None,
            kind: ContextMenuEntryKind::Separator,
        }
    }

    pub fn icon(mut self, icon: &'static str) -> Self {
        self.icon = Some(icon);
        self
    }

    /// Disables the item with `reason` unless `ok`.
    pub fn enabled_if(mut self, ok: bool, reason: impl Into<String>) -> Self {
        if !ok && self.disabled_reason.is_none() {
            self.disabled_reason = Some(reason.into());
        }
        self
    }

    pub fn require(self, check: Result<(), String>) -> Self {
        match check {
            Ok(()) => self,
            Err(reason) => self.enabled_if(false, reason),
        }
    }

    pub fn is_separator(&self) -> bool {
        matches!(self.kind, ContextMenuEntryKind::Separator)
    }

    pub fn is_enabled(&self) -> bool {
        self.disabled_reason.is_none() && !self.is_separator()
    }
}

/// Where a cascading submenu hangs off its parent menu / popup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MenuParent {
    pub rect: Rect,
    pub row_y: u16,
    pub item_idx: usize,
}

/// Footer notification with an optional one-click follow-up
/// (`[Restore Branch]` after a delete, …).
#[derive(Debug, Clone)]
pub struct Notification {
    pub text: String,
    pub is_error: bool,
    pub action: Option<(String, Vec<Action>)>,
}

/// Pending 150 ms hover-to-open (or close) of a cascading submenu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MenuHover {
    pub depth: u8,
    pub item_idx: usize,
    pub due: Instant,
}

pub const MENU_HOVER_DELAY: Duration = Duration::from_millis(150);

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
    /// Built-in three-pane conflict merge resolver (`Ours | Result | Theirs`).
    ConflictMerge,
    /// Hook or commit failure output dialog.
    HookError,
}

#[derive(Debug, Clone, Default)]
pub struct ConflictMergeState {
    pub path: String,
    pub doc: Option<ConflictDocument>,
    pub selected_block: usize,
    pub scroll: usize,
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
        /// Set for cascading submenus (placed beside the parent, flipping left).
        parent: Option<MenuParent>,
    },
    /// Layer 4: input / choice / message / rebase-todo dialog.
    Dialog(Box<Dialog>),
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
            Self::CriticalDialog { .. } | Self::Dialog(_) => 40,
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
    OpenExternalMergetool(String),
    LoadConflictDocument(String),
    SaveConflictResolution {
        path: String,
        content: Vec<u8>,
        stage: bool,
    },
    CopyToClipboard(String),
    RunCli {
        args: Vec<String>,
        stdin_payload: Option<Vec<u8>>,
        is_read_only: bool,
        refresh_after: RefreshScope,
    },
    /// High-level Git write operation (Spec 05).
    GitOp(GitOp),
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
    BranchesSearchStart,
    BranchesSearchDone,
    BranchesSearchClear,
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
    SetGraphRenderer(crate::graph::GraphRendererMode),
    CycleGraphRenderer,

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
    DiffJumpHunk(isize),
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
    StageDiffNextFile(isize),
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

    // ── Spec 05: branches popup, menus, dialogs & Git workflows ──
    OpenSubmenu {
        parent: MenuParent,
        items: Vec<ContextMenuItem>,
        depth: u8,
    },
    Notice(String),
    RunGitOp(GitOp),
    OpenDialog(Box<Dialog>),
    /// Confirm the topmost dialog (validated).
    DialogConfirm,
    SetTodoMessage {
        idx: usize,
        message: String,
    },
    OpenBranchesPopup {
        query: String,
    },
    ToggleFavoriteRef {
        full: String,
        short: String,
    },
    /// `origin/x` → smart tracking checkout (`GitRemoteBranchCheckoutUtil`).
    CheckoutRemoteBranch {
        remote_ref: String,
    },
    CheckoutRevisionExpr(String),
    CompareWithCurrent {
        branch: String,
    },
    CompareVersions {
        base: Oid,
        target: Oid,
    },
    CompareWithLocal {
        target: Oid,
    },
    OpenConflictResolver(String),
    OpenExternalMergetool(String),
    ConflictSelectBlock(isize),
    ConflictSetBlock(usize),
    ConflictApplyChoice(ConflictChoice),
    ConflictAcceptAll(ConflictChoice),
    ConflictScroll(isize),
    ConflictSaveAndStage,
    PromptDeleteBranch {
        name: String,
    },
    PromptPush {
        branch: String,
        up_to: Option<Oid>,
    },
    UncommitHead,
    OngoingResolve,
    NotificationActivate,
    NotificationDismiss,
    StashSelect(usize),
    StashMove(isize),
    StashChangesSelect(usize),
    StashChangesMove(isize),
    StashChangesExpand(bool),
    StashChangesToggleGroupByDir,
    StashChangesExpandAll(bool),
    StashDiffToggleSideBySide,
    StashDiffCycleContext,
    StashDiffJumpHunk(isize),
    StashDiffNextFile(isize),
    StashDiffScroll(isize),
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

    /// Footer notification (`[Restore Branch]`, operation errors).
    pub notification: Option<Notification>,
    /// Hover-to-open timer of cascading submenus.
    pub menu_hover: Option<MenuHover>,
    /// Number of Git write operations in flight.
    pub git_ops_running: usize,
    pub stash: StashTabState,
    pub conflict_merge: Option<ConflictMergeState>,
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

        let locales_dir = crate::config::discovery::find_config_file("locales", Some(&repo_path_display))
            .or_else(|| {
                let global_loc = crate::config::discovery::global_config_dir().join("locales");
                if global_loc.is_dir() { Some(global_loc) } else { None }
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
            splitters: SplitterState::with_default_ratios(config.jbgit.layout.splitter_defaults()),
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
            notification: None,
            menu_hover: None,
            git_ops_running: 0,
            stash,
            conflict_merge: None,
            config,
            i18n,
            pending_keys: Vec::new(),
            pending_key_at: None,
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
            || self.git_ops_running > 0
            || self.console_entries.iter().any(GitConsoleEntry::is_running)
    }

    /// Next instant the main loop must wake up for (details debounce, menu hover, or key chord timeout).
    pub fn next_deadline(&self) -> Option<Instant> {
        let chord = self
            .pending_key_at
            .map(|t| t + crate::keymap::KEY_CHORD_WINDOW);
        [
            self.details.next_deadline(),
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
            self.status_notice = Some(
                self.i18n
                    .t_fmt("toast.filter_error", &[("err", &err.to_string())]),
            );
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
            self.status_notice = Some(
                self.i18n
                    .t_fmt("toast.tip_hidden_by_filter", &[("label", &label)]),
            );
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
                self.log_filter.editing_text = false;
                self.branches.searching = false;
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
                if self.focused_pane != PaneId::LogBranchesTree {
                    self.branches.searching = false;
                }
            }
            Action::FocusPane(pane) => {
                self.focused_pane = pane;
                self.log_filter.editing_text = false;
                if self.focused_pane != PaneId::LogBranchesTree {
                    self.branches.searching = false;
                }
            }
            Action::ToggleGlyphMode => {
                self.glyph_mode = self.glyph_mode.toggle();
                self.status_notice = Some(
                    self.i18n
                        .t_fmt("toast.glyph_mode", &[("mode", self.glyph_mode.label())]),
                );
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
                    let selected = items.iter().position(|i| !i.is_separator()).unwrap_or(0);
                    self.menu_hover = None;
                    self.modal_stack.push(ModalLayer::ContextMenu {
                        anchor,
                        items,
                        selected,
                        depth,
                        parent: None,
                    });
                }
            }
            Action::PopModal => {
                self.menu_hover = None;
                match self.modal_stack.pop() {
                    Some(ModalLayer::FloatingPopup {
                        kind: PopupKind::ThreeWayInspector,
                        ..
                    }) => {
                        self.stage_workbench.three_way_range_idx = None;
                    }
                    Some(ModalLayer::FloatingPopup {
                        kind: PopupKind::ConflictMerge,
                        ..
                    }) => {
                        self.conflict_merge = None;
                    }
                    _ => {}
                }
            }
            Action::PopContextMenus => {
                self.menu_hover = None;
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
                    let label = self.log_filter.spec.branch.label();
                    self.status_notice = Some(
                        self.i18n
                            .t_fmt("toast.branch_filter_set", &[("label", &label)]),
                    );
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
                    self.status_notice = Some(self.i18n.t_fmt(
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
                self.focused_pane = PaneId::LogBranchesTree;
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
                } else if !self.log_filter.spec.branch.is_empty() {
                    self.log_filter.spec.branch = BranchFilter::default();
                    self.status_notice =
                        Some(self.i18n.t("toast.branch_filter_cleared").to_string());
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
                let st = self.i18n.on_off(self.branches.settings.navigate_on_select);
                self.status_notice = Some(self.i18n.t_fmt("toast.nav_on_select", &[("state", st)]));
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
                if let Err(err) = field.apply_i18n(
                    &mut self.log_filter.spec,
                    &expr,
                    chrono::Local::now().timestamp(),
                    &self.i18n,
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
            Action::SetGraphRenderer(mode) => {
                self.graph_renderer.set_mode(mode);
            }
            Action::CycleGraphRenderer => {
                let next = self.graph_renderer.mode().next();
                return self.dispatch(Action::SetGraphRenderer(next));
            }
            Action::TogglePresentation(t) => {
                let p = &mut self.presentation;
                let o = &mut self.log_graph.options;
                let (label_key, on) = match t {
                    PresentationToggle::CompactRefs => {
                        self.compact_refs = !self.compact_refs;
                        ("presentation.compact_refs", self.compact_refs)
                    }
                    PresentationToggle::ShowTagNames => {
                        self.show_tag_names = !self.show_tag_names;
                        ("presentation.show_tag_names", self.show_tag_names)
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
                    PresentationToggle::HighlightMerges => {
                        p.highlight_merges = !p.highlight_merges;
                        ("presentation.highlight_merges", p.highlight_merges)
                    }
                    PresentationToggle::ShowHashColumn => {
                        p.show_hash_column = !p.show_hash_column;
                        ("presentation.show_hash_column", p.show_hash_column)
                    }
                    PresentationToggle::RelativeDates => {
                        p.relative_dates = !p.relative_dates;
                        ("presentation.relative_dates", p.relative_dates)
                    }
                    PresentationToggle::UseAuthorDate => {
                        p.use_author_date = !p.use_author_date;
                        ("presentation.use_author_date", p.use_author_date)
                    }
                };
                let label = self.i18n.t(label_key);
                let state = self.i18n.on_off(on);
                self.status_notice = Some(
                    self.i18n
                        .t_fmt("toast.presentation_toggle", &[("label", label), ("state", state)]),
                );
                self.ensure_graph_cache();
            }

            // ── Commit table ──
            Action::SelectLogRow(idx) => {
                self.details.compare_override = None;
                self.log_graph.select_row(idx);
            }
            Action::ExtendLogSelectionTo(idx) => {
                self.details.compare_override = None;
                self.log_graph.extend_selection_to(idx);
            }
            Action::ScrollLog(delta) => {
                self.details.compare_override = None;
                self.log_graph.scroll_by(delta, false);
            }
            Action::ExtendLogSelection(delta) => {
                self.details.compare_override = None;
                self.log_graph.scroll_by(delta, true);
            }
            Action::LogPage { half, down } => {
                self.details.compare_override = None;
                let vh = self.log_graph.last_viewport_h.max(2);
                let step = if half { vh / 2 } else { vh.saturating_sub(1) }.max(1) as isize;
                self.log_graph
                    .scroll_by(if down { step } else { -step }, false);
            }
            Action::LogEdge { bottom } => {
                self.details.compare_override = None;
                let len = self.log_graph.visible_count();
                self.log_graph.jump_to(
                    snap.as_ref(),
                    if bottom { len.saturating_sub(1) } else { 0 },
                    false,
                );
            }
            Action::JumpToLogRow { row, center } => {
                self.details.compare_override = None;
                self.log_graph.jump_to(snap.as_ref(), row, center);
            }
            Action::GoToParent(idx) => {
                self.details.compare_override = None;
                match self.log_graph.parent_row(snap.as_ref(), idx) {
                    Some(row) => self.log_graph.jump_to(snap.as_ref(), row, false),
                    None => {
                        self.status_notice = Some(if idx == 0 {
                            self.i18n.t("toast.no_visible_parent").to_string()
                        } else {
                            let n_s = (idx + 1).to_string();
                            self.i18n.t_fmt("toast.no_visible_parent_n", &[("n", &n_s)])
                        });
                    }
                }
            }
            Action::GoToChild => {
                self.details.compare_override = None;
                match self.log_graph.child_row() {
                    Some(row) => self.log_graph.jump_to(snap.as_ref(), row, false),
                    None => {
                        self.status_notice = Some(self.i18n.t("toast.no_visible_child").to_string());
                    }
                }
            }
            Action::GoToRevision(expr) => {
                self.details.compare_override = None;
                let expr = expr.trim().to_string();
                match snap.as_ref().and_then(|s| s.resolve_revision(&expr)) {
                    Some(oid) => {
                        let short = oid.short();
                        if self.log_graph.jump_to_oid(snap.as_ref(), &oid) {
                            self.focused_pane = PaneId::LogCommitTable;
                            self.status_notice = Some(
                                self.i18n
                                    .t_fmt("toast.goto_ok", &[("expr", &expr), ("short", &short)]),
                            );
                        } else {
                            self.status_notice = Some(
                                self.i18n
                                    .t_fmt("toast.goto_hidden", &[("expr", &expr), ("short", &short)]),
                            );
                        }
                    }
                    None => {
                        self.status_notice =
                            Some(self.i18n.t_fmt("toast.goto_not_found", &[("expr", &expr)]));
                    }
                }
            }
            Action::JumpToCommit(oid) => {
                self.details.compare_override = None;
                if self.log_graph.jump_to_oid(snap.as_ref(), &oid) {
                    self.focused_pane = PaneId::LogCommitTable;
                } else {
                    let short = oid.short();
                    self.status_notice = Some(
                        self.i18n
                            .t_fmt("toast.commit_not_visible", &[("short", &short)]),
                    );
                }
            }
            Action::NavLogBack => {
                self.details.compare_override = None;
                self.log_graph.nav_back(snap.as_ref());
            }
            Action::NavLogForward => {
                self.details.compare_override = None;
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
                    let all = jbgit_core::build_change_rows(
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
                    let cur_s = (self.details.merge_parent + 1).to_string();
                    let tot_s = parents.to_string();
                    self.status_notice = Some(self.i18n.t_fmt(
                        "toast.merge_parent_switched",
                        &[("cur", &cur_s), ("total", &tot_s)],
                    ));
                } else {
                    self.status_notice = Some(self.i18n.t("toast.not_merge_commit").to_string());
                }
            }
            Action::ToggleDiffPreview => {
                self.details.diff_preview = !self.details.diff_preview;
                self.details.diff_scroll = 0;
            }
            Action::DiffCycleDock => {
                self.details.diff_dock = self.details.diff_dock.next();
                let dock_lbl = self.i18n.diff_dock_label(self.details.diff_dock);
                self.status_notice =
                    Some(self.i18n.t_fmt("toast.diff_dock_set", &[("dock", dock_lbl)]));
            }
            Action::ZoomDiff(on) => {
                self.details.diff_zoomed = on && self.details.selected_change().is_some();
                self.details.diff_scroll = 0;
            }
            Action::DiffNextFile(delta) => {
                self.details.select_next_file(delta);
            }
            Action::DiffJumpHunk(delta) => {
                self.details.jump_diff_hunk(delta);
            }
            Action::DiffToggleSideBySide => {
                self.details.diff_side_by_side = !self.details.diff_side_by_side;
            }
            Action::DiffCycleContext => {
                self.details.diff_context = self.details.diff_context.next();
                let ctx_lbl = self.i18n.diff_context_label(self.details.diff_context);
                self.status_notice = Some(
                    self.i18n
                        .t_fmt("toast.diff_context_set", &[("ctx", ctx_lbl)]),
                );
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
                self.status_notice = Some(self.i18n.t_fmt("toast.copied_text", &[("text", &text)]));
                effects.push(Effect::CopyToClipboard(text));
            }
            Action::FlushPendingDetails => {
                if self
                    .pending_key_at
                    .is_some_and(|t| t.elapsed() >= crate::keymap::KEY_CHORD_WINDOW)
                {
                    self.pending_keys.clear();
                    self.pending_key_at = None;
                }
                crate::input_menus::flush_menu_hover(self, Instant::now());
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
                if !self.dispatch_git_action(&other, &mut effects) {
                    let _ = self.dispatch_stage_action(&other, &mut effects);
                }
            }
        }

        self.reconcile_log_requests(Instant::now(), &mut effects);
        self.reconcile_stage_requests(&mut effects);
        self.stash.reconcile_requests(
            self.repo_snapshot.as_ref(),
            self.active_tab == WorkspaceTab::Stash,
            &mut effects,
        );
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

        // Commit details (debounced), or explicit compare override.
        let desired = if let Some(cmp) = &self.details.compare_override {
            Some(cmp.clone())
        } else {
            let oids = self.log_graph.selected_oids(snap.as_ref());
            if self.details.current_key.as_ref().map(|k| &k.oids) != Some(&oids) {
                self.details.merge_parent = 0;
            }
            let parent_index = if oids.len() == 1 {
                self.details.merge_parent
            } else {
                0
            };
            (!oids.is_empty()).then_some(DetailsKey::commits(oids, parent_index))
        };
        if desired != self.details.current_key {
            self.details.current_key = desired.clone();
            self.details.changes_selected = 0;
            self.details.details_scroll = 0;
            self.details.diff_scroll = 0;
            self.details.pending = match desired {
                Some(k) if self.details.cache.get(&k).is_some() => None,
                Some(k) if self.details.compare_override.is_some() => {
                    effects.push(Effect::RequestCommitDetails(k));
                    None
                }
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
            let names = jbgit_core::graph::ReachableNodes::containing_branches(
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
                        .load_settings(snapshot.common_dir.join("jbgit-branches-panel"));
                    self.stage_workbench
                        .load_history(snapshot.common_dir.join("jbgit-commit-history"));
                    self.stage_workbench
                        .load_draft(snapshot.common_dir.join("jbgit-commit-draft"));
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
                if self.stash.requested_details.as_ref() == Some(&bundle.key) {
                    self.stash.requested_details = None;
                }
                if self.stash.details.current_key.as_ref() == Some(&bundle.key) {
                    self.stash
                        .details
                        .cache
                        .insert(bundle.key.clone(), Arc::clone(&bundle));
                }
                self.details.cache.insert(bundle.key.clone(), bundle);
            }
            EngineEvent::FileDiffLoaded { diff, epoch } => {
                self.latest_file_diff_epoch = self.latest_file_diff_epoch.max(epoch);
                if self.details.requested_diff.as_ref() == Some(&diff.key) {
                    self.details.requested_diff = None;
                }
                if self.stash.details.requested_diff.as_ref() == Some(&diff.key) {
                    self.stash.details.requested_diff = None;
                }
                let hl = (self.config.jbgit.diff.syntax_highlight
                    && !diff.is_binary
                    && !diff.hunks.is_empty())
                    .then(|| Arc::new(HighlightedDiff::build(&diff)));
                if self.stash.details.desired_diff_key().as_ref() == Some(&diff.key) {
                    if let Some(h) = &hl {
                        self.stash
                            .details
                            .diff_syntax
                            .insert(diff.key.clone(), Arc::clone(h));
                    }
                    self.stash
                        .details
                        .diff_cache
                        .insert(diff.key.clone(), Arc::clone(&diff));
                }
                if let Some(h) = hl {
                    self.details.diff_syntax.insert(diff.key.clone(), h);
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
            EngineEvent::ConsoleOutput { id, stream, line } => {
                if let Some(entry) = self.console_entries.iter_mut().find(|e| e.id == id) {
                    entry.push_line(stream, line);
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
                let path_s = lock_path.display().to_string();
                self.modal_stack.push(ModalLayer::CriticalDialog {
                    title: self.i18n.t("dialog.index_lock.title").to_string(),
                    body: self.i18n.t_fmt(
                        "dialog.index_lock.body",
                        &[("path", &path_s), ("message", &message)],
                    ),
                    confirm_label: self.i18n.t("common.retry").to_string(),
                    cancel_label: self.i18n.t("common.dismiss").to_string(),
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
            EngineEvent::GitOpFinished(outcome) => {
                self.git_ops_running = self.git_ops_running.saturating_sub(1);
                self.apply_git_outcome(outcome, &mut effects);
            }
            EngineEvent::ConflictDocumentLoaded { doc } => {
                if let Some(cm) = &mut self.conflict_merge
                    && cm.path == doc.path
                {
                    cm.selected_block = 0;
                    cm.scroll = 0;
                    cm.doc = Some(doc);
                }
            }
            EngineEvent::Error(msg) => {
                self.loading_snapshot = false;
                self.details.requested_diff = None;
                self.stage_workbench.requested_diff = None;
                self.stage_workbench.commit_in_progress = false;
                self.status_notice = Some(self.i18n.t_fmt("toast.engine_error", &[("msg", &msg)]));
            }
        }

        self.reconcile_log_requests(Instant::now(), &mut effects);
        self.reconcile_stage_requests(&mut effects);
        self.stash.reconcile_requests(
            self.repo_snapshot.as_ref(),
            self.active_tab == WorkspaceTab::Stash,
            &mut effects,
        );
        effects
    }
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
            Effect::OpenExternalEditorMessage
            | Effect::OpenExternalEditorFile(_)
            | Effect::OpenExternalMergetool(_) => {
                // Handled in main loop where TerminalGuard can pause raw mode
            }
            Effect::LoadConflictDocument(path) => {
                handle.send(TaskRequest::LoadConflictDocument { path });
            }
            Effect::SaveConflictResolution {
                path,
                content,
                stage,
            } => {
                handle.send(TaskRequest::SaveConflictResolution {
                    path,
                    content,
                    stage,
                });
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
            Effect::GitOp(op) => {
                state.git_ops_running += 1;
                handle.send(TaskRequest::RunGitOp { op });
            }
            Effect::CopyToClipboard(_) | Effect::Quit => {}
        }
    }
}
