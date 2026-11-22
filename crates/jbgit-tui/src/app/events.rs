use std::sync::Arc;

use jbgit_core::{RequestSlotId, WorkspaceTab};
use jbgit_engine::EngineEvent;

use crate::dialogs::{ChoiceDialog, Dialog};
use crate::log_view::PathMatches;

use super::{Action, AppState, Effect, ModalLayer, SyntaxJobKey, UiAsyncEvent};

impl AppState {
    pub fn apply_ui_async_event(&mut self, event: UiAsyncEvent) -> Vec<Effect> {
        let mut effects = Vec::new();
        match event {
            UiAsyncEvent::ContainingBranches { oid, token, names } => {
                if self.log.details.resolve_containing_branches(oid, &token, names) {
                    self.needs_redraw = true;
                }
            }
            UiAsyncEvent::LogGraphRebuilt(rebuilt) => {
                if self.log.graph.apply_rebuilt(*rebuilt) {
                    if let Some(err) = &self.log.graph.filter_error {
                        self.status_notice = Some(
                            self.i18n
                                .t_fmt("toast.filter_error", &[("err", &err.to_string())]),
                        );
                    }
                    self.needs_redraw = true;
                    let now = self.now();
                    self.reconcile_log_requests(now, &mut effects);
                }
            }
            UiAsyncEvent::SyntaxHighlighted {
                key: SyntaxJobKey::File(fk),
                epoch,
                highlighted,
            } => {
                if !self.file_syntax.finish_cancelable(&fk, epoch) {
                    return effects;
                }
                if self.diff_caches.diffs.contains(&fk) {
                    self.diff_caches.insert_syntax(fk, highlighted);
                    self.needs_redraw = true;
                }
            }
            UiAsyncEvent::SyntaxHighlighted {
                key: SyntaxJobKey::Stage(sk),
                epoch,
                highlighted,
            } => {
                if self
                    .stage_workbench
                    .apply_syntax_highlighted(sk, epoch, highlighted, &mut self.stage_syntax)
                {
                    self.needs_redraw = true;
                }
            }
        }
        effects
    }

    pub fn apply_engine_event(&mut self, event: EngineEvent) -> Vec<Effect> {
        let mut effects = Vec::new();
        self.needs_redraw = true;

        let now = self.now();
        let snap_ref = self.repo_snapshot.clone();
        let default_remote = self.default_remote();
        let syntax_enabled = self.config.jbgit.diff.syntax_highlight;
        let mut modals = Vec::new();
        let event = {
            let mut cctx = crate::stage_view::CommitContext {
                snap: snap_ref.as_deref(),
                default_remote: &default_remote,
                i18n: &self.i18n,
                now,
                focused_pane: &mut self.focused_pane,
                loading_snapshot: &mut self.loading_snapshot,
                status_notice: &mut self.status_notice,
                effects: &mut effects,
                modals: &mut modals,
            };
            self.stage_workbench.apply_engine_event(
                event,
                &mut self.request_slots,
                &mut self.stage_syntax,
                syntax_enabled,
                &mut cctx,
            )
        };
        for m in modals {
            self.push_modal(m);
        }
        let event = match event {
            None => {
                self.reconcile_all_requests(&mut effects);
                return effects;
            }
            Some(ev) => ev,
        };

        match event {
            EngineEvent::RepoSnapshotLoaded { snapshot, epoch } => {
                if !self.request_slots.accept(RequestSlotId::RepoSnapshot, epoch) {
                    return effects;
                }
                self.loading_snapshot = false;
                let prev_head = self.repo_snapshot.as_ref().and_then(|s| s.head_oid);
                let head_changed = prev_head.is_some() && prev_head != snapshot.head_oid;
                if self.log.branches.settings_path.is_none()
                    && !snapshot.common_dir.as_os_str().is_empty()
                {
                    self.log
                        .branches
                        .load_settings(snapshot.common_dir.join("jbgit-branches-panel"));
                }
                self.stage_workbench
                    .on_repo_snapshot_loaded(&snapshot, head_changed, &mut effects);
                self.log.graph.on_snapshot_loaded();
                self.log.details.reset_containing();
                self.repo_snapshot = Some(snapshot);
            }
            EngineEvent::RefAheadBehindBackfilled { epoch, updates } => {
                if self
                    .request_slots
                    .matches_latest(RequestSlotId::RepoSnapshot, epoch)
                {
                    self.log.branches.apply_ahead_behind_backfill(&updates);
                    self.needs_redraw = true;
                }
            }
            EngineEvent::CommitDetailsLoaded {
                slot,
                bundle,
                epoch,
            } => {
                if !self.request_slots.accept(slot, epoch) {
                    return effects;
                }
                if slot == RequestSlotId::StashCommitDetails {
                    self.stash.on_details_loaded(&bundle.key);
                }
                self.diff_caches.insert_details(bundle);
            }
            EngineEvent::CommitSignatureVerified { oid, status } => {
                self.diff_caches.signatures.insert(oid, status);
                self.needs_redraw = true;
            }
            EngineEvent::FileDiffLoaded { slot, diff, epoch } => {
                if !self.request_slots.accept(slot, epoch) {
                    return effects;
                }
                match slot {
                    RequestSlotId::LogFileDiff => {
                        if self.log.details.diff.requested.as_ref() == Some(&diff.key) {
                            self.log.details.diff.requested = None;
                        }
                    }
                    RequestSlotId::StashFileDiff => {
                        self.stash.on_file_diff_loaded(&diff.key);
                    }
                    RequestSlotId::FileHistoryDiff => {
                        if let Some(fh) = self.modal_stack.file_history_mut()
                            && fh.diff.requested.as_ref() == Some(&diff.key)
                        {
                            fh.diff.requested = None;
                        }
                    }
                    _ => {}
                }
                let log_wants = self.log.details.diff_visible()
                    && self.log.details.desired_diff_key(&self.diff_caches).as_ref()
                        == Some(&diff.key);
                let stash_wants = self.active_tab == WorkspaceTab::Stash
                    && self.stash.diff_key(&self.diff_caches).as_ref() == Some(&diff.key);
                let fh_wants = self
                    .modal_stack
                    .file_history()
                    .and_then(|fh| fh.desired_diff_key())
                    .as_ref()
                    == Some(&diff.key);
                let is_wanted = log_wants || stash_wants || fh_wants;
                self.diff_caches.insert_diff(Arc::clone(&diff));
                if is_wanted && self.diff_caches.peek_syntax(&diff.key).is_none() {
                    self.schedule_file_syntax(diff, &mut effects);
                }
            }
            EngineEvent::FileHistoryLoaded { bundle, epoch } => {
                if !self
                    .request_slots
                    .accept(RequestSlotId::FileHistoryList, epoch)
                {
                    return effects;
                }
                let desired_after_load = if let Some(fh) = self.modal_stack.file_history_mut()
                    && fh.key == bundle.key
                {
                    fh.loading = false;
                    fh.error = None;
                    fh.anchor = None;
                    fh.diff.scroll.reset();
                    fh.diff.pending = None;
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
                    if let Some(cached) = self.diff_caches.get_diff(&key) {
                        if self.diff_caches.get_syntax(&key).is_none() {
                            self.schedule_file_syntax(cached, &mut effects);
                        }
                    } else if let Some(fh) = self.modal_stack.file_history_mut()
                        && fh.diff.requested.as_ref() != Some(&key)
                    {
                        fh.diff.requested = Some(key.clone());
                        effects.push(Effect::RequestFileDiff {
                            slot: RequestSlotId::FileHistoryDiff,
                            key,
                        });
                    }
                }
            }
            EngineEvent::PathMatchesLoaded {
                paths,
                matches,
                epoch,
            } => {
                if !self.request_slots.accept(RequestSlotId::PathFilter, epoch) {
                    return effects;
                }
                if let Some(req) = self.log.filter.path_requested.take()
                    && req.paths == paths
                {
                    self.log.filter.path_matches = Some(PathMatches {
                        paths,
                        snapshot: req.snapshot,
                        oids: matches,
                    });
                    self.log.on_filter_changed(
                        self.repo_snapshot.as_ref(),
                        &self.branch_colors,
                        self.async_graph_enabled,
                        &self.i18n,
                        &mut self.status_notice,
                    );
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
                self.console.push_started(entry);
            }
            EngineEvent::ConsoleOutput { id, stream, line } => {
                self.console.push_output(id, stream, line);
            }
            EngineEvent::ConsoleFinished {
                id,
                exit_code,
                duration_ms,
            } => {
                self.console.finish_command(id, exit_code, duration_ms);
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
                let body: Vec<String> = self
                    .i18n
                    .t_fmt(
                        "dialog.index_lock.body",
                        &[("path", &path_s), ("message", &message)],
                    )
                    .lines()
                    .map(ToString::to_string)
                    .collect();
                self.push_modal(ModalLayer::Dialog(Box::new(
                    Dialog::Choice(ChoiceDialog::confirm_with_default(
                        self.i18n.t("dialog.index_lock.title"),
                        body,
                        self.i18n.t("common.retry"),
                        self.i18n.t("common.dismiss"),
                        true,
                        true,
                        vec![Action::RetryCliCommand {
                            args: retry_args,
                            stdin_payload: retry_stdin,
                            is_read_only: retry_is_read_only,
                            refresh_after: retry_refresh_after,
                        }],
                    )),
                )));
            }
            EngineEvent::ExternalFsChanged(scope) => {
                effects.push(Effect::RefreshScope(scope));
            }
            EngineEvent::GitOpFinished(outcome) => {
                self.git_ops_running = self.git_ops_running.saturating_sub(1);
                if outcome.success {
                    self.stage_workbench.on_git_op_succeeded();
                }
                self.apply_git_outcome(outcome, &mut effects);
            }
            EngineEvent::ConflictDocumentLoaded { doc, epoch } => {
                if !self
                    .request_slots
                    .accept(RequestSlotId::ConflictDocument, epoch)
                {
                    return effects;
                }
                if let Some(cm) = self.modal_stack.conflict_merge_mut()
                    && cm.path == doc.path
                {
                    cm.selected_block = 0;
                    cm.scroll.reset();
                    cm.doc = Some(doc);
                }
            }
            EngineEvent::Error(msg) => {
                self.loading_snapshot = false;
                self.log.details.diff.requested = None;
                self.stash.on_error();
                self.stage_workbench.on_engine_error();
                if let Some(fh) = self.modal_stack.file_history_mut() {
                    fh.loading = false;
                    fh.diff.requested = None;
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
