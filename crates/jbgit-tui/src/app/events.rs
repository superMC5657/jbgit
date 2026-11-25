use std::sync::Arc;

use jbgit_core::{GitOp, RequestSlotId, WorkspaceTab};
use jbgit_engine::{BlameFailureKind, EngineEvent};

use crate::dialogs::{ChoiceCheckbox, ChoiceDialog, Dialog};
use crate::log_view::{AnnotateError, PathMatches};

use super::{Action, AppState, Effect, ModalLayer, SyntaxJobKey, UiAsyncEvent};

impl AppState {
    pub fn apply_ui_async_event(&mut self, event: UiAsyncEvent) -> Vec<Effect> {
        let mut effects = Vec::new();
        let event = match event {
            UiAsyncEvent::TaskStamped { task_id, event } => match event.as_ref() {
                UiAsyncEvent::InitRepoFinished(_) | UiAsyncEvent::InitRepoFailed(_) => {
                    if self.active_init_task_id != Some(task_id) {
                        return effects;
                    }
                    self.active_init_task_id = None;
                    *event
                }
                UiAsyncEvent::CloneProgress(_) => {
                    if self.active_clone_task_id != Some(task_id) {
                        return effects;
                    }
                    *event
                }
                UiAsyncEvent::CloneFinished(_) => {
                    if self.active_clone_task_id != Some(task_id) {
                        return effects;
                    }
                    self.active_clone_task_id = None;
                    *event
                }
                UiAsyncEvent::WorktreeSetupFinished { .. } => {
                    if !self.active_worktree_setup_ids.remove(&task_id) {
                        return effects;
                    }
                    *event
                }
                _ => *event,
            },
            other => other,
        };
        match event {
            UiAsyncEvent::TaskStamped { .. } => unreachable!("unwrapped above"),
            UiAsyncEvent::ContainingBranches { oid, token, names } => {
                if self
                    .log
                    .details
                    .resolve_containing_branches(oid, &token, names)
                {
                    self.needs_redraw = true;
                }
            }
            UiAsyncEvent::Askpass(req) => self.enqueue_credential(req),
            UiAsyncEvent::InitRepoFinished(dir) => {
                self.modal_stack
                    .retain(|l| !matches!(l, ModalLayer::Welcome(_)));
                self.request_repo_switch(&dir, true);
                self.needs_redraw = true;
            }
            UiAsyncEvent::InitRepoFailed(err) => {
                for layer in self.modal_stack.iter_mut().rev() {
                    if let ModalLayer::Welcome(ws) = layer {
                        ws.initializing = false;
                        ws.error = Some(err.clone());
                        break;
                    }
                }
                self.status_notice =
                    Some(self.i18n.t_fmt("toast.repo_init_failed", &[("err", &err)]));
                self.needs_redraw = true;
            }
            UiAsyncEvent::CloneProgress(line) => {
                for layer in self.modal_stack.iter_mut().rev() {
                    if let ModalLayer::CloneRepo(cd) = layer {
                        cd.push_progress(line);
                        self.needs_redraw = true;
                        break;
                    }
                }
            }
            UiAsyncEvent::CloneFinished(result) => match result {
                Ok(dir) => {
                    self.modal_stack.retain(|l| {
                        !matches!(l, ModalLayer::CloneRepo(_) | ModalLayer::Welcome(_))
                    });
                    let shown = dir.display().to_string();
                    self.status_notice =
                        Some(self.i18n.t_fmt("toast.repo_cloned", &[("path", &shown)]));
                    let auto_confirm = self.repo_snapshot.is_none();
                    self.request_repo_switch(&dir, auto_confirm);
                    self.needs_redraw = true;
                }
                Err(err) => {
                    for layer in self.modal_stack.iter_mut().rev() {
                        if let ModalLayer::CloneRepo(cd) = layer {
                            cd.cloning = false;
                            let err_lines: Vec<&str> = err
                                .lines()
                                .map(str::trim)
                                .filter(|l| !l.is_empty())
                                .collect();
                            cd.progress_lines.retain(|line| {
                                let trimmed = line.trim();
                                !trimmed.is_empty()
                                    && !err_lines
                                        .iter()
                                        .any(|e| *e == trimmed || e.ends_with(trimmed))
                            });
                            cd.error = Some(err.clone());
                            break;
                        }
                    }
                    self.status_notice =
                        Some(self.i18n.t_fmt("toast.repo_clone_failed", &[("err", &err)]));
                    self.needs_redraw = true;
                }
            },
            UiAsyncEvent::WorktreeSetupFinished {
                worktree,
                command,
                result,
            } => {
                let name = worktree.file_name().map_or_else(
                    || worktree.display().to_string(),
                    |n| n.to_string_lossy().into(),
                );
                self.status_notice = Some(match result {
                    Ok(()) => self.i18n.t_fmt(
                        "toast.worktree_setup_ok",
                        &[("name", &name), ("cmd", &command)],
                    ),
                    Err(err) => self.i18n.t_fmt(
                        "toast.worktree_setup_failed",
                        &[("name", &name), ("cmd", &command), ("err", &err)],
                    ),
                });
                self.needs_redraw = true;
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
                if self.stage_workbench.apply_syntax_highlighted(
                    sk,
                    epoch,
                    highlighted,
                    &mut self.stage_syntax,
                ) {
                    self.needs_redraw = true;
                }
            }
            UiAsyncEvent::SyntaxHighlighted {
                key: SyntaxJobKey::RangePatch { oid, path, range },
                epoch,
                highlighted,
            } => {
                if !self
                    .range_syntax
                    .finish_cancelable(&(oid, path, range), epoch)
                {
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
                if let Some(fh) = self.modal_stack.file_history_mut()
                    && fh.show_repo_version
                    && fh.repo_version_requested.as_ref() == Some(&key)
                {
                    fh.repo_version_syntax = Some(highlighted);
                    self.needs_redraw = true;
                } else if let Some(s) = self.modal_stack.tree_browser_mut()
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
                warn_crlf: self.config.jbgit.commit.warn_crlf,
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
                    self.diff_caches.clear_worktree_blames();
                    self.invalidate_worktree_diffs(&mut effects);
                    self.check_conflict_resolver_staleness();
                }
                self.reconcile_all_requests(&mut effects);
                return effects;
            }
            Some(ev) => ev,
        };

        match event {
            EngineEvent::RepoSnapshotLoaded { snapshot, epoch } => {
                if !self
                    .request_slots
                    .accept(RequestSlotId::RepoSnapshot, epoch)
                {
                    return effects;
                }
                self.loading_snapshot = false;
                self.loading_more_commits = false;
                let prev_head = self.repo_snapshot.as_ref().and_then(|s| s.head_oid);
                let prev_ongoing = self.ongoing_kind();
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
                self.log.cherry_picked = None;
                self.log.cherry_picked_requested = None;
                self.log.details.reset_containing();
                for layer in self.modal_stack.iter_mut() {
                    if let ModalLayer::Dialog(d) = layer
                        && let crate::dialogs::Dialog::Pull(p) = d.as_mut()
                    {
                        p.refresh_from_snapshot(&snapshot);
                    }
                }
                self.diff_caches.clear_blames();
                self.repo_snapshot = Some(snapshot);
                self.apply_pending_remote_selection();
                self.sync_open_push_dialog_remotes(&mut effects);
                if self.pending_update_stash
                    && self.ongoing_kind().is_none()
                    && (self.pending_update_stash_restore || prev_ongoing.is_some())
                {
                    self.restore_pending_update_stash(true, &mut effects);
                }
                if let Some(rf) = self.modal_stack.reflog_mut() {
                    rf.loading = true;
                    rf.error = None;
                    effects.push(Effect::RequestReflog(rf.key.clone()));
                }
                effects.push(Effect::RequestCommitPrecheck);
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
                        pd.selected_file =
                            pd.selected_file.min(bundle.changes.len().saturating_sub(1));
                    }
                }
                let rebase_sync = slot == RequestSlotId::RebaseCommitDetails;
                if rebase_sync {
                    for layer in self.modal_stack.iter_mut().rev() {
                        if let ModalLayer::Dialog(d) = layer
                            && let crate::dialogs::Dialog::RebaseTodo(r) = d.as_mut()
                        {
                            if r.details_requested.as_ref() == Some(&bundle.key) {
                                r.details_requested = None;
                            }
                            if r.desired_details_key().as_ref() == Some(&bundle.key) {
                                r.selected_file =
                                    r.selected_file.min(bundle.changes.len().saturating_sub(1));
                            }
                            break;
                        }
                    }
                }
                if slot == RequestSlotId::ReflogCommitDetails
                    && let Some(rf) = self.modal_stack.reflog_mut()
                {
                    if rf.details_requested.as_ref() == Some(&bundle.key) {
                        rf.details_requested = None;
                    }
                    if rf.desired_details_key().as_ref() == Some(&bundle.key) {
                        rf.changes.selected = rf
                            .changes
                            .selected
                            .min(bundle.changes.len().saturating_sub(1));
                    }
                }
                if slot == RequestSlotId::AnnotateCommitDetails
                    && let Some(ann) = self.modal_stack.annotate_mut()
                    && ann.details_requested.as_ref() == Some(&bundle.key)
                {
                    ann.details_requested = None;
                }
                if slot == RequestSlotId::FileHistoryCommitDetails
                    && let Some(fh) = self.modal_stack.file_history_mut()
                {
                    if fh.details_requested.as_ref() == Some(&bundle.key) {
                        fh.details_requested = None;
                    }
                    if fh.desired_details_key().as_ref() == Some(&bundle.key) {
                        fh.affected_selected = fh
                            .affected_selected
                            .min(bundle.changes.len().saturating_sub(1));
                    }
                }
                self.diff_caches.insert_details(bundle);
                if rebase_sync {
                    self.sync_rebase_todo_preview(&mut effects);
                }
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
            EngineEvent::CherryPickedLoaded { key, oids, epoch } => {
                if !self
                    .request_slots
                    .accept(RequestSlotId::LogCherryPicked, epoch)
                {
                    return effects;
                }
                self.log.apply_cherry_picked_loaded(key, oids);
                self.needs_redraw = true;
            }
            EngineEvent::ReflogLoaded { bundle, epoch } => {
                if !self.request_slots.accept(RequestSlotId::Reflog, epoch) {
                    return effects;
                }
                if let Some(rf) = self.modal_stack.reflog_mut()
                    && rf.key == bundle.key
                {
                    rf.apply_bundle(bundle);
                    self.needs_redraw = true;
                }
            }
            EngineEvent::ReflogFailed {
                key,
                message,
                epoch,
            } => {
                if !self.request_slots.accept(RequestSlotId::Reflog, epoch) {
                    return effects;
                }
                if let Some(rf) = self.modal_stack.reflog_mut()
                    && rf.key == key
                {
                    rf.apply_error(message);
                    self.needs_redraw = true;
                }
            }
            EngineEvent::FileDiffLoaded { slot, diff, epoch } => {
                if !self.request_slots.accept(slot, epoch) {
                    return effects;
                }
                let mut rebase_matched_request = false;
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
                    RequestSlotId::RebaseFileDiff => {
                        for layer in self.modal_stack.iter_mut().rev() {
                            if let ModalLayer::Dialog(d) = layer
                                && let crate::dialogs::Dialog::RebaseTodo(r) = d.as_mut()
                            {
                                if r.diff_requested.as_ref() == Some(&diff.key) {
                                    r.diff_requested = None;
                                    rebase_matched_request = true;
                                }
                                break;
                            }
                        }
                    }
                    RequestSlotId::ReflogFileDiff => {
                        if let Some(rf) = self.modal_stack.reflog_mut()
                            && rf.diff.requested.as_ref() == Some(&diff.key)
                        {
                            rf.diff.requested = None;
                        }
                    }
                    _ => {}
                }
                let log_wants = self.log.details.diff_visible()
                    && self
                        .log
                        .details
                        .desired_diff_key(&self.diff_caches)
                        .as_ref()
                        == Some(&diff.key);
                let stash_wants = self.active_tab == WorkspaceTab::Stash
                    && self.stash.diff_key(&self.diff_caches).as_ref() == Some(&diff.key);
                let fh_wants = self
                    .modal_stack
                    .file_history()
                    .and_then(|fh| fh.desired_diff_key_with_store(&self.diff_caches))
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
                let reflog_wants = self
                    .modal_stack
                    .reflog()
                    .and_then(|rf| rf.desired_diff_key(&self.diff_caches))
                    .as_ref()
                    == Some(&diff.key);
                let rebase_wants = rebase_matched_request
                    || self.modal_stack.iter().rev().any(|l| {
                        let ModalLayer::Dialog(d) = l else {
                            return false;
                        };
                        let crate::dialogs::Dialog::RebaseTodo(r) = d.as_ref() else {
                            return false;
                        };
                        if !r.show_diff {
                            return false;
                        }
                        if r.diff_requested.as_ref() == Some(&diff.key) {
                            return true;
                        }
                        let Some(det_key) = r.desired_details_key() else {
                            return false;
                        };
                        if det_key != diff.key.details {
                            return false;
                        }
                        self.diff_caches
                            .peek_details(&det_key)
                            .and_then(|b| b.changes.get(r.selected_file))
                            .is_some_and(|c| {
                                c.path == diff.key.path && c.old_path == diff.key.old_path
                            })
                    });
                let is_wanted = log_wants
                    || stash_wants
                    || fh_wants
                    || ann_wants
                    || push_wants
                    || reflog_wants
                    || rebase_wants;
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
                let mut range_syntax_job: Option<(
                    jbgit_core::Oid,
                    (u32, u32),
                    Arc<jbgit_core::FileDiff>,
                )> = None;
                let desired_after_load = if let Some(fh) = self.modal_stack.file_history_mut()
                    && fh.key == bundle.key
                {
                    fh.on_bundle_loaded(bundle);
                    if fh.uses_range_patch() {
                        if let (Some(range), Some(oid)) =
                            (fh.key.line_range, fh.selected_entry().map(|e| e.oid))
                            && let Some(diff) = fh.ensure_range_diff()
                        {
                            range_syntax_job = Some((oid, range, diff));
                        }
                        None
                    } else {
                        fh.desired_diff_key_with_store(&self.diff_caches)
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
                self.diff_caches.insert_blame(Arc::clone(&bundle));
                let mut syntax_bundle: Option<Arc<jbgit_core::BlameBundle>> = None;
                if let Some(ann) = self.modal_stack.annotate_mut()
                    && ann.key.cache_key() == bundle.key.cache_key()
                {
                    ann.apply_loaded_bundle(Arc::clone(&bundle));
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
                    && ann.key.cache_key() == key.cache_key()
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
            EngineEvent::TreeBlobLoaded { slot, blob, epoch } => {
                self.on_tree_blob_loaded(slot, blob, epoch, &mut effects);
            }
            EngineEvent::TreeIndexLoaded { oid, paths, epoch } => {
                self.on_tree_index_loaded(oid, paths, epoch);
            }
            EngineEvent::TreeBlobExported { path, dest, result } => {
                self.on_tree_blob_exported(&path, &dest, result);
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
                    pd.expected_remote_oid = bundle.remote_oid;
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
            EngineEvent::CommitCompleted(outcome) => {
                if outcome.success && outcome.and_push {
                    let (branch, up_to) = match self.repo_snapshot.as_deref() {
                        Some(s) if s.head_branch.is_some() => {
                            (s.head_branch.clone().unwrap_or_else(|| "HEAD".into()), None)
                        }
                        Some(s) => ("HEAD".to_string(), outcome.commit_oid.or(s.head_oid)),
                        None => ("HEAD".to_string(), outcome.commit_oid),
                    };
                    self.prompt_push(&branch, up_to, &mut effects);
                }
            }
            EngineEvent::StageStatusLoaded { .. }
            | EngineEvent::StageFileDiffLoaded { .. }
            | EngineEvent::AmendPreviewLoaded { .. }
            | EngineEvent::CommitPrecheckLoaded { .. }
            | EngineEvent::UndoCommitCompleted { .. } => {
                unreachable!("handled by apply_stage_engine_event")
            }
            EngineEvent::CommitCrlfWarning { request, files } => {
                self.stage_workbench.set_commit_in_progress(false);
                let dialog =
                    crate::repo_lifecycle::crlf_warning_dialog(&self.i18n, request, &files);
                self.push_modal(ModalLayer::Dialog(Box::new(dialog)));
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
                retry_env,
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
                self.push_modal(ModalLayer::Dialog(Box::new(Dialog::Choice(
                    ChoiceDialog::confirm_with_default(
                        self.i18n.t("dialog.index_lock.title"),
                        body,
                        self.i18n.t("common.retry"),
                        self.i18n.t("common.dismiss"),
                        true,
                        true,
                        vec![Action::RetryCliCommand {
                            args: retry_args,
                            stdin_payload: retry_stdin,
                            env: retry_env,
                            is_read_only: retry_is_read_only,
                            refresh_after: retry_refresh_after,
                        }],
                    ),
                ))));
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
                self.loading_more_commits = false;
                // Keep `requested` keys intact on diff/details views so failing
                // single-flight loads do not spin in an infinite hot retry loop
                // on every subsequent reconcile; changing selection or pressing
                // RefreshAll (`Ctrl+R`) resets them. Never unlock
                // `commit_in_progress` here — commits only conclude via
                // `CommitCompleted` or `CommitCrlfWarning`.
                if let Some(fh) = self.modal_stack.file_history_mut() {
                    fh.loading = false;
                    if fh.bundle.is_none() {
                        fh.error = Some(msg.clone());
                    }
                }
                if let Some(ann) = self.modal_stack.annotate_mut() {
                    ann.loading = false;
                    if ann.bundle.is_none() && ann.error.is_none() {
                        ann.error = Some(AnnotateError::Message(msg.clone()));
                    }
                }
                if let Some(pd) = self.modal_stack.push_dialog_mut() {
                    pd.loading = false;
                    if pd.outgoing.is_none() && pd.error.is_none() {
                        pd.error = Some(msg.clone());
                    }
                }
                if let Some(rf) = self.modal_stack.reflog_mut() {
                    rf.loading = false;
                    if rf.bundle.is_none() && rf.error.is_none() {
                        rf.error = Some(msg.clone());
                    }
                }
                if let Some(tb) = self.modal_stack.tree_browser_mut() {
                    tb.in_flight = None;
                    tb.clear_search_index_request();
                    tb.preview_requested = None;
                }
                if let Some(ap) = self.modal_stack.apply_patch_mut() {
                    ap.pending_check_due = None;
                }
                self.status_notice = Some(self.i18n.t_fmt("toast.engine_error", &[("msg", &msg)]));
                return effects;
            }
        }

        self.reconcile_all_requests(&mut effects);
        effects
    }

    /// Clears single-flight request de-duplication markers across views/modals
    /// so an explicit refresh (`Ctrl+R`) can retry previously failed fetches.
    pub(crate) fn clear_in_flight_requests(&mut self) {
        self.log.details.diff.requested = None;
        self.stash.on_error();
        self.stage_workbench.clear_in_flight_diff_request();
        if let Some(fh) = self.modal_stack.file_history_mut() {
            fh.details_requested = None;
            fh.diff.requested = None;
        }
        if let Some(ann) = self.modal_stack.annotate_mut() {
            ann.details_requested = None;
            ann.diff.requested = None;
        }
        if let Some(pd) = self.modal_stack.push_dialog_mut() {
            pd.details_requested = None;
            pd.diff.requested = None;
        }
        if let Some(rf) = self.modal_stack.reflog_mut() {
            rf.details_requested = None;
            rf.diff.requested = None;
        }
        for layer in self.modal_stack.iter_mut() {
            if let ModalLayer::Dialog(d) = layer
                && let crate::dialogs::Dialog::RebaseTodo(r) = d.as_mut()
            {
                r.details_requested = None;
                r.diff_requested = None;
            }
        }
    }

    /// Evicts cached `CompareWithLocal` entries from `diff_caches` and
    /// re-requests any actively viewed `CompareWithLocal` details/diffs.
    pub(crate) fn invalidate_worktree_diffs(&mut self, effects: &mut Vec<Effect>) {
        self.diff_caches.clear_worktree_diffs();
        if self
            .log
            .details
            .diff
            .requested
            .as_ref()
            .is_some_and(|k| k.details.mode == jbgit_core::DetailsMode::CompareWithLocal)
        {
            self.log.details.diff.requested = None;
        }
        if let Some(key) = self
            .log
            .details
            .changes
            .current_key
            .clone()
            .filter(|k| k.mode == jbgit_core::DetailsMode::CompareWithLocal)
        {
            effects.push(Effect::RequestCommitDetails {
                slot: RequestSlotId::LogCommitDetails,
                key,
            });
        }
        if let Some(fh) = self.modal_stack.file_history_mut() {
            if fh
                .diff
                .requested
                .as_ref()
                .is_some_and(|k| k.details.mode == jbgit_core::DetailsMode::CompareWithLocal)
            {
                fh.diff.requested = None;
            }
            if fh
                .details_requested
                .as_ref()
                .is_some_and(|k| k.mode == jbgit_core::DetailsMode::CompareWithLocal)
            {
                fh.details_requested = None;
            }
        }
    }

    fn check_conflict_resolver_staleness(&mut self) {
        let Some(status) = self.stage_workbench.status().cloned() else {
            return;
        };
        let mut prompt_all_resolved = false;
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
            if !cq.entries.is_empty() && cq.all_resolved() && !cq.completion_prompted {
                cq.completion_prompted = true;
                prompt_all_resolved = true;
            }
        }
        if prompt_all_resolved && self.modal_stack.conflict_merge().is_none() {
            self.open_all_conflicts_resolved_dialog();
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
