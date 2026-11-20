use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use jbgit_core::{RefKind, RefreshScope, RepositoryState, RequestEpoch};
use jbgit_engine::pool::{load_repo_snapshot_sync, load_stage_status_sync};
use jbgit_engine::{
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

fn rev(dir: &Path, spec: &str) -> jbgit_core::Oid {
    let out = Command::new("git")
        .args(["rev-parse", spec])
        .current_dir(dir)
        .output()
        .expect("git rev-parse");
    String::from_utf8_lossy(&out.stdout).trim().parse().unwrap()
}

#[test]
fn commit_details_changes_file_diff_and_path_matches() {
    use jbgit_core::{DetailsKey, DiffContext, DiffLineKind, FileDiffKey, FileStatusKind};
    use jbgit_engine::{load_commit_details_sync, load_file_diff_sync, load_path_matches_sync};

    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();
    git(dir, &["init", "-b", "main"]);
    fs::create_dir_all(dir.join("src/core")).unwrap();
    fs::write(dir.join("src/core/lib.rs"), "fn a() {\n    call(1);\n}\n").unwrap();
    fs::write(
        dir.join("old.txt"),
        "same content here\nline two\nline three\n",
    )
    .unwrap();
    git(dir, &["add", "."]);
    git(dir, &["commit", "-m", "initial"]);

    fs::write(dir.join("src/core/lib.rs"), "fn a() {\n    call(2);\n}\n").unwrap();
    git(dir, &["mv", "old.txt", "new.txt"]);
    git(dir, &["add", "."]);
    git(
        dir,
        &[
            "commit",
            "-m",
            "change call\n\nBody line.\n\nSigned-off-by: Test User <test@example.com>",
        ],
    );

    git(dir, &["checkout", "-b", "side", "HEAD~1"]);
    fs::write(dir.join("side.txt"), "side\n").unwrap();
    git(dir, &["add", "."]);
    git(dir, &["commit", "-m", "side work"]);
    git(dir, &["checkout", "main"]);
    git(dir, &["merge", "--no-ff", "side", "-m", "merge side"]);

    let loc = RepoLocation::discover(dir).unwrap();
    let token = RequestEpoch::new().issue();

    // Single commit vs parent[0]: modification + rename with line stats.
    let change = rev(dir, "HEAD~1");
    let bundle = load_commit_details_sync(&loc, &DetailsKey::single(change), &token).unwrap();
    assert_eq!(bundle.details.len(), 1);
    assert!(bundle.details[0].message.contains("Signed-off-by"));
    assert_eq!(bundle.changes.len(), 2, "{:?}", bundle.changes);
    let lib = bundle
        .changes
        .iter()
        .find(|c| c.path == "src/core/lib.rs")
        .unwrap();
    assert_eq!(
        (lib.kind, lib.additions, lib.deletions),
        (FileStatusKind::Modified, 1, 1)
    );
    let renamed = bundle.changes.iter().find(|c| c.path == "new.txt").unwrap();
    assert_eq!(renamed.kind, FileStatusKind::Renamed);
    assert_eq!(renamed.old_path.as_deref(), Some("old.txt"));

    // Merge commit: parent[0] shows side.txt; parent[1] shows main's changes.
    let merge = rev(dir, "HEAD");
    let p0 = load_commit_details_sync(&loc, &DetailsKey::single(merge), &token).unwrap();
    assert_eq!(
        p0.changes
            .iter()
            .map(|c| c.path.as_str())
            .collect::<Vec<_>>(),
        vec!["side.txt"]
    );
    let p1 = load_commit_details_sync(&loc, &DetailsKey::commits(vec![merge], 1), &token).unwrap();
    assert!(p1.changes.iter().any(|c| c.path == "src/core/lib.rs"));

    // Multi-selection: net diff of initial..HEAD~1 range == HEAD~1 alone here.
    let range = load_commit_details_sync(
        &loc,
        &DetailsKey::commits(vec![change, rev(dir, "HEAD~2")], 0),
        &token,
    )
    .unwrap();
    assert!(range.changes.iter().any(|c| c.path == "src/core/lib.rs"));
    assert!(range.changes.iter().any(|c| c.path == "new.txt"));

    // Interleaved-branch multi-selection: selecting [side, initial] does NOT pull in `change` (main's commit).
    let side = rev(dir, "side");
    let init = rev(dir, "HEAD~2");
    let cross = load_commit_details_sync(
        &loc,
        &DetailsKey::commits(vec![side, init], 0),
        &token,
    )
    .unwrap();
    let cross_paths: Vec<_> = cross.changes.iter().map(|c| c.path.as_str()).collect();
    assert!(cross_paths.contains(&"side.txt"), "{cross_paths:?}");
    assert!(cross_paths.contains(&"src/core/lib.rs"), "{cross_paths:?}");
    assert!(
        !cross_paths.contains(&"new.txt"),
        "interleaved main commit must not leak: {cross_paths:?}"
    );

    // File diff with word-level ranges.
    let diff = load_file_diff_sync(
        &loc,
        &FileDiffKey {
            details: DetailsKey::single(change),
            path: "src/core/lib.rs".into(),
            old_path: None,
            context: DiffContext::Three,
        },
        &token,
    )
    .unwrap();
    assert!(!diff.is_binary);
    let lines = &diff.hunks[0].lines;
    let del = lines
        .iter()
        .find(|l| l.kind == DiffLineKind::Deleted)
        .unwrap();
    let add = lines
        .iter()
        .find(|l| l.kind == DiffLineKind::Added)
        .unwrap();
    assert_eq!(&del.text[del.word_ranges[0].0..del.word_ranges[0].1], "1");
    assert_eq!(&add.text[add.word_ranges[0].0..add.word_ranges[0].1], "2");

    // Path filter: only commits touching src/core (initial + change; merge is TREESAME to parent[0]).
    let snap = load_repo_snapshot_sync(&loc, &token, 100).unwrap();
    let matches = load_path_matches_sync(&loc, &["src/core".to_string()], &snap, &token).unwrap();
    let mut got: Vec<_> = matches.into_iter().collect();
    got.sort();
    let mut want = vec![change, rev(dir, "HEAD~2")];
    want.sort();
    assert_eq!(got, want);
}

#[test]
fn partial_hunk_and_line_staging_matches_native_git_diff() {
    use jbgit_core::{DiffContext, LineSelection, RequestEpoch};
    use jbgit_engine::{
        StageDiffKey, StageDiffSide, apply_stage_buffer_sync, load_stage_file_diff_sync,
    };

    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();
    git(dir, &["init", "-b", "main"]);

    let initial = "\
1: keep
2: hunk1_old
3: keep
4: keep
5: keep
6: keep
7: keep
8: hunk2_old_a
9: hunk2_old_b
10: keep
11: keep
12: keep
13: keep
14: keep
15: hunk3_old
16: keep
";
    fs::write(dir.join("multi.txt"), initial).unwrap();
    git(dir, &["add", "multi.txt"]);
    git(dir, &["commit", "-m", "base"]);

    let modified = "\
1: keep
2: hunk1_new
3: keep
4: keep
5: keep
6: keep
7: keep
8: hunk2_new_a
9: hunk2_new_b
10: hunk2_new_c
11: keep
12: keep
13: keep
14: keep
15: keep
16: hunk3_new
17: keep
";
    fs::write(dir.join("multi.txt"), modified).unwrap();

    let loc = RepoLocation::discover(dir).unwrap();
    let token = RequestEpoch::new().issue();
    let diff = load_stage_file_diff_sync(
        &loc,
        &StageDiffKey {
            path: "multi.txt".into(),
            old_path: None,
            side: StageDiffSide::Unstaged,
            context: DiffContext::Three,
        },
        &token,
    )
    .unwrap();

    // Expect 3 distinct unstaged blocks
    assert_eq!(diff.tracker.unstaged_blocks.len(), 3);

    // Stage ONLY 2 lines from hunk 2 (block index 1):
    // delete line index 7 (`8: hunk2_old_a`) and add line index 7 (`8: hunk2_new_a`) and index 9 (`10: hunk2_new_c`)
    let b1 = diff.tracker.unstaged_blocks[1];
    let new_staged = diff.tracker.stage_lines(&LineSelection::from_iters(
        false,
        [b1.start1],
        [b1.start2, b1.start2 + 2],
    ));
    apply_stage_buffer_sync(&loc, "multi.txt", &new_staged, 0o100644, false).unwrap();

    // Worktree must be completely untouched
    assert_eq!(fs::read_to_string(dir.join("multi.txt")).unwrap(), modified);

    // Verify native `git diff --cached` and `git diff`
    let cached_out = Command::new("git")
        .args(["diff", "--cached"])
        .current_dir(dir)
        .output()
        .unwrap();
    let cached_str = String::from_utf8_lossy(&cached_out.stdout);
    assert!(cached_str.contains("-8: hunk2_old_a"), "{cached_str}");
    assert!(cached_str.contains("+8: hunk2_new_a"), "{cached_str}");
    assert!(cached_str.contains("+10: hunk2_new_c"), "{cached_str}");
    assert!(!cached_str.contains("hunk1_new"), "{cached_str}");
    assert!(!cached_str.contains("hunk3_new"), "{cached_str}");

    let wt_out = Command::new("git")
        .args(["diff"])
        .current_dir(dir)
        .output()
        .unwrap();
    let wt_str = String::from_utf8_lossy(&wt_out.stdout);
    assert!(wt_str.contains("+2: hunk1_new"), "{wt_str}");
    assert!(wt_str.contains("+9: hunk2_new_b"), "{wt_str}");
    assert!(wt_str.contains("+16: hunk3_new"), "{wt_str}");
}

#[test]
fn partial_stage_untracked_file_and_preserve_executable_mode() {
    use jbgit_core::{DiffContext, LineSelection, RequestEpoch};
    use jbgit_engine::{
        StageDiffKey, StageDiffSide, apply_stage_buffer_sync, load_stage_file_diff_sync,
    };

    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();
    git(dir, &["init", "-b", "main"]);
    fs::write(dir.join("init.txt"), "0\n").unwrap();
    git(dir, &["add", "."]);
    git(dir, &["commit", "-m", "init"]);

    let script = (1..=10).map(|i| format!("line {i}\n")).collect::<String>();
    fs::write(dir.join("run.sh"), &script).unwrap();

    let loc = RepoLocation::discover(dir).unwrap();
    let token = RequestEpoch::new().issue();
    let diff = load_stage_file_diff_sync(
        &loc,
        &StageDiffKey {
            path: "run.sh".into(),
            old_path: None,
            side: StageDiffSide::Unstaged,
            context: DiffContext::Three,
        },
        &token,
    )
    .unwrap();

    // Stage only the first 5 lines with 0o100755 mode
    let staged_buf = diff
        .tracker
        .stage_lines(&LineSelection::from_iters(false, [], 0..5));
    apply_stage_buffer_sync(&loc, "run.sh", &staged_buf, 0o100755, false).unwrap();

    // Verify worktree still has 10 lines, while Index has 5 lines with 100755 mode
    assert_eq!(fs::read_to_string(dir.join("run.sh")).unwrap(), script);
    let ls_out = Command::new("git")
        .args(["ls-files", "--stage", "run.sh"])
        .current_dir(dir)
        .output()
        .unwrap();
    let ls_str = String::from_utf8_lossy(&ls_out.stdout);
    assert!(ls_str.starts_with("100755 "), "expected 100755 mode: {ls_str}");
}

#[tokio::test]
async fn commit_unborn_amend_undo_and_hooks_workflow() {
    use jbgit_engine::{
        CommitRequest, EngineEvent, EngineSupervisor, TaskRequest, execute_commit_git2_sync,
        load_amend_preview_sync, undo_last_commit_sync,
    };

    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();
    git(dir, &["init", "-b", "main"]);
    git(dir, &["config", "user.name", "Tester"]);
    git(dir, &["config", "user.email", "tester@example.com"]);

    fs::write(dir.join("a.txt"), "hello\n").unwrap();
    git(dir, &["add", "a.txt"]);

    let loc = RepoLocation::discover(dir).unwrap();

    // 1. Unborn HEAD initial commit
    let res1 = execute_commit_git2_sync(
        &loc,
        &CommitRequest {
            message: "feat: initial commit".into(),
            amend: false,
            signoff: true,
            run_hooks: true,
            author_override: None,
            stage_all_tracked: false,
            and_push: false,
        },
    )
    .unwrap();
    assert!(res1.success);

    // 2. Amend commit
    fs::write(dir.join("a.txt"), "hello world\n").unwrap();
    git(dir, &["add", "a.txt"]);
    let preview = load_amend_preview_sync(&loc).unwrap().unwrap();
    assert!(preview.message.contains("Signed-off-by: Tester <tester@example.com>"));

    let res2 = execute_commit_git2_sync(
        &loc,
        &CommitRequest {
            message: "feat: amended commit".into(),
            amend: true,
            signoff: true,
            run_hooks: true,
            author_override: None,
            stage_all_tracked: false,
            and_push: false,
        },
    )
    .unwrap();
    assert!(res2.success);
    assert!(res2.commit_oid.is_some());

    // 3. Regular second commit + Undo commit
    fs::write(dir.join("b.txt"), "b\n").unwrap();
    git(dir, &["add", "b.txt"]);
    let res3 = execute_commit_git2_sync(
        &loc,
        &CommitRequest {
            message: "feat: second".into(),
            amend: false,
            signoff: false,
            run_hooks: true,
            author_override: None,
            stage_all_tracked: false,
            and_push: false,
        },
    )
    .unwrap();
    assert!(res3.success);
    let undone_msg = undo_last_commit_sync(&loc).unwrap();
    assert_eq!(undone_msg, "feat: second");

    // 4. Hook rejection with run_hooks=true vs bypass with run_hooks=false (--no-verify)
    if GitCliExecutor::detect().is_ok() {
        let hooks_dir = dir.join(".git/hooks");
        fs::create_dir_all(&hooks_dir).unwrap();
        let hook_path = hooks_dir.join("pre-commit");
        fs::write(&hook_path, "#!/bin/sh\necho 'lint failed: bad code' >&2\nexit 1\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&hook_path).unwrap().permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&hook_path, perms).unwrap();
        }

        let (handle, mut rx) = EngineSupervisor::start(loc.clone(), false).unwrap();
        handle.send(TaskRequest::ExecuteCommit {
            id: 1,
            request: CommitRequest {
                message: "feat: blocked by hook".into(),
                amend: false,
                signoff: false,
                run_hooks: true,
                author_override: None,
                stage_all_tracked: false,
                and_push: false,
            },
        });

        let mut outcome1 = None;
        let deadline = tokio::time::sleep(Duration::from_secs(5));
        tokio::pin!(deadline);
        while outcome1.is_none() {
            tokio::select! {
                Some(ev) = rx.recv() => {
                    if let EngineEvent::CommitCompleted(o) = ev {
                        outcome1 = Some(o);
                    }
                }
                _ = &mut deadline => panic!("timed out waiting for hook failure"),
            }
        }
        let o1 = outcome1.unwrap();
        assert!(!o1.success, "expected pre-commit hook to fail");
        assert_eq!(o1.preserved_message.as_deref(), Some("feat: blocked by hook"));
        assert!(
            o1.error_output
                .as_deref()
                .unwrap_or("")
                .contains("lint failed"),
            "{:?}",
            o1.error_output
        );

        // Now bypass with run_hooks = false (--no-verify)
        handle.send(TaskRequest::ExecuteCommit {
            id: 2,
            request: CommitRequest {
                message: "feat: bypass hook".into(),
                amend: false,
                signoff: false,
                run_hooks: false,
                author_override: None,
                stage_all_tracked: false,
                and_push: false,
            },
        });

        let mut outcome2 = None;
        let deadline2 = tokio::time::sleep(Duration::from_secs(5));
        tokio::pin!(deadline2);
        while outcome2.is_none() {
            tokio::select! {
                Some(ev) = rx.recv() => {
                    if let EngineEvent::CommitCompleted(o) = ev {
                        outcome2 = Some(o);
                    }
                }
                _ = &mut deadline2 => panic!("timed out waiting for --no-verify commit"),
            }
        }
        let o2 = outcome2.unwrap();
        assert!(o2.success, "expected --no-verify commit to succeed: {:?}", o2.error_output);
    }
}

#[test]
fn amend_commit_side_loads_head_parent_diff_even_when_clean() {
    use jbgit_core::{DiffContext, DiffLineKind, RequestEpoch};
    use jbgit_engine::{StageDiffKey, StageDiffSide, load_stage_file_diff_sync};

    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();
    git(dir, &["init", "-b", "main"]);
    git(dir, &["config", "user.name", "Tester"]);
    git(dir, &["config", "user.email", "tester@example.com"]);

    fs::write(dir.join("note.txt"), "before line\n").unwrap();
    git(dir, &["add", "note.txt"]);
    git(dir, &["commit", "-m", "first"]);

    fs::write(dir.join("note.txt"), "after line\n").unwrap();
    git(dir, &["add", "note.txt"]);
    git(dir, &["commit", "-m", "second"]);

    let loc = RepoLocation::discover(dir).unwrap();
    let token = RequestEpoch::new().issue();
    let diff = load_stage_file_diff_sync(
        &loc,
        &StageDiffKey {
            path: "note.txt".into(),
            old_path: None,
            side: StageDiffSide::AmendCommit,
            context: DiffContext::Three,
        },
        &token,
    )
    .unwrap();

    assert_eq!(diff.hunks.len(), 1);
    let lines = &diff.hunks[0].hunk.lines;
    assert!(
        lines
            .iter()
            .any(|l| l.kind == DiffLineKind::Deleted && l.text == "before line")
    );
    assert!(
        lines
            .iter()
            .any(|l| l.kind == DiffLineKind::Added && l.text == "after line")
    );
}

#[tokio::test]
async fn file_history_follow_renames_cjk_diffs_and_supervisor_epoch_cancellation() {
    use jbgit_core::{
        DetailsKey, DiffContext, FileDiffKey, FileHistoryKey, FileStatusKind, Oid,
    };
    use jbgit_engine::{load_file_diff_sync, load_file_history_sync};

    // 1. Unborn repo gracefully returns empty entries
    let empty_tmp = TempDir::new().unwrap();
    git(empty_tmp.path(), &["init", "-b", "main"]);
    let empty_loc = RepoLocation::discover(empty_tmp.path()).unwrap();
    let token = RequestEpoch::new().issue();
    let empty_hist = load_file_history_sync(
        &empty_loc,
        &FileHistoryKey {
            path: "missing.txt".into(),
            start_oid: None,
            follow_renames: true,
        },
        &token,
    )
    .unwrap();
    assert!(empty_hist.entries.is_empty());
    assert!(!empty_hist.truncated);

    // 2. Rename chain with spaces + CJK filename
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();
    git(dir, &["init", "-b", "main"]);
    git(dir, &["config", "user.name", "Tester"]);
    git(dir, &["config", "user.email", "tester@example.com"]);
    git(dir, &["config", "core.autocrlf", "false"]);

    let base_lines = "line 1\nline 2\nline 3\nline 4\nline 5\nline 6\nline 7\n";
    fs::write(dir.join("a.txt"), base_lines).unwrap();
    git(dir, &["add", "a.txt"]);
    git(dir, &["commit", "-m", "c1: add a.txt"]);

    fs::write(dir.join("a.txt"), format!("{base_lines}line 8\n")).unwrap();
    git(dir, &["add", "a.txt"]);
    git(dir, &["commit", "-m", "c2: edit a.txt"]);

    fs::create_dir_all(dir.join("sub")).unwrap();
    git(dir, &["mv", "a.txt", "sub/b 中文.txt"]);
    git(dir, &["commit", "-m", "c3: rename to sub/b 中文.txt"]);

    fs::write(
        dir.join("sub/b 中文.txt"),
        format!("{base_lines}line 8\nline 9\n"),
    )
    .unwrap();
    git(dir, &["add", "sub/b 中文.txt"]);
    git(dir, &["commit", "-m", "c4: edit sub/b 中文.txt"]);

    let loc = RepoLocation::discover(dir).unwrap();
    let follow = load_file_history_sync(
        &loc,
        &FileHistoryKey {
            path: "sub/b 中文.txt".into(),
            start_oid: None,
            follow_renames: true,
        },
        &token,
    )
    .unwrap();
    assert!(!follow.truncated);
    assert_eq!(follow.entries.len(), 4);
    assert_eq!(follow.entries[0].summary, "c4: edit sub/b 中文.txt");
    assert_eq!(follow.entries[0].path_at_commit, "sub/b 中文.txt");
    assert_eq!(follow.entries[0].kind, FileStatusKind::Modified);

    assert_eq!(follow.entries[1].summary, "c3: rename to sub/b 中文.txt");
    assert_eq!(follow.entries[1].kind, FileStatusKind::Renamed);
    assert_eq!(follow.entries[1].old_path.as_deref(), Some("a.txt"));
    assert_eq!(follow.entries[1].path_at_commit, "sub/b 中文.txt");

    assert_eq!(follow.entries[2].summary, "c2: edit a.txt");
    assert_eq!(follow.entries[2].path_at_commit, "a.txt");
    assert_eq!(follow.entries[2].kind, FileStatusKind::Modified);

    assert_eq!(follow.entries[3].summary, "c1: add a.txt");
    assert_eq!(follow.entries[3].path_at_commit, "a.txt");
    assert_eq!(follow.entries[3].kind, FileStatusKind::Added);

    // Every entry in the chain loads its single-commit diff cleanly
    for entry in &follow.entries {
        let diff = load_file_diff_sync(
            &loc,
            &FileDiffKey {
                details: DetailsKey::single(entry.oid),
                path: entry.path_at_commit.clone(),
                old_path: entry.old_path.clone(),
                context: DiffContext::Three,
            },
            &token,
        )
        .unwrap();
        if entry.kind != FileStatusKind::Renamed {
            assert!(
                !diff.hunks.is_empty(),
                "expected diff hunks for {} ({})",
                entry.summary,
                entry.path_at_commit
            );
        }
    }

    // Cross-rename CompareVersions (c1 vs c4) and CompareWithLocal (c1 vs worktree)
    let cmp_versions = load_file_diff_sync(
        &loc,
        &FileDiffKey {
            details: DetailsKey::compare_versions(follow.entries[3].oid, follow.entries[0].oid),
            path: follow.entries[0].path_at_commit.clone(),
            old_path: Some(follow.entries[3].path_at_commit.clone()),
            context: DiffContext::Three,
        },
        &token,
    )
    .unwrap();
    assert!(!cmp_versions.hunks.is_empty());

    fs::write(
        dir.join("sub/b 中文.txt"),
        format!("{base_lines}line 8\nline 9\nlocal edit\n"),
    )
    .unwrap();
    let cmp_local = load_file_diff_sync(
        &loc,
        &FileDiffKey {
            details: DetailsKey::compare_with_local(follow.entries[3].oid),
            path: "sub/b 中文.txt".into(),
            old_path: Some(follow.entries[3].path_at_commit.clone()),
            context: DiffContext::Three,
        },
        &token,
    )
    .unwrap();
    assert!(!cmp_local.hunks.is_empty());

    // follow_renames = false stops at rename
    let no_follow = load_file_history_sync(
        &loc,
        &FileHistoryKey {
            path: "sub/b 中文.txt".into(),
            start_oid: None,
            follow_renames: false,
        },
        &token,
    )
    .unwrap();
    assert_eq!(no_follow.entries.len(), 2);

    // Non-existent path returns empty gracefully
    let missing = load_file_history_sync(
        &loc,
        &FileHistoryKey {
            path: "does/not/exist.txt".into(),
            start_oid: Some(Oid::from_bytes(follow.entries[0].oid.as_bytes().to_owned())),
            follow_renames: true,
        },
        &token,
    )
    .unwrap();
    assert!(missing.entries.is_empty());

    // Supervisor rapid requests drop stale epochs and do not pollute Console
    let (handle, mut rx) = EngineSupervisor::start(loc, false).unwrap();
    let e1 = handle.request_file_history(FileHistoryKey {
        path: "sub/b 中文.txt".into(),
        start_oid: None,
        follow_renames: true,
    });
    let e2 = handle.request_file_history(FileHistoryKey {
        path: "sub/b 中文.txt".into(),
        start_oid: None,
        follow_renames: false,
    });
    assert!(e2 > e1);

    let latest;
    let deadline = tokio::time::sleep(Duration::from_secs(5));
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            Some(ev) = rx.recv() => {
                match ev {
                    EngineEvent::ConsoleStarted(_) => {
                        panic!("read-only file history must not emit ConsoleStarted");
                    }
                    EngineEvent::FileHistoryLoaded { bundle, epoch } if epoch == e2 => {
                        latest = Some(bundle);
                        break;
                    }
                    _ => {}
                }
            }
            _ = &mut deadline => panic!("timed out waiting for FileHistoryLoaded epoch {e2}"),
        }
    }
    assert_eq!(latest.unwrap().entries.len(), 2);
}
