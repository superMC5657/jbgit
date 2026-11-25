//! Integration tests for path filtering through the public [`EngineSupervisor`] seam.

use std::collections::HashSet;
use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use jbgit_core::{Oid, RepoSnapshot};
use jbgit_engine::{EngineBackend, EngineEvent, EngineSupervisor, RepoLocation};
use tempfile::TempDir;
use tokio::sync::mpsc::UnboundedReceiver;

fn git(dir: &Path, args: &[&str]) -> String {
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
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn write(dir: &Path, file: &str, content: &str) {
    let path = dir.join(file);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, content).unwrap();
}

fn commit(dir: &Path, files: &[(&str, &str)], msg: &str) -> Oid {
    for (f, c) in files {
        write(dir, f, c);
    }
    git(dir, &["add", "."]);
    git(dir, &["commit", "-m", msg]);
    rev(dir, "HEAD")
}

fn rev(dir: &Path, spec: &str) -> Oid {
    git(dir, &["rev-parse", spec]).trim().parse().unwrap()
}

async fn recv_matching<T>(
    rx: &mut UnboundedReceiver<EngineEvent>,
    mut predicate: impl FnMut(EngineEvent) -> Option<T>,
) -> T {
    tokio::time::timeout(Duration::from_secs(5), async {
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

#[tokio::test]
async fn path_filter_through_engine_handles_treesame_evil_merges_and_tags() {
    let tmp = TempDir::new().unwrap();
    let d = tmp.path();
    git(d, &["init", "-b", "main"]);
    git(d, &["config", "core.autocrlf", "false"]);
    git(d, &["config", "commit.gpgsign", "false"]);

    let c1 = commit(
        d,
        &[("a.txt", "a1\n"), ("b.txt", "b1\n"), ("dir/c.txt", "c1\n")],
        "c1",
    );
    commit(d, &[("dir/c.txt", "c2\n")], "m1");

    git(d, &["checkout", "-b", "side", &c1.to_string()]);
    commit(d, &[("b.txt", "b-side\n")], "s1");

    git(d, &["checkout", "-b", "tmp", &c1.to_string()]);
    let tag_only = commit(d, &[("b.txt", "b-tag\n")], "t1");
    git(d, &["tag", "orphan-tag"]);
    git(d, &["checkout", "main"]);
    git(d, &["branch", "-D", "tmp"]);

    git(d, &["merge", "--no-ff", "-m", "M1", "side"]);
    let merge_treesame = rev(d, "HEAD");

    git(d, &["checkout", "-b", "side2"]);
    commit(d, &[("a.txt", "a-side2\n")], "s2");
    git(d, &["checkout", "main"]);
    commit(d, &[("b.txt", "b-main\n")], "m2");
    git(d, &["merge", "--no-ff", "--no-commit", "side2"]);
    write(d, "a.txt", "a-evil\n");
    git(d, &["add", "a.txt"]);
    git(d, &["commit", "-m", "M2"]);
    let merge_evil = rev(d, "HEAD");

    let loc = RepoLocation::discover(d).unwrap();
    let (handle, mut rx) = EngineSupervisor::start(loc, false).unwrap();

    let snap_epoch = handle.request_repo_snapshot();
    let snapshot: Arc<RepoSnapshot> = recv_matching(&mut rx, |ev| match ev {
        EngineEvent::RepoSnapshotLoaded { snapshot, epoch } if epoch == snap_epoch => {
            Some(snapshot)
        }
        _ => None,
    })
    .await;

    let query = |paths: Vec<String>| handle.request_path_matches(paths, Arc::clone(&snapshot));

    let e_b = query(vec!["b.txt".into()]);
    let b: Arc<HashSet<Oid>> = recv_matching(&mut rx, |ev| match ev {
        EngineEvent::PathMatchesLoaded { matches, epoch, .. } if epoch == e_b => Some(matches),
        _ => None,
    })
    .await;
    assert!(
        !b.contains(&merge_treesame),
        "TREESAME merge must be excluded"
    );
    assert!(b.contains(&tag_only), "tag-only history must be walked");

    let e_a = query(vec!["a.txt".into()]);
    let a = recv_matching(&mut rx, |ev| match ev {
        EngineEvent::PathMatchesLoaded { matches, epoch, .. } if epoch == e_a => Some(matches),
        _ => None,
    })
    .await;
    assert!(
        a.contains(&merge_evil),
        "evil merge differs from every parent"
    );

    let e_none = query(vec!["missing.txt".into()]);
    let none = recv_matching(&mut rx, |ev| match ev {
        EngineEvent::PathMatchesLoaded { matches, epoch, .. } if epoch == e_none => Some(matches),
        _ => None,
    })
    .await;
    assert!(none.is_empty());
}
