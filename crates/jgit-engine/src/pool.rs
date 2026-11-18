use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread::{self, JoinHandle};

use crossbeam_channel::{Sender, bounded};
use git2::{BranchType, Delta, Repository, Sort, Status, StatusOptions};
use jgit_core::{
    CommitChangesBundle, CommitSummary, DetailsKey, EpochToken, FileDiff, FileDiffKey,
    FileStatusKind, Oid, RefInfo, RefKind, RepoSnapshot, StageEntry, StageStatusSnapshot,
};

use crate::details::{load_commit_details_sync, load_file_diff_sync, load_path_matches_sync};
use crate::error::EngineError;
use crate::probe::{RepoLocation, probe_repo_state, to_core_oid};

thread_local! {
    static THREAD_REPOS: RefCell<HashMap<PathBuf, Repository>> = RefCell::new(HashMap::new());
}

pub(crate) fn with_thread_repo<F, T>(git_dir: &Path, f: F) -> Result<T, EngineError>
where
    F: FnOnce(&Repository) -> Result<T, EngineError>,
{
    THREAD_REPOS.with(|cell| {
        let mut map = cell.borrow_mut();
        if !map.contains_key(git_dir) {
            let repo = Repository::open(git_dir)?;
            map.insert(git_dir.to_path_buf(), repo);
        }
        let repo = map
            .get(git_dir)
            .ok_or_else(|| EngineError::RepoNotFound(git_dir.to_path_buf()))?;
        f(repo)
    })
}

type JobFn = Box<dyn FnOnce() + Send + 'static>;

/// Fixed-size worker pool caching `git2::Repository` handles in `thread_local!`
/// so concurrent read queries avoid re-opening `.git/config` and packfile indices.
pub struct Git2WorkerPool {
    tx: Option<Sender<JobFn>>,
    handles: Vec<JoinHandle<()>>,
}

impl Git2WorkerPool {
    pub fn new(num_threads: usize) -> Self {
        let count = num_threads.clamp(1, 8);
        let (tx, rx) = crossbeam_channel::unbounded::<JobFn>();
        let mut handles = Vec::with_capacity(count);

        for idx in 0..count {
            let rx = rx.clone();
            let handle = thread::Builder::new()
                .name(format!("jgit-git2-worker-{idx}"))
                .spawn(move || {
                    while let Ok(job) = rx.recv() {
                        job();
                    }
                })
                .expect("failed to spawn jgit-git2-worker thread");
            handles.push(handle);
        }

        Self {
            tx: Some(tx),
            handles,
        }
    }

    pub fn spawn_task<F>(&self, f: F)
    where
        F: FnOnce() + Send + 'static,
    {
        if let Some(tx) = &self.tx {
            let _ = tx.send(Box::new(f));
        }
    }

    pub async fn load_repo_snapshot(
        &self,
        location: RepoLocation,
        token: EpochToken,
        max_commits: usize,
    ) -> Result<Arc<RepoSnapshot>, EngineError> {
        let (done_tx, done_rx) = tokio::sync::oneshot::channel();
        self.spawn_task(move || {
            let res = load_repo_snapshot_sync(&location, &token, max_commits).map(Arc::new);
            let _ = done_tx.send(res);
        });
        done_rx
            .await
            .map_err(|_| EngineError::Other("git2 worker dropped".to_string()))?
    }

    pub async fn load_stage_status(
        &self,
        location: RepoLocation,
        include_ignored: bool,
        token: EpochToken,
    ) -> Result<Arc<StageStatusSnapshot>, EngineError> {
        let (done_tx, done_rx) = tokio::sync::oneshot::channel();
        self.spawn_task(move || {
            let res = load_stage_status_sync(&location, include_ignored, &token).map(Arc::new);
            let _ = done_tx.send(res);
        });
        done_rx
            .await
            .map_err(|_| EngineError::Other("git2 worker dropped".to_string()))?
    }

    pub async fn load_commit_details(
        &self,
        location: RepoLocation,
        key: DetailsKey,
        token: EpochToken,
    ) -> Result<Arc<CommitChangesBundle>, EngineError> {
        self.run(move || load_commit_details_sync(&location, &key, &token).map(Arc::new))
            .await
    }

    pub async fn load_file_diff(
        &self,
        location: RepoLocation,
        key: FileDiffKey,
        token: EpochToken,
    ) -> Result<Arc<FileDiff>, EngineError> {
        self.run(move || load_file_diff_sync(&location, &key, &token).map(Arc::new))
            .await
    }

    pub async fn load_path_matches(
        &self,
        location: RepoLocation,
        paths: Vec<String>,
        snapshot: Arc<RepoSnapshot>,
        token: EpochToken,
    ) -> Result<Arc<HashSet<Oid>>, EngineError> {
        self.run(move || load_path_matches_sync(&location, &paths, &snapshot, &token).map(Arc::new))
            .await
    }

    async fn run<T, F>(&self, f: F) -> Result<T, EngineError>
    where
        T: Send + 'static,
        F: FnOnce() -> Result<T, EngineError> + Send + 'static,
    {
        let (done_tx, done_rx) = tokio::sync::oneshot::channel();
        self.spawn_task(move || {
            let _ = done_tx.send(f());
        });
        done_rx
            .await
            .map_err(|_| EngineError::Other("git2 worker dropped".to_string()))?
    }

    pub fn is_path_ignored_sync(&self, git_dir: PathBuf, rel_path: PathBuf) -> bool {
        let (tx, rx) = bounded(1);
        self.spawn_task(move || {
            let ignored = with_thread_repo(&git_dir, |repo| {
                Ok(repo.is_path_ignored(&rel_path).unwrap_or(false))
            })
            .unwrap_or(false);
            let _ = tx.send(ignored);
        });
        rx.recv().unwrap_or(false)
    }
}

impl Drop for Git2WorkerPool {
    fn drop(&mut self) {
        self.tx.take();
        for handle in self.handles.drain(..) {
            let _ = handle.join();
        }
    }
}

pub fn load_repo_snapshot_sync(
    location: &RepoLocation,
    token: &EpochToken,
    max_commits: usize,
) -> Result<RepoSnapshot, EngineError> {
    if token.is_stale() {
        return Err(EngineError::Cancelled);
    }

    with_thread_repo(&location.git_dir, |repo| {
        if token.is_stale() {
            return Err(EngineError::Cancelled);
        }

        let probe = probe_repo_state(repo);
        let refs = collect_refs(
            repo,
            &probe.head_branch,
            probe.state == jgit_core::RepositoryState::Detached,
        )?;

        if token.is_stale() {
            return Err(EngineError::Cancelled);
        }

        let commits = if probe.is_unborn_head {
            Vec::new()
        } else {
            collect_commits(repo, token, max_commits)?
        };

        let (user_name, user_email) = match repo.config() {
            Ok(cfg) => (
                cfg.get_string("user.name").ok(),
                cfg.get_string("user.email").ok(),
            ),
            Err(_) => (None, None),
        };

        Ok(RepoSnapshot {
            repo_name: location.repo_name.clone(),
            workdir: location.workdir.clone(),
            git_dir: location.git_dir.clone(),
            common_dir: location.common_dir.clone(),
            state: probe.state,
            is_unborn_head: probe.is_unborn_head,
            head_branch: probe.head_branch,
            head_oid: probe.head_oid,
            refs,
            commits,
            user_name,
            user_email,
        })
    })
}

fn collect_refs(
    repo: &Repository,
    head_branch: &Option<String>,
    is_detached: bool,
) -> Result<Vec<RefInfo>, EngineError> {
    let mut out = Vec::new();

    if let Ok(branches) = repo.branches(None) {
        for item in branches.flatten() {
            let (branch, branch_type) = item;
            let full_name = branch.get().name().ok().unwrap_or_default().to_string();
            let Some(name) = branch.name().ok().flatten().map(ToString::to_string) else {
                continue;
            };
            // Skip symbolic refs like origin/HEAD
            if branch.get().symbolic_target().ok().flatten().is_some() {
                continue;
            }
            let Ok(commit) = branch.get().peel_to_commit() else {
                continue;
            };
            let target_oid = to_core_oid(commit.id());

            let (kind, upstream, ahead_behind, is_head) = match branch_type {
                BranchType::Local => {
                    let is_head = !is_detached && head_branch.as_deref() == Some(name.as_str());
                    let (upstream_name, ab) = match branch.upstream() {
                        Ok(up) => {
                            let up_name = up.name().ok().flatten().map(ToString::to_string);
                            let ab = up.get().peel_to_commit().ok().and_then(|up_commit| {
                                repo.graph_ahead_behind(commit.id(), up_commit.id()).ok()
                            });
                            (up_name, ab)
                        }
                        Err(_) => (None, None),
                    };
                    (RefKind::LocalBranch, upstream_name, ab, is_head)
                }
                BranchType::Remote => (RefKind::RemoteBranch, None, None, false),
            };

            out.push(RefInfo {
                name,
                full_name,
                target_oid,
                kind,
                upstream,
                ahead_behind,
                is_head,
            });
        }
    }

    if let Ok(tag_names) = repo.tag_names(None) {
        for tag_name in tag_names.iter().flatten().flatten() {
            let full_name = format!("refs/tags/{tag_name}");
            let Ok(reference) = repo.find_reference(&full_name) else {
                continue;
            };
            let Ok(commit) = reference.peel_to_commit() else {
                continue;
            };
            out.push(RefInfo {
                name: tag_name.to_string(),
                full_name,
                target_oid: to_core_oid(commit.id()),
                kind: RefKind::Tag,
                upstream: None,
                ahead_behind: None,
                is_head: false,
            });
        }
    }

    Ok(out)
}

fn collect_commits(
    repo: &Repository,
    token: &EpochToken,
    max_commits: usize,
) -> Result<Vec<CommitSummary>, EngineError> {
    let mut revwalk = repo.revwalk()?;
    revwalk.set_sorting(Sort::TOPOLOGICAL | Sort::TIME)?;

    let _ = revwalk.push_head();
    let _ = revwalk.push_glob("refs/heads/*");
    let _ = revwalk.push_glob("refs/remotes/*");
    let _ = revwalk.push_glob("refs/tags/*");

    let mut commits = Vec::with_capacity(max_commits.min(1024));
    for (idx, oid_res) in revwalk.enumerate() {
        if idx % 256 == 0 && token.is_stale() {
            return Err(EngineError::Cancelled);
        }
        if idx >= max_commits {
            break;
        }
        let Ok(raw_oid) = oid_res else {
            continue;
        };
        let Ok(commit) = repo.find_commit(raw_oid) else {
            continue;
        };
        commits.push(commit_to_summary(&commit));
    }

    Ok(commits)
}

fn commit_to_summary(commit: &git2::Commit<'_>) -> CommitSummary {
    let parents = commit.parent_ids().map(to_core_oid).collect();
    let author = commit.author();
    CommitSummary {
        oid: to_core_oid(commit.id()),
        parents,
        author_name: author.name().unwrap_or("Unknown").to_string(),
        author_email: author.email().unwrap_or("").to_string(),
        timestamp: commit.time().seconds(),
        author_timestamp: author.when().seconds(),
        subject: commit.summary().ok().flatten().unwrap_or("").to_string(),
        body: commit
            .body()
            .ok()
            .flatten()
            .unwrap_or("")
            .trim()
            .to_string(),
    }
}

pub fn load_stage_status_sync(
    location: &RepoLocation,
    include_ignored: bool,
    token: &EpochToken,
) -> Result<StageStatusSnapshot, EngineError> {
    if token.is_stale() {
        return Err(EngineError::Cancelled);
    }

    if location.is_bare {
        return Ok(StageStatusSnapshot::default());
    }

    with_thread_repo(&location.git_dir, |repo| {
        if token.is_stale() {
            return Err(EngineError::Cancelled);
        }

        let mut opts = StatusOptions::new();
        opts.include_untracked(true)
            .recurse_untracked_dirs(true)
            .renames_head_to_index(true)
            .renames_index_to_workdir(true)
            .include_ignored(include_ignored);

        let statuses = repo.statuses(Some(&mut opts))?;

        if token.is_stale() {
            return Err(EngineError::Cancelled);
        }

        let mut snap = StageStatusSnapshot::default();
        for entry in statuses.iter() {
            let st = entry.status();
            let path = entry.path().unwrap_or_default().to_string();

            if st.contains(Status::CONFLICTED) {
                snap.conflicted.push(StageEntry {
                    path,
                    old_path: None,
                    kind: FileStatusKind::Conflicted,
                });
                continue;
            }

            if st.contains(Status::IGNORED) {
                snap.ignored.push(StageEntry {
                    path,
                    old_path: None,
                    kind: FileStatusKind::Ignored,
                });
                continue;
            }

            if let Some(kind) = index_status_kind(st) {
                let old_path = entry
                    .head_to_index()
                    .and_then(|d| d.old_file().path().map(|p| p.to_string_lossy().to_string()))
                    .filter(|old| old != &path);
                snap.staged.push(StageEntry {
                    path: path.clone(),
                    old_path,
                    kind,
                });
            }

            if st.contains(Status::WT_NEW) {
                snap.untracked.push(StageEntry {
                    path,
                    old_path: None,
                    kind: FileStatusKind::Untracked,
                });
            } else if let Some(kind) = worktree_status_kind(st) {
                let old_path = entry
                    .index_to_workdir()
                    .and_then(|d| d.old_file().path().map(|p| p.to_string_lossy().to_string()))
                    .filter(|old| old != &path);
                snap.unstaged.push(StageEntry {
                    path,
                    old_path,
                    kind,
                });
            } else if let Some(diff) = entry.index_to_workdir()
                && let Some(kind) = delta_to_kind(diff.status())
            {
                snap.unstaged.push(StageEntry {
                    path,
                    old_path: None,
                    kind,
                });
            }
        }

        Ok(snap)
    })
}

fn index_status_kind(st: Status) -> Option<FileStatusKind> {
    if st.contains(Status::INDEX_NEW) {
        Some(FileStatusKind::Added)
    } else if st.contains(Status::INDEX_MODIFIED) {
        Some(FileStatusKind::Modified)
    } else if st.contains(Status::INDEX_DELETED) {
        Some(FileStatusKind::Deleted)
    } else if st.contains(Status::INDEX_RENAMED) {
        Some(FileStatusKind::Renamed)
    } else if st.contains(Status::INDEX_TYPECHANGE) {
        Some(FileStatusKind::Typechange)
    } else {
        None
    }
}

fn worktree_status_kind(st: Status) -> Option<FileStatusKind> {
    if st.contains(Status::WT_MODIFIED) {
        Some(FileStatusKind::Modified)
    } else if st.contains(Status::WT_DELETED) {
        Some(FileStatusKind::Deleted)
    } else if st.contains(Status::WT_RENAMED) {
        Some(FileStatusKind::Renamed)
    } else if st.contains(Status::WT_TYPECHANGE) {
        Some(FileStatusKind::Typechange)
    } else {
        None
    }
}

pub(crate) fn delta_to_kind(delta: Delta) -> Option<FileStatusKind> {
    match delta {
        Delta::Added => Some(FileStatusKind::Added),
        Delta::Deleted => Some(FileStatusKind::Deleted),
        Delta::Modified => Some(FileStatusKind::Modified),
        Delta::Renamed => Some(FileStatusKind::Renamed),
        Delta::Copied => Some(FileStatusKind::Copied),
        Delta::Typechange => Some(FileStatusKind::Typechange),
        Delta::Untracked => Some(FileStatusKind::Untracked),
        Delta::Ignored => Some(FileStatusKind::Ignored),
        Delta::Conflicted => Some(FileStatusKind::Conflicted),
        _ => None,
    }
}
