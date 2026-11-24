use std::collections::VecDeque;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use chrono::Local;
use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEventKind,
};
use futures::StreamExt;
use jbgit_core::{
    AmendCommitPreview, CommitSummary, ConsoleLine, ConsoleStream, DiffContext, DiffHunk, DiffLine,
    DiffLineKind, FileChange, FileDiff, FileDiffKey, FileStatusKind, GitConsoleEntry, Oid, PaneId,
    RefInfo, RefKind, RefreshScope, RepoSnapshot, RepositoryState, SplitAxis, SplitterId,
    StageEntry, StageStatusSnapshot, StageTreeRowKind, StashEntry, WorkspaceTab,
};
use jbgit_engine::{
    CommitOutcome, EngineEvent, StageDiffKey, StageDiffSide, TaskRequest, ThreeDocStageDiff,
};
use ratatui::layout::Rect;
use unicode_width::UnicodeWidthStr;

use crate::app::{Action, AppState, ContextMenuEntryKind, ContextMenuItem, Effect, ModalLayer};
use crate::external_editor::{
    handle_external_editor_effects_with, is_external_editor_disabled_val,
    resolve_editor_command_with, split_editor_command,
};
use crate::views::truncate_to_width;

use super::TestSession;

#[test]
fn test_backend_render_tiny_and_normal_no_panic() {
    let mut ts = TestSession::with_snapshot(super::snapshot());
    for (w, h) in [(10, 4), (45, 12), (59, 15), (120, 40)] {
        let _ = ts.render(w, h);
    }
}

#[test]
fn tab_switch_and_focus_ring_and_splitter_drag() {
    let mut ts = TestSession::with_snapshot(super::snapshot());
    let _ = ts.render(120, 40);

    for (ch, expected) in [
        ('1', WorkspaceTab::Commit),
        ('2', WorkspaceTab::Log),
        ('3', WorkspaceTab::Stash),
        ('4', WorkspaceTab::Console),
    ] {
        ts.send_key(KeyCode::Char(ch), KeyModifiers::NONE);
        assert_eq!(ts.state().active_tab, expected);
    }

    ts.dispatch(Action::SwitchTab(WorkspaceTab::Log));
    assert_eq!(ts.state().focused_pane, PaneId::LogCommitTable);
    ts.dispatch(Action::CycleFocus { reverse: false });
    assert_eq!(ts.state().focused_pane, PaneId::LogChangesTree);
    ts.dispatch(Action::CycleFocus { reverse: false });
    assert_eq!(ts.state().focused_pane, PaneId::LogDetailsDiff);
    ts.dispatch(Action::CycleFocus { reverse: false });
    assert_eq!(ts.state().focused_pane, PaneId::LogBranchesTree);

    let _ = ts.render(120, 40);
    let (_, container) = ts
        .state()
        .splitter_containers
        .get(&SplitterId::LogLeftHorizontal)
        .copied()
        .unwrap();
    let initial_ratio = ts.state().splitters.ratio(SplitterId::LogLeftHorizontal);
    let split = ts.state().splitters.split(
        container,
        SplitterId::LogLeftHorizontal,
        SplitAxis::Horizontal,
    );
    let div_x = split.divider.x;
    let div_y = split.divider.y + 2;

    ts.send_mouse(MouseEventKind::Down(MouseButton::Left), div_x, div_y);
    assert!(ts.state().drag_state.is_some());

    ts.send_mouse(MouseEventKind::Drag(MouseButton::Left), div_x + 12, div_y);
    let dragged_ratio = ts.state().splitters.ratio(SplitterId::LogLeftHorizontal);
    assert!(dragged_ratio > initial_ratio);

    ts.send_mouse(MouseEventKind::Up(MouseButton::Left), div_x + 12, div_y);
    assert!(ts.state().drag_state.is_none());

    let _ = ts.render(120, 40);
    let split2 = ts.state().splitters.split(
        container,
        SplitterId::LogLeftHorizontal,
        SplitAxis::Horizontal,
    );
    let dx2 = split2.divider.x;
    for _ in 0..2 {
        ts.send_mouse(MouseEventKind::Down(MouseButton::Left), dx2, div_y);
    }
    let reset_ratio = ts.state().splitters.ratio(SplitterId::LogLeftHorizontal);
    assert!((reset_ratio - initial_ratio).abs() < 0.001);
    let _ = Rect::ZERO;
}

#[test]
fn mouse_wheel_routes_to_hovered_pane_and_right_click_preselects() {
    let oid1 = Oid::from_bytes([1u8; 20]);
    let oid2 = Oid::from_bytes([2u8; 20]);
    let snap = RepoSnapshot {
        repo_name: "demo".into(),
        workdir: ".".into(),
        git_dir: ".git".into(),
        common_dir: ".git".into(),
        state: RepositoryState::Clean,
        is_unborn_head: false,
        user_name: None,
        user_email: None,
        recent_branches: vec![],
        stashes: vec![],
        ongoing_onto: None,
        bisect: None,
        remotes: Vec::new(),
        worktrees: Vec::new(),
        submodules: Vec::new(),
        has_lfs: false,
        is_shallow: false,
        loaded_limit: 0,
        has_more_commits: false,
        head_branch: Some("main".into()),
        head_oid: Some(oid1),
        refs: vec![],
        commits: vec![
            CommitSummary {
                oid: oid1,
                parents: vec![],
                author_name: "Alice".into(),
                author_email: "a@b.c".into(),
                timestamp: 0,
                subject: "first".into(),
                author_timestamp: 0,
                body: String::new(),
            },
            CommitSummary {
                oid: oid2,
                parents: vec![oid1],
                author_name: "Bob".into(),
                author_email: "b@c.d".into(),
                timestamp: 1,
                subject: "second".into(),
                author_timestamp: 0,
                body: String::new(),
            },
        ],
    };
    let mut ts = TestSession::with_snapshot(snap);
    ts.state_mut().console.push_started(GitConsoleEntry {
        id: 1,
        started_at: Local::now(),
        duration_ms: Some(5),
        command_line: "git status".into(),
        output_lines: (0..20)
            .map(|i| ConsoleLine {
                stream: ConsoleStream::Stdout,
                text: format!("short line {i}"),
            })
            .collect(),
        exit_code: Some(0),
    });

    ts.dispatch(Action::SwitchTab(WorkspaceTab::Console));
    assert_eq!(ts.state().focused_pane, PaneId::ConsoleCommandList);
    let _ = ts.render(120, 40);
    ts.send_mouse(MouseEventKind::ScrollDown, 90, 10);
    assert_eq!(ts.state().console.output_scroll(), 0);

    ts.state_mut().console.entries_mut_for_test()[0].output_lines = (0..60)
        .map(|i| ConsoleLine {
            stream: ConsoleStream::Stdout,
            text: format!("line {i}"),
        })
        .collect();
    let _ = ts.render(120, 40);
    ts.send_mouse(MouseEventKind::ScrollDown, 90, 10);
    assert!(ts.state().console.output_scroll() > 0);
    assert_eq!(ts.state().focused_pane, PaneId::ConsoleCommandList);

    ts.dispatch(Action::SwitchTab(WorkspaceTab::Log));
    let _ = ts.render(120, 40);
    assert_eq!(ts.state().log.graph.selected_idx, 0);

    ts.send_mouse(MouseEventKind::Down(MouseButton::Right), 40, 5);
    assert_eq!(ts.state().log.graph.selected_idx, 1);
    assert_eq!(ts.state().modal_stack.len(), 1);

    let _ = ts.render(120, 40);
    ts.send_mouse(MouseEventKind::Down(MouseButton::Left), 5, 3);
    assert!(ts.state().modal_stack.is_empty());
    assert_eq!(ts.state().log.graph.selected_idx, 1);
}

#[test]
fn stale_epoch_events_are_silently_dropped_on_ui_arrival() {
    let mut ts = TestSession::new(super::test_state(".".into()));
    ts.state_mut()
        .request_slots
        .note_dispatched(jbgit_core::RequestSlotId::RepoSnapshot, 5);
    ts.state_mut()
        .request_slots
        .note_dispatched(jbgit_core::RequestSlotId::StageStatus, 4);

    let stale_snap = Arc::new(RepoSnapshot {
        repo_name: "stale".into(),
        workdir: ".".into(),
        git_dir: ".git".into(),
        common_dir: ".git".into(),
        state: RepositoryState::Clean,
        is_unborn_head: true,
        user_name: None,
        user_email: None,
        recent_branches: vec![],
        stashes: vec![],
        ongoing_onto: None,
        bisect: None,
        remotes: Vec::new(),
        worktrees: Vec::new(),
        submodules: Vec::new(),
        has_lfs: false,
        is_shallow: false,
        loaded_limit: 0,
        has_more_commits: false,
        head_branch: Some("old".into()),
        head_oid: None,
        refs: vec![],
        commits: vec![],
    });
    ts.deliver_engine_event(EngineEvent::RepoSnapshotLoaded {
        snapshot: stale_snap,
        epoch: 3,
    });
    assert!(ts.state().repo_snapshot.is_none());

    let fresh_snap = Arc::new(RepoSnapshot {
        repo_name: "fresh".into(),
        workdir: ".".into(),
        git_dir: ".git".into(),
        common_dir: ".git".into(),
        state: RepositoryState::Clean,
        is_unborn_head: false,
        user_name: None,
        user_email: None,
        recent_branches: vec![],
        stashes: vec![],
        ongoing_onto: None,
        bisect: None,
        remotes: Vec::new(),
        worktrees: Vec::new(),
        submodules: Vec::new(),
        has_lfs: false,
        is_shallow: false,
        loaded_limit: 0,
        has_more_commits: false,
        head_branch: Some("main".into()),
        head_oid: Some(Oid::ZERO),
        refs: vec![],
        commits: vec![],
    });
    ts.deliver_engine_event(EngineEvent::RepoSnapshotLoaded {
        snapshot: fresh_snap,
        epoch: 5,
    });
    assert_eq!(
        ts.state()
            .repo_snapshot
            .as_ref()
            .map(|s| s.repo_name.as_str()),
        Some("fresh")
    );

    let stale_stage = Arc::new(StageStatusSnapshot {
        unstaged: vec![StageEntry {
            path: "stale.rs".into(),
            old_path: None,
            kind: FileStatusKind::Modified,
        }],
        ..Default::default()
    });
    ts.deliver_engine_event(EngineEvent::StageStatusLoaded {
        status: stale_stage,
        epoch: 2,
    });
    assert!(ts.state().stage_workbench.status().is_none());
}

#[test]
fn cjk_truncate_respects_display_width() {
    let s = "修复中文提交信息对齐问题";
    for max_w in 0..=12 {
        let out = truncate_to_width(s, max_w);
        assert!(
            out.width() <= max_w,
            "truncated '{out}' width {} exceeded {max_w}",
            out.width()
        );
    }
    assert_eq!(truncate_to_width(s, 5), "修复…");
}

#[test]
fn context_menu_cascading_submenu_mnemonics_hover_and_no_stacking() {
    let mut ts = TestSession::new(super::test_state(".".into()));
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
    ts.dispatch(Action::SwitchTab(WorkspaceTab::Commit));
    let _ = ts.render(120, 40);

    ts.send_mouse(MouseEventKind::Down(MouseButton::Right), 10, 6);
    assert_eq!(ts.state().stage_workbench.selected_idx(), 2);
    assert_eq!(ts.state().modal_stack.len(), 1);

    let _ = ts.render(120, 40);
    ts.send_mouse(MouseEventKind::Moved, 14, 8);
    assert!(matches!(
        ts.state().modal_stack.last(),
        Some(ModalLayer::ContextMenu { selected: 1, .. })
    ));

    let _ = ts.take_requests();
    ts.send_mouse(MouseEventKind::Down(MouseButton::Right), 14, 8);
    assert!(ts.take_requests().is_empty());
    assert_eq!(ts.state().modal_stack.len(), 1);

    ts.send_key(KeyCode::Char('p'), KeyModifiers::NONE);
    assert_eq!(ts.state().modal_stack.len(), 2);
    assert!(matches!(
        ts.state().modal_stack.last(),
        Some(ModalLayer::ContextMenu { depth: 1, .. })
    ));

    ts.send_key(KeyCode::Char('s'), KeyModifiers::NONE);
    assert!(ts.state().modal_stack.is_empty());
    assert_eq!(ts.take_requests().len(), 1);
    let _ = (
        ContextMenuEntryKind::Submenu(vec![]),
        ContextMenuItem::run("x", None, None, vec![Action::RefreshAll]),
    );
}

#[test]
fn index_lock_retry_replays_blocked_cli_command() {
    let mut ts = TestSession::new(super::test_state(".".into()));
    ts.deliver_engine_event(EngineEvent::IndexLockDetected {
        lock_path: ".git/index.lock".into(),
        message: "Unable to create '.git/index.lock': File exists.".into(),
        retry_args: vec!["add".into(), "--pathspec-from-file=-".into()],
        retry_stdin: Some(b"src/lib.rs\0".to_vec()),
        retry_is_read_only: false,
        retry_refresh_after: RefreshScope::StageStatus,
    });
    assert_eq!(ts.state().modal_stack.len(), 1);
    let _ = ts.take_requests();

    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);
    assert!(ts.state().modal_stack.is_empty());
    let reqs = ts.take_requests();
    assert_eq!(reqs.len(), 1);
    match &reqs[0].0 {
        TaskRequest::RunGitCli {
            args,
            stdin_payload,
            is_read_only,
            refresh_after,
            ..
        } => {
            assert_eq!(
                args,
                &vec!["add".to_string(), "--pathspec-from-file=-".to_string()]
            );
            assert_eq!(stdin_payload.as_deref(), Some(b"src/lib.rs\0".as_slice()));
            assert!(!is_read_only);
            assert_eq!(*refresh_after, RefreshScope::StageStatus);
        }
        other => panic!("expected RunGitCli retry request, got {other:?}"),
    }
}

#[test]
fn detached_head_and_overflow_hover_tooltips_and_overflow_clicks() {
    let oid = Oid::from_bytes([7u8; 20]);
    let snap = RepoSnapshot {
        repo_name: "demo".into(),
        workdir: ".".into(),
        git_dir: ".git".into(),
        common_dir: ".git".into(),
        state: RepositoryState::Clean,
        is_unborn_head: false,
        user_name: None,
        user_email: None,
        recent_branches: vec![],
        stashes: vec![],
        ongoing_onto: None,
        bisect: None,
        remotes: Vec::new(),
        worktrees: Vec::new(),
        submodules: Vec::new(),
        has_lfs: false,
        is_shallow: false,
        loaded_limit: 0,
        has_more_commits: false,
        head_branch: None,
        head_oid: Some(oid),
        refs: vec![
            RefInfo {
                name: "alpha".into(),
                full_name: "refs/heads/alpha".into(),
                target_oid: oid,
                kind: RefKind::LocalBranch,
                upstream: None,
                ahead_behind: None,
                is_head: false,
            },
            RefInfo {
                name: "beta".into(),
                full_name: "refs/heads/beta".into(),
                target_oid: oid,
                kind: RefKind::LocalBranch,
                upstream: None,
                ahead_behind: None,
                is_head: false,
            },
            RefInfo {
                name: "gamma".into(),
                full_name: "refs/heads/gamma".into(),
                target_oid: oid,
                kind: RefKind::LocalBranch,
                upstream: None,
                ahead_behind: None,
                is_head: false,
            },
            RefInfo {
                name: "v1.0.0".into(),
                full_name: "refs/tags/v1.0.0".into(),
                target_oid: oid,
                kind: RefKind::Tag,
                upstream: None,
                ahead_behind: None,
                is_head: false,
            },
            RefInfo {
                name: "v1.0.1".into(),
                full_name: "refs/tags/v1.0.1".into(),
                target_oid: oid,
                kind: RefKind::Tag,
                upstream: None,
                ahead_behind: None,
                is_head: false,
            },
        ],
        commits: vec![CommitSummary {
            oid,
            parents: vec![],
            author_name: "Alice".into(),
            author_email: "a@b.c".into(),
            timestamp: 0,
            subject: "detached commit with many refs".into(),
            author_timestamp: 0,
            body: String::new(),
        }],
    };
    let mut ts = TestSession::with_snapshot(snap);
    let buf = ts.render(100, 24);
    let row_line = super::row_text(&buf, 4);
    let col_of = |needle: &str| {
        let byte_idx = row_line.find(needle).unwrap();
        row_line[..byte_idx].width() as u16
    };
    let head_x = col_of("HEAD");
    let plus_x = col_of("+");

    ts.send_mouse(MouseEventKind::Moved, head_x, 4);
    let buf = ts.render(100, 24);
    let full_text = super::screen_text(&buf);
    assert!(full_text.contains("Detached HEAD"), "{full_text}");

    ts.send_mouse(MouseEventKind::Moved, plus_x, 4);
    let buf = ts.render(100, 24);
    let full_text = super::screen_text(&buf);
    assert!(full_text.contains("All Commit References"), "{full_text}");
    assert!(full_text.contains("v1.0.1"), "{full_text}");

    ts.send_mouse(MouseEventKind::Down(MouseButton::Left), plus_x, 4);
    assert!(ts.state().modal_stack.is_empty());

    ts.send_mouse(MouseEventKind::Down(MouseButton::Right), plus_x, 4);
    assert_eq!(ts.state().modal_stack.len(), 1);
}

#[test]
fn long_edge_arrow_hover_click_jump_center_and_alt_left_return() {
    let oid_n = |n: u64| {
        let mut bytes = [0u8; 20];
        bytes[..8].copy_from_slice(&n.to_le_bytes());
        Oid::from_bytes(bytes)
    };

    let mut commits = Vec::new();
    for i in 0..=41u64 {
        let parents = if i == 0 {
            vec![oid_n(1), oid_n(41)]
        } else if i < 41 {
            vec![oid_n(i + 1)]
        } else {
            vec![]
        };
        commits.push(CommitSummary {
            oid: oid_n(i),
            parents,
            author_name: "Alice".into(),
            author_email: "a@b.c".into(),
            timestamp: (1000 - i) as i64,
            subject: format!("commit {i}"),
            author_timestamp: 0,
            body: String::new(),
        });
    }

    let snap = RepoSnapshot {
        repo_name: "demo".into(),
        workdir: ".".into(),
        git_dir: ".git".into(),
        common_dir: ".git".into(),
        state: RepositoryState::Clean,
        is_unborn_head: false,
        user_name: None,
        user_email: None,
        recent_branches: vec![],
        stashes: vec![],
        ongoing_onto: None,
        bisect: None,
        remotes: Vec::new(),
        worktrees: Vec::new(),
        submodules: Vec::new(),
        has_lfs: false,
        is_shallow: false,
        loaded_limit: 0,
        has_more_commits: false,
        head_branch: Some("main".into()),
        head_oid: Some(oid_n(0)),
        refs: vec![RefInfo {
            name: "main".into(),
            full_name: "refs/heads/main".into(),
            target_oid: oid_n(0),
            kind: RefKind::LocalBranch,
            upstream: None,
            ahead_behind: None,
            is_head: true,
        }],
        commits,
    };
    let mut ts = TestSession::with_snapshot(snap);

    let buf = ts.render(120, 20);
    let row1 = super::row_text(&buf, 5);
    let arrow_byte = row1
        .find('▾')
        .expect("expected long-edge down arrow on row 1");
    let arrow_x = row1[..arrow_byte].width() as u16;

    ts.send_mouse(MouseEventKind::Moved, arrow_x, 5);
    let buf = ts.render(120, 20);
    let status_line = super::row_text(&buf, 19);
    assert!(status_line.contains(&oid_n(41).short()), "{status_line}");
    assert!(status_line.contains("commit 41"), "{status_line}");

    ts.send_mouse(MouseEventKind::Down(MouseButton::Left), arrow_x, 5);
    assert_eq!(ts.state().log.graph.selected_idx, 41);
    assert!(ts.state().log.graph.scroll_offset > 0);

    ts.send_key(KeyCode::Left, KeyModifiers::ALT);
    assert_eq!(ts.state().log.graph.selected_idx, 0);
}

#[test]
fn stage_workbench_dual_presence_visual_select_and_amend_draft_restore() {
    let mut ts = TestSession::new(super::test_state(".".into()));
    ts.deliver_engine_event(EngineEvent::StageStatusLoaded {
        status: Arc::new(StageStatusSnapshot {
            staged: vec![StageEntry {
                path: "src/foo.rs".into(),
                old_path: None,
                kind: FileStatusKind::Added,
            }],
            unstaged: vec![StageEntry {
                path: "src/foo.rs".into(),
                old_path: None,
                kind: FileStatusKind::Modified,
            }],
            ..Default::default()
        }),
        epoch: 1,
    });
    ts.dispatch(Action::SwitchTab(WorkspaceTab::Commit));
    ts.dispatch(Action::StageToggleGroupByDir);

    assert_eq!(ts.state().stage_workbench.rows().len(), 4);

    ts.dispatch(Action::SelectCommitStageRow(1));
    assert_eq!(
        ts.state()
            .stage_workbench
            .current_diff_key()
            .map(|k| k.side),
        Some(StageDiffSide::Staged)
    );

    ts.dispatch(Action::SelectCommitStageRow(3));
    assert_eq!(
        ts.state()
            .stage_workbench
            .current_diff_key()
            .map(|k| k.side),
        Some(StageDiffSide::Unstaged)
    );

    let vcs = b"fn a() {}\n";
    let staged = b"fn a() {}\nfn staged_line() {}\n";
    let local = b"fn a() {}\nfn staged_line() {}\nfn local_1() {}\nfn local_2() {}\n";
    let tracker = jbgit_core::StageLineTracker::new(vcs, staged, local);
    let (hunks, is_truncated) = tracker.build_interactive_hunks(false, DiffContext::Three, 20_000);
    let diff = Arc::new(ThreeDocStageDiff {
        key: StageDiffKey {
            path: "src/foo.rs".into(),
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
    });
    let epoch = ts
        .state()
        .request_slots
        .latest(jbgit_core::RequestSlotId::StageFileDiff);
    ts.deliver_engine_event(EngineEvent::StageFileDiffLoaded { diff, epoch });

    ts.dispatch(Action::FocusPane(PaneId::CommitDiffPreview));
    ts.dispatch(Action::StageDiffMoveCursor(3));
    ts.dispatch(Action::StageDiffToggleVisual);
    let _ = ts.take_requests();
    ts.dispatch(Action::StageDiffStageSelection);
    let reqs = ts.take_requests();
    match reqs
        .into_iter()
        .find_map(|(r, _)| match r {
            TaskRequest::ApplyStageBuffer { new_staged_bytes, .. } => Some(new_staged_bytes),
            _ => None,
        }) {
        Some(new_staged_bytes) => {
            assert_eq!(
                new_staged_bytes,
                b"fn a() {}\nfn staged_line() {}\nfn local_1() {}\n"
            );
        }
        None => panic!("expected ApplyStageBuffer"),
    }

    ts.dispatch(Action::FocusPane(PaneId::CommitMessageEditor));
    ts.dispatch(Action::CommitEditorPaste("wip: my draft".into()));
    ts.deliver_engine_event(EngineEvent::AmendPreviewLoaded {
        preview: Some(AmendCommitPreview {
            short_sha: "a8f3d21".into(),
            message: "feat: previous commit".into(),
            files: vec![FileChange {
                path: "prev.rs".into(),
                old_path: None,
                kind: FileStatusKind::Modified,
                additions: 1,
                deletions: 1,
                is_binary: false,
                exists_in_workdir: true,
                old_oid: None,
                new_oid: None,
            }],
        }),
        epoch: 1,
    });

    ts.dispatch(Action::CommitToggleAmend);
    assert!(ts.state().stage_workbench.amend());
    assert_eq!(
        ts.state().stage_workbench.editor().text(),
        "feat: previous commit"
    );
    assert!(
        ts.state()
            .stage_workbench
            .rows()
            .iter()
            .any(|r| r.label.contains("Head Commit to Amend (a8f3d21)"))
    );

    let _ = ts.render(120, 40);

    ts.dispatch(Action::CommitToggleAmend);
    assert!(!ts.state().stage_workbench.amend());
    assert_eq!(ts.state().stage_workbench.editor().text(), "wip: my draft");
}

fn draft_test_session(tag: &str) -> (TestSession, PathBuf, PathBuf) {
    let mut ts = TestSession::new(super::test_state(".".into()));
    ts.deliver_engine_event(EngineEvent::StageStatusLoaded {
        status: Arc::new(StageStatusSnapshot::default()),
        epoch: 1,
    });
    let dir = std::env::temp_dir().join(format!(
        "jbgit-test-draft-debounce-{tag}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("jbgit-commit-draft");
    ts.state_mut().stage_workbench.load_draft(file.clone());
    ts.dispatch(Action::SwitchTab(WorkspaceTab::Commit));
    ts.dispatch(Action::FocusPane(PaneId::CommitMessageEditor));
    (ts, dir, file)
}

#[test]
fn commit_draft_typing_is_debounced_then_written_in_background() {
    let (mut ts, dir, file) = draft_test_session("async");
    ts.type_str("feat: twenty chars!!");
    assert!(!file.exists(), "keystrokes must not write the draft synchronously");
    assert!(ts.state().stage_workbench.draft_dirty());
    let due = ts
        .state()
        .stage_workbench
        .draft_save_deadline_for_test()
        .expect("debounce armed");
    assert!(ts.next_deadline().is_some_and(|d| d <= due));

    let _ = ts
        .session
        .advance_to(due - Duration::from_millis(1), Some(&ts.engine), &mut ts.host);
    ts.state_mut().stage_workbench.wait_draft_writes();
    assert!(!file.exists());

    let _ = ts.session.advance_to(due, Some(&ts.engine), &mut ts.host);
    assert!(!ts.state().stage_workbench.draft_dirty());
    assert!(
        ts.state()
            .stage_workbench
            .draft_save_deadline_for_test()
            .is_none()
    );
    ts.state_mut().stage_workbench.wait_draft_writes();
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "feat: twenty chars!!");

    for _ in 0..20 {
        ts.dispatch(Action::CommitEditorBackspace);
    }
    assert!(file.exists());
    let due = ts
        .state()
        .stage_workbench
        .draft_save_deadline_for_test()
        .unwrap();
    let _ = ts.session.advance_to(due, Some(&ts.engine), &mut ts.host);
    ts.state_mut().stage_workbench.wait_draft_writes();
    assert!(!file.exists());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn commit_draft_flushes_synchronously_on_exit_and_submit() {
    let (mut ts, dir, file) = draft_test_session("flush");
    ts.dispatch(Action::CommitEditorPaste("wip: exit flush".into()));
    assert!(!file.exists());
    let _ = ts.session.shutdown();
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "wip: exit flush");

    ts.dispatch(Action::CommitEditorChar('!'));
    let due = ts
        .state()
        .stage_workbench
        .draft_save_deadline_for_test()
        .unwrap();
    let _ = ts.session.advance_to(due, Some(&ts.engine), &mut ts.host);
    ts.dispatch(Action::CommitEditorChar('?'));
    let _ = ts.session.shutdown();
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "wip: exit flush!?");

    ts.dispatch(Action::CommitEditorChar('+'));
    let due = ts
        .state()
        .stage_workbench
        .draft_save_deadline_for_test()
        .unwrap();
    let _ = ts.session.advance_to(due, Some(&ts.engine), &mut ts.host);
    assert!(!ts.state().stage_workbench.draft_dirty());
    let _ = ts.session.shutdown();
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "wip: exit flush!?+");

    ts.dispatch(Action::CommitEditorChar('#'));
    assert!(ts.state().stage_workbench.draft_dirty());
    ts.dispatch(Action::SubmitCommit { and_push: false });
    assert!(!ts.state().stage_workbench.draft_dirty());
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "wip: exit flush!?+#");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stage_review_fixes_amend_diff_gutter_click_draft_persistence_and_push_confirm() {
    let mut ts = TestSession::new(super::test_state(".".into()));
    ts.deliver_engine_event(EngineEvent::StageStatusLoaded {
        status: Arc::new(StageStatusSnapshot::default()),
        epoch: 1,
    });
    let tmp_dir = std::env::temp_dir().join(format!("jbgit-test-draft-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&tmp_dir);
    let draft_file = tmp_dir.join("jbgit-commit-draft");
    let _ = std::fs::remove_file(&draft_file);
    ts.state_mut().stage_workbench.load_draft(draft_file.clone());
    ts.dispatch(Action::SwitchTab(WorkspaceTab::Commit));

    ts.dispatch(Action::FocusPane(PaneId::CommitMessageEditor));
    ts.dispatch(Action::CommitEditorPaste("draft: unsent note".into()));
    assert!(
        ts.state()
            .stage_workbench
            .commit_history_candidates(ts.state().repo_snapshot.as_deref())
            .iter()
            .any(|c| c == "draft: unsent note")
    );
    let _ = ts.session.shutdown();
    let mut reloaded = AppState::new(PathBuf::from("."), None);
    reloaded.stage_workbench.load_draft(draft_file);
    assert_eq!(reloaded.stage_workbench.editor().text(), "draft: unsent note");
    let _ = std::fs::remove_dir_all(&tmp_dir);

    ts.deliver_engine_event(EngineEvent::AmendPreviewLoaded {
        preview: Some(AmendCommitPreview {
            short_sha: "1122334".into(),
            message: "feat: head commit".into(),
            files: vec![FileChange {
                path: "prev.rs".into(),
                old_path: None,
                kind: FileStatusKind::Modified,
                additions: 1,
                deletions: 1,
                is_binary: false,
                exists_in_workdir: true,
                old_oid: None,
                new_oid: None,
            }],
        }),
        epoch: 1,
    });
    let _ = ts.take_requests();
    ts.dispatch(Action::CommitToggleAmend);
    let reqs = ts.take_requests();
    assert!(
        reqs.iter()
            .any(|(r, _)| matches!(r, TaskRequest::LoadAmendPreview))
    );
    let amend_file_idx = ts
        .state()
        .stage_workbench
        .rows()
        .iter()
        .position(|r| matches!(r.kind, StageTreeRowKind::AmendFile { .. }))
        .expect("AmendFile row");
    ts.dispatch(Action::SelectCommitStageRow(amend_file_idx));
    assert_eq!(
        ts.state()
            .stage_workbench
            .current_diff_key()
            .map(|k| k.side),
        Some(StageDiffSide::AmendCommit)
    );

    ts.deliver_engine_event(EngineEvent::AmendPreviewLoaded {
        preview: Some(AmendCommitPreview {
            short_sha: "5566778".into(),
            message: "feat: newer head commit".into(),
            files: vec![],
        }),
        epoch: 2,
    });
    assert_eq!(
        ts.state().stage_workbench.editor().text(),
        "feat: newer head commit"
    );
    ts.dispatch(Action::CommitToggleAmend);

    ts.deliver_engine_event(EngineEvent::StageStatusLoaded {
        status: Arc::new(StageStatusSnapshot {
            staged: vec![StageEntry {
                path: "src/lib.rs".into(),
                old_path: None,
                kind: FileStatusKind::Modified,
            }],
            unstaged: vec![StageEntry {
                path: "src/lib.rs".into(),
                old_path: None,
                kind: FileStatusKind::Modified,
            }],
            ..Default::default()
        }),
        epoch: 2,
    });
    ts.dispatch(Action::StageToggleGroupByDir);
    ts.dispatch(Action::SelectCommitStageRow(3));
    let vcs = b"let x = old_val;\n";
    let staged = b"let x = staged_val;\n";
    let local = b"let x = local_val;\n";
    let tracker = jbgit_core::StageLineTracker::new(vcs, staged, local);
    let (hunks, is_truncated) = tracker.build_interactive_hunks(false, DiffContext::Three, 20_000);
    ts.deliver_engine_event(EngineEvent::StageFileDiffLoaded {
        diff: Arc::new(ThreeDocStageDiff {
            key: StageDiffKey {
                path: "src/lib.rs".into(),
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
        epoch: 9,
    });

    let buf = ts.render(120, 36);
    let mut gutter_pos = None;
    for y in 0..36 {
        for x in 0..120 {
            if buf[(x, y)].symbol() == "▚" {
                gutter_pos = Some((x, y));
                break;
            }
        }
        if gutter_pos.is_some() {
            break;
        }
    }
    let (gx, gy) = gutter_pos.expect("mixed 3-way gutter bar rendered");
    ts.send_mouse(MouseEventKind::Down(MouseButton::Left), gx, gy);
    assert!(matches!(
        ts.state().modal_stack.last(),
        Some(ModalLayer::ThreeWayInspector { .. })
    ));
    let _ = ts.render(120, 36);
    ts.dispatch(Action::PopModal);
    assert!(ts.state().modal_stack.three_way_range_idx().is_none());

    ts.dispatch(Action::FocusPane(PaneId::CommitDiffPreview));
    for ch in [']', 'c'] {
        ts.send_key(KeyCode::Char(ch), KeyModifiers::NONE);
    }
    assert_eq!(ts.state().stage_workbench.diff_context(), DiffContext::Three);

    let _ = ts.take_requests();
    let mut snap = super::snapshot();
    snap.remotes = super::remote_infos(&["origin"]);
    ts.deliver_engine_event(EngineEvent::RepoSnapshotLoaded {
        snapshot: Arc::new(snap),
        epoch: 10,
    });
    ts.deliver_engine_event(EngineEvent::CommitCompleted(CommitOutcome {
        success: true,
        commit_oid: Some(jbgit_core::Oid::from_bytes([12u8; 20])),
        short_sha: Some("abc1234".into()),
        summary: "Committed abc1234".into(),
        error_output: None,
        and_push: true,
        preserved_message: None,
    }));
    assert!(ts.take_git_ops().is_empty());
    assert!(
        matches!(ts.state().modal_stack.last(), Some(ModalLayer::PushDialog(_))),
        "Commit & Push must flow into full PushDialog"
    );
    // Confirming PushDialog and receiving PushRejected must open the Push Rejected dialog.
    ts.dispatch(Action::PushConfirm);
    let push_op = ts
        .take_git_ops()
        .into_iter()
        .find(|op| matches!(op, jbgit_core::GitOp::Push { .. }))
        .expect("confirming Commit & Push PushDialog must emit typed GitOp::Push");
    let rejected = jbgit_core::GitOpOutcome::failed(push_op, jbgit_core::GitFailure::PushRejected);
    ts.deliver_engine_event(EngineEvent::GitOpFinished(rejected));
    assert!(matches!(
        ts.state().modal_stack.last(),
        Some(ModalLayer::Dialog(d))
            if matches!(d.as_ref(), crate::dialogs::Dialog::Choice(c) if c.title == ts.state().i18n.t("dialog.push.rejected_title"))
    ));
}

#[test]
fn vertical_layout_ratios_render_drag_and_reset() {
    let temp = tempfile::tempdir().unwrap();
    let cfg_dir = temp.path().join(".jbgit");
    std::fs::create_dir_all(&cfg_dir).unwrap();
    std::fs::write(
        cfg_dir.join("jbgit.toml"),
        r#"
[layout]
log_details_ratio = [1, 3]
log_diff_right_ratio = [3, 1]
log_diff_bottom_ratio = [2, 3]
commit_left_ratio = [3, 1]
stash_preview_ratio = [1, 1]
"#,
    )
    .unwrap();

    let mut ts = TestSession::new(AppState::new(temp.path().to_path_buf(), None));
    ts.deliver_engine_event(EngineEvent::RepoSnapshotLoaded {
        snapshot: Arc::new(RepoSnapshot {
            repo_name: "demo".into(),
            workdir: temp.path().to_path_buf(),
            git_dir: temp.path().join(".git"),
            common_dir: temp.path().join(".git"),
            state: RepositoryState::Clean,
            is_unborn_head: false,
            user_name: None,
            user_email: None,
            recent_branches: vec![],
            stashes: vec![StashEntry {
                index: 0,
                oid: Oid::from_bytes([2u8; 20]),
                message: "WIP on main: test".into(),
                timestamp: 1_700_000_000,
            }],
            ongoing_onto: None,
            bisect: None,
            remotes: Vec::new(),
            worktrees: Vec::new(),
            submodules: Vec::new(),
            has_lfs: false,
        is_shallow: false,
        loaded_limit: 0,
        has_more_commits: false,
            head_branch: Some("main".into()),
            head_oid: Some(Oid::from_bytes([1u8; 20])),
            refs: vec![],
            commits: vec![],
        }),
        epoch: 1,
    });

    assert!((ts.state().splitters.ratio(SplitterId::LogDetailsVertical) - 0.25).abs() < 1e-4);
    assert!(
        (ts.state()
            .splitters
            .ratio(SplitterId::LogDiffRightHorizontal)
            - 0.75)
            .abs()
            < 1e-4
    );
    assert!(
        (ts.state()
            .splitters
            .ratio(SplitterId::LogDiffBottomVertical)
            - 0.40)
            .abs()
            < 1e-4
    );
    assert!((ts.state().splitters.ratio(SplitterId::CommitLeftVertical) - 0.75).abs() < 1e-4);
    assert!(
        (ts.state()
            .splitters
            .ratio(SplitterId::StashPreviewVertical)
            - 0.50)
            .abs()
            < 1e-4
    );

    let cases = [
        (
            WorkspaceTab::Log,
            None,
            SplitterId::LogDetailsVertical,
            SplitAxis::Vertical,
            0.25f32,
            6i16,
        ),
        (
            WorkspaceTab::Log,
            Some(crate::log_view::DiffPreviewDock::Right),
            SplitterId::LogDiffRightHorizontal,
            SplitAxis::Horizontal,
            0.75f32,
            -15i16,
        ),
        (
            WorkspaceTab::Log,
            Some(crate::log_view::DiffPreviewDock::Bottom),
            SplitterId::LogDiffBottomVertical,
            SplitAxis::Vertical,
            0.40f32,
            6i16,
        ),
        (
            WorkspaceTab::Commit,
            None,
            SplitterId::CommitLeftVertical,
            SplitAxis::Vertical,
            0.75f32,
            -6i16,
        ),
        (
            WorkspaceTab::Stash,
            None,
            SplitterId::StashPreviewVertical,
            SplitAxis::Vertical,
            0.50f32,
            5i16,
        ),
    ];

    for (tab, dock, splitter_id, expected_axis, expected_default, delta) in cases {
        ts.dispatch(Action::SwitchTab(tab));
        if let Some(d) = dock {
            ts.state_mut().log.details.diff_preview = true;
            ts.state_mut().log.details.diff_dock = d;
        } else {
            ts.state_mut().log.details.diff_preview = false;
        }
        let _ = ts.render(120, 40);

        let (axis, container) = ts
            .state()
            .splitter_containers
            .get(&splitter_id)
            .copied()
            .unwrap_or_else(|| panic!("splitter {splitter_id:?} not registered on {tab:?}"));
        assert_eq!(axis, expected_axis);

        let split = ts
            .state()
            .splitters
            .split(container, splitter_id, expected_axis);
        assert!(!split.collapsed);
        let (div_x, div_y, target_x, target_y) = match expected_axis {
            SplitAxis::Vertical => (
                split.divider.x + 2,
                split.divider.y,
                split.divider.x + 2,
                (split.divider.y as i16 + delta) as u16,
            ),
            SplitAxis::Horizontal => (
                split.divider.x,
                split.divider.y + 2,
                (split.divider.x as i16 + delta) as u16,
                split.divider.y + 2,
            ),
        };

        ts.send_mouse(MouseEventKind::Down(MouseButton::Left), div_x, div_y);
        assert!(ts.state().drag_state.is_some());

        ts.send_mouse(MouseEventKind::Drag(MouseButton::Left), target_x, target_y);
        let dragged = ts.state().splitters.ratio(splitter_id);
        if delta > 0 {
            assert!(dragged > expected_default);
        } else {
            assert!(dragged < expected_default);
        }

        ts.send_mouse(MouseEventKind::Up(MouseButton::Left), target_x, target_y);

        let _ = ts.render(120, 40);
        let split2 = ts
            .state()
            .splitters
            .split(container, splitter_id, expected_axis);
        let (reset_x, reset_y) = match expected_axis {
            SplitAxis::Vertical => (split2.divider.x + 2, split2.divider.y),
            SplitAxis::Horizontal => (split2.divider.x, split2.divider.y + 2),
        };
        for _ in 0..2 {
            ts.send_mouse(MouseEventKind::Down(MouseButton::Left), reset_x, reset_y);
        }
        assert!(
            (ts.state().splitters.ratio(splitter_id) - expected_default).abs() < 1e-4,
            "reset for {splitter_id:?} expected {expected_default}, got {}",
            ts.state().splitters.ratio(splitter_id)
        );
    }
}

#[test]
fn console_overscroll_clamps_reverses_and_resizes_cleanly() {
    let mut ts = TestSession::new(super::test_state(".".into()));
    ts.state_mut().console.push_started(GitConsoleEntry {
        id: 1,
        started_at: Local::now(),
        duration_ms: Some(12),
        command_line: "git pull".into(),
        output_lines: (0..120)
            .map(|i| ConsoleLine {
                stream: ConsoleStream::Stdout,
                text: format!("create mode 100644 file_{i}.rs"),
            })
            .collect(),
        exit_code: Some(0),
    });
    ts.dispatch(Action::SwitchTab(WorkspaceTab::Console));

    let _ = ts.render(120, 30);
    let max_scroll = ts
        .state()
        .console
        .output_viewport()
        .max_scroll()
        .expect("console bound recorded");
    assert!(max_scroll > 0);

    for _ in 0..100 {
        ts.dispatch(Action::ScrollConsoleOutput(3));
    }
    assert_eq!(ts.state().console.output_scroll(), max_scroll);

    ts.dispatch(Action::ScrollConsoleOutput(-3));
    assert_eq!(ts.state().console.output_scroll(), max_scroll.saturating_sub(3));

    ts.dispatch(Action::ScrollConsoleOutput(isize::MAX / 2));
    assert_eq!(ts.state().console.output_scroll(), max_scroll);
    let _ = ts.render(120, 50);
    let taller_max_scroll = ts
        .state()
        .console
        .output_viewport()
        .max_scroll()
        .expect("taller bound recorded");
    assert!(taller_max_scroll < max_scroll);
    assert_eq!(ts.state().console.output_scroll(), taller_max_scroll);
}

#[test]
fn log_diff_double_goto_bottom_no_overflow_and_reverses_immediately() {
    let oid = super::oid_n(1);
    let snap = (*super::make_snapshot(
        PathBuf::from("."),
        vec![super::make_commit(1, vec![], "feat: sample")],
    ))
    .clone();
    let mut ts = TestSession::with_snapshot(snap);
    ts.advance_by(Duration::from_millis(35));

    let key = jbgit_core::DetailsKey::single(oid);
    ts.deliver_engine_event(EngineEvent::CommitDetailsLoaded {
        slot: jbgit_core::RequestSlotId::LogCommitDetails,
        bundle: Arc::new(jbgit_core::CommitChangesBundle {
            key: key.clone(),
            details: vec![super::make_details(oid, vec![], "feat: sample")],
            changes: vec![FileChange {
                path: "lib.rs".into(),
                old_path: None,
                kind: FileStatusKind::Modified,
                additions: 60,
                deletions: 0,
                is_binary: false,
                exists_in_workdir: true,
                old_oid: None,
                new_oid: None,
            }],
        }),
        epoch: 1,
    });
    ts.dispatch(Action::ToggleDiffPreview);
    ts.advance_by(Duration::from_millis(35));
    let diff_key = FileDiffKey {
        details: key,
        path: "lib.rs".into(),
        old_path: None,
        context: ts.state().log.details.diff.context,
        whitespace: ts.state().log.details.diff.whitespace,
    };
    let lines: Vec<DiffLine> = (0..80)
        .map(|i| DiffLine {
            kind: DiffLineKind::Added,
            old_lineno: None,
            new_lineno: Some(i + 1),
            text: format!("line {i}"),
            word_ranges: Vec::new(),
        })
        .collect();
    ts.deliver_engine_event(EngineEvent::FileDiffLoaded {
        slot: jbgit_core::RequestSlotId::LogFileDiff,
        diff: Arc::new(FileDiff {
            key: diff_key,
            hunks: vec![DiffHunk {
                header: "@@ -0,0 +1,80 @@".into(),
                lines,
            }],
            is_binary: false,
            binary: None,
            is_truncated: false,
            is_too_large: false,
            lfs: None,
            mode_change: None,
        }),
        epoch: 1,
    });
    ts.dispatch(Action::FocusPane(PaneId::LogDetailsDiff));

    let _ = ts.render(120, 30);
    let max_scroll = ts
        .state()
        .log
        .details
        .diff
        .scroll
        .max_scroll()
        .expect("log_diff bound recorded");
    assert!(max_scroll > 0);

    ts.dispatch(Action::DiffScroll(crate::app::DiffScope::Log, isize::MAX / 2));
    ts.dispatch(Action::DiffScroll(crate::app::DiffScope::Log, isize::MAX / 2));
    assert_eq!(ts.state().log.details.diff.scroll.offset(), max_scroll);

    ts.dispatch(Action::DiffScroll(crate::app::DiffScope::Log, -1));
    assert_eq!(
        ts.state().log.details.diff.scroll.offset(),
        max_scroll - 1
    );
}

#[test]
fn console_output_detail_navigation_keys_work() {
    let mut ts = TestSession::new(super::test_state(".".into()));
    ts.state_mut().console.push_started(GitConsoleEntry {
        id: 1,
        started_at: Local::now(),
        duration_ms: Some(10),
        command_line: "git log".into(),
        output_lines: (0..100)
            .map(|i| ConsoleLine {
                stream: ConsoleStream::Stdout,
                text: format!("commit {i}"),
            })
            .collect(),
        exit_code: Some(0),
    });
    ts.dispatch(Action::SwitchTab(WorkspaceTab::Console));
    ts.dispatch(Action::FocusPane(PaneId::ConsoleOutputDetail));

    let _ = ts.render(120, 30);
    let vp = ts.state().console.output_viewport();
    let viewport_h = vp.viewport_height().expect("console viewport recorded");
    let max_scroll = vp.max_scroll().expect("console bound recorded");
    assert!(viewport_h > 10);
    assert!(max_scroll > viewport_h);

    ts.send_key(KeyCode::PageDown, KeyModifiers::NONE);
    let expected_page = viewport_h.saturating_sub(1);
    assert_eq!(ts.state().console.output_scroll(), expected_page);

    ts.send_key(KeyCode::PageUp, KeyModifiers::NONE);
    assert_eq!(ts.state().console.output_scroll(), 0);

    ts.send_key(KeyCode::Char('d'), KeyModifiers::CONTROL);
    let expected_half = (viewport_h / 2).max(1);
    assert_eq!(ts.state().console.output_scroll(), expected_half);
    ts.send_key(KeyCode::Char('u'), KeyModifiers::CONTROL);
    assert_eq!(ts.state().console.output_scroll(), 0);

    ts.send_key(KeyCode::Char('G'), KeyModifiers::SHIFT);
    assert_eq!(ts.state().console.output_scroll(), max_scroll);
    ts.send_key(KeyCode::Char('g'), KeyModifiers::NONE);
    ts.send_key(KeyCode::Char('g'), KeyModifiers::NONE);
    assert_eq!(ts.state().console.output_scroll(), 0);

    ts.send_key(KeyCode::End, KeyModifiers::NONE);
    assert_eq!(ts.state().console.output_scroll(), max_scroll);
    ts.send_key(KeyCode::Home, KeyModifiers::NONE);
    assert_eq!(ts.state().console.output_scroll(), 0);

    let unbind_gb: crate::config::KeymapConfigOverlay =
        toml::from_str("[global]\nprepend_keymap = [{ on = [\"g\", \"b\"], run = \"noop\" }]\n")
            .unwrap();
    ts.state_mut().config.keymap.merge_overlay(&unbind_gb);
    ts.send_key(KeyCode::End, KeyModifiers::NONE);
    assert_eq!(ts.state().console.output_scroll(), max_scroll);
    ts.send_key(KeyCode::Char('g'), KeyModifiers::NONE);
    ts.send_key(KeyCode::Char('g'), KeyModifiers::NONE);
    assert_eq!(ts.state().console.output_scroll(), 0);

    let unbind_gg: crate::config::KeymapConfigOverlay =
        toml::from_str("[console]\nprepend_keymap = [{ on = [\"g\", \"g\"], run = \"noop\" }]\n")
            .unwrap();
    ts.state_mut().config.keymap.merge_overlay(&unbind_gg);
    ts.send_key(KeyCode::End, KeyModifiers::NONE);
    assert_eq!(ts.state().console.output_scroll(), max_scroll);
    ts.send_key(KeyCode::Char('g'), KeyModifiers::NONE);
    ts.send_key(KeyCode::Char('g'), KeyModifiers::NONE);
    assert_eq!(ts.state().console.output_scroll(), max_scroll);
}

#[test]
fn test_split_editor_command_quotes_and_windows_paths() {
    assert_eq!(split_editor_command("nvim"), Some(("nvim".to_string(), vec![])));
    assert_eq!(
        split_editor_command("code --wait --reuse-window"),
        Some((
            "code".to_string(),
            vec!["--wait".to_string(), "--reuse-window".to_string()],
        ))
    );
    assert_eq!(
        split_editor_command("\"C:\\Program Files\\Microsoft VS Code\\Code.exe\" --wait"),
        Some((
            "C:\\Program Files\\Microsoft VS Code\\Code.exe".to_string(),
            vec!["--wait".to_string()],
        ))
    );
    assert_eq!(
        split_editor_command("'/Applications/Sublime Text/subl' -w"),
        Some((
            "/Applications/Sublime Text/subl".to_string(),
            vec!["-w".to_string()],
        ))
    );
    assert_eq!(split_editor_command("   "), None);
    assert_eq!(split_editor_command("\"\""), None);
}

#[test]
fn test_resolve_editor_command_priority() {
    let resolved = resolve_editor_command_with(|k| match k {
        "GIT_EDITOR" => Some("nano -w".to_string()),
        "VISUAL" => Some("code --wait".to_string()),
        "EDITOR" => Some("vi".to_string()),
        _ => None,
    });
    assert_eq!(resolved, ("nano".to_string(), vec!["-w".to_string()]));

    let resolved_visual = resolve_editor_command_with(|k| match k {
        "GIT_EDITOR" => Some("   ".to_string()),
        "VISUAL" => Some("code --wait".to_string()),
        "EDITOR" => Some("vi".to_string()),
        _ => None,
    });
    assert_eq!(
        resolved_visual,
        ("code".to_string(), vec!["--wait".to_string()])
    );

    let fallback = resolve_editor_command_with(|_| None);
    let expected_bin = if cfg!(windows) { "notepad" } else { "vi" };
    assert_eq!(fallback, (expected_bin.to_string(), vec![]));
}

#[test]
fn test_handle_external_editor_effects_noop_and_disabled() {
    assert!(is_external_editor_disabled_val(Some("1")));
    assert!(is_external_editor_disabled_val(Some(" TRUE ")));
    assert!(is_external_editor_disabled_val(Some("yes")));
    assert!(!is_external_editor_disabled_val(Some("0")));
    assert!(!is_external_editor_disabled_val(None));

    let mut state = AppState::new_with_config(
        PathBuf::from("."),
        None,
        crate::config::AppConfig::load_defaults(),
    );
    let mut term_guard = None;
    let expected_disabled = state.i18n.t("toast.external_editor_disabled").to_string();

    let refresh = handle_external_editor_effects_with(
        &[Effect::OpenExternalEditorMessage],
        &mut state,
        &mut term_guard,
        true,
        || (":".to_string(), vec![]),
    );
    assert!(!refresh);
    assert_eq!(state.status_notice.as_deref(), Some(expected_disabled.as_str()));

    state.status_notice = None;
    let refresh = handle_external_editor_effects_with(
        &[Effect::OpenExternalEditorFile("README.md".to_string())],
        &mut state,
        &mut term_guard,
        true,
        || (":".to_string(), vec![]),
    );
    assert!(!refresh);
    assert_eq!(state.status_notice.as_deref(), Some(expected_disabled.as_str()));

    state.status_notice = None;
    state.stage_workbench.editor_mut_for_test().set_text("feat: keep message");
    let refresh = handle_external_editor_effects_with(
        &[
            Effect::OpenExternalEditorMessage,
            Effect::OpenExternalEditorFile("README.md".to_string()),
        ],
        &mut state,
        &mut term_guard,
        false,
        || (":".to_string(), vec![]),
    );
    assert!(refresh);
    assert_eq!(state.status_notice, None);
    assert_eq!(state.stage_workbench.editor().text(), "feat: keep message");
}

#[test]
fn commit_tab_diff_lru_cache_invalidation_debounce_and_effect_coalescing() {
    let mut ts = TestSession::new(super::test_state(".".into()));
    ts.dispatch(Action::SwitchTab(WorkspaceTab::Commit));
    ts.dispatch(Action::StageToggleGroupByDir);

    let status = Arc::new(StageStatusSnapshot {
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
    });
    let _ = ts.take_requests();
    ts.deliver_engine_event(EngineEvent::StageStatusLoaded { status, epoch: 1 });
    let reqs = ts.take_requests();
    assert!(reqs.iter().any(|(r, _)| matches!(
        r,
        TaskRequest::LoadStageFileDiff { key } if key.path == "a.rs"
    )));

    let make_stage_diff = |path: &str| {
        let vcs = b"fn x() {\n    let a = 1;\n}\n";
        let staged = b"fn x() {\n    let a = 1;\n}\n";
        let local = b"fn x() {\n    let a = 2;\n}\n";
        let tracker = jbgit_core::StageLineTracker::new(vcs, staged, local);
        let (hunks, is_truncated) = tracker.build_interactive_hunks(false, DiffContext::Three, 20_000);
        Arc::new(ThreeDocStageDiff {
            key: StageDiffKey {
                path: path.into(),
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
        })
    };

    ts.dispatch(Action::SelectCommitStageRow(1));
    let _ = ts.take_requests();
    for step in 0..20 {
        let delta = if step % 2 == 0 { 1 } else { -1 };
        ts.dispatch(Action::ScrollCommitStage(delta));
        assert!(
            ts.take_requests().is_empty(),
            "keyboard scroll must debounce stage diff request"
        );
    }
    ts.dispatch(Action::ScrollCommitStage(1));
    assert!(ts.take_requests().is_empty());
    assert!(ts.state().stage_workbench.has_pending_diff_for_test());

    ts.advance_by(crate::log_view::DETAILS_DEBOUNCE + Duration::from_millis(5));
    let flushed = ts.take_requests();
    assert!(matches!(
        &flushed[..],
        [(TaskRequest::LoadStageFileDiff { key }, _)] if key.path == "b.rs"
    ));

    ts.dispatch(Action::SelectCommitStageRow(1));
    let epoch_a = ts
        .state()
        .request_slots
        .latest(jbgit_core::RequestSlotId::StageFileDiff);
    ts.deliver_engine_event(EngineEvent::StageFileDiffLoaded {
        diff: make_stage_diff("a.rs"),
        epoch: epoch_a,
    });
    ts.dispatch(Action::SelectCommitStageRow(2));
    let epoch_b = ts
        .state()
        .request_slots
        .latest(jbgit_core::RequestSlotId::StageFileDiff);
    ts.deliver_engine_event(EngineEvent::StageFileDiffLoaded {
        diff: make_stage_diff("b.rs"),
        epoch: epoch_b,
    });

    let _ = ts.take_requests();
    ts.dispatch(Action::SelectCommitStageRow(1));
    assert!(!ts.take_requests().iter().any(|(r, _)| matches!(
        r,
        TaskRequest::LoadStageFileDiff { .. }
    )));
    assert_eq!(
        ts.state()
            .stage_workbench
            .current_diff()
            .map(|d| d.key.path.as_str()),
        Some("a.rs")
    );

    ts.state_mut().stage_workbench.set_diff_cursor_for_test(1);
    ts.dispatch(Action::StageDiffStageSelection);
    let key_a = StageDiffKey {
        path: "a.rs".into(),
        old_path: None,
        side: StageDiffSide::Unstaged,
        context: DiffContext::Three,
        whitespace: jbgit_core::WhitespaceMode::Default,
    };
    let key_b = StageDiffKey {
        path: "b.rs".into(),
        old_path: None,
        side: StageDiffSide::Unstaged,
        context: DiffContext::Three,
        whitespace: jbgit_core::WhitespaceMode::Default,
    };
    assert!(!ts.state().stage_workbench.diff_cache_contains(&key_a));
    assert!(ts.state().stage_workbench.diff_cache_contains(&key_b));

    let mut raw = Vec::new();
    for i in 0..10 {
        raw.push(Effect::RequestStageFileDiff(StageDiffKey {
            path: format!("f{i}.rs"),
            old_path: None,
            side: StageDiffSide::Unstaged,
            context: DiffContext::Three,
            whitespace: jbgit_core::WhitespaceMode::Default,
        }));
    }
    raw.push(Effect::CopyToClipboard("keep-me".into()));
    let coalesced = crate::coalesce_effects(raw);
    assert_eq!(coalesced.len(), 2);
    assert!(matches!(
        &coalesced[0],
        Effect::RequestStageFileDiff(k) if k.path == "f9.rs"
    ));
    assert!(matches!(
        &coalesced[1],
        Effect::CopyToClipboard(s) if s == "keep-me"
    ));

    ts.dispatch(Action::SelectCommitStageRow(2));
    assert!(ts.state().stage_workbench.diff_cache_contains(&key_b));
    let same_status = Arc::clone(ts.state().stage_workbench.status().unwrap());
    let _ = ts.take_requests();
    ts.deliver_engine_event(EngineEvent::StageStatusLoaded {
        status: Arc::new((*same_status).clone()),
        epoch: 4,
    });
    assert!(
        !ts.state().stage_workbench.diff_cache_contains(&key_b),
        "identical StageStatusLoaded must still invalidate cached diffs after external edits"
    );
    let reload_reqs = ts.take_requests();
    assert!(reload_reqs.iter().any(|(r, _)| matches!(
        r,
        TaskRequest::LoadStageFileDiff { key } if key.path == "b.rs"
    )));
}

#[tokio::test]
async fn coalescing_event_stream_does_not_poison_single_flight_waker() {
    struct SharedQueue {
        events: Mutex<VecDeque<Event>>,
        available: Condvar,
        shutdown: AtomicBool,
    }

    struct SingleFlightEventStream {
        queue: Arc<SharedQueue>,
        stream_wake_task_executed: Arc<AtomicBool>,
        task_tx: mpsc::SyncSender<Waker>,
    }

    impl SingleFlightEventStream {
        fn new() -> (Self, Arc<SharedQueue>) {
            let queue = Arc::new(SharedQueue {
                events: Mutex::new(VecDeque::new()),
                available: Condvar::new(),
                shutdown: AtomicBool::new(false),
            });
            let executed = Arc::new(AtomicBool::new(false));
            let (task_tx, task_rx) = mpsc::sync_channel::<Waker>(1);
            let q_bg = Arc::clone(&queue);
            let exec_bg = Arc::clone(&executed);
            std::thread::spawn(move || {
                while let Ok(waker) = task_rx.recv() {
                    {
                        let mut guard = q_bg.events.lock().unwrap();
                        while guard.is_empty() && !q_bg.shutdown.load(Ordering::SeqCst) {
                            guard = q_bg.available.wait(guard).unwrap();
                        }
                    }
                    exec_bg.store(false, Ordering::SeqCst);
                    waker.wake();
                }
            });
            (
                Self {
                    queue: Arc::clone(&queue),
                    stream_wake_task_executed: executed,
                    task_tx,
                },
                queue,
            )
        }
    }

    impl Drop for SingleFlightEventStream {
        fn drop(&mut self) {
            self.queue.shutdown.store(true, Ordering::SeqCst);
            self.queue.available.notify_all();
        }
    }

    impl futures::Stream for SingleFlightEventStream {
        type Item = std::io::Result<Event>;

        fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
            if let Some(ev) = self.queue.events.lock().unwrap().pop_front() {
                return Poll::Ready(Some(Ok(ev)));
            }
            if !self
                .stream_wake_task_executed
                .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                .unwrap_or_else(|x| x)
            {
                let _ = self.task_tx.send(cx.waker().clone());
            }
            Poll::Pending
        }
    }

    let push_key = |q: &Arc<SharedQueue>, ch: char| {
        q.events
            .lock()
            .unwrap()
            .push_back(Event::Key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE)));
        q.available.notify_all();
    };

    let (mut stream, queue) = SingleFlightEventStream::new();

    push_key(&queue, 'j');
    push_key(&queue, 'k');

    let q_later = Arc::clone(&queue);
    let loop_task = tokio::spawn(async move {
        let mut received = Vec::new();
        tokio::select! {
            Some(Ok(Event::Key(k))) = stream.next() => received.push(k.code),
        }
        for _ in 1..crate::MAX_COALESCE_EVENTS {
            match crate::poll_stream_ready(&mut stream).await {
                Some(Some(Ok(Event::Key(k)))) => received.push(k.code),
                Some(_) => break,
                None => break,
            }
        }
        assert_eq!(received, vec![KeyCode::Char('j'), KeyCode::Char('k')]);

        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(15));
            q_later.events.lock().unwrap().push_back(Event::Key(KeyEvent::new(
                KeyCode::Char('q'),
                KeyModifiers::NONE,
            )));
            q_later.available.notify_all();
        });

        stream.next().await
    });

    let next_ev = tokio::time::timeout(Duration::from_millis(300), loop_task)
        .await
        .expect("EventStream must not hang after non-blocking coalescing poll")
        .unwrap();
    assert!(matches!(
        next_ev,
        Some(Ok(Event::Key(k))) if k.code == KeyCode::Char('q')
    ));
}

#[test]
fn cross_consumer_request_slots_coalesce_independently_and_filter_stale_events() {
    use jbgit_core::{
        CommitChangesBundle, ConflictDocument, DetailsKey, DiffContext, FileDiff, FileDiffKey,
        RequestSlotId,
    };

    let mut ts = TestSession::new(super::test_state(".".into()));

    let key_log = FileDiffKey {
        details: DetailsKey::single(Oid::from_bytes([1; 20])),
        path: "log.rs".into(),
        old_path: None,
        context: DiffContext::Three,
        whitespace: jbgit_core::WhitespaceMode::Default,
    };
    let key_fh = FileDiffKey {
        details: DetailsKey::single(Oid::from_bytes([2; 20])),
        path: "fh.rs".into(),
        old_path: None,
        context: DiffContext::Three,
        whitespace: jbgit_core::WhitespaceMode::Default,
    };
    let key_stash = FileDiffKey {
        details: DetailsKey::single(Oid::from_bytes([3; 20])),
        path: "stash.rs".into(),
        old_path: None,
        context: DiffContext::Three,
        whitespace: jbgit_core::WhitespaceMode::Default,
    };

    // Coalescing must preserve sibling consumer requests while collapsing duplicates within one slot.
    let raw = vec![
        Effect::RequestFileDiff {
            slot: RequestSlotId::LogFileDiff,
            key: key_log.clone(),
        },
        Effect::RequestFileDiff {
            slot: RequestSlotId::FileHistoryDiff,
            key: key_fh.clone(),
        },
        Effect::RequestFileDiff {
            slot: RequestSlotId::StashFileDiff,
            key: key_stash.clone(),
        },
        Effect::RequestAmendPreview,
        Effect::LoadConflictDocument("c.rs".into()),
    ];
    let coalesced = crate::coalesce_effects(raw);
    assert_eq!(coalesced.len(), 5, "sibling slots must not collapse each other");

    ts.execute_effects(coalesced);
    assert_eq!(ts.engine.recorded_len(), 5);
    assert_eq!(ts.state().request_slots.latest(RequestSlotId::LogFileDiff), 1);
    assert_eq!(
        ts.state().request_slots.latest(RequestSlotId::FileHistoryDiff),
        1
    );
    assert_eq!(ts.state().request_slots.latest(RequestSlotId::StashFileDiff), 1);
    assert_eq!(ts.state().request_slots.latest(RequestSlotId::AmendPreview), 1);
    assert_eq!(
        ts.state().request_slots.latest(RequestSlotId::ConflictDocument),
        1
    );

    // Advance AmendPreview, ConflictDocument, and LogCommitDetails epochs to 2.
    ts.execute_effects(vec![
        Effect::RequestAmendPreview,
        Effect::LoadConflictDocument("c.rs".into()),
        Effect::RequestCommitDetails {
            slot: RequestSlotId::LogCommitDetails,
            key: DetailsKey::single(Oid::from_bytes([9; 20])),
        },
        Effect::RequestCommitDetails {
            slot: RequestSlotId::LogCommitDetails,
            key: DetailsKey::single(Oid::from_bytes([10; 20])),
        },
    ]);

    // Stale CommitDetailsLoaded (epoch 1 < 2) must be ignored.
    let stale_bundle_key = DetailsKey::single(Oid::from_bytes([9; 20]));
    ts.deliver_engine_event(EngineEvent::CommitDetailsLoaded {
        slot: RequestSlotId::LogCommitDetails,
        bundle: Arc::new(CommitChangesBundle {
            key: stale_bundle_key.clone(),
            details: vec![],
            changes: vec![],
        }),
        epoch: 1,
    });
    assert!(
        ts.state().diff_caches.details.peek(&stale_bundle_key).is_none(),
        "stale CommitDetailsLoaded must be dropped"
    );

    // Stale AmendPreviewLoaded (epoch 1 < 2) must be ignored.
    ts.deliver_engine_event(EngineEvent::AmendPreviewLoaded {
        preview: Some(AmendCommitPreview {
            short_sha: "old0000".into(),
            message: "stale amend".into(),
            files: vec![],
        }),
        epoch: 1,
    });
    assert!(
        ts.state().stage_workbench.amend_preview().is_none(),
        "stale AmendPreviewLoaded must be dropped"
    );

    // Stale ConflictDocumentLoaded (epoch 1 < 2) must be ignored.
    ts.dispatch(Action::OpenConflictResolver("c.rs".into()));
    let stale_doc = ConflictDocument::parse("c.rs", "stale\n", None, None);
    ts.deliver_engine_event(EngineEvent::ConflictDocumentLoaded {
        doc: stale_doc,
        epoch: 1,
    });
    assert!(
        ts.state()
            .modal_stack
            .conflict_merge()
            .unwrap()
            .doc
            .is_none(),
        "stale ConflictDocumentLoaded must be dropped"
    );

    // Meanwhile LogFileDiff epoch 1 is still current and lands cleanly.
    ts.state_mut().log.details.diff.requested = Some(key_log.clone());
    ts.deliver_engine_event(EngineEvent::FileDiffLoaded {
        slot: RequestSlotId::LogFileDiff,
        diff: Arc::new(FileDiff {
            key: key_log.clone(),
            hunks: vec![],
            is_binary: false,
            binary: None,
            is_truncated: false,
            is_too_large: false,
            lfs: None,
            mode_change: None,
        }),
        epoch: 1,
    });
    assert!(ts.state().log.details.diff.requested.is_none());
    assert!(ts.state().diff_caches.diffs.peek(&key_log).is_some());
}

#[test]
fn stale_diff_responses_for_same_key_never_overwrite_fresh_state_or_cache() {
    use jbgit_core::{DetailsKey, RequestSlotId};

    let mut ts = TestSession::new(super::test_state(".".into()));
    ts.dispatch(Action::SwitchTab(WorkspaceTab::Commit));
    ts.deliver_engine_event(EngineEvent::StageStatusLoaded {
        status: Arc::new(StageStatusSnapshot {
            unstaged: vec![StageEntry {
                path: "src/foo.rs".into(),
                old_path: None,
                kind: FileStatusKind::Modified,
            }],
            ..Default::default()
        }),
        epoch: 1,
    });
    // First StageFileDiff request -> epoch 1, then mutation triggers epoch 2 for the SAME key!
    ts.dispatch(Action::SelectCommitStageRow(1));
    let _ = ts.take_requests();
    ts.state_mut().stage_workbench.on_git_op_succeeded();
    ts.dispatch(Action::SelectCommitStageRow(1));
    let reqs = ts.take_requests();
    assert_eq!(ts.state().request_slots.latest(RequestSlotId::StageFileDiff), 2, "{reqs:?}");

    let make_stage_diff = |content: &[u8]| {
        let tracker = jbgit_core::StageLineTracker::new(b"", b"", content);
        let (hunks, is_truncated) = tracker.build_interactive_hunks(false, DiffContext::Three, 20_000);
        Arc::new(ThreeDocStageDiff {
            key: StageDiffKey {
                path: "src/foo.rs".into(),
                old_path: None,
                side: StageDiffSide::Unstaged,
                context: DiffContext::Three,
                whitespace: jbgit_core::WhitespaceMode::Default,
            },
            vcs_exists: false,
            staged_exists: false,
            local_exists: true,
            vcs_bytes: Vec::new(),
            staged_bytes: Vec::new(),
            local_bytes: content.to_vec(),
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
        })
    };

    // Fresh epoch 2 arrives first, then stale epoch 1 arrives for the EXACT SAME StageDiffKey.
    ts.deliver_engine_event(EngineEvent::StageFileDiffLoaded {
        diff: make_stage_diff(b"fresh\n"),
        epoch: 2,
    });
    ts.deliver_engine_event(EngineEvent::StageFileDiffLoaded {
        diff: make_stage_diff(b"stale\n"),
        epoch: 1,
    });
    assert_eq!(
        ts.state()
            .stage_workbench
            .current_diff()
            .map(|d| d.local_bytes.as_slice()),
        Some(b"fresh\n".as_slice()),
        "stale epoch 1 StageFileDiffLoaded must not overwrite fresh epoch 2 state"
    );

    // Same invariant for CompareWithLocal FileDiffLoaded: epoch 2 must not be overwritten by epoch 1.
    let target = Oid::from_bytes([0x44; 20]);
    let cmp_key = FileDiffKey {
        details: DetailsKey::compare_with_local(target),
        path: "src/foo.rs".into(),
        old_path: None,
        context: DiffContext::Three,
        whitespace: jbgit_core::WhitespaceMode::Default,
    };
    ts.state_mut()
        .request_slots
        .note_dispatched(RequestSlotId::LogFileDiff, 2);
    ts.deliver_engine_event(EngineEvent::FileDiffLoaded {
        slot: RequestSlotId::LogFileDiff,
        diff: Arc::new(FileDiff {
            key: cmp_key.clone(),
            hunks: vec![],
            is_binary: false,
            binary: None,
            is_truncated: false,
            is_too_large: false,
            lfs: None,
            mode_change: None,
        }),
        epoch: 2,
    });
    ts.deliver_engine_event(EngineEvent::FileDiffLoaded {
        slot: RequestSlotId::LogFileDiff,
        diff: Arc::new(FileDiff {
            key: cmp_key.clone(),
            hunks: vec![DiffHunk {
                header: "@@ stale @@".into(),
                lines: Vec::new(),
            }],
            is_binary: false,
            binary: None,
            is_truncated: false,
            is_too_large: false,
            lfs: None,
            mode_change: None,
        }),
        epoch: 1,
    });
    assert!(
        ts.state()
            .diff_caches
            .peek_diff(&cmp_key)
            .unwrap()
            .hunks
            .is_empty(),
        "stale epoch 1 FileDiffLoaded must not overwrite fresh epoch 2 cache"
    );
}

#[test]
fn conflict_resolver_scroll_clamps_without_dead_zone_and_commit_editor_follows_cursor() {
    use jbgit_core::{
        ConflictDocument, DetailsKey, DiffContext, FileChange, FileDiff, FileDiffKey,
        FileStatusKind, RepoSnapshot, RepositoryState, RequestSlotId, StashEntry,
    };

    let mut ts = TestSession::new(super::test_state(".".into()));

    // 1. 3-Way Conflict Resolver: repeated PageDown clamps at max_scroll; single Up moves up 1 immediately.
    ts.dispatch(Action::OpenConflictResolver("src/conflict.rs".into()));
    let mut raw = String::new();
    for i in 1..=60 {
        raw.push_str(&format!("context line {i}\n"));
    }
    raw.push_str("<<<<<<< HEAD\nours\n=======\ntheirs\n>>>>>>> feat\n");
    let doc = ConflictDocument::parse("src/conflict.rs", &raw, None, None);
    ts.deliver_engine_event(EngineEvent::ConflictDocumentLoaded { doc, epoch: 1 });

    let _ = ts.render(120, 24);
    let max_scroll = ts
        .state()
        .modal_stack
        .conflict_merge()
        .unwrap()
        .scroll
        .max_scroll()
        .expect("conflict_merge bound recorded");
    assert!(max_scroll > 0);

    for _ in 0..20 {
        ts.dispatch(Action::ConflictScroll(10));
    }
    assert_eq!(
        ts.state().modal_stack.conflict_merge().unwrap().scroll.offset(),
        max_scroll,
        "ConflictScroll must clamp at bottom without overscroll"
    );
    ts.dispatch(Action::ConflictScroll(-1));
    assert_eq!(
        ts.state().modal_stack.conflict_merge().unwrap().scroll.offset(),
        max_scroll - 1,
        "Single Up after bottom must immediately scroll up 1 line with zero dead-zone"
    );
    ts.dispatch(Action::PopModal);

    // 2. CommitMessageEditor: typing 15 lines in a 5-line viewport keeps cursor inside visible window.
    let ed = ts.state_mut().stage_workbench.editor_mut_for_test();
    ed.bind_viewport(40, 5);
    for i in 1..=15 {
        ed.insert_paste(&format!("line {i}"));
        if i < 15 {
            ed.insert_newline();
        }
        assert!(
            ed.cursor_line >= ed.scroll_row && ed.cursor_line < ed.scroll_row + 5,
            "cursor {} must remain inside [{}..{}) while typing",
            ed.cursor_line,
            ed.scroll_row,
            ed.scroll_row + 5
        );
    }
    assert_eq!(ed.scroll_row, 10);
    for _ in 0..14 {
        ed.move_up();
        assert!(
            ed.cursor_line >= ed.scroll_row && ed.cursor_line < ed.scroll_row + 5,
            "cursor {} must remain inside [{}..{}) while moving up",
            ed.cursor_line,
            ed.scroll_row,
            ed.scroll_row + 5
        );
    }
    assert_eq!(ed.scroll_row, 0);

    // 3. Single global DiffCacheStore: a details+diff loaded in Log is reused with 0 requests in Stash.
    let stash_oid = Oid::from_bytes([0xAB; 20]);
    let details_key = DetailsKey::single(stash_oid);
    let diff_key = FileDiffKey {
        details: details_key.clone(),
        path: "src/shared.rs".into(),
        old_path: None,
        context: DiffContext::Three,
        whitespace: jbgit_core::WhitespaceMode::Default,
    };
    ts.deliver_engine_event(EngineEvent::RepoSnapshotLoaded {
        snapshot: Arc::new(RepoSnapshot {
            repo_name: "demo".into(),
            workdir: ".".into(),
            git_dir: ".git".into(),
            common_dir: ".git".into(),
            state: RepositoryState::Clean,
            is_unborn_head: false,
            user_name: None,
            user_email: None,
            recent_branches: vec![],
            stashes: vec![StashEntry {
                index: 0,
                oid: stash_oid,
                message: "On main: cached stash".into(),
                timestamp: 0,
            }],
            ongoing_onto: None,
            bisect: None,
            remotes: Vec::new(),
            worktrees: Vec::new(),
            submodules: Vec::new(),
            has_lfs: false,
        is_shallow: false,
        loaded_limit: 0,
        has_more_commits: false,
            head_branch: Some("main".into()),
            head_oid: Some(stash_oid),
            refs: vec![],
            commits: vec![],
        }),
        epoch: 1,
    });
    ts.deliver_engine_event(EngineEvent::CommitDetailsLoaded {
        slot: RequestSlotId::LogCommitDetails,
        bundle: Arc::new(jbgit_core::CommitChangesBundle {
            key: details_key,
            details: vec![],
            changes: vec![FileChange {
                path: "src/shared.rs".into(),
                old_path: None,
                kind: FileStatusKind::Modified,
                additions: 1,
                deletions: 0,
                is_binary: false,
                exists_in_workdir: true,
                old_oid: None,
                new_oid: None,
            }],
        }),
        epoch: 1,
    });
    ts.deliver_engine_event(EngineEvent::FileDiffLoaded {
        slot: RequestSlotId::LogFileDiff,
        diff: Arc::new(FileDiff {
            key: diff_key,
            hunks: vec![],
            is_binary: false,
            binary: None,
            is_truncated: false,
            is_too_large: false,
            lfs: None,
            mode_change: None,
        }),
        epoch: 1,
    });
    let _ = ts.take_requests();
    ts.dispatch(Action::SwitchTab(WorkspaceTab::Stash));
    let reqs = ts.take_requests();
    assert!(
        !reqs.iter().any(|(r, _)| matches!(
            r,
            TaskRequest::LoadCommitDetails { .. } | TaskRequest::LoadFileDiff { .. }
        )),
        "Stash must reuse shared DiffCacheStore with zero redundant requests: {reqs:?}"
    );
}

#[test]
fn set_focus_blurs_inline_modes_and_log_navigation_clears_compare_override() {
    use jbgit_core::{CommitSummary, DetailsKey, RepoSnapshot, RepositoryState};

    let c1 = Oid::from_bytes([1u8; 20]);
    let c2 = Oid::from_bytes([2u8; 20]);
    let mut ts = TestSession::with_snapshot(RepoSnapshot {
        repo_name: "demo".into(),
        workdir: ".".into(),
        git_dir: ".git".into(),
        common_dir: ".git".into(),
        state: RepositoryState::Clean,
        is_unborn_head: false,
        user_name: None,
        user_email: None,
        recent_branches: vec![],
        stashes: vec![],
        ongoing_onto: None,
        bisect: None,
        remotes: Vec::new(),
        worktrees: Vec::new(),
        submodules: Vec::new(),
        has_lfs: false,
        is_shallow: false,
        loaded_limit: 0,
        has_more_commits: false,
        head_branch: Some("main".into()),
        head_oid: Some(c2),
        refs: vec![],
        commits: vec![
            CommitSummary {
                oid: c2,
                parents: vec![c1],
                author_name: "A".into(),
                author_email: "a@b".into(),
                timestamp: 2,
                author_timestamp: 2,
                subject: "second".into(),
                body: String::new(),
            },
            CommitSummary {
                oid: c1,
                parents: vec![],
                author_name: "A".into(),
                author_email: "a@b".into(),
                timestamp: 1,
                author_timestamp: 1,
                subject: "first".into(),
                body: String::new(),
            },
        ],
    });

    // Leaving pane/tab automatically blurs inline edit states and zoomed diff.
    ts.state_mut().log.filter.editing_text = true;
    ts.state_mut().log.branches.searching = true;
    ts.dispatch(Action::CommitFocusAuthorInput);
    ts.state_mut().stage_workbench.select_diff_line(3, true);
    ts.state_mut().log.details.diff.zoomed = true;

    ts.dispatch(Action::SwitchTab(WorkspaceTab::Commit));
    assert!(!ts.state().log.filter.editing_text);
    assert!(!ts.state().log.branches.searching);
    assert!(!ts.state().stage_workbench.editing_author());
    assert!(ts.state().stage_workbench.visual_anchor_row().is_none());
    assert!(!ts.state().log.details.diff.zoomed);

    // Opening a modal blurs inline editors cleanly too.
    ts.dispatch(Action::SwitchTab(WorkspaceTab::Log));
    ts.dispatch(Action::FocusFilterText);
    assert!(ts.state().log.filter.editing_text);
    ts.dispatch(Action::OpenBranchesPopup {
        query: String::new(),
    });
    assert!(!ts.state().log.filter.editing_text);
    ts.dispatch(Action::PopModal);

    // Unified commit-navigation exit clears compare_override & pending_select_path.
    ts.state_mut().log.details.compare_override = Some(DetailsKey::compare_with_local(c2));
    ts.state_mut().log.details.pending_select_path = Some("src/main.rs".into());
    ts.dispatch(Action::ScrollLog(1));
    assert!(ts.state().log.details.compare_override.is_none());
    assert!(ts.state().log.details.pending_select_path.is_none());
}

#[test]
fn conflict_resolver_scroll_follows_selected_block_and_handles_rebase_delete_special_and_staleness() {
    use super::screen_text;
    use jbgit_core::{
        ConflictDocument, ConflictParseOptions, ConflictSideInfo, ConflictSidePresence, GitOp, Oid,
        SpecialConflictInfo, SpecialConflictKind, StageStatusSnapshot,
    };

    let mut ts = TestSession::new(super::test_state(".".into()));

    // 1. 120+ line file with 3 conflict blocks: pressing n/p keeps active block's first row visible.
    ts.dispatch(Action::OpenConflictResolver("src/big.rs".into()));
    let mut raw = String::new();
    for i in 1..=5 {
        raw.push_str(&format!("top {i}\n"));
    }
    raw.push_str("<<<<<<< HEAD\nb0-ours\n=======\nb0-theirs\n>>>>>>> feat\n");
    for i in 1..=55 {
        raw.push_str(&format!("mid1 {i}\n"));
    }
    raw.push_str("<<<<<<< HEAD\nb1-ours\n=======\nb1-theirs\n>>>>>>> feat\n");
    for i in 1..=55 {
        raw.push_str(&format!("mid2 {i}\n"));
    }
    raw.push_str("<<<<<<< HEAD\nb2-ours\n=======\nb2-theirs\n>>>>>>> feat\n");

    let doc = ConflictDocument::parse("src/big.rs", &raw, None, None);
    let b0_row = doc.block_first_row(0).unwrap().0;
    let b1_row = doc.block_first_row(1).unwrap().0;
    let b2_row = doc.block_first_row(2).unwrap().0;
    assert!(b2_row > 100);
    ts.deliver_engine_event(EngineEvent::ConflictDocumentLoaded { doc, epoch: 1 });
    let _ = ts.render(120, 24);
    let vh = ts
        .state()
        .modal_stack
        .conflict_merge()
        .unwrap()
        .scroll
        .viewport_height()
        .expect("viewport height bound");

    // Press `n` twice to jump from block 0 -> block 1 -> block 2.
    ts.send_key(KeyCode::Char('n'), KeyModifiers::NONE);
    let off1 = ts.state().modal_stack.conflict_merge().unwrap().scroll.offset();
    assert!(
        b1_row >= off1 && b1_row < off1 + vh,
        "block 1 row {b1_row} must be inside visible window [{off1}, {})",
        off1 + vh
    );
    ts.send_key(KeyCode::Char('n'), KeyModifiers::NONE);
    let off2 = ts.state().modal_stack.conflict_merge().unwrap().scroll.offset();
    assert!(
        b2_row >= off2 && b2_row < off2 + vh,
        "block 2 row {b2_row} must be inside visible window [{off2}, {})",
        off2 + vh
    );
    // Press `p` twice to jump back to block 1 -> block 0.
    ts.send_key(KeyCode::Char('p'), KeyModifiers::NONE);
    let off1b = ts.state().modal_stack.conflict_merge().unwrap().scroll.offset();
    assert!(b1_row >= off1b && b1_row < off1b + vh);
    ts.send_key(KeyCode::Char('p'), KeyModifiers::NONE);
    let off0 = ts.state().modal_stack.conflict_merge().unwrap().scroll.offset();
    assert!(b0_row >= off0 && b0_row < off0 + vh);

    // 2. Background StageStatusLoaded without `src/big.rs` in conflicted marks modal stale + warns.
    ts.deliver_engine_event(EngineEvent::StageStatusLoaded {
        status: Arc::new(StageStatusSnapshot::default()),
        epoch: 1,
    });
    assert!(
        ts.state()
            .modal_stack
            .conflict_merge()
            .unwrap()
            .externally_resolved
    );
    assert!(
        ts.state()
            .status_notice
            .as_deref()
            .is_some_and(|n| n.contains("externally")),
        "{:?}",
        ts.state().status_notice
    );
    let stale_txt = screen_text(&ts.render(120, 24));
    assert!(stale_txt.contains("resolved externally"), "{stale_txt}");
    ts.dispatch(Action::PopModal);

    // 3. Rebase reversed conflict shows `Yours (replayed: ...)` and `Theirs (onto: ...)`.
    ts.dispatch(Action::OpenConflictResolver("src/rb.rs".into()));
    let rb_doc = ConflictDocument::parse_with_options(
        "src/rb.rs",
        "<<<<<<< HEAD\nonto\n=======\nreplayed\n>>>>>>> feat\n",
        None,
        None,
        ConflictParseOptions {
            is_reversed: true,
            side_presence: ConflictSidePresence::BothPresent,
            default_ours_label: Some("feat"),
            default_theirs_label: Some("main"),
        },
    );
    ts.deliver_engine_event(EngineEvent::ConflictDocumentLoaded {
        doc: rb_doc,
        epoch: 2,
    });
    let rb_txt = screen_text(&ts.render(140, 30));
    assert!(rb_txt.contains("Yours (replayed: feat)"), "{rb_txt}");
    assert!(rb_txt.contains("Theirs (onto: main)"), "{rb_txt}");
    ts.dispatch(Action::PopModal);

    // 4. Delete/Modify conflict offers Keep/Delete buttons and emits SaveConflictResolution { delete: true }.
    ts.dispatch(Action::OpenConflictResolver("gone.txt".into()));
    let dm_doc = ConflictDocument::parse_with_options(
        "gone.txt",
        "local\n",
        Some("local\n"),
        None,
        ConflictParseOptions {
            is_reversed: false,
            side_presence: ConflictSidePresence::DeletedByTheirs,
            ..Default::default()
        },
    );
    ts.deliver_engine_event(EngineEvent::ConflictDocumentLoaded {
        doc: dm_doc,
        epoch: 3,
    });
    let dm_txt = screen_text(&ts.render(140, 30));
    assert!(dm_txt.contains("Keep Yours"), "{dm_txt}");
    assert!(dm_txt.contains("Delete File"), "{dm_txt}");
    let _ = ts.take_requests();
    ts.send_key(KeyCode::Char('t'), KeyModifiers::NONE);
    let dm_del_txt = screen_text(&ts.render(140, 30));
    assert!(dm_del_txt.contains("Delete & Stage"), "{dm_del_txt}");
    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);
    let reqs = ts.take_requests();
    assert!(reqs.iter().any(|(r, _)| matches!(
        r,
        TaskRequest::SaveConflictResolution {
            path,
            stage: true,
            delete: true,
            ..
        } if path == "gone.txt"
    )));

    // 5. SpecialConflictLoaded (Binary) closes text modal and opens dedicated ChoiceDialog.
    ts.dispatch(Action::OpenConflictResolver("assets/logo.png".into()));
    ts.deliver_engine_event(EngineEvent::SpecialConflictLoaded {
        info: SpecialConflictInfo {
            path: "assets/logo.png".into(),
            kind: SpecialConflictKind::Binary,
            is_reversed: false,
            ours: Some(ConflictSideInfo {
                oid: Oid::from_bytes([0x11; 20]),
                size: Some(2048),
                mode: 0o100644,
            }),
            theirs: Some(ConflictSideInfo {
                oid: Oid::from_bytes([0x22; 20]),
                size: Some(4096),
                mode: 0o100644,
            }),
            ours_label: "HEAD".into(),
            theirs_label: "feat".into(),
        },
        epoch: 4,
    });
    assert!(ts.state().modal_stack.conflict_merge().is_none());
    let bin_txt = screen_text(&ts.render(140, 30));
    assert!(bin_txt.contains("Binary Conflict"), "{bin_txt}");
    assert!(bin_txt.contains("1111111"), "{bin_txt}");
    assert!(bin_txt.contains("2.0 KiB"), "{bin_txt}");
    assert!(bin_txt.contains("2222222"), "{bin_txt}");
    assert!(bin_txt.contains("4.0 KiB"), "{bin_txt}");
    assert!(bin_txt.contains("Accept Yours"), "{bin_txt}");
    assert!(bin_txt.contains("Accept Theirs"), "{bin_txt}");
    assert!(bin_txt.contains("Open External Mergetool"), "{bin_txt}");
    let _ = ts.take_requests();
    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);
    let bin_reqs = ts.take_requests();
    assert!(bin_reqs.iter().any(|(r, _)| matches!(
        r,
        TaskRequest::RunGitOp {
            op: GitOp::AcceptSide { paths, ours: true }
        } if paths == &vec!["assets/logo.png".to_string()]
    )));
}

#[test]
fn conflict_queue_three_way_auto_merge_inline_edit_auto_advance_and_continue_prompt() {
    use super::screen_text;
    use jbgit_core::{
        ConflictChoice, ConflictDocument, ConflictParseOptions, FileStatusKind, GitOp,
        GitOpOutcome, OngoingKind, OngoingStep, RepoSnapshot, RepositoryState, StageEntry,
        StageStatusSnapshot,
    };

    let mut ts = TestSession::new(super::test_state(".".into()));
    ts.deliver_engine_event(EngineEvent::RepoSnapshotLoaded {
        snapshot: Arc::new(RepoSnapshot {
            repo_name: "demo".into(),
            head_branch: Some("main".into()),
            state: RepositoryState::Merge,
            ..Default::default()
        }),
        epoch: 1,
    });
    let mut stage = StageStatusSnapshot {
        conflicted: vec![
            StageEntry {
                path: "src/a.rs".into(),
                old_path: None,
                kind: FileStatusKind::Conflicted,
            },
            StageEntry {
                path: "src/b.rs".into(),
                old_path: None,
                kind: FileStatusKind::Conflicted,
            },
        ],
        ..Default::default()
    };
    stage
        .conflict_labels
        .insert("src/a.rs".into(), "Both Modified".into());
    stage
        .conflict_labels
        .insert("src/b.rs".into(), "Deleted by Them".into());
    ts.deliver_engine_event(EngineEvent::StageStatusLoaded {
        status: Arc::new(stage),
        epoch: 1,
    });

    // 1. Trigger Resolve Conflicts via <Alt-c> with 2 conflicted files -> opens ConflictQueue.
    ts.send_key(KeyCode::Char('c'), KeyModifiers::ALT);
    assert!(ts.state().modal_stack.conflict_queue().is_some());
    let q_txt = screen_text(&ts.render(140, 30));
    assert!(q_txt.contains("src/a.rs"), "{q_txt}");
    assert!(q_txt.contains("src/b.rs"), "{q_txt}");
    assert!(q_txt.contains("Yours (main)"), "{q_txt}");
    assert!(q_txt.contains("Unresolved"), "{q_txt}");

    // 2. Press Enter on src/a.rs -> opens 3-pane merger; deliver 3-way document.
    let _ = ts.take_requests();
    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);
    let reqs = ts.take_requests();
    assert!(reqs.iter().any(|(r, _)| matches!(
        r,
        TaskRequest::LoadConflictDocument { path } if path == "src/a.rs"
    )));
    let doc_a = ConflictDocument::from_three_way(
        "src/a.rs",
        "",
        Some("top\nmid\nbase_line\nsep\nend\n"),
        Some("top_left\nmid\nours_line\nsep\nend\n"),
        Some("top\nmid\ntheirs_line\nsep\nend_right\n"),
        ConflictParseOptions::default(),
    );
    assert_eq!(doc_a.non_conflicting_unresolved_count(), 2);
    ts.deliver_engine_event(EngineEvent::ConflictDocumentLoaded {
        doc: doc_a,
        epoch: 1,
    });
    let m_txt = screen_text(&ts.render(140, 32));
    assert!(m_txt.contains("Apply Auto (2)"), "{m_txt}");

    // 3. Press `a` (Apply Non-Conflicting) -> resolves LeftOnly & RightOnly, focuses block 1.
    ts.send_key(KeyCode::Char('a'), KeyModifiers::NONE);
    {
        let cm = ts.state().modal_stack.conflict_merge().unwrap();
        assert_eq!(cm.doc.as_ref().unwrap().unresolved_count(), 1);
        assert_eq!(cm.selected_block, 1);
    }

    // 4. Press `b` -> expands Base (:1) ancestor panel showing `base_line`.
    ts.send_key(KeyCode::Char('b'), KeyModifiers::NONE);
    let base_txt = screen_text(&ts.render(140, 32));
    assert!(base_txt.contains("Base Ancestor (:1)"), "{base_txt}");
    assert!(base_txt.contains("base_line"), "{base_txt}");

    // 5. Press `e` -> inline multi-line Result editor, type `_custom`, press Ctrl+S to apply.
    ts.send_key(KeyCode::Char('e'), KeyModifiers::NONE);
    assert!(
        ts.state()
            .modal_stack
            .conflict_merge()
            .unwrap()
            .inline_editor
            .is_some()
    );
    let ed_txt = screen_text(&ts.render(140, 32));
    assert!(ed_txt.contains("Editing Result"), "{ed_txt}");
    for ch in "_custom".chars() {
        ts.send_key(KeyCode::Char(ch), KeyModifiers::NONE);
    }
    ts.send_key(KeyCode::Char('s'), KeyModifiers::CONTROL);
    {
        let cm = ts.state().modal_stack.conflict_merge().unwrap();
        assert!(cm.inline_editor.is_none());
        assert_eq!(
            cm.doc.as_ref().unwrap().block(1).unwrap().choice,
            ConflictChoice::Custom(vec!["ours_line_custom".into()])
        );
    }

    // 6. Test single-block reset (`u`) and Neither (`d`), then restore Custom via `y`.
    ts.send_key(KeyCode::Char('p'), KeyModifiers::NONE);
    assert_eq!(ts.state().modal_stack.conflict_merge().unwrap().selected_block, 1);
    ts.send_key(KeyCode::Char('u'), KeyModifiers::NONE);
    assert_eq!(
        ts.state()
            .modal_stack
            .conflict_merge()
            .unwrap()
            .doc
            .as_ref()
            .unwrap()
            .block(1)
            .unwrap()
            .choice,
        ConflictChoice::Unresolved
    );
    ts.send_key(KeyCode::Char('d'), KeyModifiers::NONE);
    assert_eq!(
        ts.state()
            .modal_stack
            .conflict_merge()
            .unwrap()
            .doc
            .as_ref()
            .unwrap()
            .block(1)
            .unwrap()
            .choice,
        ConflictChoice::Neither
    );

    // 7. Save & Stage src/a.rs -> auto-advances straight to src/b.rs in queue!
    let _ = ts.take_requests();
    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);
    let reqs_a = ts.take_requests();
    assert!(reqs_a.iter().any(|(r, _)| matches!(
        r,
        TaskRequest::SaveConflictResolution {
            path,
            content,
            stage: true,
            delete: false
        } if path == "src/a.rs" && content == b"top_left\nmid\nsep\nend_right\n"
    )));
    assert!(reqs_a.iter().any(|(r, _)| matches!(
        r,
        TaskRequest::LoadConflictDocument { path } if path == "src/b.rs"
    )));
    assert_eq!(
        ts.state().modal_stack.conflict_merge().unwrap().path,
        "src/b.rs"
    );

    // 8. Resolve src/b.rs -> all queue entries resolved -> pops "All Conflicts Resolved" dialog!
    let doc_b = ConflictDocument::parse(
        "src/b.rs",
        "<<<<<<< HEAD\nb_ours\n=======\nb_theirs\n>>>>>>> feat\n",
        None,
        None,
    );
    ts.deliver_engine_event(EngineEvent::ConflictDocumentLoaded {
        doc: doc_b,
        epoch: 2,
    });
    ts.send_key(KeyCode::Char('y'), KeyModifiers::NONE);
    let _ = ts.take_requests();
    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);

    let done_txt = screen_text(&ts.render(140, 32));
    assert!(done_txt.contains("All Conflicts Resolved"), "{done_txt}");
    assert!(done_txt.contains("Continue Merge"), "{done_txt}");

    // 9. Stay in queue (`Esc`) and test Revert Resolved (`r`) -> emits GitOp::UnresolveConflict!
    ts.send_key(KeyCode::Esc, KeyModifiers::NONE);
    let _ = ts.take_requests();
    ts.send_key(KeyCode::Char('r'), KeyModifiers::NONE);
    let rev_reqs = ts.take_requests();
    assert!(rev_reqs.iter().any(|(r, _)| matches!(
        r,
        TaskRequest::RunGitOp {
            op: GitOp::UnresolveConflict { paths }
        } if paths == &vec!["src/b.rs".to_string()]
    )));

    // 10. Simulate UnresolveConflict finishing, then batch Accept Yours (`y`) -> re-triggers Continue prompt -> Enter runs Ongoing::Continue!
    ts.deliver_engine_event(EngineEvent::GitOpFinished(GitOpOutcome::ok(
        GitOp::UnresolveConflict {
            paths: vec!["src/b.rs".into()],
        },
    )));
    let _ = ts.take_requests();
    ts.send_key(KeyCode::Char('y'), KeyModifiers::NONE);
    let accept_reqs = ts.take_requests();
    assert!(accept_reqs.iter().any(|(r, _)| matches!(
        r,
        TaskRequest::RunGitOp {
            op: GitOp::AcceptSide { paths, ours: true }
        } if paths == &vec!["src/b.rs".to_string()]
    )));
    ts.deliver_engine_event(EngineEvent::GitOpFinished(GitOpOutcome::ok(
        GitOp::AcceptSide {
            paths: vec!["src/b.rs".into()],
            ours: true,
        },
    )));
    let _ = ts.take_requests();
    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);
    let cont_reqs = ts.take_requests();
    assert!(cont_reqs.iter().any(|(r, _)| matches!(
        r,
        TaskRequest::RunGitOp {
            op: GitOp::Ongoing {
                kind: OngoingKind::Merge,
                step: OngoingStep::Continue
            }
        }
    )));
    assert!(ts.state().modal_stack.conflict_queue().is_none());
}

#[test]
fn commit_polish_template_merge_msg_reformat_author_identity_allow_empty_and_undo_guard() {
    let mut snap = super::snapshot();
    snap.user_name = None;
    snap.user_email = None;
    let mut ts = TestSession::with_snapshot(snap);
    ts.deliver_engine_event(EngineEvent::StageStatusLoaded {
        status: Arc::new(StageStatusSnapshot::default()),
        epoch: 1,
    });
    ts.dispatch(Action::SwitchTab(WorkspaceTab::Commit));

    // 1. commit.template loads into empty editor; MERGE_MSG supersedes template.
    ts.deliver_engine_event(EngineEvent::CommitPrecheckLoaded {
        info: jbgit_engine::CommitPrecheckInfo {
            comment_char: '#',
            commit_template: Some("# Template hint".into()),
            ..Default::default()
        },
        epoch: ts
            .state()
            .request_slots
            .latest(jbgit_core::RequestSlotId::CommitPrecheck)
            .max(1),
    });
    assert_eq!(ts.state().stage_workbench.editor().text(), "# Template hint");

    // Submitting comment-only template must be rejected as empty commit message!
    ts.dispatch(Action::SubmitCommit { and_push: false });
    assert!(ts.state().stage_workbench.empty_message_error());

    ts.deliver_engine_event(EngineEvent::CommitPrecheckLoaded {
        info: jbgit_engine::CommitPrecheckInfo {
            comment_char: '#',
            commit_template: Some("# Template hint\n".into()),
            prepared_message: Some("Merge branch 'feat' into main\n# Conflicts:\n#\ta.txt".into()),
            ..Default::default()
        },
        epoch: ts
            .state()
            .request_slots
            .latest(jbgit_core::RequestSlotId::CommitPrecheck)
            .max(2),
    });
    assert!(
        ts.state()
            .stage_workbench
            .editor()
            .text()
            .starts_with("Merge branch 'feat' into main")
    );

    // 2. Body > 72 col highlight + Reformat (`Alt+Q`) + undo.
    let long_msg = "feat: subject line\n\nThis is a very long body paragraph that definitely exceeds seventy two columns and should wrap nicely when reformatted.";
    ts.state_mut().stage_workbench.apply_external_message(
        long_msg,
        std::time::Instant::now(),
    );
    ts.dispatch(Action::CommitEditorReformat);
    let formatted = ts.state().stage_workbench.editor().text();
    let lines: Vec<&str> = formatted.lines().collect();
    assert_eq!(lines[0], "feat: subject line");
    assert!(lines.len() >= 4, "expected wrapped lines: {formatted}");
    assert!(lines[2].len() <= 72 && lines[3].len() <= 72, "{formatted}");
    ts.dispatch(Action::CommitEditorUndo);
    assert_eq!(ts.state().stage_workbench.editor().text(), long_msg);

    // 3. Author autocomplete from recent repo commits via Tab.
    ts.dispatch(Action::CommitFocusAuthorInput);
    ts.type_str("ali");
    ts.send_key(KeyCode::Tab, KeyModifiers::NONE);
    assert_eq!(
        ts.state().stage_workbench.author_input(),
        "Alice <alice@x.io>"
    );
    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);
    assert!(!ts.state().stage_workbench.editing_author());

    // 4a. Missing identity with empty staging & no tracked unstaged rejects cleanly
    //     with `toast.no_staged_to_commit` instead of opening a doomed IdentityDialog.
    ts.dispatch(Action::SubmitCommit { and_push: false });
    assert!(
        ts.state().modal_stack.last().is_none(),
        "must not open IdentityDialog when nothing can be committed"
    );
    assert_eq!(
        ts.state().status_notice.as_deref(),
        Some(ts.state().i18n.t("toast.no_staged_to_commit"))
    );

    // 4b. Missing identity with tracked unstaged changes asks `nothing_staged` confirmation
    //     FIRST, then confirming opens IdentityDialog with `stage_all_tracked: true`.
    ts.deliver_engine_event(EngineEvent::StageStatusLoaded {
        status: Arc::new(StageStatusSnapshot {
            unstaged: vec![jbgit_core::StageEntry {
                path: "tracked.rs".into(),
                old_path: None,
                kind: jbgit_core::FileStatusKind::Modified,
            }],
            ..Default::default()
        }),
        epoch: 2,
    });
    ts.dispatch(Action::SubmitCommit { and_push: false });
    assert!(
        matches!(
            ts.state().modal_stack.last(),
            Some(ModalLayer::Dialog(d))
                if matches!(d.as_ref(), crate::dialogs::Dialog::Choice(c)
                    if c.title == ts.state().i18n.t("dialog.nothing_staged.title"))
        ),
        "must confirm staging all tracked files before asking for identity"
    );
    ts.dispatch(Action::DialogConfirm);
    match ts.state().modal_stack.last() {
        Some(ModalLayer::Dialog(d)) => match d.as_ref() {
            crate::dialogs::Dialog::Identity(id) => {
                assert!(
                    id.pending_commit
                        .as_ref()
                        .is_some_and(|r| r.stage_all_tracked)
                );
            }
            other => panic!("expected IdentityDialog after confirming stage-all, got {other:?}"),
        },
        other => panic!("expected Dialog, got {other:?}"),
    }
    ts.send_key(KeyCode::Esc, KeyModifiers::NONE);

    // 4c. Failed `SetIdentity` clears `pending_commit_after_identity` so a stale commit
    //     cannot fire on a later unrelated SetIdentity.
    ts.deliver_engine_event(EngineEvent::StageStatusLoaded {
        status: Arc::new(StageStatusSnapshot::default()),
        epoch: 3,
    });
    ts.dispatch(Action::CommitToggleAllowEmpty);
    assert!(ts.state().stage_workbench.allow_empty());
    let _ = ts.take_requests();
    ts.dispatch(Action::SubmitCommit { and_push: false });
    assert!(matches!(
        ts.state().modal_stack.last(),
        Some(ModalLayer::Dialog(d)) if matches!(d.as_ref(), crate::dialogs::Dialog::Identity(_))
    ));
    let id_screen = super::screen_text(&ts.render(100, 28));
    assert!(id_screen.contains("Configure Git Identity"), "{id_screen}");
    ts.type_str("Dev User");
    ts.send_key(KeyCode::Tab, KeyModifiers::NONE);
    ts.type_str("dev@example.com");
    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);
    let set_id_op = jbgit_core::GitOp::SetIdentity {
        name: "Dev User".into(),
        email: "dev@example.com".into(),
        global: false,
    };
    assert_eq!(ts.take_git_ops(), vec![set_id_op.clone()]);
    ts.deliver_engine_event(EngineEvent::GitOpFinished(
        jbgit_core::GitOpOutcome::failed(
            set_id_op.clone(),
            jbgit_core::GitFailure::Other("permission denied".into()),
        ),
    ));
    assert!(
        ts.state().pending_commit_after_identity.is_none(),
        "failed SetIdentity must clear pending_commit_after_identity"
    );

    // 4d. Successful `SetIdentity` after resubmitting resumes `ExecuteCommit` with allow_empty: true!
    ts.dispatch(Action::SubmitCommit { and_push: false });
    ts.type_str("Dev User");
    ts.send_key(KeyCode::Tab, KeyModifiers::NONE);
    ts.type_str("dev@example.com");
    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(ts.take_git_ops(), vec![set_id_op.clone()]);
    let _ = ts.take_requests();
    ts.deliver_engine_event(EngineEvent::GitOpFinished(jbgit_core::GitOpOutcome::ok(
        set_id_op,
    )));
    let reqs = ts.take_requests();
    assert!(
        reqs.iter().any(|(r, _)| matches!(
            r,
            TaskRequest::ExecuteCommit { request, .. } if request.allow_empty
        )),
        "expected resumed ExecuteCommit with allow_empty=true, got {reqs:?}"
    );

    // 5. UndoLastCommit on a merge HEAD or already-pushed HEAD triggers danger confirmation.
    let mut pushed_merge_snap = super::snapshot();
    let head_oid = pushed_merge_snap.head_oid.unwrap();
    // Make HEAD a merge commit AND make origin/main point to HEAD (already pushed).
    if let Some(c) = pushed_merge_snap.commits.iter_mut().find(|c| c.oid == head_oid) {
        c.parents = vec![Oid::from_bytes([11u8; 20]), Oid::from_bytes([10u8; 20])];
    }
    if let Some(r) = pushed_merge_snap
        .refs
        .iter_mut()
        .find(|r| r.kind == jbgit_core::RefKind::RemoteBranch && r.name == "origin/main")
    {
        r.target_oid = head_oid;
    }
    ts.deliver_engine_event(EngineEvent::RepoSnapshotLoaded {
        snapshot: Arc::new(pushed_merge_snap),
        epoch: 20,
    });
    let _ = ts.take_requests();
    ts.dispatch(Action::UndoLastCommit);
    assert!(
        ts.take_requests()
            .iter()
            .all(|(r, _)| !matches!(r, TaskRequest::UndoLastCommit)),
        "must not immediately undo pushed merge commit without confirmation"
    );
    match ts.state().modal_stack.last() {
        Some(ModalLayer::Dialog(d)) => match d.as_ref() {
            crate::dialogs::Dialog::Choice(c) => {
                assert_eq!(c.title, "Undo Last Commit?");
                assert!(c.danger);
                assert!(c.body.iter().any(|l| l.contains("merge commit") && l.contains("origin/main")));
            }
            other => panic!("expected ChoiceDialog, got {other:?}"),
        },
        other => panic!("expected Dialog, got {other:?}"),
    }
    // Confirming the dialog executes UndoLastCommit.
    ts.dispatch(Action::DialogSelectOption {
        idx: 0,
        confirm: true,
    });
    assert!(
        ts.take_requests()
            .iter()
            .any(|(r, _)| matches!(r, TaskRequest::UndoLastCommit))
    );
}
