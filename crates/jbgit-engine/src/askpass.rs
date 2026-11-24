//! `GIT_ASKPASS` / `SSH_ASKPASS` credential bridge (Spec 12 Issue 08).
//!
//! git and ssh run the askpass program with the prompt as its only argument
//! and read the answer from its stdout. jbgit points both variables at its own
//! executable; when re-launched that way ([`client_invocation`]) the process
//! connects back to the TUI session over a local channel only the current
//! user can open, proves itself with a per-session random token, and prints
//! whatever the user typed into the credential dialog.
//!
//! Security properties:
//! - Channel: Windows named pipe with a protected DACL granting only the
//!   current user's SID, remote clients rejected; Unix domain socket inside a
//!   fresh `0700` directory, socket file `0600`. Never a TCP port.
//! - Every connection must present the 32-byte session token first
//!   (constant-time compare) or it is dropped without a reply.
//! - Secrets travel as [`Secret`] (`Zeroizing<String>`); jbgit never stores them.
//! - [`AskpassEnv`]'s `Debug` hides the token; the console only replays
//!   `git <args>`, never the environment.

use std::fmt;
use std::future::Future;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use zeroize::Zeroizing;

/// Address of the session channel (pipe name / socket path).
pub const ENV_ADDR: &str = "JBGIT_ASKPASS_ADDR";
/// Hex session token the askpass client must present.
pub const ENV_TOKEN: &str = "JBGIT_ASKPASS_TOKEN";
/// How long a single prompt may wait for the user before it is cancelled.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
/// Time allowed for a client to send its token + prompt after connecting.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_FRAME: usize = 64 * 1024;
const STATUS_CANCEL: u8 = 0;
const STATUS_OK: u8 = 1;

/// A credential value that is wiped from memory when dropped.
pub type Secret = Zeroizing<String>;

/// Environment handed to network git commands so prompts reach the TUI.
#[derive(Clone, PartialEq, Eq)]
pub struct AskpassEnv {
    exe: PathBuf,
    addr: String,
    token: String,
}

impl AskpassEnv {
    pub fn new(exe: PathBuf, addr: impl Into<String>, token: impl Into<String>) -> Self {
        Self {
            exe,
            addr: addr.into(),
            token: token.into(),
        }
    }

    pub fn addr(&self) -> &str {
        &self.addr
    }

    /// `GIT_ASKPASS`, `SSH_ASKPASS`, `SSH_ASKPASS_REQUIRE=force` plus the
    /// session address / token (`GIT_TERMINAL_PROMPT=0` stays in force).
    pub fn vars(&self) -> Vec<(String, String)> {
        let exe = self.exe.to_string_lossy().into_owned();
        vec![
            ("GIT_ASKPASS".to_string(), exe.clone()),
            ("SSH_ASKPASS".to_string(), exe),
            ("SSH_ASKPASS_REQUIRE".to_string(), "force".to_string()),
            (ENV_ADDR.to_string(), self.addr.clone()),
            (ENV_TOKEN.to_string(), self.token.clone()),
        ]
    }
}

impl fmt::Debug for AskpassEnv {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AskpassEnv")
            .field("exe", &self.exe)
            .field("addr", &self.addr)
            .field("token", &"<redacted>")
            .finish()
    }
}

/// Returns `true` when `prompt` is an OpenSSH first-connection host-key
/// confirmation prompt (`Are you sure you want to continue connecting (yes/no/[fingerprint])?`).
/// Such prompts must be displayed unmasked with the full fingerprint text.
pub fn is_ssh_host_key_prompt(prompt: &str) -> bool {
    let lower = prompt.to_ascii_lowercase();
    lower.contains("are you sure you want to continue connecting")
        || (lower.contains("authenticity of host") && lower.contains("fingerprint"))
        || (lower.contains("host key") && lower.contains("(yes/no"))
}

/// Whether the answer to `prompt` should be masked while typed
/// (`Password for …`, `Enter passphrase for key …`, tokens, PINs).
pub fn is_secret_prompt(prompt: &str) -> bool {
    if is_ssh_host_key_prompt(prompt) {
        return false;
    }
    let lower = prompt.to_ascii_lowercase();
    ["password", "passphrase", "token"]
        .iter()
        .any(|k| lower.contains(k))
        || lower
            .split(|c: char| !c.is_ascii_alphanumeric())
            .any(|w| w == "pin")
}

/// `true` when `args` is the command line git / ssh use to run an askpass
/// helper (a single prompt argument) and the session variables are present.
pub fn client_invocation(args: &[String]) -> Option<&str> {
    if args.len() != 1 {
        return None;
    }
    let has_env = std::env::var_os(ENV_ADDR).is_some() && std::env::var_os(ENV_TOKEN).is_some();
    has_env.then(|| args[0].as_str())
}

// ---------------------------------------------------------------------------
// Responders
// ---------------------------------------------------------------------------

/// One pending prompt forwarded to the UI. Dropping it cancels the prompt.
pub struct AskpassRequest {
    pub prompt: String,
    pub masked: bool,
    /// `true` when `prompt` is an OpenSSH host-key verification prompt (unmasked,
    /// renders full multi-line fingerprint and guides `yes`/`no`).
    pub host_key_confirm: bool,
    reply: oneshot::Sender<Option<Secret>>,
}

impl AskpassRequest {
    pub fn new(prompt: String) -> (Self, oneshot::Receiver<Option<Secret>>) {
        let (tx, rx) = oneshot::channel();
        let host_key_confirm = is_ssh_host_key_prompt(&prompt);
        let masked = !host_key_confirm && is_secret_prompt(&prompt);
        (
            Self {
                prompt,
                masked,
                host_key_confirm,
                reply: tx,
            },
            rx,
        )
    }

    /// Sends the user's answer (`None` = cancelled).
    pub fn respond(self, answer: Option<Secret>) {
        let _ = self.reply.send(answer);
    }

    /// The git process stopped waiting (timeout / exit).
    pub fn is_abandoned(&self) -> bool {
        self.reply.is_closed()
    }
}

impl fmt::Debug for AskpassRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AskpassRequest")
            .field("prompt", &self.prompt)
            .field("masked", &self.masked)
            .finish_non_exhaustive()
    }
}

/// Future returned by [`AskpassResponder::respond`].
pub type ResponseFuture = Pin<Box<dyn Future<Output = Option<Secret>> + Send>>;

/// Answers credential prompts (TUI dialog in production, fixed values in tests).
pub trait AskpassResponder: Send + Sync + 'static {
    fn respond(&self, prompt: String) -> ResponseFuture;
}

/// Production responder: forwards each prompt to the UI as an [`AskpassRequest`].
pub struct ChannelResponder {
    tx: mpsc::UnboundedSender<AskpassRequest>,
}

impl ChannelResponder {
    pub fn new(tx: mpsc::UnboundedSender<AskpassRequest>) -> Self {
        Self { tx }
    }
}

impl AskpassResponder for ChannelResponder {
    fn respond(&self, prompt: String) -> ResponseFuture {
        let (req, rx) = AskpassRequest::new(prompt);
        let sent = self.tx.send(req).is_ok();
        Box::pin(async move {
            if !sent {
                return None;
            }
            rx.await.ok().flatten()
        })
    }
}

/// Deterministic responder for tests / the pilot sandbox: always answers
/// `answer` (or always cancels) and records the prompts it saw.
#[derive(Default)]
pub struct FixedResponder {
    answer: Option<String>,
    prompts: Mutex<Vec<String>>,
}

impl FixedResponder {
    pub fn answering(answer: impl Into<String>) -> Self {
        Self {
            answer: Some(answer.into()),
            prompts: Mutex::new(Vec::new()),
        }
    }

    pub fn cancelling() -> Self {
        Self::default()
    }

    pub fn prompts(&self) -> Vec<String> {
        self.prompts.lock().map(|p| p.clone()).unwrap_or_default()
    }
}

impl AskpassResponder for FixedResponder {
    fn respond(&self, prompt: String) -> ResponseFuture {
        if let Ok(mut p) = self.prompts.lock() {
            p.push(prompt);
        }
        let answer = self.answer.clone().map(Zeroizing::new);
        Box::pin(async move { answer })
    }
}

// ---------------------------------------------------------------------------
// Server
// ---------------------------------------------------------------------------

/// Session-scoped askpass endpoint. Dropping it stops accepting connections
/// and removes the socket directory (Unix).
pub struct AskpassServer {
    addr: String,
    token: Zeroizing<String>,
    task: JoinHandle<()>,
    #[cfg(unix)]
    dir: PathBuf,
}

impl AskpassServer {
    /// Opens the channel and starts accepting clients. Must be called from
    /// inside a Tokio runtime.
    pub fn start(responder: Arc<dyn AskpassResponder>) -> io::Result<Self> {
        let token = Zeroizing::new(random_hex(32)?);
        let tag = random_hex(8)?;
        platform::start(responder, token, &tag)
    }

    pub fn addr(&self) -> &str {
        &self.addr
    }

    /// Environment for git subprocesses using `exe` as the askpass program.
    pub fn env(&self, exe: PathBuf) -> AskpassEnv {
        AskpassEnv::new(exe, self.addr.clone(), self.token.as_str())
    }

    /// [`Self::env`] for the running `jbgit` executable.
    pub fn env_for_current_exe(&self) -> io::Result<AskpassEnv> {
        Ok(self.env(std::env::current_exe()?))
    }
}

impl Drop for AskpassServer {
    fn drop(&mut self) {
        self.task.abort();
        #[cfg(unix)]
        {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
}

impl fmt::Debug for AskpassServer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AskpassServer")
            .field("addr", &self.addr)
            .finish_non_exhaustive()
    }
}

fn random_hex(bytes: usize) -> io::Result<String> {
    let mut buf = Zeroizing::new(vec![0u8; bytes]);
    getrandom::fill(&mut buf).map_err(|e| io::Error::other(e.to_string()))?;
    let mut out = String::with_capacity(bytes * 2);
    for b in buf.iter() {
        out.push_str(&format!("{b:02x}"));
    }
    Ok(out)
}

fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

async fn read_frame_async<S: AsyncRead + Unpin>(s: &mut S) -> io::Result<Zeroizing<Vec<u8>>> {
    let len = s.read_u32().await? as usize;
    if len > MAX_FRAME {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "frame too large",
        ));
    }
    let mut buf = Zeroizing::new(vec![0u8; len]);
    s.read_exact(&mut buf).await?;
    Ok(buf)
}

/// Serves one client: token check → prompt → responder → reply.
async fn serve_connection<S>(mut stream: S, token: &str, responder: &dyn AskpassResponder)
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let handshake = async {
        let presented = read_frame_async(&mut stream).await?;
        if !ct_eq(&presented, token.as_bytes()) {
            return Err(io::Error::new(io::ErrorKind::PermissionDenied, "bad token"));
        }
        let prompt = read_frame_async(&mut stream).await?;
        Ok::<_, io::Error>(String::from_utf8_lossy(&prompt).trim().to_string())
    };
    let Ok(Ok(prompt)) = tokio::time::timeout(HANDSHAKE_TIMEOUT, handshake).await else {
        return;
    };
    let answer = tokio::time::timeout(REQUEST_TIMEOUT, responder.respond(prompt))
        .await
        .ok()
        .flatten();
    let _ = match answer {
        Some(secret) => {
            let mut msg = Zeroizing::new(Vec::with_capacity(secret.len() + 5));
            msg.push(STATUS_OK);
            msg.extend_from_slice(&(secret.len() as u32).to_be_bytes());
            msg.extend_from_slice(secret.as_bytes());
            stream.write_all(&msg).await
        }
        None => stream.write_all(&[STATUS_CANCEL]).await,
    };
    let _ = stream.flush().await;
    let _ = stream.shutdown().await;
}

// ---------------------------------------------------------------------------
// Client (`jbgit <prompt>` launched by git / ssh)
// ---------------------------------------------------------------------------

/// Askpass entry point: asks the session behind `JBGIT_ASKPASS_ADDR` and
/// prints the answer. Returns the process exit code (1 = cancelled / error).
pub fn run_client(prompt: &str) -> i32 {
    let (Ok(addr), Ok(token)) = (std::env::var(ENV_ADDR), std::env::var(ENV_TOKEN)) else {
        return 1;
    };
    let token = Zeroizing::new(token);
    match request_credential(&addr, &token, prompt) {
        Ok(Some(secret)) => {
            let mut out = io::stdout().lock();
            let mut line = Zeroizing::new(String::with_capacity(secret.len() + 1));
            line.push_str(&secret);
            line.push('\n');
            if out
                .write_all(line.as_bytes())
                .and_then(|()| out.flush())
                .is_ok()
            {
                0
            } else {
                1
            }
        }
        _ => 1,
    }
}

/// Synchronous client used by [`run_client`]: `Ok(None)` when the user cancelled.
pub fn request_credential(addr: &str, token: &str, prompt: &str) -> io::Result<Option<Secret>> {
    let mut conn = platform::connect(addr)?;
    let mut msg = Vec::with_capacity(token.len() + prompt.len() + 8);
    msg.extend_from_slice(&(token.len() as u32).to_be_bytes());
    msg.extend_from_slice(token.as_bytes());
    let prompt = prompt.replace(['\r', '\n'], " ");
    msg.extend_from_slice(&(prompt.len() as u32).to_be_bytes());
    msg.extend_from_slice(prompt.as_bytes());
    conn.write_all(&msg)?;
    conn.flush()?;
    let mut status = [0u8; 1];
    conn.read_exact(&mut status)?;
    if status[0] != STATUS_OK {
        return Ok(None);
    }
    let mut len = [0u8; 4];
    conn.read_exact(&mut len)?;
    let len = u32::from_be_bytes(len) as usize;
    if len > MAX_FRAME {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "frame too large",
        ));
    }
    let mut buf = Zeroizing::new(vec![0u8; len]);
    conn.read_exact(&mut buf)?;
    let text = std::str::from_utf8(&buf)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "answer is not UTF-8"))?;
    Ok(Some(Zeroizing::new(text.to_string())))
}

#[allow(dead_code)]
fn socket_path_in(dir: &Path) -> PathBuf {
    dir.join("askpass.sock")
}

#[cfg(windows)]
mod platform {
    use std::ffi::c_void;
    use std::fs::{File, OpenOptions};
    use std::io;
    use std::sync::Arc;
    use std::time::Duration;

    use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, LocalFree};
    use windows_sys::Win32::Security::Authorization::{
        ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
        SDDL_REVISION_1,
    };
    use windows_sys::Win32::Security::{
        GetTokenInformation, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER,
        TokenUser,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    use zeroize::Zeroizing;

    use super::{AskpassResponder, AskpassServer, serve_connection};

    const ERROR_PIPE_BUSY: i32 = 231;

    /// Owner-only security descriptor for the pipe (`D:P(A;;GA;;;<user SID>)`).
    pub(super) struct OwnerOnlySecurity {
        sd: PSECURITY_DESCRIPTOR,
        attrs: SECURITY_ATTRIBUTES,
    }

    // The descriptor is immutable after construction and only read by
    // `CreateNamedPipeW`.
    unsafe impl Send for OwnerOnlySecurity {}
    unsafe impl Sync for OwnerOnlySecurity {}

    impl OwnerOnlySecurity {
        pub(super) fn new() -> io::Result<Self> {
            let sddl = owner_only_sddl(&current_user_sid()?);
            let wide: Vec<u16> = sddl.encode_utf16().chain(std::iter::once(0)).collect();
            let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
            // SAFETY: `wide` is NUL-terminated; `sd` receives a LocalAlloc'd
            // descriptor freed in `Drop`.
            let ok = unsafe {
                ConvertStringSecurityDescriptorToSecurityDescriptorW(
                    wide.as_ptr(),
                    SDDL_REVISION_1,
                    &mut sd,
                    std::ptr::null_mut(),
                )
            };
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(Self {
                sd,
                attrs: SECURITY_ATTRIBUTES {
                    nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
                    lpSecurityDescriptor: sd,
                    bInheritHandle: 0,
                },
            })
        }

        fn as_ptr(&self) -> *mut c_void {
            (&raw const self.attrs).cast_mut().cast()
        }
    }

    impl Drop for OwnerOnlySecurity {
        fn drop(&mut self) {
            // SAFETY: `sd` came from ConvertStringSecurityDescriptor…W.
            unsafe {
                LocalFree(self.sd);
            }
        }
    }

    /// Protected DACL with a single GENERIC_ALL ACE for `sid`.
    pub(super) fn owner_only_sddl(sid: &str) -> String {
        format!("D:P(A;;GA;;;{sid})")
    }

    /// String SID of the user owning the current process token.
    pub(super) fn current_user_sid() -> io::Result<String> {
        // SAFETY: plain Win32 token queries on the current process; every
        // handle / buffer is released before returning.
        unsafe {
            let mut token: HANDLE = std::ptr::null_mut();
            if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
                return Err(io::Error::last_os_error());
            }
            let mut len = 0u32;
            GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut len);
            let mut buf = vec![0u64; (len as usize).div_ceil(8).max(1)];
            let ok = GetTokenInformation(token, TokenUser, buf.as_mut_ptr().cast(), len, &mut len);
            CloseHandle(token);
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            let user = &*(buf.as_ptr().cast::<TOKEN_USER>());
            let mut wide: *mut u16 = std::ptr::null_mut();
            if ConvertSidToStringSidW(user.User.Sid, &mut wide) == 0 {
                return Err(io::Error::last_os_error());
            }
            let mut n = 0usize;
            while *wide.add(n) != 0 {
                n += 1;
            }
            let sid = String::from_utf16_lossy(std::slice::from_raw_parts(wide, n));
            LocalFree(wide.cast());
            Ok(sid)
        }
    }

    fn create_instance(
        addr: &str,
        security: &OwnerOnlySecurity,
        first: bool,
    ) -> io::Result<NamedPipeServer> {
        let mut opts = ServerOptions::new();
        opts.first_pipe_instance(first).reject_remote_clients(true);
        // SAFETY: `security.as_ptr()` points at a valid SECURITY_ATTRIBUTES
        // that outlives the call.
        unsafe { opts.create_with_security_attributes_raw(addr, security.as_ptr()) }
    }

    pub(super) fn start(
        responder: Arc<dyn AskpassResponder>,
        token: Zeroizing<String>,
        tag: &str,
    ) -> io::Result<AskpassServer> {
        let addr = format!(r"\\.\pipe\jbgit-askpass-{}-{tag}", std::process::id());
        let security = Arc::new(OwnerOnlySecurity::new()?);
        let first = create_instance(&addr, &security, true)?;
        let task_addr = addr.clone();
        let task_token = token.clone();
        let task = tokio::spawn(async move {
            let mut server = first;
            loop {
                if server.connect().await.is_err() {
                    match create_instance(&task_addr, &security, false) {
                        Ok(next) => {
                            server = next;
                            continue;
                        }
                        Err(_) => return,
                    }
                }
                let Ok(next) = create_instance(&task_addr, &security, false) else {
                    return;
                };
                let connected = std::mem::replace(&mut server, next);
                let responder = Arc::clone(&responder);
                let token = task_token.clone();
                tokio::spawn(async move {
                    serve_connection(connected, &token, responder.as_ref()).await;
                });
            }
        });
        Ok(AskpassServer { addr, token, task })
    }

    pub(super) fn connect(addr: &str) -> io::Result<File> {
        let mut attempts = 0;
        loop {
            match OpenOptions::new().read(true).write(true).open(addr) {
                Ok(f) => return Ok(f),
                Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY) && attempts < 40 => {
                    attempts += 1;
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(e) => return Err(e),
            }
        }
    }
}

#[cfg(unix)]
mod platform {
    use std::io;
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    use std::os::unix::net::UnixStream;
    use std::path::PathBuf;
    use std::sync::Arc;

    use tokio::net::UnixListener;
    use zeroize::Zeroizing;

    use super::{
        AskpassResponder, AskpassServer, REQUEST_TIMEOUT, serve_connection, socket_path_in,
    };

    fn runtime_base() -> PathBuf {
        std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .filter(|p| p.is_dir())
            .unwrap_or_else(std::env::temp_dir)
    }

    pub(super) fn start(
        responder: Arc<dyn AskpassResponder>,
        token: Zeroizing<String>,
        tag: &str,
    ) -> io::Result<AskpassServer> {
        let dir = runtime_base().join(format!("jbgit-askpass-{}-{tag}", std::process::id()));
        std::fs::DirBuilder::new().mode(0o700).create(&dir)?;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
        let path = socket_path_in(&dir);
        let listener = match UnixListener::bind(&path) {
            Ok(l) => l,
            Err(e) => {
                let _ = std::fs::remove_dir_all(&dir);
                return Err(e);
            }
        };
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        let task_token = token.clone();
        let task = tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                let responder = Arc::clone(&responder);
                let token = task_token.clone();
                tokio::spawn(async move {
                    serve_connection(stream, &token, responder.as_ref()).await;
                });
            }
        });
        Ok(AskpassServer {
            addr: path.to_string_lossy().into_owned(),
            token,
            task,
            dir,
        })
    }

    pub(super) fn connect(addr: &str) -> io::Result<UnixStream> {
        let stream = UnixStream::connect(addr)?;
        stream.set_read_timeout(Some(REQUEST_TIMEOUT + std::time::Duration::from_secs(10)))?;
        Ok(stream)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_prompt_detection_and_env_redaction() {
        assert!(is_secret_prompt("Password for 'https://u@h': "));
        assert!(is_secret_prompt(
            "Enter passphrase for key '/home/u/.ssh/id': "
        ));
        assert!(is_secret_prompt("Personal access token: "));
        assert!(is_secret_prompt("Enter PIN for 'yubikey': "));
        assert!(!is_secret_prompt(
            "Username for 'https://pinterest.example': "
        ));
        assert!(!is_secret_prompt("Username for 'https://h': "));
        let env = AskpassEnv::new(PathBuf::from("/bin/jbgit"), "addr", "deadbeef");
        assert!(!format!("{env:?}").contains("deadbeef"));
        let vars = env.vars();
        let get = |k: &str| vars.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str());
        assert_eq!(get("GIT_ASKPASS"), Some("/bin/jbgit"));
        assert_eq!(get("SSH_ASKPASS"), Some("/bin/jbgit"));
        assert_eq!(get("SSH_ASKPASS_REQUIRE"), Some("force"));
        assert_eq!(get(ENV_ADDR), Some("addr"));
        assert_eq!(get(ENV_TOKEN), Some("deadbeef"));
        assert!(ct_eq(b"abc", b"abc") && !ct_eq(b"abc", b"abd") && !ct_eq(b"ab", b"abc"));
        assert_eq!(random_hex(32).unwrap().len(), 64);
    }

    #[test]
    fn ssh_host_key_prompt_is_detected_and_unmasked() {
        let prompt = "The authenticity of host 'github.com (140.82.121.3)' can't be established.\n\
ED25519 key fingerprint is SHA256:+DiY3wvvV6TuJJhbpZisF/zLDA0zPMSvHdkr4UvCOqU.\n\
Are you sure you want to continue connecting (yes/no/[fingerprint])? ";
        assert!(is_ssh_host_key_prompt(prompt));
        assert!(!is_secret_prompt(prompt));
        assert!(!is_ssh_host_key_prompt(
            "Enter passphrase for key '/home/u/.ssh/id_ed25519': "
        ));

        let (req, _rx) = AskpassRequest::new(prompt.to_string());
        assert!(req.host_key_confirm);
        assert!(!req.masked);
    }

    async fn ask(addr: String, token: String, prompt: &'static str) -> io::Result<Option<Secret>> {
        tokio::task::spawn_blocking(move || request_credential(&addr, &token, prompt))
            .await
            .expect("join")
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn round_trip_answers_cancels_and_rejects_bad_tokens() {
        let fixed = Arc::new(FixedResponder::answering("s3cret"));
        let server = AskpassServer::start(fixed.clone()).expect("server");
        let env = server.env(PathBuf::from("jbgit"));
        let token = env
            .vars()
            .into_iter()
            .find(|(k, _)| k == ENV_TOKEN)
            .map(|(_, v)| v)
            .unwrap();
        let addr = server.addr().to_string();

        let got = ask(addr.clone(), token.clone(), "Password for 'https://h': ")
            .await
            .expect("io");
        assert_eq!(got.as_deref().map(String::as_str), Some("s3cret"));
        assert_eq!(
            fixed.prompts(),
            vec!["Password for 'https://h':".to_string()]
        );

        // Wrong token: the server hangs up without answering or asking.
        let bad = ask(addr.clone(), "0".repeat(64), "Username: ").await;
        assert!(bad.is_err(), "bad token must not get an answer: {bad:?}");
        assert_eq!(fixed.prompts().len(), 1);

        // Several concurrent clients are all served.
        let joins: Vec<_> = (0..3)
            .map(|_| tokio::spawn(ask(addr.clone(), token.clone(), "Username: ")))
            .collect();
        for j in joins {
            let r = j.await.unwrap().unwrap();
            assert_eq!(r.as_deref().map(String::as_str), Some("s3cret"));
        }
        drop(server);

        let cancel = AskpassServer::start(Arc::new(FixedResponder::cancelling())).unwrap();
        let env = cancel.env(PathBuf::from("jbgit"));
        let token = env
            .vars()
            .into_iter()
            .find(|(k, _)| k == ENV_TOKEN)
            .unwrap()
            .1;
        let got = ask(cancel.addr().to_string(), token, "Password: ")
            .await
            .unwrap();
        assert!(got.is_none());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn channel_responder_forwards_requests_fifo() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let server = AskpassServer::start(Arc::new(ChannelResponder::new(tx))).unwrap();
        let token = server
            .env(PathBuf::from("x"))
            .vars()
            .into_iter()
            .find(|(k, _)| k == ENV_TOKEN)
            .unwrap()
            .1;
        let pending = tokio::spawn(ask(server.addr().to_string(), token, "Password for 'h': "));
        let req = rx.recv().await.expect("request");
        assert!(req.masked);
        assert_eq!(req.prompt, "Password for 'h':");
        assert!(!format!("{req:?}").contains("reply"));
        req.respond(Some(Zeroizing::new("pw".into())));
        let got = pending.await.unwrap().unwrap();
        assert_eq!(got.as_deref().map(String::as_str), Some("pw"));
    }

    #[cfg(windows)]
    #[test]
    fn pipe_dacl_grants_only_the_current_user() {
        let sid = platform::current_user_sid().expect("sid");
        assert!(sid.starts_with("S-1-5-"), "{sid}");
        assert_eq!(
            platform::owner_only_sddl(&sid),
            format!("D:P(A;;GA;;;{sid})")
        );
        assert!(platform::OwnerOnlySecurity::new().is_ok());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn socket_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let server = AskpassServer::start(Arc::new(FixedResponder::cancelling())).unwrap();
        let sock = PathBuf::from(server.addr());
        let mode = std::fs::metadata(&sock).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let dir = sock.parent().unwrap().to_path_buf();
        let dmode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(dmode, 0o700);
        drop(server);
        assert!(!dir.exists());
    }
}
