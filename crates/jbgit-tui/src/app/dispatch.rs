use std::time::Instant;

use jbgit_core::{PaneId, RefreshScope, WorkspaceTab};

use crate::log_view::{DETAILS_DEBOUNCE, FileHistoryState, LogDispatchCtx, LogReconcileCtx};

use super::{
    Action, AppState, DiffScope, Effect, ModalLayer, PopupKind, ScrollPane, initial_menu_selection,
};

impl AppState {
    pub fn dispatch(&mut self, action: Action) -> Vec<Effect> {
        let mut effects = Vec::new();
        self.needs_redraw = true;
        self.ensure_graph_cache();
        let snap = self.repo_snapshot.clone();
        let width = self.terminal_area.width;

        match action {
            Action::Quit => {
                self.should_quit = true;
                effects.push(Effect::Quit);
            }
            Action::SwitchTab(tab) => {
                self.set_focus(tab, tab.default_pane());
                if tab == WorkspaceTab::Commit {
                    effects.push(Effect::RequestCommitPrecheck);
                }
            }
            Action::CycleFocus { reverse } => {
                let ring = self.effective_focus_ring();
                let next_pane = if let Some(pos) = ring.iter().position(|&p| p == self.focused_pane)
                {
                    let next = if reverse {
                        if pos == 0 { ring.len() - 1 } else { pos - 1 }
                    } else {
                        (pos + 1) % ring.len()
                    };
                    ring[next]
                } else if let Some(&first) = ring.first() {
                    first
                } else {
                    self.focused_pane
                };
                self.set_focus(self.active_tab, next_pane);
            }
            Action::FocusPane(pane) => {
                self.set_focus(self.active_tab, pane);
            }
            Action::ToggleGlyphMode => {
                self.glyph_mode = self.glyph_mode.toggle();
                self.status_notice = Some(
                    self.i18n
                        .t_fmt("toast.glyph_mode", &[("mode", self.glyph_mode.label())]),
                );
            }
            Action::ToggleCompactRefs => {
                return self.dispatch(Action::TogglePresentation(
                    crate::log_view::PresentationToggle::CompactRefs,
                ));
            }
            Action::ToggleTagNames => {
                return self.dispatch(Action::TogglePresentation(
                    crate::log_view::PresentationToggle::ShowTagNames,
                ));
            }
            Action::RefreshAll => {
                self.loading_snapshot = true;
                effects.push(Effect::RefreshRepoAndStage);
            }
            Action::OpenPopup {
                kind,
                initial_query,
            } => {
                self.push_modal(ModalLayer::from_popup_kind(kind, initial_query));
            }
            Action::OpenContextMenu {
                anchor,
                items,
                depth,
            } => {
                if !items.is_empty() {
                    let selected = initial_menu_selection(&items);
                    self.menu_hover = None;
                    self.push_modal(ModalLayer::ContextMenu {
                        anchor,
                        items,
                        selected,
                        depth,
                        parent: None,
                    });
                }
            }
            Action::OpenContextMenuForTarget { target, anchor } => {
                let pre = crate::input::preselect_actions_for_menu_target(&target, self);
                for a in pre {
                    effects.extend(self.dispatch(a));
                }
                if let Some(items) = crate::input::build_menu_items_for_target(&target, self)
                    && !items.is_empty()
                {
                    effects.extend(self.dispatch(Action::OpenContextMenu {
                        anchor,
                        items,
                        depth: 0,
                    }));
                }
            }
            Action::SetPendingKeySequence(seq) => {
                self.pending_keys = seq;
                self.pending_key_at = Some(self.now());
            }
            Action::ClearPendingKeySequence => {
                self.pending_keys.clear();
                self.pending_key_at = None;
            }
            Action::ModalPaste(text) => {
                let Some(top) = self.modal_stack.last_mut() else {
                    return effects;
                };
                match top {
                    ModalLayer::Dialog(d) => match d.as_mut() {
                        crate::dialogs::Dialog::Input(inp) => {
                            let clean: String =
                                text.chars().filter(|c| *c != '\n' && *c != '\r').collect();
                            if !clean.is_empty() {
                                inp.value.push_str(&clean);
                            }
                        }
                        crate::dialogs::Dialog::Message(msg) => {
                            let norm = text.replace("\r\n", "\n").replace('\r', "\n");
                            if !norm.is_empty() {
                                msg.text.push_str(&norm);
                            }
                        }
                        _ => {}
                    },
                    ModalLayer::BranchesPopup { .. }
                    | ModalLayer::ShortcutsHelp { .. }
                    | ModalLayer::FilterEditor { .. }
                    | ModalLayer::GoToRevision { .. }
                    | ModalLayer::CommitHistory { .. } => {
                        let kind = top.popup_kind().expect("popup kind");
                        let (q_ref, sel) = top.popup_query_and_selected().unwrap_or(("", 0));
                        let clean: String =
                            text.chars().filter(|c| *c != '\n' && *c != '\r').collect();
                        if !clean.is_empty() {
                            let mut next_q = q_ref.to_string();
                            next_q.push_str(&clean);
                            let next_sel = match kind {
                                PopupKind::BranchesQuickView => {
                                    crate::branches_popup::first_selectable_row(self, &next_q)
                                }
                                PopupKind::ShortcutsHelp => {
                                    let rows = crate::keymap::build_help_rows(self, &next_q);
                                    crate::keymap::first_help_entry(&rows)
                                }
                                PopupKind::FilterEditor(_)
                                | PopupKind::GoToRevision
                                | PopupKind::CommitHistory => 0,
                                _ => sel,
                            };
                            if let Some(top) = self.modal_stack.last_mut() {
                                top.set_popup_state(next_q, next_sel);
                            }
                        }
                    }
                    _ => {}
                }
            }
            Action::DialogKey(key) => {
                let i18n = self.i18n.clone();
                let outcome = match self.modal_stack.last_mut() {
                    Some(ModalLayer::Dialog(d)) => crate::dialogs::route_dialog_key(d, &key, &i18n),
                    _ => crate::dialogs::DialogKey::None,
                };
                match outcome {
                    crate::dialogs::DialogKey::None | crate::dialogs::DialogKey::Redraw => {}
                    crate::dialogs::DialogKey::Cancel => {
                        effects.extend(self.dispatch(Action::PopModal));
                    }
                    crate::dialogs::DialogKey::Confirm => {
                        effects.extend(self.dispatch(Action::DialogConfirm));
                    }
                    crate::dialogs::DialogKey::Actions(acts) => {
                        for a in acts {
                            effects.extend(self.dispatch(a));
                        }
                    }
                }
            }
            Action::DialogSelectOption { idx, confirm } => {
                if let Some(ModalLayer::Dialog(d)) = self.modal_stack.last_mut() {
                    match d.as_mut() {
                        crate::dialogs::Dialog::Choice(c) => {
                            c.selected = idx.min(c.options.len().saturating_sub(1));
                        }
                        crate::dialogs::Dialog::RebaseTodo(r) => {
                            r.selected = idx.min(r.entries.len().saturating_sub(1));
                        }
                        _ => {}
                    }
                }
                if confirm {
                    effects.extend(self.dispatch(Action::DialogConfirm));
                }
            }
            Action::DialogToggleCheckbox => {
                if let Some(ModalLayer::Dialog(d)) = self.modal_stack.last_mut()
                    && let crate::dialogs::Dialog::Input(i) = d.as_mut()
                    && let Some((_, on)) = i.checkbox.as_mut()
                {
                    *on = !*on;
                }
            }
            Action::PopupSetState { query, selected } => {
                if let Some(top) = self.modal_stack.last_mut() {
                    top.set_popup_state(query, selected);
                }
            }
            Action::ContextMenuSetSelected { depth, idx } => {
                if let Some(ModalLayer::ContextMenu {
                    depth: d,
                    selected,
                    items,
                    ..
                }) = self.modal_stack.last_mut()
                    && *d == depth
                    && idx < items.len()
                {
                    *selected = idx;
                }
            }
            Action::ContextMenuActivateItem { depth, idx } => {
                while matches!(
                    self.modal_stack.last(),
                    Some(ModalLayer::ContextMenu { depth: d, .. }) if *d > depth
                ) {
                    self.modal_stack.pop();
                }
                self.menu_hover = None;
                if let Some(ModalLayer::ContextMenu {
                    selected, items, ..
                }) = self.modal_stack.last_mut()
                    && idx < items.len()
                {
                    *selected = idx;
                }
                let follow = crate::input::activate_top_menu_item(self, idx);
                for a in follow {
                    effects.extend(self.dispatch(a));
                }
            }
            Action::MouseMove { col, row } => {
                self.hover_pos = Some((col, row));
                let next_target = self
                    .hit_registry
                    .hit_test(col, row)
                    .map(|r| r.target.clone());
                let mut changed = false;
                if let Some(crate::hit_test::HitTarget::MenuItem {
                    menu_depth,
                    item_idx,
                }) = &next_target
                {
                    let now = self.now();
                    if self.modal_stack.apply_menu_hover(
                        *menu_depth,
                        *item_idx,
                        now,
                        &mut self.menu_hover,
                    ) {
                        self.needs_redraw = true;
                    }
                    changed = true;
                }
                if let Some(crate::hit_test::HitTarget::BranchPopupRow(idx)) = &next_target
                    && let Some(ModalLayer::BranchesPopup { selected, .. }) =
                        self.modal_stack.last_mut()
                    && *selected != *idx
                {
                    *selected = *idx;
                    changed = true;
                }
                if next_target != self.hover_target {
                    let visual_change = self
                        .hover_target
                        .as_ref()
                        .is_some_and(|t| t.has_visual_hover())
                        || next_target
                            .as_ref()
                            .is_some_and(|t| t.has_visual_hover());
                    self.hover_target = next_target;
                    changed |= visual_change;
                }
                if !changed {
                    self.needs_redraw = false;
                }
            }
            Action::MouseRecordClick { col, row } => {
                self.last_click = Some(super::ClickRecord {
                    at: self.now(),
                    col,
                    row,
                });
            }
            Action::SetDragState(drag) => {
                self.drag_state = drag;
            }
            Action::DragSplitter { col, row } => {
                if let Some(super::DragState::Splitter {
                    splitter_id,
                    axis,
                    container,
                }) = self.drag_state
                {
                    self.splitters
                        .update_from_pointer(splitter_id, axis, container, col, row);
                }
            }
            Action::StageDiffSelectLine { row_idx, extend } => {
                self.stage_workbench.select_diff_line(row_idx, extend);
            }
            Action::StageDiffClearVisualAnchor => {
                self.stage_workbench.clear_visual_anchor();
            }
            Action::StageClearBracketJump => {
                self.stage_workbench.clear_bracket_jump();
            }
            Action::CommitEditorFocusBody => {
                self.stage_workbench.focus_author_done();
                self.focused_pane = PaneId::CommitMessageEditor;
            }
            Action::FileHistorySetPendingG(at) => {
                if let Some(fh) = self.modal_stack.file_history_mut() {
                    fh.pending_g = at;
                }
            }
            Action::PopModal => {
                self.menu_hover = None;
                self.modal_stack.pop();
            }
            Action::PopContextMenus => {
                self.menu_hover = None;
                self.modal_stack.dismiss_context_menus();
            }
            Action::DiscardTargetsConfirmed(targets) => {
                self.stage_workbench.invalidate_paths(&targets);
                effects.push(Effect::DiscardTargets(targets));
            }
            Action::ExecuteCommitConfirmed(req) => {
                self.stage_workbench.begin_commit_confirmed();
                effects.push(Effect::ExecuteCommit(req));
            }
            Action::RetryCliCommand {
                args,
                stdin_payload,
                is_read_only,
                refresh_after,
            } => {
                effects.push(Effect::RunCli {
                    args,
                    stdin_payload,
                    is_read_only,
                    refresh_after,
                });
            }
            Action::ResetSplitter(id) => {
                self.splitters.reset(id);
            }
            Action::FlushPendingDetails => {
                let now = self.now();
                self.flush_pending_at(now, &mut effects);
            }
            Action::RunCliProbe(args) => {
                effects.push(Effect::RunCli {
                    args,
                    stdin_payload: None,
                    is_read_only: true,
                    refresh_after: RefreshScope::None,
                });
            }
            Action::Resize | Action::TickSpinner => {
                if matches!(action, Action::TickSpinner) {
                    self.spinner_frame = self.spinner_frame.wrapping_add(1);
                }
            }
            Action::CopyText(text) => {
                self.status_notice = Some(self.i18n.t_fmt("toast.copied_text", &[("text", &text)]));
                effects.push(Effect::CopyToClipboard(text));
            }
            Action::FilterLogByPath(path) => {
                self.active_tab = WorkspaceTab::Log;
                let now = self.now();
                let mut lctx = LogDispatchCtx {
                    snap: snap.as_ref(),
                    branch_colors: &self.branch_colors,
                    diff_caches: &self.diff_caches,
                    i18n: &self.i18n,
                    now,
                    width,
                    async_graph_enabled: self.async_graph_enabled,
                    focused_pane: &mut self.focused_pane,
                    status_notice: &mut self.status_notice,
                    effects: &mut effects,
                };
                let _ = self.log.dispatch(&Action::FilterLogByPath(path), &mut lctx);
            }
            other => {
                let now = self.now();
                let handled = self.console.dispatch(&other)
                    || self.stash.dispatch(&other, &self.diff_caches, snap.as_deref())
                    || {
                        let mut lctx = LogDispatchCtx {
                            snap: snap.as_ref(),
                            branch_colors: &self.branch_colors,
                            diff_caches: &self.diff_caches,
                            i18n: &self.i18n,
                            now,
                            width,
                            async_graph_enabled: self.async_graph_enabled,
                            focused_pane: &mut self.focused_pane,
                            status_notice: &mut self.status_notice,
                            effects: &mut effects,
                        };
                        self.log.dispatch(&other, &mut lctx)
                    }
                    || self.dispatch_file_history_action(&other, &mut effects)
                    || self.dispatch_git_action(&other, &mut effects);
                if !handled {
                    let default_remote = self.default_remote();
                    let mut modals = Vec::new();
                    let mut cctx = crate::stage_view::CommitContext {
                        snap: snap.as_deref(),
                        default_remote: &default_remote,
                        i18n: &self.i18n,
                        now,
                        focused_pane: &mut self.focused_pane,
                        loading_snapshot: &mut self.loading_snapshot,
                        status_notice: &mut self.status_notice,
                        effects: &mut effects,
                        modals: &mut modals,
                    };
                    let _ = self.stage_workbench.dispatch(&other, &mut cctx);
                    for m in modals {
                        self.push_modal(m);
                    }
                }
            }
        }

        self.reconcile_all_requests(&mut effects);
        effects
    }

    pub(crate) fn reconcile_all_requests(&mut self, effects: &mut Vec<Effect>) {
        let now = self.now();
        self.reconcile_log_requests(now, effects);
        self.reconcile_file_history_requests(now, effects);
        self.stage_workbench.reconcile_requests(
            self.active_tab == WorkspaceTab::Commit,
            now,
            &mut self.stage_syntax,
            self.config.jbgit.diff.syntax_highlight,
            effects,
        );
        self.stash.reconcile_requests(
            self.repo_snapshot.as_ref(),
            &self.diff_caches,
            self.active_tab == WorkspaceTab::Stash,
            effects,
        );
        self.reconcile_stash_syntax(effects);
    }

    pub fn reconcile_log_requests(&mut self, now: Instant, effects: &mut Vec<Effect>) {
        let mut rctx = LogReconcileCtx {
            snap: self.repo_snapshot.as_ref(),
            branch_colors: &self.branch_colors,
            diff_caches: &mut self.diff_caches,
            file_syntax: &mut self.file_syntax,
            syntax_enabled: self.config.jbgit.diff.syntax_highlight,
            i18n: &self.i18n,
            async_graph_enabled: self.async_graph_enabled,
            status_notice: &mut self.status_notice,
            effects,
        };
        self.log.reconcile_requests(now, &mut rctx);
    }

    fn close_file_history(&mut self) {
        self.modal_stack.close_file_history();
    }

    pub(crate) fn jump_file_history_to_log(&mut self, effects: &mut Vec<Effect>) {
        let target_oid = self
            .modal_stack
            .file_history()
            .and_then(|fh| fh.selected_entry())
            .map(|e| e.oid);
        if let Some(oid) = target_oid {
            self.close_file_history();
            self.active_tab = WorkspaceTab::Log;
            effects.extend(self.dispatch(Action::JumpToCommit(oid)));
        }
    }

    fn dispatch_file_history_action(
        &mut self,
        action: &Action,
        effects: &mut Vec<Effect>,
    ) -> bool {
        match action {
            Action::OpenFileHistory {
                path,
                start_oid,
                follow_renames,
            } => {
                let norm = path.replace('\\', "/");
                if norm.is_empty() {
                    return true;
                }
                let key = jbgit_core::FileHistoryKey {
                    path: norm,
                    start_oid: *start_oid,
                    follow_renames: *follow_renames,
                };
                let mut fh = FileHistoryState::new(key.clone(), self.log.details.diff.context);
                fh.diff.side_by_side = self.log.details.diff.side_by_side;
                self.blur_active_pane(false);
                self.modal_stack.dismiss_context_menus();
                self.modal_stack.open_file_history(fh);
                effects.push(Effect::RequestFileHistory(key));
            }
            Action::FileHistoryMove { delta, extend } => {
                if let Some(fh) = self.modal_stack.file_history_mut() {
                    fh.move_selection(*delta, *extend);
                }
            }
            Action::FileHistorySelect { idx, extend } => {
                if let Some(fh) = self.modal_stack.file_history_mut() {
                    fh.select_row(*idx, *extend);
                }
            }
            Action::FileHistoryToggleFollow => {
                if let Some(fh) = self.modal_stack.file_history_mut() {
                    effects.push(Effect::RequestFileHistory(fh.toggle_follow()));
                }
            }
            Action::FileHistoryToggleCompareLocal => {
                if let Some(fh) = self.modal_stack.file_history_mut() {
                    fh.toggle_compare_local();
                }
            }
            Action::FileHistoryToggleZoom => {
                if let Some(fh) = self.modal_stack.file_history_mut() {
                    fh.toggle_zoom();
                }
            }
            Action::FileHistorySetFocus(focus) => {
                if let Some(fh) = self.modal_stack.file_history_mut() {
                    fh.set_focus(*focus);
                }
            }
            Action::FileHistoryToggleFocus => {
                if let Some(fh) = self.modal_stack.file_history_mut() {
                    fh.toggle_focus();
                }
            }
            Action::DiffJumpHunk(DiffScope::FileHistory, delta) => {
                if let Some(fh) = self.modal_stack.file_history_mut() {
                    fh.jump_diff_hunk(*delta, &self.diff_caches);
                }
            }
            Action::DiffToggleSideBySide(DiffScope::FileHistory) => {
                if let Some(fh) = self.modal_stack.file_history_mut() {
                    fh.toggle_side_by_side();
                }
            }
            Action::DiffCycleContext(DiffScope::FileHistory) => {
                if let Some(fh) = self.modal_stack.file_history_mut() {
                    fh.cycle_context();
                }
            }
            Action::DiffScroll(DiffScope::FileHistory, delta) => {
                self.scroll_pane(ScrollPane::FileHistoryDiff, *delta);
            }
            Action::FileHistoryJumpToLog => self.jump_file_history_to_log(effects),
            Action::FileHistoryEscape => {
                if self
                    .modal_stack
                    .file_history()
                    .is_some_and(|fh| fh.diff.zoomed)
                {
                    if let Some(fh) = self.modal_stack.file_history_mut() {
                        fh.toggle_zoom();
                    }
                } else {
                    self.close_file_history();
                }
            }
            _ => return false,
        }
        true
    }

    /// Schedules debounced diff fetches (0 ms on LRU cache hit) for `ModalLayer::FileHistory`.
    pub fn reconcile_file_history_requests(&mut self, now: Instant, effects: &mut Vec<Effect>) {
        let desired_key = match self.modal_stack.file_history_mut() {
            Some(fh) => match fh.desired_diff_key() {
                Some(k) => k,
                None => {
                    fh.diff.pending = None;
                    return;
                }
            },
            None => return,
        };
        if let Some(cached) = self.diff_caches.get_diff(&desired_key) {
            if let Some(fh) = self.modal_stack.file_history_mut() {
                fh.diff.pending = None;
            }
            if self.diff_caches.get_syntax(&desired_key).is_none() {
                self.schedule_file_syntax(cached, effects);
            }
        } else if let Some(fh) = self.modal_stack.file_history_mut()
            && fh.diff.requested.as_ref() != Some(&desired_key)
            && fh.diff.pending.as_ref().map(|(k, _)| k) != Some(&desired_key)
        {
            fh.diff.pending = Some((desired_key, now + DETAILS_DEBOUNCE));
        }
    }
}
