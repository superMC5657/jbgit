//! `[3 Stash]` workbench (Spec 05 §4.4): stash list with Apply / Pop /
//! Unstash as Branch / Drop / New / Clear, plus reused `CommitChangesBrowser`
//! + `DiffPreview` on the right for the selected stash.

use jgit_core::{PaneId, SplitAxis, SplitterId};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use unicode_width::UnicodeWidthStr;

use crate::app::{AppState, RenderContext};
use crate::hit_test::{HitTarget, StashButton};
use crate::log_view::format_relative_time;
use crate::views::log_details::{DiffHost, render_changes_browser, render_diff_preview};
use crate::views::{
    fit_left_right_spans, list_viewport, render_pane_frame, render_splitter_divider,
    row_background,
};

pub fn render_stash_tab(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &AppState,
    ctx: &mut RenderContext<'_>,
    splitter_containers: &mut Vec<(SplitterId, SplitAxis, Rect)>,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let split = state
        .splitters
        .split(area, SplitterId::StashMainHorizontal, SplitAxis::Horizontal);
    splitter_containers.push((
        SplitterId::StashMainHorizontal,
        SplitAxis::Horizontal,
        split.container,
    ));

    let stashes = state
        .repo_snapshot
        .as_ref()
        .map(|s| s.stashes.as_slice())
        .unwrap_or(&[]);
    let subtitle = format!("{} stash(es)", stashes.len());
    let list_body = render_pane_frame(frame, split.first, PaneId::StashList, &subtitle, ctx);
    render_stash_list(frame, list_body, state, ctx);

    if !split.collapsed {
        render_splitter_divider(
            frame,
            split.divider,
            SplitterId::StashMainHorizontal,
            SplitAxis::Horizontal,
            ctx,
        );
        render_stash_preview(frame, split.second, state, ctx);
    }
}

fn render_stash_list(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &AppState,
    ctx: &mut RenderContext<'_>,
) {
    if area.height == 0 {
        return;
    }
    let buttons = [
        (" ▶ Apply ", StashButton::Apply),
        (" ⇡ Pop ", StashButton::Pop),
        (" ⎇ Branch ", StashButton::Branch),
        (" ✕ Drop ", StashButton::Drop),
        (" + New ", StashButton::New),
        (" ⌫ Clear ", StashButton::Clear),
    ];
    let mut x = area.x;
    let mut spans = Vec::new();
    for (label, btn) in buttons {
        let w = label.width() as u16;
        if x + w > area.right() {
            break;
        }
        spans.push(Span::styled(
            label,
            Style::default()
                .fg(ctx.theme.fg_primary)
                .bg(ctx.theme.bg_elevated),
        ));
        spans.push(Span::raw(" "));
        ctx.hit_registry
            .register(Rect::new(x, area.y, w, 1), 2, HitTarget::StashToolbar(btn));
        x += w + 1;
    }
    frame.render_widget(
        Paragraph::new(Line::from(spans)).style(Style::default().bg(ctx.theme.bg_panel)),
        Rect::new(area.x, area.y, area.width, 1),
    );
    let list = Rect::new(
        area.x,
        area.y + 1,
        area.width,
        area.height.saturating_sub(1),
    );
    let stashes = state
        .repo_snapshot
        .as_ref()
        .map(|s| s.stashes.as_slice())
        .unwrap_or(&[]);
    if stashes.is_empty() {
        frame.render_widget(
            Paragraph::new(vec![
                Line::from(Span::styled(
                    " No stashes",
                    Style::default().fg(ctx.theme.fg_muted),
                )),
                Line::from(Span::styled(
                    " n New Stash… · Commit tab toolbar [Stash] · right-click files → Stash Selected",
                    Style::default().fg(ctx.theme.fg_muted),
                )),
            ]),
            list,
        );
        return;
    }
    let focused = ctx.focused_pane == PaneId::StashList;
    let (sel, offset) = list_viewport(stashes.len(), state.stash.selected, list.height as usize);
    let now = chrono::Local::now().timestamp();
    for (vis, (i, s)) in stashes
        .iter()
        .enumerate()
        .skip(offset)
        .take(list.height as usize)
        .enumerate()
    {
        let y = list.y + vis as u16;
        let r = Rect::new(list.x, y, list.width, 1);
        let hovered = ctx.hover_target == Some(&HitTarget::StashRow(i));
        let bg = row_background(i == sel, focused, hovered, ctx.theme.bg_canvas, ctx.theme);
        let left = vec![
            Span::styled(
                format!(" stash@{{{}}} ", s.index),
                Style::default().fg(ctx.theme.fg_accent_link),
            ),
            Span::styled(
                s.branch().map(|b| format!("{b}: ")).unwrap_or_default(),
                Style::default().fg(ctx.theme.ref_local.icon),
            ),
            Span::styled(
                s.short_message().to_string(),
                Style::default().fg(ctx.theme.fg_primary),
            ),
        ];
        let right = vec![Span::styled(
            format_relative_time(s.timestamp, now),
            Style::default().fg(ctx.theme.fg_muted),
        )];
        frame.render_widget(
            Paragraph::new(Line::from(fit_left_right_spans(
                left,
                right,
                list.width as usize,
                1,
            )))
            .style(Style::default().bg(bg)),
            r,
        );
        ctx.hit_registry.register(r, 2, HitTarget::StashRow(i));
    }
}

fn render_stash_preview(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &AppState,
    ctx: &mut RenderContext<'_>,
) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    let stashes = state
        .repo_snapshot
        .as_ref()
        .map(|s| s.stashes.as_slice())
        .unwrap_or(&[]);
    if stashes.is_empty() {
        let body = render_pane_frame(frame, area, PaneId::StashDiffPreview, "", ctx);
        frame.render_widget(
            Paragraph::new(Span::styled(
                " Select a stash to preview its changes",
                Style::default().fg(ctx.theme.fg_muted),
            )),
            body,
        );
        return;
    }

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(35), Constraint::Percentage(65)])
        .split(area);
    render_changes_browser(
        frame,
        chunks[0],
        &state.stash.details,
        DiffHost::Stash,
        PaneId::StashDiffPreview,
        ctx,
    );
    let diff_sub = state
        .stash
        .details
        .selected_change()
        .map(|c| format!("Diff · {}", c.path))
        .unwrap_or_else(|| "Diff Preview".to_string());
    let diff_body = render_pane_frame(frame, chunks[1], PaneId::StashDiffPreview, &diff_sub, ctx);
    render_diff_preview(
        frame,
        diff_body,
        &state.stash.details,
        DiffHost::Stash,
        false,
        ctx,
    );
}
