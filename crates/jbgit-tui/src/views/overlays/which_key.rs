use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph};
use unicode_width::UnicodeWidthStr;

use crate::app::{AppState, RenderContext};

use super::{draw_shadow, truncate_display_width};

pub fn render_which_key_popup(
    frame: &mut Frame<'_>,
    screen: Rect,
    footer_h: u16,
    state: &AppState,
    ctx: &RenderContext<'_>,
) {
    if !state.modal_stack.is_empty() || screen.width < 24 || screen.height < 6 {
        return;
    }
    let completions = crate::keymap::pending_completions(state);
    if completions.is_empty() {
        return;
    }

    let prefix = crate::keymap::pending_prefix_display(state);
    let title_str = format!(" {prefix} + … ");
    let visible: Vec<&(String, String)> = completions.iter().take(10).collect();

    let max_key_w = visible
        .iter()
        .map(|(k, _)| k.width())
        .max()
        .unwrap_or(2)
        .clamp(2, 12);
    let max_desc_w = visible
        .iter()
        .map(|(_, d)| d.width())
        .max()
        .unwrap_or(8)
        .clamp(8, 36);
    let cancel_hint = format!(" {}", ctx.t("help.which_key_cancel"));

    let content_w = (2 + max_key_w + 5 + max_desc_w + 1)
        .max(title_str.width() + 2)
        .max(cancel_hint.width() + 2);
    let w = ((content_w + 2) as u16)
        .clamp(30, 58)
        .min(screen.width.saturating_sub(2));
    let bottom_margin = footer_h.saturating_add(1);
    let h = ((visible.len() + 3) as u16).min(screen.height.saturating_sub(bottom_margin));
    if w < 16 || h < 5 {
        return;
    }

    let x = screen.x + screen.width.saturating_sub(w + 2);
    let y = screen.y + screen.height.saturating_sub(h + bottom_margin);
    let rect = Rect::new(x, y, w, h);

    draw_shadow(frame, screen, rect);
    crate::views::clear_modal_overlay(frame, rect);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(ctx.theme.border_focused))
        .style(Style::default().bg(ctx.theme.bg_elevated))
        .title(Span::styled(
            title_str,
            Style::default()
                .fg(ctx.theme.fg_accent_link)
                .add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);

    let mut lines: Vec<Line<'_>> = Vec::with_capacity(visible.len() + 1);
    let desc_budget = (inner.width as usize).saturating_sub(2 + max_key_w + 5 + 1);
    for (k, d) in visible {
        let key_pad = " ".repeat(max_key_w.saturating_sub(k.width()));
        let trimmed_desc = truncate_display_width(d, desc_budget);
        lines.push(Line::from(vec![
            Span::styled(" ", Style::default().bg(ctx.theme.bg_elevated)),
            Span::styled(
                k.clone(),
                Style::default()
                    .fg(ctx.theme.fg_accent_link)
                    .bg(ctx.theme.bg_elevated)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("{key_pad}  →  "),
                Style::default()
                    .fg(ctx.theme.fg_muted)
                    .bg(ctx.theme.bg_elevated),
            ),
            Span::styled(
                trimmed_desc,
                Style::default()
                    .fg(ctx.theme.fg_primary)
                    .bg(ctx.theme.bg_elevated),
            ),
        ]));
    }
    lines.push(Line::from(Span::styled(
        truncate_display_width(&cancel_hint, inner.width as usize),
        Style::default()
            .fg(ctx.theme.fg_muted)
            .bg(ctx.theme.bg_elevated),
    )));

    frame.render_widget(Paragraph::new(lines), inner);
}
