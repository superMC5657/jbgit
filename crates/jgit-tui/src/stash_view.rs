//! `[3 Stash]` workbench state (`git4idea/stash/ui`): stash list selection
//! paired with a `DetailsPaneState` so the right pane reuses
//! `CommitChangesBrowser` + `DiffPreview`.

use std::sync::Arc;

use jgit_core::{DetailsKey, FileDiffKey, RepoSnapshot, StashEntry};

use crate::app::Effect;
use crate::log_view::DetailsPaneState;

#[derive(Debug)]
pub struct StashTabState {
    pub selected: usize,
    pub details: DetailsPaneState,
    pub requested_details: Option<DetailsKey>,
}

impl Default for StashTabState {
    fn default() -> Self {
        Self {
            selected: 0,
            details: DetailsPaneState {
                diff_preview: true,
                ..Default::default()
            },
            requested_details: None,
        }
    }
}

impl StashTabState {
    pub fn selected_entry<'a>(&self, stashes: &'a [StashEntry]) -> Option<&'a StashEntry> {
        stashes.get(self.selected.min(stashes.len().saturating_sub(1)))
    }

    pub fn details_key(&self, stashes: &[StashEntry]) -> Option<DetailsKey> {
        self.selected_entry(stashes)
            .map(|s| DetailsKey::single(s.oid))
    }

    pub fn changes_len(&self) -> usize {
        self.details.change_rows().len()
    }

    pub fn diff_key(&self) -> Option<FileDiffKey> {
        self.details.desired_diff_key()
    }

    pub fn reconcile_requests(
        &mut self,
        snap: Option<&Arc<RepoSnapshot>>,
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
            self.details.current_key = None;
            return;
        }
        self.selected = self.selected.min(snap.stashes.len() - 1);
        let desired = self.details_key(&snap.stashes);
        if desired != self.details.current_key {
            self.details.current_key = desired.clone();
            self.details.changes_selected = 0;
            self.details.diff_scroll = 0;
        }
        let Some(key) = desired else {
            return;
        };
        if !self.details.cache.contains(&key) {
            if self.requested_details.as_ref() != Some(&key) {
                self.requested_details = Some(key.clone());
                effects.push(Effect::RequestCommitDetails(key));
            }
            return;
        }
        if let Some(dk) = self.diff_key()
            && !self.details.diff_cache.contains(&dk)
            && self.details.requested_diff.as_ref() != Some(&dk)
        {
            self.details.requested_diff = Some(dk.clone());
            effects.push(Effect::RequestFileDiff(dk));
        }
    }
}
