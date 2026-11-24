//! Single declarative `CommandCatalog` for domain command enablement, action
//! resolution, context-mode structural hints, and shadowing-aware shortcut
//! queries shared by keyboard routing, context menus, toolbars, footer hints,
//! and the `?` help palette.

use jbgit_core::{
    BranchNodeKind, ChangeRowKind, DetailsMode, FileStatusKind, GitOp, Oid, PaneId, RefKind,
    TodoAction, WorkspaceTab,
};
use jbgit_engine::StageDiffSide;

use crate::app::{
    Action, AppState, ChangesHost, DiffScope, PopupKind, SCROLL_GOTO_BOTTOM, SCROLL_GOTO_TOP,
};
use crate::config::KeymapScope;
use crate::dialogs::{
    ChoiceDialog, Dialog, InputDialog, InputIntent, InputValidation, MessageDialog, MessageIntent,
};
use crate::hit_test::StashButton;
use crate::views::truncate_to_width;

use super::KeyAction;
use super::actions::{git_status_probe_action, is_amend_diff, is_diff_focused, is_log_diff_zoomed};
use super::resolve::resolve_action_shortcut;

/// Result of resolving a domain command against the current [`AppState`].
pub type CommandResult = Result<Vec<Action>, String>;

/// High-level input/display context derived once from [`AppState`] and shared
/// across the footer bar and `?` help palette.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextMode {
    FilterTextEditing,
    BranchSpeedSearch,
    ZoomedDiff,
    CommitEditor { editing_author: bool },
    Normal,
}

/// Structural (non-keybinding or fallback) hint surfaced by [`CommandCatalog`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StructuralHint {
    pub key: String,
    pub label: String,
    pub action: Option<KeyAction>,
}

/// Structural hints for [`ContextMode::Normal`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NormalStructuralHints {
    pub leading: Option<StructuralHint>,
    pub after_primary: Option<StructuralHint>,
    pub console_probe_fallback: Option<StructuralHint>,
    pub help_trailing: Option<StructuralHint>,
}

const COMMIT_EDITOR_ACTIONS: [KeyAction; 5] = [
    KeyAction::Commit,
    KeyAction::CommitAndPush,
    KeyAction::ToggleAmend,
    KeyAction::ToggleSignoff,
    KeyAction::CommitHistory,
];

const ADVANCED_TOOL_ACTIONS: [(&KeyAction, &str); 6] = [
    (&KeyAction::Palette, "—"),
    (&KeyAction::ToggleGlyphMode, "F12"),
    (&KeyAction::CycleGraphRenderer, "F8"),
    (&KeyAction::UpdateSubmodules, "—"),
    (&KeyAction::Unshallow, "—"),
    (&KeyAction::GitStatusProbe, "Console: p"),
];

pub const fn commit_editor_actions() -> &'static [KeyAction] {
    &COMMIT_EDITOR_ACTIONS
}

pub const fn advanced_tool_actions() -> &'static [(&'static KeyAction, &'static str)] {
    &ADVANCED_TOOL_ACTIONS
}

pub fn context_mode(state: &AppState) -> ContextMode {
    if state.active_tab == WorkspaceTab::Log && state.log.filter.editing_text {
        return ContextMode::FilterTextEditing;
    }
    if state.active_tab == WorkspaceTab::Log
        && state.focused_pane == PaneId::LogBranchesTree
        && (state.log.branches.searching
            || (state.config.keymap.tree_speed_search && !state.log.branches.query.is_empty()))
    {
        return ContextMode::BranchSpeedSearch;
    }
    if is_log_diff_zoomed(state) {
        return ContextMode::ZoomedDiff;
    }
    if state.active_tab == WorkspaceTab::Commit && state.focused_pane == PaneId::CommitMessageEditor
    {
        return ContextMode::CommitEditor {
            editing_author: state.stage_workbench.editing_author(),
        };
    }
    ContextMode::Normal
}

pub fn filter_editing_hints(state: &AppState) -> Vec<StructuralHint> {
    let t = |k: &str| state.i18n.t(k).to_string();
    vec![
        StructuralHint {
            key: "Enter".into(),
            label: t("view.footer.pill.apply"),
            action: None,
        },
        StructuralHint {
            key: "Esc".into(),
            label: t("view.footer.pill.cancel"),
            action: None,
        },
        StructuralHint {
            key: "Alt+R".into(),
            label: t("view.footer.pill.regex"),
            action: None,
        },
        StructuralHint {
            key: "Alt+C".into(),
            label: t("view.footer.pill.case"),
            action: None,
        },
    ]
}

pub fn branch_speed_search_hints(state: &AppState) -> Vec<StructuralHint> {
    let t = |k: &str| state.i18n.t(k).to_string();
    vec![
        StructuralHint {
            key: t("view.footer.pill.type_key"),
            label: t("view.footer.pill.filter"),
            action: None,
        },
        StructuralHint {
            key: "Enter".into(),
            label: t("view.footer.pill.apply"),
            action: None,
        },
        StructuralHint {
            key: "Esc".into(),
            label: t("view.footer.pill.cancel"),
            action: None,
        },
    ]
}

pub fn commit_editor_structural_hints(
    state: &AppState,
    editing_author: bool,
) -> (StructuralHint, StructuralHint) {
    let t = |k: &str| state.i18n.t(k).to_string();
    let esc_hint = if editing_author {
        StructuralHint {
            key: "Enter/Esc".into(),
            label: t("view.footer.pill.apply"),
            action: None,
        }
    } else {
        StructuralHint {
            key: "Esc".into(),
            label: state.i18n.t("help.action.back_to_changes").to_string(),
            action: None,
        }
    };
    let edit_hint = StructuralHint {
        key: "Ctrl+E".into(),
        label: t("view.footer.pill.edit"),
        action: None,
    };
    (esc_hint, edit_hint)
}

pub fn normal_structural_hints(state: &AppState) -> NormalStructuralHints {
    let t = |k: &str| state.i18n.t(k).to_string();
    let km = &state.config.keymap;
    let in_branch_tree =
        state.active_tab == WorkspaceTab::Log && state.focused_pane == PaneId::LogBranchesTree;
    let filter_speed_search = in_branch_tree && km.tree_speed_search;

    let leading = if filter_speed_search {
        Some(StructuralHint {
            key: t("view.footer.pill.type_key"),
            label: t("view.footer.pill.search"),
            action: None,
        })
    } else if state.active_tab == WorkspaceTab::Stash && state.focused_pane == PaneId::StashList {
        Some(StructuralHint {
            key: "Enter".into(),
            label: t("view.footer.pill.inspect"),
            action: None,
        })
    } else if state.active_tab == WorkspaceTab::Console
        && state.focused_pane == PaneId::ConsoleCommandList
    {
        Some(StructuralHint {
            key: "Enter".into(),
            label: t("view.footer.pill.detail"),
            action: None,
        })
    } else {
        None
    };

    let after_primary = if matches!(
        state.focused_pane,
        PaneId::LogChangesTree
            | PaneId::LogDetailsDiff
            | PaneId::CommitDiffPreview
            | PaneId::StashDiffPreview
            | PaneId::ConsoleOutputDetail
    ) {
        Some(StructuralHint {
            key: "Esc".into(),
            label: t("view.footer.pill.back"),
            action: None,
        })
    } else if state.focused_pane == PaneId::CommitChangesTree
        && !state.stage_workbench.diff_collapsed()
        && state
            .stage_workbench
            .selected_row()
            .is_some_and(|r| !r.is_container())
    {
        Some(StructuralHint {
            key: "Enter".into(),
            label: t("view.footer.pill.diff"),
            action: None,
        })
    } else {
        None
    };

    let console_probe_fallback = if state.active_tab == WorkspaceTab::Console
        && state.focused_pane == PaneId::ConsoleCommandList
        && km
            .primary_key_for(&[KeymapScope::Global], &KeyAction::GitStatusProbe)
            .is_none()
    {
        Some(StructuralHint {
            key: super::actions::DEFAULT_CONSOLE_PROBE_CHAR.to_string(),
            label: t("view.footer.pill.status_probe"),
            action: Some(KeyAction::GitStatusProbe),
        })
    } else {
        None
    };

    let help_trailing = if state.active_tab == WorkspaceTab::Console
        && state.focused_pane == PaneId::ConsoleOutputDetail
    {
        Some(StructuralHint {
            key: "Esc".into(),
            label: t("help.action.back_to_commands"),
            action: None,
        })
    } else {
        None
    };

    NormalStructuralHints {
        leading,
        after_primary,
        console_probe_fallback,
        help_trailing,
    }
}

pub fn footer_pill_label(state: &AppState, run: &KeyAction, fallback_desc: Option<&str>) -> String {
    let key = match run {
        KeyAction::CompareOrDiff | KeyAction::CompareVersions
            if state.active_tab == WorkspaceTab::Log
                && (state.focused_pane == PaneId::LogChangesTree
                    || (state.focused_pane == PaneId::LogDetailsDiff
                        && !state.log.details.diff_preview)) =>
        {
            Some("view.footer.pill.diff_toggle")
        }
        KeyAction::ZoomDiff if is_log_diff_zoomed(state) => Some("view.footer.pill.exit_zoom"),
        _ => run.footer_pill_key(),
    };
    match key {
        Some(k) => state.i18n.t(k).to_string(),
        None if matches!(run, KeyAction::Noop) => String::new(),
        None => fallback_desc.unwrap_or(run.as_str()).to_string(),
    }
}

pub fn localized_action_desc(
    state: &AppState,
    run: &KeyAction,
    fallback_desc: Option<&str>,
) -> String {
    let i18n_key = format!("help.action.{}", run.as_str());
    let loc = state.i18n.t(&i18n_key);
    if loc != i18n_key {
        loc.to_string()
    } else if let Some(d) = fallback_desc {
        d.to_string()
    } else {
        run.as_str().to_string()
    }
}

const fn diff_scope(state: &AppState) -> DiffScope {
    match state.active_tab {
        WorkspaceTab::Commit => DiffScope::Stage,
        WorkspaceTab::Stash => DiffScope::Stash,
        _ => DiffScope::Log,
    }
}

pub fn is_action_applicable(run: &KeyAction, state: &AppState) -> bool {
    if !run.is_active() {
        return false;
    }
    if is_log_diff_zoomed(state) {
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
                | KeyAction::CycleDiffWhitespace
                | KeyAction::ToggleDiffSoftWrap
                | KeyAction::DiffScrollLeft
                | KeyAction::DiffScrollRight
                | KeyAction::DiffSearch
                | KeyAction::DiffSearchNext
                | KeyAction::DiffSearchPrev
                | KeyAction::ZoomDiff
                | KeyAction::ChooseParent
                | KeyAction::SwapCompareDirection
        ) && (!matches!(run, KeyAction::SwapCompareDirection)
            || state.log.details.is_comparing_versions());
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
        | KeyAction::UpdateProject
        | KeyAction::ConfigureRemotes
        | KeyAction::SwitchRepository
        // Stays visible on git < 2.17 so `?` documents the requirement;
        // running it explains instead of opening the modal.
        | KeyAction::Worktrees
        | KeyAction::ResolveConflicts
        | KeyAction::Palette
        | KeyAction::ToggleGlyphMode
        | KeyAction::CycleGraphRenderer => true,
        KeyAction::UpdateSubmodules => state
            .repo_snapshot
            .as_ref()
            .is_some_and(|s| !s.submodules.is_empty()),
        KeyAction::Unshallow => state
            .repo_snapshot
            .as_ref()
            .is_some_and(|s| s.is_shallow && s.default_remote().is_some()),
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
        KeyAction::CycleDiffDock | KeyAction::ZoomDiff => state.active_tab == WorkspaceTab::Log,
        KeyAction::PrevHunk
        | KeyAction::NextHunk
        | KeyAction::PrevFile
        | KeyAction::NextFile
        | KeyAction::ToggleSideBySide
        | KeyAction::CycleDiffContext
        | KeyAction::CycleDiffWhitespace
        | KeyAction::ToggleDiffSoftWrap
        | KeyAction::DiffScrollLeft
        | KeyAction::DiffScrollRight
        | KeyAction::DiffSearch
        | KeyAction::DiffSearchNext
        | KeyAction::DiffSearchPrev => is_diff_focused(state),

        KeyAction::Checkout => {
            state.active_tab == WorkspaceTab::Log
                && matches!(
                    state.focused_pane,
                    PaneId::LogBranchesTree | PaneId::LogCommitTable
                )
        }
        KeyAction::CopyHash
        | KeyAction::ToggleLogRowSelection
        | KeyAction::CopyCommitMessage
        | KeyAction::CopyRowSummary
        | KeyAction::Uncommit
        | KeyAction::Reword
        | KeyAction::DropCommit
        | KeyAction::CollapseLinearBranch
        | KeyAction::ExpandLinearBranch => {
            state.active_tab == WorkspaceTab::Log && state.focused_pane == PaneId::LogCommitTable
        }
        KeyAction::ToggleOnlyAffectedChanges => {
            state.active_tab == WorkspaceTab::Log
                && matches!(
                    state.focused_pane,
                    PaneId::LogCommitTable | PaneId::LogChangesTree | PaneId::LogDetailsDiff
                )
        }
        KeyAction::SwapCompareDirection => {
            state.active_tab == WorkspaceTab::Log && state.log.details.is_comparing_versions()
        }
        KeyAction::CollapseAllLinearBranches | KeyAction::ExpandAllLinearBranches => {
            state.active_tab == WorkspaceTab::Log
        }
        KeyAction::ApplyPatch | KeyAction::ApplyPatchClipboard => {
            state.active_tab == WorkspaceTab::Commit
        }
        KeyAction::BrowseRevision => {
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
        KeyAction::FileHistory => {
            (state.active_tab == WorkspaceTab::Log && state.focused_pane == PaneId::LogChangesTree)
                || (state.active_tab == WorkspaceTab::Commit
                    && state.focused_pane == PaneId::CommitChangesTree)
        }
        KeyAction::Annotate => {
            (state.active_tab == WorkspaceTab::Log
                && matches!(
                    state.focused_pane,
                    PaneId::LogChangesTree | PaneId::LogDetailsDiff
                ))
                || (state.active_tab == WorkspaceTab::Commit
                    && matches!(
                        state.focused_pane,
                        PaneId::CommitChangesTree | PaneId::CommitDiffPreview
                    ))
        }
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

        KeyAction::FocusCommitMsg => {
            state.active_tab == WorkspaceTab::Commit
                && matches!(
                    state.focused_pane,
                    PaneId::CommitChangesTree | PaneId::CommitDiffPreview
                )
        }
        KeyAction::SelectAll | KeyAction::DiscardChanges => {
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
                | PaneId::ConsoleCommandList
                | PaneId::ConsoleOutputDetail
        ),
    }
}

pub fn execute_action(run: &KeyAction, state: &AppState) -> Option<Vec<Action>> {
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
        KeyAction::SwitchRepository => Some(vec![Action::OpenRepoSwitcher]),
        KeyAction::ApplyPatch => Some(notice_on_err(apply_patch_command(state, false))),
        KeyAction::ApplyPatchClipboard => Some(notice_on_err(apply_patch_command(state, true))),
        KeyAction::BrowseRevision => Some(notice_on_err(browse_revision_command(state))),
        KeyAction::CollapseLinearBranch => Some(notice_on_err(graph_collapse_selected(state))),
        KeyAction::ExpandLinearBranch => Some(notice_on_err(graph_expand_selected(state))),
        KeyAction::CollapseAllLinearBranches => Some(notice_on_err(graph_collapse_all(state))),
        KeyAction::ExpandAllLinearBranches => Some(notice_on_err(graph_expand_all(state))),
        KeyAction::UpdateSubmodules => {
            if let Err(msg) = state.require_no_ongoing() {
                return Some(vec![Action::Notice(msg)]);
            }
            if state
                .repo_snapshot
                .as_ref()
                .is_none_or(|s| s.submodules.is_empty())
            {
                return Some(vec![Action::Notice(
                    state.i18n.t("toast.no_submodules").to_string(),
                )]);
            }
            Some(vec![Action::RunGitOp(GitOp::SubmoduleUpdate { path: None })])
        }
        KeyAction::Unshallow => Some(vec![Action::PromptUnshallow]),
        KeyAction::Worktrees => Some(if !crate::worktrees_view::worktrees_supported(state) {
            vec![crate::worktrees_view::unsupported_notice(state)]
        } else if state.repo_snapshot.is_some() {
            vec![Action::OpenWorktrees]
        } else {
            vec![Action::Notice(state.i18n.t("toast.repo_not_loaded").into())]
        }),
        KeyAction::ResolveConflicts => Some(vec![Action::OngoingResolve]),
        KeyAction::ConfigureRemotes => Some(if state.repo_snapshot.is_some() {
            vec![Action::OpenRemotes]
        } else {
            vec![Action::Notice(state.i18n.t("toast.repo_not_loaded").into())]
        }),
        KeyAction::UpdateProject => Some(match state.update_project_blocker() {
            Some(msg) => vec![Action::Notice(msg)],
            None => vec![Action::RunGitOp(
                state.update_project_op(state.configured_update_mode()),
            )],
        }),
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
        KeyAction::CursorDown if is_log_diff_zoomed(state) => {
            Some(vec![Action::DiffScroll(DiffScope::Log, 1)])
        }
        KeyAction::CursorDown => match state.focused_pane {
            PaneId::LogBranchesTree => Some(vec![Action::ScrollBranches(1)]),
            PaneId::LogCommitTable => Some(vec![Action::ScrollLog(1)]),
            PaneId::LogChangesTree => Some(vec![Action::ChangesMove(ChangesHost::Log, 1)]),
            PaneId::LogDetailsDiff if state.log.details.diff_visible() => {
                Some(vec![Action::DiffScroll(DiffScope::Log, 1)])
            }
            PaneId::LogDetailsDiff => Some(vec![Action::ScrollDetails(1)]),
            PaneId::CommitChangesTree => Some(vec![Action::ScrollCommitStage(1)]),
            PaneId::CommitDiffPreview => Some(vec![Action::StageDiffMoveCursor(1)]),
            PaneId::StashList => Some(vec![Action::StashMove(1)]),
            PaneId::StashDiffPreview => Some(vec![Action::ChangesMove(ChangesHost::Stash, 1)]),
            PaneId::ConsoleCommandList => Some(vec![Action::ScrollConsoleList(1)]),
            PaneId::ConsoleOutputDetail => Some(vec![Action::ScrollConsoleOutput(1)]),
            PaneId::CommitMessageEditor => None,
        },
        KeyAction::CursorUp if is_log_diff_zoomed(state) => {
            Some(vec![Action::DiffScroll(DiffScope::Log, -1)])
        }
        KeyAction::CursorUp => match state.focused_pane {
            PaneId::LogBranchesTree => Some(vec![Action::ScrollBranches(-1)]),
            PaneId::LogCommitTable => Some(vec![Action::ScrollLog(-1)]),
            PaneId::LogChangesTree => Some(vec![Action::ChangesMove(ChangesHost::Log, -1)]),
            PaneId::LogDetailsDiff if state.log.details.diff_visible() => {
                Some(vec![Action::DiffScroll(DiffScope::Log, -1)])
            }
            PaneId::LogDetailsDiff => Some(vec![Action::ScrollDetails(-1)]),
            PaneId::CommitChangesTree => Some(vec![Action::ScrollCommitStage(-1)]),
            PaneId::CommitDiffPreview => Some(vec![Action::StageDiffMoveCursor(-1)]),
            PaneId::StashList => Some(vec![Action::StashMove(-1)]),
            PaneId::StashDiffPreview => Some(vec![Action::ChangesMove(ChangesHost::Stash, -1)]),
            PaneId::ConsoleCommandList => Some(vec![Action::ScrollConsoleList(-1)]),
            PaneId::ConsoleOutputDetail => Some(vec![Action::ScrollConsoleOutput(-1)]),
            PaneId::CommitMessageEditor => None,
        },
        KeyAction::GotoTop if is_log_diff_zoomed(state) => {
            Some(vec![Action::DiffScroll(DiffScope::Log, SCROLL_GOTO_TOP)])
        }
        KeyAction::GotoTop => match state.focused_pane {
            PaneId::LogBranchesTree => Some(vec![Action::SelectBranchRow(0)]),
            PaneId::LogChangesTree => Some(vec![Action::ChangesSelect(ChangesHost::Log, 0)]),
            PaneId::LogDetailsDiff if state.log.details.diff_visible() => {
                Some(vec![Action::DiffScroll(DiffScope::Log, SCROLL_GOTO_TOP)])
            }
            PaneId::LogDetailsDiff => Some(vec![Action::ScrollDetails(SCROLL_GOTO_TOP)]),
            PaneId::CommitChangesTree => Some(vec![Action::SelectCommitStageRow(0)]),
            PaneId::CommitDiffPreview => Some(vec![Action::StageDiffMoveCursor(SCROLL_GOTO_TOP)]),
            PaneId::StashList => Some(vec![Action::StashSelect(0)]),
            PaneId::StashDiffPreview => Some(vec![Action::ChangesSelect(ChangesHost::Stash, 0)]),
            PaneId::ConsoleCommandList => Some(vec![Action::SelectConsoleRow(0)]),
            PaneId::ConsoleOutputDetail => Some(vec![Action::ScrollConsoleOutput(SCROLL_GOTO_TOP)]),
            _ => Some(vec![Action::LogEdge { bottom: false }]),
        },
        KeyAction::GotoBottom if is_log_diff_zoomed(state) => {
            Some(vec![Action::DiffScroll(DiffScope::Log, SCROLL_GOTO_BOTTOM)])
        }
        KeyAction::GotoBottom => match state.focused_pane {
            PaneId::LogBranchesTree => Some(vec![Action::SelectBranchRow(usize::MAX)]),
            PaneId::LogChangesTree => {
                Some(vec![Action::ChangesSelect(ChangesHost::Log, usize::MAX)])
            }
            PaneId::LogDetailsDiff if state.log.details.diff_visible() => {
                Some(vec![Action::DiffScroll(DiffScope::Log, SCROLL_GOTO_BOTTOM)])
            }
            PaneId::LogDetailsDiff => Some(vec![Action::ScrollDetails(SCROLL_GOTO_BOTTOM)]),
            PaneId::CommitChangesTree => Some(vec![Action::SelectCommitStageRow(usize::MAX)]),
            PaneId::CommitDiffPreview => {
                Some(vec![Action::StageDiffMoveCursor(SCROLL_GOTO_BOTTOM)])
            }
            PaneId::StashList => Some(vec![Action::StashSelect(usize::MAX)]),
            PaneId::StashDiffPreview => {
                Some(vec![Action::ChangesSelect(ChangesHost::Stash, usize::MAX)])
            }
            PaneId::ConsoleCommandList => Some(vec![Action::SelectConsoleRow(usize::MAX)]),
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
        KeyAction::SwapCompareDirection => Some(vec![Action::SwapCompareDirection]),
        KeyAction::ToggleLogRowSelection => Some(vec![Action::ToggleLogRowSelection]),
        KeyAction::ToggleOnlyAffectedChanges => Some(vec![Action::TogglePresentation(
            crate::log_view::PresentationToggle::ShowOnlyAffectedChanges,
        )]),
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
        KeyAction::CopyCommitMessage => Some(notice_on_err(commit_copy_message(state))),
        KeyAction::CopyRowSummary => Some(notice_on_err(commit_copy_row_summary(state))),
        KeyAction::Checkout => {
            if state.focused_pane == PaneId::LogBranchesTree {
                selected_branch_command(state, &KeyAction::Checkout)
            } else {
                commit_checkout(state).ok()
            }
        }
        KeyAction::NewBranch => {
            if state.active_tab == WorkspaceTab::Log
                && state.focused_pane == PaneId::LogBranchesTree
                && let Some(node) = state.log.branches.selected_node()
                && let BranchNodeKind::Leaf(kind) = node.kind
                && let Some(name) = node.ref_name.as_deref()
            {
                let existing = local_names(state);
                let default = if kind == RefKind::RemoteBranch {
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
                && let Some(commit) = state.log_commit_at(state.log.graph.selected_idx)
            {
                let oid_str = commit.oid.to_string();
                let short = commit.oid.short();
                let existing = local_names(state);
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
            if is_log_diff_zoomed(state)
                || matches!(
                    state.focused_pane,
                    PaneId::LogChangesTree | PaneId::LogDetailsDiff
                )
            {
                Some(vec![Action::SwitchMergeParent])
            } else {
                let parents = state
                    .log_commit_at(state.log.graph.selected_idx)
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
        KeyAction::FileHistory => {
            if state.active_tab == WorkspaceTab::Log && state.focused_pane == PaneId::LogChangesTree
            {
                let rows = state.log.details.change_rows(&state.diff_caches);
                let Some(row) = rows.get(state.log.details.changes.selected) else {
                    return Some(Vec::new());
                };
                match row.kind {
                    ChangeRowKind::File { change_idx } => {
                        let bundle = state.log.details.current_bundle(&state.diff_caches)?;
                        let change = bundle.changes.get(change_idx)?;
                        let start_oid = (bundle.key.mode == DetailsMode::Commits
                            && bundle.key.oids.len() == 1)
                            .then(|| bundle.key.oids[0]);
                        Some(vec![Action::OpenFileHistory {
                            path: change.path.clone(),
                            start_oid,
                            follow_renames: true,
                            line_range: None,
                        }])
                    }
                    ChangeRowKind::Dir { .. } => {
                        Some(vec![Action::FilterLogByPath(row.key.clone())])
                    }
                }
            } else if state.active_tab == WorkspaceTab::Commit
                && state.focused_pane == PaneId::CommitChangesTree
            {
                let idx = state.stage_workbench.selected_idx();
                let group = state
                    .stage_workbench
                    .rows()
                    .get(idx)
                    .map(|r| r.group_kind());
                let Some(entry) = state.stage_entry_at(idx) else {
                    return Some(Vec::new());
                };
                let untracked = group == Some(jbgit_core::StageNodeKind::Untracked)
                    || entry.kind == FileStatusKind::Untracked;
                let ignored = group == Some(jbgit_core::StageNodeKind::Ignored)
                    || entry.kind == FileStatusKind::Ignored;
                if untracked || ignored {
                    Some(vec![Action::Notice(
                        state.i18n.t("menu.stage.only_tracked_history").to_string(),
                    )])
                } else {
                    Some(vec![Action::OpenFileHistory {
                        path: entry.path,
                        start_oid: None,
                        follow_renames: true,
                        line_range: None,
                    }])
                }
            } else {
                Some(Vec::new())
            }
        }
        KeyAction::Annotate => annotate_selected_file(state),
        KeyAction::Uncommit => commit_uncommit(state).ok(),
        KeyAction::Reword => commit_reword(state).ok(),
        KeyAction::DropCommit => {
            let oids = state.selected_commits_oldest_first();
            commit_drop(state, &oids).ok()
        }

        // ── Branch ──
        KeyAction::BranchSearch => Some(vec![Action::BranchesSearchStart]),
        KeyAction::ToggleFavorite => Some(vec![Action::BranchesToggleFavorite]),
        KeyAction::RenameBranch => selected_branch_command(state, &KeyAction::RenameBranch),
        KeyAction::DeleteBranch => selected_branch_command(state, &KeyAction::DeleteBranch),
        KeyAction::NavigateBranch => Some(vec![Action::BranchesNavigate]),

        // ── Commit workbench ──
        KeyAction::FocusCommitMsg => Some(vec![Action::FocusPane(PaneId::CommitMessageEditor)]),
        KeyAction::ToggleStage => {
            if state.focused_pane == PaneId::CommitDiffPreview {
                if is_amend_diff(state) {
                    Some(Vec::new())
                } else if state
                    .stage_workbench
                    .current_diff_key()
                    .is_some_and(|k| k.side == StageDiffSide::Staged)
                {
                    Some(vec![Action::StageDiffUnstageSelection])
                } else {
                    Some(vec![Action::StageDiffStageSelection])
                }
            } else {
                let sel = state.stage_workbench.selected_idx();
                let row = state.stage_workbench.selected_row();
                match row {
                    Some(r) if r.group_kind().can_stage() => {
                        Some(vec![Action::StageActionSelected])
                    }
                    Some(r) if r.group_kind().can_unstage() => {
                        Some(vec![Action::UnstageActionSelected])
                    }
                    Some(r) if r.is_container() => Some(vec![Action::StageToggleRow(sel)]),
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
            WorkspaceTab::Stash => Some(vec![Action::ChangesToggleGroupByDir(ChangesHost::Stash)]),
            _ => Some(vec![Action::ChangesToggleGroupByDir(ChangesHost::Log)]),
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
        KeyAction::PrevHunk => Some(vec![Action::DiffJumpHunk(diff_scope(state), -1)]),
        KeyAction::NextHunk => Some(vec![Action::DiffJumpHunk(diff_scope(state), 1)]),
        KeyAction::PrevFile => Some(vec![Action::DiffNextFile(diff_scope(state), -1)]),
        KeyAction::NextFile => Some(vec![Action::DiffNextFile(diff_scope(state), 1)]),
        KeyAction::ToggleSideBySide => Some(vec![Action::DiffToggleSideBySide(diff_scope(state))]),
        KeyAction::CycleDiffContext => Some(vec![Action::DiffCycleContext(diff_scope(state))]),
        KeyAction::CycleDiffWhitespace => {
            Some(vec![Action::DiffCycleWhitespace(diff_scope(state))])
        }
        KeyAction::ToggleDiffSoftWrap => Some(vec![Action::DiffToggleSoftWrap(diff_scope(state))]),
        KeyAction::DiffScrollLeft => Some(vec![Action::DiffScrollHoriz(
            diff_scope(state),
            -crate::diff_session::DIFF_HORIZ_SCROLL_STEP,
        )]),
        KeyAction::DiffScrollRight => Some(vec![Action::DiffScrollHoriz(
            diff_scope(state),
            crate::diff_session::DIFF_HORIZ_SCROLL_STEP,
        )]),
        KeyAction::DiffSearch => Some(vec![Action::DiffSearchOpen(diff_scope(state))]),
        KeyAction::DiffSearchNext => Some(vec![Action::DiffSearchJump(diff_scope(state), 1)]),
        KeyAction::DiffSearchPrev => Some(vec![Action::DiffSearchJump(diff_scope(state), -1)]),
        KeyAction::CycleDiffDock => Some(vec![Action::DiffCycleDock]),
        KeyAction::ZoomDiff => Some(vec![Action::ZoomDiff(!state.log.details.diff.zoomed)]),

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

fn op(o: GitOp) -> Vec<Action> {
    vec![Action::RunGitOp(o)]
}

/// Copies the full commit message (subject + body) of selected Log commit(s).
pub fn commit_copy_message(state: &AppState) -> CommandResult {
    let Some(snap) = state.repo_snapshot.as_deref() else {
        return Err(state.i18n.t("toast.repo_not_loaded").to_string());
    };
    let oids = state.selected_commits_oldest_first();
    if oids.is_empty() {
        return Err(state.i18n.t("menu.log.need_single").to_string());
    }
    let bundle = state.log.details.current_bundle(&state.diff_caches);
    let mut parts = Vec::with_capacity(oids.len());
    for oid in oids.iter().rev() {
        let from_details = bundle
            .and_then(|b| b.details.iter().find(|d| d.oid == *oid))
            .map(|d| d.message.trim_end_matches(['\r', '\n']).to_string())
            .filter(|m| !m.is_empty());
        if let Some(msg) = from_details {
            parts.push(msg);
        } else if let Some(c) = snap.commit(oid) {
            parts.push(c.full_message());
        }
    }
    if parts.is_empty() {
        return Err(state.i18n.t("toast.repo_not_loaded").to_string());
    }
    Ok(vec![Action::CopyText(parts.join("\n\n"))])
}

/// Copies one-line `hash subject — author (date)` summaries for selected Log commit(s).
pub fn commit_copy_row_summary(state: &AppState) -> CommandResult {
    let Some(snap) = state.repo_snapshot.as_deref() else {
        return Err(state.i18n.t("toast.repo_not_loaded").to_string());
    };
    let oids = state.selected_commits_oldest_first();
    if oids.is_empty() {
        return Err(state.i18n.t("menu.log.need_single").to_string());
    }
    let mut lines = Vec::with_capacity(oids.len());
    for oid in oids.iter().rev() {
        if let Some(c) = snap.commit(oid) {
            let date = crate::log_view::format_absolute_time(c.timestamp, true);
            lines.push(format!(
                "{} {} — {} ({})",
                c.oid.short(),
                c.subject,
                c.author_name,
                date
            ));
        }
    }
    if lines.is_empty() {
        return Err(state.i18n.t("toast.repo_not_loaded").to_string());
    }
    Ok(vec![Action::CopyText(lines.join("\n"))])
}

fn dialog(d: Dialog) -> Vec<Action> {
    vec![Action::OpenDialog(Box::new(d))]
}

fn local_names(state: &AppState) -> Vec<String> {
    state
        .repo_snapshot
        .as_ref()
        .map(|s| {
            s.local_branch_names()
                .iter()
                .map(|n| n.to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// Shadowing-aware shortcut lookup for menus and toolbars.
///
/// Checks active scopes (with `preferred_scopes` appended so menu items opened
/// from a pane still show their pane shortcut unless shadowed by a higher-priority
/// active scope or overridden to `noop`).
pub fn shortcut_for_action(
    state: &AppState,
    preferred_scopes: &[KeymapScope],
    action: &KeyAction,
) -> Option<String> {
    let mut scopes = super::actions::scopes_for_context(state);
    for &s in preferred_scopes {
        if !scopes.contains(&s) {
            scopes.push(s);
        }
    }
    resolve_action_shortcut(state, &scopes, action)
}

/// Shadowing-aware multi-key display string (e.g. `"d / Delete"`) for destructive
/// menu entries, omitting any binding shadowed by a higher-priority scope or `noop`.
pub fn all_shortcuts_for_action(
    state: &AppState,
    preferred_scopes: &[KeymapScope],
    action: &KeyAction,
) -> Option<String> {
    let active = super::actions::scopes_for_context(state);
    let all = super::resolve::resolve_all_action_shortcuts(state, &active, preferred_scopes, action);
    if all.is_empty() {
        None
    } else {
        Some(all.join(" / "))
    }
}

fn notice_on_err(result: CommandResult) -> Vec<Action> {
    result.unwrap_or_else(|msg| vec![Action::Notice(msg)])
}

/// `Apply Patch…` (path prompt) / `Apply Patch from Clipboard`.
pub fn apply_patch_command(state: &AppState, clipboard: bool) -> CommandResult {
    if state.repo_snapshot.is_none() {
        return Err(state.i18n.t("toast.repo_not_loaded").to_string());
    }
    state.require_no_ongoing()?;
    Ok(if clipboard {
        vec![Action::OpenApplyPatch(jbgit_core::PatchInput::Clipboard)]
    } else {
        vec![Action::OpenDialog(Box::new(
            crate::dialogs::apply_patch_file_dialog(state),
        ))]
    })
}

/// `Browse Repository at Revision` for the selected (single) Log commit.
pub fn browse_revision_command(state: &AppState) -> CommandResult {
    let i18n = &state.i18n;
    let Some(commit) = state.log_commit_at(state.log.graph.selected_idx) else {
        return Err(i18n.t("menu.log.drop_no_selection").to_string());
    };
    if state.selected_commits_oldest_first().len() > 1 {
        return Err(i18n.t_fmt(
            "menu.log.need_single",
            &[("what", i18n.t("menu.log.browse_revision"))],
        ));
    }
    Ok(vec![Action::OpenTreeBrowser { oid: commit.oid }])
}

/// Linear-branch collapsing is only offered on the unfiltered graph (IDEA
/// disables `Collapse Linear Branches` while filtering or in first-parent mode).
pub fn graph_collapse_available(state: &AppState) -> Result<(), String> {
    let i18n = &state.i18n;
    if state.repo_snapshot.is_none() || state.log.graph.cached.is_none() {
        return Err(i18n.t("toast.repo_not_loaded").to_string());
    }
    if state.log.graph.options.first_parent_only {
        return Err(i18n
            .t("menu.log.collapse_disabled_first_parent")
            .to_string());
    }
    if !state.log.filter.spec.is_empty() {
        return Err(i18n.t("menu.log.collapse_disabled_filter").to_string());
    }
    Ok(())
}

/// `Collapse Linear Branch` (`zc`): the segment holding the selected commit,
/// else the expanded segment hanging below it.
pub fn graph_collapse_selected(state: &AppState) -> CommandResult {
    graph_collapse_available(state)?;
    let graph = &state.log.graph;
    let cached = graph.cached.as_ref().expect("checked above");
    cached
        .expanded_segments_at(graph.selected_idx)
        .first()
        .map(|&id| vec![Action::GraphCollapseSegment(id)])
        .ok_or_else(|| state.i18n.t("menu.log.no_linear_branch_here").to_string())
}

/// `Expand Linear Branch` (`zo`): collapsed segments below the selected commit.
pub fn graph_expand_selected(state: &AppState) -> CommandResult {
    graph_collapse_available(state)?;
    let graph = &state.log.graph;
    let cached = graph.cached.as_ref().expect("checked above");
    let ids = cached.collapsed_segments_below(graph.selected_idx);
    if ids.is_empty() {
        return Err(state.i18n.t("menu.log.nothing_collapsed_here").to_string());
    }
    Ok(vec![Action::GraphExpandSegments(ids)])
}

/// `Collapse All Linear Branches` (`zM`).
pub fn graph_collapse_all(state: &AppState) -> CommandResult {
    graph_collapse_available(state)?;
    let cached = state.log.graph.cached.as_ref().expect("checked above");
    if cached.segments.is_empty() {
        return Err(state.i18n.t("menu.log.no_linear_branches").to_string());
    }
    Ok(vec![Action::GraphCollapseAll])
}

/// `Expand All Linear Branches` (`zR`).
pub fn graph_expand_all(state: &AppState) -> CommandResult {
    graph_collapse_available(state)?;
    if !state.log.graph.collapse.is_active() {
        return Err(state.i18n.t("menu.log.nothing_collapsed").to_string());
    }
    Ok(vec![Action::GraphExpandAll])
}

/// Resolves `Checkout` for the selected commit row in `[2 Log]`.
pub fn commit_checkout(state: &AppState) -> CommandResult {
    let i18n = &state.i18n;
    let Some(snap) = state.repo_snapshot.as_deref() else {
        return Err(i18n.t("menu.log.no_current_branch").to_string());
    };
    let Some(primary) = state.log_commit_at(state.log.graph.selected_idx) else {
        return Err(i18n.t("menu.log.drop_no_selection").to_string());
    };
    let oids = state.selected_commits_oldest_first();
    if oids.len() > 1 {
        return Err(i18n.t_fmt(
            "menu.log.need_single",
            &[("what", i18n.t("menu.branch.checkout"))],
        ));
    }
    state.require_no_ongoing()?;

    let local = snap
        .refs
        .iter()
        .find(|r| r.kind == RefKind::LocalBranch && r.target_oid == primary.oid)
        .map(|r| r.name.clone());
    Ok(match local {
        Some(branch) => op(GitOp::Checkout {
            target: branch,
            detach: false,
        }),
        None => op(GitOp::Checkout {
            target: primary.oid.to_string(),
            detach: true,
        }),
    })
}

/// Resolves `Uncommit` (`HEAD` -> `HEAD~1` soft reset) for the selected commit row.
pub fn commit_uncommit(state: &AppState) -> CommandResult {
    let i18n = &state.i18n;
    let Some(snap) = state.repo_snapshot.as_deref() else {
        return Err(i18n.t("menu.log.uncommit_only_head").to_string());
    };
    let Some(primary) = state.log_commit_at(state.log.graph.selected_idx) else {
        return Err(i18n.t("menu.log.uncommit_only_head").to_string());
    };
    let single = state.selected_commits_oldest_first().len() <= 1;
    let is_head = single && snap.head_oid == Some(primary.oid);
    if !is_head {
        return Err(i18n.t("menu.log.uncommit_only_head").to_string());
    }
    if primary.parents.is_empty() {
        return Err(i18n.t("menu.log.uncommit_root").to_string());
    }
    state.require_no_ongoing()?;
    Ok(vec![Action::UncommitHead])
}

/// Resolves `Reword` (HEAD fast path or history rewrite) for the selected commit row.
pub fn commit_reword(state: &AppState) -> CommandResult {
    let i18n = &state.i18n;
    let Some(snap) = state.repo_snapshot.as_deref() else {
        return Err(i18n.t("menu.log.no_current_branch").to_string());
    };
    let Some(primary) = state.log_commit_at(state.log.graph.selected_idx) else {
        return Err(i18n.t("menu.log.drop_no_selection").to_string());
    };
    let oids = state.selected_commits_oldest_first();
    let single = oids.len() <= 1;
    let reword_lbl = i18n.t("menu.log.reword");
    if !single {
        return Err(i18n.t_fmt("menu.log.need_single", &[("what", reword_lbl)]));
    }
    let short = primary.oid.short();
    let reword_title = i18n.t_fmt("menu.log.reword_title", &[("short", &short)]);
    let is_head = snap.head_oid == Some(primary.oid);
    if is_head {
        state.require_no_ongoing()?;
        Ok(dialog(Dialog::Message(MessageDialog::new(
            reword_title,
            primary.full_message(),
            MessageIntent::RewordHead,
        ))))
    } else {
        let plan = state.rewrite_plan(primary.oid, "reword")?;
        let mut todo = plan.entries.clone();
        todo[0].action = TodoAction::Reword;
        Ok(dialog(Dialog::Message(MessageDialog::new(
            reword_title,
            plan.messages[0].clone(),
            MessageIntent::Rewrite {
                base: plan.base,
                todo,
                entry: 0,
            },
        ))))
    }
}

/// Resolves `Drop Commit(s)` confirmation dialog for `oids` (oldest-first).
pub fn commit_drop(state: &AppState, oids: &[Oid]) -> CommandResult {
    let i18n = &state.i18n;
    let Some(&oldest) = oids.first() else {
        return Err(i18n.t("menu.log.drop_no_selection").to_string());
    };
    let plan = state.rewrite_plan(oldest, "drop")?;
    let mut todo = plan.entries.clone();
    let mut subjects = Vec::new();
    for e in &mut todo {
        if oids.contains(&e.oid) {
            e.action = TodoAction::Drop;
            subjects.push(format!(
                "  • {} {}",
                e.oid.short(),
                truncate_to_width(&e.subject, 44)
            ));
        }
    }
    if subjects.len() != oids.len() {
        return Err(i18n.t("menu.log.drop_not_reachable").to_string());
    }
    let count_s = oids.len().to_string();
    let mut body = vec![i18n.t_fmt("menu.log.drop_confirm_body", &[("count", &count_s)])];
    body.extend(subjects);
    Ok(dialog(Dialog::Choice(ChoiceDialog::confirm(
        i18n.t("menu.log.drop_confirm_title"),
        body,
        i18n.t("menu.log.drop_confirm_btn"),
        i18n.cancel(),
        op(GitOp::RewriteHistory {
            base: plan.base,
            todo,
        }),
    ))))
}

/// Resolves `Checkout` for a branch/tag node in the Branches tree or popup.
pub fn branch_checkout(state: &AppState, name: &str, kind: RefKind) -> CommandResult {
    let i18n = &state.i18n;
    let Some(snap) = state.repo_snapshot.as_deref() else {
        return Err(i18n.t("menu.branch.target_not_loaded").to_string());
    };
    state.require_no_ongoing()?;
    let current = snap.head_branch.as_deref();
    match kind {
        RefKind::LocalBranch | RefKind::Head => {
            let target = if kind == RefKind::Head {
                current.unwrap_or("HEAD").to_string()
            } else {
                name.to_string()
            };
            if current == Some(target.as_str()) {
                return Err(i18n.t_fmt("menu.branch.already_checked_out", &[("name", &target)]));
            }
            if let Some(wt) = snap.worktree_of_branch(&target) {
                return Err(i18n.t_fmt(
                    "menu.branch.checked_out_in_worktree",
                    &[("name", &target), ("worktree", &wt.name())],
                ));
            }
            Ok(op(GitOp::Checkout {
                target,
                detach: false,
            }))
        }
        RefKind::RemoteBranch => Ok(vec![Action::CheckoutRemoteBranch {
            remote_ref: name.to_string(),
        }]),
        RefKind::Tag => Ok(op(GitOp::Checkout {
            target: name.to_string(),
            detach: true,
        })),
    }
}

/// Resolves `Rename Branch` for a branch node.
pub fn branch_rename(state: &AppState, name: &str, kind: RefKind) -> CommandResult {
    let i18n = &state.i18n;
    let Some(snap) = state.repo_snapshot.as_deref() else {
        return Err(i18n.t("menu.branch.target_not_loaded").to_string());
    };
    match kind {
        RefKind::LocalBranch | RefKind::Head => {
            let target = if kind == RefKind::Head {
                snap.head_branch.as_deref().unwrap_or("HEAD").to_string()
            } else {
                name.to_string()
            };
            let mut existing = local_names(state);
            existing.retain(|n| n != &target);
            Ok(dialog(Dialog::Input(InputDialog::new(
                i18n.t_fmt("dialog.rename_branch.title", &[("name", &target)]),
                i18n.t("dialog.rename_branch.prompt"),
                target.clone(),
                InputValidation::RefName {
                    what: "Branch",
                    existing,
                },
                InputIntent::RenameBranch { old: target },
            ))))
        }
        RefKind::RemoteBranch | RefKind::Tag => Err(String::new()),
    }
}

/// Resolves `Delete Branch` for a local or remote branch node.
pub fn branch_delete(state: &AppState, name: &str, kind: RefKind) -> CommandResult {
    let i18n = &state.i18n;
    let Some(snap) = state.repo_snapshot.as_deref() else {
        return Err(i18n.t("menu.branch.target_not_loaded").to_string());
    };
    match kind {
        RefKind::LocalBranch | RefKind::Head => {
            let target = if kind == RefKind::Head {
                snap.head_branch.as_deref().unwrap_or("HEAD").to_string()
            } else {
                name.to_string()
            };
            if snap.head_branch.as_deref() == Some(target.as_str()) {
                return Err(i18n.t("menu.branch.cannot_delete_current").to_string());
            }
            Ok(vec![Action::PromptDeleteBranch { name: target }])
        }
        RefKind::RemoteBranch => {
            let (remote, short) = name.split_once('/').unwrap_or(("origin", name));
            Ok(dialog(Dialog::Choice(ChoiceDialog::confirm(
                i18n.t("menu.branch.delete_remote_title"),
                vec![
                    i18n.t_fmt(
                        "menu.branch.delete_remote_prompt",
                        &[("short", short), ("remote", remote)],
                    ),
                    i18n.t("menu.branch.delete_remote_warn").into(),
                ],
                i18n.t("menu.branch.delete_remote_title"),
                i18n.cancel(),
                op(GitOp::DeleteRemoteBranch {
                    remote: remote.to_string(),
                    branch: short.to_string(),
                }),
            ))))
        }
        RefKind::Tag => {
            if snap
                .refs
                .iter()
                .any(|r| r.kind == RefKind::Tag && r.name == name)
            {
                Ok(vec![Action::PromptDeleteTag {
                    name: name.to_string(),
                }])
            } else {
                Err(i18n.t_fmt("toast.tag_not_found", &[("name", name)]))
            }
        }
    }
}

/// Resolves a branch-scoped keyboard command against the currently selected node in
/// the Branches tree, without constructing or scanning any context menu.
pub fn selected_branch_command(state: &AppState, action: &KeyAction) -> Option<Vec<Action>> {
    let node = state.log.branches.selected_node()?;
    let BranchNodeKind::Leaf(kind) = node.kind else {
        return None;
    };
    let name = node.ref_name.as_deref()?;
    let res = match action {
        KeyAction::Checkout => branch_checkout(state, name, kind),
        KeyAction::RenameBranch => branch_rename(state, name, kind),
        KeyAction::DeleteBranch => branch_delete(state, name, kind),
        _ => return None,
    };
    res.ok()
}

/// Resolves `Annotate with Git Blame` for the currently selected file in Log
/// (`LogChangesTree` / `LogDetailsDiff`) or Commit (`CommitChangesTree` / `CommitDiffPreview`).
pub fn annotate_selected_file(state: &AppState) -> Option<Vec<Action>> {
    let res = if state.active_tab == WorkspaceTab::Log
        && matches!(
            state.focused_pane,
            PaneId::LogChangesTree | PaneId::LogDetailsDiff
        ) {
        annotate_log_change(state)
    } else if state.active_tab == WorkspaceTab::Commit
        && matches!(
            state.focused_pane,
            PaneId::CommitChangesTree | PaneId::CommitDiffPreview
        ) {
        annotate_stage_entry(state)
    } else {
        return Some(Vec::new());
    };
    match res {
        Ok(actions) => Some(actions),
        Err(reason) if reason.is_empty() => Some(Vec::new()),
        Err(reason) => Some(vec![Action::Notice(reason)]),
    }
}

pub fn annotate_log_change(state: &AppState) -> CommandResult {
    let i18n = &state.i18n;
    let Some(bundle) = state.log.details.current_bundle(&state.diff_caches) else {
        return Err(i18n.t("menu.log_change.need_file").into());
    };
    let rows = state.log.details.change_rows(&state.diff_caches);
    let Some(row) = rows.get(state.log.details.changes.selected) else {
        return Err(i18n.t("menu.log_change.need_file").into());
    };
    let ChangeRowKind::File { change_idx } = row.kind else {
        return Err(i18n.t("menu.log_change.need_file").into());
    };
    let Some(change) = bundle.changes.get(change_idx) else {
        return Err(i18n.t("menu.log_change.need_file").into());
    };
    if bundle.key.mode != DetailsMode::Commits || bundle.key.oids.len() != 1 {
        return Err(i18n.t("menu.log_change.need_single_commit").into());
    }
    if change.kind == FileStatusKind::Deleted {
        return Err(i18n.t("menu.log_change.cannot_annotate_deleted").into());
    }
    Ok(vec![Action::OpenAnnotate {
        path: change.path.clone(),
        rev: Some(bundle.key.oids[0]),
    }])
}

pub fn annotate_stage_entry(state: &AppState) -> CommandResult {
    if state.focused_pane == PaneId::CommitDiffPreview {
        let path = state
            .stage_workbench
            .current_diff_key()
            .map(|k| k.path.as_str());
        return annotate_stage_diff(state, path);
    }
    annotate_stage_entry_at(state, state.stage_workbench.selected_idx())
}

pub fn annotate_stage_entry_at(state: &AppState, idx: usize) -> CommandResult {
    let i18n = &state.i18n;
    let group = state
        .stage_workbench
        .rows()
        .get(idx)
        .map(|r| r.group_kind());
    let Some(entry) = state.stage_entry_at(idx) else {
        return Err(i18n.t("menu.stage.select_file").into());
    };
    let untracked = group == Some(jbgit_core::StageNodeKind::Untracked)
        || entry.kind == FileStatusKind::Untracked;
    let ignored = group == Some(jbgit_core::StageNodeKind::Ignored)
        || entry.kind == FileStatusKind::Ignored;
    if untracked || ignored {
        return Err(i18n.t("menu.stage.only_tracked_history").into());
    }
    Ok(vec![Action::OpenAnnotate {
        path: entry.path,
        rev: None,
    }])
}

pub fn annotate_stage_diff(state: &AppState, path: Option<&str>) -> CommandResult {
    let Some(path) = path else {
        return Err(state.i18n.t("menu.stage.no_file_selected").into());
    };
    Ok(vec![Action::OpenAnnotate {
        path: path.to_string(),
        rev: None,
    }])
}

/// Commands inside `ModalLayer::Annotate` shared by keyboard routing and the
/// Annotate context menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnnotateCommand {
    SelectionHistory,
    PreviousRev,
    ToggleDiff,
    IgnoreWhitespace,
    DetectMoves,
    LocateInLog,
    CopyHash,
}

impl AnnotateCommand {
    pub const fn shortcut(self) -> &'static str {
        match self {
            Self::SelectionHistory => "H",
            Self::PreviousRev => "p",
            Self::ToggleDiff => "d",
            Self::IgnoreWhitespace => "w",
            Self::DetectMoves => "m",
            Self::LocateInLog => "Enter",
            Self::CopyHash => "y",
        }
    }

    pub const fn action(self) -> Action {
        match self {
            Self::SelectionHistory => Action::AnnotateShowSelectionHistory,
            Self::PreviousRev => Action::AnnotatePreviousRev,
            Self::ToggleDiff => Action::AnnotateToggleDiff,
            Self::IgnoreWhitespace => Action::AnnotateToggleWhitespace,
            Self::DetectMoves => Action::AnnotateToggleMoves,
            Self::LocateInLog => Action::AnnotateJumpToLog,
            Self::CopyHash => Action::AnnotateCopyHash,
        }
    }

    pub fn from_key(code: crossterm::event::KeyCode) -> Option<Self> {
        use crossterm::event::KeyCode;
        match code {
            KeyCode::Char('H') => Some(Self::SelectionHistory),
            KeyCode::Char('p') => Some(Self::PreviousRev),
            KeyCode::Char('d') => Some(Self::ToggleDiff),
            KeyCode::Char('w') => Some(Self::IgnoreWhitespace),
            KeyCode::Char('m') => Some(Self::DetectMoves),
            KeyCode::Enter => Some(Self::LocateInLog),
            KeyCode::Char('y') => Some(Self::CopyHash),
            _ => None,
        }
    }
}

pub fn evaluate_annotate_command(state: &AppState, cmd: AnnotateCommand) -> CommandResult {
    let i18n = &state.i18n;
    let Some(ann) = state.modal_stack.annotate() else {
        return Err(String::new());
    };
    match cmd {
        AnnotateCommand::SelectionHistory => {
            if ann.selected_line_range().is_none() {
                return Err(i18n.t("menu.stage.no_selection_range").into());
            }
        }
        AnnotateCommand::PreviousRev => {
            let has_prev = ann.selected_line().is_some_and(|l| {
                if l.oid.is_zero() {
                    ann.key.rev.is_none()
                        && state
                            .repo_snapshot
                            .as_ref()
                            .and_then(|s| s.head_oid)
                            .is_some()
                } else {
                    ann.selected_commit()
                        .and_then(|c| c.previous.as_ref())
                        .is_some()
                }
            });
            if !has_prev {
                return Err(i18n.t("toast.annotate_no_previous").into());
            }
        }
        AnnotateCommand::LocateInLog | AnnotateCommand::CopyHash => {
            if ann.selected_line().is_none_or(|l| l.oid.is_zero()) {
                return Err(i18n.t("toast.annotate_uncommitted").into());
            }
        }
        AnnotateCommand::ToggleDiff
        | AnnotateCommand::IgnoreWhitespace
        | AnnotateCommand::DetectMoves => {}
    }
    Ok(vec![cmd.action()])
}
