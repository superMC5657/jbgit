//! `OngoingRepositoryStateBanner` (Spec 05 §4.3): amber row under the tab bar
//! while a rebase / merge / cherry-pick / revert is paused, with one-click
//! `[Resolve] [Continue] [Skip] [Abort]`.

use jgit_core::OngoingKind;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use unicode_width::UnicodeWidthStr;

use crate::app::{AppState, RenderContext};
use crate::hit_test::{HitTarget, OngoingButton};
use crate::views::truncate_to_width;

pub fn banner_visible(state: &AppState) -> bool {
    state.ongoing_kind().is_some()
}

/// Banner text without buttons (`⚠ REBASING feat onto main (Step 2/5) — 1 conflicted file`).
pub fn banner_text(state: &AppState) -> Option<String> {
    let snap = state.repo_snapshot.as_deref()?;
    let kind = OngoingKind::from_state(&snap.state)?;
    let mut text = match &snap.state {
        jgit_core::RepositoryState::Rebase { progress, branch }
        | jgit_core::RepositoryState::RebaseInteractive { progress, branch }
        | jgit_core::RepositoryState::RebaseMerge { progress, branch } => {
            let mut t = String::from("REBASING");
            if let Some(b) = branch {
                t.push(' ');
                t.push_str(b);
            }
            if let Some(onto) = &snap.ongoing_onto {
                t.push_str(&format!(" onto {onto}"));
            }
            if let Some((cur, total)) = progress {
                t.push_str(&format!(" (Step {cur}/{total})"));
            }
            t
        }
        other => other
            .banner_label()
            .unwrap_or_else(|| kind.title().to_uppercase()),
    };
    let n = state.conflicted_count();
    if n > 0 {
        text.push_str(&format!(
            " — {n} conflicted file{}",
            if n == 1 { "" } else { "s" }
        ));
    } else {
        text.push_str(" — no conflicts left");
    }
    Some(format!("⚠ {text}"))
}

pub fn render_banner(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &AppState,
    ctx: &mut RenderContext<'_>,
) {
    if area.height == 0 {
        return;
    }
    let Some(kind) = state.ongoing_kind() else {
        return;
    };
    let Some(text) = banner_text(state) else {
        return;
    };
    let amber = ctx.theme.ref_detached.icon;
    let bg = ctx.theme.ref_detached.pill_bg;
    let mut buttons = vec![
        (" Resolve ", OngoingButton::Resolve),
        (" Continue ", OngoingButton::Continue),
    ];
    if kind.can_skip() {
        buttons.push((" Skip ", OngoingButton::Skip));
    }
    buttons.push((" Abort ", OngoingButton::Abort));
    let buttons_w: usize = buttons.iter().map(|(l, _)| l.width() + 4).sum();
    let text_w = (area.width as usize).saturating_sub(buttons_w + 1);
    let shown = truncate_to_width(&format!(" {text}"), text_w);
    let mut spans = vec![Span::styled(
        shown.clone(),
        Style::default().fg(amber).add_modifier(Modifier::BOLD),
    )];
    let pad = text_w.saturating_sub(shown.width());
    spans.push(Span::raw(" ".repeat(pad)));
    let mut x = area.x + (shown.width() + pad) as u16;
    for (label, btn) in buttons {
        spans.push(Span::raw(" "));
        x += 1;
        let style = match btn {
            OngoingButton::Abort => Style::default()
                .fg(Color::White)
                .bg(ctx.theme.status_conflicted),
            OngoingButton::Continue if state.conflicted_count() == 0 => Style::default()
                .fg(Color::Black)
                .bg(amber)
                .add_modifier(Modifier::BOLD),
            _ => Style::default()
                .fg(ctx.theme.fg_bright)
                .bg(ctx.theme.bg_elevated),
        };
        let w = label.width() as u16 + 2;
        spans.push(Span::styled(format!("[{label}]"), style));
        if x + w <= area.right() {
            ctx.hit_registry
                .register(Rect::new(x, area.y, w, 1), 6, HitTarget::OngoingBanner(btn));
        }
        x += w + 1;
        spans.push(Span::raw(" "));
    }
    frame.render_widget(
        Paragraph::new(Line::from(spans)).style(Style::default().bg(bg)),
        area,
    );
}
