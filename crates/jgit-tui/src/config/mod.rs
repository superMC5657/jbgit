pub mod discovery;
pub mod jgit_config;
pub mod keymap_config;
pub mod theme_config;

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::de::DeserializeOwned;

pub use discovery::{JGIT_CONFIG_DIR_ENV, find_config_file, global_config_dir};
pub use jgit_config::{
    DefaultTab, GlyphModePref, GraphBackendPref, JgitConfig, JgitConfigOverlay, LanguagePref,
    ThemeFlavor,
};
pub use keymap_config::{KeyBindingConfig, KeymapConfig, KeymapConfigOverlay};
pub use theme_config::{ThemeConfig, ThemeConfigOverlay};

pub const DEFAULT_JGIT_TOML: &str = include_str!("defaults/jgit.default.toml");
pub const DEFAULT_THEME_TOML: &str = include_str!("defaults/theme.default.toml");
pub const DEFAULT_KEYMAP_TOML: &str = include_str!("defaults/keymap.default.toml");

/// Root configuration aggregating all three sub-configurations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppConfig {
    pub jgit: JgitConfig,
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
    jgit_flavor_set: bool,
    theme_flavor_set: bool,
    jgit_glyph_set: bool,
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
        let jgit: JgitConfig = toml::from_str(DEFAULT_JGIT_TOML).unwrap_or_default();
        let theme: ThemeConfig = toml::from_str(DEFAULT_THEME_TOML).unwrap_or_default();
        let keymap: KeymapConfig = toml::from_str(DEFAULT_KEYMAP_TOML).unwrap_or_default();
        Self { jgit, theme, keymap }
    }

    /// Loads the configuration with layered precedence:
    /// Default -> Global User Config -> Repository Config -> JGIT_CONFIG_DIR
    pub fn load(workdir: Option<&Path>) -> Self {
        let mut config = Self::load_defaults();
        let mut flags = OverlayFlags::default();

        // Layer 1: Global user configuration
        let global_dir = global_config_dir();
        config.apply_dir_overlays(&global_dir, false, &mut flags);

        // Layer 2: Repo-level configuration (.jgit/)
        if let Some(wd) = workdir {
            let repo_jgit_dir = wd.join(".jgit");
            config.apply_dir_overlays(&repo_jgit_dir, true, &mut flags);
        }

        // Layer 3: JGIT_CONFIG_DIR environment variable (highest override)
        if let Some(env_dir) = std::env::var_os(JGIT_CONFIG_DIR_ENV) {
            config.apply_dir_overlays(Path::new(&env_dir), false, &mut flags);
        }

        // Honor jgit.toml `[general].flavor` / `[graph].glyph_mode` when theme.toml did not override them.
        if flags.jgit_flavor_set && !flags.theme_flavor_set {
            config.theme.flavor = config.jgit.general.flavor;
        }
        if flags.jgit_glyph_set && !flags.theme_glyph_set {
            config.theme.glyphs.mode = config.jgit.graph.glyph_mode;
        }

        config
    }

    fn apply_dir_overlays(&mut self, dir: &Path, allow_config_toml_alias: bool, flags: &mut OverlayFlags) {
        if allow_config_toml_alias {
            load_toml_overlay::<JgitConfigOverlay>(&dir.join("config.toml"), |ov| {
                if ov.general.as_ref().and_then(|g| g.flavor).is_some() {
                    flags.jgit_flavor_set = true;
                }
                if ov.graph.as_ref().and_then(|g| g.glyph_mode).is_some() {
                    flags.jgit_glyph_set = true;
                }
                self.jgit.merge_overlay(ov);
            });
        }
        load_toml_overlay::<JgitConfigOverlay>(&dir.join("jgit.toml"), |ov| {
            if ov.general.as_ref().and_then(|g| g.flavor).is_some() {
                flags.jgit_flavor_set = true;
            }
            if ov.graph.as_ref().and_then(|g| g.glyph_mode).is_some() {
                flags.jgit_glyph_set = true;
            }
            self.jgit.merge_overlay(ov);
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
                print!("{DEFAULT_JGIT_TOML}");
            }
        }
    }

    /// Initializes default configuration files in the destination directory.
    pub fn init_config(custom_dir: Option<&Path>, force: bool) -> Result<PathBuf> {
        let dir = match custom_dir {
            Some(d) => d.to_path_buf(),
            None => global_config_dir(),
        };

        fs::create_dir_all(&dir)
            .with_context(|| format!("Failed to create config directory at {:?}", dir))?;

        let files = [
            ("jgit.toml", DEFAULT_JGIT_TOML),
            ("theme.toml", DEFAULT_THEME_TOML),
            ("keymap.toml", DEFAULT_KEYMAP_TOML),
        ];

        for (name, content) in files {
            let file_path = dir.join(name);
            if file_path.exists() && !force {
                println!("File already exists, skipping: {:?}", file_path);
            } else {
                fs::write(&file_path, content)
                    .with_context(|| format!("Failed to write {:?}", file_path))?;
                println!("Generated configuration file: {:?}", file_path);
            }
        }

        Ok(dir)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_load_defaults() {
        let cfg = AppConfig::load_defaults();
        assert_eq!(cfg.jgit.general.language, LanguagePref::ZhCn);
        assert_eq!(cfg.jgit.general.default_tab, DefaultTab::Log);
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
        let overlay: JgitConfigOverlay = toml::from_str(overlay_toml).unwrap();
        cfg.jgit.merge_overlay(&overlay);

        assert_eq!(cfg.jgit.general.language, LanguagePref::EnUs);
        assert_eq!(cfg.jgit.general.default_tab, DefaultTab::Commit);
        assert_eq!(cfg.jgit.diff.tab_size, 2);
        // Unspecified keys retain defaults
        assert_eq!(cfg.jgit.diff.context_lines, 3);
        assert!(cfg.jgit.diff.syntax_highlight);
    }

    #[test]
    fn test_init_config_creates_files() {
        let temp = tempfile::tempdir().unwrap();
        let target_dir = temp.path().join("my_config");

        let res = AppConfig::init_config(Some(&target_dir), false).unwrap();
        assert_eq!(res, target_dir);

        assert!(target_dir.join("jgit.toml").is_file());
        assert!(target_dir.join("theme.toml").is_file());
        assert!(target_dir.join("keymap.toml").is_file());

        let jgit_content = fs::read_to_string(target_dir.join("jgit.toml")).unwrap();
        assert!(jgit_content.contains("language = \"zh-CN\""));
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
        // Preserves existing bindings while replacing `refresh` (`r` -> `<F5>`)
        assert_eq!(cfg.keymap.global.key_bindings.len(), initial_global_len);
        assert!(cfg.keymap.global.key_bindings.iter().any(|b| b.run == "quit"));
        assert!(!cfg.keymap.global.key_bindings.iter().any(|b| b.run == "refresh" && b.on == ["r"]));
        assert!(cfg.keymap.global.key_bindings.iter().any(|b| b.run == "refresh" && b.on == ["<F5>"]));
    }

    #[test]
    fn test_repo_config_toml_alias_and_flavor_fallback() {
        let temp = tempfile::tempdir().unwrap();
        let jgit_dir = temp.path().join(".jgit");
        fs::create_dir_all(&jgit_dir).unwrap();
        fs::write(
            jgit_dir.join("config.toml"),
            r#"
[general]
flavor = "gruvbox-dark"
default_tab = "branches"
"#,
        )
        .unwrap();

        let cfg = AppConfig::load(Some(temp.path()));
        assert_eq!(cfg.jgit.general.default_tab, DefaultTab::Branches);
        assert_eq!(cfg.theme.flavor, ThemeFlavor::GruvboxDark);
    }
}
