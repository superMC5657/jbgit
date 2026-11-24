//! Shared cross-workbench diff cache store, file-changes tree session,
//! diff viewing session, and unified row projection model.
//!
//! Serves `[2 Log]`, `[3 Stash]`, `File History`, and `[1 Commit]` from one
//! deep implementation instead of cloning LRU caches or duplicating tree/diff
//! state machines per workbench.

use std::cell::RefCell;
use std::sync::Arc;
use std::time::{Duration, Instant};

use jbgit_core::{
    ChangeRowKind, ChangeTreeRow, CommitChangesBundle, CommitSignature, DetailsKey, DiffContext,
    DiffHunk, FileChange, FileDiff, FileDiffKey, LruCache, Oid, PathFilter, StageDiffHunk,
    build_change_rows_filtered, side_by_side_rows,
};
use rustc_hash::FxHashSet;

use crate::app::ScrollViewport;
use crate::syntax::{HighlightedDiff, LazyWordDiffCache};

pub const DETAILS_CACHE_CAPACITY: usize = 256;
pub const DIFF_CACHE_CAPACITY: usize = 64;
pub const SIGNATURES_CACHE_CAPACITY: usize = 1024;

/// Single global LRU store shared by Log, Stash, and File History so any
/// commit details, file diff, or syntax highlight loaded in one view is
/// immediately available in every sibling view with zero dual-write sync.
#[derive(Debug)]
pub struct DiffCacheStore {
    pub details: LruCache<DetailsKey, Arc<CommitChangesBundle>>,
    pub diffs: LruCache<FileDiffKey, Arc<FileDiff>>,
    pub syntax: LruCache<FileDiffKey, Arc<HighlightedDiff>>,
    pub signatures: LruCache<Oid, CommitSignature>,
}

impl Default for DiffCacheStore {
    fn default() -> Self {
        Self::new()
    }
}

impl DiffCacheStore {
    pub fn new() -> Self {
        Self {
            details: LruCache::new(DETAILS_CACHE_CAPACITY),
            diffs: LruCache::new(DIFF_CACHE_CAPACITY),
            syntax: LruCache::new(DIFF_CACHE_CAPACITY),
            signatures: LruCache::new(SIGNATURES_CACHE_CAPACITY),
        }
    }

    pub fn peek_details(&self, key: &DetailsKey) -> Option<&Arc<CommitChangesBundle>> {
        self.details.peek(key)
    }

    pub fn insert_details(&mut self, bundle: Arc<CommitChangesBundle>) {
        self.details.insert(bundle.key.clone(), bundle);
    }

    pub fn peek_diff(&self, key: &FileDiffKey) -> Option<&Arc<FileDiff>> {
        self.diffs.peek(key)
    }

    pub fn get_diff(&mut self, key: &FileDiffKey) -> Option<Arc<FileDiff>> {
        self.diffs.get(key).cloned()
    }

    pub fn insert_diff(&mut self, diff: Arc<FileDiff>) {
        self.diffs.insert(diff.key.clone(), diff);
    }

    pub fn peek_syntax(&self, key: &FileDiffKey) -> Option<&Arc<HighlightedDiff>> {
        self.syntax.peek(key)
    }

    pub fn get_syntax(&mut self, key: &FileDiffKey) -> Option<Arc<HighlightedDiff>> {
        self.syntax.get(key).cloned()
    }

    pub fn insert_syntax(&mut self, key: FileDiffKey, highlighted: Arc<HighlightedDiff>) {
        self.syntax.insert(key, highlighted);
    }

    /// Looks up cached blob OIDs for `key` from `details` so the engine can
    /// take the `O(1)` blob-lookup fast path instead of re-diffing trees.
    pub fn known_oids_for(&self, key: &FileDiffKey) -> Option<(Option<Oid>, Option<Oid>)> {
        let bundle = self.details.peek(&key.details)?;
        let change = bundle
            .changes
            .iter()
            .find(|c| c.path == key.path && c.old_path == key.old_path)
            .or_else(|| bundle.changes.iter().find(|c| c.path == key.path))?;
        (change.old_oid.is_some() || change.new_oid.is_some())
            .then_some((change.old_oid, change.new_oid))
    }
}

/// Unified projected diff row shared across Log, Stash, File History, and Staging Workbench.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffRow {
    Header(usize),
    Unified(usize, usize),
    Split(usize, Option<usize>, Option<usize>),
}

fn build_diff_rows_from_hunks<'a>(
    hunks: impl Iterator<Item = &'a DiffHunk>,
    side_by_side: bool,
) -> Vec<DiffRow> {
    let mut rows = Vec::new();
    for (h, hunk) in hunks.enumerate() {
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

pub fn build_file_diff_rows(diff: &FileDiff, side_by_side: bool) -> Vec<DiffRow> {
    build_diff_rows_from_hunks(diff.hunks.iter(), side_by_side)
}

pub fn build_stage_diff_rows(hunks: &[StageDiffHunk], side_by_side: bool) -> Vec<DiffRow> {
    build_diff_rows_from_hunks(hunks.iter().map(|sh| &sh.hunk), side_by_side)
}

#[derive(Debug)]
struct CachedFileDiffRows {
    diff: Arc<FileDiff>,
    side_by_side: bool,
    rows: Arc<[DiffRow]>,
}

#[derive(Debug, Default)]
pub struct FileDiffRowsCache {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangesExpandState {
    AllExpanded,
    AllCollapsed,
    Mixed,
    Unavailable,
}

#[derive(Debug)]
struct CachedChangeRows {
    bundle: Arc<CommitChangesBundle>,
    group_by_dir: bool,
    collapsed: FxHashSet<String>,
    affected_filter: Option<PathFilter>,
    rows: Arc<[ChangeTreeRow]>,
}

#[derive(Debug)]
struct CachedAllChangeRows {
    bundle: Arc<CommitChangesBundle>,
    affected_filter: Option<PathFilter>,
    rows: Arc<[ChangeTreeRow]>,
}

/// Shared changed-files directory tree session used by both `[2 Log]` and `[3 Stash]`.
#[derive(Debug)]
pub struct ChangesTreeSession {
    pub current_key: Option<DetailsKey>,
    pub selected: usize,
    pub group_by_dir: bool,
    pub collapsed: FxHashSet<String>,
    /// Active path filter applied when "Show Only Affected Changes" is enabled.
    pub affected_filter: Option<PathFilter>,
    cached_change_rows: RefCell<Option<CachedChangeRows>>,
    cached_all_change_rows: RefCell<Option<CachedAllChangeRows>>,
}

impl Default for ChangesTreeSession {
    fn default() -> Self {
        Self {
            current_key: None,
            selected: 0,
            group_by_dir: true,
            collapsed: FxHashSet::default(),
            affected_filter: None,
            cached_change_rows: RefCell::new(None),
            cached_all_change_rows: RefCell::new(None),
        }
    }
}

impl ChangesTreeSession {
    pub fn current_bundle<'a>(
        &self,
        store: &'a DiffCacheStore,
    ) -> Option<&'a Arc<CommitChangesBundle>> {
        store.peek_details(self.current_key.as_ref()?)
    }

    fn matches_affected_filter(&self, change: &FileChange) -> bool {
        self.affected_filter
            .as_ref()
            .is_none_or(|pf| pf.matches_change(&change.path, change.old_path.as_deref()))
    }

    /// Returns `(shown_count, total_count)` for the currently loaded bundle.
    pub fn visible_change_counts(&self, store: &DiffCacheStore) -> Option<(usize, usize)> {
        let bundle = self.current_bundle(store)?;
        let total = bundle.changes.len();
        let shown = if self.affected_filter.is_some() {
            bundle
                .changes
                .iter()
                .filter(|c| self.matches_affected_filter(c))
                .count()
        } else {
            total
        };
        Some((shown, total))
    }

    /// Returns `(additions, deletions)` summed over currently visible (filtered) changes.
    pub fn visible_totals(&self, store: &DiffCacheStore) -> (usize, usize) {
        let Some(bundle) = self.current_bundle(store) else {
            return (0, 0);
        };
        if self.affected_filter.is_none() {
            return bundle.totals();
        }
        bundle
            .changes
            .iter()
            .filter(|c| self.matches_affected_filter(c))
            .fold((0, 0), |(a, d), c| (a + c.additions, d + c.deletions))
    }

    pub fn change_rows(&self, store: &DiffCacheStore) -> Arc<[ChangeTreeRow]> {
        let Some(bundle) = self.current_bundle(store) else {
            return Arc::from([]);
        };
        {
            let guard = self.cached_change_rows.borrow();
            if let Some(cached) = guard.as_ref()
                && Arc::ptr_eq(&cached.bundle, bundle)
                && cached.group_by_dir == self.group_by_dir
                && cached.collapsed == self.collapsed
                && cached.affected_filter == self.affected_filter
            {
                return Arc::clone(&cached.rows);
            }
        }
        let rows: Arc<[ChangeTreeRow]> = build_change_rows_filtered(
            &bundle.changes,
            self.group_by_dir,
            &self.collapsed,
            |c| self.matches_affected_filter(c),
        )
        .into();
        *self.cached_change_rows.borrow_mut() = Some(CachedChangeRows {
            bundle: Arc::clone(bundle),
            group_by_dir: self.group_by_dir,
            collapsed: self.collapsed.clone(),
            affected_filter: self.affected_filter.clone(),
            rows: Arc::clone(&rows),
        });
        rows
    }

    pub fn all_expanded_change_rows(&self, store: &DiffCacheStore) -> Option<Arc<[ChangeTreeRow]>> {
        let bundle = self.current_bundle(store)?;
        {
            let guard = self.cached_all_change_rows.borrow();
            if let Some(cached) = guard.as_ref()
                && Arc::ptr_eq(&cached.bundle, bundle)
                && cached.affected_filter == self.affected_filter
            {
                return Some(Arc::clone(&cached.rows));
            }
        }
        let rows: Arc<[ChangeTreeRow]> = build_change_rows_filtered(
            &bundle.changes,
            true,
            &FxHashSet::default(),
            |c| self.matches_affected_filter(c),
        )
        .into();
        *self.cached_all_change_rows.borrow_mut() = Some(CachedAllChangeRows {
            bundle: Arc::clone(bundle),
            affected_filter: self.affected_filter.clone(),
            rows: Arc::clone(&rows),
        });
        Some(rows)
    }

    pub fn expand_state(&self, store: &DiffCacheStore) -> ChangesExpandState {
        if !self.group_by_dir {
            return ChangesExpandState::Unavailable;
        }
        let Some(all) = self.all_expanded_change_rows(store) else {
            return ChangesExpandState::Unavailable;
        };
        let mut has_dirs = false;
        let mut all_expanded = true;
        let mut all_collapsed = true;
        for row in all.iter() {
            if matches!(row.kind, ChangeRowKind::Dir { .. }) {
                has_dirs = true;
                let collapsed = self.collapsed.contains(&row.key);
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

    pub fn selected_change<'a>(&self, store: &'a DiffCacheStore) -> Option<&'a FileChange> {
        let bundle = self.current_bundle(store)?;
        let rows = self.change_rows(store);
        match rows.get(self.selected)?.kind {
            ChangeRowKind::File { change_idx } => bundle.changes.get(change_idx),
            ChangeRowKind::Dir { .. } => None,
        }
    }

    pub fn file_row_positions(&self, store: &DiffCacheStore) -> Vec<usize> {
        self.change_rows(store)
            .iter()
            .enumerate()
            .filter(|(_, r)| matches!(r.kind, ChangeRowKind::File { .. }))
            .map(|(i, _)| i)
            .collect()
    }

    pub fn desired_diff_key(
        &self,
        store: &DiffCacheStore,
        context: DiffContext,
    ) -> Option<FileDiffKey> {
        let details = self.current_key.clone()?;
        let change = self.selected_change(store)?;
        Some(FileDiffKey {
            details,
            path: change.path.clone(),
            old_path: change.old_path.clone(),
            context,
        })
    }

    pub fn move_selection(&mut self, delta: isize, store: &DiffCacheStore) {
        let len = self.change_rows(store).len();
        if len == 0 {
            self.selected = 0;
            return;
        }
        self.selected = (self.selected as isize + delta).clamp(0, len as isize - 1) as usize;
    }

    pub fn select_row(&mut self, idx: usize, store: &DiffCacheStore) {
        let len = self.change_rows(store).len();
        if len == 0 {
            self.selected = 0;
        } else {
            self.selected = idx.min(len - 1);
        }
    }

    pub fn select_next_file(&mut self, delta: isize, store: &DiffCacheStore) {
        let files = self.file_row_positions(store);
        if files.is_empty() {
            return;
        }
        let cur = files
            .iter()
            .position(|&p| p == self.selected)
            .map_or(0, |i| i as isize + delta);
        let next = cur.rem_euclid(files.len() as isize) as usize;
        self.selected = files[next];
    }

    pub fn toggle_group_by_dir(&mut self) {
        self.group_by_dir = !self.group_by_dir;
        self.selected = 0;
    }

    pub fn expand_or_collapse(&mut self, expand: bool, store: &DiffCacheStore) {
        let rows = self.change_rows(store);
        if let Some(row) = rows.get(self.selected)
            && matches!(row.kind, ChangeRowKind::Dir { .. })
        {
            if expand {
                self.collapsed.remove(&row.key);
            } else {
                self.collapsed.insert(row.key.clone());
            }
        }
    }

    pub fn expand_all(&mut self, expand: bool, store: &DiffCacheStore) {
        if expand {
            self.collapsed.clear();
        } else if let Some(all) = self.all_expanded_change_rows(store) {
            for r in all.iter() {
                if matches!(r.kind, ChangeRowKind::Dir { .. }) {
                    self.collapsed.insert(r.key.clone());
                }
            }
        }
        let len = self.change_rows(store).len();
        if self.selected >= len {
            self.selected = len.saturating_sub(1);
        }
    }

    /// Expands collapsed ancestor directories of `path` and selects its file row.
    pub fn select_path(&mut self, path: &str, store: &DiffCacheStore) -> bool {
        let Some(bundle) = self.current_bundle(store) else {
            return false;
        };
        let Some(target_idx) = bundle.changes.iter().position(|c| c.path == path) else {
            return false;
        };
        if self.group_by_dir {
            let mut prefix = String::new();
            let mut parts = path.split('/').peekable();
            while let Some(part) = parts.next() {
                if parts.peek().is_none() {
                    break;
                }
                if !prefix.is_empty() {
                    prefix.push('/');
                }
                prefix.push_str(part);
                self.collapsed.remove(&prefix);
            }
            if let Some(all) = self.all_expanded_change_rows(store) {
                for r in all.iter() {
                    if matches!(r.kind, ChangeRowKind::Dir { .. })
                        && path.starts_with(&format!("{}/", r.key))
                    {
                        self.collapsed.remove(&r.key);
                    }
                }
            }
        }
        let rows = self.change_rows(store);
        if let Some(pos) = rows
            .iter()
            .position(|r| matches!(r.kind, ChangeRowKind::File { change_idx } if change_idx == target_idx))
        {
            self.selected = pos;
            true
        } else {
            false
        }
    }
}

/// Shared diff viewer session (Unified/Split projection cache, lazy word-diff
/// cache, context mode, hunk navigation, bounded scroll, and debounced fetch
/// tracking) reused across `[2 Log]`, `[3 Stash]`, and `File History`.
#[derive(Debug)]
pub struct DiffSession {
    pub side_by_side: bool,
    pub context: DiffContext,
    pub zoomed: bool,
    pub scroll: ScrollViewport,
    pub pending: Option<(FileDiffKey, Instant)>,
    pub requested: Option<FileDiffKey>,
    pub rows_cache: FileDiffRowsCache,
}

impl Default for DiffSession {
    fn default() -> Self {
        Self::new(DiffContext::Three)
    }
}

impl DiffSession {
    pub fn new(context: DiffContext) -> Self {
        Self {
            side_by_side: false,
            context,
            zoomed: false,
            scroll: ScrollViewport::new(),
            pending: None,
            requested: None,
            rows_cache: FileDiffRowsCache::default(),
        }
    }

    pub fn rows_for(&self, diff: &Arc<FileDiff>) -> Arc<[DiffRow]> {
        self.rows_cache.rows_for(diff, self.side_by_side)
    }

    pub fn total_rows(&self, diff: &Arc<FileDiff>) -> usize {
        if diff.is_too_large || diff.is_binary || diff.lfs.is_some() || diff.hunks.is_empty() {
            return 0;
        }
        self.rows_for(diff).len() + usize::from(diff.is_truncated)
    }

    pub fn fallback_max_scroll(&self, diff: Option<&Arc<FileDiff>>) -> usize {
        diff.map_or(usize::MAX, |d| self.total_rows(d).saturating_sub(1))
    }

    pub fn hunk_header_offsets(&self, diff: &Arc<FileDiff>) -> Vec<usize> {
        self.rows_for(diff)
            .iter()
            .enumerate()
            .filter_map(|(idx, r)| matches!(r, DiffRow::Header(_)).then_some(idx))
            .collect()
    }

    pub fn jump_hunk(&mut self, delta: isize, diff: Option<&Arc<FileDiff>>) {
        let Some(diff) = diff else {
            return;
        };
        let offsets = self.hunk_header_offsets(diff);
        if offsets.is_empty() {
            return;
        }
        let cur = self.scroll.offset();
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
        self.scroll.set_offset(target);
    }

    pub fn toggle_side_by_side(&mut self) {
        self.side_by_side = !self.side_by_side;
        self.scroll.reset();
    }

    pub fn cycle_context(&mut self) {
        self.context = self.context.next();
        self.scroll.reset();
        self.pending = None;
    }

    pub fn toggle_zoom(&mut self) {
        self.zoomed = !self.zoomed;
    }

    pub fn next_deadline(&self) -> Option<Instant> {
        self.pending.as_ref().map(|(_, at)| *at)
    }

    /// Reconciles a desired diff key against `caches`: clears `pending` on
    /// miss/hit, returns `Some(diff)` if cached and needing syntax highlight, or
    /// schedules a debounced fetch when neither cached nor already requested.
    pub fn reconcile_fetch(
        &mut self,
        desired: Option<FileDiffKey>,
        caches: &mut DiffCacheStore,
        now: Instant,
        debounce: Duration,
    ) -> Option<Arc<FileDiff>> {
        let Some(key) = desired else {
            self.pending = None;
            return None;
        };
        if let Some(cached) = caches.get_diff(&key) {
            self.pending = None;
            if caches.get_syntax(&key).is_none() {
                return Some(cached);
            }
        } else if self.requested.as_ref() != Some(&key)
            && self.pending.as_ref().map(|(k, _)| k) != Some(&key)
        {
            self.pending = Some((key, now + debounce));
        }
        None
    }

    /// Pops a pending diff request whose deadline has arrived and returns it
    /// if not already cached or in flight.
    pub fn flush_due(&mut self, caches: &DiffCacheStore, now: Instant) -> Option<FileDiffKey> {
        let (key, at) = self.pending.take()?;
        if at <= now {
            if !caches.diffs.contains(&key) && self.requested.as_ref() != Some(&key) {
                self.requested = Some(key.clone());
                return Some(key);
            }
        } else {
            self.pending = Some((key, at));
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use jbgit_core::{DiffHunk, DiffLine, DiffLineKind, FileStatusKind};

    use super::*;

    fn sample_diff() -> Arc<FileDiff> {
        let make_line = |k, t: &str| DiffLine {
            kind: k,
            old_lineno: Some(1),
            new_lineno: Some(1),
            text: t.into(),
            word_ranges: Vec::new(),
        };
        Arc::new(FileDiff {
            key: FileDiffKey {
                details: DetailsKey::single(Oid::from_bytes([1; 20])),
                path: "src/lib.rs".into(),
                old_path: None,
                context: DiffContext::Three,
            },
            hunks: vec![
                DiffHunk {
                    header: "@@ -1,2 +1,2 @@".into(),
                    lines: vec![
                        make_line(DiffLineKind::Deleted, "let a = 1;"),
                        make_line(DiffLineKind::Added, "let a = 2;"),
                    ],
                },
                DiffHunk {
                    header: "@@ -20,2 +20,2 @@".into(),
                    lines: vec![
                        make_line(DiffLineKind::Deleted, "let b = 3;"),
                        make_line(DiffLineKind::Added, "let b = 4;"),
                    ],
                },
            ],
            is_binary: false,
            is_truncated: false,
            is_too_large: false,
            lfs: None,
            mode_change: None,
        })
    }

    #[test]
    fn diff_session_hunk_navigation_and_split_toggle() {
        let diff = sample_diff();
        let mut session = DiffSession::new(DiffContext::Three);
        assert_eq!(session.hunk_header_offsets(&diff), vec![0, 3]);
        session.jump_hunk(1, Some(&diff));
        assert_eq!(session.scroll.offset(), 3);
        session.jump_hunk(1, Some(&diff));
        assert_eq!(session.scroll.offset(), 0);
        session.jump_hunk(-1, Some(&diff));
        assert_eq!(session.scroll.offset(), 3);

        session.toggle_side_by_side();
        assert!(session.side_by_side);
        assert_eq!(session.scroll.offset(), 0);
        assert_eq!(session.hunk_header_offsets(&diff), vec![0, 2]);
    }

    #[test]
    fn changes_tree_session_shared_store_and_expand_collapse() {
        let mut store = DiffCacheStore::new();
        let key = DetailsKey::single(Oid::from_bytes([7; 20]));
        store.insert_details(Arc::new(CommitChangesBundle {
            key: key.clone(),
            details: vec![],
            changes: vec![
                FileChange {
                    path: "src/a.rs".into(),
                    old_path: None,
                    kind: FileStatusKind::Modified,
                    additions: 1,
                    deletions: 1,
                    is_binary: false,
                    exists_in_workdir: true,
                    old_oid: Some(Oid::from_bytes([2; 20])),
                    new_oid: Some(Oid::from_bytes([3; 20])),
                },
                FileChange {
                    path: "src/b.rs".into(),
                    old_path: None,
                    kind: FileStatusKind::Added,
                    additions: 5,
                    deletions: 0,
                    is_binary: false,
                    exists_in_workdir: true,
                    old_oid: None,
                    new_oid: Some(Oid::from_bytes([4; 20])),
                },
            ],
        }));

        let mut tree = ChangesTreeSession {
            current_key: Some(key.clone()),
            ..Default::default()
        };
        assert_eq!(tree.expand_state(&store), ChangesExpandState::AllExpanded);
        tree.expand_all(false, &store);
        assert_eq!(tree.expand_state(&store), ChangesExpandState::AllCollapsed);
        assert!(tree.select_path("src/b.rs", &store));
        assert_eq!(tree.selected_change(&store).unwrap().path, "src/b.rs");
        let fk = tree.desired_diff_key(&store, DiffContext::Three).unwrap();
        assert_eq!(
            store.known_oids_for(&fk),
            Some((None, Some(Oid::from_bytes([4; 20]))))
        );
    }
}
