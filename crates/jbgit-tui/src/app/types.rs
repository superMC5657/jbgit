use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use jbgit_core::{
    ConflictDocument, DetailsKey, EpochToken, FileDiff, FileDiffKey, FileHistoryKey, GitOp,
    ModalButton, Oid, PaneId, RefreshScope, RepoSnapshot, SplitAxis, SplitterId,
};
use jbgit_engine::{StageDiffKey, ThreeDocStageDiff};
use ratatui::layout::Rect;

use crate::dialogs::Dialog;
use crate::graph::state::LogGraphRebuilt;
use crate::hit_test::HitTarget;
use crate::log_view::{DiffPreviewDock, FilterField, PresentationToggle};
use crate::syntax::HighlightedDiff;
use crate::theme::{BranchColorManager, GlyphMode, ThemePalette};

use super::Action;

/// Large negative delta used by `GotoTop` / `Home` scroll actions.
pub const SCROLL_GOTO_TOP: isize = isize::MIN / 2;
/// Large positive delta used by `GotoBottom` / `End` scroll actions.
pub const SCROLL_GOTO_BOTTOM: isize = isize::MAX / 2;

/// Applies a signed scroll delta within `[0, max_scroll]` without integer
/// overflow and without overscroll dead-zone when reversing direction.
pub fn apply_scroll_delta(current: usize, delta: isize, max_scroll: usize) -> usize {
    let clamped = current.min(max_scroll);
    if delta >= 0 {
        clamped.saturating_add(delta as usize).min(max_scroll)
    } else {
        clamped.saturating_sub(delta.unsigned_abs())
    }
}

/// Identifies each bounded scrollable viewport tracked by `ViewportScrollMetrics`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrollPane {
    ConsoleOutput,
    LogDiff,
    LogDetails,
    StashDiff,
    StageDiff,
    FileHistoryDiff,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct PaneScrollBound {
    pub viewport_h: usize,
    pub max_scroll: usize,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ViewportScrollMetrics {
    pub console_output: Option<PaneScrollBound>,
    pub log_diff: Option<PaneScrollBound>,
    pub log_details: Option<PaneScrollBound>,
    pub stash_diff: Option<PaneScrollBound>,
    pub stage_diff: Option<PaneScrollBound>,
    pub file_history_diff: Option<PaneScrollBound>,
}

impl ViewportScrollMetrics {
    /// Returns the last recorded scroll bound for `pane`, if rendered.
    pub fn bound(&self, pane: ScrollPane) -> Option<PaneScrollBound> {
        match pane {
            ScrollPane::ConsoleOutput => self.console_output,
            ScrollPane::LogDiff => self.log_diff,
            ScrollPane::LogDetails => self.log_details,
            ScrollPane::StashDiff => self.stash_diff,
            ScrollPane::StageDiff => self.stage_diff,
            ScrollPane::FileHistoryDiff => self.file_history_diff,
        }
    }

    /// Records the rendered viewport height and maximum scroll offset for `pane`.
    pub fn record(&mut self, pane: ScrollPane, viewport_h: usize, max_scroll: usize) {
        let bound = Some(PaneScrollBound {
            viewport_h,
            max_scroll,
        });
        match pane {
            ScrollPane::ConsoleOutput => self.console_output = bound,
            ScrollPane::LogDiff => self.log_diff = bound,
            ScrollPane::LogDetails => self.log_details = bound,
            ScrollPane::StashDiff => self.stash_diff = bound,
            ScrollPane::StageDiff => self.stage_diff = bound,
            ScrollPane::FileHistoryDiff => self.file_history_diff = bound,
        }
    }
}

pub struct RenderContext<'a> {
    pub theme: &'a ThemePalette,
    pub glyph_mode: GlyphMode,
    pub branch_colors: &'a BranchColorManager,
    pub hit_registry: &'a mut crate::hit_test::HitTestRegistry,
    pub focused_pane: PaneId,
    pub hover_target: Option<&'a HitTarget>,
    pub dragging_splitter: Option<SplitterId>,
    /// Graph backend selector; `None` renders with a plain [`crate::graph::CellGridGraphBackend`].
    pub graph_renderer: Option<&'a mut crate::graph::GraphRenderer>,
    pub i18n: Option<&'a crate::i18n::I18n>,
    pub config: Option<&'a crate::config::AppConfig>,
    pub scroll_metrics: Option<&'a mut ViewportScrollMetrics>,
    pub splitter_containers: Option<&'a mut Vec<(SplitterId, SplitAxis, Rect)>>,
}

impl<'a> RenderContext<'a> {
    pub fn minimal(
        theme: &'a ThemePalette,
        glyph_mode: GlyphMode,
        branch_colors: &'a BranchColorManager,
        hit_registry: &'a mut crate::hit_test::HitTestRegistry,
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
            scroll_metrics: None,
            splitter_containers: None,
        }
    }

    pub fn rebind_hits<'b>(
        &'a self,
        hit_registry: &'b mut crate::hit_test::HitTestRegistry,
    ) -> RenderContext<'b>
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
            scroll_metrics: None,
            splitter_containers: None,
        }
    }

    pub fn ok(&self) -> &str {
        self.i18n.map_or("OK", |i| i.ok())
    }

    pub fn cancel(&self) -> &str {
        self.i18n.map_or("Cancel", |i| i.cancel())
    }

    /// Records the rendered scroll geometry for `pane` into this frame's metrics.
    pub fn record_scroll_bound(&mut self, pane: ScrollPane, viewport_h: usize, max_scroll: usize) {
        if let Some(m) = self.scroll_metrics.as_deref_mut() {
            m.record(pane, viewport_h, max_scroll);
        }
    }

    /// Records the container rect of a rendered splitter for mouse drag calculations.
    pub fn record_splitter_container(&mut self, id: SplitterId, axis: SplitAxis, rect: Rect) {
        if let Some(containers) = self.splitter_containers.as_deref_mut() {
            containers.push((id, axis, rect));
        }
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
    /// Arbitrary follow-up actions (Git workflows).
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

pub(crate) fn initial_menu_selection(items: &[ContextMenuItem]) -> usize {
    items
        .iter()
        .position(ContextMenuItem::is_enabled)
        .or_else(|| items.iter().position(|i| !i.is_separator()))
        .unwrap_or(0)
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
    /// Single-file commit history inspector (`--follow` rename tracking + diff).
    FileHistory,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyntaxJobKey {
    File(FileDiffKey),
    Stage(StageDiffKey),
}

#[derive(Debug, Clone)]
pub enum SyntaxTarget {
    File(Arc<FileDiff>),
    Stage(Arc<ThreeDocStageDiff>),
}

#[derive(Debug, Clone)]
pub struct SyntaxJob {
    pub key: SyntaxJobKey,
    pub epoch: u64,
    pub target: SyntaxTarget,
    pub cancel: Arc<AtomicBool>,
}

/// Off-UI-thread `ReachableNodes::containing_branches` walk for one commit.
#[derive(Debug, Clone)]
pub struct ContainingBranchesJob {
    pub oid: Oid,
    pub node: jbgit_core::graph::PermNodeId,
    pub token: EpochToken,
    pub graph: Arc<jbgit_core::graph::PermanentGraph>,
    pub snapshot: Arc<RepoSnapshot>,
}

impl ContainingBranchesJob {
    pub fn run(&self) -> Option<Vec<String>> {
        if self.token.is_stale() {
            return None;
        }
        Some(
            jbgit_core::graph::ReachableNodes::containing_branches(
                &self.graph.linear,
                self.node,
                &self.snapshot.refs,
            )
            .into_iter()
            .map(|r| r.name.clone())
            .collect(),
        )
    }
}

#[derive(Debug)]
pub enum UiAsyncEvent {
    SyntaxHighlighted {
        key: SyntaxJobKey,
        epoch: u64,
        highlighted: Arc<HighlightedDiff>,
    },
    ContainingBranches {
        oid: Oid,
        token: EpochToken,
        names: Vec<String>,
    },
    LogGraphRebuilt(Box<LogGraphRebuilt>),
}

#[derive(Debug, Clone)]
pub enum Effect {
    RefreshRepoAndStage,
    RefreshScope(RefreshScope),
    RequestCommitDetails(DetailsKey),
    RequestFileDiff(FileDiffKey),
    RequestFileHistory(FileHistoryKey),
    RequestPathMatches(Vec<String>, Arc<RepoSnapshot>),
    RequestStageFileDiff(StageDiffKey),
    RequestSyntaxHighlight(SyntaxJob),
    ComputeContainingBranches(ContainingBranchesJob),
    RebuildLogGraph(Box<crate::graph::state::LogGraphJob>),
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
    StageTargets(Vec<jbgit_core::StageTarget>),
    StageAllTracked,
    UnstageTargets(Vec<jbgit_core::StageTarget>),
    DiscardTargets(Vec<jbgit_core::StageTarget>),
    ExecuteCommit(jbgit_engine::CommitRequest),
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
    /// High-level Git write operation.
    GitOp(GitOp),
    Quit,
}

#[derive(Debug)]
pub struct SyntaxJobSlot<K> {
    pub epoch: u64,
    pub cancel: Option<Arc<AtomicBool>>,
    pub in_flight: Option<K>,
}

impl<K> Default for SyntaxJobSlot<K> {
    fn default() -> Self {
        Self {
            epoch: 0,
            cancel: None,
            in_flight: None,
        }
    }
}

impl<K: PartialEq + Clone> SyntaxJobSlot<K> {
    pub fn invalidate(&mut self) {
        if let Some(prev) = self.cancel.take() {
            prev.store(true, Ordering::Relaxed);
        }
        self.epoch = self.epoch.wrapping_add(1);
        self.in_flight = None;
    }

    pub fn begin(&mut self, key: &K) -> Option<(u64, Arc<AtomicBool>)> {
        if self.in_flight.as_ref() == Some(key) {
            return None;
        }
        self.invalidate();
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancel = Some(Arc::clone(&cancel));
        self.in_flight = Some(key.clone());
        Some((self.epoch, cancel))
    }

    pub fn finish(&mut self, key: &K, epoch: u64) -> bool {
        if self.in_flight.as_ref() == Some(key) && epoch == self.epoch {
            self.in_flight = None;
        }
        epoch >= self.epoch
    }
}
