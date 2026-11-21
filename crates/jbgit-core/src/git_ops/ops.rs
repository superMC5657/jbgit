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
        args: Vec<String>,
    },
    CheckoutPaths {
        rev: String,
        paths: Vec<String>,
    },
    Smart(Box<GitOp>),
}

/// One `git` invocation of an operation.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GitStep {
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
}

impl GitStep {
    pub fn new<I, S>(args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            args: args.into_iter().map(Into::into).collect(),
            env: Vec::new(),
        }
    }

    pub fn with_env(mut self, key: &str, value: &str) -> Self {
        self.env.push((key.to_string(), value.to_string()));
        self
    }
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

    pub fn simple_steps(&self) -> Option<Vec<GitStep>> {
        let no_edit = |step: GitStep| step.with_env("GIT_EDITOR", ":");
        let steps = match self {
            Self::Checkout { target, detach } => {
                let mut args = vec!["checkout".to_string()];
                if *detach {
                    args.push("--detach".to_string());
                }
                args.push(target.clone());
                vec![GitStep::new(args)]
            }
            Self::ForceCheckout { target } => {
                vec![GitStep::new(["checkout", "-f", target.as_str()])]
            }
            Self::CheckoutNewBranch { name, start, track } => {
                let mut args = vec!["checkout".to_string(), "-b".to_string(), name.clone()];
                if *track {
                    args.push("--track".to_string());
                }
                args.push(start.clone());
                vec![GitStep::new(args)]
            }
            Self::CheckoutResetBranch { name, start } => vec![GitStep::new([
                "checkout",
                "-B",
                name.as_str(),
                "--track",
                start.as_str(),
            ])],
            Self::CreateBranch { name, start } => {
                vec![GitStep::new([
                    "branch",
                    "--",
                    name.as_str(),
                    start.as_str(),
                ])]
            }
            Self::RenameBranch { old, new } => {
                vec![GitStep::new(["branch", "-m", old.as_str(), new.as_str()])]
            }
            Self::DeleteBranch { name, force, .. } => vec![GitStep::new([
                "branch",
                if *force { "-D" } else { "-d" },
                name.as_str(),
            ])],
            Self::RestoreBranch { name, oid } => {
                vec![GitStep::new(vec![
                    "branch".to_string(),
                    name.clone(),
                    oid.to_string(),
                ])]
            }
            Self::DeleteRemoteBranch { remote, branch } => vec![GitStep::new([
                "push",
                remote.as_str(),
                "--delete",
                branch.as_str(),
            ])],
            Self::CreateTag {
                name,
                target,
                message,
            } => {
                let mut args = vec!["tag".to_string()];
                if let Some(m) = message.as_ref().filter(|m| !m.trim().is_empty()) {
                    args.extend(["-a".to_string(), "-m".to_string(), m.clone()]);
                }
                args.extend([name.clone(), target.to_string()]);
                vec![GitStep::new(args)]
            }
            Self::Merge { rev } => {
                vec![no_edit(GitStep::new(["merge", "--no-edit", rev.as_str()]))]
            }
            Self::Rebase { onto, branch } => {
                let mut args = vec!["rebase".to_string(), onto.clone()];
                if let Some(b) = branch {
                    args.push(b.clone());
                }
                vec![no_edit(GitStep::new(args))]
            }
            Self::CherryPick { oids } => {
                let mut args = vec!["cherry-pick".to_string()];
                args.extend(oids.iter().map(ToString::to_string));
                vec![no_edit(GitStep::new(args))]
            }
            Self::Revert { oids } => {
                let mut args = vec!["revert".to_string(), "--no-edit".to_string()];
                args.extend(oids.iter().map(ToString::to_string));
                vec![no_edit(GitStep::new(args))]
            }
            Self::Reset { mode, target } => vec![GitStep::new(vec![
                "reset".to_string(),
                mode.flag().to_string(),
                target.to_string(),
            ])],
            Self::Fetch => vec![GitStep::new(["fetch", "--all", "--prune"])],
            Self::Pull { rebase } => vec![no_edit(GitStep::new(if *rebase {
                vec!["pull", "--rebase"]
            } else {
                vec!["pull", "--no-edit"]
            }))],
            Self::Push {
                branch,
                remote,
                set_upstream,
                force_with_lease,
                up_to,
            } => {
                let mut args = vec!["push".to_string()];
                if *set_upstream {
                    args.push("--set-upstream".to_string());
                }
                if *force_with_lease {
                    args.push("--force-with-lease".to_string());
                }
                args.push(remote.clone());
                match up_to {
                    Some(oid) => args.push(format!("{oid}:refs/heads/{branch}")),
                    None => args.push(format!("refs/heads/{branch}:refs/heads/{branch}")),
                }
                vec![GitStep::new(args)]
            }
            Self::UpdateBranch {
                branch,
                remote,
                remote_branch,
            } => vec![GitStep::new(vec![
                "fetch".to_string(),
                remote.clone(),
                format!("refs/heads/{remote_branch}:refs/heads/{branch}"),
            ])],
            Self::FixupCommit {
                target,
                base,
                autosquash,
            } => {
                let mut steps = vec![GitStep::new(vec![
                    "commit".to_string(),
                    "--no-verify".to_string(),
                    format!("--fixup={target}"),
                ])];
                if *autosquash {
                    let onto = base.map_or_else(|| "--root".to_string(), |b| b.to_string());
                    steps.push(
                        GitStep::new(vec![
                            "rebase".to_string(),
                            "-i".to_string(),
                            "--autosquash".to_string(),
                            "--autostash".to_string(),
                            onto,
                        ])
                        .with_env("GIT_SEQUENCE_EDITOR", ":")
                        .with_env("GIT_EDITOR", ":"),
                    );
                }
                steps
            }
            Self::Ongoing { kind, step } => {
                vec![no_edit(GitStep::new([kind.command(), step.flag()]))]
            }
            Self::StashPush {
                message,
                include_untracked,
                paths,
            } => {
                let mut args = vec!["stash".to_string(), "push".to_string()];
                if *include_untracked {
                    args.push("--include-untracked".to_string());
                }
                if let Some(m) = message.as_ref().filter(|m| !m.trim().is_empty()) {
                    args.extend(["-m".to_string(), m.clone()]);
                }
                if !paths.is_empty() {
                    args.push("--".to_string());
                    args.extend(paths.iter().cloned());
                }
                vec![GitStep::new(args)]
            }
            Self::StashApply { index, pop } => vec![GitStep::new(vec![
                "stash".to_string(),
                if *pop { "pop" } else { "apply" }.to_string(),
                format!("stash@{{{index}}}"),
            ])],
            Self::StashDrop { index } => vec![GitStep::new(vec![
                "stash".to_string(),
                "drop".to_string(),
                format!("stash@{{{index}}}"),
            ])],
            Self::StashClear => vec![GitStep::new(["stash", "clear"])],
            Self::StashBranch { index, name } => vec![GitStep::new(vec![
                "stash".to_string(),
                "branch".to_string(),
                name.clone(),
                format!("stash@{{{index}}}"),
            ])],
            Self::AcceptSide { paths, ours } => {
                let mut co = vec![
                    "checkout".to_string(),
                    if *ours { "--ours" } else { "--theirs" }.to_string(),
                    "--".to_string(),
                ];
                co.extend(paths.iter().cloned());
                let mut add = vec!["add".to_string(), "--".to_string()];
                add.extend(paths.iter().cloned());
                vec![GitStep::new(co), GitStep::new(add)]
            }
            Self::IntentToAdd { paths } => {
                let mut args = vec!["add".to_string(), "-N".to_string(), "--".to_string()];
                args.extend(paths.iter().cloned());
                vec![GitStep::new(args)]
            }
            Self::CreatePatchFiles { oids, base } => {
                let mut args = vec!["format-patch".to_string(), "-o".to_string(), ".".to_string()];
                match oids.as_slice() {
                    [one] => {
                        args.push("-1".to_string());
                        args.push(one.to_string());
                    }
                    [newest, ..] => match base {
                        Some(b) => args.push(format!("{b}..{newest}")),
                        None => {
                            args.push("--root".to_string());
                            args.push(newest.to_string());
                        }
                    },
                    [] => return Some(Vec::new()),
                }
                vec![GitStep::new(args)]
            }
            Self::PatchToClipboard { args } => vec![GitStep::new(args.clone())],
            Self::CheckoutPaths { rev, paths } => {
                if paths.is_empty() {
                    Vec::new()
                } else if paths.len() <= 32 {
                    let mut args = vec!["checkout".to_string(), rev.clone(), "--".to_string()];
                    args.extend(paths.iter().cloned());
                    vec![GitStep::new(args)]
                } else {
                    return None;
                }
            }
            Self::RewordHead { .. }
            | Self::RewriteHistory { .. }
            | Self::AddToIgnore { .. }
            | Self::DeleteFiles { .. }
            | Self::Smart(_) => return None,
        };
        Some(steps)
    }

    pub fn is_read_only(&self) -> bool {
        matches!(self, Self::PatchToClipboard { .. })
    }
}
