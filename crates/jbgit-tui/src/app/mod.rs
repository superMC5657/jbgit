mod action;
mod browse;
mod credentials;
mod dispatch;
mod effects;
mod events;
mod git_workflows;
mod modal_stack;
mod repo_session;
mod scroll;
mod state;
mod types;

pub use action::{Action, ChangesHost, DiffScope};
pub use credentials::CredentialQueue;
pub use effects::execute_effects_with_host;
pub use modal_stack::{ModalLayer, ModalStack};
pub use scroll::ScrollViewport;
pub use state::AppState;
pub use types::{
    ClickRecord, ConflictMergeState, ContainingBranchesJob, ContextMenuEntryKind, ContextMenuItem,
    DragState, EditorMoveKind, Effect, MENU_HOVER_DELAY, MenuHover, MenuParent, Notification,
    PopupKind, RenderContext, SCROLL_GOTO_BOTTOM, SCROLL_GOTO_TOP, ScrollPane, SyntaxJob,
    SyntaxJobKey, SyntaxTarget, UiAsyncEvent, apply_scroll_delta,
};
pub(crate) use types::initial_menu_selection;

#[cfg(test)]
mod tests {
    use super::apply_scroll_delta;

    #[test]
    fn apply_scroll_delta_clamps_without_overflow_or_dead_zone() {
        assert_eq!(apply_scroll_delta(0, isize::MAX, 42), 42);
        assert_eq!(apply_scroll_delta(42, isize::MAX / 2, 42), 42);
        let after_twice = apply_scroll_delta(
            apply_scroll_delta(10, isize::MAX / 2, 42),
            isize::MAX / 2,
            42,
        );
        assert_eq!(after_twice, 42);

        assert_eq!(apply_scroll_delta(42, isize::MIN, 42), 0);
        assert_eq!(apply_scroll_delta(42, isize::MIN / 2, 42), 0);

        assert_eq!(apply_scroll_delta(500, -1, 42), 41);
        assert_eq!(apply_scroll_delta(500, 0, 42), 42);
        assert_eq!(apply_scroll_delta(500, 5, 42), 42);
        assert_eq!(apply_scroll_delta(5, -10, 0), 0);
        assert_eq!(apply_scroll_delta(0, 10, 0), 0);
    }
}
