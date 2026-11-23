use std::fs;
use std::path::Path;
use jbgit_core::{
    BranchCategory, DiffContext, GitOp, OngoingKind, OngoingStep, PaneId, RefNameError, ResetMode,
    RewritePlanError, StageNodeKind, TodoAction, validate_ref_name_typed,
};
use rustc_hash::FxHashMap;

use crate::graph::GraphRendererMode;
use crate::log_view::{DiffPreviewDock, FilterField, format_absolute_time};

pub const EMBEDDED_ZH_CN: &str = include_str!("locales/zh-CN.toml");
pub const EMBEDDED_EN_US: &str = include_str!("locales/en-US.toml");

#[derive(Debug, Clone)]
pub struct I18n {
    pub lang: String,
    messages: FxHashMap<String, String>,
    fallback: FxHashMap<String, String>,
}

impl Default for I18n {
    fn default() -> Self {
        Self::load("zh-CN", None)
    }
}

impl I18n {
    /// Loads the i18n manager based on language preference ("zh-CN", "en-US", or "auto")
    /// and optional user custom locales directory.
    pub fn load(lang_pref: &str, custom_locales_dir: Option<&Path>) -> Self {
        let resolved_lang = if lang_pref.eq_ignore_ascii_case("auto") {
            detect_system_language()
        } else {
            normalize_lang_code(lang_pref)
        };

        // Fallback is always en-US
        let fallback = parse_and_flatten_toml(EMBEDDED_EN_US);

        let mut messages = if resolved_lang == "zh-CN" {
            parse_and_flatten_toml(EMBEDDED_ZH_CN)
        } else {
            parse_and_flatten_toml(EMBEDDED_EN_US)
        };

        // Check user custom locales directory: <config_dir>/locales/<lang>.toml
        if let Some(dir) = custom_locales_dir {
            let custom_file = dir.join(format!("{resolved_lang}.toml"));
            if custom_file.is_file()
                && let Ok(content) = fs::read_to_string(&custom_file)
            {
                let custom_map = parse_and_flatten_toml(&content);
                messages.extend(custom_map);
            }
        }

        Self {
            lang: resolved_lang,
            messages,
            fallback,
        }
    }

    /// Looks up a translation by dot-separated key, falling back to en-US or the key itself.
    pub fn t<'a>(&'a self, key: &'a str) -> &'a str {
        if let Some(msg) = self.messages.get(key) {
            return msg.as_str();
        }
        if let Some(fallback_msg) = self.fallback.get(key) {
            return fallback_msg.as_str();
        }
        key
    }

    /// Looks up a translation and replaces `{var}` placeholders with provided argument values.
    pub fn t_fmt(&self, key: &str, args: &[(&str, &str)]) -> String {
        let mut template = self.t(key).to_string();
        for (k, v) in args {
            let needle = format!("{{{k}}}");
            template = template.replace(&needle, v);
        }
        template
    }

    // Common UI convenience accessors
    pub fn ok(&self) -> &str {
        self.t("common.ok")
    }

    pub fn cancel(&self) -> &str {
        self.t("common.cancel")
    }

    pub fn confirm(&self) -> &str {
        self.t("common.confirm")
    }

    pub fn close(&self) -> &str {
        self.t("common.close")
    }

    pub fn yes(&self) -> &str {
        self.t("common.yes")
    }

    pub fn no(&self) -> &str {
        self.t("common.no")
    }

    pub fn on_off(&self, on: bool) -> &str {
        if on {
            self.t("common.on")
        } else {
            self.t("common.off")
        }
    }

    pub fn pane_title(&self, pane: PaneId) -> &str {
        match pane {
            PaneId::CommitChangesTree => self.t("pane.commit_changes"),
            PaneId::CommitMessageEditor => self.t("pane.commit_message"),
            PaneId::CommitDiffPreview => self.t("pane.commit_diff"),
            PaneId::LogBranchesTree => self.t("pane.log_branches"),
            PaneId::LogCommitTable => self.t("pane.log_commits"),
            PaneId::LogChangesTree => self.t("pane.log_changes"),
            PaneId::LogDetailsDiff => self.t("pane.log_details"),
            PaneId::StashList => self.t("pane.stash_list"),
            PaneId::StashDiffPreview => self.t("pane.stash_diff"),
            PaneId::ConsoleCommandList => self.t("pane.console_commands"),
            PaneId::ConsoleOutputDetail => self.t("pane.console_output"),
        }
    }

    pub fn stage_group_title(&self, kind: StageNodeKind) -> &str {
        match kind {
            StageNodeKind::Conflicted => self.t("view.commit.group.conflicted"),
            StageNodeKind::Staged => self.t("view.commit.group.staged"),
            StageNodeKind::Unstaged => self.t("view.commit.group.unstaged"),
            StageNodeKind::Untracked => self.t("view.commit.group.untracked"),
            StageNodeKind::Ignored => self.t("view.commit.group.ignored"),
            StageNodeKind::AmendCommit => self.t("view.commit.amend_head"),
        }
    }

    pub fn branch_category_title(&self, cat: BranchCategory) -> &str {
        match cat {
            BranchCategory::Local => self.t("view.branches.category.local"),
            BranchCategory::Remote => self.t("view.branches.category.remote"),
            BranchCategory::Tags => self.t("view.branches.category.tags"),
        }
    }

    pub fn ongoing_title(&self, kind: OngoingKind) -> &str {
        match kind {
            OngoingKind::Rebase => self.t("ongoing.rebase"),
            OngoingKind::Merge => self.t("ongoing.merge"),
            OngoingKind::CherryPick => self.t("ongoing.cherry_pick"),
            OngoingKind::Revert => self.t("ongoing.revert"),
        }
    }

    pub fn reset_mode_label(&self, mode: ResetMode) -> &str {
        match mode {
            ResetMode::Soft => self.t("dialog.reset.mode.soft.label"),
            ResetMode::Mixed => self.t("dialog.reset.mode.mixed.label"),
            ResetMode::Hard => self.t("dialog.reset.mode.hard.label"),
            ResetMode::Keep => self.t("dialog.reset.mode.keep.label"),
        }
    }

    pub fn reset_mode_desc(&self, mode: ResetMode) -> &str {
        match mode {
            ResetMode::Soft => self.t("dialog.reset.mode.soft.desc"),
            ResetMode::Mixed => self.t("dialog.reset.mode.mixed.desc"),
            ResetMode::Hard => self.t("dialog.reset.mode.hard.desc"),
            ResetMode::Keep => self.t("dialog.reset.mode.keep.desc"),
        }
    }

    pub fn todo_action_label(&self, action: TodoAction) -> &str {
        match action {
            TodoAction::Pick => self.t("dialog.rebase.action.pick"),
            TodoAction::Reword => self.t("dialog.rebase.action.reword"),
            TodoAction::Squash => self.t("dialog.rebase.action.squash"),
            TodoAction::Fixup => self.t("dialog.rebase.action.fixup"),
            TodoAction::Drop => self.t("dialog.rebase.action.drop"),
        }
    }

    pub fn diff_context_label(&self, ctx: DiffContext) -> &str {
        match ctx {
            DiffContext::Three => "3",
            DiffContext::Ten => "10",
            DiffContext::Full => self.t("diff.context.full"),
        }
    }

    pub fn diff_dock_label(&self, dock: DiffPreviewDock) -> &str {
        match dock {
            DiffPreviewDock::Details => self.t("diff.dock.details"),
            DiffPreviewDock::Right => self.t("diff.dock.right"),
            DiffPreviewDock::Bottom => self.t("diff.dock.bottom"),
        }
    }

    pub fn graph_renderer_label(&self, mode: GraphRendererMode) -> &str {
        match mode {
            GraphRendererMode::Auto => self.t("presentation.renderer_auto"),
            GraphRendererMode::Vector => self.t("presentation.renderer_vector"),
            GraphRendererMode::CharacterGrid => self.t("presentation.renderer_cell"),
        }
    }

    pub fn filter_field_title(&self, field: FilterField) -> &str {
        match field {
            FilterField::Branch => self.t("filter.branch.title"),
            FilterField::User => self.t("filter.user.title"),
            FilterField::Date => self.t("filter.date.title"),
            FilterField::Paths => self.t("filter.paths.title"),
        }
    }

    pub fn filter_field_hint(&self, field: FilterField) -> &str {
        match field {
            FilterField::Branch => self.t("filter.branch.hint"),
            FilterField::User => self.t("filter.user.hint"),
            FilterField::Date => self.t("filter.date.hint"),
            FilterField::Paths => self.t("filter.paths.hint"),
        }
    }

    pub fn format_date_filter_label(&self, raw: &str) -> String {
        match raw {
            "Today" => self.t("filter.date_today").to_string(),
            "Yesterday" => self.t("filter.date_yesterday").to_string(),
            "Last 7 Days" => self.t("filter.date_last_7").to_string(),
            "Last 30 Days" => self.t("filter.date_last_30").to_string(),
            other => {
                if let Some(n) = other
                    .strip_prefix("Last ")
                    .and_then(|s| s.strip_suffix(" Days"))
                {
                    self.t_fmt("filter.date_last_n", &[("n", n)])
                } else {
                    other.to_string()
                }
            }
        }
    }

    pub fn format_relative_time(&self, ts: i64, now: i64) -> String {
        let delta = now - ts;
        if delta < 0 {
            return format_absolute_time(ts, false);
        }
        match delta {
            0..60 => self.t("time.just_now").to_string(),
            60..3_600 => self.t_fmt("time.minutes_ago", &[("n", &(delta / 60).to_string())]),
            3_600..86_400 => self.t_fmt("time.hours_ago", &[("n", &(delta / 3_600).to_string())]),
            86_400..172_800 => self.t("time.yesterday").to_string(),
            172_800..604_800 => {
                self.t_fmt("time.days_ago", &[("n", &(delta / 86_400).to_string())])
            }
            _ => format_absolute_time(ts, false),
        }
    }

    pub fn validate_ref_name(&self, name: &str, existing: &[&str], what_key: &str) -> Result<(), String> {
        let what = match what_key {
            "Branch" | "branch" | "validation.what.branch" => self.t("validation.what.branch"),
            "Tag" | "tag" | "validation.what.tag" => self.t("validation.what.tag"),
            other => other,
        };
        let what_lower = what.to_ascii_lowercase();
        validate_ref_name_typed(name, existing).map_err(|err| match err {
            RefNameError::Empty => self.t_fmt("validation.empty", &[("what", what)]),
            RefNameError::StartsWithDash => {
                self.t_fmt("validation.starts_with_dash", &[("what", what)])
            }
            RefNameError::ContainsSpaces => self.t_fmt("validation.spaces", &[("what", what)]),
            RefNameError::ContainsDoubleDot => {
                self.t_fmt("validation.double_dot", &[("what", what)])
            }
            RefNameError::ContainsAtBrace => self.t_fmt("validation.at_brace", &[("what", what)]),
            RefNameError::InvalidChar(ch) => {
                let c_s = ch.to_string();
                self.t_fmt("validation.bad_char", &[("what", what), ("ch", &c_s)])
            }
            RefNameError::EmptyPathComponent => {
                self.t_fmt("validation.empty_component", &[("what", what)])
            }
            RefNameError::EndsWithDot => self.t_fmt("validation.ends_with_dot", &[("what", what)]),
            RefNameError::ComponentStartsWithDot(comp) => {
                self.t_fmt("validation.comp_dot", &[("comp", &comp)])
            }
            RefNameError::ComponentEndsWithLock(comp) => {
                self.t_fmt("validation.comp_lock", &[("comp", &comp)])
            }
            RefNameError::ReservedHead => {
                self.t_fmt("validation.head_reserved", &[("what", &what_lower)])
            }
            RefNameError::AlreadyExists(n) => {
                self.t_fmt("validation.exists", &[("what", what), ("name", &n)])
            }
            RefNameError::ConflictsWithExisting(conflict) => {
                self.t_fmt("validation.conflicts", &[("what", what), ("conflict", &conflict)])
            }
        })
    }

    pub fn rewrite_plan_error(&self, err: &RewritePlanError, verb_key: &str) -> String {
        let verb = match verb_key {
            "reword" => self.t("rewrite.verb.reword"),
            "fixup" => self.t("rewrite.verb.fixup"),
            "rebase" => self.t("rewrite.verb.rebase"),
            "squash" => self.t("rewrite.verb.squash"),
            "drop" => self.t("rewrite.verb.drop"),
            other => other,
        };
        match err {
            RewritePlanError::NoCommits => self.t("rewrite.err_no_commits").to_string(),
            RewritePlanError::NotReachable => {
                self.t_fmt("rewrite.err_not_reachable", &[("verb", verb)])
            }
            RewritePlanError::ContainsMergeCommits => {
                self.t_fmt("rewrite.err_merge_commits", &[("verb", verb)])
            }
            RewritePlanError::ProtectedBranch(branch) => {
                self.t_fmt("rewrite.err_protected", &[("verb", verb), ("branch", branch)])
            }
        }
    }

    pub fn git_op_label(&self, op: &GitOp) -> String {
        match op {
            GitOp::Checkout { target, .. } => {
                self.t_fmt("git_op.checkout", &[("target", target)])
            }
            GitOp::ForceCheckout { target } => {
                self.t_fmt("git_op.force_checkout", &[("target", target)])
            }
            GitOp::CheckoutNewBranch { name, .. } => {
                self.t_fmt("git_op.checkout_new_branch", &[("name", name)])
            }
            GitOp::CheckoutResetBranch { name, .. } => {
                self.t_fmt("git_op.checkout_reset_branch", &[("name", name)])
            }
            GitOp::CreateBranch { name, .. } => {
                self.t_fmt("git_op.create_branch", &[("name", name)])
            }
            GitOp::RenameBranch { old, new } => {
                self.t_fmt("git_op.rename_branch", &[("old", old), ("new", new)])
            }
            GitOp::DeleteBranch { name, .. } => {
                self.t_fmt("git_op.delete_branch", &[("name", name)])
            }
            GitOp::RestoreBranch { name, .. } => {
                self.t_fmt("git_op.restore_branch", &[("name", name)])
            }
            GitOp::DeleteRemoteBranch { remote, branch } => self.t_fmt(
                "git_op.delete_remote_branch",
                &[("remote", remote), ("branch", branch)],
            ),
            GitOp::CreateTag { name, .. } => self.t_fmt("git_op.create_tag", &[("name", name)]),
            GitOp::DeleteTag { name, .. } => self.t_fmt("git_op.delete_tag", &[("name", name)]),
            GitOp::RestoreTag { name, .. } => self.t_fmt("git_op.restore_tag", &[("name", name)]),
            GitOp::DeleteRemoteTag { remote, name } => self.t_fmt(
                "git_op.delete_remote_tag",
                &[("remote", remote), ("name", name)],
            ),
            GitOp::PushTag { remote, name, .. } => self.t_fmt(
                "git_op.push_tag",
                &[("remote", remote), ("name", name)],
            ),
            GitOp::Merge { rev } => self.t_fmt("git_op.merge", &[("rev", rev)]),
            GitOp::Rebase { onto, branch } => match branch {
                Some(b) => self.t_fmt("git_op.rebase_branch", &[("branch", b), ("onto", onto)]),
                None => self.t_fmt("git_op.rebase_onto", &[("onto", onto)]),
            },
            GitOp::CherryPick { oids } => {
                let c = oids.len().to_string();
                self.t_fmt("git_op.cherry_pick", &[("count", &c)])
            }
            GitOp::Revert { oids } => {
                let c = oids.len().to_string();
                self.t_fmt("git_op.revert", &[("count", &c)])
            }
            GitOp::Reset { mode, target } => {
                let mode_lbl = self.reset_mode_label(*mode);
                let short = target.short();
                self.t_fmt("git_op.reset", &[("mode", mode_lbl), ("short", &short)])
            }
            GitOp::Fetch => self.t("git_op.fetch").to_string(),
            GitOp::Pull { rebase } => {
                if *rebase {
                    self.t("git_op.pull_rebase").to_string()
                } else {
                    self.t("git_op.pull").to_string()
                }
            }
            GitOp::Push {
                branch,
                remote,
                remote_branch,
                ..
            } => {
                let target = if remote_branch.is_empty() {
                    branch.as_str()
                } else {
                    remote_branch.as_str()
                };
                self.t_fmt(
                    "git_op.push_target",
                    &[("branch", branch), ("remote", remote), ("target", target)],
                )
            }
            GitOp::Sequence(ops) => ops
                .iter()
                .map(|inner| self.git_op_label(inner))
                .collect::<Vec<_>>()
                .join(" → "),
            GitOp::UpdateBranch { branch, .. } => {
                self.t_fmt("git_op.update_branch", &[("branch", branch)])
            }
            GitOp::RewordHead { .. } => self.t("git_op.reword_head").to_string(),
            GitOp::RewriteHistory { todo, .. } => {
                let c = todo.len().to_string();
                self.t_fmt("git_op.rewrite_history", &[("count", &c)])
            }
            GitOp::FixupCommit { target, .. } => {
                let short = target.short();
                self.t_fmt("git_op.fixup_commit", &[("short", &short)])
            }
            GitOp::Ongoing { kind, step } => {
                let k = self.ongoing_title(*kind);
                let key = match step {
                    OngoingStep::Continue => "git_op.ongoing_continue",
                    OngoingStep::Skip => "git_op.ongoing_skip",
                    OngoingStep::Abort => "git_op.ongoing_abort",
                };
                self.t_fmt(key, &[("kind", k)])
            }
            GitOp::StashPush { .. } => self.t("git_op.stash_push").to_string(),
            GitOp::StashApply { index, pop } => {
                let idx_s = index.to_string();
                self.t_fmt(
                    if *pop {
                        "git_op.stash_pop"
                    } else {
                        "git_op.stash_apply"
                    },
                    &[("index", &idx_s)],
                )
            }
            GitOp::StashDrop { index } => {
                let idx_s = index.to_string();
                self.t_fmt("git_op.stash_drop", &[("index", &idx_s)])
            }
            GitOp::StashClear => self.t("git_op.stash_clear").to_string(),
            GitOp::StashBranch { name, .. } => self.t_fmt("git_op.stash_branch", &[("name", name)]),
            GitOp::AcceptSide { ours, .. } => {
                if *ours {
                    self.t("git_op.accept_yours").to_string()
                } else {
                    self.t("git_op.accept_theirs").to_string()
                }
            }
            GitOp::IntentToAdd { .. } => self.t("git_op.intent_to_add").to_string(),
            GitOp::AddToIgnore { .. } => self.t("git_op.add_to_ignore").to_string(),
            GitOp::DeleteFiles { paths } => {
                let c = paths.len().to_string();
                self.t_fmt("git_op.delete_files", &[("count", &c)])
            }
            GitOp::CreatePatchFiles { .. } => self.t("git_op.create_patch").to_string(),
            GitOp::PatchToClipboard { .. } => self.t("git_op.patch_to_clipboard").to_string(),
            GitOp::CheckoutPaths { rev, paths } => {
                let short_rev = jbgit_core::short_rev(rev);
                if paths.len() == 1 {
                    self.t_fmt(
                        "git_op.checkout_paths",
                        &[("path", &paths[0]), ("rev", short_rev)],
                    )
                } else {
                    let count = paths.len().to_string();
                    self.t_fmt(
                        "git_op.checkout_paths_many",
                        &[("count", &count), ("rev", short_rev)],
                    )
                }
            }
            GitOp::Smart(inner) => {
                let inner_lbl = self.git_op_label(inner);
                self.t_fmt("git_op.smart", &[("label", &inner_lbl)])
            }
        }
    }

    pub fn load_for_cli() -> Self {
        Self::load(&detect_system_language(), None)
    }
}

fn normalize_lang_code(code: &str) -> String {
    let trimmed = code.trim();
    let lower = trimmed.to_ascii_lowercase();
    let base = lower.split(['.', '@']).next().unwrap_or("");
    if lower.starts_with("zh") {
        "zh-CN".to_string()
    } else if lower.starts_with("en") || base == "c" || base == "posix" {
        "en-US".to_string()
    } else {
        trimmed.to_string()
    }
}

#[cfg(windows)]
fn windows_ui_language() -> Option<String> {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetUserDefaultUILanguage() -> u16;
    }
    // SAFETY: GetUserDefaultUILanguage takes no arguments and returns a LANGID u16.
    let lang_id = unsafe { GetUserDefaultUILanguage() };
    if lang_id == 0 {
        return None;
    }
    // Primary language ID is in the low 10 bits; LANG_CHINESE = 0x04.
    let primary = lang_id & 0x03FF;
    if primary == 0x04 {
        Some("zh-CN".to_string())
    } else {
        Some("en-US".to_string())
    }
}

fn detect_system_language() -> String {
    for var in ["LC_ALL", "LC_MESSAGES", "LANG"] {
        if let Ok(val) = std::env::var(var) {
            let trimmed = val.trim();
            if !trimmed.is_empty() {
                return normalize_lang_code(trimmed);
            }
        }
    }

    #[cfg(windows)]
    {
        if let Some(lang) = windows_ui_language() {
            return lang;
        }
    }

    "en-US".to_string()
}

fn parse_and_flatten_toml(content: &str) -> FxHashMap<String, String> {
    let mut map = FxHashMap::default();
    if let Ok(toml::Value::Table(table)) = toml::from_str::<toml::Value>(content) {
        flatten_table("", &table, &mut map);
    }
    map
}

fn flatten_table(prefix: &str, table: &toml::map::Map<String, toml::Value>, out: &mut FxHashMap<String, String>) {
    for (k, v) in table {
        let full_key = if prefix.is_empty() {
            k.clone()
        } else {
            format!("{prefix}.{k}")
        };
        match v {
            toml::Value::String(s) => {
                out.insert(full_key, s.clone());
            }
            toml::Value::Table(inner) => {
                flatten_table(&full_key, inner, out);
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_i18n_zh_cn() {
        let i18n = I18n::load("zh-CN", None);
        assert_eq!(i18n.t("common.ok"), "确定");
        assert_eq!(i18n.t("common.cancel"), "取消");
        assert_eq!(i18n.t("menu.branch.checkout"), "检出");
        let formatted = i18n.t_fmt("menu.branch.already_checked_out", &[("name", "main")]);
        assert_eq!(formatted, "'main' 当前已检出");
    }

    #[test]
    fn test_i18n_en_us() {
        let i18n = I18n::load("en-US", None);
        assert_eq!(i18n.t("common.ok"), "OK");
        assert_eq!(i18n.t("common.cancel"), "Cancel");
        assert_eq!(i18n.t("menu.branch.checkout"), "Checkout");
        let formatted = i18n.t_fmt("menu.branch.already_checked_out", &[("name", "main")]);
        assert_eq!(formatted, "'main' is already checked out");
    }

    #[test]
    fn test_i18n_fallback() {
        let i18n = I18n::load("zh-CN", None);
        // Non-existent key returns key itself
        assert_eq!(i18n.t("non.existent.key"), "non.existent.key");
    }

    #[test]
    fn test_custom_locale_override() {
        let temp = tempfile::tempdir().unwrap();
        let locales_dir = temp.path().join("locales");
        std::fs::create_dir_all(&locales_dir).unwrap();

        let custom_toml = r#"
[common]
ok = "好嘞"
custom_extra = "自定义内容"
"#;
        std::fs::write(locales_dir.join("zh-CN.toml"), custom_toml).unwrap();

        let i18n = I18n::load("zh-CN", Some(&locales_dir));
        assert_eq!(i18n.t("common.ok"), "好嘞"); // Overridden
        assert_eq!(i18n.t("common.cancel"), "取消"); // Kept from default
        assert_eq!(i18n.t("common.custom_extra"), "自定义内容"); // Added
    }

    #[test]
    fn test_zh_cn_branch_menu_generation() {
        let mut state = crate::app::AppState::new(".".into(), None);
        state.i18n = std::sync::Arc::new(I18n::load("zh-CN", None));
        let oid = jbgit_core::Oid::from_bytes([0; 20]);
        let snap = jbgit_core::RepoSnapshot {
            head_branch: Some("main".into()),
            refs: vec![
                jbgit_core::RefInfo {
                    name: "main".into(),
                    full_name: "refs/heads/main".into(),
                    kind: jbgit_core::RefKind::LocalBranch,
                    is_head: true,
                    target_oid: oid,
                    upstream: None,
                    ahead_behind: Some((0, 0)),
                },
                jbgit_core::RefInfo {
                    name: "特性/分支-1".into(),
                    full_name: "refs/heads/特性/分支-1".into(),
                    kind: jbgit_core::RefKind::LocalBranch,
                    is_head: false,
                    target_oid: oid,
                    upstream: None,
                    ahead_behind: Some((0, 0)),
                },
            ],
            ..Default::default()
        };
        state.repo_snapshot = Some(std::sync::Arc::new(snap));

        let items = crate::menus::branch_menu_items(&state, "特性/分支-1", jbgit_core::RefKind::LocalBranch);
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();

        assert!(labels.contains(&"检出"));
        assert!(labels.iter().any(|l| l.contains("特性/分支-1")));
        assert!(labels.iter().any(|l| l.contains("与 'main' 对比")));
        assert!(labels.contains(&"与工作区对比差异"));
        assert!(labels.contains(&"删除"));
    }

    #[test]
    fn test_locale_key_parity() {
        let en = parse_and_flatten_toml(EMBEDDED_EN_US);
        let zh = parse_and_flatten_toml(EMBEDDED_ZH_CN);
        assert!(!en.is_empty());
        let missing_in_zh: Vec<&String> = en.keys().filter(|k| !zh.contains_key(*k)).collect();
        let extra_in_zh: Vec<&String> = zh.keys().filter(|k| !en.contains_key(*k)).collect();
        assert!(
            missing_in_zh.is_empty(),
            "Keys missing in zh-CN.toml: {missing_in_zh:?}"
        );
        assert!(
            extra_in_zh.is_empty(),
            "Keys missing in en-US.toml: {extra_in_zh:?}"
        );
    }

    #[test]
    fn test_zh_cn_ui_helpers_and_ops() {
        let zh = I18n::load("zh-CN", None);
        assert_eq!(zh.pane_title(PaneId::LogBranchesTree), "分支");
        assert_eq!(zh.stage_group_title(StageNodeKind::Conflicted), "合并冲突");
        assert_eq!(zh.stage_group_title(StageNodeKind::Staged), "已暂存");
        assert_eq!(zh.diff_context_label(DiffContext::Full), "全量");
        assert_eq!(zh.diff_dock_label(DiffPreviewDock::Bottom), "底部栏");
        assert_eq!(zh.format_relative_time(1000, 1010), "刚刚");
        assert_eq!(zh.format_relative_time(1000, 1000 + 5 * 60), "5分钟前");
        assert_eq!(
            zh.git_op_label(&GitOp::Checkout {
                target: "main".into(),
                detach: false,
            }),
            "检出 'main'"
        );
        assert_eq!(
            zh.git_op_label(&GitOp::Pull { rebase: true }),
            "拉取更新 (变基)"
        );
        assert_eq!(
            zh.rewrite_plan_error(&RewritePlanError::NoCommits, "reword"),
            "暂无任何提交"
        );
        assert_eq!(
            zh.validate_ref_name("HEAD", &[], "branch"),
            Err("'HEAD' 不能用作分支名称".into())
        );
    }

    #[test]
    fn test_auto_locale_returns_valid_lang() {
        let detected = detect_system_language();
        assert!(detected == "zh-CN" || detected == "en-US", "unexpected {detected}");
    }

    #[test]
    fn test_normalize_lang_code_handles_posix_and_utf8_variants() {
        assert_eq!(normalize_lang_code("C"), "en-US");
        assert_eq!(normalize_lang_code("C.UTF-8"), "en-US");
        assert_eq!(normalize_lang_code("c.utf8"), "en-US");
        assert_eq!(normalize_lang_code("POSIX"), "en-US");
        assert_eq!(normalize_lang_code("POSIX.UTF-8"), "en-US");
        assert_eq!(normalize_lang_code("en_US.UTF-8"), "en-US");
        assert_eq!(normalize_lang_code("zh_CN.UTF-8"), "zh-CN");
        assert_eq!(normalize_lang_code("ja-JP"), "ja-JP");
    }

    #[test]
    fn test_zh_cn_stage_menu_and_banner_and_cjk_paste() {
        use crossterm::event::Event;

        let mut cfg = crate::config::AppConfig::default();
        cfg.jbgit.general.language = crate::config::LanguagePref::ZhCn;
        cfg.jbgit.general.default_tab = crate::config::DefaultTab::Branches;
        cfg.jbgit.graph.backend = crate::config::GraphBackendPref::Cell;
        cfg.jbgit.diff.context_lines = 10;
        let mut state = crate::app::AppState::new_with_config(".".into(), None, cfg);

        assert_eq!(state.active_tab, jbgit_core::WorkspaceTab::Log);
        assert_eq!(state.focused_pane, jbgit_core::PaneId::LogBranchesTree);
        assert_eq!(
            state.log.graph_renderer.mode(),
            crate::graph::GraphRendererMode::CharacterGrid
        );
        assert_eq!(state.log.details.diff.context, jbgit_core::DiffContext::Ten);

        let stage_items = crate::menus::stage_tree_menu_items(&state, 0);
        assert!(
            stage_items
                .iter()
                .any(|i| i.label == "贮藏选中文件…" || i.label == "删除文件…")
        );

        // CJK paste into InputDialog
        state.modal_stack.push(crate::app::ModalLayer::Dialog(Box::new(
            crate::dialogs::new_branch_from_head_dialog(None, &state.i18n),
        )));
        for a in crate::input::route_terminal_event(&Event::Paste("特性/中文分支\n".into()), &state) {
            state.dispatch(a);
        }
        if let Some(crate::app::ModalLayer::Dialog(d)) = state.modal_stack.last() {
            if let crate::dialogs::Dialog::Input(inp) = d.as_ref() {
                assert_eq!(inp.value, "特性/中文分支");
            } else {
                panic!("expected InputDialog");
            }
        }
        state.modal_stack.pop();

        // CJK paste into GoToRevision floating popup
        state.modal_stack.push(crate::app::ModalLayer::GoToRevision {
            query: String::new(),
        });
        for a in crate::input::route_terminal_event(&Event::Paste("标签-发布".into()), &state) {
            state.dispatch(a);
        }
        if let Some(crate::app::ModalLayer::GoToRevision { query }) = state.modal_stack.last() {
            assert_eq!(query, "标签-发布");
        }
    }

    #[test]
    fn test_locale_placeholder_parity() {
        fn extract_placeholders(s: &str) -> std::collections::BTreeSet<String> {
            let mut set = std::collections::BTreeSet::new();
            let bytes = s.as_bytes();
            let mut i = 0;
            while i < bytes.len() {
                if bytes[i] == b'{'
                    && let Some(rel_end) = bytes[i + 1..].iter().position(|&b| b == b'}')
                {
                    let inner = &s[i + 1..i + 1 + rel_end];
                    if !inner.is_empty()
                        && inner.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                    {
                        set.insert(inner.to_string());
                    }
                    i += rel_end + 2;
                    continue;
                }
                i += 1;
            }
            set
        }

        let en = parse_and_flatten_toml(EMBEDDED_EN_US);
        let zh = parse_and_flatten_toml(EMBEDDED_ZH_CN);
        for (k, en_val) in &en {
            let zh_val = zh.get(k).unwrap();
            let en_ph = extract_placeholders(en_val);
            let zh_ph = extract_placeholders(zh_val);
            assert_eq!(
                en_ph, zh_ph,
                "placeholder mismatch for key '{k}': en={en_ph:?}, zh={zh_ph:?}"
            );
        }
    }
}
