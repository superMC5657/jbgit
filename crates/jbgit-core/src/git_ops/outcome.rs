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
    /// Anything else (first meaningful stderr line).
    Other(String),
}

/// Classifies the combined stderr/stdout of a failed `git` step.
pub fn classify_failure(lines: &[String]) -> GitFailure {
    let joined = lines.join("\n");
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
    GitFailure::Other(first.to_string())
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
