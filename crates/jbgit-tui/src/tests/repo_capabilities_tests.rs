//! Acceptance tests for Spec 12 Issue 14 repository capabilities:
//! granular signature verification & Log column, read-only submodule handling,
//! Git LFS pointer placeholders, and shallow clone / unshallow workflows.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crossterm::event::MouseEventKind;
use jbgit_core::{
    CommitChangesBundle, CommitSignature, DetailsKey, DiffContext, FileStatusKind, GitOp, Oid,
    SignatureStatus, StageEntry, StageLineTracker, StageStatusSnapshot, SubmoduleState,
    WorkspaceTab,
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
        },
        vcs_exists: true,
        staged_exists: true,
        local_exists: true,
        vcs_bytes: Vec::new(),
        staged_bytes: Vec::new(),
        local_bytes: Vec::new(),
        file_mode: 0o160_000,
        is_binary: false,
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
