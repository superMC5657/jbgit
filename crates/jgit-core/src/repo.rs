use std::path::PathBuf;

use crate::Oid;

/// Ongoing Git repository workflow / state indicator.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum RepositoryState {
    #[default]
    Clean,
    Merge,
    Rebase {
        progress: Option<(usize, usize)>,
        branch: Option<String>,
    },
    RebaseInteractive {
        progress: Option<(usize, usize)>,
        branch: Option<String>,
    },
    RebaseMerge {
        progress: Option<(usize, usize)>,
        branch: Option<String>,
    },
    CherryPick,
    Revert,
    Bisect,
    Detached,
}

impl RepositoryState {
    pub fn is_ongoing(&self) -> bool {
        !matches!(self, Self::Clean | Self::Detached)
    }

    pub fn banner_label(&self) -> Option<String> {
        match self {
            Self::Clean => None,
            Self::Detached => Some("DETACHED HEAD".to_string()),
            Self::Merge => Some("MERGING".to_string()),
            Self::CherryPick => Some("CHERRY-PICKING".to_string()),
            Self::Revert => Some("REVERTING".to_string()),
            Self::Bisect => Some("BISECTING".to_string()),
            Self::Rebase { progress, branch } => {
                Some(format_rebase("REBASING", *progress, branch.as_deref()))
            }
            Self::RebaseInteractive { progress, branch } => {
                Some(format_rebase("REBASING -i", *progress, branch.as_deref()))
            }
            Self::RebaseMerge { progress, branch } => {
                Some(format_rebase("REBASING", *progress, branch.as_deref()))
            }
        }
    }
}

fn format_rebase(prefix: &str, progress: Option<(usize, usize)>, branch: Option<&str>) -> String {
    match (branch, progress) {
        (Some(b), Some((cur, total))) => format!("{prefix} {b} ({cur}/{total})"),
        (Some(b), None) => format!("{prefix} {b}"),
        (None, Some((cur, total))) => format!("{prefix} ({cur}/{total})"),
        (None, None) => prefix.to_string(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RefKind {
    Head,
    LocalBranch,
    RemoteBranch,
    Tag,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefInfo {
    pub name: String,
    pub full_name: String,
    pub target_oid: Oid,
    pub kind: RefKind,
    pub upstream: Option<String>,
    pub ahead_behind: Option<(usize, usize)>,
    pub is_head: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CommitSummary {
    pub oid: Oid,
    pub parents: Vec<Oid>,
    pub author_name: String,
    pub author_email: String,
    /// Committer timestamp (seconds since epoch).
    pub timestamp: i64,
    /// Author timestamp (seconds since epoch); differs from `timestamp` after rebase / cherry-pick.
    pub author_timestamp: i64,
    pub subject: String,
    /// Message body after the subject line (may be empty). Used by the text filter.
    pub body: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RepoSnapshot {
    pub repo_name: String,
    pub workdir: PathBuf,
    pub git_dir: PathBuf,
    pub common_dir: PathBuf,
    pub state: RepositoryState,
    pub is_unborn_head: bool,
    pub head_branch: Option<String>,
    pub head_oid: Option<Oid>,
    pub refs: Vec<RefInfo>,
    pub commits: Vec<CommitSummary>,
    /// `git config user.name` (drives the `@me` user filter).
    pub user_name: Option<String>,
    /// `git config user.email`.
    pub user_email: Option<String>,
}

impl RepoSnapshot {
    /// Resolves `HEAD`, ref names, unique hex prefixes (≥ 4 chars) and
    /// `~N` / `^N` suffix chains (`HEAD~3`, `main^2`, `a1b2c3^^`) against the
    /// loaded commits (`GoToHashOrRef`).
    pub fn resolve_revision(&self, expr: &str) -> Option<Oid> {
        let expr = expr.trim();
        if expr.is_empty() {
            return None;
        }
        let split = expr.find(['~', '^']).unwrap_or(expr.len());
        let (base, mut ops) = expr.split_at(split);

        let mut cur = self.resolve_base(base)?;
        let parents_of = |oid: Oid| -> Option<&[Oid]> {
            self.commits
                .iter()
                .find(|c| c.oid == oid)
                .map(|c| c.parents.as_slice())
        };
        while let Some(op) = ops.chars().next() {
            ops = &ops[1..];
            let digits_len = ops.find(|c: char| !c.is_ascii_digit()).unwrap_or(ops.len());
            let n: Option<usize> = ops[..digits_len].parse().ok();
            ops = &ops[digits_len..];
            match op {
                '~' => {
                    for _ in 0..n.unwrap_or(1) {
                        cur = *parents_of(cur)?.first()?;
                    }
                }
                '^' => match n.unwrap_or(1) {
                    0 => {}
                    k => cur = *parents_of(cur)?.get(k - 1)?,
                },
                _ => return None,
            }
        }
        Some(cur)
    }

    fn resolve_base(&self, base: &str) -> Option<Oid> {
        if base.eq_ignore_ascii_case("HEAD") || base == "@" {
            return self.head_oid;
        }
        for kind in [RefKind::LocalBranch, RefKind::RemoteBranch, RefKind::Tag] {
            if let Some(r) = self
                .refs
                .iter()
                .find(|r| r.kind == kind && (r.name == base || r.full_name == base))
            {
                return Some(r.target_oid);
            }
        }
        if base.len() >= 4 && base.chars().all(|c| c.is_ascii_hexdigit()) {
            let mut hits = self
                .commits
                .iter()
                .filter(|c| c.oid.matches_hex_prefix(base));
            let first = hits.next()?;
            if hits.next().is_none() {
                return Some(first.oid);
            }
        }
        None
    }
}

/// Granularity of background refresh requested or triggered by FS watcher.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum RefreshScope {
    #[default]
    None,
    RefsAndGraph,
    StageStatus,
    WorktreeStatus,
    All,
}

impl RefreshScope {
    pub fn merge(self, other: Self) -> Self {
        use RefreshScope::*;
        match (self, other) {
            (None, x) | (x, None) => x,
            (All, _) | (_, All) => All,
            (a, b) if a == b => a,
            (StageStatus, WorktreeStatus) | (WorktreeStatus, StageStatus) => StageStatus,
            _ => All,
        }
    }

    pub fn needs_graph_refresh(self) -> bool {
        matches!(self, Self::RefsAndGraph | Self::All)
    }

    pub fn needs_stage_refresh(self) -> bool {
        matches!(self, Self::StageStatus | Self::WorktreeStatus | Self::All)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_refs_hashes_and_ancestry_suffixes() {
        let oid = |n: u8| Oid::from_bytes([n; 20]);
        let c = |n: u8, parents: &[u8]| CommitSummary {
            oid: oid(n),
            parents: parents.iter().map(|&p| oid(p)).collect(),
            ..Default::default()
        };
        let snap = RepoSnapshot {
            head_oid: Some(oid(4)),
            refs: vec![RefInfo {
                name: "main".into(),
                full_name: "refs/heads/main".into(),
                target_oid: oid(4),
                kind: RefKind::LocalBranch,
                upstream: None,
                ahead_behind: None,
                is_head: true,
            }],
            commits: vec![c(4, &[3, 2]), c(3, &[1]), c(2, &[1]), c(1, &[])],
            ..Default::default()
        };
        assert_eq!(snap.resolve_revision("HEAD"), Some(oid(4)));
        assert_eq!(snap.resolve_revision("main~2"), Some(oid(1)));
        assert_eq!(snap.resolve_revision("HEAD^2"), Some(oid(2)));
        assert_eq!(snap.resolve_revision("HEAD^^"), Some(oid(1)));
        assert_eq!(snap.resolve_revision("0303"), Some(oid(3)));
        assert_eq!(snap.resolve_revision("HEAD~9"), None);
        assert_eq!(snap.resolve_revision("nope"), None);
    }
}
