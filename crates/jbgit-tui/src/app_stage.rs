//! Stage workbench (`[1 Commit]`) action dispatch, 3-doc partial staging,
//! Amend workflow, Commit & Push confirmation, and draft/history persistence.

use std::sync::Arc;

use jbgit_core::{
    LineSelection, ModalButton, PaneId, RefreshScope, StageNodeKind, StageTreeRowKind,
    WorkspaceTab,
};
use jbgit_engine::{CommitRequest, EngineEvent, StageDiffSide};

use crate::app::{Action, AppState, EditorMoveKind, Effect, ModalLayer, PopupKind};
use crate::hit_test::DiffHunkActionKind;
use crate::stage_view::{CommitBanner, RollbackUndoEntry, StageDiffRow};
use crate::syntax::HighlightedDiff;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StageDiffOp {
    Stage,
    Unstage,
    Rollback,
}

impl AppState {
    pub(crate) fn dispatch_stage_action(
        &mut self,
        action: &Action,
        effects: &mut Vec<Effect>,
    ) -> bool {
        match action {
            Action::SelectCommitStageRow(idx) => {
                self.stage_workbench.sync_rows(self.stage_status.as_deref());
                let total = self.stage_item_count();
                if total > 0 {
                    self.commit_stage_selected_idx = (*idx).min(total - 1);
                    self.stage_workbench.multi_selected.clear();
                }
            }
            Action::ScrollCommitStage(delta) => {
                self.stage_workbench.sync_rows(self.stage_status.as_deref());
                let total = self.stage_item_count();
                if total > 0 {
                    self.commit_stage_selected_idx =
                        (self.commit_stage_selected_idx as isize + *delta)
                            .clamp(0, (total - 1) as isize) as usize;
                    self.stage_workbench.multi_selected.clear();
                }
            }
            Action::StageExpand(expand) => {
                let expand = *expand;
                self.stage_workbench.sync_rows(self.stage_status.as_deref());
                if let Some(row) = self
                    .stage_workbench
                    .rows
                    .get(self.commit_stage_selected_idx)
                    .cloned()
                {
                    if matches!(
                        row.kind,
                        StageTreeRowKind::Group {
                            kind: StageNodeKind::Untracked,
                            ..
                        }
                    ) {
                        self.stage_workbench.tree_opts.untracked_many_expanded = expand;
                        self.stage_workbench.sync_rows(self.stage_status.as_deref());
                    } else if row.is_container() {
                        if row.expanded != expand {
                            if expand {
                                self.stage_workbench.collapsed.remove(&row.key);
                            } else {
                                self.stage_workbench.collapsed.insert(row.key.clone());
                            }
                            self.stage_workbench.sync_rows(self.stage_status.as_deref());
                        } else if expand
                            && self.commit_stage_selected_idx + 1 < self.stage_workbench.rows.len()
                        {
                            self.commit_stage_selected_idx += 1;
                        } else if !expand && row.depth > 0 {
                            self.jump_to_stage_parent(row.depth - 1);
                        }
                    } else if !expand && row.depth > 0 {
                        self.jump_to_stage_parent(row.depth - 1);
                    }
                }
            }
            Action::StageToggleRow(idx) => {
                let idx = *idx;
                self.stage_workbench.sync_rows(self.stage_status.as_deref());
                if let Some(row) = self.stage_workbench.rows.get(idx).cloned() {
                    if matches!(
                        row.kind,
                        StageTreeRowKind::Group {
                            kind: StageNodeKind::Untracked,
                            ..
                        }
                    ) {
                        self.stage_workbench.tree_opts.untracked_many_expanded =
                            !self.stage_workbench.tree_opts.untracked_many_expanded;
                        self.stage_workbench.sync_rows(self.stage_status.as_deref());
                    } else if row.is_container() {
                        if row.expanded {
                            self.stage_workbench.collapsed.insert(row.key.clone());
                        } else {
                            self.stage_workbench.collapsed.remove(&row.key);
                        }
                        self.stage_workbench.sync_rows(self.stage_status.as_deref());
                    }
                }
            }
            Action::StageToggleMulti(idx) => {
                let idx = *idx;
                self.stage_workbench.sync_rows(self.stage_status.as_deref());
                if let Some(row) = self.stage_workbench.rows.get(idx) {
                    let key = row.key.clone();
                    if self.stage_workbench.multi_selected.is_empty()
                        && let Some(cur) = self
                            .stage_workbench
                            .rows
                            .get(self.commit_stage_selected_idx)
                        && cur.key != key
                    {
                        self.stage_workbench
                            .multi_selected
                            .insert(cur.key.clone());
                    }
                    if !self.stage_workbench.multi_selected.remove(&key) {
                        self.stage_workbench.multi_selected.insert(key);
                    }
                    self.commit_stage_selected_idx = idx;
                }
            }
            Action::StageSelectAll => {
                self.stage_workbench.sync_rows(self.stage_status.as_deref());
                self.stage_workbench.multi_selected = self
                    .stage_workbench
                    .rows
                    .iter()
                    .filter(|r| r.is_file())
                    .map(|r| r.key.clone())
                    .collect();
            }
            Action::StageToggleGroupByDir => {
                self.stage_workbench.tree_opts.group_by_dir =
                    !self.stage_workbench.tree_opts.group_by_dir;
                self.stage_workbench.sync_rows(self.stage_status.as_deref());
                self.commit_stage_selected_idx = 0;
            }
            Action::StageExpandAll(expand) => {
                if *expand {
                    self.stage_workbench.collapsed.clear();
                    self.stage_workbench.tree_opts.untracked_many_expanded = true;
                } else {
                    self.stage_workbench.tree_opts.untracked_many_expanded = false;
                    self.stage_workbench.sync_rows(self.stage_status.as_deref());
                    let keys: Vec<String> = self
                        .stage_workbench
                        .rows
                        .iter()
                        .filter(|r| matches!(r.kind, StageTreeRowKind::Dir { .. }))
                        .map(|r| r.key.clone())
                        .collect();
                    self.stage_workbench.collapsed.extend(keys);
                }
                self.stage_workbench.sync_rows(self.stage_status.as_deref());
            }
            Action::StageToggleShowIgnored => {
                self.stage_workbench.tree_opts.show_ignored =
                    !self.stage_workbench.tree_opts.show_ignored;
                self.loading_snapshot = true;
                effects.push(Effect::RefreshScope(RefreshScope::StageStatus));
            }
            Action::StageToggleDiffCollapsed => {
                self.stage_workbench.diff_collapsed = !self.stage_workbench.diff_collapsed;
                if self.stage_workbench.diff_collapsed
                    && self.focused_pane == PaneId::CommitDiffPreview
                {
                    self.focused_pane = PaneId::CommitChangesTree;
                }
            }
            Action::StageActionSelected => {
                self.stage_workbench.sync_rows(self.stage_status.as_deref());
                if let Some(status) = &self.stage_status {
                    let targets = self.stage_workbench.collect_targets_for_action(
                        self.commit_stage_selected_idx,
                        status,
                        StageNodeKind::can_stage,
                    );
                    if !targets.is_empty() {
                        let count_s = targets.len().to_string();
                        self.status_notice = Some(
                            self.i18n
                                .t_fmt("toast.staging_paths", &[("count", &count_s)]),
                        );
                        effects.push(Effect::StageTargets(targets));
                    }
                }
            }
            Action::UnstageActionSelected => {
                self.stage_workbench.sync_rows(self.stage_status.as_deref());
                if let Some(status) = &self.stage_status {
                    let targets = self.stage_workbench.collect_targets_for_action(
                        self.commit_stage_selected_idx,
                        status,
                        StageNodeKind::can_unstage,
                    );
                    if !targets.is_empty() {
                        let count_s = targets.len().to_string();
                        self.status_notice = Some(
                            self.i18n
                                .t_fmt("toast.unstaging_paths", &[("count", &count_s)]),
                        );
                        effects.push(Effect::UnstageTargets(targets));
                    }
                }
            }
            Action::StageAllTracked => {
                effects.push(Effect::StageAllTracked);
                self.status_notice =
                    Some(self.i18n.t("toast.staging_all_tracked").to_string());
            }
            Action::DiscardPromptSelected => {
                self.stage_workbench.sync_rows(self.stage_status.as_deref());
                if let Some(status) = &self.stage_status {
                    let targets = self.stage_workbench.collect_targets_for_action(
                        self.commit_stage_selected_idx,
                        status,
                        |k| matches!(k, StageNodeKind::Unstaged | StageNodeKind::Untracked),
                    );
                    if !targets.is_empty() {
                        let count_s = targets.len().to_string();
                        let sample = targets
                            .iter()
                            .take(4)
                            .map(|t| t.path.as_str())
                            .collect::<Vec<_>>()
                            .join("\n");
                        self.modal_stack.push(ModalLayer::CriticalDialog {
                            title: self.i18n.t("dialog.discard.title").to_string(),
                            body: self.i18n.t_fmt(
                                "dialog.discard.body",
                                &[("count", &count_s), ("sample", &sample)],
                            ),
                            confirm_label: self.i18n.t("common.discard").to_string(),
                            cancel_label: self.i18n.cancel().to_string(),
                            selected_button: ModalButton::Cancel,
                            confirm_effect: Some(Effect::DiscardTargets(targets)),
                        });
                    }
                }
            }
            Action::StashAll => {
                effects.push(Effect::RunCli {
                    args: vec!["stash".into(), "push".into()],
                    stdin_payload: None,
                    is_read_only: false,
                    refresh_after: RefreshScope::All,
                });
            }
            Action::ResolveConflictsNotice => {
                if let Some(status) = &self.stage_status
                    && !status.conflicted.is_empty()
                {
                    let paths = status
                        .conflicted
                        .iter()
                        .map(|e| format!("  ! {}", e.path))
                        .collect::<Vec<_>>()
                        .join("\n");
                    self.modal_stack.push(ModalLayer::CriticalDialog {
                        title: self.i18n.t("dialog.conflicts_notice.title").to_string(),
                        body: self
                            .i18n
                            .t_fmt("dialog.conflicts_notice.body", &[("paths", &paths)]),
                        confirm_label: self.i18n.ok().to_string(),
                        cancel_label: self.i18n.close().to_string(),
                        selected_button: ModalButton::Confirm,
                        confirm_effect: None,
                    });
                }
            }
            Action::StageDiffMoveCursor(delta) => {
                let len = self.stage_workbench.diff_rows().len();
                if len > 0 {
                    self.stage_workbench.diff_cursor_row =
                        (self.stage_workbench.diff_cursor_row as isize + *delta)
                            .clamp(0, len as isize - 1) as usize;
                    let c = self.stage_workbench.diff_cursor_row;
                    if c < self.stage_workbench.diff_scroll {
                        self.stage_workbench.diff_scroll = c;
                    } else if c >= self.stage_workbench.diff_scroll + 18 {
                        self.stage_workbench.diff_scroll = c.saturating_sub(17);
                    }
                }
            }
            Action::StageDiffScroll(delta) => {
                let len = self.stage_workbench.diff_rows().len();
                self.stage_workbench.diff_scroll = (self.stage_workbench.diff_scroll as isize
                    + *delta)
                    .clamp(0, len.saturating_sub(1) as isize)
                    as usize;
            }
            Action::StageDiffJumpHunk(dir) => {
                let headers = self.stage_workbench.hunk_header_rows();
                if !headers.is_empty() {
                    let cur = self.stage_workbench.diff_cursor_row;
                    let target = if *dir >= 0 {
                        headers
                            .iter()
                            .copied()
                            .find(|&h| h > cur)
                            .unwrap_or(headers[0])
                    } else {
                        headers
                            .iter()
                            .copied()
                            .rev()
                            .find(|&h| h < cur)
                            .unwrap_or(*headers.last().unwrap())
                    };
                    self.stage_workbench.diff_cursor_row = target;
                    self.stage_workbench.diff_scroll = target;
                }
            }
            Action::StageDiffNextFile(delta) => {
                self.stage_workbench.sync_rows(self.stage_status.as_deref());
                let file_indices: Vec<usize> = self
                    .stage_workbench
                    .rows
                    .iter()
                    .enumerate()
                    .filter(|(_, r)| {
                        matches!(
                            r.kind,
                            StageTreeRowKind::File { .. } | StageTreeRowKind::AmendFile { .. }
                        )
                    })
                    .map(|(i, _)| i)
                    .collect();
                if !file_indices.is_empty() {
                    let next_pos = match file_indices
                        .iter()
                        .position(|&i| i == self.commit_stage_selected_idx)
                    {
                        Some(pos) => (pos as isize + *delta)
                            .clamp(0, file_indices.len() as isize - 1) as usize,
                        None if *delta >= 0 => file_indices
                            .iter()
                            .position(|&i| i > self.commit_stage_selected_idx)
                            .unwrap_or(0),
                        None => file_indices
                            .iter()
                            .rposition(|&i| i < self.commit_stage_selected_idx)
                            .unwrap_or(file_indices.len() - 1),
                    };
                    self.commit_stage_selected_idx = file_indices[next_pos];
                    self.stage_workbench.multi_selected.clear();
                }
            }
            Action::StageDiffToggleVisual => {
                if self.stage_workbench.visual_anchor_row.is_some() {
                    self.stage_workbench.visual_anchor_row = None;
                } else {
                    self.stage_workbench.visual_anchor_row =
                        Some(self.stage_workbench.diff_cursor_row);
                }
            }
            Action::StageDiffToggleSideBySide => {
                self.stage_workbench.diff_side_by_side =
                    !self.stage_workbench.diff_side_by_side;
                self.stage_workbench.visual_anchor_row = None;
            }
            Action::StageDiffCycleContext => {
                self.stage_workbench.diff_context = self.stage_workbench.diff_context.next();
                self.stage_workbench.requested_diff = None;
                self.stage_workbench.visual_anchor_row = None;
            }
            Action::StageDiffSwitchSide => {
                if let Some(key) = &self.stage_workbench.current_diff_key {
                    let target_group = match key.side {
                        StageDiffSide::Staged => StageNodeKind::Unstaged,
                        StageDiffSide::Unstaged => StageNodeKind::Staged,
                        StageDiffSide::AmendCommit => return true,
                    };
                    let target_path = key.path.clone();
                    if let Some(pos) = self.stage_workbench.rows.iter().position(|r| {
                        matches!(
                            &r.kind,
                            StageTreeRowKind::File { group_kind, path, .. }
                            if *group_kind == target_group && *path == target_path
                        )
                    }) {
                        self.commit_stage_selected_idx = pos;
                    }
                }
            }
            Action::StageDiffStageSelection => {
                if let Some(sel) = self.stage_workbench.collect_selected_line_sets() {
                    self.apply_stage_diff_selection(StageDiffOp::Stage, sel, effects);
                }
            }
            Action::StageDiffUnstageSelection => {
                if let Some(sel) = self.stage_workbench.collect_selected_line_sets() {
                    self.apply_stage_diff_selection(StageDiffOp::Unstage, sel, effects);
                }
            }
            Action::StageDiffRollbackSelection => {
                if let Some(sel) = self.stage_workbench.collect_selected_line_sets() {
                    self.apply_stage_diff_selection(StageDiffOp::Rollback, sel, effects);
                }
            }
            Action::StageDiffUndoRollback => {
                if let Some(entry) = self.stage_workbench.rollback_undo_stack.pop() {
                    self.status_notice = Some(
                        self.i18n
                            .t_fmt("toast.undid_rollback", &[("path", &entry.path)]),
                    );
                    effects.push(Effect::WriteWorktreeBuffer {
                        path: entry.path,
                        new_local_bytes: entry.prev_local_bytes,
                    });
                }
            }
            Action::OpenThreeWayInspector { row_idx, range_idx } => {
                if let Some(r) = *row_idx {
                    self.stage_workbench.diff_cursor_row = r;
                }
                self.stage_workbench.three_way_range_idx = *range_idx;
                self.modal_stack.push(ModalLayer::FloatingPopup {
                    kind: PopupKind::ThreeWayInspector,
                    query: String::new(),
                    selected: range_idx.unwrap_or(0),
                });
            }
            Action::ApplyStageDiffHunk(hunk_idx, kind) => {
                let hunk_idx = *hunk_idx;
                match kind {
                    DiffHunkActionKind::Inspect3Way => {
                        if let Some(diff) = &self.stage_workbench.current_diff
                            && let Some(hunk) = diff.hunks.get(hunk_idx)
                            && let Some(sr_idx) =
                                hunk.line_meta.iter().find_map(|m| m.staged_range_idx)
                        {
                            let header_row = self
                                .stage_workbench
                                .diff_rows()
                                .iter()
                                .position(|r| matches!(r, StageDiffRow::Header(h) if *h == hunk_idx));
                            if let Some(hr) = header_row {
                                self.stage_workbench.diff_cursor_row = hr;
                            }
                            self.stage_workbench.three_way_range_idx = Some(sr_idx);
                            self.modal_stack.push(ModalLayer::FloatingPopup {
                                kind: PopupKind::ThreeWayInspector,
                                query: String::new(),
                                selected: sr_idx,
                            });
                        }
                    }
                    DiffHunkActionKind::Stage => {
                        if let Some(sel) = self.stage_workbench.collect_hunk_line_sets(hunk_idx) {
                            self.apply_stage_diff_selection(StageDiffOp::Stage, sel, effects);
                        }
                    }
                    DiffHunkActionKind::Unstage => {
                        if let Some(sel) = self.stage_workbench.collect_hunk_line_sets(hunk_idx) {
                            self.apply_stage_diff_selection(StageDiffOp::Unstage, sel, effects);
                        }
                    }
                    DiffHunkActionKind::Rollback => {
                        if let Some(sel) = self.stage_workbench.collect_hunk_line_sets(hunk_idx) {
                            self.apply_stage_diff_selection(StageDiffOp::Rollback, sel, effects);
                        }
                    }
                }
            }
            Action::CommitEditorChar(ch) => {
                self.stage_workbench.empty_message_error = false;
                self.stage_workbench.editor.insert_char(*ch);
                self.stage_workbench.save_draft();
            }
            Action::CommitEditorNewline => {
                self.stage_workbench.empty_message_error = false;
                self.stage_workbench.editor.insert_newline();
                self.stage_workbench.save_draft();
            }
            Action::CommitEditorPaste(text) => {
                self.stage_workbench.empty_message_error = false;
                self.stage_workbench.editor.insert_paste(text);
                self.stage_workbench.save_draft();
            }
            Action::CommitEditorBackspace => {
                self.stage_workbench.empty_message_error = false;
                self.stage_workbench.editor.backspace();
                self.stage_workbench.save_draft();
            }
            Action::CommitEditorDelete => {
                self.stage_workbench.editor.delete_forward();
                self.stage_workbench.save_draft();
            }
            Action::CommitEditorDeleteWord => {
                self.stage_workbench.editor.delete_word_backward();
                self.stage_workbench.save_draft();
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
                    self.stage_workbench.save_draft();
                }
            }
            Action::CommitEditorRedo => {
                if self.stage_workbench.editor.redo() {
                    self.stage_workbench.save_draft();
                }
            }
            Action::CommitToggleAmend => {
                let next = !self.stage_workbench.amend;
                self.stage_workbench.amend = next;
                self.stage_workbench.empty_message_error = false;
                if next {
                    self.stage_workbench.saved_draft = Some(self.stage_workbench.editor.text());
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
                    self.stage_workbench.save_draft();
                }
                self.stage_workbench.sync_rows(self.stage_status.as_deref());
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
                    if !self.stage_workbench.amend && !prev.trim().is_empty() && prev.trim() != msg.trim() {
                        self.stage_workbench.push_history(&prev);
                    }
                    self.stage_workbench.editor.set_text(&msg);
                    self.stage_workbench.empty_message_error = false;
                    self.stage_workbench.save_draft();
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

    fn jump_to_stage_parent(&mut self, target_depth: u16) {
        if let Some(parent_idx) = self.stage_workbench.rows[..self.commit_stage_selected_idx]
            .iter()
            .rposition(|r| r.depth == target_depth)
        {
            self.commit_stage_selected_idx = parent_idx;
        }
    }

    fn apply_stage_diff_selection(
        &mut self,
        op: StageDiffOp,
        sel: LineSelection,
        effects: &mut Vec<Effect>,
    ) {
        let Some(diff) = &self.stage_workbench.current_diff else {
            return;
        };
        if diff.key.side == StageDiffSide::AmendCommit {
            return;
        }
        match op {
            StageDiffOp::Stage if !sel.staged_side => {
                let new_staged = diff.tracker.stage_lines(&sel);
                self.stage_workbench.visual_anchor_row = None;
                effects.push(Effect::ApplyStageBuffer {
                    path: diff.key.path.clone(),
                    new_staged_bytes: new_staged,
                    file_mode: diff.file_mode,
                    remove_from_index: false,
                });
            }
            StageDiffOp::Unstage if sel.staged_side => {
                let new_staged = diff.tracker.unstage_lines(&sel);
                let remove = !diff.vcs_exists && new_staged.is_empty();
                self.stage_workbench.visual_anchor_row = None;
                effects.push(Effect::ApplyStageBuffer {
                    path: diff.key.path.clone(),
                    new_staged_bytes: new_staged,
                    file_mode: diff.file_mode,
                    remove_from_index: remove,
                });
            }
            StageDiffOp::Rollback if !sel.staged_side => {
                let new_local = diff.tracker.rollback_lines(&sel);
                self.stage_workbench
                    .rollback_undo_stack
                    .push(RollbackUndoEntry {
                        path: diff.key.path.clone(),
                        prev_local_bytes: diff.local_bytes.clone(),
                    });
                self.stage_workbench.visual_anchor_row = None;
                self.status_notice = Some(
                    self.i18n
                        .t_fmt("toast.rolled_back_lines", &[("path", &diff.key.path)]),
                );
                effects.push(Effect::WriteWorktreeBuffer {
                    path: diff.key.path.clone(),
                    new_local_bytes: new_local,
                });
            }
            _ => {}
        }
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

    pub fn reconcile_stage_requests(&mut self, effects: &mut Vec<Effect>) {
        self.stage_workbench.sync_rows(self.stage_status.as_deref());
        let total = self.stage_workbench.rows.len();
        if total == 0 {
            self.commit_stage_selected_idx = 0;
        } else if self.commit_stage_selected_idx >= total {
            self.commit_stage_selected_idx = total - 1;
        }

        if self.active_tab != WorkspaceTab::Commit || self.stage_workbench.diff_collapsed {
            return;
        }

        let desired = self
            .stage_workbench
            .desired_diff_key(self.commit_stage_selected_idx, self.stage_status.as_deref());

        if desired != self.stage_workbench.current_diff_key {
            self.stage_workbench.current_diff_key = desired.clone();
            self.stage_workbench.diff_scroll = 0;
            self.stage_workbench.diff_cursor_row = 0;
            self.stage_workbench.visual_anchor_row = None;
            self.stage_workbench.current_diff = None;
            self.stage_workbench.diff_syntax = None;
            if let Some(k) = desired {
                self.stage_workbench.requested_diff = Some(k.clone());
                effects.push(Effect::RequestStageFileDiff(k));
            } else {
                self.stage_workbench.requested_diff = None;
            }
        } else if let Some(k) = desired
            && self.stage_workbench.current_diff.is_none()
            && self.stage_workbench.requested_diff.as_ref() != Some(&k)
        {
            self.stage_workbench.requested_diff = Some(k.clone());
            effects.push(Effect::RequestStageFileDiff(k));
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

                let old_rows = self.stage_workbench.rows.clone();
                let old_sel = self.commit_stage_selected_idx;
                self.stage_status = Some(status);
                self.stage_workbench.sync_rows(self.stage_status.as_deref());
                if !old_rows.is_empty() {
                    self.commit_stage_selected_idx = self
                        .stage_workbench
                        .smooth_anchor_after_rows_update(&old_rows, old_sel);
                } else {
                    self.commit_stage_selected_idx = 0;
                }
                self.stage_workbench.requested_diff = None;
                if let Some(k) = self
                    .stage_workbench
                    .desired_diff_key(self.commit_stage_selected_idx, self.stage_status.as_deref())
                {
                    self.stage_workbench.current_diff_key = Some(k.clone());
                    self.stage_workbench.requested_diff = Some(k.clone());
                    effects.push(Effect::RequestStageFileDiff(k));
                }
                None
            }
            EngineEvent::StageFileDiffLoaded { diff, epoch } => {
                if epoch < self.latest_stage_diff_epoch {
                    return None;
                }
                self.latest_stage_diff_epoch = epoch;
                if self.stage_workbench.requested_diff.as_ref() == Some(&diff.key) {
                    self.stage_workbench.requested_diff = None;
                }
                if self.stage_workbench.current_diff_key.as_ref() == Some(&diff.key) {
                    if self.config.jbgit.diff.syntax_highlight
                        && !diff.is_binary
                        && !diff.hunks.is_empty()
                    {
                        let hl = Arc::new(HighlightedDiff::build(&diff.as_file_diff()));
                        self.stage_workbench.diff_syntax = Some(hl);
                    } else {
                        self.stage_workbench.diff_syntax = None;
                    }
                    self.stage_workbench.current_diff = Some(diff);
                    let len = self.stage_workbench.diff_rows().len();
                    if len == 0 {
                        self.stage_workbench.diff_cursor_row = 0;
                        self.stage_workbench.diff_scroll = 0;
                    } else {
                        self.stage_workbench.diff_cursor_row =
                            self.stage_workbench.diff_cursor_row.min(len - 1);
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
                self.stage_workbench.sync_rows(self.stage_status.as_deref());
                None
            }
            EngineEvent::CommitPrecheckLoaded { info } => {
                self.stage_workbench.precheck = info;
                None
            }
            EngineEvent::CommitCompleted(outcome) => {
                self.stage_workbench.commit_in_progress = false;
                if outcome.success {
                    let was_amend = self.stage_workbench.amend;
                    let committed_msg = self.stage_workbench.editor.text();
                    self.stage_workbench.push_history(&committed_msg);
                    self.stage_workbench.editor.clear();
                    self.stage_workbench.amend = false;
                    self.stage_workbench.amend_awaiting_message = false;
                    self.stage_workbench.saved_draft = None;
                    self.stage_workbench.amend_preview = None;
                    self.stage_workbench.empty_message_error = false;
                    self.stage_workbench.save_draft();
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
                    self.hook_error_output = Some(
                        outcome
                            .error_output
                            .unwrap_or(outcome.summary),
                    );
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
                    self.stage_workbench.save_draft();
                }
                self.status_notice =
                    Some(self.i18n.t("toast.undid_last_commit").to_string());
                None
            }
            other => Some(other),
        }
    }
}
