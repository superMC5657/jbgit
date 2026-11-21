use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use jbgit_core::{ChangeRowKind, ModalButton, PaneId, WorkspaceTab};

use crate::app::{Action, AppState, ModalLayer, PopupKind};
use crate::input_menus::{
    route_conflict_merge_key, route_context_menu_key, route_dialog,
};
use crate::input_stage::{route_commit_changes_tree_key, route_commit_priority_key};
use crate::log_view::{complete_last_token, filter_suggestions};

use super::keyboard_context_menu;

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

pub(crate) fn route_key(key: KeyEvent, state: &mut AppState) -> Vec<Action> {
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

    if !state.modal_stack.is_empty() {
        return route_modal_key(key, state);
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

    if state.active_tab == WorkspaceTab::Console
        && state.focused_pane == PaneId::ConsoleCommandList
        && state.pending_keys.is_empty()
        && key.code == KeyCode::Char(crate::keymap::actions::DEFAULT_CONSOLE_PROBE_CHAR)
        && !key.modifiers.contains(KeyModifiers::CONTROL)
        && !key.modifiers.contains(KeyModifiers::ALT)
        && state
            .config
            .keymap
            .primary_key_for(
                &[crate::config::KeymapScope::Global],
                &crate::keymap::KeyAction::GitStatusProbe,
            )
            .is_none()
    {
        return vec![crate::keymap::actions::git_status_probe_action()];
    }

    if let Some(actions) = crate::keymap::route_dynamic_key(&key, state) {
        return actions;
    }

    route_pane_fallback_key(key, state)
}

fn route_modal_key(key: KeyEvent, state: &mut AppState) -> Vec<Action> {
    let Some(top) = state.modal_stack.last_mut() else {
        return Vec::new();
    };
    match top {
        ModalLayer::CriticalDialog {
            selected_button, ..
        } => match key.code {
            KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('q') => vec![Action::PopModal],
            KeyCode::Left | KeyCode::Right | KeyCode::Tab => {
                *selected_button = selected_button.toggle();
                state.needs_redraw = true;
                Vec::new()
            }
            KeyCode::Enter | KeyCode::Char('y') => {
                if *selected_button == ModalButton::Confirm || key.code == KeyCode::Char('y') {
                    vec![Action::ConfirmCriticalDialog]
                } else {
                    vec![Action::PopModal]
                }
            }
            _ => Vec::new(),
        },
        ModalLayer::ContextMenu { .. } => route_context_menu_key(key, state),
        ModalLayer::Dialog(_) => route_dialog(&key, state),
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
            if kind == PopupKind::FileHistory {
                return route_file_history_key(&key, state);
            }
            match key.code {
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
            }
        }
    }
}

fn route_pane_fallback_key(key: KeyEvent, state: &mut AppState) -> Vec<Action> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    let in_log = state.active_tab == WorkspaceTab::Log;

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
                let row = state
                    .details
                    .change_rows()
                    .get(state.details.changes_selected)
                    .map(|r| (matches!(r.kind, ChangeRowKind::Dir { .. }), r.expanded));
                match row {
                    Some((true, expanded)) => vec![Action::ChangesExpand(!expanded)],
                    Some((false, _)) => {
                        let mut acts = Vec::new();
                        if !state.details.diff_preview {
                            acts.push(Action::ToggleDiffPreview);
                        }
                        acts.push(Action::FocusPane(PaneId::LogDetailsDiff));
                        acts
                    }
                    None => Vec::new(),
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
            KeyCode::PageDown => vec![Action::ScrollConsoleList(10)],
            KeyCode::PageUp => vec![Action::ScrollConsoleList(-10)],
            KeyCode::Enter => vec![Action::FocusPane(PaneId::ConsoleOutputDetail)],
            _ => Vec::new(),
        },
        PaneId::ConsoleOutputDetail => {
            let page_step = state.console_page_step();
            match key.code {
                KeyCode::PageDown => vec![Action::ScrollConsoleOutput(page_step)],
                KeyCode::PageUp => vec![Action::ScrollConsoleOutput(-page_step)],
                KeyCode::Esc => vec![Action::FocusPane(PaneId::ConsoleCommandList)],
                _ => Vec::new(),
            }
        }
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
        KeyCode::Home => vec![Action::ScrollDiff(crate::app::SCROLL_GOTO_TOP)],
        KeyCode::End => vec![Action::ScrollDiff(crate::app::SCROLL_GOTO_BOTTOM)],
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
            && crate::keymap::actions::is_branch_speed_search_char(c)
        {
            return Some(vec![Action::BranchesSearchInput(c)]);
        }
    }
    None
}

fn route_file_history_key(key: &KeyEvent, state: &mut AppState) -> Vec<Action> {
    let Some(fh) = state.file_history.as_mut() else {
        return vec![Action::FileHistoryEscape];
    };
    let pending_g = fh
        .pending_g
        .take()
        .is_some_and(|t| t.elapsed() <= crate::log_view::FILE_HISTORY_G_WINDOW);
    let diff_zoomed = fh.diff_zoomed;
    let diff_focused = fh.focus == crate::log_view::FileHistoryFocus::Diff || diff_zoomed;

    if key.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) {
        return match key.code {
            KeyCode::Esc => vec![Action::FileHistoryEscape],
            KeyCode::Tab | KeyCode::BackTab => vec![Action::FileHistoryToggleFocus],
            _ => Vec::new(),
        };
    }

    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    if pending_g {
        if key.code == KeyCode::Esc {
            state.needs_redraw = true;
            return Vec::new();
        }
        if key.code == KeyCode::Char('g') && !shift {
            return if diff_focused {
                vec![Action::FileHistoryScrollDiff(crate::app::SCROLL_GOTO_TOP)]
            } else {
                vec![Action::FileHistorySelect {
                    idx: 0,
                    extend: false,
                }]
            };
        }
    }

    match key.code {
        KeyCode::Esc => vec![Action::FileHistoryEscape],
        KeyCode::Tab | KeyCode::BackTab => vec![Action::FileHistoryToggleFocus],
        KeyCode::Enter => vec![Action::FileHistoryJumpToLog],
        KeyCode::Char('g') if !shift => {
            if let Some(fh) = state.file_history.as_mut() {
                fh.pending_g = Some(std::time::Instant::now());
            }
            state.needs_redraw = true;
            Vec::new()
        }
        KeyCode::Char('f') => vec![Action::FileHistoryToggleFollow],
        KeyCode::Char('l') => vec![Action::FileHistoryToggleCompareLocal],
        KeyCode::Char('S') => vec![Action::FileHistoryToggleSideBySide],
        KeyCode::Char('w') => vec![Action::FileHistoryCycleContext],
        KeyCode::Char('z') | KeyCode::F(4) => vec![Action::FileHistoryToggleZoom],
        KeyCode::Up if shift && !diff_zoomed => {
            vec![Action::FileHistoryMove {
                delta: -1,
                extend: true,
            }]
        }
        KeyCode::Down if shift && !diff_zoomed => {
            vec![Action::FileHistoryMove {
                delta: 1,
                extend: true,
            }]
        }
        KeyCode::Down | KeyCode::Char('j') if diff_focused => {
            vec![Action::FileHistoryScrollDiff(1)]
        }
        KeyCode::Up | KeyCode::Char('k') if diff_focused => {
            vec![Action::FileHistoryScrollDiff(-1)]
        }
        KeyCode::PageDown if diff_focused => {
            vec![Action::FileHistoryScrollDiff(10)]
        }
        KeyCode::PageUp if diff_focused => vec![Action::FileHistoryScrollDiff(-10)],
        KeyCode::Home if diff_focused => {
            vec![Action::FileHistoryScrollDiff(crate::app::SCROLL_GOTO_TOP)]
        }
        KeyCode::End | KeyCode::Char('G') if diff_focused => {
            vec![Action::FileHistoryScrollDiff(crate::app::SCROLL_GOTO_BOTTOM)]
        }
        KeyCode::Down | KeyCode::Char('j') if !diff_zoomed => {
            vec![Action::FileHistoryMove {
                delta: 1,
                extend: false,
            }]
        }
        KeyCode::Up | KeyCode::Char('k') if !diff_zoomed => {
            vec![Action::FileHistoryMove {
                delta: -1,
                extend: false,
            }]
        }
        KeyCode::PageDown if !diff_zoomed => {
            vec![Action::FileHistoryMove {
                delta: 10,
                extend: false,
            }]
        }
        KeyCode::PageUp if !diff_zoomed => {
            vec![Action::FileHistoryMove {
                delta: -10,
                extend: false,
            }]
        }
        KeyCode::Home if !diff_zoomed => {
            vec![Action::FileHistorySelect {
                idx: 0,
                extend: false,
            }]
        }
        KeyCode::End | KeyCode::Char('G') if !diff_zoomed => {
            vec![Action::FileHistorySelect {
                idx: usize::MAX,
                extend: false,
            }]
        }
        _ => Vec::new(),
    }
}
