use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use jbgit_core::{
    AmendCommitPreview, CommitChangesBundle, ConflictDocument, DetailsKey, EpochToken,
    EpochTracker, FileDiff, FileDiffKey, GitConsoleEntry, GitOp, GitOpOutcome, Oid, RefreshScope,
    RepoSnapshot, StageStatusSnapshot, StageTarget,
};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

use crate::cli::{CliCommand, CliStreamEvent, GitCliExecutor, GitExecutableInfo};
use crate::error::EngineError;
use crate::pool::Git2WorkerPool;
use crate::probe::{RepoLocation, probe_repo_state};
use crate::stage::{
    CommitOutcome, CommitPrecheckInfo, CommitRequest, StageDiffKey, ThreeDocStageDiff,
    build_commit_cli_args,
};
use crate::watcher::{MutationGuard, RepoWatcher};

const DEFAULT_MAX_COMMITS: usize = 50_000;

#[derive(Debug, Clone)]
pub enum TaskRequest {
    LoadRepoSnapshot {
        epoch: u64,
    },
    /// Commit metadata + (net) changed-file list for the selected commits.
    LoadCommitDetails {
        key: DetailsKey,
        epoch: u64,
    },
    /// Line diff of one file of a details selection (diff preview).
    LoadFileDiff {
        key: FileDiffKey,
        epoch: u64,
    },
    /// Commits of `snapshot` touching any of `paths` (log `Paths` filter).
    LoadPathMatches {
        paths: Vec<String>,
        snapshot: Arc<RepoSnapshot>,
        epoch: u64,
    },
    LoadStageStatus {
        include_ignored: bool,
        epoch: u64,
    },
    LoadStageFileDiff {
        key: StageDiffKey,
        epoch: u64,
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

#[derive(Debug, Clone)]
pub enum EngineEvent {
    RepoSnapshotLoaded {
        snapshot: Arc<RepoSnapshot>,
        epoch: u64,
    },
    CommitDetailsLoaded {
        bundle: Arc<CommitChangesBundle>,
        epoch: u64,
    },
    FileDiffLoaded {
        diff: Arc<FileDiff>,
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
        is_stderr: bool,
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
    },
    Error(String),
}

pub struct EngineHandle {
    pub location: RepoLocation,
    pub epochs: EpochTracker,
    pub git_info: Option<GitExecutableInfo>,
    task_tx: UnboundedSender<TaskRequest>,
}

impl EngineHandle {
    pub fn send(&self, req: TaskRequest) {
        let _ = self.task_tx.send(req);
    }

    pub fn request_repo_snapshot(&self) -> u64 {
        let epoch = self.epochs.repo_snapshot.next_epoch();
        self.send(TaskRequest::LoadRepoSnapshot { epoch });
        epoch
    }

    pub fn request_stage_status(&self, include_ignored: bool) -> u64 {
        let epoch = self.epochs.stage_status.next_epoch();
        self.send(TaskRequest::LoadStageStatus {
            include_ignored,
            epoch,
        });
        epoch
    }

    pub fn request_commit_details(&self, key: DetailsKey) -> u64 {
        let epoch = self.epochs.commit_details.next_epoch();
        self.send(TaskRequest::LoadCommitDetails { key, epoch });
        epoch
    }

    pub fn request_file_diff(&self, key: FileDiffKey) -> u64 {
        let epoch = self.epochs.file_diff.next_epoch();
        self.send(TaskRequest::LoadFileDiff { key, epoch });
        epoch
    }

    pub fn request_path_matches(&self, paths: Vec<String>, snapshot: Arc<RepoSnapshot>) -> u64 {
        let epoch = self.epochs.path_filter.next_epoch();
        self.send(TaskRequest::LoadPathMatches {
            paths,
            snapshot,
            epoch,
        });
        epoch
    }

    pub fn request_stage_file_diff(&self, key: StageDiffKey) -> u64 {
        let epoch = self.epochs.stage_diff.next_epoch();
        self.send(TaskRequest::LoadStageFileDiff { key, epoch });
        epoch
    }

    pub fn request_amend_preview(&self) {
        self.send(TaskRequest::LoadAmendPreview);
    }

    pub fn request_commit_precheck(&self) {
        self.send(TaskRequest::ProbeCommitPrecheck);
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

pub struct EngineSupervisor;

impl EngineSupervisor {
    /// Spawns the engine supervisor, worker pool, CLI executor, and FS watcher.
    /// Returns an `EngineHandle` for sending tasks and an `UnboundedReceiver<EngineEvent>`
    /// for consuming reactive snapshots and console events.
    pub fn start(
        location: RepoLocation,
        enable_watcher: bool,
    ) -> Result<(EngineHandle, UnboundedReceiver<EngineEvent>), EngineError> {
        let epochs = EpochTracker::default();
        let pool = Arc::new(Git2WorkerPool::new(4));
        let cli = GitCliExecutor::detect().ok();
        let git_info = cli.as_ref().map(GitCliExecutor::info);
        let mutation_guard = Arc::new(MutationGuard::new());

        let (task_tx, mut task_rx) = mpsc::unbounded_channel::<TaskRequest>();
        let (event_tx, event_rx) = mpsc::unbounded_channel::<EngineEvent>();

        let watcher = if enable_watcher {
            let ev_tx = event_tx.clone();
            match RepoWatcher::start(
                location.clone(),
                Arc::clone(&pool),
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

        let sup_location = location.clone();
        let sup_epochs = epochs.clone();
        let sup_cli = cli.map(Arc::new);
        let op_ids = Arc::new(std::sync::atomic::AtomicU64::new(1));

        tokio::spawn(async move {
            let _keep_watcher = watcher;

            while let Some(req) = task_rx.recv().await {
                match req {
                    TaskRequest::LoadRepoSnapshot { epoch } => {
                        let pool = Arc::clone(&pool);
                        let loc = sup_location.clone();
                        let token = EpochToken::new(sup_epochs.repo_snapshot.clone(), epoch);
                        spawn_epoch_task(
                            &event_tx,
                            async move {
                                pool.load_repo_snapshot(loc, token, DEFAULT_MAX_COMMITS)
                                    .await
                            },
                            move |snapshot| EngineEvent::RepoSnapshotLoaded { snapshot, epoch },
                        );
                    }
                    TaskRequest::LoadCommitDetails { key, epoch } => {
                        let pool = Arc::clone(&pool);
                        let loc = sup_location.clone();
                        let token = EpochToken::new(sup_epochs.commit_details.clone(), epoch);
                        spawn_epoch_task(
                            &event_tx,
                            async move { pool.load_commit_details(loc, key, token).await },
                            move |bundle| EngineEvent::CommitDetailsLoaded { bundle, epoch },
                        );
                    }
                    TaskRequest::LoadFileDiff { key, epoch } => {
                        let pool = Arc::clone(&pool);
                        let loc = sup_location.clone();
                        let token = EpochToken::new(sup_epochs.file_diff.clone(), epoch);
                        spawn_epoch_task(
                            &event_tx,
                            async move { pool.load_file_diff(loc, key, token).await },
                            move |diff| EngineEvent::FileDiffLoaded { diff, epoch },
                        );
                    }
                    TaskRequest::LoadPathMatches {
                        paths,
                        snapshot,
                        epoch,
                    } => {
                        let pool = Arc::clone(&pool);
                        let loc = sup_location.clone();
                        let token = EpochToken::new(sup_epochs.path_filter.clone(), epoch);
                        let reported = paths.clone();
                        spawn_epoch_task(
                            &event_tx,
                            async move { pool.load_path_matches(loc, paths, snapshot, token).await },
                            move |matches| EngineEvent::PathMatchesLoaded {
                                paths: reported,
                                matches,
                                epoch,
                            },
                        );
                    }
                    TaskRequest::LoadStageStatus {
                        include_ignored,
                        epoch,
                    } => {
                        let pool = Arc::clone(&pool);
                        let loc = sup_location.clone();
                        let token = EpochToken::new(sup_epochs.stage_status.clone(), epoch);
                        spawn_epoch_task(
                            &event_tx,
                            async move { pool.load_stage_status(loc, include_ignored, token).await },
                            move |status| EngineEvent::StageStatusLoaded { status, epoch },
                        );
                    }
                    TaskRequest::LoadStageFileDiff { key, epoch } => {
                        let pool = Arc::clone(&pool);
                        let loc = sup_location.clone();
                        let token = EpochToken::new(sup_epochs.stage_diff.clone(), epoch);
                        spawn_epoch_task(
                            &event_tx,
                            async move { pool.load_stage_file_diff(loc, key, token).await },
                            move |diff| EngineEvent::StageFileDiffLoaded { diff, epoch },
                        );
                    }
                    TaskRequest::LoadAmendPreview => {
                        let pool = Arc::clone(&pool);
                        let loc = sup_location.clone();
                        spawn_epoch_task(
                            &event_tx,
                            async move { pool.load_amend_preview(loc).await },
                            move |preview| EngineEvent::AmendPreviewLoaded { preview },
                        );
                    }
                    TaskRequest::ProbeCommitPrecheck => {
                        let pool = Arc::clone(&pool);
                        let loc = sup_location.clone();
                        let tx = event_tx.clone();
                        tokio::spawn(async move {
                            let info = pool.probe_commit_precheck(loc).await;
                            let _ = tx.send(EngineEvent::CommitPrecheckLoaded { info });
                        });
                    }
                    TaskRequest::ApplyStageBuffer {
                        path,
                        new_staged_bytes,
                        file_mode,
                        remove_from_index,
                    } => {
                        spawn_guarded_mutation(
                            &pool,
                            &sup_location,
                            &event_tx,
                            &mutation_guard,
                            RefreshScope::StageStatus,
                            move |pool, loc| async move {
                                pool.apply_stage_buffer(
                                    loc,
                                    path,
                                    new_staged_bytes,
                                    file_mode,
                                    remove_from_index,
                                )
                                .await
                            },
                        );
                    }
                    TaskRequest::WriteWorktreeBuffer {
                        path,
                        new_local_bytes,
                    } => {
                        spawn_guarded_mutation(
                            &pool,
                            &sup_location,
                            &event_tx,
                            &mutation_guard,
                            RefreshScope::WorktreeStatus,
                            move |pool, loc| async move {
                                pool.write_worktree_buffer(loc, path, new_local_bytes).await
                            },
                        );
                    }
                    TaskRequest::StageTargets { targets } => {
                        spawn_guarded_mutation(
                            &pool,
                            &sup_location,
                            &event_tx,
                            &mutation_guard,
                            RefreshScope::StageStatus,
                            move |pool, loc| async move { pool.stage_targets(loc, targets).await },
                        );
                    }
                    TaskRequest::StageAllTracked => {
                        spawn_guarded_mutation(
                            &pool,
                            &sup_location,
                            &event_tx,
                            &mutation_guard,
                            RefreshScope::StageStatus,
                            move |pool, loc| async move { pool.stage_all_tracked(loc).await },
                        );
                    }
                    TaskRequest::UnstageTargets { targets } => {
                        spawn_guarded_mutation(
                            &pool,
                            &sup_location,
                            &event_tx,
                            &mutation_guard,
                            RefreshScope::StageStatus,
                            move |pool, loc| async move { pool.unstage_targets(loc, targets).await },
                        );
                    }
                    TaskRequest::DiscardTargets { targets } => {
                        spawn_guarded_mutation(
                            &pool,
                            &sup_location,
                            &event_tx,
                            &mutation_guard,
                            RefreshScope::WorktreeStatus,
                            move |pool, loc| async move { pool.discard_targets(loc, targets).await },
                        );
                    }
                    TaskRequest::ExecuteCommit { id, request } => {
                        let pool = Arc::clone(&pool);
                        let cli_exec = sup_cli.as_ref().map(Arc::clone);
                        let loc = sup_location.clone();
                        let tx = event_tx.clone();
                        let guard = Arc::clone(&mutation_guard);
                        tokio::spawn(async move {
                            let token = guard.begin();
                            let precheck = pool.probe_commit_precheck(loc.clone()).await;
                            let use_cli = ((precheck.has_commit_hooks && request.run_hooks)
                                || precheck.gpg_sign)
                                && cli_exec.is_some();

                            let outcome = if use_cli {
                                if request.stage_all_tracked
                                    && let Err(e) = pool.stage_all_tracked(loc.clone()).await
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
                                let (args, stdin_payload) = build_commit_cli_args(&request);
                                let cli = cli_exec.expect("checked is_some");
                                let res = cli
                                    .execute(
                                        CliCommand {
                                            id,
                                            workdir: &loc.workdir,
                                            git_dir: &loc.git_dir,
                                            args: &args,
                                            stdin_payload: Some(&stdin_payload),
                                            is_read_only: false,
                                            env: &[],
                                        },
                                        |ev| match ev {
                                            CliStreamEvent::Started(entry) => {
                                                let _ = tx.send(EngineEvent::ConsoleStarted(entry));
                                            }
                                            CliStreamEvent::Output {
                                                id,
                                                is_stderr,
                                                line,
                                            } => {
                                                let _ = tx.send(EngineEvent::ConsoleOutput {
                                                    id,
                                                    is_stderr,
                                                    line,
                                                });
                                            }
                                            CliStreamEvent::Finished {
                                                id,
                                                exit_code,
                                                duration_ms,
                                            } => {
                                                let _ = tx.send(EngineEvent::ConsoleFinished {
                                                    id,
                                                    exit_code,
                                                    duration_ms,
                                                });
                                            }
                                        },
                                    )
                                    .await;

                                match res {
                                    Ok(out) if out.exit_code == 0 => {
                                        let oid = crate::pool::with_thread_repo(&loc.git_dir, |r| {
                                            Ok(probe_repo_state(r).head_oid)
                                        })
                                        .ok()
                                        .flatten();
                                        let short = oid.map(|o| o.short());
                                        let label = short.clone().unwrap_or_default();
                                        CommitOutcome {
                                            success: true,
                                            commit_oid: oid,
                                            short_sha: short,
                                            summary: if request.amend {
                                                format!("Amended commit {label}")
                                            } else {
                                                format!("Committed {label}")
                                            },
                                            error_output: None,
                                            and_push: request.and_push,
                                            preserved_message: None,
                                        }
                                    }
                                    Ok(out) => {
                                        let mut combined = out.stderr_lines.clone();
                                        if combined.is_empty() {
                                            combined = out.stdout_lines;
                                        } else if !out.stdout_lines.is_empty() {
                                            combined.extend(out.stdout_lines);
                                        }
                                        CommitOutcome {
                                            success: false,
                                            commit_oid: None,
                                            short_sha: None,
                                            summary: format!(
                                                "Commit failed (exit {})",
                                                out.exit_code
                                            ),
                                            error_output: Some(combined.join("\n")),
                                            and_push: false,
                                            preserved_message: Some(request.message),
                                        }
                                    }
                                    Err(e) => CommitOutcome {
                                        success: false,
                                        commit_oid: None,
                                        short_sha: None,
                                        summary: "Commit failed".to_string(),
                                        error_output: Some(e.to_string()),
                                        and_push: false,
                                        preserved_message: Some(request.message),
                                    },
                                }
                            } else {
                                match pool.execute_commit_git2(loc, request.clone()).await {
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
                                }
                            };

                            let ok = outcome.success;
                            drop(token);
                            let deferred = guard.take_pending();
                            let _ = tx.send(EngineEvent::CommitCompleted(outcome));
                            if ok {
                                let _ = tx.send(EngineEvent::ExternalFsChanged(
                                    RefreshScope::All.merge(deferred),
                                ));
                            }
                        });
                    }
                    TaskRequest::UndoLastCommit => {
                        let pool = Arc::clone(&pool);
                        let loc = sup_location.clone();
                        let tx = event_tx.clone();
                        let guard = Arc::clone(&mutation_guard);
                        tokio::spawn(async move {
                            let token = guard.begin();
                            let res = pool.undo_last_commit(loc).await;
                            drop(token);
                            let deferred = guard.take_pending();
                            match res {
                                Ok(restored_message) => {
                                    let _ = tx.send(EngineEvent::UndoCommitCompleted {
                                        restored_message,
                                    });
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
                    TaskRequest::RunGitOp { op } => {
                        let Some(cli_exec) = sup_cli.as_ref().map(Arc::clone) else {
                            let _ = event_tx
                                .send(EngineEvent::Error(EngineError::GitNotFound.to_string()));
                            continue;
                        };
                        let loc = sup_location.clone();
                        let tx = event_tx.clone();
                        let guard = Arc::clone(&mutation_guard);
                        let ids = Arc::clone(&op_ids);
                        tokio::spawn(async move {
                            let read_only = op.is_read_only();
                            let token = (!read_only).then(|| guard.begin());
                            let fwd = tx.clone();
                            let outcome =
                                crate::ops::run_git_op(&cli_exec, &loc, &ids, op, move |ev| {
                                    forward_cli_event(&fwd, ev);
                                })
                                .await;
                            drop(token);
                            let deferred = guard.take_pending();
                            let _ = tx.send(EngineEvent::GitOpFinished(outcome));
                            if !read_only {
                                let _ = tx.send(EngineEvent::ExternalFsChanged(
                                    RefreshScope::All.merge(deferred),
                                ));
                            }
                        });
                    }
                    TaskRequest::LoadConflictDocument { path } => {
                        let pool = Arc::clone(&pool);
                        let loc = sup_location.clone();
                        let tx = event_tx.clone();
                        tokio::spawn(async move {
                            match pool.load_conflict_document(loc, path).await {
                                Ok(doc) => {
                                    let _ = tx.send(EngineEvent::ConflictDocumentLoaded { doc });
                                }
                                Err(e) => {
                                    let _ = tx.send(EngineEvent::Error(e.to_string()));
                                }
                            }
                        });
                    }
                    TaskRequest::SaveConflictResolution {
                        path,
                        content,
                        stage,
                    } => {
                        spawn_guarded_mutation(
                            &pool,
                            &sup_location,
                            &event_tx,
                            &mutation_guard,
                            RefreshScope::StageStatus,
                            move |pool, loc| async move {
                                pool.save_conflict_resolution(loc, path, content, stage)
                                    .await
                            },
                        );
                    }
                    TaskRequest::RunGitCli {
                        id,
                        args,
                        stdin_payload,
                        is_read_only,
                        refresh_after,
                    } => {
                        let Some(cli_exec) = sup_cli.as_ref().map(Arc::clone) else {
                            let _ = event_tx
                                .send(EngineEvent::Error(EngineError::GitNotFound.to_string()));
                            continue;
                        };
                        let loc = sup_location.clone();
                        let tx = event_tx.clone();
                        let guard = Arc::clone(&mutation_guard);
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
                                    |ev| match ev {
                                        CliStreamEvent::Started(entry) => {
                                            let _ = tx.send(EngineEvent::ConsoleStarted(entry));
                                        }
                                        CliStreamEvent::Output {
                                            id,
                                            is_stderr,
                                            line,
                                        } => {
                                            let _ = tx.send(EngineEvent::ConsoleOutput {
                                                id,
                                                is_stderr,
                                                line,
                                            });
                                        }
                                        CliStreamEvent::Finished {
                                            id,
                                            exit_code,
                                            duration_ms,
                                        } => {
                                            let _ = tx.send(EngineEvent::ConsoleFinished {
                                                id,
                                                exit_code,
                                                duration_ms,
                                            });
                                        }
                                    },
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
                }
            }
        });

        Ok((
            EngineHandle {
                location,
                epochs,
                git_info,
                task_tx,
            },
            event_rx,
        ))
    }
}

pub(crate) fn forward_cli_event(tx: &UnboundedSender<EngineEvent>, ev: CliStreamEvent) {
    let _ = tx.send(match ev {
        CliStreamEvent::Started(entry) => EngineEvent::ConsoleStarted(entry),
        CliStreamEvent::Output {
            id,
            is_stderr,
            line,
        } => EngineEvent::ConsoleOutput {
            id,
            is_stderr,
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
    pool: &Arc<Git2WorkerPool>,
    location: &RepoLocation,
    event_tx: &UnboundedSender<EngineEvent>,
    mutation_guard: &Arc<MutationGuard>,
    scope: RefreshScope,
    op: F,
) where
    F: FnOnce(Arc<Git2WorkerPool>, RepoLocation) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = Result<(), EngineError>> + Send + 'static,
{
    let pool = Arc::clone(pool);
    let loc = location.clone();
    let tx = event_tx.clone();
    let guard = Arc::clone(mutation_guard);
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
