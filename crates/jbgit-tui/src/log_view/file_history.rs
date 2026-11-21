use std::sync::Arc;
use std::time::{Duration, Instant};

use jbgit_core::{
    DetailsKey, DiffContext, FileDiff, FileDiffKey, FileHistoryBundle, FileHistoryEntry,
    FileHistoryKey, LruCache,
};

use super::details_pane::{DiffRow, FileDiffRowsCache};

/// Chord window in `PopupKind::FileHistory` to disambiguate `gg` (jump to top) from `g` (locate in log).
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
    pub diff_side_by_side: bool,
    pub diff_context: DiffContext,
    pub diff_scroll: usize,
    pub diff_zoomed: bool,
    pub focus: FileHistoryFocus,
    pub pending_diff: Option<(FileDiffKey, Instant)>,
    pub requested_diff: Option<FileDiffKey>,
    pub pending_g: Option<Instant>,
    pub(crate) diff_rows_cache: FileDiffRowsCache,
}

impl FileHistoryState {
    pub fn new(key: FileHistoryKey, diff_context: DiffContext) -> Self {
        Self {
            key,
            bundle: None,
            loading: true,
            error: None,
            selected: 0,
            anchor: None,
            compare_with_local: false,
            diff_side_by_side: false,
            diff_context,
            diff_scroll: 0,
            diff_zoomed: false,
            focus: FileHistoryFocus::Commits,
            pending_diff: None,
            requested_diff: None,
            pending_g: None,
            diff_rows_cache: FileDiffRowsCache::default(),
        }
    }

    pub fn diff_rows_for(&self, diff: &Arc<FileDiff>) -> Arc<[DiffRow]> {
        self.diff_rows_cache.rows_for(diff, self.diff_side_by_side)
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
                context: self.diff_context,
            })
        } else if let Some((older, newer)) = self.comparison_pair() {
            Some(FileDiffKey {
                details: DetailsKey::compare_versions(older.oid, newer.oid),
                path: newer.path_at_commit.clone(),
                old_path: (older.path_at_commit != newer.path_at_commit)
                    .then(|| older.path_at_commit.clone()),
                context: self.diff_context,
            })
        } else {
            Some(FileDiffKey {
                details: DetailsKey::single(cur.oid),
                path: cur.path_at_commit.clone(),
                old_path: cur.old_path.clone(),
                context: self.diff_context,
            })
        }
    }

    pub fn next_deadline(&self) -> Option<Instant> {
        [
            self.pending_diff.as_ref().map(|(_, at)| *at),
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
            self.diff_scroll = 0;
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

    pub fn toggle_follow(&mut self) -> FileHistoryKey {
        self.key.follow_renames = !self.key.follow_renames;
        self.loading = true;
        self.error = None;
        self.anchor = None;
        self.diff_scroll = 0;
        self.pending_diff = None;
        self.requested_diff = None;
        self.key.clone()
    }

    pub fn toggle_compare_local(&mut self) {
        self.compare_with_local = !self.compare_with_local;
        self.diff_scroll = 0;
        self.pending_diff = None;
    }

    pub fn toggle_side_by_side(&mut self) {
        self.diff_side_by_side = !self.diff_side_by_side;
        self.diff_scroll = 0;
    }

    pub fn cycle_context(&mut self) {
        self.diff_context = self.diff_context.next();
        self.diff_scroll = 0;
        self.pending_diff = None;
    }

    pub fn toggle_zoom(&mut self) {
        self.diff_zoomed = !self.diff_zoomed;
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

    pub fn fallback_diff_max_scroll(
        &self,
        diff_cache: &LruCache<FileDiffKey, Arc<FileDiff>>,
    ) -> usize {
        self.desired_diff_key()
            .and_then(|k| diff_cache.peek(&k))
            .map_or(usize::MAX, |d| {
                if d.is_too_large || d.is_binary || d.hunks.is_empty() {
                    0
                } else {
                    (self.diff_rows_for(d).len() + usize::from(d.is_truncated)).saturating_sub(1)
                }
            })
    }
}
