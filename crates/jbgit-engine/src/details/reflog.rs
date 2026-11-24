use jbgit_core::{
    EpochToken, REFLOG_GIT_FORMAT, ReflogBundle, ReflogKey, parse_reflog_output,
};

use crate::cli::non_interactive_std_command;
use crate::error::EngineError;
use crate::probe::RepoLocation;

/// Loads paginated Git reflog entries via read-only
/// `git reflog show --date=unix --format=... -n <limit+1> <ref_name> --`.
pub fn load_reflog_sync(
    location: &RepoLocation,
    key: &ReflogKey,
    token: &EpochToken,
) -> Result<ReflogBundle, EngineError> {
    if token.is_stale() {
        return Err(EngineError::Cancelled);
    }
    let ref_name = if key.ref_name.trim().is_empty() {
        "HEAD"
    } else {
        key.ref_name.trim()
    };
    let fetch_n = key.limit.saturating_add(1).max(2);
    let format_arg = format!("--format={REFLOG_GIT_FORMAT}");
    let limit_arg = format!("-n{fetch_n}");

    let git = crate::cli::require_git_executable()?;
    let mut cmd = non_interactive_std_command(&git, Some(&location.workdir));
    cmd.arg("--git-dir")
        .arg(&location.git_dir)
        .arg("--work-tree")
        .arg(&location.workdir)
        .args([
            "reflog",
            "show",
            "--date=unix",
            &format_arg,
            &limit_arg,
            ref_name,
            "--",
        ]);

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
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let low = stderr.to_ascii_lowercase();
        if low.contains("does not have any commits")
            || low.contains("no reflog")
            || low.contains("unknown revision")
        {
            return Ok(ReflogBundle {
                key: key.clone(),
                entries: Vec::new(),
                has_more: false,
            });
        }
        return Err(EngineError::Other(if stderr.is_empty() {
            format!("git reflog show {ref_name} failed")
        } else {
            stderr
        }));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(parse_reflog_output(&stdout, key.clone()))
}
