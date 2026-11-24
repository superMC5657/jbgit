//! `[3 Stash]` workbench: stash list with Apply / Pop /
//! Unstash as Branch / Drop / New / Clear, plus reused `CommitChangesBrowser`
//! + `DiffPreview` on the right for the selected stash.

use jbgit_core::{PaneId, SplitAxis, SplitterId};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use unicode_width::UnicodeWidthStr;

use crate::app::{AppState, RenderContext};
use crate::hit_test::{HitTarget, StashButton};
use crate::views::log_details::{
    ChangesBrowserProps, DiffHost, DiffPreviewProps, render_changes_browser, render_diff_preview,
};
use crate::views::{
    fit_left_right_spans, list_viewport, render_pane_frame, render_splitter_divider,
    row_background,
};

pub fn render_stash_tab(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &AppState,
    ctx: &mut RenderContext<'_>,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let split = state
        .splitters
        .split(area, SplitterId::StashMainHorizontal, SplitAxis::Horizontal);
    ctx.record_splitter_container(
        SplitterId::StashMainHorizontal,
        SplitAxis::Horizontal,
        split.container,
    );

    let stashes = state
        .repo_snapshot
        .as_ref()
        .map(|s| s.stashes.as_slice())
        .unwrap_or(&[]);
    let count_s = stashes.len().to_string();
    let subtitle = state.i18n.t_fmt("view.stash.count", &[("count", &count_s)]);
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
        (state.i18n.t("view.stash.apply_btn"), StashButton::Apply),
        (state.i18n.t("view.stash.pop_btn"), StashButton::Pop),
        (state.i18n.t("view.stash.branch_btn"), StashButton::Branch),
        (state.i18n.t("view.stash.drop_btn"), StashButton::Drop),
        (state.i18n.t("view.stash.new_btn"), StashButton::New),
        (state.i18n.t("view.stash.clear_btn"), StashButton::Clear),
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
                    state.i18n.t("view.stash.no_stashes"),
                    Style::default().fg(ctx.theme.fg_muted),
                )),
                Line::from(Span::styled(
                    state.i18n.t("view.stash.empty_hint"),
                    Style::default().fg(ctx.theme.fg_muted),
                )),
            ]),
            list,
        );
        return;
    }
    let focused = ctx.focused_pane == PaneId::StashList;
    let (sel, offset) = list_viewport(stashes.len(), state.stash.selected(), list.height as usize);
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
            state.i18n.format_relative_time(s.timestamp, now),
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
                state.i18n.t("view.stash.select_preview"),
                Style::default().fg(ctx.theme.fg_muted),
            )),
            body,
        );
        return;
    }

    let preview_split = state.splitters.split(
        area,
        SplitterId::StashPreviewVertical,
        SplitAxis::Vertical,
    );
    ctx.record_splitter_container(
        SplitterId::StashPreviewVertical,
        SplitAxis::Vertical,
        preview_split.container,
    );
    render_changes_browser(
        frame,
        preview_split.first,
        ChangesBrowserProps {
            changes: state.stash.changes(),
            caches: &state.diff_caches,
            host: DiffHost::Stash,
            pane: PaneId::StashDiffPreview,
            diff_preview: true,
            merge_parent: 0,
            many_changes_count: None,
            path_filter_active: false,
            show_only_affected: false,
        },
        ctx,
    );
    if !preview_split.collapsed {
        render_splitter_divider(
            frame,
            preview_split.divider,
            SplitterId::StashPreviewVertical,
            SplitAxis::Vertical,
            ctx,
        );
        let diff_sub = state
            .stash
            .changes()
            .selected_change(&state.diff_caches)
            .map(|c| state.i18n.t_fmt("view.log_details.diff_file", &[("path", &c.path)]))
            .unwrap_or_else(|| state.i18n.t("view.log_details.diff_preview").to_string());
        let diff_body = render_pane_frame(
            frame,
            preview_split.second,
            PaneId::StashDiffPreview,
            &diff_sub,
            ctx,
        );
        render_diff_preview(
            frame,
            diff_body,
            DiffPreviewProps {
                changes: state.stash.changes(),
                diff_session: state.stash.diff(),
                caches: &state.diff_caches,
                host: DiffHost::Stash,
                diff_dock: crate::log_view::DiffPreviewDock::Details,
                zoomed: false,
            },
            ctx,
        );
    }
}
