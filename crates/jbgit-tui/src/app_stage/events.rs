use std::sync::Arc;
use std::time::Instant;

use jbgit_core::{ModalButton, RefreshScope, WorkspaceTab};
use jbgit_engine::EngineEvent;

use crate::app::{AppState, Effect, ModalLayer, PopupKind};
use crate::stage_view::CommitBanner;

impl AppState {
    pub fn reconcile_stage_requests(&mut self, now: Instant, effects: &mut Vec<Effect>) {
        self.stage_workbench.sync_rows(self.stage_status.as_ref());
        let total = self.stage_workbench.rows.len();
        if total == 0 {
            self.commit_stage_selected_idx = 0;
        } else if self.commit_stage_selected_idx >= total {
            self.commit_stage_selected_idx = total - 1;
        }

        if self.active_tab != WorkspaceTab::Commit || self.stage_workbench.diff_collapsed {
            self.stage_workbench.pending_diff = None;
            return;
        }
        let immediate = std::mem::take(&mut self.stage_workbench.pending_select_immediate);

        let desired = self
            .stage_workbench
            .desired_diff_key(self.commit_stage_selected_idx, self.stage_status.as_deref());

        if desired != self.stage_workbench.current_diff_key {
            self.stage_syntax.invalidate();
            self.stage_workbench.current_diff_key = desired.clone();
            self.stage_workbench.diff_scroll = 0;
            self.stage_workbench.diff_cursor_row = 0;
            self.stage_workbench.visual_anchor_row = None;

            if let Some(k) = desired {
                if let Some(cached) = self.stage_workbench.diff_cache.get(&k).cloned() {
                    self.stage_workbench.pending_diff = None;
                    self.stage_workbench.requested_diff = None;
                    self.stage_workbench.diff_syntax =
                        self.stage_workbench.diff_syntax_cache.get(&k).cloned();
                    self.stage_workbench.current_diff = Some(Arc::clone(&cached));
                    let len = self.stage_workbench.diff_rows().len();
                    if len > 0 {
                        self.stage_workbench.diff_cursor_row =
                            self.stage_workbench.diff_cursor_row.min(len - 1);
                    }
                    if self.stage_workbench.diff_syntax.is_none() {
                        self.schedule_stage_syntax(cached, effects);
                    }
                } else {
                    self.stage_workbench.current_diff = None;
                    self.stage_workbench.diff_syntax = None;
                    if immediate {
                        self.stage_workbench.pending_diff = None;
                        self.stage_workbench.requested_diff = Some(k.clone());
                        effects.push(Effect::RequestStageFileDiff(k));
                    } else {
                        self.stage_workbench.pending_diff =
                            Some((k, now + crate::log_view::DETAILS_DEBOUNCE));
                    }
                }
            } else {
                self.stage_workbench.current_diff = None;
                self.stage_workbench.diff_syntax = None;
                self.stage_workbench.requested_diff = None;
                self.stage_workbench.pending_diff = None;
            }
        } else if let Some(k) = desired {
            if self.stage_workbench.current_diff.is_none() {
                if let Some(cached) = self.stage_workbench.diff_cache.get(&k).cloned() {
                    self.stage_workbench.pending_diff = None;
                    self.stage_workbench.requested_diff = None;
                    self.stage_workbench.diff_syntax =
                        self.stage_workbench.diff_syntax_cache.get(&k).cloned();
                    self.stage_workbench.current_diff = Some(Arc::clone(&cached));
                    if self.stage_workbench.diff_syntax.is_none() {
                        self.schedule_stage_syntax(cached, effects);
                    }
                } else if self.stage_workbench.requested_diff.as_ref() != Some(&k)
                    && self
                        .stage_workbench
                        .pending_diff
                        .as_ref()
                        .map(|(pk, _)| pk)
                        != Some(&k)
                {
                    if immediate {
                        self.stage_workbench.pending_diff = None;
                        self.stage_workbench.requested_diff = Some(k.clone());
                        effects.push(Effect::RequestStageFileDiff(k));
                    } else {
                        self.stage_workbench.pending_diff =
                            Some((k, now + crate::log_view::DETAILS_DEBOUNCE));
                    }
                }
            } else if self.stage_workbench.diff_syntax.is_none()
                && let Some(cached_hl) = self.stage_workbench.diff_syntax_cache.get(&k).cloned()
            {
                self.stage_workbench.diff_syntax = Some(cached_hl);
            } else if self.stage_workbench.diff_syntax.is_none()
                && let Some(diff) = self.stage_workbench.current_diff.clone()
            {
                self.schedule_stage_syntax(diff, effects);
            }
        }
    }

    pub(crate) fn apply_stage_engine_event(
        &mut self,
        event: EngineEvent,
        effects: &mut Vec<Effect>,
    ) -> Option<EngineEvent> {
        match event {
            EngineEvent::StageStatusLoaded { status, epoch } => {
                if epoch < self.latest_stage_epoch {
                    return None;
                }
                self.latest_stage_epoch = epoch;

                let changed = self.stage_status.as_ref() != Some(&status);
                let had_status = self.stage_status.is_some();
                let old_rows = self.stage_workbench.rows.clone();
                let old_sel = self.commit_stage_selected_idx;
                self.stage_status = Some(status);
                if changed || had_status {
                    self.stage_workbench.invalidate_all_diff_caches();
                    if changed {
                        self.stage_workbench.current_diff_key = None;
                    }
                    self.stage_workbench.pending_select_immediate = true;
                }
                self.stage_workbench.sync_rows(self.stage_status.as_ref());
                if !old_rows.is_empty() {
                    self.commit_stage_selected_idx = self
                        .stage_workbench
                        .smooth_anchor_after_rows_update(&old_rows, old_sel);
                } else {
                    self.commit_stage_selected_idx = 0;
                }
                None
            }
            EngineEvent::StageFileDiffLoaded { diff, epoch } => {
                if self.stage_workbench.requested_diff.as_ref() == Some(&diff.key) {
                    self.stage_workbench.requested_diff = None;
                }
                let is_wanted = self.stage_workbench.current_diff_key.as_ref() == Some(&diff.key);
                if epoch < self.latest_stage_diff_epoch && !is_wanted {
                    return None;
                }
                self.latest_stage_diff_epoch = self.latest_stage_diff_epoch.max(epoch);
                self.stage_workbench
                    .diff_cache
                    .insert(diff.key.clone(), Arc::clone(&diff));
                if is_wanted {
                    self.stage_workbench.diff_syntax = self
                        .stage_workbench
                        .diff_syntax_cache
                        .get(&diff.key)
                        .cloned();
                    self.stage_workbench.current_diff = Some(Arc::clone(&diff));
                    let len = self.stage_workbench.diff_rows().len();
                    if len == 0 {
                        self.stage_workbench.diff_cursor_row = 0;
                        self.stage_workbench.diff_scroll = 0;
                    } else {
                        self.stage_workbench.diff_cursor_row =
                            self.stage_workbench.diff_cursor_row.min(len - 1);
                    }
                    if self.stage_workbench.diff_syntax.is_none() {
                        self.schedule_stage_syntax(diff, effects);
                    }
                }
                None
            }
            EngineEvent::AmendPreviewLoaded { preview } => {
                let prev_sha = self
                    .stage_workbench
                    .amend_preview
                    .as_ref()
                    .map(|p| p.short_sha.clone());
                let new_sha = preview.as_ref().map(|p| p.short_sha.clone());
                let head_changed = prev_sha != new_sha;
                if head_changed {
                    self.stage_workbench.invalidate_all_diff_caches();
                    self.stage_workbench.pending_select_immediate = true;
                }
                let should_fill = self.stage_workbench.amend
                    && (self.stage_workbench.amend_awaiting_message || head_changed);
                self.stage_workbench.amend_preview = preview.clone();
                self.stage_workbench.amend_awaiting_message = false;
                if should_fill
                    && let Some(p) = preview
                    && !p.message.trim().is_empty()
                {
                    self.stage_workbench.editor.set_text(&p.message);
                }
                self.stage_workbench.sync_rows(self.stage_status.as_ref());
                None
            }
            EngineEvent::CommitPrecheckLoaded { info } => {
                self.stage_workbench.precheck = info;
                None
            }
            EngineEvent::CommitCompleted(outcome) => {
                self.stage_workbench.commit_in_progress = false;
                if outcome.success {
                    self.stage_workbench.invalidate_all_diff_caches();
                    self.stage_workbench.pending_select_immediate = true;
                    let was_amend = self.stage_workbench.amend;
                    let committed_msg = self.stage_workbench.editor.text();
                    self.stage_workbench.push_history(&committed_msg);
                    self.stage_workbench.editor.clear();
                    self.stage_workbench.amend = false;
                    self.stage_workbench.amend_awaiting_message = false;
                    self.stage_workbench.saved_draft = None;
                    self.stage_workbench.amend_preview = None;
                    self.stage_workbench.empty_message_error = false;
                    self.stage_workbench.mark_draft_dirty(Instant::now());
                    let localized_summary = match outcome.short_sha.as_deref() {
                        Some(sha) if was_amend => {
                            self.i18n.t_fmt("toast.amended_commit", &[("sha", sha)])
                        }
                        Some(sha) => self.i18n.t_fmt("toast.committed", &[("sha", sha)]),
                        None => outcome.summary.clone(),
                    };
                    self.stage_workbench.last_commit_banner = Some(CommitBanner {
                        summary: localized_summary.clone(),
                        can_undo: true,
                    });
                    self.status_notice = Some(localized_summary);
                    if outcome.and_push {
                        let branch = self
                            .repo_snapshot
                            .as_ref()
                            .and_then(|s| s.head_branch.as_deref())
                            .unwrap_or("HEAD");
                        let sha = outcome.short_sha.as_deref().unwrap_or("commit");
                        self.modal_stack.push(ModalLayer::CriticalDialog {
                            title: self.i18n.t("dialog.push.confirm_title").to_string(),
                            body: self.i18n.t_fmt(
                                "dialog.push.confirm_body",
                                &[("sha", sha), ("branch", branch)],
                            ),
                            confirm_label: self.i18n.t("dialog.push.push_btn").to_string(),
                            cancel_label: self.i18n.cancel().to_string(),
                            selected_button: ModalButton::Confirm,
                            confirm_effect: Some(Effect::RunCli {
                                args: vec!["push".into()],
                                stdin_payload: None,
                                is_read_only: false,
                                refresh_after: RefreshScope::RefsAndGraph,
                            }),
                        });
                    }
                } else {
                    if let Some(msg) = outcome.preserved_message
                        && self.stage_workbench.editor.is_empty()
                    {
                        self.stage_workbench.editor.set_text(&msg);
                    }
                    self.status_notice = Some(outcome.summary.clone());
                    self.hook_error_output = Some(outcome.error_output.unwrap_or(outcome.summary));
                    self.modal_stack.push(ModalLayer::FloatingPopup {
                        kind: PopupKind::HookError,
                        query: String::new(),
                        selected: 0,
                    });
                }
                None
            }
            EngineEvent::UndoCommitCompleted { restored_message } => {
                self.stage_workbench.last_commit_banner = None;
                self.stage_workbench.amend_preview = None;
                if self.stage_workbench.editor.is_empty() && !restored_message.is_empty() {
                    self.stage_workbench.editor.set_text(&restored_message);
                    self.stage_workbench.mark_draft_dirty(Instant::now());
                }
                self.status_notice =
                    Some(self.i18n.t("toast.undid_last_commit").to_string());
                None
            }
            other => Some(other),
        }
    }
}
