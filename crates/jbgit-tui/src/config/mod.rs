pub mod discovery;
pub mod jbgit_config;
pub mod keymap_config;
pub mod theme_config;

use std::fs;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result};
use serde::de::DeserializeOwned;

pub use discovery::{JBGIT_CONFIG_DIR_ENV, find_config_file, global_config_dir};
pub use jbgit_config::{
    AnnotateSection, CommitSection, DefaultTab, GitSection, GlyphModePref, GraphBackendPref,
    JbgitConfig, JbgitConfigOverlay, KeymapSection, KeymapSectionOverlay, LanguagePref,
    LayoutSection, PairRatio, ThemeFlavor, TripleRatio,
};
pub use keymap_config::{
    KeyBindingConfig, KeymapConfig, KeymapConfigOverlay, KeymapPreset, KeymapScope,
};
pub use theme_config::{
    GlyphsSection, GraphThemeSection, PaletteSection, RefStyleConfig, RefsSection, ThemeConfig,
    ThemeConfigOverlay, ThemeFlavorData, TrackColorConfig,
};

pub const DEFAULT_JBGIT_TOML: &str = include_str!("defaults/jbgit.default.toml");

pub const BUILTIN_THEME_DARCULA_TOML: &str = include_str!("defaults/themes/darcula.toml");
pub const BUILTIN_THEME_CATPPUCCIN_MOCHA_TOML: &str =
    include_str!("defaults/themes/catppuccin-mocha.toml");
pub const BUILTIN_THEME_GRUVBOX_DARK_TOML: &str = include_str!("defaults/themes/gruvbox-dark.toml");

pub const BUILTIN_KEYMAP_DEFAULT_TOML: &str = include_str!("defaults/keymaps/default.toml");
pub const BUILTIN_KEYMAP_JETBRAINS_TOML: &str = include_str!("defaults/keymaps/jetbrains.toml");
pub const BUILTIN_KEYMAP_VIM_TOML: &str = include_str!("defaults/keymaps/vim.toml");

/// Validates a flavor/preset name and returns `<name>.toml` if it is a single safe path segment.
pub(crate) fn safe_preset_filename(name: &str) -> Option<String> {
    let trimmed = name.trim();
    if trimmed.is_empty()
        || trimmed.contains(['/', '\\', ':', '\0'])
        || trimmed == "."
        || trimmed == ".."
    {
        return None;
    }
    let mut comps = Path::new(trimmed).components();
    match (comps.next(), comps.next()) {
        (Some(Component::Normal(_)), None) => Some(format!("{trimmed}.toml")),
        _ => None,
    }
}

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
        let theme = ThemeConfig {
            flavor: jbgit.general.flavor.clone(),
            glyphs: GlyphsSection {
                mode: jbgit.graph.glyph_mode,
            },
            ..ThemeConfig::default()
        };
        let mut keymap = KeymapConfig::for_preset(jbgit.keymap.preset.clone());
        keymap.footer_max_lines = jbgit.keymap.footer_max_lines;
        if let Some(tss) = jbgit.keymap.tree_speed_search {
            keymap.tree_speed_search = tss;
        }
        Self {
            jbgit,
            theme,
            keymap,
        }
    }

    /// Loads the configuration with layered precedence:
    /// Default -> Global User Config -> Repository Config -> JBGIT_CONFIG_DIR
    pub fn load(workdir: Option<&Path>) -> Self {
        let global_dir = global_config_dir();
        let repo_dir = workdir.map(|wd| wd.join(".jbgit"));
        let env_dir = std::env::var_os(JBGIT_CONFIG_DIR_ENV).map(PathBuf::from);

        let mut layers: Vec<(&Path, bool)> = vec![(&global_dir, false)];
        if let Some(ref rd) = repo_dir {
            layers.push((rd.as_path(), true));
        }
        if let Some(ref ed) = env_dir {
            layers.push((ed.as_path(), false));
        }

        Self::load_from_layers(&layers)
    }

    pub(crate) fn load_from_layers(layers: &[(&Path, bool)]) -> Self {
        let mut config = Self::load_defaults();
        let mut final_preset = config.keymap.preset.clone();
        let mut keymap_overlays: Vec<KeymapConfigOverlay> = Vec::new();

        // Pass 1: Load jbgit.toml / config.toml plus optional legacy theme.toml overlays.
        for &(dir, allow_config_toml_alias) in layers {
            let mut flags = OverlayFlags::default();
            let mut jbgit_km_preset: Option<KeymapPreset> = None;
            let mut jbgit_km_footer: Option<u16> = None;
            let mut jbgit_km_tss: Option<bool> = None;

            let mut apply_jbgit = |ov: &JbgitConfigOverlay| {
                if ov
                    .general
                    .as_ref()
                    .and_then(|g| g.flavor.as_ref())
                    .is_some()
                {
                    flags.jbgit_flavor_set = true;
                }
                if ov.graph.as_ref().and_then(|g| g.glyph_mode).is_some() {
                    flags.jbgit_glyph_set = true;
                }
                if let Some(km) = &ov.keymap {
                    if let Some(p) = &km.preset {
                        jbgit_km_preset = Some(p.clone());
                    }
                    if let Some(lines) = km.footer_max_lines {
                        jbgit_km_footer = Some(lines);
                    }
                    if let Some(tss) = km.tree_speed_search {
                        jbgit_km_tss = Some(tss);
                    }
                }
                config.jbgit.merge_overlay(ov);
            };

            if allow_config_toml_alias {
                load_toml_overlay::<JbgitConfigOverlay>(&dir.join("config.toml"), &mut apply_jbgit);
            }
            load_toml_overlay::<JbgitConfigOverlay>(&dir.join("jbgit.toml"), &mut apply_jbgit);

            load_toml_overlay::<ThemeConfigOverlay>(&dir.join("theme.toml"), |ov| {
                if ov.flavor.is_some() {
                    flags.theme_flavor_set = true;
                }
                if ov.glyphs.as_ref().and_then(|g| g.mode).is_some() {
                    flags.theme_glyph_set = true;
                }
                config.theme.merge_overlay(ov);
            });
            if flags.jbgit_flavor_set && !flags.theme_flavor_set {
                config.theme.flavor = config.jbgit.general.flavor.clone();
            }
            if flags.jbgit_glyph_set && !flags.theme_glyph_set {
                config.theme.glyphs.mode = config.jbgit.graph.glyph_mode;
            }

            if let Some(p) = jbgit_km_preset {
                final_preset = p;
            }
            if jbgit_km_footer.is_some() || jbgit_km_tss.is_some() {
                keymap_overlays.push(KeymapConfigOverlay {
                    footer_max_lines: jbgit_km_footer,
                    tree_speed_search: jbgit_km_tss,
                    ..Default::default()
                });
            }
        }

        // Pass 2: Load layered `themes/<final_flavor>.toml` overrides.
        if let Some(filename) = safe_preset_filename(config.theme.flavor.as_str()) {
            for &(dir, _) in layers {
                load_toml_overlay::<ThemeFlavorData>(&dir.join("themes").join(&filename), |data| {
                    config.theme.flavor_overrides.merge_from(data);
                });
            }
        }

        // Pass 3: Build keymap from `keymaps/default.toml`, then `keymaps/<final_preset>.toml`,
        // then apply collected `[keymap]` settings from `jbgit.toml` without re-triggering preset resets.
        let mut keymap = KeymapConfig::for_preset(KeymapPreset::Default);
        for &(dir, _) in layers {
            load_toml_overlay::<KeymapConfigOverlay>(
                &dir.join("keymaps").join("default.toml"),
                |ov| {
                    keymap.apply_preset_overlay(ov);
                },
            );
        }

        if final_preset != KeymapPreset::Default {
            let mut disk_preset_overlays: Vec<KeymapConfigOverlay> = Vec::new();
            if let Some(filename) = safe_preset_filename(final_preset.as_str()) {
                for &(dir, _) in layers {
                    load_toml_overlay::<KeymapConfigOverlay>(
                        &dir.join("keymaps").join(&filename),
                        |ov| {
                            disk_preset_overlays.push(ov.clone());
                        },
                    );
                }
            }

            if !disk_preset_overlays.is_empty() {
                for ov in &disk_preset_overlays {
                    keymap.apply_preset_overlay(ov);
                }
            } else {
                let builtin = match &final_preset {
                    KeymapPreset::Jetbrains => Some(BUILTIN_KEYMAP_JETBRAINS_TOML),
                    KeymapPreset::Vim => Some(BUILTIN_KEYMAP_VIM_TOML),
                    KeymapPreset::Default | KeymapPreset::Custom(_) => None,
                };
                if let Some(toml_str) = builtin
                    && let Ok(ov) = toml::from_str::<KeymapConfigOverlay>(toml_str)
                {
                    keymap.apply_preset_overlay(&ov);
                }
            }
            keymap.preset = final_preset;
        }

        for ov in &keymap_overlays {
            keymap.merge_overlay_ignoring_preset(ov);
        }
        config.keymap = keymap;

        config
    }

    #[cfg(test)]
    fn apply_dir_overlays(&mut self, dir: &Path, allow_config_toml_alias: bool) {
        let mut flags = OverlayFlags::default();
        if allow_config_toml_alias {
            load_toml_overlay::<JbgitConfigOverlay>(&dir.join("config.toml"), |ov| {
                if ov
                    .general
                    .as_ref()
                    .and_then(|g| g.flavor.as_ref())
                    .is_some()
                {
                    flags.jbgit_flavor_set = true;
                }
                if ov.graph.as_ref().and_then(|g| g.glyph_mode).is_some() {
                    flags.jbgit_glyph_set = true;
                }
                self.jbgit.merge_overlay(ov);
            });
        }
        load_toml_overlay::<JbgitConfigOverlay>(&dir.join("jbgit.toml"), |ov| {
            if ov
                .general
                .as_ref()
                .and_then(|g| g.flavor.as_ref())
                .is_some()
            {
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

        // Honor jbgit.toml `[general].flavor` / `[graph].glyph_mode` when theme.toml in this layer did not override them.
        if flags.jbgit_flavor_set && !flags.theme_flavor_set {
            self.theme.flavor = self.jbgit.general.flavor.clone();
        }
        if flags.jbgit_glyph_set && !flags.theme_glyph_set {
            self.theme.glyphs.mode = self.jbgit.graph.glyph_mode;
        }

        if let Some(filename) = safe_preset_filename(self.theme.flavor.as_str()) {
            load_toml_overlay::<ThemeFlavorData>(&dir.join("themes").join(&filename), |data| {
                self.theme.flavor_overrides.merge_from(data);
            });
        }
    }

    /// Returns the embedded TOML content for the requested default configuration.
    pub fn default_config_content(which: Option<&str>) -> &'static str {
        match which.map(|s| s.trim().to_ascii_lowercase()).as_deref() {
            Some("theme" | "theme.toml" | "themes/darcula" | "themes/darcula.toml" | "darcula") => {
                BUILTIN_THEME_DARCULA_TOML
            }
            Some(
                "themes/catppuccin-mocha" | "themes/catppuccin-mocha.toml" | "catppuccin-mocha",
            ) => BUILTIN_THEME_CATPPUCCIN_MOCHA_TOML,
            Some("themes/gruvbox-dark" | "themes/gruvbox-dark.toml" | "gruvbox-dark") => {
                BUILTIN_THEME_GRUVBOX_DARK_TOML
            }
            Some("keymap" | "keymaps/default" | "keymaps/default.toml") => {
                BUILTIN_KEYMAP_DEFAULT_TOML
            }
            Some("keymaps/jetbrains" | "keymaps/jetbrains.toml" | "jetbrains") => {
                BUILTIN_KEYMAP_JETBRAINS_TOML
            }
            Some("keymaps/vim" | "keymaps/vim.toml" | "vim") => BUILTIN_KEYMAP_VIM_TOML,
            _ => DEFAULT_JBGIT_TOML,
        }
    }

    /// Prints the requested default configuration to standard output.
    pub fn print_default_config(which: Option<&str>) {
        print!("{}", Self::default_config_content(which));
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
            ("themes/darcula.toml", BUILTIN_THEME_DARCULA_TOML),
            (
                "themes/catppuccin-mocha.toml",
                BUILTIN_THEME_CATPPUCCIN_MOCHA_TOML,
            ),
            ("themes/gruvbox-dark.toml", BUILTIN_THEME_GRUVBOX_DARK_TOML),
            ("keymaps/default.toml", BUILTIN_KEYMAP_DEFAULT_TOML),
            ("keymaps/jetbrains.toml", BUILTIN_KEYMAP_JETBRAINS_TOML),
            ("keymaps/vim.toml", BUILTIN_KEYMAP_VIM_TOML),
        ];

        let mut report = Vec::with_capacity(files.len());
        for (name, content) in files {
            let file_path = dir.join(name);
            if file_path.exists() && !force {
                report.push((name, false));
            } else {
                if let Some(parent) = file_path.parent() {
                    fs::create_dir_all(parent)
                        .with_context(|| format!("Failed to create directory {:?}", parent))?;
                }
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

    /// Persists `[commit] warn_crlf = <value>` into `<workdir>/.jbgit/jbgit.toml`
    /// while preserving any other settings already present in that file.
    pub fn persist_repo_commit_warn_crlf(workdir: &Path, warn_crlf: bool) -> Result<PathBuf> {
        let jbgit_dir = workdir.join(".jbgit");
        fs::create_dir_all(&jbgit_dir)
            .with_context(|| format!("Failed to create {:?}", jbgit_dir))?;
        let path = jbgit_dir.join("jbgit.toml");

        let mut root = if path.is_file() {
            let existing =
                fs::read_to_string(&path).with_context(|| format!("Failed to read {:?}", path))?;
            existing
                .parse::<toml::Table>()
                .unwrap_or_else(|_| toml::Table::new())
        } else {
            toml::Table::new()
        };

        let commit_table = root
            .entry("commit".to_string())
            .or_insert_with(|| toml::Value::Table(toml::Table::new()));
        if !commit_table.is_table() {
            *commit_table = toml::Value::Table(toml::Table::new());
        }
        if let Some(tbl) = commit_table.as_table_mut() {
            tbl.insert("warn_crlf".to_string(), toml::Value::Boolean(warn_crlf));
        }

        let serialized = toml::to_string_pretty(&root)
            .with_context(|| format!("Failed to serialize {:?}", path))?;
        fs::write(&path, serialized).with_context(|| format!("Failed to write {:?}", path))?;
        Ok(path)
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
        assert_eq!(
            engine_cfg.worktree_renames,
            jbgit_engine::StatusScanMode::Never
        );
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

        let (res, report) = AppConfig::init_config_detailed(Some(&target_dir), false).unwrap();
        assert_eq!(res, target_dir);
        assert_eq!(report.len(), 7);
        assert!(report.iter().all(|(_, created)| *created));

        for rel in [
            "jbgit.toml",
            "themes/darcula.toml",
            "themes/catppuccin-mocha.toml",
            "themes/gruvbox-dark.toml",
            "keymaps/default.toml",
            "keymaps/jetbrains.toml",
            "keymaps/vim.toml",
        ] {
            assert!(target_dir.join(rel).is_file(), "missing {rel}");
        }
        assert!(!target_dir.join("theme.toml").exists());
        assert!(!target_dir.join("keymap.toml").exists());

        let jbgit_content = fs::read_to_string(target_dir.join("jbgit.toml")).unwrap();
        assert!(jbgit_content.contains("language = \"zh-CN\""));
        assert!(jbgit_content.contains("[keymap]"));

        // Second run with force=false skips all existing files
        let (_, report2) = AppConfig::init_config_detailed(Some(&target_dir), false).unwrap();
        assert!(report2.iter().all(|(_, created)| !*created));

        // force=true overwrites
        fs::write(target_dir.join("themes/darcula.toml"), "# modified").unwrap();
        let (_, report3) = AppConfig::init_config_detailed(Some(&target_dir), true).unwrap();
        assert!(report3.iter().all(|(_, created)| *created));
        assert!(
            fs::read_to_string(target_dir.join("themes/darcula.toml"))
                .unwrap()
                .contains("[palette]")
        );
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
                .any(|b| b.run == crate::keymap::KeyAction::Refresh && b.on == ["R"])
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
        assert_eq!(
            cfg.jbgit.layout.file_history_vertical_ratio.weights(),
            [2, 3]
        );

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
        assert_eq!(
            cfg.jbgit.layout.file_history_vertical_ratio.weights(),
            [1, 2]
        );
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
    { on = "R", run = "noop" },
]

[log]
prepend_keymap = [
    { on = "<Down>", run = "noop" },
    { on = "K", run = "cursor_up", desc = "Up" },
]
"#;
        let ov: KeymapConfigOverlay = toml::from_str(ov_toml).unwrap();
        cfg.keymap.merge_overlay(&ov);

        let global_refresh = cfg
            .keymap
            .all_keys_for(&[KeymapScope::Global], &KeyAction::Refresh);
        assert!(
            !global_refresh.contains(&"R".to_string()),
            "{global_refresh:?}"
        );
        assert!(
            global_refresh.contains(&"F5".to_string()),
            "{global_refresh:?}"
        );

        let log_down = cfg
            .keymap
            .all_keys_for(&[KeymapScope::Log], &KeyAction::CursorDown);
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

        // key_bindings replacement inherits show_hint = Some(true) from base ToggleBranchesPopup
        let ov_kb: KeymapConfigOverlay = toml::from_str(
            r#"
[global]
key_bindings = [
    { on = ["<F6>"], run = "toggle_branches_popup" },
]
"#,
        )
        .unwrap();
        cfg2.keymap.merge_overlay(&ov_kb);
        assert!(cfg2.keymap.global.key_bindings.iter().any(|b| {
            b.run == KeyAction::ToggleBranchesPopup && b.on == ["<F6>"] && b.show_hint == Some(true)
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
        let checkout_idx_before = cfg
            .keymap
            .log
            .key_bindings
            .iter()
            .position(|b| b.run == KeyAction::Checkout && b.on == ["c"])
            .unwrap();
        let ov_append_alias: KeymapConfigOverlay = toml::from_str(
            r#"
[log]
append_keymap = [
    { on = "C", run = "checkout" },
]
"#,
        )
        .unwrap();
        cfg.keymap.merge_overlay(&ov_append_alias);
        let checkout_idx_after = cfg
            .keymap
            .log
            .key_bindings
            .iter()
            .position(|b| b.run == KeyAction::Checkout && b.on == ["c"])
            .unwrap();
        assert_eq!(checkout_idx_after, checkout_idx_before);
        assert_eq!(
            cfg.keymap.log.key_bindings[checkout_idx_after].show_hint,
            Some(true)
        );
        let appended = cfg
            .keymap
            .log
            .key_bindings
            .iter()
            .find(|b| b.run == KeyAction::Checkout && b.on == ["C"])
            .unwrap();
        assert_eq!(appended.show_hint, None);

        // 2. append_keymap with explicit show_hint = true demotes base Some(true)
        let ov_append_explicit: KeymapConfigOverlay = toml::from_str(
            r#"
[log]
append_keymap = [
    { on = "C", run = "checkout", show_hint = true },
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
                .filter(|b| b.run == KeyAction::Checkout && b.show_hint == Some(true))
                .map(|b| b.on.clone())
                .collect::<Vec<_>>(),
            vec![vec!["C".to_string()]]
        );

        // 3. prepend_keymap with explicit show_hint = false on an alias key hides base Some(true)
        let mut cfg_hide = AppConfig::load_defaults();
        let ov_hide_alt_b: KeymapConfigOverlay = toml::from_str(
            r#"
[global]
prepend_keymap = [
    { on = "<A-b>", run = "toggle_branches_popup", show_hint = false },
]
"#,
        )
        .unwrap();
        cfg_hide.keymap.merge_overlay(&ov_hide_alt_b);
        assert!(
            !cfg_hide
                .keymap
                .global
                .key_bindings
                .iter()
                .any(|b| b.run == KeyAction::ToggleBranchesPopup && b.show_hint == Some(true))
        );

        let mut cfg_jb = KeymapConfig::for_preset(KeymapPreset::Jetbrains);
        let ov_hide_jb_pull: KeymapConfigOverlay = toml::from_str(
            r#"
[global]
prepend_keymap = [
    { on = "L", run = "git_pull", show_hint = false },
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
        let ov_unbind_dot: KeymapConfigOverlay = toml::from_str(
            r#"
[global]
prepend_keymap = [
    { on = ".", run = "noop" },
]
"#,
        )
        .unwrap();
        cfg_noop.keymap.merge_overlay(&ov_unbind_dot);
        assert!(cfg_noop.keymap.global.key_bindings.iter().any(|b| {
            b.run == KeyAction::ContextMenu && b.on == ["<S-F10>"] && b.show_hint == Some(true)
        }));

        let mut cfg_noop_disabled = AppConfig::load_defaults();
        let ov_hide_then_unbind: KeymapConfigOverlay = toml::from_str(
            r#"
[global]
prepend_keymap = [
    { on = "<S-F10>", run = "context_menu", show_hint = false },
    { on = "<Menu>", run = "context_menu", show_hint = false },
]
"#,
        )
        .unwrap();
        cfg_noop_disabled.keymap.merge_overlay(&ov_hide_then_unbind);
        // `<S-F10>` and `<Menu>` are `Some(false)`, so unbinding `.` must NOT promote them.
        cfg_noop_disabled.keymap.merge_overlay(&ov_unbind_dot);
        assert!(
            !cfg_noop_disabled
                .keymap
                .global
                .key_bindings
                .iter()
                .any(|b| b.run == KeyAction::ContextMenu && b.show_hint == Some(true))
        );

        let mut cfg_unbind_all = AppConfig::load_defaults();
        let ov_all_noop: KeymapConfigOverlay = toml::from_str(
            r#"
[global]
prepend_keymap = [
    { on = ".", run = "noop" },
    { on = "<S-F10>", run = "noop" },
    { on = "<Menu>", run = "noop" },
]
"#,
        )
        .unwrap();
        cfg_unbind_all.keymap.merge_overlay(&ov_all_noop);
        assert!(
            !cfg_unbind_all
                .keymap
                .global
                .key_bindings
                .iter()
                .any(|b| b.run == KeyAction::ContextMenu)
        );

        // 5. Vim [diff] marks <Space> -> ToggleStage as show_hint = Some(false) while keeping it bound
        let vim = KeymapConfig::for_preset(KeymapPreset::Vim);
        assert!(vim.diff.key_bindings.iter().any(|b| {
            b.run == KeyAction::ToggleStage && b.on == ["<Space>"] && b.show_hint == Some(false)
        }));
        assert!(vim.diff.key_bindings.iter().any(|b| {
            b.run == KeyAction::StageSelection && b.on == ["s"] && b.show_hint == Some(true)
        }));
        assert!(vim.diff.key_bindings.iter().any(|b| {
            b.run == KeyAction::UnstageSelection && b.on == ["u"] && b.show_hint == Some(true)
        }));
    }

    #[test]
    fn test_annotate_config_defaults_and_overlay() {
        use jbgit_config::MoveDetectionPref;
        let mut cfg = AppConfig::load_defaults();
        assert!(!cfg.jbgit.annotate.ignore_whitespace);
        assert_eq!(cfg.jbgit.annotate.detect_moves, MoveDetectionPref::None);
        assert!(!cfg.jbgit.annotate.prefer_commit_date);
        assert_eq!(cfg.jbgit.annotate.gutter_width, 28);

        let ov: JbgitConfigOverlay = toml::from_str(
            r#"
[annotate]
ignore_whitespace = true
detect_moves = true
prefer_commit_date = true
gutter_width = 40
"#,
        )
        .unwrap();
        cfg.jbgit.merge_overlay(&ov);
        assert!(cfg.jbgit.annotate.ignore_whitespace);
        assert_eq!(
            cfg.jbgit.annotate.detect_moves,
            MoveDetectionPref::AcrossFiles
        );
        assert!(cfg.jbgit.annotate.prefer_commit_date);
        assert_eq!(cfg.jbgit.annotate.gutter_width, 40);

        let within_ov: JbgitConfigOverlay = toml::from_str(
            r#"
[annotate]
detect_moves = "within-file"
"#,
        )
        .unwrap();
        cfg.jbgit.merge_overlay(&within_ov);
        assert_eq!(
            cfg.jbgit.annotate.detect_moves,
            MoveDetectionPref::WithinFile
        );

        let clamp_ov: JbgitConfigOverlay = toml::from_str(
            r#"
[annotate]
gutter_width = 4
"#,
        )
        .unwrap();
        cfg.jbgit.merge_overlay(&clamp_ov);
        assert!(cfg.jbgit.annotate.ignore_whitespace);
        assert_eq!(cfg.jbgit.annotate.gutter_width, 16);
    }

    #[test]
    fn test_embedded_keymap_presets_have_no_unknown_actions() {
        use crate::keymap::KeyAction;

        let default_cfg: KeymapConfig = toml::from_str(BUILTIN_KEYMAP_DEFAULT_TOML).unwrap();
        for list in [
            &default_cfg.global,
            &default_cfg.log,
            &default_cfg.log_changes,
            &default_cfg.commit,
            &default_cfg.branch,
            &default_cfg.diff,
            &default_cfg.stash,
            &default_cfg.console,
        ] {
            for b in &list.key_bindings {
                assert!(
                    !matches!(b.run, KeyAction::Unknown(_)),
                    "unknown: {:?}",
                    b.run
                );
            }
        }

        for preset_toml in [BUILTIN_KEYMAP_JETBRAINS_TOML, BUILTIN_KEYMAP_VIM_TOML] {
            let ov: KeymapConfigOverlay = toml::from_str(preset_toml).unwrap();
            for sec in [
                &ov.global,
                &ov.log,
                &ov.log_changes,
                &ov.commit,
                &ov.branch,
                &ov.diff,
                &ov.stash,
                &ov.console,
            ]
            .into_iter()
            .flatten()
            {
                for b in sec
                    .key_bindings
                    .iter()
                    .chain(sec.prepend_keymap.iter())
                    .chain(sec.append_keymap.iter())
                {
                    assert!(
                        !matches!(b.run, KeyAction::Unknown(_)),
                        "unknown action in preset: {:?}",
                        b.run
                    );
                }
            }
        }
    }

    #[test]
    fn test_init_config_load_roundtrip_and_preset_switch() {
        use crate::keymap::KeyAction;

        let dir = tempfile::tempdir().unwrap();
        AppConfig::init_config(Some(dir.path()), false).unwrap();

        let loaded = AppConfig::load_from_layers(&[(dir.path(), false)]);
        let mut defaults = AppConfig::load_defaults();
        // `jbgit.default.toml` has `default_parent = ""`, which `merge_overlay` normalizes to `None`.
        defaults.jbgit.worktree.default_parent = None;
        assert_eq!(loaded.jbgit, defaults.jbgit);
        assert_eq!(loaded.keymap, defaults.keymap);
        assert_eq!(loaded.theme.flavor, defaults.theme.flavor);
        assert_eq!(loaded.theme.glyphs, defaults.theme.glyphs);
        assert_eq!(loaded.theme.palette, defaults.theme.palette);
        assert_eq!(loaded.theme.refs, defaults.theme.refs);
        assert_eq!(loaded.theme.graph, defaults.theme.graph);
        let loaded_pal =
            crate::theme::ThemePalette::from_config_with_truecolor(&loaded.theme, true);
        let default_pal =
            crate::theme::ThemePalette::from_config_with_truecolor(&defaults.theme, true);
        assert_eq!(loaded_pal.bg_canvas, default_pal.bg_canvas);
        assert_eq!(loaded_pal.fg_primary, default_pal.fg_primary);
        assert_eq!(loaded_pal.track_rgbs, default_pal.track_rgbs);
        assert_eq!(loaded_pal.ref_head, default_pal.ref_head);

        // Switching `preset = "jetbrains"` directly in `jbgit.toml` activates JetBrains bindings & tree_speed_search.
        fs::write(
            dir.path().join("jbgit.toml"),
            "[general]\nflavor = \"catppuccin-mocha\"\n\n[keymap]\npreset = \"jetbrains\"\nfooter_max_lines = 2\n",
        )
        .unwrap();
        let jb = AppConfig::load_from_layers(&[(dir.path(), false)]);
        assert_eq!(jb.theme.flavor, ThemeFlavor::CatppuccinMocha);
        assert_eq!(jb.keymap.preset, KeymapPreset::Jetbrains);
        assert!(jb.keymap.tree_speed_search);
        assert_eq!(jb.keymap.footer_max_lines, 2);
        assert_eq!(
            jb.keymap
                .primary_key_for(&[KeymapScope::Global], &KeyAction::GitPush)
                .as_deref(),
            Some("Ctrl+Shift+K")
        );
        assert_eq!(
            jb.keymap
                .primary_key_for(&[KeymapScope::Global], &KeyAction::TabCommit)
                .as_deref(),
            Some("Ctrl+K")
        );

        // Explicit `tree_speed_search = false` in `jbgit.toml` overrides the preset default.
        fs::write(
            dir.path().join("jbgit.toml"),
            "[keymap]\npreset = \"jetbrains\"\ntree_speed_search = false\n",
        )
        .unwrap();
        let jb_no_tss = AppConfig::load_from_layers(&[(dir.path(), false)]);
        assert_eq!(jb_no_tss.keymap.preset, KeymapPreset::Jetbrains);
        assert!(!jb_no_tss.keymap.tree_speed_search);
    }

    #[test]
    fn test_disk_theme_flavor_override_and_custom_flavor() {
        let global_dir = tempfile::tempdir().unwrap();
        let repo_dir = tempfile::tempdir().unwrap();
        AppConfig::init_config(Some(global_dir.path()), false).unwrap();

        // Override darcula in global themes/darcula.toml (palette, refs, graph.tracks)
        fs::create_dir_all(global_dir.path().join("themes")).unwrap();
        fs::write(
            global_dir.path().join("themes/darcula.toml"),
            r##"
[palette]
bg_canvas = "#112233"

[refs]
head = { icon = "#AABBCC" }

[graph]
tracks = [
    { fg = "#123456", muted_bg = "#102030" },
]
"##,
        )
        .unwrap();

        // Repo layer overlays darcula bg_panel on top of global darcula.toml
        fs::create_dir_all(repo_dir.path().join("themes")).unwrap();
        fs::write(
            repo_dir.path().join("themes/darcula.toml"),
            r##"
[palette]
bg_panel = "#223344"
"##,
        )
        .unwrap();
        // theme.toml overrides win over themes/darcula.toml
        fs::write(
            repo_dir.path().join("theme.toml"),
            r##"
[palette]
fg_primary = "#EEEEEE"
"##,
        )
        .unwrap();

        let cfg =
            AppConfig::load_from_layers(&[(global_dir.path(), false), (repo_dir.path(), true)]);
        let pal = crate::theme::ThemePalette::from_config_with_truecolor(&cfg.theme, true);
        assert_eq!(pal.bg_canvas, ratatui::style::Color::Rgb(0x11, 0x22, 0x33));
        assert_eq!(pal.bg_canvas_rgb, (0x11, 0x22, 0x33));
        assert_eq!(pal.bg_panel, ratatui::style::Color::Rgb(0x22, 0x33, 0x44));
        assert_eq!(pal.fg_primary, ratatui::style::Color::Rgb(0xEE, 0xEE, 0xEE));
        assert_eq!(
            pal.ref_head.icon,
            ratatui::style::Color::Rgb(0xAA, 0xBB, 0xCC)
        );
        assert_eq!(pal.track_rgbs[0], ((0x12, 0x34, 0x56), (0x10, 0x20, 0x30)));
        // Non-overridden track 1 stays Darcula
        assert_eq!(pal.track_rgbs[1], ((0x57, 0xD1, 0x85), (0x37, 0x52, 0x39)));

        // Custom flavor `ocean` with partial fields falls back to Darcula for omitted keys
        fs::write(
            global_dir.path().join("themes/ocean.toml"),
            r##"
[palette]
bg_canvas = "#010203"
border_focused = "#040506"
"##,
        )
        .unwrap();
        fs::write(repo_dir.path().join("theme.toml"), "flavor = \"ocean\"\n").unwrap();
        let ocean_cfg =
            AppConfig::load_from_layers(&[(global_dir.path(), false), (repo_dir.path(), true)]);
        assert_eq!(
            ocean_cfg.theme.flavor,
            ThemeFlavor::Custom("ocean".to_string())
        );
        let ocean_pal =
            crate::theme::ThemePalette::from_config_with_truecolor(&ocean_cfg.theme, true);
        assert_eq!(
            ocean_pal.bg_canvas,
            ratatui::style::Color::Rgb(0x01, 0x02, 0x03)
        );
        assert_eq!(
            ocean_pal.border_focused,
            ratatui::style::Color::Rgb(0x04, 0x05, 0x06)
        );
        // Omitted fg_primary falls back to Darcula (#BCBEC4)
        assert_eq!(
            ocean_pal.fg_primary,
            ratatui::style::Color::Rgb(0xBC, 0xBE, 0xC4)
        );
    }

    #[test]
    fn test_disk_keymap_preset_override_and_custom_preset() {
        use crate::keymap::KeyAction;

        let dir = tempfile::tempdir().unwrap();
        let repo_dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("keymaps")).unwrap();
        fs::create_dir_all(repo_dir.path().join("keymaps")).unwrap();

        // 1. Overriding `keymaps/jetbrains.toml` replaces built-in jetbrains overlay:
        //    editing <C-t> changes it, and omitting <C-k> removes it cleanly.
        fs::write(
            dir.path().join("jbgit.toml"),
            "[keymap]\npreset = \"jetbrains\"\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("keymaps/jetbrains.toml"),
            r#"
tree_speed_search = true

[global]
prepend_keymap = [
    { on = "<C-t>", run = "update_project" },
]
"#,
        )
        .unwrap();
        let cfg = AppConfig::load_from_layers(&[(dir.path(), false)]);
        assert_eq!(cfg.keymap.preset, KeymapPreset::Jetbrains);
        assert!(cfg.keymap.tree_speed_search);
        assert_eq!(
            cfg.keymap
                .primary_key_for(&[KeymapScope::Global], &KeyAction::UpdateProject)
                .as_deref(),
            Some("Ctrl+T")
        );
        assert!(
            !cfg.keymap
                .global
                .key_bindings
                .iter()
                .any(|b| b.run == KeyAction::TabCommit && b.on == ["<C-k>"])
        );

        // 2. Overriding `keymaps/default.toml` replaces sections so deleted bindings stay removed.
        fs::write(
            dir.path().join("jbgit.toml"),
            "[keymap]\npreset = \"default\"\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("keymaps/default.toml"),
            r#"
[global]
key_bindings = [
    { on = "Q", run = "quit" },
]
"#,
        )
        .unwrap();
        let def_cfg = AppConfig::load_from_layers(&[(dir.path(), false)]);
        assert_eq!(def_cfg.keymap.global.key_bindings.len(), 1);
        assert_eq!(def_cfg.keymap.global.key_bindings[0].on, ["Q"]);
        // Unspecified `[log]` still keeps built-in default bindings
        assert!(!def_cfg.keymap.log.key_bindings.is_empty());

        // 3. Custom preset `my-keys` overlays on top of `keymaps/default.toml`, higher layer
        //    `keymaps/my-keys.toml` layers on top, and legacy `keymap.toml` is ignored.
        fs::remove_file(dir.path().join("keymaps/default.toml")).unwrap();
        fs::write(
            dir.path().join("keymaps/my-keys.toml"),
            r#"
tree_speed_search = true

[global]
prepend_keymap = [
    { on = "<F9>", run = "refresh" },
]
"#,
        )
        .unwrap();
        fs::write(
            dir.path().join("jbgit.toml"),
            "[keymap]\npreset = \"my-keys\"\n",
        )
        .unwrap();
        fs::write(
            repo_dir.path().join("keymaps/my-keys.toml"),
            r#"
[global]
prepend_keymap = [
    { on = "<F10>", run = "quit" },
]
"#,
        )
        .unwrap();
        fs::write(
            repo_dir.path().join("keymap.toml"),
            r#"
[global]
prepend_keymap = [
    { on = "<F11>", run = "help" },
]
"#,
        )
        .unwrap();
        let custom_cfg =
            AppConfig::load_from_layers(&[(dir.path(), false), (repo_dir.path(), true)]);
        assert_eq!(
            custom_cfg.keymap.preset,
            KeymapPreset::Custom("my-keys".to_string())
        );
        assert!(custom_cfg.keymap.tree_speed_search);
        assert_eq!(
            custom_cfg
                .keymap
                .primary_key_for(&[KeymapScope::Global], &KeyAction::Refresh)
                .as_deref(),
            Some("F9")
        );
        assert_eq!(
            custom_cfg
                .keymap
                .primary_key_for(&[KeymapScope::Global], &KeyAction::Quit)
                .as_deref(),
            Some("F10")
        );
        assert!(
            !custom_cfg
                .keymap
                .global
                .key_bindings
                .iter()
                .any(|b| b.on == ["<F11>"]),
            "legacy keymap.toml must not be loaded"
        );
    }

    #[test]
    fn test_safe_preset_filename_and_default_config_content() {
        assert_eq!(safe_preset_filename("darcula"), Some("darcula.toml".into()));
        assert_eq!(
            safe_preset_filename("catppuccin-mocha"),
            Some("catppuccin-mocha.toml".into())
        );
        assert_eq!(safe_preset_filename(""), None);
        assert_eq!(safe_preset_filename(".."), None);
        assert_eq!(safe_preset_filename("../secret"), None);
        assert_eq!(safe_preset_filename("a/b"), None);
        assert_eq!(safe_preset_filename(r"a\b"), None);
        assert_eq!(safe_preset_filename("C:foo"), None);

        assert_eq!(AppConfig::default_config_content(None), DEFAULT_JBGIT_TOML);
        assert_eq!(
            AppConfig::default_config_content(Some("theme")),
            BUILTIN_THEME_DARCULA_TOML
        );
        assert_eq!(
            AppConfig::default_config_content(Some("keymap")),
            BUILTIN_KEYMAP_DEFAULT_TOML
        );
        assert_eq!(
            AppConfig::default_config_content(Some("darcula")),
            BUILTIN_THEME_DARCULA_TOML
        );
        assert_eq!(
            AppConfig::default_config_content(Some("themes/catppuccin-mocha.toml")),
            BUILTIN_THEME_CATPPUCCIN_MOCHA_TOML
        );
        assert_eq!(
            AppConfig::default_config_content(Some("gruvbox-dark")),
            BUILTIN_THEME_GRUVBOX_DARK_TOML
        );
        assert_eq!(
            AppConfig::default_config_content(Some("keymaps/default")),
            BUILTIN_KEYMAP_DEFAULT_TOML
        );
        assert_eq!(
            AppConfig::default_config_content(Some("jetbrains")),
            BUILTIN_KEYMAP_JETBRAINS_TOML
        );
        assert_eq!(
            AppConfig::default_config_content(Some("vim")),
            BUILTIN_KEYMAP_VIM_TOML
        );
    }

    #[test]
    fn test_worktree_default_parent_deserializes_empty_as_none() {
        let defaults = AppConfig::load_defaults();
        assert_eq!(defaults.jbgit.worktree.default_parent, None);

        let parsed_empty: JbgitConfig =
            toml::from_str("[worktree]\ndefault_parent = \"\"\n").unwrap();
        assert_eq!(parsed_empty.worktree.default_parent, None);

        let parsed_ws: JbgitConfig =
            toml::from_str("[worktree]\ndefault_parent = \"   \"\n").unwrap();
        assert_eq!(parsed_ws.worktree.default_parent, None);

        let parsed_set: JbgitConfig =
            toml::from_str("[worktree]\ndefault_parent = \"/tmp/wt\"\n").unwrap();
        assert_eq!(
            parsed_set.worktree.default_parent.as_deref(),
            Some("/tmp/wt")
        );
    }
}
