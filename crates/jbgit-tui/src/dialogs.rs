//! Lightweight modal dialog library (`ModalDialog`):
//! validated single-line input (New Branch / Tag / Rename), radio & button
//! choice groups (`GitResetDialog`, Smart Operation, dangerous confirms),
//! multi-line message editor (Reword / Squash / annotated tag) and the
//! interactive-rebase todo table.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use jbgit_core::{
    GitOp, Oid, RepoSnapshot, ResetMode, StashEntry, TodoAction, TodoEntry,
    default_squash_messages,
};

use crate::app::Action;
use crate::i18n::I18n;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputValidation {
    None,
    /// `git check-ref-format` + duplicate check against `existing`.
    RefName {
        what: &'static str,
        existing: Vec<String>,
    },
    NonEmpty,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputIntent {
    /// Checkbox = "Checkout branch".
    NewBranch {
        start: String,
    },
    RenameBranch {
        old: String,
    },
    /// Checkbox = "Annotated (edit message next)".
    NewTag {
        target: Oid,
    },
    CheckoutRevision,
    UnstashAsBranch {
        index: usize,
    },
    /// Checkbox = "Include untracked files".
    StashMessage,
    /// New local name for a remote branch whose local namesake diverged.
    RemoteCheckoutAs {
        start: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputDialog {
    pub title: String,
    pub prompt: String,
    pub value: String,
    pub validation: InputValidation,
    pub checkbox: Option<(String, bool)>,
    pub intent: InputIntent,
}

impl InputDialog {
    pub fn new(
        title: impl Into<String>,
        prompt: impl Into<String>,
        value: impl Into<String>,
        validation: InputValidation,
        intent: InputIntent,
    ) -> Self {
        Self {
            title: title.into(),
            prompt: prompt.into(),
            value: value.into(),
            validation,
            checkbox: None,
            intent,
        }
    }

    pub fn with_checkbox(mut self, label: impl Into<String>, on: bool) -> Self {
        self.checkbox = Some((label.into(), on));
        self
    }

    /// Live inline validation error (red text; disables the confirm button).
    pub fn error(&self, i18n: &I18n) -> Option<String> {
        match &self.validation {
            InputValidation::None => None,
            InputValidation::NonEmpty => self
                .value
                .trim()
                .is_empty()
                .then(|| i18n.t("common.cannot_be_empty").to_string()),
            InputValidation::RefName { what, existing } => {
                let refs: Vec<&str> = existing.iter().map(String::as_str).collect();
                i18n.validate_ref_name(&self.value, &refs, what).err()
            }
        }
    }

    pub fn can_confirm(&self, i18n: &I18n) -> bool {
        self.error(i18n).is_none()
    }

    pub fn checked(&self) -> bool {
        self.checkbox.as_ref().is_some_and(|(_, on)| *on)
    }

    pub fn confirm_actions(&self, i18n: &I18n) -> Vec<Action> {
        let value = self.value.trim().to_string();
        let checked = self.checked();
        match &self.intent {
            InputIntent::NewBranch { start } => vec![Action::RunGitOp(if checked {
                GitOp::CheckoutNewBranch {
                    name: value,
                    start: start.clone(),
                    track: false,
                }
            } else {
                GitOp::CreateBranch {
                    name: value,
                    start: start.clone(),
                }
            })],
            InputIntent::RenameBranch { old } => vec![Action::RunGitOp(GitOp::RenameBranch {
                old: old.clone(),
                new: value,
            })],
            InputIntent::NewTag { target } => {
                if checked {
                    vec![Action::OpenDialog(Box::new(Dialog::Message(
                        MessageDialog::new(
                            i18n.t_fmt("dialog.new_tag.annotated_title", &[("name", &value)]),
                            String::new(),
                            MessageIntent::AnnotatedTag {
                                name: value,
                                target: *target,
                            },
                        ),
                    )))]
                } else {
                    vec![Action::RunGitOp(GitOp::CreateTag {
                        name: value,
                        target: *target,
                        message: None,
                    })]
                }
            }
            InputIntent::CheckoutRevision => vec![Action::CheckoutRevisionExpr(value)],
            InputIntent::UnstashAsBranch { index } => {
                vec![Action::RunGitOp(GitOp::StashBranch {
                    index: *index,
                    name: value,
                })]
            }
            InputIntent::StashMessage => vec![Action::RunGitOp(GitOp::StashPush {
                message: (!value.is_empty()).then_some(value),
                include_untracked: checked,
                paths: Vec::new(),
            })],
            InputIntent::RemoteCheckoutAs { start } => {
                vec![Action::RunGitOp(GitOp::CheckoutNewBranch {
                    name: value,
                    start: start.clone(),
                    track: true,
                })]
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct ChoiceOption {
    pub label: String,
    pub detail: String,
    pub mnemonic: Option<char>,
    pub danger: bool,
    pub actions: Vec<Action>,
}

impl ChoiceOption {
    pub fn new(label: impl Into<String>, mnemonic: Option<char>, actions: Vec<Action>) -> Self {
        Self {
            label: label.into(),
            detail: String::new(),
            mnemonic,
            danger: false,
            actions,
        }
    }

    pub fn detail(mut self, d: impl Into<String>) -> Self {
        self.detail = d.into();
        self
    }

    pub fn danger(mut self) -> Self {
        self.danger = true;
        self
    }
}

#[derive(Debug, Clone)]
pub struct ChoiceDialog {
    pub title: String,
    pub body: Vec<String>,
    pub options: Vec<ChoiceOption>,
    pub selected: usize,
    /// Radio group with a separate `[confirm] [Cancel]` row; otherwise every
    /// option is itself a button.
    pub radio: bool,
    pub confirm_label: String,
    /// Red title / border (dangerous operation).
    pub danger: bool,
}

impl ChoiceDialog {
    pub fn buttons(
        title: impl Into<String>,
        body: Vec<String>,
        options: Vec<ChoiceOption>,
    ) -> Self {
        Self {
            title: title.into(),
            body,
            options,
            selected: 0,
            radio: false,
            confirm_label: String::new(),
            danger: false,
        }
    }

    pub fn radio(
        title: impl Into<String>,
        body: Vec<String>,
        options: Vec<ChoiceOption>,
        selected: usize,
        confirm_label: impl Into<String>,
    ) -> Self {
        Self {
            title: title.into(),
            body,
            options,
            selected,
            radio: true,
            confirm_label: confirm_label.into(),
            danger: false,
        }
    }

    /// Two-button dangerous confirmation (`Reset --hard`, `Drop Commit`, …).
    pub fn confirm(
        title: impl Into<String>,
        body: Vec<String>,
        confirm: impl Into<String>,
        cancel: impl Into<String>,
        actions: Vec<Action>,
    ) -> Self {
        Self::confirm_with_default(title, body, confirm, cancel, true, false, actions)
    }

    /// Two-button confirmation dialog with configurable danger styling and initial button focus.
    pub fn confirm_with_default(
        title: impl Into<String>,
        body: Vec<String>,
        confirm: impl Into<String>,
        cancel: impl Into<String>,
        danger: bool,
        default_confirm: bool,
        actions: Vec<Action>,
    ) -> Self {
        let confirm_opt = ChoiceOption::new(confirm, None, actions);
        let confirm_opt = if danger {
            confirm_opt.danger()
        } else {
            confirm_opt
        };
        let mut d = Self::buttons(
            title,
            body,
            vec![
                confirm_opt,
                ChoiceOption::new(cancel, Some('c'), Vec::new()),
            ],
        );
        d.selected = if default_confirm { 0 } else { 1 };
        d.danger = danger;
        d
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MessageIntent {
    RewordHead,
    /// Apply the edited text as `todo[entry].message` then rewrite history.
    Rewrite {
        base: Option<Oid>,
        todo: Vec<TodoEntry>,
        entry: usize,
    },
    AnnotatedTag {
        name: String,
        target: Oid,
    },
    /// Message for one entry of the open interactive-rebase dialog.
    TodoEntry {
        idx: usize,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageDialog {
    pub title: String,
    pub text: String,
    pub intent: MessageIntent,
}

impl MessageDialog {
    pub fn new(title: impl Into<String>, text: String, intent: MessageIntent) -> Self {
        Self {
            title: title.into(),
            text,
            intent,
        }
    }

    pub fn confirm_actions(&self, i18n: &I18n) -> Vec<Action> {
        let msg = self.text.trim_end().to_string();
        if msg.trim().is_empty() && !matches!(self.intent, MessageIntent::AnnotatedTag { .. }) {
            return vec![Action::Notice(i18n.t("common.commit_msg_empty").into())];
        }
        match &self.intent {
            MessageIntent::RewordHead => {
                vec![Action::RunGitOp(GitOp::RewordHead { message: msg })]
            }
            MessageIntent::Rewrite { base, todo, entry } => {
                let mut todo = todo.clone();
                if let Some(e) = todo.get_mut(*entry) {
                    e.message = Some(msg);
                }
                vec![Action::RunGitOp(GitOp::RewriteHistory {
                    base: *base,
                    todo,
                })]
            }
            MessageIntent::AnnotatedTag { name, target } => {
                vec![Action::RunGitOp(GitOp::CreateTag {
                    name: name.clone(),
                    target: *target,
                    message: (!msg.trim().is_empty()).then_some(msg),
                })]
            }
            MessageIntent::TodoEntry { idx } => vec![Action::SetTodoMessage {
                idx: *idx,
                message: msg,
            }],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RebaseTodoDialog {
    pub base: Option<Oid>,
    pub entries: Vec<TodoEntry>,
    pub selected: usize,
    /// Full commit messages, used for squash defaults and reword prefill.
    pub messages: Vec<String>,
}

impl RebaseTodoDialog {
    pub fn error(&self, i18n: &I18n) -> Option<String> {
        let first_kept = self.entries.iter().find(|e| e.action != TodoAction::Drop);
        if let Some(e) = first_kept
            && matches!(e.action, TodoAction::Squash | TodoAction::Fixup)
        {
            return Some(i18n.t_fmt("dialog.rebase.err_squash_first", &[("subject", &e.subject)]));
        }
        if self.entries.iter().all(|e| e.action == TodoAction::Drop) {
            return Some(i18n.t("dialog.rebase.err_drop_all").into());
        }
        if let Some(e) = self
            .entries
            .iter()
            .find(|e| e.action == TodoAction::Reword && e.message.is_none())
        {
            return Some(i18n.t_fmt("dialog.rebase.err_reword_missing", &[("subject", &e.subject)]));
        }
        None
    }

    pub fn confirm_actions(&self) -> Vec<Action> {
        let mut todo = self.entries.clone();
        let lookup: Vec<(Oid, String)> = self
            .entries
            .iter()
            .zip(&self.messages)
            .map(|(e, m)| (e.oid, m.clone()))
            .collect();
        default_squash_messages(&mut todo, |oid| {
            lookup
                .iter()
                .find(|(o, _)| o == oid)
                .map(|(_, m)| m.clone())
                .unwrap_or_default()
        });
        vec![Action::RunGitOp(GitOp::RewriteHistory {
            base: self.base,
            todo,
        })]
    }

    pub fn move_selected(&mut self, delta: isize) {
        let len = self.entries.len();
        if len == 0 {
            return;
        }
        let to = (self.selected as isize + delta).clamp(0, len as isize - 1) as usize;
        if to != self.selected {
            self.entries.swap(self.selected, to);
            self.messages.swap(self.selected, to);
            self.selected = to;
        }
    }
}

#[derive(Debug, Clone)]
pub enum Dialog {
    Input(InputDialog),
    Choice(ChoiceDialog),
    Message(MessageDialog),
    RebaseTodo(RebaseTodoDialog),
}

impl Dialog {
    /// Actions emitted when the dialog is confirmed (`None` = cannot confirm).
    pub fn confirm_actions(&self, i18n: &I18n) -> Option<Vec<Action>> {
        match self {
            Self::Input(d) => d.can_confirm(i18n).then(|| d.confirm_actions(i18n)),
            Self::Choice(d) => d.options.get(d.selected).map(|o| o.actions.clone()),
            Self::Message(d) => Some(d.confirm_actions(i18n)),
            Self::RebaseTodo(d) => d.error(i18n).is_none().then(|| d.confirm_actions()),
        }
    }
}

/// Outcome of routing a key to the topmost dialog.
pub enum DialogKey {
    None,
    Redraw,
    Cancel,
    Confirm,
    Actions(Vec<Action>),
}

pub fn route_dialog_key(dialog: &mut Dialog, key: &KeyEvent, i18n: &I18n) -> DialogKey {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let plain = !ctrl && !alt;
    if key.code == KeyCode::Esc {
        return DialogKey::Cancel;
    }
    match dialog {
        Dialog::Input(d) => match key.code {
            KeyCode::Enter => DialogKey::Confirm,
            KeyCode::Backspace => {
                d.value.pop();
                DialogKey::Redraw
            }
            KeyCode::Char('u') if ctrl => {
                d.value.clear();
                DialogKey::Redraw
            }
            KeyCode::Tab | KeyCode::BackTab | KeyCode::Down | KeyCode::Up => {
                if let Some((_, on)) = d.checkbox.as_mut() {
                    *on = !*on;
                }
                DialogKey::Redraw
            }
            KeyCode::Char(c) if plain && !c.is_control() => {
                d.value.push(c);
                DialogKey::Redraw
            }
            _ => DialogKey::None,
        },
        Dialog::Choice(d) => {
            let len = d.options.len().max(1);
            match key.code {
                KeyCode::Enter => DialogKey::Confirm,
                KeyCode::Up | KeyCode::Left | KeyCode::BackTab => {
                    d.selected = (d.selected + len - 1) % len;
                    DialogKey::Redraw
                }
                KeyCode::Down | KeyCode::Right | KeyCode::Tab => {
                    d.selected = (d.selected + 1) % len;
                    DialogKey::Redraw
                }
                KeyCode::Char(c) if plain => {
                    let c = c.to_ascii_lowercase();
                    match d
                        .options
                        .iter()
                        .position(|o| o.mnemonic.is_some_and(|m| m.to_ascii_lowercase() == c))
                    {
                        Some(idx) if d.radio => {
                            d.selected = idx;
                            DialogKey::Redraw
                        }
                        Some(idx) => {
                            d.selected = idx;
                            DialogKey::Confirm
                        }
                        None if !d.radio && d.options.len() == 2 && c == 'y' => {
                            d.selected = 0;
                            DialogKey::Confirm
                        }
                        None if !d.radio && d.options.len() == 2 && matches!(c, 'n' | 'q') => {
                            DialogKey::Cancel
                        }
                        None => DialogKey::None,
                    }
                }
                _ => DialogKey::None,
            }
        }
        Dialog::Message(d) => match key.code {
            KeyCode::Enter if ctrl || alt => DialogKey::Confirm,
            KeyCode::Char('s') if ctrl => DialogKey::Confirm,
            KeyCode::Enter => {
                d.text.push('\n');
                DialogKey::Redraw
            }
            KeyCode::Backspace => {
                d.text.pop();
                DialogKey::Redraw
            }
            KeyCode::Char(c) if plain && !c.is_control() => {
                d.text.push(c);
                DialogKey::Redraw
            }
            _ => DialogKey::None,
        },
        Dialog::RebaseTodo(d) => match key.code {
            KeyCode::Enter => DialogKey::Confirm,
            KeyCode::Up if alt || key.modifiers.contains(KeyModifiers::SHIFT) => {
                d.move_selected(-1);
                DialogKey::Redraw
            }
            KeyCode::Down if alt || key.modifiers.contains(KeyModifiers::SHIFT) => {
                d.move_selected(1);
                DialogKey::Redraw
            }
            KeyCode::Char('K') => {
                d.move_selected(-1);
                DialogKey::Redraw
            }
            KeyCode::Char('J') => {
                d.move_selected(1);
                DialogKey::Redraw
            }
            KeyCode::Up | KeyCode::Char('k') => {
                d.selected = d.selected.saturating_sub(1);
                DialogKey::Redraw
            }
            KeyCode::Down | KeyCode::Char('j') => {
                d.selected = (d.selected + 1).min(d.entries.len().saturating_sub(1));
                DialogKey::Redraw
            }
            KeyCode::Char(' ') => {
                if let Some(e) = d.entries.get_mut(d.selected) {
                    e.action = e.action.next();
                }
                DialogKey::Redraw
            }
            KeyCode::Char(c) if plain => match TodoAction::from_key(c) {
                Some(TodoAction::Reword) => {
                    let idx = d.selected;
                    if let Some(e) = d.entries.get_mut(idx) {
                        e.action = TodoAction::Reword;
                        let text = e
                            .message
                            .clone()
                            .or_else(|| d.messages.get(idx).cloned())
                            .unwrap_or_default();
                        return DialogKey::Actions(vec![Action::OpenDialog(Box::new(
                            Dialog::Message(MessageDialog::new(
                                i18n.t_fmt("dialog.rebase.reword_entry_title", &[("subject", &e.subject)]),
                                text,
                                MessageIntent::TodoEntry { idx },
                            )),
                        ))]);
                    }
                    DialogKey::None
                }
                Some(action) => {
                    if let Some(e) = d.entries.get_mut(d.selected) {
                        e.action = action;
                    }
                    DialogKey::Redraw
                }
                None => DialogKey::None,
            },
            _ => DialogKey::None,
        },
    }
}

/// `New Branch…` dialog with live validation against local branch names.
pub fn new_branch_dialog(
    start: &str,
    start_label: &str,
    existing: Vec<String>,
    default: &str,
    i18n: &I18n,
) -> Dialog {
    Dialog::Input(
        InputDialog::new(
            i18n.t_fmt("dialog.new_branch.title_from", &[("start", start_label)]),
            i18n.t("dialog.new_branch.name_prompt"),
            default,
            InputValidation::RefName {
                what: "Branch",
                existing,
            },
            InputIntent::NewBranch {
                start: start.to_string(),
            },
        )
        .with_checkbox(i18n.t("dialog.new_branch.checkout_checkbox"), true),
    )
}

pub fn new_branch_from_head_dialog(snap: Option<&RepoSnapshot>, i18n: &I18n) -> Dialog {
    let existing = snap
        .map(|s| {
            s.local_branch_names()
                .iter()
                .map(|n| n.to_string())
                .collect()
        })
        .unwrap_or_default();
    let head = snap
        .and_then(|s| s.head_branch.clone())
        .unwrap_or_else(|| "HEAD".into());
    new_branch_dialog("HEAD", &head, existing, "", i18n)
}

pub fn reset_dialog(target: Oid, subject: &str, branch: &str, i18n: &I18n) -> Dialog {
    let short = target.short();
    let options = ResetMode::ALL
        .iter()
        .map(|&mode| {
            let mode_lbl = i18n.reset_mode_label(mode);
            let pad = 8usize.saturating_sub(unicode_width::UnicodeWidthStr::width(mode_lbl));
            let label = format!("{mode_lbl}{} {}", " ".repeat(pad), mode.flag());
            let mnemonic = mode.label().chars().next().map(|c| c.to_ascii_lowercase());
            let actions = if mode.is_destructive() {
                vec![Action::OpenDialog(Box::new(Dialog::Choice(
                    ChoiceDialog::confirm(
                        i18n.t("dialog.reset.hard_title"),
                        vec![
                            i18n.t_fmt(
                                "dialog.reset.hard_prompt",
                                &[("branch", branch), ("oid", &short)],
                            ),
                            i18n.t("dialog.reset.hard_warn").into(),
                        ],
                        i18n.t("dialog.reset.hard_btn"),
                        i18n.cancel(),
                        vec![Action::RunGitOp(GitOp::Reset { mode, target })],
                    ),
                )))]
            } else {
                vec![Action::RunGitOp(GitOp::Reset { mode, target })]
            };
            let o = ChoiceOption::new(label, mnemonic, actions).detail(i18n.reset_mode_desc(mode));
            if mode.is_destructive() { o.danger() } else { o }
        })
        .collect();
    Dialog::Choice(ChoiceDialog::radio(
        i18n.t("dialog.reset.title"),
        vec![
            i18n.t_fmt(
                "dialog.reset.prompt",
                &[("branch", branch), ("oid", &short)],
            ),
            format!("  \"{}\"", crate::views::truncate_to_width(subject, 52)),
        ],
        options,
        1,
        i18n.t("dialog.reset.confirm_btn"),
    ))
}

pub fn stash_label(s: &StashEntry) -> String {
    format!("stash@{{{}}}: {}", s.index, s.message)
}
