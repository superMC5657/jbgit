//! Acceptance tests for high-level Git write operations executed by
//! `jbgit_engine::run_git_op` against real temporary repositories.

use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use jbgit_core::{
    FastForward, FixupKind, GitFailure, GitOp, GitOpOutcome, MergeOptions, Oid, OngoingKind,
    OngoingStep, PullOptions, RebaseOptions, RefKind, RepoSnapshot, RepositoryState, RequestSlotId,
    ResetMode, TodoAction, TodoEntry, UpdateMode, plan_squash_todo,
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
            no_verify: false,
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
            no_verify: false,
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
            no_verify: false,
        },
    )
    .await;
    assert!(out.success, "squash failed: {:?}", out);
    assert_eq!(subjects(d), vec!["c5", "c3+c4", "c1"]);
    let out = run(
        d,
        GitOp::RewordHead {
            message: "c5 final".into(),
            no_verify: false,
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
            options: RebaseOptions::onto("main", None),
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
    let out = run(
        d,
        GitOp::Merge {
            rev: "feat".into(),
            options: Default::default(),
        },
    )
    .await;
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
            force: false,
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
    assert_eq!(
        snapshot(d).await.local_branch("topic").unwrap().target_oid,
        tip
    );
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
            keep_index: false,
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
    let out = run(
        d,
        GitOp::CherryPick {
            oids: vec![zed],
            no_commit: false,
            record_origin: false,
            mainline: None,
        },
    )
    .await;
    assert!(out.success, "{out:?}");
    assert_eq!(subjects(d)[0], "zed");
    assert!(
        snapshot(d)
            .await
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
            kind: FixupKind::Fixup,
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
    let patch_out = TempDir::new().unwrap();
    let patch = run(
        d,
        GitOp::CreatePatchFile {
            source: jbgit_core::PatchSource::Commits {
                oids: vec![new_second, new_root],
                base: None,
            },
            output: patch_out.path().to_path_buf(),
            reverse: false,
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
            whitespace: jbgit_core::WhitespaceMode::Default,
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
    let merge = run(
        d,
        GitOp::Merge {
            rev: "side".into(),
            options: Default::default(),
        },
    )
    .await;
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
        delete: false,
    });
    recv_matching(&mut rx, |ev| match ev {
        EngineEvent::ExternalFsChanged(_) => Some(()),
        _ => None,
    })
    .await;
    assert_eq!(
        fs::read_to_string(d.join("root.txt")).unwrap(),
        "side-edit\n"
    );
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
    assert_eq!(
        fs::read_to_string(d.join("root.txt")).unwrap(),
        unrelated_before
    );
}

fn configure_identity(dir: &Path) {
    git(dir, &["config", "user.name", "Test User"]);
    git(dir, &["config", "user.email", "test@example.com"]);
    git(dir, &["config", "core.autocrlf", "false"]);
}

/// Bare remote + working clone `d` (main pushed with upstream) + second clone `other`.
fn remote_fixture() -> (TempDir, TempDir, TempDir) {
    let remote = TempDir::new().unwrap();
    git(remote.path(), &["init", "--bare", "-b", "main"]);
    let tmp = init_repo();
    let d = tmp.path();
    git(
        d,
        &["remote", "add", "origin", remote.path().to_str().unwrap()],
    );
    commit_file(d, "a.txt", "a\n", "init");
    commit_file(d, "shared.txt", "base\n", "shared");
    git(d, &["push", "-u", "origin", "main"]);
    let other = TempDir::new().unwrap();
    git(
        other.path(),
        &["clone", remote.path().to_str().unwrap(), "."],
    );
    configure_identity(other.path());
    (remote, tmp, other)
}

fn merge_op(rev: &str, options: MergeOptions) -> GitOp {
    GitOp::Merge {
        rev: rev.into(),
        options,
    }
}

#[tokio::test]
async fn merge_options_squash_no_commit_message_and_ff_only() {
    let tmp = init_repo();
    let d = tmp.path();
    commit_file(d, "base.txt", "base\n", "base");
    git(d, &["checkout", "-b", "feat"]);
    commit_file(d, "f1.txt", "1\n", "feat one");
    commit_file(d, "f2.txt", "2\n", "feat two");
    git(d, &["checkout", "main"]);
    commit_file(d, "m.txt", "m\n", "main diverges");
    let head = rev(d, "HEAD");

    // --squash: changes staged, HEAD unchanged, SQUASH_MSG surfaced.
    let squash = run(
        d,
        merge_op(
            "feat",
            MergeOptions {
                squash: true,
                ..Default::default()
            },
        ),
    )
    .await;
    assert!(squash.success, "{squash:?}");
    assert_eq!(rev(d, "HEAD"), head);
    let staged = git(d, &["diff", "--cached", "--name-only"]);
    assert!(
        staged.contains("f1.txt") && staged.contains("f2.txt"),
        "{staged}"
    );
    let msg = squash.prepared_message.expect("SQUASH_MSG prefill");
    assert!(msg.contains("Squashed commit"), "{msg}");
    assert!(msg.contains("feat two"), "{msg}");
    git(d, &["reset", "--hard"]);

    // --ff-only on diverged history: NotFastForward, nothing changes.
    let ff_only = run(
        d,
        merge_op(
            "feat",
            MergeOptions {
                ff: FastForward::FfOnly,
                ..Default::default()
            },
        ),
    )
    .await;
    assert_eq!(
        ff_only.failure,
        Some(GitFailure::NotFastForward),
        "{ff_only:?}"
    );
    assert_eq!(rev(d, "HEAD"), head);

    // --no-ff --no-commit: MERGE_HEAD + MERGE_MSG, merge left uncommitted.
    let no_commit = run(
        d,
        merge_op(
            "feat",
            MergeOptions {
                ff: FastForward::NoFf,
                no_commit: true,
                ..Default::default()
            },
        ),
    )
    .await;
    assert!(no_commit.success, "{no_commit:?}");
    assert_eq!(rev(d, "HEAD"), head);
    assert!(d.join(".git").join("MERGE_HEAD").exists());
    let msg = no_commit.prepared_message.expect("MERGE_MSG prefill");
    assert!(msg.starts_with("Merge branch 'feat'"), "{msg}");
    assert_eq!(snapshot(d).await.state, RepositoryState::Merge);
    git(d, &["merge", "--abort"]);

    // Custom multi-line message via `-F -` and an explicit strategy.
    let custom = run(
        d,
        merge_op(
            "feat",
            MergeOptions {
                ff: FastForward::NoFf,
                message: Some("Custom title\n\nBody line".into()),
                strategy: Some("ort".into()),
                ..Default::default()
            },
        ),
    )
    .await;
    assert!(custom.success, "{custom:?}");
    assert!(custom.prepared_message.is_none());
    assert_eq!(git(d, &["log", "-1", "--format=%s"]).trim(), "Custom title");
    assert_eq!(git(d, &["log", "-1", "--format=%b"]).trim(), "Body line");
    assert_eq!(
        git(d, &["rev-list", "--parents", "-1", "HEAD"])
            .split_whitespace()
            .count(),
        3
    );
}

#[tokio::test]
async fn update_project_fetches_merges_and_preserves_local_changes() {
    let (_remote, tmp, other) = remote_fixture();
    let d = tmp.path();
    let o = other.path();
    commit_file(o, "r1.txt", "r1\n", "remote one");
    commit_file(o, "r2.txt", "r2\n", "remote two");
    git(o, &["push", "origin", "main"]);

    // Uncommitted tracked edit + untracked file.
    fs::write(d.join("a.txt"), "a local edit\n").unwrap();
    fs::write(d.join("scratch.txt"), "untracked\n").unwrap();
    let before = rev(d, "HEAD");

    let out = run(
        d,
        GitOp::UpdateProject {
            mode: Some(UpdateMode::Merge),
            save_changes: true,
            update_other_branches: true,
            update_submodules: false,
        },
    )
    .await;
    assert!(out.success, "{out:?}");
    let info = out.update_info.expect("update info");
    assert_eq!(info.new_commits, 2, "{info:?}");
    assert_eq!(info.files_changed, 2, "{info:?}");
    assert_eq!(info.commits.len(), 2);
    assert_eq!(info.ranges.len(), 1);
    assert_eq!(info.ranges[0].branch, "main");
    assert_eq!(info.ranges[0].before, before);
    assert_eq!(info.ranges[0].after, rev(d, "HEAD"));
    assert!(info.skipped.is_empty());
    assert_eq!(
        fs::read_to_string(d.join("a.txt")).unwrap(),
        "a local edit\n"
    );
    assert_eq!(
        fs::read_to_string(d.join("scratch.txt")).unwrap(),
        "untracked\n"
    );
    assert!(d.join("r2.txt").exists());
    assert!(git(d, &["stash", "list"]).trim().is_empty());

    // Second run: nothing new, empty info, still success.
    let again = run(
        d,
        GitOp::UpdateProject {
            mode: None,
            save_changes: true,
            update_other_branches: true,
            update_submodules: false,
        },
    )
    .await;
    assert!(again.success, "{again:?}");
    assert!(again.update_info.unwrap().is_empty());
}

#[tokio::test]
async fn update_project_fast_forwards_tracked_branches_and_skips_diverged() {
    let (_remote, tmp, other) = remote_fixture();
    let d = tmp.path();
    let o = other.path();
    for b in ["tracked", "diverged"] {
        git(d, &["branch", b]);
        git(d, &["push", "-u", "origin", b]);
    }
    git(o, &["fetch", "origin"]);
    for (b, file) in [("tracked", "t.txt"), ("diverged", "dv.txt")] {
        git(o, &["checkout", "-b", b, &format!("origin/{b}")]);
        commit_file(o, file, "remote\n", &format!("remote {b}"));
        git(o, &["push", "origin", b]);
    }
    // Local-only commit on `diverged`.
    git(d, &["checkout", "diverged"]);
    commit_file(d, "local.txt", "local\n", "local diverged");
    git(d, &["checkout", "main"]);
    let tracked_before = rev(d, "tracked");
    let diverged_before = rev(d, "diverged");

    let out = run(
        d,
        GitOp::UpdateProject {
            mode: Some(UpdateMode::Rebase),
            save_changes: true,
            update_other_branches: true,
            update_submodules: false,
        },
    )
    .await;
    assert!(out.success, "{out:?}");
    let info = out.update_info.unwrap();
    assert_eq!(info.ranges.len(), 1, "{info:?}");
    assert_eq!(info.ranges[0].branch, "tracked");
    assert_eq!(info.ranges[0].before, tracked_before);
    assert_eq!(rev(d, "tracked"), rev(d, "origin/tracked"));
    assert_eq!(info.new_commits, 1);
    assert_eq!(info.skipped.len(), 1);
    assert_eq!(info.skipped[0].branch, "diverged");
    assert_eq!(rev(d, "diverged"), diverged_before);
}

#[tokio::test]
async fn update_project_stops_on_conflict_and_keeps_stash() {
    let (_remote, tmp, other) = remote_fixture();
    let d = tmp.path();
    let o = other.path();
    commit_file(o, "shared.txt", "remote change\n", "remote edit");
    git(o, &["push", "origin", "main"]);
    commit_file(d, "shared.txt", "local change\n", "local edit");
    fs::write(d.join("a.txt"), "dirty\n").unwrap();

    let out = run(
        d,
        GitOp::UpdateProject {
            mode: Some(UpdateMode::Merge),
            save_changes: true,
            update_other_branches: false,
            update_submodules: false,
        },
    )
    .await;
    assert_eq!(out.failure, Some(GitFailure::Conflicts), "{out:?}");
    assert!(out.stash_kept);
    assert!(out.update_info.is_none());
    assert_eq!(snapshot(d).await.state, RepositoryState::Merge);
    let stashes = git(d, &["stash", "list"]);
    assert!(stashes.contains("jbgit update project"), "{stashes}");
    git(d, &["merge", "--abort"]);
    git(d, &["stash", "pop"]);
    assert_eq!(fs::read_to_string(d.join("a.txt")).unwrap(), "dirty\n");
}

#[tokio::test]
async fn rebase_conflict_reverses_accept_side_and_document_orientation() {
    use jbgit_core::ConflictChoice;

    let tmp = init_repo();
    let d = tmp.path();
    commit_file(d, "shared.txt", "base\n", "base");
    git(d, &["checkout", "-b", "feat"]);
    commit_file(d, "shared.txt", "local-feat-change\n", "feat edit");
    git(d, &["checkout", "main"]);
    commit_file(d, "shared.txt", "upstream-main-change\n", "main edit");
    git(d, &["checkout", "feat"]);

    let rb = run(
        d,
        GitOp::Rebase {
            options: RebaseOptions::onto("main", None),
        },
    )
    .await;
    assert_eq!(rb.failure, Some(GitFailure::Conflicts), "{rb:?}");

    // Loaded conflict document must swap markers/stages so Ours = replayed local commit, Theirs = onto.
    let loc = RepoLocation::discover(d).unwrap();
    let (handle, mut rx) = EngineSupervisor::start(loc, false).unwrap();
    let e1 = handle.request_conflict_document("shared.txt".into());
    let mut doc = recv_matching(&mut rx, |ev| match ev {
        EngineEvent::ConflictDocumentLoaded { doc, epoch } if epoch == e1 => Some(doc),
        _ => None,
    })
    .await;
    assert!(doc.is_reversed, "rebase conflict must set is_reversed");
    let blk = doc.block(0).unwrap();
    assert_eq!(blk.ours_lines, vec!["local-feat-change"]);
    assert_eq!(blk.theirs_lines, vec!["upstream-main-change"]);
    doc.set_choice(0, ConflictChoice::Ours);
    assert_eq!(doc.render_resolved(), "local-feat-change\n");

    // AcceptSide { ours: true } during rebase MUST keep the replayed local commit content.
    let accept_yours = run(
        d,
        GitOp::AcceptSide {
            paths: vec!["shared.txt".into()],
            ours: true,
        },
    )
    .await;
    assert!(accept_yours.success, "{accept_yours:?}");
    assert_eq!(
        fs::read_to_string(d.join("shared.txt")).unwrap(),
        "local-feat-change\n"
    );
    let porcelain = git(d, &["status", "--porcelain"]);
    assert!(!porcelain.contains("UU "), "{porcelain}");

    // Abort, re-trigger rebase conflict, and verify AcceptSide { ours: false } keeps upstream (onto).
    git(d, &["rebase", "--abort"]);
    let rb2 = run(
        d,
        GitOp::Rebase {
            options: RebaseOptions::onto("main", None),
        },
    )
    .await;
    assert_eq!(rb2.failure, Some(GitFailure::Conflicts), "{rb2:?}");
    let accept_theirs = run(
        d,
        GitOp::AcceptSide {
            paths: vec!["shared.txt".into()],
            ours: false,
        },
    )
    .await;
    assert!(accept_theirs.success, "{accept_theirs:?}");
    assert_eq!(
        fs::read_to_string(d.join("shared.txt")).unwrap(),
        "upstream-main-change\n"
    );
    let porcelain2 = git(d, &["status", "--porcelain"]);
    assert!(!porcelain2.contains("UU "), "{porcelain2}");
}

#[tokio::test]
async fn delete_modify_conflicts_resolve_via_git_rm_and_save_delete() {
    use jbgit_core::{ConflictChoice, ConflictSidePresence};

    let tmp = init_repo();
    let d = tmp.path();
    commit_file(d, "keep.txt", "anchor\n", "base");
    commit_file(d, "del_them.txt", "v1\n", "add del_them");
    commit_file(d, "del_us.txt", "v1\n", "add del_us");

    // Branch `them` deletes del_them.txt and modifies del_us.txt.
    git(d, &["checkout", "-b", "them"]);
    git(d, &["rm", "del_them.txt"]);
    git(d, &["commit", "-m", "them deletes del_them"]);
    commit_file(d, "del_us.txt", "them-edit\n", "them edits del_us");

    // `main` modifies del_them.txt and deletes del_us.txt.
    git(d, &["checkout", "main"]);
    commit_file(d, "del_them.txt", "main-edit\n", "main edits del_them");
    git(d, &["rm", "del_us.txt"]);
    git(d, &["commit", "-m", "main deletes del_us"]);

    let merge = run(
        d,
        GitOp::Merge {
            rev: "them".into(),
            options: Default::default(),
        },
    )
    .await;
    assert_eq!(merge.failure, Some(GitFailure::Conflicts), "{merge:?}");

    let loc = RepoLocation::discover(d).unwrap();
    let (handle, mut rx) = EngineSupervisor::start(loc, false).unwrap();

    // Load both conflicts and verify side_presence classification.
    let e1 = handle.request_conflict_document("del_them.txt".into());
    let mut doc_them = recv_matching(&mut rx, |ev| match ev {
        EngineEvent::ConflictDocumentLoaded { doc, epoch } if epoch == e1 => Some(doc),
        _ => None,
    })
    .await;
    assert_eq!(
        doc_them.side_presence,
        ConflictSidePresence::DeletedByTheirs
    );

    let e2 = handle.request_conflict_document("del_us.txt".into());
    let doc_us = recv_matching(&mut rx, |ev| match ev {
        EngineEvent::ConflictDocumentLoaded { doc, epoch } if epoch == e2 => Some(doc),
        _ => None,
    })
    .await;
    assert_eq!(doc_us.side_presence, ConflictSidePresence::DeletedByOurs);

    // 1. AcceptSide choosing deleted side on DeletedByTheirs runs `git rm -f` and clears conflict.
    let accept_del_them = run(
        d,
        GitOp::AcceptSide {
            paths: vec!["del_them.txt".into()],
            ours: false,
        },
    )
    .await;
    assert!(accept_del_them.success, "{accept_del_them:?}");
    assert!(!d.join("del_them.txt").exists());
    let st1 = git(d, &["status", "--porcelain"]);
    assert!(
        !st1.contains("UD del_them.txt") && !st1.contains("DU del_them.txt"),
        "{st1}"
    );
    assert!(st1.contains("D  del_them.txt"), "{st1}");

    // 2. AcceptSide choosing deleted side on DeletedByOurs runs `git rm -f` and clears conflict.
    let accept_del_us = run(
        d,
        GitOp::AcceptSide {
            paths: vec!["del_us.txt".into()],
            ours: true,
        },
    )
    .await;
    assert!(accept_del_us.success, "{accept_del_us:?}");
    assert!(!d.join("del_us.txt").exists());
    let st2 = git(d, &["status", "--porcelain"]);
    assert!(
        !st2.contains("DU del_us.txt") && !st2.contains("UD del_us.txt"),
        "{st2}"
    );

    // 3. Three-pane SaveConflictResolution { delete: true, stage: true } removes worktree + index stages.
    git(d, &["merge", "--abort"]);
    let merge2 = run(
        d,
        GitOp::Merge {
            rev: "them".into(),
            options: Default::default(),
        },
    )
    .await;
    assert_eq!(merge2.failure, Some(GitFailure::Conflicts));
    doc_them.set_choice(0, ConflictChoice::Theirs);
    assert!(doc_them.deletes_file());
    let _ = handle.dispatch(TaskRequest::SaveConflictResolution {
        path: "del_them.txt".into(),
        content: Vec::new(),
        stage: true,
        delete: true,
    });
    recv_matching(&mut rx, |ev| match ev {
        EngineEvent::ExternalFsChanged(_) => Some(()),
        _ => None,
    })
    .await;
    assert!(!d.join("del_them.txt").exists());
    let st3 = git(d, &["status", "--porcelain"]);
    assert!(st3.contains("D  del_them.txt"), "{st3}");
}

#[tokio::test]
async fn conflict_resolution_preserves_crlf_line_endings_on_disk() {
    use jbgit_core::{ConflictChoice, LineEnding};

    let tmp = init_repo();
    let d = tmp.path();
    fs::write(d.join("win.txt"), b"header\r\nbase\r\nfooter\r\n").unwrap();
    git(d, &["add", "win.txt"]);
    git(d, &["commit", "-m", "base crlf"]);

    git(d, &["checkout", "-b", "side"]);
    fs::write(d.join("win.txt"), b"header\r\nside-line\r\nfooter\r\n").unwrap();
    git(d, &["add", "win.txt"]);
    git(d, &["commit", "-m", "side crlf"]);

    git(d, &["checkout", "main"]);
    fs::write(d.join("win.txt"), b"header\r\nmain-line\r\nfooter\r\n").unwrap();
    git(d, &["add", "win.txt"]);
    git(d, &["commit", "-m", "main crlf"]);

    let merge = run(
        d,
        GitOp::Merge {
            rev: "side".into(),
            options: Default::default(),
        },
    )
    .await;
    assert_eq!(merge.failure, Some(GitFailure::Conflicts), "{merge:?}");

    let loc = RepoLocation::discover(d).unwrap();
    let (handle, mut rx) = EngineSupervisor::start(loc, false).unwrap();
    let e1 = handle.request_conflict_document("win.txt".into());
    let mut doc = recv_matching(&mut rx, |ev| match ev {
        EngineEvent::ConflictDocumentLoaded { doc, epoch } if epoch == e1 => Some(doc),
        _ => None,
    })
    .await;
    assert_eq!(doc.line_ending, LineEnding::Crlf);
    assert!(doc.has_trailing_newline);

    doc.set_choice(0, ConflictChoice::Ours);
    let _ = handle.dispatch(TaskRequest::SaveConflictResolution {
        path: "win.txt".into(),
        content: doc.render_resolved().into_bytes(),
        stage: true,
        delete: false,
    });
    recv_matching(&mut rx, |ev| match ev {
        EngineEvent::ExternalFsChanged(_) => Some(()),
        _ => None,
    })
    .await;
    let raw = fs::read(d.join("win.txt")).unwrap();
    assert_eq!(raw, b"header\r\nmain-line\r\nfooter\r\n");
}

#[tokio::test]
async fn binary_conflict_loads_special_conflict_descriptor_with_side_metadata() {
    use jbgit_core::SpecialConflictKind;

    let tmp = init_repo();
    let d = tmp.path();
    let base_bin = [0x89, b'P', b'N', b'G', 0x00, 0x01, 0x02, 0x03];
    let side_bin = [
        0x89, b'P', b'N', b'G', 0x00, 0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF, 0x11,
    ];
    let main_bin = [
        0x89, b'P', b'N', b'G', 0x00, 0x10, 0x20, 0x30, 0x40, 0x50, 0x60, 0x70, 0x80, 0x90, 0xA0,
        0xB0,
    ];

    fs::write(d.join("asset.bin"), base_bin).unwrap();
    git(d, &["add", "asset.bin"]);
    git(d, &["commit", "-m", "base bin"]);

    git(d, &["checkout", "-b", "side"]);
    fs::write(d.join("asset.bin"), side_bin).unwrap();
    git(d, &["add", "asset.bin"]);
    git(d, &["commit", "-m", "side bin"]);

    git(d, &["checkout", "main"]);
    fs::write(d.join("asset.bin"), main_bin).unwrap();
    git(d, &["add", "asset.bin"]);
    git(d, &["commit", "-m", "main bin"]);

    let merge = run(
        d,
        GitOp::Merge {
            rev: "side".into(),
            options: Default::default(),
        },
    )
    .await;
    assert_eq!(merge.failure, Some(GitFailure::Conflicts), "{merge:?}");

    let loc = RepoLocation::discover(d).unwrap();
    let (handle, mut rx) = EngineSupervisor::start(loc, false).unwrap();
    let e1 = handle.request_conflict_document("asset.bin".into());
    let info = recv_matching(&mut rx, |ev| match ev {
        EngineEvent::SpecialConflictLoaded { info, epoch } if epoch == e1 => Some(info),
        EngineEvent::ConflictDocumentLoaded { epoch, .. } if epoch == e1 => {
            panic!("binary conflict must not load as a plain text ConflictDocument");
        }
        _ => None,
    })
    .await;
    assert_eq!(info.path, "asset.bin");
    assert_eq!(info.kind, SpecialConflictKind::Binary);
    assert!(!info.is_reversed);
    assert_eq!(
        info.ours.as_ref().and_then(|s| s.size),
        Some(main_bin.len() as u64)
    );
    assert_eq!(
        info.theirs.as_ref().and_then(|s| s.size),
        Some(side_bin.len() as u64)
    );
}

#[tokio::test]
async fn submodule_gitlink_and_directory_conflicts_load_special_descriptors() {
    use jbgit_core::SpecialConflictKind;

    // 1. Submodule gitlink (mode 0o160000) conflict.
    let sub_tmp = init_repo();
    let sd = sub_tmp.path();
    commit_file(sd, "s.txt", "v1\n", "sub v1");
    let sub_c1 = rev(sd, "HEAD");
    commit_file(sd, "s.txt", "v2\n", "sub v2");
    let sub_c2 = rev(sd, "HEAD");
    commit_file(sd, "s.txt", "v3\n", "sub v3");
    let sub_c3 = rev(sd, "HEAD");

    let tmp = init_repo();
    let d = tmp.path();
    commit_file(d, "root.txt", "root\n", "root");
    git(
        d,
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("160000,{sub_c1},deps/sub"),
        ],
    );
    git(d, &["commit", "-m", "add gitlink"]);

    git(d, &["checkout", "-b", "side"]);
    git(
        d,
        &[
            "update-index",
            "--cacheinfo",
            &format!("160000,{sub_c2},deps/sub"),
        ],
    );
    git(d, &["commit", "-m", "side bump gitlink"]);

    git(d, &["checkout", "main"]);
    git(
        d,
        &[
            "update-index",
            "--cacheinfo",
            &format!("160000,{sub_c3},deps/sub"),
        ],
    );
    git(d, &["commit", "-m", "main bump gitlink"]);

    let merge = run(
        d,
        GitOp::Merge {
            rev: "side".into(),
            options: Default::default(),
        },
    )
    .await;
    assert_eq!(merge.failure, Some(GitFailure::Conflicts), "{merge:?}");

    let loc = RepoLocation::discover(d).unwrap();
    let (handle, mut rx) = EngineSupervisor::start(loc, false).unwrap();
    let e1 = handle.request_conflict_document("deps/sub".into());
    let info = recv_matching(&mut rx, |ev| match ev {
        EngineEvent::SpecialConflictLoaded { info, epoch } if epoch == e1 => Some(info),
        EngineEvent::ConflictDocumentLoaded { epoch, .. } if epoch == e1 => {
            panic!("submodule gitlink conflict must not load as text");
        }
        _ => None,
    })
    .await;
    assert_eq!(info.kind, SpecialConflictKind::Submodule);
    assert_eq!(info.ours.as_ref().map(|s| s.oid), Some(sub_c3));
    assert_eq!(info.theirs.as_ref().map(|s| s.oid), Some(sub_c2));
    assert_eq!(info.ours.as_ref().map(|s| s.mode), Some(0o160_000));

    // 2. Directory/file conflict (worktree path is a directory).
    git(d, &["merge", "--abort"]);
    git(d, &["checkout", "-b", "dir_side"]);
    fs::create_dir_all(d.join("slot")).unwrap();
    commit_file(d, "slot/nested.txt", "nested\n", "add dir slot");
    git(d, &["checkout", "main"]);
    commit_file(d, "slot", "file content\n", "add file slot");

    let merge_df = run(
        d,
        GitOp::Merge {
            rev: "dir_side".into(),
            options: Default::default(),
        },
    )
    .await;
    assert_eq!(
        merge_df.failure,
        Some(GitFailure::Conflicts),
        "{merge_df:?}"
    );

    let e2 = handle.request_conflict_document("slot".into());
    let dir_info = recv_matching(&mut rx, |ev| match ev {
        EngineEvent::SpecialConflictLoaded { info, epoch } if epoch == e2 => Some(info),
        EngineEvent::ConflictDocumentLoaded { epoch, .. } if epoch == e2 => {
            panic!("directory/file conflict must not load as text");
        }
        _ => None,
    })
    .await;
    assert_eq!(dir_info.kind, SpecialConflictKind::Directory);
}

#[tokio::test]
async fn three_way_load_classifies_non_conflicting_hunks_and_unresolve_recreates_conflict() {
    use jbgit_core::{ConflictChoice, ThreeWayBlockKind};

    let tmp = init_repo();
    let d = tmp.path();
    commit_file(d, "app.txt", "a\nb\nc\nd\ne\n", "base");

    git(d, &["checkout", "-b", "side"]);
    commit_file(d, "app.txt", "a\nb\nc-theirs\nd\ne-theirs\n", "side edits");

    git(d, &["checkout", "main"]);
    commit_file(d, "app.txt", "a-ours\nb\nc-ours\nd\ne\n", "main edits");

    let merge = run(
        d,
        GitOp::Merge {
            rev: "side".into(),
            options: Default::default(),
        },
    )
    .await;
    assert_eq!(merge.failure, Some(GitFailure::Conflicts), "{merge:?}");

    let loc = RepoLocation::discover(d).unwrap();
    let (handle, mut rx) = EngineSupervisor::start(loc, false).unwrap();
    let e1 = handle.request_conflict_document("app.txt".into());
    let mut doc = recv_matching(&mut rx, |ev| match ev {
        EngineEvent::ConflictDocumentLoaded { doc, epoch } if epoch == e1 => Some(doc),
        _ => None,
    })
    .await;

    assert!(doc.has_base, "must load :1 base ancestor");
    assert_eq!(doc.conflict_count, 3);
    assert_eq!(doc.block(0).unwrap().kind, ThreeWayBlockKind::LeftOnly);
    assert_eq!(doc.block(1).unwrap().kind, ThreeWayBlockKind::Conflict);
    assert_eq!(doc.block(1).unwrap().base_lines, vec!["c"]);
    assert_eq!(doc.block(2).unwrap().kind, ThreeWayBlockKind::RightOnly);

    assert_eq!(doc.apply_non_conflicting(), 2);
    assert_eq!(doc.unresolved_count(), 1);
    doc.set_choice(1, ConflictChoice::Custom(vec!["c-merged".into()]));
    assert_eq!(doc.unresolved_count(), 0);

    let _ = handle.dispatch(TaskRequest::SaveConflictResolution {
        path: "app.txt".into(),
        content: doc.render_resolved().into_bytes(),
        stage: true,
        delete: false,
    });
    recv_matching(&mut rx, |ev| match ev {
        EngineEvent::ExternalFsChanged(_) => Some(()),
        _ => None,
    })
    .await;
    assert_eq!(
        fs::read_to_string(d.join("app.txt")).unwrap(),
        "a-ours\nb\nc-merged\nd\ne-theirs\n"
    );
    let st_resolved = git(d, &["status", "--porcelain"]);
    assert!(st_resolved.contains("M  app.txt"), "{st_resolved}");

    // GitOp::UnresolveConflict (`git checkout -m -- app.txt`) restores conflict state & markers.
    let unresolve = run(
        d,
        GitOp::UnresolveConflict {
            paths: vec!["app.txt".into()],
        },
    )
    .await;
    assert!(unresolve.success, "{unresolve:?}");
    let st_reconflicted = git(d, &["status", "--porcelain"]);
    assert!(
        st_reconflicted.contains("UU app.txt"),
        "expected UU conflict after git checkout -m, got: {st_reconflicted}"
    );
    let disk_after = fs::read_to_string(d.join("app.txt")).unwrap();
    assert!(disk_after.contains("<<<<<<<"), "{disk_after}");
}

#[tokio::test]
async fn pull_options_ff_only_fails_and_rebase_replays_on_diverged_remote() {
    let bare = TempDir::new().unwrap();
    git(bare.path(), &["init", "--bare", "-b", "main"]);
    let tmp = init_repo();
    let d = tmp.path();
    git(
        d,
        &["remote", "add", "origin", bare.path().to_str().unwrap()],
    );
    commit_file(d, "base.txt", "base\n", "base");
    git(d, &["push", "-u", "origin", "main"]);

    // Second clone pushes a diverged commit on origin/main.
    let peer = TempDir::new().unwrap();
    git(peer.path(), &["clone", bare.path().to_str().unwrap(), "."]);
    git(peer.path(), &["config", "user.name", "Peer"]);
    git(peer.path(), &["config", "user.email", "peer@example.com"]);
    commit_file(peer.path(), "remote.txt", "remote\n", "remote change");
    git(peer.path(), &["push", "origin", "main"]);

    // Local repo creates a diverged commit.
    commit_file(d, "local.txt", "local\n", "local change");

    // Pull with --ff-only fails with NotFastForward.
    let ff_only = run(
        d,
        GitOp::Pull {
            options: PullOptions {
                remote: Some("origin".into()),
                branch: Some("main".into()),
                ff: FastForward::FfOnly,
                ..Default::default()
            },
        },
    )
    .await;
    assert!(!ff_only.success);
    assert_eq!(
        ff_only.failure,
        Some(GitFailure::NotFastForward),
        "{ff_only:?}"
    );

    // Pull with --rebase replays local commit linearly on top of remote change.
    let rebased = run(
        d,
        GitOp::Pull {
            options: PullOptions {
                remote: Some("origin".into()),
                branch: Some("main".into()),
                rebase: true,
                ..Default::default()
            },
        },
    )
    .await;
    assert!(rebased.success, "pull --rebase failed: {rebased:?}");
    assert_eq!(subjects(d), vec!["local change", "remote change", "base"]);
    assert!(d.join("remote.txt").exists() && d.join("local.txt").exists());
}

#[tokio::test]
async fn rebase_onto_upstream_with_update_refs_moves_stacked_branches() {
    let tmp = init_repo();
    let d = tmp.path();
    commit_file(d, "base.txt", "v1\n", "base");
    let old_base = rev(d, "HEAD");

    // Stacked branches: base -> stack-a (commit a) -> stack-b (commit b).
    git(d, &["checkout", "-b", "stack-a"]);
    commit_file(d, "a.txt", "a\n", "commit a");
    let old_stack_a = rev(d, "stack-a");
    git(d, &["checkout", "-b", "stack-b"]);
    commit_file(d, "b.txt", "b\n", "commit b");

    // Advance main independently.
    git(d, &["checkout", "main"]);
    commit_file(d, "main.txt", "m\n", "main advance");
    let new_main = rev(d, "main");
    git(d, &["checkout", "stack-b"]);

    // Rebase stack-b with --onto main <old_base> --update-refs.
    let res = run(
        d,
        GitOp::Rebase {
            options: RebaseOptions {
                onto: "main".into(),
                upstream: Some(old_base.to_string()),
                update_refs: true,
                ..Default::default()
            },
        },
    )
    .await;
    assert!(res.success, "rebase --onto --update-refs failed: {res:?}");
    assert_eq!(
        subjects(d),
        vec!["commit b", "commit a", "main advance", "base"]
    );
    // Stacked intermediate branch `stack-a` was automatically moved to the rewritten `commit a`!
    let new_stack_a = rev(d, "stack-a");
    assert_ne!(new_stack_a, old_stack_a);
    assert_eq!(rev(d, "stack-a~1"), new_main);
    assert_eq!(rev(d, "HEAD~1"), new_stack_a);
}

#[tokio::test]
async fn interactive_rebase_edit_pauses_amends_and_continues_preserving_subsequent_reword() {
    let tmp = init_repo();
    let d = tmp.path();
    commit_file(d, "f1.txt", "1\n", "c1");
    let base = rev(d, "HEAD");
    commit_file(d, "f2.txt", "2\n", "c2");
    let c2 = rev(d, "HEAD");
    commit_file(d, "f3.txt", "3\n", "c3");
    let c3 = rev(d, "HEAD");

    let todo = vec![
        TodoEntry {
            action: TodoAction::Edit,
            oid: c2,
            subject: "c2".into(),
            message: None,
        },
        TodoEntry {
            action: TodoAction::Reword,
            oid: c3,
            subject: "c3".into(),
            message: Some("c3 reworded after edit".into()),
        },
    ];
    let paused = run(
        d,
        GitOp::RewriteHistory {
            base: Some(base),
            todo,
            no_verify: false,
        },
    )
    .await;
    assert!(paused.success, "edit pause should exit 0: {paused:?}");
    assert!(paused.paused_for_edit, "should signal paused_for_edit");

    let snap = snapshot(d).await;
    assert!(
        matches!(snap.state, RepositoryState::RebaseInteractive { .. }),
        "expected RebaseInteractive, got {:?}",
        snap.state
    );
    assert_eq!(
        OngoingKind::from_state(&snap.state),
        Some(OngoingKind::Rebase)
    );
    // Message files for the following Reword step must still exist while paused.
    assert!(d.join(".git").join("jbgit-rebase").exists());

    // Amend c2 with extra content.
    fs::write(d.join("f2.txt"), "2-amended\n").unwrap();
    git(d, &["add", "f2.txt"]);
    git(d, &["commit", "--amend", "--no-edit"]);

    let cont = run(
        d,
        GitOp::Ongoing {
            kind: OngoingKind::Rebase,
            step: OngoingStep::Continue,
        },
    )
    .await;
    assert!(cont.success, "rebase continue failed: {cont:?}");
    assert!(!cont.paused_for_edit);
    assert_eq!(snapshot(d).await.state, RepositoryState::Clean);
    assert!(!d.join(".git").join("jbgit-rebase").exists());
    assert_eq!(subjects(d), vec!["c3 reworded after edit", "c2", "c1"]);
    assert_eq!(fs::read_to_string(d.join("f2.txt")).unwrap(), "2-amended\n");
}

#[tokio::test]
async fn fixup_and_squash_commits_fold_via_autosquash_rebase_and_non_contiguous_squash() {
    let tmp = init_repo();
    let d = tmp.path();
    commit_file(d, "a.txt", "a1\n", "c1");
    let c1 = rev(d, "HEAD");
    commit_file(d, "b.txt", "b1\n", "c2");
    let c2 = rev(d, "HEAD");
    commit_file(d, "c.txt", "c1\n", "c3");

    // 1. Standalone Fixup! commit (no immediate rebase).
    fs::write(d.join("b.txt"), "b2\n").unwrap();
    git(d, &["add", "b.txt"]);
    let fix = run(
        d,
        GitOp::FixupCommit {
            target: c2,
            kind: FixupKind::Fixup,
            base: Some(c1),
            autosquash: false,
        },
    )
    .await;
    assert!(fix.success, "{fix:?}");
    assert_eq!(subjects(d)[0], "fixup! c2");

    // 2. Standalone Squash! commit (no immediate rebase).
    fs::write(d.join("c.txt"), "c2\n").unwrap();
    git(d, &["add", "c.txt"]);
    let sq = run(
        d,
        GitOp::FixupCommit {
            target: rev(d, "HEAD~1"),
            kind: FixupKind::Squash,
            base: Some(c1),
            autosquash: false,
        },
    )
    .await;
    assert!(sq.success, "{sq:?}");
    assert_eq!(subjects(d)[0], "squash! c3");

    // 3. RebaseOptions { autosquash: true } folds both fixup! and squash! automatically.
    let auto = run(
        d,
        GitOp::Rebase {
            options: RebaseOptions {
                onto: c1.to_string(),
                autosquash: true,
                ..Default::default()
            },
        },
    )
    .await;
    assert!(auto.success, "autosquash rebase failed: {auto:?}");
    assert_eq!(subjects(d), vec!["c3", "c2", "c1"]);
    assert_eq!(fs::read_to_string(d.join("b.txt")).unwrap(), "b2\n");
    assert_eq!(fs::read_to_string(d.join("c.txt")).unwrap(), "c2\n");

    // 4. Non-contiguous squash: add c4, then squash c2 + c4 (skipping c3).
    commit_file(d, "d.txt", "d1\n", "c4");
    let snap = snapshot(d).await;
    let oid_c2 = rev(d, "HEAD~2");
    let oid_c4 = rev(d, "HEAD");
    let plan = snap.plan_rewrite_typed(oid_c2).unwrap();
    let mut sq_plan = plan_squash_todo(&plan, &[oid_c2, oid_c4]).unwrap();
    assert!(sq_plan.reordered);
    sq_plan.todo[sq_plan.message_entry_idx].message = Some("c2+c4 squashed".into());
    let rewrite = run(
        d,
        GitOp::RewriteHistory {
            base: sq_plan.base,
            todo: sq_plan.todo,
            no_verify: false,
        },
    )
    .await;
    assert!(rewrite.success, "non-contiguous squash failed: {rewrite:?}");
    assert_eq!(subjects(d), vec!["c3", "c2+c4 squashed", "c1"]);
    assert!(d.join("b.txt").exists() && d.join("c.txt").exists() && d.join("d.txt").exists());
}

#[tokio::test]
async fn set_unset_upstream_and_cleanup_merged_and_gone_branches() {
    let bare = TempDir::new().unwrap();
    git(bare.path(), &["init", "--bare"]);
    let tmp = init_repo();
    let d = tmp.path();
    commit_file(d, "base.txt", "base\n", "base");
    git(
        d,
        &["remote", "add", "origin", bare.path().to_str().unwrap()],
    );
    git(d, &["push", "-u", "origin", "main"]);

    // Create two feature branches, push them, merge one into main, delete remote of both.
    git(d, &["checkout", "-b", "feat/merged"]);
    commit_file(d, "m.txt", "merged\n", "merged work");
    git(d, &["push", "-u", "origin", "feat/merged"]);
    let merged_tip = rev(d, "HEAD");

    git(d, &["checkout", "main"]);
    git(
        d,
        &["merge", "--no-ff", "-m", "merge feat/merged", "feat/merged"],
    );

    git(d, &["checkout", "-b", "feat/unmerged-gone"]);
    commit_file(d, "u.txt", "unmerged\n", "unmerged work");
    git(d, &["push", "-u", "origin", "feat/unmerged-gone"]);

    git(d, &["checkout", "main"]);
    git(d, &["push", "origin", "--delete", "feat/merged"]);
    git(d, &["push", "origin", "--delete", "feat/unmerged-gone"]);
    git(d, &["fetch", "--prune", "origin"]);

    // Test SetUpstream & UnsetUpstream on a local branch.
    git(d, &["branch", "local-track", "main"]);
    let set_up = run(
        d,
        GitOp::SetUpstream {
            branch: "local-track".into(),
            upstream: "origin/main".into(),
        },
    )
    .await;
    assert!(set_up.success, "{set_up:?}");
    assert_eq!(
        snapshot(d)
            .await
            .local_branch("local-track")
            .unwrap()
            .upstream
            .as_deref(),
        Some("origin/main")
    );
    let unset_up = run(
        d,
        GitOp::UnsetUpstream {
            branch: "local-track".into(),
        },
    )
    .await;
    assert!(unset_up.success, "{unset_up:?}");
    assert!(
        snapshot(d)
            .await
            .local_branch("local-track")
            .unwrap()
            .upstream
            .is_none()
    );

    // Cleanup scan identifies merged (`feat/merged` + `local-track`) and `[gone]` (`feat/merged` + `feat/unmerged-gone`).
    let snap = snapshot(d).await;
    let cands = snap.scan_cleanup_branches("HEAD", &[]);
    let merged_c = cands
        .iter()
        .find(|c| c.name == "feat/merged")
        .expect("feat/merged");
    assert!(merged_c.merged_into_base && merged_c.upstream_gone);
    let gone_c = cands
        .iter()
        .find(|c| c.name == "feat/unmerged-gone")
        .expect("feat/unmerged-gone");
    assert!(!gone_c.merged_into_base && gone_c.upstream_gone);
    assert!(cands.iter().all(|c| c.name != "main"));

    // Batch Sequence delete + Undo Restore.
    let del = run(
        d,
        GitOp::Sequence(vec![GitOp::DeleteBranch {
            name: "feat/merged".into(),
            force: false,
            tip: merged_tip,
        }]),
    )
    .await;
    assert!(del.success, "{del:?}");
    assert!(snapshot(d).await.local_branch("feat/merged").is_none());

    let restore = run(
        d,
        GitOp::Sequence(vec![GitOp::RestoreBranch {
            name: "feat/merged".into(),
            oid: merged_tip,
        }]),
    )
    .await;
    assert!(restore.success, "{restore:?}");
    assert_eq!(
        snapshot(d)
            .await
            .local_branch("feat/merged")
            .unwrap()
            .target_oid,
        merged_tip
    );
}

#[tokio::test]
async fn cherry_pick_no_commit_revert_no_commit_and_empty_cherry_pick_recovery() {
    let tmp = init_repo();
    let d = tmp.path();
    commit_file(d, "a.txt", "v1\n", "init");
    git(d, &["checkout", "-b", "side"]);
    commit_file(d, "b.txt", "side-b\n", "add b");
    let add_b = rev(d, "HEAD");
    git(d, &["checkout", "main"]);
    let head_before = rev(d, "HEAD");

    // 1. Cherry-pick with `-n` applies changes without creating a new commit.
    let cp_n = run(
        d,
        GitOp::CherryPick {
            oids: vec![add_b],
            no_commit: true,
            record_origin: false,
            mainline: None,
        },
    )
    .await;
    assert!(cp_n.success, "{cp_n:?}");
    assert_eq!(
        rev(d, "HEAD"),
        head_before,
        "cherry-pick -n must not move HEAD"
    );
    assert_eq!(fs::read_to_string(d.join("b.txt")).unwrap(), "side-b\n");
    let st = git(d, &["status", "--porcelain"]);
    assert!(st.contains("A  b.txt"), "{st}");

    // Commit it, then cherry-pick the SAME commit again -> EmptyCherryPick!
    git(d, &["commit", "-m", "manual b"]);
    let cp_empty = run(
        d,
        GitOp::CherryPick {
            oids: vec![add_b],
            no_commit: false,
            record_origin: false,
            mainline: None,
        },
    )
    .await;
    assert!(!cp_empty.success);
    assert_eq!(
        cp_empty.failure,
        Some(GitFailure::EmptyCherryPick),
        "{cp_empty:?}"
    );

    // Recover with AllowEmpty -> creates empty commit preserving subject "add b".
    let allow = run(d, GitOp::CherryPickAllowEmpty).await;
    assert!(allow.success, "{allow:?}");
    assert_eq!(subjects(d)[0], "add b");

    // 2. Revert `-n` on a normal commit stages the inverse without creating a commit.
    let before_rev = rev(d, "HEAD");
    let prev_commit = rev(d, "HEAD~1");
    let rv_n = run(
        d,
        GitOp::Revert {
            oids: vec![prev_commit],
            no_commit: true,
            mainline: None,
        },
    )
    .await;
    assert!(rv_n.success, "{rv_n:?}");
    assert_eq!(rev(d, "HEAD"), before_rev, "revert -n must not move HEAD");
    assert!(!d.join("b.txt").exists());
}

#[tokio::test]
async fn stash_push_keep_index_apply_reinstate_index_and_checkout_single_file() {
    let tmp = init_repo();
    let d = tmp.path();
    commit_file(d, "staged.txt", "base-s\n", "base s");
    commit_file(d, "work.txt", "base-w\n", "base w");

    // Stage change to staged.txt, leave unstaged change in work.txt.
    fs::write(d.join("staged.txt"), "staged-v2\n").unwrap();
    git(d, &["add", "staged.txt"]);
    fs::write(d.join("work.txt"), "work-v2\n").unwrap();

    // `git stash push --keep-index` keeps staged.txt staged while cleaning work.txt!
    let push = run(
        d,
        GitOp::StashPush {
            message: Some("keep-idx".into()),
            include_untracked: false,
            keep_index: true,
            paths: Vec::new(),
        },
    )
    .await;
    assert!(push.success, "{push:?}");
    let st_after_push = git(d, &["status", "--porcelain"]);
    assert!(st_after_push.contains("M  staged.txt"), "{st_after_push}");
    assert!(!st_after_push.contains("work.txt"), "{st_after_push}");
    assert_eq!(fs::read_to_string(d.join("work.txt")).unwrap(), "base-w\n");

    // Reset clean, then apply stash with `--index` -> staged.txt is reinstated in index!
    git(d, &["reset", "--hard", "HEAD"]);
    let apply_idx = run(
        d,
        GitOp::StashApply {
            index: 0,
            pop: false,
            reinstate_index: true,
        },
    )
    .await;
    assert!(apply_idx.success, "{apply_idx:?}");
    let st_after_apply = git(d, &["status", "--porcelain"]);
    assert!(st_after_apply.contains("M  staged.txt"), "{st_after_apply}");
    assert!(st_after_apply.contains(" M work.txt"), "{st_after_apply}");

    // Reset clean, then Apply Single File (`git checkout stash@{0} -- work.txt`) touches ONLY work.txt.
    git(d, &["reset", "--hard", "HEAD"]);
    let single = run(
        d,
        GitOp::CheckoutPaths {
            rev: "stash@{0}".into(),
            paths: vec!["work.txt".into()],
        },
    )
    .await;
    assert!(single.success, "{single:?}");
    assert_eq!(fs::read_to_string(d.join("work.txt")).unwrap(), "work-v2\n");
    assert_eq!(
        fs::read_to_string(d.join("staged.txt")).unwrap(),
        "base-s\n"
    );
}

#[tokio::test]
async fn unresolve_conflict_restores_ud_du_aa_and_uu_after_accept_side() {
    let tmp = init_repo();
    let d = tmp.path();

    // Base commit with files that will trigger UD, DU, and UU conflicts.
    fs::write(d.join("deleted_by_theirs.txt"), "base-ud\n").unwrap();
    fs::write(d.join("deleted_by_ours.txt"), "base-du\n").unwrap();
    fs::write(d.join("both_modified.txt"), "base-uu\n").unwrap();
    git(d, &["add", "."]);
    git(d, &["commit", "-m", "base"]);

    // Feature branch (theirs): deletes deleted_by_theirs, modifies deleted_by_ours & both_modified, adds both_added.
    git(d, &["checkout", "-b", "feature"]);
    fs::remove_file(d.join("deleted_by_theirs.txt")).unwrap();
    fs::write(d.join("deleted_by_ours.txt"), "theirs-du\n").unwrap();
    fs::write(d.join("both_modified.txt"), "theirs-uu\n").unwrap();
    fs::write(d.join("both_added.txt"), "theirs-aa\n").unwrap();
    git(d, &["add", "-A"]);
    git(d, &["commit", "-m", "feature"]);

    // Main branch (ours): modifies deleted_by_theirs & both_modified, deletes deleted_by_ours, adds both_added.
    git(d, &["checkout", "main"]);
    fs::write(d.join("deleted_by_theirs.txt"), "ours-ud\n").unwrap();
    fs::remove_file(d.join("deleted_by_ours.txt")).unwrap();
    fs::write(d.join("both_modified.txt"), "ours-uu\n").unwrap();
    fs::write(d.join("both_added.txt"), "ours-aa\n").unwrap();
    git(d, &["add", "-A"]);
    git(d, &["commit", "-m", "main"]);

    // Trigger merge conflicts.
    let merge = run(
        d,
        GitOp::Merge {
            rev: "feature".into(),
            options: Default::default(),
        },
    )
    .await;
    assert!(!merge.success);
    let st_init = git(d, &["status", "--porcelain"]);
    assert!(st_init.contains("UD deleted_by_theirs.txt"), "{st_init}");
    assert!(st_init.contains("DU deleted_by_ours.txt"), "{st_init}");
    assert!(st_init.contains("AA both_added.txt"), "{st_init}");
    assert!(st_init.contains("UU both_modified.txt"), "{st_init}");

    // 1a. UD resolved via AcceptSide(ours=true) -> UnresolveConflict restores UD & ours worktree content.
    let accept_ud_ours = run(
        d,
        GitOp::AcceptSide {
            paths: vec!["deleted_by_theirs.txt".into()],
            ours: true,
        },
    )
    .await;
    assert!(accept_ud_ours.success, "{accept_ud_ours:?}");
    let unresolve_ud_1 = run(
        d,
        GitOp::UnresolveConflict {
            paths: vec!["deleted_by_theirs.txt".into()],
        },
    )
    .await;
    assert!(unresolve_ud_1.success, "{unresolve_ud_1:?}");
    let st_ud_1 = git(d, &["status", "--porcelain", "--", "deleted_by_theirs.txt"]);
    assert!(
        st_ud_1.contains("UD deleted_by_theirs.txt"),
        "expected UD after unresolve (from ours=true), got: {st_ud_1}"
    );
    assert_eq!(
        fs::read_to_string(d.join("deleted_by_theirs.txt")).unwrap(),
        "ours-ud\n"
    );

    // 1b. UD resolved via AcceptSide(ours=false) (`git rm -rf`) -> UnresolveConflict restores UD & worktree file.
    let accept_ud_theirs = run(
        d,
        GitOp::AcceptSide {
            paths: vec!["deleted_by_theirs.txt".into()],
            ours: false,
        },
    )
    .await;
    assert!(accept_ud_theirs.success, "{accept_ud_theirs:?}");
    assert!(
        !d.join("deleted_by_theirs.txt").exists(),
        "AcceptSide(ours=false) on UD must remove worktree file"
    );
    let unresolve_ud_2 = run(
        d,
        GitOp::UnresolveConflict {
            paths: vec!["deleted_by_theirs.txt".into()],
        },
    )
    .await;
    assert!(unresolve_ud_2.success, "{unresolve_ud_2:?}");
    let st_ud_2 = git(d, &["status", "--porcelain", "--", "deleted_by_theirs.txt"]);
    assert!(
        st_ud_2.contains("UD deleted_by_theirs.txt"),
        "expected UD after unresolve (from ours=false / git rm), got: {st_ud_2}"
    );
    assert_eq!(
        fs::read_to_string(d.join("deleted_by_theirs.txt")).unwrap(),
        "ours-ud\n"
    );

    // 2. DU resolved via AcceptSide(ours=true) (`git rm -rf`) and AcceptSide(ours=false) -> restores DU & theirs content.
    for ours in [true, false] {
        let accept = run(
            d,
            GitOp::AcceptSide {
                paths: vec!["deleted_by_ours.txt".into()],
                ours,
            },
        )
        .await;
        assert!(accept.success, "DU accept ours={ours}: {accept:?}");
        let unresolve = run(
            d,
            GitOp::UnresolveConflict {
                paths: vec!["deleted_by_ours.txt".into()],
            },
        )
        .await;
        assert!(unresolve.success, "DU unresolve ours={ours}: {unresolve:?}");
        let st_du = git(d, &["status", "--porcelain", "--", "deleted_by_ours.txt"]);
        assert!(
            st_du.contains("DU deleted_by_ours.txt"),
            "expected DU after unresolve (ours={ours}), got: {st_du}"
        );
        assert_eq!(
            fs::read_to_string(d.join("deleted_by_ours.txt")).unwrap(),
            "theirs-du\n"
        );
    }

    // 3. AA resolved via AcceptSide -> UnresolveConflict restores AA and conflict markers.
    let accept_aa = run(
        d,
        GitOp::AcceptSide {
            paths: vec!["both_added.txt".into()],
            ours: true,
        },
    )
    .await;
    assert!(accept_aa.success, "{accept_aa:?}");
    let unresolve_aa = run(
        d,
        GitOp::UnresolveConflict {
            paths: vec!["both_added.txt".into()],
        },
    )
    .await;
    assert!(unresolve_aa.success, "{unresolve_aa:?}");
    let st_aa = git(d, &["status", "--porcelain", "--", "both_added.txt"]);
    assert!(st_aa.contains("AA both_added.txt"), "{st_aa}");
    let aa_disk = fs::read_to_string(d.join("both_added.txt")).unwrap();
    assert!(aa_disk.contains("<<<<<<<"), "{aa_disk}");

    // 4. UU non-regression: resolved via AcceptSide -> UnresolveConflict restores UU and conflict markers.
    let accept_uu = run(
        d,
        GitOp::AcceptSide {
            paths: vec!["both_modified.txt".into()],
            ours: true,
        },
    )
    .await;
    assert!(accept_uu.success, "{accept_uu:?}");
    let unresolve_uu = run(
        d,
        GitOp::UnresolveConflict {
            paths: vec!["both_modified.txt".into()],
        },
    )
    .await;
    assert!(unresolve_uu.success, "{unresolve_uu:?}");
    let st_uu = git(d, &["status", "--porcelain", "--", "both_modified.txt"]);
    assert!(st_uu.contains("UU both_modified.txt"), "{st_uu}");
    let uu_disk = fs::read_to_string(d.join("both_modified.txt")).unwrap();
    assert!(uu_disk.contains("<<<<<<<"), "{uu_disk}");
}

#[tokio::test]
async fn unresolve_conflict_restores_ud_during_cherry_pick() {
    let tmp = init_repo();
    let d = tmp.path();

    commit_file(d, "victim.txt", "base\n", "base");
    git(d, &["checkout", "-b", "drop-branch"]);
    fs::remove_file(d.join("victim.txt")).unwrap();
    git(d, &["add", "-A"]);
    git(d, &["commit", "-m", "delete victim"]);
    let drop_oid: Oid = git(d, &["rev-parse", "HEAD"]).trim().parse().unwrap();

    git(d, &["checkout", "main"]);
    commit_file(d, "victim.txt", "modified-on-main\n", "modify victim");

    let cp = run(
        d,
        GitOp::CherryPick {
            oids: vec![drop_oid],
            no_commit: false,
            record_origin: false,
            mainline: None,
        },
    )
    .await;
    assert!(!cp.success);
    assert!(git(d, &["status", "--porcelain"]).contains("UD victim.txt"));

    // Resolve by accepting deletion (`ours=false`), then unresolve back to UD.
    let accept = run(
        d,
        GitOp::AcceptSide {
            paths: vec!["victim.txt".into()],
            ours: false,
        },
    )
    .await;
    assert!(accept.success, "{accept:?}");
    assert!(!d.join("victim.txt").exists());

    let unresolve = run(
        d,
        GitOp::UnresolveConflict {
            paths: vec!["victim.txt".into()],
        },
    )
    .await;
    assert!(unresolve.success, "{unresolve:?}");
    let st = git(d, &["status", "--porcelain", "--", "victim.txt"]);
    assert!(st.contains("UD victim.txt"), "got: {st}");
    assert_eq!(
        fs::read_to_string(d.join("victim.txt")).unwrap(),
        "modified-on-main\n"
    );
}
