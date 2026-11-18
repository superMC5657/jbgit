use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use jgit_core::{
    CommitChangesBundle, DetailsKey, EpochToken, EpochTracker, FileDiff, FileDiffKey,
    GitConsoleEntry, Oid, RefreshScope, RepoSnapshot, StageStatusSnapshot,
};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

use crate::cli::{CliCommand, CliStreamEvent, GitCliExecutor, GitExecutableInfo};
use crate::error::EngineError;
use crate::pool::Git2WorkerPool;
use crate::probe::RepoLocation;
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
    RunGitCli {
        id: u64,
        args: Vec<String>,
        stdin_payload: Option<Vec<u8>>,
        is_read_only: bool,
        refresh_after: RefreshScope,
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
