use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};
use unicode_width::UnicodeWidthStr;

use crate::app::{ContextMenuEntryKind, ContextMenuItem, MenuParent, RenderContext};
use crate::hit_test::HitTarget;
use crate::views::truncate_to_width;

use super::draw_shadow;

/// Rows reserved for a context-menu item list (border excluded).
fn menu_visible_rows(rect: Rect) -> usize {
    rect.height.saturating_sub(2) as usize
}

/// Scroll offset keeping `selected` visible (deterministic for hit-testing).
pub fn menu_scroll(selected: usize, visible: usize, len: usize) -> usize {
    if visible == 0 || len <= visible || selected < visible {
        0
    } else {
        (selected + 1 - visible).min(len - visible)
    }
}

/// Screen row of item `idx` when `idx` is the selected item.
pub fn menu_item_row_y(_screen: Rect, rect: Rect, items: &[ContextMenuItem], idx: usize) -> u16 {
    let visible = menu_visible_rows(rect);
    let scroll = menu_scroll(idx, visible, items.len());
    rect.y + 1 + idx.saturating_sub(scroll) as u16
}

fn menu_has_icons(items: &[ContextMenuItem]) -> bool {
    items.iter().any(|i| i.icon.is_some())
}

/// Menu rectangle (shadow excluded) with viewport auto-flip: menus that would
/// overflow the right / bottom edge open to the left / upwards, and cascading
/// submenus open on the left of their parent when the right side is full.
pub fn context_menu_rect(
    screen: Rect,
    anchor: (u16, u16),
    parent: Option<MenuParent>,
    items: &[ContextMenuItem],
) -> Rect {
    let icon_w = if menu_has_icons(items) { 2 } else { 0 };
    let max_label = items
        .iter()
        .map(|i| {
            let suffix_w = match &i.kind {
                ContextMenuEntryKind::Submenu(_) => 3,
                ContextMenuEntryKind::Action(_) => i.shortcut.as_ref().map_or(0, |s| s.width() + 3),
                ContextMenuEntryKind::Separator => 0,
            };
            i.label.width() + suffix_w + 4 + icon_w
        })
        .max()
        .unwrap_or(20);

    // One extra column / row is reserved for the drop shadow.
    let max_w = screen.width.saturating_sub(1).max(1);
    let max_h = screen.height.saturating_sub(1).max(1);
    let width = (max_label as u16).clamp(22.min(max_w), max_w);
    let height = (items.len() as u16 + 2).clamp(3.min(max_h), max_h);
    let right_edge = screen.right();
    let bottom_edge = screen.bottom();
    let fits_right = |x: u16| x.saturating_add(width).saturating_add(1) <= right_edge;

    let (x, ay) = match parent {
        Some(p) => {
            let right_of = p.rect.right();
            let x = if fits_right(right_of) {
                right_of
            } else if p.rect.x >= screen.x.saturating_add(width) {
                p.rect.x - width
            } else {
                right_edge.saturating_sub(width + 1)
            };
            (x, p.row_y.saturating_sub(1))
        }
        None => {
            let x = if fits_right(anchor.0) {
                anchor.0
            } else {
                anchor.0.saturating_sub(width)
            };
            (x, anchor.1)
        }
    };
    let y = if ay.saturating_add(height).saturating_add(1) > bottom_edge {
        ay.saturating_add(1).saturating_sub(height)
    } else {
        ay
    };
    let x = x
        .max(screen.x)
        .min(right_edge.saturating_sub(width + 1).max(screen.x));
    let y = y
        .max(screen.y)
        .min(bottom_edge.saturating_sub(height + 1).max(screen.y));
    Rect::new(x, y, width, height)
}

/// Label spans with the mnemonic letter underlined (`C̲herry-Pick`).
pub fn mnemonic_spans(label: &str, mnemonic: Option<char>, style: Style) -> Vec<Span<'static>> {
    let Some(m) = mnemonic else {
        return vec![Span::styled(label.to_string(), style)];
    };
    let lower = m.to_ascii_lowercase();
    match label
        .char_indices()
        .find(|(_, c)| c.to_ascii_lowercase() == lower)
    {
        Some((pos, c)) => {
            let end = pos + c.len_utf8();
            vec![
                Span::styled(label[..pos].to_string(), style),
                Span::styled(
                    label[pos..end].to_string(),
                    style.add_modifier(Modifier::UNDERLINED),
                ),
                Span::styled(label[end..].to_string(), style),
            ]
        }
        None => vec![Span::styled(label.to_string(), style)],
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn render_context_menu(
    frame: &mut Frame<'_>,
    screen: Rect,
    ctx: &mut RenderContext<'_>,
    anchor: (u16, u16),
    parent: Option<MenuParent>,
    items: &[ContextMenuItem],
    selected: usize,
    depth: u8,
    z: u8,
) {
    if items.is_empty() || screen.width < 12 || screen.height < 4 {
        return;
    }

    let rect = context_menu_rect(screen, anchor, parent, items);
    draw_shadow(frame, screen, rect);
    frame.render_widget(Clear, rect);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(ctx.theme.border_splitter))
        .style(Style::default().bg(ctx.theme.bg_elevated));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    ctx.hit_registry
        .register(rect, z + 1, HitTarget::MenuBody { menu_depth: depth });

    let visible = inner.height as usize;
    let scroll = menu_scroll(selected, visible, items.len());
    let has_icons = menu_has_icons(items);

    for (vis, (idx, item)) in items
        .iter()
        .enumerate()
        .skip(scroll)
        .take(visible)
        .enumerate()
    {
        let y = inner.y.saturating_add(vis as u16);
        if item.is_separator() {
            frame.render_widget(
                Paragraph::new(Span::styled(
                    format!("├{}┤", "─".repeat(inner.width as usize)),
                    Style::default().fg(ctx.theme.border_splitter),
                ))
                .style(Style::default().bg(ctx.theme.bg_elevated)),
                Rect::new(rect.x, y, rect.width, 1),
            );
            continue;
        }
        let row_rect = Rect::new(inner.x, y, inner.width, 1);
        let is_sel = idx == selected;
        let enabled = item.is_enabled();
        let bg = if is_sel {
            if enabled {
                ctx.theme.bg_selection_focused
            } else {
                ctx.theme.bg_selection_unfocused
            }
        } else {
            ctx.theme.bg_elevated
        };
        let fg = if !enabled {
            ctx.theme.fg_muted
        } else if is_sel {
            ctx.theme.fg_bright
        } else {
            ctx.theme.fg_primary
        };

        let right_label = match &item.kind {
            ContextMenuEntryKind::Submenu(_) => "▸",
            _ => item.shortcut.as_deref().unwrap_or(""),
        };
        let icon_w = if has_icons { 2 } else { 0 };
        let avail = (inner.width as usize).saturating_sub(right_label.width() + 3 + icon_w);
        let label = truncate_to_width(&item.label, avail);
        let pad = (inner.width as usize)
            .saturating_sub(label.width() + right_label.width() + 2 + icon_w);

        let mut spans = vec![Span::raw(" ")];
        if has_icons {
            let icon = item.icon.unwrap_or(" ");
            let icon = if icon.width() <= 1 {
                format!("{icon:<1} ")
            } else {
                truncate_to_width(icon, 2)
            };
            spans.push(Span::styled(
                icon,
                Style::default().fg(if enabled {
                    ctx.theme.fg_accent_link
                } else {
                    ctx.theme.fg_muted
                }),
            ));
        }
        spans.extend(mnemonic_spans(
            &label,
            if enabled { item.mnemonic } else { None },
            Style::default().fg(fg),
        ));
        spans.push(Span::raw(" ".repeat(pad)));
        spans.push(Span::styled(
            format!("{right_label} "),
            Style::default().fg(ctx.theme.fg_muted),
        ));

        frame.render_widget(
            Paragraph::new(Line::from(spans)).style(Style::default().bg(bg)),
            row_rect,
        );
        ctx.hit_registry.register(
            row_rect,
            z + 1,
            HitTarget::MenuItem {
                menu_depth: depth,
                item_idx: idx,
            },
        );
    }
    let arrow_style = Style::default().fg(ctx.theme.fg_accent_link);
    if scroll > 0 {
        frame.render_widget(
            Paragraph::new(Span::styled("▴", arrow_style)),
            Rect::new(rect.x + rect.width / 2, rect.y, 1, 1),
        );
    }
    if scroll + visible < items.len() {
        frame.render_widget(
            Paragraph::new(Span::styled("▾", arrow_style)),
            Rect::new(rect.x + rect.width / 2, rect.bottom().saturating_sub(1), 1, 1),
        );
    }
}
