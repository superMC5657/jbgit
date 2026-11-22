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

use super::{
    TestSession, make_commit, make_details, make_snapshot, oid_n, screen_text, snapshot,
};

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
    assert_eq!(detail_reqs.len(), 1, "expected 1 coalesced details request: {reqs:?}");
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
    ts.state_mut().stage_workbench.load_draft(draft_path.clone());

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
        reqs.iter()
            .any(|(r, _)| matches!(r, TaskRequest::LoadStageFileDiff { key } if key.path == "b.rs")),
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
