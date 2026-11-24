use jbgit_core::{GitOp, PaneId, PatchSource, StageNodeKind};
use jbgit_engine::StageDiffSide;

use crate::app::{Action, AppState, ContextMenuItem, DiffScope};
use crate::config::KeymapScope;
use crate::dialogs::{ChoiceDialog, Dialog};
use crate::hit_test::DiffHunkActionKind;
use crate::keymap::{KeyAction, catalog};

use super::{dialog, op, run};

/// `Git.Stage.Tree.Menu` for the stage tree row `idx` (plus selection).
pub fn stage_tree_menu_items(state: &AppState, idx: usize) -> Vec<ContextMenuItem> {
    let i18n = &state.i18n;
    let stage_sc = catalog::shortcut_for_action(state, &[KeymapScope::Commit], &KeyAction::ToggleStage);
    let discard_sc = catalog::all_shortcuts_for_action(state, &[KeymapScope::Commit], &KeyAction::DiscardChanges);
    let edit_sc = catalog::shortcut_for_action(state, &[KeymapScope::Commit], &KeyAction::EditFile);
    let row = state.stage_workbench.rows().get(idx);
    let group = row.map(|r| r.group_kind());
    let targets = state
        .stage_workbench
        .status()
        .map(|s| {
            state
                .stage_workbench
                .collect_targets_for_action(idx, s, |_| true)
        })
        .unwrap_or_default();
    let mut paths: Vec<String> = targets.iter().map(|t| t.path.clone()).collect();
    paths.dedup();
    let entry = state.stage_entry_at(idx);
    let mut staged_paths = Vec::new();
    let mut unstaged_paths = Vec::new();
    let mut untracked_paths = Vec::new();
    if let Some(status) = state.stage_workbench.status() {
        let untracked_set: std::collections::HashSet<&str> = status
            .untracked
            .iter()
            .chain(
                status
                    .unstaged
                    .iter()
                    .filter(|e| e.kind == jbgit_core::FileStatusKind::Untracked),
            )
            .map(|e| e.path.as_str())
            .collect();
        for t in &targets {
            match t.group_kind {
                StageNodeKind::Staged => staged_paths.push(t.path.clone()),
                StageNodeKind::Untracked => untracked_paths.push(t.path.clone()),
                StageNodeKind::Unstaged => {
                    if untracked_set.contains(t.path.as_str()) {
                        untracked_paths.push(t.path.clone());
                    } else {
                        unstaged_paths.push(t.path.clone());
                    }
                }
                _ => {}
            }
        }
        staged_paths.sort();
        staged_paths.dedup();
        unstaged_paths.sort();
        unstaged_paths.dedup();
        untracked_paths.sort();
        untracked_paths.dedup();
    }
    if untracked_paths.is_empty()
        && let Some(e) = &entry
        && (group == Some(StageNodeKind::Untracked)
            || e.kind == jbgit_core::FileStatusKind::Untracked)
    {
        untracked_paths.push(e.path.clone());
    }
    // Untracked files below the collapse threshold live in the Unstaged group.
    let has_untracked = !untracked_paths.is_empty();
    let entry_untracked = group == Some(StageNodeKind::Untracked)
        || entry
            .as_ref()
            .is_some_and(|e| e.kind == jbgit_core::FileStatusKind::Untracked);
    let entry_ignored = group == Some(StageNodeKind::Ignored)
        || entry
            .as_ref()
            .is_some_and(|e| e.kind == jbgit_core::FileStatusKind::Ignored);
    let file_path = entry.map(|e| e.path);
    let submodule = row.and_then(|r| r.submodule());
    let is_submodule = submodule.is_some();
    let has_paths = !paths.is_empty();
    let mut items = Vec::new();

    if is_submodule && let Some(p) = &file_path {
        let repo_target = state
            .repo_snapshot
            .as_ref()
            .map(|s| s.workdir.join(p))
            .unwrap_or_else(|| std::path::PathBuf::from(p));
        let uninit = submodule.is_some_and(|s| s.uninitialized);
        items.push(
            run(
                i18n.t("menu.stage.open_submodule"),
                None,
                None,
                vec![Action::SwitchRepository(repo_target)],
            )
            .enabled_if(!uninit, i18n.t("menu.stage.submodule_uninitialized_reason")),
        );
        items.push(run(
            i18n.t("menu.stage.update_submodule"),
            None,
            None,
            op(GitOp::SubmoduleUpdate {
                path: Some(p.clone()),
            }),
        ));
        items.push(ContextMenuItem::separator());
    }

    if group == Some(StageNodeKind::Conflicted) {
        if let Some(p) = &file_path {
            items.push(
                run(
                    i18n.t("menu.stage.resolve_conflicts"),
                    Some('s'),
                    None,
                    vec![Action::OpenConflictResolver(p.clone())],
                )
                .icon("⚠"),
            );
            items.push(run(
                i18n.t("menu.stage.open_conflict_queue"),
                Some('q'),
                catalog::shortcut_for_action(state, &[KeymapScope::Global], &KeyAction::ResolveConflicts)
                    .as_deref(),
                vec![Action::OpenConflictQueue],
            ));
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
        let non_sub = !is_submodule;
        let sub_reason = i18n.t("menu.stage.submodule_readonly_reason");
        if g.can_stage() {
            items.push(
                run(
                    i18n.t("menu.stage.stage"),
                    Some('s'),
                    stage_sc.as_deref(),
                    vec![Action::StageActionSelected],
                )
                .icon("+"),
            );
            items.push(
                run(
                    i18n.t("menu.stage.discard_changes"),
                    Some('d'),
                    discard_sc.as_deref(),
                    vec![Action::DiscardPromptSelected],
                )
                .icon("↶"),
            );
        }
        if has_untracked {
            items.push(
                run(
                    i18n.t("menu.stage.intent_to_add"),
                    Some('n'),
                    None,
                    op(GitOp::IntentToAdd {
                        paths: untracked_paths.clone(),
                    }),
                )
                .enabled_if(non_sub, sub_reason),
            );
        }
        if g.can_unstage() {
            items.push(
                run(
                    i18n.t("menu.stage.unstage"),
                    Some('u'),
                    stage_sc.as_deref(),
                    vec![Action::UnstageActionSelected],
                )
                .icon("−"),
            );
        }
    }
    let paths_count_s = paths.len().to_string();
    items.push(
        run(
            i18n.t("menu.stage.stash_changes"),
            Some('g'),
            None,
            dialog(Dialog::StashPush(crate::dialogs::StashPushDialog::new(
                i18n.t_fmt("menu.stage.stash_selected_msg", &[("count", &paths_count_s)]),
                has_untracked,
                paths.clone(),
            ))),
        )
        .enabled_if(has_paths, i18n.t("common.select_files"))
        .enabled_if(!is_submodule, i18n.t("menu.stage.submodule_readonly_reason"))
        .enabled_if(
            group != Some(StageNodeKind::Conflicted),
            i18n.t("menu.stage.resolve_before_stash"),
        ),
    );
    items.push(ContextMenuItem::separator());
    let ignore_patterns = jbgit_core::build_ignore_patterns(
        &untracked_paths,
        jbgit_core::IgnoreRuleKind::ExactPath,
        None,
    );
    items.push(
        run(
            i18n.t("menu.stage.add_to_gitignore"),
            Some('i'),
            None,
            op(GitOp::AddToIgnore {
                patterns: ignore_patterns.clone(),
                exclude_file: false,
                target_file: None,
            }),
        )
        .enabled_if(has_untracked, i18n.t("menu.stage.only_untracked_ignore")),
    );
    items.push(
        run(
            i18n.t("menu.stage.add_to_exclude"),
            Some('x'),
            None,
            op(GitOp::AddToIgnore {
                patterns: ignore_patterns.clone(),
                exclude_file: true,
                target_file: None,
            }),
        )
        .enabled_if(has_untracked, i18n.t("menu.stage.only_untracked_ignore")),
    );
    let ignore_submenu_paths = if has_untracked {
        &untracked_paths
    } else {
        &paths
    };
    items.push(build_flexible_ignore_submenu(
        state,
        ignore_submenu_paths,
        has_untracked,
    ));
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
    let history_sc = catalog::shortcut_for_action(state, &[KeymapScope::Commit], &KeyAction::FileHistory);
    let ann_sc = catalog::shortcut_for_action(state, &[KeymapScope::Commit], &KeyAction::Annotate);
    let history_action = file_path
        .as_ref()
        .map(|p| {
            vec![Action::OpenFileHistory {
                path: p.clone(),
                start_oid: None,
                follow_renames: true,
                line_range: None,
            }]
        })
        .unwrap_or_default();
    items.push(
        run(
            i18n.t("menu.stage.file_history"),
            Some('h'),
            history_sc.as_deref(),
            history_action,
        )
        .enabled_if(file_path.is_some(), i18n.t("menu.stage.select_file"))
        .enabled_if(
            !entry_untracked && !entry_ignored,
            i18n.t("menu.stage.only_tracked_history"),
        ),
    );
    items.push(ContextMenuItem::from_command(
        i18n.t("menu.stage.annotate"),
        Some('b'),
        ann_sc.as_deref(),
        catalog::annotate_stage_entry_at(state, idx),
    ));
    let stage_patch_source = PatchSource::StagePaths {
        staged: staged_paths,
        unstaged: unstaged_paths,
        untracked: untracked_paths,
    };
    let has_patch_paths = !stage_patch_source.is_empty();
    items.push(
        run(
            i18n.t("menu.stage.create_patch_file"),
            Some('w'),
            None,
            dialog(crate::dialogs::create_patch_dialog(
                state,
                stage_patch_source.clone(),
                false,
            )),
        )
        .enabled_if(has_patch_paths, i18n.t("common.select_files")),
    );
    items.push(
        run(
            i18n.t("menu.stage.create_patch_clipboard"),
            Some('c'),
            None,
            op(GitOp::PatchToClipboard {
                source: stage_patch_source,
                reverse: false,
            }),
        )
        .enabled_if(has_patch_paths, i18n.t("common.select_files")),
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
        items.push(
            run(
                i18n.t("menu.stage.open_external_editor"),
                Some('e'),
                edit_sc.as_deref(),
                vec![Action::OpenExternalEditorFile(Some(p.clone()))],
            )
            .enabled_if(!is_submodule, i18n.t("menu.stage.submodule_readonly_reason")),
        );
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
        .enabled_if(!is_submodule, i18n.t("menu.stage.submodule_readonly_reason"))
        .enabled_if(
            !matches!(group, Some(StageNodeKind::AmendCommit)),
            i18n.t("menu.stage.cannot_delete_amend"),
        ),
    );
    items.push(ContextMenuItem::separator());
    items.extend(apply_patch_items(state));
    items
}

/// `Apply Patch…` / `Apply Patch from Clipboard` (Stage tree menu).
pub fn apply_patch_items(state: &AppState) -> Vec<ContextMenuItem> {
    let i18n = &state.i18n;
    let sc = |a: &KeyAction| catalog::shortcut_for_action(state, &[KeymapScope::Commit], a);
    vec![
        ContextMenuItem::from_command(
            i18n.t("menu.stage.apply_patch"),
            None,
            sc(&KeyAction::ApplyPatch).as_deref(),
            catalog::apply_patch_command(state, false),
        ),
        ContextMenuItem::from_command(
            i18n.t("menu.stage.apply_patch_clipboard"),
            Some('v'),
            sc(&KeyAction::ApplyPatchClipboard).as_deref(),
            catalog::apply_patch_command(state, true),
        ),
    ]
}

fn diff_side_and_path(state: &AppState) -> (Option<StageDiffSide>, Option<String>) {
    let key = state.stage_workbench.current_diff_key();
    (key.map(|k| k.side), key.map(|k| k.path.clone()))
}

fn stage_diff_common_items(
    state: &AppState,
    side: Option<StageDiffSide>,
    path: Option<String>,
) -> Vec<ContextMenuItem> {
    let i18n = &state.i18n;
    let sbs_sc = catalog::shortcut_for_action(state, &[KeymapScope::Diff], &KeyAction::ToggleSideBySide);
    let ws_sc = catalog::shortcut_for_action(state, &[KeymapScope::Diff], &KeyAction::CycleDiffWhitespace);
    let wrap_sc = catalog::shortcut_for_action(state, &[KeymapScope::Diff], &KeyAction::ToggleDiffSoftWrap);
    let three_sc = catalog::shortcut_for_action(state, &[KeymapScope::Diff], &KeyAction::ThreeWayInspector);
    let edit_sc = catalog::shortcut_for_action(state, &[KeymapScope::Commit], &KeyAction::EditFile);
    let ann_sc = catalog::shortcut_for_action(state, &[KeymapScope::Commit], &KeyAction::Annotate);
    let is_amend = side == Some(StageDiffSide::AmendCommit);
    let staged = side == Some(StageDiffSide::Staged);
    let paths: Vec<String> = path.iter().cloned().collect();
    let is_untracked = !staged
        && !is_amend
        && path.as_deref().is_some_and(|p| {
            state.stage_workbench.status().is_some_and(|st| {
                st.untracked.iter().any(|e| e.path == p)
                    || st
                        .unstaged
                        .iter()
                        .any(|e| e.path == p && e.kind == jbgit_core::FileStatusKind::Untracked)
            })
        });
    let diff_patch_source = if is_amend {
        PatchSource::StagePaths {
            staged: Vec::new(),
            unstaged: Vec::new(),
            untracked: Vec::new(),
        }
    } else if staged {
        PatchSource::StagePaths {
            staged: paths,
            unstaged: Vec::new(),
            untracked: Vec::new(),
        }
    } else if is_untracked {
        PatchSource::StagePaths {
            staged: Vec::new(),
            unstaged: Vec::new(),
            untracked: paths,
        }
    } else {
        PatchSource::StagePaths {
            staged: Vec::new(),
            unstaged: paths,
            untracked: Vec::new(),
        }
    };
    vec![
        ContextMenuItem::from_command(
            i18n.t("menu.stage.annotate"),
            Some('b'),
            ann_sc.as_deref(),
            catalog::annotate_stage_diff(state, path.as_deref()),
        ),
        run(
            i18n.t("menu.stage.create_patch_file"),
            Some('p'),
            None,
            dialog(crate::dialogs::create_patch_dialog(
                state,
                diff_patch_source.clone(),
                false,
            )),
        )
        .enabled_if(path.is_some(), i18n.t("menu.stage.no_file_selected"))
        .enabled_if(!is_amend, i18n.t("menu.stage.not_for_amend")),
        run(
            i18n.t("menu.stage.copy_patch"),
            Some('c'),
            None,
            op(GitOp::PatchToClipboard {
                source: diff_patch_source,
                reverse: false,
            }),
        )
        .enabled_if(path.is_some(), i18n.t("menu.stage.no_file_selected"))
        .enabled_if(!is_amend, i18n.t("menu.stage.not_for_amend")),
        run(
            i18n.t("menu.stage.toggle_side_by_side"),
            Some('t'),
            sbs_sc.as_deref(),
            vec![Action::DiffToggleSideBySide(DiffScope::Stage)],
        ),
        run(
            i18n.t("menu.log.cycle_whitespace"),
            None,
            ws_sc.as_deref(),
            vec![Action::DiffCycleWhitespace(DiffScope::Stage)],
        ),
        run(
            i18n.t("menu.log.toggle_soft_wrap"),
            Some('w'),
            wrap_sc.as_deref(),
            vec![Action::DiffToggleSoftWrap(DiffScope::Stage)],
        ),
        run(
            i18n.t("menu.stage.inspect_three_way"),
            Some('i'),
            three_sc.as_deref(),
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
            edit_sc.as_deref(),
            vec![Action::OpenExternalEditorFile(None)],
        ),
    ]
}

/// Right-click on the Commit tab's diff (hunk / line) view.
pub fn stage_diff_menu_items(state: &AppState) -> Vec<ContextMenuItem> {
    let i18n = &state.i18n;
    let stage_sc = catalog::shortcut_for_action(state, &[KeymapScope::Diff], &KeyAction::StageSelection)
        .or_else(|| catalog::shortcut_for_action(state, &[KeymapScope::Diff], &KeyAction::ToggleStage));
    let unstage_sc = catalog::shortcut_for_action(state, &[KeymapScope::Diff], &KeyAction::UnstageSelection)
        .or_else(|| catalog::shortcut_for_action(state, &[KeymapScope::Diff], &KeyAction::ToggleStage));
    let rollback_sc = catalog::all_shortcuts_for_action(state, &[KeymapScope::Diff], &KeyAction::RollbackSelection);
    let (side, path) = diff_side_and_path(state);
    let is_readonly_type = state
        .stage_workbench
        .current_diff()
        .is_some_and(|d| d.is_line_staging_disabled())
        || state.stage_workbench.diff_whitespace() != jbgit_core::WhitespaceMode::Default;
    let editable =
        matches!(side, Some(StageDiffSide::Staged | StageDiffSide::Unstaged)) && !is_readonly_type;
    let staged = side == Some(StageDiffSide::Staged);
    let sel_range = state.stage_workbench.selected_line_range();
    let sel_history_actions = match (&path, sel_range) {
        (Some(p), Some((start_line, end_line))) => vec![Action::OpenSelectionHistory {
            path: p.clone(),
            start_oid: None,
            start_line,
            end_line,
        }],
        _ => Vec::new(),
    };
    let mut items = vec![
        run(
            i18n.t("menu.stage.stage_selection"),
            Some('s'),
            stage_sc.as_deref(),
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
            unstage_sc.as_deref(),
            vec![Action::StageDiffUnstageSelection],
        )
        .enabled_if(
            editable && staged,
            if editable {
                i18n.t("menu.stage.not_staged")
            } else {
                i18n.t("menu.stage.readonly_unstage")
            },
        ),
        run(
            i18n.t("menu.stage.rollback_selection"),
            Some('r'),
            rollback_sc.as_deref(),
            vec![Action::StageDiffRollbackSelection],
        )
        .enabled_if(editable && !staged, i18n.t("menu.stage.only_unstaged_rollback")),
        run(
            i18n.t("menu.stage.selection_history"),
            Some('h'),
            None,
            sel_history_actions,
        )
        .enabled_if(
            path.is_some() && sel_range.is_some(),
            i18n.t("menu.stage.no_selection_range"),
        ),
        ContextMenuItem::separator(),
    ];
    items.extend(stage_diff_common_items(state, side, path));
    items
}

/// Right-click on a hunk action button: act on that hunk.
pub fn stage_hunk_menu_items(state: &AppState, hunk_idx: usize) -> Vec<ContextMenuItem> {
    let i18n = &state.i18n;
    let (side, path) = diff_side_and_path(state);
    let is_readonly_type = state
        .stage_workbench
        .current_diff()
        .is_some_and(|d| d.is_line_staging_disabled())
        || state.stage_workbench.diff_whitespace() != jbgit_core::WhitespaceMode::Default;
    let editable =
        matches!(side, Some(StageDiffSide::Staged | StageDiffSide::Unstaged)) && !is_readonly_type;
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
            editable && staged,
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

/// Finds the nearest existing nested `.gitignore` in an ancestor directory of
/// `rel_path` (excluding the repository root `.gitignore`), returning
/// `(relative_dir, relative_gitignore_path)`.
fn nearest_nested_gitignore(
    workdir: &std::path::Path,
    rel_path: &str,
) -> Option<(String, std::path::PathBuf)> {
    let norm = rel_path.trim().trim_start_matches(['/', '\\']).replace('\\', "/");
    let mut cur = norm.as_str();
    while let Some((parent, _)) = cur.rsplit_once('/') {
        if parent.is_empty() {
            break;
        }
        let rel_gi = std::path::PathBuf::from(parent).join(".gitignore");
        if workdir.join(&rel_gi).is_file() {
            return Some((parent.to_string(), rel_gi));
        }
        cur = parent;
    }
    None
}

/// Flexible `Add to Ignore…` submenu: exact path / extension (`*.ext`) /
/// parent directory (`/<dir>/`) targeting root `.gitignore`, nearest nested
/// `.gitignore` (when present), or `.git/info/exclude`, plus quick `$EDITOR`
/// shortcuts for `.gitignore` and `.git/info/exclude`.
fn build_flexible_ignore_submenu(
    state: &AppState,
    paths: &[String],
    untracked: bool,
) -> ContextMenuItem {
    use jbgit_core::{IgnoreRuleKind, build_ignore_patterns};

    let i18n = &state.i18n;
    let workdir = state
        .repo_snapshot
        .as_ref()
        .map(|s| s.workdir.as_path())
        .unwrap_or_else(|| state.repo_path_display.as_path());
    let exclude_path = state
        .repo_snapshot
        .as_ref()
        .map(|s| s.common_dir.join("info").join("exclude"))
        .unwrap_or_else(|| workdir.join(".git").join("info").join("exclude"));

    let nested = paths
        .first()
        .and_then(|p| nearest_nested_gitignore(workdir, p))
        .filter(|(dir, _)| {
            let prefix = format!("{dir}/");
            paths.iter().all(|p| {
                let norm = p.trim().trim_start_matches(['/', '\\']).replace('\\', "/");
                norm.starts_with(&prefix)
            })
        });

    let mut sub: Vec<ContextMenuItem> = Vec::new();
    let reason = i18n.t("menu.stage.only_untracked_ignore");

    let push_rule_group = |sub: &mut Vec<ContextMenuItem>,
                           target_label: &str,
                           exclude_file: bool,
                           base_dir: Option<&str>,
                           target_file: Option<std::path::PathBuf>| {
        let exact = build_ignore_patterns(paths, IgnoreRuleKind::ExactPath, base_dir);
        if let Some(sample) = exact.first() {
            sub.push(
                run(
                    i18n.t_fmt(
                        "menu.stage.ignore_rule_item",
                        &[("pattern", sample), ("target", target_label)],
                    ),
                    None,
                    None,
                    op(GitOp::AddToIgnore {
                        patterns: exact,
                        exclude_file,
                        target_file: target_file.clone(),
                    }),
                )
                .enabled_if(untracked, reason),
            );
        }
        let by_ext = build_ignore_patterns(paths, IgnoreRuleKind::Extension, base_dir);
        if let Some(sample) = by_ext.first() {
            sub.push(
                run(
                    i18n.t_fmt(
                        "menu.stage.ignore_ext_item",
                        &[("pattern", sample), ("target", target_label)],
                    ),
                    None,
                    None,
                    op(GitOp::AddToIgnore {
                        patterns: by_ext,
                        exclude_file,
                        target_file: target_file.clone(),
                    }),
                )
                .enabled_if(untracked, reason),
            );
        }
        let by_dir = build_ignore_patterns(paths, IgnoreRuleKind::ParentDir, base_dir);
        if let Some(sample) = by_dir.first() {
            sub.push(
                run(
                    i18n.t_fmt(
                        "menu.stage.ignore_dir_item",
                        &[("pattern", sample), ("target", target_label)],
                    ),
                    None,
                    None,
                    op(GitOp::AddToIgnore {
                        patterns: by_dir,
                        exclude_file,
                        target_file,
                    }),
                )
                .enabled_if(untracked, reason),
            );
        }
    };

    push_rule_group(&mut sub, ".gitignore", false, None, None);
    if let Some((nested_dir, nested_file)) = nested {
        let label = format!("{nested_dir}/.gitignore");
        sub.push(ContextMenuItem::separator());
        push_rule_group(&mut sub, &label, false, Some(&nested_dir), Some(nested_file));
    }
    sub.push(ContextMenuItem::separator());
    push_rule_group(&mut sub, ".git/info/exclude", true, None, None);
    sub.push(ContextMenuItem::separator());
    sub.push(run(
        i18n.t("menu.stage.open_gitignore_editor"),
        None,
        None,
        vec![Action::OpenExternalEditorFile(Some(".gitignore".to_string()))],
    ));
    sub.push(run(
        i18n.t("menu.stage.open_exclude_editor"),
        None,
        None,
        vec![Action::OpenExternalEditorFile(Some(
            exclude_path.to_string_lossy().into_owned(),
        ))],
    ));

    ContextMenuItem::submenu(i18n.t("menu.stage.ignore_more"), None, sub)
}
