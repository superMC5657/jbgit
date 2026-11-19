use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Instant;

use chrono::Local;
use jbgit_core::{ConsoleStream, GitConsoleEntry};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

use crate::error::EngineError;

const PATHSPEC_FROM_FILE_THRESHOLD: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitExecutableInfo {
    pub path: PathBuf,
    pub version_string: String,
}

#[derive(Debug, Clone)]
pub struct GitCliOutput {
    pub exit_code: i32,
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
    pub fn detect() -> Result<Self, EngineError> {
        let git_bin = find_git_executable().ok_or(EngineError::GitNotFound)?;
        let mut cmd = std::process::Command::new(&git_bin);
        cmd.arg("--version")
            .env("GIT_PAGER", "cat")
            .env("GIT_TERMINAL_PROMPT", "0");
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        let output = cmd.output().map_err(|_| EngineError::GitNotFound)?;
        let version_string = String::from_utf8_lossy(&output.stdout).trim().to_string();

        Ok(Self {
            git_bin,
            version_string,
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
            .stderr(Stdio::piped());

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

        if let Some(payload) = stdin_payload
            && let Some(mut stdin) = child.stdin.take()
        {
            let _ = stdin.write_all(payload).await;
            let _ = stdin.shutdown().await;
        }

        let stdout = child.stdout.take();
        let stderr = child.stderr.take();

        let mut stdout_lines_reader = stdout.map(|s| BufReader::new(s).lines());
        let mut stderr_lines_reader = stderr.map(|s| BufReader::new(s).lines());

        let mut stdout_lines = Vec::new();
        let mut stderr_lines = Vec::new();
        let mut stdout_done = stdout_lines_reader.is_none();
        let mut stderr_done = stderr_lines_reader.is_none();

        while !stdout_done || !stderr_done {
            tokio::select! {
                res = next_line(&mut stdout_lines_reader), if !stdout_done => {
                    match res {
                        Some(line) => {
                            on_event(CliStreamEvent::Output {
                                id,
                                stream: ConsoleStream::Stdout,
                                line: line.clone(),
                            });
                            stdout_lines.push(line);
                        }
                        None => stdout_done = true,
                    }
                }
                res = next_line(&mut stderr_lines_reader), if !stderr_done => {
                    match res {
                        Some(line) => {
                            on_event(CliStreamEvent::Output {
                                id,
                                stream: ConsoleStream::Stderr,
                                line: line.clone(),
                            });
                            stderr_lines.push(line);
                        }
                        None => stderr_done = true,
                    }
                }
            }
        }

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

async fn next_line<R: tokio::io::AsyncBufRead + Unpin>(
    reader: &mut Option<tokio::io::Lines<R>>,
) -> Option<String> {
    match reader {
        Some(r) => r.next_line().await.ok().flatten(),
        None => None,
    }
}

fn format_args_for_display(args: &[String]) -> String {
    args.iter()
        .map(|arg| {
            if arg.contains(' ') || arg.is_empty() {
                format!("\"{arg}\"")
            } else {
                arg.clone()
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

fn find_git_executable() -> Option<PathBuf> {
    let exe_name = if cfg!(windows) { "git.exe" } else { "git" };
    if let Some(path_var) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path_var) {
            let candidate = dir.join(exe_name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}
