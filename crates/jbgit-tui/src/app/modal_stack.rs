//! Self-contained modal stack encapsulating all overlay layers (floating popups,
//! file history, conflict merge resolver, three-way hunk inspector, hook error,
//! cascading context menus, and dialogs) so push/pop automatically owns and
//! cleans up every modal's state.

use std::time::Instant;

use ratatui::layout::Rect;

use crate::dialogs::Dialog;
use crate::log_view::{AnnotateState, FileHistoryState, FilterField, PushDialogState, ReflogState};
use crate::views::overlays::{context_menu_rect, menu_item_row_y};

use super::{
    Action, ConflictMergeState, ConflictQueueState, ContextMenuEntryKind, ContextMenuItem,
    MENU_HOVER_DELAY, MenuHover, MenuParent, PopupKind, ScrollViewport,
};

#[derive(Debug)]
pub enum ModalLayer {
    /// Layer 2: global Branches Quick-View & fuzzy speed-search popup.
    BranchesPopup { query: String, selected: usize },
    /// Layer 2: searchable & executable command/shortcut palette (`?`).
    ShortcutsHelp { query: String, selected: usize },
    /// Layer 2: Darcula / theme palette & glyph debug preview.
    PalettePreview,
    /// Layer 2: inline filter-bar dimension editor (`Branch` / `User` / `Date` / `Paths`).
    FilterEditor {
        field: FilterField,
        query: String,
        selected: usize,
        /// Whether the user explicitly navigated suggestions via `Up`/`Down` or scroll,
        /// so `Enter` applies the highlighted candidate instead of treating empty input as `All`.
        selection_touched: bool,
    },
    /// Layer 2: `Ctrl+G` Go to Hash / Branch / Tag / `HEAD~N`.
    GoToRevision { query: String },
    /// Layer 2: parent chooser for merge commits with ≥ 2 parents.
    ChooseParent { selected: usize },
    /// Layer 2: child chooser for branch-point commits with ≥ 2 visible children.
    ChooseChild { selected: usize },
    /// Layer 2: maximized scrollable full commit message modal.
    CommitMessage {
        oid: jbgit_core::Oid,
        subject: String,
        message: String,
        scroll: ScrollViewport,
    },
    /// Layer 2: Git Reflog browser & recovery modal (`git reflog show` + synced diff).
    Reflog(Box<ReflogState>),
    /// Layer 2: commit message history picker (`Ctrl+M` / `[🕘]`).
    CommitHistory { query: String, selected: usize },
    /// Layer 2: self-contained single-file commit history modal (`--follow` + diff).
    FileHistory(Box<FileHistoryState>),
    /// Layer 2: self-contained per-line git blame / annotate viewer.
    Annotate(Box<AnnotateState>),
    /// Layer 2: full Push dialog (`target`, outgoing commits, files/diff preview, options).
    PushDialog(Box<PushDialogState>),
    /// Layer 2: `Configure Remotes…` list (add / rename / edit URL / remove / fetch).
    Remotes(crate::remotes_view::RemotesState),
    /// Layer 2: `Cleanup Branches…` batch merged/gone branch deleter.
    BranchCleanup(Box<crate::branch_cleanup::BranchCleanupState>),
    /// Layer 2: repository switcher (recent repositories, worktrees, submodules).
    RepoSwitcher(Box<crate::repo_switcher::RepoSwitcherState>),
    /// Layer 2: startup welcome modal for non-Git directories (`Init` / `Clone…` / `Open Recent`).
    Welcome(crate::repo_lifecycle::WelcomeState),
    /// Layer 2: `Clone Repository…` form and live progress modal.
    CloneRepo(Box<crate::repo_lifecycle::CloneDialogState>),
    /// Layer 2: `git worktree` list (open / new / lock / remove / prune).
    Worktrees(crate::worktrees_view::WorktreesState),
    /// Layer 2: Apply Patch preflight (files, mode) and confirmation.
    ApplyPatch(Box<crate::patch_view::ApplyPatchState>),
    /// Layer 2: Browse Repository at Revision (lazy tree + file preview).
    TreeBrowser(Box<crate::tree_browser::TreeBrowserState>),
    /// Layer 2: self-contained three-pane conflict resolver (`Ours | Result | Theirs`).
    ConflictResolver(Box<ConflictMergeState>),
    /// Layer 2: multi-file conflict queue table (`Path | Yours | Theirs | Status`).
    ConflictQueue(Box<ConflictQueueState>),
    /// Layer 2: three-way hunk inspector (`HEAD / Staged / Worktree`).
    ThreeWayInspector { range_idx: Option<usize> },
    /// Layer 2: hook or commit failure output modal.
    HookError { output: String },
    /// Layer 3: right-click / keyboard context menu (supports cascading submenus via `depth`).
    ContextMenu {
        anchor: (u16, u16),
        items: Vec<ContextMenuItem>,
        selected: usize,
        depth: u8,
        /// Set for cascading submenus (placed beside the parent, flipping left).
        parent: Option<MenuParent>,
    },
    /// Layer 4: input / choice / confirm / message / rebase-todo dialog.
    Dialog(Box<Dialog>),
}

impl ModalLayer {
    pub fn from_popup_kind(kind: PopupKind, initial_query: String) -> Self {
        match kind {
            PopupKind::BranchesQuickView => Self::BranchesPopup {
                query: initial_query,
                selected: 0,
            },
            PopupKind::ShortcutsHelp => Self::ShortcutsHelp {
                query: initial_query,
                selected: 0,
            },
            PopupKind::PalettePreview => Self::PalettePreview,
            PopupKind::FilterEditor(field) => Self::FilterEditor {
                field,
                query: initial_query,
                selected: 0,
                selection_touched: false,
            },
            PopupKind::GoToRevision => Self::GoToRevision {
                query: initial_query,
            },
            PopupKind::ChooseParent => Self::ChooseParent { selected: 0 },
            PopupKind::ChooseChild => Self::ChooseChild { selected: 0 },
            PopupKind::CommitHistory => Self::CommitHistory {
                query: initial_query,
                selected: 0,
            },
        }
    }

    pub fn set_popup_state(&mut self, next_query: String, next_selected: usize) {
        match self {
            Self::BranchesPopup { query, selected }
            | Self::ShortcutsHelp { query, selected }
            | Self::CommitHistory { query, selected } => {
                *query = next_query;
                *selected = next_selected;
            }
            Self::FilterEditor {
                query,
                selected,
                selection_touched,
                ..
            } => {
                *query = next_query;
                *selected = next_selected;
                *selection_touched = false;
            }
            Self::GoToRevision { query } => {
                *query = next_query;
            }
            Self::ChooseParent { selected } | Self::ChooseChild { selected } => {
                *selected = next_selected;
            }
            _ => {}
        }
    }

    pub fn select_popup_candidate(&mut self, next_selected: usize) {
        match self {
            Self::BranchesPopup { selected, .. }
            | Self::ShortcutsHelp { selected, .. }
            | Self::CommitHistory { selected, .. }
            | Self::ChooseParent { selected }
            | Self::ChooseChild { selected } => {
                *selected = next_selected;
            }
            Self::FilterEditor {
                selected,
                selection_touched,
                ..
            } => {
                *selected = next_selected;
                *selection_touched = true;
            }
            _ => {}
        }
    }

    pub fn filter_editor_selection_touched(&self) -> bool {
        matches!(
            self,
            Self::FilterEditor {
                selection_touched: true,
                ..
            }
        )
    }

    pub fn popup_kind(&self) -> Option<PopupKind> {
        match self {
            Self::BranchesPopup { .. } => Some(PopupKind::BranchesQuickView),
            Self::ShortcutsHelp { .. } => Some(PopupKind::ShortcutsHelp),
            Self::PalettePreview => Some(PopupKind::PalettePreview),
            Self::FilterEditor { field, .. } => Some(PopupKind::FilterEditor(*field)),
            Self::GoToRevision { .. } => Some(PopupKind::GoToRevision),
            Self::ChooseParent { .. } => Some(PopupKind::ChooseParent),
            Self::ChooseChild { .. } => Some(PopupKind::ChooseChild),
            Self::CommitHistory { .. } => Some(PopupKind::CommitHistory),
            _ => None,
        }
    }

    pub fn popup_query_and_selected(&self) -> Option<(&str, usize)> {
        match self {
            Self::BranchesPopup { query, selected }
            | Self::ShortcutsHelp { query, selected }
            | Self::FilterEditor {
                query, selected, ..
            }
            | Self::CommitHistory { query, selected } => Some((query.as_str(), *selected)),
            Self::GoToRevision { query } => Some((query.as_str(), 0)),
            Self::ChooseParent { selected } | Self::ChooseChild { selected } => {
                Some(("", *selected))
            }
            Self::PalettePreview => Some(("", 0)),
            _ => None,
        }
    }

    pub const fn z_level(&self) -> u8 {
        match self {
            Self::BranchesPopup { .. }
            | Self::ShortcutsHelp { .. }
            | Self::PalettePreview
            | Self::FilterEditor { .. }
            | Self::GoToRevision { .. }
            | Self::ChooseParent { .. }
            | Self::ChooseChild { .. }
            | Self::CommitMessage { .. }
            | Self::Reflog(_)
            | Self::CommitHistory { .. }
            | Self::FileHistory(_)
            | Self::Annotate(_)
            | Self::PushDialog(_)
            | Self::Remotes(_)
            | Self::BranchCleanup(_)
            | Self::RepoSwitcher(_)
            | Self::Welcome(_)
            | Self::CloneRepo(_)
            | Self::Worktrees(_)
            | Self::ApplyPatch(_)
            | Self::TreeBrowser(_)
            | Self::ConflictResolver(_)
            | Self::ConflictQueue(_)
            | Self::ThreeWayInspector { .. }
            | Self::HookError { .. } => 20,
            Self::ContextMenu { depth, .. } => 30u8.saturating_add(depth.saturating_mul(2)),
            Self::Dialog(_) => 40,
        }
    }
}

/// Ordered modal layer stack (bottom-to-top) that owns all modal-specific state.
#[derive(Debug, Default)]
pub struct ModalStack {
    layers: Vec<ModalLayer>,
}

impl ModalStack {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, layer: ModalLayer) {
        self.layers.push(layer);
    }

    pub fn pop(&mut self) -> Option<ModalLayer> {
        self.layers.pop()
    }

    pub fn last(&self) -> Option<&ModalLayer> {
        self.layers.last()
    }

    pub fn last_mut(&mut self) -> Option<&mut ModalLayer> {
        self.layers.last_mut()
    }

    pub fn get(&self, idx: usize) -> Option<&ModalLayer> {
        self.layers.get(idx)
    }

    pub fn is_empty(&self) -> bool {
        self.layers.is_empty()
    }

    pub fn len(&self) -> usize {
        self.layers.len()
    }

    pub fn truncate(&mut self, len: usize) {
        self.layers.truncate(len);
    }

    pub fn iter(&self) -> std::slice::Iter<'_, ModalLayer> {
        self.layers.iter()
    }

    pub fn iter_mut(&mut self) -> std::slice::IterMut<'_, ModalLayer> {
        self.layers.iter_mut()
    }

    pub fn retain(&mut self, f: impl FnMut(&ModalLayer) -> bool) {
        self.layers.retain(f);
    }

    pub fn dismiss_context_menus(&mut self) {
        while matches!(self.layers.last(), Some(ModalLayer::ContextMenu { .. })) {
            self.layers.pop();
        }
    }

    pub fn file_history(&self) -> Option<&FileHistoryState> {
        self.layers.iter().rev().find_map(|l| match l {
            ModalLayer::FileHistory(fh) => Some(fh.as_ref()),
            _ => None,
        })
    }

    pub fn file_history_mut(&mut self) -> Option<&mut FileHistoryState> {
        self.layers.iter_mut().rev().find_map(|l| match l {
            ModalLayer::FileHistory(fh) => Some(fh.as_mut()),
            _ => None,
        })
    }

    pub fn open_file_history(&mut self, fh: FileHistoryState) {
        if let Some(ModalLayer::FileHistory(existing)) = self.layers.last_mut() {
            **existing = fh;
        } else {
            self.layers.push(ModalLayer::FileHistory(Box::new(fh)));
        }
    }

    pub fn close_file_history(&mut self) {
        if let Some(pos) = self
            .layers
            .iter()
            .rposition(|l| matches!(l, ModalLayer::FileHistory(_)))
        {
            self.layers.remove(pos);
        }
    }

    pub fn reflog(&self) -> Option<&ReflogState> {
        self.layers.iter().rev().find_map(|l| match l {
            ModalLayer::Reflog(r) => Some(r.as_ref()),
            _ => None,
        })
    }

    pub fn reflog_mut(&mut self) -> Option<&mut ReflogState> {
        self.layers.iter_mut().rev().find_map(|l| match l {
            ModalLayer::Reflog(r) => Some(r.as_mut()),
            _ => None,
        })
    }

    pub fn open_reflog(&mut self, state: ReflogState) {
        if let Some(ModalLayer::Reflog(existing)) = self.layers.last_mut() {
            **existing = state;
        } else {
            self.layers.push(ModalLayer::Reflog(Box::new(state)));
        }
    }

    pub fn close_reflog(&mut self) {
        if let Some(pos) = self
            .layers
            .iter()
            .rposition(|l| matches!(l, ModalLayer::Reflog(_)))
        {
            self.layers.remove(pos);
        }
    }

    pub fn commit_message_scroll_mut(&mut self) -> Option<&mut ScrollViewport> {
        self.layers.iter_mut().rev().find_map(|l| match l {
            ModalLayer::CommitMessage { scroll, .. } => Some(scroll),
            _ => None,
        })
    }

    pub fn annotate(&self) -> Option<&AnnotateState> {
        self.layers.iter().rev().find_map(|l| match l {
            ModalLayer::Annotate(ann) => Some(ann.as_ref()),
            _ => None,
        })
    }

    pub fn annotate_mut(&mut self) -> Option<&mut AnnotateState> {
        self.layers.iter_mut().rev().find_map(|l| match l {
            ModalLayer::Annotate(ann) => Some(ann.as_mut()),
            _ => None,
        })
    }

    pub fn open_annotate(&mut self, ann: AnnotateState) {
        if let Some(ModalLayer::Annotate(existing)) = self.layers.last_mut() {
            **existing = ann;
        } else {
            self.layers.push(ModalLayer::Annotate(Box::new(ann)));
        }
    }

    pub fn close_annotate(&mut self) {
        if let Some(pos) = self
            .layers
            .iter()
            .rposition(|l| matches!(l, ModalLayer::Annotate(_)))
        {
            self.layers.remove(pos);
        }
    }

    pub fn push_dialog(&self) -> Option<&PushDialogState> {
        self.layers.iter().rev().find_map(|l| match l {
            ModalLayer::PushDialog(pd) => Some(pd.as_ref()),
            _ => None,
        })
    }

    pub fn apply_patch(&self) -> Option<&crate::patch_view::ApplyPatchState> {
        self.layers.iter().rev().find_map(|l| match l {
            ModalLayer::ApplyPatch(s) => Some(s.as_ref()),
            _ => None,
        })
    }

    pub fn apply_patch_mut(&mut self) -> Option<&mut crate::patch_view::ApplyPatchState> {
        self.layers.iter_mut().rev().find_map(|l| match l {
            ModalLayer::ApplyPatch(s) => Some(s.as_mut()),
            _ => None,
        })
    }

    pub fn branch_cleanup_mut(&mut self) -> Option<&mut crate::branch_cleanup::BranchCleanupState> {
        self.layers.iter_mut().rev().find_map(|l| match l {
            ModalLayer::BranchCleanup(bc) => Some(bc.as_mut()),
            _ => None,
        })
    }

    pub fn tree_browser(&self) -> Option<&crate::tree_browser::TreeBrowserState> {
        self.layers.iter().rev().find_map(|l| match l {
            ModalLayer::TreeBrowser(s) => Some(s.as_ref()),
            _ => None,
        })
    }

    pub fn tree_browser_mut(&mut self) -> Option<&mut crate::tree_browser::TreeBrowserState> {
        self.layers.iter_mut().rev().find_map(|l| match l {
            ModalLayer::TreeBrowser(s) => Some(s.as_mut()),
            _ => None,
        })
    }

    pub fn remotes_mut(&mut self) -> Option<&mut crate::remotes_view::RemotesState> {
        self.layers.iter_mut().rev().find_map(|l| match l {
            ModalLayer::Remotes(s) => Some(s),
            _ => None,
        })
    }

    pub fn worktrees_mut(&mut self) -> Option<&mut crate::worktrees_view::WorktreesState> {
        self.layers.iter_mut().rev().find_map(|l| match l {
            ModalLayer::Worktrees(s) => Some(s),
            _ => None,
        })
    }

    pub fn repo_switcher_mut(&mut self) -> Option<&mut crate::repo_switcher::RepoSwitcherState> {
        self.layers.iter_mut().rev().find_map(|l| match l {
            ModalLayer::RepoSwitcher(s) => Some(s.as_mut()),
            _ => None,
        })
    }

    pub fn push_dialog_mut(&mut self) -> Option<&mut PushDialogState> {
        self.layers.iter_mut().rev().find_map(|l| match l {
            ModalLayer::PushDialog(pd) => Some(pd.as_mut()),
            _ => None,
        })
    }

    pub fn open_push_dialog(&mut self, pd: PushDialogState) {
        if let Some(ModalLayer::PushDialog(existing)) = self.layers.last_mut() {
            **existing = pd;
        } else {
            self.layers.push(ModalLayer::PushDialog(Box::new(pd)));
        }
    }

    pub fn close_push_dialog(&mut self) {
        if let Some(pos) = self
            .layers
            .iter()
            .rposition(|l| matches!(l, ModalLayer::PushDialog(_)))
        {
            self.layers.remove(pos);
        }
    }

    pub fn conflict_merge(&self) -> Option<&ConflictMergeState> {
        self.layers.iter().rev().find_map(|l| match l {
            ModalLayer::ConflictResolver(cm) => Some(cm.as_ref()),
            _ => None,
        })
    }

    pub fn conflict_merge_mut(&mut self) -> Option<&mut ConflictMergeState> {
        self.layers.iter_mut().rev().find_map(|l| match l {
            ModalLayer::ConflictResolver(cm) => Some(cm.as_mut()),
            _ => None,
        })
    }

    pub fn open_conflict_resolver(&mut self, cm: ConflictMergeState) {
        self.close_conflict_resolver();
        self.layers.push(ModalLayer::ConflictResolver(Box::new(cm)));
    }

    pub fn close_conflict_resolver(&mut self) {
        if let Some(pos) = self
            .layers
            .iter()
            .rposition(|l| matches!(l, ModalLayer::ConflictResolver(_)))
        {
            self.layers.remove(pos);
        }
    }

    pub fn conflict_queue(&self) -> Option<&ConflictQueueState> {
        self.layers.iter().rev().find_map(|l| match l {
            ModalLayer::ConflictQueue(q) => Some(q.as_ref()),
            _ => None,
        })
    }

    pub fn conflict_queue_mut(&mut self) -> Option<&mut ConflictQueueState> {
        self.layers.iter_mut().rev().find_map(|l| match l {
            ModalLayer::ConflictQueue(q) => Some(q.as_mut()),
            _ => None,
        })
    }

    pub fn open_conflict_queue(&mut self, q: ConflictQueueState) {
        self.close_conflict_queue();
        self.layers.push(ModalLayer::ConflictQueue(Box::new(q)));
    }

    pub fn close_conflict_queue(&mut self) {
        if let Some(pos) = self
            .layers
            .iter()
            .rposition(|l| matches!(l, ModalLayer::ConflictQueue(_)))
        {
            self.layers.remove(pos);
        }
    }

    pub fn three_way_range_idx(&self) -> Option<usize> {
        self.layers.iter().rev().find_map(|l| match l {
            ModalLayer::ThreeWayInspector { range_idx } => *range_idx,
            _ => None,
        })
    }

    /// Updates menu hover selection & arms/clears the 150ms cascading-submenu
    /// hover timer. Returns `true` when visual selection changed.
    pub(crate) fn apply_menu_hover(
        &mut self,
        menu_depth: u8,
        item_idx: usize,
        now: Instant,
        menu_hover: &mut Option<MenuHover>,
    ) -> bool {
        let Some(pos) = self.layers.iter().rposition(
            |l| matches!(l, ModalLayer::ContextMenu { depth, .. } if *depth == menu_depth),
        ) else {
            return false;
        };
        if self.layers[pos + 1..]
            .iter()
            .any(|l| !matches!(l, ModalLayer::ContextMenu { .. }))
        {
            *menu_hover = None;
            return false;
        }
        let (is_submenu, changed) = match &mut self.layers[pos] {
            ModalLayer::ContextMenu {
                items, selected, ..
            } => {
                let Some(item) = items.get(item_idx) else {
                    return false;
                };
                if item.is_separator() {
                    return false;
                }
                let changed = *selected != item_idx;
                *selected = item_idx;
                (
                    matches!(item.kind, ContextMenuEntryKind::Submenu(_)) && item.is_enabled(),
                    changed,
                )
            }
            _ => return false,
        };
        let child_open_for = match self.layers.get(pos + 1) {
            Some(ModalLayer::ContextMenu {
                parent: Some(p), ..
            }) => Some(p.item_idx),
            _ => None,
        };
        let needs_timer = (is_submenu && child_open_for != Some(item_idx))
            || (child_open_for.is_some() && child_open_for != Some(item_idx));
        if needs_timer {
            let already =
                menu_hover.is_some_and(|h| h.depth == menu_depth && h.item_idx == item_idx);
            if !already {
                *menu_hover = Some(MenuHover {
                    depth: menu_depth,
                    item_idx,
                    due: now + MENU_HOVER_DELAY,
                });
            }
        } else {
            *menu_hover = None;
        }
        changed
    }

    /// Checks a due hover timer, truncates deeper submenus, and returns
    /// `(needs_redraw, Option<Action::OpenSubmenu>)`.
    pub(crate) fn take_due_menu_hover_action(
        &mut self,
        now: Instant,
        screen: Rect,
        menu_hover: &mut Option<MenuHover>,
    ) -> (bool, Option<Action>) {
        let Some(h) = *menu_hover else {
            return (false, None);
        };
        if h.due > now {
            return (false, None);
        }
        *menu_hover = None;
        let Some(pos) = self
            .layers
            .iter()
            .rposition(|l| matches!(l, ModalLayer::ContextMenu { depth, .. } if *depth == h.depth))
        else {
            return (false, None);
        };
        if self.layers[pos + 1..]
            .iter()
            .any(|l| !matches!(l, ModalLayer::ContextMenu { .. }))
        {
            return (false, None);
        }
        if let Some(ModalLayer::ContextMenu {
            parent: Some(p),
            depth,
            ..
        }) = self.layers.get(pos + 1)
            && *depth == h.depth + 1
            && p.item_idx == h.item_idx
        {
            return (false, None);
        }
        self.layers.truncate(pos + 1);
        if let Some(ModalLayer::ContextMenu {
            anchor,
            items,
            parent,
            ..
        }) = self.layers.get(pos)
            && let Some(item) = items.get(h.item_idx)
            && item.is_enabled()
            && let ContextMenuEntryKind::Submenu(children) = &item.kind
        {
            let rect = context_menu_rect(screen, *anchor, *parent, items);
            let row_y = menu_item_row_y(screen, rect, items, h.item_idx);
            return (
                true,
                Some(Action::OpenSubmenu {
                    parent: MenuParent {
                        rect,
                        row_y,
                        item_idx: h.item_idx,
                    },
                    items: children.clone(),
                    depth: h.depth + 1,
                }),
            );
        }
        (true, None)
    }
}

impl std::ops::Index<usize> for ModalStack {
    type Output = ModalLayer;

    fn index(&self, index: usize) -> &Self::Output {
        &self.layers[index]
    }
}

impl std::ops::IndexMut<usize> for ModalStack {
    fn index_mut(&mut self, index: usize) -> &mut Self::Output {
        &mut self.layers[index]
    }
}

impl<'a> IntoIterator for &'a ModalStack {
    type Item = &'a ModalLayer;
    type IntoIter = std::slice::Iter<'a, ModalLayer>;

    fn into_iter(self) -> Self::IntoIter {
        self.layers.iter()
    }
}

impl<'a> IntoIterator for &'a mut ModalStack {
    type Item = &'a mut ModalLayer;
    type IntoIter = std::slice::IterMut<'a, ModalLayer>;

    fn into_iter(self) -> Self::IntoIter {
        self.layers.iter_mut()
    }
}
