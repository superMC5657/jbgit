use std::sync::Arc;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use jbgit_core::{
    BinaryDiffInfo, CommitChangesBundle, CommitSummary, DetailsKey, DiffContext, DiffHunk,
    DiffLine, DiffLineKind, FileChange, FileDiff, FileDiffKey, FileStatusKind, Oid, PaneId,
    RepoSnapshot, RequestSlotId, StageEntry, StageStatusSnapshot, StashEntry, WhitespaceMode,
    WorkspaceTab,
};
use jbgit_engine::{EngineEvent, StageDiffKey, StageDiffSide, TaskRequest, ThreeDocStageDiff};

use super::{
    TestSession, find_item, item_run_actions, local, make_details, oid, screen_text, snapshot,
};
use crate::app::{Action, ChangesHost, Effect, ModalLayer};
use crate::config::jbgit_config::WhitespaceModePref;
use crate::config::{AppConfig, KeymapConfig, KeymapPreset};
use crate::keymap::actions::check_scopes;
use crate::keymap::{KeyAction, MatchResult, catalog};

fn seed_log_diff(ts: &mut TestSession, commit_oid: Oid, path: &str, diff: FileDiff) {
    let details_key = diff.key.details.clone();
    ts.dispatch(Action::ToggleDiffPreview);
    ts.deliver_engine_event(EngineEvent::CommitDetailsLoaded {
        slot: RequestSlotId::LogCommitDetails,
        bundle: Arc::new(CommitChangesBundle {
            key: details_key,
            details: vec![make_details(commit_oid, vec![], "diff commit")],
            changes: vec![FileChange {
                path: path.into(),
                old_path: None,
                kind: FileStatusKind::Modified,
                additions: 2,
                deletions: 1,
                old_oid: None,
                new_oid: None,
                is_binary: diff.is_binary,
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
        .position(|r| matches!(r.kind, jbgit_core::ChangeRowKind::File { .. }))
        .expect("file row");
    ts.dispatch(Action::ChangesSelect(ChangesHost::Log, file_row));
    ts.deliver_engine_event(EngineEvent::FileDiffLoaded {
        slot: RequestSlotId::LogFileDiff,
        diff: Arc::new(diff),
        epoch: ts
            .state()
            .request_slots
            .latest(RequestSlotId::LogFileDiff)
            .max(1),
    });
    ts.dispatch(Action::FocusPane(PaneId::LogDetailsDiff));
}

#[test]
fn split_diff_horizontal_scroll_and_soft_wrap_cjk_safe() {
    let mut ts = TestSession::with_snapshot(snapshot());
    let head = oid(12);
    let long_old = format!(
        "let old_prefix = \"{}\"; // OLD_TAIL_MARKER",
        "a".repeat(90)
    );
    let long_new = format!(
        "let new_prefix = \"{}\"; // NEW_TAIL_中文结束",
        "b".repeat(90)
    );
    let key = FileDiffKey {
        details: DetailsKey::single(head),
        path: "src/wide.rs".into(),
        old_path: None,
        context: DiffContext::Three,
        whitespace: WhitespaceMode::Default,
    };
    let diff = FileDiff {
        key,
        hunks: vec![DiffHunk {
            header: "@@ -1,1 +1,1 @@".into(),
            lines: vec![
                DiffLine {
                    kind: DiffLineKind::Deleted,
                    old_lineno: Some(1),
                    new_lineno: None,
                    text: long_old,
                    word_ranges: Vec::new(),
                },
                DiffLine {
                    kind: DiffLineKind::Added,
                    old_lineno: None,
                    new_lineno: Some(1),
                    text: long_new,
                    word_ranges: Vec::new(),
                },
            ],
        }],
        is_binary: false,
        binary: None,
        is_truncated: false,
        is_too_large: false,
        lfs: None,
        mode_change: None,
    };
    seed_log_diff(&mut ts, head, "src/wide.rs", diff);

    // Switch to zoomed side-by-side split mode so each column is ~60 cols wide.
    ts.send_key(KeyCode::Char('f'), KeyModifiers::NONE);
    ts.send_key(KeyCode::Char('s'), KeyModifiers::NONE);
    assert!(ts.state().log.details.diff.side_by_side);
    assert!(!ts.state().log.details.diff.soft_wrap);

    let initial = screen_text(&ts.render(140, 34));
    assert!(initial.contains("new_prefix"), "{initial}");
    assert!(
        !initial.replace(' ', "").contains("NEW_TAIL_中文结束"),
        "tail must be clipped before scrolling:\n{initial}"
    );
    assert!(!initial.contains("Col +"), "{initial}");

    // Horizontal scroll right using Shift+Right and `l` until tail is visible.
    for _ in 0..5 {
        ts.send_key(KeyCode::Right, KeyModifiers::SHIFT);
    }
    ts.send_key(KeyCode::Char('l'), KeyModifiers::NONE);
    assert!(ts.state().log.details.diff.horiz_scroll > 0);
    let scrolled = screen_text(&ts.render(140, 34));
    assert!(
        scrolled.contains(&format!(
            "Col +{}",
            ts.state().log.details.diff.horiz_scroll
        )),
        "expected Col +N indicator:\n{scrolled}"
    );
    assert!(
        scrolled.replace(' ', "").contains("NEW_TAIL_中文结束"),
        "tail must be visible after horizontal scroll:\n{scrolled}"
    );

    // Scroll left reduces offset.
    let before_left = ts.state().log.details.diff.horiz_scroll;
    ts.send_key(KeyCode::Char('h'), KeyModifiers::NONE);
    assert!(ts.state().log.details.diff.horiz_scroll < before_left);

    // Toggle Soft Wrap (`w`) -> resets horiz_scroll, shows both prefix and tail without truncation.
    ts.send_key(KeyCode::Char('w'), KeyModifiers::NONE);
    assert!(ts.state().log.details.diff.soft_wrap);
    assert_eq!(ts.state().log.details.diff.horiz_scroll, 0);
    let wrapped = screen_text(&ts.render(140, 34));
    assert!(!wrapped.contains("Col +"), "{wrapped}");
    assert!(
        !wrapped.contains('…'),
        "soft wrap must not ellipsize:\n{wrapped}"
    );
    let left_col: String = wrapped
        .lines()
        .filter_map(|l| l.split_once('│').map(|(a, _)| a.replace(' ', "")))
        .collect();
    let right_col: String = wrapped
        .lines()
        .filter_map(|l| l.split_once('│').map(|(_, b)| b.replace(' ', "")))
        .collect();
    assert!(left_col.contains("old_prefix"), "{left_col}");
    assert!(left_col.contains("OLD_TAIL_MARKER"), "{left_col}");
    assert!(right_col.contains("new_prefix"), "{right_col}");
    assert!(
        right_col.contains("NEW_TAIL_中文结束"),
        "right pane wrapped tail must be intact across wrapped rows:\n{wrapped}"
    );
}

#[test]
fn diff_inline_search_incremental_navigation_and_esc_priority() {
    let mut ts = TestSession::with_snapshot(snapshot());
    let head = oid(12);
    let mut lines = Vec::new();
    for i in 1..=60u32 {
        let text = match i {
            3 => "let first = TargetWord::init();".to_string(),
            28 => "let second = targetword_helper();".to_string(),
            55 => "let third = TARGETWORD_FINAL;".to_string(),
            _ => format!("let filler_{i} = {i};"),
        };
        lines.push(DiffLine {
            kind: DiffLineKind::Added,
            old_lineno: None,
            new_lineno: Some(i),
            text,
            word_ranges: Vec::new(),
        });
    }
    let diff = FileDiff {
        key: FileDiffKey {
            details: DetailsKey::single(head),
            path: "src/search.rs".into(),
            old_path: None,
            context: DiffContext::Three,
            whitespace: WhitespaceMode::Default,
        },
        hunks: vec![DiffHunk {
            header: "@@ -0,0 +1,60 @@".into(),
            lines,
        }],
        is_binary: false,
        binary: None,
        is_truncated: false,
        is_too_large: false,
        lfs: None,
        mode_change: None,
    };
    seed_log_diff(&mut ts, head, "src/search.rs", diff);
    let _ = ts.render(140, 24);

    // Open inline search via `/`.
    ts.send_key(KeyCode::Char('/'), KeyModifiers::NONE);
    assert!(ts.state().log.details.diff.search.active);
    assert!(ts.state().log.details.diff.search.editing);

    for ch in "targetword".chars() {
        ts.send_key(KeyCode::Char(ch), KeyModifiers::NONE);
    }
    assert_eq!(ts.state().log.details.diff.search.matches.len(), 3);
    assert_eq!(ts.state().log.details.diff.search.current, 0);
    let searching_screen = screen_text(&ts.render(140, 24));
    assert!(searching_screen.contains("[1/3]"), "{searching_screen}");
    assert!(
        searching_screen.contains("targetword"),
        "{searching_screen}"
    );

    // Confirm query with Enter -> editing stops, search remains active for n/N.
    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);
    assert!(ts.state().log.details.diff.search.active);
    assert!(!ts.state().log.details.diff.search.editing);

    // Press `n` -> jumps to match 2 and scrolls viewport down toward line 28.
    ts.send_key(KeyCode::Char('n'), KeyModifiers::NONE);
    assert_eq!(ts.state().log.details.diff.search.current, 1);
    let offset_2 = ts.state().log.details.diff.scroll.offset();
    assert!(offset_2 > 0, "viewport must scroll to match 2");
    let screen_2 = screen_text(&ts.render(140, 24));
    assert!(screen_2.contains("[2/3]"), "{screen_2}");
    assert!(screen_2.contains("targetword_helper"), "{screen_2}");

    // Press `n` -> jumps to match 3 and clamps scroll to max_scroll.
    ts.send_key(KeyCode::Char('n'), KeyModifiers::NONE);
    assert_eq!(ts.state().log.details.diff.search.current, 2);
    let offset_3 = ts.state().log.details.diff.scroll.offset();
    assert!(offset_3 >= offset_2);
    let screen_3 = screen_text(&ts.render(140, 24));
    assert!(screen_3.contains("[3/3]"), "{screen_3}");
    assert!(screen_3.contains("TARGETWORD_FINAL"), "{screen_3}");

    // Wrap forward (`n` -> [1/3]) and backward (`N` -> [3/3]).
    ts.send_key(KeyCode::Char('n'), KeyModifiers::NONE);
    assert_eq!(ts.state().log.details.diff.search.current, 0);
    ts.send_key(KeyCode::Char('N'), KeyModifiers::SHIFT);
    assert_eq!(ts.state().log.details.diff.search.current, 2);

    // Re-open editing (`/`) from the top and verify Up/Down arrow keys clamp instead of wrapping.
    ts.send_key(KeyCode::Home, KeyModifiers::NONE);
    ts.send_key(KeyCode::Char('/'), KeyModifiers::NONE);
    for ch in "targetword".chars() {
        ts.send_key(KeyCode::Char(ch), KeyModifiers::NONE);
    }
    assert_eq!(ts.state().log.details.diff.search.current, 0);
    ts.send_key(KeyCode::Up, KeyModifiers::NONE);
    assert_eq!(ts.state().log.details.diff.search.current, 0);
    ts.send_key(KeyCode::Down, KeyModifiers::NONE);
    ts.send_key(KeyCode::Down, KeyModifiers::NONE);
    assert_eq!(ts.state().log.details.diff.search.current, 2);
    ts.send_key(KeyCode::Down, KeyModifiers::NONE);
    assert_eq!(ts.state().log.details.diff.search.current, 2);
    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);

    // Esc closes inline search FIRST without leaving Diff preview!
    ts.send_key(KeyCode::Esc, KeyModifiers::NONE);
    assert!(!ts.state().log.details.diff.search.active);
    assert!(ts.state().log.details.diff_preview);
    assert_eq!(ts.state().focused_pane, PaneId::LogDetailsDiff);
}

#[test]
fn diff_whitespace_mode_cycling_and_stage_partial_staging_guard() {
    let mut ts = TestSession::with_snapshot(snapshot());
    let head = oid(12);
    let base_key = FileDiffKey {
        details: DetailsKey::single(head),
        path: "src/indent.rs".into(),
        old_path: None,
        context: DiffContext::Three,
        whitespace: WhitespaceMode::Default,
    };
    let diff = FileDiff {
        key: base_key.clone(),
        hunks: vec![DiffHunk {
            header: "@@ -1,1 +1,1 @@".into(),
            lines: vec![
                DiffLine {
                    kind: DiffLineKind::Deleted,
                    old_lineno: Some(1),
                    new_lineno: None,
                    text: "  let x = 1;".into(),
                    word_ranges: Vec::new(),
                },
                DiffLine {
                    kind: DiffLineKind::Added,
                    old_lineno: None,
                    new_lineno: Some(1),
                    text: "    let x = 1;".into(),
                    word_ranges: Vec::new(),
                },
            ],
        }],
        is_binary: false,
        binary: None,
        is_truncated: false,
        is_too_large: false,
        lfs: None,
        mode_change: None,
    };
    seed_log_diff(&mut ts, head, "src/indent.rs", diff);
    let _ = ts.take_requests();

    // Press `i` -> cycles Default -> IgnoreChange (-b) and requests new diff key.
    ts.send_key(KeyCode::Char('i'), KeyModifiers::NONE);
    assert_eq!(
        ts.state().log.details.diff.whitespace,
        WhitespaceMode::IgnoreChange
    );
    let ws_key = FileDiffKey {
        whitespace: WhitespaceMode::IgnoreChange,
        ..base_key.clone()
    };
    let reqs = ts.take_requests();
    assert!(
        reqs.iter()
            .any(|(r, _)| matches!(r, TaskRequest::LoadFileDiff { key, .. } if *key == ws_key)),
        "expected LoadFileDiff with IgnoreChange: {reqs:?}"
    );

    // Deliver empty-hunks response (pure indentation diff collapsed under -b).
    ts.deliver_engine_event(EngineEvent::FileDiffLoaded {
        slot: RequestSlotId::LogFileDiff,
        diff: Arc::new(FileDiff {
            key: ws_key,
            hunks: Vec::new(),
            is_binary: false,
            binary: None,
            is_truncated: false,
            is_too_large: false,
            lfs: None,
            mode_change: None,
        }),
        epoch: ts
            .state()
            .request_slots
            .latest(RequestSlotId::LogFileDiff)
            .max(1),
    });
    let screen = screen_text(&ts.render(140, 32));
    assert!(screen.contains("WS: -b"), "{screen}");
    assert!(
        screen.contains("No differences (ignoring whitespace)"),
        "{screen}"
    );

    // Cycle again -> IgnoreAll (-w) -> Default.
    ts.send_key(KeyCode::Char('i'), KeyModifiers::NONE);
    assert_eq!(
        ts.state().log.details.diff.whitespace,
        WhitespaceMode::IgnoreAll
    );
    ts.send_key(KeyCode::Char('i'), KeyModifiers::NONE);
    assert_eq!(
        ts.state().log.details.diff.whitespace,
        WhitespaceMode::Default
    );

    // Now test Stage Diff partial-staging guard when ignoring whitespace.
    ts.dispatch(Action::SwitchTab(WorkspaceTab::Commit));
    ts.deliver_engine_event(EngineEvent::StageStatusLoaded {
        status: Arc::new(StageStatusSnapshot {
            unstaged: vec![StageEntry {
                path: "src/indent.rs".into(),
                old_path: None,
                kind: FileStatusKind::Modified,
            }],
            ..Default::default()
        }),
        epoch: 1,
    });
    let row = ts
        .state()
        .stage_workbench
        .rows()
        .iter()
        .position(|r| r.is_file())
        .unwrap();
    ts.dispatch(Action::SelectCommitStageRow(row));
    let _ = ts.take_requests();
    ts.dispatch(Action::FocusPane(PaneId::CommitDiffPreview));

    // Press `i` in Stage Diff -> cycles to IgnoreChange and requests StageDiffKey with IgnoreChange.
    ts.send_key(KeyCode::Char('i'), KeyModifiers::NONE);
    assert_eq!(
        ts.state().stage_workbench.diff_whitespace(),
        WhitespaceMode::IgnoreChange
    );
    let stage_reqs = ts.take_requests();
    assert!(
        stage_reqs.iter().any(|(r, _)| matches!(
            r,
            TaskRequest::LoadStageFileDiff { key } if key.whitespace == WhitespaceMode::IgnoreChange
        )),
        "{stage_reqs:?}"
    );

    let vcs = b"fn a() {\n  let x = 1;\n}\n";
    let staged = vcs;
    let local = b"fn a() {\n  let x = 2;\n}\n";
    let tracker = jbgit_core::StageLineTracker::new_with_whitespace(
        vcs,
        staged,
        local,
        WhitespaceMode::IgnoreChange,
    );
    let (hunks, is_truncated) = tracker.build_interactive_hunks(false, DiffContext::Three, 20_000);
    ts.deliver_engine_event(EngineEvent::StageFileDiffLoaded {
        diff: Arc::new(ThreeDocStageDiff {
            key: StageDiffKey {
                path: "src/indent.rs".into(),
                old_path: None,
                side: StageDiffSide::Unstaged,
                context: DiffContext::Three,
                whitespace: WhitespaceMode::IgnoreChange,
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
        epoch: ts
            .state()
            .request_slots
            .latest(RequestSlotId::StageFileDiff)
            .max(1),
    });

    let stage_screen = screen_text(&ts.render(140, 32));
    assert!(
        stage_screen.contains("partial staging disabled"),
        "{stage_screen}"
    );
    assert!(
        !stage_screen.contains("[+ Stage]"),
        "hunk pills must be hidden when ignoring whitespace:\n{stage_screen}"
    );

    // Context menu disables Stage Hunk / Stage Hunk or Lines.
    let hunk_menu = crate::menus::stage_hunk_menu_items(ts.state(), 0);
    assert!(
        find_item(&hunk_menu, "Stage Hunk")
            .disabled_reason
            .is_some()
    );

    // Attempting to stage selection emits no ApplyStageBuffer and sets status notice.
    let _ = ts.take_requests();
    ts.dispatch(Action::StageDiffStageSelection);
    assert!(
        !ts.take_requests()
            .iter()
            .any(|(r, _)| matches!(r, TaskRequest::ApplyStageBuffer { .. }))
    );
    assert!(
        ts.state()
            .status_notice
            .as_deref()
            .is_some_and(|n| n.contains("Partial staging disabled")),
        "{:?}",
        ts.state().status_notice
    );
}

#[test]
fn binary_diff_card_renders_size_delta_and_short_oids_in_log_and_stage() {
    let mut ts = TestSession::with_snapshot(snapshot());
    let head = oid(12);
    let bin = BinaryDiffInfo {
        old_size: Some(12_698), // 12.4 KiB
        new_size: Some(15_155), // 14.8 KiB (+2.4 KiB)
        old_oid: Some(oid(0xaa)),
        new_oid: Some(oid(0xbb)),
    };
    let diff = FileDiff {
        key: FileDiffKey {
            details: DetailsKey::single(head),
            path: "assets/logo.png".into(),
            old_path: None,
            context: DiffContext::Three,
            whitespace: WhitespaceMode::Default,
        },
        hunks: Vec::new(),
        is_binary: true,
        binary: Some(bin.clone()),
        is_truncated: false,
        is_too_large: false,
        lfs: None,
        mode_change: None,
    };
    seed_log_diff(&mut ts, head, "assets/logo.png", diff);

    let log_screen = screen_text(&ts.render(140, 32));
    assert!(log_screen.contains("Binary File"), "{log_screen}");
    assert!(log_screen.contains("12.4 KiB"), "{log_screen}");
    assert!(log_screen.contains("14.8 KiB"), "{log_screen}");
    assert!(log_screen.contains("+2.4 KiB"), "{log_screen}");
    assert!(log_screen.contains("aaaaaaa"), "{log_screen}");
    assert!(log_screen.contains("bbbbbbb"), "{log_screen}");

    // Also verify Stage Diff renders the same binary metadata card.
    ts.dispatch(Action::SwitchTab(WorkspaceTab::Commit));
    ts.deliver_engine_event(EngineEvent::StageStatusLoaded {
        status: Arc::new(StageStatusSnapshot {
            unstaged: vec![StageEntry {
                path: "assets/logo.png".into(),
                old_path: None,
                kind: FileStatusKind::Modified,
            }],
            ..Default::default()
        }),
        epoch: 1,
    });
    let row = ts
        .state()
        .stage_workbench
        .rows()
        .iter()
        .position(|r| r.is_file())
        .unwrap();
    ts.dispatch(Action::SelectCommitStageRow(row));
    ts.deliver_engine_event(EngineEvent::StageFileDiffLoaded {
        diff: Arc::new(ThreeDocStageDiff {
            key: StageDiffKey {
                path: "assets/logo.png".into(),
                old_path: None,
                side: StageDiffSide::Unstaged,
                context: DiffContext::Three,
                whitespace: WhitespaceMode::Default,
            },
            vcs_exists: true,
            staged_exists: true,
            local_exists: true,
            vcs_bytes: Vec::new(),
            staged_bytes: Vec::new(),
            local_bytes: Vec::new(),
            file_mode: 0o100644,
            mode_change: None,
            is_symlink: false,
            is_binary: true,
            binary: Some(bin),
            is_conflicted: false,
            submodule: None,
            lfs: None,
            tracker: jbgit_core::StageLineTracker::new(b"", b"", b""),
            hunks: Vec::new(),
            is_truncated: false,
            is_too_large: false,
        }),
        epoch: ts
            .state()
            .request_slots
            .latest(RequestSlotId::StageFileDiff)
            .max(1),
    });
    let stage_screen = screen_text(&ts.render(140, 32));
    assert!(stage_screen.contains("Binary File"), "{stage_screen}");
    assert!(stage_screen.contains("12.4 KiB"), "{stage_screen}");
    assert!(stage_screen.contains("14.8 KiB"), "{stage_screen}");
    assert!(stage_screen.contains("aaaaaaa"), "{stage_screen}");
    assert!(stage_screen.contains("bbbbbbb"), "{stage_screen}");
}

#[test]
fn log_copy_commit_message_and_row_summary_single_and_multi() {
    let c2 = oid(2);
    let c1 = oid(1);
    let mut main = local("main", 2);
    main.is_head = true;
    let snap = RepoSnapshot {
        repo_name: "demo".into(),
        head_branch: Some("main".into()),
        head_oid: Some(c2),
        refs: vec![main],
        commits: vec![
            CommitSummary {
                oid: c2,
                parents: vec![c1],
                subject: "feat: add diff search".into(),
                body: "Body line 1\nBody line 2".into(),
                author_name: "Alice".into(),
                author_email: "alice@example.com".into(),
                timestamp: 1_700_000_000,
                author_timestamp: 1_700_000_000,
            },
            CommitSummary {
                oid: c1,
                parents: vec![],
                subject: "chore: init".into(),
                body: String::new(),
                author_name: "Bob".into(),
                author_email: "bob@example.com".into(),
                timestamp: 1_699_900_000,
                author_timestamp: 1_699_900_000,
            },
        ]
        .into(),
        user_name: Some("Alice".into()),
        user_email: Some("alice@example.com".into()),
        ..Default::default()
    };
    let mut ts = TestSession::with_snapshot(snap);
    ts.dispatch(Action::FocusPane(PaneId::LogCommitTable));

    // Menu includes Copy Commit Message & Copy Row Summary.
    let menu = crate::menus::log_commit_menu_items(ts.state());
    let copy_msg = find_item(&menu, "Copy Commit Message");
    let copy_row = find_item(&menu, "Copy Row Summary");
    assert!(copy_msg.is_enabled());
    assert!(copy_row.is_enabled());

    // Execute Copy Commit Message -> full subject + body.
    let msg_actions = item_run_actions(copy_msg);
    assert!(
        matches!(
            msg_actions.as_slice(),
            [Action::CopyText(s)] if s == "feat: add diff search\n\nBody line 1\nBody line 2"
        ),
        "{msg_actions:?}"
    );

    // Execute Copy Row Summary via CommandCatalog -> `hash subject — author (date)`.
    let row_actions =
        catalog::execute_action(&KeyAction::CopyRowSummary, ts.state()).expect("row summary");
    assert_eq!(row_actions.len(), 1);
    match &row_actions[0] {
        Action::CopyText(s) => {
            assert!(
                s.starts_with(&format!("{} feat: add diff search — Alice (", c2.short())),
                "{s}"
            );
            assert!(s.ends_with(')'), "{s}");
            assert!(!s.contains('\n'), "{s}");
        }
        other => panic!("unexpected action: {other:?}"),
    }

    // Multi-select both commits -> Copy Row Summary outputs 2 lines; Copy Commit Message joins both.
    ts.dispatch(Action::ExtendLogSelectionTo(1));
    let multi_msg_actions =
        catalog::execute_action(&KeyAction::CopyCommitMessage, ts.state()).expect("multi msg");
    match &multi_msg_actions[0] {
        Action::CopyText(s) => {
            assert!(s.contains("feat: add diff search"), "{s}");
            assert!(s.contains("Body line 2"), "{s}");
            assert!(s.contains("chore: init"), "{s}");
        }
        other => panic!("unexpected action: {other:?}"),
    }
    let multi_row_actions =
        catalog::execute_action(&KeyAction::CopyRowSummary, ts.state()).expect("multi summary");
    match &multi_row_actions[0] {
        Action::CopyText(s) => {
            let lines: Vec<&str> = s.lines().collect();
            assert_eq!(lines.len(), 2, "{s}");
            assert!(lines[0].contains("feat: add diff search — Alice"), "{s}");
            assert!(lines[1].contains("chore: init — Bob"), "{s}");
        }
        other => panic!("unexpected action: {other:?}"),
    }

    // Dispatch CopyText actually emits Effect::CopyToClipboard.
    let effects = ts.state_mut().dispatch(multi_row_actions[0].clone());
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::CopyToClipboard(t) if t.lines().count() == 2)),
        "{effects:?}"
    );
}

#[test]
fn diff_config_parses_whitespace_and_soft_wrap_and_initializes_sessions() {
    let mut cfg = AppConfig::load_defaults();
    let overlay: crate::config::JbgitConfigOverlay = toml::from_str(
        r#"
[diff]
whitespace = "ignore_change"
soft_wrap = true
"#,
    )
    .expect("valid diff config overlay");
    cfg.jbgit.merge_overlay(&overlay);
    assert_eq!(cfg.jbgit.diff.whitespace, WhitespaceModePref::IgnoreChange);
    assert_eq!(
        cfg.jbgit.diff.whitespace.to_core(),
        WhitespaceMode::IgnoreChange
    );
    assert!(cfg.jbgit.diff.soft_wrap);

    let state = crate::app::AppState::new_with_config(".".into(), None, cfg);
    assert_eq!(
        state.log.details.diff.whitespace,
        WhitespaceMode::IgnoreChange
    );
    assert!(state.log.details.diff.soft_wrap);
    assert_eq!(
        state.stage_workbench.diff_whitespace(),
        WhitespaceMode::IgnoreChange
    );
    assert!(state.stage_workbench.diff_soft_wrap());
    assert_eq!(state.stash.diff().whitespace, WhitespaceMode::IgnoreChange);
    assert!(state.stash.diff().soft_wrap);

    // Also test "-w" alias parsing.
    let alias_overlay: crate::config::JbgitConfigOverlay =
        toml::from_str("[diff]\nwhitespace = \"-w\"\n").expect("-w alias");
    assert_eq!(
        alias_overlay.diff.and_then(|d| d.whitespace),
        Some(WhitespaceModePref::IgnoreAll)
    );
}

#[test]
fn diff_new_keybindings_resolve_without_shadowing_across_all_presets() {
    let expected: &[(KeyEvent, KeyAction)] = &[
        (
            KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE),
            KeyAction::CycleDiffWhitespace,
        ),
        (
            KeyEvent::new(KeyCode::Char('w'), KeyModifiers::NONE),
            KeyAction::ToggleDiffSoftWrap,
        ),
        (
            KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE),
            KeyAction::DiffScrollLeft,
        ),
        (
            KeyEvent::new(KeyCode::Left, KeyModifiers::NONE),
            KeyAction::DiffScrollLeft,
        ),
        (
            KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE),
            KeyAction::DiffScrollRight,
        ),
        (
            KeyEvent::new(KeyCode::Right, KeyModifiers::NONE),
            KeyAction::DiffScrollRight,
        ),
        (
            KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE),
            KeyAction::DiffSearch,
        ),
        (
            KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL),
            KeyAction::DiffSearch,
        ),
    ];
    let search_only: &[(KeyEvent, KeyAction)] = &[
        (
            KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE),
            KeyAction::DiffSearchNext,
        ),
        (
            KeyEvent::new(KeyCode::Char('N'), KeyModifiers::SHIFT),
            KeyAction::DiffSearchPrev,
        ),
    ];

    for preset in [
        KeymapPreset::Default,
        KeymapPreset::Jetbrains,
        KeymapPreset::Vim,
    ] {
        let mut cfg = AppConfig::load_defaults();
        cfg.jbgit.keymap.preset = preset.clone();
        cfg.keymap = KeymapConfig::for_preset(preset.clone());
        let dir = tempfile::tempdir().unwrap();
        let mut ts = TestSession::with_config(dir.path().to_path_buf(), cfg);
        ts.deliver_engine_event(EngineEvent::RepoSnapshotLoaded {
            snapshot: Arc::new(RepoSnapshot {
                stashes: vec![StashEntry {
                    index: 0,
                    oid: oid(12),
                    message: "On main: wip".into(),
                    timestamp: 0,
                }],
                ..snapshot()
            }),
            epoch: 1,
        });

        let contexts = [
            (WorkspaceTab::Log, PaneId::LogDetailsDiff, false),
            (WorkspaceTab::Log, PaneId::LogDetailsDiff, true),
            (WorkspaceTab::Commit, PaneId::CommitDiffPreview, false),
            (WorkspaceTab::Stash, PaneId::StashDiffPreview, false),
        ];

        for (tab, pane, zoomed) in contexts {
            ts.dispatch(Action::SwitchTab(tab));
            ts.state_mut().log.details.diff.zoomed = zoomed;
            if pane == PaneId::LogDetailsDiff {
                ts.state_mut().log.details.diff_preview = true;
            }
            ts.dispatch(Action::FocusPane(pane));

            // Global `B` (Branches popup) must NOT be shadowed by Diff keys.
            if !zoomed {
                let m_b = check_scopes(
                    &[KeyEvent::new(KeyCode::Char('B'), KeyModifiers::SHIFT)],
                    ts.state(),
                );
                assert!(
                    matches!(m_b, MatchResult::Exact(b) if b.run == KeyAction::ToggleBranchesPopup),
                    "preset={preset:?} tab={tab:?} pane={pane:?}: `B` must remain ToggleBranchesPopup"
                );
            }
            // Anti-bleed: without an active diff search, `n` in Diff is absorbed as a no-op
            // (never bleeding into Log `new_branch` or Stash `stash_new`).
            let n_actions = crate::keymap::route_dynamic_key(
                &KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE),
                ts.state(),
            );
            assert!(
                matches!(n_actions.as_deref(), Some(&[])),
                "preset={preset:?} tab={tab:?} pane={pane:?}: inactive diff `n` must not bleed"
            );
            if !zoomed && tab == WorkspaceTab::Stash {
                // Anti-bleed: `d`/`a`/`o` in StashDiffPreview never trigger StashList ops (`o` cycles context).
                let m_o = check_scopes(
                    &[KeyEvent::new(KeyCode::Char('o'), KeyModifiers::NONE)],
                    ts.state(),
                );
                assert!(
                    matches!(m_o, MatchResult::Exact(b) if b.run == KeyAction::CycleDiffContext),
                    "preset={preset:?}: `o` in StashDiffPreview must cycle diff context"
                );
                assert!(
                    !crate::keymap::catalog::is_action_applicable(
                        &KeyAction::StashDrop,
                        ts.state()
                    ),
                    "StashDrop must not be applicable in StashDiffPreview"
                );
            }

            for (key, expected_action) in expected {
                let m = check_scopes(&[*key], ts.state());
                assert!(
                    matches!(m, MatchResult::Exact(b) if b.run == *expected_action),
                    "preset={preset:?} tab={tab:?} pane={pane:?} zoomed={zoomed} key={key:?}: expected {expected_action:?}"
                );
            }

            // Once diff search is active, `n`/`N` resolve to DiffSearchNext / DiffSearchPrev.
            let scope = match tab {
                WorkspaceTab::Commit => crate::app::DiffScope::Stage,
                WorkspaceTab::Stash => crate::app::DiffScope::Stash,
                _ => crate::app::DiffScope::Log,
            };
            ts.dispatch(Action::DiffSearchOpen(scope));
            ts.dispatch(Action::DiffSearchChar(scope, 'x'));
            ts.dispatch(Action::DiffSearchConfirm(scope));
            for (key, expected_action) in search_only {
                let m = check_scopes(&[*key], ts.state());
                assert!(
                    matches!(m, MatchResult::Exact(b) if b.run == *expected_action),
                    "preset={preset:?} tab={tab:?} pane={pane:?} zoomed={zoomed} key={key:?}: expected {expected_action:?}"
                );
            }
            ts.dispatch(Action::DiffSearchClose(scope));
        }
    }
}

#[test]
fn file_history_diff_supports_search_whitespace_and_wrap_without_breaking_commits_keys() {
    let mut ts = TestSession::with_snapshot(snapshot());
    let head = oid(12);
    ts.dispatch(Action::OpenFileHistory {
        path: "src/app.rs".into(),
        start_oid: Some(head),
        follow_renames: true,
        line_range: None,
    });
    assert!(matches!(
        ts.state().modal_stack.last(),
        Some(ModalLayer::FileHistory(_))
    ));
    let fh_ep = ts
        .state()
        .request_slots
        .latest(RequestSlotId::FileHistoryList)
        .max(1);
    ts.deliver_engine_event(EngineEvent::FileHistoryLoaded {
        bundle: Arc::new(jbgit_core::FileHistoryBundle {
            key: jbgit_core::FileHistoryKey::new("src/app.rs", Some(head), true, None),
            entries: vec![jbgit_core::FileHistoryEntry {
                oid: head,
                author_name: "Alice".into(),
                author_time: 1_700_000_000,
                summary: "update app".into(),
                kind: FileStatusKind::Modified,
                path_at_commit: "src/app.rs".into(),
                old_path: None,
                additions: 1,
                deletions: 0,
                is_binary: false,
                range_patch: None,
            }],
            truncated: false,
        }),
        epoch: fh_ep,
    });
    let diff_key = FileDiffKey {
        details: DetailsKey::single(head),
        path: "src/app.rs".into(),
        old_path: None,
        context: DiffContext::Three,
        whitespace: WhitespaceMode::Default,
    };
    ts.deliver_engine_event(EngineEvent::FileDiffLoaded {
        slot: RequestSlotId::FileHistoryDiff,
        diff: Arc::new(FileDiff {
            key: diff_key,
            hunks: vec![DiffHunk {
                header: "@@ -1,1 +1,1 @@".into(),
                lines: vec![DiffLine {
                    kind: DiffLineKind::Added,
                    old_lineno: None,
                    new_lineno: Some(1),
                    text: "let needle_here = 123;".into(),
                    word_ranges: Vec::new(),
                }],
            }],
            is_binary: false,
            binary: None,
            is_truncated: false,
            is_too_large: false,
            lfs: None,
            mode_change: None,
        }),
        epoch: ts
            .state()
            .request_slots
            .latest(RequestSlotId::FileHistoryDiff)
            .max(1),
    });

    // Focus Diff inside File History via Tab, then test `/`, `W`, `b`, and `Esc` priority.
    ts.send_key(KeyCode::Tab, KeyModifiers::NONE);
    ts.send_key(KeyCode::Char('/'), KeyModifiers::NONE);
    for ch in "needle".chars() {
        ts.send_key(KeyCode::Char(ch), KeyModifiers::NONE);
    }
    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(
        ts.state()
            .modal_stack
            .file_history()
            .unwrap()
            .diff
            .search
            .matches
            .len(),
        1
    );

    // Esc closes search first without closing File History modal.
    ts.send_key(KeyCode::Esc, KeyModifiers::NONE);
    assert!(ts.state().modal_stack.file_history().is_some());
    assert!(
        !ts.state()
            .modal_stack
            .file_history()
            .unwrap()
            .diff
            .search
            .active
    );

    // Toggle soft wrap and cycle whitespace in File History diff.
    ts.send_key(KeyCode::Char('W'), KeyModifiers::SHIFT);
    assert!(
        ts.state()
            .modal_stack
            .file_history()
            .unwrap()
            .diff
            .soft_wrap
    );
    ts.send_key(KeyCode::Char('b'), KeyModifiers::NONE);
    assert_eq!(
        ts.state()
            .modal_stack
            .file_history()
            .unwrap()
            .diff
            .whitespace,
        WhitespaceMode::IgnoreChange
    );

    // Ctrl+F also opens inline diff search when File History diff is focused.
    ts.send_key(KeyCode::Char('f'), KeyModifiers::CONTROL);
    assert!(
        ts.state()
            .modal_stack
            .file_history()
            .unwrap()
            .diff
            .search
            .editing,
        "Ctrl+F in File History diff should open inline search"
    );
    ts.send_key(KeyCode::Esc, KeyModifiers::NONE);
}

#[test]
fn ctrl_f_opens_inline_diff_search_in_commit_diff_annotate_and_reflog() {
    let mut ts = TestSession::with_snapshot(snapshot());

    // 1. [1] CommitDiffPreview: Ctrl+F opens stage diff inline search.
    ts.dispatch(Action::SwitchTab(WorkspaceTab::Commit));
    ts.dispatch(Action::FocusPane(PaneId::CommitDiffPreview));
    ts.send_key(KeyCode::Char('f'), KeyModifiers::CONTROL);
    assert!(
        ts.state().stage_workbench.diff_search().editing,
        "Ctrl+F in CommitDiffPreview should open inline diff search"
    );
    ts.send_key(KeyCode::Esc, KeyModifiers::NONE);
    assert!(!ts.state().stage_workbench.diff_search().active);

    // 2. Modal Annotate: Ctrl+F opens diff search when diff pane is focused.
    ts.dispatch(Action::OpenAnnotate {
        path: "src/app.rs".into(),
        rev: Some(oid(12)),
    });
    if let Some(ann) = ts.state_mut().modal_stack.annotate_mut() {
        ann.show_diff = true;
        ann.focus = crate::log_view::AnnotateFocus::Diff;
    }
    ts.send_key(KeyCode::Char('f'), KeyModifiers::CONTROL);
    assert!(
        ts.state()
            .modal_stack
            .annotate()
            .unwrap()
            .diff
            .search
            .editing,
        "Ctrl+F in Annotate diff should open inline diff search"
    );
    ts.send_key(KeyCode::Esc, KeyModifiers::NONE);
    ts.send_key(KeyCode::Esc, KeyModifiers::NONE);
    assert!(ts.state().modal_stack.annotate().is_none());

    // 3. Modal Reflog: Ctrl+F opens diff search when diff pane is focused.
    ts.dispatch(Action::OpenReflog("HEAD".into()));
    if let Some(rf) = ts.state_mut().modal_stack.reflog_mut() {
        rf.focus = crate::log_view::ReflogFocus::Diff;
    }
    ts.send_key(KeyCode::Char('f'), KeyModifiers::CONTROL);
    assert!(
        ts.state().modal_stack.reflog().unwrap().diff.search.editing,
        "Ctrl+F in Reflog diff should open inline diff search"
    );
}
