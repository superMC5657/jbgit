use std::fs;
use std::path::Path;
use rustc_hash::FxHashMap;

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
}

fn normalize_lang_code(code: &str) -> String {
    let trimmed = code.trim();
    let lower = trimmed.to_ascii_lowercase();
    if lower.starts_with("zh") {
        "zh-CN".to_string()
    } else if lower.starts_with("en") || lower == "c" || lower == "posix" {
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
        assert_eq!(i18n.t("menu.branch.checkout"), "检出 (Checkout)");
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
        let oid = jgit_core::Oid::from_bytes([0; 20]);
        let snap = jgit_core::RepoSnapshot {
            head_branch: Some("main".into()),
            refs: vec![
                jgit_core::RefInfo {
                    name: "main".into(),
                    full_name: "refs/heads/main".into(),
                    kind: jgit_core::RefKind::LocalBranch,
                    is_head: true,
                    target_oid: oid,
                    upstream: None,
                    ahead_behind: Some((0, 0)),
                },
                jgit_core::RefInfo {
                    name: "特性/分支-1".into(),
                    full_name: "refs/heads/特性/分支-1".into(),
                    kind: jgit_core::RefKind::LocalBranch,
                    is_head: false,
                    target_oid: oid,
                    upstream: None,
                    ahead_behind: Some((0, 0)),
                },
            ],
            ..Default::default()
        };
        state.repo_snapshot = Some(std::sync::Arc::new(snap));

        let items = crate::menus::branch_menu_items(&state, "特性/分支-1", jgit_core::RefKind::LocalBranch);
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();

        assert!(labels.contains(&"检出 (Checkout)"));
        assert!(labels.iter().any(|l| l.contains("特性/分支-1")));
        assert!(labels.iter().any(|l| l.contains("与 'main' 对比")));
        assert!(labels.contains(&"与工作区对比差异"));
        assert!(labels.contains(&"删除 (Delete)"));
    }

    #[test]
    fn test_auto_locale_returns_valid_lang() {
        let detected = detect_system_language();
        assert!(detected == "zh-CN" || detected == "en-US", "unexpected {detected}");
    }

    #[test]
    fn test_zh_cn_stage_menu_and_banner_and_cjk_paste() {
        use crossterm::event::Event;

        let mut cfg = crate::config::AppConfig::default();
        cfg.jgit.general.language = crate::config::LanguagePref::ZhCn;
        cfg.jgit.general.default_tab = crate::config::DefaultTab::Branches;
        cfg.jgit.graph.backend = crate::config::GraphBackendPref::Cell;
        cfg.jgit.diff.context_lines = 10;
        let mut state = crate::app::AppState::new_with_config(".".into(), None, cfg);

        assert_eq!(state.active_tab, jgit_core::WorkspaceTab::Log);
        assert_eq!(state.focused_pane, jgit_core::PaneId::LogBranchesTree);
        assert_eq!(
            state.graph_renderer.mode(),
            crate::graph::GraphRendererMode::CharacterGrid
        );
        assert_eq!(state.details.diff_context, jgit_core::DiffContext::Ten);

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
        crate::input::route_terminal_event(Event::Paste("特性/中文分支\n".into()), &mut state);
        if let Some(crate::app::ModalLayer::Dialog(d)) = state.modal_stack.last() {
            if let crate::dialogs::Dialog::Input(inp) = d.as_ref() {
                assert_eq!(inp.value, "特性/中文分支");
            } else {
                panic!("expected InputDialog");
            }
        }
        state.modal_stack.pop();

        // CJK paste into GoToRevision floating popup
        state.modal_stack.push(crate::app::ModalLayer::FloatingPopup {
            kind: crate::app::PopupKind::GoToRevision,
            query: String::new(),
            selected: 0,
        });
        crate::input::route_terminal_event(Event::Paste("标签-发布".into()), &mut state);
        if let Some(crate::app::ModalLayer::FloatingPopup { query, .. }) = state.modal_stack.last()
        {
            assert_eq!(query, "标签-发布");
        }
    }
}
