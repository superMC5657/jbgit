//! Multi-file Conflict Queue modal (`ModalLayer::ConflictQueue`):
//! displays all conflicted files with columns `Path`, `Yours (<branch>)`,
//! `Theirs (<branch>)`, and `Status` (`Unresolved` / `Resolved`), supports
//! single/multi-selection batch `Accept Yours`/`Accept Theirs`, launching the
//! three-pane merger or external mergetool, and reverting already-resolved
//! files via `git checkout -m`.

use jbgit_core::ConflictSideStatus;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};
use unicode_width::UnicodeWidthStr;

use crate::app::{AppState, ConflictQueueState, RenderContext, ScrollPane};
use crate::hit_test::{ConflictQueueButtonKind, HitTarget};
use crate::i18n::I18n;
use crate::views::overlays::draw_shadow;
use crate::views::truncate_to_width;

fn side_status_label(i18n: &I18n, st: ConflictSideStatus) -> &str {
    match st {
        ConflictSideStatus::Unchanged => i18n.t("conflict_queue.side_unchanged"),
        ConflictSideStatus::Modified => i18n.t("conflict_queue.side_modified"),
        ConflictSideStatus::Added => i18n.t("conflict_queue.side_added"),
        ConflictSideStatus::Deleted => i18n.t("conflict_queue.side_deleted"),
    }
}

pub fn render_conflict_queue_modal(
    frame: &mut Frame<'_>,
    screen: Rect,
    state: &AppState,
    cq: &ConflictQueueState,
    ctx: &mut RenderContext<'_>,
    z: u8,
) {
    if screen.width < 40 || screen.height < 10 {
        return;
    }
    let width = screen.width.saturating_sub(6).min(118);
    let height = screen.height.saturating_sub(4).min(28);
    let rect = Rect::new(
        screen.x + screen.width.saturating_sub(width) / 2,
        screen.y + screen.height.saturating_sub(height) / 2,
        width,
        height,
    );
    draw_shadow(frame, screen, rect);
    frame.render_widget(Clear, rect);

    let i18n = &state.i18n;
    let unres_s = cq.unresolved_count().to_string();
    let total_s = cq.entries.len().to_string();
    let title = i18n.t_fmt(
        "conflict_queue.title",
        &[("unresolved", &unres_s), ("total", &total_s)],
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
        .border_style(Style::default().fg(ctx.theme.status_conflicted))
        .style(Style::default().bg(ctx.theme.bg_elevated));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    ctx.hit_registry
        .register(rect, z + 1, HitTarget::DialogBody);

    if inner.height < 4 {
        return;
    }

    let side_w: u16 = 18.min(inner.width / 5);
    let status_w: u16 = 14.min(inner.width / 5);
    let path_w = inner
        .width
        .saturating_sub(side_w.saturating_mul(2) + status_w + 3);

    let pad = |s: &str, w: u16| {
        let trunc = truncate_to_width(s, w as usize);
        let fill = (w as usize).saturating_sub(trunc.width());
        format!("{trunc}{}", " ".repeat(fill))
    };

    let yours_hdr = i18n.t_fmt("conflict_queue.col_yours", &[("branch", &cq.yours_label)]);
    let theirs_hdr = i18n.t_fmt("conflict_queue.col_theirs", &[("branch", &cq.theirs_label)]);
    let header_line = Line::from(vec![
        Span::styled(
            pad(i18n.t("conflict_queue.col_path"), path_w),
            Style::default()
                .fg(ctx.theme.fg_bright)
                .bg(ctx.theme.bg_panel)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("│", Style::default().fg(ctx.theme.border_splitter)),
        Span::styled(
            pad(&yours_hdr, side_w),
            Style::default()
                .fg(ctx.theme.status_added)
                .bg(ctx.theme.bg_panel)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("│", Style::default().fg(ctx.theme.border_splitter)),
        Span::styled(
            pad(&theirs_hdr, side_w),
            Style::default()
                .fg(ctx.theme.fg_accent_link)
                .bg(ctx.theme.bg_panel)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("│", Style::default().fg(ctx.theme.border_splitter)),
        Span::styled(
            pad(i18n.t("conflict_queue.col_status"), status_w),
            Style::default()
                .fg(ctx.theme.fg_bright)
                .bg(ctx.theme.bg_panel)
                .add_modifier(Modifier::BOLD),
        ),
    ]);
    frame.render_widget(
        Paragraph::new(header_line),
        Rect::new(inner.x, inner.y, inner.width, 1),
    );

    let body_h = inner.height.saturating_sub(3) as usize;
    let max_scroll = cq.entries.len().saturating_sub(body_h.max(1));
    ctx.record_scroll_bound(ScrollPane::ConflictQueue, body_h.max(1), max_scroll);
    let scroll = cq.scroll.offset().min(max_scroll);

    if cq.entries.is_empty() {
        frame.render_widget(
            Paragraph::new(Span::styled(
                i18n.t("conflict_queue.empty"),
                Style::default().fg(ctx.theme.fg_muted),
            )),
            Rect::new(inner.x + 1, inner.y + 1, inner.width.saturating_sub(2), 1),
        );
    } else {
        for (vis, (idx, entry)) in cq
            .entries
            .iter()
            .enumerate()
            .skip(scroll)
            .take(body_h)
            .enumerate()
        {
            let y = inner.y + 1 + vis as u16;
            let row_rect = Rect::new(inner.x, y, inner.width, 1);
            let is_cur = idx == cq.selected;
            let is_marked = cq.multi_selected.contains(&entry.path);
            let row_bg = if is_cur {
                ctx.theme.bg_selection_focused
            } else if is_marked {
                ctx.theme.bg_selection_unfocused
            } else {
                ctx.theme.bg_canvas
            };
            let check = if is_marked { "[✓] " } else { "[ ] " };
            let path_cell = format!(" {check}{}", entry.path);
            let yours_cell = format!(" {}", side_status_label(i18n, entry.yours_status));
            let theirs_cell = format!(" {}", side_status_label(i18n, entry.theirs_status));
            let (status_cell, status_fg) = if entry.resolved {
                (
                    format!(" ✓ {}", i18n.t("conflict_queue.status_resolved")),
                    ctx.theme.status_added,
                )
            } else {
                (
                    format!(" ! {}", i18n.t("conflict_queue.status_unresolved")),
                    ctx.theme.status_conflicted,
                )
            };
            let side_fg = |st: ConflictSideStatus| match st {
                ConflictSideStatus::Deleted => ctx.theme.status_conflicted,
                ConflictSideStatus::Added => ctx.theme.status_added,
                ConflictSideStatus::Modified => ctx.theme.status_modified,
                ConflictSideStatus::Unchanged => ctx.theme.fg_muted,
            };
            let line = Line::from(vec![
                Span::styled(
                    pad(&path_cell, path_w),
                    Style::default()
                        .fg(if is_cur {
                            ctx.theme.fg_bright
                        } else {
                            ctx.theme.fg_primary
                        })
                        .bg(row_bg),
                ),
                Span::styled("│", Style::default().fg(ctx.theme.border_splitter).bg(row_bg)),
                Span::styled(
                    pad(&yours_cell, side_w),
                    Style::default().fg(side_fg(entry.yours_status)).bg(row_bg),
                ),
                Span::styled("│", Style::default().fg(ctx.theme.border_splitter).bg(row_bg)),
                Span::styled(
                    pad(&theirs_cell, side_w),
                    Style::default().fg(side_fg(entry.theirs_status)).bg(row_bg),
                ),
                Span::styled("│", Style::default().fg(ctx.theme.border_splitter).bg(row_bg)),
                Span::styled(
                    pad(&status_cell, status_w),
                    Style::default()
                        .fg(status_fg)
                        .bg(row_bg)
                        .add_modifier(Modifier::BOLD),
                ),
            ]);
            frame.render_widget(Paragraph::new(line), row_rect);
            ctx.hit_registry
                .register(row_rect, z + 2, HitTarget::ConflictQueueRow(idx));
        }
    }

    let hint_y = inner.bottom().saturating_sub(2);
    frame.render_widget(
        Paragraph::new(Span::styled(
            truncate_to_width(i18n.t("conflict_queue.hint"), inner.width as usize),
            Style::default().fg(ctx.theme.fg_muted),
        ))
        .style(Style::default().bg(ctx.theme.bg_panel)),
        Rect::new(inner.x, hint_y, inner.width, 1),
    );

    let btn_y = inner.bottom().saturating_sub(1);
    let buttons = [
        (i18n.t("conflict_queue.btn_merge"), ConflictQueueButtonKind::Merge, true),
        (
            i18n.t("conflict_queue.btn_accept_yours"),
            ConflictQueueButtonKind::AcceptYours,
            false,
        ),
        (
            i18n.t("conflict_queue.btn_accept_theirs"),
            ConflictQueueButtonKind::AcceptTheirs,
            false,
        ),
        (
            i18n.t("conflict_queue.btn_revert"),
            ConflictQueueButtonKind::RevertResolved,
            false,
        ),
        (
            i18n.t("conflict_queue.btn_mergetool"),
            ConflictQueueButtonKind::Mergetool,
            false,
        ),
        (
            i18n.t("conflict_queue.btn_close"),
            ConflictQueueButtonKind::Close,
            false,
        ),
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
            .register(r, z + 2, HitTarget::ConflictQueueButton(kind));
        bx += w + 1;
    }
}
