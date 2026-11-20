//! Pure domain model of the high-level Git write operations driven by the
//! branches popup, context menus and dialogs (Spec 05): operation catalogue,
//! CLI step builders, failure classification, ref-name validation, reflog
//! `Recent` branch parsing and interactive-rebase todo rendering.

use crate::{Oid, RepositoryState};

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

/// Interactive-rebase todo verb (`pick / reword / squash / fixup / drop`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TodoAction {
    Pick,
    Reword,
    Squash,
    Fixup,
    Drop,
}

impl TodoAction {
    pub const ALL: [Self; 5] = [
        Self::Pick,
        Self::Reword,
        Self::Squash,
        Self::Fixup,
        Self::Drop,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Pick => "pick",
            Self::Reword => "reword",
            Self::Squash => "squash",
            Self::Fixup => "fixup",
            Self::Drop => "drop",
        }
    }

    pub fn from_key(c: char) -> Option<Self> {
        match c.to_ascii_lowercase() {
            'p' => Some(Self::Pick),
            'r' | 'w' => Some(Self::Reword),
            's' => Some(Self::Squash),
            'f' => Some(Self::Fixup),
            'd' => Some(Self::Drop),
            _ => None,
        }
    }

    pub const fn next(self) -> Self {
        match self {
            Self::Pick => Self::Reword,
            Self::Reword => Self::Squash,
            Self::Squash => Self::Fixup,
            Self::Fixup => Self::Drop,
            Self::Drop => Self::Pick,
        }
    }
}

/// `base..HEAD` rewrite plan starting at a commit (oldest first).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RewritePlan {
    pub base: Option<Oid>,
    pub entries: Vec<TodoEntry>,
    pub messages: Vec<String>,
}

/// One line of a rebase plan, oldest commit first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TodoEntry {
    pub action: TodoAction,
    pub oid: Oid,
    pub subject: String,
    /// Replacement message for `Reword`, or the combined message applied after
    /// the last `Squash` / `Fixup` of a group. `None` keeps Git's message.
    pub message: Option<String>,
}

impl TodoEntry {
    pub fn pick(oid: Oid, subject: impl Into<String>) -> Self {
        Self {
            action: TodoAction::Pick,
            oid,
            subject: subject.into(),
            message: None,
        }
    }
}

/// Renders an editor-free rebase todo: only `pick` / `fixup` / `drop` / `exec`
/// lines are emitted, rewording via `exec git commit --amend --only -F <file>`
/// so Git never needs to open `GIT_EDITOR`. `message_file(i)` returns the
/// shell-quoted path of the message file written for entry `i`.
///
/// `Squash` folds the commit into its predecessor and, unless the entry
/// carries an explicit message, keeps both messages concatenated (like Git's
/// default squash message) via a prepared message file.
pub fn render_todo(entries: &[TodoEntry], mut message_file: impl FnMut(usize) -> String) -> String {
    let mut out = String::new();
    for (idx, e) in entries.iter().enumerate() {
        let verb = match e.action {
            TodoAction::Pick | TodoAction::Reword => "pick",
            TodoAction::Squash | TodoAction::Fixup => "fixup",
            TodoAction::Drop => "drop",
        };
        out.push_str(&format!(
            "{verb} {} {}\n",
            e.oid,
            sanitize_subject(&e.subject)
        ));
        let next_folds = entries
            .get(idx + 1)
            .is_some_and(|n| matches!(n.action, TodoAction::Squash | TodoAction::Fixup));
        let needs_amend = match e.action {
            TodoAction::Reword => e.message.is_some(),
            TodoAction::Squash | TodoAction::Fixup => e.message.is_some() && !next_folds,
            _ => false,
        };
        if needs_amend {
            out.push_str(&format!(
                "exec git commit --amend --only --allow-empty --no-verify -F {}\n",
                message_file(idx)
            ));
        }
    }
    out
}

/// Computes the message each `Squash` group should end with when the user did
/// not supply one: subjects/bodies of all squashed commits joined by blank
/// lines (`fixup` contributes nothing, as in Git).
pub fn default_squash_messages(entries: &mut [TodoEntry], full_message: impl Fn(&Oid) -> String) {
    let mut group_start: Option<usize> = None;
    let mut acc: Vec<String> = Vec::new();
    let len = entries.len();
    for idx in 0..len {
        let action = entries[idx].action;
        match action {
            TodoAction::Pick | TodoAction::Reword => {
                group_start = Some(idx);
                acc = vec![
                    entries[idx]
                        .message
                        .clone()
                        .unwrap_or_else(|| full_message(&entries[idx].oid)),
                ];
            }
            TodoAction::Squash => {
                if group_start.is_some() {
                    acc.push(full_message(&entries[idx].oid));
                }
            }
            TodoAction::Fixup | TodoAction::Drop => {}
        }
        let next_folds = entries
            .get(idx + 1)
            .is_some_and(|n| matches!(n.action, TodoAction::Squash | TodoAction::Fixup));
        if matches!(action, TodoAction::Squash | TodoAction::Fixup)
            && !next_folds
            && entries[idx].message.is_none()
            && group_start.is_some()
        {
            let has_squash = entries[group_start.unwrap_or(0)..=idx]
                .iter()
                .any(|e| e.action == TodoAction::Squash);
            if has_squash {
                entries[idx].message = Some(acc.join("\n\n"));
            }
        }
    }
}

fn sanitize_subject(s: &str) -> String {
    s.replace(['\n', '\r'], " ")
}

/// High-level write operation dispatched by menus / dialogs and executed by the
/// engine as one or more `git` CLI steps (streamed to the Console tab).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitOp {
    Checkout {
        target: String,
        detach: bool,
    },
    /// `git checkout -f <target>` (Smart Operation dialog: discard local).
    ForceCheckout {
        target: String,
    },
    /// `git checkout -b <name> <start>` (`--track` for remote starts).
    CheckoutNewBranch {
        name: String,
        start: String,
        track: bool,
    },
    /// `git checkout -B <name> <start> --track`: reset the existing local
    /// branch to the remote one (`GitRemoteBranchCheckoutUtil` overwrite).
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
    /// `git rebase <onto> [<branch>]`.
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
        /// `Push All up to Here`: push only up to this commit.
        up_to: Option<Oid>,
    },
    /// Fast-forward a non-current local branch from its upstream.
    UpdateBranch {
        branch: String,
        remote: String,
        remote_branch: String,
    },
    /// `git commit --amend --only` with a new message (Reword of `HEAD`).
    RewordHead {
        message: String,
    },
    /// Editor-free interactive rebase of `base..HEAD` (`None` = `--root`).
    RewriteHistory {
        base: Option<Oid>,
        todo: Vec<TodoEntry>,
    },
    /// `git commit --fixup=<target>` from the staged changes, optionally
    /// folded right away with `rebase -i --autosquash` from `base`
    /// (`None` when `target` is a root commit, using `--root`).
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
    /// `git add -N` (Stage Without Content).
    IntentToAdd {
        paths: Vec<String>,
    },
    /// Append patterns to `.gitignore` (or `.git/info/exclude`).
    AddToIgnore {
        patterns: Vec<String>,
        exclude_file: bool,
    },
    /// Delete working-tree files (`Delete File…`).
    DeleteFiles {
        paths: Vec<String>,
    },
    /// `git format-patch` into the repository root (`oids` newest first;
    /// `base` is the first parent of the oldest commit, or `None` for root).
    CreatePatchFiles {
        oids: Vec<Oid>,
        base: Option<Oid>,
    },
    /// Unified patch of commits / paths captured to the clipboard.
    PatchToClipboard {
        args: Vec<String>,
    },
    /// `git checkout <rev> -- <paths...>` (Vcs.GetVersion: restore files from revision).
    CheckoutPaths {
        rev: String,
        paths: Vec<String>,
    },
    /// Stash local changes (incl. untracked) → run `inner` → pop them back.
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

    /// Operations that Git may refuse because of local changes and that the
    /// Smart Operation dialog can retry with stash → op → pop.
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

    /// Steps that only need plain `git` invocations. Operations needing temp
    /// files (`RewriteHistory`), filesystem edits or composition (`Smart`)
    /// return `None` and are orchestrated by the engine.
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

    /// Read-only operations (never refresh the graph).
    pub fn is_read_only(&self) -> bool {
        matches!(self, Self::PatchToClipboard { .. })
    }
}

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
    /// stdout of the last step (patch text for `PatchToClipboard`).
    pub stdout: Vec<String>,
    /// Combined stderr of every step.
    pub stderr: Vec<String>,
    /// Smart operations: local changes stayed in the stash (pop conflicted
    /// or the operation stopped on conflicts).
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
    /// `On main: message` / `WIP on main: abc123 subject`.
    pub message: String,
    pub timestamp: i64,
}

impl StashEntry {
    /// Branch the stash was created on (`On <branch>:` / `WIP on <branch>:`).
    pub fn branch(&self) -> Option<&str> {
        let rest = self
            .message
            .strip_prefix("WIP on ")
            .or_else(|| self.message.strip_prefix("On "))?;
        rest.split_once(':').map(|(b, _)| b)
    }

    /// Message without the `On <branch>: ` prefix.
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

/// Parses `.git/logs/HEAD` (newest entries last) and returns up to `limit`
/// unique local branches the user checked out recently (newest first),
/// excluding `current` and branches no longer in `existing`. Only the last
/// `scan` reflog entries are considered (`git.recent.checkout.branches.reflog.entries.count`).
pub fn parse_recent_checkouts(
    reflog: &str,
    current: Option<&str>,
    existing: &[&str],
    scan: usize,
    limit: usize,
) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in reflog.lines().rev().take(scan) {
        let Some((_, msg)) = line.split_once('\t') else {
            continue;
        };
        let Some(rest) = msg.strip_prefix("checkout: moving from ") else {
            continue;
        };
        let Some((from, to)) = rest.rsplit_once(" to ") else {
            continue;
        };
        for name in [to.trim(), from.trim()] {
            if name.is_empty()
                || Some(name) == current
                || out.iter().any(|n| n == name)
                || !existing.contains(&name)
            {
                continue;
            }
            out.push(name.to_string());
        }
        if out.len() >= limit {
            break;
        }
    }
    out.truncate(limit);
    out
}

/// Typed reason a branch or tag name fails `git check-ref-format`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefNameError {
    Empty,
    StartsWithDash,
    ContainsSpaces,
    ContainsDoubleDot,
    ContainsAtBrace,
    InvalidChar(char),
    EmptyPathComponent,
    EndsWithDot,
    ComponentStartsWithDot(String),
    ComponentEndsWithLock(String),
    ReservedHead,
    AlreadyExists(String),
    ConflictsWithExisting(String),
}

impl RefNameError {
    pub fn format_en(&self, what: &str) -> String {
        match self {
            Self::Empty => format!("{what} name cannot be empty"),
            Self::StartsWithDash => format!("{what} name cannot start with '-'"),
            Self::ContainsSpaces => format!("{what} name cannot contain spaces"),
            Self::ContainsDoubleDot => format!("{what} name cannot contain '..'"),
            Self::ContainsAtBrace => format!("{what} name cannot contain '@{{' or be '@'"),
            Self::InvalidChar(c) => format!("{what} name cannot contain '{c}'"),
            Self::EmptyPathComponent => format!("{what} name has an empty path component"),
            Self::EndsWithDot => format!("{what} name cannot end with '.'"),
            Self::ComponentStartsWithDot(comp) => {
                format!("path component cannot start with '.' ('{comp}')")
            }
            Self::ComponentEndsWithLock(comp) => {
                format!("path component cannot end with '.lock' ('{comp}')")
            }
            Self::ReservedHead => format!("'HEAD' is not a valid {} name", what.to_lowercase()),
            Self::AlreadyExists(name) => format!("{what} '{name}' already exists"),
            Self::ConflictsWithExisting(conflict) => {
                format!("{what} name conflicts with existing '{conflict}'")
            }
        }
    }
}

/// Typed `git check-ref-format --branch` validation plus duplicate detection.
pub fn validate_ref_name_typed(name: &str, existing: &[&str]) -> Result<(), RefNameError> {
    if name.is_empty() {
        return Err(RefNameError::Empty);
    }
    if name.starts_with('-') {
        return Err(RefNameError::StartsWithDash);
    }
    if name.chars().any(|c| c.is_whitespace()) {
        return Err(RefNameError::ContainsSpaces);
    }
    if name.contains("..") {
        return Err(RefNameError::ContainsDoubleDot);
    }
    if name.contains("@{") || name == "@" {
        return Err(RefNameError::ContainsAtBrace);
    }
    if let Some(c) = name
        .chars()
        .find(|c| c.is_control() || matches!(c, '~' | '^' | ':' | '?' | '*' | '[' | '\\'))
    {
        return Err(RefNameError::InvalidChar(c));
    }
    if name.starts_with('/') || name.ends_with('/') || name.contains("//") {
        return Err(RefNameError::EmptyPathComponent);
    }
    if name.ends_with('.') {
        return Err(RefNameError::EndsWithDot);
    }
    for comp in name.split('/') {
        if comp.starts_with('.') {
            return Err(RefNameError::ComponentStartsWithDot(comp.to_string()));
        }
        if comp.ends_with(".lock") {
            return Err(RefNameError::ComponentEndsWithLock(comp.to_string()));
        }
    }
    if name == "HEAD" {
        return Err(RefNameError::ReservedHead);
    }
    if existing.contains(&name) {
        return Err(RefNameError::AlreadyExists(name.to_string()));
    }
    if let Some(conflict) = existing.iter().find(|e| {
        e.strip_prefix(name).is_some_and(|r| r.starts_with('/'))
            || name.strip_prefix(**e).is_some_and(|r| r.starts_with('/'))
    }) {
        return Err(RefNameError::ConflictsWithExisting((*conflict).to_string()));
    }
    Ok(())
}

/// `git check-ref-format --branch` rules plus duplicate detection.
pub fn validate_ref_name(name: &str, existing: &[&str], what: &str) -> Result<(), String> {
    validate_ref_name_typed(name, existing).map_err(|err| err.format_en(what))
}

/// Fuzzy (`nucleo`) match returning the score and the matched **char**
/// indices of `haystack` for highlighting. `None` when it does not match.
pub fn fuzzy_indices(haystack: &str, needle: &str) -> Option<(u32, Vec<u32>)> {
    let needle = needle.trim();
    if needle.is_empty() {
        return Some((0, Vec::new()));
    }
    let mut matcher = nucleo_matcher::Matcher::new(nucleo_matcher::Config::DEFAULT.match_paths());
    let pattern = nucleo_matcher::pattern::Pattern::parse(
        needle,
        nucleo_matcher::pattern::CaseMatching::Smart,
        nucleo_matcher::pattern::Normalization::Smart,
    );
    let mut buf = Vec::new();
    let mut indices = Vec::new();
    let score = pattern.indices(
        nucleo_matcher::Utf32Str::new(haystack, &mut buf),
        &mut matcher,
        &mut indices,
    )?;
    indices.sort_unstable();
    indices.dedup();
    Some((score, indices))
}

/// Branches considered "protected" (merge suggestions never offer deleting them).
pub fn is_protected_branch(name: &str) -> bool {
    matches!(
        name,
        "main" | "master" | "develop" | "development" | "trunk" | "release" | "production"
    ) || name.starts_with("release/")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn oid(n: u8) -> Oid {
        Oid::from_bytes([n; 20])
    }

    #[test]
    fn recent_checkouts_from_reflog_newest_first_unique_and_existing() {
        let reflog = "\
0 1 A <a> 1 +0000\tcommit (initial): init
1 2 A <a> 2 +0000\tcheckout: moving from main to feature/a
2 3 A <a> 3 +0000\tcheckout: moving from feature/a to bugfix/b
3 4 A <a> 4 +0000\tcommit: x
4 5 A <a> 5 +0000\tcheckout: moving from bugfix/b to gone
5 6 A <a> 6 +0000\tcheckout: moving from gone to main
";
        let existing = ["main", "feature/a", "bugfix/b"];
        let recent = parse_recent_checkouts(reflog, Some("main"), &existing, 50, 5);
        assert_eq!(
            recent,
            vec!["bugfix/b".to_string(), "feature/a".to_string()]
        );
        let one = parse_recent_checkouts(reflog, Some("main"), &existing, 50, 1);
        assert_eq!(one, vec!["bugfix/b".to_string()]);
    }

    #[test]
    fn ref_name_validation_matches_check_ref_format() {
        let existing = ["main", "feature/x"];
        assert!(validate_ref_name("feature/y", &existing, "Branch").is_ok());
        for bad in [
            "", "-x", "a b", "a..b", "a~1", "a^", "a:b", "a?", "a*", "a[", "a\\b", "/a", "a/",
            "a//b", "a.", ".a", "a/.b", "a.lock", "a@{1}", "@", "HEAD", "main", "feature",
            "main/sub",
        ] {
            assert!(
                validate_ref_name(bad, &existing, "Branch").is_err(),
                "{bad:?} should be rejected"
            );
        }
    }

    #[test]
    fn classify_local_changes_conflicts_and_unmerged() {
        let lines: Vec<String> = [
            "error: Your local changes to the following files would be overwritten by checkout:",
            "\tsrc/a.rs",
            "\tsrc/b.rs",
            "Please commit your changes or stash them before you switch branches.",
            "Aborting",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert_eq!(
            classify_failure(&lines),
            GitFailure::LocalChangesOverwritten {
                files: vec!["src/a.rs".into(), "src/b.rs".into()]
            }
        );
        assert_eq!(
            classify_failure(&["CONFLICT (content): Merge conflict in a.txt".to_string()]),
            GitFailure::Conflicts
        );
        assert_eq!(
            classify_failure(&["error: The branch 'x' is not fully merged.".to_string()]),
            GitFailure::NotFullyMerged
        );
        assert_eq!(
            classify_failure(&["fatal: bad revision".to_string()]),
            GitFailure::Other("fatal: bad revision".into())
        );
    }

    #[test]
    fn todo_is_editor_free_and_rewords_via_exec() {
        let mut todo = vec![
            TodoEntry::pick(oid(1), "one"),
            TodoEntry {
                action: TodoAction::Reword,
                oid: oid(2),
                subject: "two".into(),
                message: Some("two v2".into()),
            },
            TodoEntry {
                action: TodoAction::Drop,
                oid: oid(3),
                subject: "three".into(),
                message: None,
            },
            TodoEntry::pick(oid(4), "four"),
            TodoEntry {
                action: TodoAction::Squash,
                oid: oid(5),
                subject: "five".into(),
                message: None,
            },
        ];
        default_squash_messages(&mut todo, |o| format!("msg {}", o.short()));
        assert_eq!(
            todo[4].message.as_deref(),
            Some(format!("msg {}\n\nmsg {}", oid(4).short(), oid(5).short()).as_str())
        );
        let text = render_todo(&todo, |i| format!("'/tmp/m{i}'"));
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 7);
        assert!(lines[0].starts_with("pick 0101"));
        assert!(lines[1].starts_with("pick 0202"));
        assert_eq!(
            lines[2],
            "exec git commit --amend --only --allow-empty --no-verify -F '/tmp/m1'"
        );
        assert!(lines[3].starts_with("drop 0303"));
        assert!(lines[5].starts_with("fixup 0505"));
        assert!(lines[6].ends_with("'/tmp/m4'"));
        assert!(!text.contains("reword") && !text.contains("squash"));
    }

    #[test]
    fn fuzzy_indices_highlight_path_segments() {
        let (_, idx) = fuzzy_indices("feature/dag-layout", "f/dl").unwrap();
        let chars: Vec<char> = "feature/dag-layout".chars().collect();
        let picked: String = idx.iter().map(|&i| chars[i as usize]).collect();
        assert_eq!(picked, "f/dl");
        assert!(fuzzy_indices("main", "xyz").is_none());
    }

    #[test]
    fn steps_for_common_ops() {
        let push = GitOp::Push {
            branch: "feat".into(),
            remote: "origin".into(),
            set_upstream: true,
            force_with_lease: false,
            up_to: Some(oid(7)),
        };
        assert_eq!(
            push.simple_steps().unwrap()[0].args,
            vec![
                "push".to_string(),
                "--set-upstream".into(),
                "origin".into(),
                format!("{}:refs/heads/feat", oid(7)),
            ]
        );
        let remote_co = GitOp::CheckoutNewBranch {
            name: "feature-new".into(),
            start: "origin/feature-new".into(),
            track: true,
        };
        assert_eq!(
            remote_co.simple_steps().unwrap()[0].args,
            vec![
                "checkout",
                "-b",
                "feature-new",
                "--track",
                "origin/feature-new"
            ]
        );
        assert!(GitOp::Smart(Box::new(remote_co)).simple_steps().is_none());
        let stash = StashEntry {
            index: 0,
            oid: oid(1),
            message: "On main: wip parser".into(),
            timestamp: 0,
        };
        assert_eq!(stash.branch(), Some("main"));
        assert_eq!(stash.short_message(), "wip parser");

        let root_fixup = GitOp::FixupCommit {
            target: oid(1),
            base: None,
            autosquash: true,
        };
        let steps = root_fixup.simple_steps().unwrap();
        assert_eq!(steps[1].args.last().map(String::as_str), Some("--root"));

        let root_patch = GitOp::CreatePatchFiles {
            oids: vec![oid(2), oid(1)],
            base: None,
        };
        assert_eq!(
            root_patch.simple_steps().unwrap()[0].args,
            vec!["format-patch", "-o", ".", "--root", &oid(2).to_string()]
        );

        let restore = GitOp::CheckoutPaths {
            rev: oid(3).to_string(),
            paths: vec!["src/main.rs".into(), "dir with space/a.txt".into()],
        };
        assert!(!restore.is_read_only());
        assert_eq!(
            restore.simple_steps().unwrap()[0].args,
            vec![
                "checkout",
                &oid(3).to_string(),
                "--",
                "src/main.rs",
                "dir with space/a.txt"
            ]
        );
        let many_paths: Vec<String> = (0..35).map(|i| format!("f{i}.txt")).collect();
        let restore_many = GitOp::CheckoutPaths {
            rev: "HEAD~1".into(),
            paths: many_paths,
        };
        assert!(restore_many.simple_steps().is_none());
    }
}
