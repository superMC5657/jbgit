//! Acceptance tests for Spec 12 Issue 14 repository capabilities:
//! granular signature verification & Log column, read-only submodule handling,
//! Git LFS pointer placeholders, and shallow clone / unshallow workflows.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crossterm::event::MouseEventKind;
use jbgit_core::{
    CommitChangesBundle, CommitSignature, DetailsKey, DiffContext, FileChange, FileDiff,
    FileDiffKey, FileStatusKind, GitOp, LfsDiff, LfsPointer, Oid, RequestSlotId, SignatureStatus,
    StageEntry, StageLineTracker, StageStatusSnapshot, SubmoduleState, TreeBlob, TreeBlobContent,
    TreeEntry, TreeEntryKind, TreeListing, WorkspaceTab,
};
use jbgit_engine::{
    EngineEvent, StageDiffKey, StageDiffSide, SubmoduleDiffInfo, TaskRequest, ThreeDocStageDiff,
};

use crate::app::{Action, ContextMenuEntryKind, ModalLayer};
use crate::hit_test::HitTarget;
use crate::keymap::{KeyAction, actions::execute_action, actions::is_action_applicable};
use crate::log_view::PresentationToggle;

use super::{TestSession, find_item, item_run_actions, oid, right_click, screen_text, snapshot};

fn seeded_log_session() -> TestSession {
    let mut ts = TestSession::with_snapshot(snapshot());
    ts.dispatch(Action::SwitchTab(WorkspaceTab::Log));
    ts.dispatch(Action::SelectLogRow(0));
    ts
}

fn deliver_commit_sig_details(ts: &mut TestSession, target_oid: Oid, signature: CommitSignature) {
    let mut details = super::make_details(target_oid, vec![], "signed commit subject");
    details.signature = signature;
    ts.deliver_engine_event(EngineEvent::CommitDetailsLoaded {
        slot: jbgit_core::RequestSlotId::LogCommitDetails,
        bundle: Arc::new(CommitChangesBundle {
            key: DetailsKey::single(target_oid),
            details: vec![details],
            changes: vec![],
        }),
        epoch: ts
            .state()
            .request_slots
            .latest(jbgit_core::RequestSlotId::LogCommitDetails)
            .max(1),
    });
}

#[test]
fn granular_signature_statuses_render_badges_signer_and_key_in_details() {
    let cases = [
        (
            SignatureStatus::Good,
            Some("Alice <alice@example.com>"),
            Some("0xAAAA1111"),
            "Good signature",
        ),
        (
            SignatureStatus::GoodUnknownValidity,
            Some("Bob <bob@example.com>"),
            Some("0xBBBB2222"),
            "unknown trust validity",
        ),
        (
            SignatureStatus::Expired { key_expired: false },
            Some("Carol <carol@example.com>"),
            Some("0xCCCC3333"),
            "signature has expired",
        ),
        (
            SignatureStatus::Expired { key_expired: true },
            Some("Dave <dave@example.com>"),
            Some("0xDDDD4444"),
            "signing key has expired",
        ),
        (
            SignatureStatus::Revoked,
            None,
            Some("0xEEEE5555"),
            "revoked",
        ),
        (
            SignatureStatus::CannotCheck,
            None,
            Some("0xFFFF6666"),
            "missing key",
        ),
        (
            SignatureStatus::Bad,
            Some("Mallory <evil@example.com>"),
            Some("0x99990000"),
            "BAD signature",
        ),
    ];

    for (status, signer, key, expected_desc) in cases {
        let mut ts = seeded_log_session();
        deliver_commit_sig_details(
            &mut ts,
            oid(12),
            CommitSignature {
                status,
                signer: signer.map(str::to_string),
                key: key.map(str::to_string),
            },
        );
        let text = screen_text(&ts.render(220, 36));
        assert!(
            text.contains(expected_desc),
            "expected `{expected_desc}` for {status:?} in:\n{text}"
        );
        if let Some(s) = signer {
            assert!(text.contains(s), "missing signer `{s}` in:\n{text}");
        }
        if let Some(k) = key {
            assert!(text.contains(k), "missing key `{k}` in:\n{text}");
        }
    }
}

#[test]
fn log_signature_column_requests_batch_and_renders_icons_and_tooltip() {
    let mut ts = seeded_log_session();
    let _ = ts.take_requests();

    // Disabled by default: no signature verification batch dispatched on snapshot load.
    assert!(!ts.state().log.presentation.show_signature_column);

    // Enable the signature column: requests verification for visible commits.
    ts.dispatch(Action::TogglePresentation(
        PresentationToggle::ShowSignatureColumn,
    ));
    assert!(ts.state().log.presentation.show_signature_column);

    let sig_req = ts
        .take_requests()
        .into_iter()
        .find_map(|(req, _)| match req {
            TaskRequest::VerifyCommitSignatures { oids } => Some(oids),
            _ => None,
        })
        .expect("expected TaskRequest::VerifyCommitSignatures when enabling column");
    assert!(sig_req.contains(&oid(12)));
    assert!(sig_req.contains(&oid(11)));

    // Deliver granular verification results for the top commits.
    ts.deliver_engine_event(EngineEvent::CommitSignaturesLoaded {
        signatures: vec![
            (
                oid(12),
                CommitSignature {
                    status: SignatureStatus::Good,
                    signer: Some("Alice <alice@x.io>".into()),
                    key: Some("1111AAAA".into()),
                },
            ),
            (
                oid(11),
                CommitSignature {
                    status: SignatureStatus::Bad,
                    signer: Some("Bad Actor <bad@x.io>".into()),
                    key: Some("2222BBBB".into()),
                },
            ),
            (oid(10), CommitSignature::unsigned()),
        ],
        epoch: ts
            .state()
            .request_slots
            .latest(jbgit_core::RequestSlotId::LogSignatures),
    });

    let buf = ts.render(120, 30);
    let rendered = screen_text(&buf);
    assert!(rendered.contains('✓'), "expected ✓ icon in Log:\n{rendered}");
    assert!(rendered.contains('✕'), "expected ✕ icon in Log:\n{rendered}");

    // Hover over row 0's signature cell shows the signer + key tooltip.
    let (cx, cy) = ts
        .hit_center_of(|t| matches!(t, HitTarget::CommitSignatureCell { visible_row: 0, .. }))
        .expect("CommitSignatureCell hit target on row 0");
    ts.send_mouse(MouseEventKind::Moved, cx, cy);
    let hover_text = screen_text(&ts.render(120, 30));
    assert!(hover_text.contains("Commit Signature"), "{hover_text}");
    assert!(hover_text.contains("Alice <alice@x.io>"), "{hover_text}");
    assert!(hover_text.contains("1111AAAA"), "{hover_text}");
}

#[test]
fn submodule_stage_rows_block_mutations_and_render_diff_and_menus() {
    let mut snap = snapshot();
    snap.workdir = PathBuf::from("C:/work/super");
    assert!(!is_action_applicable(
        &KeyAction::UpdateSubmodules,
        TestSession::with_snapshot(snap.clone()).state()
    ));
    snap.submodules = vec!["libs/sub".into()];
    let mut ts = TestSession::with_snapshot(snap);
    assert!(is_action_applicable(&KeyAction::UpdateSubmodules, ts.state()));
    assert!(matches!(
        execute_action(&KeyAction::UpdateSubmodules, ts.state()).as_deref(),
        Some([Action::RunGitOp(GitOp::SubmoduleUpdate { path: None })])
    ));

    ts.dispatch(Action::SwitchTab(WorkspaceTab::Commit));
    let mut status = StageStatusSnapshot {
        unstaged: vec![
            StageEntry {
                path: "libs/sub".into(),
                old_path: None,
                kind: FileStatusKind::Modified,
            },
            StageEntry {
                path: "src/app.rs".into(),
                old_path: None,
                kind: FileStatusKind::Modified,
            },
        ],
        ..Default::default()
    };
    status.submodules.insert(
        "libs/sub".into(),
        SubmoduleState {
            new_commits: true,
            dirty_workdir: true,
            uninitialized: false,
        },
    );
    ts.deliver_engine_event(EngineEvent::StageStatusLoaded {
        status: Arc::new(status),
        epoch: 1,
    });
    let _ = ts.take_requests();

    // Verify row badge renders and hover does NOT draw `[+]` action on submodule row.
    let sub_row_idx = ts
        .state()
        .stage_workbench
        .rows()
        .iter()
        .position(|r| r.key == "file:Unstaged:libs/sub")
        .expect("libs/sub row in stage tree");
    ts.dispatch(Action::SelectCommitStageRow(sub_row_idx));
    let buf = ts.render(120, 30);
    let rendered = screen_text(&buf);
    assert!(
        rendered.contains("[new commits, dirty]"),
        "expected submodule badge in Stage tree:\n{rendered}"
    );
    let sub_rect = ts.region_of(
        |t| matches!(t, HitTarget::ChangesTreeNode { node_id, .. } if node_id.0 as usize == sub_row_idx),
    );
    ts.send_mouse(MouseEventKind::Moved, sub_rect.x + sub_rect.width - 2, sub_rect.y);
    let hovered_buf = ts.render(120, 30);
    assert!(
        !super::row_text(&hovered_buf, sub_rect.y).contains("[+]"),
        "submodule row must not render [+] hover button"
    );

    // Selecting submodule and attempting Stage / Discard must emit zero mutations and show notice.
    ts.dispatch(Action::StageActionSelected);
    assert!(
        ts.take_requests()
            .into_iter()
            .all(|(r, _)| !matches!(r, TaskRequest::StageTargets { .. }))
    );
    assert!(
        ts.state()
            .status_notice
            .as_deref()
            .is_some_and(|n| n.contains("read-only")),
        "{:?}",
        ts.state().status_notice
    );
    ts.dispatch(Action::DiscardPromptSelected);
    assert!(ts.state().modal_stack.is_empty());

    // Deliver SubmoduleDiffInfo and verify read-only diff panel + blocked line-level actions.
    let diff = Arc::new(ThreeDocStageDiff {
        key: StageDiffKey {
            path: "libs/sub".into(),
            old_path: None,
            side: StageDiffSide::Unstaged,
            context: DiffContext::Three,
            whitespace: jbgit_core::WhitespaceMode::Default,
        },
        vcs_exists: true,
        staged_exists: true,
        local_exists: true,
        vcs_bytes: Vec::new(),
        staged_bytes: Vec::new(),
        local_bytes: Vec::new(),
        file_mode: 0o160_000,
        mode_change: None,
        is_symlink: false,
        is_binary: false,
        binary: None,
        is_conflicted: false,
        submodule: Some(SubmoduleDiffInfo {
            head_oid: Some(oid(1)),
            index_oid: Some(oid(1)),
            workdir_oid: Some(oid(2)),
            state: SubmoduleState {
                new_commits: true,
                dirty_workdir: true,
                uninitialized: false,
            },
        }),
        lfs: None,
        tracker: StageLineTracker::new(b"", b"", b""),
        hunks: Vec::new(),
        is_truncated: false,
        is_too_large: false,
    });
    ts.deliver_engine_event(EngineEvent::StageFileDiffLoaded { diff, epoch: 2 });
    let diff_screen = screen_text(&ts.render(120, 30));
    assert!(
        diff_screen.contains("Submodule (Read-Only Gitlink)"),
        "{diff_screen}"
    );
    assert!(diff_screen.contains(&oid(1).to_string()), "{diff_screen}");
    assert!(diff_screen.contains(&oid(2).to_string()), "{diff_screen}");

    ts.dispatch(Action::StageDiffStageSelection);
    ts.dispatch(Action::OpenThreeWayInspector {
        row_idx: None,
        range_idx: None,
    });
    assert!(
        ts.take_requests()
            .into_iter()
            .all(|(r, _)| !matches!(r, TaskRequest::ApplyStageBuffer { .. }))
    );
    assert!(ts.state().modal_stack.is_empty());
    assert!(
        ts.state()
            .status_notice
            .as_deref()
            .is_some_and(|n| n.contains("Line-level staging is disabled")),
        "{:?}",
        ts.state().status_notice
    );

    // Right-click on submodule row provides Open Submodule in Switcher & Update Submodule, disables Stage.
    let (cx, cy) = ts
        .hit_center_of(
            |t| matches!(t, HitTarget::ChangesTreeNode { node_id, .. } if node_id.0 as usize == sub_row_idx),
        )
        .unwrap();
    ts.send_mouse_event(right_click(cx, cy));
    assert!(matches!(
        ts.state().modal_stack.last(),
        Some(ModalLayer::ContextMenu { .. })
    ));
    let (items, _, _) = super::top_menu(ts.state());
    assert!(find_item(items, "Stage").disabled_reason.is_some());
    let open_sub = find_item(items, "Open Submodule in Switcher");
    assert_eq!(open_sub.disabled_reason, None);
    let open_actions = item_run_actions(open_sub);
    assert!(
        matches!(
            open_actions.as_slice(),
            [Action::SwitchRepository(p)] if p == Path::new("C:/work/super/libs/sub")
        ),
        "{open_actions:?}"
    );
    let update_sub = find_item(items, "Update Submodule (--init --recursive)");
    assert!(matches!(update_sub.kind, ContextMenuEntryKind::Action { .. }));
    let update_actions = item_run_actions(update_sub);
    assert!(
        matches!(
            update_actions.as_slice(),
            [Action::RunGitOp(GitOp::SubmoduleUpdate { path: Some(p) })] if p == "libs/sub"
        ),
        "{update_actions:?}"
    );
}

#[test]
fn lfs_diffs_and_tree_browser_render_pointer_placeholders() {
    let ptr1 = LfsPointer {
        oid_sha256: "1111111111111111111111111111111111111111111111111111111111111111".into(),
        size: 1_048_576,
    };
    let ptr2 = LfsPointer {
        oid_sha256: "2222222222222222222222222222222222222222222222222222222222222222".into(),
        size: 2_097_152,
    };

    // 1. Log diff view renders LFS object pointer placeholder without text hunks.
    let mut ts = seeded_log_session();
    ts.dispatch(Action::ToggleDiffPreview);
    let key = DetailsKey::single(oid(12));
    ts.deliver_engine_event(EngineEvent::CommitDetailsLoaded {
        slot: RequestSlotId::LogCommitDetails,
        bundle: Arc::new(CommitChangesBundle {
            key: key.clone(),
            details: vec![super::make_details(oid(12), vec![oid(11)], "lfs commit")],
            changes: vec![FileChange {
                path: "assets/blob.bin".into(),
                old_path: None,
                kind: FileStatusKind::Modified,
                additions: 0,
                deletions: 0,
                old_oid: None,
                new_oid: None,
                is_binary: false,
                exists_in_workdir: true,
            }],
        }),
        epoch: ts
            .state()
            .request_slots
            .latest(RequestSlotId::LogCommitDetails)
            .max(1),
    });
    let file_row = ts
        .state()
        .log
        .details
        .change_rows(&ts.state().diff_caches)
        .iter()
        .position(|r| r.label == "blob.bin")
        .expect("blob.bin row in changes tree");
    ts.dispatch(Action::ChangesSelect(crate::app::ChangesHost::Log, file_row));
    let diff_key = FileDiffKey {
        details: key,
        path: "assets/blob.bin".into(),
        old_path: None,
        context: DiffContext::Three,
        whitespace: jbgit_core::WhitespaceMode::Default,
    };
    ts.deliver_engine_event(EngineEvent::FileDiffLoaded {
        slot: RequestSlotId::LogFileDiff,
        diff: Arc::new(FileDiff {
            key: diff_key,
            hunks: Vec::new(),
            is_binary: false,
            binary: None,
            is_truncated: false,
            is_too_large: false,
            lfs: Some(LfsDiff {
                old: Some(ptr1.clone()),
                new: Some(ptr2.clone()),
                filter_detected: true,
            }),
            mode_change: None,
        }),
        epoch: ts
            .state()
            .request_slots
            .latest(RequestSlotId::LogFileDiff)
            .max(1),
    });
    let log_screen = screen_text(&ts.render(160, 36));
    assert!(log_screen.contains("Git LFS Object Pointer"), "{log_screen}");
    assert!(log_screen.contains("sha256:111111111111"), "{log_screen}");
    assert!(log_screen.contains("sha256:222222222222"), "{log_screen}");
    assert!(log_screen.contains("filter=lfs active"), "{log_screen}");

    // 2. Stage diff view renders LFS placeholder and blocks line-level staging actions.
    ts.dispatch(Action::SwitchTab(WorkspaceTab::Commit));
    ts.deliver_engine_event(EngineEvent::StageStatusLoaded {
        status: Arc::new(StageStatusSnapshot {
            unstaged: vec![StageEntry {
                path: "assets/blob.bin".into(),
                old_path: None,
                kind: FileStatusKind::Modified,
            }],
            ..Default::default()
        }),
        epoch: 1,
    });
    let stage_row = ts
        .state()
        .stage_workbench
        .rows()
        .iter()
        .position(|r| r.key == "file:Unstaged:assets/blob.bin")
        .expect("assets/blob.bin row in stage tree");
    ts.dispatch(Action::SelectCommitStageRow(stage_row));
    ts.deliver_engine_event(EngineEvent::StageFileDiffLoaded {
        diff: Arc::new(ThreeDocStageDiff {
            key: StageDiffKey {
                path: "assets/blob.bin".into(),
                old_path: None,
                side: StageDiffSide::Unstaged,
                context: DiffContext::Three,
                whitespace: jbgit_core::WhitespaceMode::Default,
            },
            vcs_exists: true,
            staged_exists: true,
            local_exists: true,
            vcs_bytes: Vec::new(),
            staged_bytes: Vec::new(),
            local_bytes: Vec::new(),
            file_mode: 0o100_644,
            mode_change: None,
            is_symlink: false,
            is_binary: false,
            binary: None,
            is_conflicted: false,
            submodule: None,
            lfs: Some(LfsDiff {
                old: Some(ptr1.clone()),
                new: Some(ptr2),
                filter_detected: true,
            }),
            tracker: StageLineTracker::new(b"", b"", b""),
            hunks: Vec::new(),
            is_truncated: false,
            is_too_large: false,
        }),
        epoch: 2,
    });
    let stage_screen = screen_text(&ts.render(160, 36));
    assert!(stage_screen.contains("Git LFS Object Pointer"), "{stage_screen}");
    assert!(
        stage_screen.contains("Line-level staging is disabled for LFS pointers"),
        "{stage_screen}"
    );
    let _ = ts.take_requests();
    ts.dispatch(Action::StageDiffStageSelection);
    assert!(
        ts.take_requests()
            .into_iter()
            .all(|(r, _)| !matches!(r, TaskRequest::ApplyStageBuffer { .. }))
    );
    assert!(
        ts.state()
            .status_notice
            .as_deref()
            .is_some_and(|n| n.contains("Git LFS pointer")),
        "{:?}",
        ts.state().status_notice
    );

    // 3. Tree browser renders TreeBlobContent::Lfs pointer preview.
    ts.dispatch(Action::SwitchTab(WorkspaceTab::Log));
    ts.dispatch(Action::FocusPane(jbgit_core::PaneId::LogCommitTable));
    let _ = ts.take_requests();
    let items = crate::menus::log_commit_menu_items(ts.state());
    let browse = find_item(&items, "Browse Repository at Revision");
    ts.dispatch_many(item_run_actions(browse));
    let (tree_key, tree_epoch) = ts
        .take_requests()
        .into_iter()
        .find_map(|(r, ep)| match r {
            TaskRequest::LoadTree { key } => Some((key, ep.unwrap())),
            _ => None,
        })
        .expect("LoadTree requested");
    ts.deliver_engine_event(EngineEvent::TreeLoaded {
        listing: Arc::new(TreeListing {
            key: tree_key,
            entries: vec![TreeEntry {
                name: "blob.bin".into(),
                path: "assets/blob.bin".into(),
                kind: TreeEntryKind::Blob,
                mode: 0o100_644,
                size: Some(130),
                oid: oid(99),
                symlink_target: None,
            }],
        }),
        epoch: tree_epoch,
    });
    let (blob_key, blob_epoch) = ts
        .take_requests()
        .into_iter()
        .find_map(|(r, ep)| match r {
            TaskRequest::LoadTreeBlob { key } => Some((key, ep.unwrap())),
            _ => None,
        })
        .expect("LoadTreeBlob requested");
    ts.deliver_engine_event(EngineEvent::TreeBlobLoaded {
        blob: Arc::new(TreeBlob {
            key: blob_key,
            size: 130,
            content: TreeBlobContent::Lfs(ptr1),
        }),
        epoch: blob_epoch,
    });
    let tree_screen = screen_text(&ts.render(140, 30));
    assert!(tree_screen.contains("Git LFS pointer: sha256:111111111111"), "{tree_screen}");
    assert!(tree_screen.contains("1.0 MiB"), "{tree_screen}");
}

#[test]
fn shallow_clone_renders_boundary_unshallow_button_popup_and_dialog() {
    use crate::branches_popup::{PopupRow, build_popup_rows};
    use super::remote_infos;

    let mut full_snap = snapshot();
    full_snap.remotes = remote_infos(&["origin"]);
    let full_ts = TestSession::with_snapshot(full_snap.clone());
    assert!(!is_action_applicable(&KeyAction::Unshallow, full_ts.state()));
    assert!(!build_popup_rows(full_ts.state(), "").contains(&PopupRow::Unshallow));

    let mut shallow_snap = full_snap;
    shallow_snap.is_shallow = true;
    let mut ts = TestSession::with_snapshot(shallow_snap);
    assert!(is_action_applicable(&KeyAction::Unshallow, ts.state()));
    assert!(build_popup_rows(ts.state(), "").contains(&PopupRow::Unshallow));

    let buf = ts.render(120, 30);
    let screen = screen_text(&buf);
    assert!(screen.contains("· shallow"), "{screen}");
    assert!(screen.contains("Shallow clone boundary"), "{screen}");
    assert!(screen.contains("Unshallow…"), "{screen}");

    // Clicking the [Unshallow…] boundary button opens confirmation -> dispatching GitOp::Unshallow.
    let (bx, by) = ts
        .hit_center_of(|t| matches!(t, HitTarget::LogShallowUnshallow))
        .expect("LogShallowUnshallow pill hit region");
    ts.send_mouse_event(super::left_click(bx, by, crossterm::event::KeyModifiers::NONE));
    assert!(matches!(
        ts.state().modal_stack.last(),
        Some(ModalLayer::Dialog(_))
    ));
    ts.send_key(crossterm::event::KeyCode::Enter, crossterm::event::KeyModifiers::NONE);
    assert!(
        ts.take_git_ops()
            .into_iter()
            .any(|op| op == GitOp::Unshallow { remote: "origin".into() }),
        "confirming unshallow must emit GitOp::Unshallow"
    );
}

#[test]
fn commit_hooks_dimmed_when_absent_and_mode_symlink_no_newline_diffs_render() {
    use jbgit_core::{
        FileModeChange, GIT_MODE_EXECUTABLE, GIT_MODE_REGULAR, GIT_MODE_SYMLINK,
    };
    use jbgit_engine::CommitPrecheckInfo;

    let mut ts = seeded_log_session();
    ts.dispatch(Action::SwitchTab(WorkspaceTab::Commit));

    // 1. When no commit hooks exist, Hooks checkbox renders dimmed `Hooks (none)` and refuses toggle.
    ts.deliver_engine_event(EngineEvent::CommitPrecheckLoaded {
        info: CommitPrecheckInfo {
            has_commit_hooks: false,
            ..Default::default()
        },
    });
    let screen_no_hooks = screen_text(&ts.render(160, 36));
    assert!(
        screen_no_hooks.contains("[-] Hooks (none)"),
        "expected dimmed Hooks (none) badge:\n{screen_no_hooks}"
    );
    ts.dispatch(Action::CommitToggleRunHooks);
    assert!(ts.state().stage_workbench.run_hooks());
    assert!(
        ts.state()
            .status_notice
            .as_deref()
            .is_some_and(|n| n.contains("No Git commit hooks")),
        "{:?}",
        ts.state().status_notice
    );

    // When hooks ARE present, Hooks checkbox renders `[x] Hooks` and toggles cleanly.
    ts.deliver_engine_event(EngineEvent::CommitPrecheckLoaded {
        info: CommitPrecheckInfo {
            has_commit_hooks: true,
            ..Default::default()
        },
    });
    let screen_with_hooks = screen_text(&ts.render(160, 36));
    assert!(
        screen_with_hooks.contains("[x] Hooks"),
        "expected active [x] Hooks:\n{screen_with_hooks}"
    );
    ts.dispatch(Action::CommitToggleRunHooks);
    assert!(!ts.state().stage_workbench.run_hooks());

    // 2. Pure mode-change diff (100644 -> 100755) displays mode header instead of "No textual differences".
    ts.deliver_engine_event(EngineEvent::StageStatusLoaded {
        status: Arc::new(StageStatusSnapshot {
            staged: vec![
                StageEntry {
                    path: "run.sh".into(),
                    old_path: None,
                    kind: FileStatusKind::Modified,
                },
                StageEntry {
                    path: "eof.txt".into(),
                    old_path: None,
                    kind: FileStatusKind::Modified,
                },
                StageEntry {
                    path: "sym".into(),
                    old_path: None,
                    kind: FileStatusKind::Modified,
                },
            ],
            ..Default::default()
        }),
        epoch: 1,
    });
    ts.dispatch(Action::StageToggleGroupByDir);

    let run_row = ts
        .state()
        .stage_workbench
        .rows()
        .iter()
        .position(|r| r.key == "file:Staged:run.sh")
        .unwrap();
    ts.dispatch(Action::SelectCommitStageRow(run_row));
    let mode_diff = Arc::new(ThreeDocStageDiff {
        key: StageDiffKey {
            path: "run.sh".into(),
            old_path: None,
            side: StageDiffSide::Staged,
            context: DiffContext::Three,
            whitespace: jbgit_core::WhitespaceMode::Default,
        },
        vcs_exists: true,
        staged_exists: true,
        local_exists: true,
        vcs_bytes: b"#!/bin/sh\n".to_vec(),
        staged_bytes: b"#!/bin/sh\n".to_vec(),
        local_bytes: b"#!/bin/sh\n".to_vec(),
        file_mode: GIT_MODE_EXECUTABLE,
        mode_change: FileModeChange::from_modes(Some(GIT_MODE_REGULAR), Some(GIT_MODE_EXECUTABLE)),
        is_symlink: false,
        is_binary: false,
        binary: None,
        is_conflicted: false,
        submodule: None,
        lfs: None,
        tracker: StageLineTracker::new(b"#!/bin/sh\n", b"#!/bin/sh\n", b"#!/bin/sh\n"),
        hunks: Vec::new(),
        is_truncated: false,
        is_too_large: false,
    });
    let ep = ts
        .state()
        .request_slots
        .latest(RequestSlotId::StageFileDiff)
        .max(1);
    ts.deliver_engine_event(EngineEvent::StageFileDiffLoaded {
        diff: mode_diff,
        epoch: ep,
    });
    let mode_screen = screen_text(&ts.render(160, 36));
    assert!(mode_screen.contains("mode 100644 → 100755"), "{mode_screen}");
    assert!(
        !mode_screen.contains("No textual differences"),
        "pure mode change must not show 'No textual differences':\n{mode_screen}"
    );

    // 3. No-newline at EOF renders `\ No newline at end of file`.
    let eof_row = ts
        .state()
        .stage_workbench
        .rows()
        .iter()
        .position(|r| r.key == "file:Staged:eof.txt")
        .unwrap();
    ts.dispatch(Action::SelectCommitStageRow(eof_row));
    let tracker = StageLineTracker::new(b"hello\n", b"hello", b"hello");
    let (hunks, is_truncated) = tracker.build_interactive_hunks(true, DiffContext::Three, 20_000);
    let ep = ts
        .state()
        .request_slots
        .latest(RequestSlotId::StageFileDiff)
        .max(1);
    ts.deliver_engine_event(EngineEvent::StageFileDiffLoaded {
        diff: Arc::new(ThreeDocStageDiff {
            key: StageDiffKey {
                path: "eof.txt".into(),
                old_path: None,
                side: StageDiffSide::Staged,
                context: DiffContext::Three,
                whitespace: jbgit_core::WhitespaceMode::Default,
            },
            vcs_exists: true,
            staged_exists: true,
            local_exists: true,
            vcs_bytes: b"hello\n".to_vec(),
            staged_bytes: b"hello".to_vec(),
            local_bytes: b"hello".to_vec(),
            file_mode: GIT_MODE_REGULAR,
            mode_change: None,
            is_symlink: false,
            is_binary: false,
            binary: None,
            is_conflicted: false,
            submodule: None,
            lfs: None,
            tracker,
            hunks,
            is_truncated,
            is_too_large: false,
        }),
        epoch: ep,
    });
    let eof_screen = screen_text(&ts.render(160, 36));
    assert!(
        eof_screen.contains("\\ No newline at end of file"),
        "expected No newline marker in stage diff:\n{eof_screen}"
    );

    // 4. Symlink diff displays symlink header and blocks line-level unstaging.
    let sym_row = ts
        .state()
        .stage_workbench
        .rows()
        .iter()
        .position(|r| r.key == "file:Staged:sym")
        .unwrap();
    ts.dispatch(Action::SelectCommitStageRow(sym_row));
    let sym_tracker = StageLineTracker::new(b"old_target", b"new_target", b"new_target");
    let (sym_hunks, sym_trunc) =
        sym_tracker.build_interactive_hunks(true, DiffContext::Three, 20_000);
    let ep = ts
        .state()
        .request_slots
        .latest(RequestSlotId::StageFileDiff)
        .max(1);
    ts.deliver_engine_event(EngineEvent::StageFileDiffLoaded {
        diff: Arc::new(ThreeDocStageDiff {
            key: StageDiffKey {
                path: "sym".into(),
                old_path: None,
                side: StageDiffSide::Staged,
                context: DiffContext::Three,
                whitespace: jbgit_core::WhitespaceMode::Default,
            },
            vcs_exists: true,
            staged_exists: true,
            local_exists: true,
            vcs_bytes: b"old_target".to_vec(),
            staged_bytes: b"new_target".to_vec(),
            local_bytes: b"new_target".to_vec(),
            file_mode: GIT_MODE_SYMLINK,
            mode_change: FileModeChange::from_modes(Some(GIT_MODE_SYMLINK), Some(GIT_MODE_SYMLINK)),
            is_symlink: true,
            is_binary: false,
            binary: None,
            is_conflicted: false,
            submodule: None,
            lfs: None,
            tracker: sym_tracker,
            hunks: sym_hunks,
            is_truncated: sym_trunc,
            is_too_large: false,
        }),
        epoch: ep,
    });
    let sym_screen = screen_text(&ts.render(160, 36));
    assert!(sym_screen.contains("symlink target (120000)"), "{sym_screen}");
    let _ = ts.take_requests();
    ts.dispatch(Action::StageDiffUnstageSelection);
    assert!(
        ts.take_requests()
            .into_iter()
            .all(|(r, _)| !matches!(r, TaskRequest::ApplyStageBuffer { .. })),
        "line-level unstage on symlink must not emit ApplyStageBuffer"
    );
    assert!(
        ts.state()
            .status_notice
            .as_deref()
            .is_some_and(|n| n.contains("symbolic links")),
        "{:?}",
        ts.state().status_notice
    );
}
