use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use jbgit_core::{
    AmendCommitPreview, DiffContext, DiffLineKind, GitOp, LineSelection, LruCache, PaneId,
    RefreshScope, RepoSnapshot, RequestSlot, RequestSlotId, StageNodeKind, StageStatusSnapshot,
    StageTarget, StageTreeOptions, StageTreeRow, StageTreeRowKind, StagedRange,
    build_stage_tree_rows, collect_targets_under_row, next_anchor_after_mutation,
};
use jbgit_engine::{
    CommitPrecheckInfo, CommitRequest, EngineEvent, StageDiffKey, StageDiffSide, ThreeDocStageDiff,
};
use rustc_hash::FxHashSet;

use crate::app::{
    Action, DiffScope, EditorMoveKind, Effect, ModalLayer, ScrollViewport, SyntaxJob, SyntaxJobKey,
    SyntaxTarget, apply_scroll_delta,
};
use crate::dialogs::{ChoiceDialog, Dialog};
use crate::diff_session::{DiffRow, build_stage_diff_rows};
use crate::hit_test::DiffHunkActionKind;
use crate::i18n::I18n;
use crate::syntax::{HighlightedDiff, LazyWordDiffCache};

use super::CommitMessageEditor;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StageDiffOp {
    Stage,
    Unstage,
    Rollback,
}

/// Narrow coordination context passed by [`crate::app::AppState`] into
/// [`CommitWorkbench::dispatch`] and [`CommitWorkbench::apply_engine_event`].
pub struct CommitContext<'a> {
    pub snap: Option<&'a RepoSnapshot>,
    pub default_remote: &'a str,
    pub i18n: &'a I18n,
    pub now: Instant,
    pub focused_pane: &'a mut PaneId,
    pub loading_snapshot: &'a mut bool,
    pub status_notice: &'a mut Option<String>,
    pub effects: &'a mut Vec<Effect>,
    pub modals: &'a mut Vec<ModalLayer>,
}

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

pub type CachedStageDiffRowsEntry = (Arc<ThreeDocStageDiff>, bool, Arc<[DiffRow]>);

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
pub struct CommitWorkbench {
    tree_opts: StageTreeOptions,
    collapsed: FxHashSet<String>,
    multi_selected: FxHashSet<String>,
    status: Option<Arc<StageStatusSnapshot>>,
    rows: Vec<StageTreeRow>,
    selected_idx: usize,

    diff_collapsed: bool,
    diff_side_by_side: bool,
    diff_context: DiffContext,
    diff_scroll: ScrollViewport,
    diff_cursor_row: usize,
    visual_anchor_row: Option<usize>,
    current_diff_key: Option<StageDiffKey>,
    requested_diff: Option<StageDiffKey>,
    pending_diff: Option<(StageDiffKey, Instant)>,
    pending_select_immediate: bool,
    current_diff: Option<Arc<ThreeDocStageDiff>>,
    diff_syntax: Option<Arc<HighlightedDiff>>,
    diff_cache: LruCache<StageDiffKey, Arc<ThreeDocStageDiff>>,
    diff_syntax_cache: LruCache<StageDiffKey, Arc<HighlightedDiff>>,
    rollback_undo_stack: Vec<RollbackUndoEntry>,
    bracket_jump_at: Option<Instant>,

    editor: CommitMessageEditor,
    amend: bool,
    amend_awaiting_message: bool,
    saved_draft: Option<String>,
    amend_preview: Option<AmendCommitPreview>,
    signoff: bool,
    run_hooks: bool,
    show_author_input: bool,
    author_input: String,
    editing_author: bool,
    precheck: CommitPrecheckInfo,
    commit_in_progress: bool,
    empty_message_error: bool,
    commit_history: Vec<String>,
    history_path: Option<PathBuf>,
    draft_path: Option<PathBuf>,
    draft_dirty: bool,
    draft_save_deadline: Option<Instant>,
    draft_seq: u64,
    draft_io: Arc<DraftPersistence>,
    draft_jobs: Vec<std::thread::JoinHandle<()>>,
    last_commit_banner: Option<CommitBanner>,

    sync_rows_key: Option<SyncRowsCacheKey>,
    diff_rows_cache: RefCell<Option<CachedStageDiffRowsEntry>>,
    word_diff_cache: LazyWordDiffCache<ThreeDocStageDiff>,
}

impl Default for CommitWorkbench {
    fn default() -> Self {
        Self {
            tree_opts: StageTreeOptions::default(),
            collapsed: FxHashSet::default(),
            multi_selected: FxHashSet::default(),
            status: None,
            rows: Vec::new(),
            selected_idx: 0,
            diff_collapsed: false,
            diff_side_by_side: false,
            diff_context: DiffContext::Three,
            diff_scroll: ScrollViewport::new(),
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

impl CommitWorkbench {
    pub fn with_diff_context(diff_context: DiffContext) -> Self {
        Self {
            diff_context,
            ..Self::default()
        }
    }

    pub fn tree_opts(&self) -> StageTreeOptions {
        self.tree_opts
    }

    pub fn multi_selected(&self) -> &FxHashSet<String> {
        &self.multi_selected
    }

    pub fn diff_collapsed(&self) -> bool {
        self.diff_collapsed
    }

    pub fn diff_side_by_side(&self) -> bool {
        self.diff_side_by_side
    }

    pub fn diff_context(&self) -> DiffContext {
        self.diff_context
    }

    pub fn diff_scroll(&self) -> &ScrollViewport {
        &self.diff_scroll
    }

    pub fn diff_cursor_row(&self) -> usize {
        self.diff_cursor_row
    }

    pub fn visual_anchor_row(&self) -> Option<usize> {
        self.visual_anchor_row
    }

    pub fn current_diff_key(&self) -> Option<&StageDiffKey> {
        self.current_diff_key.as_ref()
    }

    pub fn current_diff(&self) -> Option<&Arc<ThreeDocStageDiff>> {
        self.current_diff.as_ref()
    }

    pub fn diff_syntax(&self) -> Option<&Arc<HighlightedDiff>> {
        self.diff_syntax.as_ref()
    }

    pub fn word_diff_cache(&self) -> &LazyWordDiffCache<ThreeDocStageDiff> {
        &self.word_diff_cache
    }

    pub fn bracket_jump_at(&self) -> Option<Instant> {
        self.bracket_jump_at
    }

    pub fn editor(&self) -> &CommitMessageEditor {
        &self.editor
    }

    pub fn amend(&self) -> bool {
        self.amend
    }

    pub fn amend_preview(&self) -> Option<&AmendCommitPreview> {
        self.amend_preview.as_ref()
    }

    pub fn signoff(&self) -> bool {
        self.signoff
    }

    pub fn run_hooks(&self) -> bool {
        self.run_hooks
    }

    pub fn show_author_input(&self) -> bool {
        self.show_author_input
    }

    pub fn author_input(&self) -> &str {
        &self.author_input
    }

    pub fn editing_author(&self) -> bool {
        self.editing_author
    }

    pub fn precheck(&self) -> &CommitPrecheckInfo {
        &self.precheck
    }

    pub fn commit_in_progress(&self) -> bool {
        self.commit_in_progress
    }

    pub fn set_commit_in_progress(&mut self, in_progress: bool) {
        self.commit_in_progress = in_progress;
    }

    pub fn empty_message_error(&self) -> bool {
        self.empty_message_error
    }

    pub fn last_commit_banner(&self) -> Option<&CommitBanner> {
        self.last_commit_banner.as_ref()
    }

    #[cfg(test)]
    pub(crate) fn set_diff_key_for_test(&mut self, key: Option<StageDiffKey>) {
        self.current_diff_key = key;
    }

    #[cfg(test)]
    pub(crate) fn set_diff_for_test(&mut self, diff: Option<Arc<ThreeDocStageDiff>>) {
        self.current_diff = diff;
    }

    #[cfg(test)]
    pub(crate) fn set_side_by_side_for_test(&mut self, sbs: bool) {
        self.diff_side_by_side = sbs;
    }

    #[cfg(test)]
    pub(crate) fn draft_save_deadline_for_test(&self) -> Option<Instant> {
        self.draft_save_deadline
    }

    #[cfg(test)]
    pub(crate) fn has_pending_diff_for_test(&self) -> bool {
        self.pending_diff.is_some()
    }

    #[cfg(test)]
    pub(crate) fn set_diff_cursor_for_test(&mut self, row: usize) {
        self.diff_cursor_row = row;
    }

    #[cfg(test)]
    pub(crate) fn draft_dirty(&self) -> bool {
        self.draft_dirty
    }

    #[cfg(test)]
    pub(crate) fn diff_cache_contains(&self, key: &StageDiffKey) -> bool {
        self.diff_cache.contains(key)
    }

    #[cfg(test)]
    pub(crate) fn editor_mut_for_test(&mut self) -> &mut CommitMessageEditor {
        &mut self.editor
    }

    pub fn status(&self) -> Option<&Arc<StageStatusSnapshot>> {
        self.status.as_ref()
    }

    pub fn rows(&self) -> &[StageTreeRow] {
        &self.rows
    }

    pub fn selected_idx(&self) -> usize {
        self.selected_idx
    }

    pub fn selected_row(&self) -> Option<&StageTreeRow> {
        self.rows.get(self.selected_idx)
    }

    pub fn entry_at(&self, idx: usize) -> Option<jbgit_core::StageEntry> {
        let row = self.rows.get(idx)?;
        match &row.kind {
            StageTreeRowKind::File {
                path,
                old_path,
                status_kind,
                ..
            }
            | StageTreeRowKind::AmendFile {
                path,
                old_path,
                status_kind,
            } => Some(jbgit_core::StageEntry {
                path: path.clone(),
                old_path: old_path.clone(),
                kind: *status_kind,
            }),
            _ => None,
        }
    }

    pub fn selected_entry(&self) -> Option<jbgit_core::StageEntry> {
        self.entry_at(self.selected_idx)
    }

    pub fn select_row(&mut self, idx: usize) {
        if self.rows.is_empty() {
            self.selected_idx = 0;
        } else {
            self.selected_idx = idx.min(self.rows.len() - 1);
        }
    }

    pub fn move_selection(&mut self, delta: isize) {
        if self.rows.is_empty() {
            self.selected_idx = 0;
        } else {
            self.selected_idx =
                (self.selected_idx as isize + delta).clamp(0, self.rows.len() as isize - 1)
                    as usize;
        }
    }

    pub fn clamp_selection(&mut self) {
        if self.rows.is_empty() {
            self.selected_idx = 0;
        } else if self.selected_idx >= self.rows.len() {
            self.selected_idx = self.rows.len() - 1;
        }
    }

    /// Replaces the stage status snapshot, re-syncing `rows` and smoothly
    /// anchoring `selected_idx` automatically. Returns `true` when content changed.
    pub fn apply_status(&mut self, status: Arc<StageStatusSnapshot>) -> bool {
        let changed = self.status.as_ref() != Some(&status);
        let had_status = self.status.is_some();
        let old_rows = self.rows.clone();
        let old_sel = self.selected_idx;
        self.status = Some(status);
        if changed || had_status {
            self.invalidate_all_diff_caches();
            if changed {
                self.current_diff_key = None;
            }
            self.pending_select_immediate = true;
        }
        self.rebuild_rows_internal();
        if !old_rows.is_empty() {
            self.selected_idx = self.smooth_anchor_after_rows_update(&old_rows, old_sel);
        } else {
            self.selected_idx = 0;
        }
        changed
    }

    /// Directly installs a status snapshot (used by tests & state resets) while
    /// keeping `rows` and `selected_idx` strictly synchronized.
    #[cfg(test)]
    pub fn set_status(&mut self, status: Option<Arc<StageStatusSnapshot>>) {
        self.status = status;
        self.rebuild_rows_internal();
        self.clamp_selection();
    }

    pub fn toggle_group_by_dir(&mut self) {
        self.tree_opts.group_by_dir = !self.tree_opts.group_by_dir;
        self.rebuild_rows_internal();
        self.selected_idx = 0;
    }

    pub fn toggle_show_ignored(&mut self) {
        self.tree_opts.show_ignored = !self.tree_opts.show_ignored;
        self.rebuild_rows_internal();
        self.clamp_selection();
    }

    pub fn toggle_row(&mut self, idx: usize) {
        if let Some(row) = self.rows.get(idx).cloned() {
            if matches!(
                row.kind,
                StageTreeRowKind::Group {
                    kind: StageNodeKind::Untracked,
                    ..
                }
            ) {
                self.tree_opts.untracked_many_expanded = !self.tree_opts.untracked_many_expanded;
                self.rebuild_rows_internal();
                self.clamp_selection();
            } else if row.is_container() {
                if row.expanded {
                    self.collapsed.insert(row.key);
                } else {
                    self.collapsed.remove(&row.key);
                }
                self.rebuild_rows_internal();
                self.clamp_selection();
            }
        }
    }

    pub fn expand_selected(&mut self, expand: bool) {
        let Some(row) = self.rows.get(self.selected_idx).cloned() else {
            return;
        };
        if matches!(
            row.kind,
            StageTreeRowKind::Group {
                kind: StageNodeKind::Untracked,
                ..
            }
        ) {
            self.tree_opts.untracked_many_expanded = expand;
            self.rebuild_rows_internal();
            self.clamp_selection();
        } else if row.is_container() {
            if row.expanded != expand {
                if expand {
                    self.collapsed.remove(&row.key);
                } else {
                    self.collapsed.insert(row.key);
                }
                self.rebuild_rows_internal();
                self.clamp_selection();
            } else if expand && self.selected_idx + 1 < self.rows.len() {
                self.selected_idx += 1;
            } else if !expand && row.depth > 0 {
                self.jump_to_parent(row.depth - 1);
            }
        } else if !expand && row.depth > 0 {
            self.jump_to_parent(row.depth - 1);
        }
    }

    fn jump_to_parent(&mut self, target_depth: u16) {
        if let Some(parent_idx) = self.rows[..self.selected_idx]
            .iter()
            .rposition(|r| r.depth <= target_depth && r.is_container())
        {
            self.selected_idx = parent_idx;
        }
    }

    pub fn expand_all(&mut self, expand: bool) {
        if expand {
            self.collapsed.clear();
            self.tree_opts.untracked_many_expanded = true;
        } else {
            self.tree_opts.untracked_many_expanded = false;
            self.rebuild_rows_internal();
            let keys: Vec<String> = self
                .rows
                .iter()
                .filter(|r| matches!(r.kind, StageTreeRowKind::Dir { .. }))
                .map(|r| r.key.clone())
                .collect();
            self.collapsed.extend(keys);
        }
        self.rebuild_rows_internal();
        self.clamp_selection();
    }

    pub fn toggle_multi_select(&mut self, idx: usize) {
        if let Some(row) = self.rows.get(idx) {
            let key = row.key.clone();
            if self.multi_selected.is_empty()
                && let Some(cur) = self.rows.get(self.selected_idx)
                && cur.key != key
            {
                self.multi_selected.insert(cur.key.clone());
            }
            if !self.multi_selected.remove(&key) {
                self.multi_selected.insert(key);
            }
            self.selected_idx = idx;
        }
    }

    pub fn select_all_files(&mut self) {
        self.multi_selected = self
            .rows
            .iter()
            .filter(|r| r.is_file())
            .map(|r| r.key.clone())
            .collect();
    }

    pub fn set_amend_flag(&mut self, amend: bool) {
        self.amend = amend;
        self.rebuild_rows_internal();
        self.clamp_selection();
    }

    pub fn set_amend_preview(&mut self, preview: Option<AmendCommitPreview>) {
        self.amend_preview = preview;
        self.rebuild_rows_internal();
        self.clamp_selection();
    }

    /// Cleans up inline editing & visual-selection state when focus leaves the Commit tab/pane.
    pub fn on_blur(&mut self) {
        self.editing_author = false;
        self.visual_anchor_row = None;
    }

    /// Cleans up inline text inputs without clearing diff visual line selection.
    pub fn blur_text_inputs(&mut self) {
        self.editing_author = false;
    }

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

    fn rebuild_rows_internal(&mut self) {
        let Some(status) = self.status.as_ref() else {
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

    pub fn diff_rows(&self) -> Arc<[DiffRow]> {
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
        let rows: Arc<[DiffRow]> =
            build_stage_diff_rows(&diff.hunks, self.diff_side_by_side).into();
        *self.diff_rows_cache.borrow_mut() =
            Some((Arc::clone(diff), self.diff_side_by_side, Arc::clone(&rows)));
        rows
    }

    pub fn hunk_header_rows(&self) -> Vec<usize> {
        self.diff_rows()
            .iter()
            .enumerate()
            .filter_map(|(idx, r)| matches!(r, DiffRow::Header(_)).then_some(idx))
            .collect()
    }

    pub fn selected_visual_bounds(&self) -> Option<(usize, usize)> {
        let anchor = self.visual_anchor_row?;
        let lo = anchor.min(self.diff_cursor_row);
        let hi = anchor.max(self.diff_cursor_row);
        Some((lo, hi))
    }

    /// Returns the 1-based inclusive line range for the active visual selection
    /// or the line/hunk under `diff_cursor_row` (for `Show History for Selection`).
    pub fn selected_line_range(&self) -> Option<(u32, u32)> {
        let diff = self.current_diff.as_ref()?;
        if !diff.vcs_exists || diff.is_binary || diff.is_too_large {
            return None;
        }
        let rows = self.diff_rows();
        if rows.is_empty() {
            return None;
        }

        let mut min_new: Option<u32> = None;
        let mut max_new: Option<u32> = None;
        let mut min_old: Option<u32> = None;
        let mut max_old: Option<u32> = None;
        let mut record_line = |h: usize, l: usize| {
            let Some(line) = diff.hunks.get(h).and_then(|hk| hk.hunk.lines.get(l)) else {
                return;
            };
            if let Some(n) = line.new_lineno {
                min_new = Some(min_new.map_or(n, |cur| cur.min(n)));
                max_new = Some(max_new.map_or(n, |cur| cur.max(n)));
            }
            if let Some(o) = line.old_lineno {
                min_old = Some(min_old.map_or(o, |cur| cur.min(o)));
                max_old = Some(max_old.map_or(o, |cur| cur.max(o)));
            }
        };

        let mut inspect_row = |row: DiffRow| match row {
            DiffRow::Header(h) => {
                if let Some(hk) = diff.hunks.get(h) {
                    for l in 0..hk.hunk.lines.len() {
                        record_line(h, l);
                    }
                }
            }
            DiffRow::Unified(h, l) => record_line(h, l),
            DiffRow::Split(h, left, right) => {
                if let Some(l) = left {
                    record_line(h, l);
                }
                if let Some(r) = right {
                    record_line(h, r);
                }
            }
        };

        if let Some((lo, hi)) = self.selected_visual_bounds() {
            let hi = hi.min(rows.len() - 1);
            for &r in rows.iter().take(hi + 1).skip(lo.min(hi)) {
                inspect_row(r);
            }
        } else {
            let cursor = self.diff_cursor_row.min(rows.len() - 1);
            inspect_row(rows[cursor]);
        }

        min_new.zip(max_new).or_else(|| min_old.zip(max_old))
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
                    DiffRow::Header(_) => {}
                    DiffRow::Unified(h, l) => {
                        Self::collect_line_meta(diff, h, l, &mut before_set, &mut after_set);
                    }
                    DiffRow::Split(h, left, right) => {
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
            DiffRow::Header(h)
            | DiffRow::Unified(h, _)
            | DiffRow::Split(h, _, _) => h,
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

    /// Returns the explicit `range_idx` if active, otherwise the `StagedRange` at cursor.
    pub fn three_way_range_for_popup(&self, range_idx: Option<usize>) -> Option<StagedRange> {
        let diff = self.current_diff.as_ref()?;
        if let Some(idx) = range_idx
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
            DiffRow::Header(h) => (h, 0),
            DiffRow::Unified(h, l) => (h, l),
            DiffRow::Split(h, l, r) => (h, l.or(r).unwrap_or(0)),
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

    pub fn scroll_diff(&mut self, delta: isize) {
        let fallback = self.diff_rows().len().saturating_sub(1);
        self.diff_scroll.scroll_by(delta, fallback);
    }

    pub fn bind_diff_viewport(&mut self, viewport_h: usize, max_scroll: usize) {
        self.diff_scroll.bind_bound(viewport_h, max_scroll);
    }

    pub fn bind_editor_viewport(&mut self, inner_w: usize, inner_h: usize) {
        self.editor.bind_viewport(inner_w, inner_h);
    }

    pub fn select_diff_line(&mut self, row_idx: usize, extend: bool) {
        if extend {
            if self.visual_anchor_row.is_none() {
                self.visual_anchor_row = Some(self.diff_cursor_row);
            }
        } else {
            self.visual_anchor_row = None;
        }
        self.diff_cursor_row = row_idx;
    }

    pub fn clear_visual_anchor(&mut self) {
        self.visual_anchor_row = None;
    }

    pub fn clear_bracket_jump(&mut self) {
        self.bracket_jump_at = None;
    }

    pub fn focus_author_done(&mut self) {
        self.editing_author = false;
    }

    pub fn begin_commit_confirmed(&mut self) {
        self.commit_in_progress = true;
        self.empty_message_error = false;
    }

    pub fn invalidate_paths(&mut self, targets: &[StageTarget]) {
        for t in targets {
            self.invalidate_diff_cache_for_path(&t.path);
            if let Some(old) = &t.old_path {
                self.invalidate_diff_cache_for_path(old);
            }
        }
        self.pending_select_immediate = true;
    }

    pub fn on_git_op_succeeded(&mut self) {
        self.invalidate_all_diff_caches();
        self.pending_select_immediate = true;
    }

    pub fn on_engine_error(&mut self) {
        self.requested_diff = None;
        self.commit_in_progress = false;
    }

    pub fn on_repo_snapshot_loaded(
        &mut self,
        snapshot: &RepoSnapshot,
        head_changed: bool,
        effects: &mut Vec<Effect>,
    ) {
        if self.history_path.is_none() && !snapshot.common_dir.as_os_str().is_empty() {
            self.load_history(snapshot.common_dir.join("jbgit-commit-history"));
            // Per working tree (`git_dir` == `common_dir` for the main one):
            // linked worktrees each keep their own draft.
            let draft_dir = if snapshot.git_dir.as_os_str().is_empty() {
                &snapshot.common_dir
            } else {
                &snapshot.git_dir
            };
            self.load_draft(draft_dir.join("jbgit-commit-draft"));
        }
        if head_changed && !self.amend {
            self.amend_preview = None;
        }
        if self.amend {
            effects.push(Effect::RequestAmendPreview);
        }
    }

    pub fn apply_external_message(&mut self, updated: &str, now: Instant) {
        let prev = self.editor.text();
        if !self.amend && !prev.trim().is_empty() && prev.trim() != updated.trim() {
            self.push_history(&prev);
        }
        self.editor.set_text(updated);
        self.empty_message_error = false;
        self.mark_draft_dirty(now);
    }

    pub fn apply_syntax_highlighted(
        &mut self,
        key: StageDiffKey,
        epoch: u64,
        highlighted: Arc<HighlightedDiff>,
        stage_syntax: &mut RequestSlot<StageDiffKey>,
    ) -> bool {
        if !stage_syntax.finish_cancelable(&key, epoch) {
            return false;
        }
        if self.diff_cache.contains(&key) {
            self.diff_syntax_cache
                .insert(key.clone(), Arc::clone(&highlighted));
        }
        if self.current_diff_key.as_ref() == Some(&key) {
            self.diff_syntax = Some(highlighted);
            true
        } else {
            false
        }
    }

    fn schedule_syntax(
        diff: Arc<ThreeDocStageDiff>,
        stage_syntax: &mut RequestSlot<StageDiffKey>,
        syntax_enabled: bool,
        effects: &mut Vec<Effect>,
    ) {
        if !syntax_enabled
            || diff.is_binary
            || diff.is_too_large
            || diff.submodule.is_some()
            || diff.lfs.is_some()
            || diff.hunks.is_empty()
        {
            return;
        }
        if let Some((epoch, cancel)) = stage_syntax.begin_cancelable(diff.key.clone()) {
            effects.push(Effect::RequestSyntaxHighlight(SyntaxJob {
                key: SyntaxJobKey::Stage(diff.key.clone()),
                epoch,
                target: SyntaxTarget::Stage(diff),
                cancel,
            }));
        }
    }

    pub fn reconcile_requests(
        &mut self,
        active: bool,
        now: Instant,
        stage_syntax: &mut RequestSlot<StageDiffKey>,
        syntax_enabled: bool,
        effects: &mut Vec<Effect>,
    ) {
        self.clamp_selection();

        if !active || self.diff_collapsed {
            self.pending_diff = None;
            return;
        }
        let immediate = std::mem::take(&mut self.pending_select_immediate);
        let desired = self.desired_diff_key(self.selected_idx, self.status.as_deref());

        if desired != self.current_diff_key {
            stage_syntax.invalidate();
            self.current_diff_key = desired.clone();
            self.diff_scroll.reset();
            self.diff_cursor_row = 0;
            self.visual_anchor_row = None;

            if let Some(k) = desired {
                if let Some(cached) = self.diff_cache.get(&k).cloned() {
                    self.pending_diff = None;
                    self.requested_diff = None;
                    self.diff_syntax = self.diff_syntax_cache.get(&k).cloned();
                    self.current_diff = Some(Arc::clone(&cached));
                    let len = self.diff_rows().len();
                    if len > 0 {
                        self.diff_cursor_row = self.diff_cursor_row.min(len - 1);
                    }
                    if self.diff_syntax.is_none() {
                        Self::schedule_syntax(cached, stage_syntax, syntax_enabled, effects);
                    }
                } else {
                    self.current_diff = None;
                    self.diff_syntax = None;
                    if immediate {
                        self.pending_diff = None;
                        self.requested_diff = Some(k.clone());
                        effects.push(Effect::RequestStageFileDiff(k));
                    } else {
                        self.pending_diff = Some((k, now + crate::log_view::DETAILS_DEBOUNCE));
                    }
                }
            } else {
                self.current_diff = None;
                self.diff_syntax = None;
                self.requested_diff = None;
                self.pending_diff = None;
            }
        } else if let Some(k) = desired {
            if self.current_diff.is_none() {
                if let Some(cached) = self.diff_cache.get(&k).cloned() {
                    self.pending_diff = None;
                    self.requested_diff = None;
                    self.diff_syntax = self.diff_syntax_cache.get(&k).cloned();
                    self.current_diff = Some(Arc::clone(&cached));
                    if self.diff_syntax.is_none() {
                        Self::schedule_syntax(cached, stage_syntax, syntax_enabled, effects);
                    }
                } else if self.requested_diff.as_ref() != Some(&k)
                    && self.pending_diff.as_ref().map(|(pk, _)| pk) != Some(&k)
                {
                    if immediate {
                        self.pending_diff = None;
                        self.requested_diff = Some(k.clone());
                        effects.push(Effect::RequestStageFileDiff(k));
                    } else {
                        self.pending_diff = Some((k, now + crate::log_view::DETAILS_DEBOUNCE));
                    }
                }
            } else if self.diff_syntax.is_none()
                && let Some(cached_hl) = self.diff_syntax_cache.get(&k).cloned()
            {
                self.diff_syntax = Some(cached_hl);
            } else if self.diff_syntax.is_none()
                && let Some(diff) = self.current_diff.clone()
            {
                Self::schedule_syntax(diff, stage_syntax, syntax_enabled, effects);
            }
        }
    }

    pub fn flush_pending(
        &mut self,
        now: Instant,
        stage_syntax: &mut RequestSlot<StageDiffKey>,
        syntax_enabled: bool,
        effects: &mut Vec<Effect>,
    ) {
        self.flush_draft_due(now);
        if let Some((key, at)) = self.pending_diff.take() {
            if at <= now {
                if let Some(cached) = self.diff_cache.get(&key).cloned() {
                    if self.current_diff_key.as_ref() == Some(&key) {
                        self.diff_syntax = self.diff_syntax_cache.get(&key).cloned();
                        self.current_diff = Some(Arc::clone(&cached));
                        if self.diff_syntax.is_none() {
                            Self::schedule_syntax(cached, stage_syntax, syntax_enabled, effects);
                        }
                    }
                } else if self.requested_diff.as_ref() != Some(&key) {
                    self.requested_diff = Some(key.clone());
                    effects.push(Effect::RequestStageFileDiff(key));
                }
            } else {
                self.pending_diff = Some((key, at));
            }
        }
    }

    pub fn apply_engine_event(
        &mut self,
        event: EngineEvent,
        request_slots: &mut jbgit_core::RequestSlotTable,
        stage_syntax: &mut RequestSlot<StageDiffKey>,
        syntax_enabled: bool,
        ctx: &mut CommitContext<'_>,
    ) -> Option<EngineEvent> {
        match event {
            EngineEvent::StageStatusLoaded { status, epoch } => {
                if !request_slots.accept(RequestSlotId::StageStatus, epoch) {
                    return None;
                }
                self.apply_status(status);
                None
            }
            EngineEvent::StageFileDiffLoaded { diff, epoch } => {
                if !request_slots.accept(RequestSlotId::StageFileDiff, epoch) {
                    return None;
                }
                if self.requested_diff.as_ref() == Some(&diff.key) {
                    self.requested_diff = None;
                }
                let is_wanted = self.current_diff_key.as_ref() == Some(&diff.key);
                self.diff_cache.insert(diff.key.clone(), Arc::clone(&diff));
                if is_wanted {
                    self.diff_syntax = self.diff_syntax_cache.get(&diff.key).cloned();
                    self.current_diff = Some(Arc::clone(&diff));
                    let len = self.diff_rows().len();
                    if len == 0 {
                        self.diff_cursor_row = 0;
                        self.diff_scroll.reset();
                    } else {
                        self.diff_cursor_row = self.diff_cursor_row.min(len - 1);
                    }
                    if self.diff_syntax.is_none() {
                        Self::schedule_syntax(diff, stage_syntax, syntax_enabled, ctx.effects);
                    }
                }
                None
            }
            EngineEvent::AmendPreviewLoaded { preview, epoch } => {
                if !request_slots.accept(RequestSlotId::AmendPreview, epoch) {
                    return None;
                }
                let prev_sha = self
                    .amend_preview
                    .as_ref()
                    .map(|p| p.short_sha.clone());
                let new_sha = preview.as_ref().map(|p| p.short_sha.clone());
                let head_changed = prev_sha != new_sha;
                if head_changed {
                    self.invalidate_all_diff_caches();
                    self.pending_select_immediate = true;
                }
                let should_fill = self.amend && (self.amend_awaiting_message || head_changed);
                self.set_amend_preview(preview.clone());
                self.amend_awaiting_message = false;
                if should_fill
                    && let Some(p) = preview
                    && !p.message.trim().is_empty()
                {
                    self.editor.set_text(&p.message);
                }
                None
            }
            EngineEvent::CommitPrecheckLoaded { info } => {
                self.precheck = info;
                None
            }
            EngineEvent::CommitCompleted(outcome) => {
                self.commit_in_progress = false;
                if outcome.success {
                    self.invalidate_all_diff_caches();
                    self.pending_select_immediate = true;
                    let was_amend = self.amend;
                    let committed_msg = self.editor.text();
                    self.push_history(&committed_msg);
                    self.editor.clear();
                    self.set_amend_flag(false);
                    self.amend_awaiting_message = false;
                    self.saved_draft = None;
                    self.set_amend_preview(None);
                    self.empty_message_error = false;
                    self.mark_draft_dirty(ctx.now);
                    let localized_summary = match outcome.short_sha.as_deref() {
                        Some(sha) if was_amend => {
                            ctx.i18n.t_fmt("toast.amended_commit", &[("sha", sha)])
                        }
                        Some(sha) => ctx.i18n.t_fmt("toast.committed", &[("sha", sha)]),
                        None => outcome.summary.clone(),
                    };
                    self.last_commit_banner = Some(CommitBanner {
                        summary: localized_summary.clone(),
                        can_undo: true,
                    });
                    *ctx.status_notice = Some(localized_summary);
                    if outcome.and_push {
                        let branch = ctx
                            .snap
                            .and_then(|s| s.head_branch.as_deref())
                            .unwrap_or("HEAD")
                            .to_string();
                        let upstream = ctx
                            .snap
                            .and_then(|s| s.local_branch(&branch))
                            .and_then(|l| l.upstream.clone());
                        let remote = upstream
                            .as_ref()
                            .and_then(|u| u.split_once('/').map(|(r, _)| r.to_string()))
                            .unwrap_or_else(|| ctx.default_remote.to_string());
                        let remote_branch = upstream
                            .as_ref()
                            .and_then(|u| u.split_once('/').map(|(_, rb)| rb.to_string()))
                            .unwrap_or_else(|| branch.clone());
                        let set_upstream = upstream.is_none();
                        let sha = outcome.short_sha.as_deref().unwrap_or("commit");
                        let body: Vec<String> = ctx
                            .i18n
                            .t_fmt(
                                "dialog.push.confirm_body",
                                &[("sha", sha), ("branch", &branch)],
                            )
                            .lines()
                            .map(ToString::to_string)
                            .collect();
                        ctx.modals.push(ModalLayer::Dialog(Box::new(Dialog::Choice(
                            ChoiceDialog::confirm_with_default(
                                ctx.i18n.t("dialog.push.confirm_title"),
                                body,
                                ctx.i18n.t("dialog.push.push_btn"),
                                ctx.i18n.cancel(),
                                false,
                                true,
                                vec![Action::RunGitOp(GitOp::Push {
                                    branch,
                                    remote,
                                    remote_branch,
                                    set_upstream,
                                    force: jbgit_core::PushForce::None,
                                    tags: jbgit_core::PushTagMode::None,
                                    up_to: None,
                                })],
                            ),
                        ))));
                    }
                } else {
                    if let Some(msg) = outcome.preserved_message
                        && self.editor.is_empty()
                    {
                        self.editor.set_text(&msg);
                    }
                    *ctx.status_notice = Some(outcome.summary.clone());
                    let output = outcome.error_output.unwrap_or(outcome.summary);
                    ctx.modals.push(ModalLayer::HookError { output });
                }
                None
            }
            EngineEvent::UndoCommitCompleted { restored_message } => {
                self.last_commit_banner = None;
                self.set_amend_preview(None);
                if self.editor.is_empty() && !restored_message.is_empty() {
                    self.editor.set_text(&restored_message);
                    self.mark_draft_dirty(ctx.now);
                }
                *ctx.status_notice = Some(ctx.i18n.t("toast.undid_last_commit").to_string());
                None
            }
            other => Some(other),
        }
    }

    pub fn commit_history_candidates(&self, snap: Option<&RepoSnapshot>) -> Vec<String> {
        let mut out = Vec::new();
        if let Some(draft) = &self.saved_draft
            && !draft.trim().is_empty()
        {
            out.push(draft.trim().to_string());
        }
        if !self.amend {
            let current = self.editor.text();
            let trimmed = current.trim();
            if !trimmed.is_empty() && !out.iter().any(|s| s == trimmed) {
                out.push(trimmed.to_string());
            }
        }
        for h in &self.commit_history {
            if !out.contains(h) {
                out.push(h.clone());
            }
        }
        if let Some(snap) = snap {
            for c in snap.commits.iter().take(20) {
                let full = if c.body.is_empty() {
                    c.subject.clone()
                } else {
                    format!("{}\n\n{}", c.subject, c.body)
                };
                if !full.trim().is_empty() && !out.contains(&full) {
                    out.push(full);
                    if out.len() >= 20 {
                        break;
                    }
                }
            }
        }
        out.truncate(20);
        out
    }

    pub fn dispatch(&mut self, action: &Action, ctx: &mut CommitContext<'_>) -> bool {
        if self.dispatch_tree_action(action, ctx) {
            return true;
        }
        if self.dispatch_diff_action(action, ctx) {
            return true;
        }
        self.dispatch_editor_action(action, ctx)
    }

    fn dispatch_tree_action(&mut self, action: &Action, ctx: &mut CommitContext<'_>) -> bool {
        match action {
            Action::SelectCommitStageRow(idx) => {
                if !self.rows.is_empty() {
                    self.select_row(*idx);
                    self.multi_selected.clear();
                    self.pending_select_immediate = true;
                }
            }
            Action::ScrollCommitStage(delta) => {
                if !self.rows.is_empty() {
                    self.move_selection(*delta);
                    self.multi_selected.clear();
                    self.pending_select_immediate = false;
                }
            }
            Action::StageExpand(expand) => {
                self.expand_selected(*expand);
            }
            Action::StageToggleRow(idx) => {
                self.toggle_row(*idx);
            }
            Action::StageToggleMulti(idx) => {
                self.toggle_multi_select(*idx);
            }
            Action::StageSelectAll => {
                self.select_all_files();
            }
            Action::StageToggleGroupByDir => {
                self.toggle_group_by_dir();
            }
            Action::StageExpandAll(expand) => {
                self.expand_all(*expand);
            }
            Action::StageToggleShowIgnored => {
                self.toggle_show_ignored();
                *ctx.loading_snapshot = true;
                ctx.effects.push(Effect::RefreshScope(RefreshScope::StageStatus));
            }
            Action::StageToggleDiffCollapsed => {
                self.diff_collapsed = !self.diff_collapsed;
                if self.diff_collapsed && *ctx.focused_pane == PaneId::CommitDiffPreview {
                    *ctx.focused_pane = PaneId::CommitChangesTree;
                }
            }
            Action::StageActionSelected => {
                if let Some(status) = self.status.clone() {
                    let mut targets = self.collect_targets_for_action(
                        self.selected_idx,
                        &status,
                        StageNodeKind::can_stage,
                    );
                    let had_targets = !targets.is_empty();
                    targets.retain(|t| !t.is_submodule);
                    if !targets.is_empty() {
                        self.invalidate_paths(&targets);
                        let count_s = targets.len().to_string();
                        *ctx.status_notice =
                            Some(ctx.i18n.t_fmt("toast.staging_paths", &[("count", &count_s)]));
                        ctx.effects.push(Effect::StageTargets(targets));
                    } else if had_targets {
                        *ctx.status_notice =
                            Some(ctx.i18n.t("toast.submodule_readonly").to_string());
                    }
                }
            }
            Action::UnstageActionSelected => {
                if let Some(status) = self.status.clone() {
                    let mut targets = self.collect_targets_for_action(
                        self.selected_idx,
                        &status,
                        StageNodeKind::can_unstage,
                    );
                    let had_targets = !targets.is_empty();
                    targets.retain(|t| !t.is_submodule);
                    if !targets.is_empty() {
                        self.invalidate_paths(&targets);
                        let count_s = targets.len().to_string();
                        *ctx.status_notice = Some(
                            ctx.i18n
                                .t_fmt("toast.unstaging_paths", &[("count", &count_s)]),
                        );
                        ctx.effects.push(Effect::UnstageTargets(targets));
                    } else if had_targets {
                        *ctx.status_notice =
                            Some(ctx.i18n.t("toast.submodule_readonly").to_string());
                    }
                }
            }
            Action::StageAllTracked => {
                self.invalidate_all_diff_caches();
                self.pending_select_immediate = true;
                ctx.effects.push(Effect::StageAllTracked);
                *ctx.status_notice = Some(ctx.i18n.t("toast.staging_all_tracked").to_string());
            }
            Action::DiscardPromptSelected => {
                if let Some(status) = self.status.clone() {
                    let mut targets = self.collect_targets_for_action(
                        self.selected_idx,
                        &status,
                        |k| matches!(k, StageNodeKind::Unstaged | StageNodeKind::Untracked),
                    );
                    let had_targets = !targets.is_empty();
                    targets.retain(|t| !t.is_submodule);
                    if !targets.is_empty() {
                        let count_s = targets.len().to_string();
                        let sample: String = targets
                            .iter()
                            .take(4)
                            .map(|t| format!("  • {}", t.path))
                            .collect::<Vec<_>>()
                            .join("\n");
                        let body: Vec<String> = ctx
                            .i18n
                            .t_fmt(
                                "dialog.discard.body",
                                &[("count", &count_s), ("sample", &sample)],
                            )
                            .lines()
                            .map(ToString::to_string)
                            .collect();
                        ctx.modals.push(ModalLayer::Dialog(Box::new(Dialog::Choice(
                            ChoiceDialog::confirm(
                                ctx.i18n.t("dialog.discard.title"),
                                body,
                                ctx.i18n.t("common.discard"),
                                ctx.i18n.cancel(),
                                vec![Action::DiscardTargetsConfirmed(targets)],
                            ),
                        ))));
                    } else if had_targets {
                        *ctx.status_notice =
                            Some(ctx.i18n.t("toast.submodule_readonly").to_string());
                    }
                }
            }
            Action::StashAll => {
                ctx.effects.push(Effect::GitOp(GitOp::StashPush {
                    message: None,
                    include_untracked: false,
                    paths: Vec::new(),
                }));
            }
            Action::ResolveConflictsNotice => {
                if let Some(status) = &self.status
                    && !status.conflicted.is_empty()
                {
                    let paths = status
                        .conflicted
                        .iter()
                        .map(|e| format!("  ! {}", e.path))
                        .collect::<Vec<_>>()
                        .join("\n");
                    let body: Vec<String> = ctx
                        .i18n
                        .t_fmt("dialog.conflicts_notice.body", &[("paths", &paths)])
                        .lines()
                        .map(ToString::to_string)
                        .collect();
                    ctx.modals.push(ModalLayer::Dialog(Box::new(Dialog::Choice(
                        ChoiceDialog::confirm_with_default(
                            ctx.i18n.t("dialog.conflicts_notice.title"),
                            body,
                            ctx.i18n.ok(),
                            ctx.i18n.close(),
                            false,
                            true,
                            vec![Action::RefreshAll],
                        ),
                    ))));
                }
            }
            _ => return false,
        }
        true
    }

    fn dispatch_diff_action(&mut self, action: &Action, ctx: &mut CommitContext<'_>) -> bool {
        match action {
            Action::StageDiffMoveCursor(delta) => {
                let len = self.diff_rows().len();
                if len > 0 {
                    self.diff_cursor_row =
                        apply_scroll_delta(self.diff_cursor_row, *delta, len - 1);
                    let viewport_h = self
                        .diff_scroll
                        .viewport_height()
                        .filter(|&h| h > 0)
                        .unwrap_or(18);
                    self.diff_scroll
                        .ensure_visible(self.diff_cursor_row, len, viewport_h);
                }
            }
            Action::DiffScroll(DiffScope::Stage, delta) => {
                self.scroll_diff(*delta);
            }
            Action::DiffJumpHunk(DiffScope::Stage, dir) => {
                self.bracket_jump_at = Some(ctx.now);
                let headers = self.hunk_header_rows();
                if !headers.is_empty() {
                    let cur = self.diff_cursor_row;
                    let target = if *dir >= 0 {
                        headers
                            .iter()
                            .copied()
                            .find(|&h| h > cur)
                            .unwrap_or(headers[0])
                    } else {
                        headers
                            .iter()
                            .copied()
                            .rev()
                            .find(|&h| h < cur)
                            .unwrap_or(*headers.last().unwrap())
                    };
                    self.diff_cursor_row = target;
                    self.diff_scroll.set_offset(target);
                }
            }
            Action::DiffNextFile(DiffScope::Stage, delta) => {
                let file_indices: Vec<usize> = self
                    .rows
                    .iter()
                    .enumerate()
                    .filter(|(_, r)| {
                        matches!(
                            r.kind,
                            StageTreeRowKind::File { .. } | StageTreeRowKind::AmendFile { .. }
                        )
                    })
                    .map(|(i, _)| i)
                    .collect();
                if !file_indices.is_empty() {
                    let cur = self.selected_idx;
                    let next_pos = match file_indices.iter().position(|&i| i == cur) {
                        Some(pos) => (pos as isize + *delta)
                            .clamp(0, file_indices.len() as isize - 1) as usize,
                        None if *delta >= 0 => file_indices
                            .iter()
                            .position(|&i| i > cur)
                            .unwrap_or(0),
                        None => file_indices
                            .iter()
                            .rposition(|&i| i < cur)
                            .unwrap_or(file_indices.len() - 1),
                    };
                    self.select_row(file_indices[next_pos]);
                    self.multi_selected.clear();
                }
            }
            Action::StageDiffToggleVisual => {
                if self.visual_anchor_row.is_some() {
                    self.visual_anchor_row = None;
                } else {
                    self.visual_anchor_row = Some(self.diff_cursor_row);
                }
            }
            Action::DiffToggleSideBySide(DiffScope::Stage) => {
                self.diff_side_by_side = !self.diff_side_by_side;
                self.visual_anchor_row = None;
            }
            Action::DiffCycleContext(DiffScope::Stage) => {
                self.diff_context = self.diff_context.next();
                self.requested_diff = None;
                self.pending_diff = None;
                self.visual_anchor_row = None;
                self.pending_select_immediate = true;
            }
            Action::StageDiffSwitchSide => {
                if let Some(key) = &self.current_diff_key {
                    let target_group = match key.side {
                        StageDiffSide::Staged => StageNodeKind::Unstaged,
                        StageDiffSide::Unstaged => StageNodeKind::Staged,
                        StageDiffSide::AmendCommit => return true,
                    };
                    let target_path = key.path.clone();
                    if let Some(pos) = self.rows.iter().position(|r| {
                        matches!(
                            &r.kind,
                            StageTreeRowKind::File { group_kind, path, .. }
                            if *group_kind == target_group && *path == target_path
                        )
                    }) {
                        self.select_row(pos);
                        self.pending_select_immediate = true;
                    }
                }
            }
            Action::StageDiffStageSelection => {
                if let Some(key) = self.readonly_diff_notice_key() {
                    *ctx.status_notice = Some(ctx.i18n.t(key).to_string());
                } else if let Some(sel) = self.collect_selected_line_sets() {
                    self.apply_stage_diff_selection(StageDiffOp::Stage, sel, ctx);
                }
            }
            Action::StageDiffUnstageSelection => {
                if let Some(key) = self.readonly_diff_notice_key() {
                    *ctx.status_notice = Some(ctx.i18n.t(key).to_string());
                } else if let Some(sel) = self.collect_selected_line_sets() {
                    self.apply_stage_diff_selection(StageDiffOp::Unstage, sel, ctx);
                }
            }
            Action::StageDiffRollbackSelection => {
                if let Some(key) = self.readonly_diff_notice_key() {
                    *ctx.status_notice = Some(ctx.i18n.t(key).to_string());
                } else if let Some(sel) = self.collect_selected_line_sets() {
                    self.apply_stage_diff_selection(StageDiffOp::Rollback, sel, ctx);
                }
            }
            Action::StageDiffUndoRollback => {
                if let Some(entry) = self.rollback_undo_stack.pop() {
                    self.invalidate_diff_cache_for_path(&entry.path);
                    self.pending_select_immediate = true;
                    *ctx.status_notice =
                        Some(ctx.i18n.t_fmt("toast.undid_rollback", &[("path", &entry.path)]));
                    ctx.effects.push(Effect::WriteWorktreeBuffer {
                        path: entry.path,
                        new_local_bytes: entry.prev_local_bytes,
                    });
                }
            }
            Action::OpenThreeWayInspector { row_idx, range_idx } => {
                if let Some(key) = self.readonly_diff_notice_key() {
                    *ctx.status_notice = Some(ctx.i18n.t(key).to_string());
                    return true;
                }
                if let Some(r) = *row_idx {
                    self.diff_cursor_row = r;
                }
                ctx.modals.push(ModalLayer::ThreeWayInspector {
                    range_idx: *range_idx,
                });
            }
            Action::ApplyStageDiffHunk(hunk_idx, kind) => {
                let hunk_idx = *hunk_idx;
                match kind {
                    DiffHunkActionKind::Inspect3Way => {
                        if let Some(diff) = &self.current_diff
                            && let Some(hunk) = diff.hunks.get(hunk_idx)
                            && let Some(sr_idx) =
                                hunk.line_meta.iter().find_map(|m| m.staged_range_idx)
                        {
                            let header_row = self
                                .diff_rows()
                                .iter()
                                .position(|r| matches!(r, DiffRow::Header(h) if *h == hunk_idx));
                            if let Some(hr) = header_row {
                                self.diff_cursor_row = hr;
                            }
                            ctx.modals.push(ModalLayer::ThreeWayInspector {
                                range_idx: Some(sr_idx),
                            });
                        }
                    }
                    DiffHunkActionKind::Stage => {
                        if let Some(sel) = self.collect_hunk_line_sets(hunk_idx) {
                            self.apply_stage_diff_selection(StageDiffOp::Stage, sel, ctx);
                        }
                    }
                    DiffHunkActionKind::Unstage => {
                        if let Some(sel) = self.collect_hunk_line_sets(hunk_idx) {
                            self.apply_stage_diff_selection(StageDiffOp::Unstage, sel, ctx);
                        }
                    }
                    DiffHunkActionKind::Rollback => {
                        if let Some(sel) = self.collect_hunk_line_sets(hunk_idx) {
                            self.apply_stage_diff_selection(StageDiffOp::Rollback, sel, ctx);
                        }
                    }
                }
            }
            _ => return false,
        }
        true
    }

    fn readonly_diff_notice_key(&self) -> Option<&'static str> {
        let diff = self.current_diff.as_ref()?;
        if diff.submodule.is_some() {
            Some("toast.submodule_line_staging_disabled")
        } else if diff.lfs.is_some() {
            Some("toast.lfs_line_staging_disabled")
        } else {
            None
        }
    }

    fn apply_stage_diff_selection(
        &mut self,
        op: StageDiffOp,
        sel: LineSelection,
        ctx: &mut CommitContext<'_>,
    ) {
        let Some(diff) = self.current_diff.as_ref().cloned() else {
            return;
        };
        if diff.key.side == StageDiffSide::AmendCommit {
            return;
        }
        if let Some(key) = self.readonly_diff_notice_key() {
            *ctx.status_notice = Some(ctx.i18n.t(key).to_string());
            return;
        }
        match op {
            StageDiffOp::Stage if !sel.staged_side => {
                let new_staged = diff.tracker.stage_lines(&sel);
                self.visual_anchor_row = None;
                self.invalidate_diff_cache_for_path(&diff.key.path);
                self.pending_select_immediate = true;
                ctx.effects.push(Effect::ApplyStageBuffer {
                    path: diff.key.path.clone(),
                    new_staged_bytes: new_staged,
                    file_mode: diff.file_mode,
                    remove_from_index: false,
                });
            }
            StageDiffOp::Unstage if sel.staged_side => {
                let new_staged = diff.tracker.unstage_lines(&sel);
                let remove = !diff.vcs_exists && new_staged.is_empty();
                self.visual_anchor_row = None;
                self.invalidate_diff_cache_for_path(&diff.key.path);
                self.pending_select_immediate = true;
                ctx.effects.push(Effect::ApplyStageBuffer {
                    path: diff.key.path.clone(),
                    new_staged_bytes: new_staged,
                    file_mode: diff.file_mode,
                    remove_from_index: remove,
                });
            }
            StageDiffOp::Rollback if !sel.staged_side => {
                let new_local = diff.tracker.rollback_lines(&sel);
                self.rollback_undo_stack.push(RollbackUndoEntry {
                    path: diff.key.path.clone(),
                    prev_local_bytes: diff.local_bytes.clone(),
                });
                self.visual_anchor_row = None;
                self.invalidate_diff_cache_for_path(&diff.key.path);
                self.pending_select_immediate = true;
                *ctx.status_notice = Some(
                    ctx.i18n
                        .t_fmt("toast.rolled_back_lines", &[("path", &diff.key.path)]),
                );
                ctx.effects.push(Effect::WriteWorktreeBuffer {
                    path: diff.key.path.clone(),
                    new_local_bytes: new_local,
                });
            }
            _ => {}
        }
    }

    fn dispatch_editor_action(&mut self, action: &Action, ctx: &mut CommitContext<'_>) -> bool {
        match action {
            Action::CommitEditorChar(ch) => {
                self.empty_message_error = false;
                self.editor.insert_char(*ch);
                self.mark_draft_dirty(ctx.now);
            }
            Action::CommitEditorNewline => {
                self.empty_message_error = false;
                self.editor.insert_newline();
                self.mark_draft_dirty(ctx.now);
            }
            Action::CommitEditorPaste(text) => {
                self.empty_message_error = false;
                self.editor.insert_paste(text);
                self.mark_draft_dirty(ctx.now);
            }
            Action::CommitEditorBackspace => {
                self.empty_message_error = false;
                self.editor.backspace();
                self.mark_draft_dirty(ctx.now);
            }
            Action::CommitEditorDelete => {
                self.editor.delete_forward();
                self.mark_draft_dirty(ctx.now);
            }
            Action::CommitEditorDeleteWord => {
                self.editor.delete_word_backward();
                self.mark_draft_dirty(ctx.now);
            }
            Action::CommitEditorMove(kind) => match kind {
                EditorMoveKind::Left => self.editor.move_left(),
                EditorMoveKind::Right => self.editor.move_right(),
                EditorMoveKind::Up => self.editor.move_up(),
                EditorMoveKind::Down => self.editor.move_down(),
                EditorMoveKind::Home => self.editor.move_home(),
                EditorMoveKind::End => self.editor.move_end(),
            },
            Action::CommitEditorUndo => {
                if self.editor.undo() {
                    self.mark_draft_dirty(ctx.now);
                }
            }
            Action::CommitEditorRedo => {
                if self.editor.redo() {
                    self.mark_draft_dirty(ctx.now);
                }
            }
            Action::CommitToggleAmend => {
                let next = !self.amend;
                self.set_amend_flag(next);
                self.empty_message_error = false;
                if next {
                    self.saved_draft = Some(self.editor.text());
                    self.mark_draft_dirty(ctx.now);
                    self.flush_draft_sync();
                    self.amend_awaiting_message = true;
                    if let Some(preview) = &self.amend_preview
                        && !preview.message.trim().is_empty()
                    {
                        self.editor.set_text(&preview.message);
                        self.amend_awaiting_message = false;
                    }
                    ctx.effects.push(Effect::RequestAmendPreview);
                } else {
                    self.amend_awaiting_message = false;
                    if let Some(draft) = self.saved_draft.take() {
                        self.editor.set_text(&draft);
                    }
                    self.mark_draft_dirty(ctx.now);
                    self.flush_draft_sync();
                }
            }
            Action::CommitToggleSignoff => {
                self.signoff = !self.signoff;
            }
            Action::CommitToggleRunHooks => {
                self.run_hooks = !self.run_hooks;
            }
            Action::CommitToggleAuthorInput => {
                self.show_author_input = !self.show_author_input;
                self.editing_author = self.show_author_input;
            }
            Action::CommitFocusAuthorInput => {
                *ctx.focused_pane = PaneId::CommitMessageEditor;
                self.show_author_input = true;
                self.editing_author = true;
            }
            Action::CommitAuthorChar(ch) => {
                self.author_input.push(*ch);
            }
            Action::CommitAuthorBackspace => {
                self.author_input.pop();
            }
            Action::CommitAuthorDone => {
                self.editing_author = false;
            }
            Action::CommitApplyHistory(idx) => {
                let candidates = self.commit_history_candidates(ctx.snap);
                if let Some(msg) = candidates.get(*idx).cloned() {
                    let prev = self.editor.text();
                    if !self.amend && !prev.trim().is_empty() && prev.trim() != msg.trim() {
                        self.push_history(&prev);
                    }
                    self.editor.set_text(&msg);
                    self.empty_message_error = false;
                    self.mark_draft_dirty(ctx.now);
                    *ctx.focused_pane = PaneId::CommitMessageEditor;
                }
            }
            Action::SubmitCommit { and_push } => {
                self.handle_submit_commit(*and_push, false, ctx);
            }
            Action::UndoLastCommit => {
                ctx.effects.push(Effect::UndoLastCommit);
            }
            Action::OpenExternalEditorMessage => {
                ctx.effects.push(Effect::OpenExternalEditorMessage);
            }
            Action::OpenExternalEditorFile(opt_path) => {
                let path = opt_path
                    .clone()
                    .or_else(|| self.current_diff_key.as_ref().map(|k| k.path.clone()));
                if let Some(p) = path {
                    ctx.effects.push(Effect::OpenExternalEditorFile(p));
                }
            }
            _ => return false,
        }
        true
    }

    pub fn handle_submit_commit(
        &mut self,
        and_push: bool,
        force_stage_all_tracked: bool,
        ctx: &mut CommitContext<'_>,
    ) {
        if self.commit_in_progress {
            return;
        }
        self.flush_draft_sync();
        if let Some(status) = &self.status
            && !status.conflicted.is_empty()
        {
            *ctx.status_notice = Some(ctx.i18n.t("toast.resolve_before_commit").to_string());
            let _ = self.dispatch(&Action::ResolveConflictsNotice, ctx);
            return;
        }

        let msg = self.editor.text();
        if msg.trim().is_empty() {
            self.empty_message_error = true;
            *ctx.focused_pane = PaneId::CommitMessageEditor;
            *ctx.status_notice = Some(ctx.i18n.t("common.commit_msg_empty").to_string());
            return;
        }

        let staged_empty = self.status.as_ref().is_none_or(|s| s.staged.is_empty());
        let has_tracked_unstaged = self
            .status
            .as_ref()
            .is_some_and(|s| !s.unstaged.is_empty());

        let author_override = self
            .show_author_input
            .then(|| self.author_input.clone())
            .filter(|s| !s.trim().is_empty());

        if staged_empty && !self.amend && !force_stage_all_tracked {
            if has_tracked_unstaged {
                let req = CommitRequest {
                    message: msg,
                    amend: false,
                    signoff: self.signoff,
                    run_hooks: self.run_hooks,
                    author_override,
                    stage_all_tracked: true,
                    and_push,
                };
                let body: Vec<String> = ctx
                    .i18n
                    .t("dialog.nothing_staged.body")
                    .lines()
                    .map(ToString::to_string)
                    .collect();
                ctx.modals.push(ModalLayer::Dialog(Box::new(Dialog::Choice(
                    ChoiceDialog::confirm_with_default(
                        ctx.i18n.t("dialog.nothing_staged.title"),
                        body,
                        ctx.i18n.t("dialog.nothing_staged.confirm_btn"),
                        ctx.i18n.cancel(),
                        false,
                        true,
                        vec![Action::ExecuteCommitConfirmed(req)],
                    ),
                ))));
            } else {
                *ctx.status_notice = Some(ctx.i18n.t("toast.no_staged_to_commit").to_string());
            }
            return;
        }

        self.commit_in_progress = true;
        self.empty_message_error = false;
        let req = CommitRequest {
            message: msg,
            amend: self.amend,
            signoff: self.signoff,
            run_hooks: self.run_hooks,
            author_override,
            stage_all_tracked: force_stage_all_tracked,
            and_push,
        };
        ctx.effects.push(Effect::ExecuteCommit(req));
    }
}
