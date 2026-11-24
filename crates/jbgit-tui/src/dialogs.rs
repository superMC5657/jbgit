//! Lightweight modal dialog library (`ModalDialog`):
//! validated single-line input (New Branch / Tag / Rename), radio & button
//! choice groups (`GitResetDialog`, Smart Operation, dangerous confirms),
//! multi-line message editor (Reword / Squash / annotated tag) and the
//! interactive-rebase todo table.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use jbgit_core::{
    DetailsKey, DetailsMode, FileDiffKey, GitOp, GitVersion, MergeOptions, Oid, PullOptions,
    RebaseOptionError, RebaseOptions, RefKind, RefNameError, RepoSnapshot, ResetMode, StashEntry,
    TodoAction, TodoEntry, WorktreeBranch, default_squash_messages, sanitize_branch_name,
    sanitize_branch_name_on_typing, validate_ref_name_typed,
};
use jbgit_engine::askpass::Secret;

use crate::app::{Action, ScrollViewport};
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
    /// Remote URL / path: non-empty and no whitespace (no network check).
    RemoteUrl,
    /// Must be one of `candidates` (e.g. a local branch free for a worktree).
    OneOf {
        candidates: Vec<String>,
        error_key: &'static str,
    },
    /// New working-tree location: non-empty and not an existing file or
    /// non-empty directory (relative paths resolve against `base`).
    NewPath { base: std::path::PathBuf },
    /// Existing file (relative paths resolve against `base`); `Tab` completes.
    ExistingFile { base: std::path::PathBuf },
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
    /// Step 1 of Add Remote: the name (step 2 asks for the URL).
    AddRemoteName,
    AddRemoteUrl {
        name: String,
    },
    RenameRemote {
        old: String,
    },
    /// Fetch URL (`push = false`) or push URL of `name`.
    SetRemoteUrl {
        name: String,
        push: bool,
    },
    /// New worktree, step 1 (new branch): the branch name; step 2 asks for the path.
    NewWorktreeBranch {
        start: String,
        ctx: WorktreeDialogCtx,
    },
    /// New worktree, step 1 (existing branch): the branch name.
    ExistingWorktreeBranch {
        ctx: WorktreeDialogCtx,
    },
    /// New worktree, last step: the location.
    WorktreePath {
        branch: WorktreeBranch,
        base: std::path::PathBuf,
    },
    /// Optional lock reason of `git worktree lock`.
    LockWorktree {
        path: std::path::PathBuf,
    },
    /// Repository switcher `Ctrl+O`: any directory inside a repository.
    OpenRepositoryPath,
    /// `Apply Patch…`: patch file path (relative to `base` = working tree root).
    ApplyPatchFile {
        base: std::path::PathBuf,
    },
    /// Tree Browser `Export to File…`: destination file path.
    ExportTreeBlob {
        oid: Oid,
        path: String,
        base: std::path::PathBuf,
    },
}

/// Where new working trees go by default (`<parent>/<repo>-<label>`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeDialogCtx {
    /// Main working tree (names the default directory).
    pub main: std::path::PathBuf,
    /// `[worktree] default_parent` (else the main working tree's parent).
    pub parent: Option<std::path::PathBuf>,
}

impl WorktreeDialogCtx {
    pub fn default_path(&self, label: &str) -> std::path::PathBuf {
        jbgit_core::default_worktree_path(&self.main, self.parent.as_deref(), label)
    }

    /// Directory relative input paths resolve against.
    pub fn base(&self) -> std::path::PathBuf {
        self.parent
            .clone()
            .or_else(|| self.main.parent().map(std::path::Path::to_path_buf))
            .unwrap_or_default()
    }

    /// Last step of New Worktree: the location for `branch`.
    pub fn path_dialog(&self, branch: WorktreeBranch, i18n: &I18n) -> Dialog {
        let label = match &branch {
            WorktreeBranch::Existing(n) | WorktreeBranch::New { name: n, .. } => n.clone(),
            WorktreeBranch::Detached(oid) => oid.short(),
        };
        let title = match &branch {
            WorktreeBranch::Detached(oid) => {
                i18n.t_fmt("dialog.worktree.path_title_detached", &[("rev", &oid.short())])
            }
            _ => i18n.t_fmt("dialog.worktree.path_title", &[("branch", &label)]),
        };
        Dialog::Input(
            InputDialog::new(
                title,
                i18n.t("dialog.worktree.path_prompt"),
                self.default_path(&label).display().to_string(),
                InputValidation::NewPath { base: self.base() },
                InputIntent::WorktreePath {
                    branch,
                    base: self.base(),
                },
            )
            .with_checkbox(i18n.t("dialog.worktree.force_checkbox"), false),
        )
    }
}

/// Resolves `value` against `base` when it is relative.
/// `Apply Patch…` path prompt (relative to the working tree root, `Tab` completes).
pub fn apply_patch_file_dialog(state: &crate::app::AppState) -> Dialog {
    let base = state.current_workdir();
    let i18n = &state.i18n;
    Dialog::Input(InputDialog::new(
        i18n.t("dialog.apply_patch.path_title"),
        i18n.t("dialog.apply_patch.path_prompt"),
        "",
        InputValidation::ExistingFile { base: base.clone() },
        InputIntent::ApplyPatchFile { base },
    ))
}

pub fn resolve_path(base: &std::path::Path, value: &str) -> std::path::PathBuf {
    let p = std::path::PathBuf::from(value.trim());
    if p.is_absolute() || base.as_os_str().is_empty() {
        p
    } else {
        base.join(p)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputDialog {
    pub title: String,
    pub prompt: String,
    pub value: String,
    pub validation: InputValidation,
    pub checkbox: Option<(String, bool)>,
    /// Optional second checkbox (e.g. `Sign tag (-s)` in New Tag dialog; toggled via `Ctrl+S` or click).
    pub secondary_checkbox: Option<(String, bool)>,
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
            secondary_checkbox: None,
            intent,
        }
    }

    pub fn with_checkbox(mut self, label: impl Into<String>, on: bool) -> Self {
        self.checkbox = Some((label.into(), on));
        self
    }

    pub fn with_secondary_checkbox(mut self, label: impl Into<String>, on: bool) -> Self {
        self.secondary_checkbox = Some((label.into(), on));
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
            InputValidation::RemoteUrl => {
                let v = self.value.trim();
                if v.is_empty() {
                    Some(i18n.t("dialog.remotes.url_empty").to_string())
                } else if v.chars().any(char::is_whitespace) {
                    Some(i18n.t("dialog.remotes.url_spaces").to_string())
                } else {
                    None
                }
            }
            InputValidation::RefName { what, existing } => {
                let refs: Vec<&str> = existing.iter().map(String::as_str).collect();
                i18n.validate_ref_name(&self.value, &refs, what).err()
            }
            InputValidation::OneOf {
                candidates,
                error_key,
            } => {
                let v = self.value.trim();
                (!candidates.iter().any(|c| c == v))
                    .then(|| i18n.t_fmt(error_key, &[("name", v)]))
            }
            InputValidation::ExistingFile { base } => {
                let v = self.value.trim();
                if v.is_empty() {
                    return Some(i18n.t("common.cannot_be_empty").to_string());
                }
                (!resolve_path(base, v).is_file())
                    .then(|| i18n.t("dialog.apply_patch.file_not_found").to_string())
            }
            InputValidation::NewPath { base } => {
                let v = self.value.trim();
                if v.is_empty() {
                    return Some(i18n.t("common.cannot_be_empty").to_string());
                }
                let p = resolve_path(base, v);
                let occupied = p.is_file()
                    || std::fs::read_dir(&p).is_ok_and(|mut d| d.next().is_some());
                occupied.then(|| i18n.t("dialog.worktree.path_exists").to_string())
            }
        }
    }

    pub fn can_confirm(&self, i18n: &I18n) -> bool {
        self.error(i18n).is_none()
    }

    pub fn checked(&self) -> bool {
        self.checkbox.as_ref().is_some_and(|(_, on)| *on)
    }

    pub fn secondary_checked(&self) -> bool {
        self.secondary_checkbox.as_ref().is_some_and(|(_, on)| *on)
    }

    pub fn confirm_actions(&self, i18n: &I18n) -> Vec<Action> {
        let value = self.value.trim().to_string();
        let checked = self.checked();
        let sign = self.secondary_checked();
        match &self.intent {
            InputIntent::NewBranch { start } => vec![Action::RunGitOp(if checked {
                GitOp::CheckoutNewBranch {
                    name: value,
                    start: start.clone(),
                    track: false,
                    force: false,
                }
            } else {
                GitOp::CreateBranch {
                    name: value,
                    start: start.clone(),
                    force: false,
                    track: false,
                }
            })],
            InputIntent::RenameBranch { old } => {
                let rename = GitOp::RenameBranch {
                    old: old.clone(),
                    new: value.clone(),
                };
                if checked {
                    vec![Action::RunGitOp(GitOp::Sequence(vec![
                        rename,
                        GitOp::UnsetUpstream { branch: value },
                    ]))]
                } else {
                    vec![Action::RunGitOp(rename)]
                }
            }
            InputIntent::NewTag { target } => {
                if checked || sign {
                    vec![Action::OpenDialog(Box::new(Dialog::Message(
                        MessageDialog::new(
                            i18n.t_fmt("dialog.new_tag.annotated_title", &[("name", &value)]),
                            String::new(),
                            MessageIntent::AnnotatedTag {
                                name: value,
                                target: *target,
                                sign,
                            },
                        ),
                    )))]
                } else {
                    vec![Action::RunGitOp(GitOp::CreateTag {
                        name: value,
                        target: *target,
                        message: None,
                        force: false,
                        sign: false,
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
                keep_index: false,
                paths: Vec::new(),
            })],
            InputIntent::RemoteCheckoutAs { start } => {
                vec![Action::RunGitOp(GitOp::CheckoutNewBranch {
                    name: value,
                    start: start.clone(),
                    track: true,
                    force: false,
                })]
            }
            InputIntent::AddRemoteName => vec![Action::OpenDialog(Box::new(Dialog::Input(
                InputDialog::new(
                    i18n.t_fmt("dialog.remotes.add_title", &[("name", &value)]),
                    i18n.t_fmt("dialog.remotes.url_prompt", &[("name", &value)]),
                    "",
                    InputValidation::RemoteUrl,
                    InputIntent::AddRemoteUrl { name: value },
                ),
            )))],
            InputIntent::AddRemoteUrl { name } => vec![Action::RunGitOp(GitOp::AddRemote {
                name: name.clone(),
                url: value,
            })],
            InputIntent::RenameRemote { old } => vec![Action::RunGitOp(GitOp::RenameRemote {
                old: old.clone(),
                new: value,
            })],
            InputIntent::SetRemoteUrl { name, push } => {
                vec![Action::RunGitOp(GitOp::SetRemoteUrl {
                    name: name.clone(),
                    url: value,
                    push: *push,
                })]
            }
            InputIntent::NewWorktreeBranch { start, ctx } => vec![Action::OpenDialog(Box::new(
                ctx.path_dialog(
                    WorktreeBranch::New {
                        name: value,
                        start: start.clone(),
                    },
                    i18n,
                ),
            ))],
            InputIntent::ExistingWorktreeBranch { ctx } => vec![Action::OpenDialog(Box::new(
                ctx.path_dialog(WorktreeBranch::Existing(value), i18n),
            ))],
            InputIntent::WorktreePath { branch, base } => {
                vec![Action::RunGitOp(GitOp::AddWorktree {
                    path: resolve_path(base, &value),
                    branch: branch.clone(),
                    force: checked,
                })]
            }
            InputIntent::LockWorktree { path } => vec![Action::RunGitOp(GitOp::LockWorktree {
                path: path.clone(),
                reason: (!value.is_empty()).then_some(value),
            })],
            InputIntent::OpenRepositoryPath => {
                vec![Action::SwitchRepository(std::path::PathBuf::from(value))]
            }
            InputIntent::ApplyPatchFile { base } => vec![Action::OpenApplyPatch(
                jbgit_core::PatchInput::File(resolve_path(base, &value)),
            )],
            InputIntent::ExportTreeBlob { oid, path, base } => vec![Action::ExportTreeBlob {
                oid: *oid,
                path: path.clone(),
                dest: resolve_path(base, &value),
            }],
        }
    }
}

/// Configured remote names except `skip` (duplicate check of remote dialogs).
fn other_remote_names(snap: Option<&RepoSnapshot>, skip: Option<&str>) -> Vec<String> {
    snap.map(|s| {
        s.remote_names()
            .filter(|n| Some(*n) != skip)
            .map(ToString::to_string)
            .collect()
    })
    .unwrap_or_default()
}

/// `Add Remote` step 1 (name; `origin` is suggested when no remote exists).
pub fn add_remote_dialog(snap: Option<&RepoSnapshot>, i18n: &I18n) -> Dialog {
    let suggested = if snap.is_some_and(|s| s.remotes.is_empty()) {
        "origin"
    } else {
        ""
    };
    Dialog::Input(InputDialog::new(
        i18n.t("dialog.remotes.add_name_title"),
        i18n.t("dialog.remotes.name_prompt"),
        suggested,
        InputValidation::RefName {
            what: "validation.what.remote",
            existing: other_remote_names(snap, None),
        },
        InputIntent::AddRemoteName,
    ))
}

pub fn rename_remote_dialog(snap: Option<&RepoSnapshot>, old: &str, i18n: &I18n) -> Dialog {
    Dialog::Input(InputDialog::new(
        i18n.t_fmt("dialog.remotes.rename_title", &[("name", old)]),
        i18n.t("dialog.remotes.name_prompt"),
        old,
        InputValidation::RefName {
            what: "validation.what.remote",
            existing: snap
                .map(|s| s.remote_names().map(ToString::to_string).collect())
                .unwrap_or_default(),
        },
        InputIntent::RenameRemote {
            old: old.to_string(),
        },
    ))
}

/// Edit the fetch URL (`push = false`) or push URL of `name`. The current
/// value is pre-filled as stored (the user is editing their own config).
pub fn set_remote_url_dialog(
    snap: Option<&RepoSnapshot>,
    name: &str,
    push: bool,
    i18n: &I18n,
) -> Dialog {
    let remote = snap.and_then(|s| s.remote(name));
    let current = if push {
        remote.and_then(|r| r.effective_push_url())
    } else {
        remote.and_then(|r| r.fetch_url.as_deref())
    }
    .unwrap_or_default();
    let title_key = if push {
        "dialog.remotes.push_url_title"
    } else {
        "dialog.remotes.url_title"
    };
    Dialog::Input(InputDialog::new(
        i18n.t_fmt(title_key, &[("name", name)]),
        i18n.t_fmt("dialog.remotes.url_prompt", &[("name", name)]),
        current,
        InputValidation::RemoteUrl,
        InputIntent::SetRemoteUrl {
            name: name.to_string(),
            push,
        },
    ))
}

/// Masked input answering a `GIT_ASKPASS` / `SSH_ASKPASS` prompt. The value
/// lives in a zeroizing buffer and never appears in `Debug` output.
#[derive(Clone)]
pub struct CredentialDialog {
    /// Id of the queued askpass request this dialog answers.
    pub id: u64,
    pub prompt: String,
    pub masked: bool,
    /// `true` when verifying an SSH host-key fingerprint (unmasked, full text).
    pub host_key_confirm: bool,
    /// Masked value temporarily shown in clear text (`Tab`).
    pub revealed: bool,
    pub value: Secret,
}

impl CredentialDialog {
    pub fn new(id: u64, prompt: impl Into<String>, masked: bool, host_key_confirm: bool) -> Self {
        Self {
            id,
            prompt: prompt.into(),
            masked: masked && !host_key_confirm,
            host_key_confirm,
            revealed: false,
            // Pre-sized so typing never reallocates (and leaves copies behind).
            value: Secret::new(String::with_capacity(256)),
        }
    }

    pub fn push_str(&mut self, text: &str) {
        for c in text.chars().filter(|c| !c.is_control()) {
            self.value.push(c);
        }
    }

    pub fn clear(&mut self) {
        self.value = Secret::new(String::with_capacity(256));
    }

    /// Text drawn in the input line (bullets while masked).
    pub fn display_value(&self) -> String {
        if self.masked && !self.revealed {
            "•".repeat(self.value.chars().count())
        } else {
            self.value.to_string()
        }
    }

    /// Moves the typed value out, leaving an empty buffer behind.
    pub fn take_value(&mut self) -> Secret {
        std::mem::replace(&mut self.value, Secret::new(String::new()))
    }
}

impl std::fmt::Debug for CredentialDialog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CredentialDialog")
            .field("id", &self.id)
            .field("prompt", &self.prompt)
            .field("masked", &self.masked)
            .field("value", &"<redacted>")
            .finish()
    }
}

#[derive(Debug, Clone)]
pub struct ChoiceOption {
    pub label: String,
    pub detail: String,
    pub mnemonic: Option<char>,
    pub danger: bool,
    pub actions: Vec<Action>,
    /// Optional alternate actions emitted when the dialog's checkbox is checked.
    pub checked_actions: Option<Vec<Action>>,
}

impl ChoiceOption {
    pub fn new(label: impl Into<String>, mnemonic: Option<char>, actions: Vec<Action>) -> Self {
        Self {
            label: label.into(),
            detail: String::new(),
            mnemonic,
            danger: false,
            actions,
            checked_actions: None,
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

    pub fn when_checked(mut self, actions: Vec<Action>) -> Self {
        self.checked_actions = Some(actions);
        self
    }
}

#[derive(Debug, Clone)]
pub struct ChoiceCheckbox {
    pub label: String,
    pub checked: bool,
    pub checked_actions: Vec<Action>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeleteTagRemoteProbe {
    pub name: String,
    pub tip: Oid,
    pub remote: String,
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
    pub checkbox: Option<ChoiceCheckbox>,
    pub remote_tag_probe: Option<DeleteTagRemoteProbe>,
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
            checkbox: None,
            remote_tag_probe: None,
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
            checkbox: None,
            remote_tag_probe: None,
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
        sign: bool,
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
                vec![Action::RunGitOp(GitOp::RewordHead {
                    message: msg,
                    no_verify: false,
                })]
            }
            MessageIntent::Rewrite { base, todo, entry } => {
                let mut todo = todo.clone();
                if let Some(e) = todo.get_mut(*entry) {
                    e.message = Some(msg);
                }
                vec![Action::RunGitOp(GitOp::RewriteHistory {
                    base: *base,
                    todo,
                    no_verify: false,
                })]
            }
            MessageIntent::AnnotatedTag { name, target, sign } => {
                vec![Action::RunGitOp(GitOp::CreateTag {
                    name: name.clone(),
                    target: *target,
                    message: (!msg.trim().is_empty()).then_some(msg),
                    force: false,
                    sign: *sign,
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
    /// Selected changed-file index in the commit summary preview.
    pub selected_file: usize,
    /// Full unified diff overlay for the selected commit/file (`d`).
    pub show_diff: bool,
    /// Bounded scroll state for the changed-file summary preview.
    pub files_scroll: ScrollViewport,
    /// Bounded scroll state for the full diff preview.
    pub diff_scroll: ScrollViewport,
    /// In-flight commit details key (for single-flight dedup).
    pub details_requested: Option<DetailsKey>,
    /// In-flight file diff key (for single-flight dedup).
    pub diff_requested: Option<FileDiffKey>,
    /// Whether reword/squash `exec git commit --amend` skips repository hooks.
    pub no_verify: bool,
}

impl RebaseTodoDialog {
    pub fn new(base: Option<Oid>, entries: Vec<TodoEntry>, messages: Vec<String>) -> Self {
        Self {
            base,
            entries,
            selected: 0,
            messages,
            selected_file: 0,
            show_diff: false,
            files_scroll: ScrollViewport::new(),
            diff_scroll: ScrollViewport::new(),
            details_requested: None,
            diff_requested: None,
            no_verify: false,
        }
    }

    /// Selected commit OID, or `None` when on a non-commit line (e.g. `update-ref`).
    pub fn selected_commit_oid(&self) -> Option<Oid> {
        self.entries
            .get(self.selected)
            .filter(|e| e.action.is_commit())
            .map(|e| e.oid)
    }

    pub fn desired_details_key(&self) -> Option<DetailsKey> {
        self.selected_commit_oid().map(|oid| DetailsKey {
            mode: DetailsMode::Commits,
            oids: vec![oid],
            parent_index: 0,
            path_prefix: None,
        })
    }

    pub fn select_entry(&mut self, idx: usize) {
        let clamped = idx.min(self.entries.len().saturating_sub(1));
        if clamped != self.selected {
            self.selected = clamped;
            self.selected_file = 0;
            self.files_scroll.reset();
            self.diff_scroll.reset();
            self.diff_requested = None;
            if self.selected_commit_oid().is_none() {
                self.show_diff = false;
            }
        }
    }

    pub fn error(&self, i18n: &I18n) -> Option<String> {
        let commits: Vec<&TodoEntry> = self
            .entries
            .iter()
            .filter(|e| e.action.is_commit())
            .collect();
        let first_kept = commits.iter().copied().find(|e| e.action != TodoAction::Drop);
        if let Some(e) = first_kept
            && matches!(e.action, TodoAction::Squash | TodoAction::Fixup)
        {
            return Some(i18n.t_fmt("dialog.rebase.err_squash_first", &[("subject", &e.subject)]));
        }
        if !commits.is_empty() && commits.iter().all(|e| e.action == TodoAction::Drop) {
            return Some(i18n.t("dialog.rebase.err_drop_all").into());
        }
        if let Some(e) = commits
            .iter()
            .copied()
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
            no_verify: self.no_verify,
        })]
    }

    pub fn move_selected(&mut self, delta: isize) {
        let len = self.entries.len();
        if len == 0 || !self.entries[self.selected].action.is_editable() {
            return;
        }
        let to = (self.selected as isize + delta).clamp(0, len as isize - 1) as usize;
        if to != self.selected && self.entries[to].action.is_editable() {
            self.entries.swap(self.selected, to);
            if self.selected < self.messages.len() && to < self.messages.len() {
                self.messages.swap(self.selected, to);
            }
            self.selected = to;
        }
    }
}

/// Rows of the Merge options dialog, top to bottom.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MergeDialogRow {
    FastForward,
    Squash,
    NoCommit,
    AllowUnrelated,
    Strategy,
    StrategyOption,
    Message,
}

impl MergeDialogRow {
    pub const ALL: [Self; 7] = [
        Self::FastForward,
        Self::Squash,
        Self::NoCommit,
        Self::AllowUnrelated,
        Self::Strategy,
        Self::StrategyOption,
        Self::Message,
    ];

    pub const fn mnemonic(self) -> char {
        match self {
            Self::FastForward => 'f',
            Self::Squash => 's',
            Self::NoCommit => 'n',
            Self::AllowUnrelated => 'u',
            Self::Strategy => 't',
            Self::StrategyOption => 'x',
            Self::Message => 'm',
        }
    }
}

/// `-s` choices offered by the dialog (empty = Git default).
pub const MERGE_STRATEGIES: &[&str] = &["", "ort", "recursive", "resolve", "octopus", "ours", "subtree"];
/// `-X` choices offered by the dialog (empty = none).
pub const MERGE_STRATEGY_OPTIONS: &[&str] = &[
    "",
    "ours",
    "theirs",
    "patience",
    "diff-algorithm=histogram",
    "ignore-space-change",
    "ignore-all-space",
    "renormalize",
    "no-renames",
];

fn cycle_choice(choices: &[&str], current: Option<&str>, forward: bool) -> Option<String> {
    let cur = current.unwrap_or("");
    let len = choices.len();
    let idx = choices.iter().position(|c| *c == cur).unwrap_or(0);
    let next = if forward {
        (idx + 1) % len
    } else {
        (idx + len - 1) % len
    };
    let v = choices[next];
    (!v.is_empty()).then(|| v.to_string())
}

/// `Merge…` options dialog (`GitMergeDialog`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergeDialog {
    pub rev: String,
    pub current: String,
    /// `current..rev` commit count (None when unknown).
    pub commit_count: Option<usize>,
    pub options: MergeOptions,
    pub selected: usize,
}

impl MergeDialog {
    pub fn new(
        rev: impl Into<String>,
        current: impl Into<String>,
        commit_count: Option<usize>,
        remembered: MergeOptions,
    ) -> Self {
        Self {
            rev: rev.into(),
            current: current.into(),
            commit_count,
            options: MergeOptions {
                message: None,
                ..remembered
            },
            selected: 0,
        }
    }

    pub fn row(&self) -> MergeDialogRow {
        MergeDialogRow::ALL[self.selected.min(MergeDialogRow::ALL.len() - 1)]
    }

    pub fn editing_message(&self) -> bool {
        self.row() == MergeDialogRow::Message
    }

    pub fn move_selection(&mut self, delta: isize) {
        let len = MergeDialogRow::ALL.len() as isize;
        self.selected = (self.selected as isize + delta).rem_euclid(len) as usize;
    }

    /// Toggles / cycles `row` (`forward = false` cycles backwards).
    pub fn toggle(&mut self, row: MergeDialogRow, forward: bool) {
        let o = &mut self.options;
        match row {
            MergeDialogRow::FastForward => {
                o.ff = if forward { o.ff.next() } else { o.ff.prev() };
            }
            MergeDialogRow::Squash => o.squash = !o.squash,
            MergeDialogRow::NoCommit => o.no_commit = !o.no_commit,
            MergeDialogRow::AllowUnrelated => o.allow_unrelated = !o.allow_unrelated,
            MergeDialogRow::Strategy => {
                o.strategy = cycle_choice(MERGE_STRATEGIES, o.strategy.as_deref(), forward);
            }
            MergeDialogRow::StrategyOption => {
                o.strategy_option =
                    cycle_choice(MERGE_STRATEGY_OPTIONS, o.strategy_option.as_deref(), forward);
            }
            MergeDialogRow::Message => {}
        }
    }

    pub fn message_text(&self) -> &str {
        self.options.message.as_deref().unwrap_or("")
    }

    pub fn push_message(&mut self, text: &str) {
        self.options
            .message
            .get_or_insert_with(String::new)
            .push_str(text);
    }

    pub fn pop_message(&mut self) {
        if let Some(m) = self.options.message.as_mut() {
            m.pop();
            if m.is_empty() {
                self.options.message = None;
            }
        }
    }

    /// Localized description of the first option clash, if any.
    pub fn error(&self, i18n: &I18n) -> Option<String> {
        self.options.validate().err().map(|c| {
            let (a, b) = c.flags();
            i18n.t_fmt("dialog.merge.conflict", &[("a", a), ("b", b)])
        })
    }

    pub fn confirm_actions(&self) -> Vec<Action> {
        let mut options = self.options.clone();
        options.message = options.effective_message().map(ToString::to_string);
        vec![
            Action::RememberMergeOptions(options.clone()),
            Action::RunGitOp(GitOp::Merge {
                rev: self.rev.clone(),
                options,
            }),
        ]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdentityField {
    Name,
    Email,
    Global,
}

impl IdentityField {
    pub fn next(self) -> Self {
        match self {
            Self::Name => Self::Email,
            Self::Email => Self::Global,
            Self::Global => Self::Name,
        }
    }

    pub fn prev(self) -> Self {
        match self {
            Self::Name => Self::Global,
            Self::Email => Self::Name,
            Self::Global => Self::Email,
        }
    }
}

/// Modal prompting for missing `user.name` and `user.email` before committing.
#[derive(Debug, Clone)]
pub struct IdentityDialog {
    pub name: String,
    pub email: String,
    pub global: bool,
    pub focused: IdentityField,
    pub pending_commit: Option<jbgit_engine::CommitRequest>,
}

impl IdentityDialog {
    pub fn new(
        name: Option<String>,
        email: Option<String>,
        pending_commit: Option<jbgit_engine::CommitRequest>,
    ) -> Self {
        let name_val = name.unwrap_or_default();
        let email_val = email.unwrap_or_default();
        let focused = if name_val.trim().is_empty() {
            IdentityField::Name
        } else {
            IdentityField::Email
        };
        Self {
            name: name_val,
            email: email_val,
            global: false,
            focused,
            pending_commit,
        }
    }

    pub fn error(&self, i18n: &I18n) -> Option<String> {
        if self.name.trim().is_empty() {
            Some(i18n.t("dialog.identity.err_name_empty").to_string())
        } else if self.email.trim().is_empty() {
            Some(i18n.t("dialog.identity.err_email_empty").to_string())
        } else {
            None
        }
    }

    pub fn can_confirm(&self, i18n: &I18n) -> bool {
        self.error(i18n).is_none()
    }

    pub fn confirm_actions(&self) -> Vec<Action> {
        vec![Action::SaveGitIdentityAndCommit {
            name: self.name.trim().to_string(),
            email: self.email.trim().to_string(),
            global: self.global,
            pending_commit: self.pending_commit.clone(),
        }]
    }
}

/// Cycles through `candidates` matching `prefix` (case-insensitive substring/prefix).
fn cycle_completion(candidates: &[String], current: &str, forward: bool) -> Option<String> {
    if candidates.is_empty() {
        return None;
    }
    let trimmed = current.trim();
    if let Some(exact_idx) = candidates.iter().position(|c| c == trimmed) {
        let len = candidates.len();
        let next = if forward {
            (exact_idx + 1) % len
        } else {
            (exact_idx + len - 1) % len
        };
        return Some(candidates[next].clone());
    }
    let lower = trimmed.to_ascii_lowercase();
    let matching: Vec<&String> = if lower.is_empty() {
        candidates.iter().collect()
    } else {
        let pref: Vec<&String> = candidates
            .iter()
            .filter(|c| c.to_ascii_lowercase().starts_with(&lower))
            .collect();
        if !pref.is_empty() {
            pref
        } else {
            candidates
                .iter()
                .filter(|c| c.to_ascii_lowercase().contains(&lower))
                .collect()
        }
    };
    if matching.is_empty() {
        return None;
    }
    Some(if forward {
        matching[0].clone()
    } else {
        (*matching.last().unwrap()).clone()
    })
}

/// Rows of the `Pull…` options dialog, top to bottom.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PullDialogRow {
    Remote,
    Branch,
    Rebase,
    FastForward,
    Squash,
    NoCommit,
    NoVerify,
}

impl PullDialogRow {
    pub const ALL: [Self; 7] = [
        Self::Remote,
        Self::Branch,
        Self::Rebase,
        Self::FastForward,
        Self::Squash,
        Self::NoCommit,
        Self::NoVerify,
    ];

    pub const fn mnemonic(self) -> char {
        match self {
            Self::Remote => 'r',
            Self::Branch => 'b',
            Self::Rebase => 'e',
            Self::FastForward => 'f',
            Self::Squash => 's',
            Self::NoCommit => 'n',
            Self::NoVerify => 'v',
        }
    }
}

/// `Pull…` options dialog (`GitPullDialog`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullDialog {
    pub current: String,
    pub remotes: Vec<String>,
    /// `(remote, branch_name_without_remote_prefix)` pairs.
    pub remote_branches: Vec<(String, String)>,
    pub options: PullOptions,
    pub selected: usize,
    pub fetching: bool,
}

impl PullDialog {
    pub fn from_snapshot(snap: &RepoSnapshot, remembered: PullOptions) -> Self {
        let current = snap
            .head_branch
            .clone()
            .unwrap_or_else(|| "HEAD".to_string());
        let remotes = snap.available_remotes();
        let remote_branches = Self::collect_remote_branches(snap);
        let upstream = snap
            .head_branch
            .as_deref()
            .and_then(|b| snap.local_branch(b))
            .and_then(|l| l.upstream.as_deref())
            .and_then(|u| snap.split_tracking(u));
        let default_remote = upstream
            .map(|(r, _)| r.to_string())
            .or_else(|| snap.default_remote())
            .or_else(|| remotes.first().cloned());
        let default_branch = upstream
            .map(|(_, b)| b.to_string())
            .or_else(|| {
                let rem = default_remote.as_deref()?;
                remote_branches
                    .iter()
                    .find(|(r, b)| r == rem && b == &current)
                    .map(|(_, b)| b.clone())
            });
        Self {
            current,
            remotes,
            remote_branches,
            options: PullOptions {
                remote: default_remote,
                branch: default_branch,
                ..remembered
            },
            selected: 0,
            fetching: false,
        }
    }

    fn collect_remote_branches(snap: &RepoSnapshot) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = snap
            .refs
            .iter()
            .filter(|r| r.kind == RefKind::RemoteBranch)
            .filter_map(|r| {
                snap.split_tracking(&r.name)
                    .map(|(rem, br)| (rem.to_string(), br.to_string()))
            })
            .collect();
        out.sort();
        out.dedup();
        out
    }

    pub fn refresh_from_snapshot(&mut self, snap: &RepoSnapshot) {
        self.fetching = false;
        self.remotes = snap.available_remotes();
        self.remote_branches = Self::collect_remote_branches(snap);
        if self.options.remote.is_none() {
            self.options.remote = snap.default_remote().or_else(|| self.remotes.first().cloned());
        }
    }

    pub fn row(&self) -> PullDialogRow {
        PullDialogRow::ALL[self.selected.min(PullDialogRow::ALL.len() - 1)]
    }

    pub fn editing_branch(&self) -> bool {
        self.row() == PullDialogRow::Branch
    }

    pub fn move_selection(&mut self, delta: isize) {
        let len = PullDialogRow::ALL.len() as isize;
        self.selected = (self.selected as isize + delta).rem_euclid(len) as usize;
    }

    pub fn branches_for_selected_remote(&self) -> Vec<String> {
        let Some(rem) = self.options.effective_remote() else {
            return Vec::new();
        };
        self.remote_branches
            .iter()
            .filter(|(r, _)| r == rem)
            .map(|(_, b)| b.clone())
            .collect()
    }

    pub fn branch_text(&self) -> &str {
        self.options.branch.as_deref().unwrap_or("")
    }

    pub fn push_branch(&mut self, text: &str) {
        self.options
            .branch
            .get_or_insert_with(String::new)
            .push_str(text);
    }

    pub fn pop_branch(&mut self) {
        if let Some(b) = self.options.branch.as_mut() {
            b.pop();
            if b.is_empty() {
                self.options.branch = None;
            }
        }
    }

    pub fn complete_branch(&mut self, forward: bool) {
        let candidates = self.branches_for_selected_remote();
        if let Some(next) = cycle_completion(&candidates, self.branch_text(), forward) {
            self.options.branch = Some(next);
        }
    }

    pub fn toggle(&mut self, row: PullDialogRow, forward: bool) {
        match row {
            PullDialogRow::Remote => {
                if self.remotes.is_empty() {
                    self.options.remote = None;
                    return;
                }
                let len = self.remotes.len();
                let cur = self
                    .options
                    .remote
                    .as_deref()
                    .and_then(|r| self.remotes.iter().position(|x| x == r));
                let next_idx = match cur {
                    Some(i) if forward => (i + 1) % len,
                    Some(i) => (i + len - 1) % len,
                    None => 0,
                };
                self.options.remote = Some(self.remotes[next_idx].clone());
                let valid = self.branches_for_selected_remote();
                if let Some(br) = self.options.branch.as_deref()
                    && !valid.iter().any(|v| v == br)
                {
                    self.options.branch = valid
                        .iter()
                        .find(|v| v.as_str() == self.current)
                        .cloned()
                        .or_else(|| valid.first().cloned());
                }
            }
            PullDialogRow::Branch => self.complete_branch(forward),
            PullDialogRow::Rebase => self.options.rebase = !self.options.rebase,
            PullDialogRow::FastForward => {
                self.options.ff = if forward {
                    self.options.ff.next()
                } else {
                    self.options.ff.prev()
                };
            }
            PullDialogRow::Squash => self.options.squash = !self.options.squash,
            PullDialogRow::NoCommit => self.options.no_commit = !self.options.no_commit,
            PullDialogRow::NoVerify => self.options.no_verify = !self.options.no_verify,
        }
    }

    pub fn error(&self, i18n: &I18n) -> Option<String> {
        if self.options.effective_remote().is_none() && self.options.effective_branch().is_some() {
            return Some(i18n.t("dialog.pull.err_branch_without_remote").into());
        }
        self.options.validate().err().map(|c| {
            let (a, b) = c.flags();
            i18n.t_fmt("dialog.pull.conflict", &[("a", a), ("b", b)])
        })
    }

    pub fn confirm_actions(&self) -> Vec<Action> {
        let options = PullOptions {
            remote: self.options.effective_remote().map(ToString::to_string),
            branch: self.options.effective_branch().map(ToString::to_string),
            ..self.options.clone()
        };
        vec![
            Action::RememberPullOptions(options.clone()),
            Action::RunGitOp(GitOp::Pull { options }),
        ]
    }
}

/// Rows of the standard `Rebase…` dialog, top to bottom.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RebaseDialogRow {
    Onto,
    Upstream,
    Branch,
    Interactive,
    RebaseMerges,
    UpdateRefs,
    KeepEmpty,
    Autosquash,
    Root,
}

impl RebaseDialogRow {
    pub const ALL: [Self; 9] = [
        Self::Onto,
        Self::Upstream,
        Self::Branch,
        Self::Interactive,
        Self::RebaseMerges,
        Self::UpdateRefs,
        Self::KeepEmpty,
        Self::Autosquash,
        Self::Root,
    ];

    pub const fn mnemonic(self) -> char {
        match self {
            Self::Onto => 'o',
            Self::Upstream => 'u',
            Self::Branch => 'b',
            Self::Interactive => 'i',
            Self::RebaseMerges => 'm',
            Self::UpdateRefs => 'r',
            Self::KeepEmpty => 'k',
            Self::Autosquash => 'a',
            Self::Root => 't',
        }
    }

    pub const fn is_text(self) -> bool {
        matches!(self, Self::Onto | Self::Upstream | Self::Branch)
    }
}

/// Standard `Rebase…` options dialog (`GitRebaseDialog`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RebaseDialog {
    pub current: String,
    pub ref_candidates: Vec<String>,
    pub local_branches: Vec<String>,
    pub git_version: Option<GitVersion>,
    pub options: RebaseOptions,
    pub selected: usize,
}

impl RebaseDialog {
    pub fn from_snapshot(
        snap: &RepoSnapshot,
        onto: Option<String>,
        git_version: Option<GitVersion>,
        remembered: RebaseOptions,
    ) -> Self {
        let current = snap
            .head_branch
            .clone()
            .unwrap_or_else(|| "HEAD".to_string());
        let mut local_branches: Vec<String> = snap
            .local_branch_names()
            .into_iter()
            .map(ToString::to_string)
            .collect();
        local_branches.sort();
        let mut ref_candidates = local_branches.clone();
        let mut remotes: Vec<String> = snap
            .refs
            .iter()
            .filter(|r| r.kind == RefKind::RemoteBranch)
            .map(|r| r.name.clone())
            .collect();
        remotes.sort();
        ref_candidates.extend(remotes);
        let mut tags: Vec<String> = snap
            .tag_names()
            .into_iter()
            .map(ToString::to_string)
            .collect();
        tags.sort();
        ref_candidates.extend(tags);
        ref_candidates.dedup();

        let mut options = remembered;
        options.onto = onto.unwrap_or_default();
        options.upstream = None;
        options.branch = None;
        Self {
            current,
            ref_candidates,
            local_branches,
            git_version,
            options,
            selected: 0,
        }
    }

    pub fn row(&self) -> RebaseDialogRow {
        RebaseDialogRow::ALL[self.selected.min(RebaseDialogRow::ALL.len() - 1)]
    }

    pub fn editing_text(&self) -> bool {
        self.row().is_text()
            && !(self.row() == RebaseDialogRow::Upstream
                && self.options.root
                && self.options.effective_upstream().is_none())
    }

    pub fn move_selection(&mut self, delta: isize) {
        let len = RebaseDialogRow::ALL.len() as isize;
        self.selected = (self.selected as isize + delta).rem_euclid(len) as usize;
    }

    pub fn push_text(&mut self, text: &str) {
        match self.row() {
            RebaseDialogRow::Onto => self.options.onto.push_str(text),
            RebaseDialogRow::Upstream if !self.options.root => {
                self.options
                    .upstream
                    .get_or_insert_with(String::new)
                    .push_str(text);
            }
            RebaseDialogRow::Branch => {
                self.options
                    .branch
                    .get_or_insert_with(String::new)
                    .push_str(text);
            }
            _ => {}
        }
    }

    pub fn pop_text(&mut self) {
        match self.row() {
            RebaseDialogRow::Onto => {
                self.options.onto.pop();
            }
            RebaseDialogRow::Upstream => {
                if let Some(u) = self.options.upstream.as_mut() {
                    u.pop();
                    if u.is_empty() {
                        self.options.upstream = None;
                    }
                }
            }
            RebaseDialogRow::Branch => {
                if let Some(b) = self.options.branch.as_mut() {
                    b.pop();
                    if b.is_empty() {
                        self.options.branch = None;
                    }
                }
            }
            _ => {}
        }
    }

    pub fn clear_text(&mut self) {
        match self.row() {
            RebaseDialogRow::Onto => self.options.onto.clear(),
            RebaseDialogRow::Upstream => self.options.upstream = None,
            RebaseDialogRow::Branch => self.options.branch = None,
            _ => {}
        }
    }

    pub fn complete_text(&mut self, forward: bool) {
        match self.row() {
            RebaseDialogRow::Onto => {
                if let Some(next) =
                    cycle_completion(&self.ref_candidates, &self.options.onto, forward)
                {
                    self.options.onto = next;
                }
            }
            RebaseDialogRow::Upstream if !self.options.root => {
                let cur = self.options.upstream.as_deref().unwrap_or("");
                if let Some(next) = cycle_completion(&self.ref_candidates, cur, forward) {
                    self.options.upstream = Some(next);
                }
            }
            RebaseDialogRow::Branch => {
                let cur = self.options.branch.as_deref().unwrap_or("");
                if let Some(next) = cycle_completion(&self.local_branches, cur, forward) {
                    self.options.branch = Some(next);
                }
            }
            _ => {}
        }
    }

    pub fn toggle(&mut self, row: RebaseDialogRow, forward: bool) {
        match row {
            RebaseDialogRow::Onto | RebaseDialogRow::Upstream | RebaseDialogRow::Branch => {
                self.complete_text(forward);
            }
            RebaseDialogRow::Interactive => self.options.interactive = !self.options.interactive,
            RebaseDialogRow::RebaseMerges => {
                self.options.rebase_merges = !self.options.rebase_merges;
            }
            RebaseDialogRow::UpdateRefs => self.options.update_refs = !self.options.update_refs,
            RebaseDialogRow::KeepEmpty => self.options.keep_empty = !self.options.keep_empty,
            RebaseDialogRow::Autosquash => self.options.autosquash = !self.options.autosquash,
            RebaseDialogRow::Root => self.options.root = !self.options.root,
        }
    }

    pub fn error(&self, i18n: &I18n) -> Option<String> {
        self.options.validate(self.git_version).err().map(|e| match e {
            RebaseOptionError::MissingOnto => i18n.t("dialog.rebase_opts.err_missing_onto").into(),
            RebaseOptionError::RootWithUpstream => {
                i18n.t("dialog.rebase_opts.err_root_upstream").into()
            }
            RebaseOptionError::UpdateRefsUnsupported(v) => i18n.t_fmt(
                "dialog.rebase_opts.err_update_refs_version",
                &[("version", &v.to_string())],
            ),
            RebaseOptionError::RebaseMergesUnsupported(v) => i18n.t_fmt(
                "dialog.rebase_opts.err_rebase_merges_version",
                &[("version", &v.to_string())],
            ),
        })
    }

    pub fn confirm_actions(&self) -> Vec<Action> {
        let options = RebaseOptions {
            onto: self.options.onto.trim().to_string(),
            upstream: self.options.effective_upstream().map(ToString::to_string),
            branch: self.options.effective_branch().map(ToString::to_string),
            ..self.options.clone()
        };
        if options.interactive
            && !options.root
            && !options.autosquash
            && options.effective_upstream().is_none()
            && options.effective_branch().is_none()
            && !options.onto.is_empty()
        {
            return vec![
                Action::RememberRebaseOptions(options.clone()),
                Action::OpenInteractiveRebaseFromDialog(options),
            ];
        }
        vec![
            Action::RememberRebaseOptions(options.clone()),
            Action::RunGitOp(GitOp::Rebase { options }),
        ]
    }
}

/// Rows of the unified `Create Patch…` dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CreatePatchRow {
    Destination,
    Path,
    Reverse,
}

impl CreatePatchRow {
    pub const ALL: [Self; 3] = [Self::Destination, Self::Path, Self::Reverse];
}

/// Unified `Create Patch…` configuration dialog (used by both Log and Stage menus).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreatePatchDialog {
    pub source: jbgit_core::PatchSource,
    /// `false` = save to file/directory, `true` = copy to clipboard.
    pub to_clipboard: bool,
    /// Target file/directory path (defaults to outside the worktree).
    pub path: String,
    /// Base directory for relative paths & `Tab` completion.
    pub base: std::path::PathBuf,
    /// Generate a reversed patch (`-R`).
    pub reverse: bool,
    pub selected: usize,
}

impl CreatePatchDialog {
    pub fn row(&self) -> CreatePatchRow {
        CreatePatchRow::ALL[self.selected.min(CreatePatchRow::ALL.len() - 1)]
    }

    pub fn editing_path(&self) -> bool {
        self.row() == CreatePatchRow::Path && !self.to_clipboard
    }

    pub fn move_selection(&mut self, delta: isize) {
        let len = CreatePatchRow::ALL.len() as isize;
        let mut next = (self.selected as isize + delta).rem_euclid(len) as usize;
        if self.to_clipboard && CreatePatchRow::ALL[next] == CreatePatchRow::Path {
            next = (next as isize + delta.signum()).rem_euclid(len) as usize;
        }
        self.selected = next;
    }

    pub fn toggle(&mut self, row: CreatePatchRow) {
        match row {
            CreatePatchRow::Destination => self.to_clipboard = !self.to_clipboard,
            CreatePatchRow::Reverse => self.reverse = !self.reverse,
            CreatePatchRow::Path => {}
        }
    }

    pub fn error(&self, i18n: &I18n) -> Option<String> {
        if !self.to_clipboard && self.path.trim().is_empty() {
            return Some(i18n.t("common.cannot_be_empty").into());
        }
        None
    }

    pub fn confirm_actions(&self) -> Vec<Action> {
        if self.to_clipboard {
            return vec![Action::RunGitOp(GitOp::PatchToClipboard {
                source: self.source.clone(),
                reverse: self.reverse,
            })];
        }
        let dest = resolve_path(&self.base, &self.path);
        let remember_dir = if dest.is_dir()
            || self.path.trim().ends_with('/')
            || self.path.trim().ends_with('\\')
        {
            dest.clone()
        } else {
            dest.parent()
                .map(std::path::Path::to_path_buf)
                .unwrap_or_else(|| self.base.clone())
        };
        vec![Action::ConfirmCreatePatchFile {
            source: self.source.clone(),
            output: dest,
            reverse: self.reverse,
            remember_dir,
        }]
    }
}

/// Builds the `Create Patch…` dialog defaulting to an external path outside the worktree.
pub fn create_patch_dialog(
    state: &crate::app::AppState,
    source: jbgit_core::PatchSource,
    to_clipboard: bool,
) -> Dialog {
    let workdir = state.current_workdir();
    let default_dir = state
        .last_patch_dir
        .clone()
        .or_else(|| workdir.parent().map(std::path::Path::to_path_buf))
        .unwrap_or_else(std::env::temp_dir);
    let repo_name = workdir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "repo".into());
    let default_path = default_dir.join(source.suggested_filename(&repo_name));
    Dialog::CreatePatch(CreatePatchDialog {
        source,
        to_clipboard,
        path: default_path.display().to_string(),
        base: default_dir,
        reverse: false,
        selected: if to_clipboard { 0 } else { 1 },
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NewBranchField {
    Name,
    Checkout,
    Overwrite,
    Track,
    AutoSanitize,
}

impl NewBranchField {
    pub const ALL: [Self; 5] = [
        Self::Name,
        Self::Checkout,
        Self::Overwrite,
        Self::Track,
        Self::AutoSanitize,
    ];

    pub const fn next(self) -> Self {
        match self {
            Self::Name => Self::Checkout,
            Self::Checkout => Self::Overwrite,
            Self::Overwrite => Self::Track,
            Self::Track => Self::AutoSanitize,
            Self::AutoSanitize => Self::Name,
        }
    }

    pub const fn prev(self) -> Self {
        match self {
            Self::Name => Self::AutoSanitize,
            Self::Checkout => Self::Name,
            Self::Overwrite => Self::Checkout,
            Self::Track => Self::Overwrite,
            Self::AutoSanitize => Self::Track,
        }
    }
}

/// Enhanced `New Branch…` dialog (`GitNewBranchDialog`): real-time space→`_`
/// sanitization (toggleable), Checkout branch, Overwrite existing (`-B`), and
/// Set tracking (`--track`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewBranchDialog {
    pub title: String,
    pub prompt: String,
    pub start: String,
    pub value: String,
    pub existing: Vec<String>,
    pub checkout: bool,
    pub overwrite: bool,
    pub track: bool,
    pub auto_sanitize: bool,
    pub focused: NewBranchField,
}

impl NewBranchDialog {
    pub fn new(
        title: impl Into<String>,
        prompt: impl Into<String>,
        start: impl Into<String>,
        default: impl Into<String>,
        existing: Vec<String>,
        track: bool,
    ) -> Self {
        Self {
            title: title.into(),
            prompt: prompt.into(),
            start: start.into(),
            value: default.into(),
            existing,
            checkout: true,
            overwrite: false,
            track,
            auto_sanitize: true,
            focused: NewBranchField::Name,
        }
    }

    pub fn push_str(&mut self, text: &str) {
        self.value.push_str(text);
        if self.auto_sanitize {
            self.value = sanitize_branch_name_on_typing(&self.value, "_");
        }
    }

    pub fn error(&self, i18n: &I18n) -> Option<String> {
        let refs: Vec<&str> = self.existing.iter().map(String::as_str).collect();
        match validate_ref_name_typed(&self.value, &refs) {
            Ok(()) => None,
            Err(RefNameError::AlreadyExists(_)) if self.overwrite => None,
            Err(_) => i18n.validate_ref_name(&self.value, &refs, "Branch").err(),
        }
    }

    pub fn can_confirm(&self, i18n: &I18n) -> bool {
        self.error(i18n).is_none()
    }

    pub fn confirm_actions(&self) -> Vec<Action> {
        let name = if self.auto_sanitize {
            sanitize_branch_name(&self.value, "_")
        } else {
            self.value.trim().to_string()
        };
        let op = if self.checkout {
            GitOp::CheckoutNewBranch {
                name,
                start: self.start.clone(),
                track: self.track,
                force: self.overwrite,
            }
        } else {
            GitOp::CreateBranch {
                name,
                start: self.start.clone(),
                force: self.overwrite,
                track: self.track,
            }
        };
        vec![Action::RunGitOp(op)]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StashPushField {
    Message,
    IncludeUntracked,
    KeepIndex,
    OnlySelected,
}

/// Lightweight `Stash Changes…` dialog (`GitStashDialog`): optional message,
/// `--include-untracked`, `--keep-index`, and selected-files vs all-changes toggle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StashPushDialog {
    pub message: String,
    pub include_untracked: bool,
    pub keep_index: bool,
    pub selected_paths: Vec<String>,
    pub only_selected: bool,
    pub focused: StashPushField,
}

impl StashPushDialog {
    pub fn new(
        default_message: impl Into<String>,
        include_untracked: bool,
        selected_paths: Vec<String>,
    ) -> Self {
        let only_selected = !selected_paths.is_empty();
        Self {
            message: default_message.into(),
            include_untracked,
            keep_index: false,
            selected_paths,
            only_selected,
            focused: StashPushField::Message,
        }
    }

    pub fn rows(&self) -> Vec<StashPushField> {
        let mut r = vec![
            StashPushField::Message,
            StashPushField::IncludeUntracked,
            StashPushField::KeepIndex,
        ];
        if !self.selected_paths.is_empty() {
            r.push(StashPushField::OnlySelected);
        }
        r
    }

    pub fn move_focus(&mut self, delta: isize) {
        let rows = self.rows();
        let cur = rows.iter().position(|f| *f == self.focused).unwrap_or(0) as isize;
        let len = rows.len() as isize;
        self.focused = rows[(cur + delta).rem_euclid(len) as usize];
    }

    pub fn preview_command(&self) -> String {
        let mut parts = vec!["git stash push".to_string()];
        if self.include_untracked {
            parts.push("--include-untracked".into());
        }
        if self.keep_index {
            parts.push("--keep-index".into());
        }
        let msg = self.message.trim();
        if !msg.is_empty() {
            parts.push(format!("-m \"{msg}\""));
        }
        if self.only_selected && !self.selected_paths.is_empty() {
            parts.push(format!("-- ({} file(s))", self.selected_paths.len()));
        }
        parts.join(" ")
    }

    pub fn confirm_actions(&self) -> Vec<Action> {
        let msg = self.message.trim();
        vec![Action::RunGitOp(GitOp::StashPush {
            message: (!msg.is_empty()).then(|| msg.to_string()),
            include_untracked: self.include_untracked,
            keep_index: self.keep_index,
            paths: if self.only_selected {
                self.selected_paths.clone()
            } else {
                Vec::new()
            },
        })]
    }
}

#[derive(Debug, Clone)]
pub enum Dialog {
    Input(InputDialog),
    NewBranch(NewBranchDialog),
    StashPush(StashPushDialog),
    Choice(ChoiceDialog),
    Message(MessageDialog),
    RebaseTodo(RebaseTodoDialog),
    Merge(MergeDialog),
    Pull(PullDialog),
    Rebase(RebaseDialog),
    CreatePatch(CreatePatchDialog),
    Credential(CredentialDialog),
    Identity(IdentityDialog),
}

impl Dialog {
    /// Actions emitted when the dialog is confirmed (`None` = cannot confirm).
    pub fn confirm_actions(&self, i18n: &I18n) -> Option<Vec<Action>> {
        match self {
            Self::Input(d) => d.can_confirm(i18n).then(|| d.confirm_actions(i18n)),
            Self::NewBranch(d) => d.can_confirm(i18n).then(|| d.confirm_actions()),
            Self::StashPush(d) => Some(d.confirm_actions()),
            Self::Choice(d) => {
                let opt = d.options.get(d.selected)?;
                if let Some(cb) = &d.checkbox
                    && cb.checked
                {
                    if let Some(alt) = &opt.checked_actions {
                        return Some(alt.clone());
                    }
                    if d.selected == 0 && !cb.checked_actions.is_empty() {
                        return Some(cb.checked_actions.clone());
                    }
                }
                Some(opt.actions.clone())
            }
            Self::Message(d) => Some(d.confirm_actions(i18n)),
            Self::RebaseTodo(d) => d.error(i18n).is_none().then(|| d.confirm_actions()),
            Self::Merge(d) => d.options.validate().is_ok().then(|| d.confirm_actions()),
            Self::Pull(d) => d.options.validate().is_ok().then(|| d.confirm_actions()),
            Self::Rebase(d) => d
                .options
                .validate(d.git_version)
                .is_ok()
                .then(|| d.confirm_actions()),
            Self::CreatePatch(d) => d.error(i18n).is_none().then(|| d.confirm_actions()),
            // Answered by `AppState::answer_credential` (the secret never
            // travels inside an `Action`).
            Self::Credential(_) => Some(Vec::new()),
            Self::Identity(d) => d.can_confirm(i18n).then(|| d.confirm_actions()),
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
        if let Dialog::RebaseTodo(d) = dialog
            && d.show_diff
        {
            d.show_diff = false;
            return DialogKey::Redraw;
        }
        return DialogKey::Cancel;
    }
    match dialog {
        Dialog::Credential(d) => match key.code {
            KeyCode::Enter => DialogKey::Confirm,
            KeyCode::Backspace => {
                d.value.pop();
                DialogKey::Redraw
            }
            KeyCode::Char('u') if ctrl => {
                d.clear();
                DialogKey::Redraw
            }
            KeyCode::Tab | KeyCode::BackTab if d.masked => {
                d.revealed = !d.revealed;
                DialogKey::Redraw
            }
            KeyCode::Char(c) if plain && !c.is_control() => {
                d.value.push(c);
                DialogKey::Redraw
            }
            _ => DialogKey::None,
        },
        Dialog::Identity(d) => match key.code {
            KeyCode::Enter => {
                if d.focused == IdentityField::Name && !d.name.trim().is_empty() && d.email.trim().is_empty() {
                    d.focused = IdentityField::Email;
                    DialogKey::Redraw
                } else {
                    DialogKey::Confirm
                }
            }
            KeyCode::Tab | KeyCode::Down => {
                d.focused = d.focused.next();
                DialogKey::Redraw
            }
            KeyCode::BackTab | KeyCode::Up => {
                d.focused = d.focused.prev();
                DialogKey::Redraw
            }
            KeyCode::Char(' ') if d.focused == IdentityField::Global => {
                d.global = !d.global;
                DialogKey::Redraw
            }
            KeyCode::Backspace => {
                match d.focused {
                    IdentityField::Name => {
                        d.name.pop();
                    }
                    IdentityField::Email => {
                        d.email.pop();
                    }
                    IdentityField::Global => {}
                }
                DialogKey::Redraw
            }
            KeyCode::Char('u') if ctrl => {
                match d.focused {
                    IdentityField::Name => d.name.clear(),
                    IdentityField::Email => d.email.clear(),
                    IdentityField::Global => {}
                }
                DialogKey::Redraw
            }
            KeyCode::Char(c) if plain && !c.is_control() => {
                match d.focused {
                    IdentityField::Name => d.name.push(c),
                    IdentityField::Email => d.email.push(c),
                    IdentityField::Global => {}
                }
                DialogKey::Redraw
            }
            _ => DialogKey::None,
        },
        Dialog::NewBranch(d) => match key.code {
            KeyCode::Enter => DialogKey::Confirm,
            KeyCode::Tab | KeyCode::Down => {
                d.focused = d.focused.next();
                DialogKey::Redraw
            }
            KeyCode::BackTab | KeyCode::Up => {
                d.focused = d.focused.prev();
                DialogKey::Redraw
            }
            KeyCode::Char('c') if alt => {
                d.checkout = !d.checkout;
                DialogKey::Redraw
            }
            KeyCode::Char('o') if alt => {
                d.overwrite = !d.overwrite;
                DialogKey::Redraw
            }
            KeyCode::Char('t') if alt => {
                d.track = !d.track;
                DialogKey::Redraw
            }
            KeyCode::Char('s') if alt => {
                d.auto_sanitize = !d.auto_sanitize;
                DialogKey::Redraw
            }
            KeyCode::Char(' ') if plain && d.focused != NewBranchField::Name => {
                match d.focused {
                    NewBranchField::Checkout => d.checkout = !d.checkout,
                    NewBranchField::Overwrite => d.overwrite = !d.overwrite,
                    NewBranchField::Track => d.track = !d.track,
                    NewBranchField::AutoSanitize => d.auto_sanitize = !d.auto_sanitize,
                    NewBranchField::Name => {}
                }
                DialogKey::Redraw
            }
            KeyCode::Backspace if d.focused == NewBranchField::Name => {
                d.value.pop();
                DialogKey::Redraw
            }
            KeyCode::Char('u') if ctrl && d.focused == NewBranchField::Name => {
                d.value.clear();
                DialogKey::Redraw
            }
            KeyCode::Char(c) if plain && !c.is_control() && d.focused == NewBranchField::Name => {
                d.push_str(&c.to_string());
                DialogKey::Redraw
            }
            _ => DialogKey::None,
        },
        Dialog::StashPush(d) => match key.code {
            KeyCode::Enter => DialogKey::Confirm,
            KeyCode::Tab | KeyCode::Down => {
                d.move_focus(1);
                DialogKey::Redraw
            }
            KeyCode::BackTab | KeyCode::Up => {
                d.move_focus(-1);
                DialogKey::Redraw
            }
            KeyCode::Char('u') if alt => {
                d.include_untracked = !d.include_untracked;
                DialogKey::Redraw
            }
            KeyCode::Char('k') if alt => {
                d.keep_index = !d.keep_index;
                DialogKey::Redraw
            }
            KeyCode::Char('s') if alt && !d.selected_paths.is_empty() => {
                d.only_selected = !d.only_selected;
                DialogKey::Redraw
            }
            KeyCode::Char(' ') if plain && d.focused != StashPushField::Message => {
                match d.focused {
                    StashPushField::IncludeUntracked => d.include_untracked = !d.include_untracked,
                    StashPushField::KeepIndex => d.keep_index = !d.keep_index,
                    StashPushField::OnlySelected => d.only_selected = !d.only_selected,
                    StashPushField::Message => {}
                }
                DialogKey::Redraw
            }
            KeyCode::Backspace if d.focused == StashPushField::Message => {
                d.message.pop();
                DialogKey::Redraw
            }
            KeyCode::Char('u') if ctrl && d.focused == StashPushField::Message => {
                d.message.clear();
                DialogKey::Redraw
            }
            KeyCode::Char(c) if plain && !c.is_control() && d.focused == StashPushField::Message => {
                d.message.push(c);
                DialogKey::Redraw
            }
            _ => DialogKey::None,
        },
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
            KeyCode::Char('s') if ctrl || alt => {
                if let Some((_, on)) = d.secondary_checkbox.as_mut() {
                    *on = !*on;
                }
                DialogKey::Redraw
            }
            KeyCode::Tab
                if matches!(d.validation, InputValidation::ExistingFile { .. })
                    || matches!(d.intent, InputIntent::ExportTreeBlob { .. }) =>
            {
                let base = match (&d.validation, &d.intent) {
                    (InputValidation::ExistingFile { base }, _) => Some(base),
                    (_, InputIntent::ExportTreeBlob { base, .. }) => Some(base),
                    _ => None,
                };
                if let Some(base) = base
                    && let Some(next) = crate::patch_view::complete_path(base, &d.value)
                {
                    d.value = next;
                }
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
                KeyCode::Char(' ') if plain && d.checkbox.is_some() => {
                    if let Some(cb) = d.checkbox.as_mut() {
                        cb.checked = !cb.checked;
                    }
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
        Dialog::Merge(d) => {
            let editing = d.editing_message();
            match key.code {
                KeyCode::Enter if editing && alt => {
                    d.push_message("\n");
                    DialogKey::Redraw
                }
                KeyCode::Enter => DialogKey::Confirm,
                KeyCode::Up | KeyCode::BackTab => {
                    d.move_selection(-1);
                    DialogKey::Redraw
                }
                KeyCode::Down | KeyCode::Tab => {
                    d.move_selection(1);
                    DialogKey::Redraw
                }
                KeyCode::Backspace if editing => {
                    d.pop_message();
                    DialogKey::Redraw
                }
                KeyCode::Char('u') if editing && ctrl => {
                    d.options.message = None;
                    DialogKey::Redraw
                }
                KeyCode::Char(c) if editing && plain && !c.is_control() => {
                    d.push_message(&c.to_string());
                    DialogKey::Redraw
                }
                KeyCode::Left if !editing => {
                    d.toggle(d.row(), false);
                    DialogKey::Redraw
                }
                KeyCode::Right if !editing => {
                    d.toggle(d.row(), true);
                    DialogKey::Redraw
                }
                KeyCode::Char(' ') if !editing => {
                    d.toggle(d.row(), true);
                    DialogKey::Redraw
                }
                KeyCode::Char(c) if plain => {
                    let c = c.to_ascii_lowercase();
                    match MergeDialogRow::ALL
                        .iter()
                        .position(|r| r.mnemonic() == c)
                    {
                        Some(idx) => {
                            d.selected = idx;
                            let row = d.row();
                            if row != MergeDialogRow::Message {
                                d.toggle(row, true);
                            }
                            DialogKey::Redraw
                        }
                        None => DialogKey::None,
                    }
                }
                _ => DialogKey::None,
            }
        }
        Dialog::Pull(d) => {
            let editing = d.editing_branch();
            match key.code {
                KeyCode::Enter => DialogKey::Confirm,
                KeyCode::F(5) => DialogKey::Actions(vec![Action::PullDialogFetch]),
                KeyCode::Char('r') if ctrl => DialogKey::Actions(vec![Action::PullDialogFetch]),
                KeyCode::Up | KeyCode::BackTab => {
                    d.move_selection(-1);
                    DialogKey::Redraw
                }
                KeyCode::Down | KeyCode::Tab => {
                    d.move_selection(1);
                    DialogKey::Redraw
                }
                KeyCode::Left => {
                    d.toggle(d.row(), false);
                    DialogKey::Redraw
                }
                KeyCode::Right => {
                    d.toggle(d.row(), true);
                    DialogKey::Redraw
                }
                KeyCode::Backspace if editing => {
                    d.pop_branch();
                    DialogKey::Redraw
                }
                KeyCode::Char('u') if editing && ctrl => {
                    d.options.branch = None;
                    DialogKey::Redraw
                }
                KeyCode::Char(c) if editing && plain && !c.is_control() => {
                    d.push_branch(&c.to_string());
                    DialogKey::Redraw
                }
                KeyCode::Char(' ') if !editing => {
                    d.toggle(d.row(), true);
                    DialogKey::Redraw
                }
                KeyCode::Char(c) if plain => {
                    let c = c.to_ascii_lowercase();
                    match PullDialogRow::ALL
                        .iter()
                        .position(|r| r.mnemonic() == c)
                    {
                        Some(idx) => {
                            d.selected = idx;
                            let row = d.row();
                            if row != PullDialogRow::Branch {
                                d.toggle(row, true);
                            }
                            DialogKey::Redraw
                        }
                        None => DialogKey::None,
                    }
                }
                _ => DialogKey::None,
            }
        }
        Dialog::Rebase(d) => {
            let editing = d.editing_text();
            match key.code {
                KeyCode::Enter => DialogKey::Confirm,
                KeyCode::Up | KeyCode::BackTab => {
                    d.move_selection(-1);
                    DialogKey::Redraw
                }
                KeyCode::Down | KeyCode::Tab => {
                    d.move_selection(1);
                    DialogKey::Redraw
                }
                KeyCode::Left => {
                    d.toggle(d.row(), false);
                    DialogKey::Redraw
                }
                KeyCode::Right => {
                    d.toggle(d.row(), true);
                    DialogKey::Redraw
                }
                KeyCode::Backspace if editing => {
                    d.pop_text();
                    DialogKey::Redraw
                }
                KeyCode::Char('u') if editing && ctrl => {
                    d.clear_text();
                    DialogKey::Redraw
                }
                KeyCode::Char(c) if editing && plain && !c.is_control() => {
                    d.push_text(&c.to_string());
                    DialogKey::Redraw
                }
                KeyCode::Char(' ') if !editing => {
                    if !d.row().is_text() {
                        d.toggle(d.row(), true);
                    }
                    DialogKey::Redraw
                }
                KeyCode::Char(c) if plain && !editing => {
                    let c = c.to_ascii_lowercase();
                    match RebaseDialogRow::ALL
                        .iter()
                        .position(|r| r.mnemonic() == c)
                    {
                        Some(idx) => {
                            d.selected = idx;
                            let row = d.row();
                            if !row.is_text() {
                                d.toggle(row, true);
                            }
                            DialogKey::Redraw
                        }
                        None => DialogKey::None,
                    }
                }
                _ => DialogKey::None,
            }
        }
        Dialog::CreatePatch(d) => {
            let editing = d.editing_path();
            match key.code {
                KeyCode::Enter => DialogKey::Confirm,
                KeyCode::Up | KeyCode::BackTab => {
                    d.move_selection(-1);
                    DialogKey::Redraw
                }
                KeyCode::Down => {
                    d.move_selection(1);
                    DialogKey::Redraw
                }
                KeyCode::Tab if editing => {
                    if let Some(next) = crate::patch_view::complete_path(&d.base, &d.path) {
                        d.path = next;
                    } else {
                        d.move_selection(1);
                    }
                    DialogKey::Redraw
                }
                KeyCode::Tab => {
                    d.move_selection(1);
                    DialogKey::Redraw
                }
                KeyCode::Backspace if editing => {
                    d.path.pop();
                    DialogKey::Redraw
                }
                KeyCode::Char('u') if editing && ctrl => {
                    d.path.clear();
                    DialogKey::Redraw
                }
                KeyCode::Char(c) if editing && plain && !c.is_control() => {
                    d.path.push(c);
                    DialogKey::Redraw
                }
                KeyCode::Left | KeyCode::Right | KeyCode::Char(' ') if !editing => {
                    d.toggle(d.row());
                    DialogKey::Redraw
                }
                KeyCode::Char('f') if !editing && plain => {
                    d.to_clipboard = false;
                    DialogKey::Redraw
                }
                KeyCode::Char('c') if !editing && plain => {
                    d.to_clipboard = true;
                    DialogKey::Redraw
                }
                KeyCode::Char('r') if !editing && plain => {
                    d.reverse = !d.reverse;
                    DialogKey::Redraw
                }
                _ => DialogKey::None,
            }
        }
        Dialog::RebaseTodo(d) if d.show_diff => match key.code {
            KeyCode::Char('V') => {
                d.no_verify = !d.no_verify;
                DialogKey::Redraw
            }
            KeyCode::Char('d') if plain || ctrl => {
                d.show_diff = false;
                DialogKey::Redraw
            }
            KeyCode::Up | KeyCode::Char('k') if plain => {
                d.diff_scroll.scroll_by(-1, usize::MAX);
                DialogKey::Redraw
            }
            KeyCode::Down | KeyCode::Char('j') if plain => {
                d.diff_scroll.scroll_by(1, usize::MAX);
                DialogKey::Redraw
            }
            KeyCode::PageUp => {
                let step = d.diff_scroll.page_step(10);
                d.diff_scroll.scroll_by(-step, usize::MAX);
                DialogKey::Redraw
            }
            KeyCode::PageDown => {
                let step = d.diff_scroll.page_step(10);
                d.diff_scroll.scroll_by(step, usize::MAX);
                DialogKey::Redraw
            }
            KeyCode::Left | KeyCode::Char('[') if plain => {
                d.selected_file = d.selected_file.saturating_sub(1);
                d.diff_scroll.reset();
                d.diff_requested = None;
                DialogKey::Redraw
            }
            KeyCode::Right | KeyCode::Char(']') if plain => {
                d.selected_file = d.selected_file.saturating_add(1);
                d.diff_scroll.reset();
                d.diff_requested = None;
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
                d.select_entry(d.selected.saturating_sub(1));
                DialogKey::Redraw
            }
            KeyCode::Down | KeyCode::Char('j') => {
                d.select_entry(d.selected.saturating_add(1));
                DialogKey::Redraw
            }
            KeyCode::Char('[') if plain => {
                d.selected_file = d.selected_file.saturating_sub(1);
                DialogKey::Redraw
            }
            KeyCode::Char(']') if plain => {
                d.selected_file = d.selected_file.saturating_add(1);
                DialogKey::Redraw
            }
            KeyCode::Char('V') => {
                d.no_verify = !d.no_verify;
                DialogKey::Redraw
            }
            KeyCode::Char('d') if plain || ctrl => {
                if d.selected_commit_oid().is_some() {
                    d.show_diff = true;
                    d.diff_scroll.reset();
                    d.diff_requested = None;
                    DialogKey::Redraw
                } else {
                    DialogKey::None
                }
            }
            KeyCode::Delete | KeyCode::Backspace => {
                if let Some(e) = d.entries.get_mut(d.selected)
                    && e.action.is_editable()
                {
                    e.action = TodoAction::Drop;
                }
                DialogKey::Redraw
            }
            KeyCode::Char(' ') => {
                if let Some(e) = d.entries.get_mut(d.selected)
                    && e.action.is_editable()
                {
                    e.action = e.action.next();
                }
                DialogKey::Redraw
            }
            KeyCode::Char(c) if plain => match TodoAction::from_key(c) {
                Some(TodoAction::Reword) => {
                    let idx = d.selected;
                    if let Some(e) = d.entries.get_mut(idx)
                        && e.action.is_editable()
                    {
                        e.action = TodoAction::Reword;
                        let text = e
                            .message
                            .clone()
                            .or_else(|| d.messages.get(idx).cloned())
                            .unwrap_or_default();
                        return DialogKey::Actions(vec![Action::OpenDialog(Box::new(
                            Dialog::Message(MessageDialog::new(
                                i18n.t_fmt(
                                    "dialog.rebase.reword_entry_title",
                                    &[("subject", &e.subject)],
                                ),
                                text,
                                MessageIntent::TodoEntry { idx },
                            )),
                        ))]);
                    }
                    DialogKey::None
                }
                Some(action) => {
                    if let Some(e) = d.entries.get_mut(d.selected)
                        && e.action.is_editable()
                    {
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

/// `New Branch…` dialog with real-time space→`_` sanitization, overwrite (`-B`),
/// and tracking (`--track`) options (`GitNewBranchDialog`).
pub fn new_branch_dialog(
    start: &str,
    start_label: &str,
    existing: Vec<String>,
    default: &str,
    i18n: &I18n,
) -> Dialog {
    new_branch_dialog_with_track(start, start_label, existing, default, false, i18n)
}

pub fn new_branch_dialog_with_track(
    start: &str,
    start_label: &str,
    existing: Vec<String>,
    default: &str,
    track: bool,
    i18n: &I18n,
) -> Dialog {
    Dialog::NewBranch(NewBranchDialog::new(
        i18n.t_fmt("dialog.new_branch.title_from", &[("start", start_label)]),
        i18n.t("dialog.new_branch.name_prompt"),
        start,
        default,
        existing,
        track,
    ))
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
