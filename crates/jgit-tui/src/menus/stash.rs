use jgit_core::GitOp;

use crate::app::{Action, AppState, ContextMenuItem};
use crate::dialogs::{
    ChoiceDialog, Dialog, InputDialog, InputIntent, InputValidation, stash_label,
};
use crate::hit_test::StashButton;

use super::{dialog, ellipsize, local_names, op, run};

struct StashActionSpec {
    label: &'static str,
    mnemonic: char,
    shortcut: Option<&'static str>,
    disabled_reason: Option<&'static str>,
    actions: Vec<Action>,
}

fn stash_spec(state: &AppState, btn: StashButton) -> StashActionSpec {
    let stashes = state
        .repo_snapshot
        .as_ref()
        .map(|s| s.stashes.clone())
        .unwrap_or_default();
    let entry = state.stash.selected_entry(&stashes).cloned();
    let has = entry.is_some();
    let index = entry.as_ref().map_or(0, |e| e.index);
    let label = entry.as_ref().map(stash_label).unwrap_or_default();

    match btn {
        StashButton::Apply => StashActionSpec {
            label: "Apply",
            mnemonic: 'a',
            shortcut: Some("Enter"),
            disabled_reason: (!has).then_some("No stash selected"),
            actions: op(GitOp::StashApply { index, pop: false }),
        },
        StashButton::Pop => StashActionSpec {
            label: "Pop",
            mnemonic: 'p',
            shortcut: Some("p"),
            disabled_reason: (!has).then_some("No stash selected"),
            actions: op(GitOp::StashApply { index, pop: true }),
        },
        StashButton::Branch => StashActionSpec {
            label: "Unstash as New Branch…",
            mnemonic: 'b',
            shortcut: Some("b"),
            disabled_reason: (!has).then_some("No stash selected"),
            actions: dialog(Dialog::Input(InputDialog::new(
                "Unstash as Branch",
                format!("Branch to create from {}:", ellipsize(&label, 40)),
                "",
                InputValidation::RefName {
                    what: "Branch",
                    existing: local_names(state),
                },
                InputIntent::UnstashAsBranch { index },
            ))),
        },
        StashButton::New => StashActionSpec {
            label: "New Stash…",
            mnemonic: 'n',
            shortcut: Some("n"),
            disabled_reason: None,
            actions: dialog(Dialog::Input(
                InputDialog::new(
                    "Stash Local Changes",
                    "Message (optional):",
                    "",
                    InputValidation::None,
                    InputIntent::StashMessage,
                )
                .with_checkbox("Include untracked files (-u)", true),
            )),
        },
        StashButton::Drop => StashActionSpec {
            label: "Drop",
            mnemonic: 'd',
            shortcut: Some("Del"),
            disabled_reason: (!has).then_some("No stash selected"),
            actions: dialog(Dialog::Choice(ChoiceDialog::confirm(
                "Drop Stash",
                vec![format!("Drop {label}?"), "Its changes are lost.".into()],
                "Drop",
                op(GitOp::StashDrop { index }),
            ))),
        },
        StashButton::Clear => StashActionSpec {
            label: "Clear All Stashes…",
            mnemonic: 'c',
            shortcut: None,
            disabled_reason: stashes.is_empty().then_some("No stashes"),
            actions: dialog(Dialog::Choice(ChoiceDialog::confirm(
                "Clear Stashes",
                vec![format!(
                    "Remove all {} stash(es)? This cannot be undone.",
                    stashes.len()
                )],
                "Clear All",
                op(GitOp::StashClear),
            ))),
        },
    }
}

/// Typed dispatch for Stash toolbar clicks and keyboard shortcuts.
pub fn stash_button_actions(state: &AppState, btn: StashButton) -> Vec<Action> {
    let spec = stash_spec(state, btn);
    if let Some(reason) = spec.disabled_reason {
        vec![Action::Notice(reason.to_string())]
    } else {
        spec.actions
    }
}

/// Stash list row menu.
pub fn stash_menu_items(state: &AppState) -> Vec<ContextMenuItem> {
    let to_item = |btn: StashButton| {
        let s = stash_spec(state, btn);
        let item = run(s.label, Some(s.mnemonic), s.shortcut, s.actions);
        match s.disabled_reason {
            Some(r) => item.enabled_if(false, r),
            None => item,
        }
    };
    vec![
        to_item(StashButton::Apply),
        to_item(StashButton::Pop),
        to_item(StashButton::Branch),
        ContextMenuItem::separator(),
        to_item(StashButton::New),
        ContextMenuItem::separator(),
        to_item(StashButton::Drop),
        to_item(StashButton::Clear),
    ]
}
