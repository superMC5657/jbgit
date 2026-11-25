//! Repository creation & cloning lifecycle (`git init` and `git clone --progress`)
//! executed outside an existing bound repository (Spec 13 Issue 13).

use std::fs;
use std::path::{Path, PathBuf};

use crate::askpass::AskpassEnv;
use crate::cli::{CliCommand, CliStreamEvent, GitCliExecutor};
use crate::error::EngineError;
use crate::probe::RepoLocation;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloneRequest {
    pub url: String,
    pub target_dir: PathBuf,
    pub depth: Option<u32>,
    pub recurse_submodules: bool,
}

/// Builds canonical `git clone` arguments with `--progress` and optional shallow/submodules flags.
pub fn compile_clone_args(req: &CloneRequest) -> Vec<String> {
    let mut args = vec!["clone".to_string(), "--progress".to_string()];
    if let Some(depth) = req.depth.filter(|&d| d > 0) {
        args.push("--depth".to_string());
        args.push(depth.to_string());
    }
    if req.recurse_submodules {
        args.push("--recurse-submodules".to_string());
    }
    args.push("--".to_string());
    args.push(req.url.trim().to_string());
    args.push(req.target_dir.to_string_lossy().into_owned());
    args
}

/// Runs `git init` in `dir` (creating the directory if needed) using the configured
/// or auto-detected `git` executable, then discovers and returns its [`RepoLocation`].
pub fn init_repository_sync(
    dir: &Path,
    git_executable: Option<&str>,
) -> Result<RepoLocation, EngineError> {
    if !dir.exists() {
        fs::create_dir_all(dir)?;
    }
    let git_info = crate::cli::detect_git_executable(git_executable)?;
    let mut cmd = crate::cli::non_interactive_std_write_command(&git_info.path, Some(dir));
    cmd.arg("init");
    let output = cmd.output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let detail = if !stderr.is_empty() { stderr } else { stdout };
        return Err(EngineError::Other(if detail.is_empty() {
            format!("git init failed with status {}", output.status)
        } else {
            detail
        }));
    }
    RepoLocation::discover(dir)
}

/// Runs `git clone --progress` asynchronously, routing HTTPS/SSH prompts through
/// `askpass` and streaming progress lines to `on_line`.
pub async fn clone_repository_async<F>(
    req: CloneRequest,
    git_executable: Option<&str>,
    askpass: Option<&AskpassEnv>,
    mut on_line: F,
) -> Result<PathBuf, EngineError>
where
    F: FnMut(String) + Send,
{
    if req.url.trim().is_empty() {
        return Err(EngineError::Other(
            "Repository URL cannot be empty".to_string(),
        ));
    }
    if req.target_dir.as_os_str().is_empty() {
        return Err(EngineError::Other(
            "Target directory cannot be empty".to_string(),
        ));
    }
    // Absolutize relative target_dir so passing it alongside `current_dir = parent`
    // never doubles nested relative paths (`sub/repo` -> `sub/sub/repo`).
    let target_dir = if req.target_dir.is_relative() {
        std::env::current_dir()
            .map(|cwd| cwd.join(&req.target_dir))
            .unwrap_or_else(|_| req.target_dir.clone())
    } else {
        req.target_dir.clone()
    };
    let req = CloneRequest {
        target_dir: target_dir.clone(),
        ..req
    };

    let parent_to_create = target_dir
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(Path::to_path_buf);
    let git_exe_owned = git_executable.map(str::to_string);
    let (cli, workdir) = tokio::task::spawn_blocking(move || -> Result<_, EngineError> {
        if let Some(ref parent) = parent_to_create
            && !parent.exists()
        {
            fs::create_dir_all(parent)?;
        }
        let cli = GitCliExecutor::detect_with_executable(git_exe_owned.as_deref())?;
        let workdir = parent_to_create
            .filter(|p| p.is_dir())
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("."));
        Ok((cli, workdir))
    })
    .await
    .map_err(|e| EngineError::Other(format!("clone setup task failed: {e}")))??;

    let args = compile_clone_args(&req);
    let env = askpass.map(AskpassEnv::vars).unwrap_or_default();

    let out = cli
        .execute(
            CliCommand {
                id: 0,
                workdir: &workdir,
                git_dir: &workdir,
                args: &args,
                stdin_payload: None,
                is_read_only: false,
                env: &env,
            },
            |ev| {
                if let CliStreamEvent::Output { line, .. } = ev {
                    on_line(jbgit_core::redact_urls_in(&line));
                }
            },
        )
        .await?;

    if out.exit_code != 0 {
        let stderr = jbgit_core::redact_urls_in(&out.stderr_lines.join("\n"));
        let detail = stderr.trim();
        return Err(EngineError::Other(if detail.is_empty() {
            format!("git clone exited with status {}", out.exit_code)
        } else {
            detail.to_string()
        }));
    }

    Ok(req.target_dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compile_clone_args_includes_progress_depth_and_submodules() {
        let req = CloneRequest {
            url: "https://example.com/org/repo.git".into(),
            target_dir: PathBuf::from("/tmp/repo"),
            depth: Some(1),
            recurse_submodules: true,
        };
        let args = compile_clone_args(&req);
        assert_eq!(
            args,
            vec![
                "clone",
                "--progress",
                "--depth",
                "1",
                "--recurse-submodules",
                "--",
                "https://example.com/org/repo.git",
                "/tmp/repo",
            ]
        );

        let minimal = CloneRequest {
            url: "git@example.com:org/repo.git".into(),
            target_dir: PathBuf::from("repo"),
            depth: None,
            recurse_submodules: false,
        };
        assert_eq!(
            compile_clone_args(&minimal),
            vec![
                "clone",
                "--progress",
                "--",
                "git@example.com:org/repo.git",
                "repo",
            ]
        );
    }
}
