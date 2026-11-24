use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use jbgit_core::{
    BlameBundle, BlameKey, CherryPickedKey, ConflictDocument, ConflictSideStatus, DetailsKey,
    EpochToken, FileDiff, FileDiffKey, FileHistoryKey, GitOp, Oid, OutgoingKey, PaneId,
    PatchCheckKey, ReflogKey, RefreshScope, RepoSnapshot, RequestSlotId, SplitAxis, SplitterId,
    TreeBlob, TreeBlobKey, TreeKey,
};
use jbgit_engine::{StageDiffKey, ThreeDocStageDiff};
use ratatui::layout::Rect;

use crate::graph::state::LogGraphRebuilt;
use crate::hit_test::HitTarget;
use crate::log_view::{DiffPreviewDock, FilterField};
use crate::syntax::{HighlightedDiff, HighlightedFile};
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

/// Identifies each bounded scrollable viewport rendered in a frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrollPane {
    ConsoleOutput,
    LogDiff,
    LogDetails,
    StashDiff,
    StageDiff,
    FileHistoryDiff,
    Annotate,
    AnnotateDiff,
    PushCommits,
    PushFiles,
    PushDiff,
    ConflictMerge,
    ConflictQueue,
    TreeBrowserList,
    TreeBrowserPreview,
    TreeBrowserSearch,
    ApplyPatch,
    ApplyPatchPreview,
    Worktrees,
    RepoSwitcher,
    Remotes,
    RebaseDiff,
    CommitMessageModal,
    ReflogDiff,
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
    pub scroll_bounds: Option<&'a mut Vec<(ScrollPane, usize, usize)>>,
    pub commit_editor_viewport: Option<&'a mut Option<(usize, usize)>>,
    pub splitter_containers: Option<&'a mut Vec<(SplitterId, SplitAxis, Rect)>>,
}

impl<'a> RenderContext<'a> {
    #[cfg(test)]
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
            scroll_bounds: None,
            commit_editor_viewport: None,
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
            scroll_bounds: None,
            commit_editor_viewport: None,
            splitter_containers: None,
        }
    }

    pub fn ok(&self) -> &str {
        self.i18n.map_or("OK", |i| i.ok())
    }

    pub fn cancel(&self) -> &str {
        self.i18n.map_or("Cancel", |i| i.cancel())
    }

    /// Records the rendered scroll geometry for `pane` to be bound to its [`ScrollViewport`].
    pub fn record_scroll_bound(&mut self, pane: ScrollPane, viewport_h: usize, max_scroll: usize) {
        if let Some(bounds) = self.scroll_bounds.as_deref_mut() {
            bounds.push((pane, viewport_h, max_scroll));
        }
    }

    /// Records the rendered commit-message editor inner viewport `(width, height)`.
    pub fn record_commit_editor_viewport(&mut self, inner_w: usize, inner_h: usize) {
        if let Some(slot) = self.commit_editor_viewport.as_deref_mut() {
            *slot = Some((inner_w, inner_h));
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

    pub fn diff_whitespace_label(&self, mode: jbgit_core::WhitespaceMode) -> &str {
        self.i18n.map_or_else(
            || match mode {
                jbgit_core::WhitespaceMode::Default => "Exact",
                jbgit_core::WhitespaceMode::IgnoreChange => "-b",
                jbgit_core::WhitespaceMode::IgnoreAll => "-w",
            },
            |i| i.diff_whitespace_label(mode),
        )
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
pub enum ContextMenuEntryKind {
    Action(Vec<Action>),
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
    /// Item running arbitrary [`Action`]s.
    pub fn run(
        label: impl Into<String>,
        mnemonic: Option<char>,
        shortcut: Option<&str>,
        actions: Vec<Action>,
    ) -> Self {
        Self {
            label: label.into(),
            mnemonic,
            shortcut: shortcut.map(ToString::to_string),
            icon: None,
            disabled_reason: None,
            kind: ContextMenuEntryKind::Action(actions),
        }
    }

    /// Item built directly from a [`crate::keymap::catalog::CommandResult`].
    pub fn from_command(
        label: impl Into<String>,
        mnemonic: Option<char>,
        shortcut: Option<&str>,
        result: Result<Vec<Action>, String>,
    ) -> Self {
        match result {
            Ok(actions) => Self::run(label, mnemonic, shortcut, actions),
            Err(reason) => Self::run(label, mnemonic, shortcut, Vec::new()).enabled_if(false, reason),
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
    /// Child chooser for branch-point commits with ≥ 2 visible children.
    ChooseChild,
    /// Commit message history picker (`Ctrl+M` / `[🕘]`).
    CommitHistory,
}

/// Lightweight multi-line inline editor for customizing one conflict block's
/// Result text directly inside the three-pane conflict merger.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConflictInlineEditor {
    pub block_idx: usize,
    pub lines: Vec<String>,
    pub cursor_row: usize,
    pub cursor_col: usize,
    pub h_scroll: usize,
    pub v_scroll: usize,
}

impl ConflictInlineEditor {
    pub fn open(block_idx: usize, initial_lines: Vec<String>) -> Self {
        let lines = if initial_lines.is_empty() {
            vec![String::new()]
        } else {
            initial_lines
        };
        let cursor_col = lines[0].chars().count();
        Self {
            block_idx,
            lines,
            cursor_row: 0,
            cursor_col,
            h_scroll: 0,
            v_scroll: 0,
        }
    }

    fn byte_idx_at_char(s: &str, char_idx: usize) -> usize {
        s.char_indices()
            .nth(char_idx)
            .map_or(s.len(), |(b, _)| b)
    }

    pub fn insert_char(&mut self, ch: char) {
        if let Some(line) = self.lines.get_mut(self.cursor_row) {
            let b = Self::byte_idx_at_char(line, self.cursor_col);
            line.insert(b, ch);
            self.cursor_col += 1;
        }
    }

    pub fn insert_text(&mut self, text: &str) {
        for ch in text.chars() {
            match ch {
                '\r' => {}
                '\n' => self.insert_newline(),
                other => self.insert_char(other),
            }
        }
    }

    pub fn insert_newline(&mut self) {
        if let Some(line) = self.lines.get_mut(self.cursor_row) {
            let b = Self::byte_idx_at_char(line, self.cursor_col);
            let tail = line.split_off(b);
            self.cursor_row += 1;
            self.lines.insert(self.cursor_row, tail);
            self.cursor_col = 0;
            self.h_scroll = 0;
        }
    }

    pub fn backspace(&mut self) {
        if self.cursor_col > 0 {
            if let Some(line) = self.lines.get_mut(self.cursor_row) {
                let start = Self::byte_idx_at_char(line, self.cursor_col - 1);
                let end = Self::byte_idx_at_char(line, self.cursor_col);
                line.replace_range(start..end, "");
                self.cursor_col -= 1;
            }
        } else if self.cursor_row > 0 {
            let removed = self.lines.remove(self.cursor_row);
            self.cursor_row -= 1;
            if let Some(prev) = self.lines.get_mut(self.cursor_row) {
                self.cursor_col = prev.chars().count();
                prev.push_str(&removed);
            }
        }
    }

    pub fn delete_forward(&mut self) {
        let cur_len = self
            .lines
            .get(self.cursor_row)
            .map_or(0, |l| l.chars().count());
        if self.cursor_col < cur_len {
            if let Some(line) = self.lines.get_mut(self.cursor_row) {
                let start = Self::byte_idx_at_char(line, self.cursor_col);
                let end = Self::byte_idx_at_char(line, self.cursor_col + 1);
                line.replace_range(start..end, "");
            }
        } else if self.cursor_row + 1 < self.lines.len() {
            let next = self.lines.remove(self.cursor_row + 1);
            if let Some(line) = self.lines.get_mut(self.cursor_row) {
                line.push_str(&next);
            }
        }
    }

    pub fn move_left(&mut self) {
        if self.cursor_col > 0 {
            self.cursor_col -= 1;
        } else if self.cursor_row > 0 {
            self.cursor_row -= 1;
            self.cursor_col = self.lines[self.cursor_row].chars().count();
        }
    }

    pub fn move_right(&mut self) {
        let cur_len = self
            .lines
            .get(self.cursor_row)
            .map_or(0, |l| l.chars().count());
        if self.cursor_col < cur_len {
            self.cursor_col += 1;
        } else if self.cursor_row + 1 < self.lines.len() {
            self.cursor_row += 1;
            self.cursor_col = 0;
        }
    }

    pub fn move_up(&mut self) {
        if self.cursor_row > 0 {
            self.cursor_row -= 1;
            let len = self.lines[self.cursor_row].chars().count();
            self.cursor_col = self.cursor_col.min(len);
        }
    }

    pub fn move_down(&mut self) {
        if self.cursor_row + 1 < self.lines.len() {
            self.cursor_row += 1;
            let len = self.lines[self.cursor_row].chars().count();
            self.cursor_col = self.cursor_col.min(len);
        }
    }

    pub fn move_home(&mut self) {
        self.cursor_col = 0;
        self.h_scroll = 0;
    }

    pub fn move_end(&mut self) {
        self.cursor_col = self
            .lines
            .get(self.cursor_row)
            .map_or(0, |l| l.chars().count());
    }

    /// Keeps cursor visible inside a `(width, height)` editor viewport.
    pub fn ensure_cursor_visible(&mut self, width: usize, height: usize) {
        if height > 0 {
            if self.cursor_row < self.v_scroll {
                self.v_scroll = self.cursor_row;
            } else if self.cursor_row >= self.v_scroll + height {
                self.v_scroll = self.cursor_row + 1 - height;
            }
        }
        if width > 0 {
            let cur_w = self.cursor_display_col();
            if cur_w < self.h_scroll {
                self.h_scroll = cur_w;
            } else if cur_w >= self.h_scroll + width {
                self.h_scroll = cur_w + 1 - width;
            }
        }
    }

    pub fn cursor_display_col(&self) -> usize {
        use unicode_width::UnicodeWidthChar;
        self.lines
            .get(self.cursor_row)
            .map_or(0, |line| {
                line.chars()
                    .take(self.cursor_col)
                    .map(|c| if c == '\t' { 4 } else { c.width().unwrap_or(0) })
                    .sum()
            })
    }
}

#[derive(Debug, Clone, Default)]
pub struct ConflictMergeState {
    pub path: String,
    pub doc: Option<ConflictDocument>,
    pub selected_block: usize,
    pub scroll: super::ScrollViewport,
    /// Horizontal pan offset (in display columns) shared across the three panes.
    pub h_scroll: usize,
    /// Whether the Base (`:1`) ancestor panel is expanded for the active block.
    pub show_base: bool,
    /// Active inline multi-line Result editor for a single block (`e`).
    pub inline_editor: Option<ConflictInlineEditor>,
    /// Set when a background status refresh observes that `path` is no longer
    /// conflicted (e.g. resolved externally outside `jbgit`).
    pub externally_resolved: bool,
}

impl ConflictMergeState {
    /// Adjusts [`Self::scroll`] so the first row of [`Self::selected_block`]
    /// falls inside the visible conflict viewport.
    pub fn ensure_selected_block_visible(&mut self) {
        let Some(doc) = &self.doc else {
            return;
        };
        let Some((first_row, total_rows)) = doc.block_first_row(self.selected_block) else {
            return;
        };
        if let Some(viewport_h) = self.scroll.viewport_height() {
            self.scroll.ensure_visible(first_row, total_rows, viewport_h);
        } else {
            self.scroll.set_offset(first_row);
        }
    }
}

/// One row of the multi-file Conflict Queue modal (`ModalLayer::ConflictQueue`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConflictQueueEntry {
    pub path: String,
    pub yours_status: ConflictSideStatus,
    pub theirs_status: ConflictSideStatus,
    pub resolved: bool,
}

/// Self-contained state of the multi-file Conflict Queue modal.
#[derive(Debug, Clone, Default)]
pub struct ConflictQueueState {
    pub entries: Vec<ConflictQueueEntry>,
    pub selected: usize,
    pub multi_selected: BTreeSet<String>,
    pub scroll: super::ScrollViewport,
    pub is_reversed: bool,
    pub yours_label: String,
    pub theirs_label: String,
    /// Path opened in the child three-pane merger from this queue.
    pub active_merge_path: Option<String>,
    /// Avoids re-prompting "All conflicts resolved" once already offered.
    pub completion_prompted: bool,
}

impl ConflictQueueState {
    pub fn selected_entry(&self) -> Option<&ConflictQueueEntry> {
        self.entries.get(self.selected)
    }

    pub fn unresolved_count(&self) -> usize {
        self.entries.iter().filter(|e| !e.resolved).count()
    }

    pub fn resolved_count(&self) -> usize {
        self.entries.iter().filter(|e| e.resolved).count()
    }

    pub fn all_resolved(&self) -> bool {
        !self.entries.is_empty() && self.unresolved_count() == 0
    }

    /// Target paths for batch actions: multi-selected paths if any, else the
    /// currently highlighted row.
    pub fn target_entries(&self) -> Vec<&ConflictQueueEntry> {
        if !self.multi_selected.is_empty() {
            self.entries
                .iter()
                .filter(|e| self.multi_selected.contains(&e.path))
                .collect()
        } else {
            self.selected_entry().into_iter().collect()
        }
    }

    /// Next unresolved entry index after `after_idx` (wrapping around).
    pub fn next_unresolved_after(&self, after_idx: usize) -> Option<usize> {
        if self.entries.is_empty() {
            return None;
        }
        let n = self.entries.len();
        (1..=n)
            .map(|step| (after_idx + step) % n)
            .find(|&idx| !self.entries[idx].resolved)
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
    RangePatch {
        oid: Oid,
        path: String,
        range: (u32, u32),
    },
}

#[derive(Debug, Clone)]
pub enum SyntaxTarget {
    File {
        key: SyntaxJobKey,
        diff: Arc<FileDiff>,
    },
    Stage(Arc<ThreeDocStageDiff>),
    Blame(Arc<BlameBundle>),
    TreeBlob(Arc<TreeBlob>),
}

#[derive(Debug, Clone)]
pub struct SyntaxJob {
    pub epoch: u64,
    pub target: SyntaxTarget,
    pub cancel: Arc<AtomicBool>,
}

impl SyntaxJob {
    pub fn run(self) -> Option<UiAsyncEvent> {
        match self.target {
            SyntaxTarget::File { key, diff } => {
                let highlighted = HighlightedDiff::build_cancellable_file(&diff, &self.cancel)?;
                Some(UiAsyncEvent::SyntaxHighlighted {
                    key,
                    epoch: self.epoch,
                    highlighted: Arc::new(highlighted),
                })
            }
            SyntaxTarget::Stage(diff) => {
                let highlighted = HighlightedDiff::build_cancellable_stage(&diff, &self.cancel)?;
                Some(UiAsyncEvent::SyntaxHighlighted {
                    key: SyntaxJobKey::Stage(diff.key.clone()),
                    epoch: self.epoch,
                    highlighted: Arc::new(highlighted),
                })
            }
            SyntaxTarget::Blame(bundle) => {
                let highlighted = HighlightedFile::build_cancellable(
                    &bundle.key.path,
                    &bundle.content,
                    bundle.lines.len(),
                    &self.cancel,
                )?;
                Some(UiAsyncEvent::BlameSyntaxHighlighted {
                    key: bundle.key.clone(),
                    epoch: self.epoch,
                    highlighted: Arc::new(highlighted),
                })
            }
            SyntaxTarget::TreeBlob(blob) => {
                let jbgit_core::TreeBlobContent::Text(text) = &blob.content else {
                    return None;
                };
                let highlighted = HighlightedFile::build_cancellable(
                    &blob.key.path,
                    text,
                    text.lines().count(),
                    &self.cancel,
                )?;
                Some(UiAsyncEvent::TreeBlobSyntaxHighlighted {
                    key: blob.key.clone(),
                    epoch: self.epoch,
                    highlighted: Arc::new(highlighted),
                })
            }
        }
    }
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
    BlameSyntaxHighlighted {
        key: BlameKey,
        epoch: u64,
        highlighted: Arc<HighlightedFile>,
    },
    TreeBlobSyntaxHighlighted {
        key: TreeBlobKey,
        epoch: u64,
        highlighted: Arc<HighlightedFile>,
    },
    ContainingBranches {
        oid: Oid,
        token: EpochToken,
        names: Vec<String>,
    },
    LogGraphRebuilt(Box<LogGraphRebuilt>),
    /// Credential prompt from the `GIT_ASKPASS` / `SSH_ASKPASS` bridge.
    Askpass(jbgit_engine::askpass::AskpassRequest),
    /// `git init` invoked from the Welcome modal failed.
    InitRepoFailed(String),
    /// One progress / stderr line streamed from an active `git clone`.
    CloneProgress(String),
    /// Active `git clone` completed with either the cloned directory or an error message.
    CloneFinished(Result<std::path::PathBuf, String>),
}

#[derive(Debug, Clone)]
pub enum Effect {
    RefreshRepoAndStage,
    RefreshScope(RefreshScope),
    RequestCommitDetails {
        slot: RequestSlotId,
        key: DetailsKey,
    },
    RequestCommitSignatures(Vec<Oid>),
    RequestCherryPicked(CherryPickedKey),
    RequestReflog(ReflogKey),
    RequestFileDiff {
        slot: RequestSlotId,
        key: FileDiffKey,
    },
    RequestFileHistory(FileHistoryKey),
    RequestBlame(BlameKey),
    CheckRemoteTag {
        remote: String,
        tag: String,
    },
    RequestOutgoing(OutgoingKey),
    RequestPatchCheck(PatchCheckKey),
    RequestTree(TreeKey),
    RequestTreeBlob(TreeBlobKey),
    RequestTreeIndex(Oid),
    ExportTreeBlob {
        oid: Oid,
        path: String,
        dest: std::path::PathBuf,
    },
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
        delete: bool,
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
