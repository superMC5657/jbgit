use std::time::Instant;

use crossterm::event::KeyEvent;
use jbgit_core::{
    ConflictChoice, GitOp, MergeOptions, Oid, PaneId, PullOptions, RebaseOptions, RefreshScope,
    SplitterId, StageTarget, WorkspaceTab,
};
use jbgit_engine::CommitRequest;

use crate::dialogs::Dialog;
use crate::hit_test::{DiffHunkActionKind, HitTarget};
use crate::log_view::{AnnotateFocus, FileHistoryFocus, FilterField, PresentationToggle, PushDialogFocus};

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
    Annotate,
    Push,
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
    ToggleNoMergesFilter,
    LogSpeedSearchOpen,
    LogSpeedSearchInput(char),
    LogSpeedSearchBackspace,
    LogSpeedSearchConfirm,
    LogSpeedSearchClose,
    LogSpeedSearchStep(bool),
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
    AddLogSelectionRangeTo(usize),
    ToggleLogRow(usize),
    ToggleLogRowSelection,
    MoveLogCursorOnly(isize),
    ClearLogMultiSelection,
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
    GoToParentSmart,
    GoToChild,
    GoToChildCandidate(usize),
    GoToRevision(String),
    JumpToCommit(Oid),
    OpenCommitMessageModal,
    ScrollCommitMessageModal(isize),
    OpenReflog(String),
    CloseReflog,
    ReflogFocus(crate::log_view::ReflogFocus),
    ReflogCycleFocus(bool),
    ReflogMove(isize),
    ReflogSelect(usize),
    ReflogChangesMove(isize),
    ReflogChangesSelect(usize),
    ReflogDiffScroll(isize),
    ReflogToggleSideBySide,
    ReflogCycleContext,
    ReflogLoadMore,
    ReflogCheckoutSelected,
    ReflogNewBranchSelected,
    ReflogCherryPickSelected,
    ReflogResetSelected,
    ReflogJumpInLog,
    ReflogCopyHash,
    NavLogBack,
    NavLogForward,
    /// Folds one linear segment (cursor moves to its up end when inside it).
    GraphCollapseSegment(jbgit_core::graph::SegmentId),
    GraphExpandSegments(Vec<jbgit_core::graph::SegmentId>),
    GraphExpandBekMerge(Oid),
    GraphCollapseAll,
    GraphExpandAll,
    /// Click on a fold-marker node: expands the collapsed branches below it.
    GraphExpandBelow(usize),
    /// Click on a graph edge: expands it when it is a collapsed segment edge.
    GraphExpandEdge {
        up_row: usize,
        down_row: usize,
    },

    // ── Shared Changes Tree & Diff Viewer (Log / Stash / File History / Stage) ──
    ChangesMove(ChangesHost, isize),
    ChangesSelect(ChangesHost, usize),
    ChangesExpand(ChangesHost, bool),
    ChangesToggleGroupByDir(ChangesHost),
    ChangesExpandAll(ChangesHost, bool),
    ConfirmLoadManyChanges,
    SwapCompareDirection,
    SwitchMergeParent,
    ToggleDiffPreview,
    DiffCycleDock,
    ZoomDiff(bool),
    DiffNextFile(DiffScope, isize),
    DiffJumpHunk(DiffScope, isize),
    DiffToggleSideBySide(DiffScope),
    DiffCycleContext(DiffScope),
    DiffCycleWhitespace(DiffScope),
    DiffToggleSoftWrap(DiffScope),
    DiffScrollHoriz(DiffScope, isize),
    DiffSearchOpen(DiffScope),
    DiffSearchChar(DiffScope, char),
    DiffSearchBackspace(DiffScope),
    DiffSearchClear(DiffScope),
    DiffSearchConfirm(DiffScope),
    DiffSearchClose(DiffScope),
    DiffSearchJump(DiffScope, isize),
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
    CommitEditorReformat,
    CommitToggleAmend,
    CommitToggleSignoff,
    CommitToggleRunHooks,
    CommitToggleAllowEmpty,
    CommitToggleAuthorInput,
    CommitFocusAuthorInput,
    CommitAuthorChar(char),
    CommitAuthorBackspace,
    CommitAuthorCycleCompletion(isize),
    CommitAuthorAcceptCompletion,
    CommitAuthorDone,
    CommitApplyHistory(usize),
    SubmitCommit {
        and_push: bool,
    },
    SubmitCommitStageAllTracked {
        and_push: bool,
    },
    SaveGitIdentityAndCommit {
        name: String,
        email: String,
        global: bool,
        pending_commit: Option<CommitRequest>,
    },
    UndoLastCommit,
    UndoLastCommitConfirmed,
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
    /// `origin/x` → checkout/reset local branch and rebase onto current (`GitCheckoutAndRebaseRemoteBranchWorkflow`).
    CheckoutAndRebaseRemoteBranch {
        remote_ref: String,
    },
    CheckoutRevisionExpr(String),
    CompareWithCurrent {
        branch: String,
    },
    /// Prompt picker for `Compare with…` starting from `left`.
    PromptCompareWithBranch {
        left: String,
    },
    /// Symmetric comparison between any two branches `left` and `right`.
    CompareBranches {
        left: String,
        right: String,
    },
    /// Cycle `A..B` → `B..A` → `A...B` on the active branch compare.
    CycleBranchCompareDirection,
    /// Toggle `git diff A..B` snapshot tree diff for the active branch compare.
    ToggleBranchCompareTreeDiff,
    /// Exit active branch compare mode.
    CloseBranchCompare,
    /// Prompt upstream remote branch picker for local `branch`.
    PromptSetUpstream {
        branch: String,
    },
    /// Open `ModalLayer::BranchCleanup` (`Cleanup Branches…`).
    OpenBranchCleanup,
    BranchCleanupMove(isize),
    BranchCleanupSelect(usize),
    BranchCleanupToggle,
    BranchCleanupToggleAll,
    BranchCleanupCycleBase,
    BranchCleanupConfirm,
    CompareVersions {
        base: Oid,
        target: Oid,
    },
    CompareWithLocal {
        target: Oid,
    },
    CompareDirWithLocal {
        target: Oid,
        path_prefix: String,
    },
    OpenConflictResolver(String),
    OpenConflictQueue,
    CloseConflictQueue,
    OpenExternalMergetool(String),
    ConflictSelectBlock(isize),
    ConflictSetBlock(usize),
    ConflictApplyChoice(ConflictChoice),
    ConflictAcceptAll(ConflictChoice),
    ConflictApplyNonConflicting,
    ConflictToggleBase,
    ConflictHorizontalScroll(isize),
    ConflictStartInlineEdit,
    ConflictInlineEditKey(KeyEvent),
    ConflictScroll(isize),
    ConflictSaveAndStage,
    ConflictQueueMove(isize),
    ConflictQueueSelect(usize),
    ConflictQueueToggleSelect,
    ConflictQueueToggleSelectAll,
    ConflictQueueMergeSelected,
    ConflictQueueAcceptSide(bool),
    ConflictQueueRevertResolved,
    ConflictQueueMergetool,
    PromptDeleteBranch {
        name: String,
    },
    PromptDeleteTag {
        name: String,
    },
    PromptPush {
        branch: String,
        up_to: Option<Oid>,
    },
    /// `Merge…` options dialog for merging `rev` into the current branch.
    PromptMerge {
        rev: String,
    },
    /// Persist the Merge dialog choices (`<common_dir>/jbgit-merge-options`).
    RememberMergeOptions(MergeOptions),
    /// `Pull…` options dialog (`GitPullDialog`).
    PromptPull,
    /// Persist the Pull dialog choices (`<common_dir>/jbgit-pull-options`).
    RememberPullOptions(PullOptions),
    /// Trigger fetch from inside the open `Pull…` dialog to refresh remote branches.
    PullDialogFetch,
    /// `Rebase…` standard options dialog (`GitRebaseDialog`).
    PromptRebase {
        onto: Option<String>,
    },
    /// Persist the Rebase dialog choices (`<common_dir>/jbgit-rebase-options`).
    RememberRebaseOptions(RebaseOptions),
    /// `Update Project…` mode chooser.
    PromptUpdateProject,
    /// `Unshallow Repository…` confirmation dialog.
    PromptUnshallow,
    /// `Configure Remotes…` modal (`GitConfigureRemotesDialog`).
    OpenRemotes,
    /// Move the Remotes modal selection by a delta.
    RemotesMove(isize),
    /// Select a row of the Remotes modal (`usize::MAX` = last).
    RemotesSelect(usize),
    /// Repository switcher (`<A-o>`, header repository name).
    OpenRepoSwitcher,
    /// Query / selection of the repository switcher.
    RepoSwitcherSetState {
        query: String,
        selected: usize,
    },
    /// Switch the session to the repository containing this path (guarded:
    /// running operations / open flows ask first).
    SwitchRepository(std::path::PathBuf),
    /// [`Action::SwitchRepository`] after the user confirmed the guard.
    SwitchRepositoryConfirmed(std::path::PathBuf),
    /// Remove a path from the recent repositories list.
    ForgetRecentRepo(std::path::PathBuf),
    /// Open the Welcome modal for a non-Git directory.
    OpenWelcome(std::path::PathBuf),
    /// Move selection in the Welcome modal.
    WelcomeSelect(usize),
    /// Initialize a Git repository in the specified directory and bind to it.
    WelcomeInitRepo(std::path::PathBuf),
    /// Open the `Clone Repository…` modal (`base_dir` overrides the default parent).
    OpenCloneRepo {
        base_dir: Option<std::path::PathBuf>,
    },
    /// Mutate the active `Clone Repository…` modal form.
    CloneRepoUpdate(crate::repo_lifecycle::CloneDialogAction),
    /// Validate and launch `git clone` from the active `Clone Repository…` modal.
    CloneRepoSubmit,
    /// Resolve a pre-commit CRLF warning dialog (`Fix & Commit` / `Commit As Is` / `Don't warn again`).
    CommitCrlfResolve {
        request: CommitRequest,
        fix_autocrlf: bool,
        disable_warning: bool,
    },
    /// `Working Trees` modal.
    OpenWorktrees,
    /// Move the Worktrees modal selection by a delta.
    WorktreesMove(isize),
    /// Select a row of the Worktrees modal (`usize::MAX` = last).
    WorktreesSelect(usize),
    /// Open the selected working tree (double-click).
    WorktreesOpenSelected,
    /// "New Worktree…" chooser (`None` = from HEAD, `Some` = Log commit).
    PromptNewWorktree { at: Option<jbgit_core::Oid> },
    /// Show only `oids` in the Log (read-only Range chip, `GitUpdateInfoAsLog`).
    FilterLogByRange {
        label: String,
        oids: Vec<Oid>,
    },
    ClearRangeFilter,
    UncommitHead,
    OngoingResolve,
    NotificationActivate,
    NotificationDismiss,
    StashSelect(usize),
    StashMove(isize),

    // ── Log changes operations & single-file history ──
    FilterLogByPath(String),
    /// Enters Directory History for `path`, saving the current `LogFilterSpec`
    /// snapshot so the user can restore their prior Log filter losslessly.
    ShowDirectoryHistory(String),
    /// Restores the `LogFilterSpec` snapshot captured prior to `ShowDirectoryHistory`.
    RestorePreDirHistoryFilter,
    CompareFileWithLocal {
        target: Oid,
        path: String,
    },
    OpenFileHistory {
        path: String,
        start_oid: Option<Oid>,
        follow_renames: bool,
        line_range: Option<(u32, u32)>,
    },
    OpenSelectionHistory {
        path: String,
        start_oid: Option<Oid>,
        start_line: u32,
        end_line: u32,
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
    FileHistoryAnnotate,
    FileHistoryCycleBranchScope,
    FileHistorySetBranchScope(jbgit_core::HistoryBranchScope),
    FileHistoryLoadMore,
    FileHistorySearchOpen,
    FileHistorySearchChar(char),
    FileHistorySearchBackspace,
    FileHistorySearchClear,
    FileHistorySearchConfirm,
    FileHistorySearchClose,
    FileHistoryToggleMarkAnchor,
    FileHistoryToggleCommitDetails,
    FileHistoryToggleAffectedFiles,
    FileHistoryAffectedMove(isize),
    FileHistoryAffectedSelect(usize),
    FileHistoryOpenRepoVersion,
    FileHistoryCloseRepoVersion,
    FileHistoryBlobScroll(isize),
    FileHistoryGetVersion,
    FileHistoryCopyPatch,
    FileHistoryCopyHash,
    FileHistoryEscape,

    // ── Apply Patch / Create Patch / Browse Repository at Revision ──
    /// Opens the Apply Patch dialog and runs its preflight.
    OpenApplyPatch(jbgit_core::PatchInput),
    ApplyPatch(crate::patch_view::ApplyPatchAction),
    /// Remembers the user's last chosen Create Patch output directory.
    RememberPatchDir(std::path::PathBuf),
    /// Exports raw blob bytes at `oid:path` to `dest`.
    ExportTreeBlob {
        oid: Oid,
        path: String,
        dest: std::path::PathBuf,
    },
    OpenTreeBrowser {
        oid: Oid,
    },
    /// Resolves a revision expression (Go to Revision popup) and browses it.
    BrowseRevisionExpr(String),
    TreeBrowser(crate::tree_browser::TreeBrowserAction),

    // ── Annotate / Blame modal ──
    OpenAnnotate {
        path: String,
        rev: Option<Oid>,
    },
    AnnotateMove(isize),
    AnnotateMoveExtend(isize),
    AnnotateSelectLine {
        line_idx: usize,
        extend: bool,
    },
    AnnotateScroll(isize),
    AnnotateToggleVisual,
    AnnotateToggleWhitespace,
    AnnotateToggleMoves,
    AnnotateToggleDateMode,
    AnnotateCycleColorMode,
    AnnotateToggleHashColumn,
    AnnotateToggleAuthorColumn,
    AnnotateToggleDateColumn,
    AnnotateToggleCommitDetails,
    AnnotateToggleDiff,
    AnnotateToggleFocus,
    AnnotateSetFocus(AnnotateFocus),
    AnnotatePreviousRev,
    AnnotateThisRev,
    AnnotateNavBack,
    AnnotateJumpToLog,
    AnnotateShowSelectionHistory,
    AnnotateCopyHash,
    AnnotateGotoLineStart,
    AnnotateGotoLineChar(char),
    AnnotateGotoLineBackspace,
    AnnotateGotoLineConfirm,
    AnnotateGotoLineCancel,
    AnnotateSetPendingG(Option<Instant>),
    AnnotateEscape,

    // ── Push modal ──
    PushFocusNext,
    PushFocusPrev,
    PushSetFocus(PushDialogFocus),
    PushMoveCommit(isize),
    PushSelectCommit(usize),
    PushMoveFile(isize),
    PushSelectFile(usize),
    PushToggleDiff,
    PushStartEditTarget,
    PushEditTargetChar(char),
    PushEditTargetBackspace,
    PushEditTargetClear,
    PushEditTargetMoveCompletion(isize),
    PushEditTargetAcceptCompletion(Option<usize>),
    PushEditTargetConfirm,
    PushEditTargetCancel,
    PushToggleRemotePicker,
    PushMoveRemotePicker(isize),
    PushSelectRemote(usize),
    PushCycleTags,
    PushCycleForce,
    PushToggleSetUpstream,
    PushToggleNoVerify,
    PushDefineRemote,
    PushConfirm,
    PushEscape,
}
