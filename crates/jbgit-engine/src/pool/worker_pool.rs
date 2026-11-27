use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::thread::{self, JoinHandle};

use crossbeam_channel::Sender;
use git2::Repository;
use jbgit_core::EpochToken;

use crate::error::EngineError;

/// Maximum number of repositories whose `git2::Repository` handle each worker
/// thread keeps cached; the least recently used handle (and its pack indices)
/// is dropped beyond this, so long sessions that switch between many repos stay
/// bounded.
pub(crate) const THREAD_REPO_CACHE_CAPACITY: usize = 4;

struct CachedThreadRepo {
    repo: Repository,
    common_dir: PathBuf,
    shallow_fingerprint: Option<Vec<u8>>,
    /// Set when the last job on this handle returned an error: that job may have
    /// left unwritten in-memory index edits behind, so the next job forces a full
    /// `.git/index` re-read instead of the cheap stat/checksum freshness check.
    force_index_reload: bool,
}

thread_local! {
    /// Per-thread LRU of cached repositories (most recently used last).
    static THREAD_REPOS: RefCell<Vec<(PathBuf, CachedThreadRepo)>> =
        const { RefCell::new(Vec::new()) };
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

fn open_cached_repo(git_dir: &Path) -> Result<CachedThreadRepo, EngineError> {
    let repo = Repository::open(git_dir)?;
    let common_dir = repo.commondir().to_path_buf();
    // `git2::Repository` loads `shallow_grafts` lazily on first revwalk/lookup,
    // so capturing the fingerprint right after `open()` precedes graft loading.
    let shallow_fingerprint = read_shallow_fingerprint(git_dir, &common_dir);
    Ok(CachedThreadRepo {
        repo,
        common_dir,
        shallow_fingerprint,
        force_index_reload: false,
    })
}

/// Brings a cached handle's already-loaded `git_index` up to date with
/// `.git/index` on disk. Returns `false` when the reload failed.
///
/// `index.read(false)` only re-parses the file when its stat stamp (mtime/size)
/// or trailing checksum changed, so pure ODB / revwalk / tree / commit-diff jobs
/// pay a `stat` plus a 20-byte read instead of re-reading and re-hashing the
/// whole (often multi-MB) index on every call. A handle whose index was never
/// loaded loads it lazily (once) on first `repo.index()`.
fn refresh_cached_index(cached: &CachedThreadRepo) -> bool {
    if cached.repo.is_bare() {
        return true;
    }
    match cached.repo.index() {
        Ok(mut index) => index.read(cached.force_index_reload).is_ok(),
        Err(_) => true,
    }
}

/// Executes `f` with a thread-local [`Repository`] for `git_dir`, ensuring any
/// cached `git_index` handle reflects `.git/index` on disk first and evicting
/// the cached handle whenever `<common_dir|git_dir>/shallow` changes.
///
/// `git2::Repository` caches both its `git_index` handle and internal
/// `shallow_grafts` inside the thread-local `Repository` without automatically
/// reloading on subsequent calls. Whenever another worker thread or external
/// `git` process modifies `.git/index` or `.git/shallow` (e.g. `git fetch
/// --unshallow` / `--depth`), a stale cached `Repository` would otherwise read
/// outdated index state or keep truncating revwalks/parent lists at old shallow
/// boundaries. The index refresh is a stat/checksum check
/// ([`refresh_cached_index`]), forced to a full re-read only after a failed job.
///
/// At most [`THREAD_REPO_CACHE_CAPACITY`] handles are kept per thread (LRU).
/// The thread-local cache is not borrowed while `f` runs.
pub(crate) fn with_thread_repo<F, T>(git_dir: &Path, f: F) -> Result<T, EngineError>
where
    F: FnOnce(&Repository) -> Result<T, EngineError>,
{
    let cached = THREAD_REPOS.with(|cell| {
        let mut cache = cell.borrow_mut();
        cache
            .iter()
            .position(|(dir, _)| dir == git_dir)
            .map(|pos| cache.remove(pos).1)
    });

    let mut entry = match cached {
        Some(cached) => {
            let current_shallow = read_shallow_fingerprint(git_dir, &cached.common_dir);
            if current_shallow != cached.shallow_fingerprint {
                drop(cached);
                open_cached_repo(git_dir)?
            } else if refresh_cached_index(&cached) {
                cached
            } else {
                drop(cached);
                let fresh = Repository::open(git_dir)?;
                return f(&fresh);
            }
        }
        None => open_cached_repo(git_dir)?,
    };

    let result = f(&entry.repo);
    entry.force_index_reload = result.is_err();

    THREAD_REPOS.with(|cell| {
        let mut cache = cell.borrow_mut();
        // A nested call for the same `git_dir` may have re-inserted a handle.
        cache.retain(|(dir, _)| dir != git_dir);
        cache.push((git_dir.to_path_buf(), entry));
        if cache.len() > THREAD_REPO_CACHE_CAPACITY {
            let excess = cache.len() - THREAD_REPO_CACHE_CAPACITY;
            cache.drain(..excess);
        }
    });
    result
}

/// Git directories cached on the current thread, least recently used first.
#[cfg(test)]
pub(crate) fn thread_repo_cache_dirs() -> Vec<PathBuf> {
    THREAD_REPOS.with(|cell| cell.borrow().iter().map(|(dir, _)| dir.clone()).collect())
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

    fn init_repo_with_commit(root: &Path) {
        let repo = Repository::init(root).unwrap();
        std::fs::write(root.join("a.txt"), "hello\n").unwrap();
        let mut idx = repo.index().unwrap();
        idx.add_path(Path::new("a.txt")).unwrap();
        idx.write().unwrap();
        let tree = repo.find_tree(idx.write_tree().unwrap()).unwrap();
        let sig = git2::Signature::now("Test", "test@example.com").unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
            .unwrap();
    }

    /// Adds an unwritten (in-memory only) entry to the cached handle's index.
    fn add_in_memory_marker(repo: &Repository, path: &str) {
        let mut index = repo.index().unwrap();
        let entry = git2::IndexEntry {
            ctime: git2::IndexTime::new(0, 0),
            mtime: git2::IndexTime::new(0, 0),
            dev: 0,
            ino: 0,
            mode: 0o100_644,
            uid: 0,
            gid: 0,
            file_size: 0,
            id: git2::Oid::ZERO_SHA1,
            flags: 0,
            flags_extended: 0,
            path: path.as_bytes().to_vec(),
        };
        index.add_frombuffer(&entry, b"marker\n").unwrap();
    }

    fn cached_index_has(git_dir: &Path, path: &str) -> bool {
        with_thread_repo(git_dir, |repo| {
            Ok(repo.index()?.get_path(Path::new(path), 0).is_some())
        })
        .unwrap()
    }

    #[test]
    fn cached_repo_skips_forced_index_reparse_but_sees_external_index_writes() {
        let tmp = tempfile::tempdir().unwrap();
        init_repo_with_commit(tmp.path());
        let git_dir = tmp.path().join(".git");

        // A pure ODB query (HEAD lookup) leaves an in-memory index marker behind.
        // A forced `index.read(true)` on the next call would discard it; the
        // stat/checksum refresh must keep the already-parsed index untouched.
        with_thread_repo(&git_dir, |repo| {
            repo.head()?.peel_to_commit()?;
            add_in_memory_marker(repo, "in-memory-only.txt");
            Ok(())
        })
        .unwrap();
        assert!(
            cached_index_has(&git_dir, "in-memory-only.txt"),
            "unchanged .git/index must not be force re-read on every call"
        );

        // An external `git add` (separate handle writing `.git/index`) must be
        // picked up by the cached handle through the stat/checksum check.
        std::fs::write(tmp.path().join("b.txt"), "external\n").unwrap();
        {
            let external = Repository::open(&git_dir).unwrap();
            let mut idx = external.index().unwrap();
            idx.add_path(Path::new("b.txt")).unwrap();
            idx.write().unwrap();
        }
        assert!(
            cached_index_has(&git_dir, "b.txt"),
            "external index write must be visible to the cached handle"
        );
        assert!(!cached_index_has(&git_dir, "in-memory-only.txt"));
    }

    #[test]
    fn failed_job_forces_full_index_reload_on_next_call() {
        let tmp = tempfile::tempdir().unwrap();
        init_repo_with_commit(tmp.path());
        let git_dir = tmp.path().join(".git");

        let res: Result<(), EngineError> = with_thread_repo(&git_dir, |repo| {
            add_in_memory_marker(repo, "half-applied.txt");
            Err(EngineError::Other(
                "mutation failed before index.write()".into(),
            ))
        });
        assert!(res.is_err());
        assert!(
            !cached_index_has(&git_dir, "half-applied.txt"),
            "unwritten edits of a failed job must not leak into later jobs"
        );
    }

    #[test]
    fn thread_repo_cache_evicts_least_recently_used_beyond_capacity() {
        let tmps: Vec<_> = (0..THREAD_REPO_CACHE_CAPACITY + 2)
            .map(|_| {
                let tmp = tempfile::tempdir().unwrap();
                init_repo_with_commit(tmp.path());
                tmp
            })
            .collect();
        let dirs: Vec<PathBuf> = tmps.iter().map(|t| t.path().join(".git")).collect();

        for dir in &dirs {
            with_thread_repo(dir, |_| Ok(())).unwrap();
        }
        let cached = thread_repo_cache_dirs();
        assert_eq!(cached.len(), THREAD_REPO_CACHE_CAPACITY);
        assert_eq!(cached, dirs[2..].to_vec(), "oldest handles evicted first");

        // Touching a cached repo makes it most recently used, so the next new
        // repo evicts the now-oldest entry instead.
        with_thread_repo(&dirs[2], |_| Ok(())).unwrap();
        with_thread_repo(&dirs[0], |_| Ok(())).unwrap();
        let cached = thread_repo_cache_dirs();
        assert_eq!(cached.len(), THREAD_REPO_CACHE_CAPACITY);
        assert!(!cached.contains(&dirs[3]));
        assert_eq!(cached.last(), Some(&dirs[0]));
        assert!(cached.contains(&dirs[2]));
    }
}
