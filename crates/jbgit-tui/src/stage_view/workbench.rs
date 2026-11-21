use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use jbgit_core::{
    AmendCommitPreview, DiffContext, DiffLineKind, LineSelection, LruCache, StageNodeKind,
    StageStatusSnapshot, StageTarget, StageTreeOptions, StageTreeRow, StageTreeRowKind,
    StagedRange, build_stage_tree_rows, collect_targets_under_row, next_anchor_after_mutation,
};
use jbgit_engine::{CommitPrecheckInfo, StageDiffKey, StageDiffSide, ThreeDocStageDiff};
use rustc_hash::FxHashSet;

use crate::syntax::{HighlightedDiff, LazyWordDiffCache};

use super::CommitMessageEditor;

const MAX_COMMIT_HISTORY: usize = 20;
pub const STAGE_DIFF_CACHE_CAPACITY: usize = 32;
/// Quiet period after the last commit-message edit before the draft hits disk.
pub const DRAFT_DEBOUNCE: Duration = Duration::from_millis(300);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RollbackUndoEntry {
    pub path: String,
    pub prev_local_bytes: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitBanner {
    pub summary: String,
    pub can_undo: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StageDiffRow {
    Header(usize),
    Unified(usize, usize),
    Split(usize, Option<usize>, Option<usize>),
}

#[derive(Debug, Clone)]
pub struct SyncRowsCacheKey {
    status: Arc<StageStatusSnapshot>,
    amend: bool,
    amend_sha: Option<String>,
    tree_opts: StageTreeOptions,
    collapsed: FxHashSet<String>,
}

impl PartialEq for SyncRowsCacheKey {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.status, &other.status)
            && self.amend == other.amend
            && self.amend_sha == other.amend_sha
            && self.tree_opts == other.tree_opts
            && self.collapsed == other.collapsed
    }
}

impl Eq for SyncRowsCacheKey {}

pub type CachedStageDiffRowsEntry = (Arc<ThreeDocStageDiff>, bool, Arc<[StageDiffRow]>);

/// Serialises draft file writes issued from background threads and the
/// synchronous flush path. Every write carries a monotonically increasing
/// sequence number; a write older than the newest one already on disk is
/// dropped, so a slow background write can never clobber a later flush.
#[derive(Debug, Default)]
pub struct DraftPersistence {
    written_seq: Mutex<u64>,
}

impl DraftPersistence {
    fn write(&self, path: &std::path::Path, content: Option<&str>, seq: u64) {
        let mut written = self.written_seq.lock().unwrap_or_else(|e| e.into_inner());
        if seq <= *written {
            return;
        }
        match content {
            Some(text) => {
                let _ = std::fs::write(path, text);
            }
            None => {
                let _ = std::fs::remove_file(path);
            }
        }
        *written = seq;
    }
}

#[derive(Debug)]
pub struct StageWorkbenchState {
    pub tree_opts: StageTreeOptions,
    pub collapsed: FxHashSet<String>,
    pub multi_selected: FxHashSet<String>,
    pub rows: Vec<StageTreeRow>,

    pub diff_collapsed: bool,
    pub diff_side_by_side: bool,
    pub diff_context: DiffContext,
    pub diff_scroll: usize,
    pub diff_cursor_row: usize,
    pub visual_anchor_row: Option<usize>,
    pub current_diff_key: Option<StageDiffKey>,
    pub requested_diff: Option<StageDiffKey>,
    pub pending_diff: Option<(StageDiffKey, Instant)>,
    pub pending_select_immediate: bool,
    pub current_diff: Option<Arc<ThreeDocStageDiff>>,
    pub diff_syntax: Option<Arc<HighlightedDiff>>,
    pub diff_cache: LruCache<StageDiffKey, Arc<ThreeDocStageDiff>>,
    pub diff_syntax_cache: LruCache<StageDiffKey, Arc<HighlightedDiff>>,
    pub rollback_undo_stack: Vec<RollbackUndoEntry>,
    pub three_way_range_idx: Option<usize>,
    pub bracket_jump_at: Option<Instant>,

    pub editor: CommitMessageEditor,
    pub amend: bool,
    pub amend_awaiting_message: bool,
    pub saved_draft: Option<String>,
    pub amend_preview: Option<AmendCommitPreview>,
    pub signoff: bool,
    pub run_hooks: bool,
    pub show_author_input: bool,
    pub author_input: String,
    pub editing_author: bool,
    pub precheck: CommitPrecheckInfo,
    pub commit_in_progress: bool,
    pub empty_message_error: bool,
    pub commit_history: Vec<String>,
    pub history_path: Option<PathBuf>,
    pub draft_path: Option<PathBuf>,
    /// Editor content changed since the draft file was last written.
    pub draft_dirty: bool,
    /// When the debounced background draft write fires.
    pub draft_save_deadline: Option<Instant>,
    pub(crate) draft_seq: u64,
    pub(crate) draft_io: Arc<DraftPersistence>,
    pub(crate) draft_jobs: Vec<std::thread::JoinHandle<()>>,
    pub last_commit_banner: Option<CommitBanner>,

    pub sync_rows_key: Option<SyncRowsCacheKey>,
    pub diff_rows_cache: RefCell<Option<CachedStageDiffRowsEntry>>,
    pub word_diff_cache: LazyWordDiffCache<ThreeDocStageDiff>,
}

impl Default for StageWorkbenchState {
    fn default() -> Self {
        Self {
            tree_opts: StageTreeOptions::default(),
            collapsed: FxHashSet::default(),
            multi_selected: FxHashSet::default(),
            rows: Vec::new(),
            diff_collapsed: false,
            diff_side_by_side: false,
            diff_context: DiffContext::Three,
            diff_scroll: 0,
            diff_cursor_row: 0,
            visual_anchor_row: None,
            current_diff_key: None,
            requested_diff: None,
            pending_diff: None,
            pending_select_immediate: false,
            current_diff: None,
            diff_syntax: None,
            diff_cache: LruCache::new(STAGE_DIFF_CACHE_CAPACITY),
            diff_syntax_cache: LruCache::new(STAGE_DIFF_CACHE_CAPACITY),
            rollback_undo_stack: Vec::new(),
            three_way_range_idx: None,
            bracket_jump_at: None,
            editor: CommitMessageEditor::default(),
            amend: false,
            amend_awaiting_message: false,
            saved_draft: None,
            amend_preview: None,
            signoff: false,
            run_hooks: true,
            show_author_input: false,
            author_input: String::new(),
            editing_author: false,
            precheck: CommitPrecheckInfo::default(),
            commit_in_progress: false,
            empty_message_error: false,
            commit_history: Vec::new(),
            history_path: None,
            draft_path: None,
            draft_dirty: false,
            draft_save_deadline: None,
            draft_seq: 0,
            draft_io: Arc::default(),
            draft_jobs: Vec::new(),
            last_commit_banner: None,
            sync_rows_key: None,
            diff_rows_cache: RefCell::new(None),
            word_diff_cache: LazyWordDiffCache::default(),
        }
    }
}

impl StageWorkbenchState {
    pub fn next_deadline(&self) -> Option<Instant> {
        [
            self.pending_diff.as_ref().map(|(_, at)| *at),
            self.draft_save_deadline,
        ]
        .into_iter()
        .flatten()
        .min()
    }

    pub fn invalidate_diff_cache_for_path(&mut self, path: &str) {
        self.diff_cache.retain(|k, _| k.path != path);
        self.diff_syntax_cache.retain(|k, _| k.path != path);
        if self.current_diff_key.as_ref().is_some_and(|k| k.path == path) {
            self.current_diff = None;
            self.diff_syntax = None;
            self.requested_diff = None;
        }
    }

    pub fn invalidate_all_diff_caches(&mut self) {
        self.diff_cache.clear();
        self.diff_syntax_cache.clear();
        self.current_diff = None;
        self.diff_syntax = None;
        self.requested_diff = None;
        self.pending_diff = None;
    }

    pub fn sync_rows(&mut self, status: Option<&Arc<StageStatusSnapshot>>) {
        let Some(status) = status else {
            self.rows.clear();
            self.sync_rows_key = None;
            return;
        };
        let amend_sha = if self.amend {
            self.amend_preview.as_ref().map(|p| p.short_sha.as_str())
        } else {
            None
        };
        if let Some(prev) = &self.sync_rows_key
            && Arc::ptr_eq(&prev.status, status)
            && prev.amend == self.amend
            && prev.amend_sha.as_deref() == amend_sha
            && prev.tree_opts == self.tree_opts
            && prev.collapsed == self.collapsed
        {
            return;
        }
        let amend = if self.amend {
            self.amend_preview.as_ref()
        } else {
            None
        };
        self.rows = build_stage_tree_rows(status, amend, self.tree_opts, &self.collapsed);
        let valid: FxHashSet<&str> = self.rows.iter().map(|r| r.key.as_str()).collect();
        self.multi_selected.retain(|k| valid.contains(k.as_str()));
        self.sync_rows_key = Some(SyncRowsCacheKey {
            status: Arc::clone(status),
            amend: self.amend,
            amend_sha: amend_sha.map(ToString::to_string),
            tree_opts: self.tree_opts,
            collapsed: self.collapsed.clone(),
        });
    }

    pub fn load_draft(&mut self, path: PathBuf) {
        if self.editor.is_empty()
            && let Ok(raw) = std::fs::read_to_string(&path)
            && !raw.trim().is_empty()
        {
            self.editor.set_text(&raw);
        }
        self.draft_path = Some(path);
    }

    /// Records a commit-message edit; the draft is written [`DRAFT_DEBOUNCE`]
    /// after the last edit by [`Self::flush_draft_due`] on a background thread.
    pub fn mark_draft_dirty(&mut self, now: Instant) {
        if self.draft_path.is_none() {
            return;
        }
        self.draft_dirty = true;
        self.draft_save_deadline = Some(now + DRAFT_DEBOUNCE);
    }

    fn draft_contents(&self) -> Option<String> {
        let draft = if self.amend {
            self.saved_draft.clone().unwrap_or_default()
        } else {
            self.editor.text()
        };
        (!draft.trim().is_empty()).then_some(draft)
    }

    fn take_draft_write(&mut self) -> Option<(PathBuf, Option<String>, u64)> {
        if !self.draft_dirty {
            return None;
        }
        let path = self.draft_path.clone()?;
        self.draft_dirty = false;
        self.draft_save_deadline = None;
        self.draft_seq += 1;
        Some((path, self.draft_contents(), self.draft_seq))
    }

    /// Fires the debounced draft write once its deadline has passed.
    pub fn flush_draft_due(&mut self, now: Instant) {
        if self.draft_save_deadline.is_none_or(|at| at > now) {
            return;
        }
        let Some((path, contents, seq)) = self.take_draft_write() else {
            self.draft_save_deadline = None;
            return;
        };
        self.draft_jobs.retain(|job| !job.is_finished());
        let io = Arc::clone(&self.draft_io);
        let spawned = std::thread::Builder::new()
            .name("jbgit-draft-writer".into())
            .spawn(move || io.write(&path, contents.as_deref(), seq));
        match spawned {
            Ok(job) => self.draft_jobs.push(job),
            Err(_) => {
                if let Some(path) = &self.draft_path {
                    let contents = self.draft_contents();
                    self.draft_io.write(path, contents.as_deref(), seq);
                }
            }
        }
    }

    /// Writes any pending draft immediately and waits for any in-flight write.
    pub fn flush_draft_sync(&mut self) {
        if let Some((path, contents, seq)) = self.take_draft_write() {
            self.draft_io.write(&path, contents.as_deref(), seq);
        }
        self.wait_draft_writes();
    }

    pub fn wait_draft_writes(&mut self) {
        for job in self.draft_jobs.drain(..) {
            let _ = job.join();
        }
    }

    pub fn load_history(&mut self, path: PathBuf) {
        if let Ok(raw) = std::fs::read_to_string(&path) {
            self.commit_history = raw
                .split("\n---\n")
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .take(MAX_COMMIT_HISTORY)
                .map(ToString::to_string)
                .collect();
        }
        self.history_path = Some(path);
    }

    pub fn push_history(&mut self, message: &str) {
        let trimmed = message.trim().to_string();
        if trimmed.is_empty() {
            return;
        }
        self.commit_history.retain(|m| m != &trimmed);
        self.commit_history.insert(0, trimmed);
        self.commit_history.truncate(MAX_COMMIT_HISTORY);
        if let Some(path) = &self.history_path {
            let serialized = self.commit_history.join("\n---\n");
            let _ = std::fs::write(path, serialized);
        }
    }

    pub fn collect_targets_for_action(
        &self,
        selected_idx: usize,
        status: &StageStatusSnapshot,
        filter_fn: impl Fn(StageNodeKind) -> bool,
    ) -> Vec<StageTarget> {
        if !self.multi_selected.is_empty() {
            let mut targets = Vec::new();
            let mut seen = FxHashSet::default();
            for (idx, row) in self.rows.iter().enumerate() {
                if self.multi_selected.contains(&row.key) && filter_fn(row.group_kind()) {
                    for t in collect_targets_under_row(
                        &self.rows,
                        idx,
                        status,
                        self.tree_opts.untracked_collapse_threshold,
                    ) {
                        if seen.insert((t.group_kind, t.path.clone())) {
                            targets.push(t);
                        }
                    }
                }
            }
            if !targets.is_empty() {
                return targets;
            }
        }
        if let Some(row) = self.rows.get(selected_idx)
            && filter_fn(row.group_kind())
        {
            return collect_targets_under_row(
                &self.rows,
                selected_idx,
                status,
                self.tree_opts.untracked_collapse_threshold,
            );
        }
        Vec::new()
    }

    pub fn desired_diff_key(
        &self,
        selected_idx: usize,
        status: Option<&StageStatusSnapshot>,
    ) -> Option<StageDiffKey> {
        if self.diff_collapsed {
            return None;
        }
        let row = self.rows.get(selected_idx)?;
        match &row.kind {
            StageTreeRowKind::File {
                group_kind,
                path,
                old_path,
                ..
            } => {
                let side = match group_kind {
                    StageNodeKind::Staged => StageDiffSide::Staged,
                    _ => StageDiffSide::Unstaged,
                };
                Some(StageDiffKey {
                    path: path.clone(),
                    old_path: old_path.clone(),
                    side,
                    context: self.diff_context,
                })
            }
            StageTreeRowKind::AmendFile { path, old_path, .. } => Some(StageDiffKey {
                path: path.clone(),
                old_path: old_path.clone(),
                side: StageDiffSide::AmendCommit,
                context: self.diff_context,
            }),
            StageTreeRowKind::Group {
                kind: StageNodeKind::AmendCommit,
                ..
            } => self.rows.iter().find_map(|r| match &r.kind {
                StageTreeRowKind::AmendFile { path, old_path, .. } => Some(StageDiffKey {
                    path: path.clone(),
                    old_path: old_path.clone(),
                    side: StageDiffSide::AmendCommit,
                    context: self.diff_context,
                }),
                _ => None,
            }),
            _ => {
                let st = status?;
                let targets = collect_targets_under_row(
                    &self.rows,
                    selected_idx,
                    st,
                    self.tree_opts.untracked_collapse_threshold,
                );
                let first = targets.first()?;
                let side = match first.group_kind {
                    StageNodeKind::Staged => StageDiffSide::Staged,
                    _ => StageDiffSide::Unstaged,
                };
                Some(StageDiffKey {
                    path: first.path.clone(),
                    old_path: first.old_path.clone(),
                    side,
                    context: self.diff_context,
                })
            }
        }
    }

    pub fn diff_rows(&self) -> Arc<[StageDiffRow]> {
        let Some(diff) = &self.current_diff else {
            return Arc::from([]);
        };
        {
            let guard = self.diff_rows_cache.borrow();
            if let Some((cached_diff, sbs, rows)) = guard.as_ref()
                && Arc::ptr_eq(cached_diff, diff)
                && *sbs == self.diff_side_by_side
            {
                return Arc::clone(rows);
            }
        }
        let mut out = Vec::new();
        for (h, hunk) in diff.hunks.iter().enumerate() {
            out.push(StageDiffRow::Header(h));
            if self.diff_side_by_side {
                out.extend(
                    jbgit_core::side_by_side_rows(&hunk.hunk)
                        .into_iter()
                        .map(|r| StageDiffRow::Split(h, r.left, r.right)),
                );
            } else {
                out.extend((0..hunk.hunk.lines.len()).map(|l| StageDiffRow::Unified(h, l)));
            }
        }
        let rows: Arc<[StageDiffRow]> = out.into();
        *self.diff_rows_cache.borrow_mut() =
            Some((Arc::clone(diff), self.diff_side_by_side, Arc::clone(&rows)));
        rows
    }

    pub fn hunk_header_rows(&self) -> Vec<usize> {
        self.diff_rows()
            .iter()
            .enumerate()
            .filter_map(|(idx, r)| matches!(r, StageDiffRow::Header(_)).then_some(idx))
            .collect()
    }

    pub fn selected_visual_bounds(&self) -> Option<(usize, usize)> {
        let anchor = self.visual_anchor_row?;
        let lo = anchor.min(self.diff_cursor_row);
        let hi = anchor.max(self.diff_cursor_row);
        Some((lo, hi))
    }

    /// Maps either active visual selection `[lo..=hi]` or the hunk under `diff_cursor_row`
    /// into a typed [`LineSelection`].
    pub fn collect_selected_line_sets(&self) -> Option<LineSelection> {
        let diff = self.current_diff.as_ref()?;
        let rows = self.diff_rows();
        if rows.is_empty() {
            return None;
        }
        let is_staged_side = diff.key.side == StageDiffSide::Staged;
        let mut before_set = FxHashSet::default();
        let mut after_set = FxHashSet::default();

        if let Some((lo, hi)) = self.selected_visual_bounds() {
            for r in rows.iter().take(hi + 1).skip(lo) {
                match *r {
                    StageDiffRow::Header(_) => {}
                    StageDiffRow::Unified(h, l) => {
                        Self::collect_line_meta(diff, h, l, &mut before_set, &mut after_set);
                    }
                    StageDiffRow::Split(h, left, right) => {
                        if let Some(l) = left {
                            Self::collect_line_meta(diff, h, l, &mut before_set, &mut after_set);
                        }
                        if let Some(r_idx) = right {
                            Self::collect_line_meta(
                                diff,
                                h,
                                r_idx,
                                &mut before_set,
                                &mut after_set,
                            );
                        }
                    }
                }
            }
            let sel = LineSelection::new(is_staged_side, before_set, after_set);
            return (!sel.is_empty()).then_some(sel);
        }

        let cursor = self.diff_cursor_row.min(rows.len() - 1);
        let hunk_idx = match rows[cursor] {
            StageDiffRow::Header(h)
            | StageDiffRow::Unified(h, _)
            | StageDiffRow::Split(h, _, _) => h,
        };
        let hunk = diff.hunks.get(hunk_idx)?;
        for (l_idx, _) in hunk.hunk.lines.iter().enumerate() {
            Self::collect_line_meta(diff, hunk_idx, l_idx, &mut before_set, &mut after_set);
        }
        let sel = LineSelection::new(is_staged_side, before_set, after_set);
        (!sel.is_empty()).then_some(sel)
    }

    /// Collects all changed line indices for a specific hunk `hunk_idx`.
    pub fn collect_hunk_line_sets(&self, hunk_idx: usize) -> Option<LineSelection> {
        let diff = self.current_diff.as_ref()?;
        let hunk = diff.hunks.get(hunk_idx)?;
        let is_staged_side = diff.key.side == StageDiffSide::Staged;
        let mut before_set = FxHashSet::default();
        let mut after_set = FxHashSet::default();
        for l_idx in 0..hunk.hunk.lines.len() {
            Self::collect_line_meta(diff, hunk_idx, l_idx, &mut before_set, &mut after_set);
        }
        let sel = LineSelection::new(is_staged_side, before_set, after_set);
        (!sel.is_empty()).then_some(sel)
    }

    fn collect_line_meta(
        diff: &ThreeDocStageDiff,
        hunk_idx: usize,
        line_idx: usize,
        before_set: &mut FxHashSet<usize>,
        after_set: &mut FxHashSet<usize>,
    ) {
        let Some(hunk) = diff.hunks.get(hunk_idx) else {
            return;
        };
        let Some(line) = hunk.hunk.lines.get(line_idx) else {
            return;
        };
        let Some(meta) = hunk.line_meta.get(line_idx) else {
            return;
        };
        match line.kind {
            DiffLineKind::Deleted => {
                if let Some(b) = meta.before_idx {
                    before_set.insert(b);
                }
            }
            DiffLineKind::Added => {
                if let Some(a) = meta.after_idx {
                    after_set.insert(a);
                }
            }
            DiffLineKind::Context => {}
        }
    }

    /// Returns the explicit `three_way_range_idx` if active, otherwise the `StagedRange` at cursor.
    pub fn three_way_range_for_popup(&self) -> Option<StagedRange> {
        let diff = self.current_diff.as_ref()?;
        if let Some(idx) = self.three_way_range_idx
            && let Some(sr) = diff.tracker.ranges.get(idx)
        {
            return Some(sr.clone());
        }
        self.three_way_range_at_cursor()
    }

    /// Returns the `StagedRange` at `diff_cursor_row` (or the first mixed range in that hunk).
    pub fn three_way_range_at_cursor(&self) -> Option<StagedRange> {
        let diff = self.current_diff.as_ref()?;
        let rows = self.diff_rows();
        let row = rows.get(self.diff_cursor_row)?;
        let (hunk_idx, line_idx) = match *row {
            StageDiffRow::Header(h) => (h, 0),
            StageDiffRow::Unified(h, l) => (h, l),
            StageDiffRow::Split(h, l, r) => (h, l.or(r).unwrap_or(0)),
        };
        let hunk = diff.hunks.get(hunk_idx)?;
        if let Some(meta) = hunk.line_meta.get(line_idx)
            && let Some(sr_idx) = meta.staged_range_idx
            && let Some(sr) = diff.tracker.ranges.get(sr_idx)
        {
            return Some(sr.clone());
        }
        hunk.line_meta
            .iter()
            .filter_map(|m| m.staged_range_idx)
            .find_map(|idx| diff.tracker.ranges.get(idx).cloned())
    }

    pub fn smooth_anchor_after_rows_update(
        &self,
        old_rows: &[StageTreeRow],
        old_selected: usize,
    ) -> usize {
        if let Some(old_row) = old_rows.get(old_selected)
            && let Some(pos) = self.rows.iter().position(|r| r.key == old_row.key)
        {
            return pos;
        }
        next_anchor_after_mutation(old_rows, old_selected, &self.rows)
    }
}
