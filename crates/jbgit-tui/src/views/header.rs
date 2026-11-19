use jbgit_core::WorkspaceTab;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use unicode_width::UnicodeWidthStr;

use crate::app::{AppState, RenderContext};
use crate::hit_test::HitTarget;

pub fn render_top_header(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &AppState,
    ctx: &mut RenderContext<'_>,
) {
    if area.height == 0 || area.width == 0 {
        return;
    }

    let bg = ctx.theme.bg_panel;
    let mut spans: Vec<Span<'_>> = Vec::new();

    let repo_name = state
        .repo_snapshot
        .as_ref()
        .map(|s| s.repo_name.as_str())
        .or_else(|| state.repo_path_display.file_name().and_then(|s| s.to_str()))
        .unwrap_or("jbgit");

    spans.push(Span::styled(
        format!(" {repo_name} "),
        Style::default()
            .fg(ctx.theme.fg_bright)
            .bg(bg)
            .add_modifier(Modifier::BOLD),
    ));

    // Branch widget pill
    let branch_start_x = area.x.saturating_add(span_width(&spans));
    let (pill_text, pill_fg, pill_bg) = if let Some(snap) = &state.repo_snapshot {
        let ab_suffix = snap
            .head_branch
            .as_ref()
            .and_then(|head_branch| {
                snap.refs
                    .iter()
                    .find(|r| r.is_head && &r.name == head_branch)
            })
            .map(|r| ctx.glyph_mode.symbols().format_ahead_behind(r.ahead_behind))
            .unwrap_or_default();
        let ongoing = match &snap.state {
            jbgit_core::RepositoryState::Rebase { progress, .. }
            | jbgit_core::RepositoryState::RebaseInteractive { progress, .. }
            | jbgit_core::RepositoryState::RebaseMerge { progress, .. } => Some(match progress {
                Some((c, t)) => {
                    let c_s = c.to_string();
                    let t_s = t.to_string();
                    state
                        .i18n
                        .t_fmt("banner.header_rebasing_progress", &[("cur", &c_s), ("total", &t_s)])
                }
                None => state.i18n.t("banner.header_rebasing").to_string(),
            }),
            jbgit_core::RepositoryState::Merge => {
                Some(state.i18n.t("banner.header_merging").to_string())
            }
            jbgit_core::RepositoryState::CherryPick => {
                Some(state.i18n.t("banner.header_cherry_picking").to_string())
            }
            jbgit_core::RepositoryState::Revert => {
                Some(state.i18n.t("banner.header_reverting").to_string())
            }
            _ => None,
        }
        .unwrap_or_default();
        if let Some(branch) = &snap.head_branch {
            let unborn = if snap.is_unborn_head {
                state.i18n.t("view.header.unborn")
            } else {
                ""
            };
            (
                format!(
                    " {} {}{}{}{} ▾ ",
                    ctx.glyph_mode.symbols().head,
                    branch,
                    unborn,
                    ab_suffix,
                    ongoing
                ),
                ctx.theme.ref_head.pill_fg,
                ctx.theme.ref_head.pill_bg,
            )
        } else {
            let short = snap
                .head_oid
                .map(|o| o.short())
                .unwrap_or_else(|| state.i18n.t("view.header.detached").to_string());
            (
                format!(
                    " {} {}{} ▾ ",
                    ctx.glyph_mode.symbols().detached_head,
                    short,
                    ongoing
                ),
                ctx.theme.ref_detached.pill_fg,
                ctx.theme.ref_detached.pill_bg,
            )
        }
    } else if state.repo_error.is_some() {
        (
            state.i18n.t("view.header.no_repo").to_string(),
            ctx.theme.status_untracked,
            ctx.theme.bg_elevated,
        )
    } else {
        (
            state.i18n.t("view.header.loading").to_string(),
            ctx.theme.fg_muted,
            ctx.theme.bg_elevated,
        )
    };

    let pill_w = pill_text.width() as u16;
    spans.push(Span::styled(
        pill_text,
        Style::default()
            .fg(pill_fg)
            .bg(pill_bg)
            .add_modifier(Modifier::BOLD),
    ));
    if branch_start_x < area.x.saturating_add(area.width) {
        let w = pill_w.min(
            area.x
                .saturating_add(area.width)
                .saturating_sub(branch_start_x),
        );
        ctx.hit_registry.register(
            Rect::new(branch_start_x, area.y, w, 1),
            5,
            HitTarget::BranchWidgetPill,
        );
    }

    // Ongoing state banner (Merge / Rebase progress / Cherry-Pick)
    if let Some(snap) = &state.repo_snapshot
        && let Some(banner) = localized_state_banner(&snap.state, &state.i18n)
    {
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            format!(" {banner} "),
            Style::default()
                .fg(ctx.theme.fg_bright)
                .bg(ctx.theme.status_conflicted)
                .add_modifier(Modifier::BOLD),
        ));
    }

    if let Some(err) = &state.repo_error {
        spans.push(Span::styled(
            format!("  {err}"),
            Style::default().fg(ctx.theme.status_untracked),
        ));
    } else if let Some(notice) = &state.status_notice {
        spans.push(Span::styled(
            format!("  {notice}"),
            Style::default().fg(ctx.theme.fg_accent_link),
        ));
    }

    // Right-side action badges
    let spinner = if state.has_active_tasks() {
        format!(" {} ", state.spinner_char())
    } else {
        String::new()
    };
    let refresh_btn = " ↻ ";
    let glyph_btn = format!(" {} ", ctx.glyph_mode.label());
    let help_btn = " ? ";

    let spinner_w = spinner.width() as u16;
    let right_w = spinner_w + (refresh_btn.width() + glyph_btn.width() + help_btn.width()) as u16;
    let left_w = span_width(&spans);

    if area.width > left_w.saturating_add(right_w) {
        let pad = area.width.saturating_sub(left_w).saturating_sub(right_w);
        spans.push(Span::styled(
            " ".repeat(pad as usize),
            Style::default().bg(bg),
        ));

        if !spinner.is_empty() {
            spans.push(Span::styled(
                spinner,
                Style::default().fg(ctx.theme.border_focused).bg(bg),
            ));
        }

        let rx = area
            .x
            .saturating_add(area.width)
            .saturating_sub(right_w)
            .saturating_add(spinner_w);
        ctx.hit_registry.register(
            Rect::new(rx, area.y, refresh_btn.width() as u16, 1),
            5,
            HitTarget::HeaderRefreshButton,
        );
        spans.push(Span::styled(
            refresh_btn,
            Style::default()
                .fg(ctx.theme.fg_primary)
                .bg(ctx.theme.bg_elevated),
        ));

        let gx = rx.saturating_add(refresh_btn.width() as u16);
        ctx.hit_registry.register(
            Rect::new(gx, area.y, glyph_btn.width() as u16, 1),
            5,
            HitTarget::HeaderGlyphButton,
        );
        spans.push(Span::styled(
            glyph_btn.clone(),
            Style::default().fg(ctx.theme.fg_muted).bg(bg),
        ));

        let hx = gx.saturating_add(glyph_btn.width() as u16);
        ctx.hit_registry.register(
            Rect::new(hx, area.y, help_btn.width() as u16, 1),
            5,
            HitTarget::HeaderHelpButton,
        );
        spans.push(Span::styled(
            help_btn,
            Style::default()
                .fg(ctx.theme.fg_accent_link)
                .bg(ctx.theme.bg_elevated),
        ));
    }

    let line = Paragraph::new(Line::from(spans)).style(Style::default().bg(bg));
    frame.render_widget(line, area);
}

pub fn render_tab_bar(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &AppState,
    ctx: &mut RenderContext<'_>,
) {
    if area.height == 0 || area.width == 0 {
        return;
    }

    let mut spans: Vec<Span<'_>> = Vec::new();
    let mut cursor_x = area.x;

    for tab in WorkspaceTab::ALL {
        let active = state.active_tab == tab;
        let tab_name = match tab {
            WorkspaceTab::Commit => state.i18n.t("tabs.commit"),
            WorkspaceTab::Log => state.i18n.t("tabs.log"),
            WorkspaceTab::Stash => state.i18n.t("tabs.stash"),
            WorkspaceTab::Console => state.i18n.t("tabs.console"),
        };
        let label = match tab {
            WorkspaceTab::Commit => {
                let count = state.stage_status.as_ref().map_or(0, |s| s.total_changes());
                if count > 0 {
                    format!(" [{}] {} ({}) ", tab.index(), tab_name, count)
                } else {
                    format!(" [{}] {} ", tab.index(), tab_name)
                }
            }
            WorkspaceTab::Console => {
                let count = state.console_entries.len();
                if count > 0 {
                    format!(" [{}] {} ({}) ", tab.index(), tab_name, count)
                } else {
                    format!(" [{}] {} ", tab.index(), tab_name)
                }
            }
            _ => format!(" [{}] {} ", tab.index(), tab_name),
        };

        let w = label.width() as u16;
        let style = if active {
            Style::default()
                .fg(ctx.theme.fg_bright)
                .bg(ctx.theme.bg_selection_focused)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
                .fg(ctx.theme.fg_muted)
                .bg(ctx.theme.bg_panel)
        };

        spans.push(Span::styled(label, style));
        if cursor_x < area.x.saturating_add(area.width) {
            let clamped_w = w.min(area.x.saturating_add(area.width).saturating_sub(cursor_x));
            ctx.hit_registry.register(
                Rect::new(cursor_x, area.y, clamped_w, 1),
                5,
                HitTarget::ToolTab(tab),
            );
        }
        cursor_x = cursor_x.saturating_add(w);
    }

    let line = Paragraph::new(Line::from(spans)).style(Style::default().bg(ctx.theme.bg_panel));
    frame.render_widget(line, area);
}

fn localized_state_banner(
    st: &jbgit_core::RepositoryState,
    i18n: &crate::i18n::I18n,
) -> Option<String> {
    let format_rebase = |prefix: &str, progress: Option<(usize, usize)>, branch: Option<&str>| {
        match (branch, progress) {
            (Some(b), Some((cur, total))) => format!("{prefix} {b} ({cur}/{total})"),
            (Some(b), None) => format!("{prefix} {b}"),
            (None, Some((cur, total))) => format!("{prefix} ({cur}/{total})"),
            (None, None) => prefix.to_string(),
        }
    };
    match st {
        jbgit_core::RepositoryState::Clean => None,
        jbgit_core::RepositoryState::Detached => Some(i18n.t("ongoing.state_detached").to_string()),
        jbgit_core::RepositoryState::Merge => Some(i18n.t("ongoing.state_merging").to_string()),
        jbgit_core::RepositoryState::CherryPick => {
            Some(i18n.t("ongoing.state_cherry_picking").to_string())
        }
        jbgit_core::RepositoryState::Revert => Some(i18n.t("ongoing.state_reverting").to_string()),
        jbgit_core::RepositoryState::Bisect => Some(i18n.t("ongoing.state_bisecting").to_string()),
        jbgit_core::RepositoryState::Rebase { progress, branch } => Some(format_rebase(
            i18n.t("ongoing.state_rebasing"),
            *progress,
            branch.as_deref(),
        )),
        jbgit_core::RepositoryState::RebaseInteractive { progress, branch } => Some(format_rebase(
            i18n.t("ongoing.state_rebasing_i"),
            *progress,
            branch.as_deref(),
        )),
        jbgit_core::RepositoryState::RebaseMerge { progress, branch } => Some(format_rebase(
            i18n.t("ongoing.state_rebasing"),
            *progress,
            branch.as_deref(),
        )),
    }
}

fn span_width(spans: &[Span<'_>]) -> u16 {
    spans.iter().map(|s| s.content.width() as u16).sum()
}
