use jbgit_core::GitOp;

use crate::app::{Action, AppState, ContextMenuItem};
use crate::dialogs::{
    ChoiceDialog, Dialog, InputDialog, InputIntent, InputValidation, stash_label,
};
use crate::hit_test::StashButton;

use super::{dialog, ellipsize, local_names, op, run};

struct StashActionSpec<'a> {
    label: &'a str,
    mnemonic: char,
    shortcut: Option<&'static str>,
    disabled_reason: Option<&'a str>,
    actions: Vec<Action>,
}

fn stash_spec<'a>(state: &'a AppState, btn: StashButton) -> StashActionSpec<'a> {
    let stashes = state
        .repo_snapshot
        .as_ref()
        .map(|s| s.stashes.clone())
        .unwrap_or_default();
    let entry = state.stash.selected_entry(&stashes).cloned();
    let has = entry.is_some();
    let index = entry.as_ref().map_or(0, |e| e.index);
    let label = entry.as_ref().map(stash_label).unwrap_or_default();
    let i18n = &state.i18n;
    let no_stash = (!has).then(|| i18n.t("menu.stash.no_stash_selected"));

    match btn {
        StashButton::Apply => StashActionSpec {
            label: i18n.t("menu.stash.apply"),
            mnemonic: 'a',
            shortcut: Some("Enter"),
            disabled_reason: no_stash,
            actions: op(GitOp::StashApply { index, pop: false }),
        },
        StashButton::Pop => StashActionSpec {
            label: i18n.t("menu.stash.pop"),
            mnemonic: 'p',
            shortcut: Some("p"),
            disabled_reason: no_stash,
            actions: op(GitOp::StashApply { index, pop: true }),
        },
        StashButton::Branch => StashActionSpec {
            label: i18n.t("menu.stash.branch_from_stash"),
            mnemonic: 'b',
            shortcut: Some("b"),
            disabled_reason: no_stash,
            actions: dialog(Dialog::Input(InputDialog::new(
                i18n.t("dialog.stash.unstash_title"),
                i18n.t_fmt("dialog.stash.unstash_prompt", &[("label", &ellipsize(&label, 40))]),
                "",
                InputValidation::RefName {
                    what: "Branch",
                    existing: local_names(state),
                },
                InputIntent::UnstashAsBranch { index },
            ))),
        },
        StashButton::New => StashActionSpec {
            label: i18n.t("menu.stash.new_stash"),
            mnemonic: 'n',
            shortcut: Some("n"),
            disabled_reason: None,
            actions: dialog(Dialog::Input(
                InputDialog::new(
                    i18n.t("dialog.stash.new_title"),
                    i18n.t("dialog.stash.new_prompt"),
                    "",
                    InputValidation::None,
                    InputIntent::StashMessage,
                )
                .with_checkbox(i18n.t("dialog.stash.include_untracked"), true),
            )),
        },
        StashButton::Drop => StashActionSpec {
            label: i18n.t("menu.stash.drop"),
            mnemonic: 'd',
            shortcut: Some("Del"),
            disabled_reason: no_stash,
            actions: dialog(Dialog::Choice(ChoiceDialog::confirm(
                i18n.t("dialog.stash.drop_title"),
                vec![
                    i18n.t_fmt("dialog.stash.drop_prompt", &[("label", &label)]),
                    i18n.t("dialog.stash.drop_warn").into(),
                ],
                i18n.t("dialog.stash.drop_btn"),
                i18n.cancel(),
                op(GitOp::StashDrop { index }),
            ))),
        },
        StashButton::Clear => {
            let count_s = stashes.len().to_string();
            StashActionSpec {
                label: i18n.t("menu.stash.clear_all"),
                mnemonic: 'c',
                shortcut: None,
                disabled_reason: stashes.is_empty().then(|| i18n.t("menu.stash.no_stashes")),
                actions: dialog(Dialog::Choice(ChoiceDialog::confirm(
                    i18n.t("dialog.stash.clear_title"),
                    vec![i18n.t_fmt("dialog.stash.clear_prompt", &[("count", &count_s)])],
                    i18n.t("dialog.stash.clear_btn"),
                    i18n.cancel(),
                    op(GitOp::StashClear),
                ))),
            }
        }
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
