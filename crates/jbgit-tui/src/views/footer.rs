//! Dynamic footer bar layout, status overrides, clickable hint pills,
//! and reusable modal pill-strip renderer.

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
use crate::keymap::resolve::{
    KeyResolveCtx, collect_claimed_keys, resolve_key_in_scopes,
};
use crate::keymap::{KeyAction, KeySequence, completions_for_prefix_char, scopes_for_context};
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

pub fn footer_bar_height(
    state: &AppState,
    width: u16,
    max_height: u16,
    hover_target: Option<&HitTarget>,
) -> u16 {
    if max_height == 0 || width == 0 {
        return 0;
    }
    if active_footer_override(state, hover_target).is_some() {
        return 1;
    }
    let configured = state.config.keymap.footer_max_lines;
    if configured == 0 {
        return 0;
    }
    let cap = configured.min(max_height) as usize;
    let hints = collect_footer_hints(state);
    let (rows, _) = layout_footer_pills(&hints.pills, hints.help.as_ref(), width as usize, cap);
    (rows.len() as u16).max(1).min(max_height)
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
        None => {
            render_dynamic_footer_hints(frame, area, state, ctx);
        }
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
}

fn render_graph_arrow_footer(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &AppState,
    ctx: &mut RenderContext<'_>,
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
    let suffix = state.i18n.t("view.footer.arrow_jump_suffix");
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FooterPillKind {
    Action(KeyAction),
    Prefix(char),
    Help,
    Static,
}

#[derive(Debug, Clone)]
pub struct FooterPill {
    pub key: String,
    pub label: String,
    pub kind: FooterPillKind,
}

impl FooterPill {
    pub fn new(key: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            label: label.into(),
            kind: FooterPillKind::Static,
        }
    }

    pub fn help(key: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            label: label.into(),
            kind: FooterPillKind::Help,
        }
    }

    fn from_action(action: KeyAction, key: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            label: label.into(),
            kind: FooterPillKind::Action(action),
        }
    }

    fn prefix(key: impl Into<String>, label: impl Into<String>, ch: char) -> Self {
        Self {
            key: key.into(),
            label: label.into(),
            kind: FooterPillKind::Prefix(ch),
        }
    }

    fn action(&self) -> Option<&KeyAction> {
        match &self.kind {
            FooterPillKind::Action(act) => Some(act),
            _ => None,
        }
    }

    fn hit_target(&self) -> Option<HitTarget> {
        match &self.kind {
            FooterPillKind::Action(act) => Some(HitTarget::FooterPillAction(act.clone())),
            FooterPillKind::Prefix(ch) => Some(HitTarget::FooterPrefix(*ch)),
            FooterPillKind::Help => Some(HitTarget::FooterHelp),
            FooterPillKind::Static => None,
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

fn footer_pill_label(state: &AppState, run: &KeyAction, fallback_desc: Option<&str>) -> String {
    let key = match run {
        KeyAction::CompareOrDiff | KeyAction::CompareVersions
            if state.active_tab == WorkspaceTab::Log
                && state.selected_commits_oldest_first().len() == 2 =>
        {
            Some("view.footer.pill.compare_two")
        }
        KeyAction::CompareOrDiff | KeyAction::CompareVersions
            if state.active_tab == WorkspaceTab::Log
                && (state.focused_pane == PaneId::LogChangesTree
                    || (state.focused_pane == PaneId::LogDetailsDiff
                        && !state.details.diff_preview)) =>
        {
            Some("view.footer.pill.diff_toggle")
        }
        KeyAction::ZoomDiff if state.details.diff_zoomed => Some("view.footer.pill.exit_zoom"),
        _ => run.footer_pill_key(),
    };
    match key {
        Some(k) => state.i18n.t(k).to_string(),
        None if matches!(run, KeyAction::Noop) => String::new(),
        None => fallback_desc.unwrap_or(run.as_str()).to_string(),
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
            return Some(FooterPill::help(
                k,
                state.i18n.t("view.footer.pill.help").to_string(),
            ));
        }
    }
    None
}

fn fuse_pair(
    pills: &mut Vec<FooterPill>,
    first_action: KeyAction,
    second_action: KeyAction,
    merged_label: String,
) {
    let Some(i) = pills.iter().position(|p| p.action() == Some(&first_action)) else {
        return;
    };
    let Some(j) = pills
        .iter()
        .position(|p| p.action() == Some(&second_action))
    else {
        return;
    };
    let keep = i.min(j);
    let drop = i.max(j);
    let combined_key = format!("{}/{}", pills[i].key, pills[j].key);
    pills[keep] = FooterPill::new(combined_key, merged_label);
    pills.remove(drop);
}

fn merge_paired_pills(mut pills: Vec<FooterPill>, state: &AppState) -> Vec<FooterPill> {
    let t = |k: &str| state.i18n.t(k).to_string();
    if state.config.keymap.footer_compact_pairs {
        fuse_pair(
            &mut pills,
            KeyAction::PrevHunk,
            KeyAction::NextHunk,
            t("view.footer.pill.hunk"),
        );
        fuse_pair(
            &mut pills,
            KeyAction::PrevFile,
            KeyAction::NextFile,
            t("view.footer.pill.file"),
        );
        fuse_pair(
            &mut pills,
            KeyAction::StageSelection,
            KeyAction::UnstageSelection,
            t("view.footer.pill.stage_pair"),
        );
        fuse_pair(
            &mut pills,
            KeyAction::StashApply,
            KeyAction::StashPop,
            t("view.footer.pill.apply_pop"),
        );
        fuse_pair(
            &mut pills,
            KeyAction::StashDrop,
            KeyAction::StashClear,
            t("view.footer.pill.drop_clear"),
        );
        fuse_pair(
            &mut pills,
            KeyAction::GitPull,
            KeyAction::GitPush,
            t("view.footer.pill.pull_push"),
        );
        fuse_pair(
            &mut pills,
            KeyAction::PageDownHalf,
            KeyAction::PageUpHalf,
            t("view.footer.pill.half_page"),
        );
        fuse_pair(
            &mut pills,
            KeyAction::GotoTop,
            KeyAction::GotoBottom,
            t("view.footer.pill.top_bottom"),
        );
        fuse_pair(
            &mut pills,
            KeyAction::CursorDown,
            KeyAction::CursorUp,
            t("view.footer.pill.move_line"),
        );
    }
    pills
}

fn selected_log_is_merge(state: &AppState) -> bool {
    state
        .log_commit_at(state.log_graph.selected_idx)
        .is_some_and(|c| c.parents.len() >= 2)
}

fn push_dynamic_action_pill(
    pills: &mut Vec<FooterPill>,
    seen_actions: &mut Vec<KeyAction>,
    state: &AppState,
    scopes: &[KeymapScope],
    action: KeyAction,
) {
    if seen_actions.contains(&action) || !is_action_applicable(&action, state) {
        return;
    }
    if let Some(key) = state.config.keymap.primary_key_for(scopes, &action) {
        let label = footer_pill_label(state, &action, None);
        pills.push(FooterPill::from_action(action.clone(), key, label));
        seen_actions.push(action);
    }
}

fn collect_scope_pills(
    ctx: &KeyResolveCtx<'_>,
    scopes: &[KeymapScope],
    scope_idx: usize,
    skip_help: bool,
    seen_actions: &mut Vec<KeyAction>,
    pills: &mut Vec<FooterPill>,
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
            pills.push(FooterPill::from_action(
                b.run.clone(),
                key_disp,
                footer_pill_label(ctx.state, &b.run, b.desc.as_deref()),
            ));
            seen_actions.push(b.run.clone());
        }
    }
}

fn maybe_push_prefix_pills(
    state: &AppState,
    filter_speed_search: bool,
    pills: &mut Vec<FooterPill>,
) {
    if filter_speed_search {
        return;
    }
    let t = |k: &str| state.i18n.t(k).to_string();
    let g_items = completions_for_prefix_char(state, 'g');
    let has_interesting_g = g_items.len() >= 2
        || g_items
            .iter()
            .any(|e| e.action != KeyAction::GotoTop);
    if has_interesting_g {
        pills.push(FooterPill::prefix("g", t("view.footer.pill.prefix_g"), 'g'));
    }
    let z_items = completions_for_prefix_char(state, 'z');
    if !z_items.is_empty() {
        pills.push(FooterPill::prefix("z", t("view.footer.pill.prefix_z"), 'z'));
    }
}

fn collect_footer_hints(state: &AppState) -> FooterHints {
    let km = &state.config.keymap;
    let t = |k: &str| state.i18n.t(k).to_string();

    if state.active_tab == WorkspaceTab::Log && state.log_filter.editing_text {
        return FooterHints {
            pills: vec![
                FooterPill::new("Enter", t("view.footer.pill.apply")),
                FooterPill::new("Alt+R", t("view.footer.pill.regex")),
                FooterPill::new("Alt+C", t("view.footer.pill.case")),
                FooterPill::new("Ctrl+U", t("view.footer.pill.clear")),
                FooterPill::new("Esc", t("view.footer.pill.cancel")),
            ],
            help: None,
        };
    }

    if state.active_tab == WorkspaceTab::Log
        && state.focused_pane == PaneId::LogBranchesTree
        && (state.branches.searching
            || (km.tree_speed_search && !state.branches.query.is_empty()))
    {
        return FooterHints {
            pills: vec![
                FooterPill::new(t("view.footer.pill.type_key"), t("view.footer.pill.filter")),
                FooterPill::new("Enter", t("view.footer.pill.apply")),
                FooterPill::new("Ctrl+U", t("view.footer.pill.clear")),
                FooterPill::new("Esc", t("view.footer.pill.cancel")),
            ],
            help: None,
        };
    }

    if state.details.diff_zoomed {
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
        push_dynamic_action_pill(
            &mut raw_pills,
            &mut seen_actions,
            state,
            &[KeymapScope::Global],
            KeyAction::PageDownHalf,
        );
        push_dynamic_action_pill(
            &mut raw_pills,
            &mut seen_actions,
            state,
            &[KeymapScope::Global],
            KeyAction::PageUpHalf,
        );
        collect_scope_pills(
            &ctx,
            &diff_scopes,
            0,
            false,
            &mut seen_actions,
            &mut raw_pills,
        );
        if selected_log_is_merge(state) {
            push_dynamic_action_pill(
                &mut raw_pills,
                &mut seen_actions,
                state,
                &[KeymapScope::Diff, KeymapScope::Log],
                KeyAction::ChooseParent,
            );
        }
        for pill in &mut raw_pills {
            if pill.action() == Some(&KeyAction::ZoomDiff) && pill.key != "Esc" {
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

    if state.active_tab == WorkspaceTab::Commit && state.focused_pane == PaneId::CommitMessageEditor
    {
        let commit_scopes = [KeymapScope::Commit];
        let mut seen_actions = Vec::new();
        let mut raw_pills = Vec::new();
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
        for extra in [
            KeyAction::CommitAndPush,
            KeyAction::ToggleSignoff,
            KeyAction::CommitHistory,
        ] {
            if !seen_actions.contains(&extra)
                && let Some(k) = km.primary_editor_key_for(&commit_scopes, &extra)
            {
                raw_pills.push(FooterPill::from_action(
                    extra.clone(),
                    k,
                    footer_pill_label(state, &extra, None),
                ));
                seen_actions.push(extra);
            }
        }
        raw_pills.push(FooterPill::new("Ctrl+E", t("view.footer.pill.edit")));
        let (nav_key, nav_label) = if state.stage_workbench.editing_author {
            ("Enter/Esc", t("view.footer.pill.apply"))
        } else {
            ("Esc", t("view.footer.pill.back"))
        };
        raw_pills.push(FooterPill::new(nav_key, nav_label));

        return FooterHints {
            pills: merge_paired_pills(raw_pills, state),
            help: None,
        };
    }

    if state.active_tab == WorkspaceTab::Commit
        && state.focused_pane == PaneId::CommitDiffPreview
        && state.stage_workbench.visual_anchor_row.is_some()
    {
        let diff_scopes = [KeymapScope::Diff, KeymapScope::Commit];
        let mut seen = Vec::new();
        let mut raw_pills = vec![FooterPill::new("j/k", t("view.footer.pill.extend_sel"))];
        for act in [
            KeyAction::ToggleStage,
            KeyAction::StageSelection,
            KeyAction::UnstageSelection,
            KeyAction::RollbackSelection,
        ] {
            push_dynamic_action_pill(&mut raw_pills, &mut seen, state, &diff_scopes, act);
        }
        raw_pills.push(FooterPill::new("Esc", t("view.footer.pill.exit_visual")));
        return FooterHints {
            pills: merge_paired_pills(raw_pills, state),
            help: None,
        };
    }

    if state.active_tab == WorkspaceTab::Log
        && state.focused_pane == PaneId::LogCommitTable
        && state.log_graph.anchor.is_some()
    {
        let log_scopes = [KeymapScope::Log, KeymapScope::Global];
        let mut seen = Vec::new();
        let mut raw_pills = Vec::new();
        push_dynamic_action_pill(
            &mut raw_pills,
            &mut seen,
            state,
            &log_scopes,
            KeyAction::CompareOrDiff,
        );
        push_dynamic_action_pill(
            &mut raw_pills,
            &mut seen,
            state,
            &log_scopes,
            KeyAction::CopyHash,
        );
        raw_pills.push(FooterPill::new("Shift+↑/↓", t("view.footer.pill.extend_sel")));
        raw_pills.push(FooterPill::new("Esc", t("view.footer.pill.clear_sel")));
        push_dynamic_action_pill(
            &mut raw_pills,
            &mut seen,
            state,
            &[KeymapScope::Global],
            KeyAction::ContextMenu,
        );
        return FooterHints {
            pills: merge_paired_pills(raw_pills, state),
            help: resolve_help_pill(state, false, &[]),
        };
    }

    let shadow_scopes = scopes_for_context(state);
    let in_branch_tree =
        state.active_tab == WorkspaceTab::Log && state.focused_pane == PaneId::LogBranchesTree;
    let in_diff = crate::keymap::is_diff_focused(state);
    let filter_speed_search = in_branch_tree && km.tree_speed_search;

    let leading_lit = if filter_speed_search {
        Some((t("view.footer.pill.type_key"), t("view.footer.pill.search")))
    } else if state.active_tab == WorkspaceTab::Log
        && state.focused_pane == PaneId::LogChangesTree
    {
        Some(("Enter".to_string(), t("view.footer.pill.diff")))
    } else if state.active_tab == WorkspaceTab::Stash && state.focused_pane == PaneId::StashList {
        Some(("Enter".to_string(), t("view.footer.pill.inspect")))
    } else if state.active_tab == WorkspaceTab::Console
        && state.focused_pane == PaneId::ConsoleCommandList
    {
        Some(("Enter".to_string(), t("view.footer.pill.detail")))
    } else {
        None
    };

    let mut after_primary_lit = matches!(
        state.focused_pane,
        PaneId::LogChangesTree
            | PaneId::LogDetailsDiff
            | PaneId::CommitDiffPreview
            | PaneId::StashDiffPreview
            | PaneId::ConsoleOutputDetail
    )
    .then(|| ("Esc".to_string(), t("view.footer.pill.back")));

    let mut claimed_keys: Vec<KeySequence> = Vec::new();
    let mut non_global_claimed_keys: Vec<KeySequence> = Vec::new();
    let mut seen_actions: Vec<KeyAction> = Vec::new();
    let mut raw_pills: Vec<FooterPill> = Vec::new();
    let mut inserted_prefixes = false;

    if let Some((k, l)) = leading_lit {
        raw_pills.push(FooterPill::new(k, l));
    }

    for (scope_idx, &scope) in shadow_scopes.iter().enumerate() {
        let bindings = km.bindings_for(scope);
        let display_scope = !(in_branch_tree && scope == KeymapScope::Log)
            && !(in_diff
                && matches!(
                    scope,
                    KeymapScope::Log | KeymapScope::Commit | KeymapScope::Stash
                ));

        if scope == KeymapScope::Global && !inserted_prefixes {
            inserted_prefixes = true;
            // Context-specific dynamic P0 extras before P1 prefixes
            if state.active_tab == WorkspaceTab::Log
                && matches!(
                    state.focused_pane,
                    PaneId::LogCommitTable | PaneId::LogChangesTree | PaneId::LogDetailsDiff
                )
                && selected_log_is_merge(state)
            {
                push_dynamic_action_pill(
                    &mut raw_pills,
                    &mut seen_actions,
                    state,
                    &[KeymapScope::Diff, KeymapScope::Log],
                    KeyAction::ChooseParent,
                );
            }
            if state.active_tab == WorkspaceTab::Console {
                if state.focused_pane == PaneId::ConsoleOutputDetail {
                    for nav in [
                        KeyAction::CursorDown,
                        KeyAction::CursorUp,
                        KeyAction::PageDownHalf,
                        KeyAction::PageUpHalf,
                        KeyAction::GotoTop,
                        KeyAction::GotoBottom,
                    ] {
                        push_dynamic_action_pill(
                            &mut raw_pills,
                            &mut seen_actions,
                            state,
                            &[KeymapScope::Console, KeymapScope::Global],
                            nav,
                        );
                    }
                } else {
                    for nav in [KeyAction::CursorDown, KeyAction::CursorUp] {
                        push_dynamic_action_pill(
                            &mut raw_pills,
                            &mut seen_actions,
                            state,
                            &[KeymapScope::Console, KeymapScope::Global],
                            nav,
                        );
                    }
                }
            }
            if let Some((k, l)) = after_primary_lit.take() {
                raw_pills.push(FooterPill::new(k, l));
            }
            maybe_push_prefix_pills(state, filter_speed_search, &mut raw_pills);
        }

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
                let used_k = 1 + (0..k).map(|j| pills[idx + j].display_width()).sum::<usize>();
                let hidden = rem - k;
                if used_k + overflow_display_width(hidden) + help_w <= total_w {
                    best_k = k;
                    break;
                }
            }
            rows.push((idx..idx + best_k).collect());
            return (rows, rem - best_k);
        }

        let all_used = 1 + (idx..pills.len())
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

/// Reusable styled pill-strip renderer used by both the main footer bar and modals.
pub fn render_pill_strip(
    frame: &mut Frame<'_>,
    area: Rect,
    pills: &[FooterPill],
    right_pill: Option<&FooterPill>,
    ctx: &mut RenderContext<'_>,
    z_index: u8,
    register_hits: bool,
) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    let total_w = area.width as usize;
    let (rows, hidden) = layout_footer_pills(pills, right_pill, total_w, area.height as usize);

    let pill_badge_style = Style::default()
        .bg(ctx.theme.bg_elevated)
        .fg(ctx.theme.fg_bright)
        .add_modifier(Modifier::BOLD);
    let prefix_badge_style = Style::default()
        .bg(ctx.theme.bg_elevated)
        .fg(ctx.theme.fg_accent_link)
        .add_modifier(Modifier::BOLD);
    let hover_badge_style = Style::default()
        .bg(ctx.theme.border_focused)
        .fg(ctx.theme.fg_bright)
        .add_modifier(Modifier::BOLD);
    let pill_label_style = Style::default()
        .bg(ctx.theme.bg_panel)
        .fg(ctx.theme.fg_muted);
    let hover_label_style = Style::default()
        .bg(ctx.theme.bg_panel)
        .fg(ctx.theme.fg_bright);

    let row_count = rows.len();
    let mut lines: Vec<Line<'static>> = Vec::with_capacity(row_count);

    for (row_idx, pill_indices) in rows.into_iter().enumerate() {
        let is_last_row = row_idx + 1 == row_count;
        let row_y = area.y + row_idx as u16;
        let mut spans: Vec<Span<'static>> = Vec::new();
        spans.push(Span::styled(" ", pill_label_style));
        let mut used_w = 1usize;

        for pill_idx in pill_indices {
            let pill = &pills[pill_idx];
            let pw = pill.display_width();
            let target = if register_hits {
                pill.hit_target()
            } else {
                None
            };
            let hovered = target
                .as_ref()
                .is_some_and(|t| ctx.hover_target == Some(t));
            if let Some(t) = target {
                ctx.hit_registry.register(
                    Rect::new(area.x + used_w as u16, row_y, pw.saturating_sub(1) as u16, 1),
                    z_index,
                    t,
                );
            }
            let badge_style = if hovered {
                hover_badge_style
            } else if matches!(pill.kind, FooterPillKind::Prefix(_)) {
                prefix_badge_style
            } else {
                pill_badge_style
            };
            let lbl_style = if hovered {
                hover_label_style
            } else {
                pill_label_style
            };
            used_w += pw;
            spans.push(Span::styled(pill.key_badge(), badge_style));
            spans.push(Span::styled(pill.label_text(), lbl_style));
        }

        if is_last_row && hidden > 0 {
            let ow = overflow_display_width(hidden);
            if used_w + ow <= total_w {
                let badge = overflow_badge(hidden);
                let bw = badge.width() as u16;
                let hovered = register_hits && ctx.hover_target == Some(&HitTarget::FooterOverflow);
                if register_hits {
                    ctx.hit_registry.register(
                        Rect::new(area.x + used_w as u16, row_y, bw, 1),
                        z_index,
                        HitTarget::FooterOverflow,
                    );
                }
                used_w += ow;
                spans.push(Span::styled(
                    badge,
                    if hovered {
                        hover_badge_style
                    } else {
                        Style::default()
                            .bg(ctx.theme.bg_elevated)
                            .fg(ctx.theme.fg_muted)
                    },
                ));
                spans.push(Span::styled(" ", pill_label_style));
            }
        }

        if is_last_row && let Some(help) = right_pill {
            let help_w = help.help_display_width();
            if used_w + help_w <= total_w {
                let gap = total_w.saturating_sub(used_w + help_w);
                if gap > 0 {
                    spans.push(Span::styled(" ".repeat(gap), pill_label_style));
                }
                let hx = area.x + (used_w + gap) as u16;
                let target = if register_hits {
                    help.hit_target()
                } else {
                    None
                };
                let hovered = target
                    .as_ref()
                    .is_some_and(|t| ctx.hover_target == Some(t));
                if let Some(t) = target {
                    ctx.hit_registry
                        .register(Rect::new(hx, row_y, help_w as u16, 1), z_index, t);
                }
                spans.push(Span::styled(
                    help.key_badge(),
                    if hovered {
                        hover_badge_style
                    } else {
                        Style::default()
                            .bg(ctx.theme.bg_elevated)
                            .fg(ctx.theme.fg_accent_link)
                            .add_modifier(Modifier::BOLD)
                    },
                ));
                spans.push(Span::styled(
                    help.help_label_text(),
                    if hovered {
                        hover_label_style
                    } else {
                        pill_label_style
                    },
                ));
            }
        }

        lines.push(Line::from(spans));
    }

    let footer = Paragraph::new(lines).style(Style::default().bg(ctx.theme.bg_panel));
    frame.render_widget(footer, area);
}

fn render_dynamic_footer_hints(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &AppState,
    ctx: &mut RenderContext<'_>,
) {
    if state.config.keymap.footer_max_lines == 0 || area.height == 0 || area.width == 0 {
        return;
    }

    let hints = collect_footer_hints(state);
    render_pill_strip(
        frame,
        area,
        &hints.pills,
        hints.help.as_ref(),
        ctx,
        6,
        state.modal_stack.is_empty(),
    );
}
