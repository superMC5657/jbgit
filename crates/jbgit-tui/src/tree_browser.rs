//! `Browse Repository at Revision` (Spec 12 Issue 13,
//! `GitBrowseRepoAtRevisionAction`): a lazily loaded directory tree of one
//! commit with a syntax-highlighted, size-guarded file preview, plus
//! Compare with Local / Get Version / Annotate / Show History / Copy Path.

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use jbgit_core::{
    GitOp, Oid, TreeBlob, TreeBlobContent, TreeBlobKey, TreeEntry, TreeEntryKind, TreeKey,
    TreeListing,
};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};
use unicode_width::UnicodeWidthStr;

use crate::app::{Action, AppState, ModalLayer, RenderContext};
use crate::dialogs::{ChoiceDialog, ChoiceOption, Dialog};
use crate::hit_test::HitTarget;
use crate::syntax::{HighlightedFile, render_diff_line_spans_with_tab};
use crate::views::overlays::draw_shadow;
use crate::views::truncate_to_width;

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
    pub scroll: Cell<usize>,
    pub focus: TreeBrowserFocus,
    pub preview: Option<Arc<TreeBlob>>,
    pub preview_requested: Option<TreeBlobKey>,
    pub preview_syntax: Option<Arc<HighlightedFile>>,
    pub preview_scroll: usize,
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
            scroll: Cell::new(0),
            focus: TreeBrowserFocus::Tree,
            preview: None,
            preview_requested: None,
            preview_syntax: None,
            preview_scroll: 0,
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

    fn preview_line_count(&self) -> usize {
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
                    self.preview_scroll = 0;
                }
            }
            TreeBrowserAction::Select(i) => {
                if len > 0 {
                    self.selected = (*i).min(len - 1);
                    self.focus = TreeBrowserFocus::Tree;
                    self.preview_scroll = 0;
                }
            }
            TreeBrowserAction::Expand => {
                let Some(e) = self.selected_entry().cloned() else {
                    return;
                };
                if e.is_dir() {
                    if self.expanded.contains(&e.path) {
                        // Step into the first child.
                        if self.selected + 1 < len {
                            self.selected += 1;
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
                }
            }
            TreeBrowserAction::Toggle(i) => {
                if len == 0 {
                    return;
                }
                self.selected = (*i).min(len - 1);
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
        }
    }

    fn scroll_preview(&mut self, delta: isize) {
        let max = self.preview_line_count().saturating_sub(1);
        self.preview_scroll =
            (self.preview_scroll as isize + delta).clamp(0, max as isize) as usize;
    }
}

fn top(state: &AppState) -> Option<&TreeBrowserState> {
    match state.modal_stack.last() {
        Some(ModalLayer::TreeBrowser(s)) => Some(s.as_ref()),
        _ => None,
    }
}

/// Commands on the selected entry: `c` compare with local, `g` get version,
/// `a` annotate, `h` history, `y` copy path.
pub fn tree_command(state: &AppState, cmd: char) -> Vec<Action> {
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
        'y' => vec![Action::CopyText(e.path.clone())],
        _ if e.kind == TreeEntryKind::Submodule => read_only(),
        'c' if e.kind == TreeEntryKind::Blob => vec![
            Action::PopModal,
            Action::CompareFileWithLocal {
                target: s.oid,
                path: e.path.clone(),
            },
        ],
        'c' => vec![Action::PopModal, Action::CompareWithLocal { target: s.oid }],
        'a' if e.kind == TreeEntryKind::Blob => vec![Action::OpenAnnotate {
            path: e.path.clone(),
            rev: Some(s.oid),
        }],
        'a' => vec![Action::Notice(
            i18n.t("view.tree.annotate_file_only").into(),
        )],
        'h' => vec![Action::OpenFileHistory {
            path: e.path.clone(),
            start_oid: Some(s.oid),
            follow_renames: e.kind == TreeEntryKind::Blob,
            line_range: None,
        }],
        'g' => {
            if let Err(msg) = state.require_no_ongoing() {
                return vec![Action::Notice(msg)];
            }
            let short = s.oid.short();
            vec![Action::OpenDialog(Box::new(Dialog::Choice(
                ChoiceDialog::buttons(
                    i18n.t("view.tree.get_version_title"),
                    vec![i18n.t_fmt(
                        "view.tree.get_version_body",
                        &[("path", &e.path), ("short", &short)],
                    )],
                    vec![
                        ChoiceOption::new(
                            i18n.t("view.tree.get_version_btn"),
                            Some('g'),
                            vec![Action::RunGitOp(GitOp::CheckoutPaths {
                                rev: s.oid.to_string(),
                                paths: vec![e.path.clone()],
                            })],
                        )
                        .danger(),
                        ChoiceOption::new(i18n.cancel(), Some('c'), Vec::new()),
                    ],
                ),
            )))]
        }
        _ => Vec::new(),
    }
}

/// Keyboard routing while the Tree Browser is on top.
pub fn route_tree_browser_key(key: &KeyEvent, state: &AppState) -> Vec<Action> {
    if key
        .modifiers
        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
    {
        return match key.code {
            KeyCode::Char('d') => vec![Action::TreeBrowser(TreeBrowserAction::ScrollPreview(15))],
            KeyCode::Char('u') => {
                vec![Action::TreeBrowser(TreeBrowserAction::ScrollPreview(-15))]
            }
            _ => Vec::new(),
        };
    }
    let tb = Action::TreeBrowser;
    match key.code {
        KeyCode::Esc | KeyCode::Char('q') => vec![Action::PopModal],
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
        KeyCode::Char(c @ ('c' | 'g' | 'a' | 'y')) => tree_command(state, c),
        KeyCode::Char('h' | 'H') => tree_command(state, 'h'),
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
    let title = ctx.t_fmt("view.tree.title", &[("title", &s.title)]);
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
    render_tree_pane(frame, tree_area, s, ctx, z);
    render_preview_pane(frame, preview_area, s, ctx, z);

    let hint = ctx.t("view.tree.hint").to_string();
    frame.render_widget(
        Paragraph::new(Span::styled(
            truncate_to_width(&format!(" {hint}"), inner.width as usize),
            Style::default().fg(ctx.theme.fg_muted),
        )),
        Rect::new(inner.x, inner.bottom().saturating_sub(1), inner.width, 1),
    );
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
    let mut first = s.scroll.get();
    if s.selected < first {
        first = s.selected;
    } else if s.selected >= first + h {
        first = s.selected + 1 - h;
    }
    first = first.min(s.rows.len().saturating_sub(h));
    s.scroll.set(first);
    let glyphs = ctx.glyph_mode.symbols();
    let focused = s.focus == TreeBrowserFocus::Tree;
    for (v, (idx, row)) in s.rows.iter().enumerate().skip(first).take(h).enumerate() {
        let e = &row.entry;
        let selected = idx == s.selected;
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
                    " →".to_string()
                } else {
                    String::new()
                };
                (glyphs.file_icon(&e.path), ctx.theme.fg_primary, suffix)
            }
        };
        let text = format!(" {indent}{icon} {}{suffix}", e.name);
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
    let placeholder = |frame: &mut Frame<'_>, text: String| {
        frame.render_widget(
            Paragraph::new(Span::styled(
                truncate_to_width(&format!(" {text}"), width),
                muted,
            )),
            Rect::new(area.x, area.y, area.width, 1),
        );
    };
    let Some(e) = s.selected_entry() else {
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
                ctx.t_fmt("view.tree.dir_summary", &[("path", &e.path), ("count", &n)]),
            );
        }
        TreeEntryKind::Submodule => {
            let short = e.oid.short();
            return placeholder(
                frame,
                ctx.t_fmt(
                    "view.tree.submodule_preview",
                    &[("path", &e.path), ("short", &short)],
                ),
            );
        }
        TreeEntryKind::Blob => {}
    }
    let Some(blob) = s.preview_for_selection() else {
        return placeholder(frame, ctx.t("view.tree.loading").to_string());
    };
    let text = match &blob.content {
        TreeBlobContent::Text(t) => t,
        TreeBlobContent::Binary => {
            let size = blob.size.to_string();
            return placeholder(frame, ctx.t_fmt("view.tree.binary", &[("size", &size)]));
        }
        TreeBlobContent::TooLarge { limit } => {
            let mib = (limit / (1024 * 1024)).to_string();
            return placeholder(frame, ctx.t_fmt("view.tree.too_large", &[("mib", &mib)]));
        }
        TreeBlobContent::Missing => {
            return placeholder(frame, ctx.t("view.tree.missing").to_string());
        }
    };
    let lines: Vec<&str> = text.lines().collect();
    let total = lines.len().max(1);
    let lineno_w = total.to_string().len();
    let h = area.height as usize;
    let first = s.preview_scroll.min(total.saturating_sub(h.max(1)));
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
                entry("lib", TreeEntryKind::Submodule),
                entry("a.txt", TreeEntryKind::Blob),
            ],
        ));
        assert_eq!(s.wanted_preview(), None);
        s.apply(&TreeBrowserAction::Select(1));
        assert_eq!(s.wanted_preview(), None, "submodules have no preview");
        s.apply(&TreeBrowserAction::Select(2));
        let key = s.wanted_preview().unwrap();
        assert_eq!(key.path, "a.txt");
        s.preview_requested = Some(key);
        assert_eq!(s.wanted_preview(), None);
    }
}
