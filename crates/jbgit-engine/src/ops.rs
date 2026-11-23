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
    GitFailure, GitOp, GitOpOutcome, MergeOptions, Oid, PatchSource, SkippedBranch, UpdateInfo,
    UpdateMode, UpdatedRange, classify_failure, render_todo,
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
        GitOp::CheckoutNewBranch { name, start, track } => {
            let mut args = vec!["checkout".to_string(), "-b".to_string(), name.clone()];
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
        GitOp::CreateBranch { name, start } => {
            vec![CompiledStep::new([
                "branch",
                "--",
                name.as_str(),
                start.as_str(),
            ])]
        }
        GitOp::RenameBranch { old, new } => {
            vec![CompiledStep::new(["branch", "-m", old.as_str(), new.as_str()])]
        }
        GitOp::DeleteBranch { name, force, .. } => vec![CompiledStep::new([
            "branch",
            if *force { "-D" } else { "-d" },
            name.as_str(),
        ])],
        GitOp::RestoreBranch { name, oid } => vec![CompiledStep::new(vec![
            "branch".to_string(),
            name.clone(),
            oid.to_string(),
        ])],
        GitOp::DeleteRemoteBranch { remote, branch } => vec![CompiledStep::new([
            "push",
            remote.as_str(),
            "--delete",
            branch.as_str(),
        ])],
        GitOp::CreateTag {
            name,
            target,
            message,
            force,
        } => {
            let mut args = vec!["tag".to_string()];
            if *force {
                args.push("-f".to_string());
            }
            if let Some(m) = message.as_ref().filter(|m| !m.trim().is_empty()) {
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
        GitOp::Merge { rev, options } => vec![compile_merge(rev, options)],
        GitOp::Rebase { onto, branch } => {
            let mut args = vec!["rebase".to_string(), onto.clone()];
            if let Some(b) = branch {
                args.push(b.clone());
            }
            vec![no_edit(CompiledStep::new(args))]
        }
        GitOp::CherryPick { oids } => {
            let mut args = vec!["cherry-pick".to_string()];
            args.extend(oids.iter().map(ToString::to_string));
            vec![no_edit(CompiledStep::new(args))]
        }
        GitOp::Revert { oids } => {
            let mut args = vec!["revert".to_string(), "--no-edit".to_string()];
            args.extend(oids.iter().map(ToString::to_string));
            vec![no_edit(CompiledStep::new(args))]
        }
        GitOp::Reset { mode, target } => vec![CompiledStep::new(vec![
            "reset".to_string(),
            mode.flag().to_string(),
            target.to_string(),
        ])],
        GitOp::Fetch => vec![CompiledStep::new(["fetch", "--all", "--prune"])],
        GitOp::Pull { rebase } => vec![no_edit(CompiledStep::new(if *rebase {
            vec!["pull", "--rebase"]
        } else {
            vec!["pull", "--no-edit"]
        }))],
        GitOp::Push {
            branch,
            remote,
            remote_branch,
            set_upstream,
            force,
            tags,
            up_to,
        } => {
            let mut args = vec!["push".to_string(), "--porcelain".to_string()];
            if *set_upstream {
                args.push("--set-upstream".to_string());
            }
            if let Some(flag) = force.flag() {
                args.push(flag.to_string());
            }
            if let Some(flag) = tags.flag() {
                args.push(flag.to_string());
            }
            args.push(remote.clone());
            let target_branch = if remote_branch.is_empty() {
                branch.as_str()
            } else {
                remote_branch.as_str()
            };
            match up_to {
                Some(oid) => args.push(format!("{oid}:refs/heads/{target_branch}")),
                None => args.push(format!("refs/heads/{branch}:refs/heads/{target_branch}")),
            }
            vec![CompiledStep::new(args)]
        }
        GitOp::UpdateBranch {
            branch,
            remote,
            remote_branch,
        } => vec![CompiledStep::new(vec![
            "fetch".to_string(),
            remote.clone(),
            format!("refs/heads/{remote_branch}:refs/heads/{branch}"),
        ])],
        GitOp::RewordHead { message } => vec![
            CompiledStep::new([
                "commit",
                "--amend",
                "--only",
                "--allow-empty",
                "--no-verify",
                "-F",
                "-",
            ])
            .with_stdin(message.as_bytes().to_vec()),
        ],
        GitOp::FixupCommit {
            target,
            base,
            autosquash,
        } => {
            let mut steps = vec![CompiledStep::new(vec![
                "commit".to_string(),
                "--no-verify".to_string(),
                format!("--fixup={target}"),
            ])];
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
            paths,
        } => {
            let mut args = vec!["stash".to_string(), "push".to_string()];
            if *include_untracked {
                args.push("--include-untracked".to_string());
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
        GitOp::StashApply { index, pop } => vec![CompiledStep::new(vec![
            "stash".to_string(),
            if *pop { "pop" } else { "apply" }.to_string(),
            format!("stash@{{{index}}}"),
        ])],
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
        GitOp::AcceptSide { paths, ours } => {
            if paths.is_empty() {
                Vec::new()
            } else {
                let co = CompiledStep::with_pathspecs(
                    vec![
                        "checkout".to_string(),
                        if *ours { "--ours" } else { "--theirs" }.to_string(),
                    ],
                    paths,
                );
                let add = CompiledStep::with_pathspecs(vec!["add".to_string()], paths);
                vec![co, add]
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
        GitOp::CreatePatchFiles { oids, base } => {
            let mut args = vec!["format-patch".to_string(), "-o".to_string(), ".".to_string()];
            if !push_format_patch_target(&mut args, oids, *base) {
                return Some(Vec::new());
            }
            vec![CompiledStep::new(args)]
        }
        GitOp::PatchToClipboard { source } => compile_patch_source(source),
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
        GitOp::RewriteHistory { .. }
        | GitOp::AddToIgnore { .. }
        | GitOp::DeleteFiles { .. }
        | GitOp::Sequence(_)
        | GitOp::UpdateProject { .. }
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

/// Keeps a commit-message file Git prepared for the next commit, minus `#` comments.
pub(crate) fn clean_prepared_message(raw: &str) -> Option<String> {
    let text: Vec<&str> = raw
        .lines()
        .filter(|l| !l.starts_with('#'))
        .collect();
    let joined = text.join("\n");
    let trimmed = joined.trim();
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

fn push_format_patch_target(
    args: &mut Vec<String>,
    oids: &[jbgit_core::Oid],
    base: Option<jbgit_core::Oid>,
) -> bool {
    match oids {
        [one] => {
            args.push("-1".to_string());
            args.push(one.to_string());
            true
        }
        [newest, ..] => {
            match base {
                Some(b) => args.push(format!("{b}..{newest}")),
                None => {
                    args.push("--root".to_string());
                    args.push(newest.to_string());
                }
            }
            true
        }
        [] => false,
    }
}

fn compile_patch_source(source: &PatchSource) -> Vec<CompiledStep> {
    match source {
        PatchSource::Commits { oids, base } => {
            let mut args = vec!["format-patch".to_string(), "--stdout".to_string()];
            if !push_format_patch_target(&mut args, oids, *base) {
                return Vec::new();
            }
            vec![CompiledStep::new(args)]
        }
        PatchSource::CommitFile { oid, path } => vec![CompiledStep::new([
            "show",
            "--format=",
            "--patch",
            &oid.to_string(),
            "--",
            path.as_str(),
        ])],
        PatchSource::StagePaths { staged, paths } => {
            let mut args = vec!["diff".to_string()];
            if *staged {
                args.push("--cached".to_string());
            }
            if !paths.is_empty() {
                args.push("--".to_string());
                args.extend(paths.iter().cloned());
            }
            vec![CompiledStep::new(args)]
        }
    }
}

/// Whether a compiled step talks to a remote (and may need credentials).
pub(crate) fn is_network_step(args: &[String]) -> bool {
    matches!(
        args.first().map(String::as_str),
        Some("fetch" | "pull" | "push" | "ls-remote")
    )
}

/// Runs `op` to completion through [`GitCliRunner`] and reports both the
/// classified [`GitOpOutcome`] and any `index.lock` conflict.
///
/// `askpass` (when set) is added to the environment of network steps only
/// (`fetch` / `pull` / `push` / `ls-remote`), so credential prompts reach the
/// TUI; local commands never see it.
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
    let askpass_vars = askpass.map(AskpassEnv::vars).unwrap_or_default();
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

    async fn run_plain(&self, op: GitOp) -> GitOpRunResult {
        if let Some(steps) = compile_simple_steps(&op) {
            let prepared = match &op {
                GitOp::Merge { options, .. } if options.leaves_changes_uncommitted() => {
                    Some(if options.squash { "SQUASH_MSG" } else { "MERGE_MSG" })
                }
                _ => None,
            };
            let mut res = self.run_steps(op, steps).await;
            if res.outcome.success
                && let Some(file) = prepared
            {
                res.outcome.prepared_message = std::fs::read_to_string(self.loc.git_dir.join(file))
                    .ok()
                    .and_then(|raw| clean_prepared_message(&raw));
            }
            return res;
        }
        match op.clone() {
            GitOp::RewriteHistory { base, todo } => {
                let dir = self.loc.git_dir.join("jbgit-rebase");
                let _ = std::fs::remove_dir_all(&dir);
                if let Err(e) = std::fs::create_dir_all(&dir) {
                    return GitOpRunResult {
                        outcome: GitOpOutcome::failed(op, GitFailure::Other(e.to_string())),
                        index_lock: None,
                    };
                }
                let mut write_err = None;
                let text = render_todo(&todo, |i| {
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
                let finished = finish(op, res);
                if !matches!(finished.outcome.failure, Some(GitFailure::Conflicts)) {
                    let _ = std::fs::remove_dir_all(&dir);
                }
                finished
            }
            GitOp::AddToIgnore {
                patterns,
                exclude_file,
            } => {
                let path = if exclude_file {
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
            } => {
                self.run_update_project(op, mode, save_changes, update_other_branches)
                    .await
            }
            GitOp::Smart(inner) => Box::pin(self.run_smart(*inner)).await,
            _ => GitOpRunResult {
                outcome: GitOpOutcome::failed(op, GitFailure::Other("unsupported operation".into())),
                index_lock: None,
            },
        }
    }

    async fn run_sequence(&self, ops: Vec<GitOp>) -> GitOpRunResult {
        let mut combined_stderr = Vec::new();
        let mut last_stdout = Vec::new();
        let mut any_stash_kept = false;
        for inner in &ops {
            let mut res = match inner.clone() {
                GitOp::Smart(s) => Box::pin(self.run_smart(*s)).await,
                other => Box::pin(self.run_plain(other)).await,
            };
            combined_stderr.extend(res.outcome.stderr.iter().cloned());
            last_stdout = res.outcome.stdout.clone();
            any_stash_kept |= res.outcome.stash_kept;
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
    /// other tracked branches → restore → [`UpdateInfo`].
    async fn run_update_project(
        &self,
        op: GitOp,
        mode: Option<UpdateMode>,
        save_changes: bool,
        update_other_branches: bool,
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
            let msg = format!(
                "Upstream '{}' of '{}' no longer exists",
                current.upstream_short, current.name
            );
            bail!(
                StepResult {
                    exit_code: 1,
                    stdout: Vec::new(),
                    stderr: Vec::new(),
                    index_lock: None,
                },
                GitFailure::Other(msg)
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

        // 6. Restore local changes.
        let mut outcome = GitOpOutcome::ok(op);
        outcome.update_info = Some(info);
        let mut index_lock = None;
        if stashed {
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
        if matches!(res.outcome.failure, Some(GitFailure::Conflicts)) {
            res.outcome.stash_kept = true;
            return res;
        }
        let pop = self.stash_restore().await;
        res.outcome.stderr.extend(pop.stderr.iter().cloned());
        if !pop.ok() {
            res.outcome.stash_kept = true;
            if res.outcome.success {
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
    for l in lines {
        if existing_text.lines().any(|e| e.trim() == l.trim()) {
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
            GitOp::Pull { rebase: false },
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
            GitOp::AcceptSide {
                paths: many.clone(),
                ours: true,
            },
            GitOp::IntentToAdd {
                paths: many.clone(),
            },
            GitOp::StashPush {
                message: Some("wip".into()),
                include_untracked: true,
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

        let push_force_all = GitOp::Push {
            branch: "dev".into(),
            remote: "origin".into(),
            remote_branch: String::new(),
            set_upstream: false,
            force: PushForce::Force,
            tags: PushTagMode::All,
            up_to: None,
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

        let tag_light = GitOp::CreateTag {
            name: "-tricky".into(),
            target: oid(1),
            message: None,
            force: true,
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
        };
        assert_eq!(
            compile_simple_steps(&tag_ann).unwrap()[0].args,
            vec!["tag", "-a", "-m", "release", "--", "v1.0", &oid(1).to_string()]
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

        let patch_commits = GitOp::PatchToClipboard {
            source: PatchSource::Commits {
                oids: vec![oid(2), oid(1)],
                base: None,
            },
        };
        assert_eq!(
            compile_simple_steps(&patch_commits).unwrap()[0].args,
            vec!["format-patch", "--stdout", "--root", &oid(2).to_string()]
        );

        let patch_file = GitOp::PatchToClipboard {
            source: PatchSource::CommitFile {
                oid: oid(3),
                path: "src/lib.rs".into(),
            },
        };
        assert_eq!(
            compile_simple_steps(&patch_file).unwrap()[0].args,
            vec![
                "show",
                "--format=",
                "--patch",
                &oid(3).to_string(),
                "--",
                "src/lib.rs"
            ]
        );

        let patch_stage = GitOp::PatchToClipboard {
            source: PatchSource::StagePaths {
                staged: true,
                paths: vec!["a.rs".into()],
            },
        };
        assert_eq!(
            compile_simple_steps(&patch_stage).unwrap()[0].args,
            vec!["diff", "--cached", "--", "a.rs"]
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
                onto: "origin/main".into(),
                branch: None,
            },
            GitOp::Push {
                branch: "main".into(),
                remote: "origin".into(),
                remote_branch: "main".into(),
                set_upstream: false,
                force: PushForce::None,
                tags: PushTagMode::None,
                up_to: None,
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
}
