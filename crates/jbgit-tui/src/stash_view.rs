//! `[3 Stash]` workbench (`git4idea/stash/ui`): encapsulates stash list
//! selection paired with shared [`ChangesTreeSession`] + [`DiffSession`]
//! reading from the single global [`DiffCacheStore`].

use std::sync::Arc;

use jbgit_core::{
    DetailsKey, DiffContext, FileDiff, FileDiffKey, RepoSnapshot, RequestSlotId, StashEntry,
    WhitespaceMode,
};

use crate::app::{Action, ChangesHost, DiffScope, Effect};
use crate::diff_session::{ChangesTreeSession, DiffCacheStore, DiffSession};

#[derive(Debug, Default)]
pub struct StashWorkbench {
    selected: usize,
    changes: ChangesTreeSession,
    diff: DiffSession,
    requested_details: Option<DetailsKey>,
}

impl StashWorkbench {
    pub fn with_diff_context(context: DiffContext) -> Self {
        Self::with_diff_prefs(context, WhitespaceMode::Default, false)
    }

    pub fn with_diff_prefs(
        context: DiffContext,
        whitespace: WhitespaceMode,
        soft_wrap: bool,
    ) -> Self {
        Self {
            selected: 0,
            changes: ChangesTreeSession::default(),
            diff: DiffSession::from_prefs(context, whitespace, soft_wrap),
            requested_details: None,
        }
    }

    pub fn selected(&self) -> usize {
        self.selected
    }

    pub fn changes(&self) -> &ChangesTreeSession {
        &self.changes
    }

    pub fn diff(&self) -> &DiffSession {
        &self.diff
    }

    pub fn selected_entry<'a>(&self, stashes: &'a [StashEntry]) -> Option<&'a StashEntry> {
        stashes.get(self.selected.min(stashes.len().saturating_sub(1)))
    }

    pub fn details_key(&self, stashes: &[StashEntry]) -> Option<DetailsKey> {
        self.selected_entry(stashes)
            .map(|s| DetailsKey::single(s.oid))
    }

    pub fn changes_len(&self, store: &DiffCacheStore) -> usize {
        self.changes.change_rows(store).len()
    }

    pub fn diff_key(&self, store: &DiffCacheStore) -> Option<FileDiffKey> {
        self.changes
            .desired_diff_key(store, self.diff.context, self.diff.whitespace)
    }

    pub fn scroll_diff(&mut self, delta: isize, store: &DiffCacheStore) {
        let diff = self.diff_key(store).and_then(|k| store.peek_diff(&k));
        let fallback = self.diff.fallback_max_scroll(diff);
        self.diff.scroll.scroll_by(delta, fallback);
    }

    pub fn bind_diff_viewport(&mut self, viewport_h: usize, max_scroll: usize) {
        self.diff.scroll.bind_bound(viewport_h, max_scroll);
    }

    pub fn on_details_loaded(&mut self, key: &DetailsKey) {
        if self.requested_details.as_ref() == Some(key) {
            self.requested_details = None;
        }
    }

    pub fn on_file_diff_loaded(&mut self, key: &FileDiffKey) {
        if self.diff.requested.as_ref() == Some(key) {
            self.diff.requested = None;
        }
    }

    pub fn on_error(&mut self) {
        self.diff.requested = None;
    }

    pub fn pending_syntax_diff(&self, store: &DiffCacheStore) -> Option<Arc<FileDiff>> {
        let dk = self.diff_key(store)?;
        if store.peek_syntax(&dk).is_none() {
            store.peek_diff(&dk).cloned()
        } else {
            None
        }
    }

    pub fn dispatch(
        &mut self,
        action: &Action,
        store: &DiffCacheStore,
        snap: Option<&RepoSnapshot>,
    ) -> bool {
        match action {
            Action::StashSelect(idx) => {
                let len = snap.map_or(0, |s| s.stashes.len());
                if len > 0 {
                    self.selected = (*idx).min(len - 1);
                    self.changes.selected = 0;
                    self.diff.reset_view_offsets();
                }
            }
            Action::StashMove(delta) => {
                let len = snap.map_or(0, |s| s.stashes.len());
                if len > 0 {
                    let next = (self.selected as isize + delta).clamp(0, len as isize - 1);
                    self.selected = next as usize;
                    self.changes.selected = 0;
                    self.diff.reset_view_offsets();
                }
            }
            Action::ChangesMove(ChangesHost::Stash, delta) => {
                self.changes.move_selection(*delta, store);
                self.diff.reset_view_offsets();
            }
            Action::ChangesSelect(ChangesHost::Stash, idx) => {
                self.changes.select_row(*idx, store);
                self.diff.reset_view_offsets();
            }
            Action::ChangesExpand(ChangesHost::Stash, expand) => {
                self.changes.expand_or_collapse(*expand, store);
            }
            Action::ChangesToggleGroupByDir(ChangesHost::Stash) => {
                self.changes.toggle_group_by_dir();
            }
            Action::ChangesExpandAll(ChangesHost::Stash, expand) => {
                self.changes.expand_all(*expand, store);
            }
            Action::DiffNextFile(DiffScope::Stash, delta) => {
                self.changes.select_next_file(*delta, store);
                self.diff.reset_view_offsets();
            }
            Action::DiffJumpHunk(DiffScope::Stash, delta) => {
                let diff = self.diff_key(store).and_then(|k| store.peek_diff(&k));
                self.diff.jump_hunk(*delta, diff);
            }
            Action::DiffToggleSideBySide(DiffScope::Stash) => {
                self.diff.toggle_side_by_side();
            }
            Action::DiffCycleContext(DiffScope::Stash) => {
                self.diff.cycle_context();
            }
            Action::DiffCycleWhitespace(DiffScope::Stash) => {
                self.diff.cycle_whitespace();
            }
            Action::DiffToggleSoftWrap(DiffScope::Stash) => {
                self.diff.toggle_soft_wrap();
            }
            Action::DiffScrollHoriz(DiffScope::Stash, delta) => {
                self.diff.scroll_horiz(*delta);
            }
            Action::DiffSearchOpen(DiffScope::Stash) => {
                self.diff.search.open();
            }
            Action::DiffSearchChar(DiffScope::Stash, ch) => {
                self.diff.search.push_char(*ch);
                let diff = self
                    .diff_key(store)
                    .and_then(|k| store.peek_diff(&k).cloned());
                let tab = self.diff.tab_size();
                self.diff.refresh_search(diff.as_ref(), tab);
            }
            Action::DiffSearchBackspace(DiffScope::Stash) => {
                self.diff.search.backspace();
                let diff = self
                    .diff_key(store)
                    .and_then(|k| store.peek_diff(&k).cloned());
                let tab = self.diff.tab_size();
                self.diff.refresh_search(diff.as_ref(), tab);
            }
            Action::DiffSearchClear(DiffScope::Stash) => {
                self.diff.search.clear_query();
            }
            Action::DiffSearchConfirm(DiffScope::Stash) => {
                self.diff.search.finish_editing();
            }
            Action::DiffSearchClose(DiffScope::Stash) => {
                self.diff.search.close();
            }
            Action::DiffSearchJump(DiffScope::Stash, delta) => {
                let diff = self
                    .diff_key(store)
                    .and_then(|k| store.peek_diff(&k).cloned());
                let tab = self.diff.tab_size();
                self.diff.jump_search(*delta, diff.as_ref(), tab);
            }
            Action::DiffScroll(DiffScope::Stash, delta) => {
                self.scroll_diff(*delta, store);
            }
            _ => return false,
        }
        true
    }

    pub fn reconcile_requests(
        &mut self,
        snap: Option<&Arc<RepoSnapshot>>,
        store: &DiffCacheStore,
        active: bool,
        effects: &mut Vec<Effect>,
    ) {
        if !active {
            return;
        }
        let Some(snap) = snap else {
            return;
        };
        if snap.stashes.is_empty() {
            self.selected = 0;
            self.changes.current_key = None;
            return;
        }
        self.selected = self.selected.min(snap.stashes.len() - 1);
        let desired = self.details_key(&snap.stashes);
        if desired != self.changes.current_key {
            self.changes.current_key = desired.clone();
            self.changes.selected = 0;
            self.diff.reset_view_offsets();
        }
        let Some(key) = desired else {
            return;
        };
        if !store.details.contains(&key) {
            if self.requested_details.as_ref() != Some(&key) {
                self.requested_details = Some(key.clone());
                effects.push(Effect::RequestCommitDetails {
                    slot: RequestSlotId::StashCommitDetails,
                    key,
                });
            }
            return;
        }
        if let Some(dk) = self.diff_key(store)
            && !store.diffs.contains(&dk)
            && self.diff.requested.as_ref() != Some(&dk)
        {
            self.diff.requested = Some(dk.clone());
            effects.push(Effect::RequestFileDiff {
                slot: RequestSlotId::StashFileDiff,
                key: dk,
            });
        }
    }
}
