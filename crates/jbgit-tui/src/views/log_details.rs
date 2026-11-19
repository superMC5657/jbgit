//! Right pane: `CommitChangesBrowser` (top), `CommitDetailsPanel` or inline
//! diff preview (bottom), and the full-screen zoomed diff.

use jbgit_core::{
    ChangeRowKind, ChangeTreeRow, CommitChangesBundle, CommitDetails, DetailsMode, DiffLine,
    DiffLineKind, FileDiff, MessageLineKind, PaneId, RefKind, SignatureStatus,
    classify_message_line, issue_ref_ranges, side_by_side_rows,
};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use unicode_width::UnicodeWidthStr;

use crate::app::{AppState, RenderContext, ScrollPane};
use crate::hit_test::{ChangesToolbarButton, DiffToolbarButton, HitTarget};
use crate::log_view::{
    ChangesExpandState, DetailsPaneState, DiffPreviewDock, format_absolute_time,
};
use crate::syntax::{HighlightedDiff, render_diff_line_spans_with_tab};
use crate::views::{
    fit_left_right_spans, highlight_spans, list_viewport, render_pane_frame, render_toolbar,
    row_background, truncate_to_width, wrap_text,
};

fn muted_line(ctx: &RenderContext<'_>, text: &str) -> Paragraph<'static> {
    Paragraph::new(Span::styled(
        text.to_string(),
        Style::default().fg(ctx.theme.fg_muted),
    ))
}

fn pad_label(s: &str, width: usize) -> String {
    format!(" {s}{}", " ".repeat(width.saturating_sub(s.width())))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffHost {
    Log,
    Stash,
}

impl DiffHost {
    const fn scroll_pane(self) -> ScrollPane {
        match self {
            Self::Log => ScrollPane::LogDiff,
            Self::Stash => ScrollPane::StashDiff,
        }
    }
}

// ───────────────────────────── Changes browser ─────────────────────────────

pub fn render_changes_pane(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &AppState,
    ctx: &mut RenderContext<'_>,
) {
    render_changes_browser(
        frame,
        area,
        &state.details,
        DiffHost::Log,
        PaneId::LogChangesTree,
        ctx,
    );
}

pub fn render_changes_browser(
    frame: &mut Frame<'_>,
    area: Rect,
    details: &DetailsPaneState,
    host: DiffHost,
    pane: PaneId,
    ctx: &mut RenderContext<'_>,
) {
    let bundle = details.current_bundle();
    let subtitle = match (bundle, &details.current_key) {
        (Some(b), _) => {
            let (add, del) = b.totals();
            let stats = ctx.t_fmt(
                "view.log_details.files_stats",
                &[
                    ("count", &b.changes.len().to_string()),
                    ("add", &add.to_string()),
                    ("del", &del.to_string()),
                ],
            );
            match b.key.mode {
                DetailsMode::CompareVersions if b.key.oids.len() >= 2 => ctx.t_fmt(
                    "view.log_details.compare_versions_sub",
                    &[
                        ("a", &b.key.oids[0].short()),
                        ("b", &b.key.oids[1].short()),
                        ("stats", &stats),
                    ],
                ),
                DetailsMode::CompareWithLocal if !b.key.oids.is_empty() => ctx.t_fmt(
                    "view.log_details.compare_local_sub",
                    &[("a", &b.key.oids[0].short()), ("stats", &stats)],
                ),
                _ if b.key.oids.len() > 1 => ctx.t_fmt(
                    "view.log_details.commits_stats_sub",
                    &[
                        ("count", &b.key.oids.len().to_string()),
                        ("stats", &stats),
                    ],
                ),
                _ => stats,
            }
        }
        (None, Some(_)) => ctx.t("common.loading").to_string(),
        (None, None) => String::new(),
    };
    let body = render_pane_frame(frame, area, pane, &subtitle, ctx);
    if body.height == 0 || body.width == 0 {
        return;
    }

    let merge_parents = bundle
        .filter(|b| b.key.mode == DetailsMode::Commits && b.key.oids.len() == 1)
        .and_then(|b| b.details.first())
        .map_or(0, |d| d.parents.len());
    let expand_state = details.changes_expand_state();
    let mut buttons = vec![
        (
            ctx.t("view.log_details.toolbar_dir").to_string(),
            details.changes_group_by_dir,
            ChangesToolbarButton::GroupByDir,
        ),
        (
            " ⊕ ".to_string(),
            expand_state == ChangesExpandState::AllExpanded,
            ChangesToolbarButton::ExpandAll,
        ),
        (
            " ⊖ ".to_string(),
            expand_state == ChangesExpandState::AllCollapsed,
            ChangesToolbarButton::CollapseAll,
        ),
    ];
    if host == DiffHost::Log {
        buttons.push((
            ctx.t("view.log_details.toolbar_diff").to_string(),
            details.diff_preview,
            ChangesToolbarButton::DiffPreview,
        ));
        if merge_parents >= 2 {
            buttons.push((
                ctx.t_fmt(
                    "view.log_details.toolbar_parent",
                    &[
                        ("cur", &(details.merge_parent + 1).to_string()),
                        ("total", &merge_parents.to_string()),
                    ],
                ),
                true,
                ChangesToolbarButton::SwitchParent,
            ));
        }
    }
    render_toolbar(
        frame,
        Rect::new(body.x, body.y, body.width, 1),
        ctx,
        3,
        &buttons,
        |b| match host {
            DiffHost::Log => HitTarget::ChangesToolbar(b),
            DiffHost::Stash => HitTarget::StashChangesToolbar(b),
        },
    );

    let list = Rect::new(
        body.x,
        body.y + 1,
        body.width,
        body.height.saturating_sub(1),
    );
    if list.height == 0 {
        return;
    }
    let Some(bundle) = bundle else {
        let msg = if details.current_key.is_some() {
            ctx.t("view.log_details.loading_changes")
        } else {
            ctx.t("view.log_details.select_commit_changes")
        };
        frame.render_widget(muted_line(ctx, msg), list);
        return;
    };
    if bundle.changes.is_empty() {
        frame.render_widget(muted_line(ctx, ctx.t("view.log_details.no_changes")), list);
        return;
    }
    let rows = details.change_rows();
    render_change_rows(frame, list, details, host, pane, ctx, bundle, &rows);
}

#[allow(clippy::too_many_arguments)]
fn render_change_rows(
    frame: &mut Frame<'_>,
    area: Rect,
    details: &DetailsPaneState,
    host: DiffHost,
    pane: PaneId,
    ctx: &mut RenderContext<'_>,
    bundle: &CommitChangesBundle,
    rows: &[ChangeTreeRow],
) {
    let focused = ctx.focused_pane == pane;
    let glyphs = ctx.glyph_mode.symbols();
    let (selected, offset) = list_viewport(rows.len(), details.changes_selected, area.height as usize);
    for (vis_y, idx) in (offset..rows.len()).take(area.height as usize).enumerate() {
        let row = &rows[idx];
        let rect = Rect::new(area.x, area.y + vis_y as u16, area.width, 1);
        let row_target = match host {
            DiffHost::Log => HitTarget::LogChangeRow { idx },
            DiffHost::Stash => HitTarget::StashChangeRow(idx),
        };
        let hovered = ctx.hover_target == Some(&row_target);
        let bg = row_background(
            idx == selected,
            focused,
            hovered,
            ctx.theme.bg_canvas,
            ctx.theme,
        );
        let indent = "  ".repeat(row.depth as usize);
        let mut left: Vec<Span<'static>> = vec![Span::raw(format!(" {indent}"))];
        let mut right: Vec<Span<'static>> = Vec::new();
        match row.kind {
            ChangeRowKind::Dir { file_count } => {
                let arrow = if row.expanded {
                    glyphs.tree_expanded
                } else {
                    glyphs.tree_collapsed
                };
                left.push(Span::styled(
                    format!("{arrow} "),
                    Style::default().fg(ctx.theme.fg_muted),
                ));
                left.push(Span::styled(
                    format!("{} ", glyphs.files.folder),
                    Style::default().fg(ctx.theme.fg_muted),
                ));
                left.push(Span::styled(
                    row.label.clone(),
                    Style::default().fg(ctx.theme.fg_primary),
                ));
                right.push(Span::styled(
                    format!("{file_count} "),
                    Style::default().fg(ctx.theme.fg_muted),
                ));
            }
            ChangeRowKind::File { change_idx } => {
                let change = &bundle.changes[change_idx];
                let color = ctx.theme.status_color(change.kind);
                left.push(Span::styled(
                    format!("{} ", change.kind.badge_char()),
                    Style::default().fg(color).add_modifier(Modifier::BOLD),
                ));
                left.push(Span::styled(
                    format!("{} ", glyphs.file_icon(&change.path)),
                    Style::default().fg(ctx.theme.fg_muted),
                ));
                left.push(Span::styled(row.label.clone(), Style::default().fg(color)));
                if let Some(old) = &change.old_path {
                    left.push(Span::styled(
                        format!(" ← {}", old.rsplit('/').next().unwrap_or(old)),
                        Style::default().fg(ctx.theme.fg_muted),
                    ));
                }
                if change.is_binary {
                    right.push(Span::styled(
                        ctx.t("view.log_details.bin_badge").to_string(),
                        Style::default().fg(ctx.theme.fg_muted),
                    ));
                } else {
                    right.push(Span::styled(
                        format!("+{}", change.additions),
                        Style::default().fg(ctx.theme.status_added),
                    ));
                    right.push(Span::styled(
                        format!(" -{} ", change.deletions),
                        Style::default().fg(ctx.theme.status_conflicted),
                    ));
                }
            }
        }
        let spans = fit_left_right_spans(left, right, rect.width as usize, 1);
        frame.render_widget(
            Paragraph::new(Line::from(spans)).style(Style::default().bg(bg)),
            rect,
        );
        ctx.hit_registry.register(rect, 2, row_target);
    }
}

// ───────────────────────────── Details / diff ─────────────────────────────

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

#[derive(Clone, Copy)]
enum DiffRow {
    Header(usize),
    Unified(usize, usize),
    Split(usize, Option<usize>, Option<usize>),
}

fn diff_rows(diff: &FileDiff, side_by_side: bool) -> Vec<DiffRow> {
    let mut rows = Vec::new();
    for (h, hunk) in diff.hunks.iter().enumerate() {
        rows.push(DiffRow::Header(h));
        if side_by_side {
            rows.extend(
                side_by_side_rows(hunk)
                    .into_iter()
                    .map(|r| DiffRow::Split(h, r.left, r.right)),
            );
        } else {
            rows.extend((0..hunk.lines.len()).map(|l| DiffRow::Unified(h, l)));
        }
    }
    rows
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
    if diff.is_binary {
        ctx.record_scroll_bound(host.scroll_pane(), h, 0);
        frame.render_widget(muted_line(ctx, ctx.t("view.log_details.binary_diff")), body);
        return;
    }
    if diff.hunks.is_empty() {
        ctx.record_scroll_bound(host.scroll_pane(), h, 0);
        frame.render_widget(
            muted_line(ctx, ctx.t("view.log_details.no_text_diff")),
            body,
        );
        return;
    }

    let rows = diff_rows(diff, d.diff_side_by_side);
    let mut total = rows.len();
    if diff.is_truncated {
        total += 1;
    }
    let max_scroll = total.saturating_sub(h);
    ctx.record_scroll_bound(host.scroll_pane(), h, max_scroll);
    let scroll = d.diff_scroll.min(max_scroll);
    let theme = ctx.theme;
    let num_w = 4usize;
    let tab_size = ctx.diff_tab_size();
    let empty_hl;
    let computed_hl;
    let hl_diff: &HighlightedDiff = if !ctx.diff_syntax_enabled() {
        empty_hl = HighlightedDiff::default();
        &empty_hl
    } else {
        match d.current_diff_syntax() {
            Some(cached) => cached,
            None => {
                computed_hl = HighlightedDiff::build(diff);
                &computed_hl
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
                let mut body = render_diff_line_spans_with_tab(
                    &l.text,
                    toks,
                    &l.word_ranges,
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
                let dl = &diff.hunks[*hh].lines[*l];
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
                spans.extend(render_diff_line_spans_with_tab(
                    &dl.text,
                    toks,
                    &dl.word_ranges,
                    text_w,
                    tab_size,
                    base,
                    word_bg,
                    theme.true_color,
                ));
                Line::from(spans).style(base)
            }
            Some(DiffRow::Split(hh, left, right)) => {
                let half = width / 2;
                let mut spans = side_cell(*hh, *left, |l| l.old_lineno, half);
                spans.push(Span::styled(
                    "│",
                    Style::default().fg(theme.border_splitter),
                ));
                spans.extend(side_cell(*hh, *right, |l| l.new_lineno, width - half - 1));
                Line::from(spans)
            }
        };
        frame.render_widget(Paragraph::new(line), rect);
    }
}

fn person_line(label: &str, p: &jbgit_core::PersonStamp, ctx: &RenderContext<'_>) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            pad_label(label, 10),
            Style::default().fg(ctx.theme.fg_muted),
        ),
        Span::styled(p.name.clone(), Style::default().fg(ctx.theme.fg_bright)),
        Span::styled(
            format!(" <{}>", p.email),
            Style::default().fg(ctx.theme.fg_muted),
        ),
        Span::styled(
            format!("  {}", format_absolute_time(p.time, true)),
            Style::default().fg(ctx.theme.fg_primary),
        ),
    ])
}

/// Hit target attached to a details line at an x-offset range.
struct LineHit {
    line_idx: usize,
    x: u16,
    width: u16,
    target: HitTarget,
}

fn render_commit_details(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &AppState,
    ctx: &mut RenderContext<'_>,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    ctx.hit_registry.register(area, 1, HitTarget::DetailsArea);
    let h = area.height as usize;
    let details = &state.details;
    let Some(key) = &details.current_key else {
        ctx.record_scroll_bound(ScrollPane::LogDetails, h, 0);
        frame.render_widget(muted_line(ctx, ctx.t("view.log_details.no_commit_selected")), area);
        return;
    };
    let Some(bundle) = details.current_bundle() else {
        // Instant fallback from the snapshot while details load.
        if let [oid] = key.oids.as_slice()
            && let Some(c) = state
                .repo_snapshot
                .as_ref()
                .and_then(|s| s.commits.iter().find(|c| c.oid == *oid))
        {
            let lines = vec![
                Line::from(vec![
                    Span::styled(
                        pad_label(ctx.t("view.log_details.commit_label"), 10),
                        Style::default().fg(ctx.theme.fg_muted),
                    ),
                    Span::styled(
                        c.oid.to_string(),
                        Style::default().fg(ctx.theme.fg_accent_link),
                    ),
                ]),
                Line::from(""),
                Line::from(Span::styled(
                    format!(" {}", c.subject),
                    Style::default()
                        .fg(ctx.theme.fg_bright)
                        .add_modifier(Modifier::BOLD),
                )),
                Line::from(Span::styled(
                    ctx.t("view.log_details.loading_details"),
                    Style::default().fg(ctx.theme.fg_muted),
                )),
            ];
            let fallback_max = lines.len().saturating_sub(h);
            frame.render_widget(Paragraph::new(lines), area);
            ctx.record_scroll_bound(ScrollPane::LogDetails, h, fallback_max);
        } else {
            ctx.record_scroll_bound(ScrollPane::LogDetails, h, 0);
            frame.render_widget(muted_line(ctx, ctx.t("view.log_details.loading_details")), area);
        }
        return;
    };

    let width = area.width as usize;
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut hits: Vec<LineHit> = Vec::new();

    if bundle.details.len() > 1 {
        let count_s = bundle.details.len().to_string();
        lines.push(Line::from(Span::styled(
            ctx.t_fmt("view.log_details.commits_selected", &[("count", &count_s)]),
            Style::default()
                .fg(ctx.theme.fg_bright)
                .add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(""));
        for d in &bundle.details {
            let short = d.oid.short();
            let subject = d.message.lines().next().unwrap_or("").to_string();
            hits.push(LineHit {
                line_idx: lines.len(),
                x: 1,
                width: short.width() as u16,
                target: HitTarget::DetailsParent { oid: d.oid },
            });
            lines.push(Line::from(vec![
                Span::styled(
                    format!(" {short}"),
                    Style::default().fg(ctx.theme.fg_accent_link),
                ),
                Span::styled(
                    format!(" {}", truncate_to_width(&subject, width.saturating_sub(10))),
                    Style::default().fg(ctx.theme.fg_primary),
                ),
            ]));
        }
    } else if let Some(d) = bundle.details.first() {
        build_single_details(d, state, ctx, width, &mut lines, &mut hits);
    }

    let max_scroll = lines.len().saturating_sub(h);
    ctx.record_scroll_bound(ScrollPane::LogDetails, h, max_scroll);
    let scroll = details.details_scroll.min(max_scroll);
    for (vis_y, idx) in (scroll..lines.len()).take(h).enumerate() {
        let rect = Rect::new(area.x, area.y + vis_y as u16, area.width, 1);
        frame.render_widget(Paragraph::new(lines[idx].clone()), rect);
        for hit in &hits {
            if hit.line_idx == idx && hit.x < area.width {
                let w = hit.width.min(area.width - hit.x);
                ctx.hit_registry
                    .register(Rect::new(area.x + hit.x, rect.y, w, 1), 3, hit.target.clone());
            }
        }
    }
}

fn build_single_details(
    d: &CommitDetails,
    state: &AppState,
    ctx: &RenderContext<'_>,
    width: usize,
    lines: &mut Vec<Line<'static>>,
    hits: &mut Vec<LineHit>,
) {
    let theme = ctx.theme;
    let label = |s: &str| Span::styled(pad_label(s, 10), Style::default().fg(theme.fg_muted));

    // 1. Full SHA (click to copy) + signature badge.
    let sha = d.oid.to_string();
    let mut sha_spans = vec![
        label(ctx.t("view.log_details.commit_label")),
        Span::styled(
            sha.clone(),
            Style::default()
                .fg(theme.fg_accent_link)
                .add_modifier(Modifier::UNDERLINED),
        ),
    ];
    match d.signature {
        SignatureStatus::Verified => sha_spans.push(Span::styled(
            ctx.t("view.log_details.verified").to_string(),
            Style::default()
                .fg(theme.ref_local.pill_fg)
                .bg(theme.ref_local.pill_bg),
        )),
        SignatureStatus::SignedUnverified => sha_spans.push(Span::styled(
            ctx.t("view.log_details.unverified").to_string(),
            Style::default()
                .fg(theme.ref_detached.pill_fg)
                .bg(theme.ref_detached.pill_bg),
        )),
        SignatureStatus::Unsigned => {}
    }
    hits.push(LineHit {
        line_idx: lines.len(),
        x: 11,
        width: sha.width() as u16,
        target: HitTarget::DetailsHash { oid: d.oid },
    });
    lines.push(Line::from(sha_spans));

    // 2. Author / Committer.
    if d.same_author_and_committer() {
        lines.push(person_line(ctx.t("view.log_details.author_label"), &d.author, ctx));
    } else {
        lines.push(person_line(ctx.t("view.log_details.author_label"), &d.author, ctx));
        lines.push(person_line(ctx.t("view.log_details.committer_label"), &d.committer, ctx));
    }

    // 3. Clickable parents.
    let mut parent_spans = vec![label(ctx.t("view.log_details.parents_label"))];
    if d.parents.is_empty() {
        parent_spans.push(Span::styled(
            ctx.t("view.log_details.root_commit").to_string(),
            Style::default().fg(theme.fg_muted),
        ));
    }
    let mut x = 11u16;
    for p in &d.parents {
        let short = p.short();
        hits.push(LineHit {
            line_idx: lines.len(),
            x,
            width: short.width() as u16,
            target: HitTarget::DetailsParent { oid: *p },
        });
        x += short.width() as u16 + 2;
        parent_spans.push(Span::styled(
            short,
            Style::default()
                .fg(theme.fg_accent_link)
                .add_modifier(Modifier::UNDERLINED),
        ));
        parent_spans.push(Span::raw("  "));
    }
    lines.push(Line::from(parent_spans));

    // 4. Contained in branches / tags.
    if let (Some((oid, names)), Some(snap)) = (&state.details.containing, &state.repo_snapshot)
        && *oid == d.oid
    {
        let kind_of = |n: &str| snap.refs.iter().find(|r| r.name == n).map(|r| r.kind);
        let branches: Vec<&str> = names
            .iter()
            .filter(|n| kind_of(n) != Some(RefKind::Tag))
            .map(String::as_str)
            .collect();
        let tags: Vec<&str> = names
            .iter()
            .filter(|n| kind_of(n) == Some(RefKind::Tag))
            .map(String::as_str)
            .collect();
        let mut push_list = |title: &str, items: &[&str], color: Color| {
            if items.is_empty() {
                return;
            }
            let joined = items.join(", ");
            for (i, chunk) in wrap_text(&joined, width.saturating_sub(12).max(8))
                .into_iter()
                .enumerate()
            {
                lines.push(Line::from(vec![
                    if i == 0 {
                        label(title)
                    } else {
                        Span::raw(" ".repeat(11))
                    },
                    Span::styled(chunk, Style::default().fg(color)),
                ]));
            }
        };
        push_list(ctx.t("view.log_details.branches_label"), &branches, theme.ref_local.pill_fg);
        push_list(ctx.t("view.log_details.tags_label"), &tags, theme.ref_tag.pill_fg);
        if branches.is_empty() && tags.is_empty() {
            lines.push(Line::from(vec![
                label(ctx.t("view.log_details.branches_label")),
                Span::styled(
                    ctx.t("view.log_details.not_on_branch").to_string(),
                    Style::default().fg(theme.fg_muted),
                ),
            ]));
        }
    }

    // 5. Full message.
    lines.push(Line::from(""));
    for (idx, raw) in d.message.lines().enumerate() {
        let kind = classify_message_line(idx, raw);
        let base = match kind {
            MessageLineKind::Subject => Style::default()
                .fg(theme.fg_bright)
                .add_modifier(Modifier::BOLD),
            MessageLineKind::Trailer => Style::default().fg(theme.ref_remote.pill_fg),
            MessageLineKind::Body => Style::default().fg(theme.fg_primary),
        };
        let issue = Style::default()
            .fg(theme.fg_accent_link)
            .add_modifier(Modifier::UNDERLINED);
        if raw.is_empty() {
            lines.push(Line::from(""));
            continue;
        }
        for chunk in wrap_text(raw, width.saturating_sub(2).max(8)) {
            let ranges = issue_ref_ranges(&chunk);
            let mut spans = vec![Span::raw(" ")];
            spans.extend(highlight_spans(
                &chunk,
                &ranges,
                width.saturating_sub(1),
                base,
                issue,
            ));
            lines.push(Line::from(spans));
        }
    }
}
