use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use jbgit_core::{ChangeRowKind, PaneId, WorkspaceTab};

use crate::app::{Action, AppState, ChangesHost, DiffScope, DragState, PopupKind};
use crate::hit_test::{
    BranchesToolbarButton, ChangesToolbarButton, DiffToolbarButton, FileHistoryToolbarButton,
    GraphHitKind, HitTarget,
};
use crate::input_stage::route_stage_left_click;

use super::menus::{presentation_menu_items, route_overlay_and_stash_click};

pub(crate) fn route_mouse(mouse: MouseEvent, state: &AppState) -> Vec<Action> {
    let col = mouse.column;
    let row = mouse.row;

    match mouse.kind {
        MouseEventKind::Drag(MouseButton::Left) => route_mouse_drag(col, row, state),
        MouseEventKind::Up(MouseButton::Left) => {
            if state.drag_state.is_some() {
                vec![Action::SetDragState(None)]
            } else {
                Vec::new()
            }
        }
        MouseEventKind::Moved => vec![Action::MouseMove { col, row }],
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

fn route_mouse_drag(col: u16, row: u16, state: &AppState) -> Vec<Action> {
    match state.drag_state {
        Some(DragState::Splitter { .. }) => vec![Action::DragSplitter { col, row }],
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
                return vec![Action::ChangesMove(ChangesHost::Log, delta / 3)];
            }
            HitTarget::DiffArea
            | HitTarget::StageDiffLine { .. }
            | HitTarget::StageDiffGutter { .. }
            | HitTarget::Pane(PaneId::CommitDiffPreview) => {
                return vec![if state.active_tab == WorkspaceTab::Commit {
                    Action::DiffScroll(DiffScope::Stage, delta)
                } else {
                    Action::DiffScroll(DiffScope::Log, delta)
                }];
            }
            HitTarget::DetailsArea
            | HitTarget::DetailsHash { .. }
            | HitTarget::DetailsParent { .. }
            | HitTarget::Pane(PaneId::LogDetailsDiff) => {
                return vec![if state.log.details.diff_visible() {
                    Action::DiffScroll(DiffScope::Log, delta)
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
                return vec![Action::DiffScroll(DiffScope::Stash, delta)];
            }
            HitTarget::FileHistoryRow { .. } | HitTarget::FileHistoryListArea => {
                return vec![Action::FileHistoryMove {
                    delta: delta / 3,
                    extend: false,
                }];
            }
            HitTarget::FileHistoryDiffArea => {
                return vec![Action::DiffScroll(DiffScope::FileHistory, delta)];
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
    state: &AppState,
) -> Vec<Action> {
    let is_double = state
        .last_click
        .is_some_and(|prev| prev.is_double_click(col, row));
    let mut out = vec![Action::MouseRecordClick { col, row }];
    let ctrl = modifiers.contains(KeyModifiers::CONTROL);
    let shift = modifiers.contains(KeyModifiers::SHIFT);

    let Some(region) = state.hit_registry.hit_test(col, row).cloned() else {
        return out;
    };

    let follow = match region.target {
        HitTarget::ModalBackdrop { layer_z } if (30..40).contains(&layer_z) => {
            vec![Action::PopContextMenus]
        }
        HitTarget::ModalBackdrop { .. } => vec![Action::PopModal],
        HitTarget::ModalButton(_) => vec![Action::PopModal],
        HitTarget::MenuItem {
            menu_depth,
            item_idx,
        } => vec![Action::ContextMenuActivateItem {
            depth: menu_depth,
            idx: item_idx,
        }],
        HitTarget::Splitter { id, axis } => {
            if is_double {
                vec![Action::ResetSplitter(id)]
            } else if let Some((_, container)) = state.splitter_containers.get(&id).copied() {
                vec![Action::SetDragState(Some(DragState::Splitter {
                    splitter_id: id,
                    axis,
                    container,
                }))]
            } else {
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
                vec![
                    Action::FocusPane(PaneId::LogBranchesTree),
                    Action::BranchesToggleMulti(idx),
                ]
            } else {
                let mut actions = vec![
                    Action::FocusPane(PaneId::LogBranchesTree),
                    Action::SelectBranchRow(idx),
                ];
                if is_double {
                    actions.push(Action::BranchesApplyFilter);
                }
                actions
            }
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
            let mut initial = state.log.filter.field_expr(field);
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
                Action::ChangesSelect(ChangesHost::Log, idx),
            ];
            if is_double {
                match state.log.details.change_rows(&state.diff_caches).get(idx) {
                    Some(r) if matches!(r.kind, ChangeRowKind::Dir { .. }) => {
                        actions.push(Action::ChangesExpand(ChangesHost::Log, !r.expanded));
                    }
                    Some(_) => actions.push(Action::ZoomDiff(true)),
                    None => {}
                }
            }
            actions
        }
        HitTarget::ChangesToolbar(btn) => match btn {
            ChangesToolbarButton::GroupByDir => {
                vec![Action::ChangesToggleGroupByDir(ChangesHost::Log)]
            }
            ChangesToolbarButton::ExpandAll => vec![Action::ChangesExpandAll(ChangesHost::Log, true)],
            ChangesToolbarButton::CollapseAll => {
                vec![Action::ChangesExpandAll(ChangesHost::Log, false)]
            }
            ChangesToolbarButton::DiffPreview => vec![Action::ToggleDiffPreview],
            ChangesToolbarButton::SwitchParent => vec![Action::SwitchMergeParent],
        },
        HitTarget::DetailsHash { oid } => vec![Action::CopyText(oid.to_string())],
        HitTarget::DetailsParent { oid } => vec![Action::JumpToCommit(oid)],
        HitTarget::DetailsArea | HitTarget::DiffArea => {
            vec![Action::FocusPane(PaneId::LogDetailsDiff)]
        }
        HitTarget::DiffToolbar(btn) => match btn {
            DiffToolbarButton::SideBySide => vec![Action::DiffToggleSideBySide(DiffScope::Log)],
            DiffToolbarButton::Context => vec![Action::DiffCycleContext(DiffScope::Log)],
            DiffToolbarButton::Dock => vec![Action::DiffCycleDock],
            DiffToolbarButton::Zoom => vec![Action::ZoomDiff(!state.log.details.diff.zoomed)],
            DiffToolbarButton::Close => {
                if state.log.details.diff.zoomed {
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
                    vec![
                        Action::FocusPane(PaneId::LogCommitTable),
                        Action::ExtendLogSelectionTo(visible_row),
                    ]
                } else {
                    let mut actions = vec![
                        Action::SetDragState(Some(DragState::LogSelection)),
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
            }
        },
        HitTarget::CommitTableRow {
            visible_row,
            commit_oid,
        } => {
            if shift {
                vec![
                    Action::FocusPane(PaneId::LogCommitTable),
                    Action::ExtendLogSelectionTo(visible_row),
                ]
            } else {
                let mut actions = vec![
                    Action::SetDragState(Some(DragState::LogSelection)),
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
            FileHistoryToolbarButton::SideBySide => {
                vec![Action::DiffToggleSideBySide(DiffScope::FileHistory)]
            }
            FileHistoryToolbarButton::Context => {
                vec![Action::DiffCycleContext(DiffScope::FileHistory)]
            }
            FileHistoryToolbarButton::Zoom => vec![Action::FileHistoryToggleZoom],
            FileHistoryToolbarButton::LocateInLog => vec![Action::FileHistoryJumpToLog],
        },
        HitTarget::Pane(pane) => vec![Action::FocusPane(pane)],
        other => {
            if let Some(actions) = route_overlay_and_stash_click(&other, state, is_double) {
                actions
            } else {
                route_stage_left_click(other, state, col, row, ctrl, shift, is_double)
                    .unwrap_or_default()
            }
        }
    };
    out.extend(follow);
    out
}

fn route_mouse_right_down(col: u16, row: u16, state: &AppState) -> Vec<Action> {
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

    vec![Action::OpenContextMenuForTarget {
        target: region.target,
        anchor: (col, row),
    }]
}
