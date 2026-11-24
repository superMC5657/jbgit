use std::sync::Arc;

use jbgit_core::{GitOp, RequestSlotId, WorkspaceTab};
use jbgit_engine::{BlameFailureKind, EngineEvent};

use crate::dialogs::{ChoiceCheckbox, ChoiceDialog, Dialog};
use crate::log_view::{AnnotateError, PathMatches};

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
            UiAsyncEvent::Askpass(req) => self.enqueue_credential(req),
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
            UiAsyncEvent::SyntaxHighlighted {
                key: SyntaxJobKey::RangePatch { oid, path, range },
                epoch,
                highlighted,
            } => {
                if !self.range_syntax.finish_cancelable(&(oid, path, range), epoch) {
                    return effects;
                }
                for layer in &mut self.modal_stack {
                    if let ModalLayer::FileHistory(fh) = layer
                        && fh.uses_range_patch()
                        && fh.key.line_range == Some(range)
                        && fh.selected_entry().is_some_and(|e| e.oid == oid)
                    {
                        fh.range_syntax = Some((oid, Arc::clone(&highlighted)));
                        self.needs_redraw = true;
                    }
                }
            }
            UiAsyncEvent::TreeBlobSyntaxHighlighted {
                key,
                epoch,
                highlighted,
            } => {
                if !self.tree_syntax.finish_cancelable(&key, epoch) {
                    return effects;
                }
                if let Some(s) = self.modal_stack.tree_browser_mut()
                    && s.preview_requested.as_ref() == Some(&key)
                {
                    s.preview_syntax = Some(highlighted);
                    self.needs_redraw = true;
                }
            }
            UiAsyncEvent::BlameSyntaxHighlighted {
                key,
                epoch,
                highlighted,
            } => {
                if !self.blame_syntax.finish_cancelable(&key, epoch) {
                    return effects;
                }
                for layer in &mut self.modal_stack {
                    if let ModalLayer::Annotate(ann) = layer
                        && ann.key == key
                    {
                        ann.syntax = Some(Arc::clone(&highlighted));
                        self.needs_redraw = true;
                    }
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
        let is_stage_status = matches!(event, EngineEvent::StageStatusLoaded { .. });
        let prev_stage_ptr = self.stage_workbench.status().map(Arc::as_ptr);
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
                if is_stage_status
                    && self.stage_workbench.status().map(Arc::as_ptr) != prev_stage_ptr
                {
                    self.check_conflict_resolver_staleness();
                }
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
                self.log.sig_requested.clear();
                self.log.details.reset_containing();
                self.repo_snapshot = Some(snapshot);
                self.apply_pending_remote_selection();
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
                if slot == RequestSlotId::PushCommitDetails
                    && let Some(pd) = self.modal_stack.push_dialog_mut()
                {
                    if pd.details_requested.as_ref() == Some(&bundle.key) {
                        pd.details_requested = None;
                    }
                    if pd.desired_details_key().as_ref() == Some(&bundle.key) {
                        pd.selected_file = pd.selected_file.min(bundle.changes.len().saturating_sub(1));
                    }
                }
                self.diff_caches.insert_details(bundle);
            }
            EngineEvent::CommitSignatureVerified { oid, signature } => {
                self.diff_caches.signatures.insert(oid, signature);
                self.needs_redraw = true;
            }
            EngineEvent::CommitSignaturesLoaded { signatures, epoch } => {
                if !self
                    .request_slots
                    .accept(RequestSlotId::LogSignatures, epoch)
                {
                    return effects;
                }
                self.log.sig_requested.clear();
                for (oid, signature) in signatures {
                    self.diff_caches.signatures.insert(oid, signature);
                }
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
                    RequestSlotId::AnnotateDiff => {
                        if let Some(ann) = self.modal_stack.annotate_mut()
                            && ann.diff.requested.as_ref() == Some(&diff.key)
                        {
                            ann.diff.requested = None;
                        }
                    }
                    RequestSlotId::PushFileDiff => {
                        if let Some(pd) = self.modal_stack.push_dialog_mut()
                            && pd.diff.requested.as_ref() == Some(&diff.key)
                        {
                            pd.diff.requested = None;
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
                let head_oid = self.repo_snapshot.as_ref().and_then(|s| s.head_oid);
                let ann_wants = self
                    .modal_stack
                    .annotate()
                    .and_then(|ann| ann.desired_diff_key(head_oid))
                    .as_ref()
                    == Some(&diff.key);
                let push_wants = self
                    .modal_stack
                    .push_dialog()
                    .and_then(|pd| pd.desired_diff_key(&self.diff_caches))
                    .as_ref()
                    == Some(&diff.key);
                let is_wanted = log_wants || stash_wants || fh_wants || ann_wants || push_wants;
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
                let mut range_syntax_job: Option<(jbgit_core::Oid, (u32, u32), Arc<jbgit_core::FileDiff>)> = None;
                let desired_after_load = if let Some(fh) = self.modal_stack.file_history_mut()
                    && fh.key == bundle.key
                {
                    fh.loading = false;
                    fh.error = None;
                    fh.anchor = None;
                    fh.range_diff = None;
                    fh.range_syntax = None;
                    fh.diff.scroll.reset();
                    fh.diff.pending = None;
                    fh.selected = fh
                        .key
                        .start_oid
                        .and_then(|start| bundle.entries.iter().position(|e| e.oid == start))
                        .unwrap_or(0);
                    fh.bundle = Some(bundle);
                    if fh.uses_range_patch() {
                        if let (Some(range), Some(oid)) = (
                            fh.key.line_range,
                            fh.selected_entry().map(|e| e.oid),
                        ) && let Some(diff) = fh.ensure_range_diff()
                        {
                            range_syntax_job = Some((oid, range, diff));
                        }
                        None
                    } else {
                        fh.desired_diff_key()
                    }
                } else {
                    None
                };
                if let Some((oid, range, diff)) = range_syntax_job {
                    self.schedule_range_syntax(oid, range, diff, &mut effects);
                }
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
            EngineEvent::BlameLoaded { bundle, epoch } => {
                if !self.request_slots.accept(RequestSlotId::Annotate, epoch) {
                    return effects;
                }
                let mut syntax_bundle: Option<Arc<jbgit_core::BlameBundle>> = None;
                if let Some(ann) = self.modal_stack.annotate_mut()
                    && ann.key == bundle.key
                {
                    ann.loading = false;
                    ann.error = None;
                    let focus_line = ann.pending_focus_line.take();
                    let target_cursor = focus_line
                        .and_then(|ln| {
                            bundle
                                .lines
                                .iter()
                                .position(|l| l.final_line == ln)
                                .or_else(|| usize::try_from(ln.saturating_sub(1)).ok())
                        })
                        .unwrap_or(ann.cursor);
                    ann.bundle = Some(Arc::clone(&bundle));
                    ann.syntax = None;
                    ann.visual_anchor = None;
                    ann.cursor = target_cursor.min(bundle.lines.len().saturating_sub(1));
                    ann.ensure_cursor_visible();
                    syntax_bundle = Some(bundle);
                }
                if let Some(bundle) = syntax_bundle {
                    self.schedule_blame_syntax(bundle, &mut effects);
                }
            }
            EngineEvent::BlameFailed {
                key,
                kind,
                message,
                epoch,
            } => {
                if !self.request_slots.accept(RequestSlotId::Annotate, epoch) {
                    return effects;
                }
                if let Some(ann) = self.modal_stack.annotate_mut()
                    && ann.key == key
                {
                    ann.loading = false;
                    ann.bundle = None;
                    ann.syntax = None;
                    ann.pending_focus_line = None;
                    ann.error = Some(match kind {
                        BlameFailureKind::TooLarge { limit } => AnnotateError::TooLarge { limit },
                        BlameFailureKind::Binary => AnnotateError::Binary,
                        BlameFailureKind::Other => AnnotateError::Message(message),
                    });
                }
            }
            EngineEvent::RemoteTagChecked {
                remote,
                tag,
                exists,
                epoch,
            } => {
                if !self
                    .request_slots
                    .accept(RequestSlotId::RemoteTagCheck, epoch)
                {
                    return effects;
                }
                if exists == Some(true)
                    && let Some(ModalLayer::Dialog(dlg)) = self.modal_stack.last_mut()
                    && let Dialog::Choice(choice) = dlg.as_mut()
                    && let Some(probe) = choice.remote_tag_probe.clone()
                    && probe.remote == remote
                    && probe.name == tag
                {
                    choice.checkbox = Some(ChoiceCheckbox {
                        label: self
                            .i18n
                            .t_fmt("dialog.delete_tag.also_remote", &[("remote", &remote)]),
                        checked: false,
                        checked_actions: vec![Action::RunGitOp(GitOp::Sequence(vec![
                            GitOp::DeleteTag {
                                name: probe.name.clone(),
                                tip: probe.tip,
                            },
                            GitOp::DeleteRemoteTag {
                                remote: probe.remote,
                                name: probe.name,
                            },
                        ]))],
                    });
                }
            }
            EngineEvent::PatchChecked { check, epoch } => self.on_patch_checked(check, epoch),
            EngineEvent::TreeLoaded { listing, epoch } => {
                self.on_tree_loaded(listing, epoch, &mut effects);
            }
            EngineEvent::TreeLoadFailed {
                key,
                message,
                epoch,
            } => self.on_tree_load_failed(&key, message, epoch, &mut effects),
            EngineEvent::TreeBlobLoaded { blob, epoch } => {
                self.on_tree_blob_loaded(blob, epoch, &mut effects);
            }
            EngineEvent::OutgoingLoaded { bundle, epoch } => {
                if !self
                    .request_slots
                    .accept(RequestSlotId::PushOutgoing, epoch)
                {
                    return effects;
                }
                if let Some(pd) = self.modal_stack.push_dialog_mut()
                    && pd.outgoing_key() == bundle.key
                {
                    pd.loading = false;
                    pd.error = None;
                    pd.outgoing = Some(bundle);
                    pd.selected_commit = 0;
                    pd.commit_scroll.reset();
                    pd.selected_file = 0;
                    pd.file_scroll.reset();
                    pd.show_diff = false;
                    pd.details_requested = None;
                    pd.details_pending = None;
                    pd.diff.requested = None;
                    pd.diff.pending = None;
                    pd.diff.scroll.reset();
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
                    cm.h_scroll = 0;
                    cm.show_base = false;
                    cm.inline_editor = None;
                    cm.externally_resolved = false;
                    cm.doc = Some(doc);
                }
            }
            EngineEvent::SpecialConflictLoaded { info, epoch } => {
                if !self
                    .request_slots
                    .accept(RequestSlotId::ConflictDocument, epoch)
                {
                    return effects;
                }
                if self
                    .modal_stack
                    .conflict_merge()
                    .is_some_and(|cm| cm.path == info.path)
                {
                    self.modal_stack.close_conflict_resolver();
                    self.open_special_conflict_dialog(info);
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
                if let Some(ann) = self.modal_stack.annotate_mut() {
                    ann.loading = false;
                    ann.diff.requested = None;
                    if ann.bundle.is_none() && ann.error.is_none() {
                        ann.error = Some(AnnotateError::Message(msg.clone()));
                    }
                }
                if let Some(pd) = self.modal_stack.push_dialog_mut() {
                    pd.loading = false;
                    pd.details_requested = None;
                    pd.diff.requested = None;
                    if pd.outgoing.is_none() && pd.error.is_none() {
                        pd.error = Some(msg.clone());
                    }
                }
                self.status_notice = Some(self.i18n.t_fmt("toast.engine_error", &[("msg", &msg)]));
            }
        }

        self.reconcile_all_requests(&mut effects);
        effects
    }

    fn check_conflict_resolver_staleness(&mut self) {
        let Some(status) = self.stage_workbench.status().cloned() else {
            return;
        };
        if let Some(cq) = self.modal_stack.conflict_queue_mut() {
            let is_reversed = cq.is_reversed;
            for entry in &mut cq.entries {
                entry.resolved = !status.conflicted.iter().any(|c| c.path == entry.path);
            }
            for c in &status.conflicted {
                if !cq.entries.iter().any(|e| e.path == c.path) {
                    let raw = status.conflict_labels.get(&c.path).map(String::as_str);
                    let (yours_status, theirs_status) =
                        jbgit_core::conflict_side_statuses(raw, is_reversed);
                    cq.entries.push(crate::app::ConflictQueueEntry {
                        path: c.path.clone(),
                        yours_status,
                        theirs_status,
                        resolved: false,
                    });
                    cq.completion_prompted = false;
                }
            }
        }
        let Some(cm) = self.modal_stack.conflict_merge_mut() else {
            return;
        };
        if cm.doc.is_none() {
            return;
        }
        let still_conflicted = status.conflicted.iter().any(|e| e.path == cm.path);
        if !still_conflicted && !cm.externally_resolved {
            cm.externally_resolved = true;
            let path = cm.path.clone();
            self.status_notice = Some(
                self.i18n
                    .t_fmt("toast.conflict_resolved_externally", &[("path", &path)]),
            );
        } else if still_conflicted {
            cm.externally_resolved = false;
        }
    }
}
