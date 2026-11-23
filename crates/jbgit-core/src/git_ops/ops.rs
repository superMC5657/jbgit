use crate::{Oid, RefreshScope, RepositoryState};

use super::TodoEntry;

/// Tag push mode for [`GitOp::Push`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum PushTagMode {
    #[default]
    None,
    /// `--follow-tags`
    Follow,
    /// `--tags`
    All,
}

impl PushTagMode {
    pub const ALL: [Self; 3] = [Self::None, Self::Follow, Self::All];

    pub const fn flag(self) -> Option<&'static str> {
        match self {
            Self::None => None,
            Self::Follow => Some("--follow-tags"),
            Self::All => Some("--tags"),
        }
    }

    pub const fn next(self) -> Self {
        match self {
            Self::None => Self::Follow,
            Self::Follow => Self::All,
            Self::All => Self::None,
        }
    }
}

/// Force-push mode for [`GitOp::Push`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum PushForce {
    #[default]
    None,
    /// `--force-with-lease`
    WithLease,
    /// `--force`
    Force,
}

impl PushForce {
    pub const ALL: [Self; 3] = [Self::None, Self::WithLease, Self::Force];

    pub const fn flag(self) -> Option<&'static str> {
        match self {
            Self::None => None,
            Self::WithLease => Some("--force-with-lease"),
            Self::Force => Some("--force"),
        }
    }

    pub const fn is_force(self) -> bool {
        !matches!(self, Self::None)
    }

    pub const fn next(self) -> Self {
        match self {
            Self::None => Self::WithLease,
            Self::WithLease => Self::Force,
            Self::Force => Self::None,
        }
    }
}

/// Fast-forward policy of [`MergeOptions`] (`GitMergeOption.NO_FF` / `FF_ONLY`).
///
/// `--no-ff` and `--ff-only` are mutually exclusive by construction.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum FastForward {
    /// Git default: fast-forward when possible, otherwise create a merge commit.
    #[default]
    Default,
    /// `--no-ff`
    NoFf,
    /// `--ff-only`
    FfOnly,
}

impl FastForward {
    pub const ALL: [Self; 3] = [Self::Default, Self::NoFf, Self::FfOnly];

    pub const fn flag(self) -> Option<&'static str> {
        match self {
            Self::Default => None,
            Self::NoFf => Some("--no-ff"),
            Self::FfOnly => Some("--ff-only"),
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::NoFf => "no-ff",
            Self::FfOnly => "ff-only",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|v| v.as_str() == s.trim())
    }

    pub const fn next(self) -> Self {
        match self {
            Self::Default => Self::NoFf,
            Self::NoFf => Self::FfOnly,
            Self::FfOnly => Self::Default,
        }
    }

    pub const fn prev(self) -> Self {
        match self {
            Self::Default => Self::FfOnly,
            Self::NoFf => Self::Default,
            Self::FfOnly => Self::NoFf,
        }
    }
}

/// A pair of mutually exclusive merge options (`GitMergeOption.isOptionSuitable`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MergeOptionConflict {
    /// `--squash` with `--no-ff`
    SquashNoFf,
    /// `--squash` with `--ff-only`
    SquashFfOnly,
    /// `--squash` with `-m`
    SquashMessage,
    /// `--ff-only` with `-m`
    FfOnlyMessage,
    /// `--ff-only` with `--no-commit`
    FfOnlyNoCommit,
    /// `-m` with `--no-commit`
    MessageNoCommit,
}

impl MergeOptionConflict {
    /// The two clashing command-line flags.
    pub const fn flags(self) -> (&'static str, &'static str) {
        match self {
            Self::SquashNoFf => ("--squash", "--no-ff"),
            Self::SquashFfOnly => ("--squash", "--ff-only"),
            Self::SquashMessage => ("--squash", "-m"),
            Self::FfOnlyMessage => ("--ff-only", "-m"),
            Self::FfOnlyNoCommit => ("--ff-only", "--no-commit"),
            Self::MessageNoCommit => ("-m", "--no-commit"),
        }
    }
}

/// Options of the Merge dialog (`GitMergeDialog`), compiled by the engine into
/// `git merge` flags. [`MergeOptions::default`] keeps the legacy
/// `merge --no-edit <rev>` command line.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct MergeOptions {
    pub ff: FastForward,
    /// `--squash`
    pub squash: bool,
    /// `--no-commit`
    pub no_commit: bool,
    /// Custom commit message (passed as a single `-m` argument; multi-line is safe).
    pub message: Option<String>,
    /// `-s <strategy>`
    pub strategy: Option<String>,
    /// `-X <option>`
    pub strategy_option: Option<String>,
    /// `--allow-unrelated-histories`
    pub allow_unrelated: bool,
}

impl MergeOptions {
    /// Custom message with surrounding blank text removed (`None` when empty).
    pub fn effective_message(&self) -> Option<&str> {
        self.message
            .as_deref()
            .map(str::trim)
            .filter(|m| !m.is_empty())
    }

    /// Checks the `GitMergeOption` incompatibility table; returns the first clash.
    pub fn validate(&self) -> Result<(), MergeOptionConflict> {
        let has_msg = self.effective_message().is_some();
        if self.squash && self.ff == FastForward::NoFf {
            return Err(MergeOptionConflict::SquashNoFf);
        }
        if self.squash && self.ff == FastForward::FfOnly {
            return Err(MergeOptionConflict::SquashFfOnly);
        }
        if self.squash && has_msg {
            return Err(MergeOptionConflict::SquashMessage);
        }
        if self.ff == FastForward::FfOnly && has_msg {
            return Err(MergeOptionConflict::FfOnlyMessage);
        }
        if self.ff == FastForward::FfOnly && self.no_commit {
            return Err(MergeOptionConflict::FfOnlyNoCommit);
        }
        if has_msg && self.no_commit {
            return Err(MergeOptionConflict::MessageNoCommit);
        }
        Ok(())
    }

    /// Whether the merge leaves its result staged for a manual commit.
    pub const fn leaves_changes_uncommitted(&self) -> bool {
        self.squash || self.no_commit
    }

    /// Compact flag summary (`--no-ff -s ours`), empty for defaults.
    pub fn summary(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if let Some(f) = self.ff.flag() {
            parts.push(f.to_string());
        }
        if self.squash {
            parts.push("--squash".into());
        }
        if self.no_commit {
            parts.push("--no-commit".into());
        }
        if let Some(s) = self.strategy.as_deref().filter(|s| !s.is_empty()) {
            parts.push(format!("-s {s}"));
        }
        if let Some(x) = self.strategy_option.as_deref().filter(|s| !s.is_empty()) {
            parts.push(format!("-X {x}"));
        }
        if self.allow_unrelated {
            parts.push("--allow-unrelated-histories".into());
        }
        if self.effective_message().is_some() {
            parts.push("-m".into());
        }
        parts.join(" ")
    }

    /// Serialises the remembered choices (never the message) as `key=value` lines.
    pub fn to_remembered(&self) -> String {
        format!(
            "ff={}\nsquash={}\nno_commit={}\nstrategy={}\nstrategy_option={}\nallow_unrelated={}\n",
            self.ff.as_str(),
            self.squash,
            self.no_commit,
            self.strategy.as_deref().unwrap_or(""),
            self.strategy_option.as_deref().unwrap_or(""),
            self.allow_unrelated,
        )
    }

    /// Parses [`Self::to_remembered`] output; unknown keys are ignored.
    pub fn from_remembered(text: &str) -> Self {
        let mut out = Self::default();
        for line in text.lines() {
            let Some((k, v)) = line.split_once('=') else {
                continue;
            };
            let v = v.trim();
            let opt = || (!v.is_empty()).then(|| v.to_string());
            match k.trim() {
                "ff" => out.ff = FastForward::parse(v).unwrap_or_default(),
                "squash" => out.squash = v == "true",
                "no_commit" => out.no_commit = v == "true",
                "strategy" => out.strategy = opt(),
                "strategy_option" => out.strategy_option = opt(),
                "allow_unrelated" => out.allow_unrelated = v == "true",
                _ => {}
            }
        }
        out
    }
}

/// How Update Project integrates upstream changes into the current branch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UpdateMode {
    /// `merge --no-edit @{u}`
    Merge,
    /// `rebase @{u}`
    Rebase,
}

impl UpdateMode {
    /// Interprets a `pull.rebase` config value (`true` / `merges` / `interactive` → rebase).
    pub fn from_pull_rebase(value: Option<&str>) -> Self {
        let rebase = value.is_some_and(|v| {
            matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "true" | "yes" | "on" | "1" | "merges" | "m" | "interactive" | "i"
            )
        });
        if rebase { Self::Rebase } else { Self::Merge }
    }
}

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
        force: bool,
    },
    DeleteTag {
        name: String,
        tip: Oid,
    },
    RestoreTag {
        name: String,
        oid: Oid,
    },
    DeleteRemoteTag {
        remote: String,
        name: String,
    },
    PushTag {
        remote: String,
        name: String,
        force: bool,
    },
    /// `git remote add <name> <url>`.
    AddRemote {
        name: String,
        url: String,
    },
    /// `git remote rename <old> <new>` (remote-tracking refs follow).
    RenameRemote {
        old: String,
        new: String,
    },
    /// `git remote remove <name>` (drops its remote-tracking refs).
    RemoveRemote {
        name: String,
    },
    /// `git remote set-url [--push] <name> <url>`.
    SetRemoteUrl {
        name: String,
        url: String,
        push: bool,
    },
    /// `git fetch --prune <name>` for one remote ([`GitOp::Fetch`] stays `--all`).
    FetchRemote {
        name: String,
    },
    /// `git worktree add [-b <name>|--detach] -- <path> [<commit-ish>]`.
    AddWorktree {
        path: std::path::PathBuf,
        branch: crate::WorktreeBranch,
    },
    /// `git worktree remove [--force] -- <path>`.
    RemoveWorktree {
        path: std::path::PathBuf,
        force: bool,
    },
    /// `git worktree lock [--reason <reason>] -- <path>`.
    LockWorktree {
        path: std::path::PathBuf,
        reason: Option<String>,
    },
    /// `git worktree unlock -- <path>`.
    UnlockWorktree {
        path: std::path::PathBuf,
    },
    /// `git worktree prune --verbose`.
    PruneWorktrees,
    Merge {
        rev: String,
        options: MergeOptions,
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
        remote_branch: String,
        set_upstream: bool,
        force: PushForce,
        tags: PushTagMode,
        up_to: Option<Oid>,
    },
    Sequence(Vec<GitOp>),
    /// Update Project: save changes → `fetch --all --prune` → merge / rebase the
    /// current branch onto its upstream → fast-forward other tracked branches →
    /// restore changes. `mode: None` follows `pull.rebase`.
    UpdateProject {
        mode: Option<UpdateMode>,
        save_changes: bool,
        update_other_branches: bool,
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
    /// `git apply [--index | --3way] --whitespace=nowarn` (clipboard on stdin).
    ApplyPatch {
        source: crate::PatchInput,
        mode: crate::ApplyMode,
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
            Self::DeleteTag { name, .. } => format!("Delete tag '{name}'"),
            Self::RestoreTag { name, .. } => format!("Restore tag '{name}'"),
            Self::DeleteRemoteTag { remote, name } => {
                format!("Delete remote tag '{remote}/{name}'")
            }
            Self::PushTag { remote, name, .. } => format!("Push tag '{name}' to '{remote}'"),
            Self::AddRemote { name, .. } => format!("Add remote '{name}'"),
            Self::RenameRemote { old, new } => format!("Rename remote '{old}' → '{new}'"),
            Self::RemoveRemote { name } => format!("Remove remote '{name}'"),
            Self::SetRemoteUrl { name, push, .. } => {
                if *push {
                    format!("Set push URL of '{name}'")
                } else {
                    format!("Set URL of '{name}'")
                }
            }
            Self::FetchRemote { name } => format!("Fetch '{name}'"),
            Self::AddWorktree { path, branch } => match branch.branch_name() {
                Some(b) => format!("New worktree '{}' on '{b}'", path.display()),
                None => format!("New worktree '{}' (detached)", path.display()),
            },
            Self::RemoveWorktree { path, force } => {
                let f = if *force { " (force)" } else { "" };
                format!("Remove worktree '{}'{f}", path.display())
            }
            Self::LockWorktree { path, .. } => format!("Lock worktree '{}'", path.display()),
            Self::UnlockWorktree { path } => format!("Unlock worktree '{}'", path.display()),
            Self::PruneWorktrees => "Prune worktrees".to_string(),
            Self::Merge { rev, options } => {
                if options.squash {
                    format!("Squash merge '{rev}'")
                } else {
                    format!("Merge '{rev}'")
                }
            }
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
            Self::Push {
                branch,
                remote,
                remote_branch,
                ..
            } => {
                let target = if remote_branch.is_empty() {
                    branch.as_str()
                } else {
                    remote_branch.as_str()
                };
                format!("Push '{branch}' → '{remote}/{target}'")
            }
            Self::Sequence(ops) => ops
                .iter()
                .map(Self::label)
                .collect::<Vec<_>>()
                .join(" → "),
            Self::UpdateProject { .. } => "Update Project".to_string(),
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
            Self::ApplyPatch { source, .. } => match source {
                crate::PatchInput::File(path) => {
                    let name = path
                        .file_name()
                        .map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into());
                    format!("Apply patch '{name}'")
                }
                crate::PatchInput::Clipboard => "Apply patch from clipboard".to_string(),
            },
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

    /// Operations that talk to a remote (credential prompts may be needed).
    pub fn is_network(&self) -> bool {
        match self {
            Self::Fetch
            | Self::FetchRemote { .. }
            | Self::Pull { .. }
            | Self::Push { .. }
            | Self::PushTag { .. }
            | Self::DeleteRemoteTag { .. }
            | Self::DeleteRemoteBranch { .. }
            | Self::UpdateBranch { .. }
            | Self::UpdateProject { .. } => true,
            Self::Sequence(ops) => ops.iter().any(Self::is_network),
            Self::Smart(inner) => inner.is_network(),
            _ => false,
        }
    }

    pub fn is_read_only(&self) -> bool {
        match self {
            Self::PatchToClipboard { .. } => true,
            Self::Sequence(ops) => ops.iter().all(Self::is_read_only),
            _ => false,
        }
    }

    /// Minimal [`RefreshScope`] needed after running this operation.
    pub fn refresh_scope(&self) -> RefreshScope {
        match self {
            Self::PatchToClipboard { .. } => RefreshScope::None,
            Self::CreateBranch { .. }
            | Self::RenameBranch { .. }
            | Self::DeleteBranch { .. }
            | Self::RestoreBranch { .. }
            | Self::DeleteRemoteBranch { .. }
            | Self::CreateTag { .. }
            | Self::DeleteTag { .. }
            | Self::RestoreTag { .. }
            | Self::DeleteRemoteTag { .. }
            | Self::PushTag { .. }
            | Self::AddRemote { .. }
            | Self::RenameRemote { .. }
            | Self::RemoveRemote { .. }
            | Self::SetRemoteUrl { .. }
            | Self::FetchRemote { .. }
            | Self::AddWorktree { .. }
            | Self::RemoveWorktree { .. }
            | Self::LockWorktree { .. }
            | Self::UnlockWorktree { .. }
            | Self::PruneWorktrees
            | Self::Fetch
            | Self::Push { .. }
            | Self::UpdateBranch { .. } => RefreshScope::RefsAndGraph,
            Self::Sequence(ops) => ops
                .iter()
                .fold(RefreshScope::None, |acc, op| acc.merge(op.refresh_scope())),
            _ => RefreshScope::All,
        }
    }
}
