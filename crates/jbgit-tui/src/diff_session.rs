//! Shared cross-workbench diff cache store, file-changes tree session,
//! diff viewing session, and unified row projection model.
//!
//! Serves `[2 Log]`, `[3 Stash]`, `File History`, and `[1 Commit]` from one
//! deep implementation instead of cloning LRU caches or duplicating tree/diff
//! state machines per workbench.

use std::cell::{Cell, RefCell};
use std::sync::Arc;
use std::time::{Duration, Instant};

use jbgit_core::{
    ChangeRowKind, ChangeTreeRow, CommitChangesBundle, CommitSignature, DetailsKey, DiffContext,
    DiffHunk, FileChange, FileDiff, FileDiffKey, LruCache, Oid, PathFilter, StageDiffHunk,
    WhitespaceMode, build_change_rows_filtered, side_by_side_rows,
};
use rustc_hash::FxHashSet;

use crate::app::ScrollViewport;
use crate::syntax::{HighlightedDiff, LazyWordDiffCache, wrapped_line_row_count};

pub const DETAILS_CACHE_CAPACITY: usize = 256;
pub const DIFF_CACHE_CAPACITY: usize = 64;
pub const SIGNATURES_CACHE_CAPACITY: usize = 1024;
pub const DIFF_HORIZ_SCROLL_STEP: isize = 4;
pub const DIFF_HORIZ_SCROLL_MAX: u16 = 4096;

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
        whitespace: WhitespaceMode,
    ) -> Option<FileDiffKey> {
        let details = self.current_key.clone()?;
        let change = self.selected_change(store)?;
        Some(FileDiffKey {
            details,
            path: change.path.clone(),
            old_path: change.old_path.clone(),
            context,
            whitespace,
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

/// Case-insensitive non-overlapping substring search returning valid UTF-8 byte ranges in `haystack`.
pub fn find_case_insensitive_ranges(haystack: &str, query: &str) -> Vec<(usize, usize)> {
    if query.is_empty() || haystack.is_empty() {
        return Vec::new();
    }
    if haystack.is_ascii() && query.is_ascii() {
        let h = haystack.as_bytes();
        let q: Vec<u8> = query.bytes().map(|b| b.to_ascii_lowercase()).collect();
        let qlen = q.len();
        let mut out = Vec::new();
        let mut i = 0usize;
        while i + qlen <= h.len() {
            if h[i..i + qlen]
                .iter()
                .zip(q.iter())
                .all(|(&a, &b)| a.to_ascii_lowercase() == b)
            {
                out.push((i, i + qlen));
                i += qlen;
            } else {
                i += 1;
            }
        }
        return out;
    }

    let q_chars: Vec<char> = query.chars().flat_map(char::to_lowercase).collect();
    if q_chars.is_empty() {
        return Vec::new();
    }
    let mut lowered: Vec<(char, usize, usize)> = Vec::with_capacity(haystack.len());
    for (start, ch) in haystack.char_indices() {
        let end = start + ch.len_utf8();
        for lc in ch.to_lowercase() {
            lowered.push((lc, start, end));
        }
    }
    let qlen = q_chars.len();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i + qlen <= lowered.len() {
        if lowered[i..i + qlen]
            .iter()
            .zip(q_chars.iter())
            .all(|(&(lc, _, _), &qc)| lc == qc)
        {
            let byte_start = lowered[i].1;
            let byte_end = lowered[i + qlen - 1].2;
            out.push((byte_start, byte_end));
            i += qlen;
        } else {
            i += 1;
        }
    }
    out
}

/// One inline search hit within a projected diff row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiffSearchMatch {
    /// Index into projected [`DiffRow`]s.
    pub row_idx: usize,
    /// `0` for Unified/Header or Split-left; `1` for Split-right.
    pub side: u8,
    /// Valid UTF-8 byte range `(start, end)` within the matched text.
    pub byte_range: (usize, usize),
}

/// Reusable inline `/` diff search state shared across Log, Stage, File History,
/// Stash, Annotate, and Push diff viewers.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DiffSearchState {
    pub active: bool,
    pub editing: bool,
    pub query: String,
    pub matches: Vec<DiffSearchMatch>,
    pub current: usize,
}

/// `(all_match_ranges, active_match_range)` for one projected diff row side.
pub type RowSearchRanges = (Vec<(usize, usize)>, Option<(usize, usize)>);

impl DiffSearchState {
    pub fn open(&mut self) {
        self.active = true;
        self.editing = true;
        self.query.clear();
        self.matches.clear();
        self.current = 0;
    }

    pub fn close(&mut self) {
        self.active = false;
        self.editing = false;
        self.query.clear();
        self.matches.clear();
        self.current = 0;
    }

    pub fn finish_editing(&mut self) {
        if self.query.is_empty() {
            self.close();
        } else {
            self.editing = false;
        }
    }

    pub fn push_char(&mut self, c: char) {
        self.query.push(c);
    }

    pub fn backspace(&mut self) {
        self.query.pop();
    }

    pub fn clear_query(&mut self) {
        self.query.clear();
        self.matches.clear();
        self.current = 0;
    }

    /// Recomputes all case-insensitive matches over `rows`, picking the first match
    /// at or after `anchor_row` (wrapping to 0 if none follow).
    pub fn recompute<'a>(
        &mut self,
        rows: &[DiffRow],
        get_hunk: impl Fn(usize) -> &'a DiffHunk,
        anchor_row: usize,
    ) -> Option<usize> {
        self.matches.clear();
        self.current = 0;
        if self.query.is_empty() {
            return None;
        }
        for (row_idx, row) in rows.iter().enumerate() {
            match *row {
                DiffRow::Header(hh) => {
                    for range in find_case_insensitive_ranges(&get_hunk(hh).header, &self.query) {
                        self.matches.push(DiffSearchMatch {
                            row_idx,
                            side: 0,
                            byte_range: range,
                        });
                    }
                }
                DiffRow::Unified(hh, l) => {
                    let text = &get_hunk(hh).lines[l].text;
                    for range in find_case_insensitive_ranges(text, &self.query) {
                        self.matches.push(DiffSearchMatch {
                            row_idx,
                            side: 0,
                            byte_range: range,
                        });
                    }
                }
                DiffRow::Split(hh, left, right) => {
                    let hunk = get_hunk(hh);
                    if let Some(li) = left {
                        for range in find_case_insensitive_ranges(&hunk.lines[li].text, &self.query)
                        {
                            self.matches.push(DiffSearchMatch {
                                row_idx,
                                side: 0,
                                byte_range: range,
                            });
                        }
                    }
                    if let Some(ri) = right {
                        for range in find_case_insensitive_ranges(&hunk.lines[ri].text, &self.query)
                        {
                            self.matches.push(DiffSearchMatch {
                                row_idx,
                                side: 1,
                                byte_range: range,
                            });
                        }
                    }
                }
            }
        }
        if self.matches.is_empty() {
            return None;
        }
        self.current = self
            .matches
            .iter()
            .position(|m| m.row_idx >= anchor_row)
            .unwrap_or(0);
        Some(self.matches[self.current].row_idx)
    }

    /// Steps to the next (`delta >= 0`) or previous (`delta < 0`) match, wrapping around.
    pub fn step(&mut self, delta: isize) -> Option<usize> {
        if self.matches.is_empty() {
            return None;
        }
        let len = self.matches.len() as isize;
        self.current = (self.current as isize + delta).rem_euclid(len) as usize;
        Some(self.matches[self.current].row_idx)
    }

    pub fn current_match(&self) -> Option<&DiffSearchMatch> {
        self.matches.get(self.current)
    }

    /// Returns `(all_ranges, active_range)` for a projected row and side (`0` or `1`).
    pub fn ranges_for(&self, row_idx: usize, side: u8) -> RowSearchRanges {
        if !self.active || self.matches.is_empty() {
            return (Vec::new(), None);
        }
        let mut ranges = Vec::new();
        let mut active = None;
        for (idx, m) in self.matches.iter().enumerate() {
            if m.row_idx == row_idx && m.side == side {
                ranges.push(m.byte_range);
                if idx == self.current {
                    active = Some(m.byte_range);
                }
            }
        }
        (ranges, active)
    }

    /// Match counter badge (`[3/12]` or `[0/0]`).
    pub fn count_badge(&self) -> String {
        if self.matches.is_empty() {
            "[0/0]".to_string()
        } else {
            format!("[{}/{}]", self.current + 1, self.matches.len())
        }
    }
}

/// Visual row height of one projected [`DiffRow`] under current soft-wrap settings.
pub fn diff_row_visual_height<'a>(
    row: DiffRow,
    get_hunk: impl Fn(usize) -> &'a DiffHunk,
    soft_wrap: bool,
    unified_text_w: usize,
    split_left_w: usize,
    split_right_w: usize,
    tab_size: usize,
) -> usize {
    if !soft_wrap {
        return 1;
    }
    match row {
        DiffRow::Header(_) => 1,
        DiffRow::Unified(hh, l) => {
            wrapped_line_row_count(&get_hunk(hh).lines[l].text, unified_text_w, tab_size)
        }
        DiffRow::Split(hh, left, right) => {
            let hunk = get_hunk(hh);
            let lh = left.map_or(1, |li| {
                wrapped_line_row_count(&hunk.lines[li].text, split_left_w, tab_size)
            });
            let rh = right.map_or(1, |ri| {
                wrapped_line_row_count(&hunk.lines[ri].text, split_right_w, tab_size)
            });
            lh.max(rh).max(1)
        }
    }
}

/// Shared diff viewer session (Unified/Split projection cache, lazy word-diff
/// cache, context & whitespace mode, horizontal scroll, soft wrap, inline search,
/// hunk navigation, bounded scroll, and debounced fetch tracking) reused across
/// `[2 Log]`, `[3 Stash]`, `File History`, `Annotate`, and `Push`.
#[derive(Debug)]
pub struct DiffSession {
    pub side_by_side: bool,
    pub context: DiffContext,
    pub whitespace: WhitespaceMode,
    pub horiz_scroll: u16,
    pub soft_wrap: bool,
    pub search: DiffSearchState,
    pub zoomed: bool,
    pub scroll: ScrollViewport,
    pub pending: Option<(FileDiffKey, Instant)>,
    pub requested: Option<FileDiffKey>,
    pub rows_cache: FileDiffRowsCache,
    last_wrap_widths: Cell<(usize, usize, usize)>,
}

impl Default for DiffSession {
    fn default() -> Self {
        Self::new(DiffContext::Three)
    }
}

impl DiffSession {
    pub fn new(context: DiffContext) -> Self {
        Self::from_prefs(context, WhitespaceMode::Default, false)
    }

    pub fn from_prefs(
        context: DiffContext,
        whitespace: WhitespaceMode,
        soft_wrap: bool,
    ) -> Self {
        Self {
            side_by_side: false,
            context,
            whitespace,
            horiz_scroll: 0,
            soft_wrap,
            search: DiffSearchState::default(),
            zoomed: false,
            scroll: ScrollViewport::new(),
            pending: None,
            requested: None,
            rows_cache: FileDiffRowsCache::default(),
            last_wrap_widths: Cell::new((80, 36, 36)),
        }
    }

    pub fn record_wrap_widths(&self, unified_w: usize, split_left_w: usize, split_right_w: usize) {
        self.last_wrap_widths
            .set((unified_w.max(1), split_left_w.max(1), split_right_w.max(1)));
    }

    pub fn wrap_widths(&self) -> (usize, usize, usize) {
        self.last_wrap_widths.get()
    }

    pub fn rows_for(&self, diff: &Arc<FileDiff>) -> Arc<[DiffRow]> {
        self.rows_cache.rows_for(diff, self.side_by_side)
    }

    pub fn visual_row_offset_of(&self, diff: &Arc<FileDiff>, row_idx: usize, tab_size: usize) -> usize {
        if !self.soft_wrap {
            return row_idx;
        }
        let rows = self.rows_for(diff);
        let (uw, slw, srw) = self.last_wrap_widths.get();
        rows.iter()
            .take(row_idx)
            .map(|&r| diff_row_visual_height(r, |hh| &diff.hunks[hh], true, uw, slw, srw, tab_size))
            .sum()
    }

    pub fn total_rows(&self, diff: &Arc<FileDiff>) -> usize {
        if diff.is_too_large || diff.is_binary || diff.lfs.is_some() || diff.hunks.is_empty() {
            return 0;
        }
        let rows = self.rows_for(diff);
        let content_rows: usize = if self.soft_wrap {
            let (uw, slw, srw) = self.last_wrap_widths.get();
            rows.iter()
                .map(|&r| diff_row_visual_height(r, |hh| &diff.hunks[hh], true, uw, slw, srw, 4))
                .sum()
        } else {
            rows.len()
        };
        content_rows + usize::from(diff.is_truncated)
    }

    pub fn fallback_max_scroll(&self, diff: Option<&Arc<FileDiff>>) -> usize {
        diff.map_or(usize::MAX, |d| self.total_rows(d).saturating_sub(1))
    }

    pub fn hunk_header_offsets(&self, diff: &Arc<FileDiff>) -> Vec<usize> {
        let rows = self.rows_for(diff);
        if !self.soft_wrap {
            return rows
                .iter()
                .enumerate()
                .filter_map(|(idx, r)| matches!(r, DiffRow::Header(_)).then_some(idx))
                .collect();
        }
        let (uw, slw, srw) = self.last_wrap_widths.get();
        let mut vis = 0usize;
        let mut out = Vec::new();
        for &r in rows.iter() {
            if matches!(r, DiffRow::Header(_)) {
                out.push(vis);
            }
            vis += diff_row_visual_height(r, |hh| &diff.hunks[hh], true, uw, slw, srw, 4);
        }
        out
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
        self.horiz_scroll = 0;
        self.scroll.reset();
    }

    pub fn cycle_context(&mut self) {
        self.context = self.context.next();
        self.scroll.reset();
        self.pending = None;
    }

    pub fn cycle_whitespace(&mut self) {
        self.whitespace = self.whitespace.next();
        self.scroll.reset();
        self.pending = None;
    }

    pub fn toggle_soft_wrap(&mut self) {
        self.soft_wrap = !self.soft_wrap;
        if self.soft_wrap {
            self.horiz_scroll = 0;
        }
        self.scroll.reset();
    }

    pub fn scroll_horiz(&mut self, delta: isize) {
        if self.soft_wrap {
            return;
        }
        let next = (self.horiz_scroll as isize + delta).clamp(0, DIFF_HORIZ_SCROLL_MAX as isize);
        self.horiz_scroll = next as u16;
    }

    pub fn reset_view_offsets(&mut self) {
        self.scroll.reset();
        self.horiz_scroll = 0;
        if self.search.active {
            self.search.matches.clear();
            self.search.current = 0;
        }
    }

    /// Recomputes inline search matches against `diff` and centers `scroll` on the active hit.
    pub fn refresh_search(&mut self, diff: Option<&Arc<FileDiff>>, tab_size: usize) {
        let Some(diff) = diff else {
            self.search.matches.clear();
            self.search.current = 0;
            return;
        };
        let rows = self.rows_for(diff);
        let anchor = self.scroll.offset().min(rows.len());
        if let Some(row_idx) = self.search.recompute(&rows, |hh| &diff.hunks[hh], anchor) {
            self.center_on_row(row_idx, diff, tab_size);
        }
    }

    /// Jumps to the next (`delta >= 0`) or previous (`delta < 0`) search hit and centers `scroll`.
    pub fn jump_search(&mut self, delta: isize, diff: Option<&Arc<FileDiff>>, tab_size: usize) {
        let Some(diff) = diff else {
            return;
        };
        if self.search.active && self.search.matches.is_empty() && !self.search.query.is_empty() {
            self.refresh_search(Some(diff), tab_size);
            return;
        }
        if let Some(row_idx) = self.search.step(delta) {
            self.center_on_row(row_idx, diff, tab_size);
        }
    }

    /// Scrolls `self.scroll` so projected `row_idx` sits vertically centered in the viewport.
    pub fn center_on_row(&mut self, row_idx: usize, diff: &Arc<FileDiff>, tab_size: usize) {
        let vis_row = self.visual_row_offset_of(diff, row_idx, tab_size);
        let vh = self.scroll.viewport_height().unwrap_or(20);
        let target = vis_row.saturating_sub(vh / 2);
        self.scroll.set_offset(target);
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
                whitespace: WhitespaceMode::Default,
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
            binary: None,
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
        let fk = tree
            .desired_diff_key(&store, DiffContext::Three, WhitespaceMode::Default)
            .unwrap();
        assert_eq!(
            store.known_oids_for(&fk),
            Some((None, Some(Oid::from_bytes([4; 20]))))
        );
    }

    #[test]
    fn diff_search_finds_case_insensitive_matches_and_centers_scroll() {
        assert_eq!(
            find_case_insensitive_ranges("Alpha alpha ALPHA", "alpha"),
            vec![(0, 5), (6, 11), (12, 17)]
        );
        assert_eq!(
            find_case_insensitive_ranges("你好Foo世界foo", "FOO"),
            vec![(6, 9), (15, 18)]
        );

        let diff = sample_diff();
        let mut session = DiffSession::new(DiffContext::Three);
        session.scroll.bind_bound(2, 5);
        session.search.open();
        session.search.query = "LET".into();
        session.refresh_search(Some(&diff), 4);
        assert_eq!(session.search.matches.len(), 4);
        assert_eq!(session.search.current, 0);

        session.jump_search(1, Some(&diff), 4);
        assert_eq!(session.search.current, 1);
        session.jump_search(-1, Some(&diff), 4);
        assert_eq!(session.search.current, 0);
        session.jump_search(-1, Some(&diff), 4);
        assert_eq!(session.search.current, 3);

        // Verify Split mode also indexes both left (side=0) and right (side=1) matches.
        session.toggle_side_by_side();
        session.refresh_search(Some(&diff), 4);
        assert_eq!(session.search.matches.len(), 4);
        assert!(session.search.matches.iter().any(|m| m.side == 0));
        assert!(session.search.matches.iter().any(|m| m.side == 1));
    }
}
