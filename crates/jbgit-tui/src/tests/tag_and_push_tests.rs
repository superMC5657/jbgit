use std::sync::Arc;
use std::time::Duration;

use crossterm::event::{KeyCode, KeyModifiers};
use jbgit_core::{
    GitFailure, GitOp, GitOpOutcome, OutgoingBundle, OutgoingKey, PushForce, PushTagMode,
    RefInfo, RefKind, RepoSnapshot,
};
use jbgit_engine::{EngineEvent, TaskRequest};

use crate::app::{Action, ContextMenuEntryKind, ModalLayer};
use crate::dialogs::Dialog;
use crate::menus::{branch_menu_items, log_commit_menu_items};

use super::{
    TestSession, find_item, item_run_actions, local, make_commit, oid, remote, screen_text,
    snapshot, test_state,
};

fn tag_ref(name: &str, target: u8) -> RefInfo {
    RefInfo {
        name: name.into(),
        full_name: format!("refs/tags/{name}"),
        target_oid: oid(target),
        kind: RefKind::Tag,
        upstream: None,
        ahead_behind: None,
        is_head: false,
    }
}

#[test]
fn tag_menus_handle_no_remote_single_remote_multiple_remotes_and_log_submenus() {
    // 1. No remotes configured: remote tag actions disabled with reason.
    let mut main_head = local("main", 1);
    main_head.is_head = true;
    let no_remote_snap = RepoSnapshot {
        repo_name: "local-only".into(),
        head_branch: Some("main".into()),
        head_oid: Some(oid(1)),
        refs: vec![main_head.clone(), tag_ref("v1.0", 1)],
        commits: vec![make_commit(1, vec![], "init")],
        remotes: vec![],
        ..Default::default()
    };
    let state_no_remote = test_state(no_remote_snap.workdir.clone());
    let mut ts_no_remote = TestSession::new(state_no_remote);
    ts_no_remote.deliver_engine_event(EngineEvent::RepoSnapshotLoaded {
        snapshot: Arc::new(no_remote_snap),
        epoch: 1,
    });

    let items = branch_menu_items(ts_no_remote.state(), "v1.0", RefKind::Tag);
    assert_eq!(
        find_item(&items, "Push Tag").disabled_reason.as_deref(),
        Some("No remote configured")
    );
    assert_eq!(
        find_item(&items, "Delete Tag on Remote")
            .disabled_reason
            .as_deref(),
        Some("No remote configured")
    );
    assert!(find_item(&items, "Checkout").disabled_reason.is_none());
    assert!(find_item(&items, "New Branch").disabled_reason.is_none());
    assert!(find_item(&items, "Compare with").disabled_reason.is_none());
    assert!(find_item(&items, "Delete Tag").disabled_reason.is_none());
    assert!(find_item(&items, "Copy Tag Name").disabled_reason.is_none());

    // 2. Single remote (`origin`) + tag attached to HEAD commit in Log menu.
    let mut single_snap = snapshot();
    single_snap.remotes = super::remote_infos(&["origin"]);
    single_snap.refs.push(tag_ref("v1.0", 12));
    let mut ts_single = TestSession::with_snapshot(single_snap);
    let items = branch_menu_items(ts_single.state(), "v1.0", RefKind::Tag);
    let push_to = find_item(&items, "Push Tag to 'origin'");
    assert!(push_to.disabled_reason.is_none());
    let del_remote = find_item(&items, "Delete Tag on 'origin'");
    assert!(del_remote.disabled_reason.is_none());
    ts_single.dispatch_many(item_run_actions(del_remote));
    assert!(matches!(
        ts_single.state().modal_stack.last(),
        Some(ModalLayer::Dialog(d)) if matches!(d.as_ref(), Dialog::Choice(c) if c.body.iter().any(|l| l.contains("affects everyone")))
    ));
    ts_single.send_key(KeyCode::Left, KeyModifiers::NONE);
    ts_single.send_key(KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(
        ts_single.take_git_ops(),
        vec![GitOp::DeleteRemoteTag {
            remote: "origin".into(),
            name: "v1.0".into(),
        }]
    );

    // Log commit menu on c12 exposes Tag 'v1.0' submenu.
    ts_single.state_mut().log.graph.selected_idx = 0;
    let log_items = log_commit_menu_items(ts_single.state());
    let tag_sub = find_item(&log_items, "Tag 'v1.0'");
    let ContextMenuEntryKind::Submenu(sub_items) = &tag_sub.kind else {
        panic!("expected submenu for Tag 'v1.0'");
    };
    assert!(
        sub_items
            .iter()
            .any(|i| i.label.starts_with("Push Tag to 'origin'"))
    );
    assert!(sub_items.iter().any(|i| i.label == "Delete Tag"));

    // 3. Multiple remotes (`origin`, `upstream`): Push Tag / Delete Tag on Remote use submenus.
    let mut multi_snap = snapshot();
    multi_snap.remotes = super::remote_infos(&["upstream", "origin"]);
    multi_snap.refs.push(remote("upstream/main", 2));
    multi_snap.refs.push(tag_ref("v1.0", 12));
    let ts_multi = TestSession::with_snapshot(multi_snap);
    let multi_items = branch_menu_items(ts_multi.state(), "v1.0", RefKind::Tag);
    let ContextMenuEntryKind::Submenu(push_remotes) = &find_item(&multi_items, "Push Tag…").kind
    else {
        panic!("expected Push Tag submenu for multiple remotes");
    };
    let names: Vec<&str> = push_remotes.iter().map(|i| i.label.as_str()).collect();
    assert_eq!(names, vec!["origin", "upstream"]);

    let ContextMenuEntryKind::Submenu(del_remotes) =
        &find_item(&multi_items, "Delete Tag on Remote…").kind
    else {
        panic!("expected Delete Tag on Remote submenu for multiple remotes");
    };
    assert_eq!(del_remotes.len(), 2);
}

#[test]
fn delete_tag_probes_remote_checkbox_restore_notification_and_tag_exists_overwrite() {
    let mut snap = snapshot();
    snap.remotes = super::remote_infos(&["origin"]);
    snap.refs.push(tag_ref("v1.0", 12));
    let mut ts = TestSession::with_snapshot(snap);
    let _ = ts.take_requests();

    // PromptDeleteTag triggers CheckRemoteTag and opens confirmation without checkbox initially.
    ts.dispatch(Action::PromptDeleteTag {
        name: "v1.0".into(),
    });
    let check_epoch = ts
        .take_requests()
        .into_iter()
        .find_map(|(req, ep)| match req {
            TaskRequest::CheckRemoteTag { remote, tag }
                if remote == "origin" && tag == "v1.0" =>
            {
                ep
            }
            _ => None,
        })
        .expect("CheckRemoteTag request missing");

    // Remote returns exists: Some(false) -> checkbox stays hidden.
    ts.deliver_engine_event(EngineEvent::RemoteTagChecked {
        remote: "origin".into(),
        tag: "v1.0".into(),
        exists: Some(false),
        epoch: check_epoch,
    });
    match ts.state().modal_stack.last() {
        Some(ModalLayer::Dialog(d)) => match d.as_ref() {
            Dialog::Choice(c) => assert!(c.checkbox.is_none()),
            other => panic!("expected ChoiceDialog, got {other:?}"),
        },
        other => panic!("expected Dialog, got {other:?}"),
    }
    ts.send_key(KeyCode::Esc, KeyModifiers::NONE);

    // Re-open PromptDeleteTag and deliver exists: Some(true) -> checkbox appears.
    ts.dispatch(Action::PromptDeleteTag {
        name: "v1.0".into(),
    });
    let check_epoch_2 = ts
        .take_requests()
        .into_iter()
        .find_map(|(req, ep)| match req {
            TaskRequest::CheckRemoteTag { .. } => ep,
            _ => None,
        })
        .expect("second CheckRemoteTag request missing");
    ts.deliver_engine_event(EngineEvent::RemoteTagChecked {
        remote: "origin".into(),
        tag: "v1.0".into(),
        exists: Some(true),
        epoch: check_epoch_2,
    });
    match ts.state().modal_stack.last() {
        Some(ModalLayer::Dialog(d)) => match d.as_ref() {
            Dialog::Choice(c) => {
                let cb = c.checkbox.as_ref().expect("checkbox should be visible");
                assert!(!cb.checked);
                assert_eq!(cb.label, "Also delete tag on remote 'origin'");
            }
            other => panic!("expected ChoiceDialog, got {other:?}"),
        },
        other => panic!("expected Dialog, got {other:?}"),
    }

    // Space toggles checkbox on; Left selects primary action (Confirm) and Enter emits Sequence([DeleteTag, DeleteRemoteTag]).
    ts.send_key(KeyCode::Char(' '), KeyModifiers::NONE);
    ts.send_key(KeyCode::Left, KeyModifiers::NONE);
    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(
        ts.take_git_ops(),
        vec![GitOp::Sequence(vec![
            GitOp::DeleteTag {
                name: "v1.0".into(),
                tip: oid(12),
            },
            GitOp::DeleteRemoteTag {
                remote: "origin".into(),
                name: "v1.0".into(),
            },
        ])]
    );

    // DeleteTag success populates notification with Restore Tag action.
    ts.deliver_engine_event(EngineEvent::GitOpFinished(GitOpOutcome::ok(
        GitOp::DeleteTag {
            name: "v1.0".into(),
            tip: oid(12),
        },
    )));
    let notif = ts
        .state()
        .notification
        .as_ref()
        .expect("notification should be shown after deleting tag");
    assert!(notif.text.contains("v1.0"));
    assert_eq!(
        notif.action.as_ref().map(|(l, _)| l.as_str()),
        Some("Restore Tag")
    );
    ts.dispatch(Action::NotificationActivate);
    assert_eq!(
        ts.take_git_ops(),
        vec![GitOp::RestoreTag {
            name: "v1.0".into(),
            oid: oid(12),
        }]
    );

    // TagAlreadyExists opens overwrite confirmation for CreateTag & PushTag.
    ts.deliver_engine_event(EngineEvent::GitOpFinished(GitOpOutcome::failed(
        GitOp::CreateTag {
            name: "v2.0".into(),
            target: oid(12),
            message: Some("note".into()),
            force: false,
        },
        GitFailure::TagAlreadyExists,
    )));
    ts.send_key(KeyCode::Left, KeyModifiers::NONE);
    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(
        ts.take_git_ops(),
        vec![GitOp::CreateTag {
            name: "v2.0".into(),
            target: oid(12),
            message: Some("note".into()),
            force: true,
        }]
    );

    ts.deliver_engine_event(EngineEvent::GitOpFinished(GitOpOutcome::failed(
        GitOp::PushTag {
            remote: "origin".into(),
            name: "v2.0".into(),
            force: false,
        },
        GitFailure::TagAlreadyExists,
    )));
    ts.send_key(KeyCode::Left, KeyModifiers::NONE);
    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(
        ts.take_git_ops(),
        vec![GitOp::PushTag {
            remote: "origin".into(),
            name: "v2.0".into(),
            force: true,
        }]
    );
}

#[test]
fn push_dialog_preview_target_completion_up_to_protected_branches_and_retry() {
    let mut snap = snapshot();
    snap.remotes = super::remote_infos(&["origin"]);
    let commits_list = snap.commits.clone();
    let mut ts = TestSession::with_snapshot(snap);
    let _ = ts.take_requests();

    // Open Push dialog for `main`.
    ts.dispatch(Action::PromptPush {
        branch: "main".into(),
        up_to: None,
    });
    assert!(matches!(
        ts.state().modal_stack.last(),
        Some(ModalLayer::PushDialog(_))
    ));
    let (out_key, out_epoch) = ts
        .take_requests()
        .into_iter()
        .find_map(|(req, ep)| match req {
            TaskRequest::LoadOutgoing { key } => Some((key, ep.expect("epoch"))),
            _ => None,
        })
        .expect("LoadOutgoing request missing");
    assert_eq!(
        out_key,
        OutgoingKey {
            branch: "main".into(),
            remote: "origin".into(),
            remote_branch: "main".into(),
            up_to: None,
        }
    );

    // Loading state renders cleanly.
    let loading_buf = ts.render(120, 36);
    assert!(screen_text(&loading_buf).contains("Target"));

    // Deliver outgoing bundle -> populates 3 columns and debounces CommitDetails request.
    let bundle = Arc::new(OutgoingBundle {
        key: out_key,
        commits: commits_list[..3].to_vec(), // c12, c11, c10
        files_changed: 4,
        remote_exists: true,
        remote_oid: Some(oid(10)),
        truncated: false,
    });
    ts.deliver_engine_event(EngineEvent::OutgoingLoaded {
        bundle,
        epoch: out_epoch,
    });
    ts.advance_by(Duration::from_millis(150));
    assert!(ts.take_requests().iter().any(|(req, _)| matches!(
        req,
        TaskRequest::LoadCommitDetails { key, .. } if key.oids == vec![oid(12)]
    )));
    let loaded_text = screen_text(&ts.render(140, 36));
    assert!(loaded_text.contains("Outgoing Commits (3)"), "{loaded_text}");
    assert!(loaded_text.contains("Changed Files"), "{loaded_text}");
    assert!(loaded_text.contains("Loading changes…"), "{loaded_text}");
    assert!(
        loaded_text.contains("3 commit(s) · 4 changed file(s)"),
        "{loaded_text}"
    );
    assert!(loaded_text.contains("c12"), "{loaded_text}");

    // Inline target editing + Tab completion + Enter reload.
    ts.send_key(KeyCode::Char('e'), KeyModifiers::NONE);
    ts.send_key(KeyCode::Char('u'), KeyModifiers::CONTROL);
    ts.type_str("feature/d");
    ts.send_key(KeyCode::Tab, KeyModifiers::NONE);
    assert_eq!(
        ts.state().modal_stack.push_dialog().unwrap().target_draft,
        "feature/dag-layout"
    );
    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);
    assert!(ts.take_requests().iter().any(|(req, _)| matches!(
        req,
        TaskRequest::LoadOutgoing { key } if key.remote_branch == "feature/dag-layout"
    )));

    // Edit target back to protected branch `main`, cycle Force (`f`), confirm is blocked.
    ts.send_key(KeyCode::Char('e'), KeyModifiers::NONE);
    ts.send_key(KeyCode::Char('u'), KeyModifiers::CONTROL);
    ts.type_str("main");
    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);
    let _ = ts.take_requests();

    ts.send_key(KeyCode::Char('f'), KeyModifiers::NONE);
    {
        let pd = ts.state().modal_stack.push_dialog().unwrap();
        assert_eq!(pd.force, PushForce::WithLease);
        assert!(pd.is_force_blocked(&ts.state().config.jbgit.push.protected_branches));
    }
    let blocked_text = screen_text(&ts.render(120, 36));
    assert!(
        blocked_text.contains("Force push is disabled for protected branch 'main'."),
        "{blocked_text}"
    );
    ts.dispatch(Action::PushConfirm);
    assert!(matches!(
        ts.state().modal_stack.last(),
        Some(ModalLayer::PushDialog(_))
    ));
    assert!(ts.take_git_ops().is_empty());

    // Custom glob pattern `release/*` also blocks force push.
    ts.state_mut()
        .config
        .jbgit
        .push
        .protected_branches
        .push("release/*".into());
    ts.send_key(KeyCode::Char('e'), KeyModifiers::NONE);
    ts.send_key(KeyCode::Char('u'), KeyModifiers::CONTROL);
    ts.type_str("release/1.0");
    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);
    assert!(
        ts.state()
            .modal_stack
            .push_dialog()
            .unwrap()
            .is_force_blocked(&ts.state().config.jbgit.push.protected_branches)
    );
    ts.dispatch(Action::PushConfirm);
    assert!(ts.take_git_ops().is_empty());

    // Esc closes PushDialog without emitting any GitOp.
    ts.send_key(KeyCode::Esc, KeyModifiers::NONE);
    assert!(ts.state().modal_stack.push_dialog().is_none());
    assert!(ts.take_git_ops().is_empty());

    // Non-protected target allows Force push + Follow tags.
    ts.dispatch(Action::PromptPush {
        branch: "main".into(),
        up_to: None,
    });
    ts.send_key(KeyCode::Char('e'), KeyModifiers::NONE);
    ts.send_key(KeyCode::Char('u'), KeyModifiers::CONTROL);
    ts.type_str("feature/ok");
    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);
    ts.send_key(KeyCode::Char('f'), KeyModifiers::NONE);
    ts.send_key(KeyCode::Char('t'), KeyModifiers::NONE);
    ts.dispatch(Action::PushConfirm);
    assert!(ts.state().modal_stack.push_dialog().is_none());
    assert_eq!(
        ts.take_git_ops(),
        vec![GitOp::Push {
            branch: "main".into(),
            remote: "origin".into(),
            remote_branch: "feature/ok".into(),
            set_upstream: false,
            force: PushForce::WithLease,
            tags: PushTagMode::Follow,
            up_to: None,
            no_verify: false,
            expected_remote_oid: None,
        }]
    );

    // Push up to truncation: unit-check visible_commits() truncation & end-to-end Push op with up_to.
    ts.dispatch(Action::PromptPush {
        branch: "main".into(),
        up_to: Some(oid(10)),
    });
    let up_to_epoch = ts
        .take_requests()
        .into_iter()
        .find_map(|(req, ep)| match req {
            TaskRequest::LoadOutgoing { .. } => ep,
            _ => None,
        })
        .expect("LoadOutgoing epoch");
    ts.deliver_engine_event(EngineEvent::OutgoingLoaded {
        bundle: Arc::new(OutgoingBundle {
            key: OutgoingKey {
                branch: "main".into(),
                remote: "origin".into(),
                remote_branch: "main".into(),
                up_to: Some(oid(10)),
            },
            commits: commits_list[2..3].to_vec(),
            files_changed: 1,
            remote_exists: true,
            remote_oid: Some(oid(10)),
            truncated: false,
        }),
        epoch: up_to_epoch,
    });
    assert_eq!(
        ts.state()
            .modal_stack
            .push_dialog()
            .unwrap()
            .visible_commits()
            .len(),
        1
    );
    // Also verify PushDialogState::visible_commits truncates an untruncated bundle down to up_to.
    {
        let mut trunc_pd = crate::log_view::PushDialogState::new(
            "main".into(),
            vec!["origin".into()],
            0,
            "main".into(),
            false,
            Some(oid(10)),
            jbgit_core::DiffContext::default(),
        );
        trunc_pd.outgoing = Some(Arc::new(OutgoingBundle {
            key: OutgoingKey {
                branch: "main".into(),
                remote: "origin".into(),
                remote_branch: "main".into(),
                up_to: None,
            },
            commits: commits_list[..3].to_vec(), // c12, c11, c10
            files_changed: 2,
            remote_exists: true,
            remote_oid: Some(oid(10)),
            truncated: false,
        }));
        let truncated = trunc_pd.visible_commits();
        assert_eq!(truncated.len(), 1);
        assert_eq!(truncated[0].oid, oid(10));
    }
    ts.dispatch(Action::PushConfirm);
    assert_eq!(
        ts.take_git_ops(),
        vec![GitOp::Push {
            branch: "main".into(),
            remote: "origin".into(),
            remote_branch: "main".into(),
            set_upstream: false,
            force: PushForce::None,
            tags: PushTagMode::None,
            up_to: Some(oid(10)),
            no_verify: false,
            expected_remote_oid: None,
        }]
    );

    // PushRejected on protected `main`: offers Merge/Rebase & Retry, no Force Push.
    let push_main = GitOp::Push {
        branch: "main".into(),
        remote: "origin".into(),
        remote_branch: "main".into(),
        set_upstream: false,
        force: PushForce::None,
        tags: PushTagMode::None,
        up_to: None,
        no_verify: false,
        expected_remote_oid: None,
    };
    ts.deliver_engine_event(EngineEvent::GitOpFinished(GitOpOutcome::failed(
        push_main.clone(),
        GitFailure::PushRejected,
    )));
    match ts.state().modal_stack.last() {
        Some(ModalLayer::Dialog(d)) => match d.as_ref() {
            Dialog::Choice(c) => {
                let labels: Vec<&str> = c.options.iter().map(|o| o.label.as_str()).collect();
                assert!(labels.contains(&"Merge & Retry"), "{labels:?}");
                assert!(labels.contains(&"Rebase & Retry"), "{labels:?}");
                assert!(!labels.iter().any(|l| l.contains("Force")), "{labels:?}");
            }
            other => panic!("expected ChoiceDialog, got {other:?}"),
        },
        other => panic!("expected Dialog, got {other:?}"),
    }
    ts.send_key(KeyCode::Char('r'), KeyModifiers::NONE);
    assert_eq!(
        ts.take_git_ops(),
        vec![GitOp::Sequence(vec![
            GitOp::Smart(Box::new(GitOp::Sequence(vec![
                GitOp::Fetch,
                GitOp::Rebase {
                    onto: "origin/main".into(),
                    branch: None,
                },
            ]))),
            push_main.clone(),
        ])]
    );

    // PushRejected on non-protected `feature/x`: Force Push (with lease) IS offered.
    ts.deliver_engine_event(EngineEvent::GitOpFinished(GitOpOutcome::failed(
        GitOp::Push {
            branch: "main".into(),
            remote: "origin".into(),
            remote_branch: "feature/x".into(),
            set_upstream: false,
            force: PushForce::None,
            tags: PushTagMode::None,
            up_to: None,
            no_verify: false,
            expected_remote_oid: None,
        },
        GitFailure::PushRejected,
    )));
    match ts.state().modal_stack.last() {
        Some(ModalLayer::Dialog(d)) => match d.as_ref() {
            Dialog::Choice(c) => {
                assert!(
                    c.options
                        .iter()
                        .any(|o| o.label == "Force Push (with lease)")
                );
            }
            other => panic!("expected ChoiceDialog, got {other:?}"),
        },
        other => panic!("expected Dialog, got {other:?}"),
    }
    ts.send_key(KeyCode::Esc, KeyModifiers::NONE);

    // Push success notification summarizes commits + tags from porcelain output.
    let mut ok_outcome = GitOpOutcome::ok(push_main);
    ok_outcome.stdout = vec![
        "To /tmp/bare".into(),
        format!(
            " \trefs/heads/main:refs/heads/main\t{}..{}",
            oid(2).short(),
            oid(12).short()
        ),
        "*\trefs/tags/v1.0:refs/tags/v1.0\t[new tag]".into(),
    ];
    ts.deliver_engine_event(EngineEvent::GitOpFinished(ok_outcome));
    let summary_text = ts
        .state()
        .notification
        .as_ref()
        .expect("push summary notification missing")
        .text
        .clone();
    assert_eq!(summary_text, "Pushed 10 commit(s) to origin/main · 1 tag(s)");
}

#[test]
fn prompt_push_empty_remotes_slash_remote_and_push_rejected_up_to() {
    // 1. Empty remotes: no phantom "origin", loading=false, no RequestOutgoing, can_push=Err.
    let mut main_head = local("main", 12);
    main_head.is_head = true;
    let empty_snap = RepoSnapshot {
        repo_name: "local-only".into(),
        head_branch: Some("main".into()),
        head_oid: Some(oid(12)),
        refs: vec![main_head.clone()],
        commits: vec![make_commit(12, vec![], "init")],
        remotes: vec![],
        ..Default::default()
    };
    let mut ts = TestSession::with_snapshot(empty_snap);
    let _ = ts.take_requests();
    ts.dispatch(Action::PromptPush {
        branch: "main".into(),
        up_to: None,
    });
    assert!(
        ts.take_requests()
            .into_iter()
            .all(|(r, _)| !matches!(r, TaskRequest::LoadOutgoing { .. }))
    );
    let pd = ts.state().modal_stack.push_dialog().unwrap();
    assert!(pd.remotes.is_empty());
    assert!(!pd.loading);
    assert!(
        pd.can_push(&ts.state().config.jbgit.push.protected_branches, &ts.state().i18n)
            .is_err()
    );
    ts.send_key(KeyCode::Esc, KeyModifiers::NONE);

    // 2. Remote name containing `/`: split_tracking picks "team/upstream" and "feat/demo".
    let mut slash_branch = local("feat/demo", 12);
    slash_branch.is_head = true;
    slash_branch.upstream = Some("team/upstream/feat/demo".into());
    let slash_snap = RepoSnapshot {
        repo_name: "slash".into(),
        head_branch: Some("feat/demo".into()),
        head_oid: Some(oid(12)),
        refs: vec![
            slash_branch,
            remote("team/upstream/feat/demo", 10),
        ],
        commits: vec![make_commit(12, vec![oid(10)], "tip"), make_commit(10, vec![], "base")],
        remotes: super::remote_infos(&["origin", "team/upstream"]),
        ..Default::default()
    };
    let mut ts_slash = TestSession::with_snapshot(slash_snap);
    let _ = ts_slash.take_requests();
    ts_slash.dispatch(Action::PromptPush {
        branch: "feat/demo".into(),
        up_to: None,
    });
    let pd_slash = ts_slash.state().modal_stack.push_dialog().unwrap();
    assert_eq!(pd_slash.remote(), "team/upstream");
    assert_eq!(pd_slash.remote_branch, "feat/demo");

    // 3. PushRejected with older up_to: offers Fetch & Merge / Fetch & Rebase without stale up_to push.
    let mut main_snap = snapshot();
    main_snap.remotes = super::remote_infos(&["origin"]);
    let mut ts_rej = TestSession::with_snapshot(main_snap);
    ts_rej.deliver_engine_event(EngineEvent::GitOpFinished(GitOpOutcome::failed(
        GitOp::Push {
            branch: "main".into(),
            remote: "origin".into(),
            remote_branch: "main".into(),
            set_upstream: false,
            force: PushForce::None,
            tags: PushTagMode::None,
            up_to: Some(oid(10)), // older than main tip oid(12)
            no_verify: false,
            expected_remote_oid: None,
        },
        GitFailure::PushRejected,
    )));
    match ts_rej.state().modal_stack.last() {
        Some(ModalLayer::Dialog(d)) => match d.as_ref() {
            Dialog::Choice(c) => {
                let labels: Vec<&str> = c.options.iter().map(|o| o.label.as_str()).collect();
                assert!(labels.contains(&"Fetch & Merge"), "{labels:?}");
                assert!(labels.contains(&"Fetch & Rebase"), "{labels:?}");
                assert!(!labels.contains(&"Merge & Retry"), "{labels:?}");
                assert!(!labels.contains(&"Rebase & Retry"), "{labels:?}");
            }
            other => panic!("expected ChoiceDialog, got {other:?}"),
        },
        other => panic!("expected Dialog, got {other:?}"),
    }
    ts_rej.send_key(KeyCode::Char('r'), KeyModifiers::NONE);
    assert_eq!(
        ts_rej.take_git_ops(),
        vec![GitOp::Smart(Box::new(GitOp::Sequence(vec![
            GitOp::Fetch,
            GitOp::Rebase {
                onto: "origin/main".into(),
                branch: None,
            },
        ])))]
    );

    // 4. PushRejected when up_to == branch tip: retries with up_to: None after smart rebase.
    ts_rej.deliver_engine_event(EngineEvent::GitOpFinished(GitOpOutcome::failed(
        GitOp::Push {
            branch: "main".into(),
            remote: "origin".into(),
            remote_branch: "main".into(),
            set_upstream: false,
            force: PushForce::None,
            tags: PushTagMode::None,
            up_to: Some(oid(12)),
            no_verify: false,
            expected_remote_oid: None,
        },
        GitFailure::PushRejected,
    )));
    ts_rej.send_key(KeyCode::Char('r'), KeyModifiers::NONE);
    assert_eq!(
        ts_rej.take_git_ops(),
        vec![GitOp::Sequence(vec![
            GitOp::Smart(Box::new(GitOp::Sequence(vec![
                GitOp::Fetch,
                GitOp::Rebase {
                    onto: "origin/main".into(),
                    branch: None,
                },
            ]))),
            GitOp::Push {
                branch: "main".into(),
                remote: "origin".into(),
                remote_branch: "main".into(),
                set_upstream: false,
                force: PushForce::None,
                tags: PushTagMode::None,
                up_to: None,
                no_verify: false,
                expected_remote_oid: None,
            },
        ])]
    );
}

#[test]
fn delete_remote_tag_dialog_uses_tag_copy_not_branch_in_zh_cn() {
    let mut snap = snapshot();
    snap.remotes = super::remote_infos(&["origin"]);
    snap.refs.push(tag_ref("v1.0", 12));
    let mut ts = TestSession::with_snapshot(snap);
    ts.state_mut().i18n = std::sync::Arc::new(crate::i18n::I18n::load("zh-CN", None));

    let items = crate::menus::branch_menu_items(ts.state(), "v1.0", RefKind::Tag);
    let ContextMenuEntryKind::Action(actions) =
        &find_item(&items, "删除 'origin' 上的标签").kind
    else {
        panic!("expected direct delete-remote-tag action item");
    };
    ts.dispatch_many(actions.clone());

    match ts.state().modal_stack.last() {
        Some(ModalLayer::Dialog(d)) => match d.as_ref() {
            Dialog::Choice(c) => {
                let body = c.body.join("\n");
                assert!(body.contains("标签"), "expected 标签 in body: {body}");
                assert!(body.contains("v1.0"), "expected v1.0 in body: {body}");
                assert!(!body.contains("分支"), "must not mention 分支: {body}");
            }
            other => panic!("expected ChoiceDialog, got {other:?}"),
        },
        other => panic!("expected Dialog, got {other:?}"),
    }
}

#[test]
fn detached_head_push_no_verify_pinned_lease_inline_define_remote_and_push_refspec() {
    // 1. Detached HEAD unlocks PushDialog via `KeyAction::GitPush` (`P`).
    let mut detached_snap = snapshot();
    detached_snap.head_branch = None;
    detached_snap.head_oid = Some(oid(12));
    detached_snap.remotes = super::remote_infos(&["origin"]);
    let mut ts = TestSession::with_snapshot(detached_snap);
    let _ = ts.take_requests();

    ts.send_key(KeyCode::Char('P'), KeyModifiers::SHIFT);
    let pd = ts
        .state()
        .modal_stack
        .push_dialog()
        .expect("detached HEAD must open PushDialog");
    assert_eq!(pd.branch, "HEAD");
    assert_eq!(pd.up_to, Some(oid(12)));
    assert!(!pd.set_upstream);
    assert!(pd.remote_branch.is_empty());

    // Confirming with empty target branch is rejected for detached HEAD.
    ts.dispatch(Action::PushConfirm);
    assert!(ts.state().modal_stack.push_dialog().is_some());
    assert!(ts.take_git_ops().is_empty());

    // Set target branch, toggle `--no-verify` (`n`) and `--force-with-lease` (`f`),
    // deliver OutgoingLoaded with `remote_oid`, and confirm!
    ts.send_key(KeyCode::Char('e'), KeyModifiers::NONE);
    ts.type_str("rescue/detached");
    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);
    let out_epoch = ts
        .take_requests()
        .into_iter()
        .find_map(|(req, ep)| match req {
            TaskRequest::LoadOutgoing { .. } => ep,
            _ => None,
        })
        .expect("LoadOutgoing epoch");
    ts.deliver_engine_event(EngineEvent::OutgoingLoaded {
        bundle: Arc::new(OutgoingBundle {
            key: OutgoingKey {
                branch: "HEAD".into(),
                remote: "origin".into(),
                remote_branch: "rescue/detached".into(),
                up_to: Some(oid(12)),
            },
            commits: vec![make_commit(12, vec![oid(11)], "detached work")],
            files_changed: 1,
            remote_exists: true,
            remote_oid: Some(oid(9)),
            truncated: false,
        }),
        epoch: out_epoch,
    });
    ts.send_key(KeyCode::Char('n'), KeyModifiers::NONE);
    ts.send_key(KeyCode::Char('f'), KeyModifiers::NONE);
    // `u` is ignored on detached HEAD.
    ts.send_key(KeyCode::Char('u'), KeyModifiers::NONE);
    ts.dispatch(Action::PushConfirm);
    assert_eq!(
        ts.take_git_ops(),
        vec![GitOp::Push {
            branch: "HEAD".into(),
            remote: "origin".into(),
            remote_branch: "rescue/detached".into(),
            set_upstream: false,
            force: PushForce::WithLease,
            tags: PushTagMode::None,
            up_to: Some(oid(12)),
            no_verify: true,
            expected_remote_oid: Some(oid(9)),
        }]
    );

    // 2. Empty remotes renders inline `[+ Define Remote…]` and refreshes PushDialog once added.
    let mut no_remote_snap = snapshot();
    no_remote_snap.remotes.clear();
    no_remote_snap.refs.retain(|r| r.kind != RefKind::RemoteBranch);
    for r in &mut no_remote_snap.refs {
        r.upstream = None;
    }
    let mut ts_empty = TestSession::with_snapshot(no_remote_snap.clone());
    ts_empty.dispatch(Action::PromptPush {
        branch: "main".into(),
        up_to: None,
    });
    let text = screen_text(&ts_empty.render(120, 32));
    assert!(text.contains("[+ Define Remote…]"), "{text}");
    let (dx, dy) = ts_empty
        .hit_center_of(|t| matches!(t, crate::hit_test::HitTarget::PushDefineRemote))
        .expect("inline Define Remote button hit target");
    ts_empty.send_mouse_event(super::left_click(dx, dy, KeyModifiers::NONE));
    assert!(matches!(
        ts_empty.state().modal_stack.last(),
        Some(ModalLayer::Dialog(d)) if matches!(d.as_ref(), Dialog::Input(_))
    ));
    // Dismiss AddRemote dialog and simulate snapshot refresh with `origin` + push refspec.
    ts_empty.send_key(KeyCode::Esc, KeyModifiers::NONE);
    let _ = ts_empty.take_requests();
    let mut with_remote = no_remote_snap;
    with_remote.remotes = vec![jbgit_core::RemoteInfo {
        name: "origin".into(),
        fetch_url: Some("https://example.com/repo.git".into()),
        push_url: None,
        push_refspecs: vec!["refs/heads/main:refs/heads/trunk".into()],
    }];
    ts_empty.deliver_engine_event(EngineEvent::RepoSnapshotLoaded {
        snapshot: Arc::new(with_remote),
        epoch: 99,
    });
    let refreshed_pd = ts_empty.state().modal_stack.push_dialog().unwrap();
    assert_eq!(refreshed_pd.remote(), "origin");
    assert_eq!(refreshed_pd.remote_branch, "trunk");
    assert!(ts_empty.take_requests().iter().any(|(req, _)| matches!(
        req,
        TaskRequest::LoadOutgoing { key } if key.remote == "origin" && key.remote_branch == "trunk"
    )));
}

#[test]
fn push_rejected_warns_when_rebase_range_contains_merge_commits() {
    // Build a snapshot where `main` (oid 4) is a merge commit ahead of `origin/main` (oid 5).
    let commit_with_oid = |n: u8, parents: Vec<jbgit_core::Oid>, subject: &str| {
        let mut c = make_commit(n, parents, subject);
        c.oid = oid(n);
        c
    };
    let mut head = local("main", 4);
    head.is_head = true;
    head.upstream = Some("origin/main".into());
    let snap = RepoSnapshot {
        repo_name: "merge-warn".into(),
        head_branch: Some("main".into()),
        head_oid: Some(oid(4)),
        refs: vec![head, remote("origin/main", 5)],
        commits: vec![
            commit_with_oid(5, vec![oid(1)], "remote"),
            commit_with_oid(4, vec![oid(2), oid(3)], "Merge topic into main"),
            commit_with_oid(3, vec![oid(1)], "topic"),
            commit_with_oid(2, vec![oid(1)], "local"),
            commit_with_oid(1, vec![], "base"),
        ],
        remotes: super::remote_infos(&["origin"]),
        ..Default::default()
    };
    let mut ts = TestSession::with_snapshot(snap);
    ts.deliver_engine_event(EngineEvent::GitOpFinished(GitOpOutcome::failed(
        GitOp::Push {
            branch: "main".into(),
            remote: "origin".into(),
            remote_branch: "main".into(),
            set_upstream: false,
            force: PushForce::None,
            tags: PushTagMode::None,
            up_to: None,
            no_verify: false,
            expected_remote_oid: None,
        },
        GitFailure::PushRejected,
    )));
    match ts.state().modal_stack.last() {
        Some(ModalLayer::Dialog(d)) => match d.as_ref() {
            Dialog::Choice(c) => {
                assert!(
                    c.body.iter().any(|l| l.contains("merge commit")),
                    "expected rebase-over-merge warning in body: {:?}",
                    c.body
                );
                let rebase_opt = c
                    .options
                    .iter()
                    .find(|o| o.label == "Rebase & Retry")
                    .expect("Rebase & Retry option");
                assert!(rebase_opt.danger, "Rebase & Retry should be marked danger when merge commits exist");
            }
            other => panic!("expected ChoiceDialog, got {other:?}"),
        },
        other => panic!("expected Dialog, got {other:?}"),
    }
}
