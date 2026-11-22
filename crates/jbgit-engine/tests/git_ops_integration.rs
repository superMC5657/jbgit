//! Acceptance tests for high-level Git write operations executed by
//! `jbgit_engine::run_git_op` against real temporary repositories.

use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use jbgit_core::{
    GitFailure, GitOp, GitOpOutcome, Oid, OngoingKind, OngoingStep, RefKind, RepoSnapshot,
    RepositoryState, RequestSlotId, ResetMode, TodoAction, TodoEntry,
};
use jbgit_engine::{EngineBackend, EngineEvent, EngineSupervisor, RepoLocation, TaskRequest};
use tempfile::TempDir;
use tokio::sync::mpsc::UnboundedReceiver;

fn git_out(dir: &Path, args: &[&str]) -> (bool, String) {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "Test User")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "Test User")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .output()
        .expect("failed to execute git");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    (output.status.success(), text)
}

fn git(dir: &Path, args: &[&str]) -> String {
    let (ok, text) = git_out(dir, args);
    assert!(ok, "git {args:?} failed: {text}");
    text
}

fn init_repo() -> TempDir {
    let tmp = TempDir::new().unwrap();
    git(tmp.path(), &["init", "-b", "main"]);
    git(tmp.path(), &["config", "user.name", "Test User"]);
    git(tmp.path(), &["config", "user.email", "test@example.com"]);
    git(tmp.path(), &["config", "core.autocrlf", "false"]);
    tmp
}

fn commit_file(dir: &Path, file: &str, content: &str, msg: &str) {
    fs::write(dir.join(file), content).unwrap();
    git(dir, &["add", "."]);
    git(dir, &["commit", "-m", msg]);
}

fn rev(dir: &Path, spec: &str) -> Oid {
    git(dir, &["rev-parse", spec]).trim().parse().unwrap()
}

fn subjects(dir: &Path) -> Vec<String> {
    git(dir, &["log", "--format=%s"])
        .lines()
        .map(ToString::to_string)
        .collect()
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

async fn run(dir: &Path, op: GitOp) -> GitOpOutcome {
    let loc = RepoLocation::discover(dir).unwrap();
    let (handle, mut rx) = EngineSupervisor::start(loc, false).unwrap();
    let _ = handle.dispatch(TaskRequest::RunGitOp { op });
    recv_matching(&mut rx, |ev| match ev {
        EngineEvent::GitOpFinished(out) => Some(out),
        _ => None,
    })
    .await
}

async fn snapshot(dir: &Path) -> Arc<RepoSnapshot> {
    let loc = RepoLocation::discover(dir).unwrap();
    let (handle, mut rx) = EngineSupervisor::start(loc, false).unwrap();
    let epoch = handle.request_repo_snapshot();
    recv_matching(&mut rx, |ev| match ev {
        EngineEvent::RepoSnapshotLoaded { snapshot, epoch: e } if e == epoch => Some(snapshot),
        _ => None,
    })
    .await
}

#[tokio::test]
async fn reword_and_drop_three_commits_back_preserve_later_history() {
    let tmp = init_repo();
    let d = tmp.path();
    for i in 1..=5 {
        commit_file(d, &format!("f{i}.txt"), &format!("{i}\n"), &format!("c{i}"));
    }
    // HEAD = c5; HEAD~3 = c2.
    let c2 = rev(d, "HEAD~3");
    let snap = snapshot(d).await;
    let todo_from = |base: Oid| -> Vec<TodoEntry> {
        let mut v: Vec<TodoEntry> = snap
            .commits
            .iter()
            .take_while(|c| c.oid != base)
            .map(|c| TodoEntry::pick(c.oid, c.subject.clone()))
            .collect();
        v.reverse();
        v
    };

    // Reword c2 (three steps before HEAD).
    let mut todo = todo_from(rev(d, "HEAD~4"));
    assert_eq!(todo.len(), 4);
    assert_eq!(todo[0].oid, c2);
    todo[0].action = TodoAction::Reword;
    todo[0].message = Some("c2 reworded\n\nwith body".into());
    let out = run(
        d,
        GitOp::RewriteHistory {
            base: Some(rev(d, "HEAD~4")),
            todo,
        },
    )
    .await;
    assert!(out.success, "reword failed: {:?}", out);
    assert_eq!(subjects(d), vec!["c5", "c4", "c3", "c2 reworded", "c1"]);
    assert_eq!(
        git(d, &["log", "-1", "--format=%b", "HEAD~3"]).trim(),
        "with body"
    );
    for i in 1..=5 {
        assert!(d.join(format!("f{i}.txt")).exists());
    }

    // Drop the (rewritten) c2.
    let snap = snapshot(d).await;
    let base = rev(d, "HEAD~4");
    let mut todo: Vec<TodoEntry> = snap
        .commits
        .iter()
        .take_while(|c| c.oid != base)
        .map(|c| TodoEntry::pick(c.oid, c.subject.clone()))
        .collect();
    todo.reverse();
    todo[0].action = TodoAction::Drop;
    let out = run(
        d,
        GitOp::RewriteHistory {
            base: Some(base),
            todo,
        },
    )
    .await;
    assert!(out.success, "drop failed: {:?}", out);
    assert_eq!(subjects(d), vec!["c5", "c4", "c3", "c1"]);
    assert!(!d.join("f2.txt").exists());
    assert!(d.join("f5.txt").exists());
    assert!(!d.join(".git").join("jbgit-rebase").exists());

    // Squash c4 into c3 (combined message), HEAD reword fast path.
    let snap = snapshot(d).await;
    let base = rev(d, "HEAD~3");
    let mut todo: Vec<TodoEntry> = snap
        .commits
        .iter()
        .take_while(|c| c.oid != base)
        .map(|c| TodoEntry::pick(c.oid, c.subject.clone()))
        .collect();
    todo.reverse();
    todo[1].action = TodoAction::Squash;
    todo[1].message = Some("c3+c4".into());
    let out = run(
        d,
        GitOp::RewriteHistory {
            base: Some(base),
            todo,
        },
    )
    .await;
    assert!(out.success, "squash failed: {:?}", out);
    assert_eq!(subjects(d), vec!["c5", "c3+c4", "c1"]);
    let out = run(
        d,
        GitOp::RewordHead {
            message: "c5 final".into(),
        },
    )
    .await;
    assert!(out.success);
    assert_eq!(subjects(d)[0], "c5 final");
}

#[tokio::test]
async fn smart_checkout_stashes_switches_and_restores_local_changes() {
    let tmp = init_repo();
    let d = tmp.path();
    let base: String = (1..=10).map(|i| format!("line {i}\n")).collect();
    commit_file(d, "shared.txt", &base, "base");
    git(d, &["checkout", "-b", "feature"]);
    commit_file(
        d,
        "shared.txt",
        &base.replace("line 1\n", "feature 1\n"),
        "feature edit",
    );
    git(d, &["checkout", "main"]);

    // Local, uncommitted edit of the same file (different region) + untracked file.
    fs::write(
        d.join("shared.txt"),
        base.replace("line 10\n", "local 10\n"),
    )
    .unwrap();
    fs::write(d.join("notes.txt"), "untracked\n").unwrap();

    let plain = run(
        d,
        GitOp::Checkout {
            target: "feature".into(),
            detach: false,
        },
    )
    .await;
    assert!(!plain.success);
    match plain.failure {
        Some(GitFailure::LocalChangesOverwritten { files }) => {
            assert_eq!(files, vec!["shared.txt".to_string()]);
        }
        other => panic!("expected local-changes failure, got {other:?}"),
    }
    assert_eq!(snapshot(d).await.head_branch.as_deref(), Some("main"));

    let smart = run(
        d,
        GitOp::Smart(Box::new(GitOp::Checkout {
            target: "feature".into(),
            detach: false,
        })),
    )
    .await;
    assert!(smart.success, "smart checkout failed: {smart:?}");
    assert!(!smart.stash_kept);
    assert_eq!(snapshot(d).await.head_branch.as_deref(), Some("feature"));
    let content = fs::read_to_string(d.join("shared.txt")).unwrap();
    assert!(content.starts_with("feature 1\n"), "{content}");
    assert!(content.ends_with("local 10\n"), "{content}");
    assert_eq!(
        fs::read_to_string(d.join("notes.txt")).unwrap(),
        "untracked\n"
    );
    assert!(snapshot(d).await.stashes.is_empty());
}

#[tokio::test]
async fn rebase_conflict_reports_progress_and_abort_restores_original_state() {
    let tmp = init_repo();
    let d = tmp.path();
    commit_file(d, "conflict.txt", "base\n", "base");
    git(d, &["checkout", "-b", "feat"]);
    commit_file(d, "conflict.txt", "feat change\n", "feat 1");
    commit_file(d, "other.txt", "x\n", "feat 2");
    git(d, &["checkout", "main"]);
    commit_file(d, "conflict.txt", "main change\n", "main change");
    git(d, &["checkout", "feat"]);
    let original_head = rev(d, "HEAD");

    let out = run(
        d,
        GitOp::Rebase {
            onto: "main".into(),
            branch: None,
        },
    )
    .await;
    assert!(!out.success);
    assert_eq!(out.failure, Some(GitFailure::Conflicts));

    let snap = snapshot(d).await;
    assert_eq!(
        OngoingKind::from_state(&snap.state),
        Some(OngoingKind::Rebase)
    );
    let label = snap.state.banner_label().unwrap();
    assert!(label.contains("REBASING"), "{label}");
    assert!(label.contains("feat"), "{label}");
    assert!(label.contains("(1/2)"), "{label}");

    let abort = run(
        d,
        GitOp::Ongoing {
            kind: OngoingKind::Rebase,
            step: OngoingStep::Abort,
        },
    )
    .await;
    assert!(abort.success, "{abort:?}");
    let snap = snapshot(d).await;
    assert_eq!(snap.state, RepositoryState::Clean);
    assert_eq!(snap.head_branch.as_deref(), Some("feat"));
    assert_eq!(rev(d, "HEAD"), original_head);
    assert!(git(d, &["status", "--porcelain"]).trim().is_empty());

    // Merge conflict → MERGING banner → abort.
    git(d, &["checkout", "main"]);
    let out = run(d, GitOp::Merge { rev: "feat".into() }).await;
    assert_eq!(out.failure, Some(GitFailure::Conflicts));
    assert_eq!(snapshot(d).await.state, RepositoryState::Merge);
    let abort = run(
        d,
        GitOp::Ongoing {
            kind: OngoingKind::Merge,
            step: OngoingStep::Abort,
        },
    )
    .await;
    assert!(abort.success);
    assert_eq!(snapshot(d).await.state, RepositoryState::Clean);
}

#[tokio::test]
async fn remote_checkout_tracks_and_branch_delete_can_be_restored() {
    let remote = TempDir::new().unwrap();
    git(remote.path(), &["init", "--bare", "-b", "main"]);
    let tmp = init_repo();
    let d = tmp.path();
    git(
        d,
        &["remote", "add", "origin", remote.path().to_str().unwrap()],
    );
    commit_file(d, "a.txt", "a\n", "init");
    git(d, &["push", "-u", "origin", "main"]);
    git(d, &["checkout", "-b", "feature-new"]);
    commit_file(d, "b.txt", "b\n", "feature work");
    git(d, &["push", "origin", "feature-new"]);
    git(d, &["checkout", "main"]);
    git(d, &["branch", "-D", "feature-new"]);

    let out = run(
        d,
        GitOp::CheckoutNewBranch {
            name: "feature-new".into(),
            start: "origin/feature-new".into(),
            track: true,
        },
    )
    .await;
    assert!(out.success, "{out:?}");
    let snap = snapshot(d).await;
    assert_eq!(snap.head_branch.as_deref(), Some("feature-new"));
    let local = snap.local_branch("feature-new").unwrap();
    assert_eq!(local.upstream.as_deref(), Some("origin/feature-new"));
    assert_eq!(snap.recent_branches, vec!["main".to_string()]);

    // Unmerged local branch: -d refuses, -D deletes, restore brings it back.
    git(d, &["checkout", "-b", "topic"]);
    commit_file(d, "t.txt", "t\n", "topic work");
    let tip = rev(d, "HEAD");
    git(d, &["checkout", "main"]);
    let out = run(
        d,
        GitOp::DeleteBranch {
            name: "topic".into(),
            force: false,
            tip,
        },
    )
    .await;
    assert_eq!(out.failure, Some(GitFailure::NotFullyMerged));
    let out = run(
        d,
        GitOp::DeleteBranch {
            name: "topic".into(),
            force: true,
            tip,
        },
    )
    .await;
    assert!(out.success);
    assert!(snapshot(d).await.local_branch("topic").is_none());
    let out = run(
        d,
        GitOp::RestoreBranch {
            name: "topic".into(),
            oid: tip,
        },
    )
    .await;
    assert!(out.success);
    assert_eq!(snapshot(d).await.local_branch("topic").unwrap().target_oid, tip);
    let recent = snapshot(d).await.recent_branches.clone();
    assert!(recent.contains(&"topic".to_string()), "{recent:?}");
}

#[tokio::test]
async fn reset_modes_stash_workflow_and_cherry_pick() {
    let tmp = init_repo();
    let d = tmp.path();
    commit_file(d, "a.txt", "1\n", "one");
    commit_file(d, "a.txt", "2\n", "two");
    let one = rev(d, "HEAD~1");

    // Keep with no conflicting local changes keeps unrelated edits.
    fs::write(d.join("b.txt"), "local\n").unwrap();
    git(d, &["add", "b.txt"]);
    let out = run(
        d,
        GitOp::Reset {
            mode: ResetMode::Soft,
            target: one,
        },
    )
    .await;
    assert!(out.success);
    assert_eq!(subjects(d), vec!["one"]);
    let status = git(d, &["status", "--porcelain"]);
    assert!(
        status.contains("M  a.txt") && status.contains("A  b.txt"),
        "{status}"
    );

    // Stash push with message + untracked, then list, apply, drop, branch.
    fs::write(d.join("c.txt"), "untracked\n").unwrap();
    let out = run(
        d,
        GitOp::StashPush {
            message: Some("wip parser".into()),
            include_untracked: true,
            paths: Vec::new(),
        },
    )
    .await;
    assert!(out.success, "{out:?}");
    let snap = snapshot(d).await;
    assert_eq!(snap.stashes.len(), 1);
    assert_eq!(snap.stashes[0].short_message(), "wip parser");
    assert_eq!(snap.stashes[0].branch(), Some("main"));
    assert!(!d.join("c.txt").exists());
    let out = run(
        d,
        GitOp::StashBranch {
            index: 0,
            name: "from-stash".into(),
        },
    )
    .await;
    assert!(out.success, "{out:?}");
    let snap = snapshot(d).await;
    assert_eq!(snap.head_branch.as_deref(), Some("from-stash"));
    assert!(snap.stashes.is_empty());
    assert!(d.join("c.txt").exists());

    // Hard reset back, then cherry-pick `two` from the reflog.
    let out = run(
        d,
        GitOp::Reset {
            mode: ResetMode::Hard,
            target: one,
        },
    )
    .await;
    assert!(out.success);
    git(d, &["checkout", "main"]);
    git(d, &["clean", "-fd"]);
    git(d, &["checkout", "-b", "pick-target", &one.to_string()]);
    commit_file(d, "z.txt", "z\n", "zed");
    git(d, &["checkout", "main"]);
    let zed = rev(d, "pick-target");
    let out = run(d, GitOp::CherryPick { oids: vec![zed] }).await;
    assert!(out.success, "{out:?}");
    assert_eq!(subjects(d)[0], "zed");
    assert!(
        snapshot(d).await
            .refs
            .iter()
            .any(|r| r.kind == RefKind::LocalBranch && r.name == "pick-target")
    );
}

#[tokio::test]
async fn root_commit_fixup_patch_compare_and_conflict_resolution() {
    use jbgit_core::{ConflictChoice, DetailsKey, DiffContext, FileDiffKey};

    let tmp = init_repo();
    let d = tmp.path();
    commit_file(d, "root.txt", "v1\n", "root");
    let root = rev(d, "HEAD");
    commit_file(d, "second.txt", "s1\n", "second");
    let _second = rev(d, "HEAD");

    // Root-commit fixup + autosquash and root-inclusive format-patch work without ~1.
    fs::write(d.join("root.txt"), "v2\n").unwrap();
    git(d, &["add", "root.txt"]);
    let fix = run(
        d,
        GitOp::FixupCommit {
            target: root,
            base: None,
            autosquash: true,
        },
    )
    .await;
    assert!(fix.success, "{fix:?}");
    assert_eq!(subjects(d), vec!["second", "root"]);
    assert_eq!(fs::read_to_string(d.join("root.txt")).unwrap(), "v2\n");

    let new_root = rev(d, "HEAD~1");
    let new_second = rev(d, "HEAD");
    let patch = run(
        d,
        GitOp::CreatePatchFiles {
            oids: vec![new_second, new_root],
            base: None,
        },
    )
    .await;
    assert!(patch.success, "{patch:?}");

    // CompareVersions & CompareWithLocal produce real file changes + diffs.
    let loc = RepoLocation::discover(d).unwrap();
    let (handle, mut rx) = EngineSupervisor::start(loc, false).unwrap();

    let e1 = handle.request_commit_details(
        RequestSlotId::LogCommitDetails,
        DetailsKey::compare_versions(new_root, new_second),
    );
    let cmp_ver = recv_matching(&mut rx, |ev| match ev {
        EngineEvent::CommitDetailsLoaded { bundle, epoch, .. } if epoch == e1 => Some(bundle),
        _ => None,
    })
    .await;
    assert!(cmp_ver.changes.iter().any(|c| c.path == "second.txt"));

    fs::write(d.join("root.txt"), "v3-local\n").unwrap();
    let e2 = handle.request_commit_details(
        RequestSlotId::LogCommitDetails,
        DetailsKey::compare_with_local(new_root),
    );
    let cmp_local = recv_matching(&mut rx, |ev| match ev {
        EngineEvent::CommitDetailsLoaded { bundle, epoch, .. } if epoch == e2 => Some(bundle),
        _ => None,
    })
    .await;
    assert!(cmp_local.changes.iter().any(|c| c.path == "root.txt"));

    let e3 = handle.request_file_diff(
        RequestSlotId::LogFileDiff,
        FileDiffKey {
            details: DetailsKey::compare_with_local(new_root),
            path: "root.txt".into(),
            old_path: None,
            context: DiffContext::Three,
        },
    );
    let diff = recv_matching(&mut rx, |ev| match ev {
        EngineEvent::FileDiffLoaded { diff, epoch, .. } if epoch == e3 => Some(diff),
        _ => None,
    })
    .await;
    assert!(!diff.hunks.is_empty());

    // Three-pane conflict loader + resolver stages the resolved file.
    git(d, &["checkout", "--", "root.txt"]);
    git(d, &["checkout", "-b", "side", &new_root.to_string()]);
    commit_file(d, "root.txt", "side-edit\n", "side edit");
    git(d, &["checkout", "main"]);
    commit_file(d, "root.txt", "main-edit\n", "main edit");
    let merge = run(d, GitOp::Merge { rev: "side".into() }).await;
    assert_eq!(merge.failure, Some(GitFailure::Conflicts));

    let e4 = handle.request_conflict_document("root.txt".into());
    let mut doc = recv_matching(&mut rx, |ev| match ev {
        EngineEvent::ConflictDocumentLoaded { doc, epoch } if epoch == e4 => Some(doc),
        _ => None,
    })
    .await;
    assert_eq!(doc.conflict_count, 1);
    doc.set_choice(0, ConflictChoice::Theirs);
    let _ = handle.dispatch(TaskRequest::SaveConflictResolution {
        path: "root.txt".into(),
        content: doc.render_merged().into_bytes(),
        stage: true,
    });
    recv_matching(&mut rx, |ev| match ev {
        EngineEvent::ExternalFsChanged(_) => Some(()),
        _ => None,
    })
    .await;
    assert_eq!(fs::read_to_string(d.join("root.txt")).unwrap(), "side-edit\n");
    let porcelain = git(d, &["status", "--porcelain"]);
    assert!(porcelain.contains("M  root.txt"), "{porcelain}");

    // GitOp::CheckoutPaths restores an older file revision without touching unrelated files.
    git(d, &["commit", "-m", "resolved"]);
    fs::create_dir_all(d.join("sub dir")).unwrap();
    commit_file(d, "sub dir/note name.txt", "old version\n", "add note");
    let old_oid = rev(d, "HEAD");
    commit_file(d, "sub dir/note name.txt", "new version\n", "update note");
    let unrelated_before = fs::read_to_string(d.join("root.txt")).unwrap();
    let restore = run(
        d,
        GitOp::CheckoutPaths {
            rev: old_oid.to_string(),
            paths: vec!["sub dir/note name.txt".into()],
        },
    )
    .await;
    assert!(restore.success, "CheckoutPaths failed: {restore:?}");
    assert_eq!(
        fs::read_to_string(d.join("sub dir/note name.txt")).unwrap(),
        "old version\n"
    );
    assert_eq!(fs::read_to_string(d.join("root.txt")).unwrap(), unrelated_before);
}
