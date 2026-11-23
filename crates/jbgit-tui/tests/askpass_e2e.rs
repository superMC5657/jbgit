//! End-to-end check of the `GIT_ASKPASS` bridge (Spec 12 Issue 08): a real
//! `git credential fill` launches the built `jbgit` binary as its askpass
//! helper, which reaches the session server over the owner-only local channel.
//!
//! An HTTP Basic Auth push test needs a `git http-backend` server; it is
//! covered manually (see `docs/manual.md`, "Credentials"). This test exercises
//! the same git → askpass → jbgit path without any network.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Arc;

use jbgit_core::{GitFailure, classify_failure};
use jbgit_engine::askpass::{AskpassServer, FixedResponder};

fn credential_fill(vars: Vec<(String, String)>) -> std::process::Output {
    let mut child = Command::new("git")
        .args(["-c", "credential.helper=", "credential", "fill"])
        .env("GIT_TERMINAL_PROMPT", "0")
        .env_remove("GCM_INTERACTIVE")
        .envs(vars)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("git");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"protocol=https\nhost=example.com\n\n")
        .unwrap();
    child.wait_with_output().expect("git output")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn git_credential_fill_is_answered_through_the_askpass_bridge() {
    let exe = PathBuf::from(env!("CARGO_BIN_EXE_jbgit"));

    let responder = Arc::new(FixedResponder::answering("hunter2"));
    let server = AskpassServer::start(responder.clone()).expect("askpass server");
    let vars = server.env(exe.clone()).vars();
    let out = tokio::task::spawn_blocking(move || credential_fill(vars))
        .await
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(stdout.contains("username=hunter2"), "{stdout}");
    assert!(stdout.contains("password=hunter2"), "{stdout}");
    let prompts = responder.prompts();
    assert_eq!(prompts.len(), 2, "{prompts:?}");
    assert!(
        prompts[0].starts_with("Username for 'https://example.com'"),
        "{prompts:?}"
    );
    assert!(
        prompts[1].starts_with("Password for 'https://hunter2@example.com'"),
        "{prompts:?}"
    );
    drop(server);

    // Cancelling makes askpass exit 1: git gives up with an auth failure.
    let server = AskpassServer::start(Arc::new(FixedResponder::cancelling())).unwrap();
    let vars = server.env(exe.clone()).vars();
    let out = tokio::task::spawn_blocking(move || credential_fill(vars))
        .await
        .unwrap();
    assert!(!out.status.success());
    let stderr: Vec<String> = String::from_utf8_lossy(&out.stderr)
        .lines()
        .map(ToString::to_string)
        .collect();
    assert_eq!(
        classify_failure(&stderr),
        GitFailure::AuthenticationFailed,
        "{stderr:?}"
    );

    // A token that does not match the session gets nothing.
    let mut vars = server.env(exe).vars();
    for (k, v) in &mut vars {
        if k == "JBGIT_ASKPASS_TOKEN" {
            *v = "0".repeat(64);
        }
    }
    let out = tokio::task::spawn_blocking(move || credential_fill(vars))
        .await
        .unwrap();
    assert!(!out.status.success());
    assert!(!String::from_utf8_lossy(&out.stdout).contains("password="));
}
