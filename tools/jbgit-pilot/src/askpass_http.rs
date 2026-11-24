//! Local HTTP Basic Auth + `git-http-backend` server for `--scenario askpass`.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use anyhow::{Context, Result, bail};

// Basic Auth credential pair: `alice` / `s3cr3t` -> Base64("alice:s3cr3t") == "YWxpY2U6czNjcjN0"
const ASKPASS_USER: &str = "alice";
const EXPECTED_AUTH_HEADER: &str = "Basic YWxpY2U6czNjcjN0";

/// Locates `git-http-backend` from `git --exec-path`.
pub fn find_http_backend() -> Result<PathBuf> {
    let out = Command::new("git")
        .arg("--exec-path")
        .output()
        .context("run git --exec-path")?;
    if !out.status.success() {
        bail!(
            "git --exec-path failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let exec_dir = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
    let bin = if cfg!(windows) {
        "git-http-backend.exe"
    } else {
        "git-http-backend"
    };
    let path = exec_dir.join(bin);
    if !path.is_file() {
        bail!("missing git-http-backend at {}", path.display());
    }
    Ok(path)
}

/// Ephemeral loopback Git HTTP server enforcing Basic Auth (`alice` / `s3cr3t`).
pub struct AskpassHttpServer {
    addr: SocketAddr,
    shutdown: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
}

impl AskpassHttpServer {
    pub fn start(project_root: PathBuf) -> Result<Self> {
        let backend = find_http_backend()?;
        let listener = TcpListener::bind("127.0.0.1:0").context("bind askpass http listener")?;
        let addr = listener.local_addr()?;
        let shutdown = Arc::new(AtomicBool::new(false));
        let shutdown_loop = Arc::clone(&shutdown);

        let join = thread::spawn(move || {
            for conn in listener.incoming() {
                if shutdown_loop.load(Ordering::Acquire) {
                    break;
                }
                let Ok(stream) = conn else {
                    break;
                };
                if shutdown_loop.load(Ordering::Acquire) {
                    break;
                }
                let root = project_root.clone();
                let backend_path = backend.clone();
                thread::spawn(move || {
                    handle_http_conn(stream, &root, &backend_path);
                });
            }
        });

        Ok(Self {
            addr,
            shutdown,
            join: Some(join),
        })
    }

    pub fn repo_url(&self) -> String {
        format!("http://127.0.0.1:{}/repo.git", self.addr.port())
    }
}

impl Drop for AskpassHttpServer {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        let _ = TcpStream::connect_timeout(&self.addr, Duration::from_millis(200));
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

struct HttpRequest {
    method: String,
    target: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

fn read_chunk(stream: &mut TcpStream, buf: &mut Vec<u8>) -> Option<usize> {
    let mut tmp = [0u8; 4096];
    let n = stream.read(&mut tmp).ok()?;
    if n > 0 {
        buf.extend_from_slice(&tmp[..n]);
    }
    Some(n)
}

fn write_internal_error(stream: &mut TcpStream) {
    let _ = stream.write_all(
        b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
    );
}

fn read_http_request(stream: &mut TcpStream) -> Option<HttpRequest> {
    let mut buf = Vec::with_capacity(4096);
    let header_end = loop {
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos;
        }
        if read_chunk(stream, &mut buf)? == 0 {
            return None;
        }
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
        stream.write_all(b"HTTP/1.1 100 Continue\r\n\r\n").ok()?;
        stream.flush().ok()?;
    }

    let body = if chunked {
        read_chunked_body(stream, &mut rest)?
    } else if let Some(len) = content_len {
        while rest.len() < len {
            if read_chunk(stream, &mut rest)? == 0 {
                break;
            }
        }
        rest.truncate(len);
        rest
    } else {
        Vec::new()
    };

    Some(HttpRequest {
        method,
        target,
        headers,
        body,
    })
}

fn read_chunked_body(stream: &mut TcpStream, rest: &mut Vec<u8>) -> Option<Vec<u8>> {
    let mut decoded = Vec::new();
    let mut cursor = 0usize;
    loop {
        while !rest[cursor..].windows(2).any(|w| w == b"\r\n") {
            if read_chunk(stream, rest)? == 0 {
                return None;
            }
        }
        let rel_crlf = rest[cursor..].windows(2).position(|w| w == b"\r\n")?;
        let size_line = String::from_utf8_lossy(&rest[cursor..cursor + rel_crlf]);
        let size_hex = size_line.split(';').next().unwrap_or("").trim();
        let size = usize::from_str_radix(size_hex, 16).ok()?;
        cursor += rel_crlf + 2;
        if size == 0 {
            break;
        }
        while rest.len() < cursor + size + 2 {
            if read_chunk(stream, rest)? == 0 {
                return None;
            }
        }
        decoded.extend_from_slice(&rest[cursor..cursor + size]);
        cursor += size + 2;
    }
    Some(decoded)
}

fn handle_http_conn(mut stream: TcpStream, project_root: &Path, backend: &Path) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(15)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(15)));

    let Some(req) = read_http_request(&mut stream) else {
        return;
    };

    let authed = req.headers.iter().any(|(k, v)| {
        k.eq_ignore_ascii_case("authorization") && v.trim() == EXPECTED_AUTH_HEADER
    });
    if !authed {
        let resp = concat!(
            "HTTP/1.1 401 Unauthorized\r\n",
            "WWW-Authenticate: Basic realm=\"jbgit-pilot\"\r\n",
            "Content-Length: 0\r\n",
            "Connection: close\r\n\r\n"
        );
        let _ = stream.write_all(resp.as_bytes());
        let _ = stream.flush();
        return;
    }

    let (path_info, query) = req.target.split_once('?').unwrap_or((&req.target, ""));
    let content_type = req
        .headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-type"))
        .map(|(_, v)| v.as_str())
        .unwrap_or("");

    let mut cmd = Command::new(backend);
    cmd.env("GIT_PROJECT_ROOT", project_root)
        .env("GIT_HTTP_EXPORT_ALL", "1")
        .env("REQUEST_METHOD", &req.method)
        .env("PATH_INFO", path_info)
        .env("QUERY_STRING", query)
        .env("CONTENT_TYPE", content_type)
        .env("CONTENT_LENGTH", req.body.len().to_string())
        .env("REMOTE_USER", ASKPASS_USER)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let Ok(mut child) = cmd.spawn() else {
        write_internal_error(&mut stream);
        return;
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(&req.body);
    }
    let Ok(out) = child.wait_with_output() else {
        return;
    };
    if !out.status.success() {
        write_internal_error(&mut stream);
        return;
    }

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
    let _ = stream.write_all(wire.as_bytes());
    let _ = stream.write_all(cgi_body);
    let _ = stream.flush();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sandbox::TestDir;

    #[test]
    fn http_server_requires_basic_auth_and_serves_git_info_refs() {
        let tmp = TestDir::new("askpass-http");
        let bare = tmp.0.join("repo.git");
        std::fs::create_dir_all(&bare).unwrap();
        let init = Command::new("git")
            .args(["init", "--bare", "-b", "main"])
            .current_dir(&bare)
            .output()
            .unwrap();
        assert!(init.status.success());

        let srv = AskpassHttpServer::start(tmp.0.clone()).unwrap();
        let port = srv
            .repo_url()
            .trim_start_matches("http://127.0.0.1:")
            .split('/')
            .next()
            .unwrap()
            .parse::<u16>()
            .unwrap();

        // 1. Unauthenticated request receives 401 with WWW-Authenticate: Basic.
        let mut s1 = TcpStream::connect(("127.0.0.1", port)).unwrap();
        s1.write_all(
            b"GET /repo.git/info/refs?service=git-upload-pack HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n",
        )
        .unwrap();
        let mut resp1 = String::new();
        s1.read_to_string(&mut resp1).unwrap();
        assert!(resp1.starts_with("HTTP/1.1 401 Unauthorized"), "{resp1}");
        assert!(
            resp1.contains("WWW-Authenticate: Basic"),
            "missing Basic challenge: {resp1}"
        );

        // 2. Authenticated request receives 200 OK from git-http-backend.
        let mut s2 = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let req2 = format!(
            "GET /repo.git/info/refs?service=git-upload-pack HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: {EXPECTED_AUTH_HEADER}\r\nConnection: close\r\n\r\n"
        );
        s2.write_all(req2.as_bytes()).unwrap();
        let mut resp2 = Vec::new();
        s2.read_to_end(&mut resp2).unwrap();
        let text2 = String::from_utf8_lossy(&resp2);
        assert!(text2.starts_with("HTTP/1.1 200 OK"), "{text2}");
        assert!(text2.contains("git-upload-pack"), "{text2}");
    }
}
