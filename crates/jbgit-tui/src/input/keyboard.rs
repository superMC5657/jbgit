use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use jbgit_core::{ChangeRowKind, PaneId, WorkspaceTab};

use crate::app::{Action, AppState, ChangesHost, DiffScope, ModalLayer, PopupKind};
use crate::hit_test::HitTarget;
use crate::input_stage::{route_commit_changes_tree_key, route_commit_priority_key};
use crate::log_view::{complete_last_token, filter_suggestions_with_context};

use super::menus::{
    keyboard_context_menu, route_conflict_merge_key, route_conflict_queue_key,
    route_context_menu_key, route_stash_fallback_key,
};

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

pub(crate) fn route_diff_search_editing_key(key: &KeyEvent, scope: DiffScope) -> Vec<Action> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    match key.code {
        KeyCode::Esc => vec![Action::DiffSearchClose(scope)],
        KeyCode::Enter => vec![Action::DiffSearchConfirm(scope)],
        KeyCode::Backspace => vec![Action::DiffSearchBackspace(scope)],
        KeyCode::Char('u') if ctrl => vec![Action::DiffSearchClear(scope)],
        KeyCode::Down => vec![Action::DiffSearchJump(scope, 1)],
        KeyCode::Up => vec![Action::DiffSearchJump(scope, -1)],
        KeyCode::Char(c) if !ctrl && !alt => vec![Action::DiffSearchChar(scope, c)],
        _ => Vec::new(),
    }
}

pub(crate) fn route_key(key: KeyEvent, state: &AppState) -> Vec<Action> {
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

    if state.active_tab == WorkspaceTab::Stash
        && state.focused_pane == PaneId::StashDiffPreview
        && state.stash.diff().search.editing
    {
        return route_diff_search_editing_key(&key, DiffScope::Stash);
    }

    let clear_bracket = state.active_tab == WorkspaceTab::Commit
        && state.focused_pane == PaneId::CommitDiffPreview
        && state.stage_workbench.bracket_jump_at().is_some()
        && !matches!(key.code, KeyCode::Char(']') | KeyCode::Char('['));

    let mut prefix = Vec::new();
    if clear_bracket {
        prefix.push(Action::StageClearBracketJump);
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
        prefix.push(crate::keymap::actions::git_status_probe_action());
        return prefix;
    }

    if let Some(actions) = crate::keymap::route_dynamic_key(&key, state) {
        prefix.extend(actions);
        return prefix;
    }

    if !state.pending_keys.is_empty() {
        prefix.push(Action::ClearPendingKeySequence);
    }
    prefix.extend(route_pane_fallback_key(key, state));
    prefix
}

fn route_modal_key(key: KeyEvent, state: &AppState) -> Vec<Action> {
    let Some(top) = state.modal_stack.last() else {
        return Vec::new();
    };
    match top {
        ModalLayer::ContextMenu { .. } => route_context_menu_key(key, state),
        ModalLayer::Dialog(_) => vec![Action::DialogKey(key)],
        ModalLayer::FileHistory(_) => route_file_history_key(&key, state),
        ModalLayer::Reflog(_) => route_reflog_key(&key, state),
        ModalLayer::CommitMessage { .. } => match key.code {
            KeyCode::Esc | KeyCode::Char('q') => vec![Action::PopModal],
            KeyCode::Down | KeyCode::Char('j') => vec![Action::ScrollCommitMessageModal(1)],
            KeyCode::Up | KeyCode::Char('k') => vec![Action::ScrollCommitMessageModal(-1)],
            KeyCode::PageDown => vec![Action::ScrollCommitMessageModal(10)],
            KeyCode::PageUp => vec![Action::ScrollCommitMessageModal(-10)],
            KeyCode::Home | KeyCode::Char('g') => {
                vec![Action::ScrollCommitMessageModal(crate::app::SCROLL_GOTO_TOP)]
            }
            KeyCode::End | KeyCode::Char('G') => {
                vec![Action::ScrollCommitMessageModal(
                    crate::app::SCROLL_GOTO_BOTTOM,
                )]
            }
            _ => Vec::new(),
        },
        ModalLayer::Annotate(_) => route_annotate_key(&key, state),
        ModalLayer::PushDialog(_) => route_push_dialog_key(&key, state),
        ModalLayer::Remotes(_) => crate::remotes_view::route_remotes_key(&key, state),
        ModalLayer::BranchCleanup(_) => {
            crate::branch_cleanup::route_branch_cleanup_key(&key, state)
        }
        ModalLayer::RepoSwitcher(_) => crate::repo_switcher::route_repo_switcher_key(&key, state),
        ModalLayer::Welcome(_) => crate::repo_lifecycle::route_welcome_key(&key, state),
        ModalLayer::CloneRepo(_) => crate::repo_lifecycle::route_clone_repo_key(&key, state),
        ModalLayer::Worktrees(_) => crate::worktrees_view::route_worktrees_key(&key, state),
        ModalLayer::ApplyPatch(_) => crate::patch_view::route_apply_patch_key(&key, state),
        ModalLayer::TreeBrowser(_) => crate::tree_browser::route_tree_browser_key(&key, state),
        ModalLayer::ConflictResolver(_) => route_conflict_merge_key(&key, state),
        ModalLayer::ConflictQueue(_) => route_conflict_queue_key(&key),
        ModalLayer::ThreeWayInspector { .. } | ModalLayer::HookError { .. } => match key.code {
            KeyCode::Esc | KeyCode::Enter => vec![Action::PopModal],
            _ => Vec::new(),
        },
        ModalLayer::BranchesPopup { .. } => {
            crate::branches_popup::route_branches_popup_key(&key, state)
        }
        ModalLayer::ShortcutsHelp { .. } => route_shortcuts_help_key(&key, state),
        ModalLayer::PalettePreview => match key.code {
            KeyCode::Esc => vec![Action::PopModal],
            _ => Vec::new(),
        },
        ModalLayer::FilterEditor { .. }
        | ModalLayer::GoToRevision { .. }
        | ModalLayer::ChooseParent { .. }
        | ModalLayer::ChooseChild { .. }
        | ModalLayer::CommitHistory { .. } => {
            let kind = top.popup_kind().expect("popup kind");
            let (query_ref, selected_val) = top.popup_query_and_selected().unwrap_or(("", 0));
            let query = query_ref.to_string();
            let selected = selected_val;
            match key.code {
                KeyCode::Esc => vec![Action::PopModal],
                KeyCode::Enter => match kind {
                    PopupKind::FilterEditor(field) => vec![
                        Action::PopModal,
                        Action::ApplyFilterField {
                            field,
                            expr: query,
                        },
                    ],
                    PopupKind::GoToRevision if key.modifiers.contains(KeyModifiers::ALT) => {
                        vec![Action::PopModal, Action::BrowseRevisionExpr(query)]
                    }
                    PopupKind::GoToRevision => {
                        vec![Action::PopModal, Action::GoToRevision(query)]
                    }
                    PopupKind::ChooseParent => {
                        vec![Action::PopModal, Action::GoToParent(selected)]
                    }
                    PopupKind::ChooseChild => {
                        vec![Action::PopModal, Action::GoToChildCandidate(selected)]
                    }
                    PopupKind::CommitHistory => {
                        let q_low = query.to_ascii_lowercase();
                        let candidates = state
                            .stage_workbench
                            .commit_history_candidates(state.repo_snapshot.as_deref());
                        let mapped = candidates
                            .iter()
                            .enumerate()
                            .filter(|(_, m)| {
                                q_low.is_empty() || m.to_ascii_lowercase().contains(&q_low)
                            })
                            .nth(selected)
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
                    if (kind == PopupKind::ChooseParent || kind == PopupKind::ChooseChild)
                        && !key.modifiers.contains(KeyModifiers::CONTROL)
                        && !key.modifiers.contains(KeyModifiers::ALT) =>
                {
                    let idx = (d as usize) - ('1' as usize);
                    if kind == PopupKind::ChooseParent {
                        let pcount = state
                            .log
                            .graph
                            .parent_candidates(state.repo_snapshot.as_ref())
                            .len();
                        if idx < pcount {
                            vec![Action::PopModal, Action::GoToParent(idx)]
                        } else {
                            Vec::new()
                        }
                    } else {
                        let ccount = state
                            .log
                            .graph
                            .child_candidates(state.repo_snapshot.as_ref())
                            .len();
                        if idx < ccount {
                            vec![Action::PopModal, Action::GoToChildCandidate(idx)]
                        } else {
                            Vec::new()
                        }
                    }
                }
                KeyCode::Tab => {
                    if let PopupKind::FilterEditor(field) = kind {
                        let snap = state.repo_snapshot.as_deref();
                        let has_fav = !state
                            .log
                            .favorite_branch_names(state.repo_snapshot.as_ref())
                            .is_empty();
                        let hist = state.log.filter.history_for(field);
                        let suggestions =
                            filter_suggestions_with_context(field, snap, &query, has_fav, hist);
                        if let Some(s) = suggestions.get(selected) {
                            return vec![Action::PopupSetState {
                                query: complete_last_token(&query, s),
                                selected: 0,
                            }];
                        }
                    }
                    Vec::new()
                }
                KeyCode::Backspace => {
                    let mut next_q = query;
                    next_q.pop();
                    vec![Action::PopupSetState {
                        query: next_q,
                        selected: 0,
                    }]
                }
                KeyCode::Up => vec![Action::PopupSetState {
                    query,
                    selected: selected.saturating_sub(1),
                }],
                KeyCode::Down => vec![Action::PopupSetState {
                    query,
                    selected: selected.saturating_add(1),
                }],
                KeyCode::Char(ch)
                    if !key.modifiers.contains(KeyModifiers::CONTROL)
                        && !key.modifiers.contains(KeyModifiers::ALT) =>
                {
                    let mut next_q = query;
                    next_q.push(ch);
                    vec![Action::PopupSetState {
                        query: next_q,
                        selected: 0,
                    }]
                }
                _ => Vec::new(),
            }
        }
    }
}

fn route_pane_fallback_key(key: KeyEvent, state: &AppState) -> Vec<Action> {
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
            KeyCode::Down if ctrl => vec![Action::MoveLogCursorOnly(1)],
            KeyCode::Up if ctrl => vec![Action::MoveLogCursorOnly(-1)],
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
                    .log_commit_at(state.log.graph.selected_idx)
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
            KeyCode::Right => vec![Action::GoToParentSmart],
            KeyCode::Left => vec![Action::GoToChild],
            KeyCode::Enter
                if state.log.graph.is_cursor_on_load_more()
                    || (state.log.graph.visible_count() == 0
                        && state
                            .repo_snapshot
                            .as_ref()
                            .is_some_and(|s| s.has_more_commits)) =>
            {
                vec![Action::LoadMoreCommits]
            }
            KeyCode::Enter
                if state
                    .log
                    .graph
                    .cached
                    .as_ref()
                    .and_then(|c| c.visible.bek_merge_at_row(state.log.graph.selected_idx))
                    .is_some() =>
            {
                let oid = state
                    .log
                    .graph
                    .cached
                    .as_ref()
                    .and_then(|c| c.visible.bek_merge_at_row(state.log.graph.selected_idx))
                    .expect("bek merge oid");
                vec![Action::GraphExpandBekMerge(oid)]
            }
            KeyCode::Enter if state.log.details.many_changes_blocked.is_some() => vec![
                Action::FocusPane(PaneId::LogChangesTree),
                Action::ConfirmLoadManyChanges,
            ],
            KeyCode::Enter => vec![Action::FocusPane(PaneId::LogChangesTree)],
            KeyCode::Esc if state.log.graph.has_multi_selection() => {
                vec![Action::ClearLogMultiSelection]
            }
            KeyCode::Esc if state.log.filter.saved_before_dir_history.is_some() => {
                vec![Action::RestorePreDirHistoryFilter]
            }
            _ => Vec::new(),
        },
        PaneId::LogChangesTree => match key.code {
            KeyCode::PageDown => vec![Action::ChangesMove(ChangesHost::Log, 10)],
            KeyCode::PageUp => vec![Action::ChangesMove(ChangesHost::Log, -10)],
            KeyCode::Right => vec![Action::ChangesExpand(ChangesHost::Log, true)],
            KeyCode::Left => vec![Action::ChangesExpand(ChangesHost::Log, false)],
            KeyCode::Enter if state.log.details.many_changes_blocked.is_some() => {
                vec![Action::ConfirmLoadManyChanges]
            }
            KeyCode::Enter if ctrl => vec![Action::ZoomDiff(true)],
            KeyCode::Enter => {
                let row = state
                    .log
                    .details
                    .change_rows(&state.diff_caches)
                    .get(state.log.details.changes.selected)
                    .map(|r| (matches!(r.kind, ChangeRowKind::Dir { .. }), r.expanded));
                match row {
                    Some((true, expanded)) => vec![Action::ChangesExpand(ChangesHost::Log, !expanded)],
                    Some((false, _)) => {
                        let mut acts = Vec::new();
                        if !state.log.details.diff_preview {
                            acts.push(Action::ToggleDiffPreview);
                        }
                        acts.push(Action::FocusPane(PaneId::LogDetailsDiff));
                        acts
                    }
                    None => Vec::new(),
                }
            }
            KeyCode::Esc => vec![Action::FocusPane(PaneId::LogCommitTable)],
            KeyCode::Char('v') if !ctrl && !alt => {
                vec![Action::ChangesToggleGroupByDir(ChangesHost::Log)]
            }
            KeyCode::Char('e') if !ctrl && !alt => vec![Action::ChangesExpandAll(ChangesHost::Log, true)],
            KeyCode::Char('E') if !ctrl && !alt => vec![Action::ChangesExpandAll(ChangesHost::Log, false)],
            _ => Vec::new(),
        },
        PaneId::LogDetailsDiff if state.log.details.diff_preview => {
            let page = state.log.graph.last_viewport_h.max(4) as isize / 2;
            match key.code {
                KeyCode::PageDown => vec![Action::DiffScroll(DiffScope::Log, page)],
                KeyCode::PageUp => vec![Action::DiffScroll(DiffScope::Log, -page)],
                KeyCode::Left if shift => vec![Action::DiffScrollHoriz(DiffScope::Log, -16)],
                KeyCode::Right if shift => vec![Action::DiffScrollHoriz(DiffScope::Log, 16)],
                KeyCode::Enter if ctrl => vec![Action::ZoomDiff(true)],
                KeyCode::Esc if state.log.details.diff.search.active => {
                    vec![Action::DiffSearchClose(DiffScope::Log)]
                }
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
        PaneId::StashList | PaneId::StashDiffPreview => route_stash_fallback_key(&key, state),
        _ => Vec::new(),
    }
}

fn route_shortcuts_help_key(key: &KeyEvent, state: &AppState) -> Vec<Action> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let (query, selected) = match state.modal_stack.last() {
        Some(ModalLayer::ShortcutsHelp { query, selected }) => (query.clone(), *selected),
        _ => return Vec::new(),
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
            vec![Action::PopupSetState {
                query,
                selected: next,
            }]
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
            vec![Action::PopupSetState {
                query: String::new(),
                selected: first,
            }]
        }
        KeyCode::Backspace => {
            let mut q = query;
            q.pop();
            let rows = crate::keymap::build_help_rows(state, &q);
            let first = crate::keymap::first_help_entry(&rows);
            vec![Action::PopupSetState {
                query: q,
                selected: first,
            }]
        }
        KeyCode::Char(ch) if !ctrl && !alt => {
            let mut q = query;
            q.push(ch);
            let rows = crate::keymap::build_help_rows(state, &q);
            let first = crate::keymap::first_help_entry(&rows);
            vec![Action::PopupSetState {
                query: q,
                selected: first,
            }]
        }
        _ => Vec::new(),
    }
}

/// Fallback keymap for full-screen zoomed diff viewer (after dynamic keymap).
fn route_diff_zoomed_fallback(key: &KeyEvent, page: isize, search_active: bool) -> Vec<Action> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    match key.code {
        KeyCode::PageDown => vec![Action::DiffScroll(DiffScope::Log, page)],
        KeyCode::Char(' ') => vec![Action::DiffScroll(DiffScope::Log, page)],
        KeyCode::PageUp => vec![Action::DiffScroll(DiffScope::Log, -page)],
        KeyCode::Left if shift => vec![Action::DiffScrollHoriz(DiffScope::Log, -16)],
        KeyCode::Right if shift => vec![Action::DiffScrollHoriz(DiffScope::Log, 16)],
        KeyCode::Home => vec![Action::DiffScroll(DiffScope::Log, crate::app::SCROLL_GOTO_TOP)],
        KeyCode::End => vec![Action::DiffScroll(DiffScope::Log, crate::app::SCROLL_GOTO_BOTTOM)],
        KeyCode::Char('P') => vec![Action::SwitchMergeParent],
        KeyCode::Enter if ctrl => vec![Action::ZoomDiff(false)],
        KeyCode::Esc if search_active => vec![Action::DiffSearchClose(DiffScope::Log)],
        KeyCode::Esc => vec![Action::ZoomDiff(false)],
        _ => Vec::new(),
    }
}

/// Log-tab keys that must win over global shortcuts: zoomed diff, filter text
/// typing and Branches search mode.
fn route_log_priority_key(key: &KeyEvent, state: &AppState) -> Option<Vec<Action>> {
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

    if state.log.details.diff.search.editing
        && (state.log.details.diff.zoomed
            || (state.focused_pane == PaneId::LogDetailsDiff && state.log.details.diff_preview))
    {
        return Some(route_diff_search_editing_key(key, DiffScope::Log));
    }

    if state.log.details.diff.zoomed {
        if let Some(actions) = crate::keymap::route_dynamic_key(key, state) {
            return Some(actions);
        }
        let mut out = Vec::new();
        if !state.pending_keys.is_empty() {
            out.push(Action::ClearPendingKeySequence);
        }
        let page = state.terminal_area.height.max(6) as isize / 2;
        out.extend(route_diff_zoomed_fallback(
            key,
            page,
            state.log.details.diff.search.active,
        ));
        return Some(out);
    }

    if state.log.filter.editing_text {
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

    if state.log.speed_search.active {
        if state.log.speed_search.editing {
            return Some(match key.code {
                KeyCode::Esc => vec![Action::LogSpeedSearchClose],
                KeyCode::Enter => vec![Action::LogSpeedSearchConfirm],
                KeyCode::Down => vec![Action::LogSpeedSearchStep(true)],
                KeyCode::Up => vec![Action::LogSpeedSearchStep(false)],
                KeyCode::Backspace => vec![Action::LogSpeedSearchBackspace],
                _ => match plain_char(key) {
                    Some(c) => vec![Action::LogSpeedSearchInput(c)],
                    None => Vec::new(),
                },
            });
        } else if state.focused_pane == PaneId::LogCommitTable && !ctrl && !alt {
            match key.code {
                KeyCode::Char('n') => return Some(vec![Action::LogSpeedSearchStep(true)]),
                KeyCode::Char('N') => return Some(vec![Action::LogSpeedSearchStep(false)]),
                KeyCode::Esc => return Some(vec![Action::LogSpeedSearchClose]),
                _ => {}
            }
        }
    }

    if state.focused_pane == PaneId::LogBranchesTree {
        let active_search = state.log.branches.searching;
        let speed_active = state.config.keymap.tree_speed_search && !state.log.branches.query.is_empty();
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

fn route_file_history_key(key: &KeyEvent, state: &AppState) -> Vec<Action> {
    let Some(fh) = state.modal_stack.file_history() else {
        return vec![Action::FileHistoryEscape];
    };
    if fh.show_repo_version {
        return match key.code {
            KeyCode::Esc | KeyCode::Char('v') => vec![Action::FileHistoryCloseRepoVersion],
            KeyCode::Down | KeyCode::Char('j') => vec![Action::FileHistoryBlobScroll(1)],
            KeyCode::Up | KeyCode::Char('k') => vec![Action::FileHistoryBlobScroll(-1)],
            KeyCode::PageDown => vec![Action::FileHistoryBlobScroll(15)],
            KeyCode::PageUp => vec![Action::FileHistoryBlobScroll(-15)],
            KeyCode::Home => vec![Action::FileHistoryBlobScroll(crate::app::SCROLL_GOTO_TOP)],
            KeyCode::End | KeyCode::Char('G') => {
                vec![Action::FileHistoryBlobScroll(crate::app::SCROLL_GOTO_BOTTOM)]
            }
            _ => Vec::new(),
        };
    }
    if fh.diff.search.editing {
        return route_diff_search_editing_key(key, DiffScope::FileHistory);
    }
    if fh.search_editing {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        return match key.code {
            KeyCode::Esc => vec![Action::FileHistorySearchClose],
            KeyCode::Enter => vec![Action::FileHistorySearchConfirm],
            KeyCode::Backspace => vec![Action::FileHistorySearchBackspace],
            KeyCode::Char('u') if ctrl => vec![Action::FileHistorySearchClear],
            KeyCode::Down => vec![Action::FileHistoryMove {
                delta: 1,
                extend: false,
            }],
            KeyCode::Up => vec![Action::FileHistoryMove {
                delta: -1,
                extend: false,
            }],
            KeyCode::Char(c) if !ctrl && !key.modifiers.contains(KeyModifiers::ALT) => {
                vec![Action::FileHistorySearchChar(c)]
            }
            _ => Vec::new(),
        };
    }
    let had_pending_g = fh.pending_g.is_some();
    let pending_g = fh
        .pending_g
        .is_some_and(|t| state.now().saturating_duration_since(t) <= crate::log_view::FILE_HISTORY_G_WINDOW);
    let diff_zoomed = fh.diff.zoomed;
    let diff_focused = fh.focus == crate::log_view::FileHistoryFocus::Diff || diff_zoomed;
    let affected_focused =
        fh.focus == crate::log_view::FileHistoryFocus::AffectedFiles && fh.show_affected_files && !diff_zoomed;

    let mut prefix = Vec::new();
    if had_pending_g {
        prefix.push(Action::FileHistorySetPendingG(None));
    }

    if key.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) {
        if key.modifiers.contains(KeyModifiers::ALT)
            && matches!(key.code, KeyCode::Char('g' | 'G'))
        {
            prefix.push(Action::FileHistoryAnnotate);
            return prefix;
        }
        match key.code {
            KeyCode::Esc => prefix.push(Action::FileHistoryEscape),
            KeyCode::Tab | KeyCode::BackTab => prefix.push(Action::FileHistoryToggleFocus),
            _ => {}
        }
        return prefix;
    }

    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    if pending_g {
        if key.code == KeyCode::Esc {
            return prefix;
        }
        if key.code == KeyCode::Char('g') && !shift {
            if diff_focused {
                prefix.push(Action::DiffScroll(
                    DiffScope::FileHistory,
                    crate::app::SCROLL_GOTO_TOP,
                ));
            } else {
                prefix.push(Action::FileHistorySelect {
                    idx: 0,
                    extend: false,
                });
            }
            return prefix;
        }
    }

    let action = match key.code {
        KeyCode::Esc => Some(Action::FileHistoryEscape),
        KeyCode::Tab | KeyCode::BackTab => Some(Action::FileHistoryToggleFocus),
        KeyCode::Enter if fh.is_on_load_more_row() => Some(Action::FileHistoryLoadMore),
        KeyCode::Enter => Some(Action::FileHistoryJumpToLog),
        KeyCode::Char('.') | KeyCode::Menu => {
            let idx = fh.selected;
            let anchor = state
                .hit_registry
                .find(|t| matches!(t, HitTarget::FileHistoryRow { idx: i } if *i == idx))
                .map(|r| {
                    let x = r.rect.x + (r.rect.width / 2).min(r.rect.width.saturating_sub(1));
                    (x, r.rect.y)
                })
                .unwrap_or_else(|| {
                    (
                        state.terminal_area.x + state.terminal_area.width / 4,
                        state.terminal_area.y + 5,
                    )
                });
            Some(Action::OpenContextMenuForTarget {
                target: HitTarget::FileHistoryRow { idx },
                anchor,
            })
        }
        KeyCode::Char(' ') if !diff_focused && !affected_focused => {
            Some(Action::FileHistoryToggleMarkAnchor)
        }
        KeyCode::Char('B') if !fh.is_selection_history() => {
            Some(Action::FileHistoryCycleBranchScope)
        }
        KeyCode::Char('L') => Some(Action::FileHistoryLoadMore),
        KeyCode::Char('i') => Some(Action::FileHistoryToggleCommitDetails),
        KeyCode::Char('a') => Some(Action::FileHistoryToggleAffectedFiles),
        KeyCode::Char('v') => Some(Action::FileHistoryOpenRepoVersion),
        KeyCode::Char('y') => Some(Action::FileHistoryCopyHash),
        KeyCode::Char('p') => Some(Action::FileHistoryCopyPatch),
        KeyCode::Char('R') => Some(Action::FileHistoryGetVersion),
        KeyCode::Char('g') if !shift => {
            return vec![Action::FileHistorySetPendingG(Some(state.now()))];
        }
        KeyCode::Char('f') if !fh.is_selection_history() => Some(Action::FileHistoryToggleFollow),
        KeyCode::Char('b') if diff_focused => {
            Some(Action::DiffCycleWhitespace(DiffScope::FileHistory))
        }
        KeyCode::Char('b') => Some(Action::FileHistoryAnnotate),
        KeyCode::Char('l') if diff_focused => Some(Action::DiffScrollHoriz(
            DiffScope::FileHistory,
            crate::diff_session::DIFF_HORIZ_SCROLL_STEP,
        )),
        KeyCode::Char('h') if diff_focused => Some(Action::DiffScrollHoriz(
            DiffScope::FileHistory,
            -crate::diff_session::DIFF_HORIZ_SCROLL_STEP,
        )),
        KeyCode::Left if shift || diff_focused => Some(Action::DiffScrollHoriz(
            DiffScope::FileHistory,
            if shift {
                -16
            } else {
                -crate::diff_session::DIFF_HORIZ_SCROLL_STEP
            },
        )),
        KeyCode::Right if shift || diff_focused => Some(Action::DiffScrollHoriz(
            DiffScope::FileHistory,
            if shift {
                16
            } else {
                crate::diff_session::DIFF_HORIZ_SCROLL_STEP
            },
        )),
        KeyCode::Char('l') => Some(Action::FileHistoryToggleCompareLocal),
        KeyCode::Char('S') => Some(Action::DiffToggleSideBySide(DiffScope::FileHistory)),
        KeyCode::Char('w') => Some(Action::DiffCycleContext(DiffScope::FileHistory)),
        KeyCode::Char('W') => Some(Action::DiffToggleSoftWrap(DiffScope::FileHistory)),
        KeyCode::Char('/') if diff_focused => Some(Action::DiffSearchOpen(DiffScope::FileHistory)),
        KeyCode::Char('/') => Some(Action::FileHistorySearchOpen),
        KeyCode::Char('n') if fh.diff.search.active || diff_focused => {
            Some(Action::DiffSearchJump(DiffScope::FileHistory, 1))
        }
        KeyCode::Char('N') if fh.diff.search.active || diff_focused => {
            Some(Action::DiffSearchJump(DiffScope::FileHistory, -1))
        }
        KeyCode::Char('[') => Some(Action::DiffJumpHunk(DiffScope::FileHistory, -1)),
        KeyCode::Char(']') => Some(Action::DiffJumpHunk(DiffScope::FileHistory, 1)),
        KeyCode::Char('z') | KeyCode::F(4) => Some(Action::FileHistoryToggleZoom),
        KeyCode::Down | KeyCode::Char('j') if affected_focused => {
            Some(Action::FileHistoryAffectedMove(1))
        }
        KeyCode::Up | KeyCode::Char('k') if affected_focused => {
            Some(Action::FileHistoryAffectedMove(-1))
        }
        KeyCode::Up if shift && !diff_zoomed => Some(Action::FileHistoryMove {
            delta: -1,
            extend: true,
        }),
        KeyCode::Down if shift && !diff_zoomed => Some(Action::FileHistoryMove {
            delta: 1,
            extend: true,
        }),
        KeyCode::Down | KeyCode::Char('j') if diff_focused => {
            Some(Action::DiffScroll(DiffScope::FileHistory, 1))
        }
        KeyCode::Up | KeyCode::Char('k') if diff_focused => {
            Some(Action::DiffScroll(DiffScope::FileHistory, -1))
        }
        KeyCode::PageDown if diff_focused => Some(Action::DiffScroll(DiffScope::FileHistory, 10)),
        KeyCode::PageUp if diff_focused => Some(Action::DiffScroll(DiffScope::FileHistory, -10)),
        KeyCode::Home if diff_focused => Some(Action::DiffScroll(
            DiffScope::FileHistory,
            crate::app::SCROLL_GOTO_TOP,
        )),
        KeyCode::End | KeyCode::Char('G') if diff_focused => Some(Action::DiffScroll(
            DiffScope::FileHistory,
            crate::app::SCROLL_GOTO_BOTTOM,
        )),
        KeyCode::Down | KeyCode::Char('j') if !diff_zoomed => Some(Action::FileHistoryMove {
            delta: 1,
            extend: false,
        }),
        KeyCode::Up | KeyCode::Char('k') if !diff_zoomed => Some(Action::FileHistoryMove {
            delta: -1,
            extend: false,
        }),
        KeyCode::PageDown if !diff_zoomed => Some(Action::FileHistoryMove {
            delta: 10,
            extend: false,
        }),
        KeyCode::PageUp if !diff_zoomed => Some(Action::FileHistoryMove {
            delta: -10,
            extend: false,
        }),
        KeyCode::Home if !diff_zoomed => Some(Action::FileHistorySelect {
            idx: 0,
            extend: false,
        }),
        KeyCode::End | KeyCode::Char('G') if !diff_zoomed => Some(Action::FileHistorySelect {
            idx: usize::MAX,
            extend: false,
        }),
        _ => None,
    };
    if let Some(a) = action {
        prefix.push(a);
    }
    prefix
}

fn route_annotate_key(key: &KeyEvent, state: &AppState) -> Vec<Action> {
    let Some(ann) = state.modal_stack.annotate() else {
        return vec![Action::AnnotateEscape];
    };
    if ann.diff.search.editing {
        return route_diff_search_editing_key(key, DiffScope::Annotate);
    }
    if ann.goto_line_input.is_some() {
        return match key.code {
            KeyCode::Esc => vec![Action::AnnotateGotoLineCancel],
            KeyCode::Enter => vec![Action::AnnotateGotoLineConfirm],
            KeyCode::Backspace => vec![Action::AnnotateGotoLineBackspace],
            KeyCode::Char(c) if c.is_ascii_digit() => vec![Action::AnnotateGotoLineChar(c)],
            _ => Vec::new(),
        };
    }

    let had_pending_g = ann.pending_g.is_some();
    let pending_g = ann
        .pending_g
        .is_some_and(|t| state.now().saturating_duration_since(t) <= crate::log_view::FILE_HISTORY_G_WINDOW);
    let diff_focused = ann.focus == crate::log_view::AnnotateFocus::Diff && ann.show_diff;
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);

    let mut prefix = Vec::new();
    if had_pending_g {
        prefix.push(Action::AnnotateSetPendingG(None));
    }

    if ctrl || alt {
        let a = match key.code {
            KeyCode::Esc => Some(Action::AnnotateEscape),
            KeyCode::Tab | KeyCode::BackTab => Some(Action::AnnotateToggleFocus),
            KeyCode::Char('g') if ctrl => Some(Action::AnnotateGotoLineStart),
            KeyCode::Char('d') if ctrl => Some(if diff_focused {
                Action::DiffScroll(DiffScope::Annotate, 10)
            } else {
                Action::AnnotateMove(10)
            }),
            KeyCode::Char('u') if ctrl => Some(if diff_focused {
                Action::DiffScroll(DiffScope::Annotate, -10)
            } else {
                Action::AnnotateMove(-10)
            }),
            _ => None,
        };
        if let Some(action) = a {
            prefix.push(action);
        }
        return prefix;
    }

    if pending_g {
        if key.code == KeyCode::Esc {
            return prefix;
        }
        if key.code == KeyCode::Char('g') && !shift {
            if diff_focused {
                prefix.push(Action::DiffScroll(
                    DiffScope::Annotate,
                    crate::app::SCROLL_GOTO_TOP,
                ));
            } else {
                prefix.push(Action::AnnotateSelectLine {
                    line_idx: 0,
                    extend: ann.visual_anchor.is_some(),
                });
            }
            return prefix;
        }
    }

    let action = if let Some(cmd) = crate::keymap::catalog::AnnotateCommand::from_key(key.code) {
        Some(cmd.action())
    } else {
        match key.code {
            KeyCode::Esc => Some(Action::AnnotateEscape),
            KeyCode::Tab | KeyCode::BackTab => Some(Action::AnnotateToggleFocus),
            KeyCode::Char('g') if !shift => {
                return vec![Action::AnnotateSetPendingG(Some(state.now()))];
            }
            KeyCode::Char(':') => Some(Action::AnnotateGotoLineStart),
            KeyCode::Char('v' | 'V') => Some(Action::AnnotateToggleVisual),
            KeyCode::Char('P') | KeyCode::Backspace => Some(Action::AnnotateNavBack),
            KeyCode::Char('S') => Some(Action::DiffToggleSideBySide(DiffScope::Annotate)),
            KeyCode::Char('c') => Some(Action::DiffCycleContext(DiffScope::Annotate)),
            KeyCode::Char('W') if diff_focused => {
                Some(Action::DiffToggleSoftWrap(DiffScope::Annotate))
            }
            KeyCode::Char('b') if diff_focused => {
                Some(Action::DiffCycleWhitespace(DiffScope::Annotate))
            }
            KeyCode::Char('/') if diff_focused => {
                Some(Action::DiffSearchOpen(DiffScope::Annotate))
            }
            KeyCode::Char('n') if diff_focused && ann.diff.search.active => {
                Some(Action::DiffSearchJump(DiffScope::Annotate, 1))
            }
            KeyCode::Char('N') if diff_focused && ann.diff.search.active => {
                Some(Action::DiffSearchJump(DiffScope::Annotate, -1))
            }
            KeyCode::Left | KeyCode::Char('h') if diff_focused => Some(Action::DiffScrollHoriz(
                DiffScope::Annotate,
                if shift { -16 } else { -crate::diff_session::DIFF_HORIZ_SCROLL_STEP },
            )),
            KeyCode::Right | KeyCode::Char('l') if diff_focused => Some(Action::DiffScrollHoriz(
                DiffScope::Annotate,
                if shift { 16 } else { crate::diff_session::DIFF_HORIZ_SCROLL_STEP },
            )),
            KeyCode::Char('[') => Some(Action::DiffJumpHunk(DiffScope::Annotate, -1)),
            KeyCode::Char(']') => Some(Action::DiffJumpHunk(DiffScope::Annotate, 1)),
            KeyCode::F(10) if shift => {
            let line_idx = ann.cursor;
            let anchor = state
                .hit_registry
                .find(|t| matches!(t, HitTarget::AnnotateLine { line_idx: idx } if *idx == line_idx))
                .map(|r| {
                    let x = r.rect.x + (r.rect.width / 2).min(r.rect.width.saturating_sub(1));
                    (x, r.rect.y)
                })
                .unwrap_or_else(|| {
                    (
                        state.terminal_area.x + state.terminal_area.width / 3,
                        state.terminal_area.y + 5,
                    )
                });
            Some(Action::OpenContextMenuForTarget {
                target: HitTarget::AnnotateLine { line_idx },
                anchor,
            })
        }
        KeyCode::Char('.') | KeyCode::Menu => {
            let line_idx = ann.cursor;
            let anchor = state
                .hit_registry
                .find(|t| matches!(t, HitTarget::AnnotateLine { line_idx: idx } if *idx == line_idx))
                .map(|r| {
                    let x = r.rect.x + (r.rect.width / 2).min(r.rect.width.saturating_sub(1));
                    (x, r.rect.y)
                })
                .unwrap_or_else(|| {
                    (
                        state.terminal_area.x + state.terminal_area.width / 3,
                        state.terminal_area.y + 5,
                    )
                });
            Some(Action::OpenContextMenuForTarget {
                target: HitTarget::AnnotateLine { line_idx },
                anchor,
            })
        }
        KeyCode::Up if shift && !diff_focused => Some(Action::AnnotateMoveExtend(-1)),
        KeyCode::Down if shift && !diff_focused => Some(Action::AnnotateMoveExtend(1)),
        KeyCode::Down | KeyCode::Char('j') if diff_focused => {
            Some(Action::DiffScroll(DiffScope::Annotate, 1))
        }
        KeyCode::Up | KeyCode::Char('k') if diff_focused => {
            Some(Action::DiffScroll(DiffScope::Annotate, -1))
        }
        KeyCode::PageDown if diff_focused => Some(Action::DiffScroll(DiffScope::Annotate, 10)),
        KeyCode::PageUp if diff_focused => Some(Action::DiffScroll(DiffScope::Annotate, -10)),
        KeyCode::Home if diff_focused => Some(Action::DiffScroll(
            DiffScope::Annotate,
            crate::app::SCROLL_GOTO_TOP,
        )),
        KeyCode::End | KeyCode::Char('G') if diff_focused => Some(Action::DiffScroll(
            DiffScope::Annotate,
            crate::app::SCROLL_GOTO_BOTTOM,
        )),
        KeyCode::Down | KeyCode::Char('j') => Some(Action::AnnotateMove(1)),
        KeyCode::Up | KeyCode::Char('k') => Some(Action::AnnotateMove(-1)),
        KeyCode::PageDown => Some(Action::AnnotateMove(10)),
        KeyCode::PageUp => Some(Action::AnnotateMove(-10)),
        KeyCode::Home => Some(Action::AnnotateSelectLine {
            line_idx: 0,
            extend: shift || ann.visual_anchor.is_some(),
        }),
        KeyCode::End | KeyCode::Char('G') => Some(Action::AnnotateSelectLine {
            line_idx: usize::MAX,
            extend: ann.visual_anchor.is_some(),
        }),
            _ => None,
        }
    };
    if let Some(a) = action {
        prefix.push(a);
    }
    prefix
}

fn route_push_dialog_key(key: &KeyEvent, state: &AppState) -> Vec<Action> {
    let Some(pd) = state.modal_stack.push_dialog() else {
        return vec![Action::PushEscape];
    };
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let plain = !ctrl && !alt;

    if pd.editing_target {
        return match key.code {
            KeyCode::Esc => vec![Action::PushEditTargetCancel],
            KeyCode::Enter => vec![Action::PushEditTargetConfirm],
            KeyCode::Tab => vec![Action::PushEditTargetAcceptCompletion(None)],
            KeyCode::Up => vec![Action::PushEditTargetMoveCompletion(-1)],
            KeyCode::Down => vec![Action::PushEditTargetMoveCompletion(1)],
            KeyCode::Backspace => vec![Action::PushEditTargetBackspace],
            KeyCode::Char('u') if ctrl => vec![Action::PushEditTargetClear],
            KeyCode::Char(c) if plain && !c.is_control() => vec![Action::PushEditTargetChar(c)],
            _ => Vec::new(),
        };
    }

    if pd.picking_remote {
        return match key.code {
            KeyCode::Esc => vec![Action::PushToggleRemotePicker],
            KeyCode::Enter => vec![Action::PushToggleRemotePicker],
            KeyCode::Up | KeyCode::Char('k') if plain => vec![Action::PushMoveRemotePicker(-1)],
            KeyCode::Down | KeyCode::Char('j') if plain => vec![Action::PushMoveRemotePicker(1)],
            _ => Vec::new(),
        };
    }

    if ctrl || alt {
        return match key.code {
            KeyCode::Esc => vec![Action::PushEscape],
            KeyCode::Enter if ctrl || alt => vec![Action::PushConfirm],
            KeyCode::Tab | KeyCode::BackTab => vec![Action::PushFocusPrev],
            _ => Vec::new(),
        };
    }

    use crate::log_view::PushDialogFocus;
    let diff_focused = pd.show_diff && pd.focus == PushDialogFocus::Files;
    match key.code {
        KeyCode::Esc => vec![Action::PushEscape],
        KeyCode::Tab => vec![Action::PushFocusNext],
        KeyCode::BackTab => vec![Action::PushFocusPrev],
        KeyCode::Char('e') => vec![Action::PushStartEditTarget],
        KeyCode::Char('r') if pd.remotes.is_empty() => vec![Action::PushDefineRemote],
        KeyCode::Char('r') => vec![Action::PushToggleRemotePicker],
        KeyCode::Char('a') if pd.remotes.is_empty() => vec![Action::PushDefineRemote],
        KeyCode::Char('t') => vec![Action::PushCycleTags],
        KeyCode::Char('f') => vec![Action::PushCycleForce],
        KeyCode::Char('u') => vec![Action::PushToggleSetUpstream],
        KeyCode::Char('n') => vec![Action::PushToggleNoVerify],
        KeyCode::Char('d') => vec![Action::PushToggleDiff],
        KeyCode::Char('S') if pd.show_diff => vec![Action::DiffToggleSideBySide(DiffScope::Push)],
        KeyCode::Char('c') if pd.show_diff => vec![Action::DiffCycleContext(DiffScope::Push)],
        KeyCode::Char('[') if pd.show_diff => vec![Action::DiffJumpHunk(DiffScope::Push, -1)],
        KeyCode::Char(']') if pd.show_diff => vec![Action::DiffJumpHunk(DiffScope::Push, 1)],
        KeyCode::Enter => match pd.focus {
            PushDialogFocus::Target => vec![Action::PushStartEditTarget],
            PushDialogFocus::Files => vec![Action::PushToggleDiff],
            PushDialogFocus::Commits => vec![Action::PushConfirm],
        },
        KeyCode::Down | KeyCode::Char('j') => match pd.focus {
            PushDialogFocus::Commits => vec![Action::PushMoveCommit(1)],
            PushDialogFocus::Files if diff_focused => vec![Action::DiffScroll(DiffScope::Push, 1)],
            PushDialogFocus::Files => vec![Action::PushMoveFile(1)],
            PushDialogFocus::Target => vec![Action::PushFocusNext],
        },
        KeyCode::Up | KeyCode::Char('k') => match pd.focus {
            PushDialogFocus::Commits => vec![Action::PushMoveCommit(-1)],
            PushDialogFocus::Files if diff_focused => {
                vec![Action::DiffScroll(DiffScope::Push, -1)]
            }
            PushDialogFocus::Files => vec![Action::PushMoveFile(-1)],
            PushDialogFocus::Target => vec![Action::PushFocusPrev],
        },
        KeyCode::PageDown => match pd.focus {
            PushDialogFocus::Commits => vec![Action::PushMoveCommit(10)],
            PushDialogFocus::Files if diff_focused => {
                vec![Action::DiffScroll(DiffScope::Push, 10)]
            }
            PushDialogFocus::Files => vec![Action::PushMoveFile(10)],
            PushDialogFocus::Target => Vec::new(),
        },
        KeyCode::PageUp => match pd.focus {
            PushDialogFocus::Commits => vec![Action::PushMoveCommit(-10)],
            PushDialogFocus::Files if diff_focused => {
                vec![Action::DiffScroll(DiffScope::Push, -10)]
            }
            PushDialogFocus::Files => vec![Action::PushMoveFile(-10)],
            PushDialogFocus::Target => Vec::new(),
        },
        KeyCode::Home => match pd.focus {
            PushDialogFocus::Commits => vec![Action::PushSelectCommit(0)],
            PushDialogFocus::Files if diff_focused => vec![Action::DiffScroll(
                DiffScope::Push,
                crate::app::SCROLL_GOTO_TOP,
            )],
            PushDialogFocus::Files => vec![Action::PushSelectFile(0)],
            PushDialogFocus::Target => Vec::new(),
        },
        KeyCode::End | KeyCode::Char('G') => match pd.focus {
            PushDialogFocus::Commits => vec![Action::PushSelectCommit(usize::MAX)],
            PushDialogFocus::Files if diff_focused => vec![Action::DiffScroll(
                DiffScope::Push,
                crate::app::SCROLL_GOTO_BOTTOM,
            )],
            PushDialogFocus::Files => vec![Action::PushSelectFile(usize::MAX)],
            PushDialogFocus::Target => Vec::new(),
        },
        _ => Vec::new(),
    }
}

fn route_reflog_key(key: &KeyEvent, state: &AppState) -> Vec<Action> {
    use crate::log_view::ReflogFocus;
    let Some(rf) = state.modal_stack.reflog() else {
        return Vec::new();
    };
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    match key.code {
        KeyCode::Esc | KeyCode::Char('q') if !ctrl && !alt => vec![Action::CloseReflog],
        KeyCode::Tab => vec![Action::ReflogCycleFocus(false)],
        KeyCode::BackTab => vec![Action::ReflogCycleFocus(true)],
        KeyCode::Char('j') | KeyCode::Down => match rf.focus {
            ReflogFocus::Entries => vec![Action::ReflogMove(1)],
            ReflogFocus::Changes => vec![Action::ReflogChangesMove(1)],
            ReflogFocus::Diff => vec![Action::ReflogDiffScroll(1)],
        },
        KeyCode::Char('k') | KeyCode::Up => match rf.focus {
            ReflogFocus::Entries => vec![Action::ReflogMove(-1)],
            ReflogFocus::Changes => vec![Action::ReflogChangesMove(-1)],
            ReflogFocus::Diff => vec![Action::ReflogDiffScroll(-1)],
        },
        KeyCode::PageDown => match rf.focus {
            ReflogFocus::Entries => vec![Action::ReflogMove(10)],
            ReflogFocus::Changes => vec![Action::ReflogChangesMove(10)],
            ReflogFocus::Diff => vec![Action::ReflogDiffScroll(12)],
        },
        KeyCode::PageUp => match rf.focus {
            ReflogFocus::Entries => vec![Action::ReflogMove(-10)],
            ReflogFocus::Changes => vec![Action::ReflogChangesMove(-10)],
            ReflogFocus::Diff => vec![Action::ReflogDiffScroll(-12)],
        },
        KeyCode::Home => match rf.focus {
            ReflogFocus::Entries => vec![Action::ReflogSelect(0)],
            ReflogFocus::Changes => vec![Action::ReflogChangesSelect(0)],
            ReflogFocus::Diff => vec![Action::ReflogDiffScroll(crate::app::SCROLL_GOTO_TOP)],
        },
        KeyCode::End | KeyCode::Char('G') => match rf.focus {
            ReflogFocus::Entries => vec![Action::ReflogSelect(usize::MAX)],
            ReflogFocus::Changes => vec![Action::ReflogChangesSelect(usize::MAX)],
            ReflogFocus::Diff => vec![Action::ReflogDiffScroll(crate::app::SCROLL_GOTO_BOTTOM)],
        },
        KeyCode::Char('c') if !ctrl && !alt => vec![Action::ReflogCheckoutSelected],
        KeyCode::Char('n') if !ctrl && !alt => vec![Action::ReflogNewBranchSelected],
        KeyCode::Char('p') if !ctrl && !alt => vec![Action::ReflogCherryPickSelected],
        KeyCode::Char('r') if !ctrl && !alt => vec![Action::ReflogResetSelected],
        KeyCode::Char('y') if !ctrl && !alt => vec![Action::ReflogCopyHash],
        KeyCode::Char('m') if !ctrl && !alt => vec![Action::ReflogLoadMore],
        KeyCode::Char('l') if !ctrl && !alt => vec![Action::ReflogJumpInLog],
        KeyCode::Enter if rf.focus == ReflogFocus::Entries => vec![Action::ReflogJumpInLog],
        KeyCode::Char('S') if !ctrl && !alt => vec![Action::ReflogToggleSideBySide],
        KeyCode::Char('w') if !ctrl && !alt => vec![Action::ReflogCycleContext],
        _ => Vec::new(),
    }
}
