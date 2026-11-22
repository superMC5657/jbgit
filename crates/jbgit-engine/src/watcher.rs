use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use git2::Repository;
use jbgit_core::RefreshScope;
use notify::RecursiveMode;
use notify_debouncer_mini::{DebouncedEvent, DebouncedEventKind, Debouncer, new_debouncer};

use crate::error::EngineError;
use crate::probe::RepoLocation;

/// How long a cached "is this directory ignored?" answer stays valid. Short enough
/// that an edited `.gitignore` in a parent we never saw an event for heals quickly.
const IGNORE_DIR_CACHE_TTL: Duration = Duration::from_secs(2);
/// Upper bound on cached directories before the cache is simply reset.
const IGNORE_DIR_CACHE_MAX: usize = 4096;

/// Suppresses intermediate FS refresh notifications while `jbgit-engine` itself is
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
            // Owned by the debouncer's own thread: ignore checks never touch the
            // shared `Git2WorkerPool`, so build/IDE file storms cannot steal the
            // workers serving interactive diff requests.
            let mut filter =
                WorktreeEventFilter::new(location.workdir.clone(), location.git_dir.clone());
            let guard_wt = Arc::clone(&guard);
            let on_refresh_wt = on_refresh;

            let mut wt_debouncer = new_debouncer(
                Duration::from_millis(250),
                move |res: Result<Vec<DebouncedEvent>, notify::Error>| {
                    if let Ok(events) = res {
                        let changed =
                            filter.batch_is_relevant(events.iter().map(|ev| ev.path.as_path()));
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

/// Decides whether a batch of worktree FS events warrants a status refresh.
///
/// Runs entirely on the watcher's debouncer thread with its own lazily opened
/// `git2::Repository`. A batch is first reduced by the static directory
/// blacklist and de-duplicated; parent directories are checked against
/// `.gitignore` once (cached for [`IGNORE_DIR_CACHE_TTL`]) so thousands of
/// events under one ignored output directory cost a single libgit2 query.
pub struct WorktreeEventFilter {
    workdir: PathBuf,
    git_dir: PathBuf,
    repo: Option<Repository>,
    repo_open_failed: bool,
    dir_cache: HashMap<PathBuf, (bool, Instant)>,
    #[cfg(test)]
    ignore_queries: usize,
}

impl WorktreeEventFilter {
    pub fn new(workdir: PathBuf, git_dir: PathBuf) -> Self {
        Self {
            workdir,
            git_dir,
            repo: None,
            repo_open_failed: false,
            dir_cache: HashMap::new(),
            #[cfg(test)]
            ignore_queries: 0,
        }
    }

    /// Number of `.gitignore` lookups issued so far (tests).
    #[cfg(test)]
    pub fn ignore_queries(&self) -> usize {
        self.ignore_queries
    }

    /// `true` as soon as one event path is a non-ignored worktree file. Edits to
    /// a `.gitignore` are always relevant and reset the directory cache.
    pub fn batch_is_relevant<'a, I>(&mut self, paths: I) -> bool
    where
        I: IntoIterator<Item = &'a Path>,
    {
        let mut seen = HashSet::new();
        let mut candidates = Vec::new();
        for path in paths {
            if let Some(rel) = statically_relevant_rel_path(path, &self.workdir)
                && seen.insert(rel.clone())
            {
                candidates.push(rel);
            }
        }
        if candidates.is_empty() {
            return false;
        }
        if candidates
            .iter()
            .any(|rel| rel.file_name().is_some_and(|n| n == ".gitignore"))
        {
            self.dir_cache.clear();
            return true;
        }
        let now = Instant::now();
        candidates.iter().any(|rel| !self.is_ignored(rel, now))
    }

    fn is_ignored(&mut self, rel: &Path, now: Instant) -> bool {
        if let Some(parent) = rel.parent().filter(|p| !p.as_os_str().is_empty())
            && self.dir_is_ignored(parent, now)
        {
            // Git cannot re-include a file below an excluded directory.
            return true;
        }
        self.query_ignored(rel)
    }

    fn dir_is_ignored(&mut self, dir: &Path, now: Instant) -> bool {
        if let Some(&(ignored, at)) = self.dir_cache.get(dir)
            && now.saturating_duration_since(at) < IGNORE_DIR_CACHE_TTL
        {
            return ignored;
        }
        let ignored = self.query_ignored(dir);
        if self.dir_cache.len() >= IGNORE_DIR_CACHE_MAX {
            self.dir_cache.clear();
        }
        self.dir_cache.insert(dir.to_path_buf(), (ignored, now));
        ignored
    }

    fn query_ignored(&mut self, rel: &Path) -> bool {
        if self.repo.is_none() && !self.repo_open_failed {
            self.repo = Repository::open(&self.git_dir).ok();
            self.repo_open_failed = self.repo.is_none();
        }
        let Some(repo) = &self.repo else {
            return false;
        };
        #[cfg(test)]
        {
            self.ignore_queries += 1;
        }
        repo.is_path_ignored(rel).unwrap_or(false)
    }
}

/// Workdir-relative path of `path` unless it is outside the worktree, the
/// worktree root itself, or under a hard-coded noise directory.
fn statically_relevant_rel_path(path: &Path, workdir: &Path) -> Option<PathBuf> {
    let rel = path.strip_prefix(workdir).ok()?;
    if rel.as_os_str().is_empty() {
        return None;
    }
    for comp in rel.components() {
        if let std::path::Component::Normal(name) = comp
            && matches!(
                name.to_string_lossy().as_ref(),
                ".git" | "target" | "node_modules" | ".idea"
            )
        {
            return None;
        }
    }
    Some(rel.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn init_repo(dir: &Path) -> PathBuf {
        let repo = Repository::init(dir).unwrap();
        repo.path().to_path_buf()
    }

    #[test]
    fn worktree_filter_honours_gitignore_rules() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let git_dir = init_repo(&root);
        std::fs::write(root.join(".gitignore"), "build/\n*.log\n").unwrap();
        std::fs::create_dir_all(root.join("build/obj")).unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("build/obj/out.o"), "x").unwrap();
        std::fs::write(root.join("debug.log"), "x").unwrap();
        std::fs::write(root.join("src/main.rs"), "x").unwrap();

        let mut filter = WorktreeEventFilter::new(root.clone(), git_dir);
        let ignored_out = root.join("build").join("obj").join("out.o");
        let ignored_log = root.join("debug.log");
        let source = root.join("src").join("main.rs");
        let noise = root.join("target").join("debug").join("jbgit.exe");
        let git_meta = root.join(".git").join("index");

        assert!(!filter.batch_is_relevant([ignored_out.as_path()]));
        assert!(!filter.batch_is_relevant([ignored_log.as_path()]));
        assert!(!filter.batch_is_relevant([noise.as_path(), git_meta.as_path()]));
        assert!(filter.batch_is_relevant([ignored_out.as_path(), source.as_path()]));
        assert!(filter.batch_is_relevant([root.join(".gitignore").as_path()]));
        assert!(!filter.batch_is_relevant([root.as_path()]));
    }

    #[test]
    fn worktree_filter_collapses_event_storm_under_ignored_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let git_dir = init_repo(&root);
        std::fs::write(root.join(".gitignore"), "dist/\n").unwrap();
        std::fs::create_dir_all(root.join("dist")).unwrap();

        let paths: Vec<PathBuf> = (0..2_000)
            .map(|i| root.join("dist").join(format!("chunk-{i}.js")))
            .collect();
        let mut filter = WorktreeEventFilter::new(root.clone(), git_dir);
        assert!(!filter.batch_is_relevant(paths.iter().map(PathBuf::as_path)));
        // One lookup for `dist/`; every child short-circuits on the cached parent.
        assert_eq!(filter.ignore_queries(), 1);

        // A second storm within the TTL needs no new lookups at all.
        assert!(!filter.batch_is_relevant(paths.iter().map(PathBuf::as_path)));
        assert_eq!(filter.ignore_queries(), 1);
    }

    #[test]
    fn worktree_filter_without_repo_treats_paths_as_relevant() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let mut filter = WorktreeEventFilter::new(root.clone(), root.join("missing.git"));
        assert!(filter.batch_is_relevant([root.join("a.txt").as_path()]));
    }

    #[test]
    fn mutation_guard_defers_and_merges_scopes() {
        let guard = Arc::new(MutationGuard::new());
        let token = guard.begin();
        assert!(guard.is_mutating());
        assert!(guard.record_or_defer(RefreshScope::StageStatus).is_none());
        assert!(guard.record_or_defer(RefreshScope::RefsAndGraph).is_none());
        drop(token);
        assert!(!guard.is_mutating());
        assert_eq!(guard.take_pending(), RefreshScope::All);
    }
}
