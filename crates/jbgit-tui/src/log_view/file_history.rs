use std::sync::Arc;
use std::time::{Duration, Instant};

use jbgit_core::{
    DEFAULT_FILE_HISTORY_LIMIT, DetailsKey, DiffContext, FileChange, FileDiff, FileDiffKey,
    FileHistoryBundle, FileHistoryEntry, FileHistoryKey, HistoryBranchScope, Oid, TreeBlob,
    TreeBlobContent, TreeBlobKey, WhitespaceMode, fuzzy_match, parse_unified_patch_hunks,
};

use crate::app::ScrollViewport;
use crate::diff_session::{DiffCacheStore, DiffRow, DiffSession};
use crate::syntax::{HighlightedDiff, HighlightedFile};

/// Chord window in `ModalLayer::FileHistory` to disambiguate `gg` (jump to top) from `g` (locate in log).
pub const FILE_HISTORY_G_WINDOW: Duration = Duration::from_millis(350);

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum FileHistoryFocus {
    #[default]
    Commits,
    AffectedFiles,
    Diff,
}

#[derive(Debug)]
pub struct FileHistoryState {
    pub key: FileHistoryKey,
    pub bundle: Option<Arc<FileHistoryBundle>>,
    pub loading: bool,
    pub error: Option<String>,
    /// Index into visible (filtered) entries.
    pub selected: usize,
    /// Anchor row (visible index) for two-revision comparison (`Shift+Up`/`Down`,
    /// `Shift+Click`, or `Space` non-adjacent pin).
    pub anchor: Option<usize>,
    /// True when `anchor` was explicitly pinned via `Space` so plain cursor
    /// navigation keeps the anchor active for non-adjacent comparison.
    pub anchor_pinned: bool,
    pub compare_with_local: bool,
    pub focus: FileHistoryFocus,
    pub diff: DiffSession,
    pub pending_g: Option<Instant>,
    pub range_diff: Option<(Oid, Arc<FileDiff>)>,
    pub range_syntax: Option<(Oid, Arc<HighlightedDiff>)>,
    /// Inline fuzzy search over history commits (`/` when Commits pane focused).
    pub search_active: bool,
    pub search_editing: bool,
    pub search_query: String,
    /// Indices into `bundle.entries` matching `search_query`.
    pub filtered_indices: Vec<usize>,
    /// Preserved selected OID across pagination (`Load More`) reloads.
    pub pending_preserve_oid: Option<Oid>,
    /// Toggle full commit message, author/committer metadata strip (`i`).
    pub show_commit_details: bool,
    /// Toggle expandable "All Affected Files" panel for the selected commit (`a`).
    pub show_affected_files: bool,
    pub details_requested: Option<DetailsKey>,
    pub affected_selected: usize,
    pub affected_scroll: ScrollViewport,
    /// Read-only full-file preview at the selected revision (`v` / Open Repository Version).
    pub show_repo_version: bool,
    pub repo_version_blob: Option<Arc<TreeBlob>>,
    pub repo_version_requested: Option<TreeBlobKey>,
    pub repo_version_syntax: Option<Arc<HighlightedFile>>,
    pub repo_version_scroll: ScrollViewport,
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
            anchor_pinned: false,
            compare_with_local: false,
            focus: FileHistoryFocus::Commits,
            diff: DiffSession::from_prefs(diff_context, diff_whitespace, diff_soft_wrap),
            pending_g: None,
            range_diff: None,
            range_syntax: None,
            search_active: false,
            search_editing: false,
            search_query: String::new(),
            filtered_indices: Vec::new(),
            pending_preserve_oid: None,
            show_commit_details: false,
            show_affected_files: false,
            details_requested: None,
            affected_selected: 0,
            affected_scroll: ScrollViewport::new(),
            show_repo_version: false,
            repo_version_blob: None,
            repo_version_requested: None,
            repo_version_syntax: None,
            repo_version_scroll: ScrollViewport::new(),
        }
    }

    pub fn is_selection_history(&self) -> bool {
        self.key.line_range.is_some()
    }

    pub fn uses_range_patch(&self) -> bool {
        self.key.line_range.is_some()
            && !self.compare_with_local
            && !self.show_affected_files
            && self.comparison_pair().is_none()
    }

    pub fn ensure_range_diff(&mut self) -> Option<Arc<FileDiff>> {
        if !self.uses_range_patch() {
            return None;
        }
        let entry = self.selected_entry()?.clone();
        if let Some((oid, diff)) = &self.range_diff
            && *oid == entry.oid
            && diff.key.context == self.diff.context
            && diff.key.whitespace == self.diff.whitespace
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

    /// Raw unloaded/loaded entries in the bundle.
    pub fn entries(&self) -> &[FileHistoryEntry] {
        self.bundle.as_deref().map_or(&[], |b| b.entries.as_slice())
    }

    /// Number of visible entries after inline `/` search filtering.
    pub fn visible_len(&self) -> usize {
        if self.search_query.is_empty() {
            self.entries().len()
        } else {
            self.filtered_indices.len()
        }
    }

    /// Resolves a visible row index into its [`FileHistoryEntry`].
    pub fn visible_entry(&self, vis_idx: usize) -> Option<&FileHistoryEntry> {
        let entries = self.entries();
        if self.search_query.is_empty() {
            entries.get(vis_idx)
        } else {
            let raw_idx = *self.filtered_indices.get(vis_idx)?;
            entries.get(raw_idx)
        }
    }

    /// Resolves a visible row index into its underlying index in `bundle.entries`.
    pub fn raw_index(&self, vis_idx: usize) -> Option<usize> {
        if self.search_query.is_empty() {
            (vis_idx < self.entries().len()).then_some(vis_idx)
        } else {
            self.filtered_indices.get(vis_idx).copied()
        }
    }

    pub fn selected_entry(&self) -> Option<&FileHistoryEntry> {
        self.visible_entry(self.selected)
    }

    fn entry_matches_query(entry: &FileHistoryEntry, query: &str) -> bool {
        let short = entry.oid.short();
        fuzzy_match(query, &entry.summary)
            || fuzzy_match(query, &entry.author_name)
            || fuzzy_match(query, &short)
            || entry
                .oid
                .to_string()
                .starts_with(&query.to_ascii_lowercase())
    }

    /// Recomputes `filtered_indices` from `search_query` while preserving the
    /// currently selected and anchored commits when they remain visible.
    pub fn rebuild_filter(&mut self) {
        let prev_sel_oid = self.selected_entry().map(|e| e.oid);
        let prev_anchor_oid = self
            .anchor
            .and_then(|a| self.visible_entry(a))
            .map(|e| e.oid);

        let entries = self.entries();
        let q = self.search_query.trim();
        if q.is_empty() {
            self.filtered_indices = (0..entries.len()).collect();
        } else {
            self.filtered_indices = entries
                .iter()
                .enumerate()
                .filter_map(|(i, e)| Self::entry_matches_query(e, q).then_some(i))
                .collect();
        }

        let vis_len = self.visible_len();
        if let Some(oid) = prev_sel_oid
            && let Some(pos) = (0..vis_len).find(|&i| self.visible_entry(i).is_some_and(|e| e.oid == oid))
        {
            self.selected = pos;
        } else {
            self.selected = self.selected.min(vis_len.saturating_sub(1));
        }

        self.anchor = prev_anchor_oid.and_then(|oid| {
            (0..vis_len).find(|&i| self.visible_entry(i).is_some_and(|e| e.oid == oid))
        });
        if self.anchor.is_none() {
            self.anchor_pinned = false;
        }
    }

    /// Applies a newly loaded [`FileHistoryBundle`], preserving selection on
    /// pagination reloads and rebuilding the search filter.
    pub fn on_bundle_loaded(&mut self, bundle: Arc<FileHistoryBundle>) {
        self.loading = false;
        self.error = None;
        let preserve = self.pending_preserve_oid.take();
        if preserve.is_none() {
            self.anchor = None;
            self.anchor_pinned = false;
        }
        self.range_diff = None;
        self.range_syntax = None;
        self.diff.reset_view_offsets();
        self.diff.pending = None;
        self.affected_selected = 0;
        self.affected_scroll.reset();
        self.bundle = Some(bundle);

        let q = self.search_query.trim().to_string();
        let entries = self.entries();
        if q.is_empty() {
            self.filtered_indices = (0..entries.len()).collect();
        } else {
            self.filtered_indices = entries
                .iter()
                .enumerate()
                .filter_map(|(i, e)| Self::entry_matches_query(e, &q).then_some(i))
                .collect();
        }

        let target_oid = preserve.or(self.key.start_oid);
        let vis_len = self.visible_len();
        self.selected = target_oid
            .and_then(|oid| {
                (0..vis_len).find(|&i| self.visible_entry(i).is_some_and(|e| e.oid == oid))
            })
            .unwrap_or(0);
    }

    /// When two distinct commits are selected via `anchor`, returns `(older, newer)`.
    pub fn comparison_pair(&self) -> Option<(&FileHistoryEntry, &FileHistoryEntry)> {
        let a = self.anchor?;
        let raw_a = self.raw_index(a)?;
        let raw_s = self.raw_index(self.selected)?;
        if raw_a == raw_s {
            return None;
        }
        let entries = self.entries();
        let newer_idx = raw_s.min(raw_a);
        let older_idx = raw_s.max(raw_a);
        Some((&entries[older_idx], &entries[newer_idx]))
    }

    /// Desired commit details key when either Commit Details or All Affected
    /// Files is enabled.
    pub fn desired_details_key(&self) -> Option<DetailsKey> {
        if !self.show_commit_details && !self.show_affected_files {
            return None;
        }
        let entry = self.selected_entry()?;
        Some(DetailsKey::single(entry.oid))
    }

    /// Currently selected affected file when `show_affected_files` is open.
    pub fn selected_affected_file<'a>(&self, store: &'a DiffCacheStore) -> Option<&'a FileChange> {
        if !self.show_affected_files {
            return None;
        }
        let det_key = self.desired_details_key()?;
        let bundle = store.peek_details(&det_key)?;
        bundle.changes.get(self.affected_selected)
    }

    pub fn desired_diff_key_with_store(&self, store: &DiffCacheStore) -> Option<FileDiffKey> {
        if self.show_affected_files
            && let Some(cur) = self.selected_entry()
            && let Some(ch) = self.selected_affected_file(store)
        {
            return Some(FileDiffKey {
                details: DetailsKey::single(cur.oid),
                path: ch.path.clone(),
                old_path: ch.old_path.clone(),
                context: self.diff.context,
                whitespace: self.diff.whitespace,
            });
        }
        self.desired_diff_key()
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
        if extend {
            if self.anchor.is_none() {
                self.anchor = Some(self.selected);
            }
            self.anchor_pinned = false;
        } else if !self.anchor_pinned {
            self.anchor = None;
        }
        if next != self.selected || !extend {
            let changed = next != self.selected;
            self.selected = next;
            self.diff.scroll.reset();
            if changed {
                self.affected_selected = 0;
                self.affected_scroll.reset();
                self.details_requested = None;
            }
        }
    }

    pub fn has_load_more_row(&self) -> bool {
        !self.loading && self.bundle.as_ref().is_some_and(|b| b.truncated)
    }

    pub fn is_on_load_more_row(&self) -> bool {
        self.has_load_more_row() && self.selected == self.visible_len()
    }

    pub fn move_selection(&mut self, delta: isize, extend: bool) {
        let len = self.visible_len();
        if len == 0 {
            return;
        }
        let max_idx = if self.has_load_more_row() && !extend {
            len
        } else {
            len - 1
        };
        let next = (self.selected as isize + delta).clamp(0, max_idx as isize) as usize;
        self.apply_selection(next, extend);
    }

    pub fn select_row(&mut self, idx: usize, extend: bool) {
        let len = self.visible_len();
        if len == 0 {
            return;
        }
        self.apply_selection(idx.min(len - 1), extend);
    }

    /// Toggles `Space`-pinned non-adjacent comparison anchor on the current row.
    pub fn toggle_mark_anchor(&mut self) {
        if self.visible_len() == 0 || self.is_on_load_more_row() {
            return;
        }
        if self.anchor == Some(self.selected) && self.anchor_pinned {
            self.anchor = None;
            self.anchor_pinned = false;
        } else {
            self.anchor = Some(self.selected);
            self.anchor_pinned = true;
        }
        self.diff.reset_view_offsets();
        self.diff.pending = None;
    }

    pub fn clear_anchor(&mut self) {
        self.anchor = None;
        self.anchor_pinned = false;
        self.diff.reset_view_offsets();
        self.diff.pending = None;
    }

    pub fn toggle_follow(&mut self) -> Option<FileHistoryKey> {
        if self.is_selection_history()
            || matches!(self.key.scope, HistoryBranchScope::AllBranches)
        {
            return None;
        }
        self.key.follow_renames = !self.key.follow_renames;
        self.loading = true;
        self.error = None;
        self.anchor = None;
        self.anchor_pinned = false;
        self.diff.scroll.reset();
        self.diff.pending = None;
        self.diff.requested = None;
        Some(self.key.clone())
    }

    pub fn set_branch_scope(&mut self, scope: HistoryBranchScope) -> Option<FileHistoryKey> {
        if self.is_selection_history() || self.key.scope == scope {
            return None;
        }
        self.key.scope = scope;
        self.key.limit = DEFAULT_FILE_HISTORY_LIMIT;
        self.loading = true;
        self.error = None;
        self.anchor = None;
        self.anchor_pinned = false;
        self.pending_preserve_oid = None;
        self.diff.scroll.reset();
        self.diff.pending = None;
        self.diff.requested = None;
        Some(self.key.clone())
    }

    /// Requests the next page (+500 commits) when `bundle.truncated` is true.
    pub fn load_more(&mut self) -> Option<FileHistoryKey> {
        if self.loading || !self.bundle.as_ref().is_some_and(|b| b.truncated) {
            return None;
        }
        // When triggered from the virtual `[Load More]` row, anchor on the last
        // currently loaded entry so reload stays at the pagination boundary and
        // preserves any `Space`-pinned comparison anchor.
        self.pending_preserve_oid = self
            .selected_entry()
            .or_else(|| self.visible_entry(self.visible_len().saturating_sub(1)))
            .map(|e| e.oid);
        self.key.limit = self
            .key
            .effective_limit()
            .saturating_add(DEFAULT_FILE_HISTORY_LIMIT);
        self.loading = true;
        self.error = None;
        Some(self.key.clone())
    }

    pub fn open_search(&mut self) {
        self.search_active = true;
        self.search_editing = true;
    }

    pub fn search_push_char(&mut self, ch: char) {
        self.search_query.push(ch);
        self.rebuild_filter();
    }

    pub fn search_backspace(&mut self) {
        self.search_query.pop();
        self.rebuild_filter();
    }

    pub fn search_clear(&mut self) {
        self.search_query.clear();
        self.rebuild_filter();
    }

    pub fn search_confirm(&mut self) {
        self.search_editing = false;
        if self.search_query.trim().is_empty() {
            self.search_active = false;
            self.search_query.clear();
            self.rebuild_filter();
        }
    }

    pub fn search_close(&mut self) {
        self.search_active = false;
        self.search_editing = false;
        if !self.search_query.is_empty() {
            self.search_query.clear();
            self.rebuild_filter();
        }
    }

    pub fn toggle_compare_local(&mut self) {
        self.compare_with_local = !self.compare_with_local;
        self.diff.scroll.reset();
        self.diff.pending = None;
    }

    pub fn toggle_commit_details(&mut self) {
        self.show_commit_details = !self.show_commit_details;
        if !self.show_commit_details && !self.show_affected_files {
            self.details_requested = None;
        }
    }

    pub fn toggle_affected_files(&mut self) {
        self.show_affected_files = !self.show_affected_files;
        if self.show_affected_files {
            self.affected_selected = 0;
            self.affected_scroll.reset();
            self.focus = FileHistoryFocus::AffectedFiles;
        } else {
            if self.focus == FileHistoryFocus::AffectedFiles {
                self.focus = FileHistoryFocus::Commits;
            }
            if !self.show_commit_details {
                self.details_requested = None;
            }
        }
        self.diff.scroll.reset();
        self.diff.pending = None;
    }

    pub fn move_affected(&mut self, delta: isize, len: usize) {
        if len == 0 {
            return;
        }
        let next = (self.affected_selected as isize + delta).clamp(0, len as isize - 1) as usize;
        if next != self.affected_selected {
            self.affected_selected = next;
            if let Some(vh) = self.affected_scroll.viewport_height() {
                self.affected_scroll.ensure_visible(next, len, vh);
            }
            self.diff.scroll.reset();
            self.diff.pending = None;
        }
    }

    pub fn select_affected(&mut self, idx: usize, len: usize) {
        if len == 0 {
            return;
        }
        let next = idx.min(len - 1);
        self.affected_selected = next;
        self.focus = FileHistoryFocus::AffectedFiles;
        if let Some(vh) = self.affected_scroll.viewport_height() {
            self.affected_scroll.ensure_visible(next, len, vh);
        }
        self.diff.scroll.reset();
        self.diff.pending = None;
    }

    pub fn repo_version_line_count(&self) -> usize {
        match self.repo_version_blob.as_deref().map(|b| &b.content) {
            Some(TreeBlobContent::Text(t)) => t.lines().count(),
            _ => 0,
        }
    }

    pub fn close_repo_version(&mut self) {
        self.show_repo_version = false;
        self.repo_version_blob = None;
        self.repo_version_requested = None;
        self.repo_version_syntax = None;
        self.repo_version_scroll.reset();
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
        self.current_range_diff().cloned().or_else(|| {
            self.desired_diff_key_with_store(store)
                .and_then(|k| store.peek_diff(&k).cloned())
        })
    }

    pub fn toggle_zoom(&mut self) {
        self.diff.toggle_zoom();
    }

    pub fn jump_diff_hunk(&mut self, delta: isize, store: &DiffCacheStore) {
        let diff = self.current_diff_arc(store);
        self.diff.jump_hunk(delta, diff.as_ref());
    }

    pub fn set_focus(&mut self, focus: FileHistoryFocus) {
        if focus == FileHistoryFocus::AffectedFiles && !self.show_affected_files {
            self.focus = FileHistoryFocus::Commits;
        } else {
            self.focus = focus;
        }
    }

    pub fn toggle_focus(&mut self) {
        self.focus = match self.focus {
            FileHistoryFocus::Commits => {
                if self.show_affected_files {
                    FileHistoryFocus::AffectedFiles
                } else {
                    FileHistoryFocus::Diff
                }
            }
            FileHistoryFocus::AffectedFiles => FileHistoryFocus::Diff,
            FileHistoryFocus::Diff => FileHistoryFocus::Commits,
        };
    }

    pub fn fallback_diff_max_scroll(&self, store: &DiffCacheStore) -> usize {
        let diff = self.current_range_diff().or_else(|| {
            self.desired_diff_key_with_store(store)
                .and_then(|k| store.peek_diff(&k))
        });
        self.diff.fallback_max_scroll(diff)
    }
}
