use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::Stdio;
use std::time::Instant;

use chrono::Local;
use jbgit_core::{ConsoleStream, GitConsoleEntry};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWriteExt, BufReader};
use tokio::process::Command;

use crate::error::EngineError;

const PATHSPEC_FROM_FILE_THRESHOLD: usize = 32;

/// Structured `index.lock` conflict surfaced by any CLI-backed operation so
/// the supervisor can emit a uniform [`crate::EngineEvent::IndexLockDetected`].
#[derive(Debug, Clone)]
pub(crate) struct IndexLockConflict {
    pub lock_path: PathBuf,
    pub message: String,
    pub retry_args: Vec<String>,
    pub retry_stdin: Option<Vec<u8>>,
    pub retry_env: Vec<(String, String)>,
    pub retry_is_read_only: bool,
}

/// Narrow subprocess seam between `jbgit-engine` command compilers (`ops`, `stage`)
/// and the underlying `git` executable (real subprocess in production vs
/// deterministic recording double in tests).
pub(crate) trait GitCliRunner: Send + Sync {
    fn execute_boxed<'a>(
        &'a self,
        req: CliCommand<'a>,
        on_event: &'a (dyn Fn(CliStreamEvent) + Send + Sync),
    ) -> Pin<Box<dyn Future<Output = Result<GitCliOutput, EngineError>> + Send + 'a>>;
}

impl GitCliRunner for GitCliExecutor {
    fn execute_boxed<'a>(
        &'a self,
        req: CliCommand<'a>,
        on_event: &'a (dyn Fn(CliStreamEvent) + Send + Sync),
    ) -> Pin<Box<dyn Future<Output = Result<GitCliOutput, EngineError>> + Send + 'a>> {
        Box::pin(async move { self.execute(req, on_event).await })
    }
}

/// Single canonical non-interactive `std::process::Command` constructor for `git`
/// (used by both `GitCliExecutor::detect` and synchronous read-only helpers in `details`).
pub(crate) fn non_interactive_std_command(git: &Path, cwd: Option<&Path>) -> std::process::Command {
    let mut cmd = non_interactive_std_write_command(git, cwd);
    cmd.env("GIT_OPTIONAL_LOCKS", "0");
    cmd
}

/// Non-interactive `std::process::Command` for mutating helpers (omits `GIT_OPTIONAL_LOCKS=0`).
pub(crate) fn non_interactive_std_write_command(
    git: &Path,
    cwd: Option<&Path>,
) -> std::process::Command {
    let mut cmd = std::process::Command::new(git);
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    cmd.env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_PAGER", "cat")
        .env("LC_ALL", "C.UTF-8")
        .stdin(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    cmd
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitExecutableInfo {
    pub path: PathBuf,
    pub version_string: String,
}

#[derive(Debug, Clone)]
pub struct GitCliOutput {
    pub exit_code: i32,
    #[allow(dead_code)]
    pub duration_ms: u64,
    pub stdout_lines: Vec<String>,
    pub stderr_lines: Vec<String>,
    pub index_lock_conflict: Option<(PathBuf, String)>,
}

/// Builds git CLI arguments for path lists, automatically switching to
/// `--pathspec-from-file=- --pathspec-file-nul` when more than 32 paths are passed.
pub fn prepare_pathspec_args(
    mut base_args: Vec<String>,
    paths: &[String],
) -> (Vec<String>, Option<Vec<u8>>) {
    if paths.is_empty() {
        return (base_args, None);
    }

    if paths.len() > PATHSPEC_FROM_FILE_THRESHOLD {
        base_args.push("--pathspec-from-file=-".to_string());
        base_args.push("--pathspec-file-nul".to_string());
        let mut payload = Vec::new();
        for p in paths {
            payload.extend_from_slice(p.as_bytes());
            payload.push(0);
        }
        (base_args, Some(payload))
    } else {
        base_args.push("--".to_string());
        base_args.extend(paths.iter().cloned());
        (base_args, None)
    }
}

#[derive(Debug, Clone)]
pub struct GitCliExecutor {
    git_bin: PathBuf,
    version_string: String,
}

#[derive(Debug, Clone, Copy)]
pub struct CliCommand<'a> {
    pub id: u64,
    pub workdir: &'a Path,
    pub git_dir: &'a Path,
    pub args: &'a [String],
    pub stdin_payload: Option<&'a [u8]>,
    pub is_read_only: bool,
    /// Extra environment variables (`GIT_SEQUENCE_EDITOR`, `GIT_EDITOR`, …).
    pub env: &'a [(String, String)],
}

impl GitCliExecutor {
    #[allow(dead_code)]
    pub fn detect() -> Result<Self, EngineError> {
        let configured = configured_git_executable();
        Self::detect_with_executable(configured.as_deref())
    }

    pub fn detect_with_executable(configured: Option<&str>) -> Result<Self, EngineError> {
        let info = detect_git_executable(configured)?;
        Ok(Self {
            git_bin: info.path,
            version_string: info.version_string,
        })
    }

    pub fn info(&self) -> GitExecutableInfo {
        GitExecutableInfo {
            path: self.git_bin.clone(),
            version_string: self.version_string.clone(),
        }
    }

    /// Executes a git CLI command asynchronously, streaming `stdout` and `stderr`
    /// lines to `on_event` and isolating environment variables (`GIT_OPTIONAL_LOCKS=0`
    /// for read-only commands, `GIT_PAGER=cat`, `GIT_TERMINAL_PROMPT=0`, `LC_ALL=C.UTF-8`).
    pub async fn execute<F>(
        &self,
        req: CliCommand<'_>,
        mut on_event: F,
    ) -> Result<GitCliOutput, EngineError>
    where
        F: FnMut(CliStreamEvent) + Send,
    {
        let CliCommand {
            id,
            workdir,
            git_dir,
            args,
            stdin_payload,
            is_read_only,
            env,
        } = req;
        let started_at = Local::now();
        let command_line = format!("git {}", format_args_for_display(args));
        let start = Instant::now();

        on_event(CliStreamEvent::Started(GitConsoleEntry {
            id,
            started_at,
            duration_ms: None,
            command_line,
            output_lines: Vec::new(),
            exit_code: None,
        }));

        let mut cmd = Command::new(&self.git_bin);
        cmd.args(args)
            .current_dir(workdir)
            .env("GIT_PAGER", "cat")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("LC_ALL", "C.UTF-8")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        if is_read_only {
            cmd.env("GIT_OPTIONAL_LOCKS", "0");
        }
        for (k, v) in env {
            cmd.env(k, v);
        }

        if stdin_payload.is_some() {
            cmd.stdin(Stdio::piped());
        } else {
            cmd.stdin(Stdio::null());
        }

        #[cfg(windows)]
        {
            cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }

        let mut child = cmd.spawn()?;
        #[cfg(windows)]
        let _job_guard = child.raw_handle().and_then(win_job::WindowsJobGuard::assign);

        // Write stdin concurrently with reading stdout/stderr to avoid OS pipe buffer deadlocks.
        let stdin_writer = AbortOnDrop(
            if let Some(payload) = stdin_payload
                && let Some(mut stdin) = child.stdin.take()
            {
                let owned = payload.to_vec();
                Some(tokio::spawn(async move {
                    let _ = stdin.write_all(&owned).await;
                    let _ = stdin.shutdown().await;
                }))
            } else {
                None
            },
        );

        let mut stdout_reader = child.stdout.take().map(BufReader::new);
        let mut stderr_reader = child.stderr.take().map(BufReader::new);

        let mut stdout_buf = Vec::new();
        let mut stderr_state = StderrLineState::default();
        let mut stdout_lines = Vec::new();
        let mut stderr_lines: Vec<String> = Vec::new();
        let mut stdout_done = stdout_reader.is_none();
        let mut stderr_done = stderr_reader.is_none();

        while !stdout_done || !stderr_done {
            tokio::select! {
                res = next_lf_line_lossy(&mut stdout_reader, &mut stdout_buf), if !stdout_done => {
                    match res {
                        Some(line) => {
                            on_event(CliStreamEvent::Output {
                                id,
                                stream: ConsoleStream::Stdout,
                                line: jbgit_core::redact_urls_in(line.trim_end_matches('\r')),
                            });
                            stdout_lines.push(line);
                        }
                        None => stdout_done = true,
                    }
                }
                res = next_cr_or_lf_segment(&mut stderr_reader, &mut stderr_state), if !stderr_done => {
                    match res {
                        Some(StderrSegment { line, overwrites_prev }) => {
                            let redacted = jbgit_core::redact_urls_in(&line);
                            let out_line = if overwrites_prev {
                                if !line.is_empty() {
                                    if let Some(last) = stderr_lines.last_mut() {
                                        *last = line;
                                    } else {
                                        stderr_lines.push(line);
                                    }
                                }
                                format!("\r{redacted}")
                            } else {
                                stderr_lines.push(line);
                                redacted
                            };
                            on_event(CliStreamEvent::Output {
                                id,
                                stream: ConsoleStream::Stderr,
                                line: out_line,
                            });
                        }
                        None => stderr_done = true,
                    }
                }
            }
        }

        stdin_writer.finish().await;
        let status = child.wait().await?;
        let exit_code = status.code().unwrap_or(-1);
        let duration_ms = start.elapsed().as_millis() as u64;

        let index_lock_conflict = detect_index_lock_conflict(&stderr_lines, git_dir);

        on_event(CliStreamEvent::Finished {
            id,
            exit_code,
            duration_ms,
        });

        Ok(GitCliOutput {
            exit_code,
            duration_ms,
            stdout_lines,
            stderr_lines,
            index_lock_conflict,
        })
    }
}

#[derive(Debug, Clone)]
pub enum CliStreamEvent {
    Started(GitConsoleEntry),
    Output {
        id: u64,
        stream: ConsoleStream,
        line: String,
    },
    Finished {
        id: u64,
        exit_code: i32,
        duration_ms: u64,
    },
}

struct AbortOnDrop(Option<tokio::task::JoinHandle<()>>);

impl AbortOnDrop {
    async fn finish(mut self) {
        if let Some(handle) = self.0.take() {
            let _ = handle.await;
        }
    }
}

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        if let Some(handle) = self.0.take() {
            handle.abort();
        }
    }
}

#[cfg(windows)]
mod win_job {
    use std::ffi::c_void;
    use std::mem::{size_of, zeroed};
    use std::ptr::null_mut;

    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};

    const JOB_OBJECT_EXTENDED_LIMIT_INFORMATION_CLASS: u32 = 9;
    const JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE: u32 = 0x0000_2000;

    #[repr(C)]
    struct JobObjectBasicLimitInformation {
        per_process_user_time_limit: i64,
        per_job_user_time_limit: i64,
        limit_flags: u32,
        minimum_working_set_size: usize,
        maximum_working_set_size: usize,
        active_process_limit: u32,
        affinity: usize,
        priority_class: u32,
        scheduling_class: u32,
    }

    #[repr(C)]
    struct IoCounters {
        read_operation_count: u64,
        write_operation_count: u64,
        other_operation_count: u64,
        read_transfer_count: u64,
        write_transfer_count: u64,
        other_transfer_count: u64,
    }

    #[repr(C)]
    struct JobObjectExtendedLimitInformation {
        basic_limit_information: JobObjectBasicLimitInformation,
        io_info: IoCounters,
        process_memory_limit: usize,
        job_memory_limit: usize,
        peak_process_memory_used: usize,
        peak_job_memory_used: usize,
    }

    unsafe extern "system" {
        fn CreateJobObjectW(lp_job_attributes: *const c_void, lp_name: *const u16) -> HANDLE;
        fn SetInformationJobObject(
            h_job: HANDLE,
            job_object_information_class: u32,
            lp_job_object_information: *const c_void,
            cb_job_object_information_length: u32,
        ) -> i32;
        fn AssignProcessToJobObject(h_job: HANDLE, h_process: HANDLE) -> i32;
    }

    /// Binds a spawned Windows child process (and all grandchildren) to a Job
    /// Object configured with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`, so dropping
    /// the executing future terminates the entire process tree.
    pub(super) struct WindowsJobGuard {
        handle: HANDLE,
    }

    // Kernel Job Object HANDLEs are process-wide and safe to close from any worker thread.
    unsafe impl Send for WindowsJobGuard {}
    unsafe impl Sync for WindowsJobGuard {}

    impl WindowsJobGuard {
        pub(super) fn assign(process_handle: *mut c_void) -> Option<Self> {
            unsafe {
                let job = CreateJobObjectW(null_mut(), null_mut());
                if job.is_null() {
                    return None;
                }
                let mut info: JobObjectExtendedLimitInformation = zeroed();
                info.basic_limit_information.limit_flags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                let ok_set = SetInformationJobObject(
                    job,
                    JOB_OBJECT_EXTENDED_LIMIT_INFORMATION_CLASS,
                    (&raw const info).cast(),
                    size_of::<JobObjectExtendedLimitInformation>() as u32,
                );
                if ok_set == 0 {
                    CloseHandle(job);
                    return None;
                }
                let ok_assign = AssignProcessToJobObject(job, process_handle);
                if ok_assign == 0 {
                    CloseHandle(job);
                    return None;
                }
                Some(Self { handle: job })
            }
        }
    }

    impl Drop for WindowsJobGuard {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.handle);
            }
        }
    }
}

/// Cancellation-safe raw-byte stdout line reader.
///
/// Reads raw bytes up to `\n` (never failing on non-UTF-8 sequences so OS stdout
/// pipes cannot deadlock or truncate) and strips only `\n`, preserving any `\r`
/// content bytes needed by CRLF `git diff` / `git format-patch` patches.
async fn next_lf_line_lossy<R: AsyncRead + Unpin>(
    reader: &mut Option<BufReader<R>>,
    buf: &mut Vec<u8>,
) -> Option<String> {
    let r = reader.as_mut()?;
    loop {
        let Ok(avail) = r.fill_buf().await else {
            let _ = reader.take();
            return drain_buf_lossy(buf);
        };
        if avail.is_empty() {
            let _ = reader.take();
            return drain_buf_lossy(buf);
        }
        if let Some(pos) = avail.iter().position(|&b| b == b'\n') {
            buf.extend_from_slice(&avail[..pos]);
            r.consume(pos + 1);
            let line = String::from_utf8_lossy(buf).into_owned();
            buf.clear();
            return Some(line);
        }
        buf.extend_from_slice(avail);
        let len = avail.len();
        r.consume(len);
    }
}

fn drain_buf_lossy(buf: &mut Vec<u8>) -> Option<String> {
    if buf.is_empty() {
        None
    } else {
        let line = String::from_utf8_lossy(buf).into_owned();
        buf.clear();
        Some(line)
    }
}

#[derive(Default)]
struct StderrLineState {
    buf: Vec<u8>,
    /// True after emitting a `\r`-terminated segment; if the next chunk starts
    /// with `\n` (a CRLF split across reads), that `\n` is swallowed and
    /// `prev_cr` is cleared so the following line starts fresh rather than
    /// overwriting the previous line.
    prev_cr: bool,
}

#[derive(Debug, PartialEq, Eq)]
struct StderrSegment {
    line: String,
    overwrites_prev: bool,
}

/// Cancellation-safe stderr reader splitting on `\r`, `\n`, or `\r\n`.
///
/// Emits `\r`-terminated progress lines immediately without waiting for the
/// next chunk, while keeping partial bytes in `state.buf` outside `tokio::select!`.
async fn next_cr_or_lf_segment<R: AsyncRead + Unpin>(
    reader: &mut Option<BufReader<R>>,
    state: &mut StderrLineState,
) -> Option<StderrSegment> {
    let r = reader.as_mut()?;
    loop {
        let Ok(avail) = r.fill_buf().await else {
            let _ = reader.take();
            return drain_buf_lossy(&mut state.buf).map(|line| {
                let overwrites_prev = state.prev_cr;
                state.prev_cr = false;
                StderrSegment {
                    line,
                    overwrites_prev,
                }
            });
        };
        if avail.is_empty() {
            let _ = reader.take();
            return drain_buf_lossy(&mut state.buf).map(|line| {
                let overwrites_prev = state.prev_cr;
                state.prev_cr = false;
                StderrSegment {
                    line,
                    overwrites_prev,
                }
            });
        }
        if state.buf.is_empty() && state.prev_cr && avail[0] == b'\n' {
            r.consume(1);
            state.prev_cr = false;
            continue;
        }
        if let Some(pos) = avail.iter().position(|&b| b == b'\n' || b == b'\r') {
            state.buf.extend_from_slice(&avail[..pos]);
            let delim = avail[pos];
            let (consume_len, ended_with_cr) = if delim == b'\r' {
                if avail.get(pos + 1) == Some(&b'\n') {
                    (pos + 2, false)
                } else {
                    (pos + 1, true)
                }
            } else {
                (pos + 1, false)
            };
            r.consume(consume_len);
            let line = String::from_utf8_lossy(&state.buf).into_owned();
            state.buf.clear();
            let overwrites_prev = state.prev_cr;
            state.prev_cr = ended_with_cr;
            return Some(StderrSegment {
                line,
                overwrites_prev,
            });
        }
        state.buf.extend_from_slice(avail);
        let len = avail.len();
        r.consume(len);
    }
}

/// Console command line: environment is never shown and URL credentials
/// (`https://user:password@host`) are masked.
fn format_args_for_display(args: &[String]) -> String {
    args.iter()
        .map(|arg| {
            let arg = jbgit_core::redact_urls_in(arg);
            if arg.contains(' ') || arg.is_empty() {
                format!("\"{arg}\"")
            } else {
                arg
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn detect_index_lock_conflict(
    stderr_lines: &[String],
    git_dir: &Path,
) -> Option<(PathBuf, String)> {
    let joined = stderr_lines.join("\n");
    if joined.contains("index.lock")
        && (joined.contains("File exists") || joined.contains("Unable to create"))
    {
        return Some((git_dir.join("index.lock"), joined));
    }
    None
}

use std::sync::RwLock;

#[derive(Debug, Clone)]
struct CachedGitBinary {
    configured: Option<String>,
    resolved: Option<(PathBuf, Option<jbgit_core::GitVersion>)>,
}

static CONFIGURED_GIT_EXECUTABLE: RwLock<Option<String>> = RwLock::new(None);
static CACHED_GIT_BINARY: RwLock<Option<CachedGitBinary>> = RwLock::new(None);

/// Updates the process-wide `[git] executable` override (empty or `None` reverts
/// to automatic detection) and invalidates cached binary detection.
pub fn set_configured_git_executable(path: Option<String>) {
    let normalized = path
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    if let Ok(mut slot) = CONFIGURED_GIT_EXECUTABLE.write() {
        *slot = normalized;
    }
    if let Ok(mut cache) = CACHED_GIT_BINARY.write() {
        *cache = None;
    }
}

pub fn configured_git_executable() -> Option<String> {
    CONFIGURED_GIT_EXECUTABLE
        .read()
        .ok()
        .and_then(|g| g.clone())
}

/// Resolves the `git` binary respecting the configured `[git] executable` override.
pub(crate) fn cached_git_executable() -> Option<PathBuf> {
    git_binary_info().map(|(p, _)| p)
}

/// Resolves the `git` binary strictly: returns [`EngineError::InvalidGitExecutable`]
/// when an explicit `[git] executable` override is configured and invalid (never
/// silently falling back to `"git"` on `PATH`).
pub(crate) fn require_git_executable() -> Result<PathBuf, EngineError> {
    if let Some(path) = cached_git_executable() {
        return Ok(path);
    }
    let configured = configured_git_executable();
    detect_git_executable(configured.as_deref()).map(|info| info.path)
}

/// Max pathspecs passed on the command line to read-only helpers; beyond this
/// callers use their libgit2 fallback (`git log` has no `--pathspec-from-file`).
pub(crate) const MAX_INLINE_PATHSPECS: usize = PATHSPEC_FROM_FILE_THRESHOLD;

/// Process-wide git binary and its parsed version (re-computed if the configured override changes).
fn git_binary_info() -> Option<(PathBuf, Option<jbgit_core::GitVersion>)> {
    let configured = configured_git_executable();
    if let Ok(guard) = CACHED_GIT_BINARY.read()
        && let Some(entry) = guard.as_ref()
        && entry.configured == configured
    {
        return entry.resolved.clone();
    }
    let resolved = detect_git_executable(configured.as_deref())
        .ok()
        .map(|info| {
            let ver = jbgit_core::GitVersion::parse(&info.version_string);
            (info.path, ver)
        });
    if let Ok(mut guard) = CACHED_GIT_BINARY.write() {
        *guard = Some(CachedGitBinary {
            configured,
            resolved: resolved.clone(),
        });
    }
    resolved
}

/// Synchronous `git worktree list --porcelain [-z]` (snapshot worker thread;
/// `-z` only on git >= 2.36). `None` when git is unavailable or fails.
pub(crate) fn worktree_list_sync(cwd: &Path) -> Option<Vec<jbgit_core::WorktreeInfo>> {
    let (bin, version) = git_binary_info()?;
    let mut cmd = non_interactive_std_command(&bin, Some(cwd));
    cmd.args(["worktree", "list", "--porcelain"]);
    if version.is_some_and(|v| v >= jbgit_core::GitVersion::WORKTREE_LIST_Z) {
        cmd.arg("-z");
    }
    let out = cmd.output().ok()?;
    out.status
        .success()
        .then(|| jbgit_core::parse_worktree_list_porcelain(&out.stdout))
}

fn expand_tilde_path(raw: &str) -> PathBuf {
    if let Some(rest) = raw.strip_prefix("~/").or_else(|| raw.strip_prefix("~\\"))
        && let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))
    {
        return PathBuf::from(home).join(rest);
    }
    PathBuf::from(raw)
}

fn standard_git_install_candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    #[cfg(windows)]
    {
        if let Some(pf) = std::env::var_os("ProgramFiles") {
            let p = PathBuf::from(pf);
            out.push(p.join("Git").join("cmd").join("git.exe"));
            out.push(p.join("Git").join("bin").join("git.exe"));
        }
        if let Some(pf86) = std::env::var_os("ProgramFiles(x86)") {
            let p = PathBuf::from(pf86);
            out.push(p.join("Git").join("cmd").join("git.exe"));
            out.push(p.join("Git").join("bin").join("git.exe"));
        }
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            out.push(
                PathBuf::from(local)
                    .join("Programs")
                    .join("Git")
                    .join("cmd")
                    .join("git.exe"),
            );
        }
        out.push(PathBuf::from(r"C:\Program Files\Git\cmd\git.exe"));
        out.push(PathBuf::from(r"C:\Program Files\Git\bin\git.exe"));
    }
    #[cfg(not(windows))]
    {
        out.push(PathBuf::from("/usr/local/bin/git"));
        out.push(PathBuf::from("/opt/homebrew/bin/git"));
        out.push(PathBuf::from("/usr/bin/git"));
    }
    out
}

/// Resolves the `git` executable path by priority:
/// 1. Explicit `[git] executable` override (invalid path returns a clear error);
/// 2. `PATH` search;
/// 3. Platform standard installation directories fallback.
pub fn find_git_executable_with_override(configured: Option<&str>) -> Result<PathBuf, EngineError> {
    if let Some(raw) = configured.map(str::trim).filter(|s| !s.is_empty()) {
        let expanded = expand_tilde_path(raw);
        let path = if expanded.is_relative() {
            std::env::current_dir()
                .map(|cwd| cwd.join(&expanded))
                .unwrap_or(expanded)
        } else {
            expanded
        };
        if !path.is_file() {
            return Err(EngineError::InvalidGitExecutable {
                path,
                detail: "file does not exist or is not a regular file".to_string(),
            });
        }
        return Ok(path);
    }

    let exe_name = if cfg!(windows) { "git.exe" } else { "git" };
    if let Some(path_var) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path_var) {
            let candidate = dir.join(exe_name);
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }

    for candidate in standard_git_install_candidates() {
        if candidate.is_file() {
            return Ok(candidate);
        }
    }

    Err(EngineError::GitNotFound)
}

/// Detects the `git` binary (honoring an optional `[git] executable` override)
/// and probes `git --version`.
pub fn detect_git_executable(configured: Option<&str>) -> Result<GitExecutableInfo, EngineError> {
    let explicit = configured.is_some_and(|s| !s.trim().is_empty());
    let git_bin = find_git_executable_with_override(configured)?;
    let mut cmd = non_interactive_std_command(&git_bin, None);
    cmd.arg("--version");
    let output = cmd.output().map_err(|e| {
        if explicit {
            EngineError::InvalidGitExecutable {
                path: git_bin.clone(),
                detail: e.to_string(),
            }
        } else {
            EngineError::GitNotFound
        }
    })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(if explicit {
            EngineError::InvalidGitExecutable {
                path: git_bin,
                detail: if stderr.is_empty() {
                    format!("exited with status {}", output.status)
                } else {
                    stderr
                },
            }
        } else {
            EngineError::GitNotFound
        });
    }
    let version_string = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Ok(GitExecutableInfo {
        path: git_bin,
        version_string,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn console_command_line_masks_url_credentials() {
        let args: Vec<String> = ["remote", "add", "fork", "https://u:secret@h/x.git"]
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(
            format_args_for_display(&args),
            "remote add fork https://u:***@h/x.git"
        );
    }

    #[test]
    fn pathspec_from_file_threshold() {
        let small: Vec<String> = (0..10).map(|i| format!("file_{i}.rs")).collect();
        let (args_s, stdin_s) = prepare_pathspec_args(vec!["add".into()], &small);
        assert!(stdin_s.is_none());
        assert!(args_s.contains(&"--".to_string()));

        let large: Vec<String> = (0..40).map(|i| format!("file_{i}.rs")).collect();
        let (args_l, stdin_l) = prepare_pathspec_args(vec!["add".into()], &large);
        assert!(args_l.contains(&"--pathspec-from-file=-".to_string()));
        assert!(args_l.contains(&"--pathspec-file-nul".to_string()));
        let payload = stdin_l.expect("expected nul-delimited stdin payload");
        assert_eq!(payload.iter().filter(|&&b| b == 0).count(), 40);
    }

    #[test]
    fn find_git_executable_explicit_override_and_invalid_path_error() {
        // Explicit invalid path must fail with EngineError::InvalidGitExecutable (never silently fall back to PATH).
        let missing = std::env::temp_dir().join("jbgit_nonexistent_git_binary_12345");
        let missing_str = missing.to_string_lossy().to_string();
        let err = find_git_executable_with_override(Some(&missing_str))
            .expect_err("expected InvalidGitExecutable for missing explicit path");
        assert!(
            matches!(err, EngineError::InvalidGitExecutable { ref path, .. } if path == &missing_str),
            "unexpected error variant: {err:?}"
        );

        // Explicit valid path takes priority over PATH and returns that exact path.
        let detected = find_git_executable_with_override(None).expect("git installed in env");
        let explicit_str = detected.to_string_lossy().to_string();
        let resolved = find_git_executable_with_override(Some(&explicit_str))
            .expect("explicit valid git path must resolve");
        assert_eq!(resolved, detected);

        // Empty / whitespace override falls back cleanly to auto-detection.
        let fallback = find_git_executable_with_override(Some("   "))
            .expect("blank override falls back to auto-detect");
        assert!(fallback.is_file());
    }

    #[tokio::test]
    async fn stdout_non_utf8_and_crlf_preserved_without_truncation_or_deadlock() {
        // 1. Unit check on `next_lf_line_lossy`: preserves `\r` and decodes invalid UTF-8 lossily.
        let (mut tx, rx) = tokio::io::duplex(256);
        let mut reader = Some(BufReader::new(rx));
        let mut buf = Vec::new();
        tx.write_all(b"first\xff\xfe\r\nsecond\r\nlast_no_nl")
            .await
            .unwrap();
        drop(tx);

        let mut lines = Vec::new();
        while let Some(line) = next_lf_line_lossy(&mut reader, &mut buf).await {
            lines.push(line);
        }
        assert_eq!(lines.len(), 3);
        assert!(lines[0].starts_with("first"));
        assert!(lines[0].contains('\u{FFFD}'));
        assert!(lines[0].ends_with('\r'), "CRLF \\r must be preserved");
        assert_eq!(lines[1], "second\r");
        assert_eq!(lines[2], "last_no_nl");

        // 2. End-to-end check with real `git diff --no-index` emitting >32 KiB of
        //    non-UTF-8 + CRLF lines (previously deadlocked the OS pipe buffer or silently truncated).
        let dir = tempfile::tempdir().unwrap();
        let file_a = dir.path().join("a.txt");
        let file_b = dir.path().join("b.txt");
        std::fs::write(&file_a, b"header\r\n").unwrap();
        let mut large_non_utf8 = Vec::with_capacity(64 * 1024);
        large_non_utf8.extend_from_slice(b"header\r\n");
        for i in 0..800 {
            large_non_utf8.extend_from_slice(format!("row-{i:04}-").as_bytes());
            large_non_utf8.extend_from_slice(&[0xC4, 0xE3, 0xBA, 0xC3, 0xFF, 0xFE]); // GBK + invalid UTF-8
            large_non_utf8.extend_from_slice(b"-tail-padding-padding-padding\r\n");
        }
        std::fs::write(&file_b, &large_non_utf8).unwrap();

        let exec = GitCliExecutor::detect().expect("git installed");
        let args = vec![
            "-c".to_string(),
            "core.autocrlf=false".to_string(),
            "diff".to_string(),
            "--no-index".to_string(),
            "--text".to_string(),
            "--".to_string(),
            "a.txt".to_string(),
            "b.txt".to_string(),
        ];
        let mut console_lines = Vec::new();
        let out = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            exec.execute(
                CliCommand {
                    id: 1,
                    workdir: dir.path(),
                    git_dir: dir.path(),
                    args: &args,
                    stdin_payload: None,
                    is_read_only: true,
                    env: &[],
                },
                |ev| {
                    if let CliStreamEvent::Output {
                        stream: ConsoleStream::Stdout,
                        line,
                        ..
                    } = ev
                    {
                        console_lines.push(line);
                    }
                },
            ),
        )
        .await
        .expect("must not deadlock on >pipe-buffer non-UTF-8 stdout")
        .expect("git diff executed");

        // `git diff --no-index` returns exit code 1 when files differ.
        assert_eq!(out.exit_code, 1);
        assert!(
            out.stdout_lines.iter().any(|l| l.contains("+row-0799-")),
            "output must not be truncated at first invalid UTF-8 line (got {} lines)",
            out.stdout_lines.len()
        );
        let last_added = out
            .stdout_lines
            .iter()
            .find(|l| l.contains("+row-0799-"))
            .unwrap();
        assert!(
            last_added.ends_with('\r'),
            "stdout_lines must preserve trailing \\r for CRLF patches"
        );
        let console_added = console_lines
            .iter()
            .find(|l| l.contains("+row-0799-"))
            .unwrap();
        assert!(
            !console_added.ends_with('\r'),
            "console stream display should strip trailing \\r"
        );
    }

    #[tokio::test]
    async fn stderr_cr_or_lf_segment_is_cancellation_safe_and_emits_cr_immediately() {
        let (mut tx, rx) = tokio::io::duplex(256);
        let mut reader = Some(BufReader::new(rx));
        let mut state = StderrLineState::default();

        // 1. `\r`-terminated progress line must emit immediately without waiting for more bytes.
        tx.write_all(b"Counting objects: 10%\r").await.unwrap();
        let seg1 = tokio::time::timeout(
            std::time::Duration::from_millis(200),
            next_cr_or_lf_segment(&mut reader, &mut state),
        )
        .await
        .expect("CR segment must emit immediately without waiting for next chunk")
        .expect("first segment");
        assert_eq!(
            seg1,
            StderrSegment {
                line: "Counting objects: 10%".to_string(),
                overwrites_prev: false,
            }
        );

        // Next progress update overwrites previous line.
        tx.write_all(b"Counting objects: 100%, done.\r\n")
            .await
            .unwrap();
        let seg2 = next_cr_or_lf_segment(&mut reader, &mut state)
            .await
            .unwrap();
        assert_eq!(
            seg2,
            StderrSegment {
                line: "Counting objects: 100%, done.".to_string(),
                overwrites_prev: true,
            }
        );

        // 2. Split `\r\n` across chunks must NOT cause the next line to overwrite the first.
        tx.write_all(b"fatal: first error\r").await.unwrap();
        let seg3 = next_cr_or_lf_segment(&mut reader, &mut state)
            .await
            .unwrap();
        assert_eq!(
            seg3,
            StderrSegment {
                line: "fatal: first error".to_string(),
                overwrites_prev: false,
            }
        );
        tx.write_all(b"\nsecond line\r\n").await.unwrap();
        let seg4 = next_cr_or_lf_segment(&mut reader, &mut state)
            .await
            .unwrap();
        assert_eq!(
            seg4,
            StderrSegment {
                line: "second line".to_string(),
                overwrites_prev: false,
            }
        );

        // 3. Cancellation safety inside `tokio::select!`: partial chunk consumed before
        //    another branch wins must NOT be lost.
        tx.write_all(b"fatal: Unable to ").await.unwrap();
        let (notify_tx, notify_rx) = tokio::sync::oneshot::channel::<()>();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            let _ = notify_tx.send(());
        });
        let cancelled = tokio::select! {
            _ = next_cr_or_lf_segment(&mut reader, &mut state) => false,
            _ = notify_rx => true,
        };
        assert!(cancelled, "oneshot branch should cancel partial stderr read");
        assert!(
            !state.buf.is_empty(),
            "partial stderr bytes must remain preserved in external state"
        );

        tx.write_all(b"create '.git/index.lock': File exists.\n")
            .await
            .unwrap();
        drop(tx);
        let seg5 = next_cr_or_lf_segment(&mut reader, &mut state)
            .await
            .unwrap();
        assert_eq!(
            seg5,
            StderrSegment {
                line: "fatal: Unable to create '.git/index.lock': File exists.".to_string(),
                overwrites_prev: false,
            }
        );
        assert!(next_cr_or_lf_segment(&mut reader, &mut state).await.is_none());
    }

    #[tokio::test]
    async fn execute_drop_kills_child_and_aborts_stdin_writer() {
        let dir = tempfile::tempdir().unwrap();
        let exec = GitCliExecutor::detect().expect("git installed");

        // Verify AbortOnDrop cancels an uncompleted task when dropped.
        let (tx, mut rx) = tokio::sync::oneshot::channel::<()>();
        let guard = AbortOnDrop(Some(tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
            let _ = tx.send(());
        })));
        drop(guard);
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        assert!(
            matches!(rx.try_recv(), Err(tokio::sync::oneshot::error::TryRecvError::Closed)),
            "AbortOnDrop must abort spawned writer task"
        );

        // On Windows, directly verify WindowsJobGuard succeeds with real kernel APIs
        // and terminates a spawned child process on drop.
        #[cfg(windows)]
        {
            let mut cmd = Command::new(&exec.git_bin);
            cmd.args(["-c", "alias.hang=!sleep 30", "hang"])
                .current_dir(dir.path())
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .creation_flags(0x0800_0000);
            let mut child = cmd.spawn().expect("spawn git sleep");
            let job = win_job::WindowsJobGuard::assign(
                child.raw_handle().expect("windows raw handle"),
            );
            assert!(
                job.is_some(),
                "WindowsJobGuard::assign must succeed (validates Win32 struct layout)"
            );
            // Process must still be alive before the Job Object handle is closed.
            tokio::time::sleep(std::time::Duration::from_millis(80)).await;
            assert!(
                child.try_wait().expect("try_wait").is_none(),
                "child sleep 30 must still be running before dropping WindowsJobGuard"
            );
            // Closing the Job Object with JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
            // terminates the process tree immediately (Windows exits job-killed processes with code 0).
            drop(job);
            let _status = tokio::time::timeout(std::time::Duration::from_secs(5), child.wait())
                .await
                .expect("dropping WindowsJobGuard must terminate child promptly")
                .expect("wait child");
        }

        // Verify cancelling `execute` via JoinHandle::abort() returns promptly.
        let exec_clone = exec.clone();
        let workdir = dir.path().to_path_buf();
        let handle = tokio::spawn(async move {
            let args = vec![
                "-c".to_string(),
                "alias.hang=!sleep 30".to_string(),
                "hang".to_string(),
            ];
            let _ = exec_clone
                .execute(
                    CliCommand {
                        id: 99,
                        workdir: &workdir,
                        git_dir: &workdir,
                        args: &args,
                        stdin_payload: Some(&vec![b'x'; 1024 * 1024]),
                        is_read_only: true,
                        env: &[],
                    },
                    |_| {},
                )
                .await;
        });
        tokio::time::sleep(std::time::Duration::from_millis(120)).await;
        handle.abort();
        let err = tokio::time::timeout(std::time::Duration::from_secs(5), handle)
            .await
            .expect("aborted execute future must finish promptly")
            .expect_err("expected JoinError::is_cancelled");
        assert!(err.is_cancelled());
    }
}
