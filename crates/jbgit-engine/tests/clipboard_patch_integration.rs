//! `Apply Patch from Clipboard` through the `JBGIT_CLIPBOARD_FILE` seam.
//! Its own test binary: it sets a process-wide environment variable.

use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use jbgit_core::{ApplyMode, GitOp, PatchCheckKey, PatchInput};
use jbgit_engine::{EngineBackend, EngineEvent, EngineSupervisor, RepoLocation, TaskRequest};
use tempfile::TempDir;

fn git(dir: &Path, args: &[&str]) -> Vec<u8> {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "T")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "T")
        .env("GIT_COMMITTER_EMAIL", "t@example.com")
        .output()
        .expect("git");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
}

#[tokio::test]
async fn clipboard_patch_is_checked_and_applied_from_stdin() {
    let dir_guard = TempDir::new().unwrap();
    let dir = dir_guard.path();
    git(dir, &["init", "-b", "main"]);
    git(dir, &["config", "core.autocrlf", "false"]);
    fs::write(dir.join("f.txt"), "one\ntwo\n").unwrap();
    git(dir, &["add", "."]);
    git(dir, &["commit", "-m", "init"]);
    fs::write(dir.join("f.txt"), "one\nTWO\n").unwrap();
    let patch = git(dir, &["diff"]);
    git(dir, &["checkout", "--", "."]);

    let clip_dir = TempDir::new().unwrap();
    let clip = clip_dir.path().join("clipboard.txt");
    // Simulate a Windows clipboard round trip: uniform CRLF endings.
    let crlf = String::from_utf8(patch).unwrap().replace('\n', "\r\n");
    fs::write(&clip, crlf).unwrap();
    // SAFETY: this binary runs a single test; nothing reads the env concurrently.
    unsafe { std::env::set_var(jbgit_engine::clipboard::CLIPBOARD_FILE_ENV, &clip) };

    let loc = RepoLocation::discover(dir).unwrap();
    let (handle, mut rx) = EngineSupervisor::start(loc, false).unwrap();
    let key = PatchCheckKey {
        source: PatchInput::Clipboard,
        mode: ApplyMode::WorkingTree,
    };
    let epoch = handle
        .dispatch(TaskRequest::CheckPatch { key: key.clone() })
        .unwrap();
    let check = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match rx.recv().await.expect("engine alive") {
                EngineEvent::PatchChecked { check, epoch: e } if e == epoch => break check,
                _ => {}
            }
        }
    })
    .await
    .unwrap();
    assert!(check.applies(), "{check:?}");
    assert_eq!(check.files.len(), 1);

    let _ = handle.dispatch(TaskRequest::RunGitOp {
        op: GitOp::ApplyPatch {
            source: PatchInput::Clipboard,
            mode: ApplyMode::WorkingTree,
        },
    });
    let outcome = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let EngineEvent::GitOpFinished(o) = rx.recv().await.expect("engine alive") {
                break o;
            }
        }
    })
    .await
    .unwrap();
    assert!(outcome.success, "{outcome:?}");
    let text = fs::read_to_string(dir.join("f.txt")).unwrap();
    assert_eq!(text.replace("\r\n", "\n"), "one\nTWO\n");
}
