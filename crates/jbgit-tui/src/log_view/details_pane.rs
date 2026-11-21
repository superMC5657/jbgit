use std::cell::RefCell;
use std::sync::Arc;
use std::time::{Duration, Instant};

use jbgit_core::{
    ChangeRowKind, ChangeTreeRow, CommitChangesBundle, DetailsKey, DiffContext, EpochToken,
    FileChange, FileDiff, FileDiffKey, LruCache, Oid, RequestEpoch, SignatureStatus,
    build_change_rows, side_by_side_rows,
};
use rustc_hash::FxHashSet;

use crate::syntax::{HighlightedDiff, LazyWordDiffCache};

/// Debounce between a selection change and the details request.
pub const DETAILS_DEBOUNCE: Duration = Duration::from_millis(30);
pub const DETAILS_CACHE_CAPACITY: usize = 256;
pub const DIFF_CACHE_CAPACITY: usize = 64;
/// Per-snapshot memo of containing-branch BFS results.
pub const CONTAINING_CACHE_CAPACITY: usize = 256;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangesExpandState {
    AllExpanded,
    AllCollapsed,
    Mixed,
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffRow {
    Header(usize),
    Unified(usize, usize),
    Split(usize, Option<usize>, Option<usize>),
}

pub fn build_file_diff_rows(diff: &FileDiff, side_by_side: bool) -> Vec<DiffRow> {
    let mut rows = Vec::new();
    for (h, hunk) in diff.hunks.iter().enumerate() {
        rows.push(DiffRow::Header(h));
        if side_by_side {
            rows.extend(
                side_by_side_rows(hunk)
                    .into_iter()
                    .map(|r| DiffRow::Split(h, r.left, r.right)),
            );
        } else {
            rows.extend((0..hunk.lines.len()).map(|l| DiffRow::Unified(h, l)));
        }
    }
    rows
}

#[derive(Debug)]
struct CachedFileDiffRows {
    diff: Arc<FileDiff>,
    side_by_side: bool,
    rows: Arc<[DiffRow]>,
}

#[derive(Debug, Default)]
pub(crate) struct FileDiffRowsCache {
    entry: RefCell<Option<CachedFileDiffRows>>,
    pub(crate) word_cache: LazyWordDiffCache<FileDiff>,
}

impl Clone for FileDiffRowsCache {
    fn clone(&self) -> Self {
        Self::default()
    }
}

impl FileDiffRowsCache {
    pub fn rows_for(&self, diff: &Arc<FileDiff>, side_by_side: bool) -> Arc<[DiffRow]> {
        {
            let guard = self.entry.borrow();
            if let Some(cached) = guard.as_ref()
                && cached.side_by_side == side_by_side
                && Arc::ptr_eq(&cached.diff, diff)
            {
                return Arc::clone(&cached.rows);
            }
        }
        let rows: Arc<[DiffRow]> = build_file_diff_rows(diff, side_by_side).into();
        *self.entry.borrow_mut() = Some(CachedFileDiffRows {
            diff: Arc::clone(diff),
            side_by_side,
            rows: Arc::clone(&rows),
        });
        rows
    }
}

#[derive(Debug)]
pub(crate) struct CachedChangeRows {
    bundle: Arc<CommitChangesBundle>,
    group_by_dir: bool,
    collapsed: FxHashSet<String>,
    rows: Arc<[ChangeTreeRow]>,
}

pub(crate) type CachedAllChangeRowsEntry = (Arc<CommitChangesBundle>, Arc<[ChangeTreeRow]>);

#[derive(Debug)]
pub struct DetailsPaneState {
    pub cache: LruCache<DetailsKey, Arc<CommitChangesBundle>>,
    pub diff_cache: LruCache<FileDiffKey, Arc<FileDiff>>,
    pub diff_syntax: LruCache<FileDiffKey, Arc<HighlightedDiff>>,
    pub signature_overrides: LruCache<Oid, SignatureStatus>,
    /// Details the right panes currently represent.
    pub current_key: Option<DetailsKey>,
    /// Explicit comparison override (`Compare Versions` / `Compare with Local`).
    pub compare_override: Option<DetailsKey>,
    /// Debounced request: key + deadline.
    pub pending: Option<(DetailsKey, Instant)>,
    /// Which parent a single merge commit is compared against.
    pub merge_parent: usize,
    pub requested_diff: Option<FileDiffKey>,
    /// Containing branches/tags for the single selected commit.
    pub containing: Option<(Oid, Vec<String>)>,
    /// Background containing-branch results for the current snapshot.
    pub containing_cache: LruCache<Oid, Vec<String>>,
    /// Commit + epoch token of the in-flight containing-branch job.
    pub containing_requested: Option<(Oid, EpochToken)>,
    pub containing_epochs: RequestEpoch,

    pub changes_selected: usize,
    pub changes_group_by_dir: bool,
    pub changes_collapsed: FxHashSet<String>,

    pub details_scroll: usize,
    /// Inline diff preview visibility.
    pub diff_preview: bool,
    /// Where the inline diff preview is docked (`Details` / `Right` / `Bottom`).
    pub diff_dock: DiffPreviewDock,
    /// Full-screen zoomed diff (`F4` / `Ctrl+Enter`).
    pub diff_zoomed: bool,
    pub diff_side_by_side: bool,
    pub diff_context: DiffContext,
    pub diff_scroll: usize,
    /// Path to auto-select once the next `CommitChangesBundle` resolves.
    pub pending_select_path: Option<String>,

    pub(crate) cached_change_rows: RefCell<Option<CachedChangeRows>>,
    pub(crate) cached_all_change_rows: RefCell<Option<CachedAllChangeRowsEntry>>,
    pub(crate) diff_rows_cache: FileDiffRowsCache,
}

impl Default for DetailsPaneState {
    fn default() -> Self {
        Self {
            cache: LruCache::new(DETAILS_CACHE_CAPACITY),
            diff_cache: LruCache::new(DIFF_CACHE_CAPACITY),
            diff_syntax: LruCache::new(DIFF_CACHE_CAPACITY),
            signature_overrides: LruCache::new(DETAILS_CACHE_CAPACITY),
            current_key: None,
            compare_override: None,
            pending: None,
            merge_parent: 0,
            requested_diff: None,
            containing: None,
            containing_cache: LruCache::new(CONTAINING_CACHE_CAPACITY),
            containing_requested: None,
            containing_epochs: RequestEpoch::new(),
            changes_selected: 0,
            changes_group_by_dir: true,
            changes_collapsed: FxHashSet::default(),
            details_scroll: 0,
            diff_preview: false,
            diff_dock: DiffPreviewDock::Details,
            diff_zoomed: false,
            diff_side_by_side: false,
            diff_context: DiffContext::Three,
            diff_scroll: 0,
            pending_select_path: None,
            cached_change_rows: RefCell::new(None),
            cached_all_change_rows: RefCell::new(None),
            diff_rows_cache: FileDiffRowsCache::default(),
        }
    }
}

impl DetailsPaneState {
    pub fn current_bundle(&self) -> Option<&Arc<CommitChangesBundle>> {
        self.cache.peek(self.current_key.as_ref()?)
    }

    pub fn change_rows(&self) -> Arc<[ChangeTreeRow]> {
        let Some(bundle) = self.current_bundle() else {
            return Arc::from([]);
        };
        {
            let guard = self.cached_change_rows.borrow();
            if let Some(cached) = guard.as_ref()
                && Arc::ptr_eq(&cached.bundle, bundle)
                && cached.group_by_dir == self.changes_group_by_dir
                && cached.collapsed == self.changes_collapsed
            {
                return Arc::clone(&cached.rows);
            }
        }
        let rows: Arc<[ChangeTreeRow]> = build_change_rows(
            &bundle.changes,
            self.changes_group_by_dir,
            &self.changes_collapsed,
        )
        .into();
        *self.cached_change_rows.borrow_mut() = Some(CachedChangeRows {
            bundle: Arc::clone(bundle),
            group_by_dir: self.changes_group_by_dir,
            collapsed: self.changes_collapsed.clone(),
            rows: Arc::clone(&rows),
        });
        rows
    }

    pub fn all_expanded_change_rows(&self) -> Option<Arc<[ChangeTreeRow]>> {
        let bundle = self.current_bundle()?;
        {
            let guard = self.cached_all_change_rows.borrow();
            if let Some((b, rows)) = guard.as_ref()
                && Arc::ptr_eq(b, bundle)
            {
                return Some(Arc::clone(rows));
            }
        }
        let rows: Arc<[ChangeTreeRow]> =
            build_change_rows(&bundle.changes, true, &FxHashSet::default()).into();
        *self.cached_all_change_rows.borrow_mut() = Some((Arc::clone(bundle), Arc::clone(&rows)));
        Some(rows)
    }

    pub fn diff_rows_for(&self, diff: &Arc<FileDiff>, side_by_side: bool) -> Arc<[DiffRow]> {
        self.diff_rows_cache.rows_for(diff, side_by_side)
    }

    /// Returns the expansion state for the current changes tree's root directories.
    pub fn changes_expand_state(&self) -> ChangesExpandState {
        if !self.changes_group_by_dir {
            return ChangesExpandState::Unavailable;
        }
        let Some(all) = self.all_expanded_change_rows() else {
            return ChangesExpandState::Unavailable;
        };
        let mut has_dirs = false;
        let mut all_expanded = true;
        let mut all_collapsed = true;
        for row in all.iter() {
            if matches!(row.kind, ChangeRowKind::Dir { .. }) {
                has_dirs = true;
                let collapsed = self.changes_collapsed.contains(&row.key);
                if collapsed {
                    all_expanded = false;
                } else if row.depth == 0 {
                    all_collapsed = false;
                }
            }
        }
        match (has_dirs, all_expanded, all_collapsed) {
            (false, _, _) => ChangesExpandState::Unavailable,
            (true, true, false) => ChangesExpandState::AllExpanded,
            (true, false, true) => ChangesExpandState::AllCollapsed,
            _ => ChangesExpandState::Mixed,
        }
    }

    /// Selected changed file, if the selected row is a file.
    pub fn selected_change(&self) -> Option<&FileChange> {
        let bundle = self.current_bundle()?;
        let rows = self.change_rows();
        match rows.get(self.changes_selected)?.kind {
            ChangeRowKind::File { change_idx } => bundle.changes.get(change_idx),
            ChangeRowKind::Dir { .. } => None,
        }
    }

    /// File-row positions (indices into `change_rows`) for `[` / `]` paging.
    pub fn file_row_positions(&self) -> Vec<usize> {
        self.change_rows()
            .iter()
            .enumerate()
            .filter(|(_, r)| matches!(r.kind, ChangeRowKind::File { .. }))
            .map(|(i, _)| i)
            .collect()
    }

    pub fn desired_diff_key(&self) -> Option<FileDiffKey> {
        let details = self.current_key.clone()?;
        let change = self.selected_change()?;
        Some(FileDiffKey {
            details,
            path: change.path.clone(),
            old_path: change.old_path.clone(),
            context: self.diff_context,
        })
    }

    pub fn current_diff(&self) -> Option<&Arc<FileDiff>> {
        self.diff_cache.peek(&self.desired_diff_key()?)
    }

    pub fn current_diff_syntax(&self) -> Option<&Arc<HighlightedDiff>> {
        self.diff_syntax.peek(&self.desired_diff_key()?)
    }

    pub fn diff_visible(&self) -> bool {
        self.diff_preview || self.diff_zoomed
    }

    /// Fallback upper scroll bound used before the diff pane has rendered a frame.
    pub fn fallback_diff_max_scroll(&self) -> usize {
        self.current_diff()
            .map_or(usize::MAX, |_| self.diff_total_rows().saturating_sub(1))
    }

    pub fn next_deadline(&self) -> Option<Instant> {
        self.pending.as_ref().map(|(_, at)| *at)
    }

    pub fn reset_containing(&mut self) {
        self.containing = None;
        self.containing_cache.clear();
        self.containing_requested = None;
        let _ = self.containing_epochs.issue();
    }

    /// Resolves a single-commit containing-branch query from cache or issues a
    /// new cancellable [`EpochToken`] when an off-thread BFS is needed.
    pub fn request_containing_branches(&mut self, oid: Oid) -> Option<EpochToken> {
        if self.pending.is_some() || self.containing.as_ref().is_some_and(|(o, _)| *o == oid) {
            return None;
        }
        if let Some(names) = self.containing_cache.get(&oid).cloned() {
            self.containing = Some((oid, names));
            return None;
        }
        if self
            .containing_requested
            .as_ref()
            .is_some_and(|(o, _)| *o == oid)
        {
            return None;
        }
        let token = self.containing_epochs.issue();
        self.containing_requested = Some((oid, token.clone()));
        Some(token)
    }

    /// Applies an off-thread containing-branch result if it is still current.
    /// Returns `true` when the active pane display changed.
    pub fn resolve_containing_branches(
        &mut self,
        oid: Oid,
        token: &EpochToken,
        names: Vec<String>,
    ) -> bool {
        let is_current = self
            .containing_requested
            .as_ref()
            .is_some_and(|(o, t)| *o == oid && t.epoch() == token.epoch() && !token.is_stale());
        if !is_current {
            return false;
        }
        self.containing_requested = None;
        self.containing_cache.insert(oid, names.clone());
        let selected = self
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

    pub fn diff_total_rows(&self) -> usize {
        let Some(diff) = self.current_diff() else {
            return 0;
        };
        if diff.is_too_large || diff.is_binary || diff.hunks.is_empty() {
            return 0;
        }
        self.diff_rows_for(diff, self.diff_side_by_side).len() + usize::from(diff.is_truncated)
    }

    pub fn hunk_header_offsets(&self) -> Vec<usize> {
        let Some(diff) = self.current_diff() else {
            return Vec::new();
        };
        self.diff_rows_for(diff, self.diff_side_by_side)
            .iter()
            .enumerate()
            .filter_map(|(idx, r)| matches!(r, DiffRow::Header(_)).then_some(idx))
            .collect()
    }

    pub fn jump_diff_hunk(&mut self, delta: isize) {
        let offsets = self.hunk_header_offsets();
        if offsets.is_empty() {
            return;
        }
        let cur = self.diff_scroll;
        let target = if delta >= 0 {
            offsets
                .iter()
                .copied()
                .find(|&o| o > cur)
                .unwrap_or(offsets[0])
        } else {
            offsets
                .iter()
                .copied()
                .rev()
                .find(|&o| o < cur)
                .unwrap_or(*offsets.last().unwrap())
        };
        self.diff_scroll = target;
    }

    pub fn select_next_file(&mut self, delta: isize) {
        let files = self.file_row_positions();
        if files.is_empty() {
            return;
        }
        let cur = files
            .iter()
            .position(|&p| p == self.changes_selected)
            .map_or(0, |i| i as isize + delta);
        let next = cur.clamp(0, files.len() as isize - 1) as usize;
        self.changes_selected = files[next];
        self.diff_scroll = 0;
    }

    /// Resolves `pending_select_path` once the current details bundle is available,
    /// expanding any collapsed ancestor directories so the file row is visible.
    pub fn try_select_pending_path(&mut self) -> Option<(String, bool)> {
        let bundle = self.current_bundle()?.clone();
        let path = self.pending_select_path.take()?;
        if bundle.changes.iter().any(|c| c.path == path) {
            let to_expand: Vec<String> = self
                .changes_collapsed
                .iter()
                .filter(|dir| {
                    path.starts_with(dir.as_str())
                        && path.as_bytes().get(dir.len()) == Some(&b'/')
                })
                .cloned()
                .collect();
            for dir in to_expand {
                self.changes_collapsed.remove(&dir);
            }
            if let Some(idx) = self
                .change_rows()
                .iter()
                .position(|r| matches!(r.kind, ChangeRowKind::File { .. }) && r.key == path)
            {
                self.changes_selected = idx;
                self.diff_scroll = 0;
                return Some((path, true));
            }
        }
        Some((path, false))
    }
}
