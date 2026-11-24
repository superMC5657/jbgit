use std::sync::Arc;
use std::time::{Duration, Instant};

use jbgit_core::{
    DetailsKey, DiffContext, FileDiff, FileDiffKey, FileHistoryBundle, FileHistoryEntry,
    FileHistoryKey, Oid, WhitespaceMode, parse_unified_patch_hunks,
};

use crate::diff_session::{DiffCacheStore, DiffRow, DiffSession};
use crate::syntax::HighlightedDiff;

/// Chord window in `ModalLayer::FileHistory` to disambiguate `gg` (jump to top) from `g` (locate in log).
pub const FILE_HISTORY_G_WINDOW: Duration = Duration::from_millis(350);

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum FileHistoryFocus {
    #[default]
    Commits,
    Diff,
}

#[derive(Debug)]
pub struct FileHistoryState {
    pub key: FileHistoryKey,
    pub bundle: Option<Arc<FileHistoryBundle>>,
    pub loading: bool,
    pub error: Option<String>,
    pub selected: usize,
    /// Anchor row for two-revision comparison (`Shift+Up`/`Down` or `Shift+Click`).
    pub anchor: Option<usize>,
    pub compare_with_local: bool,
    pub focus: FileHistoryFocus,
    pub diff: DiffSession,
    pub pending_g: Option<Instant>,
    pub range_diff: Option<(Oid, Arc<FileDiff>)>,
    pub range_syntax: Option<(Oid, Arc<HighlightedDiff>)>,
}

impl FileHistoryState {
    pub fn new(key: FileHistoryKey, diff_context: DiffContext) -> Self {
        Self::with_diff_prefs(key, diff_context, WhitespaceMode::Default, false)
    }

    pub fn with_diff_prefs(
        key: FileHistoryKey,
        diff_context: DiffContext,
        diff_whitespace: WhitespaceMode,
        diff_soft_wrap: bool,
    ) -> Self {
        Self {
            key,
            bundle: None,
            loading: true,
            error: None,
            selected: 0,
            anchor: None,
            compare_with_local: false,
            focus: FileHistoryFocus::Commits,
            diff: DiffSession::from_prefs(diff_context, diff_whitespace, diff_soft_wrap),
            pending_g: None,
            range_diff: None,
            range_syntax: None,
        }
    }

    pub fn is_selection_history(&self) -> bool {
        self.key.line_range.is_some()
    }

    pub fn uses_range_patch(&self) -> bool {
        self.key.line_range.is_some()
            && !self.compare_with_local
            && self.comparison_pair().is_none()
    }

    pub fn ensure_range_diff(&mut self) -> Option<Arc<FileDiff>> {
        if !self.uses_range_patch() {
            return None;
        }
        let entry = self.selected_entry()?.clone();
        if let Some((oid, diff)) = &self.range_diff
            && *oid == entry.oid
        {
            return Some(Arc::clone(diff));
        }
        let hunks = entry
            .range_patch
            .as_deref()
            .map(parse_unified_patch_hunks)
            .unwrap_or_default();
        let diff = Arc::new(FileDiff {
            key: FileDiffKey {
                details: DetailsKey::single(entry.oid),
                path: entry.path_at_commit.clone(),
                old_path: entry.old_path.clone(),
                context: self.diff.context,
                whitespace: self.diff.whitespace,
            },
            hunks,
            is_binary: entry.is_binary,
            binary: None,
            is_truncated: false,
            is_too_large: false,
            lfs: None,
            mode_change: None,
        });
        self.range_diff = Some((entry.oid, Arc::clone(&diff)));
        self.range_syntax = None;
        Some(diff)
    }

    pub fn current_range_diff(&self) -> Option<&Arc<FileDiff>> {
        if !self.uses_range_patch() {
            return None;
        }
        let cur = self.selected_entry()?;
        self.range_diff
            .as_ref()
            .filter(|(oid, _)| *oid == cur.oid)
            .map(|(_, d)| d)
    }

    pub fn current_range_syntax(&self) -> Option<&Arc<HighlightedDiff>> {
        if !self.uses_range_patch() {
            return None;
        }
        let cur = self.selected_entry()?;
        self.range_syntax
            .as_ref()
            .filter(|(oid, _)| *oid == cur.oid)
            .map(|(_, s)| s)
    }

    pub fn diff_rows_for(&self, diff: &Arc<FileDiff>) -> Arc<[DiffRow]> {
        self.diff.rows_for(diff)
    }

    pub fn entries(&self) -> &[FileHistoryEntry] {
        self.bundle.as_deref().map_or(&[], |b| b.entries.as_slice())
    }

    pub fn selected_entry(&self) -> Option<&FileHistoryEntry> {
        self.entries().get(self.selected)
    }

    /// When two distinct commits are selected via `anchor`, returns `(older, newer)`.
    pub fn comparison_pair(&self) -> Option<(&FileHistoryEntry, &FileHistoryEntry)> {
        let entries = self.entries();
        let a = self.anchor?;
        if a == self.selected || a >= entries.len() || self.selected >= entries.len() {
            return None;
        }
        let newer_idx = self.selected.min(a);
        let older_idx = self.selected.max(a);
        Some((&entries[older_idx], &entries[newer_idx]))
    }

    pub fn desired_diff_key(&self) -> Option<FileDiffKey> {
        let cur = self.selected_entry()?;
        if self.compare_with_local {
            Some(FileDiffKey {
                details: DetailsKey::compare_with_local(cur.oid),
                path: self.key.path.clone(),
                old_path: (cur.path_at_commit != self.key.path)
                    .then(|| cur.path_at_commit.clone()),
                context: self.diff.context,
                whitespace: self.diff.whitespace,
            })
        } else if let Some((older, newer)) = self.comparison_pair() {
            Some(FileDiffKey {
                details: DetailsKey::compare_versions(older.oid, newer.oid),
                path: newer.path_at_commit.clone(),
                old_path: (older.path_at_commit != newer.path_at_commit)
                    .then(|| older.path_at_commit.clone()),
                context: self.diff.context,
                whitespace: self.diff.whitespace,
            })
        } else {
            Some(FileDiffKey {
                details: DetailsKey::single(cur.oid),
                path: cur.path_at_commit.clone(),
                old_path: cur.old_path.clone(),
                context: self.diff.context,
                whitespace: self.diff.whitespace,
            })
        }
    }

    pub fn next_deadline(&self) -> Option<Instant> {
        [
            self.diff.next_deadline(),
            self.pending_g.map(|t| t + FILE_HISTORY_G_WINDOW),
        ]
        .into_iter()
        .flatten()
        .min()
    }

    fn apply_selection(&mut self, next: usize, extend: bool) {
        if extend && self.anchor.is_none() {
            self.anchor = Some(self.selected);
        } else if !extend {
            self.anchor = None;
        }
        if next != self.selected || !extend {
            self.selected = next;
            self.diff.scroll.reset();
        }
    }

    pub fn move_selection(&mut self, delta: isize, extend: bool) {
        let len = self.entries().len();
        if len == 0 {
            return;
        }
        let next = (self.selected as isize + delta).clamp(0, len as isize - 1) as usize;
        self.apply_selection(next, extend);
    }

    pub fn select_row(&mut self, idx: usize, extend: bool) {
        let len = self.entries().len();
        if len == 0 {
            return;
        }
        self.apply_selection(idx.min(len - 1), extend);
    }

    pub fn toggle_follow(&mut self) -> Option<FileHistoryKey> {
        if self.is_selection_history() {
            return None;
        }
        self.key.follow_renames = !self.key.follow_renames;
        self.loading = true;
        self.error = None;
        self.anchor = None;
        self.diff.scroll.reset();
        self.diff.pending = None;
        self.diff.requested = None;
        Some(self.key.clone())
    }

    pub fn toggle_compare_local(&mut self) {
        self.compare_with_local = !self.compare_with_local;
        self.diff.scroll.reset();
        self.diff.pending = None;
    }

    pub fn toggle_side_by_side(&mut self) {
        self.diff.toggle_side_by_side();
    }

    pub fn cycle_context(&mut self) {
        self.diff.cycle_context();
    }

    pub fn cycle_whitespace(&mut self) {
        self.diff.cycle_whitespace();
    }

    pub fn toggle_soft_wrap(&mut self) {
        self.diff.toggle_soft_wrap();
    }

    pub fn current_diff_arc(&self, store: &DiffCacheStore) -> Option<Arc<FileDiff>> {
        self.current_range_diff()
            .cloned()
            .or_else(|| self.desired_diff_key().and_then(|k| store.peek_diff(&k).cloned()))
    }

    pub fn toggle_zoom(&mut self) {
        self.diff.toggle_zoom();
    }

    pub fn jump_diff_hunk(&mut self, delta: isize, store: &DiffCacheStore) {
        let diff = self
            .current_range_diff()
            .cloned()
            .or_else(|| self.desired_diff_key().and_then(|k| store.peek_diff(&k).cloned()));
        self.diff.jump_hunk(delta, diff.as_ref());
    }

    pub fn set_focus(&mut self, focus: FileHistoryFocus) {
        self.focus = focus;
    }

    pub fn toggle_focus(&mut self) {
        self.focus = match self.focus {
            FileHistoryFocus::Commits => FileHistoryFocus::Diff,
            FileHistoryFocus::Diff => FileHistoryFocus::Commits,
        };
    }

    pub fn fallback_diff_max_scroll(&self, store: &DiffCacheStore) -> usize {
        let diff = self
            .current_range_diff()
            .or_else(|| self.desired_diff_key().and_then(|k| store.peek_diff(&k)));
        self.diff.fallback_max_scroll(diff)
    }
}
