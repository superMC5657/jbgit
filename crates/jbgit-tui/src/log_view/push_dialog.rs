use std::collections::HashSet;
use std::sync::Arc;
use std::time::Instant;

use jbgit_core::{
    CommitSummary, DetailsKey, DiffContext, FileChange, FileDiffKey, GitOp, Oid, OutgoingBundle,
    OutgoingKey, PushForce, PushTagMode, RepoSnapshot, fuzzy_indices,
    matches_protected_branch_patterns, natural_cmp, validate_ref_name_typed,
};

use crate::app::ScrollViewport;
use crate::diff_session::{DiffCacheStore, DiffSession};
use crate::i18n::I18n;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum PushDialogFocus {
    Target,
    #[default]
    Commits,
    Files,
}

#[derive(Debug)]
pub struct PushDialogState {
    pub branch: String,
    pub remotes: Vec<String>,
    pub remote_idx: usize,
    pub remote_branch: String,
    pub editing_target: bool,
    pub target_draft: String,
    pub completion_selected: usize,
    pub picking_remote: bool,
    pub up_to: Option<Oid>,
    pub tags: PushTagMode,
    pub force: PushForce,
    pub set_upstream: bool,
    pub focus: PushDialogFocus,
    pub loading: bool,
    pub error: Option<String>,
    pub outgoing: Option<Arc<OutgoingBundle>>,
    pub selected_commit: usize,
    pub commit_scroll: ScrollViewport,
    pub selected_file: usize,
    pub file_scroll: ScrollViewport,
    pub show_diff: bool,
    pub diff: DiffSession,
    pub details_requested: Option<DetailsKey>,
    pub details_pending: Option<(DetailsKey, Instant)>,
}

impl PushDialogState {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        branch: String,
        remotes: Vec<String>,
        remote_idx: usize,
        remote_branch: String,
        set_upstream: bool,
        up_to: Option<Oid>,
        diff_context: DiffContext,
    ) -> Self {
        Self {
            branch,
            remotes,
            remote_idx,
            remote_branch,
            editing_target: false,
            target_draft: String::new(),
            completion_selected: 0,
            picking_remote: false,
            up_to,
            tags: PushTagMode::None,
            force: PushForce::None,
            set_upstream,
            focus: PushDialogFocus::Commits,
            loading: true,
            error: None,
            outgoing: None,
            selected_commit: 0,
            commit_scroll: ScrollViewport::new(),
            selected_file: 0,
            file_scroll: ScrollViewport::new(),
            show_diff: false,
            diff: DiffSession::new(diff_context),
            details_requested: None,
            details_pending: None,
        }
    }

    pub fn remote(&self) -> &str {
        self.remotes
            .get(self.remote_idx)
            .map_or("", String::as_str)
    }

    pub fn outgoing_key(&self) -> OutgoingKey {
        OutgoingKey {
            branch: self.branch.clone(),
            remote: self.remote().to_string(),
            remote_branch: self.remote_branch.clone(),
            up_to: self.up_to,
        }
    }

    /// Commits queued to be pushed (`up_to` truncates newer commits when present).
    pub fn visible_commits(&self) -> &[CommitSummary] {
        let Some(bundle) = self.outgoing.as_deref() else {
            return &[];
        };
        let all = bundle.commits.as_slice();
        if let Some(target) = self.up_to
            && bundle.key.up_to.is_none()
            && let Some(pos) = all.iter().position(|c| c.oid == target)
        {
            return &all[pos..];
        }
        all
    }

    pub fn selected_commit_summary(&self) -> Option<&CommitSummary> {
        let commits = self.visible_commits();
        commits.get(self.selected_commit)
    }

    /// Completion candidates matching `target_draft` (current remote's branches first).
    pub fn completion_candidates(&self, snap: Option<&RepoSnapshot>) -> Vec<String> {
        let Some(snap) = snap else {
            return Vec::new();
        };
        let active_remote = self.remote();
        let mut active_branches = Vec::new();
        let mut other_branches = Vec::new();
        let mut seen = HashSet::new();

        for r in &snap.refs {
            if r.kind != jbgit_core::RefKind::RemoteBranch {
                continue;
            }
            let Some((rm, short)) = snap.split_tracking(&r.name) else {
                continue;
            };
            if rm == active_remote {
                if seen.insert(short.to_string()) {
                    active_branches.push(short.to_string());
                }
            } else {
                other_branches.push(short.to_string());
            }
        }
        active_branches.sort_by(|a, b| match (a == &self.branch, b == &self.branch) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => natural_cmp(a, b),
        });
        other_branches.sort_by(|a, b| natural_cmp(a, b));
        let mut ordered = active_branches;
        for name in other_branches {
            if seen.insert(name.clone()) {
                ordered.push(name);
            }
        }

        let query = self.target_draft.trim();
        if query.is_empty() {
            return ordered;
        }
        let mut scored: Vec<(u32, usize, String)> = ordered
            .into_iter()
            .enumerate()
            .filter_map(|(pos, cand)| fuzzy_indices(&cand, query).map(|(score, _)| (score, pos, cand)))
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        scored.into_iter().map(|(_, _, c)| c).collect()
    }

    pub fn target_validation_error(&self, i18n: &I18n) -> Option<String> {
        let candidate = if self.editing_target {
            self.target_draft.trim()
        } else {
            self.remote_branch.trim()
        };
        validate_ref_name_typed(candidate, &[])
            .err()
            .map(|err| match err {
                jbgit_core::RefNameError::Empty => i18n.t_fmt(
                    "validation.empty",
                    &[("what", i18n.t("validation.what.branch"))],
                ),
                _ => i18n
                    .validate_ref_name(candidate, &[], "Branch")
                    .err()
                    .unwrap_or_default(),
            })
    }

    pub fn is_force_blocked(&self, protected_patterns: &[String]) -> bool {
        self.force.is_force()
            && matches_protected_branch_patterns(self.remote_branch.trim(), protected_patterns)
    }

    pub fn can_push(&self, protected_patterns: &[String], i18n: &I18n) -> Result<(), String> {
        if self.editing_target {
            return Err(i18n.t("dialog.push_full.finish_editing_target").to_string());
        }
        if self.remote().is_empty() {
            return Err(i18n.t("menu.branch.no_remote_configured").to_string());
        }
        if let Some(err) = self.target_validation_error(i18n) {
            return Err(err);
        }
        if self.is_force_blocked(protected_patterns) {
            return Err(i18n.t_fmt(
                "dialog.push_full.protected_force_blocked",
                &[("branch", self.remote_branch.trim())],
            ));
        }
        Ok(())
    }

    pub fn build_push_op(&self) -> GitOp {
        GitOp::Push {
            branch: self.branch.clone(),
            remote: self.remote().to_string(),
            remote_branch: self.remote_branch.trim().to_string(),
            set_upstream: self.set_upstream,
            force: self.force,
            tags: self.tags,
            up_to: self.up_to,
        }
    }

    pub fn desired_details_key(&self) -> Option<DetailsKey> {
        let commit = self.selected_commit_summary()?;
        Some(DetailsKey::single(commit.oid))
    }

    pub fn selected_file_changes<'a>(&self, caches: &'a DiffCacheStore) -> &'a [FileChange] {
        let Some(key) = self.desired_details_key() else {
            return &[];
        };
        caches
            .peek_details(&key)
            .map_or(&[], |b| b.changes.as_slice())
    }

    pub fn selected_file_change<'a>(&self, caches: &'a DiffCacheStore) -> Option<&'a FileChange> {
        self.selected_file_changes(caches).get(self.selected_file)
    }

    pub fn desired_diff_key(&self, caches: &DiffCacheStore) -> Option<FileDiffKey> {
        if !self.show_diff {
            return None;
        }
        let details = self.desired_details_key()?;
        let change = self.selected_file_change(caches)?;
        Some(FileDiffKey {
            details,
            path: change.path.clone(),
            old_path: change.old_path.clone(),
            context: self.diff.context,
            whitespace: self.diff.whitespace,
        })
    }

    pub fn next_deadline(&self) -> Option<Instant> {
        let d = self.details_pending.as_ref().map(|(_, t)| *t);
        let f = self.diff.pending.as_ref().map(|(_, t)| *t);
        match (d, f) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (Some(a), None) | (None, Some(a)) => Some(a),
            (None, None) => None,
        }
    }
}
