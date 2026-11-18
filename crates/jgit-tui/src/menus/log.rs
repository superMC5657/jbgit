use jgit_core::{GitOp, Oid, RefKind, TodoAction};

use crate::app::{Action, AppState, ContextMenuAction, ContextMenuItem};
use crate::dialogs::{
    ChoiceDialog, Dialog, InputDialog, InputIntent, InputValidation, MessageDialog, MessageIntent,
    RebaseTodoDialog, new_branch_dialog, reset_dialog,
};

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
    let single = n == 1;
    let short = primary.oid.short();
    let head = snap.head_oid;
    let branch = snap.head_branch.clone();
    let cur = branch.clone().unwrap_or_else(|| "HEAD".into());
    let no_ongoing = state.require_no_ongoing();
    let need_single = |what: &str| format!("{what} works on a single commit");
    let mut items = Vec::new();

    // ── Checkout & compare ──
    let locals: Vec<String> = snap
        .refs
        .iter()
        .filter(|r| r.kind == RefKind::LocalBranch && r.target_oid == primary.oid)
        .map(|r| r.name.clone())
        .collect();
    let checkout_label = format!("Checkout Revision '{short}'");
    let checkout = if locals.is_empty() || !single {
        run(
            checkout_label,
            Some('c'),
            None,
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
                    format!("Checkout '{b}'"),
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
            format!("Checkout Revision '{short}' (Detached HEAD)"),
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
            .enabled_if(single, need_single("Checkout"))
            .require(no_ongoing.clone()),
    );
    items.push(
        run(
            "Compare with Local",
            Some('d'),
            Some("d"),
            vec![Action::CompareWithLocal {
                target: primary.oid,
            }],
        )
        .enabled_if(single, need_single("Compare with Local")),
    );
    let compare_versions = match oids.as_slice() {
        [base, target] => vec![Action::CompareVersions {
            base: *base,
            target: *target,
        }],
        _ => Vec::new(),
    };
    items.push(
        run("Compare Versions", None, Some("Ctrl+D"), compare_versions)
            .enabled_if(n == 2, "Select exactly 2 commits to compare"),
    );
    items.push(ContextMenuItem::separator());

    // ── Cherry-pick & revert ──
    let in_head: Vec<bool> = oids.iter().map(|o| state.is_ancestor_of_head(*o)).collect();
    items.push(
        run(
            if single {
                "Cherry-Pick".to_string()
            } else {
                format!("Cherry-Pick {n} Commits")
            },
            Some('p'),
            Some("p"),
            op(GitOp::CherryPick { oids: oids.clone() }),
        )
        .icon("🍒")
        .enabled_if(
            !in_head.iter().any(|b| *b),
            "Cannot cherry-pick: commit is already in the current branch",
        )
        .enabled_if(head.is_some(), "No current branch")
        .require(no_ongoing.clone()),
    );
    let mut newest_first = oids.clone();
    newest_first.reverse();
    items.push(
        run(
            if single {
                "Revert Commit".to_string()
            } else {
                format!("Revert {n} Commits")
            },
            Some('v'),
            Some("v"),
            op(GitOp::Revert { oids: newest_first }),
        )
        .enabled_if(
            in_head.iter().all(|b| *b),
            "Cannot revert: commit is not in the current branch",
        )
        .require(no_ongoing.clone()),
    );
    items.push(
        run(
            "Reset Current Branch to Here…",
            Some('r'),
            Some("r"),
            dialog(reset_dialog(primary.oid, &primary.subject, &cur)),
        )
        .enabled_if(single, need_single("Reset"))
        .require(no_ongoing.clone()),
    );
    let is_head = single && head == Some(primary.oid);
    items.push(
        run("Uncommit", Some('u'), Some("u"), vec![Action::UncommitHead])
            .enabled_if(is_head, "Uncommit is only available for the HEAD commit")
            .enabled_if(
                !primary.parents.is_empty(),
                "Cannot uncommit the root commit",
            )
            .require(no_ongoing.clone()),
    );
    items.push(ContextMenuItem::separator());

    // ── History rewriting ──
    let reword = if !single {
        run("Reword Commit…", Some('w'), Some("F2"), Vec::new())
            .enabled_if(false, need_single("Reword"))
    } else if is_head {
        run(
            "Reword Commit…",
            Some('w'),
            Some("F2"),
            dialog(Dialog::Message(MessageDialog::new(
                format!("Reword {short}"),
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
                    "Reword Commit…",
                    Some('w'),
                    Some("F2"),
                    dialog(Dialog::Message(MessageDialog::new(
                        format!("Reword {short}"),
                        plan.messages[0].clone(),
                        MessageIntent::Rewrite {
                            base: plan.base,
                            todo,
                            entry: 0,
                        },
                    ))),
                )
            }
            Err(e) => run("Reword Commit…", Some('w'), Some("F2"), Vec::new()).enabled_if(false, e),
        }
    };
    items.push(reword.icon("✎"));
    items.push(squash_item(state, &oids));
    let fixup = if single {
        run(
            "Fixup…",
            Some('f'),
            Some("f"),
            op(GitOp::FixupCommit {
                target: primary.oid,
                base: primary.parents.first().copied(),
                autosquash: true,
            }),
        )
        .enabled_if(
            state.has_staged_changes(),
            "Fixup needs staged changes to fold into the commit",
        )
        .require(state.rewrite_plan(primary.oid, "fixup").map(|_| ()))
    } else {
        run("Fixup…", Some('f'), Some("f"), Vec::new()).enabled_if(false, need_single("Fixup"))
    };
    items.push(fixup);
    items.push(drop_item(state, &oids));
    let interactive = match state.rewrite_plan(primary.oid, "rebase") {
        Ok(plan) if single => run(
            "Interactive Rebase from Here…",
            Some('i'),
            Some("i"),
            dialog(Dialog::RebaseTodo(RebaseTodoDialog {
                base: plan.base,
                entries: plan.entries,
                selected: 0,
                messages: plan.messages,
            })),
        ),
        Ok(_) => run(
            "Interactive Rebase from Here…",
            Some('i'),
            Some("i"),
            Vec::new(),
        )
        .enabled_if(false, need_single("Interactive rebase")),
        Err(e) => run(
            "Interactive Rebase from Here…",
            Some('i'),
            Some("i"),
            Vec::new(),
        )
        .enabled_if(false, e),
    };
    items.push(interactive);
    let push_up_to = match &branch {
        Some(b) => run(
            "Push All up to Here…",
            None,
            None,
            vec![Action::PromptPush {
                branch: b.clone(),
                up_to: Some(primary.oid),
            }],
        )
        .enabled_if(
            state.is_ancestor_of_head(primary.oid),
            "Commit is not in the current branch",
        ),
        None => run("Push All up to Here…", None, None, Vec::new())
            .enabled_if(false, "Detached HEAD: no branch to push"),
    };
    items.push(push_up_to.enabled_if(single, need_single("Push up to here")));
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
                format!("Branch '{}'", ellipsize(name, 28)),
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
    items.push(
        run(
            "New Branch…",
            Some('n'),
            Some("Ctrl+Alt+N"),
            dialog(new_branch_dialog(
                &primary.oid.to_string(),
                &short,
                local_names(state),
                "",
            )),
        )
        .icon("+")
        .enabled_if(single, need_single("New Branch")),
    );
    let tags: Vec<String> = snap.tag_names().iter().map(|t| t.to_string()).collect();
    items.push(
        run(
            "New Tag…",
            Some('t'),
            Some("t"),
            dialog(Dialog::Input(
                InputDialog::new(
                    format!("New Tag on {short}"),
                    "Tag name:",
                    "",
                    InputValidation::RefName {
                        what: "Tag",
                        existing: tags,
                    },
                    InputIntent::NewTag {
                        target: primary.oid,
                    },
                )
                .with_checkbox("Annotated (edit message next)", false),
            )),
        )
        .icon("🏷")
        .enabled_if(single, need_single("New Tag")),
    );
    items.push(ContextMenuItem::separator());

    // ── Utilities & navigation ──
    items.push(run(
        "Copy Revision Number",
        Some('y'),
        Some("y"),
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
        "Create Patch…",
        Some('a'),
        vec![
            run(
                "Save as .patch File(s) in Repository Root",
                Some('s'),
                None,
                op(GitOp::CreatePatchFiles {
                    oids: newest_first,
                    base: oldest_parent,
                }),
            ),
            run(
                "Copy Patch to Clipboard",
                Some('c'),
                None,
                op(GitOp::PatchToClipboard { args: patch_args }),
            ),
        ],
    ));
    items.push(ContextMenuItem::action(
        "Go to Hash / Branch / Tag…",
        Some('g'),
        Some("Ctrl+G"),
        ContextMenuAction::OpenGoTo,
    ));
    items.push(run("Go to Child", None, Some("←"), vec![Action::GoToChild]));
    items.push(run(
        "Go to Parent",
        None,
        Some("→"),
        vec![Action::GoToParent(0)],
    ));
    items.push(ContextMenuItem::submenu(
        "Presentation",
        Some('o'),
        crate::input::presentation_menu_items(state),
    ));
    items
}

fn squash_item(state: &AppState, oids: &[Oid]) -> ContextMenuItem {
    let label = if oids.len() >= 2 {
        format!("Squash {} Commits…", oids.len())
    } else {
        "Squash Commits…".to_string()
    };
    let base = run(label.clone(), Some('s'), Some("s"), Vec::new());
    if oids.len() < 2 {
        return base.enabled_if(false, "Select 2 or more commits to squash");
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
        return base.enabled_if(
            false,
            "Cannot squash: commits are not all reachable from HEAD",
        );
    }
    let first = positions[0];
    let last = *positions.last().unwrap();
    if last - first + 1 != positions.len() {
        return base.enabled_if(
            false,
            "Cannot squash: selected commits must be contiguous on the same branch",
        );
    }
    let combined = plan.messages[first..=last].join("\n\n");
    let mut todo = plan.entries;
    for e in &mut todo[(first + 1)..=last] {
        e.action = TodoAction::Squash;
    }
    ContextMenuItem::run(
        label,
        Some('s'),
        Some("s"),
        dialog(Dialog::Message(MessageDialog::new(
            format!("Squash {} Commits", oids.len()),
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
    let label = if oids.len() > 1 {
        format!("Drop {} Commits", oids.len())
    } else {
        "Drop Commit".to_string()
    };
    let Some(&oldest) = oids.first() else {
        return run(label, Some('x'), Some("Del"), Vec::new())
            .enabled_if(false, "No commit selected");
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
                return run(label, Some('x'), Some("Del"), Vec::new()).enabled_if(
                    false,
                    "Cannot drop: commits are not all reachable from HEAD",
                );
            }
            let mut body = vec![format!(
                "Remove {} commit(s) from the current branch?",
                oids.len()
            )];
            body.extend(subjects);
            run(
                label,
                Some('x'),
                Some("Del"),
                dialog(Dialog::Choice(ChoiceDialog::confirm(
                    "Drop Commits",
                    body,
                    "Drop",
                    op(GitOp::RewriteHistory {
                        base: plan.base,
                        todo,
                    }),
                ))),
            )
            .icon("✕")
        }
        Err(e) => run(label, Some('x'), Some("Del"), Vec::new()).enabled_if(false, e),
    }
}

/// Right-click on the Log diff preview.
pub fn log_diff_menu_items(state: &AppState) -> Vec<ContextMenuItem> {
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
            "Toggle Unified / Side-by-Side",
            Some('s'),
            Some("s"),
            vec![Action::DiffToggleSideBySide],
        ),
        run(
            "Cycle Context Lines",
            Some('c'),
            Some("c"),
            vec![Action::DiffCycleContext],
        ),
        run(
            "Copy Patch of File",
            Some('p'),
            None,
            op(GitOp::PatchToClipboard {
                args: patch.clone(),
            }),
        )
        .enabled_if(!patch.is_empty(), "Select a single commit and a file"),
        run(
            "Full-Screen Diff",
            Some('z'),
            Some("F4"),
            vec![Action::ZoomDiff(!state.details.diff_zoomed)],
        ),
    ]
}
