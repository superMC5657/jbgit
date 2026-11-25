//! `Configure Remotes…` modal (`GitConfigureRemotesDialog`): lists the
//! configured remotes with their fetch / push URLs (credentials masked) and
//! offers add / rename / edit URL / remove / fetch on the selected remote.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use jbgit_core::{GitOp, RemoteInfo, RepoSnapshot};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph};
use unicode_width::UnicodeWidthStr;

use crate::app::{Action, AppState, ModalLayer, RenderContext, ScrollPane, ScrollViewport};
use crate::dialogs::{
    ChoiceDialog, Dialog, add_remote_dialog, rename_remote_dialog, set_remote_url_dialog,
};
use crate::hit_test::HitTarget;
use crate::i18n::I18n;
use crate::views::overlays::draw_shadow;
use crate::views::truncate_to_width;

/// Selection state of [`ModalLayer::Remotes`] (the list itself is read live
/// from the repository snapshot, so it follows add / rename / remove).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RemotesState {
    pub selected: usize,
    pub scroll: ScrollViewport,
}

impl RemotesState {
    /// Starts on the default remote (current upstream's remote → `origin`).
    pub fn for_snapshot(snap: Option<&RepoSnapshot>) -> Self {
        let selected = snap
            .and_then(|s| {
                let def = s.default_remote()?;
                s.remotes.iter().position(|r| r.name == def)
            })
            .unwrap_or(0);
        Self {
            selected,
            scroll: ScrollViewport::new(),
        }
    }
}

fn remotes(state: &AppState) -> &[RemoteInfo] {
    state
        .repo_snapshot
        .as_deref()
        .map_or(&[][..], |s| s.remotes.as_slice())
}

/// Currently selected remote (clamped to the live list).
pub fn selected_remote<'a>(state: &'a AppState, rs: &RemotesState) -> Option<&'a RemoteInfo> {
    let list = remotes(state);
    list.get(rs.selected.min(list.len().saturating_sub(1)))
}

fn top_state(state: &AppState) -> Option<RemotesState> {
    match state.modal_stack.last() {
        Some(ModalLayer::Remotes(rs)) => Some(*rs),
        _ => None,
    }
}

/// Danger confirmation for `git remote remove`, naming the remote-tracking
/// branches that disappear with it.
pub fn remove_remote_confirm(snap: Option<&RepoSnapshot>, name: &str, i18n: &I18n) -> Dialog {
    let tracking = snap.map_or(0, |s| s.tracking_refs_of(name).len());
    let upstream_users = snap.map_or(0, |s| {
        s.refs
            .iter()
            .filter(|r| {
                r.kind == jbgit_core::RefKind::LocalBranch
                    && r.upstream
                        .as_deref()
                        .is_some_and(|u| s.remote_of_tracking(u) == Some(name))
            })
            .count()
    });
    let t_s = tracking.to_string();
    let u_s = upstream_users.to_string();
    let mut body = vec![
        i18n.t_fmt("dialog.remotes.remove_body", &[("name", name)]),
        i18n.t_fmt(
            "dialog.remotes.remove_tracking",
            &[("name", name), ("count", &t_s)],
        ),
    ];
    if upstream_users > 0 {
        body.push(i18n.t_fmt("dialog.remotes.remove_upstreams", &[("count", &u_s)]));
    }
    Dialog::Choice(ChoiceDialog::confirm(
        i18n.t_fmt("dialog.remotes.remove_title", &[("name", name)]),
        body,
        i18n.t("dialog.remotes.remove_btn"),
        i18n.cancel(),
        vec![Action::RunGitOp(GitOp::RemoveRemote {
            name: name.to_string(),
        })],
    ))
}

fn open(d: Dialog) -> Vec<Action> {
    vec![Action::OpenDialog(Box::new(d))]
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteCommand {
    Add,
    Rename,
    EditFetchUrl,
    EditPushUrl,
    Remove,
    Fetch,
}

/// Actions for an editing command on the selected remote.
pub fn remote_command(state: &AppState, cmd: RemoteCommand) -> Vec<Action> {
    let Some(rs) = top_state(state) else {
        return Vec::new();
    };
    let snap = state.repo_snapshot.as_deref();
    let i18n = &state.i18n;
    if cmd == RemoteCommand::Add {
        return open(add_remote_dialog(snap, i18n));
    }
    let Some(remote) = selected_remote(state, &rs) else {
        return vec![Action::Notice(i18n.t("view.remotes.none_selected").into())];
    };
    let name = remote.name.as_str();
    match cmd {
        RemoteCommand::Add => unreachable!(),
        RemoteCommand::Rename => open(rename_remote_dialog(snap, name, i18n)),
        RemoteCommand::EditFetchUrl => open(set_remote_url_dialog(snap, name, false, i18n)),
        RemoteCommand::EditPushUrl => open(set_remote_url_dialog(snap, name, true, i18n)),
        RemoteCommand::Remove => open(remove_remote_confirm(snap, name, i18n)),
        RemoteCommand::Fetch => vec![Action::RunGitOp(GitOp::FetchRemote {
            name: name.to_string(),
        })],
    }
}

/// Keyboard routing while the Remotes modal is the topmost layer.
pub fn route_remotes_key(key: &KeyEvent, state: &AppState) -> Vec<Action> {
    if key
        .modifiers
        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
    {
        return Vec::new();
    }
    match key.code {
        KeyCode::Esc | KeyCode::Char('q') => vec![Action::PopModal],
        KeyCode::Up | KeyCode::Char('k') => vec![Action::RemotesMove(-1)],
        KeyCode::Down | KeyCode::Char('j') => vec![Action::RemotesMove(1)],
        KeyCode::Home | KeyCode::Char('g') => vec![Action::RemotesSelect(0)],
        KeyCode::End | KeyCode::Char('G') => vec![Action::RemotesSelect(usize::MAX)],
        KeyCode::Char('a' | 'n') | KeyCode::Insert => remote_command(state, RemoteCommand::Add),
        KeyCode::Char('r') | KeyCode::F(2) => remote_command(state, RemoteCommand::Rename),
        KeyCode::Char('e') | KeyCode::Enter => remote_command(state, RemoteCommand::EditFetchUrl),
        KeyCode::Char('p') => remote_command(state, RemoteCommand::EditPushUrl),
        KeyCode::Char('d') | KeyCode::Delete => remote_command(state, RemoteCommand::Remove),
        KeyCode::Char('f') => remote_command(state, RemoteCommand::Fetch),
        _ => Vec::new(),
    }
}

/// Applies a selection change to the topmost Remotes layer.
pub fn apply_selection(state: &mut AppState, select: Option<usize>, delta: isize) {
    let len = remotes(state).len();
    if let Some(ModalLayer::Remotes(rs)) = state.modal_stack.last_mut() {
        let cur = rs.selected.min(len.saturating_sub(1));
        rs.selected = match select {
            Some(idx) => idx.min(len.saturating_sub(1)),
            None => (cur as isize + delta).clamp(0, len.saturating_sub(1) as isize) as usize,
        };
        if let Some(vh) = rs.scroll.viewport_height() {
            rs.scroll.ensure_visible(rs.selected, len, vh);
        }
    }
}

pub fn remotes_modal_rect(screen: Rect, rows: usize) -> Rect {
    let w = screen
        .width
        .saturating_sub(4)
        .clamp(20.min(screen.width), 96);
    let h = (rows as u16 * 2 + 6)
        .clamp(9, 24)
        .min(screen.height.saturating_sub(2).max(5));
    Rect::new(
        screen.x + screen.width.saturating_sub(w) / 2,
        screen.y + screen.height.saturating_sub(h) / 2,
        w,
        h,
    )
}

pub fn render_remotes_modal(
    frame: &mut Frame<'_>,
    screen: Rect,
    state: &AppState,
    rs: &RemotesState,
    ctx: &mut RenderContext<'_>,
    z: u8,
) {
    if screen.width < 20 || screen.height < 8 {
        return;
    }
    let list = remotes(state);
    let rect = remotes_modal_rect(screen, list.len().max(1));
    draw_shadow(frame, screen, rect);
    crate::views::clear_modal_overlay(frame, rect);
    let title = format!(" {} ", ctx.t("view.remotes.title"));
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
    if inner.height < 3 || inner.width < 10 {
        return;
    }
    let w = inner.width as usize;
    let list_h = inner.height.saturating_sub(2);

    if list.is_empty() {
        ctx.record_scroll_bound(ScrollPane::Remotes, (list_h / 2).max(1) as usize, 0);
        frame.render_widget(
            Paragraph::new(Span::styled(
                format!(" {}", ctx.t("view.remotes.empty")),
                Style::default().fg(ctx.theme.fg_muted),
            )),
            Rect::new(inner.x, inner.y, inner.width, 1),
        );
    } else {
        let selected = rs.selected.min(list.len() - 1);
        let per_page = (list_h / 2).max(1) as usize;
        let mut vp = rs.scroll;
        vp.ensure_visible(selected, list.len(), per_page);
        let first = vp.offset();
        ctx.record_scroll_bound(
            ScrollPane::Remotes,
            per_page,
            list.len().saturating_sub(per_page),
        );
        let name_w = list
            .iter()
            .map(|r| r.name.width())
            .max()
            .unwrap_or(0)
            .clamp(6, 24);
        let push_label = ctx.t("view.remotes.push_label").to_string();
        let fetch_label = ctx.t("view.remotes.fetch_label").to_string();
        let no_url = ctx.t("view.remotes.no_url").to_string();
        for (row, (idx, remote)) in list
            .iter()
            .enumerate()
            .skip(first)
            .take(per_page)
            .enumerate()
        {
            let y = inner.y + (row as u16) * 2;
            let is_sel = idx == selected;
            let bg = if is_sel {
                ctx.theme.bg_selection_focused
            } else {
                ctx.theme.bg_elevated
            };
            let name = truncate_to_width(&remote.name, name_w);
            let pad = " ".repeat(name_w.saturating_sub(name.width()));
            let url_w = w.saturating_sub(name_w + 4 + fetch_label.width());
            let fetch = remote.display_fetch_url();
            let fetch = if fetch.is_empty() {
                no_url.clone()
            } else {
                fetch
            };
            let line1 = Line::from(vec![
                Span::styled(
                    format!(" {name}{pad}  "),
                    Style::default()
                        .fg(ctx.theme.fg_bright)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(fetch_label.clone(), Style::default().fg(ctx.theme.fg_muted)),
                Span::styled(
                    truncate_to_width(&fetch, url_w),
                    Style::default().fg(ctx.theme.fg_primary),
                ),
            ]);
            let push = remote
                .display_push_url()
                .unwrap_or_else(|| ctx.t("view.remotes.push_same").to_string());
            let line2 = Line::from(vec![
                Span::raw(format!(" {}  ", " ".repeat(name_w))),
                Span::styled(push_label.clone(), Style::default().fg(ctx.theme.fg_muted)),
                Span::styled(
                    truncate_to_width(&push, url_w),
                    Style::default().fg(if remote.push_url.is_some() {
                        ctx.theme.fg_primary
                    } else {
                        ctx.theme.fg_muted
                    }),
                ),
            ]);
            let row_rect = Rect::new(inner.x, y, inner.width, 2.min(list_h - row as u16 * 2));
            frame.render_widget(
                Paragraph::new(vec![line1, line2]).style(Style::default().bg(bg)),
                row_rect,
            );
            ctx.hit_registry
                .register(row_rect, z + 2, HitTarget::RemotesRow(idx));
        }
    }

    let hint = ctx.t("view.remotes.hint").to_string();
    frame.render_widget(
        Paragraph::new(Span::styled(
            truncate_to_width(&format!(" {hint}"), w),
            Style::default().fg(ctx.theme.fg_muted),
        )),
        Rect::new(inner.x, inner.bottom().saturating_sub(1), inner.width, 1),
    );
}
