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
    route_stash_key,
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
    if key.code == KeyCode::F(12) {
        return vec![Action::ToggleGlyphMode];
    }
    if key.code == KeyCode::F(8) {
        return vec![Action::CycleGraphRenderer];
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
                return match key.code {
                    KeyCode::Esc => vec![Action::PopModal],
                    KeyCode::Char('?') if kind == PopupKind::ShortcutsHelp => {
                        vec![Action::PopModal]
                    }
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

    if let Some(actions) = route_stash_key(&key, state) {
        return actions;
    }

    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let in_log = state.active_tab == WorkspaceTab::Log;

    // Keyboard-equivalent context menu: Menu key, Shift+F10, `.` or `m`.
    let menu_trigger = key.code == KeyCode::Menu
        || (key.code == KeyCode::F(10) && key.modifiers.contains(KeyModifiers::SHIFT))
        || (!ctrl && !alt && key.code == KeyCode::Char('.'))
        || (!ctrl
            && !alt
            && key.code == KeyCode::Char('m')
            && state.focused_pane != PaneId::LogDetailsDiff);
    if menu_trigger && let Some(actions) = keyboard_context_menu(state) {
        return actions;
    }

    // `GitBranchesPopup`: Ctrl+` / Ctrl+Shift+` (~) or Alt+B outside the Log tab
    // (where Alt+B keeps collapsing the Branches pane).
    if (ctrl && matches!(key.code, KeyCode::Char('`' | '~')))
        || (alt && !in_log && matches!(key.code, KeyCode::Char('b' | 'B')))
    {
        return vec![Action::OpenBranchesPopup {
            query: String::new(),
        }];
    }
    if ctrl && alt && matches!(key.code, KeyCode::Char('n' | 'N')) {
        return vec![Action::OpenDialog(Box::new(
            crate::dialogs::new_branch_from_head_dialog(
                state.repo_snapshot.as_deref(),
                &state.i18n,
            ),
        ))];
    }

    // Layer 0: Dynamic Keymap (from keymap.toml)
    if let Some(actions) = crate::keymap::route_dynamic_key(&key, state) {
        return actions;
    }

    // Layer 1: Workspace global shortcuts fallback
    match key.code {
        KeyCode::Char('g') if ctrl => {
            return if in_log {
                vec![Action::OpenPopup {
                    kind: PopupKind::GoToRevision,
                    initial_query: String::new(),
                }]
            } else {
                vec![Action::ToggleGlyphMode]
            };
        }
        KeyCode::Char('b') if alt && in_log => return vec![Action::ToggleBranchesPane],
        KeyCode::Char('l') if ctrl && in_log => return vec![Action::FocusFilterText],
        KeyCode::Char('/') if in_log => return vec![Action::FocusFilterText],
        KeyCode::F(9) => {
            return vec![Action::OpenPopup {
                kind: PopupKind::PalettePreview,
                initial_query: String::new(),
            }];
        }
        KeyCode::Char(d @ ('1' | '2' | '3' | '4')) if !ctrl && !alt => {
            if let Some(tab) = WorkspaceTab::from_digit(d) {
                return vec![Action::SwitchTab(tab)];
            }
        }
        KeyCode::Tab => return vec![Action::CycleFocus { reverse: false }],
        KeyCode::BackTab => return vec![Action::CycleFocus { reverse: true }],
        KeyCode::Char('p') if !ctrl && !alt => {
            return vec![Action::RunCliProbe(vec![
                "status".into(),
                "--short".into(),
                "--branch".into(),
            ])];
        }
        _ => {}
    }

    let shift = key.modifiers.contains(KeyModifiers::SHIFT);

    // Focused pane navigation
    match state.focused_pane {
        PaneId::LogBranchesTree => match key.code {
            KeyCode::Down => vec![Action::ScrollBranches(1)],
            KeyCode::Up => vec![Action::ScrollBranches(-1)],
            KeyCode::PageDown => vec![Action::ScrollBranches(10)],
            KeyCode::PageUp => vec![Action::ScrollBranches(-10)],
            KeyCode::Home => vec![Action::SelectBranchRow(0)],
            KeyCode::End => vec![Action::SelectBranchRow(usize::MAX)],
            KeyCode::Right => vec![Action::BranchesExpand(true)],
            KeyCode::Left => vec![Action::BranchesExpand(false)],
            KeyCode::Enter => vec![Action::BranchesApplyFilter],
            KeyCode::Char('J') => vec![Action::BranchesNavigate],
            KeyCode::Char(' ') => vec![Action::BranchesToggleFavorite],
            KeyCode::Esc => vec![Action::BranchesEscape],
            _ => Vec::new(),
        },
        PaneId::LogCommitTable => match key.code {
            KeyCode::Down if shift => vec![Action::ExtendLogSelection(1)],
            KeyCode::Up if shift => vec![Action::ExtendLogSelection(-1)],
            KeyCode::Down | KeyCode::Char('j') => vec![Action::ScrollLog(1)],
            KeyCode::Up | KeyCode::Char('k') => vec![Action::ScrollLog(-1)],
            KeyCode::PageDown => vec![Action::LogPage {
                half: false,
                down: true,
            }],
            KeyCode::PageUp => vec![Action::LogPage {
                half: false,
                down: false,
            }],
            KeyCode::Char('d') if ctrl => {
                let oids = state.selected_commits_oldest_first();
                if let [base, target] = oids.as_slice() {
                    vec![Action::CompareVersions {
                        base: *base,
                        target: *target,
                    }]
                } else {
                    vec![Action::LogPage {
                        half: true,
                        down: true,
                    }]
                }
            }
            KeyCode::Char('u') if ctrl => vec![Action::LogPage {
                half: true,
                down: false,
            }],
            KeyCode::Home | KeyCode::Char('g') => vec![Action::LogEdge { bottom: false }],
            KeyCode::End | KeyCode::Char('G') => vec![Action::LogEdge { bottom: true }],
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
            KeyCode::Char('P') => {
                let parents = state
                    .log_commit_at(state.log_graph.selected_idx)
                    .map_or(0, |c| c.parents.len());
                if parents >= 2 {
                    vec![Action::OpenPopup {
                        kind: PopupKind::ChooseParent,
                        initial_query: String::new(),
                    }]
                } else {
                    vec![Action::GoToParent(0)]
                }
            }
            KeyCode::Right => vec![Action::GoToParent(0)],
            KeyCode::Left => vec![Action::GoToChild],
            KeyCode::Enter => vec![Action::FocusPane(PaneId::LogChangesTree)],
            KeyCode::Esc if state.log_graph.anchor.is_some() => {
                vec![Action::SelectLogRow(state.log_graph.selected_idx)]
            }
            KeyCode::Char('c') => vec![Action::ToggleCompactRefs],
            KeyCode::Char('t') => vec![Action::ToggleTagNames],
            _ => Vec::new(),
        },
        PaneId::LogChangesTree => match key.code {
            KeyCode::Down | KeyCode::Char('j') => vec![Action::ChangesMove(1)],
            KeyCode::Up | KeyCode::Char('k') => vec![Action::ChangesMove(-1)],
            KeyCode::PageDown => vec![Action::ChangesMove(10)],
            KeyCode::PageUp => vec![Action::ChangesMove(-10)],
            KeyCode::Right => vec![Action::ChangesExpand(true)],
            KeyCode::Left => vec![Action::ChangesExpand(false)],
            KeyCode::Enter if ctrl => vec![Action::ZoomDiff(true)],
            KeyCode::F(4) => vec![Action::ZoomDiff(true)],
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
            KeyCode::Char('d') if ctrl => vec![Action::ToggleDiffPreview],
            KeyCode::Char('v') => vec![Action::ChangesToggleGroupByDir],
            KeyCode::Char('e') => vec![Action::ChangesExpandAll(true)],
            KeyCode::Char('E') => vec![Action::ChangesExpandAll(false)],
            KeyCode::Char('P') => vec![Action::SwitchMergeParent],
            _ => Vec::new(),
        },
        PaneId::LogDetailsDiff if state.details.diff_preview => {
            let page = state.log_graph.last_viewport_h.max(4) as isize / 2;
            route_diff_key(&key, page, false)
        }
        PaneId::LogDetailsDiff => match key.code {
            KeyCode::Down | KeyCode::Char('j') => vec![Action::ScrollDetails(1)],
            KeyCode::Up | KeyCode::Char('k') => vec![Action::ScrollDetails(-1)],
            KeyCode::PageDown => vec![Action::ScrollDetails(10)],
            KeyCode::PageUp => vec![Action::ScrollDetails(-10)],
            KeyCode::Char('d') if ctrl => vec![Action::ToggleDiffPreview],
            KeyCode::Char('P') => vec![Action::SwitchMergeParent],
            _ => Vec::new(),
        },
        PaneId::ConsoleCommandList => match key.code {
            KeyCode::Down | KeyCode::Char('j') => vec![Action::ScrollConsoleList(1)],
            KeyCode::Up | KeyCode::Char('k') => vec![Action::ScrollConsoleList(-1)],
            KeyCode::Enter => vec![Action::FocusPane(PaneId::ConsoleOutputDetail)],
            _ => Vec::new(),
        },
        PaneId::ConsoleOutputDetail => match key.code {
            KeyCode::Down | KeyCode::Char('j') => vec![Action::ScrollConsoleOutput(1)],
            KeyCode::Up | KeyCode::Char('k') => vec![Action::ScrollConsoleOutput(-1)],
            KeyCode::PageDown => vec![Action::ScrollConsoleOutput(10)],
            KeyCode::PageUp => vec![Action::ScrollConsoleOutput(-10)],
            _ => Vec::new(),
        },
        PaneId::CommitChangesTree => route_commit_changes_tree_key(&key, state),
        _ => Vec::new(),
    }
}

/// Shared keymap for both inline and full-screen zoomed diff viewers.
fn route_diff_key(key: &KeyEvent, page: isize, zoomed: bool) -> Vec<Action> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Down | KeyCode::Char('j') => vec![Action::ScrollDiff(1)],
        KeyCode::Up | KeyCode::Char('k') => vec![Action::ScrollDiff(-1)],
        KeyCode::PageDown => vec![Action::ScrollDiff(page)],
        KeyCode::Char(' ') if zoomed => vec![Action::ScrollDiff(page)],
        KeyCode::PageUp => vec![Action::ScrollDiff(-page)],
        KeyCode::Home | KeyCode::Char('g') => vec![Action::ScrollDiff(isize::MIN / 2)],
        KeyCode::Char('s') => vec![Action::DiffToggleSideBySide],
        KeyCode::Char('c') => vec![Action::DiffCycleContext],
        KeyCode::Char('m') if !zoomed => vec![Action::DiffCycleDock],
        KeyCode::Char('[') => vec![Action::DiffNextFile(-1)],
        KeyCode::Char(']') => vec![Action::DiffNextFile(1)],
        KeyCode::Char('P') => vec![Action::SwitchMergeParent],
        KeyCode::F(4) => vec![Action::ZoomDiff(!zoomed)],
        KeyCode::Enter if ctrl => vec![Action::ZoomDiff(!zoomed)],
        KeyCode::Esc => vec![if zoomed {
            Action::ZoomDiff(false)
        } else {
            Action::ToggleDiffPreview
        }],
        KeyCode::Char('d') if ctrl && !zoomed => vec![Action::ToggleDiffPreview],
        KeyCode::Char('q') if zoomed => vec![Action::Quit],
        _ => Vec::new(),
    }
}

/// Log-tab keys that must win over global shortcuts: zoomed diff, filter text
/// typing and Branches speed search.
fn route_log_priority_key(key: &KeyEvent, state: &AppState) -> Option<Vec<Action>> {
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

    if state.details.diff_zoomed {
        let page = state.terminal_area.height.max(6) as isize / 2;
        return Some(route_diff_key(key, page, true));
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
        let searching = !state.branches.query.is_empty();
        if searching {
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
        } else if let Some(c) = plain_char(key)
            && (c.is_alphabetic() || matches!(c, '-' | '_' | '/' | '.' | '0' | '5'..='9'))
            && c != 'J'
        {
            return Some(vec![Action::BranchesSearchInput(c)]);
        }
    }
    None
}

fn keyboard_context_menu(state: &mut AppState) -> Option<Vec<Action>> {
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
            items.push(ContextMenuItem::action(
                if is_leaf {
                    format!("Show only {branch_name} in Log")
                } else {
                    "Show only these branches in Log".to_string()
                },
                Some('f'),
                Some("Enter"),
                ContextMenuAction::BranchesApplyFilter,
            ));
            if is_leaf {
                items.push(ContextMenuItem::action(
                    "Navigate Log to Branch Tip",
                    Some('n'),
                    Some("J"),
                    ContextMenuAction::BranchesNavigate,
                ));
                items.push(ContextMenuItem::action(
                    if node.is_some_and(|n| n.is_favorite) {
                        "Unmark as Favorite"
                    } else {
                        "Mark as Favorite"
                    },
                    Some('m'),
                    Some("Space"),
                    ContextMenuAction::BranchesToggleFavorite,
                ));
                items.push(ContextMenuItem::action(
                    format!("Log {branch_name}"),
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
                "Branches View Options",
                Some('o'),
                vec![
                    ContextMenuItem::action(
                        format!(
                            "{} Group by Directory",
                            check(settings.options.group_by_directory)
                        ),
                        Some('d'),
                        None,
                        ContextMenuAction::BranchesToggleGroupByDir,
                    ),
                    ContextMenuItem::action(
                        format!("{} Show Tags", check(settings.options.show_tags)),
                        Some('t'),
                        None,
                        ContextMenuAction::BranchesToggleShowTags,
                    ),
                    ContextMenuItem::action(
                        format!(
                            "{} Navigate Log to Branch on Selection",
                            check(settings.navigate_on_select)
                        ),
                        Some('n'),
                        None,
                        ContextMenuAction::BranchesToggleNavigateOnSelect,
                    ),
                ],
            ));
            items.push(ContextMenuItem::action(
                "Refresh Repository",
                Some('r'),
                Some("r"),
                ContextMenuAction::RefreshRepo,
            ));
            items
        }
        HitTarget::ChangesTreeNode { node_id, .. } => {
            let mut items = stage_tree_context_menu_items(state, node_id.0 as usize, actions);
            items.push(ContextMenuItem::submenu(
                "Git Probes",
                Some('p'),
                probe_submenu_items(),
            ));
            items
        }
        HitTarget::ConsoleCommandRow { idx } => {
            if state.console_selected_idx != *idx {
                actions.push(Action::FocusPane(PaneId::ConsoleCommandList));
                actions.push(Action::SelectConsoleRow(*idx));
            }
            probe_submenu_items()
        }
        _ => return None,
    };
    Some(items)
}

fn probe_submenu_items() -> Vec<ContextMenuItem> {
    vec![
        ContextMenuItem::action(
            "Run git status --short --branch",
            Some('s'),
            Some("p"),
            ContextMenuAction::RunReadOnlyProbe(vec![
                "status".into(),
                "--short".into(),
                "--branch".into(),
            ]),
        ),
        ContextMenuItem::action(
            "Run git branch -vv",
            Some('b'),
            None,
            ContextMenuAction::RunReadOnlyProbe(vec!["branch".into(), "-vv".into()]),
        ),
        ContextMenuItem::action(
            "Run git log -n 10 --oneline",
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
    let item =
        |on: bool, label: &str, mnemonic: char, shortcut: Option<&str>, t: PresentationToggle| {
            ContextMenuItem::action(
                format!("{} {label}", check(on)),
                Some(mnemonic),
                shortcut,
                ContextMenuAction::TogglePresentation(t),
            )
        };
    vec![
        item(
            state.compact_refs,
            "Compact References View",
            'c',
            Some("c"),
            PresentationToggle::CompactRefs,
        ),
        item(
            state.show_tag_names,
            "Show Tag Names",
            't',
            Some("t"),
            PresentationToggle::ShowTagNames,
        ),
        item(
            o.show_long_edges,
            "Show Long Edges",
            'l',
            None,
            PresentationToggle::ShowLongEdges,
        ),
        item(
            o.sort_type == SortType::IntelliSort,
            "IntelliSort (Bek)",
            'i',
            None,
            PresentationToggle::IntelliSort,
        ),
        item(
            o.first_parent_only,
            "First Parent Only",
            'f',
            None,
            PresentationToggle::FirstParentOnly,
        ),
        item(
            p.highlight_current_branch,
            "Highlight: Current Branch",
            'b',
            None,
            PresentationToggle::HighlightCurrentBranch,
        ),
        item(
            p.highlight_merges,
            "Highlight: Merge Commits",
            'm',
            None,
            PresentationToggle::HighlightMerges,
        ),
        item(
            p.show_hash_column,
            "Show Hash Column",
            'h',
            None,
            PresentationToggle::ShowHashColumn,
        ),
        item(
            p.relative_dates,
            "Relative Dates",
            'r',
            None,
            PresentationToggle::RelativeDates,
        ),
        item(
            p.use_author_date,
            "Use Author Date",
            'u',
            None,
            PresentationToggle::UseAuthorDate,
        ),
        ContextMenuItem::separator(),
        ContextMenuItem::submenu(
            format!("Graph Renderer: {}", state.graph_renderer.mode().label()),
            Some('g'),
            GraphRendererMode::ALL
                .into_iter()
                .map(|mode| {
                    let on = state.graph_renderer.mode() == mode;
                    ContextMenuItem::action(
                        format!("{} {}", if on { "●" } else { "○" }, mode.label()),
                        mode.label().chars().next().map(|c| c.to_ascii_lowercase()),
                        (mode == state.graph_renderer.mode().next()).then_some("F8"),
                        ContextMenuAction::SetGraphRenderer(mode),
                    )
                })
                .collect(),
        ),
    ]
}
