//! Key, mouse, and context-menu routing for the `[1 Commit]` staging workbench.

use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use jgit_core::PaneId;
use jgit_engine::StageDiffSide;

use crate::app::{
    Action, AppState, ContextMenuAction, ContextMenuItem, EditorMoveKind, PopupKind,
};
use crate::hit_test::{
    CommitOptionKind, HitTarget, StageDiffToolbarButton, StageToolbarButton,
};

const BRACKET_CHORD_WINDOW: Duration = Duration::from_millis(500);

pub(crate) fn route_commit_priority_key(
    key: &KeyEvent,
    state: &mut AppState,
) -> Option<Vec<Action>> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);

    // Tab-wide commit shortcuts
    match key.code {
        KeyCode::Char('m') if alt => return Some(vec![Action::CommitToggleAmend]),
        KeyCode::Char('s') if alt => return Some(vec![Action::CommitToggleSignoff]),
        KeyCode::Char('k') if ctrl && alt => {
            return Some(vec![Action::SubmitCommit { and_push: true }]);
        }
        KeyCode::Enter if ctrl => {
            return Some(vec![Action::SubmitCommit { and_push: false }]);
        }
        KeyCode::Char('m') if ctrl => {
            return Some(vec![Action::OpenPopup {
                kind: PopupKind::CommitHistory,
                initial_query: String::new(),
            }]);
        }
        _ => {}
    }

    if state.focused_pane == PaneId::CommitMessageEditor {
        match key.code {
            KeyCode::Tab => return Some(vec![Action::CycleFocus { reverse: false }]),
            KeyCode::BackTab => return Some(vec![Action::CycleFocus { reverse: true }]),
            KeyCode::Char('e') if ctrl => return Some(vec![Action::OpenExternalEditorMessage]),
            _ => {}
        }
        if state.stage_workbench.editing_author {
            return Some(match key.code {
                KeyCode::Esc | KeyCode::Enter => vec![Action::CommitAuthorDone],
                KeyCode::Backspace => vec![Action::CommitAuthorBackspace],
                KeyCode::Char(c) if !ctrl && !alt => vec![Action::CommitAuthorChar(c)],
                _ => Vec::new(),
            });
        }
        return Some(match key.code {
            KeyCode::Esc => vec![Action::FocusPane(PaneId::CommitChangesTree)],
            KeyCode::Enter if alt => vec![Action::SubmitCommit { and_push: false }],
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
        let side = state
            .stage_workbench
            .current_diff_key
            .as_ref()
            .map(|k| k.side);
        let is_amend_ref = side == Some(StageDiffSide::AmendCommit);

        // Support `]c` / `[c` hunk jump sequence without accidentally cycling diff context on `c`.
        if key.code == KeyCode::Char('c')
            && !ctrl
            && !alt
            && state
                .stage_workbench
                .bracket_jump_at
                .take()
                .is_some_and(|t| t.elapsed() < BRACKET_CHORD_WINDOW)
        {
            return Some(Vec::new());
        }
        if !matches!(key.code, KeyCode::Char(']') | KeyCode::Char('[')) {
            state.stage_workbench.bracket_jump_at = None;
        }

        match key.code {
            KeyCode::Down | KeyCode::Char('j') if !ctrl && !alt => {
                return Some(vec![Action::StageDiffMoveCursor(1)]);
            }
            KeyCode::Up | KeyCode::Char('k') if !ctrl && !alt => {
                return Some(vec![Action::StageDiffMoveCursor(-1)]);
            }
            KeyCode::PageDown => return Some(vec![Action::StageDiffMoveCursor(10)]),
            KeyCode::PageUp => return Some(vec![Action::StageDiffMoveCursor(-10)]),
            KeyCode::Char(']') if !ctrl && !alt => {
                state.stage_workbench.bracket_jump_at = Some(Instant::now());
                return Some(vec![Action::StageDiffJumpHunk(1)]);
            }
            KeyCode::Char('[') if !ctrl && !alt => {
                state.stage_workbench.bracket_jump_at = Some(Instant::now());
                return Some(vec![Action::StageDiffJumpHunk(-1)]);
            }
            KeyCode::Char('n') if !ctrl && !alt => {
                return Some(vec![Action::StageDiffJumpHunk(1)]);
            }
            KeyCode::Char('p') if !ctrl && !alt => {
                return Some(vec![Action::StageDiffJumpHunk(-1)]);
            }
            KeyCode::Char('v') if !ctrl && !alt && !is_amend_ref => {
                return Some(vec![Action::StageDiffToggleVisual]);
            }
            KeyCode::Char('s') | KeyCode::Char(' ') if !ctrl && !alt && !is_amend_ref => {
                return Some(vec![if side == Some(StageDiffSide::Staged) {
                    Action::StageDiffUnstageSelection
                } else {
                    Action::StageDiffStageSelection
                }]);
            }
            KeyCode::Char('u') if !ctrl && !alt && !is_amend_ref => {
                return Some(vec![Action::StageDiffUnstageSelection]);
            }
            KeyCode::Char('r') if !ctrl && !alt && !is_amend_ref => {
                return Some(vec![Action::StageDiffRollbackSelection]);
            }
            KeyCode::Char('z') if ctrl && !is_amend_ref => {
                return Some(vec![Action::StageDiffUndoRollback]);
            }
            KeyCode::Char('i') if !ctrl && !alt && !is_amend_ref => {
                return Some(vec![Action::OpenThreeWayInspector {
                    row_idx: None,
                    range_idx: None,
                }]);
            }
            KeyCode::Char('x') if !ctrl && !alt && !is_amend_ref => {
                return Some(vec![Action::StageDiffSwitchSide]);
            }
            KeyCode::Char('c') if !ctrl && !alt => {
                return Some(vec![Action::StageDiffCycleContext]);
            }
            KeyCode::Char('S') if !ctrl && !alt => {
                return Some(vec![Action::StageDiffToggleSideBySide]);
            }
            KeyCode::Char('e') if !ctrl && !alt => {
                return Some(vec![Action::OpenExternalEditorFile(None)]);
            }
            KeyCode::Esc => {
                if state.stage_workbench.visual_anchor_row.is_some() {
                    return Some(vec![Action::StageDiffToggleVisual]);
                }
                return Some(vec![Action::FocusPane(PaneId::CommitChangesTree)]);
            }
            _ => {}
        }
    }

    None
}

pub(crate) fn route_commit_changes_tree_key(key: &KeyEvent, state: &AppState) -> Vec<Action> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    match key.code {
        KeyCode::Down | KeyCode::Char('j') => vec![Action::ScrollCommitStage(1)],
        KeyCode::Up | KeyCode::Char('k') => vec![Action::ScrollCommitStage(-1)],
        KeyCode::PageDown => vec![Action::ScrollCommitStage(10)],
        KeyCode::PageUp => vec![Action::ScrollCommitStage(-10)],
        KeyCode::Home | KeyCode::Char('g') => vec![Action::SelectCommitStageRow(0)],
        KeyCode::End | KeyCode::Char('G') => vec![Action::SelectCommitStageRow(usize::MAX)],
        KeyCode::Right => vec![Action::StageExpand(true)],
        KeyCode::Left => vec![Action::StageExpand(false)],
        KeyCode::Enter => {
            let row = state
                .stage_workbench
                .rows
                .get(state.commit_stage_selected_idx);
            match row {
                Some(r) if r.is_container() => {
                    vec![Action::StageToggleRow(state.commit_stage_selected_idx)]
                }
                Some(_) => vec![Action::FocusPane(PaneId::CommitDiffPreview)],
                None => Vec::new(),
            }
        }
        KeyCode::Char(' ') => {
            let row = state
                .stage_workbench
                .rows
                .get(state.commit_stage_selected_idx);
            match row {
                Some(r) if r.group_kind().can_stage() => vec![Action::StageActionSelected],
                Some(r) if r.group_kind().can_unstage() => vec![Action::UnstageActionSelected],
                Some(r) if r.is_container() => {
                    vec![Action::StageToggleRow(state.commit_stage_selected_idx)]
                }
                _ => Vec::new(),
            }
        }
        KeyCode::Char('s') | KeyCode::Char('+') => vec![Action::StageActionSelected],
        KeyCode::Char('u') | KeyCode::Char('-') => vec![Action::UnstageActionSelected],
        KeyCode::Char('d') if !ctrl => vec![Action::DiscardPromptSelected],
        KeyCode::Char('z') if ctrl && alt => vec![Action::DiscardPromptSelected],
        KeyCode::Char('a') if ctrl => vec![Action::StageSelectAll],
        KeyCode::Char('v') => vec![Action::StageToggleGroupByDir],
        KeyCode::Char('e') => vec![Action::OpenExternalEditorFile(None)],
        _ => Vec::new(),
    }
}

pub(crate) fn route_stage_left_click(
    target: HitTarget,
    state: &mut AppState,
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
                    .rows
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
                && let Some(r) = state.stage_workbench.rows.get(idx)
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
            StageDiffToolbarButton::SideBySide => vec![Action::StageDiffToggleSideBySide],
            StageDiffToolbarButton::Context => vec![Action::StageDiffCycleContext],
            StageDiffToolbarButton::PrevHunk => vec![Action::StageDiffJumpHunk(-1)],
            StageDiffToolbarButton::NextHunk => vec![Action::StageDiffJumpHunk(1)],
            StageDiffToolbarButton::Close => vec![Action::StageToggleDiffCollapsed],
        }),
        HitTarget::DiffHunkAction { hunk_idx, action } => Ok(vec![
            Action::FocusPane(PaneId::CommitDiffPreview),
            Action::ApplyStageDiffHunk(hunk_idx, action),
        ]),
        HitTarget::StageDiffGutter {
            row_idx,
            staged_range_idx,
        } => {
            state.stage_workbench.visual_anchor_row = None;
            Ok(vec![
                Action::FocusPane(PaneId::CommitDiffPreview),
                Action::OpenThreeWayInspector {
                    row_idx: Some(row_idx),
                    range_idx: staged_range_idx,
                },
            ])
        }
        HitTarget::StageDiffLine { row_idx } => {
            if shift {
                if state.stage_workbench.visual_anchor_row.is_none() {
                    state.stage_workbench.visual_anchor_row =
                        Some(state.stage_workbench.diff_cursor_row);
                }
            } else {
                state.stage_workbench.visual_anchor_row = None;
            }
            state.stage_workbench.diff_cursor_row = row_idx;
            Ok(vec![Action::FocusPane(PaneId::CommitDiffPreview)])
        }
        HitTarget::CommitEditorArea => {
            state.stage_workbench.editing_author = false;
            Ok(vec![Action::FocusPane(PaneId::CommitMessageEditor)])
        }
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
    actions: &mut Vec<Action>,
) -> Vec<ContextMenuItem> {
    if state.commit_stage_selected_idx != idx {
        actions.push(Action::FocusPane(PaneId::CommitChangesTree));
        actions.push(Action::SelectCommitStageRow(idx));
    }
    let row = state.stage_workbench.rows.get(idx);
    let group_kind = row.map(|r| r.group_kind());
    let file_path = state
        .stage_entry_at(idx)
        .map(|e| e.path.clone())
        .unwrap_or_default();
    let mut items = Vec::new();
    if let Some(gk) = group_kind {
        if gk.can_stage() {
            items.push(ContextMenuItem::action(
                "Stage",
                Some('s'),
                Some("Space"),
                ContextMenuAction::StageSelected,
            ));
            items.push(ContextMenuItem::action(
                "Rollback / Discard…",
                Some('d'),
                Some("d"),
                ContextMenuAction::DiscardSelected,
            ));
        } else if gk.can_unstage() {
            items.push(ContextMenuItem::action(
                "Unstage",
                Some('u'),
                Some("Space"),
                ContextMenuAction::UnstageSelected,
            ));
        }
    }
    if !file_path.is_empty() {
        items.push(ContextMenuItem::action(
            format!("Open {file_path} in $EDITOR"),
            Some('e'),
            Some("e"),
            ContextMenuAction::OpenExternalEditorFile(file_path.clone()),
        ));
        items.push(ContextMenuItem::action(
            format!("Diff {file_path}"),
            Some('i'),
            None,
            ContextMenuAction::RunReadOnlyProbe(vec![
                "diff".into(),
                "--".into(),
                file_path,
            ]),
        ));
    }
    items.push(ContextMenuItem::submenu(
        "Stage View Options",
        Some('o'),
        stage_options_menu_items(state),
    ));
    items.push(ContextMenuItem::action(
        "Refresh Stage Status",
        Some('r'),
        Some("r"),
        ContextMenuAction::RefreshRepo,
    ));
    items
}

pub(crate) fn stage_options_menu_items(state: &AppState) -> Vec<ContextMenuItem> {
    let check = |on: bool| if on { "✓" } else { " " };
    vec![
        ContextMenuItem::action(
            format!(
                "{} Group by Directory",
                check(state.stage_workbench.tree_opts.group_by_dir)
            ),
            Some('g'),
            Some("v"),
            ContextMenuAction::StageToggleGroupByDir,
        ),
        ContextMenuItem::action(
            format!(
                "{} Show Ignored Files",
                check(state.stage_workbench.tree_opts.show_ignored)
            ),
            Some('i'),
            None,
            ContextMenuAction::StageToggleShowIgnored,
        ),
        ContextMenuItem::action(
            "Expand All",
            Some('e'),
            None,
            ContextMenuAction::StageExpandAll(true),
        ),
        ContextMenuItem::action(
            "Collapse All",
            Some('c'),
            None,
            ContextMenuAction::StageExpandAll(false),
        ),
        ContextMenuItem::action(
            "Stage All Tracked",
            Some('a'),
            None,
            ContextMenuAction::StageAllTracked,
        ),
    ]
}
