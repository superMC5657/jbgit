use crate::{Oid, RepositoryState};

use super::TodoEntry;

/// Returns a 7-char prefix when `rev` is a full 40-hex SHA-1 hash, otherwise `rev` as-is.
pub fn short_rev(rev: &str) -> &str {
    if rev.len() == 40 && rev.bytes().all(|b| b.is_ascii_hexdigit()) {
        &rev[..7]
    } else {
        rev
    }
}

/// `git reset` mode of the four-mode Reset dialog (`GitResetDialog`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ResetMode {
    Soft,
    Mixed,
    Hard,
    Keep,
}

impl ResetMode {
    pub const ALL: [Self; 4] = [Self::Soft, Self::Mixed, Self::Hard, Self::Keep];

    pub const fn flag(self) -> &'static str {
        match self {
            Self::Soft => "--soft",
            Self::Mixed => "--mixed",
            Self::Hard => "--hard",
            Self::Keep => "--keep",
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Soft => "Soft",
            Self::Mixed => "Mixed",
            Self::Hard => "Hard",
            Self::Keep => "Keep",
        }
    }

    pub const fn description(self) -> &'static str {
        match self {
            Self::Soft => "Files and index untouched; reset changes become Staged",
            Self::Mixed => "Files untouched; index reset, changes become Unstaged",
            Self::Hard => "Discard ALL uncommitted changes in index and working tree",
            Self::Keep => {
                "Update committed files, keep unrelated local changes (abort on conflict)"
            }
        }
    }

    pub const fn is_destructive(self) -> bool {
        matches!(self, Self::Hard)
    }
}

/// Multi-step operation currently paused by Git (sequencer state).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OngoingKind {
    Rebase,
    Merge,
    CherryPick,
    Revert,
}

impl OngoingKind {
    pub fn from_state(state: &RepositoryState) -> Option<Self> {
        match state {
            RepositoryState::Merge => Some(Self::Merge),
            RepositoryState::Rebase { .. }
            | RepositoryState::RebaseInteractive { .. }
            | RepositoryState::RebaseMerge { .. } => Some(Self::Rebase),
            RepositoryState::CherryPick => Some(Self::CherryPick),
            RepositoryState::Revert => Some(Self::Revert),
            _ => None,
        }
    }

    pub const fn command(self) -> &'static str {
        match self {
            Self::Rebase => "rebase",
            Self::Merge => "merge",
            Self::CherryPick => "cherry-pick",
            Self::Revert => "revert",
        }
    }

    pub const fn title(self) -> &'static str {
        match self {
            Self::Rebase => "Rebase",
            Self::Merge => "Merge",
            Self::CherryPick => "Cherry-pick",
            Self::Revert => "Revert",
        }
    }

    /// `git merge --skip` does not exist.
    pub const fn can_skip(self) -> bool {
        !matches!(self, Self::Merge)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OngoingStep {
    Continue,
    Skip,
    Abort,
}

impl OngoingStep {
    pub const fn flag(self) -> &'static str {
        match self {
            Self::Continue => "--continue",
            Self::Skip => "--skip",
            Self::Abort => "--abort",
        }
    }
}

/// Domain-level specification of what patch should be copied to the clipboard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PatchSource {
    /// Selected commits (`oids` newest-first, optional exclusive `base` parent).
    Commits { oids: Vec<Oid>, base: Option<Oid> },
    /// Single-file patch from one commit.
    CommitFile { oid: Oid, path: String },
    /// Staged (`--cached`) or working-tree diff for selected paths.
    StagePaths { staged: bool, paths: Vec<String> },
}

/// High-level write operation dispatched by menus / dialogs and executed by the
/// engine as one or more `git` CLI steps (streamed to the Console tab).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitOp {
    Checkout {
        target: String,
        detach: bool,
    },
    ForceCheckout {
        target: String,
    },
    CheckoutNewBranch {
        name: String,
        start: String,
        track: bool,
    },
    CheckoutResetBranch {
        name: String,
        start: String,
    },
    CreateBranch {
        name: String,
        start: String,
    },
    RenameBranch {
        old: String,
        new: String,
    },
    DeleteBranch {
        name: String,
        force: bool,
        tip: Oid,
    },
    RestoreBranch {
        name: String,
        oid: Oid,
    },
    DeleteRemoteBranch {
        remote: String,
        branch: String,
    },
    CreateTag {
        name: String,
        target: Oid,
        message: Option<String>,
    },
    Merge {
        rev: String,
    },
    Rebase {
        onto: String,
        branch: Option<String>,
    },
    CherryPick {
        oids: Vec<Oid>,
    },
    Revert {
        oids: Vec<Oid>,
    },
    Reset {
        mode: ResetMode,
        target: Oid,
    },
    Fetch,
    Pull {
        rebase: bool,
    },
    Push {
        branch: String,
        remote: String,
        set_upstream: bool,
        force_with_lease: bool,
        up_to: Option<Oid>,
    },
    UpdateBranch {
        branch: String,
        remote: String,
        remote_branch: String,
    },
    RewordHead {
        message: String,
    },
    RewriteHistory {
        base: Option<Oid>,
        todo: Vec<TodoEntry>,
    },
    FixupCommit {
        target: Oid,
        base: Option<Oid>,
        autosquash: bool,
    },
    Ongoing {
        kind: OngoingKind,
        step: OngoingStep,
    },
    StashPush {
        message: Option<String>,
        include_untracked: bool,
        paths: Vec<String>,
    },
    StashApply {
        index: usize,
        pop: bool,
    },
    StashDrop {
        index: usize,
    },
    StashClear,
    StashBranch {
        index: usize,
        name: String,
    },
    AcceptSide {
        paths: Vec<String>,
        ours: bool,
    },
    IntentToAdd {
        paths: Vec<String>,
    },
    AddToIgnore {
        patterns: Vec<String>,
        exclude_file: bool,
    },
    DeleteFiles {
        paths: Vec<String>,
    },
    CreatePatchFiles {
        oids: Vec<Oid>,
        base: Option<Oid>,
    },
    PatchToClipboard {
        source: PatchSource,
    },
    CheckoutPaths {
        rev: String,
        paths: Vec<String>,
    },
    Smart(Box<GitOp>),
}

impl GitOp {
    /// Short human label (`Checkout 'main'`) used for notices and Smart dialog titles.
    pub fn label(&self) -> String {
        match self {
            Self::Checkout { target, .. } => format!("Checkout '{target}'"),
            Self::ForceCheckout { target } => format!("Force Checkout '{target}'"),
            Self::CheckoutNewBranch { name, .. } => format!("Checkout new branch '{name}'"),
            Self::CheckoutResetBranch { name, .. } => format!("Checkout and reset '{name}'"),
            Self::CreateBranch { name, .. } => format!("Create branch '{name}'"),
            Self::RenameBranch { old, new } => format!("Rename '{old}' → '{new}'"),
            Self::DeleteBranch { name, .. } => format!("Delete branch '{name}'"),
            Self::RestoreBranch { name, .. } => format!("Restore branch '{name}'"),
            Self::DeleteRemoteBranch { remote, branch } => {
                format!("Delete remote branch '{remote}/{branch}'")
            }
            Self::CreateTag { name, .. } => format!("Create tag '{name}'"),
            Self::Merge { rev } => format!("Merge '{rev}'"),
            Self::Rebase { onto, branch } => match branch {
                Some(b) => format!("Rebase '{b}' onto '{onto}'"),
                None => format!("Rebase onto '{onto}'"),
            },
            Self::CherryPick { oids } => format!("Cherry-pick {} commit(s)", oids.len()),
            Self::Revert { oids } => format!("Revert {} commit(s)", oids.len()),
            Self::Reset { mode, target } => {
                format!("Reset ({}) to {}", mode.label(), target.short())
            }
            Self::Fetch => "Fetch".to_string(),
            Self::Pull { rebase } => {
                if *rebase {
                    "Pull (rebase)".to_string()
                } else {
                    "Pull".to_string()
                }
            }
            Self::Push { branch, .. } => format!("Push '{branch}'"),
            Self::UpdateBranch { branch, .. } => format!("Update '{branch}'"),
            Self::RewordHead { .. } => "Reword HEAD".to_string(),
            Self::RewriteHistory { todo, .. } => format!("Rewrite {} commit(s)", todo.len()),
            Self::FixupCommit { target, .. } => format!("Fixup {}", target.short()),
            Self::Ongoing { kind, step } => {
                format!("{} {}", kind.title(), step.flag().trim_start_matches('-'))
            }
            Self::StashPush { .. } => "Stash".to_string(),
            Self::StashApply { index, pop } => {
                format!("{} stash@{{{index}}}", if *pop { "Pop" } else { "Apply" })
            }
            Self::StashDrop { index } => format!("Drop stash@{{{index}}}"),
            Self::StashClear => "Clear stashes".to_string(),
            Self::StashBranch { name, .. } => format!("Unstash as branch '{name}'"),
            Self::AcceptSide { ours, .. } => {
                if *ours {
                    "Accept Yours".to_string()
                } else {
                    "Accept Theirs".to_string()
                }
            }
            Self::IntentToAdd { .. } => "Stage without content".to_string(),
            Self::AddToIgnore { .. } => "Add to ignore".to_string(),
            Self::DeleteFiles { paths } => format!("Delete {} file(s)", paths.len()),
            Self::CreatePatchFiles { .. } => "Create patch".to_string(),
            Self::PatchToClipboard { .. } => "Copy patch".to_string(),
            Self::CheckoutPaths { rev, paths } => {
                let short_rev = short_rev(rev);
                if let [one] = paths.as_slice() {
                    format!("Restore '{one}' from {short_rev}")
                } else {
                    format!("Restore {} file(s) from {short_rev}", paths.len())
                }
            }
            Self::Smart(inner) => format!("Smart {}", inner.label()),
        }
    }

    pub fn supports_smart(&self) -> bool {
        matches!(
            self,
            Self::Checkout { .. }
                | Self::CheckoutNewBranch { .. }
                | Self::CheckoutResetBranch { .. }
                | Self::Merge { .. }
                | Self::Rebase { .. }
                | Self::Pull { .. }
                | Self::CherryPick { .. }
                | Self::Revert { .. }
                | Self::Reset {
                    mode: ResetMode::Keep,
                    ..
                }
        )
    }

    pub fn is_read_only(&self) -> bool {
        matches!(self, Self::PatchToClipboard { .. })
    }
}
