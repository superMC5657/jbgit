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
            Some(k) => Err(format!(
                "Cannot start: a {} is in progress (Continue or Abort it first)",
                k.title().to_lowercase()
            )),
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
                self.status_notice = Some(format!(
                    "Comparing {} ↔ {} (select a commit to return)",
                    base.short(),
                    target.short()
                ));
            }
            Action::CompareWithLocal { target } => {
                self.active_tab = WorkspaceTab::Log;
                self.focused_pane = PaneId::LogChangesTree;
                self.details.diff_preview = true;
                self.details.compare_override = Some(DetailsKey::compare_with_local(*target));
                self.status_notice = Some(format!(
                    "Comparing {} ↔ Working Tree (select a commit to return)",
                    target.short()
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
                        format!("Saved & staged resolved '{path}'")
                    } else {
                        format!(
                            "Saved '{path}' ({} unresolved block(s) kept)",
                            doc.unresolved_count()
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
                self.status_notice = Some("Uncommitting HEAD (changes stay staged)…".into());
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
                    self.status_notice =
                        Some("No conflicted files — press Continue on the banner".into());
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
                    self.push_dialog(Dialog::Choice(ChoiceDialog::buttons(
                        format!("Checkout '{remote_ref}'"),
                        vec![
                            format!("Local branch '{local_name}' already exists and has diverged:"),
                            format!(
                                "  {ahead} local commit(s) not in '{remote_ref}', {behind} remote commit(s) missing locally."
                            ),
                        ],
                        vec![
                            ChoiceOption::new(
                                format!("Checkout local '{local_name}'"),
                                Some('l'),
                                vec![Action::RunGitOp(GitOp::Checkout {
                                    target: local_name.clone(),
                                    detach: false,
                                })],
                            ),
                            ChoiceOption::new(
                                format!("Reset '{local_name}' to '{remote_ref}'"),
                                Some('r'),
                                vec![Action::RunGitOp(GitOp::CheckoutResetBranch {
                                    name: local_name.clone(),
                                    start: remote_ref.to_string(),
                                })],
                            )
                            .detail(format!("drops {ahead} local commit(s)"))
                            .danger(),
                            ChoiceOption::new(
                                "New Branch…",
                                Some('n'),
                                vec![Action::OpenDialog(Box::new(Dialog::Input(
                                    InputDialog::new(
                                        format!("Checkout '{remote_ref}' as New Branch"),
                                        "New local branch name:",
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
                            ChoiceOption::new("Cancel", Some('c'), Vec::new()),
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
        let mut body = vec![format!(
            "Branch '{name}' has {count} commit(s) not merged into HEAD{}.",
            if local.upstream.is_some() {
                " or its upstream"
            } else {
                ""
            }
        )];
        body.push("Those commits are lost unless you restore the branch right away.".into());
        self.push_dialog(Dialog::Choice(ChoiceDialog::confirm(
            "Delete Unmerged Branch",
            body,
            "Force Delete (-D)",
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
        let mut body = Vec::new();
        match (up_to, local.and_then(|l| l.ahead_behind)) {
            (Some(oid), _) => body.push(format!(
                "Push commits of '{branch}' up to {} (later commits stay local).",
                oid.short()
            )),
            (None, Some((ahead, behind))) => {
                body.push(format!(
                    "{ahead} commit(s) will be pushed to '{remote}/{branch}'."
                ));
                if behind > 0 {
                    body.push(format!(
                        "⚠ Remote has {behind} commit(s) you do not have: a normal push will be rejected."
                    ));
                }
            }
            (None, None) => body.push(format!(
                "New branch: '{branch}' will be pushed and tracked as '{remote}/{branch}'."
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
            format!("Push '{branch}' → '{remote}'"),
            body,
            vec![
                ChoiceOption::new("Push", Some('p'), vec![op(false)]),
                ChoiceOption::new("Force Push (with lease)", Some('f'), vec![op(true)]).danger(),
                ChoiceOption::new("Cancel", Some('c'), Vec::new()),
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
                    self.notification = Some(Notification {
                        text: format!("Deleted branch '{name}' ({})", tip.short()),
                        is_error: false,
                        action: Some((
                            "Restore Branch".into(),
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
                        self.push_dialog(Dialog::Choice(ChoiceDialog::buttons(
                            "Merge Succeeded",
                            vec![
                                format!("'{rev}' was merged into '{cur}'."),
                                format!("Delete the merged branch '{rev}'?"),
                            ],
                            vec![
                                ChoiceOption::new(
                                    format!("Delete '{rev}'"),
                                    Some('d'),
                                    vec![Action::RunGitOp(GitOp::DeleteBranch {
                                        name: rev.clone(),
                                        force: false,
                                        tip,
                                    })],
                                ),
                                ChoiceOption::new("Keep", Some('k'), Vec::new()),
                            ],
                        )));
                    }
                }
                GitOp::PatchToClipboard { .. } => {
                    let mut text = outcome.stdout.join("\n");
                    text.push('\n');
                    self.status_notice = Some(format!(
                        "Copied patch ({} lines) to clipboard",
                        outcome.stdout.len()
                    ));
                    effects.push(Effect::CopyToClipboard(text));
                }
                GitOp::CreatePatchFiles { .. } => {
                    self.status_notice = Some(format!(
                        "Created {} patch file(s) in the repository root",
                        outcome.stdout.len()
                    ));
                }
                GitOp::RewordHead { .. } | GitOp::RewriteHistory { .. } => {
                    self.log_graph.anchor = None;
                }
                _ => {}
            }
            if outcome.stash_kept {
                self.notification = Some(Notification {
                    text:
                        "Local changes could not be re-applied cleanly; they are saved in stash@{0}"
                            .into(),
                    is_error: true,
                    action: Some((
                        "Unstash".into(),
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
                let mut body = vec![format!(
                    "{label} would overwrite local changes in {} file(s):",
                    files.len().max(1)
                )];
                body.extend(files.iter().take(6).map(|f| format!("  • {f}")));
                if files.len() > 6 {
                    body.push(format!("  … and {} more", files.len() - 6));
                }
                body.push(String::new());
                body.push(format!(
                    "Smart {verb} stashes them, runs the operation and restores them."
                ));
                let mut options = vec![ChoiceOption::new(
                    format!("Smart {verb}"),
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
                            "Force Checkout (Discard Local)",
                            Some('f'),
                            vec![Action::RunGitOp(GitOp::ForceCheckout { target })],
                        )
                        .danger(),
                    );
                }
                options.push(ChoiceOption::new("Cancel", Some('c'), Vec::new()));
                self.status_notice = Some(format!("{label}: local changes would be overwritten"));
                self.push_dialog(Dialog::Choice(ChoiceDialog::buttons(
                    format!("Smart {verb}"),
                    body,
                    options,
                )));
            }
            Some(GitFailure::Conflicts) => {
                let kept = if outcome.stash_kept {
                    " Local changes are saved in stash@{0}."
                } else {
                    ""
                };
                self.status_notice = Some(format!("{label} stopped on conflicts.{kept}"));
                self.notification = Some(Notification {
                    text: format!("{label}: resolve conflicts, then Continue on the banner.{kept}"),
                    is_error: true,
                    action: Some(("Resolve".into(), vec![Action::OngoingResolve])),
                });
            }
            Some(GitFailure::NotFullyMerged) => {
                if let GitOp::DeleteBranch { name, tip, .. } = &op {
                    self.push_dialog(Dialog::Choice(ChoiceDialog::confirm(
                        "Delete Unmerged Branch",
                        vec![
                            format!("Branch '{name}' is not fully merged."),
                            "Force delete it? You can restore it right after.".into(),
                        ],
                        "Force Delete (-D)",
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
                        "Push Rejected",
                        vec![
                            format!("'{remote}/{branch}' contains commits you do not have."),
                            "Update (pull) first, or force push with lease.".into(),
                        ],
                        "Force Push (with lease)",
                        vec![Action::RunGitOp(GitOp::Push {
                            branch: branch.clone(),
                            remote: remote.clone(),
                            set_upstream: *set_upstream,
                            force_with_lease: true,
                            up_to: *up_to,
                        })],
                    )));
                } else {
                    self.notify_error(format!("{label}: push rejected"));
                }
            }
            Some(
                GitFailure::LocalChangesOverwritten { .. }
                | GitFailure::UntrackedOverwritten { .. },
            ) => {
                self.notify_error(format!("{label}: local changes would be overwritten"));
            }
            Some(GitFailure::Other(msg)) => self.notify_error(format!("{label} failed: {msg}")),
            None => self.notify_error(format!("{label} failed")),
        }
    }

    fn notify_error(&mut self, text: String) {
        self.status_notice = Some(text.clone());
        self.notification = Some(Notification {
            text,
            is_error: true,
            action: Some((
                "Show Console".into(),
                vec![Action::SwitchTab(WorkspaceTab::Console)],
            )),
        });
    }
}
