use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use jbgit_core::{ChangeRowKind, ModalButton, PaneId, WorkspaceTab};

use crate::app::{Action, AppState, ClickRecord, DragState, ModalLayer, PopupKind};
use crate::hit_test::{
    BranchesToolbarButton, ChangesToolbarButton, DiffToolbarButton, FileHistoryToolbarButton,
    GraphHitKind, HitTarget,
};
use crate::input_menus::{activate_top_menu_item, on_menu_hover, route_overlay_and_stash_click};
use crate::input_stage::route_stage_left_click;

use super::menus::{build_menu_for, presentation_menu_items};

pub(crate) fn route_mouse(mouse: MouseEvent, state: &mut AppState) -> Vec<Action> {
    let col = mouse.column;
    let row = mouse.row;

    match mouse.kind {
        MouseEventKind::Drag(MouseButton::Left) => route_mouse_drag(col, row, state),
        MouseEventKind::Up(MouseButton::Left) => {
            if state.drag_state.take().is_some() {
                state.needs_redraw = true;
            }
            Vec::new()
        }
        MouseEventKind::Moved => route_mouse_move(col, row, state),
        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
            let delta = if matches!(mouse.kind, MouseEventKind::ScrollUp) {
                -3
            } else {
                3
            };
            route_mouse_scroll(col, row, delta, state)
        }
        MouseEventKind::Down(MouseButton::Left) => {
            route_mouse_left_down(col, row, mouse.modifiers, state)
        }
        MouseEventKind::Down(MouseButton::Right) => route_mouse_right_down(col, row, state),
        _ => Vec::new(),
    }
}

fn route_mouse_drag(col: u16, row: u16, state: &mut AppState) -> Vec<Action> {
    match state.drag_state {
        Some(DragState::Splitter {
            splitter_id,
            axis,
            container,
        }) => {
            state
                .splitters
                .update_from_pointer(splitter_id, axis, container, col, row);
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
    }
}

fn route_mouse_move(col: u16, row: u16, state: &mut AppState) -> Vec<Action> {
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
        let visual_change = state
            .hover_target
            .as_ref()
            .is_some_and(|t| t.has_visual_hover())
            || next_target
                .as_ref()
                .is_some_and(|t| t.has_visual_hover());
        state.hover_target = next_target;
        if visual_change {
            state.needs_redraw = true;
        }
    }
    Vec::new()
}

fn route_mouse_scroll(col: u16, row: u16, delta: isize, state: &AppState) -> Vec<Action> {
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
            HitTarget::ConsoleCommandRow { .. } | HitTarget::Pane(PaneId::ConsoleCommandList) => {
                return vec![Action::ScrollConsoleList(delta / 3)];
            }
            HitTarget::ConsoleOutputArea | HitTarget::Pane(PaneId::ConsoleOutputDetail) => {
                return vec![Action::ScrollConsoleOutput(delta)];
            }
            HitTarget::ChangesTreeNode { .. } | HitTarget::Pane(PaneId::CommitChangesTree) => {
                return vec![Action::ScrollCommitStage(delta / 3)];
            }
            HitTarget::StashRow(_) | HitTarget::Pane(PaneId::StashList) => {
                return vec![Action::StashMove(delta / 3)];
            }
            HitTarget::StashChangeRow(_) | HitTarget::Pane(PaneId::StashDiffPreview) => {
                return vec![Action::StashDiffScroll(delta)];
            }
            HitTarget::FileHistoryRow { .. } | HitTarget::FileHistoryListArea => {
                return vec![Action::FileHistoryMove {
                    delta: delta / 3,
                    extend: false,
                }];
            }
            HitTarget::FileHistoryDiffArea => {
                return vec![Action::FileHistoryScrollDiff(delta)];
            }
            _ => {}
        }
    }
    Vec::new()
}

fn route_mouse_left_down(
    col: u16,
    row: u16,
    modifiers: KeyModifiers,
    state: &mut AppState,
) -> Vec<Action> {
    let is_double = state
        .last_click
        .is_some_and(|prev| prev.is_double_click(col, row));
    state.last_click = Some(ClickRecord {
        at: std::time::Instant::now(),
        col,
        row,
    });
    let ctrl = modifiers.contains(KeyModifiers::CONTROL);
    let shift = modifiers.contains(KeyModifiers::SHIFT);

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
            if let Some(ModalLayer::ContextMenu { selected, .. }) = state.modal_stack.last_mut() {
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
        HitTarget::BranchWidgetPill => vec![Action::OpenBranchesPopup {
            query: String::new(),
        }],
        HitTarget::HeaderRefreshButton => vec![Action::RefreshAll],
        HitTarget::HeaderGlyphButton => vec![Action::ToggleGlyphMode],
        HitTarget::HeaderHelpButton => vec![Action::OpenPopup {
            kind: PopupKind::ShortcutsHelp,
            initial_query: String::new(),
        }],
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
        HitTarget::ConsoleCommandRow { idx } => vec![
            Action::FocusPane(PaneId::ConsoleCommandList),
            Action::SelectConsoleRow(idx),
        ],
        HitTarget::ConsoleOutputArea => vec![Action::FocusPane(PaneId::ConsoleOutputDetail)],
        HitTarget::FileHistoryRow { idx } => {
            let mut actions = vec![
                Action::FileHistorySetFocus(crate::log_view::FileHistoryFocus::Commits),
                Action::FileHistorySelect { idx, extend: shift },
            ];
            if is_double && !shift {
                actions.push(Action::FileHistoryJumpToLog);
            }
            actions
        }
        HitTarget::FileHistoryListArea => {
            vec![Action::FileHistorySetFocus(
                crate::log_view::FileHistoryFocus::Commits,
            )]
        }
        HitTarget::FileHistoryDiffArea => {
            vec![Action::FileHistorySetFocus(
                crate::log_view::FileHistoryFocus::Diff,
            )]
        }
        HitTarget::FileHistoryToolbar(btn) => match btn {
            FileHistoryToolbarButton::FollowRenames => vec![Action::FileHistoryToggleFollow],
            FileHistoryToolbarButton::CompareLocal => {
                vec![Action::FileHistoryToggleCompareLocal]
            }
            FileHistoryToolbarButton::SideBySide => vec![Action::FileHistoryToggleSideBySide],
            FileHistoryToolbarButton::Context => vec![Action::FileHistoryCycleContext],
            FileHistoryToolbarButton::Zoom => vec![Action::FileHistoryToggleZoom],
            FileHistoryToolbarButton::LocateInLog => vec![Action::FileHistoryJumpToLog],
        },
        HitTarget::Pane(pane) => vec![Action::FocusPane(pane)],
        other => {
            if let Some(actions) = route_overlay_and_stash_click(&other, state, is_double) {
                return actions;
            }
            route_stage_left_click(other, state, col, row, ctrl, shift, is_double)
                .unwrap_or_default()
        }
    }
}

fn route_mouse_right_down(col: u16, row: u16, state: &mut AppState) -> Vec<Action> {
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
            | HitTarget::ModalButton(_)
            | HitTarget::BranchPopupStar(_)
            | HitTarget::BranchPopupFetch
            | HitTarget::BranchPopupSettings
            | HitTarget::FileHistoryRow { .. }
            | HitTarget::FileHistoryToolbar(_)
            | HitTarget::FileHistoryListArea
            | HitTarget::FileHistoryDiffArea
    ) {
        return Vec::new();
    }
    if let HitTarget::BranchPopupRow(idx) = region.target {
        return route_overlay_and_stash_click(&HitTarget::BranchPopupRow(idx), state, false)
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
