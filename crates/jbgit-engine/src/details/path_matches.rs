use std::collections::HashSet;
use std::path::Path;
use std::process::Stdio;

use git2::{DiffOptions, Repository};
use jbgit_core::{EpochToken, Oid, RepoSnapshot};

use crate::error::EngineError;
use crate::pool::with_thread_repo;
use crate::probe::RepoLocation;

use super::{non_interactive_git_command, to_git2_oid};

/// Commits of `snapshot` touching any of `paths` (log `Paths` filter).
///
/// Prefers the native `git log` path walk, which uses the commit-graph and
/// changed-path Bloom filters when the repository has them; falls back to the
/// per-commit libgit2 tree diff when no `git` CLI is available, the pathspec
/// list is too long for the command line, or the CLI run fails.
pub fn load_path_matches_sync(
    location: &RepoLocation,
    paths: &[String],
    snapshot: &RepoSnapshot,
    token: &EpochToken,
) -> Result<HashSet<Oid>, EngineError> {
    if token.is_stale() {
        return Err(EngineError::Cancelled);
    }
    if paths.len() <= crate::cli::MAX_INLINE_PATHSPECS
        && let Some(git) = crate::cli::cached_git_executable()
    {
        match load_path_matches_cli(git, location, paths, snapshot, token) {
            Ok(set) => return Ok(set),
            Err(EngineError::Cancelled) => return Err(EngineError::Cancelled),
            Err(e) => tracing::debug!("git log path filter failed, using libgit2: {e}"),
        }
    }
    load_path_matches_git2(location, paths, snapshot, token)
}

/// `git log --full-history` over the snapshot's ref set.
pub fn load_path_matches_cli(
    git: &Path,
    location: &RepoLocation,
    paths: &[String],
    snapshot: &RepoSnapshot,
    token: &EpochToken,
) -> Result<HashSet<Oid>, EngineError> {
    use std::io::BufRead;

    if token.is_stale() {
        return Err(EngineError::Cancelled);
    }
    if snapshot.commits.is_empty() || paths.is_empty() {
        return Ok(HashSet::new());
    }
    let known: HashSet<Oid> = snapshot.commits.iter().map(|c| c.oid).collect();
    let mut args: Vec<String> = [
        "-c",
        "core.quotepath=false",
        "-c",
        "log.showSignature=false",
        "log",
        "--format=%H",
        "--full-history",
        "HEAD",
        "--branches",
        "--remotes",
        "--tags",
    ]
    .iter()
    .map(ToString::to_string)
    .collect();
    args.push("--".to_string());
    args.extend(paths.iter().cloned());

    let cwd = if location.is_bare {
        &location.git_dir
    } else {
        &location.workdir
    };
    let mut cmd = non_interactive_git_command(git, Some(cwd));
    cmd.args(&args)
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = cmd.spawn()?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| EngineError::Other("git log: no stdout".to_string()))?;

    let (line_tx, line_rx) = crossbeam_channel::unbounded::<String>();
    let reader = std::thread::Builder::new()
        .name("jbgit-path-log-reader".into())
        .spawn(move || {
            for line in std::io::BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if line_tx.send(line).is_err() {
                    break;
                }
            }
        })?;

    let mut out = HashSet::new();
    let mut cancelled = false;
    let mut lines = 0usize;
    loop {
        match line_rx.recv_timeout(std::time::Duration::from_millis(50)) {
            Ok(line) => {
                lines += 1;
                if let Ok(oid) = line.trim().parse::<Oid>()
                    && known.contains(&oid)
                {
                    out.insert(oid);
                }
                if lines.is_multiple_of(256) && token.is_stale() {
                    cancelled = true;
                    break;
                }
            }
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                if token.is_stale() {
                    cancelled = true;
                    break;
                }
            }
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
        }
    }
    if cancelled {
        let _ = child.kill();
        let _ = child.wait();
        drop(line_rx);
        let _ = reader.join();
        return Err(EngineError::Cancelled);
    }
    let _ = reader.join();
    let status = child.wait()?;
    if !status.success() {
        return Err(EngineError::Other(format!("git log exited with {status}")));
    }

    let merges: Vec<Oid> = snapshot
        .commits
        .iter()
        .filter(|c| c.parents.len() > 1 && out.contains(&c.oid))
        .map(|c| c.oid)
        .collect();
    if !merges.is_empty() {
        with_thread_repo(&location.git_dir, |repo| {
            for (idx, oid) in merges.iter().enumerate() {
                if idx % 64 == 0 && token.is_stale() {
                    return Err(EngineError::Cancelled);
                }
                if !commit_touches_paths(repo, oid, paths)? {
                    out.remove(oid);
                }
            }
            Ok(())
        })?;
    }
    Ok(out)
}

/// libgit2 fallback: diff every snapshot commit against each parent.
pub fn load_path_matches_git2(
    location: &RepoLocation,
    paths: &[String],
    snapshot: &RepoSnapshot,
    token: &EpochToken,
) -> Result<HashSet<Oid>, EngineError> {
    if token.is_stale() {
        return Err(EngineError::Cancelled);
    }

    with_thread_repo(&location.git_dir, |repo| {
        let mut out = HashSet::new();
        for (idx, summary) in snapshot.commits.iter().enumerate() {
            if idx % 64 == 0 && token.is_stale() {
                return Err(EngineError::Cancelled);
            }
            if commit_touches_paths(repo, &summary.oid, paths)? {
                out.insert(summary.oid);
            }
        }
        Ok(out)
    })
}

/// TREESAME rule shared by both path-filter backends: a commit matches when its
/// tree differs at `paths` from *every* parent (root: from the empty tree).
fn commit_touches_paths(
    repo: &Repository,
    oid: &Oid,
    paths: &[String],
) -> Result<bool, EngineError> {
    let Ok(commit) = repo.find_commit(to_git2_oid(oid)?) else {
        return Ok(false);
    };
    let Ok(tree) = commit.tree() else {
        return Ok(false);
    };
    let touches = |parent: Option<&git2::Tree<'_>>| -> bool {
        let mut opts = DiffOptions::new();
        for p in paths {
            opts.pathspec(p);
        }
        repo.diff_tree_to_tree(parent, Some(&tree), Some(&mut opts))
            .map(|d| d.deltas().len() > 0)
            .unwrap_or(false)
    };
    Ok(if commit.parent_count() == 0 {
        touches(None)
    } else {
        commit
            .parents()
            .all(|p| p.tree().is_ok_and(|pt| touches(Some(&pt))))
    })
}
