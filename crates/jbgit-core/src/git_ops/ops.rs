use crate::{GitVersion, Oid, RefreshScope, RepositoryState};

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

/// A pair of mutually exclusive pull options (`GitPullOption.isOptionSuitable`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PullOptionConflict {
    /// `--rebase` with `--squash`
    RebaseSquash,
    /// `--rebase` with `--no-ff`
    RebaseNoFf,
    /// `--rebase` with `--ff-only`
    RebaseFfOnly,
    /// `--rebase` with `--no-commit`
    RebaseNoCommit,
    /// `--squash` with `--no-ff`
    SquashNoFf,
    /// `--squash` with `--ff-only`
    SquashFfOnly,
}

impl PullOptionConflict {
    /// The two clashing command-line flags.
    pub const fn flags(self) -> (&'static str, &'static str) {
        match self {
            Self::RebaseSquash => ("--rebase", "--squash"),
            Self::RebaseNoFf => ("--rebase", "--no-ff"),
            Self::RebaseFfOnly => ("--rebase", "--ff-only"),
            Self::RebaseNoCommit => ("--rebase", "--no-commit"),
            Self::SquashNoFf => ("--squash", "--no-ff"),
            Self::SquashFfOnly => ("--squash", "--ff-only"),
        }
    }
}

/// Options of the Pull dialog (`GitPullDialog`), compiled by the engine into
/// `git pull` flags. [`PullOptions::default`] keeps the legacy `pull --no-edit`
/// command line.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct PullOptions {
    /// Explicit remote name (`None` = Git upstream default).
    pub remote: Option<String>,
    /// Explicit remote branch name (`None` = Git upstream default).
    pub branch: Option<String>,
    /// `--rebase`
    pub rebase: bool,
    /// `--no-ff` / `--ff-only`
    pub ff: FastForward,
    /// `--squash`
    pub squash: bool,
    /// `--no-commit`
    pub no_commit: bool,
    /// `--no-verify`
    pub no_verify: bool,
}

impl PullOptions {
    pub fn effective_remote(&self) -> Option<&str> {
        self.remote
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
    }

    pub fn effective_branch(&self) -> Option<&str> {
        self.branch
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
    }

    /// Checks the `GitPullOption` incompatibility matrix; returns the first clash.
    pub fn validate(&self) -> Result<(), PullOptionConflict> {
        if self.rebase {
            if self.squash {
                return Err(PullOptionConflict::RebaseSquash);
            }
            match self.ff {
                FastForward::NoFf => return Err(PullOptionConflict::RebaseNoFf),
                FastForward::FfOnly => return Err(PullOptionConflict::RebaseFfOnly),
                FastForward::Default => {}
            }
            if self.no_commit {
                return Err(PullOptionConflict::RebaseNoCommit);
            }
        }
        if self.squash {
            match self.ff {
                FastForward::NoFf => return Err(PullOptionConflict::SquashNoFf),
                FastForward::FfOnly => return Err(PullOptionConflict::SquashFfOnly),
                FastForward::Default => {}
            }
        }
        Ok(())
    }

    /// Whether the pull leaves its result staged for a manual commit.
    pub const fn leaves_changes_uncommitted(&self) -> bool {
        !self.rebase && (self.squash || self.no_commit)
    }

    /// Compact command-line preview of flags + target (`--rebase origin main`).
    pub fn summary(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if self.rebase {
            parts.push("--rebase".into());
        } else {
            if let Some(f) = self.ff.flag() {
                parts.push(f.to_string());
            }
            if self.squash {
                parts.push("--squash".into());
            }
            if self.no_commit {
                parts.push("--no-commit".into());
            }
        }
        if self.no_verify {
            parts.push("--no-verify".into());
        }
        if let Some(remote) = self.effective_remote() {
            parts.push(remote.to_string());
            if let Some(branch) = self.effective_branch() {
                parts.push(branch.to_string());
            }
        }
        parts.join(" ")
    }

    /// Serialises remembered flag choices (never remote/branch) as `key=value` lines.
    pub fn to_remembered(&self) -> String {
        format!(
            "rebase={}\nff={}\nsquash={}\nno_commit={}\nno_verify={}\n",
            self.rebase,
            self.ff.as_str(),
            self.squash,
            self.no_commit,
            self.no_verify,
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
            match k.trim() {
                "rebase" => out.rebase = v == "true",
                "ff" => out.ff = FastForward::parse(v).unwrap_or_default(),
                "squash" => out.squash = v == "true",
                "no_commit" => out.no_commit = v == "true",
                "no_verify" => out.no_verify = v == "true",
                _ => {}
            }
        }
        out
    }
}

/// Validation failure of [`RebaseOptions`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RebaseOptionError {
    /// Target (`onto`) is required unless `--root` is set.
    MissingOnto,
    /// `--root` cannot be combined with an explicit `<upstream>` old base.
    RootWithUpstream,
    /// `--update-refs` requires Git >= 2.38.
    UpdateRefsUnsupported(GitVersion),
    /// Combining `--rebase-merges` with `-i` requires Git >= 2.18.
    RebaseMergesUnsupported(GitVersion),
}

/// Options of the standard Rebase dialog (`GitRebaseDialog`), compiled by the
/// engine into `git rebase` flags.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct RebaseOptions {
    /// Target new base (`git rebase <onto>` or `git rebase --onto <onto> <upstream>`).
    /// May be empty only when `root == true`.
    pub onto: String,
    /// Optional old base when using `--onto <onto> <upstream>`.
    pub upstream: Option<String>,
    /// Optional branch to switch to and rebase (`git rebase ... <branch>`).
    pub branch: Option<String>,
    /// `-i` / `--interactive`
    pub interactive: bool,
    /// `--rebase-merges`
    pub rebase_merges: bool,
    /// `--update-refs` (Git >= 2.38)
    pub update_refs: bool,
    /// `--keep-empty`
    pub keep_empty: bool,
    /// `--autosquash`
    pub autosquash: bool,
    /// `--root`
    pub root: bool,
}

impl RebaseOptions {
    /// Convenience constructor for simple `git rebase <onto> [<branch>]`.
    pub fn onto(onto: impl Into<String>, branch: Option<String>) -> Self {
        Self {
            onto: onto.into(),
            branch,
            ..Self::default()
        }
    }

    pub fn effective_onto(&self) -> Option<&str> {
        let s = self.onto.trim();
        (!s.is_empty()).then_some(s)
    }

    pub fn effective_upstream(&self) -> Option<&str> {
        self.upstream
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
    }

    pub fn effective_branch(&self) -> Option<&str> {
        self.branch
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
    }

    /// Validates option combinations and Git version requirements.
    pub fn validate(&self, git_version: Option<GitVersion>) -> Result<(), RebaseOptionError> {
        if self.root && self.effective_upstream().is_some() {
            return Err(RebaseOptionError::RootWithUpstream);
        }
        if !self.root && self.effective_onto().is_none() {
            return Err(RebaseOptionError::MissingOnto);
        }
        if self.update_refs
            && let Some(v) = git_version
            && !v.supports_rebase_update_refs()
        {
            return Err(RebaseOptionError::UpdateRefsUnsupported(v));
        }
        if self.rebase_merges
            && let Some(v) = git_version
            && !v.supports_rebase_merges()
        {
            return Err(RebaseOptionError::RebaseMergesUnsupported(v));
        }
        Ok(())
    }

    /// Compact command-line preview of arguments after `git rebase`.
    pub fn summary(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if self.interactive {
            parts.push("-i".into());
        }
        if self.rebase_merges {
            parts.push("--rebase-merges".into());
        }
        if self.update_refs {
            parts.push("--update-refs".into());
        }
        if self.keep_empty {
            parts.push("--keep-empty".into());
        }
        if self.autosquash {
            parts.push("--autosquash".into());
        }
        if self.root {
            if let Some(onto) = self.effective_onto() {
                parts.push("--onto".into());
                parts.push(onto.to_string());
            }
            parts.push("--root".into());
        } else if let Some(upstream) = self.effective_upstream() {
            if let Some(onto) = self.effective_onto() {
                parts.push("--onto".into());
                parts.push(onto.to_string());
            }
            parts.push(upstream.to_string());
        } else if let Some(onto) = self.effective_onto() {
            parts.push(onto.to_string());
        }
        if let Some(branch) = self.effective_branch() {
            parts.push(branch.to_string());
        }
        parts.join(" ")
    }

    /// Serialises remembered flag choices (never branch/onto/upstream) as `key=value` lines.
    pub fn to_remembered(&self) -> String {
        format!(
            "interactive={}\nrebase_merges={}\nupdate_refs={}\nkeep_empty={}\nautosquash={}\nroot={}\n",
            self.interactive,
            self.rebase_merges,
            self.update_refs,
            self.keep_empty,
            self.autosquash,
            self.root,
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
            match k.trim() {
                "interactive" => out.interactive = v == "true",
                "rebase_merges" => out.rebase_merges = v == "true",
                "update_refs" => out.update_refs = v == "true",
                "keep_empty" => out.keep_empty = v == "true",
                "autosquash" => out.autosquash = v == "true",
                "root" => out.root = v == "true",
                _ => {}
            }
        }
        out
    }
}

/// Kind of autosquash commit created by [`GitOp::FixupCommit`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum FixupKind {
    /// `git commit --fixup=<target>` (`fixup! <subject>`)
    #[default]
    Fixup,
    /// `git commit --squash=<target>` (`squash! <subject>`)
    Squash,
}

impl FixupKind {
    pub const fn flag_prefix(self) -> &'static str {
        match self {
            Self::Fixup => "--fixup=",
            Self::Squash => "--squash=",
        }
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

/// Domain-level specification of what patch should be generated (saved to file or copied to clipboard).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PatchSource {
    /// Selected commits (`oids` newest-first, optional exclusive `base` parent).
    Commits { oids: Vec<Oid>, base: Option<Oid> },
    /// Single-file patch from one commit.
    CommitFile { oid: Oid, path: String },
    /// Stage selection: staged (`HEAD..index`), unstaged (`index..worktree`),
    /// and untracked files. A path present in both `staged` and `unstaged`
    /// emits the combined `HEAD..worktree` diff.
    StagePaths {
        staged: Vec<String>,
        unstaged: Vec<String>,
        untracked: Vec<String>,
    },
}

impl PatchSource {
    pub fn is_empty(&self) -> bool {
        match self {
            Self::Commits { oids, .. } => oids.is_empty(),
            Self::CommitFile { path, .. } => path.is_empty(),
            Self::StagePaths {
                staged,
                unstaged,
                untracked,
            } => staged.is_empty() && unstaged.is_empty() && untracked.is_empty(),
        }
    }

    /// Suggested default `.patch` filename (never containing path separators).
    pub fn suggested_filename(&self, repo_name: &str) -> String {
        let safe_repo: String = repo_name
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
            .collect();
        let safe_repo = if safe_repo.is_empty() {
            "changes".to_string()
        } else {
            safe_repo
        };
        match self {
            Self::Commits { oids, .. } => match oids.as_slice() {
                [one] => format!("{}.patch", one.short()),
                [newest, ..] => format!("{safe_repo}-{}-commits-{}.patch", oids.len(), newest.short()),
                [] => format!("{safe_repo}.patch"),
            },
            Self::CommitFile { oid, path } => {
                let stem = path
                    .rsplit('/')
                    .next()
                    .unwrap_or("file")
                    .replace(['\\', '/', ':', '*', '?', '"', '<', '>', '|', ' '], "_");
                format!("{}-{stem}.patch", oid.short())
            }
            Self::StagePaths { .. } => format!("{safe_repo}-changes.patch"),
        }
    }
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
        options: RebaseOptions,
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
        options: PullOptions,
    },
    Push {
        branch: String,
        remote: String,
        remote_branch: String,
        set_upstream: bool,
        force: PushForce,
        tags: PushTagMode,
        up_to: Option<Oid>,
        no_verify: bool,
        expected_remote_oid: Option<Oid>,
    },
    Sequence(Vec<GitOp>),
    /// Update Project: save changes → `fetch --all --prune` → merge / rebase the
    /// current branch onto its upstream → fast-forward other tracked branches →
    /// optional submodule update → restore changes. `mode: None` follows `pull.rebase`.
    UpdateProject {
        mode: Option<UpdateMode>,
        save_changes: bool,
        update_other_branches: bool,
        update_submodules: bool,
    },
    /// `git config [--global] user.name <name>` and `user.email <email>`.
    SetIdentity {
        name: String,
        email: String,
        global: bool,
    },
    /// `git branch --set-upstream-to=<upstream> <branch>`.
    SetUpstream {
        branch: String,
        upstream: String,
    },
    UpdateBranch {
        branch: String,
        remote: String,
        remote_branch: String,
    },
    RewordHead {
        message: String,
        no_verify: bool,
    },
    RewriteHistory {
        base: Option<Oid>,
        todo: Vec<TodoEntry>,
        no_verify: bool,
    },
    FixupCommit {
        target: Oid,
        kind: FixupKind,
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
    /// `git checkout -m -- <paths...>` — recreates conflict markers for
    /// previously resolved/staged files.
    UnresolveConflict {
        paths: Vec<String>,
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
    CreatePatchFile {
        source: PatchSource,
        output: std::path::PathBuf,
        reverse: bool,
    },
    PatchToClipboard {
        source: PatchSource,
        reverse: bool,
    },
    CheckoutPaths {
        rev: String,
        paths: Vec<String>,
    },
    /// `git restore --source=<rev> [--staged] [--worktree] -- <paths...>`.
    RestorePaths {
        rev: String,
        paths: Vec<String>,
        staged: bool,
        worktree: bool,
    },
    /// `git apply [--index | --3way] [-R] [-p<n>] [--directory=<dir>] [--include=<p>...] --whitespace=nowarn`.
    ApplyPatch {
        source: crate::PatchInput,
        options: crate::ApplyOptions,
    },
    /// `git submodule update --init --recursive [-- <path>]`.
    SubmoduleUpdate {
        path: Option<String>,
    },
    /// `git fetch --unshallow --progress <remote>`.
    Unshallow {
        remote: String,
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
            Self::Rebase { options } => {
                let target = options
                    .effective_onto()
                    .unwrap_or(if options.root { "--root" } else { "HEAD" });
                match options.effective_branch() {
                    Some(b) => format!("Rebase '{b}' onto '{target}'"),
                    None => format!("Rebase onto '{target}'"),
                }
            }
            Self::CherryPick { oids } => format!("Cherry-pick {} commit(s)", oids.len()),
            Self::Revert { oids } => format!("Revert {} commit(s)", oids.len()),
            Self::Reset { mode, target } => {
                format!("Reset ({}) to {}", mode.label(), target.short())
            }
            Self::Fetch => "Fetch".to_string(),
            Self::Pull { options } => {
                if options.rebase {
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
            Self::SetIdentity { .. } => "Configure Git identity".to_string(),
            Self::SetUpstream { branch, upstream } => {
                format!("Set upstream of '{branch}' → '{upstream}'")
            }
            Self::UpdateBranch { branch, .. } => format!("Update '{branch}'"),
            Self::RewordHead { .. } => "Reword HEAD".to_string(),
            Self::RewriteHistory { todo, .. } => format!("Rewrite {} commit(s)", todo.len()),
            Self::FixupCommit { target, kind, .. } => match kind {
                FixupKind::Fixup => format!("Fixup {}", target.short()),
                FixupKind::Squash => format!("Squash! {}", target.short()),
            },
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
            Self::UnresolveConflict { paths } => {
                if let [one] = paths.as_slice() {
                    format!("Revert resolved '{one}'")
                } else {
                    format!("Revert {} resolved file(s)", paths.len())
                }
            }
            Self::IntentToAdd { .. } => "Stage without content".to_string(),
            Self::AddToIgnore { .. } => "Add to ignore".to_string(),
            Self::DeleteFiles { paths } => format!("Delete {} file(s)", paths.len()),
            Self::CreatePatchFile { output, .. } => {
                let name = output
                    .file_name()
                    .map_or_else(|| output.display().to_string(), |n| n.to_string_lossy().into());
                format!("Create patch '{name}'")
            }
            Self::PatchToClipboard { .. } => "Copy patch".to_string(),
            Self::CheckoutPaths { rev, paths }
            | Self::RestorePaths { rev, paths, .. } => {
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
            Self::SubmoduleUpdate { path: Some(p) } => format!("Update Submodule '{p}'"),
            Self::SubmoduleUpdate { path: None } => "Update Submodules".to_string(),
            Self::Unshallow { remote } => format!("Unshallow from '{remote}'"),
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
            | Self::UpdateProject { .. }
            | Self::SubmoduleUpdate { .. }
            | Self::Unshallow { .. } => true,
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
            | Self::SetUpstream { .. }
            | Self::UpdateBranch { .. } => RefreshScope::RefsAndGraph,
            Self::Sequence(ops) => ops
                .iter()
                .fold(RefreshScope::None, |acc, op| acc.merge(op.refresh_scope())),
            _ => RefreshScope::All,
        }
    }
}

/// Infers a default local directory name from a Git clone URL or local path
/// (HTTPS, `ssh://`, SCP-style `git@host:user/repo.git`, or local path).
/// Strips query/fragment suffixes, trailing slashes, and `.git` suffix.
pub fn infer_clone_dir_name(url: &str) -> Option<String> {
    let mut s = url.trim();
    if s.is_empty() {
        return None;
    }
    if let Some((before_frag, _)) = s.split_once('#') {
        s = before_frag;
    }
    if let Some((before_query, _)) = s.split_once('?') {
        s = before_query;
    }
    s = s.trim_end_matches(['/', '\\']);
    if s.is_empty() {
        return None;
    }

    let path_part = if let Some((_, rest)) = s.split_once("://") {
        // Must have a path segment after the authority (`host[:port]/path`).
        let (_, path) = rest.split_once('/')?;
        path.trim_end_matches('/')
    } else {
        // Distinguish Windows drive letter (`C:\...` / `C:/...`) from SCP-style `git@host:user/repo`.
        let is_windows_drive = s.len() >= 2
            && s.as_bytes()[0].is_ascii_alphabetic()
            && s.as_bytes()[1] == b':';
        if !is_windows_drive
            && let Some((host, path)) = s.split_once(':')
            && !host.is_empty()
            && !host.contains(['/', '\\'])
        {
            path.trim_end_matches(['/', '\\'])
        } else {
            s
        }
    };

    if path_part.is_empty() {
        return None;
    }
    let last = path_part
        .rsplit(['/', '\\'])
        .find(|seg| !seg.is_empty())?;
    let stripped = if last.len() >= 4 && last[last.len() - 4..].eq_ignore_ascii_case(".git") {
        &last[..last.len() - 4]
    } else {
        last
    };
    if stripped.is_empty()
        || stripped == "."
        || stripped == ".."
        || stripped.contains([':', '\0'])
    {
        return None;
    }
    Some(stripped.to_string())
}

#[cfg(test)]
mod clone_url_tests {
    use super::infer_clone_dir_name;

    #[test]
    fn infers_directory_name_from_https_ssh_scp_and_local_urls() {
        assert_eq!(
            infer_clone_dir_name("https://github.com/org/jbgit.git").as_deref(),
            Some("jbgit")
        );
        assert_eq!(
            infer_clone_dir_name("https://github.com/org/jbgit///").as_deref(),
            Some("jbgit")
        );
        assert_eq!(
            infer_clone_dir_name("https://github.com/org/jbgit.git?ref=main#readme").as_deref(),
            Some("jbgit")
        );
        assert_eq!(
            infer_clone_dir_name("git@github.com:org/my-repo.git").as_deref(),
            Some("my-repo")
        );
        assert_eq!(
            infer_clone_dir_name("ssh://git@example.com:2222/team/service.git/").as_deref(),
            Some("service")
        );
        assert_eq!(
            infer_clone_dir_name("file:///tmp/repos/bare-repo.git").as_deref(),
            Some("bare-repo")
        );
        assert_eq!(
            infer_clone_dir_name(r"C:\work\repos\demo.git\").as_deref(),
            Some("demo")
        );
        assert_eq!(
            infer_clone_dir_name("/var/git/project.GIT").as_deref(),
            Some("project")
        );
        assert_eq!(infer_clone_dir_name(""), None);
        assert_eq!(infer_clone_dir_name("   "), None);
        assert_eq!(infer_clone_dir_name("https://github.com/"), None);
        assert_eq!(infer_clone_dir_name("https://github.com/.git"), None);
    }
}
