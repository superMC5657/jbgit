//! `GitBranchesPopup`: global branch speed-search popup with
//! `nucleo` fuzzy filtering + match highlighting, reflog `Recent` section,
//! local / remote grouping, ★ favorites, `↑↓` sync counters, ongoing
//! operation actions and cascading per-branch action submenus.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use jbgit_core::{Oid, OngoingKind, RefKind, fuzzy_indices, natural_cmp};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};
use unicode_width::UnicodeWidthStr;

use crate::app::{Action, AppState, MenuParent, ModalLayer, RenderContext};
use crate::config::KeymapScope;
use crate::dialogs::{Dialog, InputDialog, InputIntent, InputValidation, new_branch_from_head_dialog};
use crate::hit_test::HitTarget;
use crate::keymap::{KeyAction, KeySequence};
use crate::menus::{branch_menu_items, ongoing_menu_items};
use crate::views::truncate_to_width;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PopupBranch {
    pub name: String,
    pub kind: RefKind,
    pub target: Oid,
    pub is_head: bool,
    pub is_favorite: bool,
    pub upstream: Option<String>,
    pub ahead_behind: Option<(usize, usize)>,
    /// Matched char indices of `name` (highlighted in `#548AF7` bold).
    pub indices: Vec<u32>,
    pub indent: u16,
    pub full_ref: String,
    pub short: String,
    /// Another working tree has this local branch checked out (its name).
    pub worktree: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PopupRow {
    Header(String),
    Folder(String),
    Ongoing(OngoingKind),
    UpdateProject,
    NewBranch,
    CheckoutRevision,
    ConfigureRemotes,
    Worktrees,
    Unshallow,
    Branch(PopupBranch),
}

impl PopupRow {
    pub fn selectable(&self) -> bool {
        !matches!(self, Self::Header(_) | Self::Folder(_))
    }
}

fn branch_row(
    state: &AppState,
    r: &jbgit_core::RefInfo,
    query: &str,
    indent: u16,
) -> Option<(u32, PopupBranch)> {
    let (score, indices) = fuzzy_indices(&r.name, query)?;
    let short = if r.kind == RefKind::RemoteBranch {
        r.name
            .split_once('/')
            .map_or(r.name.clone(), |(_, s)| s.to_string())
    } else {
        r.name.clone()
    };
    let is_favorite = r.kind != RefKind::Tag
        && state
            .log
            .branches
            .settings
            .favorites
            .is_favorite(&r.full_name, &short);
    Some((
        score,
        PopupBranch {
            name: r.name.clone(),
            kind: r.kind,
            target: r.target_oid,
            is_head: r.is_head,
            is_favorite,
            upstream: r.upstream.clone(),
            ahead_behind: r.ahead_behind,
            indices,
            indent,
            full_ref: r.full_name.clone(),
            short,
            worktree: (r.kind == RefKind::LocalBranch)
                .then_some(state.repo_snapshot.as_deref())
                .flatten()
                .and_then(|s| s.worktree_of_branch(&r.name))
                .map(jbgit_core::WorktreeInfo::name),
        },
    ))
}

/// Builds the popup rows for `query` (empty query = grouped tree view).
pub fn build_popup_rows(state: &AppState, query: &str) -> Vec<PopupRow> {
    let mut rows = Vec::new();
    let Some(snap) = state.repo_snapshot.as_deref() else {
        return rows;
    };
    let q = query.trim();
    if let Some(kind) = OngoingKind::from_state(&snap.state) {
        rows.push(PopupRow::Ongoing(kind));
    }
    if q.is_empty() {
        if snap.head_branch.is_some() {
            rows.push(PopupRow::UpdateProject);
        }
        rows.push(PopupRow::NewBranch);
        rows.push(PopupRow::CheckoutRevision);
        rows.push(PopupRow::ConfigureRemotes);
        if crate::worktrees_view::worktrees_supported(state) {
            rows.push(PopupRow::Worktrees);
        }
        if snap.is_shallow {
            rows.push(PopupRow::Unshallow);
        }
    }

    let sort_scored = |v: &mut Vec<(u32, PopupBranch)>| {
        if q.is_empty() {
            v.sort_by(|a, b| {
                b.1.is_head
                    .cmp(&a.1.is_head)
                    .then(b.1.is_favorite.cmp(&a.1.is_favorite))
                    .then_with(|| natural_cmp(&a.1.name, &b.1.name))
            });
        } else {
            v.sort_by(|a, b| {
                b.0.cmp(&a.0)
                    .then_with(|| natural_cmp(&a.1.name, &b.1.name))
            });
        }
    };

    // Recent (reflog).
    let recent: Vec<PopupBranch> = snap
        .recent_branches
        .iter()
        .filter_map(|n| snap.local_branch(n))
        .filter_map(|r| branch_row(state, r, q, 1).map(|(_, b)| b))
        .collect();
    if !recent.is_empty() {
        rows.push(PopupRow::Header(state.i18n.t("view.branches_popup.recent").into()));
        rows.extend(recent.into_iter().map(PopupRow::Branch));
    }

    let push_section = |title: &str, kind: RefKind, group: bool, rows: &mut Vec<PopupRow>| {
        let mut items: Vec<(u32, PopupBranch)> = snap
            .refs
            .iter()
            .filter(|r| r.kind == kind)
            .filter_map(|r| branch_row(state, r, q, 1))
            .collect();
        if items.is_empty() {
            return;
        }
        sort_scored(&mut items);
        rows.push(PopupRow::Header(title.to_string()));
        if !group || !q.is_empty() {
            rows.extend(items.into_iter().map(|(_, b)| PopupRow::Branch(b)));
            return;
        }
        // Group `feature/*` style names under a folder row (top-level only).
        let folder_of = |b: &PopupBranch| -> Option<String> {
            let name = if kind == RefKind::RemoteBranch {
                b.short.as_str()
            } else {
                b.name.as_str()
            };
            name.split_once('/').map(|(f, _)| f.to_string())
        };
        let (flat, nested): (Vec<_>, Vec<_>) = items
            .into_iter()
            .map(|(_, b)| b)
            .partition(|b| b.is_head || b.is_favorite || folder_of(b).is_none());
        rows.extend(flat.into_iter().map(PopupRow::Branch));
        let mut folders: Vec<String> = nested.iter().filter_map(folder_of).collect();
        folders.dedup();
        folders.sort_by(|a, b| natural_cmp(a, b));
        folders.dedup();
        for f in folders {
            rows.push(PopupRow::Folder(f.clone()));
            for b in nested
                .iter()
                .filter(|b| folder_of(b).as_deref() == Some(f.as_str()))
            {
                let mut b = b.clone();
                b.indent = 2;
                rows.push(PopupRow::Branch(b));
            }
        }
    };
    let local_hdr = state.i18n.t("view.branches_popup.local");
    let remote_hdr = state.i18n.t("view.branches_popup.remote");
    let tags_hdr = state.i18n.t("view.branches_popup.tags");
    push_section(local_hdr, RefKind::LocalBranch, true, &mut rows);
    push_section(remote_hdr, RefKind::RemoteBranch, true, &mut rows);
    if !q.is_empty() {
        push_section(tags_hdr, RefKind::Tag, false, &mut rows);
    }
    rows
}

/// Initial selection: best-matching branch (or the first actionable row).
pub fn first_selectable_row(state: &AppState, query: &str) -> usize {
    let rows = build_popup_rows(state, query);
    let first_branch = |pred: &dyn Fn(&PopupBranch) -> bool| {
        rows.iter()
            .position(|r| matches!(r, PopupRow::Branch(b) if pred(b)))
    };
    if query.trim().is_empty() {
        first_branch(&|b| !b.is_head)
            .or_else(|| first_branch(&|_| true))
            .or_else(|| rows.iter().position(PopupRow::selectable))
            .unwrap_or(0)
    } else {
        // Highest score across Local / Remote / Tags (Recent repeats locals).
        let best = rows
            .iter()
            .enumerate()
            .filter_map(|(i, r)| match r {
                PopupRow::Branch(b) => Some((i, b)),
                _ => None,
            })
            .max_by(|(ia, a), (ib, b)| {
                let sa = fuzzy_indices(&a.name, query).map_or(0, |s| s.0);
                let sb = fuzzy_indices(&b.name, query).map_or(0, |s| s.0);
                sa.cmp(&sb).then(ib.cmp(ia))
            })
            .map(|(i, _)| i);
        best.or_else(|| rows.iter().position(PopupRow::selectable))
            .unwrap_or(0)
    }
}

pub struct PopupLayout {
    pub rect: Rect,
    pub list: Rect,
    pub scroll: usize,
}

pub fn popup_layout(screen: Rect, rows: usize, selected: usize) -> PopupLayout {
    let width = screen
        .width
        .saturating_sub(4)
        .clamp(20.min(screen.width), 68);
    let height = (rows as u16 + 4)
        .clamp(8, 26)
        .min(screen.height.saturating_sub(2).max(5));
    let x = screen.x + 2.min(screen.width.saturating_sub(width));
    let y = screen.y + 1.min(screen.height.saturating_sub(height));
    let rect = Rect::new(x, y, width, height);
    let list = Rect::new(
        rect.x + 1,
        rect.y + 3,
        rect.width.saturating_sub(2),
        rect.height.saturating_sub(4),
    );
    let visible = list.height as usize;
    let scroll = if visible == 0 || selected < visible {
        0
    } else {
        selected + 1 - visible
    };
    PopupLayout { rect, list, scroll }
}

fn move_selection(rows: &[PopupRow], from: usize, delta: isize) -> usize {
    let n = rows.len();
    if n == 0 {
        return 0;
    }
    let mut idx = from.min(n - 1);
    for _ in 0..n {
        idx = ((idx as isize + delta).rem_euclid(n as isize)) as usize;
        if rows[idx].selectable() {
            return idx;
        }
    }
    from
}

/// Actions for activating popup row `idx` (Enter / → / click).
pub fn activate_row(state: &AppState, query: &str, idx: usize) -> Vec<Action> {
    let rows = build_popup_rows(state, query);
    let Some(row) = rows.get(idx) else {
        return Vec::new();
    };
    let layout = popup_layout(state.terminal_area, rows.len(), idx);
    let row_y = layout.list.y + (idx.saturating_sub(layout.scroll)) as u16;
    let parent = MenuParent {
        rect: layout.rect,
        row_y,
        item_idx: idx,
    };
    let snap = state.repo_snapshot.as_deref();
    match row {
        PopupRow::Branch(b) => vec![Action::OpenSubmenu {
            parent,
            items: branch_menu_items(state, &b.name, b.kind),
            depth: 0,
        }],
        PopupRow::Ongoing(kind) => vec![Action::OpenSubmenu {
            parent,
            items: ongoing_menu_items(state, *kind),
            depth: 0,
        }],
        PopupRow::UpdateProject => vec![Action::PopModal, Action::PromptUpdateProject],
        PopupRow::ConfigureRemotes => vec![Action::PopModal, Action::OpenRemotes],
        PopupRow::Worktrees => vec![Action::PopModal, Action::OpenWorktrees],
        PopupRow::Unshallow => vec![Action::PopModal, Action::PromptUnshallow],
        PopupRow::NewBranch => vec![
            Action::PopModal,
            Action::OpenDialog(Box::new(new_branch_from_head_dialog(snap, &state.i18n))),
        ],
        PopupRow::CheckoutRevision => vec![
            Action::PopModal,
            Action::OpenDialog(Box::new(Dialog::Input(InputDialog::new(
                state.i18n.t("dialog.checkout_revision.title"),
                state.i18n.t("dialog.checkout_revision.prompt"),
                "",
                InputValidation::NonEmpty,
                InputIntent::CheckoutRevision,
            )))),
        ],
        PopupRow::Header(_) | PopupRow::Folder(_) => Vec::new(),
    }
}

/// Resolves the active, unshadowed single-key editor chord for `KeyAction::NewBranch`
/// inside the branches popup.
fn popup_new_branch_seq(state: &AppState) -> Option<KeySequence> {
    state.config.keymap.primary_single_editor_seq_for(
        &[KeymapScope::Branch, KeymapScope::Log, KeymapScope::Global],
        &KeyAction::NewBranch,
    )
}

/// Keyboard routing while the popup is the topmost layer (pure read).
pub fn route_branches_popup_key(key: &KeyEvent, state: &AppState) -> Vec<Action> {
    if popup_new_branch_seq(state).is_some_and(|seq| seq.matches_exact(&[*key])) {
        return vec![
            Action::PopModal,
            Action::OpenDialog(Box::new(new_branch_from_head_dialog(
                state.repo_snapshot.as_deref(),
                &state.i18n,
            ))),
        ];
    }
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let (query, selected) = match state.modal_stack.last() {
        Some(ModalLayer::BranchesPopup { query, selected }) => (query.clone(), *selected),
        _ => return Vec::new(),
    };
    let rows = build_popup_rows(state, &query);
    match key.code {
        KeyCode::Esc => vec![Action::PopModal],
        KeyCode::Up => vec![Action::PopupSetState {
            query,
            selected: move_selection(&rows, selected, -1),
        }],
        KeyCode::Down => vec![Action::PopupSetState {
            query,
            selected: move_selection(&rows, selected, 1),
        }],
        KeyCode::PageUp | KeyCode::PageDown => {
            let mut sel = selected;
            let d = if key.code == KeyCode::PageUp { -1 } else { 1 };
            for _ in 0..10 {
                sel = move_selection(&rows, sel, d);
            }
            vec![Action::PopupSetState {
                query,
                selected: sel,
            }]
        }
        KeyCode::Enter | KeyCode::Right => activate_row(state, &query, selected),
        KeyCode::Char('f') if alt => {
            vec![Action::PopModal, Action::RunGitOp(jbgit_core::GitOp::Fetch)]
        }
        KeyCode::Backspace => {
            let mut q = query;
            q.pop();
            let sel = first_selectable_row(state, &q);
            vec![Action::PopupSetState {
                query: q,
                selected: sel,
            }]
        }
        KeyCode::Char('u') if ctrl => {
            let sel = first_selectable_row(state, "");
            vec![Action::PopupSetState {
                query: String::new(),
                selected: sel,
            }]
        }
        KeyCode::Char(c) if !ctrl && !alt && !c.is_whitespace() => {
            let mut q = query;
            q.push(c);
            let sel = first_selectable_row(state, &q);
            vec![Action::PopupSetState {
                query: q,
                selected: sel,
            }]
        }
        _ => Vec::new(),
    }
}

pub fn render_branches_popup(
    frame: &mut Frame<'_>,
    screen: Rect,
    state: &AppState,
    ctx: &mut RenderContext<'_>,
    query: &str,
    selected: usize,
    z: u8,
) {
    let rows = build_popup_rows(state, query);
    let selected = if rows.get(selected).is_some_and(PopupRow::selectable) {
        selected
    } else {
        rows.iter().position(PopupRow::selectable).unwrap_or(0)
    };
    let layout = popup_layout(screen, rows.len(), selected);
    let rect = layout.rect;
    if rect.width < 10 || rect.height < 5 {
        return;
    }
    frame.render_widget(Clear, rect);
    let g = ctx.glyph_mode.symbols();
    let repo = state
        .repo_snapshot
        .as_ref()
        .map_or("jbgit", |s| s.repo_name.as_str());
    let block = Block::default()
        .title(Span::styled(
            format!(" {} ({repo}) ", state.i18n.t("view.branches_popup.title")),
            Style::default()
                .fg(ctx.theme.fg_bright)
                .add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(ctx.theme.border_focused))
        .style(Style::default().bg(ctx.theme.bg_elevated));
    frame.render_widget(block, rect);
    ctx.hit_registry
        .register(rect, z + 1, HitTarget::DialogBody);

    // Title-bar buttons: [↻ Fetch] [⚙]
    let fetch = state.i18n.t("view.branches_popup.fetch_btn");
    let gear = " ⚙ ";
    let gx = rect.right().saturating_sub(gear.width() as u16 + 2);
    let fx = gx.saturating_sub(fetch.width() as u16 + 1);
    if fx > rect.x + 24 {
        let btn = Style::default()
            .fg(ctx.theme.fg_accent_link)
            .bg(ctx.theme.bg_panel);
        frame.render_widget(
            Paragraph::new(Span::styled(fetch, btn)),
            Rect::new(fx, rect.y, fetch.width() as u16, 1),
        );
        frame.render_widget(
            Paragraph::new(Span::styled(gear, btn)),
            Rect::new(gx, rect.y, gear.width() as u16, 1),
        );
        ctx.hit_registry.register(
            Rect::new(fx, rect.y, fetch.width() as u16, 1),
            z + 2,
            HitTarget::BranchPopupFetch,
        );
        ctx.hit_registry.register(
            Rect::new(gx, rect.y, gear.width() as u16, 1),
            z + 2,
            HitTarget::BranchPopupSettings,
        );
    }

    let inner_w = rect.width.saturating_sub(2) as usize;
    // Search line.
    let search = Line::from(vec![
        Span::styled(" 🔍 ", Style::default().fg(ctx.theme.fg_muted)),
        Span::styled(
            if query.is_empty() {
                state.i18n.t("view.branches_popup.search_placeholder").to_string()
            } else {
                format!("{query}▏")
            },
            Style::default().fg(if query.is_empty() {
                ctx.theme.fg_muted
            } else {
                ctx.theme.fg_bright
            }),
        ),
    ]);
    let esc = "[Esc] ";
    let mut spans = crate::views::fit_left_right_spans(
        search.spans,
        vec![Span::styled(esc, Style::default().fg(ctx.theme.fg_muted))],
        inner_w,
        0,
    );
    spans.truncate(spans.len());
    let search_rect = Rect::new(rect.x + 1, rect.y + 1, rect.width.saturating_sub(2), 1);
    frame.render_widget(Paragraph::new(Line::from(spans)), search_rect);
    crate::views::register_right_esc_cancel(ctx, search_rect, esc, z + 2);
    frame.render_widget(
        Paragraph::new(Span::styled(
            format!("├{}┤", "─".repeat(inner_w)),
            Style::default().fg(ctx.theme.border_focused),
        )),
        Rect::new(rect.x, rect.y + 2, rect.width, 1),
    );

    let list = layout.list;
    if rows.is_empty() {
        frame.render_widget(
            Paragraph::new(Span::styled(
                state.i18n.t("view.branches_popup.empty"),
                Style::default().fg(ctx.theme.fg_muted),
            )),
            list,
        );
        return;
    }
    for (vis, (idx, row)) in rows
        .iter()
        .enumerate()
        .skip(layout.scroll)
        .take(list.height as usize)
        .enumerate()
    {
        let y = list.y + vis as u16;
        let row_rect = Rect::new(list.x, y, list.width, 1);
        let is_sel = idx == selected;
        let bg = if is_sel {
            ctx.theme.bg_selection_focused
        } else {
            ctx.theme.bg_elevated
        };
        let fg = ctx.theme.fg_primary;
        let (left, right): (Vec<Span<'static>>, Vec<Span<'static>>) = match row {
            PopupRow::Header(t) => (
                vec![Span::styled(
                    format!(" {} {t}", g.tree_expanded),
                    Style::default()
                        .fg(ctx.theme.fg_muted)
                        .add_modifier(Modifier::BOLD),
                )],
                Vec::new(),
            ),
            PopupRow::Folder(f) => (
                vec![Span::styled(
                    format!("   {} {f}", g.tree_expanded),
                    Style::default().fg(ctx.theme.fg_muted),
                )],
                Vec::new(),
            ),
            PopupRow::Ongoing(kind) => {
                let kind_title = state.i18n.ongoing_title(*kind);
                let skip = if kind.can_skip() {
                    state.i18n.t("view.branches_popup.ongoing_skip_part")
                } else {
                    ""
                };
                (
                    vec![Span::styled(
                        state.i18n.t_fmt(
                            "view.branches_popup.ongoing_in_progress",
                            &[("icon", g.warning), ("kind", kind_title), ("skip", skip)],
                        ),
                        Style::default()
                            .fg(ctx.theme.ref_detached.icon)
                            .add_modifier(Modifier::BOLD),
                    )],
                    vec![Span::styled("▸ ", Style::default().fg(ctx.theme.fg_muted))],
                )
            }
            PopupRow::NewBranch => {
                let right = match popup_new_branch_seq(state) {
                    Some(seq) => vec![Span::styled(
                        format!("{} ", seq.to_display_string()),
                        Style::default().fg(ctx.theme.fg_muted),
                    )],
                    None => Vec::new(),
                };
                (
                    vec![Span::styled(
                        state.i18n.t("view.branches_popup.new_branch_btn").to_string(),
                        Style::default().fg(fg),
                    )],
                    right,
                )
            }
            PopupRow::UpdateProject => {
                let right = match crate::keymap::catalog::shortcut_for_action(
                    state,
                    &[crate::config::KeymapScope::Global],
                    &crate::keymap::KeyAction::UpdateProject,
                ) {
                    Some(sc) => vec![Span::styled(
                        format!("{sc} "),
                        Style::default().fg(ctx.theme.fg_muted),
                    )],
                    None => Vec::new(),
                };
                (
                    vec![Span::styled(
                        state.i18n.t("view.branches_popup.update_project_btn").to_string(),
                        Style::default().fg(fg),
                    )],
                    right,
                )
            }
            PopupRow::CheckoutRevision => (
                vec![Span::styled(
                    state.i18n.t("view.branches_popup.checkout_rev_btn").to_string(),
                    Style::default().fg(fg),
                )],
                Vec::new(),
            ),
            PopupRow::ConfigureRemotes => {
                let right = match crate::keymap::catalog::shortcut_for_action(
                    state,
                    &[KeymapScope::Global],
                    &KeyAction::ConfigureRemotes,
                ) {
                    Some(sc) => vec![Span::styled(
                        format!("{sc} "),
                        Style::default().fg(ctx.theme.fg_muted),
                    )],
                    None => Vec::new(),
                };
                (
                    vec![Span::styled(
                        state.i18n.t("view.branches_popup.configure_remotes_btn").to_string(),
                        Style::default().fg(fg),
                    )],
                    right,
                )
            }
            PopupRow::Worktrees => {
                let right = match crate::keymap::catalog::shortcut_for_action(
                    state,
                    &[KeymapScope::Global],
                    &KeyAction::Worktrees,
                ) {
                    Some(sc) => vec![Span::styled(
                        format!("{sc} "),
                        Style::default().fg(ctx.theme.fg_muted),
                    )],
                    None => Vec::new(),
                };
                (
                    vec![Span::styled(
                        state.i18n.t("view.branches_popup.worktrees_btn").to_string(),
                        Style::default().fg(fg),
                    )],
                    right,
                )
            }
            PopupRow::Unshallow => {
                let right = match crate::keymap::catalog::shortcut_for_action(
                    state,
                    &[KeymapScope::Global],
                    &KeyAction::Unshallow,
                ) {
                    Some(sc) => vec![Span::styled(
                        format!("{sc} "),
                        Style::default().fg(ctx.theme.fg_muted),
                    )],
                    None => Vec::new(),
                };
                (
                    vec![Span::styled(
                        state.i18n.t("view.branches_popup.unshallow_btn").to_string(),
                        Style::default().fg(fg),
                    )],
                    right,
                )
            }
            PopupRow::Branch(b) => branch_spans(ctx, b, is_sel),
        };
        let line = crate::views::fit_left_right_spans(left, right, inner_w, 0);
        frame.render_widget(
            Paragraph::new(Line::from(line)).style(Style::default().bg(bg)),
            row_rect,
        );
        if row.selectable() {
            ctx.hit_registry
                .register(row_rect, z + 2, HitTarget::BranchPopupRow(idx));
        }
        if let PopupRow::Branch(b) = row
            && b.kind != RefKind::Tag
        {
            let star_x = list.x + 1 + b.indent;
            ctx.hit_registry.register(
                Rect::new(star_x, y, 2, 1),
                z + 3,
                HitTarget::BranchPopupStar(idx),
            );
        }
    }
    if layout.scroll > 0 {
        frame.render_widget(
            Paragraph::new(Span::styled("▴", Style::default().fg(ctx.theme.fg_muted))),
            Rect::new(rect.right().saturating_sub(3), rect.y + 2, 1, 1),
        );
    }
    if layout.scroll + (list.height as usize) < rows.len() {
        frame.render_widget(
            Paragraph::new(Span::styled("▾", Style::default().fg(ctx.theme.fg_muted))),
            Rect::new(
                rect.right().saturating_sub(3),
                rect.bottom().saturating_sub(1),
                1,
                1,
            ),
        );
    }
}

fn branch_spans(
    ctx: &RenderContext<'_>,
    b: &PopupBranch,
    is_sel: bool,
) -> (Vec<Span<'static>>, Vec<Span<'static>>) {
    let g = ctx.glyph_mode.symbols();
    let mut left = vec![Span::raw(" ".repeat(b.indent as usize + 1))];
    let star = if b.is_head || b.is_favorite {
        Span::styled(
            format!("{} ", g.favorite_star),
            Style::default().fg(ctx.theme.ref_head.icon),
        )
    } else {
        Span::styled("☆ ", Style::default().fg(ctx.theme.fg_muted))
    };
    left.push(star);
    let icon = match b.kind {
        RefKind::RemoteBranch => g.remote_branch,
        RefKind::Tag => g.tag,
        _ if b.is_head => g.head,
        _ => g.local_branch,
    };
    left.push(Span::styled(
        format!("{icon} "),
        Style::default().fg(match b.kind {
            RefKind::RemoteBranch => ctx.theme.ref_remote.icon,
            RefKind::Tag => ctx.theme.ref_tag.icon,
            _ if b.is_head => ctx.theme.ref_head.icon,
            _ => ctx.theme.ref_local.icon,
        }),
    ));
    let base = Style::default().fg(if is_sel {
        ctx.theme.fg_bright
    } else {
        ctx.theme.fg_primary
    });
    let hl = Style::default()
        .fg(ctx.theme.fg_accent_link)
        .add_modifier(Modifier::BOLD);
    // Highlight matched chars (char indices → spans).
    let mut cur = String::new();
    let mut cur_hl = false;
    for (i, ch) in b.name.chars().enumerate() {
        let is_hl = b.indices.binary_search(&(i as u32)).is_ok();
        if is_hl != cur_hl && !cur.is_empty() {
            left.push(Span::styled(
                std::mem::take(&mut cur),
                if cur_hl { hl } else { base },
            ));
        }
        cur_hl = is_hl;
        cur.push(ch);
    }
    if !cur.is_empty() {
        left.push(Span::styled(cur, if cur_hl { hl } else { base }));
    }
    let mut right = Vec::new();
    if let Some(wt) = &b.worktree {
        right.push(Span::styled(
            format!("⎇ {} ", truncate_to_width(wt, 18)),
            Style::default().fg(ctx.theme.status_modified),
        ));
    }
    if let Some(up) = &b.upstream {
        right.push(Span::styled(
            truncate_to_width(up, 22),
            Style::default().fg(ctx.theme.fg_muted),
        ));
    }
    let ab = g.format_ahead_behind(b.ahead_behind);
    if !ab.is_empty() {
        right.push(Span::styled(
            ab,
            Style::default().fg(ctx.theme.fg_accent_link),
        ));
    }
    right.push(Span::styled(" ▸ ", Style::default().fg(ctx.theme.fg_muted)));
    (left, right)
}
