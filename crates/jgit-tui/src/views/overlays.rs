use jgit_core::{ModalButton, RefKind};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use unicode_width::UnicodeWidthStr;

use crate::app::{
    AppState, ContextMenuEntryKind, ContextMenuItem, ModalLayer, PopupKind, RenderContext,
};
use crate::hit_test::HitTarget;
use crate::views::truncate_to_width;

pub fn context_menu_rect(screen: Rect, anchor: (u16, u16), items: &[ContextMenuItem]) -> Rect {
    let max_label = items
        .iter()
        .map(|i| {
            let suffix_w = match &i.kind {
                ContextMenuEntryKind::Submenu(_) => 3,
                ContextMenuEntryKind::Action(_) => {
                    i.shortcut.as_ref().map_or(0, |s| s.width() + 3)
                }
            };
            i.label.width() + suffix_w + 4
        })
        .max()
        .unwrap_or(20);

    let width = (max_label as u16).clamp(22, screen.width.saturating_sub(2).max(22));
    let height = (items.len() as u16 + 2).min(screen.height.saturating_sub(1).max(3));

    let x = anchor
        .0
        .min(screen.x.saturating_add(screen.width).saturating_sub(width));
    let y = anchor
        .1
        .min(screen.y.saturating_add(screen.height).saturating_sub(height));
    Rect::new(x, y, width, height)
}

pub fn render_modals(
    frame: &mut Frame<'_>,
    screen: Rect,
    state: &AppState,
    ctx: &mut RenderContext<'_>,
) {
    for layer in &state.modal_stack {
        let z = layer.z_level();
        // Register full-screen backdrop so lower-layer widgets cannot receive clicks
        ctx.hit_registry
            .register(screen, z, HitTarget::ModalBackdrop { layer_z: z });

        match layer {
            ModalLayer::FloatingPopup {
                kind,
                query,
                selected,
            } => render_floating_popup(frame, screen, state, ctx, *kind, query, *selected, z),
            ModalLayer::ContextMenu {
                anchor,
                items,
                selected,
                depth,
            } => render_context_menu(frame, screen, ctx, *anchor, items, *selected, *depth, z),
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

#[allow(clippy::too_many_arguments)]
fn render_floating_popup(
    frame: &mut Frame<'_>,
    screen: Rect,
    state: &AppState,
    ctx: &mut RenderContext<'_>,
    kind: PopupKind,
    query: &str,
    selected: usize,
    z: u8,
) {
    let width = screen.width.clamp(screen.width.min(24), 64);
    let height = screen.height.clamp(screen.height.min(8), 18);
    let x = screen.x.saturating_add(screen.width.saturating_sub(width) / 2);
    let y = screen.y.saturating_add(screen.height.saturating_sub(height) / 2);
    let rect = Rect::new(x, y, width, height);

    frame.render_widget(Clear, rect);

    let title = match kind {
        PopupKind::ShortcutsHelp => " Keyboard & Mouse Shortcuts (Esc to close) ",
        PopupKind::BranchesQuickView => " Git Branches (Type to filter, Esc to close) ",
    };

    let block = Block::default()
        .title(Span::styled(
            title,
            Style::default()
                .fg(ctx.theme.fg_bright)
                .add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(ctx.theme.border_focused))
        .style(Style::default().bg(ctx.theme.bg_elevated));

    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    // Block clicks inside popup from triggering backdrop close
    ctx.hit_registry
        .register(rect, z + 1, HitTarget::ModalBackdrop { layer_z: z });

    match kind {
        PopupKind::ShortcutsHelp => {
            let lines = vec![
                Line::from(Span::styled(
                    " 1 / 2 / 3 / 4     Switch Commit / Log / Stash / Console",
                    Style::default().fg(ctx.theme.fg_primary),
                )),
                Line::from(Span::styled(
                    " Tab / Shift+Tab   Cycle focused pane inside current tab",
                    Style::default().fg(ctx.theme.fg_primary),
                )),
                Line::from(Span::styled(
                    " j / k / Up / Down Navigate branches, commits, files, or commands",
                    Style::default().fg(ctx.theme.fg_primary),
                )),
                Line::from(Span::styled(
                    " Mouse Drag │ / ─  Resize splitters (double-click resets)",
                    Style::default().fg(ctx.theme.fg_primary),
                )),
                Line::from(Span::styled(
                    " Mouse Wheel       Scrolls pane under cursor",
                    Style::default().fg(ctx.theme.fg_primary),
                )),
                Line::from(Span::styled(
                    " Right Click       Pre-select item & open Context Menu",
                    Style::default().fg(ctx.theme.fg_primary),
                )),
                Line::from(Span::styled(
                    " r / p             Refresh snapshot / Run git status probe",
                    Style::default().fg(ctx.theme.fg_primary),
                )),
                Line::from(Span::styled(
                    " Ctrl+G            Toggle Unicode / NerdFont glyphs",
                    Style::default().fg(ctx.theme.fg_primary),
                )),
                Line::from(Span::styled(
                    " q / Ctrl+C        Quit cleanly",
                    Style::default().fg(ctx.theme.fg_primary),
                )),
            ];
            frame.render_widget(Paragraph::new(lines), inner);
        }
        PopupKind::BranchesQuickView => {
            let mut lines = vec![Line::from(vec![
                Span::styled(" Search: ", Style::default().fg(ctx.theme.fg_muted)),
                Span::styled(
                    if query.is_empty() {
                        "<type to filter>"
                    } else {
                        query
                    },
                    Style::default().fg(ctx.theme.fg_bright),
                ),
            ])];

            if let Some(snap) = &state.repo_snapshot {
                let q_low = query.to_ascii_lowercase();
                let filtered: Vec<_> = snap
                    .refs
                    .iter()
                    .filter(|r| matches!(r.kind, RefKind::LocalBranch | RefKind::RemoteBranch))
                    .filter(|r| q_low.is_empty() || r.name.to_ascii_lowercase().contains(&q_low))
                    .collect();

                let sel = if filtered.is_empty() {
                    0
                } else {
                    selected.min(filtered.len() - 1)
                };

                for (idx, r) in filtered
                    .iter()
                    .take(inner.height.saturating_sub(2) as usize)
                    .enumerate()
                {
                    let is_sel = idx == sel;
                    let bg = if is_sel {
                        ctx.theme.bg_selection_focused
                    } else {
                        ctx.theme.bg_elevated
                    };
                    let tag = match r.kind {
                        RefKind::LocalBranch if r.is_head => ctx.glyph_mode.head_icon(),
                        RefKind::LocalBranch => ctx.glyph_mode.branch_icon(),
                        _ => ctx.glyph_mode.remote_icon(),
                    };
                    let prefix = format!(" {tag} ");
                    let max_name = (inner.width as usize).saturating_sub(prefix.width());
                    lines.push(
                        Line::from(Span::styled(
                            format!("{prefix}{}", truncate_to_width(&r.name, max_name)),
                            Style::default().fg(ctx.theme.fg_primary),
                        ))
                        .style(Style::default().bg(bg)),
                    );
                }
            }
            frame.render_widget(Paragraph::new(lines), inner);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn render_context_menu(
    frame: &mut Frame<'_>,
    screen: Rect,
    ctx: &mut RenderContext<'_>,
    anchor: (u16, u16),
    items: &[ContextMenuItem],
    selected: usize,
    depth: u8,
    z: u8,
) {
    if items.is_empty() || screen.width < 12 || screen.height < 4 {
        return;
    }

    let rect = context_menu_rect(screen, anchor, items);
    frame.render_widget(Clear, rect);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(ctx.theme.border_focused))
        .style(Style::default().bg(ctx.theme.bg_elevated));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);

    for (idx, item) in items.iter().take(inner.height as usize).enumerate() {
        let row_rect = Rect::new(inner.x, inner.y.saturating_add(idx as u16), inner.width, 1);
        let is_sel = idx == selected;
        let bg = if is_sel {
            ctx.theme.bg_selection_focused
        } else {
            ctx.theme.bg_elevated
        };

        let right_label = match &item.kind {
            ContextMenuEntryKind::Submenu(_) => "▸",
            ContextMenuEntryKind::Action(_) => item.shortcut.as_deref().unwrap_or(""),
        };
        let avail = (inner.width as usize).saturating_sub(right_label.width() + 3);
        let label = truncate_to_width(&item.label, avail);
        let pad = (inner.width as usize).saturating_sub(label.width() + right_label.width() + 2);

        let line = Line::from(vec![
            Span::styled(
                format!(" {label}"),
                Style::default().fg(if is_sel {
                    ctx.theme.fg_bright
                } else {
                    ctx.theme.fg_primary
                }),
            ),
            Span::raw(" ".repeat(pad)),
            Span::styled(
                format!("{right_label} "),
                Style::default().fg(ctx.theme.fg_muted),
            ),
        ]);

        frame.render_widget(Paragraph::new(line).style(Style::default().bg(bg)), row_rect);
        ctx.hit_registry.register(
            row_rect,
            z + 1,
            HitTarget::MenuItem {
                menu_depth: depth,
                item_idx: idx,
            },
        );
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
    let x = screen.x.saturating_add(screen.width.saturating_sub(width) / 2);
    let y = screen.y.saturating_add(screen.height.saturating_sub(height) / 2);
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
            btn0_rect.x.saturating_add(btn0_rect.width).saturating_add(2),
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
            Style::default().fg(ctx.theme.fg_primary).bg(ctx.theme.bg_panel)
        };
        let s1 = if selected_button == ModalButton::Cancel {
            Style::default()
                .fg(ctx.theme.fg_bright)
                .bg(ctx.theme.border_focused)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(ctx.theme.fg_primary).bg(ctx.theme.bg_panel)
        };

        frame.render_widget(Paragraph::new(Span::styled(btn0, s0)), btn0_rect);
        frame.render_widget(Paragraph::new(Span::styled(btn1, s1)), btn1_rect);
        ctx.hit_registry
            .register(btn0_rect, z + 1, HitTarget::ModalButton(ModalButton::Confirm));
        ctx.hit_registry
            .register(btn1_rect, z + 1, HitTarget::ModalButton(ModalButton::Cancel));
    }
}
