mod context_menu;
mod popups;
mod tooltip;
mod which_key;

use jbgit_core::ModalButton;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::Span;
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use unicode_width::UnicodeWidthStr;

use crate::app::{AppState, ModalLayer, RenderContext};
use crate::hit_test::HitTarget;

pub use context_menu::{context_menu_rect, menu_item_row_y, mnemonic_spans};
pub use tooltip::render_hover_tooltip;
pub use which_key::render_which_key_popup;

pub fn render_modals(
    frame: &mut Frame<'_>,
    screen: Rect,
    state: &AppState,
    ctx: &mut RenderContext<'_>,
) {
    for layer in &state.modal_stack {
        let z = layer.z_level();
        // Register full-screen backdrop so lower-layer widgets cannot receive clicks
        // (cascading submenus share their root menu's backdrop so the parent
        // menu stays hoverable while a child is open).
        if !matches!(layer, ModalLayer::ContextMenu { depth, .. } if *depth > 0) {
            ctx.hit_registry
                .register(screen, z, HitTarget::ModalBackdrop { layer_z: z });
        }

        match layer {
            ModalLayer::FloatingPopup {
                kind,
                query,
                selected,
            } => popups::render_floating_popup(frame, screen, state, ctx, *kind, query, *selected, z),
            ModalLayer::ContextMenu {
                anchor,
                items,
                selected,
                depth,
                parent,
            } => context_menu::render_context_menu(
                frame, screen, ctx, *anchor, *parent, items, *selected, *depth, z,
            ),
            ModalLayer::Dialog(d) => crate::views::dialogs::render_dialog(frame, screen, ctx, d, z),
            ModalLayer::CriticalDialog {
                title,
                body,
                confirm_label,
                cancel_label,
                selected_button,
                ..
            } => render_critical_dialog(
                frame,
                screen,
                ctx,
                title,
                body,
                confirm_label,
                cancel_label,
                *selected_button,
                z,
            ),
        }
    }
}

pub(super) fn truncate_display_width(text: &str, max_w: usize) -> String {
    if text.width() <= max_w {
        return text.to_string();
    }
    if max_w == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut w = 0usize;
    let limit = max_w.saturating_sub(1);
    for ch in text.chars() {
        let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if w + cw > limit {
            break;
        }
        out.push(ch);
        w += cw;
    }
    out.push('…');
    out
}

/// Darkens the cells under a 1-cell drop shadow (right column + bottom row).
pub fn draw_shadow(frame: &mut Frame<'_>, screen: Rect, rect: Rect) {
    let shade = ratatui::style::Color::Rgb(14, 14, 16);
    let dim = ratatui::style::Color::Rgb(70, 72, 76);
    let buf = frame.buffer_mut();
    let sx = rect.right();
    if sx < screen.right() {
        for y in rect.y.saturating_add(1)..rect.bottom().saturating_add(1).min(screen.bottom()) {
            if let Some(cell) = buf.cell_mut((sx, y)) {
                cell.set_bg(shade).set_fg(dim);
            }
        }
    }
    let sy = rect.bottom();
    if sy < screen.bottom() {
        for x in rect.x.saturating_add(1)..rect.right().min(screen.right()) {
            if let Some(cell) = buf.cell_mut((x, sy)) {
                cell.set_bg(shade).set_fg(dim);
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn render_critical_dialog(
    frame: &mut Frame<'_>,
    screen: Rect,
    ctx: &mut RenderContext<'_>,
    title: &str,
    body: &str,
    confirm_label: &str,
    cancel_label: &str,
    selected_button: ModalButton,
    z: u8,
) {
    let width = screen.width.clamp(screen.width.min(28), 60);
    let height = screen.height.clamp(screen.height.min(7), 12);
    let x = screen
        .x
        .saturating_add(screen.width.saturating_sub(width) / 2);
    let y = screen
        .y
        .saturating_add(screen.height.saturating_sub(height) / 2);
    let rect = Rect::new(x, y, width, height);

    frame.render_widget(Clear, rect);

    let block = Block::default()
        .title(Span::styled(
            format!(" {title} "),
            Style::default()
                .fg(ctx.theme.status_conflicted)
                .add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(ctx.theme.status_conflicted))
        .style(Style::default().bg(ctx.theme.bg_elevated));

    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    ctx.hit_registry.register(rect, z + 1, HitTarget::DialogBody);

    let body_h = inner.height.saturating_sub(2);
    let body_rect = Rect::new(inner.x, inner.y, inner.width, body_h);
    frame.render_widget(
        Paragraph::new(body)
            .wrap(Wrap { trim: false })
            .style(Style::default().fg(ctx.theme.fg_primary)),
        body_rect,
    );

    if inner.height >= 2 {
        let btn_y = inner.y.saturating_add(inner.height.saturating_sub(1));
        let btn0 = format!(" [{confirm_label}] ");
        let btn1 = format!(" [{cancel_label}] ");
        let btn0_rect = Rect::new(inner.x.saturating_add(1), btn_y, btn0.width() as u16, 1);
        let btn1_rect = Rect::new(
            btn0_rect
                .x
                .saturating_add(btn0_rect.width)
                .saturating_add(2),
            btn_y,
            btn1.width() as u16,
            1,
        );

        let s0 = if selected_button == ModalButton::Confirm {
            Style::default()
                .fg(ctx.theme.fg_bright)
                .bg(ctx.theme.border_focused)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
                .fg(ctx.theme.fg_primary)
                .bg(ctx.theme.bg_panel)
        };
        let s1 = if selected_button == ModalButton::Cancel {
            Style::default()
                .fg(ctx.theme.fg_bright)
                .bg(ctx.theme.border_focused)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
                .fg(ctx.theme.fg_primary)
                .bg(ctx.theme.bg_panel)
        };

        frame.render_widget(Paragraph::new(Span::styled(btn0, s0)), btn0_rect);
        frame.render_widget(Paragraph::new(Span::styled(btn1, s1)), btn1_rect);
        ctx.hit_registry.register(
            btn0_rect,
            z + 2,
            HitTarget::ModalButton(ModalButton::Confirm),
        );
        ctx.hit_registry.register(
            btn1_rect,
            z + 2,
            HitTarget::ModalButton(ModalButton::Cancel),
        );
    }
}
