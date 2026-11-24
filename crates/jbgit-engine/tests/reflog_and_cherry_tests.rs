use std::path::Path;
use std::process::Command;

use jbgit_core::{CherryPickedKey, Oid, ReflogActionKind, ReflogKey, RequestEpoch};
use jbgit_engine::{RepoLocation, load_cherry_picked_sync, load_reflog_sync};
use tempfile::TempDir;

fn run_git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .expect("run git");
    assert!(
        out.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn init_temp_repo() -> (TempDir, RepoLocation) {
    let td = TempDir::new().expect("tempdir");
    run_git(td.path(), &["init", "-b", "main"]);
    run_git(td.path(), &["config", "user.name", "Test User"]);
    run_git(td.path(), &["config", "user.email", "test@example.com"]);
    let loc = RepoLocation {
        workdir: td.path().to_path_buf(),
        git_dir: td.path().join(".git"),
        common_dir: td.path().join(".git"),
        is_bare: false,
        repo_name: "test".to_string(),
    };
    (td, loc)
}

#[test]
fn load_cherry_picked_sync_marks_picked_commit_and_ignores_unpicked() {
    let (td, loc) = init_temp_repo();

    std::fs::write(td.path().join("base.txt"), "base\n").unwrap();
    run_git(td.path(), &["add", "."]);
    run_git(td.path(), &["commit", "-m", "base commit"]);

    run_git(td.path(), &["checkout", "-b", "feat"]);
    std::fs::write(td.path().join("picked.txt"), "picked content\n").unwrap();
    run_git(td.path(), &["add", "."]);
    run_git(td.path(), &["commit", "-m", "feat: picked change"]);
    let feat_picked_oid: Oid = run_git(td.path(), &["rev-parse", "HEAD"]).parse().unwrap();

    std::fs::write(td.path().join("unpicked.txt"), "unpicked content\n").unwrap();
    run_git(td.path(), &["add", "."]);
    run_git(td.path(), &["commit", "-m", "feat: unpicked change"]);
    let feat_unpicked_oid: Oid = run_git(td.path(), &["rev-parse", "HEAD"]).parse().unwrap();

    // Diverge main with a commit, then cherry-pick only the first feature commit onto main.
    run_git(td.path(), &["checkout", "main"]);
    std::fs::write(td.path().join("main_only.txt"), "main work\n").unwrap();
    run_git(td.path(), &["add", "."]);
    run_git(td.path(), &["commit", "-m", "main: independent commit"]);
    run_git(td.path(), &["cherry-pick", &feat_picked_oid.to_string()]);
    let main_head: Oid = run_git(td.path(), &["rev-parse", "HEAD"]).parse().unwrap();
    assert_ne!(main_head, feat_picked_oid);

    let epoch = RequestEpoch::new();
    let token = epoch.issue();
    let picked = load_cherry_picked_sync(
        &loc,
        &CherryPickedKey {
            head_oid: main_head,
            branch: "feat".to_string(),
        },
        &token,
    )
    .expect("load_cherry_picked_sync");

    assert!(
        picked.contains(&feat_picked_oid),
        "cherry-picked commit must be marked as already picked"
    );
    assert!(
        !picked.contains(&feat_unpicked_oid),
        "unpicked commit must not be marked"
    );
}

#[test]
fn load_reflog_sync_recovers_dropped_commit_after_reset_hard_and_paginates() {
    let (td, loc) = init_temp_repo();

    std::fs::write(td.path().join("a.txt"), "v1\n").unwrap();
    run_git(td.path(), &["add", "."]);
    run_git(td.path(), &["commit", "-m", "first commit"]);
    let first_oid: Oid = run_git(td.path(), &["rev-parse", "HEAD"]).parse().unwrap();

    std::fs::write(td.path().join("a.txt"), "v2\n").unwrap();
    run_git(td.path(), &["add", "."]);
    run_git(td.path(), &["commit", "-m", "accidentally dropped commit"]);
    let dropped_oid: Oid = run_git(td.path(), &["rev-parse", "HEAD"]).parse().unwrap();

    // Accidental reset --hard HEAD~1 drops `dropped_oid` from main's ancestry.
    run_git(td.path(), &["reset", "--hard", "HEAD~1"]);
    assert_eq!(
        run_git(td.path(), &["rev-parse", "HEAD"]),
        first_oid.to_string()
    );

    let epoch = RequestEpoch::new();
    let token = epoch.issue();
    let bundle = load_reflog_sync(&loc, &ReflogKey::head(), &token).expect("load_reflog_sync");
    assert!(!bundle.entries.is_empty());
    assert_eq!(bundle.entries[0].oid, first_oid);
    assert_eq!(bundle.entries[0].action, ReflogActionKind::Reset);
    assert!(
        bundle.entries.iter().any(|e| e.oid == dropped_oid && e.action == ReflogActionKind::Commit),
        "reflog must expose the dropped commit OID for recovery"
    );

    // Pagination check: limit = 1 should report has_more = true.
    let page1 = load_reflog_sync(
        &loc,
        &ReflogKey {
            ref_name: "HEAD".into(),
            limit: 1,
        },
        &token,
    )
    .expect("paginated reflog");
    assert_eq!(page1.entries.len(), 1);
    assert!(page1.has_more);
}
