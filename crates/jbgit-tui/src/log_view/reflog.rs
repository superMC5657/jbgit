//! Self-contained Reflog modal state (`git reflog show` + synced Changes & Diff preview).

use std::sync::Arc;

use jbgit_core::{
    DEFAULT_REFLOG_LIMIT, DetailsKey, DiffContext, FileChange, FileDiffKey, ReflogBundle,
    ReflogEntry, ReflogKey,
};

use crate::diff_session::{ChangesTreeSession, DiffCacheStore, DiffSession};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ReflogFocus {
    #[default]
    Entries,
    Changes,
    Diff,
}

impl ReflogFocus {
    pub const fn next(self) -> Self {
        match self {
            Self::Entries => Self::Changes,
            Self::Changes => Self::Diff,
            Self::Diff => Self::Entries,
        }
    }

    pub const fn prev(self) -> Self {
        match self {
            Self::Entries => Self::Diff,
            Self::Changes => Self::Entries,
            Self::Diff => Self::Changes,
        }
    }
}

#[derive(Debug)]
pub struct ReflogState {
    pub key: ReflogKey,
    pub bundle: Option<Arc<ReflogBundle>>,
    pub loading: bool,
    pub error: Option<String>,
    pub selected: usize,
    pub focus: ReflogFocus,
    pub changes: ChangesTreeSession,
    pub diff: DiffSession,
    pub details_requested: Option<DetailsKey>,
}

impl ReflogState {
    pub fn new(ref_name: impl Into<String>, diff_context: DiffContext) -> Self {
        Self::new_with_prefs(
            ref_name,
            diff_context,
            jbgit_core::WhitespaceMode::Default,
            false,
        )
    }

    pub fn new_with_prefs(
        ref_name: impl Into<String>,
        diff_context: DiffContext,
        whitespace: jbgit_core::WhitespaceMode,
        soft_wrap: bool,
    ) -> Self {
        Self {
            key: ReflogKey::for_ref(ref_name),
            bundle: None,
            loading: true,
            error: None,
            selected: 0,
            focus: ReflogFocus::Entries,
            changes: ChangesTreeSession::default(),
            diff: DiffSession::from_prefs(diff_context, whitespace, soft_wrap),
            details_requested: None,
        }
    }

    pub fn entries(&self) -> &[ReflogEntry] {
        self.bundle
            .as_ref()
            .map_or(&[], |b| b.entries.as_slice())
    }

    pub fn has_more(&self) -> bool {
        self.bundle.as_ref().is_some_and(|b| b.has_more)
    }

    pub fn selected_entry(&self) -> Option<&ReflogEntry> {
        let entries = self.entries();
        entries.get(self.selected.min(entries.len().saturating_sub(1)))
    }

    pub fn apply_bundle(&mut self, bundle: Arc<ReflogBundle>) {
        self.loading = false;
        self.error = None;
        let len = bundle.entries.len();
        self.bundle = Some(bundle);
        if len == 0 {
            self.selected = 0;
        } else {
            self.selected = self.selected.min(len - 1);
        }
        self.sync_selection_key();
    }

    pub fn apply_error(&mut self, msg: String) {
        self.loading = false;
        self.error = Some(msg);
    }

    pub fn move_selection(&mut self, delta: isize) {
        let len = self.entries().len();
        if len == 0 {
            return;
        }
        let next = (self.selected as isize + delta).clamp(0, (len - 1) as isize) as usize;
        self.select_row(next);
    }

    pub fn select_row(&mut self, idx: usize) {
        let len = self.entries().len();
        if len == 0 {
            return;
        }
        let next = idx.min(len - 1);
        if next != self.selected {
            self.selected = next;
            self.sync_selection_key();
        }
    }

    pub fn sync_selection_key(&mut self) {
        let next_key = self.desired_details_key();
        if self.changes.current_key != next_key {
            self.changes.current_key = next_key;
            self.changes.selected = 0;
            self.diff.scroll.reset();
            self.diff.requested = None;
        }
    }

    pub fn desired_details_key(&self) -> Option<DetailsKey> {
        self.selected_entry().map(|e| DetailsKey::single(e.oid))
    }

    pub fn selected_change<'a>(&self, caches: &'a DiffCacheStore) -> Option<&'a FileChange> {
        self.changes.selected_change(caches)
    }

    pub fn desired_diff_key(&self, caches: &DiffCacheStore) -> Option<FileDiffKey> {
        let details = self.changes.current_key.clone()?;
        let ch = self.selected_change(caches)?;
        Some(FileDiffKey {
            details,
            path: ch.path.clone(),
            old_path: ch.old_path.clone(),
            context: self.diff.context,
            whitespace: self.diff.whitespace,
        })
    }

    pub fn next_deadline(&self) -> Option<std::time::Instant> {
        self.diff.next_deadline()
    }

    pub fn load_more(&mut self) -> ReflogKey {
        self.key.limit = self.key.limit.saturating_add(DEFAULT_REFLOG_LIMIT);
        self.loading = true;
        self.error = None;
        self.key.clone()
    }
}
