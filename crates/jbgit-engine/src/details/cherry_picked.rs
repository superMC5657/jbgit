use std::collections::HashSet;
use std::path::PathBuf;

use jbgit_core::{CherryPickedKey, EpochToken, Oid, parse_git_cherry_output};

use crate::cli::non_interactive_std_command;
use crate::error::EngineError;
use crate::probe::RepoLocation;

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
    let branch = key.branch.trim();
    if branch.is_empty() {
        return Ok(HashSet::new());
    }

    let git = crate::cli::cached_git_executable().unwrap_or_else(|| PathBuf::from("git"));
    let mut cmd = non_interactive_std_command(&git, Some(&location.workdir));
    cmd.arg("--git-dir")
        .arg(&location.git_dir)
        .arg("--work-tree")
        .arg(&location.workdir)
        .args(["cherry", &key.head_oid.to_string(), branch]);

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

    if !output.status.success() {
        // Non-existent branch / unborn repo: gracefully return empty set instead of failing the Log view.
        return Ok(HashSet::new());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(parse_git_cherry_output(&stdout).into_iter().collect())
}
