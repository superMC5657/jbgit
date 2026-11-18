use jgit_core::{GitOp, PaneId, StageNodeKind};
use jgit_engine::StageDiffSide;

use crate::app::{Action, AppState, ContextMenuAction, ContextMenuItem};
use crate::dialogs::{ChoiceDialog, Dialog};
use crate::hit_test::DiffHunkActionKind;

use super::{dialog, diff_patch_args, op, run};

/// `Git.Stage.Tree.Menu` for the stage tree row `idx` (plus selection).
pub fn stage_tree_menu_items(state: &AppState, idx: usize) -> Vec<ContextMenuItem> {
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
            .is_some_and(|e| e.kind == jgit_core::FileStatusKind::Untracked)
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
                    "Resolve Conflicts… (3-Pane Merge)",
                    Some('r'),
                    None,
                    vec![Action::OpenConflictResolver(p.clone())],
                )
                .icon("⚠"),
            );
            items.push(run(
                "Open in git mergetool",
                Some('m'),
                None,
                vec![Action::OpenExternalMergetool(p.clone())],
            ));
        }
        items.push(run(
            "Accept Yours",
            Some('y'),
            None,
            op(GitOp::AcceptSide {
                paths: paths.clone(),
                ours: true,
            }),
        ));
        items.push(run(
            "Accept Theirs",
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
                    "Stage",
                    Some('s'),
                    Some("+"),
                    ContextMenuAction::StageSelected,
                )
                .icon("+"),
            );
            items.push(
                ContextMenuItem::action(
                    "Rollback / Discard Changes…",
                    Some('d'),
                    Some("d"),
                    ContextMenuAction::DiscardSelected,
                )
                .icon("↶"),
            );
        }
        if untracked {
            items.push(run(
                "Stage Without Content (git add -N)",
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
                    "Unstage",
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
            "Stash Selected Files…",
            Some('h'),
            None,
            op(GitOp::StashPush {
                message: Some(format!("Selected files ({})", paths.len())),
                include_untracked: untracked,
                paths: paths.clone(),
            }),
        )
        .enabled_if(has_paths, "Select files to stash")
        .enabled_if(
            group != Some(StageNodeKind::Conflicted),
            "Resolve conflicts before stashing",
        ),
    );
    items.push(ContextMenuItem::separator());
    let ignore_patterns: Vec<String> = paths.iter().map(|p| format!("/{p}")).collect();
    items.push(
        run(
            "Add to .gitignore",
            Some('i'),
            None,
            op(GitOp::AddToIgnore {
                patterns: ignore_patterns.clone(),
                exclude_file: false,
            }),
        )
        .enabled_if(untracked, "Only untracked files can be ignored"),
    );
    items.push(
        run(
            "Add to .git/info/exclude",
            Some('x'),
            None,
            op(GitOp::AddToIgnore {
                patterns: ignore_patterns,
                exclude_file: true,
            }),
        )
        .enabled_if(untracked, "Only untracked files can be ignored"),
    );
    items.push(
        run(
            "Show Diff",
            Some('f'),
            Some("Enter"),
            vec![Action::FocusPane(PaneId::CommitDiffPreview)],
        )
        .enabled_if(file_path.is_some(), "Select a file"),
    );
    items.push(
        run(
            "Three-Side Diff (HEAD ↔ Staged ↔ Local)",
            Some('3'),
            None,
            vec![Action::OpenThreeWayInspector {
                row_idx: None,
                range_idx: None,
            }],
        )
        .enabled_if(file_path.is_some(), "Select a file"),
    );
    let patch_args = diff_patch_args(group == Some(StageNodeKind::Staged), &paths);
    items.push(
        run(
            "Create Patch to Clipboard",
            Some('c'),
            None,
            op(GitOp::PatchToClipboard { args: patch_args }),
        )
        .enabled_if(has_paths, "Select files"),
    );
    items.push(ContextMenuItem::separator());
    items.push(
        run(
            "Copy Relative Path",
            Some('a'),
            None,
            vec![Action::CopyText(paths.join("\n"))],
        )
        .enabled_if(has_paths, "Select files"),
    );
    if let Some(p) = &file_path {
        items.push(ContextMenuItem::action(
            "Open in $EDITOR",
            Some('e'),
            Some("e"),
            ContextMenuAction::OpenExternalEditorFile(p.clone()),
        ));
    }
    let mut body = vec![format!(
        "Delete {} file(s) from the working tree?",
        paths.len()
    )];
    body.extend(paths.iter().take(5).map(|p| format!("  • {p}")));
    items.push(
        run(
            "Delete File…",
            Some('l'),
            None,
            dialog(Dialog::Choice(ChoiceDialog::confirm(
                "Delete Files",
                body,
                "Delete",
                op(GitOp::DeleteFiles {
                    paths: paths.clone(),
                }),
            ))),
        )
        .icon("✕")
        .enabled_if(has_paths, "Select files")
        .enabled_if(
            !matches!(group, Some(StageNodeKind::AmendCommit)),
            "Cannot delete files of the amended commit",
        ),
    );
    items
}

fn diff_side_and_path(state: &AppState) -> (Option<StageDiffSide>, Option<String>) {
    let key = state.stage_workbench.current_diff_key.as_ref();
    (key.map(|k| k.side), key.map(|k| k.path.clone()))
}

fn stage_diff_common_items(side: Option<StageDiffSide>, path: Option<String>) -> Vec<ContextMenuItem> {
    let staged = side == Some(StageDiffSide::Staged);
    let paths: Vec<String> = path.iter().cloned().collect();
    let patch = diff_patch_args(staged, &paths);
    vec![
        run(
            "Copy Patch",
            Some('c'),
            None,
            op(GitOp::PatchToClipboard { args: patch }),
        )
        .enabled_if(path.is_some(), "No file selected"),
        run(
            "Toggle Unified / Side-by-Side",
            Some('t'),
            Some("S"),
            vec![Action::StageDiffToggleSideBySide],
        ),
        run(
            "Inspect HEAD / Staged / Local",
            Some('i'),
            Some("i"),
            vec![Action::OpenThreeWayInspector {
                row_idx: None,
                range_idx: None,
            }],
        )
        .enabled_if(
            matches!(side, Some(StageDiffSide::Staged | StageDiffSide::Unstaged)),
            "Not available for Amend Commit reference",
        ),
        run(
            "Open in $EDITOR",
            Some('e'),
            Some("e"),
            vec![Action::OpenExternalEditorFile(None)],
        ),
    ]
}

/// Right-click on the Commit tab's diff (hunk / line) view.
pub fn stage_diff_menu_items(state: &AppState) -> Vec<ContextMenuItem> {
    let (side, path) = diff_side_and_path(state);
    let editable = matches!(side, Some(StageDiffSide::Staged | StageDiffSide::Unstaged));
    let staged = side == Some(StageDiffSide::Staged);
    let mut items = vec![
        run(
            "Stage Hunk or Lines",
            Some('s'),
            Some("s"),
            vec![Action::StageDiffStageSelection],
        )
        .enabled_if(
            editable && !staged,
            if staged {
                "Lines are already staged"
            } else {
                "Read-only diff cannot be staged"
            },
        ),
        run(
            "Unstage Hunk or Lines",
            Some('u'),
            Some("u"),
            vec![Action::StageDiffUnstageSelection],
        )
        .enabled_if(
            staged,
            if editable {
                "Lines are not staged"
            } else {
                "Read-only diff cannot be unstaged"
            },
        ),
        run(
            "Rollback Lines",
            Some('r'),
            Some("r"),
            vec![Action::StageDiffRollbackSelection],
        )
        .enabled_if(
            editable && !staged,
            "Only unstaged lines can be rolled back",
        ),
        ContextMenuItem::separator(),
    ];
    items.extend(stage_diff_common_items(side, path));
    items
}

/// Right-click on a hunk action button: act on that hunk.
pub fn stage_hunk_menu_items(state: &AppState, hunk_idx: usize) -> Vec<ContextMenuItem> {
    let (side, path) = diff_side_and_path(state);
    let editable = matches!(side, Some(StageDiffSide::Staged | StageDiffSide::Unstaged));
    let staged = side == Some(StageDiffSide::Staged);
    let mut items = vec![
        run(
            "Stage Hunk",
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
                "Hunk is already staged"
            } else {
                "Read-only diff cannot be staged"
            },
        ),
        run(
            "Unstage Hunk",
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
                "Hunk is not staged"
            } else {
                "Read-only diff cannot be unstaged"
            },
        ),
        run(
            "Rollback Hunk",
            Some('r'),
            None,
            vec![Action::ApplyStageDiffHunk(
                hunk_idx,
                DiffHunkActionKind::Rollback,
            )],
        )
        .enabled_if(
            editable && !staged,
            "Only unstaged hunks can be rolled back",
        ),
        ContextMenuItem::separator(),
    ];
    items.extend(stage_diff_common_items(side, path));
    items
}
