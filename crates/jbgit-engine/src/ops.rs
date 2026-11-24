//! Unified [`GitOp`] CLI compiler and executor.
//!
//! Translates pure declarative [`jbgit_core::GitOp`] domain intents into one or
//! more `git` CLI invocations (including `--pathspec-from-file` for large path
//! sets, **Smart Operation Saver** stash/restore wrapping, and editor-free
//! interactive rebase) and runs them through the [`GitCliRunner`] seam.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use jbgit_core::{
    FixupKind, GitFailure, GitOp, GitOpOutcome, MergeOptions, Oid, OngoingKind, OngoingStep,
    PatchSource, PullOptions, RebaseOptions, SkippedBranch, UpdateInfo, UpdateMode, UpdatedRange,
    WorktreeBranch, classify_failure, is_reverse_conflict_state, render_todo,
};

use crate::askpass::AskpassEnv;
use crate::cli::{
    CliCommand, CliStreamEvent, GitCliRunner, IndexLockConflict, prepare_pathspec_args,
};
use crate::probe::RepoLocation;

/// Console ids for operation steps live far above UI-allocated ids.
pub const OP_CONSOLE_ID_BASE: u64 = 1 << 40;

/// Upper bound of commits listed in an [`UpdateInfo`] (Log range filter payload).
pub(crate) const UPDATE_INFO_COMMIT_CAP: usize = 10_000;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct CompiledStep {
    pub args: Vec<String>,
    pub stdin: Option<Vec<u8>>,
    pub env: Vec<(String, String)>,
}

impl CompiledStep {
    fn new<I, S>(args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            args: args.into_iter().map(Into::into).collect(),
            stdin: None,
            env: Vec::new(),
        }
    }

    fn with_pathspecs(base_args: Vec<String>, paths: &[String]) -> Self {
        let (args, stdin) = prepare_pathspec_args(base_args, paths);
        Self {
            args,
            stdin,
            env: Vec::new(),
        }
    }

    fn with_stdin(mut self, stdin: Vec<u8>) -> Self {
        self.stdin = Some(stdin);
        self
    }

    fn with_env(mut self, key: &str, value: &str) -> Self {
        self.env.push((key.to_string(), value.to_string()));
        self
    }
}

/// Outcome of running a [`GitOp`] plus any detected `index.lock` conflict for retry UI.
pub(crate) struct GitOpRunResult {
    pub outcome: GitOpOutcome,
    pub index_lock: Option<IndexLockConflict>,
}

struct StepResult {
    exit_code: i32,
    stdout: Vec<String>,
    stderr: Vec<String>,
    index_lock: Option<IndexLockConflict>,
}

impl StepResult {
    fn ok(&self) -> bool {
        self.exit_code == 0
    }

    fn all_lines(&self) -> Vec<String> {
        let mut v = self.stderr.clone();
        v.extend(self.stdout.iter().cloned());
        v
    }
}

/// Compiles declarative [`GitOp`] variants into concrete `git` CLI steps.
/// Returns `None` for stateful/filesystem operations handled directly by [`Runner`].
pub(crate) fn compile_simple_steps(op: &GitOp) -> Option<Vec<CompiledStep>> {
    let no_edit = |step: CompiledStep| step.with_env("GIT_EDITOR", ":");
    let steps = match op {
        GitOp::Checkout { target, detach } => {
            let mut args = vec!["checkout".to_string()];
            if *detach {
                args.push("--detach".to_string());
            }
            args.push(target.clone());
            vec![CompiledStep::new(args)]
        }
        GitOp::ForceCheckout { target } => {
            vec![CompiledStep::new(["checkout", "-f", target.as_str()])]
        }
        GitOp::CheckoutNewBranch {
            name,
            start,
            track,
            force,
        } => {
            let mut args = vec![
                "checkout".to_string(),
                if *force { "-B" } else { "-b" }.to_string(),
                name.clone(),
            ];
            if *track {
                args.push("--track".to_string());
            }
            args.push(start.clone());
            vec![CompiledStep::new(args)]
        }
        GitOp::CheckoutResetBranch { name, start } => vec![CompiledStep::new([
            "checkout",
            "-B",
            name.as_str(),
            "--track",
            start.as_str(),
        ])],
        GitOp::CreateBranch {
            name,
            start,
            force,
            track,
        } => {
            let mut args = vec!["branch".to_string()];
            if *force {
                args.push("-f".to_string());
            }
            if *track {
                args.push("--track".to_string());
            }
            args.extend(["--".to_string(), name.clone(), start.clone()]);
            vec![CompiledStep::new(args)]
        }
        GitOp::RenameBranch { old, new } => {
            vec![CompiledStep::new([
                "branch",
                "-m",
                "--",
                old.as_str(),
                new.as_str(),
            ])]
        }
        GitOp::DeleteBranch { name, force, .. } => vec![CompiledStep::new([
            "branch",
            if *force { "-D" } else { "-d" },
            "--",
            name.as_str(),
        ])],
        GitOp::RestoreBranch { name, oid } => vec![CompiledStep::new(vec![
            "branch".to_string(),
            "--".to_string(),
            name.clone(),
            oid.to_string(),
        ])],
        GitOp::DeleteRemoteBranch { remote, branch } => vec![CompiledStep::new([
            "push",
            "--delete",
            "--",
            remote.as_str(),
            branch.as_str(),
        ])],
        GitOp::CreateTag {
            name,
            target,
            message,
            force,
            sign,
        } => {
            let mut args = vec!["tag".to_string()];
            if *force {
                args.push("-f".to_string());
            }
            if *sign {
                let msg = message
                    .as_deref()
                    .filter(|m| !m.trim().is_empty())
                    .unwrap_or(name.as_str());
                args.extend(["-s".to_string(), "-m".to_string(), msg.to_string()]);
            } else if let Some(m) = message.as_ref().filter(|m| !m.trim().is_empty()) {
                args.extend(["-a".to_string(), "-m".to_string(), m.clone()]);
            }
            args.extend(["--".to_string(), name.clone(), target.to_string()]);
            vec![CompiledStep::new(args)]
        }
        GitOp::DeleteTag { name, .. } => {
            vec![CompiledStep::new(["tag", "-d", "--", name.as_str()])]
        }
        GitOp::RestoreTag { name, oid } => vec![CompiledStep::new(vec![
            "tag".to_string(),
            "--".to_string(),
            name.clone(),
            oid.to_string(),
        ])],
        GitOp::DeleteRemoteTag { remote, name } => vec![CompiledStep::new([
            "push",
            remote.as_str(),
            "--delete",
            &format!("refs/tags/{name}"),
        ])],
        GitOp::PushTag {
            remote,
            name,
            force,
        } => {
            let mut args = vec!["push".to_string()];
            if *force {
                args.push("--force".to_string());
            }
            args.push(remote.clone());
            args.push(format!("refs/tags/{name}:refs/tags/{name}"));
            vec![CompiledStep::new(args)]
        }
        GitOp::AddRemote { name, url } => vec![CompiledStep::new([
            "remote",
            "add",
            "--",
            name.as_str(),
            url.as_str(),
        ])],
        GitOp::RenameRemote { old, new } => vec![CompiledStep::new([
            "remote",
            "rename",
            "--",
            old.as_str(),
            new.as_str(),
        ])],
        GitOp::RemoveRemote { name } => {
            vec![CompiledStep::new(["remote", "remove", "--", name.as_str()])]
        }
        GitOp::SetRemoteUrl { name, url, push } => {
            let mut args = vec!["remote".to_string(), "set-url".to_string()];
            if *push {
                args.push("--push".to_string());
            }
            args.extend(["--".to_string(), name.clone(), url.clone()]);
            vec![CompiledStep::new(args)]
        }
        GitOp::FetchRemote { name } => {
            vec![CompiledStep::new(["fetch", "--prune", name.as_str()])]
        }
        GitOp::AddWorktree {
            path,
            branch,
            force,
        } => {
            let path = path.to_string_lossy().into_owned();
            let mut args = vec!["worktree".to_string(), "add".to_string()];
            if *force {
                args.push("--force".to_string());
            }
            let tail = match branch {
                WorktreeBranch::Existing(name) => vec![path, name.clone()],
                WorktreeBranch::New { name, start } => {
                    args.extend(["-b".to_string(), name.clone()]);
                    vec![path, start.clone()]
                }
                WorktreeBranch::Detached(oid) => {
                    args.push("--detach".to_string());
                    vec![path, oid.to_string()]
                }
            };
            args.push("--".to_string());
            args.extend(tail);
            vec![CompiledStep::new(args)]
        }
        GitOp::RemoveWorktree { path, force } => {
            let mut args = vec!["worktree".to_string(), "remove".to_string()];
            if *force {
                args.push("--force".to_string());
            }
            args.extend(["--".to_string(), path.to_string_lossy().into_owned()]);
            vec![CompiledStep::new(args)]
        }
        GitOp::LockWorktree { path, reason } => {
            let mut args = vec!["worktree".to_string(), "lock".to_string()];
            if let Some(r) = reason.as_ref().filter(|r| !r.trim().is_empty()) {
                args.extend(["--reason".to_string(), r.trim().to_string()]);
            }
            args.extend(["--".to_string(), path.to_string_lossy().into_owned()]);
            vec![CompiledStep::new(args)]
        }
        GitOp::UnlockWorktree { path } => vec![CompiledStep::new(vec![
            "worktree".to_string(),
            "unlock".to_string(),
            "--".to_string(),
            path.to_string_lossy().into_owned(),
        ])],
        GitOp::PruneWorktrees => vec![CompiledStep::new(["worktree", "prune", "--verbose"])],
        GitOp::Merge { rev, options } => vec![compile_merge(rev, options)],
        GitOp::Rebase { options } => vec![compile_rebase(options)],
        GitOp::CherryPick {
            oids,
            no_commit,
            record_origin,
            mainline,
        } => {
            let mut args = vec!["cherry-pick".to_string()];
            if *no_commit {
                args.push("-n".to_string());
            }
            if *record_origin && !*no_commit {
                args.push("-x".to_string());
            }
            if let Some(parent) = mainline {
                args.push("-m".to_string());
                args.push(parent.to_string());
            }
            args.extend(oids.iter().map(ToString::to_string));
            vec![no_edit(CompiledStep::new(args))]
        }
        GitOp::Revert {
            oids,
            no_commit,
            mainline,
        } => {
            let mut args = vec!["revert".to_string()];
            if *no_commit {
                args.push("-n".to_string());
            } else {
                args.push("--no-edit".to_string());
            }
            if let Some(parent) = mainline {
                args.push("-m".to_string());
                args.push(parent.to_string());
            }
            args.extend(oids.iter().map(ToString::to_string));
            vec![no_edit(CompiledStep::new(args))]
        }
        GitOp::Reset { mode, target } => vec![CompiledStep::new(vec![
            "reset".to_string(),
            mode.flag().to_string(),
            target.to_string(),
        ])],
        GitOp::Fetch => vec![CompiledStep::new(["fetch", "--all", "--prune"])],
        GitOp::Pull { options } => vec![compile_pull(options)],
        GitOp::Push {
            branch,
            remote,
            remote_branch,
            set_upstream,
            force,
            tags,
            up_to,
            no_verify,
            expected_remote_oid,
        } => {
            let mut args = vec!["push".to_string(), "--porcelain".to_string()];
            let is_detached = branch == "HEAD";
            if *set_upstream && !is_detached {
                args.push("--set-upstream".to_string());
            }
            if *no_verify {
                args.push("--no-verify".to_string());
            }
            let target_branch = if remote_branch.is_empty() {
                branch.as_str()
            } else {
                remote_branch.as_str()
            };
            let dest_ref = if target_branch.starts_with("refs/") {
                target_branch.to_string()
            } else {
                format!("refs/heads/{target_branch}")
            };
            match force {
                jbgit_core::PushForce::WithLease => {
                    if let Some(oid) = expected_remote_oid {
                        args.push(format!("--force-with-lease={dest_ref}:{oid}"));
                    } else {
                        args.push("--force-with-lease".to_string());
                    }
                }
                jbgit_core::PushForce::Force => {
                    args.push("--force".to_string());
                }
                jbgit_core::PushForce::None => {}
            }
            if let Some(flag) = tags.flag() {
                args.push(flag.to_string());
            }
            args.push(remote.clone());
            match up_to {
                Some(oid) => args.push(format!("{oid}:{dest_ref}")),
                None if is_detached => args.push(format!("HEAD:{dest_ref}")),
                None => args.push(format!("refs/heads/{branch}:{dest_ref}")),
            }
            vec![CompiledStep::new(args)]
        }
        GitOp::SetIdentity {
            name,
            email,
            global,
        } => {
            let mut name_args = vec!["config".to_string()];
            let mut email_args = vec!["config".to_string()];
            if *global {
                name_args.push("--global".to_string());
                email_args.push("--global".to_string());
            }
            name_args.push("--".to_string());
            name_args.push("user.name".to_string());
            name_args.push(name.clone());
            email_args.push("--".to_string());
            email_args.push("user.email".to_string());
            email_args.push(email.clone());
            vec![CompiledStep::new(name_args), CompiledStep::new(email_args)]
        }
        GitOp::SetUpstream { branch, upstream } => vec![CompiledStep::new(vec![
            "branch".to_string(),
            format!("--set-upstream-to={upstream}"),
            "--".to_string(),
            branch.clone(),
        ])],
        GitOp::UnsetUpstream { branch } => vec![CompiledStep::new(vec![
            "branch".to_string(),
            "--unset-upstream".to_string(),
            "--".to_string(),
            branch.clone(),
        ])],
        GitOp::UpdateBranch {
            branch,
            remote,
            remote_branch,
        } => vec![CompiledStep::new(vec![
            "fetch".to_string(),
            remote.clone(),
            format!("refs/heads/{remote_branch}:refs/heads/{branch}"),
        ])],
        GitOp::RewordHead { message, no_verify } => {
            let mut args = vec![
                "commit".to_string(),
                "--amend".to_string(),
                "--only".to_string(),
                "--allow-empty".to_string(),
            ];
            if *no_verify {
                args.push("--no-verify".to_string());
            }
            args.extend(["-F".to_string(), "-".to_string()]);
            vec![CompiledStep::new(args).with_stdin(message.as_bytes().to_vec())]
        }
        GitOp::FixupCommit {
            target,
            kind,
            base,
            autosquash,
        } => {
            let mut commit_args = vec!["commit".to_string()];
            if *kind == FixupKind::Squash {
                commit_args.push("--no-edit".to_string());
            }
            commit_args.push(format!("{}{target}", kind.flag_prefix()));
            let mut steps = vec![no_edit(CompiledStep::new(commit_args))];
            if *autosquash {
                let onto = base.map_or_else(|| "--root".to_string(), |b| b.to_string());
                steps.push(
                    CompiledStep::new(vec![
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
        GitOp::Ongoing { kind, step } => {
            vec![no_edit(CompiledStep::new([kind.command(), step.flag()]))]
        }
        GitOp::StashPush {
            message,
            include_untracked,
            keep_index,
            paths,
        } => {
            let mut args = vec!["stash".to_string(), "push".to_string()];
            if *include_untracked {
                args.push("--include-untracked".to_string());
            }
            if *keep_index {
                args.push("--keep-index".to_string());
            }
            if let Some(m) = message.as_ref().filter(|m| !m.trim().is_empty()) {
                args.extend(["-m".to_string(), m.clone()]);
            }
            if paths.is_empty() {
                vec![CompiledStep::new(args)]
            } else {
                vec![CompiledStep::with_pathspecs(args, paths)]
            }
        }
        GitOp::StashApply {
            index,
            pop,
            reinstate_index,
        } => {
            let mut args = vec![
                "stash".to_string(),
                if *pop { "pop" } else { "apply" }.to_string(),
            ];
            if *reinstate_index {
                args.push("--index".to_string());
            }
            args.push(format!("stash@{{{index}}}"));
            vec![CompiledStep::new(args)]
        }
        GitOp::StashDrop { index } => vec![CompiledStep::new(vec![
            "stash".to_string(),
            "drop".to_string(),
            format!("stash@{{{index}}}"),
        ])],
        GitOp::StashClear => vec![CompiledStep::new(["stash", "clear"])],
        GitOp::StashBranch { index, name } => vec![CompiledStep::new(vec![
            "stash".to_string(),
            "branch".to_string(),
            name.clone(),
            format!("stash@{{{index}}}"),
        ])],
        GitOp::UnresolveConflict { paths } => {
            if paths.is_empty() {
                Vec::new()
            } else {
                vec![CompiledStep::with_pathspecs(
                    vec!["checkout".to_string(), "-m".to_string()],
                    paths,
                )]
            }
        }
        GitOp::IntentToAdd { paths } => {
            if paths.is_empty() {
                Vec::new()
            } else {
                vec![CompiledStep::with_pathspecs(
                    vec!["add".to_string(), "-N".to_string()],
                    paths,
                )]
            }
        }
        GitOp::CheckoutPaths { rev, paths } => {
            if paths.is_empty() {
                Vec::new()
            } else {
                vec![CompiledStep::with_pathspecs(
                    vec!["checkout".to_string(), rev.clone()],
                    paths,
                )]
            }
        }
        GitOp::RestorePaths {
            rev,
            paths,
            staged,
            worktree,
        } => {
            if paths.is_empty() {
                Vec::new()
            } else {
                let mut args = vec!["restore".to_string(), format!("--source={rev}")];
                if *staged {
                    args.push("--staged".to_string());
                }
                if *worktree || !*staged {
                    args.push("--worktree".to_string());
                }
                vec![CompiledStep::with_pathspecs(args, paths)]
            }
        }
        GitOp::SubmoduleUpdate { path } => {
            let mut args = vec![
                "submodule".to_string(),
                "update".to_string(),
                "--init".to_string(),
                "--recursive".to_string(),
            ];
            if let Some(p) = path {
                args.push("--".to_string());
                args.push(p.clone());
            }
            vec![CompiledStep::new(args)]
        }
        GitOp::Unshallow { remote } => vec![CompiledStep::new([
            "fetch",
            "--unshallow",
            "--progress",
            remote.as_str(),
        ])],
        GitOp::BisectStart { bad, good } => {
            let mut args = vec!["bisect".to_string(), "start".to_string()];
            if let Some(b) = bad.as_deref().filter(|s| !s.trim().is_empty()) {
                args.push(b.to_string());
                args.extend(good.iter().cloned());
            } else if !good.is_empty() {
                args.push("HEAD".to_string());
                args.extend(good.iter().cloned());
            }
            vec![CompiledStep::new(args)]
        }
        GitOp::BisectMark { good, rev } => {
            let mut args = vec![
                "bisect".to_string(),
                if *good { "good" } else { "bad" }.to_string(),
            ];
            if let Some(r) = rev.as_deref().filter(|s| !s.trim().is_empty()) {
                args.push(r.to_string());
            }
            vec![CompiledStep::new(args)]
        }
        GitOp::BisectSkip => vec![CompiledStep::new(["bisect", "skip"])],
        GitOp::BisectReset => vec![CompiledStep::new(["bisect", "reset"])],
        GitOp::AcceptSide { .. }
        | GitOp::RewriteHistory { .. }
        | GitOp::ApplyPatch { .. }
        | GitOp::CreatePatchFile { .. }
        | GitOp::PatchToClipboard { .. }
        | GitOp::AddToIgnore { .. }
        | GitOp::DeleteFiles { .. }
        | GitOp::Sequence(_)
        | GitOp::UpdateProject { .. }
        | GitOp::CherryPickAllowEmpty
        | GitOp::Smart(_) => return None,
    };
    Some(steps)
}

/// One `refs/heads/*` entry as seen by Update Project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LocalBranchRef {
    pub name: String,
    pub oid: Oid,
    /// Full upstream ref name (`refs/remotes/origin/main`), empty when untracked.
    pub upstream: String,
    /// Short upstream name (`origin/main`).
    pub upstream_short: String,
    pub is_head: bool,
}

/// Parsed `git for-each-ref` output over `refs/heads` + `refs/remotes`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct BranchRefTable {
    pub heads: Vec<LocalBranchRef>,
    pub tips: HashMap<String, Oid>,
}

impl BranchRefTable {
    pub fn current(&self) -> Option<&LocalBranchRef> {
        self.heads.iter().find(|h| h.is_head)
    }

    pub fn tip(&self, full_ref: &str) -> Option<Oid> {
        self.tips.get(full_ref).copied()
    }
}

/// `--format` of [`parse_branch_refs`] (tab separated; ref names never contain tabs).
pub(crate) const BRANCH_REFS_FORMAT: &str =
    "--format=%(refname)%09%(objectname)%09%(upstream)%09%(upstream:short)%09%(HEAD)";

pub(crate) fn parse_branch_refs(lines: &[String]) -> BranchRefTable {
    let mut table = BranchRefTable::default();
    for line in lines {
        let mut parts = line.trim_end_matches('\r').split('\t');
        let (Some(full), Some(oid)) = (parts.next(), parts.next()) else {
            continue;
        };
        let Ok(oid) = oid.trim().parse::<Oid>() else {
            continue;
        };
        let upstream = parts.next().unwrap_or("").trim().to_string();
        let upstream_short = parts.next().unwrap_or("").trim().to_string();
        let is_head = parts.next().unwrap_or("").trim() == "*";
        table.tips.insert(full.to_string(), oid);
        if let Some(name) = full.strip_prefix("refs/heads/") {
            table.heads.push(LocalBranchRef {
                name: name.to_string(),
                oid,
                upstream,
                upstream_short,
                is_head,
            });
        }
    }
    table
}

/// Keeps a commit-message file Git prepared for the next commit, minus comment lines
/// matching the repository's `core.commentChar`.
#[cfg(test)]
pub(crate) fn clean_prepared_message(raw: &str) -> Option<String> {
    clean_prepared_message_with_char(raw, '#')
}

pub(crate) fn clean_prepared_message_with_char(raw: &str, comment_char: char) -> Option<String> {
    let cleaned = jbgit_core::strip_commit_comments(raw, comment_char);
    let trimmed = cleaned.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn compile_merge(rev: &str, options: &MergeOptions) -> CompiledStep {
    if *options == MergeOptions::default() {
        return CompiledStep::new(["merge", "--no-edit", rev]).with_env("GIT_EDITOR", ":");
    }
    let mut args = vec!["merge".to_string()];
    if let Some(flag) = options.ff.flag() {
        args.push(flag.to_string());
    }
    if options.squash {
        args.push("--squash".to_string());
    }
    if options.no_commit {
        args.push("--no-commit".to_string());
    }
    if let Some(s) = options.strategy.as_deref().filter(|s| !s.trim().is_empty()) {
        args.extend(["-s".to_string(), s.trim().to_string()]);
    }
    if let Some(x) = options
        .strategy_option
        .as_deref()
        .filter(|s| !s.trim().is_empty())
    {
        args.extend(["-X".to_string(), x.trim().to_string()]);
    }
    if options.allow_unrelated {
        args.push("--allow-unrelated-histories".to_string());
    }
    // `git merge -F -` does not read stdin, so the (possibly multi-line) message
    // travels as one `-m` argument; no shell is involved, so newlines are preserved.
    match options.effective_message() {
        Some(m) => args.extend(["-m".to_string(), m.to_string()]),
        None => args.push("--no-edit".to_string()),
    }
    args.push(rev.to_string());
    CompiledStep::new(args).with_env("GIT_EDITOR", ":")
}

fn compile_pull(options: &PullOptions) -> CompiledStep {
    if *options == PullOptions::default() {
        return CompiledStep::new(["pull", "--no-edit"]).with_env("GIT_EDITOR", ":");
    }
    let mut args = vec!["pull".to_string()];
    if options.rebase {
        args.push("--rebase".to_string());
    } else {
        if let Some(flag) = options.ff.flag() {
            args.push(flag.to_string());
        }
        if options.squash {
            args.push("--squash".to_string());
        }
        if options.no_commit {
            args.push("--no-commit".to_string());
        }
        if !options.squash && !options.no_commit {
            args.push("--no-edit".to_string());
        }
    }
    if options.no_verify {
        args.push("--no-verify".to_string());
    }
    if let Some(remote) = options.effective_remote() {
        args.push(remote.to_string());
        if let Some(branch) = options.effective_branch() {
            args.push(branch.to_string());
        }
    }
    CompiledStep::new(args).with_env("GIT_EDITOR", ":")
}

fn compile_rebase(options: &RebaseOptions) -> CompiledStep {
    let mut args = vec!["rebase".to_string()];
    let use_interactive = options.interactive || options.autosquash;
    if use_interactive {
        args.push("-i".to_string());
    }
    if options.rebase_merges {
        args.push("--rebase-merges".to_string());
    }
    if options.update_refs {
        args.push("--update-refs".to_string());
    }
    if options.keep_empty {
        args.push("--keep-empty".to_string());
    }
    if options.autosquash {
        args.push("--autosquash".to_string());
    }
    if options.root {
        if let Some(onto) = options.effective_onto() {
            args.push("--onto".to_string());
            args.push(onto.to_string());
        }
        args.push("--root".to_string());
    } else if let Some(upstream) = options.effective_upstream() {
        if let Some(onto) = options.effective_onto() {
            args.push("--onto".to_string());
            args.push(onto.to_string());
        }
        args.push(upstream.to_string());
    } else if let Some(onto) = options.effective_onto() {
        args.push(onto.to_string());
    }
    if let Some(branch) = options.effective_branch() {
        args.push(branch.to_string());
    }
    let step = CompiledStep::new(args).with_env("GIT_EDITOR", ":");
    if use_interactive {
        step.with_env("GIT_SEQUENCE_EDITOR", ":")
    } else {
        step
    }
}

/// Standard Git empty-tree object id (used when diffing against an unborn HEAD).
const EMPTY_TREE_OID: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

pub(crate) fn compile_patch_source(
    source: &PatchSource,
    reverse: bool,
    head_exists: bool,
) -> Vec<CompiledStep> {
    match source {
        PatchSource::Commits { oids, .. } => {
            if !reverse {
                oids.iter()
                    .rev()
                    .map(|oid| {
                        CompiledStep::new([
                            "format-patch",
                            "--stdout",
                            "-1",
                            &oid.to_string(),
                        ])
                    })
                    .collect()
            } else {
                oids.iter()
                    .map(|oid| {
                        CompiledStep::new([
                            "show",
                            "--format=",
                            "--patch",
                            "-R",
                            &oid.to_string(),
                        ])
                    })
                    .collect()
            }
        }
        PatchSource::CommitFile { oid, path } => {
            let mut args = vec![
                "show".to_string(),
                "--format=".to_string(),
                "--patch".to_string(),
            ];
            if reverse {
                args.push("-R".to_string());
            }
            args.push(oid.to_string());
            args.push("--".to_string());
            args.push(path.clone());
            vec![CompiledStep::new(args)]
        }
        PatchSource::StagePaths {
            staged,
            unstaged,
            ..
        } => {
            let staged_set: HashSet<&str> = staged.iter().map(String::as_str).collect();
            let unstaged_set: HashSet<&str> = unstaged.iter().map(String::as_str).collect();
            let mut both: Vec<String> = staged_set
                .intersection(&unstaged_set)
                .map(|s| (*s).to_string())
                .collect();
            let mut staged_only: Vec<String> = staged_set
                .difference(&unstaged_set)
                .map(|s| (*s).to_string())
                .collect();
            let mut unstaged_only: Vec<String> = unstaged_set
                .difference(&staged_set)
                .map(|s| (*s).to_string())
                .collect();
            both.sort();
            staged_only.sort();
            unstaged_only.sort();

            let mut steps = Vec::new();
            if !both.is_empty() {
                let mut args = vec!["diff".to_string()];
                if reverse {
                    args.push("-R".to_string());
                }
                args.push(if head_exists { "HEAD" } else { EMPTY_TREE_OID }.to_string());
                args.push("--".to_string());
                args.extend(both);
                steps.push(CompiledStep::new(args));
            }
            if !staged_only.is_empty() {
                let mut args = vec!["diff".to_string(), "--cached".to_string()];
                if reverse {
                    args.push("-R".to_string());
                }
                args.push("--".to_string());
                args.extend(staged_only);
                steps.push(CompiledStep::new(args));
            }
            if !unstaged_only.is_empty() {
                let mut args = vec!["diff".to_string()];
                if reverse {
                    args.push("-R".to_string());
                }
                args.push("--".to_string());
                args.extend(unstaged_only);
                steps.push(CompiledStep::new(args));
            }
            steps
        }
    }
}

/// Compiles the CLI steps for [`GitOp::AcceptSide`]: paths whose accepted stage
/// has a blob run `git checkout --ours/--theirs` + `git add`; paths whose
/// accepted stage is absent (Delete/Modify or Both-Deleted) run `git rm -rf`.
#[cfg(test)]
pub(crate) fn compile_accept_side_steps(
    to_checkout_add: &[String],
    to_remove: &[String],
    git_ours: bool,
) -> Vec<CompiledStep> {
    compile_accept_side_steps_with_gitlinks(to_checkout_add, &[], to_remove, git_ours)
}

fn compile_accept_side_steps_with_gitlinks(
    to_checkout_add: &[String],
    to_update_gitlinks: &[(String, String)],
    to_remove: &[String],
    git_ours: bool,
) -> Vec<CompiledStep> {
    let mut steps = Vec::new();
    if !to_checkout_add.is_empty() {
        steps.push(CompiledStep::with_pathspecs(
            vec![
                "checkout".to_string(),
                if git_ours { "--ours" } else { "--theirs" }.to_string(),
            ],
            to_checkout_add,
        ));
        steps.push(CompiledStep::with_pathspecs(
            vec!["add".to_string()],
            to_checkout_add,
        ));
    }
    for (path, oid_hex) in to_update_gitlinks {
        // Update submodule gitlink index entry directly to the chosen commit OID
        // (avoids `git add` overwriting stage 0 with the unchanged submodule workdir HEAD).
        steps.push(CompiledStep::new(vec![
            "update-index".to_string(),
            "--cacheinfo".to_string(),
            format!("160000,{oid_hex},{path}"),
        ]));
    }
    if !to_remove.is_empty() {
        steps.push(CompiledStep::with_pathspecs(
            vec!["rm".to_string(), "-r".to_string(), "-f".to_string()],
            to_remove,
        ));
    }
    steps
}

struct AcceptSidePlan {
    git_ours: bool,
    to_checkout_add: Vec<String>,
    to_update_gitlinks: Vec<(String, String)>,
    to_remove: Vec<String>,
}

/// Resolves semantic `ours` (`true` = Accept Yours, `false` = Accept Theirs)
/// against the repository state (flipping `--ours`/`--theirs` during rebase)
/// and partitions `paths` into `(to_checkout_add, to_update_gitlinks, to_remove)`
/// based on index conflict stage presence and mode.
fn plan_accept_side(git_dir: &Path, paths: &[String], ours: bool) -> AcceptSidePlan {
    let Ok(repo) = git2::Repository::open(git_dir) else {
        return AcceptSidePlan {
            git_ours: ours,
            to_checkout_add: paths.to_vec(),
            to_update_gitlinks: Vec::new(),
            to_remove: Vec::new(),
        };
    };
    let probe = crate::probe::probe_repo_state(&repo);
    let reversed = is_reverse_conflict_state(&probe.state);
    let git_ours = ours ^ reversed;

    type ConflictStageEntry = (Option<(u32, String)>, Option<(u32, String)>);
    let mut stage_entries: HashMap<String, ConflictStageEntry> = HashMap::new();
    if let Ok(index) = repo.index()
        && let Ok(conflicts) = index.conflicts()
    {
        for item in conflicts.flatten() {
            let entry_path = item
                .our
                .as_ref()
                .or(item.their.as_ref())
                .or(item.ancestor.as_ref())
                .map(|e| String::from_utf8_lossy(&e.path).replace('\\', "/"));
            if let Some(p) = entry_path {
                let our_info = item.our.as_ref().map(|e| (e.mode, e.id.to_string()));
                let their_info = item.their.as_ref().map(|e| (e.mode, e.id.to_string()));
                stage_entries.insert(p, (our_info, their_info));
            }
        }
    }

    let mut to_checkout_add = Vec::new();
    let mut to_update_gitlinks = Vec::new();
    let mut to_remove = Vec::new();
    for p in paths {
        let norm = p.replace('\\', "/");
        match stage_entries.get(&norm) {
            Some((our_info, their_info)) => {
                let chosen = if git_ours { our_info } else { their_info };
                match chosen {
                    Some((mode, oid_hex)) if *mode == jbgit_core::GIT_MODE_GITLINK => {
                        to_update_gitlinks.push((norm, oid_hex.clone()));
                    }
                    Some(_) => to_checkout_add.push(p.clone()),
                    None => to_remove.push(p.clone()),
                }
            }
            None => to_checkout_add.push(p.clone()),
        }
    }
    AcceptSidePlan {
        git_ours,
        to_checkout_add,
        to_update_gitlinks,
        to_remove,
    }
}

/// Whether a compiled step talks to a remote (and may need credentials).
pub(crate) fn is_network_step(args: &[String]) -> bool {
    matches!(
        args.first().map(String::as_str),
        Some("fetch" | "pull" | "push" | "ls-remote" | "submodule")
    )
}

/// Runs `op` to completion through [`GitCliRunner`] and reports both the
/// classified [`GitOpOutcome`] and any `index.lock` conflict.
///
/// `askpass` (when set) is added to the environment of network steps only
/// (`fetch` / `pull` / `push` / `ls-remote` / `submodule`), so credential
/// prompts reach the TUI; local commands never see it.
pub(crate) async fn run_git_op<R, F>(
    cli: &R,
    loc: &RepoLocation,
    ids: &AtomicU64,
    op: GitOp,
    askpass: Option<&AskpassEnv>,
    on_cli: F,
) -> GitOpRunResult
where
    R: GitCliRunner,
    F: Fn(CliStreamEvent) + Send + Sync,
{
    let askpass_vars = if op.is_network() {
        askpass.map(AskpassEnv::vars).unwrap_or_default()
    } else {
        Vec::new()
    };
    let runner = Runner {
        cli,
        loc,
        ids,
        askpass_vars: &askpass_vars,
        on_cli: &on_cli,
    };
    match op {
        GitOp::Smart(inner) => runner.run_smart(*inner).await,
        other => runner.run_plain(other).await,
    }
}

struct Runner<'a, R: GitCliRunner> {
    cli: &'a R,
    loc: &'a RepoLocation,
    ids: &'a AtomicU64,
    askpass_vars: &'a [(String, String)],
    on_cli: &'a (dyn Fn(CliStreamEvent) + Send + Sync),
}

impl<R: GitCliRunner> Runner<'_, R> {
    async fn step(&self, step: &CompiledStep, read_only: bool) -> StepResult {
        let id = OP_CONSOLE_ID_BASE + self.ids.fetch_add(1, Ordering::Relaxed);
        let with_askpass;
        let env: &[(String, String)] =
            if !self.askpass_vars.is_empty() && is_network_step(&step.args) {
                with_askpass = step
                    .env
                    .iter()
                    .chain(self.askpass_vars.iter())
                    .cloned()
                    .collect::<Vec<_>>();
                &with_askpass
            } else {
                &step.env
            };
        let res = self
            .cli
            .execute_boxed(
                CliCommand {
                    id,
                    workdir: &self.loc.workdir,
                    git_dir: &self.loc.git_dir,
                    args: &step.args,
                    stdin_payload: step.stdin.as_deref(),
                    is_read_only: read_only,
                    env,
                },
                self.on_cli,
            )
            .await;
        match res {
            Ok(out) => {
                let index_lock = out.index_lock_conflict.map(|(lock_path, message)| {
                    IndexLockConflict {
                        lock_path,
                        message,
                        retry_args: step.args.clone(),
                        retry_stdin: step.stdin.clone(),
                        retry_is_read_only: read_only,
                    }
                });
                StepResult {
                    exit_code: out.exit_code,
                    stdout: out.stdout_lines,
                    stderr: out.stderr_lines,
                    index_lock,
                }
            }
            Err(e) => StepResult {
                exit_code: -1,
                stdout: Vec::new(),
                stderr: vec![format!("fatal: {e}")],
                index_lock: None,
            },
        }
    }

    async fn run_steps(&self, op: GitOp, steps: Vec<CompiledStep>) -> GitOpRunResult {
        let read_only = op.is_read_only();
        let mut outcome = GitOpOutcome::ok(op);
        for step in &steps {
            let res = self.step(step, read_only).await;
            outcome.stderr.extend(res.stderr.iter().cloned());
            outcome.stdout = res.stdout.clone();
            if !res.ok() {
                outcome.success = false;
                outcome.failure = Some(classify_failure(&res.all_lines()));
                return GitOpRunResult {
                    outcome,
                    index_lock: res.index_lock,
                };
            }
        }
        GitOpRunResult {
            outcome,
            index_lock: None,
        }
    }

    fn is_rebase_active(&self) -> bool {
        self.loc.git_dir.join("rebase-merge").is_dir()
            || self.loc.git_dir.join("rebase-apply").is_dir()
    }

    async fn run_plain(&self, op: GitOp) -> GitOpRunResult {
        if let Some(steps) = compile_simple_steps(&op) {
            let prepared = match &op {
                GitOp::Merge { options, .. } if options.leaves_changes_uncommitted() => {
                    Some(if options.squash { "SQUASH_MSG" } else { "MERGE_MSG" })
                }
                GitOp::Pull { options } if options.leaves_changes_uncommitted() => {
                    Some(if options.squash { "SQUASH_MSG" } else { "MERGE_MSG" })
                }
                _ => None,
            };
            let is_ongoing_rebase = matches!(
                &op,
                GitOp::Ongoing {
                    kind: OngoingKind::Rebase,
                    step: OngoingStep::Continue | OngoingStep::Abort | OngoingStep::Skip,
                }
            );
            let mut res = self.run_steps(op, steps).await;
            if res.outcome.success
                && let Some(file) = prepared
            {
                let comment_char = git2::Repository::open(&self.loc.git_dir)
                    .ok()
                    .and_then(|r| r.config().ok())
                    .and_then(|c| c.get_string("core.commentChar").ok())
                    .map(|v| jbgit_core::parse_comment_char(Some(&v)))
                    .unwrap_or('#');
                res.outcome.prepared_message =
                    std::fs::read_to_string(self.loc.git_dir.join(file))
                        .ok()
                        .and_then(|raw| clean_prepared_message_with_char(&raw, comment_char));
            }
            if is_ongoing_rebase {
                let active = self.is_rebase_active();
                if res.outcome.success && active {
                    res.outcome.paused_for_edit = true;
                }
                if !active {
                    let _ = std::fs::remove_dir_all(self.loc.git_dir.join("jbgit-rebase"));
                }
            }
            if res.outcome.success
                && let GitOp::AddWorktree { path, .. } = &res.outcome.op
            {
                let src = self.loc.workdir.clone();
                let dst = if path.is_relative() {
                    src.join(path)
                } else {
                    path.clone()
                };
                let _ = tokio::task::spawn_blocking(move || {
                    apply_worktreeinclude_sync(&src, &dst);
                })
                .await;
            }
            return res;
        }
        match op.clone() {
            GitOp::CherryPickAllowEmpty => {
                let commit_step = CompiledStep::new([
                    "commit",
                    "--allow-empty",
                    "--no-edit",
                ])
                .with_env("GIT_EDITOR", ":");
                let res = self.run_steps(op.clone(), vec![commit_step]).await;
                if res.outcome.success
                    && self.loc.git_dir.join("sequencer").join("todo").exists()
                {
                    let cont = CompiledStep::new(["cherry-pick", "--continue"])
                        .with_env("GIT_EDITOR", ":");
                    return self.run_steps(op, vec![cont]).await;
                }
                res
            }
            GitOp::AcceptSide { paths, ours } => self.run_accept_side(op, paths, ours).await,
            GitOp::ApplyPatch { source, options } => {
                let (file, stdin) = match crate::details::resolve_patch_input(self.loc, &source) {
                    Ok(x) => x,
                    Err(e) => {
                        return GitOpRunResult {
                            outcome: GitOpOutcome::failed(op, GitFailure::PatchDoesNotApply {
                                detail: e,
                            }),
                            index_lock: None,
                        };
                    }
                };
                let mut step = CompiledStep::new(crate::details::apply_args(
                    &options,
                    false,
                    file.as_deref(),
                ));
                step.stdin = stdin;
                let mut res = self.run_steps(op, vec![step]).await;
                // `--3way` prints `U <path>` on stdout and the notes on stderr.
                if let Some(GitFailure::PatchConflicts { paths }) = &mut res.outcome.failure
                    && paths.is_empty()
                {
                    *paths = jbgit_core::parse_apply_conflicts(&res.outcome.stdout);
                }
                res
            }
            GitOp::CreatePatchFile {
                source,
                output,
                reverse,
            } => self.run_create_patch_file(op, source, output, reverse).await,
            GitOp::PatchToClipboard { source, reverse } => {
                self.run_patch_to_clipboard(op, source, reverse).await
            }
            GitOp::RewriteHistory {
                base,
                todo,
                no_verify,
            } => {
                let dir = self.loc.git_dir.join("jbgit-rebase");
                let _ = std::fs::remove_dir_all(&dir);
                if let Err(e) = std::fs::create_dir_all(&dir) {
                    return GitOpRunResult {
                        outcome: GitOpOutcome::failed(op, GitFailure::Other(e.to_string())),
                        index_lock: None,
                    };
                }
                let mut write_err = None;
                let text = render_todo(&todo, no_verify, |i| {
                    let path = dir.join(format!("msg-{i}.txt"));
                    let msg = todo[i].message.clone().unwrap_or_default();
                    if let Err(e) = std::fs::write(&path, msg) {
                        write_err = Some(e.to_string());
                    }
                    sh_quote(&path)
                });
                let todo_path = dir.join("git-rebase-todo");
                if let Err(e) = std::fs::write(&todo_path, text) {
                    write_err = Some(e.to_string());
                }
                if let Some(e) = write_err {
                    return GitOpRunResult {
                        outcome: GitOpOutcome::failed(op, GitFailure::Other(e)),
                        index_lock: None,
                    };
                }
                let mut args = vec![
                    "rebase".to_string(),
                    "-i".to_string(),
                    "--autostash".to_string(),
                    "--no-autosquash".to_string(),
                ];
                match base {
                    Some(b) => args.push(b.to_string()),
                    None => args.push("--root".to_string()),
                }
                let step = CompiledStep::new(args)
                    .with_env("GIT_SEQUENCE_EDITOR", &sequence_editor_script(&todo_path))
                    .with_env("GIT_EDITOR", ":");
                let res = self.step(&step, false).await;
                let mut finished = finish(op, res);
                let rebase_active = self.is_rebase_active();
                if finished.outcome.success && rebase_active {
                    finished.outcome.paused_for_edit = true;
                }
                if !rebase_active {
                    let _ = std::fs::remove_dir_all(&dir);
                }
                finished
            }
            GitOp::AddToIgnore {
                patterns,
                exclude_file,
                target_file,
            } => {
                let path = if let Some(rel) = target_file {
                    if rel.is_absolute() {
                        rel
                    } else {
                        self.loc.workdir.join(rel)
                    }
                } else if exclude_file {
                    self.loc.common_dir.join("info").join("exclude")
                } else {
                    self.loc.workdir.join(".gitignore")
                };
                let outcome = match append_lines(&path, &patterns) {
                    Ok(()) => GitOpOutcome::ok(op),
                    Err(e) => GitOpOutcome::failed(op, GitFailure::Other(e.to_string())),
                };
                GitOpRunResult {
                    outcome,
                    index_lock: None,
                }
            }
            GitOp::DeleteFiles { paths } => {
                for p in &paths {
                    let full = self.loc.workdir.join(p);
                    let res = if full.is_dir() {
                        std::fs::remove_dir_all(&full)
                    } else {
                        std::fs::remove_file(&full)
                    };
                    if let Err(e) = res
                        && e.kind() != std::io::ErrorKind::NotFound
                    {
                        return GitOpRunResult {
                            outcome: GitOpOutcome::failed(
                                op,
                                GitFailure::Other(format!("{p}: {e}")),
                            ),
                            index_lock: None,
                        };
                    }
                }
                GitOpRunResult {
                    outcome: GitOpOutcome::ok(op),
                    index_lock: None,
                }
            }
            GitOp::Sequence(ops) => self.run_sequence(ops).await,
            GitOp::UpdateProject {
                mode,
                save_changes,
                update_other_branches,
                update_submodules,
            } => {
                self.run_update_project(
                    op,
                    mode,
                    save_changes,
                    update_other_branches,
                    update_submodules,
                )
                .await
            }
            GitOp::Smart(inner) => Box::pin(self.run_smart(*inner)).await,
            _ => GitOpRunResult {
                outcome: GitOpOutcome::failed(op, GitFailure::Other("unsupported operation".into())),
                index_lock: None,
            },
        }
    }

    async fn run_accept_side(&self, op: GitOp, paths: Vec<String>, ours: bool) -> GitOpRunResult {
        if paths.is_empty() {
            return GitOpRunResult {
                outcome: GitOpOutcome::ok(op),
                index_lock: None,
            };
        }
        let plan = plan_accept_side(&self.loc.git_dir, &paths, ours);
        let steps = compile_accept_side_steps_with_gitlinks(
            &plan.to_checkout_add,
            &plan.to_update_gitlinks,
            &plan.to_remove,
            plan.git_ours,
        );
        self.run_steps(op, steps).await
    }

    async fn collect_patch_lines(
        &self,
        source: &PatchSource,
        reverse: bool,
    ) -> Result<Vec<String>, Box<(GitFailure, Vec<String>, Option<IndexLockConflict>)>> {
        let head_exists = git2::Repository::open(&self.loc.git_dir)
            .ok()
            .is_some_and(|r| r.head().is_ok());
        let steps = compile_patch_source(source, reverse, head_exists);
        let mut lines: Vec<String> = Vec::new();
        for step in &steps {
            let res = self.step(step, true).await;
            if !res.ok() {
                let failure = classify_failure(&res.all_lines());
                return Err(Box::new((failure, res.stderr, res.index_lock)));
            }
            lines.extend(res.stdout);
        }

        if let PatchSource::StagePaths {
            staged,
            unstaged,
            untracked,
        } = source
        {
            let tracked: HashSet<&str> = staged
                .iter()
                .chain(unstaged.iter())
                .map(String::as_str)
                .collect();
            let mut extra: Vec<&str> = untracked
                .iter()
                .map(String::as_str)
                .filter(|p| !tracked.contains(*p))
                .collect();
            extra.sort_unstable();
            extra.dedup();
            for rel in extra {
                let abs = self.loc.workdir.join(rel);
                let meta = match std::fs::symlink_metadata(&abs) {
                    Ok(m) => m,
                    Err(e) => {
                        return Err(Box::new((
                            GitFailure::Other(format!("failed to read '{rel}': {e}")),
                            Vec::new(),
                            None,
                        )));
                    }
                };
                if meta.file_type().is_symlink() {
                    let target = match std::fs::read_link(&abs) {
                        Ok(t) => t.to_string_lossy().replace('\\', "/"),
                        Err(e) => {
                            return Err(Box::new((
                                GitFailure::Other(format!("failed to read symlink '{rel}': {e}")),
                                Vec::new(),
                                None,
                            )));
                        }
                    };
                    let norm = rel.replace('\\', "/");
                    lines.push(format!("diff --git a/{norm} b/{norm}"));
                    if reverse {
                        lines.push("deleted file mode 120000".to_string());
                        lines.push(format!("--- a/{norm}"));
                        lines.push("+++ /dev/null".to_string());
                        lines.push("@@ -1 +0,0 @@".to_string());
                        lines.push(format!("-{target}"));
                        lines.push("\\ No newline at end of file".to_string());
                    } else {
                        lines.push("new file mode 120000".to_string());
                        lines.push("--- /dev/null".to_string());
                        lines.push(format!("+++ b/{norm}"));
                        lines.push("@@ -0,0 +1 @@".to_string());
                        lines.push(format!("+{target}"));
                        lines.push("\\ No newline at end of file".to_string());
                    }
                    continue;
                }
                let use_binary_cli = meta.len() as usize > jbgit_core::MAX_DIFF_FILE_BYTES;
                let maybe_patch = if use_binary_cli {
                    None
                } else {
                    let abs_read = abs.clone();
                    let bytes = match tokio::task::spawn_blocking(move || std::fs::read(&abs_read))
                        .await
                    {
                        Ok(Ok(b)) => b,
                        Ok(Err(e)) => {
                            return Err(Box::new((
                                GitFailure::Other(format!("failed to read '{rel}': {e}")),
                                Vec::new(),
                                None,
                            )));
                        }
                        Err(e) => {
                            return Err(Box::new((
                                GitFailure::Other(format!("failed to read '{rel}': {e}")),
                                Vec::new(),
                                None,
                            )));
                        }
                    };
                    jbgit_core::build_untracked_file_patch(rel, &bytes, reverse)
                };
                if let Some(patch_text) = maybe_patch {
                    let trimmed = patch_text.strip_suffix('\n').unwrap_or(&patch_text);
                    if !trimmed.is_empty() {
                        lines.extend(trimmed.split('\n').map(str::to_string));
                    }
                } else {
                    // Binary untracked file: delegate to `git diff --no-index --binary`.
                    let args = if reverse {
                        vec![
                            "diff".to_string(),
                            "--no-index".to_string(),
                            "--binary".to_string(),
                            "--".to_string(),
                            rel.to_string(),
                            "/dev/null".to_string(),
                        ]
                    } else {
                        vec![
                            "diff".to_string(),
                            "--no-index".to_string(),
                            "--binary".to_string(),
                            "--".to_string(),
                            "/dev/null".to_string(),
                            rel.to_string(),
                        ]
                    };
                    let res = self.step(&CompiledStep::new(args), true).await;
                    // `git diff --no-index` exits 1 when differences exist.
                    if res.exit_code != 0 && res.exit_code != 1 {
                        let failure = classify_failure(&res.all_lines());
                        return Err(Box::new((failure, res.stderr, res.index_lock)));
                    }
                    lines.extend(res.stdout);
                }
            }
        }
        Ok(lines)
    }

    async fn run_patch_to_clipboard(
        &self,
        op: GitOp,
        source: PatchSource,
        reverse: bool,
    ) -> GitOpRunResult {
        match self.collect_patch_lines(&source, reverse).await {
            Ok(stdout) => {
                let mut outcome = GitOpOutcome::ok(op);
                outcome.stdout = stdout;
                GitOpRunResult {
                    outcome,
                    index_lock: None,
                }
            }
            Err(err) => {
                let (failure, stderr, index_lock) = *err;
                let mut outcome = GitOpOutcome::failed(op, failure);
                outcome.stderr = stderr;
                GitOpRunResult {
                    outcome,
                    index_lock,
                }
            }
        }
    }

    async fn run_create_patch_file(
        &self,
        op: GitOp,
        source: PatchSource,
        output: PathBuf,
        reverse: bool,
    ) -> GitOpRunResult {
        let abs = if output.is_absolute() {
            output.clone()
        } else {
            self.loc.workdir.join(&output)
        };
        let raw_s = output.to_string_lossy();
        let wants_dir = abs.is_dir() || raw_s.ends_with('/') || raw_s.ends_with('\\');

        // Directory target for non-reversed commit range -> `git format-patch -o <dir>`.
        if wants_dir
            && !reverse
            && let PatchSource::Commits { oids, .. } = &source
        {
            if oids.is_empty() {
                return GitOpRunResult {
                    outcome: GitOpOutcome::ok(op),
                    index_lock: None,
                };
            }
            if let Err(e) = std::fs::create_dir_all(&abs) {
                return GitOpRunResult {
                    outcome: GitOpOutcome::failed(op, GitFailure::Other(e.to_string())),
                    index_lock: None,
                };
            }
            let out_dir = abs.display().to_string();
            // Emit one format-patch invocation per commit (oldest first) so
            // non-contiguous selections export only the selected commits with
            // clean sequential numbering (`0001-…`, `0002-…`).
            let steps: Vec<CompiledStep> = oids
                .iter()
                .rev()
                .enumerate()
                .map(|(idx, oid)| {
                    CompiledStep::new(vec![
                        "format-patch".to_string(),
                        "-o".to_string(),
                        out_dir.clone(),
                        "--start-number".to_string(),
                        (idx + 1).to_string(),
                        "-1".to_string(),
                        oid.to_string(),
                    ])
                })
                .collect();
            return self.run_steps(op, steps).await;
        }

        let dest = if wants_dir {
            let repo_name = self
                .loc
                .workdir
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "repo".into());
            abs.join(source.suggested_filename(&repo_name))
        } else {
            abs
        };

        let lines = match self.collect_patch_lines(&source, reverse).await {
            Ok(l) => l,
            Err(err) => {
                let (failure, stderr, index_lock) = *err;
                let mut outcome = GitOpOutcome::failed(op, failure);
                outcome.stderr = stderr;
                return GitOpRunResult {
                    outcome,
                    index_lock,
                };
            }
        };
        if let Some(parent) = dest.parent()
            && !parent.as_os_str().is_empty()
            && let Err(e) = std::fs::create_dir_all(parent)
        {
            return GitOpRunResult {
                outcome: GitOpOutcome::failed(op, GitFailure::Other(e.to_string())),
                index_lock: None,
            };
        }
        let mut content = lines.join("\n");
        if !content.is_empty() && !content.ends_with('\n') {
            content.push('\n');
        }
        if let Err(e) = std::fs::write(&dest, content.as_bytes()) {
            return GitOpRunResult {
                outcome: GitOpOutcome::failed(op, GitFailure::Other(e.to_string())),
                index_lock: None,
            };
        }
        let mut outcome = GitOpOutcome::ok(op);
        outcome.stdout = vec![dest.display().to_string()];
        GitOpRunResult {
            outcome,
            index_lock: None,
        }
    }

    async fn run_sequence(&self, ops: Vec<GitOp>) -> GitOpRunResult {
        let mut combined_stderr = Vec::new();
        let mut last_stdout = Vec::new();
        let mut any_stash_kept = false;
        let mut prepared_message = None;
        let mut update_info = None;
        let mut paused_for_edit = false;
        for inner in &ops {
            let mut res = match inner.clone() {
                GitOp::Smart(s) => Box::pin(self.run_smart(*s)).await,
                other => Box::pin(self.run_plain(other)).await,
            };
            combined_stderr.extend(res.outcome.stderr.iter().cloned());
            last_stdout = res.outcome.stdout.clone();
            any_stash_kept |= res.outcome.stash_kept;
            if res.outcome.prepared_message.is_some() {
                prepared_message = res.outcome.prepared_message.clone();
            }
            if res.outcome.update_info.is_some() {
                update_info = res.outcome.update_info.clone();
            }
            paused_for_edit |= res.outcome.paused_for_edit;
            if !res.outcome.success {
                res.outcome.stderr = combined_stderr;
                res.outcome.stash_kept = any_stash_kept;
                return res;
            }
        }
        let mut outcome = GitOpOutcome::ok(GitOp::Sequence(ops));
        outcome.stdout = last_stdout;
        outcome.stderr = combined_stderr;
        outcome.stash_kept = any_stash_kept;
        outcome.prepared_message = prepared_message;
        outcome.update_info = update_info;
        outcome.paused_for_edit = paused_for_edit;
        GitOpRunResult {
            outcome,
            index_lock: None,
        }
    }

    /// Saves local changes (including untracked) to a new stash entry.
    /// `Ok(true)` when something was stashed.
    async fn stash_save(&self, message: &str) -> Result<bool, Box<StepResult>> {
        let save = CompiledStep::new(vec![
            "stash".to_string(),
            "push".to_string(),
            "--include-untracked".to_string(),
            "-m".to_string(),
            message.to_string(),
        ]);
        let saved = self.step(&save, false).await;
        if !saved.ok() {
            return Err(Box::new(saved));
        }
        Ok(!saved
            .all_lines()
            .iter()
            .any(|l| l.contains("No local changes to save")))
    }

    /// `stash pop --index`, falling back to a plain `pop` when the index cannot be restored.
    async fn stash_restore(&self) -> StepResult {
        let pop = self
            .step(&CompiledStep::new(["stash", "pop", "--index"]), false)
            .await;
        if pop.ok() {
            return pop;
        }
        if pop
            .all_lines()
            .iter()
            .any(|l| l.contains("--index") || l.contains("index"))
            && !pop.all_lines().iter().any(|l| l.contains("CONFLICT"))
        {
            return self.step(&CompiledStep::new(["stash", "pop"]), false).await;
        }
        pop
    }

    async fn read_branch_refs(&self) -> Result<BranchRefTable, Box<StepResult>> {
        let res = self
            .step(
                &CompiledStep::new([
                    "for-each-ref",
                    BRANCH_REFS_FORMAT,
                    "refs/heads",
                    "refs/remotes",
                ]),
                true,
            )
            .await;
        if res.ok() {
            Ok(parse_branch_refs(&res.stdout))
        } else {
            Err(Box::new(res))
        }
    }

    async fn read_config(&self, key: &str) -> Option<String> {
        let res = self
            .step(
                &CompiledStep::new(["config", "--default", "", "--get", key]),
                true,
            )
            .await;
        if !res.ok() {
            return None;
        }
        res.stdout
            .iter()
            .map(|l| l.trim())
            .find(|l| !l.is_empty())
            .map(ToString::to_string)
    }

    async fn is_ancestor(&self, ancestor: Oid, descendant: Oid) -> Option<bool> {
        let res = self
            .step(
                &CompiledStep::new(vec![
                    "merge-base".to_string(),
                    "--is-ancestor".to_string(),
                    ancestor.to_string(),
                    descendant.to_string(),
                ]),
                true,
            )
            .await;
        match res.exit_code {
            0 => Some(true),
            1 => Some(false),
            _ => None,
        }
    }

    async fn rev_parse_head(&self) -> Option<Oid> {
        let res = self
            .step(&CompiledStep::new(["rev-parse", "--verify", "HEAD"]), true)
            .await;
        res.stdout.first().and_then(|l| l.trim().parse().ok())
    }

    /// Commits / files brought in by `ranges` (union over all ranges).
    async fn collect_update_info(
        &self,
        ranges: Vec<UpdatedRange>,
        skipped: Vec<SkippedBranch>,
    ) -> UpdateInfo {
        let mut commits: Vec<Oid> = Vec::new();
        let mut seen: HashSet<Oid> = HashSet::new();
        let mut files: HashSet<String> = HashSet::new();
        for r in &ranges {
            let list = self
                .step(
                    &CompiledStep::new(vec![
                        "rev-list".to_string(),
                        format!("--max-count={UPDATE_INFO_COMMIT_CAP}"),
                        r.after.to_string(),
                        format!("^{}", r.before),
                    ]),
                    true,
                )
                .await;
            for oid in list.stdout.iter().filter_map(|l| l.trim().parse::<Oid>().ok()) {
                if seen.insert(oid) {
                    commits.push(oid);
                }
            }
            let diff = self
                .step(
                    &CompiledStep::new(vec![
                        "diff".to_string(),
                        "--name-only".to_string(),
                        "--no-renames".to_string(),
                        r.before.to_string(),
                        r.after.to_string(),
                    ]),
                    true,
                )
                .await;
            files.extend(
                diff.stdout
                    .iter()
                    .map(|l| l.trim())
                    .filter(|l| !l.is_empty())
                    .map(ToString::to_string),
            );
        }
        UpdateInfo {
            new_commits: commits.len(),
            files_changed: files.len(),
            commits,
            ranges,
            skipped,
        }
    }

    /// `GitUpdateProcess`: save → fetch → merge/rebase current → fast-forward
    /// other tracked branches → optional submodule update → restore → [`UpdateInfo`].
    async fn run_update_project(
        &self,
        op: GitOp,
        mode: Option<UpdateMode>,
        save_changes: bool,
        update_other_branches: bool,
        update_submodules: bool,
    ) -> GitOpRunResult {
        let fail = |op: GitOp, res: Option<StepResult>, failure: GitFailure| {
            let mut out = GitOpOutcome::failed(op, failure);
            let mut index_lock = None;
            if let Some(r) = res {
                out.stderr = r.stderr;
                index_lock = r.index_lock;
            }
            GitOpRunResult {
                outcome: out,
                index_lock,
            }
        };
        // 1. Record the pre-update positions.
        let before = match self.read_branch_refs().await {
            Ok(t) => t,
            Err(res) => {
                let msg = first_line(&res.all_lines());
                return fail(op, Some(*res), GitFailure::Other(msg));
            }
        };
        let Some(current) = before.current().cloned() else {
            return fail(
                op,
                None,
                GitFailure::Other("Update Project needs a checked-out branch (HEAD is detached)".into()),
            );
        };
        if current.upstream.is_empty() {
            return fail(
                op,
                None,
                GitFailure::Other(format!(
                    "Branch '{}' has no upstream branch to update from",
                    current.name
                )),
            );
        }
        let mode = match mode {
            Some(m) => m,
            None => {
                let branch_rebase = self
                    .read_config(&format!("branch.{}.rebase", current.name))
                    .await;
                let value = match branch_rebase {
                    Some(v) => Some(v),
                    None => self.read_config("pull.rebase").await,
                };
                UpdateMode::from_pull_rebase(value.as_deref())
            }
        };

        // 2. Save local changes.
        let stashed = if save_changes {
            match self.stash_save("jbgit update project").await {
                Ok(s) => s,
                Err(res) => {
                    let msg = format!(
                        "Could not save local changes: {}",
                        first_line(&res.all_lines())
                    );
                    return fail(op, Some(*res), GitFailure::Other(msg));
                }
            }
        } else {
            false
        };
        let mut stderr: Vec<String> = Vec::new();

        // Restores the stash after an early failure and reports `failure`.
        macro_rules! bail {
            ($res:expr, $failure:expr) => {{
                let res: StepResult = $res;
                let mut out = GitOpOutcome::failed(op.clone(), $failure);
                stderr.extend(res.stderr.iter().cloned());
                if stashed {
                    let pop = self.stash_restore().await;
                    stderr.extend(pop.stderr.iter().cloned());
                    out.stash_kept = !pop.ok();
                }
                out.stderr = stderr;
                return GitOpRunResult {
                    outcome: out,
                    index_lock: res.index_lock,
                };
            }};
        }

        // 3. Fetch.
        let fetch = self
            .step(&CompiledStep::new(["fetch", "--all", "--prune"]), false)
            .await;
        if !fetch.ok() {
            let failure = classify_failure(&fetch.all_lines());
            bail!(fetch, failure);
        }
        stderr.extend(fetch.stderr.iter().cloned());
        let fetched = match self.read_branch_refs().await {
            Ok(t) => t,
            Err(res) => {
                let msg = first_line(&res.all_lines());
                bail!(*res, GitFailure::Other(msg));
            }
        };

        // 4. Current branch.
        if fetched.tip(&current.upstream).is_none() {
            bail!(
                StepResult {
                    exit_code: 1,
                    stdout: Vec::new(),
                    stderr: Vec::new(),
                    index_lock: None,
                },
                GitFailure::UpstreamGone {
                    branch: current.name.clone(),
                    upstream: current.upstream_short.clone(),
                }
            );
        }
        let upstream_arg = if current.upstream_short.is_empty() {
            "@{u}".to_string()
        } else {
            current.upstream_short.clone()
        };
        let integrate = match mode {
            UpdateMode::Merge => CompiledStep::new(vec![
                "merge".to_string(),
                "--no-edit".to_string(),
                upstream_arg,
            ]),
            UpdateMode::Rebase => CompiledStep::new(vec!["rebase".to_string(), upstream_arg]),
        }
        .with_env("GIT_EDITOR", ":");
        let res = self.step(&integrate, false).await;
        stderr.extend(res.stderr.iter().cloned());
        if !res.ok() {
            let failure = classify_failure(&res.all_lines());
            if matches!(failure, GitFailure::Conflicts) {
                let mut out = GitOpOutcome::failed(op, failure);
                out.stderr = stderr;
                out.stash_kept = stashed;
                return GitOpRunResult {
                    outcome: out,
                    index_lock: res.index_lock,
                };
            }
            let res = StepResult {
                stderr: Vec::new(),
                ..res
            };
            bail!(res, failure);
        }

        // 5. Other tracked branches: fast-forward only.
        let mut ranges: Vec<UpdatedRange> = Vec::new();
        let mut skipped: Vec<SkippedBranch> = Vec::new();
        if update_other_branches {
            for head in before.heads.iter().filter(|h| !h.is_head && !h.upstream.is_empty()) {
                let Some(up) = fetched.tip(&head.upstream) else {
                    skipped.push(SkippedBranch {
                        branch: head.name.clone(),
                        reason: format!("upstream '{}' is gone", head.upstream_short),
                    });
                    continue;
                };
                if up == head.oid || self.is_ancestor(up, head.oid).await == Some(true) {
                    continue;
                }
                if self.is_ancestor(head.oid, up).await != Some(true) {
                    skipped.push(SkippedBranch {
                        branch: head.name.clone(),
                        reason: format!("diverged from '{}'", head.upstream_short),
                    });
                    continue;
                }
                let ff = self
                    .step(
                        &CompiledStep::new(vec![
                            "fetch".to_string(),
                            ".".to_string(),
                            format!("{}:refs/heads/{}", head.upstream, head.name),
                        ]),
                        false,
                    )
                    .await;
                if ff.ok() {
                    ranges.push(UpdatedRange {
                        branch: head.name.clone(),
                        before: head.oid,
                        after: up,
                    });
                } else {
                    skipped.push(SkippedBranch {
                        branch: head.name.clone(),
                        reason: first_line(&ff.all_lines()),
                    });
                }
            }
        }
        if let Some(after) = self.rev_parse_head().await
            && after != current.oid
        {
            ranges.insert(
                0,
                UpdatedRange {
                    branch: current.name.clone(),
                    before: current.oid,
                    after,
                },
            );
        }
        let info = self.collect_update_info(ranges, skipped).await;

        // 5b. Update initialized submodules when requested.
        let mut submodule_err: Option<(GitFailure, Option<IndexLockConflict>)> = None;
        if update_submodules {
            let sub = self
                .step(&CompiledStep::new(["submodule", "update", "--recursive"]), false)
                .await;
            stderr.extend(sub.stderr.iter().cloned());
            if !sub.ok() {
                submodule_err = Some((classify_failure(&sub.all_lines()), sub.index_lock));
            }
        }

        // 6. Restore local changes.
        let mut outcome = GitOpOutcome::ok(op);
        outcome.update_info = Some(info);
        let mut index_lock = None;
        if let Some((failure, lock)) = submodule_err {
            outcome.success = false;
            outcome.failure = Some(failure);
            outcome.stash_kept = stashed;
            index_lock = lock;
        } else if stashed {
            let pop = self.stash_restore().await;
            stderr.extend(pop.stderr.iter().cloned());
            if !pop.ok() {
                outcome.success = false;
                outcome.stash_kept = true;
                outcome.failure = Some(GitFailure::Conflicts);
                index_lock = pop.index_lock;
            }
        }
        outcome.stderr = stderr;
        GitOpRunResult {
            outcome,
            index_lock,
        }
    }

    /// `GitPreservingProcess`: stash local changes (including untracked),
    /// run the operation, then pop them back.
    async fn run_smart(&self, inner: GitOp) -> GitOpRunResult {
        let label = inner.label();
        let stashed = match self.stash_save(&format!("jbgit smart: {label}")).await {
            Ok(s) => s,
            Err(saved) => {
                let mut out = GitOpOutcome::failed(
                    GitOp::Smart(Box::new(inner)),
                    GitFailure::Other(format!(
                        "Could not save local changes: {}",
                        first_line(&saved.all_lines())
                    )),
                );
                out.stderr = saved.stderr;
                return GitOpRunResult {
                    outcome: out,
                    index_lock: saved.index_lock,
                };
            }
        };

        let mut res = Box::pin(self.run_plain(inner.clone())).await;
        res.outcome.op = GitOp::Smart(Box::new(inner));
        if !stashed {
            return res;
        }
        if matches!(res.outcome.failure, Some(GitFailure::Conflicts)) || res.outcome.paused_for_edit
        {
            res.outcome.stash_kept = true;
            return res;
        }
        let pop = self.stash_restore().await;
        res.outcome.stderr.extend(pop.stderr.iter().cloned());
        if !pop.ok() {
            res.outcome.stash_kept = true;
            if res.outcome.success {
                res.outcome.success = false;
                res.outcome.failure = Some(GitFailure::Conflicts);
            }
            if res.index_lock.is_none() {
                res.index_lock = pop.index_lock;
            }
        }
        res
    }
}

fn finish(op: GitOp, res: StepResult) -> GitOpRunResult {
    let mut out = GitOpOutcome::ok(op);
    out.stdout = res.stdout.clone();
    out.stderr = res.stderr.clone();
    if !res.ok() {
        out.success = false;
        out.failure = Some(classify_failure(&res.all_lines()));
    }
    GitOpRunResult {
        outcome: out,
        index_lock: res.index_lock,
    }
}

fn first_line(lines: &[String]) -> String {
    lines
        .iter()
        .find(|l| !l.trim().is_empty())
        .cloned()
        .unwrap_or_default()
}

fn sh_quote(path: &Path) -> String {
    let p = path.to_string_lossy().replace('\\', "/");
    format!("'{}'", p.replace('\'', r"'\''"))
}

fn sequence_editor_script(src: &Path) -> String {
    format!(
        "jbgit_seq() {{ while IFS= read -r l || [ -n \"$l\" ]; do printf '%s\\n' \"$l\"; done < {} > \"$1\"; }}; jbgit_seq",
        sh_quote(src)
    )
}

/// Copies ignored / untracked files matching `<src_workdir>/.worktreeinclude`
/// into `dest_workdir` after `git worktree add` succeeds (never overwriting
/// existing files in the newly created working tree).
pub fn apply_worktreeinclude_sync(src_workdir: &std::path::Path, dest_workdir: &std::path::Path) -> Vec<String> {
    let include_file = src_workdir.join(".worktreeinclude");
    let Ok(rules_text) = std::fs::read_to_string(&include_file) else {
        return Vec::new();
    };
    if rules_text.trim().is_empty() || !dest_workdir.is_dir() {
        return Vec::new();
    }

    let repo = git2::Repository::discover(src_workdir).ok();
    let tracked_paths: HashSet<String> = repo
        .as_ref()
        .and_then(|r| r.index().ok())
        .map(|idx| {
            idx.iter()
                .map(|e| String::from_utf8_lossy(&e.path).replace('\\', "/"))
                .collect()
        })
        .unwrap_or_default();

    let mut candidates = Vec::new();
    collect_worktreeinclude_candidates(
        src_workdir,
        src_workdir,
        0,
        &rules_text,
        &tracked_paths,
        &mut candidates,
    );
    let matched = jbgit_core::match_worktreeinclude_paths(&rules_text, &candidates);
    let mut copied = Vec::new();
    for rel in matched {
        if tracked_paths.contains(&rel) {
            continue;
        }
        let src = src_workdir.join(&rel);
        let dst = dest_workdir.join(&rel);
        if !src.is_file() || dst.exists() {
            continue;
        }
        if let Some(parent) = dst.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if std::fs::copy(&src, &dst).is_ok() {
            copied.push(rel);
        }
    }
    copied
}

fn dir_explicitly_mentioned(rules_text: &str, dir_name: &str) -> bool {
    rules_text.lines().any(|l| {
        let t = l.trim();
        !t.is_empty() && !t.starts_with('#') && t.contains(dir_name)
    })
}

fn collect_worktreeinclude_candidates(
    root: &std::path::Path,
    dir: &std::path::Path,
    depth: usize,
    rules_text: &str,
    tracked_paths: &HashSet<String>,
    out: &mut Vec<String>,
) {
    const MAX_DEPTH: usize = 8;
    const MAX_FILES: usize = 10_000;
    const HEAVY_DIRS: [&str; 6] = ["node_modules", "target", ".venv", "venv", "dist", "build"];
    if depth > MAX_DEPTH || out.len() >= MAX_FILES {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if out.len() >= MAX_FILES {
            break;
        }
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str == ".git" || name_str == ".worktreeinclude" {
            continue;
        }
        let path = entry.path();
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if meta.file_type().is_symlink() {
            continue;
        }
        if meta.is_dir() {
            if HEAVY_DIRS.contains(&name_str.as_ref())
                && !dir_explicitly_mentioned(rules_text, &name_str)
            {
                continue;
            }
            collect_worktreeinclude_candidates(
                root,
                &path,
                depth + 1,
                rules_text,
                tracked_paths,
                out,
            );
        } else if meta.is_file()
            && let Ok(rel) = path.strip_prefix(root)
        {
            let rel_str = rel.to_string_lossy().replace('\\', "/");
            if !tracked_paths.contains(&rel_str) {
                out.push(rel_str);
            }
        }
    }
}

fn append_lines(path: &PathBuf, lines: &[String]) -> std::io::Result<()> {
    use std::io::Write;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let existing = std::fs::read(path).unwrap_or_default();
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    if !existing.is_empty() && !existing.ends_with(b"\n") {
        f.write_all(b"\n")?;
    }
    let existing_text = String::from_utf8_lossy(&existing);
    let mut seen: HashSet<String> = existing_text
        .lines()
        .map(|e| e.trim().to_string())
        .collect();
    for l in lines {
        let trimmed = l.trim();
        if trimmed.is_empty() || !seen.insert(trimmed.to_string()) {
            continue;
        }
        f.write_all(l.as_bytes())?;
        f.write_all(b"\n")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::Mutex;

    use jbgit_core::{Oid, OngoingKind, OngoingStep, PushForce, PushTagMode, ResetMode};

    use crate::cli::GitCliOutput;
    use crate::error::EngineError;

    use super::*;

    #[derive(Debug, Clone)]
    struct RecordedCall {
        args: Vec<String>,
        stdin: Option<Vec<u8>>,
        is_read_only: bool,
        env: Vec<(String, String)>,
    }

    struct RecordingCliRunner {
        calls: Mutex<Vec<RecordedCall>>,
        responses: Mutex<VecDeque<GitCliOutput>>,
    }

    impl RecordingCliRunner {
        fn with_responses(responses: Vec<GitCliOutput>) -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
                responses: Mutex::new(responses.into()),
            }
        }

        fn calls(&self) -> Vec<RecordedCall> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl GitCliRunner for RecordingCliRunner {
        fn execute_boxed<'a>(
            &'a self,
            req: CliCommand<'a>,
            _on_event: &'a (dyn Fn(CliStreamEvent) + Send + Sync),
        ) -> Pin<Box<dyn Future<Output = Result<GitCliOutput, EngineError>> + Send + 'a>> {
            self.calls.lock().unwrap().push(RecordedCall {
                args: req.args.to_vec(),
                stdin: req.stdin_payload.map(|b| b.to_vec()),
                is_read_only: req.is_read_only,
                env: req.env.to_vec(),
            });
            let resp = self
                .responses
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or(GitCliOutput {
                    exit_code: 0,
                    duration_ms: 1,
                    stdout_lines: Vec::new(),
                    stderr_lines: Vec::new(),
                    index_lock_conflict: None,
                });
            Box::pin(async move { Ok(resp) })
        }
    }

    fn oid(b: u8) -> Oid {
        Oid::from_bytes([b; 20])
    }

    fn compiled_args(op: &GitOp) -> Vec<Vec<String>> {
        compile_simple_steps(op)
            .expect("simple op")
            .into_iter()
            .map(|s| s.args)
            .collect()
    }

    #[test]
    fn remote_management_ops_compile_to_git_remote() {
        let v = |xs: &[&str]| vec![xs.iter().map(ToString::to_string).collect::<Vec<_>>()];
        assert_eq!(
            compiled_args(&GitOp::AddRemote {
                name: "fork".into(),
                url: "https://u:secret@h/x.git".into(),
            }),
            v(&["remote", "add", "--", "fork", "https://u:secret@h/x.git"])
        );
        assert_eq!(
            compiled_args(&GitOp::RenameRemote {
                old: "fork".into(),
                new: "mine".into(),
            }),
            v(&["remote", "rename", "--", "fork", "mine"])
        );
        assert_eq!(
            compiled_args(&GitOp::RemoveRemote { name: "mine".into() }),
            v(&["remote", "remove", "--", "mine"])
        );
        assert_eq!(
            compiled_args(&GitOp::SetRemoteUrl {
                name: "o".into(),
                url: "u".into(),
                push: false,
            }),
            v(&["remote", "set-url", "--", "o", "u"])
        );
        assert_eq!(
            compiled_args(&GitOp::SetRemoteUrl {
                name: "o".into(),
                url: "u".into(),
                push: true,
            }),
            v(&["remote", "set-url", "--push", "--", "o", "u"])
        );
        assert_eq!(
            compiled_args(&GitOp::FetchRemote { name: "fork".into() }),
            v(&["fetch", "--prune", "fork"])
        );
        assert_eq!(compiled_args(&GitOp::Fetch), v(&["fetch", "--all", "--prune"]));
    }

    #[test]
    fn worktree_ops_compile_to_git_worktree() {
        let v = |xs: &[&str]| vec![xs.iter().map(ToString::to_string).collect::<Vec<_>>()];
        let oid = Oid::from_bytes([0xab; 20]);
        assert_eq!(
            compiled_args(&GitOp::AddWorktree {
                path: PathBuf::from("/w/x"),
                branch: WorktreeBranch::Existing("feature/x".into()),
                force: false,
            }),
            v(&["worktree", "add", "--", "/w/x", "feature/x"])
        );
        assert_eq!(
            compiled_args(&GitOp::AddWorktree {
                path: PathBuf::from("/w/y"),
                branch: WorktreeBranch::New {
                    name: "y".into(),
                    start: "main".into(),
                },
                force: false,
            }),
            v(&["worktree", "add", "-b", "y", "--", "/w/y", "main"])
        );
        assert_eq!(
            compiled_args(&GitOp::AddWorktree {
                path: PathBuf::from("/w/z"),
                branch: WorktreeBranch::Detached(oid),
                force: false,
            }),
            v(&["worktree", "add", "--detach", "--", "/w/z", &oid.to_string()])
        );
        assert_eq!(
            compiled_args(&GitOp::AddWorktree {
                path: PathBuf::from("/w/f"),
                branch: WorktreeBranch::Existing("feature/x".into()),
                force: true,
            }),
            v(&["worktree", "add", "--force", "--", "/w/f", "feature/x"])
        );
        assert_eq!(
            compiled_args(&GitOp::RemoveWorktree {
                path: PathBuf::from("/w/x"),
                force: true,
            }),
            v(&["worktree", "remove", "--force", "--", "/w/x"])
        );
        assert_eq!(
            compiled_args(&GitOp::LockWorktree {
                path: PathBuf::from("/w/x"),
                reason: Some(" usb ".into()),
            }),
            v(&["worktree", "lock", "--reason", "usb", "--", "/w/x"])
        );
        assert_eq!(
            compiled_args(&GitOp::LockWorktree {
                path: PathBuf::from("/w/x"),
                reason: Some("  ".into()),
            }),
            v(&["worktree", "lock", "--", "/w/x"])
        );
        assert_eq!(
            compiled_args(&GitOp::UnlockWorktree {
                path: PathBuf::from("/w/x"),
            }),
            v(&["worktree", "unlock", "--", "/w/x"])
        );
        assert_eq!(
            compiled_args(&GitOp::PruneWorktrees),
            v(&["worktree", "prune", "--verbose"])
        );
    }

    #[tokio::test]
    async fn askpass_env_reaches_network_steps_only() {
        let env = AskpassEnv::new(PathBuf::from("/bin/jbgit"), "pipe", "tok");
        let runner = RecordingCliRunner::with_responses(Vec::new());
        let ids = AtomicU64::new(1);
        let seq = GitOp::Sequence(vec![
            GitOp::AddRemote {
                name: "fork".into(),
                url: "u".into(),
            },
            GitOp::FetchRemote { name: "fork".into() },
            GitOp::Pull {
                options: PullOptions::default(),
            },
            GitOp::Checkout {
                target: "main".into(),
                detach: false,
            },
        ]);
        let res = run_git_op(&runner, &dummy_loc(), &ids, seq, Some(&env), |_| {}).await;
        assert!(res.outcome.success);
        let calls = runner.calls();
        let has = |c: &RecordedCall, k: &str| c.env.iter().any(|(n, _)| n == k);
        assert_eq!(calls.len(), 4);
        assert!(!has(&calls[0], "GIT_ASKPASS"), "remote add is local");
        assert!(has(&calls[1], "GIT_ASKPASS") && has(&calls[1], "JBGIT_ASKPASS_TOKEN"));
        assert!(has(&calls[1], "SSH_ASKPASS_REQUIRE"));
        assert!(has(&calls[2], "GIT_ASKPASS"), "pull keeps GIT_EDITOR and adds askpass");
        assert!(has(&calls[2], "GIT_EDITOR"));
        assert!(calls[3].env.is_empty(), "checkout is local");

        let plain = RecordingCliRunner::with_responses(Vec::new());
        let _ = run_git_op(&plain, &dummy_loc(), &ids, GitOp::Fetch, None, |_| {}).await;
        assert!(plain.calls()[0].env.is_empty());
    }

    fn dummy_loc() -> RepoLocation {
        RepoLocation {
            workdir: PathBuf::from("/repo"),
            git_dir: PathBuf::from("/repo/.git"),
            common_dir: PathBuf::from("/repo/.git"),
            is_bare: false,
            repo_name: "repo".into(),
        }
    }

    #[test]
    fn path_bearing_ops_switch_to_pathspec_from_file_above_threshold() {
        let many: Vec<String> = (0..55).map(|i| format!("dir/file {i}.rs")).collect();

        for op in [
            GitOp::CheckoutPaths {
                rev: "HEAD~1".into(),
                paths: many.clone(),
            },
            GitOp::UnresolveConflict {
                paths: many.clone(),
            },
            GitOp::IntentToAdd {
                paths: many.clone(),
            },
            GitOp::StashPush {
                message: Some("wip".into()),
                include_untracked: true,
                keep_index: false,
                paths: many.clone(),
            },
        ] {
            let steps = compile_simple_steps(&op).expect("compiled steps");
            assert!(!steps.is_empty());
            for step in &steps {
                assert!(
                    step.args.contains(&"--pathspec-from-file=-".to_string()),
                    "op {op:?} step {:?} missed --pathspec-from-file=-",
                    step.args
                );
                assert!(step.args.contains(&"--pathspec-file-nul".to_string()));
                let payload = step.stdin.as_ref().expect("nul-delimited stdin");
                assert_eq!(payload.iter().filter(|&&b| b == 0).count(), 55);
            }
        }

        let accept_steps = compile_accept_side_steps(&many, &many, true);
        assert_eq!(accept_steps.len(), 3);
        assert_eq!(accept_steps[0].args[0], "checkout");
        assert_eq!(accept_steps[1].args[0], "add");
        assert_eq!(accept_steps[2].args[0], "rm");
        for step in &accept_steps {
            assert!(step.args.contains(&"--pathspec-from-file=-".to_string()));
            assert!(step.args.contains(&"--pathspec-file-nul".to_string()));
            let payload = step.stdin.as_ref().expect("nul-delimited stdin");
            assert_eq!(payload.iter().filter(|&&b| b == 0).count(), 55);
        }
    }

    #[test]
    fn compiles_patch_sources_and_common_ops() {
        let push = GitOp::Push {
            branch: "feat".into(),
            remote: "origin".into(),
            remote_branch: "feat".into(),
            set_upstream: true,
            force: PushForce::None,
            tags: PushTagMode::None,
            up_to: Some(oid(7)),
            no_verify: false,
            expected_remote_oid: None,
        };
        assert_eq!(
            compile_simple_steps(&push).unwrap()[0].args,
            vec![
                "push".to_string(),
                "--porcelain".into(),
                "--set-upstream".into(),
                "origin".into(),
                format!("{}:refs/heads/feat", oid(7)),
            ]
        );

        let push_custom = GitOp::Push {
            branch: "local-feat".into(),
            remote: "upstream".into(),
            remote_branch: "team/feat".into(),
            set_upstream: false,
            force: PushForce::WithLease,
            tags: PushTagMode::Follow,
            up_to: None,
            no_verify: false,
            expected_remote_oid: None,
        };
        assert_eq!(
            compile_simple_steps(&push_custom).unwrap()[0].args,
            vec![
                "push",
                "--porcelain",
                "--force-with-lease",
                "--follow-tags",
                "upstream",
                "refs/heads/local-feat:refs/heads/team/feat",
            ]
        );

        let push_lease_pinned = GitOp::Push {
            branch: "local-feat".into(),
            remote: "origin".into(),
            remote_branch: "feat".into(),
            set_upstream: false,
            force: PushForce::WithLease,
            tags: PushTagMode::None,
            up_to: None,
            no_verify: true,
            expected_remote_oid: Some(oid(5)),
        };
        assert_eq!(
            compile_simple_steps(&push_lease_pinned).unwrap()[0].args,
            vec![
                "push".to_string(),
                "--porcelain".into(),
                "--no-verify".into(),
                format!("--force-with-lease=refs/heads/feat:{}", oid(5)),
                "origin".into(),
                "refs/heads/local-feat:refs/heads/feat".into(),
            ]
        );

        let push_detached = GitOp::Push {
            branch: "HEAD".into(),
            remote: "origin".into(),
            remote_branch: "rescue".into(),
            set_upstream: true, // Ignored for detached HEAD
            force: PushForce::None,
            tags: PushTagMode::None,
            up_to: Some(oid(8)),
            no_verify: false,
            expected_remote_oid: None,
        };
        assert_eq!(
            compile_simple_steps(&push_detached).unwrap()[0].args,
            vec![
                "push".to_string(),
                "--porcelain".into(),
                "origin".into(),
                format!("{}:refs/heads/rescue", oid(8)),
            ]
        );

        let push_force_all = GitOp::Push {
            branch: "dev".into(),
            remote: "origin".into(),
            remote_branch: String::new(),
            set_upstream: false,
            force: PushForce::Force,
            tags: PushTagMode::All,
            up_to: None,
            no_verify: false,
            expected_remote_oid: None,
        };
        assert_eq!(
            compile_simple_steps(&push_force_all).unwrap()[0].args,
            vec![
                "push",
                "--porcelain",
                "--force",
                "--tags",
                "origin",
                "refs/heads/dev:refs/heads/dev",
            ]
        );

        let set_id = GitOp::SetIdentity {
            name: "Alice".into(),
            email: "alice@example.com".into(),
            global: true,
        };
        let id_steps = compile_simple_steps(&set_id).unwrap();
        assert_eq!(id_steps.len(), 2);
        assert_eq!(
            id_steps[0].args,
            vec!["config", "--global", "--", "user.name", "Alice"]
        );
        assert_eq!(
            id_steps[1].args,
            vec!["config", "--global", "--", "user.email", "alice@example.com"]
        );

        let set_up = GitOp::SetUpstream {
            branch: "feat".into(),
            upstream: "origin/feat".into(),
        };
        assert_eq!(
            compile_simple_steps(&set_up).unwrap()[0].args,
            vec!["branch", "--set-upstream-to=origin/feat", "--", "feat"]
        );

        let tag_light = GitOp::CreateTag {
            name: "-tricky".into(),
            target: oid(1),
            message: None,
            force: true,
            sign: false,
        };
        assert_eq!(
            compile_simple_steps(&tag_light).unwrap()[0].args,
            vec!["tag", "-f", "--", "-tricky", &oid(1).to_string()]
        );

        let tag_ann = GitOp::CreateTag {
            name: "v1.0".into(),
            target: oid(1),
            message: Some("release".into()),
            force: false,
            sign: false,
        };
        assert_eq!(
            compile_simple_steps(&tag_ann).unwrap()[0].args,
            vec!["tag", "-a", "-m", "release", "--", "v1.0", &oid(1).to_string()]
        );

        let tag_signed = GitOp::CreateTag {
            name: "v2.0".into(),
            target: oid(1),
            message: None,
            force: false,
            sign: true,
        };
        assert_eq!(
            compile_simple_steps(&tag_signed).unwrap()[0].args,
            vec!["tag", "-s", "-m", "v2.0", "--", "v2.0", &oid(1).to_string()]
        );

        let bisect_start = GitOp::BisectStart {
            bad: Some("HEAD".into()),
            good: vec!["abc1234".into()],
        };
        assert_eq!(
            compile_simple_steps(&bisect_start).unwrap()[0].args,
            vec!["bisect", "start", "HEAD", "abc1234"]
        );
        assert_eq!(
            compile_simple_steps(&GitOp::BisectMark {
                good: true,
                rev: None,
            })
            .unwrap()[0]
                .args,
            vec!["bisect", "good"]
        );
        assert_eq!(
            compile_simple_steps(&GitOp::BisectMark {
                good: false,
                rev: Some("deadbeef".into()),
            })
            .unwrap()[0]
                .args,
            vec!["bisect", "bad", "deadbeef"]
        );
        assert_eq!(
            compile_simple_steps(&GitOp::BisectSkip).unwrap()[0].args,
            vec!["bisect", "skip"]
        );
        assert_eq!(
            compile_simple_steps(&GitOp::BisectReset).unwrap()[0].args,
            vec!["bisect", "reset"]
        );

        let tag_del = GitOp::DeleteTag {
            name: "v1.0".into(),
            tip: oid(1),
        };
        assert_eq!(
            compile_simple_steps(&tag_del).unwrap()[0].args,
            vec!["tag", "-d", "--", "v1.0"]
        );

        let tag_restore = GitOp::RestoreTag {
            name: "v1.0".into(),
            oid: oid(1),
        };
        assert_eq!(
            compile_simple_steps(&tag_restore).unwrap()[0].args,
            vec!["tag", "--", "v1.0", &oid(1).to_string()]
        );

        let tag_del_remote = GitOp::DeleteRemoteTag {
            remote: "origin".into(),
            name: "v1.0".into(),
        };
        assert_eq!(
            compile_simple_steps(&tag_del_remote).unwrap()[0].args,
            vec!["push", "origin", "--delete", "refs/tags/v1.0"]
        );

        let tag_push = GitOp::PushTag {
            remote: "origin".into(),
            name: "v1.0".into(),
            force: true,
        };
        assert_eq!(
            compile_simple_steps(&tag_push).unwrap()[0].args,
            vec![
                "push",
                "--force",
                "origin",
                "refs/tags/v1.0:refs/tags/v1.0",
            ]
        );

        let patch_commits = PatchSource::Commits {
            oids: vec![oid(2), oid(1)],
            base: None,
        };
        let fwd_steps = compile_patch_source(&patch_commits, false, true);
        assert_eq!(fwd_steps.len(), 2);
        assert_eq!(
            fwd_steps[0].args,
            vec!["format-patch", "--stdout", "-1", &oid(1).to_string()]
        );
        assert_eq!(
            fwd_steps[1].args,
            vec!["format-patch", "--stdout", "-1", &oid(2).to_string()]
        );
        let rev_steps = compile_patch_source(&patch_commits, true, true);
        assert_eq!(rev_steps.len(), 2);
        assert_eq!(
            rev_steps[0].args,
            vec!["show", "--format=", "--patch", "-R", &oid(2).to_string()]
        );
        assert_eq!(
            rev_steps[1].args,
            vec!["show", "--format=", "--patch", "-R", &oid(1).to_string()]
        );

        let patch_file = PatchSource::CommitFile {
            oid: oid(3),
            path: "src/lib.rs".into(),
        };
        assert_eq!(
            compile_patch_source(&patch_file, false, true)[0].args,
            vec![
                "show",
                "--format=",
                "--patch",
                &oid(3).to_string(),
                "--",
                "src/lib.rs"
            ]
        );
        assert_eq!(
            compile_patch_source(&patch_file, true, true)[0].args,
            vec![
                "show",
                "--format=",
                "--patch",
                "-R",
                &oid(3).to_string(),
                "--",
                "src/lib.rs"
            ]
        );

        let patch_stage = PatchSource::StagePaths {
            staged: vec!["both.rs".into(), "staged.rs".into()],
            unstaged: vec!["both.rs".into(), "work.rs".into()],
            untracked: vec!["new.rs".into()],
        };
        let stage_steps = compile_patch_source(&patch_stage, false, true);
        assert_eq!(stage_steps.len(), 3);
        assert_eq!(stage_steps[0].args, vec!["diff", "HEAD", "--", "both.rs"]);
        assert_eq!(
            stage_steps[1].args,
            vec!["diff", "--cached", "--", "staged.rs"]
        );
        assert_eq!(stage_steps[2].args, vec!["diff", "--", "work.rs"]);

        let restore_wt = GitOp::RestorePaths {
            rev: oid(4).to_string(),
            paths: vec!["a.rs".into(), "b.rs".into()],
            staged: false,
            worktree: true,
        };
        assert_eq!(
            compile_simple_steps(&restore_wt).unwrap()[0].args,
            vec![
                "restore",
                &format!("--source={}", oid(4)),
                "--worktree",
                "--",
                "a.rs",
                "b.rs",
            ]
        );
        let restore_both = GitOp::RestorePaths {
            rev: oid(4).to_string(),
            paths: vec!["a.rs".into()],
            staged: true,
            worktree: true,
        };
        assert_eq!(
            compile_simple_steps(&restore_both).unwrap()[0].args,
            vec![
                "restore",
                &format!("--source={}", oid(4)),
                "--staged",
                "--worktree",
                "--",
                "a.rs",
            ]
        );

        let ongoing = GitOp::Ongoing {
            kind: OngoingKind::Rebase,
            step: OngoingStep::Continue,
        };
        let steps = compile_simple_steps(&ongoing).unwrap();
        assert_eq!(steps[0].args, vec!["rebase", "--continue"]);
        assert_eq!(steps[0].env, vec![("GIT_EDITOR".into(), ":".into())]);

        let reset = GitOp::Reset {
            mode: ResetMode::Keep,
            target: oid(5),
        };
        assert_eq!(
            compile_simple_steps(&reset).unwrap()[0].args,
            vec!["reset", "--keep", &oid(5).to_string()]
        );
    }

    #[tokio::test]
    async fn recording_runner_classifies_push_rejected_and_propagates_index_lock() {
        let runner = RecordingCliRunner::with_responses(vec![GitCliOutput {
            exit_code: 1,
            duration_ms: 5,
            stdout_lines: Vec::new(),
            stderr_lines: vec!["! [rejected] main -> main (non-fast-forward)".into()],
            index_lock_conflict: None,
        }]);
        let ids = AtomicU64::new(1);
        let res = run_git_op(
            &runner,
            &dummy_loc(),
            &ids,
            GitOp::Push {
                branch: "main".into(),
                remote: "origin".into(),
                remote_branch: "main".into(),
                set_upstream: false,
                force: PushForce::None,
                tags: PushTagMode::None,
                up_to: None,
                no_verify: false,
                expected_remote_oid: None,
            },
            None,
            |_| {},
        )
        .await;
        assert!(!res.outcome.success);
        assert_eq!(res.outcome.failure, Some(GitFailure::PushRejected));
        assert!(res.index_lock.is_none());
        let calls = runner.calls();
        assert!(!calls[0].is_read_only);
        assert_eq!(
            calls[0].args,
            vec!["push", "--porcelain", "origin", "refs/heads/main:refs/heads/main"]
        );
        assert!(calls[0].stdin.is_none());
        assert!(calls[0].env.is_empty());

        let lock_runner = RecordingCliRunner::with_responses(vec![GitCliOutput {
            exit_code: 128,
            duration_ms: 2,
            stdout_lines: Vec::new(),
            stderr_lines: vec!["fatal: Unable to create '.git/index.lock': File exists.".into()],
            index_lock_conflict: Some((
                PathBuf::from("/repo/.git/index.lock"),
                "index.lock exists".into(),
            )),
        }]);
        let lock_res = run_git_op(
            &lock_runner,
            &dummy_loc(),
            &ids,
            GitOp::Checkout {
                target: "main".into(),
                detach: false,
            },
            None,
            |_| {},
        )
        .await;
        assert!(!lock_res.outcome.success);
        let conflict = lock_res
            .index_lock
            .expect("index.lock conflict must be surfaced");
        assert_eq!(conflict.retry_args, vec!["checkout", "main"]);
        assert_eq!(conflict.lock_path, PathBuf::from("/repo/.git/index.lock"));
    }

    #[tokio::test]
    async fn sequence_stops_on_first_failure_preserving_inner_op_and_failure() {
        let runner = RecordingCliRunner::with_responses(vec![
            GitCliOutput {
                exit_code: 0,
                duration_ms: 2,
                stdout_lines: Vec::new(),
                stderr_lines: Vec::new(),
                index_lock_conflict: None,
            },
            GitCliOutput {
                exit_code: 1,
                duration_ms: 3,
                stdout_lines: Vec::new(),
                stderr_lines: vec!["CONFLICT (content): Merge conflict in src/lib.rs".into()],
                index_lock_conflict: None,
            },
        ]);
        let ids = AtomicU64::new(1);
        let seq = GitOp::Sequence(vec![
            GitOp::Fetch,
            GitOp::Rebase {
                options: RebaseOptions::onto("origin/main", None),
            },
            GitOp::Push {
                branch: "main".into(),
                remote: "origin".into(),
                remote_branch: "main".into(),
                set_upstream: false,
                force: PushForce::None,
                tags: PushTagMode::None,
                up_to: None,
                no_verify: false,
                expected_remote_oid: None,
            },
        ]);
        let res = run_git_op(&runner, &dummy_loc(), &ids, seq, None, |_| {}).await;
        assert!(!res.outcome.success);
        assert_eq!(res.outcome.failure, Some(GitFailure::Conflicts));
        assert!(matches!(res.outcome.op, GitOp::Rebase { .. }));
        assert_eq!(runner.calls().len(), 2);
    }

    #[test]
    fn merge_compiles_default_regression_and_every_option() {
        let plain = GitOp::Merge {
            rev: "feat".into(),
            options: MergeOptions::default(),
        };
        let steps = compile_simple_steps(&plain).unwrap();
        assert_eq!(steps[0].args, vec!["merge", "--no-edit", "feat"]);
        assert!(steps[0].stdin.is_none());
        assert_eq!(steps[0].env, vec![("GIT_EDITOR".into(), ":".into())]);

        let full = GitOp::Merge {
            rev: "origin/feat".into(),
            options: MergeOptions {
                ff: jbgit_core::FastForward::NoFf,
                squash: false,
                no_commit: false,
                message: Some("Merge feat\n\nwith body line\n".into()),
                strategy: Some("ours".into()),
                strategy_option: Some("theirs".into()),
                allow_unrelated: true,
            },
        };
        let step = &compile_simple_steps(&full).unwrap()[0];
        assert_eq!(
            step.args,
            vec![
                "merge",
                "--no-ff",
                "-s",
                "ours",
                "-X",
                "theirs",
                "--allow-unrelated-histories",
                "-m",
                "Merge feat\n\nwith body line",
                "origin/feat",
            ]
        );
        assert!(step.stdin.is_none());

        let squash = GitOp::Merge {
            rev: "feat".into(),
            options: MergeOptions {
                squash: true,
                ..Default::default()
            },
        };
        assert_eq!(
            compile_simple_steps(&squash).unwrap()[0].args,
            vec!["merge", "--squash", "--no-edit", "feat"]
        );

        let ff_only_no_commit = GitOp::Merge {
            rev: "feat".into(),
            options: MergeOptions {
                ff: jbgit_core::FastForward::FfOnly,
                ..Default::default()
            },
        };
        assert_eq!(
            compile_simple_steps(&ff_only_no_commit).unwrap()[0].args,
            vec!["merge", "--ff-only", "--no-edit", "feat"]
        );

        let no_commit = GitOp::Merge {
            rev: "feat".into(),
            options: MergeOptions {
                no_commit: true,
                ff: jbgit_core::FastForward::NoFf,
                ..Default::default()
            },
        };
        assert_eq!(
            compile_simple_steps(&no_commit).unwrap()[0].args,
            vec!["merge", "--no-ff", "--no-commit", "--no-edit", "feat"]
        );
        assert!(
            compile_simple_steps(&GitOp::UpdateProject {
                mode: None,
                save_changes: true,
                update_other_branches: true,
                update_submodules: false,
            })
            .is_none()
        );
    }

    #[test]
    fn branch_ref_table_and_prepared_message_parsing() {
        let lines = vec![
            format!(
                "refs/heads/main\t{}\trefs/remotes/origin/main\torigin/main\t*",
                oid(1)
            ),
            format!("refs/heads/topic\t{}\t\t\t ", oid(2)),
            format!("refs/remotes/origin/main\t{}\t\t\t", oid(3)),
            "garbage line".to_string(),
        ];
        let table = parse_branch_refs(&lines);
        assert_eq!(table.heads.len(), 2);
        let cur = table.current().unwrap();
        assert_eq!(cur.name, "main");
        assert_eq!(cur.upstream, "refs/remotes/origin/main");
        assert_eq!(cur.upstream_short, "origin/main");
        assert_eq!(table.tip("refs/remotes/origin/main"), Some(oid(3)));
        assert!(table.heads[1].upstream.is_empty());
        assert!(!table.heads[1].is_head);

        assert_eq!(
            clean_prepared_message("Squashed commit of the following:\n\ncommit abc\n# Conflicts:\n#\tx.rs\n\n"),
            Some("Squashed commit of the following:\n\ncommit abc".to_string())
        );
        assert_eq!(clean_prepared_message("# only comments\n"), None);
    }

    #[tokio::test]
    async fn update_project_bails_without_upstream_or_on_detached_head() {
        let detached = RecordingCliRunner::with_responses(vec![GitCliOutput {
            exit_code: 0,
            duration_ms: 1,
            stdout_lines: vec![format!("refs/heads/main\t{}\t\t\t ", oid(1))],
            stderr_lines: Vec::new(),
            index_lock_conflict: None,
        }]);
        let ids = AtomicU64::new(1);
        let op = GitOp::UpdateProject {
            mode: Some(jbgit_core::UpdateMode::Merge),
            save_changes: true,
            update_other_branches: true,
            update_submodules: false,
        };
        let res = run_git_op(&detached, &dummy_loc(), &ids, op.clone(), None, |_| {}).await;
        assert!(!res.outcome.success);
        assert!(matches!(res.outcome.failure, Some(GitFailure::Other(ref m)) if m.contains("detached")));
        assert_eq!(detached.calls().len(), 1, "nothing is stashed or fetched");

        let untracked = RecordingCliRunner::with_responses(vec![GitCliOutput {
            exit_code: 0,
            duration_ms: 1,
            stdout_lines: vec![format!("refs/heads/main\t{}\t\t\t*", oid(1))],
            stderr_lines: Vec::new(),
            index_lock_conflict: None,
        }]);
        let res = run_git_op(&untracked, &dummy_loc(), &ids, op, None, |_| {}).await;
        assert!(matches!(res.outcome.failure, Some(GitFailure::Other(ref m)) if m.contains("no upstream")));
        let calls = untracked.calls();
        assert_eq!(calls.len(), 1);
        assert!(calls[0].is_read_only);
        assert_eq!(calls[0].args[0], "for-each-ref");
    }

    #[tokio::test]
    async fn askpass_env_is_passed_to_submodule_update_and_omitted_for_local_ops() {
        let askpass = AskpassEnv::new(PathBuf::from("/usr/bin/jbgit"), "pipe://test", "tok");
        let ids = AtomicU64::new(1);

        let runner = RecordingCliRunner::with_responses(Vec::new());
        let res = run_git_op(
            &runner,
            &dummy_loc(),
            &ids,
            GitOp::SubmoduleUpdate {
                path: Some("vendor/lib".into()),
            },
            Some(&askpass),
            |_| {},
        )
        .await;
        assert!(res.outcome.success);
        let calls = runner.calls();
        assert_eq!(calls.len(), 1);
        assert!(
            calls[0].env.iter().any(|(k, _)| k == "GIT_ASKPASS"),
            "SubmoduleUpdate must receive GIT_ASKPASS: {:?}",
            calls[0].env
        );

        let local_runner = RecordingCliRunner::with_responses(Vec::new());
        let local_res = run_git_op(
            &local_runner,
            &dummy_loc(),
            &ids,
            GitOp::CreateBranch {
                name: "topic".into(),
                start: "HEAD".into(),
                force: false,
                track: false,
            },
            Some(&askpass),
            |_| {},
        )
        .await;
        assert!(local_res.outcome.success);
        let local_calls = local_runner.calls();
        assert_eq!(local_calls.len(), 1);
        assert!(
            local_calls[0].env.is_empty(),
            "local ops must not receive askpass env: {:?}",
            local_calls[0].env
        );
    }

    #[test]
    fn pull_rebase_reword_and_fixup_compile_all_options() {
        // Default Pull keeps legacy `pull --no-edit`.
        let default_pull = compile_simple_steps(&GitOp::Pull {
            options: PullOptions::default(),
        })
        .unwrap();
        assert_eq!(default_pull[0].args, vec!["pull", "--no-edit"]);
        assert_eq!(default_pull[0].env, vec![("GIT_EDITOR".into(), ":".into())]);

        // Pull with rebase + no-verify + remote + branch.
        let rebase_pull = compile_simple_steps(&GitOp::Pull {
            options: PullOptions {
                remote: Some("origin".into()),
                branch: Some("main".into()),
                rebase: true,
                no_verify: true,
                ..Default::default()
            },
        })
        .unwrap();
        assert_eq!(
            rebase_pull[0].args,
            vec!["pull", "--rebase", "--no-verify", "origin", "main"]
        );

        // Pull with --ff-only / --squash / --no-commit.
        let ff_pull = compile_simple_steps(&GitOp::Pull {
            options: PullOptions {
                ff: jbgit_core::FastForward::FfOnly,
                ..Default::default()
            },
        })
        .unwrap();
        assert_eq!(ff_pull[0].args, vec!["pull", "--ff-only", "--no-edit"]);

        let squash_pull = compile_simple_steps(&GitOp::Pull {
            options: PullOptions {
                squash: true,
                no_commit: true,
                remote: Some("upstream".into()),
                branch: Some("dev".into()),
                ..Default::default()
            },
        })
        .unwrap();
        assert_eq!(
            squash_pull[0].args,
            vec!["pull", "--squash", "--no-commit", "upstream", "dev"]
        );

        // Rebase simple, --onto, --root, and all flags.
        let plain_rebase = compile_simple_steps(&GitOp::Rebase {
            options: RebaseOptions::onto("main", Some("feat".into())),
        })
        .unwrap();
        assert_eq!(plain_rebase[0].args, vec!["rebase", "main", "feat"]);
        assert_eq!(plain_rebase[0].env, vec![("GIT_EDITOR".into(), ":".into())]);

        let onto_rebase = compile_simple_steps(&GitOp::Rebase {
            options: RebaseOptions {
                onto: "main".into(),
                upstream: Some("old-base".into()),
                branch: Some("topic".into()),
                interactive: false,
                rebase_merges: true,
                update_refs: true,
                keep_empty: true,
                autosquash: true,
                root: false,
            },
        })
        .unwrap();
        assert_eq!(
            onto_rebase[0].args,
            vec![
                "rebase",
                "-i",
                "--rebase-merges",
                "--update-refs",
                "--keep-empty",
                "--autosquash",
                "--onto",
                "main",
                "old-base",
                "topic",
            ]
        );
        assert!(onto_rebase[0].env.iter().any(|(k, v)| k == "GIT_SEQUENCE_EDITOR" && v == ":"));

        let root_rebase = compile_simple_steps(&GitOp::Rebase {
            options: RebaseOptions {
                onto: String::new(),
                root: true,
                keep_empty: true,
                ..Default::default()
            },
        })
        .unwrap();
        assert_eq!(root_rebase[0].args, vec!["rebase", "--keep-empty", "--root"]);

        // RewordHead respects hooks by default, passes --no-verify only when requested.
        let reword_hooks = compile_simple_steps(&GitOp::RewordHead {
            message: "new msg".into(),
            no_verify: false,
        })
        .unwrap();
        assert_eq!(
            reword_hooks[0].args,
            vec!["commit", "--amend", "--only", "--allow-empty", "-F", "-"]
        );
        let reword_no_verify = compile_simple_steps(&GitOp::RewordHead {
            message: "new msg".into(),
            no_verify: true,
        })
        .unwrap();
        assert_eq!(
            reword_no_verify[0].args,
            vec![
                "commit",
                "--amend",
                "--only",
                "--allow-empty",
                "--no-verify",
                "-F",
                "-"
            ]
        );

        // FixupCommit split: Fixup!, Squash!, and Fixup & Autosquash.
        let fixup_only = compile_simple_steps(&GitOp::FixupCommit {
            target: oid(1),
            kind: FixupKind::Fixup,
            base: Some(oid(2)),
            autosquash: false,
        })
        .unwrap();
        assert_eq!(fixup_only.len(), 1);
        assert_eq!(fixup_only[0].args, vec!["commit", &format!("--fixup={}", oid(1))]);

        let squash_only = compile_simple_steps(&GitOp::FixupCommit {
            target: oid(1),
            kind: FixupKind::Squash,
            base: Some(oid(2)),
            autosquash: false,
        })
        .unwrap();
        assert_eq!(squash_only.len(), 1);
        assert_eq!(
            squash_only[0].args,
            vec!["commit", "--no-edit", &format!("--squash={}", oid(1))]
        );

        let fixup_auto = compile_simple_steps(&GitOp::FixupCommit {
            target: oid(1),
            kind: FixupKind::Fixup,
            base: Some(oid(2)),
            autosquash: true,
        })
        .unwrap();
        assert_eq!(fixup_auto.len(), 2);
        assert_eq!(
            fixup_auto[1].args,
            vec!["rebase", "-i", "--autosquash", "--autostash", &oid(2).to_string()]
        );
    }

    #[test]
    fn issue_06_ops_compile_upstream_cherry_pick_revert_and_stash_flags() {
        let s = compile_simple_steps(&GitOp::SetUpstream {
            branch: "feat".into(),
            upstream: "origin/feat".into(),
        })
        .unwrap();
        assert_eq!(
            s[0].args,
            vec!["branch", "--set-upstream-to=origin/feat", "--", "feat"]
        );

        let s = compile_simple_steps(&GitOp::UnsetUpstream {
            branch: "feat".into(),
        })
        .unwrap();
        assert_eq!(s[0].args, vec!["branch", "--unset-upstream", "--", "feat"]);

        let s = compile_simple_steps(&GitOp::CheckoutNewBranch {
            name: "feat".into(),
            start: "origin/feat".into(),
            track: true,
            force: true,
        })
        .unwrap();
        assert_eq!(s[0].args, vec!["checkout", "-B", "feat", "--track", "origin/feat"]);

        let s = compile_simple_steps(&GitOp::CreateBranch {
            name: "feat".into(),
            start: "origin/feat".into(),
            force: true,
            track: true,
        })
        .unwrap();
        assert_eq!(s[0].args, vec!["branch", "-f", "--track", "--", "feat", "origin/feat"]);

        let target = oid(7);
        let s = compile_simple_steps(&GitOp::CherryPick {
            oids: vec![target],
            no_commit: true,
            record_origin: true,
            mainline: Some(1),
        })
        .unwrap();
        assert_eq!(s[0].args, vec!["cherry-pick", "-n", "-m", "1", &target.to_string()]);

        let s = compile_simple_steps(&GitOp::CherryPick {
            oids: vec![target],
            no_commit: false,
            record_origin: true,
            mainline: Some(2),
        })
        .unwrap();
        assert_eq!(s[0].args, vec!["cherry-pick", "-x", "-m", "2", &target.to_string()]);

        let s = compile_simple_steps(&GitOp::Revert {
            oids: vec![target],
            no_commit: true,
            mainline: Some(1),
        })
        .unwrap();
        assert_eq!(s[0].args, vec!["revert", "-n", "-m", "1", &target.to_string()]);

        let s = compile_simple_steps(&GitOp::Revert {
            oids: vec![target],
            no_commit: false,
            mainline: Some(2),
        })
        .unwrap();
        assert_eq!(s[0].args, vec!["revert", "--no-edit", "-m", "2", &target.to_string()]);

        let s = compile_simple_steps(&GitOp::StashPush {
            message: Some("save".into()),
            include_untracked: true,
            keep_index: true,
            paths: vec!["a.txt".into()],
        })
        .unwrap();
        assert_eq!(
            s[0].args,
            vec!["stash", "push", "--include-untracked", "--keep-index", "-m", "save", "--", "a.txt"]
        );

        let s = compile_simple_steps(&GitOp::StashApply {
            index: 1,
            pop: false,
            reinstate_index: true,
        })
        .unwrap();
        assert_eq!(s[0].args, vec!["stash", "apply", "--index", "stash@{1}"]);

        let s = compile_simple_steps(&GitOp::StashApply {
            index: 0,
            pop: true,
            reinstate_index: true,
        })
        .unwrap();
        assert_eq!(s[0].args, vec!["stash", "pop", "--index", "stash@{0}"]);
    }
}
