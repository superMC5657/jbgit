use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::thread::{self, JoinHandle};

use crossbeam_channel::Sender;
use git2::Repository;
use jbgit_core::EpochToken;

use crate::error::EngineError;

thread_local! {
    static THREAD_REPOS: RefCell<HashMap<PathBuf, Repository>> = RefCell::new(HashMap::new());
}

/// Executes `f` with a thread-local [`Repository`] for `git_dir`, ensuring any
/// cached `git_index` handle is reloaded from `.git/index` on disk first.
///
/// `git2::Repository` caches its `git_index` handle inside the thread-local
/// `Repository` without automatically reloading on subsequent `repo.index()`,
/// `repo.statuses()`, or `repo.reset_default()` calls. Whenever another worker
/// thread or external `git` process modifies `.git/index`, any cached
/// `Repository` would otherwise read or overwrite `.git/index` using its stale
/// snapshot.
pub(crate) fn with_thread_repo<F, T>(git_dir: &Path, f: F) -> Result<T, EngineError>
where
    F: FnOnce(&Repository) -> Result<T, EngineError>,
{
    THREAD_REPOS.with(|cell| {
        let mut map = cell.borrow_mut();
        let was_cached = map.contains_key(git_dir);
        if !was_cached {
            let repo = Repository::open(git_dir)?;
            map.insert(git_dir.to_path_buf(), repo);
        }
        if was_cached {
            let reload_failed = {
                let repo = map
                    .get(git_dir)
                    .ok_or_else(|| EngineError::RepoNotFound(git_dir.to_path_buf()))?;
                !repo.is_bare()
                    && repo
                        .index()
                        .ok()
                        .is_some_and(|mut index| index.read(true).is_err())
            };
            if reload_failed {
                map.remove(git_dir);
                let fresh = Repository::open(git_dir)?;
                return f(&fresh);
            }
        }
        let repo = map
            .get(git_dir)
            .ok_or_else(|| EngineError::RepoNotFound(git_dir.to_path_buf()))?;
        f(repo)
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

    pub(crate) async fn run_with<T, F>(&self, priority: TaskPriority, f: F) -> Result<T, EngineError>
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
        for handle in self.handles.drain(..) {
            let _ = handle.join();
        }
    }
}
