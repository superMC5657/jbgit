pub mod askpass;
pub(crate) mod blob_reader;
pub(crate) mod cli;
pub mod clipboard;
pub(crate) mod details;
pub mod error;
pub(crate) mod ops;
pub mod pool;
pub mod probe;
pub mod stage;
pub mod supervisor;
pub(crate) mod watcher;

pub use cli::GitExecutableInfo;
pub use error::EngineError;
pub use pool::{
    AheadBehindUpdate, LARGE_INDEX_ENTRIES, StatusScanConfig, StatusScanMode, StatusScanPolicy,
};
pub use probe::{RepoLocation, RepoProbeResult};
pub use stage::{
    CommitOutcome, CommitPrecheckInfo, CommitRequest, StageDiffKey, StageDiffSide,
    SubmoduleDiffInfo, ThreeDocStageDiff,
};
pub use supervisor::{
    BlameFailureKind, EngineBackend, EngineEvent, EngineHandle, EngineSupervisor,
    RecordingEngineBackend, TaskRequest,
};
