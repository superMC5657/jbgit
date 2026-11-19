use jbgit_core::{GitOp, Oid, RefKind, TodoAction};

use crate::app::{Action, AppState, ContextMenuAction, ContextMenuItem};
use crate::config::KeymapScope;
use crate::dialogs::{
    ChoiceDialog, Dialog, InputDialog, InputIntent, InputValidation, MessageDialog, MessageIntent,
    RebaseTodoDialog, new_branch_dialog, reset_dialog,
};
use crate::keymap::KeyAction;

use super::{branch_menu_items, dialog, ellipsize, local_names, op, run};

/// Full Log commit-row menu for the current selection (§3.1).
pub fn log_commit_menu_items(state: &AppState) -> Vec<ContextMenuItem> {
    let Some(snap) = state.repo_snapshot.as_deref() else {
        return Vec::new();
    };
    let Some(primary) = state.log_commit_at(state.log_graph.selected_idx).cloned() else {
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
    let km = &state.config.keymap;
    let checkout_sc = km.primary_key_for(&[KeymapScope::Log], &KeyAction::Checkout);
    let compare_sc = km.primary_key_for(&[KeymapScope::Log], &KeyAction::CompareOrDiff);
    let uncommit_sc = km.primary_key_for(&[KeymapScope::Log], &KeyAction::Uncommit);
    let reword_sc = km.primary_key_for(&[KeymapScope::Log], &KeyAction::Reword);
    let new_branch_sc = km.primary_key_for(&[KeymapScope::Log], &KeyAction::NewBranch);
    let copy_sc = km.primary_key_for(&[KeymapScope::Log], &KeyAction::CopyHash);
    let goto_sc = km.primary_key_for(&[KeymapScope::Log], &KeyAction::GotoRevision);
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
    items.push(
        run(
            if single {
                i18n.t("menu.log.cherry_pick").to_string()
            } else {
                i18n.t_fmt("menu.log.cherry_pick_many", &[("count", &n_str)])
            },
            Some('p'),
            None,
            op(GitOp::CherryPick { oids: oids.clone() }),
        )
        .icon("🍒")
        .enabled_if(
            !in_head.iter().any(|b| *b),
            i18n.t("menu.log.cherry_pick_in_head"),
        )
        .enabled_if(head.is_some(), i18n.t("menu.log.no_current_branch"))
        .require(no_ongoing.clone()),
    );
    let mut newest_first = oids.clone();
    newest_first.reverse();
    items.push(
        run(
            if single {
                i18n.t("menu.log.revert_commit").to_string()
            } else {
                i18n.t_fmt("menu.log.revert_many", &[("count", &n_str)])
            },
            Some('v'),
            None,
            op(GitOp::Revert { oids: newest_first }),
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
    let is_head = single && head == Some(primary.oid);
    items.push(
        run(
            i18n.t("menu.log.uncommit"),
            Some('u'),
            uncommit_sc.as_deref(),
            vec![Action::UncommitHead],
        )
            .enabled_if(is_head, i18n.t("menu.log.uncommit_only_head"))
            .enabled_if(
                !primary.parents.is_empty(),
                i18n.t("menu.log.uncommit_root"),
            )
            .require(no_ongoing.clone()),
    );
    items.push(ContextMenuItem::separator());

    // ── History rewriting ──
    let reword_lbl = i18n.t("menu.log.reword");
    let reword_title = i18n.t_fmt("menu.log.reword_title", &[("short", &short)]);
    let reword = if !single {
        run(reword_lbl, Some('w'), reword_sc.as_deref(), Vec::new())
            .enabled_if(false, need_single(reword_lbl))
    } else if is_head {
        run(
            reword_lbl,
            Some('w'),
            reword_sc.as_deref(),
            dialog(Dialog::Message(MessageDialog::new(
                reword_title,
                primary.full_message(),
                MessageIntent::RewordHead,
            ))),
        )
        .require(no_ongoing.clone())
    } else {
        match state.rewrite_plan(primary.oid, "reword") {
            Ok(plan) => {
                let mut todo = plan.entries.clone();
                todo[0].action = TodoAction::Reword;
                run(
                    reword_lbl,
                    Some('w'),
                    reword_sc.as_deref(),
                    dialog(Dialog::Message(MessageDialog::new(
                        reword_title,
                        plan.messages[0].clone(),
                        MessageIntent::Rewrite {
                            base: plan.base,
                            todo,
                            entry: 0,
                        },
                    ))),
                )
            }
            Err(e) => run(reword_lbl, Some('w'), reword_sc.as_deref(), Vec::new())
                .enabled_if(false, e),
        }
    };
    items.push(reword.icon("✎"));
    items.push(squash_item(state, &oids));
    let fixup_lbl = i18n.t("menu.log.fixup");
    let fixup = if single {
        run(
            fixup_lbl,
            Some('f'),
            None,
            op(GitOp::FixupCommit {
                target: primary.oid,
                base: primary.parents.first().copied(),
                autosquash: true,
            }),
        )
        .enabled_if(
            state.has_staged_changes(),
            i18n.t("menu.log.fixup_need_staged"),
        )
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
            dialog(Dialog::RebaseTodo(RebaseTodoDialog {
                base: plan.base,
                entries: plan.entries,
                selected: 0,
                messages: plan.messages,
            })),
        ),
        Ok(_) => run(rebase_lbl, Some('i'), None, Vec::new())
            .enabled_if(false, need_single(rebase_lbl)),
        Err(e) => run(rebase_lbl, Some('i'), None, Vec::new()).enabled_if(false, e),
    };
    items.push(interactive);
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
                .with_checkbox(i18n.t("dialog.new_tag.annotated_checkbox"), false),
            )),
        )
        .icon("🏷")
        .enabled_if(single, need_single(new_tag_lbl)),
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
    let mut newest_first = oids.clone();
    newest_first.reverse();
    let oldest_parent = oids
        .first()
        .and_then(|o| snap.commit(o))
        .and_then(|c| c.parents.first().copied());
    let patch_args = match oids.as_slice() {
        [one] => vec![
            "format-patch".to_string(),
            "--stdout".into(),
            "-1".into(),
            one.to_string(),
        ],
        [_, .., newest] => {
            let mut args = vec!["format-patch".to_string(), "--stdout".into()];
            match oldest_parent {
                Some(b) => args.push(format!("{b}..{newest}")),
                None => {
                    args.push("--root".into());
                    args.push(newest.to_string());
                }
            }
            args
        }
        [] => Vec::new(),
    };
    items.push(ContextMenuItem::submenu(
        i18n.t("menu.log.create_patch"),
        Some('a'),
        vec![
            run(
                i18n.t("menu.log.save_patch_files"),
                Some('s'),
                None,
                op(GitOp::CreatePatchFiles {
                    oids: newest_first,
                    base: oldest_parent,
                }),
            ),
            run(
                i18n.t("menu.log.copy_patch_clipboard"),
                Some('c'),
                None,
                op(GitOp::PatchToClipboard { args: patch_args }),
            ),
        ],
    ));
    items.push(ContextMenuItem::action(
        i18n.t("menu.log.goto_revision"),
        Some('g'),
        goto_sc.as_deref(),
        ContextMenuAction::OpenGoTo,
    ));
    items.push(run(i18n.t("menu.log.go_to_child"), None, Some("←"), vec![Action::GoToChild]));
    items.push(run(
        i18n.t("menu.log.go_to_parent"),
        None,
        Some("→"),
        vec![Action::GoToParent(0)],
    ));
    items.push(ContextMenuItem::submenu(
        i18n.t("menu.log.presentation"),
        Some('o'),
        crate::input::presentation_menu_items(state),
    ));
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
    let positions: Vec<usize> = plan
        .entries
        .iter()
        .enumerate()
        .filter(|(_, e)| oids.contains(&e.oid))
        .map(|(i, _)| i)
        .collect();
    if positions.len() != oids.len() {
        return base.enabled_if(false, i18n.t("menu.log.squash_not_reachable"));
    }
    let first = positions[0];
    let last = *positions.last().unwrap();
    if last - first + 1 != positions.len() {
        return base.enabled_if(false, i18n.t("menu.log.squash_non_contiguous"));
    }
    let combined = plan.messages[first..=last].join("\n\n");
    let mut todo = plan.entries;
    for e in &mut todo[(first + 1)..=last] {
        e.action = TodoAction::Squash;
    }
    ContextMenuItem::run(
        label,
        Some('s'),
        None,
        dialog(Dialog::Message(MessageDialog::new(
            i18n.t_fmt("menu.log.squash_title", &[("count", &count_s)]),
            combined,
            MessageIntent::Rewrite {
                base: plan.base,
                todo,
                entry: last,
            },
        ))),
    )
}

fn drop_item(state: &AppState, oids: &[Oid]) -> ContextMenuItem {
    let i18n = &state.i18n;
    let drop_sc = state
        .config
        .keymap
        .all_keys_for(&[KeymapScope::Log], &KeyAction::DropCommit)
        .join(" / ");
    let drop_sc = (!drop_sc.is_empty()).then_some(drop_sc);
    let count_s = oids.len().to_string();
    let label = if oids.len() > 1 {
        i18n.t_fmt("menu.log.drop_many", &[("count", &count_s)])
    } else {
        i18n.t("menu.log.drop_commit").to_string()
    };
    let Some(&oldest) = oids.first() else {
        return run(label, Some('x'), drop_sc.as_deref(), Vec::new())
            .enabled_if(false, i18n.t("menu.log.drop_no_selection"));
    };
    match state.rewrite_plan(oldest, "drop") {
        Ok(plan) => {
            let mut todo = plan.entries.clone();
            let mut subjects = Vec::new();
            for e in &mut todo {
                if oids.contains(&e.oid) {
                    e.action = TodoAction::Drop;
                    subjects.push(format!(
                        "  • {} {}",
                        e.oid.short(),
                        ellipsize(&e.subject, 44)
                    ));
                }
            }
            if subjects.len() != oids.len() {
                return run(label, Some('x'), drop_sc.as_deref(), Vec::new())
                    .enabled_if(false, i18n.t("menu.log.drop_not_reachable"));
            }
            let mut body = vec![i18n.t_fmt("menu.log.drop_confirm_body", &[("count", &count_s)])];
            body.extend(subjects);
            run(
                label,
                Some('x'),
                drop_sc.as_deref(),
                dialog(Dialog::Choice(ChoiceDialog::confirm(
                    i18n.t("menu.log.drop_confirm_title"),
                    body,
                    i18n.t("menu.log.drop_confirm_btn"),
                    i18n.cancel(),
                    op(GitOp::RewriteHistory {
                        base: plan.base,
                        todo,
                    }),
                ))),
            )
            .icon("✕")
        }
        Err(e) => run(label, Some('x'), drop_sc.as_deref(), Vec::new()).enabled_if(false, e),
    }
}

/// Right-click on the Log diff preview.
pub fn log_diff_menu_items(state: &AppState) -> Vec<ContextMenuItem> {
    let i18n = &state.i18n;
    let km = &state.config.keymap;
    let sbs_sc = km.primary_key_for(&[KeymapScope::Diff], &KeyAction::ToggleSideBySide);
    let ctx_sc = km.primary_key_for(&[KeymapScope::Diff], &KeyAction::CycleDiffContext);
    let zoom_sc = km.primary_key_for(&[KeymapScope::Diff], &KeyAction::ZoomDiff);
    let change = state.details.selected_change().map(|c| c.path.clone());
    let oids = state
        .details
        .current_key
        .as_ref()
        .map(|k| k.oids.clone())
        .unwrap_or_default();
    let patch = match (oids.as_slice(), &change) {
        ([one], Some(p)) => vec![
            "show".to_string(),
            "--format=".into(),
            "--patch".into(),
            one.to_string(),
            "--".into(),
            p.clone(),
        ],
        _ => Vec::new(),
    };
    vec![
        run(
            i18n.t("menu.log.toggle_side_by_side"),
            Some('s'),
            sbs_sc.as_deref(),
            vec![Action::DiffToggleSideBySide],
        ),
        run(
            i18n.t("menu.log.cycle_context"),
            Some('c'),
            ctx_sc.as_deref(),
            vec![Action::DiffCycleContext],
        ),
        run(
            i18n.t("menu.log.copy_patch_file"),
            Some('p'),
            None,
            op(GitOp::PatchToClipboard {
                args: patch.clone(),
            }),
        )
        .enabled_if(!patch.is_empty(), i18n.t("menu.log.copy_patch_need_file")),
        run(
            i18n.t("menu.log.fullscreen_diff"),
            Some('z'),
            zoom_sc.as_deref(),
            vec![Action::ZoomDiff(!state.details.diff_zoomed)],
        ),
    ]
}
