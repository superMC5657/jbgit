use jbgit_core::{LineSelection, StageNodeKind, StageTreeRowKind};
use jbgit_engine::StageDiffSide;

use crate::app::{Action, AppState, Effect, ModalLayer, PopupKind, ScrollPane, apply_scroll_delta};
use crate::hit_test::DiffHunkActionKind;
use crate::stage_view::{RollbackUndoEntry, StageDiffRow};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StageDiffOp {
    Stage,
    Unstage,
    Rollback,
}

impl AppState {
    pub(super) fn dispatch_stage_diff_action(
        &mut self,
        action: &Action,
        effects: &mut Vec<Effect>,
    ) -> bool {
        match action {
            Action::StageDiffMoveCursor(delta) => {
                let len = self.stage_workbench.diff_rows().len();
                if len > 0 {
                    self.stage_workbench.diff_cursor_row =
                        apply_scroll_delta(self.stage_workbench.diff_cursor_row, *delta, len - 1);
                    let viewport_h = self
                        .scroll_metrics
                        .stage_diff
                        .map(|b| b.viewport_h)
                        .filter(|&h| h > 0)
                        .unwrap_or(18);
                    let c = self.stage_workbench.diff_cursor_row;
                    if c < self.stage_workbench.diff_scroll {
                        self.stage_workbench.diff_scroll = c;
                    } else if c >= self.stage_workbench.diff_scroll + viewport_h {
                        self.stage_workbench.diff_scroll =
                            c.saturating_sub(viewport_h.saturating_sub(1));
                    }
                }
            }
            Action::StageDiffScroll(delta) => {
                self.scroll_pane(ScrollPane::StageDiff, *delta);
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
                self.stage_workbench.sync_rows(self.stage_status.as_ref());
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
                self.stage_workbench.diff_side_by_side = !self.stage_workbench.diff_side_by_side;
                self.stage_workbench.visual_anchor_row = None;
            }
            Action::StageDiffCycleContext => {
                self.stage_workbench.diff_context = self.stage_workbench.diff_context.next();
                self.stage_workbench.requested_diff = None;
                self.stage_workbench.pending_diff = None;
                self.stage_workbench.visual_anchor_row = None;
                self.stage_workbench.pending_select_immediate = true;
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
                        self.stage_workbench.pending_select_immediate = true;
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
                    self.stage_workbench.invalidate_diff_cache_for_path(&entry.path);
                    self.stage_workbench.pending_select_immediate = true;
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
            _ => return false,
        }
        true
    }

    fn apply_stage_diff_selection(
        &mut self,
        op: StageDiffOp,
        sel: LineSelection,
        effects: &mut Vec<Effect>,
    ) {
        let Some(diff) = self.stage_workbench.current_diff.as_ref().cloned() else {
            return;
        };
        if diff.key.side == StageDiffSide::AmendCommit {
            return;
        }
        match op {
            StageDiffOp::Stage if !sel.staged_side => {
                let new_staged = diff.tracker.stage_lines(&sel);
                self.stage_workbench.visual_anchor_row = None;
                self.stage_workbench.invalidate_diff_cache_for_path(&diff.key.path);
                self.stage_workbench.pending_select_immediate = true;
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
                self.stage_workbench.invalidate_diff_cache_for_path(&diff.key.path);
                self.stage_workbench.pending_select_immediate = true;
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
                self.stage_workbench.invalidate_diff_cache_for_path(&diff.key.path);
                self.stage_workbench.pending_select_immediate = true;
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
}
