//! Repository switcher (Spec 12 Issue 09, `GitRepositoryManager` analogue
//! for a single active repository): the user-level recent repositories store
//! (`recent-repos.toml`) and the `RepoSwitcher` modal listing recent
//! repositories, linked working trees and submodules with fuzzy filtering.

use std::path::{Component, Path, PathBuf};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use jbgit_core::{fuzzy_indices, same_path};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};
use serde::{Deserialize, Serialize};
use unicode_width::UnicodeWidthStr;

use crate::app::{Action, AppState, ModalLayer, RenderContext, ScrollPane, ScrollViewport};
use crate::dialogs::{Dialog, InputDialog, InputIntent, InputValidation};
use crate::hit_test::HitTarget;
use crate::i18n::I18n;
use crate::views::overlays::draw_shadow;
use crate::views::truncate_to_width;

/// Classification of a repository working directory based on its `.git` entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RepoPathKind {
    /// Normal standalone / main working tree (`.git` is a directory, or unknown).
    Standalone,
    /// Git submodule (`.git` file points into `.../.git/modules/...` or
    /// `.../.git/worktrees/<wt>/modules/...`), with its nearest ancestor
    /// repository working tree when present.
    Submodule { parent: Option<PathBuf> },
    /// Linked working tree (`.git` file points into `.../.git/worktrees/<name>`),
    /// with the non-bare main working tree when present.
    LinkedWorktree { main_workdir: Option<PathBuf> },
}

fn normalize_lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push(comp);
                }
            }
            other => out.push(other),
        }
    }
    out
}

/// Parses a `.git` gitlink file (`gitdir: <path>`) and resolves it against `workdir`.
fn resolve_gitdir_file(workdir: &Path) -> Option<PathBuf> {
    let dot_git = workdir.join(".git");
    if !dot_git.is_file() {
        return None;
    }
    let content = std::fs::read_to_string(&dot_git).ok()?;
    let line = content.lines().next()?.trim();
    let target = line.strip_prefix("gitdir:")?.trim();
    if target.is_empty() {
        return None;
    }
    let raw = Path::new(target);
    let resolved = if raw.is_absolute() {
        raw.to_path_buf()
    } else {
        workdir.join(raw)
    };
    Some(normalize_lexical(&resolved))
}

fn find_nearest_parent_repo(workdir: &Path) -> Option<PathBuf> {
    workdir
        .ancestors()
        .skip(1)
        .find(|dir| dir.join(".git").exists())
        .map(Path::to_path_buf)
}

/// Classifies `workdir` by inspecting its `.git` file (when present).
pub fn classify_repo_path(workdir: &Path) -> RepoPathKind {
    let Some(gitdir) = resolve_gitdir_file(workdir) else {
        return RepoPathKind::Standalone;
    };
    let comps: Vec<Component<'_>> = gitdir.components().collect();
    let git_idx = comps.iter().rposition(|c| match c {
        Component::Normal(name) => {
            let s = name.to_string_lossy();
            s == ".git" || s.ends_with(".git")
        }
        _ => false,
    });
    let Some(git_idx) = git_idx else {
        return RepoPathKind::Standalone;
    };
    let after: Vec<String> = comps[git_idx + 1..]
        .iter()
        .filter_map(|c| match c {
            Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect();

    if after.first().map(String::as_str) == Some("modules")
        || (after.first().map(String::as_str) == Some("worktrees")
            && after.get(2).map(String::as_str) == Some("modules"))
    {
        return RepoPathKind::Submodule {
            parent: find_nearest_parent_repo(workdir),
        };
    }

    if after.len() == 2 && after[0] == "worktrees" {
        let is_dot_git = matches!(
            comps[git_idx],
            Component::Normal(name) if name == ".git"
        );
        let main_workdir = if is_dot_git && git_idx > 0 {
            let mut parent = PathBuf::new();
            for c in &comps[..git_idx] {
                parent.push(c);
            }
            parent.is_dir().then_some(parent)
        } else {
            None
        };
        return RepoPathKind::LinkedWorktree { main_workdir };
    }

    RepoPathKind::Standalone
}

/// Walks out of submodules and linked worktrees to find the standalone parent
/// repository that belongs in the global `recent-repos.toml` list.
pub fn standalone_recent_target(workdir: &Path) -> Option<PathBuf> {
    let mut cur = workdir.to_path_buf();
    for _ in 0..8 {
        match classify_repo_path(&cur) {
            RepoPathKind::Standalone => return Some(cur),
            RepoPathKind::Submodule { parent } => cur = parent?,
            RepoPathKind::LinkedWorktree { main_workdir } => cur = main_workdir?,
        }
    }
    None
}

/// Upper bound of remembered repositories.
pub const MAX_RECENT_REPOS: usize = 20;
/// File name of the store inside the user configuration directory.
pub const RECENT_REPOS_FILE: &str = "recent-repos.toml";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecentRepo {
    pub path: PathBuf,
    pub name: String,
    /// Unix seconds of the last time the repository was opened.
    pub last_opened: i64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct RecentReposFile {
    #[serde(default)]
    repos: Vec<RecentRepo>,
}

/// Most-recently-opened repositories (newest first, ≤ [`MAX_RECENT_REPOS`]).
/// `file = None` keeps the list in memory only (tests).
#[derive(Debug, Default, Clone)]
pub struct RecentRepos {
    file: Option<PathBuf>,
    entries: Vec<RecentRepo>,
    /// Names of entries dropped because their directory no longer exists
    /// (reported once by the switcher).
    pruned: Vec<String>,
}

impl RecentRepos {
    pub fn in_memory() -> Self {
        Self::default()
    }

    /// `$JBGIT_CONFIG_DIR/recent-repos.toml`, else the user config directory.
    pub fn default_file() -> PathBuf {
        std::env::var_os(crate::config::discovery::JBGIT_CONFIG_DIR_ENV)
            .map(PathBuf::from)
            .unwrap_or_else(crate::config::discovery::global_config_dir)
            .join(RECENT_REPOS_FILE)
    }

    /// Loads `file`, dropping entries whose directory no longer exists or that
    /// belong to a submodule / linked worktree (those are shown under their own
    /// project-local tags instead of the global recent list).
    pub fn load(file: PathBuf) -> Self {
        let parsed: RecentReposFile = std::fs::read_to_string(&file)
            .ok()
            .and_then(|t| toml::from_str(&t).ok())
            .unwrap_or_default();
        let mut store = Self {
            file: Some(file),
            entries: Vec::new(),
            pruned: Vec::new(),
        };
        let mut entries = parsed.repos;
        entries.sort_by_key(|a| std::cmp::Reverse(a.last_opened));
        let mut cleaned = false;
        for e in entries {
            if !e.path.is_dir() {
                store.pruned.push(e.name);
            } else if !matches!(classify_repo_path(&e.path), RepoPathKind::Standalone) {
                cleaned = true;
            } else if !store.entries.iter().any(|x| same_path(&x.path, &e.path)) {
                store.entries.push(e);
            }
        }
        store.entries.truncate(MAX_RECENT_REPOS);
        if !store.pruned.is_empty() || cleaned {
            store.save();
        }
        store
    }

    pub fn entries(&self) -> &[RecentRepo] {
        &self.entries
    }

    /// Moves `path` to the front (inserting it when new) and persists.
    pub fn record(&mut self, path: &Path, name: &str) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() as i64);
        self.entries.retain(|e| !same_path(&e.path, path));
        self.entries.insert(
            0,
            RecentRepo {
                path: path.to_path_buf(),
                name: name.to_string(),
                last_opened: now,
            },
        );
        self.entries.truncate(MAX_RECENT_REPOS);
        self.save();
    }

    /// Removes `path`; returns whether it was listed.
    pub fn forget(&mut self, path: &Path) -> bool {
        let before = self.entries.len();
        self.entries.retain(|e| !same_path(&e.path, path));
        let removed = self.entries.len() != before;
        if removed {
            self.save();
        }
        removed
    }

    pub fn take_pruned(&mut self) -> Vec<String> {
        std::mem::take(&mut self.pruned)
    }

    fn save(&self) {
        let Some(file) = &self.file else { return };
        let body = RecentReposFile {
            repos: self.entries.clone(),
        };
        if let Ok(text) = toml::to_string_pretty(&body) {
            if let Some(dir) = file.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::write(file, text);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwitcherSource {
    Parent,
    Worktree,
    Submodule,
    Recent,
}

impl SwitcherSource {
    pub const fn is_project_local(self) -> bool {
        matches!(self, Self::Parent | Self::Worktree | Self::Submodule)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SwitcherItem {
    pub source: SwitcherSource,
    pub name: String,
    pub path: PathBuf,
    /// Branch of a worktree, or the path for recent repositories / submodules.
    pub detail: String,
}

/// State of [`ModalLayer::RepoSwitcher`]; the item list is captured when the
/// modal opens (no filesystem access while rendering).
#[derive(Debug, Clone, Default)]
pub struct RepoSwitcherState {
    pub query: String,
    pub selected: usize,
    pub scroll: ScrollViewport,
    pub items: Vec<SwitcherItem>,
}

impl RepoSwitcherState {
    /// Indices into `items` matching the query (best fuzzy score first,
    /// source order kept on ties).
    pub fn visible(&self) -> Vec<usize> {
        if self.query.trim().is_empty() {
            return (0..self.items.len()).collect();
        }
        let mut scored: Vec<(u32, usize)> = self
            .items
            .iter()
            .enumerate()
            .filter_map(|(i, it)| {
                let by_name = fuzzy_indices(&it.name, &self.query).map(|s| s.0);
                let by_path =
                    fuzzy_indices(&it.path.to_string_lossy(), &self.query).map(|s| s.0 / 2);
                by_name.max(by_path).map(|s| (s, i))
            })
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        scored.into_iter().map(|(_, i)| i).collect()
    }

    pub fn selected_item(&self) -> Option<&SwitcherItem> {
        let vis = self.visible();
        let idx = *vis.get(self.selected.min(vis.len().saturating_sub(1)))?;
        self.items.get(idx)
    }

    /// Whether a horizontal divider separates project-local targets from global recent repos.
    pub fn shows_divider(&self, vis: &[usize]) -> bool {
        self.query.trim().is_empty()
            && vis.iter().any(|i| self.items[*i].source.is_project_local())
            && vis
                .iter()
                .any(|i| self.items[*i].source == SwitcherSource::Recent)
    }

    /// `(display_row_of_selected, total_display_rows)` accounting for the optional divider.
    pub fn display_selection_and_len(&self, vis: &[usize]) -> (usize, usize) {
        if vis.is_empty() {
            return (0, 0);
        }
        let sel = self.selected.min(vis.len() - 1);
        if self.shows_divider(vis) {
            let extra = usize::from(self.items[vis[sel]].source == SwitcherSource::Recent);
            (sel + extra, vis.len() + 1)
        } else {
            (sel, vis.len())
        }
    }
}

/// Project-local targets first (parent repository when inside a submodule,
/// other working trees, initialized submodules), followed by global recent
/// standalone repositories (the current working tree is omitted).
pub fn build_switcher_items(state: &AppState) -> Vec<SwitcherItem> {
    let current = state
        .repo_snapshot
        .as_ref()
        .map_or_else(|| state.repo_path_display.clone(), |s| s.workdir.clone());
    let mut items: Vec<SwitcherItem> = Vec::new();
    let push = |items: &mut Vec<SwitcherItem>, item: SwitcherItem| {
        if !same_path(&item.path, &current) && !items.iter().any(|x| same_path(&x.path, &item.path))
        {
            items.push(item);
        }
    };

    if let RepoPathKind::Submodule {
        parent: Some(parent_path),
    } = classify_repo_path(&current)
    {
        let name = parent_path.file_name().map_or_else(
            || parent_path.display().to_string(),
            |n| n.to_string_lossy().into(),
        );
        let detail = parent_path.display().to_string();
        push(
            &mut items,
            SwitcherItem {
                source: SwitcherSource::Parent,
                name,
                path: parent_path,
                detail,
            },
        );
    }

    if let Some(snap) = state.repo_snapshot.as_deref() {
        let detached = state.i18n.t("view.repo_switcher.detached");
        for wt in snap
            .worktrees
            .iter()
            .filter(|w| !w.is_bare && !w.is_prunable())
        {
            let detail = match (&wt.branch, wt.head) {
                (Some(b), _) => b.clone(),
                (None, Some(h)) => format!("{detached} {}", h.short()),
                (None, None) => detached.to_string(),
            };
            push(
                &mut items,
                SwitcherItem {
                    source: SwitcherSource::Worktree,
                    name: wt.name(),
                    path: wt.path.clone(),
                    detail,
                },
            );
        }
        for sub in &snap.submodules {
            let path = snap.workdir.join(sub);
            if path.join(".git").exists() {
                let detail = path.display().to_string();
                push(
                    &mut items,
                    SwitcherItem {
                        source: SwitcherSource::Submodule,
                        name: sub.clone(),
                        path,
                        detail,
                    },
                );
            }
        }
    }

    for r in state.recent_repos.entries() {
        if !matches!(classify_repo_path(&r.path), RepoPathKind::Standalone) {
            continue;
        }
        push(
            &mut items,
            SwitcherItem {
                source: SwitcherSource::Recent,
                name: r.name.clone(),
                path: r.path.clone(),
                detail: r.path.display().to_string(),
            },
        );
    }

    items
}

/// `Ctrl+O` — open any path (resolved against the current repository's
/// parent directory when relative).
pub fn open_path_dialog(i18n: &I18n) -> Dialog {
    Dialog::Input(InputDialog::new(
        i18n.t("dialog.repo_switcher.open_path_title"),
        i18n.t("dialog.repo_switcher.open_path_prompt"),
        "",
        InputValidation::NonEmpty,
        InputIntent::OpenRepositoryPath,
    ))
}

fn top(state: &AppState) -> Option<&RepoSwitcherState> {
    match state.modal_stack.last() {
        Some(ModalLayer::RepoSwitcher(rs)) => Some(rs),
        _ => None,
    }
}

fn set(query: String, selected: usize) -> Vec<Action> {
    vec![Action::RepoSwitcherSetState { query, selected }]
}

/// Actions activating visible row `row`.
pub fn activate_row(rs: &RepoSwitcherState, row: usize) -> Vec<Action> {
    let vis = rs.visible();
    match vis.get(row).and_then(|i| rs.items.get(*i)) {
        Some(item) => vec![
            Action::PopModal,
            Action::SwitchRepository(item.path.clone()),
        ],
        None => Vec::new(),
    }
}

/// Keyboard routing while the switcher is the topmost layer.
pub fn route_repo_switcher_key(key: &KeyEvent, state: &AppState) -> Vec<Action> {
    let Some(rs) = top(state) else {
        return Vec::new();
    };
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let len = rs.visible().len();
    let sel = rs.selected.min(len.saturating_sub(1));
    let query = rs.query.clone();
    match key.code {
        KeyCode::Esc => vec![Action::PopModal],
        KeyCode::Enter => activate_row(rs, sel),
        KeyCode::Char('o') if ctrl => vec![
            Action::PopModal,
            Action::OpenDialog(Box::new(open_path_dialog(&state.i18n))),
        ],
        KeyCode::Char('l') if ctrl => vec![
            Action::PopModal,
            Action::OpenCloneRepo { base_dir: None },
        ],
        KeyCode::Char('u') if ctrl => set(String::new(), 0),
        KeyCode::Up => set(query, sel.saturating_sub(1)),
        KeyCode::Char('p') if ctrl => set(query, sel.saturating_sub(1)),
        KeyCode::Down => set(query, (sel + 1).min(len.saturating_sub(1))),
        KeyCode::Char('n') if ctrl => set(query, (sel + 1).min(len.saturating_sub(1))),
        KeyCode::Home => set(query, 0),
        KeyCode::End => set(query, len.saturating_sub(1)),
        KeyCode::Delete => match rs.selected_item() {
            Some(item) if item.source == SwitcherSource::Recent => {
                vec![Action::ForgetRecentRepo(item.path.clone())]
            }
            _ => Vec::new(),
        },
        KeyCode::Backspace => {
            let mut q = query;
            q.pop();
            set(q, 0)
        }
        KeyCode::Char(c) if !ctrl && !alt => {
            let mut q = query;
            q.push(c);
            set(q, 0)
        }
        _ => Vec::new(),
    }
}

pub fn render_repo_switcher_modal(
    frame: &mut Frame<'_>,
    screen: Rect,
    state: &AppState,
    rs: &RepoSwitcherState,
    ctx: &mut RenderContext<'_>,
    z: u8,
) {
    if screen.width < 24 || screen.height < 8 {
        return;
    }
    let vis = rs.visible();
    let show_divider = rs.shows_divider(&vis);
    let (sel_display, total_rows) = rs.display_selection_and_len(&vis);

    let w = screen
        .width
        .saturating_sub(4)
        .clamp(24.min(screen.width), 100);
    let h = (total_rows.max(1) as u16 + 6)
        .clamp(9, 26)
        .min(screen.height.saturating_sub(2).max(6));
    let rect = Rect::new(
        screen.x + screen.width.saturating_sub(w) / 2,
        screen.y + screen.height.saturating_sub(h) / 2,
        w,
        h,
    );
    draw_shadow(frame, screen, rect);
    frame.render_widget(Clear, rect);
    let current = state
        .repo_snapshot
        .as_ref()
        .map_or("jbgit", |s| s.repo_name.as_str());
    let title = format!(
        " {} ",
        ctx.t_fmt("view.repo_switcher.title", &[("name", current)])
    );
    let block = Block::default()
        .title(Span::styled(
            title,
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
    if inner.height < 4 || inner.width < 10 {
        return;
    }
    let width = inner.width as usize;

    let query_line = Line::from(vec![
        Span::styled(" ❯ ", Style::default().fg(ctx.theme.fg_accent_link)),
        Span::styled(rs.query.clone(), Style::default().fg(ctx.theme.fg_bright)),
        Span::styled("▏", Style::default().fg(ctx.theme.fg_muted)),
    ]);
    frame.render_widget(
        Paragraph::new(query_line),
        Rect::new(inner.x, inner.y, inner.width, 1),
    );

    let list_top = inner.y + 1;
    let list_h = inner.height.saturating_sub(2) as usize;
    if vis.is_empty() {
        ctx.record_scroll_bound(ScrollPane::RepoSwitcher, list_h, 0);
        frame.render_widget(
            Paragraph::new(Span::styled(
                format!(" {}", ctx.t("view.repo_switcher.empty")),
                Style::default().fg(ctx.theme.fg_muted),
            )),
            Rect::new(inner.x, list_top, inner.width, 1),
        );
    } else {
        enum DisplayRow {
            Item(usize),
            Separator,
        }
        let mut display_rows = Vec::with_capacity(total_rows);
        let mut inserted_sep = false;
        for (row, &idx) in vis.iter().enumerate() {
            if show_divider
                && !inserted_sep
                && rs.items[idx].source == SwitcherSource::Recent
            {
                display_rows.push(DisplayRow::Separator);
                inserted_sep = true;
            }
            display_rows.push(DisplayRow::Item(row));
        }

        let sel = rs.selected.min(vis.len() - 1);
        let mut vp = rs.scroll;
        vp.ensure_visible(sel_display, display_rows.len(), list_h);
        let first = vp.offset();
        ctx.record_scroll_bound(
            ScrollPane::RepoSwitcher,
            list_h,
            display_rows.len().saturating_sub(list_h),
        );
        let tags = [
            ctx.t("view.repo_switcher.tag_parent").to_string(),
            ctx.t("view.repo_switcher.tag_worktree").to_string(),
            ctx.t("view.repo_switcher.tag_submodule").to_string(),
            ctx.t("view.repo_switcher.tag_recent").to_string(),
        ];
        let tag_of = |s: SwitcherSource| match s {
            SwitcherSource::Parent => (&tags[0], ctx.theme.ref_head.icon),
            SwitcherSource::Worktree => (&tags[1], ctx.theme.ref_local.icon),
            SwitcherSource::Submodule => (&tags[2], ctx.theme.fg_accent_link),
            SwitcherSource::Recent => (&tags[3], ctx.theme.fg_muted),
        };
        let tag_w = tags.iter().map(|t| t.width()).max().unwrap_or(8);
        let name_w = vis
            .iter()
            .map(|i| rs.items[*i].name.width())
            .max()
            .unwrap_or(0)
            .clamp(8, 32);
        for (d_idx, d_row) in display_rows.iter().enumerate().skip(first).take(list_h) {
            let y = list_top + (d_idx - first) as u16;
            let row_rect = Rect::new(inner.x, y, inner.width, 1);
            match *d_row {
                DisplayRow::Separator => {
                    let rule = format!(" {} ", "─".repeat(width.saturating_sub(2)));
                    frame.render_widget(
                        Paragraph::new(Span::styled(
                            rule,
                            Style::default()
                                .fg(ctx.theme.border_splitter)
                                .bg(ctx.theme.bg_elevated),
                        )),
                        row_rect,
                    );
                }
                DisplayRow::Item(row) => {
                    let item = &rs.items[vis[row]];
                    let is_sel = row == sel;
                    let bg = if is_sel {
                        ctx.theme.bg_selection_focused
                    } else {
                        ctx.theme.bg_elevated
                    };
                    let (tag, tag_fg) = tag_of(item.source);
                    let name = truncate_to_width(&item.name, name_w);
                    let detail_w = width.saturating_sub(tag_w + name_w + 6);
                    let line = Line::from(vec![
                        Span::styled(
                            format!(" {tag}{} ", " ".repeat(tag_w.saturating_sub(tag.width()))),
                            Style::default().fg(tag_fg),
                        ),
                        Span::styled(
                            format!(
                                "{name}{}  ",
                                " ".repeat(name_w.saturating_sub(name.width()))
                            ),
                            Style::default()
                                .fg(ctx.theme.fg_bright)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(
                            truncate_to_width(&item.detail, detail_w),
                            Style::default().fg(ctx.theme.fg_primary),
                        ),
                    ]);
                    frame.render_widget(
                        Paragraph::new(line).style(Style::default().bg(bg)),
                        row_rect,
                    );
                    ctx.hit_registry
                        .register(row_rect, z + 2, HitTarget::RepoSwitcherRow(row));
                }
            }
        }
    }

    let hint = ctx.t("view.repo_switcher.hint").to_string();
    frame.render_widget(
        Paragraph::new(Span::styled(
            truncate_to_width(&format!(" {hint}"), width),
            Style::default().fg(ctx.theme.fg_muted),
        )),
        Rect::new(inner.x, inner.bottom().saturating_sub(1), inner.width, 1),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recent_repos_persist_prune_and_cap() {
        let tmp = tempfile::TempDir::new().unwrap();
        let file = tmp.path().join("cfg").join(RECENT_REPOS_FILE);
        let a = tmp.path().join("a");
        let b = tmp.path().join("b");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();

        let mut store = RecentRepos::load(file.clone());
        assert!(store.entries().is_empty());
        store.record(&a, "a");
        store.record(&b, "b");
        store.record(&a, "a");
        let names: Vec<&str> = store.entries().iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["a", "b"]);

        std::fs::remove_dir_all(&b).unwrap();
        let mut reloaded = RecentRepos::load(file.clone());
        assert_eq!(reloaded.entries().len(), 1);
        assert_eq!(reloaded.take_pruned(), vec!["b".to_string()]);
        assert!(reloaded.take_pruned().is_empty());
        // The pruned list was written back.
        assert_eq!(RecentRepos::load(file.clone()).entries().len(), 1);

        for i in 0..30 {
            let p = tmp.path().join(format!("r{i}"));
            std::fs::create_dir_all(&p).unwrap();
            reloaded.record(&p, &format!("r{i}"));
        }
        assert_eq!(reloaded.entries().len(), MAX_RECENT_REPOS);
        assert_eq!(reloaded.entries()[0].name, "r29");
        assert!(reloaded.forget(&tmp.path().join("r29")));
        assert!(!reloaded.forget(&tmp.path().join("r29")));
        assert_eq!(RecentRepos::load(file).entries()[0].name, "r28");
    }

    #[test]
    fn switcher_filter_prefers_name_matches() {
        let item = |name: &str, path: &str| SwitcherItem {
            source: SwitcherSource::Recent,
            name: name.into(),
            path: PathBuf::from(path),
            detail: String::new(),
        };
        let mut rs = RepoSwitcherState {
            query: String::new(),
            selected: 0,
            scroll: ScrollViewport::new(),
            items: vec![
                item("alpha", "/src/alpha"),
                item("beta", "/src/beta"),
                item("jbgit-feature", "/src/jbgit-feature"),
            ],
        };
        assert_eq!(rs.visible(), vec![0, 1, 2]);
        rs.query = "feat".into();
        assert_eq!(rs.visible(), vec![2]);
        assert_eq!(
            rs.selected_item().map(|i| i.name.as_str()),
            Some("jbgit-feature")
        );
        rs.query = "zzz".into();
        assert!(rs.visible().is_empty());
        assert!(rs.selected_item().is_none());
    }

    #[test]
    fn classify_repo_path_distinguishes_standalone_submodule_and_worktree() {
        let tmp = tempfile::TempDir::new().unwrap();
        let main_repo = tmp.path().join("modules").join("all2api");
        let sub_repo = main_repo.join("third_party").join("qoder2api");
        let nested_sub = sub_repo.join("vendor").join("inner");
        let wt_repo = tmp.path().join("modules").join("all2api-feat");
        let wt_sub = wt_repo.join("third_party").join("qoder2api");

        std::fs::create_dir_all(main_repo.join(".git/modules/third_party/qoder2api/modules/vendor/inner")).unwrap();
        std::fs::create_dir_all(main_repo.join(".git/worktrees/feat/modules/qoder2api")).unwrap();
        std::fs::create_dir_all(&sub_repo).unwrap();
        std::fs::create_dir_all(&nested_sub).unwrap();
        std::fs::create_dir_all(&wt_repo).unwrap();
        std::fs::create_dir_all(&wt_sub).unwrap();

        std::fs::write(
            sub_repo.join(".git"),
            "gitdir: ../../.git/modules/third_party/qoder2api\n",
        )
        .unwrap();
        std::fs::write(
            nested_sub.join(".git"),
            "gitdir: ../../../../.git/modules/third_party/qoder2api/modules/vendor/inner\n",
        )
        .unwrap();
        std::fs::write(
            wt_repo.join(".git"),
            format!(
                "gitdir: {}\n",
                main_repo.join(".git/worktrees/feat").display()
            ),
        )
        .unwrap();
        std::fs::write(
            wt_sub.join(".git"),
            format!(
                "gitdir: {}\n",
                main_repo
                    .join(".git/worktrees/feat/modules/qoder2api")
                    .display()
            ),
        )
        .unwrap();

        assert_eq!(classify_repo_path(&main_repo), RepoPathKind::Standalone);
        assert_eq!(
            classify_repo_path(&sub_repo),
            RepoPathKind::Submodule {
                parent: Some(main_repo.clone())
            }
        );
        assert_eq!(
            classify_repo_path(&nested_sub),
            RepoPathKind::Submodule {
                parent: Some(sub_repo.clone())
            }
        );
        // Even though the ancestor folder is literally named `modules`, `wt_repo` is a worktree.
        assert_eq!(
            classify_repo_path(&wt_repo),
            RepoPathKind::LinkedWorktree {
                main_workdir: Some(main_repo.clone())
            }
        );
        assert_eq!(
            classify_repo_path(&wt_sub),
            RepoPathKind::Submodule {
                parent: Some(wt_repo.clone())
            }
        );

        assert_eq!(standalone_recent_target(&main_repo), Some(main_repo.clone()));
        assert_eq!(standalone_recent_target(&sub_repo), Some(main_repo.clone()));
        assert_eq!(standalone_recent_target(&nested_sub), Some(main_repo.clone()));
        assert_eq!(standalone_recent_target(&wt_repo), Some(main_repo.clone()));
        assert_eq!(standalone_recent_target(&wt_sub), Some(main_repo.clone()));

        // RecentRepos::load silently strips submodule and linked worktree entries.
        let file = tmp.path().join("cfg").join(RECENT_REPOS_FILE);
        let mut store = RecentRepos::load(file.clone());
        store.record(&main_repo, "all2api");
        store.record(&sub_repo, "qoder2api");
        store.record(&wt_repo, "all2api-feat");
        let mut reloaded = RecentRepos::load(file);
        let names: Vec<&str> = reloaded.entries().iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["all2api"]);
        assert!(reloaded.take_pruned().is_empty());
    }
}
