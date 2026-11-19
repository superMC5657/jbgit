use jbgit_core::{GitOp, PaneId, StageNodeKind};
use jbgit_engine::StageDiffSide;

use crate::app::{Action, AppState, ContextMenuAction, ContextMenuItem};
use crate::dialogs::{ChoiceDialog, Dialog};
use crate::hit_test::DiffHunkActionKind;

use super::{dialog, diff_patch_args, op, run};

/// `Git.Stage.Tree.Menu` for the stage tree row `idx` (plus selection).
pub fn stage_tree_menu_items(state: &AppState, idx: usize) -> Vec<ContextMenuItem> {
    let i18n = &state.i18n;
    let row = state.stage_workbench.rows.get(idx);
    let group = row.map(|r| r.group_kind());
    let targets = state
        .stage_status
        .as_ref()
        .map(|s| {
            state
                .stage_workbench
                .collect_targets_for_action(idx, s, |_| true)
        })
        .unwrap_or_default();
    let mut paths: Vec<String> = targets.iter().map(|t| t.path.clone()).collect();
    paths.dedup();
    let entry = state.stage_entry_at(idx);
    // Untracked files below the collapse threshold live in the Unstaged group.
    let untracked = group == Some(StageNodeKind::Untracked)
        || entry
            .as_ref()
            .is_some_and(|e| e.kind == jbgit_core::FileStatusKind::Untracked)
        || targets
            .iter()
            .any(|t| t.group_kind == StageNodeKind::Untracked);
    let file_path = entry.map(|e| e.path);
    let has_paths = !paths.is_empty();
    let mut items = Vec::new();

    if group == Some(StageNodeKind::Conflicted) {
        if let Some(p) = &file_path {
            items.push(
                run(
                    i18n.t("menu.stage.resolve_conflicts"),
                    Some('r'),
                    None,
                    vec![Action::OpenConflictResolver(p.clone())],
                )
                .icon("⚠"),
            );
            items.push(run(
                i18n.t("menu.stage.open_mergetool"),
                Some('m'),
                None,
                vec![Action::OpenExternalMergetool(p.clone())],
            ));
        }
        items.push(run(
            i18n.t("menu.stage.accept_yours"),
            Some('y'),
            None,
            op(GitOp::AcceptSide {
                paths: paths.clone(),
                ours: true,
            }),
        ));
        items.push(run(
            i18n.t("menu.stage.accept_theirs"),
            Some('t'),
            None,
            op(GitOp::AcceptSide {
                paths: paths.clone(),
                ours: false,
            }),
        ));
        items.push(ContextMenuItem::separator());
    }
    if let Some(g) = group {
        if g.can_stage() {
            items.push(
                ContextMenuItem::action(
                    i18n.t("menu.stage.stage"),
                    Some('s'),
                    Some("+"),
                    ContextMenuAction::StageSelected,
                )
                .icon("+"),
            );
            items.push(
                ContextMenuItem::action(
                    i18n.t("menu.stage.discard_changes"),
                    Some('d'),
                    Some("d"),
                    ContextMenuAction::DiscardSelected,
                )
                .icon("↶"),
            );
        }
        if untracked {
            items.push(run(
                i18n.t("menu.stage.intent_to_add"),
                Some('n'),
                None,
                op(GitOp::IntentToAdd {
                    paths: paths.clone(),
                }),
            ));
        }
        if g.can_unstage() {
            items.push(
                ContextMenuItem::action(
                    i18n.t("menu.stage.unstage"),
                    Some('u'),
                    Some("-"),
                    ContextMenuAction::UnstageSelected,
                )
                .icon("−"),
            );
        }
    }
    items.push(
        run(
            i18n.t("menu.stage.stash_changes"),
            Some('h'),
            None,
            op(GitOp::StashPush {
                message: Some(format!("Selected files ({})", paths.len())),
                include_untracked: untracked,
                paths: paths.clone(),
            }),
        )
        .enabled_if(has_paths, i18n.t("common.select_files"))
        .enabled_if(
            group != Some(StageNodeKind::Conflicted),
            i18n.t("menu.stage.resolve_before_stash"),
        ),
    );
    items.push(ContextMenuItem::separator());
    let ignore_patterns: Vec<String> = paths.iter().map(|p| format!("/{p}")).collect();
    items.push(
        run(
            i18n.t("menu.stage.add_to_gitignore"),
            Some('i'),
            None,
            op(GitOp::AddToIgnore {
                patterns: ignore_patterns.clone(),
                exclude_file: false,
            }),
        )
        .enabled_if(untracked, i18n.t("menu.stage.only_untracked_ignore")),
    );
    items.push(
        run(
            i18n.t("menu.stage.add_to_exclude"),
            Some('x'),
            None,
            op(GitOp::AddToIgnore {
                patterns: ignore_patterns,
                exclude_file: true,
            }),
        )
        .enabled_if(untracked, i18n.t("menu.stage.only_untracked_ignore")),
    );
    items.push(
        run(
            i18n.t("menu.stage.show_diff"),
            Some('f'),
            Some("Enter"),
            vec![Action::FocusPane(PaneId::CommitDiffPreview)],
        )
        .enabled_if(file_path.is_some(), i18n.t("menu.stage.select_file")),
    );
    items.push(
        run(
            i18n.t("menu.stage.three_side_diff"),
            Some('3'),
            None,
            vec![Action::OpenThreeWayInspector {
                row_idx: None,
                range_idx: None,
            }],
        )
        .enabled_if(file_path.is_some(), i18n.t("menu.stage.select_file")),
    );
    let patch_args = diff_patch_args(group == Some(StageNodeKind::Staged), &paths);
    items.push(
        run(
            i18n.t("menu.stage.create_patch_clipboard"),
            Some('c'),
            None,
            op(GitOp::PatchToClipboard { args: patch_args }),
        )
        .enabled_if(has_paths, i18n.t("common.select_files")),
    );
    items.push(ContextMenuItem::separator());
    items.push(
        run(
            i18n.t("menu.stage.copy_relative_path"),
            Some('a'),
            None,
            vec![Action::CopyText(paths.join("\n"))],
        )
        .enabled_if(has_paths, i18n.t("common.select_files")),
    );
    if let Some(p) = &file_path {
        items.push(ContextMenuItem::action(
            i18n.t("menu.stage.open_external_editor"),
            Some('e'),
            Some("e"),
            ContextMenuAction::OpenExternalEditorFile(p.clone()),
        ));
    }
    let count_s = paths.len().to_string();
    let mut body = vec![i18n.t_fmt("menu.stage.delete_files_prompt", &[("count", &count_s)])];
    body.extend(paths.iter().take(5).map(|p| format!("  • {p}")));
    items.push(
        run(
            i18n.t("menu.stage.delete_files"),
            Some('l'),
            None,
            dialog(Dialog::Choice(ChoiceDialog::confirm(
                i18n.t("menu.stage.delete_files_title"),
                body,
                i18n.t("menu.stage.delete_btn"),
                i18n.cancel(),
                op(GitOp::DeleteFiles {
                    paths: paths.clone(),
                }),
            ))),
        )
        .icon("✕")
        .enabled_if(has_paths, i18n.t("common.select_files"))
        .enabled_if(
            !matches!(group, Some(StageNodeKind::AmendCommit)),
            i18n.t("menu.stage.cannot_delete_amend"),
        ),
    );
    items
}

fn diff_side_and_path(state: &AppState) -> (Option<StageDiffSide>, Option<String>) {
    let key = state.stage_workbench.current_diff_key.as_ref();
    (key.map(|k| k.side), key.map(|k| k.path.clone()))
}

fn stage_diff_common_items(
    state: &AppState,
    side: Option<StageDiffSide>,
    path: Option<String>,
) -> Vec<ContextMenuItem> {
    let i18n = &state.i18n;
    let staged = side == Some(StageDiffSide::Staged);
    let paths: Vec<String> = path.iter().cloned().collect();
    let patch = diff_patch_args(staged, &paths);
    vec![
        run(
            i18n.t("menu.stage.copy_patch"),
            Some('c'),
            None,
            op(GitOp::PatchToClipboard { args: patch }),
        )
        .enabled_if(path.is_some(), i18n.t("menu.stage.no_file_selected")),
        run(
            i18n.t("menu.stage.toggle_side_by_side"),
            Some('t'),
            Some("S"),
            vec![Action::StageDiffToggleSideBySide],
        ),
        run(
            i18n.t("menu.stage.inspect_three_way"),
            Some('i'),
            Some("i"),
            vec![Action::OpenThreeWayInspector {
                row_idx: None,
                range_idx: None,
            }],
        )
        .enabled_if(
            matches!(side, Some(StageDiffSide::Staged | StageDiffSide::Unstaged)),
            i18n.t("menu.stage.not_for_amend"),
        ),
        run(
            i18n.t("menu.stage.open_external_editor"),
            Some('e'),
            Some("e"),
            vec![Action::OpenExternalEditorFile(None)],
        ),
    ]
}

/// Right-click on the Commit tab's diff (hunk / line) view.
pub fn stage_diff_menu_items(state: &AppState) -> Vec<ContextMenuItem> {
    let i18n = &state.i18n;
    let (side, path) = diff_side_and_path(state);
    let editable = matches!(side, Some(StageDiffSide::Staged | StageDiffSide::Unstaged));
    let staged = side == Some(StageDiffSide::Staged);
    let mut items = vec![
        run(
            i18n.t("menu.stage.stage_selection"),
            Some('s'),
            Some("s"),
            vec![Action::StageDiffStageSelection],
        )
        .enabled_if(
            editable && !staged,
            if staged {
                i18n.t("menu.stage.already_staged")
            } else {
                i18n.t("menu.stage.readonly_stage")
            },
        ),
        run(
            i18n.t("menu.stage.unstage_selection"),
            Some('u'),
            Some("u"),
            vec![Action::StageDiffUnstageSelection],
        )
        .enabled_if(
            staged,
            if editable {
                i18n.t("menu.stage.not_staged")
            } else {
                i18n.t("menu.stage.readonly_unstage")
            },
        ),
        run(
            i18n.t("menu.stage.rollback_selection"),
            Some('r'),
            Some("r"),
            vec![Action::StageDiffRollbackSelection],
        )
        .enabled_if(editable && !staged, i18n.t("menu.stage.only_unstaged_rollback")),
        ContextMenuItem::separator(),
    ];
    items.extend(stage_diff_common_items(state, side, path));
    items
}

/// Right-click on a hunk action button: act on that hunk.
pub fn stage_hunk_menu_items(state: &AppState, hunk_idx: usize) -> Vec<ContextMenuItem> {
    let i18n = &state.i18n;
    let (side, path) = diff_side_and_path(state);
    let editable = matches!(side, Some(StageDiffSide::Staged | StageDiffSide::Unstaged));
    let staged = side == Some(StageDiffSide::Staged);
    let mut items = vec![
        run(
            i18n.t("menu.stage.stage_hunk"),
            Some('s'),
            None,
            vec![Action::ApplyStageDiffHunk(
                hunk_idx,
                DiffHunkActionKind::Stage,
            )],
        )
        .enabled_if(
            editable && !staged,
            if staged {
                i18n.t("menu.stage.hunk_already_staged")
            } else {
                i18n.t("menu.stage.readonly_stage")
            },
        ),
        run(
            i18n.t("menu.stage.unstage_hunk"),
            Some('u'),
            None,
            vec![Action::ApplyStageDiffHunk(
                hunk_idx,
                DiffHunkActionKind::Unstage,
            )],
        )
        .enabled_if(
            staged,
            if editable {
                i18n.t("menu.stage.hunk_not_staged")
            } else {
                i18n.t("menu.stage.readonly_unstage")
            },
        ),
        run(
            i18n.t("menu.stage.rollback_hunk"),
            Some('r'),
            None,
            vec![Action::ApplyStageDiffHunk(
                hunk_idx,
                DiffHunkActionKind::Rollback,
            )],
        )
        .enabled_if(
            editable && !staged,
            i18n.t("menu.stage.only_unstaged_hunk_rollback"),
        ),
        ContextMenuItem::separator(),
    ];
    items.extend(stage_diff_common_items(state, side, path));
    items
}
