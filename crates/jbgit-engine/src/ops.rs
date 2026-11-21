//! Executes high-level [`GitOp`]s as sequences of `git` CLI steps,
//! streaming every step to the Console tab, with:
//!
//! * **Smart Operation Saver** (`GitPreservingProcess`): `stash push -u` →
//!   operation → `stash pop`, keeping the stash when anything conflicts;
//! * **editor-free interactive rebase**: the todo is written by a POSIX-shell
//!   builtin `GIT_SEQUENCE_EDITOR` and rewording happens through
//!   `exec git commit --amend --only -F <file>` lines, so Git never opens an
//!   interactive editor.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use jbgit_core::{GitFailure, GitOp, GitOpOutcome, GitStep, classify_failure, render_todo};

use crate::cli::{CliCommand, CliStreamEvent, GitCliExecutor};
use crate::probe::RepoLocation;

/// Console ids for operation steps live far above UI-allocated ids.
pub const OP_CONSOLE_ID_BASE: u64 = 1 << 40;

struct StepResult {
    exit_code: i32,
    stdout: Vec<String>,
    stderr: Vec<String>,
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

/// Runs `op` to completion and reports the classified outcome. `on_cli`
/// receives console stream events of every executed step.
pub async fn run_git_op<F>(
    cli: &GitCliExecutor,
    loc: &RepoLocation,
    ids: &AtomicU64,
    op: GitOp,
    on_cli: F,
) -> GitOpOutcome
where
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

struct Runner<'a> {
    cli: &'a GitCliExecutor,
    loc: &'a RepoLocation,
    ids: &'a AtomicU64,
    on_cli: &'a (dyn Fn(CliStreamEvent) + Send + Sync),
}

impl Runner<'_> {
    async fn step(&self, step: &GitStep, stdin: Option<&[u8]>, read_only: bool) -> StepResult {
        let id = OP_CONSOLE_ID_BASE + self.ids.fetch_add(1, Ordering::Relaxed);
        let res = self
            .cli
            .execute(
                CliCommand {
                    id,
                    workdir: &self.loc.workdir,
                    git_dir: &self.loc.git_dir,
                    args: &step.args,
                    stdin_payload: stdin,
                    is_read_only: read_only,
                    env: &step.env,
                },
                |ev| (self.on_cli)(ev),
            )
            .await;
        match res {
            Ok(out) => StepResult {
                exit_code: out.exit_code,
                stdout: out.stdout_lines,
                stderr: out.stderr_lines,
            },
            Err(e) => StepResult {
                exit_code: -1,
                stdout: Vec::new(),
                stderr: vec![format!("fatal: {e}")],
            },
        }
    }

    async fn run_steps(&self, op: GitOp, steps: Vec<GitStep>) -> GitOpOutcome {
        let read_only = op.is_read_only();
        let mut outcome = GitOpOutcome::ok(op);
        for step in &steps {
            let res = self.step(step, None, read_only).await;
            outcome.stderr.extend(res.stderr.iter().cloned());
            outcome.stdout = res.stdout.clone();
            if !res.ok() {
                outcome.success = false;
                outcome.failure = Some(classify_failure(&res.all_lines()));
                return outcome;
            }
        }
        outcome
    }

    async fn run_plain(&self, op: GitOp) -> GitOpOutcome {
        if let Some(steps) = op.simple_steps() {
            return self.run_steps(op, steps).await;
        }
        match op.clone() {
            GitOp::RewordHead { message } => {
                let step = GitStep::new([
                    "commit",
                    "--amend",
                    "--only",
                    "--allow-empty",
                    "--no-verify",
                    "-F",
                    "-",
                ]);
                let res = self.step(&step, Some(message.as_bytes()), false).await;
                finish(op, res)
            }
            GitOp::RewriteHistory { base, todo } => {
                let dir = self.loc.git_dir.join("jbgit-rebase");
                let _ = std::fs::remove_dir_all(&dir);
                if let Err(e) = std::fs::create_dir_all(&dir) {
                    return GitOpOutcome::failed(op, GitFailure::Other(e.to_string()));
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
                    return GitOpOutcome::failed(op, GitFailure::Other(e));
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
                let step = GitStep::new(args)
                    .with_env("GIT_SEQUENCE_EDITOR", &sequence_editor_script(&todo_path))
                    .with_env("GIT_EDITOR", ":");
                let res = self.step(&step, None, false).await;
                let outcome = finish(op, res);
                if !matches!(outcome.failure, Some(GitFailure::Conflicts)) {
                    let _ = std::fs::remove_dir_all(&dir);
                }
                outcome
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
                match append_lines(&path, &patterns) {
                    Ok(()) => GitOpOutcome::ok(op),
                    Err(e) => GitOpOutcome::failed(op, GitFailure::Other(e.to_string())),
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
                        return GitOpOutcome::failed(op, GitFailure::Other(format!("{p}: {e}")));
                    }
                }
                GitOpOutcome::ok(op)
            }
            GitOp::CheckoutPaths { rev, paths } => {
                if paths.is_empty() {
                    return GitOpOutcome::ok(op);
                }
                let (args, stdin) =
                    crate::cli::prepare_pathspec_args(vec!["checkout".to_string(), rev], &paths);
                let res = self
                    .step(&GitStep::new(args), stdin.as_deref(), false)
                    .await;
                finish(op, res)
            }
            GitOp::Smart(inner) => Box::pin(self.run_smart(*inner)).await,
            _ => GitOpOutcome::failed(op, GitFailure::Other("unsupported operation".into())),
        }
    }

    /// `GitPreservingProcess`: stash local changes (including untracked),
    /// run the operation, then pop them back.
    async fn run_smart(&self, inner: GitOp) -> GitOpOutcome {
        let label = inner.label();
        let save = GitStep::new(vec![
            "stash".to_string(),
            "push".to_string(),
            "--include-untracked".to_string(),
            "-m".to_string(),
            format!("jbgit smart: {label}"),
        ]);
        let saved = self.step(&save, None, false).await;
        if !saved.ok() {
            let mut out = GitOpOutcome::failed(
                GitOp::Smart(Box::new(inner)),
                GitFailure::Other(format!(
                    "Could not save local changes: {}",
                    first_line(&saved.all_lines())
                )),
            );
            out.stderr = saved.stderr;
            return out;
        }
        let stashed = !saved
            .all_lines()
            .iter()
            .any(|l| l.contains("No local changes to save"));

        let mut outcome = Box::pin(self.run_plain(inner.clone())).await;
        outcome.op = GitOp::Smart(Box::new(inner));
        if !stashed {
            return outcome;
        }
        if matches!(outcome.failure, Some(GitFailure::Conflicts)) {
            // The operation is paused on conflicts: keep the changes stashed
            // until the user finishes it (IDEA shows "Local changes saved").
            outcome.stash_kept = true;
            return outcome;
        }
        let pop = self
            .step(&GitStep::new(["stash", "pop", "--index"]), None, false)
            .await;
        let pop = if pop.ok() {
            pop
        } else if pop
            .all_lines()
            .iter()
            .any(|l| l.contains("--index") || l.contains("index"))
            && !pop.all_lines().iter().any(|l| l.contains("CONFLICT"))
        {
            // Index could not be restored exactly; fall back to a plain pop.
            self.step(&GitStep::new(["stash", "pop"]), None, false)
                .await
        } else {
            pop
        };
        outcome.stderr.extend(pop.stderr.iter().cloned());
        if !pop.ok() {
            outcome.stash_kept = true;
            if outcome.success {
                outcome.failure = Some(GitFailure::Conflicts);
            }
        }
        outcome
    }
}

fn finish(op: GitOp, res: StepResult) -> GitOpOutcome {
    let mut out = GitOpOutcome::ok(op);
    out.stdout = res.stdout.clone();
    out.stderr = res.stderr.clone();
    if !res.ok() {
        out.success = false;
        out.failure = Some(classify_failure(&res.all_lines()));
    }
    out
}

fn first_line(lines: &[String]) -> String {
    lines
        .iter()
        .find(|l| !l.trim().is_empty())
        .cloned()
        .unwrap_or_default()
}

/// Forward-slash, single-quoted path usable inside Git's POSIX shell (also on
/// Windows, where Git for Windows' `sh` accepts `C:/…`).
fn sh_quote(path: &Path) -> String {
    let p = path.to_string_lossy().replace('\\', "/");
    format!("'{}'", p.replace('\'', r"'\''"))
}

/// A `GIT_SEQUENCE_EDITOR` that overwrites Git's todo with ours using only
/// shell builtins (`read` / `printf`), so no external `cp` / `cat` is needed.
/// Git runs it as `sh -c '<script> "$@"' <script> <todo-file>`.
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
