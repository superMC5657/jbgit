use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread::{self, JoinHandle};

use crossbeam_channel::{Sender, bounded};
use git2::{BranchType, Delta, Repository, Sort, Status, StatusOptions};
use jbgit_core::{
    AmendCommitPreview, CommitChangesBundle, CommitSummary, ConflictDocument, DetailsKey,
    EpochToken, FileDiff, FileDiffKey, FileStatusKind, Oid, RefInfo, RefKind, RepoSnapshot,
    StageEntry, StageNodeKind, StageStatusSnapshot, StageTarget,
};

use crate::details::{load_commit_details_sync, load_file_diff_sync, load_path_matches_sync};
use crate::error::EngineError;
use crate::probe::{RepoLocation, probe_repo_state, to_core_oid};
use crate::stage::{
    CommitOutcome, CommitPrecheckInfo, CommitRequest, StageDiffKey, ThreeDocStageDiff,
    apply_stage_buffer_sync, discard_targets_sync, execute_commit_git2_sync,
    load_amend_preview_sync, load_stage_file_diff_sync, probe_commit_precheck_sync,
    stage_all_tracked_sync, stage_targets_sync, undo_last_commit_sync, unstage_targets_sync,
    write_worktree_buffer_sync,
};

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
                .name(format!("jbgit-git2-worker-{idx}"))
                .spawn(move || {
                    while let Ok(job) = rx.recv() {
                        job();
                    }
                })
                .expect("failed to spawn jbgit-git2-worker thread");
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

    pub async fn load_stage_file_diff(
        &self,
        location: RepoLocation,
        key: StageDiffKey,
        token: EpochToken,
    ) -> Result<Arc<ThreeDocStageDiff>, EngineError> {
        self.run(move || load_stage_file_diff_sync(&location, &key, &token).map(Arc::new))
            .await
    }

    pub async fn load_amend_preview(
        &self,
        location: RepoLocation,
    ) -> Result<Option<AmendCommitPreview>, EngineError> {
        self.run(move || load_amend_preview_sync(&location)).await
    }

    pub async fn probe_commit_precheck(&self, location: RepoLocation) -> CommitPrecheckInfo {
        self.run(move || Ok(probe_commit_precheck_sync(&location)))
            .await
            .unwrap_or_default()
    }

    pub async fn apply_stage_buffer(
        &self,
        location: RepoLocation,
        path: String,
        new_staged_bytes: Vec<u8>,
        file_mode: u32,
        remove_from_index: bool,
    ) -> Result<(), EngineError> {
        self.run(move || {
            apply_stage_buffer_sync(
                &location,
                &path,
                &new_staged_bytes,
                file_mode,
                remove_from_index,
            )
        })
        .await
    }

    pub async fn write_worktree_buffer(
        &self,
        location: RepoLocation,
        path: String,
        new_local_bytes: Vec<u8>,
    ) -> Result<(), EngineError> {
        self.run(move || write_worktree_buffer_sync(&location, &path, &new_local_bytes))
            .await
    }

    pub async fn stage_targets(
        &self,
        location: RepoLocation,
        targets: Vec<StageTarget>,
    ) -> Result<(), EngineError> {
        self.run(move || stage_targets_sync(&location, &targets))
            .await
    }

    pub async fn stage_all_tracked(&self, location: RepoLocation) -> Result<(), EngineError> {
        self.run(move || stage_all_tracked_sync(&location)).await
    }

    pub async fn unstage_targets(
        &self,
        location: RepoLocation,
        targets: Vec<StageTarget>,
    ) -> Result<(), EngineError> {
        self.run(move || unstage_targets_sync(&location, &targets))
            .await
    }

    pub async fn discard_targets(
        &self,
        location: RepoLocation,
        targets: Vec<StageTarget>,
    ) -> Result<(), EngineError> {
        self.run(move || discard_targets_sync(&location, &targets))
            .await
    }

    pub async fn execute_commit_git2(
        &self,
        location: RepoLocation,
        request: CommitRequest,
    ) -> Result<CommitOutcome, EngineError> {
        self.run(move || execute_commit_git2_sync(&location, &request))
            .await
    }

    pub async fn undo_last_commit(&self, location: RepoLocation) -> Result<String, EngineError> {
        self.run(move || undo_last_commit_sync(&location)).await
    }

    pub async fn load_conflict_document(
        &self,
        location: RepoLocation,
        path: String,
    ) -> Result<ConflictDocument, EngineError> {
        self.run(move || load_conflict_document_sync(&location, &path))
            .await
    }

    pub async fn save_conflict_resolution(
        &self,
        location: RepoLocation,
        path: String,
        content: Vec<u8>,
        stage: bool,
    ) -> Result<(), EngineError> {
        self.run(move || {
            write_worktree_buffer_sync(&location, &path, &content)?;
            if stage {
                stage_targets_sync(
                    &location,
                    &[StageTarget {
                        path,
                        old_path: None,
                        status_kind: FileStatusKind::Conflicted,
                        group_kind: StageNodeKind::Conflicted,
                    }],
                )?;
            }
            Ok(())
        })
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

fn load_conflict_document_sync(
    location: &RepoLocation,
    path: &str,
) -> Result<ConflictDocument, EngineError> {
    let worktree_bytes = std::fs::read(location.workdir.join(path)).unwrap_or_default();
    let worktree_text = String::from_utf8_lossy(&worktree_bytes);
    let (index_ours, index_theirs) = with_thread_repo(&location.git_dir, |repo| {
        let mut ours = None;
        let mut theirs = None;
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
                if entry_path.as_deref() == Some(path) {
                    if let Some(our) = item.our
                        && let Ok(blob) = repo.find_blob(our.id)
                    {
                        ours = Some(String::from_utf8_lossy(blob.content()).into_owned());
                    }
                    if let Some(their) = item.their
                        && let Ok(blob) = repo.find_blob(their.id)
                    {
                        theirs = Some(String::from_utf8_lossy(blob.content()).into_owned());
                    }
                    break;
                }
            }
        }
        Ok((ours, theirs))
    })?;
    Ok(ConflictDocument::parse(
        path,
        &worktree_text,
        index_ours.as_deref(),
        index_theirs.as_deref(),
    ))
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
            probe.state == jbgit_core::RepositoryState::Detached,
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

        let recent_branches = {
            let existing: Vec<&str> = refs
                .iter()
                .filter(|r| r.kind == RefKind::LocalBranch)
                .map(|r| r.name.as_str())
                .collect();
            let reflog = read_tail(&location.git_dir.join("logs").join("HEAD"), 64 * 1024);
            jbgit_core::parse_recent_checkouts(
                &reflog,
                probe.head_branch.as_deref(),
                &existing,
                50,
                5,
            )
        };
        let stashes = collect_stashes(repo);
        let ongoing_onto = ["rebase-merge", "rebase-apply"]
            .iter()
            .find_map(|d| std::fs::read_to_string(location.git_dir.join(d).join("onto")).ok())
            .and_then(|hex| hex.trim().parse::<Oid>().ok())
            .map(|oid| {
                refs.iter()
                    .filter(|r| r.target_oid == oid)
                    .min_by_key(|r| match r.kind {
                        RefKind::LocalBranch => 0,
                        RefKind::RemoteBranch => 1,
                        _ => 2,
                    })
                    .map_or_else(|| oid.short(), |r| r.name.clone())
            });

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
            recent_branches,
            stashes,
            ongoing_onto,
        })
    })
}

/// Reads at most the last `max` bytes of a text file (reflogs can be huge).
fn read_tail(path: &Path, max: u64) -> String {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut f) = std::fs::File::open(path) else {
        return String::new();
    };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    if len > max {
        let _ = f.seek(SeekFrom::Start(len - max));
    }
    let mut buf = Vec::new();
    let _ = f.read_to_end(&mut buf);
    let text = String::from_utf8_lossy(&buf).into_owned();
    if len > max {
        // Drop the (probably partial) first line.
        text.split_once('\n')
            .map(|(_, r)| r.to_string())
            .unwrap_or_default()
    } else {
        text
    }
}

/// `stash@{n}` entries from the `refs/stash` reflog (index 0 = newest).
pub(crate) fn collect_stashes(repo: &Repository) -> Vec<jbgit_core::StashEntry> {
    let Ok(reflog) = repo.reflog("refs/stash") else {
        return Vec::new();
    };
    reflog
        .iter()
        .enumerate()
        .map(|(index, entry)| jbgit_core::StashEntry {
            index,
            oid: to_core_oid(entry.id_new()),
            message: entry.message().ok().flatten().unwrap_or("").to_string(),
            timestamp: entry.committer().when().seconds(),
        })
        .collect()
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
        if let Ok(index) = repo.index()
            && let Ok(conflicts) = index.conflicts()
        {
            for item in conflicts.flatten() {
                let p = item
                    .our
                    .as_ref()
                    .or(item.their.as_ref())
                    .or(item.ancestor.as_ref())
                    .map(|e| String::from_utf8_lossy(&e.path).replace('\\', "/"));
                if let Some(path) = p {
                    let label = match (
                        item.ancestor.is_some(),
                        item.our.is_some(),
                        item.their.is_some(),
                    ) {
                        (true, true, true) => "Both Modified",
                        (false, true, true) => "Added by Both",
                        (true, true, false) => "Deleted by Them",
                        (true, false, true) => "Deleted by Us",
                        (true, false, false) => "Both Deleted",
                        (false, true, false) => "Added by Us",
                        (false, false, true) => "Added by Them",
                        (false, false, false) => "Conflicted",
                    };
                    snap.conflict_labels.insert(path, label.to_string());
                }
            }
        }
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
