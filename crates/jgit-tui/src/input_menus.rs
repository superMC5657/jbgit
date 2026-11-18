//! Spec 05 input routing: cascading context-menu navigation (separator
//! skipping, disabled items, mnemonics, 150 ms hover-to-open), the unified
//! right-click / keyboard (`Menu`, `Shift+F10`, `.`, `m`) context-menu
//! builder, dialogs, the branches popup, the ongoing banner and the Stash tab.

use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use jgit_core::{ConflictChoice, GitOp, OngoingStep, PaneId, WorkspaceTab};
use ratatui::layout::Rect;

use crate::app::{
    Action, AppState, ContextMenuEntryKind, ContextMenuItem, MENU_HOVER_DELAY, MenuHover,
    MenuParent, ModalLayer, PopupKind,
};
use crate::branches_popup::activate_row;
use crate::dialogs::{Dialog, DialogKey, route_dialog_key};
use crate::hit_test::{
    ChangesToolbarButton, ConflictMergeButtonKind, DialogButtonKind, DiffToolbarButton, HitTarget,
    OngoingButton, StashButton,
};
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
        .is_some_and(|l| {
            matches!(
                l,
                ModalLayer::FloatingPopup {
                    kind: PopupKind::BranchesQuickView,
                    ..
                }
            )
        })
}

/// Activates item `idx` of the topmost context menu.
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
        ContextMenuEntryKind::Action(act) => {
            let mut out = vec![Action::PopContextMenus];
            if menus_over_branches_popup(state) {
                out.push(Action::PopModal);
            }
            out.extend(crate::input::resolve_menu_action(act.clone()));
            out
        }
    }
}

pub fn route_context_menu_key(key: KeyEvent, state: &mut AppState) -> Vec<Action> {
    let Some(ModalLayer::ContextMenu {
        items,
        selected,
        depth,
        ..
    }) = state.modal_stack.last_mut()
    else {
        return Vec::new();
    };
    let _ = depth;
    let redraw = |state_sel: &mut usize, to: usize| {
        *state_sel = to;
    };
    let actions = match key.code {
        KeyCode::Esc => vec![Action::PopModal],
        KeyCode::Left => vec![Action::PopModal],
        KeyCode::Up | KeyCode::Char('k') if !items.is_empty() => {
            let to = step_selectable(items, *selected, -1);
            redraw(selected, to);
            Vec::new()
        }
        KeyCode::Down | KeyCode::Char('j') if !items.is_empty() => {
            let to = step_selectable(items, *selected, 1);
            redraw(selected, to);
            Vec::new()
        }
        KeyCode::Home if !items.is_empty() => {
            let to = step_selectable(items, items.len() - 1, 1);
            redraw(selected, to);
            Vec::new()
        }
        KeyCode::End if !items.is_empty() => {
            let to = step_selectable(items, 0, -1);
            redraw(selected, to);
            Vec::new()
        }
        KeyCode::Right => {
            let idx = *selected;
            if items
                .get(idx)
                .is_some_and(|i| matches!(i.kind, ContextMenuEntryKind::Submenu(_)))
            {
                return activate_top_menu_item(state, idx);
            }
            Vec::new()
        }
        KeyCode::Enter | KeyCode::Char(' ') => {
            let idx = *selected;
            return activate_top_menu_item(state, idx);
        }
        KeyCode::Delete => {
            if let Some(idx) = items
                .iter()
                .position(|i| i.is_enabled() && i.shortcut.as_deref() == Some("Del"))
            {
                return activate_top_menu_item(state, idx);
            }
            Vec::new()
        }
        KeyCode::Char(ch)
            if !key.modifiers.contains(KeyModifiers::CONTROL)
                && !key.modifiers.contains(KeyModifiers::ALT) =>
        {
            let target = ch.to_ascii_lowercase();
            if let Some(idx) = items.iter().position(|i| {
                !i.is_separator() && i.mnemonic.is_some_and(|m| m.to_ascii_lowercase() == target)
            }) {
                return activate_top_menu_item(state, idx);
            }
            Vec::new()
        }
        _ => Vec::new(),
    };
    state.needs_redraw = true;
    actions
}

/// Pointer moved over a menu item: select it and (re)arm the 150 ms
/// hover timer that opens its submenu or closes a sibling's submenu.
pub fn on_menu_hover(state: &mut AppState, menu_depth: u8, item_idx: usize) {
    let Some(pos) = state
        .modal_stack
        .iter()
        .rposition(|l| matches!(l, ModalLayer::ContextMenu { depth, .. } if *depth == menu_depth))
    else {
        return;
    };
    let (is_submenu, changed) = match &mut state.modal_stack[pos] {
        ModalLayer::ContextMenu {
            items, selected, ..
        } => {
            let Some(item) = items.get(item_idx) else {
                return;
            };
            if item.is_separator() {
                return;
            }
            let changed = *selected != item_idx;
            *selected = item_idx;
            (
                matches!(item.kind, ContextMenuEntryKind::Submenu(_)) && item.is_enabled(),
                changed,
            )
        }
        _ => return,
    };
    let child_open_for = match state.modal_stack.get(pos + 1) {
        Some(ModalLayer::ContextMenu {
            parent: Some(p), ..
        }) => Some(p.item_idx),
        _ => None,
    };
    let needs_timer = (is_submenu && child_open_for != Some(item_idx))
        || (child_open_for.is_some() && child_open_for != Some(item_idx));
    if needs_timer {
        let already = state
            .menu_hover
            .is_some_and(|h| h.depth == menu_depth && h.item_idx == item_idx);
        if !already {
            state.menu_hover = Some(MenuHover {
                depth: menu_depth,
                item_idx,
                due: std::time::Instant::now() + MENU_HOVER_DELAY,
            });
        }
    } else {
        state.menu_hover = None;
    }
    if changed {
        state.needs_redraw = true;
    }
}

/// Fires a due hover timer: closes deeper submenus and opens the hovered one.
pub fn flush_menu_hover(state: &mut AppState, now: Instant) {
    let Some(h) = state.menu_hover else {
        return;
    };
    if h.due > now {
        return;
    }
    state.menu_hover = None;
    let Some(pos) = state
        .modal_stack
        .iter()
        .rposition(|l| matches!(l, ModalLayer::ContextMenu { depth, .. } if *depth == h.depth))
    else {
        return;
    };
    if let Some(ModalLayer::ContextMenu {
        parent: Some(p),
        depth,
        ..
    }) = state.modal_stack.get(pos + 1)
        && *depth == h.depth + 1
        && p.item_idx == h.item_idx
    {
        return;
    }
    state.modal_stack.truncate(pos + 1);
    let screen = state.terminal_area;
    if let Some(ModalLayer::ContextMenu {
        anchor,
        items,
        parent,
        ..
    }) = state.modal_stack.get(pos)
        && let Some(item) = items.get(h.item_idx)
        && item.is_enabled()
        && let ContextMenuEntryKind::Submenu(children) = &item.kind
    {
        let rect = context_menu_rect(screen, *anchor, *parent, items);
        let row_y = menu_item_row_y(screen, rect, items, h.item_idx);
        let children = children.clone();
        let _ = state.dispatch(Action::OpenSubmenu {
            parent: MenuParent {
                rect,
                row_y,
                item_idx: h.item_idx,
            },
            items: children,
            depth: h.depth + 1,
        });
    }
    state.needs_redraw = true;
}

/// Keyboard routing for the built-in three-pane conflict merge modal.
pub fn route_conflict_merge_key(key: &KeyEvent, state: &AppState) -> Vec<Action> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let path = state
        .conflict_merge
        .as_ref()
        .map(|cm| cm.path.clone())
        .unwrap_or_default();
    match key.code {
        KeyCode::Esc => vec![Action::PopModal],
        KeyCode::Enter => vec![Action::ConflictSaveAndStage],
        KeyCode::Char('s') if ctrl => vec![Action::ConflictSaveAndStage],
        KeyCode::Down | KeyCode::Char('j') if !ctrl && !alt => vec![Action::ConflictScroll(1)],
        KeyCode::Up | KeyCode::Char('k') if !ctrl && !alt => vec![Action::ConflictScroll(-1)],
        KeyCode::PageDown => vec![Action::ConflictScroll(10)],
        KeyCode::PageUp => vec![Action::ConflictScroll(-10)],
        KeyCode::Char('n') | KeyCode::Tab if !ctrl && !alt => vec![Action::ConflictSelectBlock(1)],
        KeyCode::Char('p') | KeyCode::BackTab if !ctrl && !alt => {
            vec![Action::ConflictSelectBlock(-1)]
        }
        KeyCode::Char('1' | 'y') if !ctrl && !alt => {
            vec![Action::ConflictApplyChoice(ConflictChoice::Ours)]
        }
        KeyCode::Char('2' | 't') if !ctrl && !alt => {
            vec![Action::ConflictApplyChoice(ConflictChoice::Theirs)]
        }
        KeyCode::Char('3' | 'b') if !ctrl && !alt => {
            vec![Action::ConflictApplyChoice(ConflictChoice::BothOursFirst)]
        }
        KeyCode::Char('0' | 'x') if !ctrl && !alt => {
            vec![Action::ConflictApplyChoice(ConflictChoice::Unresolved)]
        }
        KeyCode::Char('Y') if !ctrl && !alt => {
            vec![Action::ConflictAcceptAll(ConflictChoice::Ours)]
        }
        KeyCode::Char('T') if !ctrl && !alt => {
            vec![Action::ConflictAcceptAll(ConflictChoice::Theirs)]
        }
        KeyCode::Char('m') if !ctrl && !alt && !path.is_empty() => {
            vec![Action::OpenExternalMergetool(path)]
        }
        KeyCode::Char('e') if !ctrl && !alt && !path.is_empty() => {
            vec![Action::OpenExternalEditorFile(Some(path))]
        }
        _ => Vec::new(),
    }
}

/// Keyboard routing of the topmost dialog.
pub fn route_dialog(key: &KeyEvent, state: &mut AppState) -> Vec<Action> {
    let Some(ModalLayer::Dialog(d)) = state.modal_stack.last_mut() else {
        return Vec::new();
    };
    match route_dialog_key(d, key) {
        DialogKey::None => Vec::new(),
        DialogKey::Redraw => {
            state.needs_redraw = true;
            Vec::new()
        }
        DialogKey::Cancel => vec![Action::PopModal],
        DialogKey::Confirm => vec![Action::DialogConfirm],
        DialogKey::Actions(a) => {
            state.needs_redraw = true;
            a
        }
    }
}

/// Left-clicks on Spec 05 hit targets (dialogs, popup, banner, stash, …).
pub fn route_spec05_click(
    target: &HitTarget,
    state: &mut AppState,
    is_double: bool,
) -> Option<Vec<Action>> {
    let actions = match target {
        HitTarget::MenuBody { .. } | HitTarget::DialogBody => Vec::new(),
        HitTarget::DialogOption(i) => {
            let i = *i;
            let mut confirm = false;
            if let Some(ModalLayer::Dialog(d)) = state.modal_stack.last_mut() {
                match d.as_mut() {
                    Dialog::Choice(c) => {
                        c.selected = i.min(c.options.len().saturating_sub(1));
                        confirm = !c.radio || is_double;
                    }
                    Dialog::RebaseTodo(r) => {
                        r.selected = i.min(r.entries.len().saturating_sub(1));
                    }
                    _ => {}
                }
            }
            state.needs_redraw = true;
            if confirm {
                vec![Action::DialogConfirm]
            } else {
                Vec::new()
            }
        }
        HitTarget::DialogButton(DialogButtonKind::Confirm) => vec![Action::DialogConfirm],
        HitTarget::DialogButton(DialogButtonKind::Cancel) => vec![Action::PopModal],
        HitTarget::DialogCheckbox => {
            if let Some(ModalLayer::Dialog(d)) = state.modal_stack.last_mut()
                && let Dialog::Input(i) = d.as_mut()
                && let Some((_, on)) = i.checkbox.as_mut()
            {
                *on = !*on;
            }
            state.needs_redraw = true;
            Vec::new()
        }
        HitTarget::BranchPopupRow(idx) => {
            let idx = *idx;
            let query = match state.modal_stack.last_mut() {
                Some(ModalLayer::FloatingPopup {
                    kind: PopupKind::BranchesQuickView,
                    query,
                    selected,
                }) => {
                    *selected = idx;
                    query.clone()
                }
                _ => return Some(Vec::new()),
            };
            activate_row(state, &query, idx)
        }
        HitTarget::BranchPopupStar(idx) => {
            let query = match state.modal_stack.last() {
                Some(ModalLayer::FloatingPopup { query, .. }) => query.clone(),
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
            let o = &state.branches.settings.options;
            vec![Action::OpenContextMenu {
                anchor: (rect.right().saturating_sub(20), rect.y + 1),
                items: vec![
                    ContextMenuItem::run(
                        format!(
                            "{} Group by Directory (Branches pane)",
                            check(o.group_by_directory)
                        ),
                        Some('d'),
                        None,
                        vec![Action::BranchesToggleGroupByDir],
                    ),
                    ContextMenuItem::run(
                        format!("{} Show Tags (Branches pane)", check(o.show_tags)),
                        Some('t'),
                        None,
                        vec![Action::BranchesToggleShowTags],
                    ),
                    ContextMenuItem::separator(),
                    ContextMenuItem::run(
                        "Fetch All Remotes",
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
                    vec![Action::Notice(format!(
                        "Resolve {} conflicted file(s) and stage them before continuing",
                        state.conflicted_count()
                    ))]
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
            Action::StashChangesSelect(*i),
        ],
        HitTarget::StashToolbar(btn) => menus::stash_button_actions(state, *btn),
        HitTarget::StashChangesToolbar(btn) => match btn {
            ChangesToolbarButton::GroupByDir => vec![Action::StashChangesToggleGroupByDir],
            ChangesToolbarButton::ExpandAll => vec![Action::StashChangesExpandAll(true)],
            ChangesToolbarButton::CollapseAll => vec![Action::StashChangesExpandAll(false)],
            _ => Vec::new(),
        },
        HitTarget::StashDiffToolbar(btn) => match btn {
            DiffToolbarButton::SideBySide => vec![Action::StashDiffToggleSideBySide],
            DiffToolbarButton::Context => vec![Action::StashDiffCycleContext],
            _ => Vec::new(),
        },
        HitTarget::ConflictMergeBlock(id) => vec![Action::ConflictSetBlock(*id)],
        HitTarget::ConflictMergeButton(btn) => {
            let path = state
                .conflict_merge
                .as_ref()
                .map(|cm| cm.path.clone())
                .unwrap_or_default();
            match btn {
                ConflictMergeButtonKind::AcceptOurs => {
                    vec![Action::ConflictApplyChoice(ConflictChoice::Ours)]
                }
                ConflictMergeButtonKind::AcceptTheirs => {
                    vec![Action::ConflictApplyChoice(ConflictChoice::Theirs)]
                }
                ConflictMergeButtonKind::AcceptBoth => {
                    vec![Action::ConflictApplyChoice(ConflictChoice::BothOursFirst)]
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
        _ => return None,
    };
    Some(actions)
}

/// Stash tab keys (take priority over global single-letter shortcuts).
pub fn route_stash_key(key: &KeyEvent, state: &AppState) -> Option<Vec<Action>> {
    if state.active_tab != WorkspaceTab::Stash
        || key.modifiers.contains(KeyModifiers::CONTROL)
        || key.modifiers.contains(KeyModifiers::ALT)
    {
        return None;
    }
    match state.focused_pane {
        PaneId::StashList => Some(match key.code {
            KeyCode::Down | KeyCode::Char('j') => vec![Action::StashMove(1)],
            KeyCode::Up | KeyCode::Char('k') => vec![Action::StashMove(-1)],
            KeyCode::Home => vec![Action::StashSelect(0)],
            KeyCode::End => vec![Action::StashSelect(usize::MAX)],
            KeyCode::Enter | KeyCode::Char('a') => {
                menus::stash_button_actions(state, StashButton::Apply)
            }
            KeyCode::Char('p') => menus::stash_button_actions(state, StashButton::Pop),
            KeyCode::Char('b') => menus::stash_button_actions(state, StashButton::Branch),
            KeyCode::Char('n') => menus::stash_button_actions(state, StashButton::New),
            KeyCode::Char('d') | KeyCode::Delete => {
                menus::stash_button_actions(state, StashButton::Drop)
            }
            KeyCode::Char('C') => menus::stash_button_actions(state, StashButton::Clear),
            KeyCode::Right => vec![Action::FocusPane(PaneId::StashDiffPreview)],
            _ => return None,
        }),
        PaneId::StashDiffPreview => Some(match key.code {
            KeyCode::Down | KeyCode::Char('j') => vec![Action::StashChangesMove(1)],
            KeyCode::Up | KeyCode::Char('k') => vec![Action::StashChangesMove(-1)],
            KeyCode::Right => vec![Action::StashChangesExpand(true)],
            KeyCode::Left => vec![Action::StashChangesExpand(false)],
            KeyCode::Char('v') => vec![Action::StashChangesToggleGroupByDir],
            KeyCode::Char('e') => vec![Action::StashChangesExpandAll(true)],
            KeyCode::Char('E') => vec![Action::StashChangesExpandAll(false)],
            KeyCode::Char('s') => vec![Action::StashDiffToggleSideBySide],
            KeyCode::Char('c') => vec![Action::StashDiffCycleContext],
            KeyCode::PageDown | KeyCode::Char(' ') => vec![Action::StashDiffScroll(10)],
            KeyCode::PageUp => vec![Action::StashDiffScroll(-10)],
            KeyCode::Esc => vec![Action::FocusPane(PaneId::StashList)],
            _ => return None,
        }),
        _ => None,
    }
}

/// Builds the context menu for `target`, emitting pre-selection actions
/// (right-click on an unselected row selects it first; a row inside the
/// current multi-selection keeps the whole selection).
pub fn context_menu_for_target(
    target: &HitTarget,
    state: &mut AppState,
    actions: &mut Vec<Action>,
) -> Option<Vec<ContextMenuItem>> {
    let items = match target {
        HitTarget::CommitTableRow { visible_row, .. }
        | HitTarget::GraphPrintElement { visible_row, .. } => {
            if !state.log_graph.is_row_selected(*visible_row) {
                state.log_graph.select_row(*visible_row);
                actions.push(Action::FocusPane(PaneId::LogCommitTable));
                actions.push(Action::SelectLogRow(*visible_row));
            } else {
                actions.push(Action::FocusPane(PaneId::LogCommitTable));
            }
            menus::log_commit_menu_items(state)
        }
        HitTarget::CommitRefPill {
            visible_row,
            ref_name,
            ..
        } => {
            if state.log_graph.selected_idx != *visible_row {
                state.log_graph.select_row(*visible_row);
                actions.push(Action::FocusPane(PaneId::LogCommitTable));
                actions.push(Action::SelectLogRow(*visible_row));
            }
            let mut items = menus::ref_menu_items(state, ref_name);
            if items.is_empty() {
                items = menus::log_commit_menu_items(state);
            } else {
                items.push(ContextMenuItem::separator());
                items.push(ContextMenuItem::submenu(
                    format!("Commit {}", short_of_row(state, *visible_row)),
                    Some('i'),
                    menus::log_commit_menu_items(state),
                ));
                items.push(ContextMenuItem::submenu(
                    "Presentation",
                    Some('o'),
                    crate::input::presentation_menu_items(state),
                ));
            }
            items
        }
        HitTarget::CommitRefOverflow {
            visible_row,
            hidden_refs,
        } => {
            if state.log_graph.selected_idx != *visible_row {
                state.log_graph.select_row(*visible_row);
                actions.push(Action::FocusPane(PaneId::LogCommitTable));
                actions.push(Action::SelectLogRow(*visible_row));
            }
            hidden_refs
                .iter()
                .map(|name| {
                    let mut sub = menus::ref_menu_items(state, name);
                    if sub.is_empty() {
                        sub.push(ContextMenuItem::run(
                            format!("Show log for {name}"),
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
                .collect()
        }
        HitTarget::StageDiffLine { row_idx } => {
            state.stage_workbench.diff_cursor_row = *row_idx;
            if state
                .stage_workbench
                .visual_anchor_row
                .is_some_and(|a| !(a.min(*row_idx)..=a.max(*row_idx)).contains(row_idx))
            {
                state.stage_workbench.visual_anchor_row = None;
            }
            actions.push(Action::FocusPane(PaneId::CommitDiffPreview));
            menus::stage_diff_menu_items(state)
        }
        HitTarget::StageDiffGutter { row_idx, .. } => {
            state.stage_workbench.diff_cursor_row = *row_idx;
            actions.push(Action::FocusPane(PaneId::CommitDiffPreview));
            menus::stage_diff_menu_items(state)
        }
        HitTarget::DiffHunkAction { hunk_idx, .. } => {
            actions.push(Action::FocusPane(PaneId::CommitDiffPreview));
            menus::stage_hunk_menu_items(state, *hunk_idx)
        }
        HitTarget::Pane(PaneId::CommitDiffPreview) => menus::stage_diff_menu_items(state),
        HitTarget::DiffArea => {
            actions.push(Action::FocusPane(PaneId::LogDetailsDiff));
            menus::log_diff_menu_items(state)
        }
        HitTarget::StashRow(i) => {
            state.stash.selected = *i;
            actions.push(Action::FocusPane(PaneId::StashList));
            actions.push(Action::StashSelect(*i));
            menus::stash_menu_items(state)
        }
        HitTarget::StashChangeRow(_)
        | HitTarget::Pane(PaneId::StashList | PaneId::StashDiffPreview) => {
            menus::stash_menu_items(state)
        }
        HitTarget::OngoingBanner(_) => {
            let kind = state.ongoing_kind()?;
            menus::ongoing_menu_items(state, kind)
        }
        _ => return None,
    };
    Some(items)
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
            let sel = state.log_graph.selected_idx;
            pick(
                &|t| matches!(t, HitTarget::CommitTableRow { visible_row, .. } if *visible_row == sel),
            )
        }
        PaneId::LogBranchesTree => {
            let sel = state.branches.selected;
            pick(&|t| matches!(t, HitTarget::BranchTreeNode { row, .. } if *row == sel))
        }
        PaneId::CommitChangesTree => {
            let sel = state.commit_stage_selected_idx as u32;
            pick(&|t| matches!(t, HitTarget::ChangesTreeNode { node_id, .. } if node_id.0 == sel))
        }
        PaneId::CommitDiffPreview => {
            let sel = state.stage_workbench.diff_cursor_row;
            pick(&|t| matches!(t, HitTarget::StageDiffLine { row_idx } if *row_idx == sel))
                .or_else(|| pick(&|t| matches!(t, HitTarget::Pane(PaneId::CommitDiffPreview))))
        }
        PaneId::LogDetailsDiff => pick(&|t| matches!(t, HitTarget::DiffArea)),
        PaneId::StashList => {
            let sel = state.stash.selected;
            pick(&|t| matches!(t, HitTarget::StashRow(i) if *i == sel))
                .or_else(|| pick(&|t| matches!(t, HitTarget::Pane(PaneId::StashList))))
        }
        PaneId::StashDiffPreview => {
            pick(&|t| matches!(t, HitTarget::Pane(PaneId::StashDiffPreview)))
        }
        PaneId::ConsoleCommandList => {
            let sel = state.console_selected_idx;
            pick(&|t| matches!(t, HitTarget::ConsoleCommandRow { idx } if *idx == sel))
        }
        _ => None,
    };
    let (target, rect) = found?;
    let x = rect.x + (rect.width * 2 / 3).min(rect.width.saturating_sub(1));
    Some((target, (x, rect.y)))
}
