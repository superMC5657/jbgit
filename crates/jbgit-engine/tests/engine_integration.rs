use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use jbgit_core::{
    AmendCommitPreview, BlameBundle, BlameKey, BlameOptions, CommitChangesBundle, DetailsKey,
    DiffContext, DiffLineKind, FileDiff, FileDiffKey, FileHistoryBundle, FileHistoryKey,
    FileStatusKind, LineSelection, MAX_DIFF_FILE_BYTES, Oid, RefKind, RefreshScope, RepoSnapshot,
    RepositoryState, RequestSlotId, StageStatusSnapshot,
};
use jbgit_engine::{
    BlameFailureKind, CommitOutcome, CommitRequest, EngineBackend, EngineEvent, EngineHandle,
    EngineSupervisor, RecordingEngineBackend, RepoLocation, StageDiffKey, StageDiffSide,
    TaskRequest, ThreeDocStageDiff,
};
use tempfile::TempDir;
use tokio::sync::mpsc::UnboundedReceiver;

fn git_output(dir: &Path, args: &[&str]) -> std::process::Output {
    Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "Test User")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "Test User")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .output()
        .expect("failed to execute git")
}

fn git(dir: &Path, args: &[&str]) {
    let output = git_output(dir, args);
    assert!(
        output.status.success(),
        "git {:?} failed: stdout={}, stderr={}",
        args,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn rev(dir: &Path, spec: &str) -> Oid {
    let out = Command::new("git")
        .args(["rev-parse", spec])
        .current_dir(dir)
        .output()
        .expect("git rev-parse");
    String::from_utf8_lossy(&out.stdout).trim().parse().unwrap()
}

async fn recv_matching<T>(
    rx: &mut UnboundedReceiver<EngineEvent>,
    mut predicate: impl FnMut(EngineEvent) -> Option<T>,
) -> T {
    tokio::time::timeout(Duration::from_secs(10), async {
        while let Some(ev) = rx.recv().await {
            if let Some(val) = predicate(ev) {
                return val;
            }
        }
        panic!("engine event channel closed before expected event");
    })
    .await
    .expect("timed out waiting for engine event")
}

async fn fetch_snapshot(
    handle: &EngineHandle,
    rx: &mut UnboundedReceiver<EngineEvent>,
) -> Arc<RepoSnapshot> {
    let epoch = handle.request_repo_snapshot();
    recv_matching(rx, |ev| match ev {
        EngineEvent::RepoSnapshotLoaded { snapshot, epoch: e } if e == epoch => Some(snapshot),
        _ => None,
    })
    .await
}

async fn fetch_stage_status(
    handle: &EngineHandle,
    rx: &mut UnboundedReceiver<EngineEvent>,
    include_ignored: bool,
) -> Arc<StageStatusSnapshot> {
    let epoch = handle.request_stage_status(include_ignored);
    recv_matching(rx, |ev| match ev {
        EngineEvent::StageStatusLoaded { status, epoch: e } if e == epoch => Some(status),
        _ => None,
    })
    .await
}

async fn fetch_commit_details(
    handle: &EngineHandle,
    rx: &mut UnboundedReceiver<EngineEvent>,
    slot: RequestSlotId,
    key: DetailsKey,
) -> Arc<CommitChangesBundle> {
    let epoch = handle.request_commit_details(slot, key);
    recv_matching(rx, |ev| match ev {
        EngineEvent::CommitDetailsLoaded {
            slot: s,
            bundle,
            epoch: e,
        } if s == slot && e == epoch => Some(bundle),
        _ => None,
    })
    .await
}

async fn fetch_file_diff(
    handle: &EngineHandle,
    rx: &mut UnboundedReceiver<EngineEvent>,
    slot: RequestSlotId,
    key: FileDiffKey,
    known_oids: Option<(Option<Oid>, Option<Oid>)>,
) -> Arc<FileDiff> {
    let epoch = handle.request_file_diff_with_oids(slot, key, known_oids);
    recv_matching(rx, |ev| match ev {
        EngineEvent::FileDiffLoaded {
            slot: s,
            diff,
            epoch: e,
        } if s == slot && e == epoch => Some(diff),
        _ => None,
    })
    .await
}

async fn fetch_file_history(
    handle: &EngineHandle,
    rx: &mut UnboundedReceiver<EngineEvent>,
    key: FileHistoryKey,
) -> Arc<FileHistoryBundle> {
    let epoch = handle.request_file_history(key);
    recv_matching(rx, |ev| match ev {
        EngineEvent::FileHistoryLoaded { bundle, epoch: e } if e == epoch => Some(bundle),
        _ => None,
    })
    .await
}

async fn fetch_blame(
    handle: &EngineHandle,
    rx: &mut UnboundedReceiver<EngineEvent>,
    key: BlameKey,
) -> Result<Arc<BlameBundle>, (BlameFailureKind, String)> {
    let epoch = handle.request_blame(key);
    recv_matching(rx, |ev| match ev {
        EngineEvent::ConsoleStarted(_) => {
            panic!("read-only blame must not emit ConsoleStarted");
        }
        EngineEvent::BlameLoaded { bundle, epoch: e } if e == epoch => Some(Ok(bundle)),
        EngineEvent::BlameFailed {
            kind,
            message,
            epoch: e,
            ..
        } if e == epoch => Some(Err((kind, message))),
        _ => None,
    })
    .await
}

async fn fetch_stage_diff(
    handle: &EngineHandle,
    rx: &mut UnboundedReceiver<EngineEvent>,
    key: StageDiffKey,
) -> Arc<ThreeDocStageDiff> {
    let epoch = handle.request_stage_file_diff(key);
    recv_matching(rx, |ev| match ev {
        EngineEvent::StageFileDiffLoaded { diff, epoch: e } if e == epoch => Some(diff),
        _ => None,
    })
    .await
}

async fn fetch_amend_preview(
    handle: &EngineHandle,
    rx: &mut UnboundedReceiver<EngineEvent>,
) -> Option<AmendCommitPreview> {
    let epoch = handle.request_amend_preview();
    recv_matching(rx, |ev| match ev {
        EngineEvent::AmendPreviewLoaded { preview, epoch: e } if e == epoch => Some(preview),
        _ => None,
    })
    .await
}

async fn exec_commit(
    handle: &EngineHandle,
    rx: &mut UnboundedReceiver<EngineEvent>,
    id: u64,
    request: CommitRequest,
) -> CommitOutcome {
    let _ = handle.dispatch(TaskRequest::ExecuteCommit { id, request });
    recv_matching(rx, |ev| match ev {
        EngineEvent::CommitCompleted(o) => Some(o),
        _ => None,
    })
    .await
}

#[tokio::test]
async fn unborn_empty_repo_and_stage_status() {
    let tmp = TempDir::new().unwrap();
    git(tmp.path(), &["init", "-b", "main"]);
    fs::write(tmp.path().join("hello.txt"), "hello\n").unwrap();

    let loc = RepoLocation::discover(tmp.path()).unwrap();
    let (handle, mut rx) = EngineSupervisor::start(loc, false).unwrap();

    let snap = fetch_snapshot(&handle, &mut rx).await;
    assert!(snap.is_unborn_head);
    assert_eq!(snap.head_branch.as_deref(), Some("main"));
    assert_eq!(snap.state, RepositoryState::Clean);
    assert!(snap.commits.is_empty());

    let stage = fetch_stage_status(&handle, &mut rx, false).await;
    assert_eq!(stage.untracked.len(), 1);
    assert_eq!(stage.untracked[0].path, "hello.txt");
}

#[tokio::test]
async fn branches_tags_and_ahead_behind_counts() {
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

    let work_b = TempDir::new().unwrap();
    git(
        work_b.path(),
        &["clone", remote_dir.path().to_str().unwrap(), "."],
    );
    fs::write(work_b.path().join("b.txt"), "from b\n").unwrap();
    git(work_b.path(), &["add", "."]);
    git(work_b.path(), &["commit", "-m", "remote commit"]);
    git(work_b.path(), &["push", "origin", "main"]);

    git(work_a.path(), &["fetch", "origin"]);
    fs::write(work_a.path().join("a.txt"), "v2\n").unwrap();
    git(work_a.path(), &["commit", "-am", "local 1"]);
    fs::write(work_a.path().join("a.txt"), "v3\n").unwrap();
    git(work_a.path(), &["commit", "-am", "local 2"]);

    git(work_a.path(), &["branch", "feature/ui"]);
    git(work_a.path(), &["tag", "-a", "v0.1.0", "-m", "release"]);

    let loc = RepoLocation::discover(work_a.path()).unwrap();
    let (handle, mut rx) = EngineSupervisor::start(loc, false).unwrap();
    let snap = fetch_snapshot(&handle, &mut rx).await;

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

#[tokio::test]
async fn detects_rebase_conflict_state_and_progress() {
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
    let (handle, mut rx) = EngineSupervisor::start(loc, false).unwrap();
    let snap = fetch_snapshot(&handle, &mut rx).await;

    match &snap.state {
        RepositoryState::RebaseMerge { progress, branch }
        | RepositoryState::RebaseInteractive { progress, branch }
        | RepositoryState::Rebase { progress, branch } => {
            assert_eq!(*progress, Some((1, 1)));
            assert_eq!(branch.as_deref(), Some("feat"));
        }
        other => panic!("expected rebase state, got {other:?}"),
    }

    let stage = fetch_stage_status(&handle, &mut rx, false).await;
    assert_eq!(stage.conflicted.len(), 1);
    assert_eq!(stage.conflicted[0].path, "conflict.txt");
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

    if handle.git_info().is_some() {
        let _ = handle.dispatch(TaskRequest::RunGitCli {
            id: 42,
            args: vec!["status".into(), "--short".into()],
            stdin_payload: None,
            is_read_only: true,
            refresh_after: RefreshScope::None,
        });

        let mut saw_started = false;
        let mut saw_finished = false;
        tokio::time::timeout(Duration::from_secs(5), async {
            while !(saw_started && saw_finished) {
                match rx.recv().await {
                    Some(EngineEvent::ConsoleStarted(entry)) if entry.id == 42 => {
                        saw_started = true;
                        assert!(entry.command_line.contains("git status"));
                    }
                    Some(EngineEvent::ConsoleFinished {
                        id: 42, exit_code, ..
                    }) => {
                        assert_eq!(exit_code, 0);
                        saw_finished = true;
                    }
                    Some(_) => {}
                    None => panic!("channel closed"),
                }
            }
        })
        .await
        .expect("timed out waiting for CLI console events");
    }

    // Trigger external FS changes and verify watcher notifications arrive deterministically.
    fs::write(tmp.path().join("README.md"), "modified\n").unwrap();
    recv_matching(&mut rx, |ev| match ev {
        EngineEvent::ExternalFsChanged(scope) if scope.needs_stage_refresh() => Some(()),
        _ => None,
    })
    .await;

    git(tmp.path(), &["branch", "foo"]);
    recv_matching(&mut rx, |ev| match ev {
        EngineEvent::ExternalFsChanged(scope) if scope.needs_graph_refresh() => Some(()),
        _ => None,
    })
    .await;
}

#[tokio::test]
async fn sibling_consumer_file_diff_slots_never_cancel_each_other() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();
    git(dir, &["init", "-b", "main"]);
    fs::write(dir.join("a.txt"), "a1\n").unwrap();
    fs::write(dir.join("b.txt"), "b1\n").unwrap();
    fs::write(dir.join("c.txt"), "c1\n").unwrap();
    git(dir, &["add", "."]);
    git(dir, &["commit", "-m", "c1"]);

    fs::write(dir.join("a.txt"), "a2\n").unwrap();
    fs::write(dir.join("b.txt"), "b2\n").unwrap();
    fs::write(dir.join("c.txt"), "c2\n").unwrap();
    git(dir, &["add", "."]);
    git(dir, &["commit", "-m", "c2"]);
    let c2 = rev(dir, "HEAD");

    let loc = RepoLocation::discover(dir).unwrap();
    let (handle, mut rx) = EngineSupervisor::start(loc, false).unwrap();

    // Interleave requests across three distinct consumer slots concurrently.
    let key_a = FileDiffKey {
        details: DetailsKey::single(c2),
        path: "a.txt".into(),
        old_path: None,
        context: DiffContext::Three,
    };
    let key_b = FileDiffKey {
        details: DetailsKey::single(c2),
        path: "b.txt".into(),
        old_path: None,
        context: DiffContext::Three,
    };
    let key_c = FileDiffKey {
        details: DetailsKey::single(c2),
        path: "c.txt".into(),
        old_path: None,
        context: DiffContext::Three,
    };

    let e_log = handle.request_file_diff(RequestSlotId::LogFileDiff, key_a.clone());
    let e_fh = handle.request_file_diff(RequestSlotId::FileHistoryDiff, key_b.clone());
    let e_stash = handle.request_file_diff(RequestSlotId::StashFileDiff, key_c.clone());

    assert_eq!(e_log, 1);
    assert_eq!(e_fh, 1);
    assert_eq!(e_stash, 1);

    let mut got_log = false;
    let mut got_fh = false;
    let mut got_stash = false;
    tokio::time::timeout(Duration::from_secs(5), async {
        while !(got_log && got_fh && got_stash) {
            match rx.recv().await {
                Some(EngineEvent::FileDiffLoaded { slot, diff, epoch }) => match slot {
                    RequestSlotId::LogFileDiff => {
                        assert_eq!(epoch, e_log);
                        assert_eq!(diff.key, key_a);
                        got_log = true;
                    }
                    RequestSlotId::FileHistoryDiff => {
                        assert_eq!(epoch, e_fh);
                        assert_eq!(diff.key, key_b);
                        got_fh = true;
                    }
                    RequestSlotId::StashFileDiff => {
                        assert_eq!(epoch, e_stash);
                        assert_eq!(diff.key, key_c);
                        got_stash = true;
                    }
                    other => panic!("unexpected slot {other:?}"),
                },
                Some(_) => {}
                None => panic!("channel closed before all three sibling diffs arrived"),
            }
        }
    })
    .await
    .expect("sibling consumer diff requests must all complete without cancelling each other");

    // Within the SAME slot, a newer request supersedes the older epoch and completes cleanly.
    let old_e = handle.request_file_diff(RequestSlotId::LogFileDiff, key_a);
    let new_e = handle.request_file_diff(RequestSlotId::LogFileDiff, key_b.clone());
    assert!(new_e > old_e);

    let final_diff = recv_matching(&mut rx, |ev| match ev {
        EngineEvent::FileDiffLoaded {
            slot: RequestSlotId::LogFileDiff,
            diff,
            epoch,
        } if epoch == new_e => Some(diff),
        _ => None,
    })
    .await;
    assert_eq!(final_diff.key, key_b);
}

#[test]
fn recording_engine_backend_assigns_independent_per_slot_epochs() {
    let backend = RecordingEngineBackend::new();
    let e_log_1 = backend.request_file_diff(
        RequestSlotId::LogFileDiff,
        FileDiffKey {
            details: DetailsKey::single(Oid::ZERO),
            path: "a.rs".into(),
            old_path: None,
            context: DiffContext::Three,
        },
    );
    let e_fh_1 = backend.request_file_diff(
        RequestSlotId::FileHistoryDiff,
        FileDiffKey {
            details: DetailsKey::single(Oid::ZERO),
            path: "b.rs".into(),
            old_path: None,
            context: DiffContext::Three,
        },
    );
    let e_log_2 = backend.request_file_diff(
        RequestSlotId::LogFileDiff,
        FileDiffKey {
            details: DetailsKey::single(Oid::ZERO),
            path: "c.rs".into(),
            old_path: None,
            context: DiffContext::Three,
        },
    );
    assert_eq!(e_log_1, 1);
    assert_eq!(e_fh_1, 1);
    assert_eq!(e_log_2, 2);
    assert_eq!(backend.latest_epoch(RequestSlotId::LogFileDiff), 2);
    assert_eq!(backend.latest_epoch(RequestSlotId::FileHistoryDiff), 1);
    assert_eq!(backend.take_recorded().len(), 3);
}

#[tokio::test]
async fn commit_details_changes_file_diff_and_path_matches() {
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
    let (handle, mut rx) = EngineSupervisor::start(loc, false).unwrap();

    let change = rev(dir, "HEAD~1");
    let bundle = fetch_commit_details(
        &handle,
        &mut rx,
        RequestSlotId::LogCommitDetails,
        DetailsKey::single(change),
    )
    .await;
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

    let merge = rev(dir, "HEAD");
    let p0 = fetch_commit_details(
        &handle,
        &mut rx,
        RequestSlotId::LogCommitDetails,
        DetailsKey::single(merge),
    )
    .await;
    assert_eq!(
        p0.changes
            .iter()
            .map(|c| c.path.as_str())
            .collect::<Vec<_>>(),
        vec!["side.txt"]
    );
    let p1 = fetch_commit_details(
        &handle,
        &mut rx,
        RequestSlotId::LogCommitDetails,
        DetailsKey::commits(vec![merge], 1),
    )
    .await;
    assert!(p1.changes.iter().any(|c| c.path == "src/core/lib.rs"));

    let range = fetch_commit_details(
        &handle,
        &mut rx,
        RequestSlotId::LogCommitDetails,
        DetailsKey::commits(vec![change, rev(dir, "HEAD~2")], 0),
    )
    .await;
    assert!(range.changes.iter().any(|c| c.path == "src/core/lib.rs"));
    assert!(range.changes.iter().any(|c| c.path == "new.txt"));

    let side = rev(dir, "side");
    let init = rev(dir, "HEAD~2");
    let cross = fetch_commit_details(
        &handle,
        &mut rx,
        RequestSlotId::LogCommitDetails,
        DetailsKey::commits(vec![side, init], 0),
    )
    .await;
    let cross_paths: Vec<_> = cross.changes.iter().map(|c| c.path.as_str()).collect();
    assert!(cross_paths.contains(&"side.txt"), "{cross_paths:?}");
    assert!(cross_paths.contains(&"src/core/lib.rs"), "{cross_paths:?}");
    assert!(
        !cross_paths.contains(&"new.txt"),
        "interleaved main commit must not leak: {cross_paths:?}"
    );

    let diff = fetch_file_diff(
        &handle,
        &mut rx,
        RequestSlotId::LogFileDiff,
        FileDiffKey {
            details: DetailsKey::single(change),
            path: "src/core/lib.rs".into(),
            old_path: None,
            context: DiffContext::Three,
        },
        None,
    )
    .await;
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

    let snap = fetch_snapshot(&handle, &mut rx).await;
    let pe = handle.request_path_matches(vec!["src/core".to_string()], snap);
    let matches = recv_matching(&mut rx, |ev| match ev {
        EngineEvent::PathMatchesLoaded { matches, epoch, .. } if epoch == pe => Some(matches),
        _ => None,
    })
    .await;
    let mut got: Vec<_> = matches.iter().copied().collect();
    got.sort();
    let mut want = vec![change, rev(dir, "HEAD~2")];
    want.sort();
    assert_eq!(got, want);
}

#[tokio::test]
async fn partial_hunk_and_line_staging_matches_native_git_diff() {
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
    let (handle, mut rx) = EngineSupervisor::start(loc, false).unwrap();

    let diff = fetch_stage_diff(
        &handle,
        &mut rx,
        StageDiffKey {
            path: "multi.txt".into(),
            old_path: None,
            side: StageDiffSide::Unstaged,
            context: DiffContext::Three,
        },
    )
    .await;
    assert_eq!(diff.tracker.unstaged_blocks.len(), 3);

    let b1 = diff.tracker.unstaged_blocks[1];
    let new_staged = diff.tracker.stage_lines(&LineSelection::from_iters(
        false,
        [b1.start1],
        [b1.start2, b1.start2 + 2],
    ));
    let _ = handle.dispatch(TaskRequest::ApplyStageBuffer {
        path: "multi.txt".into(),
        new_staged_bytes: new_staged,
        file_mode: 0o100644,
        remove_from_index: false,
    });
    recv_matching(&mut rx, |ev| match ev {
        EngineEvent::ExternalFsChanged(_) => Some(()),
        _ => None,
    })
    .await;

    assert_eq!(fs::read_to_string(dir.join("multi.txt")).unwrap(), modified);

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

#[tokio::test]
async fn partial_stage_untracked_file_and_preserve_executable_mode() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();
    git(dir, &["init", "-b", "main"]);
    fs::write(dir.join("init.txt"), "0\n").unwrap();
    git(dir, &["add", "."]);
    git(dir, &["commit", "-m", "init"]);

    let script = (1..=10).map(|i| format!("line {i}\n")).collect::<String>();
    fs::write(dir.join("run.sh"), &script).unwrap();

    let loc = RepoLocation::discover(dir).unwrap();
    let (handle, mut rx) = EngineSupervisor::start(loc, false).unwrap();

    let diff = fetch_stage_diff(
        &handle,
        &mut rx,
        StageDiffKey {
            path: "run.sh".into(),
            old_path: None,
            side: StageDiffSide::Unstaged,
            context: DiffContext::Three,
        },
    )
    .await;

    let staged_buf = diff
        .tracker
        .stage_lines(&LineSelection::from_iters(false, [], 0..5));
    let _ = handle.dispatch(TaskRequest::ApplyStageBuffer {
        path: "run.sh".into(),
        new_staged_bytes: staged_buf,
        file_mode: 0o100755,
        remove_from_index: false,
    });
    recv_matching(&mut rx, |ev| match ev {
        EngineEvent::ExternalFsChanged(_) => Some(()),
        _ => None,
    })
    .await;

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
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();
    git(dir, &["init", "-b", "main"]);
    git(dir, &["config", "user.name", "Tester"]);
    git(dir, &["config", "user.email", "tester@example.com"]);

    fs::write(dir.join("a.txt"), "hello\n").unwrap();
    git(dir, &["add", "a.txt"]);

    let loc = RepoLocation::discover(dir).unwrap();
    let (handle, mut rx) = EngineSupervisor::start(loc, false).unwrap();

    let res1 = exec_commit(
        &handle,
        &mut rx,
        1,
        CommitRequest {
            message: "feat: initial commit".into(),
            amend: false,
            signoff: true,
            run_hooks: true,
            author_override: None,
            stage_all_tracked: false,
            and_push: false,
        },
    )
    .await;
    assert!(res1.success);

    fs::write(dir.join("a.txt"), "hello world\n").unwrap();
    git(dir, &["add", "a.txt"]);
    let preview = fetch_amend_preview(&handle, &mut rx).await.unwrap();
    assert!(
        preview
            .message
            .contains("Signed-off-by: Tester <tester@example.com>")
    );

    let res2 = exec_commit(
        &handle,
        &mut rx,
        2,
        CommitRequest {
            message: "feat: amended commit".into(),
            amend: true,
            signoff: true,
            run_hooks: true,
            author_override: None,
            stage_all_tracked: false,
            and_push: false,
        },
    )
    .await;
    assert!(res2.success);
    assert!(res2.commit_oid.is_some());

    fs::write(dir.join("b.txt"), "b\n").unwrap();
    git(dir, &["add", "b.txt"]);
    let res3 = exec_commit(
        &handle,
        &mut rx,
        3,
        CommitRequest {
            message: "feat: second".into(),
            amend: false,
            signoff: false,
            run_hooks: true,
            author_override: None,
            stage_all_tracked: false,
            and_push: false,
        },
    )
    .await;
    assert!(res3.success);

    let _ = handle.dispatch(TaskRequest::UndoLastCommit);
    let undone_msg = recv_matching(&mut rx, |ev| match ev {
        EngineEvent::UndoCommitCompleted { restored_message } => Some(restored_message),
        _ => None,
    })
    .await;
    assert_eq!(undone_msg, "feat: second");

    if handle.git_info().is_some() {
        let hooks_dir = dir.join(".git/hooks");
        fs::create_dir_all(&hooks_dir).unwrap();
        let hook_path = hooks_dir.join("pre-commit");
        fs::write(
            &hook_path,
            "#!/bin/sh\necho 'lint failed: bad code' >&2\nexit 1\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&hook_path).unwrap().permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&hook_path, perms).unwrap();
        }

        let o1 = exec_commit(
            &handle,
            &mut rx,
            4,
            CommitRequest {
                message: "feat: blocked by hook".into(),
                amend: false,
                signoff: false,
                run_hooks: true,
                author_override: None,
                stage_all_tracked: false,
                and_push: false,
            },
        )
        .await;
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

        let o2 = exec_commit(
            &handle,
            &mut rx,
            5,
            CommitRequest {
                message: "feat: bypass hook".into(),
                amend: false,
                signoff: false,
                run_hooks: false,
                author_override: None,
                stage_all_tracked: false,
                and_push: false,
            },
        )
        .await;
        assert!(
            o2.success,
            "expected --no-verify commit to succeed: {:?}",
            o2.error_output
        );
    }
}

#[tokio::test]
async fn amend_commit_side_loads_head_parent_diff_even_when_clean() {
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
    let (handle, mut rx) = EngineSupervisor::start(loc, false).unwrap();
    let diff = fetch_stage_diff(
        &handle,
        &mut rx,
        StageDiffKey {
            path: "note.txt".into(),
            old_path: None,
            side: StageDiffSide::AmendCommit,
            context: DiffContext::Three,
        },
    )
    .await;

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
    let empty_tmp = TempDir::new().unwrap();
    git(empty_tmp.path(), &["init", "-b", "main"]);
    let empty_loc = RepoLocation::discover(empty_tmp.path()).unwrap();
    let (empty_handle, mut empty_rx) = EngineSupervisor::start(empty_loc, false).unwrap();
    let empty_hist = fetch_file_history(
        &empty_handle,
        &mut empty_rx,
        FileHistoryKey {
            path: "missing.txt".into(),
            start_oid: None,
            follow_renames: true,
            line_range: None,
        },
    )
    .await;
    assert!(empty_hist.entries.is_empty());
    assert!(!empty_hist.truncated);

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
    let (handle, mut rx) = EngineSupervisor::start(loc, false).unwrap();

    let follow = fetch_file_history(
        &handle,
        &mut rx,
        FileHistoryKey {
            path: "sub/b 中文.txt".into(),
            start_oid: None,
            follow_renames: true,
            line_range: None,
        },
    )
    .await;
    assert!(!follow.truncated);
    assert_eq!(follow.entries.len(), 4);
    assert_eq!(follow.entries[0].summary, "c4: edit sub/b 中文.txt");
    assert_eq!(follow.entries[1].kind, FileStatusKind::Renamed);
    assert_eq!(follow.entries[1].old_path.as_deref(), Some("a.txt"));

    for entry in &follow.entries {
        let diff = fetch_file_diff(
            &handle,
            &mut rx,
            RequestSlotId::FileHistoryDiff,
            FileDiffKey {
                details: DetailsKey::single(entry.oid),
                path: entry.path_at_commit.clone(),
                old_path: entry.old_path.clone(),
                context: DiffContext::Three,
            },
            None,
        )
        .await;
        if entry.kind != FileStatusKind::Renamed {
            assert!(!diff.hunks.is_empty());
        }
    }

    let cmp_versions = fetch_file_diff(
        &handle,
        &mut rx,
        RequestSlotId::FileHistoryDiff,
        FileDiffKey {
            details: DetailsKey::compare_versions(follow.entries[3].oid, follow.entries[0].oid),
            path: follow.entries[0].path_at_commit.clone(),
            old_path: Some(follow.entries[3].path_at_commit.clone()),
            context: DiffContext::Three,
        },
        None,
    )
    .await;
    assert!(!cmp_versions.hunks.is_empty());

    fs::write(
        dir.join("sub/b 中文.txt"),
        format!("{base_lines}line 8\nline 9\nlocal edit\n"),
    )
    .unwrap();
    let cmp_local = fetch_file_diff(
        &handle,
        &mut rx,
        RequestSlotId::FileHistoryDiff,
        FileDiffKey {
            details: DetailsKey::compare_with_local(follow.entries[3].oid),
            path: "sub/b 中文.txt".into(),
            old_path: Some(follow.entries[3].path_at_commit.clone()),
            context: DiffContext::Three,
        },
        None,
    )
    .await;
    assert!(!cmp_local.hunks.is_empty());

    let no_follow = fetch_file_history(
        &handle,
        &mut rx,
        FileHistoryKey {
            path: "sub/b 中文.txt".into(),
            start_oid: None,
            follow_renames: false,
            line_range: None,
        },
    )
    .await;
    assert_eq!(no_follow.entries.len(), 2);

    let missing = fetch_file_history(
        &handle,
        &mut rx,
        FileHistoryKey {
            path: "does/not/exist.txt".into(),
            start_oid: Some(Oid::from_bytes(follow.entries[0].oid.as_bytes().to_owned())),
            follow_renames: true,
            line_range: None,
        },
    )
    .await;
    assert!(missing.entries.is_empty());

    let e1 = handle.request_file_history(FileHistoryKey {
        path: "sub/b 中文.txt".into(),
        start_oid: None,
        follow_renames: true,
        line_range: None,
    });
    let e2 = handle.request_file_history(FileHistoryKey {
        path: "sub/b 中文.txt".into(),
        start_oid: None,
        follow_renames: false,
        line_range: None,
    });
    assert!(e2 > e1);

    let latest = recv_matching(&mut rx, |ev| match ev {
        EngineEvent::ConsoleStarted(_) => {
            panic!("read-only file history must not emit ConsoleStarted");
        }
        EngineEvent::FileHistoryLoaded { bundle, epoch } if epoch == e2 => Some(bundle),
        _ => None,
    })
    .await;
    assert_eq!(latest.entries.len(), 2);
}

#[tokio::test]
async fn large_file_exceeding_limit_short_circuits_file_and_stage_diff() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();
    git(dir, &["init", "-b", "main"]);

    let huge = vec![b'a'; MAX_DIFF_FILE_BYTES + 1024];
    fs::write(dir.join("huge.txt"), &huge).unwrap();
    git(dir, &["add", "huge.txt"]);
    git(dir, &["commit", "-m", "add huge file"]);
    let head = rev(dir, "HEAD");

    let loc = RepoLocation::discover(dir).unwrap();
    let (handle, mut rx) = EngineSupervisor::start(loc, false).unwrap();

    let file_diff = fetch_file_diff(
        &handle,
        &mut rx,
        RequestSlotId::LogFileDiff,
        FileDiffKey {
            details: DetailsKey::single(head),
            path: "huge.txt".into(),
            old_path: None,
            context: DiffContext::Three,
        },
        None,
    )
    .await;
    assert!(file_diff.is_too_large);
    assert!(file_diff.hunks.is_empty());

    let mut huge_wt = huge;
    huge_wt.extend_from_slice(b"\nmore\n");
    fs::write(dir.join("huge.txt"), &huge_wt).unwrap();

    let stage_diff = fetch_stage_diff(
        &handle,
        &mut rx,
        StageDiffKey {
            path: "huge.txt".into(),
            old_path: None,
            side: StageDiffSide::Unstaged,
            context: DiffContext::Three,
        },
    )
    .await;
    assert!(stage_diff.is_too_large);
    assert!(stage_diff.hunks.is_empty());
    assert!(stage_diff.vcs_bytes.is_empty());
    assert!(stage_diff.local_bytes.is_empty());
}

#[tokio::test]
async fn fast_file_diff_paths_match_rename_add_delete_compare_local_and_multi_commit_oids() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();
    git(dir, &["init", "-b", "main"]);

    fs::write(dir.join("keep.txt"), "one\ntwo\n").unwrap();
    fs::write(dir.join("to_delete.txt"), "bye1\nbye2\n").unwrap();
    fs::write(dir.join("old_name.txt"), "rename line 1\nrename line 2\n").unwrap();
    git(dir, &["add", "."]);
    git(dir, &["commit", "-m", "c1"]);
    let c1 = rev(dir, "HEAD");

    fs::write(dir.join("keep.txt"), "one\ntwo_v2\n").unwrap();
    fs::write(dir.join("added.txt"), "new_a\nnew_b\n").unwrap();
    fs::remove_file(dir.join("to_delete.txt")).unwrap();
    git(dir, &["mv", "old_name.txt", "new_name.txt"]);
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-m", "c2"]);
    let c2 = rev(dir, "HEAD");

    fs::write(dir.join("keep.txt"), "one\ntwo_v3\nthree\n").unwrap();
    git(dir, &["add", "keep.txt"]);
    git(dir, &["commit", "-m", "c3"]);
    let c3 = rev(dir, "HEAD");

    let loc = RepoLocation::discover(dir).unwrap();
    let (handle, mut rx) = EngineSupervisor::start(loc, false).unwrap();

    let bundle_c2 = fetch_commit_details(
        &handle,
        &mut rx,
        RequestSlotId::LogCommitDetails,
        DetailsKey::single(c2),
    )
    .await;
    let ren = bundle_c2
        .changes
        .iter()
        .find(|c| c.path == "new_name.txt")
        .unwrap();
    assert_eq!(ren.kind, FileStatusKind::Renamed);
    assert_eq!(ren.old_path.as_deref(), Some("old_name.txt"));
    assert!(ren.old_oid.is_some() && ren.new_oid.is_some());

    let diff_add = fetch_file_diff(
        &handle,
        &mut rx,
        RequestSlotId::LogFileDiff,
        FileDiffKey {
            details: DetailsKey::single(c2),
            path: "added.txt".into(),
            old_path: None,
            context: DiffContext::Three,
        },
        None,
    )
    .await;
    assert!(
        diff_add.hunks[0]
            .lines
            .iter()
            .all(|l| l.kind == DiffLineKind::Added)
    );

    let diff_del = fetch_file_diff(
        &handle,
        &mut rx,
        RequestSlotId::LogFileDiff,
        FileDiffKey {
            details: DetailsKey::single(c2),
            path: "to_delete.txt".into(),
            old_path: None,
            context: DiffContext::Three,
        },
        None,
    )
    .await;
    assert!(
        diff_del.hunks[0]
            .lines
            .iter()
            .all(|l| l.kind == DiffLineKind::Deleted)
    );

    let diff_ren = fetch_file_diff(
        &handle,
        &mut rx,
        RequestSlotId::LogFileDiff,
        FileDiffKey {
            details: DetailsKey::single(c2),
            path: "new_name.txt".into(),
            old_path: Some("old_name.txt".into()),
            context: DiffContext::Three,
        },
        None,
    )
    .await;
    assert!(diff_ren.hunks.is_empty(), "identical rename has empty hunks");

    let multi_key = DetailsKey::commits(vec![c3, c2], 0);
    let multi_bundle = fetch_commit_details(
        &handle,
        &mut rx,
        RequestSlotId::LogCommitDetails,
        multi_key.clone(),
    )
    .await;
    let keep_change = multi_bundle
        .changes
        .iter()
        .find(|c| c.path == "keep.txt")
        .unwrap();
    let fkey = FileDiffKey {
        details: multi_key,
        path: "keep.txt".into(),
        old_path: None,
        context: DiffContext::Three,
    };
    let via_oids = fetch_file_diff(
        &handle,
        &mut rx,
        RequestSlotId::LogFileDiff,
        fkey.clone(),
        Some((keep_change.old_oid, keep_change.new_oid)),
    )
    .await;
    let via_fallback = fetch_file_diff(
        &handle,
        &mut rx,
        RequestSlotId::LogFileDiff,
        fkey,
        None,
    )
    .await;
    assert_eq!(via_oids, via_fallback);

    fs::write(dir.join("keep.txt"), "one\ntwo_local\n").unwrap();
    let cmp_local = fetch_file_diff(
        &handle,
        &mut rx,
        RequestSlotId::LogFileDiff,
        FileDiffKey {
            details: DetailsKey::compare_with_local(c1),
            path: "keep.txt".into(),
            old_path: None,
            context: DiffContext::Three,
        },
        None,
    )
    .await;
    assert!(
        cmp_local.hunks[0]
            .lines
            .iter()
            .any(|l| l.kind == DiffLineKind::Added && l.text == "two_local")
    );

    let cmp_ver = fetch_file_diff(
        &handle,
        &mut rx,
        RequestSlotId::LogFileDiff,
        FileDiffKey {
            details: DetailsKey::compare_versions(c1, c3),
            path: "keep.txt".into(),
            old_path: None,
            context: DiffContext::Three,
        },
        None,
    )
    .await;
    assert!(
        cmp_ver.hunks[0]
            .lines
            .iter()
            .any(|l| l.kind == DiffLineKind::Added && l.text == "three")
    );
}

#[tokio::test]
async fn stage_diff_index_entry_conflict_vs_normal_detection() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();
    git(dir, &["init", "-b", "main"]);
    fs::write(dir.join("conflict.txt"), "base\n").unwrap();
    fs::write(dir.join("clean.txt"), "ok\n").unwrap();
    git(dir, &["add", "."]);
    git(dir, &["commit", "-m", "base"]);

    git(dir, &["checkout", "-b", "branch_a"]);
    fs::write(dir.join("conflict.txt"), "ours\n").unwrap();
    git(dir, &["add", "conflict.txt"]);
    git(dir, &["commit", "-m", "ours"]);

    git(dir, &["checkout", "main"]);
    fs::write(dir.join("conflict.txt"), "theirs\n").unwrap();
    fs::write(dir.join("clean.txt"), "ok_edited\n").unwrap();
    git(dir, &["add", "."]);
    git(dir, &["commit", "-m", "theirs"]);

    let merge_out = git_output(dir, &["merge", "branch_a"]);
    assert!(
        !merge_out.status.success()
            && String::from_utf8_lossy(&merge_out.stdout).contains("CONFLICT"),
        "expected merge conflict, got status={:?}, stdout={}, stderr={}",
        merge_out.status,
        String::from_utf8_lossy(&merge_out.stdout),
        String::from_utf8_lossy(&merge_out.stderr),
    );

    let loc = RepoLocation::discover(dir).unwrap();
    let (handle, mut rx) = EngineSupervisor::start(loc, false).unwrap();

    let conflicted = fetch_stage_diff(
        &handle,
        &mut rx,
        StageDiffKey {
            path: "conflict.txt".into(),
            old_path: None,
            side: StageDiffSide::Unstaged,
            context: DiffContext::Three,
        },
    )
    .await;
    assert!(conflicted.is_conflicted);
    assert!(!conflicted.hunks.is_empty());

    let normal = fetch_stage_diff(
        &handle,
        &mut rx,
        StageDiffKey {
            path: "clean.txt".into(),
            old_path: None,
            side: StageDiffSide::Unstaged,
            context: DiffContext::Three,
        },
    )
    .await;
    assert!(!normal.is_conflicted);
}

#[tokio::test]
async fn real_tab_indented_files_preserve_raw_tabs_in_diffs_and_partial_staging() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();
    git(dir, &["init", "-b", "main"]);
    git(dir, &["config", "core.autocrlf", "false"]);

    let v1 = "fn main() {\n\tlet x = 1;\n\tlet y = 2;\n}\n";
    fs::write(dir.join("tabs.rs"), v1).unwrap();
    git(dir, &["add", "tabs.rs"]);
    git(dir, &["commit", "-m", "v1"]);

    let v2 = "fn main() {\n\tlet x = 10;\n\tlet y = 2;\n}\n";
    fs::write(dir.join("tabs.rs"), v2).unwrap();
    git(dir, &["add", "tabs.rs"]);
    git(dir, &["commit", "-m", "v2"]);
    let c2 = rev(dir, "HEAD");

    let v3 = "fn main() {\n\tlet x = 10;\n\tlet y = 20;\n\tlet z = 30;\n}\n";
    fs::write(dir.join("tabs.rs"), v3).unwrap();

    let loc = RepoLocation::discover(dir).unwrap();
    let (handle, mut rx) = EngineSupervisor::start(loc, false).unwrap();

    // 1. Two-doc FileDiff preserves raw '\t' and accurate word_ranges byte offsets.
    let file_diff = fetch_file_diff(
        &handle,
        &mut rx,
        RequestSlotId::LogFileDiff,
        FileDiffKey {
            details: DetailsKey::single(c2),
            path: "tabs.rs".into(),
            old_path: None,
            context: DiffContext::Three,
        },
        None,
    )
    .await;
    let added = file_diff.hunks[0]
        .lines
        .iter()
        .find(|l| l.kind == DiffLineKind::Added)
        .expect("added line");
    assert!(
        added.text.starts_with('\t'),
        "FileDiff must preserve raw tab: {:?}",
        added.text
    );
    assert_eq!(added.text, "\tlet x = 10;");
    assert_eq!(
        &added.text[added.word_ranges[0].0..added.word_ranges[0].1],
        "10"
    );

    // 2. Three-doc StageDiff preserves raw '\t' and partial staging keeps verbatim '\t' bytes.
    let stage_diff = fetch_stage_diff(
        &handle,
        &mut rx,
        StageDiffKey {
            path: "tabs.rs".into(),
            old_path: None,
            side: StageDiffSide::Unstaged,
            context: DiffContext::Three,
        },
    )
    .await;
    let stage_added = stage_diff.hunks[0]
        .hunk
        .lines
        .iter()
        .find(|l| l.kind == DiffLineKind::Added)
        .expect("stage added line");
    assert!(
        stage_added.text.starts_with('\t'),
        "StageDiff must preserve raw tab: {:?}",
        stage_added.text
    );

    let b = stage_diff.tracker.unstaged_blocks[0];
    let new_staged = stage_diff.tracker.stage_lines(&LineSelection::from_iters(
        false,
        [b.start1],
        [b.start2],
    ));
    assert!(
        new_staged.windows(5).any(|w| w == b"\tlet "),
        "partial stage buffer must retain raw tab bytes"
    );
    let _ = handle.dispatch(TaskRequest::ApplyStageBuffer {
        path: "tabs.rs".into(),
        new_staged_bytes: new_staged.clone(),
        file_mode: 0o100644,
        remove_from_index: false,
    });
    recv_matching(&mut rx, |ev| match ev {
        EngineEvent::ExternalFsChanged(_) => Some(()),
        _ => None,
    })
    .await;
    assert_eq!(fs::read_to_string(dir.join("tabs.rs")).unwrap(), v3);
    let staged_blob = Command::new("git")
        .args(["show", ":tabs.rs"])
        .current_dir(dir)
        .output()
        .unwrap();
    assert_eq!(staged_blob.stdout, new_staged);
}

#[tokio::test]
async fn blame_tracks_commits_moves_uncommitted_lines_and_guards() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();
    git(dir, &["init", "-b", "main"]);
    git(dir, &["config", "user.name", "Tester"]);
    git(dir, &["config", "user.email", "tester@example.com"]);
    git(dir, &["config", "core.autocrlf", "false"]);

    // c1: 3 lines
    fs::write(dir.join("code.rs"), "line1_v1\nline2_v1\nline3_v1\n").unwrap();
    git(dir, &["add", "code.rs"]);
    git(dir, &["commit", "-m", "c1: initial"]);
    let c1 = rev(dir, "HEAD");

    // c2: modify line 2
    fs::write(dir.join("code.rs"), "line1_v1\nline2_v2\nline3_v1\n").unwrap();
    git(dir, &["add", "code.rs"]);
    git(dir, &["commit", "-m", "c2: edit line 2"]);
    let c2 = rev(dir, "HEAD");

    // c3: modify line 3
    fs::write(dir.join("code.rs"), "line1_v1\nline2_v2\nline3_v3\n").unwrap();
    git(dir, &["add", "code.rs"]);
    git(dir, &["commit", "-m", "c3: edit line 3"]);
    let c3 = rev(dir, "HEAD");

    let loc = RepoLocation::discover(dir).unwrap();
    let (handle, mut rx) = EngineSupervisor::start(loc, false).unwrap();

    // Check commit c3 blame
    let b3 = fetch_blame(
        &handle,
        &mut rx,
        BlameKey {
            path: "code.rs".into(),
            rev: Some(c3),
            options: BlameOptions::default(),
        },
    )
    .await
    .expect("blame c3");
    assert!(!b3.uncommitted_oid_present);
    assert_eq!(b3.lines.len(), 3);
    assert_eq!(b3.lines[0].oid, c1);
    assert_eq!(b3.lines[1].oid, c2);
    assert_eq!(b3.lines[2].oid, c3);
    assert_eq!(b3.content_lines(), vec!["line1_v1", "line2_v2", "line3_v3"]);
    assert!(b3.commits[&c1].is_boundary);
    assert_eq!(b3.commits[&c3].previous, Some((c2, "code.rs".into())));

    // c4: git mv + move lines around to verify -M -C still traces back to c1/c2/c3
    git(dir, &["mv", "code.rs", "moved.rs"]);
    git(dir, &["commit", "-m", "c4: rename code.rs"]);
    let c4 = rev(dir, "HEAD");
    fs::write(dir.join("moved.rs"), "line1_v1\nline2_v2\nline3_v3\nuncommitted\n").unwrap();

    let moved_blame = fetch_blame(
        &handle,
        &mut rx,
        BlameKey {
            path: "moved.rs".into(),
            rev: None,
            options: BlameOptions {
                ignore_whitespace: false,
                detect_moves: true,
            },
        },
    )
    .await
    .expect("blame moved worktree");
    assert!(moved_blame.uncommitted_oid_present);
    assert_eq!(moved_blame.lines.len(), 4);
    assert_eq!(moved_blame.lines[0].oid, c1);
    assert_eq!(moved_blame.lines[0].orig_path, "code.rs");
    assert_eq!(moved_blame.lines[1].oid, c2);
    assert_eq!(moved_blame.lines[2].oid, c3);
    assert_eq!(moved_blame.lines[3].oid, Oid::ZERO);
    assert_eq!(
        moved_blame.commits[&Oid::ZERO].author.name,
        "Not Committed Yet"
    );

    // Also confirm `c4` exists and blaming at `c4` works
    let c4_blame = fetch_blame(
        &handle,
        &mut rx,
        BlameKey {
            path: "moved.rs".into(),
            rev: Some(c4),
            options: BlameOptions::default(),
        },
    )
    .await
    .expect("blame c4");
    assert_eq!(c4_blame.lines.len(), 3);

    // Binary and > 4 MiB files must return structured BlameFailureKind.
    fs::write(dir.join("bin.dat"), b"\x00\x01\x02hello").unwrap();
    git(dir, &["add", "bin.dat"]);
    git(dir, &["commit", "-m", "add binary"]);
    let (kind, _) = fetch_blame(
        &handle,
        &mut rx,
        BlameKey {
            path: "bin.dat".into(),
            rev: None,
            options: BlameOptions::default(),
        },
    )
    .await
    .expect_err("binary must fail");
    assert_eq!(kind, BlameFailureKind::Binary);

    let huge = vec![b'x'; MAX_DIFF_FILE_BYTES + 1024];
    fs::write(dir.join("huge.txt"), &huge).unwrap();
    let (kind, _) = fetch_blame(
        &handle,
        &mut rx,
        BlameKey {
            path: "huge.txt".into(),
            rev: None,
            options: BlameOptions::default(),
        },
    )
    .await
    .expect_err("huge worktree file must fail");
    assert_eq!(
        kind,
        BlameFailureKind::TooLarge {
            limit: MAX_DIFF_FILE_BYTES
        }
    );
}

#[tokio::test]
async fn selection_history_traces_line_range_and_crosses_renames() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();
    git(dir, &["init", "-b", "main"]);
    git(dir, &["config", "user.name", "Tester"]);
    git(dir, &["config", "user.email", "tester@example.com"]);
    git(dir, &["config", "core.autocrlf", "false"]);

    // Commit 1: file with top_fn (lines 1..=3) and bottom_fn (lines 5..=7)
    fs::write(
        dir.join("funcs.rs"),
        "fn top_fn() {\n    let x = 1;\n}\n\nfn bottom_fn() {\n    let y = 1;\n}\n",
    )
    .unwrap();
    git(dir, &["add", "funcs.rs"]);
    git(dir, &["commit", "-m", "c1: add top_fn and bottom_fn"]);

    // Commit 2: edit bottom_fn only
    fs::write(
        dir.join("funcs.rs"),
        "fn top_fn() {\n    let x = 1;\n}\n\nfn bottom_fn() {\n    let y = 2;\n}\n",
    )
    .unwrap();
    git(dir, &["add", "funcs.rs"]);
    git(dir, &["commit", "-m", "c2: edit bottom_fn only"]);

    // Commit 3: edit top_fn
    fs::write(
        dir.join("funcs.rs"),
        "fn top_fn() {\n    let x = 2;\n}\n\nfn bottom_fn() {\n    let y = 2;\n}\n",
    )
    .unwrap();
    git(dir, &["add", "funcs.rs"]);
    git(dir, &["commit", "-m", "c3: edit top_fn"]);

    // Commit 4: edit bottom_fn only again
    fs::write(
        dir.join("funcs.rs"),
        "fn top_fn() {\n    let x = 2;\n}\n\nfn bottom_fn() {\n    let y = 3;\n}\n",
    )
    .unwrap();
    git(dir, &["add", "funcs.rs"]);
    git(dir, &["commit", "-m", "c4: edit bottom_fn again"]);

    // Commit 5: git mv + edit top_fn
    git(dir, &["mv", "funcs.rs", "renamed_funcs.rs"]);
    fs::write(
        dir.join("renamed_funcs.rs"),
        "fn top_fn() {\n    let x = 3;\n}\n\nfn bottom_fn() {\n    let y = 3;\n}\n",
    )
    .unwrap();
    git(dir, &["add", "renamed_funcs.rs"]);
    git(dir, &["commit", "-m", "c5: rename and edit top_fn"]);

    let loc = RepoLocation::discover(dir).unwrap();
    let (handle, mut rx) = EngineSupervisor::start(loc, false).unwrap();

    let sel = fetch_file_history(
        &handle,
        &mut rx,
        FileHistoryKey {
            path: "renamed_funcs.rs".into(),
            start_oid: None,
            follow_renames: false,
            line_range: Some((1, 3)),
        },
    )
    .await;

    assert!(!sel.truncated);
    // Only c5, c3, c1 touched top_fn (lines 1..=3); c4 and c2 touched bottom_fn only!
    let summaries: Vec<&str> = sel.entries.iter().map(|e| e.summary.as_str()).collect();
    assert_eq!(
        summaries,
        vec![
            "c5: rename and edit top_fn",
            "c3: edit top_fn",
            "c1: add top_fn and bottom_fn"
        ]
    );
    assert!(sel.entries.iter().all(|e| e.range_patch.is_some()));
    assert_eq!(sel.entries[0].old_path.as_deref(), Some("funcs.rs"));
    assert_eq!(sel.entries[0].path_at_commit, "renamed_funcs.rs");
    assert_eq!(sel.entries[2].path_at_commit, "funcs.rs");
}

async fn run_git_op(
    handle: &jbgit_engine::EngineHandle,
    rx: &mut UnboundedReceiver<EngineEvent>,
    op: jbgit_core::GitOp,
) -> jbgit_core::GitOpOutcome {
    let _ = handle.dispatch(jbgit_engine::TaskRequest::RunGitOp { op });
    recv_matching(rx, |ev| match ev {
        EngineEvent::GitOpFinished(outcome) => Some(outcome),
        _ => None,
    })
    .await
}

async fn check_remote_tag(
    handle: &jbgit_engine::EngineHandle,
    rx: &mut UnboundedReceiver<EngineEvent>,
    remote: &str,
    tag: &str,
) -> Option<bool> {
    let epoch = handle
        .dispatch(jbgit_engine::TaskRequest::CheckRemoteTag {
            remote: remote.to_string(),
            tag: tag.to_string(),
        })
        .expect("CheckRemoteTag epoch");
    let expected_remote = remote.to_string();
    let expected_tag = tag.to_string();
    recv_matching(rx, |ev| match ev {
        EngineEvent::ConsoleStarted(_) => {
            panic!("read-only CheckRemoteTag must not emit ConsoleStarted");
        }
        EngineEvent::RemoteTagChecked {
            remote,
            tag,
            exists,
            epoch: e,
        } if e == epoch && remote == expected_remote && tag == expected_tag => Some(exists),
        _ => None,
    })
    .await
}

async fn load_outgoing(
    handle: &jbgit_engine::EngineHandle,
    rx: &mut UnboundedReceiver<EngineEvent>,
    key: jbgit_core::OutgoingKey,
) -> Arc<jbgit_core::OutgoingBundle> {
    let epoch = handle
        .dispatch(jbgit_engine::TaskRequest::LoadOutgoing { key: key.clone() })
        .expect("LoadOutgoing epoch");
    recv_matching(rx, |ev| match ev {
        EngineEvent::ConsoleStarted(_) => {
            panic!("read-only LoadOutgoing must not emit ConsoleStarted");
        }
        EngineEvent::OutgoingLoaded { bundle, epoch: e } if e == epoch && bundle.key == key => {
            Some(bundle)
        }
        _ => None,
    })
    .await
}

#[tokio::test]
async fn snapshot_lists_remotes_with_fetch_and_push_urls() {
    let dir_guard = TempDir::new().unwrap();
    let dir = dir_guard.path();
    git(dir, &["init", "-b", "main"]);
    fs::write(dir.join("a.txt"), "a\n").unwrap();
    git(dir, &["add", "."]);
    git(dir, &["commit", "-m", "init"]);

    let loc = RepoLocation::discover(dir).unwrap();
    let (handle, mut rx) = EngineSupervisor::start(loc, false).unwrap();
    let snap = fetch_snapshot(&handle, &mut rx).await;
    assert!(snap.remotes.is_empty());
    assert_eq!(snap.default_remote(), None);

    git(dir, &["remote", "add", "zeta", "https://u:secret@example.com/z.git"]);
    git(dir, &["remote", "add", "origin", "https://example.com/o.git"]);
    git(dir, &["config", "remote.pushonly.pushurl", "ssh://git@example.com/p.git"]);
    let snap = fetch_snapshot(&handle, &mut rx).await;
    let names: Vec<&str> = snap.remote_names().collect();
    assert_eq!(names, vec!["origin", "pushonly", "zeta"]);
    assert_eq!(snap.default_remote().as_deref(), Some("origin"));

    let zeta = snap.remote("zeta").unwrap();
    assert_eq!(zeta.fetch_url.as_deref(), Some("https://u:secret@example.com/z.git"));
    assert_eq!(zeta.push_url, None);
    assert_eq!(zeta.display_fetch_url(), "https://u:***@example.com/z.git");
    assert!(!format!("{snap:?}").contains("secret"));

    let push_only = snap.remote("pushonly").unwrap();
    assert_eq!(push_only.fetch_url, None);
    assert_eq!(push_only.push_url.as_deref(), Some("ssh://git@example.com/p.git"));
    assert_eq!(push_only.effective_push_url(), Some("ssh://git@example.com/p.git"));
}

#[tokio::test]
async fn remote_lifecycle_add_fetch_rename_set_url_and_remove() {
    use jbgit_core::{GitFailure, GitOp};

    let upstream_guard = TempDir::new().unwrap();
    let upstream = upstream_guard.path();
    git(upstream, &["init", "-b", "main"]);
    fs::write(upstream.join("u.txt"), "u\n").unwrap();
    git(upstream, &["add", "."]);
    git(upstream, &["commit", "-m", "upstream"]);
    git(upstream, &["branch", "feature"]);

    let local_guard = TempDir::new().unwrap();
    let dir = local_guard.path();
    git(dir, &["init", "-b", "main"]);
    fs::write(dir.join("l.txt"), "l\n").unwrap();
    git(dir, &["add", "."]);
    git(dir, &["commit", "-m", "local"]);

    let loc = RepoLocation::discover(dir).unwrap();
    let (handle, mut rx) = EngineSupervisor::start(loc, false).unwrap();
    let url = upstream.to_str().unwrap().to_string();

    let added = run_git_op(
        &handle,
        &mut rx,
        GitOp::AddRemote {
            name: "fork".into(),
            url: url.clone(),
        },
    )
    .await;
    assert!(added.success, "{added:?}");

    let dup = run_git_op(
        &handle,
        &mut rx,
        GitOp::AddRemote {
            name: "fork".into(),
            url: url.clone(),
        },
    )
    .await;
    assert!(!dup.success);
    assert_eq!(dup.failure, Some(GitFailure::RemoteAlreadyExists));

    let fetched = run_git_op(&handle, &mut rx, GitOp::FetchRemote { name: "fork".into() }).await;
    assert!(fetched.success, "{fetched:?}");
    let snap = fetch_snapshot(&handle, &mut rx).await;
    let tracking: Vec<&str> = snap
        .tracking_refs_of("fork")
        .iter()
        .map(|r| r.name.as_str())
        .collect();
    assert_eq!(tracking, vec!["fork/feature", "fork/main"]);

    let renamed = run_git_op(
        &handle,
        &mut rx,
        GitOp::RenameRemote {
            old: "fork".into(),
            new: "mine".into(),
        },
    )
    .await;
    assert!(renamed.success, "{renamed:?}");
    let snap = fetch_snapshot(&handle, &mut rx).await;
    assert!(snap.remote("fork").is_none());
    assert_eq!(snap.remote("mine").unwrap().fetch_url.as_deref(), Some(url.as_str()));
    let tracking: Vec<&str> = snap
        .tracking_refs_of("mine")
        .iter()
        .map(|r| r.name.as_str())
        .collect();
    assert_eq!(tracking, vec!["mine/feature", "mine/main"]);
    assert!(snap.tracking_refs_of("fork").is_empty());

    let push_url = "https://u:pw@example.com/mine.git";
    let set = run_git_op(
        &handle,
        &mut rx,
        GitOp::SetRemoteUrl {
            name: "mine".into(),
            url: push_url.into(),
            push: true,
        },
    )
    .await;
    assert!(set.success, "{set:?}");
    let snap = fetch_snapshot(&handle, &mut rx).await;
    let mine = snap.remote("mine").unwrap();
    assert_eq!(mine.push_url.as_deref(), Some(push_url));
    assert_eq!(mine.fetch_url.as_deref(), Some(url.as_str()));

    let removed = run_git_op(&handle, &mut rx, GitOp::RemoveRemote { name: "mine".into() }).await;
    assert!(removed.success, "{removed:?}");
    let snap = fetch_snapshot(&handle, &mut rx).await;
    assert!(snap.remotes.is_empty());
    assert!(!snap.refs.iter().any(|r| r.kind == RefKind::RemoteBranch));
}

#[tokio::test]
async fn tag_management_lifecycle_push_remote_check_overwrite_delete_and_restore() {
    use jbgit_core::{GitFailure, GitOp};

    let bare_dir = TempDir::new().unwrap();
    git(bare_dir.path(), &["init", "--bare", "-b", "main"]);

    let local_dir = TempDir::new().unwrap();
    let dir = local_dir.path();
    git(dir, &["init", "-b", "main"]);
    git(dir, &["config", "user.name", "Tester"]);
    git(dir, &["config", "user.email", "tester@example.com"]);
    git(
        dir,
        &["remote", "add", "origin", bare_dir.path().to_str().unwrap()],
    );

    fs::write(dir.join("file.txt"), "c1\n").unwrap();
    git(dir, &["add", "."]);
    git(dir, &["commit", "-m", "c1"]);
    let c1 = rev(dir, "HEAD");

    fs::write(dir.join("file.txt"), "c2\n").unwrap();
    git(dir, &["add", "."]);
    git(dir, &["commit", "-m", "c2"]);
    let c2 = rev(dir, "HEAD");
    git(dir, &["push", "-u", "origin", "main"]);

    let loc = RepoLocation::discover(dir).unwrap();
    let (handle, mut rx) = EngineSupervisor::start(loc, false).unwrap();

    // Create annotated tag v1.0 on c1.
    let out = run_git_op(
        &handle,
        &mut rx,
        GitOp::CreateTag {
            name: "v1.0".into(),
            target: c1,
            message: Some("release 1.0".into()),
            force: false,
        },
    )
    .await;
    assert!(out.success, "{out:?}");
    assert_eq!(rev(dir, "v1.0^{}"), c1);

    // Creating duplicate tag without force fails with TagAlreadyExists.
    let dup = run_git_op(
        &handle,
        &mut rx,
        GitOp::CreateTag {
            name: "v1.0".into(),
            target: c2,
            message: None,
            force: false,
        },
    )
    .await;
    assert!(!dup.success);
    assert_eq!(dup.failure, Some(GitFailure::TagAlreadyExists));

    // Overwriting with force = true moves local tag to c2.
    let overwrite = run_git_op(
        &handle,
        &mut rx,
        GitOp::CreateTag {
            name: "v1.0".into(),
            target: c2,
            message: None,
            force: true,
        },
    )
    .await;
    assert!(overwrite.success, "{overwrite:?}");
    assert_eq!(rev(dir, "v1.0^{}"), c2);

    // Remote does not have v1.0 yet.
    assert_eq!(
        check_remote_tag(&handle, &mut rx, "origin", "v1.0").await,
        Some(false)
    );

    // Push tag to origin, then remote check reports Some(true).
    let push_tag = run_git_op(
        &handle,
        &mut rx,
        GitOp::PushTag {
            remote: "origin".into(),
            name: "v1.0".into(),
            force: false,
        },
    )
    .await;
    assert!(push_tag.success, "{push_tag:?}");
    assert_eq!(
        check_remote_tag(&handle, &mut rx, "origin", "v1.0").await,
        Some(true)
    );

    // Move local v1.0 back to c1; non-forced PushTag fails with TagAlreadyExists.
    let move_back = run_git_op(
        &handle,
        &mut rx,
        GitOp::CreateTag {
            name: "v1.0".into(),
            target: c1,
            message: None,
            force: true,
        },
    )
    .await;
    assert!(move_back.success, "{move_back:?}");

    let push_conflict = run_git_op(
        &handle,
        &mut rx,
        GitOp::PushTag {
            remote: "origin".into(),
            name: "v1.0".into(),
            force: false,
        },
    )
    .await;
    assert!(!push_conflict.success);
    assert_eq!(push_conflict.failure, Some(GitFailure::TagAlreadyExists));

    // Force push tag succeeds and updates remote tag target.
    let force_push_tag = run_git_op(
        &handle,
        &mut rx,
        GitOp::PushTag {
            remote: "origin".into(),
            name: "v1.0".into(),
            force: true,
        },
    )
    .await;
    assert!(force_push_tag.success, "{force_push_tag:?}");
    assert_eq!(rev(bare_dir.path(), "refs/tags/v1.0^{}"), c1);

    // Delete remote tag, verify CheckRemoteTag now returns Some(false).
    let del_remote = run_git_op(
        &handle,
        &mut rx,
        GitOp::DeleteRemoteTag {
            remote: "origin".into(),
            name: "v1.0".into(),
        },
    )
    .await;
    assert!(del_remote.success, "{del_remote:?}");
    assert_eq!(
        check_remote_tag(&handle, &mut rx, "origin", "v1.0").await,
        Some(false)
    );

    // Delete local tag, verify gone, then RestoreTag restores it at c1.
    let del_local = run_git_op(
        &handle,
        &mut rx,
        GitOp::DeleteTag {
            name: "v1.0".into(),
            tip: c1,
        },
    )
    .await;
    assert!(del_local.success, "{del_local:?}");
    let listed = git_output(dir, &["tag", "-l", "v1.0"]);
    assert!(String::from_utf8_lossy(&listed.stdout).trim().is_empty());

    let restored = run_git_op(
        &handle,
        &mut rx,
        GitOp::RestoreTag {
            name: "v1.0".into(),
            oid: c1,
        },
    )
    .await;
    assert!(restored.success, "{restored:?}");
    assert_eq!(rev(dir, "v1.0^{}"), c1);
}

#[tokio::test]
async fn outgoing_commits_and_push_rejected_sequence_retry() {
    use jbgit_core::{
        GitFailure, GitOp, OutgoingKey, PushForce, PushTagMode, parse_push_porcelain,
    };

    let bare_dir = TempDir::new().unwrap();
    git(bare_dir.path(), &["init", "--bare", "-b", "main"]);

    let work_a = TempDir::new().unwrap();
    let dir_a = work_a.path();
    git(dir_a, &["init", "-b", "main"]);
    git(dir_a, &["config", "user.name", "Tester A"]);
    git(dir_a, &["config", "user.email", "a@example.com"]);
    git(
        dir_a,
        &["remote", "add", "origin", bare_dir.path().to_str().unwrap()],
    );

    fs::write(dir_a.join("base.txt"), "base\n").unwrap();
    git(dir_a, &["add", "."]);
    git(dir_a, &["commit", "-m", "c1: base"]);
    git(dir_a, &["push", "-u", "origin", "main"]);

    // Second clone used later to advance remote main independently.
    let work_b = TempDir::new().unwrap();
    let dir_b = work_b.path();
    git(dir_b, &["clone", bare_dir.path().to_str().unwrap(), "."]);
    git(dir_b, &["config", "user.name", "Tester B"]);
    git(dir_b, &["config", "user.email", "b@example.com"]);

    let loc = RepoLocation::discover(dir_a).unwrap();
    let (handle, mut rx) = EngineSupervisor::start(loc, false).unwrap();

    // 1. Up-to-date branch: remote_exists = true, 0 commits, 0 files.
    let up_to_date = load_outgoing(
        &handle,
        &mut rx,
        OutgoingKey {
            branch: "main".into(),
            remote: "origin".into(),
            remote_branch: "main".into(),
            up_to: None,
        },
    )
    .await;
    assert!(up_to_date.remote_exists);
    assert!(up_to_date.commits.is_empty());
    assert_eq!(up_to_date.files_changed, 0);

    // 2. Branch whose remote ref does not exist yet.
    git(dir_a, &["checkout", "-b", "feat"]);
    fs::write(dir_a.join("a.txt"), "a1\n").unwrap();
    git(dir_a, &["add", "."]);
    git(dir_a, &["commit", "-m", "f1: add a.txt"]);
    let f1 = rev(dir_a, "HEAD");

    fs::write(dir_a.join("b.txt"), "b1\n").unwrap();
    git(dir_a, &["add", "."]);
    git(dir_a, &["commit", "-m", "f2: add b.txt"]);
    let f2 = rev(dir_a, "HEAD");

    let new_branch_bundle = load_outgoing(
        &handle,
        &mut rx,
        OutgoingKey {
            branch: "feat".into(),
            remote: "origin".into(),
            remote_branch: "feat".into(),
            up_to: None,
        },
    )
    .await;
    assert!(!new_branch_bundle.remote_exists);
    assert_eq!(new_branch_bundle.commits.len(), 2);
    assert_eq!(new_branch_bundle.commits[0].oid, f2);
    assert_eq!(new_branch_bundle.commits[1].oid, f1);
    assert_eq!(new_branch_bundle.files_changed, 2);

    // Truncated with up_to: Some(f1).
    let truncated_bundle = load_outgoing(
        &handle,
        &mut rx,
        OutgoingKey {
            branch: "feat".into(),
            remote: "origin".into(),
            remote_branch: "feat".into(),
            up_to: Some(f1),
        },
    )
    .await;
    assert_eq!(truncated_bundle.commits.len(), 1);
    assert_eq!(truncated_bundle.commits[0].oid, f1);
    assert_eq!(truncated_bundle.files_changed, 1);

    // 3. Ahead on existing remote branch `main`.
    git(dir_a, &["checkout", "main"]);
    fs::write(dir_a.join("m2.txt"), "m2\n").unwrap();
    git(dir_a, &["add", "."]);
    git(dir_a, &["commit", "-m", "m2"]);
    fs::write(dir_a.join("m3.txt"), "m3\n").unwrap();
    git(dir_a, &["add", "."]);
    git(dir_a, &["commit", "-m", "m3"]);

    let ahead_bundle = load_outgoing(
        &handle,
        &mut rx,
        OutgoingKey {
            branch: "main".into(),
            remote: "origin".into(),
            remote_branch: "main".into(),
            up_to: None,
        },
    )
    .await;
    assert!(ahead_bundle.remote_exists);
    assert_eq!(ahead_bundle.commits.len(), 2);
    assert_eq!(ahead_bundle.files_changed, 2);

    // 4. Advance bare remote from work_b -> local push gets rejected -> Sequence(Fetch + Rebase + Push) succeeds.
    fs::write(dir_b.join("remote_only.txt"), "from b\n").unwrap();
    git(dir_b, &["add", "."]);
    git(dir_b, &["commit", "-m", "remote commit"]);
    git(dir_b, &["push", "origin", "main"]);

    let push_main = GitOp::Push {
        branch: "main".into(),
        remote: "origin".into(),
        remote_branch: "main".into(),
        set_upstream: false,
        force: PushForce::None,
        tags: PushTagMode::None,
        up_to: None,
    };
    let rejected = run_git_op(&handle, &mut rx, push_main.clone()).await;
    assert!(!rejected.success);
    assert_eq!(rejected.failure, Some(GitFailure::PushRejected));

    let recovered = run_git_op(
        &handle,
        &mut rx,
        GitOp::Sequence(vec![
            GitOp::Fetch,
            GitOp::Rebase {
                onto: "origin/main".into(),
                branch: None,
            },
            push_main,
        ]),
    )
    .await;
    assert!(recovered.success, "{recovered:?}");
    let summary = parse_push_porcelain(&recovered.stdout);
    assert!(summary.commit_range.is_some(), "{:?}", recovered.stdout);
    assert_eq!(rev(bare_dir.path(), "refs/heads/main"), rev(dir_a, "HEAD"));
    assert!(dir_a.join("remote_only.txt").exists());

    // 5. Push with Follow tags pushes reachable annotated tag and porcelain summary reports it.
    fs::write(dir_a.join("m4.txt"), "m4\n").unwrap();
    git(dir_a, &["add", "."]);
    git(dir_a, &["commit", "-m", "m4"]);
    git(dir_a, &["tag", "-a", "v2.0", "-m", "v2 release"]);

    let follow = run_git_op(
        &handle,
        &mut rx,
        GitOp::Push {
            branch: "main".into(),
            remote: "origin".into(),
            remote_branch: "main".into(),
            set_upstream: false,
            force: PushForce::None,
            tags: PushTagMode::Follow,
            up_to: None,
        },
    )
    .await;
    assert!(follow.success, "{follow:?}");
    let follow_summary = parse_push_porcelain(&follow.stdout);
    assert_eq!(follow_summary.pushed_tags, 1, "{:?}", follow.stdout);
    assert!(follow_summary.commit_range.is_some());
    assert_eq!(rev(bare_dir.path(), "refs/tags/v2.0^{}"), rev(dir_a, "HEAD"));
}

#[tokio::test]
async fn worktree_lifecycle_add_list_lock_remove_and_prune() {
    use jbgit_core::{GitFailure, GitOp, WorktreeBranch};

    let root_guard = TempDir::new().unwrap();
    let root = root_guard.path();
    let dir = root.join("main");
    fs::create_dir_all(&dir).unwrap();
    git(&dir, &["init", "-b", "main"]);
    fs::write(dir.join("a.txt"), "a\n").unwrap();
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-m", "init"]);
    git(&dir, &["branch", "feature"]);

    let loc = RepoLocation::discover(&dir).unwrap();
    let (handle, mut rx) = EngineSupervisor::start(loc, false).unwrap();

    // No linked worktrees: only the synthesized main entry.
    let snap = fetch_snapshot(&handle, &mut rx).await;
    assert_eq!(snap.worktrees.len(), 1);
    let main_wt = &snap.worktrees[0];
    assert!(main_wt.is_main && !main_wt.is_bare && !main_wt.detached);
    assert_eq!(main_wt.branch.as_deref(), Some("main"));
    assert_eq!(snap.current_worktree().map(|w| w.is_main), Some(true));
    assert!(snap.worktree_of_branch("feature").is_none());

    let wt_feature = root.join("main-feature");
    let added = run_git_op(
        &handle,
        &mut rx,
        GitOp::AddWorktree {
            path: wt_feature.clone(),
            branch: WorktreeBranch::Existing("feature".into()),
        },
    )
    .await;
    assert!(added.success, "{added:?}");
    let wt_new = root.join("main-topic");
    let added = run_git_op(
        &handle,
        &mut rx,
        GitOp::AddWorktree {
            path: wt_new.clone(),
            branch: WorktreeBranch::New {
                name: "topic".into(),
                start: "main".into(),
            },
        },
    )
    .await;
    assert!(added.success, "{added:?}");
    let head = rev(&dir, "HEAD");
    let wt_detached = root.join("main-detached");
    let added = run_git_op(
        &handle,
        &mut rx,
        GitOp::AddWorktree {
            path: wt_detached.clone(),
            branch: WorktreeBranch::Detached(head),
        },
    )
    .await;
    assert!(added.success, "{added:?}");

    // The same branch cannot be checked out twice.
    let dup = run_git_op(
        &handle,
        &mut rx,
        GitOp::AddWorktree {
            path: root.join("main-feature-2"),
            branch: WorktreeBranch::Existing("feature".into()),
        },
    )
    .await;
    assert!(!dup.success);
    assert!(
        matches!(&dup.failure, Some(GitFailure::BranchCheckedOutInWorktree { branch, .. }) if branch == "feature"),
        "{dup:?}"
    );

    let snap = fetch_snapshot(&handle, &mut rx).await;
    assert_eq!(snap.worktrees.len(), 4, "{:?}", snap.worktrees);
    assert!(snap.worktrees[0].is_main);
    assert!(snap.current_worktree().is_some_and(|w| w.is_main));
    let feature = snap.worktree_of_branch("feature").expect("feature worktree");
    assert!(feature.is_at(&wt_feature.canonicalize().unwrap()));
    assert_eq!(snap.worktree_of_branch("topic").map(|w| w.name()), Some("main-topic".into()));
    let detached = snap.worktrees.iter().find(|w| w.name() == "main-detached").unwrap();
    assert!(detached.detached && detached.head == Some(head));
    assert!(snap.worktree_of_branch("main").is_none(), "current worktree is not 'other'");

    // A snapshot loaded from the linked worktree sees itself as current.
    let wt_loc = RepoLocation::discover(&wt_feature).unwrap();
    let (wt_handle, mut wt_rx) = EngineSupervisor::start(wt_loc, false).unwrap();
    let wt_snap = fetch_snapshot(&wt_handle, &mut wt_rx).await;
    assert_eq!(wt_snap.head_branch.as_deref(), Some("feature"));
    assert_eq!(wt_snap.current_worktree().and_then(|w| w.branch.clone()), Some("feature".into()));
    assert!(wt_snap.worktree_of_branch("main").is_some_and(|w| w.is_main));
    assert!(wt_handle.shutdown(Duration::from_secs(5)).await);

    // Lock → remove refused → unlock → remove.
    let locked = run_git_op(
        &handle,
        &mut rx,
        GitOp::LockWorktree {
            path: wt_feature.clone(),
            reason: Some("on usb disk".into()),
        },
    )
    .await;
    assert!(locked.success, "{locked:?}");
    let snap = fetch_snapshot(&handle, &mut rx).await;
    assert_eq!(
        snap.worktree_of_branch("feature").and_then(|w| w.locked.clone()).as_deref(),
        Some("on usb disk")
    );
    let refused = run_git_op(
        &handle,
        &mut rx,
        GitOp::RemoveWorktree {
            path: wt_feature.clone(),
            force: false,
        },
    )
    .await;
    assert!(!refused.success);
    assert_eq!(refused.failure, Some(GitFailure::WorktreeLocked), "{refused:?}");
    let unlocked = run_git_op(
        &handle,
        &mut rx,
        GitOp::UnlockWorktree {
            path: wt_feature.clone(),
        },
    )
    .await;
    assert!(unlocked.success, "{unlocked:?}");

    // Dirty → remove refused until forced.
    fs::write(wt_feature.join("dirty.txt"), "x\n").unwrap();
    let dirty = run_git_op(
        &handle,
        &mut rx,
        GitOp::RemoveWorktree {
            path: wt_feature.clone(),
            force: false,
        },
    )
    .await;
    assert_eq!(dirty.failure, Some(GitFailure::WorktreeDirty), "{dirty:?}");
    let forced = run_git_op(
        &handle,
        &mut rx,
        GitOp::RemoveWorktree {
            path: wt_feature.clone(),
            force: true,
        },
    )
    .await;
    assert!(forced.success, "{forced:?}");
    assert!(!wt_feature.exists());

    // A deleted directory becomes prunable, then prune drops it.
    fs::remove_dir_all(&wt_new).unwrap();
    let snap = fetch_snapshot(&handle, &mut rx).await;
    let topic = snap.worktrees.iter().find(|w| w.branch.as_deref() == Some("topic")).unwrap();
    assert!(topic.is_prunable(), "{topic:?}");
    let pruned = run_git_op(&handle, &mut rx, GitOp::PruneWorktrees).await;
    assert!(pruned.success, "{pruned:?}");
    let snap = fetch_snapshot(&handle, &mut rx).await;
    assert_eq!(snap.worktrees.len(), 2, "{:?}", snap.worktrees);
    assert!(snap.worktree_of_branch("topic").is_none());

    assert!(!handle.is_mutating());
    assert!(handle.shutdown(Duration::from_secs(5)).await);
}

#[tokio::test]
async fn snapshot_lists_submodule_paths() {
    let sub_guard = TempDir::new().unwrap();
    let sub = sub_guard.path();
    git(sub, &["init", "-b", "main"]);
    fs::write(sub.join("s.txt"), "s\n").unwrap();
    git(sub, &["add", "."]);
    git(sub, &["commit", "-m", "sub"]);

    let dir_guard = TempDir::new().unwrap();
    let dir = dir_guard.path();
    git(dir, &["init", "-b", "main"]);
    fs::write(dir.join("a.txt"), "a\n").unwrap();
    git(dir, &["add", "."]);
    git(dir, &["commit", "-m", "init"]);
    git(
        dir,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            sub.to_str().unwrap(),
            "libs/sub",
        ],
    );

    let loc = RepoLocation::discover(dir).unwrap();
    let (handle, mut rx) = EngineSupervisor::start(loc, false).unwrap();
    let snap = fetch_snapshot(&handle, &mut rx).await;
    assert_eq!(snap.submodules, vec!["libs/sub".to_string()]);
}

async fn check_patch(
    handle: &jbgit_engine::EngineHandle,
    rx: &mut UnboundedReceiver<EngineEvent>,
    key: jbgit_core::PatchCheckKey,
) -> Arc<jbgit_core::PatchCheck> {
    let epoch = handle
        .dispatch(jbgit_engine::TaskRequest::CheckPatch { key: key.clone() })
        .expect("CheckPatch epoch");
    recv_matching(rx, |ev| match ev {
        EngineEvent::ConsoleStarted(_) => panic!("read-only CheckPatch must not emit ConsoleStarted"),
        EngineEvent::PatchChecked { check, epoch: e } if e == epoch && check.key == key => {
            Some(check)
        }
        _ => None,
    })
    .await
}

fn worktree_text(dir: &Path, rel: &str) -> String {
    fs::read_to_string(dir.join(rel)).unwrap().replace("\r\n", "\n")
}

#[tokio::test]
async fn apply_patch_check_round_trip_index_mode_and_three_way_conflicts() {
    use jbgit_core::{ApplyMode, GitFailure, GitOp, PatchCheckKey, PatchInput};

    let dir_guard = TempDir::new().unwrap();
    let dir = dir_guard.path();
    git(dir, &["init", "-b", "main"]);
    git(dir, &["config", "core.autocrlf", "false"]);
    fs::write(dir.join("f.txt"), "a\nb\nc\n").unwrap();
    fs::create_dir_all(dir.join("sub")).unwrap();
    fs::write(dir.join("sub/g.txt"), "x\n").unwrap();
    git(dir, &["add", "."]);
    git(dir, &["commit", "-m", "init"]);

    // Export a patch, then revert the working tree.
    fs::write(dir.join("f.txt"), "a\nB\nc\n").unwrap();
    fs::write(dir.join("sub/g.txt"), "x\ny\n").unwrap();
    let patch_dir = TempDir::new().unwrap();
    let patch_path = patch_dir.path().join("fix.patch");
    let diff = git_output(dir, &["diff"]);
    fs::write(&patch_path, &diff.stdout).unwrap();
    git(dir, &["checkout", "--", "."]);

    let loc = RepoLocation::discover(dir).unwrap();
    let (handle, mut rx) = EngineSupervisor::start(loc, false).unwrap();
    let source = PatchInput::File(patch_path.clone());

    // Preflight lists the files with counts and changes nothing.
    let key = PatchCheckKey {
        source: source.clone(),
        mode: ApplyMode::WorkingTree,
    };
    let check = check_patch(&handle, &mut rx, key).await;
    assert!(check.applies(), "{check:?}");
    let rows: Vec<_> = check
        .files
        .iter()
        .map(|f| (f.path.as_str(), f.added, f.deleted))
        .collect();
    assert_eq!(
        rows,
        vec![("f.txt", Some(1), Some(1)), ("sub/g.txt", Some(1), Some(0))]
    );
    assert_eq!(worktree_text(dir, "f.txt"), "a\nb\nc\n");

    // Apply to working tree + index.
    let outcome = run_git_op(
        &handle,
        &mut rx,
        GitOp::ApplyPatch {
            source: source.clone(),
            mode: ApplyMode::Index,
        },
    )
    .await;
    assert!(outcome.success, "{outcome:?}");
    assert_eq!(worktree_text(dir, "f.txt"), "a\nB\nc\n");
    let staged = git_output(dir, &["diff", "--cached", "--name-only"]);
    assert_eq!(
        String::from_utf8_lossy(&staged.stdout).trim(),
        "f.txt\nsub/g.txt"
    );

    // Commit a conflicting change on top of the original content.
    git(dir, &["reset", "--hard", "HEAD"]);
    fs::write(dir.join("f.txt"), "a\nZ\nc\n").unwrap();
    git(dir, &["commit", "-am", "conflicting"]);

    // A failing check reports the first error and does not modify anything.
    let failed = check_patch(
        &handle,
        &mut rx,
        PatchCheckKey {
            source: source.clone(),
            mode: ApplyMode::WorkingTree,
        },
    )
    .await;
    assert_eq!(
        failed.error.as_deref(),
        Some("error: f.txt: patch does not apply")
    );
    assert_eq!(worktree_text(dir, "f.txt"), "a\nZ\nc\n");
    assert_eq!(worktree_text(dir, "sub/g.txt"), "x\n");

    // A three-way check flags the conflicting file; the apply leaves it unmerged.
    let three_way = check_patch(
        &handle,
        &mut rx,
        PatchCheckKey {
            source: source.clone(),
            mode: ApplyMode::ThreeWay,
        },
    )
    .await;
    assert!(three_way.applies(), "{three_way:?}");
    assert_eq!(three_way.conflict_count(), 1);
    assert!(
        three_way
            .files
            .iter()
            .any(|f| f.path == "f.txt" && f.conflicts)
    );

    let outcome = run_git_op(
        &handle,
        &mut rx,
        GitOp::ApplyPatch {
            source,
            mode: ApplyMode::ThreeWay,
        },
    )
    .await;
    assert!(!outcome.success);
    assert_eq!(
        outcome.failure,
        Some(GitFailure::PatchConflicts {
            paths: vec!["f.txt".into()]
        })
    );
    let status = fetch_stage_status(&handle, &mut rx, false).await;
    assert_eq!(
        status
            .conflicted
            .iter()
            .map(|e| e.path.as_str())
            .collect::<Vec<_>>(),
        vec!["f.txt"]
    );
    assert!(worktree_text(dir, "f.txt").contains("<<<<<<<"));

    // A missing patch file fails the op cleanly.
    let outcome = run_git_op(
        &handle,
        &mut rx,
        GitOp::ApplyPatch {
            source: PatchInput::File(patch_dir.path().join("missing.patch")),
            mode: ApplyMode::WorkingTree,
        },
    )
    .await;
    assert!(matches!(
        outcome.failure,
        Some(GitFailure::PatchDoesNotApply { .. })
    ));
}

async fn load_tree(
    handle: &EngineHandle,
    rx: &mut UnboundedReceiver<EngineEvent>,
    key: jbgit_core::TreeKey,
) -> Arc<jbgit_core::TreeListing> {
    let epoch = handle.request_tree(key.clone());
    recv_matching(rx, |ev| match ev {
        EngineEvent::TreeLoaded { listing, epoch: e } if e == epoch && listing.key == key => {
            Some(listing)
        }
        _ => None,
    })
    .await
}

async fn load_tree_blob(
    handle: &EngineHandle,
    rx: &mut UnboundedReceiver<EngineEvent>,
    key: jbgit_core::TreeBlobKey,
) -> Arc<jbgit_core::TreeBlob> {
    let epoch = handle.request_tree_blob(key.clone());
    recv_matching(rx, |ev| match ev {
        EngineEvent::TreeBlobLoaded { blob, epoch: e } if e == epoch && blob.key == key => {
            Some(blob)
        }
        _ => None,
    })
    .await
}

#[tokio::test]
async fn tree_browser_lists_directories_lazily_with_submodules_and_binary_previews() {
    use jbgit_core::{TreeBlobContent, TreeBlobKey, TreeEntryKind, TreeKey};

    let sub_guard = TempDir::new().unwrap();
    let sub = sub_guard.path();
    git(sub, &["init", "-b", "main"]);
    fs::write(sub.join("s.txt"), "s\n").unwrap();
    git(sub, &["add", "."]);
    git(sub, &["commit", "-m", "sub"]);

    let dir_guard = TempDir::new().unwrap();
    let dir = dir_guard.path();
    git(dir, &["init", "-b", "main"]);
    git(dir, &["config", "core.autocrlf", "false"]);
    fs::create_dir_all(dir.join("src/nested")).unwrap();
    fs::write(dir.join("README.md"), "# demo\n").unwrap();
    fs::write(dir.join("src/main.rs"), "fn main() {}\n").unwrap();
    fs::write(dir.join("src/nested/deep.txt"), "deep\n").unwrap();
    fs::write(dir.join("logo.bin"), [0u8, 159, 146, 150, 0, 1, 2]).unwrap();
    git(dir, &["add", "."]);
    git(dir, &["commit", "-m", "init"]);
    git(
        dir,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            sub.to_str().unwrap(),
            "libs/sub",
        ],
    );
    git(dir, &["commit", "-m", "add submodule"]);
    let head = rev(dir, "HEAD");
    // Later edit: the browser shows the revision, not the working tree.
    fs::write(dir.join("README.md"), "# changed\n").unwrap();

    let loc = RepoLocation::discover(dir).unwrap();
    let (handle, mut rx) = EngineSupervisor::start(loc, false).unwrap();
    let key = |dir_path: &str| TreeKey {
        oid: head,
        dir: dir_path.to_string(),
    };

    let root = load_tree(&handle, &mut rx, key("")).await;
    let names: Vec<_> = root
        .entries
        .iter()
        .map(|e| (e.name.as_str(), e.kind))
        .collect();
    assert_eq!(
        names,
        vec![
            ("libs", TreeEntryKind::Tree),
            ("src", TreeEntryKind::Tree),
            (".gitmodules", TreeEntryKind::Blob),
            ("logo.bin", TreeEntryKind::Blob),
            ("README.md", TreeEntryKind::Blob),
        ]
    );
    let readme = root.entries.iter().find(|e| e.name == "README.md").unwrap();
    assert_eq!(readme.size, Some(7));

    // Subdirectories load lazily, one level at a time.
    let src = load_tree(&handle, &mut rx, key("src")).await;
    let src_paths: Vec<_> = src.entries.iter().map(|e| e.path.as_str()).collect();
    assert_eq!(src_paths, vec!["src/nested", "src/main.rs"]);
    let libs = load_tree(&handle, &mut rx, key("libs")).await;
    assert_eq!(libs.entries.len(), 1);
    assert_eq!(libs.entries[0].kind, TreeEntryKind::Submodule);
    assert_eq!(libs.entries[0].mode, 0o160_000);
    assert_eq!(libs.entries[0].oid, rev(sub, "HEAD"));

    let text = load_tree_blob(
        &handle,
        &mut rx,
        TreeBlobKey {
            oid: head,
            path: "README.md".into(),
        },
    )
    .await;
    assert_eq!(text.content, TreeBlobContent::Text("# demo\n".into()));
    let bin = load_tree_blob(
        &handle,
        &mut rx,
        TreeBlobKey {
            oid: head,
            path: "logo.bin".into(),
        },
    )
    .await;
    assert_eq!(bin.content, TreeBlobContent::Binary);
    assert_eq!(bin.size, 7);
    let missing = load_tree_blob(
        &handle,
        &mut rx,
        TreeBlobKey {
            oid: head,
            path: "nope.txt".into(),
        },
    )
    .await;
    assert_eq!(missing.content, TreeBlobContent::Missing);

    // Unknown directories fail without wedging the request slot.
    let epoch = handle.request_tree(key("does/not/exist"));
    let failed = recv_matching(&mut rx, |ev| match ev {
        EngineEvent::TreeLoadFailed {
            epoch: e, message, ..
        } if e == epoch => Some(message),
        _ => None,
    })
    .await;
    assert!(!failed.is_empty());
}
