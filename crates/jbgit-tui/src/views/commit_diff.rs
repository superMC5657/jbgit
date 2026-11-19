//! Interactive right-hand `StageDiffWorkbench` for `[1 Commit]`:
//! three-state gutter bars, hunk-header inline action pills, visual line selection (`v`),
//! hunk jumping (`n`/`p`/`]c`/`[c`), and 3-way inspector (`i`).

use jbgit_core::{DiffLine, DiffLineKind, LineGutterMark, PaneId};
use jbgit_engine::StageDiffSide;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use unicode_width::UnicodeWidthStr;

use crate::app::{AppState, RenderContext};
use crate::hit_test::{DiffHunkActionKind, HitTarget, StageDiffToolbarButton};
use crate::stage_view::StageDiffRow;
use crate::syntax::{HighlightedDiff, render_diff_line_spans_with_tab};
use crate::views::{fit_left_right_spans, render_pane_frame, truncate_to_width};

fn muted_line(ctx: &RenderContext<'_>, text: &str) -> Paragraph<'static> {
    Paragraph::new(Span::styled(
        text.to_string(),
        Style::default().fg(ctx.theme.fg_muted),
    ))
}

pub fn render_stage_diff_workbench(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &AppState,
    ctx: &mut RenderContext<'_>,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let sw = &state.stage_workbench;
    let subtitle = if let Some(key) = &sw.current_diff_key {
        let side_label = match key.side {
            StageDiffSide::Staged => state.i18n.t("view.commit.diff_staged_side"),
            StageDiffSide::Unstaged => state.i18n.t("view.commit.diff_unstaged_side"),
            StageDiffSide::AmendCommit => state.i18n.t("view.commit.diff_amend_side"),
        };
        format!("{} ({side_label})", key.path)
    } else {
        state.i18n.t("view.commit.diff_select_file").to_string()
    };

    let body = render_pane_frame(frame, area, PaneId::CommitDiffPreview, &subtitle, ctx);
    if body.width == 0 || body.height == 0 {
        return;
    }

    // Toolbar row
    let toolbar_rect = Rect::new(body.x, body.y, body.width, 1);
    render_stage_diff_toolbar(frame, toolbar_rect, state, ctx);

    // Reserve bottom hint bar if height >= 3
    let show_hints = body.height >= 4;
    let content_h = if show_hints {
        body.height.saturating_sub(2)
    } else {
        body.height.saturating_sub(1)
    };
    let diff_area = Rect::new(body.x, body.y + 1, body.width, content_h);
    ctx.hit_registry.register(diff_area, 2, HitTarget::DiffArea);

    if show_hints {
        let hint_rect = Rect::new(body.x, body.y + 1 + content_h, body.width, 1);
        render_diff_hint_bar(frame, hint_rect, state, ctx);
    }

    if diff_area.height == 0 {
        return;
    }

    let Some(diff) = &sw.current_diff else {
        let msg = if sw.current_diff_key.is_some() {
            state.i18n.t("view.commit.diff_loading")
        } else {
            state.i18n.t("view.commit.diff_select_prompt")
        };
        frame.render_widget(muted_line(ctx, msg), diff_area);
        return;
    };

    if diff.is_binary {
        frame.render_widget(muted_line(ctx, state.i18n.t("view.commit.diff_binary")), diff_area);
        return;
    }
    if diff.hunks.is_empty() {
        frame.render_widget(
            muted_line(ctx, state.i18n.t("view.commit.diff_no_text")),
            diff_area,
        );
        return;
    }

    let rows = sw.diff_rows();
    let total = rows.len() + usize::from(diff.is_truncated);
    let h = diff_area.height as usize;
    let scroll = sw.diff_scroll.min(total.saturating_sub(h));
    let theme = ctx.theme;
    let focused = ctx.focused_pane == PaneId::CommitDiffPreview;
    let visual_bounds = sw.selected_visual_bounds();
    let num_w = 4usize;
    let tab_size = ctx.diff_tab_size();

    let empty_hl;
    let computed_hl;
    let hl_diff: &HighlightedDiff = if !ctx.diff_syntax_enabled() {
        empty_hl = HighlightedDiff::default();
        &empty_hl
    } else {
        match &sw.diff_syntax {
            Some(cached) => cached,
            None => {
                computed_hl = HighlightedDiff::build(&diff.as_file_diff());
                &computed_hl
            }
        }
    };

    let line_style = |kind: DiffLineKind, row_selected: bool, is_cursor: bool| -> (Style, Color, &'static str) {
        let mut base = match kind {
            DiffLineKind::Added => Style::default().fg(theme.fg_primary).bg(theme.diff_add_bg),
            DiffLineKind::Deleted => Style::default().fg(theme.fg_primary).bg(theme.diff_del_bg),
            DiffLineKind::Context => Style::default().fg(theme.fg_primary).bg(theme.bg_canvas),
        };
        if row_selected {
            base = base.bg(theme.bg_selection_focused);
        } else if is_cursor && focused {
            base = base.bg(theme.bg_hover);
        }
        let word_bg = match kind {
            DiffLineKind::Added => theme.diff_add_word_bg,
            DiffLineKind::Deleted => theme.diff_del_word_bg,
            DiffLineKind::Context => theme.bg_canvas,
        };
        let marker = match kind {
            DiffLineKind::Added => "+",
            DiffLineKind::Deleted => "-",
            DiffLineKind::Context => " ",
        };
        (base, word_bg, marker)
    };

    let gutter_bar = |mark: LineGutterMark, is_cursor: bool| -> Span<'static> {
        let (sym, fg) = match mark {
            LineGutterMark::None => (if is_cursor && focused { "›" } else { " " }, theme.fg_muted),
            LineGutterMark::Unstaged(DiffLineKind::Added) => ("▌", theme.status_added),
            LineGutterMark::Unstaged(DiffLineKind::Deleted) => ("▸", theme.status_conflicted),
            LineGutterMark::Unstaged(DiffLineKind::Context) => ("▌", theme.status_modified),
            LineGutterMark::Staged(DiffLineKind::Added) => ("▏", theme.status_added),
            LineGutterMark::Staged(DiffLineKind::Deleted) => ("▹", theme.status_conflicted),
            LineGutterMark::Staged(DiffLineKind::Context) => ("▏", theme.status_modified),
            LineGutterMark::Both => ("▚", theme.ref_head.icon),
        };
        Span::styled(sym, Style::default().fg(fg).bg(theme.bg_panel))
    };

    let gutter = Style::default().fg(theme.fg_muted).bg(theme.bg_panel);
    let num = |n: Option<u32>| n.map_or_else(|| " ".repeat(num_w), |v| format!("{v:>num_w$}"));
    let width = diff_area.width as usize;

    let side_cell = |hh: usize,
                     line_idx: Option<usize>,
                     lineno: fn(&DiffLine) -> Option<u32>,
                     cell_w: usize,
                     row_sel: bool,
                     is_cursor: bool|
     -> Vec<Span<'static>> {
        let mut spans = Vec::new();
        match line_idx {
            Some(li) => {
                let l = &diff.hunks[hh].hunk.lines[li];
                let mark = diff.hunks[hh]
                    .line_meta
                    .get(li)
                    .map_or(LineGutterMark::None, |m| m.gutter_mark);
                let toks = hl_diff
                    .hunks
                    .get(hh)
                    .and_then(|h| h.get(li))
                    .map(Vec::as_slice)
                    .unwrap_or(&[]);
                let (base, word_bg, marker) = line_style(l.kind, row_sel, is_cursor);
                spans.push(gutter_bar(mark, is_cursor));
                spans.push(Span::styled(format!("{} ", num(lineno(l))), gutter));
                spans.push(Span::styled(marker.to_string(), base));
                let text_w = cell_w.saturating_sub(num_w + 3);
                let mut line_body = render_diff_line_spans_with_tab(
                    &l.text,
                    toks,
                    &l.word_ranges,
                    text_w,
                    tab_size,
                    base,
                    word_bg,
                    theme.true_color,
                );
                let used: usize = line_body.iter().map(|s| s.content.width()).sum();
                line_body.push(Span::styled(" ".repeat(text_w.saturating_sub(used)), base));
                spans.extend(line_body);
            }
            None => {
                spans.push(Span::styled(" ".repeat(num_w + 2), gutter));
                spans.push(Span::styled(
                    " ".repeat(cell_w.saturating_sub(num_w + 2)),
                    Style::default().bg(theme.bg_panel),
                ));
            }
        }
        spans
    };

    for (vis_y, row_idx) in (scroll..total).take(h).enumerate() {
        let rect = Rect::new(diff_area.x, diff_area.y + vis_y as u16, diff_area.width, 1);
        let is_cursor = row_idx == sw.diff_cursor_row;
        let in_visual = visual_bounds.is_some_and(|(lo, hi)| row_idx >= lo && row_idx <= hi);

        match rows.get(row_idx) {
            None => {
                frame.render_widget(
                    Paragraph::new(Line::from(Span::styled(
                        state.i18n.t("view.commit.diff_truncated"),
                        Style::default().fg(theme.status_conflicted),
                    ))),
                    rect,
                );
            }
            Some(StageDiffRow::Header(hh)) => {
                let hunk_idx = *hh;
                let hunk = &diff.hunks[hunk_idx];
                let bg = if is_cursor && focused {
                    theme.bg_selection_unfocused
                } else {
                    theme.diff_mod_bg
                };

                let mut pills: Vec<(String, Color, DiffHunkActionKind)> = Vec::new();
                match diff.key.side {
                    StageDiffSide::Staged => {
                        pills.push((
                            state.i18n.t("view.commit.hunk_unstage_pill").to_string(),
                            theme.status_untracked,
                            DiffHunkActionKind::Unstage,
                        ));
                    }
                    StageDiffSide::Unstaged => {
                        pills.push((
                            state.i18n.t("view.commit.hunk_stage_pill").to_string(),
                            theme.status_added,
                            DiffHunkActionKind::Stage,
                        ));
                        pills.push((
                            state.i18n.t("view.commit.hunk_rollback_pill").to_string(),
                            theme.status_conflicted,
                            DiffHunkActionKind::Rollback,
                        ));
                    }
                    StageDiffSide::AmendCommit => {}
                }
                if diff.key.side != StageDiffSide::AmendCommit && hunk.has_mixed_range {
                    pills.push((
                        state.i18n.t("view.commit.hunk_three_way_pill").to_string(),
                        theme.ref_head.icon,
                        DiffHunkActionKind::Inspect3Way,
                    ));
                }

                let pills_w: usize = pills.iter().map(|(t, _, _)| t.width() + 1).sum();
                let header_w = width.saturating_sub(pills_w + 1);
                let left = vec![Span::styled(
                    format!(" {}", truncate_to_width(&hunk.hunk.header, header_w)),
                    Style::default()
                        .fg(theme.fg_accent_link)
                        .bg(bg)
                        .add_modifier(Modifier::BOLD),
                )];
                let right: Vec<Span<'static>> = pills
                    .iter()
                    .flat_map(|(t, col, _)| {
                        vec![
                            Span::styled(
                                t.clone(),
                                Style::default()
                                    .fg(*col)
                                    .bg(theme.bg_elevated)
                                    .add_modifier(Modifier::BOLD),
                            ),
                            Span::styled(" ", Style::default().bg(bg)),
                        ]
                    })
                    .collect();

                let spans = fit_left_right_spans(left, right, width, 0);
                frame.render_widget(
                    Paragraph::new(Line::from(spans)).style(Style::default().bg(bg)),
                    rect,
                );
                ctx.hit_registry
                    .register(rect, 3, HitTarget::StageDiffLine { row_idx });

                // Register right-aligned clickable pill hit targets
                let mut rx = rect.right();
                for (t, _, act) in pills.iter().rev() {
                    let tw = t.width() as u16;
                    rx = rx.saturating_sub(1 + tw);
                    if rx >= rect.x {
                        ctx.hit_registry.register(
                            Rect::new(rx, rect.y, tw, 1),
                            5,
                            HitTarget::DiffHunkAction {
                                hunk_idx,
                                action: *act,
                            },
                        );
                    }
                }
            }
            Some(StageDiffRow::Unified(hh, l)) => {
                let dl = &diff.hunks[*hh].hunk.lines[*l];
                let meta = diff.hunks[*hh].line_meta.get(*l);
                let mark = meta.map_or(LineGutterMark::None, |m| m.gutter_mark);
                let sr_idx = meta.and_then(|m| m.staged_range_idx);
                let toks = hl_diff
                    .hunks
                    .get(*hh)
                    .and_then(|h| h.get(*l))
                    .map(Vec::as_slice)
                    .unwrap_or(&[]);
                let (base, word_bg, marker) = line_style(dl.kind, in_visual, is_cursor);
                let mut spans = vec![
                    gutter_bar(mark, is_cursor),
                    Span::styled(
                        format!("{} {} ", num(dl.old_lineno), num(dl.new_lineno)),
                        gutter,
                    ),
                    Span::styled(marker.to_string(), base),
                ];
                let text_w = width.saturating_sub(num_w * 2 + 4);
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
                frame.render_widget(Paragraph::new(Line::from(spans)).style(base), rect);
                ctx.hit_registry
                    .register(rect, 3, HitTarget::StageDiffLine { row_idx });
                if diff.key.side != StageDiffSide::AmendCommit
                    && mark != LineGutterMark::None
                    && sr_idx.is_some()
                    && rect.width >= 1
                {
                    ctx.hit_registry.register(
                        Rect::new(rect.x, rect.y, 1, 1),
                        4,
                        HitTarget::StageDiffGutter {
                            row_idx,
                            staged_range_idx: sr_idx,
                        },
                    );
                }
            }
            Some(StageDiffRow::Split(hh, left, right)) => {
                let half = width / 2;
                let mut spans = side_cell(
                    *hh,
                    *left,
                    |l| l.old_lineno,
                    half,
                    in_visual,
                    is_cursor,
                );
                spans.push(Span::styled(
                    "│",
                    Style::default().fg(theme.border_splitter),
                ));
                spans.extend(side_cell(
                    *hh,
                    *right,
                    |l| l.new_lineno,
                    width.saturating_sub(half + 1),
                    in_visual,
                    is_cursor,
                ));
                frame.render_widget(Paragraph::new(Line::from(spans)), rect);
                ctx.hit_registry
                    .register(rect, 3, HitTarget::StageDiffLine { row_idx });
                if diff.key.side != StageDiffSide::AmendCommit {
                    if let Some(li) = *left
                        && let Some(meta) = diff.hunks[*hh].line_meta.get(li)
                        && meta.gutter_mark != LineGutterMark::None
                        && meta.staged_range_idx.is_some()
                        && rect.width >= 1
                    {
                        ctx.hit_registry.register(
                            Rect::new(rect.x, rect.y, 1, 1),
                            4,
                            HitTarget::StageDiffGutter {
                                row_idx,
                                staged_range_idx: meta.staged_range_idx,
                            },
                        );
                    }
                    let right_gx = rect.x.saturating_add(half as u16 + 1);
                    if let Some(ri) = *right
                        && let Some(meta) = diff.hunks[*hh].line_meta.get(ri)
                        && meta.gutter_mark != LineGutterMark::None
                        && meta.staged_range_idx.is_some()
                        && right_gx < rect.right()
                    {
                        ctx.hit_registry.register(
                            Rect::new(right_gx, rect.y, 1, 1),
                            4,
                            HitTarget::StageDiffGutter {
                                row_idx,
                                staged_range_idx: meta.staged_range_idx,
                            },
                        );
                    }
                }
            }
        }
    }
}

fn render_stage_diff_toolbar(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &AppState,
    ctx: &mut RenderContext<'_>,
) {
    let sw = &state.stage_workbench;
    frame.render_widget(
        Paragraph::new("").style(Style::default().bg(ctx.theme.bg_panel)),
        area,
    );

    let mut buttons: Vec<(String, bool, StageDiffToolbarButton)> = vec![
        (
            if sw.diff_side_by_side {
                state.i18n.t("view.commit.diff_split_btn")
            } else {
                state.i18n.t("view.commit.diff_unified_btn")
            }
            .to_string(),
            sw.diff_side_by_side,
            StageDiffToolbarButton::SideBySide,
        ),
        (
            state.i18n.t_fmt(
                "view.commit.diff_context_btn",
                &[("ctx", state.i18n.diff_context_label(sw.diff_context))],
            ),
            false,
            StageDiffToolbarButton::Context,
        ),
        (
            state.i18n.t("view.commit.diff_prev_hunk_btn").to_string(),
            false,
            StageDiffToolbarButton::PrevHunk,
        ),
        (
            state.i18n.t("view.commit.diff_next_hunk_btn").to_string(),
            false,
            StageDiffToolbarButton::NextHunk,
        ),
    ];

    if let Some(diff) = &sw.current_diff
        && (!diff.tracker.staged_blocks.is_empty() && !diff.tracker.unstaged_blocks.is_empty())
    {
        let label = if diff.key.side == StageDiffSide::Staged {
            state.i18n.t("view.commit.diff_view_unstaged_btn")
        } else {
            state.i18n.t("view.commit.diff_view_staged_btn")
        };
        buttons.push((label.to_string(), true, StageDiffToolbarButton::SwitchSide));
    }
    buttons.push((" × ".to_string(), false, StageDiffToolbarButton::Close));

    let mut x = area.x;
    for (text, active, btn) in buttons {
        let w = text.width() as u16;
        if x + w > area.right() {
            break;
        }
        let rect = Rect::new(x, area.y, w, 1);
        let style = if active {
            Style::default()
                .fg(ctx.theme.fg_bright)
                .bg(ctx.theme.bg_selection_unfocused)
        } else {
            Style::default()
                .fg(ctx.theme.fg_muted)
                .bg(ctx.theme.bg_panel)
        };
        frame.render_widget(Paragraph::new(Span::styled(text, style)), rect);
        ctx.hit_registry
            .register(rect, 4, HitTarget::StageDiffToolbar(btn));
        x += w + 1;
    }
}

fn render_diff_hint_bar(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &AppState,
    ctx: &RenderContext<'_>,
) {
    let sw = &state.stage_workbench;
    let is_amend_ref = sw
        .current_diff_key
        .as_ref()
        .is_some_and(|k| k.side == StageDiffSide::AmendCommit);
    let text = if is_amend_ref {
        state.i18n.t("view.commit.diff_hint_amend").to_string()
    } else {
        let mut parts = Vec::new();
        if sw.visual_anchor_row.is_some() {
            parts.push(state.i18n.t("view.commit.diff_visual_line"));
        }
        let undo_hint = if !sw.rollback_undo_stack.is_empty() {
            state.i18n.t("view.commit.diff_undo_rollback_hint")
        } else {
            ""
        };
        state.i18n.t_fmt(
            "view.commit.diff_hint_editable",
            &[("mode", &parts.join(" · ")), ("undo", undo_hint)],
        )
    };
    frame.render_widget(
        Paragraph::new(Span::styled(
            truncate_to_width(&text, area.width as usize),
            Style::default().fg(if sw.visual_anchor_row.is_some() {
                ctx.theme.ref_head.icon
            } else {
                ctx.theme.fg_muted
            }),
        ))
        .style(Style::default().bg(ctx.theme.bg_panel)),
        area,
    );
}
