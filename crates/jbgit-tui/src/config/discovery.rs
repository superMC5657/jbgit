use std::path::{Path, PathBuf};

/// Environment variable for overriding the configuration directory.
pub const JBGIT_CONFIG_DIR_ENV: &str = "JBGIT_CONFIG_DIR";

/// Returns the default global configuration directory for jbgit on the current platform.
///
/// - Windows: `%APPDATA%\jbgit\config` (falls back to `~/.config/jbgit`)
/// - Unix/macOS: `$XDG_CONFIG_HOME/jbgit` (falls back to `~/.config/jbgit`)
pub fn global_config_dir() -> PathBuf {
    #[cfg(windows)]
    {
        if let Some(appdata) = std::env::var_os("APPDATA") {
            let mut p = PathBuf::from(appdata);
            p.push("jbgit");
            p.push("config");
            return p;
        }
        if let Some(userprofile) = std::env::var_os("USERPROFILE") {
            let mut p = PathBuf::from(userprofile);
            p.push(".config");
            p.push("jbgit");
            return p;
        }
        PathBuf::from(".config/jbgit")
    }

    #[cfg(not(windows))]
    {
        if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
            let mut p = PathBuf::from(xdg);
            p.push("jbgit");
            return p;
        }
        if let Some(home) = std::env::var_os("HOME") {
            let mut p = PathBuf::from(home);
            p.push(".config");
            p.push("jbgit");
            return p;
        }
        PathBuf::from(".config/jbgit")
    }
}

/// Discovers a configuration file according to the precedence rules:
/// 1. `$JBGIT_CONFIG_DIR/<filename>`
/// 2. `<workdir>/.jbgit/<filename>` (or `<workdir>/.jbgit/config.toml` when `filename == "jbgit.toml"`)
/// 3. `<global_config_dir>/<filename>`
pub fn find_config_file(filename: &str, workdir: Option<&Path>) -> Option<PathBuf> {
    // 1. Check JBGIT_CONFIG_DIR
    if let Some(dir) = std::env::var_os(JBGIT_CONFIG_DIR_ENV) {
        let p = PathBuf::from(dir).join(filename);
        if p.exists() {
            return Some(p);
        }
    }

    // 2. Check repo-level .jbgit directory
    if let Some(wd) = workdir {
        let jbgit_dir = wd.join(".jbgit");
        let p = jbgit_dir.join(filename);
        if p.exists() {
            return Some(p);
        }
        if filename == "jbgit.toml" {
            let alt = jbgit_dir.join("config.toml");
            if alt.is_file() {
                return Some(alt);
            }
        }
    }

    // 3. Check global user config directory
    let global_p = global_config_dir().join(filename);
    if global_p.exists() {
        return Some(global_p);
    }

    None
}
