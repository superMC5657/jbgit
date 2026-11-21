use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread::{self, JoinHandle};

use crossbeam_channel::Sender;
use git2::{BranchType, Delta, Repository, Sort, Status, StatusOptions};
use jbgit_core::{
    AmendCommitPreview, CommitChangesBundle, CommitSummary, ConflictDocument, DetailsKey,
    EpochToken, FileDiff, FileDiffKey, FileHistoryBundle, FileHistoryKey, FileStatusKind, Oid,
    RefInfo, RefKind, RepoSnapshot, StageEntry, StageNodeKind, StageStatusSnapshot, StageTarget,
};

use crate::details::{
    load_commit_details_sync, load_file_diff_with_oids_sync, load_file_history_sync,
    load_path_matches_sync,
};
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

/// Index size above which worktree-side rename detection is skipped during the
/// status scan. Pairing every deleted file with every untracked file is the
/// dominant cost on large (especially Windows) worktrees; staged renames
/// (`HEAD -> index`) are still detected.
pub const LARGE_INDEX_ENTRIES: usize = 20_000;
/// Local branches (besides `HEAD`, always computed) that get an ahead/behind
/// count per snapshot, most recently committed first.
pub const AHEAD_BEHIND_BRANCH_LIMIT: usize = 32;
/// Wall-clock budget for those extra ahead/behind walks per snapshot.
pub const AHEAD_BEHIND_TIME_BUDGET: std::time::Duration = std::time::Duration::from_millis(150);

/// Strategy for an optional status-scan pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StatusScanMode {
    /// Enable on repositories up to `large_index_entries`, skip above it.
    #[default]
    Auto,
    Always,
    Never,
}

impl StatusScanMode {
    pub fn resolve(self, entries: usize, threshold: usize) -> bool {
        match self {
            Self::Auto => entries <= threshold,
            Self::Always => true,
            Self::Never => false,
        }
    }
}

/// Configurable performance knobs for repository snapshot and worktree status scans.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StatusScanConfig {
    pub large_index_entries: usize,
    pub worktree_renames: StatusScanMode,
    pub recurse_untracked_dirs: StatusScanMode,
    pub load_commit_bodies: bool,
}

impl Default for StatusScanConfig {
    fn default() -> Self {
        Self {
            large_index_entries: LARGE_INDEX_ENTRIES,
            worktree_renames: StatusScanMode::Auto,
            recurse_untracked_dirs: StatusScanMode::Always,
            load_commit_bodies: true,
        }
    }
}

impl StatusScanConfig {
    pub fn policy(&self, index_entries: usize) -> StatusScanPolicy {
        StatusScanPolicy {
            worktree_renames: self
                .worktree_renames
                .resolve(index_entries, self.large_index_entries),
            recurse_untracked_dirs: self
                .recurse_untracked_dirs
                .resolve(index_entries, self.large_index_entries),
        }
    }
}

/// Status-scan options chosen for a repository of a given size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StatusScanPolicy {
    pub worktree_renames: bool,
    pub recurse_untracked_dirs: bool,
}

impl StatusScanPolicy {
    pub fn for_index_entries(entries: usize) -> Self {
        StatusScanConfig::default().policy(entries)
    }
}

/// Backfilled `(full_ref_name, (ahead, behind))` pair.
pub type AheadBehindUpdate = (String, (usize, usize));

/// A local tracking branch whose `ahead_behind` walk was deferred past the initial
/// snapshot budget so cold start stays bounded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingAheadBehind {
    pub full_name: String,
    pub local: Oid,
    pub upstream: Oid,
}

/// Scheduling class of a [`Git2WorkerPool`] job.
///
/// `Interactive` jobs (diff previews, commit details, stage buffer writes the user
/// is waiting on) are always dequeued before `Background` jobs (snapshot reloads,
/// status scans, path-filter walks) and additionally own a reserved worker that
/// never picks up background work, so a queue full of heavy scans cannot starve
/// an interactive request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskPriority {
    Interactive,
    Background,
}

/// Fixed-size worker pool caching `git2::Repository` handles in `thread_local!`
/// so concurrent read queries avoid re-opening `.git/config` and packfile indices.
///
/// Jobs flow through two queues (`high` / `bg`). Worker `0` is reserved for the
/// interactive queue whenever the pool has more than one thread; the remaining
/// workers drain both queues with `select_biased!`, always preferring `high`.
pub struct Git2WorkerPool {
    high_tx: Option<Sender<JobFn>>,
    bg_tx: Option<Sender<JobFn>>,
    handles: Vec<JoinHandle<()>>,
}

impl Git2WorkerPool {
    pub fn new(num_threads: usize) -> Self {
        let count = num_threads.clamp(1, 8);
        let reserved_interactive = usize::from(count > 1);
        let (high_tx, high_rx) = crossbeam_channel::unbounded::<JobFn>();
        let (bg_tx, bg_rx) = crossbeam_channel::unbounded::<JobFn>();
        let mut handles = Vec::with_capacity(count);

        for idx in 0..count {
            let high_rx = high_rx.clone();
            let bg_rx = bg_rx.clone();
            let interactive_only = idx < reserved_interactive;
            let handle = thread::Builder::new()
                .name(format!("jbgit-git2-worker-{idx}"))
                .spawn(move || {
                    if interactive_only {
                        drop(bg_rx);
                        while let Ok(job) = high_rx.recv() {
                            job();
                        }
                    } else {
                        biased_worker_loop(&high_rx, &bg_rx);
                    }
                })
                .expect("failed to spawn jbgit-git2-worker thread");
            handles.push(handle);
        }

        Self {
            high_tx: Some(high_tx),
            bg_tx: Some(bg_tx),
            handles,
        }
    }

    /// Enqueues an interactive job (kept for callers predating priorities).
    pub fn spawn_task<F>(&self, f: F)
    where
        F: FnOnce() + Send + 'static,
    {
        self.spawn_with_priority(TaskPriority::Interactive, f);
    }

    pub fn spawn_with_priority<F>(&self, priority: TaskPriority, f: F)
    where
        F: FnOnce() + Send + 'static,
    {
        let tx = match priority {
            TaskPriority::Interactive => &self.high_tx,
            TaskPriority::Background => &self.bg_tx,
        };
        if let Some(tx) = tx {
            let _ = tx.send(Box::new(f));
        }
    }

    pub async fn load_repo_snapshot(
        &self,
        location: RepoLocation,
        token: EpochToken,
        max_commits: usize,
    ) -> Result<Arc<RepoSnapshot>, EngineError> {
        let (snap, _) = self
            .load_repo_snapshot_with_pending(
                location,
                token,
                max_commits,
                StatusScanConfig::default(),
            )
            .await?;
        Ok(snap)
    }

    pub async fn load_repo_snapshot_with_pending(
        &self,
        location: RepoLocation,
        token: EpochToken,
        max_commits: usize,
        scan_config: StatusScanConfig,
    ) -> Result<(Arc<RepoSnapshot>, Vec<PendingAheadBehind>), EngineError> {
        self.run_epoch(TaskPriority::Background, token, move |token| {
            let (snap, pending) = load_repo_snapshot_with_config_sync(
                &location,
                token,
                max_commits,
                scan_config,
            )?;
            Ok((Arc::new(snap), pending))
        })
        .await
    }

    pub async fn compute_ahead_behind_batch(
        &self,
        location: RepoLocation,
        pending: Vec<PendingAheadBehind>,
        token: EpochToken,
    ) -> Result<Vec<AheadBehindUpdate>, EngineError> {
        self.run_epoch(TaskPriority::Background, token, move |token| {
            compute_ahead_behind_batch_sync(&location, &pending, token)
        })
        .await
    }

    pub async fn load_stage_status(
        &self,
        location: RepoLocation,
        include_ignored: bool,
        token: EpochToken,
    ) -> Result<Arc<StageStatusSnapshot>, EngineError> {
        self.load_stage_status_with_config(
            location,
            include_ignored,
            StatusScanConfig::default(),
            token,
        )
        .await
    }

    pub async fn load_stage_status_with_config(
        &self,
        location: RepoLocation,
        include_ignored: bool,
        scan_config: StatusScanConfig,
        token: EpochToken,
    ) -> Result<Arc<StageStatusSnapshot>, EngineError> {
        self.run_epoch(TaskPriority::Background, token, move |token| {
            load_stage_status_with_config_sync(&location, include_ignored, scan_config, token)
                .map(Arc::new)
        })
        .await
    }

    pub async fn load_commit_details(
        &self,
        location: RepoLocation,
        key: DetailsKey,
        token: EpochToken,
    ) -> Result<Arc<CommitChangesBundle>, EngineError> {
        self.run_epoch(TaskPriority::Interactive, token, move |token| {
            load_commit_details_sync(&location, &key, token).map(Arc::new)
        })
        .await
    }

    pub async fn load_file_diff(
        &self,
        location: RepoLocation,
        key: FileDiffKey,
        known_oids: Option<(Option<Oid>, Option<Oid>)>,
        token: EpochToken,
    ) -> Result<Arc<FileDiff>, EngineError> {
        self.run_epoch(TaskPriority::Interactive, token, move |token| {
            load_file_diff_with_oids_sync(&location, &key, known_oids, token).map(Arc::new)
        })
        .await
    }

    pub async fn load_file_history(
        &self,
        location: RepoLocation,
        key: FileHistoryKey,
        token: EpochToken,
    ) -> Result<Arc<FileHistoryBundle>, EngineError> {
        self.run_epoch(TaskPriority::Interactive, token, move |token| {
            load_file_history_sync(&location, &key, token).map(Arc::new)
        })
        .await
    }

    pub async fn load_path_matches(
        &self,
        location: RepoLocation,
        paths: Vec<String>,
        snapshot: Arc<RepoSnapshot>,
        token: EpochToken,
    ) -> Result<Arc<HashSet<Oid>>, EngineError> {
        self.run_epoch(TaskPriority::Background, token, move |token| {
            load_path_matches_sync(&location, &paths, &snapshot, token).map(Arc::new)
        })
        .await
    }

    pub async fn load_stage_file_diff(
        &self,
        location: RepoLocation,
        key: StageDiffKey,
        token: EpochToken,
    ) -> Result<Arc<ThreeDocStageDiff>, EngineError> {
        self.run_epoch(TaskPriority::Interactive, token, move |token| {
            load_stage_file_diff_sync(&location, &key, token).map(Arc::new)
        })
        .await
    }

    pub async fn load_amend_preview(
        &self,
        location: RepoLocation,
    ) -> Result<Option<AmendCommitPreview>, EngineError> {
        self.run_with(TaskPriority::Background, move || load_amend_preview_sync(&location))
            .await
    }

    /// Hook / GPG probe. `Background` for the passive workbench refresh,
    /// `Interactive` when a commit the user just submitted is waiting on it.
    pub async fn probe_commit_precheck(
        &self,
        location: RepoLocation,
        priority: TaskPriority,
    ) -> CommitPrecheckInfo {
        self.run_with(priority, move || {
            Ok(probe_commit_precheck_sync(&location))
        })
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

    /// Interactive one-shot job (user-initiated writes and loads).
    async fn run<T, F>(&self, f: F) -> Result<T, EngineError>
    where
        T: Send + 'static,
        F: FnOnce() -> Result<T, EngineError> + Send + 'static,
    {
        self.run_with(TaskPriority::Interactive, f).await
    }

    async fn run_with<T, F>(&self, priority: TaskPriority, f: F) -> Result<T, EngineError>
    where
        T: Send + 'static,
        F: FnOnce() -> Result<T, EngineError> + Send + 'static,
    {
        let (done_tx, done_rx) = tokio::sync::oneshot::channel();
        self.spawn_with_priority(priority, move || {
            let _ = done_tx.send(f());
        });
        done_rx
            .await
            .map_err(|_| EngineError::Other("git2 worker dropped".to_string()))?
    }

    /// Epoch-guarded job: a request superseded while still queued is dropped the
    /// moment a worker dequeues it, without touching the repository.
    async fn run_epoch<T, F>(
        &self,
        priority: TaskPriority,
        token: EpochToken,
        f: F,
    ) -> Result<T, EngineError>
    where
        T: Send + 'static,
        F: FnOnce(&EpochToken) -> Result<T, EngineError> + Send + 'static,
    {
        self.run_with(priority, move || {
            if token.is_stale() {
                return Err(EngineError::Cancelled);
            }
            f(&token)
        })
        .await
    }
}

/// Worker loop shared by the non-reserved threads: always drain `high` first.
/// Once either queue disconnects (pool drop), the other is drained to completion
/// so already-submitted jobs still deliver their results.
fn biased_worker_loop(
    high_rx: &crossbeam_channel::Receiver<JobFn>,
    bg_rx: &crossbeam_channel::Receiver<JobFn>,
) {
    loop {
        crossbeam_channel::select_biased! {
            recv(high_rx) -> msg => match msg {
                Ok(job) => job(),
                Err(_) => {
                    while let Ok(job) = bg_rx.recv() {
                        job();
                    }
                    return;
                }
            },
            recv(bg_rx) -> msg => match msg {
                Ok(job) => job(),
                Err(_) => {
                    while let Ok(job) = high_rx.recv() {
                        job();
                    }
                    return;
                }
            },
        }
    }
}

impl Drop for Git2WorkerPool {
    fn drop(&mut self) {
        self.high_tx.take();
        self.bg_tx.take();
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
    let (snap, _) = load_repo_snapshot_with_config_sync(
        location,
        token,
        max_commits,
        StatusScanConfig::default(),
    )?;
    Ok(snap)
}

pub fn load_repo_snapshot_with_config_sync(
    location: &RepoLocation,
    token: &EpochToken,
    max_commits: usize,
    scan_config: StatusScanConfig,
) -> Result<(RepoSnapshot, Vec<PendingAheadBehind>), EngineError> {
    if token.is_stale() {
        return Err(EngineError::Cancelled);
    }

    with_thread_repo(&location.git_dir, |repo| {
        if token.is_stale() {
            return Err(EngineError::Cancelled);
        }

        let probe = probe_repo_state(repo);
        let (refs, pending_ahead_behind) = collect_refs(
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
            collect_commits(repo, token, max_commits, scan_config.load_commit_bodies)?
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

        Ok((
            RepoSnapshot {
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
            },
            pending_ahead_behind,
        ))
    })
}

pub fn compute_ahead_behind_batch_sync(
    location: &RepoLocation,
    pending: &[PendingAheadBehind],
    token: &EpochToken,
) -> Result<Vec<AheadBehindUpdate>, EngineError> {
    if token.is_stale() {
        return Err(EngineError::Cancelled);
    }
    if pending.is_empty() {
        return Ok(Vec::new());
    }
    with_thread_repo(&location.git_dir, |repo| {
        let mut out = Vec::new();
        for (idx, item) in pending.iter().enumerate() {
            if idx % 16 == 0 && token.is_stale() {
                return Err(EngineError::Cancelled);
            }
            let (Ok(local), Ok(upstream)) = (
                git2::Oid::from_bytes(item.local.as_bytes()),
                git2::Oid::from_bytes(item.upstream.as_bytes()),
            ) else {
                continue;
            };
            if let Ok(ab) = repo.graph_ahead_behind(local, upstream) {
                out.push((item.full_name.clone(), ab));
            }
        }
        Ok(out)
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

struct AheadBehindJob {
    ref_idx: usize,
    full_name: String,
    local: git2::Oid,
    upstream: git2::Oid,
    tip_time: i64,
    is_head: bool,
}

fn collect_refs(
    repo: &Repository,
    head_branch: &Option<String>,
    is_detached: bool,
) -> Result<(Vec<RefInfo>, Vec<PendingAheadBehind>), EngineError> {
    let mut out = Vec::new();
    let mut ahead_behind_jobs: Vec<AheadBehindJob> = Vec::new();

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

            let (kind, upstream, is_head) = match branch_type {
                BranchType::Local => {
                    let is_head = !is_detached && head_branch.as_deref() == Some(name.as_str());
                    let upstream_name = match branch.upstream() {
                        Ok(up) => {
                            if let Ok(up_commit) = up.get().peel_to_commit() {
                                ahead_behind_jobs.push(AheadBehindJob {
                                    ref_idx: out.len(),
                                    full_name: full_name.clone(),
                                    local: commit.id(),
                                    upstream: up_commit.id(),
                                    tip_time: commit.time().seconds(),
                                    is_head,
                                });
                            }
                            up.name().ok().flatten().map(ToString::to_string)
                        }
                        Err(_) => None,
                    };
                    (RefKind::LocalBranch, upstream_name, is_head)
                }
                BranchType::Remote => (RefKind::RemoteBranch, None, false),
            };

            out.push(RefInfo {
                name,
                full_name,
                target_oid,
                kind,
                upstream,
                ahead_behind: None,
                is_head,
            });
        }
    }

    // `graph_ahead_behind` walks history per branch; repositories with hundreds
    // of stale local branches would pay that on every refresh. HEAD is always
    // counted, then the most recently committed branches within a fixed budget;
    // the remainder are deferred to a background backfill pass.
    ahead_behind_jobs.sort_by_key(|j| (!j.is_head, std::cmp::Reverse(j.tip_time)));
    let started = std::time::Instant::now();
    let mut extra = 0usize;
    let mut deferred = Vec::new();
    let mut budget_exhausted = false;
    for job in ahead_behind_jobs {
        if !job.is_head {
            if budget_exhausted
                || extra >= AHEAD_BEHIND_BRANCH_LIMIT
                || started.elapsed() > AHEAD_BEHIND_TIME_BUDGET
            {
                budget_exhausted = true;
                deferred.push(PendingAheadBehind {
                    full_name: job.full_name,
                    local: to_core_oid(job.local),
                    upstream: to_core_oid(job.upstream),
                });
                continue;
            }
            extra += 1;
        }
        if let Some(info) = out.get_mut(job.ref_idx) {
            info.ahead_behind = repo.graph_ahead_behind(job.local, job.upstream).ok();
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

    Ok((out, deferred))
}

fn collect_commits(
    repo: &Repository,
    token: &EpochToken,
    max_commits: usize,
    load_commit_bodies: bool,
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
        commits.push(commit_to_summary(&commit, load_commit_bodies));
    }

    Ok(commits)
}

fn commit_to_summary(commit: &git2::Commit<'_>, load_body: bool) -> CommitSummary {
    let parents = commit.parent_ids().map(to_core_oid).collect();
    let author = commit.author();
    let body = if load_body {
        commit
            .body()
            .ok()
            .flatten()
            .unwrap_or("")
            .trim()
            .to_string()
    } else {
        String::new()
    };
    CommitSummary {
        oid: to_core_oid(commit.id()),
        parents,
        author_name: author.name().unwrap_or("Unknown").to_string(),
        author_email: author.email().unwrap_or("").to_string(),
        timestamp: commit.time().seconds(),
        author_timestamp: author.when().seconds(),
        subject: commit.summary().ok().flatten().unwrap_or("").to_string(),
        body,
    }
}

pub fn load_stage_status_sync(
    location: &RepoLocation,
    include_ignored: bool,
    token: &EpochToken,
) -> Result<StageStatusSnapshot, EngineError> {
    load_stage_status_with_config_sync(
        location,
        include_ignored,
        StatusScanConfig::default(),
        token,
    )
}

pub fn load_stage_status_with_config_sync(
    location: &RepoLocation,
    include_ignored: bool,
    scan_config: StatusScanConfig,
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

        let index_entries = repo.index().map(|idx| idx.len()).unwrap_or(0);
        let policy = scan_config.policy(index_entries);
        let mut opts = StatusOptions::new();
        opts.include_untracked(true)
            .recurse_untracked_dirs(policy.recurse_untracked_dirs)
            .renames_head_to_index(true)
            .renames_index_to_workdir(policy.worktree_renames)
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

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use jbgit_core::RequestEpoch;

    use super::*;

    #[test]
    fn interactive_job_runs_while_background_queue_is_saturated() {
        let pool = Git2WorkerPool::new(4);
        let (gate_tx, gate_rx) = crossbeam_channel::bounded::<()>(0);
        let started = Arc::new(AtomicUsize::new(0));

        // Far more blocking background jobs than workers.
        for _ in 0..12 {
            let gate_rx = gate_rx.clone();
            let started = Arc::clone(&started);
            pool.spawn_with_priority(TaskPriority::Background, move || {
                started.fetch_add(1, Ordering::SeqCst);
                let _ = gate_rx.recv();
            });
        }

        let (done_tx, done_rx) = crossbeam_channel::bounded(1);
        pool.spawn_with_priority(TaskPriority::Interactive, move || {
            let _ = done_tx.send(());
        });
        assert!(
            done_rx.recv_timeout(Duration::from_secs(5)).is_ok(),
            "interactive job starved behind background work"
        );
        // The reserved interactive worker never picked up a background job.
        assert!(started.load(Ordering::SeqCst) <= 3);

        drop(gate_tx);
        drop(pool);
        assert_eq!(started.load(Ordering::SeqCst), 12);
    }

    #[test]
    fn single_worker_prefers_interactive_over_queued_background() {
        let pool = Git2WorkerPool::new(1);
        let order = Arc::new(Mutex::new(Vec::new()));
        let (started_tx, started_rx) = crossbeam_channel::bounded(1);
        let (gate_tx, gate_rx) = crossbeam_channel::bounded::<()>(0);

        {
            let order = Arc::clone(&order);
            pool.spawn_with_priority(TaskPriority::Background, move || {
                let _ = started_tx.send(());
                let _ = gate_rx.recv();
                order.lock().unwrap().push("bg-blocker");
            });
        }
        started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        for label in ["bg-1", "bg-2"] {
            let order = Arc::clone(&order);
            pool.spawn_with_priority(TaskPriority::Background, move || {
                order.lock().unwrap().push(label);
            });
        }
        {
            let order = Arc::clone(&order);
            pool.spawn_with_priority(TaskPriority::Interactive, move || {
                order.lock().unwrap().push("interactive");
            });
        }
        drop(gate_tx);
        drop(pool);
        assert_eq!(
            *order.lock().unwrap(),
            vec!["bg-blocker", "interactive", "bg-1", "bg-2"]
        );
    }

    #[test]
    fn status_policy_drops_worktree_renames_on_large_index() {
        assert!(StatusScanPolicy::for_index_entries(0).worktree_renames);
        assert!(StatusScanPolicy::for_index_entries(LARGE_INDEX_ENTRIES).worktree_renames);
        assert!(!StatusScanPolicy::for_index_entries(LARGE_INDEX_ENTRIES + 1).worktree_renames);
        assert!(StatusScanPolicy::for_index_entries(LARGE_INDEX_ENTRIES + 1).recurse_untracked_dirs);

        let custom = StatusScanConfig {
            large_index_entries: 100,
            worktree_renames: StatusScanMode::Always,
            recurse_untracked_dirs: StatusScanMode::Auto,
            load_commit_bodies: false,
        };
        let small = custom.policy(50);
        assert!(small.worktree_renames);
        assert!(small.recurse_untracked_dirs);
        let big = custom.policy(150);
        assert!(big.worktree_renames);
        assert!(!big.recurse_untracked_dirs);
    }

    #[test]
    fn collect_refs_defers_excess_ahead_behind_and_backfills_in_batch() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let repo = Repository::init(root).unwrap();
        {
            let mut cfg = repo.config().unwrap();
            cfg.set_str("user.name", "Test").unwrap();
            cfg.set_str("user.email", "test@example.com").unwrap();
        }
        repo.remote("origin", "https://example.com/repo.git").unwrap();
        std::fs::write(root.join("a.txt"), "hello\n").unwrap();
        let mut idx = repo.index().unwrap();
        idx.add_path(Path::new("a.txt")).unwrap();
        let tree_id = idx.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let sig = git2::Signature::now("Test", "test@example.com").unwrap();
        let c1 = repo
            .commit(Some("HEAD"), &sig, &sig, "init\n\nbody text", &tree, &[])
            .unwrap();
        let c1_commit = repo.find_commit(c1).unwrap();

        // Create remote tracking targets and > AHEAD_BEHIND_BRANCH_LIMIT local branches.
        let total_extra = AHEAD_BEHIND_BRANCH_LIMIT + 5;
        for i in 0..total_extra {
            let remote_ref = format!("refs/remotes/origin/b-{i:02}");
            repo.reference(&remote_ref, c1, true, "remote").unwrap();
            let mut branch = repo.branch(&format!("b-{i:02}"), &c1_commit, true).unwrap();
            branch.set_upstream(Some(&format!("origin/b-{i:02}"))).unwrap();
        }
        // Create one more commit on b-00 so its ahead count is 1.
        std::fs::write(root.join("a.txt"), "v2\n").unwrap();
        idx.add_path(Path::new("a.txt")).unwrap();
        let tree2 = repo.find_tree(idx.write_tree().unwrap()).unwrap();
        let c2 = repo
            .commit(Some("refs/heads/b-00"), &sig, &sig, "second\n\nsecond body", &tree2, &[&c1_commit])
            .unwrap();
        let _ = c2;

        let loc = crate::probe::RepoLocation::discover(root).unwrap();
        let epoch = RequestEpoch::new();
        let token = epoch.issue();
        let (snap_no_body, pending) = load_repo_snapshot_with_config_sync(
            &loc,
            &token,
            100,
            StatusScanConfig {
                load_commit_bodies: false,
                ..Default::default()
            },
        )
        .unwrap();
        assert!(snap_no_body.commits.iter().all(|c| c.body.is_empty()));
        assert_eq!(pending.len(), 5, "excess branches deferred for backfill");

        let updates = compute_ahead_behind_batch_sync(&loc, &pending, &token).unwrap();
        assert_eq!(updates.len(), 5);
        assert!(updates.iter().all(|(_, ab)| *ab == (0, 0)));
    }

    #[tokio::test]
    async fn superseded_epoch_job_is_skipped_at_dequeue() {
        let pool = Git2WorkerPool::new(2);
        let tracker = RequestEpoch::new();
        let stale = tracker.issue();
        let _newer = tracker.issue();
        let ran = Arc::new(AtomicUsize::new(0));
        let ran_job = Arc::clone(&ran);
        let res = pool
            .run_epoch(TaskPriority::Background, stale, move |_| {
                ran_job.fetch_add(1, Ordering::SeqCst);
                Ok(())
            })
            .await;
        assert!(matches!(res, Err(EngineError::Cancelled)));
        assert_eq!(ran.load(Ordering::SeqCst), 0);
    }
}
