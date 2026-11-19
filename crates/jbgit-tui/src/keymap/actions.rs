//! Typed keymap scope resolution, applicability checks, and action dispatch.

use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent};
use jbgit_core::{BranchNodeKind, GitOp, PaneId, WorkspaceTab};
use jbgit_engine::StageDiffSide;

use crate::app::{
    Action, AppState, ContextMenuAction, ContextMenuEntryKind, ContextMenuItem, PopupKind,
    SCROLL_GOTO_BOTTOM, SCROLL_GOTO_TOP,
};
use crate::config::keymap_config::{KeyBindingConfig, KeymapScope};
use crate::hit_test::StashButton;

use super::{
    KEY_CHORD_WINDOW, KeyAction, KeySequence, MatchResult, match_bindings_filtered,
};

pub const DEFAULT_CONSOLE_PROBE_CHAR: char = 'p';

pub fn console_status_probe_key(keymap: &crate::config::KeymapConfig) -> String {
    keymap
        .primary_key_for(&[KeymapScope::Global], &KeyAction::GitStatusProbe)
        .unwrap_or_else(|| DEFAULT_CONSOLE_PROBE_CHAR.to_string())
}

pub fn git_status_probe_action() -> Action {
    Action::RunCliProbe(vec![
        "status".into(),
        "--short".into(),
        "--branch".into(),
    ])
}

pub fn is_branch_speed_search_char(c: char) -> bool {
    (c.is_alphabetic() || matches!(c, '-' | '_' | '/' | '0' | '5'..='9')) && c != 'J'
}

pub fn is_amend_diff(state: &AppState) -> bool {
    state
        .stage_workbench
        .current_diff_key
        .as_ref()
        .is_some_and(|k| k.side == StageDiffSide::AmendCommit)
}

fn route_diff_host(
    state: &AppState,
    commit: Action,
    stash: Action,
    log: Action,
) -> Option<Vec<Action>> {
    Some(vec![match state.active_tab {
        WorkspaceTab::Commit => commit,
        WorkspaceTab::Stash => stash,
        _ => log,
    }])
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

/// Ordered list of keymap scopes active for the current tab/focus (highest priority first).
pub fn scopes_for_context(state: &AppState) -> Vec<KeymapScope> {
    let mut scopes = Vec::with_capacity(4);
    if is_diff_focused(state) {
        scopes.push(KeymapScope::Diff);
    }
    if !state.details.diff_zoomed {
        match state.active_tab {
            WorkspaceTab::Log => {
                if state.focused_pane == PaneId::LogBranchesTree {
                    scopes.push(KeymapScope::Branch);
                }
                scopes.push(KeymapScope::Log);
            }
            WorkspaceTab::Commit => scopes.push(KeymapScope::Commit),
            WorkspaceTab::Stash => scopes.push(KeymapScope::Stash),
            WorkspaceTab::Console => scopes.push(KeymapScope::Console),
        }
        scopes.push(KeymapScope::Global);
    }
    scopes
}

pub fn is_action_applicable(run: &KeyAction, state: &AppState) -> bool {
    if !run.is_active() {
        return false;
    }
    if state.details.diff_zoomed {
        return matches!(
            run,
            KeyAction::Quit
                | KeyAction::Help
                | KeyAction::CursorDown
                | KeyAction::CursorUp
                | KeyAction::GotoTop
                | KeyAction::GotoBottom
                | KeyAction::PrevHunk
                | KeyAction::NextHunk
                | KeyAction::PrevFile
                | KeyAction::NextFile
                | KeyAction::ToggleSideBySide
                | KeyAction::CycleDiffContext
                | KeyAction::ZoomDiff
                | KeyAction::ChooseParent
        );
    }

    match run {
        KeyAction::Noop | KeyAction::Unknown(_) => false,
        KeyAction::Quit
        | KeyAction::Help
        | KeyAction::ContextMenu
        | KeyAction::Refresh
        | KeyAction::TabCommit
        | KeyAction::TabLog
        | KeyAction::TabBranches
        | KeyAction::TabStash
        | KeyAction::TabConsole
        | KeyAction::FocusNext
        | KeyAction::FocusPrev
        | KeyAction::ToggleBranchesPopup
        | KeyAction::GitFetch
        | KeyAction::GitPull
        | KeyAction::GitPush
        | KeyAction::Palette
        | KeyAction::ToggleGlyphMode
        | KeyAction::CycleGraphRenderer => true,
        KeyAction::GitStatusProbe => {
            state.active_tab == WorkspaceTab::Console
                && state.focused_pane == PaneId::ConsoleCommandList
        }

        KeyAction::ToggleVisual
        | KeyAction::RollbackSelection
        | KeyAction::UndoRollback
        | KeyAction::SwitchDiffSide
        | KeyAction::ThreeWayInspector => {
            state.active_tab == WorkspaceTab::Commit
                && state.focused_pane == PaneId::CommitDiffPreview
                && !is_amend_diff(state)
        }
        KeyAction::ToggleStage | KeyAction::StageSelection | KeyAction::UnstageSelection => {
            state.active_tab == WorkspaceTab::Commit
                && (state.focused_pane == PaneId::CommitChangesTree
                    || (state.focused_pane == PaneId::CommitDiffPreview && !is_amend_diff(state)))
        }
        KeyAction::CycleDiffDock => state.active_tab == WorkspaceTab::Log,
        KeyAction::ZoomDiff => state.active_tab == WorkspaceTab::Log,
        KeyAction::PrevHunk
        | KeyAction::NextHunk
        | KeyAction::PrevFile
        | KeyAction::NextFile
        | KeyAction::ToggleSideBySide
        | KeyAction::CycleDiffContext => is_diff_focused(state),

        KeyAction::Checkout => {
            state.active_tab == WorkspaceTab::Log
                && matches!(
                    state.focused_pane,
                    PaneId::LogBranchesTree | PaneId::LogCommitTable
                )
        }
        KeyAction::CopyHash | KeyAction::Uncommit | KeyAction::Reword | KeyAction::DropCommit => {
            state.active_tab == WorkspaceTab::Log && state.focused_pane == PaneId::LogCommitTable
        }
        KeyAction::ChooseParent => {
            state.active_tab == WorkspaceTab::Log
                && matches!(
                    state.focused_pane,
                    PaneId::LogCommitTable | PaneId::LogChangesTree | PaneId::LogDetailsDiff
                )
        }
        KeyAction::NewBranch => true,
        KeyAction::CompareOrDiff
        | KeyAction::CompareVersions
        | KeyAction::GotoRevision
        | KeyAction::FocusFilter
        | KeyAction::ToggleBranchesPane => state.active_tab == WorkspaceTab::Log,
        KeyAction::PageDownHalf | KeyAction::PageUpHalf => {
            state.active_tab == WorkspaceTab::Log
                || state.focused_pane == PaneId::ConsoleOutputDetail
        }

        KeyAction::BranchSearch
        | KeyAction::ToggleFavorite
        | KeyAction::RenameBranch
        | KeyAction::DeleteBranch
        | KeyAction::NavigateBranch => {
            state.active_tab == WorkspaceTab::Log && state.focused_pane == PaneId::LogBranchesTree
        }

        KeyAction::FocusCommitMsg | KeyAction::SelectAll | KeyAction::DiscardChanges => {
            state.active_tab == WorkspaceTab::Commit
                && state.focused_pane == PaneId::CommitChangesTree
        }
        KeyAction::Commit
        | KeyAction::CommitAndPush
        | KeyAction::ToggleAmend
        | KeyAction::ToggleSignoff
        | KeyAction::CommitHistory => state.active_tab == WorkspaceTab::Commit,
        KeyAction::EditFile => {
            state.active_tab == WorkspaceTab::Commit
                && matches!(
                    state.focused_pane,
                    PaneId::CommitChangesTree | PaneId::CommitDiffPreview
                )
        }
        KeyAction::ToggleGroup => matches!(
            state.focused_pane,
            PaneId::CommitChangesTree | PaneId::LogChangesTree | PaneId::StashDiffPreview
        ),
        KeyAction::StashNew
        | KeyAction::StashApply
        | KeyAction::StashPop
        | KeyAction::StashDrop
        | KeyAction::StashBranch
        | KeyAction::StashClear => state.active_tab == WorkspaceTab::Stash,

        KeyAction::CursorDown | KeyAction::CursorUp => {
            state.focused_pane != PaneId::CommitMessageEditor
        }
        KeyAction::GotoTop | KeyAction::GotoBottom => matches!(
            state.focused_pane,
            PaneId::LogBranchesTree
                | PaneId::LogCommitTable
                | PaneId::LogChangesTree
                | PaneId::LogDetailsDiff
                | PaneId::CommitChangesTree
                | PaneId::CommitDiffPreview
                | PaneId::StashList
                | PaneId::StashDiffPreview
                | PaneId::ConsoleOutputDetail
        ),
    }
}

pub fn execute_action(run: &KeyAction, state: &mut AppState) -> Option<Vec<Action>> {
    match run {
        KeyAction::Noop | KeyAction::Unknown(_) => Some(Vec::new()),

        // ── Global ──
        KeyAction::Quit => Some(vec![Action::Quit]),
        KeyAction::Help => Some(vec![Action::OpenPopup {
            kind: PopupKind::ShortcutsHelp,
            initial_query: String::new(),
        }]),
        KeyAction::ContextMenu => crate::input::keyboard_context_menu(state),
        KeyAction::Refresh => Some(vec![Action::RefreshAll]),
        KeyAction::TabCommit => Some(vec![Action::SwitchTab(WorkspaceTab::Commit)]),
        KeyAction::TabLog => Some(vec![Action::SwitchTab(WorkspaceTab::Log)]),
        KeyAction::TabBranches => Some(vec![
            Action::SwitchTab(WorkspaceTab::Log),
            Action::FocusPane(PaneId::LogBranchesTree),
        ]),
        KeyAction::TabStash => Some(vec![Action::SwitchTab(WorkspaceTab::Stash)]),
        KeyAction::TabConsole => Some(vec![Action::SwitchTab(WorkspaceTab::Console)]),
        KeyAction::FocusNext => Some(vec![Action::CycleFocus { reverse: false }]),
        KeyAction::FocusPrev => Some(vec![Action::CycleFocus { reverse: true }]),
        KeyAction::ToggleBranchesPopup => Some(vec![Action::OpenBranchesPopup {
            query: String::new(),
        }]),
        KeyAction::ToggleBranchesPane => Some(vec![Action::ToggleBranchesPane]),
        KeyAction::GitFetch => Some(vec![Action::RunGitOp(GitOp::Fetch)]),
        KeyAction::GitPull => {
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
        KeyAction::GitPush => {
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
        KeyAction::Palette => Some(vec![Action::OpenPopup {
            kind: PopupKind::PalettePreview,
            initial_query: String::new(),
        }]),
        KeyAction::ToggleGlyphMode => Some(vec![Action::ToggleGlyphMode]),
        KeyAction::CycleGraphRenderer => Some(vec![Action::CycleGraphRenderer]),
        KeyAction::GitStatusProbe => Some(vec![git_status_probe_action()]),
        KeyAction::GotoRevision => Some(vec![Action::OpenPopup {
            kind: PopupKind::GoToRevision,
            initial_query: String::new(),
        }]),

        // ── Navigation ──
        KeyAction::CursorDown => match state.focused_pane {
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
        KeyAction::CursorUp => match state.focused_pane {
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
        KeyAction::GotoTop => match state.focused_pane {
            PaneId::LogBranchesTree => Some(vec![Action::SelectBranchRow(0)]),
            PaneId::LogChangesTree => Some(vec![Action::ChangesSelectRow(0)]),
            PaneId::LogDetailsDiff if state.details.diff_visible() => {
                Some(vec![Action::ScrollDiff(SCROLL_GOTO_TOP)])
            }
            PaneId::LogDetailsDiff => Some(vec![Action::ScrollDetails(SCROLL_GOTO_TOP)]),
            PaneId::CommitChangesTree => Some(vec![Action::SelectCommitStageRow(0)]),
            PaneId::CommitDiffPreview => Some(vec![Action::StageDiffMoveCursor(SCROLL_GOTO_TOP)]),
            PaneId::StashList => Some(vec![Action::StashSelect(0)]),
            PaneId::StashDiffPreview => Some(vec![Action::StashChangesSelect(0)]),
            PaneId::ConsoleOutputDetail => Some(vec![Action::ScrollConsoleOutput(SCROLL_GOTO_TOP)]),
            _ => Some(vec![Action::LogEdge { bottom: false }]),
        },
        KeyAction::GotoBottom => match state.focused_pane {
            PaneId::LogBranchesTree => Some(vec![Action::SelectBranchRow(usize::MAX)]),
            PaneId::LogChangesTree => Some(vec![Action::ChangesSelectRow(usize::MAX)]),
            PaneId::LogDetailsDiff if state.details.diff_visible() => {
                Some(vec![Action::ScrollDiff(SCROLL_GOTO_BOTTOM)])
            }
            PaneId::LogDetailsDiff => Some(vec![Action::ScrollDetails(SCROLL_GOTO_BOTTOM)]),
            PaneId::CommitChangesTree => Some(vec![Action::SelectCommitStageRow(usize::MAX)]),
            PaneId::CommitDiffPreview => {
                Some(vec![Action::StageDiffMoveCursor(SCROLL_GOTO_BOTTOM)])
            }
            PaneId::StashList => Some(vec![Action::StashSelect(usize::MAX)]),
            PaneId::StashDiffPreview => Some(vec![Action::StashChangesSelect(usize::MAX)]),
            PaneId::ConsoleOutputDetail => {
                Some(vec![Action::ScrollConsoleOutput(SCROLL_GOTO_BOTTOM)])
            }
            _ => Some(vec![Action::LogEdge { bottom: true }]),
        },
        KeyAction::PageDownHalf => {
            if state.focused_pane == PaneId::ConsoleOutputDetail {
                Some(vec![Action::ScrollConsoleOutput(
                    state.console_half_page_step(),
                )])
            } else {
                Some(vec![Action::LogPage {
                    half: true,
                    down: true,
                }])
            }
        }
        KeyAction::PageUpHalf => {
            if state.focused_pane == PaneId::ConsoleOutputDetail {
                Some(vec![Action::ScrollConsoleOutput(
                    -state.console_half_page_step(),
                )])
            } else {
                Some(vec![Action::LogPage {
                    half: true,
                    down: false,
                }])
            }
        }

        // ── Log ──
        KeyAction::CompareOrDiff | KeyAction::CompareVersions => {
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
        KeyAction::CopyHash => {
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
        KeyAction::Checkout => {
            if state.focused_pane == PaneId::LogBranchesTree {
                branch_menu_action_by_mnemonic(state, 'c')
            } else {
                let items = crate::menus::log_commit_menu_items(state);
                find_action_by_mnemonic(items, 'c')
            }
        }
        KeyAction::NewBranch => {
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
        KeyAction::ChooseParent => {
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
        KeyAction::FocusFilter => Some(vec![Action::FocusFilterText]),
        KeyAction::Uncommit => {
            let items = crate::menus::log_commit_menu_items(state);
            find_action_by_mnemonic(items, 'u')
        }
        KeyAction::Reword => {
            let items = crate::menus::log_commit_menu_items(state);
            find_action_by_mnemonic(items, 'w')
        }
        KeyAction::DropCommit => {
            let items = crate::menus::log_commit_menu_items(state);
            find_action_by_mnemonic(items, 'x')
        }

        // ── Branch ──
        KeyAction::BranchSearch => Some(vec![Action::BranchesSearchStart]),
        KeyAction::ToggleFavorite => Some(vec![Action::BranchesToggleFavorite]),
        KeyAction::RenameBranch => branch_menu_action_by_mnemonic(state, 'e'),
        KeyAction::DeleteBranch => branch_menu_action_by_mnemonic(state, 'd'),
        KeyAction::NavigateBranch => Some(vec![Action::BranchesNavigate]),

        // ── Commit workbench ──
        KeyAction::FocusCommitMsg => Some(vec![Action::FocusPane(PaneId::CommitMessageEditor)]),
        KeyAction::ToggleStage => {
            if state.focused_pane == PaneId::CommitDiffPreview {
                if is_amend_diff(state) {
                    Some(Vec::new())
                } else if state
                    .stage_workbench
                    .current_diff_key
                    .as_ref()
                    .is_some_and(|k| k.side == StageDiffSide::Staged)
                {
                    Some(vec![Action::StageDiffUnstageSelection])
                } else {
                    Some(vec![Action::StageDiffStageSelection])
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
        KeyAction::StageSelection => {
            if state.focused_pane == PaneId::CommitDiffPreview {
                if is_amend_diff(state) {
                    Some(Vec::new())
                } else {
                    Some(vec![Action::StageDiffStageSelection])
                }
            } else {
                Some(vec![Action::StageActionSelected])
            }
        }
        KeyAction::UnstageSelection => {
            if state.focused_pane == PaneId::CommitDiffPreview {
                if is_amend_diff(state) {
                    Some(Vec::new())
                } else {
                    Some(vec![Action::StageDiffUnstageSelection])
                }
            } else {
                Some(vec![Action::UnstageActionSelected])
            }
        }
        KeyAction::Commit => Some(vec![Action::SubmitCommit { and_push: false }]),
        KeyAction::CommitAndPush => Some(vec![Action::SubmitCommit { and_push: true }]),
        KeyAction::ToggleAmend => Some(vec![Action::CommitToggleAmend]),
        KeyAction::ToggleSignoff => Some(vec![Action::CommitToggleSignoff]),
        KeyAction::CommitHistory => Some(vec![Action::OpenPopup {
            kind: PopupKind::CommitHistory,
            initial_query: String::new(),
        }]),
        KeyAction::SelectAll => Some(vec![Action::StageSelectAll]),
        KeyAction::DiscardChanges => Some(vec![Action::DiscardPromptSelected]),
        KeyAction::ToggleGroup => match state.active_tab {
            WorkspaceTab::Commit => Some(vec![Action::StageToggleGroupByDir]),
            WorkspaceTab::Stash => Some(vec![Action::StashChangesToggleGroupByDir]),
            _ => Some(vec![Action::ChangesToggleGroupByDir]),
        },
        KeyAction::EditFile => Some(vec![Action::OpenExternalEditorFile(None)]),

        // ── Commit Diff (read-only when AmendCommit) ──
        KeyAction::ToggleVisual
        | KeyAction::RollbackSelection
        | KeyAction::UndoRollback
        | KeyAction::SwitchDiffSide
        | KeyAction::ThreeWayInspector => {
            if is_amend_diff(state) {
                return Some(Vec::new());
            }
            match run {
                KeyAction::ToggleVisual => Some(vec![Action::StageDiffToggleVisual]),
                KeyAction::RollbackSelection => Some(vec![Action::StageDiffRollbackSelection]),
                KeyAction::UndoRollback => Some(vec![Action::StageDiffUndoRollback]),
                KeyAction::SwitchDiffSide => Some(vec![Action::StageDiffSwitchSide]),
                KeyAction::ThreeWayInspector => Some(vec![Action::OpenThreeWayInspector {
                    row_idx: None,
                    range_idx: None,
                }]),
                _ => unreachable!(),
            }
        }

        // ── Diff (shared across Log / Commit / Stash) ──
        KeyAction::PrevHunk => {
            if state.active_tab == WorkspaceTab::Commit {
                state.stage_workbench.bracket_jump_at = Some(Instant::now());
            }
            route_diff_host(
                state,
                Action::StageDiffJumpHunk(-1),
                Action::StashDiffJumpHunk(-1),
                Action::DiffJumpHunk(-1),
            )
        }
        KeyAction::NextHunk => {
            if state.active_tab == WorkspaceTab::Commit {
                state.stage_workbench.bracket_jump_at = Some(Instant::now());
            }
            route_diff_host(
                state,
                Action::StageDiffJumpHunk(1),
                Action::StashDiffJumpHunk(1),
                Action::DiffJumpHunk(1),
            )
        }
        KeyAction::PrevFile => route_diff_host(
            state,
            Action::StageDiffNextFile(-1),
            Action::StashDiffNextFile(-1),
            Action::DiffNextFile(-1),
        ),
        KeyAction::NextFile => route_diff_host(
            state,
            Action::StageDiffNextFile(1),
            Action::StashDiffNextFile(1),
            Action::DiffNextFile(1),
        ),
        KeyAction::ToggleSideBySide => route_diff_host(
            state,
            Action::StageDiffToggleSideBySide,
            Action::StashDiffToggleSideBySide,
            Action::DiffToggleSideBySide,
        ),
        KeyAction::CycleDiffContext => route_diff_host(
            state,
            Action::StageDiffCycleContext,
            Action::StashDiffCycleContext,
            Action::DiffCycleContext,
        ),
        KeyAction::CycleDiffDock => Some(vec![Action::DiffCycleDock]),
        KeyAction::ZoomDiff => Some(vec![Action::ZoomDiff(!state.details.diff_zoomed)]),

        // ── Stash ──
        KeyAction::StashNew => Some(crate::menus::stash_button_actions(state, StashButton::New)),
        KeyAction::StashApply => Some(crate::menus::stash_button_actions(state, StashButton::Apply)),
        KeyAction::StashPop => Some(crate::menus::stash_button_actions(state, StashButton::Pop)),
        KeyAction::StashDrop => Some(crate::menus::stash_button_actions(state, StashButton::Drop)),
        KeyAction::StashBranch => {
            Some(crate::menus::stash_button_actions(state, StashButton::Branch))
        }
        KeyAction::StashClear => Some(crate::menus::stash_button_actions(state, StashButton::Clear)),
    }
}

pub fn check_scopes_filtered<'a, F>(
    events: &[KeyEvent],
    state: &'a AppState,
    mut extra_allow: F,
) -> MatchResult<'a>
where
    F: FnMut(&KeyBindingConfig) -> bool,
{
    let keymap = &state.config.keymap;
    let scopes = scopes_for_context(state);

    for scope in scopes {
        match match_bindings_filtered(keymap.bindings_for(scope), events, |b| {
            is_action_applicable(&b.run, state) && extra_allow(b)
        }) {
            MatchResult::None => {}
            other => return other,
        }
    }

    if state.details.diff_zoomed {
        return match_bindings_filtered(&keymap.global.key_bindings, events, |b| {
            matches!(b.run, KeyAction::Quit | KeyAction::Help) && extra_allow(b)
        });
    }

    MatchResult::None
}

pub fn check_scopes<'a>(events: &[KeyEvent], state: &'a AppState) -> MatchResult<'a> {
    check_scopes_filtered(events, state, |_| true)
}

fn route_dynamic_inner<F>(
    key: &KeyEvent,
    state: &mut AppState,
    mut extra_allow: F,
) -> Option<Vec<Action>>
where
    F: FnMut(&KeyBindingConfig) -> bool,
{
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

    match check_scopes_filtered(&pending, state, &mut extra_allow) {
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
                match check_scopes_filtered(&single, state, &mut extra_allow) {
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

/// Routes an incoming key through the user-configured keymap rules.
pub fn route_dynamic_key(key: &KeyEvent, state: &mut AppState) -> Option<Vec<Action>> {
    route_dynamic_inner(key, state, |_| true)
}

/// Routes only non-typing modifier/function/tab chords when inside `CommitMessageEditor`
/// so plain character typing is never swallowed while user-remapped shortcuts still work.
pub fn route_editor_dynamic_key(key: &KeyEvent, state: &mut AppState) -> Option<Vec<Action>> {
    if state.pending_keys.is_empty() {
        let is_chord = key.modifiers.intersects(
            crossterm::event::KeyModifiers::CONTROL | crossterm::event::KeyModifiers::ALT,
        ) || matches!(
            key.code,
            KeyCode::Tab | KeyCode::BackTab | KeyCode::F(_)
        );
        if !is_chord {
            return None;
        }
    }
    route_dynamic_inner(key, state, |b| {
        KeySequence::parse(&b.on).is_some_and(|s| s.starts_with_editor_chord())
    })
}
