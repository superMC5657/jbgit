//! Repository switcher (Spec 12 Issue 09, `GitRepositoryManager` analogue
//! for a single active repository): the user-level recent repositories store
//! (`recent-repos.toml`) and the `RepoSwitcher` modal listing recent
//! repositories, linked working trees and submodules with fuzzy filtering.

use std::path::{Path, PathBuf};

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

    /// Loads `file`, dropping entries whose directory no longer exists.
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
        for e in entries {
            if !e.path.is_dir() {
                store.pruned.push(e.name);
            } else if !store.entries.iter().any(|x| same_path(&x.path, &e.path)) {
                store.entries.push(e);
            }
        }
        store.entries.truncate(MAX_RECENT_REPOS);
        if !store.pruned.is_empty() {
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
    Recent,
    Worktree,
    Submodule,
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
}

/// Recent repositories, then the current repository's other working trees,
/// then its initialized submodules (the current working tree is omitted).
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
    for r in state.recent_repos.entries() {
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
                push(
                    &mut items,
                    SwitcherItem {
                        source: SwitcherSource::Submodule,
                        name: sub.clone(),
                        path,
                        detail: sub.clone(),
                    },
                );
            }
        }
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
    let w = screen
        .width
        .saturating_sub(4)
        .clamp(24.min(screen.width), 100);
    let h = (vis.len().max(1) as u16 + 6)
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
        let sel = rs.selected.min(vis.len() - 1);
        let mut vp = rs.scroll;
        vp.ensure_visible(sel, vis.len(), list_h);
        let first = vp.offset();
        ctx.record_scroll_bound(
            ScrollPane::RepoSwitcher,
            list_h,
            vis.len().saturating_sub(list_h),
        );
        let tags = [
            ctx.t("view.repo_switcher.tag_recent").to_string(),
            ctx.t("view.repo_switcher.tag_worktree").to_string(),
            ctx.t("view.repo_switcher.tag_submodule").to_string(),
        ];
        let tag_of = |s: SwitcherSource| match s {
            SwitcherSource::Recent => tags[0].clone(),
            SwitcherSource::Worktree => tags[1].clone(),
            SwitcherSource::Submodule => tags[2].clone(),
        };
        let tag_w = tags.iter().map(|t| t.width()).max().unwrap_or(8);
        let name_w = vis
            .iter()
            .map(|i| rs.items[*i].name.width())
            .max()
            .unwrap_or(0)
            .clamp(8, 32);
        for (row, idx) in vis.iter().enumerate().skip(first).take(list_h) {
            let item = &rs.items[*idx];
            let y = list_top + (row - first) as u16;
            let is_sel = row == sel;
            let bg = if is_sel {
                ctx.theme.bg_selection_focused
            } else {
                ctx.theme.bg_elevated
            };
            let tag = tag_of(item.source);
            let name = truncate_to_width(&item.name, name_w);
            let detail_w = width.saturating_sub(tag_w + name_w + 6);
            let line = Line::from(vec![
                Span::styled(
                    format!(" {tag}{} ", " ".repeat(tag_w.saturating_sub(tag.width()))),
                    Style::default().fg(ctx.theme.fg_muted),
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
            let row_rect = Rect::new(inner.x, y, inner.width, 1);
            frame.render_widget(
                Paragraph::new(line).style(Style::default().bg(bg)),
                row_rect,
            );
            ctx.hit_registry
                .register(row_rect, z + 2, HitTarget::RepoSwitcherRow(row));
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
}
