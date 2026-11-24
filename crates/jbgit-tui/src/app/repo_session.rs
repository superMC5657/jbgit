//! Repository switching on the app side (Spec 12 Issues 09–10): switch
//! guards, the repository switcher / Working Trees modal actions, and the
//! worktree operation follow-ups. The actual unbind → bind is performed by
//! the host (see [`crate::session::RepoSession`]) once
//! [`AppState::pending_repo_switch`] is set.

use std::path::{Path, PathBuf};

use jbgit_core::{ConflictChoice, ConflictSegment, GitFailure, GitOp, same_path};

use crate::dialogs::{ChoiceDialog, ChoiceOption, Dialog};
use crate::repo_switcher::{RepoSwitcherState, build_switcher_items};
use crate::worktrees_view::WorktreesState;

use super::{Action, AppState, Effect, ModalLayer, Notification};

impl AppState {
    /// Working directory of the bound repository.
    pub(crate) fn current_workdir(&self) -> PathBuf {
        self.repo_snapshot
            .as_ref()
            .map_or_else(|| self.repo_path_display.clone(), |s| s.workdir.clone())
    }

    pub(crate) fn dispatch_repo_action(
        &mut self,
        action: &Action,
        effects: &mut Vec<Effect>,
    ) -> bool {
        let _ = effects;
        match action {
            Action::OpenRepoSwitcher => {
                self.modal_stack.retain(|l| {
                    !matches!(
                        l,
                        ModalLayer::ContextMenu { .. } | ModalLayer::RepoSwitcher(_)
                    )
                });
                self.blur_active_pane(false);
                let pruned = self.recent_repos.take_pruned();
                if !pruned.is_empty() {
                    self.status_notice = Some(self.i18n.t_fmt(
                        "toast.recent_repos_pruned",
                        &[("names", &pruned.join(", "))],
                    ));
                }
                let items = build_switcher_items(self);
                self.push_modal(ModalLayer::RepoSwitcher(Box::new(RepoSwitcherState {
                    query: String::new(),
                    selected: 0,
                    scroll: crate::app::ScrollViewport::new(),
                    items,
                })));
            }
            Action::RepoSwitcherSetState { query, selected } => {
                if let Some(ModalLayer::RepoSwitcher(rs)) = self.modal_stack.last_mut() {
                    rs.query.clone_from(query);
                    let len = rs.visible().len();
                    rs.selected = (*selected).min(len.saturating_sub(1));
                    if let Some(vh) = rs.scroll.viewport_height() {
                        rs.scroll.ensure_visible(rs.selected, len, vh);
                    }
                }
            }
            Action::ForgetRecentRepo(path) => {
                if self.recent_repos.forget(path) {
                    let name = path.file_name().map_or_else(
                        || path.display().to_string(),
                        |n| n.to_string_lossy().into(),
                    );
                    self.status_notice = Some(
                        self.i18n
                            .t_fmt("toast.recent_repo_forgotten", &[("name", &name)]),
                    );
                }
                let items = build_switcher_items(self);
                if let Some(ModalLayer::RepoSwitcher(rs)) = self.modal_stack.last_mut() {
                    rs.items = items;
                    let len = rs.visible().len();
                    rs.selected = rs.selected.min(len.saturating_sub(1));
                    if let Some(vh) = rs.scroll.viewport_height() {
                        rs.scroll.ensure_visible(rs.selected, len, vh);
                    }
                }
            }
            Action::SwitchRepository(path) => self.request_repo_switch(path, false),
            Action::SwitchRepositoryConfirmed(path) => self.request_repo_switch(path, true),
            Action::OpenWelcome(dir) => {
                self.modal_stack
                    .retain(|l| !matches!(l, ModalLayer::Welcome(_)));
                self.push_modal(ModalLayer::Welcome(
                    crate::repo_lifecycle::WelcomeState::new(dir.clone()),
                ));
            }
            Action::WelcomeSelect(idx) => {
                if let Some(ModalLayer::Welcome(ws)) = self.modal_stack.last_mut() {
                    ws.selected = (*idx).min(crate::repo_lifecycle::WELCOME_ENTRY_COUNT - 1);
                }
            }
            Action::WelcomeInitRepo(dir) => {
                if let Some(ModalLayer::Welcome(ws)) = self.modal_stack.last_mut() {
                    ws.initializing = true;
                    ws.error = None;
                }
                let shown = dir.display().to_string();
                self.status_notice = Some(
                    self.i18n
                        .t_fmt("toast.repo_initializing", &[("path", &shown)]),
                );
                self.pending_init_repo = Some(dir.clone());
            }
            Action::OpenCloneRepo { base_dir } => {
                let base = crate::repo_lifecycle::default_clone_base_dir(self, base_dir.as_deref());
                self.modal_stack.retain(|l| {
                    !matches!(
                        l,
                        ModalLayer::ContextMenu { .. } | ModalLayer::CloneRepo(_)
                    )
                });
                self.blur_active_pane(false);
                self.push_modal(ModalLayer::CloneRepo(Box::new(
                    crate::repo_lifecycle::CloneDialogState::new(base),
                )));
            }
            Action::CloneRepoUpdate(act) => {
                if let Some(ModalLayer::CloneRepo(cd)) = self.modal_stack.last_mut() {
                    cd.apply(act.clone());
                }
            }
            Action::CloneRepoSubmit => {
                let i18n = self.i18n.clone();
                if let Some(ModalLayer::CloneRepo(cd)) = self.modal_stack.last_mut() {
                    if cd.cloning {
                        return true;
                    }
                    match cd.build_request(&i18n) {
                        Ok(req) => {
                            cd.error = None;
                            cd.cloning = true;
                            cd.progress_lines.clear();
                            let dest = req.target_dir.display().to_string();
                            self.status_notice = Some(
                                i18n.t_fmt("toast.repo_cloning", &[("path", &dest)]),
                            );
                            self.pending_clone_repo = Some(req);
                        }
                        Err(msg) => {
                            cd.error = Some(msg);
                        }
                    }
                }
            }
            Action::CloneRepoCancel => {
                for layer in self.modal_stack.iter_mut().rev() {
                    if let ModalLayer::CloneRepo(cd) = layer {
                        if cd.cloning {
                            cd.cloning = false;
                            cd.error = Some(self.i18n.t("toast.repo_clone_cancelled").to_string());
                            self.pending_clone_cancel = true;
                            self.status_notice =
                                Some(self.i18n.t("toast.repo_clone_cancelled").to_string());
                        }
                        break;
                    }
                }
            }
            Action::OpenWorktrees => {
                if !crate::worktrees_view::worktrees_supported(self) {
                    let notice = crate::worktrees_view::unsupported_notice(self);
                    effects.extend(self.dispatch(notice));
                    return true;
                }
                self.modal_stack.retain(|l| {
                    !matches!(l, ModalLayer::ContextMenu { .. } | ModalLayer::Worktrees(_))
                });
                self.blur_active_pane(false);
                let ws = WorktreesState::for_snapshot(self.repo_snapshot.as_deref());
                self.push_modal(ModalLayer::Worktrees(ws));
            }
            Action::WorktreesMove(delta) => {
                crate::worktrees_view::apply_selection(self, None, *delta);
            }
            Action::WorktreesSelect(idx) => {
                crate::worktrees_view::apply_selection(self, Some(*idx), 0);
            }
            Action::WorktreesOpenSelected => {
                for a in crate::worktrees_view::worktree_command(
                    self,
                    crate::worktrees_view::WorktreeCommand::Open,
                ) {
                    effects.extend(self.dispatch(a));
                }
            }
            Action::PromptNewWorktree { at } => {
                if crate::worktrees_view::worktrees_supported(self) {
                    let d = crate::worktrees_view::new_worktree_dialog(self, *at);
                    self.push_modal(ModalLayer::Dialog(Box::new(d)));
                } else {
                    let notice = crate::worktrees_view::unsupported_notice(self);
                    effects.extend(self.dispatch(notice));
                }
            }
            Action::PromptNewWorktreeFromBranch(remote_ref) => {
                if crate::worktrees_view::worktrees_supported(self) {
                    let ctx = crate::worktrees_view::dialog_ctx(self);
                    let short = self
                        .repo_snapshot
                        .as_deref()
                        .and_then(|s| s.split_tracking(remote_ref).map(|(_, b)| b.to_string()))
                        .unwrap_or_else(|| {
                            remote_ref
                                .split_once('/')
                                .map_or(remote_ref.as_str(), |(_, b)| b)
                                .to_string()
                        });
                    let local_exists = self.repo_snapshot.as_deref().is_some_and(|s| {
                        s.refs
                            .iter()
                            .any(|r| r.kind == jbgit_core::RefKind::LocalBranch && r.name == short)
                    });
                    let wt_branch = if local_exists {
                        jbgit_core::WorktreeBranch::Existing(short)
                    } else {
                        jbgit_core::WorktreeBranch::New {
                            name: short,
                            start: remote_ref.clone(),
                        }
                    };
                    let d = ctx.path_dialog(wt_branch, &self.i18n);
                    self.push_modal(ModalLayer::Dialog(Box::new(d)));
                } else {
                    let notice = crate::worktrees_view::unsupported_notice(self);
                    effects.extend(self.dispatch(notice));
                }
            }
            Action::RunWorktreeSetup { worktree, command } => {
                let name = worktree.file_name().map_or_else(
                    || worktree.display().to_string(),
                    |n| n.to_string_lossy().into(),
                );
                self.status_notice = Some(self.i18n.t_fmt(
                    "toast.worktree_setup_running",
                    &[("name", &name), ("cmd", command)],
                ));
                self.pending_worktree_setup = Some((worktree.clone(), command.clone()));
            }
            _ => return false,
        }
        true
    }

    /// Reasons a switch needs confirmation (empty = switch right away).
    fn switch_blockers(&self) -> Vec<String> {
        let i18n = &self.i18n;
        let mut out = Vec::new();
        if self.git_ops_running > 0 {
            out.push(i18n.t("dialog.repo_switch.op_running").to_string());
        }
        if self.stage_workbench.commit_in_progress() {
            out.push(i18n.t("dialog.repo_switch.commit_running").to_string());
        }
        let open_flow = self.modal_stack.iter().any(|l| match l {
            ModalLayer::Dialog(d) => matches!(
                d.as_ref(),
                Dialog::RebaseTodo(_)
                    | Dialog::Merge(_)
                    | Dialog::Pull(_)
                    | Dialog::Rebase(_)
                    | Dialog::CreatePatch(_)
                    | Dialog::Message(_)
            ),
            ModalLayer::PushDialog(_) => true,
            ModalLayer::CloneRepo(cd) => cd.cloning,
            ModalLayer::Welcome(ws) => ws.initializing,
            _ => false,
        });
        if open_flow {
            out.push(i18n.t("dialog.repo_switch.flow_open").to_string());
        }
        out
    }

    /// Unsaved choices in the built-in conflict resolver.
    fn has_unsaved_conflict_edits(&self) -> bool {
        self.modal_stack.iter().any(|l| match l {
            ModalLayer::ConflictResolver(c) => {
                c.inline_editor.is_some()
                    || c.doc.as_ref().is_some_and(|doc| {
                        doc.segments.iter().any(|s| {
                            matches!(s, ConflictSegment::Conflict(b) if b.choice != ConflictChoice::Unresolved)
                        })
                    })
            }
            _ => false,
        })
    }

    /// Validates and (after the guards) schedules a switch to `path`.
    pub(crate) fn request_repo_switch(&mut self, path: &Path, confirmed: bool) {
        let i18n = self.i18n.clone();
        let current = self.current_workdir();
        let path = crate::dialogs::resolve_path(
            current.parent().unwrap_or(Path::new("")),
            &path.to_string_lossy(),
        );
        let shown = path.display().to_string();
        if !path.is_dir() {
            self.recent_repos.forget(&path);
            self.notify_repo_error(i18n.t_fmt("toast.repo_path_missing", &[("path", &shown)]));
            return;
        }
        if self.repo_snapshot.is_some() && same_path(&path, &current) {
            self.status_notice = Some(i18n.t_fmt("toast.repo_already_open", &[("path", &shown)]));
            return;
        }
        if self.has_unsaved_conflict_edits() {
            self.notify_repo_error(i18n.t("toast.repo_switch_conflict_unsaved").to_string());
            return;
        }
        if !confirmed {
            let reasons = self.switch_blockers();
            if !reasons.is_empty() {
                let mut body = vec![i18n.t_fmt("dialog.repo_switch.body", &[("path", &shown)])];
                body.extend(reasons.into_iter().map(|r| format!("  • {r}")));
                body.push(i18n.t("dialog.repo_switch.wait_note").to_string());
                self.push_modal(ModalLayer::Dialog(Box::new(Dialog::Choice(
                    ChoiceDialog::confirm_with_default(
                        i18n.t("dialog.repo_switch.title"),
                        body,
                        i18n.t("dialog.repo_switch.confirm_btn"),
                        i18n.cancel(),
                        false,
                        false,
                        vec![Action::SwitchRepositoryConfirmed(path)],
                    ),
                ))));
                return;
            }
        }
        self.status_notice = Some(i18n.t_fmt("toast.repo_switching", &[("path", &shown)]));
        self.pending_repo_switch = Some(path);
    }

    /// Called once the session is bound to this state's repository: records
    /// it in the recent list and, after a switch (`Some(writes_finished)`),
    /// announces it (warning when the previous repository's operations
    /// outlived the unbind timeout).
    pub(crate) fn on_repository_bound(&mut self, switched: Option<bool>) {
        if self.repo_error.is_some() {
            return;
        }
        let path = self.repo_path_display.clone();
        let name = path.file_name().map_or_else(
            || path.display().to_string(),
            |n| n.to_string_lossy().into(),
        );
        self.recent_repos.record(&path, &name);
        let Some(writes_finished) = switched else {
            return;
        };
        let mut text = self.i18n.t_fmt("toast.repo_switched", &[("name", &name)]);
        if !writes_finished {
            text.push_str(" · ");
            text.push_str(self.i18n.t("toast.repo_switch_ops_detached"));
        }
        self.status_notice = Some(text.clone());
        self.notification = Some(Notification {
            text,
            is_error: !writes_finished,
            action: None,
        });
    }

    /// The host could not bind `target` (not a repository / engine failure);
    /// the current repository stays bound.
    pub(crate) fn report_repo_switch_failure(&mut self, target: &Path, error: &str) {
        self.recent_repos.forget(target);
        let shown = target.display().to_string();
        self.notify_repo_error(self.i18n.t_fmt(
            "toast.repo_switch_failed",
            &[("path", &shown), ("error", error)],
        ));
    }

    fn notify_repo_error(&mut self, text: String) {
        self.status_notice = Some(text.clone());
        self.notification = Some(Notification {
            text,
            is_error: true,
            action: None,
        });
    }

    /// Follow-up of a successful worktree operation.
    pub(crate) fn on_worktree_op_succeeded(&mut self, op: &GitOp) {
        let i18n = self.i18n.clone();
        match op {
            GitOp::AddWorktree { path, .. } => {
                let name = path.file_name().map_or_else(
                    || path.display().to_string(),
                    |n| n.to_string_lossy().into(),
                );
                let text = i18n.t_fmt("toast.worktree_created", &[("name", &name)]);
                self.status_notice = Some(text.clone());
                self.notification = Some(Notification {
                    text,
                    is_error: false,
                    action: Some((
                        i18n.t_fmt("toast.open_worktree_btn", &[("name", &name)]),
                        vec![Action::SwitchRepository(path.clone())],
                    )),
                });
                if let Some(cmd) = self
                    .config
                    .jbgit
                    .worktree
                    .setup_command
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                {
                    let cmd_s = cmd.to_string();
                    let path_s = path.display().to_string();
                    self.push_modal(ModalLayer::Dialog(Box::new(Dialog::Choice(
                        ChoiceDialog::confirm_with_default(
                            i18n.t("dialog.worktree_setup.title"),
                            vec![
                                i18n.t_fmt(
                                    "dialog.worktree_setup.body",
                                    &[("path", &path_s), ("cmd", &cmd_s)],
                                ),
                                i18n.t("dialog.worktree_setup.hint").to_string(),
                            ],
                            i18n.t("dialog.worktree_setup.run_btn"),
                            i18n.cancel(),
                            false,
                            false,
                            vec![Action::RunWorktreeSetup {
                                worktree: path.clone(),
                                command: cmd_s,
                            }],
                        ),
                    ))));
                }
            }
            GitOp::RemoveWorktree { path, .. } => {
                self.status_notice = Some(i18n.t_fmt(
                    "toast.worktree_removed",
                    &[("path", &path.display().to_string())],
                ));
            }
            GitOp::PruneWorktrees => {
                self.status_notice = Some(i18n.t("toast.worktrees_pruned").to_string());
            }
            _ => {}
        }
    }

    /// Follow-up dialogs of worktree-specific failures; `true` when handled.
    pub(crate) fn on_worktree_failure(&mut self, op: &GitOp, failure: &GitFailure) -> bool {
        let i18n = self.i18n.clone();
        match failure {
            GitFailure::BranchCheckedOutInWorktree { branch, path } => {
                let target = self
                    .repo_snapshot
                    .as_deref()
                    .and_then(|s| s.worktree_of_branch(branch))
                    .map_or_else(|| PathBuf::from(path), |w| w.path.clone());
                self.status_notice = Some(i18n.t_fmt(
                    "toast.branch_in_worktree",
                    &[("branch", branch), ("path", path)],
                ));
                self.push_modal(ModalLayer::Dialog(Box::new(Dialog::Choice(
                    ChoiceDialog::buttons(
                        i18n.t("dialog.branch_in_worktree.title"),
                        vec![
                            i18n.t_fmt(
                                "dialog.branch_in_worktree.body",
                                &[("branch", branch), ("path", path)],
                            ),
                            i18n.t("dialog.branch_in_worktree.hint").to_string(),
                        ],
                        vec![
                            ChoiceOption::new(
                                i18n.t("dialog.branch_in_worktree.open_btn"),
                                Some('o'),
                                vec![Action::SwitchRepository(target)],
                            ),
                            ChoiceOption::new(i18n.t("common.close"), Some('c'), Vec::new()),
                        ],
                    ),
                ))));
                true
            }
            GitFailure::WorktreeDirty => {
                if let GitOp::RemoveWorktree { path, force: false } = op {
                    let d = crate::worktrees_view::force_remove_confirm(path, &i18n);
                    self.push_modal(ModalLayer::Dialog(Box::new(d)));
                    true
                } else {
                    false
                }
            }
            GitFailure::WorktreeLocked => {
                if let GitOp::RemoveWorktree { path, .. } = op {
                    let name = path.file_name().map_or_else(
                        || path.display().to_string(),
                        |n| n.to_string_lossy().into(),
                    );
                    self.notify_repo_error(
                        i18n.t_fmt("toast.worktree_locked_unlock_first", &[("name", &name)]),
                    );
                    true
                } else {
                    false
                }
            }
            _ => false,
        }
    }
}
