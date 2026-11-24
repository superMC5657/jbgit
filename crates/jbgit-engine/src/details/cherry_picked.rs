use std::collections::HashSet;

use jbgit_core::{CherryPickedKey, EpochToken, Oid, parse_git_cherry_output};

use crate::cli::non_interactive_std_command;
use crate::error::EngineError;
use crate::probe::RepoLocation;

fn build_cherry_command(
    location: &RepoLocation,
    key: &CherryPickedKey,
) -> Result<Option<std::process::Command>, EngineError> {
    let branch = key.branch.trim();
    if branch.is_empty() {
        return Ok(None);
    }
    let git = crate::cli::require_git_executable()?;
    let mut cmd = non_interactive_std_command(&git, Some(&location.workdir));
    cmd.arg("--git-dir")
        .arg(&location.git_dir)
        .arg("--work-tree")
        .arg(&location.workdir)
        .args(["cherry", &key.head_oid.to_string(), branch]);
    Ok(Some(cmd))
}

fn finish_cherry_output(output: &std::process::Output) -> HashSet<Oid> {
    if !output.status.success() {
        // Non-existent branch / unborn repo: gracefully return empty set instead of failing the Log view.
        return HashSet::new();
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    parse_git_cherry_output(&stdout).into_iter().collect()
}

/// Runs read-only `git cherry <head_oid> <branch>` to find commits on `<branch>`
/// whose patch-id already exists on `<head_oid>` (`- <oid>` lines).
pub fn load_cherry_picked_sync(
    location: &RepoLocation,
    key: &CherryPickedKey,
    token: &EpochToken,
) -> Result<HashSet<Oid>, EngineError> {
    if token.is_stale() {
        return Err(EngineError::Cancelled);
    }
    let Some(mut cmd) = build_cherry_command(location, key)? else {
        return Ok(HashSet::new());
    };
    let output = cmd.output().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            EngineError::GitNotFound
        } else {
            EngineError::Io(e)
        }
    })?;
    if token.is_stale() {
        return Err(EngineError::Cancelled);
    }
    Ok(finish_cherry_output(&output))
}

/// Async cancellable variant of [`load_cherry_picked_sync`] that runs outside
/// [`crate::pool::Git2WorkerPool`].
pub(crate) async fn load_cherry_picked_async(
    location: &RepoLocation,
    key: &CherryPickedKey,
    token: &EpochToken,
) -> Result<HashSet<Oid>, EngineError> {
    if token.is_stale() {
        return Err(EngineError::Cancelled);
    }
    let Some(cmd) = build_cherry_command(location, key)? else {
        return Ok(HashSet::new());
    };
    let output = super::run_git_output_cancellable(cmd, None, token).await?;
    Ok(finish_cherry_output(&output))
}
