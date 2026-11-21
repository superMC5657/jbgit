mod keyboard;
mod menus;
mod mouse;

use crossterm::event::{Event, KeyEventKind};
use jbgit_core::{PaneId, WorkspaceTab};

use crate::app::{Action, AppState, ModalLayer, PopupKind};

pub(crate) use keyboard::route_key;
pub(crate) use menus::{keyboard_context_menu, presentation_menu_items, resolve_menu_action};
pub(crate) use mouse::route_mouse;

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
                            let next_sel =
                                crate::branches_popup::first_selectable_row(state, &next_q);
                            if let Some(ModalLayer::FloatingPopup {
                                query, selected, ..
                            }) = state.modal_stack.last_mut()
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
                            if let Some(ModalLayer::FloatingPopup {
                                query, selected, ..
                            }) = state.modal_stack.last_mut()
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
