//! Library surface for `jbgit-tui`, exposing configuration, i18n, keymaps,
//! terminal guard, and the deterministic [`session::TuiSession`] turn driver.

pub(crate) mod app;
mod branches_popup;
pub mod config;
mod console_workbench;
mod dialogs;
mod diff_session;
mod external_editor;
pub mod graph;
mod hit_test;
pub mod i18n;
mod input;
mod input_stage;
pub mod keymap;
mod log_view;
mod menus;
pub mod session;
mod stage_view;
mod stash_view;
pub mod syntax;
pub mod terminal;
#[cfg(test)]
mod tests;
mod theme;
mod views;
mod widgets;

pub use app::{Action, AppState, Effect, UiAsyncEvent};
pub use session::{
    MAX_COALESCE_EVENTS, ProdHost, SessionHost, SessionInput, TestHost, TuiSession, TurnReport,
    coalesce_effects, init_engine_and_session, poll_stream_ready, recv_engine,
    spawn_wezterm_cell_probe,
};
