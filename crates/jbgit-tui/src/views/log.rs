use jbgit_core::graph::{GraphPrintElement, PrintElementGenerator, VisibleRowIdx};
use jbgit_core::{GroupOptions, Oid, PaneId, RefInfo, SplitAxis, SplitterId, group_refs_for_table};
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
use crate::log_view::{DiffPreviewDock, FilterField, format_absolute_time};
use crate::views::log_branches::{render_branches_pane, render_branches_stripe};
use crate::views::log_details::{
    render_changes_pane, render_details_or_diff, render_docked_diff, render_zoomed_diff,
};
use crate::views::{
    highlight_spans, render_pane_frame, render_splitter_divider, row_background, truncate_to_width,
};
use crate::widgets::PillBadgeBar;
use crate::widgets::pill_badge::layout_pills;

/// Width of the collapsed Branches stripe (`ExpandStripeButton`).
pub const STRIPE_WIDTH: u16 = 2;

pub fn render_log_tab(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &AppState,
    ctx: &mut RenderContext<'_>,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    if state.log.details.diff.zoomed {
        render_zoomed_diff(frame, area, state, ctx);
        return;
    }

    let mut main_area = area;
    let mut bottom_diff_area: Option<Rect> = None;
    if state.log.details.diff_preview && state.log.details.diff_dock == DiffPreviewDock::Bottom {
        let bottom_split =
            state
                .splitters
                .split(area, SplitterId::LogDiffBottomVertical, SplitAxis::Vertical);
        ctx.record_splitter_container(
            SplitterId::LogDiffBottomVertical,
            SplitAxis::Vertical,
            bottom_split.container,
        );
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

    let collapsed = state.log.branches.is_collapsed(state.terminal_area.width);
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
        let left_split = state.splitters.split(
            main_area,
            SplitterId::LogLeftHorizontal,
            SplitAxis::Horizontal,
        );
        ctx.record_splitter_container(
            SplitterId::LogLeftHorizontal,
            SplitAxis::Horizontal,
            left_split.container,
        );
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
    if state.log.details.diff_preview && state.log.details.diff_dock == DiffPreviewDock::Right {
        let dock_split = state.splitters.split(
            mid_right_area,
            SplitterId::LogDiffRightHorizontal,
            SplitAxis::Horizontal,
        );
        ctx.record_splitter_container(
            SplitterId::LogDiffRightHorizontal,
            SplitAxis::Horizontal,
            dock_split.container,
        );
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
    ctx.record_splitter_container(
        SplitterId::LogRightHorizontal,
        SplitAxis::Horizontal,
        right_split.container,
    );

    let commits_sub = state
        .repo_snapshot
        .as_ref()
        .map(|s| {
            if s.is_unborn_head {
                state.i18n.t("view.log.unborn_head").to_string()
            } else {
                let visible = state.log.graph.visible_count();
                let sel_n = state.log.graph.selection_count();
                let total_s = if s.has_more_commits {
                    format!("{}+", s.commits.len())
                } else {
                    s.commits.len().to_string()
                };
                let vis_s = visible.to_string();
                let mut out = if visible == s.commits.len() {
                    state
                        .i18n
                        .t_fmt("view.log.commits_count", &[("count", &total_s)])
                } else {
                    state.i18n.t_fmt(
                        "view.log.commits_filtered",
                        &[("visible", &vis_s), ("total", &total_s)],
                    )
                };
                if sel_n > 1 || (sel_n == 1 && state.log.graph.is_mark_mode()) {
                    let sel_s = sel_n.to_string();
                    out.push_str(
                        &state
                            .i18n
                            .t_fmt("view.log.selected_count", &[("count", &sel_s)]),
                    );
                }
                if s.is_shallow {
                    out.push_str(state.i18n.t("view.log.shallow_badge"));
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
        let mut table_y = center_body.y + 1;
        let mut table_h = center_body.height - 1;
        if let Some(bc) = &state.log.branch_compare
            && table_h > 1
        {
            let cmp_bar = Rect::new(center_body.x, table_y, center_body.width, 1);
            render_branch_compare_bar(frame, cmp_bar, bc, state, ctx);
            table_y += 1;
            table_h -= 1;
        }
        let table = Rect::new(center_body.x, table_y, center_body.width, table_h);
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
        ctx.record_splitter_container(
            SplitterId::LogDetailsVertical,
            SplitAxis::Vertical,
            details_split.container,
        );
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

/// Compact 1-line bar shown while comparing two branches (`A..B` / `B..A` / `A...B` + Tree Diff).
fn render_branch_compare_bar(
    frame: &mut Frame<'_>,
    area: Rect,
    bc: &crate::log_view::BranchCompareState,
    state: &AppState,
    ctx: &mut RenderContext<'_>,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let range = bc.range_label();
    let ol = bc.only_left.to_string();
    let or = bc.only_right.to_string();
    let label = state.i18n.t_fmt(
        "view.log.compare_banner",
        &[("range", &range), ("left_only", &ol), ("right_only", &or)],
    );
    let cycle_btn = state.i18n.t("view.log.compare_cycle_btn");
    let diff_btn = if bc.tree_diff {
        state.i18n.t("view.log.compare_tree_diff_on_btn")
    } else {
        state.i18n.t("view.log.compare_tree_diff_btn")
    };
    let close_btn = " ✕ ";
    let btns_w = cycle_btn.width() + diff_btn.width() + close_btn.width() + 2;
    let label_w = (area.width as usize).saturating_sub(btns_w + 1);
    let mut x = area.x;
    let mut spans = vec![Span::styled(
        format!(" {} ", truncate_to_width(&label, label_w)),
        Style::default()
            .fg(ctx.theme.fg_bright)
            .bg(ctx.theme.bg_elevated)
            .add_modifier(Modifier::BOLD),
    )];
    x += spans[0].content.width() as u16;
    let buttons = [
        (cycle_btn, HitTarget::BranchCompareCycle, false),
        (diff_btn, HitTarget::BranchCompareTreeDiff, bc.tree_diff),
        (close_btn, HitTarget::BranchCompareClose, false),
    ];
    for (txt, target, active) in buttons {
        let w = txt.width() as u16;
        if x + w > area.right() {
            break;
        }
        let st = if active {
            Style::default()
                .fg(ctx.theme.bg_canvas)
                .bg(ctx.theme.fg_accent_link)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
                .fg(ctx.theme.fg_accent_link)
                .bg(ctx.theme.bg_panel)
        };
        spans.push(Span::styled(txt.to_string(), st));
        ctx.hit_registry
            .register(Rect::new(x, area.y, w, 1), 3, target);
        x += w;
        if x < area.right() {
            spans.push(Span::styled(
                " ",
                Style::default().bg(ctx.theme.bg_elevated),
            ));
            x += 1;
        }
    }
    frame.render_widget(
        Paragraph::new(Line::from(spans)).style(Style::default().bg(ctx.theme.bg_elevated)),
        area,
    );
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
    let filter = &state.log.filter;

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

    let mut active_fields: Vec<FilterField> = Vec::new();
    let mut inactive_fields: Vec<FilterField> = Vec::new();
    for field in FilterField::ALL {
        if filter.field_active(field) {
            active_fields.push(field);
        } else {
            inactive_fields.push(field);
        }
    }

    let nm_on = filter.spec.no_merges;
    let nm_label = state.i18n.t("filter.no_merges");
    let nm_chip_full = format!(" [{nm_label}] ");
    let nm_chip_compact = format!(" [{nm_label}]");
    let nm_chip_tight = format!("[{nm_label}]");

    let any_chip_active = !active_fields.is_empty() || nm_on || filter.spec.range.is_some();

    let condensed_inactive_fields_w: usize = inactive_fields
        .iter()
        .map(|&field| format!(" {}▾", state.i18n.filter_field_title(field)).width())
        .sum();
    let min_inactive_compact =
        condensed_inactive_fields_w + if nm_on { 0 } else { nm_chip_compact.width() };
    let min_inactive_tight =
        condensed_inactive_fields_w + if nm_on { 0 } else { nm_chip_tight.width() };

    // Text box: stay trimmer when unfocused & empty while chip filters are active
    // so active chips and condensed inactive triggers (`路径▾`, `[排除合并]`) remain reachable at 140x40.
    let text = &filter.spec.text;
    let usable_w = area.width.saturating_sub(reserve) as usize;
    let box_w = if text.query.is_empty() && !filter.editing_text && any_chip_active {
        let base_w = ((area.width as usize) / 4).clamp(10, 20);
        let active_desired_w: usize = active_fields
            .iter()
            .map(|&field| {
                let field_title = state.i18n.filter_field_title(field);
                let empty_w = state
                    .i18n
                    .t_fmt("filter.chip", &[("field", field_title), ("label", "")])
                    .width();
                let label_w = if field == FilterField::Date {
                    state
                        .i18n
                        .format_date_filter_label(&filter.field_label(field))
                        .width()
                } else {
                    filter.field_label(field).width()
                };
                empty_w + 1 + label_w.min(14)
            })
            .sum::<usize>()
            + if nm_on { nm_chip_compact.width() } else { 0 };
        let needed = 8 + active_desired_w + min_inactive_compact;
        if usable_w < base_w + needed {
            usable_w.saturating_sub(needed).clamp(8, 12)
        } else {
            base_w
        }
    } else {
        ((area.width as usize) / 3).clamp(10, 32)
    };
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
    let err = state.log.graph.filter_error.is_some();
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

    if let Some(range) = &filter.spec.range {
        let n_s = range.len().to_string();
        let chip = state.i18n.t_fmt(
            "filter.range_chip",
            &[
                ("label", &truncate_to_width(&range.label, 18)),
                ("count", &n_s),
            ],
        );
        let style = Style::default()
            .fg(theme.fg_bright)
            .bg(theme.border_focused);
        if put(frame, &mut x, &chip, style, None, reserve) {
            put(
                frame,
                &mut x,
                "×",
                Style::default()
                    .fg(theme.fg_bright)
                    .bg(theme.border_focused),
                Some(HitTarget::FilterRangeClear),
                reserve,
            );
        }
    }

    if state.log.filter.saved_before_dir_history.is_some() {
        put(
            frame,
            &mut x,
            &format!(" [{}]", state.i18n.t("view.log.restore_dir_history_filter")),
            Style::default()
                .fg(theme.status_modified)
                .bg(theme.bg_panel)
                .add_modifier(Modifier::BOLD),
            Some(HitTarget::FilterRestoreDirHistory),
            reserve,
        );
    }

    // Render active filter chips ahead of inactive `All` chips so active filters
    // (Date, Paths, --no-merges, etc.) are never silently dropped on narrow panes.
    let nm_active_style = Style::default()
        .fg(theme.fg_bright)
        .bg(theme.border_focused)
        .add_modifier(Modifier::BOLD);
    let nm_inactive_style = Style::default().fg(theme.fg_muted).bg(theme.bg_panel);

    let total_active = active_fields.len() + usize::from(nm_on);
    let mut processed_active = 0usize;
    let mut hidden_active = 0usize;

    let active_chip_style = Style::default().fg(theme.fg_accent_link).bg(theme.bg_panel);
    let clear_style = Style::default().fg(theme.fg_muted).bg(theme.bg_panel);

    for field in active_fields {
        processed_active += 1;
        let more_active_after = processed_active < total_active;
        let overflow_reserve: u16 = if hidden_active > 0 || more_active_after {
            6
        } else {
            0
        };
        let mut label = if field == FilterField::Date {
            state
                .i18n
                .format_date_filter_label(&filter.field_label(field))
        } else {
            filter.field_label(field)
        };
        if field == FilterField::Paths && filter.path_matches_pending(state.repo_snapshot.as_ref())
        {
            label.push('…');
        }
        let field_title = state.i18n.filter_field_title(field);
        let empty_chip = state
            .i18n
            .t_fmt("filter.chip", &[("field", field_title), ("label", "")]);
        // Overhead includes the trailing clickable `×` clear button (1 col).
        let overhead = empty_chip.width() + 1;
        let avail_with_ovf = right.saturating_sub(x + reserve + overflow_reserve) as usize;
        let avail_no_ovf = right.saturating_sub(x + reserve) as usize;
        let budget = if !more_active_after && hidden_active == 0 {
            avail_no_ovf
        } else {
            avail_with_ovf
        };
        // Dynamically budget active chip label width so condensed inactive triggers
        // (`路径▾`, `[排除合并]`, etc.) stay visible at 140x40 while preserving at least
        // 8 cols of readable label before falling back to prioritizing the active chip.
        let desired_label = label.width().min(18);
        let max_label_cap = if !more_active_after && hidden_active == 0 {
            let after_compact = budget.saturating_sub(min_inactive_compact + overhead);
            let after_tight = budget.saturating_sub(min_inactive_tight + overhead);
            if after_compact >= desired_label || after_tight >= desired_label {
                desired_label
            } else if after_compact >= 8 {
                after_compact.min(18)
            } else if after_tight >= 8 {
                after_tight.min(18)
            } else {
                budget.saturating_sub(overhead).min(18)
            }
        } else {
            18
        };
        let chip = if max_label_cap >= 3 && budget >= overhead + 3 {
            let effective_cap = max_label_cap.min(budget.saturating_sub(overhead));
            Some(state.i18n.t_fmt(
                "filter.chip",
                &[
                    ("field", field_title),
                    ("label", &truncate_to_width(&label, effective_cap)),
                ],
            ))
        } else {
            None
        };
        if let Some(chip_text) = chip {
            if put(
                frame,
                &mut x,
                &chip_text,
                active_chip_style,
                Some(HitTarget::FilterChip(field)),
                reserve + 1,
            ) {
                put(
                    frame,
                    &mut x,
                    "×",
                    clear_style,
                    Some(HitTarget::FilterClear(field)),
                    reserve,
                );
            } else {
                hidden_active += 1;
            }
        } else {
            hidden_active += 1;
        }
    }

    if nm_on {
        let ovf_res = if hidden_active > 0 { 6 } else { 0 };
        let placed = put(
            frame,
            &mut x,
            &nm_chip_full,
            nm_active_style,
            Some(HitTarget::FilterNoMergesToggle),
            reserve + ovf_res,
        ) || put(
            frame,
            &mut x,
            &nm_chip_compact,
            nm_active_style,
            Some(HitTarget::FilterNoMergesToggle),
            reserve + ovf_res,
        );
        if !placed {
            hidden_active += 1;
        }
    }

    if hidden_active > 0 {
        let ovf_pill = format!(" +{hidden_active}× ");
        put(
            frame,
            &mut x,
            &ovf_pill,
            nm_active_style,
            Some(HitTarget::FilterClearAll),
            reserve,
        );
    }

    // Inactive chips fill remaining space; condense `Field: All ▾` -> `Field▾` when narrow
    // so Date / Paths / --no-merges remain reachable at 140x40.
    let all_label = state.i18n.t("filter.all");
    let inactive_style = Style::default().fg(theme.fg_primary).bg(theme.bg_panel);
    let full_inactive_w: usize = inactive_fields
        .iter()
        .map(|&field| {
            state
                .i18n
                .t_fmt(
                    "filter.chip",
                    &[
                        ("field", state.i18n.filter_field_title(field)),
                        ("label", all_label),
                    ],
                )
                .width()
        })
        .sum::<usize>()
        + if nm_on { 0 } else { nm_chip_full.width() };
    let avail_inactive = right.saturating_sub(x + reserve) as usize;
    let condense_inactive = full_inactive_w > avail_inactive;

    for field in inactive_fields {
        let chip = if condense_inactive {
            format!(" {}▾", state.i18n.filter_field_title(field))
        } else {
            state.i18n.t_fmt(
                "filter.chip",
                &[
                    ("field", state.i18n.filter_field_title(field)),
                    ("label", all_label),
                ],
            )
        };
        if !put(
            frame,
            &mut x,
            &chip,
            inactive_style,
            Some(HitTarget::FilterChip(field)),
            reserve,
        ) {
            break;
        }
    }

    if !nm_on {
        if condense_inactive {
            if !put(
                frame,
                &mut x,
                &nm_chip_compact,
                nm_inactive_style,
                Some(HitTarget::FilterNoMergesToggle),
                reserve,
            ) {
                put(
                    frame,
                    &mut x,
                    &nm_chip_tight,
                    nm_inactive_style,
                    Some(HitTarget::FilterNoMergesToggle),
                    reserve,
                );
            }
        } else {
            put(
                frame,
                &mut x,
                &nm_chip_full,
                nm_inactive_style,
                Some(HitTarget::FilterNoMergesToggle),
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
        if !state.is_unattached_workspace() {
            frame.render_widget(
                Paragraph::new(Span::styled(
                    state.i18n.t("view.log.loading_graph"),
                    Style::default().fg(ctx.theme.fg_muted),
                )),
                area,
            );
        }
        return;
    };

    let Some(cached) = &state.log.graph.cached else {
        return;
    };

    if snap.commits.is_empty() || cached.visible_count() == 0 {
        let msg = if snap.is_unborn_head {
            state.i18n.t("view.log.empty_unborn")
        } else if !state.log.filter.spec.is_empty() {
            state.i18n.t("view.log.empty_filtered")
        } else {
            state.i18n.t("view.log.empty_none")
        };
        frame.render_widget(
            Paragraph::new(Span::styled(msg, Style::default().fg(ctx.theme.fg_muted))),
            Rect::new(area.x, area.y, area.width, 1),
        );
        if snap.has_more_commits && area.height >= 2 {
            render_load_more_row(
                frame,
                Rect::new(area.x, area.y.saturating_add(1), area.width, 1),
                state,
                ctx,
                false,
                ctx.focused_pane == PaneId::LogCommitTable,
            );
        }
        return;
    }

    // Viewport virtualization: only `offset .. offset + height` rows are touched.
    let speed_search = &state.log.speed_search;
    let speed_bar_h: u16 = if speed_search.active && area.height >= 2 {
        1
    } else {
        0
    };
    let table_area = Rect::new(
        area.x,
        area.y,
        area.width,
        area.height.saturating_sub(speed_bar_h),
    );
    let visible_h = table_area.height as usize;
    let total = cached.visible_count();
    let has_load_more = snap.has_more_commits && total > 0;
    let nav_total = total + usize::from(has_load_more);
    let selected = state
        .log
        .graph
        .selected_idx
        .min(nav_total.saturating_sub(1));
    let commit_h = if !has_load_more && snap.is_shallow && visible_h >= 2 && selected + 1 == total {
        visible_h - 1
    } else {
        visible_h
    };
    let max_off = nav_total.saturating_sub(commit_h);
    let mut offset = state.log.graph.scroll_offset.min(max_off);
    if selected < offset {
        offset = selected;
    } else if commit_h > 0 && selected >= offset + commit_h {
        offset = selected + 1 - commit_h;
    }

    let end = (offset + commit_h).min(total);
    let rows_len = end.saturating_sub(offset);

    let focused = ctx.focused_pane == PaneId::LogCommitTable;
    let presentation = &state.log.presentation;
    let highlight_current_branch = state.log.should_highlight_current_branch(snap);
    let highlight_my_commits = state.log.should_highlight_my_commits();
    let hovered_expanded_seg = hovered_expanded_segment(cached, ctx.hover_target);
    let mut edges_cache = cached.edges_cache.borrow_mut();
    let mut elem_gen = PrintElementGenerator::with_cache(
        &cached.permanent,
        &cached.visible,
        snap.head_oid,
        &mut edges_cache,
    );
    let matcher = state.log.filter.compiled_text_matcher();

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
        let is_cursor = row_idx == state.log.graph.selected_idx;
        let is_sel = state.log.graph.is_row_selected(row_idx);
        let has_multi = state.log.graph.has_multi_selection();
        let is_in_current_branch =
            highlight_current_branch && cached.current_branch_reachable.contains(perm_id as usize);
        let is_hovered = matches!(
            ctx.hover_target,
            Some(HitTarget::CommitTableRow { visible_row, .. })
            | Some(HitTarget::CommitSignatureCell { visible_row, .. })
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
        let bg = if is_sel && is_cursor {
            row_background(true, focused, is_hovered, default_bg, ctx.theme)
        } else if is_sel {
            // Secondary highlight distinguishes non-cursor selected rows from the cursor row.
            ctx.theme.bg_selection_unfocused
        } else if is_cursor && has_multi {
            ctx.theme.bg_hover
        } else {
            row_background(false, focused, is_hovered, default_bg, ctx.theme)
        };

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
            let is_elem_selected = is_element_highlighted(
                &el,
                row_idx,
                ctx.hover_target,
                cached,
                hovered_expanded_seg,
            );
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

    let max_graph_cols = ((table_area.width as usize * 45) / 100)
        .max(2)
        .min(table_area.width as usize);
    let graph_tracks = cached.recommended_tracks.max(max_track_pos + 1).max(1);
    let graph_cols = ((graph_tracks * 2).min(max_graph_cols) as u16).min(table_area.width);
    let graph_area = Rect::new(table_area.x, table_area.y, graph_cols, rows_len as u16);

    // Fixed-width right columns: [Author 14-18] [Date 12-16] [Hash 8] [Sig 3].
    let text_total = table_area.width.saturating_sub(graph_cols);
    let author_w: u16 = if presentation.show_author_column && text_total >= 60 {
        if text_total >= 100 { 18 } else { 14 }
    } else {
        0
    };
    let date_w: u16 = if presentation.show_date_column && text_total >= 40 {
        if presentation.relative_dates { 12 } else { 17 }
    } else {
        0
    };
    let hash_w: u16 = if presentation.show_hash_column && text_total >= 50 {
        9
    } else {
        0
    };
    let sig_w: u16 = if presentation.show_signature_column && text_total >= 45 {
        3
    } else {
        0
    };
    let right_cols = author_w + date_w + hash_w + sig_w;
    let now = chrono::Local::now().timestamp();

    // Register row-level hit regions first (z=1) before graph elements & pills (z=2)
    for (vis_idx, row) in prepared_rows.iter().enumerate() {
        let row_idx = offset + vis_idx;
        let row_rect = Rect::new(
            table_area.x,
            table_area.y.saturating_add(vis_idx as u16),
            table_area.width,
            1,
        );
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
        let row_y = table_area.y.saturating_add(vis_idx as u16);
        let text_x = table_area.x.saturating_add(graph_cols);
        let text_w = table_area.right().saturating_sub(text_x);
        if text_w == 0 {
            continue;
        }
        let is_cursor = row_idx == state.log.graph.selected_idx;
        let is_sel = state.log.graph.is_row_selected(row_idx);
        let has_multi = state.log.graph.has_multi_selection();
        let is_head = snap.head_oid == Some(commit.oid);
        let is_my_commit =
            highlight_my_commits
                && (snap.user_email.as_deref().is_some_and(|e| {
                    !e.is_empty() && e.eq_ignore_ascii_case(&commit.author_email)
                }) || snap
                    .user_name
                    .as_deref()
                    .is_some_and(|n| !n.is_empty() && n == commit.author_name));
        let is_cherry_picked = state.log.is_commit_cherry_picked(snap, &commit.oid);
        let main_w = text_w.saturating_sub(right_cols);
        let main_rect = Rect::new(text_x, row_y, main_w, 1);

        frame.render_widget(
            Paragraph::new("").style(Style::default().bg(bg)),
            Rect::new(text_x, row_y, text_w, 1),
        );

        if has_multi && main_rect.width > 0 {
            let marker_rect = Rect::new(main_rect.x, row_y, 1, 1);
            if is_sel {
                let marker_fg = if is_cursor {
                    ctx.theme.fg_bright
                } else {
                    ctx.theme.fg_accent_link
                };
                frame.render_widget(
                    Paragraph::new(Span::styled("◆", Style::default().fg(marker_fg).bg(bg))),
                    marker_rect,
                );
            } else if is_cursor {
                frame.render_widget(
                    Paragraph::new(Span::styled(
                        "▸",
                        Style::default().fg(ctx.theme.fg_bright).bg(bg),
                    )),
                    marker_rect,
                );
            }
        }

        let head_marker = is_head.then(|| RefInfo::head_marker(commit.oid));
        let groups = group_refs_for_table(
            cached
                .refs_for_oid(snap, commit.oid)
                .chain(head_marker.as_ref()),
            GroupOptions {
                compact: state.log.presentation.compact_refs,
                show_tag_names: state.log.presentation.show_tag_names,
            },
        );

        let content_x = main_rect.x.saturating_add(1).min(main_rect.right());
        let content_w = main_rect.right().saturating_sub(content_x);
        let subject_area = if presentation.align_labels_right && !groups.is_empty() {
            let layout = layout_pills(&groups, ctx.glyph_mode.symbols(), content_w);
            let pills_w = layout.width.min(content_w);
            let pills_x = main_rect.right().saturating_sub(pills_w);
            if pills_w > 0 {
                PillBadgeBar {
                    groups: &groups,
                    glyphs: ctx.glyph_mode.symbols(),
                    theme: ctx.theme,
                    row_bg: bg,
                    visible_row: row_idx,
                }
                .render(
                    Rect::new(pills_x, row_y, pills_w, 1),
                    frame.buffer_mut(),
                    ctx.hit_registry,
                );
            }
            Rect::new(content_x, row_y, pills_x.saturating_sub(content_x), 1)
        } else {
            let pills_area = Rect::new(content_x, row_y, content_w, 1);
            let pills_w = PillBadgeBar {
                groups: &groups,
                glyphs: ctx.glyph_mode.symbols(),
                theme: ctx.theme,
                row_bg: bg,
                visible_row: row_idx,
            }
            .render(pills_area, frame.buffer_mut(), ctx.hit_registry);
            let subject_x = content_x.saturating_add(pills_w);
            Rect::new(
                subject_x,
                row_y,
                main_rect.right().saturating_sub(subject_x),
                1,
            )
        };

        let is_merge = commit.parents.len() > 1;
        let is_speed_hit = speed_search.is_row_match(row_idx);
        let base = Style::default()
            .fg(if is_cherry_picked && !is_sel {
                // Already Cherry-Picked highlighter dims picked commits
                ctx.theme.fg_muted
            } else if is_sel || is_head || is_my_commit {
                ctx.theme.fg_bright
            } else if is_merge && presentation.highlight_merges {
                // MergeCommitsHighlighter
                ctx.theme.fg_muted
            } else {
                ctx.theme.fg_primary
            })
            .bg(bg)
            .add_modifier(if is_head || (is_my_commit && !is_cherry_picked) {
                Modifier::BOLD
            } else {
                Modifier::empty()
            });
        let hl = Style::default()
            .fg(ctx.theme.ref_head.pill_fg)
            .bg(ctx.theme.ref_head.pill_bg)
            .add_modifier(Modifier::BOLD);
        let mut ranges = matcher
            .as_ref()
            .map(|m| m.find_ranges(&commit.subject))
            .unwrap_or_default();
        if speed_search.active {
            ranges.extend(speed_search.query_ranges(&commit.subject));
            ranges.sort_unstable();
        }
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
            let author_style = Style::default()
                .fg(if is_cherry_picked && !is_sel {
                    ctx.theme.fg_muted
                } else if is_sel || is_my_commit || is_speed_hit {
                    ctx.theme.fg_bright
                } else {
                    ctx.theme.fg_primary
                })
                .bg(bg)
                .add_modifier(if is_my_commit && !is_cherry_picked {
                    Modifier::BOLD
                } else {
                    Modifier::empty()
                });
            frame.render_widget(
                Paragraph::new(Span::styled(format!(" {author}"), author_style)),
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
                state.i18n.format_relative_time(ts, now)
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
            col_x += hash_w;
        }
        if sig_w > 0 {
            let sig_status = state
                .diff_caches
                .signatures
                .peek(&commit.oid)
                .map_or(jbgit_core::SignatureStatus::Pending, |s| s.status);
            let (icon_fg, _, _) = ctx.theme.signature_colors(sig_status);
            let sig_rect = Rect::new(col_x, row_y, sig_w, 1);
            frame.render_widget(
                Paragraph::new(Span::styled(
                    format!(" {} ", sig_status.icon()),
                    Style::default().fg(icon_fg).bg(bg),
                )),
                sig_rect,
            );
            ctx.hit_registry.register(
                sig_rect,
                2,
                HitTarget::CommitSignatureCell {
                    visible_row: row_idx,
                    commit_oid: commit.oid,
                },
            );
        }
    }

    if has_load_more && total >= offset && total < offset + visible_h {
        let load_more_y = table_area.y.saturating_add((total - offset) as u16);
        let is_cursor = state.log.graph.selected_idx == total;
        render_load_more_row(
            frame,
            Rect::new(table_area.x, load_more_y, table_area.width, 1),
            state,
            ctx,
            is_cursor,
            focused,
        );
    } else if snap.is_shallow && end == total && rows_len < visible_h {
        let boundary_y = table_area.y.saturating_add(rows_len as u16);
        let label = state.i18n.t("view.log.shallow_boundary");
        let btn = format!(" {} ", state.i18n.t("view.log.unshallow_btn"));
        let btn_w = btn.width() as u16;
        let line_w = table_area.width;
        let max_label_w = line_w.saturating_sub(btn_w.saturating_add(7)) as usize;
        let prefix = if max_label_w > 0 {
            format!("─── {} · ", super::truncate_to_width(label, max_label_w))
        } else {
            String::new()
        };
        let prefix_w = prefix.width() as u16;
        let muted = Style::default().fg(ctx.theme.fg_muted);
        let pill_style = Style::default()
            .fg(ctx.theme.ref_detached.pill_fg)
            .bg(ctx.theme.ref_detached.pill_bg)
            .add_modifier(Modifier::BOLD);
        let trailing_cols = line_w.saturating_sub(prefix_w.saturating_add(btn_w));
        let trailing = " ───"
            .chars()
            .take(trailing_cols as usize)
            .collect::<String>();
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(prefix, muted),
                Span::styled(btn, pill_style),
                Span::styled(trailing, muted),
            ])),
            Rect::new(table_area.x, boundary_y, line_w, 1),
        );
        let pill_x = table_area.x.saturating_add(prefix_w);
        if pill_x < table_area.right() && btn_w > 0 {
            let reg_w = btn_w.min(table_area.right() - pill_x);
            ctx.hit_registry.register(
                Rect::new(pill_x, boundary_y, reg_w, 1),
                2,
                HitTarget::LogShallowUnshallow,
            );
        }
    }

    if speed_bar_h > 0 {
        let bar_rect = Rect::new(area.x, area.bottom().saturating_sub(1), area.width, 1);
        let count_txt = if speed_search.query.trim().is_empty() {
            String::new()
        } else if speed_search.matches.is_empty() {
            format!("  {}", state.i18n.t("view.log.speed_search_no_match"))
        } else {
            let cur = speed_search.current.map_or(0, |i| i + 1);
            format!("  {}/{}", cur, speed_search.matches.len())
        };
        let cursor = if speed_search.editing { "▏" } else { "" };
        let line = Line::from(vec![
            Span::styled(
                format!(" {} ", state.i18n.t("view.log.speed_search_label")),
                Style::default()
                    .fg(ctx.theme.fg_accent_link)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("{}{cursor}", speed_search.query),
                Style::default().fg(ctx.theme.fg_bright),
            ),
            Span::styled(
                count_txt,
                Style::default().fg(
                    if speed_search.matches.is_empty() && !speed_search.query.is_empty() {
                        ctx.theme.status_conflicted
                    } else {
                        ctx.theme.status_added
                    },
                ),
            ),
            Span::styled(
                format!("  {}", state.i18n.t("view.log.speed_search_hint")),
                Style::default().fg(ctx.theme.fg_muted),
            ),
        ]);
        frame.render_widget(
            Paragraph::new(line).style(Style::default().bg(ctx.theme.bg_panel)),
            bar_rect,
        );
    }
}

fn hovered_expanded_segment(
    cached: &crate::graph::state::CachedLogGraph,
    hover_target: Option<&HitTarget>,
) -> Option<jbgit_core::graph::SegmentId> {
    if !cached.visible.can_collapse {
        return None;
    }
    match hover_target? {
        HitTarget::GraphPrintElement {
            visible_row,
            kind: GraphHitKind::Node { .. },
            ..
        } => cached.expanded_segment_of_row(*visible_row),
        HitTarget::GraphPrintElement {
            kind:
                GraphHitKind::Edge { up_row, down_row }
                | GraphHitKind::Arrow {
                    up_row, down_row, ..
                },
            ..
        } => cached.expanded_segment_at_edge(*up_row, *down_row),
        _ => None,
    }
}

fn format_thousands(n: usize) -> String {
    let digits = n.to_string();
    let len = digits.len();
    let mut out = String::with_capacity(len + len / 3);
    for (idx, ch) in digits.chars().enumerate() {
        if idx > 0 && (len - idx).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

fn render_load_more_row(
    frame: &mut Frame<'_>,
    row_rect: Rect,
    state: &AppState,
    ctx: &mut RenderContext<'_>,
    is_cursor: bool,
    focused: bool,
) {
    if row_rect.width == 0 || row_rect.height == 0 {
        return;
    }
    let is_hovered = matches!(ctx.hover_target, Some(HitTarget::LogLoadMore));
    let bg = row_background(
        is_cursor,
        focused,
        is_hovered,
        ctx.theme.bg_canvas,
        ctx.theme,
    );
    let count_s = format_thousands(state.commit_page_size.max(1));
    let btn_label = if state.loading_more_commits {
        state.i18n.t("view.log.loading_more_btn").to_string()
    } else {
        state
            .i18n
            .t_fmt("view.log.load_more_btn", &[("count", &count_s)])
    };
    let btn = format!("[{btn_label}]");
    let line_w = row_rect.width as usize;
    let display_btn = super::truncate_to_width(&btn, line_w);
    let pill_fg = if is_cursor {
        ctx.theme.fg_bright
    } else {
        ctx.theme.fg_accent_link
    };
    let muted = Style::default().fg(ctx.theme.fg_muted).bg(bg);
    let btn_style = Style::default()
        .fg(pill_fg)
        .bg(bg)
        .add_modifier(Modifier::BOLD);

    let prefix = if line_w >= display_btn.width() + 8 {
        "─── "
    } else {
        ""
    };
    let suffix = if line_w >= display_btn.width() + 8 {
        " ───"
    } else {
        ""
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(prefix, muted),
            Span::styled(display_btn, btn_style),
            Span::styled(suffix, muted),
        ]))
        .style(Style::default().bg(bg)),
        row_rect,
    );
    ctx.hit_registry
        .register(row_rect, 2, HitTarget::LogLoadMore);
}

fn is_element_highlighted(
    el: &GraphPrintElement,
    row_idx: usize,
    hover_target: Option<&HitTarget>,
    cached: &crate::graph::state::CachedLogGraph,
    hovered_expanded_seg: Option<jbgit_core::graph::SegmentId>,
) -> bool {
    // Highlight the entire expanded collapsible segment when hovering any of its nodes/edges.
    if let Some(seg_id) = hovered_expanded_seg {
        match el {
            GraphPrintElement::Node { .. }
                if cached.is_row_in_expanded_segment(row_idx, seg_id) =>
            {
                return true;
            }
            GraphPrintElement::Edge {
                up_row, down_row, ..
            }
            | GraphPrintElement::TerminalEdge {
                up_row, down_row, ..
            } if cached.expanded_segment_at_edge(*up_row as usize, *down_row as usize)
                == Some(seg_id) =>
            {
                return true;
            }
            _ => {}
        }
    }

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
                && (*direction == jbgit_core::graph::EdgeDirection::Down) == *is_down
        }
        _ => false,
    }
}
