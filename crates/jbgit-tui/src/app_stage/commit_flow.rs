use std::time::Instant;

use jbgit_core::{ModalButton, PaneId};
use jbgit_engine::CommitRequest;

use crate::app::{Action, AppState, EditorMoveKind, Effect, ModalLayer};

impl AppState {
    pub(super) fn dispatch_commit_editor_action(
        &mut self,
        action: &Action,
        effects: &mut Vec<Effect>,
    ) -> bool {
        match action {
            Action::CommitEditorChar(ch) => {
                self.stage_workbench.empty_message_error = false;
                self.stage_workbench.editor.insert_char(*ch);
                self.stage_workbench.mark_draft_dirty(Instant::now());
            }
            Action::CommitEditorNewline => {
                self.stage_workbench.empty_message_error = false;
                self.stage_workbench.editor.insert_newline();
                self.stage_workbench.mark_draft_dirty(Instant::now());
            }
            Action::CommitEditorPaste(text) => {
                self.stage_workbench.empty_message_error = false;
                self.stage_workbench.editor.insert_paste(text);
                self.stage_workbench.mark_draft_dirty(Instant::now());
            }
            Action::CommitEditorBackspace => {
                self.stage_workbench.empty_message_error = false;
                self.stage_workbench.editor.backspace();
                self.stage_workbench.mark_draft_dirty(Instant::now());
            }
            Action::CommitEditorDelete => {
                self.stage_workbench.editor.delete_forward();
                self.stage_workbench.mark_draft_dirty(Instant::now());
            }
            Action::CommitEditorDeleteWord => {
                self.stage_workbench.editor.delete_word_backward();
                self.stage_workbench.mark_draft_dirty(Instant::now());
            }
            Action::CommitEditorMove(kind) => match kind {
                EditorMoveKind::Left => self.stage_workbench.editor.move_left(),
                EditorMoveKind::Right => self.stage_workbench.editor.move_right(),
                EditorMoveKind::Up => self.stage_workbench.editor.move_up(),
                EditorMoveKind::Down => self.stage_workbench.editor.move_down(),
                EditorMoveKind::Home => self.stage_workbench.editor.move_home(),
                EditorMoveKind::End => self.stage_workbench.editor.move_end(),
            },
            Action::CommitEditorUndo => {
                if self.stage_workbench.editor.undo() {
                    self.stage_workbench.mark_draft_dirty(Instant::now());
                }
            }
            Action::CommitEditorRedo => {
                if self.stage_workbench.editor.redo() {
                    self.stage_workbench.mark_draft_dirty(Instant::now());
                }
            }
            Action::CommitToggleAmend => {
                let next = !self.stage_workbench.amend;
                self.stage_workbench.amend = next;
                self.stage_workbench.empty_message_error = false;
                if next {
                    self.stage_workbench.saved_draft = Some(self.stage_workbench.editor.text());
                    self.stage_workbench.mark_draft_dirty(Instant::now());
                    self.stage_workbench.flush_draft_sync();
                    self.stage_workbench.amend_awaiting_message = true;
                    if let Some(preview) = &self.stage_workbench.amend_preview
                        && !preview.message.trim().is_empty()
                    {
                        self.stage_workbench.editor.set_text(&preview.message);
                        self.stage_workbench.amend_awaiting_message = false;
                    }
                    effects.push(Effect::RequestAmendPreview);
                } else {
                    self.stage_workbench.amend_awaiting_message = false;
                    if let Some(draft) = self.stage_workbench.saved_draft.take() {
                        self.stage_workbench.editor.set_text(&draft);
                    }
                    self.stage_workbench.mark_draft_dirty(Instant::now());
                    self.stage_workbench.flush_draft_sync();
                }
                self.stage_workbench.sync_rows(self.stage_status.as_ref());
            }
            Action::CommitToggleSignoff => {
                self.stage_workbench.signoff = !self.stage_workbench.signoff;
            }
            Action::CommitToggleRunHooks => {
                self.stage_workbench.run_hooks = !self.stage_workbench.run_hooks;
            }
            Action::CommitToggleAuthorInput => {
                self.stage_workbench.show_author_input = !self.stage_workbench.show_author_input;
                self.stage_workbench.editing_author = self.stage_workbench.show_author_input;
            }
            Action::CommitFocusAuthorInput => {
                self.focused_pane = PaneId::CommitMessageEditor;
                self.stage_workbench.show_author_input = true;
                self.stage_workbench.editing_author = true;
            }
            Action::CommitAuthorChar(ch) => {
                self.stage_workbench.author_input.push(*ch);
            }
            Action::CommitAuthorBackspace => {
                self.stage_workbench.author_input.pop();
            }
            Action::CommitAuthorDone => {
                self.stage_workbench.editing_author = false;
            }
            Action::CommitApplyHistory(idx) => {
                let candidates = self.commit_history_candidates();
                if let Some(msg) = candidates.get(*idx).cloned() {
                    let prev = self.stage_workbench.editor.text();
                    if !self.stage_workbench.amend
                        && !prev.trim().is_empty()
                        && prev.trim() != msg.trim()
                    {
                        self.stage_workbench.push_history(&prev);
                    }
                    self.stage_workbench.editor.set_text(&msg);
                    self.stage_workbench.empty_message_error = false;
                    self.stage_workbench.mark_draft_dirty(Instant::now());
                    self.focused_pane = PaneId::CommitMessageEditor;
                }
            }
            Action::SubmitCommit { and_push } => {
                self.handle_submit_commit(*and_push, false, effects);
            }
            Action::UndoLastCommit => {
                effects.push(Effect::UndoLastCommit);
            }
            Action::OpenExternalEditorMessage => {
                effects.push(Effect::OpenExternalEditorMessage);
            }
            Action::OpenExternalEditorFile(opt_path) => {
                let path = opt_path.clone().or_else(|| {
                    self.stage_workbench
                        .current_diff_key
                        .as_ref()
                        .map(|k| k.path.clone())
                });
                if let Some(p) = path {
                    effects.push(Effect::OpenExternalEditorFile(p));
                }
            }
            _ => return false,
        }
        true
    }

    pub fn commit_history_candidates(&self) -> Vec<String> {
        let mut out = Vec::new();
        if let Some(draft) = &self.stage_workbench.saved_draft
            && !draft.trim().is_empty()
        {
            out.push(draft.trim().to_string());
        }
        if !self.stage_workbench.amend {
            let current = self.stage_workbench.editor.text();
            let trimmed = current.trim();
            if !trimmed.is_empty() && !out.iter().any(|s| s == trimmed) {
                out.push(trimmed.to_string());
            }
        }
        for h in &self.stage_workbench.commit_history {
            if !out.contains(h) {
                out.push(h.clone());
            }
        }
        if let Some(snap) = &self.repo_snapshot {
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

    pub(crate) fn handle_submit_commit(
        &mut self,
        and_push: bool,
        force_stage_all_tracked: bool,
        effects: &mut Vec<Effect>,
    ) {
        if self.stage_workbench.commit_in_progress {
            return;
        }
        self.stage_workbench.flush_draft_sync();
        if let Some(status) = &self.stage_status
            && !status.conflicted.is_empty()
        {
            self.status_notice = Some(self.i18n.t("toast.resolve_before_commit").to_string());
            let _ = self.dispatch(Action::ResolveConflictsNotice);
            return;
        }

        let msg = self.stage_workbench.editor.text();
        if msg.trim().is_empty() {
            self.stage_workbench.empty_message_error = true;
            self.focused_pane = PaneId::CommitMessageEditor;
            self.status_notice = Some(self.i18n.t("common.commit_msg_empty").to_string());
            return;
        }

        let staged_empty = self
            .stage_status
            .as_ref()
            .is_none_or(|s| s.staged.is_empty());
        let has_tracked_unstaged = self
            .stage_status
            .as_ref()
            .is_some_and(|s| !s.unstaged.is_empty());

        let author_override = self
            .stage_workbench
            .show_author_input
            .then(|| self.stage_workbench.author_input.clone())
            .filter(|s| !s.trim().is_empty());

        if staged_empty && !self.stage_workbench.amend && !force_stage_all_tracked {
            if has_tracked_unstaged {
                let req = CommitRequest {
                    message: msg,
                    amend: false,
                    signoff: self.stage_workbench.signoff,
                    run_hooks: self.stage_workbench.run_hooks,
                    author_override,
                    stage_all_tracked: true,
                    and_push,
                };
                self.modal_stack.push(ModalLayer::CriticalDialog {
                    title: self.i18n.t("dialog.nothing_staged.title").to_string(),
                    body: self.i18n.t("dialog.nothing_staged.body").to_string(),
                    confirm_label: self.i18n.t("dialog.nothing_staged.confirm_btn").to_string(),
                    cancel_label: self.i18n.cancel().to_string(),
                    selected_button: ModalButton::Confirm,
                    confirm_effect: Some(Effect::ExecuteCommit(req)),
                });
            } else {
                self.status_notice = Some(self.i18n.t("toast.no_staged_to_commit").to_string());
            }
            return;
        }

        self.stage_workbench.commit_in_progress = true;
        self.stage_workbench.empty_message_error = false;
        let req = CommitRequest {
            message: msg,
            amend: self.stage_workbench.amend,
            signoff: self.stage_workbench.signoff,
            run_hooks: self.stage_workbench.run_hooks,
            author_override,
            stage_all_tracked: force_stage_all_tracked,
            and_push,
        };
        effects.push(Effect::ExecuteCommit(req));
    }
}
