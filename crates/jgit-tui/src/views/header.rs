use jgit_core::WorkspaceTab;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use unicode_width::UnicodeWidthStr;

use crate::app::{AppState, ModalLayer, RenderContext};
use crate::hit_test::{GraphHitKind, HitTarget};
use crate::views::truncate_to_width;

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
            jgit_core::RepositoryState::Rebase { progress, .. }
            | jgit_core::RepositoryState::RebaseInteractive { progress, .. }
            | jgit_core::RepositoryState::RebaseMerge { progress, .. } => Some(match progress {
                Some((c, t)) => format!(" · Rebasing {c}/{t}"),
                None => " · Rebasing".to_string(),
            }),
            jgit_core::RepositoryState::Merge => Some(" · Merging".to_string()),
            jgit_core::RepositoryState::CherryPick => Some(" · Cherry-picking".to_string()),
            jgit_core::RepositoryState::Revert => Some(" · Reverting".to_string()),
            _ => None,
        }
        .unwrap_or_default();
        if let Some(branch) = &snap.head_branch {
            let unborn = if snap.is_unborn_head { " (unborn)" } else { "" };
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
                .unwrap_or_else(|| "detached".to_string());
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
    ctx: &mut RenderContext<'_>,
) {
    if area.height == 0 || area.width == 0 {
        return;
    }

    // Disabled-reason of the focused context-menu item.
    if let Some(ModalLayer::ContextMenu {
        items, selected, ..
    }) = state.modal_stack.last()
        && let Some(item) = items.get(*selected)
        && let Some(reason) = &item.disabled_reason
    {
        frame.render_widget(
            Paragraph::new(Span::styled(
                truncate_to_width(&format!(" ⊘ {}: {reason}", item.label), area.width as usize),
                Style::default().fg(ctx.theme.status_untracked),
            ))
            .style(Style::default().bg(ctx.theme.bg_panel)),
            area,
        );
        return;
    }

    if state.modal_stack.is_empty()
        && let Some(n) = &state.notification
    {
        let icon = if n.is_error { "✕" } else { "✔" };
        let color = if n.is_error {
            ctx.theme.status_conflicted
        } else {
            ctx.theme.status_added
        };
        let action = n.action.as_ref().map(|(l, _)| format!(" [{l}] "));
        let dismiss = " ✕ ";
        let right_w = action.as_ref().map_or(0, |a| a.width()) + dismiss.width() + 1;
        let text = truncate_to_width(
            &format!(" {icon} {}", n.text),
            (area.width as usize).saturating_sub(right_w + 1),
        );
        let mut spans = vec![Span::styled(text.clone(), Style::default().fg(color))];
        let mut x = area.x + text.width() as u16;
        if let Some(a) = action {
            spans.push(Span::raw(" "));
            x += 1;
            let w = a.width() as u16;
            spans.push(Span::styled(
                a,
                Style::default()
                    .fg(ctx.theme.fg_bright)
                    .bg(ctx.theme.border_focused)
                    .add_modifier(Modifier::BOLD),
            ));
            ctx.hit_registry
                .register(Rect::new(x, area.y, w, 1), 6, HitTarget::NotificationAction);
            x += w;
        }
        spans.push(Span::raw(" "));
        x += 1;
        spans.push(Span::styled(dismiss, Style::default().fg(ctx.theme.fg_muted)));
        ctx.hit_registry.register(
            Rect::new(x, area.y, dismiss.width() as u16, 1),
            6,
            HitTarget::NotificationDismiss,
        );
        spans.push(Span::styled(
            "  Alt+Z action · Alt+X dismiss",
            Style::default().fg(ctx.theme.fg_muted),
        ));
        frame.render_widget(
            Paragraph::new(Line::from(spans)).style(Style::default().bg(ctx.theme.bg_panel)),
            area,
        );
        return;
    }

    if let Some(HitTarget::GraphPrintElement {
        kind:
            GraphHitKind::Arrow {
                target_row,
                target_oid,
                is_down,
                ..
            },
        ..
    }) = ctx.hover_target
    {
        let g = &ctx.glyph_mode.symbols().graph;
        let arrow = if *is_down { g.arrow_down } else { g.arrow_up };
        let dir_label = if *is_down { "parent" } else { "child" };
        let target_subj = state
            .log_commit_at(*target_row)
            .map(|c| c.subject.as_str())
            .unwrap_or("");
        let prefix = format!(" {arrow} Jump to {dir_label} ");
        let short = target_oid.short();
        let suffix = " (Click to jump, Alt+Left to return)";
        let subj_budget = (area.width as usize)
            .saturating_sub(prefix.width() + short.width() + 2 + suffix.width());
        let line = Line::from(vec![
            Span::styled(prefix, Style::default().fg(ctx.theme.fg_muted)),
            Span::styled(short, Style::default().fg(ctx.theme.fg_accent_link)),
            Span::styled(
                format!("  {}", truncate_to_width(target_subj, subj_budget)),
                Style::default().fg(ctx.theme.fg_bright),
            ),
            Span::styled(suffix, Style::default().fg(ctx.theme.fg_muted)),
        ]);
        frame.render_widget(
            Paragraph::new(line).style(Style::default().bg(ctx.theme.bg_panel)),
            area,
        );
        return;
    }

    let hint = match state.active_tab {
        WorkspaceTab::Commit => {
            " Tab Focus | Space Stage/Unstage | v Visual | s/u/r Hunk | n/p Prev/Next | Alt+M Amend | Ctrl+Enter Commit | ? Help"
        }
        WorkspaceTab::Log => {
            " Tab Focus | / Filter | Shift+↑↓ Range | →/← Parent/Child | Ctrl+G Go to | Ctrl+` Branches | . Menu | F4 Diff | ? Help"
        }
        WorkspaceTab::Console => {
            " 1-4 Tabs | Tab Focus | j/k Select/Scroll | p Run git status | r Refresh | RightClick Probes | q Quit"
        }
        WorkspaceTab::Stash => {
            " j/k Select | Enter/a Apply | p Pop | b Unstash as Branch | d Drop | n New | C Clear | . Menu | ? Help"
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
