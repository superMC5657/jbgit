use std::time::Instant;

use jbgit_core::{PaneId, RefreshScope, WorkspaceTab};

use crate::dialogs::{ChoiceDialog, Dialog};
use crate::log_view::{
    AnnotateFocus, AnnotateNavFrame, AnnotateState, DETAILS_DEBOUNCE, FileHistoryState,
    LogDispatchCtx, LogReconcileCtx,
};

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
                    ModalLayer::Annotate(ann) => {
                        if let Some(buf) = ann.goto_line_input.as_mut() {
                            for ch in text.chars().filter(|c| c.is_ascii_digit()) {
                                if buf.len() < 9 {
                                    buf.push(ch);
                                }
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
                if self.modal_stack.annotate().is_none() {
                    self.blame_syntax.invalidate();
                }
                if self.modal_stack.file_history().is_none() {
                    self.range_syntax.invalidate();
                }
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
                    || self.dispatch_annotate_action(&other, &mut effects)
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
        self.reconcile_annotate_requests(now, effects);
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
        if self.modal_stack.file_history().is_none() {
            self.range_syntax.invalidate();
        }
    }

    fn close_annotate(&mut self) {
        self.modal_stack.close_annotate();
        if self.modal_stack.annotate().is_none() {
            self.blame_syntax.invalidate();
        }
    }

    pub(crate) fn jump_file_history_to_log(&mut self, effects: &mut Vec<Effect>) {
        let target_oid = self
            .modal_stack
            .file_history()
            .and_then(|fh| fh.selected_entry())
            .map(|e| e.oid);
        if let Some(oid) = target_oid {
            while self.modal_stack.file_history().is_some()
                || self.modal_stack.annotate().is_some()
            {
                self.close_annotate();
                self.close_file_history();
            }
            self.active_tab = WorkspaceTab::Log;
            effects.extend(self.dispatch(Action::JumpToCommit(oid)));
        }
    }

    pub(crate) fn jump_annotate_to_log(&mut self, effects: &mut Vec<Effect>) {
        let target_oid = self
            .modal_stack
            .annotate()
            .and_then(|ann| ann.selected_line())
            .map(|l| l.oid);
        match target_oid {
            Some(oid) if !oid.is_zero() => {
                while self.modal_stack.file_history().is_some()
                    || self.modal_stack.annotate().is_some()
                {
                    self.close_annotate();
                    self.close_file_history();
                }
                self.active_tab = WorkspaceTab::Log;
                effects.extend(self.dispatch(Action::JumpToCommit(oid)));
            }
            Some(_) => {
                self.status_notice = Some(self.i18n.t("toast.annotate_uncommitted").to_string());
            }
            None => {}
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
                line_range,
            } => {
                let norm = path.replace('\\', "/");
                if norm.is_empty() {
                    return true;
                }
                let normalized_range = line_range.map(|(a, b)| {
                    let lo = a.min(b).max(1);
                    let hi = a.max(b).max(lo);
                    (lo, hi)
                });
                let key = jbgit_core::FileHistoryKey {
                    path: norm,
                    start_oid: *start_oid,
                    follow_renames: if normalized_range.is_some() {
                        false
                    } else {
                        *follow_renames
                    },
                    line_range: normalized_range,
                };
                let mut fh = FileHistoryState::new(key.clone(), self.log.details.diff.context);
                fh.diff.side_by_side = self.log.details.diff.side_by_side;
                self.blur_active_pane(false);
                self.modal_stack.dismiss_context_menus();
                self.modal_stack.open_file_history(fh);
                self.range_syntax.invalidate();
                effects.push(Effect::RequestFileHistory(key));
            }
            Action::OpenSelectionHistory {
                path,
                start_oid,
                start_line,
                end_line,
            } => {
                let norm = path.replace('\\', "/");
                if norm.is_empty() {
                    return true;
                }
                let lo = (*start_line).min(*end_line).max(1);
                let hi = (*start_line).max(*end_line).max(lo);
                let span = hi - lo + 1;
                let open_action = Action::OpenFileHistory {
                    path: norm.clone(),
                    start_oid: *start_oid,
                    follow_renames: false,
                    line_range: Some((lo, hi)),
                };
                if span > jbgit_core::MAX_SELECTION_HISTORY_LINES {
                    let lo_s = lo.to_string();
                    let hi_s = hi.to_string();
                    let span_s = span.to_string();
                    let body = self
                        .i18n
                        .t_fmt(
                            "dialog.selection_history_large.body",
                            &[
                                ("path", &norm),
                                ("start", &lo_s),
                                ("end", &hi_s),
                                ("lines", &span_s),
                            ],
                        )
                        .split('\n')
                        .map(str::to_string)
                        .collect();
                    let dlg = ChoiceDialog::confirm_with_default(
                        self.i18n.t("dialog.selection_history_large.title"),
                        body,
                        self.i18n.t("dialog.selection_history_large.confirm_btn"),
                        self.i18n.cancel(),
                        false,
                        true,
                        vec![open_action],
                    );
                    effects.extend(self.dispatch(Action::OpenDialog(Box::new(Dialog::Choice(dlg)))));
                } else {
                    effects.extend(self.dispatch(open_action));
                }
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
                if let Some(fh) = self.modal_stack.file_history_mut()
                    && let Some(key) = fh.toggle_follow()
                {
                    effects.push(Effect::RequestFileHistory(key));
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
            Action::FileHistoryAnnotate => {
                if let Some(fh) = self.modal_stack.file_history() {
                    let (path, rev) = match fh.selected_entry() {
                        Some(entry) => (entry.path_at_commit.clone(), Some(entry.oid)),
                        None => (fh.key.path.clone(), fh.key.start_oid),
                    };
                    effects.extend(self.dispatch(Action::OpenAnnotate { path, rev }));
                }
            }
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

    fn dispatch_annotate_action(&mut self, action: &Action, effects: &mut Vec<Effect>) -> bool {
        match action {
            Action::OpenAnnotate { path, rev } => {
                let norm = path.replace('\\', "/");
                if norm.is_empty() {
                    return true;
                }
                let key = jbgit_core::BlameKey {
                    path: norm,
                    rev: *rev,
                    options: jbgit_core::BlameOptions {
                        ignore_whitespace: self.config.jbgit.annotate.ignore_whitespace,
                        detect_moves: self.config.jbgit.annotate.detect_moves,
                    },
                };
                let ann = AnnotateState::new(
                    key.clone(),
                    self.log.details.diff.context,
                    self.log.details.diff.side_by_side,
                );
                self.blur_active_pane(false);
                self.modal_stack.dismiss_context_menus();
                self.modal_stack.open_annotate(ann);
                self.blame_syntax.invalidate();
                effects.push(Effect::RequestBlame(key));
            }
            Action::AnnotateMove(delta) => {
                if let Some(ann) = self.modal_stack.annotate_mut() {
                    ann.move_cursor(*delta, false);
                }
            }
            Action::AnnotateMoveExtend(delta) => {
                if let Some(ann) = self.modal_stack.annotate_mut() {
                    ann.move_cursor(*delta, true);
                }
            }
            Action::AnnotateSelectLine { line_idx, extend } => {
                if let Some(ann) = self.modal_stack.annotate_mut() {
                    ann.select_line(*line_idx, *extend);
                }
            }
            Action::AnnotateScroll(delta) => {
                self.scroll_pane(ScrollPane::Annotate, *delta);
            }
            Action::AnnotateToggleVisual => {
                if let Some(ann) = self.modal_stack.annotate_mut() {
                    ann.toggle_visual();
                }
            }
            Action::AnnotateToggleWhitespace => {
                if let Some(ann) = self.modal_stack.annotate_mut() {
                    ann.key.options.ignore_whitespace = !ann.key.options.ignore_whitespace;
                    ann.pending_focus_line = ann.selected_line().map(|l| l.final_line);
                    ann.loading = true;
                    ann.error = None;
                    ann.syntax = None;
                    ann.diff.requested = None;
                    ann.diff.pending = None;
                    let key = ann.key.clone();
                    self.blame_syntax.invalidate();
                    effects.push(Effect::RequestBlame(key));
                }
            }
            Action::AnnotateToggleMoves => {
                if let Some(ann) = self.modal_stack.annotate_mut() {
                    ann.key.options.detect_moves = !ann.key.options.detect_moves;
                    ann.pending_focus_line = ann.selected_line().map(|l| l.final_line);
                    ann.loading = true;
                    ann.error = None;
                    ann.syntax = None;
                    ann.diff.requested = None;
                    ann.diff.pending = None;
                    let key = ann.key.clone();
                    self.blame_syntax.invalidate();
                    effects.push(Effect::RequestBlame(key));
                }
            }
            Action::AnnotateToggleDiff => {
                if let Some(ann) = self.modal_stack.annotate_mut() {
                    ann.show_diff = !ann.show_diff;
                    if !ann.show_diff {
                        ann.focus = AnnotateFocus::Code;
                        ann.diff.pending = None;
                    }
                    ann.diff.scroll.reset();
                }
            }
            Action::AnnotateToggleFocus => {
                if let Some(ann) = self.modal_stack.annotate_mut()
                    && ann.show_diff
                {
                    ann.focus = match ann.focus {
                        AnnotateFocus::Code => AnnotateFocus::Diff,
                        AnnotateFocus::Diff => AnnotateFocus::Code,
                    };
                }
            }
            Action::AnnotateSetFocus(focus) => {
                if let Some(ann) = self.modal_stack.annotate_mut() {
                    ann.focus = if ann.show_diff {
                        *focus
                    } else {
                        AnnotateFocus::Code
                    };
                }
            }
            Action::DiffJumpHunk(DiffScope::Annotate, delta) => {
                let head_oid = self.repo_snapshot.as_ref().and_then(|s| s.head_oid);
                if let Some(ann) = self.modal_stack.annotate_mut() {
                    let diff = ann
                        .desired_diff_key(head_oid)
                        .and_then(|k| self.diff_caches.peek_diff(&k));
                    ann.diff.jump_hunk(*delta, diff);
                }
            }
            Action::DiffToggleSideBySide(DiffScope::Annotate) => {
                if let Some(ann) = self.modal_stack.annotate_mut() {
                    ann.diff.toggle_side_by_side();
                }
            }
            Action::DiffCycleContext(DiffScope::Annotate) => {
                if let Some(ann) = self.modal_stack.annotate_mut() {
                    ann.diff.cycle_context();
                }
            }
            Action::DiffScroll(DiffScope::Annotate, delta) => {
                self.scroll_pane(ScrollPane::AnnotateDiff, *delta);
            }
            Action::AnnotatePreviousRev => {
                let head_oid = self.repo_snapshot.as_ref().and_then(|s| s.head_oid);
                let target = if let Some(ann) = self.modal_stack.annotate() {
                    ann.selected_line().and_then(|line| {
                        if line.oid.is_zero() {
                            head_oid.filter(|_| ann.key.rev.is_none()).map(|h| {
                                let p = if line.orig_path.is_empty() {
                                    ann.key.path.clone()
                                } else {
                                    line.orig_path.clone()
                                };
                                (h, p, line.final_line)
                            })
                        } else {
                            ann.selected_commit()
                                .and_then(|c| c.previous.as_ref())
                                .map(|(oid, prev_path)| {
                                    let p = if prev_path.is_empty() {
                                        ann.key.path.clone()
                                    } else {
                                        prev_path.clone()
                                    };
                                    (*oid, p, line.orig_line.max(1))
                                })
                        }
                    })
                } else {
                    None
                };
                if let Some((prev_oid, prev_path, focus_line)) = target {
                    if let Some(ann) = self.modal_stack.annotate_mut() {
                        let nav_frame = AnnotateNavFrame {
                            key: ann.key.clone(),
                            cursor: ann.cursor,
                        };
                        ann.nav_back.push(nav_frame);
                        ann.key = jbgit_core::BlameKey {
                            path: prev_path,
                            rev: Some(prev_oid),
                            options: ann.key.options,
                        };
                        ann.loading = true;
                        ann.error = None;
                        ann.syntax = None;
                        ann.visual_anchor = None;
                        ann.pending_focus_line = Some(focus_line);
                        ann.diff.requested = None;
                        ann.diff.pending = None;
                        ann.diff.scroll.reset();
                        let new_key = ann.key.clone();
                        self.blame_syntax.invalidate();
                        effects.push(Effect::RequestBlame(new_key));
                    }
                } else if self.modal_stack.annotate().is_some() {
                    self.status_notice = Some(self.i18n.t("toast.annotate_no_previous").to_string());
                }
            }
            Action::AnnotateNavBack => {
                if let Some(ann) = self.modal_stack.annotate_mut()
                    && let Some(frame) = ann.nav_back.pop()
                {
                    ann.key = frame.key;
                    ann.cursor = frame.cursor;
                    ann.loading = true;
                    ann.error = None;
                    ann.syntax = None;
                    ann.visual_anchor = None;
                    ann.pending_focus_line = Some(frame.cursor as u32 + 1);
                    ann.diff.requested = None;
                    ann.diff.pending = None;
                    ann.diff.scroll.reset();
                    let key = ann.key.clone();
                    self.blame_syntax.invalidate();
                    effects.push(Effect::RequestBlame(key));
                }
            }
            Action::AnnotateJumpToLog => self.jump_annotate_to_log(effects),
            Action::AnnotateShowSelectionHistory => {
                let target = self.modal_stack.annotate().and_then(|ann| {
                    let (start_line, end_line) = ann.selected_line_range()?;
                    Some((ann.key.path.clone(), ann.key.rev, start_line, end_line))
                });
                if let Some((path, start_oid, start_line, end_line)) = target {
                    effects.extend(self.dispatch(Action::OpenSelectionHistory {
                        path,
                        start_oid,
                        start_line,
                        end_line,
                    }));
                }
            }
            Action::AnnotateCopyHash => {
                if let Some(oid) = self
                    .modal_stack
                    .annotate()
                    .and_then(|ann| ann.selected_line())
                    .map(|l| l.oid)
                {
                    if oid.is_zero() {
                        self.status_notice =
                            Some(self.i18n.t("toast.annotate_uncommitted").to_string());
                    } else {
                        effects.extend(self.dispatch(Action::CopyText(oid.to_string())));
                    }
                }
            }
            Action::AnnotateGotoLineStart => {
                if let Some(ann) = self.modal_stack.annotate_mut() {
                    ann.goto_line_input = Some(String::new());
                }
            }
            Action::AnnotateGotoLineChar(ch) => {
                if ch.is_ascii_digit()
                    && let Some(ann) = self.modal_stack.annotate_mut()
                    && let Some(buf) = ann.goto_line_input.as_mut()
                    && buf.len() < 10
                {
                    buf.push(*ch);
                }
            }
            Action::AnnotateGotoLineBackspace => {
                if let Some(ann) = self.modal_stack.annotate_mut()
                    && let Some(buf) = ann.goto_line_input.as_mut()
                {
                    buf.pop();
                }
            }
            Action::AnnotateGotoLineConfirm => {
                if let Some(ann) = self.modal_stack.annotate_mut()
                    && let Some(buf) = ann.goto_line_input.take()
                    && let Ok(ln) = buf.trim().parse::<u32>()
                    && ln > 0
                {
                    let idx = ann
                        .bundle
                        .as_ref()
                        .and_then(|b| b.lines.iter().position(|l| l.final_line >= ln))
                        .unwrap_or_else(|| ln.saturating_sub(1) as usize);
                    ann.select_line(idx, false);
                }
            }
            Action::AnnotateGotoLineCancel => {
                if let Some(ann) = self.modal_stack.annotate_mut() {
                    ann.goto_line_input = None;
                }
            }
            Action::AnnotateSetPendingG(at) => {
                if let Some(ann) = self.modal_stack.annotate_mut() {
                    ann.pending_g = *at;
                }
            }
            Action::AnnotateEscape => {
                let (has_goto, has_visual) = match self.modal_stack.annotate() {
                    Some(ann) => (
                        ann.goto_line_input.is_some(),
                        ann.visual_anchor.is_some(),
                    ),
                    None => (false, false),
                };
                if has_goto {
                    if let Some(ann) = self.modal_stack.annotate_mut() {
                        ann.goto_line_input = None;
                    }
                } else if has_visual {
                    if let Some(ann) = self.modal_stack.annotate_mut() {
                        ann.clear_visual();
                    }
                } else {
                    self.close_annotate();
                }
            }
            _ => return false,
        }
        true
    }

    /// Schedules debounced diff fetches (0 ms on LRU cache hit) for `ModalLayer::FileHistory`.
    pub fn reconcile_file_history_requests(&mut self, now: Instant, effects: &mut Vec<Effect>) {
        if self
            .modal_stack
            .file_history()
            .is_some_and(crate::log_view::FileHistoryState::uses_range_patch)
        {
            let range_job = if let Some(fh) = self.modal_stack.file_history_mut() {
                fh.diff.pending = None;
                let range = fh.key.line_range;
                let oid = fh.selected_entry().map(|e| e.oid);
                let built = fh.ensure_range_diff();
                match (oid, range, built) {
                    (Some(oid), Some(range), Some(diff)) if fh.current_range_syntax().is_none() => {
                        Some((oid, range, diff))
                    }
                    _ => None,
                }
            } else {
                None
            };
            if let Some((oid, range, diff)) = range_job {
                self.schedule_range_syntax(oid, range, diff, effects);
            }
            return;
        }
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

    /// Schedules debounced diff fetches (0 ms on LRU cache hit) for `ModalLayer::Annotate`.
    pub fn reconcile_annotate_requests(&mut self, now: Instant, effects: &mut Vec<Effect>) {
        let head_oid = self.repo_snapshot.as_ref().and_then(|s| s.head_oid);
        let desired_key = match self.modal_stack.annotate_mut() {
            Some(ann) => match ann.desired_diff_key(head_oid) {
                Some(k) => k,
                None => {
                    ann.diff.pending = None;
                    return;
                }
            },
            None => return,
        };
        if let Some(cached) = self.diff_caches.get_diff(&desired_key) {
            if let Some(ann) = self.modal_stack.annotate_mut() {
                ann.diff.pending = None;
            }
            if self.diff_caches.get_syntax(&desired_key).is_none() {
                self.schedule_file_syntax(cached, effects);
            }
        } else if let Some(ann) = self.modal_stack.annotate_mut()
            && ann.diff.requested.as_ref() != Some(&desired_key)
            && ann.diff.pending.as_ref().map(|(k, _)| k) != Some(&desired_key)
        {
            ann.diff.pending = Some((desired_key, now + DETAILS_DEBOUNCE));
        }
    }
}
