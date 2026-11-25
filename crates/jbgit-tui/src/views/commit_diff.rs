//! Interactive right-hand `StageDiffWorkbench` for `[1 Commit]`:
//! three-state gutter bars, hunk-header inline action pills, visual line selection (`v`),
//! hunk jumping (`n`/`p`/`]c`/`[c`), and 3-way inspector (`i`).

use jbgit_core::{DiffLineKind, LineGutterMark, PaneId, WhitespaceMode};
use jbgit_engine::StageDiffSide;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use unicode_width::UnicodeWidthStr;

use crate::app::{AppState, RenderContext, ScrollPane};
use crate::diff_session::{DiffRow, diff_row_visual_height};
use crate::hit_test::{DiffHunkActionKind, HitTarget, StageDiffToolbarButton};
use crate::stage_view::CommitWorkbench;
use crate::syntax::HighlightedDiff;
use crate::views::log_details::{
    DiffLineRenderInput, diff_line_style, render_binary_diff_card, render_diff_search_bar,
    render_diff_side_cell, render_diff_side_wrapped_cells, render_diff_unified_spans,
    render_diff_unified_wrapped_rows, render_lfs_diff_placeholder, render_mode_change_banner,
};
use crate::views::{fit_left_right_spans, render_pane_frame, render_toolbar, truncate_to_width};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DiffHintMode {
    Amend,
    WhitespaceIgnore,
    VisualLine,
}

fn diff_hint_mode(sw: &CommitWorkbench) -> Option<DiffHintMode> {
    if sw
        .current_diff_key()
        .is_some_and(|k| k.side == StageDiffSide::AmendCommit)
    {
        Some(DiffHintMode::Amend)
    } else if sw.diff_whitespace() != WhitespaceMode::Default {
        Some(DiffHintMode::WhitespaceIgnore)
    } else if sw.visual_anchor_row().is_some() {
        Some(DiffHintMode::VisualLine)
    } else {
        None
    }
}

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
    let mut subtitle = if let Some(key) = sw.current_diff_key() {
        let side_label = match key.side {
            StageDiffSide::Staged => state.i18n.t("view.commit.diff_staged_side"),
            StageDiffSide::Unstaged => state.i18n.t("view.commit.diff_unstaged_side"),
            StageDiffSide::AmendCommit => state.i18n.t("view.commit.diff_amend_side"),
        };
        format!("{} ({side_label})", key.path)
    } else {
        state.i18n.t("view.commit.diff_select_file").to_string()
    };
    if !sw.diff_soft_wrap() && sw.diff_horiz_scroll() > 0 {
        subtitle.push_str(&format!("  Col +{}", sw.diff_horiz_scroll()));
    }

    let body = render_pane_frame(frame, area, PaneId::CommitDiffPreview, &subtitle, ctx);
    if body.width == 0 || body.height == 0 {
        return;
    }

    // Toolbar row
    let toolbar_rect = Rect::new(body.x, body.y, body.width, 1);
    render_stage_diff_toolbar(frame, toolbar_rect, state, ctx);

    // Reserve bottom bar for active inline search, Amend reference, whitespace ignore, or Visual line mode
    let show_search = sw.diff_search().active && body.height >= 3;
    let hint_mode = (!show_search)
        .then(|| diff_hint_mode(sw))
        .flatten()
        .filter(|_| body.height >= 4);
    let content_h = if show_search || hint_mode.is_some() {
        body.height.saturating_sub(2)
    } else {
        body.height.saturating_sub(1)
    };
    let diff_area = Rect::new(body.x, body.y + 1, body.width, content_h);
    ctx.hit_registry.register(diff_area, 2, HitTarget::DiffArea);

    if show_search {
        let search_rect = Rect::new(body.x, body.y + 1 + content_h, body.width, 1);
        render_diff_search_bar(frame, search_rect, sw.diff_search(), ctx);
    } else if let Some(mode) = hint_mode {
        let hint_rect = Rect::new(body.x, body.y + 1 + content_h, body.width, 1);
        render_diff_hint_bar(frame, hint_rect, mode, state, ctx);
    }

    if diff_area.height == 0 {
        return;
    }

    let h = diff_area.height as usize;
    let Some(diff) = sw.current_diff() else {
        ctx.record_scroll_bound(ScrollPane::StageDiff, h, 0);
        let msg = if sw.current_diff_key().is_some() {
            state.i18n.t("view.commit.diff_loading")
        } else {
            state.i18n.t("view.commit.diff_select_prompt")
        };
        frame.render_widget(muted_line(ctx, msg), diff_area);
        return;
    };

    if let Some(sub) = &diff.submodule {
        ctx.record_scroll_bound(ScrollPane::StageDiff, h, 0);
        let fmt_oid = |oid: Option<jbgit_core::Oid>| {
            oid.map_or_else(
                || state.i18n.t("view.commit.submodule_none").to_string(),
                |o| o.to_string(),
            )
        };
        let state_key = match (
            sub.state.uninitialized,
            sub.state.new_commits,
            sub.state.dirty_workdir,
        ) {
            (true, _, _) => "view.commit.submodule_uninitialized",
            (false, true, true) => "view.commit.submodule_new_and_dirty",
            (false, true, false) => "view.commit.submodule_new_commits",
            (false, false, true) => "view.commit.submodule_dirty_workdir",
            (false, false, false) => "view.commit.submodule_clean",
        };
        let head_s = fmt_oid(sub.head_oid);
        let idx_s = fmt_oid(sub.index_oid);
        let wd_s = fmt_oid(sub.workdir_oid);
        let lines = vec![
            Line::from(Span::styled(
                format!(" {} ", state.i18n.t("view.commit.submodule_diff_title")),
                Style::default()
                    .fg(ctx.theme.fg_bright)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
            Line::from(vec![
                Span::styled(
                    format!(" {}: ", state.i18n.t("view.commit.submodule_state_label")),
                    Style::default().fg(ctx.theme.fg_muted),
                ),
                Span::styled(
                    state.i18n.t(state_key).to_string(),
                    Style::default().fg(ctx.theme.ref_detached.pill_fg),
                ),
            ]),
            Line::from(vec![
                Span::styled(" HEAD:    ", Style::default().fg(ctx.theme.fg_muted)),
                Span::styled(head_s, Style::default().fg(ctx.theme.fg_primary)),
            ]),
            Line::from(vec![
                Span::styled(" Index:   ", Style::default().fg(ctx.theme.fg_muted)),
                Span::styled(idx_s, Style::default().fg(ctx.theme.fg_primary)),
            ]),
            Line::from(vec![
                Span::styled(" Workdir: ", Style::default().fg(ctx.theme.fg_muted)),
                Span::styled(wd_s, Style::default().fg(ctx.theme.status_modified)),
            ]),
            Line::from(""),
            Line::from(Span::styled(
                format!(" {}", state.i18n.t("view.commit.submodule_diff_hint")),
                Style::default().fg(ctx.theme.fg_muted),
            )),
        ];
        frame.render_widget(Paragraph::new(lines), diff_area);
        return;
    }

    if let Some(lfs) = &diff.lfs {
        ctx.record_scroll_bound(ScrollPane::StageDiff, h, 0);
        render_lfs_diff_placeholder(
            frame,
            diff_area,
            lfs,
            Some("view.commit.lfs_line_staging_disabled"),
            ctx,
        );
        return;
    }

    if diff.is_too_large {
        ctx.record_scroll_bound(ScrollPane::StageDiff, h, 0);
        frame.render_widget(
            muted_line(ctx, state.i18n.t("view.commit.diff_too_large")),
            diff_area,
        );
        return;
    }
    if diff.is_binary {
        ctx.record_scroll_bound(ScrollPane::StageDiff, h, 0);
        render_binary_diff_card(frame, diff_area, diff.binary.as_ref(), ctx);
        return;
    }
    let mut diff_area = diff_area;
    if let Some(mc) = &diff.mode_change {
        let banner_rect = Rect::new(diff_area.x, diff_area.y, diff_area.width, 1);
        render_mode_change_banner(frame, banner_rect, mc, ctx);
        if diff.hunks.is_empty() {
            ctx.record_scroll_bound(ScrollPane::StageDiff, h, 0);
            if diff_area.height > 1 {
                let hint_rect = Rect::new(
                    diff_area.x,
                    diff_area.y + 1,
                    diff_area.width,
                    diff_area.height - 1,
                );
                frame.render_widget(
                    muted_line(ctx, state.i18n.t("view.diff.mode_only_hint")),
                    hint_rect,
                );
            }
            return;
        }
        if diff_area.height <= 1 {
            ctx.record_scroll_bound(ScrollPane::StageDiff, 0, 0);
            return;
        }
        diff_area = Rect::new(
            diff_area.x,
            diff_area.y + 1,
            diff_area.width,
            diff_area.height - 1,
        );
    } else if diff.hunks.is_empty() {
        ctx.record_scroll_bound(ScrollPane::StageDiff, h, 0);
        let msg = if diff.key.whitespace != WhitespaceMode::Default {
            state.i18n.t("view.log_details.no_diff_ignoring_ws")
        } else {
            state.i18n.t("view.commit.diff_no_text")
        };
        frame.render_widget(muted_line(ctx, msg), diff_area);
        return;
    }
    let h = diff_area.height as usize;

    let rows = sw.diff_rows();
    let theme = ctx.theme;
    let focused = ctx.focused_pane == PaneId::CommitDiffPreview;
    let visual_bounds = sw.selected_visual_bounds();
    let tab_size = ctx.diff_tab_size();
    let horiz_scroll = if sw.diff_soft_wrap() {
        0
    } else {
        sw.diff_horiz_scroll() as usize
    };
    let width = diff_area.width as usize;
    let half = width / 2;
    let right_cell_w = width.saturating_sub(half + 1);
    // Match `render_diff_unified_wrapped_rows` (`DIFF_LINE_NUM_W*2 + 3 + has_gutter = 12`)
    // and `render_diff_side_wrapped_cells` (`DIFF_LINE_NUM_W + 2 + g_cols = 7`).
    let unified_text_w = width.saturating_sub(12).max(1);
    let split_left_w = half.saturating_sub(7).max(1);
    let split_right_w = right_cell_w.saturating_sub(7).max(1);
    sw.record_diff_wrap_widths(unified_text_w, split_left_w, split_right_w);
    sw.record_diff_tab_size(tab_size);
    let allow_partial = diff.key.side != StageDiffSide::AmendCommit
        && !diff.is_line_staging_disabled()
        && sw.diff_whitespace() == WhitespaceMode::Default;

    let empty_hl;
    let hl_diff: &HighlightedDiff = if !ctx.diff_syntax_enabled() {
        empty_hl = HighlightedDiff::default();
        &empty_hl
    } else {
        match sw.diff_syntax() {
            Some(cached) => cached,
            None => {
                empty_hl = HighlightedDiff::default();
                &empty_hl
            }
        }
    };

    let line_style =
        |kind: DiffLineKind, row_selected: bool, is_cursor: bool| -> (Style, Color, &'static str) {
            let (mut base, word_bg, marker) = diff_line_style(theme, kind);
            if row_selected {
                base = base.bg(theme.bg_selection_focused);
            } else if is_cursor && focused {
                base = base.bg(theme.bg_hover);
            }
            (base, word_bg, marker)
        };

    let gutter_bar = |mark: LineGutterMark, is_cursor: bool| -> Span<'static> {
        let (sym, fg) = match mark {
            LineGutterMark::None => (if is_cursor && focused { "›" } else { " " }, theme.fg_muted),
            LineGutterMark::Unstaged(DiffLineKind::Added) => ("▌", theme.status_added),
            LineGutterMark::Unstaged(DiffLineKind::Deleted) => ("▸", theme.status_conflicted),
            LineGutterMark::Unstaged(DiffLineKind::Context | DiffLineKind::NoNewline) => {
                ("▌", theme.status_modified)
            }
            LineGutterMark::Staged(DiffLineKind::Added) => ("▏", theme.status_added),
            LineGutterMark::Staged(DiffLineKind::Deleted) => ("▹", theme.status_conflicted),
            LineGutterMark::Staged(DiffLineKind::Context | DiffLineKind::NoNewline) => {
                ("▏", theme.status_modified)
            }
            LineGutterMark::Both => ("▚", theme.ref_head.icon),
        };
        Span::styled(sym, Style::default().fg(fg).bg(theme.bg_panel))
    };

    let gutter = Style::default().fg(theme.fg_muted).bg(theme.bg_panel);
    let make_input = {
        let _ = ();
        #[inline]
        #[allow(clippy::too_many_arguments)]
        fn build<'a>(
            diff: &'a jbgit_engine::ThreeDocStageDiff,
            hl_diff: &'a HighlightedDiff,
            hh: usize,
            li: usize,
            custom_word_ranges: Option<&'a [(usize, usize)]>,
            search_ranges: &'a [(usize, usize)],
            active_search_range: Option<(usize, usize)>,
            style: (Style, Color, &'static str),
            leading_gutter: Span<'static>,
        ) -> DiffLineRenderInput<'a> {
            let line = &diff.hunks[hh].hunk.lines[li];
            let tokens = hl_diff
                .hunks
                .get(hh)
                .and_then(|h| h.get(li))
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            DiffLineRenderInput {
                line,
                tokens,
                custom_word_ranges,
                search_ranges,
                active_search_range,
                base: style.0,
                word_bg: style.1,
                marker: style.2,
                leading_gutter: Some(leading_gutter),
            }
        }
        build
    };

    let render_header_row = |frame: &mut Frame<'_>,
                             ctx: &mut RenderContext<'_>,
                             rect: Rect,
                             row_idx: usize,
                             hunk_idx: usize,
                             is_cursor: bool| {
        let hunk = &diff.hunks[hunk_idx];
        let bg = if is_cursor && focused {
            theme.bg_selection_unfocused
        } else {
            theme.diff_mod_bg
        };

        let mut pills: Vec<(String, Color, DiffHunkActionKind)> = Vec::new();
        if allow_partial {
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
                    if !diff.is_untracked() {
                        pills.push((
                            state.i18n.t("view.commit.hunk_rollback_pill").to_string(),
                            theme.status_conflicted,
                            DiffHunkActionKind::Rollback,
                        ));
                    }
                }
                StageDiffSide::AmendCommit => {}
            }
            if hunk.has_mixed_range {
                pills.push((
                    state.i18n.t("view.commit.hunk_three_way_pill").to_string(),
                    theme.ref_head.icon,
                    DiffHunkActionKind::Inspect3Way,
                ));
            }
        }

        let pills_w: usize = pills.iter().map(|(t, _, _)| t.width() + 1).sum();
        let header_w = width.saturating_sub(pills_w + 1);
        let (sr, active_sr) = sw.diff_search().ranges_for(row_idx, 0);
        let base_hdr = Style::default()
            .fg(theme.fg_accent_link)
            .bg(bg)
            .add_modifier(Modifier::BOLD);
        let mut left = vec![Span::styled(" ", base_hdr)];
        left.extend(crate::views::log_details::render_hunk_header_spans(
            &hunk.hunk.header,
            header_w,
            base_hdr,
            &sr,
            active_sr,
            theme,
        ));
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
    };

    if !sw.diff_soft_wrap() {
        let total = rows.len() + usize::from(diff.is_truncated);
        let max_scroll = total.saturating_sub(h);
        ctx.record_scroll_bound(ScrollPane::StageDiff, h, max_scroll);
        let scroll = sw.diff_scroll().offset().min(max_scroll);

        for (vis_y, row_idx) in (scroll..total).take(h).enumerate() {
            let rect = Rect::new(diff_area.x, diff_area.y + vis_y as u16, diff_area.width, 1);
            let is_cursor = row_idx == sw.diff_cursor_row();
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
                Some(DiffRow::Header(hh)) => {
                    render_header_row(frame, ctx, rect, row_idx, *hh, is_cursor);
                }
                Some(DiffRow::Unified(hh, l)) => {
                    let hunk_lines = &diff.hunks[*hh].hunk.lines;
                    let meta = diff.hunks[*hh].line_meta.get(*l);
                    let mark = meta.map_or(LineGutterMark::None, |m| m.gutter_mark);
                    let sr_idx = meta.and_then(|m| m.staged_range_idx);
                    let lazy_wr = sw
                        .word_diff_cache()
                        .unified_ranges(diff, *hh, *l, hunk_lines);
                    let (sr, active_sr) = sw.diff_search().ranges_for(row_idx, 0);
                    let style = line_style(hunk_lines[*l].kind, in_visual, is_cursor);
                    let inp = make_input(
                        diff,
                        hl_diff,
                        *hh,
                        *l,
                        lazy_wr.as_deref(),
                        &sr,
                        active_sr,
                        style,
                        gutter_bar(mark, is_cursor),
                    );
                    let base = inp.base;
                    let spans = render_diff_unified_spans(
                        inp,
                        width,
                        tab_size,
                        horiz_scroll,
                        theme,
                        gutter,
                    );
                    frame.render_widget(Paragraph::new(Line::from(spans)).style(base), rect);
                    ctx.hit_registry
                        .register(rect, 3, HitTarget::StageDiffLine { row_idx });
                    if allow_partial
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
                Some(DiffRow::Split(hh, left, right)) => {
                    let (lazy_l, lazy_r) = sw.word_diff_cache().split_ranges(
                        diff,
                        *hh,
                        *left,
                        *right,
                        &diff.hunks[*hh].hunk.lines,
                    );
                    let (sr_l, active_l) = sw.diff_search().ranges_for(row_idx, 0);
                    let (sr_r, active_r) = sw.diff_search().ranges_for(row_idx, 1);
                    let side_meta = |opt_li: Option<usize>| {
                        let li = opt_li?;
                        let line = &diff.hunks[*hh].hunk.lines[li];
                        let mark = diff.hunks[*hh]
                            .line_meta
                            .get(li)
                            .map_or(LineGutterMark::None, |m| m.gutter_mark);
                        let style = line_style(line.kind, in_visual, is_cursor);
                        Some((mark, style, li))
                    };
                    let left_in = side_meta(*left).map(|(m, st, li)| {
                        make_input(
                            diff,
                            hl_diff,
                            *hh,
                            li,
                            lazy_l.as_deref(),
                            &sr_l,
                            active_l,
                            st,
                            gutter_bar(m, is_cursor),
                        )
                    });
                    let right_in = side_meta(*right).map(|(m, st, li)| {
                        make_input(
                            diff,
                            hl_diff,
                            *hh,
                            li,
                            lazy_r.as_deref(),
                            &sr_r,
                            active_r,
                            st,
                            gutter_bar(m, is_cursor),
                        )
                    });
                    let mut spans = render_diff_side_cell(
                        left_in,
                        |l| l.old_lineno,
                        half,
                        true,
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
                        right_in,
                        |l| l.new_lineno,
                        right_cell_w,
                        true,
                        tab_size,
                        horiz_scroll,
                        theme,
                        gutter,
                    ));
                    frame.render_widget(Paragraph::new(Line::from(spans)), rect);
                    ctx.hit_registry
                        .register(rect, 3, HitTarget::StageDiffLine { row_idx });
                    if allow_partial {
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
        return;
    }

    // Soft-wrap mode
    let heights: Vec<usize> = rows
        .iter()
        .map(|&r| {
            diff_row_visual_height(
                r,
                |hh| &diff.hunks[hh].hunk,
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
    ctx.record_scroll_bound(ScrollPane::StageDiff, h, max_scroll);
    let scroll = sw.diff_scroll().offset().min(max_scroll);

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
        let is_cursor = row_idx == sw.diff_cursor_row();
        let in_visual = visual_bounds.is_some_and(|(lo, hi)| row_idx >= lo && row_idx <= hi);

        match row {
            DiffRow::Header(hh) => {
                let rect = Rect::new(diff_area.x, diff_area.y + drawn as u16, diff_area.width, 1);
                render_header_row(frame, ctx, rect, row_idx, hh, is_cursor);
                drawn += 1;
            }
            DiffRow::Unified(hh, l) => {
                let hunk_lines = &diff.hunks[hh].hunk.lines;
                let meta = diff.hunks[hh].line_meta.get(l);
                let mark = meta.map_or(LineGutterMark::None, |m| m.gutter_mark);
                let sr_idx = meta.and_then(|m| m.staged_range_idx);
                let lazy_wr = sw.word_diff_cache().unified_ranges(diff, hh, l, hunk_lines);
                let (sr, active_sr) = sw.diff_search().ranges_for(row_idx, 0);
                let style = line_style(hunk_lines[l].kind, in_visual, is_cursor);
                let inp = make_input(
                    diff,
                    hl_diff,
                    hh,
                    l,
                    lazy_wr.as_deref(),
                    &sr,
                    active_sr,
                    style,
                    gutter_bar(mark, is_cursor),
                );
                let base = inp.base;
                let wrapped = render_diff_unified_wrapped_rows(inp, width, tab_size, theme, gutter);
                for (offset, sub_spans) in wrapped
                    .into_iter()
                    .skip(skip_sub)
                    .take(take_sub)
                    .enumerate()
                {
                    let rect =
                        Rect::new(diff_area.x, diff_area.y + drawn as u16, diff_area.width, 1);
                    frame.render_widget(Paragraph::new(Line::from(sub_spans)).style(base), rect);
                    ctx.hit_registry
                        .register(rect, 3, HitTarget::StageDiffLine { row_idx });
                    if offset == 0
                        && skip_sub == 0
                        && allow_partial
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
                    drawn += 1;
                }
            }
            DiffRow::Split(hh, left, right) => {
                let (lazy_l, lazy_r) = sw.word_diff_cache().split_ranges(
                    diff,
                    hh,
                    left,
                    right,
                    &diff.hunks[hh].hunk.lines,
                );
                let (sr_l, active_l) = sw.diff_search().ranges_for(row_idx, 0);
                let (sr_r, active_r) = sw.diff_search().ranges_for(row_idx, 1);
                let side_meta = |opt_li: Option<usize>| {
                    let li = opt_li?;
                    let line = &diff.hunks[hh].hunk.lines[li];
                    let mark = diff.hunks[hh]
                        .line_meta
                        .get(li)
                        .map_or(LineGutterMark::None, |m| m.gutter_mark);
                    let style = line_style(line.kind, in_visual, is_cursor);
                    Some((mark, style, li))
                };
                let left_rows = render_diff_side_wrapped_cells(
                    side_meta(left).map(|(m, st, li)| {
                        make_input(
                            diff,
                            hl_diff,
                            hh,
                            li,
                            lazy_l.as_deref(),
                            &sr_l,
                            active_l,
                            st,
                            gutter_bar(m, is_cursor),
                        )
                    }),
                    |l| l.old_lineno,
                    half,
                    true,
                    tab_size,
                    theme,
                    gutter,
                );
                let right_rows = render_diff_side_wrapped_cells(
                    side_meta(right).map(|(m, st, li)| {
                        make_input(
                            diff,
                            hl_diff,
                            hh,
                            li,
                            lazy_r.as_deref(),
                            &sr_r,
                            active_r,
                            st,
                            gutter_bar(m, is_cursor),
                        )
                    }),
                    |l| l.new_lineno,
                    right_cell_w,
                    true,
                    tab_size,
                    theme,
                    gutter,
                );
                let blank_cell = |cell_w: usize| -> Vec<Span<'static>> {
                    vec![
                        Span::styled(" ".repeat(7), gutter),
                        Span::styled(
                            " ".repeat(cell_w.saturating_sub(7)),
                            Style::default().bg(theme.bg_panel),
                        ),
                    ]
                };
                let sub_count = left_rows.len().max(right_rows.len()).max(1);
                for sub_i in skip_sub..(skip_sub + take_sub).min(sub_count) {
                    let rect =
                        Rect::new(diff_area.x, diff_area.y + drawn as u16, diff_area.width, 1);
                    let mut spans = left_rows
                        .get(sub_i)
                        .cloned()
                        .unwrap_or_else(|| blank_cell(half));
                    spans.push(Span::styled(
                        "│",
                        Style::default().fg(theme.border_splitter),
                    ));
                    spans.extend(
                        right_rows
                            .get(sub_i)
                            .cloned()
                            .unwrap_or_else(|| blank_cell(right_cell_w)),
                    );
                    frame.render_widget(Paragraph::new(Line::from(spans)), rect);
                    ctx.hit_registry
                        .register(rect, 3, HitTarget::StageDiffLine { row_idx });
                    if sub_i == 0 && allow_partial {
                        if let Some(li) = left
                            && let Some(meta) = diff.hunks[hh].line_meta.get(li)
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
                        if let Some(ri) = right
                            && let Some(meta) = diff.hunks[hh].line_meta.get(ri)
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
                    drawn += 1;
                }
            }
        }
        cur_vis = row_end;
    }

    if diff.is_truncated && drawn < h && cur_vis >= scroll {
        let rect = Rect::new(diff_area.x, diff_area.y + drawn as u16, diff_area.width, 1);
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                state.i18n.t("view.commit.diff_truncated"),
                Style::default().fg(theme.status_conflicted),
            ))),
            rect,
        );
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
            if sw.diff_side_by_side() {
                state.i18n.t("view.commit.diff_split_btn")
            } else {
                state.i18n.t("view.commit.diff_unified_btn")
            }
            .to_string(),
            sw.diff_side_by_side(),
            StageDiffToolbarButton::SideBySide,
        ),
        (
            state.i18n.t_fmt(
                "view.commit.diff_context_btn",
                &[("ctx", state.i18n.diff_context_label(sw.diff_context()))],
            ),
            false,
            StageDiffToolbarButton::Context,
        ),
        (
            state.i18n.t_fmt(
                "view.log_details.ws_btn",
                &[("ws", state.i18n.diff_whitespace_label(sw.diff_whitespace()))],
            ),
            sw.diff_whitespace() != WhitespaceMode::Default,
            StageDiffToolbarButton::Whitespace,
        ),
        (
            state.i18n.t("view.log_details.wrap_btn").to_string(),
            sw.diff_soft_wrap(),
            StageDiffToolbarButton::SoftWrap,
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

    if let Some(diff) = sw.current_diff()
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

    render_toolbar(frame, area, ctx, 4, &buttons, HitTarget::StageDiffToolbar);
}

fn render_diff_hint_bar(
    frame: &mut Frame<'_>,
    area: Rect,
    mode: DiffHintMode,
    state: &AppState,
    ctx: &RenderContext<'_>,
) {
    let (text, fg) = match mode {
        DiffHintMode::Amend => (
            state.i18n.t("view.commit.diff_hint_amend").to_string(),
            ctx.theme.fg_muted,
        ),
        DiffHintMode::WhitespaceIgnore => (
            format!(" {}", state.i18n.t("view.commit.diff_hint_whitespace")),
            ctx.theme.status_untracked,
        ),
        DiffHintMode::VisualLine => (
            format!(" {}", state.i18n.t("view.commit.diff_visual_line")),
            ctx.theme.ref_head.icon,
        ),
    };
    frame.render_widget(
        Paragraph::new(Span::styled(
            truncate_to_width(&text, area.width as usize),
            Style::default().fg(fg),
        ))
        .style(Style::default().bg(ctx.theme.bg_panel)),
        area,
    );
}
