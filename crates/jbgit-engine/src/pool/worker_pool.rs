use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread::{self, JoinHandle};

use crossbeam_channel::Sender;
use git2::Repository;
use jbgit_core::{
    AmendCommitPreview, CommitChangesBundle, ConflictDocument, DetailsKey, EpochToken, FileDiff,
    FileDiffKey, FileHistoryBundle, FileHistoryKey, FileStatusKind, Oid, RepoSnapshot,
    StageNodeKind, StageStatusSnapshot, StageTarget,
};

use crate::details::{
    load_commit_details_sync, load_file_diff_with_oids_sync, load_file_history_sync,
    load_path_matches_sync,
};
use crate::error::EngineError;
use crate::probe::RepoLocation;
use crate::stage::{
    CommitOutcome, CommitPrecheckInfo, CommitRequest, StageDiffKey, ThreeDocStageDiff,
    apply_stage_buffer_sync, discard_targets_sync, execute_commit_git2_sync,
    load_amend_preview_sync, load_stage_file_diff_sync, probe_commit_precheck_sync,
    stage_all_tracked_sync, stage_targets_sync, undo_last_commit_sync, unstage_targets_sync,
    write_worktree_buffer_sync,
};

use super::scan_config::{AheadBehindUpdate, PendingAheadBehind, StatusScanConfig};
use super::snapshot::{
    compute_ahead_behind_batch_sync, load_conflict_document_sync,
    load_repo_snapshot_with_config_sync,
};
use super::stage_status::load_stage_status_with_config_sync;

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

/// Scheduling class of a [`Git2WorkerPool`] job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskPriority {
    Interactive,
    Background,
}

/// Fixed-size worker pool caching `git2::Repository` handles in `thread_local!`
/// so concurrent read queries avoid re-opening `.git/config` and packfile indices.
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
            let (snap, pending) =
                load_repo_snapshot_with_config_sync(&location, token, max_commits, scan_config)?;
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
        self.run_with(TaskPriority::Background, move || {
            load_amend_preview_sync(&location)
        })
        .await
    }

    pub async fn probe_commit_precheck(
        &self,
        location: RepoLocation,
        priority: TaskPriority,
    ) -> CommitPrecheckInfo {
        self.run_with(priority, move || Ok(probe_commit_precheck_sync(&location)))
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

    pub(crate) async fn run_epoch<T, F>(
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
