use jgit_core::{PaneId, SplitAxis, SplitterId};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use unicode_width::UnicodeWidthStr;

use crate::app::{AppState, RenderContext};
use crate::hit_test::HitTarget;
use crate::views::{
    list_viewport, render_pane_frame, render_splitter_divider, row_background, truncate_to_width,
};

pub fn render_console_tab(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &AppState,
    ctx: &mut RenderContext<'_>,
    splitter_containers: &mut Vec<(SplitterId, SplitAxis, Rect)>,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let split = state
        .splitters
        .split(area, SplitterId::ConsoleMainHorizontal, SplitAxis::Horizontal);
    splitter_containers.push((
        SplitterId::ConsoleMainHorizontal,
        SplitAxis::Horizontal,
        split.container,
    ));

    let list_sub = format!("{} entries (press 'p' to probe)", state.console_entries.len());
    let list_body = render_pane_frame(
        frame,
        split.first,
        PaneId::ConsoleCommandList,
        &list_sub,
        ctx,
    );
    render_command_list(frame, list_body, state, ctx);

    if !split.collapsed {
        render_splitter_divider(
            frame,
            split.divider,
            SplitterId::ConsoleMainHorizontal,
            SplitAxis::Horizontal,
            ctx,
        );

        let detail_sub = state
            .console_entries
            .get(state.console_selected_idx)
            .map(|e| format!("#{} {}", e.id, e.timestamp_label()))
            .unwrap_or_default();
        let out_body = render_pane_frame(
            frame,
            split.second,
            PaneId::ConsoleOutputDetail,
            &detail_sub,
            ctx,
        );
        render_command_output(frame, out_body, state, ctx);
    }
}

fn render_command_list(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &AppState,
    ctx: &mut RenderContext<'_>,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    if state.console_entries.is_empty() {
        frame.render_widget(
            Paragraph::new(Span::styled(
                " No Git CLI commands executed yet. Press 'p' to run git status.",
                Style::default().fg(ctx.theme.fg_muted),
            )),
            area,
        );
        return;
    }

    let visible_h = area.height as usize;
    let (selected, offset) = list_viewport(
        state.console_entries.len(),
        state.console_selected_idx,
        visible_h,
    );

    let focused = ctx.focused_pane == PaneId::ConsoleCommandList;

    for (vis_idx, entry) in state
        .console_entries
        .iter()
        .skip(offset)
        .take(visible_h)
        .enumerate()
    {
        let idx = offset + vis_idx;
        let row_rect = Rect::new(area.x, area.y.saturating_add(vis_idx as u16), area.width, 1);
        let is_sel = idx == selected;

        let is_hovered = matches!(
            ctx.hover_target,
            Some(HitTarget::ConsoleCommandRow { idx: h }) if *h == idx
        );
        let bg = row_background(is_sel, focused, is_hovered, ctx.theme.bg_canvas, ctx.theme);

        let (badge_txt, badge_fg) = match entry.exit_code {
            None => (format!(" {} ", state.spinner_char()), ctx.theme.border_focused),
            Some(0) => (" ✓ ".to_string(), ctx.theme.status_added),
            Some(code) => (format!(" ✗{code}"), ctx.theme.status_conflicted),
        };

        let dur = entry
            .duration_ms
            .map(|ms| format!(" {ms}ms"))
            .unwrap_or_else(|| " …".to_string());

        let reserved = 10 + badge_txt.width() + dur.width();
        let max_cmd = (area.width as usize).saturating_sub(reserved);

        let line = Line::from(vec![
            Span::styled(
                format!(" [{}]", entry.timestamp_label()),
                Style::default().fg(ctx.theme.fg_muted),
            ),
            Span::styled(badge_txt, Style::default().fg(badge_fg).add_modifier(Modifier::BOLD)),
            Span::styled(
                truncate_to_width(&entry.command_line, max_cmd),
                Style::default().fg(if is_sel {
                    ctx.theme.fg_bright
                } else {
                    ctx.theme.fg_primary
                }),
            ),
            Span::styled(dur, Style::default().fg(ctx.theme.fg_muted)),
        ]);

        frame.render_widget(Paragraph::new(line).style(Style::default().bg(bg)), row_rect);
        ctx.hit_registry
            .register(row_rect, 1, HitTarget::ConsoleCommandRow { idx });
    }
}

fn render_command_output(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &AppState,
    ctx: &mut RenderContext<'_>,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    ctx.hit_registry
        .register(area, 1, HitTarget::ConsoleOutputArea);

    let Some(entry) = state.console_entries.get(state.console_selected_idx) else {
        frame.render_widget(
            Paragraph::new(Span::styled(
                " Select a command entry on the left to inspect stdout/stderr.",
                Style::default().fg(ctx.theme.fg_muted),
            )),
            area,
        );
        return;
    };

    let mut lines: Vec<Line<'_>> = Vec::new();
    let status_span = match entry.exit_code {
        None => Span::styled(
            format!("RUNNING {}", state.spinner_char()),
            Style::default().fg(ctx.theme.border_focused),
        ),
        Some(0) => Span::styled(
            format!(
                "EXIT 0 ({} ms)",
                entry.duration_ms.unwrap_or_default()
            ),
            Style::default()
                .fg(ctx.theme.status_added)
                .add_modifier(Modifier::BOLD),
        ),
        Some(code) => Span::styled(
            format!(
                "EXIT {code} ({} ms)",
                entry.duration_ms.unwrap_or_default()
            ),
            Style::default()
                .fg(ctx.theme.status_conflicted)
                .add_modifier(Modifier::BOLD),
        ),
    };

    lines.push(Line::from(vec![
        Span::styled(
            format!(" $ {}  ", entry.command_line),
            Style::default()
                .fg(ctx.theme.fg_bright)
                .add_modifier(Modifier::BOLD),
        ),
        status_span,
    ]));
    lines.push(Line::from(Span::styled(
        " ────────────────────────────────────────",
        Style::default().fg(ctx.theme.border_splitter),
    )));

    if entry.stdout_lines.is_empty() && entry.stderr_lines.is_empty() {
        lines.push(Line::from(Span::styled(
            " (no output)",
            Style::default().fg(ctx.theme.fg_muted),
        )));
    } else {
        for l in &entry.stdout_lines {
            lines.push(Line::from(Span::styled(
                format!(" {l}"),
                Style::default().fg(ctx.theme.fg_primary),
            )));
        }
        for l in &entry.stderr_lines {
            lines.push(Line::from(Span::styled(
                format!(" [stderr] {l}"),
                Style::default().fg(ctx.theme.status_conflicted),
            )));
        }
    }

    let visible_h = area.height as usize;
    let max_scroll = lines.len().saturating_sub(visible_h);
    let scroll = state.console_output_scroll.min(max_scroll);

    let slice: Vec<Line<'_>> = lines.into_iter().skip(scroll).take(visible_h).collect();
    frame.render_widget(Paragraph::new(slice), area);
}
