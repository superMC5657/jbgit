//! End-to-end check of the `GIT_ASKPASS` bridge (Spec 12 Issue 08): a real
//! `git credential fill` launches the built `jbgit` binary as its askpass
//! helper, which reaches the session server over the owner-only local channel.
//!
//! An HTTP Basic Auth push test needs a `git http-backend` server; it is
//! covered manually (see `docs/manual.md`, "Credentials"). This test exercises
//! the same git → askpass → jbgit path without any network.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

use jbgit_core::{GitFailure, classify_failure};
use jbgit_engine::askpass::{
    AskpassResponder, AskpassServer, FixedResponder, ResponseFuture, Secret, request_credential,
};
use jbgit_tui::init_engine_and_session;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

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

fn git_cmd(dir: &Path, args: &[&str]) -> std::process::Output {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "Alice")
        .env("GIT_AUTHOR_EMAIL", "alice@example.com")
        .env("GIT_COMMITTER_NAME", "Alice")
        .env("GIT_COMMITTER_EMAIL", "alice@example.com")
        .output()
        .expect("git");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn repo_session_unbind_closes_askpass_server() {
    let tmp = tempfile::TempDir::new().unwrap();
    let repo_dir = tmp.path().join("repo");
    std::fs::create_dir_all(&repo_dir).unwrap();
    git_cmd(&repo_dir, &["init", "-b", "main"]);
    std::fs::write(repo_dir.join("a.txt"), "hi\n").unwrap();
    git_cmd(&repo_dir, &["add", "."]);
    git_cmd(&repo_dir, &["commit", "-m", "init"]);

    let server = AskpassServer::start(Arc::new(FixedResponder::answering("ok"))).unwrap();
    let (repo, _session) = init_engine_and_session(repo_dir, Some(server));
    let env = repo.askpass_env().expect("askpass env bound");
    let addr = env.addr().to_string();
    let token = env
        .vars()
        .into_iter()
        .find(|(k, _)| k == "JBGIT_ASKPASS_TOKEN")
        .map(|(_, v)| v)
        .expect("token");

    let addr_before = addr.clone();
    let token_before = token.clone();
    let reply = tokio::task::spawn_blocking(move || {
        request_credential(&addr_before, &token_before, "Password:")
    })
    .await
    .unwrap()
    .unwrap();
    assert_eq!(reply.as_deref().map(String::as_str), Some("ok"));

    assert!(repo.unbind().await);

    let after = tokio::task::spawn_blocking(move || request_credential(&addr, &token, "Password:"))
        .await
        .unwrap();
    assert!(
        after.is_err(),
        "expected closed askpass channel after unbind, got {after:?}"
    );
}

struct BasicAuthResponder {
    prompts: Mutex<Vec<String>>,
}

impl BasicAuthResponder {
    fn new() -> Self {
        Self {
            prompts: Mutex::new(Vec::new()),
        }
    }

    fn prompts(&self) -> Vec<String> {
        self.prompts.lock().map(|p| p.clone()).unwrap_or_default()
    }
}

impl AskpassResponder for BasicAuthResponder {
    fn respond(&self, prompt: String) -> ResponseFuture {
        if let Ok(mut p) = self.prompts.lock() {
            p.push(prompt.clone());
        }
        let ans = if prompt.to_ascii_lowercase().contains("username") {
            "alice"
        } else {
            "s3cr3t"
        };
        let secret = Secret::new(ans.to_string());
        Box::pin(async move { Some(secret) })
    }
}

fn find_http_backend() -> PathBuf {
    let out = Command::new("git")
        .arg("--exec-path")
        .output()
        .expect("git --exec-path");
    assert!(out.status.success());
    let exec_dir = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
    let bin = if cfg!(windows) {
        "git-http-backend.exe"
    } else {
        "git-http-backend"
    };
    let path = exec_dir.join(bin);
    assert!(path.is_file(), "missing {path:?}");
    path
}

async fn read_http_request(
    stream: &mut TcpStream,
) -> Option<(String, String, Vec<(String, String)>, Vec<u8>)> {
    let mut buf = Vec::with_capacity(4096);
    let header_end = loop {
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos;
        }
        let mut tmp = [0u8; 2048];
        let n = stream.read(&mut tmp).await.ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&tmp[..n]);
    };

    let head_str = String::from_utf8_lossy(&buf[..header_end]).into_owned();
    let mut rest = buf[header_end + 4..].to_vec();
    let mut lines = head_str.split("\r\n");
    let req_line = lines.next()?;
    let mut parts = req_line.split_whitespace();
    let method = parts.next()?.to_string();
    let target = parts.next()?.to_string();

    let mut headers = Vec::new();
    let mut content_len: Option<usize> = None;
    let mut chunked = false;
    let mut expect_continue = false;
    for line in lines {
        if let Some((k, v)) = line.split_once(':') {
            let k_trim = k.trim().to_string();
            let v_trim = v.trim().to_string();
            if k_trim.eq_ignore_ascii_case("content-length") {
                content_len = v_trim.parse::<usize>().ok();
            } else if k_trim.eq_ignore_ascii_case("transfer-encoding")
                && v_trim.to_ascii_lowercase().contains("chunked")
            {
                chunked = true;
            } else if k_trim.eq_ignore_ascii_case("expect")
                && v_trim.to_ascii_lowercase().contains("100-continue")
            {
                expect_continue = true;
            }
            headers.push((k_trim, v_trim));
        }
    }

    if expect_continue {
        stream
            .write_all(b"HTTP/1.1 100 Continue\r\n\r\n")
            .await
            .ok()?;
        stream.flush().await.ok()?;
    }

    let body = if chunked {
        let mut decoded = Vec::new();
        let mut cursor = 0usize;
        loop {
            while !rest[cursor..].windows(2).any(|w| w == b"\r\n") {
                let mut tmp = [0u8; 4096];
                let n = stream.read(&mut tmp).await.ok()?;
                if n == 0 {
                    return None;
                }
                rest.extend_from_slice(&tmp[..n]);
            }
            let rel_crlf = rest[cursor..]
                .windows(2)
                .position(|w| w == b"\r\n")
                .unwrap();
            let size_line = String::from_utf8_lossy(&rest[cursor..cursor + rel_crlf]);
            let size_hex = size_line.split(';').next().unwrap_or("").trim();
            let size = usize::from_str_radix(size_hex, 16).ok()?;
            cursor += rel_crlf + 2;
            if size == 0 {
                break;
            }
            while rest.len() < cursor + size + 2 {
                let mut tmp = [0u8; 4096];
                let n = stream.read(&mut tmp).await.ok()?;
                if n == 0 {
                    return None;
                }
                rest.extend_from_slice(&tmp[..n]);
            }
            decoded.extend_from_slice(&rest[cursor..cursor + size]);
            cursor += size + 2;
        }
        decoded
    } else if let Some(len) = content_len {
        while rest.len() < len {
            let mut tmp = [0u8; 4096];
            let n = stream.read(&mut tmp).await.ok()?;
            if n == 0 {
                break;
            }
            rest.extend_from_slice(&tmp[..n]);
        }
        rest.truncate(len);
        rest
    } else {
        Vec::new()
    };

    Some((method, target, headers, body))
}

async fn handle_http_conn(mut stream: TcpStream, project_root: PathBuf, backend: PathBuf) {
    let Some((method, target, headers, body)) = read_http_request(&mut stream).await else {
        return;
    };

    // Base64("alice:s3cr3t") == "YWxpY2U6czNjcjN0"
    let authed = headers.iter().any(|(k, v)| {
        k.eq_ignore_ascii_case("authorization") && v.trim() == "Basic YWxpY2U6czNjcjN0"
    });
    if !authed {
        let resp = concat!(
            "HTTP/1.1 401 Unauthorized\r\n",
            "WWW-Authenticate: Basic realm=\"jbgit-test\"\r\n",
            "Content-Length: 0\r\n",
            "Connection: close\r\n\r\n"
        );
        let _ = stream.write_all(resp.as_bytes()).await;
        return;
    }

    let (path_info, query) = target.split_once('?').unwrap_or((&target, ""));
    let content_type = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-type"))
        .map(|(_, v)| v.as_str())
        .unwrap_or("");

    let mut cmd = tokio::process::Command::new(&backend);
    cmd.env("GIT_PROJECT_ROOT", &project_root)
        .env("GIT_HTTP_EXPORT_ALL", "1")
        .env("REQUEST_METHOD", &method)
        .env("PATH_INFO", path_info)
        .env("QUERY_STRING", query)
        .env("CONTENT_TYPE", content_type)
        .env("CONTENT_LENGTH", body.len().to_string())
        .env("REMOTE_USER", "alice")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().expect("spawn git-http-backend");
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(&body).await;
        drop(stdin);
    }
    let out = child
        .wait_with_output()
        .await
        .expect("git-http-backend output");
    assert!(
        out.status.success(),
        "git-http-backend failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let (cgi_head, cgi_body) = if let Some(p) = out.stdout.windows(4).position(|w| w == b"\r\n\r\n")
    {
        (&out.stdout[..p], &out.stdout[p + 4..])
    } else if let Some(p) = out.stdout.windows(2).position(|w| w == b"\n\n") {
        (&out.stdout[..p], &out.stdout[p + 2..])
    } else {
        (&[][..], out.stdout.as_slice())
    };

    let mut status_line = "200 OK".to_string();
    let mut resp_headers = Vec::new();
    for line in String::from_utf8_lossy(cgi_head).lines() {
        let line = line.trim_end_matches('\r');
        if let Some(val) = line
            .strip_prefix("Status:")
            .or_else(|| line.strip_prefix("status:"))
        {
            status_line = val.trim().to_string();
        } else if !line.is_empty() {
            resp_headers.push(line.to_string());
        }
    }

    let mut wire = format!("HTTP/1.1 {status_line}\r\n");
    for h in resp_headers {
        wire.push_str(&h);
        wire.push_str("\r\n");
    }
    wire.push_str(&format!("Content-Length: {}\r\n", cgi_body.len()));
    wire.push_str("Connection: close\r\n\r\n");
    let _ = stream.write_all(wire.as_bytes()).await;
    let _ = stream.write_all(cgi_body).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn http_basic_auth_push_uses_askpass_bridge_and_classifies_cancel() {
    let backend = find_http_backend();
    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path().join("srv");
    let bare = root.join("repo.git");
    let local = tmp.path().join("local");
    std::fs::create_dir_all(&bare).unwrap();
    std::fs::create_dir_all(&local).unwrap();

    git_cmd(&bare, &["init", "--bare", "-b", "main"]);
    git_cmd(&bare, &["config", "http.receivepack", "true"]);

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let srv_root = root.clone();
    let srv_task = tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            let root_c = srv_root.clone();
            let backend_c = backend.clone();
            tokio::spawn(async move {
                handle_http_conn(stream, root_c, backend_c).await;
            });
        }
    });

    git_cmd(&local, &["init", "-b", "main"]);
    git_cmd(&local, &["config", "user.name", "Alice"]);
    git_cmd(&local, &["config", "user.email", "alice@example.com"]);
    std::fs::write(local.join("f.txt"), "one\n").unwrap();
    git_cmd(&local, &["add", "."]);
    git_cmd(&local, &["commit", "-m", "c1"]);
    git_cmd(
        &local,
        &[
            "remote",
            "add",
            "origin",
            &format!("http://127.0.0.1:{port}/repo.git"),
        ],
    );

    let exe = PathBuf::from(env!("CARGO_BIN_EXE_jbgit"));
    let responder = Arc::new(BasicAuthResponder::new());
    let server = AskpassServer::start(responder.clone()).unwrap();
    let vars = server.env(exe.clone()).vars();
    let local_c = local.clone();
    let out = tokio::task::spawn_blocking(move || {
        Command::new("git")
            .args([
                "-c",
                "credential.helper=",
                "-c",
                "http.emptyAuth=false",
                "push",
                "origin",
                "main",
            ])
            .current_dir(&local_c)
            .env("GIT_TERMINAL_PROMPT", "0")
            .env_remove("GCM_INTERACTIVE")
            .envs(vars)
            .output()
            .expect("git push")
    })
    .await
    .unwrap();
    assert!(
        out.status.success(),
        "HTTP push failed: {}\nprompts={:?}",
        String::from_utf8_lossy(&out.stderr),
        responder.prompts()
    );
    assert!(
        !responder.prompts().is_empty(),
        "expected Basic Auth askpass prompts"
    );
    let local_head = git_cmd(&local, &["rev-parse", "HEAD"]).stdout;
    let bare_head = git_cmd(&bare, &["rev-parse", "refs/heads/main"]).stdout;
    assert_eq!(local_head, bare_head);
    drop(server);

    // Second commit + cancelled askpass => AuthenticationFailed.
    std::fs::write(local.join("f.txt"), "two\n").unwrap();
    git_cmd(&local, &["add", "."]);
    git_cmd(&local, &["commit", "-m", "c2"]);

    let cancel_srv = AskpassServer::start(Arc::new(FixedResponder::cancelling())).unwrap();
    let vars = cancel_srv.env(exe).vars();
    let local_c = local.clone();
    let out = tokio::task::spawn_blocking(move || {
        Command::new("git")
            .args([
                "-c",
                "credential.helper=",
                "-c",
                "http.emptyAuth=false",
                "push",
                "origin",
                "main",
            ])
            .current_dir(&local_c)
            .env("GIT_TERMINAL_PROMPT", "0")
            .env_remove("GCM_INTERACTIVE")
            .envs(vars)
            .output()
            .expect("git push cancel")
    })
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
    srv_task.abort();
}
