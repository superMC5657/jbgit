use jbgit_core::{PaneId, WorkspaceTab};

use crate::config::KeymapScope;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use unicode_width::UnicodeWidthStr;

use crate::app::{AppState, ModalLayer, RenderContext};
use crate::hit_test::{GraphHitKind, HitTarget};
use crate::keymap::KeyAction;
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
            state.i18n.t("view.header.notification_hint"),
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
        let dir_label = if *is_down {
            state.i18n.t("view.footer.arrow_parent")
        } else {
            state.i18n.t("view.footer.arrow_child")
        };
        let target_subj = state
            .log_commit_at(*target_row)
            .map(|c| c.subject.as_str())
            .unwrap_or("");
        let prefix = state
            .i18n
            .t_fmt("view.footer.arrow_jump_prefix", &[("arrow", arrow), ("dir", dir_label)]);
        let short = target_oid.short();
        let suffix = state.i18n.t("view.footer.arrow_jump_suffix");
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

    render_dynamic_footer_hints(frame, area, state, ctx);
}

fn footer_key_pair(a: Option<String>, b: Option<String>) -> Option<String> {
    match (a, b) {
        (Some(k1), Some(k2)) => Some(format!("{k1}/{k2}")),
        (Some(k1), None) => Some(k1),
        (None, Some(k2)) => Some(k2),
        (None, None) => None,
    }
}

fn push_footer_key(
    pills: &mut Vec<(String, String)>,
    ctx: &RenderContext<'_>,
    key: Option<String>,
    i18n_key: &str,
) {
    if let Some(k) = key {
        pills.push((k, ctx.t(i18n_key).to_string()));
    }
}

fn push_footer_lit(
    pills: &mut Vec<(String, String)>,
    ctx: &RenderContext<'_>,
    key: &str,
    i18n_key: &str,
) {
    pills.push((key.to_string(), ctx.t(i18n_key).to_string()));
}

fn push_diff_nav_pills(
    pills: &mut Vec<(String, String)>,
    ctx: &RenderContext<'_>,
    km: &crate::config::KeymapConfig,
    scopes: &[KeymapScope],
    include_files: bool,
) {
    push_footer_key(
        pills,
        ctx,
        footer_key_pair(
            km.primary_key_for(scopes, &KeyAction::PrevHunk),
            km.primary_key_for(scopes, &KeyAction::NextHunk),
        ),
        "view.footer.pill.hunk",
    );
    if include_files {
        push_footer_key(
            pills,
            ctx,
            footer_key_pair(
                km.primary_key_for(scopes, &KeyAction::PrevFile),
                km.primary_key_for(scopes, &KeyAction::NextFile),
            ),
            "view.footer.pill.file",
        );
    }
}

fn push_diff_toggle_pills(
    pills: &mut Vec<(String, String)>,
    ctx: &RenderContext<'_>,
    km: &crate::config::KeymapConfig,
    scopes: &[KeymapScope],
) {
    push_footer_key(
        pills,
        ctx,
        km.primary_key_for(scopes, &KeyAction::ToggleSideBySide),
        "view.footer.pill.side_by_side",
    );
    push_footer_key(
        pills,
        ctx,
        km.primary_key_for(scopes, &KeyAction::CycleDiffContext),
        "view.footer.pill.context",
    );
}

fn usable_branch_tree_key(
    km: &crate::config::KeymapConfig,
    scopes: &[KeymapScope],
    action: &KeyAction,
) -> Option<String> {
    if !km.tree_speed_search {
        return km.primary_key_for(scopes, action);
    }
    for &scope in scopes {
        for b in km.bindings_for(scope) {
            if &b.run == action
                && let Some(seq) = crate::keymap::KeySequence::parse(&b.on)
            {
                let captured = seq.strokes.first().is_some_and(|s| {
                    !s.ctrl
                        && !s.alt
                        && matches!(
                            s.code,
                            crossterm::event::KeyCode::Char(c)
                                if crate::keymap::actions::is_branch_speed_search_char(c)
                        )
                });
                if !captured {
                    return Some(seq.to_display_string());
                }
            }
        }
    }
    None
}

fn render_dynamic_footer_hints(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &AppState,
    ctx: &RenderContext<'_>,
) {
    use unicode_width::UnicodeWidthStr;

    let km = &state.config.keymap;
    let mut pills: Vec<(String, String)> = Vec::new();
    let mut include_help_pill = true;

    if state.active_tab == WorkspaceTab::Log && state.log_filter.editing_text {
        include_help_pill = false;
        push_footer_lit(&mut pills, ctx, "Enter", "view.footer.pill.apply");
        push_footer_lit(&mut pills, ctx, "Esc", "view.footer.pill.cancel");
        push_footer_lit(&mut pills, ctx, "Alt+R", "view.footer.pill.regex");
        push_footer_lit(&mut pills, ctx, "Alt+C", "view.footer.pill.case");
    } else if state.active_tab == WorkspaceTab::Log
        && state.focused_pane == PaneId::LogBranchesTree
        && (state.branches.searching
            || (km.tree_speed_search && !state.branches.query.is_empty()))
    {
        include_help_pill = false;
        push_footer_lit(
            &mut pills,
            ctx,
            ctx.t("view.footer.pill.type_key"),
            "view.footer.pill.filter",
        );
        push_footer_lit(&mut pills, ctx, "Enter", "view.footer.pill.apply");
        push_footer_lit(&mut pills, ctx, "Esc", "view.footer.pill.cancel");
    } else if state.details.diff_zoomed {
        let diff_scopes = &[KeymapScope::Diff];
        push_diff_nav_pills(&mut pills, ctx, km, diff_scopes, true);
        push_diff_toggle_pills(&mut pills, ctx, km, diff_scopes);
        let zoom_key = match km.primary_key_for(diff_scopes, &KeyAction::ZoomDiff) {
            Some(k) if k != "Esc" => format!("{k}/Esc"),
            _ => "Esc".to_string(),
        };
        pills.push((zoom_key, ctx.t("view.footer.pill.exit_zoom").to_string()));
    } else {
        match (state.active_tab, state.focused_pane) {
            (WorkspaceTab::Commit, PaneId::CommitChangesTree) => {
                let scopes = &[KeymapScope::Commit, KeymapScope::Global];
                push_footer_key(
                    &mut pills,
                    ctx,
                    km.primary_key_for(scopes, &KeyAction::ToggleStage)
                        .or_else(|| km.primary_key_for(scopes, &KeyAction::StageSelection)),
                    "view.footer.pill.stage",
                );
                push_footer_key(
                    &mut pills,
                    ctx,
                    km.primary_key_for(scopes, &KeyAction::DiscardChanges),
                    "view.footer.pill.discard",
                );
                push_footer_key(
                    &mut pills,
                    ctx,
                    km.primary_key_for(scopes, &KeyAction::FocusCommitMsg),
                    "view.footer.pill.msg",
                );
                push_footer_key(
                    &mut pills,
                    ctx,
                    km.primary_key_for(scopes, &KeyAction::ToggleAmend),
                    "view.footer.pill.amend",
                );
                push_footer_key(
                    &mut pills,
                    ctx,
                    km.primary_key_for(&[KeymapScope::Global], &KeyAction::ContextMenu),
                    "view.footer.pill.menu",
                );
                push_footer_key(
                    &mut pills,
                    ctx,
                    km.primary_key_for(scopes, &KeyAction::EditFile),
                    "view.footer.pill.edit",
                );
                push_footer_key(
                    &mut pills,
                    ctx,
                    km.primary_key_for(scopes, &KeyAction::ToggleGroup),
                    "view.footer.pill.group",
                );
                push_footer_key(
                    &mut pills,
                    ctx,
                    km.primary_key_for(&[KeymapScope::Global], &KeyAction::ToggleBranchesPopup),
                    "view.footer.pill.branches",
                );
                push_footer_key(
                    &mut pills,
                    ctx,
                    km.primary_key_for(scopes, &KeyAction::Commit),
                    "view.footer.pill.commit",
                );
            }
            (WorkspaceTab::Commit, PaneId::CommitMessageEditor) => {
                include_help_pill = false;
                let commit_scopes = &[KeymapScope::Commit];
                let commit_keys = km.editor_keys_for(commit_scopes, &KeyAction::Commit);
                let commit_label = if commit_keys.is_empty() {
                    None
                } else {
                    Some(
                        commit_keys
                            .into_iter()
                            .take(2)
                            .collect::<Vec<_>>()
                            .join(" / "),
                    )
                };
                push_footer_key(&mut pills, ctx, commit_label, "view.footer.pill.commit");
                push_footer_key(
                    &mut pills,
                    ctx,
                    km.primary_editor_key_for(commit_scopes, &KeyAction::ToggleAmend),
                    "view.footer.pill.amend",
                );
                push_footer_key(
                    &mut pills,
                    ctx,
                    km.primary_editor_key_for(commit_scopes, &KeyAction::ToggleSignoff),
                    "view.footer.pill.signoff",
                );
                push_footer_key(
                    &mut pills,
                    ctx,
                    km.primary_editor_key_for(commit_scopes, &KeyAction::CommitHistory),
                    "view.footer.pill.history",
                );
                if state.stage_workbench.editing_author {
                    push_footer_lit(&mut pills, ctx, "Enter/Esc", "view.footer.pill.apply");
                } else {
                    push_footer_lit(&mut pills, ctx, "Esc", "view.footer.pill.back");
                }
                push_footer_lit(&mut pills, ctx, "Ctrl+E", "view.footer.pill.edit");
                push_footer_key(
                    &mut pills,
                    ctx,
                    km.primary_editor_key_for(commit_scopes, &KeyAction::CommitAndPush),
                    "view.footer.pill.commit_push",
                );
            }
            (WorkspaceTab::Commit, PaneId::CommitDiffPreview) => {
                let scopes = &[KeymapScope::Diff, KeymapScope::Commit, KeymapScope::Global];
                if crate::keymap::actions::is_amend_diff(state) {
                    push_diff_nav_pills(&mut pills, ctx, km, scopes, false);
                    push_diff_toggle_pills(&mut pills, ctx, km, scopes);
                } else {
                    push_footer_key(
                        &mut pills,
                        ctx,
                        km.primary_key_for(scopes, &KeyAction::StageSelection)
                            .or_else(|| km.primary_key_for(scopes, &KeyAction::ToggleStage)),
                        "view.footer.pill.stage",
                    );
                    push_footer_key(
                        &mut pills,
                        ctx,
                        km.primary_key_for(scopes, &KeyAction::RollbackSelection),
                        "view.footer.pill.rollback",
                    );
                    push_footer_key(
                        &mut pills,
                        ctx,
                        km.primary_key_for(scopes, &KeyAction::ToggleVisual),
                        "view.footer.pill.visual",
                    );
                    push_diff_nav_pills(&mut pills, ctx, km, scopes, false);
                    push_diff_toggle_pills(&mut pills, ctx, km, scopes);
                    push_footer_key(
                        &mut pills,
                        ctx,
                        km.primary_key_for(scopes, &KeyAction::SwitchDiffSide),
                        "view.footer.pill.switch_side",
                    );
                    push_footer_key(
                        &mut pills,
                        ctx,
                        km.primary_key_for(scopes, &KeyAction::ThreeWayInspector),
                        "view.footer.pill.three_way",
                    );
                }
                push_footer_lit(&mut pills, ctx, "Esc", "view.footer.pill.back");
            }
            (WorkspaceTab::Log, PaneId::LogBranchesTree) => {
                let scopes = &[KeymapScope::Branch, KeymapScope::Log, KeymapScope::Global];
                let global = &[KeymapScope::Global];
                if km.tree_speed_search {
                    push_footer_lit(
                        &mut pills,
                        ctx,
                        ctx.t("view.footer.pill.type_key"),
                        "view.footer.pill.search",
                    );
                }
                push_footer_key(
                    &mut pills,
                    ctx,
                    usable_branch_tree_key(km, scopes, &KeyAction::BranchSearch),
                    "view.footer.pill.search",
                );
                push_footer_key(
                    &mut pills,
                    ctx,
                    usable_branch_tree_key(km, scopes, &KeyAction::Checkout),
                    "view.footer.pill.checkout",
                );
                push_footer_key(
                    &mut pills,
                    ctx,
                    usable_branch_tree_key(km, scopes, &KeyAction::NewBranch),
                    "view.footer.pill.new_branch",
                );
                push_footer_key(
                    &mut pills,
                    ctx,
                    usable_branch_tree_key(km, scopes, &KeyAction::ToggleFavorite),
                    "view.footer.pill.favorite",
                );
                push_footer_key(
                    &mut pills,
                    ctx,
                    usable_branch_tree_key(km, global, &KeyAction::GitFetch),
                    "view.footer.pill.fetch",
                );
                push_footer_key(
                    &mut pills,
                    ctx,
                    usable_branch_tree_key(km, global, &KeyAction::GitPull),
                    "view.footer.pill.pull",
                );
                push_footer_key(
                    &mut pills,
                    ctx,
                    usable_branch_tree_key(km, global, &KeyAction::GitPush),
                    "view.footer.pill.push",
                );
                push_footer_key(
                    &mut pills,
                    ctx,
                    usable_branch_tree_key(km, global, &KeyAction::ContextMenu),
                    "view.footer.pill.menu",
                );
                push_footer_key(
                    &mut pills,
                    ctx,
                    usable_branch_tree_key(km, scopes, &KeyAction::RenameBranch),
                    "view.footer.pill.rename",
                );
                push_footer_key(
                    &mut pills,
                    ctx,
                    usable_branch_tree_key(km, scopes, &KeyAction::DeleteBranch),
                    "view.footer.pill.delete",
                );
            }
            (WorkspaceTab::Log, PaneId::LogCommitTable) => {
                let scopes = &[KeymapScope::Log, KeymapScope::Global];
                let global = &[KeymapScope::Global];
                push_footer_key(
                    &mut pills,
                    ctx,
                    km.primary_key_for(scopes, &KeyAction::FocusFilter),
                    "view.footer.pill.filter",
                );
                push_footer_key(
                    &mut pills,
                    ctx,
                    km.primary_key_for(scopes, &KeyAction::Checkout),
                    "view.footer.pill.checkout",
                );
                push_footer_key(
                    &mut pills,
                    ctx,
                    km.primary_key_for(scopes, &KeyAction::CompareOrDiff),
                    "view.footer.pill.diff",
                );
                push_footer_key(
                    &mut pills,
                    ctx,
                    km.primary_key_for(scopes, &KeyAction::NewBranch),
                    "view.footer.pill.new_branch",
                );
                push_footer_key(
                    &mut pills,
                    ctx,
                    km.primary_key_for(scopes, &KeyAction::CopyHash),
                    "view.footer.pill.copy_hash",
                );
                push_footer_key(
                    &mut pills,
                    ctx,
                    km.primary_key_for(global, &KeyAction::ContextMenu),
                    "view.footer.pill.menu",
                );
                push_footer_key(
                    &mut pills,
                    ctx,
                    km.primary_key_for(global, &KeyAction::ToggleBranchesPopup),
                    "view.footer.pill.branches",
                );
                push_footer_key(
                    &mut pills,
                    ctx,
                    km.primary_key_for(global, &KeyAction::GitPull),
                    "view.footer.pill.pull",
                );
                push_footer_key(
                    &mut pills,
                    ctx,
                    km.primary_key_for(global, &KeyAction::GitPush),
                    "view.footer.pill.push",
                );
                push_footer_key(
                    &mut pills,
                    ctx,
                    km.primary_key_for(scopes, &KeyAction::ToggleBranchesPane),
                    "view.footer.pill.branches_pane",
                );
                push_footer_key(
                    &mut pills,
                    ctx,
                    km.primary_key_for(scopes, &KeyAction::GotoRevision),
                    "view.footer.pill.goto",
                );
            }
            (WorkspaceTab::Log, PaneId::LogChangesTree) => {
                let scopes = &[KeymapScope::Log, KeymapScope::Global];
                push_footer_key(
                    &mut pills,
                    ctx,
                    km.primary_key_for(scopes, &KeyAction::CompareOrDiff),
                    "view.footer.pill.diff_toggle",
                );
                push_footer_key(
                    &mut pills,
                    ctx,
                    km.primary_key_for(scopes, &KeyAction::ZoomDiff),
                    "view.footer.pill.zoom",
                );
                push_footer_key(
                    &mut pills,
                    ctx,
                    km.primary_key_for(scopes, &KeyAction::ToggleGroup),
                    "view.footer.pill.group",
                );
                push_footer_key(
                    &mut pills,
                    ctx,
                    km.primary_key_for(&[KeymapScope::Global], &KeyAction::ContextMenu),
                    "view.footer.pill.menu",
                );
                push_footer_lit(&mut pills, ctx, "Esc", "view.footer.pill.back");
            }
            (WorkspaceTab::Log, PaneId::LogDetailsDiff) => {
                if state.details.diff_preview {
                    let scopes = &[KeymapScope::Diff, KeymapScope::Log, KeymapScope::Global];
                    push_footer_key(
                        &mut pills,
                        ctx,
                        km.primary_key_for(scopes, &KeyAction::ZoomDiff),
                        "view.footer.pill.zoom",
                    );
                    push_footer_key(
                        &mut pills,
                        ctx,
                        km.primary_key_for(scopes, &KeyAction::CycleDiffDock),
                        "view.footer.pill.dock",
                    );
                    push_diff_toggle_pills(&mut pills, ctx, km, scopes);
                    push_diff_nav_pills(&mut pills, ctx, km, scopes, true);
                    push_footer_lit(&mut pills, ctx, "Esc", "view.footer.pill.back");
                } else {
                    let scopes = &[KeymapScope::Log, KeymapScope::Global];
                    push_footer_key(
                        &mut pills,
                        ctx,
                        km.primary_key_for(scopes, &KeyAction::CompareOrDiff),
                        "view.footer.pill.diff_toggle",
                    );
                    push_footer_key(
                        &mut pills,
                        ctx,
                        km.primary_key_for(scopes, &KeyAction::ZoomDiff),
                        "view.footer.pill.zoom",
                    );
                    push_footer_key(
                        &mut pills,
                        ctx,
                        km.primary_key_for(scopes, &KeyAction::ChooseParent),
                        "view.footer.pill.parent",
                    );
                    push_footer_key(
                        &mut pills,
                        ctx,
                        km.primary_key_for(&[KeymapScope::Global], &KeyAction::ContextMenu),
                        "view.footer.pill.menu",
                    );
                    push_footer_lit(&mut pills, ctx, "Esc", "view.footer.pill.back");
                }
            }
            (WorkspaceTab::Stash, PaneId::StashList) => {
                let scopes = &[KeymapScope::Stash, KeymapScope::Global];
                push_footer_lit(&mut pills, ctx, "Enter", "view.footer.pill.inspect");
                push_footer_key(
                    &mut pills,
                    ctx,
                    km.primary_key_for(scopes, &KeyAction::StashApply),
                    "view.footer.pill.apply",
                );
                push_footer_key(
                    &mut pills,
                    ctx,
                    km.primary_key_for(scopes, &KeyAction::StashPop),
                    "view.footer.pill.pop",
                );
                push_footer_key(
                    &mut pills,
                    ctx,
                    km.primary_key_for(scopes, &KeyAction::StashNew),
                    "view.footer.pill.stash_new",
                );
                push_footer_key(
                    &mut pills,
                    ctx,
                    km.primary_key_for(scopes, &KeyAction::StashDrop),
                    "view.footer.pill.drop",
                );
                push_footer_key(
                    &mut pills,
                    ctx,
                    km.primary_key_for(scopes, &KeyAction::StashBranch),
                    "view.footer.pill.new_branch",
                );
                push_footer_key(
                    &mut pills,
                    ctx,
                    km.primary_key_for(&[KeymapScope::Global], &KeyAction::ContextMenu),
                    "view.footer.pill.menu",
                );
            }
            (WorkspaceTab::Stash, PaneId::StashDiffPreview) => {
                let scopes = &[KeymapScope::Diff, KeymapScope::Stash, KeymapScope::Global];
                push_diff_toggle_pills(&mut pills, ctx, km, scopes);
                push_diff_nav_pills(&mut pills, ctx, km, scopes, true);
                push_footer_key(
                    &mut pills,
                    ctx,
                    km.primary_key_for(scopes, &KeyAction::ToggleGroup),
                    "view.footer.pill.group",
                );
                push_footer_lit(&mut pills, ctx, "Esc", "view.footer.pill.back");
            }
            (WorkspaceTab::Console, PaneId::ConsoleCommandList) => {
                let global = &[KeymapScope::Global];
                push_footer_key(
                    &mut pills,
                    ctx,
                    km.primary_key_for(global, &KeyAction::FocusNext),
                    "view.footer.pill.detail",
                );
                push_footer_key(
                    &mut pills,
                    ctx,
                    Some(crate::keymap::actions::console_status_probe_key(km)),
                    "view.footer.pill.status_probe",
                );
                push_footer_key(
                    &mut pills,
                    ctx,
                    km.primary_key_for(global, &KeyAction::Refresh),
                    "view.footer.pill.refresh",
                );
            }
            (WorkspaceTab::Console, PaneId::ConsoleOutputDetail) => {
                let global = &[KeymapScope::Global];
                push_footer_lit(&mut pills, ctx, "Esc", "view.footer.pill.back");
                push_footer_key(
                    &mut pills,
                    ctx,
                    km.primary_key_for(global, &KeyAction::Refresh),
                    "view.footer.pill.refresh",
                );
            }
            _ => {}
        }
    }

    let help_pill = if include_help_pill {
        km.primary_key_for(&[KeymapScope::Global], &KeyAction::Help)
            .map(|k| (k, ctx.t("view.footer.pill.help").to_string()))
    } else {
        None
    };

    let pill_badge_style = Style::default()
        .bg(ctx.theme.bg_elevated)
        .fg(ctx.theme.fg_bright)
        .add_modifier(Modifier::BOLD);
    let pill_label_style = Style::default()
        .bg(ctx.theme.bg_panel)
        .fg(ctx.theme.fg_muted);

    let total_w = area.width as usize;
    let help_w = help_pill
        .as_ref()
        .map_or(0, |(k, l)| 2 + k.width() + 2 + l.width() + 1);
    let max_left_w = total_w.saturating_sub(help_w + 1);

    let mut spans: Vec<Span<'static>> = Vec::new();
    spans.push(Span::styled(" ", pill_label_style));
    let mut used_w = 1usize;

    for (key, label) in pills {
        let badge = format!(" {key} ");
        let lbl = format!(" {label}  ");
        let w = badge.width() + lbl.width();
        if used_w + w > max_left_w {
            break;
        }
        used_w += w;
        spans.push(Span::styled(badge, pill_badge_style));
        spans.push(Span::styled(lbl, pill_label_style));
    }

    if let Some((hk, hl)) = help_pill
        && used_w + help_w <= total_w
    {
        let gap = total_w.saturating_sub(used_w + help_w);
        if gap > 0 {
            spans.push(Span::styled(" ".repeat(gap), pill_label_style));
        }
        spans.push(Span::styled(
            format!(" {hk} "),
            Style::default()
                .bg(ctx.theme.bg_elevated)
                .fg(ctx.theme.fg_accent_link)
                .add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(format!(" {hl} "), pill_label_style));
    }

    let footer = Paragraph::new(Line::from(spans)).style(Style::default().bg(ctx.theme.bg_panel));
    frame.render_widget(footer, area);
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
