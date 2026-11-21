use std::collections::VecDeque;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant};

use chrono::Local;
use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use futures::StreamExt;
use jbgit_core::{
    AmendCommitPreview, CommitSummary, ConsoleLine, ConsoleStream, DiffContext, DiffHunk, DiffLine,
    DiffLineKind, FileChange, FileDiff, FileDiffKey, FileStatusKind, GitConsoleEntry, Oid, PaneId,
    PersonStamp, RefInfo, RefKind, RefreshScope, RepoSnapshot, RepositoryState, SignatureStatus,
    SplitAxis, SplitterId, StageEntry, StageStatusSnapshot, StageTreeRowKind, StashEntry,
    WorkspaceTab,
};
use jbgit_engine::{CommitOutcome, EngineEvent, StageDiffKey, StageDiffSide, ThreeDocStageDiff};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::layout::Rect;
use unicode_width::UnicodeWidthStr;

use crate::app::{Action, AppState, ContextMenuAction, ContextMenuEntryKind, ContextMenuItem, Effect, ModalLayer};
use crate::external_editor::{
    handle_external_editor_effects_with, is_external_editor_disabled_val,
    resolve_editor_command_with, split_editor_command,
};
use crate::input::route_terminal_event;
use crate::views::{render_app, truncate_to_width};

#[test]
fn test_backend_render_tiny_and_normal_no_panic() {
    let mut state = AppState::new_with_config(
        PathBuf::from("."),
        None,
        crate::config::AppConfig::load_defaults(),
    );
    for (w, h) in [(10, 4), (45, 12), (59, 15), (120, 40)] {
        let backend = TestBackend::new(w, h);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| render_app(f, &mut state)).unwrap();
    }
}

#[test]
fn tab_switch_and_focus_ring_and_splitter_drag() {
    let mut state = AppState::new_with_config(
        PathBuf::from("."),
        None,
        crate::config::AppConfig::load_defaults(),
    );
    let backend = TestBackend::new(120, 40);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|f| render_app(f, &mut state)).unwrap();

    for (ch, expected) in [
        ('1', WorkspaceTab::Commit),
        ('2', WorkspaceTab::Log),
        ('3', WorkspaceTab::Stash),
        ('4', WorkspaceTab::Console),
    ] {
        let actions = route_terminal_event(
            Event::Key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE)),
            &mut state,
        );
        for a in actions {
            state.dispatch(a);
        }
        assert_eq!(state.active_tab, expected);
    }

    state.dispatch(Action::SwitchTab(WorkspaceTab::Log));
    assert_eq!(state.focused_pane, PaneId::LogCommitTable);
    state.dispatch(Action::CycleFocus { reverse: false });
    assert_eq!(state.focused_pane, PaneId::LogChangesTree);
    state.dispatch(Action::CycleFocus { reverse: false });
    assert_eq!(state.focused_pane, PaneId::LogDetailsDiff);
    state.dispatch(Action::CycleFocus { reverse: false });
    assert_eq!(state.focused_pane, PaneId::LogBranchesTree);

    terminal.draw(|f| render_app(f, &mut state)).unwrap();
    let (_, container) = state
        .splitter_containers
        .get(&SplitterId::LogLeftHorizontal)
        .copied()
        .unwrap();
    let initial_ratio = state.splitters.ratio(SplitterId::LogLeftHorizontal);
    let split = state.splitters.split(
        container,
        SplitterId::LogLeftHorizontal,
        SplitAxis::Horizontal,
    );
    let div_x = split.divider.x;
    let div_y = split.divider.y + 2;

    let actions = route_terminal_event(
        Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: div_x,
            row: div_y,
            modifiers: KeyModifiers::NONE,
        }),
        &mut state,
    );
    for a in actions {
        state.dispatch(a);
    }
    assert!(state.drag_state.is_some());

    route_terminal_event(
        Event::Mouse(MouseEvent {
            kind: MouseEventKind::Drag(MouseButton::Left),
            column: div_x + 12,
            row: div_y,
            modifiers: KeyModifiers::NONE,
        }),
        &mut state,
    );
    let dragged_ratio = state.splitters.ratio(SplitterId::LogLeftHorizontal);
    assert!(dragged_ratio > initial_ratio);

    route_terminal_event(
        Event::Mouse(MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            column: div_x + 12,
            row: div_y,
            modifiers: KeyModifiers::NONE,
        }),
        &mut state,
    );
    assert!(state.drag_state.is_none());

    terminal.draw(|f| render_app(f, &mut state)).unwrap();
    let split2 = state.splitters.split(
        container,
        SplitterId::LogLeftHorizontal,
        SplitAxis::Horizontal,
    );
    let dx2 = split2.divider.x;
    for _ in 0..2 {
        let acts = route_terminal_event(
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: dx2,
                row: div_y,
                modifiers: KeyModifiers::NONE,
            }),
            &mut state,
        );
        for a in acts {
            state.dispatch(a);
        }
    }
    let reset_ratio = state.splitters.ratio(SplitterId::LogLeftHorizontal);
    assert!((reset_ratio - initial_ratio).abs() < 0.001);
    let _ = Rect::ZERO;
}

#[test]
fn mouse_wheel_routes_to_hovered_pane_and_right_click_preselects() {
    let mut state = AppState::new_with_config(
        PathBuf::from("."),
        None,
        crate::config::AppConfig::load_defaults(),
    );
    let oid1 = Oid::from_bytes([1u8; 20]);
    let oid2 = Oid::from_bytes([2u8; 20]);
    state.repo_snapshot = Some(Arc::new(RepoSnapshot {
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
    }));
    state.console_entries.push(GitConsoleEntry {
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

    let backend = TestBackend::new(120, 40);
    let mut terminal = Terminal::new(backend).unwrap();

    state.dispatch(Action::SwitchTab(WorkspaceTab::Console));
    assert_eq!(state.focused_pane, PaneId::ConsoleCommandList);
    terminal.draw(|f| render_app(f, &mut state)).unwrap();
    for a in route_terminal_event(
        Event::Mouse(MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: 90,
            row: 10,
            modifiers: KeyModifiers::NONE,
        }),
        &mut state,
    ) {
        state.dispatch(a);
    }
    assert_eq!(state.console_output_scroll, 0);

    state.console_entries[0].output_lines = (0..60)
        .map(|i| ConsoleLine {
            stream: ConsoleStream::Stdout,
            text: format!("line {i}"),
        })
        .collect();
    terminal.draw(|f| render_app(f, &mut state)).unwrap();
    let actions = route_terminal_event(
        Event::Mouse(MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: 90,
            row: 10,
            modifiers: KeyModifiers::NONE,
        }),
        &mut state,
    );
    for a in actions {
        state.dispatch(a);
    }
    assert!(state.console_output_scroll > 0);
    assert_eq!(state.focused_pane, PaneId::ConsoleCommandList);

    state.dispatch(Action::SwitchTab(WorkspaceTab::Log));
    terminal.draw(|f| render_app(f, &mut state)).unwrap();
    assert_eq!(state.log_graph.selected_idx, 0);

    let actions = route_terminal_event(
        Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Right),
            column: 40,
            row: 5,
            modifiers: KeyModifiers::NONE,
        }),
        &mut state,
    );
    for a in actions {
        state.dispatch(a);
    }
    assert_eq!(state.log_graph.selected_idx, 1);
    assert_eq!(state.modal_stack.len(), 1);

    terminal.draw(|f| render_app(f, &mut state)).unwrap();
    let actions = route_terminal_event(
        Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 5,
            row: 3,
            modifiers: KeyModifiers::NONE,
        }),
        &mut state,
    );
    for a in actions {
        state.dispatch(a);
    }
    assert!(state.modal_stack.is_empty());
    assert_eq!(state.log_graph.selected_idx, 1);
}

#[test]
fn stale_epoch_events_are_silently_dropped_on_ui_arrival() {
    let mut state = AppState::new_with_config(
        PathBuf::from("."),
        None,
        crate::config::AppConfig::load_defaults(),
    );
    state.latest_repo_epoch = 5;
    state.latest_stage_epoch = 4;

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
        head_branch: Some("old".into()),
        head_oid: None,
        refs: vec![],
        commits: vec![],
    });
    state.apply_engine_event(EngineEvent::RepoSnapshotLoaded {
        snapshot: stale_snap,
        epoch: 3,
    });
    assert!(state.repo_snapshot.is_none());

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
        head_branch: Some("main".into()),
        head_oid: Some(Oid::ZERO),
        refs: vec![],
        commits: vec![],
    });
    state.apply_engine_event(EngineEvent::RepoSnapshotLoaded {
        snapshot: fresh_snap,
        epoch: 5,
    });
    assert_eq!(
        state.repo_snapshot.as_ref().map(|s| s.repo_name.as_str()),
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
    state.apply_engine_event(EngineEvent::StageStatusLoaded {
        status: stale_stage,
        epoch: 2,
    });
    assert!(state.stage_status.is_none());
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
    let mut state = AppState::new_with_config(
        PathBuf::from("."),
        None,
        crate::config::AppConfig::load_defaults(),
    );
    state.stage_status = Some(Arc::new(StageStatusSnapshot {
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
    }));
    state.dispatch(Action::SwitchTab(WorkspaceTab::Commit));

    let backend = TestBackend::new(120, 40);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|f| render_app(f, &mut state)).unwrap();

    let actions = route_terminal_event(
        Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Right),
            column: 10,
            row: 6,
            modifiers: KeyModifiers::NONE,
        }),
        &mut state,
    );
    for a in actions {
        state.dispatch(a);
    }
    assert_eq!(state.commit_stage_selected_idx, 2);
    assert_eq!(state.modal_stack.len(), 1);

    terminal.draw(|f| render_app(f, &mut state)).unwrap();
    route_terminal_event(
        Event::Mouse(MouseEvent {
            kind: MouseEventKind::Moved,
            column: 14,
            row: 8,
            modifiers: KeyModifiers::NONE,
        }),
        &mut state,
    );
    assert!(matches!(
        state.modal_stack.last(),
        Some(ModalLayer::ContextMenu { selected: 1, .. })
    ));

    let dup_actions = route_terminal_event(
        Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Right),
            column: 14,
            row: 8,
            modifiers: KeyModifiers::NONE,
        }),
        &mut state,
    );
    assert!(dup_actions.is_empty());

    let sub_actions = route_terminal_event(
        Event::Key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::NONE)),
        &mut state,
    );
    for a in sub_actions {
        state.dispatch(a);
    }
    assert_eq!(state.modal_stack.len(), 2);
    assert!(matches!(
        state.modal_stack.last(),
        Some(ModalLayer::ContextMenu { depth: 1, .. })
    ));

    let run_actions = route_terminal_event(
        Event::Key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE)),
        &mut state,
    );
    let mut effects = Vec::new();
    for a in run_actions {
        effects.extend(state.dispatch(a));
    }
    assert!(state.modal_stack.is_empty());
    assert_eq!(effects.len(), 1);
    let _ = (
        ContextMenuAction::RefreshRepo,
        ContextMenuEntryKind::Submenu(vec![]),
        ContextMenuItem::action("x", None, None, ContextMenuAction::RefreshRepo),
    );
}

#[test]
fn index_lock_retry_replays_blocked_cli_command() {
    let mut state = AppState::new_with_config(
        PathBuf::from("."),
        None,
        crate::config::AppConfig::load_defaults(),
    );
    state.apply_engine_event(EngineEvent::IndexLockDetected {
        lock_path: ".git/index.lock".into(),
        message: "Unable to create '.git/index.lock': File exists.".into(),
        retry_args: vec!["add".into(), "--pathspec-from-file=-".into()],
        retry_stdin: Some(b"src/lib.rs\0".to_vec()),
        retry_is_read_only: false,
        retry_refresh_after: RefreshScope::StageStatus,
    });
    assert_eq!(state.modal_stack.len(), 1);

    let actions = route_terminal_event(
        Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        &mut state,
    );
    let mut effects = Vec::new();
    for a in actions {
        effects.extend(state.dispatch(a));
    }
    assert!(state.modal_stack.is_empty());
    assert_eq!(effects.len(), 1);
    match &effects[0] {
        Effect::RunCli {
            args,
            stdin_payload,
            is_read_only,
            refresh_after,
        } => {
            assert_eq!(
                args,
                &vec!["add".to_string(), "--pathspec-from-file=-".to_string()]
            );
            assert_eq!(stdin_payload.as_deref(), Some(b"src/lib.rs\0".as_slice()));
            assert!(!is_read_only);
            assert_eq!(*refresh_after, RefreshScope::StageStatus);
        }
        other => panic!("expected RunCli retry effect, got {other:?}"),
    }
}

#[test]
fn detached_head_and_overflow_hover_tooltips_and_overflow_clicks() {
    let mut state = AppState::new_with_config(
        PathBuf::from("."),
        None,
        crate::config::AppConfig::load_defaults(),
    );
    let oid = Oid::from_bytes([7u8; 20]);
    state.repo_snapshot = Some(Arc::new(RepoSnapshot {
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
    }));

    state.i18n = Arc::new(crate::i18n::I18n::load("en-US", None));
    let backend = TestBackend::new(100, 24);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|f| render_app(f, &mut state)).unwrap();

    let row0_text = |buf: &ratatui::buffer::Buffer, y: u16| -> String {
        (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect()
    };
    let row_line = row0_text(terminal.backend().buffer(), 4);
    let col_of = |needle: &str| {
        let byte_idx = row_line.find(needle).unwrap();
        row_line[..byte_idx].width() as u16
    };
    let head_x = col_of("HEAD");
    let plus_x = col_of("+");

    route_terminal_event(
        Event::Mouse(MouseEvent {
            kind: MouseEventKind::Moved,
            column: head_x,
            row: 4,
            modifiers: KeyModifiers::NONE,
        }),
        &mut state,
    );
    terminal.draw(|f| render_app(f, &mut state)).unwrap();
    let full_text: String = (0..24)
        .map(|y| row0_text(terminal.backend().buffer(), y))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(full_text.contains("Detached HEAD"), "{full_text}");

    route_terminal_event(
        Event::Mouse(MouseEvent {
            kind: MouseEventKind::Moved,
            column: plus_x,
            row: 4,
            modifiers: KeyModifiers::NONE,
        }),
        &mut state,
    );
    terminal.draw(|f| render_app(f, &mut state)).unwrap();
    let full_text: String = (0..24)
        .map(|y| row0_text(terminal.backend().buffer(), y))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(full_text.contains("All Commit References"), "{full_text}");
    assert!(full_text.contains("v1.0.1"), "{full_text}");

    let left_acts = route_terminal_event(
        Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: plus_x,
            row: 4,
            modifiers: KeyModifiers::NONE,
        }),
        &mut state,
    );
    for a in left_acts {
        state.dispatch(a);
    }
    assert!(state.modal_stack.is_empty());

    let right_acts = route_terminal_event(
        Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Right),
            column: plus_x,
            row: 4,
            modifiers: KeyModifiers::NONE,
        }),
        &mut state,
    );
    for a in right_acts {
        state.dispatch(a);
    }
    assert_eq!(state.modal_stack.len(), 1);
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

    let mut state = AppState::new_with_config(
        PathBuf::from("."),
        None,
        crate::config::AppConfig::load_defaults(),
    );
    state.repo_snapshot = Some(Arc::new(RepoSnapshot {
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
    }));

    let backend = TestBackend::new(120, 20);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|f| render_app(f, &mut state)).unwrap();

    let row_text = |buf: &ratatui::buffer::Buffer, y: u16| -> String {
        (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect()
    };
    let row1 = row_text(terminal.backend().buffer(), 5);
    let arrow_byte = row1
        .find('▾')
        .expect("expected long-edge down arrow on row 1");
    let arrow_x = row1[..arrow_byte].width() as u16;

    route_terminal_event(
        Event::Mouse(MouseEvent {
            kind: MouseEventKind::Moved,
            column: arrow_x,
            row: 5,
            modifiers: KeyModifiers::NONE,
        }),
        &mut state,
    );
    terminal.draw(|f| render_app(f, &mut state)).unwrap();
    let status_line = row_text(terminal.backend().buffer(), 19);
    assert!(status_line.contains(&oid_n(41).short()), "{status_line}");
    assert!(status_line.contains("commit 41"), "{status_line}");

    let acts = route_terminal_event(
        Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: arrow_x,
            row: 5,
            modifiers: KeyModifiers::NONE,
        }),
        &mut state,
    );
    for a in acts {
        state.dispatch(a);
    }
    assert_eq!(state.log_graph.selected_idx, 41);
    assert!(state.log_graph.scroll_offset > 0);

    let back_acts = route_terminal_event(
        Event::Key(KeyEvent::new(KeyCode::Left, KeyModifiers::ALT)),
        &mut state,
    );
    for a in back_acts {
        state.dispatch(a);
    }
    assert_eq!(state.log_graph.selected_idx, 0);
}

#[test]
fn stage_workbench_dual_presence_visual_select_and_amend_draft_restore() {
    let mut state = AppState::new_with_config(
        PathBuf::from("."),
        None,
        crate::config::AppConfig::load_defaults(),
    );
    state.stage_status = Some(Arc::new(StageStatusSnapshot {
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
    }));
    state.dispatch(Action::SwitchTab(WorkspaceTab::Commit));
    state.dispatch(Action::StageToggleGroupByDir);

    assert_eq!(state.stage_workbench.rows.len(), 4);

    state.dispatch(Action::SelectCommitStageRow(1));
    assert_eq!(
        state
            .stage_workbench
            .current_diff_key
            .as_ref()
            .map(|k| k.side),
        Some(StageDiffSide::Staged)
    );

    state.dispatch(Action::SelectCommitStageRow(3));
    assert_eq!(
        state
            .stage_workbench
            .current_diff_key
            .as_ref()
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
        },
        vcs_exists: true,
        staged_exists: true,
        local_exists: true,
        vcs_bytes: vcs.to_vec(),
        staged_bytes: staged.to_vec(),
        local_bytes: local.to_vec(),
        file_mode: 0o100644,
        is_binary: false,
        is_conflicted: false,
        tracker,
        hunks,
        is_truncated,
        is_too_large: false,
    });
    state.apply_engine_event(EngineEvent::StageFileDiffLoaded { diff, epoch: 1 });

    state.dispatch(Action::FocusPane(PaneId::CommitDiffPreview));
    state.dispatch(Action::StageDiffMoveCursor(3));
    state.dispatch(Action::StageDiffToggleVisual);
    let effs = state.dispatch(Action::StageDiffStageSelection);
    match effs.into_iter().find(|e| matches!(e, Effect::ApplyStageBuffer { .. })) {
        Some(Effect::ApplyStageBuffer { new_staged_bytes, .. }) => {
            assert_eq!(
                new_staged_bytes,
                b"fn a() {}\nfn staged_line() {}\nfn local_1() {}\n"
            );
        }
        other => panic!("expected ApplyStageBuffer, got {other:?}"),
    }

    state.dispatch(Action::FocusPane(PaneId::CommitMessageEditor));
    state.dispatch(Action::CommitEditorPaste("wip: my draft".into()));
    state.apply_engine_event(EngineEvent::AmendPreviewLoaded {
        preview: Some(AmendCommitPreview {
            short_sha: "a8f3d21".into(),
            message: "feat: previous commit".into(),
            files: vec![FileChange {
                path: "prev.rs".into(),
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
    });

    state.dispatch(Action::CommitToggleAmend);
    assert!(state.stage_workbench.amend);
    assert_eq!(state.stage_workbench.editor.text(), "feat: previous commit");
    assert!(
        state
            .stage_workbench
            .rows
            .iter()
            .any(|r| r.label.contains("Head Commit to Amend (a8f3d21)"))
    );

    let backend = TestBackend::new(120, 40);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|f| render_app(f, &mut state)).unwrap();

    state.dispatch(Action::CommitToggleAmend);
    assert!(!state.stage_workbench.amend);
    assert_eq!(state.stage_workbench.editor.text(), "wip: my draft");
}

fn draft_test_state(tag: &str) -> (AppState, PathBuf, PathBuf) {
    let mut state = AppState::new_with_config(
        PathBuf::from("."),
        None,
        crate::config::AppConfig::load_defaults(),
    );
    state.stage_status = Some(Arc::new(StageStatusSnapshot::default()));
    let dir = std::env::temp_dir().join(format!(
        "jbgit-test-draft-debounce-{tag}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("jbgit-commit-draft");
    state.stage_workbench.load_draft(file.clone());
    state.dispatch(Action::SwitchTab(WorkspaceTab::Commit));
    state.dispatch(Action::FocusPane(PaneId::CommitMessageEditor));
    (state, dir, file)
}

#[test]
fn commit_draft_typing_is_debounced_then_written_in_background() {
    let (mut state, dir, file) = draft_test_state("async");
    for ch in "feat: twenty chars!!".chars() {
        state.dispatch(Action::CommitEditorChar(ch));
    }
    assert!(!file.exists(), "keystrokes must not write the draft synchronously");
    assert!(state.stage_workbench.draft_dirty);
    let due = state.stage_workbench.draft_save_deadline.expect("debounce armed");
    assert!(state.next_deadline().is_some_and(|d| d <= due));

    let mut effects = Vec::new();
    state.flush_pending_at(due - Duration::from_millis(1), &mut effects);
    state.stage_workbench.wait_draft_writes();
    assert!(!file.exists());

    state.flush_pending_at(due, &mut effects);
    assert!(!state.stage_workbench.draft_dirty);
    assert!(state.stage_workbench.draft_save_deadline.is_none());
    state.stage_workbench.wait_draft_writes();
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "feat: twenty chars!!");

    for _ in 0..20 {
        state.dispatch(Action::CommitEditorBackspace);
    }
    assert!(file.exists());
    let due = state.stage_workbench.draft_save_deadline.unwrap();
    state.flush_pending_at(due, &mut effects);
    state.stage_workbench.wait_draft_writes();
    assert!(!file.exists());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn commit_draft_flushes_synchronously_on_exit_and_submit() {
    let (mut state, dir, file) = draft_test_state("flush");
    state.dispatch(Action::CommitEditorPaste("wip: exit flush".into()));
    assert!(!file.exists());
    state.stage_workbench.flush_draft_sync();
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "wip: exit flush");

    state.dispatch(Action::CommitEditorChar('!'));
    let due = state.stage_workbench.draft_save_deadline.unwrap();
    state.flush_pending_at(due, &mut Vec::new());
    state.dispatch(Action::CommitEditorChar('?'));
    state.stage_workbench.flush_draft_sync();
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "wip: exit flush!?");

    state.dispatch(Action::CommitEditorChar('+'));
    let due = state.stage_workbench.draft_save_deadline.unwrap();
    state.flush_pending_at(due, &mut Vec::new());
    assert!(!state.stage_workbench.draft_dirty);
    state.stage_workbench.flush_draft_sync();
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "wip: exit flush!?+");

    state.dispatch(Action::CommitEditorChar('#'));
    assert!(state.stage_workbench.draft_dirty);
    state.dispatch(Action::SubmitCommit { and_push: false });
    assert!(!state.stage_workbench.draft_dirty);
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "wip: exit flush!?+#");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stage_review_fixes_amend_diff_gutter_click_draft_persistence_and_push_confirm() {
    let mut state = AppState::new_with_config(
        PathBuf::from("."),
        None,
        crate::config::AppConfig::load_defaults(),
    );
    state.stage_status = Some(Arc::new(StageStatusSnapshot::default()));
    let tmp_dir = std::env::temp_dir().join(format!("jbgit-test-draft-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&tmp_dir);
    let draft_file = tmp_dir.join("jbgit-commit-draft");
    let _ = std::fs::remove_file(&draft_file);
    state.stage_workbench.load_draft(draft_file.clone());
    state.dispatch(Action::SwitchTab(WorkspaceTab::Commit));

    state.dispatch(Action::FocusPane(PaneId::CommitMessageEditor));
    state.dispatch(Action::CommitEditorPaste("draft: unsent note".into()));
    assert!(state.commit_history_candidates().iter().any(|c| c == "draft: unsent note"));
    state.stage_workbench.flush_draft_sync();
    let mut reloaded = AppState::new(PathBuf::from("."), None);
    reloaded.stage_workbench.load_draft(draft_file);
    assert_eq!(reloaded.stage_workbench.editor.text(), "draft: unsent note");
    let _ = std::fs::remove_dir_all(&tmp_dir);

    state.apply_engine_event(EngineEvent::AmendPreviewLoaded {
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
    });
    let amend_effs = state.dispatch(Action::CommitToggleAmend);
    assert!(
        amend_effs
            .iter()
            .any(|e| matches!(e, Effect::RequestAmendPreview))
    );
    let amend_file_idx = state
        .stage_workbench
        .rows
        .iter()
        .position(|r| matches!(r.kind, StageTreeRowKind::AmendFile { .. }))
        .expect("AmendFile row");
    state.dispatch(Action::SelectCommitStageRow(amend_file_idx));
    assert_eq!(
        state
            .stage_workbench
            .current_diff_key
            .as_ref()
            .map(|k| k.side),
        Some(StageDiffSide::AmendCommit)
    );

    state.apply_engine_event(EngineEvent::AmendPreviewLoaded {
        preview: Some(AmendCommitPreview {
            short_sha: "5566778".into(),
            message: "feat: newer head commit".into(),
            files: vec![],
        }),
    });
    assert_eq!(state.stage_workbench.editor.text(), "feat: newer head commit");
    state.dispatch(Action::CommitToggleAmend);

    state.stage_status = Some(Arc::new(StageStatusSnapshot {
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
    }));
    state.dispatch(Action::StageToggleGroupByDir);
    state.dispatch(Action::SelectCommitStageRow(3));
    let vcs = b"let x = old_val;\n";
    let staged = b"let x = staged_val;\n";
    let local = b"let x = local_val;\n";
    let tracker = jbgit_core::StageLineTracker::new(vcs, staged, local);
    let (hunks, is_truncated) = tracker.build_interactive_hunks(false, DiffContext::Three, 20_000);
    state.stage_workbench.current_diff_key = Some(StageDiffKey {
        path: "src/lib.rs".into(),
        old_path: None,
        side: StageDiffSide::Unstaged,
        context: DiffContext::Three,
    });
    state.apply_engine_event(EngineEvent::StageFileDiffLoaded {
        diff: Arc::new(ThreeDocStageDiff {
            key: StageDiffKey {
                path: "src/lib.rs".into(),
                old_path: None,
                side: StageDiffSide::Unstaged,
                context: DiffContext::Three,
            },
            vcs_exists: true,
            staged_exists: true,
            local_exists: true,
            vcs_bytes: vcs.to_vec(),
            staged_bytes: staged.to_vec(),
            local_bytes: local.to_vec(),
            file_mode: 0o100644,
            is_binary: false,
            is_conflicted: false,
            tracker,
            hunks,
            is_truncated,
            is_too_large: false,
        }),
        epoch: 1,
    });

    let backend = TestBackend::new(120, 36);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|f| render_app(f, &mut state)).unwrap();

    let buf = terminal.backend().buffer();
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
    let click_acts = route_terminal_event(
        Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: gx,
            row: gy,
            modifiers: KeyModifiers::NONE,
        }),
        &mut state,
    );
    for a in click_acts {
        state.dispatch(a);
    }
    assert!(matches!(
        state.modal_stack.last(),
        Some(ModalLayer::FloatingPopup {
            kind: crate::app::PopupKind::ThreeWayInspector,
            ..
        })
    ));
    terminal.draw(|f| render_app(f, &mut state)).unwrap();
    state.dispatch(Action::PopModal);
    assert!(state.stage_workbench.three_way_range_idx.is_none());

    state.dispatch(Action::FocusPane(PaneId::CommitDiffPreview));
    for ch in [']', 'c'] {
        for a in route_terminal_event(
            Event::Key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE)),
            &mut state,
        ) {
            state.dispatch(a);
        }
    }
    assert_eq!(state.stage_workbench.diff_context, DiffContext::Three);

    let commit_effs = state.apply_engine_event(EngineEvent::CommitCompleted(CommitOutcome {
        success: true,
        commit_oid: None,
        short_sha: Some("abc1234".into()),
        summary: "Committed abc1234".into(),
        error_output: None,
        and_push: true,
        preserved_message: None,
    }));
    assert!(!commit_effs.iter().any(|e| matches!(
        e,
        Effect::RunCli { args, .. } if args.first().map(String::as_str) == Some("push")
    )));
    assert!(matches!(
        state.modal_stack.last(),
        Some(ModalLayer::CriticalDialog { title, .. })
            if title == state.i18n.t("dialog.push.confirm_title")
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

    let mut state = AppState::new(temp.path().to_path_buf(), None);
    state.repo_snapshot = Some(Arc::new(RepoSnapshot {
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
        head_branch: Some("main".into()),
        head_oid: Some(Oid::from_bytes([1u8; 20])),
        refs: vec![],
        commits: vec![],
    }));

    assert!((state.splitters.ratio(SplitterId::LogDetailsVertical) - 0.25).abs() < 1e-4);
    assert!((state.splitters.ratio(SplitterId::LogDiffRightHorizontal) - 0.75).abs() < 1e-4);
    assert!((state.splitters.ratio(SplitterId::LogDiffBottomVertical) - 0.40).abs() < 1e-4);
    assert!((state.splitters.ratio(SplitterId::CommitLeftVertical) - 0.75).abs() < 1e-4);
    assert!((state.splitters.ratio(SplitterId::StashPreviewVertical) - 0.50).abs() < 1e-4);

    let backend = TestBackend::new(120, 40);
    let mut terminal = Terminal::new(backend).unwrap();

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
        state.dispatch(Action::SwitchTab(tab));
        if let Some(d) = dock {
            state.details.diff_preview = true;
            state.details.diff_dock = d;
        } else {
            state.details.diff_preview = false;
        }
        terminal.draw(|f| render_app(f, &mut state)).unwrap();

        let (axis, container) = state
            .splitter_containers
            .get(&splitter_id)
            .copied()
            .unwrap_or_else(|| panic!("splitter {splitter_id:?} not registered on {tab:?}"));
        assert_eq!(axis, expected_axis);

        let split = state.splitters.split(container, splitter_id, expected_axis);
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

        for a in route_terminal_event(
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: div_x,
                row: div_y,
                modifiers: KeyModifiers::NONE,
            }),
            &mut state,
        ) {
            state.dispatch(a);
        }
        assert!(state.drag_state.is_some());

        route_terminal_event(
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::Drag(MouseButton::Left),
                column: target_x,
                row: target_y,
                modifiers: KeyModifiers::NONE,
            }),
            &mut state,
        );
        let dragged = state.splitters.ratio(splitter_id);
        if delta > 0 {
            assert!(dragged > expected_default);
        } else {
            assert!(dragged < expected_default);
        }

        route_terminal_event(
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::Up(MouseButton::Left),
                column: target_x,
                row: target_y,
                modifiers: KeyModifiers::NONE,
            }),
            &mut state,
        );

        terminal.draw(|f| render_app(f, &mut state)).unwrap();
        let split2 = state.splitters.split(container, splitter_id, expected_axis);
        let (reset_x, reset_y) = match expected_axis {
            SplitAxis::Vertical => (split2.divider.x + 2, split2.divider.y),
            SplitAxis::Horizontal => (split2.divider.x, split2.divider.y + 2),
        };
        for _ in 0..2 {
            for a in route_terminal_event(
                Event::Mouse(MouseEvent {
                    kind: MouseEventKind::Down(MouseButton::Left),
                    column: reset_x,
                    row: reset_y,
                    modifiers: KeyModifiers::NONE,
                }),
                &mut state,
            ) {
                state.dispatch(a);
            }
        }
        assert!(
            (state.splitters.ratio(splitter_id) - expected_default).abs() < 1e-4,
            "reset for {splitter_id:?} expected {expected_default}, got {}",
            state.splitters.ratio(splitter_id)
        );
    }
}

#[test]
fn console_overscroll_clamps_reverses_and_resizes_cleanly() {
    let mut state = AppState::new_with_config(
        PathBuf::from("."),
        None,
        crate::config::AppConfig::load_defaults(),
    );
    state.console_entries.push(GitConsoleEntry {
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
    state.dispatch(Action::SwitchTab(WorkspaceTab::Console));

    let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
    terminal.draw(|f| render_app(f, &mut state)).unwrap();
    let bound = state.scroll_metrics.console_output.expect("console bound recorded");
    assert!(bound.max_scroll > 0);

    for _ in 0..100 {
        state.dispatch(Action::ScrollConsoleOutput(3));
    }
    assert_eq!(state.console_output_scroll, bound.max_scroll);

    state.dispatch(Action::ScrollConsoleOutput(-3));
    assert_eq!(state.console_output_scroll, bound.max_scroll.saturating_sub(3));

    state.dispatch(Action::ScrollConsoleOutput(isize::MAX / 2));
    assert_eq!(state.console_output_scroll, bound.max_scroll);
    terminal.backend_mut().resize(120, 50);
    terminal.draw(|f| render_app(f, &mut state)).unwrap();
    let taller_bound = state.scroll_metrics.console_output.expect("taller bound recorded");
    assert!(taller_bound.max_scroll < bound.max_scroll);
    assert_eq!(state.console_output_scroll, taller_bound.max_scroll);
}

#[test]
fn log_diff_double_goto_bottom_no_overflow_and_reverses_immediately() {
    let mut state = AppState::new_with_config(
        PathBuf::from("."),
        None,
        crate::config::AppConfig::load_defaults(),
    );
    let oid = Oid::from_bytes([1u8; 20]);
    let changes = vec![FileChange {
        path: "lib.rs".into(),
        old_path: None,
        kind: FileStatusKind::Modified,
        additions: 60,
        deletions: 0,
        is_binary: false,
        exists_in_workdir: true,
        old_oid: None,
        new_oid: None,
    }];
    let details = jbgit_core::CommitDetails {
        oid,
        parents: vec![],
        author: PersonStamp {
            name: "Dev".into(),
            email: "dev@example.com".into(),
            time: 1_700_000_000,
            offset_minutes: 0,
        },
        committer: PersonStamp {
            name: "Dev".into(),
            email: "dev@example.com".into(),
            time: 1_700_000_000,
            offset_minutes: 0,
        },
        message: "feat: sample".into(),
        signature: SignatureStatus::Unsigned,
    };
    let key = jbgit_core::DetailsKey::single(oid);
    state.details.cache.insert(
        key.clone(),
        Arc::new(jbgit_core::CommitChangesBundle {
            key: key.clone(),
            details: vec![details],
            changes,
        }),
    );
    let diff_key = FileDiffKey {
        details: key.clone(),
        path: "lib.rs".into(),
        old_path: None,
        context: state.details.diff_context,
    };
    let lines: Vec<DiffLine> = (0..80)
        .map(|i| DiffLine {
            kind: DiffLineKind::Added,
            old_lineno: None,
            new_lineno: Some(i + 1),
            text: format!("let v{i} = {i};"),
            word_ranges: vec![],
        })
        .collect();
    state.details.diff_cache.insert(
        diff_key.clone(),
        Arc::new(FileDiff {
            key: diff_key,
            is_binary: false,
            is_truncated: false,
            is_too_large: false,
            hunks: vec![DiffHunk {
                header: "@@ -0,0 +1,80 @@".into(),
                lines,
            }],
        }),
    );
    state.details.current_key = Some(key);
    state.details.diff_preview = true;
    state.active_tab = WorkspaceTab::Log;
    state.focused_pane = PaneId::LogDetailsDiff;

    let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
    terminal.draw(|f| render_app(f, &mut state)).unwrap();
    let bound = state.scroll_metrics.log_diff.expect("log_diff bound recorded");
    assert!(bound.max_scroll > 0);

    state.dispatch(Action::ScrollDiff(isize::MAX / 2));
    state.dispatch(Action::ScrollDiff(isize::MAX / 2));
    assert_eq!(state.details.diff_scroll, bound.max_scroll);

    state.dispatch(Action::ScrollDiff(-1));
    assert_eq!(state.details.diff_scroll, bound.max_scroll - 1);
}

#[test]
fn console_output_detail_navigation_keys_work() {
    let mut state = AppState::new_with_config(
        PathBuf::from("."),
        None,
        crate::config::AppConfig::load_defaults(),
    );
    state.console_entries.push(GitConsoleEntry {
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
    state.dispatch(Action::SwitchTab(WorkspaceTab::Console));
    state.dispatch(Action::FocusPane(PaneId::ConsoleOutputDetail));

    let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
    terminal.draw(|f| render_app(f, &mut state)).unwrap();
    let bound = state.scroll_metrics.console_output.expect("console bound recorded");
    assert!(bound.viewport_h > 10);
    assert!(bound.max_scroll > bound.viewport_h);

    let send_key = |state: &mut AppState, code: KeyCode, modifiers: KeyModifiers| {
        let actions = route_terminal_event(
            Event::Key(KeyEvent::new(code, modifiers)),
            state,
        );
        for a in actions {
            state.dispatch(a);
        }
    };

    send_key(&mut state, KeyCode::PageDown, KeyModifiers::NONE);
    let expected_page = bound.viewport_h.saturating_sub(1);
    assert_eq!(state.console_output_scroll, expected_page);

    send_key(&mut state, KeyCode::PageUp, KeyModifiers::NONE);
    assert_eq!(state.console_output_scroll, 0);

    send_key(&mut state, KeyCode::Char('d'), KeyModifiers::CONTROL);
    let expected_half = (bound.viewport_h / 2).max(1);
    assert_eq!(state.console_output_scroll, expected_half);
    send_key(&mut state, KeyCode::Char('u'), KeyModifiers::CONTROL);
    assert_eq!(state.console_output_scroll, 0);

    send_key(&mut state, KeyCode::Char('G'), KeyModifiers::SHIFT);
    assert_eq!(state.console_output_scroll, bound.max_scroll);
    send_key(&mut state, KeyCode::Char('g'), KeyModifiers::NONE);
    send_key(&mut state, KeyCode::Char('g'), KeyModifiers::NONE);
    assert_eq!(state.console_output_scroll, 0);

    send_key(&mut state, KeyCode::End, KeyModifiers::NONE);
    assert_eq!(state.console_output_scroll, bound.max_scroll);
    send_key(&mut state, KeyCode::Home, KeyModifiers::NONE);
    assert_eq!(state.console_output_scroll, 0);

    let unbind_gb: crate::config::KeymapConfigOverlay =
        toml::from_str("[global]\nprepend_keymap = [{ on = [\"g\", \"b\"], run = \"noop\" }]\n")
            .unwrap();
    state.config.keymap.merge_overlay(&unbind_gb);
    send_key(&mut state, KeyCode::End, KeyModifiers::NONE);
    assert_eq!(state.console_output_scroll, bound.max_scroll);
    send_key(&mut state, KeyCode::Char('g'), KeyModifiers::NONE);
    send_key(&mut state, KeyCode::Char('g'), KeyModifiers::NONE);
    assert_eq!(state.console_output_scroll, 0);

    let unbind_gg: crate::config::KeymapConfigOverlay =
        toml::from_str("[console]\nprepend_keymap = [{ on = [\"g\", \"g\"], run = \"noop\" }]\n")
            .unwrap();
    state.config.keymap.merge_overlay(&unbind_gg);
    send_key(&mut state, KeyCode::End, KeyModifiers::NONE);
    assert_eq!(state.console_output_scroll, bound.max_scroll);
    send_key(&mut state, KeyCode::Char('g'), KeyModifiers::NONE);
    send_key(&mut state, KeyCode::Char('g'), KeyModifiers::NONE);
    assert_eq!(state.console_output_scroll, bound.max_scroll);
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
    state.stage_workbench.editor.set_text("feat: keep message");
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
    assert_eq!(state.stage_workbench.editor.text(), "feat: keep message");
}

#[test]
fn commit_tab_diff_lru_cache_invalidation_debounce_and_effect_coalescing() {
    let mut state = AppState::new_with_config(
        PathBuf::from("."),
        None,
        crate::config::AppConfig::load_defaults(),
    );
    state.stage_workbench.tree_opts.group_by_dir = false;
    state.dispatch(Action::SwitchTab(WorkspaceTab::Commit));

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
    let init_effs = state.apply_engine_event(EngineEvent::StageStatusLoaded {
        status,
        epoch: 1,
    });
    assert!(init_effs.iter().any(|e| matches!(
        e,
        Effect::RequestStageFileDiff(k) if k.path == "a.rs"
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
            },
            vcs_exists: true,
            staged_exists: true,
            local_exists: true,
            vcs_bytes: vcs.to_vec(),
            staged_bytes: staged.to_vec(),
            local_bytes: local.to_vec(),
            file_mode: 0o100644,
            is_binary: false,
            is_conflicted: false,
            tracker,
            hunks,
            is_truncated,
            is_too_large: false,
        })
    };

    state.dispatch(Action::SelectCommitStageRow(1));
    for step in 0..20 {
        let delta = if step % 2 == 0 { 1 } else { -1 };
        let effs = state.dispatch(Action::ScrollCommitStage(delta));
        assert!(
            !effs.iter().any(|e| matches!(e, Effect::RequestStageFileDiff(_))),
            "keyboard scroll must debounce stage diff request"
        );
    }
    let effs = state.dispatch(Action::ScrollCommitStage(1));
    assert!(!effs.iter().any(|e| matches!(e, Effect::RequestStageFileDiff(_))));
    assert!(state.stage_workbench.pending_diff.is_some());

    let mut flushed = Vec::new();
    state.flush_pending_at(
        Instant::now() + crate::log_view::DETAILS_DEBOUNCE + Duration::from_millis(5),
        &mut flushed,
    );
    assert!(matches!(
        &flushed[..],
        [Effect::RequestStageFileDiff(k)] if k.path == "b.rs"
    ));

    state.dispatch(Action::SelectCommitStageRow(1));
    state.apply_engine_event(EngineEvent::StageFileDiffLoaded {
        diff: make_stage_diff("a.rs"),
        epoch: 2,
    });
    state.dispatch(Action::SelectCommitStageRow(2));
    state.apply_engine_event(EngineEvent::StageFileDiffLoaded {
        diff: make_stage_diff("b.rs"),
        epoch: 3,
    });

    let back_to_a = state.dispatch(Action::SelectCommitStageRow(1));
    assert!(!back_to_a.iter().any(|e| matches!(e, Effect::RequestStageFileDiff(_))));
    assert_eq!(
        state
            .stage_workbench
            .current_diff
            .as_ref()
            .map(|d| d.key.path.as_str()),
        Some("a.rs")
    );

    state.stage_workbench.diff_cursor_row = 1;
    let _ = state.dispatch(Action::StageDiffStageSelection);
    let key_a = StageDiffKey {
        path: "a.rs".into(),
        old_path: None,
        side: StageDiffSide::Unstaged,
        context: DiffContext::Three,
    };
    let key_b = StageDiffKey {
        path: "b.rs".into(),
        old_path: None,
        side: StageDiffSide::Unstaged,
        context: DiffContext::Three,
    };
    assert!(!state.stage_workbench.diff_cache.contains(&key_a));
    assert!(state.stage_workbench.diff_cache.contains(&key_b));

    let mut raw = Vec::new();
    for i in 0..10 {
        raw.push(Effect::RequestStageFileDiff(StageDiffKey {
            path: format!("f{i}.rs"),
            old_path: None,
            side: StageDiffSide::Unstaged,
            context: DiffContext::Three,
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

    state.dispatch(Action::SelectCommitStageRow(2));
    assert!(state.stage_workbench.diff_cache.contains(&key_b));
    let same_status = Arc::clone(state.stage_status.as_ref().unwrap());
    let reload_effs = state.apply_engine_event(EngineEvent::StageStatusLoaded {
        status: Arc::new((*same_status).clone()),
        epoch: 4,
    });
    assert!(
        !state.stage_workbench.diff_cache.contains(&key_b),
        "identical StageStatusLoaded must still invalidate cached diffs after external edits"
    );
    assert!(reload_effs.iter().any(|e| matches!(
        e,
        Effect::RequestStageFileDiff(k) if k.path == "b.rs"
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
