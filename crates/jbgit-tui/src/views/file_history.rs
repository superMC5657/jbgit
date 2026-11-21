//! Modal dual-split single-file history inspector (`--follow` rename tracking
//! + syntax-highlighted diff preview).

use jbgit_core::{FileStatusKind, SplitAxis, SplitterId};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};
use unicode_width::UnicodeWidthStr;

use crate::app::{AppState, RenderContext, ScrollPane};
use crate::hit_test::{FileHistoryToolbarButton, HitTarget};
use crate::log_view::{FileHistoryFocus, FileHistoryState};
use crate::views::log_details::{FileDiffRenderProps, render_file_diff_body};
use crate::views::overlays::draw_shadow;
use crate::views::{
    fit_left_right_spans, list_viewport, render_splitter_divider_z, render_toolbar,
    row_background, truncate_to_width,
};

pub fn render_file_history_modal(
    frame: &mut Frame<'_>,
    screen: Rect,
    state: &AppState,
    ctx: &mut RenderContext<'_>,
    z: u8,
) {
    if screen.width < 20 || screen.height < 10 {
        return;
    }
    let Some(fh) = &state.file_history else {
        return;
    };

    let width = if screen.width >= 36 {
        ((u32::from(screen.width) * 92) / 100).max(32) as u16
    } else {
        screen.width.saturating_sub(2).max(20)
    }
    .min(screen.width);
    let height = if screen.height >= 14 {
        ((u32::from(screen.height) * 88) / 100).max(12) as u16
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
    frame.render_widget(Clear, rect);

    let count_s = match &fh.bundle {
        Some(b) if b.truncated => format!("{}+", b.entries.len()),
        Some(b) => b.entries.len().to_string(),
        None => "…".to_string(),
    };
    let follow_tag = if fh.key.follow_renames {
        ctx.t("view.file_history.follow_badge")
    } else {
        ""
    };
    let title = ctx.t_fmt(
        "view.file_history.title",
        &[
            ("path", &fh.key.path),
            ("follow", follow_tag),
            ("count", &count_s),
        ],
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

    let buttons = vec![
        (
            ctx.t("view.file_history.btn_follow").to_string(),
            fh.key.follow_renames,
            FileHistoryToolbarButton::FollowRenames,
        ),
        (
            ctx.t("view.file_history.btn_compare_local").to_string(),
            fh.compare_with_local,
            FileHistoryToolbarButton::CompareLocal,
        ),
        (
            if fh.diff_side_by_side {
                ctx.t("view.log_details.side_by_side_btn")
            } else {
                ctx.t("view.log_details.unified_btn")
            }
            .to_string(),
            fh.diff_side_by_side,
            FileHistoryToolbarButton::SideBySide,
        ),
        (
            ctx.t_fmt(
                "view.log_details.ctx_btn",
                &[("ctx", ctx.diff_context_label(fh.diff_context))],
            ),
            false,
            FileHistoryToolbarButton::Context,
        ),
        (
            if fh.diff_zoomed {
                ctx.t("view.log_details.restore_btn")
            } else {
                ctx.t("view.log_details.zoom_btn")
            }
            .to_string(),
            fh.diff_zoomed,
            FileHistoryToolbarButton::Zoom,
        ),
        (
            ctx.t("view.file_history.btn_locate_log").to_string(),
            false,
            FileHistoryToolbarButton::LocateInLog,
        ),
    ];
    render_toolbar(
        frame,
        toolbar_rect,
        ctx,
        z + 3,
        &buttons,
        HitTarget::FileHistoryToolbar,
    );

    if content.height >= 2 && content.width >= 8 {
        if fh.diff_zoomed {
            render_diff_pane(frame, content, state, fh, ctx, z + 2);
        } else {
            let (splitter_id, axis) = if content.width >= 100 {
                (SplitterId::FileHistoryHorizontal, SplitAxis::Horizontal)
            } else {
                (SplitterId::FileHistoryVertical, SplitAxis::Vertical)
            };
            let split = state.splitters.split(content, splitter_id, axis);
            ctx.record_splitter_container(splitter_id, axis, split.container);
            render_list_pane(frame, split.first, state, fh, ctx, z + 2);
            if !split.collapsed {
                render_splitter_divider_z(frame, split.divider, splitter_id, axis, ctx, z + 3);
                render_diff_pane(frame, split.second, state, fh, ctx, z + 2);
            }
        }
    }

    frame.render_widget(
        Paragraph::new(Span::styled(
            truncate_to_width(
                ctx.t("view.file_history.footer_hint"),
                footer_rect.width as usize,
            ),
            Style::default().fg(ctx.theme.fg_muted),
        ))
        .style(Style::default().bg(ctx.theme.bg_panel)),
        footer_rect,
    );
}

fn render_list_pane(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &AppState,
    fh: &FileHistoryState,
    ctx: &mut RenderContext<'_>,
    z: u8,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    ctx.hit_registry
        .register(area, z, HitTarget::FileHistoryListArea);

    let focused = fh.focus == FileHistoryFocus::Commits;
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
            ctx.t("view.file_history.commits_header").to_string(),
            Style::default().fg(title_fg).add_modifier(Modifier::BOLD),
        ),
    ];
    if fh.loading {
        hdr_spans.push(Span::styled(
            format!("  {} {}", state.spinner_char(), ctx.t("common.loading")),
            Style::default().fg(ctx.theme.fg_accent_link),
        ));
    } else if fh.bundle.as_ref().is_some_and(|b| b.truncated) {
        hdr_spans.push(Span::styled(
            format!("  {}", ctx.t("view.file_history.truncated_hint")),
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

    if fh.loading && fh.bundle.is_none() {
        frame.render_widget(
            Paragraph::new(Span::styled(
                truncate_to_width(ctx.t("view.file_history.loading"), body.width as usize),
                Style::default().fg(ctx.theme.fg_muted),
            )),
            Rect::new(body.x, body.y, body.width, 1),
        );
        return;
    }
    if let Some(err) = &fh.error
        && fh.bundle.is_none()
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

    let entries = fh.entries();
    if entries.is_empty() {
        frame.render_widget(
            Paragraph::new(Span::styled(
                truncate_to_width(ctx.t("view.file_history.empty"), body.width as usize),
                Style::default().fg(ctx.theme.fg_muted),
            )),
            Rect::new(body.x, body.y, body.width, 1),
        );
        return;
    }

    let now = chrono::Local::now().timestamp();
    let (selected, offset) = list_viewport(entries.len(), fh.selected, body.height as usize);
    let range = (!fh.compare_with_local)
        .then_some(fh.anchor)
        .flatten()
        .filter(|&a| a != selected && a < entries.len())
        .map(|a| (selected.min(a), selected.max(a)));

    for (vis_y, idx) in (offset..entries.len())
        .take(body.height as usize)
        .enumerate()
    {
        let entry = &entries[idx];
        let rect = Rect::new(body.x, body.y + vis_y as u16, body.width, 1);
        let row_target = HitTarget::FileHistoryRow { idx };
        let hovered = ctx.hover_target == Some(&row_target);
        let is_sel = idx == selected;
        let in_range = range.is_some_and(|(lo, hi)| (lo..=hi).contains(&idx));
        let default_bg = if in_range {
            ctx.theme.bg_selection_unfocused
        } else {
            ctx.theme.bg_canvas
        };
        let bg = row_background(is_sel, focused, hovered, default_bg, ctx.theme);

        let status_color = ctx.theme.status_color(entry.kind);
        let compact_path = |p: &str| -> String {
            if p.width() > 28 {
                truncate_to_width(p.rsplit('/').next().unwrap_or(p), 28)
            } else {
                p.to_string()
            }
        };
        let mut left: Vec<Span<'static>> = vec![
            Span::styled(
                if in_range { "◆" } else { " " },
                Style::default().fg(ctx.theme.fg_accent_link),
            ),
            Span::styled(
                format!("{} ", entry.kind.badge_char()),
                Style::default()
                    .fg(status_color)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("{} ", entry.oid.short()),
                Style::default().fg(ctx.theme.fg_accent_link),
            ),
        ];

        if let Some(old) = &entry.old_path {
            left.push(Span::styled(
                format!("← {}  ", compact_path(old)),
                Style::default().fg(ctx.theme.status_color(FileStatusKind::Renamed)),
            ));
        } else if entry.path_at_commit != fh.key.path {
            left.push(Span::styled(
                format!("@ {}  ", compact_path(&entry.path_at_commit)),
                Style::default().fg(ctx.theme.status_color(FileStatusKind::Renamed)),
            ));
        }

        left.push(Span::styled(
            entry.summary.clone(),
            Style::default().fg(if is_sel {
                ctx.theme.fg_bright
            } else {
                ctx.theme.fg_primary
            }),
        ));

        let mut right: Vec<Span<'static>> = Vec::new();
        if entry.is_binary {
            right.push(Span::styled(
                ctx.t("view.log_details.bin_badge").to_string(),
                Style::default().fg(ctx.theme.fg_muted),
            ));
        } else if entry.additions > 0 || entry.deletions > 0 {
            right.push(Span::styled(
                format!("+{}", entry.additions),
                Style::default().fg(ctx.theme.status_added),
            ));
            right.push(Span::styled(
                format!(" -{}", entry.deletions),
                Style::default().fg(ctx.theme.status_conflicted),
            ));
        }
        if rect.width >= 44 {
            let max_author = if rect.width >= 60 { 12 } else { 10 };
            let author = truncate_to_width(&entry.author_name, max_author);
            if !right.is_empty() {
                right.push(Span::raw(" "));
            }
            right.push(Span::styled(
                author,
                Style::default().fg(ctx.theme.fg_muted),
            ));
        }
        if rect.width >= 36 {
            let rel = ctx.format_relative_time(entry.author_time, now);
            if !right.is_empty() {
                right.push(Span::raw(" "));
            }
            right.push(Span::styled(rel, Style::default().fg(ctx.theme.fg_muted)));
        }

        let spans = fit_left_right_spans(left, right, rect.width as usize, 1);
        frame.render_widget(
            Paragraph::new(Line::from(spans)).style(Style::default().bg(bg)),
            rect,
        );
        ctx.hit_registry.register(rect, z + 1, row_target);
    }
}

fn render_diff_pane(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &AppState,
    fh: &FileHistoryState,
    ctx: &mut RenderContext<'_>,
    z: u8,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    ctx.hit_registry
        .register(area, z, HitTarget::FileHistoryDiffArea);

    let focused = fh.focus == FileHistoryFocus::Diff || fh.diff_zoomed;
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

    let header_label = if let Some(cur) = fh.selected_entry() {
        if fh.compare_with_local {
            ctx.t_fmt(
                "view.file_history.diff_compare_local",
                &[("oid", &cur.oid.short()), ("path", &fh.key.path)],
            )
        } else if let Some((older, newer)) = fh.comparison_pair() {
            if older.path_at_commit != newer.path_at_commit {
                ctx.t_fmt(
                    "view.file_history.diff_compare_versions_renamed",
                    &[
                        ("older", &older.oid.short()),
                        ("newer", &newer.oid.short()),
                        ("old", &older.path_at_commit),
                        ("path", &newer.path_at_commit),
                    ],
                )
            } else {
                ctx.t_fmt(
                    "view.file_history.diff_compare_versions",
                    &[
                        ("older", &older.oid.short()),
                        ("newer", &newer.oid.short()),
                        ("path", &newer.path_at_commit),
                    ],
                )
            }
        } else if let Some(old) = &cur.old_path {
            ctx.t_fmt(
                "view.file_history.diff_renamed",
                &[
                    ("oid", &cur.oid.short()),
                    ("old", old),
                    ("path", &cur.path_at_commit),
                ],
            )
        } else {
            ctx.t_fmt(
                "view.file_history.diff_single",
                &[("oid", &cur.oid.short()), ("path", &cur.path_at_commit)],
            )
        }
    } else {
        ctx.t("view.log_details.diff_preview").to_string()
    };

    let hdr_spans = vec![
        Span::styled(if focused { "▌" } else { " " }, Style::default().fg(bar_fg)),
        Span::styled(
            truncate_to_width(&header_label, area.width.saturating_sub(2) as usize),
            Style::default().fg(if focused {
                ctx.theme.fg_bright
            } else {
                ctx.theme.fg_primary
            }),
        ),
    ];
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

    let h = body.height as usize;
    let Some(key) = fh.desired_diff_key() else {
        ctx.record_scroll_bound(ScrollPane::FileHistoryDiff, h, 0);
        let msg = if fh.loading {
            ctx.t("view.file_history.loading")
        } else if fh.entries().is_empty() {
            ctx.t("view.file_history.empty")
        } else {
            ctx.t("view.log_details.select_file_diff")
        };
        frame.render_widget(
            Paragraph::new(Span::styled(
                truncate_to_width(msg, body.width as usize),
                Style::default().fg(ctx.theme.fg_muted),
            )),
            Rect::new(body.x, body.y, body.width, 1),
        );
        return;
    };

    let Some(diff) = state.details.diff_cache.peek(&key) else {
        ctx.record_scroll_bound(ScrollPane::FileHistoryDiff, h, 0);
        let msg = if fh.selected_entry().is_some_and(|e| e.is_binary) {
            ctx.t("view.log_details.binary_diff")
        } else {
            ctx.t("view.log_details.loading_diff")
        };
        frame.render_widget(
            Paragraph::new(Span::styled(
                truncate_to_width(msg, body.width as usize),
                Style::default().fg(ctx.theme.fg_muted),
            )),
            Rect::new(body.x, body.y, body.width, 1),
        );
        return;
    };

    let cached_syntax = state
        .details
        .diff_syntax
        .peek(&key)
        .map(std::sync::Arc::as_ref);
    render_file_diff_body(
        frame,
        body,
        FileDiffRenderProps {
            diff,
            rows_cache: &fh.diff_rows_cache,
            cached_syntax,
            side_by_side: fh.diff_side_by_side,
            diff_scroll: fh.diff_scroll,
            scroll_pane: ScrollPane::FileHistoryDiff,
        },
        ctx,
    );
}
