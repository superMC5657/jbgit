use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use crate::cli::non_interactive_std_command;
use crate::probe::RepoLocation;

const REMOTE_TAG_CHECK_TIMEOUT: Duration = Duration::from_secs(5);

/// Parses `git ls-remote --tags <remote> refs/tags/<tag>` stdout.
pub(crate) fn parse_ls_remote_tag_exists(raw: &[u8], tag: &str) -> bool {
    let target = format!("refs/tags/{tag}");
    let peeled = format!("{target}^{{}}");
    let text = String::from_utf8_lossy(raw);
    for line in text.lines() {
        let mut cols = line.split_whitespace();
        let _oid = cols.next();
        if let Some(refname) = cols.next()
            && (refname == target || refname == peeled)
        {
            return true;
        }
    }
    false
}

/// Checks whether `refs/tags/<tag>` exists on `remote` with a 5-second timeout.
///
/// Returns `None` on timeout, missing git binary, or non-zero exit so callers
/// treat existence as unknown.
pub(crate) async fn check_remote_tag_async(
    git_exe: &Path,
    loc: &RepoLocation,
    remote: &str,
    tag: &str,
) -> Option<bool> {
    let std_cmd = non_interactive_std_command(git_exe, Some(&loc.workdir));
    let mut cmd = tokio::process::Command::from(std_cmd);
    let ref_pattern = format!("refs/tags/{tag}");
    cmd.args(["ls-remote", "--tags", remote, &ref_pattern])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let child = cmd.spawn().ok()?;
    let output = tokio::time::timeout(REMOTE_TAG_CHECK_TIMEOUT, child.wait_with_output())
        .await
        .ok()?
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(parse_ls_remote_tag_exists(&output.stdout, tag))
}
