use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum EngineError {
    #[error("no git repository found at or above {0}")]
    RepoNotFound(PathBuf),

    #[error("git executable not found in PATH")]
    GitNotFound,

    #[error("git index lock conflict at {lock_path}: {detail}")]
    IndexLockConflict { lock_path: PathBuf, detail: String },

    #[error("git2 error: {0}")]
    Git2(#[from] git2::Error),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("request superseded and cancelled")]
    Cancelled,

    #[error("filesystem watcher error: {0}")]
    Watcher(String),

    #[error("{0}")]
    Other(String),
}
