//! End-to-end deterministic tests exercising the real [`TuiSession`] turn
//! driver, [`RecordingEngineBackend`], [`TestHost`], virtual clock advancement,
//! 64-event batch coalescing, and off-screen `TestBackend` rendering.

use std::sync::Arc;
use std::time::Duration;

use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use jbgit_core::{
    CommitChangesBundle, DetailsKey, FileChange, FileStatusKind, PaneId, RefreshScope,
    RequestSlotId, StageEntry, StageStatusSnapshot, WorkspaceTab,
};
use jbgit_engine::{EngineEvent, TaskRequest};

use crate::app::Action;
use crate::session::{ExternalEditorCall, MAX_COALESCE_EVENTS, SessionInput};

use super::{TestSession, make_commit, make_details, make_snapshot, oid_n, screen_text, snapshot};

#[test]
fn tui_session_caps_batch_at_64_events_and_coalesces_requests() {
    let commits: Vec<_> = (1..=80u8)
        .rev()
        .map(|i| {
            let parents = if i > 1 { vec![oid_n(i - 1)] } else { vec![] };
            make_commit(i, parents, &format!("commit-{i:02}"))
        })
        .collect();
    let snap = (*make_snapshot(".".into(), commits)).clone();
    let mut ts = TestSession::with_snapshot(snap);
    let _ = ts.take_requests();

    // Feed 75 Down key events in a single turn: TuiSession must cap at MAX_COALESCE_EVENTS (64).
    let events: Vec<Event> = (0..75)
        .map(|_| Event::Key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)))
        .collect();
    let report = ts.send_events(events);
    assert_eq!(report.processed_inputs, MAX_COALESCE_EVENTS);
    assert_eq!(ts.state().log.graph.selected_idx, MAX_COALESCE_EVENTS);

    // Before the 30ms debounce expires, no CommitDetails request has hit the engine.
    assert!(ts.take_requests().is_empty());

    // Advance virtual clock past 30ms: exactly ONE coalesced CommitDetails request fires
    // for the final selected commit (row 64 -> commit 80 - 64 = 16).
    ts.advance_by(Duration::from_millis(35));
    let reqs = ts.take_requests();
    let detail_reqs: Vec<_> = reqs
        .iter()
        .filter(|(r, _)| {
            matches!(
                r,
                TaskRequest::LoadCommitDetails {
                    slot: RequestSlotId::LogCommitDetails,
                    ..
                }
            )
        })
        .collect();
    assert_eq!(
        detail_reqs.len(),
        1,
        "expected 1 coalesced details request: {reqs:?}"
    );
    let expected_oid = ts.state().log_commit_at(MAX_COALESCE_EVENTS).unwrap().oid;
    assert!(matches!(
        &detail_reqs[0].0,
        TaskRequest::LoadCommitDetails { key, .. } if key == &DetailsKey::single(expected_oid)
    ));
}

#[test]
fn tui_session_virtual_clock_drives_30ms_120ms_and_300ms_debounces() {
    let mut ts = TestSession::with_snapshot(snapshot());
    let _ = ts.take_requests();

    // 1. 120ms text-filter debounce in Log tab: typing "fix" doesn't filter until 120ms elapses.
    ts.send_key(KeyCode::Char('/'), KeyModifiers::NONE);
    assert!(ts.state().log.filter.editing_text);
    for ch in ['f', 'i', 'x'] {
        ts.send_key(KeyCode::Char(ch), KeyModifiers::NONE);
    }
    assert!(ts.state().log.filter.text_apply_at.is_some());
    ts.advance_by(Duration::from_millis(50));
    assert!(
        ts.state().log.filter.text_apply_at.is_some(),
        "text filter must not fire before 120ms"
    );
    ts.advance_by(Duration::from_millis(80));
    assert!(
        ts.state().log.filter.text_apply_at.is_none(),
        "text filter must apply once 120ms elapses"
    );
    assert_eq!(ts.state().log.filter.spec.text.query, "fix");

    // 2. 300ms commit-message draft debounce in Commit tab (zero thread::sleep).
    let dir = tempfile::tempdir().unwrap();
    let draft_path = dir.path().join("draft.txt");
    ts.state_mut()
        .stage_workbench
        .load_draft(draft_path.clone());

    ts.dispatch(Action::SwitchTab(WorkspaceTab::Commit));
    ts.dispatch(Action::FocusPane(PaneId::CommitMessageEditor));
    ts.send_event(Event::Paste("feat: virtual clock draft".into()));
    assert!(ts.state().stage_workbench.draft_dirty());
    assert!(!draft_path.exists());

    // Advance 200ms -> still debouncing.
    ts.advance_by(Duration::from_millis(200));
    assert!(ts.state().stage_workbench.draft_dirty());

    // Advance another 120ms (total 320ms >= 300ms) -> background save scheduled.
    ts.advance_by(Duration::from_millis(120));
    assert!(!ts.state().stage_workbench.draft_dirty());
    ts.state_mut().stage_workbench.flush_draft_sync();
    assert_eq!(
        std::fs::read_to_string(&draft_path).unwrap(),
        "feat: virtual clock draft"
    );

    // 3. 30ms stage file diff debounce.
    ts.deliver_engine_event(EngineEvent::StageStatusLoaded {
        status: Arc::new(StageStatusSnapshot {
            unstaged: vec![
                StageEntry {
                    path: "a.rs".into(),
                    old_path: None,
                    kind: FileStatusKind::Modified,
                },
                StageEntry {
                    path: "b.rs".into(),
                    old_path: None,
                    kind: FileStatusKind::Modified,
                },
            ],
            ..Default::default()
        }),
        epoch: 1,
    });
    ts.dispatch(Action::FocusPane(PaneId::CommitChangesTree));
    let _ = ts.take_requests();

    ts.send_key(KeyCode::Down, KeyModifiers::NONE);
    ts.send_key(KeyCode::Down, KeyModifiers::NONE);
    // Selection moved via keyboard; advance 35ms to fire the 30ms stage diff debounce.
    ts.advance_by(Duration::from_millis(35));
    let reqs = ts.take_requests();
    assert!(
        reqs.iter().any(
            |(r, _)| matches!(r, TaskRequest::LoadStageFileDiff { key } if key.path == "b.rs")
        ),
        "expected debounced StageFileDiff for b.rs, got {reqs:?}"
    );
}

#[test]
fn tui_session_host_isolates_clipboard_and_external_editor_handoffs() {
    let mut ts = TestSession::with_snapshot(snapshot());
    let _ = ts.take_requests();

    // Copying commit hash via 'y' goes to TestHost's in-memory clipboard, never OS clipboard.
    ts.send_key(KeyCode::Char('y'), KeyModifiers::NONE);
    assert_eq!(ts.clipboard().len(), 1);
    assert_eq!(ts.clipboard()[0].len(), 40);

    // External file editor handoff stops event coalescing, records the call on TestHost,
    // and automatically requests a StageStatus refresh via EngineBackend.
    let report = ts.step([
        SessionInput::Action(Action::OpenExternalEditorFile(Some("src/main.rs".into()))),
        SessionInput::Action(Action::SwitchTab(WorkspaceTab::Console)),
    ]);
    assert!(report.stopped_for_handoff);
    assert_eq!(
        report.processed_inputs, 1,
        "batch must stop immediately once a terminal handoff effect is produced"
    );
    assert_eq!(
        ts.host.external_editor_calls,
        vec![ExternalEditorCall::File("src/main.rs".into())]
    );
    let reqs = ts.take_requests();
    assert!(
        reqs.iter()
            .any(|(r, _)| matches!(r, TaskRequest::LoadStageStatus { .. })),
        "external file editor must trigger StageStatus refresh: {reqs:?}"
    );

    // Resize stops batching and notifies TestHost.
    let resize_report = ts.send_events([
        Event::Resize(120, 40),
        Event::Key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)),
    ]);
    assert!(resize_report.stopped_for_resize);
    assert_eq!(resize_report.processed_inputs, 1);
    assert_eq!(ts.host.resize_count, 1);

    // Rendering via TuiSession::render draws all tabs cleanly without panics.
    let buf = ts.render(120, 35);
    let text = screen_text(&buf);
    assert!(text.contains("[1] Commit"));
    assert!(text.contains("[2] Log"));
    assert!(text.contains("[3] Stash"));
    assert!(text.contains("[4] Console"));

    // Delivering CommitDetailsLoaded through TuiSession renders the changes in the Log tab.
    let top_oid = ts.state().log_commit_at(0).unwrap().oid;
    let key = DetailsKey::single(top_oid);
    ts.deliver_engine_event(EngineEvent::CommitDetailsLoaded {
        slot: RequestSlotId::LogCommitDetails,
        bundle: Arc::new(CommitChangesBundle {
            key,
            details: vec![make_details(top_oid, vec![], "feat: session test")],
            changes: vec![FileChange {
                path: "src/session.rs".into(),
                old_path: None,
                kind: FileStatusKind::Added,
                additions: 12,
                deletions: 0,
                is_binary: false,
                exists_in_workdir: true,
                old_oid: None,
                new_oid: None,
            }],
        }),
        epoch: 1,
    });
    let buf = ts.render(140, 35);
    assert!(screen_text(&buf).contains("session.rs"));

    // Verify RefreshScope::StageStatus from external mergetool works identically.
    let _ = ts.take_requests();
    ts.dispatch(Action::OpenExternalMergetool("conflict.rs".into()));
    assert_eq!(
        ts.host.external_editor_calls.last(),
        Some(&ExternalEditorCall::Mergetool("conflict.rs".into()))
    );
    let reqs = ts.take_requests();
    assert!(reqs.iter().any(|(r, _)| matches!(
        r,
        TaskRequest::LoadStageStatus { .. }
            | TaskRequest::RunGitCli {
                refresh_after: RefreshScope::StageStatus,
                ..
            }
    )));
}

#[test]
fn stamped_clone_and_rebind_drop_stale_tasks_invalidate_tree_syntax_and_sync_credentials() {
    use crate::app::{ModalLayer, UiAsyncEvent};
    use crate::repo_lifecycle::{CloneDialogAction, CloneField};

    let tmp = tempfile::TempDir::new().unwrap();
    let repo_dir = tmp.path().join("repo-a");
    let cloned_dir = tmp.path().join("repo-cloned");
    std::fs::create_dir_all(&repo_dir).unwrap();
    std::fs::create_dir_all(&cloned_dir).unwrap();

    let mut snap = snapshot();
    snap.workdir = repo_dir.clone();
    let mut ts = TestSession::with_snapshot(snap);

    // 1. Start clone task #1, cancel it, start clone task #2 -> stale task #1 completion is dropped.
    ts.dispatch(Action::OpenCloneRepo {
        base_dir: Some(tmp.path().to_path_buf()),
    });
    ts.dispatch(Action::CloneRepoUpdate(CloneDialogAction::SetFocus(
        CloneField::Url,
    )));
    ts.type_str("https://example.com/repo-cloned.git");
    ts.dispatch(Action::CloneRepoSubmit);
    let (task1, _) = ts
        .session
        .take_clone_repo_task()
        .expect("task 1 must be queued");

    ts.dispatch(Action::CloneRepoCancel);
    assert!(ts.session.take_clone_cancel_request());

    // Deliver late stamped completion from cancelled task #1 -> must be ignored.
    let gen0 = ts.session.generation();
    ts.session.step_turn(
        [SessionInput::UiAsyncFrom(
            gen0,
            UiAsyncEvent::TaskStamped {
                task_id: task1,
                event: Box::new(UiAsyncEvent::CloneFinished(Ok(cloned_dir.clone()))),
            },
        )],
        ts.session.now(),
        Some(&ts.engine),
        &mut ts.host,
    );
    assert!(
        ts.session.take_repo_switch_request().is_none(),
        "cancelled clone task completion must not trigger repo switch"
    );

    // 2. Start clone task #2, then rebind to another repo -> old generation completion is dropped,
    //    tree_syntax is invalidated on prepare_unbind, and queued credentials re-sync on rebind.
    ts.dispatch(Action::CloneRepoSubmit);
    let (task2, _) = ts
        .session
        .take_clone_repo_task()
        .expect("task 2 must be queued");

    let tree_key = jbgit_core::TreeBlobKey {
        oid: jbgit_core::Oid::from_bytes([4u8; 20]),
        path: "src/lib.rs".into(),
    };
    let (tree_epoch, _) = ts
        .state_mut()
        .tree_syntax
        .begin_cancelable(tree_key)
        .expect("tree syntax job should start");

    let (cred_req, _cred_rx) = jbgit_engine::askpass::AskpassRequest::new("Password:".to_string());
    ts.deliver_ui_async(UiAsyncEvent::Askpass(cred_req));
    assert!(!ts.state().credentials.is_empty());

    let _ = ts.session.prepare_unbind();
    assert!(
        !ts.state().tree_syntax.is_busy(),
        "prepare_unbind must invalidate tree_syntax (epoch {tree_epoch})"
    );

    let next_state = super::test_state(tmp.path().join("repo-b"));
    ts.session.rebind(next_state, true);
    assert!(
        ts.state().modal_stack.iter().any(|l| matches!(
            l,
            ModalLayer::Dialog(d) if matches!(d.as_ref(), crate::dialogs::Dialog::Credential(_))
        )),
        "rebind must call sync_credential_dialog() to re-open carried credential prompt"
    );

    // Deliver late task #2 completion stamped with pre-rebind generation -> dropped!
    ts.session.step_turn(
        [SessionInput::UiAsyncFrom(
            gen0,
            UiAsyncEvent::TaskStamped {
                task_id: task2,
                event: Box::new(UiAsyncEvent::CloneFinished(Ok(cloned_dir))),
            },
        )],
        ts.session.now(),
        Some(&ts.engine),
        &mut ts.host,
    );
    assert!(
        ts.session.take_repo_switch_request().is_none(),
        "pre-rebind clone task must not hijack newly bound repository"
    );
}

#[test]
fn log_flush_pending_respects_details_deadline() {
    let mut ts = TestSession::with_snapshot(snapshot());
    let _ = ts.take_requests();
    // Move selection to arm the 30ms details debounce timer.
    ts.send_key(KeyCode::Down, KeyModifiers::NONE);
    assert!(ts.state().log.details.pending.is_some());
    let _ = ts.take_requests();

    // Flush BEFORE the 30ms deadline elapses (e.g. another timer woke the loop).
    ts.step([SessionInput::FlushPendingDetails]);
    let early_reqs = ts.take_requests();
    assert!(
        early_reqs.is_empty(),
        "flush_pending must not emit LoadCommitDetails before deadline: {early_reqs:?}"
    );
    assert!(
        ts.state().log.details.pending.is_some(),
        "pending details entry must remain armed until deadline"
    );

    // Advance past deadline -> now emits LoadCommitDetails.
    ts.advance_by(Duration::from_millis(40));
    let due_reqs = ts.take_requests();
    assert!(
        due_reqs
            .iter()
            .any(|(r, _)| matches!(r, TaskRequest::LoadCommitDetails { .. })),
        "flush_pending must emit LoadCommitDetails once deadline passes: {due_reqs:?}"
    );
}
