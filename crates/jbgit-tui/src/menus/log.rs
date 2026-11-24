use jbgit_core::{
    ChangeRowKind, DetailsMode, FileStatusKind, FixupKind, GitOp, Oid, PatchSource, RebaseOptions,
    RefKind, plan_squash_todo,
};

use crate::app::{Action, AppState, ContextMenuItem, DiffScope, PopupKind};
use crate::config::KeymapScope;
use crate::dialogs::{
    ChoiceDialog, Dialog, InputDialog, InputIntent, InputValidation, MessageDialog, MessageIntent,
    RebaseTodoDialog, new_branch_dialog, reset_dialog,
};
use crate::keymap::{KeyAction, catalog};

use super::{branch_menu_items, dialog, ellipsize, local_names, op, run};

/// Full Log commit-row menu for the current selection (§3.1).
pub fn log_commit_menu_items(state: &AppState) -> Vec<ContextMenuItem> {
    let Some(snap) = state.repo_snapshot.as_deref() else {
        return Vec::new();
    };
    let Some(primary) = state.log_commit_at(state.log.graph.selected_idx).cloned() else {
        return Vec::new();
    };
    let oids = state.selected_commits_oldest_first();
    let n = oids.len().max(1);
    let n_str = n.to_string();
    let single = n == 1;
    let short = primary.oid.short();
    let head = snap.head_oid;
    let branch = snap.head_branch.clone();
    let cur = branch.clone().unwrap_or_else(|| "HEAD".into());
    let no_ongoing = state.require_no_ongoing();
    let i18n = &state.i18n;
    let checkout_sc = catalog::shortcut_for_action(state, &[KeymapScope::Log], &KeyAction::Checkout);
    let compare_sc = catalog::shortcut_for_action(state, &[KeymapScope::Log], &KeyAction::CompareOrDiff);
    let uncommit_sc = catalog::shortcut_for_action(state, &[KeymapScope::Log], &KeyAction::Uncommit);
    let reword_sc = catalog::shortcut_for_action(state, &[KeymapScope::Log], &KeyAction::Reword);
    let new_branch_sc = catalog::shortcut_for_action(state, &[KeymapScope::Log], &KeyAction::NewBranch);
    let copy_sc = catalog::shortcut_for_action(state, &[KeymapScope::Log], &KeyAction::CopyHash);
    let goto_sc = catalog::shortcut_for_action(state, &[KeymapScope::Log], &KeyAction::GotoRevision);
    let need_single = |what: &str| i18n.t_fmt("menu.log.need_single", &[("what", what)]);
    let mut items = Vec::new();

    // ── Checkout & compare ──
    let locals: Vec<String> = snap
        .refs
        .iter()
        .filter(|r| r.kind == RefKind::LocalBranch && r.target_oid == primary.oid)
        .map(|r| r.name.clone())
        .collect();
    let checkout_label = i18n.t_fmt("menu.log.checkout_revision", &[("short", &short)]);
    let checkout = if locals.is_empty() || !single {
        run(
            checkout_label,
            Some('c'),
            checkout_sc.as_deref(),
            op(GitOp::Checkout {
                target: primary.oid.to_string(),
                detach: true,
            }),
        )
    } else {
        let mut sub: Vec<ContextMenuItem> = locals
            .iter()
            .map(|b| {
                run(
                    i18n.t_fmt("menu.log.checkout_branch", &[("name", b)]),
                    None,
                    None,
                    op(GitOp::Checkout {
                        target: b.clone(),
                        detach: false,
                    }),
                )
                .icon("⎇")
            })
            .collect();
        sub.push(run(
            i18n.t_fmt("menu.log.checkout_detached", &[("short", &short)]),
            Some('d'),
            None,
            op(GitOp::Checkout {
                target: primary.oid.to_string(),
                detach: true,
            }),
        ));
        ContextMenuItem::submenu(checkout_label, Some('c'), sub)
    };
    items.push(
        checkout
            .icon("⎇")
            .enabled_if(single, need_single(i18n.t("menu.branch.checkout")))
            .require(no_ongoing.clone()),
    );
    let compare_local_lbl = i18n.t("menu.log.compare_with_local");
    items.push(
        run(
            compare_local_lbl,
            Some('d'),
            None,
            vec![Action::CompareWithLocal {
                target: primary.oid,
            }],
        )
        .enabled_if(single, need_single(compare_local_lbl)),
    );
    let compare_versions = match oids.as_slice() {
        [base, target] => vec![Action::CompareVersions {
            base: *base,
            target: *target,
        }],
        _ => Vec::new(),
    };
    items.push(
        run(
            i18n.t("menu.log.compare_versions"),
            None,
            compare_sc.as_deref(),
            compare_versions,
        )
        .enabled_if(n == 2, i18n.t("menu.log.compare_need_two")),
    );
    items.push(ContextMenuItem::separator());

    // ── Cherry-pick & revert ──
    let in_head: Vec<bool> = oids.iter().map(|o| state.is_ancestor_of_head(*o)).collect();
    let merge_commit_in_selection = oids
        .iter()
        .find_map(|o| snap.commit(o).filter(|c| c.parents.len() > 1));
    let mainline_dialog = |is_revert: bool, no_commit: bool, record_origin: bool| -> Vec<Action> {
        if let Some(merge_c) = merge_commit_in_selection {
            let merge_short = merge_c.oid.short();
            let mut opts: Vec<crate::dialogs::ChoiceOption> = merge_c
                .parents
                .iter()
                .enumerate()
                .map(|(i, p_oid)| {
                    let parent_num = (i + 1) as u8;
                    let p_num_s = parent_num.to_string();
                    let p_short = p_oid.short();
                    let p_subj = snap
                        .commits
                        .iter()
                        .find(|c| c.oid == *p_oid)
                        .map(|c| ellipsize(&c.subject, 44))
                        .unwrap_or_default();
                    let git_op = if is_revert {
                        let mut newest_first = oids.clone();
                        newest_first.reverse();
                        GitOp::Revert {
                            oids: newest_first,
                            no_commit,
                            mainline: Some(parent_num),
                        }
                    } else {
                        GitOp::CherryPick {
                            oids: oids.clone(),
                            no_commit,
                            record_origin,
                            mainline: Some(parent_num),
                        }
                    };
                    crate::dialogs::ChoiceOption::new(
                        i18n.t_fmt(
                            "dialog.mainline.parent_option",
                            &[("n", &p_num_s), ("short", &p_short), ("subject", &p_subj)],
                        ),
                        char::from_digit(u32::from(parent_num), 10),
                        op(git_op),
                    )
                })
                .collect();
            opts.push(crate::dialogs::ChoiceOption::new(
                i18n.cancel(),
                Some('c'),
                Vec::new(),
            ));
            let title_key = if is_revert {
                "dialog.mainline.revert_title"
            } else {
                "dialog.mainline.cherry_pick_title"
            };
            dialog(Dialog::Choice(ChoiceDialog::buttons(
                i18n.t(title_key),
                vec![i18n.t_fmt(
                    "dialog.mainline.prompt",
                    &[
                        ("short", &merge_short),
                        ("subject", &ellipsize(&merge_c.subject, 48)),
                    ],
                )],
                opts,
            )))
        } else if is_revert {
            let mut newest_first = oids.clone();
            newest_first.reverse();
            op(GitOp::Revert {
                oids: newest_first,
                no_commit,
                mainline: None,
            })
        } else {
            op(GitOp::CherryPick {
                oids: oids.clone(),
                no_commit,
                record_origin,
                mainline: None,
            })
        }
    };
    items.push(
        run(
            if single {
                i18n.t("menu.log.cherry_pick").to_string()
            } else {
                i18n.t_fmt("menu.log.cherry_pick_many", &[("count", &n_str)])
            },
            Some('p'),
            None,
            mainline_dialog(false, false, false),
        )
        .icon("🍒")
        .enabled_if(
            !in_head.iter().any(|b| *b),
            i18n.t("menu.log.cherry_pick_in_head"),
        )
        .enabled_if(head.is_some(), i18n.t("menu.log.no_current_branch"))
        .require(no_ongoing.clone()),
    );
    items.push(
        run(
            i18n.t("menu.log.cherry_pick_no_commit"),
            None,
            None,
            mainline_dialog(false, true, false),
        )
        .enabled_if(
            !in_head.iter().any(|b| *b),
            i18n.t("menu.log.cherry_pick_in_head"),
        )
        .enabled_if(head.is_some(), i18n.t("menu.log.no_current_branch"))
        .require(no_ongoing.clone()),
    );
    items.push(
        run(
            i18n.t("menu.log.cherry_pick_record_origin"),
            None,
            None,
            mainline_dialog(false, false, true),
        )
        .enabled_if(
            !in_head.iter().any(|b| *b),
            i18n.t("menu.log.cherry_pick_in_head"),
        )
        .enabled_if(head.is_some(), i18n.t("menu.log.no_current_branch"))
        .require(no_ongoing.clone()),
    );
    items.push(
        run(
            if single {
                i18n.t("menu.log.revert_commit").to_string()
            } else {
                i18n.t_fmt("menu.log.revert_many", &[("count", &n_str)])
            },
            Some('v'),
            None,
            mainline_dialog(true, false, false),
        )
        .enabled_if(
            in_head.iter().all(|b| *b),
            i18n.t("menu.log.revert_not_in_head"),
        )
        .require(no_ongoing.clone()),
    );
    items.push(
        run(
            i18n.t("menu.log.revert_no_commit"),
            None,
            None,
            mainline_dialog(true, true, false),
        )
        .enabled_if(
            in_head.iter().all(|b| *b),
            i18n.t("menu.log.revert_not_in_head"),
        )
        .require(no_ongoing.clone()),
    );
    let reset_lbl = i18n.t("menu.log.reset_current_branch");
    items.push(
        run(
            reset_lbl,
            Some('r'),
            None,
            dialog(reset_dialog(primary.oid, &primary.subject, &cur, i18n)),
        )
        .enabled_if(single, need_single(reset_lbl))
        .require(no_ongoing.clone()),
    );
    items.push(ContextMenuItem::from_command(
        i18n.t("menu.log.uncommit"),
        Some('u'),
        uncommit_sc.as_deref(),
        catalog::commit_uncommit(state),
    ));
    items.push(ContextMenuItem::separator());

    // ── History rewriting ──
    let reword_lbl = i18n.t("menu.log.reword");
    items.push(
        ContextMenuItem::from_command(
            reword_lbl,
            Some('w'),
            reword_sc.as_deref(),
            catalog::commit_reword(state),
        )
        .icon("✎"),
    );
    items.push(squash_item(state, &oids));
    let staged = state.has_staged_changes();
    let in_head_primary = state.is_ancestor_of_head(primary.oid);
    let fixup_only_lbl = i18n.t("menu.log.fixup_commit");
    let fixup_only = if single {
        run(
            fixup_only_lbl,
            None,
            None,
            op(GitOp::FixupCommit {
                target: primary.oid,
                kind: FixupKind::Fixup,
                base: primary.parents.first().copied(),
                autosquash: false,
            }),
        )
        .enabled_if(staged, i18n.t("menu.log.fixup_need_staged"))
        .enabled_if(in_head_primary, i18n.t("menu.log.push_not_in_head"))
        .require(no_ongoing.clone())
    } else {
        run(fixup_only_lbl, None, None, Vec::new())
            .enabled_if(false, need_single(fixup_only_lbl))
    };
    items.push(fixup_only);
    let squash_only_lbl = i18n.t("menu.log.squash_commit");
    let squash_only = if single {
        run(
            squash_only_lbl,
            None,
            None,
            op(GitOp::FixupCommit {
                target: primary.oid,
                kind: FixupKind::Squash,
                base: primary.parents.first().copied(),
                autosquash: false,
            }),
        )
        .enabled_if(staged, i18n.t("menu.log.fixup_need_staged"))
        .enabled_if(in_head_primary, i18n.t("menu.log.push_not_in_head"))
        .require(no_ongoing.clone())
    } else {
        run(squash_only_lbl, None, None, Vec::new())
            .enabled_if(false, need_single(squash_only_lbl))
    };
    items.push(squash_only);
    let fixup_lbl = i18n.t("menu.log.fixup");
    let fixup = if single {
        run(
            fixup_lbl,
            Some('f'),
            None,
            op(GitOp::FixupCommit {
                target: primary.oid,
                kind: FixupKind::Fixup,
                base: primary.parents.first().copied(),
                autosquash: true,
            }),
        )
        .enabled_if(staged, i18n.t("menu.log.fixup_need_staged"))
        .require(state.rewrite_plan(primary.oid, "fixup").map(|_| ()))
    } else {
        run(fixup_lbl, Some('f'), None, Vec::new()).enabled_if(false, need_single(fixup_lbl))
    };
    items.push(fixup);
    items.push(drop_item(state, &oids));
    let rebase_lbl = i18n.t("menu.log.interactive_rebase");
    let interactive = match state.rewrite_plan(primary.oid, "rebase") {
        Ok(plan) if single => run(
            rebase_lbl,
            Some('i'),
            None,
            dialog(Dialog::RebaseTodo(RebaseTodoDialog::new(
                plan.base,
                plan.entries,
                plan.messages,
            ))),
        ),
        Ok(_) => run(rebase_lbl, Some('i'), None, Vec::new())
            .enabled_if(false, need_single(rebase_lbl)),
        Err(e) => run(rebase_lbl, Some('i'), None, Vec::new()).enabled_if(false, e),
    };
    items.push(interactive);
    let rebase_onto_lbl = i18n.t_fmt(
        "menu.log.rebase_onto_commit",
        &[("cur", &ellipsize(&cur, 20))],
    );
    let rebase_onto = if single {
        run(
            rebase_onto_lbl.clone(),
            None,
            None,
            op(GitOp::Rebase {
                options: RebaseOptions::onto(primary.oid.to_string(), None),
            }),
        )
        .enabled_if(branch.is_some(), i18n.t("menu.log.detached_no_branch"))
        .enabled_if(
            !in_head_primary,
            i18n.t("menu.log.rebase_commit_in_head"),
        )
        .require(no_ongoing.clone())
    } else {
        run(rebase_onto_lbl.clone(), None, None, Vec::new())
            .enabled_if(false, need_single(&rebase_onto_lbl))
    };
    items.push(rebase_onto);
    let push_up_lbl = i18n.t("menu.log.push_up_to");
    let push_up_to = match &branch {
        Some(b) => run(
            push_up_lbl,
            None,
            None,
            vec![Action::PromptPush {
                branch: b.clone(),
                up_to: Some(primary.oid),
            }],
        )
        .enabled_if(
            state.is_ancestor_of_head(primary.oid),
            i18n.t("menu.log.push_not_in_head"),
        ),
        None if snap.head_oid.is_some() => run(
            push_up_lbl,
            None,
            None,
            vec![Action::PromptPush {
                branch: "HEAD".to_string(),
                up_to: Some(primary.oid),
            }],
        )
        .enabled_if(
            state.is_ancestor_of_head(primary.oid),
            i18n.t("menu.log.push_not_in_head"),
        ),
        None => run(push_up_lbl, None, None, Vec::new())
            .enabled_if(false, i18n.t("menu.log.detached_no_branch")),
    };
    items.push(push_up_to.enabled_if(single, need_single(push_up_lbl)));
    items.push(ContextMenuItem::separator());

    // ── Branches & tags ──
    let pill_refs: Vec<(String, RefKind)> = snap
        .refs
        .iter()
        .filter(|r| r.target_oid == primary.oid && r.kind != RefKind::Tag)
        .map(|r| (r.name.clone(), r.kind))
        .collect();
    for (i, (name, kind)) in pill_refs.iter().take(4).enumerate() {
        items.push(
            ContextMenuItem::submenu(
                i18n.t_fmt("menu.log.branch_sub", &[("name", &ellipsize(name, 28))]),
                (i == 0).then_some('b'),
                branch_menu_items(state, name, *kind),
            )
            .icon(if *kind == RefKind::RemoteBranch {
                "☁"
            } else {
                "⎇"
            }),
        );
    }
    let pill_tags: Vec<String> = snap
        .refs
        .iter()
        .filter(|r| r.target_oid == primary.oid && r.kind == RefKind::Tag)
        .map(|r| r.name.clone())
        .collect();
    for tag_name in pill_tags.iter().take(4) {
        items.push(
            ContextMenuItem::submenu(
                i18n.t_fmt("menu.log.tag_sub", &[("name", &ellipsize(tag_name, 28))]),
                None,
                branch_menu_items(state, tag_name, RefKind::Tag),
            )
            .icon("🏷"),
        );
    }
    let new_branch_lbl = i18n.t("menu.log.new_branch_here");
    items.push(
        run(
            new_branch_lbl,
            Some('n'),
            new_branch_sc.as_deref(),
            dialog(new_branch_dialog(
                &primary.oid.to_string(),
                &short,
                local_names(state),
                "",
                i18n,
            )),
        )
        .icon("+")
        .enabled_if(single, need_single(new_branch_lbl)),
    );
    if crate::worktrees_view::worktrees_supported(state) {
        let new_wt_lbl = i18n.t("menu.log.new_worktree_here");
        items.push(
            run(
                new_wt_lbl,
                Some('w'),
                None,
                vec![Action::PromptNewWorktree {
                    at: Some(primary.oid),
                }],
            )
            .icon("⎇")
            .enabled_if(single, need_single(new_wt_lbl)),
        );
    }
    let tags: Vec<String> = snap.tag_names().iter().map(|t| t.to_string()).collect();
    let new_tag_lbl = i18n.t("menu.log.new_tag_here");
    items.push(
        run(
            new_tag_lbl,
            Some('t'),
            None,
            dialog(Dialog::Input(
                InputDialog::new(
                    i18n.t_fmt("menu.log.new_tag_title", &[("short", &short)]),
                    i18n.t("dialog.new_tag.name_prompt"),
                    "",
                    InputValidation::RefName {
                        what: "Tag",
                        existing: tags,
                    },
                    InputIntent::NewTag {
                        target: primary.oid,
                    },
                )
                .with_checkbox(i18n.t("dialog.new_tag.annotated_checkbox"), false)
                .with_secondary_checkbox(i18n.t("dialog.new_tag.sign_checkbox"), false),
            )),
        )
        .icon("🏷")
        .enabled_if(single, need_single(new_tag_lbl)),
    );
    let bisect_bad_lbl = if state.is_bisecting() {
        i18n.t("menu.log.bisect_mark_bad")
    } else {
        i18n.t("menu.log.bisect_start_bad")
    };
    let bisect_bad_actions = if state.is_bisecting() {
        op(GitOp::BisectMark {
            good: false,
            rev: Some(primary.oid.to_string()),
        })
    } else {
        dialog(Dialog::Choice(ChoiceDialog::confirm(
            i18n.t("dialog.bisect_start.title"),
            vec![i18n.t_fmt(
                "dialog.bisect_start.body_bad",
                &[("short", &short), ("subject", &ellipsize(&primary.subject, 48))],
            )],
            i18n.t("dialog.bisect_start.confirm_btn"),
            i18n.cancel(),
            op(GitOp::BisectStart {
                bad: Some(primary.oid.to_string()),
                good: Vec::new(),
            }),
        )))
    };
    items.push(
        run(bisect_bad_lbl, None, None, bisect_bad_actions)
            .enabled_if(single, need_single(bisect_bad_lbl))
            .enabled_if(
                state.ongoing_kind().is_none(),
                i18n.t("menu.log.finish_ongoing_first"),
            ),
    );
    let bisect_good_lbl = if state.is_bisecting() {
        i18n.t("menu.log.bisect_mark_good")
    } else {
        i18n.t("menu.log.bisect_start_good")
    };
    let bisect_good_actions = if state.is_bisecting() {
        op(GitOp::BisectMark {
            good: true,
            rev: Some(primary.oid.to_string()),
        })
    } else {
        dialog(Dialog::Choice(ChoiceDialog::confirm(
            i18n.t("dialog.bisect_start.title"),
            vec![i18n.t_fmt(
                "dialog.bisect_start.body_good",
                &[("short", &short), ("subject", &ellipsize(&primary.subject, 48))],
            )],
            i18n.t("dialog.bisect_start.confirm_btn"),
            i18n.cancel(),
            op(GitOp::BisectStart {
                bad: Some("HEAD".to_string()),
                good: vec![primary.oid.to_string()],
            }),
        )))
    };
    items.push(
        run(bisect_good_lbl, None, None, bisect_good_actions)
            .enabled_if(single, need_single(bisect_good_lbl))
            .enabled_if(
                state.ongoing_kind().is_none(),
                i18n.t("menu.log.finish_ongoing_first"),
            ),
    );
    items.push(ContextMenuItem::separator());

    // ── Utilities & navigation ──
    items.push(run(
        i18n.t("menu.log.copy_revision"),
        Some('y'),
        copy_sc.as_deref(),
        vec![Action::CopyText(
            oids.iter()
                .rev()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(" "),
        )],
    ));
    items.push(ContextMenuItem::from_command(
        i18n.t("menu.log.copy_commit_message"),
        None,
        catalog::shortcut_for_action(state, &[KeymapScope::Log], &KeyAction::CopyCommitMessage)
            .as_deref(),
        catalog::commit_copy_message(state),
    ));
    items.push(ContextMenuItem::from_command(
        i18n.t("menu.log.copy_row_summary"),
        None,
        catalog::shortcut_for_action(state, &[KeymapScope::Log], &KeyAction::CopyRowSummary)
            .as_deref(),
        catalog::commit_copy_row_summary(state),
    ));
    let mut newest_first = oids.clone();
    newest_first.reverse();
    let oldest_parent = oids
        .first()
        .and_then(|o| snap.commit(o))
        .and_then(|c| c.parents.first().copied());
    let commits_patch_source = PatchSource::Commits {
        oids: newest_first,
        base: oldest_parent,
    };
    items.push(ContextMenuItem::submenu(
        i18n.t("menu.log.create_patch"),
        Some('a'),
        vec![
            run(
                i18n.t("menu.log.save_patch_files"),
                Some('s'),
                None,
                dialog(crate::dialogs::create_patch_dialog(
                    state,
                    commits_patch_source.clone(),
                    false,
                )),
            ),
            run(
                i18n.t("menu.log.copy_patch_clipboard"),
                Some('c'),
                None,
                op(GitOp::PatchToClipboard {
                    source: commits_patch_source,
                    reverse: false,
                }),
            ),
        ],
    ));
    items.push(ContextMenuItem::from_command(
        i18n.t("menu.log.browse_revision"),
        Some('b'),
        catalog::shortcut_for_action(state, &[KeymapScope::Log], &KeyAction::BrowseRevision)
            .as_deref(),
        catalog::browse_revision_command(state),
    ));
    items.push(run(
        i18n.t("menu.log.goto_revision"),
        Some('g'),
        goto_sc.as_deref(),
        vec![Action::OpenPopup {
            kind: PopupKind::GoToRevision,
            initial_query: String::new(),
        }],
    ));
    items.push(run(i18n.t("menu.log.go_to_child"), None, Some("←"), vec![Action::GoToChild]));
    items.push(run(
        i18n.t("menu.log.go_to_parent"),
        None,
        Some("→"),
        vec![Action::GoToParentSmart],
    ));
    items.push(run(
        i18n.t("menu.log.maximize_commit_message"),
        None,
        catalog::shortcut_for_action(state, &[KeymapScope::Log], &KeyAction::MaximizeCommitMessage)
            .as_deref(),
        vec![Action::OpenCommitMessageModal],
    ));
    items.push(run(
        i18n.t("menu.log.show_reflog"),
        None,
        catalog::shortcut_for_action(state, &[KeymapScope::Log], &KeyAction::OpenReflog).as_deref(),
        vec![Action::OpenReflog("HEAD".to_string())],
    ));
    // Fold / unfold at the selected commit: expanding wins when something
    // hangs collapsed below it.
    let expand = catalog::graph_expand_selected(state);
    if expand.is_ok() {
        items.push(ContextMenuItem::from_command(
            i18n.t("menu.log.expand_linear_branch"),
            Some('x'),
            catalog::shortcut_for_action(
                state,
                &[KeymapScope::Log],
                &KeyAction::ExpandLinearBranch,
            )
            .as_deref(),
            expand,
        ));
    } else {
        items.push(ContextMenuItem::from_command(
            i18n.t("menu.log.collapse_linear_branch"),
            Some('l'),
            catalog::shortcut_for_action(
                state,
                &[KeymapScope::Log],
                &KeyAction::CollapseLinearBranch,
            )
            .as_deref(),
            catalog::graph_collapse_selected(state),
        ));
    }
    items.push(ContextMenuItem::submenu(
        i18n.t("menu.log.presentation"),
        Some('o'),
        crate::input::presentation_menu_items(state),
    ));
    if snap.has_more_commits {
        items.push(
            run(
                i18n.t("menu.log.load_more"),
                Some('l'),
                None,
                vec![Action::LoadMoreCommits],
            )
            .enabled_if(
                !state.loading_more_commits,
                i18n.t("view.log.loading_more_btn"),
            ),
        );
    }
    if snap.is_shallow {
        items.push(
            run(
                i18n.t("menu.log.unshallow"),
                Some('u'),
                None,
                vec![Action::PromptUnshallow],
            )
            .enabled_if(
                snap.default_remote().is_some(),
                i18n.t("toast.no_remote_for_unshallow"),
            ),
        );
    }
    items
}

fn squash_item(state: &AppState, oids: &[Oid]) -> ContextMenuItem {
    let i18n = &state.i18n;
    let count_s = oids.len().to_string();
    let label = if oids.len() >= 2 {
        i18n.t_fmt("menu.log.squash_many", &[("count", &count_s)])
    } else {
        i18n.t("menu.log.squash_commits").to_string()
    };
    let base = run(label.clone(), Some('s'), None, Vec::new());
    if oids.len() < 2 {
        return base.enabled_if(false, i18n.t("menu.log.squash_need_two"));
    }
    let plan = match state.rewrite_plan(oids[0], "squash") {
        Ok(p) => p,
        Err(e) => return base.enabled_if(false, e),
    };
    let Some(sq) = plan_squash_todo(&plan, oids) else {
        return base.enabled_if(false, i18n.t("menu.log.squash_not_reachable"));
    };
    ContextMenuItem::run(
        label,
        Some('s'),
        None,
        dialog(Dialog::Message(MessageDialog::new(
            i18n.t_fmt("menu.log.squash_title", &[("count", &count_s)]),
            sq.combined_message,
            MessageIntent::Rewrite {
                base: sq.base,
                todo: sq.todo,
                entry: sq.message_entry_idx,
            },
        ))),
    )
}

fn drop_item(state: &AppState, oids: &[Oid]) -> ContextMenuItem {
    let i18n = &state.i18n;
    let drop_sc = catalog::all_shortcuts_for_action(state, &[KeymapScope::Log], &KeyAction::DropCommit);
    let count_s = oids.len().to_string();
    let label = if oids.len() > 1 {
        i18n.t_fmt("menu.log.drop_many", &[("count", &count_s)])
    } else {
        i18n.t("menu.log.drop_commit").to_string()
    };
    let item = ContextMenuItem::from_command(
        label,
        Some('x'),
        drop_sc.as_deref(),
        catalog::commit_drop(state, oids),
    );
    if item.disabled_reason.is_none() {
        item.icon("✕")
    } else {
        item
    }
}

/// Right-click on the Log diff preview.
pub fn log_diff_menu_items(state: &AppState) -> Vec<ContextMenuItem> {
    let i18n = &state.i18n;
    let sbs_sc = catalog::shortcut_for_action(state, &[KeymapScope::Diff], &KeyAction::ToggleSideBySide);
    let ctx_sc = catalog::shortcut_for_action(state, &[KeymapScope::Diff], &KeyAction::CycleDiffContext);
    let ws_sc = catalog::shortcut_for_action(state, &[KeymapScope::Diff], &KeyAction::CycleDiffWhitespace);
    let wrap_sc = catalog::shortcut_for_action(state, &[KeymapScope::Diff], &KeyAction::ToggleDiffSoftWrap);
    let zoom_sc = catalog::shortcut_for_action(state, &[KeymapScope::Diff], &KeyAction::ZoomDiff);
    let ann_sc = catalog::shortcut_for_action(state, &[KeymapScope::Log], &KeyAction::Annotate);
    let selected_change = state.log.details.selected_change(&state.diff_caches);
    let change = selected_change.map(|c| c.path.clone());
    let is_deleted = selected_change.is_some_and(|c| c.kind == FileStatusKind::Deleted);
    let oids = state
        .log
        .details
        .changes
        .current_key
        .as_ref()
        .map(|k| k.oids.clone())
        .unwrap_or_default();
    let single_oid = match oids.as_slice() {
        [one] => Some(*one),
        _ => None,
    };
    let patch_source = match (oids.as_slice(), &change) {
        ([one], Some(p)) => Some(PatchSource::CommitFile {
            oid: *one,
            path: p.clone(),
        }),
        _ => None,
    };
    let ann_actions = match (single_oid, &change) {
        (Some(oid), Some(p)) if !is_deleted => vec![Action::OpenAnnotate {
            path: p.clone(),
            rev: Some(oid),
        }],
        _ => Vec::new(),
    };
    let mut items = vec![
        run(
            i18n.t("menu.log_change.annotate"),
            Some('a'),
            ann_sc.as_deref(),
            ann_actions,
        )
        .enabled_if(change.is_some(), i18n.t("menu.log_change.need_file"))
        .enabled_if(single_oid.is_some(), i18n.t("menu.log_change.need_single_commit"))
        .enabled_if(!is_deleted, i18n.t("menu.log_change.cannot_annotate_deleted")),
        run(
            i18n.t("menu.log.toggle_side_by_side"),
            Some('s'),
            sbs_sc.as_deref(),
            vec![Action::DiffToggleSideBySide(DiffScope::Log)],
        ),
        run(
            i18n.t("menu.log.cycle_context"),
            Some('c'),
            ctx_sc.as_deref(),
            vec![Action::DiffCycleContext(DiffScope::Log)],
        ),
        run(
            i18n.t("menu.log.cycle_whitespace"),
            Some('b'),
            ws_sc.as_deref(),
            vec![Action::DiffCycleWhitespace(DiffScope::Log)],
        ),
        run(
            i18n.t("menu.log.toggle_soft_wrap"),
            Some('w'),
            wrap_sc.as_deref(),
            vec![Action::DiffToggleSoftWrap(DiffScope::Log)],
        ),
        run(
            i18n.t("menu.log.copy_patch_file"),
            Some('p'),
            None,
            patch_source
                .as_ref()
                .map(|source| {
                    op(GitOp::PatchToClipboard {
                        source: source.clone(),
                        reverse: false,
                    })
                })
                .unwrap_or_default(),
        )
        .enabled_if(patch_source.is_some(), i18n.t("menu.log.copy_patch_need_file")),
        run(
            i18n.t("menu.log.fullscreen_diff"),
            Some('z'),
            zoom_sc.as_deref(),
            vec![Action::ZoomDiff(!state.log.details.diff.zoomed)],
        ),
    ];
    if state.log.details.is_comparing_versions() {
        let swap_sc = catalog::shortcut_for_action(
            state,
            &[KeymapScope::Diff, KeymapScope::Log],
            &KeyAction::SwapCompareDirection,
        );
        items.push(run(
            i18n.t("menu.log.swap_compare_direction"),
            Some('x'),
            swap_sc.as_deref(),
            vec![Action::SwapCompareDirection],
        ));
    }
    items
}

/// Right-click / keyboard menu for `[2 Log]` Changes tree (`PaneId::LogChangesTree`).
pub fn log_change_menu_items(state: &AppState) -> Vec<ContextMenuItem> {
    let i18n = &state.i18n;
    let path_filter_active = !state.log.filter.spec.paths.is_empty();
    let only_affected_item = || {
        let check = if state.log.presentation.show_only_affected_changes {
            "✓"
        } else {
            " "
        };
        let sc = catalog::shortcut_for_action(
            state,
            &[KeymapScope::Log],
            &KeyAction::ToggleOnlyAffectedChanges,
        );
        run(
            format!("{} {}", check, i18n.t("presentation.show_only_affected_changes")),
            None,
            sc.as_deref(),
            vec![Action::TogglePresentation(
                crate::log_view::PresentationToggle::ShowOnlyAffectedChanges,
            )],
        )
    };
    let swap_item = || {
        let swap_sc = catalog::shortcut_for_action(
            state,
            &[KeymapScope::Log, KeymapScope::Diff],
            &KeyAction::SwapCompareDirection,
        );
        run(
            i18n.t("menu.log.swap_compare_direction"),
            Some('x'),
            swap_sc.as_deref(),
            vec![Action::SwapCompareDirection],
        )
    };

    let Some(bundle) = state.log.details.current_bundle(&state.diff_caches) else {
        return if state.log.details.many_changes_blocked.is_some() {
            vec![run(
                i18n.t("menu.log_change.load_many_changes"),
                Some('l'),
                Some("Enter"),
                vec![Action::ConfirmLoadManyChanges],
            )]
        } else {
            Vec::new()
        };
    };
    let rows = state.log.details.change_rows(&state.diff_caches);
    let Some(row) = rows.get(state.log.details.changes.selected) else {
        let mut fallback = Vec::new();
        if path_filter_active {
            fallback.push(only_affected_item());
        }
        if state.log.details.is_comparing_versions() {
            fallback.push(swap_item());
        }
        return fallback;
    };

    let file_change = match row.kind {
        ChangeRowKind::File { change_idx } => bundle.changes.get(change_idx),
        ChangeRowKind::Dir { .. } => None,
    };
    let is_file = file_change.is_some();
    let is_deleted = file_change.is_some_and(|c| c.kind == FileStatusKind::Deleted);
    let row_path = file_change.map_or_else(|| row.key.clone(), |c| c.path.clone());
    let single_oid = (bundle.key.mode == DetailsMode::Commits && bundle.key.oids.len() == 1)
        .then(|| bundle.key.oids[0]);
    let no_ongoing = state.require_no_ongoing();

    let exists_in_workdir = is_file
        && match state.stage_workbench.status().map(std::sync::Arc::as_ref) {
            Some(st)
                if st
                    .unstaged
                    .iter()
                    .any(|e| e.path == row_path && e.kind == FileStatusKind::Deleted) =>
            {
                false
            }
            Some(st) if st.untracked.iter().any(|e| e.path == row_path) => true,
            _ => file_change.is_some_and(|c| c.exists_in_workdir),
        };

    let i18n = &state.i18n;
    let diff_sc = catalog::shortcut_for_action(state, &[KeymapScope::Log], &KeyAction::CompareOrDiff);
    let zoom_sc = catalog::shortcut_for_action(
        state,
        &[KeymapScope::Log, KeymapScope::Diff],
        &KeyAction::ZoomDiff,
    );
    let hist_sc = catalog::shortcut_for_action(state, &[KeymapScope::Log], &KeyAction::FileHistory);
    let ann_sc = catalog::shortcut_for_action(state, &[KeymapScope::Log], &KeyAction::Annotate);

    let need_file = i18n.t("menu.log_change.need_file");
    let need_single = i18n.t("menu.log_change.need_single_commit");

    let mut items = Vec::new();

    // ── View & compare ──
    items.push(
        run(
            i18n.t("menu.log_change.toggle_diff"),
            Some('d'),
            diff_sc.as_deref(),
            vec![Action::ToggleDiffPreview],
        )
        .enabled_if(is_file, need_file),
    );
    items.push(
        run(
            i18n.t("menu.log_change.fullscreen_diff"),
            Some('z'),
            zoom_sc.as_deref(),
            vec![Action::ZoomDiff(true)],
        )
        .enabled_if(is_file, need_file),
    );
    let cmp_local_actions = single_oid
        .filter(|_| is_file)
        .map(|target| {
            vec![Action::CompareFileWithLocal {
                target,
                path: row_path.clone(),
            }]
        })
        .unwrap_or_default();
    items.push(
        run(
            i18n.t("menu.log_change.compare_local"),
            Some('l'),
            None,
            cmp_local_actions,
        )
        .enabled_if(is_file, need_file)
        .enabled_if(single_oid.is_some(), need_single),
    );
    items.push(ContextMenuItem::separator());

    // ── History & filter ──
    if is_file {
        items.push(run(
            i18n.t("menu.log_change.show_file_history"),
            Some('h'),
            hist_sc.as_deref(),
            vec![Action::OpenFileHistory {
                path: row_path.clone(),
                start_oid: single_oid,
                follow_renames: true,
                line_range: None,
            }],
        ));
        items.push(ContextMenuItem::from_command(
            i18n.t("menu.log_change.annotate"),
            Some('a'),
            ann_sc.as_deref(),
            catalog::annotate_log_change(state),
        ));
    } else {
        items.push(run(
            i18n.t("menu.log_change.show_dir_history"),
            Some('h'),
            hist_sc.as_deref(),
            vec![Action::ShowDirectoryHistory(row_path.clone())],
        ));
    }
    items.push(run(
        i18n.t("menu.log_change.filter_by_path"),
        Some('f'),
        None,
        vec![Action::FilterLogByPath(row_path.clone())],
    ));
    if path_filter_active {
        items.push(only_affected_item());
    }
    if state.log.details.is_comparing_versions() {
        items.push(swap_item());
    }
    items.push(ContextMenuItem::separator());

    // ── Restore & edit ──
    let restore_actions = match single_oid {
        Some(oid) if is_file && !is_deleted => {
            let short = oid.short();
            let body_lines: Vec<String> = i18n
                .t_fmt(
                    "dialog.restore_file.body",
                    &[("path", &row_path), ("short", &short)],
                )
                .split('\n')
                .map(str::to_string)
                .collect();
            dialog(Dialog::Choice(ChoiceDialog::confirm(
                i18n.t("dialog.restore_file.title"),
                body_lines,
                i18n.t("dialog.restore_file.confirm_btn"),
                i18n.cancel(),
                op(GitOp::CheckoutPaths {
                    rev: oid.to_string(),
                    paths: vec![row_path.clone()],
                }),
            )))
        }
        _ => Vec::new(),
    };
    items.push(
        run(
            i18n.t("menu.log_change.restore_version"),
            Some('g'),
            None,
            restore_actions,
        )
        .enabled_if(is_file, need_file)
        .enabled_if(single_oid.is_some(), need_single)
        .enabled_if(!is_deleted, i18n.t("menu.log_change.cannot_restore_deleted"))
        .require(no_ongoing),
    );
    items.push(
        run(
            i18n.t("menu.log_change.open_editor"),
            Some('e'),
            None,
            vec![Action::OpenExternalEditorFile(Some(row_path.clone()))],
        )
        .enabled_if(is_file, need_file)
        .enabled_if(exists_in_workdir, i18n.t("menu.log_change.not_in_workdir")),
    );
    items.push(ContextMenuItem::separator());

    // ── Clipboard ──
    items.push(run(
        i18n.t("menu.log_change.copy_path"),
        Some('c'),
        None,
        vec![Action::CopyText(row_path.clone())],
    ));
    let patch_actions = match single_oid {
        Some(oid) if is_file => op(GitOp::PatchToClipboard {
            source: PatchSource::CommitFile {
                oid,
                path: row_path,
            },
            reverse: false,
        }),
        _ => Vec::new(),
    };
    items.push(
        run(
            i18n.t("menu.log_change.copy_patch"),
            Some('p'),
            None,
            patch_actions,
        )
        .enabled_if(is_file, need_file)
        .enabled_if(single_oid.is_some(), need_single),
    );

    items
}

/// Right-click / keyboard menu inside `ModalLayer::Annotate`.
pub fn annotate_menu_items(state: &AppState) -> Vec<ContextMenuItem> {
    let Some(ann) = state.modal_stack.annotate() else {
        return Vec::new();
    };
    let i18n = &state.i18n;
    let cmd_item = |label: String, mnemonic: Option<char>, cmd: catalog::AnnotateCommand| {
        let sc = cmd.shortcut();
        ContextMenuItem::from_command(
            label,
            mnemonic,
            (!sc.is_empty()).then_some(sc),
            catalog::evaluate_annotate_command(state, cmd),
        )
    };
    let check = |on: bool, label: &str| format!("{} {label}", if on { "✓" } else { " " });
    let moves_label = format!(
        "{} ({})",
        i18n.t("menu.annotate.detect_moves"),
        ann.key.options.move_detection.badge()
    );
    let color_label = match ann.color_mode {
        crate::log_view::AnnotateColorMode::Age => i18n.t("menu.annotate.color_age"),
        crate::log_view::AnnotateColorMode::Author => i18n.t("menu.annotate.color_author"),
        crate::log_view::AnnotateColorMode::None => i18n.t("menu.annotate.color_none"),
    };

    vec![
        cmd_item(
            i18n.t("menu.annotate.selection_history").into(),
            Some('h'),
            catalog::AnnotateCommand::SelectionHistory,
        ),
        cmd_item(
            i18n.t("menu.annotate.this_rev").into(),
            Some('r'),
            catalog::AnnotateCommand::ThisRev,
        ),
        cmd_item(
            i18n.t("menu.annotate.prev_rev").into(),
            Some('p'),
            catalog::AnnotateCommand::PreviousRev,
        ),
        cmd_item(
            i18n.t("menu.annotate.toggle_diff").into(),
            Some('d'),
            catalog::AnnotateCommand::ToggleDiff,
        ),
        cmd_item(
            check(
                ann.show_commit_details,
                i18n.t("menu.annotate.commit_details"),
            ),
            Some('i'),
            catalog::AnnotateCommand::ToggleCommitDetails,
        ),
        ContextMenuItem::separator(),
        cmd_item(
            check(
                ann.key.options.ignore_whitespace,
                i18n.t("menu.annotate.ignore_whitespace"),
            ),
            Some('w'),
            catalog::AnnotateCommand::IgnoreWhitespace,
        ),
        cmd_item(
            check(ann.key.options.move_detection.is_enabled(), &moves_label),
            Some('m'),
            catalog::AnnotateCommand::DetectMoves,
        ),
        cmd_item(
            check(
                ann.key.options.prefer_commit_date,
                i18n.t("menu.annotate.prefer_commit_date"),
            ),
            Some('t'),
            catalog::AnnotateCommand::ToggleDateMode,
        ),
        cmd_item(
            format!("{}: {color_label}", i18n.t("menu.annotate.color_mode")),
            Some('c'),
            catalog::AnnotateCommand::CycleColorMode,
        ),
        cmd_item(
            check(ann.show_hash, i18n.t("menu.annotate.col_hash")),
            None,
            catalog::AnnotateCommand::ToggleHashColumn,
        ),
        cmd_item(
            check(ann.show_author, i18n.t("menu.annotate.col_author")),
            None,
            catalog::AnnotateCommand::ToggleAuthorColumn,
        ),
        cmd_item(
            check(ann.show_date, i18n.t("menu.annotate.col_date")),
            None,
            catalog::AnnotateCommand::ToggleDateColumn,
        ),
        ContextMenuItem::separator(),
        cmd_item(
            i18n.t("menu.annotate.locate_log").into(),
            Some('j'),
            catalog::AnnotateCommand::LocateInLog,
        ),
        cmd_item(
            i18n.t("menu.annotate.copy_hash").into(),
            Some('y'),
            catalog::AnnotateCommand::CopyHash,
        ),
    ]
}

/// Right-click / keyboard (`.`) context menu for a row inside `ModalLayer::FileHistory`.
pub fn file_history_menu_items(state: &AppState) -> Vec<ContextMenuItem> {
    let Some(fh) = state.modal_stack.file_history() else {
        return Vec::new();
    };
    let Some(entry) = fh.selected_entry() else {
        return Vec::new();
    };
    let i18n = &state.i18n;
    let is_deleted = entry.kind == FileStatusKind::Deleted;
    let ongoing = state.ongoing_kind().is_some();
    let need_not_deleted = i18n.t("menu.log_change.deleted_in_commit");
    let need_no_ongoing = i18n.t("toast.ongoing_in_progress");
    let check = |on: bool, label: &str| format!("{} {label}", if on { "✓" } else { " " });
    let is_marked = fh.anchor == Some(fh.selected) && fh.anchor_pinned;

    vec![
        run(
            i18n.t("menu.file_history.get_version"),
            Some('g'),
            Some("R"),
            vec![Action::FileHistoryGetVersion],
        )
        .enabled_if(!is_deleted, need_not_deleted)
        .enabled_if(!ongoing, need_no_ongoing),
        run(
            check(
                fh.show_affected_files,
                i18n.t("menu.file_history.show_affected_files"),
            ),
            Some('a'),
            Some("a"),
            vec![Action::FileHistoryToggleAffectedFiles],
        ),
        run(
            i18n.t("menu.file_history.open_repo_version"),
            Some('v'),
            Some("v"),
            vec![Action::FileHistoryOpenRepoVersion],
        )
        .enabled_if(!is_deleted, need_not_deleted),
        run(
            i18n.t("menu.file_history.create_patch"),
            Some('s'),
            None,
            dialog(crate::dialogs::create_patch_dialog(
                state,
                PatchSource::CommitFile {
                    oid: entry.oid,
                    path: entry.path_at_commit.clone(),
                },
                false,
            )),
        ),
        run(
            i18n.t("menu.file_history.copy_patch"),
            Some('p'),
            Some("p"),
            vec![Action::FileHistoryCopyPatch],
        ),
        run(
            i18n.t("menu.file_history.copy_hash"),
            Some('y'),
            Some("y"),
            vec![Action::FileHistoryCopyHash],
        ),
        run(
            check(
                fh.show_commit_details,
                i18n.t("menu.file_history.toggle_commit_details"),
            ),
            Some('i'),
            Some("i"),
            vec![Action::FileHistoryToggleCommitDetails],
        ),
        ContextMenuItem::separator(),
        run(
            check(
                is_marked,
                i18n.t("menu.file_history.mark_for_compare"),
            ),
            Some('m'),
            Some("Space"),
            vec![Action::FileHistoryToggleMarkAnchor],
        ),
        run(
            check(
                fh.compare_with_local,
                i18n.t("menu.file_history.compare_local"),
            ),
            Some('l'),
            Some("l"),
            vec![Action::FileHistoryToggleCompareLocal],
        ),
        run(
            i18n.t("menu.file_history.annotate"),
            Some('b'),
            Some("b"),
            vec![Action::FileHistoryAnnotate],
        ),
        run(
            i18n.t("menu.file_history.locate_log"),
            Some('j'),
            Some("Enter"),
            vec![Action::FileHistoryJumpToLog],
        ),
    ]
}

/// Branch scope selector menu inside `ModalLayer::FileHistory`.
pub fn file_history_scope_menu_items(state: &AppState) -> Vec<ContextMenuItem> {
    let Some(fh) = state.modal_stack.file_history() else {
        return Vec::new();
    };
    let i18n = &state.i18n;
    let check = |on: bool, label: &str| format!("{} {label}", if on { "✓" } else { " " });
    let mut items = vec![
        run(
            check(
                fh.key.scope == jbgit_core::HistoryBranchScope::CurrentHead,
                i18n.t("menu.file_history.scope_head"),
            ),
            Some('h'),
            None,
            vec![Action::FileHistorySetBranchScope(
                jbgit_core::HistoryBranchScope::CurrentHead,
            )],
        ),
        run(
            check(
                fh.key.scope == jbgit_core::HistoryBranchScope::AllBranches,
                i18n.t("menu.file_history.scope_all"),
            ),
            Some('a'),
            None,
            vec![Action::FileHistorySetBranchScope(
                jbgit_core::HistoryBranchScope::AllBranches,
            )],
        ),
    ];
    let branches = super::local_names(state);
    if !branches.is_empty() {
        items.push(ContextMenuItem::separator());
        for name in branches.into_iter().take(24) {
            let active = fh.key.scope == jbgit_core::HistoryBranchScope::Branch(name.clone());
            items.push(run(
                check(active, &name),
                None,
                None,
                vec![Action::FileHistorySetBranchScope(
                    jbgit_core::HistoryBranchScope::Branch(name),
                )],
            ));
        }
    }
    let remotes: Vec<String> = state
        .repo_snapshot
        .as_deref()
        .map(|s| {
            let mut r: Vec<String> = s
                .refs
                .iter()
                .filter(|r| r.kind == jbgit_core::RefKind::RemoteBranch)
                .map(|r| r.name.clone())
                .collect();
            r.sort();
            r.dedup();
            r
        })
        .unwrap_or_default();
    if !remotes.is_empty() {
        items.push(ContextMenuItem::separator());
        for name in remotes.into_iter().take(24) {
            let active = fh.key.scope == jbgit_core::HistoryBranchScope::Branch(name.clone());
            items.push(run(
                check(active, &name),
                None,
                None,
                vec![Action::FileHistorySetBranchScope(
                    jbgit_core::HistoryBranchScope::Branch(name),
                )],
            ));
        }
    }
    items
}
