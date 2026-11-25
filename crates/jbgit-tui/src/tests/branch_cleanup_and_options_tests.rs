//! Spec 13 #06 TUI tests: upstream management, RenameBranch unset-upstream,
//! NewBranch options, unmerged delete commit preview, Branch Cleanup modal +
//! batch Undo, two-branch symmetric compare, Checkout & Rebase remote,
//! Cherry-Pick/Revert options & EmptyCherryPick recovery, and Stash options.

use std::sync::Arc;

use crossterm::event::{KeyCode, KeyModifiers};
use jbgit_core::{
    BranchCompareDirection, CommitChangesBundle, CommitDetails, DetailsKey, DetailsMode,
    FileChange, FileStatusKind, GitFailure, GitOp, GitOpOutcome, OngoingKind, OngoingStep, RefInfo,
    RefKind, RepoSnapshot, RequestSlotId, StageEntry, StageStatusSnapshot, StashEntry,
    WorkspaceTab,
};
use jbgit_engine::EngineEvent;

use crate::app::{Action, ModalLayer};
use crate::dialogs::Dialog;
use crate::menus::{
    branch_menu_items, log_commit_menu_items, stage_tree_menu_items, stash_file_menu_items,
    stash_menu_items,
};

use super::{
    TestSession, find_item, item_run_actions, local, make_commit, oid, screen_text, snapshot,
};

fn issue_06_snapshot() -> RepoSnapshot {
    let mut snap = snapshot();
    // Add a merged branch with `[gone]` upstream (`feat/done-gone` -> c6),
    // a merged branch without upstream (`feat/done-local` -> c4),
    // an unmerged branch with commits c20, c21 (`feat/wip`),
    // a merge commit c30 merging c12 & c20, and a remote branch `origin/remote-topic`.
    let mut gone_merged = local("feat/done-gone", 6);
    gone_merged.upstream = Some("origin/feat/done-gone".into());
    snap.refs.push(gone_merged);
    snap.refs.push(local("feat/done-local", 4));

    let c20 = make_commit(20, vec![oid(5)], "wip first commit");
    let c21 = make_commit(21, vec![c20.oid], "wip second commit");
    let c30 = make_commit(30, vec![oid(12), c20.oid], "merge wip into main");
    let c20_oid = c20.oid;
    let c21_oid = c21.oid;
    let c30_oid = c30.oid;
    snap.commits.insert(0, c20);
    snap.commits.insert(0, c21);
    snap.commits.insert(0, c30);

    snap.refs.push(RefInfo {
        name: "feat/wip".into(),
        full_name: "refs/heads/feat/wip".into(),
        target_oid: c21_oid,
        kind: RefKind::LocalBranch,
        upstream: None,
        ahead_behind: None,
        is_head: false,
    });
    snap.refs.push(RefInfo {
        name: "origin/remote-topic".into(),
        full_name: "refs/remotes/origin/remote-topic".into(),
        target_oid: c20_oid,
        kind: RefKind::RemoteBranch,
        upstream: None,
        ahead_behind: None,
        is_head: false,
    });
    snap.refs.push(RefInfo {
        name: "merge-tip".into(),
        full_name: "refs/heads/merge-tip".into(),
        target_oid: c30_oid,
        kind: RefKind::LocalBranch,
        upstream: None,
        ahead_behind: None,
        is_head: false,
    });
    snap
}

#[test]
fn branch_cleanup_modal_untoggles_one_batch_deletes_and_restores_undo() {
    let mut ts = TestSession::with_snapshot(issue_06_snapshot());
    let _ = ts.take_requests();

    ts.dispatch(Action::OpenBranchCleanup);
    let initial_selected = {
        let ModalLayer::BranchCleanup(bc) = ts.state().modal_stack.last().expect("cleanup modal")
        else {
            panic!("expected BranchCleanup modal");
        };
        let names: Vec<&str> = bc.rows.iter().map(|r| r.candidate.name.as_str()).collect();
        // `feat/done-gone` (merged + [gone]) sorts before `feat/done-local` and `feature/dag-layout`.
        assert_eq!(names[0], "feat/done-gone");
        assert!(bc.rows[0].candidate.upstream_gone);
        assert!(bc.rows.iter().all(|r| r.candidate.name != "main"));
        assert!(bc.rows.iter().all(|r| r.candidate.name != "feat/wip"));
        bc.selected_count()
    };

    let rendered = screen_text(&ts.render(120, 36));
    assert!(rendered.contains("Cleanup Merged Branches"), "{rendered}");
    assert!(rendered.contains("[gone]"), "{rendered}");

    // Uncheck the first branch (`feat/done-gone`), keep remaining merged branches checked.
    assert!(initial_selected >= 2);
    ts.send_key(KeyCode::Char(' '), KeyModifiers::NONE);
    let ModalLayer::BranchCleanup(bc2) = ts.state().modal_stack.last().unwrap() else {
        panic!();
    };
    assert!(!bc2.rows[0].selected);
    assert_eq!(bc2.selected_count(), initial_selected - 1);

    // Press Enter to batch-delete remaining selected branches via GitOp::Sequence.
    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);
    assert!(ts.state().modal_stack.is_empty());
    let ops = ts.take_git_ops();
    let [GitOp::Sequence(del_ops)] = ops.as_slice() else {
        panic!("expected GitOp::Sequence delete, got {ops:?}");
    };
    assert_eq!(del_ops.len(), initial_selected - 1);
    assert!(
        del_ops
            .iter()
            .all(|o| !matches!(o, GitOp::DeleteBranch { name, .. } if name == "feat/done-gone"))
    );

    // Delivering success outcome surfaces batch Undo `[Restore N Branch(es)]` notification!
    ts.deliver_engine_event(EngineEvent::GitOpFinished(GitOpOutcome::ok(
        GitOp::Sequence(del_ops.clone()),
    )));
    let note = ts.state().notification.as_ref().expect("undo notification");
    assert!(note.text.contains("Deleted"), "{}", note.text);
    ts.dispatch(Action::NotificationActivate);
    let undo_ops = ts.take_git_ops();
    let [GitOp::Sequence(restore_ops)] = undo_ops.as_slice() else {
        panic!("expected batch restore Sequence, got {undo_ops:?}");
    };
    assert_eq!(restore_ops.len(), del_ops.len());
    assert!(
        restore_ops
            .iter()
            .all(|o| matches!(o, GitOp::RestoreBranch { .. }))
    );
}

#[test]
fn upstream_menus_rename_unset_upstream_new_branch_options_and_unmerged_preview() {
    let mut ts = TestSession::with_snapshot(issue_06_snapshot());
    let _ = ts.take_requests();

    // 1. Set Upstream… and Unset Upstream on local branch `main` (which tracks origin/main).
    let items = branch_menu_items(ts.state(), "main", RefKind::LocalBranch);
    let set_up = find_item(&items, "Set Upstream…");
    assert!(set_up.is_enabled());
    ts.dispatch_many(item_run_actions(set_up));
    assert!(matches!(
        ts.state().modal_stack.last(),
        Some(ModalLayer::Dialog(d)) if matches!(d.as_ref(), Dialog::Choice(_))
    ));
    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);
    let ops = ts.take_git_ops();
    assert!(
        matches!(ops.as_slice(), [GitOp::SetUpstream { branch, .. }] if branch == "main"),
        "{ops:?}"
    );

    let unset_up = find_item(&items, "Unset Upstream");
    assert!(unset_up.is_enabled());
    ts.dispatch_many(item_run_actions(unset_up));
    assert_eq!(
        ts.take_git_ops(),
        vec![GitOp::UnsetUpstream {
            branch: "main".into()
        }]
    );

    // 2. Rename Branch on tracked branch includes "Unset upstream branch" checkbox.
    let rename = find_item(&items, "Rename…");
    ts.dispatch_many(item_run_actions(rename));
    ts.send_key(KeyCode::Char('u'), KeyModifiers::CONTROL);
    ts.type_str("main-renamed");
    ts.send_key(KeyCode::Tab, KeyModifiers::NONE); // check "Unset upstream branch"
    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(
        ts.take_git_ops(),
        vec![GitOp::Sequence(vec![
            GitOp::RenameBranch {
                old: "main".into(),
                new: "main-renamed".into(),
            },
            GitOp::UnsetUpstream {
                branch: "main-renamed".into(),
            },
        ])]
    );

    // 3. NewBranch dialog: auto-sanitizes spaces -> `_`, supports Overwrite (-B) and Track (--track).
    ts.send_key(KeyCode::Char('n'), KeyModifiers::NONE);
    ts.type_str("my new topic");
    let Some(ModalLayer::Dialog(d)) = ts.state().modal_stack.last() else {
        panic!();
    };
    let Dialog::NewBranch(nb) = d.as_ref() else {
        panic!("expected NewBranch dialog");
    };
    assert_eq!(nb.value, "my_new_topic");
    // Now type existing branch "main", toggle Overwrite (-B) via Alt+O and Track (--track) via Alt+T.
    ts.send_key(KeyCode::Char('u'), KeyModifiers::CONTROL);
    ts.type_str("main");
    ts.send_key(KeyCode::Char('o'), KeyModifiers::ALT);
    ts.send_key(KeyCode::Char('t'), KeyModifiers::ALT);
    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);
    let ops = ts.take_git_ops();
    assert!(
        matches!(
            ops.as_slice(),
            [GitOp::CheckoutNewBranch {
                name,
                track: true,
                force: true,
                ..
            }] if name == "main"
        ),
        "{ops:?}"
    );

    // 4. Deleting unmerged `feat/wip` lists unmerged commits (short + subject) in dialog body!
    ts.dispatch(Action::PromptDeleteBranch {
        name: "feat/wip".into(),
    });
    let Some(ModalLayer::Dialog(d)) = ts.state().modal_stack.last() else {
        panic!("expected unmerged delete confirm dialog");
    };
    let Dialog::Choice(c) = d.as_ref() else {
        panic!();
    };
    let body = c.body.join("\n");
    assert!(body.contains("wip first commit"), "{body}");
    assert!(body.contains("wip second commit"), "{body}");
}

#[test]
fn checkout_rebase_remote_and_compare_any_two_branches_cycle_and_tree_diff() {
    let mut ts = TestSession::with_snapshot(issue_06_snapshot());
    let _ = ts.take_requests();

    // 1. Remote branch menu has "Checkout and Rebase onto 'main'".
    let r_items = branch_menu_items(ts.state(), "origin/remote-topic", RefKind::RemoteBranch);
    let cb_item = find_item(&r_items, "Checkout and Rebase onto 'main'");
    assert!(cb_item.is_enabled(), "{:?}", cb_item.disabled_reason);
    ts.dispatch_many(item_run_actions(cb_item));
    assert_eq!(
        ts.take_git_ops(),
        vec![GitOp::Sequence(vec![
            GitOp::CreateBranch {
                name: "remote-topic".into(),
                start: "origin/remote-topic".into(),
                force: false,
                track: true,
            },
            GitOp::Rebase {
                options: jbgit_core::RebaseOptions::onto("main", Some("remote-topic".into())),
            },
        ])]
    );

    // 2. Compare Any Two Branches (`Compare with…`) between `feat/done-local` and `feat/wip`.
    let l_items = branch_menu_items(ts.state(), "feat/done-local", RefKind::LocalBranch);
    let cmp_other = find_item(&l_items, "Compare with…");
    assert!(cmp_other.is_enabled());
    ts.dispatch(Action::CompareBranches {
        left: "feat/done-local".into(),
        right: "feat/wip".into(),
    });
    assert_eq!(
        ts.state().log.branch_compare.as_ref().unwrap().direction,
        BranchCompareDirection::Symmetric
    );
    let banner = screen_text(&ts.render(140, 36));
    assert!(banner.contains("feat/done-local...feat/wip"), "{banner}");

    // Cycle direction: Symmetric -> OnlyRight (`A..B`) -> OnlyLeft (`B..A`).
    ts.dispatch(Action::CycleBranchCompareDirection);
    assert_eq!(
        ts.state().log.branch_compare.as_ref().unwrap().direction,
        BranchCompareDirection::OnlyRight
    );
    ts.dispatch(Action::CycleBranchCompareDirection);
    assert_eq!(
        ts.state().log.branch_compare.as_ref().unwrap().direction,
        BranchCompareDirection::OnlyLeft
    );

    // Toggle Tree Diff -> sets CompareVersions DetailsKey (`git diff`).
    ts.dispatch(Action::ToggleBranchCompareTreeDiff);
    assert!(matches!(
        ts.state().log.details.compare_override,
        Some(DetailsKey {
            mode: DetailsMode::CompareVersions,
            ..
        })
    ));
}

#[test]
fn cherry_pick_revert_no_commit_mainline_picker_and_empty_cherry_pick_recovery() {
    let mut ts = TestSession::with_snapshot(issue_06_snapshot());
    let _ = ts.take_requests();

    // Row 0 is merge commit `c30` (parents: c12, c20), NOT in HEAD `main` (c12).
    ts.dispatch(Action::SelectLogRow(0));
    let items = log_commit_menu_items(ts.state());
    let cp_n = find_item(&items, "Cherry-Pick Without Committing (-n)");
    assert!(cp_n.is_enabled(), "{:?}", cp_n.disabled_reason);
    // Merge commit opens mainline parent picker (`-m 1` / `-m 2`).
    ts.dispatch_many(item_run_actions(cp_n));
    let Some(ModalLayer::Dialog(d)) = ts.state().modal_stack.last() else {
        panic!("expected mainline picker dialog");
    };
    let Dialog::Choice(c) = d.as_ref() else {
        panic!();
    };
    assert!(c.options[0].label.contains("-m 1"), "{:?}", c.options);
    assert!(c.options[1].label.contains("-m 2"), "{:?}", c.options);
    // Select parent 2 (`-m 2`).
    ts.send_key(KeyCode::Char('2'), KeyModifiers::NONE);
    let ops = ts.take_git_ops();
    assert!(
        matches!(
            ops.as_slice(),
            [GitOp::CherryPick {
                no_commit: true,
                mainline: Some(2),
                ..
            }]
        ),
        "{ops:?}"
    );

    // Row 1 is `c21` (non-merge commit on `feat/wip`): Cherry-Pick -n runs directly.
    ts.dispatch(Action::SelectLogRow(1));
    let items2 = log_commit_menu_items(ts.state());
    let cp_n2 = find_item(&items2, "Cherry-Pick Without Committing (-n)");
    ts.dispatch_many(item_run_actions(cp_n2));
    assert!(matches!(
        ts.take_git_ops().as_slice(),
        [GitOp::CherryPick {
            no_commit: true,
            mainline: None,
            ..
        }]
    ));

    // EmptyCherryPick failure opens Skip / Allow Empty / Abort recovery dialog.
    ts.deliver_engine_event(EngineEvent::GitOpFinished(GitOpOutcome::failed(
        GitOp::CherryPick {
            oids: vec![oid(20)],
            no_commit: false,
            record_origin: false,
            mainline: None,
        },
        GitFailure::EmptyCherryPick,
    )));
    let text = screen_text(&ts.render(120, 36));
    assert!(text.contains("Empty Cherry-Pick"), "{text}");
    assert!(
        text.contains("Skip") && text.contains("Allow Empty"),
        "{text}"
    );
    // Pressing 'e' chooses Allow Empty.
    ts.send_key(KeyCode::Char('e'), KeyModifiers::NONE);
    assert_eq!(ts.take_git_ops(), vec![GitOp::CherryPickAllowEmpty]);

    // Trigger again and press 's' -> Ongoing CherryPick Skip.
    ts.deliver_engine_event(EngineEvent::GitOpFinished(GitOpOutcome::failed(
        GitOp::CherryPick {
            oids: vec![oid(20)],
            no_commit: false,
            record_origin: false,
            mainline: None,
        },
        GitFailure::EmptyCherryPick,
    )));
    ts.send_key(KeyCode::Char('s'), KeyModifiers::NONE);
    assert_eq!(
        ts.take_git_ops(),
        vec![GitOp::Ongoing {
            kind: OngoingKind::CherryPick,
            step: OngoingStep::Skip,
        }]
    );
}

#[test]
fn stash_dialog_keep_index_reinstate_index_and_stash_single_file_actions() {
    let mut snap = snapshot();
    snap.stashes.push(StashEntry {
        index: 0,
        oid: oid(50),
        message: "On main: saved work".into(),
        timestamp: 1000,
    });
    let mut ts = TestSession::with_snapshot(snap);
    ts.deliver_engine_event(EngineEvent::StageStatusLoaded {
        epoch: 1,
        status: Arc::new(StageStatusSnapshot {
            unstaged: vec![StageEntry {
                path: "src/lib.rs".into(),
                old_path: None,
                kind: FileStatusKind::Modified,
            }],
            ..Default::default()
        }),
    });
    let _ = ts.take_requests();

    // 1. Stage menu `Stash Changes…` opens StashPushDialog with `--keep-index` toggle!
    ts.dispatch(Action::SwitchTab(WorkspaceTab::Commit));
    let items = stage_tree_menu_items(ts.state(), 1); // first file row under Unstaged
    let stash_item = find_item(&items, "Stash Selected Files…");
    assert!(stash_item.is_enabled(), "{:?}", stash_item.disabled_reason);
    ts.dispatch_many(item_run_actions(stash_item));
    assert!(matches!(
        ts.state().modal_stack.last(),
        Some(ModalLayer::Dialog(d)) if matches!(d.as_ref(), Dialog::StashPush(_))
    ));
    // Toggle `--keep-index` via Alt+K and confirm!
    ts.send_key(KeyCode::Char('k'), KeyModifiers::ALT);
    let dlg_text = screen_text(&ts.render(120, 36));
    assert!(dlg_text.contains("--keep-index"), "{dlg_text}");
    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);
    let ops = ts.take_git_ops();
    assert!(
        matches!(
            ops.as_slice(),
            [GitOp::StashPush {
                keep_index: true,
                paths,
                ..
            }] if paths == &vec!["src/lib.rs".to_string()]
        ),
        "{ops:?}"
    );

    // 2. Stash tab menu includes `--index` Apply/Pop and single-file Diff with Local / Apply Single File.
    ts.dispatch(Action::SwitchTab(WorkspaceTab::Stash));
    let s_items = stash_menu_items(ts.state());
    let apply_idx = find_item(&s_items, "Apply Stash (Reinstate Index --index)");
    ts.dispatch_many(item_run_actions(apply_idx));
    assert_eq!(
        ts.take_git_ops(),
        vec![GitOp::StashApply {
            index: 0,
            pop: false,
            reinstate_index: true,
        }]
    );

    // Load stash commit details so a file row is selected in Stash tab.
    let key = DetailsKey::single(oid(50));
    ts.deliver_engine_event(EngineEvent::CommitDetailsLoaded {
        slot: RequestSlotId::StashCommitDetails,
        epoch: 1,
        bundle: Arc::new(CommitChangesBundle {
            key,
            details: vec![CommitDetails {
                oid: oid(50),
                message: "On main: saved work".into(),
                ..Default::default()
            }],
            changes: vec![FileChange {
                path: "src/app.rs".into(),
                old_path: None,
                kind: FileStatusKind::Modified,
                additions: 2,
                deletions: 1,
                is_binary: false,
                exists_in_workdir: true,
                old_oid: None,
                new_oid: None,
            }],
        }),
    });

    // Row 0 is the `src/` directory header; select row 1 (`src/app.rs`).
    ts.dispatch(Action::ChangesSelect(crate::app::ChangesHost::Stash, 1));
    let f_items = stash_file_menu_items(ts.state());
    let apply_file = find_item(&f_items, "Apply Single File");
    ts.dispatch_many(item_run_actions(apply_file));
    assert_eq!(
        ts.take_git_ops(),
        vec![GitOp::CheckoutPaths {
            rev: "stash@{0}".into(),
            paths: vec!["src/app.rs".into()],
        }]
    );

    let diff_local = find_item(&f_items, "Diff with Local");
    ts.dispatch_many(item_run_actions(diff_local));
    assert_eq!(ts.state().active_tab, WorkspaceTab::Log);
    assert!(matches!(
        ts.state().log.details.compare_override,
        Some(DetailsKey {
            mode: DetailsMode::CompareWithLocal,
            ..
        })
    ));
}
