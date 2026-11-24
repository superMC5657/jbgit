use std::collections::HashMap;
use std::sync::Arc;

use crossterm::event::{KeyCode, KeyModifiers};
use jbgit_core::{
    BlameBundle, BlameCommit, BlameKey, BlameLine, BlameOptions, CommitChangesBundle, DetailsKey,
    DiffContext, FileChange, FileDiffKey, FileHistoryBundle, FileHistoryEntry, FileHistoryKey,
    FileStatusKind, Oid, PaneId, PersonStamp, RequestSlotId, StageEntry, StageStatusSnapshot,
    WorkspaceTab,
};
use jbgit_engine::{
    BlameFailureKind, EngineEvent, StageDiffKey, StageDiffSide, TaskRequest, ThreeDocStageDiff,
};

use crate::app::{Action, ChangesHost, ModalLayer, SyntaxJobKey, UiAsyncEvent};
use crate::dialogs::Dialog;
use crate::hit_test::HitTarget;
use crate::menus::{log_change_menu_items, stage_diff_menu_items, stage_tree_menu_items};
use crate::syntax::{HighlightedDiff, HighlightedFile};

use super::{TestSession, oid, oid_n, screen_text, snapshot, top_menu};

fn stamp(name: &str, time: i64) -> PersonStamp {
    PersonStamp {
        name: name.to_string(),
        email: format!("{}@example.com", name.to_lowercase()),
        time,
        offset_minutes: 0,
    }
}

fn sample_blame_bundle(c1: Oid, c2: Oid) -> Arc<BlameBundle> {
    let mut commits = HashMap::new();
    commits.insert(
        c1,
        BlameCommit {
            oid: c1,
            author: stamp("Alice", 1_700_000_000),
            committer_time: 1_700_000_000,
            committer_offset_minutes: 0,
            summary: "initial auth module".to_string(),
            previous: None,
            is_boundary: true,
        },
    );
    commits.insert(
        c2,
        BlameCommit {
            oid: c2,
            author: stamp("Bob", 1_700_086_400),
            committer_time: 1_700_090_000,
            committer_offset_minutes: 0,
            summary: "refine token expiry check".to_string(),
            previous: Some((c1, "src/old_auth.rs".to_string())),
            is_boundary: false,
        },
    );
    let zero = Oid::from_bytes([0u8; 20]);
    commits.insert(
        zero,
        BlameCommit {
            oid: zero,
            author: stamp("Not Committed Yet", 0),
            committer_time: 0,
            committer_offset_minutes: 0,
            summary: "Not Committed Yet".to_string(),
            previous: None,
            is_boundary: false,
        },
    );

    Arc::new(BlameBundle {
        key: BlameKey {
            path: "src/auth.rs".to_string(),
            rev: None,
            options: BlameOptions::default(),
        },
        lines: vec![
            BlameLine {
                final_line: 1,
                orig_line: 1,
                oid: c1,
                orig_path: "src/auth.rs".to_string(),
            },
            BlameLine {
                final_line: 2,
                orig_line: 2,
                oid: c1,
                orig_path: "src/auth.rs".to_string(),
            },
            BlameLine {
                final_line: 3,
                orig_line: 10,
                oid: c2,
                orig_path: "src/auth.rs".to_string(),
            },
            BlameLine {
                final_line: 4,
                orig_line: 4,
                oid: zero,
                orig_path: "src/auth.rs".to_string(),
            },
        ],
        commits,
        content: Arc::from(
            "pub fn auth() {\n    let ttl = 60;\n    validate(ttl);\n    audit_log();\n}",
        ),
        uncommitted_oid_present: true,
    })
}

#[test]
fn annotate_entry_points_menus_and_alt_g_open_blame_modal() {
    let snap = snapshot();
    let c1 = snap.commits[0].oid;
    let c2 = snap.commits[1].oid;
    let mut ts = TestSession::with_snapshot(snap.clone());

    // 1. LogChangesTree + <A-g> + menu item
    ts.dispatch(Action::SwitchTab(WorkspaceTab::Log));
    ts.dispatch(Action::SelectLogRow(0));
    ts.dispatch(Action::FocusPane(PaneId::LogChangesTree));
    let _ = ts.take_requests();

    ts.deliver_engine_event(EngineEvent::CommitDetailsLoaded {
        slot: RequestSlotId::LogCommitDetails,
        bundle: Arc::new(CommitChangesBundle {
            key: DetailsKey::single(c1),
            details: Vec::new(),
            changes: vec![FileChange {
                path: "src/auth.rs".into(),
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
        epoch: 1,
    });

    ts.dispatch(Action::ChangesSelect(ChangesHost::Log, 1));
    let menu = log_change_menu_items(ts.state());
    let ann_item = menu
        .iter()
        .find(|i| i.label.contains("Annotate"))
        .expect("log_change_menu_items includes Annotate");
    assert!(ann_item.is_enabled());
    assert_eq!(ann_item.shortcut.as_deref(), Some("Alt+G"));

    ts.send_key(KeyCode::Char('g'), KeyModifiers::ALT);
    assert!(matches!(
        ts.state().modal_stack.last(),
        Some(ModalLayer::Annotate(ann))
            if ann.key.path == "src/auth.rs" && ann.key.rev == Some(c1)
    ));
    let reqs = ts.take_requests();
    assert!(reqs.iter().any(|(r, _)| matches!(
        r,
        TaskRequest::LoadBlame { key } if key.path == "src/auth.rs" && key.rev == Some(c1)
    )));
    ts.send_key(KeyCode::Esc, KeyModifiers::NONE);
    assert!(ts.state().modal_stack.is_empty());

    // LogDetailsDiff + <A-g>
    ts.dispatch(Action::FocusPane(PaneId::LogDetailsDiff));
    ts.send_key(KeyCode::Char('g'), KeyModifiers::ALT);
    assert!(ts.state().modal_stack.annotate().is_some());
    ts.send_key(KeyCode::Esc, KeyModifiers::NONE);

    // 2. Stage Tree (tracked allowed, untracked refused) + Stage Diff (<A-g>)
    ts.dispatch(Action::SwitchTab(WorkspaceTab::Commit));
    ts.dispatch(Action::FocusPane(PaneId::CommitChangesTree));
    ts.deliver_engine_event(EngineEvent::StageStatusLoaded {
        status: Arc::new(StageStatusSnapshot {
            staged: Vec::new(),
            unstaged: vec![StageEntry {
                path: "src/auth.rs".into(),
                old_path: None,
                kind: FileStatusKind::Modified,
            }],
            untracked: vec![StageEntry {
                path: "notes.tmp".into(),
                old_path: None,
                kind: FileStatusKind::Untracked,
            }],
            ignored: Vec::new(),
            conflicted: Vec::new(),
            conflict_labels: Default::default(),
            submodules: Default::default(),
        }),
        epoch: 1,
    });
    let tracked_idx = ts
        .state()
        .stage_workbench
        .rows()
        .iter()
        .position(|r| r.is_file() && r.key.ends_with("src/auth.rs"))
        .unwrap();
    let untracked_idx = ts
        .state()
        .stage_workbench
        .rows()
        .iter()
        .position(|r| r.is_file() && r.key.ends_with("notes.tmp"))
        .unwrap();

    let tree_menu = stage_tree_menu_items(ts.state(), tracked_idx);
    assert!(
        tree_menu
            .iter()
            .any(|i| i.label.contains("Annotate") && i.is_enabled())
    );
    let untracked_menu = stage_tree_menu_items(ts.state(), untracked_idx);
    let bad_ann = untracked_menu
        .iter()
        .find(|i| i.label.contains("Annotate"))
        .unwrap();
    assert!(!bad_ann.is_enabled());

    ts.dispatch(Action::SelectCommitStageRow(tracked_idx));
    let _ = ts.take_requests();
    ts.send_key(KeyCode::Char('g'), KeyModifiers::ALT);
    assert!(matches!(
        ts.state().modal_stack.last(),
        Some(ModalLayer::Annotate(ann)) if ann.key.path == "src/auth.rs" && ann.key.rev.is_none()
    ));
    ts.send_key(KeyCode::Esc, KeyModifiers::NONE);

    // 3. File History ('b' and <A-g>) stacks Annotate cleanly above FileHistory
    ts.dispatch(Action::OpenFileHistory {
        path: "src/auth.rs".into(),
        start_oid: None,
        follow_renames: true,
        line_range: None,
    });
    let fh_epoch = ts
        .state()
        .request_slots
        .latest(RequestSlotId::FileHistoryList);
    ts.deliver_engine_event(EngineEvent::FileHistoryLoaded {
        bundle: Arc::new(FileHistoryBundle {
            key: FileHistoryKey::new("src/auth.rs", None, true, None),
            entries: vec![FileHistoryEntry {
                oid: c2,
                author_name: "Bob".into(),
                author_time: 1_700_086_400,
                summary: "refine token expiry check".into(),
                kind: FileStatusKind::Modified,
                path_at_commit: "src/auth.rs".into(),
                old_path: None,
                additions: 1,
                deletions: 1,
                is_binary: false,
                range_patch: None,
            }],
            truncated: false,
        }),
        epoch: fh_epoch,
    });
    ts.send_key(KeyCode::Char('b'), KeyModifiers::NONE);
    assert_eq!(ts.state().modal_stack.len(), 2);
    assert!(matches!(
        ts.state().modal_stack.last(),
        Some(ModalLayer::Annotate(ann)) if ann.key.rev == Some(c2) && ann.key.path == "src/auth.rs"
    ));
    ts.send_key(KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(ts.state().modal_stack.len(), 1);
    assert!(ts.state().modal_stack.file_history().is_some());

    // <A-g> inside FileHistory also stacks Annotate
    let _ = snap.commits.len();
    ts.send_key(KeyCode::Char('g'), KeyModifiers::ALT);
    assert_eq!(ts.state().modal_stack.len(), 2);
    assert!(matches!(
        ts.state().modal_stack.last(),
        Some(ModalLayer::Annotate(_))
    ));
}

#[test]
fn annotate_renders_hunk_dedup_uncommitted_toggles_prev_rev_diff_goto_and_errors() {
    let snap = snapshot();
    let c1 = snap.commits[0].oid;
    let c2 = snap.commits[1].oid;
    let mut ts = TestSession::with_snapshot(snap);

    ts.dispatch(Action::OpenAnnotate {
        path: "src/auth.rs".into(),
        rev: None,
    });
    let ep1 = ts.state().request_slots.latest(RequestSlotId::Annotate);
    let bundle = sample_blame_bundle(c1, c2);
    ts.deliver_engine_event(EngineEvent::BlameLoaded {
        bundle: Arc::clone(&bundle),
        epoch: ep1,
    });
    ts.deliver_ui_async(UiAsyncEvent::BlameSyntaxHighlighted {
        key: bundle.key.clone(),
        epoch: ts.state().blame_syntax.latest_epoch(),
        highlighted: Arc::new(HighlightedFile {
            lines: vec![Vec::new(); 4],
        }),
    });

    let buf = ts.render(120, 32);
    let txt = screen_text(&buf);
    assert!(txt.contains("src/auth.rs"));
    assert!(txt.contains("Working Tree"));
    assert!(txt.contains("Alice"));
    assert!(txt.contains("Bob"));
    assert!(txt.contains("Local"));
    // Contiguous hunk for c1 covers lines 1 and 2: short(c1) appears on line 1's gutter row
    // and in the bottom detail strip, while line 2's gutter is deduplicated.
    let line1_row = txt.lines().find(|l| l.contains("pub fn auth()")).unwrap();
    let line2_row = txt.lines().find(|l| l.contains("let ttl = 60;")).unwrap();
    assert!(line1_row.contains(&c1.short()));
    assert!(!line2_row.contains(&c1.short()));

    // Move to line 3 (index 2, Bob c2) and toggle -w / -M preserving focus line
    ts.send_key(KeyCode::Char('j'), KeyModifiers::NONE);
    ts.send_key(KeyCode::Char('j'), KeyModifiers::NONE);
    assert_eq!(ts.state().modal_stack.annotate().unwrap().cursor, 2);
    let _ = ts.take_requests();

    ts.send_key(KeyCode::Char('w'), KeyModifiers::NONE);
    let ep_w = ts.state().request_slots.latest(RequestSlotId::Annotate);
    assert!(ep_w > ep1);
    let reqs = ts.take_requests();
    assert!(reqs.iter().any(|(r, _)| matches!(
        r,
        TaskRequest::LoadBlame { key }
            if key.options.ignore_whitespace
                && key.options.move_detection == jbgit_core::MoveDetection::None
    )));
    let mut b_w = (*bundle).clone();
    b_w.key.options.ignore_whitespace = true;
    ts.deliver_engine_event(EngineEvent::BlameLoaded {
        bundle: Arc::new(b_w),
        epoch: ep_w,
    });
    assert_eq!(ts.state().modal_stack.annotate().unwrap().cursor, 2);

    ts.send_key(KeyCode::Char('m'), KeyModifiers::NONE);
    let ep_m = ts.state().request_slots.latest(RequestSlotId::Annotate);
    let reqs = ts.take_requests();
    assert!(reqs.iter().any(|(r, _)| matches!(
        r,
        TaskRequest::LoadBlame { key }
            if key.options.ignore_whitespace
                && key.options.move_detection == jbgit_core::MoveDetection::WithinFile
    )));
    let mut b_m = (*bundle).clone();
    b_m.key.options.ignore_whitespace = true;
    b_m.key.options.move_detection = jbgit_core::MoveDetection::WithinFile;
    ts.deliver_engine_event(EngineEvent::BlameLoaded {
        bundle: Arc::new(b_m),
        epoch: ep_m,
    });

    // 'p' Annotate Previous Revision (from c2 -> c1 / src/old_auth.rs) & 'P' nav back
    ts.send_key(KeyCode::Char('p'), KeyModifiers::NONE);
    let ep_p = ts.state().request_slots.latest(RequestSlotId::Annotate);
    assert_eq!(ts.state().modal_stack.annotate().unwrap().nav_back.len(), 1);
    let reqs = ts.take_requests();
    assert!(reqs.iter().any(|(r, _)| matches!(
        r,
        TaskRequest::LoadBlame { key }
            if key.path == "src/old_auth.rs" && key.rev == Some(c1)
    )));
    // 'P' nav back hits DiffCacheStore blame LRU cache with ZERO new LoadBlame requests!
    ts.send_key(KeyCode::Char('P'), KeyModifiers::SHIFT);
    let ep_back = ts.state().request_slots.latest(RequestSlotId::Annotate);
    assert_eq!(
        ep_back, ep_p,
        "nav_back to a cached blame key must hit DiffCacheStore without re-requesting git blame"
    );
    let back_reqs = ts.take_requests();
    assert!(
        !back_reqs
            .iter()
            .any(|(r, _)| matches!(r, TaskRequest::LoadBlame { .. })),
        "cached nav_back must emit zero LoadBlame requests"
    );
    let ann_back = ts.state().modal_stack.annotate().unwrap();
    assert!(ann_back.nav_back.is_empty());
    assert!(!ann_back.loading);
    assert!(ann_back.bundle.is_some());

    // Go-to-Line via ':' -> "1" -> Enter
    ts.send_key(KeyCode::Char(':'), KeyModifiers::NONE);
    ts.type_str("1");
    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(ts.state().modal_stack.annotate().unwrap().cursor, 0);

    // Root commit c1 has no previous -> notice instead of crash
    ts.send_key(KeyCode::Char('p'), KeyModifiers::NONE);
    assert_eq!(
        ts.state().status_notice.as_deref(),
        Some(ts.state().i18n.t("toast.annotate_no_previous"))
    );

    // Toggle inline Diff ('d') + Tab focus switch + debounce load via RequestSlotId::AnnotateDiff
    ts.send_key(KeyCode::Char('d'), KeyModifiers::NONE);
    assert!(ts.state().modal_stack.annotate().unwrap().show_diff);
    let _ = ts.take_requests();
    ts.advance_by(crate::log_view::DETAILS_DEBOUNCE);
    let diff_reqs = ts.take_requests();
    assert!(diff_reqs.iter().any(|(r, _)| matches!(
        r,
        TaskRequest::LoadFileDiff { slot: RequestSlotId::AnnotateDiff, key, .. }
            if key.details.oids == vec![c1]
    )));
    ts.send_key(KeyCode::Tab, KeyModifiers::NONE);
    assert_eq!(
        ts.state().modal_stack.annotate().unwrap().focus,
        crate::log_view::AnnotateFocus::Diff
    );
    ts.send_key(KeyCode::Tab, KeyModifiers::NONE);
    assert_eq!(
        ts.state().modal_stack.annotate().unwrap().focus,
        crate::log_view::AnnotateFocus::Code
    );

    // Single 'g' timeout ONLY clears pending_g in Annotate (must NOT close modal or jump to Log)
    ts.send_key(KeyCode::Char('j'), KeyModifiers::NONE);
    assert_eq!(ts.state().modal_stack.annotate().unwrap().cursor, 1);
    ts.send_key(KeyCode::Char('g'), KeyModifiers::NONE);
    assert!(ts.state().modal_stack.annotate().unwrap().pending_g.is_some());
    ts.advance_by(crate::log_view::FILE_HISTORY_G_WINDOW);
    assert!(ts.state().modal_stack.annotate().is_some());
    assert!(ts.state().modal_stack.annotate().unwrap().pending_g.is_none());

    // Double 'gg' jumps back to line 0
    ts.send_key(KeyCode::Char('g'), KeyModifiers::NONE);
    ts.send_key(KeyCode::Char('g'), KeyModifiers::NONE);
    assert_eq!(ts.state().modal_stack.annotate().unwrap().cursor, 0);

    // Enter on uncommitted line sets notice; Enter on c2 jumps to Log
    ts.send_key(KeyCode::Char('G'), KeyModifiers::SHIFT);
    assert_eq!(ts.state().modal_stack.annotate().unwrap().cursor, 3);
    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);
    assert!(ts.state().modal_stack.annotate().is_some());
    assert_eq!(
        ts.state().status_notice.as_deref(),
        Some(ts.state().i18n.t("toast.annotate_uncommitted"))
    );

    ts.send_key(KeyCode::Char('k'), KeyModifiers::NONE); // cursor = 2 (c2)
    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);
    assert!(ts.state().modal_stack.is_empty());
    assert_eq!(ts.state().active_tab, WorkspaceTab::Log);
    assert_eq!(
        ts.state()
            .log
            .graph
            .selected_oids(ts.state().repo_snapshot.as_ref()),
        vec![c2]
    );

    // Error rendering: TooLarge
    ts.dispatch(Action::OpenAnnotate {
        path: "big.bin".into(),
        rev: None,
    });
    let ep_err = ts.state().request_slots.latest(RequestSlotId::Annotate);
    ts.deliver_engine_event(EngineEvent::BlameFailed {
        key: BlameKey {
            path: "big.bin".into(),
            rev: None,
            options: BlameOptions::default(),
        },
        kind: BlameFailureKind::TooLarge {
            limit: 4 * 1024 * 1024,
        },
        message: "too large".into(),
        epoch: ep_err,
    });
    let err_txt = screen_text(&ts.render(100, 24));
    assert!(err_txt.contains("4 MiB"));
}

#[test]
fn selection_history_from_annotate_and_stage_diff_renders_range_patch_and_confirms_large() {
    let snap = snapshot();
    let c1 = snap.commits[0].oid;
    let c2 = snap.commits[1].oid;
    let mut ts = TestSession::with_snapshot(snap);

    ts.dispatch(Action::OpenAnnotate {
        path: "src/auth.rs".into(),
        rev: Some(c2),
    });
    let ep = ts.state().request_slots.latest(RequestSlotId::Annotate);
    let mut b = (*sample_blame_bundle(c1, c2)).clone();
    b.key.rev = Some(c2);
    ts.deliver_engine_event(EngineEvent::BlameLoaded {
        bundle: Arc::new(b),
        epoch: ep,
    });
    // Ensure Annotate hit regions exist first
    let _ = ts.render(120, 32);

    // Visual selection lines 2..=3 via 'V' + 'j' + 'H'
    ts.send_key(KeyCode::Char('j'), KeyModifiers::NONE);
    ts.send_key(KeyCode::Char('V'), KeyModifiers::SHIFT);
    ts.send_key(KeyCode::Char('j'), KeyModifiers::NONE);
    let _ = ts.take_requests();
    ts.send_key(KeyCode::Char('H'), KeyModifiers::SHIFT);

    assert_eq!(ts.state().modal_stack.len(), 2);
    assert!(matches!(
        ts.state().modal_stack.last(),
        Some(ModalLayer::FileHistory(fh))
            if fh.key.path == "src/auth.rs"
                && fh.key.start_oid == Some(c2)
                && fh.key.line_range == Some((2, 3))
                && !fh.key.follow_renames
    ));
    let reqs = ts.take_requests();
    assert!(reqs.iter().any(|(r, _)| matches!(
        r,
        TaskRequest::LoadFileHistory { key } if key.line_range == Some((2, 3)) && !key.follow_renames
    )));

    // Pressing 'f' in selection history is a no-op (must NOT dispatch LoadFileHistory)
    ts.send_key(KeyCode::Char('f'), KeyModifiers::NONE);
    assert!(ts.take_requests().is_empty());

    // Deliver FileHistoryLoaded with range_patch; verify ZERO LoadFileDiff / diff_caches insertion
    let fh_ep = ts
        .state()
        .request_slots
        .latest(RequestSlotId::FileHistoryList);
    ts.deliver_engine_event(EngineEvent::FileHistoryLoaded {
        bundle: Arc::new(FileHistoryBundle {
            key: FileHistoryKey::new("src/auth.rs", Some(c2), false, Some((2, 3))),
            entries: vec![FileHistoryEntry {
                oid: c2,
                author_name: "Bob".into(),
                author_time: 1_700_086_400,
                summary: "refine token expiry check".into(),
                kind: FileStatusKind::Modified,
                path_at_commit: "src/auth.rs".into(),
                old_path: None,
                additions: 1,
                deletions: 1,
                is_binary: false,
                range_patch: Some(Arc::from(
                    "@@ -2,2 +2,2 @@\n-    let ttl = 30;\n+    let ttl = 60;\n     validate(ttl);",
                )),
            }],
            truncated: false,
        }),
        epoch: fh_ep,
    });
    assert!(!ts.take_requests().iter().any(|(r, _)| matches!(
        r,
        TaskRequest::LoadFileDiff { .. }
    )));
    let diff_key = FileDiffKey {
        details: DetailsKey::single(c2),
        path: "src/auth.rs".into(),
        old_path: None,
        context: DiffContext::Three,
        whitespace: jbgit_core::WhitespaceMode::Default,
    };
    assert!(ts.state().diff_caches.peek_diff(&diff_key).is_none());

    // Deliver range syntax event and verify screen displays path:start-end + [Range Patch] badge
    let r_ep = ts.state().range_syntax.latest_epoch();
    ts.deliver_ui_async(UiAsyncEvent::SyntaxHighlighted {
        key: SyntaxJobKey::RangePatch {
            oid: c2,
            path: "src/auth.rs".into(),
            range: (2, 3),
        },
        epoch: r_ep,
        highlighted: Arc::new(HighlightedDiff::default()),
    });
    let buf = ts.render(120, 32);
    let txt = screen_text(&buf);
    assert!(txt.contains("src/auth.rs:2-3"));
    assert!(txt.contains("Range Patch"));
    assert!(txt.contains("let ttl = 60;"));

    // Lower occluded Annotate Layer 2 must NOT leak hit regions into hit_registry
    assert!(ts.state().hit_registry.find(|t| matches!(
        t,
        HitTarget::AnnotateLine { .. }
            | HitTarget::AnnotateToolbar(_)
            | HitTarget::AnnotateArea
            | HitTarget::AnnotateDiffArea
    )).is_none());

    // Esc pops Selection History cleanly back to Annotate
    ts.send_key(KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(ts.state().modal_stack.len(), 1);
    assert!(ts.state().modal_stack.annotate().is_some());

    // Span > 2000 lines prompts confirmation ChoiceDialog
    ts.dispatch(Action::OpenSelectionHistory {
        path: "src/auth.rs".into(),
        start_oid: None,
        start_line: 1,
        end_line: 2500,
    });
    assert!(matches!(
        ts.state().modal_stack.last(),
        Some(ModalLayer::Dialog(d)) if matches!(d.as_ref(), Dialog::Choice(_))
    ));
    // Cancel closes dialog without opening Selection History
    ts.send_key(KeyCode::Esc, KeyModifiers::NONE);
    assert!(ts.state().modal_stack.annotate().is_some());
    assert!(ts.state().modal_stack.file_history().is_none());

    // Confirm opens Selection History
    ts.dispatch(Action::OpenSelectionHistory {
        path: "src/auth.rs".into(),
        start_oid: None,
        start_line: 1,
        end_line: 2500,
    });
    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);
    assert!(matches!(
        ts.state().modal_stack.last(),
        Some(ModalLayer::FileHistory(fh)) if fh.key.line_range == Some((1, 2500))
    ));

    // Close modals and test Stage Diff Visual selection -> Context Menu "Show History for Selection…"
    ts.dispatch(Action::PopModal);
    ts.dispatch(Action::PopModal);
    assert!(ts.state().modal_stack.is_empty());

    ts.dispatch(Action::SwitchTab(WorkspaceTab::Commit));
    ts.dispatch(Action::FocusPane(PaneId::CommitChangesTree));
    ts.deliver_engine_event(EngineEvent::StageStatusLoaded {
        status: Arc::new(StageStatusSnapshot {
            staged: Vec::new(),
            unstaged: vec![StageEntry {
                path: "src/auth.rs".into(),
                old_path: None,
                kind: FileStatusKind::Modified,
            }],
            untracked: Vec::new(),
            ignored: Vec::new(),
            conflicted: Vec::new(),
            conflict_labels: Default::default(),
            submodules: Default::default(),
        }),
        epoch: 1,
    });
    let auth_idx = ts
        .state()
        .stage_workbench
        .rows()
        .iter()
        .position(|r| r.is_file() && r.key.ends_with("src/auth.rs"))
        .unwrap();
    ts.dispatch(Action::SelectCommitStageRow(auth_idx));
    let sdiff_ep = ts
        .state()
        .request_slots
        .latest(RequestSlotId::StageFileDiff);
    let vcs = b"fn a() {}\nfn b() {}\nfn c() {}\n";
    let staged = b"fn a() {}\nfn b() {}\nfn c() {}\n";
    let local = b"fn a() {}\nfn b_new() {}\nfn c_new() {}\n";
    let tracker = jbgit_core::StageLineTracker::new(vcs, staged, local);
    let (hunks, is_truncated) = tracker.build_interactive_hunks(false, DiffContext::Three, 20_000);
    ts.deliver_engine_event(EngineEvent::StageFileDiffLoaded {
        diff: Arc::new(ThreeDocStageDiff {
            key: StageDiffKey {
                path: "src/auth.rs".into(),
                old_path: None,
                side: StageDiffSide::Unstaged,
                context: DiffContext::Three,
                whitespace: jbgit_core::WhitespaceMode::Default,
            },
            vcs_exists: true,
            staged_exists: true,
            local_exists: true,
            vcs_bytes: vcs.to_vec(),
            staged_bytes: staged.to_vec(),
            local_bytes: local.to_vec(),
            file_mode: 0o100644,
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
        epoch: sdiff_ep,
    });

    ts.dispatch(Action::FocusPane(PaneId::CommitDiffPreview));
    ts.dispatch(Action::StageDiffToggleVisual);
    ts.dispatch(Action::StageDiffMoveCursor(1));
    assert!(ts.state().stage_workbench.selected_line_range().is_some());

    let sd_menu = stage_diff_menu_items(ts.state());
    let sel_hist = sd_menu
        .iter()
        .find(|i| i.label.contains("Show History for Selection"))
        .expect("stage diff menu includes Show History for Selection");
    assert!(sel_hist.is_enabled());

    // Open menu via '.' and verify activating it opens Selection History for src/auth.rs
    let _ = ts.render(120, 32);
    ts.send_key(KeyCode::Char('.'), KeyModifiers::NONE);
    let (items, _, _) = top_menu(ts.state());
    let idx = items
        .iter()
        .position(|i| i.label.contains("Show History for Selection"))
        .unwrap();
    ts.dispatch(Action::ContextMenuActivateItem { depth: 0, idx });
    assert!(matches!(
        ts.state().modal_stack.last(),
        Some(ModalLayer::FileHistory(fh))
            if fh.key.path == "src/auth.rs" && fh.key.line_range.is_some()
    ));

    let key = BlameKey {
        path: "src/lib.rs".into(),
        rev: Some(oid(10)),
        options: BlameOptions {
            ignore_whitespace: true,
            move_detection: jbgit_core::MoveDetection::AcrossFiles,
            prefer_commit_date: false,
        },
    };
    assert_eq!(key.rev, Some(oid(10)));
    assert_ne!(oid(10), oid_n(10));
}

#[test]
fn annotate_moves_date_mode_columns_coloring_this_rev_details_and_cache_invalidation() {
    use crate::log_view::AnnotateColorMode;
    use jbgit_core::{CommitDetails, MoveDetection};

    let snap = snapshot();
    let c1 = snap.commits[0].oid;
    let c2 = snap.commits[1].oid;
    let mut ts = TestSession::with_snapshot(snap.clone());

    ts.dispatch(Action::OpenAnnotate {
        path: "src/auth.rs".into(),
        rev: None,
    });
    let ep1 = ts.state().request_slots.latest(RequestSlotId::Annotate);
    let _ = ts.take_requests();
    let bundle = sample_blame_bundle(c1, c2);
    ts.deliver_engine_event(EngineEvent::BlameLoaded {
        bundle: Arc::clone(&bundle),
        epoch: ep1,
    });

    // 1. Cycle MoveDetection: None -> WithinFile (-M) -> AcrossFiles (-M -C) -> None (cached!)
    ts.send_key(KeyCode::Char('m'), KeyModifiers::NONE);
    let ep_m1 = ts.state().request_slots.latest(RequestSlotId::Annotate);
    let reqs = ts.take_requests();
    assert!(reqs.iter().any(|(r, _)| matches!(
        r,
        TaskRequest::LoadBlame { key } if key.options.move_detection == MoveDetection::WithinFile
    )));
    let mut b_m1 = (*bundle).clone();
    b_m1.key.options.move_detection = MoveDetection::WithinFile;
    ts.deliver_engine_event(EngineEvent::BlameLoaded {
        bundle: Arc::new(b_m1),
        epoch: ep_m1,
    });

    ts.send_key(KeyCode::Char('m'), KeyModifiers::NONE);
    let ep_m2 = ts.state().request_slots.latest(RequestSlotId::Annotate);
    let reqs = ts.take_requests();
    assert!(reqs.iter().any(|(r, _)| matches!(
        r,
        TaskRequest::LoadBlame { key } if key.options.move_detection == MoveDetection::AcrossFiles
    )));
    let mut b_m2 = (*bundle).clone();
    b_m2.key.options.move_detection = MoveDetection::AcrossFiles;
    ts.deliver_engine_event(EngineEvent::BlameLoaded {
        bundle: Arc::new(b_m2),
        epoch: ep_m2,
    });

    // Cycling back to None hits cache with zero LoadBlame requests!
    ts.send_key(KeyCode::Char('m'), KeyModifiers::NONE);
    assert!(
        !ts.take_requests()
            .iter()
            .any(|(r, _)| matches!(r, TaskRequest::LoadBlame { .. })),
        "cycling back to MoveDetection::None should hit blame LRU cache"
    );
    assert_eq!(
        ts.state()
            .modal_stack
            .annotate()
            .unwrap()
            .key
            .options
            .move_detection,
        MoveDetection::None
    );

    // 2. Toggle prefer_commit_date ('D'): instant 0ms update, zero LoadBlame requests
    assert!(
        !ts.state()
            .modal_stack
            .annotate()
            .unwrap()
            .key
            .options
            .prefer_commit_date
    );
    ts.send_key(KeyCode::Char('D'), KeyModifiers::SHIFT);
    assert!(
        ts.state()
            .modal_stack
            .annotate()
            .unwrap()
            .key
            .options
            .prefer_commit_date
    );
    assert!(
        !ts.take_requests()
            .iter()
            .any(|(r, _)| matches!(r, TaskRequest::LoadBlame { .. })),
        "toggling prefer_commit_date must not re-run git blame"
    );
    let _ = ts.render(120, 34);

    // 3. Cycle coloring mode ('C'): Age -> Author -> None -> Age
    assert_eq!(
        ts.state().modal_stack.annotate().unwrap().color_mode,
        AnnotateColorMode::Age
    );
    ts.send_key(KeyCode::Char('C'), KeyModifiers::SHIFT);
    assert_eq!(
        ts.state().modal_stack.annotate().unwrap().color_mode,
        AnnotateColorMode::Author
    );
    let _ = ts.render(120, 34);
    ts.send_key(KeyCode::Char('C'), KeyModifiers::SHIFT);
    assert_eq!(
        ts.state().modal_stack.annotate().unwrap().color_mode,
        AnnotateColorMode::None
    );
    let _ = ts.render(120, 34);

    // 4. Independent gutter column toggles (including all hidden) render cleanly
    ts.dispatch(Action::AnnotateToggleHashColumn);
    ts.dispatch(Action::AnnotateToggleAuthorColumn);
    ts.dispatch(Action::AnnotateToggleDateColumn);
    {
        let ann = ts.state().modal_stack.annotate().unwrap();
        assert!(!ann.show_hash && !ann.show_author && !ann.show_date);
    }
    let _ = ts.render(120, 34);
    ts.dispatch(Action::AnnotateToggleHashColumn);
    assert!(ts.state().modal_stack.annotate().unwrap().show_hash);

    // 5. 'i' opens Commit Details & Affected Files via RequestSlotId::AnnotateCommitDetails
    ts.send_key(KeyCode::Char('j'), KeyModifiers::NONE);
    ts.send_key(KeyCode::Char('j'), KeyModifiers::NONE); // cursor on line 2 (c2)
    ts.send_key(KeyCode::Char('i'), KeyModifiers::NONE);
    assert!(
        ts.state()
            .modal_stack
            .annotate()
            .unwrap()
            .show_commit_details
    );
    let reqs = ts.take_requests();
    assert!(reqs.iter().any(|(r, _)| matches!(
        r,
        TaskRequest::LoadCommitDetails {
            slot: RequestSlotId::AnnotateCommitDetails,
            key
        } if key.oids == vec![c2]
    )));
    let det_ep = ts
        .state()
        .request_slots
        .latest(RequestSlotId::AnnotateCommitDetails);
    ts.deliver_engine_event(EngineEvent::CommitDetailsLoaded {
        slot: RequestSlotId::AnnotateCommitDetails,
        bundle: Arc::new(CommitChangesBundle {
            key: DetailsKey::single(c2),
            details: vec![CommitDetails {
                oid: c2,
                parents: vec![c1],
                author: stamp("Bob", 1_700_086_400),
                committer: stamp("Bob", 1_700_090_000),
                message: "refine token expiry check\n\nExtended TTL to 60s.".into(),
                signature: Default::default(),
            }],
            changes: vec![
                FileChange {
                    path: "src/auth.rs".into(),
                    old_path: None,
                    kind: FileStatusKind::Modified,
                    additions: 1,
                    deletions: 1,
                    is_binary: false,
                    old_oid: None,
                    new_oid: None,
                    exists_in_workdir: true,
                },
                FileChange {
                    path: "src/config.rs".into(),
                    old_path: None,
                    kind: FileStatusKind::Modified,
                    additions: 2,
                    deletions: 0,
                    is_binary: false,
                    old_oid: None,
                    new_oid: None,
                    exists_in_workdir: true,
                },
            ],
        }),
        epoch: det_ep,
    });
    let buf = ts.render(120, 34);
    let rendered: String = (0..buf.area.height)
        .map(|y| {
            (0..buf.area.width)
                .map(|x| buf[(x, y)].symbol())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(rendered.contains("Extended TTL to 60s."));
    assert!(rendered.contains("src/config.rs"));

    // Esc closes commit details first without closing Annotate
    ts.send_key(KeyCode::Esc, KeyModifiers::NONE);
    assert!(
        !ts.state()
            .modal_stack
            .annotate()
            .unwrap()
            .show_commit_details
    );

    // 6. 'r' Annotate This Revision opens blame at c2, pushes nav_back, and 'P' hits cache
    ts.send_key(KeyCode::Char('r'), KeyModifiers::NONE);
    assert_eq!(ts.state().modal_stack.annotate().unwrap().nav_back.len(), 1);
    assert_eq!(ts.state().modal_stack.annotate().unwrap().key.rev, Some(c2));
    let _ = ts.take_requests();
    ts.send_key(KeyCode::Char('P'), KeyModifiers::SHIFT);
    assert!(
        ts.state()
            .modal_stack
            .annotate()
            .unwrap()
            .nav_back
            .is_empty()
    );
    assert!(
        !ts.take_requests()
            .iter()
            .any(|(r, _)| matches!(r, TaskRequest::LoadBlame { .. })),
        "nav_back after Annotate This Revision must hit blame LRU cache"
    );

    // 7. RepoSnapshotLoaded invalidates DiffCacheStore::blames
    assert!(!ts.state().diff_caches.blames.is_empty());
    ts.deliver_engine_event(EngineEvent::RepoSnapshotLoaded {
        snapshot: Arc::new(snap),
        epoch: ts
            .state()
            .request_slots
            .latest(RequestSlotId::RepoSnapshot)
            .max(1),
    });
    assert!(
        ts.state().diff_caches.blames.is_empty(),
        "RepoSnapshotLoaded must invalidate blame LRU cache"
    );
}
