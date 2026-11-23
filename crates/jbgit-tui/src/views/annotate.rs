//! Modal full-file Git Blame / Annotate view with age-heatmap gutter,
//! syntax highlighting, visual line selection, and optional inline diff preview.

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};
use unicode_width::UnicodeWidthStr;

use crate::app::{AppState, RenderContext, ScrollPane};
use crate::hit_test::{AnnotateToolbarButton, HitTarget};
use crate::log_view::{AnnotateError, AnnotateFocus, AnnotateState, format_absolute_time};
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
    frame.render_widget(Clear, rect);

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
    if ann.key.options.detect_moves {
        title.push_str(" [M]");
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
    let detail_rect = Rect::new(inner.x, footer_rect.y.saturating_sub(detail_h), inner.width, detail_h);
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
    let buttons = vec![
        (
            ctx.t("view.annotate.btn_whitespace").to_string(),
            ann.key.options.ignore_whitespace,
            AnnotateToolbarButton::IgnoreWhitespace,
        ),
        (
            ctx.t("view.annotate.btn_moves").to_string(),
            ann.key.options.detect_moves,
            AnnotateToolbarButton::DetectMoves,
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
        if ann.show_diff && content.height >= 10 {
            let parts = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Percentage(62),
                    Constraint::Min(4),
                ])
                .split(content);
            render_code_pane(frame, parts[0], ann, ctx, z + 2);
            render_inline_diff_pane(frame, parts[1], state, ann, ctx, z + 2);
        } else {
            render_code_pane(frame, content, ann, ctx, z + 2);
        }
    }

    if detail_h > 0 {
        render_detail_strip(frame, detail_rect, ann, ctx);
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
    ctx.hit_registry
        .register(area, z, HitTarget::AnnotateArea);

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
        let age_color = if line.oid.is_zero() {
            ctx.theme.status_modified
        } else {
            ctx.theme.annotate_age_color(bucket)
        };

        let mut spans: Vec<Span<'static>> = Vec::new();
        let stripe_fg = if same_as_prev_visible {
            ctx.theme.border_splitter
        } else {
            age_color
        };
        spans.push(Span::styled("▎", Style::default().fg(stripe_fg).bg(bg)));

        let meta_w = gutter_w.saturating_sub(1);
        if same_as_prev_visible {
            spans.push(Span::styled(" ".repeat(meta_w), Style::default().bg(bg)));
        } else {
            let meta_text = format_gutter_meta(
                line.oid.is_zero(),
                &line.oid.short(),
                bundle
                    .commits
                    .get(&line.oid)
                    .map(|c| c.author.name.as_str())
                    .unwrap_or(""),
                bundle
                    .commits
                    .get(&line.oid)
                    .map(|c| c.author.time)
                    .unwrap_or(0),
                &uncomm_label,
                meta_w,
            );
            let meta_fg = if line.oid.is_zero() {
                ctx.theme.status_modified
            } else {
                age_color
            };
            spans.push(Span::styled(
                meta_text,
                Style::default().fg(meta_fg).bg(bg),
            ));
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
        ctx.hit_registry
            .register(row_rect, z + 1, HitTarget::AnnotateLine { line_idx: row_idx });
    }
}

fn format_gutter_meta(
    uncommitted: bool,
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
        if width >= 18 {
            format!("{short} {uncomm_label}")
        } else {
            uncomm_label.to_string()
        }
    } else {
        let date = if time > 0 {
            format_absolute_time(time, false)
        } else {
            String::new()
        };
        if width >= 25 && !date.is_empty() {
            let author_w = width.saturating_sub(short.width() + 1 + 1 + date.width() + 1);
            let a = truncate_to_width(author, author_w);
            let a_pad = " ".repeat(author_w.saturating_sub(a.width()));
            format!("{short} {a}{a_pad} {date}")
        } else if !author.is_empty() {
            format!("{short} {author}")
        } else {
            short.to_string()
        }
    };
    let clipped = truncate_to_width(&text, width);
    let pad = width.saturating_sub(clipped.width());
    format!("{clipped}{}", " ".repeat(pad))
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
        Some(line) if line.oid.is_zero() => ctx
            .t_fmt("view.annotate.diff_uncommitted", &[("path", &ann.key.path)]),
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

    let body = Rect::new(area.x, area.y + 1, area.width, area.height.saturating_sub(1));
    frame.render_widget(
        Paragraph::new("").style(Style::default().bg(ctx.theme.bg_canvas)),
        body,
    );

    let h = body.height as usize;
    let Some(key) = ann.desired_diff_key(head_oid) else {
        ctx.record_scroll_bound(ScrollPane::AnnotateDiff, h, 0);
        frame.render_widget(
            Paragraph::new(Span::styled(
                truncate_to_width(ctx.t("view.log_details.select_file_diff"), body.width as usize),
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
            rows_cache: &ann.diff.rows_cache,
            cached_syntax,
            side_by_side: ann.diff.side_by_side,
            diff_scroll: ann.diff.scroll.offset(),
            scroll_pane: ScrollPane::AnnotateDiff,
        },
        ctx,
    );
}

fn render_detail_strip(
    frame: &mut Frame<'_>,
    area: Rect,
    ann: &AnnotateState,
    ctx: &RenderContext<'_>,
) {
    frame.render_widget(
        Paragraph::new("").style(Style::default().bg(ctx.theme.bg_panel)),
        area,
    );

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
            &[("line", &line.final_line.to_string()), ("path", &ann.key.path)],
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
        let rel = ctx.format_relative_time(commit.author.time, now_ts);
        let abs = format_absolute_time(commit.author.time, true);
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
        let summary = if let Some((prev, _)) = &commit.previous {
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
