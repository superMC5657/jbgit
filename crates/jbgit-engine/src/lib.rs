pub mod askpass;
pub(crate) mod blob_reader;
pub(crate) mod cli;
pub(crate) mod clipboard;
pub(crate) mod details;
pub mod error;
pub mod lifecycle;
pub(crate) mod ops;
pub mod pool;
pub mod probe;
pub mod stage;
pub mod supervisor;
pub(crate) mod watcher;

pub use cli::{
    GitExecutableInfo, configured_git_executable, detect_git_executable,
    find_git_executable_with_override, set_configured_git_executable,
};
pub use details::{load_cherry_picked_sync, load_reflog_sync};
pub use error::EngineError;
pub use lifecycle::{CloneRequest, clone_repository_async, compile_clone_args, init_repository_sync};
pub use pool::{
    AheadBehindUpdate, DEFAULT_COMMIT_PAGE_SIZE, LARGE_INDEX_ENTRIES, MAX_INCREMENTAL_NEW_COMMITS,
    SnapshotCommitCache, SnapshotLoadKind, SnapshotLoadMode, StatusScanConfig, StatusScanMode,
    StatusScanPolicy, load_repo_snapshot_incremental_sync,
};
pub use probe::{RepoLocation, RepoProbeResult};
pub use stage::{
    CommitOutcome, CommitPrecheckInfo, CommitRequest, CrlfProblem, StageDiffKey, StageDiffSide,
    SubmoduleDiffInfo, ThreeDocStageDiff, autocrlf_prevents_crlf_warning, bytes_contain_crlf_text,
    detect_crlf_commit_problem_sync, gitattributes_handles_crlf, probe_commit_precheck_sync,
    set_core_autocrlf_true_sync,
};
pub use supervisor::{
    BlameFailureKind, EngineBackend, EngineEvent, EngineHandle, EngineSupervisor,
    RecordingEngineBackend, TaskRequest,
};
