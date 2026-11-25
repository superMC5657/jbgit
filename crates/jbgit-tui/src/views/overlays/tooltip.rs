use jbgit_core::{GroupOptions, RefInfo, group_refs_for_table};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use unicode_width::UnicodeWidthStr;

use crate::app::{AppState, RenderContext};
use crate::hit_test::HitTarget;
use crate::theme::RefBadgeKind;
use crate::views::log_details::signature_badge_and_desc_keys;

/// Renders a hover tooltip when the pointer rests over a Detached HEAD pill or
/// a `+N` overflow pill and no modal layer is open.
pub fn render_hover_tooltip(
    frame: &mut Frame<'_>,
    screen: Rect,
    state: &AppState,
    ctx: &RenderContext<'_>,
) {
    if !state.modal_stack.is_empty() || screen.width < 16 || screen.height < 5 {
        return;
    }
    let (Some(target), Some((hx, hy))) = (ctx.hover_target, state.hover_pos) else {
        return;
    };

    let (title, border_color, lines): (&str, _, Vec<Line<'_>>) = match target {
        HitTarget::CommitRefPill {
            is_detached_head: true,
            ..
        } => {
            let g = ctx.glyph_mode.symbols();
            (
                ctx.t("tooltip.detached_title"),
                ctx.theme.ref_detached.icon,
                vec![
                    Line::from(vec![
                        Span::styled(
                            format!(" {} ", g.detached_head),
                            Style::default().fg(ctx.theme.ref_detached.icon),
                        ),
                        Span::styled(
                            ctx.t("tooltip.detached_line1").to_string(),
                            Style::default().fg(ctx.theme.ref_detached.pill_fg),
                        ),
                    ]),
                    Line::from(Span::styled(
                        ctx.t("tooltip.detached_line2").to_string(),
                        Style::default().fg(ctx.theme.fg_muted),
                    )),
                ],
            )
        }
        HitTarget::CommitRefOverflow {
            visible_row,
            hidden_refs,
        } => {
            let g = ctx.glyph_mode.symbols();
            let mut ref_lines = Vec::new();
            if let Some(snap) = &state.repo_snapshot
                && let Some(commit) = state.log_commit_at(*visible_row)
            {
                let is_head = snap.head_oid == Some(commit.oid);
                let head_marker = is_head.then(|| RefInfo::head_marker(commit.oid));
                let groups = if let Some(cached) = &state.log.graph.cached {
                    group_refs_for_table(
                        cached
                            .refs_for_oid(snap, commit.oid)
                            .chain(head_marker.as_ref()),
                        GroupOptions::default(),
                    )
                } else {
                    group_refs_for_table(
                        snap.refs
                            .iter()
                            .filter(|r| r.target_oid == commit.oid)
                            .chain(head_marker.as_ref()),
                        GroupOptions::default(),
                    )
                };
                for grp in &groups {
                    let badge = RefBadgeKind::from_group(grp);
                    let style = ctx.theme.ref_style(badge);
                    let mut spans = vec![Span::raw(" ")];
                    for (icon, color) in ctx.theme.badge_icons(g, badge).into_iter().flatten() {
                        spans.push(Span::styled(icon, Style::default().fg(color)));
                    }
                    spans.push(Span::styled(
                        format!(" {}", grp.label),
                        Style::default().fg(style.pill_fg),
                    ));
                    ref_lines.push(Line::from(spans));
                }
            }
            if ref_lines.is_empty() {
                for name in hidden_refs {
                    ref_lines.push(Line::from(Span::styled(
                        format!(" {} {name}", g.local_branch),
                        Style::default().fg(ctx.theme.fg_primary),
                    )));
                }
            }
            ref_lines.push(Line::from(Span::styled(
                ctx.t("tooltip.overflow_hint").to_string(),
                Style::default().fg(ctx.theme.fg_muted),
            )));
            (
                ctx.t("tooltip.overflow_title"),
                ctx.theme.border_focused,
                ref_lines,
            )
        }
        HitTarget::CommitSignatureCell { commit_oid, .. } => {
            let Some(sig) = state.diff_caches.signatures.peek(commit_oid) else {
                return;
            };
            if !sig.status.is_signed() {
                return;
            }
            let (_, desc_key) = signature_badge_and_desc_keys(sig.status);
            let (icon_fg, _, _) = ctx.theme.signature_colors(sig.status);
            let mut sig_lines = vec![Line::from(vec![
                Span::styled(
                    format!(" {} ", sig.status.icon()),
                    Style::default().fg(icon_fg),
                ),
                Span::styled(
                    ctx.t(desc_key).to_string(),
                    Style::default().fg(ctx.theme.fg_bright),
                ),
            ])];
            if let Some(signer) = &sig.signer {
                sig_lines.push(Line::from(Span::styled(
                    state
                        .i18n
                        .t_fmt("tooltip.sig_signer", &[("signer", signer)]),
                    Style::default().fg(ctx.theme.fg_primary),
                )));
            }
            if let Some(key) = &sig.key {
                sig_lines.push(Line::from(Span::styled(
                    state.i18n.t_fmt("tooltip.sig_key", &[("key", key)]),
                    Style::default().fg(ctx.theme.fg_muted),
                )));
            }
            (ctx.t("tooltip.signature_title"), icon_fg, sig_lines)
        }
        _ => return,
    };

    let content_w = lines
        .iter()
        .map(Line::width)
        .max()
        .unwrap_or(20)
        .max(title.width() + 2);
    let width = (content_w as u16 + 2).clamp(20, screen.width.saturating_sub(2).max(20));
    let height = (lines.len() as u16 + 2).min(screen.height.saturating_sub(1).max(3));

    let x = hx.min(screen.right().saturating_sub(width));
    let below_y = hy.saturating_add(1);
    let y = if below_y.saturating_add(height) <= screen.bottom() {
        below_y
    } else {
        hy.saturating_sub(height)
    };
    let rect = Rect::new(x, y, width, height);

    crate::views::clear_modal_overlay(frame, rect);
    let block = Block::default()
        .title(Span::styled(
            title,
            Style::default()
                .fg(ctx.theme.fg_bright)
                .add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color))
        .style(Style::default().bg(ctx.theme.bg_elevated));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    frame.render_widget(Paragraph::new(lines), inner);
}
