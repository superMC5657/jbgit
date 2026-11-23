use crate::Oid;

use super::GitOp;

/// Why a Git write operation failed (drives the follow-up dialog).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitFailure {
    /// `Your local changes to the following files would be overwritten…`
    LocalChangesOverwritten { files: Vec<String> },
    /// `untracked working tree files would be overwritten…`
    UntrackedOverwritten { files: Vec<String> },
    /// Merge / rebase / cherry-pick stopped on conflicts.
    Conflicts,
    /// `git branch -d` on an unmerged branch.
    NotFullyMerged,
    /// Tag already exists locally or on the remote.
    TagAlreadyExists,
    /// Push rejected (non-fast-forward / stale info).
    PushRejected,
    /// `merge --ff-only` (or a fast-forward-only update) on diverged history.
    NotFastForward,
    /// HTTPS credentials rejected / missing, or SSH key refused.
    AuthenticationFailed,
    /// `git remote add` / `rename` onto an existing remote name.
    RemoteAlreadyExists,
    /// The branch is checked out in another working tree (`worktree add`,
    /// `checkout`, `branch -d` refuse it).
    BranchCheckedOutInWorktree { branch: String, path: String },
    /// `git worktree remove` on a working tree with local changes.
    WorktreeDirty,
    /// `git worktree remove` on a locked working tree.
    WorktreeLocked,
    /// `git apply` rejected the patch (first `error:` line).
    PatchDoesNotApply { detail: String },
    /// `git apply --3way` applied with conflicts in `paths`.
    PatchConflicts { paths: Vec<String> },
    /// Anything else (first meaningful stderr line).
    Other(String),
}

/// Classifies the combined stderr/stdout of a failed `git` step.
pub fn classify_failure(lines: &[String]) -> GitFailure {
    let joined = lines.join("\n");
    if is_authentication_failure(&joined) {
        return GitFailure::AuthenticationFailed;
    }
    if joined.contains("error: remote ") && joined.contains(" already exists") {
        return GitFailure::RemoteAlreadyExists;
    }
    if let Some(f) = parse_checked_out_elsewhere(lines) {
        return f;
    }
    if joined.contains("contains modified or untracked files") {
        return GitFailure::WorktreeDirty;
    }
    if joined.contains("cannot remove a locked working tree") {
        return GitFailure::WorktreeLocked;
    }
    if joined.contains("' with conflicts.") && joined.contains("Applied patch to '") {
        return GitFailure::PatchConflicts {
            paths: crate::parse_apply_conflicts(lines),
        };
    }
    if joined.contains("patch does not apply")
        || joined.contains("corrupt patch at line")
        || joined.contains("No valid patches in input")
        || joined.contains("does not exist in index")
        || joined.contains("already exists in working directory")
        || joined.contains("does not match index")
    {
        return GitFailure::PatchDoesNotApply {
            detail: crate::first_apply_error(lines),
        };
    }
    let collect_files = |marker: &str| -> Vec<String> {
        let mut files = Vec::new();
        let mut in_list = false;
        for line in lines {
            if line.contains(marker) {
                in_list = true;
                continue;
            }
            if in_list {
                if line.starts_with('\t') || line.starts_with("    ") {
                    files.push(line.trim().to_string());
                } else if !files.is_empty() || !line.trim().is_empty() {
                    in_list = !files.is_empty() && line.trim().is_empty();
                    if !in_list {
                        break;
                    }
                }
            }
        }
        files
    };
    if joined.contains("untracked working tree files would be overwritten") {
        return GitFailure::UntrackedOverwritten {
            files: collect_files("untracked working tree files would be overwritten"),
        };
    }
    if joined.contains("would be overwritten by")
        || joined.contains("Please commit your changes or stash them")
        || joined.contains("cannot rebase: You have unstaged changes")
        || joined.contains("Your index contains uncommitted changes")
        || joined.contains("cannot pull with rebase: You have unstaged changes")
        || joined.contains("Entry '") && joined.contains("not uptodate. Cannot merge")
    {
        return GitFailure::LocalChangesOverwritten {
            files: collect_files("would be overwritten by"),
        };
    }
    if joined.contains("CONFLICT")
        || joined.contains("could not apply")
        || joined.contains("Automatic merge failed")
        || joined.contains("after resolving the conflicts")
        || joined.contains("fix conflicts and then commit")
    {
        return GitFailure::Conflicts;
    }
    if joined.contains("is not fully merged") {
        return GitFailure::NotFullyMerged;
    }
    if (joined.contains("tag '") && joined.contains("already exists"))
        || (joined.contains("refs/tags/") && joined.contains("already exists"))
        || (joined.contains("[rejected]") && joined.contains("already exists"))
        || joined.contains("(already exists)")
    {
        return GitFailure::TagAlreadyExists;
    }
    if joined.contains("Not possible to fast-forward")
        || joined.contains("not possible to fast-forward")
    {
        return GitFailure::NotFastForward;
    }
    if joined.contains("[rejected]")
        || joined.contains("non-fast-forward")
        || joined.contains("stale info")
    {
        return GitFailure::PushRejected;
    }
    let first = lines
        .iter()
        .map(|l| l.trim())
        .find(|l| l.starts_with("error:") || l.starts_with("fatal:"))
        .or_else(|| lines.iter().map(|l| l.trim()).find(|l| !l.is_empty()))
        .unwrap_or("git command failed");
    GitFailure::Other(crate::redact_urls_in(first))
}

/// `fatal: 'x' is already checked out at '/p'` (older git),
/// `fatal: 'x' is already used by worktree at '/p'` (git ≥ 2.42) and the
/// `branch -d` forms `Cannot delete branch 'x' checked out at '/p'` /
/// `cannot delete branch 'x' used by worktree at '/p'`.
fn parse_checked_out_elsewhere(lines: &[String]) -> Option<GitFailure> {
    const MARKERS: [&str; 4] = [
        "' is already checked out at '",
        "' is already used by worktree at '",
        "' used by worktree at '",
        "' checked out at '",
    ];
    lines.iter().find_map(|line| {
        let lower = line.to_lowercase();
        if !(lower.contains("already") || lower.contains("cannot delete branch")) {
            return None;
        }
        let marker = MARKERS.iter().find(|m| line.contains(*m))?;
        let (head, tail) = line.split_once(marker)?;
        let branch = head.rsplit('\'').next().unwrap_or_default().to_string();
        let path = tail.trim_end().trim_end_matches('\'').to_string();
        Some(GitFailure::BranchCheckedOutInWorktree { branch, path })
    })
}

/// Credential / key rejection messages from git, git-remote-http(s) and ssh.
fn is_authentication_failure(joined: &str) -> bool {
    const MARKERS: [&str; 9] = [
        "Authentication failed",
        "Permission denied (publickey",
        "could not read Username",
        "could not read Password",
        "Invalid username or password",
        "HTTP Basic: Access denied",
        "terminal prompts disabled",
        "The requested URL returned error: 401",
        "The requested URL returned error: 403",
    ];
    MARKERS.iter().any(|m| joined.contains(m))
}

/// One branch moved by Update Project (`GitUpdatedRanges`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdatedRange {
    pub branch: String,
    pub before: Oid,
    pub after: Oid,
}

/// A tracked branch Update Project could not fast-forward.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedBranch {
    pub branch: String,
    pub reason: String,
}

/// Summary of what Update Project brought in (`GitUpdateInfoAsLog`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UpdateInfo {
    pub ranges: Vec<UpdatedRange>,
    pub skipped: Vec<SkippedBranch>,
    /// Distinct commits reachable from any `after` but not from its `before`.
    pub new_commits: usize,
    /// Distinct paths changed across all ranges.
    pub files_changed: usize,
    /// The new commits themselves (newest first, capped), for the Log range filter.
    pub commits: Vec<Oid>,
}

impl UpdateInfo {
    pub fn is_empty(&self) -> bool {
        self.ranges.is_empty()
    }

    /// `abc1234..def5678` for a single range, `N ranges` otherwise.
    pub fn range_label(&self) -> String {
        match self.ranges.as_slice() {
            [] => String::new(),
            [one] => format!("{}..{}", one.before.short(), one.after.short()),
            many => format!("{} ranges", many.len()),
        }
    }
}

/// Result of a [`GitOp`] run by the engine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitOpOutcome {
    pub op: GitOp,
    pub success: bool,
    pub failure: Option<GitFailure>,
    pub stdout: Vec<String>,
    pub stderr: Vec<String>,
    pub stash_kept: bool,
    /// Commit message Git prepared for a pending commit (`SQUASH_MSG` / `MERGE_MSG`).
    pub prepared_message: Option<String>,
    /// Update Project summary.
    pub update_info: Option<UpdateInfo>,
}

impl GitOpOutcome {
    pub fn ok(op: GitOp) -> Self {
        Self {
            op,
            success: true,
            failure: None,
            stdout: Vec::new(),
            stderr: Vec::new(),
            stash_kept: false,
            prepared_message: None,
            update_info: None,
        }
    }

    pub fn failed(op: GitOp, failure: GitFailure) -> Self {
        Self {
            op,
            success: false,
            failure: Some(failure),
            stdout: Vec::new(),
            stderr: Vec::new(),
            stash_kept: false,
            prepared_message: None,
            update_info: None,
        }
    }
}

/// One `stash@{n}` entry.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StashEntry {
    pub index: usize,
    pub oid: Oid,
    pub message: String,
    pub timestamp: i64,
}

impl StashEntry {
    pub fn branch(&self) -> Option<&str> {
        let rest = self
            .message
            .strip_prefix("WIP on ")
            .or_else(|| self.message.strip_prefix("On "))?;
        rest.split_once(':').map(|(b, _)| b)
    }

    pub fn short_message(&self) -> &str {
        match self.branch() {
            Some(_) => self
                .message
                .split_once(": ")
                .map_or(self.message.as_str(), |(_, m)| m),
            None => &self.message,
        }
    }
}

/// Summary extracted from `git push --porcelain` stdout lines.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PushOutcomeSummary {
    pub branch_up_to_date: bool,
    pub new_branch: Option<String>,
    pub forced_update: bool,
    pub commit_range: Option<String>,
    pub pushed_tags: usize,
}

fn is_hex_range_token(s: &str) -> bool {
    let Some((a, b)) = s.split_once("...") .or_else(|| s.split_once("..")) else {
        return false;
    };
    !a.is_empty()
        && !b.is_empty()
        && a.bytes().all(|c| c.is_ascii_hexdigit())
        && b.bytes().all(|c| c.is_ascii_hexdigit())
}

/// Parses `git push --porcelain` stdout lines into a [`PushOutcomeSummary`].
///
/// Format: `<flag>\t<from>:<to>\t<summary> [(<reason>)]`
pub fn parse_push_porcelain(stdout: &[String]) -> PushOutcomeSummary {
    let mut out = PushOutcomeSummary::default();
    for raw in stdout {
        let line = raw.trim_end_matches('\r');
        let mut parts = line.splitn(3, '\t');
        let Some(flag_str) = parts.next() else {
            continue;
        };
        let Some(refspec) = parts.next() else {
            continue;
        };
        let Some(summary_raw) = parts.next() else {
            continue;
        };
        let mut chars = flag_str.chars();
        let Some(flag) = chars.next() else {
            continue;
        };
        if chars.next().is_some() || !matches!(flag, ' ' | '+' | '-' | '*' | '!' | '=') {
            continue;
        }
        let Some((_from, to)) = refspec.split_once(':') else {
            continue;
        };
        let summary = summary_raw.trim();
        let summary_head = summary.split(" (").next().unwrap_or(summary).trim();
        if to.starts_with("refs/tags/") || summary.contains("[new tag]") {
            if matches!(flag, '*' | ' ' | '+') {
                out.pushed_tags += 1;
            }
        } else if to.starts_with("refs/heads/") || summary.contains("[new branch]") {
            match flag {
                '*' => {
                    out.new_branch =
                        Some(to.strip_prefix("refs/heads/").unwrap_or(to).to_string());
                }
                '=' => {
                    out.branch_up_to_date = true;
                }
                '+' => {
                    out.forced_update = true;
                    if is_hex_range_token(summary_head) {
                        out.commit_range = Some(summary_head.to_string());
                    }
                }
                ' ' if is_hex_range_token(summary_head) => {
                    out.commit_range = Some(summary_head.to_string());
                }
                _ => {}
            }
        }
    }
    out
}
