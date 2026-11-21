mod commit_flow;
mod events;
mod partial_diff;

use jbgit_core::{ModalButton, RefreshScope, StageNodeKind, StageTreeRowKind};

use crate::app::{Action, AppState, Effect, ModalLayer};

impl AppState {
    pub(crate) fn dispatch_stage_action(
        &mut self,
        action: &Action,
        effects: &mut Vec<Effect>,
    ) -> bool {
        if self.dispatch_stage_tree_action(action, effects) {
            return true;
        }
        if self.dispatch_stage_diff_action(action, effects) {
            return true;
        }
        self.dispatch_commit_editor_action(action, effects)
    }

    fn dispatch_stage_tree_action(&mut self, action: &Action, effects: &mut Vec<Effect>) -> bool {
        match action {
            Action::SelectCommitStageRow(idx) => {
                self.stage_workbench.sync_rows(self.stage_status.as_ref());
                let total = self.stage_item_count();
                if total > 0 {
                    self.commit_stage_selected_idx = (*idx).min(total - 1);
                    self.stage_workbench.multi_selected.clear();
                    self.stage_workbench.pending_select_immediate = true;
                }
            }
            Action::ScrollCommitStage(delta) => {
                self.stage_workbench.sync_rows(self.stage_status.as_ref());
                let total = self.stage_item_count();
                if total > 0 {
                    self.commit_stage_selected_idx =
                        (self.commit_stage_selected_idx as isize + *delta)
                            .clamp(0, (total - 1) as isize) as usize;
                    self.stage_workbench.multi_selected.clear();
                    self.stage_workbench.pending_select_immediate = false;
                }
            }
            Action::StageExpand(expand) => {
                let expand = *expand;
                self.stage_workbench.sync_rows(self.stage_status.as_ref());
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
                        self.stage_workbench.sync_rows(self.stage_status.as_ref());
                    } else if row.is_container() {
                        if row.expanded != expand {
                            if expand {
                                self.stage_workbench.collapsed.remove(&row.key);
                            } else {
                                self.stage_workbench.collapsed.insert(row.key.clone());
                            }
                            self.stage_workbench.sync_rows(self.stage_status.as_ref());
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
                self.stage_workbench.sync_rows(self.stage_status.as_ref());
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
                        self.stage_workbench.sync_rows(self.stage_status.as_ref());
                    } else if row.is_container() {
                        if row.expanded {
                            self.stage_workbench.collapsed.insert(row.key.clone());
                        } else {
                            self.stage_workbench.collapsed.remove(&row.key);
                        }
                        self.stage_workbench.sync_rows(self.stage_status.as_ref());
                    }
                }
            }
            Action::StageToggleMulti(idx) => {
                let idx = *idx;
                self.stage_workbench.sync_rows(self.stage_status.as_ref());
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
                self.stage_workbench.sync_rows(self.stage_status.as_ref());
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
                self.stage_workbench.sync_rows(self.stage_status.as_ref());
                self.commit_stage_selected_idx = 0;
            }
            Action::StageExpandAll(expand) => {
                if *expand {
                    self.stage_workbench.collapsed.clear();
                    self.stage_workbench.tree_opts.untracked_many_expanded = true;
                } else {
                    self.stage_workbench.tree_opts.untracked_many_expanded = false;
                    self.stage_workbench.sync_rows(self.stage_status.as_ref());
                    let keys: Vec<String> = self
                        .stage_workbench
                        .rows
                        .iter()
                        .filter(|r| matches!(r.kind, StageTreeRowKind::Dir { .. }))
                        .map(|r| r.key.clone())
                        .collect();
                    self.stage_workbench.collapsed.extend(keys);
                }
                self.stage_workbench.sync_rows(self.stage_status.as_ref());
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
                    && self.focused_pane == jbgit_core::PaneId::CommitDiffPreview
                {
                    self.focused_pane = jbgit_core::PaneId::CommitChangesTree;
                }
            }
            Action::StageActionSelected => {
                self.stage_workbench.sync_rows(self.stage_status.as_ref());
                if let Some(status) = &self.stage_status {
                    let targets = self.stage_workbench.collect_targets_for_action(
                        self.commit_stage_selected_idx,
                        status,
                        StageNodeKind::can_stage,
                    );
                    if !targets.is_empty() {
                        for t in &targets {
                            self.stage_workbench.invalidate_diff_cache_for_path(&t.path);
                            if let Some(old) = &t.old_path {
                                self.stage_workbench.invalidate_diff_cache_for_path(old);
                            }
                        }
                        self.stage_workbench.pending_select_immediate = true;
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
                self.stage_workbench.sync_rows(self.stage_status.as_ref());
                if let Some(status) = &self.stage_status {
                    let targets = self.stage_workbench.collect_targets_for_action(
                        self.commit_stage_selected_idx,
                        status,
                        StageNodeKind::can_unstage,
                    );
                    if !targets.is_empty() {
                        for t in &targets {
                            self.stage_workbench.invalidate_diff_cache_for_path(&t.path);
                            if let Some(old) = &t.old_path {
                                self.stage_workbench.invalidate_diff_cache_for_path(old);
                            }
                        }
                        self.stage_workbench.pending_select_immediate = true;
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
                self.stage_workbench.invalidate_all_diff_caches();
                self.stage_workbench.pending_select_immediate = true;
                effects.push(Effect::StageAllTracked);
                self.status_notice =
                    Some(self.i18n.t("toast.staging_all_tracked").to_string());
            }
            Action::DiscardPromptSelected => {
                self.stage_workbench.sync_rows(self.stage_status.as_ref());
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
}
