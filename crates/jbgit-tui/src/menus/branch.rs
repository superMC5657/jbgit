use jbgit_core::{
    GitOp, MergeOptions, OngoingKind, OngoingStep, PullOptions, RebaseOptions, RefKind,
};

use crate::app::{Action, AppState, ContextMenuItem};
use crate::config::KeymapScope;
use crate::dialogs::{ChoiceDialog, Dialog, new_branch_dialog, new_branch_dialog_with_track};
use crate::keymap::{KeyAction, catalog};

use super::{dialog, ellipsize, local_names, op, run};

/// Per-branch action submenu (`GitBranchPopupActions`).
pub fn branch_menu_items(state: &AppState, name: &str, kind: RefKind) -> Vec<ContextMenuItem> {
    let Some(snap) = state.repo_snapshot.as_deref() else {
        return Vec::new();
    };
    let i18n = &state.i18n;
    let checkout_sc = catalog::shortcut_for_action(state, &[KeymapScope::Branch], &KeyAction::Checkout);
    let new_branch_sc = catalog::shortcut_for_action(state, &[KeymapScope::Branch], &KeyAction::NewBranch);
    let pull_sc = catalog::shortcut_for_action(state, &[KeymapScope::Global], &KeyAction::GitPull);
    let pull_dlg_sc =
        catalog::shortcut_for_action(state, &[KeymapScope::Global], &KeyAction::GitPullDialog);
    let rebase_dlg_sc =
        catalog::shortcut_for_action(state, &[KeymapScope::Global], &KeyAction::GitRebaseDialog);
    let push_sc = catalog::shortcut_for_action(state, &[KeymapScope::Global], &KeyAction::GitPush);
    let rename_sc = catalog::shortcut_for_action(state, &[KeymapScope::Branch], &KeyAction::RenameBranch);
    let delete_sc = catalog::all_shortcuts_for_action(state, &[KeymapScope::Branch], &KeyAction::DeleteBranch);
    let fav_sc = catalog::shortcut_for_action(state, &[KeymapScope::Branch], &KeyAction::ToggleFavorite);

    let current = snap.head_branch.clone();
    let cur = current.clone().unwrap_or_else(|| "HEAD".to_string());
    let no_ongoing = state.require_no_ongoing();
    let detached = current.is_none();
    let star_item = |full: &str, short: &str| {
        let fav = state.log.branches.settings.favorites.is_favorite(full, short);
        run(
            if fav {
                i18n.t("menu.branch.unmark_favorite")
            } else {
                i18n.t("menu.branch.mark_favorite")
            },
            Some('f'),
            fav_sc.as_deref(),
            vec![Action::ToggleFavoriteRef {
                full: full.to_string(),
                short: short.to_string(),
            }],
        )
        .icon("★")
    };
    let mut items = Vec::new();
    match kind {
        RefKind::LocalBranch | RefKind::Head => {
            let name = if kind == RefKind::Head {
                cur.clone()
            } else {
                name.to_string()
            };
            let info = snap.local_branch(&name);
            let is_current = current.as_deref() == Some(name.as_str());
            let full = format!("refs/heads/{name}");
            items.push(
                ContextMenuItem::from_command(
                    i18n.t("menu.branch.checkout"),
                    Some('c'),
                    checkout_sc.as_deref(),
                    catalog::branch_checkout(state, &name, kind),
                )
                .icon("⎇"),
            );
            if let Some(wt) = snap.worktree_of_branch(&name) {
                items.push(run(
                    i18n.t_fmt("menu.branch.open_in_worktree", &[("worktree", &ellipsize(&wt.name(), 24))]),
                    Some('t'),
                    None,
                    vec![Action::SwitchRepository(wt.path.clone())],
                ));
            } else if !is_current && crate::worktrees_view::worktrees_supported(state) {
                items.push(run(
                    i18n.t("menu.branch.new_worktree"),
                    Some('t'),
                    None,
                    dialog(crate::worktrees_view::new_worktree_for_branch_dialog(state, &name)),
                ));
            }
            items.push(
                run(
                    i18n.t_fmt("menu.branch.new_branch_from", &[("name", &ellipsize(&name, 24))]),
                    Some('n'),
                    new_branch_sc.as_deref(),
                    dialog(new_branch_dialog(&name, &name, local_names(state), "", i18n)),
                )
                .icon("+"),
            );
            items.push(
                run(
                    i18n.t_fmt("menu.branch.checkout_and_rebase", &[("cur", &ellipsize(&cur, 24))]),
                    Some('k'),
                    None,
                    op(GitOp::Rebase {
                        options: RebaseOptions::onto(cur.clone(), Some(name.clone())),
                    }),
                )
                .enabled_if(
                    !is_current && !detached,
                    i18n.t("menu.branch.choose_other_branch"),
                )
                .require(no_ongoing.clone()),
            );
            items.push(ContextMenuItem::separator());
            items.push(
                run(
                    i18n.t_fmt("menu.branch.compare_with", &[("cur", &ellipsize(&cur, 24))]),
                    Some('o'),
                    None,
                    vec![Action::CompareWithCurrent {
                        branch: name.clone(),
                    }],
                )
                .enabled_if(!is_current, i18n.t("menu.branch.compare_self")),
            );
            items.push(run(
                i18n.t("menu.branch.compare_with_other"),
                None,
                None,
                vec![Action::PromptCompareWithBranch { left: name.clone() }],
            ));
            items.push(run(
                i18n.t_fmt("menu.branch.show_reflog", &[("name", &ellipsize(&name, 24))]),
                None,
                catalog::shortcut_for_action(state, &[KeymapScope::Branch], &KeyAction::OpenReflog)
                    .as_deref(),
                vec![Action::OpenReflog(name.clone())],
            ));
            let diff_local = info
                .map(|r| vec![Action::CompareWithLocal { target: r.target_oid }])
                .unwrap_or_default();
            items.push(
                run(i18n.t("menu.branch.show_diff_working_tree"), Some('w'), None, diff_local)
                    .enabled_if(info.is_some(), i18n.t("menu.branch.target_not_loaded")),
            );
            items.push(ContextMenuItem::separator());
            items.push(
                run(
                    i18n.t_fmt(
                        "menu.branch.rebase_onto",
                        &[("cur", &ellipsize(&cur, 18)), ("name", &ellipsize(&name, 18))],
                    ),
                    Some('b'),
                    None,
                    op(GitOp::Rebase {
                        options: RebaseOptions::onto(name.clone(), None),
                    }),
                )
                .enabled_if(!is_current, i18n.t("menu.branch.rebase_self"))
                .require(no_ongoing.clone()),
            );
            items.push(
                run(
                    i18n.t("menu.branch.rebase_options"),
                    Some('r'),
                    rebase_dlg_sc.as_deref(),
                    vec![Action::PromptRebase {
                        onto: (!is_current).then(|| name.clone()),
                    }],
                )
                .require(no_ongoing.clone()),
            );
            items.push(
                run(
                    i18n.t_fmt(
                        "menu.branch.merge_into",
                        &[("name", &ellipsize(&name, 18)), ("cur", &ellipsize(&cur, 18))],
                    ),
                    Some('m'),
                    None,
                    op(GitOp::Merge {
                        rev: name.clone(),
                        options: MergeOptions::default(),
                    }),
                )
                .enabled_if(!is_current, i18n.t("menu.branch.merge_self"))
                .require(no_ongoing.clone()),
            );
            items.push(
                merge_options_item(state, &name, &cur)
                    .enabled_if(!is_current, i18n.t("menu.branch.merge_self"))
                    .require(no_ongoing.clone()),
            );
            items.push(ContextMenuItem::separator());
            let upstream = info.and_then(|i| i.upstream.clone());
            let update = if is_current {
                op(GitOp::Pull {
                    options: PullOptions::default(),
                })
            } else {
                match upstream.as_deref().and_then(|u| snap.split_tracking(u)) {
                    Some((remote, rb)) => op(GitOp::UpdateBranch {
                        branch: name.clone(),
                        remote: remote.to_string(),
                        remote_branch: rb.to_string(),
                    }),
                    None => Vec::new(),
                }
            };
            items.push(
                run(
                    i18n.t("menu.branch.update"),
                    Some('u'),
                    if is_current { pull_sc.as_deref() } else { None },
                    update,
                )
                    .icon("↓")
                    .enabled_if(
                        upstream.is_some(),
                        i18n.t_fmt("menu.branch.no_upstream", &[("name", &name)]),
                    )
                    .require(if is_current {
                        no_ongoing.clone()
                    } else {
                        Ok(())
                    }),
            );
            if is_current {
                items.push(
                    run(
                        i18n.t("menu.branch.pull_options"),
                        Some('l'),
                        pull_dlg_sc.as_deref(),
                        vec![Action::PromptPull],
                    )
                    .icon("↓")
                    .enabled_if(!detached, i18n.t("menu.log.detached_no_branch"))
                    .require(no_ongoing.clone()),
                );
            }
            items.push(
                run(
                    i18n.t("menu.branch.push"),
                    Some('p'),
                    if is_current { push_sc.as_deref() } else { None },
                    vec![Action::PromptPush {
                        branch: name.clone(),
                        up_to: None,
                    }],
                )
                .icon("↑"),
            );
            let has_remote_branches = snap
                .refs
                .iter()
                .any(|r| r.kind == RefKind::RemoteBranch);
            items.push(
                run(
                    i18n.t("menu.branch.set_upstream"),
                    None,
                    None,
                    vec![Action::PromptSetUpstream {
                        branch: name.clone(),
                    }],
                )
                .enabled_if(has_remote_branches, i18n.t("menu.branch.no_remote_branches")),
            );
            items.push(
                run(
                    i18n.t("menu.branch.unset_upstream"),
                    None,
                    None,
                    op(GitOp::UnsetUpstream {
                        branch: name.clone(),
                    }),
                )
                .enabled_if(
                    upstream.is_some(),
                    i18n.t_fmt("menu.branch.no_upstream", &[("name", &name)]),
                ),
            );
            items.push(ContextMenuItem::separator());
            items.push(ContextMenuItem::from_command(
                i18n.t("menu.branch.rename"),
                Some('e'),
                rename_sc.as_deref(),
                catalog::branch_rename(state, &name, kind),
            ));
            items.push(
                ContextMenuItem::from_command(
                    i18n.t("menu.branch.delete"),
                    Some('d'),
                    delete_sc.as_deref(),
                    catalog::branch_delete(state, &name, kind),
                )
                .icon("✕"),
            );
            items.push(ContextMenuItem::separator());
            items.push(star_item(&full, &name));
        }
        RefKind::RemoteBranch => {
            let (_remote, short) = snap
                .split_tracking(name)
                .unwrap_or_else(|| name.split_once('/').unwrap_or(("origin", name)));
            let full = format!("refs/remotes/{name}");
            let target_oid = snap
                .refs
                .iter()
                .find(|r| r.kind == RefKind::RemoteBranch && r.name == name)
                .map(|r| r.target_oid);
            items.push(
                ContextMenuItem::from_command(
                    i18n.t("menu.branch.checkout"),
                    Some('c'),
                    checkout_sc.as_deref(),
                    catalog::branch_checkout(state, name, RefKind::RemoteBranch),
                )
                .icon("⎇"),
            );
            items.push(
                run(
                    i18n.t_fmt("menu.branch.new_branch_from", &[("name", &ellipsize(name, 24))]),
                    Some('n'),
                    new_branch_sc.as_deref(),
                    dialog(new_branch_dialog_with_track(
                        name,
                        name,
                        local_names(state),
                        short,
                        true,
                        i18n,
                    )),
                )
                .icon("+"),
            );
            items.push(
                run(
                    i18n.t_fmt(
                        "menu.branch.checkout_and_rebase",
                        &[("cur", &ellipsize(&cur, 24))],
                    ),
                    Some('k'),
                    None,
                    vec![Action::CheckoutAndRebaseRemoteBranch {
                        remote_ref: name.to_string(),
                    }],
                )
                .enabled_if(
                    !detached && current.as_deref() != Some(short),
                    i18n.t("menu.branch.choose_other_branch"),
                )
                .require(no_ongoing.clone()),
            );
            items.push(
                run(
                    i18n.t_fmt("menu.branch.new_worktree_from", &[("name", &ellipsize(name, 24))]),
                    Some('t'),
                    None,
                    vec![Action::PromptNewWorktreeFromBranch(name.to_string())],
                )
                .icon("⎇"),
            );
            items.push(ContextMenuItem::separator());
            items.push(run(
                i18n.t_fmt("menu.branch.compare_with", &[("cur", &ellipsize(&cur, 24))]),
                Some('o'),
                None,
                vec![Action::CompareWithCurrent {
                    branch: name.to_string(),
                }],
            ));
            items.push(run(
                i18n.t("menu.branch.compare_with_other"),
                None,
                None,
                vec![Action::PromptCompareWithBranch {
                    left: name.to_string(),
                }],
            ));
            let diff_local = target_oid
                .map(|target| vec![Action::CompareWithLocal { target }])
                .unwrap_or_default();
            items.push(
                run(i18n.t("menu.branch.show_diff_working_tree"), Some('w'), None, diff_local)
                    .enabled_if(target_oid.is_some(), i18n.t("menu.branch.target_not_loaded")),
            );
            items.push(ContextMenuItem::separator());
            items.push(
                run(
                    i18n.t_fmt(
                        "menu.branch.merge_into",
                        &[("name", &ellipsize(name, 18)), ("cur", &ellipsize(&cur, 18))],
                    ),
                    Some('m'),
                    None,
                    op(GitOp::Merge {
                        rev: name.to_string(),
                        options: MergeOptions::default(),
                    }),
                )
                .require(no_ongoing.clone()),
            );
            items.push(merge_options_item(state, name, &cur).require(no_ongoing.clone()));
            items.push(
                run(
                    i18n.t_fmt(
                        "menu.branch.rebase_onto",
                        &[("cur", &ellipsize(&cur, 18)), ("name", &ellipsize(name, 22))],
                    ),
                    Some('b'),
                    None,
                    op(GitOp::Rebase {
                        options: RebaseOptions::onto(name.to_string(), None),
                    }),
                )
                .require(no_ongoing.clone()),
            );
            items.push(
                run(
                    i18n.t("menu.branch.rebase_options"),
                    Some('r'),
                    rebase_dlg_sc.as_deref(),
                    vec![Action::PromptRebase {
                        onto: Some(name.to_string()),
                    }],
                )
                .require(no_ongoing.clone()),
            );
            items.push(ContextMenuItem::separator());
            items.push(
                ContextMenuItem::from_command(
                    i18n.t("menu.branch.delete"),
                    Some('d'),
                    None,
                    catalog::branch_delete(state, name, RefKind::RemoteBranch),
                )
                .icon("✕"),
            );
            items.push(ContextMenuItem::separator());
            items.push(star_item(&full, short));
        }
        RefKind::Tag => {
            let remotes = state.available_remotes();
            items.push(
                ContextMenuItem::from_command(
                    i18n.t("menu.branch.checkout"),
                    Some('c'),
                    checkout_sc.as_deref(),
                    catalog::branch_checkout(state, name, RefKind::Tag),
                )
                .icon("⌖"),
            );
            items.push(
                run(
                    i18n.t_fmt("menu.branch.new_branch_from", &[("name", &ellipsize(name, 24))]),
                    Some('n'),
                    new_branch_sc.as_deref(),
                    dialog(new_branch_dialog(name, name, local_names(state), "", i18n)),
                )
                .icon("+"),
            );
            items.push(ContextMenuItem::separator());
            items.push(run(
                i18n.t_fmt("menu.branch.compare_with", &[("cur", &ellipsize(&cur, 24))]),
                Some('o'),
                None,
                vec![Action::CompareWithCurrent {
                    branch: name.to_string(),
                }],
            ));
            items.push(run(
                i18n.t("menu.branch.compare_with_other"),
                None,
                None,
                vec![Action::PromptCompareWithBranch {
                    left: name.to_string(),
                }],
            ));
            items.push(
                run(
                    i18n.t_fmt(
                        "menu.branch.merge_into",
                        &[("name", &ellipsize(name, 18)), ("cur", &ellipsize(&cur, 18))],
                    ),
                    Some('m'),
                    None,
                    op(GitOp::Merge {
                        rev: name.to_string(),
                        options: MergeOptions::default(),
                    }),
                )
                .require(no_ongoing.clone()),
            );
            items.push(merge_options_item(state, name, &cur).require(no_ongoing.clone()));
            items.push(
                run(
                    i18n.t("menu.branch.rebase_options"),
                    Some('b'),
                    rebase_dlg_sc.as_deref(),
                    vec![Action::PromptRebase {
                        onto: Some(name.to_string()),
                    }],
                )
                .require(no_ongoing.clone()),
            );
            items.push(ContextMenuItem::separator());

            let push_tag_dialog = |remote: &str| {
                dialog(Dialog::Choice(ChoiceDialog::confirm_with_default(
                    i18n.t("dialog.push_tag.title"),
                    vec![i18n.t_fmt(
                        "dialog.push_tag.body",
                        &[("name", name), ("remote", remote)],
                    )],
                    i18n.t("dialog.push.push_btn"),
                    i18n.cancel(),
                    false,
                    true,
                    op(GitOp::PushTag {
                        remote: remote.to_string(),
                        name: name.to_string(),
                        force: false,
                    }),
                )))
            };
            let push_item = match remotes.as_slice() {
                [] => run(i18n.t("menu.branch.push_tag"), Some('p'), None, Vec::new())
                    .enabled_if(false, i18n.t("menu.branch.no_remote_configured")),
                [only] => run(
                    i18n.t_fmt("menu.branch.push_tag_to", &[("remote", only)]),
                    Some('p'),
                    None,
                    push_tag_dialog(only),
                ),
                many => ContextMenuItem::submenu(
                    i18n.t("menu.branch.push_tag"),
                    Some('p'),
                    many.iter()
                        .map(|rm| run(rm.clone(), None, None, push_tag_dialog(rm)))
                        .collect(),
                ),
            };
            items.push(push_item.icon("↑"));

            items.push(
                ContextMenuItem::from_command(
                    i18n.t("menu.branch.delete_tag"),
                    Some('d'),
                    delete_sc.as_deref(),
                    catalog::branch_delete(state, name, RefKind::Tag),
                )
                .icon("✕"),
            );

            let del_remote_dialog = |remote: &str| {
                dialog(Dialog::Choice(ChoiceDialog::confirm(
                    i18n.t("menu.branch.delete_remote_tag_title"),
                    vec![
                        i18n.t_fmt(
                            "menu.branch.delete_remote_tag_prompt",
                            &[("short", name), ("remote", remote)],
                        ),
                        i18n.t("menu.branch.delete_remote_warn").into(),
                    ],
                    i18n.t("menu.branch.delete_remote_tag_title"),
                    i18n.cancel(),
                    op(GitOp::DeleteRemoteTag {
                        remote: remote.to_string(),
                        name: name.to_string(),
                    }),
                )))
            };
            let del_remote_item = match remotes.as_slice() {
                [] => run(
                    i18n.t("menu.branch.delete_tag_on_remote"),
                    Some('r'),
                    None,
                    Vec::new(),
                )
                .enabled_if(false, i18n.t("menu.branch.no_remote_configured")),
                [only] => run(
                    i18n.t_fmt("menu.branch.delete_tag_on_named_remote", &[("remote", only)]),
                    Some('r'),
                    None,
                    del_remote_dialog(only),
                ),
                many => ContextMenuItem::submenu(
                    i18n.t("menu.branch.delete_tag_on_remote"),
                    Some('r'),
                    many.iter()
                        .map(|rm| run(rm.clone(), None, None, del_remote_dialog(rm)))
                        .collect(),
                ),
            };
            items.push(del_remote_item.icon("✕"));

            items.push(ContextMenuItem::separator());
            items.push(run(
                i18n.t("menu.branch.copy_tag_name"),
                Some('y'),
                None,
                vec![Action::CopyText(name.to_string())],
            ));
        }
    }
    items
}

/// `Merge '<name>' into '<cur>'…` — opens the Merge options dialog.
fn merge_options_item(state: &AppState, name: &str, cur: &str) -> ContextMenuItem {
    run(
        state.i18n.t_fmt(
            "menu.branch.merge_options",
            &[("name", &ellipsize(name, 18)), ("cur", &ellipsize(cur, 18))],
        ),
        Some('g'),
        None,
        vec![Action::PromptMerge {
            rev: name.to_string(),
        }],
    )
}

/// Menu of the branches pane `Remote` category / `<remote>` group nodes.
pub fn remote_group_menu_items(state: &AppState, remote: Option<&str>) -> Vec<ContextMenuItem> {
    let i18n = &state.i18n;
    let mut items = Vec::new();
    if let Some(name) = remote.filter(|n| {
        state
            .repo_snapshot
            .as_deref()
            .is_some_and(|s| s.remote(n).is_some())
    }) {
        items.push(ContextMenuItem::run(
            i18n.t_fmt("menu.branch.fetch_remote", &[("name", name)]),
            Some('f'),
            None,
            vec![Action::RunGitOp(GitOp::FetchRemote {
                name: name.to_string(),
            })],
        ));
    }
    let sc = catalog::shortcut_for_action(
        state,
        &[crate::config::KeymapScope::Global],
        &crate::keymap::KeyAction::ConfigureRemotes,
    );
    items.push(ContextMenuItem::run(
        i18n.t("menu.branch.configure_remotes"),
        Some('c'),
        sc.as_deref(),
        vec![Action::OpenRemotes],
    ));
    items
}

pub fn ongoing_menu_items(state: &AppState, kind: OngoingKind) -> Vec<ContextMenuItem> {
    let i18n = &state.i18n;
    let kind_title = i18n.ongoing_title(kind);
    let conflicts = state.conflicted_count();
    let conflicts_s = conflicts.to_string();
    let step = |s: OngoingStep| op(GitOp::Ongoing { kind, step: s });
    let mut items = vec![
        run(
            i18n.t_fmt("menu.branch.ongoing_continue", &[("kind", kind_title)]),
            Some('c'),
            None,
            step(OngoingStep::Continue),
        )
        .icon("▶")
        .enabled_if(
            conflicts == 0,
            i18n.t_fmt("menu.branch.cannot_continue_conflicts", &[("count", &conflicts_s)]),
        ),
    ];
    if kind.can_skip() {
        items.push(run(i18n.t("menu.branch.skip_commit"), Some('s'), None, step(OngoingStep::Skip)).icon("⤼"));
    }
    items.push(
        run(
            i18n.t_fmt("menu.branch.ongoing_abort", &[("kind", kind_title)]),
            Some('a'),
            None,
            step(OngoingStep::Abort),
        )
        .icon("✕"),
    );
    items.push(ContextMenuItem::separator());
    items.push(run(
        i18n.t("menu.stage.resolve_conflicts"),
        Some('r'),
        None,
        vec![Action::OngoingResolve],
    ));
    items
}

/// Menu for a ref pill / `HEAD` marker in the Log table.
pub fn ref_menu_items(state: &AppState, ref_name: &str) -> Vec<ContextMenuItem> {
    let kind = state.repo_snapshot.as_deref().and_then(|s| {
        [RefKind::LocalBranch, RefKind::RemoteBranch, RefKind::Tag]
            .into_iter()
            .find(|k| s.refs.iter().any(|r| r.kind == *k && r.name == ref_name))
    });
    match kind {
        Some(k) => branch_menu_items(state, ref_name, k),
        None => Vec::new(),
    }
}
