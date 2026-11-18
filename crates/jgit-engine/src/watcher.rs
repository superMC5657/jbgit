use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use jgit_core::RefreshScope;
use notify::RecursiveMode;
use notify_debouncer_mini::{DebouncedEvent, DebouncedEventKind, Debouncer, new_debouncer};

use crate::error::EngineError;
use crate::pool::Git2WorkerPool;
use crate::probe::RepoLocation;

/// Suppresses intermediate FS refresh notifications while `jgit-engine` itself is
/// executing a mutating Git CLI command, coalescing them into one refresh on completion.
#[derive(Debug, Default)]
pub struct MutationGuard {
    active: AtomicUsize,
    pending: Mutex<RefreshScope>,
}

impl MutationGuard {
    pub fn new() -> Self {
        Self {
            active: AtomicUsize::new(0),
            pending: Mutex::new(RefreshScope::None),
        }
    }

    pub fn begin(self: &Arc<Self>) -> MutationToken {
        self.active.fetch_add(1, Ordering::AcqRel);
        MutationToken {
            guard: Arc::clone(self),
        }
    }

    pub fn is_mutating(&self) -> bool {
        self.active.load(Ordering::Acquire) > 0
    }

    /// Records a refresh scope. Returns `Some(scope)` if no mutation is active
    /// and the caller should dispatch immediately, or `None` if deferred.
    pub fn record_or_defer(&self, scope: RefreshScope) -> Option<RefreshScope> {
        if self.is_mutating() {
            if let Ok(mut pending) = self.pending.lock() {
                *pending = pending.merge(scope);
            }
            None
        } else {
            Some(scope)
        }
    }

    pub fn take_pending(&self) -> RefreshScope {
        if let Ok(mut pending) = self.pending.lock() {
            let taken = *pending;
            *pending = RefreshScope::None;
            taken
        } else {
            RefreshScope::None
        }
    }
}

pub struct MutationToken {
    guard: Arc<MutationGuard>,
}

impl Drop for MutationToken {
    fn drop(&mut self) {
        self.guard.active.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Dual-cadence debounced repository watcher:
/// - `.git/` metadata (`HEAD`, `refs/`, `packed-refs`, `index`, `MERGE_HEAD`, etc.): 100ms
/// - Worktree files (filtering `.git/`, `target/`, `node_modules/`, `.gitignore`): 250ms
pub struct RepoWatcher {
    _git_debouncer: Debouncer<notify::RecommendedWatcher>,
    _worktree_debouncer: Option<Debouncer<notify::RecommendedWatcher>>,
}

impl RepoWatcher {
    pub fn start<F>(
        location: RepoLocation,
        pool: Arc<Git2WorkerPool>,
        guard: Arc<MutationGuard>,
        on_refresh: F,
    ) -> Result<Self, EngineError>
    where
        F: Fn(RefreshScope) + Send + Sync + Clone + 'static,
    {
        let git_dir = location.git_dir.clone();
        let common_dir = location.common_dir.clone();
        let guard_git = Arc::clone(&guard);
        let on_refresh_git = on_refresh.clone();

        let mut git_debouncer = new_debouncer(
            Duration::from_millis(100),
            move |res: Result<Vec<DebouncedEvent>, notify::Error>| {
                if let Ok(events) = res {
                    let mut combined = RefreshScope::None;
                    for ev in events {
                        if matches!(
                            ev.kind,
                            DebouncedEventKind::Any | DebouncedEventKind::AnyContinuous
                        ) {
                            combined = combined.merge(classify_git_meta_path(&ev.path));
                        }
                    }
                    if combined != RefreshScope::None
                        && let Some(scope) = guard_git.record_or_defer(combined)
                    {
                        on_refresh_git(scope);
                    }
                }
            },
        )
        .map_err(|e| EngineError::Watcher(e.to_string()))?;

        git_debouncer
            .watcher()
            .watch(&git_dir, RecursiveMode::Recursive)
            .map_err(|e| EngineError::Watcher(e.to_string()))?;

        if common_dir != git_dir {
            let _ = git_debouncer
                .watcher()
                .watch(&common_dir, RecursiveMode::Recursive);
        }

        let worktree_debouncer = if !location.is_bare {
            let workdir = location.workdir.clone();
            let git_dir_for_wt = location.git_dir.clone();
            let guard_wt = Arc::clone(&guard);
            let on_refresh_wt = on_refresh;

            let mut wt_debouncer = new_debouncer(
                Duration::from_millis(250),
                move |res: Result<Vec<DebouncedEvent>, notify::Error>| {
                    if let Ok(events) = res {
                        let mut changed = false;
                        for ev in events {
                            if is_relevant_worktree_path(&ev.path, &workdir, &git_dir_for_wt, &pool)
                            {
                                changed = true;
                                break;
                            }
                        }
                        if changed
                            && let Some(scope) =
                                guard_wt.record_or_defer(RefreshScope::WorktreeStatus)
                        {
                            on_refresh_wt(scope);
                        }
                    }
                },
            )
            .map_err(|e| EngineError::Watcher(e.to_string()))?;

            wt_debouncer
                .watcher()
                .watch(&location.workdir, RecursiveMode::Recursive)
                .map_err(|e| EngineError::Watcher(e.to_string()))?;

            Some(wt_debouncer)
        } else {
            None
        };

        Ok(Self {
            _git_debouncer: git_debouncer,
            _worktree_debouncer: worktree_debouncer,
        })
    }
}

pub fn classify_git_meta_path(path: &Path) -> RefreshScope {
    let s = path.to_string_lossy();
    let normalized = s.replace('\\', "/");

    // Ignore lock files, object database writes, and commit edit messages
    if normalized.ends_with(".lock")
        || normalized.contains("/objects/")
        || normalized.contains("/logs/")
        || normalized.ends_with("/COMMIT_EDITMSG")
    {
        return RefreshScope::None;
    }

    if normalized.ends_with("/index") {
        return RefreshScope::StageStatus;
    }

    if normalized.ends_with("/HEAD")
        || normalized.contains("/refs/")
        || normalized.ends_with("/packed-refs")
        || normalized.ends_with("/MERGE_HEAD")
        || normalized.ends_with("/REBASE_HEAD")
        || normalized.ends_with("/CHERRY_PICK_HEAD")
        || normalized.ends_with("/REVERT_HEAD")
        || normalized.ends_with("/BISECT_LOG")
        || normalized.contains("/rebase-merge/")
        || normalized.contains("/rebase-apply/")
    {
        return RefreshScope::RefsAndGraph;
    }

    RefreshScope::None
}

fn is_relevant_worktree_path(
    path: &Path,
    workdir: &Path,
    git_dir: &Path,
    pool: &Git2WorkerPool,
) -> bool {
    let rel: PathBuf = match path.strip_prefix(workdir) {
        Ok(r) => r.to_path_buf(),
        Err(_) => return false,
    };

    if rel.as_os_str().is_empty() {
        return false;
    }

    for comp in rel.components() {
        if let std::path::Component::Normal(name) = comp {
            let name_str = name.to_string_lossy();
            if matches!(
                name_str.as_ref(),
                ".git" | "target" | "node_modules" | ".idea"
            ) {
                return false;
            }
        }
    }

    !pool.is_path_ignored_sync(git_dir.to_path_buf(), rel)
}
