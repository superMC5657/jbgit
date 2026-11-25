//! `OngoingRepositoryStateBanner`: amber row under the tab bar
//! while a rebase / merge / cherry-pick / revert is paused, with one-click
//! `[Resolve] [Continue] [Skip] [Abort]`.

use jbgit_core::OngoingKind;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use unicode_width::UnicodeWidthStr;

use crate::app::{AppState, RenderContext};
use crate::hit_test::{BisectButton, HitTarget, OngoingButton};
use crate::views::truncate_to_width;

pub fn banner_visible(state: &AppState) -> bool {
    state.ongoing_kind().is_some() || state.is_bisecting()
}

/// Banner text without buttons (`⚠ REBASING feat onto main (Step 2/5) — 1 conflicted file`,
/// or `⚠ BISECTING — ~3 step(s) left (6 rev(s))`).
pub fn banner_text(state: &AppState) -> Option<String> {
    let snap = state.repo_snapshot.as_deref()?;
    let i18n = &state.i18n;
    if snap.state == jbgit_core::RepositoryState::Bisect {
        let mut text = i18n.t("ongoing.state_bisecting").to_string();
        if let Some(info) = &snap.bisect
            && let (Some(steps), Some(revs)) = (info.remaining_steps, info.remaining_revisions)
        {
            let steps_s = steps.to_string();
            let revs_s = revs.to_string();
            text.push_str(&i18n.t_fmt(
                "banner.bisect_remaining",
                &[("steps", &steps_s), ("revs", &revs_s)],
            ));
        }
        return Some(format!("⚠ {text}"));
    }
    let kind = OngoingKind::from_state(&snap.state)?;
    let mut text = match &snap.state {
        jbgit_core::RepositoryState::Rebase { progress, branch }
        | jbgit_core::RepositoryState::RebaseInteractive { progress, branch }
        | jbgit_core::RepositoryState::RebaseMerge { progress, branch } => {
            let mut t = i18n.t("banner.rebasing_prefix").to_string();
            if let Some(b) = branch {
                t.push(' ');
                t.push_str(b);
            }
            if let Some(onto) = &snap.ongoing_onto {
                t.push_str(&i18n.t_fmt("banner.onto", &[("onto", onto)]));
            }
            if let Some((cur, total)) = progress {
                let c_s = cur.to_string();
                let t_s = total.to_string();
                t.push_str(&i18n.t_fmt("banner.step", &[("cur", &c_s), ("total", &t_s)]));
            }
            t
        }
        jbgit_core::RepositoryState::Merge => i18n.t("ongoing.state_merging").to_string(),
        jbgit_core::RepositoryState::CherryPick => {
            i18n.t("ongoing.state_cherry_picking").to_string()
        }
        jbgit_core::RepositoryState::Revert => i18n.t("ongoing.state_reverting").to_string(),
        jbgit_core::RepositoryState::Bisect => i18n.t("ongoing.state_bisecting").to_string(),
        jbgit_core::RepositoryState::Detached => i18n.t("ongoing.state_detached").to_string(),
        other => other
            .banner_label()
            .unwrap_or_else(|| i18n.ongoing_title(kind).to_uppercase()),
    };
    let n = state.conflicted_count();
    if n == 1 {
        text.push_str(i18n.t("banner.conflicted_one"));
    } else if n > 1 {
        let count_s = n.to_string();
        text.push_str(&i18n.t_fmt("banner.conflicted_many", &[("count", &count_s)]));
    } else {
        text.push_str(i18n.t("banner.no_conflicts"));
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
    if state.is_bisecting() {
        let Some(text) = banner_text(state) else {
            return;
        };
        let amber = ctx.theme.ref_detached.icon;
        let bg = ctx.theme.ref_detached.pill_bg;
        let raw_buttons = [
            (
                state.i18n.t("banner.bisect_good_btn"),
                BisectButton::Good,
                crate::keymap::KeyAction::BisectGood,
            ),
            (
                state.i18n.t("banner.bisect_bad_btn"),
                BisectButton::Bad,
                crate::keymap::KeyAction::BisectBad,
            ),
            (
                state.i18n.t("banner.bisect_skip_btn"),
                BisectButton::Skip,
                crate::keymap::KeyAction::BisectSkip,
            ),
            (
                state.i18n.t("banner.bisect_reset_btn"),
                BisectButton::Reset,
                crate::keymap::KeyAction::BisectReset,
            ),
        ];
        let full_buttons: Vec<(String, BisectButton)> = raw_buttons
            .iter()
            .map(|(lbl, btn, ka)| {
                let sc = crate::keymap::catalog::shortcut_for_action(
                    state,
                    &[crate::config::KeymapScope::Global],
                    ka,
                );
                let text = match sc {
                    Some(k) => format!("{lbl} {k}"),
                    None => (*lbl).to_string(),
                };
                (text, *btn)
            })
            .collect();
        let full_w: usize = full_buttons.iter().map(|(l, _)| l.width() + 4).sum();
        let buttons: Vec<(String, BisectButton)> =
            if (area.width as usize) >= full_w + text.width() + 4 {
                full_buttons
            } else {
                raw_buttons
                    .iter()
                    .map(|(lbl, btn, _)| ((*lbl).to_string(), *btn))
                    .collect()
            };
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
                BisectButton::Good => Style::default()
                    .fg(Color::Black)
                    .bg(ctx.theme.status_added)
                    .add_modifier(Modifier::BOLD),
                BisectButton::Bad => Style::default()
                    .fg(Color::White)
                    .bg(ctx.theme.status_conflicted)
                    .add_modifier(Modifier::BOLD),
                BisectButton::Reset => Style::default()
                    .fg(Color::Black)
                    .bg(amber)
                    .add_modifier(Modifier::BOLD),
                BisectButton::Skip => Style::default()
                    .fg(ctx.theme.fg_bright)
                    .bg(ctx.theme.bg_elevated),
            };
            let w = label.width() as u16 + 2;
            spans.push(Span::styled(format!("[{label}]"), style));
            if x + w <= area.right() {
                ctx.hit_registry.register(
                    Rect::new(x, area.y, w, 1),
                    6,
                    HitTarget::BisectBanner(btn),
                );
            }
            x += w + 1;
            spans.push(Span::raw(" "));
        }
        frame.render_widget(
            Paragraph::new(Line::from(spans)).style(Style::default().bg(bg)),
            area,
        );
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
        (state.i18n.t("banner.resolve_btn"), OngoingButton::Resolve),
        (state.i18n.t("banner.continue_btn"), OngoingButton::Continue),
    ];
    if kind.can_skip() {
        buttons.push((state.i18n.t("banner.skip_btn"), OngoingButton::Skip));
    }
    buttons.push((state.i18n.t("banner.abort_btn"), OngoingButton::Abort));
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
