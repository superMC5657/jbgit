//! Git action dispatch: Git write operations, comparison / conflict-merge
//! workflows, and follow-up dialogs driven by operation outcomes (Smart
//! Operation, unmerged delete guard, merged-branch cleanup, `[Restore Branch]`).

use std::collections::{BTreeSet, HashSet};

use crossterm::event::{KeyCode, KeyModifiers};
use jbgit_core::{
    ConflictChoice, DiffContext, FastForward, FileDiffKey, GitFailure, GitOp, GitOpOutcome,
    MergeOptions, Oid, OngoingKind, OngoingStep, PaneId, PullOptions, PushForce, RebaseOptions,
    RefKind, RepoSnapshot, RepositoryState, RequestSlotId, RewritePlan, SpecialConflictInfo,
    SpecialConflictKind, StageNodeKind, StageTreeRowKind, UpdateInfo, UpdateMode, WorkspaceTab,
    conflict_side_statuses, is_protected_branch, is_reverse_conflict_state,
    matches_protected_branch_patterns, parse_push_porcelain,
};

use crate::app::{
    Action, AppState, ConflictInlineEditor, ConflictMergeState, ConflictQueueEntry,
    ConflictQueueState, Effect, ModalLayer, Notification,
};
use crate::branches_popup::first_selectable_row;
use crate::dialogs::{
    ChoiceDialog, ChoiceOption, DeleteTagRemoteProbe, Dialog, InputDialog, InputIntent,
    InputValidation, MergeDialog, PullDialog, RebaseDialog,
};
use crate::log_view::PushDialogState;

/// Stash message used by the engine's Update Project save step.
const UPDATE_PROJECT_STASH_MESSAGE: &str = "jbgit update project";

fn format_byte_size(bytes: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = 1024 * 1024;
    if bytes >= MIB {
        format!("{:.1} MiB", bytes as f64 / MIB as f64)
    } else if bytes >= KIB {
        format!("{:.1} KiB", bytes as f64 / KIB as f64)
    } else {
        format!("{bytes} B")
    }
}

impl AppState {
    pub(crate) fn snapshot(&self) -> Option<&RepoSnapshot> {
        self.repo_snapshot.as_deref()
    }

    pub(crate) fn ongoing_kind(&self) -> Option<OngoingKind> {
        self.snapshot()
            .and_then(|s| OngoingKind::from_state(&s.state))
    }

    pub(crate) fn is_bisecting(&self) -> bool {
        self.snapshot()
            .is_some_and(|s| s.state == jbgit_core::RepositoryState::Bisect)
    }

    pub(crate) fn conflicted_count(&self) -> usize {
        self.stage_workbench
            .status()
            .map_or(0, |s| s.conflicted.len())
    }

    pub(crate) fn has_staged_changes(&self) -> bool {
        self.stage_workbench
            .status()
            .is_some_and(|s| !s.staged.is_empty())
    }

    /// Fails with a user-facing reason when a write operation must not start.
    pub(crate) fn require_no_ongoing(&self) -> Result<(), String> {
        match self.ongoing_kind() {
            Some(k) => {
                let kind_title = self.i18n.ongoing_title(k);
                let kind_str = if self.i18n.lang == "en-US" {
                    kind_title.to_lowercase()
                } else {
                    kind_title.to_string()
                };
                Err(self
                    .i18n
                    .t_fmt("toast.ongoing_blocked", &[("kind", &kind_str)]))
            }
            None => Ok(()),
        }
    }

    pub(crate) fn is_ancestor_of_head(&self, oid: Oid) -> bool {
        self.snapshot()
            .and_then(|s| s.head_oid.map(|h| s.reaches(h, oid)))
            .unwrap_or(false)
    }

    /// Plan for rewriting `oldest..HEAD` along the first-parent chain.
    pub(crate) fn rewrite_plan(&self, oldest: Oid, verb: &str) -> Result<RewritePlan, String> {
        let snap = self
            .snapshot()
            .ok_or_else(|| self.i18n.t("toast.repo_not_loaded").to_string())?;
        self.require_no_ongoing()?;
        snap.plan_rewrite_typed(oldest)
            .map_err(|err| self.i18n.rewrite_plan_error(&err, verb))
    }

    /// Selected Log commits ordered oldest → newest (topological).
    pub(crate) fn selected_commits_oldest_first(&self) -> Vec<Oid> {
        let Some(snap) = self.snapshot() else {
            return Vec::new();
        };
        let selected: HashSet<Oid> = self
            .log
            .graph
            .selected_oids(self.repo_snapshot.as_ref())
            .into_iter()
            .collect();
        snap.commits
            .iter()
            .rev()
            .filter(|c| selected.contains(&c.oid))
            .map(|c| c.oid)
            .collect()
    }

    fn start_git_op(&mut self, op: GitOp, effects: &mut Vec<Effect>) {
        let label = self.i18n.git_op_label(&op);
        self.status_notice = Some(self.i18n.t_fmt("toast.op_running", &[("label", &label)]));
        self.notification = None;
        effects.push(Effect::GitOp(op));
    }

    pub(super) fn push_dialog(&mut self, d: Dialog) {
        self.push_modal(ModalLayer::Dialog(Box::new(d)));
    }

    fn run_actions(&mut self, actions: Vec<Action>, effects: &mut Vec<Effect>) {
        for a in actions {
            effects.extend(self.dispatch(a));
        }
    }

    pub(crate) fn dispatch_git_action(
        &mut self,
        action: &Action,
        effects: &mut Vec<Effect>,
    ) -> bool {
        match action {
            Action::OpenSubmenu {
                parent,
                items,
                depth,
            } => {
                while matches!(
                    self.modal_stack.last(),
                    Some(ModalLayer::ContextMenu { depth: d, .. }) if *d >= *depth
                ) {
                    self.modal_stack.pop();
                }
                if !items.is_empty() {
                    let selected = crate::app::initial_menu_selection(items);
                    self.modal_stack.push(ModalLayer::ContextMenu {
                        anchor: (parent.rect.right(), parent.row_y),
                        items: items.clone(),
                        selected,
                        depth: *depth,
                        parent: Some(*parent),
                    });
                }
            }
            Action::Notice(text) => self.status_notice = Some(text.clone()),
            Action::RunGitOp(op) => self.start_git_op(op.clone(), effects),
            Action::OpenDialog(d) => {
                self.push_dialog((**d).clone());
                self.sync_rebase_todo_preview(effects);
            }
            Action::DialogConfirm => {
                if self.answer_credential(true) {
                    return true;
                }
                let Some(ModalLayer::Dialog(d)) = self.modal_stack.last() else {
                    return true;
                };
                match d.confirm_actions(&self.i18n) {
                    Some(actions) => {
                        self.modal_stack.pop();
                        self.run_actions(actions, effects);
                    }
                    None => {
                        if let Dialog::Input(i) = d.as_ref()
                            && let Some(err) = i.error(&self.i18n)
                        {
                            self.status_notice = Some(err);
                        } else if let Dialog::RebaseTodo(r) = d.as_ref()
                            && let Some(err) = r.error(&self.i18n)
                        {
                            self.status_notice = Some(err);
                        } else if let Dialog::Merge(m) = d.as_ref()
                            && let Some(err) = m.error(&self.i18n)
                        {
                            self.status_notice = Some(err);
                        } else if let Dialog::Pull(p) = d.as_ref()
                            && let Some(err) = p.error(&self.i18n)
                        {
                            self.status_notice = Some(err);
                        } else if let Dialog::Rebase(r) = d.as_ref()
                            && let Some(err) = r.error(&self.i18n)
                        {
                            self.status_notice = Some(err);
                        } else if let Dialog::CreatePatch(cp) = d.as_ref()
                            && let Some(err) = cp.error(&self.i18n)
                        {
                            self.status_notice = Some(err);
                        } else if let Dialog::Identity(id) = d.as_ref()
                            && let Some(err) = id.error(&self.i18n)
                        {
                            self.status_notice = Some(err);
                        }
                    }
                }
            }
            Action::SetTodoMessage { idx, message } => {
                if let Some(ModalLayer::Dialog(d)) = self
                    .modal_stack
                    .iter_mut()
                    .rev()
                    .find(|l| matches!(l, ModalLayer::Dialog(d) if matches!(d.as_ref(), Dialog::RebaseTodo(_))))
                    && let Dialog::RebaseTodo(r) = d.as_mut()
                    && let Some(e) = r.entries.get_mut(*idx)
                {
                    e.action = jbgit_core::TodoAction::Reword;
                    e.message = Some(message.clone());
                }
            }
            Action::OpenBranchesPopup { query } => {
                self.modal_stack
                    .retain(|l| !matches!(l, ModalLayer::ContextMenu { .. }));
                let selected = first_selectable_row(self, query);
                self.push_modal(ModalLayer::BranchesPopup {
                    query: query.clone(),
                    selected,
                });
            }
            Action::ToggleFavoriteRef { full, short } => {
                let on = self.log.branches.settings.favorites.toggle(full, short);
                self.log.branches.save_settings();
                let snap = self.repo_snapshot.clone();
                self.log.branches.sync_model(snap.as_ref(), true);
                self.status_notice = Some(self.i18n.t_fmt(
                    if on {
                        "toast.favorite_added"
                    } else {
                        "toast.favorite_removed"
                    },
                    &[("name", short)],
                ));
            }
            Action::CheckoutRemoteBranch { remote_ref } => self.checkout_remote(remote_ref, effects),
            Action::CheckoutAndRebaseRemoteBranch { remote_ref } => {
                self.checkout_and_rebase_remote(remote_ref, effects);
            }
            Action::CheckoutRevisionExpr(expr) => {
                let is_local = self.snapshot().is_some_and(|s| s.local_branch(expr).is_some());
                self.start_git_op(
                    GitOp::Checkout {
                        target: expr.clone(),
                        detach: !is_local,
                    },
                    effects,
                );
            }
            Action::CompareWithCurrent { branch } => self.compare_with_current(branch),
            Action::PromptCompareWithBranch { left } => self.prompt_compare_with_branch(left),
            Action::CompareBranches { left, right } => self.compare_branches(left, right),
            Action::CycleBranchCompareDirection => {
                let _ = self.log.cycle_branch_compare_direction(
                    self.repo_snapshot.as_ref(),
                    &self.branch_colors,
                    self.async_graph_enabled,
                    &self.i18n,
                    &mut self.status_notice,
                );
            }
            Action::ToggleBranchCompareTreeDiff => {
                if self
                    .log
                    .toggle_branch_compare_tree_diff(&self.i18n, &mut self.status_notice)
                    && self
                        .log
                        .branch_compare
                        .as_ref()
                        .is_some_and(|bc| bc.tree_diff)
                {
                    self.set_focus(WorkspaceTab::Log, PaneId::LogChangesTree);
                }
            }
            Action::CloseBranchCompare => {
                self.log.close_branch_compare(
                    self.repo_snapshot.as_ref(),
                    &self.branch_colors,
                    self.async_graph_enabled,
                );
            }
            Action::PromptSetUpstream { branch } => self.prompt_set_upstream(branch),
            Action::OpenBranchCleanup => self.open_branch_cleanup(),
            Action::BranchCleanupMove(delta) => {
                if let Some(bc) = self.modal_stack.branch_cleanup_mut() {
                    bc.move_by(*delta);
                }
            }
            Action::BranchCleanupSelect(idx) => {
                if let Some(bc) = self.modal_stack.branch_cleanup_mut() {
                    bc.select(*idx);
                }
            }
            Action::BranchCleanupToggle => {
                if let Some(bc) = self.modal_stack.branch_cleanup_mut() {
                    bc.toggle_current();
                }
            }
            Action::BranchCleanupToggleAll => {
                if let Some(bc) = self.modal_stack.branch_cleanup_mut() {
                    bc.toggle_all();
                }
            }
            Action::BranchCleanupCycleBase => {
                let snap = self.repo_snapshot.clone();
                let extra = self.config.jbgit.push.protected_branches.clone();
                if let Some(snap) = snap.as_deref()
                    && let Some(bc) = self.modal_stack.branch_cleanup_mut()
                {
                    bc.cycle_base(snap, &extra);
                }
            }
            Action::BranchCleanupConfirm => {
                let op = self
                    .modal_stack
                    .branch_cleanup_mut()
                    .and_then(|bc| bc.delete_op());
                if let Some(op) = op {
                    if let GitOp::Sequence(ops) = &op {
                        self.pending_batch_branch_deletes = ops
                            .iter()
                            .filter_map(|o| match o {
                                GitOp::DeleteBranch { name, tip, .. } => Some((name.clone(), *tip)),
                                _ => None,
                            })
                            .collect();
                    } else {
                        self.pending_batch_branch_deletes.clear();
                    }
                    self.modal_stack.pop();
                    self.start_git_op(op, effects);
                }
            }
            Action::CompareVersions { base, target } => {
                self.set_focus(WorkspaceTab::Log, PaneId::LogChangesTree);
                self.log
                    .start_compare_versions(*base, *target, &self.i18n, &mut self.status_notice);
            }
            Action::CompareWithLocal { target } => {
                self.start_compare_with_local(*target, None, None);
            }
            Action::CompareDirWithLocal {
                target,
                path_prefix,
            } => {
                self.start_compare_with_local(*target, None, Some(path_prefix.clone()));
            }
            Action::CompareFileWithLocal { target, path } => {
                self.start_compare_with_local(*target, Some(path.clone()), None);
            }
            Action::OpenConflictResolver(path) => {
                self.blur_active_pane(false);
                self.modal_stack.open_conflict_resolver(ConflictMergeState {
                    path: path.clone(),
                    doc: None,
                    selected_block: 0,
                    scroll: crate::app::ScrollViewport::new(),
                    h_scroll: 0,
                    show_base: false,
                    inline_editor: None,
                    externally_resolved: false,
                });
                effects.push(Effect::LoadConflictDocument(path.clone()));
            }
            Action::OpenConflictQueue => {
                self.open_conflict_queue_modal();
            }
            Action::CloseConflictQueue => {
                self.modal_stack.close_conflict_queue();
            }
            Action::OpenExternalMergetool(path) => {
                effects.push(Effect::OpenExternalMergetool(path.clone()));
            }
            Action::ConflictSelectBlock(delta) => {
                if let Some(cm) = self.modal_stack.conflict_merge_mut()
                    && let Some(doc) = &cm.doc
                    && doc.conflict_count > 0
                {
                    let next = (cm.selected_block as isize + delta)
                        .clamp(0, doc.conflict_count as isize - 1) as usize;
                    cm.selected_block = next;
                    cm.ensure_selected_block_visible();
                }
            }
            Action::ConflictSetBlock(idx) => {
                if let Some(cm) = self.modal_stack.conflict_merge_mut()
                    && let Some(doc) = &cm.doc
                    && *idx < doc.conflict_count
                {
                    cm.selected_block = *idx;
                    cm.ensure_selected_block_visible();
                }
            }
            Action::ConflictApplyChoice(choice) => {
                if let Some(cm) = self.modal_stack.conflict_merge_mut()
                    && let Some(doc) = &mut cm.doc
                {
                    if doc.is_delete_modify()
                        && matches!(
                            choice,
                            ConflictChoice::BothOursFirst
                                | ConflictChoice::BothTheirsFirst
                                | ConflictChoice::Neither
                                | ConflictChoice::Custom(_)
                        )
                    {
                        return true;
                    }
                    doc.set_choice(cm.selected_block, choice.clone());
                    if choice.is_resolved() && cm.selected_block + 1 < doc.conflict_count {
                        cm.selected_block += 1;
                        cm.ensure_selected_block_visible();
                    }
                }
            }
            Action::ConflictAcceptAll(choice) => {
                if let Some(cm) = self.modal_stack.conflict_merge_mut()
                    && let Some(doc) = &mut cm.doc
                {
                    doc.accept_all(choice.clone());
                }
            }
            Action::ConflictApplyNonConflicting => {
                let applied = if let Some(cm) = self.modal_stack.conflict_merge_mut()
                    && let Some(doc) = &mut cm.doc
                {
                    let count = doc.apply_non_conflicting();
                    if let Some(next_unres) = doc.first_unresolved_block() {
                        cm.selected_block = next_unres;
                        cm.ensure_selected_block_visible();
                    }
                    Some(count)
                } else {
                    None
                };
                if let Some(count) = applied {
                    let count_s = count.to_string();
                    self.status_notice = Some(if count > 0 {
                        self.i18n
                            .t_fmt("toast.conflict_applied_auto", &[("count", &count_s)])
                    } else {
                        self.i18n.t("toast.conflict_no_auto").to_string()
                    });
                }
            }
            Action::ConflictToggleBase => {
                if let Some(cm) = self.modal_stack.conflict_merge_mut() {
                    cm.show_base = !cm.show_base;
                }
            }
            Action::ConflictHorizontalScroll(delta) => {
                if let Some(cm) = self.modal_stack.conflict_merge_mut() {
                    if *delta >= 0 {
                        cm.h_scroll = cm.h_scroll.saturating_add(*delta as usize);
                    } else {
                        cm.h_scroll = cm.h_scroll.saturating_sub(delta.unsigned_abs());
                    }
                }
            }
            Action::ConflictStartInlineEdit => {
                if let Some(cm) = self.modal_stack.conflict_merge_mut()
                    && let Some(doc) = &cm.doc
                    && !doc.is_delete_modify()
                    && let Some(blk) = doc.block(cm.selected_block)
                {
                    let initial = blk
                        .resolved_lines()
                        .unwrap_or_else(|| blk.ours_lines.clone());
                    cm.inline_editor = Some(ConflictInlineEditor::open(cm.selected_block, initial));
                }
            }
            Action::ConflictInlineEditKey(key) => {
                let mut notice = None;
                if let Some(cm) = self.modal_stack.conflict_merge_mut()
                    && let Some(ed) = cm.inline_editor.as_mut()
                {
                    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
                    let alt = key.modifiers.contains(KeyModifiers::ALT);
                    let is_commit = matches!(key.code, KeyCode::F(2))
                        || (ctrl && matches!(key.code, KeyCode::Char('s') | KeyCode::Enter))
                        || (alt && key.code == KeyCode::Enter);
                    if is_commit {
                        let edited = cm.inline_editor.take().expect("checked is_some");
                        if let Some(doc) = &mut cm.doc {
                            doc.set_choice(
                                edited.block_idx,
                                ConflictChoice::Custom(edited.lines),
                            );
                            if edited.block_idx + 1 < doc.conflict_count {
                                cm.selected_block = edited.block_idx + 1;
                                cm.ensure_selected_block_visible();
                            }
                        }
                        notice = Some(self.i18n.t("toast.conflict_inline_applied").to_string());
                    } else if key.code == KeyCode::Esc {
                        cm.inline_editor = None;
                    } else {
                        match key.code {
                            KeyCode::Enter => ed.insert_newline(),
                            KeyCode::Backspace => ed.backspace(),
                            KeyCode::Delete => ed.delete_forward(),
                            KeyCode::Left => ed.move_left(),
                            KeyCode::Right => ed.move_right(),
                            KeyCode::Up => ed.move_up(),
                            KeyCode::Down => ed.move_down(),
                            KeyCode::Home => ed.move_home(),
                            KeyCode::End => ed.move_end(),
                            KeyCode::Tab => ed.insert_text("    "),
                            KeyCode::Char(c) if !ctrl && !alt => ed.insert_char(c),
                            _ => {}
                        }
                        let tab_size = self.config.jbgit.diff.tab_size;
                        ed.ensure_cursor_visible_with_tab(60, 5, tab_size);
                    }
                }
                if let Some(msg) = notice {
                    self.status_notice = Some(msg);
                }
            }
            Action::ConflictScroll(delta) => {
                self.scroll_pane(crate::app::ScrollPane::ConflictMerge, *delta);
            }
            Action::ConflictSaveAndStage => {
                if let Some(cm) = self.modal_stack.conflict_merge()
                    && let Some(doc) = &cm.doc
                {
                    let all_resolved = doc.unresolved_count() == 0;
                    let deletes = doc.deletes_file();
                    let path = doc.path.clone();
                    let unresolved = doc.unresolved_count();
                    if doc.side_presence == jbgit_core::ConflictSidePresence::BothDeleted
                        && !all_resolved
                    {
                        let count_s = unresolved.to_string();
                        self.status_notice = Some(self.i18n.t_fmt(
                            "toast.saved_unresolved_conflict",
                            &[("path", &path), ("count", &count_s)],
                        ));
                        return true;
                    }
                    let content = if deletes {
                        Vec::new()
                    } else {
                        doc.render_resolved().into_bytes()
                    };
                    effects.push(Effect::SaveConflictResolution {
                        path: path.clone(),
                        content,
                        stage: all_resolved,
                        delete: deletes,
                    });
                    self.status_notice = Some(if deletes {
                        self.i18n
                            .t_fmt("toast.deleted_staged_conflict", &[("path", &path)])
                    } else if all_resolved {
                        self.i18n
                            .t_fmt("toast.saved_staged_conflict", &[("path", &path)])
                    } else {
                        let count_s = unresolved.to_string();
                        self.i18n.t_fmt(
                            "toast.saved_unresolved_conflict",
                            &[("path", &path), ("count", &count_s)],
                        )
                    });
                    self.modal_stack.close_conflict_resolver();
                    if all_resolved || deletes {
                        self.on_conflict_file_resolved_from_merger(&path, effects);
                    }
                }
            }
            Action::ConflictQueueMove(delta) => {
                if let Some(cq) = self.modal_stack.conflict_queue_mut()
                    && !cq.entries.is_empty()
                {
                    let next = (cq.selected as isize + delta)
                        .clamp(0, cq.entries.len() as isize - 1) as usize;
                    cq.selected = next;
                    if let Some(vh) = cq.scroll.viewport_height() {
                        cq.scroll.ensure_visible(cq.selected, cq.entries.len(), vh);
                    }
                }
            }
            Action::ConflictQueueSelect(idx) => {
                if let Some(cq) = self.modal_stack.conflict_queue_mut()
                    && !cq.entries.is_empty()
                {
                    cq.selected = (*idx).min(cq.entries.len() - 1);
                    if let Some(vh) = cq.scroll.viewport_height() {
                        cq.scroll.ensure_visible(cq.selected, cq.entries.len(), vh);
                    }
                }
            }
            Action::ConflictQueueToggleSelect => {
                if let Some(cq) = self.modal_stack.conflict_queue_mut()
                    && let Some(path) = cq.selected_entry().map(|e| e.path.clone())
                {
                    if !cq.multi_selected.remove(&path) {
                        cq.multi_selected.insert(path);
                    }
                    if cq.selected + 1 < cq.entries.len() {
                        cq.selected += 1;
                    }
                }
            }
            Action::ConflictQueueToggleSelectAll => {
                if let Some(cq) = self.modal_stack.conflict_queue_mut() {
                    if cq.multi_selected.len() == cq.entries.len() {
                        cq.multi_selected.clear();
                    } else {
                        cq.multi_selected = cq.entries.iter().map(|e| e.path.clone()).collect();
                    }
                }
            }
            Action::ConflictQueueMergeSelected => {
                let target = self
                    .modal_stack
                    .conflict_queue_mut()
                    .and_then(|cq| {
                        let entry = cq.selected_entry()?.clone();
                        cq.active_merge_path = Some(entry.path.clone());
                        Some(entry)
                    });
                if let Some(entry) = target {
                    if entry.resolved {
                        self.status_notice = Some(self.i18n.t_fmt(
                            "toast.conflict_already_resolved_hint",
                            &[("path", &entry.path)],
                        ));
                    } else {
                        let _ = self
                            .dispatch_git_action(&Action::OpenConflictResolver(entry.path), effects);
                    }
                }
            }
            Action::ConflictQueueAcceptSide(ours) => {
                let paths: Vec<String> = self
                    .modal_stack
                    .conflict_queue()
                    .map(|cq| {
                        cq.target_entries()
                            .into_iter()
                            .filter(|e| !e.resolved)
                            .map(|e| e.path.clone())
                            .collect()
                    })
                    .unwrap_or_default();
                if paths.is_empty() {
                    self.status_notice =
                        Some(self.i18n.t("toast.conflict_queue_none_unresolved").into());
                } else {
                    self.start_git_op(GitOp::AcceptSide { paths, ours: *ours }, effects);
                }
            }
            Action::ConflictQueueRevertResolved => {
                let paths: Vec<String> = self
                    .modal_stack
                    .conflict_queue()
                    .map(|cq| {
                        cq.target_entries()
                            .into_iter()
                            .filter(|e| e.resolved)
                            .map(|e| e.path.clone())
                            .collect()
                    })
                    .unwrap_or_default();
                if paths.is_empty() {
                    self.status_notice =
                        Some(self.i18n.t("toast.conflict_queue_none_resolved").into());
                } else {
                    self.start_git_op(GitOp::UnresolveConflict { paths }, effects);
                }
            }
            Action::ConflictQueueMergetool => {
                if let Some(path) = self
                    .modal_stack
                    .conflict_queue()
                    .and_then(|cq| cq.selected_entry().map(|e| e.path.clone()))
                {
                    effects.push(Effect::OpenExternalMergetool(path));
                }
            }
            Action::PromptDeleteBranch { name } => self.prompt_delete_branch(name, effects),
            Action::PromptDeleteTag { name } => self.prompt_delete_tag(name, effects),
            Action::PromptPush { branch, up_to } => self.prompt_push(branch, *up_to, effects),
            Action::PromptMerge { rev } => self.prompt_merge(rev),
            Action::RememberMergeOptions(options) => {
                let body = options.to_remembered();
                self.remembered_merge_options = Some(MergeOptions::from_remembered(&body));
                if let Some(path) = self.merge_options_path() {
                    super::spawn_background_io("jbgit-opts-writer", move || {
                        let _ = std::fs::write(path, body);
                    });
                }
            }
            Action::PromptPull => self.prompt_pull(),
            Action::RememberPullOptions(options) => {
                let body = options.to_remembered();
                self.remembered_pull_options = Some(PullOptions::from_remembered(&body));
                if let Some(path) = self.pull_options_path() {
                    super::spawn_background_io("jbgit-opts-writer", move || {
                        let _ = std::fs::write(path, body);
                    });
                }
            }
            Action::PullDialogFetch => {
                let remote = if let Some(ModalLayer::Dialog(d)) = self.modal_stack.last_mut()
                    && let Dialog::Pull(p) = d.as_mut()
                {
                    p.fetching = true;
                    p.options.effective_remote().map(ToString::to_string)
                } else {
                    None
                };
                let op = match remote {
                    Some(name) => GitOp::FetchRemote { name },
                    None => GitOp::Fetch,
                };
                self.start_git_op(op, effects);
            }
            Action::PromptRebase { onto } => self.prompt_rebase(onto.clone()),
            Action::RememberRebaseOptions(options) => {
                let body = options.to_remembered();
                self.remembered_rebase_options = Some(RebaseOptions::from_remembered(&body));
                if let Some(path) = self.rebase_options_path() {
                    super::spawn_background_io("jbgit-opts-writer", move || {
                        let _ = std::fs::write(path, body);
                    });
                }
            }
            Action::OpenInteractiveRebaseFromDialog(options) => {
                let opened = self.open_interactive_rebase_onto(&options.onto);
                if opened {
                    self.sync_rebase_todo_preview(effects);
                } else {
                    self.start_git_op(GitOp::Rebase { options: options.clone() }, effects);
                }
            }
            Action::ConfirmCreatePatchFile {
                source,
                output,
                reverse,
                remember_dir,
            } => {
                let workdir = self.current_workdir();
                let in_worktree = !workdir.as_os_str().is_empty() && output.starts_with(&workdir);
                let exists = output.is_file();
                let run_actions = vec![
                    Action::RememberPatchDir(remember_dir.clone()),
                    Action::RunGitOp(GitOp::CreatePatchFile {
                        source: source.clone(),
                        output: output.clone(),
                        reverse: *reverse,
                    }),
                ];
                if in_worktree || exists {
                    let i18n = self.i18n.clone();
                    let shown = output.display().to_string();
                    let body_key = if in_worktree {
                        "dialog.create_patch.warn_worktree"
                    } else {
                        "dialog.create_patch.warn_overwrite"
                    };
                    self.push_dialog(Dialog::Choice(ChoiceDialog::buttons(
                        i18n.t("dialog.create_patch.title"),
                        vec![i18n.t_fmt(body_key, &[("path", &shown)])],
                        vec![
                            ChoiceOption::new(
                                i18n.t("dialog.create_patch.confirm_write"),
                                Some('w'),
                                run_actions,
                            ),
                            ChoiceOption::new(i18n.t("common.cancel"), Some('c'), Vec::new()),
                        ],
                    )));
                } else {
                    self.run_actions(run_actions, effects);
                }
            }
            Action::PromptUpdateProject => self.prompt_update_project(),
            Action::PromptUnshallow => self.prompt_unshallow(),
            Action::OpenRemotes => {
                self.modal_stack
                    .retain(|l| !matches!(l, ModalLayer::ContextMenu { .. } | ModalLayer::Remotes(_)));
                self.blur_active_pane(false);
                let rs = crate::remotes_view::RemotesState::for_snapshot(self.snapshot());
                self.push_modal(ModalLayer::Remotes(rs));
            }
            Action::RemotesMove(delta) => crate::remotes_view::apply_selection(self, None, *delta),
            Action::RemotesSelect(idx) => {
                crate::remotes_view::apply_selection(self, Some(*idx), 0);
            }
            Action::UncommitHead => {
                if let Some(warning) =
                    crate::stage_view::undo_head_warning_message(self.snapshot(), &self.i18n)
                {
                    let body: Vec<String> = warning.lines().map(ToString::to_string).collect();
                    let i18n = self.i18n.clone();
                    self.push_dialog(Dialog::Choice(ChoiceDialog::confirm(
                        i18n.t("dialog.undo_commit.title"),
                        body,
                        i18n.t("dialog.undo_commit.confirm_btn"),
                        i18n.cancel(),
                        vec![
                            Action::SwitchTab(WorkspaceTab::Commit),
                            Action::FocusPane(PaneId::CommitMessageEditor),
                            Action::UndoLastCommitConfirmed,
                        ],
                    )));
                } else {
                    self.set_focus(WorkspaceTab::Commit, PaneId::CommitMessageEditor);
                    self.status_notice = Some(self.i18n.t("toast.uncommitting_head").into());
                    effects.push(Effect::UndoLastCommit);
                }
            }
            Action::SaveGitIdentityAndCommit {
                name,
                email,
                global,
                pending_commit,
            } => {
                self.pending_commit_after_identity = pending_commit.clone();
                effects.extend(self.dispatch(Action::RunGitOp(GitOp::SetIdentity {
                    name: name.clone(),
                    email: email.clone(),
                    global: *global,
                })));
            }
            Action::OngoingResolve => {
                self.set_focus(WorkspaceTab::Commit, PaneId::CommitChangesTree);
                let conflicted_paths: Vec<String> = self
                    .stage_workbench
                    .status()
                    .map(|s| s.conflicted.iter().map(|e| e.path.clone()).collect())
                    .unwrap_or_default();
                if conflicted_paths.len() >= 2 {
                    self.open_conflict_queue_modal();
                } else if let Some(path) = conflicted_paths.into_iter().next() {
                    if let Some(pos) = self.stage_workbench.rows().iter().position(|r| {
                        matches!(
                            &r.kind,
                            StageTreeRowKind::File {
                                group_kind: StageNodeKind::Conflicted,
                                path: p,
                                ..
                            } if p == &path
                        )
                    }) {
                        self.stage_workbench.select_row(pos);
                    }
                    let _ = self.dispatch_git_action(&Action::OpenConflictResolver(path), effects);
                } else {
                    self.status_notice = Some(self.i18n.t("toast.no_conflicts_banner").into());
                }
            }
            Action::NotificationActivate => {
                if let Some(n) = self.notification.take()
                    && let Some((_, actions)) = n.action
                {
                    self.run_actions(actions, effects);
                }
            }
            Action::NotificationDismiss => self.notification = None,
            _ => return false,
        }
        true
    }

    fn conflict_queue_side_labels(&self) -> (bool, String, String) {
        let snap = self.snapshot();
        let is_reversed = snap.is_some_and(|s| is_reverse_conflict_state(&s.state));
        if is_reversed {
            let yours = snap
                .and_then(|s| match &s.state {
                    RepositoryState::Rebase { branch, .. }
                    | RepositoryState::RebaseInteractive { branch, .. }
                    | RepositoryState::RebaseMerge { branch, .. } => branch.clone(),
                    _ => None,
                })
                .unwrap_or_else(|| "replayed".to_string());
            let theirs = snap
                .and_then(|s| s.ongoing_onto.clone())
                .unwrap_or_else(|| "onto".to_string());
            (true, yours, theirs)
        } else {
            let yours = snap
                .and_then(|s| s.head_branch.clone())
                .unwrap_or_else(|| "HEAD".to_string());
            (false, yours, "incoming".to_string())
        }
    }

    pub(crate) fn open_conflict_queue_modal(&mut self) {
        let Some(status) = self.stage_workbench.status() else {
            self.status_notice = Some(self.i18n.t("toast.no_conflicts_banner").into());
            return;
        };
        if status.conflicted.is_empty() {
            self.status_notice = Some(self.i18n.t("toast.no_conflicts_banner").into());
            return;
        }
        let (is_reversed, yours_label, theirs_label) = self.conflict_queue_side_labels();
        let entries: Vec<ConflictQueueEntry> = status
            .conflicted
            .iter()
            .map(|e| {
                let raw_label = status.conflict_labels.get(&e.path).map(String::as_str);
                let (yours_status, theirs_status) = conflict_side_statuses(raw_label, is_reversed);
                ConflictQueueEntry {
                    path: e.path.clone(),
                    yours_status,
                    theirs_status,
                    resolved: false,
                }
            })
            .collect();
        self.blur_active_pane(false);
        self.modal_stack.open_conflict_queue(ConflictQueueState {
            entries,
            selected: 0,
            multi_selected: BTreeSet::new(),
            scroll: crate::app::ScrollViewport::new(),
            is_reversed,
            yours_label,
            theirs_label,
            active_merge_path: None,
            completion_prompted: false,
        });
    }

    fn on_conflict_file_resolved_from_merger(&mut self, path: &str, effects: &mut Vec<Effect>) {
        let next_step = if let Some(cq) = self.modal_stack.conflict_queue_mut() {
            let mut resolved_idx = cq.selected;
            if let Some((idx, entry)) = cq
                .entries
                .iter_mut()
                .enumerate()
                .find(|(_, e)| e.path == path)
            {
                entry.resolved = true;
                resolved_idx = idx;
            }
            cq.multi_selected.remove(path);
            cq.active_merge_path = None;
            if let Some(next_idx) = cq.next_unresolved_after(resolved_idx) {
                cq.selected = next_idx;
                let next_path = cq.entries[next_idx].path.clone();
                cq.active_merge_path = Some(next_path.clone());
                Some(Ok(next_path))
            } else if cq.all_resolved() && !cq.completion_prompted {
                cq.completion_prompted = true;
                Some(Err(()))
            } else {
                None
            }
        } else {
            let remaining = self
                .stage_workbench
                .status()
                .map_or(0, |s| s.conflicted.iter().filter(|c| c.path != path).count());
            if remaining == 0 && self.ongoing_kind().is_some() {
                Some(Err(()))
            } else {
                None
            }
        };
        match next_step {
            Some(Ok(next_path)) => {
                let _ = self.dispatch_git_action(&Action::OpenConflictResolver(next_path), effects);
            }
            Some(Err(())) => {
                self.open_all_conflicts_resolved_dialog();
            }
            None => {}
        }
    }

    pub(crate) fn open_all_conflicts_resolved_dialog(&mut self) {
        let i18n = self.i18n.clone();
        if let Some(kind) = self.ongoing_kind() {
            let kind_title = i18n.ongoing_title(kind);
            self.push_dialog(Dialog::Choice(ChoiceDialog::buttons(
                i18n.t("dialog.all_conflicts_resolved.title"),
                vec![i18n.t_fmt(
                    "dialog.all_conflicts_resolved.body_ongoing",
                    &[("kind", kind_title)],
                )],
                vec![
                    ChoiceOption::new(
                        i18n.t_fmt(
                            "dialog.all_conflicts_resolved.continue_btn",
                            &[("kind", kind_title)],
                        ),
                        Some('c'),
                        vec![
                            Action::CloseConflictQueue,
                            Action::RunGitOp(GitOp::Ongoing {
                                kind,
                                step: OngoingStep::Continue,
                            }),
                        ],
                    ),
                    ChoiceOption::new(
                        i18n.t("dialog.all_conflicts_resolved.commit_panel_btn"),
                        Some('o'),
                        vec![
                            Action::CloseConflictQueue,
                            Action::SwitchTab(WorkspaceTab::Commit),
                            Action::FocusPane(PaneId::CommitMessageEditor),
                        ],
                    ),
                    ChoiceOption::new(
                        i18n.t("dialog.all_conflicts_resolved.stay_btn"),
                        Some('s'),
                        Vec::new(),
                    ),
                ],
            )));
        } else {
            self.push_dialog(Dialog::Choice(ChoiceDialog::buttons(
                i18n.t("dialog.all_conflicts_resolved.title"),
                vec![i18n.t("dialog.all_conflicts_resolved.body_commit").to_string()],
                vec![
                    ChoiceOption::new(
                        i18n.t("dialog.all_conflicts_resolved.commit_panel_btn"),
                        Some('o'),
                        vec![
                            Action::CloseConflictQueue,
                            Action::SwitchTab(WorkspaceTab::Commit),
                            Action::FocusPane(PaneId::CommitMessageEditor),
                        ],
                    ),
                    ChoiceOption::new(
                        i18n.t("dialog.all_conflicts_resolved.stay_btn"),
                        Some('s'),
                        Vec::new(),
                    ),
                ],
            )));
        }
    }

    pub(crate) fn open_special_conflict_dialog(&mut self, info: SpecialConflictInfo) {
        let i18n = self.i18n.clone();
        let (title_key, body_key) = match info.kind {
            SpecialConflictKind::Binary => (
                "dialog.special_conflict.title_binary",
                "dialog.special_conflict.body_binary",
            ),
            SpecialConflictKind::Submodule => (
                "dialog.special_conflict.title_submodule",
                "dialog.special_conflict.body_submodule",
            ),
            SpecialConflictKind::Directory => (
                "dialog.special_conflict.title_directory",
                "dialog.special_conflict.body_directory",
            ),
        };
        let format_side = |side: Option<&jbgit_core::ConflictSideInfo>| -> String {
            match side {
                Some(s) => {
                    let short = s.oid.short();
                    if info.kind == SpecialConflictKind::Submodule {
                        format!("commit {short}")
                    } else if let Some(sz) = s.size {
                        format!("{short} ({})", format_byte_size(sz))
                    } else {
                        short
                    }
                }
                None => i18n.t("dialog.special_conflict.side_deleted").to_string(),
            }
        };
        let ours_meta = format_side(info.ours.as_ref());
        let theirs_meta = format_side(info.theirs.as_ref());
        let (yours_line, theirs_line) = if info.is_reversed {
            (
                i18n.t_fmt(
                    "dialog.special_conflict.yours_rebase",
                    &[("label", &info.ours_label), ("meta", &ours_meta)],
                ),
                i18n.t_fmt(
                    "dialog.special_conflict.theirs_rebase",
                    &[("label", &info.theirs_label), ("meta", &theirs_meta)],
                ),
            )
        } else {
            (
                i18n.t_fmt(
                    "dialog.special_conflict.yours",
                    &[("label", &info.ours_label), ("meta", &ours_meta)],
                ),
                i18n.t_fmt(
                    "dialog.special_conflict.theirs",
                    &[("label", &info.theirs_label), ("meta", &theirs_meta)],
                ),
            )
        };
        let mut ours_opt = ChoiceOption::new(
            i18n.t("dialog.special_conflict.accept_yours"),
            Some('y'),
            vec![Action::RunGitOp(GitOp::AcceptSide {
                paths: vec![info.path.clone()],
                ours: true,
            })],
        )
        .detail(if info.ours.is_some() {
            ours_meta
        } else {
            i18n.t("dialog.special_conflict.delete_detail").to_string()
        });
        if info.ours.is_none() {
            ours_opt = ours_opt.danger();
        }
        let mut theirs_opt = ChoiceOption::new(
            i18n.t("dialog.special_conflict.accept_theirs"),
            Some('t'),
            vec![Action::RunGitOp(GitOp::AcceptSide {
                paths: vec![info.path.clone()],
                ours: false,
            })],
        )
        .detail(if info.theirs.is_some() {
            theirs_meta
        } else {
            i18n.t("dialog.special_conflict.delete_detail").to_string()
        });
        if info.theirs.is_none() {
            theirs_opt = theirs_opt.danger();
        }
        let options = vec![
            ours_opt,
            theirs_opt,
            ChoiceOption::new(
                i18n.t("dialog.special_conflict.open_mergetool"),
                Some('m'),
                vec![Action::OpenExternalMergetool(info.path.clone())],
            ),
            ChoiceOption::new(i18n.cancel(), Some('c'), Vec::new()),
        ];
        self.push_dialog(Dialog::Choice(ChoiceDialog::buttons(
            i18n.t_fmt(title_key, &[("path", &info.path)]),
            vec![i18n.t(body_key).to_string(), yours_line, theirs_line],
            options,
        )));
    }

    fn start_compare_with_local(
        &mut self,
        target: Oid,
        select_path: Option<String>,
        path_prefix: Option<String>,
    ) {
        self.set_focus(WorkspaceTab::Log, PaneId::LogChangesTree);
        self.log.start_compare_with_local(
            target,
            select_path,
            path_prefix,
            &self.i18n,
            &mut self.status_notice,
        );
    }

    fn checkout_remote(&mut self, remote_ref: &str, effects: &mut Vec<Effect>) {
        let Some(snap) = self.repo_snapshot.clone() else {
            return;
        };
        let Some(remote) = snap
            .refs
            .iter()
            .find(|r| r.kind == RefKind::RemoteBranch && r.name == remote_ref)
        else {
            self.status_notice = Some(
                self.i18n
                    .t_fmt("toast.remote_not_found", &[("name", remote_ref)]),
            );
            return;
        };
        let local_name = snap
            .split_tracking(remote_ref)
            .map(|(_, b)| b)
            .unwrap_or_else(|| remote_ref.split_once('/').map_or(remote_ref, |(_, b)| b))
            .to_string();
        let op = match snap.local_branch(&local_name) {
            None => GitOp::CheckoutNewBranch {
                name: local_name,
                start: remote_ref.to_string(),
                track: true,
                force: false,
            },
            Some(l) if l.target_oid == remote.target_oid => GitOp::Checkout {
                target: local_name,
                detach: false,
            },
            Some(l) => {
                let ahead = snap.count_exclusive(l.target_oid, remote.target_oid);
                if ahead == 0 {
                    // Local is behind: fast-forward it to the remote tip.
                    GitOp::CheckoutResetBranch {
                        name: local_name,
                        start: remote_ref.to_string(),
                    }
                } else {
                    let existing: Vec<String> = snap
                        .local_branch_names()
                        .iter()
                        .map(|s| s.to_string())
                        .collect();
                    let suggestion = (1..)
                        .map(|i| format!("{local_name}-{i}"))
                        .find(|n| !existing.contains(n))
                        .unwrap_or_default();
                    let behind = snap.count_exclusive(remote.target_oid, l.target_oid);
                    let ahead_s = ahead.to_string();
                    let behind_s = behind.to_string();
                    let i18n = self.i18n.clone();
                    self.push_dialog(Dialog::Choice(ChoiceDialog::buttons(
                        i18n.t_fmt("dialog.diverged_checkout.title", &[("remote_ref", remote_ref)]),
                        vec![
                            i18n.t_fmt("dialog.diverged_checkout.prompt", &[("local", &local_name)]),
                            i18n.t_fmt(
                                "dialog.diverged_checkout.counts",
                                &[("ahead", &ahead_s), ("remote_ref", remote_ref), ("behind", &behind_s)],
                            ),
                        ],
                        vec![
                            ChoiceOption::new(
                                i18n.t_fmt("dialog.diverged_checkout.checkout_local", &[("local", &local_name)]),
                                Some('l'),
                                vec![Action::RunGitOp(GitOp::Checkout {
                                    target: local_name.clone(),
                                    detach: false,
                                })],
                            ),
                            ChoiceOption::new(
                                i18n.t_fmt(
                                    "dialog.diverged_checkout.reset_to_remote",
                                    &[("local", &local_name), ("remote_ref", remote_ref)],
                                ),
                                Some('r'),
                                vec![Action::RunGitOp(GitOp::CheckoutResetBranch {
                                    name: local_name.clone(),
                                    start: remote_ref.to_string(),
                                })],
                            )
                            .detail(i18n.t_fmt("dialog.diverged_checkout.drops_commits", &[("ahead", &ahead_s)]))
                            .danger(),
                            ChoiceOption::new(
                                i18n.t("dialog.diverged_checkout.new_branch"),
                                Some('n'),
                                vec![Action::OpenDialog(Box::new(Dialog::Input(
                                    InputDialog::new(
                                        i18n.t_fmt("dialog.diverged_checkout.new_branch_title", &[("remote_ref", remote_ref)]),
                                        i18n.t("dialog.diverged_checkout.new_branch_prompt"),
                                        suggestion,
                                        InputValidation::RefName {
                                            what: "Branch",
                                            existing,
                                        },
                                        InputIntent::RemoteCheckoutAs {
                                            start: remote_ref.to_string(),
                                        },
                                    ),
                                )))],
                            ),
                            ChoiceOption::new(i18n.cancel(), Some('c'), Vec::new()),
                        ],
                    )));
                    return;
                }
            }
        };
        self.start_git_op(op, effects);
    }

    /// `GitCheckoutAndRebaseRemoteBranchWorkflow`: creates/resets the matching
    /// local branch from `remote_ref` and rebases it onto the current branch.
    fn checkout_and_rebase_remote(&mut self, remote_ref: &str, effects: &mut Vec<Effect>) {
        let Some(snap) = self.repo_snapshot.clone() else {
            return;
        };
        let Some(current) = snap.head_branch.clone() else {
            self.notify_error(self.i18n.t("menu.branch.choose_other_branch").to_string());
            return;
        };
        let Some(remote) = snap
            .refs
            .iter()
            .find(|r| r.kind == RefKind::RemoteBranch && r.name == remote_ref)
        else {
            self.status_notice = Some(
                self.i18n
                    .t_fmt("toast.remote_not_found", &[("name", remote_ref)]),
            );
            return;
        };
        let local_name = snap
            .split_tracking(remote_ref)
            .map(|(_, short)| short.to_string())
            .unwrap_or_else(|| {
                remote_ref
                    .split_once('/')
                    .map_or(remote_ref, |(_, short)| short)
                    .to_string()
            });
        if current == local_name {
            self.notify_error(self.i18n.t_fmt(
                "toast.checkout_rebase_same_current",
                &[("name", &local_name)],
            ));
            return;
        }
        let create_and_rebase = |force: bool| {
            GitOp::Sequence(vec![
                GitOp::CreateBranch {
                    name: local_name.clone(),
                    start: remote_ref.to_string(),
                    force,
                    track: true,
                },
                GitOp::Rebase {
                    options: RebaseOptions::onto(current.clone(), Some(local_name.clone())),
                },
            ])
        };
        match snap.local_branch(&local_name) {
            None => self.start_git_op(create_and_rebase(false), effects),
            Some(l) if l.target_oid == remote.target_oid => {
                self.start_git_op(
                    GitOp::Rebase {
                        options: RebaseOptions::onto(current, Some(local_name)),
                    },
                    effects,
                );
            }
            Some(l) => {
                let ahead = snap.count_exclusive(l.target_oid, remote.target_oid);
                if ahead == 0 {
                    self.start_git_op(create_and_rebase(true), effects);
                } else {
                    let ahead_s = ahead.to_string();
                    let i18n = self.i18n.clone();
                    self.push_dialog(Dialog::Choice(ChoiceDialog::buttons(
                        i18n.t_fmt(
                            "dialog.checkout_rebase_remote.title",
                            &[("remote_ref", remote_ref)],
                        ),
                        vec![i18n.t_fmt(
                            "dialog.checkout_rebase_remote.body",
                            &[
                                ("local", &local_name),
                                ("ahead", &ahead_s),
                                ("remote_ref", remote_ref),
                                ("cur", &current),
                            ],
                        )],
                        vec![
                            ChoiceOption::new(
                                i18n.t_fmt(
                                    "dialog.checkout_rebase_remote.keep_local_btn",
                                    &[("local", &local_name)],
                                ),
                                Some('k'),
                                vec![Action::RunGitOp(GitOp::Rebase {
                                    options: RebaseOptions::onto(
                                        current.clone(),
                                        Some(local_name.clone()),
                                    ),
                                })],
                            ),
                            ChoiceOption::new(
                                i18n.t_fmt(
                                    "dialog.checkout_rebase_remote.reset_btn",
                                    &[("remote_ref", remote_ref)],
                                ),
                                Some('r'),
                                vec![Action::RunGitOp(create_and_rebase(true))],
                            )
                            .detail(i18n.t_fmt(
                                "dialog.diverged_checkout.drops_commits",
                                &[("ahead", &ahead_s)],
                            ))
                            .danger(),
                            ChoiceOption::new(i18n.cancel(), Some('c'), Vec::new()),
                        ],
                    )));
                }
            }
        }
    }

    fn compare_branches(&mut self, left: &str, right: &str) {
        self.set_focus(WorkspaceTab::Log, PaneId::LogCommitTable);
        let _ = self.log.compare_branches(
            left,
            right,
            self.repo_snapshot.as_ref(),
            &self.branch_colors,
            self.async_graph_enabled,
            &self.i18n,
            &mut self.status_notice,
        );
    }

    fn prompt_compare_with_branch(&mut self, left: &str) {
        let Some(snap) = self.repo_snapshot.clone() else {
            return;
        };
        let mut candidates: Vec<String> = snap
            .refs
            .iter()
            .filter(|r| {
                matches!(
                    r.kind,
                    RefKind::LocalBranch | RefKind::RemoteBranch | RefKind::Tag
                ) && r.name != left
            })
            .map(|r| r.name.clone())
            .collect();
        candidates.sort();
        candidates.dedup();
        if candidates.is_empty() {
            self.status_notice = Some(self.i18n.t("toast.no_other_branches_to_compare").into());
            return;
        }
        let i18n = self.i18n.clone();
        let mut opts: Vec<ChoiceOption> = candidates
            .into_iter()
            .take(24)
            .map(|right| {
                ChoiceOption::new(
                    right.clone(),
                    None,
                    vec![Action::CompareBranches {
                        left: left.to_string(),
                        right,
                    }],
                )
            })
            .collect();
        opts.push(ChoiceOption::new(i18n.cancel(), Some('c'), Vec::new()));
        self.push_dialog(Dialog::Choice(ChoiceDialog::buttons(
            i18n.t_fmt("dialog.compare_with.title", &[("branch", left)]),
            vec![i18n.t_fmt("dialog.compare_with.prompt", &[("branch", left)])],
            opts,
        )));
    }

    fn prompt_set_upstream(&mut self, branch: &str) {
        let Some(snap) = self.repo_snapshot.clone() else {
            return;
        };
        let mut remotes: Vec<String> = snap
            .refs
            .iter()
            .filter(|r| r.kind == RefKind::RemoteBranch)
            .map(|r| r.name.clone())
            .collect();
        if remotes.is_empty() {
            self.status_notice = Some(self.i18n.t("menu.branch.no_remote_branches").into());
            return;
        }
        let preferred = format!("{}/{branch}", self.default_remote());
        remotes.sort_by(|a, b| {
            (b == &preferred)
                .cmp(&(a == &preferred))
                .then_with(|| jbgit_core::natural_cmp(a, b))
        });
        let i18n = self.i18n.clone();
        let mut opts: Vec<ChoiceOption> = remotes
            .into_iter()
            .take(24)
            .map(|upstream| {
                ChoiceOption::new(
                    upstream.clone(),
                    None,
                    vec![Action::RunGitOp(GitOp::SetUpstream {
                        branch: branch.to_string(),
                        upstream,
                    })],
                )
            })
            .collect();
        opts.push(ChoiceOption::new(i18n.cancel(), Some('c'), Vec::new()));
        self.push_dialog(Dialog::Choice(ChoiceDialog::buttons(
            i18n.t_fmt("dialog.set_upstream.title", &[("branch", branch)]),
            vec![i18n.t_fmt("dialog.set_upstream.prompt", &[("branch", branch)])],
            opts,
        )));
    }

    fn open_branch_cleanup(&mut self) {
        let Some(snap) = self.repo_snapshot.clone() else {
            return;
        };
        self.modal_stack
            .retain(|l| !matches!(l, ModalLayer::ContextMenu { .. } | ModalLayer::BranchCleanup(_)));
        self.blur_active_pane(false);
        let bc = crate::branch_cleanup::BranchCleanupState::for_snapshot(
            &snap,
            &self.config.jbgit.push.protected_branches,
        );
        self.push_modal(ModalLayer::BranchCleanup(Box::new(bc)));
    }

    fn compare_with_current(&mut self, branch: &str) {
        self.set_focus(WorkspaceTab::Log, PaneId::LogCommitTable);
        let _ = self.log.compare_with_current(
            branch,
            self.repo_snapshot.as_ref(),
            &self.branch_colors,
            self.async_graph_enabled,
            &self.i18n,
            &mut self.status_notice,
        );
    }

    fn prompt_delete_branch(&mut self, name: &str, effects: &mut Vec<Effect>) {
        let Some(snap) = self.repo_snapshot.clone() else {
            return;
        };
        let Some(local) = snap.local_branch(name) else {
            self.status_notice = Some(self.i18n.t_fmt("toast.branch_not_found", &[("name", name)]));
            return;
        };
        if local.is_head {
            self.status_notice = Some(
                self.i18n
                    .t_fmt("toast.cannot_delete_current", &[("name", name)]),
            );
            return;
        }
        let tip = local.target_oid;
        let unmerged_head = snap.head_oid.map(|h| snap.count_exclusive(tip, h));
        let upstream_oid = local
            .upstream
            .as_ref()
            .and_then(|u| {
                snap.refs
                    .iter()
                    .find(|r| r.kind == RefKind::RemoteBranch && &r.name == u)
            })
            .map(|r| r.target_oid);
        let unmerged_up = upstream_oid.map(|u| snap.count_exclusive(tip, u));
        let merged = unmerged_head == Some(0) || unmerged_up == Some(0);
        if merged {
            self.start_git_op(
                GitOp::DeleteBranch {
                    name: name.to_string(),
                    force: false,
                    tip,
                },
                effects,
            );
            return;
        }
        let count = unmerged_head.unwrap_or(0).max(unmerged_up.unwrap_or(0));
        let count_s = count.to_string();
        let prompt_key = if local.upstream.is_some() {
            "dialog.confirm_delete.prompt_unmerged_upstream"
        } else {
            "dialog.confirm_delete.prompt_unmerged_head"
        };
        let exclude_oid = if unmerged_up.unwrap_or(0) > unmerged_head.unwrap_or(0) {
            upstream_oid.or(snap.head_oid)
        } else {
            snap.head_oid.or(upstream_oid)
        };
        let i18n = self.i18n.clone();
        let mut body = vec![i18n.t_fmt(prompt_key, &[("name", name), ("count", &count_s)])];
        if let Some(ex) = exclude_oid {
            const PREVIEW_LIMIT: usize = 8;
            let commits = snap.exclusive_commits(tip, ex, PREVIEW_LIMIT);
            for c in &commits {
                body.push(format!("  • {} {}", c.oid.short(), c.subject));
            }
            if count > commits.len() && !commits.is_empty() {
                let more = (count - commits.len()).to_string();
                body.push(i18n.t_fmt("dialog.confirm_delete.and_more", &[("count", &more)]));
            }
        }
        body.push(i18n.t("dialog.confirm_delete.warn_unmerged").into());
        self.push_dialog(Dialog::Choice(ChoiceDialog::confirm(
            i18n.t("dialog.confirm_delete.title_unmerged"),
            body,
            i18n.t("dialog.confirm_delete.force_delete_btn"),
            i18n.cancel(),
            vec![Action::RunGitOp(GitOp::DeleteBranch {
                name: name.to_string(),
                force: true,
                tip,
            })],
        )));
    }

    pub(crate) fn available_remotes(&self) -> Vec<String> {
        self.snapshot()
            .map(RepoSnapshot::available_remotes)
            .unwrap_or_default()
    }

    /// Current upstream's remote → `origin` → first configured remote.
    pub(crate) fn default_remote(&self) -> String {
        self.snapshot()
            .and_then(RepoSnapshot::default_remote)
            .unwrap_or_else(|| "origin".to_string())
    }

    fn prompt_delete_tag(&mut self, name: &str, effects: &mut Vec<Effect>) {
        let Some(snap) = self.repo_snapshot.clone() else {
            return;
        };
        let Some(tag_ref) = snap
            .refs
            .iter()
            .find(|r| r.kind == RefKind::Tag && r.name == name)
        else {
            self.status_notice = Some(self.i18n.t_fmt("toast.tag_not_found", &[("name", name)]));
            return;
        };
        let tip = tag_ref.target_oid;
        let i18n = self.i18n.clone();
        let short = tip.short();
        let mut dlg = ChoiceDialog::confirm(
            i18n.t("dialog.delete_tag.title"),
            vec![i18n.t_fmt(
                "dialog.delete_tag.body",
                &[("name", name), ("short", &short)],
            )],
            i18n.t("dialog.delete_tag.confirm_btn"),
            i18n.cancel(),
            vec![Action::RunGitOp(GitOp::DeleteTag {
                name: name.to_string(),
                tip,
            })],
        );
        if let Some(remote) = self.available_remotes().into_iter().next() {
            dlg.remote_tag_probe = Some(DeleteTagRemoteProbe {
                name: name.to_string(),
                tip,
                remote: remote.clone(),
            });
            effects.push(Effect::CheckRemoteTag {
                remote,
                tag: name.to_string(),
            });
        }
        self.push_dialog(Dialog::Choice(dlg));
    }

    pub(crate) fn prompt_push(&mut self, branch: &str, up_to: Option<Oid>, effects: &mut Vec<Effect>) {
        let Some(snap) = self.repo_snapshot.clone() else {
            return;
        };
        let is_detached = branch == "HEAD";
        let local = (!is_detached).then(|| snap.local_branch(branch)).flatten();
        let upstream = local.and_then(|l| l.upstream.clone());
        let remotes = snap.available_remotes();
        let split_up = upstream.as_deref().and_then(|u| snap.split_tracking(u));
        let preferred_remote = split_up
            .map(|(r, _)| r.to_string())
            .or_else(|| snap.default_remote());
        let remote_idx = preferred_remote
            .as_deref()
            .and_then(|p| remotes.iter().position(|r| r == p))
            .unwrap_or(0);
        let selected_remote = remotes.get(remote_idx).map(String::as_str).unwrap_or("");
        let refspec_target = (!is_detached)
            .then(|| snap.remote(selected_remote))
            .flatten()
            .and_then(|r| jbgit_core::resolve_push_refspec_target(branch, &r.push_refspecs));
        let remote_branch = if is_detached {
            String::new()
        } else {
            refspec_target
                .or_else(|| split_up.map(|(_, rb)| rb.to_string()))
                .unwrap_or_else(|| branch.to_string())
        };
        let set_upstream = !is_detached && upstream.is_none();
        let expected_remote_oid = (!selected_remote.is_empty() && !remote_branch.is_empty())
            .then(|| snap.remote_branch(selected_remote, &remote_branch))
            .flatten()
            .map(|r| r.target_oid);
        let mut pd = PushDialogState::new(
            branch.to_string(),
            remotes,
            remote_idx,
            remote_branch,
            set_upstream,
            up_to,
            jbgit_core::DiffContext::from_lines(self.config.jbgit.diff.context_lines),
        );
        pd.diff.whitespace = self.config.jbgit.diff.whitespace.to_core();
        pd.diff.soft_wrap = self.config.jbgit.diff.soft_wrap;
        pd.expected_remote_oid = expected_remote_oid;
        if is_detached && pd.remote_branch.is_empty() {
            pd.focus = crate::log_view::PushDialogFocus::Target;
        }
        let has_remote = !pd.remote().is_empty();
        if !has_remote {
            pd.loading = false;
        }
        let key = pd.outgoing_key();
        self.blur_active_pane(false);
        self.modal_stack.open_push_dialog(pd);
        if has_remote {
            effects.push(Effect::RequestOutgoing(key));
        }
    }

    /// Refreshes an open PushDialog's remote list after adding/renaming a remote
    /// (e.g. via inline `[+ Define Remote…]`).
    pub(crate) fn sync_open_push_dialog_remotes(&mut self, effects: &mut Vec<Effect>) {
        let Some(snap) = self.repo_snapshot.clone() else {
            return;
        };
        let avail = snap.available_remotes();
        let mut reload_key = None;
        for layer in self.modal_stack.iter_mut() {
            if let ModalLayer::PushDialog(pd) = layer {
                let was_empty = pd.remotes.is_empty();
                let prev_remote = pd.remote().to_string();
                if pd.remotes != avail {
                    pd.remotes = avail.clone();
                    pd.remote_idx = avail
                        .iter()
                        .position(|r| r == &prev_remote)
                        .or_else(|| {
                            snap.default_remote()
                                .and_then(|d| avail.iter().position(|r| r == &d))
                        })
                        .unwrap_or(0);
                    if was_empty && !pd.remote().is_empty() {
                        if !pd.is_detached()
                            && let Some(mapped) = snap.remote(pd.remote()).and_then(|r| {
                                jbgit_core::resolve_push_refspec_target(
                                    &pd.branch,
                                    &r.push_refspecs,
                                )
                            })
                        {
                            pd.remote_branch = mapped;
                        }
                        pd.expected_remote_oid = snap
                            .remote_branch(pd.remote(), &pd.remote_branch)
                            .map(|r| r.target_oid);
                        pd.loading = true;
                        pd.outgoing = None;
                        reload_key = Some(pd.outgoing_key());
                    }
                }
            }
        }
        if let Some(key) = reload_key {
            effects.push(Effect::RequestOutgoing(key));
        }
    }

    pub(crate) fn preload_remembered_options_from_dir(&mut self, common_dir: &std::path::Path) {
        if common_dir.as_os_str().is_empty() {
            return;
        }
        self.remembered_merge_options = Some(
            std::fs::read_to_string(common_dir.join("jbgit-merge-options"))
                .ok()
                .map(|t| MergeOptions::from_remembered(&t))
                .unwrap_or_default(),
        );
        self.remembered_pull_options = Some(
            std::fs::read_to_string(common_dir.join("jbgit-pull-options"))
                .ok()
                .map(|t| PullOptions::from_remembered(&t))
                .unwrap_or_default(),
        );
        self.remembered_rebase_options = Some(
            std::fs::read_to_string(common_dir.join("jbgit-rebase-options"))
                .ok()
                .map(|t| RebaseOptions::from_remembered(&t))
                .unwrap_or_default(),
        );
    }

    fn merge_options_path(&self) -> Option<std::path::PathBuf> {
        let snap = self.snapshot()?;
        (!snap.common_dir.as_os_str().is_empty())
            .then(|| snap.common_dir.join("jbgit-merge-options"))
    }

    fn load_merge_options(&self) -> MergeOptions {
        if let Some(opts) = &self.remembered_merge_options {
            return opts.clone();
        }
        #[cfg(test)]
        {
            self.merge_options_path()
                .and_then(|p| std::fs::read_to_string(p).ok())
                .map(|t| MergeOptions::from_remembered(&t))
                .unwrap_or_default()
        }
        #[cfg(not(test))]
        {
            MergeOptions::default()
        }
    }

    fn prompt_merge(&mut self, rev: &str) {
        if let Err(msg) = self.require_no_ongoing() {
            self.status_notice = Some(msg);
            return;
        }
        let snap = self.snapshot();
        let current = snap
            .and_then(|s| s.head_branch.clone())
            .unwrap_or_else(|| "HEAD".to_string());
        let count = snap.and_then(|s| {
            let target = s.resolve_revision(rev)?;
            let head = s.head_oid?;
            Some(s.count_exclusive(target, head))
        });
        let remembered = self.load_merge_options();
        self.blur_active_pane(false);
        self.push_dialog(Dialog::Merge(MergeDialog::new(rev, current, count, remembered)));
    }

    fn pull_options_path(&self) -> Option<std::path::PathBuf> {
        let snap = self.snapshot()?;
        (!snap.common_dir.as_os_str().is_empty())
            .then(|| snap.common_dir.join("jbgit-pull-options"))
    }

    fn load_pull_options(&self) -> PullOptions {
        if let Some(opts) = &self.remembered_pull_options {
            return opts.clone();
        }
        #[cfg(test)]
        {
            self.pull_options_path()
                .and_then(|p| std::fs::read_to_string(p).ok())
                .map(|t| PullOptions::from_remembered(&t))
                .unwrap_or_default()
        }
        #[cfg(not(test))]
        {
            PullOptions::default()
        }
    }

    fn prompt_pull(&mut self) {
        if let Err(msg) = self.require_no_ongoing() {
            self.status_notice = Some(msg);
            return;
        }
        let Some(snap) = self.snapshot().cloned() else {
            self.status_notice = Some(self.i18n.t("toast.repo_not_loaded").to_string());
            return;
        };
        if snap.head_branch.is_none() {
            self.status_notice = Some(self.i18n.t("menu.log.detached_no_branch").to_string());
            return;
        }
        let remembered = self.load_pull_options();
        self.blur_active_pane(false);
        self.push_dialog(Dialog::Pull(PullDialog::from_snapshot(&snap, remembered)));
    }

    fn rebase_options_path(&self) -> Option<std::path::PathBuf> {
        let snap = self.snapshot()?;
        (!snap.common_dir.as_os_str().is_empty())
            .then(|| snap.common_dir.join("jbgit-rebase-options"))
    }

    fn load_rebase_options(&self) -> RebaseOptions {
        if let Some(opts) = &self.remembered_rebase_options {
            return opts.clone();
        }
        #[cfg(test)]
        {
            self.rebase_options_path()
                .and_then(|p| std::fs::read_to_string(p).ok())
                .map(|t| RebaseOptions::from_remembered(&t))
                .unwrap_or_default()
        }
        #[cfg(not(test))]
        {
            RebaseOptions::default()
        }
    }

    fn prompt_rebase(&mut self, onto: Option<String>) {
        if let Err(msg) = self.require_no_ongoing() {
            self.status_notice = Some(msg);
            return;
        }
        let Some(snap) = self.snapshot().cloned() else {
            self.status_notice = Some(self.i18n.t("toast.repo_not_loaded").to_string());
            return;
        };
        let remembered = self.load_rebase_options();
        let git_version = self.git_version;
        self.blur_active_pane(false);
        self.push_dialog(Dialog::Rebase(RebaseDialog::from_snapshot(
            &snap,
            onto,
            git_version,
            remembered,
        )));
    }

    /// Resolves `onto` in the current snapshot and opens `RebaseTodoDialog` for the
    /// linear first-parent commits in `onto..HEAD`. Returns `false` if no linear
    /// rewrite plan can be built (caller falls back to `GitOp::Rebase`).
    fn open_interactive_rebase_onto(&mut self, onto: &str) -> bool {
        let Some(snap) = self.snapshot().cloned() else {
            return false;
        };
        let Some(head_oid) = snap.head_oid else {
            return false;
        };
        let Some(onto_oid) = snap.resolve_revision(onto) else {
            return false;
        };
        if head_oid == onto_oid {
            return false;
        }
        // Walk first-parent chain from HEAD to find the direct child of `onto_oid`.
        let mut cur = head_oid;
        let mut oldest = None;
        for _ in 0..5000 {
            let Some(c) = snap.commit(&cur) else {
                break;
            };
            if c.parents.len() > 1 {
                return false;
            }
            if c.parents.first() == Some(&onto_oid) {
                oldest = Some(cur);
                break;
            }
            let Some(&parent) = c.parents.first() else {
                break;
            };
            cur = parent;
        }
        let Some(oldest_oid) = oldest else {
            return false;
        };
        match snap.plan_rewrite_typed(oldest_oid) {
            Ok(plan) => {
                self.push_dialog(Dialog::RebaseTodo(crate::dialogs::RebaseTodoDialog::new(
                    plan.base,
                    plan.entries,
                    plan.messages,
                )));
                true
            }
            Err(_) => false,
        }
    }

    /// Requests single-flight commit details and (when open) file diff for the
    /// active `RebaseTodoDialog` selection.
    pub(crate) fn sync_rebase_todo_preview(&mut self, effects: &mut Vec<Effect>) {
        let context = DiffContext::from_lines(self.config.jbgit.diff.context_lines);
        let Some(ModalLayer::Dialog(d)) = self.modal_stack.last_mut() else {
            return;
        };
        let Dialog::RebaseTodo(r) = d.as_mut() else {
            return;
        };
        let Some(details_key) = r.desired_details_key() else {
            return;
        };
        if !self.diff_caches.details.contains(&details_key) {
            if r.details_requested.as_ref() != Some(&details_key) {
                r.details_requested = Some(details_key.clone());
                effects.push(Effect::RequestCommitDetails {
                    slot: RequestSlotId::RebaseCommitDetails,
                    key: details_key,
                });
            }
            return;
        }
        let Some(bundle) = self.diff_caches.details.peek(&details_key) else {
            return;
        };
        if !bundle.changes.is_empty() {
            r.selected_file = r.selected_file.min(bundle.changes.len() - 1);
        } else {
            r.selected_file = 0;
        }
        let mut cached_for_syntax = None;
        if r.show_diff
            && let Some(change) = bundle.changes.get(r.selected_file)
        {
            let diff_key = FileDiffKey {
                details: details_key,
                path: change.path.clone(),
                old_path: change.old_path.clone(),
                context,
                whitespace: self.config.jbgit.diff.whitespace.to_core(),
            };
            if let Some(cached) = self.diff_caches.peek_diff(&diff_key) {
                if self.diff_caches.peek_syntax(&diff_key).is_none() {
                    cached_for_syntax = Some(std::sync::Arc::clone(cached));
                }
            } else if r.diff_requested.as_ref() != Some(&diff_key) {
                r.diff_requested = Some(diff_key.clone());
                effects.push(Effect::RequestFileDiff {
                    slot: RequestSlotId::RebaseFileDiff,
                    key: diff_key,
                });
            }
        }
        if let Some(diff) = cached_for_syntax {
            self.schedule_file_syntax(diff, effects);
        }
    }

    /// `[update] mode` from `jbgit.toml` (`None` = follow `pull.rebase`).
    pub(crate) fn configured_update_mode(&self) -> Option<UpdateMode> {
        self.config.jbgit.update.mode.to_update_mode()
    }

    pub(crate) fn update_project_op(
        &self,
        mode: Option<UpdateMode>,
        update_submodules: bool,
    ) -> GitOp {
        GitOp::UpdateProject {
            mode,
            save_changes: self.config.jbgit.update.save_changes,
            update_other_branches: self.config.jbgit.update.update_other_branches,
            update_submodules,
        }
    }

    /// True when the checked-out branch has no upstream or its upstream ref is `[gone]`.
    pub(crate) fn needs_upstream_picker_for_update(&self) -> bool {
        let Some(snap) = self.snapshot() else {
            return false;
        };
        let Some(head_ref) = snap.head_ref() else {
            return false;
        };
        head_ref.upstream.is_none() || snap.is_upstream_gone(head_ref)
    }

    /// Reason Update Project cannot start (ongoing operation / detached HEAD).
    pub(crate) fn update_project_blocker(&self) -> Option<String> {
        if let Err(msg) = self.require_no_ongoing() {
            return Some(msg);
        }
        if self.snapshot().and_then(|s| s.head_branch.as_ref()).is_none() {
            return Some(self.i18n.t("menu.log.detached_no_branch").to_string());
        }
        None
    }

    fn prompt_update_project(&mut self) {
        if let Some(msg) = self.update_project_blocker() {
            self.status_notice = Some(msg);
            return;
        }
        let i18n = self.i18n.clone();
        let has_submodules = self.snapshot().is_some_and(|s| !s.submodules.is_empty());

        // If current branch has no upstream or its upstream ref is [gone], let
        // the user pick a tracking branch first, then run Update Project.
        if self.needs_upstream_picker_for_update()
            && let Some(snap) = self.repo_snapshot.clone()
            && let Some(branch) = snap.head_branch.clone()
        {
            let mut remote_branches: Vec<String> = snap
                .refs
                .iter()
                .filter(|r| r.kind == RefKind::RemoteBranch)
                .map(|r| r.name.clone())
                .collect();
            let preferred = snap
                .default_remote()
                .map(|r| format!("{r}/{branch}"));
            remote_branches.sort_by(|a, b| match (
                preferred.as_deref() == Some(a.as_str()),
                preferred.as_deref() == Some(b.as_str()),
            ) {
                (true, false) => std::cmp::Ordering::Less,
                (false, true) => std::cmp::Ordering::Greater,
                _ => jbgit_core::natural_cmp(a, b),
            });
            if remote_branches.is_empty() {
                self.status_notice = Some(
                    i18n.t_fmt("toast.update_no_upstream_candidates", &[("branch", &branch)]),
                );
                return;
            }
            let mode = self.configured_update_mode();
            let options = remote_branches
                .iter()
                .take(12)
                .map(|up| {
                    let base_op = GitOp::Sequence(vec![
                        GitOp::SetUpstream {
                            branch: branch.clone(),
                            upstream: up.clone(),
                        },
                        self.update_project_op(mode, false),
                    ]);
                    let sub_op = GitOp::Sequence(vec![
                        GitOp::SetUpstream {
                            branch: branch.clone(),
                            upstream: up.clone(),
                        },
                        self.update_project_op(mode, true),
                    ]);
                    ChoiceOption::new(up.clone(), None, vec![Action::RunGitOp(base_op)])
                        .detail(i18n.t_fmt("dialog.update_project.track_detail", &[("branch", &branch), ("upstream", up)]))
                        .when_checked(vec![Action::RunGitOp(sub_op)])
                })
                .collect();
            let is_gone = snap.head_ref().is_some_and(|r| snap.is_upstream_gone(r));
            let body_key = if is_gone {
                "dialog.update_project.upstream_gone_body"
            } else {
                "dialog.update_project.no_upstream_body"
            };
            let mut dlg = ChoiceDialog::radio(
                i18n.t("dialog.update_project.select_upstream_title"),
                vec![i18n.t_fmt(body_key, &[("branch", &branch)])],
                options,
                0,
                i18n.t("dialog.update_project.set_and_update_btn"),
            );
            if has_submodules {
                dlg.checkbox = Some(crate::dialogs::ChoiceCheckbox {
                    label: i18n.t("dialog.update_project.update_submodules").to_string(),
                    checked: false,
                    checked_actions: Vec::new(),
                });
            }
            self.blur_active_pane(false);
            self.push_dialog(Dialog::Choice(dlg));
            return;
        }

        let modes: [(Option<UpdateMode>, &str, &str, char); 3] = [
            (
                None,
                "dialog.update_project.mode_config",
                "dialog.update_project.mode_config_desc",
                'g',
            ),
            (
                Some(UpdateMode::Merge),
                "dialog.update_project.mode_merge",
                "dialog.update_project.mode_merge_desc",
                'm',
            ),
            (
                Some(UpdateMode::Rebase),
                "dialog.update_project.mode_rebase",
                "dialog.update_project.mode_rebase_desc",
                'r',
            ),
        ];
        let configured = self.configured_update_mode();
        let selected = modes.iter().position(|m| m.0 == configured).unwrap_or(0);
        let options = modes
            .iter()
            .map(|(mode, label, desc, mn)| {
                ChoiceOption::new(
                    i18n.t(label),
                    Some(*mn),
                    vec![Action::RunGitOp(self.update_project_op(*mode, false))],
                )
                .detail(i18n.t(desc))
                .when_checked(vec![Action::RunGitOp(self.update_project_op(*mode, true))])
            })
            .collect();
        let mut body = vec![i18n.t("dialog.update_project.body").to_string()];
        if !self.config.jbgit.update.update_other_branches {
            body.push(i18n.t("dialog.update_project.current_only").to_string());
        }
        let mut dlg = ChoiceDialog::radio(
            i18n.t("dialog.update_project.title"),
            body,
            options,
            selected,
            i18n.t("dialog.update_project.confirm_btn"),
        );
        if has_submodules {
            dlg.checkbox = Some(crate::dialogs::ChoiceCheckbox {
                label: i18n.t("dialog.update_project.update_submodules").to_string(),
                checked: false,
                checked_actions: Vec::new(),
            });
        }
        self.blur_active_pane(false);
        self.push_dialog(Dialog::Choice(dlg));
    }

    pub(crate) fn prompt_unshallow(&mut self) {
        if let Err(msg) = self.require_no_ongoing() {
            self.status_notice = Some(msg);
            return;
        }
        let Some(snap) = self.repo_snapshot.as_ref() else {
            self.status_notice = Some(self.i18n.t("toast.repo_not_loaded").to_string());
            return;
        };
        if !snap.is_shallow {
            self.status_notice = Some(self.i18n.t("toast.not_shallow").to_string());
            return;
        }
        let Some(remote_name) = snap.default_remote() else {
            self.status_notice = Some(self.i18n.t("toast.no_remote_for_unshallow").to_string());
            return;
        };
        let i18n = self.i18n.clone();
        self.blur_active_pane(false);
        self.push_dialog(Dialog::Choice(ChoiceDialog::confirm_with_default(
            i18n.t("dialog.unshallow.title"),
            vec![i18n.t_fmt("dialog.unshallow.body", &[("remote", &remote_name)])],
            i18n.t("dialog.unshallow.confirm_btn"),
            i18n.cancel(),
            false,
            true,
            vec![Action::RunGitOp(GitOp::Unshallow {
                remote: remote_name,
            })],
        )));
    }

    fn notify_update_info(&mut self, info: Option<&UpdateInfo>) {
        let i18n = self.i18n.clone();
        let skipped = info.map(|i| i.skipped.as_slice()).unwrap_or_default();
        let skipped_part = (!skipped.is_empty()).then(|| {
            let names = skipped
                .iter()
                .take(3)
                .map(|s| format!("{} ({})", s.branch, s.reason))
                .collect::<Vec<_>>()
                .join(", ");
            let n_s = skipped.len().to_string();
            i18n.t_fmt("toast.update_skipped", &[("count", &n_s), ("names", &names)])
        });
        let Some(info) = info.filter(|i| !i.is_empty()) else {
            let mut text = i18n.t("toast.update_up_to_date").to_string();
            if let Some(s) = skipped_part {
                text = format!("{text} · {s}");
            }
            self.status_notice = Some(text.clone());
            self.notification = Some(Notification {
                text,
                is_error: false,
                action: None,
            });
            return;
        };
        let c_s = info.new_commits.to_string();
        let f_s = info.files_changed.to_string();
        let mut text = i18n.t_fmt("toast.update_done", &[("commits", &c_s), ("files", &f_s)]);
        if let Some(s) = skipped_part {
            text = format!("{text} · {s}");
        }
        self.status_notice = Some(text.clone());
        self.notification = Some(Notification {
            text,
            is_error: false,
            action: Some((
                i18n.t("toast.view_btn").into(),
                vec![Action::FilterLogByRange {
                    label: info.range_label(),
                    oids: info.commits.clone(),
                }],
            )),
        });
    }

    /// Pops the stash Update Project kept while the user resolved conflicts.
    pub(crate) fn restore_pending_update_stash(
        &mut self,
        from_snapshot: bool,
        effects: &mut Vec<Effect>,
    ) {
        if !self.pending_update_stash {
            return;
        }
        if self.ongoing_kind().is_some() {
            return;
        }
        self.pending_update_stash = false;
        self.pending_update_stash_restore = false;
        let stashes = self.snapshot().map(|s| s.stashes.as_slice()).unwrap_or(&[]);
        let matched = stashes
            .iter()
            .find(|e| e.message.contains(UPDATE_PROJECT_STASH_MESSAGE))
            .map(|e| e.index);
        let index = match matched {
            Some(idx) => idx,
            // Only fall back to index 0 when invoked immediately after Ongoing
            // finished on a pre-refresh snapshot that had no stashes at all;
            // never pop an unrelated existing stash@{0} or a missing stash after
            // a fresh snapshot arrived.
            None if !from_snapshot && stashes.is_empty() => 0,
            None => return,
        };
        self.start_git_op(
            GitOp::StashApply {
                index,
                pop: true,
                reinstate_index: false,
            },
            effects,
        );
    }

    /// Reacts to a finished [`GitOp`]: notices, Smart Operation dialog,
    /// merged-branch cleanup, `[Restore Branch]`, clipboard patches.
    pub(crate) fn apply_git_outcome(&mut self, outcome: GitOpOutcome, effects: &mut Vec<Effect>) {
        let op = match &outcome.op {
            GitOp::Smart(inner) => inner.as_ref().clone(),
            other => other.clone(),
        };
        if matches!(op, GitOp::Fetch | GitOp::FetchRemote { .. }) {
            for layer in &mut self.modal_stack {
                if let ModalLayer::Dialog(d) = layer
                    && let Dialog::Pull(p) = d.as_mut()
                {
                    p.fetching = false;
                }
            }
        }
        let is_smart = matches!(outcome.op, GitOp::Smart(_));
        let op_label = self.i18n.git_op_label(&outcome.op);
        if outcome.success {
            self.status_notice = Some(self.i18n.t_fmt("toast.op_done", &[("label", &op_label)]));
            match &op {
                GitOp::DeleteBranch { name, tip, .. } => {
                    let short_s = tip.short();
                    self.notification = Some(Notification {
                        text: self.i18n.t_fmt(
                            "toast.deleted_branch",
                            &[("name", name), ("short", &short_s)],
                        ),
                        is_error: false,
                        action: Some((
                            self.i18n.t("toast.restore_branch_btn").into(),
                            vec![Action::RunGitOp(GitOp::RestoreBranch {
                                name: name.clone(),
                                oid: *tip,
                            })],
                        )),
                    });
                }
                GitOp::DeleteTag { name, tip } => {
                    self.notify_deleted_tag(name, *tip);
                }
                GitOp::Sequence(ops) => {
                    self.pending_batch_branch_deletes.clear();
                    if !ops.is_empty()
                        && ops
                            .iter()
                            .all(|o| matches!(o, GitOp::DeleteBranch { .. }))
                    {
                        let restore_ops: Vec<GitOp> = ops
                            .iter()
                            .filter_map(|o| match o {
                                GitOp::DeleteBranch { name, tip, .. } => {
                                    Some(GitOp::RestoreBranch {
                                        name: name.clone(),
                                        oid: *tip,
                                    })
                                }
                                _ => None,
                            })
                            .collect();
                        let count_s = restore_ops.len().to_string();
                        self.notification = Some(Notification {
                            text: self.i18n.t_fmt(
                                "toast.deleted_branches_batch",
                                &[("count", &count_s)],
                            ),
                            is_error: false,
                            action: Some((
                                self.i18n.t_fmt(
                                    "toast.restore_branches_batch_btn",
                                    &[("count", &count_s)],
                                ),
                                vec![Action::RunGitOp(GitOp::Sequence(restore_ops))],
                            )),
                        });
                    }
                    if let Some(GitOp::DeleteTag { name, tip }) = ops.first() {
                        self.notify_deleted_tag(name, *tip);
                    }
                    if let Some(GitOp::Push {
                        branch,
                        remote,
                        remote_branch,
                        ..
                    }) = ops.last()
                    {
                        self.notify_push_summary(branch, remote, remote_branch, &outcome.stdout);
                    }
                }
                GitOp::Push {
                    branch,
                    remote,
                    remote_branch,
                    ..
                } => {
                    self.notify_push_summary(branch, remote, remote_branch, &outcome.stdout);
                }
                GitOp::Pull { options }
                    if options.leaves_changes_uncommitted() && outcome.prepared_message.is_some() =>
                {
                    let msg = outcome.prepared_message.clone().unwrap_or_default();
                    let now = self.now();
                    self.stage_workbench.apply_prepared_message(&msg, now);
                    effects.extend(self.dispatch(Action::SwitchTab(WorkspaceTab::Commit)));
                    let text = self
                        .i18n
                        .t(if options.squash {
                            "toast.squash_staged"
                        } else {
                            "toast.merge_staged"
                        })
                        .to_string();
                    self.status_notice = Some(text.clone());
                    self.notification = Some(Notification {
                        text,
                        is_error: false,
                        action: None,
                    });
                }
                GitOp::Merge { options, .. }
                    if options.leaves_changes_uncommitted() && outcome.prepared_message.is_some() =>
                {
                    let msg = outcome.prepared_message.clone().unwrap_or_default();
                    let now = self.now();
                    self.stage_workbench.apply_prepared_message(&msg, now);
                    effects.extend(self.dispatch(Action::SwitchTab(WorkspaceTab::Commit)));
                    let text = self
                        .i18n
                        .t(if options.squash {
                            "toast.squash_staged"
                        } else {
                            "toast.merge_staged"
                        })
                        .to_string();
                    self.status_notice = Some(text.clone());
                    self.notification = Some(Notification {
                        text,
                        is_error: false,
                        action: None,
                    });
                }
                GitOp::Merge { rev, options } if !options.leaves_changes_uncommitted() => {
                    let snap = self.snapshot();
                    let current = snap.and_then(|s| s.head_branch.clone());
                    let source = snap.and_then(|s| s.local_branch(rev)).map(|r| r.target_oid);
                    if let Some(tip) = source
                        && !is_protected_branch(rev)
                        && current.as_deref() != Some(rev.as_str())
                    {
                        let cur = current.unwrap_or_else(|| "HEAD".into());
                        let i18n = self.i18n.clone();
                        self.push_dialog(Dialog::Choice(ChoiceDialog::buttons(
                            i18n.t("dialog.merge_cleanup.title"),
                            vec![
                                i18n.t_fmt("dialog.merge_cleanup.merged_into", &[("rev", rev), ("cur", &cur)]),
                                i18n.t_fmt("dialog.merge_cleanup.ask_delete", &[("rev", rev)]),
                            ],
                            vec![
                                ChoiceOption::new(
                                    i18n.t_fmt("dialog.merge_cleanup.delete_btn", &[("rev", rev)]),
                                    Some('d'),
                                    vec![Action::RunGitOp(GitOp::DeleteBranch {
                                        name: rev.clone(),
                                        force: false,
                                        tip,
                                    })],
                                ),
                                ChoiceOption::new(i18n.t("dialog.merge_cleanup.keep_btn"), Some('k'), Vec::new()),
                            ],
                        )));
                    }
                }
                GitOp::AcceptSide { paths, .. } => {
                    let prompt_done = if let Some(cq) = self.modal_stack.conflict_queue_mut() {
                        for p in paths {
                            if let Some(entry) = cq.entries.iter_mut().find(|e| &e.path == p) {
                                entry.resolved = true;
                            }
                            cq.multi_selected.remove(p);
                        }
                        if cq.all_resolved() && !cq.completion_prompted {
                            cq.completion_prompted = true;
                            true
                        } else {
                            false
                        }
                    } else {
                        false
                    };
                    if prompt_done {
                        self.open_all_conflicts_resolved_dialog();
                    }
                }
                GitOp::UnresolveConflict { paths } => {
                    if let Some(cq) = self.modal_stack.conflict_queue_mut() {
                        for p in paths {
                            if let Some(entry) = cq.entries.iter_mut().find(|e| &e.path == p) {
                                entry.resolved = false;
                            }
                            cq.multi_selected.remove(p);
                        }
                        cq.completion_prompted = false;
                    }
                }
                GitOp::PatchToClipboard { .. } => {
                    let mut text = outcome.stdout.join("\n");
                    text.push('\n');
                    let count_s = outcome.stdout.len().to_string();
                    self.status_notice = Some(self.i18n.t_fmt("toast.copied_patch", &[("count", &count_s)]));
                    effects.push(Effect::CopyToClipboard(text));
                }
                GitOp::CreatePatchFile { .. } => {
                    let count_s = outcome.stdout.len().to_string();
                    self.status_notice = Some(self.i18n.t_fmt("toast.created_patches", &[("count", &count_s)]));
                }
                GitOp::RewordHead { .. } | GitOp::RewriteHistory { .. } => {
                    self.log.graph.clear_multi_selection();
                    if outcome.paused_for_edit {
                        self.stage_workbench.set_amend_flag(true);
                        effects.extend(self.dispatch(Action::SwitchTab(WorkspaceTab::Commit)));
                        let text = self.i18n.t("toast.rebase_edit_paused").to_string();
                        self.status_notice = Some(text.clone());
                        self.notification = Some(Notification {
                            text,
                            is_error: false,
                            action: Some((
                                self.i18n.t("menu.branch.continue_rebase").into(),
                                vec![Action::RunGitOp(GitOp::Ongoing {
                                    kind: OngoingKind::Rebase,
                                    step: OngoingStep::Continue,
                                })],
                            )),
                        });
                    }
                }
                GitOp::UpdateProject { .. } => {
                    self.notify_update_info(outcome.update_info.as_ref());
                }
                GitOp::SetIdentity { name, email, .. } => {
                    self.stage_workbench
                        .set_identity_in_precheck(name.clone(), email.clone());
                    effects.push(Effect::RequestCommitPrecheck);
                    self.status_notice = Some(self.i18n.t("toast.identity_saved").to_string());
                    if let Some(pending) = self.pending_commit_after_identity.take() {
                        effects.extend(self.dispatch(Action::ExecuteCommitConfirmed(pending)));
                    }
                }
                GitOp::AddRemote { name, .. } => {
                    let text = self.i18n.t_fmt("toast.remote_added", &[("name", name)]);
                    self.status_notice = Some(text.clone());
                    self.notification = Some(Notification {
                        text,
                        is_error: false,
                        action: Some((
                            self.i18n.t_fmt("toast.fetch_remote_btn", &[("name", name)]),
                            vec![Action::RunGitOp(GitOp::FetchRemote { name: name.clone() })],
                        )),
                    });
                    self.select_remote_in_modal(name);
                }
                GitOp::RenameRemote { new, .. } => self.select_remote_in_modal(new),
                GitOp::AddWorktree { .. }
                | GitOp::RemoveWorktree { .. }
                | GitOp::PruneWorktrees => self.on_worktree_op_succeeded(&op),
                GitOp::SubmoduleUpdate { path } => {
                    self.status_notice = Some(match path {
                        Some(p) => self.i18n.t_fmt("toast.submodule_updated_one", &[("path", p)]),
                        None => self.i18n.t("toast.submodules_updated").to_string(),
                    });
                }
                GitOp::Unshallow { remote } => {
                    self.status_notice = Some(
                        self.i18n
                            .t_fmt("toast.unshallow_done", &[("remote", remote)]),
                    );
                }
                GitOp::BisectStart { .. } | GitOp::BisectMark { .. } => {
                    if let Some(culprit_line) = outcome
                        .stdout
                        .iter()
                        .find(|l| l.contains("is the first bad commit"))
                    {
                        let text = culprit_line.trim().to_string();
                        self.status_notice = Some(text.clone());
                        self.notification = Some(Notification {
                            text,
                            is_error: false,
                            action: Some((
                                self.i18n.t("banner.bisect_reset_btn").into(),
                                vec![Action::RunGitOp(GitOp::BisectReset)],
                            )),
                        });
                    }
                }
                GitOp::Ongoing {
                    step: OngoingStep::Continue | OngoingStep::Abort | OngoingStep::Skip,
                    ..
                } => {
                    if outcome.paused_for_edit {
                        self.stage_workbench.set_amend_flag(true);
                        effects.extend(self.dispatch(Action::SwitchTab(WorkspaceTab::Commit)));
                        let text = self.i18n.t("toast.rebase_edit_paused").to_string();
                        self.status_notice = Some(text.clone());
                        self.notification = Some(Notification {
                            text,
                            is_error: false,
                            action: Some((
                                self.i18n.t("menu.branch.continue_rebase").into(),
                                vec![Action::RunGitOp(GitOp::Ongoing {
                                    kind: OngoingKind::Rebase,
                                    step: OngoingStep::Continue,
                                })],
                            )),
                        });
                    } else {
                        let now = self.now();
                        self.stage_workbench.clear_stale_prepared_message(now);
                        self.pending_update_stash_restore = true;
                        self.restore_pending_update_stash(false, effects);
                    }
                }
                _ => {}
            }
            if outcome.stash_kept {
                self.notification = Some(Notification {
                    text: self.i18n.t("toast.stash_kept").into(),
                    is_error: true,
                    action: Some((
                        self.i18n.t("toast.unstash_btn").into(),
                        vec![Action::RunGitOp(GitOp::StashApply {
                            index: 0,
                            pop: true,
                            reinstate_index: false,
                        })],
                    )),
                });
            }
            return;
        }

        let label = op_label;
        if matches!(op, GitOp::SetIdentity { .. }) {
            self.pending_commit_after_identity = None;
        }
        if let Some(f) = &outcome.failure
            && self.on_worktree_failure(&op, f)
        {
            return;
        }
        let i18n = self.i18n.clone();
        match outcome.failure.clone() {
            Some(
                GitFailure::LocalChangesOverwritten { files }
                | GitFailure::UntrackedOverwritten { files },
            ) if op.supports_smart() && !is_smart => {
                let verb_key = match &op {
                    GitOp::Merge { .. } => "dialog.smart.verb.merge",
                    GitOp::Rebase { .. } => "dialog.smart.verb.rebase",
                    GitOp::Pull { options } if options.rebase => "dialog.smart.verb.rebase",
                    GitOp::Pull { .. } => "dialog.smart.verb.pull",
                    GitOp::CherryPick { .. } => "dialog.smart.verb.cherry_pick",
                    GitOp::Revert { .. } => "dialog.smart.verb.revert",
                    GitOp::Reset { .. } => "dialog.smart.verb.reset",
                    _ => "dialog.smart.verb.checkout",
                };
                let verb = i18n.t(verb_key);
                let count_s = files.len().max(1).to_string();
                let mut body = vec![i18n.t_fmt(
                    "dialog.smart.overwrite_prompt",
                    &[("label", &label), ("count", &count_s)],
                )];
                body.extend(files.iter().take(6).map(|f| format!("  • {f}")));
                if files.len() > 6 {
                    let rem_s = (files.len() - 6).to_string();
                    body.push(i18n.t_fmt("dialog.smart.more_files", &[("count", &rem_s)]));
                }
                body.push(String::new());
                body.push(i18n.t_fmt("dialog.smart.explain", &[("verb", verb)]));
                let mut options = vec![ChoiceOption::new(
                    i18n.t_fmt("dialog.smart.title", &[("verb", verb)]),
                    Some('s'),
                    vec![Action::RunGitOp(GitOp::Smart(Box::new(op.clone())))],
                )];
                let force_target = match &op {
                    GitOp::Checkout { target, .. } => Some(target.clone()),
                    _ => None,
                };
                if let Some(target) = force_target {
                    options.push(
                        ChoiceOption::new(
                            i18n.t("dialog.smart.force_checkout"),
                            Some('f'),
                            vec![Action::RunGitOp(GitOp::ForceCheckout { target })],
                        )
                        .danger(),
                    );
                }
                options.push(ChoiceOption::new(i18n.cancel(), Some('c'), Vec::new()));
                self.status_notice = Some(i18n.t_fmt("toast.overwritten_notice", &[("label", &label)]));
                self.push_dialog(Dialog::Choice(ChoiceDialog::buttons(
                    i18n.t_fmt("dialog.smart.title", &[("verb", verb)]),
                    body,
                    options,
                )));
            }
            Some(GitFailure::Conflicts) => {
                let is_update = matches!(op, GitOp::UpdateProject { .. });
                if is_update && outcome.stash_kept && outcome.update_info.is_none() {
                    self.pending_update_stash = true;
                    self.pending_update_stash_restore = false;
                }
                let kept = if is_update && outcome.stash_kept {
                    if outcome.update_info.is_some() {
                        i18n.t("toast.update_stash_pop_conflict")
                    } else {
                        i18n.t("toast.update_stash_kept")
                    }
                } else if outcome.stash_kept {
                    i18n.t("toast.local_changes_kept")
                } else {
                    ""
                };
                self.status_notice = Some(i18n.t_fmt(
                    "toast.stopped_on_conflicts",
                    &[("label", &label), ("kept", kept)],
                ));
                self.notification = Some(Notification {
                    text: i18n.t_fmt(
                        "toast.resolve_conflicts_toast",
                        &[("label", &label), ("kept", kept)],
                    ),
                    is_error: true,
                    action: Some((i18n.t("toast.resolve_btn").into(), vec![Action::OngoingResolve])),
                });
            }
            Some(GitFailure::EmptyCherryPick) => {
                self.push_dialog(Dialog::Choice(ChoiceDialog::buttons(
                    i18n.t("dialog.empty_cherry_pick.title"),
                    vec![i18n.t("dialog.empty_cherry_pick.body").to_string()],
                    vec![
                        ChoiceOption::new(
                            i18n.t("dialog.empty_cherry_pick.skip_btn"),
                            Some('s'),
                            vec![Action::RunGitOp(GitOp::Ongoing {
                                kind: OngoingKind::CherryPick,
                                step: OngoingStep::Skip,
                            })],
                        ),
                        ChoiceOption::new(
                            i18n.t("dialog.empty_cherry_pick.allow_empty_btn"),
                            Some('e'),
                            vec![Action::RunGitOp(GitOp::CherryPickAllowEmpty)],
                        ),
                        ChoiceOption::new(
                            i18n.t("dialog.empty_cherry_pick.abort_btn"),
                            Some('a'),
                            vec![Action::RunGitOp(GitOp::Ongoing {
                                kind: OngoingKind::CherryPick,
                                step: OngoingStep::Abort,
                            })],
                        ),
                    ],
                )));
            }
            Some(GitFailure::NotFullyMerged) => {
                if let GitOp::DeleteBranch { name, tip, .. } = &op {
                    let mut body = vec![i18n.t_fmt(
                        "dialog.confirm_delete.not_merged_prompt",
                        &[("name", name)],
                    )];
                    if let Some(snap) = self.snapshot()
                        && let Some(head) = snap.head_oid
                    {
                        const PREVIEW_LIMIT: usize = 8;
                        let total = snap.count_exclusive(*tip, head);
                        let commits = snap.exclusive_commits(*tip, head, PREVIEW_LIMIT);
                        for c in &commits {
                            body.push(format!("  • {} {}", c.oid.short(), c.subject));
                        }
                        if total > commits.len() && !commits.is_empty() {
                            let more = (total - commits.len()).to_string();
                            body.push(i18n.t_fmt(
                                "dialog.confirm_delete.and_more",
                                &[("count", &more)],
                            ));
                        }
                    }
                    body.push(i18n.t("dialog.confirm_delete.not_merged_warn").into());
                    self.push_dialog(Dialog::Choice(ChoiceDialog::confirm(
                        i18n.t("dialog.confirm_delete.title_unmerged"),
                        body,
                        i18n.t("dialog.confirm_delete.force_delete_btn"),
                        i18n.cancel(),
                        vec![Action::RunGitOp(GitOp::DeleteBranch {
                            name: name.clone(),
                            force: true,
                            tip: *tip,
                        })],
                    )));
                }
            }
            Some(GitFailure::TagAlreadyExists) => {
                match &op {
                    GitOp::CreateTag {
                        name,
                        target,
                        message,
                        force: false,
                        sign,
                    } => {
                        self.push_dialog(Dialog::Choice(ChoiceDialog::confirm(
                            i18n.t("dialog.tag_exists.title"),
                            vec![i18n.t_fmt("dialog.tag_exists.local_body", &[("name", name)])],
                            i18n.t("dialog.tag_exists.overwrite_btn"),
                            i18n.cancel(),
                            vec![Action::RunGitOp(GitOp::CreateTag {
                                name: name.clone(),
                                target: *target,
                                message: message.clone(),
                                force: true,
                                sign: *sign,
                            })],
                        )));
                    }
                    GitOp::PushTag {
                        remote,
                        name,
                        force: false,
                    } => {
                        self.push_dialog(Dialog::Choice(ChoiceDialog::confirm(
                            i18n.t("dialog.tag_exists.title"),
                            vec![i18n.t_fmt(
                                "dialog.tag_exists.remote_body",
                                &[("name", name), ("remote", remote)],
                            )],
                            i18n.t("dialog.tag_exists.force_push_btn"),
                            i18n.cancel(),
                            vec![Action::RunGitOp(GitOp::PushTag {
                                remote: remote.clone(),
                                name: name.clone(),
                                force: true,
                            })],
                        )));
                    }
                    _ => {
                        self.notify_error(i18n.t_fmt("toast.op_failed", &[("label", &label)]));
                    }
                }
            }
            Some(GitFailure::PushRejected) => {
                if let GitOp::Push {
                    branch,
                    remote,
                    remote_branch,
                    set_upstream,
                    force,
                    tags,
                    up_to,
                    no_verify,
                    expected_remote_oid,
                } = &op
                {
                    let target_branch = if remote_branch.is_empty() {
                        branch.as_str()
                    } else {
                        remote_branch.as_str()
                    };
                    let target_ref = format!("{remote}/{target_branch}");
                    let is_current = self
                        .snapshot()
                        .and_then(|s| s.head_branch.as_deref())
                        .is_none_or(|cur| cur == branch);
                    let is_protected = matches_protected_branch_patterns(
                        target_branch,
                        &self.config.jbgit.push.protected_branches,
                    );
                    let branch_tip = self
                        .snapshot()
                        .and_then(|s| s.local_branch(branch).map(|b| b.target_oid).or(s.head_oid));
                    let remote_tip = self
                        .snapshot()
                        .and_then(|s| s.remote_branch(remote, target_branch))
                        .map(|r| r.target_oid);
                    let rebase_has_merges = match (self.snapshot(), remote_tip, branch_tip) {
                        (Some(snap), Some(base), Some(tip)) => {
                            snap.has_merge_commits_in_range(base, tip)
                        }
                        _ => false,
                    };
                    let partial_up_to = up_to.filter(|oid| branch_tip != Some(*oid));
                    let smart_merge = GitOp::Smart(Box::new(GitOp::Sequence(vec![
                        GitOp::Fetch,
                        GitOp::Merge {
                            rev: target_ref.clone(),
                            options: MergeOptions::default(),
                        },
                    ])));
                    let smart_rebase = GitOp::Smart(Box::new(GitOp::Sequence(vec![
                        GitOp::Fetch,
                        GitOp::Rebase {
                            options: RebaseOptions::onto(
                                target_ref.clone(),
                                (!is_current).then(|| branch.clone()),
                            ),
                        },
                    ])));
                    let mut options = Vec::new();
                    if partial_up_to.is_some() {
                        if is_current {
                            options.push(ChoiceOption::new(
                                i18n.t("dialog.push.merge_only_btn"),
                                Some('m'),
                                vec![Action::RunGitOp(smart_merge)],
                            ));
                        }
                        let mut rebase_opt = ChoiceOption::new(
                            i18n.t("dialog.push.rebase_only_btn"),
                            Some('r'),
                            vec![Action::RunGitOp(smart_rebase)],
                        );
                        if rebase_has_merges {
                            rebase_opt = rebase_opt.danger();
                        }
                        options.push(rebase_opt);
                    } else {
                        let retry_push = GitOp::Push {
                            branch: branch.clone(),
                            remote: remote.clone(),
                            remote_branch: target_branch.to_string(),
                            set_upstream: *set_upstream,
                            force: *force,
                            tags: *tags,
                            up_to: None,
                            no_verify: *no_verify,
                            expected_remote_oid: None,
                        };
                        if is_current {
                            options.push(ChoiceOption::new(
                                i18n.t("dialog.push.merge_retry_btn"),
                                Some('m'),
                                vec![Action::RunGitOp(GitOp::Sequence(vec![
                                    smart_merge,
                                    retry_push.clone(),
                                ]))],
                            ));
                        }
                        let mut rebase_opt = ChoiceOption::new(
                            i18n.t("dialog.push.rebase_retry_btn"),
                            Some('r'),
                            vec![Action::RunGitOp(GitOp::Sequence(vec![
                                smart_rebase,
                                retry_push,
                            ]))],
                        );
                        if rebase_has_merges {
                            rebase_opt = rebase_opt.danger();
                        }
                        options.push(rebase_opt);
                    }
                    if !force.is_force() && !is_protected {
                        options.push(
                            ChoiceOption::new(
                                i18n.t("dialog.push.force_push_btn"),
                                Some('f'),
                                vec![Action::RunGitOp(GitOp::Push {
                                    branch: branch.clone(),
                                    remote: remote.clone(),
                                    remote_branch: target_branch.to_string(),
                                    set_upstream: *set_upstream,
                                    force: PushForce::WithLease,
                                    tags: *tags,
                                    up_to: *up_to,
                                    no_verify: *no_verify,
                                    expected_remote_oid: *expected_remote_oid,
                                })],
                            )
                            .danger(),
                        );
                    }
                    options.push(ChoiceOption::new(i18n.cancel(), Some('c'), Vec::new()));
                    let mut body = vec![i18n.t_fmt(
                        "dialog.push.rejected_body",
                        &[("remote", remote), ("branch", target_branch)],
                    )];
                    if rebase_has_merges {
                        body.push(i18n.t("dialog.push.rebase_merge_warning").to_string());
                    }
                    if let Some(oid) = partial_up_to {
                        body.push(i18n.t_fmt(
                            "dialog.push.rejected_up_to_note",
                            &[("oid", &oid.short())],
                        ));
                    } else if is_protected {
                        body.push(i18n.t_fmt(
                            "dialog.push_full.protected_force_blocked",
                            &[("branch", target_branch)],
                        ));
                    } else {
                        body.push(i18n.t("dialog.push.rejected_hint").into());
                    }
                    self.push_dialog(Dialog::Choice(ChoiceDialog::buttons(
                        i18n.t("dialog.push.rejected_title"),
                        body,
                        options,
                    )));
                } else {
                    self.notify_error(i18n.t_fmt("toast.push_rejected_notice", &[("label", &label)]));
                }
            }
            Some(GitFailure::NotFastForward) => match &op {
                GitOp::Merge { rev, options } if options.ff == FastForward::FfOnly => {
                    let cur = self
                        .snapshot()
                        .and_then(|s| s.head_branch.clone())
                        .unwrap_or_else(|| "HEAD".to_string());
                    let merge_anyway = GitOp::Merge {
                        rev: rev.clone(),
                        options: MergeOptions {
                            ff: FastForward::Default,
                            ..options.clone()
                        },
                    };
                    self.push_dialog(Dialog::Choice(ChoiceDialog::buttons(
                        i18n.t("dialog.not_ff.title"),
                        vec![
                            i18n.t_fmt("dialog.not_ff.body", &[("rev", rev), ("cur", &cur)]),
                            i18n.t("dialog.not_ff.hint").to_string(),
                        ],
                        vec![
                            ChoiceOption::new(
                                i18n.t("dialog.not_ff.merge_btn"),
                                Some('m'),
                                vec![Action::RunGitOp(merge_anyway)],
                            ),
                            ChoiceOption::new(
                                i18n.t_fmt("dialog.not_ff.rebase_btn", &[("rev", rev)]),
                                Some('r'),
                                vec![Action::RunGitOp(GitOp::Rebase {
                                    options: RebaseOptions::onto(rev.clone(), None),
                                })],
                            ),
                            ChoiceOption::new(i18n.cancel(), Some('c'), Vec::new()),
                        ],
                    )));
                }
                _ => self.notify_error(i18n.t_fmt("toast.not_fast_forward", &[("label", &label)])),
            },
            Some(
                GitFailure::LocalChangesOverwritten { .. }
                | GitFailure::UntrackedOverwritten { .. },
            ) => {
                self.notify_error(i18n.t_fmt("toast.overwritten_notice", &[("label", &label)]));
            }
            Some(GitFailure::GpgSignFailed { detail }) => {
                let mut body = vec![
                    i18n.t_fmt("dialog.gpg_failed.body", &[("label", &label)]),
                    detail.clone(),
                    String::new(),
                ];
                body.extend(
                    i18n.t("dialog.gpg_failed.hint")
                        .lines()
                        .map(ToString::to_string),
                );
                self.status_notice = Some(i18n.t_fmt("toast.gpg_failed", &[("label", &label)]));
                self.push_dialog(Dialog::Choice(ChoiceDialog::buttons(
                    i18n.t("dialog.gpg_failed.title"),
                    body,
                    vec![ChoiceOption::new(i18n.t("common.close"), Some('c'), Vec::new())],
                )));
            }
            Some(GitFailure::AuthenticationFailed) => {
                let body = vec![
                    i18n.t_fmt("dialog.auth_failed.body", &[("label", &label)]),
                    String::new(),
                    i18n.t("dialog.auth_failed.https_hint").to_string(),
                    i18n.t("dialog.auth_failed.https_cmd").to_string(),
                    i18n.t("dialog.auth_failed.ssh_hint").to_string(),
                    i18n.t("dialog.auth_failed.ssh_cmd").to_string(),
                    String::new(),
                    i18n.t("dialog.auth_failed.no_store").to_string(),
                ];
                self.status_notice = Some(i18n.t_fmt("toast.auth_failed", &[("label", &label)]));
                self.push_dialog(Dialog::Choice(ChoiceDialog::buttons(
                    i18n.t("dialog.auth_failed.title"),
                    body,
                    vec![
                        ChoiceOption::new(
                            i18n.t("toast.show_console_btn"),
                            Some('s'),
                            vec![Action::SwitchTab(WorkspaceTab::Console)],
                        ),
                        ChoiceOption::new(i18n.t("common.close"), Some('c'), Vec::new()),
                    ],
                )));
            }
            Some(GitFailure::RemoteAlreadyExists) => {
                let name = match &op {
                    GitOp::AddRemote { name, .. } => name.clone(),
                    GitOp::RenameRemote { new, .. } => new.clone(),
                    _ => String::new(),
                };
                self.notify_error(i18n.t_fmt("toast.remote_exists", &[("name", &name)]));
            }
            Some(GitFailure::UpstreamGone { branch, .. }) => {
                self.prompt_set_upstream(&branch);
            }
            Some(GitFailure::Other(msg)) => {
                self.notify_error(i18n.t_fmt("toast.op_failed_msg", &[("label", &label), ("error", &msg)]));
            }
            Some(GitFailure::PatchDoesNotApply { detail }) => {
                self.notify_error(i18n.t_fmt("toast.op_failed_msg", &[("label", &label), ("error", &detail)]));
            }
            Some(GitFailure::PatchConflicts { paths }) => {
                // `git apply --3way` left unmerged entries: resolve them in the
                // Commit tab's Conflicts group (three-pane merge).
                let count = paths.len().max(1).to_string();
                let text = i18n.t_fmt("toast.patch_conflicts", &[("count", &count)]);
                self.status_notice = Some(text.clone());
                let mut resolve = vec![Action::SwitchTab(WorkspaceTab::Commit)];
                if let [one] = paths.as_slice() {
                    resolve.push(Action::OpenConflictResolver(one.clone()));
                }
                self.notification = Some(Notification {
                    text,
                    is_error: true,
                    action: Some((i18n.t("toast.resolve_conflicts_btn").into(), resolve)),
                });
                effects.push(Effect::RefreshRepoAndStage);
            }
            Some(
                GitFailure::BranchCheckedOutInWorktree { .. }
                | GitFailure::WorktreeDirty
                | GitFailure::WorktreeLocked,
            ) => self.notify_error(i18n.t_fmt("toast.op_failed", &[("label", &label)])),
            None => self.notify_error(i18n.t_fmt("toast.op_failed", &[("label", &label)])),
        }

        // If a Branch Cleanup batch DeleteBranch sequence stopped partway through,
        // still attach an Undo/Restore toast for all preceding branches already deleted.
        if let GitOp::DeleteBranch { name, .. } = &op
            && !self.pending_batch_branch_deletes.is_empty()
        {
            let batch = std::mem::take(&mut self.pending_batch_branch_deletes);
            if let Some(failed_idx) = batch.iter().position(|(b, _)| b == name)
                && failed_idx > 0
            {
                let restore_ops: Vec<GitOp> = batch[..failed_idx]
                    .iter()
                    .map(|(b, tip)| GitOp::RestoreBranch {
                        name: b.clone(),
                        oid: *tip,
                    })
                    .collect();
                let count_s = restore_ops.len().to_string();
                self.notification = Some(Notification {
                    text: i18n.t_fmt(
                        "toast.deleted_branches_partial",
                        &[("count", &count_s), ("failed", name)],
                    ),
                    is_error: true,
                    action: Some((
                        i18n.t_fmt("toast.restore_branches_batch_btn", &[("count", &count_s)]),
                        vec![Action::RunGitOp(GitOp::Sequence(restore_ops))],
                    )),
                });
            }
        }
    }

    /// Moves the open Remotes modal's selection onto `name` once it appears.
    fn select_remote_in_modal(&mut self, name: &str) {
        self.pending_remote_selection = Some(name.to_string());
        self.apply_pending_remote_selection();
    }

    /// Applies [`Self::select_remote_in_modal`] when the refreshed snapshot
    /// lists the remote (called after every snapshot load).
    pub(crate) fn apply_pending_remote_selection(&mut self) {
        let Some(name) = self.pending_remote_selection.clone() else {
            return;
        };
        let Some(idx) = self
            .snapshot()
            .and_then(|s| s.remotes.iter().position(|r| r.name == name))
        else {
            return;
        };
        self.pending_remote_selection = None;
        for layer in self.modal_stack.iter_mut() {
            if let ModalLayer::Remotes(rs) = layer {
                rs.selected = idx;
            }
        }
    }

    fn notify_deleted_tag(&mut self, name: &str, tip: Oid) {
        let short_s = tip.short();
        self.notification = Some(Notification {
            text: self
                .i18n
                .t_fmt("toast.deleted_tag", &[("name", name), ("short", &short_s)]),
            is_error: false,
            action: Some((
                self.i18n.t("toast.restore_tag_btn").into(),
                vec![Action::RunGitOp(GitOp::RestoreTag {
                    name: name.to_string(),
                    oid: tip,
                })],
            )),
        });
    }

    fn notify_push_summary(
        &mut self,
        branch: &str,
        remote: &str,
        remote_branch: &str,
        stdout: &[String],
    ) {
        let expected_commits = self.last_push_expected_commits.take();
        let parsed = parse_push_porcelain(stdout);
        let target_branch = if remote_branch.is_empty() {
            branch
        } else {
            remote_branch
        };
        let target = format!("{remote}/{target_branch}");

        let range_commits = parsed.commit_range.as_deref().and_then(|r| {
            let (old_hex, new_hex) = r.split_once("...").or_else(|| r.split_once(".."))?;
            let snap = self.snapshot()?;
            let old_oid = snap.resolve_revision(old_hex)?;
            let new_oid = snap.resolve_revision(new_hex)?;
            Some(snap.count_exclusive(new_oid, old_oid))
        });

        let mut parts = Vec::new();
        if let Some(new_b) = &parsed.new_branch {
            let full_new = format!("{remote}/{new_b}");
            parts.push(
                self.i18n
                    .t_fmt("toast.push_summary.new_branch", &[("target", &full_new)]),
            );
            if let Some(count) = expected_commits.filter(|&n| n > 0) {
                let c_s = count.to_string();
                parts.push(
                    self.i18n
                        .t_fmt("toast.push_summary.commits", &[("count", &c_s)]),
                );
            }
        } else if parsed.branch_up_to_date {
            parts.push(
                self.i18n
                    .t_fmt("toast.push_summary.up_to_date", &[("target", &target)]),
            );
        } else if let Some(count) = range_commits.or(expected_commits) {
            let c_s = count.to_string();
            let commits_part = self
                .i18n
                .t_fmt("toast.push_summary.commits", &[("count", &c_s)]);
            parts.push(self.i18n.t_fmt(
                "toast.push_summary.pushed_to",
                &[("commits", &commits_part), ("target", &target)],
            ));
        } else {
            parts.push(self.i18n.t_fmt(
                "toast.push_summary.pushed_branch",
                &[("branch", branch), ("target", &target)],
            ));
        }

        if parsed.pushed_tags > 0 {
            let t_s = parsed.pushed_tags.to_string();
            parts.push(
                self.i18n
                    .t_fmt("toast.push_summary.tags", &[("count", &t_s)]),
            );
        }

        let summary = parts.join(" · ");
        self.status_notice = Some(summary.clone());
        self.notification = Some(Notification {
            text: summary,
            is_error: false,
            action: None,
        });
    }

    fn notify_error(&mut self, text: String) {
        self.status_notice = Some(text.clone());
        self.notification = Some(Notification {
            text,
            is_error: true,
            action: Some((
                self.i18n.t("toast.show_console_btn").into(),
                vec![Action::SwitchTab(WorkspaceTab::Console)],
            )),
        });
    }
}
