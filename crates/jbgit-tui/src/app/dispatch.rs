use std::sync::Arc;
use std::time::Instant;

use jbgit_core::{GitOp, PaneId, RefreshScope, RequestSlotId, WorkspaceTab};

use crate::dialogs::{ChoiceDialog, Dialog};
use crate::log_view::{
    AnnotateFocus, AnnotateNavFrame, AnnotateState, DETAILS_DEBOUNCE, FileHistoryState,
    LogDispatchCtx, LogReconcileCtx, PushDialogFocus,
};

use super::{
    Action, AppState, DiffScope, Effect, ModalLayer, PopupKind, ScrollPane, initial_menu_selection,
};

impl AppState {
    fn handle_mouse_move(&mut self, col: u16, row: u16) {
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
            if self
                .modal_stack
                .apply_menu_hover(*menu_depth, *item_idx, now, &mut self.menu_hover)
            {
                changed = true;
            }
        }
        if let Some(crate::hit_test::HitTarget::BranchPopupRow(idx)) = &next_target
            && let Some(ModalLayer::BranchesPopup { selected, .. }) = self.modal_stack.last_mut()
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
                || next_target.as_ref().is_some_and(|t| t.has_visual_hover());
            self.hover_target = next_target;
            changed |= visual_change;
        }
        if changed {
            self.needs_redraw = true;
        }
    }

    pub fn dispatch(&mut self, action: Action) -> Vec<Effect> {
        if let Action::MouseMove { col, row } = action {
            self.handle_mouse_move(col, row);
            return Vec::new();
        }
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
                self.clear_in_flight_requests();
                self.diff_caches.clear_worktree_blames();
                self.invalidate_worktree_diffs(&mut effects);
                effects.push(Effect::RefreshRepoAndStage);
            }
            Action::GoToParentSmart => {
                let candidates = self
                    .log
                    .graph
                    .parent_candidates(self.repo_snapshot.as_ref());
                if candidates.len() >= 2 {
                    self.push_modal(ModalLayer::ChooseParent { selected: 0 });
                } else {
                    effects.extend(self.dispatch(Action::GoToParent(0)));
                }
            }
            Action::GoToChild => {
                let candidates = self.log.graph.child_candidates(self.repo_snapshot.as_ref());
                if candidates.len() >= 2 {
                    self.push_modal(ModalLayer::ChooseChild { selected: 0 });
                } else {
                    let snap = self.repo_snapshot.clone();
                    let now = self.now();
                    let width = self.terminal_area.width;
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
                    let _ = self.log.dispatch(&Action::GoToChild, &mut lctx);
                }
            }
            Action::OpenCommitMessageModal => {
                let single = self
                    .log
                    .details
                    .current_bundle(&self.diff_caches)
                    .and_then(|b| (b.details.len() == 1).then(|| b.details[0].clone()));
                let fallback = self.log_commit_at(self.log.graph.selected_idx).cloned();
                if let Some(d) = single {
                    let subject = d.message.lines().next().unwrap_or("").to_string();
                    self.push_modal(ModalLayer::CommitMessage {
                        oid: d.oid,
                        subject,
                        message: d.message,
                        scroll: super::ScrollViewport::new(),
                    });
                } else if let Some(c) = fallback {
                    let msg = if c.body.is_empty() {
                        c.subject.clone()
                    } else {
                        format!("{}\n\n{}", c.subject, c.body)
                    };
                    self.push_modal(ModalLayer::CommitMessage {
                        oid: c.oid,
                        subject: c.subject,
                        message: msg,
                        scroll: super::ScrollViewport::new(),
                    });
                }
            }
            Action::ScrollCommitMessageModal(delta) => {
                self.scroll_pane(ScrollPane::CommitMessageModal, delta);
            }
            Action::ScrollRebaseDiff(delta) => {
                self.scroll_pane(ScrollPane::RebaseDiff, delta);
            }
            Action::LoadMoreCommits => {
                let has_more = self
                    .repo_snapshot
                    .as_ref()
                    .is_some_and(|s| s.has_more_commits);
                if !has_more {
                    self.status_notice = Some(self.i18n.t("toast.log_no_more_commits").to_string());
                } else if !self.loading_more_commits {
                    self.loading_more_commits = true;
                    self.loading_snapshot = true;
                    effects.push(Effect::LoadMoreCommits);
                }
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
                    ModalLayer::ConflictResolver(cm) => {
                        let tab_size = self.config.jbgit.diff.tab_size;
                        if let Some(ed) = &mut cm.inline_editor {
                            ed.insert_text(&text);
                            ed.ensure_cursor_visible_with_tab(60, 5, tab_size);
                        }
                    }
                    ModalLayer::CloneRepo(cd) => {
                        cd.apply(crate::repo_lifecycle::CloneDialogAction::Paste(text));
                    }
                    ModalLayer::Dialog(d) => match d.as_mut() {
                        crate::dialogs::Dialog::Credential(c) => c.push_str(&text),
                        crate::dialogs::Dialog::Input(inp) => {
                            let clean: String =
                                text.chars().filter(|c| *c != '\n' && *c != '\r').collect();
                            if !clean.is_empty() {
                                if inp.selected_all {
                                    inp.value.clear();
                                    inp.selected_all = false;
                                }
                                inp.value.push_str(&clean);
                            }
                        }
                        crate::dialogs::Dialog::NewBranch(nb)
                            if nb.focused == crate::dialogs::NewBranchField::Name =>
                        {
                            let clean: String =
                                text.chars().filter(|c| *c != '\n' && *c != '\r').collect();
                            if !clean.is_empty() {
                                nb.push_str(&clean);
                            }
                        }
                        crate::dialogs::Dialog::StashPush(sp)
                            if sp.focused == crate::dialogs::StashPushField::Message =>
                        {
                            let clean: String =
                                text.chars().filter(|c| *c != '\n' && *c != '\r').collect();
                            if !clean.is_empty() {
                                if sp.selected_all {
                                    sp.message.clear();
                                    sp.selected_all = false;
                                }
                                sp.message.push_str(&clean);
                            }
                        }
                        crate::dialogs::Dialog::Message(msg) => {
                            let norm = text.replace("\r\n", "\n").replace('\r', "\n");
                            if !norm.is_empty() {
                                msg.text.push_str(&norm);
                            }
                        }
                        crate::dialogs::Dialog::Merge(m) if m.editing_message() => {
                            let norm = text.replace("\r\n", "\n").replace('\r', "\n");
                            if !norm.is_empty() {
                                m.push_message(&norm);
                            }
                        }
                        crate::dialogs::Dialog::Identity(id) => {
                            let clean: String =
                                text.chars().filter(|c| *c != '\n' && *c != '\r').collect();
                            if !clean.is_empty() {
                                match id.focused {
                                    crate::dialogs::IdentityField::Name => {
                                        if id.selected_all {
                                            id.name.clear();
                                            id.selected_all = false;
                                        }
                                        id.name.push_str(&clean);
                                    }
                                    crate::dialogs::IdentityField::Email => {
                                        if id.selected_all {
                                            id.email.clear();
                                            id.selected_all = false;
                                        }
                                        id.email.push_str(&clean);
                                    }
                                    crate::dialogs::IdentityField::Global => {}
                                }
                            }
                        }
                        crate::dialogs::Dialog::Pull(p) if p.editing_branch() => {
                            let clean: String = text
                                .chars()
                                .filter(|c| *c != '\n' && *c != '\r' && !c.is_control())
                                .collect();
                            if !clean.is_empty() {
                                p.push_branch(&clean);
                            }
                        }
                        crate::dialogs::Dialog::Rebase(r) if r.editing_text() => {
                            let clean: String = text
                                .chars()
                                .filter(|c| *c != '\n' && *c != '\r' && !c.is_control())
                                .collect();
                            if !clean.is_empty() {
                                r.push_text(&clean);
                            }
                        }
                        crate::dialogs::Dialog::CreatePatch(cp) if cp.editing_path() => {
                            let clean: String =
                                text.chars().filter(|c| *c != '\n' && *c != '\r').collect();
                            if !clean.is_empty() {
                                cp.push_path(&clean);
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
                    ModalLayer::RepoSwitcher(rs) => {
                        let clean: String = text.chars().filter(|c| !c.is_control()).collect();
                        if !clean.is_empty() {
                            rs.query.push_str(&clean);
                            rs.selected = 0;
                        }
                    }
                    ModalLayer::Annotate(ann) => {
                        if ann.diff.search.editing {
                            let chars: Vec<char> = text
                                .chars()
                                .filter(|c| *c != '\n' && *c != '\r' && !c.is_control())
                                .collect();
                            for ch in chars {
                                effects.extend(
                                    self.dispatch(Action::DiffSearchChar(DiffScope::Annotate, ch)),
                                );
                            }
                        } else if let Some(buf) = ann.goto_line_input.as_mut() {
                            for ch in text.chars().filter(|c| c.is_ascii_digit()) {
                                if buf.len() < 9 {
                                    buf.push(ch);
                                }
                            }
                        }
                    }
                    ModalLayer::FileHistory(fh) if fh.search_editing => {
                        let chars: Vec<char> = text
                            .chars()
                            .filter(|c| *c != '\n' && *c != '\r' && !c.is_control())
                            .collect();
                        for ch in chars {
                            effects.extend(self.dispatch(Action::FileHistorySearchChar(ch)));
                        }
                    }
                    ModalLayer::FileHistory(fh) if fh.diff.search.editing => {
                        let chars: Vec<char> = text
                            .chars()
                            .filter(|c| *c != '\n' && *c != '\r' && !c.is_control())
                            .collect();
                        for ch in chars {
                            effects.extend(
                                self.dispatch(Action::DiffSearchChar(DiffScope::FileHistory, ch)),
                            );
                        }
                    }
                    ModalLayer::Reflog(rf) if rf.diff.search.editing => {
                        let chars: Vec<char> = text
                            .chars()
                            .filter(|c| *c != '\n' && *c != '\r' && !c.is_control())
                            .collect();
                        for ch in chars {
                            effects.extend(
                                self.dispatch(Action::DiffSearchChar(DiffScope::Reflog, ch)),
                            );
                        }
                    }
                    ModalLayer::PushDialog(pd) if pd.diff.search.editing => {
                        let chars: Vec<char> = text
                            .chars()
                            .filter(|c| *c != '\n' && *c != '\r' && !c.is_control())
                            .collect();
                        for ch in chars {
                            effects
                                .extend(self.dispatch(Action::DiffSearchChar(DiffScope::Push, ch)));
                        }
                    }
                    ModalLayer::PushDialog(pd) if pd.editing_target => {
                        let clean: String = text
                            .chars()
                            .filter(|c| *c != '\n' && *c != '\r' && !c.is_control())
                            .collect();
                        if !clean.is_empty() {
                            pd.push_target_text(&clean);
                        }
                    }
                    ModalLayer::TreeBrowser(tb) if tb.search_open => {
                        let chars: Vec<char> = text
                            .chars()
                            .filter(|c| *c != '\n' && *c != '\r' && !c.is_control())
                            .collect();
                        for ch in chars {
                            tb.apply(&crate::tree_browser::TreeBrowserAction::SearchInput(ch));
                        }
                    }
                    ModalLayer::ApplyPatch(ap) if ap.editing_dir.is_some() => {
                        let clean: String = text
                            .chars()
                            .filter(|c| *c != '\n' && *c != '\r' && !c.is_control())
                            .collect();
                        if !clean.is_empty()
                            && let Some(buf) = ap.editing_dir.as_mut()
                        {
                            buf.push_str(&clean);
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
                    crate::dialogs::DialogKey::None | crate::dialogs::DialogKey::Redraw => {
                        self.sync_rebase_todo_preview(&mut effects);
                    }
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
                        self.sync_rebase_todo_preview(&mut effects);
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
                            r.select_entry(idx);
                        }
                        crate::dialogs::Dialog::Merge(m) => {
                            let rows = crate::dialogs::MergeDialogRow::ALL;
                            m.selected = idx.min(rows.len() - 1);
                            m.selected_all = false;
                            let row = m.row();
                            if row != crate::dialogs::MergeDialogRow::Message {
                                m.toggle(row, true);
                            }
                        }
                        crate::dialogs::Dialog::Identity(id) => {
                            id.focused = match idx {
                                0 => crate::dialogs::IdentityField::Name,
                                _ => crate::dialogs::IdentityField::Email,
                            };
                            id.selected_all = false;
                        }
                        crate::dialogs::Dialog::Pull(p) => {
                            let rows = crate::dialogs::PullDialogRow::ALL;
                            p.selected = idx.min(rows.len() - 1);
                            p.selected_all = false;
                            let row = p.row();
                            if row != crate::dialogs::PullDialogRow::Branch {
                                p.toggle(row, true);
                            }
                        }
                        crate::dialogs::Dialog::Rebase(r) => {
                            let rows = crate::dialogs::RebaseDialogRow::ALL;
                            r.selected = idx.min(rows.len() - 1);
                            r.selected_all = false;
                            let row = r.row();
                            if !row.is_text() {
                                r.toggle(row, true);
                            }
                        }
                        crate::dialogs::Dialog::CreatePatch(cp) => {
                            let rows = crate::dialogs::CreatePatchRow::ALL;
                            let target = idx.min(rows.len() - 1);
                            if cp.to_clipboard
                                && rows[target] == crate::dialogs::CreatePatchRow::Path
                            {
                                return effects;
                            }
                            cp.selected = target;
                            cp.selected_all = false;
                            let row = cp.row();
                            if row != crate::dialogs::CreatePatchRow::Path {
                                cp.toggle(row);
                            }
                        }
                        crate::dialogs::Dialog::NewBranch(nb) => {
                            nb.selected_all = false;
                            match idx {
                                0 => {
                                    nb.focused = crate::dialogs::NewBranchField::Checkout;
                                    nb.checkout = !nb.checkout;
                                }
                                1 => {
                                    nb.focused = crate::dialogs::NewBranchField::Overwrite;
                                    nb.overwrite = !nb.overwrite;
                                }
                                2 => {
                                    nb.focused = crate::dialogs::NewBranchField::Track;
                                    nb.track = !nb.track;
                                }
                                3 => {
                                    nb.focused = crate::dialogs::NewBranchField::AutoSanitize;
                                    nb.auto_sanitize = !nb.auto_sanitize;
                                }
                                _ => {}
                            }
                        }
                        crate::dialogs::Dialog::StashPush(sp) => {
                            sp.selected_all = false;
                            match idx {
                                0 => {
                                    sp.focused = crate::dialogs::StashPushField::IncludeUntracked;
                                    sp.include_untracked = !sp.include_untracked;
                                }
                                1 => {
                                    sp.focused = crate::dialogs::StashPushField::KeepIndex;
                                    sp.keep_index = !sp.keep_index;
                                }
                                2 if !sp.selected_paths.is_empty() => {
                                    sp.focused = crate::dialogs::StashPushField::OnlySelected;
                                    sp.only_selected = !sp.only_selected;
                                }
                                _ => {}
                            }
                        }
                        _ => {}
                    }
                }
                self.sync_rebase_todo_preview(&mut effects);
                if confirm {
                    effects.extend(self.dispatch(Action::DialogConfirm));
                }
            }
            Action::DialogToggleCheckbox => {
                if let Some(ModalLayer::Dialog(d)) = self.modal_stack.last_mut() {
                    match d.as_mut() {
                        crate::dialogs::Dialog::Input(i) => {
                            if let Some((_, on)) = i.checkbox.as_mut() {
                                *on = !*on;
                            }
                        }
                        crate::dialogs::Dialog::Choice(c) => {
                            if let Some(cb) = c.checkbox.as_mut() {
                                cb.checked = !cb.checked;
                            }
                        }
                        crate::dialogs::Dialog::Identity(id) => {
                            id.global = !id.global;
                            id.focused = crate::dialogs::IdentityField::Global;
                            id.selected_all = false;
                        }
                        _ => {}
                    }
                }
            }
            Action::DialogToggleSecondaryCheckbox => {
                if let Some(ModalLayer::Dialog(d)) = self.modal_stack.last_mut()
                    && let crate::dialogs::Dialog::Input(i) = d.as_mut()
                    && let Some((_, on)) = i.secondary_checkbox.as_mut()
                {
                    *on = !*on;
                }
            }
            Action::PopupSetState { query, selected } => {
                if let Some(top) = self.modal_stack.last_mut() {
                    top.set_popup_state(query, selected);
                }
            }
            Action::PopupSelectCandidate(selected) => {
                if let Some(top) = self.modal_stack.last_mut() {
                    top.select_popup_candidate(selected);
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
            Action::MouseMove { .. } => {
                unreachable!("fast-pathed before ensure_graph_cache");
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
                // Do not allow backdrop click / PopModal to orphan an in-flight clone
                // or dismiss the only Welcome screen when no repository is bound.
                match self.modal_stack.last() {
                    Some(ModalLayer::CloneRepo(cd)) if cd.cloning => {
                        return effects;
                    }
                    Some(ModalLayer::Welcome(_))
                        if self.repo_snapshot.is_none() && self.modal_stack.len() == 1 =>
                    {
                        return effects;
                    }
                    _ => {}
                }
                if !self.answer_credential(false) {
                    self.modal_stack.pop();
                }
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
                self.stage_workbench
                    .clear_rollback_undo_for_targets(&targets);
                effects.push(Effect::DiscardTargets(targets));
            }
            Action::ExecuteCommitConfirmed(req) => {
                self.stage_workbench.begin_commit_confirmed();
                effects.push(Effect::ExecuteCommit(req));
            }
            Action::CommitCrlfResolve {
                mut request,
                fix_autocrlf,
                disable_warning,
            } => {
                if disable_warning {
                    self.config.jbgit.commit.warn_crlf = false;
                    let workdir = self.current_workdir();
                    if self.repo_snapshot.is_some() && workdir.is_dir() {
                        super::spawn_background_io("jbgit-cfg-writer", move || {
                            let _ = crate::config::AppConfig::persist_repo_commit_warn_crlf(
                                &workdir, false,
                            );
                        });
                    }
                }
                request.check_crlf = false;
                request.fix_autocrlf = fix_autocrlf;
                self.stage_workbench.begin_commit_confirmed();
                effects.push(Effect::ExecuteCommit(request));
            }
            Action::RetryCliCommand {
                args,
                stdin_payload,
                env,
                is_read_only,
                refresh_after,
            } => {
                effects.push(Effect::RunCli {
                    args,
                    stdin_payload,
                    env,
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
                    env: Vec::new(),
                    is_read_only: true,
                    refresh_after: RefreshScope::None,
                });
            }
            Action::Resize | Action::TickSpinner => {
                if matches!(action, Action::TickSpinner) {
                    self.spinner_frame = self.spinner_frame.wrapping_add(1);
                    if !self.credentials.is_empty() {
                        self.sync_credential_dialog();
                    }
                }
            }
            Action::CopyText(text) => {
                self.status_notice = Some(self.i18n.t_fmt("toast.copied_text", &[("text", &text)]));
                effects.push(Effect::CopyToClipboard(text));
            }
            action @ Action::FilterLogByRange { .. } => {
                self.set_focus(WorkspaceTab::Log, PaneId::LogCommitTable);
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
                let _ = self.log.dispatch(&action, &mut lctx);
            }
            Action::FilterLogByPath(path) => {
                self.set_focus(WorkspaceTab::Log, PaneId::LogCommitTable);
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
            Action::ShowDirectoryHistory(path) => {
                self.set_focus(WorkspaceTab::Log, PaneId::LogCommitTable);
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
                let _ = self
                    .log
                    .dispatch(&Action::ShowDirectoryHistory(path), &mut lctx);
            }
            Action::RestorePreDirHistoryFilter => {
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
                let _ = self
                    .log
                    .dispatch(&Action::RestorePreDirHistoryFilter, &mut lctx);
            }
            other => {
                let now = self.now();
                let handled = self.console.dispatch(&other)
                    || self
                        .stash
                        .dispatch(&other, &self.diff_caches, snap.as_deref())
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
                    || self.dispatch_reflog_action(&other, &mut effects)
                    || self.dispatch_annotate_action(&other, &mut effects)
                    || self.dispatch_push_action(&other, &mut effects)
                    || self.dispatch_git_action(&other, &mut effects)
                    || self.dispatch_repo_action(&other, &mut effects)
                    || self.dispatch_browse_action(&other, &mut effects);
                if !handled {
                    let default_remote = self.default_remote();
                    let mut modals = Vec::new();
                    let mut cctx = crate::stage_view::CommitContext {
                        snap: snap.as_deref(),
                        default_remote: &default_remote,
                        warn_crlf: self.config.jbgit.commit.warn_crlf,
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
        self.reconcile_reflog_requests(now, effects);
        self.reconcile_annotate_requests(now, effects);
        self.reconcile_push_dialog_requests(now, effects);
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
            while self.modal_stack.file_history().is_some() || self.modal_stack.annotate().is_some()
            {
                self.close_annotate();
                self.close_file_history();
            }
            self.set_focus(WorkspaceTab::Log, PaneId::LogCommitTable);
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
                self.set_focus(WorkspaceTab::Log, PaneId::LogCommitTable);
                effects.extend(self.dispatch(Action::JumpToCommit(oid)));
            }
            Some(_) => {
                self.status_notice = Some(self.i18n.t("toast.annotate_uncommitted").to_string());
            }
            None => {}
        }
    }

    fn dispatch_file_history_action(&mut self, action: &Action, effects: &mut Vec<Effect>) -> bool {
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
                let key = jbgit_core::FileHistoryKey::new(
                    norm,
                    *start_oid,
                    if normalized_range.is_some() {
                        false
                    } else {
                        *follow_renames
                    },
                    normalized_range,
                );
                let mut fh = FileHistoryState::with_diff_prefs(
                    key.clone(),
                    self.log.details.diff.context,
                    self.log.details.diff.whitespace,
                    self.log.details.diff.soft_wrap,
                );
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
                    effects
                        .extend(self.dispatch(Action::OpenDialog(Box::new(Dialog::Choice(dlg)))));
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
                let all_branches = self.modal_stack.file_history().is_some_and(|fh| {
                    matches!(fh.key.scope, jbgit_core::HistoryBranchScope::AllBranches)
                });
                if all_branches {
                    self.status_notice = Some(
                        self.i18n
                            .t("toast.file_history_follow_disabled_all")
                            .to_string(),
                    );
                } else if let Some(fh) = self.modal_stack.file_history_mut()
                    && let Some(key) = fh.toggle_follow()
                {
                    effects.push(Effect::RequestFileHistory(key));
                }
            }
            Action::FileHistoryCycleBranchScope => {
                if self
                    .modal_stack
                    .file_history()
                    .is_some_and(|fh| !fh.is_selection_history())
                {
                    let anchor = self
                        .hit_registry
                        .find(|t| {
                            matches!(
                                t,
                                crate::hit_test::HitTarget::FileHistoryToolbar(
                                    crate::hit_test::FileHistoryToolbarButton::BranchScope
                                )
                            )
                        })
                        .map(|r| (r.rect.x, r.rect.y.saturating_add(1)))
                        .unwrap_or_else(|| (self.terminal_area.x + 6, self.terminal_area.y + 4));
                    effects.extend(self.dispatch(Action::OpenContextMenuForTarget {
                        target: crate::hit_test::HitTarget::FileHistoryToolbar(
                            crate::hit_test::FileHistoryToolbarButton::BranchScope,
                        ),
                        anchor,
                    }));
                }
            }
            Action::FileHistorySetBranchScope(scope) => {
                if let Some(fh) = self.modal_stack.file_history_mut()
                    && let Some(key) = fh.set_branch_scope(scope.clone())
                {
                    effects.push(Effect::RequestFileHistory(key));
                }
            }
            Action::FileHistoryLoadMore => {
                if let Some(fh) = self.modal_stack.file_history_mut()
                    && let Some(key) = fh.load_more()
                {
                    effects.push(Effect::RequestFileHistory(key));
                }
            }
            Action::FileHistorySearchOpen => {
                if let Some(fh) = self.modal_stack.file_history_mut() {
                    fh.open_search();
                }
            }
            Action::FileHistorySearchChar(ch) => {
                if let Some(fh) = self.modal_stack.file_history_mut() {
                    fh.search_push_char(*ch);
                }
            }
            Action::FileHistorySearchBackspace => {
                if let Some(fh) = self.modal_stack.file_history_mut() {
                    fh.search_backspace();
                }
            }
            Action::FileHistorySearchClear => {
                if let Some(fh) = self.modal_stack.file_history_mut() {
                    fh.search_clear();
                }
            }
            Action::FileHistorySearchConfirm => {
                if let Some(fh) = self.modal_stack.file_history_mut() {
                    fh.search_confirm();
                }
            }
            Action::FileHistorySearchClose => {
                if let Some(fh) = self.modal_stack.file_history_mut() {
                    fh.search_close();
                }
            }
            Action::FileHistoryToggleMarkAnchor => {
                if let Some(fh) = self.modal_stack.file_history_mut() {
                    fh.toggle_mark_anchor();
                }
            }
            Action::FileHistoryToggleCommitDetails => {
                if let Some(fh) = self.modal_stack.file_history_mut() {
                    fh.toggle_commit_details();
                }
            }
            Action::FileHistoryToggleAffectedFiles => {
                if let Some(fh) = self.modal_stack.file_history_mut() {
                    fh.toggle_affected_files();
                }
            }
            Action::FileHistoryAffectedMove(delta) => {
                let len = self
                    .modal_stack
                    .file_history()
                    .and_then(|fh| fh.desired_details_key())
                    .and_then(|k| self.diff_caches.peek_details(&k))
                    .map_or(0, |b| b.changes.len());
                if let Some(fh) = self.modal_stack.file_history_mut() {
                    fh.move_affected(*delta, len);
                }
            }
            Action::FileHistoryAffectedSelect(idx) => {
                let len = self
                    .modal_stack
                    .file_history()
                    .and_then(|fh| fh.desired_details_key())
                    .and_then(|k| self.diff_caches.peek_details(&k))
                    .map_or(0, |b| b.changes.len());
                if let Some(fh) = self.modal_stack.file_history_mut() {
                    fh.select_affected(*idx, len);
                }
            }
            Action::FileHistoryOpenRepoVersion => {
                let target = self.modal_stack.file_history().and_then(|fh| {
                    let e = fh.selected_entry()?;
                    if e.kind == jbgit_core::FileStatusKind::Deleted {
                        return None;
                    }
                    Some(jbgit_core::TreeBlobKey {
                        oid: e.oid,
                        path: e.path_at_commit.clone(),
                    })
                });
                if let Some(key) = target {
                    if let Some(fh) = self.modal_stack.file_history_mut() {
                        fh.show_repo_version = true;
                        fh.repo_version_blob = None;
                        fh.repo_version_syntax = None;
                        fh.repo_version_requested = Some(key.clone());
                        fh.repo_version_scroll.reset();
                    }
                    self.tree_syntax.invalidate();
                    effects.push(Effect::RequestTreeBlob {
                        slot: RequestSlotId::FileHistoryRepoVersion,
                        key,
                    });
                } else if self.modal_stack.file_history().is_some() {
                    self.status_notice =
                        Some(self.i18n.t("menu.log_change.deleted_in_commit").to_string());
                }
            }
            Action::FileHistoryCloseRepoVersion => {
                if let Some(fh) = self.modal_stack.file_history_mut() {
                    fh.close_repo_version();
                }
            }
            Action::FileHistoryBlobScroll(delta) => {
                self.scroll_pane(ScrollPane::FileHistoryBlob, *delta);
            }
            Action::FileHistoryGetVersion => {
                if let Err(msg) = self.require_no_ongoing() {
                    self.status_notice = Some(msg);
                    return true;
                }
                let target = self.modal_stack.file_history().and_then(|fh| {
                    let e = fh.selected_entry()?;
                    if e.kind == jbgit_core::FileStatusKind::Deleted {
                        return None;
                    }
                    Some((e.oid, e.path_at_commit.clone()))
                });
                if let Some((oid, path)) = target {
                    let short = oid.short();
                    let body_lines: Vec<String> = self
                        .i18n
                        .t_fmt(
                            "dialog.restore_file.body",
                            &[("path", &path), ("short", &short)],
                        )
                        .split('\n')
                        .map(str::to_string)
                        .collect();
                    let dlg = ChoiceDialog::confirm(
                        self.i18n.t("dialog.restore_file.title"),
                        body_lines,
                        self.i18n.t("dialog.restore_file.confirm_btn"),
                        self.i18n.cancel(),
                        vec![Action::RunGitOp(GitOp::CheckoutPaths {
                            rev: oid.to_string(),
                            paths: vec![path],
                        })],
                    );
                    effects
                        .extend(self.dispatch(Action::OpenDialog(Box::new(Dialog::Choice(dlg)))));
                } else if self.modal_stack.file_history().is_some() {
                    self.status_notice =
                        Some(self.i18n.t("menu.log_change.deleted_in_commit").to_string());
                }
            }
            Action::FileHistoryCopyPatch => {
                if let Some((oid, path)) = self
                    .modal_stack
                    .file_history()
                    .and_then(|fh| fh.selected_entry())
                    .map(|e| (e.oid, e.path_at_commit.clone()))
                {
                    effects.extend(self.dispatch(Action::RunGitOp(GitOp::PatchToClipboard {
                        source: jbgit_core::PatchSource::CommitFile { oid, path },
                        reverse: false,
                    })));
                }
            }
            Action::FileHistoryCopyHash => {
                if let Some(oid) = self
                    .modal_stack
                    .file_history()
                    .and_then(|fh| fh.selected_entry())
                    .map(|e| e.oid)
                {
                    effects.extend(self.dispatch(Action::CopyText(oid.to_string())));
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
            Action::DiffCycleWhitespace(DiffScope::FileHistory) => {
                if let Some(fh) = self.modal_stack.file_history_mut() {
                    fh.cycle_whitespace();
                }
            }
            Action::DiffToggleSoftWrap(DiffScope::FileHistory) => {
                if let Some(fh) = self.modal_stack.file_history_mut() {
                    fh.toggle_soft_wrap();
                }
            }
            Action::DiffScrollHoriz(DiffScope::FileHistory, delta) => {
                if let Some(fh) = self.modal_stack.file_history_mut() {
                    fh.diff.scroll_horiz(*delta);
                }
            }
            Action::DiffSearchOpen(DiffScope::FileHistory) => {
                if let Some(fh) = self.modal_stack.file_history_mut() {
                    fh.diff.search.open();
                }
            }
            Action::DiffSearchChar(DiffScope::FileHistory, ch) => {
                let tab_size = self.config.jbgit.diff.tab_size;
                if let Some(fh) = self.modal_stack.file_history_mut() {
                    fh.diff.search.push_char(*ch);
                    let diff = fh.current_diff_arc(&self.diff_caches);
                    fh.diff.refresh_search(diff.as_ref(), tab_size);
                }
            }
            Action::DiffSearchBackspace(DiffScope::FileHistory) => {
                let tab_size = self.config.jbgit.diff.tab_size;
                if let Some(fh) = self.modal_stack.file_history_mut() {
                    fh.diff.search.backspace();
                    let diff = fh.current_diff_arc(&self.diff_caches);
                    fh.diff.refresh_search(diff.as_ref(), tab_size);
                }
            }
            Action::DiffSearchClear(DiffScope::FileHistory) => {
                if let Some(fh) = self.modal_stack.file_history_mut() {
                    fh.diff.search.clear_query();
                }
            }
            Action::DiffSearchConfirm(DiffScope::FileHistory) => {
                if let Some(fh) = self.modal_stack.file_history_mut() {
                    fh.diff.search.finish_editing();
                }
            }
            Action::DiffSearchClose(DiffScope::FileHistory) => {
                if let Some(fh) = self.modal_stack.file_history_mut() {
                    fh.diff.search.close();
                }
            }
            Action::DiffSearchJump(DiffScope::FileHistory, delta) => {
                let tab_size = self.config.jbgit.diff.tab_size;
                if let Some(fh) = self.modal_stack.file_history_mut() {
                    let diff = fh.current_diff_arc(&self.diff_caches);
                    fh.diff.jump_search(*delta, diff.as_ref(), tab_size);
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
                let (repo_ver, diff_search, list_search, affected, details, pinned, zoomed) =
                    match self.modal_stack.file_history() {
                        Some(fh) => (
                            fh.show_repo_version,
                            fh.diff.search.active,
                            fh.search_active,
                            fh.show_affected_files,
                            fh.show_commit_details,
                            fh.anchor_pinned,
                            fh.diff.zoomed,
                        ),
                        None => (false, false, false, false, false, false, false),
                    };
                if repo_ver {
                    if let Some(fh) = self.modal_stack.file_history_mut() {
                        fh.close_repo_version();
                    }
                } else if diff_search {
                    if let Some(fh) = self.modal_stack.file_history_mut() {
                        fh.diff.search.close();
                    }
                } else if list_search {
                    if let Some(fh) = self.modal_stack.file_history_mut() {
                        fh.search_close();
                    }
                } else if affected {
                    if let Some(fh) = self.modal_stack.file_history_mut() {
                        fh.toggle_affected_files();
                    }
                } else if details {
                    if let Some(fh) = self.modal_stack.file_history_mut() {
                        fh.toggle_commit_details();
                    }
                } else if pinned {
                    if let Some(fh) = self.modal_stack.file_history_mut() {
                        fh.clear_anchor();
                    }
                } else if zoomed {
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

    fn load_or_request_blame(&mut self, key: jbgit_core::BlameKey, effects: &mut Vec<Effect>) {
        self.blame_syntax.invalidate();
        if let Some(cached) = self.diff_caches.get_blame(&key) {
            if let Some(ann) = self.modal_stack.annotate_mut() {
                ann.apply_loaded_bundle(Arc::clone(&cached));
            }
            self.schedule_blame_syntax(cached, effects);
        } else {
            effects.push(Effect::RequestBlame(key));
        }
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
                        move_detection: self.config.jbgit.annotate.detect_moves.to_core(),
                        prefer_commit_date: self.config.jbgit.annotate.prefer_commit_date,
                    },
                };
                let ann = AnnotateState::with_diff_prefs(
                    key.clone(),
                    self.log.details.diff.context,
                    self.log.details.diff.whitespace,
                    self.log.details.diff.soft_wrap,
                    self.log.details.diff.side_by_side,
                );
                self.blur_active_pane(false);
                self.modal_stack.dismiss_context_menus();
                self.modal_stack.open_annotate(ann);
                self.load_or_request_blame(key, effects);
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
                let key = self.modal_stack.annotate_mut().map(|ann| {
                    ann.key.options.ignore_whitespace = !ann.key.options.ignore_whitespace;
                    ann.pending_focus_line = ann.selected_line().map(|l| l.final_line);
                    ann.loading = true;
                    ann.error = None;
                    ann.syntax = None;
                    ann.diff.requested = None;
                    ann.diff.pending = None;
                    ann.key.clone()
                });
                if let Some(key) = key {
                    self.load_or_request_blame(key, effects);
                }
            }
            Action::AnnotateToggleMoves => {
                let key = self.modal_stack.annotate_mut().map(|ann| {
                    ann.key.options.move_detection = ann.key.options.move_detection.next();
                    ann.pending_focus_line = ann.selected_line().map(|l| l.final_line);
                    ann.loading = true;
                    ann.error = None;
                    ann.syntax = None;
                    ann.diff.requested = None;
                    ann.diff.pending = None;
                    ann.key.clone()
                });
                if let Some(key) = key {
                    self.load_or_request_blame(key, effects);
                }
            }
            Action::AnnotateToggleDateMode => {
                if let Some(ann) = self.modal_stack.annotate_mut() {
                    ann.key.options.prefer_commit_date = !ann.key.options.prefer_commit_date;
                }
            }
            Action::AnnotateCycleColorMode => {
                if let Some(ann) = self.modal_stack.annotate_mut() {
                    ann.color_mode = ann.color_mode.next();
                }
            }
            Action::AnnotateToggleHashColumn => {
                if let Some(ann) = self.modal_stack.annotate_mut() {
                    ann.show_hash = !ann.show_hash;
                }
            }
            Action::AnnotateToggleAuthorColumn => {
                if let Some(ann) = self.modal_stack.annotate_mut() {
                    ann.show_author = !ann.show_author;
                }
            }
            Action::AnnotateToggleDateColumn => {
                if let Some(ann) = self.modal_stack.annotate_mut() {
                    ann.show_date = !ann.show_date;
                }
            }
            Action::AnnotateToggleCommitDetails => {
                if let Some(ann) = self.modal_stack.annotate_mut() {
                    ann.show_commit_details = !ann.show_commit_details;
                    if !ann.show_commit_details {
                        ann.details_requested = None;
                    }
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
                    && !ann.show_commit_details
                {
                    ann.focus = match ann.focus {
                        AnnotateFocus::Code => AnnotateFocus::Diff,
                        AnnotateFocus::Diff => AnnotateFocus::Code,
                    };
                }
            }
            Action::AnnotateSetFocus(focus) => {
                if let Some(ann) = self.modal_stack.annotate_mut() {
                    ann.focus = if ann.show_diff && !ann.show_commit_details {
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
            Action::DiffCycleWhitespace(DiffScope::Annotate) => {
                if let Some(ann) = self.modal_stack.annotate_mut() {
                    ann.diff.cycle_whitespace();
                }
            }
            Action::DiffToggleSoftWrap(DiffScope::Annotate) => {
                if let Some(ann) = self.modal_stack.annotate_mut() {
                    ann.diff.toggle_soft_wrap();
                }
            }
            Action::DiffScrollHoriz(DiffScope::Annotate, delta) => {
                if let Some(ann) = self.modal_stack.annotate_mut() {
                    ann.diff.scroll_horiz(*delta);
                }
            }
            Action::DiffSearchOpen(DiffScope::Annotate) => {
                if let Some(ann) = self.modal_stack.annotate_mut() {
                    ann.diff.search.open();
                }
            }
            Action::DiffSearchChar(DiffScope::Annotate, ch) => {
                let head_oid = self.repo_snapshot.as_ref().and_then(|s| s.head_oid);
                let tab_size = self.config.jbgit.diff.tab_size;
                if let Some(ann) = self.modal_stack.annotate_mut() {
                    ann.diff.search.push_char(*ch);
                    let diff = ann
                        .desired_diff_key(head_oid)
                        .and_then(|k| self.diff_caches.peek_diff(&k).cloned());
                    ann.diff.refresh_search(diff.as_ref(), tab_size);
                }
            }
            Action::DiffSearchBackspace(DiffScope::Annotate) => {
                let head_oid = self.repo_snapshot.as_ref().and_then(|s| s.head_oid);
                let tab_size = self.config.jbgit.diff.tab_size;
                if let Some(ann) = self.modal_stack.annotate_mut() {
                    ann.diff.search.backspace();
                    let diff = ann
                        .desired_diff_key(head_oid)
                        .and_then(|k| self.diff_caches.peek_diff(&k).cloned());
                    ann.diff.refresh_search(diff.as_ref(), tab_size);
                }
            }
            Action::DiffSearchClear(DiffScope::Annotate) => {
                if let Some(ann) = self.modal_stack.annotate_mut() {
                    ann.diff.search.clear_query();
                }
            }
            Action::DiffSearchConfirm(DiffScope::Annotate) => {
                if let Some(ann) = self.modal_stack.annotate_mut() {
                    ann.diff.search.finish_editing();
                }
            }
            Action::DiffSearchClose(DiffScope::Annotate) => {
                if let Some(ann) = self.modal_stack.annotate_mut() {
                    ann.diff.search.close();
                }
            }
            Action::DiffSearchJump(DiffScope::Annotate, delta) => {
                let head_oid = self.repo_snapshot.as_ref().and_then(|s| s.head_oid);
                let tab_size = self.config.jbgit.diff.tab_size;
                if let Some(ann) = self.modal_stack.annotate_mut() {
                    let diff = ann
                        .desired_diff_key(head_oid)
                        .and_then(|k| self.diff_caches.peek_diff(&k).cloned());
                    ann.diff.jump_search(*delta, diff.as_ref(), tab_size);
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
                            ann.selected_previous().map(|(oid, prev_path)| {
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
                    let new_key = self.modal_stack.annotate_mut().map(|ann| {
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
                        ann.details_requested = None;
                        ann.key.clone()
                    });
                    if let Some(new_key) = new_key {
                        self.load_or_request_blame(new_key, effects);
                    }
                } else if self.modal_stack.annotate().is_some() {
                    self.status_notice =
                        Some(self.i18n.t("toast.annotate_no_previous").to_string());
                }
            }
            Action::AnnotateThisRev => {
                let target = self.modal_stack.annotate().and_then(|ann| {
                    let line = ann.selected_line()?;
                    if line.oid.is_zero() {
                        return None;
                    }
                    let path = if line.orig_path.is_empty() {
                        ann.key.path.clone()
                    } else {
                        line.orig_path.clone()
                    };
                    Some((line.oid, path, line.orig_line.max(1)))
                });
                if let Some((rev_oid, rev_path, focus_line)) = target {
                    let new_key = self.modal_stack.annotate_mut().map(|ann| {
                        let nav_frame = AnnotateNavFrame {
                            key: ann.key.clone(),
                            cursor: ann.cursor,
                        };
                        ann.nav_back.push(nav_frame);
                        ann.key = jbgit_core::BlameKey {
                            path: rev_path,
                            rev: Some(rev_oid),
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
                        ann.details_requested = None;
                        ann.key.clone()
                    });
                    if let Some(new_key) = new_key {
                        self.load_or_request_blame(new_key, effects);
                    }
                } else if self.modal_stack.annotate().is_some() {
                    self.status_notice =
                        Some(self.i18n.t("toast.annotate_uncommitted").to_string());
                }
            }
            Action::AnnotateNavBack => {
                let key = self.modal_stack.annotate_mut().and_then(|ann| {
                    let frame = ann.nav_back.pop()?;
                    let current_prefer_commit_date = ann.key.options.prefer_commit_date;
                    ann.key = frame.key;
                    ann.key.options.prefer_commit_date = current_prefer_commit_date;
                    ann.cursor = frame.cursor;
                    ann.loading = true;
                    ann.error = None;
                    ann.syntax = None;
                    ann.visual_anchor = None;
                    ann.pending_focus_line = Some(frame.cursor as u32 + 1);
                    ann.diff.requested = None;
                    ann.diff.pending = None;
                    ann.diff.scroll.reset();
                    ann.details_requested = None;
                    Some(ann.key.clone())
                });
                if let Some(key) = key {
                    self.load_or_request_blame(key, effects);
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
                let (has_search, has_goto, has_details, has_visual) =
                    match self.modal_stack.annotate() {
                        Some(ann) => (
                            ann.diff.search.active,
                            ann.goto_line_input.is_some(),
                            ann.show_commit_details,
                            ann.visual_anchor.is_some(),
                        ),
                        None => (false, false, false, false),
                    };
                if has_search {
                    if let Some(ann) = self.modal_stack.annotate_mut() {
                        ann.diff.search.close();
                    }
                } else if has_goto {
                    if let Some(ann) = self.modal_stack.annotate_mut() {
                        ann.goto_line_input = None;
                    }
                } else if has_details {
                    if let Some(ann) = self.modal_stack.annotate_mut() {
                        ann.show_commit_details = false;
                        ann.details_requested = None;
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

    /// Schedules debounced diff fetches (0 ms on LRU cache hit) and optional
    /// commit-details fetches for `ModalLayer::FileHistory`.
    pub fn reconcile_file_history_requests(&mut self, now: Instant, effects: &mut Vec<Effect>) {
        if let Some(det_key) = self
            .modal_stack
            .file_history()
            .and_then(|fh| fh.desired_details_key())
            && self.diff_caches.peek_details(&det_key).is_none()
            && let Some(fh) = self.modal_stack.file_history_mut()
            && fh.details_requested.as_ref() != Some(&det_key)
        {
            fh.details_requested = Some(det_key.clone());
            effects.push(Effect::RequestCommitDetails {
                slot: RequestSlotId::FileHistoryCommitDetails,
                key: det_key,
            });
        }

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
        let desired_key = self
            .modal_stack
            .file_history()
            .and_then(|fh| fh.desired_diff_key_with_store(&self.diff_caches));
        let cached = self.modal_stack.file_history_mut().and_then(|fh| {
            fh.diff
                .reconcile_fetch(desired_key, &mut self.diff_caches, now, DETAILS_DEBOUNCE)
        });
        if let Some(diff) = cached {
            self.schedule_file_syntax(diff, effects);
        }
    }

    /// Schedules debounced diff fetches (0 ms on LRU cache hit) and optional
    /// commit-details fetches for `ModalLayer::Annotate`.
    pub fn reconcile_annotate_requests(&mut self, now: Instant, effects: &mut Vec<Effect>) {
        let head_oid = self.repo_snapshot.as_ref().and_then(|s| s.head_oid);
        let desired_key = self
            .modal_stack
            .annotate()
            .and_then(|ann| ann.desired_diff_key(head_oid));
        let cached = self.modal_stack.annotate_mut().and_then(|ann| {
            ann.diff
                .reconcile_fetch(desired_key, &mut self.diff_caches, now, DETAILS_DEBOUNCE)
        });
        if let Some(diff) = cached {
            self.schedule_file_syntax(diff, effects);
        }

        if let Some(det_key) = self
            .modal_stack
            .annotate()
            .and_then(|ann| ann.desired_details_key())
            && self.diff_caches.peek_details(&det_key).is_none()
            && let Some(ann) = self.modal_stack.annotate_mut()
            && ann.details_requested.as_ref() != Some(&det_key)
        {
            ann.details_requested = Some(det_key.clone());
            effects.push(Effect::RequestCommitDetails {
                slot: RequestSlotId::AnnotateCommitDetails,
                key: det_key,
            });
        }
    }

    fn dispatch_push_action(&mut self, action: &Action, effects: &mut Vec<Effect>) -> bool {
        let snap = self.repo_snapshot.clone();
        match action {
            Action::PushFocusNext => {
                if let Some(pd) = self.modal_stack.push_dialog_mut() {
                    pd.focus = match pd.focus {
                        PushDialogFocus::Target => PushDialogFocus::Commits,
                        PushDialogFocus::Commits => PushDialogFocus::Files,
                        PushDialogFocus::Files => PushDialogFocus::Target,
                    };
                }
            }
            Action::PushFocusPrev => {
                if let Some(pd) = self.modal_stack.push_dialog_mut() {
                    pd.focus = match pd.focus {
                        PushDialogFocus::Target => PushDialogFocus::Files,
                        PushDialogFocus::Commits => PushDialogFocus::Target,
                        PushDialogFocus::Files => PushDialogFocus::Commits,
                    };
                }
            }
            Action::PushSetFocus(focus) => {
                if let Some(pd) = self.modal_stack.push_dialog_mut() {
                    pd.focus = *focus;
                }
            }
            Action::PushMoveCommit(delta) => {
                if let Some(pd) = self.modal_stack.push_dialog_mut() {
                    let len = pd.visible_commits().len();
                    if len > 0 {
                        let next = (pd.selected_commit as isize + *delta).clamp(0, len as isize - 1)
                            as usize;
                        if next != pd.selected_commit {
                            pd.selected_commit = next;
                            if let Some(vh) = pd.commit_scroll.viewport_height() {
                                pd.commit_scroll.ensure_visible(next, len, vh);
                            }
                            pd.selected_file = 0;
                            pd.file_scroll.reset();
                            pd.show_diff = false;
                            pd.diff.scroll.reset();
                            pd.diff.pending = None;
                            pd.diff.requested = None;
                        }
                    }
                }
            }
            Action::PushSelectCommit(idx) => {
                if let Some(pd) = self.modal_stack.push_dialog_mut() {
                    let len = pd.visible_commits().len();
                    if len > 0 {
                        let next = (*idx).min(len - 1);
                        if next != pd.selected_commit {
                            pd.selected_commit = next;
                            if let Some(vh) = pd.commit_scroll.viewport_height() {
                                pd.commit_scroll.ensure_visible(next, len, vh);
                            }
                            pd.selected_file = 0;
                            pd.file_scroll.reset();
                            pd.show_diff = false;
                            pd.diff.scroll.reset();
                            pd.diff.pending = None;
                            pd.diff.requested = None;
                        }
                    }
                }
            }
            Action::PushMoveFile(delta) => {
                if let Some(pd) = self.modal_stack.push_dialog_mut() {
                    let len = pd.selected_file_changes(&self.diff_caches).len();
                    if len > 0 {
                        let next = (pd.selected_file as isize + *delta).clamp(0, len as isize - 1)
                            as usize;
                        if next != pd.selected_file {
                            pd.selected_file = next;
                            if let Some(vh) = pd.file_scroll.viewport_height() {
                                pd.file_scroll.ensure_visible(next, len, vh);
                            }
                            pd.diff.scroll.reset();
                            pd.diff.pending = None;
                            pd.diff.requested = None;
                        }
                    }
                }
            }
            Action::PushSelectFile(idx) => {
                if let Some(pd) = self.modal_stack.push_dialog_mut() {
                    let len = pd.selected_file_changes(&self.diff_caches).len();
                    if len > 0 {
                        let next = (*idx).min(len - 1);
                        if next != pd.selected_file {
                            pd.selected_file = next;
                            if let Some(vh) = pd.file_scroll.viewport_height() {
                                pd.file_scroll.ensure_visible(next, len, vh);
                            }
                            pd.diff.scroll.reset();
                            pd.diff.pending = None;
                            pd.diff.requested = None;
                        }
                    }
                }
            }
            Action::PushToggleDiff => {
                if let Some(pd) = self.modal_stack.push_dialog_mut() {
                    if pd.show_diff {
                        pd.show_diff = false;
                        pd.diff.pending = None;
                        pd.diff.scroll.reset();
                    } else if !pd.selected_file_changes(&self.diff_caches).is_empty() {
                        pd.show_diff = true;
                        pd.focus = PushDialogFocus::Files;
                        pd.diff.scroll.reset();
                    }
                }
            }
            Action::DiffJumpHunk(DiffScope::Push, delta) => {
                if let Some(pd) = self.modal_stack.push_dialog_mut() {
                    let diff = pd
                        .desired_diff_key(&self.diff_caches)
                        .and_then(|k| self.diff_caches.peek_diff(&k));
                    pd.diff.jump_hunk(*delta, diff);
                }
            }
            Action::DiffToggleSideBySide(DiffScope::Push) => {
                if let Some(pd) = self.modal_stack.push_dialog_mut() {
                    pd.diff.toggle_side_by_side();
                }
            }
            Action::DiffCycleContext(DiffScope::Push) => {
                if let Some(pd) = self.modal_stack.push_dialog_mut() {
                    pd.diff.cycle_context();
                }
            }
            Action::DiffCycleWhitespace(DiffScope::Push) => {
                if let Some(pd) = self.modal_stack.push_dialog_mut() {
                    pd.diff.cycle_whitespace();
                }
            }
            Action::DiffToggleSoftWrap(DiffScope::Push) => {
                if let Some(pd) = self.modal_stack.push_dialog_mut() {
                    pd.diff.toggle_soft_wrap();
                }
            }
            Action::DiffScrollHoriz(DiffScope::Push, delta) => {
                if let Some(pd) = self.modal_stack.push_dialog_mut() {
                    pd.diff.scroll_horiz(*delta);
                }
            }
            Action::DiffSearchOpen(DiffScope::Push) => {
                if let Some(pd) = self.modal_stack.push_dialog_mut() {
                    pd.diff.search.open();
                }
            }
            Action::DiffSearchChar(DiffScope::Push, ch) => {
                let tab_size = self.config.jbgit.diff.tab_size;
                if let Some(pd) = self.modal_stack.push_dialog_mut() {
                    pd.diff.search.push_char(*ch);
                    let diff = pd
                        .desired_diff_key(&self.diff_caches)
                        .and_then(|k| self.diff_caches.peek_diff(&k).cloned());
                    pd.diff.refresh_search(diff.as_ref(), tab_size);
                }
            }
            Action::DiffSearchBackspace(DiffScope::Push) => {
                let tab_size = self.config.jbgit.diff.tab_size;
                if let Some(pd) = self.modal_stack.push_dialog_mut() {
                    pd.diff.search.backspace();
                    let diff = pd
                        .desired_diff_key(&self.diff_caches)
                        .and_then(|k| self.diff_caches.peek_diff(&k).cloned());
                    pd.diff.refresh_search(diff.as_ref(), tab_size);
                }
            }
            Action::DiffSearchClear(DiffScope::Push) => {
                if let Some(pd) = self.modal_stack.push_dialog_mut() {
                    pd.diff.search.clear_query();
                }
            }
            Action::DiffSearchConfirm(DiffScope::Push) => {
                if let Some(pd) = self.modal_stack.push_dialog_mut() {
                    pd.diff.search.finish_editing();
                }
            }
            Action::DiffSearchClose(DiffScope::Push) => {
                if let Some(pd) = self.modal_stack.push_dialog_mut() {
                    pd.diff.search.close();
                }
            }
            Action::DiffSearchJump(DiffScope::Push, delta) => {
                let tab_size = self.config.jbgit.diff.tab_size;
                if let Some(pd) = self.modal_stack.push_dialog_mut() {
                    let diff = pd
                        .desired_diff_key(&self.diff_caches)
                        .and_then(|k| self.diff_caches.peek_diff(&k).cloned());
                    pd.diff.jump_search(*delta, diff.as_ref(), tab_size);
                }
            }
            Action::DiffScroll(DiffScope::Push, delta) => {
                self.scroll_pane(ScrollPane::PushDiff, *delta);
            }
            Action::PushStartEditTarget => {
                if let Some(pd) = self.modal_stack.push_dialog_mut() {
                    pd.start_edit_target();
                }
            }
            Action::PushEditTargetChar(ch) => {
                if !ch.is_control()
                    && let Some(pd) = self.modal_stack.push_dialog_mut()
                    && pd.editing_target
                {
                    pd.push_target_char(*ch);
                }
            }
            Action::PushEditTargetBackspace => {
                if let Some(pd) = self.modal_stack.push_dialog_mut()
                    && pd.editing_target
                {
                    pd.pop_target_char();
                }
            }
            Action::PushEditTargetSelectAll => {
                if let Some(pd) = self.modal_stack.push_dialog_mut()
                    && pd.editing_target
                {
                    pd.select_all_target();
                }
            }
            Action::PushEditTargetClear => {
                if let Some(pd) = self.modal_stack.push_dialog_mut()
                    && pd.editing_target
                {
                    pd.clear_target_draft();
                }
            }
            Action::PushEditTargetMoveCompletion(delta) => {
                if let Some(pd) = self.modal_stack.push_dialog_mut()
                    && pd.editing_target
                {
                    let len = pd.completion_candidates(snap.as_deref()).len();
                    if len > 0 {
                        pd.completion_selected = (pd.completion_selected as isize + *delta)
                            .rem_euclid(len as isize)
                            as usize;
                    }
                }
            }
            Action::PushEditTargetAcceptCompletion(opt_idx) => {
                if let Some(pd) = self.modal_stack.push_dialog_mut()
                    && pd.editing_target
                {
                    let cands = pd.completion_candidates(snap.as_deref());
                    let idx = opt_idx.unwrap_or(pd.completion_selected);
                    if let Some(choice) = cands.get(idx) {
                        pd.set_target_draft(choice.clone());
                    }
                }
            }
            Action::PushEditTargetConfirm => {
                enum EditOutcome {
                    Err(String),
                    Reload(jbgit_core::OutgoingKey),
                    Same,
                }
                let outcome = if let Some(pd) = self.modal_stack.push_dialog_mut()
                    && pd.editing_target
                {
                    if let Some(err) = pd.target_validation_error(&self.i18n) {
                        Some(EditOutcome::Err(err))
                    } else {
                        let next_branch = pd.target_draft.trim().to_string();
                        pd.finish_edit_target();
                        if next_branch != pd.remote_branch {
                            pd.remote_branch = next_branch;
                            pd.expected_remote_oid = self
                                .repo_snapshot
                                .as_deref()
                                .and_then(|s| s.remote_branch(pd.remote(), &pd.remote_branch))
                                .map(|r| r.target_oid);
                            pd.loading = true;
                            pd.error = None;
                            pd.outgoing = None;
                            pd.selected_commit = 0;
                            pd.commit_scroll.reset();
                            pd.selected_file = 0;
                            pd.file_scroll.reset();
                            pd.show_diff = false;
                            pd.details_requested = None;
                            pd.details_pending = None;
                            pd.diff.requested = None;
                            pd.diff.pending = None;
                            Some(EditOutcome::Reload(pd.outgoing_key()))
                        } else {
                            Some(EditOutcome::Same)
                        }
                    }
                } else {
                    None
                };
                match outcome {
                    Some(EditOutcome::Err(err)) => {
                        self.status_notice = Some(err);
                    }
                    Some(EditOutcome::Reload(key)) => {
                        effects.push(Effect::RequestOutgoing(key));
                    }
                    _ => {}
                }
            }
            Action::PushEditTargetCancel => {
                if let Some(pd) = self.modal_stack.push_dialog_mut() {
                    pd.cancel_edit_target();
                }
            }
            Action::PushToggleRemotePicker => {
                if let Some(pd) = self.modal_stack.push_dialog_mut() {
                    if pd.remotes.len() > 1 {
                        pd.picking_remote = !pd.picking_remote;
                        pd.finish_edit_target();
                        pd.focus = PushDialogFocus::Target;
                    } else {
                        pd.picking_remote = false;
                    }
                }
            }
            Action::PushMoveRemotePicker(delta) => {
                let key = if let Some(pd) = self.modal_stack.push_dialog_mut()
                    && pd.picking_remote
                    && pd.remotes.len() > 1
                {
                    let len = pd.remotes.len();
                    let next = (pd.remote_idx as isize + *delta).rem_euclid(len as isize) as usize;
                    if next != pd.remote_idx {
                        let prev_remote = pd.remote().to_string();
                        pd.remote_idx = next;
                        let new_remote = pd.remote().to_string();
                        if let Some(snap) = self.repo_snapshot.as_deref() {
                            let prev_default = snap
                                .remote(&prev_remote)
                                .and_then(|r| {
                                    jbgit_core::resolve_push_refspec_target(
                                        &pd.branch,
                                        &r.push_refspecs,
                                    )
                                })
                                .unwrap_or_else(|| pd.branch.clone());
                            if !pd.is_detached() && pd.remote_branch == prev_default {
                                pd.remote_branch = snap
                                    .remote(&new_remote)
                                    .and_then(|r| {
                                        jbgit_core::resolve_push_refspec_target(
                                            &pd.branch,
                                            &r.push_refspecs,
                                        )
                                    })
                                    .unwrap_or_else(|| pd.branch.clone());
                            }
                            pd.expected_remote_oid = snap
                                .remote_branch(&new_remote, &pd.remote_branch)
                                .map(|r| r.target_oid);
                        }
                        pd.loading = true;
                        pd.error = None;
                        pd.outgoing = None;
                        pd.selected_commit = 0;
                        pd.commit_scroll.reset();
                        pd.selected_file = 0;
                        pd.file_scroll.reset();
                        pd.show_diff = false;
                        pd.details_requested = None;
                        pd.details_pending = None;
                        pd.diff.requested = None;
                        pd.diff.pending = None;
                        Some(pd.outgoing_key())
                    } else {
                        None
                    }
                } else {
                    None
                };
                if let Some(k) = key {
                    effects.push(Effect::RequestOutgoing(k));
                }
            }
            Action::PushSelectRemote(idx) => {
                let key = if let Some(pd) = self.modal_stack.push_dialog_mut() {
                    pd.picking_remote = false;
                    let len = pd.remotes.len();
                    if len > 0 {
                        let next = (*idx).min(len - 1);
                        if next != pd.remote_idx {
                            let prev_remote = pd.remote().to_string();
                            pd.remote_idx = next;
                            let new_remote = pd.remote().to_string();
                            if let Some(snap) = self.repo_snapshot.as_deref() {
                                let prev_default = snap
                                    .remote(&prev_remote)
                                    .and_then(|r| {
                                        jbgit_core::resolve_push_refspec_target(
                                            &pd.branch,
                                            &r.push_refspecs,
                                        )
                                    })
                                    .unwrap_or_else(|| pd.branch.clone());
                                if !pd.is_detached() && pd.remote_branch == prev_default {
                                    pd.remote_branch = snap
                                        .remote(&new_remote)
                                        .and_then(|r| {
                                            jbgit_core::resolve_push_refspec_target(
                                                &pd.branch,
                                                &r.push_refspecs,
                                            )
                                        })
                                        .unwrap_or_else(|| pd.branch.clone());
                                }
                                pd.expected_remote_oid = snap
                                    .remote_branch(&new_remote, &pd.remote_branch)
                                    .map(|r| r.target_oid);
                            }
                            pd.loading = true;
                            pd.error = None;
                            pd.outgoing = None;
                            pd.selected_commit = 0;
                            pd.commit_scroll.reset();
                            pd.selected_file = 0;
                            pd.file_scroll.reset();
                            pd.show_diff = false;
                            pd.details_requested = None;
                            pd.details_pending = None;
                            pd.diff.requested = None;
                            pd.diff.pending = None;
                            Some(pd.outgoing_key())
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                } else {
                    None
                };
                if let Some(k) = key {
                    effects.push(Effect::RequestOutgoing(k));
                }
            }
            Action::PushCycleTags => {
                if let Some(pd) = self.modal_stack.push_dialog_mut() {
                    pd.tags = pd.tags.next();
                }
            }
            Action::PushCycleForce => {
                if let Some(pd) = self.modal_stack.push_dialog_mut() {
                    pd.force = pd.force.next();
                }
            }
            Action::PushToggleSetUpstream => {
                if let Some(pd) = self.modal_stack.push_dialog_mut() {
                    if pd.is_detached() {
                        self.status_notice = Some(
                            self.i18n
                                .t("dialog.push_full.detached_no_upstream")
                                .to_string(),
                        );
                    } else {
                        pd.set_upstream = !pd.set_upstream;
                    }
                }
            }
            Action::PushToggleNoVerify => {
                if let Some(pd) = self.modal_stack.push_dialog_mut() {
                    pd.no_verify = !pd.no_verify;
                }
            }
            Action::PushDefineRemote => {
                let dlg = crate::dialogs::add_remote_dialog(self.snapshot(), &self.i18n);
                self.push_dialog(dlg);
            }
            Action::PushConfirm => {
                let check = self.modal_stack.push_dialog().map(|pd| {
                    pd.can_push(&self.config.jbgit.push.protected_branches, &self.i18n)
                        .map(|()| (pd.visible_commits().len(), pd.build_push_op()))
                });
                match check {
                    Some(Ok((expected_count, op))) => {
                        self.last_push_expected_commits = Some(expected_count);
                        self.modal_stack.close_push_dialog();
                        effects.extend(self.dispatch(Action::RunGitOp(op)));
                    }
                    Some(Err(reason)) => {
                        self.status_notice = Some(reason);
                    }
                    None => {}
                }
            }
            Action::PushEscape => {
                enum EscOutcome {
                    CancelEdit,
                    CloseRemote,
                    CloseDiff,
                    CloseModal,
                }
                let outcome = self.modal_stack.push_dialog().map(|pd| {
                    if pd.editing_target {
                        EscOutcome::CancelEdit
                    } else if pd.picking_remote {
                        EscOutcome::CloseRemote
                    } else if pd.show_diff {
                        EscOutcome::CloseDiff
                    } else {
                        EscOutcome::CloseModal
                    }
                });
                match outcome {
                    Some(EscOutcome::CancelEdit) => {
                        if let Some(pd) = self.modal_stack.push_dialog_mut() {
                            pd.cancel_edit_target();
                        }
                    }
                    Some(EscOutcome::CloseRemote) => {
                        if let Some(pd) = self.modal_stack.push_dialog_mut() {
                            pd.picking_remote = false;
                        }
                    }
                    Some(EscOutcome::CloseDiff) => {
                        if let Some(pd) = self.modal_stack.push_dialog_mut() {
                            pd.show_diff = false;
                            pd.diff.pending = None;
                            pd.diff.scroll.reset();
                        }
                    }
                    Some(EscOutcome::CloseModal) | None => {
                        self.modal_stack.close_push_dialog();
                    }
                }
            }
            _ => return false,
        }
        true
    }

    pub fn reconcile_push_dialog_requests(&mut self, now: Instant, effects: &mut Vec<Effect>) {
        let desired_details = match self.modal_stack.push_dialog_mut() {
            Some(pd) => match pd.desired_details_key() {
                Some(k) => Some(k),
                None => {
                    pd.details_pending = None;
                    None
                }
            },
            None => return,
        };
        if let Some(key) = desired_details {
            if self.diff_caches.details.contains(&key) {
                if let Some(pd) = self.modal_stack.push_dialog_mut() {
                    pd.details_pending = None;
                    let len = pd.selected_file_changes(&self.diff_caches).len();
                    pd.selected_file = pd.selected_file.min(len.saturating_sub(1));
                }
            } else if let Some(pd) = self.modal_stack.push_dialog_mut()
                && pd.details_requested.as_ref() != Some(&key)
                && pd.details_pending.as_ref().map(|(k, _)| k) != Some(&key)
            {
                pd.details_pending = Some((key, now + DETAILS_DEBOUNCE));
            }
        }

        let desired_diff = self
            .modal_stack
            .push_dialog()
            .and_then(|pd| pd.desired_diff_key(&self.diff_caches));
        let cached = self.modal_stack.push_dialog_mut().and_then(|pd| {
            pd.diff
                .reconcile_fetch(desired_diff, &mut self.diff_caches, now, DETAILS_DEBOUNCE)
        });
        if let Some(diff) = cached {
            self.schedule_file_syntax(diff, effects);
        }
    }

    fn dispatch_reflog_action(&mut self, action: &Action, effects: &mut Vec<Effect>) -> bool {
        match action {
            Action::OpenReflog(ref_name) => {
                let ref_name = if ref_name.trim().is_empty() {
                    "HEAD".to_string()
                } else {
                    ref_name.trim().to_string()
                };
                let state = crate::log_view::ReflogState::new_with_prefs(
                    ref_name,
                    self.log.details.diff.context,
                    self.config.jbgit.diff.whitespace.to_core(),
                    self.config.jbgit.diff.soft_wrap,
                );
                let key = state.key.clone();
                self.blur_active_pane(false);
                self.modal_stack.dismiss_context_menus();
                self.modal_stack.open_reflog(state);
                effects.push(Effect::RequestReflog(key));
            }
            Action::CloseReflog => {
                self.modal_stack.close_reflog();
            }
            Action::ReflogFocus(focus) => {
                if let Some(rf) = self.modal_stack.reflog_mut() {
                    rf.focus = *focus;
                }
            }
            Action::ReflogCycleFocus(reverse) => {
                if let Some(rf) = self.modal_stack.reflog_mut() {
                    rf.focus = if *reverse {
                        rf.focus.prev()
                    } else {
                        rf.focus.next()
                    };
                }
            }
            Action::ReflogMove(delta) => {
                if let Some(rf) = self.modal_stack.reflog_mut() {
                    rf.move_selection(*delta);
                }
            }
            Action::ReflogSelect(idx) => {
                if let Some(rf) = self.modal_stack.reflog_mut() {
                    rf.select_row(*idx);
                }
            }
            Action::ReflogChangesMove(delta) => {
                if let Some(rf) = self.modal_stack.reflog_mut() {
                    rf.changes.move_selection(*delta, &self.diff_caches);
                    rf.diff.reset_view_offsets();
                }
            }
            Action::ReflogChangesSelect(idx) => {
                if let Some(rf) = self.modal_stack.reflog_mut() {
                    rf.changes.select_row(*idx, &self.diff_caches);
                    rf.diff.reset_view_offsets();
                }
            }
            Action::ReflogDiffScroll(delta) => {
                self.scroll_pane(ScrollPane::ReflogDiff, *delta);
            }
            Action::ReflogToggleSideBySide => {
                if let Some(rf) = self.modal_stack.reflog_mut() {
                    rf.diff.toggle_side_by_side();
                }
            }
            Action::ReflogCycleContext => {
                if let Some(rf) = self.modal_stack.reflog_mut() {
                    rf.diff.cycle_context();
                }
            }
            Action::DiffJumpHunk(DiffScope::Reflog, delta) => {
                if let Some(rf) = self.modal_stack.reflog_mut() {
                    let diff = rf
                        .desired_diff_key(&self.diff_caches)
                        .and_then(|k| self.diff_caches.peek_diff(&k));
                    rf.diff.jump_hunk(*delta, diff);
                }
            }
            Action::DiffCycleWhitespace(DiffScope::Reflog) => {
                if let Some(rf) = self.modal_stack.reflog_mut() {
                    rf.diff.cycle_whitespace();
                }
            }
            Action::DiffToggleSoftWrap(DiffScope::Reflog) => {
                if let Some(rf) = self.modal_stack.reflog_mut() {
                    rf.diff.toggle_soft_wrap();
                }
            }
            Action::DiffScrollHoriz(DiffScope::Reflog, delta) => {
                if let Some(rf) = self.modal_stack.reflog_mut() {
                    rf.diff.scroll_horiz(*delta);
                }
            }
            Action::DiffSearchOpen(DiffScope::Reflog) => {
                if let Some(rf) = self.modal_stack.reflog_mut() {
                    rf.diff.search.open();
                }
            }
            Action::DiffSearchChar(DiffScope::Reflog, ch) => {
                let tab_size = self.config.jbgit.diff.tab_size;
                if let Some(rf) = self.modal_stack.reflog_mut() {
                    rf.diff.search.push_char(*ch);
                    let diff = rf
                        .desired_diff_key(&self.diff_caches)
                        .and_then(|k| self.diff_caches.peek_diff(&k).cloned());
                    rf.diff.refresh_search(diff.as_ref(), tab_size);
                }
            }
            Action::DiffSearchBackspace(DiffScope::Reflog) => {
                let tab_size = self.config.jbgit.diff.tab_size;
                if let Some(rf) = self.modal_stack.reflog_mut() {
                    rf.diff.search.backspace();
                    let diff = rf
                        .desired_diff_key(&self.diff_caches)
                        .and_then(|k| self.diff_caches.peek_diff(&k).cloned());
                    rf.diff.refresh_search(diff.as_ref(), tab_size);
                }
            }
            Action::DiffSearchClear(DiffScope::Reflog) => {
                if let Some(rf) = self.modal_stack.reflog_mut() {
                    rf.diff.search.clear_query();
                }
            }
            Action::DiffSearchConfirm(DiffScope::Reflog) => {
                if let Some(rf) = self.modal_stack.reflog_mut() {
                    rf.diff.search.finish_editing();
                }
            }
            Action::DiffSearchClose(DiffScope::Reflog) => {
                if let Some(rf) = self.modal_stack.reflog_mut() {
                    rf.diff.search.close();
                }
            }
            Action::DiffSearchJump(DiffScope::Reflog, delta) => {
                let tab_size = self.config.jbgit.diff.tab_size;
                if let Some(rf) = self.modal_stack.reflog_mut() {
                    let diff = rf
                        .desired_diff_key(&self.diff_caches)
                        .and_then(|k| self.diff_caches.peek_diff(&k).cloned());
                    rf.diff.jump_search(*delta, diff.as_ref(), tab_size);
                }
            }
            Action::ReflogLoadMore => {
                if let Some(rf) = self.modal_stack.reflog_mut()
                    && !rf.loading
                    && rf.has_more()
                {
                    let key = rf.load_more();
                    effects.push(Effect::RequestReflog(key));
                }
            }
            Action::ReflogCheckoutSelected => {
                if let Some(entry) = self
                    .modal_stack
                    .reflog()
                    .and_then(|rf| rf.selected_entry().cloned())
                {
                    self.modal_stack.close_reflog();
                    effects.extend(self.dispatch(Action::RunGitOp(jbgit_core::GitOp::Checkout {
                        target: entry.oid.to_string(),
                        detach: true,
                    })));
                }
            }
            Action::ReflogNewBranchSelected => {
                if let Some(entry) = self
                    .modal_stack
                    .reflog()
                    .and_then(|rf| rf.selected_entry().cloned())
                {
                    let existing: Vec<String> = self
                        .repo_snapshot
                        .as_deref()
                        .map(|s| {
                            s.refs
                                .iter()
                                .filter(|r| r.kind == jbgit_core::RefKind::LocalBranch)
                                .map(|r| r.name.clone())
                                .collect()
                        })
                        .unwrap_or_default();
                    let short = entry.oid.short();
                    let dlg = crate::dialogs::new_branch_dialog(
                        &entry.oid.to_string(),
                        &short,
                        existing,
                        "",
                        &self.i18n,
                    );
                    self.push_modal(ModalLayer::Dialog(Box::new(dlg)));
                }
            }
            Action::ReflogCherryPickSelected => {
                if let Some(entry) = self
                    .modal_stack
                    .reflog()
                    .and_then(|rf| rf.selected_entry().cloned())
                {
                    self.modal_stack.close_reflog();
                    effects.extend(self.dispatch(Action::RunGitOp(
                        jbgit_core::GitOp::CherryPick {
                            oids: vec![entry.oid],
                            no_commit: false,
                            record_origin: false,
                            mainline: None,
                        },
                    )));
                }
            }
            Action::ReflogResetSelected => {
                if let Some(entry) = self
                    .modal_stack
                    .reflog()
                    .and_then(|rf| rf.selected_entry().cloned())
                {
                    let cur = self
                        .repo_snapshot
                        .as_deref()
                        .and_then(|s| s.head_branch.clone())
                        .unwrap_or_else(|| "HEAD".to_string());
                    let dlg =
                        crate::dialogs::reset_dialog(entry.oid, &entry.message, &cur, &self.i18n);
                    self.push_modal(ModalLayer::Dialog(Box::new(dlg)));
                }
            }
            Action::ReflogJumpInLog => {
                if let Some(entry) = self
                    .modal_stack
                    .reflog()
                    .and_then(|rf| rf.selected_entry().cloned())
                {
                    self.modal_stack.close_reflog();
                    self.set_focus(WorkspaceTab::Log, PaneId::LogCommitTable);
                    effects.extend(self.dispatch(Action::JumpToCommit(entry.oid)));
                }
            }
            Action::ReflogCopyHash => {
                if let Some(entry) = self
                    .modal_stack
                    .reflog()
                    .and_then(|rf| rf.selected_entry().cloned())
                {
                    effects.extend(self.dispatch(Action::CopyText(entry.oid.to_string())));
                }
            }
            _ => return false,
        }
        true
    }

    pub fn reconcile_reflog_requests(&mut self, now: Instant, effects: &mut Vec<Effect>) {
        let Some(desired_details) = self
            .modal_stack
            .reflog()
            .and_then(|rf| rf.desired_details_key())
        else {
            return;
        };
        if let Some(rf) = self.modal_stack.reflog_mut()
            && rf.changes.current_key.as_ref() != Some(&desired_details)
        {
            rf.changes.current_key = Some(desired_details.clone());
            rf.changes.selected = 0;
            rf.diff.scroll.reset();
        }
        if !self.diff_caches.details.contains(&desired_details)
            && let Some(rf) = self.modal_stack.reflog_mut()
            && rf.details_requested.as_ref() != Some(&desired_details)
        {
            rf.details_requested = Some(desired_details.clone());
            effects.push(Effect::RequestCommitDetails {
                slot: jbgit_core::RequestSlotId::ReflogCommitDetails,
                key: desired_details,
            });
        }

        let desired_diff = self
            .modal_stack
            .reflog()
            .and_then(|rf| rf.desired_diff_key(&self.diff_caches));
        let cached = self.modal_stack.reflog_mut().and_then(|rf| {
            rf.diff
                .reconcile_fetch(desired_diff, &mut self.diff_caches, now, DETAILS_DEBOUNCE)
        });
        if let Some(diff) = cached {
            self.schedule_file_syntax(diff, effects);
        }
    }
}
