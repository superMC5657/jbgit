use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use jbgit_core::{
    AmendCommitPreview, BlameBundle, BlameKey, CherryPickedKey, CommitChangesBundle,
    CommitSignature, ConflictDocument, ConsoleStream, DetailsKey, EpochToken, FileDiff,
    FileDiffKey, FileHistoryBundle, FileHistoryKey, FileStatusKind, GitConsoleEntry, GitOp,
    GitOpOutcome, LoadedConflict, LruCache, Oid, OutgoingBundle, OutgoingKey, PatchCheck,
    PatchCheckKey, ReflogBundle, ReflogKey, RefreshScope, RepoSnapshot, RequestSlotId,
    SignatureStatus, SlotRegistry, SpecialConflictInfo, StageNodeKind, StageStatusSnapshot,
    StageTarget, TreeBlob, TreeBlobKey, TreeKey, TreeListing,
};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

use crate::askpass::AskpassEnv;
use crate::cli::{CliCommand, CliStreamEvent, GitCliExecutor, GitExecutableInfo};
use crate::details::{
    check_patch_async, check_remote_tag_async, export_tree_blob_sync, load_blame_async,
    load_cherry_picked_async, load_commit_details_sync, load_file_diff_with_oids_sync,
    load_file_history_async, load_outgoing_sync, load_path_matches_sync, load_reflog_async,
    load_tree_blob_sync, load_tree_index_async, load_tree_sync, verify_commit_signature_sync,
    verify_commit_signatures_batch_sync,
};
use crate::error::EngineError;
use crate::pool::{
    AheadBehindUpdate, Git2WorkerPool, SnapshotCommitCache, SnapshotLoadMode, StatusScanConfig,
    TaskPriority, compute_ahead_behind_batch_sync, load_conflict_document_sync,
    load_repo_snapshot_incremental_sync, load_stage_status_with_config_sync,
};
use crate::probe::RepoLocation;
use crate::stage::{
    CommitOutcome, CommitPrecheckInfo, CommitRequest, StageDiffKey, ThreeDocStageDiff,
    apply_stage_buffer_sync, discard_targets_sync, execute_commit_cli_async,
    execute_commit_git2_sync, load_amend_preview_sync, load_stage_file_diff_sync,
    probe_commit_precheck_sync, remove_conflict_path_sync, stage_all_tracked_sync,
    stage_targets_sync, undo_last_commit_sync, unstage_targets_sync, write_worktree_buffer_sync,
};
use crate::watcher::{MutationGuard, MutationPermit, RepoWatcher};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlameFailureKind {
    TooLarge { limit: usize },
    Binary,
    Other,
}

#[derive(Debug, Clone)]
pub enum TaskRequest {
    LoadRepoSnapshot,
    /// Explicit full repository reload bypassing the incremental commit cache.
    LoadRepoSnapshotFull,
    /// Progressive pagination: expands the commit limit by one page and reloads.
    LoadMoreCommits,
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
    /// Read-only `git apply --check --numstat` preflight of the Apply Patch dialog.
    CheckPatch {
        key: PatchCheckKey,
    },
    /// One directory of a commit's tree (Browse Repository at Revision).
    LoadTree {
        key: TreeKey,
    },
    /// File content at a revision for the tree browser or file history preview.
    LoadTreeBlob {
        slot: RequestSlotId,
        key: TreeBlobKey,
    },
    /// Recursive file path index for `/` fuzzy search in the tree browser.
    LoadTreeIndex {
        oid: Oid,
    },
    /// Export raw blob bytes at `oid:path` to `dest`.
    ExportTreeBlob {
        oid: Oid,
        path: String,
        dest: PathBuf,
    },
    /// Commits of `snapshot` touching any of `paths` (log `Paths` filter).
    LoadPathMatches {
        paths: Vec<String>,
        snapshot: Arc<RepoSnapshot>,
    },
    /// Batch signature verification (`git log --no-walk --stdin`) for Log table rows.
    VerifyCommitSignatures {
        oids: Vec<Oid>,
    },
    /// Patch-equivalent commit detection (`git cherry <head_oid> <branch>`).
    LoadCherryPicked {
        key: CherryPickedKey,
    },
    /// Paginated Git reflog entries (`git reflog show`).
    LoadReflog {
        key: ReflogKey,
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
        env: Vec<(String, String)>,
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
        delete: bool,
    },
}

impl TaskRequest {
    /// Returns the consumer slot whose single-flight epoch governs this request, if any.
    pub const fn slot(&self) -> Option<RequestSlotId> {
        match self {
            Self::LoadRepoSnapshot | Self::LoadRepoSnapshotFull | Self::LoadMoreCommits => {
                Some(RequestSlotId::RepoSnapshot)
            }
            Self::LoadCommitDetails { slot, .. } => Some(*slot),
            Self::LoadFileDiff { slot, .. } => Some(*slot),
            Self::LoadFileHistory { .. } => Some(RequestSlotId::FileHistoryList),
            Self::LoadBlame { .. } => Some(RequestSlotId::Annotate),
            Self::CheckRemoteTag { .. } => Some(RequestSlotId::RemoteTagCheck),
            Self::LoadOutgoing { .. } => Some(RequestSlotId::PushOutgoing),
            Self::CheckPatch { .. } => Some(RequestSlotId::PatchCheck),
            Self::LoadTree { .. } => Some(RequestSlotId::TreeBrowser),
            Self::LoadTreeBlob { slot, .. } => Some(*slot),
            Self::LoadTreeIndex { .. } => Some(RequestSlotId::TreeIndex),
            Self::VerifyCommitSignatures { .. } => Some(RequestSlotId::LogSignatures),
            Self::LoadCherryPicked { .. } => Some(RequestSlotId::LogCherryPicked),
            Self::LoadReflog { .. } => Some(RequestSlotId::Reflog),
            Self::LoadPathMatches { .. } => Some(RequestSlotId::PathFilter),
            Self::LoadStageStatus { .. } => Some(RequestSlotId::StageStatus),
            Self::LoadStageFileDiff { .. } => Some(RequestSlotId::StageFileDiff),
            Self::LoadAmendPreview => Some(RequestSlotId::AmendPreview),
            Self::LoadConflictDocument { .. } => Some(RequestSlotId::ConflictDocument),
            Self::ProbeCommitPrecheck => Some(RequestSlotId::CommitPrecheck),
            Self::ApplyStageBuffer { .. }
            | Self::WriteWorktreeBuffer { .. }
            | Self::StageTargets { .. }
            | Self::StageAllTracked
            | Self::UnstageTargets { .. }
            | Self::DiscardTargets { .. }
            | Self::ExecuteCommit { .. }
            | Self::UndoLastCommit
            | Self::RunGitCli { .. }
            | Self::RunGitOp { .. }
            | Self::SaveConflictResolution { .. }
            | Self::ExportTreeBlob { .. } => None,
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
        signature: CommitSignature,
    },
    CommitSignaturesLoaded {
        signatures: Vec<(Oid, CommitSignature)>,
        epoch: u64,
    },
    CherryPickedLoaded {
        key: CherryPickedKey,
        oids: Arc<HashSet<Oid>>,
        epoch: u64,
    },
    ReflogLoaded {
        bundle: Arc<ReflogBundle>,
        epoch: u64,
    },
    ReflogFailed {
        key: ReflogKey,
        message: String,
        epoch: u64,
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
    PatchChecked {
        check: Arc<PatchCheck>,
        epoch: u64,
    },
    TreeLoaded {
        listing: Arc<TreeListing>,
        epoch: u64,
    },
    TreeLoadFailed {
        key: TreeKey,
        message: String,
        epoch: u64,
    },
    TreeBlobLoaded {
        slot: RequestSlotId,
        blob: Arc<TreeBlob>,
        epoch: u64,
    },
    TreeIndexLoaded {
        oid: Oid,
        paths: Arc<Vec<String>>,
        epoch: u64,
    },
    TreeBlobExported {
        path: String,
        dest: PathBuf,
        result: Result<u64, String>,
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
        epoch: u64,
    },
    /// Pre-commit CRLF warning: commit was paused so the user can choose Fix & Commit,
    /// Commit As Is, or Don't Warn Again.
    CommitCrlfWarning {
        request: CommitRequest,
        files: Vec<String>,
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
        retry_env: Vec<(String, String)>,
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
    SpecialConflictLoaded {
        info: SpecialConflictInfo,
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

    fn request_repo_snapshot_full(&self) -> u64 {
        self.dispatch(TaskRequest::LoadRepoSnapshotFull)
            .expect("LoadRepoSnapshotFull always has a slot")
    }

    fn request_load_more_commits(&self) -> u64 {
        self.dispatch(TaskRequest::LoadMoreCommits)
            .expect("LoadMoreCommits always has a slot")
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

    fn request_patch_check(&self, key: PatchCheckKey) -> u64 {
        self.dispatch(TaskRequest::CheckPatch { key })
            .expect("CheckPatch always has a slot")
    }

    fn request_tree(&self, key: TreeKey) -> u64 {
        self.dispatch(TaskRequest::LoadTree { key })
            .expect("LoadTree always has a slot")
    }

    fn request_tree_blob(&self, slot: RequestSlotId, key: TreeBlobKey) -> u64 {
        self.dispatch(TaskRequest::LoadTreeBlob { slot, key })
            .expect("LoadTreeBlob always has a slot")
    }

    fn request_tree_index(&self, oid: Oid) -> u64 {
        self.dispatch(TaskRequest::LoadTreeIndex { oid })
            .expect("LoadTreeIndex always has a slot")
    }

    fn request_export_tree_blob(&self, oid: Oid, path: String, dest: PathBuf) {
        self.dispatch(TaskRequest::ExportTreeBlob { oid, path, dest });
    }

    fn request_commit_signatures(&self, oids: Vec<Oid>) -> u64 {
        self.dispatch(TaskRequest::VerifyCommitSignatures { oids })
            .expect("VerifyCommitSignatures always has a slot")
    }

    fn request_cherry_picked(&self, key: CherryPickedKey) -> u64 {
        self.dispatch(TaskRequest::LoadCherryPicked { key })
            .expect("LoadCherryPicked always has a slot")
    }

    fn request_reflog(&self, key: ReflogKey) -> u64 {
        self.dispatch(TaskRequest::LoadReflog { key })
            .expect("LoadReflog always has a slot")
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

    fn request_commit_precheck(&self) -> u64 {
        self.dispatch(TaskRequest::ProbeCommitPrecheck)
            .expect("ProbeCommitPrecheck always has a slot")
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
        self.recorded
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .len()
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
    commit_page_size: usize,
    task_tx: UnboundedSender<ScheduledTask>,
    mutation_guard: Arc<MutationGuard>,
}

impl EngineHandle {
    pub fn location(&self) -> &RepoLocation {
        &self.location
    }

    pub fn git_info(&self) -> Option<&GitExecutableInfo> {
        self.git_info.as_ref()
    }

    /// Configured commit page size (initial load and `LoadMoreCommits` increment).
    pub fn commit_page_size(&self) -> usize {
        self.commit_page_size
    }

    /// Whether a write operation (Git op, commit, stage mutation) is running.
    pub fn is_mutating(&self) -> bool {
        self.mutation_guard.is_mutating()
    }

    /// Stops this engine (repository session unbind, Spec 12 Issue 09):
    /// every request slot is advanced so in-flight reads observe a stale
    /// token and drop their results, the task queue closes (the supervisor
    /// loop and the filesystem watcher stop), then in-flight write operations
    /// get up to `timeout` to finish. Returns `false` if writes were still
    /// running when the timeout expired (they keep running detached).
    pub async fn shutdown(self, timeout: std::time::Duration) -> bool {
        for slot in RequestSlotId::ALL {
            let _ = self.slots.issue(slot);
        }
        let guard = Arc::clone(&self.mutation_guard);
        drop(self);
        let deadline = tokio::time::Instant::now() + timeout;
        while guard.is_mutating() {
            if tokio::time::Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        true
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
    cli_error: Option<String>,
    mutation_guard: Arc<MutationGuard>,
    event_tx: UnboundedSender<EngineEvent>,
    op_ids: Arc<AtomicU64>,
    sig_cache: Arc<Mutex<LruCache<Oid, CommitSignature>>>,
    snapshot_cache: Arc<Mutex<Option<SnapshotCommitCache>>>,
    commit_limit: Arc<AtomicUsize>,
    /// Credential bridge for network [`GitOp`]s (`None` = prompts fail fast).
    askpass: Option<Arc<AskpassEnv>>,
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
        Self::start_with_askpass(location, enable_watcher, scan_config, None)
    }

    /// Variant of [`Self::start_with_config`] whose network operations route
    /// credential prompts through `askpass` (Spec 12 Issue 08).
    pub fn start_with_askpass(
        location: RepoLocation,
        enable_watcher: bool,
        scan_config: StatusScanConfig,
        askpass: Option<AskpassEnv>,
    ) -> Result<(EngineHandle, UnboundedReceiver<EngineEvent>), EngineError> {
        Self::start_with_options(location, enable_watcher, scan_config, askpass, None)
    }

    /// Full supervisor startup honoring explicit `[git] executable` configuration.
    pub fn start_with_options(
        location: RepoLocation,
        enable_watcher: bool,
        scan_config: StatusScanConfig,
        askpass: Option<AskpassEnv>,
        git_executable: Option<String>,
    ) -> Result<(EngineHandle, UnboundedReceiver<EngineEvent>), EngineError> {
        crate::cli::set_configured_git_executable(git_executable.clone());
        let slots = SlotRegistry::new();
        let pool = Arc::new(Git2WorkerPool::new(4));
        let cli_res = GitCliExecutor::detect_with_executable(git_executable.as_deref());
        let (cli, cli_error) = match cli_res {
            Ok(exec) => (Some(exec), None),
            Err(e) => (None, Some(e.to_string())),
        };
        let git_info = cli.as_ref().map(GitCliExecutor::info);
        let mutation_guard = Arc::new(MutationGuard::new());

        let (task_tx, mut task_rx) = mpsc::unbounded_channel::<ScheduledTask>();
        let (event_tx, event_rx) = mpsc::unbounded_channel::<EngineEvent>();
        if let Some(ref err) = cli_error
            && git_executable
                .as_deref()
                .is_some_and(|s| !s.trim().is_empty())
        {
            let _ = event_tx.send(EngineEvent::Error(err.clone()));
        }

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

        let commit_page_size = scan_config.effective_commit_page_size();
        let ctx = SupervisorContext {
            location: location.clone(),
            scan_config,
            slots: slots.clone(),
            pool,
            cli: cli.map(Arc::new),
            cli_error,
            mutation_guard: Arc::clone(&mutation_guard),
            event_tx,
            op_ids: Arc::new(AtomicU64::new(1)),
            sig_cache: Arc::new(Mutex::new(LruCache::new(1024))),
            snapshot_cache: Arc::new(Mutex::new(None)),
            commit_limit: Arc::new(AtomicUsize::new(commit_page_size)),
            askpass: askpass.map(Arc::new),
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
                commit_page_size,
                task_tx,
                mutation_guard,
            },
            event_rx,
        ))
    }
}

fn handle_task_request(ctx: &SupervisorContext, task: ScheduledTask) {
    let ScheduledTask { req, token } = task;
    match req {
        TaskRequest::LoadRepoSnapshot => {
            let limit = ctx.commit_limit.load(Ordering::Relaxed);
            spawn_load_repo_snapshot(
                ctx,
                token.expect("RepoSnapshot token"),
                limit,
                SnapshotLoadMode::Incremental,
            );
        }
        TaskRequest::LoadRepoSnapshotFull => {
            let limit = ctx.commit_limit.load(Ordering::Relaxed);
            spawn_load_repo_snapshot(
                ctx,
                token.expect("RepoSnapshotFull token"),
                limit,
                SnapshotLoadMode::FullNoReuse,
            );
        }
        TaskRequest::LoadMoreCommits => {
            let page = ctx.scan_config.effective_commit_page_size();
            let limit = ctx
                .commit_limit
                .try_update(Ordering::Relaxed, Ordering::Relaxed, |cur| {
                    Some(cur.saturating_add(page))
                })
                .map_or(page, |prev| prev.saturating_add(page));
            spawn_load_repo_snapshot(
                ctx,
                token.expect("LoadMoreCommits token"),
                limit,
                SnapshotLoadMode::Paginate,
            );
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
            let loc = ctx.location.clone();
            let token = token.expect("FileHistory token");
            let epoch = token.epoch();
            spawn_epoch_task(
                &ctx.event_tx,
                async move {
                    load_file_history_async(&loc, &key, &token)
                        .await
                        .map(Arc::new)
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
                let res = load_blame_async(&pool, &loc, &key, &token)
                    .await
                    .map(Arc::new);
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
                .or_else(|| crate::cli::require_git_executable().ok());
            let token = token.expect("CheckRemoteTag token");
            let epoch = token.epoch();
            let tx = ctx.event_tx.clone();
            tokio::spawn(async move {
                if token.is_stale() {
                    return;
                }
                let exists = match git_exe {
                    Some(exe) => check_remote_tag_async(&exe, &loc, &remote, &tag).await,
                    None => None,
                };
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
        TaskRequest::CheckPatch { key } => {
            let loc = ctx.location.clone();
            let token = token.expect("CheckPatch token");
            let epoch = token.epoch();
            spawn_epoch_task(
                &ctx.event_tx,
                async move { check_patch_async(&loc, &key, &token).await.map(Arc::new) },
                move |check| EngineEvent::PatchChecked { check, epoch },
            );
        }
        TaskRequest::LoadTree { key } => {
            let pool = Arc::clone(&ctx.pool);
            let loc = ctx.location.clone();
            let token = token.expect("LoadTree token");
            let epoch = token.epoch();
            let tx = ctx.event_tx.clone();
            tokio::spawn(async move {
                let fail_key = key.clone();
                let res = pool
                    .run_epoch(TaskPriority::Interactive, token, move |tok| {
                        load_tree_sync(&loc, &key, tok).map(Arc::new)
                    })
                    .await;
                match res {
                    Ok(listing) => {
                        let _ = tx.send(EngineEvent::TreeLoaded { listing, epoch });
                    }
                    Err(EngineError::Cancelled) => {}
                    Err(e) => {
                        let _ = tx.send(EngineEvent::TreeLoadFailed {
                            key: fail_key,
                            message: e.to_string(),
                            epoch,
                        });
                    }
                }
            });
        }
        TaskRequest::LoadTreeBlob { slot, key } => {
            let pool = Arc::clone(&ctx.pool);
            let loc = ctx.location.clone();
            let token = token.expect("LoadTreeBlob token");
            let epoch = token.epoch();
            spawn_epoch_task(
                &ctx.event_tx,
                async move {
                    pool.run_epoch(TaskPriority::Interactive, token, move |tok| {
                        load_tree_blob_sync(&loc, &key, tok).map(Arc::new)
                    })
                    .await
                },
                move |blob| EngineEvent::TreeBlobLoaded { slot, blob, epoch },
            );
        }
        TaskRequest::LoadTreeIndex { oid } => {
            let loc = ctx.location.clone();
            let token = token.expect("LoadTreeIndex token");
            let epoch = token.epoch();
            spawn_epoch_task(
                &ctx.event_tx,
                async move { load_tree_index_async(&loc, oid, &token).await.map(Arc::new) },
                move |paths| EngineEvent::TreeIndexLoaded { oid, paths, epoch },
            );
        }
        TaskRequest::ExportTreeBlob { oid, path, dest } => {
            let pool = Arc::clone(&ctx.pool);
            let loc = ctx.location.clone();
            let tx = ctx.event_tx.clone();
            tokio::spawn(async move {
                let path_for_task = path.clone();
                let dest_for_task = dest.clone();
                let res = pool
                    .run_with(TaskPriority::Interactive, move || {
                        export_tree_blob_sync(&loc, oid, &path_for_task, &dest_for_task)
                    })
                    .await;
                let result = res.map_err(|e| e.to_string());
                let _ = tx.send(EngineEvent::TreeBlobExported { path, dest, result });
            });
        }
        TaskRequest::VerifyCommitSignatures { oids } => {
            spawn_verify_commit_signatures(ctx, oids, token.expect("LogSignatures token"));
        }
        TaskRequest::LoadCherryPicked { key } => {
            let loc = ctx.location.clone();
            let token = token.expect("LogCherryPicked token");
            let epoch = token.epoch();
            let reported_key = key.clone();
            spawn_epoch_task(
                &ctx.event_tx,
                async move {
                    load_cherry_picked_async(&loc, &key, &token)
                        .await
                        .map(Arc::new)
                },
                move |oids| EngineEvent::CherryPickedLoaded {
                    key: reported_key,
                    oids,
                    epoch,
                },
            );
        }
        TaskRequest::LoadReflog { key } => {
            let loc = ctx.location.clone();
            let token = token.expect("Reflog token");
            let epoch = token.epoch();
            let tx = ctx.event_tx.clone();
            tokio::spawn(async move {
                let fail_key = key.clone();
                let res = load_reflog_async(&loc, &key, &token).await.map(Arc::new);
                match res {
                    Ok(bundle) => {
                        let _ = tx.send(EngineEvent::ReflogLoaded { bundle, epoch });
                    }
                    Err(EngineError::Cancelled) => {}
                    Err(err) => {
                        let _ = tx.send(EngineEvent::ReflogFailed {
                            key: fail_key,
                            message: err.to_string(),
                            epoch,
                        });
                    }
                }
            });
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
            let token = token.expect("ProbeCommitPrecheck token");
            let epoch = token.epoch();
            let tx = ctx.event_tx.clone();
            tokio::spawn(async move {
                if token.is_stale() {
                    return;
                }
                let info = pool
                    .run_epoch(TaskPriority::Background, token.clone(), move |_| {
                        Ok(probe_commit_precheck_sync(&loc))
                    })
                    .await;
                if let Ok(info) = info
                    && !token.is_stale()
                {
                    let _ = tx.send(EngineEvent::CommitPrecheckLoaded { info, epoch });
                }
            });
        }
        TaskRequest::ApplyStageBuffer {
            path,
            new_staged_bytes,
            file_mode,
            remove_from_index,
        } => spawn_guarded_mutation(
            ctx,
            RefreshScope::StageStatus,
            move |pool, loc| async move {
                pool.run_with(TaskPriority::Interactive, move || {
                    apply_stage_buffer_sync(
                        &loc,
                        &path,
                        &new_staged_bytes,
                        file_mode,
                        remove_from_index,
                    )
                })
                .await
            },
        ),
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
            spawn_guarded_mutation(
                ctx,
                RefreshScope::StageStatus,
                move |pool, loc| async move {
                    pool.run_with(TaskPriority::Interactive, move || {
                        stage_targets_sync(&loc, &targets)
                    })
                    .await
                },
            );
        }
        TaskRequest::StageAllTracked => {
            spawn_guarded_mutation(
                ctx,
                RefreshScope::StageStatus,
                move |pool, loc| async move {
                    pool.run_with(TaskPriority::Interactive, move || {
                        stage_all_tracked_sync(&loc)
                    })
                    .await
                },
            );
        }
        TaskRequest::UnstageTargets { targets } => {
            spawn_guarded_mutation(
                ctx,
                RefreshScope::StageStatus,
                move |pool, loc| async move {
                    pool.run_with(TaskPriority::Interactive, move || {
                        unstage_targets_sync(&loc, &targets)
                    })
                    .await
                },
            );
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
                move |loaded| match loaded {
                    LoadedConflict::Text(doc) => EngineEvent::ConflictDocumentLoaded { doc, epoch },
                    LoadedConflict::Special(info) => {
                        EngineEvent::SpecialConflictLoaded { info, epoch }
                    }
                },
            );
        }
        TaskRequest::SaveConflictResolution {
            path,
            content,
            stage,
            delete,
        } => {
            let pool = Arc::clone(&ctx.pool);
            let loc = ctx.location.clone();
            let tx = ctx.event_tx.clone();
            let guard = Arc::clone(&ctx.mutation_guard);
            tokio::spawn(async move {
                let permit = guard.acquire().await;
                let res: Result<(), (EngineError, RefreshScope)> = pool
                    .run_with(TaskPriority::Interactive, move || {
                        if delete {
                            if let Err(e) = remove_conflict_path_sync(&loc, &path, stage) {
                                return Ok(Err((e, RefreshScope::None)));
                            }
                        } else {
                            if let Err(e) = write_worktree_buffer_sync(&loc, &path, &content) {
                                return Ok(Err((e, RefreshScope::None)));
                            }
                            if stage
                                && let Err(e) = stage_targets_sync(
                                    &loc,
                                    &[StageTarget {
                                        path,
                                        old_path: None,
                                        status_kind: FileStatusKind::Conflicted,
                                        group_kind: StageNodeKind::Conflicted,
                                        is_submodule: false,
                                    }],
                                )
                            {
                                // Worktree file was already written before staging failed.
                                return Ok(Err((e, RefreshScope::WorktreeStatus)));
                            }
                        }
                        Ok(Ok(()))
                    })
                    .await
                    .unwrap_or_else(|e| Err((e, RefreshScope::None)));
                let deferred = permit.finish();
                match res {
                    Ok(()) => {
                        let _ = tx.send(EngineEvent::ExternalFsChanged(
                            RefreshScope::StageStatus.merge(deferred),
                        ));
                    }
                    Err((e, partial_scope)) => {
                        let _ = tx.send(EngineEvent::Error(e.to_string()));
                        let merged = partial_scope.merge(deferred);
                        if merged != RefreshScope::None {
                            let _ = tx.send(EngineEvent::ExternalFsChanged(merged));
                        }
                    }
                }
            });
        }
        TaskRequest::RunGitCli {
            id,
            args,
            stdin_payload,
            env,
            is_read_only,
            refresh_after,
        } => spawn_run_git_cli(
            ctx,
            id,
            args,
            stdin_payload,
            env,
            is_read_only,
            refresh_after,
        ),
    }
}

fn spawn_load_repo_snapshot(
    ctx: &SupervisorContext,
    token: EpochToken,
    max_commits: usize,
    mode: SnapshotLoadMode,
) {
    let pool = Arc::clone(&ctx.pool);
    let loc = ctx.location.clone();
    let scan_config = ctx.scan_config;
    let snapshot_cache = Arc::clone(&ctx.snapshot_cache);
    let epoch = token.epoch();
    let tx = ctx.event_tx.clone();
    tokio::spawn(async move {
        let prev_cache = if mode == SnapshotLoadMode::FullNoReuse {
            None
        } else {
            snapshot_cache
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone()
        };
        let loc_snap = loc.clone();
        let snap_res = pool
            .run_epoch(TaskPriority::Background, token.clone(), move |tok| {
                let (snap, pending, new_cache, _kind) = load_repo_snapshot_incremental_sync(
                    &loc_snap,
                    tok,
                    max_commits,
                    scan_config,
                    prev_cache.as_ref(),
                    mode,
                )?;
                Ok((Arc::new(snap), pending, new_cache))
            })
            .await;

        match snap_res {
            Ok((snapshot, pending, new_cache)) => {
                if !token.is_stale() {
                    *snapshot_cache.lock().unwrap_or_else(|e| e.into_inner()) = Some(new_cache);
                    let _ = tx.send(EngineEvent::RepoSnapshotLoaded { snapshot, epoch });
                }
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
    let details_token = ctx.slots.token_for(slot, epoch);
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
                        if d.signature.status == SignatureStatus::Pending {
                            if let Some(cached) = cache.get(&d.oid).cloned() {
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
                    let git_dir = loc.git_dir;
                    tokio::task::spawn_blocking(move || {
                        for oid in unverified_oids {
                            if details_token.is_stale() {
                                break;
                            }
                            let signature = verify_commit_signature_sync(&git_dir, oid);
                            {
                                let mut cache = sig_cache.lock().unwrap_or_else(|e| e.into_inner());
                                cache.insert(oid, signature.clone());
                            }
                            if !details_token.is_stale() {
                                let _ = tx
                                    .send(EngineEvent::CommitSignatureVerified { oid, signature });
                            }
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

fn spawn_verify_commit_signatures(ctx: &SupervisorContext, oids: Vec<Oid>, token: EpochToken) {
    let git_dir = ctx.location.git_dir.clone();
    let tx = ctx.event_tx.clone();
    let sig_cache = Arc::clone(&ctx.sig_cache);
    let epoch = token.epoch();
    tokio::spawn(async move {
        if token.is_stale() {
            return;
        }
        let mut results = Vec::with_capacity(oids.len());
        let mut uncached = Vec::new();
        {
            let mut cache = sig_cache.lock().unwrap_or_else(|e| e.into_inner());
            for oid in oids {
                if let Some(cached) = cache.get(&oid).cloned() {
                    results.push((oid, cached));
                } else {
                    uncached.push(oid);
                }
            }
        }
        if !uncached.is_empty() {
            let sig_token = token.clone();
            let cache_ref = Arc::clone(&sig_cache);
            let verified = tokio::task::spawn_blocking(move || {
                verify_commit_signatures_batch_sync(&git_dir, &uncached, &sig_token)
            })
            .await
            .unwrap_or_default();
            if token.is_stale() {
                return;
            }
            {
                let mut cache = cache_ref.lock().unwrap_or_else(|e| e.into_inner());
                for (oid, sig) in &verified {
                    cache.insert(*oid, sig.clone());
                }
            }
            results.extend(verified);
        }
        if !token.is_stale() {
            let _ = tx.send(EngineEvent::CommitSignaturesLoaded {
                signatures: results,
                epoch,
            });
        }
    });
}

fn spawn_execute_commit(ctx: &SupervisorContext, id: u64, mut request: CommitRequest) {
    let pool = Arc::clone(&ctx.pool);
    let cli_exec = ctx.cli.as_ref().map(Arc::clone);
    let cli_error = ctx.cli_error.clone();
    let loc = ctx.location.clone();
    let tx = ctx.event_tx.clone();
    let guard = Arc::clone(&ctx.mutation_guard);
    let op_ids = Arc::clone(&ctx.op_ids);
    tokio::spawn(async move {
        let permit = guard.acquire().await;
        let finish_permit =
            |permit: MutationPermit, tx: &UnboundedSender<EngineEvent>, extra: RefreshScope| {
                let scope = extra.merge(permit.finish());
                if scope != RefreshScope::None {
                    let _ = tx.send(EngineEvent::ExternalFsChanged(scope));
                }
            };
        if request.check_crlf {
            let loc_crlf = loc.clone();
            let stage_all = request.stage_all_tracked;
            if let Ok(Some(problem)) = pool
                .run_with(TaskPriority::Interactive, move || {
                    crate::stage::detect_crlf_commit_problem_sync(&loc_crlf, stage_all)
                })
                .await
            {
                finish_permit(permit, &tx, RefreshScope::None);
                let _ = tx.send(EngineEvent::CommitCrlfWarning {
                    request,
                    files: problem.files,
                });
                return;
            }
        }
        let mut staged_tracked_early = false;
        let mut mutated_index = false;
        if request.fix_autocrlf {
            let fixed = if let Some(cli) = cli_exec.as_ref() {
                let cfg_id = crate::ops::OP_CONSOLE_ID_BASE
                    + op_ids.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let args = vec![
                    "config".to_string(),
                    "core.autocrlf".to_string(),
                    "true".to_string(),
                ];
                let fwd = tx.clone();
                cli.execute(
                    CliCommand {
                        id: cfg_id,
                        workdir: &loc.workdir,
                        git_dir: &loc.git_dir,
                        args: &args,
                        stdin_payload: None,
                        is_read_only: false,
                        env: &[],
                    },
                    move |ev| forward_cli_event(&fwd, ev),
                )
                .await
                .map(|o| o.exit_code == 0)
                .unwrap_or(false)
            } else {
                false
            };
            if !fixed {
                let loc_fix = loc.clone();
                if let Err(e) = pool
                    .run_with(TaskPriority::Interactive, move || {
                        crate::stage::set_core_autocrlf_true_sync(&loc_fix)
                    })
                    .await
                {
                    finish_permit(permit, &tx, RefreshScope::None);
                    let _ = tx.send(EngineEvent::CommitCompleted(CommitOutcome {
                        success: false,
                        commit_oid: None,
                        short_sha: None,
                        summary: "Failed to set core.autocrlf".to_string(),
                        error_output: Some(e.to_string()),
                        and_push: false,
                        preserved_message: Some(request.message),
                    }));
                    return;
                }
            }
            if request.stage_all_tracked {
                let loc_stage = loc.clone();
                if let Err(e) = pool
                    .run_with(TaskPriority::Interactive, move || {
                        stage_all_tracked_sync(&loc_stage)
                    })
                    .await
                {
                    finish_permit(permit, &tx, RefreshScope::None);
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
                staged_tracked_early = true;
                mutated_index = true;
            }
            let loc_norm = loc.clone();
            if let Err(e) = pool
                .run_with(TaskPriority::Interactive, move || {
                    crate::stage::renormalize_staged_crlf_entries_sync(&loc_norm)
                })
                .await
            {
                let extra = if mutated_index {
                    RefreshScope::StageStatus
                } else {
                    RefreshScope::None
                };
                finish_permit(permit, &tx, extra);
                let _ = tx.send(EngineEvent::CommitCompleted(CommitOutcome {
                    success: false,
                    commit_oid: None,
                    short_sha: None,
                    summary: "Failed to normalize staged CRLF entries".to_string(),
                    error_output: Some(e.to_string()),
                    and_push: false,
                    preserved_message: Some(request.message),
                }));
                return;
            }
            mutated_index = true;
        }
        let loc_pre = loc.clone();
        let precheck = pool
            .run_with(TaskPriority::Interactive, move || {
                Ok(probe_commit_precheck_sync(&loc_pre))
            })
            .await
            .unwrap_or_default();
        let effective_gpg = request.gpg_sign.unwrap_or(precheck.gpg_sign);
        let needs_cli = (precheck.has_commit_hooks && request.run_hooks)
            || effective_gpg
            || request.gpg_sign.is_some()
            || precheck.has_sequencer_state;

        if needs_cli && cli_exec.is_none() {
            let extra = if mutated_index {
                RefreshScope::StageStatus
            } else {
                RefreshScope::None
            };
            finish_permit(permit, &tx, extra);
            let detail = cli_error.unwrap_or_else(|| {
                "Git CLI is required for signed commits, commit hooks, or merge/sequencer commits."
                    .to_string()
            });
            let _ = tx.send(EngineEvent::CommitCompleted(CommitOutcome {
                success: false,
                commit_oid: None,
                short_sha: None,
                summary: "Git CLI unavailable for commit".to_string(),
                error_output: Some(detail),
                and_push: false,
                preserved_message: Some(request.message),
            }));
            return;
        }

        let (outcome, index_lock) = if needs_cli {
            if request.stage_all_tracked && !staged_tracked_early {
                let loc_stage = loc.clone();
                if let Err(e) = pool
                    .run_with(TaskPriority::Interactive, move || {
                        stage_all_tracked_sync(&loc_stage)
                    })
                    .await
                {
                    let extra = if mutated_index {
                        RefreshScope::StageStatus
                    } else {
                        RefreshScope::None
                    };
                    finish_permit(permit, &tx, extra);
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
                mutated_index = true;
            }
            let cli = cli_exec.expect("checked is_some");
            let fwd = tx.clone();
            execute_commit_cli_async(cli.as_ref(), &loc, id, request, move |ev| {
                forward_cli_event(&fwd, ev);
            })
            .await
        } else {
            if staged_tracked_early {
                request.stage_all_tracked = false;
            }
            if request.stage_all_tracked {
                // `execute_commit_git2_sync` writes updated tracked entries to `.git/index`
                // before validating the commit message or signature.
                mutated_index = true;
            }
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
        let deferred = permit.finish();
        if let Some(conflict) = index_lock {
            let _ = tx.send(EngineEvent::IndexLockDetected {
                lock_path: conflict.lock_path,
                message: conflict.message,
                retry_args: conflict.retry_args,
                retry_stdin: conflict.retry_stdin,
                retry_env: conflict.retry_env,
                retry_is_read_only: conflict.retry_is_read_only,
                retry_refresh_after: RefreshScope::All,
            });
        }
        let _ = tx.send(EngineEvent::CommitCompleted(outcome));
        let scope = if ok {
            RefreshScope::All.merge(deferred)
        } else if mutated_index {
            RefreshScope::StageStatus.merge(deferred)
        } else {
            deferred
        };
        if scope != RefreshScope::None {
            let _ = tx.send(EngineEvent::ExternalFsChanged(scope));
        }
    });
}

fn spawn_undo_last_commit(ctx: &SupervisorContext) {
    let pool = Arc::clone(&ctx.pool);
    let loc = ctx.location.clone();
    let tx = ctx.event_tx.clone();
    let guard = Arc::clone(&ctx.mutation_guard);
    tokio::spawn(async move {
        let permit = guard.acquire().await;
        let res = pool
            .run_with(TaskPriority::Interactive, move || {
                undo_last_commit_sync(&loc)
            })
            .await;
        let deferred = permit.finish();
        match res {
            Ok(restored_message) => {
                let _ = tx.send(EngineEvent::UndoCommitCompleted { restored_message });
                let _ = tx.send(EngineEvent::ExternalFsChanged(
                    RefreshScope::All.merge(deferred),
                ));
            }
            Err(e) => {
                let _ = tx.send(EngineEvent::Error(e.to_string()));
                if deferred != RefreshScope::None {
                    let _ = tx.send(EngineEvent::ExternalFsChanged(deferred));
                }
            }
        }
    });
}

fn spawn_run_git_op(ctx: &SupervisorContext, op: GitOp) {
    let Some(cli_exec) = ctx.cli.as_ref().map(Arc::clone) else {
        let msg = ctx
            .cli_error
            .clone()
            .unwrap_or_else(|| EngineError::GitNotFound.to_string());
        let _ = ctx.event_tx.send(EngineEvent::Error(msg));
        return;
    };
    let loc = ctx.location.clone();
    let tx = ctx.event_tx.clone();
    let guard = Arc::clone(&ctx.mutation_guard);
    let ids = Arc::clone(&ctx.op_ids);
    let askpass = ctx.askpass.clone();
    tokio::spawn(async move {
        let read_only = op.is_read_only();
        let refresh = op.refresh_scope();
        let permit = if read_only {
            None
        } else {
            Some(guard.acquire().await)
        };
        let fwd = tx.clone();
        let res = crate::ops::run_git_op(
            cli_exec.as_ref(),
            &loc,
            &ids,
            op,
            askpass.as_deref(),
            move |ev| {
                forward_cli_event(&fwd, ev);
            },
        )
        .await;
        let deferred = permit.map_or(RefreshScope::None, MutationPermit::finish);
        if let Some(conflict) = res.index_lock {
            let _ = tx.send(EngineEvent::IndexLockDetected {
                lock_path: conflict.lock_path,
                message: conflict.message,
                retry_args: conflict.retry_args,
                retry_stdin: conflict.retry_stdin,
                retry_env: conflict.retry_env,
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
    env: Vec<(String, String)>,
    is_read_only: bool,
    refresh_after: RefreshScope,
) {
    let Some(cli_exec) = ctx.cli.as_ref().map(Arc::clone) else {
        let msg = ctx
            .cli_error
            .clone()
            .unwrap_or_else(|| EngineError::GitNotFound.to_string());
        let _ = ctx.event_tx.send(EngineEvent::Error(msg));
        return;
    };
    let loc = ctx.location.clone();
    let tx = ctx.event_tx.clone();
    let guard = Arc::clone(&ctx.mutation_guard);
    tokio::spawn(async move {
        let permit = if is_read_only {
            None
        } else {
            Some(guard.acquire().await)
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
                    env: &env,
                },
                |ev| forward_cli_event(&tx, ev),
            )
            .await;

        let deferred = permit.map_or(RefreshScope::None, MutationPermit::finish);

        match res {
            Ok(out) => {
                if let Some((lock_path, message)) = out.index_lock_conflict {
                    let _ = tx.send(EngineEvent::IndexLockDetected {
                        lock_path,
                        message,
                        retry_args: args,
                        retry_stdin: stdin_payload,
                        retry_env: env,
                        retry_is_read_only: is_read_only,
                        retry_refresh_after: refresh_after,
                    });
                }
                let merged = refresh_after.merge(deferred);
                if merged != RefreshScope::None {
                    let _ = tx.send(EngineEvent::ExternalFsChanged(merged));
                }
            }
            Err(e) => {
                let _ = tx.send(EngineEvent::Error(e.to_string()));
                if deferred != RefreshScope::None {
                    let _ = tx.send(EngineEvent::ExternalFsChanged(deferred));
                }
            }
        }
    });
}

pub(crate) fn forward_cli_event(tx: &UnboundedSender<EngineEvent>, ev: CliStreamEvent) {
    let _ = tx.send(match ev {
        CliStreamEvent::Started(entry) => EngineEvent::ConsoleStarted(entry),
        CliStreamEvent::Output { id, stream, line } => {
            EngineEvent::ConsoleOutput { id, stream, line }
        }
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

fn spawn_guarded_mutation<F, Fut>(ctx: &SupervisorContext, scope: RefreshScope, op: F)
where
    F: FnOnce(Arc<Git2WorkerPool>, RepoLocation) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = Result<(), EngineError>> + Send + 'static,
{
    let pool = Arc::clone(&ctx.pool);
    let loc = ctx.location.clone();
    let tx = ctx.event_tx.clone();
    let guard = Arc::clone(&ctx.mutation_guard);
    tokio::spawn(async move {
        let permit = guard.acquire().await;
        let res = op(pool, loc).await;
        let deferred = permit.finish();
        match res {
            Ok(()) => {
                let _ = tx.send(EngineEvent::ExternalFsChanged(scope.merge(deferred)));
            }
            Err(e) => {
                let _ = tx.send(EngineEvent::Error(e.to_string()));
                if deferred != RefreshScope::None {
                    let _ = tx.send(EngineEvent::ExternalFsChanged(deferred));
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::process::Command;
    use std::time::Duration;

    use tempfile::TempDir;

    use super::*;

    fn git(dir: &Path, args: &[&str]) {
        let out = Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_AUTHOR_NAME", "Test")
            .env("GIT_AUTHOR_EMAIL", "test@example.com")
            .env("GIT_COMMITTER_NAME", "Test")
            .env("GIT_COMMITTER_EMAIL", "test@example.com")
            .output()
            .expect("git command");
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn init_repo_with_commit() -> (TempDir, RepoLocation) {
        let tmp = TempDir::new().unwrap();
        let d = tmp.path();
        git(d, &["init", "-b", "main"]);
        git(d, &["config", "user.name", "Test"]);
        git(d, &["config", "user.email", "test@example.com"]);
        git(d, &["config", "commit.gpgsign", "false"]);
        std::fs::write(d.join("a.txt"), "v1\n").unwrap();
        git(d, &["add", "a.txt"]);
        git(d, &["commit", "-m", "initial"]);
        let loc = RepoLocation::discover(d).unwrap();
        (tmp, loc)
    }

    #[tokio::test]
    async fn read_only_cli_and_git_op_do_not_steal_pending_mutation_refresh() {
        let (_tmp, loc) = init_repo_with_commit();
        let (handle, mut rx) = EngineSupervisor::start(loc, false).unwrap();

        // Simulate an active write mutation with deferred watcher events.
        let permit = handle.mutation_guard.acquire().await;
        assert!(
            handle
                .mutation_guard
                .record_or_defer(RefreshScope::StageStatus)
                .is_none()
        );

        // 1. Read-only RunGitCli completes while the mutation permit is still held.
        handle.dispatch(TaskRequest::RunGitCli {
            id: 42,
            args: vec!["rev-parse".into(), "HEAD".into()],
            stdin_payload: None,
            env: Vec::new(),
            is_read_only: true,
            refresh_after: RefreshScope::None,
        });
        let mut cli_done = false;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while !cli_done {
            let ev = tokio::time::timeout_at(deadline, rx.recv())
                .await
                .expect("timeout waiting for ConsoleFinished")
                .expect("channel open");
            match ev {
                EngineEvent::ExternalFsChanged(scope) => {
                    panic!("read-only RunGitCli stole deferred refresh: {scope:?}");
                }
                EngineEvent::ConsoleFinished { id: 42, .. } => cli_done = true,
                _ => {}
            }
        }

        // 2. Read-only RunGitOp completes while the mutation permit is still held.
        handle.dispatch(TaskRequest::RunGitOp {
            op: GitOp::Sequence(Vec::new()),
        });
        let mut op_done = false;
        while !op_done {
            let ev = tokio::time::timeout_at(deadline, rx.recv())
                .await
                .expect("timeout waiting for GitOpFinished")
                .expect("channel open");
            match ev {
                EngineEvent::ExternalFsChanged(scope) => {
                    panic!("read-only RunGitOp stole deferred refresh: {scope:?}");
                }
                EngineEvent::GitOpFinished(_) => op_done = true,
                _ => {}
            }
        }

        // The real mutator must still receive its deferred refresh scope.
        assert_eq!(permit.finish(), RefreshScope::StageStatus);
        assert!(handle.shutdown(Duration::from_secs(2)).await);
    }

    #[tokio::test]
    async fn failed_commit_after_stage_all_and_failed_undo_preserve_refresh_scopes() {
        let (tmp, loc) = init_repo_with_commit();
        std::fs::write(tmp.path().join("a.txt"), "v2\n").unwrap();
        let (handle, mut rx) = EngineSupervisor::start(loc, false).unwrap();

        // 1. ExecuteCommit with `stage_all_tracked: true` mutates `.git/index`, then fails
        // on empty commit message — must still emit StageStatus so UI syncs with the index.
        handle.dispatch(TaskRequest::ExecuteCommit {
            id: 1,
            request: CommitRequest {
                message: "   ".into(),
                amend: false,
                stage_all_tracked: true,
                allow_empty: false,
                signoff: false,
                and_push: false,
                gpg_sign: None,
                author_override: None,
                run_hooks: false,
                check_crlf: false,
                fix_autocrlf: false,
            },
        });
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        let mut saw_failed_commit = false;
        let mut saw_stage_refresh = false;
        while !(saw_failed_commit && saw_stage_refresh) {
            let ev = tokio::time::timeout_at(deadline, rx.recv())
                .await
                .expect("timeout waiting for failed commit + stage refresh")
                .expect("channel open");
            match ev {
                EngineEvent::CommitCompleted(outcome) => {
                    assert!(!outcome.success);
                    saw_failed_commit = true;
                }
                EngineEvent::ExternalFsChanged(scope) => {
                    assert_eq!(scope, RefreshScope::StageStatus);
                    saw_stage_refresh = true;
                }
                _ => {}
            }
        }

        // 2. Deterministic partial-failure test on `SaveConflictResolution`:
        // Hold `index.lock` so `SaveConflictResolution` writes the worktree file and then
        // fails during `stage_targets_sync`, emitting both Error and WorktreeStatus refresh.
        let lock_path = tmp.path().join(".git").join("index.lock");
        std::fs::write(&lock_path, "locked").unwrap();
        let hold = handle.mutation_guard.acquire().await;
        handle.dispatch(TaskRequest::SaveConflictResolution {
            path: "a.txt".into(),
            content: b"resolved\n".to_vec(),
            stage: true,
            delete: false,
        });
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert_eq!(hold.finish(), RefreshScope::None);
        let mut saw_conflict_err = false;
        let mut saw_partial_refresh = false;
        while !(saw_conflict_err && saw_partial_refresh) {
            let ev = tokio::time::timeout_at(deadline, rx.recv())
                .await
                .expect("timeout waiting for SaveConflictResolution error + refresh")
                .expect("channel open");
            match ev {
                EngineEvent::Error(_) => saw_conflict_err = true,
                EngineEvent::ExternalFsChanged(scope) => {
                    assert_eq!(scope, RefreshScope::WorktreeStatus);
                    saw_partial_refresh = true;
                }
                _ => {}
            }
        }
        let _ = std::fs::remove_file(&lock_path);
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("a.txt")).unwrap(),
            "resolved\n"
        );

        assert!(handle.shutdown(Duration::from_secs(2)).await);
    }

    #[tokio::test]
    async fn failed_guarded_mutation_flushes_deferred_watcher_events() {
        // Unborn repo (zero commits) guarantees `UndoLastCommit` fails deterministically.
        let tmp = TempDir::new().unwrap();
        git(tmp.path(), &["init", "-b", "main"]);
        let loc = RepoLocation::discover(tmp.path()).unwrap();
        let (handle, mut rx) = EngineSupervisor::start(loc, false).unwrap();

        // Queue `UndoLastCommit` behind `hold`, record a deferred watcher scope, then drop
        // `hold` without calling `finish()` so the failing `UndoLastCommit` owns the drain.
        let hold = handle.mutation_guard.acquire().await;
        handle.dispatch(TaskRequest::UndoLastCommit);
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert!(
            handle
                .mutation_guard
                .record_or_defer(RefreshScope::RefsAndGraph)
                .is_none()
        );
        drop(hold);

        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        let mut saw_err = false;
        let mut saw_deferred = false;
        while !(saw_err && saw_deferred) {
            let ev = tokio::time::timeout_at(deadline, rx.recv())
                .await
                .expect("timeout waiting for failed UndoLastCommit error + deferred refresh")
                .expect("channel open");
            match ev {
                EngineEvent::Error(_) => saw_err = true,
                EngineEvent::ExternalFsChanged(scope) => {
                    assert_eq!(scope, RefreshScope::RefsAndGraph);
                    saw_deferred = true;
                }
                _ => {}
            }
        }
        assert!(handle.shutdown(Duration::from_secs(2)).await);
    }
}
