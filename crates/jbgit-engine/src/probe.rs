use std::fs;
use std::path::{Path, PathBuf};

use git2::{ErrorCode, Repository, RepositoryState as Git2State};
use jbgit_core::{Oid, RepositoryState};

use crate::error::EngineError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoLocation {
    pub repo_name: String,
    pub workdir: PathBuf,
    pub git_dir: PathBuf,
    pub common_dir: PathBuf,
    pub is_bare: bool,
}

impl RepoLocation {
    /// Discovers a Git repository starting from `path`, supporting standard repos,
    /// linked worktrees (`.git` file), and submodules.
    pub fn discover(path: &Path) -> Result<Self, EngineError> {
        let search_start = if path.as_os_str().is_empty() {
            Path::new(".")
        } else {
            path
        };

        let repo = Repository::discover(search_start)
            .map_err(|_| EngineError::RepoNotFound(search_start.to_path_buf()))?;

        let git_dir = canonical_or_self(repo.path());
        let common_dir = canonical_or_self(repo.commondir());
        let is_bare = repo.is_bare();
        let workdir = match repo.workdir() {
            Some(wd) => canonical_or_self(wd),
            None => git_dir.clone(),
        };

        let repo_name = workdir
            .file_name()
            .and_then(|s| s.to_str())
            .filter(|s| !s.is_empty())
            .map(ToString::to_string)
            .unwrap_or_else(|| workdir.display().to_string());

        Ok(Self {
            repo_name,
            workdir,
            git_dir,
            common_dir,
            is_bare,
        })
    }
}

fn canonical_or_self(p: &Path) -> PathBuf {
    strip_verbatim_prefix(fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf()))
}

#[cfg(windows)]
fn strip_verbatim_prefix(p: PathBuf) -> PathBuf {
    let s = p.to_string_lossy();
    if let Some(rest) = s.strip_prefix(r"\\?\") {
        PathBuf::from(rest)
    } else {
        p
    }
}

#[cfg(not(windows))]
fn strip_verbatim_prefix(p: PathBuf) -> PathBuf {
    p
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoProbeResult {
    pub state: RepositoryState,
    pub is_unborn_head: bool,
    pub head_branch: Option<String>,
    pub head_oid: Option<Oid>,
}

pub fn to_core_oid(oid: git2::Oid) -> Oid {
    let mut bytes = [0u8; 20];
    let slice = oid.as_bytes();
    let len = slice.len().min(20);
    bytes[..len].copy_from_slice(&slice[..len]);
    Oid::from_bytes(bytes)
}

/// Probes HEAD, unborn branch state, detached HEAD, and multi-step sequencer states
/// (merge, rebase / interactive rebase progress, cherry-pick, revert, bisect).
pub fn probe_repo_state(repo: &Repository) -> RepoProbeResult {
    let git_dir = repo.path();

    let (is_unborn_head, head_branch, head_oid, is_detached) = match repo.head() {
        Ok(head) => {
            let oid = head.target().map(to_core_oid);
            let is_detached = repo.head_detached().unwrap_or(false);
            let branch = if !is_detached && head.is_branch() {
                head.shorthand().ok().map(ToString::to_string)
            } else {
                None
            };
            (false, branch, oid, is_detached)
        }
        Err(err) if err.code() == ErrorCode::UnbornBranch || err.code() == ErrorCode::NotFound => {
            let branch = read_unborn_branch_name(git_dir, repo);
            (true, branch, None, false)
        }
        Err(_) => (false, None, None, repo.head_detached().unwrap_or(false)),
    };

    let state = detect_repository_state(repo, git_dir, is_detached);

    RepoProbeResult {
        state,
        is_unborn_head,
        head_branch,
        head_oid,
    }
}

fn read_unborn_branch_name(git_dir: &Path, repo: &Repository) -> Option<String> {
    if let Ok(head_ref) = repo.find_reference("HEAD")
        && let Ok(Some(sym)) = head_ref.symbolic_target()
    {
        return Some(strip_refs_heads(sym));
    }

    let head_path = git_dir.join("HEAD");
    let content = fs::read_to_string(head_path).ok()?;
    let trimmed = content.trim();
    trimmed
        .strip_prefix("ref:")
        .map(|rest| strip_refs_heads(rest.trim()))
}

fn detect_repository_state(
    repo: &Repository,
    git_dir: &Path,
    is_detached: bool,
) -> RepositoryState {
    if git_dir.join("MERGE_HEAD").exists() || repo.state() == Git2State::Merge {
        return RepositoryState::Merge;
    }

    let rebase_merge_dir = git_dir.join("rebase-merge");
    if rebase_merge_dir.is_dir() {
        let progress = read_progress_pair(
            &rebase_merge_dir.join("msgnum"),
            &rebase_merge_dir.join("end"),
        );
        let branch = read_rebase_branch(&rebase_merge_dir.join("head-name"));
        if rebase_merge_dir.join("interactive").exists()
            || repo.state() == Git2State::RebaseInteractive
        {
            return RepositoryState::RebaseInteractive { progress, branch };
        }
        return RepositoryState::RebaseMerge { progress, branch };
    }

    let rebase_apply_dir = git_dir.join("rebase-apply");
    if rebase_apply_dir.is_dir() {
        let progress = read_progress_pair(
            &rebase_apply_dir.join("next"),
            &rebase_apply_dir.join("last"),
        );
        let branch = read_rebase_branch(&rebase_apply_dir.join("head-name"));
        return RepositoryState::Rebase { progress, branch };
    }

    if matches!(
        repo.state(),
        Git2State::Rebase
            | Git2State::RebaseInteractive
            | Git2State::RebaseMerge
            | Git2State::ApplyMailboxOrRebase
    ) {
        return RepositoryState::Rebase {
            progress: None,
            branch: None,
        };
    }

    if git_dir.join("CHERRY_PICK_HEAD").exists()
        || matches!(
            repo.state(),
            Git2State::CherryPick | Git2State::CherryPickSequence
        )
    {
        return RepositoryState::CherryPick;
    }

    if git_dir.join("REVERT_HEAD").exists()
        || matches!(repo.state(), Git2State::Revert | Git2State::RevertSequence)
    {
        return RepositoryState::Revert;
    }

    if git_dir.join("BISECT_LOG").exists() || repo.state() == Git2State::Bisect {
        return RepositoryState::Bisect;
    }

    if is_detached {
        return RepositoryState::Detached;
    }

    RepositoryState::Clean
}

fn read_progress_pair(cur_path: &Path, total_path: &Path) -> Option<(usize, usize)> {
    let cur = fs::read_to_string(cur_path)
        .ok()?
        .trim()
        .parse::<usize>()
        .ok()?;
    let total = fs::read_to_string(total_path)
        .ok()?
        .trim()
        .parse::<usize>()
        .ok()?;
    Some((cur, total))
}

fn read_rebase_branch(head_name_path: &Path) -> Option<String> {
    let content = fs::read_to_string(head_name_path).ok()?;
    let trimmed = content.trim();
    if trimmed.is_empty() || trimmed == "detached HEAD" {
        return None;
    }
    Some(strip_refs_heads(trimmed))
}

fn strip_refs_heads(s: &str) -> String {
    s.strip_prefix("refs/heads/").unwrap_or(s).to_string()
}
