//! Dynamic footer bar layout, status overrides, and hint pill rendering.

use jbgit_core::{Oid, PaneId, WorkspaceTab};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use unicode_width::UnicodeWidthStr;

use crate::app::{AppState, ModalLayer, Notification, RenderContext};
use crate::config::KeymapScope;
use crate::hit_test::{GraphHitKind, HitTarget};
use crate::keymap::actions::is_action_applicable;
use crate::keymap::catalog::{self, ContextMode};
use crate::keymap::resolve::{KeyResolveCtx, collect_claimed_keys, resolve_key_in_scopes};
use crate::keymap::{KeyAction, KeySequence, scopes_for_context};
use crate::views::truncate_to_width;

enum FooterOverride<'a> {
    MenuDisabled {
        label: &'a str,
        reason: &'a str,
    },
    Notification(&'a Notification),
    GraphArrow {
        target_row: usize,
        target_oid: Oid,
        is_down: bool,
    },
}

fn active_footer_override<'a>(
    state: &'a AppState,
    hover_target: Option<&'a HitTarget>,
) -> Option<FooterOverride<'a>> {
    if let Some(ModalLayer::ContextMenu {
        items, selected, ..
    }) = state.modal_stack.last()
        && let Some(item) = items.get(*selected)
        && let Some(reason) = item.disabled_reason.as_deref()
    {
        return Some(FooterOverride::MenuDisabled {
            label: &item.label,
            reason,
        });
    }
    if state.modal_stack.is_empty()
        && let Some(n) = state.notification.as_ref()
    {
        return Some(FooterOverride::Notification(n));
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
    }) = hover_target
    {
        return Some(FooterOverride::GraphArrow {
            target_row: *target_row,
            target_oid: *target_oid,
            is_down: *is_down,
        });
    }
    None
}

enum FooterPlanKind {
    Empty,
    Override,
    Dynamic {
        hints: FooterHints,
        rows: Vec<Vec<usize>>,
        hidden: usize,
    },
}

pub struct FooterRenderPlan {
    pub height: u16,
    kind: FooterPlanKind,
}

pub fn plan_footer_bar(
    state: &AppState,
    width: u16,
    max_height: u16,
    hover_target: Option<&HitTarget>,
) -> FooterRenderPlan {
    if max_height == 0 || width == 0 {
        return FooterRenderPlan {
            height: 0,
            kind: FooterPlanKind::Empty,
        };
    }
    if active_footer_override(state, hover_target).is_some() {
        return FooterRenderPlan {
            height: 1,
            kind: FooterPlanKind::Override,
        };
    }
    let configured = state.config.keymap.footer_max_lines;
    if configured == 0 {
        return FooterRenderPlan {
            height: 0,
            kind: FooterPlanKind::Empty,
        };
    }
    let cap = configured.min(max_height) as usize;
    let hints = collect_footer_hints(state);
    let (rows, hidden) =
        layout_footer_pills(&hints.pills, hints.help.as_ref(), width as usize, cap);
    let height = (rows.len() as u16).max(1).min(max_height);
    FooterRenderPlan {
        height,
        kind: FooterPlanKind::Dynamic {
            hints,
            rows,
            hidden,
        },
    }
}

pub fn render_footer_bar_with_plan(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &AppState,
    ctx: &mut RenderContext<'_>,
    plan: &FooterRenderPlan,
) {
    if area.height == 0 || area.width == 0 {
        return;
    }

    match active_footer_override(state, ctx.hover_target) {
        Some(FooterOverride::MenuDisabled { label, reason }) => {
            frame.render_widget(
                Paragraph::new(Span::styled(
                    truncate_to_width(&format!(" ⊘ {label}: {reason}"), area.width as usize),
                    Style::default().fg(ctx.theme.status_untracked),
                ))
                .style(Style::default().bg(ctx.theme.bg_panel)),
                area,
            );
        }
        Some(FooterOverride::Notification(n)) => {
            render_notification_footer(frame, area, state, ctx, n);
        }
        Some(FooterOverride::GraphArrow {
            target_row,
            target_oid,
            is_down,
        }) => {
            render_graph_arrow_footer(frame, area, state, ctx, target_row, target_oid, is_down);
        }
        None => match &plan.kind {
            FooterPlanKind::Dynamic {
                hints,
                rows,
                hidden,
            } => render_dynamic_footer_hints_from_plan(frame, area, ctx, hints, rows, *hidden),
            FooterPlanKind::Empty | FooterPlanKind::Override => {}
        },
    }
}

fn render_notification_footer(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &AppState,
    ctx: &mut RenderContext<'_>,
    n: &Notification,
) {
    let icon = if n.is_error { "✕" } else { "✔" };
    let color = if n.is_error {
        ctx.theme.status_conflicted
    } else {
        ctx.theme.status_added
    };
    let action = n.action.as_ref().map(|(l, _)| format!(" [{l}] "));
    let dismiss = " ✕ ";
    let hint = state.i18n.t(if n.action.is_some() {
        "view.header.notification_hint"
    } else {
        "view.header.notification_dismiss_hint"
    });
    let right_w = action.as_ref().map_or(0, |a| a.width() + 1) + dismiss.width() + 1 + hint.width();
    let text = truncate_to_width(
        &format!(" {icon} {}", n.text),
        (area.width as usize).saturating_sub(right_w),
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
    spans.push(Span::styled(
        dismiss,
        Style::default().fg(ctx.theme.fg_muted),
    ));
    ctx.hit_registry.register(
        Rect::new(x, area.y, dismiss.width() as u16, 1),
        6,
        HitTarget::NotificationDismiss,
    );
    spans.push(Span::styled(hint, Style::default().fg(ctx.theme.fg_muted)));
    frame.render_widget(
        Paragraph::new(Line::from(spans)).style(Style::default().bg(ctx.theme.bg_panel)),
        area,
    );
}

fn render_graph_arrow_footer(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &AppState,
    ctx: &RenderContext<'_>,
    target_row: usize,
    target_oid: Oid,
    is_down: bool,
) {
    let g = &ctx.glyph_mode.symbols().graph;
    let arrow = if is_down { g.arrow_down } else { g.arrow_up };
    let dir_label = if is_down {
        state.i18n.t("view.footer.arrow_parent")
    } else {
        state.i18n.t("view.footer.arrow_child")
    };
    let target_subj = state
        .log_commit_at(target_row)
        .map(|c| c.subject.as_str())
        .unwrap_or("");
    let prefix = state.i18n.t_fmt(
        "view.footer.arrow_jump_prefix",
        &[("arrow", arrow), ("dir", dir_label)],
    );
    let short = target_oid.short();
    let back_key = crate::keymap::catalog::shortcut_for_action(
        state,
        &[KeymapScope::Log, KeymapScope::Global],
        &KeyAction::NavLogBack,
    );
    let suffix = match back_key.as_deref() {
        Some(key) => state
            .i18n
            .t_fmt("view.footer.arrow_jump_suffix", &[("key", key)]),
        None => state
            .i18n
            .t("view.footer.arrow_jump_suffix_no_key")
            .to_string(),
    };
    let subj_budget =
        (area.width as usize).saturating_sub(prefix.width() + short.width() + 2 + suffix.width());
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
}

struct RawPill {
    action: Option<KeyAction>,
    key: String,
    label: String,
    emphasized: bool,
}

struct FooterPill {
    key: String,
    label: String,
    emphasized: bool,
}

impl FooterPill {
    fn new(key: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            label: label.into(),
            emphasized: false,
        }
    }

    fn with_emphasized(key: impl Into<String>, label: impl Into<String>, emphasized: bool) -> Self {
        Self {
            key: key.into(),
            label: label.into(),
            emphasized,
        }
    }

    fn key_badge(&self) -> String {
        format!(" {} ", self.key)
    }

    fn label_text(&self) -> String {
        format!(" {}  ", self.label)
    }

    fn display_width(&self) -> usize {
        self.key_badge().width() + self.label_text().width()
    }

    fn help_label_text(&self) -> String {
        format!(" {} ", self.label)
    }

    fn help_display_width(&self) -> usize {
        self.key_badge().width() + self.help_label_text().width()
    }
}

struct FooterHints {
    pills: Vec<FooterPill>,
    help: Option<FooterPill>,
}

fn overflow_badge(hidden: usize) -> String {
    format!(" +{hidden} ")
}

fn overflow_display_width(hidden: usize) -> usize {
    if hidden > 0 {
        overflow_badge(hidden).width() + 1
    } else {
        0
    }
}

fn resolve_help_pill(
    state: &AppState,
    filter_speed_search: bool,
    claimed_keys: &[KeySequence],
) -> Option<FooterPill> {
    if !is_action_applicable(&KeyAction::Help, state) {
        return None;
    }
    let ctx = KeyResolveCtx {
        state,
        filter_speed_search,
        editor_only: false,
        claimed_keys,
    };
    let global_scopes = [KeymapScope::Global];
    let global_bindings = state.config.keymap.bindings_for(KeymapScope::Global);
    for (idx, b) in global_bindings.iter().enumerate() {
        if b.run == KeyAction::Help
            && b.show_hint == Some(true)
            && let Some(k) = resolve_key_in_scopes(&ctx, &global_scopes, 0, idx, b)
        {
            return Some(FooterPill::new(
                k,
                state.i18n.t("view.footer.pill.help").to_string(),
            ));
        }
    }
    None
}

fn fuse_pair(
    pills: &mut Vec<RawPill>,
    first_action: KeyAction,
    second_action: KeyAction,
    merged_label: String,
) {
    let Some(i) = pills
        .iter()
        .position(|p| p.action.as_ref() == Some(&first_action))
    else {
        return;
    };
    let Some(j) = pills
        .iter()
        .position(|p| p.action.as_ref() == Some(&second_action))
    else {
        return;
    };
    let keep = i.min(j);
    let drop = i.max(j);
    let combined_key = format!("{}/{}", pills[i].key, pills[j].key);
    pills[keep] = RawPill {
        action: None,
        key: combined_key,
        label: merged_label,
        emphasized: false,
    };
    pills.remove(drop);
}

fn merge_paired_pills(mut raw: Vec<RawPill>, state: &AppState) -> Vec<FooterPill> {
    fuse_pair(
        &mut raw,
        KeyAction::PrevHunk,
        KeyAction::NextHunk,
        state.i18n.t("view.footer.pill.hunk").to_string(),
    );
    fuse_pair(
        &mut raw,
        KeyAction::PrevFile,
        KeyAction::NextFile,
        state.i18n.t("view.footer.pill.file").to_string(),
    );
    fuse_pair(
        &mut raw,
        KeyAction::PrevCommit,
        KeyAction::NextCommit,
        state.i18n.t("view.footer.pill.commit").to_string(),
    );
    raw.into_iter()
        .map(|p| FooterPill::with_emphasized(p.key, p.label, p.emphasized))
        .collect()
}

fn collect_scope_pills(
    ctx: &KeyResolveCtx<'_>,
    scopes: &[KeymapScope],
    scope_idx: usize,
    skip_help: bool,
    seen_actions: &mut Vec<KeyAction>,
    raw_pills: &mut Vec<RawPill>,
) {
    let bindings = ctx.state.config.keymap.bindings_for(scopes[scope_idx]);
    for (idx, b) in bindings.iter().enumerate() {
        if b.show_hint != Some(true)
            || (skip_help && b.run == KeyAction::Help)
            || seen_actions.contains(&b.run)
            || !is_action_applicable(&b.run, ctx.state)
        {
            continue;
        }
        if let Some(key_disp) = resolve_key_in_scopes(ctx, scopes, scope_idx, idx, b) {
            raw_pills.push(RawPill {
                action: Some(b.run.clone()),
                key: key_disp,
                label: catalog::footer_pill_label(ctx.state, &b.run, b.desc.as_deref()),
                emphasized: false,
            });
            seen_actions.push(b.run.clone());
        }
    }
}

fn collect_footer_hints(state: &AppState) -> FooterHints {
    let km = &state.config.keymap;
    let t = |k: &str| state.i18n.t(k).to_string();

    match catalog::context_mode(state) {
        ContextMode::FilterTextEditing => {
            return FooterHints {
                pills: catalog::filter_editing_hints(state)
                    .into_iter()
                    .map(|h| FooterPill::new(h.key, h.label))
                    .collect(),
                help: None,
            };
        }
        ContextMode::BranchSpeedSearch => {
            return FooterHints {
                pills: catalog::branch_speed_search_hints(state)
                    .into_iter()
                    .map(|h| FooterPill::new(h.key, h.label))
                    .collect(),
                help: None,
            };
        }
        ContextMode::ZoomedDiff => {
            let diff_scopes = [KeymapScope::Diff];
            let mut claimed_keys = Vec::new();
            let mut seen_actions = Vec::new();
            let mut raw_pills = Vec::new();
            let ctx = KeyResolveCtx {
                state,
                filter_speed_search: false,
                editor_only: false,
                claimed_keys: &claimed_keys,
            };
            collect_scope_pills(
                &ctx,
                &diff_scopes,
                0,
                false,
                &mut seen_actions,
                &mut raw_pills,
            );
            for pill in &mut raw_pills {
                if pill.action.as_ref() == Some(&KeyAction::ZoomDiff) && pill.key != "Esc" {
                    pill.key = format!("{}/Esc", pill.key);
                }
            }
            collect_claimed_keys(
                km.bindings_for(KeymapScope::Diff),
                state,
                false,
                &mut claimed_keys,
                None,
            );
            return FooterHints {
                pills: merge_paired_pills(raw_pills, state),
                help: resolve_help_pill(state, false, &claimed_keys),
            };
        }
        ContextMode::CommitEditor { editing_author } => {
            let commit_scopes = [KeymapScope::Commit];
            let mut seen_actions = Vec::new();
            let mut raw_pills = Vec::new();
            let esc_hint = catalog::commit_editor_structural_hints(state, editing_author);
            let esc_label = if editing_author {
                esc_hint.label
            } else {
                t("view.footer.pill.exit_edit")
            };
            raw_pills.push(RawPill {
                action: None,
                key: esc_hint.key,
                label: esc_label,
                emphasized: true,
            });

            let ctx = KeyResolveCtx {
                state,
                filter_speed_search: false,
                editor_only: true,
                claimed_keys: &[],
            };
            collect_scope_pills(
                &ctx,
                &commit_scopes,
                0,
                false,
                &mut seen_actions,
                &mut raw_pills,
            );

            return FooterHints {
                pills: merge_paired_pills(raw_pills, state),
                help: None,
            };
        }
        ContextMode::Normal => {}
    }

    let shadow_scopes = scopes_for_context(state);
    let in_branch_tree =
        state.active_tab == WorkspaceTab::Log && state.focused_pane == PaneId::LogBranchesTree;
    let filter_speed_search = in_branch_tree && km.tree_speed_search;
    let structural = catalog::normal_structural_hints(state);
    let mut after_primary_lit = structural.after_primary;

    let mut claimed_keys: Vec<KeySequence> = Vec::new();
    let mut non_global_claimed_keys: Vec<KeySequence> = Vec::new();
    let mut seen_actions: Vec<KeyAction> = Vec::new();
    let mut raw_pills: Vec<RawPill> = Vec::new();

    if let Some(lead) = structural.leading {
        raw_pills.push(RawPill {
            action: lead.action,
            key: lead.key,
            label: lead.label,
            emphasized: false,
        });
    }

    for (scope_idx, &scope) in shadow_scopes.iter().enumerate() {
        let bindings = km.bindings_for(scope);
        let display_scope = !(in_branch_tree && scope == KeymapScope::Log);

        if display_scope {
            let ctx = KeyResolveCtx {
                state,
                filter_speed_search,
                editor_only: false,
                claimed_keys: &claimed_keys,
            };
            collect_scope_pills(
                &ctx,
                &shadow_scopes,
                scope_idx,
                true,
                &mut seen_actions,
                &mut raw_pills,
            );

            // Prevent lower-priority scopes from re-advertising an action explicitly hidden here.
            for b in bindings {
                if b.show_hint == Some(false)
                    && is_action_applicable(&b.run, state)
                    && !seen_actions.contains(&b.run)
                {
                    seen_actions.push(b.run.clone());
                }
            }

            if let Some(lit) = after_primary_lit.take() {
                raw_pills.push(RawPill {
                    action: lit.action,
                    key: lit.key,
                    label: lit.label,
                    emphasized: false,
                });
            }
        }

        let ng = (scope != KeymapScope::Global).then_some(&mut non_global_claimed_keys);
        collect_claimed_keys(bindings, state, filter_speed_search, &mut claimed_keys, ng);
    }

    FooterHints {
        pills: merge_paired_pills(raw_pills, state),
        help: resolve_help_pill(state, filter_speed_search, &non_global_claimed_keys),
    }
}

fn layout_footer_pills(
    pills: &[FooterPill],
    help_pill: Option<&FooterPill>,
    total_w: usize,
    max_lines: usize,
) -> (Vec<Vec<usize>>, usize) {
    if max_lines == 0 {
        return (Vec::new(), pills.len());
    }

    let help_w = help_pill.map_or(0, FooterPill::help_display_width);
    let mut rows: Vec<Vec<usize>> = Vec::new();
    let mut idx = 0usize;

    for line_idx in 0..max_lines {
        let is_last_allowed = line_idx + 1 == max_lines;
        let rem = pills.len() - idx;

        if is_last_allowed {
            let mut best_k = 0usize;
            for k in (0..=rem).rev() {
                let used_k = 1
                    + (0..k)
                        .map(|j| pills[idx + j].display_width())
                        .sum::<usize>();
                let hidden = rem - k;
                if used_k + overflow_display_width(hidden) + help_w <= total_w {
                    best_k = k;
                    break;
                }
            }
            rows.push((idx..idx + best_k).collect());
            return (rows, rem - best_k);
        }

        let all_used = 1
            + (idx..pills.len())
                .map(|i| pills[i].display_width())
                .sum::<usize>();
        if all_used + help_w <= total_w {
            rows.push((idx..pills.len()).collect());
            return (rows, 0);
        }

        let mut row = Vec::new();
        let mut used = 1usize;
        while idx < pills.len() {
            let w = pills[idx].display_width();
            if used + w <= total_w {
                used += w;
                row.push(idx);
                idx += 1;
            } else if row.is_empty() {
                row.push(idx);
                idx += 1;
                break;
            } else {
                break;
            }
        }
        rows.push(row);

        if idx == pills.len() && help_pill.is_none() {
            return (rows, 0);
        }
    }

    (rows, pills.len() - idx)
}

fn render_dynamic_footer_hints_from_plan(
    frame: &mut Frame<'_>,
    area: Rect,
    ctx: &RenderContext<'_>,
    hints: &FooterHints,
    rows: &[Vec<usize>],
    hidden: usize,
) {
    if area.height == 0 || area.width == 0 {
        return;
    }

    let total_w = area.width as usize;
    let visible_rows = rows.len().min(area.height as usize);

    let pill_badge_style = Style::default()
        .bg(ctx.theme.bg_elevated)
        .fg(ctx.theme.fg_bright)
        .add_modifier(Modifier::BOLD);
    let pill_label_style = Style::default()
        .bg(ctx.theme.bg_panel)
        .fg(ctx.theme.fg_muted);
    let emphasized_badge_style = Style::default()
        .bg(ctx.theme.border_focused)
        .fg(ctx.theme.fg_bright)
        .add_modifier(Modifier::BOLD);
    let emphasized_label_style = Style::default()
        .bg(ctx.theme.bg_panel)
        .fg(ctx.theme.fg_bright)
        .add_modifier(Modifier::BOLD);

    let mut lines: Vec<Line<'static>> = Vec::with_capacity(visible_rows);

    for (row_idx, pill_indices) in rows.iter().take(visible_rows).enumerate() {
        let is_last_row = row_idx + 1 == visible_rows;
        let mut spans: Vec<Span<'static>> = Vec::new();
        spans.push(Span::styled(" ", pill_label_style));
        let mut used_w = 1usize;

        for &pill_idx in pill_indices {
            let pill = &hints.pills[pill_idx];
            used_w += pill.display_width();
            let (badge_style, label_style) = if pill.emphasized {
                (emphasized_badge_style, emphasized_label_style)
            } else {
                (pill_badge_style, pill_label_style)
            };
            spans.push(Span::styled(pill.key_badge(), badge_style));
            spans.push(Span::styled(pill.label_text(), label_style));
        }

        if is_last_row && hidden > 0 {
            let ow = overflow_display_width(hidden);
            if used_w + ow <= total_w {
                used_w += ow;
                spans.push(Span::styled(
                    overflow_badge(hidden),
                    Style::default()
                        .bg(ctx.theme.bg_elevated)
                        .fg(ctx.theme.fg_muted),
                ));
                spans.push(Span::styled(" ", pill_label_style));
            }
        }

        if is_last_row && let Some(help) = &hints.help {
            let help_w = help.help_display_width();
            if used_w + help_w <= total_w {
                let gap = total_w.saturating_sub(used_w + help_w);
                if gap > 0 {
                    spans.push(Span::styled(" ".repeat(gap), pill_label_style));
                }
                spans.push(Span::styled(
                    help.key_badge(),
                    Style::default()
                        .bg(ctx.theme.bg_elevated)
                        .fg(ctx.theme.fg_accent_link)
                        .add_modifier(Modifier::BOLD),
                ));
                spans.push(Span::styled(help.help_label_text(), pill_label_style));
            }
        }

        lines.push(Line::from(spans));
    }

    let footer = Paragraph::new(lines).style(Style::default().bg(ctx.theme.bg_panel));
    frame.render_widget(footer, area);
}
