use jgit_core::graph::{GraphPrintElement, PrintElementGenerator, VisibleRowIdx};
use jgit_core::{GroupOptions, Oid, PaneId, RefInfo, SplitAxis, SplitterId, group_refs_for_table};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use unicode_width::UnicodeWidthStr;

use crate::app::{AppState, RenderContext};
use crate::graph::{
    CellGridGraphBackend, GraphRenderBackend, GraphViewportSlice, RenderGraphElement, RowGraphSlice,
};
use crate::hit_test::{GraphHitKind, HitTarget};
use crate::log_view::{DiffPreviewDock, FilterField, format_absolute_time, format_relative_time};
use crate::views::log_branches::{render_branches_pane, render_branches_stripe};
use crate::views::log_details::{
    render_changes_pane, render_details_or_diff, render_docked_diff, render_zoomed_diff,
};
use crate::views::{
    highlight_spans, render_pane_frame, render_splitter_divider, row_background, truncate_to_width,
};
use crate::widgets::PillBadgeBar;

/// Width of the collapsed Branches stripe (`ExpandStripeButton`).
pub const STRIPE_WIDTH: u16 = 2;

pub fn render_log_tab(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &AppState,
    ctx: &mut RenderContext<'_>,
    splitter_containers: &mut Vec<(SplitterId, SplitAxis, Rect)>,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    if state.details.diff_zoomed {
        render_zoomed_diff(frame, area, state, ctx);
        return;
    }

    let mut main_area = area;
    let mut bottom_diff_area: Option<Rect> = None;
    if state.details.diff_preview && state.details.diff_dock == DiffPreviewDock::Bottom {
        let bottom_split =
            state
                .splitters
                .split(area, SplitterId::LogDiffBottomVertical, SplitAxis::Vertical);
        splitter_containers.push((
            SplitterId::LogDiffBottomVertical,
            SplitAxis::Vertical,
            bottom_split.container,
        ));
        main_area = bottom_split.first;
        if !bottom_split.collapsed {
            render_splitter_divider(
                frame,
                bottom_split.divider,
                SplitterId::LogDiffBottomVertical,
                SplitAxis::Vertical,
                ctx,
            );
            bottom_diff_area = Some(bottom_split.second);
        }
    }

    let collapsed = state.branches.is_collapsed(state.terminal_area.width);
    let mut mid_right_area = if collapsed {
        let stripe_w = STRIPE_WIDTH.min(main_area.width);
        render_branches_stripe(
            frame,
            Rect::new(main_area.x, main_area.y, stripe_w, main_area.height),
            ctx,
        );
        Rect::new(
            main_area.x + stripe_w,
            main_area.y,
            main_area.width - stripe_w,
            main_area.height,
        )
    } else {
        let left_split =
            state
                .splitters
                .split(main_area, SplitterId::LogLeftHorizontal, SplitAxis::Horizontal);
        splitter_containers.push((
            SplitterId::LogLeftHorizontal,
            SplitAxis::Horizontal,
            left_split.container,
        ));
        if left_split.collapsed {
            left_split.first
        } else {
            render_branches_pane(frame, left_split.first, state, ctx);
            render_splitter_divider(
                frame,
                left_split.divider,
                SplitterId::LogLeftHorizontal,
                SplitAxis::Horizontal,
                ctx,
            );
            left_split.second
        }
    };

    let mut right_diff_area: Option<Rect> = None;
    if state.details.diff_preview && state.details.diff_dock == DiffPreviewDock::Right {
        let dock_split = state.splitters.split(
            mid_right_area,
            SplitterId::LogDiffRightHorizontal,
            SplitAxis::Horizontal,
        );
        splitter_containers.push((
            SplitterId::LogDiffRightHorizontal,
            SplitAxis::Horizontal,
            dock_split.container,
        ));
        mid_right_area = dock_split.first;
        if !dock_split.collapsed {
            render_splitter_divider(
                frame,
                dock_split.divider,
                SplitterId::LogDiffRightHorizontal,
                SplitAxis::Horizontal,
                ctx,
            );
            right_diff_area = Some(dock_split.second);
        }
    }

    let right_split = state.splitters.split(
        mid_right_area,
        SplitterId::LogRightHorizontal,
        SplitAxis::Horizontal,
    );
    splitter_containers.push((
        SplitterId::LogRightHorizontal,
        SplitAxis::Horizontal,
        right_split.container,
    ));

    let commits_sub = state
        .repo_snapshot
        .as_ref()
        .map(|s| {
            if s.is_unborn_head {
                state.i18n.t("view.log.unborn_head").to_string()
            } else {
                let visible = state.log_graph.visible_count();
                let sel = state.log_graph.selected_range();
                let sel_n = sel.end() - sel.start() + 1;
                let total_s = s.commits.len().to_string();
                let vis_s = visible.to_string();
                let mut out = if visible == s.commits.len() {
                    state.i18n.t_fmt("view.log.commits_count", &[("count", &total_s)])
                } else {
                    state.i18n.t_fmt(
                        "view.log.commits_filtered",
                        &[("visible", &vis_s), ("total", &total_s)],
                    )
                };
                if sel_n > 1 {
                    let sel_s = sel_n.to_string();
                    out.push_str(&state.i18n.t_fmt("view.log.selected_count", &[("count", &sel_s)]));
                }
                out
            }
        })
        .unwrap_or_default();
    let center_body = render_pane_frame(
        frame,
        right_split.first,
        PaneId::LogCommitTable,
        &commits_sub,
        ctx,
    );
    if center_body.height > 0 {
        let bar = Rect::new(center_body.x, center_body.y, center_body.width, 1);
        render_filter_bar(frame, bar, state, ctx);
        let table = Rect::new(
            center_body.x,
            center_body.y + 1,
            center_body.width,
            center_body.height - 1,
        );
        render_commits_table(frame, table, state, ctx);
    }

    if !right_split.collapsed {
        render_splitter_divider(
            frame,
            right_split.divider,
            SplitterId::LogRightHorizontal,
            SplitAxis::Horizontal,
            ctx,
        );

        let details_split = state.splitters.split(
            right_split.second,
            SplitterId::LogDetailsVertical,
            SplitAxis::Vertical,
        );
        splitter_containers.push((
            SplitterId::LogDetailsVertical,
            SplitAxis::Vertical,
            details_split.container,
        ));
        render_changes_pane(frame, details_split.first, state, ctx);
        if !details_split.collapsed {
            render_splitter_divider(
                frame,
                details_split.divider,
                SplitterId::LogDetailsVertical,
                SplitAxis::Vertical,
                ctx,
            );
            render_details_or_diff(frame, details_split.second, state, ctx);
        }
    }

    if let Some(diff_area) = right_diff_area {
        render_docked_diff(frame, diff_area, state, ctx);
    }
    if let Some(diff_area) = bottom_diff_area {
        render_docked_diff(frame, diff_area, state, ctx);
    }
}

/// `VcsLogFilterBar`: text box with `[.*]` / `[Cc]`, four dimension chips and `⚙`.
fn render_filter_bar(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &AppState,
    ctx: &mut RenderContext<'_>,
) {
    if area.width == 0 {
        return;
    }
    let theme = ctx.theme;
    frame.render_widget(
        Paragraph::new("").style(Style::default().bg(theme.bg_panel)),
        area,
    );
    let mut x = area.x;
    let right = area.right();
    let filter = &state.log_filter;

    // ⚙ Presentation, pinned to the far right.
    let gear = " ⚙ ";
    let gear_w = gear.width() as u16;
    if area.width > gear_w + 10 {
        let rect = Rect::new(right - gear_w, area.y, gear_w, 1);
        frame.render_widget(
            Paragraph::new(Span::styled(
                gear,
                Style::default().fg(theme.fg_primary).bg(theme.bg_panel),
            )),
            rect,
        );
        ctx.hit_registry
            .register(rect, 3, HitTarget::FilterPresentation);
    }
    let reserve = gear_w;

    let registry = &mut *ctx.hit_registry;
    let mut put = |frame: &mut Frame<'_>,
                   x: &mut u16,
                   text: &str,
                   style: Style,
                   target: Option<HitTarget>,
                   reserve: u16|
     -> bool {
        let w = text.width() as u16;
        if *x + w + reserve > right {
            return false;
        }
        let rect = Rect::new(*x, area.y, w, 1);
        frame.render_widget(Paragraph::new(Span::styled(text.to_string(), style)), rect);
        if let Some(t) = target {
            registry.register(rect, 3, t);
        }
        *x += w;
        true
    };

    // Text box.
    let text = &filter.spec.text;
    let box_w = ((area.width as usize) / 3).clamp(10, 32);
    let (content, fg) = if text.query.is_empty() && !filter.editing_text {
        (
            state.i18n.t("view.log.search_placeholder").to_string(),
            theme.fg_muted,
        )
    } else {
        (text.query.clone(), theme.fg_bright)
    };
    let cursor = if filter.editing_text { "▏" } else { "" };
    let inner_w = box_w.saturating_sub(3 + cursor.width());
    let shown = if content.width() > inner_w {
        // Keep the tail visible while typing.
        let mut tail: String = content.chars().rev().collect();
        tail = truncate_to_width(&tail, inner_w);
        tail.chars().rev().collect()
    } else {
        content
    };
    let pad = inner_w.saturating_sub(shown.width());
    let box_bg = if filter.editing_text {
        theme.bg_selection_focused
    } else {
        theme.bg_elevated
    };
    let box_text = format!(" ⌕ {shown}{cursor}{}", " ".repeat(pad));
    let err = state.log_graph.filter_error.is_some();
    put(
        frame,
        &mut x,
        &box_text,
        Style::default()
            .fg(if err { theme.status_conflicted } else { fg })
            .bg(box_bg),
        Some(HitTarget::FilterTextInput),
        reserve,
    );
    let toggle_style = |on: bool| {
        if on {
            Style::default()
                .fg(theme.fg_bright)
                .bg(theme.border_focused)
        } else {
            Style::default().fg(theme.fg_muted).bg(box_bg)
        }
    };
    put(
        frame,
        &mut x,
        "[.*]",
        toggle_style(text.regex),
        Some(HitTarget::FilterRegexToggle),
        reserve,
    );
    put(
        frame,
        &mut x,
        "[Cc]",
        toggle_style(text.match_case),
        Some(HitTarget::FilterCaseToggle),
        reserve,
    );

    for field in FilterField::ALL {
        let active = filter.field_active(field);
        let mut label = filter.field_label(field);
        if field == FilterField::Paths && filter.path_matches_pending(state.repo_snapshot.as_ref())
        {
            label.push('…');
        }
        let chip = format!(" {}: {} ▾", field.title(), truncate_to_width(&label, 18));
        let style = Style::default()
            .fg(if active {
                theme.fg_accent_link
            } else {
                theme.fg_primary
            })
            .bg(theme.bg_panel);
        if !put(
            frame,
            &mut x,
            &chip,
            style,
            Some(HitTarget::FilterChip(field)),
            reserve,
        ) {
            break;
        }
        if active {
            put(
                frame,
                &mut x,
                "×",
                Style::default().fg(theme.fg_muted).bg(theme.bg_panel),
                Some(HitTarget::FilterClear(field)),
                reserve,
            );
        }
    }
}

fn render_commits_table(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &AppState,
    ctx: &mut RenderContext<'_>,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let Some(snap) = &state.repo_snapshot else {
        frame.render_widget(
            Paragraph::new(Span::styled(
                " Loading commit graph…",
                Style::default().fg(ctx.theme.fg_muted),
            )),
            area,
        );
        return;
    };

    let Some(cached) = &state.log_graph.cached else {
        return;
    };

    if snap.commits.is_empty() || cached.visible_count() == 0 {
        let msg = if snap.is_unborn_head {
            " Empty repository (unborn HEAD — no commits yet)"
        } else if !state.log_filter.spec.is_empty() {
            " No commits match the current filters"
        } else {
            " No commits found"
        };
        frame.render_widget(
            Paragraph::new(Span::styled(msg, Style::default().fg(ctx.theme.fg_muted))),
            area,
        );
        return;
    }

    // Viewport virtualization: only `offset .. offset + height` rows are touched.
    let visible_h = area.height as usize;
    let total = cached.visible_count();
    let selected = state.log_graph.selected_idx.min(total.saturating_sub(1));
    let max_off = total.saturating_sub(visible_h);
    let mut offset = state.log_graph.scroll_offset.min(max_off);
    if selected < offset {
        offset = selected;
    } else if visible_h > 0 && selected >= offset + visible_h {
        offset = selected + 1 - visible_h;
    }

    let end = (offset + visible_h).min(total);
    let rows_len = end - offset;

    let focused = ctx.focused_pane == PaneId::LogCommitTable;
    let presentation = &state.presentation;
    let mut elem_gen =
        PrintElementGenerator::new(&cached.permanent, &cached.visible, snap.head_oid);
    let matcher = state.log_filter.spec.text.compile().ok().flatten();

    struct PreparedLogRow {
        visible_row: VisibleRowIdx,
        commit_oid: Oid,
        row_bg: Color,
        elements: Vec<RenderGraphElement>,
    }

    let mut prepared_rows: Vec<PreparedLogRow> = Vec::with_capacity(rows_len);
    let mut max_track_pos = 0usize;

    for row_idx in offset..end {
        let vis_row = row_idx as VisibleRowIdx;
        let perm_id = cached.visible.linear.perm_id(vis_row);
        let commit = cached
            .commit_at_visible_row(snap, row_idx)
            .expect("visible row maps to valid commit");
        let is_sel = state.log_graph.is_row_selected(row_idx);
        let is_in_current_branch = presentation.highlight_current_branch
            && cached.current_branch_reachable.contains(perm_id as usize);
        let is_hovered = matches!(
            ctx.hover_target,
            Some(HitTarget::CommitTableRow { visible_row, .. })
            | Some(HitTarget::GraphPrintElement { visible_row, .. })
            | Some(HitTarget::CommitRefPill { visible_row, .. })
            | Some(HitTarget::CommitRefOverflow { visible_row, .. })
                if *visible_row == row_idx
        );
        let default_bg = if is_in_current_branch {
            ctx.theme.bg_current_branch_row
        } else {
            ctx.theme.bg_canvas
        };
        let bg = row_background(is_sel, focused, is_hovered, default_bg, ctx.theme);

        let raw_elems = elem_gen.print_elements(vis_row);
        let mut rendered = Vec::with_capacity(raw_elems.len());
        for el in raw_elems {
            max_track_pos = max_track_pos.max(el.max_pos());
            let target_oid = match &el {
                GraphPrintElement::TerminalEdge { target_perm_id, .. } => {
                    Some(cached.permanent.linear.commit(*target_perm_id).oid)
                }
                _ => None,
            };
            let is_elem_selected = is_element_highlighted(&el, row_idx, ctx.hover_target);
            rendered.push(RenderGraphElement {
                element: el,
                target_oid,
                is_selected: is_elem_selected,
            });
        }
        prepared_rows.push(PreparedLogRow {
            visible_row: vis_row,
            commit_oid: commit.oid,
            row_bg: bg,
            elements: rendered,
        });
    }

    let max_graph_cols = ((area.width as usize * 45) / 100)
        .max(2)
        .min(area.width as usize);
    let graph_tracks = cached.recommended_tracks.max(max_track_pos + 1).max(1);
    let graph_cols = ((graph_tracks * 2).min(max_graph_cols) as u16).min(area.width);
    let graph_area = Rect::new(area.x, area.y, graph_cols, rows_len as u16);

    // Fixed-width right columns: [Author 14-18] [Date 12-16] [Hash 8].
    let text_total = area.width.saturating_sub(graph_cols);
    let author_w: u16 = if text_total >= 60 {
        if text_total >= 100 { 18 } else { 14 }
    } else {
        0
    };
    let date_w: u16 = if text_total >= 40 {
        if presentation.relative_dates { 12 } else { 17 }
    } else {
        0
    };
    let hash_w: u16 = if presentation.show_hash_column && text_total >= 50 {
        9
    } else {
        0
    };
    let right_cols = author_w + date_w + hash_w;
    let now = chrono::Local::now().timestamp();

    // Register row-level hit regions first (z=1) before graph elements & pills (z=2)
    for (vis_idx, row) in prepared_rows.iter().enumerate() {
        let row_idx = offset + vis_idx;
        let row_rect = Rect::new(area.x, area.y.saturating_add(vis_idx as u16), area.width, 1);
        ctx.hit_registry.register(
            row_rect,
            1,
            HitTarget::CommitTableRow {
                visible_row: row_idx,
                commit_oid: row.commit_oid,
            },
        );
    }

    let row_slices: Vec<RowGraphSlice<'_>> = prepared_rows
        .iter()
        .map(|row| RowGraphSlice {
            visible_row: row.visible_row,
            commit_oid: row.commit_oid,
            row_bg: row.row_bg,
            elements: &row.elements,
        })
        .collect();

    let viewport_slice = GraphViewportSlice {
        rows: &row_slices,
        area: graph_area,
        occluding_rects: &[],
        color_map: &cached.color_map,
    };
    match ctx.graph_renderer.take() {
        Some(renderer) => {
            renderer.render_viewport(&viewport_slice, frame.buffer_mut(), ctx);
            ctx.graph_renderer = Some(renderer);
        }
        None => {
            CellGridGraphBackend::new().render_viewport(&viewport_slice, frame.buffer_mut(), ctx)
        }
    }

    for (vis_idx, row) in prepared_rows.iter().enumerate() {
        let bg = row.row_bg;
        let row_idx = offset + vis_idx;
        let commit = cached
            .commit_at_visible_row(snap, row_idx)
            .expect("visible row maps to valid commit");
        let row_y = area.y.saturating_add(vis_idx as u16);
        let text_x = area.x.saturating_add(graph_cols);
        let text_w = area.right().saturating_sub(text_x);
        if text_w == 0 {
            continue;
        }
        let is_sel = state.log_graph.is_row_selected(row_idx);
        let is_head = snap.head_oid == Some(commit.oid);
        let main_w = text_w.saturating_sub(right_cols);
        let main_rect = Rect::new(text_x, row_y, main_w, 1);

        frame.render_widget(
            Paragraph::new("").style(Style::default().bg(bg)),
            Rect::new(text_x, row_y, text_w, 1),
        );

        let head_marker = is_head.then(|| RefInfo::head_marker(commit.oid));
        let groups = group_refs_for_table(
            snap.refs
                .iter()
                .filter(|r| r.target_oid == commit.oid)
                .chain(head_marker.as_ref()),
            GroupOptions {
                compact: state.compact_refs,
                show_tag_names: state.show_tag_names,
            },
        );

        let pills_x = main_rect.x.saturating_add(1).min(main_rect.right());
        let pills_area = Rect::new(pills_x, row_y, main_rect.right().saturating_sub(pills_x), 1);
        let pills_w = PillBadgeBar {
            groups: &groups,
            glyphs: ctx.glyph_mode.symbols(),
            theme: ctx.theme,
            row_bg: bg,
            visible_row: row_idx,
        }
        .render(pills_area, frame.buffer_mut(), ctx.hit_registry);

        let subject_x = pills_x.saturating_add(pills_w);
        let subject_area = Rect::new(
            subject_x,
            row_y,
            main_rect.right().saturating_sub(subject_x),
            1,
        );
        let is_merge = commit.parents.len() > 1;
        let base = Style::default()
            .fg(if is_sel || is_head {
                ctx.theme.fg_bright
            } else if is_merge && presentation.highlight_merges {
                // MergeCommitsHighlighter
                ctx.theme.fg_muted
            } else {
                ctx.theme.fg_primary
            })
            .bg(bg)
            .add_modifier(if is_head {
                Modifier::BOLD
            } else {
                Modifier::empty()
            });
        let hl = Style::default()
            .fg(ctx.theme.ref_head.pill_fg)
            .bg(ctx.theme.ref_head.pill_bg)
            .add_modifier(Modifier::BOLD);
        let ranges = matcher
            .as_ref()
            .map(|m| m.find_ranges(&commit.subject))
            .unwrap_or_default();
        let spans = highlight_spans(
            &commit.subject,
            &ranges,
            subject_area.width as usize,
            base,
            hl,
        );
        frame.render_widget(Paragraph::new(Line::from(spans)), subject_area);

        // Right columns.
        let mut col_x = main_rect.right();
        let muted = Style::default().fg(ctx.theme.fg_muted).bg(bg);
        if author_w > 0 {
            let author = truncate_to_width(&commit.author_name, author_w as usize - 1);
            frame.render_widget(
                Paragraph::new(Span::styled(
                    format!(" {author}"),
                    Style::default()
                        .fg(if is_sel {
                            ctx.theme.fg_bright
                        } else {
                            ctx.theme.fg_primary
                        })
                        .bg(bg),
                )),
                Rect::new(col_x, row_y, author_w, 1),
            );
            col_x += author_w;
        }
        if date_w > 0 {
            let ts = if presentation.use_author_date {
                commit.author_timestamp
            } else {
                commit.timestamp
            };
            let date = if presentation.relative_dates {
                format_relative_time(ts, now)
            } else {
                format_absolute_time(ts, true)
            };
            frame.render_widget(
                Paragraph::new(Span::styled(
                    format!(" {}", truncate_to_width(&date, date_w as usize - 1)),
                    muted,
                )),
                Rect::new(col_x, row_y, date_w, 1),
            );
            col_x += date_w;
        }
        if hash_w > 0 {
            frame.render_widget(
                Paragraph::new(Span::styled(format!(" {}", commit.oid.short()), muted)),
                Rect::new(col_x, row_y, hash_w, 1),
            );
        }
    }
}

fn is_element_highlighted(
    el: &GraphPrintElement,
    row_idx: usize,
    hover_target: Option<&HitTarget>,
) -> bool {
    let Some(HitTarget::GraphPrintElement {
        visible_row: hover_row,
        kind,
        ..
    }) = hover_target
    else {
        return false;
    };

    match (el, kind) {
        (GraphPrintElement::Node { .. }, GraphHitKind::Node { .. }) => *hover_row == row_idx,
        (
            GraphPrintElement::Edge {
                up_row, down_row, ..
            },
            GraphHitKind::Edge {
                up_row: h_up,
                down_row: h_dn,
            },
        )
        | (
            GraphPrintElement::Edge {
                up_row, down_row, ..
            },
            GraphHitKind::Arrow {
                up_row: h_up,
                down_row: h_dn,
                ..
            },
        )
        | (
            GraphPrintElement::TerminalEdge {
                up_row, down_row, ..
            },
            GraphHitKind::Edge {
                up_row: h_up,
                down_row: h_dn,
            },
        ) => (*up_row as usize, *down_row as usize) == (*h_up, *h_dn),
        (
            GraphPrintElement::TerminalEdge {
                up_row,
                down_row,
                target_row,
                direction,
                ..
            },
            GraphHitKind::Arrow {
                up_row: h_up,
                down_row: h_dn,
                target_row: h_target,
                is_down,
                ..
            },
        ) => {
            *hover_row == row_idx
                && (*up_row as usize, *down_row as usize) == (*h_up, *h_dn)
                && *target_row as usize == *h_target
                && (*direction == jgit_core::graph::EdgeDirection::Down) == *is_down
        }
        _ => false,
    }
}
