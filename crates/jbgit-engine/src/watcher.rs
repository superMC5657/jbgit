use std::borrow::Cow;
use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
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
/// Upper bound on cached directories. When reached, expired entries are evicted
/// first and the cache is only reset if every entry is still fresh.
const IGNORE_DIR_CACHE_MAX: usize = 4096;

/// Suppresses intermediate FS refresh notifications while `jbgit-engine` itself is
/// executing a mutating Git operation, coalescing them into one refresh on completion,
/// and serializes concurrent mutations so they cannot race on `.git/index`.
#[derive(Debug, Default)]
pub struct MutationGuard {
    active: AtomicUsize,
    pending: Mutex<RefreshScope>,
    serial: Arc<tokio::sync::Mutex<()>>,
}

impl MutationGuard {
    pub fn new() -> Self {
        Self {
            active: AtomicUsize::new(0),
            pending: Mutex::new(RefreshScope::None),
            serial: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    fn begin(self: &Arc<Self>) -> MutationToken {
        self.active.fetch_add(1, Ordering::AcqRel);
        MutationToken {
            guard: Arc::clone(self),
        }
    }

    /// Marks a mutation active immediately (so the watcher defers events and
    /// `EngineHandle::shutdown` waits for queued work) and acquires the exclusive
    /// mutation lock.
    pub async fn acquire(self: &Arc<Self>) -> MutationPermit {
        let token = self.begin();
        let lock = Arc::clone(&self.serial).lock_owned().await;
        MutationPermit {
            _lock: lock,
            _token: token,
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

/// RAII permit representing an active, serialized repository mutation.
///
/// Field drop order is significant: `_lock` is released before `_token`
/// decrements `active`, so queued mutations stay covered by `is_mutating()`
/// and the last mutation only clears `is_mutating()` after releasing the lock.
pub struct MutationPermit {
    _lock: tokio::sync::OwnedMutexGuard<()>,
    _token: MutationToken,
}

impl MutationPermit {
    /// Releases the serialization lock and active mutation token in order, then
    /// drains any watcher refresh scope deferred while the mutation ran.
    pub fn finish(self) -> RefreshScope {
        let guard = Arc::clone(&self._token.guard);
        drop(self);
        guard.take_pending()
    }
}

struct MutationToken {
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
        let git_dir_cb = git_dir.clone();
        let common_dir_cb = common_dir.clone();
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
                            combined = combined.merge(classify_git_meta_path(
                                &ev.path,
                                &git_dir_cb,
                                &common_dir_cb,
                            ));
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

        // Kept recursive on purpose: Windows `ReadDirectoryChangesW` watches a whole
        // tree (it cannot exclude `objects/`), entries such as `MERGE_HEAD` or
        // `rebase-merge/` cannot be watched before they exist, and in a non-bare
        // repo the recursive worktree watch reports `.git/**` anyway. Irrelevant
        // subtrees are instead dropped allocation-free in `classify_git_meta_path`.
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

fn normalize_watcher_path(path: &Path) -> String {
    let raw = path.to_string_lossy();
    #[cfg(windows)]
    let stripped = crate::probe::strip_verbatim_prefix_str(&raw);
    #[cfg(not(windows))]
    let stripped: Option<String> = None;
    stripped.as_deref().unwrap_or(&raw).replace('\\', "/")
}

/// Slow-path `path` relative to `base` (`/`-separated) for spellings the plain
/// component-wise [`Path::strip_prefix`] rejects: Windows `\\?\` verbatim
/// prefixes and drive-letter / directory case differences. Only reached when
/// the allocation-free `strip_prefix` fails.
fn strip_prefix_normalized_fallback(path: &Path, base: &Path) -> Option<String> {
    let norm_path = normalize_watcher_path(path);
    let norm_base = normalize_watcher_path(base);
    let base_trimmed = norm_base.trim_end_matches('/');
    if base_trimmed.is_empty() {
        return None;
    }
    #[cfg(windows)]
    let matched_rest = {
        let matches_base = norm_path
            .as_bytes()
            .get(..base_trimmed.len())
            .is_some_and(|head| head.eq_ignore_ascii_case(base_trimmed.as_bytes()));
        if matches_base {
            let rest = &norm_path[base_trimmed.len()..];
            if rest.is_empty() || rest.starts_with('/') {
                Some(rest.trim_start_matches('/'))
            } else {
                None
            }
        } else {
            None
        }
    };
    #[cfg(not(windows))]
    let matched_rest = norm_path
        .strip_prefix(base_trimmed)
        .filter(|rest| rest.is_empty() || rest.starts_with('/'))
        .map(|rest| rest.trim_start_matches('/'));
    matched_rest.map(ToString::to_string)
}

/// `path` relative to `base`: borrowed when the spelling matches (the common
/// case, since notify reports paths under the watched root as given), owned
/// via [`strip_prefix_normalized_fallback`] otherwise.
fn rel_path_under<'a>(path: &'a Path, base: &Path) -> Option<Cow<'a, Path>> {
    if let Ok(rel) = path.strip_prefix(base) {
        return Some(Cow::Borrowed(rel));
    }
    strip_prefix_normalized_fallback(path, base).map(|rel| Cow::Owned(PathBuf::from(rel)))
}

/// Classifies one `.git` event path. Allocation-free whenever the event path
/// shares the watched directory's spelling, so an `objects/` / `logs/` write
/// storm (fetch, gc, commit) is dropped by a first-component check before any
/// string work.
pub fn classify_git_meta_path(path: &Path, git_dir: &Path, common_dir: &Path) -> RefreshScope {
    // Strip `git_dir` first because in linked worktrees `git_dir` lives inside `common_dir`
    // (`<common_dir>/worktrees/<name>`). Matching relative segments avoids false hits on
    // ancestor directories named `logs`, `objects`, or `refs`.
    if let Ok(rel) = path.strip_prefix(git_dir) {
        return classify_git_rel_path(rel);
    }
    if git_dir != common_dir {
        let mut matched_common_worktrees = false;
        if let Ok(rel) = path.strip_prefix(common_dir) {
            let first_normal = rel.components().find_map(|c| match c {
                Component::Normal(name) => Some(name),
                _ => None,
            });
            if first_normal.is_some_and(|name| name == "worktrees") {
                // `path` shares `common_dir` casing up to `worktrees/`, but may still
                // match `git_dir` under a different spelling/casing on `<name>`.
                matched_common_worktrees = true;
            } else {
                return classify_git_rel_path(rel);
            }
        }
        if let Some(rel) = strip_prefix_normalized_fallback(path, git_dir) {
            return classify_git_rel_path(Path::new(&rel));
        }
        if matched_common_worktrees {
            return RefreshScope::None;
        }
        if let Some(rel) = strip_prefix_normalized_fallback(path, common_dir) {
            return classify_git_rel_path(Path::new(&rel));
        }
        return RefreshScope::None;
    }
    let Some(rel) = strip_prefix_normalized_fallback(path, git_dir) else {
        return RefreshScope::None;
    };
    classify_git_rel_path(Path::new(&rel))
}

/// Refresh scope for a path relative to a git directory. Only an allow-list of
/// metadata entries refreshes; everything else (`objects/`, `logs/`, `hooks/`,
/// `info/`, `lfs/`, `modules/`, other `worktrees/`, `config`, `FETCH_HEAD`,
/// `ORIG_HEAD`, `*.lock`, `COMMIT_EDITMSG`, ...) is ignored.
fn classify_git_rel_path(rel: &Path) -> RefreshScope {
    let mut names = rel.components().filter_map(|c| match c {
        Component::Normal(name) => Some(name),
        _ => None,
    });
    let Some(first) = names.next() else {
        return RefreshScope::None;
    };
    // Object database writes and reflogs: the bulk of `.git` event volume.
    if first == "objects" || first == "logs" {
        return RefreshScope::None;
    }
    let leaf = names.next_back();
    let is_nested = leaf.is_some();
    if leaf.unwrap_or(first).as_encoded_bytes().ends_with(b".lock") {
        return RefreshScope::None;
    }
    let Some(first) = first.to_str() else {
        return RefreshScope::None;
    };

    if is_nested {
        return if matches!(first, "refs" | "rebase-merge" | "rebase-apply") {
            RefreshScope::RefsAndGraph
        } else {
            RefreshScope::None
        };
    }
    match first {
        "index" => RefreshScope::StageStatus,
        "HEAD" | "refs" | "packed-refs" | "shallow" | "MERGE_HEAD" | "REBASE_HEAD"
        | "CHERRY_PICK_HEAD" | "REVERT_HEAD" | "BISECT_LOG" | "rebase-merge" | "rebase-apply" => {
            RefreshScope::RefsAndGraph
        }
        _ => RefreshScope::None,
    }
}

/// Decides whether a batch of worktree FS events warrants a status refresh.
///
/// Runs entirely on the watcher's debouncer thread with its own lazily opened
/// `git2::Repository`. Events are first reduced by the static directory
/// blacklist (allocation-free for paths spelled like the watched root). Each
/// remaining path's ancestor directories are then checked against a bounded
/// cache of `.gitignore` verdicts (valid for [`IGNORE_DIR_CACHE_TTL`]): one
/// cached ignored ancestor rejects the path by prefix, so a storm anywhere
/// below one ignored output directory (`dist/a/b/c.js`, `out/x/y/z.o`) costs a
/// single libgit2 query instead of one per distinct parent directory.
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
    ///
    /// The debouncer already de-duplicates event paths, so the batch is walked
    /// twice without collecting it: a cheap `.gitignore` scan first (so a
    /// stale cache is never consulted for a batch that edits ignore rules),
    /// then ignore checks that stop at the first relevant path.
    pub fn batch_is_relevant<'a, I>(&mut self, paths: I) -> bool
    where
        I: IntoIterator<Item = &'a Path>,
        I::IntoIter: Clone,
    {
        let paths = paths.into_iter();
        let edits_gitignore = paths.clone().any(|path| {
            path.file_name().is_some_and(|n| n == ".gitignore")
                && statically_relevant_rel_path(path, &self.workdir).is_some()
        });
        if edits_gitignore {
            self.dir_cache.clear();
            return true;
        }
        let now = Instant::now();
        for path in paths {
            if let Some(rel) = statically_relevant_rel_path(path, &self.workdir)
                && !self.is_ignored(&rel, now)
            {
                return true;
            }
        }
        false
    }

    fn is_ignored(&mut self, rel: &Path, now: Instant) -> bool {
        // An event on an already-known ignored directory itself.
        if self.cached_dir_verdict(rel, now) == Some(true) {
            return true;
        }
        if self.ancestor_dir_ignored(rel, now) {
            // Git cannot re-include a file below an excluded directory.
            return true;
        }
        self.query_ignored(rel)
    }

    /// Whether any proper ancestor directory of `rel` is ignored. Cached
    /// verdicts are consulted deepest-first without allocating; only when some
    /// ancestor is unknown are the ancestors resolved top-down, stopping at the
    /// first ignored one (everything below it is ignored as well).
    fn ancestor_dir_ignored(&mut self, rel: &Path, now: Instant) -> bool {
        let mut any_unknown = false;
        for dir in proper_ancestors(rel) {
            match self.cached_dir_verdict(dir, now) {
                Some(true) => return true,
                Some(false) => {}
                None => any_unknown = true,
            }
        }
        if !any_unknown {
            return false;
        }
        let dirs: Vec<&Path> = proper_ancestors(rel).collect();
        for dir in dirs.into_iter().rev() {
            let ignored = match self.cached_dir_verdict(dir, now) {
                Some(ignored) => ignored,
                None => {
                    let ignored = self.query_ignored(dir);
                    self.cache_dir_verdict(dir, ignored, now);
                    ignored
                }
            };
            if ignored {
                return true;
            }
        }
        false
    }

    fn cached_dir_verdict(&self, dir: &Path, now: Instant) -> Option<bool> {
        self.dir_cache
            .get(dir)
            .filter(|(_, at)| now.saturating_duration_since(*at) < IGNORE_DIR_CACHE_TTL)
            .map(|&(ignored, _)| ignored)
    }

    fn cache_dir_verdict(&mut self, dir: &Path, ignored: bool, now: Instant) {
        if self.dir_cache.len() >= IGNORE_DIR_CACHE_MAX {
            self.dir_cache
                .retain(|_, (_, at)| now.saturating_duration_since(*at) < IGNORE_DIR_CACHE_TTL);
            if self.dir_cache.len() >= IGNORE_DIR_CACHE_MAX {
                self.dir_cache.clear();
            }
        }
        self.dir_cache.insert(dir.to_path_buf(), (ignored, now));
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

/// Non-empty proper ancestors of a relative path, deepest first.
fn proper_ancestors(rel: &Path) -> impl Iterator<Item = &Path> {
    rel.ancestors()
        .skip(1)
        .filter(|dir| !dir.as_os_str().is_empty())
}

/// Workdir-relative path of `path` unless it is outside the worktree, the
/// worktree root itself, or under a hard-coded noise directory. Borrowed (no
/// allocation) when `path` is spelled like `workdir`; Windows `\\?\` verbatim
/// and drive-letter / case variants fall back to a normalized comparison.
fn statically_relevant_rel_path<'a>(path: &'a Path, workdir: &Path) -> Option<Cow<'a, Path>> {
    let rel = rel_path_under(path, workdir)?;
    if rel.as_os_str().is_empty() {
        return None;
    }
    for comp in rel.components() {
        if let Component::Normal(name) = comp
            && (name == ".git" || name == "target" || name == "node_modules" || name == ".idea")
        {
            return None;
        }
    }
    Some(rel)
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
    fn worktree_filter_ancestor_cache_rejects_deep_paths_under_ignored_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let git_dir = init_repo(&root);
        std::fs::write(root.join(".gitignore"), "out/\n").unwrap();
        std::fs::create_dir_all(root.join("out").join("debug")).unwrap();
        std::fs::create_dir_all(root.join("src").join("deep")).unwrap();

        // 500 distinct leaf directories: a parent-only cache would need one
        // lookup per directory; the ancestor cache needs one for `out/`.
        let paths: Vec<PathBuf> = (0..500)
            .map(|i| {
                root.join("out")
                    .join("debug")
                    .join("build")
                    .join(format!("crate-{i}"))
                    .join("out")
                    .join("gen.rs")
            })
            .collect();
        let mut filter = WorktreeEventFilter::new(root.clone(), git_dir);
        assert!(!filter.batch_is_relevant(paths.iter().map(PathBuf::as_path)));
        assert_eq!(filter.ignore_queries(), 1);
        assert!(!filter.batch_is_relevant([root.join("out").as_path()]));
        assert_eq!(filter.ignore_queries(), 1);

        // Non-ignored deep paths still reach the per-file check.
        let source = root.join("src").join("deep").join("lib.rs");
        assert!(filter.batch_is_relevant([source.as_path()]));
    }

    #[test]
    fn worktree_filter_dir_cache_stays_bounded() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let git_dir = init_repo(&root);
        let mut filter = WorktreeEventFilter::new(root.clone(), git_dir);
        let paths: Vec<PathBuf> = (0..IGNORE_DIR_CACHE_MAX + 10)
            .map(|i| root.join(format!("d{i}")).join("f.txt"))
            .collect();
        let now = Instant::now();
        for path in &paths {
            let rel = statically_relevant_rel_path(path, &root).unwrap();
            assert!(!filter.is_ignored(&rel, now));
        }
        assert!(filter.dir_cache.len() <= IGNORE_DIR_CACHE_MAX);
    }

    #[test]
    fn statically_relevant_rel_path_borrows_and_rejects_noise_dirs() {
        let root = Path::new("/repo");
        let src = root.join("src").join("main.rs");
        let rel = statically_relevant_rel_path(&src, root).unwrap();
        assert!(matches!(rel, Cow::Borrowed(_)));
        assert_eq!(rel.as_ref(), Path::new("src").join("main.rs"));
        for noise in [".git", "target", "node_modules", ".idea"] {
            let p = root.join("a").join(noise).join("x");
            assert!(statically_relevant_rel_path(&p, root).is_none(), "{noise}");
        }
        assert!(statically_relevant_rel_path(root, root).is_none());
        assert!(statically_relevant_rel_path(Path::new("/other/a.rs"), root).is_none());
        assert!(statically_relevant_rel_path(Path::new("/repository/a.rs"), root).is_none());
    }

    #[cfg(windows)]
    #[test]
    fn statically_relevant_rel_path_normalizes_verbatim_and_drive_case() {
        let rel_of = |path: &str, root: &str| {
            statically_relevant_rel_path(Path::new(path), Path::new(root)).map(Cow::into_owned)
        };
        let expected = Some(Path::new("src").join("a.rs"));
        let workdir = r"C:\Work\Repo";
        assert_eq!(rel_of(r"\\?\C:\Work\Repo\src\a.rs", workdir), expected);
        assert_eq!(rel_of(r"c:\work\repo\src\a.rs", workdir), expected);
        let verbatim_root = r"\\?\c:\work\repo";
        assert_eq!(rel_of(r"C:\Work\Repo\src\a.rs", verbatim_root), expected);
        assert_eq!(rel_of(r"\\?\C:\Work\Repo\.git\index", workdir), None);
        assert_eq!(rel_of(r"\\?\C:\Work\Repo2\a.rs", workdir), None);
        assert_eq!(rel_of(r"\\?\C:\Work\Repo", workdir), None);
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

    #[tokio::test]
    async fn mutation_guard_acquire_serializes_and_finishes_in_order() {
        let guard = Arc::new(MutationGuard::new());
        let first = guard.acquire().await;
        assert!(guard.is_mutating());
        assert!(guard.record_or_defer(RefreshScope::StageStatus).is_none());

        let guard_for_second = Arc::clone(&guard);
        let second_acquired = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let second_flag = Arc::clone(&second_acquired);
        let waiter = tokio::spawn(async move {
            let permit = guard_for_second.acquire().await;
            second_flag.store(true, Ordering::Release);
            permit.finish()
        });

        tokio::task::yield_now().await;
        assert!(!second_acquired.load(Ordering::Acquire));

        let deferred = first.finish();
        assert_eq!(deferred, RefreshScope::StageStatus);

        let second_deferred = waiter.await.unwrap();
        assert!(second_acquired.load(Ordering::Acquire));
        assert_eq!(second_deferred, RefreshScope::None);
        assert!(!guard.is_mutating());
    }

    #[test]
    fn classify_git_meta_path_drops_object_storms_and_keeps_metadata() {
        let git = Path::new("/repo/.git");
        let classify = |rel: &str| classify_git_meta_path(&git.join(rel), git, git);
        for rel in [
            "objects/ab/cdef0123",
            "objects/pack/pack-1.pack",
            "objects/pack/pack-1.idx",
            "objects/info/commit-graph",
            "objects",
            "logs/HEAD",
            "logs/refs/heads/main",
            "hooks/pre-commit",
            "info/exclude",
            "lfs/objects/aa/bb",
            "modules/sub/HEAD",
            "worktrees/other/HEAD",
            "config",
            "FETCH_HEAD",
            "ORIG_HEAD",
            "COMMIT_EDITMSG",
            "index.lock",
            "refs/heads/main.lock",
            "HEAD.lock",
            "",
        ] {
            assert_eq!(classify(rel), RefreshScope::None, "{rel}");
        }
        assert_eq!(classify("index"), RefreshScope::StageStatus);
        for rel in [
            "HEAD",
            "refs",
            "refs/heads/x",
            "refs/remotes/origin/feature/a",
            "refs/tags/v1",
            "packed-refs",
            "shallow",
            "MERGE_HEAD",
            "REBASE_HEAD",
            "CHERRY_PICK_HEAD",
            "REVERT_HEAD",
            "BISECT_LOG",
            "rebase-merge",
            "rebase-merge/done",
            "rebase-apply",
            "rebase-apply/next",
        ] {
            assert_eq!(classify(rel), RefreshScope::RefsAndGraph, "{rel}");
        }
        assert_eq!(
            classify_git_meta_path(Path::new("/elsewhere/HEAD"), git, git),
            RefreshScope::None
        );
    }

    #[cfg(windows)]
    #[test]
    fn classify_git_meta_path_normalizes_verbatim_and_drive_case() {
        use RefreshScope::{None as Skip, RefsAndGraph as Refs, StageStatus as Stage};
        let classify = |path: &str, git: &str| {
            classify_git_meta_path(Path::new(path), Path::new(git), Path::new(git))
        };
        let git = r"C:\Work\Repo\.git";
        assert_eq!(classify(r"\\?\C:\Work\Repo\.git\HEAD", git), Refs);
        assert_eq!(classify(r"c:\work\repo\.git\index", git), Stage);
        assert_eq!(classify(r"\\?\c:\WORK\Repo\.git\refs\heads\x", git), Refs);
        assert_eq!(classify(r"\\?\C:\Work\Repo\.git\MERGE_HEAD", git), Refs);
        assert_eq!(classify(r"\\?\C:\Work\Repo\.git\objects\ab\cd", git), Skip);
        assert_eq!(classify(r"\\?\C:\Work\Repo\.gitx\HEAD", git), Skip);
        let verbatim_git = r"\\?\C:\Work\Repo\.git";
        assert_eq!(classify(r"c:\work\repo\.git\HEAD", verbatim_git), Refs);

        // Linked worktree with mixed Windows casing/verbatim spelling: `git_dir`
        // must take precedence over `common_dir/worktrees/<name>`.
        let common = Path::new(r"C:\Work\Repo\.git");
        let wt_git = Path::new(r"C:\Work\Repo\.git\worktrees\wt1");
        assert_eq!(
            classify_git_meta_path(
                Path::new(r"C:\Work\Repo\.git\worktrees\WT1\HEAD"),
                wt_git,
                common
            ),
            Refs
        );
        assert_eq!(
            classify_git_meta_path(
                Path::new(r"C:\Work\Repo\.git\worktrees\WT1\index"),
                wt_git,
                common
            ),
            Stage
        );
        assert_eq!(
            classify_git_meta_path(
                Path::new(r"\\?\c:\Work\Repo\.git\worktrees\WT1\index"),
                wt_git,
                common
            ),
            Stage
        );
        assert_eq!(
            classify_git_meta_path(
                Path::new(r"C:\Work\Repo\.git\worktrees\other\HEAD"),
                wt_git,
                common
            ),
            Skip
        );
        assert_eq!(
            classify_git_meta_path(
                Path::new(r"C:\Work\Repo\.git\objects\ab\cd"),
                wt_git,
                common
            ),
            Skip
        );
        assert_eq!(
            classify_git_meta_path(
                Path::new(r"C:\Work\Repo\.git\refs\heads\main"),
                wt_git,
                common
            ),
            Refs
        );
    }

    #[test]
    fn classify_git_meta_path_strips_git_dir_prefix_before_matching() {
        // Ancestor paths containing `logs`, `objects`, or `refs` must not poison classification.
        let logs_git = Path::new(r"C:\Users\dev\logs\myrepo\.git");
        assert_eq!(
            classify_git_meta_path(&logs_git.join("HEAD"), logs_git, logs_git),
            RefreshScope::RefsAndGraph
        );
        assert_eq!(
            classify_git_meta_path(&logs_git.join("index"), logs_git, logs_git),
            RefreshScope::StageStatus
        );
        assert_eq!(
            classify_git_meta_path(&logs_git.join("logs").join("HEAD"), logs_git, logs_git),
            RefreshScope::None
        );

        let objs_git = Path::new("/var/objects/app/.git");
        assert_eq!(
            classify_git_meta_path(
                &objs_git.join("refs").join("heads").join("main"),
                objs_git,
                objs_git
            ),
            RefreshScope::RefsAndGraph
        );
        assert_eq!(
            classify_git_meta_path(
                &objs_git.join("objects").join("ab").join("cdef"),
                objs_git,
                objs_git
            ),
            RefreshScope::None
        );

        let refs_git = Path::new("/home/refs/repo/.git");
        assert_eq!(
            classify_git_meta_path(&refs_git.join("config"), refs_git, refs_git),
            RefreshScope::None
        );
        assert_eq!(
            classify_git_meta_path(&refs_git.join("index.lock"), refs_git, refs_git),
            RefreshScope::None
        );

        // Linked worktree: `git_dir` nested under `common_dir/worktrees/<name>`.
        let common = Path::new("/repo/.git");
        let wt_git = common.join("worktrees").join("wt1");
        assert_eq!(
            classify_git_meta_path(&wt_git.join("HEAD"), &wt_git, common),
            RefreshScope::RefsAndGraph
        );
        assert_eq!(
            classify_git_meta_path(&wt_git.join("index"), &wt_git, common),
            RefreshScope::StageStatus
        );
        assert_eq!(
            classify_git_meta_path(&wt_git.join("logs").join("HEAD"), &wt_git, common),
            RefreshScope::None
        );
        assert_eq!(
            classify_git_meta_path(
                &common.join("refs").join("heads").join("main"),
                &wt_git,
                common
            ),
            RefreshScope::RefsAndGraph
        );
        assert_eq!(
            classify_git_meta_path(
                &common.join("worktrees").join("other").join("index"),
                &wt_git,
                common
            ),
            RefreshScope::None
        );
    }
}
