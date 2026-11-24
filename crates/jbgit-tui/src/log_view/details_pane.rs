use std::sync::Arc;
use std::time::{Duration, Instant};

use jbgit_core::{
    ChangeTreeRow, CommitChangesBundle, DetailsKey, DetailsMode, DiffContext, EpochToken,
    FileChange, FileDiff, FileDiffKey, LruCache, Oid, RequestSlot,
};

use crate::app::ScrollViewport;
pub use crate::diff_session::{ChangesTreeSession, DiffCacheStore, DiffSession};
#[cfg(test)]
pub use crate::diff_session::{ChangesExpandState, DETAILS_CACHE_CAPACITY};

/// Debounce between a selection change and the details request.
pub const DETAILS_DEBOUNCE: Duration = Duration::from_millis(30);
/// Per-snapshot memo of containing-branch BFS results.
pub const CONTAINING_CACHE_CAPACITY: usize = 256;
/// Maximum number of commits auto-aggregated in the Log Changes pane before
/// requiring explicit user confirmation (`VcsLogUtil.getShownChangesLimit()`).
pub const MAX_AGGREGATED_COMMITS: usize = 100;

/// Inline diff preview dock location (`MoveDiffPreviewToRight` / `MoveDiffPreviewToBottom`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DiffPreviewDock {
    /// Replace the bottom-right Commit Details panel.
    #[default]
    Details,
    /// Expand a 4th column on the far right of the Log tab.
    Right,
    /// Expand a full-width bar at the bottom of the Log tab.
    Bottom,
}

impl DiffPreviewDock {
    pub const fn next(self) -> Self {
        match self {
            Self::Details => Self::Right,
            Self::Right => Self::Bottom,
            Self::Bottom => Self::Details,
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Details => "Details",
            Self::Right => "Right",
            Self::Bottom => "Bottom",
        }
    }
}

#[derive(Debug)]
pub struct DetailsPaneState {
    pub changes: ChangesTreeSession,
    pub diff: DiffSession,
    /// Explicit comparison override (`Compare Versions` / `Compare with Local`).
    pub compare_override: Option<DetailsKey>,
    /// Debounced details request: key + deadline.
    pub pending: Option<(DetailsKey, Instant)>,
    /// Which parent a single merge commit is compared against.
    pub merge_parent: usize,
    /// Containing branches/tags for the single selected commit.
    pub containing: Option<(Oid, Vec<String>)>,
    /// Background containing-branch results for the current snapshot.
    pub containing_cache: LruCache<Oid, Vec<String>>,
    /// Single-flight slot tracking the in-flight containing-branch BFS job.
    pub containing_slot: RequestSlot<Oid>,
    pub details_scroll: ScrollViewport,
    /// Inline diff preview visibility.
    pub diff_preview: bool,
    /// Where the inline diff preview is docked (`Details` / `Right` / `Bottom`).
    pub diff_dock: DiffPreviewDock,
    /// Path to auto-select once the next `CommitChangesBundle` resolves.
    pub pending_select_path: Option<String>,
    /// Multi-commit selection withheld by [`MAX_AGGREGATED_COMMITS`] until the
    /// user presses Enter / clicks to confirm loading aggregated changes.
    pub many_changes_blocked: Option<DetailsKey>,
    /// Exact large-selection key the user explicitly confirmed loading.
    pub many_changes_confirmed: Option<DetailsKey>,
}

impl Default for DetailsPaneState {
    fn default() -> Self {
        Self {
            changes: ChangesTreeSession::default(),
            diff: DiffSession::new(DiffContext::Three),
            compare_override: None,
            pending: None,
            merge_parent: 0,
            containing: None,
            containing_cache: LruCache::new(CONTAINING_CACHE_CAPACITY),
            containing_slot: RequestSlot::new(),
            details_scroll: ScrollViewport::new(),
            diff_preview: false,
            diff_dock: DiffPreviewDock::Details,
            pending_select_path: None,
            many_changes_blocked: None,
            many_changes_confirmed: None,
        }
    }
}

impl DetailsPaneState {
    pub fn is_comparing_versions(&self) -> bool {
        self.compare_override
            .as_ref()
            .or(self.changes.current_key.as_ref())
            .is_some_and(|k| k.mode == DetailsMode::CompareVersions && k.oids.len() >= 2)
    }

    pub fn current_bundle<'a>(
        &self,
        store: &'a DiffCacheStore,
    ) -> Option<&'a Arc<CommitChangesBundle>> {
        self.changes.current_bundle(store)
    }

    pub fn change_rows(&self, store: &DiffCacheStore) -> Arc<[ChangeTreeRow]> {
        self.changes.change_rows(store)
    }

    pub fn selected_change<'a>(&self, store: &'a DiffCacheStore) -> Option<&'a FileChange> {
        self.changes.selected_change(store)
    }

    pub fn file_row_positions(&self, store: &DiffCacheStore) -> Vec<usize> {
        self.changes.file_row_positions(store)
    }

    pub fn desired_diff_key(&self, store: &DiffCacheStore) -> Option<FileDiffKey> {
        self.changes.desired_diff_key(store, self.diff.context)
    }

    pub fn current_diff<'a>(&self, store: &'a DiffCacheStore) -> Option<&'a Arc<FileDiff>> {
        store.peek_diff(&self.desired_diff_key(store)?)
    }

    pub fn diff_visible(&self) -> bool {
        self.diff_preview || self.diff.zoomed
    }

    pub fn fallback_diff_max_scroll(&self, store: &DiffCacheStore) -> usize {
        self.diff.fallback_max_scroll(self.current_diff(store))
    }

    pub fn next_deadline(&self) -> Option<Instant> {
        [self.pending.as_ref().map(|(_, at)| *at), self.diff.next_deadline()]
            .into_iter()
            .flatten()
            .min()
    }

    pub fn reset_containing(&mut self) {
        self.containing = None;
        self.containing_cache.clear();
        self.containing_slot.invalidate();
    }

    pub fn request_containing_branches(&mut self, oid: Oid) -> Option<EpochToken> {
        if self.pending.is_some() || self.containing.as_ref().is_some_and(|(o, _)| *o == oid) {
            return None;
        }
        if let Some(names) = self.containing_cache.get(&oid).cloned() {
            self.containing = Some((oid, names));
            return None;
        }
        self.containing_slot.begin_token(oid)
    }

    pub fn resolve_containing_branches(
        &mut self,
        oid: Oid,
        token: &EpochToken,
        names: Vec<String>,
    ) -> bool {
        if !self.containing_slot.is_in_flight(&oid)
            || !self.containing_slot.finish_token(&oid, token)
        {
            return false;
        }
        self.containing_cache.insert(oid, names.clone());
        let selected = self
            .changes
            .current_key
            .as_ref()
            .is_some_and(|k| k.oids.as_slice() == [oid]);
        if selected {
            self.containing = Some((oid, names));
            true
        } else {
            false
        }
    }

    pub fn jump_diff_hunk(&mut self, delta: isize, store: &DiffCacheStore) {
        let diff = self.current_diff(store);
        self.diff.jump_hunk(delta, diff);
    }

    pub fn select_next_file(&mut self, delta: isize, store: &DiffCacheStore) {
        self.changes.select_next_file(delta, store);
        self.diff.scroll.reset();
    }

    /// Consumes `pending_select_path` once the active bundle has loaded.
    /// Returns `Some((path, found))` when a pending selection was resolved.
    pub fn try_select_pending_path(&mut self, store: &DiffCacheStore) -> Option<(String, bool)> {
        let target = self.pending_select_path.as_ref()?;
        self.current_bundle(store)?;
        let path = target.clone();
        self.pending_select_path = None;
        let found = self.changes.select_path(&path, store);
        if found {
            self.diff.scroll.reset();
        }
        Some((path, found))
    }
}
