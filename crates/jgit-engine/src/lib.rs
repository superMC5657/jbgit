pub mod cli;
pub mod error;
pub mod pool;
pub mod probe;
pub mod supervisor;
pub mod watcher;

pub use cli::{GitCliExecutor, GitExecutableInfo, prepare_pathspec_args};
pub use error::EngineError;
pub use pool::Git2WorkerPool;
pub use probe::{RepoLocation, RepoProbeResult, probe_repo_state};
pub use supervisor::{EngineEvent, EngineHandle, EngineSupervisor, TaskRequest};
pub use watcher::{MutationGuard, MutationToken, RepoWatcher};
