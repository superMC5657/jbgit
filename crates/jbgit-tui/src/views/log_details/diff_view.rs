use std::sync::Arc;

use jbgit_core::{
    BinaryDiffInfo, DiffLine, DiffLineKind, FileDiff, FileModeChange, PaneId, WhitespaceMode,
    format_byte_size,
};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use unicode_width::UnicodeWidthStr;

use crate::app::{AppState, RenderContext, ScrollPane};
use crate::diff_session::{
    ChangesTreeSession, DiffCacheStore, DiffRow, DiffSearchState, DiffSession,
    diff_row_visual_height,
};
use crate::hit_test::{DiffToolbarButton, HitTarget};
use crate::log_view::DiffPreviewDock;
use crate::syntax::{
    DiffLineRenderOptions, HighlightedDiff, SyntaxToken, render_diff_line_spans_opts,
    wrap_diff_line_spans_opts,
};
use crate::theme::ThemePalette;
use crate::views::{fit_left_right_spans, render_pane_frame, render_toolbar, truncate_to_width};

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

pub(crate) fn diff_search_styles(theme: &ThemePalette) -> (Style, Style) {
    let hit = Style::default()
        .fg(theme.bg_canvas)
        .bg(theme.status_modified)
        .add_modifier(Modifier::BOLD);
    let active = Style::default()
        .fg(theme.fg_bright)
        .bg(theme.border_focused)
        .add_modifier(Modifier::BOLD);
    (hit, active)
}

pub(crate) struct DiffLineRenderInput<'a> {
    pub line: &'a DiffLine,
    pub tokens: &'a [SyntaxToken],
    pub custom_word_ranges: Option<&'a [(usize, usize)]>,
    pub search_ranges: &'a [(usize, usize)],
    pub active_search_range: Option<(usize, usize)>,
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

fn line_opts<'a>(
    input: &DiffLineRenderInput<'a>,
    max_width: usize,
    tab_size: usize,
    horiz_scroll: usize,
    theme: &ThemePalette,
) -> DiffLineRenderOptions<'a> {
    let (search_style, active_search_style) = diff_search_styles(theme);
    DiffLineRenderOptions {
        max_width,
        tab_size,
        horiz_scroll,
        base: input.base,
        word_bg: input.word_bg,
        search_ranges: input.search_ranges,
        active_search_range: input.active_search_range,
        search_style,
        active_search_style,
        true_color: theme.true_color,
    }
}

pub(crate) fn render_diff_unified_spans(
    input: DiffLineRenderInput<'_>,
    width: usize,
    tab_size: usize,
    horiz_scroll: usize,
    theme: &ThemePalette,
    gutter: Style,
) -> Vec<Span<'static>> {
    let has_gutter = usize::from(input.leading_gutter.is_some());
    let mut spans = Vec::with_capacity(6);
    if let Some(g) = input.leading_gutter.clone() {
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
    let opts = line_opts(&input, text_w, tab_size, horiz_scroll, theme);
    spans.extend(render_diff_line_spans_opts(
        &input.line.text,
        input.tokens,
        wr,
        opts,
    ));
    spans
}

pub(crate) fn render_diff_unified_wrapped_rows(
    input: DiffLineRenderInput<'_>,
    width: usize,
    tab_size: usize,
    theme: &ThemePalette,
    gutter: Style,
) -> Vec<Vec<Span<'static>>> {
    let has_gutter = usize::from(input.leading_gutter.is_some());
    let text_w = width.saturating_sub(DIFF_LINE_NUM_W * 2 + 3 + has_gutter).max(1);
    let wr = input.custom_word_ranges.unwrap_or(&input.line.word_ranges);
    let opts = line_opts(&input, text_w, tab_size, 0, theme);
    let wrapped = wrap_diff_line_spans_opts(&input.line.text, input.tokens, wr, opts);
    let blank_nums = format!("{} {} ", " ".repeat(DIFF_LINE_NUM_W), " ".repeat(DIFF_LINE_NUM_W));
    let first_nums = format!(
        "{} {} ",
        format_lineno(input.line.old_lineno),
        format_lineno(input.line.new_lineno)
    );
    wrapped
        .into_iter()
        .enumerate()
        .map(|(sub_idx, body)| {
            let mut spans = Vec::with_capacity(body.len() + 3);
            if let Some(g) = &input.leading_gutter {
                if sub_idx == 0 {
                    spans.push(g.clone());
                } else {
                    spans.push(Span::styled(" ", gutter));
                }
            }
            spans.push(Span::styled(
                if sub_idx == 0 {
                    first_nums.clone()
                } else {
                    blank_nums.clone()
                },
                gutter,
            ));
            spans.push(Span::styled(
                if sub_idx == 0 {
                    input.marker.to_string()
                } else {
                    " ".to_string()
                },
                input.base,
            ));
            spans.extend(body);
            spans
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn render_diff_side_cell(
    input: Option<DiffLineRenderInput<'_>>,
    lineno: fn(&DiffLine) -> Option<u32>,
    cell_w: usize,
    has_leading_gutter: bool,
    tab_size: usize,
    horiz_scroll: usize,
    theme: &ThemePalette,
    gutter: Style,
) -> Vec<Span<'static>> {
    let g_cols = usize::from(has_leading_gutter);
    let mut spans = Vec::new();
    match input {
        Some(inp) => {
            if let Some(g) = inp.leading_gutter.clone() {
                spans.push(g);
            }
            spans.push(Span::styled(format!("{} ", format_lineno(lineno(inp.line))), gutter));
            spans.push(Span::styled(inp.marker.to_string(), inp.base));
            let text_w = cell_w.saturating_sub(DIFF_LINE_NUM_W + 2 + g_cols);
            let wr = inp.custom_word_ranges.unwrap_or(&inp.line.word_ranges);
            let opts = line_opts(&inp, text_w, tab_size, horiz_scroll, theme);
            let mut body = render_diff_line_spans_opts(&inp.line.text, inp.tokens, wr, opts);
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

#[allow(clippy::too_many_arguments)]
pub(crate) fn render_diff_side_wrapped_cells(
    input: Option<DiffLineRenderInput<'_>>,
    lineno: fn(&DiffLine) -> Option<u32>,
    cell_w: usize,
    has_leading_gutter: bool,
    tab_size: usize,
    theme: &ThemePalette,
    gutter: Style,
) -> Vec<Vec<Span<'static>>> {
    let g_cols = usize::from(has_leading_gutter);
    let empty_row = || {
        vec![
            Span::styled(" ".repeat(DIFF_LINE_NUM_W + 1 + g_cols), gutter),
            Span::styled(
                " ".repeat(cell_w.saturating_sub(DIFF_LINE_NUM_W + 1 + g_cols)),
                Style::default().bg(theme.bg_panel),
            ),
        ]
    };
    let Some(inp) = input else {
        return vec![empty_row()];
    };
    let text_w = cell_w.saturating_sub(DIFF_LINE_NUM_W + 2 + g_cols).max(1);
    let wr = inp.custom_word_ranges.unwrap_or(&inp.line.word_ranges);
    let opts = line_opts(&inp, text_w, tab_size, 0, theme);
    let wrapped = wrap_diff_line_spans_opts(&inp.line.text, inp.tokens, wr, opts);
    let first_num = format!("{} ", format_lineno(lineno(inp.line)));
    let blank_num = format!("{} ", " ".repeat(DIFF_LINE_NUM_W));
    wrapped
        .into_iter()
        .enumerate()
        .map(|(sub_idx, mut body)| {
            let mut spans = Vec::with_capacity(body.len() + 4);
            if let Some(g) = &inp.leading_gutter {
                if sub_idx == 0 {
                    spans.push(g.clone());
                } else {
                    spans.push(Span::styled(" ", gutter));
                }
            }
            spans.push(Span::styled(
                if sub_idx == 0 {
                    first_num.clone()
                } else {
                    blank_num.clone()
                },
                gutter,
            ));
            spans.push(Span::styled(
                if sub_idx == 0 {
                    inp.marker.to_string()
                } else {
                    " ".to_string()
                },
                inp.base,
            ));
            let used: usize = body.iter().map(|s| s.content.width()).sum();
            body.push(Span::styled(
                " ".repeat(text_w.saturating_sub(used)),
                inp.base,
            ));
            spans.extend(body);
            spans
        })
        .collect()
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
    let mut subtitle = state
        .log
        .details
        .selected_change(&state.diff_caches)
        .map(|c| ctx.t_fmt("view.log_details.diff_file", &[("path", &c.path)]))
        .unwrap_or_else(|| ctx.t("view.log_details.diff_preview").to_string());
    if !state.log.details.diff.soft_wrap && state.log.details.diff.horiz_scroll > 0 {
        subtitle.push_str(&format!("  Col +{}", state.log.details.diff.horiz_scroll));
    }
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
    let mut header = ctx.t_fmt(
        "view.log_details.zoom_header",
        &[
            ("path", &path),
            ("pos", &pos.to_string()),
            ("total", &files.len().to_string()),
        ],
    );
    if !state.log.details.diff.soft_wrap && state.log.details.diff.horiz_scroll > 0 {
        header.push_str(&format!("  Col +{}", state.log.details.diff.horiz_scroll));
    }
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
    pub session: &'a DiffSession,
    pub cached_syntax: Option<&'a HighlightedDiff>,
    pub scroll_pane: ScrollPane,
}

pub(crate) fn render_binary_diff_card(
    frame: &mut Frame<'_>,
    area: Rect,
    binary: Option<&BinaryDiffInfo>,
    ctx: &RenderContext<'_>,
) {
    let Some(info) = binary else {
        frame.render_widget(muted_line(ctx, ctx.t("view.log_details.binary_diff")), area);
        return;
    };
    let missing = ctx.t("view.log_details.binary_none");
    let fmt_side = |size: Option<u64>, oid: Option<jbgit_core::Oid>| -> String {
        match (oid, size) {
            (Some(o), Some(s)) => format!("{}  ({} / {} B)", o.short(), format_byte_size(s), s),
            (None, Some(s)) => format!("{} ({} B)", format_byte_size(s), s),
            (Some(o), None) => o.short(),
            (None, None) => missing.to_string(),
        }
    };
    let size_summary = info.size_summary(missing);
    let delta_color = match (info.old_size, info.new_size) {
        (Some(o), Some(n)) if n > o => ctx.theme.status_added,
        (Some(o), Some(n)) if n < o => ctx.theme.status_conflicted,
        (None, Some(_)) => ctx.theme.status_added,
        (Some(_), None) => ctx.theme.status_conflicted,
        _ => ctx.theme.fg_primary,
    };
    let lines = vec![
        Line::from(Span::styled(
            format!(" {} ", ctx.t("view.log_details.binary_title")),
            Style::default()
                .fg(ctx.theme.fg_bright)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(vec![
            Span::styled(
                format!(" {}: ", ctx.t("view.log_details.binary_size")),
                Style::default().fg(ctx.theme.fg_muted),
            ),
            Span::styled(
                size_summary,
                Style::default().fg(delta_color).add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(vec![
            Span::styled(
                format!(" {}: ", ctx.t("view.log_details.binary_old")),
                Style::default().fg(ctx.theme.fg_muted),
            ),
            Span::styled(
                fmt_side(info.old_size, info.old_oid),
                Style::default().fg(ctx.theme.fg_primary),
            ),
        ]),
        Line::from(vec![
            Span::styled(
                format!(" {}: ", ctx.t("view.log_details.binary_new")),
                Style::default().fg(ctx.theme.fg_muted),
            ),
            Span::styled(
                fmt_side(info.new_size, info.new_oid),
                Style::default().fg(ctx.theme.status_modified),
            ),
        ]),
    ];
    frame.render_widget(Paragraph::new(lines), area);
}

pub(crate) fn render_diff_search_bar(
    frame: &mut Frame<'_>,
    area: Rect,
    search: &DiffSearchState,
    ctx: &RenderContext<'_>,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let cursor = if search.editing { "▌" } else { "" };
    let count = if search.query.is_empty() {
        String::new()
    } else {
        format!("  {}", search.count_badge())
    };
    let count_color = if search.matches.is_empty() && !search.query.is_empty() {
        ctx.theme.status_conflicted
    } else {
        ctx.theme.fg_accent_link
    };
    let left = vec![
        Span::styled(
            " /",
            Style::default()
                .fg(ctx.theme.fg_accent_link)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("{}{}", search.query, cursor),
            Style::default().fg(ctx.theme.fg_bright),
        ),
        Span::styled(count, Style::default().fg(count_color)),
    ];
    let left_w: usize = left.iter().map(|s| s.content.width()).sum();
    let hint_key = if search.editing {
        "view.log_details.search_hint_editing"
    } else {
        "view.log_details.search_hint_nav"
    };
    let hint_text = format!("{} ", ctx.t(hint_key));
    let right = if left_w + 2 + hint_text.width() <= area.width as usize {
        vec![Span::styled(
            hint_text,
            Style::default().fg(ctx.theme.fg_muted),
        )]
    } else {
        Vec::new()
    };
    let spans = fit_left_right_spans(left, right, area.width as usize, 1);
    frame.render_widget(
        Paragraph::new(Line::from(spans)).style(Style::default().bg(ctx.theme.bg_panel)),
        area,
    );
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

/// Renders a hunk header line (`@@ ... @@`) truncated to `max_width` with inline
/// search match highlights applied.
pub(crate) fn render_hunk_header_spans(
    header: &str,
    max_width: usize,
    base: Style,
    search_ranges: &[(usize, usize)],
    active_search: Option<(usize, usize)>,
    theme: &ThemePalette,
) -> Vec<Span<'static>> {
    if search_ranges.is_empty() && active_search.is_none() {
        return vec![Span::styled(truncate_to_width(header, max_width), base)];
    }
    let truncated = truncate_to_width(header, max_width);
    let len = truncated.len();
    let mut cuts = vec![0usize, len];
    for &(s, e) in search_ranges {
        let s = s.min(len);
        let e = e.min(len);
        if s < e && truncated.is_char_boundary(s) && truncated.is_char_boundary(e) {
            cuts.push(s);
            cuts.push(e);
        }
    }
    if let Some((s, e)) = active_search {
        let s = s.min(len);
        let e = e.min(len);
        if s < e && truncated.is_char_boundary(s) && truncated.is_char_boundary(e) {
            cuts.push(s);
            cuts.push(e);
        }
    }
    cuts.sort_unstable();
    cuts.dedup();

    let mut out = Vec::with_capacity(cuts.len());
    for pair in cuts.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        if a >= b {
            continue;
        }
        let is_active = active_search.is_some_and(|(s, e)| a >= s && b <= e);
        let is_match = search_ranges.iter().any(|&(s, e)| a >= s && b <= e);
        let style = if is_active {
            Style::default()
                .fg(theme.bg_canvas)
                .bg(theme.status_modified)
                .add_modifier(Modifier::BOLD)
        } else if is_match {
            Style::default()
                .fg(theme.fg_primary)
                .bg(theme.bg_selection_focused)
        } else {
            base
        };
        out.push(Span::styled(truncated[a..b].to_string(), style));
    }
    out
}

pub fn render_file_diff_body(
    frame: &mut Frame<'_>,
    area: Rect,
    props: FileDiffRenderProps<'_>,
    ctx: &mut RenderContext<'_>,
) {
    let FileDiffRenderProps {
        diff,
        session,
        cached_syntax,
        scroll_pane,
    } = props;
    if area.width == 0 || area.height == 0 {
        return;
    }

    let show_search_bar = session.search.active && area.height >= 2;
    let body = if show_search_bar {
        let search_rect = Rect::new(area.x, area.bottom() - 1, area.width, 1);
        render_diff_search_bar(frame, search_rect, &session.search, ctx);
        Rect::new(area.x, area.y, area.width, area.height - 1)
    } else {
        area
    };

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
        render_binary_diff_card(frame, body, diff.binary.as_ref(), ctx);
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
        let msg_key = if diff.key.whitespace != WhitespaceMode::Default {
            "view.log_details.no_diff_ignoring_ws"
        } else {
            "view.log_details.no_text_diff"
        };
        frame.render_widget(muted_line(ctx, ctx.t(msg_key)), body);
        return;
    }
    let h = body.height as usize;

    let width = body.width as usize;
    let half = width / 2;
    let unified_text_w = width.saturating_sub(DIFF_LINE_NUM_W * 2 + 3).max(1);
    let split_left_w = half.saturating_sub(DIFF_LINE_NUM_W + 2).max(1);
    let split_right_w = width
        .saturating_sub(half + 1)
        .saturating_sub(DIFF_LINE_NUM_W + 2)
        .max(1);
    session.record_wrap_widths(unified_text_w, split_left_w, split_right_w);

    let rows = session.rows_for(diff);
    let theme = ctx.theme;
    let tab_size = ctx.diff_tab_size();
    session.record_tab_size(tab_size);
    let horiz_scroll = if session.soft_wrap {
        0
    } else {
        session.horiz_scroll as usize
    };
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
    #[allow(clippy::too_many_arguments)]
    fn make_file_input<'a>(
        diff: &'a FileDiff,
        hl_diff: &'a HighlightedDiff,
        theme: &ThemePalette,
        hh: usize,
        li: usize,
        custom_word_ranges: Option<&'a [(usize, usize)]>,
        search_ranges: &'a [(usize, usize)],
        active_search_range: Option<(usize, usize)>,
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
            search_ranges,
            active_search_range,
            base,
            word_bg,
            marker,
            leading_gutter: None,
        }
    }

    if !session.soft_wrap {
        let mut total = rows.len();
        if diff.is_truncated {
            total += 1;
        }
        let max_scroll = total.saturating_sub(h);
        ctx.record_scroll_bound(scroll_pane, h, max_scroll);
        let scroll = session.scroll.offset().min(max_scroll);

        for (vis_y, row_idx) in (scroll..total).take(h).enumerate() {
            let rect = Rect::new(body.x, body.y + vis_y as u16, body.width, 1);
            let line = match rows.get(row_idx) {
                None => Line::from(Span::styled(
                    ctx.t("view.log_details.diff_truncated").to_string(),
                    Style::default().fg(theme.status_conflicted),
                )),
                Some(DiffRow::Header(hh)) => {
                    let (sr, active_sr) = session.search.ranges_for(row_idx, 0);
                    let base = Style::default()
                        .fg(theme.fg_accent_link)
                        .bg(theme.diff_mod_bg);
                    Line::from(render_hunk_header_spans(
                        &diff.hunks[*hh].header,
                        width,
                        base,
                        &sr,
                        active_sr,
                        theme,
                    ))
                    .style(Style::default().bg(theme.diff_mod_bg))
                }
                Some(DiffRow::Unified(hh, l)) => {
                    let hunk_lines = &diff.hunks[*hh].lines;
                    let lazy_wr = session
                        .rows_cache
                        .word_cache
                        .unified_ranges(diff, *hh, *l, hunk_lines);
                    let (sr, active_sr) = session.search.ranges_for(row_idx, 0);
                    let inp = make_file_input(
                        diff,
                        hl_diff,
                        theme,
                        *hh,
                        *l,
                        lazy_wr.as_deref(),
                        &sr,
                        active_sr,
                    );
                    let base = inp.base;
                    let spans =
                        render_diff_unified_spans(inp, width, tab_size, horiz_scroll, theme, gutter);
                    Line::from(spans).style(base)
                }
                Some(DiffRow::Split(hh, left, right)) => {
                    let (lazy_l, lazy_r) = session.rows_cache.word_cache.split_ranges(
                        diff,
                        *hh,
                        *left,
                        *right,
                        &diff.hunks[*hh].lines,
                    );
                    let (sr_l, active_l) = session.search.ranges_for(row_idx, 0);
                    let (sr_r, active_r) = session.search.ranges_for(row_idx, 1);
                    let mut spans = render_diff_side_cell(
                        left.map(|li| {
                            make_file_input(
                                diff,
                                hl_diff,
                                theme,
                                *hh,
                                li,
                                lazy_l.as_deref(),
                                &sr_l,
                                active_l,
                            )
                        }),
                        |l| l.old_lineno,
                        half,
                        false,
                        tab_size,
                        horiz_scroll,
                        theme,
                        gutter,
                    );
                    spans.push(Span::styled(
                        "│",
                        Style::default().fg(theme.border_splitter),
                    ));
                    spans.extend(render_diff_side_cell(
                        right.map(|li| {
                            make_file_input(
                                diff,
                                hl_diff,
                                theme,
                                *hh,
                                li,
                                lazy_r.as_deref(),
                                &sr_r,
                                active_r,
                            )
                        }),
                        |l| l.new_lineno,
                        width.saturating_sub(half + 1),
                        false,
                        tab_size,
                        horiz_scroll,
                        theme,
                        gutter,
                    ));
                    Line::from(spans)
                }
            };
            frame.render_widget(Paragraph::new(line), rect);
        }
        return;
    }

    // Soft-wrap mode: compute visual row heights and render visible wrapped sub-rows.
    let heights: Vec<usize> = rows
        .iter()
        .map(|&r| {
            diff_row_visual_height(
                r,
                |hh| &diff.hunks[hh],
                true,
                unified_text_w,
                split_left_w,
                split_right_w,
                tab_size,
            )
        })
        .collect();
    let total_vis: usize = heights.iter().sum::<usize>() + usize::from(diff.is_truncated);
    let max_scroll = total_vis.saturating_sub(h);
    ctx.record_scroll_bound(scroll_pane, h, max_scroll);
    let scroll = session.scroll.offset().min(max_scroll);

    let mut cur_vis = 0usize;
    let mut drawn = 0usize;
    for (row_idx, (&row, &row_h)) in rows.iter().zip(heights.iter()).enumerate() {
        let row_end = cur_vis + row_h;
        if row_end <= scroll {
            cur_vis = row_end;
            continue;
        }
        if drawn >= h {
            break;
        }
        let skip_sub = scroll.saturating_sub(cur_vis);
        let take_sub = (row_h - skip_sub).min(h - drawn);
        match row {
            DiffRow::Header(hh) => {
                let rect = Rect::new(body.x, body.y + drawn as u16, body.width, 1);
                let (sr, active_sr) = session.search.ranges_for(row_idx, 0);
                let base = Style::default()
                    .fg(theme.fg_accent_link)
                    .bg(theme.diff_mod_bg);
                let line = Line::from(render_hunk_header_spans(
                    &diff.hunks[hh].header,
                    width,
                    base,
                    &sr,
                    active_sr,
                    theme,
                ))
                .style(Style::default().bg(theme.diff_mod_bg));
                frame.render_widget(Paragraph::new(line), rect);
                drawn += 1;
            }
            DiffRow::Unified(hh, l) => {
                let hunk_lines = &diff.hunks[hh].lines;
                let lazy_wr = session
                    .rows_cache
                    .word_cache
                    .unified_ranges(diff, hh, l, hunk_lines);
                let (sr, active_sr) = session.search.ranges_for(row_idx, 0);
                let inp = make_file_input(
                    diff,
                    hl_diff,
                    theme,
                    hh,
                    l,
                    lazy_wr.as_deref(),
                    &sr,
                    active_sr,
                );
                let base = inp.base;
                let wrapped = render_diff_unified_wrapped_rows(inp, width, tab_size, theme, gutter);
                for sub_spans in wrapped.into_iter().skip(skip_sub).take(take_sub) {
                    let rect = Rect::new(body.x, body.y + drawn as u16, body.width, 1);
                    frame.render_widget(Paragraph::new(Line::from(sub_spans).style(base)), rect);
                    drawn += 1;
                }
            }
            DiffRow::Split(hh, left, right) => {
                let (lazy_l, lazy_r) = session.rows_cache.word_cache.split_ranges(
                    diff,
                    hh,
                    left,
                    right,
                    &diff.hunks[hh].lines,
                );
                let (sr_l, active_l) = session.search.ranges_for(row_idx, 0);
                let (sr_r, active_r) = session.search.ranges_for(row_idx, 1);
                let right_cell_w = width.saturating_sub(half + 1);
                let left_rows = render_diff_side_wrapped_cells(
                    left.map(|li| {
                        make_file_input(
                            diff,
                            hl_diff,
                            theme,
                            hh,
                            li,
                            lazy_l.as_deref(),
                            &sr_l,
                            active_l,
                        )
                    }),
                    |l| l.old_lineno,
                    half,
                    false,
                    tab_size,
                    theme,
                    gutter,
                );
                let right_rows = render_diff_side_wrapped_cells(
                    right.map(|li| {
                        make_file_input(
                            diff,
                            hl_diff,
                            theme,
                            hh,
                            li,
                            lazy_r.as_deref(),
                            &sr_r,
                            active_r,
                        )
                    }),
                    |l| l.new_lineno,
                    right_cell_w,
                    false,
                    tab_size,
                    theme,
                    gutter,
                );
                let empty_cell = |w: usize| {
                    vec![
                        Span::styled(" ".repeat(DIFF_LINE_NUM_W + 1), gutter),
                        Span::styled(
                            " ".repeat(w.saturating_sub(DIFF_LINE_NUM_W + 1)),
                            Style::default().bg(theme.bg_panel),
                        ),
                    ]
                };
                for sub_i in skip_sub..(skip_sub + take_sub) {
                    let mut spans = left_rows
                        .get(sub_i)
                        .cloned()
                        .unwrap_or_else(|| empty_cell(half));
                    spans.push(Span::styled(
                        "│",
                        Style::default().fg(theme.border_splitter),
                    ));
                    spans.extend(
                        right_rows
                            .get(sub_i)
                            .cloned()
                            .unwrap_or_else(|| empty_cell(right_cell_w)),
                    );
                    let rect = Rect::new(body.x, body.y + drawn as u16, body.width, 1);
                    frame.render_widget(Paragraph::new(Line::from(spans)), rect);
                    drawn += 1;
                }
            }
        }
        cur_vis = row_end;
    }

    if diff.is_truncated && drawn < h && cur_vis >= scroll {
        let rect = Rect::new(body.x, body.y + drawn as u16, body.width, 1);
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                ctx.t("view.log_details.diff_truncated").to_string(),
                Style::default().fg(theme.status_conflicted),
            ))),
            rect,
        );
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
    if host == DiffHost::Log
        && changes
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
    buttons.push((
        ctx.t_fmt(
            "view.log_details.ws_btn",
            &[("ws", ctx.diff_whitespace_label(diff_session.whitespace))],
        ),
        diff_session.whitespace != WhitespaceMode::Default,
        DiffToolbarButton::Whitespace,
    ));
    buttons.push((
        ctx.t("view.log_details.wrap_btn").to_string(),
        diff_session.soft_wrap,
        DiffToolbarButton::SoftWrap,
    ));
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
    let desired_key = changes.desired_diff_key(caches, diff_session.context, diff_session.whitespace);
    let Some(diff) = desired_key.as_ref().and_then(|k| caches.peek_diff(k)) else {
        ctx.record_scroll_bound(host.scroll_pane(), h, 0);
        if change.is_binary {
            let partial = BinaryDiffInfo {
                old_size: None,
                new_size: None,
                old_oid: change.old_oid,
                new_oid: change.new_oid,
            };
            if partial.old_oid.is_some() || partial.new_oid.is_some() {
                render_binary_diff_card(frame, body, Some(&partial), ctx);
            } else {
                frame.render_widget(muted_line(ctx, ctx.t("view.log_details.binary_diff")), body);
            }
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
            session: diff_session,
            cached_syntax,
            scroll_pane: host.scroll_pane(),
        },
        ctx,
    );
}
