use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use jgit_core::{ModalButton, PaneId, WorkspaceTab};

use crate::app::{
    Action, AppState, ClickRecord, ContextMenuAction, ContextMenuEntryKind, ContextMenuItem,
    DragState, ModalLayer, PopupKind,
};
use crate::hit_test::{GraphHitKind, HitTarget};
use crate::views::log::ordered_branch_refs;
use crate::views::overlays::context_menu_rect;

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
        Event::FocusGained | Event::FocusLost | Event::Paste(_) => Vec::new(),
    }
}

fn route_key(key: KeyEvent, state: &mut AppState) -> Vec<Action> {
    if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
        return vec![Action::Quit];
    }
    if key.code == KeyCode::F(12) {
        return vec![Action::ToggleGlyphMode];
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
            ModalLayer::FloatingPopup {
                kind,
                query,
                selected,
            } => {
                return match key.code {
                    KeyCode::Esc => vec![Action::PopModal],
                    KeyCode::Char('?') if *kind == PopupKind::ShortcutsHelp => {
                        vec![Action::PopModal]
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

    // Layer 1: Workspace global shortcuts
    match key.code {
        KeyCode::Char('q') => return vec![Action::Quit],
        KeyCode::Char('?') => {
            return vec![Action::OpenPopup {
                kind: PopupKind::ShortcutsHelp,
                initial_query: String::new(),
            }];
        }
        KeyCode::Char('g') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            return vec![Action::ToggleGlyphMode];
        }
        KeyCode::F(9) => {
            return vec![Action::OpenPopup {
                kind: PopupKind::PalettePreview,
                initial_query: String::new(),
            }];
        }
        KeyCode::Char(d @ ('1' | '2' | '3' | '4')) => {
            if let Some(tab) = WorkspaceTab::from_digit(d) {
                return vec![Action::SwitchTab(tab)];
            }
        }
        KeyCode::Tab => return vec![Action::CycleFocus { reverse: false }],
        KeyCode::BackTab => return vec![Action::CycleFocus { reverse: true }],
        KeyCode::Char('r') => return vec![Action::RefreshAll],
        KeyCode::Char('p') => {
            return vec![Action::RunCliProbe(vec![
                "status".into(),
                "--short".into(),
                "--branch".into(),
            ])];
        }
        _ => {}
    }

    // Focused pane navigation
    match state.focused_pane {
        PaneId::LogBranchesTree => match key.code {
            KeyCode::Down | KeyCode::Char('j') => vec![Action::ScrollBranches(1)],
            KeyCode::Up | KeyCode::Char('k') => vec![Action::ScrollBranches(-1)],
            _ => Vec::new(),
        },
        PaneId::LogCommitTable => match key.code {
            KeyCode::Down | KeyCode::Char('j') => vec![Action::ScrollLog(1)],
            KeyCode::Up | KeyCode::Char('k') => vec![Action::ScrollLog(-1)],
            KeyCode::PageDown => vec![Action::ScrollLog(10)],
            KeyCode::PageUp => vec![Action::ScrollLog(-10)],
            KeyCode::Home => vec![Action::SelectLogRow(0)],
            KeyCode::Left if key.modifiers.contains(KeyModifiers::ALT) => {
                vec![Action::NavLogBack]
            }
            KeyCode::Char('c') => vec![Action::ToggleCompactRefs],
            KeyCode::Char('t') => vec![Action::ToggleTagNames],
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
        PaneId::CommitChangesTree => match key.code {
            KeyCode::Down | KeyCode::Char('j') => vec![Action::ScrollCommitStage(1)],
            KeyCode::Up | KeyCode::Char('k') => vec![Action::ScrollCommitStage(-1)],
            _ => Vec::new(),
        },
        _ => Vec::new(),
    }
}

fn route_context_menu_key(key: KeyEvent, state: &mut AppState) -> Vec<Action> {
    match key.code {
        KeyCode::Esc | KeyCode::Left => vec![Action::PopModal],
        KeyCode::Up | KeyCode::Char('k') => {
            if let Some(ModalLayer::ContextMenu {
                items, selected, ..
            }) = state.modal_stack.last_mut()
                && !items.is_empty()
            {
                *selected = if *selected == 0 {
                    items.len() - 1
                } else {
                    *selected - 1
                };
                state.needs_redraw = true;
            }
            Vec::new()
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if let Some(ModalLayer::ContextMenu {
                items, selected, ..
            }) = state.modal_stack.last_mut()
                && !items.is_empty()
            {
                *selected = (*selected + 1) % items.len();
                state.needs_redraw = true;
            }
            Vec::new()
        }
        KeyCode::Right => {
            let Some(ModalLayer::ContextMenu {
                anchor,
                items,
                selected,
                depth,
            }) = state.modal_stack.last()
            else {
                return Vec::new();
            };
            if let Some(item) = items.get(*selected)
                && matches!(item.kind, ContextMenuEntryKind::Submenu(_))
            {
                return activate_menu_item(*anchor, items, *selected, *depth, state.terminal_area);
            }
            Vec::new()
        }
        KeyCode::Enter => {
            let Some(ModalLayer::ContextMenu {
                anchor,
                items,
                selected,
                depth,
            }) = state.modal_stack.last()
            else {
                return Vec::new();
            };
            activate_menu_item(*anchor, items, *selected, *depth, state.terminal_area)
        }
        KeyCode::Char(ch)
            if !key.modifiers.contains(KeyModifiers::CONTROL)
                && !key.modifiers.contains(KeyModifiers::ALT) =>
        {
            let target = ch.to_ascii_lowercase();
            let Some(ModalLayer::ContextMenu {
                anchor,
                items,
                depth,
                ..
            }) = state.modal_stack.last()
            else {
                return Vec::new();
            };
            if let Some(idx) = items
                .iter()
                .position(|i| i.mnemonic.is_some_and(|m| m.to_ascii_lowercase() == target))
            {
                return activate_menu_item(*anchor, items, idx, *depth, state.terminal_area);
            }
            Vec::new()
        }
        _ => Vec::new(),
    }
}

fn activate_menu_item(
    anchor: (u16, u16),
    items: &[ContextMenuItem],
    idx: usize,
    depth: u8,
    screen: ratatui::layout::Rect,
) -> Vec<Action> {
    let Some(item) = items.get(idx) else {
        return Vec::new();
    };
    match &item.kind {
        ContextMenuEntryKind::Submenu(children) => {
            let parent_rect = context_menu_rect(screen, anchor, items);
            let sub_anchor = (
                parent_rect.x.saturating_add(parent_rect.width),
                parent_rect.y.saturating_add(1).saturating_add(idx as u16),
            );
            vec![Action::OpenContextMenu {
                anchor: sub_anchor,
                items: children.clone(),
                depth: depth.saturating_add(1),
            }]
        }
        ContextMenuEntryKind::Action(act) => {
            let mut out = vec![Action::PopContextMenus];
            out.extend(resolve_menu_action(act.clone()));
            out
        }
    }
}

fn route_mouse(mouse: MouseEvent, state: &mut AppState) -> Vec<Action> {
    let col = mouse.column;
    let row = mouse.row;

    match mouse.kind {
        MouseEventKind::Drag(MouseButton::Left) => {
            if let Some(drag) = state.drag_state {
                state.splitters.update_from_pointer(
                    drag.splitter_id,
                    drag.axis,
                    drag.container,
                    col,
                    row,
                );
                state.needs_redraw = true;
            }
            Vec::new()
        }
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
                && let Some(ModalLayer::ContextMenu {
                    items,
                    selected,
                    depth,
                    ..
                }) = state.modal_stack.last_mut()
                && *depth == *menu_depth
                && *item_idx < items.len()
                && *selected != *item_idx
            {
                *selected = *item_idx;
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
                    HitTarget::BranchTreeNode { .. } | HitTarget::Pane(PaneId::LogBranchesTree) => {
                        return vec![Action::ScrollBranches(delta / 3)];
                    }
                    HitTarget::CommitTableRow { .. }
                    | HitTarget::GraphPrintElement { .. }
                    | HitTarget::CommitRefPill { .. }
                    | HitTarget::CommitRefOverflow { .. }
                    | HitTarget::Pane(PaneId::LogCommitTable) => {
                        return vec![Action::ScrollLog(delta / 3)];
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

            let Some(region) = state.hit_registry.hit_test(col, row).cloned() else {
                return Vec::new();
            };

            match region.target {
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
                    if let Some(ModalLayer::ContextMenu {
                        anchor,
                        items,
                        depth,
                        ..
                    }) = state.modal_stack.last()
                    {
                        return activate_menu_item(
                            *anchor,
                            items,
                            item_idx,
                            *depth,
                            state.terminal_area,
                        );
                    }
                    Vec::new()
                }
                HitTarget::Splitter { id, axis } => {
                    if is_double {
                        vec![Action::ResetSplitter(id)]
                    } else {
                        if let Some((_, container)) = state.splitter_containers.get(&id).copied() {
                            state.drag_state = Some(DragState {
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
                    vec![Action::OpenPopup {
                        kind: PopupKind::BranchesQuickView,
                        initial_query: String::new(),
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
                HitTarget::BranchTreeNode { node_id, .. } => {
                    let idx = node_id.0 as usize;
                    let mut actions = vec![
                        Action::FocusPane(PaneId::LogBranchesTree),
                        Action::SelectBranchRow(idx),
                    ];
                    if is_double
                        && let Some(snap) = &state.repo_snapshot
                        && let Some(r) = ordered_branch_refs(snap).get(idx)
                    {
                        actions.push(Action::OpenPopup {
                            kind: PopupKind::BranchesQuickView,
                            initial_query: r.name.clone(),
                        });
                    }
                    actions
                }
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
                        actions.push(Action::OpenPopup {
                            kind: PopupKind::BranchesQuickView,
                            initial_query: ref_name,
                        });
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
                HitTarget::ChangesTreeNode { node_id, .. } => {
                    let idx = node_id.0 as usize;
                    let mut actions = vec![
                        Action::FocusPane(PaneId::CommitChangesTree),
                        Action::SelectCommitStageRow(idx),
                    ];
                    if is_double && let Some(entry) = state.stage_entry_at(idx) {
                        actions.push(Action::RunCliProbe(vec![
                            "diff".into(),
                            "--".into(),
                            entry.path.clone(),
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
            }
        }
        MouseEventKind::Down(MouseButton::Right) => {
            let Some(region) = state.hit_registry.hit_test(col, row).cloned() else {
                return Vec::new();
            };

            if matches!(region.target, HitTarget::ModalBackdrop { .. }) {
                return vec![Action::PopModal];
            }
            if matches!(region.target, HitTarget::MenuItem { .. }) {
                return Vec::new();
            }

            let mut actions = Vec::new();
            if matches!(
                state.modal_stack.last(),
                Some(ModalLayer::ContextMenu { .. })
            ) {
                actions.push(Action::PopContextMenus);
            }

            let menu_items = match &region.target {
                HitTarget::CommitTableRow {
                    visible_row,
                    commit_oid,
                }
                | HitTarget::GraphPrintElement {
                    visible_row,
                    commit_oid,
                    ..
                } => {
                    if state.log_graph.selected_idx != *visible_row {
                        actions.push(Action::FocusPane(PaneId::LogCommitTable));
                        actions.push(Action::SelectLogRow(*visible_row));
                    }
                    let short = commit_oid.short();
                    vec![
                        ContextMenuItem::action(
                            format!("Inspect commit {short} via git show"),
                            Some('i'),
                            None,
                            ContextMenuAction::RunReadOnlyProbe(vec![
                                "show".into(),
                                "--stat".into(),
                                commit_oid.to_string(),
                            ]),
                        ),
                        ContextMenuItem::action(
                            "Refresh Log & Refs",
                            Some('r'),
                            Some("r"),
                            ContextMenuAction::RefreshRepo,
                        ),
                        ContextMenuItem::submenu("Git Probes", Some('p'), probe_submenu_items()),
                        ContextMenuItem::action(
                            "Open Console Tab",
                            Some('c'),
                            Some("4"),
                            ContextMenuAction::SwitchTab(WorkspaceTab::Console),
                        ),
                        ref_view_options_submenu(state),
                    ]
                }
                HitTarget::CommitRefPill {
                    visible_row,
                    ref_name,
                    ..
                } => {
                    if state.log_graph.selected_idx != *visible_row {
                        actions.push(Action::FocusPane(PaneId::LogCommitTable));
                        actions.push(Action::SelectLogRow(*visible_row));
                    }
                    let mut items = ref_pill_menu_items(ref_name);
                    items.push(ref_view_options_submenu(state));
                    items
                }
                HitTarget::CommitRefOverflow {
                    visible_row,
                    hidden_refs,
                } => {
                    if state.log_graph.selected_idx != *visible_row {
                        actions.push(Action::FocusPane(PaneId::LogCommitTable));
                        actions.push(Action::SelectLogRow(*visible_row));
                    }
                    overflow_menu_items(hidden_refs)
                }
                HitTarget::BranchTreeNode { node_id, .. } => {
                    let idx = node_id.0 as usize;
                    if state.branches_selected_idx != idx {
                        actions.push(Action::FocusPane(PaneId::LogBranchesTree));
                        actions.push(Action::SelectBranchRow(idx));
                    }
                    let branch_name = state
                        .repo_snapshot
                        .as_ref()
                        .and_then(|s| ordered_branch_refs(s).get(idx).map(|r| r.name.clone()))
                        .unwrap_or_else(|| "HEAD".to_string());
                    vec![
                        ContextMenuItem::action(
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
                        ),
                        ContextMenuItem::submenu("Git Probes", Some('p'), probe_submenu_items()),
                        ContextMenuItem::action(
                            "Refresh Repository",
                            Some('r'),
                            Some("r"),
                            ContextMenuAction::RefreshRepo,
                        ),
                    ]
                }
                HitTarget::ChangesTreeNode { node_id, .. } => {
                    let idx = node_id.0 as usize;
                    if state.commit_stage_selected_idx != idx {
                        actions.push(Action::FocusPane(PaneId::CommitChangesTree));
                        actions.push(Action::SelectCommitStageRow(idx));
                    }
                    let file_path = state
                        .stage_entry_at(idx)
                        .map(|e| e.path.clone())
                        .unwrap_or_default();
                    let mut items = Vec::new();
                    if !file_path.is_empty() {
                        items.push(ContextMenuItem::action(
                            format!("Diff {file_path}"),
                            Some('d'),
                            None,
                            ContextMenuAction::RunReadOnlyProbe(vec![
                                "diff".into(),
                                "--".into(),
                                file_path,
                            ]),
                        ));
                    }
                    items.push(ContextMenuItem::action(
                        "Refresh Stage Status",
                        Some('r'),
                        Some("r"),
                        ContextMenuAction::RefreshRepo,
                    ));
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
                _ => vec![
                    ContextMenuItem::action(
                        "Refresh Repository Snapshot",
                        Some('r'),
                        Some("r"),
                        ContextMenuAction::RefreshRepo,
                    ),
                    ContextMenuItem::submenu("Git Probes", Some('p'), probe_submenu_items()),
                    ContextMenuItem::action(
                        "Toggle Unicode / NerdFont",
                        Some('g'),
                        Some("F12"),
                        ContextMenuAction::ToggleGlyphMode,
                    ),
                    ContextMenuItem::action(
                        "Palette Preview",
                        Some('v'),
                        Some("F9"),
                        ContextMenuAction::OpenPalettePreview,
                    ),
                    ContextMenuItem::action(
                        "Keyboard Shortcuts",
                        Some('h'),
                        Some("?"),
                        ContextMenuAction::OpenHelp,
                    ),
                ],
            };

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

fn resolve_menu_action(act: ContextMenuAction) -> Vec<Action> {
    match act {
        ContextMenuAction::SwitchTab(tab) => vec![Action::SwitchTab(tab)],
        ContextMenuAction::RefreshRepo => vec![Action::RefreshAll],
        ContextMenuAction::RunReadOnlyProbe(args) => vec![Action::RunCliProbe(args)],
        ContextMenuAction::ToggleGlyphMode => vec![Action::ToggleGlyphMode],
        ContextMenuAction::ToggleCompactRefs => vec![Action::ToggleCompactRefs],
        ContextMenuAction::ToggleTagNames => vec![Action::ToggleTagNames],
        ContextMenuAction::OpenPalettePreview => vec![Action::OpenPopup {
            kind: PopupKind::PalettePreview,
            initial_query: String::new(),
        }],
        ContextMenuAction::OpenHelp => vec![Action::OpenPopup {
            kind: PopupKind::ShortcutsHelp,
            initial_query: String::new(),
        }],
    }
}

fn ref_pill_menu_items(ref_name: &str) -> Vec<ContextMenuItem> {
    vec![
        ContextMenuItem::action(
            format!("Show log for {ref_name}"),
            Some('l'),
            None,
            ContextMenuAction::RunReadOnlyProbe(vec![
                "log".into(),
                "-n".into(),
                "10".into(),
                "--oneline".into(),
                ref_name.to_string(),
            ]),
        ),
        ContextMenuItem::action(
            "Refresh Log & Refs",
            Some('r'),
            Some("r"),
            ContextMenuAction::RefreshRepo,
        ),
    ]
}

/// Menu behind a `+N` overflow pill: one submenu per hidden ref.
fn overflow_menu_items(hidden_refs: &[String]) -> Vec<ContextMenuItem> {
    hidden_refs
        .iter()
        .map(|name| ContextMenuItem::submenu(name.clone(), None, ref_pill_menu_items(name)))
        .collect()
}

fn ref_view_options_submenu(state: &AppState) -> ContextMenuItem {
    let check = |on: bool| if on { "✓" } else { " " };
    ContextMenuItem::submenu(
        "Reference View Options",
        Some('o'),
        vec![
            ContextMenuItem::action(
                format!("{} Compact References View", check(state.compact_refs)),
                Some('c'),
                Some("c"),
                ContextMenuAction::ToggleCompactRefs,
            ),
            ContextMenuItem::action(
                format!("{} Show Tag Names", check(state.show_tag_names)),
                Some('t'),
                Some("t"),
                ContextMenuAction::ToggleTagNames,
            ),
        ],
    )
}
