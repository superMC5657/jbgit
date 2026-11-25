pub(crate) mod keyboard;
pub(crate) mod menus;
mod mouse;

use crossterm::event::{Event, KeyEventKind};
use jbgit_core::{PaneId, WorkspaceTab};

use crate::app::{Action, AppState};

pub(crate) use keyboard::route_key;
pub(crate) use menus::{
    activate_top_menu_item, build_menu_items_for_target, keyboard_context_menu,
    preselect_actions_for_menu_target, presentation_menu_items,
};
pub(crate) use mouse::route_mouse;

/// Pure terminal event router: translates raw Crossterm events into explicit
/// [`Action`] values without mutating [`AppState`].
pub fn route_terminal_event(event: &Event, state: &AppState) -> Vec<Action> {
    match event {
        Event::Key(key) => {
            if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
                return Vec::new();
            }
            route_key(*key, state)
        }
        Event::Mouse(mouse) => route_mouse(*mouse, state),
        Event::Resize(_, _) => vec![Action::Resize],
        Event::Paste(text) => route_paste(text, state),
        Event::FocusGained | Event::FocusLost => Vec::new(),
    }
}

fn route_paste(text: &str, state: &AppState) -> Vec<Action> {
    if !state.modal_stack.is_empty() {
        return vec![Action::ModalPaste(text.to_string())];
    }

    if state.active_tab == WorkspaceTab::Commit && state.focused_pane == PaneId::CommitMessageEditor
    {
        if state.stage_workbench.editing_author() {
            return text
                .chars()
                .filter(|c| *c != '\n' && *c != '\r')
                .map(Action::CommitAuthorChar)
                .collect();
        }
        return vec![Action::CommitEditorPaste(text.to_string())];
    }

    if state.active_tab == WorkspaceTab::Log
        && state.log.details.diff.search.editing
        && (state.log.details.diff.zoomed
            || (state.focused_pane == PaneId::LogDetailsDiff && state.log.details.diff_preview))
    {
        return text
            .chars()
            .filter(|c| *c != '\n' && *c != '\r' && !c.is_control())
            .map(|c| Action::DiffSearchChar(crate::app::DiffScope::Log, c))
            .collect();
    }

    if state.active_tab == WorkspaceTab::Commit
        && state.focused_pane == PaneId::CommitDiffPreview
        && state.stage_workbench.diff_search().editing
    {
        return text
            .chars()
            .filter(|c| *c != '\n' && *c != '\r' && !c.is_control())
            .map(|c| Action::DiffSearchChar(crate::app::DiffScope::Stage, c))
            .collect();
    }

    if state.active_tab == WorkspaceTab::Stash
        && state.focused_pane == PaneId::StashDiffPreview
        && state.stash.diff().search.editing
    {
        return text
            .chars()
            .filter(|c| *c != '\n' && *c != '\r' && !c.is_control())
            .map(|c| Action::DiffSearchChar(crate::app::DiffScope::Stash, c))
            .collect();
    }

    if state.active_tab == WorkspaceTab::Log && state.log.speed_search.editing {
        return text
            .chars()
            .filter(|c| *c != '\n' && *c != '\r' && !c.is_control())
            .map(Action::LogSpeedSearchInput)
            .collect();
    }

    if state.active_tab == WorkspaceTab::Log && state.log.filter.editing_text {
        return text
            .chars()
            .filter(|c| *c != '\n' && *c != '\r')
            .map(Action::FilterTextInput)
            .collect();
    }

    if state.active_tab == WorkspaceTab::Log
        && state.focused_pane == PaneId::LogBranchesTree
        && (state.log.branches.searching || state.config.keymap.tree_speed_search)
    {
        return text
            .chars()
            .filter(|c| !c.is_control() && *c != ' ')
            .map(Action::BranchesSearchInput)
            .collect();
    }

    Vec::new()
}
