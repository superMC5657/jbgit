use std::time::Instant;

use crossterm::event::KeyEvent;
use jbgit_core::{
    ConflictChoice, GitOp, Oid, PaneId, RefreshScope, SplitterId, StageTarget, WorkspaceTab,
};
use jbgit_engine::CommitRequest;

use crate::dialogs::Dialog;
use crate::hit_test::{DiffHunkActionKind, HitTarget};
use crate::log_view::{FileHistoryFocus, FilterField, PresentationToggle};

use super::{ContextMenuItem, DragState, EditorMoveKind, MenuParent, PopupKind};

/// Target host for shared changed-files tree actions (`[2 Log]` or `[3 Stash]`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangesHost {
    Log,
    Stash,
}

/// Target host for shared diff viewer actions across all four workbenches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffScope {
    Log,
    Stash,
    FileHistory,
    Stage,
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
    /// Pre-selects `target` (when needed) and opens its context menu at `anchor`.
    OpenContextMenuForTarget {
        target: HitTarget,
        anchor: (u16, u16),
    },
    PopModal,
    PopContextMenus,
    DiscardTargetsConfirmed(Vec<StageTarget>),
    ExecuteCommitConfirmed(CommitRequest),
    RetryCliCommand {
        args: Vec<String>,
        stdin_payload: Option<Vec<u8>>,
        is_read_only: bool,
        refresh_after: RefreshScope,
    },
    ResetSplitter(SplitterId),

    // ── Pure input-routing state transitions ──
    SetPendingKeySequence(Vec<KeyEvent>),
    ClearPendingKeySequence,
    ModalPaste(String),
    DialogKey(KeyEvent),
    DialogSelectOption {
        idx: usize,
        confirm: bool,
    },
    DialogToggleCheckbox,
    PopupSetState {
        query: String,
        selected: usize,
    },
    ContextMenuSetSelected {
        depth: u8,
        idx: usize,
    },
    ContextMenuActivateItem {
        depth: u8,
        idx: usize,
    },
    MouseMove {
        col: u16,
        row: u16,
    },
    MouseRecordClick {
        col: u16,
        row: u16,
    },
    SetDragState(Option<DragState>),
    DragSplitter {
        col: u16,
        row: u16,
    },
    StageDiffSelectLine {
        row_idx: usize,
        extend: bool,
    },
    StageDiffClearVisualAnchor,
    StageClearBracketJump,
    CommitEditorFocusBody,
    FileHistorySetPendingG(Option<Instant>),

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

    // ── Shared Changes Tree & Diff Viewer (Log / Stash / File History / Stage) ──
    ChangesMove(ChangesHost, isize),
    ChangesSelect(ChangesHost, usize),
    ChangesExpand(ChangesHost, bool),
    ChangesToggleGroupByDir(ChangesHost),
    ChangesExpandAll(ChangesHost, bool),
    SwitchMergeParent,
    ToggleDiffPreview,
    DiffCycleDock,
    ZoomDiff(bool),
    DiffNextFile(DiffScope, isize),
    DiffJumpHunk(DiffScope, isize),
    DiffToggleSideBySide(DiffScope),
    DiffCycleContext(DiffScope),
    DiffScroll(DiffScope, isize),
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
    StageDiffToggleVisual,
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

    // ── Branches popup, menus, dialogs & Git workflows ──
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

    // ── Log changes operations & single-file history ──
    FilterLogByPath(String),
    CompareFileWithLocal {
        target: Oid,
        path: String,
    },
    OpenFileHistory {
        path: String,
        start_oid: Option<Oid>,
        follow_renames: bool,
    },
    FileHistoryMove {
        delta: isize,
        extend: bool,
    },
    FileHistorySelect {
        idx: usize,
        extend: bool,
    },
    FileHistoryToggleFollow,
    FileHistoryToggleCompareLocal,
    FileHistoryToggleZoom,
    FileHistorySetFocus(FileHistoryFocus),
    FileHistoryToggleFocus,
    FileHistoryJumpToLog,
    FileHistoryEscape,
}
