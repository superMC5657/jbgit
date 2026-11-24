use std::sync::Arc;

use jbgit_core::{DiffLine, DiffLineKind, FileDiff, FileModeChange, PaneId};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use unicode_width::UnicodeWidthStr;

use crate::app::{AppState, RenderContext, ScrollPane};
use crate::diff_session::{ChangesTreeSession, DiffCacheStore, DiffRow, DiffSession, FileDiffRowsCache};
use crate::hit_test::{DiffToolbarButton, HitTarget};
use crate::log_view::DiffPreviewDock;
use crate::syntax::{HighlightedDiff, SyntaxToken, render_diff_line_spans_with_tab};
use crate::theme::ThemePalette;
use crate::views::{render_pane_frame, render_toolbar, truncate_to_width};

/// Shared base line style, word-diff highlight background, and prefix glyph
/// reused by both Log/Stash/FileHistory diffs and Staging Workbench diffs.
pub(crate) fn diff_line_style(theme: &ThemePalette, kind: DiffLineKind) -> (Style, Color, &'static str) {
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
        DiffLineKind::NoNewline => (
            Style::default()
                .fg(theme.fg_muted)
                .bg(theme.bg_panel)
                .add_modifier(Modifier::ITALIC),
            theme.bg_panel,
            "\\",
        ),
    }
}

/// Formats a localized metadata header banner for mode/typechange/symlink diffs
/// (e.g. `mode 100644 → 100755`, `typechange 100644 → 120000 (symlink target)`,
/// `symlink target (120000)`).
pub(crate) fn format_mode_change_header(mc: &FileModeChange, ctx: &RenderContext<'_>) -> String {
    let old_s = mc
        .old_mode
        .map(FileModeChange::format_mode)
        .unwrap_or_else(|| "------".to_string());
    let new_s = mc
        .new_mode
        .map(FileModeChange::format_mode)
        .unwrap_or_else(|| "------".to_string());
    if mc.is_typechange() {
        if mc.is_symlink {
            ctx.t_fmt(
                "view.diff.typechange_symlink",
                &[("old", &old_s), ("new", &new_s)],
            )
        } else {
            ctx.t_fmt("view.diff.typechange", &[("old", &old_s), ("new", &new_s)])
        }
    } else if mc.is_mode_changed() {
        ctx.t_fmt("view.diff.mode_change", &[("old", &old_s), ("new", &new_s)])
    } else {
        ctx.t("view.diff.symlink_target").to_string()
    }
}

/// Renders a 1-row metadata header banner at `area` for file mode / symlink changes.
pub(crate) fn render_mode_change_banner(
    frame: &mut Frame<'_>,
    area: Rect,
    mc: &FileModeChange,
    ctx: &RenderContext<'_>,
) {
    let label = format!(" {} ", format_mode_change_header(mc, ctx));
    frame.render_widget(
        Paragraph::new(Span::styled(
            truncate_to_width(&label, area.width as usize),
            Style::default()
                .fg(ctx.theme.ref_detached.pill_fg)
                .bg(ctx.theme.bg_elevated)
                .add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().bg(ctx.theme.bg_elevated)),
        area,
    );
}

pub(crate) struct DiffLineRenderInput<'a> {
    pub line: &'a DiffLine,
    pub tokens: &'a [SyntaxToken],
    pub custom_word_ranges: Option<&'a [(usize, usize)]>,
    pub base: Style,
    pub word_bg: Color,
    pub marker: &'static str,
    pub leading_gutter: Option<Span<'static>>,
}

const DIFF_LINE_NUM_W: usize = 4;

fn format_lineno(n: Option<u32>) -> String {
    n.map_or_else(
        || " ".repeat(DIFF_LINE_NUM_W),
        |v| format!("{v:>DIFF_LINE_NUM_W$}"),
    )
}

pub(crate) fn render_diff_unified_spans(
    input: DiffLineRenderInput<'_>,
    width: usize,
    tab_size: usize,
    theme: &ThemePalette,
    gutter: Style,
) -> Vec<Span<'static>> {
    let has_gutter = usize::from(input.leading_gutter.is_some());
    let mut spans = Vec::with_capacity(6);
    if let Some(g) = input.leading_gutter {
        spans.push(g);
    }
    spans.push(Span::styled(
        format!(
            "{} {} ",
            format_lineno(input.line.old_lineno),
            format_lineno(input.line.new_lineno)
        ),
        gutter,
    ));
    spans.push(Span::styled(input.marker.to_string(), input.base));
    let text_w = width.saturating_sub(DIFF_LINE_NUM_W * 2 + 3 + has_gutter);
    let wr = input.custom_word_ranges.unwrap_or(&input.line.word_ranges);
    spans.extend(render_diff_line_spans_with_tab(
        &input.line.text,
        input.tokens,
        wr,
        text_w,
        tab_size,
        input.base,
        input.word_bg,
        theme.true_color,
    ));
    spans
}

pub(crate) fn render_diff_side_cell(
    input: Option<DiffLineRenderInput<'_>>,
    lineno: fn(&DiffLine) -> Option<u32>,
    cell_w: usize,
    has_leading_gutter: bool,
    tab_size: usize,
    theme: &ThemePalette,
    gutter: Style,
) -> Vec<Span<'static>> {
    let g_cols = usize::from(has_leading_gutter);
    let mut spans = Vec::new();
    match input {
        Some(inp) => {
            if let Some(g) = inp.leading_gutter {
                spans.push(g);
            }
            spans.push(Span::styled(format!("{} ", format_lineno(lineno(inp.line))), gutter));
            spans.push(Span::styled(inp.marker.to_string(), inp.base));
            let text_w = cell_w.saturating_sub(DIFF_LINE_NUM_W + 2 + g_cols);
            let wr = inp.custom_word_ranges.unwrap_or(&inp.line.word_ranges);
            let mut body = render_diff_line_spans_with_tab(
                &inp.line.text,
                inp.tokens,
                wr,
                text_w,
                tab_size,
                inp.base,
                inp.word_bg,
                theme.true_color,
            );
            let used: usize = body.iter().map(|s| s.content.width()).sum();
            body.push(Span::styled(
                " ".repeat(text_w.saturating_sub(used)),
                inp.base,
            ));
            spans.extend(body);
        }
        None => {
            spans.push(Span::styled(" ".repeat(DIFF_LINE_NUM_W + 1 + g_cols), gutter));
            spans.push(Span::styled(
                " ".repeat(cell_w.saturating_sub(DIFF_LINE_NUM_W + 1 + g_cols)),
                Style::default().bg(theme.bg_panel),
            ));
        }
    }
    spans
}

use super::commit_details::render_commit_details;
use super::{DiffHost, muted_line};

pub fn render_details_or_diff(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &AppState,
    ctx: &mut RenderContext<'_>,
) {
    if state.log.details.diff_preview && state.log.details.diff_dock == DiffPreviewDock::Details {
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
        .log
        .details
        .selected_change(&state.diff_caches)
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
    let files = state.log.details.file_row_positions(&state.diff_caches);
    let pos = files
        .iter()
        .position(|&p| p == state.log.details.changes.selected)
        .map_or(0, |i| i + 1);
    let path = state
        .log
        .details
        .selected_change(&state.diff_caches)
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
    render_diff_preview(
        frame,
        area,
        DiffPreviewProps {
            changes: &state.log.details.changes,
            diff_session: &state.log.details.diff,
            caches: &state.diff_caches,
            host: DiffHost::Log,
            diff_dock: state.log.details.diff_dock,
            zoomed,
        },
        ctx,
    );
}

pub struct FileDiffRenderProps<'a> {
    pub diff: &'a Arc<FileDiff>,
    pub rows_cache: &'a FileDiffRowsCache,
    pub cached_syntax: Option<&'a HighlightedDiff>,
    pub side_by_side: bool,
    pub diff_scroll: usize,
    pub scroll_pane: ScrollPane,
}

pub fn render_lfs_diff_placeholder(
    frame: &mut Frame<'_>,
    area: Rect,
    lfs: &jbgit_core::LfsDiff,
    extra_hint_key: Option<&str>,
    ctx: &RenderContext<'_>,
) {
    let fmt_ptr = |opt: Option<&jbgit_core::LfsPointer>| match opt {
        Some(p) => format!("sha256:{}  ({})", p.short_oid(), p.format_size()),
        None => ctx.t("view.log_details.lfs_none").to_string(),
    };
    let mut lines = vec![
        Line::from(Span::styled(
            format!(" {} ", ctx.t("view.log_details.lfs_title")),
            Style::default()
                .fg(ctx.theme.fg_bright)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(vec![
            Span::styled(
                format!(" {}: ", ctx.t("view.log_details.lfs_old")),
                Style::default().fg(ctx.theme.fg_muted),
            ),
            Span::styled(fmt_ptr(lfs.old.as_ref()), Style::default().fg(ctx.theme.fg_primary)),
        ]),
        Line::from(vec![
            Span::styled(
                format!(" {}: ", ctx.t("view.log_details.lfs_new")),
                Style::default().fg(ctx.theme.fg_muted),
            ),
            Span::styled(
                fmt_ptr(lfs.new.as_ref()),
                Style::default().fg(ctx.theme.status_modified),
            ),
        ]),
    ];
    if lfs.filter_detected {
        lines.push(Line::from(Span::styled(
            format!(" {}", ctx.t("view.log_details.lfs_filter_active")),
            Style::default().fg(ctx.theme.fg_muted),
        )));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        format!(" {}", ctx.t("view.log_details.lfs_hint")),
        Style::default().fg(ctx.theme.fg_muted),
    )));
    if let Some(key) = extra_hint_key {
        lines.push(Line::from(Span::styled(
            format!(" {}", ctx.t(key)),
            Style::default().fg(ctx.theme.fg_muted),
        )));
    }
    frame.render_widget(Paragraph::new(lines), area);
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
    if let Some(lfs) = &diff.lfs {
        ctx.record_scroll_bound(scroll_pane, h, 0);
        render_lfs_diff_placeholder(frame, body, lfs, None, ctx);
        return;
    }
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
    let mut body = body;
    if let Some(mc) = &diff.mode_change {
        let banner_rect = Rect::new(body.x, body.y, body.width, 1);
        render_mode_change_banner(frame, banner_rect, mc, ctx);
        if diff.hunks.is_empty() {
            ctx.record_scroll_bound(scroll_pane, h, 0);
            if body.height > 1 {
                let hint_rect = Rect::new(body.x, body.y + 1, body.width, body.height - 1);
                frame.render_widget(muted_line(ctx, ctx.t("view.diff.mode_only_hint")), hint_rect);
            }
            return;
        }
        if body.height <= 1 {
            ctx.record_scroll_bound(scroll_pane, 0, 0);
            return;
        }
        body = Rect::new(body.x, body.y + 1, body.width, body.height - 1);
    } else if diff.hunks.is_empty() {
        ctx.record_scroll_bound(scroll_pane, h, 0);
        frame.render_widget(
            muted_line(ctx, ctx.t("view.log_details.no_text_diff")),
            body,
        );
        return;
    }
    let h = body.height as usize;

    let rows = rows_cache.rows_for(diff, side_by_side);
    let mut total = rows.len();
    if diff.is_truncated {
        total += 1;
    }
    let max_scroll = total.saturating_sub(h);
    ctx.record_scroll_bound(scroll_pane, h, max_scroll);
    let scroll = diff_scroll.min(max_scroll);
    let theme = ctx.theme;
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

    let gutter = Style::default().fg(theme.fg_muted).bg(theme.bg_panel);
    fn make_file_input<'a>(
        diff: &'a FileDiff,
        hl_diff: &'a HighlightedDiff,
        theme: &ThemePalette,
        hh: usize,
        li: usize,
        custom_word_ranges: Option<&'a [(usize, usize)]>,
    ) -> DiffLineRenderInput<'a> {
        let line = &diff.hunks[hh].lines[li];
        let tokens = hl_diff
            .hunks
            .get(hh)
            .and_then(|h| h.get(li))
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let (base, word_bg, marker) = diff_line_style(theme, line.kind);
        DiffLineRenderInput {
            line,
            tokens,
            custom_word_ranges,
            base,
            word_bg,
            marker,
            leading_gutter: None,
        }
    }

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
                let lazy_wr = rows_cache.word_cache.unified_ranges(diff, *hh, *l, hunk_lines);
                let inp = make_file_input(diff, hl_diff, theme, *hh, *l, lazy_wr.as_deref());
                let base = inp.base;
                let spans = render_diff_unified_spans(inp, width, tab_size, theme, gutter);
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
                let mut spans = render_diff_side_cell(
                    left.map(|li| make_file_input(diff, hl_diff, theme, *hh, li, lazy_l.as_deref())),
                    |l| l.old_lineno,
                    half,
                    false,
                    tab_size,
                    theme,
                    gutter,
                );
                spans.push(Span::styled(
                    "│",
                    Style::default().fg(theme.border_splitter),
                ));
                spans.extend(render_diff_side_cell(
                    right.map(|li| {
                        make_file_input(diff, hl_diff, theme, *hh, li, lazy_r.as_deref())
                    }),
                    |l| l.new_lineno,
                    width - half - 1,
                    false,
                    tab_size,
                    theme,
                    gutter,
                ));
                Line::from(spans)
            }
        };
        frame.render_widget(Paragraph::new(line), rect);
    }
}

pub struct DiffPreviewProps<'a> {
    pub changes: &'a ChangesTreeSession,
    pub diff_session: &'a DiffSession,
    pub caches: &'a DiffCacheStore,
    pub host: DiffHost,
    pub diff_dock: DiffPreviewDock,
    pub zoomed: bool,
}

pub fn render_diff_preview(
    frame: &mut Frame<'_>,
    area: Rect,
    props: DiffPreviewProps<'_>,
    ctx: &mut RenderContext<'_>,
) {
    let DiffPreviewProps {
        changes,
        diff_session,
        caches,
        host,
        diff_dock,
        zoomed,
    } = props;
    if area.width == 0 || area.height == 0 {
        return;
    }
    let mut buttons = vec![
        (
            if diff_session.side_by_side {
                ctx.t("view.log_details.side_by_side_btn")
            } else {
                ctx.t("view.log_details.unified_btn")
            }
            .to_string(),
            diff_session.side_by_side,
            DiffToolbarButton::SideBySide,
        ),
        (
            ctx.t_fmt(
                "view.log_details.ctx_btn",
                &[("ctx", ctx.diff_context_label(diff_session.context))],
            ),
            false,
            DiffToolbarButton::Context,
        ),
    ];
    if host == DiffHost::Log {
        if changes
            .current_key
            .as_ref()
            .is_some_and(|k| k.mode == jbgit_core::DetailsMode::CompareVersions && k.oids.len() >= 2)
        {
            buttons.push((
                ctx.t("view.log_details.toolbar_swap").to_string(),
                false,
                DiffToolbarButton::SwapCompare,
            ));
        }
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
                    &[("dock", ctx.diff_dock_label(diff_dock))],
                ),
                diff_dock != DiffPreviewDock::Details,
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
    let Some(change) = changes.selected_change(caches) else {
        ctx.record_scroll_bound(host.scroll_pane(), h, 0);
        let msg = if changes.current_bundle(caches).is_none() {
            ctx.t("common.loading")
        } else {
            ctx.t("view.log_details.select_file_diff")
        };
        frame.render_widget(muted_line(ctx, msg), body);
        return;
    };
    let desired_key = changes.desired_diff_key(caches, diff_session.context);
    let Some(diff) = desired_key.as_ref().and_then(|k| caches.peek_diff(k)) else {
        ctx.record_scroll_bound(host.scroll_pane(), h, 0);
        if change.is_binary {
            frame.render_widget(muted_line(ctx, ctx.t("view.log_details.binary_diff")), body);
        } else {
            frame.render_widget(muted_line(ctx, ctx.t("view.log_details.loading_diff")), body);
        }
        return;
    };
    let cached_syntax = desired_key
        .as_ref()
        .and_then(|k| caches.peek_syntax(k))
        .map(Arc::as_ref);
    render_file_diff_body(
        frame,
        body,
        FileDiffRenderProps {
            diff,
            rows_cache: &diff_session.rows_cache,
            cached_syntax,
            side_by_side: diff_session.side_by_side,
            diff_scroll: diff_session.scroll.offset(),
            scroll_pane: host.scroll_pane(),
        },
        ctx,
    );
}
