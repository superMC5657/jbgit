//! Key, mouse, and context-menu routing for the `[1 Commit]` staging workbench.

use std::time::Duration;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use jbgit_core::PaneId;
use jbgit_engine::StageDiffSide;

use crate::app::{Action, AppState, ContextMenuItem, DiffScope, EditorMoveKind, PopupKind};
use crate::hit_test::{
    CommitOptionKind, HitTarget, StageDiffToolbarButton, StageToolbarButton,
};
use crate::keymap::{KeyAction, catalog};

const BRACKET_CHORD_WINDOW: Duration = Duration::from_millis(500);

pub(crate) fn route_commit_priority_key(
    key: &KeyEvent,
    state: &AppState,
) -> Option<Vec<Action>> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);

    if state.focused_pane == PaneId::CommitMessageEditor {
        if let Some(actions) = crate::keymap::route_editor_dynamic_key(key, state) {
            return Some(actions);
        }
        if key.code == KeyCode::Char('e') && ctrl && !alt {
            return Some(vec![Action::OpenExternalEditorMessage]);
        }
        if state.stage_workbench.editing_author() {
            return Some(match key.code {
                KeyCode::Esc | KeyCode::Enter => vec![Action::CommitAuthorDone],
                KeyCode::Backspace => vec![Action::CommitAuthorBackspace],
                KeyCode::Char(c) if !ctrl && !alt => vec![Action::CommitAuthorChar(c)],
                _ => Vec::new(),
            });
        }
        return Some(match key.code {
            KeyCode::Esc => vec![Action::FocusPane(PaneId::CommitChangesTree)],
            KeyCode::Enter => vec![Action::CommitEditorNewline],
            KeyCode::Backspace if alt => vec![Action::CommitEditorDeleteWord],
            KeyCode::Backspace => vec![Action::CommitEditorBackspace],
            KeyCode::Delete => vec![Action::CommitEditorDelete],
            KeyCode::Char('w') if ctrl => vec![Action::CommitEditorDeleteWord],
            KeyCode::Char('z') if ctrl && shift => vec![Action::CommitEditorRedo],
            KeyCode::Char('z') if ctrl => vec![Action::CommitEditorUndo],
            KeyCode::Char('y') if ctrl => vec![Action::CommitEditorRedo],
            KeyCode::Left => vec![Action::CommitEditorMove(EditorMoveKind::Left)],
            KeyCode::Right => vec![Action::CommitEditorMove(EditorMoveKind::Right)],
            KeyCode::Up => vec![Action::CommitEditorMove(EditorMoveKind::Up)],
            KeyCode::Down => vec![Action::CommitEditorMove(EditorMoveKind::Down)],
            KeyCode::Home => vec![Action::CommitEditorMove(EditorMoveKind::Home)],
            KeyCode::End => vec![Action::CommitEditorMove(EditorMoveKind::End)],
            KeyCode::Char(c) if !ctrl && !alt => vec![Action::CommitEditorChar(c)],
            _ => Vec::new(),
        });
    }

    if state.focused_pane == PaneId::CommitDiffPreview {
        // Support `]c` / `[c` hunk jump sequence without accidentally triggering `c` shortcuts.
        if key.code == KeyCode::Char('c')
            && !ctrl
            && !alt
            && state
                .stage_workbench
                .bracket_jump_at()
                .is_some_and(|t| t.elapsed() < BRACKET_CHORD_WINDOW)
        {
            return Some(vec![Action::StageClearBracketJump]);
        }
    }

    None
}

pub(crate) fn route_commit_diff_fallback_key(key: &KeyEvent, state: &AppState) -> Vec<Action> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let side = state
        .stage_workbench
        .current_diff_key()
        .map(|k| k.side);
    let is_amend_ref = side == Some(StageDiffSide::AmendCommit);

    match key.code {
        KeyCode::PageDown => vec![Action::StageDiffMoveCursor(10)],
        KeyCode::PageUp => vec![Action::StageDiffMoveCursor(-10)],
        KeyCode::Char('s' | '+') if !ctrl && !alt && !is_amend_ref => {
            vec![if side == Some(StageDiffSide::Staged) {
                Action::StageDiffUnstageSelection
            } else {
                Action::StageDiffStageSelection
            }]
        }
        KeyCode::Char('u' | '-') if !ctrl && !alt && !is_amend_ref => {
            vec![Action::StageDiffUnstageSelection]
        }
        KeyCode::Esc => {
            if state.stage_workbench.visual_anchor_row().is_some() {
                vec![Action::StageDiffToggleVisual]
            } else {
                vec![Action::FocusPane(PaneId::CommitChangesTree)]
            }
        }
        _ => Vec::new(),
    }
}

pub(crate) fn route_commit_changes_tree_key(key: &KeyEvent, state: &AppState) -> Vec<Action> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    match key.code {
        KeyCode::PageDown => vec![Action::ScrollCommitStage(10)],
        KeyCode::PageUp => vec![Action::ScrollCommitStage(-10)],
        KeyCode::Right => vec![Action::StageExpand(true)],
        KeyCode::Left => vec![Action::StageExpand(false)],
        KeyCode::Enter => {
            let sel = state.stage_workbench.selected_idx();
            let row = state.stage_workbench.selected_row();
            match row {
                Some(r) if r.is_container() => {
                    vec![Action::StageToggleRow(sel)]
                }
                Some(_) => vec![Action::FocusPane(PaneId::CommitDiffPreview)],
                None => Vec::new(),
            }
        }
        KeyCode::Char('s' | '+') if !ctrl && !alt => vec![Action::StageActionSelected],
        KeyCode::Char('u' | '-') if !ctrl && !alt => vec![Action::UnstageActionSelected],
        KeyCode::Char('z') if ctrl && alt => vec![Action::DiscardPromptSelected],
        KeyCode::Char('m') if alt => vec![Action::CommitToggleAmend],
        KeyCode::Char('s') if alt => vec![Action::CommitToggleSignoff],
        _ => Vec::new(),
    }
}

pub(crate) fn route_stage_left_click(
    target: HitTarget,
    state: &AppState,
    col: u16,
    row: u16,
    ctrl: bool,
    shift: bool,
    is_double: bool,
) -> Result<Vec<Action>, HitTarget> {
    match target {
        HitTarget::ChangesTreeNode {
            node_id,
            on_hover_action_icon,
            ..
        } => {
            let idx = node_id.0 as usize;
            if on_hover_action_icon {
                let group_kind = state
                    .stage_workbench
                    .rows()
                    .get(idx)
                    .map(|r| r.group_kind());
                let mut actions = vec![
                    Action::FocusPane(PaneId::CommitChangesTree),
                    Action::SelectCommitStageRow(idx),
                ];
                if let Some(gk) = group_kind {
                    if gk.can_stage() {
                        actions.push(Action::StageActionSelected);
                    } else if gk.can_unstage() {
                        actions.push(Action::UnstageActionSelected);
                    }
                }
                return Ok(actions);
            }
            if ctrl {
                return Ok(vec![
                    Action::FocusPane(PaneId::CommitChangesTree),
                    Action::StageToggleMulti(idx),
                ]);
            }
            let mut actions = vec![
                Action::FocusPane(PaneId::CommitChangesTree),
                Action::SelectCommitStageRow(idx),
            ];
            if is_double
                && let Some(r) = state.stage_workbench.rows().get(idx)
            {
                if r.is_container() {
                    actions.push(Action::StageToggleRow(idx));
                } else {
                    actions.push(Action::FocusPane(PaneId::CommitDiffPreview));
                }
            }
            Ok(actions)
        }
        HitTarget::StageToolbar(btn) => Ok(match btn {
            StageToolbarButton::Refresh => vec![Action::RefreshAll],
            StageToolbarButton::StageAllTracked => vec![Action::StageAllTracked],
            StageToolbarButton::Stash => vec![Action::StashAll],
            StageToolbarButton::GroupByDir => vec![Action::StageToggleGroupByDir],
            StageToolbarButton::ToggleDiff => vec![Action::StageToggleDiffCollapsed],
            StageToolbarButton::Options => vec![Action::OpenContextMenu {
                anchor: (col, row.saturating_add(1)),
                items: stage_options_menu_items(state),
                depth: 0,
            }],
        }),
        HitTarget::StageResolveConflicts => Ok(vec![Action::ResolveConflictsNotice]),
        HitTarget::StageDiffToolbar(btn) => Ok(match btn {
            StageDiffToolbarButton::SwitchSide => vec![Action::StageDiffSwitchSide],
            StageDiffToolbarButton::SideBySide => {
                vec![Action::DiffToggleSideBySide(DiffScope::Stage)]
            }
            StageDiffToolbarButton::Context => vec![Action::DiffCycleContext(DiffScope::Stage)],
            StageDiffToolbarButton::PrevHunk => vec![Action::DiffJumpHunk(DiffScope::Stage, -1)],
            StageDiffToolbarButton::NextHunk => vec![Action::DiffJumpHunk(DiffScope::Stage, 1)],
            StageDiffToolbarButton::Close => vec![Action::StageToggleDiffCollapsed],
        }),
        HitTarget::DiffHunkAction { hunk_idx, action } => Ok(vec![
            Action::FocusPane(PaneId::CommitDiffPreview),
            Action::ApplyStageDiffHunk(hunk_idx, action),
        ]),
        HitTarget::StageDiffGutter {
            row_idx,
            staged_range_idx,
        } => Ok(vec![
            Action::StageDiffClearVisualAnchor,
            Action::FocusPane(PaneId::CommitDiffPreview),
            Action::OpenThreeWayInspector {
                row_idx: Some(row_idx),
                range_idx: staged_range_idx,
            },
        ]),
        HitTarget::StageDiffLine { row_idx } => Ok(vec![
            Action::StageDiffSelectLine {
                row_idx,
                extend: shift,
            },
            Action::FocusPane(PaneId::CommitDiffPreview),
        ]),
        HitTarget::CommitEditorArea => Ok(vec![Action::CommitEditorFocusBody]),
        HitTarget::CommitOption(kind) => Ok(match kind {
            CommitOptionKind::Amend => vec![Action::CommitToggleAmend],
            CommitOptionKind::Signoff => vec![Action::CommitToggleSignoff],
            CommitOptionKind::RunHooks => vec![Action::CommitToggleRunHooks],
            CommitOptionKind::AuthorToggle => vec![Action::CommitToggleAuthorInput],
            CommitOptionKind::AuthorInput => vec![Action::CommitFocusAuthorInput],
            CommitOptionKind::History => vec![Action::OpenPopup {
                kind: PopupKind::CommitHistory,
                initial_query: String::new(),
            }],
            CommitOptionKind::ExternalEditor => vec![Action::OpenExternalEditorMessage],
        }),
        HitTarget::CommitButton { and_push } => Ok(vec![Action::SubmitCommit { and_push }]),
        HitTarget::UndoCommitButton => Ok(vec![Action::UndoLastCommit]),
        other => Err(other),
    }
}

pub(crate) fn stage_tree_context_menu_items(
    state: &AppState,
    idx: usize,
) -> Vec<ContextMenuItem> {
    let mut items = crate::menus::stage_tree_menu_items(state, idx);
    let refresh_sc =
        catalog::shortcut_for_action(state, &[crate::config::KeymapScope::Global], &KeyAction::Refresh);
    items.push(ContextMenuItem::separator());
    items.push(ContextMenuItem::submenu(
        state.i18n.t("menu.stage.view_options"),
        Some('o'),
        stage_options_menu_items(state),
    ));
    items.push(ContextMenuItem::run(
        state.i18n.t("menu.stage.refresh_status"),
        Some('r'),
        refresh_sc.as_deref(),
        vec![Action::RefreshAll],
    ));
    items
}

pub(crate) fn stage_options_menu_items(state: &AppState) -> Vec<ContextMenuItem> {
    let check = |on: bool| if on { "✓" } else { " " };
    let i18n = &state.i18n;
    let group_sc = catalog::shortcut_for_action(
        state,
        &[crate::config::KeymapScope::Commit],
        &KeyAction::ToggleGroup,
    );
    vec![
        ContextMenuItem::run(
            format!(
                "{} {}",
                check(state.stage_workbench.tree_opts().group_by_dir),
                i18n.t("menu.stage.opt_group_by_dir")
            ),
            Some('g'),
            group_sc.as_deref(),
            vec![Action::StageToggleGroupByDir],
        ),
        ContextMenuItem::run(
            format!(
                "{} {}",
                check(state.stage_workbench.tree_opts().show_ignored),
                i18n.t("menu.stage.opt_show_ignored")
            ),
            Some('i'),
            None,
            vec![Action::StageToggleShowIgnored],
        ),
        ContextMenuItem::run(
            i18n.t("menu.stage.expand_all"),
            Some('e'),
            None,
            vec![Action::StageExpandAll(true)],
        ),
        ContextMenuItem::run(
            i18n.t("menu.stage.collapse_all"),
            Some('c'),
            None,
            vec![Action::StageExpandAll(false)],
        ),
        ContextMenuItem::run(
            i18n.t("menu.stage.stage_all_tracked"),
            Some('a'),
            None,
            vec![Action::StageAllTracked],
        ),
    ]
}
