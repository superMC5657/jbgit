use std::sync::Arc;
use std::time::Instant;

use jbgit_core::{ModalButton, WorkspaceTab};
use jbgit_engine::EngineEvent;

use crate::log_view::PathMatches;

use super::{AppState, Effect, ModalLayer, SyntaxJobKey, UiAsyncEvent};

impl AppState {
    pub fn apply_ui_async_event(&mut self, event: UiAsyncEvent) -> Vec<Effect> {
        let mut effects = Vec::new();
        match event {
            UiAsyncEvent::ContainingBranches { oid, token, names } => {
                if self.details.resolve_containing_branches(oid, &token, names) {
                    self.needs_redraw = true;
                }
            }
            UiAsyncEvent::LogGraphRebuilt(rebuilt) => {
                if self.log_graph.apply_rebuilt(*rebuilt) {
                    if let Some(err) = &self.log_graph.filter_error {
                        self.status_notice = Some(
                            self.i18n
                                .t_fmt("toast.filter_error", &[("err", &err.to_string())]),
                        );
                    }
                    self.needs_redraw = true;
                    self.reconcile_log_requests(Instant::now(), &mut effects);
                }
            }
            UiAsyncEvent::SyntaxHighlighted {
                key: SyntaxJobKey::File(fk),
                epoch,
                highlighted,
            } => {
                if !self.file_syntax.finish(&fk, epoch) {
                    return effects;
                }
                let mut updated = false;
                if self.details.diff_cache.contains(&fk) {
                    self.details
                        .diff_syntax
                        .insert(fk.clone(), Arc::clone(&highlighted));
                    updated = true;
                }
                if self.stash.details.diff_cache.contains(&fk) {
                    self.stash.details.diff_syntax.insert(fk, highlighted);
                    updated = true;
                }
                if updated {
                    self.needs_redraw = true;
                }
            }
            UiAsyncEvent::SyntaxHighlighted {
                key: SyntaxJobKey::Stage(sk),
                epoch,
                highlighted,
            } => {
                if !self.stage_syntax.finish(&sk, epoch) {
                    return effects;
                }
                if self.stage_workbench.diff_cache.contains(&sk) {
                    self.stage_workbench
                        .diff_syntax_cache
                        .insert(sk.clone(), Arc::clone(&highlighted));
                }
                if self.stage_workbench.current_diff_key.as_ref() == Some(&sk) {
                    self.stage_workbench.diff_syntax = Some(highlighted);
                    self.needs_redraw = true;
                }
            }
        }
        effects
    }

    pub fn apply_engine_event(&mut self, event: EngineEvent) -> Vec<Effect> {
        let mut effects = Vec::new();
        self.needs_redraw = true;

        let event = match self.apply_stage_engine_event(event, &mut effects) {
            None => {
                self.reconcile_log_requests(Instant::now(), &mut effects);
                self.reconcile_stage_requests(Instant::now(), &mut effects);
                return effects;
            }
            Some(ev) => ev,
        };

        match event {
            EngineEvent::RepoSnapshotLoaded { snapshot, epoch } => {
                if epoch < self.latest_repo_epoch {
                    return effects;
                }
                self.latest_repo_epoch = epoch;
                self.loading_snapshot = false;
                if self.branches.settings_path.is_none()
                    && !snapshot.common_dir.as_os_str().is_empty()
                {
                    self.branches
                        .load_settings(snapshot.common_dir.join("jbgit-branches-panel"));
                    self.stage_workbench
                        .load_history(snapshot.common_dir.join("jbgit-commit-history"));
                    self.stage_workbench
                        .load_draft(snapshot.common_dir.join("jbgit-commit-draft"));
                }
                let prev_head = self.repo_snapshot.as_ref().and_then(|s| s.head_oid);
                let head_changed = prev_head.is_some() && prev_head != snapshot.head_oid;
                if head_changed && !self.stage_workbench.amend {
                    self.stage_workbench.amend_preview = None;
                }
                self.log_graph.on_snapshot_loaded();
                self.details.reset_containing();
                self.repo_snapshot = Some(snapshot);
                if self.stage_workbench.amend {
                    effects.push(Effect::RequestAmendPreview);
                }
            }
            EngineEvent::RefAheadBehindBackfilled { epoch, updates } => {
                if epoch == self.latest_repo_epoch {
                    self.branches.apply_ahead_behind_backfill(&updates);
                    self.needs_redraw = true;
                }
            }
            EngineEvent::CommitDetailsLoaded { bundle, epoch } => {
                self.latest_commit_details_epoch = self.latest_commit_details_epoch.max(epoch);
                if self.stash.requested_details.as_ref() == Some(&bundle.key) {
                    self.stash.requested_details = None;
                }
                if self.stash.details.current_key.as_ref() == Some(&bundle.key) {
                    self.stash
                        .details
                        .cache
                        .insert(bundle.key.clone(), Arc::clone(&bundle));
                }
                self.details.cache.insert(bundle.key.clone(), bundle);
            }
            EngineEvent::CommitSignatureVerified { oid, status } => {
                self.details.signature_overrides.insert(oid, status);
                self.needs_redraw = true;
            }
            EngineEvent::FileDiffLoaded { diff, epoch } => {
                let log_wants = self.details.diff_visible()
                    && self.details.desired_diff_key().as_ref() == Some(&diff.key);
                let stash_wants = self.active_tab == WorkspaceTab::Stash
                    && self.stash.details.desired_diff_key().as_ref() == Some(&diff.key);
                let fh_wants = self
                    .file_history
                    .as_ref()
                    .and_then(|fh| fh.desired_diff_key())
                    .as_ref()
                    == Some(&diff.key);
                let is_wanted = log_wants || stash_wants || fh_wants;
                if epoch < self.latest_file_diff_epoch && !is_wanted {
                    return effects;
                }
                self.latest_file_diff_epoch = self.latest_file_diff_epoch.max(epoch);
                if self.details.requested_diff.as_ref() == Some(&diff.key) {
                    self.details.requested_diff = None;
                }
                if self.stash.details.requested_diff.as_ref() == Some(&diff.key) {
                    self.stash.details.requested_diff = None;
                }
                if let Some(fh) = &mut self.file_history
                    && fh.requested_diff.as_ref() == Some(&diff.key)
                {
                    fh.requested_diff = None;
                }
                if stash_wants || self.stash.details.desired_diff_key().as_ref() == Some(&diff.key)
                {
                    self.stash
                        .details
                        .diff_cache
                        .insert(diff.key.clone(), Arc::clone(&diff));
                }
                self.details
                    .diff_cache
                    .insert(diff.key.clone(), Arc::clone(&diff));
                if is_wanted && self.details.diff_syntax.peek(&diff.key).is_none() {
                    self.schedule_file_syntax(diff, &mut effects);
                }
            }
            EngineEvent::FileHistoryLoaded { bundle, epoch } => {
                if epoch < self.latest_file_history_epoch {
                    return effects;
                }
                self.latest_file_history_epoch = epoch;
                let desired_after_load = if let Some(fh) = &mut self.file_history
                    && fh.key == bundle.key
                {
                    fh.loading = false;
                    fh.error = None;
                    fh.anchor = None;
                    fh.diff_scroll = 0;
                    fh.pending_diff = None;
                    fh.selected = fh
                        .key
                        .start_oid
                        .and_then(|start| bundle.entries.iter().position(|e| e.oid == start))
                        .unwrap_or(0);
                    fh.bundle = Some(bundle);
                    fh.desired_diff_key()
                } else {
                    None
                };
                if let Some(key) = desired_after_load {
                    if let Some(cached) = self.details.diff_cache.get(&key).cloned() {
                        if self.details.diff_syntax.get(&key).is_none() {
                            self.schedule_file_syntax(cached, &mut effects);
                        }
                    } else if let Some(fh) = &mut self.file_history
                        && fh.requested_diff.as_ref() != Some(&key)
                    {
                        fh.requested_diff = Some(key.clone());
                        effects.push(Effect::RequestFileDiff(key));
                    }
                }
            }
            EngineEvent::PathMatchesLoaded {
                paths,
                matches,
                epoch,
            } => {
                if epoch < self.latest_path_epoch {
                    return effects;
                }
                self.latest_path_epoch = epoch;
                if let Some(req) = self.log_filter.path_requested.take()
                    && req.paths == paths
                {
                    self.log_filter.path_matches = Some(PathMatches {
                        paths,
                        snapshot: req.snapshot,
                        oids: matches,
                    });
                    self.on_filter_changed();
                }
            }
            EngineEvent::StageStatusLoaded { .. }
            | EngineEvent::StageFileDiffLoaded { .. }
            | EngineEvent::AmendPreviewLoaded { .. }
            | EngineEvent::CommitPrecheckLoaded { .. }
            | EngineEvent::CommitCompleted(_)
            | EngineEvent::UndoCommitCompleted { .. } => {
                unreachable!("handled by apply_stage_engine_event")
            }
            EngineEvent::ConsoleStarted(entry) => {
                self.console_entries.insert(0, entry);
                self.console_selected_idx = 0;
                self.console_output_scroll = 0;
            }
            EngineEvent::ConsoleOutput { id, stream, line } => {
                if let Some(entry) = self.console_entries.iter_mut().find(|e| e.id == id) {
                    entry.push_line(stream, line);
                }
            }
            EngineEvent::ConsoleFinished {
                id,
                exit_code,
                duration_ms,
            } => {
                if let Some(entry) = self.console_entries.iter_mut().find(|e| e.id == id) {
                    entry.exit_code = Some(exit_code);
                    entry.duration_ms = Some(duration_ms);
                }
            }
            EngineEvent::IndexLockDetected {
                lock_path,
                message,
                retry_args,
                retry_stdin,
                retry_is_read_only,
                retry_refresh_after,
            } => {
                let path_s = lock_path.display().to_string();
                self.modal_stack.push(ModalLayer::CriticalDialog {
                    title: self.i18n.t("dialog.index_lock.title").to_string(),
                    body: self.i18n.t_fmt(
                        "dialog.index_lock.body",
                        &[("path", &path_s), ("message", &message)],
                    ),
                    confirm_label: self.i18n.t("common.retry").to_string(),
                    cancel_label: self.i18n.t("common.dismiss").to_string(),
                    selected_button: ModalButton::Confirm,
                    confirm_effect: Some(Effect::RunCli {
                        args: retry_args,
                        stdin_payload: retry_stdin,
                        is_read_only: retry_is_read_only,
                        refresh_after: retry_refresh_after,
                    }),
                });
            }
            EngineEvent::ExternalFsChanged(scope) => {
                effects.push(Effect::RefreshScope(scope));
            }
            EngineEvent::GitOpFinished(outcome) => {
                self.git_ops_running = self.git_ops_running.saturating_sub(1);
                if outcome.success {
                    self.stage_workbench.invalidate_all_diff_caches();
                    self.stage_workbench.pending_select_immediate = true;
                }
                self.apply_git_outcome(outcome, &mut effects);
            }
            EngineEvent::ConflictDocumentLoaded { doc } => {
                if let Some(cm) = &mut self.conflict_merge
                    && cm.path == doc.path
                {
                    cm.selected_block = 0;
                    cm.scroll = 0;
                    cm.doc = Some(doc);
                }
            }
            EngineEvent::Error(msg) => {
                self.loading_snapshot = false;
                self.details.requested_diff = None;
                self.stage_workbench.requested_diff = None;
                self.stage_workbench.commit_in_progress = false;
                if let Some(fh) = &mut self.file_history {
                    fh.loading = false;
                    fh.requested_diff = None;
                    if fh.bundle.is_none() {
                        fh.error = Some(msg.clone());
                    }
                }
                self.status_notice = Some(self.i18n.t_fmt("toast.engine_error", &[("msg", &msg)]));
            }
        }

        self.reconcile_all_requests(&mut effects);
        effects
    }
}
