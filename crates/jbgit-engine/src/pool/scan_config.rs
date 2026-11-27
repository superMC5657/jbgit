use jbgit_core::Oid;

/// Default number of commits loaded per Log page (initial load and each `Load More` step).
pub const DEFAULT_COMMIT_PAGE_SIZE: usize = 50_000;
/// Index size above which worktree-side rename detection is skipped during the
/// status scan. Pairing every deleted file with every untracked file is the
/// dominant cost on large (especially Windows) worktrees; staged renames
/// (`HEAD -> index`) are still detected.
pub const LARGE_INDEX_ENTRIES: usize = 20_000;
/// Index size above which `copies_from_unmodified` copy detection (pairing every
/// newly added file with every unmodified `HEAD` file, `O(added x index)`) is
/// skipped. Copies from modified/deleted sources are still detected.
pub const COPY_SCAN_MAX_INDEX_ENTRIES: usize = 2_000;
/// Number of newly added staged files above which `copies_from_unmodified`
/// copy detection is skipped (bulk `git add` of many files).
pub const COPY_SCAN_MAX_ADDED: usize = 32;

/// Whether the expensive `copies_from_unmodified` similarity scan may run for an
/// index of `index_entries` entries with `added` newly staged files.
pub fn copies_from_unmodified_allowed(index_entries: usize, added: usize) -> bool {
    index_entries <= COPY_SCAN_MAX_INDEX_ENTRIES && added <= COPY_SCAN_MAX_ADDED
}

/// Local branches (besides `HEAD`, always computed) that get an ahead/behind
/// count per snapshot, most recently committed first.
pub const AHEAD_BEHIND_BRANCH_LIMIT: usize = 32;
/// Wall-clock budget for those extra ahead/behind walks per snapshot.
pub const AHEAD_BEHIND_TIME_BUDGET: std::time::Duration = std::time::Duration::from_millis(150);

/// Strategy for an optional status-scan pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StatusScanMode {
    /// Enable on repositories up to `large_index_entries`, skip above it.
    #[default]
    Auto,
    Always,
    Never,
}

impl StatusScanMode {
    pub fn resolve(self, entries: usize, threshold: usize) -> bool {
        match self {
            Self::Auto => entries <= threshold,
            Self::Always => true,
            Self::Never => false,
        }
    }
}

/// Configurable performance knobs for repository snapshot and worktree status scans.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StatusScanConfig {
    pub large_index_entries: usize,
    pub worktree_renames: StatusScanMode,
    pub recurse_untracked_dirs: StatusScanMode,
    pub load_commit_bodies: bool,
    /// Commit pagination step (initial load limit and increment per `LoadMoreCommits`).
    pub commit_page_size: usize,
}

impl Default for StatusScanConfig {
    fn default() -> Self {
        Self {
            large_index_entries: LARGE_INDEX_ENTRIES,
            worktree_renames: StatusScanMode::Auto,
            recurse_untracked_dirs: StatusScanMode::Always,
            load_commit_bodies: true,
            commit_page_size: DEFAULT_COMMIT_PAGE_SIZE,
        }
    }
}

impl StatusScanConfig {
    /// [`copies_from_unmodified_allowed`], additionally capped by a lower
    /// configured [`Self::large_index_entries`].
    pub fn copies_from_unmodified(&self, index_entries: usize, added: usize) -> bool {
        index_entries <= self.large_index_entries
            && copies_from_unmodified_allowed(index_entries, added)
    }

    /// Returns a positive commit page size (falling back to [`DEFAULT_COMMIT_PAGE_SIZE`]
    /// if configured as `0`).
    pub fn effective_commit_page_size(&self) -> usize {
        if self.commit_page_size == 0 {
            DEFAULT_COMMIT_PAGE_SIZE
        } else {
            self.commit_page_size
        }
    }

    pub fn policy(&self, index_entries: usize) -> StatusScanPolicy {
        StatusScanPolicy {
            worktree_renames: self
                .worktree_renames
                .resolve(index_entries, self.large_index_entries),
            recurse_untracked_dirs: self
                .recurse_untracked_dirs
                .resolve(index_entries, self.large_index_entries),
        }
    }
}

/// Status-scan options chosen for a repository of a given size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StatusScanPolicy {
    pub worktree_renames: bool,
    pub recurse_untracked_dirs: bool,
}

impl StatusScanPolicy {
    pub fn for_index_entries(entries: usize) -> Self {
        StatusScanConfig::default().policy(entries)
    }
}

/// Backfilled `(full_ref_name, (ahead, behind))` pair.
pub type AheadBehindUpdate = (String, (usize, usize));

/// A local tracking branch whose `ahead_behind` walk was deferred past the initial
/// snapshot budget so cold start stays bounded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingAheadBehind {
    pub full_name: String,
    pub local: Oid,
    pub upstream: Oid,
}
