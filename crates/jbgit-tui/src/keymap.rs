//! Dynamic keymap parser, sequence matcher, action dispatcher, Which-Key completions,
//! and searchable/executable command-help builder.

use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use jbgit_core::{BranchNodeKind, GitOp, PaneId, WorkspaceTab};

use crate::app::{
    Action, AppState, ContextMenuAction, ContextMenuEntryKind, ContextMenuItem, PopupKind,
};
use crate::config::keymap_config::{KeyBindingConfig, KeymapScope};
use crate::hit_test::StashButton;

pub const KEY_CHORD_WINDOW: Duration = Duration::from_millis(1200);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct KeyStroke {
    pub code: KeyCode,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

fn parse_named_key(name: &str) -> Option<KeyCode> {
    match name.to_ascii_lowercase().as_str() {
        "enter" | "return" => Some(KeyCode::Enter),
        "esc" | "escape" => Some(KeyCode::Esc),
        "space" => Some(KeyCode::Char(' ')),
        "tab" => Some(KeyCode::Tab),
        "backtab" => Some(KeyCode::BackTab),
        "backspace" => Some(KeyCode::Backspace),
        "del" | "delete" => Some(KeyCode::Delete),
        "up" => Some(KeyCode::Up),
        "down" => Some(KeyCode::Down),
        "left" => Some(KeyCode::Left),
        "right" => Some(KeyCode::Right),
        "home" => Some(KeyCode::Home),
        "end" => Some(KeyCode::End),
        "pageup" | "pgup" => Some(KeyCode::PageUp),
        "pagedown" | "pgdn" => Some(KeyCode::PageDown),
        f if f.starts_with('f') && f.len() > 1 && f[1..].chars().all(|c| c.is_ascii_digit()) => {
            f[1..].parse::<u8>().ok().map(KeyCode::F)
        }
        _ => None,
    }
}

impl KeyStroke {
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim();
        if s.is_empty() {
            return None;
        }

        // Bracketed notation, e.g. "<C-d>", "<A-1>", "<Space>", "<C-Enter>", "<F2>", "<C-S-k>"
        if s.starts_with('<') && s.ends_with('>') && s.len() >= 3 {
            let inner = &s[1..s.len() - 1];
            let parts: Vec<&str> = inner.split('-').collect();
            if parts.is_empty() {
                return None;
            }

            let mut ctrl = false;
            let mut alt = false;
            let mut shift = false;

            for &m in &parts[..parts.len() - 1] {
                match m.to_ascii_lowercase().as_str() {
                    "c" | "ctrl" | "control" => ctrl = true,
                    "a" | "alt" => alt = true,
                    "s" | "shift" => shift = true,
                    _ => {}
                }
            }

            let key_part = parts[parts.len() - 1];
            let parsed_code = parse_named_key(key_part).or_else(|| {
                let mut chars = key_part.chars();
                let first = chars.next()?;
                chars.next().is_none().then_some(KeyCode::Char(first))
            })?;

            // Normalize <S-Tab> / <BackTab> so equality & matching agree.
            let (code, shift) =
                if (parsed_code == KeyCode::Tab && shift) || parsed_code == KeyCode::BackTab {
                    (KeyCode::BackTab, false)
                } else {
                    (parsed_code, shift)
                };

            Some(KeyStroke {
                code,
                ctrl,
                alt,
                shift,
            })
        } else {
            // Non-bracketed representation: "q", "G", "j", "k", "r", "?", etc.
            let mut chars = s.chars();
            let first = chars.next()?;
            if chars.next().is_none() {
                let shift = first.is_ascii_uppercase();
                Some(KeyStroke {
                    code: KeyCode::Char(first),
                    ctrl: false,
                    alt: false,
                    shift,
                })
            } else {
                let code = parse_named_key(s)?;
                Some(KeyStroke {
                    code,
                    ctrl: false,
                    alt: false,
                    shift: false,
                })
            }
        }
    }

    pub fn matches(&self, event: &KeyEvent) -> bool {
        let ctrl = event.modifiers.contains(KeyModifiers::CONTROL);
        let alt = event.modifiers.contains(KeyModifiers::ALT);
        let shift = event.modifiers.contains(KeyModifiers::SHIFT);

        if self.ctrl != ctrl || self.alt != alt {
            return false;
        }

        match (self.code, event.code) {
            (KeyCode::Char(a), KeyCode::Char(b)) => {
                if self.ctrl || self.alt {
                    if a.is_ascii_alphabetic() {
                        a.eq_ignore_ascii_case(&b) && self.shift == shift
                    } else {
                        a == b
                    }
                } else if a.is_ascii_uppercase() {
                    a == b || (a.to_ascii_lowercase() == b && shift)
                } else if a.is_ascii_lowercase() {
                    a == b && !shift
                } else if a == ' ' {
                    b == ' ' && self.shift == shift
                } else {
                    // Shifted punctuation like '?', '{', '}', '+' may include SHIFT in crossterm.
                    a == b
                }
            }
            (KeyCode::BackTab, KeyCode::BackTab) => true,
            (KeyCode::BackTab, KeyCode::Tab) => shift,
            (KeyCode::Tab, KeyCode::BackTab) => self.shift,
            (a, b) => a == b && self.shift == shift,
        }
    }

    pub fn to_display_string(&self) -> String {
        let mut s = String::new();
        if self.ctrl {
            s.push_str("Ctrl+");
        }
        if self.alt {
            s.push_str("Alt+");
        }
        let suppress_shift = matches!(self.code, KeyCode::BackTab)
            || matches!(self.code, KeyCode::Char(c) if c.is_ascii_uppercase())
            || (self.code == KeyCode::Tab && self.shift);
        if self.shift && !suppress_shift {
            s.push_str("Shift+");
        }
        match self.code {
            KeyCode::Char(' ') => s.push_str("Space"),
            KeyCode::Char(c) if (self.ctrl || self.alt) && c.is_ascii_alphabetic() => {
                s.push(c.to_ascii_uppercase());
            }
            KeyCode::Char(c) => s.push(c),
            KeyCode::Enter => s.push_str("Enter"),
            KeyCode::Esc => s.push_str("Esc"),
            KeyCode::Backspace => s.push_str("Backspace"),
            KeyCode::Delete => s.push_str("Del"),
            KeyCode::Up => s.push('↑'),
            KeyCode::Down => s.push('↓'),
            KeyCode::Left => s.push('←'),
            KeyCode::Right => s.push('→'),
            KeyCode::Home => s.push_str("Home"),
            KeyCode::End => s.push_str("End"),
            KeyCode::PageUp => s.push_str("PgUp"),
            KeyCode::PageDown => s.push_str("PgDn"),
            KeyCode::Tab if self.shift => s.push_str("Shift+Tab"),
            KeyCode::Tab => s.push_str("Tab"),
            KeyCode::BackTab => s.push_str("Shift+Tab"),
            KeyCode::F(n) => s.push_str(&format!("F{n}")),
            _ => s.push('?'),
        }
        s
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeySequence {
    pub strokes: Vec<KeyStroke>,
}

impl KeySequence {
    pub fn parse(keys: &[String]) -> Option<Self> {
        if keys.is_empty() {
            return None;
        }
        let mut strokes = Vec::with_capacity(keys.len());
        for k in keys {
            strokes.push(KeyStroke::parse(k)?);
        }
        Some(Self { strokes })
    }

    pub fn matches_exact(&self, events: &[KeyEvent]) -> bool {
        if self.strokes.len() != events.len() {
            return false;
        }
        self.strokes
            .iter()
            .zip(events.iter())
            .all(|(stroke, ev)| stroke.matches(ev))
    }

    pub fn matches_prefix(&self, events: &[KeyEvent]) -> bool {
        if events.is_empty() || events.len() >= self.strokes.len() {
            return false;
        }
        self.strokes[..events.len()]
            .iter()
            .zip(events.iter())
            .all(|(stroke, ev)| stroke.matches(ev))
    }

    pub fn to_display_string(&self) -> String {
        self.strokes
            .iter()
            .map(|s| s.to_display_string())
            .collect::<Vec<_>>()
            .join(" ")
    }
}

pub enum MatchResult<'a> {
    Exact(&'a KeyBindingConfig),
    Prefix,
    None,
}

pub fn match_bindings<'a>(
    bindings: &'a [KeyBindingConfig],
    events: &[KeyEvent],
) -> MatchResult<'a> {
    match_bindings_filtered(bindings, events, |_| true)
}

pub fn match_bindings_filtered<'a, F>(
    bindings: &'a [KeyBindingConfig],
    events: &[KeyEvent],
    mut allow: F,
) -> MatchResult<'a>
where
    F: FnMut(&KeyBindingConfig) -> bool,
{
    let mut prefix_found = false;

    for binding in bindings {
        if !allow(binding) {
            continue;
        }
        if let Some(seq) = KeySequence::parse(&binding.on) {
            if seq.matches_exact(events) {
                return MatchResult::Exact(binding);
            }
            if seq.matches_prefix(events) {
                prefix_found = true;
            }
        }
    }

    if prefix_found {
        MatchResult::Prefix
    } else {
        MatchResult::None
    }
}

fn find_action_by_mnemonic(items: Vec<ContextMenuItem>, mnemonic: char) -> Option<Vec<Action>> {
    let target = mnemonic.to_ascii_lowercase();
    for item in items {
        if item.disabled_reason.is_some() {
            continue;
        }
        let matches = item.mnemonic.is_some_and(|m| m.to_ascii_lowercase() == target);
        match item.kind {
            ContextMenuEntryKind::Action(ContextMenuAction::Run(actions)) if matches => {
                return Some(actions);
            }
            ContextMenuEntryKind::Submenu(sub) if matches => {
                if let Some(first) = sub.into_iter().find_map(|s| match s.kind {
                    ContextMenuEntryKind::Action(ContextMenuAction::Run(a))
                        if s.disabled_reason.is_none() =>
                    {
                        Some(a)
                    }
                    _ => None,
                }) {
                    return Some(first);
                }
            }
            ContextMenuEntryKind::Submenu(sub) => {
                if let Some(found) = find_action_by_mnemonic(sub, mnemonic) {
                    return Some(found);
                }
            }
            _ => {}
        }
    }
    None
}

fn branch_menu_action_by_mnemonic(state: &AppState, mnemonic: char) -> Option<Vec<Action>> {
    let node = state.branches.selected_node()?;
    let BranchNodeKind::Leaf(kind) = node.kind else {
        return None;
    };
    let ref_name = node.ref_name.as_deref()?;
    let items = crate::menus::branch_menu_items(state, ref_name, kind);
    find_action_by_mnemonic(items, mnemonic)
}

pub fn is_diff_focused(state: &AppState) -> bool {
    match state.active_tab {
        WorkspaceTab::Log => {
            state.details.diff_zoomed
                || (state.focused_pane == PaneId::LogDetailsDiff && state.details.diff_preview)
        }
        WorkspaceTab::Commit => state.focused_pane == PaneId::CommitDiffPreview,
        WorkspaceTab::Stash => state.focused_pane == PaneId::StashDiffPreview,
        WorkspaceTab::Console => false,
    }
}

pub fn is_action_applicable(run: &str, state: &AppState) -> bool {
    match run {
        "noop" | "" => false,
        "quit" | "help" => true,
        _ if state.details.diff_zoomed => matches!(
            run,
            "cursor_down"
                | "cursor_up"
                | "goto_top"
                | "goto_bottom"
                | "prev_hunk"
                | "next_hunk"
                | "prev_file"
                | "next_file"
                | "toggle_side_by_side"
                | "cycle_diff_context"
                | "zoom_diff"
                | "choose_parent"
        ),
        "toggle_visual"
        | "rollback_selection"
        | "undo_rollback"
        | "switch_diff_side"
        | "three_way_inspector" => {
            state.active_tab == WorkspaceTab::Commit
                && state.focused_pane == PaneId::CommitDiffPreview
        }
        "stage_selection" | "unstage_selection" => {
            state.active_tab == WorkspaceTab::Commit
                && matches!(
                    state.focused_pane,
                    PaneId::CommitDiffPreview | PaneId::CommitChangesTree
                )
        }
        "toggle_stage" => state.active_tab == WorkspaceTab::Commit,
        "cycle_diff_dock" => state.active_tab == WorkspaceTab::Log,
        "zoom_diff" => {
            state.active_tab == WorkspaceTab::Log || state.focused_pane == PaneId::StashDiffPreview
        }
        "prev_hunk"
        | "next_hunk"
        | "prev_file"
        | "next_file"
        | "toggle_side_by_side"
        | "cycle_diff_context" => is_diff_focused(state),
        "checkout" => {
            state.active_tab == WorkspaceTab::Log
                && matches!(
                    state.focused_pane,
                    PaneId::LogBranchesTree | PaneId::LogCommitTable
                )
        }
        "copy_hash" | "uncommit" | "reword" | "drop_commit" => {
            state.active_tab == WorkspaceTab::Log && state.focused_pane == PaneId::LogCommitTable
        }
        "choose_parent" => {
            state.active_tab == WorkspaceTab::Log
                && matches!(
                    state.focused_pane,
                    PaneId::LogCommitTable | PaneId::LogChangesTree | PaneId::LogDetailsDiff
                )
        }
        "compare_or_diff"
        | "compare_versions"
        | "goto_revision"
        | "focus_filter"
        | "toggle_branches_pane"
        | "page_down_half"
        | "page_up_half" => state.active_tab == WorkspaceTab::Log,
        "branch_search" | "toggle_favorite" | "rename_branch" | "delete_branch" | "navigate_branch" => {
            state.active_tab == WorkspaceTab::Log && state.focused_pane == PaneId::LogBranchesTree
        }
        "focus_commit_msg" | "select_all" | "discard_changes" => {
            state.active_tab == WorkspaceTab::Commit
                && state.focused_pane == PaneId::CommitChangesTree
        }
        "commit" | "commit_and_push" | "toggle_amend" | "toggle_signoff" | "commit_history" => {
            state.active_tab == WorkspaceTab::Commit
        }
        "edit_file" => {
            state.active_tab == WorkspaceTab::Commit
                && matches!(
                    state.focused_pane,
                    PaneId::CommitChangesTree | PaneId::CommitDiffPreview
                )
        }
        "toggle_group" => matches!(
            state.focused_pane,
            PaneId::CommitChangesTree | PaneId::LogChangesTree | PaneId::StashDiffPreview
        ),
        "stash_new" | "stash_apply" | "stash_pop" | "stash_drop" | "stash_branch"
        | "stash_clear" => state.active_tab == WorkspaceTab::Stash,
        "cursor_down" | "cursor_up" => state.focused_pane != PaneId::CommitMessageEditor,
        "goto_top" | "goto_bottom" => matches!(
            state.focused_pane,
            PaneId::LogBranchesTree
                | PaneId::LogCommitTable
                | PaneId::LogChangesTree
                | PaneId::LogDetailsDiff
                | PaneId::CommitChangesTree
                | PaneId::CommitDiffPreview
                | PaneId::StashList
                | PaneId::StashDiffPreview
        ),
        _ => true,
    }
}

pub fn execute_action(run: &str, state: &mut AppState) -> Option<Vec<Action>> {
    match run {
        "noop" => Some(Vec::new()),

        // ── Global ──
        "quit" => Some(vec![Action::Quit]),
        "help" => Some(vec![Action::OpenPopup {
            kind: PopupKind::ShortcutsHelp,
            initial_query: String::new(),
        }]),
        "context_menu" => crate::input::keyboard_context_menu(state),
        "refresh" => Some(vec![Action::RefreshAll]),
        "tab_commit" => Some(vec![Action::SwitchTab(WorkspaceTab::Commit)]),
        "tab_log" => Some(vec![Action::SwitchTab(WorkspaceTab::Log)]),
        "tab_branches" => Some(vec![
            Action::SwitchTab(WorkspaceTab::Log),
            Action::FocusPane(PaneId::LogBranchesTree),
        ]),
        "tab_stash" => Some(vec![Action::SwitchTab(WorkspaceTab::Stash)]),
        "tab_console" => Some(vec![Action::SwitchTab(WorkspaceTab::Console)]),
        "focus_next" => Some(vec![Action::CycleFocus { reverse: false }]),
        "focus_prev" => Some(vec![Action::CycleFocus { reverse: true }]),
        "toggle_branches_popup" => Some(vec![Action::OpenBranchesPopup {
            query: String::new(),
        }]),
        "toggle_branches_pane" => Some(vec![Action::ToggleBranchesPane]),
        "git_fetch" => Some(vec![Action::RunGitOp(GitOp::Fetch)]),
        "git_pull" => {
            if let Err(msg) = state.require_no_ongoing() {
                return Some(vec![Action::Notice(msg)]);
            }
            if state
                .repo_snapshot
                .as_ref()
                .and_then(|s| s.head_branch.as_ref())
                .is_none()
            {
                return Some(vec![Action::Notice(
                    state.i18n.t("menu.log.detached_no_branch").to_string(),
                )]);
            }
            Some(vec![Action::RunGitOp(GitOp::Pull { rebase: false })])
        }
        "git_push" => {
            let Some(branch) = state
                .repo_snapshot
                .as_ref()
                .and_then(|s| s.head_branch.clone())
            else {
                return Some(vec![Action::Notice(
                    state.i18n.t("menu.log.detached_no_branch").to_string(),
                )]);
            };
            Some(vec![Action::PromptPush {
                branch,
                up_to: None,
            }])
        }
        "palette" => Some(vec![Action::OpenPopup {
            kind: PopupKind::PalettePreview,
            initial_query: String::new(),
        }]),
        "toggle_glyph_mode" => Some(vec![Action::ToggleGlyphMode]),
        "cycle_graph_renderer" => Some(vec![Action::CycleGraphRenderer]),
        "git_status_probe" => Some(vec![Action::RunCliProbe(vec![
            "status".into(),
            "--short".into(),
            "--branch".into(),
        ])]),
        "goto_revision" => Some(vec![Action::OpenPopup {
            kind: PopupKind::GoToRevision,
            initial_query: String::new(),
        }]),

        // ── Navigation ──
        "cursor_down" => match state.focused_pane {
            PaneId::LogBranchesTree => Some(vec![Action::ScrollBranches(1)]),
            PaneId::LogCommitTable => Some(vec![Action::ScrollLog(1)]),
            PaneId::LogChangesTree => Some(vec![Action::ChangesMove(1)]),
            PaneId::LogDetailsDiff if state.details.diff_visible() => {
                Some(vec![Action::ScrollDiff(1)])
            }
            PaneId::LogDetailsDiff => Some(vec![Action::ScrollDetails(1)]),
            PaneId::CommitChangesTree => Some(vec![Action::ScrollCommitStage(1)]),
            PaneId::CommitDiffPreview => Some(vec![Action::StageDiffMoveCursor(1)]),
            PaneId::StashList => Some(vec![Action::StashMove(1)]),
            PaneId::StashDiffPreview => Some(vec![Action::StashChangesMove(1)]),
            PaneId::ConsoleCommandList => Some(vec![Action::ScrollConsoleList(1)]),
            PaneId::ConsoleOutputDetail => Some(vec![Action::ScrollConsoleOutput(1)]),
            PaneId::CommitMessageEditor => None,
        },
        "cursor_up" => match state.focused_pane {
            PaneId::LogBranchesTree => Some(vec![Action::ScrollBranches(-1)]),
            PaneId::LogCommitTable => Some(vec![Action::ScrollLog(-1)]),
            PaneId::LogChangesTree => Some(vec![Action::ChangesMove(-1)]),
            PaneId::LogDetailsDiff if state.details.diff_visible() => {
                Some(vec![Action::ScrollDiff(-1)])
            }
            PaneId::LogDetailsDiff => Some(vec![Action::ScrollDetails(-1)]),
            PaneId::CommitChangesTree => Some(vec![Action::ScrollCommitStage(-1)]),
            PaneId::CommitDiffPreview => Some(vec![Action::StageDiffMoveCursor(-1)]),
            PaneId::StashList => Some(vec![Action::StashMove(-1)]),
            PaneId::StashDiffPreview => Some(vec![Action::StashChangesMove(-1)]),
            PaneId::ConsoleCommandList => Some(vec![Action::ScrollConsoleList(-1)]),
            PaneId::ConsoleOutputDetail => Some(vec![Action::ScrollConsoleOutput(-1)]),
            PaneId::CommitMessageEditor => None,
        },
        "goto_top" => match state.focused_pane {
            PaneId::LogBranchesTree => Some(vec![Action::SelectBranchRow(0)]),
            PaneId::LogChangesTree => Some(vec![Action::ChangesSelectRow(0)]),
            PaneId::LogDetailsDiff if state.details.diff_visible() => {
                Some(vec![Action::ScrollDiff(isize::MIN / 2)])
            }
            PaneId::LogDetailsDiff => Some(vec![Action::ScrollDetails(isize::MIN / 2)]),
            PaneId::CommitChangesTree => Some(vec![Action::SelectCommitStageRow(0)]),
            PaneId::CommitDiffPreview => Some(vec![Action::StageDiffMoveCursor(isize::MIN / 2)]),
            PaneId::StashList => Some(vec![Action::StashSelect(0)]),
            PaneId::StashDiffPreview => Some(vec![Action::StashChangesSelect(0)]),
            _ => Some(vec![Action::LogEdge { bottom: false }]),
        },
        "goto_bottom" => match state.focused_pane {
            PaneId::LogBranchesTree => Some(vec![Action::SelectBranchRow(usize::MAX)]),
            PaneId::LogChangesTree => Some(vec![Action::ChangesSelectRow(usize::MAX)]),
            PaneId::LogDetailsDiff if state.details.diff_visible() => {
                Some(vec![Action::ScrollDiff(isize::MAX / 2)])
            }
            PaneId::LogDetailsDiff => Some(vec![Action::ScrollDetails(isize::MAX / 2)]),
            PaneId::CommitChangesTree => Some(vec![Action::SelectCommitStageRow(usize::MAX)]),
            PaneId::CommitDiffPreview => Some(vec![Action::StageDiffMoveCursor(isize::MAX / 2)]),
            PaneId::StashList => Some(vec![Action::StashSelect(usize::MAX)]),
            PaneId::StashDiffPreview => Some(vec![Action::StashChangesSelect(usize::MAX)]),
            _ => Some(vec![Action::LogEdge { bottom: true }]),
        },
        "page_down_half" => Some(vec![Action::LogPage {
            half: true,
            down: true,
        }]),
        "page_up_half" => Some(vec![Action::LogPage {
            half: true,
            down: false,
        }]),

        // ── Log ──
        "compare_or_diff" | "compare_versions" => {
            let oids = state.selected_commits_oldest_first();
            if let [base, target] = oids.as_slice() {
                Some(vec![Action::CompareVersions {
                    base: *base,
                    target: *target,
                }])
            } else {
                Some(vec![Action::ToggleDiffPreview])
            }
        }
        "copy_hash" => {
            let oids = state.selected_commits_oldest_first();
            if oids.is_empty() {
                Some(Vec::new())
            } else {
                let text = oids
                    .iter()
                    .rev()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(" ");
                Some(vec![Action::CopyText(text)])
            }
        }
        "checkout" => {
            if state.focused_pane == PaneId::LogBranchesTree {
                branch_menu_action_by_mnemonic(state, 'c')
            } else {
                let items = crate::menus::log_commit_menu_items(state);
                find_action_by_mnemonic(items, 'c')
            }
        }
        "new_branch" => {
            if state.active_tab == WorkspaceTab::Log
                && state.focused_pane == PaneId::LogBranchesTree
                && let Some(node) = state.branches.selected_node()
                && let BranchNodeKind::Leaf(kind) = node.kind
                && kind != jbgit_core::RefKind::Tag
                && let Some(name) = node.ref_name.as_deref()
            {
                let existing: Vec<String> = state
                    .repo_snapshot
                    .as_ref()
                    .map(|s| s.local_branch_names().iter().map(|n| n.to_string()).collect())
                    .unwrap_or_default();
                let default = if kind == jbgit_core::RefKind::RemoteBranch {
                    name.split_once('/').map_or("", |(_, s)| s)
                } else {
                    ""
                };
                return Some(vec![Action::OpenDialog(Box::new(
                    crate::dialogs::new_branch_dialog(
                        name,
                        name,
                        existing,
                        default,
                        &state.i18n,
                    ),
                ))]);
            }
            if state.active_tab == WorkspaceTab::Log
                && state.focused_pane == PaneId::LogCommitTable
                && let Some(commit) = state.log_commit_at(state.log_graph.selected_idx)
            {
                let oid_str = commit.oid.to_string();
                let short = commit.oid.short();
                let existing: Vec<String> = state
                    .repo_snapshot
                    .as_ref()
                    .map(|s| s.local_branch_names().iter().map(|n| n.to_string()).collect())
                    .unwrap_or_default();
                return Some(vec![Action::OpenDialog(Box::new(
                    crate::dialogs::new_branch_dialog(
                        &oid_str,
                        &short,
                        existing,
                        "",
                        &state.i18n,
                    ),
                ))]);
            }
            Some(vec![Action::OpenDialog(Box::new(
                crate::dialogs::new_branch_from_head_dialog(
                    state.repo_snapshot.as_deref(),
                    &state.i18n,
                ),
            ))])
        }
        "choose_parent" => {
            if matches!(
                state.focused_pane,
                PaneId::LogChangesTree | PaneId::LogDetailsDiff
            ) {
                Some(vec![Action::SwitchMergeParent])
            } else {
                let parents = state
                    .log_commit_at(state.log_graph.selected_idx)
                    .map_or(0, |c| c.parents.len());
                if parents >= 2 {
                    Some(vec![Action::OpenPopup {
                        kind: PopupKind::ChooseParent,
                        initial_query: String::new(),
                    }])
                } else {
                    Some(vec![Action::GoToParent(0)])
                }
            }
        }
        "focus_filter" => Some(vec![Action::FocusFilterText]),
        "uncommit" => {
            let items = crate::menus::log_commit_menu_items(state);
            find_action_by_mnemonic(items, 'u')
        }
        "reword" => {
            let items = crate::menus::log_commit_menu_items(state);
            find_action_by_mnemonic(items, 'w')
        }
        "drop_commit" => {
            let items = crate::menus::log_commit_menu_items(state);
            find_action_by_mnemonic(items, 'x')
        }

        // ── Branch ──
        "branch_search" => Some(vec![Action::BranchesSearchStart]),
        "toggle_favorite" => Some(vec![Action::BranchesToggleFavorite]),
        "rename_branch" => branch_menu_action_by_mnemonic(state, 'e'),
        "delete_branch" => branch_menu_action_by_mnemonic(state, 'd'),
        "navigate_branch" => Some(vec![Action::BranchesNavigate]),

        // ── Commit workbench ──
        "focus_commit_msg" => Some(vec![Action::FocusPane(PaneId::CommitMessageEditor)]),
        "toggle_stage" => {
            if state.focused_pane == PaneId::CommitDiffPreview {
                let side = state
                    .stage_workbench
                    .current_diff_key
                    .as_ref()
                    .map(|k| k.side);
                let is_amend_ref = side == Some(jbgit_engine::StageDiffSide::AmendCommit);
                if !is_amend_ref {
                    if side == Some(jbgit_engine::StageDiffSide::Staged) {
                        Some(vec![Action::StageDiffUnstageSelection])
                    } else {
                        Some(vec![Action::StageDiffStageSelection])
                    }
                } else {
                    Some(Vec::new())
                }
            } else {
                let row = state
                    .stage_workbench
                    .rows
                    .get(state.commit_stage_selected_idx);
                match row {
                    Some(r) if r.group_kind().can_stage() => {
                        Some(vec![Action::StageActionSelected])
                    }
                    Some(r) if r.group_kind().can_unstage() => {
                        Some(vec![Action::UnstageActionSelected])
                    }
                    Some(r) if r.is_container() => {
                        Some(vec![Action::StageToggleRow(state.commit_stage_selected_idx)])
                    }
                    _ => Some(Vec::new()),
                }
            }
        }
        "stage_selection" => {
            if state.focused_pane == PaneId::CommitDiffPreview {
                let is_amend_ref = state
                    .stage_workbench
                    .current_diff_key
                    .as_ref()
                    .is_some_and(|k| k.side == jbgit_engine::StageDiffSide::AmendCommit);
                if is_amend_ref {
                    Some(Vec::new())
                } else {
                    Some(vec![Action::StageDiffStageSelection])
                }
            } else {
                Some(vec![Action::StageActionSelected])
            }
        }
        "unstage_selection" => {
            if state.focused_pane == PaneId::CommitDiffPreview {
                let is_amend_ref = state
                    .stage_workbench
                    .current_diff_key
                    .as_ref()
                    .is_some_and(|k| k.side == jbgit_engine::StageDiffSide::AmendCommit);
                if is_amend_ref {
                    Some(Vec::new())
                } else {
                    Some(vec![Action::StageDiffUnstageSelection])
                }
            } else {
                Some(vec![Action::UnstageActionSelected])
            }
        }
        "commit" => Some(vec![Action::SubmitCommit { and_push: false }]),
        "commit_and_push" => Some(vec![Action::SubmitCommit { and_push: true }]),
        "toggle_amend" => Some(vec![Action::CommitToggleAmend]),
        "toggle_signoff" => Some(vec![Action::CommitToggleSignoff]),
        "commit_history" => Some(vec![Action::OpenPopup {
            kind: PopupKind::CommitHistory,
            initial_query: String::new(),
        }]),
        "select_all" => Some(vec![Action::StageSelectAll]),
        "discard_changes" => Some(vec![Action::DiscardPromptSelected]),
        "toggle_group" => match state.active_tab {
            WorkspaceTab::Commit => Some(vec![Action::StageToggleGroupByDir]),
            WorkspaceTab::Stash => Some(vec![Action::StashChangesToggleGroupByDir]),
            _ => Some(vec![Action::ChangesToggleGroupByDir]),
        },
        "edit_file" => Some(vec![Action::OpenExternalEditorFile(None)]),

        // ── Commit Diff ──
        "toggle_visual" => {
            let is_amend_ref = state
                .stage_workbench
                .current_diff_key
                .as_ref()
                .is_some_and(|k| k.side == jbgit_engine::StageDiffSide::AmendCommit);
            if is_amend_ref {
                Some(Vec::new())
            } else {
                Some(vec![Action::StageDiffToggleVisual])
            }
        }
        "rollback_selection" => {
            let is_amend_ref = state
                .stage_workbench
                .current_diff_key
                .as_ref()
                .is_some_and(|k| k.side == jbgit_engine::StageDiffSide::AmendCommit);
            if is_amend_ref {
                Some(Vec::new())
            } else {
                Some(vec![Action::StageDiffRollbackSelection])
            }
        }
        "undo_rollback" => {
            let is_amend_ref = state
                .stage_workbench
                .current_diff_key
                .as_ref()
                .is_some_and(|k| k.side == jbgit_engine::StageDiffSide::AmendCommit);
            if is_amend_ref {
                Some(Vec::new())
            } else {
                Some(vec![Action::StageDiffUndoRollback])
            }
        }
        "switch_diff_side" => {
            let is_amend_ref = state
                .stage_workbench
                .current_diff_key
                .as_ref()
                .is_some_and(|k| k.side == jbgit_engine::StageDiffSide::AmendCommit);
            if is_amend_ref {
                Some(Vec::new())
            } else {
                Some(vec![Action::StageDiffSwitchSide])
            }
        }
        "three_way_inspector" => {
            let is_amend_ref = state
                .stage_workbench
                .current_diff_key
                .as_ref()
                .is_some_and(|k| k.side == jbgit_engine::StageDiffSide::AmendCommit);
            if is_amend_ref {
                Some(Vec::new())
            } else {
                Some(vec![Action::OpenThreeWayInspector {
                    row_idx: None,
                    range_idx: None,
                }])
            }
        }

        // ── Diff (shared across Log / Commit / Stash) ──
        "prev_hunk" => match state.active_tab {
            WorkspaceTab::Commit => {
                state.stage_workbench.bracket_jump_at = Some(Instant::now());
                Some(vec![Action::StageDiffJumpHunk(-1)])
            }
            WorkspaceTab::Stash => Some(vec![Action::StashDiffJumpHunk(-1)]),
            _ => Some(vec![Action::DiffJumpHunk(-1)]),
        },
        "next_hunk" => match state.active_tab {
            WorkspaceTab::Commit => {
                state.stage_workbench.bracket_jump_at = Some(Instant::now());
                Some(vec![Action::StageDiffJumpHunk(1)])
            }
            WorkspaceTab::Stash => Some(vec![Action::StashDiffJumpHunk(1)]),
            _ => Some(vec![Action::DiffJumpHunk(1)]),
        },
        "prev_file" => match state.active_tab {
            WorkspaceTab::Commit => Some(vec![Action::StageDiffNextFile(-1)]),
            WorkspaceTab::Stash => Some(vec![Action::StashDiffNextFile(-1)]),
            _ => Some(vec![Action::DiffNextFile(-1)]),
        },
        "next_file" => match state.active_tab {
            WorkspaceTab::Commit => Some(vec![Action::StageDiffNextFile(1)]),
            WorkspaceTab::Stash => Some(vec![Action::StashDiffNextFile(1)]),
            _ => Some(vec![Action::DiffNextFile(1)]),
        },
        "toggle_side_by_side" => match state.active_tab {
            WorkspaceTab::Commit => Some(vec![Action::StageDiffToggleSideBySide]),
            WorkspaceTab::Stash => Some(vec![Action::StashDiffToggleSideBySide]),
            _ => Some(vec![Action::DiffToggleSideBySide]),
        },
        "cycle_diff_context" => match state.active_tab {
            WorkspaceTab::Commit => Some(vec![Action::StageDiffCycleContext]),
            WorkspaceTab::Stash => Some(vec![Action::StashDiffCycleContext]),
            _ => Some(vec![Action::DiffCycleContext]),
        },
        "cycle_diff_dock" => Some(vec![Action::DiffCycleDock]),
        "zoom_diff" => Some(vec![Action::ZoomDiff(!state.details.diff_zoomed)]),

        // ── Stash ──
        "stash_new" => Some(crate::menus::stash_button_actions(state, StashButton::New)),
        "stash_apply" => Some(crate::menus::stash_button_actions(state, StashButton::Apply)),
        "stash_pop" => Some(crate::menus::stash_button_actions(state, StashButton::Pop)),
        "stash_drop" => Some(crate::menus::stash_button_actions(state, StashButton::Drop)),
        "stash_branch" => Some(crate::menus::stash_button_actions(state, StashButton::Branch)),
        "stash_clear" => Some(crate::menus::stash_button_actions(state, StashButton::Clear)),

        _ => None,
    }
}

pub fn check_scopes<'a>(events: &[KeyEvent], state: &'a AppState) -> MatchResult<'a> {
    let keymap = &state.config.keymap;
    let active_tab = state.active_tab;
    let focused_pane = state.focused_pane;
    let diff_focused = is_diff_focused(state);

    if diff_focused {
        match match_bindings_filtered(&keymap.diff.key_bindings, events, |b| {
            is_action_applicable(&b.run, state)
        }) {
            MatchResult::None => {}
            other => return other,
        }
    }

    if state.details.diff_zoomed {
        return match_bindings_filtered(&keymap.global.key_bindings, events, |b| {
            matches!(b.run.as_str(), "quit" | "help")
        });
    }

    match active_tab {
        WorkspaceTab::Log => {
            if focused_pane == PaneId::LogBranchesTree {
                match match_bindings_filtered(&keymap.branch.key_bindings, events, |b| {
                    is_action_applicable(&b.run, state)
                }) {
                    MatchResult::None => {}
                    other => return other,
                }
            }
            match match_bindings_filtered(&keymap.log.key_bindings, events, |b| {
                is_action_applicable(&b.run, state)
            }) {
                MatchResult::None => {}
                other => return other,
            }
        }
        WorkspaceTab::Commit => {
            match match_bindings_filtered(&keymap.commit.key_bindings, events, |b| {
                is_action_applicable(&b.run, state)
            }) {
                MatchResult::None => {}
                other => return other,
            }
        }
        WorkspaceTab::Stash => {
            match match_bindings_filtered(&keymap.stash.key_bindings, events, |b| {
                is_action_applicable(&b.run, state)
            }) {
                MatchResult::None => {}
                other => return other,
            }
        }
        WorkspaceTab::Console => {}
    }

    match_bindings_filtered(&keymap.global.key_bindings, events, |b| {
        is_action_applicable(&b.run, state)
    })
}

/// Routes an incoming key through the user-configured keymap rules.
/// Returns `Some(actions)` if matched or consumed (for multi-key prefix).
/// Returns `None` if no keymap rule matched, allowing fallback to default handlers.
pub fn route_dynamic_key(key: &KeyEvent, state: &mut AppState) -> Option<Vec<Action>> {
    if let Some(t) = state.pending_key_at
        && t.elapsed() > KEY_CHORD_WINDOW
    {
        state.pending_keys.clear();
        state.pending_key_at = None;
    }

    if !state.pending_keys.is_empty() && key.code == KeyCode::Esc {
        state.pending_keys.clear();
        state.pending_key_at = None;
        state.needs_redraw = true;
        return Some(Vec::new());
    }

    state.pending_keys.push(*key);
    let pending = state.pending_keys.clone();

    match check_scopes(&pending, state) {
        MatchResult::Exact(binding) => {
            let run = binding.run.clone();
            state.pending_keys.clear();
            state.pending_key_at = None;
            execute_action(&run, state)
        }
        MatchResult::Prefix => {
            state.pending_key_at = Some(Instant::now());
            state.needs_redraw = true;
            Some(Vec::new())
        }
        MatchResult::None => {
            if state.pending_keys.len() > 1 {
                state.pending_keys = vec![*key];
                let single = state.pending_keys.clone();
                match check_scopes(&single, state) {
                    MatchResult::Exact(binding) => {
                        let run = binding.run.clone();
                        state.pending_keys.clear();
                        state.pending_key_at = None;
                        execute_action(&run, state)
                    }
                    MatchResult::Prefix => {
                        state.pending_key_at = Some(Instant::now());
                        state.needs_redraw = true;
                        Some(Vec::new())
                    }
                    MatchResult::None => {
                        state.pending_keys.clear();
                        state.pending_key_at = None;
                        state.needs_redraw = true;
                        None
                    }
                }
            } else {
                state.pending_keys.clear();
                state.pending_key_at = None;
                None
            }
        }
    }
}

pub fn localized_action_desc(
    state: &AppState,
    run: &str,
    fallback_desc: Option<&str>,
) -> String {
    let i18n_key = format!("help.action.{run}");
    let loc = state.i18n.t(&i18n_key);
    if loc != i18n_key {
        loc.to_string()
    } else if let Some(d) = fallback_desc {
        d.to_string()
    } else {
        run.to_string()
    }
}

/// Returns `(next_key_display, description)` pairs for the currently pending prefix sequence.
pub fn pending_completions(state: &AppState) -> Vec<(String, String)> {
    if state.pending_keys.is_empty() {
        return Vec::new();
    }
    if state
        .pending_key_at
        .is_some_and(|t| t.elapsed() > KEY_CHORD_WINDOW)
    {
        return Vec::new();
    }

    let km = &state.config.keymap;
    let mut lists: Vec<&[KeyBindingConfig]> = Vec::new();
    if is_diff_focused(state) {
        lists.push(&km.diff.key_bindings);
    }
    if !state.details.diff_zoomed {
        match state.active_tab {
            WorkspaceTab::Log => {
                if state.focused_pane == PaneId::LogBranchesTree {
                    lists.push(&km.branch.key_bindings);
                }
                lists.push(&km.log.key_bindings);
            }
            WorkspaceTab::Commit => lists.push(&km.commit.key_bindings),
            WorkspaceTab::Stash => lists.push(&km.stash.key_bindings),
            WorkspaceTab::Console => {}
        }
        lists.push(&km.global.key_bindings);
    }

    let prefix_len = state.pending_keys.len();
    let mut out: Vec<(String, String)> = Vec::new();
    for bindings in lists {
        for b in bindings {
            if !is_action_applicable(&b.run, state) {
                continue;
            }
            if let Some(seq) = KeySequence::parse(&b.on)
                && seq.matches_prefix(&state.pending_keys)
                && let Some(next_stroke) = seq.strokes.get(prefix_len)
            {
                let next_disp = next_stroke.to_display_string();
                if !out.iter().any(|(k, _)| k == &next_disp) {
                    let desc = localized_action_desc(state, &b.run, b.desc.as_deref());
                    out.push((next_disp, desc));
                }
            }
        }
    }
    out
}

pub fn pending_prefix_display(state: &AppState) -> String {
    state
        .pending_keys
        .iter()
        .map(|ev| {
            let shift = ev.modifiers.contains(KeyModifiers::SHIFT);
            let stroke = KeyStroke {
                code: ev.code,
                ctrl: ev.modifiers.contains(KeyModifiers::CONTROL),
                alt: ev.modifiers.contains(KeyModifiers::ALT),
                shift,
            };
            stroke.to_display_string()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HelpRow {
    Section(String),
    Entry {
        keys: String,
        desc: String,
        scope_tag: String,
        run: Option<String>,
    },
}

impl HelpRow {
    pub fn is_selectable(&self) -> bool {
        matches!(self, Self::Entry { .. })
    }
}

fn collect_merged_entries(
    state: &AppState,
    bindings: &[KeyBindingConfig],
    scope_tag: &str,
    filter_applicable: bool,
) -> Vec<HelpRow> {
    let mut order: Vec<String> = Vec::new();
    let mut keys_by_run: Vec<(String, Vec<String>, Option<String>)> = Vec::new();

    for b in bindings {
        if b.run.is_empty() || b.run == "noop" {
            continue;
        }
        if filter_applicable && !is_action_applicable(&b.run, state) {
            continue;
        }
        let Some(seq) = KeySequence::parse(&b.on) else {
            continue;
        };
        let key_disp = seq.to_display_string();
        if let Some(pos) = order.iter().position(|r| r == &b.run) {
            let entry = &mut keys_by_run[pos];
            if !entry.1.contains(&key_disp) {
                entry.1.push(key_disp);
            }
            if entry.2.is_none() {
                entry.2 = b.desc.clone();
            }
        } else {
            order.push(b.run.clone());
            keys_by_run.push((b.run.clone(), vec![key_disp], b.desc.clone()));
        }
    }

    keys_by_run
        .into_iter()
        .map(|(run, keys, fallback_desc)| HelpRow::Entry {
            keys: keys.join(" / "),
            desc: localized_action_desc(state, &run, fallback_desc.as_deref()),
            scope_tag: scope_tag.to_string(),
            run: Some(run),
        })
        .collect()
}

fn current_context_rows(state: &AppState) -> Vec<HelpRow> {
    let km = &state.config.keymap;
    let i18n = &state.i18n;
    let pane_title = i18n.pane_title(state.focused_pane);
    let mut out = Vec::new();

    if is_diff_focused(state) {
        out.extend(collect_merged_entries(
            state,
            &km.diff.key_bindings,
            i18n.t("help.scope.diff"),
            true,
        ));
    }

    match state.active_tab {
        WorkspaceTab::Log => {
            if state.focused_pane == PaneId::LogBranchesTree {
                out.extend(collect_merged_entries(
                    state,
                    &km.branch.key_bindings,
                    i18n.t("help.scope.branch"),
                    true,
                ));
            }
            out.extend(collect_merged_entries(
                state,
                &km.log.key_bindings,
                i18n.t("help.scope.log"),
                true,
            ));
        }
        WorkspaceTab::Commit => {
            if state.focused_pane == PaneId::CommitMessageEditor {
                out.push(HelpRow::Entry {
                    keys: "Ctrl+S / Ctrl+Enter".to_string(),
                    desc: localized_action_desc(state, "commit", None),
                    scope_tag: pane_title.to_string(),
                    run: Some("commit".to_string()),
                });
                out.push(HelpRow::Entry {
                    keys: "Alt+A / Alt+M".to_string(),
                    desc: localized_action_desc(state, "toggle_amend", None),
                    scope_tag: pane_title.to_string(),
                    run: Some("toggle_amend".to_string()),
                });
                out.push(HelpRow::Entry {
                    keys: "Ctrl+M".to_string(),
                    desc: localized_action_desc(state, "commit_history", None),
                    scope_tag: pane_title.to_string(),
                    run: Some("commit_history".to_string()),
                });
                out.push(HelpRow::Entry {
                    keys: "Esc".to_string(),
                    desc: i18n.t("help.action.back_to_changes").to_string(),
                    scope_tag: pane_title.to_string(),
                    run: None,
                });
                return out;
            }
            out.extend(collect_merged_entries(
                state,
                &km.commit.key_bindings,
                i18n.t("help.scope.commit"),
                true,
            ));
        }
        WorkspaceTab::Stash => {
            out.extend(collect_merged_entries(
                state,
                &km.stash.key_bindings,
                i18n.t("help.scope.stash"),
                true,
            ));
        }
        WorkspaceTab::Console => {
            out.push(HelpRow::Entry {
                keys: "p".to_string(),
                desc: localized_action_desc(state, "git_status_probe", None),
                scope_tag: pane_title.to_string(),
                run: Some("git_status_probe".to_string()),
            });
        }
    }

    // Deduplicate by `run` preserving first occurrence (diff wins over tab scope).
    let mut seen = Vec::new();
    out.retain(|row| match row {
        HelpRow::Entry { run: Some(r), .. } => {
            if seen.contains(r) {
                false
            } else {
                seen.push(r.clone());
                true
            }
        }
        _ => true,
    });
    out
}

fn advanced_tool_rows(state: &AppState) -> Vec<HelpRow> {
    let i18n = &state.i18n;
    let scope = i18n.t("help.scope.advanced").to_string();
    let km = &state.config.keymap;
    let key_or = |run: &str, default_key: &str| -> String {
        km.primary_key_for(
            &[
                KeymapScope::Global,
                KeymapScope::Log,
                KeymapScope::Commit,
                KeymapScope::Branch,
                KeymapScope::Diff,
                KeymapScope::Stash,
            ],
            run,
        )
        .unwrap_or_else(|| default_key.to_string())
    };
    vec![
        HelpRow::Entry {
            keys: key_or("palette", "—"),
            desc: localized_action_desc(state, "palette", None),
            scope_tag: scope.clone(),
            run: Some("palette".to_string()),
        },
        HelpRow::Entry {
            keys: key_or("toggle_glyph_mode", "F12"),
            desc: localized_action_desc(state, "toggle_glyph_mode", None),
            scope_tag: scope.clone(),
            run: Some("toggle_glyph_mode".to_string()),
        },
        HelpRow::Entry {
            keys: key_or("cycle_graph_renderer", "F8"),
            desc: localized_action_desc(state, "cycle_graph_renderer", None),
            scope_tag: scope.clone(),
            run: Some("cycle_graph_renderer".to_string()),
        },
        HelpRow::Entry {
            keys: key_or("git_status_probe", "Console: p"),
            desc: localized_action_desc(state, "git_status_probe", None),
            scope_tag: scope,
            run: Some("git_status_probe".to_string()),
        },
    ]
}

pub fn build_help_rows(state: &AppState, query: &str) -> Vec<HelpRow> {
    let i18n = &state.i18n;
    let km = &state.config.keymap;
    let q = query.trim().to_lowercase();

    let ctx_rows = current_context_rows(state);
    let global_rows = collect_merged_entries(
        state,
        &km.global.key_bindings,
        i18n.t("help.scope.global"),
        false,
    );
    let commit_rows = collect_merged_entries(
        state,
        &km.commit.key_bindings,
        i18n.t("help.scope.commit"),
        false,
    );
    let log_rows = collect_merged_entries(
        state,
        &km.log.key_bindings,
        i18n.t("help.scope.log"),
        false,
    );
    let branch_rows = collect_merged_entries(
        state,
        &km.branch.key_bindings,
        i18n.t("help.scope.branch"),
        false,
    );
    let diff_rows = collect_merged_entries(
        state,
        &km.diff.key_bindings,
        i18n.t("help.scope.diff"),
        false,
    );
    let stash_rows = collect_merged_entries(
        state,
        &km.stash.key_bindings,
        i18n.t("help.scope.stash"),
        false,
    );
    let adv_rows = advanced_tool_rows(state);
    let other_rows = vec![
        HelpRow::Entry {
            keys: i18n.t("help.mouse_drag_key").to_string(),
            desc: i18n.t("help.mouse_drag").to_string(),
            scope_tag: i18n.t("help.scope.global").to_string(),
            run: None,
        },
        HelpRow::Entry {
            keys: i18n.t("help.right_click_key").to_string(),
            desc: i18n.t("help.right_click").to_string(),
            scope_tag: i18n.t("help.scope.global").to_string(),
            run: None,
        },
    ];

    if q.is_empty() {
        let mut rows = Vec::new();
        let pane_title = i18n.pane_title(state.focused_pane);
        if !ctx_rows.is_empty() {
            rows.push(HelpRow::Section(
                i18n.t_fmt("help.current_context", &[("pane", pane_title)]),
            ));
            rows.extend(ctx_rows);
        }
        for (title, sec_rows) in [
            (i18n.t("help.section_global"), global_rows),
            (i18n.t("help.section_commit"), commit_rows),
            (i18n.t("help.section_log"), log_rows),
            (i18n.t("help.section_branch"), branch_rows),
            (i18n.t("help.section_diff"), diff_rows),
            (i18n.t("help.section_stash"), stash_rows),
            (i18n.t("help.section_advanced"), adv_rows),
            (i18n.t("help.section_other"), other_rows),
        ] {
            if !sec_rows.is_empty() {
                rows.push(HelpRow::Section(title.to_string()));
                rows.extend(sec_rows);
            }
        }
        return rows;
    }

    let mut matched: Vec<(u8, HelpRow)> = Vec::new();
    let mut seen_runs: Vec<(Option<String>, String)> = Vec::new();
    for row in ctx_rows
        .into_iter()
        .chain(global_rows)
        .chain(commit_rows)
        .chain(log_rows)
        .chain(branch_rows)
        .chain(diff_rows)
        .chain(stash_rows)
        .chain(adv_rows)
        .chain(other_rows)
    {
        if let HelpRow::Entry {
            ref keys,
            ref desc,
            ref scope_tag,
            ref run,
        } = row
        {
            let run_lc = run.as_deref().unwrap_or("").to_lowercase();
            let keys_lc = keys.to_lowercase();
            let desc_lc = desc.to_lowercase();
            let scope_lc = scope_tag.to_lowercase();
            let rank = if run_lc == q || keys_lc == q {
                Some(0u8)
            } else if run_lc.starts_with(&q) || desc_lc.starts_with(&q) {
                Some(1)
            } else if run_lc.contains(&q) || desc_lc.contains(&q) || keys_lc.contains(&q) {
                Some(2)
            } else if scope_lc.contains(&q) {
                Some(3)
            } else {
                None
            };
            if let Some(score) = rank {
                let dedup_key = (run.clone(), desc.clone());
                if !seen_runs.contains(&dedup_key) {
                    seen_runs.push(dedup_key);
                    matched.push((score, row));
                }
            }
        }
    }
    matched.sort_by_key(|(score, _)| *score);
    matched.into_iter().map(|(_, row)| row).collect()
}

pub fn first_help_entry(rows: &[HelpRow]) -> usize {
    rows.iter().position(HelpRow::is_selectable).unwrap_or(0)
}

pub fn resolve_help_selection(rows: &[HelpRow], selected: usize) -> usize {
    if rows.get(selected).is_some_and(HelpRow::is_selectable) {
        selected
    } else {
        first_help_entry(rows)
    }
}

pub fn step_help_selection(rows: &[HelpRow], from: usize, delta: isize) -> usize {
    let n = rows.len();
    if n == 0 || !rows.iter().any(HelpRow::is_selectable) {
        return 0;
    }
    let mut idx = resolve_help_selection(rows, from);
    let step = if delta >= 0 { 1isize } else { -1isize };
    let count = delta.unsigned_abs().max(1);
    for _ in 0..count {
        for _ in 0..n {
            idx = (idx as isize + step).rem_euclid(n as isize) as usize;
            if rows[idx].is_selectable() {
                break;
            }
        }
    }
    idx
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_and_match_keystrokes() {
        let k = KeyStroke::parse("<C-d>").unwrap();
        assert_eq!(k.code, KeyCode::Char('d'));
        assert!(k.ctrl);
        assert!(!k.alt);
        assert_eq!(k.to_display_string(), "Ctrl+D");

        let ev_ctrl_d = KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL);
        assert!(k.matches(&ev_ctrl_d));

        let ev_d = KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE);
        assert!(!k.matches(&ev_d));

        let k_space = KeyStroke::parse("<Space>").unwrap();
        let ev_space = KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE);
        assert!(k_space.matches(&ev_space));

        let k_f2 = KeyStroke::parse("<F2>").unwrap();
        let ev_f2 = KeyEvent::new(KeyCode::F(2), KeyModifiers::NONE);
        assert!(k_f2.matches(&ev_f2));

        let k_a1 = KeyStroke::parse("<A-1>").unwrap();
        let ev_a1 = KeyEvent::new(KeyCode::Char('1'), KeyModifiers::ALT);
        assert!(k_a1.matches(&ev_a1));

        // Shifted punctuation on Windows/Kitty must match even when SHIFT modifier is present.
        let k_qmark = KeyStroke::parse("?").unwrap();
        let ev_qmark_shift = KeyEvent::new(KeyCode::Char('?'), KeyModifiers::SHIFT);
        assert!(k_qmark.matches(&ev_qmark_shift));
        let k_lbrace = KeyStroke::parse("{").unwrap();
        let ev_lbrace_shift = KeyEvent::new(KeyCode::Char('{'), KeyModifiers::SHIFT);
        assert!(k_lbrace.matches(&ev_lbrace_shift));

        // Ctrl+Shift+K must stay distinct from Ctrl+K.
        let k_csk = KeyStroke::parse("<C-S-k>").unwrap();
        let k_ck = KeyStroke::parse("<C-k>").unwrap();
        let ev_csk = KeyEvent::new(KeyCode::Char('K'), KeyModifiers::CONTROL | KeyModifiers::SHIFT);
        let ev_ck = KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL);
        assert!(k_csk.matches(&ev_csk));
        assert!(!k_ck.matches(&ev_csk));
        assert!(k_ck.matches(&ev_ck));
        assert!(!k_csk.matches(&ev_ck));
        assert_eq!(k_csk.to_display_string(), "Ctrl+Shift+K");

        // BackTab matches with or without SHIFT modifier.
        let k_bt = KeyStroke::parse("<BackTab>").unwrap();
        assert!(k_bt.matches(&KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT)));
        assert_eq!(k_bt.to_display_string(), "Shift+Tab");
    }

    #[test]
    fn sequence_exact_and_prefix_matching() {
        let seq = KeySequence::parse(&["g".to_string(), "g".to_string()]).unwrap();
        let ev_g = KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE);

        assert!(seq.matches_prefix(&[ev_g]));
        assert!(seq.matches_exact(&[ev_g, ev_g]));

        let ev_j = KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE);
        assert!(!seq.matches_prefix(&[ev_j]));
        assert!(!seq.matches_exact(&[ev_g, ev_j]));
    }

    #[test]
    fn dynamic_keymap_routing_and_remapping() {
        use std::path::PathBuf;
        use crate::config::AppConfig;

        let config = AppConfig::default();
        let mut state = AppState::new_with_config(PathBuf::from("."), None, config);

        // 1. Default routing: "r" refreshes
        let ev_r = KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE);
        let actions = route_dynamic_key(&ev_r, &mut state);
        assert!(matches!(actions.as_deref(), Some([Action::RefreshAll])));

        // 2. Default routing: multi-key sequence "g" then "g" + Which-Key completions
        let ev_g = KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE);
        let first = route_dynamic_key(&ev_g, &mut state);
        assert!(matches!(first.as_deref(), Some([]))); // prefix consumed
        let completions = pending_completions(&state);
        assert!(completions.iter().any(|(k, _)| k == "g"));
        assert!(completions.iter().any(|(k, _)| k == "r"));
        assert!(completions.iter().any(|(k, _)| k == "b"));
        let second = route_dynamic_key(&ev_g, &mut state);
        assert!(matches!(
            second.as_deref(),
            Some([Action::LogEdge { bottom: false }])
        ));

        // 2b. Esc cancels pending chord cleanly
        let first_esc = route_dynamic_key(&ev_g, &mut state);
        assert!(matches!(first_esc.as_deref(), Some([])));
        assert!(!state.pending_keys.is_empty());
        let cancel = route_dynamic_key(&KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &mut state);
        assert!(matches!(cancel.as_deref(), Some([])));
        assert!(state.pending_keys.is_empty());

        // 3. Diff scope routing: 'S' toggles side-by-side
        state.active_tab = WorkspaceTab::Log;
        state.focused_pane = PaneId::LogDetailsDiff;
        state.details.diff_preview = true;
        let ev_s_upper = KeyEvent::new(KeyCode::Char('S'), KeyModifiers::SHIFT);
        let actions_s = route_dynamic_key(&ev_s_upper, &mut state);
        assert!(matches!(
            actions_s.as_deref(),
            Some([Action::DiffToggleSideBySide])
        ));

        // 3b. Global remote keys: b (branches popup), f (fetch), P (push)
        state.focused_pane = PaneId::LogCommitTable;
        let ev_b = KeyEvent::new(KeyCode::Char('b'), KeyModifiers::NONE);
        assert!(matches!(
            route_dynamic_key(&ev_b, &mut state).as_deref(),
            Some([Action::OpenBranchesPopup { .. }])
        ));
        let ev_f = KeyEvent::new(KeyCode::Char('f'), KeyModifiers::NONE);
        assert!(matches!(
            route_dynamic_key(&ev_f, &mut state).as_deref(),
            Some([Action::RunGitOp(GitOp::Fetch)])
        ));

        // 4. Commit Diff: 'r' refreshes (does NOT rollback), while 'd' rolls back!
        state.active_tab = WorkspaceTab::Commit;
        state.focused_pane = PaneId::CommitDiffPreview;
        let actions_r_diff = route_dynamic_key(&ev_r, &mut state);
        assert!(matches!(actions_r_diff.as_deref(), Some([Action::RefreshAll])));
        let ev_d = KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE);
        let actions_d_diff = route_dynamic_key(&ev_d, &mut state);
        assert!(matches!(
            actions_d_diff.as_deref(),
            Some([Action::StageDiffRollbackSelection])
        ));

        // 4b. Ctrl+S submits commit in Commit view
        let ev_cs = KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL);
        assert!(matches!(
            route_dynamic_key(&ev_cs, &mut state).as_deref(),
            Some([Action::SubmitCommit { and_push: false }])
        ));

        // 5. Custom keymap: remap refresh to F5 via overlay (key_bindings mode)
        let mut custom_config = AppConfig::default();
        let ov: crate::config::KeymapConfigOverlay = toml::from_str(
            r#"
[global]
key_bindings = [{ on = ["<F5>"], run = "refresh" }]
"#,
        )
        .unwrap();
        custom_config.keymap.merge_overlay(&ov);
        let mut state_custom = AppState::new_with_config(PathBuf::from("."), None, custom_config);

        let ev_f5 = KeyEvent::new(KeyCode::F(5), KeyModifiers::NONE);
        let actions_f5 = route_dynamic_key(&ev_f5, &mut state_custom);
        assert!(matches!(actions_f5.as_deref(), Some([Action::RefreshAll])));

        // Pressing r does NOT refresh when replaced via key_bindings
        let actions_r = route_dynamic_key(&ev_r, &mut state_custom);
        assert!(actions_r.is_none());

        // Existing global bindings like 'q' are preserved
        let ev_q = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE);
        let actions_q = route_dynamic_key(&ev_q, &mut state_custom);
        assert!(matches!(actions_q.as_deref(), Some([Action::Quit])));
    }

    #[test]
    fn help_rows_merge_aliases_and_filter_search() {
        use std::path::PathBuf;
        use crate::config::AppConfig;

        let config = AppConfig::default();
        let state = AppState::new_with_config(PathBuf::from("."), None, config);
        let rows = build_help_rows(&state, "");
        assert!(matches!(rows.first(), Some(HelpRow::Section(_))));
        assert!(rows.iter().any(|r| matches!(
            r,
            HelpRow::Entry { keys, run: Some(run), .. } if run == "cursor_down" && keys == "j / ↓"
        )));
        // Palette is available under advanced section and executable
        let palette_rows = build_help_rows(&state, "palette");
        assert!(palette_rows.iter().any(|r| matches!(
            r,
            HelpRow::Entry { run: Some(run), .. } if run == "palette"
        )));
    }
}
