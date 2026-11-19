//! Context-menu builders (Spec 05 §3): Log commit rows, branch / remote /
//! tag submenus, Stage tree & diff menus, ongoing-operation actions and
//! `[3 Stash]` actions. Items whose preconditions fail stay visible but
//! disabled, carrying the reason shown in the footer.

mod branch;
mod log;
mod stage;
mod stash;

pub use branch::{branch_menu_items, ongoing_menu_items, ref_menu_items};
pub use log::{log_commit_menu_items, log_diff_menu_items};
pub use stage::{stage_diff_menu_items, stage_hunk_menu_items, stage_tree_menu_items};
pub use stash::{stash_button_actions, stash_menu_items};

use jbgit_core::GitOp;

use crate::app::{Action, AppState, ContextMenuItem};
use crate::dialogs::Dialog;

pub(super) fn run(
    label: impl Into<String>,
    m: Option<char>,
    sc: Option<&str>,
    a: Vec<Action>,
) -> ContextMenuItem {
    ContextMenuItem::run(label, m, sc, a)
}

pub(super) fn op(o: GitOp) -> Vec<Action> {
    vec![Action::RunGitOp(o)]
}

pub(super) fn dialog(d: Dialog) -> Vec<Action> {
    vec![Action::OpenDialog(Box::new(d))]
}

pub(super) fn local_names(state: &AppState) -> Vec<String> {
    state
        .repo_snapshot
        .as_ref()
        .map(|s| {
            s.local_branch_names()
                .iter()
                .map(|n| n.to_string())
                .collect()
        })
        .unwrap_or_default()
}

pub(super) fn ellipsize(s: &str, max: usize) -> String {
    crate::views::truncate_to_width(s, max)
}

pub(super) fn diff_patch_args(staged: bool, paths: &[String]) -> Vec<String> {
    let mut args = vec!["diff".to_string()];
    if staged {
        args.push("--cached".to_string());
    }
    args.push("--".to_string());
    args.extend(paths.iter().cloned());
    args
}
