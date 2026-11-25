use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use jbgit_core::{
    BlameKey, BlameOptions, ConflictSidePresence, DetailsKey, DiffContext, FileDiffKey,
    FileHistoryKey, FileModeChange, FileStatusKind, GIT_MODE_EXECUTABLE, GIT_MODE_REGULAR,
    GIT_MODE_SYMLINK, HistoryBranchScope, Oid, ReflogKey, RequestEpoch, RequestSlotId,
    WhitespaceMode,
};
use jbgit_engine::{
    EngineBackend, EngineEvent, EngineSupervisor, RepoLocation, SnapshotLoadMode, StatusScanConfig,
    load_repo_snapshot_incremental_sync,
};
use tempfile::TempDir;
use tokio::sync::mpsc::UnboundedReceiver;

fn git_raw(dir: &Path, args: &[&str]) -> std::process::Output {
    Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "Test User")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "Test User")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .output()
        .expect("failed to run git")
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = git_raw(dir, args);
    assert!(
        out.status.success(),
        "git {args:?} failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn git_with_stdin(dir: &Path, args: &[&str], stdin_data: &str) -> String {
    let mut child = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "Test User")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "Test User")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn git");
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(stdin_data.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn init_repo(dir: &Path) {
    git(dir, &["init", "-b", "main"]);
    git(dir, &["config", "user.name", "Test User"]);
    git(dir, &["config", "user.email", "test@example.com"]);
    git(dir, &["config", "commit.gpgsign", "false"]);
    git(dir, &["config", "core.autocrlf", "false"]);
}

fn write_file(dir: &Path, rel: &str, content: &str) {
    let full = dir.join(rel);
    if let Some(parent) = full.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(full, content).unwrap();
}

fn commit_all(dir: &Path, msg: &str) -> Oid {
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-m", msg]);
    git(dir, &["rev-parse", "HEAD"]).trim().parse().unwrap()
}

async fn recv_until<T>(
    rx: &mut UnboundedReceiver<EngineEvent>,
    mut predicate: impl FnMut(EngineEvent) -> Option<T>,
) -> T {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let ev = tokio::time::timeout_at(deadline, rx.recv())
            .await
            .expect("timed out waiting for EngineEvent")
            .expect("engine event channel closed");
        if let Some(val) = predicate(ev) {
            return val;
        }
    }
}

#[tokio::test]
async fn remote_tag_check_rejects_option_injection_and_detects_real_tags() {
    let tmp = TempDir::new().unwrap();
    let remote_dir = tmp.path().join("remote.git");
    let local_dir = tmp.path().join("local");
    fs::create_dir_all(&remote_dir).unwrap();
    fs::create_dir_all(&local_dir).unwrap();

    git(&remote_dir, &["init", "--bare", "-b", "main"]);
    init_repo(&local_dir);
    write_file(&local_dir, "a.txt", "hello\n");
    commit_all(&local_dir, "init");
    git(&local_dir, &["tag", "v1.0"]);
    git(
        &local_dir,
        &["remote", "add", "origin", remote_dir.to_str().unwrap()],
    );
    git(&local_dir, &["push", "origin", "main", "--tags"]);

    let loc = RepoLocation::discover(&local_dir).unwrap();
    let (handle, mut rx) = EngineSupervisor::start(loc, false).unwrap();

    // Real existing tag
    let ep1 = handle.request_check_remote_tag("origin".into(), "v1.0".into());
    let exists1 = recv_until(&mut rx, |ev| match ev {
        EngineEvent::RemoteTagChecked { epoch, exists, .. } if epoch == ep1 => Some(exists),
        _ => None,
    })
    .await;
    assert_eq!(exists1, Some(true));

    // Real missing tag
    let ep2 = handle.request_check_remote_tag("origin".into(), "v9.9".into());
    let exists2 = recv_until(&mut rx, |ev| match ev {
        EngineEvent::RemoteTagChecked { epoch, exists, .. } if epoch == ep2 => Some(exists),
        _ => None,
    })
    .await;
    assert_eq!(exists2, Some(false));

    // Option-injection remote name starting with `--` must be treated as a positional remote,
    // never as a git ls-remote option (`--upload-pack=...`).
    let sentinel = tmp.path().join("pwned.txt");
    let evil_remote = format!("--upload-pack=echo pwned > {}", sentinel.display());
    let ep3 = handle.request_check_remote_tag(evil_remote, "v1.0".into());
    let exists3 = recv_until(&mut rx, |ev| match ev {
        EngineEvent::RemoteTagChecked { epoch, exists, .. } if epoch == ep3 => Some(exists),
        _ => None,
    })
    .await;
    assert_eq!(exists3, None);
    assert!(
        !sentinel.exists(),
        "option injection executed upload-pack command!"
    );

    assert!(handle.shutdown(Duration::from_secs(2)).await);
}

#[tokio::test]
async fn selection_history_works_with_all_branches_and_ambiguous_branch_name() {
    let tmp = TempDir::new().unwrap();
    let d = tmp.path();
    git(d, &["init", "-b", "docs"]);
    git(d, &["config", "user.name", "Test User"]);
    git(d, &["config", "user.email", "test@example.com"]);
    git(d, &["config", "commit.gpgsign", "false"]);
    git(d, &["config", "core.autocrlf", "false"]);

    write_file(d, "docs/notes.txt", "line1\nline2\nline3\n");
    let c1 = commit_all(d, "add notes");
    write_file(d, "docs/notes.txt", "line1-v2\nline2\nline3\n");
    let c2 = commit_all(d, "edit line1");

    // Second branch so `--all` would fail `git log -L` if passed.
    git(d, &["checkout", "-b", "feature"]);
    write_file(d, "other.txt", "feature\n");
    commit_all(d, "feature commit");
    git(d, &["checkout", "docs"]);

    let loc = RepoLocation::discover(d).unwrap();
    let (handle, mut rx) = EngineSupervisor::start(loc, false).unwrap();

    // 1. Selection History under AllBranches falls back cleanly instead of passing `--all` to `-L`.
    let mut all_key = FileHistoryKey::new("docs/notes.txt", Some(c2), true, Some((1, 2)));
    all_key.scope = HistoryBranchScope::AllBranches;
    let ep1 = handle.request_file_history(all_key);
    let b1 = recv_until(&mut rx, |ev| match ev {
        EngineEvent::FileHistoryLoaded { bundle, epoch } if epoch == ep1 => Some(bundle),
        EngineEvent::Error(e) => panic!("Selection History AllBranches failed: {e}"),
        _ => None,
    })
    .await;
    assert_eq!(b1.entries.len(), 2);
    assert_eq!(b1.entries[0].oid, c2);
    assert_eq!(b1.entries[1].oid, c1);

    // 2. Selection History scoped to branch `docs` (same name as top-level directory `docs/`)
    // must disambiguate with trailing `--` instead of failing with ambiguous argument.
    let mut branch_key = FileHistoryKey::new("docs/notes.txt", None, true, Some((1, 2)));
    branch_key.scope = HistoryBranchScope::Branch("docs".into());
    let ep2 = handle.request_file_history(branch_key);
    let b2 = recv_until(&mut rx, |ev| match ev {
        EngineEvent::FileHistoryLoaded { bundle, epoch } if epoch == ep2 => Some(bundle),
        EngineEvent::Error(e) => panic!("Selection History Branch(docs) failed: {e}"),
        _ => None,
    })
    .await;
    assert_eq!(b2.entries.len(), 2);
    assert_eq!(b2.entries[0].oid, c2);
    assert_eq!(b2.entries[1].oid, c1);

    assert!(handle.shutdown(Duration::from_secs(2)).await);
}

#[test]
fn unborn_orphan_head_preserves_commits_from_existing_branches() {
    let tmp = TempDir::new().unwrap();
    let d = tmp.path();
    init_repo(d);
    write_file(d, "a.txt", "one\n");
    let c1 = commit_all(d, "first");
    write_file(d, "a.txt", "two\n");
    let c2 = commit_all(d, "second");

    // Switch to an orphan branch (unborn HEAD) while `main` still holds commits.
    git(d, &["checkout", "--orphan", "wip"]);

    let loc = RepoLocation::discover(d).unwrap();
    let token = RequestEpoch::new().issue();
    let (snap, _, _, _) = load_repo_snapshot_incremental_sync(
        &loc,
        &token,
        500,
        StatusScanConfig::default(),
        None,
        SnapshotLoadMode::FullNoReuse,
    )
    .expect("snapshot load");

    assert!(snap.is_unborn_head);
    assert_eq!(snap.head_branch.as_deref(), Some("wip"));
    assert_eq!(snap.head_oid, None);
    let oids: Vec<Oid> = snap.commits.iter().map(|c| c.oid).collect();
    assert_eq!(oids, vec![c2, c1]);
    assert!(snap.refs.iter().any(|r| r.name == "main"));
}

#[test]
fn linked_worktree_of_shallow_clone_reports_is_shallow_and_fingerprint() {
    let tmp = TempDir::new().unwrap();
    let src = tmp.path().join("src");
    let shallow_clone = tmp.path().join("shallow");
    let linked_wt = tmp.path().join("linked-wt");
    fs::create_dir_all(&src).unwrap();

    init_repo(&src);
    write_file(&src, "f.txt", "1\n");
    commit_all(&src, "c1");
    write_file(&src, "f.txt", "2\n");
    commit_all(&src, "c2");
    write_file(&src, "f.txt", "3\n");
    commit_all(&src, "c3");

    let file_url = format!("file:///{}", src.to_string_lossy().replace('\\', "/"));
    git(
        tmp.path(),
        &[
            "clone",
            "--depth",
            "1",
            &file_url,
            shallow_clone.to_str().unwrap(),
        ],
    );
    git(
        &shallow_clone,
        &[
            "worktree",
            "add",
            "-b",
            "wt-branch",
            linked_wt.to_str().unwrap(),
        ],
    );

    let loc = RepoLocation::discover(&linked_wt).unwrap();
    let token = RequestEpoch::new().issue();
    let (snap, _, cache, _) = load_repo_snapshot_incremental_sync(
        &loc,
        &token,
        500,
        StatusScanConfig::default(),
        None,
        SnapshotLoadMode::FullNoReuse,
    )
    .expect("snapshot load in linked worktree");

    assert!(
        snap.is_shallow,
        "linked worktree of a shallow clone must report is_shallow == true"
    );
    assert!(
        cache.shallow_fingerprint.is_some(),
        "linked worktree of a shallow clone must retain shallow_fingerprint from common_dir"
    );
}

#[tokio::test]
async fn rebase_delete_modify_conflict_labels_match_reverse_conflict_state() {
    let tmp = TempDir::new().unwrap();
    let d = tmp.path();
    init_repo(d);

    write_file(d, "del_by_onto.txt", "base\n");
    write_file(d, "del_by_topic.txt", "base\n");
    commit_all(d, "base");

    git(d, &["checkout", "-b", "topic"]);
    write_file(d, "del_by_onto.txt", "modified on topic\n");
    git(d, &["rm", "del_by_topic.txt"]);
    commit_all(d, "topic edits");

    git(d, &["checkout", "main"]);
    git(d, &["rm", "del_by_onto.txt"]);
    write_file(d, "del_by_topic.txt", "modified on main\n");
    commit_all(d, "main edits");

    git(d, &["checkout", "topic"]);
    let rebase_out = git_raw(d, &["rebase", "main"]);
    assert!(!rebase_out.status.success(), "expected rebase conflict");

    let loc = RepoLocation::discover(d).unwrap();
    let (handle, mut rx) = EngineSupervisor::start(loc, false).unwrap();

    let ep = handle.request_stage_status(false);
    let status = recv_until(&mut rx, |ev| match ev {
        EngineEvent::StageStatusLoaded { status, epoch } if epoch == ep => Some(status),
        _ => None,
    })
    .await;

    // During rebase, `topic` (replayed) is "Us" and `main` (onto) is "Them":
    // - `del_by_onto.txt` was deleted by `main` ("Them") and modified by `topic` ("Us")
    // - `del_by_topic.txt` was deleted by `topic` ("Us") and modified by `main` ("Them")
    assert_eq!(
        status
            .conflict_labels
            .get("del_by_onto.txt")
            .map(String::as_str),
        Some("Deleted by Them")
    );
    assert_eq!(
        status
            .conflict_labels
            .get("del_by_topic.txt")
            .map(String::as_str),
        Some("Deleted by Us")
    );

    // Cross-check agreement with the Merge Conflict Resolver document orientation.
    let ep_doc1 = handle.request_conflict_document("del_by_onto.txt".into());
    let doc1 = recv_until(&mut rx, |ev| match ev {
        EngineEvent::ConflictDocumentLoaded { doc, epoch } if epoch == ep_doc1 => Some(doc),
        _ => None,
    })
    .await;
    assert_eq!(doc1.side_presence, ConflictSidePresence::DeletedByTheirs);

    let ep_doc2 = handle.request_conflict_document("del_by_topic.txt".into());
    let doc2 = recv_until(&mut rx, |ev| match ev {
        EngineEvent::ConflictDocumentLoaded { doc, epoch } if epoch == ep_doc2 => Some(doc),
        _ => None,
    })
    .await;
    assert_eq!(doc2.side_presence, ConflictSidePresence::DeletedByOurs);

    assert!(handle.shutdown(Duration::from_secs(2)).await);
}

#[tokio::test]
async fn conflicted_submodule_is_not_duplicated_into_unstaged() {
    let tmp = TempDir::new().unwrap();
    let sub_src = tmp.path().join("sub-src");
    let super_dir = tmp.path().join("super");
    fs::create_dir_all(&sub_src).unwrap();
    fs::create_dir_all(&super_dir).unwrap();

    init_repo(&sub_src);
    write_file(&sub_src, "s.txt", "s0\n");
    commit_all(&sub_src, "s0");
    write_file(&sub_src, "s.txt", "s1\n");
    let s1 = commit_all(&sub_src, "s1");
    git(&sub_src, &["checkout", "-b", "alt", "HEAD~1"]);
    write_file(&sub_src, "s.txt", "s2\n");
    let s2 = commit_all(&sub_src, "s2");
    git(&sub_src, &["checkout", "main"]);

    init_repo(&super_dir);
    git(
        &super_dir,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            sub_src.to_str().unwrap(),
            "libs/sub",
        ],
    );
    git(&super_dir.join("libs/sub"), &["checkout", "HEAD~1"]);
    commit_all(&super_dir, "add submodule at s0");

    git(&super_dir, &["checkout", "-b", "feature"]);
    git(&super_dir.join("libs/sub"), &["fetch", "origin"]);
    git(&super_dir.join("libs/sub"), &["checkout", &s2.to_string()]);
    commit_all(&super_dir, "feature updates submodule to s2");

    git(&super_dir, &["checkout", "main"]);
    git(&super_dir.join("libs/sub"), &["checkout", &s1.to_string()]);
    commit_all(&super_dir, "main updates submodule to s1");

    let merge_out = git_raw(&super_dir, &["merge", "feature"]);
    assert!(!merge_out.status.success(), "expected submodule conflict");
    // Dirty the submodule worktree so `SubmoduleState::dirty_workdir` is true too.
    write_file(&super_dir, "libs/sub/dirty.txt", "dirty\n");

    let loc = RepoLocation::discover(&super_dir).unwrap();
    let (handle, mut rx) = EngineSupervisor::start(loc, false).unwrap();

    let ep = handle.request_stage_status(false);
    let status = recv_until(&mut rx, |ev| match ev {
        EngineEvent::StageStatusLoaded { status, epoch } if epoch == ep => Some(status),
        _ => None,
    })
    .await;

    assert!(
        status.conflicted.iter().any(|e| e.path == "libs/sub"),
        "expected libs/sub in conflicted: {:?}",
        status.conflicted
    );
    assert!(
        !status.unstaged.iter().any(|e| e.path == "libs/sub"),
        "conflicted submodule libs/sub must not be duplicated in unstaged: {:?}",
        status.unstaged
    );

    assert!(handle.shutdown(Duration::from_secs(2)).await);
}

#[tokio::test]
async fn multi_commit_net_changes_preserve_displaced_delete_rename_and_path_swaps() {
    let tmp = TempDir::new().unwrap();
    let d = tmp.path();
    init_repo(d);

    let alpha = "alpha line 1\nalpha line 2\nalpha line 3\nalpha line 4\n";
    let beta = "beta line 1\nbeta line 2\nbeta line 3\nbeta line 4\n";
    write_file(d, "a.txt", alpha);
    write_file(d, "b.txt", beta);
    commit_all(d, "base");

    // 1. Delete `b.txt` in c1, then rename `a.txt -> b.txt` in c2.
    git(d, &["rm", "b.txt"]);
    let c1 = commit_all(d, "delete b.txt");
    git(d, &["mv", "a.txt", "b.txt"]);
    let c2 = commit_all(d, "rename a.txt to b.txt");

    // 2. Prepare two files `x.txt` and `y.txt`, modify both, then swap `x <-> y` via
    // rename commits (`x -> tmp`, `y -> x`, `tmp -> y`) so git emits real `Delta::Renamed`.
    let x_body = "x-unique-1\nx-unique-2\nx-unique-3\nx-unique-4\n";
    let y_body = "y-unique-1\ny-unique-2\ny-unique-3\ny-unique-4\n";
    write_file(d, "x.txt", x_body);
    write_file(d, "y.txt", y_body);
    commit_all(d, "add x and y");

    write_file(d, "x.txt", &format!("{x_body}x-edit\n"));
    write_file(d, "y.txt", &format!("{y_body}y-edit\n"));
    let c3 = commit_all(d, "edit x and y");

    git(d, &["mv", "x.txt", "tmp_swap.txt"]);
    let c4 = commit_all(d, "mv x to tmp");
    git(d, &["mv", "y.txt", "x.txt"]);
    let c5 = commit_all(d, "mv y to x");
    git(d, &["mv", "tmp_swap.txt", "y.txt"]);
    let c6 = commit_all(d, "mv tmp to y");

    let loc = RepoLocation::discover(d).unwrap();
    let (handle, mut rx) = EngineSupervisor::start(loc, false).unwrap();

    // Multi-select [c2, c1]: must keep BOTH Deleted b.txt AND Renamed a.txt -> b.txt.
    let ep1 = handle.request_commit_details(
        RequestSlotId::LogCommitDetails,
        DetailsKey::commits(vec![c2, c1], 0),
    );
    let b1 = recv_until(&mut rx, |ev| match ev {
        EngineEvent::CommitDetailsLoaded { bundle, epoch, .. } if epoch == ep1 => Some(bundle),
        _ => None,
    })
    .await;
    assert!(
        b1.changes.iter().any(|c| c.path == "b.txt"
            && c.kind == FileStatusKind::Deleted
            && c.old_path.is_none()),
        "expected displaced Deleted b.txt in {:?}",
        b1.changes
    );
    assert!(
        b1.changes.iter().any(|c| {
            c.path == "b.txt"
                && c.kind == FileStatusKind::Renamed
                && c.old_path.as_deref() == Some("a.txt")
        }),
        "expected Renamed a.txt -> b.txt in {:?}",
        b1.changes
    );

    // Multi-select [c6, c5, c4, c3]: must preserve BOTH swapped renames without clobbering.
    let ep2 = handle.request_commit_details(
        RequestSlotId::LogCommitDetails,
        DetailsKey::commits(vec![c6, c5, c4, c3], 0),
    );
    let b2 = recv_until(&mut rx, |ev| match ev {
        EngineEvent::CommitDetailsLoaded { bundle, epoch, .. } if epoch == ep2 => Some(bundle),
        _ => None,
    })
    .await;
    assert!(
        b2.changes.iter().any(|c| {
            c.path == "x.txt"
                && c.kind == FileStatusKind::Renamed
                && c.old_path.as_deref() == Some("y.txt")
        }),
        "expected Renamed y.txt -> x.txt in {:?}",
        b2.changes
    );
    assert!(
        b2.changes.iter().any(|c| {
            c.path == "y.txt"
                && c.kind == FileStatusKind::Renamed
                && c.old_path.as_deref() == Some("x.txt")
        }),
        "expected Renamed x.txt -> y.txt in {:?}",
        b2.changes
    );

    // Non-contiguous collision: select [c5 (mv y -> x), c3 (edit x & y)] skipping c4.
    // The displaced modification of original `x.txt` must not be silently dropped when `y -> x` lands.
    let ep3 = handle.request_commit_details(
        RequestSlotId::LogCommitDetails,
        DetailsKey::commits(vec![c5, c3], 0),
    );
    let b3 = recv_until(&mut rx, |ev| match ev {
        EngineEvent::CommitDetailsLoaded { bundle, epoch, .. } if epoch == ep3 => Some(bundle),
        _ => None,
    })
    .await;
    assert!(
        b3.changes.iter().any(|c| c.path == "x.txt"
            && c.kind == FileStatusKind::Modified
            && c.old_path.is_none()),
        "expected displaced Modified x.txt in {:?}",
        b3.changes
    );
    assert!(
        b3.changes.iter().any(|c| {
            c.path == "x.txt"
                && c.kind == FileStatusKind::Renamed
                && c.old_path.as_deref() == Some("y.txt")
        }),
        "expected Renamed y.txt -> x.txt in {:?}",
        b3.changes
    );

    assert!(handle.shutdown(Duration::from_secs(2)).await);
}

#[tokio::test]
async fn multi_commit_file_diff_resolves_modes_from_matching_commits() {
    let tmp = TempDir::new().unwrap();
    let d = tmp.path();
    init_repo(d);

    write_file(d, "script.sh", "#!/bin/sh\necho v1\n");
    write_file(d, "other.txt", "base\n");
    commit_all(d, "base");

    // Branch `side`: change `script.sh` mode 100644 -> 100755 and add a symlink `sym.txt` (120000).
    git(d, &["checkout", "-b", "side"]);
    write_file(d, "script.sh", "#!/bin/sh\necho v2\n");
    git(d, &["add", "script.sh"]);
    git(d, &["update-index", "--chmod=+x", "script.sh"]);
    let sym_oid = git_with_stdin(d, &["hash-object", "-w", "--stdin"], "target/file.txt")
        .trim()
        .to_string();
    git(
        d,
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("120000,{sym_oid},sym.txt"),
        ],
    );
    git(d, &["commit", "-m", "side exec + symlink"]);
    let s1: Oid = git(d, &["rev-parse", "HEAD"]).trim().parse().unwrap();

    // Branch `main`: `sym.txt` is a regular 100644 file, and `m1` only touches `other.txt`.
    git(d, &["checkout", "main"]);
    write_file(d, "sym.txt", "regular file on main\n");
    commit_all(d, "main adds regular sym.txt");
    write_file(d, "other.txt", "main edit\n");
    let m1 = commit_all(d, "main edits other.txt");

    let loc = RepoLocation::discover(d).unwrap();
    let (handle, mut rx) = EngineSupervisor::start(loc, false).unwrap();

    // Multi-select non-contiguous cross-branch `[m1, s1]`:
    // `oids.first()` is `m1` (where `script.sh` is 100644 and `sym.txt` is 100644 regular).
    let details_key = DetailsKey::commits(vec![m1, s1], 0);
    let ep_det =
        handle.request_commit_details(RequestSlotId::LogCommitDetails, details_key.clone());
    let bundle = recv_until(&mut rx, |ev| match ev {
        EngineEvent::CommitDetailsLoaded { bundle, epoch, .. } if epoch == ep_det => Some(bundle),
        _ => None,
    })
    .await;

    let script_change = bundle
        .changes
        .iter()
        .find(|c| c.path == "script.sh")
        .expect("script.sh change");
    let diff_ep = handle.request_file_diff_with_oids(
        RequestSlotId::LogFileDiff,
        FileDiffKey {
            details: details_key.clone(),
            path: "script.sh".into(),
            old_path: None,
            context: DiffContext::default(),
            whitespace: WhitespaceMode::default(),
        },
        Some((script_change.old_oid, script_change.new_oid)),
    );
    let script_diff = recv_until(&mut rx, |ev| match ev {
        EngineEvent::FileDiffLoaded { diff, epoch, .. } if epoch == diff_ep => Some(diff),
        _ => None,
    })
    .await;
    assert_eq!(
        script_diff.mode_change,
        FileModeChange::from_modes(Some(GIT_MODE_REGULAR), Some(GIT_MODE_EXECUTABLE)),
        "mode_change must resolve 100644 -> 100755 from commit s1 instead of guessing from m1"
    );

    let sym_change = bundle
        .changes
        .iter()
        .find(|c| c.path == "sym.txt")
        .expect("sym.txt change");
    assert_eq!(sym_change.kind, FileStatusKind::Added);
    assert_eq!(sym_change.old_oid, None);
    let sym_ep = handle.request_file_diff_with_oids(
        RequestSlotId::LogFileDiff,
        FileDiffKey {
            details: details_key,
            path: "sym.txt".into(),
            old_path: None,
            context: DiffContext::default(),
            whitespace: WhitespaceMode::default(),
        },
        Some((sym_change.old_oid, sym_change.new_oid)),
    );
    let sym_diff = recv_until(&mut rx, |ev| match ev {
        EngineEvent::FileDiffLoaded { diff, epoch, .. } if epoch == sym_ep => Some(diff),
        _ => None,
    })
    .await;
    assert_eq!(
        sym_diff.mode_change,
        FileModeChange::from_modes(None, Some(GIT_MODE_SYMLINK)),
        "added symlink in s1 must report (None -> 120000, is_symlink=true) and not invent an old_mode from m1"
    );
    assert!(!sym_diff.is_binary);

    assert!(handle.shutdown(Duration::from_secs(2)).await);
}

#[tokio::test]
async fn conflict_resolver_handles_directory_and_dangling_symlinks() {
    let tmp = TempDir::new().unwrap();
    let d = tmp.path();
    init_repo(d);

    write_file(d, "seed.txt", "seed\n");
    commit_all(d, "seed");

    fs::create_dir_all(d.join("real_dir")).unwrap();
    let dir_link = d.join("link_to_dir");
    let dangling_link = d.join("dangling_link");

    #[cfg(unix)]
    let symlinks_ok = std::os::unix::fs::symlink("real_dir", &dir_link).is_ok()
        && std::os::unix::fs::symlink("does_not_exist", &dangling_link).is_ok();
    #[cfg(windows)]
    let symlinks_ok = std::os::windows::fs::symlink_dir("real_dir", &dir_link).is_ok()
        && std::os::windows::fs::symlink_file("does_not_exist", &dangling_link).is_ok();

    if !symlinks_ok {
        // Windows environments without Developer Mode / SeCreateSymbolicLinkPrivilege
        // cannot create OS symlinks in unprivileged tests.
        return;
    }

    let base_blob = git_with_stdin(d, &["hash-object", "-w", "--stdin"], "base_target")
        .trim()
        .to_string();
    let ours_blob = git_with_stdin(d, &["hash-object", "-w", "--stdin"], "real_dir")
        .trim()
        .to_string();
    let theirs_blob = git_with_stdin(d, &["hash-object", "-w", "--stdin"], "other_dir")
        .trim()
        .to_string();
    let dangling_blob = git_with_stdin(d, &["hash-object", "-w", "--stdin"], "does_not_exist")
        .trim()
        .to_string();

    let index_entries = format!(
        "120000 {base_blob} 1\tlink_to_dir\n\
         120000 {ours_blob} 2\tlink_to_dir\n\
         120000 {theirs_blob} 3\tlink_to_dir\n"
    );
    git_with_stdin(d, &["update-index", "--index-info"], &index_entries);
    let _ = dangling_blob;

    let loc = RepoLocation::discover(d).unwrap();
    let (handle, mut rx) = EngineSupervisor::start(loc, false).unwrap();

    // 1. Symlink pointing to a directory must load as text conflict (NOT SpecialConflictKind::Directory).
    let ep1 = handle.request_conflict_document("link_to_dir".into());
    let doc1 = recv_until(&mut rx, |ev| match ev {
        EngineEvent::ConflictDocumentLoaded { doc, epoch } if epoch == ep1 => Some(doc),
        EngineEvent::SpecialConflictLoaded { info, epoch } if epoch == ep1 => {
            panic!("symlink to directory misclassified as special conflict: {info:?}");
        }
        _ => None,
    })
    .await;
    assert!(
        doc1.render_merged().contains("real_dir"),
        "expected symlink target in conflict doc, got: {:?}",
        doc1.render_merged()
    );

    // 2. Dangling symlink (no index conflict stages -> parsed directly from worktree) must read
    // its link target via `symlink_metadata`, not empty worktree text from `is_file() == false`.
    let ep2 = handle.request_conflict_document("dangling_link".into());
    let doc2 = recv_until(&mut rx, |ev| match ev {
        EngineEvent::ConflictDocumentLoaded { doc, epoch } if epoch == ep2 => Some(doc),
        EngineEvent::SpecialConflictLoaded { info, epoch } if epoch == ep2 => {
            panic!("dangling symlink misclassified as special conflict: {info:?}");
        }
        _ => None,
    })
    .await;
    assert!(
        doc2.render_merged().contains("does_not_exist"),
        "expected dangling symlink target from worktree, got: {:?}",
        doc2.render_merged()
    );

    assert!(handle.shutdown(Duration::from_secs(2)).await);
}

#[tokio::test]
async fn compare_with_local_matches_literal_brackets_in_path_prefix() {
    let tmp = TempDir::new().unwrap();
    let d = tmp.path();
    init_repo(d);

    write_file(d, "app/[id]/page.tsx", "export default 1;\n");
    write_file(d, "other/file.txt", "keep\n");
    let head = commit_all(d, "init");

    write_file(d, "app/[id]/page.tsx", "export default 2;\n");
    write_file(d, "other/file.txt", "changed\n");

    let loc = RepoLocation::discover(d).unwrap();
    let (handle, mut rx) = EngineSupervisor::start(loc, false).unwrap();

    let mut compare_key = DetailsKey::compare_with_local(head);
    compare_key.path_prefix = Some("app/[id]".into());
    let ep = handle.request_commit_details(RequestSlotId::LogCommitDetails, compare_key);
    let bundle = recv_until(&mut rx, |ev| match ev {
        EngineEvent::CommitDetailsLoaded { bundle, epoch, .. } if epoch == ep => Some(bundle),
        _ => None,
    })
    .await;

    let paths: Vec<&str> = bundle.changes.iter().map(|c| c.path.as_str()).collect();
    assert_eq!(paths, vec!["app/[id]/page.tsx"]);

    assert!(handle.shutdown(Duration::from_secs(2)).await);
}

#[tokio::test]
async fn rapid_cli_detail_requests_cancel_without_starving_interactive_pool() {
    let tmp = TempDir::new().unwrap();
    let d = tmp.path();
    init_repo(d);

    for i in 0..6 {
        write_file(d, &format!("f{i}.txt"), &format!("line {i}\n"));
    }
    let head = commit_all(d, "seed files");

    let loc = RepoLocation::discover(d).unwrap();
    let (handle, mut rx) = EngineSupervisor::start(loc, false).unwrap();

    // Burst 8 rapid requests across all async CLI loaders + interactive git2 details.
    let mut last_hist_ep = 0;
    let mut last_blame_ep = 0;
    let mut last_tree_ep = 0;
    let mut last_reflog_ep = 0;
    for i in 0..8 {
        let f = format!("f{}.txt", i % 6);
        last_hist_ep = handle.request_file_history(FileHistoryKey::new(&f, None, true, None));
        last_blame_ep = handle.request_blame(BlameKey {
            path: f,
            rev: None,
            options: BlameOptions::default(),
        });
        last_tree_ep = handle.request_tree_index(head);
        last_reflog_ep = handle.request_reflog(ReflogKey::head());
    }
    let details_ep =
        handle.request_commit_details(RequestSlotId::LogCommitDetails, DetailsKey::single(head));

    let mut got_hist = false;
    let mut got_blame = false;
    let mut got_tree = false;
    let mut got_reflog = false;
    let mut got_details = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !(got_hist && got_blame && got_tree && got_reflog && got_details) {
        let ev = tokio::time::timeout_at(deadline, rx.recv())
            .await
            .expect("timed out waiting for latest epochs")
            .expect("channel open");
        match ev {
            EngineEvent::FileHistoryLoaded { epoch, .. } if epoch == last_hist_ep => {
                got_hist = true;
            }
            EngineEvent::BlameLoaded { epoch, .. } if epoch == last_blame_ep => {
                got_blame = true;
            }
            EngineEvent::TreeIndexLoaded { epoch, .. } if epoch == last_tree_ep => {
                got_tree = true;
            }
            EngineEvent::ReflogLoaded { epoch, .. } if epoch == last_reflog_ep => {
                got_reflog = true;
            }
            EngineEvent::CommitDetailsLoaded { epoch, .. } if epoch == details_ep => {
                got_details = true;
            }
            EngineEvent::Error(e) => panic!("unexpected engine error: {e}"),
            _ => {}
        }
    }

    assert!(handle.shutdown(Duration::from_secs(2)).await);
}
