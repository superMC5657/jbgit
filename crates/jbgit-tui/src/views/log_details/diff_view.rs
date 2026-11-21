use std::sync::Arc;

use jbgit_core::{DiffLine, DiffLineKind, FileDiff, PaneId};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use unicode_width::UnicodeWidthStr;

use crate::app::{AppState, RenderContext, ScrollPane};
use crate::hit_test::{DiffToolbarButton, HitTarget};
use crate::log_view::{DetailsPaneState, DiffPreviewDock, DiffRow, FileDiffRowsCache};
use crate::syntax::{HighlightedDiff, render_diff_line_spans_with_tab};
use crate::views::{render_pane_frame, render_toolbar, truncate_to_width};

use super::commit_details::render_commit_details;
use super::{DiffHost, muted_line};

pub fn render_details_or_diff(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &AppState,
    ctx: &mut RenderContext<'_>,
) {
    if state.details.diff_preview && state.details.diff_dock == DiffPreviewDock::Details {
        render_docked_diff(frame, area, state, ctx);
    } else {
        let body = render_pane_frame(frame, area, PaneId::LogDetailsDiff, "", ctx);
        render_commit_details(frame, body, state, ctx);
    }
}

pub fn render_docked_diff(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &AppState,
    ctx: &mut RenderContext<'_>,
) {
    let subtitle = state
        .details
        .selected_change()
        .map(|c| ctx.t_fmt("view.log_details.diff_file", &[("path", &c.path)]))
        .unwrap_or_else(|| ctx.t("view.log_details.diff_preview").to_string());
    let body = render_pane_frame(frame, area, PaneId::LogDetailsDiff, &subtitle, ctx);
    render_diff_view(frame, body, state, ctx, false);
}

pub fn render_zoomed_diff(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &AppState,
    ctx: &mut RenderContext<'_>,
) {
    if area.height == 0 {
        return;
    }
    let files = state.details.file_row_positions();
    let pos = files
        .iter()
        .position(|&p| p == state.details.changes_selected)
        .map_or(0, |i| i + 1);
    let path = state
        .details
        .selected_change()
        .map(|c| c.path.clone())
        .unwrap_or_default();
    let header = ctx.t_fmt(
        "view.log_details.zoom_header",
        &[
            ("path", &path),
            ("pos", &pos.to_string()),
            ("total", &files.len().to_string()),
        ],
    );
    frame.render_widget(
        Paragraph::new(Span::styled(
            truncate_to_width(&header, area.width as usize),
            Style::default()
                .fg(ctx.theme.fg_bright)
                .add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().bg(ctx.theme.bg_panel)),
        Rect::new(area.x, area.y, area.width, 1),
    );
    ctx.hit_registry
        .register(area, 1, HitTarget::Pane(PaneId::LogDetailsDiff));
    let body = Rect::new(area.x, area.y + 1, area.width, area.height - 1);
    render_diff_view(frame, body, state, ctx, true);
}

fn render_diff_view(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &AppState,
    ctx: &mut RenderContext<'_>,
    zoomed: bool,
) {
    render_diff_preview(frame, area, &state.details, DiffHost::Log, zoomed, ctx);
}

pub struct FileDiffRenderProps<'a> {
    pub diff: &'a Arc<FileDiff>,
    pub rows_cache: &'a FileDiffRowsCache,
    pub cached_syntax: Option<&'a HighlightedDiff>,
    pub side_by_side: bool,
    pub diff_scroll: usize,
    pub scroll_pane: ScrollPane,
}

pub fn render_file_diff_body(
    frame: &mut Frame<'_>,
    body: Rect,
    props: FileDiffRenderProps<'_>,
    ctx: &mut RenderContext<'_>,
) {
    let FileDiffRenderProps {
        diff,
        rows_cache,
        cached_syntax,
        side_by_side,
        diff_scroll,
        scroll_pane,
    } = props;
    if body.width == 0 || body.height == 0 {
        return;
    }
    let h = body.height as usize;
    if diff.is_too_large {
        ctx.record_scroll_bound(scroll_pane, h, 0);
        frame.render_widget(
            muted_line(ctx, ctx.t("view.log_details.too_large_diff")),
            body,
        );
        return;
    }
    if diff.is_binary {
        ctx.record_scroll_bound(scroll_pane, h, 0);
        frame.render_widget(muted_line(ctx, ctx.t("view.log_details.binary_diff")), body);
        return;
    }
    if diff.hunks.is_empty() {
        ctx.record_scroll_bound(scroll_pane, h, 0);
        frame.render_widget(
            muted_line(ctx, ctx.t("view.log_details.no_text_diff")),
            body,
        );
        return;
    }

    let rows = rows_cache.rows_for(diff, side_by_side);
    let mut total = rows.len();
    if diff.is_truncated {
        total += 1;
    }
    let max_scroll = total.saturating_sub(h);
    ctx.record_scroll_bound(scroll_pane, h, max_scroll);
    let scroll = diff_scroll.min(max_scroll);
    let theme = ctx.theme;
    let num_w = 4usize;
    let tab_size = ctx.diff_tab_size();
    let empty_hl;
    let hl_diff: &HighlightedDiff = if !ctx.diff_syntax_enabled() {
        empty_hl = HighlightedDiff::default();
        &empty_hl
    } else {
        match cached_syntax {
            Some(cached) => cached,
            None => {
                empty_hl = HighlightedDiff::default();
                &empty_hl
            }
        }
    };

    let line_style = |kind: DiffLineKind| -> (Style, Color, &'static str) {
        match kind {
            DiffLineKind::Added => (
                Style::default().fg(theme.fg_primary).bg(theme.diff_add_bg),
                theme.diff_add_word_bg,
                "+",
            ),
            DiffLineKind::Deleted => (
                Style::default().fg(theme.fg_primary).bg(theme.diff_del_bg),
                theme.diff_del_word_bg,
                "-",
            ),
            DiffLineKind::Context => (
                Style::default().fg(theme.fg_primary).bg(theme.bg_canvas),
                theme.bg_canvas,
                " ",
            ),
        }
    };
    let gutter = Style::default().fg(theme.fg_muted).bg(theme.bg_panel);
    let num = |n: Option<u32>| n.map_or_else(|| " ".repeat(num_w), |v| format!("{v:>num_w$}"));

    let side_cell = |hh: usize,
                     line_idx: Option<usize>,
                     custom_wr: Option<&[(usize, usize)]>,
                     lineno: fn(&DiffLine) -> Option<u32>,
                     width: usize|
     -> Vec<Span<'static>> {
        let mut spans = Vec::new();
        match line_idx {
            Some(li) => {
                let l = &diff.hunks[hh].lines[li];
                let toks = hl_diff
                    .hunks
                    .get(hh)
                    .and_then(|h| h.get(li))
                    .map(Vec::as_slice)
                    .unwrap_or(&[]);
                let (base, word_bg, marker) = line_style(l.kind);
                spans.push(Span::styled(format!("{} ", num(lineno(l))), gutter));
                spans.push(Span::styled(marker.to_string(), base));
                let text_w = width.saturating_sub(num_w + 2);
                let wr = custom_wr.unwrap_or(&l.word_ranges);
                let mut body = render_diff_line_spans_with_tab(
                    &l.text,
                    toks,
                    wr,
                    text_w,
                    tab_size,
                    base,
                    word_bg,
                    theme.true_color,
                );
                let used: usize = body.iter().map(|s| s.content.width()).sum();
                body.push(Span::styled(" ".repeat(text_w.saturating_sub(used)), base));
                spans.extend(body);
            }
            None => {
                spans.push(Span::styled(" ".repeat(num_w + 1), gutter));
                spans.push(Span::styled(
                    " ".repeat(width.saturating_sub(num_w + 1)),
                    Style::default().bg(theme.bg_panel),
                ));
            }
        }
        spans
    };

    let width = body.width as usize;
    for (vis_y, row_idx) in (scroll..total).take(h).enumerate() {
        let rect = Rect::new(body.x, body.y + vis_y as u16, body.width, 1);
        let line = match rows.get(row_idx) {
            None => Line::from(Span::styled(
                ctx.t("view.log_details.diff_truncated").to_string(),
                Style::default().fg(theme.status_conflicted),
            )),
            Some(DiffRow::Header(hh)) => Line::from(Span::styled(
                truncate_to_width(&diff.hunks[*hh].header, width),
                Style::default()
                    .fg(theme.fg_accent_link)
                    .bg(theme.diff_mod_bg),
            ))
            .style(Style::default().bg(theme.diff_mod_bg)),
            Some(DiffRow::Unified(hh, l)) => {
                let hunk_lines = &diff.hunks[*hh].lines;
                let dl = &hunk_lines[*l];
                let toks = hl_diff
                    .hunks
                    .get(*hh)
                    .and_then(|h| h.get(*l))
                    .map(Vec::as_slice)
                    .unwrap_or(&[]);
                let (base, word_bg, marker) = line_style(dl.kind);
                let mut spans = vec![
                    Span::styled(
                        format!("{} {} ", num(dl.old_lineno), num(dl.new_lineno)),
                        gutter,
                    ),
                    Span::styled(marker.to_string(), base),
                ];
                let text_w = width.saturating_sub(num_w * 2 + 3);
                let lazy_wr = rows_cache.word_cache.unified_ranges(diff, *hh, *l, hunk_lines);
                let wr = lazy_wr.as_deref().unwrap_or(&dl.word_ranges);
                spans.extend(render_diff_line_spans_with_tab(
                    &dl.text,
                    toks,
                    wr,
                    text_w,
                    tab_size,
                    base,
                    word_bg,
                    theme.true_color,
                ));
                Line::from(spans).style(base)
            }
            Some(DiffRow::Split(hh, left, right)) => {
                let (lazy_l, lazy_r) = rows_cache.word_cache.split_ranges(
                    diff,
                    *hh,
                    *left,
                    *right,
                    &diff.hunks[*hh].lines,
                );
                let half = width / 2;
                let mut spans = side_cell(*hh, *left, lazy_l.as_deref(), |l| l.old_lineno, half);
                spans.push(Span::styled(
                    "│",
                    Style::default().fg(theme.border_splitter),
                ));
                spans.extend(side_cell(
                    *hh,
                    *right,
                    lazy_r.as_deref(),
                    |l| l.new_lineno,
                    width - half - 1,
                ));
                Line::from(spans)
            }
        };
        frame.render_widget(Paragraph::new(line), rect);
    }
}

pub fn render_diff_preview(
    frame: &mut Frame<'_>,
    area: Rect,
    d: &DetailsPaneState,
    host: DiffHost,
    zoomed: bool,
    ctx: &mut RenderContext<'_>,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let mut buttons = vec![
        (
            if d.diff_side_by_side {
                ctx.t("view.log_details.side_by_side_btn")
            } else {
                ctx.t("view.log_details.unified_btn")
            }
            .to_string(),
            d.diff_side_by_side,
            DiffToolbarButton::SideBySide,
        ),
        (
            ctx.t_fmt(
                "view.log_details.ctx_btn",
                &[("ctx", ctx.diff_context_label(d.diff_context))],
            ),
            false,
            DiffToolbarButton::Context,
        ),
    ];
    if host == DiffHost::Log {
        let zoom_label = if zoomed {
            ctx.t("view.log_details.restore_btn").to_string()
        } else {
            ctx.t("view.log_details.zoom_btn").to_string()
        };
        buttons.push((zoom_label, zoomed, DiffToolbarButton::Zoom));
        if !zoomed {
            buttons.push((
                ctx.t_fmt(
                    "view.log_details.dock_btn",
                    &[("dock", ctx.diff_dock_label(d.diff_dock))],
                ),
                d.diff_dock != DiffPreviewDock::Details,
                DiffToolbarButton::Dock,
            ));
        }
        buttons.push((" × ".to_string(), false, DiffToolbarButton::Close));
    }
    render_toolbar(
        frame,
        Rect::new(area.x, area.y, area.width, 1),
        ctx,
        3,
        &buttons,
        |b| match host {
            DiffHost::Log => HitTarget::DiffToolbar(b),
            DiffHost::Stash => HitTarget::StashDiffToolbar(b),
        },
    );
    let body = Rect::new(area.x, area.y + 1, area.width, area.height - 1);
    if body.height == 0 {
        return;
    }
    ctx.hit_registry.register(
        body,
        2,
        match host {
            DiffHost::Log => HitTarget::DiffArea,
            DiffHost::Stash => HitTarget::Pane(PaneId::StashDiffPreview),
        },
    );

    let h = body.height as usize;
    let Some(change) = d.selected_change() else {
        ctx.record_scroll_bound(host.scroll_pane(), h, 0);
        let msg = if d.current_bundle().is_none() {
            ctx.t("common.loading")
        } else {
            ctx.t("view.log_details.select_file_diff")
        };
        frame.render_widget(muted_line(ctx, msg), body);
        return;
    };
    let Some(diff) = d.current_diff() else {
        ctx.record_scroll_bound(host.scroll_pane(), h, 0);
        if change.is_binary {
            frame.render_widget(muted_line(ctx, ctx.t("view.log_details.binary_diff")), body);
        } else {
            frame.render_widget(muted_line(ctx, ctx.t("view.log_details.loading_diff")), body);
        }
        return;
    };
    render_file_diff_body(
        frame,
        body,
        FileDiffRenderProps {
            diff,
            rows_cache: &d.diff_rows_cache,
            cached_syntax: d.current_diff_syntax().map(Arc::as_ref),
            side_by_side: d.diff_side_by_side,
            diff_scroll: d.diff_scroll,
            scroll_pane: host.scroll_pane(),
        },
        ctx,
    );
}
