//! Modal full-file Git Blame / Annotate view with age-heatmap gutter,
//! syntax highlighting, visual line selection, and optional inline diff preview.

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph};
use unicode_width::UnicodeWidthStr;

use crate::app::{AppState, RenderContext, ScrollPane};
use crate::hit_test::{AnnotateToolbarButton, HitTarget};
use crate::log_view::{
    AnnotateColorMode, AnnotateError, AnnotateFocus, AnnotateState, format_absolute_time,
};
use crate::syntax::render_diff_line_spans_with_tab;
use crate::views::log_details::{FileDiffRenderProps, render_file_diff_body};
use crate::views::overlays::draw_shadow;
use crate::views::{render_toolbar, truncate_to_width};

pub fn render_annotate_modal(
    frame: &mut Frame<'_>,
    screen: Rect,
    state: &AppState,
    ann: &AnnotateState,
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

    let rev_label = ann
        .key
        .rev
        .map(|o| o.short())
        .unwrap_or_else(|| ctx.t("view.annotate.workdir").to_string());
    let lines_s = ann
        .bundle
        .as_ref()
        .map_or_else(|| "…".to_string(), |b| b.lines.len().to_string());
    let mut title = ctx.t_fmt(
        "view.annotate.title",
        &[
            ("path", &ann.key.path),
            ("rev", &rev_label),
            ("lines", &lines_s),
        ],
    );
    if ann.key.options.ignore_whitespace {
        title.push_str(" [W]");
    }
    if ann.key.options.move_detection.is_enabled() {
        title.push_str(&format!(" [{}]", ann.key.options.move_detection.badge()));
    }
    if ann.key.options.prefer_commit_date {
        title.push_str(&format!(" [{}]", ctx.t("view.annotate.badge_commit_date")));
    }
    if let Some((lo, hi)) = ann.selected_line_range()
        && ann.visual_anchor.is_some()
    {
        title.push_str(&format!(" [L{lo}-L{hi}]"));
    }

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

    if inner.height < 6 || inner.width < 16 {
        return;
    }

    let toolbar_rect = Rect::new(inner.x, inner.y, inner.width, 1);
    let footer_rect = Rect::new(inner.x, inner.bottom().saturating_sub(1), inner.width, 1);
    let detail_h = if inner.height >= 8 { 2u16 } else { 0u16 };
    let detail_rect = Rect::new(
        inner.x,
        footer_rect.y.saturating_sub(detail_h),
        inner.width,
        detail_h,
    );
    let content_h = detail_rect.y.saturating_sub(inner.y + 1);
    let content = Rect::new(inner.x, inner.y + 1, inner.width, content_h);

    let back_label = if ann.nav_back.is_empty() {
        ctx.t("view.annotate.btn_back").to_string()
    } else {
        ctx.t_fmt(
            "view.annotate.btn_back_count",
            &[("count", &ann.nav_back.len().to_string())],
        )
    };
    let moves_label = format!(
        "{}:{}",
        ctx.t("view.annotate.btn_moves"),
        ann.key.options.move_detection.badge()
    );
    let date_label = if ann.key.options.prefer_commit_date {
        ctx.t("view.annotate.btn_date_commit").to_string()
    } else {
        ctx.t("view.annotate.btn_date_author").to_string()
    };
    let color_label = match ann.color_mode {
        AnnotateColorMode::Age => ctx.t("view.annotate.btn_color_age").to_string(),
        AnnotateColorMode::Author => ctx.t("view.annotate.btn_color_author").to_string(),
        AnnotateColorMode::None => ctx.t("view.annotate.btn_color_none").to_string(),
    };
    let buttons = vec![
        (
            ctx.t("view.annotate.btn_whitespace").to_string(),
            ann.key.options.ignore_whitespace,
            AnnotateToolbarButton::IgnoreWhitespace,
        ),
        (
            moves_label,
            ann.key.options.move_detection.is_enabled(),
            AnnotateToolbarButton::DetectMoves,
        ),
        (
            date_label,
            ann.key.options.prefer_commit_date,
            AnnotateToolbarButton::DateMode,
        ),
        (
            color_label,
            ann.color_mode != AnnotateColorMode::None,
            AnnotateToolbarButton::ColorMode,
        ),
        (
            ctx.t("view.annotate.btn_prev_rev").to_string(),
            false,
            AnnotateToolbarButton::PreviousRev,
        ),
        (
            back_label,
            !ann.nav_back.is_empty(),
            AnnotateToolbarButton::NavBack,
        ),
        (
            ctx.t("view.annotate.btn_diff").to_string(),
            ann.show_diff,
            AnnotateToolbarButton::ShowDiff,
        ),
        (
            ctx.t("view.annotate.btn_details").to_string(),
            ann.show_commit_details,
            AnnotateToolbarButton::CommitDetails,
        ),
        (
            ctx.t("view.annotate.btn_selection_history").to_string(),
            ann.visual_anchor.is_some(),
            AnnotateToolbarButton::SelectionHistory,
        ),
        (
            ctx.t("view.file_history.btn_locate_log").to_string(),
            false,
            AnnotateToolbarButton::LocateInLog,
        ),
    ];
    render_toolbar(
        frame,
        toolbar_rect,
        ctx,
        z + 3,
        &buttons,
        HitTarget::AnnotateToolbar,
    );

    if content.height >= 2 && content.width >= 12 {
        if ann.show_commit_details && content.height >= 10 {
            let parts = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Percentage(58), Constraint::Min(5)])
                .split(content);
            render_code_pane(frame, parts[0], ann, ctx, z + 2);
            render_commit_details_pane(frame, parts[1], state, ann, ctx, z + 2);
        } else if ann.show_diff && content.height >= 10 {
            let parts = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Percentage(62), Constraint::Min(4)])
                .split(content);
            render_code_pane(frame, parts[0], ann, ctx, z + 2);
            render_inline_diff_pane(frame, parts[1], state, ann, ctx, z + 2);
        } else {
            render_code_pane(frame, content, ann, ctx, z + 2);
        }
    }

    if detail_h > 0 {
        render_detail_strip(frame, detail_rect, ann, ctx, z + 2);
    }

    let hint = if ann.visual_anchor.is_some() {
        ctx.t("view.annotate.footer_hint_visual")
    } else {
        ctx.t("view.annotate.footer_hint")
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

fn render_code_pane(
    frame: &mut Frame<'_>,
    area: Rect,
    ann: &AnnotateState,
    ctx: &mut RenderContext<'_>,
    z: u8,
) {
    frame.render_widget(
        Paragraph::new("").style(Style::default().bg(ctx.theme.bg_canvas)),
        area,
    );
    ctx.hit_registry.register(area, z, HitTarget::AnnotateArea);

    let vh = area.height as usize;
    if let Some(err) = &ann.error {
        ctx.record_scroll_bound(ScrollPane::Annotate, vh, 0);
        let msg = match err {
            AnnotateError::TooLarge { limit } => {
                let mib = (limit / (1024 * 1024)).max(1).to_string();
                ctx.t_fmt("view.annotate.err_too_large", &[("mib", &mib)])
            }
            AnnotateError::Binary => ctx.t("view.annotate.err_binary").to_string(),
            AnnotateError::Message(m) => ctx.t_fmt("view.annotate.err_message", &[("msg", m)]),
        };
        frame.render_widget(
            Paragraph::new(Span::styled(
                truncate_to_width(&msg, area.width as usize),
                Style::default().fg(ctx.theme.status_deleted),
            )),
            Rect::new(area.x, area.y, area.width, 1),
        );
        return;
    }

    let Some(bundle) = &ann.bundle else {
        ctx.record_scroll_bound(ScrollPane::Annotate, vh, 0);
        let msg = if ann.loading {
            ctx.t("view.annotate.loading")
        } else {
            ctx.t("view.annotate.empty")
        };
        frame.render_widget(
            Paragraph::new(Span::styled(
                truncate_to_width(msg, area.width as usize),
                Style::default().fg(ctx.theme.fg_muted),
            )),
            Rect::new(area.x, area.y, area.width, 1),
        );
        return;
    };

    if bundle.lines.is_empty() {
        ctx.record_scroll_bound(ScrollPane::Annotate, vh, 0);
        frame.render_widget(
            Paragraph::new(Span::styled(
                truncate_to_width(ctx.t("view.annotate.empty"), area.width as usize),
                Style::default().fg(ctx.theme.fg_muted),
            )),
            Rect::new(area.x, area.y, area.width, 1),
        );
        return;
    }

    let len = bundle.lines.len();
    let max_scroll = len.saturating_sub(vh);
    ctx.record_scroll_bound(ScrollPane::Annotate, vh, max_scroll);
    let top = ann.scroll.offset().min(max_scroll);

    let configured_gutter = ctx
        .config
        .map_or(28usize, |c| c.jbgit.annotate.gutter_width)
        .clamp(16, 60);
    let max_gutter = (area.width as usize).saturating_sub(14).max(10);
    let gutter_w = configured_gutter.min(max_gutter);
    let lineno_w = len.to_string().len().max(3);
    let content_lines = bundle.content_lines();
    let visual_bounds = ann.selected_visual_bounds();
    let code_focused = ann.focus == AnnotateFocus::Code;
    let now_ts = chrono::Utc::now().timestamp();
    let tab_size = ctx.diff_tab_size();
    let uncomm_label = ctx.t("view.annotate.uncommitted_short").to_string();

    let rows = vh.min(len.saturating_sub(top));
    for v in 0..rows {
        let row_idx = top + v;
        let line = &bundle.lines[row_idx];
        let same_as_prev_visible = v > 0 && bundle.lines[row_idx - 1].oid == line.oid;
        let is_cursor = row_idx == ann.cursor;
        let in_visual = visual_bounds.is_some_and(|(lo, hi)| (lo..=hi).contains(&row_idx));
        let is_hovered = matches!(
            ctx.hover_target,
            Some(HitTarget::AnnotateLine { line_idx }) if *line_idx == row_idx
        );

        let bg = if is_cursor && code_focused {
            ctx.theme.bg_selection_focused
        } else if in_visual || is_cursor {
            ctx.theme.bg_selection_unfocused
        } else if is_hovered {
            ctx.theme.bg_hover
        } else {
            ctx.theme.bg_canvas
        };

        let bucket = ann.commit_age_bucket(line.oid, now_ts);
        let commit_opt = bundle.commits.get(&line.oid);
        let accent_color = if line.oid.is_zero() {
            ctx.theme.status_modified
        } else {
            match ann.color_mode {
                AnnotateColorMode::Age => ctx.theme.annotate_age_color(bucket),
                AnnotateColorMode::Author => {
                    let author_name = commit_opt.map(|c| c.author.name.as_str()).unwrap_or("");
                    ctx.branch_colors
                        .get_track_color(jbgit_core::branch_color_id(author_name))
                }
                AnnotateColorMode::None => ctx.theme.fg_muted,
            }
        };

        let any_meta_col = ann.show_hash || ann.show_author || ann.show_date;
        let meta_w = if any_meta_col {
            gutter_w.saturating_sub(1)
        } else {
            0
        };

        let mut spans: Vec<Span<'static>> = Vec::new();
        let stripe_fg = if same_as_prev_visible {
            ctx.theme.border_splitter
        } else {
            accent_color
        };
        spans.push(Span::styled("▎", Style::default().fg(stripe_fg).bg(bg)));

        if meta_w > 0 {
            if same_as_prev_visible {
                spans.push(Span::styled(" ".repeat(meta_w), Style::default().bg(bg)));
            } else {
                let effective_time = commit_opt
                    .map(|c| c.effective_time(ann.key.options.prefer_commit_date))
                    .unwrap_or(0);
                let meta_text = format_gutter_meta(
                    line.oid.is_zero(),
                    ann.show_hash,
                    ann.show_author,
                    ann.show_date,
                    &line.oid.short(),
                    commit_opt.map(|c| c.author.name.as_str()).unwrap_or(""),
                    effective_time,
                    &uncomm_label,
                    meta_w,
                );
                let meta_fg = if line.oid.is_zero() {
                    ctx.theme.status_modified
                } else {
                    accent_color
                };
                spans.push(Span::styled(meta_text, Style::default().fg(meta_fg).bg(bg)));
            }
        }

        let marker = if is_cursor {
            ">"
        } else if in_visual {
            "│"
        } else {
            " "
        };
        spans.push(Span::styled(
            format!(" {marker}{:>lineno_w$} │ ", line.final_line),
            Style::default().fg(ctx.theme.fg_muted).bg(bg),
        ));

        let prefix_w = 1 + meta_w + 1 + 1 + lineno_w + 3;
        let code_w = (area.width as usize).saturating_sub(prefix_w);
        let src_line = content_lines.get(row_idx).copied().unwrap_or("");
        let tokens = ann
            .syntax
            .as_ref()
            .and_then(|s| s.lines.get(row_idx))
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let code_spans = render_diff_line_spans_with_tab(
            src_line,
            tokens,
            &[],
            code_w,
            tab_size,
            Style::default().fg(ctx.theme.fg_primary).bg(bg),
            bg,
            ctx.theme.true_color,
        );
        spans.extend(code_spans);

        let row_rect = Rect::new(area.x, area.y + v as u16, area.width, 1);
        frame.render_widget(
            Paragraph::new(Line::from(spans)).style(Style::default().bg(bg)),
            row_rect,
        );
        ctx.hit_registry.register(
            row_rect,
            z + 1,
            HitTarget::AnnotateLine { line_idx: row_idx },
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn format_gutter_meta(
    uncommitted: bool,
    show_hash: bool,
    show_author: bool,
    show_date: bool,
    short: &str,
    author: &str,
    time: i64,
    uncomm_label: &str,
    width: usize,
) -> String {
    if width == 0 {
        return String::new();
    }
    let text = if uncommitted {
        if show_hash && width >= 18 {
            format!("{short} {uncomm_label}")
        } else {
            uncomm_label.to_string()
        }
    } else {
        let date = if show_date && time > 0 {
            format_absolute_time(time, false)
        } else {
            String::new()
        };
        let mut parts: Vec<String> = Vec::new();
        if show_hash {
            parts.push(short.to_string());
        }
        if show_author && !author.is_empty() {
            if !date.is_empty() && width >= 20 {
                let used = parts.iter().map(|p| p.width() + 1).sum::<usize>() + date.width() + 1;
                let author_w = width.saturating_sub(used);
                let a = truncate_to_width(author, author_w);
                let a_pad = " ".repeat(author_w.saturating_sub(a.width()));
                parts.push(format!("{a}{a_pad}"));
            } else {
                parts.push(author.to_string());
            }
        }
        if !date.is_empty() {
            parts.push(date);
        }
        parts.join(" ")
    };
    let clipped = truncate_to_width(&text, width);
    let pad = width.saturating_sub(clipped.width());
    format!("{clipped}{}", " ".repeat(pad))
}

fn render_commit_details_pane(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &AppState,
    ann: &AnnotateState,
    ctx: &mut RenderContext<'_>,
    _z: u8,
) {
    if area.height < 2 || area.width < 10 {
        return;
    }
    let hdr_rect = Rect::new(area.x, area.y, area.width, 1);
    let hdr_label = match ann.selected_line() {
        Some(line) if !line.oid.is_zero() => {
            ctx.t_fmt("view.annotate.details_title", &[("oid", &line.oid.short())])
        }
        _ => ctx.t("view.annotate.btn_details").to_string(),
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

    let Some(det_key) = ann.desired_details_key() else {
        frame.render_widget(
            Paragraph::new(Span::styled(
                truncate_to_width(ctx.t("toast.annotate_uncommitted"), body.width as usize),
                Style::default().fg(ctx.theme.fg_muted),
            )),
            Rect::new(body.x, body.y, body.width, 1),
        );
        return;
    };

    let Some(bundle) = state.diff_caches.peek_details(&det_key) else {
        frame.render_widget(
            Paragraph::new(Span::styled(
                truncate_to_width(ctx.t("common.loading"), body.width as usize),
                Style::default().fg(ctx.theme.fg_muted),
            )),
            Rect::new(body.x, body.y, body.width, 1),
        );
        return;
    };

    let mut lines: Vec<Line<'static>> = Vec::new();
    let max_w = body.width.saturating_sub(1) as usize;
    if let Some(details) = bundle.details.first() {
        let author_line = format!(
            "{}: {} <{}> ({})",
            ctx.t("view.annotate.author_label"),
            details.author.name,
            details.author.email,
            format_absolute_time(details.author.time, true),
        );
        lines.push(Line::from(Span::styled(
            truncate_to_width(&author_line, max_w),
            Style::default().fg(ctx.theme.fg_accent_link),
        )));
        if !details.same_author_and_committer() {
            let committer_line = format!(
                "{}: {} <{}> ({})",
                ctx.t("view.annotate.committer_label"),
                details.committer.name,
                details.committer.email,
                format_absolute_time(details.committer.time, true),
            );
            lines.push(Line::from(Span::styled(
                truncate_to_width(&committer_line, max_w),
                Style::default().fg(ctx.theme.fg_muted),
            )));
        }
        for msg_line in details.message.lines() {
            lines.push(Line::from(Span::styled(
                truncate_to_width(msg_line, max_w),
                Style::default().fg(ctx.theme.fg_primary),
            )));
        }
    }
    let files_hdr = ctx.t_fmt(
        "view.annotate.affected_files_header",
        &[("count", &bundle.changes.len().to_string())],
    );
    lines.push(Line::from(Span::styled(
        truncate_to_width(&files_hdr, max_w),
        Style::default()
            .fg(ctx.theme.fg_bright)
            .add_modifier(Modifier::BOLD),
    )));
    for ch in &bundle.changes {
        let badge = ch.kind.badge_char();
        let entry = format!(" {badge} {}", ch.path);
        lines.push(Line::from(Span::styled(
            truncate_to_width(&entry, max_w),
            Style::default().fg(ctx.theme.fg_primary),
        )));
    }

    frame.render_widget(
        Paragraph::new(lines).style(Style::default().bg(ctx.theme.bg_canvas)),
        body,
    );
}

fn render_inline_diff_pane(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &AppState,
    ann: &AnnotateState,
    ctx: &mut RenderContext<'_>,
    z: u8,
) {
    if area.height < 2 || area.width < 10 {
        return;
    }
    ctx.hit_registry
        .register(area, z, HitTarget::AnnotateDiffArea);

    let focused = ann.focus == AnnotateFocus::Diff;
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

    let head_oid = state.repo_snapshot.as_ref().and_then(|s| s.head_oid);
    let header_label = match ann.selected_line() {
        Some(line) if line.oid.is_zero() => {
            ctx.t_fmt("view.annotate.diff_uncommitted", &[("path", &ann.key.path)])
        }
        Some(line) => {
            let p = if line.orig_path.is_empty() {
                &ann.key.path
            } else {
                &line.orig_path
            };
            ctx.t_fmt(
                "view.annotate.diff_commit",
                &[("oid", &line.oid.short()), ("path", p)],
            )
        }
        None => ctx.t("view.log_details.diff_preview").to_string(),
    };

    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(if focused { "▌" } else { " " }, Style::default().fg(bar_fg)),
            Span::styled(
                truncate_to_width(&header_label, area.width.saturating_sub(2) as usize),
                Style::default().fg(if focused {
                    ctx.theme.fg_bright
                } else {
                    ctx.theme.fg_primary
                }),
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

    let h = body.height as usize;
    let Some(key) = ann.desired_diff_key(head_oid) else {
        ctx.record_scroll_bound(ScrollPane::AnnotateDiff, h, 0);
        frame.render_widget(
            Paragraph::new(Span::styled(
                truncate_to_width(
                    ctx.t("view.log_details.select_file_diff"),
                    body.width as usize,
                ),
                Style::default().fg(ctx.theme.fg_muted),
            )),
            Rect::new(body.x, body.y, body.width, 1),
        );
        return;
    };

    let Some(diff) = state.diff_caches.peek_diff(&key) else {
        ctx.record_scroll_bound(ScrollPane::AnnotateDiff, h, 0);
        frame.render_widget(
            Paragraph::new(Span::styled(
                truncate_to_width(ctx.t("view.log_details.loading_diff"), body.width as usize),
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
            session: &ann.diff,
            cached_syntax,
            scroll_pane: ScrollPane::AnnotateDiff,
        },
        ctx,
    );
}

fn render_detail_strip(
    frame: &mut Frame<'_>,
    area: Rect,
    ann: &AnnotateState,
    ctx: &mut RenderContext<'_>,
    z: u8,
) {
    frame.render_widget(
        Paragraph::new("").style(Style::default().bg(ctx.theme.bg_panel)),
        area,
    );
    ctx.hit_registry
        .register(area, z, HitTarget::AnnotateDetailStrip);

    if let Some(buf) = &ann.goto_line_input {
        let prompt = ctx.t_fmt("view.annotate.goto_prompt", &[("input", buf)]);
        frame.render_widget(
            Paragraph::new(vec![
                Line::from(Span::styled(
                    truncate_to_width(&prompt, area.width as usize),
                    Style::default()
                        .fg(ctx.theme.fg_bright)
                        .add_modifier(Modifier::BOLD),
                )),
                Line::from(Span::styled(
                    truncate_to_width(ctx.t("view.annotate.goto_hint"), area.width as usize),
                    Style::default().fg(ctx.theme.fg_muted),
                )),
            ])
            .style(Style::default().bg(ctx.theme.bg_panel)),
            area,
        );
        return;
    }

    let Some(line) = ann.selected_line() else {
        return;
    };
    if line.oid.is_zero() {
        let line1 = ctx.t_fmt(
            "view.annotate.detail_uncommitted",
            &[
                ("line", &line.final_line.to_string()),
                ("path", &ann.key.path),
            ],
        );
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                truncate_to_width(&line1, area.width as usize),
                Style::default().fg(ctx.theme.status_modified),
            )))
            .style(Style::default().bg(ctx.theme.bg_panel)),
            area,
        );
        return;
    }

    if let Some(commit) = ann.selected_commit() {
        let now_ts = chrono::Utc::now().timestamp();
        let ts = commit.effective_time(ann.key.options.prefer_commit_date);
        let rel = ctx.format_relative_time(ts, now_ts);
        let abs = format_absolute_time(ts, true);
        let header = format!(
            "{} • {} <{}> • {} ({}) • {}:{}",
            line.oid.short(),
            commit.author.name,
            commit.author.email,
            rel,
            abs,
            if line.orig_path.is_empty() {
                ann.key.path.as_str()
            } else {
                line.orig_path.as_str()
            },
            line.orig_line,
        );
        let summary = if let Some((prev, _)) = ann.selected_previous() {
            format!("{}  (parent {})", commit.summary, prev.short())
        } else {
            commit.summary.clone()
        };
        frame.render_widget(
            Paragraph::new(vec![
                Line::from(Span::styled(
                    truncate_to_width(&header, area.width as usize),
                    Style::default().fg(ctx.theme.fg_accent_link),
                )),
                Line::from(Span::styled(
                    truncate_to_width(&summary, area.width as usize),
                    Style::default().fg(ctx.theme.fg_primary),
                )),
            ])
            .style(Style::default().bg(ctx.theme.bg_panel)),
            area,
        );
    }
}
