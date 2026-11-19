use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use jbgit_core::{ChangeRowKind, ModalButton, PaneId, WorkspaceTab};

use crate::app::{
    Action, AppState, ClickRecord, ContextMenuAction, ContextMenuItem,
    DragState, ModalLayer, PopupKind,
};
use crate::graph::GraphRendererMode;
use crate::hit_test::{
    BranchesToolbarButton, ChangesToolbarButton, DiffToolbarButton, GraphHitKind, HitTarget,
};
use crate::input_stage::{
    route_commit_changes_tree_key, route_commit_priority_key, route_stage_left_click,
    stage_tree_context_menu_items,
};
use crate::input_menus::{
    activate_top_menu_item, context_menu_for_target, keyboard_menu_target, on_menu_hover,
    route_conflict_merge_key, route_context_menu_key, route_dialog, route_spec05_click,
};
use crate::log_view::{PresentationToggle, complete_last_token, filter_suggestions};

pub fn route_terminal_event(event: Event, state: &mut AppState) -> Vec<Action> {
    match event {
        Event::Key(key) => {
            if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
                return Vec::new();
            }
            route_key(key, state)
        }
        Event::Mouse(mouse) => route_mouse(mouse, state),
        Event::Resize(_, _) => vec![Action::Resize],
        Event::Paste(text) => route_paste(text, state),
        Event::FocusGained | Event::FocusLost => Vec::new(),
    }
}

fn route_paste(text: String, state: &mut AppState) -> Vec<Action> {
    if let Some(top) = state.modal_stack.last_mut() {
        match top {
            ModalLayer::Dialog(d) => match d.as_mut() {
                crate::dialogs::Dialog::Input(inp) => {
                    let clean: String = text.chars().filter(|c| *c != '\n' && *c != '\r').collect();
                    if !clean.is_empty() {
                        inp.value.push_str(&clean);
                        state.needs_redraw = true;
                    }
                }
                crate::dialogs::Dialog::Message(msg) => {
                    let norm = text.replace("\r\n", "\n").replace('\r', "\n");
                    if !norm.is_empty() {
                        msg.text.push_str(&norm);
                        state.needs_redraw = true;
                    }
                }
                _ => {}
            },
            ModalLayer::FloatingPopup {
                kind,
                query,
                selected,
            } => {
                let kind = *kind;
                match kind {
                    PopupKind::BranchesQuickView => {
                        let clean: String = text
                            .chars()
                            .filter(|c| *c != '\n' && *c != '\r')
                            .collect();
                        if !clean.is_empty() {
                            let mut next_q = query.clone();
                            next_q.push_str(&clean);
                            let next_sel = crate::branches_popup::first_selectable_row(state, &next_q);
                            if let Some(ModalLayer::FloatingPopup { query, selected, .. }) =
                                state.modal_stack.last_mut()
                            {
                                *query = next_q;
                                *selected = next_sel;
                            }
                            state.needs_redraw = true;
                        }
                    }
                    PopupKind::ShortcutsHelp => {
                        let clean: String = text
                            .chars()
                            .filter(|c| *c != '\n' && *c != '\r')
                            .collect();
                        if !clean.is_empty() {
                            let mut next_q = query.clone();
                            next_q.push_str(&clean);
                            let rows = crate::keymap::build_help_rows(state, &next_q);
                            let next_sel = crate::keymap::first_help_entry(&rows);
                            if let Some(ModalLayer::FloatingPopup { query, selected, .. }) =
                                state.modal_stack.last_mut()
                            {
                                *query = next_q;
                                *selected = next_sel;
                            }
                            state.needs_redraw = true;
                        }
                    }
                    PopupKind::FilterEditor(_)
                    | PopupKind::GoToRevision
                    | PopupKind::CommitHistory => {
                        let clean: String = text
                            .chars()
                            .filter(|c| *c != '\n' && *c != '\r')
                            .collect();
                        if !clean.is_empty() {
                            query.push_str(&clean);
                            *selected = 0;
                            state.needs_redraw = true;
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
        return Vec::new();
    }

    if state.active_tab == WorkspaceTab::Commit
        && state.focused_pane == PaneId::CommitMessageEditor
    {
        if state.stage_workbench.editing_author {
            return text
                .chars()
                .filter(|c| *c != '\n' && *c != '\r')
                .map(Action::CommitAuthorChar)
                .collect();
        }
        return vec![Action::CommitEditorPaste(text)];
    }

    if state.active_tab == WorkspaceTab::Log && state.log_filter.editing_text {
        return text
            .chars()
            .filter(|c| *c != '\n' && *c != '\r')
            .map(Action::FilterTextInput)
            .collect();
    }

    if state.active_tab == WorkspaceTab::Log && state.focused_pane == PaneId::LogBranchesTree {
        return text
            .chars()
            .filter(|c| !c.is_control() && *c != ' ')
            .map(Action::BranchesSearchInput)
            .collect();
    }

    Vec::new()
}

fn plain_char(key: &KeyEvent) -> Option<char> {
    match key.code {
        KeyCode::Char(c)
            if !key.modifiers.contains(KeyModifiers::CONTROL)
                && !key.modifiers.contains(KeyModifiers::ALT) =>
        {
            Some(c)
        }
        _ => None,
    }
}

fn route_key(key: KeyEvent, state: &mut AppState) -> Vec<Action> {
    if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
        return vec![Action::Quit];
    }
    if state.modal_stack.is_empty() && key.modifiers.contains(KeyModifiers::ALT) {
        match key.code {
            KeyCode::Char('z')
                if state
                    .notification
                    .as_ref()
                    .is_some_and(|n| n.action.is_some()) =>
            {
                return vec![Action::NotificationActivate];
            }
            KeyCode::Char('x') if state.notification.is_some() => {
                return vec![Action::NotificationDismiss];
            }
            _ => {}
        }
    }

    // Top-down Z-order interception: check topmost modal layer first
    if let Some(top) = state.modal_stack.last_mut() {
        match top {
            ModalLayer::CriticalDialog {
                selected_button, ..
            } => {
                return match key.code {
                    KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('q') => {
                        vec![Action::PopModal]
                    }
                    KeyCode::Left | KeyCode::Right | KeyCode::Tab => {
                        *selected_button = selected_button.toggle();
                        state.needs_redraw = true;
                        Vec::new()
                    }
                    KeyCode::Enter | KeyCode::Char('y') => {
                        if *selected_button == ModalButton::Confirm
                            || key.code == KeyCode::Char('y')
                        {
                            vec![Action::ConfirmCriticalDialog]
                        } else {
                            vec![Action::PopModal]
                        }
                    }
                    _ => Vec::new(),
                };
            }
            ModalLayer::ContextMenu { .. } => {
                return route_context_menu_key(key, state);
            }
            ModalLayer::Dialog(_) => return route_dialog(&key, state),
            ModalLayer::FloatingPopup {
                kind,
                query,
                selected,
            } => {
                let kind = *kind;
                if kind == PopupKind::BranchesQuickView {
                    return crate::branches_popup::route_branches_popup_key(&key, state);
                }
                if kind == PopupKind::ConflictMerge {
                    return route_conflict_merge_key(&key, state);
                }
                if kind == PopupKind::ShortcutsHelp {
                    return route_shortcuts_help_key(&key, state);
                }
                return match key.code {
                    KeyCode::Esc => vec![Action::PopModal],
                    KeyCode::Enter => match kind {
                        PopupKind::FilterEditor(field) => vec![
                            Action::PopModal,
                            Action::ApplyFilterField {
                                field,
                                expr: query.clone(),
                            },
                        ],
                        PopupKind::GoToRevision => {
                            vec![Action::PopModal, Action::GoToRevision(query.clone())]
                        }
                        PopupKind::ChooseParent => {
                            let idx = *selected;
                            vec![Action::PopModal, Action::GoToParent(idx)]
                        }
                        PopupKind::CommitHistory => {
                            let q_low = query.to_ascii_lowercase();
                            let sel = *selected;
                            let candidates = state.commit_history_candidates();
                            let mapped = candidates
                                .iter()
                                .enumerate()
                                .filter(|(_, m)| {
                                    q_low.is_empty() || m.to_ascii_lowercase().contains(&q_low)
                                })
                                .nth(sel)
                                .map(|(i, _)| i);
                            if let Some(idx) = mapped {
                                vec![Action::PopModal, Action::CommitApplyHistory(idx)]
                            } else {
                                vec![Action::PopModal]
                            }
                        }
                        _ => Vec::new(),
                    },
                    KeyCode::Char(d @ '1'..='9')
                        if kind == PopupKind::ChooseParent
                            && !key.modifiers.contains(KeyModifiers::CONTROL)
                            && !key.modifiers.contains(KeyModifiers::ALT) =>
                    {
                        let idx = (d as usize) - ('1' as usize);
                        let pcount = state
                            .log_commit_at(state.log_graph.selected_idx)
                            .map_or(0, |c| c.parents.len());
                        if idx < pcount {
                            vec![Action::PopModal, Action::GoToParent(idx)]
                        } else {
                            Vec::new()
                        }
                    }
                    KeyCode::Tab => {
                        if let PopupKind::FilterEditor(field) = kind {
                            let snap = state.repo_snapshot.as_deref();
                            let suggestions = filter_suggestions(field, snap, query);
                            if let Some(s) = suggestions.get(*selected) {
                                *query = complete_last_token(query, s);
                                *selected = 0;
                                state.needs_redraw = true;
                            }
                        }
                        Vec::new()
                    }
                    KeyCode::Backspace => {
                        query.pop();
                        *selected = 0;
                        state.needs_redraw = true;
                        Vec::new()
                    }
                    KeyCode::Up => {
                        *selected = selected.saturating_sub(1);
                        state.needs_redraw = true;
                        Vec::new()
                    }
                    KeyCode::Down => {
                        *selected = selected.saturating_add(1);
                        state.needs_redraw = true;
                        Vec::new()
                    }
                    KeyCode::Char(ch)
                        if !key.modifiers.contains(KeyModifiers::CONTROL)
                            && !key.modifiers.contains(KeyModifiers::ALT) =>
                    {
                        query.push(ch);
                        *selected = 0;
                        state.needs_redraw = true;
                        Vec::new()
                    }
                    _ => Vec::new(),
                };
            }
        }
    }

    if state.active_tab == WorkspaceTab::Log
        && let Some(actions) = route_log_priority_key(&key, state)
    {
        return actions;
    }

    if state.active_tab == WorkspaceTab::Commit
        && let Some(actions) = route_commit_priority_key(&key, state)
    {
        return actions;
    }

    // Layer 0: Dynamic Keymap (from keymap.toml; handles Esc chord cancellation internally)
    if let Some(actions) = crate::keymap::route_dynamic_key(&key, state) {
        return actions;
    }

    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    let in_log = state.active_tab == WorkspaceTab::Log;

    // Layer 1: Structural & compatibility fallbacks
    match key.code {
        KeyCode::Menu => {
            if let Some(actions) = keyboard_context_menu(state) {
                return actions;
            }
        }
        KeyCode::Char('b' | 'B') if alt && !in_log => {
            return vec![Action::OpenBranchesPopup {
                query: String::new(),
            }];
        }
        KeyCode::Char('n' | 'N') if ctrl && alt => {
            return vec![Action::OpenDialog(Box::new(
                crate::dialogs::new_branch_from_head_dialog(
                    state.repo_snapshot.as_deref(),
                    &state.i18n,
                ),
            ))];
        }
        KeyCode::F(12) => return vec![Action::ToggleGlyphMode],
        KeyCode::F(8) => return vec![Action::CycleGraphRenderer],
        _ => {}
    }

    // Focused pane structural fallbacks
    match state.focused_pane {
        PaneId::LogBranchesTree => match key.code {
            KeyCode::PageDown => vec![Action::ScrollBranches(10)],
            KeyCode::PageUp => vec![Action::ScrollBranches(-10)],
            KeyCode::Right => vec![Action::BranchesExpand(true)],
            KeyCode::Left => vec![Action::BranchesExpand(false)],
            KeyCode::Enter => vec![Action::BranchesApplyFilter],
            KeyCode::Esc => vec![Action::BranchesEscape],
            _ => Vec::new(),
        },
        PaneId::LogCommitTable => match key.code {
            KeyCode::Down if shift => vec![Action::ExtendLogSelection(1)],
            KeyCode::Up if shift => vec![Action::ExtendLogSelection(-1)],
            KeyCode::PageDown => vec![Action::LogPage {
                half: false,
                down: true,
            }],
            KeyCode::PageUp => vec![Action::LogPage {
                half: false,
                down: false,
            }],
            KeyCode::Left if alt => vec![Action::NavLogBack],
            KeyCode::Right if alt => vec![Action::NavLogForward],
            KeyCode::Right if shift => {
                let parents = state
                    .log_commit_at(state.log_graph.selected_idx)
                    .map_or(0, |c| c.parents.len());
                if parents > 2 {
                    vec![Action::OpenPopup {
                        kind: PopupKind::ChooseParent,
                        initial_query: String::new(),
                    }]
                } else {
                    vec![Action::GoToParent(1)]
                }
            }
            KeyCode::Right => vec![Action::GoToParent(0)],
            KeyCode::Left => vec![Action::GoToChild],
            KeyCode::Enter => vec![Action::FocusPane(PaneId::LogChangesTree)],
            KeyCode::Esc if state.log_graph.anchor.is_some() => {
                vec![Action::SelectLogRow(state.log_graph.selected_idx)]
            }
            _ => Vec::new(),
        },
        PaneId::LogChangesTree => match key.code {
            KeyCode::PageDown => vec![Action::ChangesMove(10)],
            KeyCode::PageUp => vec![Action::ChangesMove(-10)],
            KeyCode::Right => vec![Action::ChangesExpand(true)],
            KeyCode::Left => vec![Action::ChangesExpand(false)],
            KeyCode::Enter if ctrl => vec![Action::ZoomDiff(true)],
            KeyCode::Enter => {
                let is_dir = state
                    .details
                    .change_rows()
                    .get(state.details.changes_selected)
                    .map(|r| (matches!(r.kind, ChangeRowKind::Dir { .. }), r.expanded));
                match is_dir {
                    Some((true, expanded)) => vec![Action::ChangesExpand(!expanded)],
                    _ => vec![Action::ToggleDiffPreview],
                }
            }
            KeyCode::Esc => vec![Action::FocusPane(PaneId::LogCommitTable)],
            KeyCode::Char('v') if !ctrl && !alt => vec![Action::ChangesToggleGroupByDir],
            KeyCode::Char('e') if !ctrl && !alt => vec![Action::ChangesExpandAll(true)],
            KeyCode::Char('E') if !ctrl && !alt => vec![Action::ChangesExpandAll(false)],
            _ => Vec::new(),
        },
        PaneId::LogDetailsDiff if state.details.diff_preview => {
            let page = state.log_graph.last_viewport_h.max(4) as isize / 2;
            match key.code {
                KeyCode::PageDown => vec![Action::ScrollDiff(page)],
                KeyCode::PageUp => vec![Action::ScrollDiff(-page)],
                KeyCode::Enter if ctrl => vec![Action::ZoomDiff(true)],
                KeyCode::Esc => vec![
                    Action::ToggleDiffPreview,
                    Action::FocusPane(PaneId::LogChangesTree),
                ],
                _ => Vec::new(),
            }
        }
        PaneId::LogDetailsDiff => match key.code {
            KeyCode::PageDown => vec![Action::ScrollDetails(10)],
            KeyCode::PageUp => vec![Action::ScrollDetails(-10)],
            KeyCode::Esc => vec![Action::FocusPane(PaneId::LogCommitTable)],
            _ => Vec::new(),
        },
        PaneId::ConsoleCommandList => match key.code {
            KeyCode::Enter => vec![Action::FocusPane(PaneId::ConsoleOutputDetail)],
            KeyCode::Char('p') if !ctrl && !alt => vec![Action::RunCliProbe(vec![
                "status".into(),
                "--short".into(),
                "--branch".into(),
            ])],
            _ => Vec::new(),
        },
        PaneId::ConsoleOutputDetail => match key.code {
            KeyCode::PageDown => vec![Action::ScrollConsoleOutput(10)],
            KeyCode::PageUp => vec![Action::ScrollConsoleOutput(-10)],
            KeyCode::Esc => vec![Action::FocusPane(PaneId::ConsoleCommandList)],
            _ => Vec::new(),
        },
        PaneId::CommitChangesTree => route_commit_changes_tree_key(&key, state),
        PaneId::CommitDiffPreview => crate::input_stage::route_commit_diff_fallback_key(&key, state),
        PaneId::StashList | PaneId::StashDiffPreview => {
            crate::input_menus::route_stash_fallback_key(&key, state)
        }
        _ => Vec::new(),
    }
}

fn route_shortcuts_help_key(key: &KeyEvent, state: &mut AppState) -> Vec<Action> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let (query, selected) = match state.modal_stack.last() {
        Some(ModalLayer::FloatingPopup {
            kind: PopupKind::ShortcutsHelp,
            query,
            selected,
        }) => (query.clone(), *selected),
        _ => return Vec::new(),
    };

    let set = |state: &mut AppState, q: String, sel: usize| {
        if let Some(ModalLayer::FloatingPopup {
            query, selected, ..
        }) = state.modal_stack.last_mut()
        {
            *query = q;
            *selected = sel;
        }
        state.needs_redraw = true;
    };

    match key.code {
        KeyCode::Esc => vec![Action::PopModal],
        KeyCode::Char('?') if query.is_empty() && !ctrl && !alt => vec![Action::PopModal],
        KeyCode::Up | KeyCode::Down | KeyCode::PageUp | KeyCode::PageDown => {
            let rows = crate::keymap::build_help_rows(state, &query);
            let delta = match key.code {
                KeyCode::Up => -1,
                KeyCode::Down => 1,
                KeyCode::PageUp => -8,
                _ => 8,
            };
            let next = crate::keymap::step_help_selection(&rows, selected, delta);
            set(state, query, next);
            Vec::new()
        }
        KeyCode::Enter => {
            let rows = crate::keymap::build_help_rows(state, &query);
            let resolved = crate::keymap::resolve_help_selection(&rows, selected);
            let run_opt = match rows.get(resolved) {
                Some(crate::keymap::HelpRow::Entry { run, .. }) => run.clone(),
                _ => None,
            };
            let mut out = vec![Action::PopModal];
            if let Some(r) = run_opt
                && r != crate::keymap::KeyAction::Help
                && let Some(actions) = crate::keymap::execute_action(&r, state)
            {
                out.extend(actions);
            }
            out
        }
        KeyCode::Char('u') if ctrl => {
            let rows = crate::keymap::build_help_rows(state, "");
            let first = crate::keymap::first_help_entry(&rows);
            set(state, String::new(), first);
            Vec::new()
        }
        KeyCode::Backspace => {
            let mut q = query;
            q.pop();
            let rows = crate::keymap::build_help_rows(state, &q);
            let first = crate::keymap::first_help_entry(&rows);
            set(state, q, first);
            Vec::new()
        }
        KeyCode::Char(ch) if !ctrl && !alt => {
            let mut q = query;
            q.push(ch);
            let rows = crate::keymap::build_help_rows(state, &q);
            let first = crate::keymap::first_help_entry(&rows);
            set(state, q, first);
            Vec::new()
        }
        _ => Vec::new(),
    }
}

/// Fallback keymap for full-screen zoomed diff viewer (after dynamic keymap).
fn route_diff_zoomed_fallback(key: &KeyEvent, page: isize) -> Vec<Action> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::PageDown => vec![Action::ScrollDiff(page)],
        KeyCode::Char(' ') => vec![Action::ScrollDiff(page)],
        KeyCode::PageUp => vec![Action::ScrollDiff(-page)],
        KeyCode::Home => vec![Action::ScrollDiff(isize::MIN / 2)],
        KeyCode::End => vec![Action::ScrollDiff(isize::MAX / 2)],
        KeyCode::Char('P') => vec![Action::SwitchMergeParent],
        KeyCode::Enter if ctrl => vec![Action::ZoomDiff(false)],
        KeyCode::Esc => vec![Action::ZoomDiff(false)],
        _ => Vec::new(),
    }
}

/// Log-tab keys that must win over global shortcuts: zoomed diff, filter text
/// typing and Branches search mode.
fn route_log_priority_key(key: &KeyEvent, state: &mut AppState) -> Option<Vec<Action>> {
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

    if state.details.diff_zoomed {
        if let Some(actions) = crate::keymap::route_dynamic_key(key, state) {
            return Some(actions);
        }
        let page = state.terminal_area.height.max(6) as isize / 2;
        return Some(route_diff_zoomed_fallback(key, page));
    }

    if state.log_filter.editing_text {
        return Some(match key.code {
            KeyCode::Esc | KeyCode::Enter | KeyCode::Down | KeyCode::Tab => {
                vec![Action::FilterTextDone]
            }
            KeyCode::Backspace => vec![Action::FilterTextBackspace],
            KeyCode::Char('r') if alt => vec![Action::ToggleFilterRegex],
            KeyCode::Char('c') if alt => vec![Action::ToggleFilterCase],
            KeyCode::Char('u') if ctrl => vec![Action::FilterTextClear],
            _ => match plain_char(key) {
                Some(c) => vec![Action::FilterTextInput(c)],
                None => Vec::new(),
            },
        });
    }

    if state.focused_pane == PaneId::LogBranchesTree {
        let active_search = state.branches.searching;
        let speed_active = state.config.keymap.tree_speed_search && !state.branches.query.is_empty();
        if active_search {
            match key.code {
                KeyCode::Enter => return Some(vec![Action::BranchesSearchDone]),
                KeyCode::Up => return Some(vec![Action::ScrollBranches(-1)]),
                KeyCode::Down => return Some(vec![Action::ScrollBranches(1)]),
                KeyCode::Char('u') if ctrl => return Some(vec![Action::BranchesSearchClear]),
                _ => {}
            }
        }
        if active_search || speed_active {
            match key.code {
                KeyCode::Esc => return Some(vec![Action::BranchesEscape]),
                KeyCode::Backspace => return Some(vec![Action::BranchesSearchBackspace]),
                _ => {}
            }
            if let Some(c) = plain_char(key)
                && c != ' '
            {
                return Some(vec![Action::BranchesSearchInput(c)]);
            }
        } else if state.config.keymap.tree_speed_search
            && let Some(c) = plain_char(key)
            && (c.is_alphabetic() || matches!(c, '-' | '_' | '/' | '0' | '5'..='9'))
            && c != 'J'
        {
            return Some(vec![Action::BranchesSearchInput(c)]);
        }
    }
    None
}

pub(crate) fn keyboard_context_menu(state: &mut AppState) -> Option<Vec<Action>> {
    let (target, anchor) = keyboard_menu_target(state)?;
    let mut actions = Vec::new();
    let items = build_menu_for(&target, state, &mut actions)?;
    if items.is_empty() {
        return None;
    }
    actions.push(Action::OpenContextMenu {
        anchor,
        items,
        depth: 0,
    });
    Some(actions)
}

fn route_mouse(mouse: MouseEvent, state: &mut AppState) -> Vec<Action> {
    let col = mouse.column;
    let row = mouse.row;

    match mouse.kind {
        MouseEventKind::Drag(MouseButton::Left) => match state.drag_state {
            Some(DragState::Splitter {
                splitter_id,
                axis,
                container,
            }) => {
                state.splitters.update_from_pointer(
                    splitter_id,
                    axis,
                    container,
                    col,
                    row,
                );
                state.needs_redraw = true;
                Vec::new()
            }
            Some(DragState::LogSelection) => {
                if let Some(region) = state.hit_registry.hit_test(col, row) {
                    let target_row = match &region.target {
                        HitTarget::CommitTableRow { visible_row, .. }
                        | HitTarget::GraphPrintElement { visible_row, .. }
                        | HitTarget::CommitRefPill { visible_row, .. }
                        | HitTarget::CommitRefOverflow { visible_row, .. } => Some(*visible_row),
                        _ => None,
                    };
                    if let Some(r) = target_row {
                        return vec![Action::ExtendLogSelectionTo(r)];
                    }
                }
                Vec::new()
            }
            None => Vec::new(),
        },
        MouseEventKind::Up(MouseButton::Left) => {
            if state.drag_state.take().is_some() {
                state.needs_redraw = true;
            }
            Vec::new()
        }
        MouseEventKind::Moved => {
            state.hover_pos = Some((col, row));
            let next_target = state
                .hit_registry
                .hit_test(col, row)
                .map(|r| r.target.clone());
            if let Some(HitTarget::MenuItem {
                menu_depth,
                item_idx,
            }) = &next_target
            {
                on_menu_hover(state, *menu_depth, *item_idx);
            }
            if let Some(HitTarget::BranchPopupRow(idx)) = &next_target
                && let Some(ModalLayer::FloatingPopup {
                    kind: PopupKind::BranchesQuickView,
                    selected,
                    ..
                }) = state.modal_stack.last_mut()
                && *selected != *idx
            {
                *selected = *idx;
                state.needs_redraw = true;
            }
            if next_target != state.hover_target {
                state.hover_target = next_target;
                state.needs_redraw = true;
            }
            Vec::new()
        }
        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
            let delta = if matches!(mouse.kind, MouseEventKind::ScrollUp) {
                -3
            } else {
                3
            };
            // Route wheel to panel under cursor, NOT necessarily keyboard-focused pane!
            if let Some(region) = state.hit_registry.hit_test(col, row) {
                match &region.target {
                    HitTarget::BranchTreeNode { .. }
                    | HitTarget::BranchTreeToggle { .. }
                    | HitTarget::Pane(PaneId::LogBranchesTree) => {
                        return vec![Action::ScrollBranches(delta / 3)];
                    }
                    HitTarget::CommitTableRow { .. }
                    | HitTarget::GraphPrintElement { .. }
                    | HitTarget::CommitRefPill { .. }
                    | HitTarget::CommitRefOverflow { .. }
                    | HitTarget::Pane(PaneId::LogCommitTable) => {
                        return vec![Action::ScrollLog(delta / 3)];
                    }
                    HitTarget::LogChangeRow { .. }
                    | HitTarget::ChangesToolbar(_)
                    | HitTarget::Pane(PaneId::LogChangesTree) => {
                        return vec![Action::ChangesMove(delta / 3)];
                    }
                    HitTarget::DiffArea
                    | HitTarget::StageDiffLine { .. }
                    | HitTarget::StageDiffGutter { .. }
                    | HitTarget::Pane(PaneId::CommitDiffPreview) => {
                        return vec![if state.active_tab == WorkspaceTab::Commit {
                            Action::StageDiffScroll(delta)
                        } else {
                            Action::ScrollDiff(delta)
                        }];
                    }
                    HitTarget::DetailsArea
                    | HitTarget::DetailsHash { .. }
                    | HitTarget::DetailsParent { .. }
                    | HitTarget::Pane(PaneId::LogDetailsDiff) => {
                        return vec![if state.details.diff_visible() {
                            Action::ScrollDiff(delta)
                        } else {
                            Action::ScrollDetails(delta)
                        }];
                    }
                    HitTarget::ConsoleCommandRow { .. }
                    | HitTarget::Pane(PaneId::ConsoleCommandList) => {
                        return vec![Action::ScrollConsoleList(delta / 3)];
                    }
                    HitTarget::ConsoleOutputArea | HitTarget::Pane(PaneId::ConsoleOutputDetail) => {
                        return vec![Action::ScrollConsoleOutput(delta)];
                    }
                    HitTarget::ChangesTreeNode { .. }
                    | HitTarget::Pane(PaneId::CommitChangesTree) => {
                        return vec![Action::ScrollCommitStage(delta / 3)];
                    }
                    HitTarget::StashRow(_) | HitTarget::Pane(PaneId::StashList) => {
                        return vec![Action::StashMove(delta / 3)];
                    }
                    HitTarget::StashChangeRow(_) | HitTarget::Pane(PaneId::StashDiffPreview) => {
                        return vec![Action::StashDiffScroll(delta)];
                    }
                    _ => {}
                }
            }
            Vec::new()
        }
        MouseEventKind::Down(MouseButton::Left) => {
            let is_double = state
                .last_click
                .is_some_and(|prev| prev.is_double_click(col, row));
            state.last_click = Some(ClickRecord {
                at: std::time::Instant::now(),
                col,
                row,
            });
            let ctrl = mouse.modifiers.contains(KeyModifiers::CONTROL);
            let shift = mouse.modifiers.contains(KeyModifiers::SHIFT);

            let Some(region) = state.hit_registry.hit_test(col, row).cloned() else {
                return Vec::new();
            };

            match region.target {
                HitTarget::ModalBackdrop { layer_z } if (30..40).contains(&layer_z) => {
                    vec![Action::PopContextMenus]
                }
                HitTarget::ModalBackdrop { .. } => vec![Action::PopModal],
                HitTarget::ModalButton(btn) => match btn {
                    ModalButton::Confirm => vec![Action::ConfirmCriticalDialog],
                    ModalButton::Cancel => vec![Action::PopModal],
                },
                HitTarget::MenuItem {
                    menu_depth,
                    item_idx,
                } => {
                    while matches!(
                        state.modal_stack.last(),
                        Some(ModalLayer::ContextMenu { depth, .. }) if *depth > menu_depth
                    ) {
                        state.modal_stack.pop();
                    }
                    state.menu_hover = None;
                    if let Some(ModalLayer::ContextMenu { selected, .. }) =
                        state.modal_stack.last_mut()
                    {
                        *selected = item_idx;
                    }
                    activate_top_menu_item(state, item_idx)
                }
                HitTarget::Splitter { id, axis } => {
                    if is_double {
                        vec![Action::ResetSplitter(id)]
                    } else {
                        if let Some((_, container)) = state.splitter_containers.get(&id).copied() {
                            state.drag_state = Some(DragState::Splitter {
                                splitter_id: id,
                                axis,
                                container,
                            });
                            state.needs_redraw = true;
                        }
                        Vec::new()
                    }
                }
                HitTarget::ToolTab(tab) => vec![Action::SwitchTab(tab)],
                HitTarget::BranchWidgetPill => {
                    vec![Action::OpenBranchesPopup {
                        query: String::new(),
                    }]
                }
                HitTarget::HeaderRefreshButton => vec![Action::RefreshAll],
                HitTarget::HeaderGlyphButton => vec![Action::ToggleGlyphMode],
                HitTarget::HeaderHelpButton => {
                    vec![Action::OpenPopup {
                        kind: PopupKind::ShortcutsHelp,
                        initial_query: String::new(),
                    }]
                }
                HitTarget::BranchTreeNode { row: idx, .. } => {
                    if ctrl {
                        return vec![
                            Action::FocusPane(PaneId::LogBranchesTree),
                            Action::BranchesToggleMulti(idx),
                        ];
                    }
                    let mut actions = vec![
                        Action::FocusPane(PaneId::LogBranchesTree),
                        Action::SelectBranchRow(idx),
                    ];
                    if is_double {
                        actions.push(Action::BranchesApplyFilter);
                    }
                    actions
                }
                HitTarget::BranchTreeToggle { row } => vec![
                    Action::FocusPane(PaneId::LogBranchesTree),
                    Action::SelectBranchRow(row),
                    Action::BranchesToggleRow(row),
                ],
                HitTarget::BranchesToolbar(btn) => match btn {
                    BranchesToolbarButton::Collapse => vec![Action::ToggleBranchesPane],
                    BranchesToolbarButton::Locate => vec![Action::BranchesNavigate],
                    BranchesToolbarButton::GroupByDir => vec![Action::BranchesToggleGroupByDir],
                },
                HitTarget::BranchesExpandStripe => vec![Action::ToggleBranchesPane],
                HitTarget::FilterTextInput => vec![Action::FocusFilterText],
                HitTarget::FilterRegexToggle => vec![Action::ToggleFilterRegex],
                HitTarget::FilterCaseToggle => vec![Action::ToggleFilterCase],
                HitTarget::FilterChip(field) => {
                    let mut initial = state.log_filter.field_expr(field);
                    if !initial.is_empty() {
                        initial.push(' ');
                    }
                    vec![Action::OpenPopup {
                        kind: PopupKind::FilterEditor(field),
                        initial_query: initial,
                    }]
                }
                HitTarget::FilterClear(field) => vec![Action::ClearFilterField(field)],
                HitTarget::FilterPresentation => vec![Action::OpenContextMenu {
                    anchor: (col, row.saturating_add(1)),
                    items: presentation_menu_items(state),
                    depth: 0,
                }],
                HitTarget::LogChangeRow { idx } => {
                    let mut actions = vec![
                        Action::FocusPane(PaneId::LogChangesTree),
                        Action::ChangesSelectRow(idx),
                    ];
                    if is_double {
                        match state.details.change_rows().get(idx) {
                            Some(r) if matches!(r.kind, ChangeRowKind::Dir { .. }) => {
                                actions.push(Action::ChangesExpand(!r.expanded));
                            }
                            Some(_) => actions.push(Action::ZoomDiff(true)),
                            None => {}
                        }
                    }
                    actions
                }
                HitTarget::ChangesToolbar(btn) => match btn {
                    ChangesToolbarButton::GroupByDir => vec![Action::ChangesToggleGroupByDir],
                    ChangesToolbarButton::ExpandAll => vec![Action::ChangesExpandAll(true)],
                    ChangesToolbarButton::CollapseAll => vec![Action::ChangesExpandAll(false)],
                    ChangesToolbarButton::DiffPreview => vec![Action::ToggleDiffPreview],
                    ChangesToolbarButton::SwitchParent => vec![Action::SwitchMergeParent],
                },
                HitTarget::DetailsHash { oid } => vec![Action::CopyText(oid.to_string())],
                HitTarget::DetailsParent { oid } => vec![Action::JumpToCommit(oid)],
                HitTarget::DetailsArea | HitTarget::DiffArea => {
                    vec![Action::FocusPane(PaneId::LogDetailsDiff)]
                }
                HitTarget::DiffToolbar(btn) => match btn {
                    DiffToolbarButton::SideBySide => vec![Action::DiffToggleSideBySide],
                    DiffToolbarButton::Context => vec![Action::DiffCycleContext],
                    DiffToolbarButton::Dock => vec![Action::DiffCycleDock],
                    DiffToolbarButton::Zoom => vec![Action::ZoomDiff(!state.details.diff_zoomed)],
                    DiffToolbarButton::Close => {
                        if state.details.diff_zoomed {
                            vec![Action::ZoomDiff(false)]
                        } else {
                            vec![Action::ToggleDiffPreview]
                        }
                    }
                },
                HitTarget::CommitRefPill {
                    visible_row,
                    ref_name,
                    ..
                } => {
                    let mut actions = vec![
                        Action::FocusPane(PaneId::LogCommitTable),
                        Action::SelectLogRow(visible_row),
                    ];
                    if is_double {
                        actions.push(Action::OpenBranchesPopup { query: ref_name });
                    }
                    actions
                }
                HitTarget::CommitRefOverflow { visible_row, .. } => vec![
                    Action::FocusPane(PaneId::LogCommitTable),
                    Action::SelectLogRow(visible_row),
                ],
                HitTarget::GraphPrintElement {
                    visible_row,
                    commit_oid,
                    kind,
                    ..
                } => match kind {
                    GraphHitKind::Arrow { target_row, .. } => vec![
                        Action::FocusPane(PaneId::LogCommitTable),
                        Action::JumpToLogRow {
                            row: target_row,
                            center: true,
                        },
                    ],
                    GraphHitKind::Node { .. } | GraphHitKind::Edge { .. } => {
                        if shift {
                            return vec![
                                Action::FocusPane(PaneId::LogCommitTable),
                                Action::ExtendLogSelectionTo(visible_row),
                            ];
                        }
                        state.drag_state = Some(DragState::LogSelection);
                        let mut actions = vec![
                            Action::FocusPane(PaneId::LogCommitTable),
                            Action::SelectLogRow(visible_row),
                        ];
                        if is_double {
                            actions.push(Action::RunCliProbe(vec![
                                "show".into(),
                                "--stat".into(),
                                commit_oid.to_string(),
                            ]));
                        }
                        actions
                    }
                },
                HitTarget::CommitTableRow {
                    visible_row,
                    commit_oid,
                } => {
                    if shift {
                        return vec![
                            Action::FocusPane(PaneId::LogCommitTable),
                            Action::ExtendLogSelectionTo(visible_row),
                        ];
                    }
                    state.drag_state = Some(DragState::LogSelection);
                    let mut actions = vec![
                        Action::FocusPane(PaneId::LogCommitTable),
                        Action::SelectLogRow(visible_row),
                    ];
                    if is_double {
                        actions.push(Action::RunCliProbe(vec![
                            "show".into(),
                            "--stat".into(),
                            commit_oid.to_string(),
                        ]));
                    }
                    actions
                }
                HitTarget::ConsoleCommandRow { idx } => {
                    vec![
                        Action::FocusPane(PaneId::ConsoleCommandList),
                        Action::SelectConsoleRow(idx),
                    ]
                }
                HitTarget::ConsoleOutputArea => {
                    vec![Action::FocusPane(PaneId::ConsoleOutputDetail)]
                }
                HitTarget::Pane(pane) => vec![Action::FocusPane(pane)],
                other => {
                    if let Some(actions) = route_spec05_click(&other, state, is_double) {
                        return actions;
                    }
                    route_stage_left_click(other, state, col, row, ctrl, shift, is_double)
                        .unwrap_or_default()
                }
            }
        }
        MouseEventKind::Down(MouseButton::Right) => {
            let Some(region) = state.hit_registry.hit_test(col, row).cloned() else {
                return Vec::new();
            };

            if matches!(region.target, HitTarget::ModalBackdrop { .. }) {
                return vec![Action::PopModal];
            }
            if matches!(
                region.target,
                HitTarget::MenuItem { .. }
                    | HitTarget::MenuBody { .. }
                    | HitTarget::DialogBody
                    | HitTarget::DialogOption(_)
                    | HitTarget::DialogButton(_)
                    | HitTarget::DialogCheckbox
                    | HitTarget::BranchPopupStar(_)
                    | HitTarget::BranchPopupFetch
                    | HitTarget::BranchPopupSettings
            ) {
                return Vec::new();
            }
            if let HitTarget::BranchPopupRow(idx) = region.target {
                return route_spec05_click(&HitTarget::BranchPopupRow(idx), state, false)
                    .unwrap_or_default();
            }
            if matches!(region.target, HitTarget::BranchWidgetPill) {
                return vec![Action::OpenBranchesPopup {
                    query: String::new(),
                }];
            }

            let mut actions = Vec::new();
            let Some(menu_items) = build_menu_for(&region.target, state, &mut actions) else {
                return Vec::new();
            };
            if menu_items.is_empty() {
                return Vec::new();
            }
            if matches!(
                state.modal_stack.last(),
                Some(ModalLayer::ContextMenu { .. })
            ) {
                actions.insert(0, Action::PopContextMenus);
            }

            actions.push(Action::OpenContextMenu {
                anchor: (col, row),
                items: menu_items,
                depth: 0,
            });
            actions
        }
        _ => Vec::new(),
    }
}

/// Context-menu items for a right-clicked (or keyboard-focused) target.
fn build_menu_for(
    target: &HitTarget,
    state: &mut AppState,
    actions: &mut Vec<Action>,
) -> Option<Vec<ContextMenuItem>> {
    if let Some(items) = context_menu_for_target(target, state, actions) {
        return Some(items);
    }
    let items = match target {
        HitTarget::BranchTreeNode { row: idx, .. } => {
            let idx = *idx;
            if state.branches.selected != idx {
                actions.push(Action::FocusPane(PaneId::LogBranchesTree));
                actions.push(Action::SelectBranchRow(idx));
            }
            let node = state
                .branches
                .rows
                .get(idx)
                .map(|r| &state.branches.model.nodes[r.node]);
            let branch_name = node
                .and_then(|n| n.ref_name.clone())
                .unwrap_or_else(|| "HEAD".to_string());
            let is_leaf = node.is_some_and(|n| n.is_leaf());
            let check = |on: bool| if on { "✓" } else { " " };
            let settings = &state.branches.settings;
            let mut items = match node.map(|n| (n.kind, n.ref_name.clone())) {
                Some((jbgit_core::BranchNodeKind::Leaf(kind), Some(name))) => {
                    let mut b = crate::menus::branch_menu_items(state, &name, kind);
                    if !b.is_empty() {
                        b.push(ContextMenuItem::separator());
                    }
                    b
                }
                Some((jbgit_core::BranchNodeKind::Leaf(jbgit_core::RefKind::Head), None)) => {
                    let mut b =
                        crate::menus::branch_menu_items(state, "", jbgit_core::RefKind::Head);
                    b.push(ContextMenuItem::separator());
                    b
                }
                _ => Vec::new(),
            };
            let i18n = &state.i18n;
            let km = &state.config.keymap;
            let nav_sc = km.primary_key_for(
                &[crate::config::KeymapScope::Branch],
                &crate::keymap::KeyAction::NavigateBranch,
            );
            let fav_sc = km.primary_key_for(
                &[crate::config::KeymapScope::Branch],
                &crate::keymap::KeyAction::ToggleFavorite,
            );
            let refresh_sc = km.primary_key_for(
                &[crate::config::KeymapScope::Global],
                &crate::keymap::KeyAction::Refresh,
            );
            items.push(ContextMenuItem::action(
                if is_leaf {
                    i18n.t_fmt("menu.branch.show_only_branch", &[("name", &branch_name)])
                } else {
                    i18n.t("menu.branch.show_only_branches").to_string()
                },
                Some('f'),
                Some("Enter"),
                ContextMenuAction::BranchesApplyFilter,
            ));
            if is_leaf {
                items.push(ContextMenuItem::action(
                    i18n.t("menu.branch.navigate_to_tip"),
                    Some('n'),
                    nav_sc.as_deref(),
                    ContextMenuAction::BranchesNavigate,
                ));
                items.push(ContextMenuItem::action(
                    if node.is_some_and(|n| n.is_favorite) {
                        i18n.t("menu.branch.unmark_favorite")
                    } else {
                        i18n.t("menu.branch.mark_favorite")
                    },
                    Some('m'),
                    fav_sc.as_deref(),
                    ContextMenuAction::BranchesToggleFavorite,
                ));
                items.push(ContextMenuItem::action(
                    i18n.t_fmt("menu.branch.log_branch", &[("name", &branch_name)]),
                    Some('l'),
                    None,
                    ContextMenuAction::RunReadOnlyProbe(vec![
                        "log".into(),
                        "-n".into(),
                        "10".into(),
                        "--oneline".into(),
                        branch_name,
                    ]),
                ));
            }
            items.push(ContextMenuItem::submenu(
                i18n.t("menu.branch.view_options"),
                Some('o'),
                vec![
                    ContextMenuItem::action(
                        format!(
                            "{} {}",
                            check(settings.options.group_by_directory),
                            i18n.t("menu.branch.opt_group_by_dir")
                        ),
                        Some('d'),
                        None,
                        ContextMenuAction::BranchesToggleGroupByDir,
                    ),
                    ContextMenuItem::action(
                        format!(
                            "{} {}",
                            check(settings.options.show_tags),
                            i18n.t("menu.branch.opt_show_tags")
                        ),
                        Some('t'),
                        None,
                        ContextMenuAction::BranchesToggleShowTags,
                    ),
                    ContextMenuItem::action(
                        format!(
                            "{} {}",
                            check(settings.navigate_on_select),
                            i18n.t("menu.branch.opt_nav_on_select")
                        ),
                        Some('n'),
                        None,
                        ContextMenuAction::BranchesToggleNavigateOnSelect,
                    ),
                ],
            ));
            items.push(ContextMenuItem::action(
                i18n.t("menu.branch.refresh_repo"),
                Some('r'),
                refresh_sc.as_deref(),
                ContextMenuAction::RefreshRepo,
            ));
            items
        }
        HitTarget::ChangesTreeNode { node_id, .. } => {
            let mut items = stage_tree_context_menu_items(state, node_id.0 as usize, actions);
            items.push(ContextMenuItem::submenu(
                state.i18n.t("menu.stage.git_probes"),
                Some('p'),
                probe_submenu_items(&state.i18n),
            ));
            items
        }
        HitTarget::ConsoleCommandRow { idx } => {
            if state.console_selected_idx != *idx {
                actions.push(Action::FocusPane(PaneId::ConsoleCommandList));
                actions.push(Action::SelectConsoleRow(*idx));
            }
            probe_submenu_items(&state.i18n)
        }
        _ => return None,
    };
    Some(items)
}

fn probe_submenu_items(i18n: &crate::i18n::I18n) -> Vec<ContextMenuItem> {
    vec![
        ContextMenuItem::action(
            i18n.t("menu.stage.probe_status"),
            Some('s'),
            None,
            ContextMenuAction::RunReadOnlyProbe(vec![
                "status".into(),
                "--short".into(),
                "--branch".into(),
            ]),
        ),
        ContextMenuItem::action(
            i18n.t("menu.stage.probe_branch"),
            Some('b'),
            None,
            ContextMenuAction::RunReadOnlyProbe(vec!["branch".into(), "-vv".into()]),
        ),
        ContextMenuItem::action(
            i18n.t("menu.stage.probe_log"),
            Some('l'),
            None,
            ContextMenuAction::RunReadOnlyProbe(vec![
                "log".into(),
                "-n".into(),
                "10".into(),
                "--oneline".into(),
            ]),
        ),
    ]
}

pub(crate) fn resolve_menu_action(act: ContextMenuAction) -> Vec<Action> {
    match act {
        ContextMenuAction::Run(actions) => actions,
        ContextMenuAction::RefreshRepo => vec![Action::RefreshAll],
        ContextMenuAction::RunReadOnlyProbe(args) => vec![Action::RunCliProbe(args)],
        ContextMenuAction::TogglePresentation(t) => vec![Action::TogglePresentation(t)],
        ContextMenuAction::SetGraphRenderer(mode) => vec![Action::SetGraphRenderer(mode)],
        ContextMenuAction::BranchesApplyFilter => vec![Action::BranchesApplyFilter],
        ContextMenuAction::BranchesNavigate => vec![Action::BranchesNavigate],
        ContextMenuAction::BranchesToggleFavorite => vec![Action::BranchesToggleFavorite],
        ContextMenuAction::BranchesToggleGroupByDir => vec![Action::BranchesToggleGroupByDir],
        ContextMenuAction::BranchesToggleShowTags => vec![Action::BranchesToggleShowTags],
        ContextMenuAction::BranchesToggleNavigateOnSelect => {
            vec![Action::BranchesToggleNavigateOnSelect]
        }
        ContextMenuAction::OpenGoTo => vec![Action::OpenPopup {
            kind: PopupKind::GoToRevision,
            initial_query: String::new(),
        }],
        ContextMenuAction::StageSelected => vec![Action::StageActionSelected],
        ContextMenuAction::UnstageSelected => vec![Action::UnstageActionSelected],
        ContextMenuAction::StageAllTracked => vec![Action::StageAllTracked],
        ContextMenuAction::DiscardSelected => vec![Action::DiscardPromptSelected],
        ContextMenuAction::StageToggleGroupByDir => vec![Action::StageToggleGroupByDir],
        ContextMenuAction::StageToggleShowIgnored => vec![Action::StageToggleShowIgnored],
        ContextMenuAction::StageExpandAll(expand) => vec![Action::StageExpandAll(expand)],
        ContextMenuAction::OpenExternalEditorFile(p) => {
            vec![Action::OpenExternalEditorFile(Some(p))]
        }
    }
}

/// `[⚙ Presentation]` menu (`Vcs.Log.PresentationSettings`).
pub fn presentation_menu_items(state: &AppState) -> Vec<ContextMenuItem> {
    use jbgit_core::graph::SortType;
    let check = |on: bool| if on { "✓" } else { " " };
    let p = &state.presentation;
    let o = &state.log_graph.options;
    let i18n = &state.i18n;
    let item =
        |on: bool, key: &str, mnemonic: char, shortcut: Option<&str>, t: PresentationToggle| {
            ContextMenuItem::action(
                format!("{} {}", check(on), i18n.t(key)),
                Some(mnemonic),
                shortcut,
                ContextMenuAction::TogglePresentation(t),
            )
        };
    vec![
        item(
            state.compact_refs,
            "presentation.compact_refs",
            'c',
            None,
            PresentationToggle::CompactRefs,
        ),
        item(
            state.show_tag_names,
            "presentation.show_tag_names",
            't',
            None,
            PresentationToggle::ShowTagNames,
        ),
        item(
            o.show_long_edges,
            "presentation.show_long_edges",
            'l',
            None,
            PresentationToggle::ShowLongEdges,
        ),
        item(
            o.sort_type == SortType::IntelliSort,
            "presentation.intelli_sort",
            'i',
            None,
            PresentationToggle::IntelliSort,
        ),
        item(
            o.first_parent_only,
            "presentation.first_parent_only",
            'f',
            None,
            PresentationToggle::FirstParentOnly,
        ),
        item(
            p.highlight_current_branch,
            "presentation.highlight_current_branch",
            'b',
            None,
            PresentationToggle::HighlightCurrentBranch,
        ),
        item(
            p.highlight_merges,
            "presentation.highlight_merges",
            'm',
            None,
            PresentationToggle::HighlightMerges,
        ),
        item(
            p.show_hash_column,
            "presentation.show_hash_column",
            'h',
            None,
            PresentationToggle::ShowHashColumn,
        ),
        item(
            p.relative_dates,
            "presentation.relative_dates",
            'r',
            None,
            PresentationToggle::RelativeDates,
        ),
        item(
            p.use_author_date,
            "presentation.use_author_date",
            'u',
            None,
            PresentationToggle::UseAuthorDate,
        ),
        ContextMenuItem::separator(),
        ContextMenuItem::submenu(
            i18n.t_fmt(
                "presentation.graph_renderer",
                &[("mode", i18n.graph_renderer_label(state.graph_renderer.mode()))],
            ),
            Some('g'),
            GraphRendererMode::ALL
                .into_iter()
                .map(|mode| {
                    let on = state.graph_renderer.mode() == mode;
                    ContextMenuItem::action(
                        format!("{} {}", if on { "●" } else { "○" }, i18n.graph_renderer_label(mode)),
                        mode.label().chars().next().map(|c| c.to_ascii_lowercase()),
                        (mode == state.graph_renderer.mode().next()).then_some("F8"),
                        ContextMenuAction::SetGraphRenderer(mode),
                    )
                })
                .collect(),
        ),
    ]
}
