pub mod discovery;
pub mod jbgit_config;
pub mod keymap_config;
pub mod theme_config;

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::de::DeserializeOwned;

pub use discovery::{JBGIT_CONFIG_DIR_ENV, find_config_file, global_config_dir};
pub use jbgit_config::{
    DefaultTab, GlyphModePref, GraphBackendPref, JbgitConfig, JbgitConfigOverlay, LanguagePref,
    LayoutSection, PairRatio, ThemeFlavor, TripleRatio,
};
pub use keymap_config::{KeyBindingConfig, KeymapConfig, KeymapConfigOverlay, KeymapPreset, KeymapScope};
pub use theme_config::{ThemeConfig, ThemeConfigOverlay};

pub const DEFAULT_JBGIT_TOML: &str = include_str!("defaults/jbgit.default.toml");
pub const DEFAULT_THEME_TOML: &str = include_str!("defaults/theme.default.toml");
pub const DEFAULT_KEYMAP_TOML: &str = include_str!("defaults/keymap.default.toml");

/// Root configuration aggregating all three sub-configurations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppConfig {
    pub jbgit: JbgitConfig,
    pub theme: ThemeConfig,
    pub keymap: KeymapConfig,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self::load_defaults()
    }
}

#[derive(Default)]
struct OverlayFlags {
    jbgit_flavor_set: bool,
    theme_flavor_set: bool,
    jbgit_glyph_set: bool,
    theme_glyph_set: bool,
}

fn load_toml_overlay<T: DeserializeOwned>(path: &Path, apply: impl FnOnce(&T)) {
    if path.is_file()
        && let Ok(content) = fs::read_to_string(path)
        && let Ok(overlay) = toml::from_str::<T>(&content)
    {
        apply(&overlay);
    }
}

impl AppConfig {
    /// Parses the compiled-in default configurations.
    pub fn load_defaults() -> Self {
        let jbgit: JbgitConfig = toml::from_str(DEFAULT_JBGIT_TOML).unwrap_or_default();
        let theme: ThemeConfig = toml::from_str(DEFAULT_THEME_TOML).unwrap_or_default();
        let keymap = KeymapConfig::for_preset(KeymapPreset::Default);
        Self { jbgit, theme, keymap }
    }

    /// Loads the configuration with layered precedence:
    /// Default -> Global User Config -> Repository Config -> JBGIT_CONFIG_DIR
    pub fn load(workdir: Option<&Path>) -> Self {
        let mut config = Self::load_defaults();

        // Layer 1: Global user configuration
        let global_dir = global_config_dir();
        config.apply_dir_overlays(&global_dir, false);

        // Layer 2: Repo-level configuration (.jbgit/)
        if let Some(wd) = workdir {
            let repo_jbgit_dir = wd.join(".jbgit");
            config.apply_dir_overlays(&repo_jbgit_dir, true);
        }

        // Layer 3: JBGIT_CONFIG_DIR environment variable (highest override)
        if let Some(env_dir) = std::env::var_os(JBGIT_CONFIG_DIR_ENV) {
            config.apply_dir_overlays(Path::new(&env_dir), false);
        }

        config
    }

    fn apply_dir_overlays(&mut self, dir: &Path, allow_config_toml_alias: bool) {
        let mut flags = OverlayFlags::default();
        if allow_config_toml_alias {
            load_toml_overlay::<JbgitConfigOverlay>(&dir.join("config.toml"), |ov| {
                if ov.general.as_ref().and_then(|g| g.flavor).is_some() {
                    flags.jbgit_flavor_set = true;
                }
                if ov.graph.as_ref().and_then(|g| g.glyph_mode).is_some() {
                    flags.jbgit_glyph_set = true;
                }
                self.jbgit.merge_overlay(ov);
            });
        }
        load_toml_overlay::<JbgitConfigOverlay>(&dir.join("jbgit.toml"), |ov| {
            if ov.general.as_ref().and_then(|g| g.flavor).is_some() {
                flags.jbgit_flavor_set = true;
            }
            if ov.graph.as_ref().and_then(|g| g.glyph_mode).is_some() {
                flags.jbgit_glyph_set = true;
            }
            self.jbgit.merge_overlay(ov);
        });
        load_toml_overlay::<ThemeConfigOverlay>(&dir.join("theme.toml"), |ov| {
            if ov.flavor.is_some() {
                flags.theme_flavor_set = true;
            }
            if ov.glyphs.as_ref().and_then(|g| g.mode).is_some() {
                flags.theme_glyph_set = true;
            }
            self.theme.merge_overlay(ov);
        });
        load_toml_overlay::<KeymapConfigOverlay>(&dir.join("keymap.toml"), |ov| {
            self.keymap.merge_overlay(ov);
        });

        // Honor jbgit.toml `[general].flavor` / `[graph].glyph_mode` when theme.toml in this layer did not override them.
        if flags.jbgit_flavor_set && !flags.theme_flavor_set {
            self.theme.flavor = self.jbgit.general.flavor;
        }
        if flags.jbgit_glyph_set && !flags.theme_glyph_set {
            self.theme.glyphs.mode = self.jbgit.graph.glyph_mode;
        }
    }

    /// Prints the requested default configuration to standard output.
    pub fn print_default_config(which: Option<&str>) {
        match which {
            Some("theme") | Some("theme.toml") => {
                print!("{DEFAULT_THEME_TOML}");
            }
            Some("keymap") | Some("keymap.toml") => {
                print!("{DEFAULT_KEYMAP_TOML}");
            }
            _ => {
                print!("{DEFAULT_JBGIT_TOML}");
            }
        }
    }

    /// Initializes default configuration files in the destination directory,
    /// returning `(dir, [(filename, created)])` without writing to stdout.
    pub fn init_config_detailed(
        custom_dir: Option<&Path>,
        force: bool,
    ) -> Result<(PathBuf, Vec<(&'static str, bool)>)> {
        let dir = match custom_dir {
            Some(d) => d.to_path_buf(),
            None => global_config_dir(),
        };

        fs::create_dir_all(&dir)
            .with_context(|| format!("Failed to create config directory at {:?}", dir))?;

        let files = [
            ("jbgit.toml", DEFAULT_JBGIT_TOML),
            ("theme.toml", DEFAULT_THEME_TOML),
            ("keymap.toml", DEFAULT_KEYMAP_TOML),
        ];

        let mut report = Vec::with_capacity(files.len());
        for (name, content) in files {
            let file_path = dir.join(name);
            if file_path.exists() && !force {
                report.push((name, false));
            } else {
                fs::write(&file_path, content)
                    .with_context(|| format!("Failed to write {:?}", file_path))?;
                report.push((name, true));
            }
        }

        Ok((dir, report))
    }

    /// Initializes default configuration files in the destination directory.
    pub fn init_config(custom_dir: Option<&Path>, force: bool) -> Result<PathBuf> {
        let (dir, _) = Self::init_config_detailed(custom_dir, force)?;
        Ok(dir)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_load_defaults() {
        let cfg = AppConfig::load_defaults();
        assert_eq!(cfg.jbgit.general.language, LanguagePref::ZhCn);
        assert_eq!(cfg.jbgit.general.default_tab, DefaultTab::Log);
        assert_eq!(cfg.theme.flavor, ThemeFlavor::Darcula);
        assert_eq!(cfg.theme.glyphs.mode, GlyphModePref::NerdFont);
        assert!(cfg.theme.palette.bg_canvas.is_none());
        assert!(!cfg.keymap.global.key_bindings.is_empty());
        assert!(!cfg.keymap.diff.key_bindings.is_empty());
    }

    #[test]
    fn test_layered_merge() {
        let mut cfg = AppConfig::load_defaults();
        let overlay_toml = r#"
[general]
language = "en-US"
default_tab = "commit"

[diff]
tab_size = 2
"#;
        let overlay: JbgitConfigOverlay = toml::from_str(overlay_toml).unwrap();
        cfg.jbgit.merge_overlay(&overlay);

        assert_eq!(cfg.jbgit.general.language, LanguagePref::EnUs);
        assert_eq!(cfg.jbgit.general.default_tab, DefaultTab::Commit);
        assert_eq!(cfg.jbgit.diff.tab_size, 2);
        // Unspecified keys retain defaults
        assert_eq!(cfg.jbgit.diff.context_lines, 3);
        assert!(cfg.jbgit.diff.syntax_highlight);

        let perf_toml = r#"
[performance]
large_index_entries = 5000
worktree_renames = "never"
recurse_untracked_dirs = "auto"
load_commit_bodies = false
"#;
        let perf_ov: JbgitConfigOverlay = toml::from_str(perf_toml).unwrap();
        cfg.jbgit.merge_overlay(&perf_ov);
        let engine_cfg = cfg.jbgit.performance.to_engine_config();
        assert_eq!(engine_cfg.large_index_entries, 5000);
        assert_eq!(engine_cfg.worktree_renames, jbgit_engine::StatusScanMode::Never);
        assert_eq!(
            engine_cfg.recurse_untracked_dirs,
            jbgit_engine::StatusScanMode::Auto
        );
        assert!(!engine_cfg.load_commit_bodies);
    }

    #[test]
    fn test_init_config_creates_files() {
        let temp = tempfile::tempdir().unwrap();
        let target_dir = temp.path().join("my_config");

        let res = AppConfig::init_config(Some(&target_dir), false).unwrap();
        assert_eq!(res, target_dir);

        assert!(target_dir.join("jbgit.toml").is_file());
        assert!(target_dir.join("theme.toml").is_file());
        assert!(target_dir.join("keymap.toml").is_file());

        let jbgit_content = fs::read_to_string(target_dir.join("jbgit.toml")).unwrap();
        assert!(jbgit_content.contains("language = \"zh-CN\""));
    }

    #[test]
    fn test_theme_and_keymap_overlays() {
        let mut cfg = AppConfig::load_defaults();
        let theme_str = r##"
flavor = "catppuccin-mocha"

[palette]
bg_canvas = "#000000"
"##;
        let theme_ov: ThemeConfigOverlay = toml::from_str(theme_str).unwrap();
        cfg.theme.merge_overlay(&theme_ov);
        assert_eq!(cfg.theme.flavor, ThemeFlavor::CatppuccinMocha);
        assert_eq!(cfg.theme.palette.bg_canvas.as_deref(), Some("#000000"));
        let palette = crate::theme::ThemePalette::from_config_with_truecolor(&cfg.theme, true);
        assert_eq!(palette.bg_canvas, ratatui::style::Color::Rgb(0, 0, 0));
        // Non-overridden Catppuccin palette colors must NOT be overwritten by Darcula defaults
        assert_eq!(
            palette.border_focused,
            ratatui::style::Color::Rgb(0x89, 0xB4, 0xFA)
        );

        let initial_global_len = cfg.keymap.global.key_bindings.len();
        let keymap_str = r#"
[global]
key_bindings = [
    { on = ["<F5>"], run = "refresh", desc = "Refresh" }
]
"#;
        let keymap_ov: KeymapConfigOverlay = toml::from_str(keymap_str).unwrap();
        cfg.keymap.merge_overlay(&keymap_ov);
        // Preserves existing bindings while replacing `refresh` (`r` + `<F5>` -> `<F5>`)
        assert_eq!(cfg.keymap.global.key_bindings.len(), initial_global_len - 1);
        assert!(
            cfg.keymap
                .global
                .key_bindings
                .iter()
                .any(|b| b.run == crate::keymap::KeyAction::Quit)
        );
        assert!(
            !cfg.keymap
                .global
                .key_bindings
                .iter()
                .any(|b| b.run == crate::keymap::KeyAction::Refresh && b.on == ["r"])
        );
        assert!(
            cfg.keymap
                .global
                .key_bindings
                .iter()
                .any(|b| b.run == crate::keymap::KeyAction::Refresh && b.on == ["<F5>"])
        );
    }

    #[test]
    fn test_repo_config_toml_alias_and_flavor_fallback() {
        let temp = tempfile::tempdir().unwrap();
        let jbgit_dir = temp.path().join(".jbgit");
        fs::create_dir_all(&jbgit_dir).unwrap();
        fs::write(
            jbgit_dir.join("config.toml"),
            r#"
[general]
flavor = "gruvbox-dark"
default_tab = "branches"
"#,
        )
        .unwrap();

        let cfg = AppConfig::load(Some(temp.path()));
        assert_eq!(cfg.jbgit.general.default_tab, DefaultTab::Branches);
        assert_eq!(cfg.theme.flavor, ThemeFlavor::GruvboxDark);
    }

    #[test]
    fn test_layout_ratio_parsing_and_overlay() {
        assert!(toml::from_str::<JbgitConfig>(DEFAULT_JBGIT_TOML).is_ok());
        let mut cfg = AppConfig::load_defaults();
        assert_eq!(cfg.jbgit.layout.log_ratio.weights(), [1, 3, 2]);
        assert_eq!(cfg.jbgit.layout.log_details_ratio.weights(), [3, 2]);
        assert_eq!(cfg.jbgit.layout.log_diff_right_ratio.weights(), [2, 1]);
        assert_eq!(cfg.jbgit.layout.log_diff_bottom_ratio.weights(), [3, 2]);
        assert_eq!(cfg.jbgit.layout.commit_ratio.weights(), [1, 2]);
        assert_eq!(cfg.jbgit.layout.commit_left_ratio.weights(), [3, 2]);
        assert_eq!(cfg.jbgit.layout.stash_ratio.weights(), [2, 3]);
        assert_eq!(cfg.jbgit.layout.stash_preview_ratio.weights(), [1, 2]);
        assert_eq!(cfg.jbgit.layout.console_ratio.weights(), [2, 3]);
        assert_eq!(cfg.jbgit.layout.file_history_ratio.weights(), [2, 3]);
        assert_eq!(cfg.jbgit.layout.file_history_vertical_ratio.weights(), [2, 3]);

        let overlay_toml = r#"
[layout]
log_ratio = [1, 4, 3]
log_details_ratio = [1, 1]
log_diff_right_ratio = [3, 2]
log_diff_bottom_ratio = [2, 1]
commit_ratio = [2, 5]
commit_left_ratio = [4, 1]
stash_preview_ratio = [2, 3]
file_history_ratio = [1, 3]
file_history_vertical_ratio = [1, 2]
"#;
        let overlay: JbgitConfigOverlay = toml::from_str(overlay_toml).unwrap();
        cfg.jbgit.merge_overlay(&overlay);

        assert_eq!(cfg.jbgit.layout.log_ratio.weights(), [1, 4, 3]);
        assert_eq!(cfg.jbgit.layout.log_details_ratio.weights(), [1, 1]);
        assert_eq!(cfg.jbgit.layout.log_diff_right_ratio.weights(), [3, 2]);
        assert_eq!(cfg.jbgit.layout.log_diff_bottom_ratio.weights(), [2, 1]);
        assert_eq!(cfg.jbgit.layout.commit_ratio.weights(), [2, 5]);
        assert_eq!(cfg.jbgit.layout.commit_left_ratio.weights(), [4, 1]);
        assert_eq!(cfg.jbgit.layout.stash_preview_ratio.weights(), [2, 3]);
        assert_eq!(cfg.jbgit.layout.file_history_ratio.weights(), [1, 3]);
        assert_eq!(cfg.jbgit.layout.file_history_vertical_ratio.weights(), [1, 2]);
        // Unspecified ratios keep defaults
        assert_eq!(cfg.jbgit.layout.stash_ratio.weights(), [2, 3]);
        assert_eq!(cfg.jbgit.layout.console_ratio.weights(), [2, 3]);
    }

    #[test]
    fn test_layout_invalid_overlays_preserve_lower_layer_and_sibling_keys() {
        let mut cfg = AppConfig::load_defaults();
        let valid_base: JbgitConfigOverlay = toml::from_str(
            r#"
[layout]
log_ratio = [2, 5, 3]
log_details_ratio = [4, 3]
log_diff_right_ratio = [3, 1]
log_diff_bottom_ratio = [4, 1]
commit_ratio = [3, 7]
commit_left_ratio = [7, 3]
stash_ratio = [1, 4]
stash_preview_ratio = [2, 5]
"#,
        )
        .unwrap();
        cfg.jbgit.merge_overlay(&valid_base);

        // Invalid ratios (zero sums, wrong lengths) must be ignored individually without
        // wiping lower-layer ratios or rejecting sibling valid keys in the same file.
        let bad_overlay: JbgitConfigOverlay = toml::from_str(
            r#"
[layout]
log_ratio = [0, 0, 0]
log_details_ratio = [0, 0]
log_diff_right_ratio = [1, 2, 3]
log_diff_bottom_ratio = []
commit_ratio = [0, 0]
commit_left_ratio = [5]
stash_ratio = [1]
stash_preview_ratio = [0, 0]
console_ratio = [1, 4]
"#,
        )
        .unwrap();
        cfg.jbgit.merge_overlay(&bad_overlay);

        assert_eq!(cfg.jbgit.layout.log_ratio.weights(), [2, 5, 3]);
        assert_eq!(cfg.jbgit.layout.log_details_ratio.weights(), [4, 3]);
        assert_eq!(cfg.jbgit.layout.log_diff_right_ratio.weights(), [3, 1]);
        assert_eq!(cfg.jbgit.layout.log_diff_bottom_ratio.weights(), [4, 1]);
        assert_eq!(cfg.jbgit.layout.commit_ratio.weights(), [3, 7]);
        assert_eq!(cfg.jbgit.layout.commit_left_ratio.weights(), [7, 3]);
        assert_eq!(cfg.jbgit.layout.stash_ratio.weights(), [1, 4]);
        assert_eq!(cfg.jbgit.layout.stash_preview_ratio.weights(), [2, 5]);
        assert_eq!(cfg.jbgit.layout.console_ratio.weights(), [1, 4]);

        // 2-element log_ratio and trailing-zero [1, 0, 0] are also rejected cleanly.
        let two_elem_log: JbgitConfigOverlay = toml::from_str(
            r#"
[layout]
log_ratio = [1, 2]
"#,
        )
        .unwrap();
        cfg.jbgit.merge_overlay(&two_elem_log);
        assert_eq!(cfg.jbgit.layout.log_ratio.weights(), [2, 5, 3]);

        let trailing_zero_log: JbgitConfigOverlay = toml::from_str(
            r#"
[layout]
log_ratio = [1, 0, 0]
"#,
        )
        .unwrap();
        cfg.jbgit.merge_overlay(&trailing_zero_log);
        assert_eq!(cfg.jbgit.layout.log_ratio.weights(), [2, 5, 3]);
    }

    #[test]
    fn test_layout_section_splitter_defaults_integration() {
        use crate::widgets::SplitterState;
        use jbgit_core::SplitterId;

        // Default [1, 3, 2] -> left 1/6, right 3/5
        let defaults = LayoutSection::default();
        let mut state = SplitterState::with_default_ratios(defaults.splitter_defaults());
        assert!((state.ratio(SplitterId::LogLeftHorizontal) - (1.0 / 6.0)).abs() < 1e-4);
        assert!((state.ratio(SplitterId::LogRightHorizontal) - 0.6).abs() < 1e-4);
        assert!((state.ratio(SplitterId::LogDetailsVertical) - 0.6).abs() < 1e-4);
        assert!((state.ratio(SplitterId::LogDiffRightHorizontal) - (2.0 / 3.0)).abs() < 1e-4);
        assert!((state.ratio(SplitterId::LogDiffBottomVertical) - 0.6).abs() < 1e-4);
        assert!((state.ratio(SplitterId::CommitMainHorizontal) - (1.0 / 3.0)).abs() < 1e-4);
        assert!((state.ratio(SplitterId::CommitLeftVertical) - 0.6).abs() < 1e-4);
        assert!((state.ratio(SplitterId::StashMainHorizontal) - 0.4).abs() < 1e-4);
        assert!((state.ratio(SplitterId::StashPreviewVertical) - (1.0 / 3.0)).abs() < 1e-4);
        assert!((state.ratio(SplitterId::ConsoleMainHorizontal) - 0.4).abs() < 1e-4);
        assert!((state.ratio(SplitterId::FileHistoryHorizontal) - 0.4).abs() < 1e-4);
        assert!((state.ratio(SplitterId::FileHistoryVertical) - 0.4).abs() < 1e-4);

        // Reset restores the layout-configured default (not hardcoded SplitterState::default)
        state.reset(SplitterId::LogLeftHorizontal);
        state.reset(SplitterId::LogRightHorizontal);
        state.reset(SplitterId::LogDetailsVertical);
        state.reset(SplitterId::CommitLeftVertical);
        state.reset(SplitterId::StashPreviewVertical);
        assert!((state.ratio(SplitterId::LogLeftHorizontal) - (1.0 / 6.0)).abs() < 1e-4);
        assert!((state.ratio(SplitterId::LogRightHorizontal) - 0.6).abs() < 1e-4);
        assert!((state.ratio(SplitterId::LogDetailsVertical) - 0.6).abs() < 1e-4);
        assert!((state.ratio(SplitterId::CommitLeftVertical) - 0.6).abs() < 1e-4);
        assert!((state.ratio(SplitterId::StashPreviewVertical) - (1.0 / 3.0)).abs() < 1e-4);

        let custom = LayoutSection {
            log_ratio: TripleRatio::new(1, 4, 3).unwrap(),
            log_details_ratio: PairRatio::new(1, 3).unwrap(),
            log_diff_right_ratio: PairRatio::new(3, 1).unwrap(),
            log_diff_bottom_ratio: PairRatio::new(2, 3).unwrap(),
            commit_ratio: PairRatio::new(1, 3).unwrap(),
            commit_left_ratio: PairRatio::new(4, 1).unwrap(),
            stash_ratio: PairRatio::new(3, 2).unwrap(),
            stash_preview_ratio: PairRatio::new(1, 1).unwrap(),
            console_ratio: PairRatio::new(1, 1).unwrap(),
            file_history_ratio: PairRatio::new(1, 3).unwrap(),
            file_history_vertical_ratio: PairRatio::new(3, 5).unwrap(),
        };
        let mut custom_state = SplitterState::with_default_ratios(custom.splitter_defaults());
        assert!((custom_state.ratio(SplitterId::LogLeftHorizontal) - 0.125).abs() < 1e-4);
        assert!((custom_state.ratio(SplitterId::LogRightHorizontal) - (4.0 / 7.0)).abs() < 1e-4);
        assert!((custom_state.ratio(SplitterId::LogDetailsVertical) - 0.25).abs() < 1e-4);
        assert!((custom_state.ratio(SplitterId::LogDiffRightHorizontal) - 0.75).abs() < 1e-4);
        assert!((custom_state.ratio(SplitterId::LogDiffBottomVertical) - 0.40).abs() < 1e-4);
        assert!((custom_state.ratio(SplitterId::CommitMainHorizontal) - 0.25).abs() < 1e-4);
        assert!((custom_state.ratio(SplitterId::CommitLeftVertical) - 0.80).abs() < 1e-4);
        assert!((custom_state.ratio(SplitterId::StashMainHorizontal) - 0.60).abs() < 1e-4);
        assert!((custom_state.ratio(SplitterId::StashPreviewVertical) - 0.50).abs() < 1e-4);
        assert!((custom_state.ratio(SplitterId::ConsoleMainHorizontal) - 0.50).abs() < 1e-4);
        assert!((custom_state.ratio(SplitterId::FileHistoryHorizontal) - 0.25).abs() < 1e-4);
        assert!((custom_state.ratio(SplitterId::FileHistoryVertical) - 0.375).abs() < 1e-4);

        // Mutating then resetting vertical & dock splitters restores custom configured defaults
        custom_state.update_from_pointer(
            SplitterId::LogDetailsVertical,
            jbgit_core::SplitAxis::Vertical,
            ratatui::layout::Rect::new(0, 0, 40, 40),
            0,
            25,
        );
        assert!((custom_state.ratio(SplitterId::LogDetailsVertical) - 0.25).abs() > 0.1);
        custom_state.reset(SplitterId::LogDetailsVertical);
        assert!((custom_state.ratio(SplitterId::LogDetailsVertical) - 0.25).abs() < 1e-4);

        custom_state.update_from_pointer(
            SplitterId::CommitLeftVertical,
            jbgit_core::SplitAxis::Vertical,
            ratatui::layout::Rect::new(0, 0, 40, 40),
            0,
            12,
        );
        assert!((custom_state.ratio(SplitterId::CommitLeftVertical) - 0.80).abs() > 0.1);
        custom_state.reset(SplitterId::CommitLeftVertical);
        assert!((custom_state.ratio(SplitterId::CommitLeftVertical) - 0.80).abs() < 1e-4);

        custom_state.update_from_pointer(
            SplitterId::LogDiffRightHorizontal,
            jbgit_core::SplitAxis::Horizontal,
            ratatui::layout::Rect::new(0, 0, 120, 40),
            60,
            0,
        );
        assert!((custom_state.ratio(SplitterId::LogDiffRightHorizontal) - 0.75).abs() > 0.1);
        custom_state.reset(SplitterId::LogDiffRightHorizontal);
        assert!((custom_state.ratio(SplitterId::LogDiffRightHorizontal) - 0.75).abs() < 1e-4);

        custom_state.update_from_pointer(
            SplitterId::LogDiffBottomVertical,
            jbgit_core::SplitAxis::Vertical,
            ratatui::layout::Rect::new(0, 0, 40, 40),
            0,
            30,
        );
        assert!((custom_state.ratio(SplitterId::LogDiffBottomVertical) - 0.40).abs() > 0.1);
        custom_state.reset(SplitterId::LogDiffBottomVertical);
        assert!((custom_state.ratio(SplitterId::LogDiffBottomVertical) - 0.40).abs() < 1e-4);
    }

    #[test]
    fn test_cross_layer_flavor_and_glyph_fallback_precedence() {
        let layer1 = tempfile::tempdir().unwrap();
        let layer2 = tempfile::tempdir().unwrap();
        let layer3 = tempfile::tempdir().unwrap();

        fs::write(
            layer1.path().join("theme.toml"),
            "flavor = \"catppuccin-mocha\"\n[glyphs]\nmode = \"unicode\"\n",
        )
        .unwrap();
        fs::write(
            layer2.path().join("jbgit.toml"),
            "[general]\nflavor = \"gruvbox-dark\"\n[graph]\nglyph_mode = \"nerd_font\"\n",
        )
        .unwrap();
        fs::write(
            layer3.path().join("jbgit.toml"),
            "[general]\nflavor = \"darcula\"\n",
        )
        .unwrap();
        fs::write(
            layer3.path().join("theme.toml"),
            "flavor = \"catppuccin-mocha\"\n",
        )
        .unwrap();

        let mut cfg = AppConfig::load_defaults();
        cfg.apply_dir_overlays(layer1.path(), false);
        assert_eq!(cfg.theme.flavor, ThemeFlavor::CatppuccinMocha);
        assert_eq!(cfg.theme.glyphs.mode, GlyphModePref::Unicode);

        // Layer 2 jbgit.toml without same-layer theme.toml overrides Layer 1 theme.toml
        cfg.apply_dir_overlays(layer2.path(), false);
        assert_eq!(cfg.theme.flavor, ThemeFlavor::GruvboxDark);
        assert_eq!(cfg.theme.glyphs.mode, GlyphModePref::NerdFont);

        // Layer 3 same-layer theme.toml wins over same-layer jbgit.toml
        cfg.apply_dir_overlays(layer3.path(), false);
        assert_eq!(cfg.theme.flavor, ThemeFlavor::CatppuccinMocha);
    }

    #[test]
    fn test_keymap_presets_jetbrains_and_vim() {
        use crate::keymap::KeyAction;

        let mut jb_cfg = AppConfig::load_defaults();
        let jb_ov: KeymapConfigOverlay = toml::from_str("preset = \"jetbrains\"\n").unwrap();
        jb_cfg.keymap.merge_overlay(&jb_ov);
        assert_eq!(jb_cfg.keymap.preset, KeymapPreset::Jetbrains);
        assert!(jb_cfg.keymap.tree_speed_search);
        assert_eq!(
            jb_cfg
                .keymap
                .primary_key_for(&[KeymapScope::Global], &KeyAction::GitPush)
                .as_deref(),
            Some("Ctrl+Shift+K")
        );
        assert_eq!(
            jb_cfg
                .keymap
                .primary_key_for(&[KeymapScope::Global], &KeyAction::TabCommit)
                .as_deref(),
            Some("Ctrl+K")
        );
        assert_eq!(
            jb_cfg
                .keymap
                .primary_key_for(&[KeymapScope::Global], &KeyAction::GitPull)
                .as_deref(),
            Some("Ctrl+T")
        );
        assert_eq!(
            jb_cfg
                .keymap
                .primary_key_for(&[KeymapScope::Global], &KeyAction::NewBranch)
                .as_deref(),
            Some("Ctrl+Alt+N")
        );

        let mut vim_cfg = AppConfig::load_defaults();
        let vim_ov: KeymapConfigOverlay = toml::from_str("preset = \"vim\"\n").unwrap();
        vim_cfg.keymap.merge_overlay(&vim_ov);
        assert_eq!(vim_cfg.keymap.preset, KeymapPreset::Vim);
        assert!(!vim_cfg.keymap.tree_speed_search);
        assert_eq!(
            vim_cfg
                .keymap
                .primary_key_for(&[KeymapScope::Log], &KeyAction::PageDownHalf)
                .as_deref(),
            Some("Ctrl+D")
        );
        assert_eq!(
            vim_cfg
                .keymap
                .primary_key_for(&[KeymapScope::Log], &KeyAction::CompareOrDiff)
                .as_deref(),
            Some("d")
        );
        assert_eq!(
            vim_cfg
                .keymap
                .primary_key_for(&[KeymapScope::Diff], &KeyAction::StageSelection)
                .as_deref(),
            Some("s")
        );
        assert_eq!(
            vim_cfg
                .keymap
                .primary_key_for(&[KeymapScope::Diff], &KeyAction::UnstageSelection)
                .as_deref(),
            Some("u")
        );
    }

    #[test]
    fn test_keymap_prepend_noop_and_string_on() {
        use crate::keymap::KeyAction;

        let mut cfg = AppConfig::load_defaults();
        let ov_toml = r#"
[global]
prepend_keymap = [
    { on = "r", run = "noop" },
]

[log]
prepend_keymap = [
    { on = "<Down>", run = "noop" },
    { on = "K", run = "cursor_up", desc = "Up" },
]
"#;
        let ov: KeymapConfigOverlay = toml::from_str(ov_toml).unwrap();
        cfg.keymap.merge_overlay(&ov);

        let global_refresh = cfg.keymap.all_keys_for(&[KeymapScope::Global], &KeyAction::Refresh);
        assert!(!global_refresh.contains(&"r".to_string()), "{global_refresh:?}");
        assert!(global_refresh.contains(&"F5".to_string()), "{global_refresh:?}");

        let log_down = cfg.keymap.all_keys_for(&[KeymapScope::Log], &KeyAction::CursorDown);
        assert!(!log_down.contains(&"↓".to_string()), "{log_down:?}");
        assert!(log_down.contains(&"j".to_string()), "{log_down:?}");

        assert_eq!(
            cfg.keymap
                .primary_key_for(&[KeymapScope::Log], &KeyAction::CursorUp)
                .as_deref(),
            Some("K")
        );
    }

    #[test]
    fn test_keymap_footer_max_lines_and_show_hint_inheritance() {
        use crate::keymap::KeyAction;

        assert_eq!(KeymapConfig::default().footer_max_lines, 1);
        let mut cfg = AppConfig::load_defaults();
        assert_eq!(cfg.keymap.footer_max_lines, 1);

        // Parsing footer_max_lines (0, 2, 3) and preserving across preset switch
        for lines in [0u16, 2, 3] {
            let ov: KeymapConfigOverlay =
                toml::from_str(&format!("footer_max_lines = {lines}\n")).unwrap();
            cfg.keymap.merge_overlay(&ov);
            assert_eq!(cfg.keymap.footer_max_lines, lines);
        }

        let preset_ov: KeymapConfigOverlay = toml::from_str("preset = \"vim\"\n").unwrap();
        cfg.keymap.merge_overlay(&preset_ov);
        assert_eq!(cfg.keymap.preset, KeymapPreset::Vim);
        assert_eq!(cfg.keymap.footer_max_lines, 3);

        // Vim [log] replaces <C-d> with page_down_half while `d` inherits show_hint = Some(true)
        assert!(
            cfg.keymap
                .log
                .key_bindings
                .iter()
                .any(|b| b.run == KeyAction::CompareOrDiff
                    && b.on == ["d"]
                    && b.show_hint == Some(true))
        );

        // Prepending rebind without show_hint inherits Some(true) and demotes base binding,
        // so a later show_hint = false cleanly hides the action.
        let mut cfg2 = AppConfig::load_defaults();
        let ov_rebind: KeymapConfigOverlay = toml::from_str(
            r#"
[commit]
prepend_keymap = [
    { on = "S", run = "toggle_stage" },
]
"#,
        )
        .unwrap();
        cfg2.keymap.merge_overlay(&ov_rebind);
        assert!(cfg2.keymap.commit.key_bindings.iter().any(|b| {
            b.run == KeyAction::ToggleStage && b.on == ["S"] && b.show_hint == Some(true)
        }));
        assert!(cfg2.keymap.commit.key_bindings.iter().all(|b| {
            !(b.run == KeyAction::ToggleStage && b.on == ["<Space>"] && b.show_hint == Some(true))
        }));

        let ov_hide: KeymapConfigOverlay = toml::from_str(
            r#"
[commit]
prepend_keymap = [
    { on = "S", run = "toggle_stage", show_hint = false },
]
"#,
        )
        .unwrap();
        cfg2.keymap.merge_overlay(&ov_hide);
        assert!(cfg2.keymap.commit.key_bindings.iter().any(|b| {
            b.run == KeyAction::ToggleStage && b.on == ["S"] && b.show_hint == Some(false)
        }));
        assert!(
            !cfg2
                .keymap
                .commit
                .key_bindings
                .iter()
                .any(|b| b.run == KeyAction::ToggleStage && b.show_hint == Some(true))
        );

        // key_bindings replacement inherits show_hint = Some(true) from base Refresh
        let ov_kb: KeymapConfigOverlay = toml::from_str(
            r#"
[global]
key_bindings = [
    { on = ["<F5>"], run = "refresh" },
]
"#,
        )
        .unwrap();
        cfg2.keymap.merge_overlay(&ov_kb);
        assert!(cfg2.keymap.global.key_bindings.iter().any(|b| {
            b.run == KeyAction::Refresh && b.on == ["<F5>"] && b.show_hint == Some(true)
        }));

        // Explicit show_hint = true turns on a hidden action
        let ov_enable: KeymapConfigOverlay = toml::from_str(
            r#"
[log]
prepend_keymap = [
    { on = "u", run = "uncommit", show_hint = true },
]
"#,
        )
        .unwrap();
        cfg2.keymap.merge_overlay(&ov_enable);
        assert!(cfg2.keymap.log.key_bindings.iter().any(|b| {
            b.run == KeyAction::Uncommit && b.on == ["u"] && b.show_hint == Some(true)
        }));

        // Full key_bindings overlay listing primary + alias without show_hint keeps Some(true) only on `/`
        let mut cfg3 = AppConfig::load_defaults();
        let ov_multi_alias: KeymapConfigOverlay = toml::from_str(
            r#"
[log]
key_bindings = [
    { on = ["/"], run = "focus_filter" },
    { on = ["<C-f>"], run = "focus_filter" },
]
"#,
        )
        .unwrap();
        cfg3.keymap.merge_overlay(&ov_multi_alias);
        let focus_bindings: Vec<_> = cfg3
            .keymap
            .log
            .key_bindings
            .iter()
            .filter(|b| b.run == KeyAction::FocusFilter)
            .collect();
        assert_eq!(focus_bindings.len(), 2);
        assert_eq!(focus_bindings[0].on, ["/"]);
        assert_eq!(focus_bindings[0].show_hint, Some(true));
        assert_eq!(focus_bindings[1].on, ["<C-f>"]);
        assert_eq!(focus_bindings[1].show_hint, None);

        // Explicit show_hint = false alongside sibling alias without show_hint does NOT steal Some(true)
        let mut cfg4 = AppConfig::load_defaults();
        let ov_hide_with_alias: KeymapConfigOverlay = toml::from_str(
            r#"
[log]
key_bindings = [
    { on = "/", run = "focus_filter", show_hint = false },
    { on = "<C-f>", run = "focus_filter" },
]
"#,
        )
        .unwrap();
        cfg4.keymap.merge_overlay(&ov_hide_with_alias);
        assert!(
            !cfg4
                .keymap
                .log
                .key_bindings
                .iter()
                .any(|b| b.run == KeyAction::FocusFilter && b.show_hint == Some(true))
        );
    }

    #[test]
    fn test_keymap_append_noop_and_explicit_hide_hint_semantics() {
        use crate::keymap::KeyAction;

        // 1. append_keymap without show_hint preserves primary binding's show_hint and order
        let mut cfg = AppConfig::load_defaults();
        let copy_idx_before = cfg
            .keymap
            .log
            .key_bindings
            .iter()
            .position(|b| b.run == KeyAction::CopyHash && b.on == ["y"])
            .unwrap();
        let ov_append_alias: KeymapConfigOverlay = toml::from_str(
            r#"
[log]
append_keymap = [
    { on = "Y", run = "copy_hash" },
]
"#,
        )
        .unwrap();
        cfg.keymap.merge_overlay(&ov_append_alias);
        let copy_idx_after = cfg
            .keymap
            .log
            .key_bindings
            .iter()
            .position(|b| b.run == KeyAction::CopyHash && b.on == ["y"])
            .unwrap();
        assert_eq!(copy_idx_after, copy_idx_before);
        assert_eq!(
            cfg.keymap.log.key_bindings[copy_idx_after].show_hint,
            Some(true)
        );
        let appended = cfg
            .keymap
            .log
            .key_bindings
            .iter()
            .find(|b| b.run == KeyAction::CopyHash && b.on == ["Y"])
            .unwrap();
        assert_eq!(appended.show_hint, None);

        // 2. append_keymap with explicit show_hint = true demotes base Some(true)
        let ov_append_explicit: KeymapConfigOverlay = toml::from_str(
            r#"
[log]
append_keymap = [
    { on = "Y", run = "copy_hash", show_hint = true },
]
"#,
        )
        .unwrap();
        cfg.keymap.merge_overlay(&ov_append_explicit);
        assert_eq!(
            cfg.keymap
                .log
                .key_bindings
                .iter()
                .filter(|b| b.run == KeyAction::CopyHash && b.show_hint == Some(true))
                .map(|b| b.on.clone())
                .collect::<Vec<_>>(),
            vec![vec!["Y".to_string()]]
        );

        // 3. prepend_keymap with explicit show_hint = false on an alias key hides base Some(true)
        let mut cfg_hide = AppConfig::load_defaults();
        let ov_hide_f5: KeymapConfigOverlay = toml::from_str(
            r#"
[global]
prepend_keymap = [
    { on = "<F5>", run = "refresh", show_hint = false },
]
"#,
        )
        .unwrap();
        cfg_hide.keymap.merge_overlay(&ov_hide_f5);
        assert!(
            !cfg_hide
                .keymap
                .global
                .key_bindings
                .iter()
                .any(|b| b.run == KeyAction::Refresh && b.show_hint == Some(true))
        );

        let mut cfg_jb = KeymapConfig::for_preset(KeymapPreset::Jetbrains);
        let ov_hide_jb_pull: KeymapConfigOverlay = toml::from_str(
            r#"
[global]
prepend_keymap = [
    { on = "p", run = "git_pull", show_hint = false },
]
"#,
        )
        .unwrap();
        cfg_jb.merge_overlay(&ov_hide_jb_pull);
        assert!(
            !cfg_jb
                .global
                .key_bindings
                .iter()
                .any(|b| b.run == KeyAction::GitPull && b.show_hint == Some(true))
        );

        // 4. Unbinding primary key via noop promotes surviving alias, unless disabled/also unbound
        let mut cfg_noop = AppConfig::load_defaults();
        let ov_unbind_r: KeymapConfigOverlay = toml::from_str(
            r#"
[global]
prepend_keymap = [
    { on = "r", run = "noop" },
]
"#,
        )
        .unwrap();
        cfg_noop.keymap.merge_overlay(&ov_unbind_r);
        assert!(cfg_noop.keymap.global.key_bindings.iter().any(|b| {
            b.run == KeyAction::Refresh && b.on == ["<F5>"] && b.show_hint == Some(true)
        }));

        let mut cfg_noop_disabled = AppConfig::load_defaults();
        let ov_hide_then_unbind: KeymapConfigOverlay = toml::from_str(
            r#"
[global]
prepend_keymap = [
    { on = "<F5>", run = "refresh", show_hint = false },
]
"#,
        )
        .unwrap();
        cfg_noop_disabled.keymap.merge_overlay(&ov_hide_then_unbind);
        // Re-enable `r` first? No, `<F5>` is `Some(false)`, so unbinding `r` must NOT promote `<F5>`.
        cfg_noop_disabled.keymap.merge_overlay(&ov_unbind_r);
        assert!(
            !cfg_noop_disabled
                .keymap
                .global
                .key_bindings
                .iter()
                .any(|b| b.run == KeyAction::Refresh && b.show_hint == Some(true))
        );

        let mut cfg_unbind_both = AppConfig::load_defaults();
        let ov_both_noop: KeymapConfigOverlay = toml::from_str(
            r#"
[global]
prepend_keymap = [
    { on = "r", run = "noop" },
    { on = "<F5>", run = "noop" },
]
"#,
        )
        .unwrap();
        cfg_unbind_both.keymap.merge_overlay(&ov_both_noop);
        assert!(
            !cfg_unbind_both
                .keymap
                .global
                .key_bindings
                .iter()
                .any(|b| b.run == KeyAction::Refresh)
        );

        // 5. Vim [diff] marks <Space> -> ToggleStage as show_hint = Some(false) while keeping it bound
        let vim = KeymapConfig::for_preset(KeymapPreset::Vim);
        assert!(vim.diff.key_bindings.iter().any(|b| {
            b.run == KeyAction::ToggleStage
                && b.on == ["<Space>"]
                && b.show_hint == Some(false)
        }));
        assert!(vim.diff.key_bindings.iter().any(|b| {
            b.run == KeyAction::StageSelection && b.on == ["s"] && b.show_hint == Some(true)
        }));
        assert!(vim.diff.key_bindings.iter().any(|b| {
            b.run == KeyAction::UnstageSelection && b.on == ["u"] && b.show_hint == Some(true)
        }));
    }
}
