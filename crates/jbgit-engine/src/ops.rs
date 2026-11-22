//! Unified [`GitOp`] CLI compiler and executor.
//!
//! Translates pure declarative [`jbgit_core::GitOp`] domain intents into one or
//! more `git` CLI invocations (including `--pathspec-from-file` for large path
//! sets, **Smart Operation Saver** stash/restore wrapping, and editor-free
//! interactive rebase) and runs them through the [`GitCliRunner`] seam.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use jbgit_core::{
    GitFailure, GitOp, GitOpOutcome, PatchSource, classify_failure, render_todo,
};

use crate::cli::{
    CliCommand, CliStreamEvent, GitCliRunner, IndexLockConflict, prepare_pathspec_args,
};
use crate::probe::RepoLocation;

/// Console ids for operation steps live far above UI-allocated ids.
pub const OP_CONSOLE_ID_BASE: u64 = 1 << 40;

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
        } => {
            let mut args = vec!["tag".to_string()];
            if let Some(m) = message.as_ref().filter(|m| !m.trim().is_empty()) {
                args.extend(["-a".to_string(), "-m".to_string(), m.clone()]);
            }
            args.extend([name.clone(), target.to_string()]);
            vec![CompiledStep::new(args)]
        }
        GitOp::Merge { rev } => {
            vec![no_edit(CompiledStep::new([
                "merge",
                "--no-edit",
                rev.as_str(),
            ]))]
        }
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
        | GitOp::Smart(_) => return None,
    };
    Some(steps)
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

/// Runs `op` to completion through [`GitCliRunner`] and reports both the
/// classified [`GitOpOutcome`] and any `index.lock` conflict.
pub(crate) async fn run_git_op<R, F>(
    cli: &R,
    loc: &RepoLocation,
    ids: &AtomicU64,
    op: GitOp,
    on_cli: F,
) -> GitOpRunResult
where
    R: GitCliRunner,
    F: Fn(CliStreamEvent) + Send + Sync,
{
    let runner = Runner {
        cli,
        loc,
        ids,
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
    on_cli: &'a (dyn Fn(CliStreamEvent) + Send + Sync),
}

impl<R: GitCliRunner> Runner<'_, R> {
    async fn step(&self, step: &CompiledStep, read_only: bool) -> StepResult {
        let id = OP_CONSOLE_ID_BASE + self.ids.fetch_add(1, Ordering::Relaxed);
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
                    env: &step.env,
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
            return self.run_steps(op, steps).await;
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
            GitOp::Smart(inner) => Box::pin(self.run_smart(*inner)).await,
            _ => GitOpRunResult {
                outcome: GitOpOutcome::failed(op, GitFailure::Other("unsupported operation".into())),
                index_lock: None,
            },
        }
    }

    /// `GitPreservingProcess`: stash local changes (including untracked),
    /// run the operation, then pop them back.
    async fn run_smart(&self, inner: GitOp) -> GitOpRunResult {
        let label = inner.label();
        let save = CompiledStep::new(vec![
            "stash".to_string(),
            "push".to_string(),
            "--include-untracked".to_string(),
            "-m".to_string(),
            format!("jbgit smart: {label}"),
        ]);
        let saved = self.step(&save, false).await;
        if !saved.ok() {
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
        let stashed = !saved
            .all_lines()
            .iter()
            .any(|l| l.contains("No local changes to save"));

        let mut res = Box::pin(self.run_plain(inner.clone())).await;
        res.outcome.op = GitOp::Smart(Box::new(inner));
        if !stashed {
            return res;
        }
        if matches!(res.outcome.failure, Some(GitFailure::Conflicts)) {
            res.outcome.stash_kept = true;
            return res;
        }
        let pop = self
            .step(&CompiledStep::new(["stash", "pop", "--index"]), false)
            .await;
        let pop = if pop.ok() {
            pop
        } else if pop
            .all_lines()
            .iter()
            .any(|l| l.contains("--index") || l.contains("index"))
            && !pop.all_lines().iter().any(|l| l.contains("CONFLICT"))
        {
            self.step(&CompiledStep::new(["stash", "pop"]), false).await
        } else {
            pop
        };
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

    use jbgit_core::{Oid, OngoingKind, OngoingStep, ResetMode};

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
            set_upstream: true,
            force_with_lease: false,
            up_to: Some(oid(7)),
        };
        assert_eq!(
            compile_simple_steps(&push).unwrap()[0].args,
            vec![
                "push".to_string(),
                "--set-upstream".into(),
                "origin".into(),
                format!("{}:refs/heads/feat", oid(7)),
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
                set_upstream: false,
                force_with_lease: false,
                up_to: None,
            },
            |_| {},
        )
        .await;
        assert!(!res.outcome.success);
        assert_eq!(res.outcome.failure, Some(GitFailure::PushRejected));
        assert!(res.index_lock.is_none());
        let calls = runner.calls();
        assert!(!calls[0].is_read_only);
        assert_eq!(calls[0].args, vec!["push", "origin", "refs/heads/main:refs/heads/main"]);
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
}
