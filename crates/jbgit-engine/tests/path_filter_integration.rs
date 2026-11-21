//! The native `git log --full-history` path filter must
//! return exactly the commits the libgit2 per-commit tree diff returns,
//! including TREESAME merges, evil merges and tag-only history.

use std::collections::HashSet;
use std::fs;
use std::path::Path;
use std::process::Command;

use jbgit_core::{Oid, RepoSnapshot, RequestEpoch};
use jbgit_engine::pool::load_repo_snapshot_sync;
use jbgit_engine::{
    EngineError, GitCliExecutor, RepoLocation, load_path_matches_cli, load_path_matches_git2,
    load_path_matches_sync,
};
use tempfile::TempDir;

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

struct Fixture {
    _tmp: TempDir,
    loc: RepoLocation,
    snapshot: RepoSnapshot,
    merge_treesame: Oid,
    merge_evil: Oid,
    tag_only: Oid,
}

/// ```text
///   c1 ── m1 ─────── M1 ── m2 ── M2(evil, a.txt)   main
///    \              /        \  /
///     s1(b.txt) ───┘          s2(a.txt)            side / side2
///    \
///     t1(b.txt)   reachable only through tag `orphan-tag`
/// ```
fn fixture() -> Fixture {
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
    let snapshot = load_repo_snapshot_sync(&loc, &RequestEpoch::new().issue(), 1000).unwrap();
    Fixture {
        _tmp: tmp,
        loc,
        snapshot,
        merge_treesame,
        merge_evil,
        tag_only,
    }
}

fn paths(list: &[&str]) -> Vec<String> {
    list.iter().map(ToString::to_string).collect()
}

#[test]
fn git_log_path_filter_matches_libgit2_treesame_semantics() {
    let fx = fixture();
    let git_bin = GitCliExecutor::detect().expect("git on PATH").info().path;
    let epoch = RequestEpoch::new();

    for set in [
        paths(&["a.txt"]),
        paths(&["b.txt"]),
        paths(&["dir"]),
        paths(&["dir/c.txt", "b.txt"]),
        paths(&["missing.txt"]),
    ] {
        let token = epoch.issue();
        let cli = load_path_matches_cli(&git_bin, &fx.loc, &set, &fx.snapshot, &token).unwrap();
        let lib = load_path_matches_git2(&fx.loc, &set, &fx.snapshot, &token).unwrap();
        assert_eq!(cli, lib, "path set {set:?}");
        let via_sync = load_path_matches_sync(&fx.loc, &set, &fx.snapshot, &token).unwrap();
        assert_eq!(via_sync, lib, "path set {set:?}");
    }

    let token = epoch.issue();
    let b = load_path_matches_cli(&git_bin, &fx.loc, &paths(&["b.txt"]), &fx.snapshot, &token)
        .unwrap();
    assert!(!b.contains(&fx.merge_treesame), "TREESAME merge must be excluded");
    assert!(b.contains(&fx.tag_only), "tag-only history must be walked");

    let a = load_path_matches_cli(&git_bin, &fx.loc, &paths(&["a.txt"]), &fx.snapshot, &token)
        .unwrap();
    assert!(a.contains(&fx.merge_evil), "evil merge differs from every parent");

    let none: HashSet<Oid> = load_path_matches_cli(
        &git_bin,
        &fx.loc,
        &paths(&["missing.txt"]),
        &fx.snapshot,
        &token,
    )
    .unwrap();
    assert!(none.is_empty());
}

#[test]
fn git_log_path_filter_honours_cancellation() {
    let fx = fixture();
    let git_bin = GitCliExecutor::detect().expect("git on PATH").info().path;
    let epoch = RequestEpoch::new();
    let stale = epoch.issue();
    let _newer = epoch.issue();
    let res = load_path_matches_cli(&git_bin, &fx.loc, &paths(&["a.txt"]), &fx.snapshot, &stale);
    assert!(matches!(res, Err(EngineError::Cancelled)), "{res:?}");
    let res = load_path_matches_sync(&fx.loc, &paths(&["a.txt"]), &fx.snapshot, &stale);
    assert!(matches!(res, Err(EngineError::Cancelled)), "{res:?}");
}
