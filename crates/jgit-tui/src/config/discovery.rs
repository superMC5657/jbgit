use std::path::{Path, PathBuf};

/// Environment variable for overriding the configuration directory.
pub const JGIT_CONFIG_DIR_ENV: &str = "JGIT_CONFIG_DIR";

/// Returns the default global configuration directory for jGit on the current platform.
///
/// - Windows: `%APPDATA%\jgit\config` (falls back to `~/.config/jgit`)
/// - Unix/macOS: `$XDG_CONFIG_HOME/jgit` (falls back to `~/.config/jgit`)
pub fn global_config_dir() -> PathBuf {
    #[cfg(windows)]
    {
        if let Some(appdata) = std::env::var_os("APPDATA") {
            let mut p = PathBuf::from(appdata);
            p.push("jgit");
            p.push("config");
            return p;
        }
        if let Some(userprofile) = std::env::var_os("USERPROFILE") {
            let mut p = PathBuf::from(userprofile);
            p.push(".config");
            p.push("jgit");
            return p;
        }
        PathBuf::from(".config/jgit")
    }

    #[cfg(not(windows))]
    {
        if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
            let mut p = PathBuf::from(xdg);
            p.push("jgit");
            return p;
        }
        if let Some(home) = std::env::var_os("HOME") {
            let mut p = PathBuf::from(home);
            p.push(".config");
            p.push("jgit");
            return p;
        }
        PathBuf::from(".config/jgit")
    }
}

/// Discovers a configuration file according to the precedence rules:
/// 1. `$JGIT_CONFIG_DIR/<filename>`
/// 2. `<workdir>/.jgit/<filename>` (or `<workdir>/.jgit/config.toml` when `filename == "jgit.toml"`)
/// 3. `<global_config_dir>/<filename>`
pub fn find_config_file(filename: &str, workdir: Option<&Path>) -> Option<PathBuf> {
    // 1. Check JGIT_CONFIG_DIR
    if let Some(dir) = std::env::var_os(JGIT_CONFIG_DIR_ENV) {
        let p = PathBuf::from(dir).join(filename);
        if p.exists() {
            return Some(p);
        }
    }

    // 2. Check repo-level .jgit directory
    if let Some(wd) = workdir {
        let jgit_dir = wd.join(".jgit");
        let p = jgit_dir.join(filename);
        if p.exists() {
            return Some(p);
        }
        if filename == "jgit.toml" {
            let alt = jgit_dir.join("config.toml");
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
