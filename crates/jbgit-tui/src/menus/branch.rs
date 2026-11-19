use jbgit_core::{GitOp, OngoingKind, OngoingStep, RefKind};

use crate::app::{Action, AppState, ContextMenuItem};
use crate::dialogs::{
    ChoiceDialog, Dialog, InputDialog, InputIntent, InputValidation, new_branch_dialog,
};

use super::{dialog, ellipsize, local_names, op, run};

/// Per-branch action submenu (`GitBranchPopupActions`).
pub fn branch_menu_items(state: &AppState, name: &str, kind: RefKind) -> Vec<ContextMenuItem> {
    let Some(snap) = state.repo_snapshot.as_deref() else {
        return Vec::new();
    };
    let i18n = &state.i18n;
    let current = snap.head_branch.clone();
    let cur = current.clone().unwrap_or_else(|| "HEAD".to_string());
    let no_ongoing = state.require_no_ongoing();
    let detached = current.is_none();
    let star_item = |full: &str, short: &str| {
        let fav = state.branches.settings.favorites.is_favorite(full, short);
        run(
            if fav {
                i18n.t("menu.branch.unmark_favorite")
            } else {
                i18n.t("menu.branch.mark_favorite")
            },
            Some('f'),
            None,
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
                run(
                    i18n.t("menu.branch.checkout"),
                    Some('c'),
                    None,
                    op(GitOp::Checkout {
                        target: name.clone(),
                        detach: false,
                    }),
                )
                .icon("⎇")
                .enabled_if(!is_current, i18n.t_fmt("menu.branch.already_checked_out", &[("name", &name)]))
                .require(no_ongoing.clone()),
            );
            items.push(
                run(
                    i18n.t_fmt("menu.branch.new_branch_from", &[("name", &ellipsize(&name, 24))]),
                    Some('n'),
                    None,
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
                        onto: cur.clone(),
                        branch: Some(name.clone()),
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
                        onto: name.clone(),
                        branch: None,
                    }),
                )
                .enabled_if(!is_current, i18n.t("menu.branch.rebase_self"))
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
                    op(GitOp::Merge { rev: name.clone() }),
                )
                .enabled_if(!is_current, i18n.t("menu.branch.merge_self"))
                .require(no_ongoing.clone()),
            );
            items.push(ContextMenuItem::separator());
            let upstream = info.and_then(|i| i.upstream.clone());
            let update = if is_current {
                op(GitOp::Pull { rebase: false })
            } else {
                match upstream.as_ref().and_then(|u| u.split_once('/')) {
                    Some((remote, rb)) => op(GitOp::UpdateBranch {
                        branch: name.clone(),
                        remote: remote.to_string(),
                        remote_branch: rb.to_string(),
                    }),
                    None => Vec::new(),
                }
            };
            items.push(
                run(i18n.t("menu.branch.update"), Some('u'), None, update)
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
            items.push(
                run(
                    i18n.t("menu.branch.push"),
                    Some('p'),
                    Some("Ctrl+Shift+K"),
                    vec![Action::PromptPush {
                        branch: name.clone(),
                        up_to: None,
                    }],
                )
                .icon("↑"),
            );
            items.push(ContextMenuItem::separator());
            let mut existing = local_names(state);
            existing.retain(|n| n != &name);
            items.push(run(
                i18n.t("menu.branch.rename"),
                Some('e'),
                Some("F2"),
                dialog(Dialog::Input(InputDialog::new(
                    i18n.t_fmt("dialog.rename_branch.title", &[("name", &name)]),
                    i18n.t("dialog.rename_branch.prompt"),
                    name.clone(),
                    InputValidation::RefName {
                        what: "Branch",
                        existing,
                    },
                    InputIntent::RenameBranch { old: name.clone() },
                ))),
            ));
            items.push(
                run(
                    i18n.t("menu.branch.delete"),
                    Some('d'),
                    Some("Del"),
                    vec![Action::PromptDeleteBranch { name: name.clone() }],
                )
                .icon("✕")
                .enabled_if(!is_current, i18n.t("menu.branch.cannot_delete_current")),
            );
            items.push(ContextMenuItem::separator());
            items.push(star_item(&full, &name));
        }
        RefKind::RemoteBranch => {
            let (remote, short) = name.split_once('/').unwrap_or(("origin", name));
            let full = format!("refs/remotes/{name}");
            let target_oid = snap
                .refs
                .iter()
                .find(|r| r.kind == RefKind::RemoteBranch && r.name == name)
                .map(|r| r.target_oid);
            items.push(
                run(
                    i18n.t("menu.branch.checkout"),
                    Some('c'),
                    None,
                    vec![Action::CheckoutRemoteBranch {
                        remote_ref: name.to_string(),
                    }],
                )
                .icon("⎇")
                .require(no_ongoing.clone()),
            );
            items.push(
                run(
                    i18n.t_fmt("menu.branch.new_branch_from", &[("name", &ellipsize(name, 24))]),
                    Some('n'),
                    None,
                    dialog(new_branch_dialog(name, name, local_names(state), short, i18n)),
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
                    }),
                )
                .require(no_ongoing.clone()),
            );
            items.push(
                run(
                    i18n.t_fmt(
                        "menu.branch.rebase_onto",
                        &[("cur", &ellipsize(&cur, 18)), ("name", &ellipsize(name, 22))],
                    ),
                    Some('b'),
                    None,
                    op(GitOp::Rebase {
                        onto: name.to_string(),
                        branch: None,
                    }),
                )
                .require(no_ongoing.clone()),
            );
            items.push(ContextMenuItem::separator());
            items.push(
                run(
                    i18n.t("menu.branch.delete"),
                    Some('d'),
                    None,
                    dialog(Dialog::Choice(ChoiceDialog::confirm(
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
                    ))),
                )
                .icon("✕"),
            );
            items.push(ContextMenuItem::separator());
            items.push(star_item(&full, short));
        }
        RefKind::Tag => {
            items.push(
                run(
                    i18n.t("menu.branch.checkout"),
                    Some('c'),
                    None,
                    op(GitOp::Checkout {
                        target: name.to_string(),
                        detach: true,
                    }),
                )
                .icon("⌖")
                .require(no_ongoing.clone()),
            );
            items.push(run(
                i18n.t_fmt("menu.branch.new_branch_from", &[("name", &ellipsize(name, 24))]),
                Some('n'),
                None,
                dialog(new_branch_dialog(name, name, local_names(state), "", i18n)),
            ));
            items.push(run(
                i18n.t_fmt("menu.branch.compare_with", &[("cur", &ellipsize(&cur, 24))]),
                Some('o'),
                None,
                vec![Action::CompareWithCurrent {
                    branch: name.to_string(),
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
                    }),
                )
                .require(no_ongoing.clone()),
            );
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

pub fn ongoing_menu_items(state: &AppState, kind: OngoingKind) -> Vec<ContextMenuItem> {
    let i18n = &state.i18n;
    let conflicts = state.conflicted_count();
    let conflicts_s = conflicts.to_string();
    let step = |s: OngoingStep| op(GitOp::Ongoing { kind, step: s });
    let mut items = vec![
        run(
            i18n.t_fmt("menu.branch.ongoing_continue", &[("kind", kind.title())]),
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
            i18n.t_fmt("menu.branch.ongoing_abort", &[("kind", kind.title())]),
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
