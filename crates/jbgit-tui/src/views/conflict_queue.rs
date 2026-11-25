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
use ratatui::widgets::{Block, BorderType, Borders, Paragraph};
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
    let compact = screen.width <= 84 || screen.height <= 24;
    let rect = if compact {
        crate::views::compact_modal_rect(screen)
    } else {
        let width = screen.width.saturating_sub(6).min(118);
        let height = screen.height.saturating_sub(4).min(28);
        crate::views::centered_modal_rect(screen, width, height)
    };
    if rect.width < screen.width {
        draw_shadow(frame, screen, rect);
    }
    crate::views::clear_modal_overlay(frame, rect);

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

    // Precompute wrapped footer button rows so narrow viewports (`50x16`) keep all
    // actions (including Revert / Mergetool / Close) visible and clickable.
    let buttons = [
        QueueButton::new(
            i18n.t("conflict_queue.btn_merge"),
            ConflictQueueButtonKind::Merge,
            true,
        ),
        QueueButton::new(
            i18n.t("conflict_queue.btn_accept_yours"),
            ConflictQueueButtonKind::AcceptYours,
            false,
        ),
        QueueButton::new(
            i18n.t("conflict_queue.btn_accept_theirs"),
            ConflictQueueButtonKind::AcceptTheirs,
            false,
        ),
        QueueButton::new(
            i18n.t("conflict_queue.btn_revert"),
            ConflictQueueButtonKind::RevertResolved,
            false,
        ),
        QueueButton::new(
            i18n.t("conflict_queue.btn_mergetool"),
            ConflictQueueButtonKind::Mergetool,
            false,
        ),
        QueueButton::new(
            i18n.t("conflict_queue.btn_close"),
            ConflictQueueButtonKind::Close,
            false,
        ),
    ];
    let max_btn_rows = (inner.height.saturating_sub(3) as usize).clamp(1, 2);
    let button_rows = layout_queue_buttons(
        &buttons,
        inner.width.saturating_sub(2) as usize,
        max_btn_rows,
    );
    let btn_row_count = button_rows.len().max(1) as u16;
    let footer_reserved: u16 = 1 + btn_row_count;

    let body_h = inner.height.saturating_sub(1 + footer_reserved) as usize;
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
                Span::styled(
                    "│",
                    Style::default().fg(ctx.theme.border_splitter).bg(row_bg),
                ),
                Span::styled(
                    pad(&yours_cell, side_w),
                    Style::default().fg(side_fg(entry.yours_status)).bg(row_bg),
                ),
                Span::styled(
                    "│",
                    Style::default().fg(ctx.theme.border_splitter).bg(row_bg),
                ),
                Span::styled(
                    pad(&theirs_cell, side_w),
                    Style::default().fg(side_fg(entry.theirs_status)).bg(row_bg),
                ),
                Span::styled(
                    "│",
                    Style::default().fg(ctx.theme.border_splitter).bg(row_bg),
                ),
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

    let hint_y = inner.bottom().saturating_sub(footer_reserved);
    frame.render_widget(
        Paragraph::new(Span::styled(
            truncate_to_width(i18n.t("conflict_queue.hint"), inner.width as usize),
            Style::default().fg(ctx.theme.fg_muted),
        ))
        .style(Style::default().bg(ctx.theme.bg_panel)),
        Rect::new(inner.x, hint_y, inner.width, 1),
    );

    for (row_idx, row_buttons) in button_rows.iter().enumerate() {
        let btn_y = hint_y + 1 + row_idx as u16;
        if btn_y >= inner.bottom() {
            break;
        }
        let mut bx = inner.x + 1;
        for btn in row_buttons {
            let w = btn.label.width() as u16;
            if bx + w > inner.right() {
                break;
            }
            let r = Rect::new(bx, btn_y, w, 1);
            let hovered = ctx.hover_target == Some(&HitTarget::ConflictQueueButton(btn.kind));
            let style = if btn.primary {
                let base = Style::default()
                    .fg(ctx.theme.fg_bright)
                    .bg(ctx.theme.border_focused)
                    .add_modifier(Modifier::BOLD);
                if hovered {
                    base.add_modifier(Modifier::REVERSED)
                } else {
                    base
                }
            } else if hovered {
                Style::default()
                    .fg(ctx.theme.fg_bright)
                    .bg(ctx.theme.bg_hover)
            } else {
                Style::default()
                    .fg(ctx.theme.fg_primary)
                    .bg(ctx.theme.bg_panel)
            };
            frame.render_widget(Paragraph::new(Span::styled(btn.label.as_str(), style)), r);
            ctx.hit_registry
                .register(r, z + 2, HitTarget::ConflictQueueButton(btn.kind));
            bx += w + 1;
        }
    }
}

#[derive(Clone)]
struct QueueButton {
    label: String,
    kind: ConflictQueueButtonKind,
    primary: bool,
}

impl QueueButton {
    fn new(label: impl Into<String>, kind: ConflictQueueButtonKind, primary: bool) -> Self {
        Self {
            label: label.into(),
            kind,
            primary,
        }
    }
}

/// Packs conflict-queue footer buttons into up to `max_rows` rows for `avail_w`,
/// ensuring `Merge` and `Close` remain reachable even on minimum-size (`40x10` / `50x16`)
/// viewports.
fn layout_queue_buttons(
    buttons: &[QueueButton],
    avail_w: usize,
    max_rows: usize,
) -> Vec<Vec<QueueButton>> {
    if buttons.is_empty() || avail_w == 0 {
        return Vec::new();
    }
    let mut rows: Vec<Vec<QueueButton>> = vec![Vec::new()];
    let mut cur_w = 0usize;
    for btn in buttons {
        let w = btn.label.width();
        let needed = if cur_w == 0 { w } else { w + 1 };
        if cur_w > 0 && cur_w + needed > avail_w {
            rows.push(vec![btn.clone()]);
            cur_w = w;
        } else {
            rows.last_mut().unwrap().push(btn.clone());
            cur_w += needed;
        }
    }
    if rows.len() <= max_rows.max(1) {
        return rows;
    }

    // Ultra-narrow fallback: pin Merge and Close on the final row so primary workflow
    // and modal exit stay accessible while packing remaining actions into available space.
    let mut pinned = Vec::new();
    let mut rest = Vec::new();
    for btn in buttons {
        if matches!(
            btn.kind,
            ConflictQueueButtonKind::Merge | ConflictQueueButtonKind::Close
        ) {
            pinned.push(btn.clone());
        } else {
            rest.push(btn.clone());
        }
    }
    if max_rows <= 1 {
        let mut row = Vec::new();
        let mut w_used = 0usize;
        let pinned_w: usize = pinned
            .iter()
            .enumerate()
            .map(|(i, b)| b.label.width() + usize::from(i > 0))
            .sum();
        for btn in rest {
            let w = btn.label.width();
            let extra = if w_used == 0 { w } else { w + 1 };
            let with_pinned = w_used + extra + if pinned_w > 0 { 1 + pinned_w } else { 0 };
            if with_pinned <= avail_w {
                row.push(btn);
                w_used += extra;
            }
        }
        row.extend(pinned);
        return vec![row];
    }

    let mut row1 = Vec::new();
    let mut row1_w = 0usize;
    let mut remaining = Vec::new();
    for btn in rest {
        let w = btn.label.width();
        let needed = if row1_w == 0 { w } else { w + 1 };
        if row1_w + needed <= avail_w {
            row1.push(btn);
            row1_w += needed;
        } else {
            remaining.push(btn);
        }
    }
    let pinned_w: usize = pinned
        .iter()
        .enumerate()
        .map(|(i, b)| b.label.width() + usize::from(i > 0))
        .sum();
    let mut row2 = Vec::new();
    let mut row2_w = 0usize;
    for btn in remaining {
        let w = btn.label.width();
        let extra = if row2_w == 0 { w } else { w + 1 };
        let with_pinned = row2_w + extra + if pinned_w > 0 { 1 + pinned_w } else { 0 };
        if with_pinned <= avail_w {
            row2.push(btn);
            row2_w += extra;
        }
    }
    row2.extend(pinned);
    if row1.is_empty() {
        vec![row2]
    } else {
        vec![row1, row2]
    }
}
