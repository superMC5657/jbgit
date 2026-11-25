//! Modal dual-split single-file history inspector (`--follow` rename tracking,
//! branch scope, pagination, inline search, context actions, affected files,
//! repository version preview, and syntax-highlighted diff preview).

use jbgit_core::{
    DEFAULT_FILE_HISTORY_LIMIT, FileStatusKind, HistoryBranchScope, SplitAxis, SplitterId,
    TreeBlobContent,
};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph};
use unicode_width::UnicodeWidthStr;

use crate::app::{AppState, RenderContext, ScrollPane};
use crate::hit_test::{FileHistoryToolbarButton, HitTarget};
use crate::log_view::{FileHistoryFocus, FileHistoryState, format_absolute_time};
use crate::syntax::render_diff_line_spans_with_tab;
use crate::views::log_details::{FileDiffRenderProps, render_file_diff_body};
use crate::views::overlays::draw_shadow;
use crate::views::{
    fit_left_right_spans, list_viewport, render_splitter_divider_z, render_toolbar, row_background,
    truncate_to_width,
};

pub fn render_file_history_modal(
    frame: &mut Frame<'_>,
    screen: Rect,
    state: &AppState,
    fh: &FileHistoryState,
    ctx: &mut RenderContext<'_>,
    z: u8,
) {
    if screen.width < 20 || screen.height < 10 {
        return;
    }

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
    crate::views::clear_modal_overlay(frame, rect);

    let count_s = match &fh.bundle {
        Some(b) if b.truncated => format!("{}+", b.entries.len()),
        Some(b) => b.entries.len().to_string(),
        None => "…".to_string(),
    };
    let title = if let Some((start, end)) = fh.key.line_range {
        let range_path = format!("{}:{start}-{end}", fh.key.path);
        ctx.t_fmt(
            "view.file_history.selection_title",
            &[("path", &range_path), ("count", &count_s)],
        )
    } else {
        let follow_tag = if fh.key.effective_follow() {
            ctx.t("view.file_history.follow_badge")
        } else {
            ""
        };
        ctx.t_fmt(
            "view.file_history.title",
            &[
                ("path", &fh.key.path),
                ("follow", follow_tag),
                ("count", &count_s),
            ],
        )
    };

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

    let mut buttons = Vec::new();
    if !fh.is_selection_history() {
        let head_oid = state.repo_snapshot.as_deref().and_then(|s| s.head_oid);
        let pinned_start_oid = match &fh.key.scope {
            HistoryBranchScope::CurrentHead => {
                fh.key.start_oid.filter(|oid| head_oid != Some(*oid))
            }
            _ => None,
        };
        let scope_label = match &fh.key.scope {
            HistoryBranchScope::CurrentHead => match pinned_start_oid {
                Some(oid) => oid.short(),
                None => ctx.t("view.file_history.scope_head").to_string(),
            },
            HistoryBranchScope::AllBranches => ctx.t("view.file_history.scope_all").to_string(),
            HistoryBranchScope::Branch(b) => truncate_to_width(b, 16),
        };
        let scope_active =
            !matches!(fh.key.scope, HistoryBranchScope::CurrentHead) || pinned_start_oid.is_some();
        buttons.push((
            ctx.t_fmt("view.file_history.btn_scope", &[("scope", &scope_label)]),
            scope_active,
            FileHistoryToolbarButton::BranchScope,
        ));
        let follow_label = if matches!(fh.key.scope, HistoryBranchScope::AllBranches) {
            ctx.t("view.file_history.btn_follow_disabled").to_string()
        } else {
            ctx.t("view.file_history.btn_follow").to_string()
        };
        buttons.push((
            follow_label,
            fh.key.effective_follow(),
            FileHistoryToolbarButton::FollowRenames,
        ));
    }
    buttons.extend([
        (
            ctx.t("view.file_history.btn_compare_local").to_string(),
            fh.compare_with_local,
            FileHistoryToolbarButton::CompareLocal,
        ),
        (
            ctx.t("view.file_history.btn_details").to_string(),
            fh.show_commit_details,
            FileHistoryToolbarButton::CommitDetails,
        ),
        (
            ctx.t("view.file_history.btn_affected").to_string(),
            fh.show_affected_files,
            FileHistoryToolbarButton::AffectedFiles,
        ),
        (
            if fh.diff.side_by_side {
                ctx.t("view.log_details.side_by_side_btn")
            } else {
                ctx.t("view.log_details.unified_btn")
            }
            .to_string(),
            fh.diff.side_by_side,
            FileHistoryToolbarButton::SideBySide,
        ),
        (
            ctx.t_fmt(
                "view.log_details.ctx_btn",
                &[("ctx", ctx.diff_context_label(fh.diff.context))],
            ),
            false,
            FileHistoryToolbarButton::Context,
        ),
        (
            ctx.t_fmt(
                "view.log_details.ws_btn",
                &[("ws", ctx.diff_whitespace_label(fh.diff.whitespace))],
            ),
            fh.diff.whitespace != jbgit_core::WhitespaceMode::Default,
            FileHistoryToolbarButton::Whitespace,
        ),
        (
            ctx.t("view.log_details.wrap_btn").to_string(),
            fh.diff.soft_wrap,
            FileHistoryToolbarButton::SoftWrap,
        ),
        (
            if fh.diff.zoomed {
                ctx.t("view.log_details.restore_btn")
            } else {
                ctx.t("view.log_details.zoom_btn")
            }
            .to_string(),
            fh.diff.zoomed,
            FileHistoryToolbarButton::Zoom,
        ),
        (
            ctx.t("view.file_history.btn_annotate").to_string(),
            false,
            FileHistoryToolbarButton::Annotate,
        ),
        (
            ctx.t("view.file_history.btn_locate_log").to_string(),
            false,
            FileHistoryToolbarButton::LocateInLog,
        ),
    ]);
    render_toolbar(
        frame,
        toolbar_rect,
        ctx,
        z + 3,
        &buttons,
        HitTarget::FileHistoryToolbar,
    );

    if content.height >= 2 && content.width >= 8 {
        if fh.show_repo_version {
            render_repo_version_pane(frame, content, state, fh, ctx, z + 2);
        } else if fh.diff.zoomed {
            render_diff_pane(frame, content, state, fh, ctx, z + 2);
        } else {
            let (splitter_id, axis) = if content.width >= 100 {
                (SplitterId::FileHistoryHorizontal, SplitAxis::Horizontal)
            } else {
                (SplitterId::FileHistoryVertical, SplitAxis::Vertical)
            };
            let split = state.splitters.split(content, splitter_id, axis);
            ctx.record_splitter_container(splitter_id, axis, split.container);
            if fh.show_affected_files && split.first.height >= 8 {
                let parts = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([Constraint::Percentage(60), Constraint::Min(4)])
                    .split(split.first);
                render_list_pane(frame, parts[0], state, fh, ctx, z + 2);
                render_affected_files_pane(frame, parts[1], state, fh, ctx, z + 2);
            } else {
                render_list_pane(frame, split.first, state, fh, ctx, z + 2);
            }
            if !split.collapsed {
                render_splitter_divider_z(frame, split.divider, splitter_id, axis, ctx, z + 3);
                render_diff_pane(frame, split.second, state, fh, ctx, z + 2);
            }
        }
    }

    let hint = if fh.show_repo_version {
        ctx.t("view.file_history.footer_repo_version")
    } else if fh.is_selection_history() {
        ctx.t("view.file_history.selection_footer_hint")
    } else {
        ctx.t("view.file_history.footer_hint")
    };
    frame.render_widget(
        Paragraph::new(Span::styled(
            truncate_to_width(hint, footer_rect.width as usize),
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

    let focused = fh.focus == FileHistoryFocus::Commits && !fh.diff.zoomed;
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

    let mut body = Rect::new(
        area.x,
        area.y + 1,
        area.width,
        area.height.saturating_sub(1),
    );
    if body.height == 0 {
        return;
    }
    frame.render_widget(
        Paragraph::new("").style(Style::default().bg(ctx.theme.bg_canvas)),
        body,
    );

    if fh.search_active && body.height >= 2 {
        let search_rect = Rect::new(body.x, body.y, body.width, 1);
        let cursor = if fh.search_editing { "█" } else { "" };
        let count_tag = format!(" ({}/{})", fh.visible_len(), fh.entries().len());
        let search_line = format!("/ {}{}{}", fh.search_query, cursor, count_tag);
        frame.render_widget(
            Paragraph::new(Span::styled(
                truncate_to_width(&search_line, search_rect.width as usize),
                Style::default()
                    .fg(if fh.search_editing {
                        ctx.theme.fg_bright
                    } else {
                        ctx.theme.fg_accent_link
                    })
                    .bg(ctx.theme.bg_panel),
            ))
            .style(Style::default().bg(ctx.theme.bg_panel)),
            search_rect,
        );
        body = Rect::new(
            body.x,
            body.y + 1,
            body.width,
            body.height.saturating_sub(1),
        );
    }

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

    let vis_len = fh.visible_len();
    if vis_len == 0 {
        let msg = if !fh.search_query.is_empty() {
            ctx.t("view.file_history.no_search_matches")
        } else {
            ctx.t("view.file_history.empty")
        };
        frame.render_widget(
            Paragraph::new(Span::styled(
                truncate_to_width(msg, body.width as usize),
                Style::default().fg(ctx.theme.fg_muted),
            )),
            Rect::new(body.x, body.y, body.width, 1),
        );
        return;
    }

    let show_load_more = fh.has_load_more_row();
    let total_rows = vis_len + usize::from(show_load_more);
    let now = chrono::Local::now().timestamp();
    let (selected, offset) = list_viewport(
        total_rows,
        fh.selected.min(total_rows.saturating_sub(1)),
        body.height as usize,
    );
    let range = (!fh.compare_with_local)
        .then_some(fh.anchor)
        .flatten()
        .filter(|&a| a != selected && a < vis_len && selected < vis_len)
        .map(|a| (selected.min(a), selected.max(a)));

    for (vis_y, idx) in (offset..total_rows).take(body.height as usize).enumerate() {
        let rect = Rect::new(body.x, body.y + vis_y as u16, body.width, 1);
        if show_load_more && idx == vis_len {
            let hovered = ctx.hover_target == Some(&HitTarget::FileHistoryLoadMore);
            let is_sel = idx == selected;
            let bg = row_background(is_sel, focused, hovered, ctx.theme.bg_panel, ctx.theme);
            let label = ctx.t_fmt(
                "view.file_history.load_more",
                &[("step", &DEFAULT_FILE_HISTORY_LIMIT.to_string())],
            );
            frame.render_widget(
                Paragraph::new(Span::styled(
                    truncate_to_width(&label, rect.width as usize),
                    Style::default()
                        .fg(ctx.theme.fg_accent_link)
                        .add_modifier(Modifier::BOLD),
                ))
                .style(Style::default().bg(bg)),
                rect,
            );
            ctx.hit_registry
                .register(rect, z + 1, HitTarget::FileHistoryLoadMore);
            continue;
        }

        let Some(entry) = fh.visible_entry(idx) else {
            continue;
        };
        let row_target = HitTarget::FileHistoryRow { idx };
        let hovered = ctx.hover_target == Some(&row_target);
        let is_sel = idx == selected;
        let is_pinned_anchor = fh.anchor == Some(idx) && fh.anchor_pinned;
        let in_range = range.is_some_and(|(lo, hi)| (lo..=hi).contains(&idx));
        let default_bg = if in_range || is_pinned_anchor {
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
        let marker = if is_pinned_anchor {
            "★"
        } else if in_range {
            "◆"
        } else {
            " "
        };
        let mut left: Vec<Span<'static>> = vec![
            Span::styled(
                marker,
                Style::default().fg(if is_pinned_anchor {
                    ctx.theme.status_modified
                } else {
                    ctx.theme.fg_accent_link
                }),
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

fn render_affected_files_pane(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &AppState,
    fh: &FileHistoryState,
    ctx: &mut RenderContext<'_>,
    z: u8,
) {
    if area.height < 2 || area.width < 8 {
        return;
    }
    let focused = fh.focus == FileHistoryFocus::AffectedFiles;
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

    let bundle = fh
        .desired_details_key()
        .and_then(|k| state.diff_caches.peek_details(&k));
    let count_s = bundle.map_or_else(|| "…".to_string(), |b| b.changes.len().to_string());
    let title = ctx.t_fmt("view.file_history.affected_header", &[("count", &count_s)]);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(if focused { "▌" } else { " " }, Style::default().fg(bar_fg)),
            Span::styled(
                truncate_to_width(&title, area.width.saturating_sub(2) as usize),
                Style::default()
                    .fg(if focused {
                        ctx.theme.fg_bright
                    } else {
                        ctx.theme.fg_primary
                    })
                    .add_modifier(Modifier::BOLD),
            ),
        ]))
        .style(Style::default().bg(hdr_bg)),
        hdr_rect,
    );

    let body = Rect::new(
        area.x,
        area.y + 1,
        area.width,
        area.height.saturating_sub(1),
    );
    frame.render_widget(
        Paragraph::new("").style(Style::default().bg(ctx.theme.bg_canvas)),
        body,
    );

    let Some(bundle) = bundle else {
        frame.render_widget(
            Paragraph::new(Span::styled(
                truncate_to_width(ctx.t("common.loading"), body.width as usize),
                Style::default().fg(ctx.theme.fg_muted),
            )),
            Rect::new(body.x, body.y, body.width, 1),
        );
        return;
    };

    if bundle.changes.is_empty() {
        return;
    }

    let (sel, offset) = list_viewport(
        bundle.changes.len(),
        fh.affected_selected.min(bundle.changes.len() - 1),
        body.height as usize,
    );
    for (vis_y, idx) in (offset..bundle.changes.len())
        .take(body.height as usize)
        .enumerate()
    {
        let ch = &bundle.changes[idx];
        let row_rect = Rect::new(body.x, body.y + vis_y as u16, body.width, 1);
        let target = HitTarget::FileHistoryAffectedFileRow { idx };
        let hovered = ctx.hover_target == Some(&target);
        let bg = row_background(idx == sel, focused, hovered, ctx.theme.bg_canvas, ctx.theme);
        let status_color = ctx.theme.status_color(ch.kind);
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(
                    format!(" {} ", ch.kind.badge_char()),
                    Style::default()
                        .fg(status_color)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    truncate_to_width(&ch.path, body.width.saturating_sub(4) as usize),
                    Style::default().fg(if idx == sel {
                        ctx.theme.fg_bright
                    } else {
                        ctx.theme.fg_primary
                    }),
                ),
            ]))
            .style(Style::default().bg(bg)),
            row_rect,
        );
        ctx.hit_registry.register(row_rect, z + 1, target);
    }
}

fn render_repo_version_pane(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &AppState,
    fh: &FileHistoryState,
    ctx: &mut RenderContext<'_>,
    z: u8,
) {
    if area.height < 2 || area.width < 10 {
        return;
    }
    ctx.hit_registry
        .register(area, z, HitTarget::FileHistoryBlobArea);

    let hdr_rect = Rect::new(area.x, area.y, area.width, 1);
    let hdr_label = match &fh.repo_version_requested {
        Some(k) => ctx.t_fmt(
            "view.file_history.repo_version_header",
            &[("oid", &k.oid.short()), ("path", &k.path)],
        ),
        None => ctx.t("menu.file_history.open_repo_version").to_string(),
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("▌", Style::default().fg(ctx.theme.border_focused)),
            Span::styled(
                truncate_to_width(&hdr_label, area.width.saturating_sub(2) as usize),
                Style::default()
                    .fg(ctx.theme.fg_bright)
                    .add_modifier(Modifier::BOLD),
            ),
        ]))
        .style(Style::default().bg(ctx.theme.bg_panel)),
        hdr_rect,
    );

    let body = Rect::new(
        area.x,
        area.y + 1,
        area.width,
        area.height.saturating_sub(1),
    );
    frame.render_widget(
        Paragraph::new("").style(Style::default().bg(ctx.theme.bg_canvas)),
        body,
    );
    let h = body.height as usize;

    let Some(blob) = fh.repo_version_blob.as_deref() else {
        ctx.record_scroll_bound(ScrollPane::FileHistoryBlob, h, 0);
        frame.render_widget(
            Paragraph::new(Span::styled(
                truncate_to_width(ctx.t("common.loading"), body.width as usize),
                Style::default().fg(ctx.theme.fg_muted),
            )),
            Rect::new(body.x, body.y, body.width, 1),
        );
        return;
    };

    match &blob.content {
        TreeBlobContent::Binary => {
            ctx.record_scroll_bound(ScrollPane::FileHistoryBlob, h, 0);
            let msg = ctx.t_fmt("view.tree.binary", &[("size", &blob.size.to_string())]);
            frame.render_widget(
                Paragraph::new(Span::styled(
                    truncate_to_width(&msg, body.width as usize),
                    Style::default().fg(ctx.theme.fg_muted),
                )),
                Rect::new(body.x, body.y, body.width, 1),
            );
        }
        TreeBlobContent::Lfs(ptr) => {
            ctx.record_scroll_bound(ScrollPane::FileHistoryBlob, h, 0);
            let oid_short = truncate_to_width(&ptr.oid_sha256, 12);
            let size_s = jbgit_core::format_byte_size(ptr.size);
            let msg = ctx.t_fmt(
                "view.tree.lfs_preview",
                &[("oid", &oid_short), ("size", &size_s)],
            );
            frame.render_widget(
                Paragraph::new(Span::styled(
                    truncate_to_width(&msg, body.width as usize),
                    Style::default().fg(ctx.theme.fg_accent_link),
                )),
                Rect::new(body.x, body.y, body.width, 1),
            );
        }
        TreeBlobContent::TooLarge { limit, .. } => {
            ctx.record_scroll_bound(ScrollPane::FileHistoryBlob, h, 0);
            let mib = (limit / (1024 * 1024)).max(1).to_string();
            let msg = ctx.t_fmt("view.tree.too_large", &[("mib", &mib)]);
            frame.render_widget(
                Paragraph::new(Span::styled(
                    truncate_to_width(&msg, body.width as usize),
                    Style::default().fg(ctx.theme.status_modified),
                )),
                Rect::new(body.x, body.y, body.width, 1),
            );
        }
        TreeBlobContent::Missing => {
            ctx.record_scroll_bound(ScrollPane::FileHistoryBlob, h, 0);
            frame.render_widget(
                Paragraph::new(Span::styled(
                    truncate_to_width(ctx.t("view.tree.missing"), body.width as usize),
                    Style::default().fg(ctx.theme.status_deleted),
                )),
                Rect::new(body.x, body.y, body.width, 1),
            );
        }
        TreeBlobContent::Symlink(target) => {
            ctx.record_scroll_bound(ScrollPane::FileHistoryBlob, h, 0);
            let msg = ctx.t_fmt("view.tree.symlink_preview", &[("target", target)]);
            frame.render_widget(
                Paragraph::new(Span::styled(
                    truncate_to_width(&msg, body.width as usize),
                    Style::default().fg(ctx.theme.fg_accent_link),
                )),
                Rect::new(body.x, body.y, body.width, 1),
            );
        }
        TreeBlobContent::Text(text) => {
            let lines: Vec<&str> = text.lines().collect();
            let max_scroll = lines.len().saturating_sub(h);
            ctx.record_scroll_bound(ScrollPane::FileHistoryBlob, h, max_scroll);
            let offset = fh.repo_version_scroll.offset().min(max_scroll);
            let lineno_w = lines.len().max(1).to_string().len().max(3);
            let tab_size = state.config.jbgit.diff.tab_size;
            let code_w = (body.width as usize).saturating_sub(lineno_w + 4);
            let bg = ctx.theme.bg_canvas;

            for (vis_y, row_idx) in (offset..lines.len()).take(h).enumerate() {
                let mut spans = vec![Span::styled(
                    format!(" {:>lineno_w$} │ ", row_idx + 1),
                    Style::default().fg(ctx.theme.fg_muted).bg(bg),
                )];
                let tokens = fh
                    .repo_version_syntax
                    .as_ref()
                    .and_then(|s| s.lines.get(row_idx))
                    .map(Vec::as_slice)
                    .unwrap_or(&[]);
                spans.extend(render_diff_line_spans_with_tab(
                    lines[row_idx],
                    tokens,
                    &[],
                    code_w,
                    tab_size,
                    Style::default().fg(ctx.theme.fg_primary).bg(bg),
                    bg,
                    ctx.theme.true_color,
                ));
                frame.render_widget(
                    Paragraph::new(Line::from(spans)).style(Style::default().bg(bg)),
                    Rect::new(body.x, body.y + vis_y as u16, body.width, 1),
                );
            }
        }
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

    let focused = fh.focus == FileHistoryFocus::Diff || fh.diff.zoomed;
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

    let mut header_label = if let Some(cur) = fh.selected_entry() {
        if fh.show_affected_files
            && let Some(ch) = fh.selected_affected_file(&state.diff_caches)
        {
            ctx.t_fmt(
                "view.file_history.diff_single",
                &[("oid", &cur.oid.short()), ("path", &ch.path)],
            )
        } else if fh.compare_with_local {
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
        } else if fh.uses_range_patch()
            && let Some((start, end)) = fh.key.line_range
        {
            ctx.t_fmt(
                "view.file_history.diff_range_patch",
                &[
                    ("badge", ctx.t("view.file_history.range_patch_badge")),
                    ("oid", &cur.oid.short()),
                    ("path", &cur.path_at_commit),
                    ("start", &start.to_string()),
                    ("end", &end.to_string()),
                ],
            )
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
    if !fh.diff.soft_wrap && fh.diff.horiz_scroll > 0 {
        header_label.push_str(&format!("  Col +{}", fh.diff.horiz_scroll));
    }

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

    let mut body = Rect::new(
        area.x,
        area.y + 1,
        area.width,
        area.height.saturating_sub(1),
    );
    if body.height == 0 {
        return;
    }

    if fh.show_commit_details && body.height >= 5 {
        let mut detail_lines: Vec<Line<'static>> = Vec::new();
        let max_w = body.width.saturating_sub(2) as usize;
        if let Some(det_key) = fh.desired_details_key()
            && let Some(bundle) = state.diff_caches.peek_details(&det_key)
            && let Some(det) = bundle.details.first()
        {
            detail_lines.push(Line::from(Span::styled(
                truncate_to_width(
                    &format!(
                        "{}: {} <{}>  {}",
                        ctx.t("view.annotate.author_label"),
                        det.author.name,
                        det.author.email,
                        format_absolute_time(det.author.time, true),
                    ),
                    max_w,
                ),
                Style::default().fg(ctx.theme.fg_accent_link),
            )));
            if !det.same_author_and_committer() {
                detail_lines.push(Line::from(Span::styled(
                    truncate_to_width(
                        &format!(
                            "{}: {} <{}>  {}",
                            ctx.t("view.annotate.committer_label"),
                            det.committer.name,
                            det.committer.email,
                            format_absolute_time(det.committer.time, true),
                        ),
                        max_w,
                    ),
                    Style::default().fg(ctx.theme.fg_muted),
                )));
            }
            for ln in det.message.lines().take(3) {
                detail_lines.push(Line::from(Span::styled(
                    truncate_to_width(ln, max_w),
                    Style::default().fg(ctx.theme.fg_primary),
                )));
            }
        } else {
            detail_lines.push(Line::from(Span::styled(
                truncate_to_width(ctx.t("common.loading"), max_w),
                Style::default().fg(ctx.theme.fg_muted),
            )));
        }
        let strip_h = (detail_lines.len() as u16)
            .min(5)
            .min(body.height.saturating_sub(2));
        if strip_h > 0 {
            let strip_rect = Rect::new(body.x, body.y, body.width, strip_h);
            frame.render_widget(
                Paragraph::new(detail_lines).style(Style::default().bg(ctx.theme.bg_panel)),
                strip_rect,
            );
            body = Rect::new(
                body.x,
                body.y + strip_h,
                body.width,
                body.height.saturating_sub(strip_h),
            );
        }
    }

    frame.render_widget(
        Paragraph::new("").style(Style::default().bg(ctx.theme.bg_canvas)),
        body,
    );

    let h = body.height as usize;
    if fh.uses_range_patch() {
        let Some(diff) = fh.current_range_diff() else {
            ctx.record_scroll_bound(ScrollPane::FileHistoryDiff, h, 0);
            let msg = if fh.loading {
                ctx.t("view.file_history.loading")
            } else if fh.entries().is_empty() {
                ctx.t("view.file_history.empty")
            } else {
                ctx.t("view.file_history.no_range_patch")
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
        let cached_syntax = fh.current_range_syntax().map(std::sync::Arc::as_ref);
        render_file_diff_body(
            frame,
            body,
            FileDiffRenderProps {
                diff,
                session: &fh.diff,
                cached_syntax,
                scroll_pane: ScrollPane::FileHistoryDiff,
            },
            ctx,
        );
        return;
    }

    let Some(key) = fh.desired_diff_key_with_store(&state.diff_caches) else {
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

    let Some(diff) = state.diff_caches.peek_diff(&key) else {
        ctx.record_scroll_bound(ScrollPane::FileHistoryDiff, h, 0);
        let msg = if !fh.show_affected_files && fh.selected_entry().is_some_and(|e| e.is_binary) {
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
        .diff_caches
        .peek_syntax(&key)
        .map(std::sync::Arc::as_ref);
    render_file_diff_body(
        frame,
        body,
        FileDiffRenderProps {
            diff,
            session: &fh.diff,
            cached_syntax,
            scroll_pane: ScrollPane::FileHistoryDiff,
        },
        ctx,
    );
}
