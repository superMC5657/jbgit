use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use jbgit_core::{ChangeRowKind, PaneId, WorkspaceTab};

use crate::app::{Action, AppState, ChangesHost, DiffScope, DragState, PopupKind};
use crate::hit_test::{
    AnnotateToolbarButton, BranchesToolbarButton, ChangesToolbarButton, DiffToolbarButton,
    FileHistoryToolbarButton, GraphHitKind, HitTarget, PushActionButtonKind, PushOptionKind,
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
                    | HitTarget::CommitSignatureCell { visible_row, .. }
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
            HitTarget::TreeBrowserRow(_) => {
                return vec![Action::TreeBrowser(
                    crate::tree_browser::TreeBrowserAction::Move(delta / 3),
                )];
            }
            HitTarget::TreeBrowserPreview => {
                return vec![Action::TreeBrowser(
                    crate::tree_browser::TreeBrowserAction::ScrollPreview(delta),
                )];
            }
            HitTarget::TreeBrowserSearchRow(_) => {
                return vec![Action::TreeBrowser(
                    crate::tree_browser::TreeBrowserAction::SearchMove(delta / 3),
                )];
            }
            HitTarget::ApplyPatchFileRow(_) | HitTarget::ApplyPatchFileCheck(_) => {
                return vec![Action::ApplyPatch(
                    crate::patch_view::ApplyPatchAction::Move(delta / 3),
                )];
            }
            HitTarget::ApplyPatchPreview => {
                return vec![Action::ApplyPatch(
                    crate::patch_view::ApplyPatchAction::ScrollPreview(delta),
                )];
            }
            HitTarget::BranchTreeNode { .. }
            | HitTarget::BranchTreeToggle { .. }
            | HitTarget::Pane(PaneId::LogBranchesTree) => {
                return vec![Action::ScrollBranches(delta / 3)];
            }
            HitTarget::CommitTableRow { .. }
            | HitTarget::CommitSignatureCell { .. }
            | HitTarget::LogShallowUnshallow
            | HitTarget::LogLoadMore
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
            | HitTarget::DetailsCommitHash { .. }
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
            HitTarget::StashChangeRow(_) | HitTarget::StashChangesToolbar(_) => {
                return vec![Action::ChangesMove(ChangesHost::Stash, delta / 3)];
            }
            HitTarget::Pane(PaneId::StashDiffPreview) => {
                return vec![Action::DiffScroll(DiffScope::Stash, delta)];
            }
            HitTarget::FileHistoryRow { .. } | HitTarget::FileHistoryListArea => {
                return vec![Action::FileHistoryMove {
                    delta: delta / 3,
                    extend: false,
                }];
            }
            HitTarget::FileHistoryAffectedFileRow { .. } => {
                return vec![Action::FileHistoryAffectedMove(delta / 3)];
            }
            HitTarget::FileHistoryBlobArea => {
                return vec![Action::FileHistoryBlobScroll(delta)];
            }
            HitTarget::FileHistoryDiffArea => {
                return vec![Action::DiffScroll(DiffScope::FileHistory, delta)];
            }
            HitTarget::ReflogRow { .. } | HitTarget::ReflogListArea => {
                return vec![Action::ReflogMove(delta / 3)];
            }
            HitTarget::ReflogChangeRow { .. } | HitTarget::ReflogChangesArea => {
                return vec![Action::ReflogChangesMove(delta / 3)];
            }
            HitTarget::ReflogDiffArea => {
                return vec![Action::ReflogDiffScroll(delta)];
            }
            HitTarget::AnnotateLine { .. } | HitTarget::AnnotateArea => {
                return vec![Action::AnnotateScroll(delta)];
            }
            HitTarget::AnnotateDiffArea => {
                return vec![Action::DiffScroll(DiffScope::Annotate, delta)];
            }
            HitTarget::PushCommitRow(_) | HitTarget::PushCommitListArea => {
                return vec![Action::PushMoveCommit(delta / 3)];
            }
            HitTarget::PushFileRow(_) | HitTarget::PushFileListArea => {
                return vec![Action::PushMoveFile(delta / 3)];
            }
            HitTarget::PushDiffArea => {
                return vec![Action::DiffScroll(DiffScope::Push, delta)];
            }
            HitTarget::ConflictQueueRow(_) => {
                return vec![Action::ConflictQueueMove(delta / 3)];
            }
            HitTarget::ConflictMergeBlock(_) => {
                return vec![Action::ConflictScroll(delta)];
            }
            HitTarget::BranchCleanupRow(_) => {
                return vec![Action::BranchCleanupMove(delta / 3)];
            }
            HitTarget::BranchPopupRow(_) | HitTarget::BranchPopupStar(_) => {
                let key = KeyEvent::new(
                    if delta > 0 {
                        KeyCode::Down
                    } else {
                        KeyCode::Up
                    },
                    KeyModifiers::NONE,
                );
                return crate::branches_popup::route_branches_popup_key(&key, state);
            }
            HitTarget::FilterEditorRow(_) => {
                if let Some(top @ crate::app::ModalLayer::FilterEditor { .. }) =
                    state.modal_stack.last()
                    && let Some(kind) = top.popup_kind()
                {
                    let (query_ref, selected) = top.popup_query_and_selected().unwrap_or(("", 0));
                    let max_idx = super::keyboard::popup_candidate_count(kind, query_ref, state)
                        .saturating_sub(1);
                    let step = delta / 3;
                    let next =
                        (selected.min(max_idx) as isize + step).clamp(0, max_idx as isize) as usize;
                    return vec![Action::PopupSelectCandidate(next)];
                }
            }
            HitTarget::RepoSwitcherRow(_) => {
                let key = KeyEvent::new(
                    if delta > 0 {
                        KeyCode::Down
                    } else {
                        KeyCode::Up
                    },
                    KeyModifiers::NONE,
                );
                return crate::repo_switcher::route_repo_switcher_key(&key, state);
            }
            HitTarget::WorktreesRow(_) => {
                return vec![Action::WorktreesMove(delta / 3)];
            }
            HitTarget::RemotesRow(_) => {
                return vec![Action::RemotesMove(delta / 3)];
            }
            HitTarget::DialogBody => match state.modal_stack.last() {
                Some(crate::app::ModalLayer::CommitMessage { .. }) => {
                    return vec![Action::ScrollCommitMessageModal(delta)];
                }
                Some(crate::app::ModalLayer::ConflictResolver(_)) => {
                    return vec![Action::ConflictScroll(delta)];
                }
                Some(crate::app::ModalLayer::ConflictQueue(_)) => {
                    return vec![Action::ConflictQueueMove(delta / 3)];
                }
                Some(crate::app::ModalLayer::BranchCleanup(_)) => {
                    return vec![Action::BranchCleanupMove(delta / 3)];
                }
                Some(crate::app::ModalLayer::ShortcutsHelp { query, selected }) => {
                    let rows = crate::keymap::build_help_rows(state, query);
                    let next = crate::keymap::step_help_selection(&rows, *selected, delta);
                    return vec![Action::PopupSetState {
                        query: query.clone(),
                        selected: next,
                    }];
                }
                Some(crate::app::ModalLayer::BranchesPopup { .. }) => {
                    let key = KeyEvent::new(
                        if delta > 0 {
                            KeyCode::Down
                        } else {
                            KeyCode::Up
                        },
                        KeyModifiers::NONE,
                    );
                    return crate::branches_popup::route_branches_popup_key(&key, state);
                }
                Some(crate::app::ModalLayer::RepoSwitcher(_)) => {
                    let key = KeyEvent::new(
                        if delta > 0 {
                            KeyCode::Down
                        } else {
                            KeyCode::Up
                        },
                        KeyModifiers::NONE,
                    );
                    return crate::repo_switcher::route_repo_switcher_key(&key, state);
                }
                Some(crate::app::ModalLayer::Worktrees(_)) => {
                    return vec![Action::WorktreesMove(delta / 3)];
                }
                Some(crate::app::ModalLayer::Remotes(_)) => {
                    return vec![Action::RemotesMove(delta / 3)];
                }
                Some(
                    top @ (crate::app::ModalLayer::FilterEditor { .. }
                    | crate::app::ModalLayer::CommitHistory { .. }
                    | crate::app::ModalLayer::ChooseParent { .. }
                    | crate::app::ModalLayer::ChooseChild { .. }),
                ) => {
                    if let Some(kind) = top.popup_kind() {
                        let (query_ref, selected) =
                            top.popup_query_and_selected().unwrap_or(("", 0));
                        let max_idx =
                            super::keyboard::popup_candidate_count(kind, query_ref, state)
                                .saturating_sub(1);
                        let step = delta / 3;
                        let next = (selected.min(max_idx) as isize + step)
                            .clamp(0, max_idx as isize) as usize;
                        return vec![Action::PopupSelectCandidate(next)];
                    }
                }
                Some(crate::app::ModalLayer::Dialog(d)) if matches!(d.as_ref(), crate::dialogs::Dialog::RebaseTodo(r) if r.show_diff) =>
                {
                    return vec![Action::ScrollRebaseDiff(delta)];
                }
                Some(crate::app::ModalLayer::Dialog(d)) if matches!(d.as_ref(), crate::dialogs::Dialog::RebaseTodo(r) if !r.show_diff) =>
                {
                    let key = KeyEvent::new(
                        if delta > 0 {
                            KeyCode::Down
                        } else {
                            KeyCode::Up
                        },
                        KeyModifiers::NONE,
                    );
                    return vec![Action::DialogKey(key)];
                }
                _ => {}
            },
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
        HitTarget::HeaderBehindBadge => vec![Action::PromptUpdateProject],
        HitTarget::LogShallowUnshallow => vec![Action::PromptUnshallow],
        HitTarget::LogLoadMore => vec![
            Action::FocusPane(PaneId::LogCommitTable),
            Action::LoadMoreCommits,
        ],
        HitTarget::HeaderGlyphButton => vec![Action::ToggleGlyphMode],
        HitTarget::HeaderRepoName => vec![Action::OpenRepoSwitcher],
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
        HitTarget::FilterNoMergesToggle => vec![Action::ToggleNoMergesFilter],
        HitTarget::FilterChip(field) => {
            let mut initial = state.log.filter.field_expr(field);
            if !initial.is_empty() {
                match field {
                    crate::log_view::FilterField::User => initial.push_str(", "),
                    _ => initial.push(' '),
                }
            }
            vec![Action::OpenPopup {
                kind: PopupKind::FilterEditor(field),
                initial_query: initial,
            }]
        }
        HitTarget::FilterClear(field) => vec![Action::ClearFilterField(field)],
        HitTarget::FilterClearAll => vec![Action::ClearAllFilters],
        HitTarget::FilterRangeClear => vec![Action::ClearRangeFilter],
        HitTarget::FilterRestoreDirHistory => vec![Action::RestorePreDirHistoryFilter],
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
            ChangesToolbarButton::ExpandAll => {
                vec![Action::ChangesExpandAll(ChangesHost::Log, true)]
            }
            ChangesToolbarButton::CollapseAll => {
                vec![Action::ChangesExpandAll(ChangesHost::Log, false)]
            }
            ChangesToolbarButton::DiffPreview => vec![Action::ToggleDiffPreview],
            ChangesToolbarButton::SwitchParent => vec![Action::SwitchMergeParent],
            ChangesToolbarButton::OnlyAffected => vec![Action::TogglePresentation(
                crate::log_view::PresentationToggle::ShowOnlyAffectedChanges,
            )],
            ChangesToolbarButton::SwapCompare => vec![Action::SwapCompareDirection],
        },
        HitTarget::ChangesFilteredBadge => vec![Action::TogglePresentation(
            crate::log_view::PresentationToggle::ShowOnlyAffectedChanges,
        )],
        HitTarget::ChangesLoadMany => vec![
            Action::FocusPane(PaneId::LogChangesTree),
            Action::ConfirmLoadManyChanges,
        ],
        HitTarget::DetailsHash { oid } => vec![Action::CopyText(oid.to_string())],
        HitTarget::DetailsParent { oid } => vec![Action::JumpToCommit(oid)],
        HitTarget::DetailsCommitHash { oid } => {
            if matches!(
                state.modal_stack.last(),
                Some(crate::app::ModalLayer::CommitMessage { .. })
            ) {
                vec![Action::PopModal, Action::JumpToCommit(oid)]
            } else {
                vec![Action::JumpToCommit(oid)]
            }
        }
        HitTarget::DetailsMaximizeMessage => vec![Action::OpenCommitMessageModal],
        HitTarget::DetailsArea | HitTarget::DiffArea => {
            vec![Action::FocusPane(PaneId::LogDetailsDiff)]
        }
        HitTarget::DiffToolbar(btn) => match btn {
            DiffToolbarButton::SideBySide => vec![Action::DiffToggleSideBySide(DiffScope::Log)],
            DiffToolbarButton::Context => vec![Action::DiffCycleContext(DiffScope::Log)],
            DiffToolbarButton::SwapCompare => vec![Action::SwapCompareDirection],
            DiffToolbarButton::Whitespace => vec![Action::DiffCycleWhitespace(DiffScope::Log)],
            DiffToolbarButton::SoftWrap => vec![Action::DiffToggleSoftWrap(DiffScope::Log)],
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
            if ctrl && shift {
                vec![
                    Action::FocusPane(PaneId::LogCommitTable),
                    Action::AddLogSelectionRangeTo(visible_row),
                ]
            } else if ctrl {
                vec![
                    Action::FocusPane(PaneId::LogCommitTable),
                    Action::ToggleLogRow(visible_row),
                ]
            } else if shift {
                vec![
                    Action::FocusPane(PaneId::LogCommitTable),
                    Action::ExtendLogSelectionTo(visible_row),
                ]
            } else {
                let mut actions = vec![
                    Action::FocusPane(PaneId::LogCommitTable),
                    Action::SelectLogRow(visible_row),
                ];
                if is_double {
                    actions.push(Action::OpenBranchesPopup { query: ref_name });
                }
                actions
            }
        }
        HitTarget::CommitRefOverflow { visible_row, .. } => {
            if ctrl && shift {
                vec![
                    Action::FocusPane(PaneId::LogCommitTable),
                    Action::AddLogSelectionRangeTo(visible_row),
                ]
            } else if ctrl {
                vec![
                    Action::FocusPane(PaneId::LogCommitTable),
                    Action::ToggleLogRow(visible_row),
                ]
            } else if shift {
                vec![
                    Action::FocusPane(PaneId::LogCommitTable),
                    Action::ExtendLogSelectionTo(visible_row),
                ]
            } else {
                vec![
                    Action::FocusPane(PaneId::LogCommitTable),
                    Action::SelectLogRow(visible_row),
                ]
            }
        }
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
                let expand_edge = match kind {
                    GraphHitKind::Edge { up_row, down_row } if !shift && !ctrl => {
                        Some(Action::GraphExpandEdge { up_row, down_row })
                    }
                    GraphHitKind::Node {
                        collapsed_below: true,
                        ..
                    } if !shift && !ctrl => Some(Action::GraphExpandBelow(visible_row)),
                    _ => None,
                };
                if ctrl && shift {
                    vec![
                        Action::FocusPane(PaneId::LogCommitTable),
                        Action::AddLogSelectionRangeTo(visible_row),
                    ]
                } else if ctrl {
                    vec![
                        Action::FocusPane(PaneId::LogCommitTable),
                        Action::ToggleLogRow(visible_row),
                    ]
                } else if shift {
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
                    // After selecting: the selection survives the re-projection.
                    actions.extend(expand_edge);
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
        }
        | HitTarget::CommitSignatureCell {
            visible_row,
            commit_oid,
        } => {
            if ctrl && shift {
                vec![
                    Action::FocusPane(PaneId::LogCommitTable),
                    Action::AddLogSelectionRangeTo(visible_row),
                ]
            } else if ctrl {
                vec![
                    Action::FocusPane(PaneId::LogCommitTable),
                    Action::ToggleLogRow(visible_row),
                ]
            } else if shift {
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
        HitTarget::FileHistoryLoadMore => vec![Action::FileHistoryLoadMore],
        HitTarget::FileHistoryAffectedFileRow { idx } => {
            vec![Action::FileHistoryAffectedSelect(idx)]
        }
        HitTarget::FileHistoryBlobArea => Vec::new(),
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
            FileHistoryToolbarButton::BranchScope => vec![Action::FileHistoryCycleBranchScope],
            FileHistoryToolbarButton::FollowRenames => vec![Action::FileHistoryToggleFollow],
            FileHistoryToolbarButton::CompareLocal => {
                vec![Action::FileHistoryToggleCompareLocal]
            }
            FileHistoryToolbarButton::CommitDetails => {
                vec![Action::FileHistoryToggleCommitDetails]
            }
            FileHistoryToolbarButton::AffectedFiles => {
                vec![Action::FileHistoryToggleAffectedFiles]
            }
            FileHistoryToolbarButton::SideBySide => {
                vec![Action::DiffToggleSideBySide(DiffScope::FileHistory)]
            }
            FileHistoryToolbarButton::Context => {
                vec![Action::DiffCycleContext(DiffScope::FileHistory)]
            }
            FileHistoryToolbarButton::Whitespace => {
                vec![Action::DiffCycleWhitespace(DiffScope::FileHistory)]
            }
            FileHistoryToolbarButton::SoftWrap => {
                vec![Action::DiffToggleSoftWrap(DiffScope::FileHistory)]
            }
            FileHistoryToolbarButton::Zoom => vec![Action::FileHistoryToggleZoom],
            FileHistoryToolbarButton::LocateInLog => vec![Action::FileHistoryJumpToLog],
            FileHistoryToolbarButton::Annotate => vec![Action::FileHistoryAnnotate],
        },
        HitTarget::ReflogRow { idx } => {
            let mut actions = vec![
                Action::ReflogFocus(crate::log_view::ReflogFocus::Entries),
                Action::ReflogSelect(idx),
            ];
            if is_double {
                actions.push(Action::ReflogJumpInLog);
            }
            actions
        }
        HitTarget::ReflogChangeRow { idx } => vec![
            Action::ReflogFocus(crate::log_view::ReflogFocus::Changes),
            Action::ReflogChangesSelect(idx),
        ],
        HitTarget::ReflogListArea => {
            vec![Action::ReflogFocus(crate::log_view::ReflogFocus::Entries)]
        }
        HitTarget::ReflogChangesArea => {
            vec![Action::ReflogFocus(crate::log_view::ReflogFocus::Changes)]
        }
        HitTarget::ReflogDiffArea => {
            vec![Action::ReflogFocus(crate::log_view::ReflogFocus::Diff)]
        }
        HitTarget::ReflogToolbar(btn) => match btn {
            crate::hit_test::ReflogToolbarButton::Checkout => vec![Action::ReflogCheckoutSelected],
            crate::hit_test::ReflogToolbarButton::NewBranch => {
                vec![Action::ReflogNewBranchSelected]
            }
            crate::hit_test::ReflogToolbarButton::CherryPick => {
                vec![Action::ReflogCherryPickSelected]
            }
            crate::hit_test::ReflogToolbarButton::ResetHere => vec![Action::ReflogResetSelected],
            crate::hit_test::ReflogToolbarButton::JumpInLog => vec![Action::ReflogJumpInLog],
            crate::hit_test::ReflogToolbarButton::LoadMore => vec![Action::ReflogLoadMore],
            crate::hit_test::ReflogToolbarButton::SideBySide => {
                vec![Action::ReflogToggleSideBySide]
            }
            crate::hit_test::ReflogToolbarButton::Context => vec![Action::ReflogCycleContext],
        },
        HitTarget::AnnotateLine { line_idx } => {
            let mut actions = vec![
                Action::AnnotateSetFocus(crate::log_view::AnnotateFocus::Code),
                Action::AnnotateSelectLine {
                    line_idx,
                    extend: shift,
                },
            ];
            if is_double && !shift {
                actions.push(Action::AnnotateJumpToLog);
            }
            actions
        }
        HitTarget::AnnotateArea => {
            vec![Action::AnnotateSetFocus(
                crate::log_view::AnnotateFocus::Code,
            )]
        }
        HitTarget::AnnotateDiffArea => {
            vec![Action::AnnotateSetFocus(
                crate::log_view::AnnotateFocus::Diff,
            )]
        }
        HitTarget::AnnotateToolbar(btn) => match btn {
            AnnotateToolbarButton::IgnoreWhitespace => vec![Action::AnnotateToggleWhitespace],
            AnnotateToolbarButton::DetectMoves => vec![Action::AnnotateToggleMoves],
            AnnotateToolbarButton::DateMode => vec![Action::AnnotateToggleDateMode],
            AnnotateToolbarButton::ColorMode => vec![Action::AnnotateCycleColorMode],
            AnnotateToolbarButton::PreviousRev => vec![Action::AnnotatePreviousRev],
            AnnotateToolbarButton::NavBack => vec![Action::AnnotateNavBack],
            AnnotateToolbarButton::ShowDiff => vec![Action::AnnotateToggleDiff],
            AnnotateToolbarButton::CommitDetails => vec![Action::AnnotateToggleCommitDetails],
            AnnotateToolbarButton::SelectionHistory => vec![Action::AnnotateShowSelectionHistory],
            AnnotateToolbarButton::LocateInLog => vec![Action::AnnotateJumpToLog],
        },
        HitTarget::AnnotateDetailStrip => vec![Action::AnnotateToggleCommitDetails],
        HitTarget::PushTargetBranch => vec![Action::PushStartEditTarget],
        HitTarget::PushRemotePicker => vec![Action::PushToggleRemotePicker],
        HitTarget::PushDefineRemote => vec![Action::PushDefineRemote],
        HitTarget::PushRemoteOption(idx) => vec![Action::PushSelectRemote(idx)],
        HitTarget::PushCompletionItem(idx) => {
            vec![Action::PushEditTargetAcceptCompletion(Some(idx))]
        }
        HitTarget::PushCommitRow(idx) => vec![
            Action::PushSetFocus(crate::log_view::PushDialogFocus::Commits),
            Action::PushSelectCommit(idx),
        ],
        HitTarget::PushCommitListArea => vec![Action::PushSetFocus(
            crate::log_view::PushDialogFocus::Commits,
        )],
        HitTarget::PushFileRow(idx) => {
            let mut actions = vec![
                Action::PushSetFocus(crate::log_view::PushDialogFocus::Files),
                Action::PushSelectFile(idx),
            ];
            if is_double {
                actions.push(Action::PushToggleDiff);
            }
            actions
        }
        HitTarget::PushFileListArea | HitTarget::PushDiffArea => {
            vec![Action::PushSetFocus(
                crate::log_view::PushDialogFocus::Files,
            )]
        }
        HitTarget::PushDiffBackButton => vec![Action::PushToggleDiff],
        HitTarget::PushOption(opt) => match opt {
            PushOptionKind::Tags => vec![Action::PushCycleTags],
            PushOptionKind::Force => vec![Action::PushCycleForce],
            PushOptionKind::SetUpstream => vec![Action::PushToggleSetUpstream],
            PushOptionKind::NoVerify => vec![Action::PushToggleNoVerify],
        },
        HitTarget::PushActionButton(btn) => match btn {
            PushActionButtonKind::Confirm => vec![Action::PushConfirm],
            PushActionButtonKind::Cancel => vec![Action::PushEscape],
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
            | HitTarget::DialogSecondaryCheckbox
            | HitTarget::ModalButton(_)
            | HitTarget::BranchPopupStar(_)
            | HitTarget::FilterEditorRow(_)
            | HitTarget::RemotesRow(_)
            | HitTarget::RepoSwitcherRow(_)
            | HitTarget::WelcomeItem(_)
            | HitTarget::CloneField(_)
            | HitTarget::CloneButton { .. }
            | HitTarget::WorktreesRow(_)
            | HitTarget::ApplyPatchMode(_)
            | HitTarget::ApplyPatchToggleReverse
            | HitTarget::ApplyPatchStrip(_)
            | HitTarget::ApplyPatchEditDir
            | HitTarget::ApplyPatchFileRow(_)
            | HitTarget::ApplyPatchFileCheck(_)
            | HitTarget::ApplyPatchPreview
            | HitTarget::ApplyPatchButton { .. }
            | HitTarget::TreeBrowserRow(_)
            | HitTarget::TreeBrowserPreview
            | HitTarget::TreeBrowserSearchRow(_)
            | HitTarget::HeaderRepoName
            | HitTarget::BranchPopupFetch
            | HitTarget::BranchPopupSettings
            | HitTarget::FileHistoryLoadMore
            | HitTarget::FileHistoryAffectedFileRow { .. }
            | HitTarget::FileHistoryBlobArea
            | HitTarget::FileHistoryToolbar(_)
            | HitTarget::FileHistoryListArea
            | HitTarget::FileHistoryDiffArea
            | HitTarget::ReflogRow { .. }
            | HitTarget::ReflogChangeRow { .. }
            | HitTarget::ReflogToolbar(_)
            | HitTarget::ReflogListArea
            | HitTarget::ReflogChangesArea
            | HitTarget::ReflogDiffArea
            | HitTarget::AnnotateToolbar(_)
            | HitTarget::AnnotateDetailStrip
            | HitTarget::FilterRestoreDirHistory
            | HitTarget::PushTargetBranch
            | HitTarget::PushRemotePicker
            | HitTarget::PushDefineRemote
            | HitTarget::PushRemoteOption(_)
            | HitTarget::PushCompletionItem(_)
            | HitTarget::PushCommitRow(_)
            | HitTarget::PushCommitListArea
            | HitTarget::PushFileRow(_)
            | HitTarget::PushFileListArea
            | HitTarget::PushDiffArea
            | HitTarget::PushDiffBackButton
            | HitTarget::PushOption(_)
            | HitTarget::PushActionButton(_)
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
