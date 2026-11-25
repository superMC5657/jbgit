//! Dual-pane Git Reflog recovery modal (`git reflog show` + synced Changes & Diff preview).

use jbgit_core::{ChangeRowKind, ReflogActionKind, SplitAxis, SplitterId};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph};
use unicode_width::UnicodeWidthStr;

use crate::app::{AppState, RenderContext, ScrollPane};
use crate::hit_test::{HitTarget, ReflogToolbarButton};
use crate::log_view::{ReflogFocus, ReflogState};
use crate::views::log_details::{FileDiffRenderProps, render_file_diff_body};
use crate::views::overlays::draw_shadow;
use crate::views::{
    fit_left_right_spans, list_viewport, render_splitter_divider_z, render_toolbar,
    row_background, truncate_to_width,
};

fn action_badge_color(kind: ReflogActionKind, ctx: &RenderContext<'_>) -> Color {
    match kind {
        ReflogActionKind::Reset => ctx.theme.status_deleted,
        ReflogActionKind::Rebase => ctx.theme.status_modified,
        ReflogActionKind::Commit => ctx.theme.status_added,
        ReflogActionKind::Checkout => ctx.theme.fg_accent_link,
        ReflogActionKind::Merge | ReflogActionKind::CherryPick => ctx.theme.ref_remote.pill_fg,
        ReflogActionKind::Pull | ReflogActionKind::Branch | ReflogActionKind::Stash => {
            ctx.theme.ref_tag.pill_fg
        }
        ReflogActionKind::Other => ctx.theme.fg_muted,
    }
}

pub fn render_reflog_modal(
    frame: &mut Frame<'_>,
    screen: Rect,
    state: &AppState,
    rf: &ReflogState,
    ctx: &mut RenderContext<'_>,
    z: u8,
) {
    if screen.width < 24 || screen.height < 10 {
        return;
    }

    let width = if screen.width >= 40 {
        ((u32::from(screen.width) * 94) / 100).max(36) as u16
    } else {
        screen.width.saturating_sub(2).max(24)
    }
    .min(screen.width);
    let height = if screen.height >= 14 {
        ((u32::from(screen.height) * 90) / 100).max(12) as u16
    } else {
        screen.height.saturating_sub(2).max(10)
    }
    .min(screen.height);

    let rect = Rect::new(
        screen.x + screen.width.saturating_sub(width) / 2,
        screen.y + screen.height.saturating_sub(height) / 2,
        width,
        height,
    );

    draw_shadow(frame, screen, rect);
    crate::views::clear_modal_overlay(frame, rect);

    let count_s = match &rf.bundle {
        Some(b) if b.has_more => format!("{}+", b.entries.len()),
        Some(b) => b.entries.len().to_string(),
        None => "…".to_string(),
    };
    let title = ctx.t_fmt(
        "view.reflog.title",
        &[("ref", &rf.key.ref_name), ("count", &count_s)],
    );

    let block = Block::default()
        .title(Span::styled(
            truncate_to_width(&title, rect.width.saturating_sub(4) as usize),
            Style::default()
                .fg(ctx.theme.fg_bright)
                .add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(ctx.theme.border_focused))
        .style(Style::default().bg(ctx.theme.bg_elevated));

    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    ctx.hit_registry
        .register(rect, z + 1, HitTarget::DialogBody);

    if inner.height < 4 || inner.width < 12 {
        return;
    }

    let toolbar_rect = Rect::new(inner.x, inner.y, inner.width, 1);
    let footer_rect = Rect::new(inner.x, inner.bottom().saturating_sub(1), inner.width, 1);
    let content = Rect::new(
        inner.x,
        inner.y + 1,
        inner.width,
        inner.height.saturating_sub(2),
    );

    let mut buttons = vec![
        (
            ctx.t("view.reflog.btn_checkout").to_string(),
            false,
            ReflogToolbarButton::Checkout,
        ),
        (
            ctx.t("view.reflog.btn_new_branch").to_string(),
            false,
            ReflogToolbarButton::NewBranch,
        ),
        (
            ctx.t("view.reflog.btn_cherry_pick").to_string(),
            false,
            ReflogToolbarButton::CherryPick,
        ),
        (
            ctx.t("view.reflog.btn_reset").to_string(),
            false,
            ReflogToolbarButton::ResetHere,
        ),
        (
            ctx.t("view.reflog.btn_jump_log").to_string(),
            false,
            ReflogToolbarButton::JumpInLog,
        ),
    ];
    if rf.has_more() {
        buttons.push((
            ctx.t("view.reflog.btn_load_more").to_string(),
            false,
            ReflogToolbarButton::LoadMore,
        ));
    }
    buttons.push((
        if rf.diff.side_by_side {
            ctx.t("view.log_details.side_by_side_btn")
        } else {
            ctx.t("view.log_details.unified_btn")
        }
        .to_string(),
        rf.diff.side_by_side,
        ReflogToolbarButton::SideBySide,
    ));
    buttons.push((
        ctx.t_fmt(
            "view.log_details.ctx_btn",
            &[("ctx", ctx.diff_context_label(rf.diff.context))],
        ),
        false,
        ReflogToolbarButton::Context,
    ));

    render_toolbar(
        frame,
        toolbar_rect,
        ctx,
        z + 3,
        &buttons,
        HitTarget::ReflogToolbar,
    );

    if content.height >= 2 && content.width >= 10 {
        let (splitter_id, axis) = if content.width >= 96 {
            (SplitterId::ReflogHorizontal, SplitAxis::Horizontal)
        } else {
            (SplitterId::ReflogVertical, SplitAxis::Vertical)
        };
        let split = state.splitters.split(content, splitter_id, axis);
        ctx.record_splitter_container(splitter_id, axis, split.container);
        render_entries_pane(frame, split.first, state, rf, ctx, z + 2);
        if !split.collapsed {
            render_splitter_divider_z(frame, split.divider, splitter_id, axis, ctx, z + 3);
            render_right_pane(frame, split.second, state, rf, ctx, z + 2);
        }
    }

    frame.render_widget(
        Paragraph::new(Span::styled(
            truncate_to_width(
                ctx.t("view.reflog.footer_hint"),
                footer_rect.width as usize,
            ),
            Style::default().fg(ctx.theme.fg_muted),
        ))
        .style(Style::default().bg(ctx.theme.bg_panel)),
        footer_rect,
    );
}

fn render_entries_pane(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &AppState,
    rf: &ReflogState,
    ctx: &mut RenderContext<'_>,
    z: u8,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    ctx.hit_registry
        .register(area, z, HitTarget::ReflogListArea);

    let focused = rf.focus == ReflogFocus::Entries;
    let hdr_rect = Rect::new(area.x, area.y, area.width, 1);
    let hdr_bg = if focused {
        ctx.theme.bg_panel
    } else {
        ctx.theme.bg_canvas
    };
    let bar_fg = if focused {
        ctx.theme.border_focused
    } else {
        ctx.theme.fg_muted
    };
    let title_fg = if focused {
        ctx.theme.fg_bright
    } else {
        ctx.theme.fg_primary
    };
    let mut hdr_spans = vec![
        Span::styled(if focused { "▌" } else { " " }, Style::default().fg(bar_fg)),
        Span::styled(
            ctx.t("view.reflog.entries_header").to_string(),
            Style::default().fg(title_fg).add_modifier(Modifier::BOLD),
        ),
    ];
    if rf.loading {
        hdr_spans.push(Span::styled(
            format!("  {} {}", state.spinner_char(), ctx.t("common.loading")),
            Style::default().fg(ctx.theme.fg_accent_link),
        ));
    } else if rf.has_more() {
        hdr_spans.push(Span::styled(
            format!("  {}", ctx.t("view.reflog.more_hint")),
            Style::default().fg(ctx.theme.status_modified),
        ));
    }
    frame.render_widget(
        Paragraph::new(Line::from(hdr_spans)).style(Style::default().bg(hdr_bg)),
        hdr_rect,
    );

    let body = Rect::new(area.x, area.y + 1, area.width, area.height.saturating_sub(1));
    if body.height == 0 {
        return;
    }
    frame.render_widget(
        Paragraph::new("").style(Style::default().bg(ctx.theme.bg_canvas)),
        body,
    );

    if rf.loading && rf.bundle.is_none() {
        frame.render_widget(
            Paragraph::new(Span::styled(
                truncate_to_width(ctx.t("view.reflog.loading"), body.width as usize),
                Style::default().fg(ctx.theme.fg_muted),
            )),
            Rect::new(body.x, body.y, body.width, 1),
        );
        return;
    }
    if let Some(err) = &rf.error
        && rf.bundle.is_none()
    {
        frame.render_widget(
            Paragraph::new(Span::styled(
                truncate_to_width(&format!(" {err}"), body.width as usize),
                Style::default().fg(ctx.theme.status_conflicted),
            )),
            Rect::new(body.x, body.y, body.width, 1),
        );
        return;
    }

    let entries = rf.entries();
    if entries.is_empty() {
        frame.render_widget(
            Paragraph::new(Span::styled(
                truncate_to_width(ctx.t("view.reflog.empty"), body.width as usize),
                Style::default().fg(ctx.theme.fg_muted),
            )),
            Rect::new(body.x, body.y, body.width, 1),
        );
        return;
    }

    let now = chrono::Local::now().timestamp();
    let (selected, offset) = list_viewport(entries.len(), rf.selected, body.height as usize);
    for (vis_y, idx) in (offset..entries.len())
        .take(body.height as usize)
        .enumerate()
    {
        let entry = &entries[idx];
        let rect = Rect::new(body.x, body.y + vis_y as u16, body.width, 1);
        let row_target = HitTarget::ReflogRow { idx };
        let hovered = ctx.hover_target == Some(&row_target);
        let is_sel = idx == selected;
        let bg = row_background(is_sel, focused, hovered, ctx.theme.bg_canvas, ctx.theme);

        let badge_col = action_badge_color(entry.action, ctx);
        let short_oid = entry.oid.short();
        let rel_time = ctx.format_relative_time(entry.timestamp, now);
        let right = vec![Span::styled(
            format!(" {rel_time} "),
            Style::default().fg(ctx.theme.fg_muted),
        )];
        let left_prefix_w =
            entry.selector.width() + entry.action.badge().width() + short_oid.width() + 7;
        let msg_budget = (body.width as usize).saturating_sub(left_prefix_w + rel_time.width() + 3);
        let left = vec![
            Span::styled(
                format!(" {} ", entry.selector),
                Style::default()
                    .fg(ctx.theme.fg_accent_link)
                    .add_modifier(if is_sel { Modifier::BOLD } else { Modifier::empty() }),
            ),
            Span::styled(
                format!("[{}] ", entry.action.badge()),
                Style::default().fg(badge_col).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("{short_oid} "),
                Style::default().fg(ctx.theme.fg_muted),
            ),
            Span::styled(
                truncate_to_width(&entry.message, msg_budget),
                Style::default().fg(if is_sel {
                    ctx.theme.fg_bright
                } else {
                    ctx.theme.fg_primary
                }),
            ),
        ];

        let line = Line::from(fit_left_right_spans(left, right, body.width as usize, 0));
        frame.render_widget(Paragraph::new(line).style(Style::default().bg(bg)), rect);
        ctx.hit_registry.register(rect, z + 1, row_target);
    }
}

fn render_right_pane(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &AppState,
    rf: &ReflogState,
    ctx: &mut RenderContext<'_>,
    z: u8,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let split = state.splitters.split(
        area,
        SplitterId::ReflogRightVertical,
        SplitAxis::Vertical,
    );
    ctx.record_splitter_container(
        SplitterId::ReflogRightVertical,
        SplitAxis::Vertical,
        split.container,
    );
    render_changes_pane(frame, split.first, state, rf, ctx, z);
    if !split.collapsed {
        render_splitter_divider_z(
            frame,
            split.divider,
            SplitterId::ReflogRightVertical,
            SplitAxis::Vertical,
            ctx,
            z + 1,
        );
        render_diff_pane(frame, split.second, state, rf, ctx, z);
    }
}

fn render_changes_pane(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &AppState,
    rf: &ReflogState,
    ctx: &mut RenderContext<'_>,
    z: u8,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    ctx.hit_registry
        .register(area, z, HitTarget::ReflogChangesArea);

    let focused = rf.focus == ReflogFocus::Changes;
    let hdr_rect = Rect::new(area.x, area.y, area.width, 1);
    let hdr_bg = if focused {
        ctx.theme.bg_panel
    } else {
        ctx.theme.bg_canvas
    };
    let bar_fg = if focused {
        ctx.theme.border_focused
    } else {
        ctx.theme.fg_muted
    };
    let title_fg = if focused {
        ctx.theme.fg_bright
    } else {
        ctx.theme.fg_primary
    };
    let rows = rf.changes.change_rows(&state.diff_caches);
    let file_count = rf
        .changes
        .current_bundle(&state.diff_caches)
        .map_or(0, |b| b.changes.len());
    let hdr_text = if file_count > 0 {
        format!("{} ({file_count})", ctx.t("view.reflog.changes_header"))
    } else {
        ctx.t("view.reflog.changes_header").to_string()
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(if focused { "▌" } else { " " }, Style::default().fg(bar_fg)),
            Span::styled(
                hdr_text,
                Style::default().fg(title_fg).add_modifier(Modifier::BOLD),
            ),
        ]))
        .style(Style::default().bg(hdr_bg)),
        hdr_rect,
    );

    let body = Rect::new(area.x, area.y + 1, area.width, area.height.saturating_sub(1));
    if body.height == 0 {
        return;
    }
    frame.render_widget(
        Paragraph::new("").style(Style::default().bg(ctx.theme.bg_canvas)),
        body,
    );
    if rows.is_empty() {
        let msg = if rf.selected_entry().is_some() && rf.changes.current_bundle(&state.diff_caches).is_none() {
            ctx.t("common.loading")
        } else {
            ctx.t("view.log_details.no_changes")
        };
        frame.render_widget(
            Paragraph::new(Span::styled(
                format!(" {msg}"),
                Style::default().fg(ctx.theme.fg_muted),
            )),
            Rect::new(body.x, body.y, body.width, 1),
        );
        return;
    }

    let (selected, offset) = list_viewport(rows.len(), rf.changes.selected, body.height as usize);
    for (vis_y, idx) in (offset..rows.len())
        .take(body.height as usize)
        .enumerate()
    {
        let r = &rows[idx];
        let rect = Rect::new(body.x, body.y + vis_y as u16, body.width, 1);
        let row_target = HitTarget::ReflogChangeRow { idx };
        let hovered = ctx.hover_target == Some(&row_target);
        let is_sel = idx == selected;
        let bg = row_background(is_sel, focused, hovered, ctx.theme.bg_canvas, ctx.theme);
        let indent = "  ".repeat(r.depth as usize);
        let line = match r.kind {
            ChangeRowKind::Dir { file_count, .. } => Line::from(vec![
                Span::styled(format!(" {indent}"), Style::default().bg(bg)),
                Span::styled(
                    if r.expanded { "▾ " } else { "▸ " },
                    Style::default().fg(ctx.theme.fg_muted).bg(bg),
                ),
                Span::styled(
                    format!("{} ({file_count})", r.label),
                    Style::default().fg(ctx.theme.fg_primary).bg(bg),
                ),
            ]),
            ChangeRowKind::File { change_idx } => {
                let change = rf
                    .changes
                    .current_bundle(&state.diff_caches)
                    .and_then(|b| b.changes.get(change_idx));
                let status_col = change.map_or(ctx.theme.fg_primary, |c| ctx.theme.status_color(c.kind));
                let badge = change.map_or('M', |c| c.kind.badge_char());
                Line::from(vec![
                    Span::styled(format!(" {indent}"), Style::default().bg(bg)),
                    Span::styled(
                        format!("{badge} "),
                        Style::default()
                            .fg(status_col)
                            .bg(bg)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        truncate_to_width(
                            &r.label,
                            (body.width as usize).saturating_sub(indent.len() + 4),
                        ),
                        Style::default()
                            .fg(if is_sel {
                                ctx.theme.fg_bright
                            } else {
                                status_col
                            })
                            .bg(bg),
                    ),
                ])
            }
        };
        frame.render_widget(Paragraph::new(line).style(Style::default().bg(bg)), rect);
        ctx.hit_registry.register(rect, z + 1, row_target);
    }
}

fn render_diff_pane(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &AppState,
    rf: &ReflogState,
    ctx: &mut RenderContext<'_>,
    z: u8,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    ctx.hit_registry
        .register(area, z, HitTarget::ReflogDiffArea);

    let focused = rf.focus == ReflogFocus::Diff;
    let hdr_rect = Rect::new(area.x, area.y, area.width, 1);
    let hdr_bg = if focused {
        ctx.theme.bg_panel
    } else {
        ctx.theme.bg_canvas
    };
    let bar_fg = if focused {
        ctx.theme.border_focused
    } else {
        ctx.theme.fg_muted
    };
    let title_fg = if focused {
        ctx.theme.fg_bright
    } else {
        ctx.theme.fg_primary
    };
    let path_label = rf
        .selected_change(&state.diff_caches)
        .map(|c| c.path.as_str())
        .unwrap_or("");
    let hdr = if path_label.is_empty() {
        ctx.t("view.reflog.diff_header").to_string()
    } else {
        format!("{} · {}", ctx.t("view.reflog.diff_header"), path_label)
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(if focused { "▌" } else { " " }, Style::default().fg(bar_fg)),
            Span::styled(
                truncate_to_width(&hdr, area.width.saturating_sub(2) as usize),
                Style::default().fg(title_fg).add_modifier(Modifier::BOLD),
            ),
        ]))
        .style(Style::default().bg(hdr_bg)),
        hdr_rect,
    );

    let body = Rect::new(area.x, area.y + 1, area.width, area.height.saturating_sub(1));
    if body.height == 0 {
        return;
    }
    let h = body.height as usize;
    let Some(key) = rf.desired_diff_key(&state.diff_caches) else {
        ctx.record_scroll_bound(ScrollPane::ReflogDiff, h, 0);
        frame.render_widget(
            Paragraph::new(Span::styled(
                format!(" {}", ctx.t("view.log_details.select_file_diff")),
                Style::default().fg(ctx.theme.fg_muted),
            ))
            .style(Style::default().bg(ctx.theme.bg_canvas)),
            body,
        );
        return;
    };
    let Some(diff) = state.diff_caches.peek_diff(&key) else {
        ctx.record_scroll_bound(ScrollPane::ReflogDiff, h, 0);
        frame.render_widget(
            Paragraph::new(Span::styled(
                format!(" {}", ctx.t("view.log_details.loading_diff")),
                Style::default().fg(ctx.theme.fg_muted),
            ))
            .style(Style::default().bg(ctx.theme.bg_canvas)),
            body,
        );
        return;
    };
    let cached_syntax = state
        .diff_caches
        .peek_syntax(&key)
        .map(std::sync::Arc::as_ref);
    render_file_diff_body(
        frame,
        body,
        FileDiffRenderProps {
            diff,
            session: &rf.diff,
            cached_syntax,
            scroll_pane: ScrollPane::ReflogDiff,
        },
        ctx,
    );
}
