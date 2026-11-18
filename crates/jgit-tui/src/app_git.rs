//! Spec 05 action dispatch: Git write operations, comparison / conflict-merge
//! workflows, and follow-up dialogs driven by operation outcomes (Smart
//! Operation, unmerged delete guard, merged-branch cleanup, `[Restore Branch]`).

use std::collections::HashSet;

use jgit_core::{
    ChangeRowKind, DetailsKey, GitFailure, GitOp, GitOpOutcome, Oid, OngoingKind, PaneId, RefKind,
    RepoSnapshot, RewritePlan, StageNodeKind, StageTreeRowKind, WorkspaceTab, is_protected_branch,
};

use crate::app::{
    Action, AppState, ConflictMergeState, Effect, ModalLayer, Notification, PopupKind,
};
use crate::branches_popup::first_selectable_row;
use crate::dialogs::{
    ChoiceDialog, ChoiceOption, Dialog, InputDialog, InputIntent, InputValidation,
};

impl AppState {
    pub(crate) fn snapshot(&self) -> Option<&RepoSnapshot> {
        self.repo_snapshot.as_deref()
    }

    pub(crate) fn ongoing_kind(&self) -> Option<OngoingKind> {
        self.snapshot()
            .and_then(|s| OngoingKind::from_state(&s.state))
    }

    pub(crate) fn conflicted_count(&self) -> usize {
        self.stage_status.as_ref().map_or(0, |s| s.conflicted.len())
    }

    pub(crate) fn has_staged_changes(&self) -> bool {
        self.stage_status
            .as_ref()
            .is_some_and(|s| !s.staged.is_empty())
    }

    /// Fails with a user-facing reason when a write operation must not start.
    pub(crate) fn require_no_ongoing(&self) -> Result<(), String> {
        match self.ongoing_kind() {
            Some(k) => {
                let kind_low = k.title().to_lowercase();
                Err(self.i18n.t_fmt("toast.ongoing_blocked", &[("kind", &kind_low)]))
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
        let snap = self.snapshot().ok_or("Repository not loaded")?;
        self.require_no_ongoing()?;
        snap.plan_rewrite(oldest, verb)
    }

    /// Selected Log commits ordered oldest → newest (topological).
    pub(crate) fn selected_commits_oldest_first(&self) -> Vec<Oid> {
        let Some(snap) = self.snapshot() else {
            return Vec::new();
        };
        let selected: HashSet<Oid> = self
            .log_graph
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
        self.status_notice = Some(format!("{}…", op.label()));
        self.notification = None;
        effects.push(Effect::GitOp(op));
    }

    fn push_dialog(&mut self, d: Dialog) {
        self.modal_stack.push(ModalLayer::Dialog(Box::new(d)));
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
                    let selected = items.iter().position(|i| !i.is_separator()).unwrap_or(0);
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
            Action::OpenDialog(d) => self.push_dialog((**d).clone()),
            Action::DialogConfirm => {
                let Some(ModalLayer::Dialog(d)) = self.modal_stack.last() else {
                    return true;
                };
                match d.confirm_actions() {
                    Some(actions) => {
                        self.modal_stack.pop();
                        self.run_actions(actions, effects);
                    }
                    None => {
                        if let Dialog::Input(i) = d.as_ref()
                            && let Some(err) = i.error()
                        {
                            self.status_notice = Some(err);
                        } else if let Dialog::RebaseTodo(r) = d.as_ref()
                            && let Some(err) = r.error()
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
                    e.action = jgit_core::TodoAction::Reword;
                    e.message = Some(message.clone());
                }
            }
            Action::OpenBranchesPopup { query } => {
                self.modal_stack
                    .retain(|l| !matches!(l, ModalLayer::ContextMenu { .. }));
                let selected = first_selectable_row(self, query);
                self.modal_stack.push(ModalLayer::FloatingPopup {
                    kind: PopupKind::BranchesQuickView,
                    query: query.clone(),
                    selected,
                });
            }
            Action::ToggleFavoriteRef { full, short } => {
                let on = self.branches.settings.favorites.toggle(full, short);
                self.branches.save_settings();
                let snap = self.repo_snapshot.clone();
                self.branches.sync_model(snap.as_ref(), true);
                self.status_notice = Some(format!(
                    "{} {short}",
                    if on { "★ Favorite:" } else { "Unfavorited:" }
                ));
            }
            Action::CheckoutRemoteBranch { remote_ref } => self.checkout_remote(remote_ref, effects),
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
            Action::CompareVersions { base, target } => {
                self.active_tab = WorkspaceTab::Log;
                self.focused_pane = PaneId::LogChangesTree;
                self.details.diff_preview = true;
                self.details.compare_override = Some(DetailsKey::compare_versions(*base, *target));
                let b_s = base.short();
                let t_s = target.short();
                self.status_notice = Some(self.i18n.t_fmt(
                    "toast.comparing_versions",
                    &[("base", &b_s), ("target", &t_s)],
                ));
            }
            Action::CompareWithLocal { target } => {
                self.active_tab = WorkspaceTab::Log;
                self.focused_pane = PaneId::LogChangesTree;
                self.details.diff_preview = true;
                self.details.compare_override = Some(DetailsKey::compare_with_local(*target));
                let t_s = target.short();
                self.status_notice = Some(self.i18n.t_fmt(
                    "toast.comparing_local",
                    &[("target", &t_s)],
                ));
            }
            Action::OpenConflictResolver(path) => {
                self.conflict_merge = Some(ConflictMergeState {
                    path: path.clone(),
                    doc: None,
                    selected_block: 0,
                    scroll: 0,
                });
                self.modal_stack.push(ModalLayer::FloatingPopup {
                    kind: PopupKind::ConflictMerge,
                    query: String::new(),
                    selected: 0,
                });
                effects.push(Effect::LoadConflictDocument(path.clone()));
            }
            Action::OpenExternalMergetool(path) => {
                effects.push(Effect::OpenExternalMergetool(path.clone()));
            }
            Action::ConflictSelectBlock(delta) => {
                if let Some(cm) = &mut self.conflict_merge
                    && let Some(doc) = &cm.doc
                    && doc.conflict_count > 0
                {
                    let next = (cm.selected_block as isize + delta)
                        .clamp(0, doc.conflict_count as isize - 1) as usize;
                    cm.selected_block = next;
                }
            }
            Action::ConflictSetBlock(idx) => {
                if let Some(cm) = &mut self.conflict_merge
                    && let Some(doc) = &cm.doc
                    && *idx < doc.conflict_count
                {
                    cm.selected_block = *idx;
                }
            }
            Action::ConflictApplyChoice(choice) => {
                if let Some(cm) = &mut self.conflict_merge
                    && let Some(doc) = &mut cm.doc
                {
                    doc.set_choice(cm.selected_block, *choice);
                    if cm.selected_block + 1 < doc.conflict_count {
                        cm.selected_block += 1;
                    }
                }
            }
            Action::ConflictAcceptAll(choice) => {
                if let Some(cm) = &mut self.conflict_merge
                    && let Some(doc) = &mut cm.doc
                {
                    doc.accept_all(*choice);
                }
            }
            Action::ConflictScroll(delta) => {
                if let Some(cm) = &mut self.conflict_merge {
                    cm.scroll = (cm.scroll as isize + delta).max(0) as usize;
                }
            }
            Action::ConflictSaveAndStage => {
                if let Some(cm) = &self.conflict_merge
                    && let Some(doc) = &cm.doc
                {
                    let all_resolved = doc.unresolved_count() == 0;
                    let path = doc.path.clone();
                    let content = doc.render_merged().into_bytes();
                    effects.push(Effect::SaveConflictResolution {
                        path: path.clone(),
                        content,
                        stage: all_resolved,
                    });
                    self.status_notice = Some(if all_resolved {
                        self.i18n.t_fmt("toast.saved_staged_conflict", &[("path", &path)])
                    } else {
                        let count_s = doc.unresolved_count().to_string();
                        self.i18n.t_fmt(
                            "toast.saved_unresolved_conflict",
                            &[("path", &path), ("count", &count_s)],
                        )
                    });
                    self.conflict_merge = None;
                    self.modal_stack.pop();
                }
            }
            Action::PromptDeleteBranch { name } => self.prompt_delete_branch(name, effects),
            Action::PromptPush { branch, up_to } => self.prompt_push(branch, *up_to),
            Action::UncommitHead => {
                self.active_tab = WorkspaceTab::Commit;
                self.focused_pane = PaneId::CommitMessageEditor;
                self.status_notice = Some(self.i18n.t("toast.uncommitting_head").into());
                effects.push(Effect::UndoLastCommit);
            }
            Action::OngoingResolve => {
                self.active_tab = WorkspaceTab::Commit;
                self.focused_pane = PaneId::CommitChangesTree;
                self.stage_workbench
                    .sync_rows(self.stage_status.as_deref());
                let first_conflict = self.stage_workbench.rows.iter().enumerate().find_map(|(i, r)| {
                    match &r.kind {
                        StageTreeRowKind::File {
                            group_kind: StageNodeKind::Conflicted,
                            path,
                            ..
                        } => Some((i, path.clone())),
                        _ => None,
                    }
                });
                if let Some((pos, path)) = first_conflict {
                    self.commit_stage_selected_idx = pos;
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
            Action::StashSelect(idx) => {
                let len = self.snapshot().map_or(0, |s| s.stashes.len());
                if len > 0 {
                    self.stash.selected = (*idx).min(len - 1);
                    self.stash.details.changes_selected = 0;
                    self.stash.details.diff_scroll = 0;
                }
            }
            Action::StashMove(delta) => {
                let len = self.snapshot().map_or(0, |s| s.stashes.len());
                if len > 0 {
                    let next = (self.stash.selected as isize + delta).clamp(0, len as isize - 1);
                    let _ = self.dispatch_git_action(&Action::StashSelect(next as usize), effects);
                }
            }
            Action::StashChangesSelect(idx) => {
                let len = self.stash.changes_len();
                if len > 0 {
                    self.stash.details.changes_selected = (*idx).min(len - 1);
                    self.stash.details.diff_scroll = 0;
                }
            }
            Action::StashChangesMove(delta) => {
                let len = self.stash.changes_len();
                if len > 0 {
                    let next = (self.stash.details.changes_selected as isize + delta)
                        .clamp(0, len as isize - 1);
                    self.stash.details.changes_selected = next as usize;
                    self.stash.details.diff_scroll = 0;
                }
            }
            Action::StashChangesExpand(expand) => {
                let rows = self.stash.details.change_rows();
                if let Some(row) = rows.get(self.stash.details.changes_selected)
                    && matches!(row.kind, ChangeRowKind::Dir { .. })
                {
                    if *expand {
                        self.stash.details.changes_collapsed.remove(&row.key);
                    } else {
                        self.stash
                            .details
                            .changes_collapsed
                            .insert(row.key.clone());
                    }
                }
            }
            Action::StashChangesToggleGroupByDir => {
                self.stash.details.changes_group_by_dir = !self.stash.details.changes_group_by_dir;
                self.stash.details.changes_selected = 0;
            }
            Action::StashChangesExpandAll(expand) => {
                if *expand {
                    self.stash.details.changes_collapsed.clear();
                } else if let Some(bundle) = self.stash.details.current_bundle() {
                    let all = jgit_core::build_change_rows(
                        &bundle.changes,
                        true,
                        &rustc_hash::FxHashSet::default(),
                    );
                    self.stash.details.changes_collapsed = all
                        .into_iter()
                        .filter(|r| r.depth == 0 && matches!(r.kind, ChangeRowKind::Dir { .. }))
                        .map(|r| r.key)
                        .collect();
                    self.stash.details.changes_selected = 0;
                }
            }
            Action::StashDiffToggleSideBySide => {
                self.stash.details.diff_side_by_side = !self.stash.details.diff_side_by_side;
            }
            Action::StashDiffCycleContext => {
                self.stash.details.diff_context = self.stash.details.diff_context.next();
            }
            Action::StashDiffScroll(delta) => {
                self.stash.details.diff_scroll =
                    (self.stash.details.diff_scroll as isize + delta).max(0) as usize;
            }
            _ => return false,
        }
        true
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
            self.status_notice = Some(format!("Remote branch '{remote_ref}' not found"));
            return;
        };
        let local_name = remote_ref
            .split_once('/')
            .map_or(remote_ref, |(_, b)| b)
            .to_string();
        let op = match snap.local_branch(&local_name) {
            None => GitOp::CheckoutNewBranch {
                name: local_name,
                start: remote_ref.to_string(),
                track: true,
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

    fn compare_with_current(&mut self, branch: &str) {
        let Some(snap) = self.repo_snapshot.clone() else {
            return;
        };
        let current = snap
            .head_branch
            .clone()
            .unwrap_or_else(|| "HEAD".to_string());
        let (Some(a), Some(b)) = (snap.resolve_revision(branch), snap.head_oid) else {
            self.status_notice = Some(format!("Cannot compare '{branch}'"));
            return;
        };
        let only_a = snap.count_exclusive(a, b);
        let only_b = snap.count_exclusive(b, a);
        self.active_tab = WorkspaceTab::Log;
        self.focused_pane = PaneId::LogCommitTable;
        self.log_filter.spec.branch =
            jgit_core::BranchFilter::from_names(vec![branch.to_string(), current.clone()]);
        self.log_graph.filter_generation = self.log_graph.filter_generation.wrapping_add(1);
        self.ensure_graph_cache();
        self.log_graph.jump_to_oid(self.repo_snapshot.as_ref(), &a);
        self.status_notice = Some(format!(
            "Compare '{branch}' ↔ '{current}': {only_a} commit(s) only in '{branch}', {only_b} only in '{current}' (Esc in Branches clears)"
        ));
    }

    fn prompt_delete_branch(&mut self, name: &str, effects: &mut Vec<Effect>) {
        let Some(snap) = self.repo_snapshot.clone() else {
            return;
        };
        let Some(local) = snap.local_branch(name) else {
            self.status_notice = Some(format!("Branch '{name}' not found"));
            return;
        };
        if local.is_head {
            self.status_notice = Some(format!("Cannot delete the current branch '{name}'"));
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
        let i18n = self.i18n.clone();
        let mut body = vec![i18n.t_fmt(prompt_key, &[("name", name), ("count", &count_s)])];
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

    pub(crate) fn default_remote(&self) -> String {
        self.snapshot()
            .and_then(|s| {
                s.refs
                    .iter()
                    .find(|r| r.kind == RefKind::RemoteBranch)
                    .and_then(|r| r.name.split_once('/').map(|(rm, _)| rm.to_string()))
            })
            .unwrap_or_else(|| "origin".to_string())
    }

    fn prompt_push(&mut self, branch: &str, up_to: Option<Oid>) {
        let Some(snap) = self.repo_snapshot.clone() else {
            return;
        };
        let local = snap.local_branch(branch);
        let upstream = local.and_then(|l| l.upstream.clone());
        let remote = upstream
            .as_ref()
            .and_then(|u| u.split_once('/').map(|(r, _)| r.to_string()))
            .unwrap_or_else(|| self.default_remote());
        let set_upstream = upstream.is_none();
        let i18n = self.i18n.clone();
        let mut body = Vec::new();
        match (up_to, local.and_then(|l| l.ahead_behind)) {
            (Some(oid), _) => {
                let oid_s = oid.short();
                body.push(i18n.t_fmt("dialog.push.up_to_body", &[("branch", branch), ("oid", &oid_s)]));
            }
            (None, Some((ahead, behind))) => {
                let ahead_s = ahead.to_string();
                body.push(i18n.t_fmt(
                    "dialog.push.normal_body",
                    &[("ahead", &ahead_s), ("remote", &remote), ("branch", branch)],
                ));
                if behind > 0 {
                    let behind_s = behind.to_string();
                    body.push(i18n.t_fmt("dialog.push.behind_warn", &[("behind", &behind_s)]));
                }
            }
            (None, None) => body.push(i18n.t_fmt(
                "dialog.push.new_branch_body",
                &[("branch", branch), ("remote", &remote)],
            )),
        }
        let op = |force| {
            Action::RunGitOp(GitOp::Push {
                branch: branch.to_string(),
                remote: remote.clone(),
                set_upstream,
                force_with_lease: force,
                up_to,
            })
        };
        self.push_dialog(Dialog::Choice(ChoiceDialog::buttons(
            i18n.t_fmt("dialog.push.title", &[("branch", branch), ("remote", &remote)]),
            body,
            vec![
                ChoiceOption::new(i18n.t("dialog.push.push_btn"), Some('p'), vec![op(false)]),
                ChoiceOption::new(i18n.t("dialog.push.force_push_btn"), Some('f'), vec![op(true)]).danger(),
                ChoiceOption::new(i18n.cancel(), Some('c'), Vec::new()),
            ],
        )));
    }

    /// Reacts to a finished [`GitOp`]: notices, Smart Operation dialog,
    /// merged-branch cleanup, `[Restore Branch]`, clipboard patches.
    pub(crate) fn apply_git_outcome(&mut self, outcome: GitOpOutcome, effects: &mut Vec<Effect>) {
        let op = match &outcome.op {
            GitOp::Smart(inner) => inner.as_ref().clone(),
            other => other.clone(),
        };
        let is_smart = matches!(outcome.op, GitOp::Smart(_));
        if outcome.success {
            self.status_notice = Some(format!("✔ {}", outcome.op.label()));
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
                GitOp::Merge { rev } => {
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
                GitOp::PatchToClipboard { .. } => {
                    let mut text = outcome.stdout.join("\n");
                    text.push('\n');
                    let count_s = outcome.stdout.len().to_string();
                    self.status_notice = Some(self.i18n.t_fmt("toast.copied_patch", &[("count", &count_s)]));
                    effects.push(Effect::CopyToClipboard(text));
                }
                GitOp::CreatePatchFiles { .. } => {
                    let count_s = outcome.stdout.len().to_string();
                    self.status_notice = Some(self.i18n.t_fmt("toast.created_patches", &[("count", &count_s)]));
                }
                GitOp::RewordHead { .. } | GitOp::RewriteHistory { .. } => {
                    self.log_graph.anchor = None;
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
                        })],
                    )),
                });
            }
            return;
        }

        let label = outcome.op.label();
        let i18n = self.i18n.clone();
        match outcome.failure.clone() {
            Some(
                GitFailure::LocalChangesOverwritten { files }
                | GitFailure::UntrackedOverwritten { files },
            ) if op.supports_smart() && !is_smart => {
                let verb = match &op {
                    GitOp::Merge { .. } => "Merge",
                    GitOp::Rebase { .. } | GitOp::Pull { rebase: true } => "Rebase",
                    GitOp::Pull { .. } => "Pull",
                    GitOp::CherryPick { .. } => "Cherry-Pick",
                    GitOp::Revert { .. } => "Revert",
                    GitOp::Reset { .. } => "Reset",
                    _ => "Checkout",
                };
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
                let kept = if outcome.stash_kept {
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
            Some(GitFailure::NotFullyMerged) => {
                if let GitOp::DeleteBranch { name, tip, .. } = &op {
                    self.push_dialog(Dialog::Choice(ChoiceDialog::confirm(
                        i18n.t("dialog.confirm_delete.title_unmerged"),
                        vec![
                            i18n.t_fmt("dialog.confirm_delete.not_merged_prompt", &[("name", name)]),
                            i18n.t("dialog.confirm_delete.not_merged_warn").into(),
                        ],
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
            Some(GitFailure::PushRejected) => {
                if let GitOp::Push {
                    branch,
                    remote,
                    set_upstream,
                    up_to,
                    force_with_lease: false,
                } = &op
                {
                    self.push_dialog(Dialog::Choice(ChoiceDialog::confirm(
                        i18n.t("dialog.push.rejected_title"),
                        vec![
                            i18n.t_fmt("dialog.push.rejected_body", &[("remote", remote), ("branch", branch)]),
                            i18n.t("dialog.push.rejected_hint").into(),
                        ],
                        i18n.t("dialog.push.force_push_btn"),
                        i18n.cancel(),
                        vec![Action::RunGitOp(GitOp::Push {
                            branch: branch.clone(),
                            remote: remote.clone(),
                            set_upstream: *set_upstream,
                            force_with_lease: true,
                            up_to: *up_to,
                        })],
                    )));
                } else {
                    self.notify_error(i18n.t_fmt("toast.push_rejected_notice", &[("label", &label)]));
                }
            }
            Some(
                GitFailure::LocalChangesOverwritten { .. }
                | GitFailure::UntrackedOverwritten { .. },
            ) => {
                self.notify_error(i18n.t_fmt("toast.overwritten_notice", &[("label", &label)]));
            }
            Some(GitFailure::Other(msg)) => {
                self.notify_error(i18n.t_fmt("toast.op_failed_msg", &[("label", &label), ("error", &msg)]));
            }
            None => self.notify_error(i18n.t_fmt("toast.op_failed", &[("label", &label)])),
        }
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
