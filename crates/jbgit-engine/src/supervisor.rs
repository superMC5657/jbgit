use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex};

use jbgit_core::{
    AmendCommitPreview, BlameBundle, BlameKey, CommitChangesBundle, ConflictDocument, ConsoleStream,
    DetailsKey, EpochToken, FileDiff, FileDiffKey, FileHistoryBundle, FileHistoryKey,
    FileStatusKind, GitConsoleEntry, GitOp, GitOpOutcome, LruCache, Oid, OutgoingBundle,
    OutgoingKey, RefreshScope, RepoSnapshot, RequestSlotId, SignatureStatus, SlotRegistry,
    StageNodeKind, StageStatusSnapshot, StageTarget,
};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

use crate::cli::{CliCommand, CliStreamEvent, GitCliExecutor, GitExecutableInfo};
use crate::details::{
    check_remote_tag_async, load_blame_sync, load_commit_details_sync,
    load_file_diff_with_oids_sync, load_file_history_sync, load_outgoing_sync,
    load_path_matches_sync, verify_commit_signature_sync,
};
use crate::error::EngineError;
use crate::pool::{
    AheadBehindUpdate, Git2WorkerPool, StatusScanConfig, TaskPriority,
    compute_ahead_behind_batch_sync, load_conflict_document_sync,
    load_repo_snapshot_with_config_sync, load_stage_status_with_config_sync,
};
use crate::probe::RepoLocation;
use crate::stage::{
    CommitOutcome, CommitPrecheckInfo, CommitRequest, StageDiffKey, ThreeDocStageDiff,
    apply_stage_buffer_sync, discard_targets_sync, execute_commit_cli_async,
    execute_commit_git2_sync, load_amend_preview_sync, load_stage_file_diff_sync,
    probe_commit_precheck_sync, stage_all_tracked_sync, stage_targets_sync, undo_last_commit_sync,
    unstage_targets_sync, write_worktree_buffer_sync,
};
use crate::watcher::{MutationGuard, RepoWatcher};

const DEFAULT_MAX_COMMITS: usize = 50_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlameFailureKind {
    TooLarge { limit: usize },
    Binary,
    Other,
}

#[derive(Debug, Clone)]
pub enum TaskRequest {
    LoadRepoSnapshot,
    /// Commit metadata + (net) changed-file list for the selected commits.
    LoadCommitDetails {
        slot: RequestSlotId,
        key: DetailsKey,
    },
    /// Line diff of one file of a details selection (diff preview).
    LoadFileDiff {
        slot: RequestSlotId,
        key: FileDiffKey,
        known_oids: Option<(Option<Oid>, Option<Oid>)>,
    },
    /// Commit history of a single file (`--follow` rename tracking or `-L` line range).
    LoadFileHistory {
        key: FileHistoryKey,
    },
    /// Per-line blame annotation (`git blame --porcelain`).
    LoadBlame {
        key: BlameKey,
    },
    /// Silent check whether `refs/tags/<tag>` exists on `remote`.
    CheckRemoteTag {
        remote: String,
        tag: String,
    },
    /// Pending commits and net file count for the Push dialog.
    LoadOutgoing {
        key: OutgoingKey,
    },
    /// Commits of `snapshot` touching any of `paths` (log `Paths` filter).
    LoadPathMatches {
        paths: Vec<String>,
        snapshot: Arc<RepoSnapshot>,
    },
    LoadStageStatus {
        include_ignored: bool,
    },
    LoadStageFileDiff {
        key: StageDiffKey,
    },
    LoadAmendPreview,
    ProbeCommitPrecheck,
    ApplyStageBuffer {
        path: String,
        new_staged_bytes: Vec<u8>,
        file_mode: u32,
        remove_from_index: bool,
    },
    WriteWorktreeBuffer {
        path: String,
        new_local_bytes: Vec<u8>,
    },
    StageTargets {
        targets: Vec<StageTarget>,
    },
    StageAllTracked,
    UnstageTargets {
        targets: Vec<StageTarget>,
    },
    DiscardTargets {
        targets: Vec<StageTarget>,
    },
    ExecuteCommit {
        id: u64,
        request: CommitRequest,
    },
    UndoLastCommit,
    RunGitCli {
        id: u64,
        args: Vec<String>,
        stdin_payload: Option<Vec<u8>>,
        is_read_only: bool,
        refresh_after: RefreshScope,
    },
    /// High-level write operation (checkout, merge, rebase, reset, stash, …).
    RunGitOp {
        op: GitOp,
    },
    LoadConflictDocument {
        path: String,
    },
    SaveConflictResolution {
        path: String,
        content: Vec<u8>,
        stage: bool,
    },
}

impl TaskRequest {
    /// Returns the consumer slot whose single-flight epoch governs this request, if any.
    pub const fn slot(&self) -> Option<RequestSlotId> {
        match self {
            Self::LoadRepoSnapshot => Some(RequestSlotId::RepoSnapshot),
            Self::LoadCommitDetails { slot, .. } => Some(*slot),
            Self::LoadFileDiff { slot, .. } => Some(*slot),
            Self::LoadFileHistory { .. } => Some(RequestSlotId::FileHistoryList),
            Self::LoadBlame { .. } => Some(RequestSlotId::Annotate),
            Self::CheckRemoteTag { .. } => Some(RequestSlotId::RemoteTagCheck),
            Self::LoadOutgoing { .. } => Some(RequestSlotId::PushOutgoing),
            Self::LoadPathMatches { .. } => Some(RequestSlotId::PathFilter),
            Self::LoadStageStatus { .. } => Some(RequestSlotId::StageStatus),
            Self::LoadStageFileDiff { .. } => Some(RequestSlotId::StageFileDiff),
            Self::LoadAmendPreview => Some(RequestSlotId::AmendPreview),
            Self::LoadConflictDocument { .. } => Some(RequestSlotId::ConflictDocument),
            Self::ProbeCommitPrecheck
            | Self::ApplyStageBuffer { .. }
            | Self::WriteWorktreeBuffer { .. }
            | Self::StageTargets { .. }
            | Self::StageAllTracked
            | Self::UnstageTargets { .. }
            | Self::DiscardTargets { .. }
            | Self::ExecuteCommit { .. }
            | Self::UndoLastCommit
            | Self::RunGitCli { .. }
            | Self::RunGitOp { .. }
            | Self::SaveConflictResolution { .. } => None,
        }
    }
}

#[derive(Debug, Clone)]
pub enum EngineEvent {
    RepoSnapshotLoaded {
        snapshot: Arc<RepoSnapshot>,
        epoch: u64,
    },
    /// Delayed `ahead_behind` counts for tracking branches skipped by the
    /// initial snapshot budget.
    RefAheadBehindBackfilled {
        epoch: u64,
        updates: Vec<AheadBehindUpdate>,
    },
    CommitDetailsLoaded {
        slot: RequestSlotId,
        bundle: Arc<CommitChangesBundle>,
        epoch: u64,
    },
    CommitSignatureVerified {
        oid: Oid,
        status: SignatureStatus,
    },
    FileDiffLoaded {
        slot: RequestSlotId,
        diff: Arc<FileDiff>,
        epoch: u64,
    },
    FileHistoryLoaded {
        bundle: Arc<FileHistoryBundle>,
        epoch: u64,
    },
    BlameLoaded {
        bundle: Arc<BlameBundle>,
        epoch: u64,
    },
    BlameFailed {
        key: BlameKey,
        kind: BlameFailureKind,
        message: String,
        epoch: u64,
    },
    RemoteTagChecked {
        remote: String,
        tag: String,
        exists: Option<bool>,
        epoch: u64,
    },
    OutgoingLoaded {
        bundle: Arc<OutgoingBundle>,
        epoch: u64,
    },
    PathMatchesLoaded {
        paths: Vec<String>,
        matches: Arc<HashSet<Oid>>,
        epoch: u64,
    },
    StageStatusLoaded {
        status: Arc<StageStatusSnapshot>,
        epoch: u64,
    },
    StageFileDiffLoaded {
        diff: Arc<ThreeDocStageDiff>,
        epoch: u64,
    },
    AmendPreviewLoaded {
        preview: Option<AmendCommitPreview>,
        epoch: u64,
    },
    CommitPrecheckLoaded {
        info: CommitPrecheckInfo,
    },
    CommitCompleted(CommitOutcome),
    UndoCommitCompleted {
        restored_message: String,
    },
    ConsoleStarted(GitConsoleEntry),
    ConsoleOutput {
        id: u64,
        stream: ConsoleStream,
        line: String,
    },
    ConsoleFinished {
        id: u64,
        exit_code: i32,
        duration_ms: u64,
    },
    IndexLockDetected {
        lock_path: PathBuf,
        message: String,
        retry_args: Vec<String>,
        retry_stdin: Option<Vec<u8>>,
        retry_is_read_only: bool,
        retry_refresh_after: RefreshScope,
    },
    ExternalFsChanged(RefreshScope),
    /// A [`TaskRequest::RunGitOp`] finished (successfully or not).
    GitOpFinished(GitOpOutcome),
    ConflictDocumentLoaded {
        doc: ConflictDocument,
        epoch: u64,
    },
    Error(String),
}

/// Narrow engine dispatch seam shared between the real [`EngineSupervisor`]
/// handle and deterministic local test doubles.
pub trait EngineBackend: Send + Sync {
    /// Submits a [`TaskRequest`] to the backend.
    ///
    /// Returns `Some(epoch)` when the request is bound to a [`RequestSlotId`],
    /// automatically advancing that slot's monotonic counter.
    fn dispatch(&self, req: TaskRequest) -> Option<u64>;

    fn request_repo_snapshot(&self) -> u64 {
        self.dispatch(TaskRequest::LoadRepoSnapshot)
            .expect("LoadRepoSnapshot always has a slot")
    }

    fn request_stage_status(&self, include_ignored: bool) -> u64 {
        self.dispatch(TaskRequest::LoadStageStatus { include_ignored })
            .expect("LoadStageStatus always has a slot")
    }

    fn request_commit_details(&self, slot: RequestSlotId, key: DetailsKey) -> u64 {
        self.dispatch(TaskRequest::LoadCommitDetails { slot, key })
            .expect("LoadCommitDetails always has a slot")
    }

    fn request_file_diff(&self, slot: RequestSlotId, key: FileDiffKey) -> u64 {
        self.request_file_diff_with_oids(slot, key, None)
    }

    fn request_file_diff_with_oids(
        &self,
        slot: RequestSlotId,
        key: FileDiffKey,
        known_oids: Option<(Option<Oid>, Option<Oid>)>,
    ) -> u64 {
        self.dispatch(TaskRequest::LoadFileDiff {
            slot,
            key,
            known_oids,
        })
        .expect("LoadFileDiff always has a slot")
    }

    fn request_file_history(&self, key: FileHistoryKey) -> u64 {
        self.dispatch(TaskRequest::LoadFileHistory { key })
            .expect("LoadFileHistory always has a slot")
    }

    fn request_blame(&self, key: BlameKey) -> u64 {
        self.dispatch(TaskRequest::LoadBlame { key })
            .expect("LoadBlame always has a slot")
    }

    fn request_check_remote_tag(&self, remote: String, tag: String) -> u64 {
        self.dispatch(TaskRequest::CheckRemoteTag { remote, tag })
            .expect("CheckRemoteTag always has a slot")
    }

    fn request_outgoing(&self, key: OutgoingKey) -> u64 {
        self.dispatch(TaskRequest::LoadOutgoing { key })
            .expect("LoadOutgoing always has a slot")
    }

    fn request_path_matches(&self, paths: Vec<String>, snapshot: Arc<RepoSnapshot>) -> u64 {
        self.dispatch(TaskRequest::LoadPathMatches { paths, snapshot })
            .expect("LoadPathMatches always has a slot")
    }

    fn request_stage_file_diff(&self, key: StageDiffKey) -> u64 {
        self.dispatch(TaskRequest::LoadStageFileDiff { key })
            .expect("LoadStageFileDiff always has a slot")
    }

    fn request_amend_preview(&self) -> u64 {
        self.dispatch(TaskRequest::LoadAmendPreview)
            .expect("LoadAmendPreview always has a slot")
    }

    fn request_conflict_document(&self, path: String) -> u64 {
        self.dispatch(TaskRequest::LoadConflictDocument { path })
            .expect("LoadConflictDocument always has a slot")
    }

    fn request_commit_precheck(&self) {
        let _ = self.dispatch(TaskRequest::ProbeCommitPrecheck);
    }
}

/// Deterministic in-memory [`EngineBackend`] adapter for fast, sleep-free tests.
#[derive(Debug, Default)]
pub struct RecordingEngineBackend {
    slots: SlotRegistry,
    recorded: Mutex<Vec<(TaskRequest, Option<u64>)>>,
}

impl RecordingEngineBackend {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn take_recorded(&self) -> Vec<(TaskRequest, Option<u64>)> {
        std::mem::take(&mut *self.recorded.lock().unwrap_or_else(|e| e.into_inner()))
    }

    pub fn recorded_len(&self) -> usize {
        self.recorded.lock().unwrap_or_else(|e| e.into_inner()).len()
    }

    pub fn latest_epoch(&self, slot: RequestSlotId) -> u64 {
        self.slots.epoch_for(slot).current()
    }
}

impl EngineBackend for RecordingEngineBackend {
    fn dispatch(&self, req: TaskRequest) -> Option<u64> {
        let epoch = req.slot().map(|slot| self.slots.next_epoch(slot));
        self.recorded
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push((req, epoch));
        epoch
    }
}

struct ScheduledTask {
    req: TaskRequest,
    token: Option<EpochToken>,
}

pub struct EngineHandle {
    location: RepoLocation,
    slots: SlotRegistry,
    git_info: Option<GitExecutableInfo>,
    task_tx: UnboundedSender<ScheduledTask>,
}

impl EngineHandle {
    pub fn location(&self) -> &RepoLocation {
        &self.location
    }

    pub fn git_info(&self) -> Option<&GitExecutableInfo> {
        self.git_info.as_ref()
    }
}

impl EngineBackend for EngineHandle {
    fn dispatch(&self, req: TaskRequest) -> Option<u64> {
        let token = req.slot().map(|slot| self.slots.issue(slot));
        let epoch = token.as_ref().map(EpochToken::epoch);
        let _ = self.task_tx.send(ScheduledTask { req, token });
        epoch
    }
}

fn spawn_epoch_task<Fut, T, M>(event_tx: &UnboundedSender<EngineEvent>, fut: Fut, map_ok: M)
where
    Fut: std::future::Future<Output = Result<T, EngineError>> + Send + 'static,
    M: FnOnce(T) -> EngineEvent + Send + 'static,
{
    let tx = event_tx.clone();
    tokio::spawn(async move {
        match fut.await {
            Ok(val) => {
                let _ = tx.send(map_ok(val));
            }
            Err(EngineError::Cancelled) => {}
            Err(e) => {
                let _ = tx.send(EngineEvent::Error(e.to_string()));
            }
        }
    });
}

struct SupervisorContext {
    location: RepoLocation,
    scan_config: StatusScanConfig,
    slots: SlotRegistry,
    pool: Arc<Git2WorkerPool>,
    cli: Option<Arc<GitCliExecutor>>,
    mutation_guard: Arc<MutationGuard>,
    event_tx: UnboundedSender<EngineEvent>,
    op_ids: Arc<AtomicU64>,
    sig_cache: Arc<Mutex<LruCache<Oid, SignatureStatus>>>,
}

pub struct EngineSupervisor;

impl EngineSupervisor {
    /// Spawns the engine supervisor, worker pool, CLI executor, and FS watcher.
    pub fn start(
        location: RepoLocation,
        enable_watcher: bool,
    ) -> Result<(EngineHandle, UnboundedReceiver<EngineEvent>), EngineError> {
        Self::start_with_config(location, enable_watcher, StatusScanConfig::default())
    }

    /// Variant of [`Self::start`] using explicit status/snapshot scan policies.
    pub fn start_with_config(
        location: RepoLocation,
        enable_watcher: bool,
        scan_config: StatusScanConfig,
    ) -> Result<(EngineHandle, UnboundedReceiver<EngineEvent>), EngineError> {
        let slots = SlotRegistry::new();
        let pool = Arc::new(Git2WorkerPool::new(4));
        let cli = GitCliExecutor::detect().ok();
        let git_info = cli.as_ref().map(GitCliExecutor::info);
        let mutation_guard = Arc::new(MutationGuard::new());

        let (task_tx, mut task_rx) = mpsc::unbounded_channel::<ScheduledTask>();
        let (event_tx, event_rx) = mpsc::unbounded_channel::<EngineEvent>();

        let watcher = if enable_watcher {
            let ev_tx = event_tx.clone();
            match RepoWatcher::start(
                location.clone(),
                Arc::clone(&mutation_guard),
                move |scope| {
                    let _ = ev_tx.send(EngineEvent::ExternalFsChanged(scope));
                },
            ) {
                Ok(w) => Some(w),
                Err(e) => {
                    let _ = event_tx.send(EngineEvent::Error(format!(
                        "filesystem watcher disabled: {e}"
                    )));
                    None
                }
            }
        } else {
            None
        };

        let ctx = SupervisorContext {
            location: location.clone(),
            scan_config,
            slots: slots.clone(),
            pool,
            cli: cli.map(Arc::new),
            mutation_guard,
            event_tx,
            op_ids: Arc::new(AtomicU64::new(1)),
            sig_cache: Arc::new(Mutex::new(LruCache::new(256))),
        };

        tokio::spawn(async move {
            let _keep_watcher = watcher;
            while let Some(task) = task_rx.recv().await {
                handle_task_request(&ctx, task);
            }
        });

        Ok((
            EngineHandle {
                location,
                slots,
                git_info,
                task_tx,
            },
            event_rx,
        ))
    }
}

fn handle_task_request(ctx: &SupervisorContext, task: ScheduledTask) {
    let ScheduledTask { req, token } = task;
    match req {
        TaskRequest::LoadRepoSnapshot => {
            spawn_load_repo_snapshot(ctx, token.expect("RepoSnapshot token"));
        }
        TaskRequest::LoadCommitDetails { slot, key } => {
            spawn_load_commit_details(ctx, slot, key, token.expect("CommitDetails token"));
        }
        TaskRequest::LoadFileDiff {
            slot,
            key,
            known_oids,
        } => {
            let pool = Arc::clone(&ctx.pool);
            let loc = ctx.location.clone();
            let token = token.expect("FileDiff token");
            let epoch = token.epoch();
            spawn_epoch_task(
                &ctx.event_tx,
                async move {
                    pool.run_epoch(TaskPriority::Interactive, token, move |tok| {
                        load_file_diff_with_oids_sync(&loc, &key, known_oids, tok).map(Arc::new)
                    })
                    .await
                },
                move |diff| EngineEvent::FileDiffLoaded { slot, diff, epoch },
            );
        }
        TaskRequest::LoadFileHistory { key } => {
            let pool = Arc::clone(&ctx.pool);
            let loc = ctx.location.clone();
            let token = token.expect("FileHistory token");
            let epoch = token.epoch();
            spawn_epoch_task(
                &ctx.event_tx,
                async move {
                    pool.run_epoch(TaskPriority::Interactive, token, move |tok| {
                        load_file_history_sync(&loc, &key, tok).map(Arc::new)
                    })
                    .await
                },
                move |bundle| EngineEvent::FileHistoryLoaded { bundle, epoch },
            );
        }
        TaskRequest::LoadBlame { key } => {
            let pool = Arc::clone(&ctx.pool);
            let loc = ctx.location.clone();
            let token = token.expect("Blame token");
            let epoch = token.epoch();
            let tx = ctx.event_tx.clone();
            tokio::spawn(async move {
                let fail_key = key.clone();
                let res = pool
                    .run_epoch(TaskPriority::Interactive, token, move |tok| {
                        load_blame_sync(&loc, &key, tok).map(Arc::new)
                    })
                    .await;
                match res {
                    Ok(bundle) => {
                        let _ = tx.send(EngineEvent::BlameLoaded { bundle, epoch });
                    }
                    Err(EngineError::Cancelled) => {}
                    Err(EngineError::TooLarge { limit }) => {
                        let _ = tx.send(EngineEvent::BlameFailed {
                            key: fail_key,
                            kind: BlameFailureKind::TooLarge { limit },
                            message: format!("file exceeds size limit ({limit} bytes)"),
                            epoch,
                        });
                    }
                    Err(EngineError::Binary) => {
                        let _ = tx.send(EngineEvent::BlameFailed {
                            key: fail_key,
                            kind: BlameFailureKind::Binary,
                            message: "binary file cannot be annotated".to_string(),
                            epoch,
                        });
                    }
                    Err(e) => {
                        let _ = tx.send(EngineEvent::BlameFailed {
                            key: fail_key,
                            kind: BlameFailureKind::Other,
                            message: e.to_string(),
                            epoch,
                        });
                    }
                }
            });
        }
        TaskRequest::CheckRemoteTag { remote, tag } => {
            let loc = ctx.location.clone();
            let git_exe = ctx
                .cli
                .as_ref()
                .map(|c| c.info().path)
                .unwrap_or_else(|| PathBuf::from("git"));
            let token = token.expect("CheckRemoteTag token");
            let epoch = token.epoch();
            let tx = ctx.event_tx.clone();
            tokio::spawn(async move {
                if token.is_stale() {
                    return;
                }
                let exists = check_remote_tag_async(&git_exe, &loc, &remote, &tag).await;
                if !token.is_stale() {
                    let _ = tx.send(EngineEvent::RemoteTagChecked {
                        remote,
                        tag,
                        exists,
                        epoch,
                    });
                }
            });
        }
        TaskRequest::LoadOutgoing { key } => {
            let pool = Arc::clone(&ctx.pool);
            let loc = ctx.location.clone();
            let token = token.expect("LoadOutgoing token");
            let epoch = token.epoch();
            spawn_epoch_task(
                &ctx.event_tx,
                async move {
                    pool.run_epoch(TaskPriority::Interactive, token, move |tok| {
                        load_outgoing_sync(&loc, &key, tok).map(Arc::new)
                    })
                    .await
                },
                move |bundle| EngineEvent::OutgoingLoaded { bundle, epoch },
            );
        }
        TaskRequest::LoadPathMatches { paths, snapshot } => {
            let pool = Arc::clone(&ctx.pool);
            let loc = ctx.location.clone();
            let token = token.expect("PathMatches token");
            let epoch = token.epoch();
            let reported = paths.clone();
            spawn_epoch_task(
                &ctx.event_tx,
                async move {
                    pool.run_epoch(TaskPriority::Background, token, move |tok| {
                        load_path_matches_sync(&loc, &paths, &snapshot, tok).map(Arc::new)
                    })
                    .await
                },
                move |matches| EngineEvent::PathMatchesLoaded {
                    paths: reported,
                    matches,
                    epoch,
                },
            );
        }
        TaskRequest::LoadStageStatus { include_ignored } => {
            let pool = Arc::clone(&ctx.pool);
            let loc = ctx.location.clone();
            let scan_config = ctx.scan_config;
            let token = token.expect("StageStatus token");
            let epoch = token.epoch();
            spawn_epoch_task(
                &ctx.event_tx,
                async move {
                    pool.run_epoch(TaskPriority::Background, token, move |tok| {
                        load_stage_status_with_config_sync(&loc, include_ignored, scan_config, tok)
                            .map(Arc::new)
                    })
                    .await
                },
                move |status| EngineEvent::StageStatusLoaded { status, epoch },
            );
        }
        TaskRequest::LoadStageFileDiff { key } => {
            let pool = Arc::clone(&ctx.pool);
            let loc = ctx.location.clone();
            let token = token.expect("StageFileDiff token");
            let epoch = token.epoch();
            spawn_epoch_task(
                &ctx.event_tx,
                async move {
                    pool.run_epoch(TaskPriority::Interactive, token, move |tok| {
                        load_stage_file_diff_sync(&loc, &key, tok).map(Arc::new)
                    })
                    .await
                },
                move |diff| EngineEvent::StageFileDiffLoaded { diff, epoch },
            );
        }
        TaskRequest::LoadAmendPreview => {
            let pool = Arc::clone(&ctx.pool);
            let loc = ctx.location.clone();
            let token = token.expect("AmendPreview token");
            let epoch = token.epoch();
            spawn_epoch_task(
                &ctx.event_tx,
                async move {
                    pool.run_epoch(TaskPriority::Background, token, move |_| {
                        load_amend_preview_sync(&loc)
                    })
                    .await
                },
                move |preview| EngineEvent::AmendPreviewLoaded { preview, epoch },
            );
        }
        TaskRequest::ProbeCommitPrecheck => {
            let pool = Arc::clone(&ctx.pool);
            let loc = ctx.location.clone();
            let tx = ctx.event_tx.clone();
            tokio::spawn(async move {
                let info = pool
                    .run_with(TaskPriority::Background, move || {
                        Ok(probe_commit_precheck_sync(&loc))
                    })
                    .await
                    .unwrap_or_default();
                let _ = tx.send(EngineEvent::CommitPrecheckLoaded { info });
            });
        }
        TaskRequest::ApplyStageBuffer {
            path,
            new_staged_bytes,
            file_mode,
            remove_from_index,
        } => spawn_guarded_mutation(ctx, RefreshScope::StageStatus, move |pool, loc| async move {
            pool.run_with(TaskPriority::Interactive, move || {
                apply_stage_buffer_sync(&loc, &path, &new_staged_bytes, file_mode, remove_from_index)
            })
            .await
        }),
        TaskRequest::WriteWorktreeBuffer {
            path,
            new_local_bytes,
        } => spawn_guarded_mutation(
            ctx,
            RefreshScope::WorktreeStatus,
            move |pool, loc| async move {
                pool.run_with(TaskPriority::Interactive, move || {
                    write_worktree_buffer_sync(&loc, &path, &new_local_bytes)
                })
                .await
            },
        ),
        TaskRequest::StageTargets { targets } => {
            spawn_guarded_mutation(ctx, RefreshScope::StageStatus, move |pool, loc| async move {
                pool.run_with(TaskPriority::Interactive, move || {
                    stage_targets_sync(&loc, &targets)
                })
                .await
            });
        }
        TaskRequest::StageAllTracked => {
            spawn_guarded_mutation(ctx, RefreshScope::StageStatus, move |pool, loc| async move {
                pool.run_with(TaskPriority::Interactive, move || {
                    stage_all_tracked_sync(&loc)
                })
                .await
            });
        }
        TaskRequest::UnstageTargets { targets } => {
            spawn_guarded_mutation(ctx, RefreshScope::StageStatus, move |pool, loc| async move {
                pool.run_with(TaskPriority::Interactive, move || {
                    unstage_targets_sync(&loc, &targets)
                })
                .await
            });
        }
        TaskRequest::DiscardTargets { targets } => {
            spawn_guarded_mutation(
                ctx,
                RefreshScope::WorktreeStatus,
                move |pool, loc| async move {
                    pool.run_with(TaskPriority::Interactive, move || {
                        discard_targets_sync(&loc, &targets)
                    })
                    .await
                },
            );
        }
        TaskRequest::ExecuteCommit { id, request } => spawn_execute_commit(ctx, id, request),
        TaskRequest::UndoLastCommit => spawn_undo_last_commit(ctx),
        TaskRequest::RunGitOp { op } => spawn_run_git_op(ctx, op),
        TaskRequest::LoadConflictDocument { path } => {
            let pool = Arc::clone(&ctx.pool);
            let loc = ctx.location.clone();
            let token = token.expect("ConflictDocument token");
            let epoch = token.epoch();
            spawn_epoch_task(
                &ctx.event_tx,
                async move {
                    pool.run_epoch(TaskPriority::Interactive, token, move |_| {
                        load_conflict_document_sync(&loc, &path)
                    })
                    .await
                },
                move |doc| EngineEvent::ConflictDocumentLoaded { doc, epoch },
            );
        }
        TaskRequest::SaveConflictResolution {
            path,
            content,
            stage,
        } => spawn_guarded_mutation(ctx, RefreshScope::StageStatus, move |pool, loc| async move {
            pool.run_with(TaskPriority::Interactive, move || {
                write_worktree_buffer_sync(&loc, &path, &content)?;
                if stage {
                    stage_targets_sync(
                        &loc,
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
        }),
        TaskRequest::RunGitCli {
            id,
            args,
            stdin_payload,
            is_read_only,
            refresh_after,
        } => spawn_run_git_cli(ctx, id, args, stdin_payload, is_read_only, refresh_after),
    }
}

fn spawn_load_repo_snapshot(ctx: &SupervisorContext, token: EpochToken) {
    let pool = Arc::clone(&ctx.pool);
    let loc = ctx.location.clone();
    let scan_config = ctx.scan_config;
    let epoch = token.epoch();
    let tx = ctx.event_tx.clone();
    tokio::spawn(async move {
        let loc_snap = loc.clone();
        let snap_res = pool
            .run_epoch(TaskPriority::Background, token.clone(), move |tok| {
                let (snap, pending) = load_repo_snapshot_with_config_sync(
                    &loc_snap,
                    tok,
                    DEFAULT_MAX_COMMITS,
                    scan_config,
                )?;
                Ok((Arc::new(snap), pending))
            })
            .await;

        match snap_res {
            Ok((snapshot, pending)) => {
                let _ = tx.send(EngineEvent::RepoSnapshotLoaded { snapshot, epoch });
                if !pending.is_empty() && !token.is_stale() {
                    let backfill_res = pool
                        .run_epoch(TaskPriority::Background, token, move |tok| {
                            compute_ahead_behind_batch_sync(&loc, &pending, tok)
                        })
                        .await;
                    if let Ok(updates) = backfill_res
                        && !updates.is_empty()
                    {
                        let _ = tx.send(EngineEvent::RefAheadBehindBackfilled { epoch, updates });
                    }
                }
            }
            Err(EngineError::Cancelled) => {}
            Err(e) => {
                let _ = tx.send(EngineEvent::Error(e.to_string()));
            }
        }
    });
}

fn spawn_load_commit_details(
    ctx: &SupervisorContext,
    slot: RequestSlotId,
    key: DetailsKey,
    token: EpochToken,
) {
    let pool = Arc::clone(&ctx.pool);
    let loc = ctx.location.clone();
    let epoch = token.epoch();
    let tx = ctx.event_tx.clone();
    let sig_cache = Arc::clone(&ctx.sig_cache);
    let sig_epoch = ctx.slots.epoch_for(RequestSlotId::CommitSignature).clone();
    tokio::spawn(async move {
        let loc_details = loc.clone();
        let res = pool
            .run_epoch(TaskPriority::Interactive, token, move |tok| {
                load_commit_details_sync(&loc_details, &key, tok).map(Arc::new)
            })
            .await;
        match res {
            Ok(mut bundle) => {
                let mut unverified_oids = Vec::new();
                {
                    let mut cache = sig_cache.lock().unwrap_or_else(|e| e.into_inner());
                    let b_mut = Arc::make_mut(&mut bundle);
                    for d in &mut b_mut.details {
                        if d.signature == SignatureStatus::SignedUnverified {
                            if let Some(&cached) = cache.get(&d.oid) {
                                d.signature = cached;
                            } else {
                                unverified_oids.push(d.oid);
                            }
                        }
                    }
                }
                let _ = tx.send(EngineEvent::CommitDetailsLoaded {
                    slot,
                    bundle,
                    epoch,
                });
                if !unverified_oids.is_empty() {
                    let sig_token = sig_epoch.issue();
                    let git_dir = loc.git_dir;
                    tokio::task::spawn_blocking(move || {
                        for oid in unverified_oids {
                            if sig_token.is_stale() {
                                break;
                            }
                            let status = verify_commit_signature_sync(&git_dir, oid);
                            {
                                let mut cache =
                                    sig_cache.lock().unwrap_or_else(|e| e.into_inner());
                                cache.insert(oid, status);
                            }
                            let _ = tx.send(EngineEvent::CommitSignatureVerified { oid, status });
                        }
                    });
                }
            }
            Err(EngineError::Cancelled) => {}
            Err(e) => {
                let _ = tx.send(EngineEvent::Error(e.to_string()));
            }
        }
    });
}

fn spawn_execute_commit(ctx: &SupervisorContext, id: u64, request: CommitRequest) {
    let pool = Arc::clone(&ctx.pool);
    let cli_exec = ctx.cli.as_ref().map(Arc::clone);
    let loc = ctx.location.clone();
    let tx = ctx.event_tx.clone();
    let guard = Arc::clone(&ctx.mutation_guard);
    tokio::spawn(async move {
        let token = guard.begin();
        let loc_pre = loc.clone();
        let precheck = pool
            .run_with(TaskPriority::Interactive, move || {
                Ok(probe_commit_precheck_sync(&loc_pre))
            })
            .await
            .unwrap_or_default();
        let use_cli =
            ((precheck.has_commit_hooks && request.run_hooks) || precheck.gpg_sign)
                && cli_exec.is_some();

        let (outcome, index_lock) = if use_cli {
            if request.stage_all_tracked {
                let loc_stage = loc.clone();
                if let Err(e) = pool
                    .run_with(TaskPriority::Interactive, move || {
                        stage_all_tracked_sync(&loc_stage)
                    })
                    .await
                {
                    drop(token);
                    let _ = tx.send(EngineEvent::CommitCompleted(CommitOutcome {
                        success: false,
                        commit_oid: None,
                        short_sha: None,
                        summary: "Stage all tracked failed".to_string(),
                        error_output: Some(e.to_string()),
                        and_push: false,
                        preserved_message: Some(request.message),
                    }));
                    return;
                }
            }
            let cli = cli_exec.expect("checked is_some");
            let fwd = tx.clone();
            execute_commit_cli_async(cli.as_ref(), &loc, id, request, move |ev| {
                forward_cli_event(&fwd, ev);
            })
            .await
        } else {
            let req_for_git2 = request.clone();
            let outcome = match pool
                .run_with(TaskPriority::Interactive, move || {
                    execute_commit_git2_sync(&loc, &req_for_git2)
                })
                .await
            {
                Ok(o) => o,
                Err(e) => CommitOutcome {
                    success: false,
                    commit_oid: None,
                    short_sha: None,
                    summary: "Commit failed".to_string(),
                    error_output: Some(e.to_string()),
                    and_push: false,
                    preserved_message: Some(request.message),
                },
            };
            (outcome, None)
        };

        let ok = outcome.success;
        drop(token);
        let deferred = guard.take_pending();
        if let Some(conflict) = index_lock {
            let _ = tx.send(EngineEvent::IndexLockDetected {
                lock_path: conflict.lock_path,
                message: conflict.message,
                retry_args: conflict.retry_args,
                retry_stdin: conflict.retry_stdin,
                retry_is_read_only: conflict.retry_is_read_only,
                retry_refresh_after: RefreshScope::All,
            });
        }
        let _ = tx.send(EngineEvent::CommitCompleted(outcome));
        if ok {
            let _ = tx.send(EngineEvent::ExternalFsChanged(
                RefreshScope::All.merge(deferred),
            ));
        }
    });
}

fn spawn_undo_last_commit(ctx: &SupervisorContext) {
    let pool = Arc::clone(&ctx.pool);
    let loc = ctx.location.clone();
    let tx = ctx.event_tx.clone();
    let guard = Arc::clone(&ctx.mutation_guard);
    tokio::spawn(async move {
        let token = guard.begin();
        let res = pool
            .run_with(TaskPriority::Interactive, move || {
                undo_last_commit_sync(&loc)
            })
            .await;
        drop(token);
        let deferred = guard.take_pending();
        match res {
            Ok(restored_message) => {
                let _ = tx.send(EngineEvent::UndoCommitCompleted { restored_message });
                let _ = tx.send(EngineEvent::ExternalFsChanged(
                    RefreshScope::All.merge(deferred),
                ));
            }
            Err(e) => {
                let _ = tx.send(EngineEvent::Error(e.to_string()));
            }
        }
    });
}

fn spawn_run_git_op(ctx: &SupervisorContext, op: GitOp) {
    let Some(cli_exec) = ctx.cli.as_ref().map(Arc::clone) else {
        let _ = ctx
            .event_tx
            .send(EngineEvent::Error(EngineError::GitNotFound.to_string()));
        return;
    };
    let loc = ctx.location.clone();
    let tx = ctx.event_tx.clone();
    let guard = Arc::clone(&ctx.mutation_guard);
    let ids = Arc::clone(&ctx.op_ids);
    tokio::spawn(async move {
        let read_only = op.is_read_only();
        let refresh = op.refresh_scope();
        let token = (!read_only).then(|| guard.begin());
        let fwd = tx.clone();
        let res = crate::ops::run_git_op(cli_exec.as_ref(), &loc, &ids, op, move |ev| {
            forward_cli_event(&fwd, ev);
        })
        .await;
        drop(token);
        let deferred = guard.take_pending();
        if let Some(conflict) = res.index_lock {
            let _ = tx.send(EngineEvent::IndexLockDetected {
                lock_path: conflict.lock_path,
                message: conflict.message,
                retry_args: conflict.retry_args,
                retry_stdin: conflict.retry_stdin,
                retry_is_read_only: conflict.retry_is_read_only,
                retry_refresh_after: refresh,
            });
        }
        let _ = tx.send(EngineEvent::GitOpFinished(res.outcome));
        let merged = refresh.merge(deferred);
        if merged != RefreshScope::None {
            let _ = tx.send(EngineEvent::ExternalFsChanged(merged));
        }
    });
}

fn spawn_run_git_cli(
    ctx: &SupervisorContext,
    id: u64,
    args: Vec<String>,
    stdin_payload: Option<Vec<u8>>,
    is_read_only: bool,
    refresh_after: RefreshScope,
) {
    let Some(cli_exec) = ctx.cli.as_ref().map(Arc::clone) else {
        let _ = ctx
            .event_tx
            .send(EngineEvent::Error(EngineError::GitNotFound.to_string()));
        return;
    };
    let loc = ctx.location.clone();
    let tx = ctx.event_tx.clone();
    let guard = Arc::clone(&ctx.mutation_guard);
    tokio::spawn(async move {
        let _token = if is_read_only {
            None
        } else {
            Some(guard.begin())
        };

        let res = cli_exec
            .execute(
                CliCommand {
                    id,
                    workdir: &loc.workdir,
                    git_dir: &loc.git_dir,
                    args: &args,
                    stdin_payload: stdin_payload.as_deref(),
                    is_read_only,
                    env: &[],
                },
                |ev| forward_cli_event(&tx, ev),
            )
            .await;

        drop(_token);

        match res {
            Ok(out) => {
                if let Some((lock_path, message)) = out.index_lock_conflict {
                    let _ = tx.send(EngineEvent::IndexLockDetected {
                        lock_path,
                        message,
                        retry_args: args,
                        retry_stdin: stdin_payload,
                        retry_is_read_only: is_read_only,
                        retry_refresh_after: refresh_after,
                    });
                }
                let deferred = guard.take_pending();
                let merged = refresh_after.merge(deferred);
                if merged != RefreshScope::None {
                    let _ = tx.send(EngineEvent::ExternalFsChanged(merged));
                }
            }
            Err(e) => {
                let _ = tx.send(EngineEvent::Error(e.to_string()));
            }
        }
    });
}

pub(crate) fn forward_cli_event(tx: &UnboundedSender<EngineEvent>, ev: CliStreamEvent) {
    let _ = tx.send(match ev {
        CliStreamEvent::Started(entry) => EngineEvent::ConsoleStarted(entry),
        CliStreamEvent::Output { id, stream, line } => EngineEvent::ConsoleOutput {
            id,
            stream,
            line,
        },
        CliStreamEvent::Finished {
            id,
            exit_code,
            duration_ms,
        } => EngineEvent::ConsoleFinished {
            id,
            exit_code,
            duration_ms,
        },
    });
}

fn spawn_guarded_mutation<F, Fut>(
    ctx: &SupervisorContext,
    scope: RefreshScope,
    op: F,
) where
    F: FnOnce(Arc<Git2WorkerPool>, RepoLocation) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = Result<(), EngineError>> + Send + 'static,
{
    let pool = Arc::clone(&ctx.pool);
    let loc = ctx.location.clone();
    let tx = ctx.event_tx.clone();
    let guard = Arc::clone(&ctx.mutation_guard);
    tokio::spawn(async move {
        let token = guard.begin();
        let res = op(pool, loc).await;
        drop(token);
        let deferred = guard.take_pending();
        match res {
            Ok(()) => {
                let _ = tx.send(EngineEvent::ExternalFsChanged(scope.merge(deferred)));
            }
            Err(e) => {
                let _ = tx.send(EngineEvent::Error(e.to_string()));
            }
        }
    });
}
