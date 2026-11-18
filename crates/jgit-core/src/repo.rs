use std::path::PathBuf;

use crate::Oid;

/// Ongoing Git repository workflow / state indicator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RepositoryState {
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
            Self::Rebase { progress, branch } => Some(format_rebase("REBASING", *progress, branch.as_deref())),
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitSummary {
    pub oid: Oid,
    pub parents: Vec<Oid>,
    pub author_name: String,
    pub author_email: String,
    pub timestamp: i64,
    pub subject: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
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
