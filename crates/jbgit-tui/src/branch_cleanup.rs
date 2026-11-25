//! `ModalLayer::BranchCleanup` (`CleanupBranchesDialog`): scans non-current,
//! non-protected local branches merged into `HEAD` or a chosen base branch
//! (highlighting `[gone]` upstreams), lets the user toggle checkboxes, and
//! batch-deletes the selected set via `GitOp::Sequence` (with one-click Undo).

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use jbgit_core::{CleanupBranchCandidate, GitOp, RepoSnapshot};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph};
use unicode_width::UnicodeWidthStr;

use crate::app::{Action, AppState, RenderContext, ScrollPane, ScrollViewport};
use crate::hit_test::{BranchCleanupButtonKind, HitTarget};
use crate::views::truncate_to_width;

fn centered_rect(screen: Rect, w: u16, h: u16) -> Rect {
    let w = w
        .min(screen.width.saturating_sub(2))
        .max(10.min(screen.width));
    let h = h
        .min(screen.height.saturating_sub(2))
        .max(5.min(screen.height));
    Rect::new(
        screen.x + screen.width.saturating_sub(w) / 2,
        screen.y + screen.height.saturating_sub(h) / 2,
        w,
        h,
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchCleanupRow {
    pub candidate: CleanupBranchCandidate,
    pub selected: bool,
}

#[derive(Debug, Clone)]
pub struct BranchCleanupState {
    pub base_ref: String,
    pub base_choices: Vec<String>,
    pub rows: Vec<BranchCleanupRow>,
    pub selected_idx: usize,
    pub scroll: ScrollViewport,
}

impl BranchCleanupState {
    pub fn for_snapshot(snap: &RepoSnapshot, extra_protected: &[String]) -> Self {
        let mut base_choices = vec!["HEAD".to_string()];
        for cand in ["main", "master", "develop", "development", "trunk"] {
            if snap.local_branch(cand).is_some()
                && snap.head_branch.as_deref() != Some(cand)
                && !base_choices.iter().any(|b| b == cand)
            {
                base_choices.push(cand.to_string());
            }
        }
        for r in &snap.refs {
            if r.kind == jbgit_core::RefKind::LocalBranch
                && !r.is_head
                && (jbgit_core::is_protected_branch(&r.name)
                    || jbgit_core::matches_protected_branch_patterns(&r.name, extra_protected))
                && !base_choices.iter().any(|b| b == &r.name)
            {
                base_choices.push(r.name.clone());
            }
        }
        let mut state = Self {
            base_ref: "HEAD".to_string(),
            base_choices,
            rows: Vec::new(),
            selected_idx: 0,
            scroll: ScrollViewport::new(),
        };
        state.refresh(snap, extra_protected);
        state
    }

    pub fn refresh(&mut self, snap: &RepoSnapshot, extra_protected: &[String]) {
        let cands = snap.scan_cleanup_branches(&self.base_ref, extra_protected);
        self.rows = cands
            .into_iter()
            .map(|candidate| {
                let selected = candidate.merged_into_base;
                BranchCleanupRow {
                    candidate,
                    selected,
                }
            })
            .collect();
        self.selected_idx = self.selected_idx.min(self.rows.len().saturating_sub(1));
        self.scroll.reset();
    }

    pub fn cycle_base(&mut self, snap: &RepoSnapshot, extra_protected: &[String]) {
        if self.base_choices.len() <= 1 {
            return;
        }
        let pos = self
            .base_choices
            .iter()
            .position(|b| b == &self.base_ref)
            .unwrap_or(0);
        self.base_ref = self.base_choices[(pos + 1) % self.base_choices.len()].clone();
        self.refresh(snap, extra_protected);
    }

    pub fn selected_count(&self) -> usize {
        self.rows.iter().filter(|r| r.selected).count()
    }

    pub fn toggle_current(&mut self) {
        if let Some(row) = self.rows.get_mut(self.selected_idx) {
            row.selected = !row.selected;
        }
    }

    pub fn toggle_all(&mut self) {
        let all_on = !self.rows.is_empty() && self.rows.iter().all(|r| r.selected);
        for row in &mut self.rows {
            row.selected = !all_on;
        }
    }

    pub fn move_by(&mut self, delta: isize) {
        if self.rows.is_empty() {
            return;
        }
        let next = (self.selected_idx as isize + delta).clamp(0, self.rows.len() as isize - 1);
        self.selected_idx = next as usize;
        self.ensure_selected_visible();
    }

    pub fn select(&mut self, idx: usize) {
        if self.rows.is_empty() {
            return;
        }
        self.selected_idx = idx.min(self.rows.len() - 1);
        self.ensure_selected_visible();
    }

    pub fn ensure_selected_visible(&mut self) {
        let h = self.scroll.viewport_height().unwrap_or(1).max(1);
        let cur = self.scroll.offset();
        let next = if self.selected_idx < cur {
            self.selected_idx
        } else if self.selected_idx >= cur + h {
            self.selected_idx + 1 - h
        } else {
            cur
        };
        let max_scroll = self.rows.len().saturating_sub(h);
        self.scroll.set_offset(next.min(max_scroll));
    }

    /// Builds the batch `GitOp::Sequence` deleting all checked branches.
    pub fn delete_op(&self) -> Option<GitOp> {
        let ops: Vec<GitOp> = self
            .rows
            .iter()
            .filter(|r| r.selected)
            .map(|r| GitOp::DeleteBranch {
                name: r.candidate.name.clone(),
                force: !r.candidate.merged_into_head,
                tip: r.candidate.tip,
            })
            .collect();
        (!ops.is_empty()).then_some(GitOp::Sequence(ops))
    }
}

pub fn route_branch_cleanup_key(key: &KeyEvent, _state: &AppState) -> Vec<Action> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    match key.code {
        KeyCode::Esc => vec![Action::PopModal],
        KeyCode::Up | KeyCode::Char('k') if !ctrl && !alt => vec![Action::BranchCleanupMove(-1)],
        KeyCode::Down | KeyCode::Char('j') if !ctrl && !alt => vec![Action::BranchCleanupMove(1)],
        KeyCode::PageUp => vec![Action::BranchCleanupMove(-8)],
        KeyCode::PageDown => vec![Action::BranchCleanupMove(8)],
        KeyCode::Home => vec![Action::BranchCleanupSelect(0)],
        KeyCode::End => vec![Action::BranchCleanupSelect(usize::MAX)],
        KeyCode::Char(' ') if !ctrl && !alt => vec![Action::BranchCleanupToggle],
        KeyCode::Char('a') if !ctrl && !alt => vec![Action::BranchCleanupToggleAll],
        KeyCode::Char('b') | KeyCode::Tab if !ctrl && !alt => vec![Action::BranchCleanupCycleBase],
        KeyCode::Enter | KeyCode::Char('d') if !ctrl && !alt => vec![Action::BranchCleanupConfirm],
        _ => Vec::new(),
    }
}

pub fn render_branch_cleanup(
    frame: &mut Frame<'_>,
    screen: Rect,
    bc: &BranchCleanupState,
    ctx: &mut RenderContext<'_>,
    z: u8,
) {
    let w = (screen.width.saturating_sub(6)).clamp(48, 86);
    let list_rows = bc.rows.len().clamp(4, 14) as u16;
    let h = (list_rows + 7).min(screen.height.saturating_sub(2)).max(9);
    let area = centered_rect(screen, w, h);
    crate::views::clear_modal_overlay(frame, area);
    let title = ctx.t("dialog.cleanup_branches.title");
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(ctx.theme.border_focused))
        .style(Style::default().bg(ctx.theme.bg_elevated))
        .title(Span::styled(
            format!(" {title} "),
            Style::default()
                .fg(ctx.theme.fg_primary)
                .add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    ctx.hit_registry.register(area, z, HitTarget::DialogBody);
    if inner.width < 12 || inner.height < 5 {
        return;
    }

    // Header row: base branch + count summary.
    let sel_s = bc.selected_count().to_string();
    let tot_s = bc.rows.len().to_string();
    let header_text = ctx.i18n.map_or_else(
        || format!("Base: {}  ({sel_s}/{tot_s} selected)", bc.base_ref),
        |i| {
            i.t_fmt(
                "dialog.cleanup_branches.header",
                &[
                    ("base", &bc.base_ref),
                    ("selected", &sel_s),
                    ("total", &tot_s),
                ],
            )
        },
    );
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            truncate_to_width(&header_text, inner.width as usize),
            Style::default().fg(ctx.theme.fg_muted),
        ))),
        Rect::new(inner.x, inner.y, inner.width, 1),
    );

    let list_y = inner.y + 2;
    let footer_h = 2u16;
    let list_h = inner.height.saturating_sub(2 + footer_h).max(1);
    let list_area = Rect::new(inner.x, list_y, inner.width, list_h);

    let max_scroll = bc.rows.len().saturating_sub(list_h as usize);
    ctx.record_scroll_bound(ScrollPane::BranchCleanup, list_h as usize, max_scroll);
    let offset = bc.scroll.offset().min(max_scroll);

    if bc.rows.is_empty() {
        let empty = ctx.t("dialog.cleanup_branches.empty");
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                truncate_to_width(empty, list_area.width as usize),
                Style::default().fg(ctx.theme.fg_muted),
            ))),
            list_area,
        );
    } else {
        let has_sb = max_scroll > 0 && list_area.width > 6;
        let row_w = if has_sb {
            list_area.width.saturating_sub(1)
        } else {
            list_area.width
        };
        for (vis, (idx, row)) in bc
            .rows
            .iter()
            .enumerate()
            .skip(offset)
            .take(list_h as usize)
            .enumerate()
        {
            let y = list_area.y + vis as u16;
            let r = Rect::new(list_area.x, y, row_w, 1);
            let active = idx == bc.selected_idx;
            let bg = if active {
                ctx.theme.bg_selection_focused
            } else {
                ctx.theme.bg_elevated
            };
            let check = if row.selected { "[✓] " } else { "[ ] " };
            let mut badge = String::new();
            if row.candidate.merged_into_base {
                badge.push_str(ctx.t("dialog.cleanup_branches.badge_merged"));
            } else {
                badge.push_str(ctx.t("dialog.cleanup_branches.badge_unmerged"));
            }
            if row.candidate.upstream_gone {
                badge.push_str(" [gone]");
            }
            let up = row
                .candidate
                .upstream
                .as_deref()
                .map(|u| format!(" → {u}"))
                .unwrap_or_default();
            let badge_style = if row.candidate.upstream_gone && !row.candidate.merged_into_base {
                Style::default().fg(ctx.theme.status_modified)
            } else if row.candidate.upstream_gone {
                Style::default().fg(ctx.theme.fg_accent_link)
            } else {
                Style::default().fg(ctx.theme.status_added)
            };
            let used = check.width() + badge.width() + 2;
            let name_budget = (row_w as usize).saturating_sub(used).max(8);
            let name_txt = truncate_to_width(&format!("{}{up}", row.candidate.name), name_budget);
            let pad = (row_w as usize)
                .saturating_sub(check.width() + name_txt.width() + badge.width() + 1);
            let line = Line::from(vec![
                Span::styled(
                    check,
                    Style::default()
                        .fg(if row.selected {
                            ctx.theme.fg_accent_link
                        } else {
                            ctx.theme.fg_muted
                        })
                        .bg(bg),
                ),
                Span::styled(
                    name_txt,
                    Style::default()
                        .fg(ctx.theme.fg_primary)
                        .bg(bg)
                        .add_modifier(if active {
                            Modifier::BOLD
                        } else {
                            Modifier::empty()
                        }),
                ),
                Span::styled(" ".repeat(pad.max(1)), Style::default().bg(bg)),
                Span::styled(badge, badge_style.bg(bg)),
            ]);
            frame.render_widget(Paragraph::new(line).style(Style::default().bg(bg)), r);
            ctx.hit_registry
                .register(r, z + 1, HitTarget::BranchCleanupRow(idx));
        }
        if has_sb {
            let sb = Rect::new(list_area.right() - 1, list_area.y, 1, list_area.height);
            let thumb = ((list_h as usize * list_h as usize) / bc.rows.len().max(1)).max(1);
            let top = (offset * (list_h as usize).saturating_sub(thumb))
                .checked_div(max_scroll)
                .unwrap_or(0);
            for dy in 0..list_h as usize {
                let ch = if dy >= top && dy < top + thumb {
                    "█"
                } else {
                    "░"
                };
                frame.render_widget(
                    Paragraph::new(Span::styled(ch, Style::default().fg(ctx.theme.fg_muted))),
                    Rect::new(sb.x, sb.y + dy as u16, 1, 1),
                );
            }
        }
    }

    // Buttons + hint row at bottom.
    let btn_y = inner.bottom().saturating_sub(2);
    let can_delete = bc.selected_count() > 0;
    let del_lbl = ctx.i18n.map_or_else(
        || format!(" Delete ({sel_s}) "),
        |i| i.t_fmt("dialog.cleanup_branches.delete_btn", &[("count", &sel_s)]),
    );
    let base_lbl = ctx.i18n.map_or_else(
        || format!(" Base: {} ", bc.base_ref),
        |i| {
            i.t_fmt(
                "dialog.cleanup_branches.base_btn",
                &[("base", &bc.base_ref)],
            )
        },
    );
    let close_lbl = format!(" {} ", ctx.t("common.close"));

    let mut x = inner.x;
    let buttons = [
        (
            del_lbl,
            BranchCleanupButtonKind::Delete,
            if can_delete {
                Style::default()
                    .fg(ctx.theme.bg_canvas)
                    .bg(ctx.theme.status_conflicted)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
                    .fg(ctx.theme.fg_muted)
                    .bg(ctx.theme.bg_panel)
            },
        ),
        (
            base_lbl,
            BranchCleanupButtonKind::CycleBase,
            Style::default()
                .fg(ctx.theme.fg_primary)
                .bg(ctx.theme.bg_panel),
        ),
        (
            close_lbl,
            BranchCleanupButtonKind::Close,
            Style::default()
                .fg(ctx.theme.fg_primary)
                .bg(ctx.theme.bg_panel),
        ),
    ];
    for (lbl, kind, style) in buttons {
        let bw = lbl.width() as u16;
        if x + bw > inner.right() {
            break;
        }
        let r = Rect::new(x, btn_y, bw, 1);
        frame.render_widget(Paragraph::new(Span::styled(lbl, style)), r);
        ctx.hit_registry
            .register(r, z + 1, HitTarget::BranchCleanupButton(kind));
        x += bw + 1;
    }

    let hint = ctx.t("dialog.cleanup_branches.hint");
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            truncate_to_width(hint, inner.width as usize),
            Style::default().fg(ctx.theme.fg_muted),
        ))),
        Rect::new(inner.x, inner.bottom().saturating_sub(1), inner.width, 1),
    );
}
