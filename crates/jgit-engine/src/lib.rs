pub mod cli;
pub mod details;
pub mod error;
pub mod pool;
pub mod probe;
pub mod stage;
pub mod supervisor;
pub mod watcher;

pub use cli::{GitCliExecutor, GitExecutableInfo, prepare_pathspec_args};
pub use details::{load_commit_details_sync, load_file_diff_sync, load_path_matches_sync};
pub use error::EngineError;
pub use pool::Git2WorkerPool;
pub use probe::{RepoLocation, RepoProbeResult, probe_repo_state};
pub use stage::{
    CommitOutcome, CommitPrecheckInfo, CommitRequest, StageDiffKey, StageDiffSide,
    ThreeDocStageDiff, apply_stage_buffer_sync, build_commit_cli_args, discard_targets_sync,
    execute_commit_git2_sync, load_amend_preview_sync, load_stage_file_diff_sync,
    probe_commit_precheck_sync, stage_all_tracked_sync, stage_targets_sync, undo_last_commit_sync,
    unstage_targets_sync, write_worktree_buffer_sync,
};
pub use supervisor::{EngineEvent, EngineHandle, EngineSupervisor, TaskRequest};
pub use watcher::{MutationGuard, MutationToken, RepoWatcher};
