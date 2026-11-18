use jgit_core::WorkspaceTab;
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
        .or_else(|| {
            state
                .repo_path_display
                .file_name()
                .and_then(|s| s.to_str())
        })
        .unwrap_or("jGit");

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
        let mut ab_suffix = String::new();
        if let Some(ref head_branch) = snap.head_branch
            && let Some(r) = snap.refs.iter().find(|r| r.is_head && &r.name == head_branch)
            && let Some((ahead, behind)) = r.ahead_behind
        {
            if ahead > 0 {
                ab_suffix.push_str(&format!(" ↑{ahead}"));
            }
            if behind > 0 {
                ab_suffix.push_str(&format!(" ↓{behind}"));
            }
        }
        if let Some(branch) = &snap.head_branch {
            let unborn = if snap.is_unborn_head { " (unborn)" } else { "" };
            (
                format!(" {} {}{}{} ", ctx.glyph_mode.head_icon(), branch, unborn, ab_suffix),
                ctx.theme.ref_head_fg,
                ctx.theme.ref_head_bg,
            )
        } else {
            let short = snap
                .head_oid
                .map(|o| o.short())
                .unwrap_or_else(|| "detached".to_string());
            (
                format!(" {} {} ", ctx.glyph_mode.warn_icon(), short),
                ctx.theme.status_conflicted,
                ctx.theme.bg_elevated,
            )
        }
    } else if state.repo_error.is_some() {
        (
            " No Repo ".to_string(),
            ctx.theme.status_untracked,
            ctx.theme.bg_elevated,
        )
    } else {
        (
            " Loading… ".to_string(),
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
        let w = pill_w.min(area.x.saturating_add(area.width).saturating_sub(branch_start_x));
        ctx.hit_registry.register(
            Rect::new(branch_start_x, area.y, w, 1),
            5,
            HitTarget::BranchWidgetPill,
        );
    }

    // Ongoing state banner (Merge / Rebase progress / Cherry-Pick)
    if let Some(snap) = &state.repo_snapshot
        && let Some(banner) = snap.state.banner_label()
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
    let right_w =
        spinner_w + (refresh_btn.width() + glyph_btn.width() + help_btn.width()) as u16;
    let left_w = span_width(&spans);

    if area.width > left_w.saturating_add(right_w) {
        let pad = area.width.saturating_sub(left_w).saturating_sub(right_w);
        spans.push(Span::styled(" ".repeat(pad as usize), Style::default().bg(bg)));

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
            Style::default().fg(ctx.theme.fg_primary).bg(ctx.theme.bg_elevated),
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
            Style::default().fg(ctx.theme.fg_accent_link).bg(ctx.theme.bg_elevated),
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
        let label = match tab {
            WorkspaceTab::Commit => {
                let count = state.stage_status.as_ref().map_or(0, |s| s.total_changes());
                if count > 0 {
                    format!(" [{}] {} ({}) ", tab.index(), tab.title(), count)
                } else {
                    format!(" [{}] {} ", tab.index(), tab.title())
                }
            }
            WorkspaceTab::Console => {
                let count = state.console_entries.len();
                if count > 0 {
                    format!(" [{}] {} ({}) ", tab.index(), tab.title(), count)
                } else {
                    format!(" [{}] {} ", tab.index(), tab.title())
                }
            }
            _ => format!(" [{}] {} ", tab.index(), tab.title()),
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

pub fn render_footer_bar(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &AppState,
    ctx: &RenderContext<'_>,
) {
    if area.height == 0 || area.width == 0 {
        return;
    }

    let hint = match state.active_tab {
        WorkspaceTab::Log => {
            " 1-4 Tabs | Tab Focus | j/k Select | Drag │ Split (DblClick Reset) | Wheel Scroll | RightClick Menu | ? Help | q Quit"
        }
        WorkspaceTab::Console => {
            " 1-4 Tabs | Tab Focus | j/k Select/Scroll | p Run git status | r Refresh | RightClick Probes | q Quit"
        }
        _ => {
            " 1-4 Tabs | Tab Focus | Drag │/─ Split (DblClick Reset) | r Refresh | p Probe | ? Help | q Quit"
        }
    };

    let footer = Paragraph::new(Line::from(Span::styled(
        hint,
        Style::default().fg(ctx.theme.fg_muted),
    )))
    .style(Style::default().bg(ctx.theme.bg_panel));
    frame.render_widget(footer, area);
}

fn span_width(spans: &[Span<'_>]) -> u16 {
    spans.iter().map(|s| s.content.width() as u16).sum()
}
