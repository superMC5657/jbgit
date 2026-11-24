//! `Browse Repository at Revision` (Spec 12 Issue 13 + Spec 13 Issue 10,
//! `GitBrowseRepoAtRevisionAction`): a lazily loaded directory tree of one
//! commit with `/` async `nucleo` fuzzy path search, multi-select batch
//! `Get Version` (`--worktree` vs `--staged --worktree`), syntax-highlighted
//! size-guarded file preview, explicit symlink target display, `Copy File
//! Content`, `Export to File…`, directory-scoped `Compare with Local`,
//! Annotate, Show History, and Copy Path.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use jbgit_core::{
    GitOp, Oid, TreeBlob, TreeBlobContent, TreeBlobKey, TreeEntry, TreeEntryKind, TreeKey,
    TreeListing, fuzzy_filter_paths,
};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};
use unicode_width::UnicodeWidthStr;

use crate::app::{Action, AppState, ModalLayer, RenderContext, ScrollPane, ScrollViewport};
use crate::dialogs::{ChoiceDialog, ChoiceOption, Dialog, InputDialog, InputIntent, InputValidation};
use crate::hit_test::HitTarget;
use crate::syntax::{HighlightedFile, render_diff_line_spans_with_tab};
use crate::views::overlays::draw_shadow;
use crate::views::truncate_to_width;

const MAX_SEARCH_RESULTS: usize = 200;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TreeBrowserFocus {
    #[default]
    Tree,
    Preview,
}

/// Sub-actions of [`Action::TreeBrowser`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TreeBrowserAction {
    Move(isize),
    Select(usize),
    /// Expand a directory / focus the preview of a file.
    Expand,
    /// Collapse a directory (or jump to its parent).
    Collapse,
    /// Double-click / Enter: toggle a directory.
    Toggle(usize),
    ToggleFocus,
    ScrollPreview(isize),
    /// Toggle multi-select checkbox on the selected row (`Space`).
    ToggleMultiSelect,
    /// Open `/` fuzzy path search.
    OpenSearch,
    /// Close `/` fuzzy path search.
    CloseSearch,
    SearchInput(char),
    SearchBackspace,
    SearchClear,
    SearchMove(isize),
    SearchSelect(usize),
    SearchConfirm,
}

/// One visible row of the flattened tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeRow {
    pub entry: TreeEntry,
    pub depth: usize,
}

/// State of [`ModalLayer::TreeBrowser`].
#[derive(Debug)]
pub struct TreeBrowserState {
    pub oid: Oid,
    /// `short subject` shown in the title.
    pub title: String,
    pub listings: HashMap<String, Arc<TreeListing>>,
    pub expanded: HashSet<String>,
    /// Directories waiting to be listed (one request in flight at a time).
    pub pending: Vec<String>,
    pub in_flight: Option<String>,
    pub errors: HashMap<String, String>,
    pub rows: Vec<TreeRow>,
    pub selected: usize,
    /// Multi-selected paths for batch `Get Version`.
    pub multi_selected: HashSet<String>,
    pub scroll: ScrollViewport,
    pub focus: TreeBrowserFocus,
    pub preview: Option<Arc<TreeBlob>>,
    pub preview_requested: Option<TreeBlobKey>,
    pub preview_syntax: Option<Arc<HighlightedFile>>,
    pub preview_scroll: ScrollViewport,
    /// `/` fuzzy path search state.
    pub search_open: bool,
    pub search_query: String,
    pub search_index: Option<Arc<Vec<String>>>,
    search_index_requested: bool,
    search_index_pending: bool,
    pub search_matches: Vec<(String, Vec<u32>)>,
    pub search_selected: usize,
    pub search_scroll: ScrollViewport,
    /// Target path to jump to once its ancestor listings finish loading.
    pub pending_reveal_path: Option<String>,
}

impl TreeBrowserState {
    pub fn new(oid: Oid, title: String) -> Self {
        Self {
            oid,
            title,
            listings: HashMap::new(),
            expanded: HashSet::new(),
            pending: vec![String::new()],
            in_flight: None,
            errors: HashMap::new(),
            rows: Vec::new(),
            selected: 0,
            multi_selected: HashSet::new(),
            scroll: ScrollViewport::new(),
            focus: TreeBrowserFocus::Tree,
            preview: None,
            preview_requested: None,
            preview_syntax: None,
            preview_scroll: ScrollViewport::new(),
            search_open: false,
            search_query: String::new(),
            search_index: None,
            search_index_requested: false,
            search_index_pending: false,
            search_matches: Vec::new(),
            search_selected: 0,
            search_scroll: ScrollViewport::new(),
            pending_reveal_path: None,
        }
    }

    /// Next directory listing to request, if none is in flight.
    pub fn next_request(&mut self) -> Option<TreeKey> {
        if self.in_flight.is_some() {
            return None;
        }
        let dir = self.pending.first().cloned()?;
        self.pending.remove(0);
        self.in_flight = Some(dir.clone());
        Some(TreeKey { oid: self.oid, dir })
    }

    /// Returns `Some(oid)` once when `/` search needs the recursive path index.
    pub fn take_wanted_index_request(&mut self) -> Option<Oid> {
        if self.search_index_pending {
            self.search_index_pending = false;
            return Some(self.oid);
        }
        None
    }

    pub fn clear_search_index_request(&mut self) {
        self.search_index_requested = false;
        self.search_index_pending = false;
    }

    pub fn on_index_loaded(&mut self, oid: Oid, paths: Arc<Vec<String>>) {
        if oid != self.oid {
            return;
        }
        self.search_index = Some(paths);
        self.recompute_search_matches();
    }

    fn recompute_search_matches(&mut self) {
        let Some(index) = self.search_index.as_deref() else {
            self.search_matches.clear();
            self.search_selected = 0;
            return;
        };
        self.search_matches = fuzzy_filter_paths(index, &self.search_query, MAX_SEARCH_RESULTS);
        self.search_selected = self
            .search_selected
            .min(self.search_matches.len().saturating_sub(1));
        self.sync_search_scroll();
    }

    fn sync_search_scroll(&mut self) {
        if let Some(vh) = self.search_scroll.viewport_height() {
            self.search_scroll
                .ensure_visible(self.search_selected, self.search_matches.len(), vh);
        }
    }

    pub fn on_listing(&mut self, listing: Arc<TreeListing>) {
        if listing.key.oid != self.oid {
            return;
        }
        if self.in_flight.as_deref() == Some(listing.key.dir.as_str()) {
            self.in_flight = None;
        }
        self.errors.remove(&listing.key.dir);
        self.listings.insert(listing.key.dir.clone(), listing);
        self.rebuild_rows();
        self.try_reveal_pending();
    }

    pub fn on_listing_failed(&mut self, key: &TreeKey, message: String) {
        if key.oid != self.oid {
            return;
        }
        if self.in_flight.as_deref() == Some(key.dir.as_str()) {
            self.in_flight = None;
        }
        self.errors.insert(key.dir.clone(), message);
        self.expanded.remove(&key.dir);
        self.pending_reveal_path = None;
        self.rebuild_rows();
    }

    pub fn is_loading(&self, dir: &str) -> bool {
        self.in_flight.as_deref() == Some(dir) || self.pending.iter().any(|d| d == dir)
    }

    pub fn rebuild_rows(&mut self) {
        let keep = self.selected_entry().map(|e| e.path.clone());
        let mut rows = Vec::new();
        self.push_rows("", 0, &mut rows);
        self.rows = rows;
        if let Some(path) = keep
            && let Some(i) = self.rows.iter().position(|r| r.entry.path == path)
        {
            self.selected = i;
        }
        self.selected = self.selected.min(self.rows.len().saturating_sub(1));
        self.sync_list_scroll();
    }

    fn try_reveal_pending(&mut self) {
        let Some(target) = self.pending_reveal_path.as_deref() else {
            return;
        };
        if let Some(idx) = self.rows.iter().position(|r| r.entry.path == target) {
            self.selected = idx;
            self.focus = TreeBrowserFocus::Tree;
            self.preview_scroll.reset();
            self.pending_reveal_path = None;
            self.sync_list_scroll();
        }
    }

    /// Expands all ancestor directories of `path` and schedules selection once loaded.
    pub fn reveal_path(&mut self, path: &str) {
        self.pending_reveal_path = Some(path.to_string());
        let mut cur = String::new();
        let parts: Vec<&str> = path.split('/').collect();
        if parts.len() > 1 {
            for seg in &parts[..parts.len() - 1] {
                if !cur.is_empty() {
                    cur.push('/');
                }
                cur.push_str(seg);
                self.expand(&cur);
            }
        }
        self.try_reveal_pending();
    }

    fn sync_list_scroll(&mut self) {
        if let Some(vh) = self.scroll.viewport_height() {
            self.scroll.ensure_visible(self.selected, self.rows.len(), vh);
        }
    }

    fn push_rows(&self, dir: &str, depth: usize, out: &mut Vec<TreeRow>) {
        let Some(listing) = self.listings.get(dir) else {
            return;
        };
        for e in &listing.entries {
            out.push(TreeRow {
                entry: e.clone(),
                depth,
            });
            if e.is_dir() && self.expanded.contains(&e.path) {
                self.push_rows(&e.path, depth + 1, out);
            }
        }
    }

    pub fn selected_entry(&self) -> Option<&TreeEntry> {
        self.rows.get(self.selected).map(|r| &r.entry)
    }

    /// Expands `path` (queueing its listing when not loaded yet).
    pub fn expand(&mut self, path: &str) {
        self.expanded.insert(path.to_string());
        if !self.listings.contains_key(path) && !self.is_loading(path) {
            self.errors.remove(path);
            self.pending.push(path.to_string());
        }
        self.rebuild_rows();
    }

    pub fn collapse(&mut self, path: &str) {
        self.expanded.remove(path);
        self.rebuild_rows();
    }

    /// Preview request for the selected blob, when it changed.
    pub fn wanted_preview(&self) -> Option<TreeBlobKey> {
        let e = self.selected_entry()?;
        if e.kind != TreeEntryKind::Blob {
            return None;
        }
        let key = TreeBlobKey {
            oid: self.oid,
            path: e.path.clone(),
        };
        (self.preview_requested.as_ref() != Some(&key)).then_some(key)
    }

    pub fn preview_for_selection(&self) -> Option<&TreeBlob> {
        let e = self.selected_entry()?;
        self.preview
            .as_deref()
            .filter(|b| b.key.path == e.path && b.key.oid == self.oid)
    }

    pub(crate) fn preview_line_count(&self) -> usize {
        match self.preview_for_selection().map(|b| &b.content) {
            Some(TreeBlobContent::Text(t)) => t.lines().count(),
            _ => 0,
        }
    }

    /// Applies a sub-action (pure state; requests are issued by the caller).
    pub fn apply(&mut self, action: &TreeBrowserAction) {
        let len = self.rows.len();
        match action {
            TreeBrowserAction::Move(delta) => {
                if self.focus == TreeBrowserFocus::Preview {
                    self.scroll_preview(*delta);
                } else if len > 0 {
                    self.selected =
                        (self.selected as isize + delta).clamp(0, len as isize - 1) as usize;
                    self.preview_scroll.reset();
                    self.sync_list_scroll();
                }
            }
            TreeBrowserAction::Select(i) => {
                if len > 0 {
                    self.selected = (*i).min(len - 1);
                    self.focus = TreeBrowserFocus::Tree;
                    self.preview_scroll.reset();
                    self.sync_list_scroll();
                }
            }
            TreeBrowserAction::Expand => {
                let Some(e) = self.selected_entry().cloned() else {
                    return;
                };
                if e.is_dir() {
                    if self.expanded.contains(&e.path) {
                        if self.selected + 1 < len {
                            self.selected += 1;
                            self.sync_list_scroll();
                        }
                    } else {
                        self.expand(&e.path);
                    }
                } else if e.kind == TreeEntryKind::Blob {
                    self.focus = TreeBrowserFocus::Preview;
                }
            }
            TreeBrowserAction::Collapse => {
                if self.focus == TreeBrowserFocus::Preview {
                    self.focus = TreeBrowserFocus::Tree;
                    return;
                }
                let Some(e) = self.selected_entry().cloned() else {
                    return;
                };
                if e.is_dir() && self.expanded.contains(&e.path) {
                    self.collapse(&e.path);
                } else if let Some((parent, _)) = e.path.rsplit_once('/')
                    && let Some(i) = self.rows.iter().position(|r| r.entry.path == parent)
                {
                    self.selected = i;
                    self.sync_list_scroll();
                }
            }
            TreeBrowserAction::Toggle(i) => {
                if len == 0 {
                    return;
                }
                self.selected = (*i).min(len - 1);
                self.sync_list_scroll();
                let Some(e) = self.selected_entry().cloned() else {
                    return;
                };
                if e.is_dir() {
                    if self.expanded.contains(&e.path) {
                        self.collapse(&e.path);
                    } else {
                        self.expand(&e.path);
                    }
                } else if e.kind == TreeEntryKind::Blob {
                    self.focus = TreeBrowserFocus::Preview;
                }
            }
            TreeBrowserAction::ToggleFocus => {
                self.focus = match self.focus {
                    TreeBrowserFocus::Tree => TreeBrowserFocus::Preview,
                    TreeBrowserFocus::Preview => TreeBrowserFocus::Tree,
                };
            }
            TreeBrowserAction::ScrollPreview(delta) => self.scroll_preview(*delta),
            TreeBrowserAction::ToggleMultiSelect => {
                if let Some(e) = self.selected_entry().cloned()
                    && e.kind != TreeEntryKind::Submodule
                {
                    if !self.multi_selected.remove(&e.path) {
                        self.multi_selected.insert(e.path);
                    }
                    if self.selected + 1 < len {
                        self.selected += 1;
                        self.preview_scroll.reset();
                        self.sync_list_scroll();
                    }
                }
            }
            TreeBrowserAction::OpenSearch => {
                self.search_open = true;
                self.focus = TreeBrowserFocus::Tree;
                if !self.search_index_requested {
                    self.search_index_requested = true;
                    self.search_index_pending = true;
                }
                self.recompute_search_matches();
            }
            TreeBrowserAction::CloseSearch => {
                self.search_open = false;
            }
            TreeBrowserAction::SearchInput(c) => {
                self.search_query.push(*c);
                self.search_selected = 0;
                self.search_scroll.reset();
                self.recompute_search_matches();
            }
            TreeBrowserAction::SearchBackspace => {
                self.search_query.pop();
                self.search_selected = 0;
                self.search_scroll.reset();
                self.recompute_search_matches();
            }
            TreeBrowserAction::SearchClear => {
                self.search_query.clear();
                self.search_selected = 0;
                self.search_scroll.reset();
                self.recompute_search_matches();
            }
            TreeBrowserAction::SearchMove(delta) => {
                let mlen = self.search_matches.len();
                if mlen > 0 {
                    self.search_selected = (self.search_selected as isize + delta)
                        .clamp(0, mlen as isize - 1)
                        as usize;
                    self.sync_search_scroll();
                }
            }
            TreeBrowserAction::SearchSelect(i) => {
                let mlen = self.search_matches.len();
                if mlen > 0 {
                    self.search_selected = (*i).min(mlen - 1);
                    self.sync_search_scroll();
                }
            }
            TreeBrowserAction::SearchConfirm => {
                if let Some((path, _)) = self.search_matches.get(self.search_selected).cloned() {
                    self.search_open = false;
                    self.reveal_path(&path);
                }
            }
        }
    }

    fn scroll_preview(&mut self, delta: isize) {
        let fallback = self.preview_line_count().saturating_sub(1);
        self.preview_scroll.scroll_by(delta, fallback);
    }
}

fn top(state: &AppState) -> Option<&TreeBrowserState> {
    match state.modal_stack.last() {
        Some(ModalLayer::TreeBrowser(s)) => Some(s.as_ref()),
        _ => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TreeCommand {
    CopyPath,
    CopyContent,
    ExportToFile,
    CompareWithLocal,
    Annotate,
    ShowHistory,
    GetVersion,
}

/// Commands on the selected entry (or multi-selected entries for `GetVersion`).
pub fn tree_command(state: &AppState, cmd: TreeCommand) -> Vec<Action> {
    let i18n = &state.i18n;
    let Some(s) = top(state) else {
        return Vec::new();
    };
    let Some(e) = s.selected_entry() else {
        return vec![Action::Notice(i18n.t("view.tree.nothing_selected").into())];
    };
    let read_only = || {
        vec![Action::Notice(i18n.t_fmt(
            "view.tree.submodule_read_only",
            &[("path", &e.path)],
        ))]
    };
    match cmd {
        TreeCommand::CopyPath => vec![Action::CopyText(e.path.clone())],
        _ if e.kind == TreeEntryKind::Submodule
            && (cmd != TreeCommand::GetVersion || s.multi_selected.is_empty()) =>
        {
            read_only()
        }
        TreeCommand::CopyContent => {
            if e.kind != TreeEntryKind::Blob {
                return vec![Action::Notice(
                    i18n.t("view.tree.copy_content_file_only").into(),
                )];
            }
            let Some(blob) = s.preview_for_selection() else {
                return vec![Action::Notice(i18n.t("view.tree.loading").into())];
            };
            match &blob.content {
                TreeBlobContent::Text(text) => vec![Action::CopyText(text.clone())],
                TreeBlobContent::Symlink(target) => vec![Action::CopyText(target.clone())],
                TreeBlobContent::Binary => vec![Action::Notice(
                    i18n.t("view.tree.copy_content_binary").into(),
                )],
                TreeBlobContent::TooLarge { .. } => vec![Action::Notice(
                    i18n.t("view.tree.copy_content_too_large").into(),
                )],
                TreeBlobContent::Lfs(_) | TreeBlobContent::Missing => vec![Action::Notice(
                    i18n.t("view.tree.copy_content_unavailable").into(),
                )],
            }
        }
        TreeCommand::ExportToFile => {
            if e.kind != TreeEntryKind::Blob {
                return vec![Action::Notice(
                    i18n.t("view.tree.export_file_only").into(),
                )];
            }
            let workdir = state.current_workdir();
            let base = state
                .last_patch_dir
                .clone()
                .or_else(|| workdir.parent().map(Path::to_path_buf))
                .unwrap_or_else(|| workdir.clone());
            let default_dest = base.join(&e.name).display().to_string();
            vec![Action::OpenDialog(Box::new(Dialog::Input(
                InputDialog::new(
                    i18n.t("view.tree.export_title"),
                    i18n.t_fmt("view.tree.export_prompt", &[("path", &e.path)]),
                    default_dest,
                    InputValidation::NonEmpty,
                    InputIntent::ExportTreeBlob {
                        oid: s.oid,
                        path: e.path.clone(),
                        base,
                    },
                ),
            )))]
        }
        TreeCommand::CompareWithLocal if e.kind == TreeEntryKind::Blob => vec![
            Action::PopModal,
            Action::CompareFileWithLocal {
                target: s.oid,
                path: e.path.clone(),
            },
        ],
        TreeCommand::CompareWithLocal => vec![
            Action::PopModal,
            Action::CompareDirWithLocal {
                target: s.oid,
                path_prefix: e.path.clone(),
            },
        ],
        TreeCommand::Annotate if e.kind == TreeEntryKind::Blob => vec![Action::OpenAnnotate {
            path: e.path.clone(),
            rev: Some(s.oid),
        }],
        TreeCommand::Annotate => vec![Action::Notice(
            i18n.t("view.tree.annotate_file_only").into(),
        )],
        TreeCommand::ShowHistory => vec![Action::OpenFileHistory {
            path: e.path.clone(),
            start_oid: Some(s.oid),
            follow_renames: e.kind == TreeEntryKind::Blob,
            line_range: None,
        }],
        TreeCommand::GetVersion => {
            if let Err(msg) = state.require_no_ongoing() {
                return vec![Action::Notice(msg)];
            }
            let mut paths: Vec<String> = if !s.multi_selected.is_empty() {
                s.multi_selected.iter().cloned().collect()
            } else {
                vec![e.path.clone()]
            };
            paths.sort();
            let short = s.oid.short();
            let summary_line = if let [one] = paths.as_slice() {
                i18n.t_fmt(
                    "view.tree.get_version_body",
                    &[("path", one), ("short", &short)],
                )
            } else {
                let count_s = paths.len().to_string();
                i18n.t_fmt(
                    "view.tree.get_version_body_many",
                    &[("count", &count_s), ("short", &short)],
                )
            };
            let rev = s.oid.to_string();
            let worktree_opt = ChoiceOption::new(
                i18n.t("view.tree.get_version_worktree"),
                Some('w'),
                vec![Action::RunGitOp(GitOp::RestorePaths {
                    rev: rev.clone(),
                    paths: paths.clone(),
                    staged: false,
                    worktree: true,
                })],
            )
            .detail(i18n.t("view.tree.get_version_worktree_detail"));
            let staged_opt = ChoiceOption::new(
                i18n.t("view.tree.get_version_staged"),
                Some('s'),
                vec![Action::RunGitOp(GitOp::RestorePaths {
                    rev,
                    paths,
                    staged: true,
                    worktree: true,
                })],
            )
            .detail(i18n.t("view.tree.get_version_staged_detail"))
            .danger();
            vec![Action::OpenDialog(Box::new(Dialog::Choice(
                ChoiceDialog::radio(
                    i18n.t("view.tree.get_version_title"),
                    vec![summary_line],
                    vec![worktree_opt, staged_opt],
                    0,
                    i18n.t("view.tree.get_version_btn"),
                ),
            )))]
        }
    }
}

/// Keyboard routing while the Tree Browser is on top.
pub fn route_tree_browser_key(key: &KeyEvent, state: &AppState) -> Vec<Action> {
    let tb = Action::TreeBrowser;
    if top(state).is_some_and(|s| s.search_open) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        return match key.code {
            KeyCode::Esc => vec![tb(TreeBrowserAction::CloseSearch)],
            KeyCode::Enter => vec![tb(TreeBrowserAction::SearchConfirm)],
            KeyCode::Down => vec![tb(TreeBrowserAction::SearchMove(1))],
            KeyCode::Up => vec![tb(TreeBrowserAction::SearchMove(-1))],
            KeyCode::Char('n') if ctrl => vec![tb(TreeBrowserAction::SearchMove(1))],
            KeyCode::Char('p') if ctrl => vec![tb(TreeBrowserAction::SearchMove(-1))],
            KeyCode::PageDown => vec![tb(TreeBrowserAction::SearchMove(10))],
            KeyCode::PageUp => vec![tb(TreeBrowserAction::SearchMove(-10))],
            KeyCode::Backspace => vec![tb(TreeBrowserAction::SearchBackspace)],
            KeyCode::Char('u') if ctrl => vec![tb(TreeBrowserAction::SearchClear)],
            KeyCode::Char(c) if !ctrl && !alt && !c.is_control() => {
                vec![tb(TreeBrowserAction::SearchInput(c))]
            }
            _ => Vec::new(),
        };
    }

    if key
        .modifiers
        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
    {
        return match key.code {
            KeyCode::Char('d') => vec![tb(TreeBrowserAction::ScrollPreview(15))],
            KeyCode::Char('u') => vec![tb(TreeBrowserAction::ScrollPreview(-15))],
            _ => Vec::new(),
        };
    }
    match key.code {
        KeyCode::Esc | KeyCode::Char('q') => vec![Action::PopModal],
        KeyCode::Char('/') => vec![tb(TreeBrowserAction::OpenSearch)],
        KeyCode::Char(' ') => vec![tb(TreeBrowserAction::ToggleMultiSelect)],
        KeyCode::Down | KeyCode::Char('j') => vec![tb(TreeBrowserAction::Move(1))],
        KeyCode::Up | KeyCode::Char('k') => vec![tb(TreeBrowserAction::Move(-1))],
        KeyCode::PageDown => vec![tb(TreeBrowserAction::Move(15))],
        KeyCode::PageUp => vec![tb(TreeBrowserAction::Move(-15))],
        KeyCode::Home => vec![tb(TreeBrowserAction::Select(0))],
        KeyCode::End | KeyCode::Char('G') => vec![tb(TreeBrowserAction::Select(usize::MAX))],
        KeyCode::Right | KeyCode::Char('l') => vec![tb(TreeBrowserAction::Expand)],
        KeyCode::Left => vec![tb(TreeBrowserAction::Collapse)],
        KeyCode::Enter => match top(state).map(|s| s.selected) {
            Some(i) => vec![tb(TreeBrowserAction::Toggle(i))],
            None => Vec::new(),
        },
        KeyCode::Tab | KeyCode::BackTab => vec![tb(TreeBrowserAction::ToggleFocus)],
        KeyCode::Char('c') => tree_command(state, TreeCommand::CompareWithLocal),
        KeyCode::Char('g') => tree_command(state, TreeCommand::GetVersion),
        KeyCode::Char('a') => tree_command(state, TreeCommand::Annotate),
        KeyCode::Char('y') => tree_command(state, TreeCommand::CopyPath),
        KeyCode::Char('Y') => tree_command(state, TreeCommand::CopyContent),
        KeyCode::Char('e') => tree_command(state, TreeCommand::ExportToFile),
        KeyCode::Char('h' | 'H') => tree_command(state, TreeCommand::ShowHistory),
        _ => Vec::new(),
    }
}

pub fn render_tree_browser(
    frame: &mut Frame<'_>,
    screen: Rect,
    s: &TreeBrowserState,
    ctx: &mut RenderContext<'_>,
    z: u8,
) {
    if screen.width < 40 || screen.height < 10 {
        return;
    }
    let rect = Rect::new(
        screen.x + 1,
        screen.y + 1,
        screen.width.saturating_sub(2),
        screen.height.saturating_sub(2),
    );
    draw_shadow(frame, screen, rect);
    frame.render_widget(Clear, rect);
    let mut title = ctx.t_fmt("view.tree.title", &[("title", &s.title)]);
    if !s.multi_selected.is_empty() {
        let count_s = s.multi_selected.len().to_string();
        title.push_str(&format!(
            "  [{}]",
            ctx.t_fmt("view.tree.selected_badge", &[("count", &count_s)])
        ));
    }
    let block = Block::default()
        .title(Span::styled(
            format!(
                " {} ",
                truncate_to_width(&title, rect.width.saturating_sub(4) as usize)
            ),
            Style::default()
                .fg(ctx.theme.fg_bright)
                .add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(ctx.theme.border_focused))
        .style(Style::default().bg(ctx.theme.bg_elevated));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    ctx.hit_registry
        .register(rect, z + 1, HitTarget::DialogBody);
    if inner.height < 3 {
        return;
    }
    let body_h = inner.height.saturating_sub(1);
    let tree_w = (inner.width * 2 / 5).clamp(20.min(inner.width), 60);
    let tree_area = Rect::new(inner.x, inner.y, tree_w, body_h);
    let sep_x = inner.x + tree_w;
    let preview_area = Rect::new(
        sep_x + 1,
        inner.y,
        inner.right().saturating_sub(sep_x + 1),
        body_h,
    );
    for y in inner.y..inner.y + body_h {
        frame.render_widget(
            Paragraph::new(Span::styled(
                "│",
                Style::default().fg(ctx.theme.border_splitter),
            )),
            Rect::new(sep_x, y, 1, 1),
        );
    }
    if s.search_open {
        render_search_pane(frame, tree_area, s, ctx, z);
    } else {
        render_tree_pane(frame, tree_area, s, ctx, z);
    }
    render_preview_pane(frame, preview_area, s, ctx, z);

    let hint = if s.search_open {
        ctx.t("view.tree.search_hint").to_string()
    } else {
        ctx.t("view.tree.hint").to_string()
    };
    frame.render_widget(
        Paragraph::new(Span::styled(
            truncate_to_width(&format!(" {hint}"), inner.width as usize),
            Style::default().fg(ctx.theme.fg_muted),
        )),
        Rect::new(inner.x, inner.bottom().saturating_sub(1), inner.width, 1),
    );
}

fn render_search_pane(
    frame: &mut Frame<'_>,
    area: Rect,
    s: &TreeBrowserState,
    ctx: &mut RenderContext<'_>,
    z: u8,
) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    let width = area.width as usize;
    let status_suffix = match &s.search_index {
        None => format!(" ({})", ctx.t("view.tree.search_indexing")),
        Some(_) => format!(" ({})", s.search_matches.len()),
    };
    let bar = format!(" / {}▏{status_suffix}", s.search_query);
    frame.render_widget(
        Paragraph::new(Span::styled(
            truncate_to_width(&bar, width),
            Style::default()
                .fg(ctx.theme.fg_bright)
                .bg(ctx.theme.bg_panel)
                .add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().bg(ctx.theme.bg_panel)),
        Rect::new(area.x, area.y, area.width, 1),
    );

    let list_h = area.height.saturating_sub(1) as usize;
    if list_h == 0 {
        ctx.record_scroll_bound(ScrollPane::TreeBrowserSearch, 0, 0);
        return;
    }
    if s.search_index.is_none() {
        ctx.record_scroll_bound(ScrollPane::TreeBrowserSearch, list_h, 0);
        frame.render_widget(
            Paragraph::new(Span::styled(
                truncate_to_width(&format!(" {}", ctx.t("view.tree.search_indexing")), width),
                Style::default().fg(ctx.theme.fg_muted),
            )),
            Rect::new(area.x, area.y + 1, area.width, 1),
        );
        return;
    }
    if s.search_matches.is_empty() {
        ctx.record_scroll_bound(ScrollPane::TreeBrowserSearch, list_h, 0);
        frame.render_widget(
            Paragraph::new(Span::styled(
                truncate_to_width(&format!(" {}", ctx.t("view.tree.search_no_matches")), width),
                Style::default().fg(ctx.theme.fg_muted),
            )),
            Rect::new(area.x, area.y + 1, area.width, 1),
        );
        return;
    }

    let mut vp = s.search_scroll;
    vp.ensure_visible(s.search_selected, s.search_matches.len(), list_h);
    let first = vp.offset();
    ctx.record_scroll_bound(
        ScrollPane::TreeBrowserSearch,
        list_h,
        s.search_matches.len().saturating_sub(list_h),
    );

    for (v, (idx, (path, matched_indices))) in s
        .search_matches
        .iter()
        .enumerate()
        .skip(first)
        .take(list_h)
        .enumerate()
    {
        let selected = idx == s.search_selected;
        let bg = if selected {
            ctx.theme.bg_selection_focused
        } else {
            ctx.theme.bg_elevated
        };
        let mut spans = vec![Span::styled(" ", Style::default().bg(bg))];
        let mut used = 1usize;
        let mut mi_pos = 0usize;
        for (ci, ch) in path.chars().enumerate() {
            let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
            if used + cw > width {
                break;
            }
            let hit = matched_indices
                .get(mi_pos)
                .is_some_and(|&m| m as usize == ci);
            if hit {
                mi_pos += 1;
            }
            let style = if hit {
                Style::default()
                    .fg(ctx.theme.fg_accent_link)
                    .bg(bg)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(ctx.theme.fg_primary).bg(bg)
            };
            spans.push(Span::styled(ch.to_string(), style));
            used += cw;
        }
        let row_rect = Rect::new(area.x, area.y + 1 + v as u16, area.width, 1);
        frame.render_widget(
            Paragraph::new(Line::from(spans)).style(Style::default().bg(bg)),
            row_rect,
        );
        ctx.hit_registry
            .register(row_rect, z + 2, HitTarget::TreeBrowserSearchRow(idx));
    }
}

fn render_tree_pane(
    frame: &mut Frame<'_>,
    area: Rect,
    s: &TreeBrowserState,
    ctx: &mut RenderContext<'_>,
    z: u8,
) {
    let h = area.height as usize;
    let width = area.width as usize;
    if s.rows.is_empty() {
        ctx.record_scroll_bound(ScrollPane::TreeBrowserList, h, 0);
        let text = if let Some(err) = s.errors.get("") {
            err.clone()
        } else if s.is_loading("") {
            ctx.t("view.tree.loading").to_string()
        } else {
            ctx.t("view.tree.empty").to_string()
        };
        frame.render_widget(
            Paragraph::new(Span::styled(
                truncate_to_width(&format!(" {text}"), width),
                Style::default().fg(ctx.theme.fg_muted),
            )),
            Rect::new(area.x, area.y, area.width, 1),
        );
        return;
    }
    let mut vp = s.scroll;
    vp.ensure_visible(s.selected, s.rows.len(), h);
    let first = vp.offset();
    ctx.record_scroll_bound(
        ScrollPane::TreeBrowserList,
        h,
        s.rows.len().saturating_sub(h),
    );
    let glyphs = ctx.glyph_mode.symbols();
    let focused = s.focus == TreeBrowserFocus::Tree;
    let show_checks = !s.multi_selected.is_empty();
    for (v, (idx, row)) in s.rows.iter().enumerate().skip(first).take(h).enumerate() {
        let e = &row.entry;
        let selected = idx == s.selected;
        let checked = s.multi_selected.contains(&e.path);
        let bg = if selected {
            if focused {
                ctx.theme.bg_selection_focused
            } else {
                ctx.theme.bg_selection_unfocused
            }
        } else {
            ctx.theme.bg_elevated
        };
        let indent = "  ".repeat(row.depth);
        let check_prefix = if show_checks {
            if e.kind == TreeEntryKind::Submodule {
                "    "
            } else if checked {
                "[✓] "
            } else {
                "[ ] "
            }
        } else {
            ""
        };
        let (icon, fg, suffix) = match e.kind {
            TreeEntryKind::Tree => {
                let open = s.expanded.contains(&e.path);
                let icon = if s.is_loading(&e.path) {
                    "…"
                } else if open {
                    glyphs.tree_expanded
                } else {
                    glyphs.tree_collapsed
                };
                (icon, ctx.theme.fg_bright, String::new())
            }
            TreeEntryKind::Submodule => (
                "⊙",
                ctx.theme.fg_muted,
                format!(" @{} {}", e.oid.short(), ctx.t("view.tree.submodule_tag")),
            ),
            TreeEntryKind::Blob => {
                let suffix = if e.is_symlink() {
                    match e.symlink_target.as_deref() {
                        Some(target) if !target.is_empty() => format!(" → {target}"),
                        _ => " →".to_string(),
                    }
                } else {
                    String::new()
                };
                (glyphs.file_icon(&e.path), ctx.theme.fg_primary, suffix)
            }
        };
        let text = format!(" {check_prefix}{indent}{icon} {}{suffix}", e.name);
        let row_rect = Rect::new(area.x, area.y + v as u16, area.width, 1);
        frame.render_widget(
            Paragraph::new(Span::styled(
                truncate_to_width(&text, width),
                Style::default().fg(fg).bg(bg),
            ))
            .style(Style::default().bg(bg)),
            row_rect,
        );
        ctx.hit_registry
            .register(row_rect, z + 2, HitTarget::TreeBrowserRow(idx));
    }
}

fn render_preview_pane(
    frame: &mut Frame<'_>,
    area: Rect,
    s: &TreeBrowserState,
    ctx: &mut RenderContext<'_>,
    z: u8,
) {
    ctx.hit_registry
        .register(area, z + 2, HitTarget::TreeBrowserPreview);
    let width = area.width as usize;
    let muted = Style::default().fg(ctx.theme.fg_muted);
    let h = area.height as usize;
    let placeholder = |frame: &mut Frame<'_>, ctx: &mut RenderContext<'_>, text: String, style: Style| {
        ctx.record_scroll_bound(ScrollPane::TreeBrowserPreview, h, 0);
        frame.render_widget(
            Paragraph::new(Span::styled(
                truncate_to_width(&format!(" {text}"), width),
                style,
            )),
            Rect::new(area.x, area.y, area.width, 1),
        );
    };
    let Some(e) = s.selected_entry() else {
        ctx.record_scroll_bound(ScrollPane::TreeBrowserPreview, h, 0);
        return;
    };
    match e.kind {
        TreeEntryKind::Tree => {
            let n = s
                .listings
                .get(&e.path)
                .map(|l| l.entries.len().to_string())
                .unwrap_or_else(|| "?".into());
            return placeholder(
                frame,
                ctx,
                ctx.t_fmt("view.tree.dir_summary", &[("path", &e.path), ("count", &n)]),
                muted,
            );
        }
        TreeEntryKind::Submodule => {
            let short = e.oid.short();
            return placeholder(
                frame,
                ctx,
                ctx.t_fmt(
                    "view.tree.submodule_preview",
                    &[("path", &e.path), ("short", &short)],
                ),
                muted,
            );
        }
        TreeEntryKind::Blob => {}
    }
    let Some(blob) = s.preview_for_selection() else {
        return placeholder(frame, ctx, ctx.t("view.tree.loading").to_string(), muted);
    };
    let text = match &blob.content {
        TreeBlobContent::Text(t) => t,
        TreeBlobContent::Symlink(target) => {
            return placeholder(
                frame,
                ctx,
                ctx.t_fmt("view.tree.symlink_preview", &[("target", target)]),
                Style::default()
                    .fg(ctx.theme.fg_accent_link)
                    .add_modifier(Modifier::BOLD),
            );
        }
        TreeBlobContent::Lfs(ptr) => {
            let size = ptr.format_size();
            return placeholder(
                frame,
                ctx,
                ctx.t_fmt(
                    "view.tree.lfs_preview",
                    &[("oid", ptr.short_oid()), ("size", &size)],
                ),
                muted,
            );
        }
        TreeBlobContent::Binary => {
            let size = blob.size.to_string();
            return placeholder(
                frame,
                ctx,
                ctx.t_fmt("view.tree.binary", &[("size", &size)]),
                muted,
            );
        }
        TreeBlobContent::TooLarge { limit } => {
            let mib = (limit / (1024 * 1024)).to_string();
            return placeholder(
                frame,
                ctx,
                ctx.t_fmt("view.tree.too_large", &[("mib", &mib)]),
                muted,
            );
        }
        TreeBlobContent::Missing => {
            return placeholder(frame, ctx, ctx.t("view.tree.missing").to_string(), muted);
        }
    };
    let lines: Vec<&str> = text.lines().collect();
    let total = lines.len().max(1);
    let lineno_w = total.to_string().len();
    let max_scroll = total.saturating_sub(h.max(1));
    ctx.record_scroll_bound(ScrollPane::TreeBrowserPreview, h, max_scroll);
    let first = s.preview_scroll.offset().min(max_scroll);
    let tab_size = ctx.diff_tab_size();
    let bg = ctx.theme.bg_elevated;
    for (v, (i, src)) in lines.iter().enumerate().skip(first).take(h).enumerate() {
        let tokens = s
            .preview_syntax
            .as_ref()
            .and_then(|hl| hl.lines.get(i))
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let gutter = format!("{:>lineno_w$} ", i + 1);
        let code_w = width.saturating_sub(gutter.width() + 1);
        let mut spans = vec![Span::styled(gutter, muted.bg(bg)), Span::raw(" ")];
        spans.extend(render_diff_line_spans_with_tab(
            src,
            tokens,
            &[],
            code_w,
            tab_size,
            Style::default().fg(ctx.theme.fg_primary).bg(bg),
            bg,
            ctx.theme.true_color,
        ));
        frame.render_widget(
            Paragraph::new(Line::from(spans)).style(Style::default().bg(bg)),
            Rect::new(area.x, area.y + v as u16, area.width, 1),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(path: &str, kind: TreeEntryKind) -> TreeEntry {
        TreeEntry {
            name: path.rsplit('/').next().unwrap().into(),
            path: path.into(),
            kind,
            mode: 0o100_644,
            size: Some(1),
            oid: Oid::from_bytes([1; 20]),
            symlink_target: None,
        }
    }

    fn listing(oid: Oid, dir: &str, entries: Vec<TreeEntry>) -> Arc<TreeListing> {
        Arc::new(TreeListing {
            key: TreeKey {
                oid,
                dir: dir.into(),
            },
            entries,
        })
    }

    #[test]
    fn listings_load_one_directory_at_a_time_and_flatten_expanded_rows() {
        let oid = Oid::from_bytes([7; 20]);
        let mut s = TreeBrowserState::new(oid, "t".into());
        assert_eq!(s.next_request().map(|k| k.dir), Some(String::new()));
        assert_eq!(s.next_request(), None, "single request in flight");
        s.on_listing(listing(
            oid,
            "",
            vec![
                entry("src", TreeEntryKind::Tree),
                entry("docs", TreeEntryKind::Tree),
                entry("a.txt", TreeEntryKind::Blob),
            ],
        ));
        assert_eq!(s.rows.len(), 3);

        s.expand("src");
        s.expand("docs");
        assert!(s.is_loading("src") && s.is_loading("docs"));
        assert_eq!(s.next_request().map(|k| k.dir).as_deref(), Some("src"));
        s.on_listing(listing(
            oid,
            "src",
            vec![entry("src/m.rs", TreeEntryKind::Blob)],
        ));
        assert_eq!(s.next_request().map(|k| k.dir).as_deref(), Some("docs"));
        s.on_listing_failed(
            &TreeKey {
                oid,
                dir: "docs".into(),
            },
            "boom".into(),
        );
        let paths: Vec<_> = s.rows.iter().map(|r| r.entry.path.as_str()).collect();
        assert_eq!(paths, vec!["src", "src/m.rs", "docs", "a.txt"]);
        assert_eq!(s.rows[1].depth, 1);

        // Collapse from a child jumps to its parent, then folds it.
        s.apply(&TreeBrowserAction::Select(1));
        s.apply(&TreeBrowserAction::Collapse);
        assert_eq!(s.selected_entry().unwrap().path, "src");
        s.apply(&TreeBrowserAction::Collapse);
        assert_eq!(s.rows.len(), 3);
        // Re-expanding a loaded directory needs no request.
        s.apply(&TreeBrowserAction::Expand);
        assert!(s.pending.is_empty());
        assert_eq!(s.rows.len(), 4);
    }

    #[test]
    fn preview_requests_follow_the_selected_blob_only() {
        let oid = Oid::from_bytes([7; 20]);
        let mut s = TreeBrowserState::new(oid, "t".into());
        s.next_request();
        s.on_listing(listing(
            oid,
            "",
            vec![
                entry("src", TreeEntryKind::Tree),
                entry("README.md", TreeEntryKind::Blob),
                entry("sub", TreeEntryKind::Submodule),
            ],
        ));
        assert_eq!(s.wanted_preview(), None, "directory has no blob preview");
        s.apply(&TreeBrowserAction::Select(1));
        let key = s.wanted_preview().expect("blob preview");
        assert_eq!(key.path, "README.md");
        s.preview_requested = Some(key);
        assert_eq!(s.wanted_preview(), None, "already requested");
        s.apply(&TreeBrowserAction::Select(2));
        assert_eq!(s.wanted_preview(), None, "submodule has no blob preview");
    }
}
