//! Built-in three-pane conflict merge resolver (`Ours` | `Result` | `Theirs`).

use jbgit_core::ConflictChoice;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};
use unicode_width::UnicodeWidthStr;

use crate::app::{AppState, RenderContext};
use crate::hit_test::{ConflictMergeButtonKind, HitTarget};
use crate::views::overlays::draw_shadow;
use crate::views::truncate_to_width;

pub fn render_conflict_merge_modal(
    frame: &mut Frame<'_>,
    screen: Rect,
    state: &AppState,
    ctx: &mut RenderContext<'_>,
    z: u8,
) {
    if screen.width < 30 || screen.height < 10 {
        return;
    }
    let width = screen.width.saturating_sub(4).min(136);
    let height = screen.height.saturating_sub(2).min(34);
    let rect = Rect::new(
        screen.x + screen.width.saturating_sub(width) / 2,
        screen.y + screen.height.saturating_sub(height) / 2,
        width,
        height,
    );
    draw_shadow(frame, screen, rect);
    frame.render_widget(Clear, rect);

    let Some(cm) = &state.conflict_merge else {
        return;
    };
    let title = match &cm.doc {
        Some(doc) => format!(
            " Resolve Conflicts — {} ({} unresolved / {}) ",
            cm.path,
            doc.unresolved_count(),
            doc.conflict_count
        ),
        None => format!(" Resolve Conflicts — {} (Loading…) ", cm.path),
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
        .border_style(Style::default().fg(ctx.theme.status_modified))
        .style(Style::default().bg(ctx.theme.bg_elevated));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    ctx.hit_registry
        .register(rect, z + 1, HitTarget::DialogBody);

    if inner.height < 4 {
        return;
    }

    let Some(doc) = &cm.doc else {
        frame.render_widget(
            Paragraph::new(Span::styled(
                " Loading conflict blocks…",
                Style::default().fg(ctx.theme.fg_muted),
            )),
            inner,
        );
        return;
    };

    let col_w = (inner.width.saturating_sub(2)) / 3;
    let right_w = inner.width.saturating_sub(col_w * 2 + 2);
    let header_rect = Rect::new(inner.x, inner.y, inner.width, 1);
    let active_block = doc.block(cm.selected_block);
    let ours_title = active_block.map_or("Ours", |b| b.ours_label.as_str());
    let theirs_title = active_block.map_or("Theirs", |b| b.theirs_label.as_str());
    let pad_col = |t: &str, w: u16| {
        let trunc = truncate_to_width(t, w as usize);
        let pad = (w as usize).saturating_sub(trunc.width());
        format!("{trunc}{}", " ".repeat(pad))
    };
    let hdr = Line::from(vec![
        Span::styled(
            pad_col(&format!(" Ours ({ours_title})"), col_w),
            Style::default()
                .fg(ctx.theme.status_added)
                .bg(ctx.theme.bg_panel)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("│", Style::default().fg(ctx.theme.border_splitter)),
        Span::styled(
            pad_col(" Merged Result", col_w),
            Style::default()
                .fg(ctx.theme.fg_bright)
                .bg(ctx.theme.bg_panel)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("│", Style::default().fg(ctx.theme.border_splitter)),
        Span::styled(
            pad_col(&format!(" Theirs ({theirs_title})"), right_w),
            Style::default()
                .fg(ctx.theme.fg_accent_link)
                .bg(ctx.theme.bg_panel)
                .add_modifier(Modifier::BOLD),
        ),
    ]);
    frame.render_widget(Paragraph::new(hdr), header_rect);

    let body_h = inner.height.saturating_sub(3) as usize;
    let rows = doc.three_pane_rows();
    let scroll = cm.scroll.min(rows.len().saturating_sub(body_h.max(1)));
    for (vis, row) in rows.iter().skip(scroll).take(body_h).enumerate() {
        let y = inner.y + 1 + vis as u16;
        let line_rect = Rect::new(inner.x, y, inner.width, 1);
        let is_active = row.block_idx == Some(cm.selected_block);
        let (ours_bg, mid_bg, theirs_bg) = match row.choice {
            None => (ctx.theme.bg_canvas, ctx.theme.bg_canvas, ctx.theme.bg_canvas),
            Some(ConflictChoice::Unresolved) => {
                let hi = if is_active {
                    ctx.theme.bg_selection_focused
                } else {
                    ctx.theme.diff_mod_bg
                };
                (ctx.theme.diff_add_bg, hi, ctx.theme.diff_del_bg)
            }
            Some(_) => {
                let hi = if is_active {
                    ctx.theme.bg_selection_unfocused
                } else {
                    ctx.theme.diff_add_bg
                };
                (ctx.theme.diff_add_bg, hi, ctx.theme.diff_del_bg)
            }
        };
        let mid_fg = match row.choice {
            Some(ConflictChoice::Unresolved) => ctx.theme.status_conflicted,
            Some(_) => ctx.theme.fg_bright,
            None => ctx.theme.fg_primary,
        };
        let marker = if is_active { "▎" } else { " " };
        let line = Line::from(vec![
            Span::styled(
                pad_col(&format!("{marker}{}", row.ours), col_w),
                Style::default().fg(ctx.theme.fg_primary).bg(ours_bg),
            ),
            Span::styled("│", Style::default().fg(ctx.theme.border_splitter)),
            Span::styled(
                pad_col(&format!(" {}", row.merged), col_w),
                Style::default().fg(mid_fg).bg(mid_bg),
            ),
            Span::styled("│", Style::default().fg(ctx.theme.border_splitter)),
            Span::styled(
                pad_col(&format!(" {}", row.theirs), right_w),
                Style::default().fg(ctx.theme.fg_primary).bg(theirs_bg),
            ),
        ]);
        frame.render_widget(Paragraph::new(line), line_rect);
        if let Some(bid) = row.block_idx {
            ctx.hit_registry
                .register(line_rect, z + 2, HitTarget::ConflictMergeBlock(bid));
        }
    }

    let hint_y = inner.bottom().saturating_sub(2);
    frame.render_widget(
        Paragraph::new(Span::styled(
            truncate_to_width(
                " n/p conflict · ↑↓ scroll · y Ours · t Theirs · b Both · Y/T All · m mergetool · e $EDITOR · Enter Save & Stage",
                inner.width as usize,
            ),
            Style::default().fg(ctx.theme.fg_muted),
        ))
        .style(Style::default().bg(ctx.theme.bg_panel)),
        Rect::new(inner.x, hint_y, inner.width, 1),
    );

    let btn_y = inner.bottom().saturating_sub(1);
    let buttons = [
        (" [y] Ours ", ConflictMergeButtonKind::AcceptOurs, false),
        (" [t] Theirs ", ConflictMergeButtonKind::AcceptTheirs, false),
        (" [b] Both ", ConflictMergeButtonKind::AcceptBoth, false),
        (" [Y] All Ours ", ConflictMergeButtonKind::AcceptAllOurs, false),
        (" [T] All Theirs ", ConflictMergeButtonKind::AcceptAllTheirs, false),
        (" [m] mergetool ", ConflictMergeButtonKind::Mergetool, false),
        (" [e] $EDITOR ", ConflictMergeButtonKind::ExternalEditor, false),
        (
            if doc.unresolved_count() == 0 {
                " Save & Stage "
            } else {
                " Save "
            },
            ConflictMergeButtonKind::SaveAndStage,
            true,
        ),
        (" Close ", ConflictMergeButtonKind::Cancel, false),
    ];
    let mut bx = inner.x + 1;
    for (label, kind, primary) in buttons {
        let w = label.width() as u16;
        if bx + w > inner.right() {
            break;
        }
        let r = Rect::new(bx, btn_y, w, 1);
        let style = if primary {
            Style::default()
                .fg(ctx.theme.fg_bright)
                .bg(ctx.theme.border_focused)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
                .fg(ctx.theme.fg_primary)
                .bg(ctx.theme.bg_panel)
        };
        frame.render_widget(Paragraph::new(Span::styled(label, style)), r);
        ctx.hit_registry
            .register(r, z + 2, HitTarget::ConflictMergeButton(kind));
        bx += w + 1;
    }
}
