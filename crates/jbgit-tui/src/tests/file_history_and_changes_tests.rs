use std::sync::Arc;

use crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use jbgit_core::{
    BranchFilter, ChangeRowKind, CommitChangesBundle, DetailsKey, DiffContext, DiffHunk, DiffLine,
    DiffLineKind, FileChange, FileDiff, FileDiffKey, FileHistoryBundle, FileHistoryEntry,
    FileHistoryKey, FileStatusKind, GitOp, PaneId, StageEntry, StageStatusSnapshot, WorkspaceTab,
};
use jbgit_engine::{EngineEvent, TaskRequest};

use crate::app::{Action, ChangesHost, ModalLayer};
use crate::dialogs::Dialog;
use crate::hit_test::HitTarget;
use crate::log_view::FileHistoryFocus;
use crate::menus::stage_tree_menu_items;

use super::{
    TestSession, item_run_actions, left_click, make_commit, make_details, make_snapshot,
    menu_item_by_label, oid_n, right_click,
};

#[test]
fn log_changes_context_menu_right_click_keyboard_dot_and_enablement() {
    let dir = tempfile::tempdir().unwrap();
    let workdir = dir.path().to_path_buf();
    std::fs::create_dir_all(workdir.join("dir with space")).unwrap();
    std::fs::write(workdir.join("dir with space/a.rs"), "pub fn a() {}\n").unwrap();

    let c1 = oid_n(1);
    let c2 = oid_n(2);
    let snap = (*make_snapshot(
        workdir,
        vec![
            make_commit(2, vec![c1], "modify files"),
            make_commit(1, vec![], "init"),
        ],
    ))
    .clone();
    let mut ts = TestSession::with_snapshot(snap);
    ts.dispatch(Action::SwitchTab(WorkspaceTab::Log));
    ts.state_mut().log.details.changes.group_by_dir = true;

    let key_single = DetailsKey::commits(vec![c2], 0);
    let bundle_single = Arc::new(CommitChangesBundle {
        key: key_single,
        details: vec![make_details(c2, vec![c1], "modify files")],
        changes: vec![
            FileChange {
                kind: FileStatusKind::Modified,
                path: "dir with space/a.rs".into(),
                old_path: None,
                is_binary: false,
                additions: 2,
                deletions: 1,
                exists_in_workdir: true,
                old_oid: None,
                new_oid: None,
            },
            FileChange {
                kind: FileStatusKind::Modified,
                path: "gone.rs".into(),
                old_path: None,
                is_binary: false,
                additions: 1,
                deletions: 0,
                exists_in_workdir: false,
                old_oid: None,
                new_oid: None,
            },
            FileChange {
                kind: FileStatusKind::Deleted,
                path: "deleted.rs".into(),
                old_path: None,
                is_binary: false,
                additions: 0,
                deletions: 5,
                exists_in_workdir: false,
                old_oid: None,
                new_oid: None,
            },
        ],
    });
    ts.deliver_engine_event(EngineEvent::CommitDetailsLoaded {
        slot: jbgit_core::RequestSlotId::LogCommitDetails,
        bundle: bundle_single,
        epoch: 1,
    });
    let _ = ts.render(140, 40);

    let rows = ts
        .state()
        .log
        .details
        .change_rows(&ts.state().diff_caches);
    let dir_idx = rows
        .iter()
        .position(|r| matches!(r.kind, ChangeRowKind::Dir { .. }) && r.key == "dir with space")
        .expect("dir row");
    let present_idx = rows
        .iter()
        .position(|r| matches!(r.kind, ChangeRowKind::File { .. }) && r.key == "dir with space/a.rs")
        .expect("present file row");
    let gone_idx = rows
        .iter()
        .position(|r| matches!(r.kind, ChangeRowKind::File { .. }) && r.key == "gone.rs")
        .expect("gone file row");
    let deleted_idx = rows
        .iter()
        .position(|r| matches!(r.kind, ChangeRowKind::File { .. }) && r.key == "deleted.rs")
        .expect("deleted file row");

    // 1. Right-click on existing file row in LogChangesTree.
    let (rx, ry) = ts
        .hit_center_of(|t| matches!(t, HitTarget::LogChangeRow { idx } if *idx == present_idx))
        .expect("hit target for present file");
    ts.send_mouse_event(right_click(rx, ry));
    let items = match ts.state().modal_stack.last() {
        Some(ModalLayer::ContextMenu { items, .. }) => items.clone(),
        other => panic!("expected ContextMenu, got {other:?}"),
    };
    assert!(menu_item_by_label(&items, "Toggle Diff Preview").is_enabled());
    let fs_diff = menu_item_by_label(&items, "Full-Screen Diff");
    assert!(fs_diff.is_enabled());
    assert_eq!(fs_diff.shortcut.as_deref(), Some("F4"));
    assert!(menu_item_by_label(&items, "Compare with Local").is_enabled());
    assert!(menu_item_by_label(&items, "Show History for Selected File").is_enabled());
    assert!(menu_item_by_label(&items, "Filter Log by Path").is_enabled());
    assert!(menu_item_by_label(&items, "Get Version").is_enabled());
    assert!(menu_item_by_label(&items, "Open in External Editor").is_enabled());
    assert!(menu_item_by_label(&items, "Copy Relative Path").is_enabled());
    assert!(menu_item_by_label(&items, "Copy Patch of File").is_enabled());
    ts.dispatch(Action::PopModal);

    // 2. Right-click on file missing from worktree disables Open in External Editor.
    let (gx, gy) = ts
        .hit_center_of(|t| matches!(t, HitTarget::LogChangeRow { idx } if *idx == gone_idx))
        .expect("hit target for gone file");
    ts.send_mouse_event(right_click(gx, gy));
    let items = match ts.state().modal_stack.last() {
        Some(ModalLayer::ContextMenu { items, .. }) => items.clone(),
        _ => panic!("expected menu"),
    };
    let ed_item = menu_item_by_label(&items, "Open in External Editor");
    assert!(!ed_item.is_enabled());
    assert_eq!(
        ed_item.disabled_reason.as_deref(),
        Some(ts.state().i18n.t("menu.log_change.not_in_workdir"))
    );
    ts.dispatch(Action::PopModal);

    // 3. Deleted file disables Get Version.
    let (dx, dy) = ts
        .hit_center_of(|t| matches!(t, HitTarget::LogChangeRow { idx } if *idx == deleted_idx))
        .expect("hit target for deleted file");
    ts.send_mouse_event(right_click(dx, dy));
    let items = match ts.state().modal_stack.last() {
        Some(ModalLayer::ContextMenu { items, .. }) => items.clone(),
        _ => panic!("expected menu"),
    };
    let restore = menu_item_by_label(&items, "Get Version");
    assert!(!restore.is_enabled());
    assert_eq!(
        restore.disabled_reason.as_deref(),
        Some(ts.state().i18n.t("menu.log_change.cannot_restore_deleted"))
    );
    ts.dispatch(Action::PopModal);

    // 4. Directory row via keyboard `.` in PaneId::LogChangesTree.
    ts.dispatch(Action::FocusPane(PaneId::LogChangesTree));
    ts.dispatch(Action::ChangesSelect(ChangesHost::Log, dir_idx));
    ts.send_key(KeyCode::Char('.'), KeyModifiers::NONE);
    let items = match ts.state().modal_stack.last() {
        Some(ModalLayer::ContextMenu { items, .. }) => items.clone(),
        other => panic!("expected directory ContextMenu via '.', got {other:?}"),
    };
    assert!(menu_item_by_label(&items, "Show History for Selected Directory").is_enabled());
    assert!(menu_item_by_label(&items, "Filter Log by Path").is_enabled());
    assert!(menu_item_by_label(&items, "Copy Relative Path").is_enabled());
    for file_only in [
        "Toggle Diff Preview",
        "Compare with Local",
        "Get Version",
        "Open in External Editor",
        "Copy Patch of File",
    ] {
        let item = menu_item_by_label(&items, file_only);
        assert!(!item.is_enabled(), "{file_only} must be disabled on directory row");
        assert_eq!(
            item.disabled_reason.as_deref(),
            Some(ts.state().i18n.t("menu.log_change.need_file"))
        );
    }
    ts.dispatch(Action::PopModal);

    // 5. Multi-commit selection disables single-commit operations.
    ts.dispatch(Action::FocusPane(PaneId::LogCommitTable));
    ts.dispatch(Action::SelectLogRow(0));
    ts.dispatch(Action::ExtendLogSelectionTo(1));
    let key_multi = DetailsKey::commits(vec![c2, c1], 0);
    let bundle_multi = Arc::new(CommitChangesBundle {
        key: key_multi,
        details: vec![make_details(c1, vec![], "init"), make_details(c2, vec![c1], "mod")],
        changes: vec![FileChange {
            kind: FileStatusKind::Modified,
            path: "dir with space/a.rs".into(),
            old_path: None,
            is_binary: false,
            additions: 3,
            deletions: 0,
            exists_in_workdir: true,
            old_oid: None,
            new_oid: None,
        }],
    });
    ts.deliver_engine_event(EngineEvent::CommitDetailsLoaded {
        slot: jbgit_core::RequestSlotId::LogCommitDetails,
        bundle: bundle_multi,
        epoch: 2,
    });
    let _ = ts.render(140, 40);
    ts.dispatch(Action::FocusPane(PaneId::LogChangesTree));
    ts.dispatch(Action::ChangesSelect(ChangesHost::Log, 1));
    ts.send_key(KeyCode::Char('.'), KeyModifiers::NONE);
    let items = match ts.state().modal_stack.last() {
        Some(ModalLayer::ContextMenu { items, .. }) => items.clone(),
        _ => panic!("expected menu"),
    };
    for single_only in ["Compare with Local", "Get Version", "Copy Patch of File"] {
        let item = menu_item_by_label(&items, single_only);
        assert!(!item.is_enabled(), "{single_only} must be disabled for multi-commit");
        assert_eq!(
            item.disabled_reason.as_deref(),
            Some(ts.state().i18n.t("menu.log_change.need_single_commit"))
        );
    }
    assert!(menu_item_by_label(&items, "Show History for Selected File").is_enabled());
}

#[test]
fn filter_log_by_path_preserves_spaces_and_requests_matches() {
    let dir = tempfile::tempdir().unwrap();
    let workdir = dir.path().to_path_buf();
    let snap = (*make_snapshot(workdir, vec![make_commit(1, vec![], "init")])).clone();
    let mut ts = TestSession::with_snapshot(snap);
    let _ = ts.take_requests();

    ts.dispatch(Action::FilterLogByPath("dir with space/a.rs".into()));
    assert_eq!(
        ts.state().log.filter.spec.paths.paths,
        vec!["dir with space/a.rs".to_string()]
    );
    assert!(
        ts.state()
            .status_notice
            .as_deref()
            .is_some_and(|n| n.contains("dir with space/a.rs"))
    );
    let reqs = ts.take_requests();
    assert!(reqs.iter().any(|(r, _)| matches!(
        r,
        TaskRequest::LoadPathMatches { paths, .. } if paths == &vec!["dir with space/a.rs".to_string()]
    )));
}

#[test]
fn compare_file_with_local_expands_collapsed_dir_cache_hit_and_missing_toast() {
    let dir = tempfile::tempdir().unwrap();
    let workdir = dir.path().to_path_buf();
    let c1 = oid_n(1);
    let c2 = oid_n(2);
    let snap = (*make_snapshot(
        workdir,
        vec![make_commit(2, vec![c1], "c2"), make_commit(1, vec![], "c1")],
    ))
    .clone();
    let mut ts = TestSession::with_snapshot(snap);
    ts.state_mut().log.details.changes.group_by_dir = true;
    ts.state_mut()
        .log
        .details
        .changes
        .collapsed
        .insert("dir with space".into());
    let _ = ts.take_requests();

    ts.dispatch(Action::CompareFileWithLocal {
        target: c1,
        path: "dir with space/a.rs".into(),
    });
    assert!(ts.state().log.details.diff_preview);
    assert_eq!(
        ts.state().log.details.compare_override,
        Some(DetailsKey::compare_with_local(c1))
    );
    let reqs = ts.take_requests();
    assert!(reqs.iter().any(|(r, _)| matches!(
        r,
        TaskRequest::LoadCommitDetails { key: k, .. } if *k == DetailsKey::compare_with_local(c1)
    )));

    let cmp_bundle = Arc::new(CommitChangesBundle {
        key: DetailsKey::compare_with_local(c1),
        details: vec![make_details(c1, vec![], "c1")],
        changes: vec![
            FileChange {
                kind: FileStatusKind::Modified,
                path: "alpha.rs".into(),
                old_path: None,
                is_binary: false,
                additions: 1,
                deletions: 0,
                exists_in_workdir: true,
                old_oid: None,
                new_oid: None,
            },
            FileChange {
                kind: FileStatusKind::Modified,
                path: "dir with space/a.rs".into(),
                old_path: None,
                is_binary: false,
                additions: 4,
                deletions: 1,
                exists_in_workdir: true,
                old_oid: None,
                new_oid: None,
            },
        ],
    });
    ts.deliver_engine_event(EngineEvent::CommitDetailsLoaded {
        slot: jbgit_core::RequestSlotId::LogCommitDetails,
        bundle: cmp_bundle,
        epoch: 1,
    });
    assert!(
        !ts.state()
            .log
            .details
            .changes
            .collapsed
            .contains("dir with space")
    );
    assert_eq!(
        ts.state()
            .log
            .details
            .selected_change(&ts.state().diff_caches)
            .map(|c| c.path.as_str()),
        Some("dir with space/a.rs")
    );

    ts.dispatch(Action::SelectLogRow(0));
    ts.state_mut()
        .log
        .details
        .changes
        .collapsed
        .insert("dir with space".into());
    ts.dispatch(Action::CompareFileWithLocal {
        target: c1,
        path: "dir with space/a.rs".into(),
    });
    assert!(
        !ts.state()
            .log
            .details
            .changes
            .collapsed
            .contains("dir with space")
    );
    assert_eq!(
        ts.state()
            .log
            .details
            .selected_change(&ts.state().diff_caches)
            .map(|c| c.path.as_str()),
        Some("dir with space/a.rs")
    );

    ts.dispatch(Action::CompareFileWithLocal {
        target: c2,
        path: "identical.rs".into(),
    });
    let empty_cmp = Arc::new(CommitChangesBundle {
        key: DetailsKey::compare_with_local(c2),
        details: vec![make_details(c2, vec![c1], "c2")],
        changes: Vec::new(),
    });
    ts.deliver_engine_event(EngineEvent::CommitDetailsLoaded {
        slot: jbgit_core::RequestSlotId::LogCommitDetails,
        bundle: empty_cmp,
        epoch: 2,
    });
    assert!(
        ts.state()
            .status_notice
            .as_deref()
            .is_some_and(|n| n.contains("identical.rs") && n.contains("no differences"))
    );
}

#[test]
fn get_version_menu_item_opens_confirm_dialog_and_dispatches_checkout_paths() {
    let dir = tempfile::tempdir().unwrap();
    let workdir = dir.path().to_path_buf();
    let c1 = oid_n(1);
    let snap = (*make_snapshot(workdir, vec![make_commit(1, vec![], "init")])).clone();
    let mut ts = TestSession::with_snapshot(snap);
    ts.state_mut().log.details.changes.group_by_dir = false;
    let bundle = Arc::new(CommitChangesBundle {
        key: DetailsKey::commits(vec![c1], 0),
        details: vec![make_details(c1, vec![], "init")],
        changes: vec![FileChange {
            kind: FileStatusKind::Modified,
            path: "src/lib.rs".into(),
            old_path: None,
            is_binary: false,
            additions: 1,
            deletions: 0,
            exists_in_workdir: true,
            old_oid: None,
            new_oid: None,
        }],
    });
    ts.deliver_engine_event(EngineEvent::CommitDetailsLoaded {
        slot: jbgit_core::RequestSlotId::LogCommitDetails,
        bundle,
        epoch: 1,
    });

    let items = crate::menus::log_change_menu_items(ts.state());
    let restore = menu_item_by_label(&items, "Get Version");
    ts.dispatch_many(item_run_actions(restore));
    match ts.state().modal_stack.last() {
        Some(ModalLayer::Dialog(d)) => match d.as_ref() {
            Dialog::Choice(c) => {
                assert_eq!(c.selected, 1);
                assert!(c.body.len() >= 2);
                assert!(c.body.iter().all(|l| !l.contains('\n')));
            }
            other => panic!("expected ChoiceDialog, got {other:?}"),
        },
        other => panic!("expected Dialog, got {other:?}"),
    }

    let _ = ts.take_requests();
    ts.send_key(KeyCode::Left, KeyModifiers::NONE);
    assert!(matches!(
        ts.state().modal_stack.last(),
        Some(ModalLayer::Dialog(d)) if matches!(d.as_ref(), Dialog::Choice(c) if c.selected == 0)
    ));
    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);
    assert!(ts.state().modal_stack.is_empty());
    assert!(ts.take_git_ops().iter().any(|op| matches!(
        op,
        GitOp::CheckoutPaths { rev, paths }
            if rev == &c1.to_string() && paths == &vec!["src/lib.rs".to_string()]
    )));
}

#[test]
fn commit_stage_tree_history_menu_and_h_shortcut() {
    let dir = tempfile::tempdir().unwrap();
    let workdir = dir.path().to_path_buf();
    let c1 = oid_n(1);
    let snap = (*make_snapshot(workdir, vec![make_commit(1, vec![], "init")])).clone();
    let mut ts = TestSession::with_snapshot(snap);
    ts.dispatch(Action::SwitchTab(WorkspaceTab::Commit));
    ts.dispatch(Action::FocusPane(PaneId::CommitChangesTree));
    ts.dispatch(Action::StageToggleShowIgnored);

    let status = StageStatusSnapshot {
        staged: vec![StageEntry {
            path: "tracked.rs".into(),
            old_path: None,
            kind: FileStatusKind::Modified,
        }],
        unstaged: Vec::new(),
        untracked: vec![StageEntry {
            path: "new_untracked.rs".into(),
            old_path: None,
            kind: FileStatusKind::Untracked,
        }],
        ignored: vec![StageEntry {
            path: "build.log".into(),
            old_path: None,
            kind: FileStatusKind::Ignored,
        }],
        conflicted: Vec::new(),
        conflict_labels: Default::default(),
        submodules: Default::default(),
    };
    ts.deliver_engine_event(EngineEvent::StageStatusLoaded {
        status: Arc::new(status),
        epoch: 1,
    });

    let tracked_idx = ts
        .state()
        .stage_workbench
        .rows()
        .iter()
        .position(|r| r.is_file() && r.key.ends_with("tracked.rs"))
        .expect("tracked row");
    let untracked_idx = ts
        .state()
        .stage_workbench
        .rows()
        .iter()
        .position(|r| r.is_file() && r.key.ends_with("new_untracked.rs"))
        .expect("untracked row");
    let ignored_idx = ts
        .state()
        .stage_workbench
        .rows()
        .iter()
        .position(|r| r.is_file() && r.key.ends_with("build.log"))
        .expect("ignored row");

    let tracked_menu = stage_tree_menu_items(ts.state(), tracked_idx);
    assert!(menu_item_by_label(&tracked_menu, "Show File History").is_enabled());

    for bad_idx in [untracked_idx, ignored_idx] {
        let menu = stage_tree_menu_items(ts.state(), bad_idx);
        let hist = menu_item_by_label(&menu, "Show File History");
        assert!(!hist.is_enabled());
        assert_eq!(
            hist.disabled_reason.as_deref(),
            Some(ts.state().i18n.t("menu.stage.only_tracked_history"))
        );
    }

    ts.dispatch(Action::SelectCommitStageRow(untracked_idx));
    ts.send_key(KeyCode::Char('H'), KeyModifiers::NONE);
    assert!(ts.state().modal_stack.file_history().is_none());
    assert_eq!(
        ts.state().status_notice.as_deref(),
        Some(ts.state().i18n.t("menu.stage.only_tracked_history"))
    );

    ts.dispatch(Action::SelectCommitStageRow(tracked_idx));
    let _ = ts.take_requests();
    ts.send_key(KeyCode::Char('H'), KeyModifiers::NONE);
    assert!(matches!(
        ts.state().modal_stack.last(),
        Some(ModalLayer::FileHistory(_))
    ));
    let reqs = ts.take_requests();
    assert!(reqs.iter().any(|(r, _)| matches!(
        r,
        TaskRequest::LoadFileHistory { key: k }
            if k.path == "tracked.rs" && k.start_oid.is_none() && k.follow_renames
    )));
    ts.dispatch(Action::FileHistoryEscape);

    ts.dispatch(Action::SwitchTab(WorkspaceTab::Log));
    ts.dispatch(Action::FocusPane(PaneId::LogChangesTree));
    ts.state_mut().log.details.changes.group_by_dir = true;
    let bundle = Arc::new(CommitChangesBundle {
        key: DetailsKey::commits(vec![c1], 0),
        details: vec![make_details(c1, vec![], "init")],
        changes: vec![FileChange {
            kind: FileStatusKind::Added,
            path: "sub/item.rs".into(),
            old_path: None,
            is_binary: false,
            additions: 1,
            deletions: 0,
            exists_in_workdir: true,
            old_oid: None,
            new_oid: None,
        }],
    });
    ts.deliver_engine_event(EngineEvent::CommitDetailsLoaded {
        slot: jbgit_core::RequestSlotId::LogCommitDetails,
        bundle,
        epoch: 2,
    });

    ts.dispatch(Action::ChangesSelect(ChangesHost::Log, 1));
    let _ = ts.take_requests();
    ts.send_key(KeyCode::Char('H'), KeyModifiers::NONE);
    let reqs = ts.take_requests();
    assert!(reqs.iter().any(|(r, _)| matches!(
        r,
        TaskRequest::LoadFileHistory { key: k }
            if k.path == "sub/item.rs" && k.start_oid == Some(c1) && k.follow_renames
    )));
    ts.dispatch(Action::FileHistoryEscape);

    ts.dispatch(Action::ChangesSelect(ChangesHost::Log, 0));
    let _ = ts.take_requests();
    ts.send_key(KeyCode::Char('H'), KeyModifiers::NONE);
    assert_eq!(
        ts.state().log.filter.spec.paths.paths,
        vec!["sub".to_string()]
    );
    let reqs = ts.take_requests();
    assert!(
        reqs.iter()
            .any(|(r, _)| matches!(r, TaskRequest::LoadPathMatches { .. }))
    );
}

#[test]
fn file_history_modal_end_to_end_renames_compare_navigation_and_scroll() {
    let dir = tempfile::tempdir().unwrap();
    let workdir = dir.path().to_path_buf();
    let c1 = oid_n(1);
    let c2 = oid_n(2);
    let c3 = oid_n(3);
    let snap = (*make_snapshot(
        workdir,
        vec![
            make_commit(3, vec![c2], "rename mid -> new"),
            make_commit(2, vec![c1], "rename old -> mid"),
            make_commit(1, vec![], "create old"),
        ],
    ))
    .clone();
    let mut ts = TestSession::with_snapshot(snap);
    ts.dispatch(Action::SwitchTab(WorkspaceTab::Log));
    ts.dispatch(Action::FocusPane(PaneId::LogChangesTree));
    ts.state_mut().log.filter.spec.branch = BranchFilter::parse("main");
    let _ = ts.take_requests();

    ts.dispatch(Action::OpenFileHistory {
        path: "new_name.rs".into(),
        start_oid: Some(c2),
        follow_renames: true,
        line_range: None,
    });
    assert!(matches!(
        ts.state().modal_stack.last(),
        Some(ModalLayer::FileHistory(_))
    ));
    let reqs = ts.take_requests();
    assert!(reqs.iter().any(|(r, _)| matches!(
        r,
        TaskRequest::LoadFileHistory { key: k }
            if k.path == "new_name.rs" && k.start_oid == Some(c2) && k.follow_renames
    )));

    let hist_key = FileHistoryKey {
        path: "new_name.rs".into(),
        start_oid: Some(c2),
        follow_renames: true,
        line_range: None,
    };
    let make_hist_entries = || {
        vec![
            FileHistoryEntry {
                oid: c3,
                author_name: "Alice".into(),
                author_time: 1_700_000_180,
                summary: "rename mid -> new".into(),
                kind: FileStatusKind::Renamed,
                path_at_commit: "new_name.rs".into(),
                old_path: Some("mid_name.rs".into()),
                additions: 0,
                deletions: 0,
                is_binary: false,
                range_patch: None,
            },
            FileHistoryEntry {
                oid: c2,
                author_name: "Alice".into(),
                author_time: 1_700_000_120,
                summary: "rename old -> mid".into(),
                kind: FileStatusKind::Renamed,
                path_at_commit: "mid_name.rs".into(),
                old_path: Some("old_name.rs".into()),
                additions: 1,
                deletions: 1,
                is_binary: false,
                range_patch: None,
            },
            FileHistoryEntry {
                oid: c1,
                author_name: "Alice".into(),
                author_time: 1_700_000_060,
                summary: "create old".into(),
                kind: FileStatusKind::Added,
                path_at_commit: "old_name.rs".into(),
                old_path: None,
                additions: 40,
                deletions: 0,
                is_binary: false,
                range_patch: None,
            },
        ]
    };
    ts.deliver_engine_event(EngineEvent::FileHistoryLoaded {
        bundle: Arc::new(FileHistoryBundle {
            key: hist_key.clone(),
            entries: make_hist_entries(),
            truncated: false,
        }),
        epoch: 1,
    });
    assert_eq!(ts.state().modal_stack.file_history().unwrap().selected, 1);
    let expected_diff_key = FileDiffKey {
        details: DetailsKey::single(c2),
        path: "mid_name.rs".into(),
        old_path: Some("old_name.rs".into()),
        context: DiffContext::Three,
        whitespace: jbgit_core::WhitespaceMode::Default,
    };
    let reqs = ts.take_requests();
    assert!(reqs.iter().any(|(r, _)| matches!(
        r,
        TaskRequest::LoadFileDiff { key: k, .. } if *k == expected_diff_key
    )));

    let diff = FileDiff {
        key: expected_diff_key,
        hunks: vec![
            DiffHunk {
                header: "@@ -1,20 +1,20 @@".into(),
                lines: (1..=20)
                    .map(|i| DiffLine {
                        kind: DiffLineKind::Added,
                        old_lineno: None,
                        new_lineno: Some(i),
                        text: format!("line {i}"),
                        word_ranges: Vec::new(),
                    })
                    .collect(),
            },
            DiffHunk {
                header: "@@ -40,15 +40,15 @@".into(),
                lines: (40..=54)
                    .map(|i| DiffLine {
                        kind: DiffLineKind::Added,
                        old_lineno: None,
                        new_lineno: Some(i),
                        text: format!("line {i}"),
                        word_ranges: Vec::new(),
                    })
                    .collect(),
            },
        ],
        is_binary: false,
        binary: None,
        is_truncated: false,
        is_too_large: false,
        lfs: None,
        mode_change: None,
    };
    ts.deliver_engine_event(EngineEvent::FileDiffLoaded {
        slot: jbgit_core::RequestSlotId::FileHistoryDiff,
        diff: Arc::new(diff),
        epoch: 1,
    });

    let wide = super::screen_text(&ts.render(140, 40));
    assert!(wide.contains("← old_name.rs"));
    assert!(wide.contains("@ mid_name.rs") || wide.contains("← mid_name.rs"));
    assert!(wide.contains("Alice"), "author should render at 140x40:\n{wide}");
    assert!(wide.contains("2023-"), "commit date should render at 140x40:\n{wide}");
    assert!(
        ts.hit_center_of(|t| matches!(t, HitTarget::FileHistoryRow { idx: 0 }))
            .is_some()
    );
    assert!(
        ts.hit_center_of(|t| matches!(t, HitTarget::FileHistoryListArea))
            .is_some()
    );
    assert!(
        ts.hit_center_of(|t| matches!(t, HitTarget::FileHistoryDiffArea))
            .is_some()
    );
    assert!(
        ts.hit_center_of(|t| matches!(t, HitTarget::FileHistoryToolbar(_)))
            .is_some()
    );

    let _ = ts.render(80, 24);
    let _ = ts.render(140, 40);

    let _ = ts.take_requests();
    ts.send_key(KeyCode::Char('f'), KeyModifiers::NONE);
    assert!(!ts.state().modal_stack.file_history().unwrap().key.follow_renames);
    let reqs = ts.take_requests();
    assert!(reqs.iter().any(|(r, _)| matches!(
        r,
        TaskRequest::LoadFileHistory { key: k } if !k.follow_renames && k.path == "new_name.rs"
    )));
    ts.send_key(KeyCode::Char('f'), KeyModifiers::NONE);
    let epoch = ts
        .state()
        .request_slots
        .latest(jbgit_core::RequestSlotId::FileHistoryList);
    ts.deliver_engine_event(EngineEvent::FileHistoryLoaded {
        bundle: Arc::new(FileHistoryBundle {
            key: hist_key,
            entries: make_hist_entries(),
            truncated: false,
        }),
        epoch,
    });

    ts.send_key(KeyCode::Char('l'), KeyModifiers::NONE);
    let cmp_local_key = ts
        .state()
        .modal_stack
        .file_history()
        .unwrap()
        .desired_diff_key()
        .expect("local diff key");
    assert_eq!(cmp_local_key.details, DetailsKey::compare_with_local(c2));
    assert_eq!(cmp_local_key.path, "new_name.rs");
    assert_eq!(cmp_local_key.old_path.as_deref(), Some("mid_name.rs"));
    ts.send_key(KeyCode::Char('l'), KeyModifiers::NONE);

    ts.send_key(KeyCode::Down, KeyModifiers::SHIFT);
    let fh = ts.state().modal_stack.file_history().unwrap();
    assert_eq!(fh.anchor, Some(1));
    assert_eq!(fh.selected, 2);
    let cmp_two = fh.desired_diff_key().expect("compare versions diff key");
    assert_eq!(cmp_two.details, DetailsKey::compare_versions(c1, c2));
    assert_eq!(cmp_two.path, "mid_name.rs");
    assert_eq!(cmp_two.old_path.as_deref(), Some("old_name.rs"));
    let cmp_screen = super::screen_text(&ts.render(140, 40));
    assert!(
        cmp_screen.contains("old_name.rs → mid_name.rs"),
        "cross-rename comparison header must show old → new path:\n{cmp_screen}"
    );

    ts.send_key(KeyCode::Char('g'), KeyModifiers::NONE);
    ts.send_key(KeyCode::Char('g'), KeyModifiers::NONE);
    assert_eq!(ts.state().modal_stack.file_history().unwrap().selected, 0);
    ts.send_key(KeyCode::Down, KeyModifiers::NONE);
    let (row0_x, row0_y) = ts
        .hit_center_of(|t| matches!(t, HitTarget::FileHistoryRow { idx: 0 }))
        .unwrap();
    ts.send_mouse_event(left_click(row0_x, row0_y, KeyModifiers::SHIFT));
    let fh = ts.state().modal_stack.file_history().unwrap();
    assert_eq!(fh.anchor, Some(1));
    assert_eq!(fh.selected, 0);
    let cmp_c2_c3 = fh.desired_diff_key().unwrap();
    assert_eq!(cmp_c2_c3.details, DetailsKey::compare_versions(c2, c3));

    ts.send_key(KeyCode::Down, KeyModifiers::NONE);
    assert_eq!(ts.state().modal_stack.file_history().unwrap().anchor, None);
    let _ = ts.render(140, 24);
    let max_scroll = ts
        .state()
        .modal_stack
        .file_history()
        .unwrap()
        .diff
        .scroll
        .max_scroll()
        .expect("FileHistoryDiff bound recorded");
    assert!(max_scroll > 0);

    // `[` / `]` hunk jumping inside FileHistory modal works cleanly.
    assert_eq!(
        ts.state().modal_stack.file_history().unwrap().diff.scroll.offset(),
        0
    );
    ts.send_key(KeyCode::Char(']'), KeyModifiers::NONE);
    let second_hunk_offset = ts
        .state()
        .modal_stack
        .file_history()
        .unwrap()
        .diff
        .scroll
        .offset();
    assert!(
        second_hunk_offset > 0,
        "']' must jump to second hunk, got {second_hunk_offset}"
    );
    ts.send_key(KeyCode::Char('['), KeyModifiers::NONE);
    assert_eq!(
        ts.state().modal_stack.file_history().unwrap().diff.scroll.offset(),
        0,
        "'[' must jump back to first hunk"
    );

    ts.send_key(KeyCode::Tab, KeyModifiers::NONE);
    assert_eq!(
        ts.state().modal_stack.file_history().unwrap().focus,
        FileHistoryFocus::Diff
    );
    ts.send_key(KeyCode::End, KeyModifiers::NONE);
    ts.send_key(KeyCode::Down, KeyModifiers::NONE);
    assert_eq!(
        ts.state().modal_stack.file_history().unwrap().diff.scroll.offset(),
        max_scroll
    );
    ts.send_key(KeyCode::Up, KeyModifiers::NONE);
    assert_eq!(
        ts.state().modal_stack.file_history().unwrap().diff.scroll.offset(),
        max_scroll.saturating_sub(1)
    );
    ts.send_key(KeyCode::Char('g'), KeyModifiers::NONE);
    ts.send_key(KeyCode::Char('g'), KeyModifiers::NONE);
    assert_eq!(
        ts.state().modal_stack.file_history().unwrap().diff.scroll.offset(),
        0
    );

    ts.send_key(KeyCode::Char('z'), KeyModifiers::NONE);
    assert!(ts.state().modal_stack.file_history().unwrap().diff.zoomed);
    ts.send_key(KeyCode::Esc, KeyModifiers::NONE);
    assert!(!ts.state().modal_stack.file_history().unwrap().diff.zoomed);
    assert!(matches!(
        ts.state().modal_stack.last(),
        Some(ModalLayer::FileHistory(_))
    ));
    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);
    assert!(ts.state().modal_stack.file_history().is_none());
    assert!(ts.state().modal_stack.is_empty());
    assert_eq!(ts.state().active_tab, WorkspaceTab::Log);
    assert_eq!(ts.state().focused_pane, PaneId::LogCommitTable);
    assert_eq!(
        ts.state()
            .log
            .graph
            .selected_oids(ts.state().repo_snapshot.as_ref()),
        vec![c2]
    );
    assert_eq!(
        ts.state().log.filter.spec.branch,
        BranchFilter::parse("main")
    );

    ts.dispatch(Action::FocusPane(PaneId::LogChangesTree));
    ts.dispatch(Action::OpenFileHistory {
        path: "new_name.rs".into(),
        start_oid: Some(c1),
        follow_renames: true,
        line_range: None,
    });
    let epoch = ts
        .state()
        .request_slots
        .latest(jbgit_core::RequestSlotId::FileHistoryList);
    ts.deliver_engine_event(EngineEvent::FileHistoryLoaded {
        bundle: Arc::new(FileHistoryBundle {
            key: FileHistoryKey {
                path: "new_name.rs".into(),
                start_oid: Some(c1),
                follow_renames: true,
                line_range: None,
            },
            entries: vec![FileHistoryEntry {
                oid: c1,
                author_name: "Alice".into(),
                author_time: 1_700_000_060,
                summary: "create old".into(),
                kind: FileStatusKind::Added,
                path_at_commit: "old_name.rs".into(),
                old_path: None,
                additions: 1,
                deletions: 0,
                is_binary: false,
                range_patch: None,
            }],
            truncated: false,
        }),
        epoch,
    });
    ts.send_key(KeyCode::Esc, KeyModifiers::NONE);
    assert!(ts.state().modal_stack.file_history().is_none());
    assert!(ts.state().modal_stack.is_empty());
    assert_eq!(ts.state().focused_pane, PaneId::LogChangesTree);
    assert_eq!(
        ts.state().log.filter.spec.branch,
        BranchFilter::parse("main")
    );

    ts.dispatch(Action::OpenFileHistory {
        path: "new_name.rs".into(),
        start_oid: Some(c1),
        follow_renames: true,
        line_range: None,
    });
    let epoch = ts
        .state()
        .request_slots
        .latest(jbgit_core::RequestSlotId::FileHistoryList);
    ts.deliver_engine_event(EngineEvent::FileHistoryLoaded {
        bundle: Arc::new(FileHistoryBundle {
            key: FileHistoryKey {
                path: "new_name.rs".into(),
                start_oid: Some(c1),
                follow_renames: true,
                line_range: None,
            },
            entries: vec![FileHistoryEntry {
                oid: c1,
                author_name: "Alice".into(),
                author_time: 1_700_000_060,
                summary: "create old".into(),
                kind: FileStatusKind::Added,
                path_at_commit: "old_name.rs".into(),
                old_path: None,
                additions: 1,
                deletions: 0,
                is_binary: false,
                range_patch: None,
            }],
            truncated: false,
        }),
        epoch,
    });
    ts.send_key(KeyCode::Char('g'), KeyModifiers::NONE);
    assert!(ts.state().modal_stack.file_history().is_some());
    ts.advance_by(crate::log_view::FILE_HISTORY_G_WINDOW);
    assert!(ts.state().modal_stack.file_history().is_none());
    assert!(ts.state().modal_stack.is_empty());
    assert_eq!(ts.state().active_tab, WorkspaceTab::Log);
    assert_eq!(ts.state().focused_pane, PaneId::LogCommitTable);
    assert_eq!(
        ts.state()
            .log
            .graph
            .selected_oids(ts.state().repo_snapshot.as_ref()),
        vec![c1]
    );
}

#[test]
fn file_history_modal_uses_configured_layout_ratios_and_supports_splitter_drag_reset() {
    use jbgit_core::{SplitAxis, SplitterId};

    let dir = tempfile::tempdir().unwrap();
    let workdir = dir.path().to_path_buf();
    let mut cfg = crate::config::AppConfig::load_defaults();
    let ov: crate::config::JbgitConfigOverlay = toml::from_str(
        r#"
[layout]
file_history_ratio = [1, 3]
file_history_vertical_ratio = [1, 2]
"#,
    )
    .unwrap();
    cfg.jbgit.merge_overlay(&ov);

    let mut ts = TestSession::with_config(workdir, cfg);
    ts.dispatch(Action::OpenFileHistory {
        path: "src/lib.rs".into(),
        start_oid: None,
        follow_renames: true,
        line_range: None,
    });

    let _ = ts.render(140, 40);
    let list_area = ts.region_of(|t| matches!(t, HitTarget::FileHistoryListArea));
    let diff_area = ts.region_of(|t| matches!(t, HitTarget::FileHistoryDiffArea));
    let (_, container) = ts
        .state()
        .splitter_containers
        .get(&SplitterId::FileHistoryHorizontal)
        .copied()
        .expect("FileHistoryHorizontal container recorded");
    assert_eq!(
        list_area.width,
        ((f32::from(container.width) * 0.25).round() as u16).max(24)
    );
    assert_eq!(list_area.width + 1 + diff_area.width, container.width);

    let div_col = list_area.right();
    let div_row = list_area.y + 2;
    let hit = ts.state().hit_registry.hit_test(div_col, div_row).unwrap();
    assert_eq!(
        hit.target,
        HitTarget::Splitter {
            id: SplitterId::FileHistoryHorizontal,
            axis: SplitAxis::Horizontal,
        }
    );

    let drag_target = container.x + (container.width * 3) / 5;
    for ev in [
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: div_col,
            row: div_row,
            modifiers: KeyModifiers::NONE,
        },
        MouseEvent {
            kind: MouseEventKind::Drag(MouseButton::Left),
            column: drag_target,
            row: div_row,
            modifiers: KeyModifiers::NONE,
        },
        MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            column: drag_target,
            row: div_row,
            modifiers: KeyModifiers::NONE,
        },
    ] {
        ts.send_mouse_event(ev);
    }
    let _ = ts.render(140, 40);
    let dragged_list = ts.region_of(|t| matches!(t, HitTarget::FileHistoryListArea));
    assert!(dragged_list.width > list_area.width);

    ts.state_mut().last_click = None;
    let new_div_col = dragged_list.right();
    for _ in 0..2 {
        ts.send_mouse_event(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: new_div_col,
            row: div_row,
            modifiers: KeyModifiers::NONE,
        });
    }
    let _ = ts.render(140, 40);
    let reset_list = ts.region_of(|t| matches!(t, HitTarget::FileHistoryListArea));
    assert_eq!(reset_list.width, list_area.width);

    let _ = ts.render(80, 32);
    let v_list = ts.region_of(|t| matches!(t, HitTarget::FileHistoryListArea));
    let v_diff = ts.region_of(|t| matches!(t, HitTarget::FileHistoryDiffArea));
    let (_, v_container) = ts
        .state()
        .splitter_containers
        .get(&SplitterId::FileHistoryVertical)
        .copied()
        .expect("FileHistoryVertical container recorded");
    assert_eq!(
        v_list.height,
        ((f32::from(v_container.height) / 3.0).round() as u16).max(3)
    );
    assert_eq!(v_list.height + 1 + v_diff.height, v_container.height);

    let v_div_col = v_container.x + 2;
    let v_div_row = v_list.bottom();
    let v_hit = ts
        .state()
        .hit_registry
        .hit_test(v_div_col, v_div_row)
        .unwrap();
    assert_eq!(
        v_hit.target,
        HitTarget::Splitter {
            id: SplitterId::FileHistoryVertical,
            axis: SplitAxis::Vertical,
        }
    );

    let v_drag_target = v_container.y + (v_container.height * 2) / 3;
    for kind in [
        MouseEventKind::Down(MouseButton::Left),
        MouseEventKind::Drag(MouseButton::Left),
        MouseEventKind::Up(MouseButton::Left),
    ] {
        let row = if matches!(kind, MouseEventKind::Down(_)) {
            v_div_row
        } else {
            v_drag_target
        };
        ts.send_mouse_event(MouseEvent {
            kind,
            column: v_div_col,
            row,
            modifiers: KeyModifiers::NONE,
        });
    }
    let _ = ts.render(80, 32);
    let dragged_v_list = ts.region_of(|t| matches!(t, HitTarget::FileHistoryListArea));
    assert!(dragged_v_list.height > v_list.height);

    ts.state_mut().last_click = None;
    let new_v_div_row = dragged_v_list.bottom();
    for _ in 0..2 {
        ts.send_mouse_event(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: v_div_col,
            row: new_v_div_row,
            modifiers: KeyModifiers::NONE,
        });
    }
    let _ = ts.render(80, 32);
    let reset_v_list = ts.region_of(|t| matches!(t, HitTarget::FileHistoryListArea));
    assert_eq!(reset_v_list.height, v_list.height);
}

#[test]
fn directory_context_menu_starts_on_enabled_item_and_disabled_mnemonic_updates_footer() {
    let dir = tempfile::tempdir().unwrap();
    let workdir = dir.path().to_path_buf();
    let c1 = oid_n(1);
    let c2 = oid_n(2);
    let snap = (*make_snapshot(
        workdir,
        vec![make_commit(2, vec![c1], "mod"), make_commit(1, vec![], "init")],
    ))
    .clone();
    let mut ts = TestSession::with_snapshot(snap);
    ts.dispatch(Action::SwitchTab(WorkspaceTab::Log));

    let bundle = Arc::new(CommitChangesBundle {
        key: DetailsKey::single(c2),
        details: vec![make_details(c2, vec![c1], "mod")],
        changes: vec![FileChange {
            kind: FileStatusKind::Modified,
            path: "src/components/a.rs".into(),
            old_path: None,
            is_binary: false,
            additions: 1,
            deletions: 0,
            exists_in_workdir: true,
            old_oid: None,
            new_oid: None,
        }],
    });
    ts.deliver_engine_event(EngineEvent::CommitDetailsLoaded {
        slot: jbgit_core::RequestSlotId::LogCommitDetails,
        bundle,
        epoch: 1,
    });
    let _ = ts.render(140, 40);

    let rows = ts
        .state()
        .log
        .details
        .change_rows(&ts.state().diff_caches)
        .to_vec();
    let dir_idx = rows
        .iter()
        .position(|r| matches!(r.kind, ChangeRowKind::Dir { .. }))
        .expect("directory row");

    ts.dispatch(Action::FocusPane(PaneId::LogChangesTree));
    ts.dispatch(Action::ChangesSelect(ChangesHost::Log, dir_idx));
    ts.send_key(KeyCode::Char('.'), KeyModifiers::NONE);

    let (items, selected) = match ts.state().modal_stack.last() {
        Some(ModalLayer::ContextMenu {
            items, selected, ..
        }) => (items.clone(), *selected),
        other => panic!("expected directory ContextMenu, got {other:?}"),
    };
    assert!(
        items[selected].is_enabled(),
        "initial menu selection on directory row must skip disabled items, got {:?}",
        items[selected].label
    );
    assert_eq!(items[selected].label, "Show History for Selected Directory");

    let buf = ts.render(140, 40);
    let footer = (0..buf.area.width)
        .map(|x| buf[(x, 39)].symbol())
        .collect::<String>();
    assert!(
        !footer.contains("Toggle Diff Preview"),
        "footer must not show disabled Toggle Diff Preview on open: {footer}"
    );

    ts.send_key(KeyCode::Char('p'), KeyModifiers::NONE);
    let (items, selected) = match ts.state().modal_stack.last() {
        Some(ModalLayer::ContextMenu {
            items, selected, ..
        }) => (items.clone(), *selected),
        other => panic!("expected ContextMenu still open, got {other:?}"),
    };
    assert_eq!(items[selected].label, "Copy Patch of File");
    let buf = ts.render(140, 40);
    let footer = (0..buf.area.width)
        .map(|x| buf[(x, 39)].symbol())
        .collect::<String>();
    assert!(
        footer.contains("Copy Patch of File: Select a file"),
        "footer must show disabled item's own reason, got: {footer}"
    );
}
