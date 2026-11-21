use std::path::PathBuf;
use std::sync::Arc;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use jbgit_core::{
    BranchFilter, ChangeRowKind, CommitChangesBundle, CommitDetails, CommitSummary, DetailsKey,
    DiffContext, DiffHunk, DiffLine, DiffLineKind, FileChange, FileDiff, FileDiffKey,
    FileHistoryBundle, FileHistoryEntry, FileHistoryKey, FileStatusKind, GitOp, Oid, PaneId,
    PersonStamp, RefInfo, RefKind, RepoSnapshot, RepositoryState, SignatureStatus, StageEntry,
    StageStatusSnapshot, WorkspaceTab,
};
use jbgit_engine::EngineEvent;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;

use crate::app::{
    Action, AppState, ContextMenuAction, ContextMenuEntryKind, ContextMenuItem, Effect,
    ModalLayer, PopupKind, ScrollPane,
};
use crate::config::AppConfig;
use crate::dialogs::Dialog;
use crate::hit_test::HitTarget;
use crate::i18n::I18n;
use crate::input::{route_key, route_mouse};
use crate::log_view::FileHistoryFocus;
use crate::menus::stage_tree_menu_items;
use crate::views::render_app;

fn oid_n(n: u8) -> Oid {
    let mut bytes = [0u8; 20];
    bytes[0] = n;
    bytes[19] = n;
    Oid::from_bytes(bytes)
}

fn make_commit(n: u8, parents: Vec<Oid>, subject: &str) -> CommitSummary {
    CommitSummary {
        oid: oid_n(n),
        parents,
        author_name: "Alice".to_string(),
        author_email: "alice@example.com".to_string(),
        timestamp: 1_700_000_000 + i64::from(n) * 60,
        author_timestamp: 1_700_000_000 + i64::from(n) * 60,
        subject: subject.to_string(),
        body: String::new(),
    }
}

fn make_details(oid: Oid, parents: Vec<Oid>, subject: &str) -> CommitDetails {
    CommitDetails {
        oid,
        parents,
        author: PersonStamp {
            name: "Alice".into(),
            email: "alice@example.com".into(),
            time: 1_700_000_000,
            offset_minutes: 0,
        },
        committer: PersonStamp {
            name: "Alice".into(),
            email: "alice@example.com".into(),
            time: 1_700_000_000,
            offset_minutes: 0,
        },
        message: subject.into(),
        signature: SignatureStatus::default(),
    }
}

fn make_snapshot(workdir: PathBuf, commits: Vec<CommitSummary>) -> Arc<RepoSnapshot> {
    let head_oid = commits.first().map(|c| c.oid);
    let mut refs = Vec::new();
    if let Some(h) = head_oid {
        refs.push(RefInfo {
            name: "main".into(),
            full_name: "refs/heads/main".into(),
            target_oid: h,
            kind: RefKind::LocalBranch,
            upstream: None,
            ahead_behind: None,
            is_head: true,
        });
    }
    Arc::new(RepoSnapshot {
        repo_name: "spec09-test".into(),
        workdir,
        git_dir: PathBuf::new(),
        common_dir: PathBuf::new(),
        state: RepositoryState::Clean,
        is_unborn_head: commits.is_empty(),
        head_branch: head_oid.map(|_| "main".into()),
        head_oid,
        refs,
        commits,
        user_name: Some("Alice".into()),
        user_email: Some("alice@example.com".into()),
        recent_branches: vec!["main".into()],
        stashes: Vec::new(),
        ongoing_onto: None,
    })
}

fn test_state(workdir: PathBuf) -> AppState {
    let config = AppConfig::default();
    let mut state = AppState::new_with_config(workdir, None, config);
    state.i18n = Arc::new(I18n::load("en-US", None));
    state
}

fn render_to_buffer(state: &mut AppState, width: u16, height: u16) -> Buffer {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).expect("test backend");
    terminal
        .draw(|frame| render_app(frame, state))
        .expect("draw");
    terminal.backend().buffer().clone()
}

fn buffer_text(buf: &Buffer) -> String {
    let mut out = String::new();
    for y in 0..buf.area.height {
        for x in 0..buf.area.width {
            out.push_str(buf[(x, y)].symbol());
        }
        out.push('\n');
    }
    out
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn send_key(state: &mut AppState, code: KeyCode, mods: KeyModifiers) -> Vec<Effect> {
    let acts = route_key(KeyEvent::new(code, mods), state);
    dispatch_all(state, acts)
}

fn left_click(col: u16, row: u16, mods: KeyModifiers) -> MouseEvent {
    MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: col,
        row,
        modifiers: mods,
    }
}

fn right_click(col: u16, row: u16) -> MouseEvent {
    MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Right),
        column: col,
        row,
        modifiers: KeyModifiers::NONE,
    }
}

fn hit_center_of(state: &AppState, pred: impl Fn(&HitTarget) -> bool) -> Option<(u16, u16)> {
    state
        .hit_registry
        .find(pred)
        .map(|r| (r.rect.x + r.rect.width / 2, r.rect.y + r.rect.height / 2))
}

fn dispatch_all(state: &mut AppState, actions: Vec<Action>) -> Vec<Effect> {
    let mut out = Vec::new();
    for a in actions {
        out.extend(state.dispatch(a));
    }
    out
}

fn menu_item_by_label<'a>(items: &'a [ContextMenuItem], substr: &str) -> &'a ContextMenuItem {
    items
        .iter()
        .find(|i| !i.is_separator() && i.label.contains(substr))
        .unwrap_or_else(|| {
            let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
            panic!("menu item containing '{substr}' not found in {labels:?}");
        })
}

fn item_run_actions(item: &ContextMenuItem) -> Vec<Action> {
    match &item.kind {
        ContextMenuEntryKind::Action(ContextMenuAction::Run(actions)) => actions.clone(),
        other => panic!("expected Run item, got {other:?}"),
    }
}

#[test]
fn log_changes_context_menu_right_click_keyboard_dot_and_enablement() {
    let dir = tempfile::tempdir().unwrap();
    let workdir = dir.path().to_path_buf();
    std::fs::create_dir_all(workdir.join("dir with space")).unwrap();
    std::fs::write(workdir.join("dir with space/a.rs"), "pub fn a() {}\n").unwrap();

    let c1 = oid_n(1);
    let c2 = oid_n(2);
    let mut state = test_state(workdir.clone());
    state.repo_snapshot = Some(make_snapshot(
        workdir,
        vec![
            make_commit(2, vec![c1], "modify files"),
            make_commit(1, vec![], "init"),
        ],
    ));
    state.active_tab = WorkspaceTab::Log;
    state.details.changes_group_by_dir = true;

    let key_single = DetailsKey::commits(vec![c2], 0);
    let bundle_single = Arc::new(CommitChangesBundle {
        key: key_single.clone(),
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
    let _ = state.apply_engine_event(EngineEvent::CommitDetailsLoaded {
        bundle: bundle_single,
        epoch: 1,
    });
    let _ = render_to_buffer(&mut state, 140, 40);

    let rows = state.details.change_rows();
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
    let (rx, ry) = hit_center_of(&state, |t| matches!(t, HitTarget::LogChangeRow { idx } if *idx == present_idx))
        .expect("hit target for present file");
    let acts = route_mouse(right_click(rx, ry), &mut state);
    let _ = dispatch_all(&mut state, acts);
    let items = match state.modal_stack.last() {
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
    let _ = state.dispatch(Action::PopModal);

    // 2. Right-click on file missing from worktree disables Open in External Editor.
    let (gx, gy) = hit_center_of(&state, |t| matches!(t, HitTarget::LogChangeRow { idx } if *idx == gone_idx))
        .expect("hit target for gone file");
    let acts = route_mouse(right_click(gx, gy), &mut state);
    let _ = dispatch_all(&mut state, acts);
    let items = match state.modal_stack.last() {
        Some(ModalLayer::ContextMenu { items, .. }) => items.clone(),
        _ => panic!("expected menu"),
    };
    let ed_item = menu_item_by_label(&items, "Open in External Editor");
    assert!(!ed_item.is_enabled());
    assert_eq!(
        ed_item.disabled_reason.as_deref(),
        Some(state.i18n.t("menu.log_change.not_in_workdir"))
    );
    let _ = state.dispatch(Action::PopModal);

    // 3. Deleted file disables Get Version.
    let (dx, dy) = hit_center_of(&state, |t| matches!(t, HitTarget::LogChangeRow { idx } if *idx == deleted_idx))
        .expect("hit target for deleted file");
    let acts = route_mouse(right_click(dx, dy), &mut state);
    let _ = dispatch_all(&mut state, acts);
    let items = match state.modal_stack.last() {
        Some(ModalLayer::ContextMenu { items, .. }) => items.clone(),
        _ => panic!("expected menu"),
    };
    let restore = menu_item_by_label(&items, "Get Version");
    assert!(!restore.is_enabled());
    assert_eq!(
        restore.disabled_reason.as_deref(),
        Some(state.i18n.t("menu.log_change.cannot_restore_deleted"))
    );
    let _ = state.dispatch(Action::PopModal);

    // 4. Directory row via keyboard `.` in PaneId::LogChangesTree.
    let _ = state.dispatch(Action::FocusPane(PaneId::LogChangesTree));
    let _ = state.dispatch(Action::ChangesSelectRow(dir_idx));
    let acts = route_key(key(KeyCode::Char('.')), &mut state);
    let _ = dispatch_all(&mut state, acts);
    let items = match state.modal_stack.last() {
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
            Some(state.i18n.t("menu.log_change.need_file"))
        );
    }
    let _ = state.dispatch(Action::PopModal);

    // 5. Multi-commit selection disables single-commit operations.
    let _ = state.dispatch(Action::FocusPane(PaneId::LogCommitTable));
    let _ = state.dispatch(Action::SelectLogRow(0));
    let _ = state.dispatch(Action::ExtendLogSelectionTo(1));
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
    let _ = state.apply_engine_event(EngineEvent::CommitDetailsLoaded {
        bundle: bundle_multi,
        epoch: 2,
    });
    let _ = render_to_buffer(&mut state, 140, 40);
    let _ = state.dispatch(Action::FocusPane(PaneId::LogChangesTree));
    let _ = state.dispatch(Action::ChangesSelectRow(1));
    let acts = route_key(key(KeyCode::Char('.')), &mut state);
    let _ = dispatch_all(&mut state, acts);
    let items = match state.modal_stack.last() {
        Some(ModalLayer::ContextMenu { items, .. }) => items.clone(),
        _ => panic!("expected menu"),
    };
    for single_only in ["Compare with Local", "Get Version", "Copy Patch of File"] {
        let item = menu_item_by_label(&items, single_only);
        assert!(!item.is_enabled(), "{single_only} must be disabled for multi-commit");
        assert_eq!(
            item.disabled_reason.as_deref(),
            Some(state.i18n.t("menu.log_change.need_single_commit"))
        );
    }
    // Show History for Selected File works with multi-commit (falls back to HEAD history).
    assert!(menu_item_by_label(&items, "Show History for Selected File").is_enabled());
}

#[test]
fn filter_log_by_path_preserves_spaces_and_requests_matches() {
    let dir = tempfile::tempdir().unwrap();
    let workdir = dir.path().to_path_buf();
    let _c1 = oid_n(1);
    let mut state = test_state(workdir.clone());
    state.repo_snapshot = Some(make_snapshot(workdir, vec![make_commit(1, vec![], "init")]));

    let effects = state.dispatch(Action::FilterLogByPath("dir with space/a.rs".into()));
    assert_eq!(
        state.log_filter.spec.paths.paths,
        vec!["dir with space/a.rs".to_string()]
    );
    assert!(
        state
            .status_notice
            .as_deref()
            .is_some_and(|n| n.contains("dir with space/a.rs"))
    );
    assert!(effects.iter().any(|e| matches!(
        e,
        Effect::RequestPathMatches(paths, _) if paths == &vec!["dir with space/a.rs".to_string()]
    )));
}

#[test]
fn compare_file_with_local_expands_collapsed_dir_cache_hit_and_missing_toast() {
    let dir = tempfile::tempdir().unwrap();
    let workdir = dir.path().to_path_buf();
    let c1 = oid_n(1);
    let c2 = oid_n(2);
    let mut state = test_state(workdir.clone());
    state.repo_snapshot = Some(make_snapshot(
        workdir,
        vec![make_commit(2, vec![c1], "c2"), make_commit(1, vec![], "c1")],
    ));
    state.details.changes_group_by_dir = true;
    state.details.changes_collapsed.insert("dir with space".into());

    let effects = state.dispatch(Action::CompareFileWithLocal {
        target: c1,
        path: "dir with space/a.rs".into(),
    });
    assert!(state.details.diff_preview);
    assert_eq!(
        state.details.compare_override,
        Some(DetailsKey::compare_with_local(c1))
    );
    assert!(effects.iter().any(|e| matches!(
        e,
        Effect::RequestCommitDetails(k) if *k == DetailsKey::compare_with_local(c1)
    )));

    // Engine returns local comparison bundle containing `dir with space/a.rs`.
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
    let _ = state.apply_engine_event(EngineEvent::CommitDetailsLoaded {
        bundle: cmp_bundle,
        epoch: 1,
    });
    assert!(!state.details.changes_collapsed.contains("dir with space"));
    assert_eq!(
        state.details.selected_change().map(|c| c.path.as_str()),
        Some("dir with space/a.rs")
    );

    // Re-collapsing + selecting another commit + triggering CompareFileWithLocal hits cache immediately.
    let _ = state.dispatch(Action::SelectLogRow(0));
    state.details.changes_collapsed.insert("dir with space".into());
    let _ = state.dispatch(Action::CompareFileWithLocal {
        target: c1,
        path: "dir with space/a.rs".into(),
    });
    assert!(!state.details.changes_collapsed.contains("dir with space"));
    assert_eq!(
        state.details.selected_change().map(|c| c.path.as_str()),
        Some("dir with space/a.rs")
    );

    // When target path is missing from local comparison bundle, toast is shown.
    let _ = state.dispatch(Action::CompareFileWithLocal {
        target: c2,
        path: "identical.rs".into(),
    });
    let empty_cmp = Arc::new(CommitChangesBundle {
        key: DetailsKey::compare_with_local(c2),
        details: vec![make_details(c2, vec![c1], "c2")],
        changes: Vec::new(),
    });
    let _ = state.apply_engine_event(EngineEvent::CommitDetailsLoaded {
        bundle: empty_cmp,
        epoch: 2,
    });
    assert!(
        state
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
    let mut state = test_state(workdir.clone());
    state.repo_snapshot = Some(make_snapshot(workdir, vec![make_commit(1, vec![], "init")]));
    state.details.changes_group_by_dir = false;
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
    let _ = state.apply_engine_event(EngineEvent::CommitDetailsLoaded {
        bundle,
        epoch: 1,
    });

    let items = crate::menus::log_change_menu_items(&state);
    let restore = menu_item_by_label(&items, "Get Version");
    let _ = dispatch_all(&mut state, item_run_actions(restore));
    match state.modal_stack.last() {
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

    // Destructive confirm defaults to Cancel (1); Left moves to Restore (0), then Enter confirms.
    let _ = send_key(&mut state, KeyCode::Left, KeyModifiers::NONE);
    assert!(matches!(
        state.modal_stack.last(),
        Some(ModalLayer::Dialog(d)) if matches!(d.as_ref(), Dialog::Choice(c) if c.selected == 0)
    ));
    let effects = send_key(&mut state, KeyCode::Enter, KeyModifiers::NONE);
    assert!(state.modal_stack.is_empty());
    assert!(effects.iter().any(|e| matches!(
        e,
        Effect::GitOp(GitOp::CheckoutPaths { rev, paths })
            if rev == &c1.to_string() && paths == &vec!["src/lib.rs".to_string()]
    )));
}

#[test]
fn commit_stage_tree_history_menu_and_h_shortcut() {
    let dir = tempfile::tempdir().unwrap();
    let workdir = dir.path().to_path_buf();
    let c1 = oid_n(1);
    let mut state = test_state(workdir.clone());
    state.repo_snapshot = Some(make_snapshot(workdir, vec![make_commit(1, vec![], "init")]));
    state.active_tab = WorkspaceTab::Commit;
    state.focused_pane = PaneId::CommitChangesTree;
    state.stage_workbench.tree_opts.show_ignored = true;

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
    };
    let _ = state.apply_engine_event(EngineEvent::StageStatusLoaded {
        status: Arc::new(status),
        epoch: 1,
    });

    let tracked_idx = state
        .stage_workbench
        .rows
        .iter()
        .position(|r| r.is_file() && r.key.ends_with("tracked.rs"))
        .expect("tracked row");
    let untracked_idx = state
        .stage_workbench
        .rows
        .iter()
        .position(|r| r.is_file() && r.key.ends_with("new_untracked.rs"))
        .expect("untracked row");
    let ignored_idx = state
        .stage_workbench
        .rows
        .iter()
        .position(|r| r.is_file() && r.key.ends_with("build.log"))
        .expect("ignored row");

    let tracked_menu = stage_tree_menu_items(&state, tracked_idx);
    assert!(menu_item_by_label(&tracked_menu, "Show File History").is_enabled());

    for bad_idx in [untracked_idx, ignored_idx] {
        let menu = stage_tree_menu_items(&state, bad_idx);
        let hist = menu_item_by_label(&menu, "Show File History");
        assert!(!hist.is_enabled());
        assert_eq!(
            hist.disabled_reason.as_deref(),
            Some(state.i18n.t("menu.stage.only_tracked_history"))
        );
    }

    // Pressing H on untracked row sets notice and keeps modal closed.
    state.commit_stage_selected_idx = untracked_idx;
    let acts = route_key(key(KeyCode::Char('H')), &mut state);
    let _ = dispatch_all(&mut state, acts);
    assert!(state.file_history.is_none());
    assert_eq!(
        state.status_notice.as_deref(),
        Some(state.i18n.t("menu.stage.only_tracked_history"))
    );

    // Pressing H on tracked row opens FileHistory starting at HEAD (start_oid: None).
    state.commit_stage_selected_idx = tracked_idx;
    let acts = route_key(key(KeyCode::Char('H')), &mut state);
    let effs = dispatch_all(&mut state, acts);
    assert!(matches!(
        state.modal_stack.last(),
        Some(ModalLayer::FloatingPopup {
            kind: PopupKind::FileHistory,
            ..
        })
    ));
    assert!(effs.iter().any(|e| matches!(
        e,
        Effect::RequestFileHistory(k)
            if k.path == "tracked.rs" && k.start_oid.is_none() && k.follow_renames
    )));
    let _ = state.dispatch(Action::FileHistoryEscape);

    // Pressing H in [2 Log] Changes tree: file opens FileHistory, dir filters Log.
    state.active_tab = WorkspaceTab::Log;
    state.focused_pane = PaneId::LogChangesTree;
    state.details.changes_group_by_dir = true;
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
    let _ = state.apply_engine_event(EngineEvent::CommitDetailsLoaded { bundle, epoch: 2 });

    // Row 0 is dir "sub", row 1 is file "sub/item.rs".
    let _ = state.dispatch(Action::ChangesSelectRow(1));
    let effs = send_key(&mut state, KeyCode::Char('H'), KeyModifiers::NONE);
    assert!(effs.iter().any(|e| matches!(
        e,
        Effect::RequestFileHistory(k)
            if k.path == "sub/item.rs" && k.start_oid == Some(c1) && k.follow_renames
    )));
    let _ = state.dispatch(Action::FileHistoryEscape);

    let _ = state.dispatch(Action::ChangesSelectRow(0));
    let effs = send_key(&mut state, KeyCode::Char('H'), KeyModifiers::NONE);
    assert_eq!(state.log_filter.spec.paths.paths, vec!["sub".to_string()]);
    assert!(effs.iter().any(|e| matches!(e, Effect::RequestPathMatches(..))));
}

#[test]
fn file_history_modal_end_to_end_renames_compare_navigation_and_scroll() {
    let dir = tempfile::tempdir().unwrap();
    let workdir = dir.path().to_path_buf();
    let c1 = oid_n(1);
    let c2 = oid_n(2);
    let c3 = oid_n(3);
    let mut state = test_state(workdir.clone());
    state.repo_snapshot = Some(make_snapshot(
        workdir,
        vec![
            make_commit(3, vec![c2], "rename mid -> new"),
            make_commit(2, vec![c1], "rename old -> mid"),
            make_commit(1, vec![], "create old"),
        ],
    ));
    state.active_tab = WorkspaceTab::Log;
    state.focused_pane = PaneId::LogChangesTree;
    state.log_filter.spec.branch = BranchFilter::parse("main");

    let open_effects = state.dispatch(Action::OpenFileHistory {
        path: "new_name.rs".into(),
        start_oid: Some(c2),
        follow_renames: true,
    });
    assert!(matches!(
        state.modal_stack.last(),
        Some(ModalLayer::FloatingPopup {
            kind: PopupKind::FileHistory,
            ..
        })
    ));
    assert!(open_effects.iter().any(|e| matches!(
        e,
        Effect::RequestFileHistory(k)
            if k.path == "new_name.rs" && k.start_oid == Some(c2) && k.follow_renames
    )));

    // Deliver rename chain history; `start_oid == Some(c2)` must preselect index 1.
    let hist_key = FileHistoryKey {
        path: "new_name.rs".into(),
        start_oid: Some(c2),
        follow_renames: true,
    };
    let hist_bundle = Arc::new(FileHistoryBundle {
        key: hist_key.clone(),
        entries: vec![
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
            },
        ],
        truncated: false,
    });
    let effs = state.apply_engine_event(EngineEvent::FileHistoryLoaded {
        bundle: hist_bundle,
        epoch: 1,
    });
    assert_eq!(state.file_history.as_ref().unwrap().selected, 1);
    let expected_diff_key = FileDiffKey {
        details: DetailsKey::single(c2),
        path: "mid_name.rs".into(),
        old_path: Some("old_name.rs".into()),
        context: DiffContext::Three,
    };
    assert!(effs.iter().any(|e| matches!(
        e,
        Effect::RequestFileDiff(k) if *k == expected_diff_key
    )));

    // Provide multiline diff for scroll & render testing.
    let diff = FileDiff {
        key: expected_diff_key.clone(),
        hunks: vec![DiffHunk {
            header: "@@ -1,35 +1,35 @@".into(),
            lines: (1..=35)
                .map(|i| DiffLine {
                    kind: DiffLineKind::Added,
                    old_lineno: None,
                    new_lineno: Some(i),
                    text: format!("line {i}"),
                    word_ranges: Vec::new(),
                })
                .collect(),
        }],
        is_binary: false,
        is_truncated: false,
        is_too_large: false,
    };
    let _ = state.apply_engine_event(EngineEvent::FileDiffLoaded {
        diff: Arc::new(diff),
        epoch: 1,
    });

    // Wide layout (horizontal split): rename badges, author, date + HitTargets render cleanly.
    let wide = buffer_text(&render_to_buffer(&mut state, 140, 40));
    assert!(wide.contains("← old_name.rs"));
    assert!(wide.contains("@ mid_name.rs") || wide.contains("← mid_name.rs"));
    assert!(wide.contains("Alice"), "author should render at 140x40:\n{wide}");
    assert!(
        wide.contains("2023-"),
        "commit date should render at 140x40:\n{wide}"
    );
    assert!(hit_center_of(&state, |t| matches!(t, HitTarget::FileHistoryRow { idx: 0 })).is_some());
    assert!(hit_center_of(&state, |t| matches!(t, HitTarget::FileHistoryListArea)).is_some());
    assert!(hit_center_of(&state, |t| matches!(t, HitTarget::FileHistoryDiffArea)).is_some());
    assert!(hit_center_of(&state, |t| matches!(t, HitTarget::FileHistoryToolbar(_))).is_some());

    // Narrow layout (<100 inner width triggers vertical stack): renders cleanly without panic.
    let _ = render_to_buffer(&mut state, 80, 24);
    let _ = render_to_buffer(&mut state, 140, 40);

    // `f` toggles follow_renames and requests fresh FileHistory.
    let effs = send_key(&mut state, KeyCode::Char('f'), KeyModifiers::NONE);
    assert!(!state.file_history.as_ref().unwrap().key.follow_renames);
    assert!(effs.iter().any(|e| matches!(
        e,
        Effect::RequestFileHistory(k) if !k.follow_renames && k.path == "new_name.rs"
    )));
    // Restore follow_renames back on & re-deliver bundle.
    let _ = send_key(&mut state, KeyCode::Char('f'), KeyModifiers::NONE);
    let _ = state.apply_engine_event(EngineEvent::FileHistoryLoaded {
        bundle: Arc::new(FileHistoryBundle {
            key: hist_key,
            entries: vec![
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
                },
            ],
            truncated: false,
        }),
        epoch: 2,
    });

    // `l` toggles compare_with_local against repo path.
    let _ = send_key(&mut state, KeyCode::Char('l'), KeyModifiers::NONE);
    let cmp_local_key = state
        .file_history
        .as_ref()
        .unwrap()
        .desired_diff_key()
        .expect("local diff key");
    assert_eq!(cmp_local_key.details, DetailsKey::compare_with_local(c2));
    assert_eq!(cmp_local_key.path, "new_name.rs");
    assert_eq!(cmp_local_key.old_path.as_deref(), Some("mid_name.rs"));
    let _ = send_key(&mut state, KeyCode::Char('l'), KeyModifiers::NONE);

    // `Shift+Down` extends selection across rename boundary (c1 -> c2).
    let _ = send_key(&mut state, KeyCode::Down, KeyModifiers::SHIFT);
    let fh = state.file_history.as_ref().unwrap();
    assert_eq!(fh.anchor, Some(1));
    assert_eq!(fh.selected, 2);
    let cmp_two = fh.desired_diff_key().expect("compare versions diff key");
    assert_eq!(cmp_two.details, DetailsKey::compare_versions(c1, c2));
    assert_eq!(cmp_two.path, "mid_name.rs");
    assert_eq!(cmp_two.old_path.as_deref(), Some("old_name.rs"));
    let cmp_screen = buffer_text(&render_to_buffer(&mut state, 140, 40));
    assert!(
        cmp_screen.contains("old_name.rs → mid_name.rs"),
        "cross-rename comparison header must show old → new path:\n{cmp_screen}"
    );

    // `gg` jumps to top in list focus, then Shift+Click on row 0 from row 1 compares c2 -> c3.
    let _ = send_key(&mut state, KeyCode::Char('g'), KeyModifiers::NONE);
    let _ = send_key(&mut state, KeyCode::Char('g'), KeyModifiers::NONE);
    assert_eq!(state.file_history.as_ref().unwrap().selected, 0);
    let _ = send_key(&mut state, KeyCode::Down, KeyModifiers::NONE);
    let (row0_x, row0_y) = hit_center_of(&state, |t| matches!(t, HitTarget::FileHistoryRow { idx: 0 })).unwrap();
    let click_acts = route_mouse(left_click(row0_x, row0_y, KeyModifiers::SHIFT), &mut state);
    let _ = dispatch_all(&mut state, click_acts);
    let fh = state.file_history.as_ref().unwrap();
    assert_eq!(fh.anchor, Some(1));
    assert_eq!(fh.selected, 0);
    let cmp_c2_c3 = fh.desired_diff_key().unwrap();
    assert_eq!(cmp_c2_c3.details, DetailsKey::compare_versions(c2, c3));

    // Plain Down moves selection back onto index 1 (c2) and clears anchor, then verify scroll clamping.
    let _ = send_key(&mut state, KeyCode::Down, KeyModifiers::NONE);
    assert_eq!(state.file_history.as_ref().unwrap().anchor, None);
    let _ = render_to_buffer(&mut state, 140, 24);
    let max_scroll = state
        .scroll_metrics
        .bound(ScrollPane::FileHistoryDiff)
        .expect("FileHistoryDiff bound recorded")
        .max_scroll;
    assert!(max_scroll > 0);

    // Tab to Diff focus, jump to bottom / overscroll, then Up reverses immediately (no dead-zone), and `gg` jumps to top.
    let _ = send_key(&mut state, KeyCode::Tab, KeyModifiers::NONE);
    assert_eq!(
        state.file_history.as_ref().unwrap().focus,
        FileHistoryFocus::Diff
    );
    let _ = send_key(&mut state, KeyCode::End, KeyModifiers::NONE);
    let _ = send_key(&mut state, KeyCode::Down, KeyModifiers::NONE);
    assert_eq!(state.file_history.as_ref().unwrap().diff_scroll, max_scroll);
    let _ = send_key(&mut state, KeyCode::Up, KeyModifiers::NONE);
    assert_eq!(
        state.file_history.as_ref().unwrap().diff_scroll,
        max_scroll.saturating_sub(1)
    );
    let _ = send_key(&mut state, KeyCode::Char('g'), KeyModifiers::NONE);
    let _ = send_key(&mut state, KeyCode::Char('g'), KeyModifiers::NONE);
    assert_eq!(state.file_history.as_ref().unwrap().diff_scroll, 0);

    // `z` toggles zoom, first `Esc` only un-zooms, `Enter` jumps to commit in `[2 Log]`.
    let _ = send_key(&mut state, KeyCode::Char('z'), KeyModifiers::NONE);
    assert!(state.file_history.as_ref().unwrap().diff_zoomed);
    let _ = send_key(&mut state, KeyCode::Esc, KeyModifiers::NONE);
    assert!(!state.file_history.as_ref().unwrap().diff_zoomed);
    assert!(matches!(
        state.modal_stack.last(),
        Some(ModalLayer::FloatingPopup {
            kind: PopupKind::FileHistory,
            ..
        })
    ));
    let _ = send_key(&mut state, KeyCode::Enter, KeyModifiers::NONE);
    assert!(state.file_history.is_none());
    assert!(state.modal_stack.is_empty());
    assert_eq!(state.active_tab, WorkspaceTab::Log);
    assert_eq!(state.focused_pane, PaneId::LogCommitTable);
    assert_eq!(state.log_graph.selected_oids(state.repo_snapshot.as_ref()), vec![c2]);
    assert_eq!(state.log_filter.spec.branch, BranchFilter::parse("main"));

    // Verify `g` also jumps cleanly, and `Esc` preserves prior `LogFilterSpec` and `focused_pane`.
    let _ = state.dispatch(Action::FocusPane(PaneId::LogChangesTree));
    let _ = state.dispatch(Action::OpenFileHistory {
        path: "new_name.rs".into(),
        start_oid: Some(c1),
        follow_renames: true,
    });
    let _ = state.apply_engine_event(EngineEvent::FileHistoryLoaded {
        bundle: Arc::new(FileHistoryBundle {
            key: FileHistoryKey {
                path: "new_name.rs".into(),
                start_oid: Some(c1),
                follow_renames: true,
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
            }],
            truncated: false,
        }),
        epoch: 3,
    });
    let _ = send_key(&mut state, KeyCode::Esc, KeyModifiers::NONE);
    assert!(state.file_history.is_none());
    assert!(state.modal_stack.is_empty());
    assert_eq!(state.focused_pane, PaneId::LogChangesTree);
    assert_eq!(state.log_filter.spec.branch, BranchFilter::parse("main"));

    // And `g` locates the commit in Log.
    let _ = state.dispatch(Action::OpenFileHistory {
        path: "new_name.rs".into(),
        start_oid: Some(c1),
        follow_renames: true,
    });
    let _ = state.apply_engine_event(EngineEvent::FileHistoryLoaded {
        bundle: Arc::new(FileHistoryBundle {
            key: FileHistoryKey {
                path: "new_name.rs".into(),
                start_oid: Some(c1),
                follow_renames: true,
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
            }],
            truncated: false,
        }),
        epoch: 4,
    });
    let _ = send_key(&mut state, KeyCode::Char('g'), KeyModifiers::NONE);
    assert!(state.file_history.is_some());
    state.file_history.as_mut().unwrap().pending_g =
        Some(std::time::Instant::now() - crate::log_view::FILE_HISTORY_G_WINDOW);
    let _ = state.dispatch(Action::FlushPendingDetails);
    assert!(state.file_history.is_none());
    assert!(state.modal_stack.is_empty());
    assert_eq!(state.active_tab, WorkspaceTab::Log);
    assert_eq!(state.focused_pane, PaneId::LogCommitTable);
    assert_eq!(state.log_graph.selected_oids(state.repo_snapshot.as_ref()), vec![c1]);
}

#[test]
fn file_history_modal_uses_configured_layout_ratios_and_supports_splitter_drag_reset() {
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
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

    let mut state = AppState::new_with_config(workdir, None, cfg);
    let _ = state.dispatch(Action::OpenFileHistory {
        path: "src/lib.rs".into(),
        start_oid: None,
        follow_renames: true,
    });

    // Wide terminal: uses FileHistoryHorizontal (0.25 ratio from [1, 3]).
    let _ = render_to_buffer(&mut state, 140, 40);
    let list_area = state
        .hit_registry
        .find(|t| matches!(t, HitTarget::FileHistoryListArea))
        .expect("FileHistoryListArea hit region")
        .rect;
    let diff_area = state
        .hit_registry
        .find(|t| matches!(t, HitTarget::FileHistoryDiffArea))
        .expect("FileHistoryDiffArea hit region")
        .rect;
    let (_, container) = state
        .splitter_containers
        .get(&SplitterId::FileHistoryHorizontal)
        .copied()
        .expect("FileHistoryHorizontal container recorded");
    assert_eq!(
        list_area.width,
        ((f32::from(container.width) * 0.25).round() as u16).max(24)
    );
    assert_eq!(list_area.width + 1 + diff_area.width, container.width);

    // Hit-test directly on the divider column must resolve to Splitter (not DialogBody).
    let div_col = list_area.right();
    let div_row = list_area.y + 2;
    let hit = state.hit_registry.hit_test(div_col, div_row).unwrap();
    assert_eq!(
        hit.target,
        HitTarget::Splitter {
            id: SplitterId::FileHistoryHorizontal,
            axis: SplitAxis::Horizontal,
        }
    );

    // Drag divider rightward, verify list pane widens, then double-click to reset to [1, 3].
    let drag_target = container.x + (container.width * 3) / 5;
    let _ = crate::input::route_mouse(
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: div_col,
            row: div_row,
            modifiers: KeyModifiers::NONE,
        },
        &mut state,
    );
    let _ = crate::input::route_mouse(
        MouseEvent {
            kind: MouseEventKind::Drag(MouseButton::Left),
            column: drag_target,
            row: div_row,
            modifiers: KeyModifiers::NONE,
        },
        &mut state,
    );
    let _ = crate::input::route_mouse(
        MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            column: drag_target,
            row: div_row,
            modifiers: KeyModifiers::NONE,
        },
        &mut state,
    );
    let _ = render_to_buffer(&mut state, 140, 40);
    let dragged_list = state
        .hit_registry
        .find(|t| matches!(t, HitTarget::FileHistoryListArea))
        .unwrap()
        .rect;
    assert!(dragged_list.width > list_area.width);

    // Double-click the new divider position to reset to configured [1, 3].
    state.last_click = None;
    let new_div_col = dragged_list.right();
    for _ in 0..2 {
        let actions = crate::input::route_mouse(
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: new_div_col,
                row: div_row,
                modifiers: KeyModifiers::NONE,
            },
            &mut state,
        );
        for a in actions {
            let _ = state.dispatch(a);
        }
    }
    let _ = render_to_buffer(&mut state, 140, 40);
    let reset_list = state
        .hit_registry
        .find(|t| matches!(t, HitTarget::FileHistoryListArea))
        .unwrap()
        .rect;
    assert_eq!(reset_list.width, list_area.width);

    // Narrow terminal (<100 content width): uses FileHistoryVertical (1/3 ratio from [1, 2]).
    let _ = render_to_buffer(&mut state, 80, 32);
    let v_list = state
        .hit_registry
        .find(|t| matches!(t, HitTarget::FileHistoryListArea))
        .unwrap()
        .rect;
    let v_diff = state
        .hit_registry
        .find(|t| matches!(t, HitTarget::FileHistoryDiffArea))
        .unwrap()
        .rect;
    let (_, v_container) = state
        .splitter_containers
        .get(&SplitterId::FileHistoryVertical)
        .copied()
        .expect("FileHistoryVertical container recorded");
    assert_eq!(
        v_list.height,
        ((f32::from(v_container.height) / 3.0).round() as u16).max(3)
    );
    assert_eq!(v_list.height + 1 + v_diff.height, v_container.height);

    // Hit-test directly on the vertical divider row must resolve to Splitter (not DialogBody).
    let v_div_col = v_container.x + 2;
    let v_div_row = v_list.bottom();
    let v_hit = state.hit_registry.hit_test(v_div_col, v_div_row).unwrap();
    assert_eq!(
        v_hit.target,
        HitTarget::Splitter {
            id: SplitterId::FileHistoryVertical,
            axis: SplitAxis::Vertical,
        }
    );

    // Drag vertical divider downward, verify list pane grows, then double-click to reset to [1, 2].
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
        let _ = crate::input::route_mouse(
            MouseEvent {
                kind,
                column: v_div_col,
                row,
                modifiers: KeyModifiers::NONE,
            },
            &mut state,
        );
    }
    let _ = render_to_buffer(&mut state, 80, 32);
    let dragged_v_list = state
        .hit_registry
        .find(|t| matches!(t, HitTarget::FileHistoryListArea))
        .unwrap()
        .rect;
    assert!(dragged_v_list.height > v_list.height);

    state.last_click = None;
    let new_v_div_row = dragged_v_list.bottom();
    for _ in 0..2 {
        let actions = crate::input::route_mouse(
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: v_div_col,
                row: new_v_div_row,
                modifiers: KeyModifiers::NONE,
            },
            &mut state,
        );
        for a in actions {
            let _ = state.dispatch(a);
        }
    }
    let _ = render_to_buffer(&mut state, 80, 32);
    let reset_v_list = state
        .hit_registry
        .find(|t| matches!(t, HitTarget::FileHistoryListArea))
        .unwrap()
        .rect;
    assert_eq!(reset_v_list.height, v_list.height);
}

#[test]
fn directory_context_menu_starts_on_enabled_item_and_disabled_mnemonic_updates_footer() {
    let dir = tempfile::tempdir().unwrap();
    let workdir = dir.path().to_path_buf();
    let c1 = oid_n(1);
    let c2 = oid_n(2);
    let mut state = test_state(workdir.clone());

    state.repo_snapshot = Some(make_snapshot(
        workdir,
        vec![make_commit(2, vec![c1], "mod"), make_commit(1, vec![], "init")],
    ));
    let _ = state.dispatch(Action::SwitchTab(WorkspaceTab::Log));

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
    let _ = state.apply_engine_event(EngineEvent::CommitDetailsLoaded { bundle, epoch: 1 });
    let _ = render_to_buffer(&mut state, 140, 40);

    let rows = state.details.change_rows().to_vec();
    let dir_idx = rows
        .iter()
        .position(|r| matches!(r.kind, ChangeRowKind::Dir { .. }))
        .expect("directory row");

    let _ = state.dispatch(Action::FocusPane(PaneId::LogChangesTree));
    let _ = state.dispatch(Action::ChangesSelectRow(dir_idx));
    let acts = route_key(key(KeyCode::Char('.')), &mut state);
    let _ = dispatch_all(&mut state, acts);

    let (items, selected) = match state.modal_stack.last() {
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

    let buf = render_to_buffer(&mut state, 140, 40);
    let footer = (0..buf.area.width)
        .map(|x| buf[(x, 39)].symbol())
        .collect::<String>();
    assert!(
        !footer.contains("Toggle Diff Preview"),
        "footer must not show disabled Toggle Diff Preview on open: {footer}"
    );

    // Pressing disabled mnemonic 'p' ("Copy Patch of File") moves selection to it so footer shows its own reason.
    let acts = route_key(key(KeyCode::Char('p')), &mut state);
    let _ = dispatch_all(&mut state, acts);
    let (items, selected) = match state.modal_stack.last() {
        Some(ModalLayer::ContextMenu {
            items, selected, ..
        }) => (items.clone(), *selected),
        other => panic!("expected ContextMenu still open, got {other:?}"),
    };
    assert_eq!(items[selected].label, "Copy Patch of File");
    let buf = render_to_buffer(&mut state, 140, 40);
    let footer = (0..buf.area.width)
        .map(|x| buf[(x, 39)].symbol())
        .collect::<String>();
    assert!(
        footer.contains("Copy Patch of File: Select a file"),
        "footer must show disabled item's own reason, got: {footer}"
    );
}
