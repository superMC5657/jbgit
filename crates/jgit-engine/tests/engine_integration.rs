use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use jgit_core::{RefKind, RefreshScope, RepositoryState, RequestEpoch};
use jgit_engine::pool::{load_repo_snapshot_sync, load_stage_status_sync};
use jgit_engine::{
    EngineError, EngineEvent, EngineSupervisor, GitCliExecutor, MutationGuard, RepoLocation,
    TaskRequest, prepare_pathspec_args,
};
use tempfile::TempDir;

fn git(dir: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "Test User")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "Test User")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .output()
        .expect("failed to execute git");
    assert!(
        output.status.success(),
        "git {:?} failed: stdout={}, stderr={}",
        args,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn unborn_empty_repo_and_stage_status() {
    let tmp = TempDir::new().unwrap();
    git(tmp.path(), &["init", "-b", "main"]);

    fs::write(tmp.path().join("hello.txt"), "hello\n").unwrap();

    let loc = RepoLocation::discover(tmp.path()).unwrap();
    let epoch_tracker = RequestEpoch::new();
    let token = epoch_tracker.issue();

    let snap = load_repo_snapshot_sync(&loc, &token, 1000).unwrap();
    assert!(snap.is_unborn_head);
    assert_eq!(snap.head_branch.as_deref(), Some("main"));
    assert_eq!(snap.state, RepositoryState::Clean);
    assert!(snap.commits.is_empty());

    let stage = load_stage_status_sync(&loc, false, &token).unwrap();
    assert_eq!(stage.untracked.len(), 1);
    assert_eq!(stage.untracked[0].path, "hello.txt");
}

#[test]
fn branches_tags_and_ahead_behind_counts() {
    let remote_dir = TempDir::new().unwrap();
    git(remote_dir.path(), &["init", "--bare", "-b", "main"]);

    let work_a = TempDir::new().unwrap();
    git(work_a.path(), &["init", "-b", "main"]);
    git(
        work_a.path(),
        &[
            "remote",
            "add",
            "origin",
            remote_dir.path().to_str().unwrap(),
        ],
    );
    fs::write(work_a.path().join("a.txt"), "v1\n").unwrap();
    git(work_a.path(), &["add", "."]);
    git(work_a.path(), &["commit", "-m", "initial"]);
    git(work_a.path(), &["push", "-u", "origin", "main"]);

    // Clone to work_b and push one commit ahead to remote
    let work_b = TempDir::new().unwrap();
    git(
        work_b.path(),
        &["clone", remote_dir.path().to_str().unwrap(), "."],
    );
    fs::write(work_b.path().join("b.txt"), "from b\n").unwrap();
    git(work_b.path(), &["add", "."]);
    git(work_b.path(), &["commit", "-m", "remote commit"]);
    git(work_b.path(), &["push", "origin", "main"]);

    // Fetch in work_a and make two local commits -> 2 ahead, 1 behind
    git(work_a.path(), &["fetch", "origin"]);
    fs::write(work_a.path().join("a.txt"), "v2\n").unwrap();
    git(work_a.path(), &["commit", "-am", "local 1"]);
    fs::write(work_a.path().join("a.txt"), "v3\n").unwrap();
    git(work_a.path(), &["commit", "-am", "local 2"]);

    // Add feature branch and annotated tag
    git(work_a.path(), &["branch", "feature/ui"]);
    git(work_a.path(), &["tag", "-a", "v0.1.0", "-m", "release"]);

    let loc = RepoLocation::discover(work_a.path()).unwrap();
    let tracker = RequestEpoch::new();
    let token = tracker.issue();
    let snap = load_repo_snapshot_sync(&loc, &token, 1000).unwrap();

    assert!(!snap.is_unborn_head);
    assert_eq!(snap.head_branch.as_deref(), Some("main"));
    assert_eq!(snap.state, RepositoryState::Clean);

    let main_ref = snap
        .refs
        .iter()
        .find(|r| r.kind == RefKind::LocalBranch && r.name == "main")
        .expect("main branch missing");
    assert!(main_ref.is_head);
    assert_eq!(main_ref.upstream.as_deref(), Some("origin/main"));
    assert_eq!(main_ref.ahead_behind, Some((2, 1)));

    assert!(
        snap.refs
            .iter()
            .any(|r| r.kind == RefKind::LocalBranch && r.name == "feature/ui")
    );
    assert!(
        snap.refs
            .iter()
            .any(|r| r.kind == RefKind::RemoteBranch && r.name == "origin/main")
    );
    assert!(
        snap.refs
            .iter()
            .any(|r| r.kind == RefKind::Tag && r.name == "v0.1.0")
    );
    assert!(snap.commits.len() >= 4);
}

#[test]
fn detects_rebase_conflict_state_and_progress() {
    let tmp = TempDir::new().unwrap();
    git(tmp.path(), &["init", "-b", "main"]);
    fs::write(tmp.path().join("conflict.txt"), "base\n").unwrap();
    git(tmp.path(), &["add", "."]);
    git(tmp.path(), &["commit", "-m", "base"]);

    git(tmp.path(), &["checkout", "-b", "feat"]);
    fs::write(tmp.path().join("conflict.txt"), "feat change\n").unwrap();
    git(tmp.path(), &["commit", "-am", "feat commit"]);

    git(tmp.path(), &["checkout", "main"]);
    fs::write(tmp.path().join("conflict.txt"), "main change\n").unwrap();
    git(tmp.path(), &["commit", "-am", "main commit"]);

    git(tmp.path(), &["checkout", "feat"]);
    let rebase_out = Command::new("git")
        .args(["rebase", "main"])
        .current_dir(tmp.path())
        .env("GIT_AUTHOR_NAME", "Test User")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "Test User")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .output()
        .unwrap();
    assert!(!rebase_out.status.success(), "expected rebase conflict");

    let loc = RepoLocation::discover(tmp.path()).unwrap();
    let tracker = RequestEpoch::new();
    let token = tracker.issue();
    let snap = load_repo_snapshot_sync(&loc, &token, 1000).unwrap();

    match snap.state {
        RepositoryState::RebaseMerge { progress, branch }
        | RepositoryState::RebaseInteractive { progress, branch }
        | RepositoryState::Rebase { progress, branch } => {
            assert_eq!(progress, Some((1, 1)));
            assert_eq!(branch.as_deref(), Some("feat"));
        }
        other => panic!("expected rebase state, got {other:?}"),
    }

    let stage = load_stage_status_sync(&loc, false, &token).unwrap();
    assert_eq!(stage.conflicted.len(), 1);
    assert_eq!(stage.conflicted[0].path, "conflict.txt");
}

#[test]
fn pathspec_from_file_threshold() {
    let small: Vec<String> = (0..10).map(|i| format!("file_{i}.rs")).collect();
    let (args_s, stdin_s) = prepare_pathspec_args(vec!["add".into()], &small);
    assert!(stdin_s.is_none());
    assert!(args_s.contains(&"--".to_string()));

    let large: Vec<String> = (0..40).map(|i| format!("file_{i}.rs")).collect();
    let (args_l, stdin_l) = prepare_pathspec_args(vec!["add".into()], &large);
    assert!(args_l.contains(&"--pathspec-from-file=-".to_string()));
    assert!(args_l.contains(&"--pathspec-file-nul".to_string()));
    let payload = stdin_l.expect("expected nul-delimited stdin payload");
    assert_eq!(payload.iter().filter(|&&b| b == 0).count(), 40);
}

#[tokio::test]
async fn watcher_and_cli_console_events() {
    let tmp = TempDir::new().unwrap();
    git(tmp.path(), &["init", "-b", "main"]);
    fs::write(tmp.path().join("README.md"), "init\n").unwrap();
    git(tmp.path(), &["add", "."]);
    git(tmp.path(), &["commit", "-m", "init"]);

    let loc = RepoLocation::discover(tmp.path()).unwrap();
    let (handle, mut rx) = EngineSupervisor::start(loc, true).unwrap();

    // Verify CLI console streaming
    if GitCliExecutor::detect().is_ok() {
        handle.send(TaskRequest::RunGitCli {
            id: 42,
            args: vec!["status".into(), "--short".into()],
            stdin_payload: None,
            is_read_only: true,
            refresh_after: RefreshScope::None,
        });

        let mut saw_started = false;
        let mut saw_finished = false;
        let deadline = tokio::time::sleep(Duration::from_secs(3));
        tokio::pin!(deadline);
        while !(saw_started && saw_finished) {
            tokio::select! {
                Some(ev) = rx.recv() => match ev {
                    EngineEvent::ConsoleStarted(entry) if entry.id == 42 => {
                        saw_started = true;
                        assert!(entry.command_line.contains("git status"));
                    }
                    EngineEvent::ConsoleFinished { id: 42, exit_code, .. } => {
                        assert_eq!(exit_code, 0);
                        saw_finished = true;
                    }
                    _ => {}
                },
                _ = &mut deadline => panic!("timed out waiting for CLI console events"),
            }
        }
    }

    // Give watcher a moment to arm, then modify worktree & create branch externally
    tokio::time::sleep(Duration::from_millis(150)).await;
    fs::write(tmp.path().join("README.md"), "modified\n").unwrap();

    let mut saw_wt = false;
    let deadline = tokio::time::sleep(Duration::from_secs(3));
    tokio::pin!(deadline);
    while !saw_wt {
        tokio::select! {
            Some(ev) = rx.recv() => {
                if let EngineEvent::ExternalFsChanged(scope) = ev
                    && scope.needs_stage_refresh()
                {
                    saw_wt = true;
                }
            }
            _ = &mut deadline => panic!("timed out waiting for worktree ExternalFsChanged"),
        }
    }

    git(tmp.path(), &["branch", "foo"]);
    let mut saw_ref = false;
    let deadline2 = tokio::time::sleep(Duration::from_secs(3));
    tokio::pin!(deadline2);
    while !saw_ref {
        tokio::select! {
            Some(ev) = rx.recv() => {
                if let EngineEvent::ExternalFsChanged(scope) = ev
                    && scope.needs_graph_refresh()
                {
                    saw_ref = true;
                }
            }
            _ = &mut deadline2 => panic!("timed out waiting for ref ExternalFsChanged"),
        }
    }
}

#[test]
fn epoch_cancellation_and_mutation_guard() {
    let tmp = TempDir::new().unwrap();
    git(tmp.path(), &["init", "-b", "main"]);
    fs::write(tmp.path().join("f.txt"), "1\n").unwrap();
    git(tmp.path(), &["add", "."]);
    git(tmp.path(), &["commit", "-m", "c1"]);

    let loc = RepoLocation::discover(tmp.path()).unwrap();
    let tracker = RequestEpoch::new();
    let stale_token = tracker.issue();
    let _newer_token = tracker.issue();

    let res = load_repo_snapshot_sync(&loc, &stale_token, 100);
    assert!(matches!(res, Err(EngineError::Cancelled)));

    let guard = std::sync::Arc::new(MutationGuard::new());
    let token = guard.begin();
    assert!(guard.record_or_defer(RefreshScope::StageStatus).is_none());
    assert!(guard.record_or_defer(RefreshScope::RefsAndGraph).is_none());
    drop(token);
    assert!(!guard.is_mutating());
    assert_eq!(guard.take_pending(), RefreshScope::All);
}
