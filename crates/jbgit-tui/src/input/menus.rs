//! Unified, read-only input routing and context-menu builders for cascading
//! menus, dialogs, the branches popup, the ongoing banner, and the Stash tab.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use jbgit_core::{ConflictChoice, GitOp, OngoingStep, PaneId, WorkspaceTab};
use ratatui::layout::Rect;

use crate::app::{
    Action, AppState, ChangesHost, ContextMenuEntryKind, ContextMenuItem, DiffScope, MenuParent,
    ModalLayer,
};
use crate::branches_popup::activate_row;
use crate::config::KeymapScope;
use crate::graph::GraphRendererMode;
use crate::hit_test::{
    ChangesToolbarButton, ConflictMergeButtonKind, ConflictQueueButtonKind, DialogButtonKind,
    DiffToolbarButton, HitTarget, OngoingButton,
};
use crate::input_stage::stage_tree_context_menu_items;
use crate::keymap::{KeyAction, catalog};
use crate::log_view::PresentationToggle;
use crate::menus;
use crate::views::overlays::{context_menu_rect, menu_item_row_y};

/// Index of the next selectable (non-separator) item in direction `delta`.
fn step_selectable(items: &[ContextMenuItem], from: usize, delta: isize) -> usize {
    let n = items.len();
    if n == 0 {
        return 0;
    }
    let mut idx = from.min(n - 1);
    for _ in 0..n {
        idx = (idx as isize + delta).rem_euclid(n as isize) as usize;
        if !items[idx].is_separator() {
            return idx;
        }
    }
    from
}

/// True when the layer directly beneath the open context menus is the
/// branches popup (actions then close the popup as well).
fn menus_over_branches_popup(state: &AppState) -> bool {
    state
        .modal_stack
        .iter()
        .rev()
        .find(|l| !matches!(l, ModalLayer::ContextMenu { .. }))
        .is_some_and(|l| matches!(l, ModalLayer::BranchesPopup { .. }))
}

/// Activates item `idx` of the topmost context menu (pure read).
pub fn activate_top_menu_item(state: &AppState, idx: usize) -> Vec<Action> {
    let Some(ModalLayer::ContextMenu {
        anchor,
        items,
        depth,
        parent,
        ..
    }) = state.modal_stack.last()
    else {
        return Vec::new();
    };
    let Some(item) = items.get(idx) else {
        return Vec::new();
    };
    if let Some(reason) = &item.disabled_reason {
        return vec![Action::Notice(format!("{}: {reason}", item.label))];
    }
    match &item.kind {
        ContextMenuEntryKind::Separator => Vec::new(),
        ContextMenuEntryKind::Submenu(children) => {
            let screen = state.terminal_area;
            let rect = context_menu_rect(screen, *anchor, *parent, items);
            vec![Action::OpenSubmenu {
                parent: MenuParent {
                    rect,
                    row_y: menu_item_row_y(screen, rect, items, idx),
                    item_idx: idx,
                },
                items: children.clone(),
                depth: depth.saturating_add(1),
            }]
        }
        ContextMenuEntryKind::Action(actions) => {
            let mut out = vec![Action::PopContextMenus];
            if menus_over_branches_popup(state) {
                out.push(Action::PopModal);
            }
            out.extend(actions.iter().cloned());
            out
        }
    }
}

pub fn route_context_menu_key(key: KeyEvent, state: &AppState) -> Vec<Action> {
    let Some(ModalLayer::ContextMenu {
        items,
        selected,
        depth,
        ..
    }) = state.modal_stack.last()
    else {
        return Vec::new();
    };
    let depth = *depth;
    let selected = *selected;
    match key.code {
        KeyCode::Esc | KeyCode::Left => vec![Action::PopModal],
        KeyCode::Up | KeyCode::Char('k') if !items.is_empty() => {
            let idx = step_selectable(items, selected, -1);
            vec![Action::ContextMenuSetSelected { depth, idx }]
        }
        KeyCode::Down | KeyCode::Char('j') if !items.is_empty() => {
            let idx = step_selectable(items, selected, 1);
            vec![Action::ContextMenuSetSelected { depth, idx }]
        }
        KeyCode::Home if !items.is_empty() => {
            let idx = step_selectable(items, items.len() - 1, 1);
            vec![Action::ContextMenuSetSelected { depth, idx }]
        }
        KeyCode::End if !items.is_empty() => {
            let idx = step_selectable(items, 0, -1);
            vec![Action::ContextMenuSetSelected { depth, idx }]
        }
        KeyCode::Right => {
            if items
                .get(selected)
                .is_some_and(|i| matches!(i.kind, ContextMenuEntryKind::Submenu(_)))
            {
                activate_top_menu_item(state, selected)
            } else {
                Vec::new()
            }
        }
        KeyCode::Enter | KeyCode::Char(' ') => activate_top_menu_item(state, selected),
        KeyCode::Delete => {
            if let Some(idx) = items.iter().position(|i| {
                !i.is_separator()
                    && i.shortcut
                        .as_deref()
                        .is_some_and(|s| s == "Del" || s.split(" / ").any(|p| p == "Del"))
            }) {
                let mut out = vec![Action::ContextMenuSetSelected { depth, idx }];
                out.extend(activate_top_menu_item(state, idx));
                out
            } else {
                Vec::new()
            }
        }
        KeyCode::Char(ch)
            if !key.modifiers.contains(KeyModifiers::CONTROL)
                && !key.modifiers.contains(KeyModifiers::ALT) =>
        {
            let target = ch.to_ascii_lowercase();
            if let Some(idx) = items.iter().position(|i| {
                !i.is_separator() && i.mnemonic.is_some_and(|m| m.to_ascii_lowercase() == target)
            }) {
                let mut out = vec![Action::ContextMenuSetSelected { depth, idx }];
                out.extend(activate_top_menu_item(state, idx));
                out
            } else {
                Vec::new()
            }
        }
        _ => Vec::new(),
    }
}

/// Keyboard routing for the built-in three-pane 3-way conflict merge modal.
pub fn route_conflict_merge_key(key: &KeyEvent, state: &AppState) -> Vec<Action> {
    if state
        .modal_stack
        .conflict_merge()
        .is_some_and(|cm| cm.inline_editor.is_some())
    {
        return vec![Action::ConflictInlineEditKey(*key)];
    }
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let path = state
        .modal_stack
        .conflict_merge()
        .map(|cm| cm.path.clone())
        .unwrap_or_default();
    match key.code {
        KeyCode::Esc => vec![Action::PopModal],
        KeyCode::Enter => vec![Action::ConflictSaveAndStage],
        KeyCode::Char('s') if ctrl => vec![Action::ConflictSaveAndStage],
        KeyCode::Down | KeyCode::Char('j') if !ctrl && !alt => vec![Action::ConflictScroll(1)],
        KeyCode::Up | KeyCode::Char('k') if !ctrl && !alt => vec![Action::ConflictScroll(-1)],
        KeyCode::Left | KeyCode::Char('h') if !ctrl && !alt => {
            vec![Action::ConflictHorizontalScroll(-4)]
        }
        KeyCode::Right | KeyCode::Char('l') if !ctrl && !alt => {
            vec![Action::ConflictHorizontalScroll(4)]
        }
        KeyCode::PageDown => vec![Action::ConflictScroll(10)],
        KeyCode::PageUp => vec![Action::ConflictScroll(-10)],
        KeyCode::Char('n') | KeyCode::Tab if !ctrl && !alt => vec![Action::ConflictSelectBlock(1)],
        KeyCode::Char('p') | KeyCode::BackTab if !ctrl && !alt => {
            vec![Action::ConflictSelectBlock(-1)]
        }
        KeyCode::Char('a') if !ctrl && !alt => vec![Action::ConflictApplyNonConflicting],
        KeyCode::Char('b') if !ctrl && !alt => vec![Action::ConflictToggleBase],
        KeyCode::Char('1' | 'y') if !ctrl && !alt => {
            vec![Action::ConflictApplyChoice(ConflictChoice::Ours)]
        }
        KeyCode::Char('2' | 't') if !ctrl && !alt => {
            vec![Action::ConflictApplyChoice(ConflictChoice::Theirs)]
        }
        KeyCode::Char('3' | 'B') if !ctrl && !alt => {
            vec![Action::ConflictApplyChoice(ConflictChoice::BothOursFirst)]
        }
        KeyCode::Char('4' | 'd') if !ctrl && !alt => {
            vec![Action::ConflictApplyChoice(ConflictChoice::Neither)]
        }
        KeyCode::Char('0' | 'x' | 'u') if !ctrl && !alt => {
            vec![Action::ConflictApplyChoice(ConflictChoice::Unresolved)]
        }
        KeyCode::Char('e') if !ctrl && !alt => vec![Action::ConflictStartInlineEdit],
        KeyCode::Char('Y') if !ctrl && !alt => {
            vec![Action::ConflictAcceptAll(ConflictChoice::Ours)]
        }
        KeyCode::Char('T') if !ctrl && !alt => {
            vec![Action::ConflictAcceptAll(ConflictChoice::Theirs)]
        }
        KeyCode::Char('m') if !ctrl && !alt && !path.is_empty() => {
            vec![Action::OpenExternalMergetool(path)]
        }
        KeyCode::Char('E') if !ctrl && !alt && !path.is_empty() => {
            vec![Action::OpenExternalEditorFile(Some(path))]
        }
        _ => Vec::new(),
    }
}

/// Keyboard routing for the multi-file Conflict Queue modal.
pub fn route_conflict_queue_key(key: &KeyEvent) -> Vec<Action> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    match key.code {
        KeyCode::Esc => vec![Action::PopModal],
        KeyCode::Down | KeyCode::Char('j') if !ctrl && !alt => vec![Action::ConflictQueueMove(1)],
        KeyCode::Up | KeyCode::Char('k') if !ctrl && !alt => vec![Action::ConflictQueueMove(-1)],
        KeyCode::PageDown => vec![Action::ConflictQueueMove(10)],
        KeyCode::PageUp => vec![Action::ConflictQueueMove(-10)],
        KeyCode::Home | KeyCode::Char('g') if !ctrl && !alt => vec![Action::ConflictQueueSelect(0)],
        KeyCode::End | KeyCode::Char('G') if !ctrl && !alt => {
            vec![Action::ConflictQueueSelect(usize::MAX)]
        }
        KeyCode::Char(' ') if !ctrl && !alt => vec![Action::ConflictQueueToggleSelect],
        KeyCode::Char('a') if !alt => vec![Action::ConflictQueueToggleSelectAll],
        KeyCode::Enter | KeyCode::Char('m') if !ctrl && !alt => {
            vec![Action::ConflictQueueMergeSelected]
        }
        KeyCode::Char('y') if !ctrl && !alt => vec![Action::ConflictQueueAcceptSide(true)],
        KeyCode::Char('t') if !ctrl && !alt => vec![Action::ConflictQueueAcceptSide(false)],
        KeyCode::Char('r') if !ctrl && !alt => vec![Action::ConflictQueueRevertResolved],
        KeyCode::Char('M') if !ctrl && !alt => vec![Action::ConflictQueueMergetool],
        _ => Vec::new(),
    }
}

/// Left-clicks on overlay / workbench hit targets (dialogs, popup, banner, stash, …).
pub fn route_overlay_and_stash_click(
    target: &HitTarget,
    state: &AppState,
    is_double: bool,
) -> Option<Vec<Action>> {
    let actions = match target {
        HitTarget::MenuBody { .. } | HitTarget::DialogBody => Vec::new(),
        HitTarget::DialogOption(i) => {
            let confirm = match state.modal_stack.last() {
                Some(ModalLayer::Dialog(d)) => match d.as_ref() {
                    crate::dialogs::Dialog::Choice(c) => !c.radio || is_double,
                    _ => false,
                },
                _ => false,
            };
            vec![Action::DialogSelectOption { idx: *i, confirm }]
        }
        HitTarget::DialogButton(DialogButtonKind::Confirm) => vec![Action::DialogConfirm],
        HitTarget::DialogButton(DialogButtonKind::Cancel) => vec![Action::PopModal],
        HitTarget::DialogCheckbox => vec![Action::DialogToggleCheckbox],
        HitTarget::RepoSwitcherRow(row) => {
            let mut out = vec![Action::RepoSwitcherSetState {
                query: match state.modal_stack.last() {
                    Some(ModalLayer::RepoSwitcher(rs)) => rs.query.clone(),
                    _ => String::new(),
                },
                selected: *row,
            }];
            if is_double && let Some(ModalLayer::RepoSwitcher(rs)) = state.modal_stack.last() {
                out = crate::repo_switcher::activate_row(rs, *row);
            }
            out
        }
        HitTarget::ApplyPatchMode(mode) => vec![Action::ApplyPatchSetMode(Some(*mode))],
        HitTarget::ApplyPatchButton { apply: true } => vec![Action::ApplyPatchConfirm],
        HitTarget::ApplyPatchButton { apply: false } => vec![Action::PopModal],
        HitTarget::TreeBrowserRow(idx) => {
            use crate::tree_browser::TreeBrowserAction;
            if is_double {
                vec![Action::TreeBrowser(TreeBrowserAction::Toggle(*idx))]
            } else {
                vec![Action::TreeBrowser(TreeBrowserAction::Select(*idx))]
            }
        }
        HitTarget::TreeBrowserPreview => Vec::new(),
        HitTarget::WorktreesRow(idx) => {
            let mut out = vec![Action::WorktreesSelect(*idx)];
            if is_double {
                // Re-evaluated after the selection lands on the clicked row.
                out.push(Action::WorktreesOpenSelected);
            }
            out
        }
        HitTarget::RemotesRow(idx) => {
            let mut out = vec![Action::RemotesSelect(*idx)];
            if is_double {
                let probe = crate::remotes_view::RemotesState {
                    selected: *idx,
                    ..Default::default()
                };
                if let Some(r) = crate::remotes_view::selected_remote(state, &probe) {
                    out.push(Action::OpenDialog(Box::new(
                        crate::dialogs::set_remote_url_dialog(
                            state.repo_snapshot.as_deref(),
                            &r.name,
                            false,
                            &state.i18n,
                        ),
                    )));
                }
            }
            out
        }
        HitTarget::BranchPopupRow(idx) => {
            let idx = *idx;
            let query = match state.modal_stack.last() {
                Some(ModalLayer::BranchesPopup { query, .. }) => query.clone(),
                _ => return Some(Vec::new()),
            };
            let mut out = vec![Action::PopupSetState {
                query: query.clone(),
                selected: idx,
            }];
            out.extend(activate_row(state, &query, idx));
            out
        }
        HitTarget::BranchPopupStar(idx) => {
            let query = match state.modal_stack.last() {
                Some(ModalLayer::BranchesPopup { query, .. }) => query.clone(),
                _ => String::new(),
            };
            let rows = crate::branches_popup::build_popup_rows(state, &query);
            match rows.get(*idx) {
                Some(crate::branches_popup::PopupRow::Branch(b)) => {
                    vec![Action::ToggleFavoriteRef {
                        full: b.full_ref.clone(),
                        short: b.short.clone(),
                    }]
                }
                _ => Vec::new(),
            }
        }
        HitTarget::BranchPopupFetch => vec![Action::PopModal, Action::RunGitOp(GitOp::Fetch)],
        HitTarget::BranchPopupSettings => {
            let rect = crate::branches_popup::popup_layout(state.terminal_area, 1, 0).rect;
            let check = |on: bool| if on { "✓" } else { " " };
            let o = &state.log.branches.settings.options;
            let i18n = &state.i18n;
            vec![Action::OpenContextMenu {
                anchor: (rect.right().saturating_sub(20), rect.y + 1),
                items: vec![
                    ContextMenuItem::run(
                        format!(
                            "{} {}",
                            check(o.group_by_directory),
                            i18n.t("view.branches_popup.opt_group_by_dir")
                        ),
                        Some('d'),
                        None,
                        vec![Action::BranchesToggleGroupByDir],
                    ),
                    ContextMenuItem::run(
                        format!(
                            "{} {}",
                            check(o.show_tags),
                            i18n.t("view.branches_popup.opt_show_tags")
                        ),
                        Some('t'),
                        None,
                        vec![Action::BranchesToggleShowTags],
                    ),
                    ContextMenuItem::separator(),
                    ContextMenuItem::run(
                        i18n.t("view.branches_popup.fetch_all_remotes"),
                        Some('f'),
                        Some("Alt+F"),
                        vec![Action::RunGitOp(GitOp::Fetch)],
                    ),
                ],
                depth: 0,
            }]
        }
        HitTarget::OngoingBanner(btn) => {
            let Some(kind) = state.ongoing_kind() else {
                return Some(Vec::new());
            };
            let step = |s| vec![Action::RunGitOp(GitOp::Ongoing { kind, step: s })];
            match btn {
                OngoingButton::Resolve => vec![Action::OngoingResolve],
                OngoingButton::Continue if state.conflicted_count() > 0 => {
                    let count_s = state.conflicted_count().to_string();
                    vec![Action::Notice(
                        state
                            .i18n
                            .t_fmt("toast.resolve_before_continue", &[("count", &count_s)]),
                    )]
                }
                OngoingButton::Continue => step(OngoingStep::Continue),
                OngoingButton::Skip => step(OngoingStep::Skip),
                OngoingButton::Abort => step(OngoingStep::Abort),
            }
        }
        HitTarget::NotificationAction => vec![Action::NotificationActivate],
        HitTarget::NotificationDismiss => vec![Action::NotificationDismiss],
        HitTarget::StashRow(i) => vec![
            Action::FocusPane(PaneId::StashList),
            Action::StashSelect(*i),
        ],
        HitTarget::StashChangeRow(i) => vec![
            Action::FocusPane(PaneId::StashDiffPreview),
            Action::ChangesSelect(ChangesHost::Stash, *i),
        ],
        HitTarget::StashToolbar(btn) => menus::stash_button_actions(state, *btn),
        HitTarget::StashChangesToolbar(btn) => match btn {
            ChangesToolbarButton::GroupByDir => {
                vec![Action::ChangesToggleGroupByDir(ChangesHost::Stash)]
            }
            ChangesToolbarButton::ExpandAll => {
                vec![Action::ChangesExpandAll(ChangesHost::Stash, true)]
            }
            ChangesToolbarButton::CollapseAll => {
                vec![Action::ChangesExpandAll(ChangesHost::Stash, false)]
            }
            _ => Vec::new(),
        },
        HitTarget::StashDiffToolbar(btn) => match btn {
            DiffToolbarButton::SideBySide => vec![Action::DiffToggleSideBySide(DiffScope::Stash)],
            DiffToolbarButton::Context => vec![Action::DiffCycleContext(DiffScope::Stash)],
            DiffToolbarButton::Whitespace => vec![Action::DiffCycleWhitespace(DiffScope::Stash)],
            DiffToolbarButton::SoftWrap => vec![Action::DiffToggleSoftWrap(DiffScope::Stash)],
            _ => Vec::new(),
        },
        HitTarget::ConflictMergeBlock(id) => vec![Action::ConflictSetBlock(*id)],
        HitTarget::ConflictMergeButton(btn) => {
            let path = state
                .modal_stack
                .conflict_merge()
                .map(|cm| cm.path.clone())
                .unwrap_or_default();
            match btn {
                ConflictMergeButtonKind::ApplyNonConflicting => {
                    vec![Action::ConflictApplyNonConflicting]
                }
                ConflictMergeButtonKind::ToggleBase => vec![Action::ConflictToggleBase],
                ConflictMergeButtonKind::AcceptOurs => {
                    vec![Action::ConflictApplyChoice(ConflictChoice::Ours)]
                }
                ConflictMergeButtonKind::AcceptTheirs => {
                    vec![Action::ConflictApplyChoice(ConflictChoice::Theirs)]
                }
                ConflictMergeButtonKind::AcceptBoth => {
                    vec![Action::ConflictApplyChoice(ConflictChoice::BothOursFirst)]
                }
                ConflictMergeButtonKind::AcceptNeither => {
                    vec![Action::ConflictApplyChoice(ConflictChoice::Neither)]
                }
                ConflictMergeButtonKind::InlineEdit => vec![Action::ConflictStartInlineEdit],
                ConflictMergeButtonKind::ResetBlock => {
                    vec![Action::ConflictApplyChoice(ConflictChoice::Unresolved)]
                }
                ConflictMergeButtonKind::AcceptAllOurs => {
                    vec![Action::ConflictAcceptAll(ConflictChoice::Ours)]
                }
                ConflictMergeButtonKind::AcceptAllTheirs => {
                    vec![Action::ConflictAcceptAll(ConflictChoice::Theirs)]
                }
                ConflictMergeButtonKind::Mergetool if !path.is_empty() => {
                    vec![Action::OpenExternalMergetool(path)]
                }
                ConflictMergeButtonKind::ExternalEditor if !path.is_empty() => {
                    vec![Action::OpenExternalEditorFile(Some(path))]
                }
                ConflictMergeButtonKind::Mergetool | ConflictMergeButtonKind::ExternalEditor => {
                    Vec::new()
                }
                ConflictMergeButtonKind::SaveAndStage => vec![Action::ConflictSaveAndStage],
                ConflictMergeButtonKind::Cancel => vec![Action::PopModal],
            }
        }
        HitTarget::ConflictQueueRow(idx) => {
            if is_double {
                vec![
                    Action::ConflictQueueSelect(*idx),
                    Action::ConflictQueueMergeSelected,
                ]
            } else {
                vec![Action::ConflictQueueSelect(*idx)]
            }
        }
        HitTarget::ConflictQueueButton(btn) => match btn {
            ConflictQueueButtonKind::Merge => vec![Action::ConflictQueueMergeSelected],
            ConflictQueueButtonKind::AcceptYours => vec![Action::ConflictQueueAcceptSide(true)],
            ConflictQueueButtonKind::AcceptTheirs => vec![Action::ConflictQueueAcceptSide(false)],
            ConflictQueueButtonKind::RevertResolved => vec![Action::ConflictQueueRevertResolved],
            ConflictQueueButtonKind::Mergetool => vec![Action::ConflictQueueMergetool],
            ConflictQueueButtonKind::Close => vec![Action::PopModal],
        },
        _ => return None,
    };
    Some(actions)
}

/// Stash tab structural fallbacks (after dynamic keymap routing).
pub fn route_stash_fallback_key(key: &KeyEvent, state: &AppState) -> Vec<Action> {
    if state.active_tab != WorkspaceTab::Stash
        || key.modifiers.contains(KeyModifiers::CONTROL)
        || key.modifiers.contains(KeyModifiers::ALT)
    {
        return Vec::new();
    }
    match state.focused_pane {
        PaneId::StashList => match key.code {
            KeyCode::PageDown => vec![Action::StashMove(10)],
            KeyCode::PageUp => vec![Action::StashMove(-10)],
            KeyCode::Enter | KeyCode::Right => vec![Action::FocusPane(PaneId::StashDiffPreview)],
            _ => Vec::new(),
        },
        PaneId::StashDiffPreview => {
            let shift = key.modifiers.contains(KeyModifiers::SHIFT);
            match key.code {
                KeyCode::Left if shift => vec![Action::DiffScrollHoriz(DiffScope::Stash, -16)],
                KeyCode::Right if shift => vec![Action::DiffScrollHoriz(DiffScope::Stash, 16)],
                KeyCode::Right => vec![Action::ChangesExpand(ChangesHost::Stash, true)],
                KeyCode::Left => vec![Action::ChangesExpand(ChangesHost::Stash, false)],
                KeyCode::Char('v') => vec![Action::ChangesToggleGroupByDir(ChangesHost::Stash)],
                KeyCode::Char('e') => vec![Action::ChangesExpandAll(ChangesHost::Stash, true)],
                KeyCode::Char('E') => vec![Action::ChangesExpandAll(ChangesHost::Stash, false)],
                KeyCode::PageDown | KeyCode::Char(' ') => {
                    vec![Action::DiffScroll(DiffScope::Stash, 10)]
                }
                KeyCode::PageUp => vec![Action::DiffScroll(DiffScope::Stash, -10)],
                KeyCode::Esc if state.stash.diff().search.active => {
                    vec![Action::DiffSearchClose(DiffScope::Stash)]
                }
                KeyCode::Esc => vec![Action::FocusPane(PaneId::StashList)],
                _ => Vec::new(),
            }
        }
        _ => Vec::new(),
    }
}

pub(crate) fn keyboard_context_menu(state: &AppState) -> Option<Vec<Action>> {
    let (target, anchor) = keyboard_menu_target(state)?;
    Some(vec![Action::OpenContextMenuForTarget { target, anchor }])
}

/// Pre-selection actions to run BEFORE building menu items for `target`.
pub(crate) fn preselect_actions_for_menu_target(
    target: &HitTarget,
    state: &AppState,
) -> Vec<Action> {
    match target {
        HitTarget::CommitTableRow { visible_row, .. }
        | HitTarget::CommitSignatureCell { visible_row, .. }
        | HitTarget::GraphPrintElement { visible_row, .. } => {
            if !state.log.graph.is_row_selected(*visible_row) {
                vec![
                    Action::FocusPane(PaneId::LogCommitTable),
                    Action::SelectLogRow(*visible_row),
                ]
            } else {
                vec![Action::FocusPane(PaneId::LogCommitTable)]
            }
        }
        HitTarget::CommitRefPill { visible_row, .. }
        | HitTarget::CommitRefOverflow { visible_row, .. } => {
            if state.log.graph.selected_idx != *visible_row {
                vec![
                    Action::FocusPane(PaneId::LogCommitTable),
                    Action::SelectLogRow(*visible_row),
                ]
            } else {
                Vec::new()
            }
        }
        HitTarget::StageDiffLine { row_idx } => {
            let mut out = vec![Action::FocusPane(PaneId::CommitDiffPreview)];
            let keep_visual = state
                .stage_workbench
                .visual_anchor_row()
                .is_some_and(|a| (a.min(*row_idx)..=a.max(*row_idx)).contains(row_idx));
            out.push(Action::StageDiffSelectLine {
                row_idx: *row_idx,
                extend: keep_visual,
            });
            out
        }
        HitTarget::StageDiffGutter { row_idx, .. } => vec![
            Action::FocusPane(PaneId::CommitDiffPreview),
            Action::StageDiffSelectLine {
                row_idx: *row_idx,
                extend: false,
            },
        ],
        HitTarget::DiffHunkAction { .. } => vec![Action::FocusPane(PaneId::CommitDiffPreview)],
        HitTarget::DiffArea => vec![Action::FocusPane(PaneId::LogDetailsDiff)],
        HitTarget::StashRow(i) => vec![
            Action::FocusPane(PaneId::StashList),
            Action::StashSelect(*i),
        ],
        HitTarget::LogChangeRow { idx } => {
            let mut out = vec![Action::FocusPane(PaneId::LogChangesTree)];
            if state.log.details.changes.selected != *idx {
                out.push(Action::ChangesSelect(ChangesHost::Log, *idx));
            }
            out
        }
        HitTarget::AnnotateLine { line_idx } => {
            let preserve_visual = state
                .modal_stack
                .annotate()
                .and_then(|ann| ann.selected_visual_bounds())
                .is_some_and(|(lo, hi)| (lo..=hi).contains(line_idx));
            if preserve_visual {
                Vec::new()
            } else {
                vec![Action::AnnotateSelectLine {
                    line_idx: *line_idx,
                    extend: false,
                }]
            }
        }
        HitTarget::Pane(PaneId::LogChangesTree) => vec![Action::FocusPane(PaneId::LogChangesTree)],
        HitTarget::BranchTreeNode { row: idx, .. } => {
            if state.log.branches.selected != *idx {
                vec![
                    Action::FocusPane(PaneId::LogBranchesTree),
                    Action::SelectBranchRow(*idx),
                ]
            } else {
                Vec::new()
            }
        }
        HitTarget::ChangesTreeNode { node_id, .. } => {
            let idx = node_id.0 as usize;
            if state.stage_workbench.selected_idx() != idx {
                vec![
                    Action::FocusPane(PaneId::CommitChangesTree),
                    Action::SelectCommitStageRow(idx),
                ]
            } else {
                Vec::new()
            }
        }
        HitTarget::ConsoleCommandRow { idx } => {
            if state.console.selected_idx() != *idx {
                vec![
                    Action::FocusPane(PaneId::ConsoleCommandList),
                    Action::SelectConsoleRow(*idx),
                ]
            } else {
                Vec::new()
            }
        }
        _ => Vec::new(),
    }
}

/// Pure menu-item builder for `target` after pre-selection has been applied to `state`.
pub(crate) fn build_menu_items_for_target(
    target: &HitTarget,
    state: &AppState,
) -> Option<Vec<ContextMenuItem>> {
    let items = match target {
        HitTarget::CommitTableRow { .. }
        | HitTarget::CommitSignatureCell { .. }
        | HitTarget::GraphPrintElement { .. } => {
            menus::log_commit_menu_items(state)
        }
        HitTarget::CommitRefPill {
            visible_row,
            ref_name,
            ..
        } => {
            let mut items = menus::ref_menu_items(state, ref_name);
            if items.is_empty() {
                items = menus::log_commit_menu_items(state);
            } else {
                let short = short_of_row(state, *visible_row);
                items.push(ContextMenuItem::separator());
                items.push(ContextMenuItem::submenu(
                    state.i18n.t_fmt("menu.log.commit_sub", &[("short", &short)]),
                    Some('i'),
                    menus::log_commit_menu_items(state),
                ));
                items.push(ContextMenuItem::submenu(
                    state.i18n.t("menu.log.presentation"),
                    Some('o'),
                    presentation_menu_items(state),
                ));
            }
            items
        }
        HitTarget::CommitRefOverflow { hidden_refs, .. } => hidden_refs
            .iter()
            .map(|name| {
                let mut sub = menus::ref_menu_items(state, name);
                if sub.is_empty() {
                    sub.push(ContextMenuItem::run(
                        state.i18n.t_fmt("menu.log.show_log_for", &[("name", name)]),
                        Some('l'),
                        None,
                        vec![Action::RunCliProbe(vec![
                            "log".into(),
                            "-n".into(),
                            "10".into(),
                            "--oneline".into(),
                            name.clone(),
                        ])],
                    ));
                }
                ContextMenuItem::submenu(name.clone(), None, sub)
            })
            .collect(),
        HitTarget::StageDiffLine { .. }
        | HitTarget::StageDiffGutter { .. }
        | HitTarget::Pane(PaneId::CommitDiffPreview) => menus::stage_diff_menu_items(state),
        HitTarget::DiffHunkAction { hunk_idx, .. } => menus::stage_hunk_menu_items(state, *hunk_idx),
        HitTarget::DiffArea => menus::log_diff_menu_items(state),
        HitTarget::StashRow(_)
        | HitTarget::StashChangeRow(_)
        | HitTarget::Pane(PaneId::StashList | PaneId::StashDiffPreview) => {
            menus::stash_menu_items(state)
        }
        HitTarget::OngoingBanner(_) => {
            let kind = state.ongoing_kind()?;
            menus::ongoing_menu_items(state, kind)
        }
        HitTarget::LogChangeRow { .. } | HitTarget::Pane(PaneId::LogChangesTree) => {
            menus::log_change_menu_items(state)
        }
        HitTarget::AnnotateLine { .. }
        | HitTarget::AnnotateArea
        | HitTarget::AnnotateDiffArea => menus::annotate_menu_items(state),
        HitTarget::BranchTreeNode { row: idx, .. } => {
            let idx = *idx;
            let node = state
                .log
                .branches
                .rows
                .get(idx)
                .map(|r| &state.log.branches.model.nodes[r.node]);
            let branch_name = node
                .and_then(|n| n.ref_name.clone())
                .unwrap_or_else(|| "HEAD".to_string());
            let is_leaf = node.is_some_and(|n| n.is_leaf());
            let check = |on: bool| if on { "✓" } else { " " };
            let settings = &state.log.branches.settings;
            let mut items = match node.map(|n| (n.kind, n.ref_name.clone())) {
                Some((jbgit_core::BranchNodeKind::Leaf(kind), Some(name))) => {
                    let mut b = menus::branch_menu_items(state, &name, kind);
                    if !b.is_empty() {
                        b.push(ContextMenuItem::separator());
                    }
                    b
                }
                Some((jbgit_core::BranchNodeKind::Leaf(jbgit_core::RefKind::Head), None)) => {
                    let mut b = menus::branch_menu_items(state, "", jbgit_core::RefKind::Head);
                    b.push(ContextMenuItem::separator());
                    b
                }
                Some((
                    jbgit_core::BranchNodeKind::Category(jbgit_core::BranchCategory::Remote)
                    | jbgit_core::BranchNodeKind::RemoteGroup,
                    _,
                )) => {
                    let group = node
                        .filter(|n| n.kind == jbgit_core::BranchNodeKind::RemoteGroup)
                        .map(|n| n.label.clone());
                    let mut b = menus::remote_group_menu_items(state, group.as_deref());
                    b.push(ContextMenuItem::separator());
                    b
                }
                _ => Vec::new(),
            };
            let i18n = &state.i18n;
            let nav_sc =
                catalog::shortcut_for_action(state, &[KeymapScope::Branch], &KeyAction::NavigateBranch);
            let fav_sc =
                catalog::shortcut_for_action(state, &[KeymapScope::Branch], &KeyAction::ToggleFavorite);
            let refresh_sc =
                catalog::shortcut_for_action(state, &[KeymapScope::Global], &KeyAction::Refresh);
            items.push(ContextMenuItem::run(
                if is_leaf {
                    i18n.t_fmt("menu.branch.show_only_branch", &[("name", &branch_name)])
                } else {
                    i18n.t("menu.branch.show_only_branches").to_string()
                },
                Some('f'),
                Some("Enter"),
                vec![Action::BranchesApplyFilter],
            ));
            if is_leaf {
                items.push(ContextMenuItem::run(
                    i18n.t("menu.branch.navigate_to_tip"),
                    Some('n'),
                    nav_sc.as_deref(),
                    vec![Action::BranchesNavigate],
                ));
                items.push(ContextMenuItem::run(
                    if node.is_some_and(|n| n.is_favorite) {
                        i18n.t("menu.branch.unmark_favorite")
                    } else {
                        i18n.t("menu.branch.mark_favorite")
                    },
                    Some('m'),
                    fav_sc.as_deref(),
                    vec![Action::BranchesToggleFavorite],
                ));
                items.push(ContextMenuItem::run(
                    i18n.t_fmt("menu.branch.log_branch", &[("name", &branch_name)]),
                    Some('l'),
                    None,
                    vec![Action::RunCliProbe(vec![
                        "log".into(),
                        "-n".into(),
                        "10".into(),
                        "--oneline".into(),
                        branch_name,
                    ])],
                ));
            }
            items.push(ContextMenuItem::submenu(
                i18n.t("menu.branch.view_options"),
                Some('o'),
                vec![
                    ContextMenuItem::run(
                        format!(
                            "{} {}",
                            check(settings.options.group_by_directory),
                            i18n.t("menu.branch.opt_group_by_dir")
                        ),
                        Some('d'),
                        None,
                        vec![Action::BranchesToggleGroupByDir],
                    ),
                    ContextMenuItem::run(
                        format!(
                            "{} {}",
                            check(settings.options.show_tags),
                            i18n.t("menu.branch.opt_show_tags")
                        ),
                        Some('t'),
                        None,
                        vec![Action::BranchesToggleShowTags],
                    ),
                    ContextMenuItem::run(
                        format!(
                            "{} {}",
                            check(settings.navigate_on_select),
                            i18n.t("menu.branch.opt_nav_on_select")
                        ),
                        Some('n'),
                        None,
                        vec![Action::BranchesToggleNavigateOnSelect],
                    ),
                ],
            ));
            items.push(ContextMenuItem::run(
                i18n.t("menu.branch.refresh_repo"),
                Some('r'),
                refresh_sc.as_deref(),
                vec![Action::RefreshAll],
            ));
            items
        }
        HitTarget::ChangesTreeNode { node_id, .. } => {
            let mut items = stage_tree_context_menu_items(state, node_id.0 as usize);
            items.push(ContextMenuItem::submenu(
                state.i18n.t("menu.stage.git_probes"),
                Some('p'),
                probe_submenu_items(&state.i18n),
            ));
            items
        }
        HitTarget::ConsoleCommandRow { .. } => probe_submenu_items(&state.i18n),
        _ => return None,
    };
    Some(items)
}

fn probe_submenu_items(i18n: &crate::i18n::I18n) -> Vec<ContextMenuItem> {
    vec![
        ContextMenuItem::run(
            i18n.t("menu.stage.probe_status"),
            Some('s'),
            None,
            vec![Action::RunCliProbe(vec![
                "status".into(),
                "--short".into(),
                "--branch".into(),
            ])],
        ),
        ContextMenuItem::run(
            i18n.t("menu.stage.probe_branch"),
            Some('b'),
            None,
            vec![Action::RunCliProbe(vec!["branch".into(), "-vv".into()])],
        ),
        ContextMenuItem::run(
            i18n.t("menu.stage.probe_log"),
            Some('l'),
            None,
            vec![Action::RunCliProbe(vec![
                "log".into(),
                "-n".into(),
                "10".into(),
                "--oneline".into(),
            ])],
        ),
    ]
}

fn short_of_row(state: &AppState, row: usize) -> String {
    state
        .log_commit_at(row)
        .map(|c| c.oid.short())
        .unwrap_or_default()
}

/// Synthesizes the hit target + anchor for the keyboard menu trigger: the
/// selected row of the focused pane, anchored right of its centre.
pub fn keyboard_menu_target(state: &AppState) -> Option<(HitTarget, (u16, u16))> {
    let pick = |pred: &dyn Fn(&HitTarget) -> bool| -> Option<(HitTarget, Rect)> {
        state
            .hit_registry
            .find(|t| pred(t))
            .map(|r| (r.target.clone(), r.rect))
    };
    let found = match state.focused_pane {
        PaneId::LogCommitTable => {
            let sel = state.log.graph.selected_idx;
            pick(
                &|t| matches!(t, HitTarget::CommitTableRow { visible_row, .. } if *visible_row == sel),
            )
        }
        PaneId::LogBranchesTree => {
            let sel = state.log.branches.selected;
            pick(&|t| matches!(t, HitTarget::BranchTreeNode { row, .. } if *row == sel))
        }
        PaneId::CommitChangesTree => {
            let sel = state.stage_workbench.selected_idx() as u32;
            pick(&|t| matches!(t, HitTarget::ChangesTreeNode { node_id, .. } if node_id.0 == sel))
        }
        PaneId::CommitDiffPreview => {
            let sel = state.stage_workbench.diff_cursor_row();
            pick(&|t| matches!(t, HitTarget::StageDiffLine { row_idx } if *row_idx == sel))
                .or_else(|| pick(&|t| matches!(t, HitTarget::Pane(PaneId::CommitDiffPreview))))
        }
        PaneId::LogChangesTree => {
            let sel = state.log.details.changes.selected;
            pick(&|t| matches!(t, HitTarget::LogChangeRow { idx } if *idx == sel))
                .or_else(|| pick(&|t| matches!(t, HitTarget::Pane(PaneId::LogChangesTree))))
        }
        PaneId::LogDetailsDiff => pick(&|t| matches!(t, HitTarget::DiffArea)),
        PaneId::StashList => {
            let sel = state.stash.selected();
            pick(&|t| matches!(t, HitTarget::StashRow(i) if *i == sel))
                .or_else(|| pick(&|t| matches!(t, HitTarget::Pane(PaneId::StashList))))
        }
        PaneId::StashDiffPreview => {
            pick(&|t| matches!(t, HitTarget::Pane(PaneId::StashDiffPreview)))
        }
        PaneId::ConsoleCommandList => {
            let sel = state.console.selected_idx();
            pick(&|t| matches!(t, HitTarget::ConsoleCommandRow { idx } if *idx == sel))
        }
        _ => None,
    };
    let (target, rect) = found?;
    let x = rect.x + (rect.width * 2 / 3).min(rect.width.saturating_sub(1));
    Some((target, (x, rect.y)))
}

/// `[⚙ Presentation]` menu (`Vcs.Log.PresentationSettings`).
pub fn presentation_menu_items(state: &AppState) -> Vec<ContextMenuItem> {
    use jbgit_core::graph::SortType;
    let check = |on: bool| if on { "✓" } else { " " };
    let p = &state.log.presentation;
    let o = &state.log.graph.options;
    let i18n = &state.i18n;
    let item =
        |on: bool, key: &str, mnemonic: char, shortcut: Option<&str>, t: PresentationToggle| {
            ContextMenuItem::run(
                format!("{} {}", check(on), i18n.t(key)),
                Some(mnemonic),
                shortcut,
                vec![Action::TogglePresentation(t)],
            )
        };
    vec![
        item(
            p.compact_refs,
            "presentation.compact_refs",
            'c',
            None,
            PresentationToggle::CompactRefs,
        ),
        item(
            p.show_tag_names,
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
            p.show_signature_column,
            "presentation.show_signature_column",
            's',
            None,
            PresentationToggle::ShowSignatureColumn,
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
        item(
            p.show_only_affected_changes,
            "presentation.show_only_affected_changes",
            'a',
            None,
            PresentationToggle::ShowOnlyAffectedChanges,
        ),
        ContextMenuItem::separator(),
        // Shortcuts live in `?` help; a shortcut column would widen this
        // cascading menu past the room left of its parent.
        ContextMenuItem::from_command(
            i18n.t("presentation.collapse_all"),
            Some('o'),
            None,
            catalog::graph_collapse_all(state),
        ),
        ContextMenuItem::from_command(
            i18n.t("presentation.expand_all"),
            Some('e'),
            None,
            catalog::graph_expand_all(state),
        ),
        ContextMenuItem::separator(),
        ContextMenuItem::submenu(
            i18n.t_fmt(
                "presentation.graph_renderer",
                &[("mode", i18n.graph_renderer_label(state.log.graph_renderer.mode()))],
            ),
            Some('g'),
            GraphRendererMode::ALL
                .into_iter()
                .map(|mode| {
                    let on = state.log.graph_renderer.mode() == mode;
                    ContextMenuItem::run(
                        format!(
                            "{} {}",
                            if on { "●" } else { "○" },
                            i18n.graph_renderer_label(mode)
                        ),
                        mode.label().chars().next().map(|c| c.to_ascii_lowercase()),
                        (mode == state.log.graph_renderer.mode().next()).then_some("F8"),
                        vec![Action::SetGraphRenderer(mode)],
                    )
                })
                .collect(),
        ),
    ]
}
