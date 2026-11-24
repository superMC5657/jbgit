//! Spec 12 #09 (repository session rebind + switcher) and #10 (worktrees).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crossterm::event::{Event, KeyCode, KeyModifiers, MouseButton, MouseEventKind};
use jbgit_core::{
    GitFailure, GitOp, GitOpOutcome, GitVersion, PaneId, RepoSnapshot, WorkspaceTab,
    WorktreeBranch, WorktreeInfo,
};
use jbgit_engine::EngineEvent;
use tempfile::TempDir;

use crate::app::{Action, ModalLayer, UiAsyncEvent};
use crate::branches_popup::{PopupRow, build_popup_rows};
use crate::dialogs::Dialog;
use crate::repo_switcher::{RecentRepos, SwitcherSource};
use crate::session::SessionInput;

use super::{TestSession, find_item, item_run_actions, oid, screen_text, snapshot, test_state};

/// `main` checked out in `<root>/jbgit` (current), `feature/dag-layout` in a
/// locked linked worktree, a detached prunable one; real directories so
/// switching validates.
fn worktree_fixture() -> (TempDir, RepoSnapshot) {
    let root = TempDir::new().unwrap();
    let main = root.path().join("jbgit");
    let feature = root.path().join("jbgit-feature");
    std::fs::create_dir_all(main.join(".git")).unwrap();
    std::fs::create_dir_all(&feature).unwrap();
    let mut snap = snapshot();
    snap.workdir = main.clone();
    snap.git_dir = main.join(".git");
    snap.common_dir = main.join(".git");
    snap.worktrees = vec![
        WorktreeInfo {
            path: main,
            head: Some(oid(12)),
            branch: Some("main".into()),
            is_main: true,
            ..WorktreeInfo::default()
        },
        WorktreeInfo {
            path: feature,
            head: Some(oid(5)),
            branch: Some("feature/dag-layout".into()),
            locked: Some("usb".into()),
            ..WorktreeInfo::default()
        },
        WorktreeInfo {
            path: root.path().join("gone"),
            head: Some(oid(3)),
            detached: true,
            prunable: Some("gitdir file points to non-existent location".into()),
            ..WorktreeInfo::default()
        },
    ];
    (root, snap)
}

fn key(ts: &mut TestSession, c: char) {
    ts.send_key(KeyCode::Char(c), KeyModifiers::NONE);
}

fn top_dialog(ts: &TestSession) -> &Dialog {
    match ts.state().modal_stack.last() {
        Some(ModalLayer::Dialog(d)) => d.as_ref(),
        other => panic!("expected dialog, got {other:?}"),
    }
}

fn dialog_input_value(ts: &TestSession) -> String {
    match top_dialog(ts) {
        Dialog::Input(i) => i.value.clone(),
        other => panic!("expected input dialog, got {other:?}"),
    }
}

fn clear_input(ts: &mut TestSession) {
    ts.send_key(KeyCode::Char('u'), KeyModifiers::CONTROL);
}

fn worktrees_selected(ts: &TestSession) -> usize {
    ts.state()
        .modal_stack
        .iter()
        .find_map(|l| match l {
            ModalLayer::Worktrees(ws) => Some(ws.selected),
            _ => None,
        })
        .expect("Worktrees modal open")
}

fn loaded(snap: RepoSnapshot) -> EngineEvent {
    EngineEvent::RepoSnapshotLoaded {
        snapshot: Arc::new(snap),
        epoch: 1,
    }
}

fn repo_snapshot_at(dir: &Path, name: &str, branch: &str) -> RepoSnapshot {
    let mut snap = snapshot();
    snap.repo_name = name.into();
    snap.workdir = dir.to_path_buf();
    snap.git_dir = dir.join(".git");
    snap.common_dir = dir.join(".git");
    snap.head_branch = Some(branch.into());
    snap
}

#[test]
fn late_results_of_the_previous_repository_are_dropped_after_rebind() {
    let a = TempDir::new().unwrap();
    let b = TempDir::new().unwrap();
    let mut ts = TestSession::new(test_state(a.path().to_path_buf()));
    let gen_a = ts.session.generation();
    ts.step([SessionInput::EngineFrom(
        gen_a,
        loaded(repo_snapshot_at(a.path(), "repo-a", "main")),
    )]);
    assert_eq!(
        ts.state().repo_snapshot.as_ref().unwrap().repo_name,
        "repo-a"
    );

    // A slow diff / operation of A is in flight when the user switches.
    ts.state_mut().git_ops_running = 1;
    ts.session.prepare_unbind();
    ts.session.rebind(test_state(b.path().to_path_buf()), true);
    let gen_b = ts.session.generation();
    assert_ne!(gen_a, gen_b);
    assert!(ts.state().repo_snapshot.is_none());
    assert_eq!(ts.state().git_ops_running, 0);
    assert!(
        ts.state()
            .notification
            .as_ref()
            .is_some_and(|n| !n.is_error),
        "switch is announced"
    );

    // A's late results arrive stamped with A's generation: all dropped.
    ts.step([
        SessionInput::EngineFrom(gen_a, loaded(repo_snapshot_at(a.path(), "repo-a", "main"))),
        SessionInput::EngineFrom(
            gen_a,
            EngineEvent::GitOpFinished(GitOpOutcome::ok(GitOp::Fetch)),
        ),
        SessionInput::UiAsyncFrom(
            gen_a,
            UiAsyncEvent::ContainingBranches {
                oid: oid(1),
                token: jbgit_core::RequestEpoch::new().issue(),
                names: vec!["leak".into()],
            },
        ),
    ]);
    assert!(ts.state().repo_snapshot.is_none(), "A must not pollute B");
    assert_eq!(ts.state().git_ops_running, 0);

    // B's own results are applied.
    ts.step([SessionInput::EngineFrom(
        gen_b,
        loaded(repo_snapshot_at(b.path(), "repo-b", "develop")),
    )]);
    let text = screen_text(&ts.render(120, 30));
    assert!(text.contains("repo-b"), "{text}");
    assert!(text.contains("develop"), "{text}");
    assert!(!text.contains("repo-a"), "{text}");
}

#[test]
fn drafts_are_saved_and_restored_per_repository() {
    let a = TempDir::new().unwrap();
    let wt = TempDir::new().unwrap();
    let main_snap = repo_snapshot_at(a.path(), "jbgit", "main");
    // A linked worktree: own git_dir, shared common_dir.
    let mut wt_snap = repo_snapshot_at(wt.path(), "jbgit-feature", "feature/dag-layout");
    wt_snap.common_dir = a.path().join(".git");
    wt_snap.git_dir = a
        .path()
        .join(".git")
        .join("worktrees")
        .join("jbgit-feature");
    std::fs::create_dir_all(&wt_snap.git_dir).unwrap();
    std::fs::create_dir_all(&main_snap.git_dir).unwrap();

    let mut ts = TestSession::new(test_state(a.path().to_path_buf()));
    ts.deliver_engine_event(loaded(main_snap.clone()));
    ts.dispatch(Action::SwitchTab(WorkspaceTab::Commit));
    ts.dispatch(Action::FocusPane(PaneId::CommitMessageEditor));
    ts.send_event(Event::Paste("feat: main draft".into()));

    // Switching flushes the pending draft synchronously.
    ts.session.prepare_unbind();
    let main_draft = main_snap.git_dir.join("jbgit-commit-draft");
    assert_eq!(
        std::fs::read_to_string(&main_draft).unwrap(),
        "feat: main draft"
    );

    ts.session.rebind(test_state(wt.path().to_path_buf()), true);
    ts.deliver_engine_event(loaded(wt_snap.clone()));
    assert_eq!(ts.state().stage_workbench.editor().text(), "");
    let text = screen_text(&ts.render(120, 30));
    assert!(
        text.contains("jbgit-feature") && text.contains("feature/dag-layout"),
        "{text}"
    );
    ts.dispatch(Action::SwitchTab(WorkspaceTab::Commit));
    ts.dispatch(Action::FocusPane(PaneId::CommitMessageEditor));
    ts.send_event(Event::Paste("fix: worktree draft".into()));
    ts.session.prepare_unbind();
    assert_eq!(
        std::fs::read_to_string(wt_snap.git_dir.join("jbgit-commit-draft")).unwrap(),
        "fix: worktree draft"
    );

    // Back to the main repository: its draft comes back, untouched.
    ts.session.rebind(test_state(a.path().to_path_buf()), true);
    ts.deliver_engine_event(loaded(main_snap));
    assert_eq!(
        ts.state().stage_workbench.editor().text(),
        "feat: main draft"
    );
    let text = screen_text(&ts.render(120, 30));
    assert!(
        text.contains("jbgit") && !text.contains("jbgit-feature"),
        "{text}"
    );
}

#[test]
fn switch_guards_confirm_running_work_and_reject_bad_targets() {
    let (root, snap) = worktree_fixture();
    let feature = snap.worktrees[1].path.clone();
    let mut ts = TestSession::with_snapshot(snap.clone());

    // Same repository / subdirectory-free target → notice only.
    ts.dispatch(Action::SwitchRepository(snap.workdir.clone()));
    assert!(ts.session.take_repo_switch_request().is_none());

    // Missing path → error, dropped from the recent list.
    let missing = root.path().join("nope");
    ts.state_mut().recent_repos.record(&missing, "nope");
    ts.dispatch(Action::SwitchRepository(missing.clone()));
    assert!(ts.session.take_repo_switch_request().is_none());
    assert!(ts.state().notification.as_ref().is_some_and(|n| n.is_error));
    assert!(ts.state().recent_repos.entries().is_empty());

    // A running operation asks first.
    ts.state_mut().git_ops_running = 1;
    ts.dispatch(Action::SwitchRepository(feature.clone()));
    assert!(ts.session.take_repo_switch_request().is_none());
    let text = screen_text(&ts.render(120, 30));
    assert!(text.contains("Switch Repository?"), "{text}");
    assert!(text.contains("A Git operation is still running"), "{text}");
    // Cancel is the default button.
    ts.send_key(KeyCode::Left, KeyModifiers::NONE);
    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(ts.session.take_repo_switch_request(), Some(feature.clone()));

    // Unsaved conflict-resolver choices block the switch outright.
    ts.state_mut().git_ops_running = 0;
    let mut doc = jbgit_core::ConflictDocument::parse(
        "a.txt",
        "<<<<<<< ours\na\n=======\nb\n>>>>>>> theirs\n",
        None,
        None,
    );
    doc.set_choice(0, jbgit_core::ConflictChoice::Ours);
    ts.state_mut()
        .modal_stack
        .push(ModalLayer::ConflictResolver(Box::new(
            crate::app::ConflictMergeState {
                path: "a.txt".into(),
                doc: Some(doc),
                selected_block: 0,
                scroll: Default::default(),
                externally_resolved: false,
            },
        )));
    ts.dispatch(Action::SwitchRepository(feature.clone()));
    assert!(ts.session.take_repo_switch_request().is_none());
    assert!(
        ts.state()
            .status_notice
            .as_deref()
            .is_some_and(|n| n.contains("conflict merge")),
        "{:?}",
        ts.state().status_notice
    );
    ts.state_mut().modal_stack.truncate(0);

    // Nothing pending → switches right away.
    ts.dispatch(Action::SwitchRepository(feature.clone()));
    assert_eq!(ts.session.take_repo_switch_request(), Some(feature));
}

#[test]
fn repo_switcher_lists_recent_worktrees_and_submodules_and_switches() {
    let (root, mut snap) = worktree_fixture();
    let other = root.path().join("other-repo");
    let missing = root.path().join("deleted-repo");
    std::fs::create_dir_all(&other).unwrap();
    std::fs::create_dir_all(&missing).unwrap();
    std::fs::create_dir_all(snap.workdir.join("libs/sub/.git")).unwrap();
    snap.submodules = vec!["libs/sub".into(), "libs/uninitialized".into()];

    let store_dir = TempDir::new().unwrap();
    let store_file = store_dir.path().join("recent-repos.toml");
    {
        let mut store = RecentRepos::load(store_file.clone());
        store.record(&missing, "deleted-repo");
        store.record(&other, "other-repo");
        store.record(&snap.workdir, "jbgit");
    }
    std::fs::remove_dir_all(&missing).unwrap();

    let mut ts = TestSession::with_snapshot(snap.clone());
    ts.state_mut().recent_repos = RecentRepos::load(store_file);

    // <A-o> opens it; the missing repository is pruned and reported.
    ts.send_key(KeyCode::Char('o'), KeyModifiers::ALT);
    let rs = match ts.state().modal_stack.last() {
        Some(ModalLayer::RepoSwitcher(rs)) => rs.as_ref().clone(),
        other => panic!("expected switcher, got {other:?}"),
    };
    let listed: Vec<(SwitcherSource, &str)> = rs
        .items
        .iter()
        .map(|i| (i.source, i.name.as_str()))
        .collect();
    assert_eq!(
        listed,
        vec![
            (SwitcherSource::Recent, "other-repo"),
            (SwitcherSource::Worktree, "jbgit-feature"),
            (SwitcherSource::Submodule, "libs/sub"),
        ],
        "current repo, prunable worktree and uninitialized submodule are skipped"
    );
    assert!(
        ts.state()
            .status_notice
            .as_deref()
            .is_some_and(|n| n.contains("deleted-repo")),
        "{:?}",
        ts.state().status_notice
    );
    let text = screen_text(&ts.render(120, 30));
    assert!(text.contains("Switch Repository"), "{text}");
    assert!(text.contains("feature/dag-layout"), "{text}");

    // Type to filter, Enter switches.
    ts.type_str("feat");
    let text = screen_text(&ts.render(120, 30));
    assert!(!text.contains("other-repo"), "{text}");
    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(
        ts.session.take_repo_switch_request(),
        Some(snap.worktrees[1].path.clone())
    );
    assert!(ts.state().modal_stack.is_empty());

    // Del forgets a recent entry.
    ts.dispatch(Action::OpenRepoSwitcher);
    ts.send_key(KeyCode::Delete, KeyModifiers::NONE);
    assert!(
        ts.state()
            .recent_repos
            .entries()
            .iter()
            .all(|e| e.name != "other-repo")
    );

    // Ctrl+O asks for any path.
    ts.send_key(KeyCode::Char('o'), KeyModifiers::CONTROL);
    assert!(matches!(top_dialog(&ts), Dialog::Input(_)));
    ts.type_str(&other.display().to_string());
    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(ts.session.take_repo_switch_request(), Some(other));

    // Clicking the repository name in the header opens the switcher.
    ts.render(120, 30);
    let (col, row) = ts
        .hit_center_of(|t| matches!(t, crate::hit_test::HitTarget::HeaderRepoName))
        .expect("header repo name hit target");
    ts.send_mouse(MouseEventKind::Down(MouseButton::Left), col, row);
    assert!(matches!(
        ts.state().modal_stack.last(),
        Some(ModalLayer::RepoSwitcher(_))
    ));
}

#[test]
fn worktrees_modal_drives_open_lock_remove_prune_and_new() {
    let (_root, snap) = worktree_fixture();
    let feature = snap.worktrees[1].path.clone();
    let mut ts = TestSession::with_snapshot(snap.clone());

    // <A-w> opens on the current working tree.
    ts.send_key(KeyCode::Char('w'), KeyModifiers::ALT);
    assert_eq!(worktrees_selected(&ts), 0);
    let text = screen_text(&ts.render(130, 32));
    assert!(text.contains("Working Trees"), "{text}");
    assert!(text.contains("● jbgit"), "{text}");
    assert!(text.contains("[main]"), "{text}");
    assert!(text.contains("[locked: usb]"), "{text}");
    assert!(text.contains("[prunable]"), "{text}");
    assert!(text.contains("(detached)"), "{text}");

    // The current / main tree can be neither removed nor locked.
    key(&mut ts, 'd');
    key(&mut ts, 'l');
    assert!(ts.take_git_ops().is_empty());

    // Locked feature tree: remove refused, `l` unlocks.
    key(&mut ts, 'j');
    key(&mut ts, 'd');
    assert!(ts.take_git_ops().is_empty());
    assert!(
        ts.state()
            .status_notice
            .as_deref()
            .is_some_and(|n| n.contains("unlock"))
    );
    key(&mut ts, 'l');
    assert_eq!(
        ts.take_git_ops(),
        vec![GitOp::UnlockWorktree {
            path: feature.clone()
        }]
    );

    // Once unlocked: lock asks for a reason, remove asks, dirty → force.
    let mut unlocked = snap.clone();
    unlocked.worktrees[1].locked = None;
    ts.deliver_engine_event(loaded(unlocked));
    key(&mut ts, 'l');
    ts.type_str("on usb");
    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(
        ts.take_git_ops(),
        vec![GitOp::LockWorktree {
            path: feature.clone(),
            reason: Some("on usb".into()),
        }]
    );
    key(&mut ts, 'd');
    let text = screen_text(&ts.render(130, 32));
    assert!(text.contains("Remove Worktree 'jbgit-feature'?"), "{text}");
    assert!(
        text.contains("Branch 'feature/dag-layout' is kept."),
        "{text}"
    );
    ts.send_key(KeyCode::Left, KeyModifiers::NONE);
    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);
    let remove = GitOp::RemoveWorktree {
        path: feature.clone(),
        force: false,
    };
    assert_eq!(ts.take_git_ops(), vec![remove.clone()]);
    let mut dirty = GitOpOutcome::ok(remove);
    dirty.success = false;
    dirty.failure = Some(GitFailure::WorktreeDirty);
    ts.deliver_engine_event(EngineEvent::GitOpFinished(dirty));
    let text = screen_text(&ts.render(130, 32));
    assert!(text.contains("Worktree Has Local Changes"), "{text}");
    ts.send_key(KeyCode::Left, KeyModifiers::NONE);
    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(
        ts.take_git_ops(),
        vec![GitOp::RemoveWorktree {
            path: feature.clone(),
            force: true,
        }]
    );

    // Prune.
    key(&mut ts, 'p');
    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(ts.take_git_ops(), vec![GitOp::PruneWorktrees]);

    // Enter opens the selected (non-current) tree through a repository switch
    // (the operations above never reported back, so finish them first).
    ts.state_mut().git_ops_running = 0;
    key(&mut ts, 'g');
    key(&mut ts, 'j');
    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(ts.session.take_repo_switch_request(), Some(feature));
}

#[test]
fn new_worktree_flow_defaults_path_and_offers_open() {
    let (root, snap) = worktree_fixture();
    let mut ts = TestSession::with_snapshot(snap);
    ts.dispatch(Action::OpenWorktrees);

    // New → new branch from HEAD → name → default sibling path.
    key(&mut ts, 'n');
    let text = screen_text(&ts.render(130, 32));
    assert!(text.contains("New branch from HEAD"), "{text}");
    assert!(text.contains("Existing branch"), "{text}");
    assert!(text.contains("Detached HEAD at"), "{text}");
    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);
    ts.type_str("feature/login");
    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);
    let expected = root.path().join("jbgit-feature-login");
    assert_eq!(dialog_input_value(&ts), expected.display().to_string());
    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);
    let add = GitOp::AddWorktree {
        path: expected.clone(),
        branch: WorktreeBranch::New {
            name: "feature/login".into(),
            start: "HEAD".into(),
        },
    };
    assert_eq!(ts.take_git_ops(), vec![add.clone()]);

    // An occupied path is rejected.
    ts.dispatch(Action::PromptNewWorktree { at: None });
    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);
    ts.type_str("x");
    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);
    clear_input(&mut ts);
    ts.type_str(&root.path().join("jbgit").display().to_string());
    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);
    assert!(ts.take_git_ops().is_empty());
    let text = screen_text(&ts.render(130, 32));
    assert!(text.contains("already exists"), "{text}");
    ts.send_key(KeyCode::Esc, KeyModifiers::NONE);

    // Success offers "Open"; it switches once the directory exists.
    std::fs::create_dir_all(&expected).unwrap();
    ts.deliver_engine_event(EngineEvent::GitOpFinished(GitOpOutcome::ok(add)));
    let text = screen_text(&ts.render(130, 32));
    assert!(
        text.contains("Created worktree 'jbgit-feature-login'"),
        "{text}"
    );
    ts.dispatch(Action::NotificationActivate);
    assert_eq!(ts.session.take_repo_switch_request(), Some(expected));

    // Existing branch: only branches free of every worktree are accepted.
    ts.state_mut().modal_stack.truncate(0);
    ts.dispatch(Action::PromptNewWorktree { at: None });
    ts.send_key(KeyCode::Down, KeyModifiers::NONE);
    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);
    clear_input(&mut ts);
    ts.type_str("feature/dag-layout");
    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);
    let text = screen_text(&ts.render(130, 32));
    assert!(text.contains("not a local branch free"), "{text}");
    clear_input(&mut ts);
    ts.type_str("bugfix/unmerged");
    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);
    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(
        ts.take_git_ops(),
        vec![GitOp::AddWorktree {
            path: root.path().join("jbgit-bugfix-unmerged"),
            branch: WorktreeBranch::Existing("bugfix/unmerged".into()),
        }]
    );
}

#[test]
fn branches_checked_out_elsewhere_link_to_their_worktree() {
    let (_root, snap) = worktree_fixture();
    let feature = snap.worktrees[1].path.clone();
    let mut ts = TestSession::with_snapshot(snap);

    // Popup marks the branch and lists the Working Trees entry.
    let rows = build_popup_rows(ts.state(), "");
    let marked = rows
        .iter()
        .find_map(|r| match r {
            PopupRow::Branch(b) if b.name == "feature/dag-layout" => Some(b.worktree.clone()),
            _ => None,
        })
        .expect("feature row");
    assert_eq!(marked.as_deref(), Some("jbgit-feature"));
    assert!(rows.contains(&PopupRow::Worktrees));
    ts.dispatch(Action::OpenBranchesPopup {
        query: String::new(),
    });
    let text = screen_text(&ts.render(130, 32));
    assert!(text.contains("⎇ jbgit-feature"), "{text}");
    ts.state_mut().modal_stack.truncate(0);

    // Log Branches Pane also marks the branch checked out in a linked worktree.
    ts.dispatch(Action::SwitchTab(jbgit_core::WorkspaceTab::Log));
    let log_text = screen_text(&ts.render(140, 32));
    assert!(log_text.contains("⎇ jbgit-feature"), "{log_text}");

    // Checkout is disabled with the reason; "Open in Worktree" switches.
    let items = crate::menus::branch_menu_items(
        ts.state(),
        "feature/dag-layout",
        jbgit_core::RefKind::LocalBranch,
    );
    let checkout = find_item(&items, "Checkout");
    assert!(
        checkout
            .disabled_reason
            .as_deref()
            .is_some_and(|r| r.contains("jbgit-feature")),
        "{checkout:?}"
    );
    let open = find_item(&items, "Open in Worktree 'jbgit-feature'");
    assert!(matches!(
        item_run_actions(open).as_slice(),
        [Action::SwitchRepository(p)] if *p == feature
    ));
    // A free branch offers "New Worktree…" instead.
    let items = crate::menus::branch_menu_items(
        ts.state(),
        "bugfix/unmerged",
        jbgit_core::RefKind::LocalBranch,
    );
    find_item(&items, "New Worktree…");

    // Git refusing a checkout for that reason offers to open the worktree.
    let op = GitOp::Checkout {
        target: "feature/dag-layout".into(),
        detach: false,
    };
    let mut failed = GitOpOutcome::ok(op);
    failed.success = false;
    failed.failure = Some(GitFailure::BranchCheckedOutInWorktree {
        branch: "feature/dag-layout".into(),
        path: feature.display().to_string(),
    });
    ts.deliver_engine_event(EngineEvent::GitOpFinished(failed));
    let text = screen_text(&ts.render(130, 32));
    assert!(text.contains("Branch Checked Out Elsewhere"), "{text}");
    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(ts.session.take_repo_switch_request(), Some(feature));

    // Log commit menu: "New Worktree from Here…" (new branch / detached).
    let items = crate::menus::log_commit_menu_items(ts.state());
    let item = find_item(&items, "New Worktree from Here…");
    ts.dispatch_many(item_run_actions(item));
    let text = screen_text(&ts.render(130, 32));
    assert!(text.contains("New branch from"), "{text}");
    assert!(text.contains("Detached HEAD at"), "{text}");
    assert!(!text.contains("Existing branch"), "{text}");
}

#[test]
fn worktrees_are_hidden_on_old_git() {
    let (_root, snap) = worktree_fixture();
    let mut ts = TestSession::with_snapshot(snap);
    ts.state_mut().git_version = GitVersion::parse("git version 2.16.2");
    ts.send_key(KeyCode::Char('w'), KeyModifiers::ALT);
    assert!(ts.state().modal_stack.is_empty());
    assert!(
        ts.state()
            .status_notice
            .as_deref()
            .is_some_and(|n| n.contains("2.17") && n.contains("2.16.2")),
        "{:?}",
        ts.state().status_notice
    );
    assert!(!build_popup_rows(ts.state(), "").contains(&PopupRow::Worktrees));
    let items = crate::menus::log_commit_menu_items(ts.state());
    assert!(items.iter().all(|i| !i.label.contains("Worktree")));
    let items = crate::menus::branch_menu_items(
        ts.state(),
        "bugfix/unmerged",
        jbgit_core::RefKind::LocalBranch,
    );
    assert!(items.iter().all(|i| !i.label.contains("Worktree")));
}

#[test]
fn rebind_carries_terminal_state_and_records_recent_repository() {
    let a = TempDir::new().unwrap();
    let b = TempDir::new().unwrap();
    let mut ts = TestSession::new(test_state(a.path().to_path_buf()));
    ts.render(100, 30);
    let area = ts.state().terminal_area;
    ts.state_mut().git_version = Some(GitVersion::new(2, 45, 0));
    ts.session.prepare_unbind();
    ts.session.rebind(test_state(b.path().to_path_buf()), false);
    assert_eq!(ts.state().terminal_area, area);
    assert_eq!(ts.state().git_version, Some(GitVersion::new(2, 45, 0)));
    let recent: Vec<PathBuf> = ts
        .state()
        .recent_repos
        .entries()
        .iter()
        .map(|e| e.path.clone())
        .collect();
    assert_eq!(recent, vec![b.path().to_path_buf()]);
    // Writes outliving the 5 s unbind timeout are reported.
    assert!(
        ts.state()
            .notification
            .as_ref()
            .is_some_and(|n| n.is_error && n.text.contains("background")),
        "{:?}",
        ts.state().notification
    );
}
