use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::thread::{self, JoinHandle};

use crossbeam_channel::Sender;
use git2::Repository;
use jbgit_core::EpochToken;

use crate::error::EngineError;

struct CachedThreadRepo {
    repo: Repository,
    common_dir: PathBuf,
    shallow_fingerprint: Option<Vec<u8>>,
}

thread_local! {
    static THREAD_REPOS: RefCell<HashMap<PathBuf, CachedThreadRepo>> = RefCell::new(HashMap::new());
}

/// Reads the current `<common_dir|git_dir>/shallow` file contents when non-empty.
///
/// Used both to invalidate cached thread-local `git2::Repository` handles (whose
/// internal `shallow_grafts` survive external `git fetch --unshallow` / `--depth`
/// runs) and to guard [`super::snapshot::SnapshotCommitCache`] reuse.
pub(crate) fn read_shallow_fingerprint(git_dir: &Path, common_dir: &Path) -> Option<Vec<u8>> {
    if let Ok(bytes) = std::fs::read(common_dir.join("shallow"))
        && !bytes.is_empty()
    {
        return Some(bytes);
    }
    if common_dir != git_dir
        && let Ok(bytes) = std::fs::read(git_dir.join("shallow"))
        && !bytes.is_empty()
    {
        return Some(bytes);
    }
    None
}

fn resolve_common_dir_hint(git_dir: &Path) -> PathBuf {
    if let Ok(raw) = std::fs::read_to_string(git_dir.join("commondir")) {
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            let p = Path::new(trimmed);
            return if p.is_absolute() {
                p.to_path_buf()
            } else {
                git_dir.join(p)
            };
        }
    }
    git_dir.to_path_buf()
}

fn open_cached_repo(git_dir: &Path) -> Result<CachedThreadRepo, EngineError> {
    let common_hint = resolve_common_dir_hint(git_dir);
    let pre_fingerprint = read_shallow_fingerprint(git_dir, &common_hint);
    let repo = Repository::open(git_dir)?;
    let common_dir = repo.commondir().to_path_buf();
    let shallow_fingerprint = if common_dir == common_hint {
        pre_fingerprint
    } else {
        read_shallow_fingerprint(git_dir, &common_dir)
    };
    Ok(CachedThreadRepo {
        repo,
        common_dir,
        shallow_fingerprint,
    })
}

/// Executes `f` with a thread-local [`Repository`] for `git_dir`, ensuring any
/// cached `git_index` handle is reloaded from `.git/index` on disk first and
/// evicting the cached handle whenever `<common_dir|git_dir>/shallow` changes.
///
/// `git2::Repository` caches both its `git_index` handle and internal
/// `shallow_grafts` inside the thread-local `Repository` without automatically
/// reloading on subsequent calls. Whenever another worker thread or external
/// `git` process modifies `.git/index` or `.git/shallow` (e.g. `git fetch
/// --unshallow` / `--depth`), a stale cached `Repository` would otherwise read
/// outdated index state or keep truncating revwalks/parent lists at old shallow
/// boundaries.
pub(crate) fn with_thread_repo<F, T>(git_dir: &Path, f: F) -> Result<T, EngineError>
where
    F: FnOnce(&Repository) -> Result<T, EngineError>,
{
    THREAD_REPOS.with(|cell| {
        let mut map = cell.borrow_mut();
        if let Some(cached) = map.get(git_dir) {
            let current_shallow = read_shallow_fingerprint(git_dir, &cached.common_dir);
            if current_shallow != cached.shallow_fingerprint {
                let common_dir = cached.common_dir.clone();
                map.remove(git_dir);
                let repo = Repository::open(git_dir)?;
                map.insert(
                    git_dir.to_path_buf(),
                    CachedThreadRepo {
                        repo,
                        common_dir,
                        shallow_fingerprint: current_shallow,
                    },
                );
            } else {
                let reload_failed = !cached.repo.is_bare()
                    && cached
                        .repo
                        .index()
                        .ok()
                        .is_some_and(|mut index| index.read(true).is_err());
                if reload_failed {
                    map.remove(git_dir);
                    let fresh = Repository::open(git_dir)?;
                    return f(&fresh);
                }
            }
        } else {
            let entry = open_cached_repo(git_dir)?;
            map.insert(git_dir.to_path_buf(), entry);
        }
        let entry = map
            .get(git_dir)
            .ok_or_else(|| EngineError::RepoNotFound(git_dir.to_path_buf()))?;
        f(&entry.repo)
    })
}

type JobFn = Box<dyn FnOnce() + Send + 'static>;

/// Scheduling class of a [`Git2WorkerPool`] job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TaskPriority {
    Interactive,
    Background,
}

/// Fixed-size worker pool caching `git2::Repository` handles in `thread_local!`
/// so concurrent read queries avoid re-opening `.git/config` and packfile indices.
pub(crate) struct Git2WorkerPool {
    high_tx: Option<Sender<JobFn>>,
    bg_tx: Option<Sender<JobFn>>,
    handles: Vec<JoinHandle<()>>,
}

impl Git2WorkerPool {
    pub(crate) fn new(num_threads: usize) -> Self {
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

    pub(crate) fn spawn_with_priority<F>(&self, priority: TaskPriority, f: F)
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

    pub(crate) async fn run_with<T, F>(
        &self,
        priority: TaskPriority,
        f: F,
    ) -> Result<T, EngineError>
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
        let handles = std::mem::take(&mut self.handles);
        if handles.is_empty() {
            return;
        }
        if tokio::runtime::Handle::try_current().is_ok() {
            // Avoid blocking a Tokio async worker thread on OS thread joins during
            // repo switches or shutdown.
            let _ = thread::Builder::new()
                .name("jbgit-git2-pool-teardown".into())
                .spawn(move || {
                    for handle in handles {
                        let _ = handle.join();
                    }
                });
        } else {
            for handle in handles {
                let _ = handle.join();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Barrier};
    use std::time::{Duration, Instant};

    use super::*;

    #[tokio::test]
    async fn dropping_pool_inside_tokio_runtime_does_not_block_async_thread() {
        let pool = Git2WorkerPool::new(1);
        let started = Arc::new(Barrier::new(2));
        let started_worker = Arc::clone(&started);
        pool.spawn_with_priority(TaskPriority::Interactive, move || {
            started_worker.wait();
            thread::sleep(Duration::from_millis(250));
        });
        started.wait();

        let t0 = Instant::now();
        drop(pool);
        assert!(
            t0.elapsed() < Duration::from_millis(120),
            "Git2WorkerPool::drop blocked Tokio thread for {:?}",
            t0.elapsed()
        );
    }
}
