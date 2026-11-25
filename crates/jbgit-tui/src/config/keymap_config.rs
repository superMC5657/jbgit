use std::fmt;
use std::sync::OnceLock;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::keymap::KeyAction;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum KeymapPreset {
    #[default]
    Default,
    Jetbrains,
    Vim,
    Custom(String),
}

impl KeymapPreset {
    pub fn parse(s: &str) -> Self {
        let trimmed = s.trim();
        match trimmed.to_ascii_lowercase().as_str() {
            "" | "default" | "minimal" => Self::Default,
            "jetbrains" | "idea" => Self::Jetbrains,
            "vim" | "lazygit" => Self::Vim,
            _ => Self::Custom(trimmed.to_string()),
        }
    }

    pub fn as_str(&self) -> &str {
        match self {
            Self::Default => "default",
            Self::Jetbrains => "jetbrains",
            Self::Vim => "vim",
            Self::Custom(s) => s.as_str(),
        }
    }
}

impl Serialize for KeymapPreset {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for KeymapPreset {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Ok(Self::parse(&s))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeymapScope {
    Global,
    Log,
    Commit,
    Branch,
    Diff,
    Stash,
    Console,
}

fn deserialize_key_seq<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum OneOrMany {
        One(String),
        Many(Vec<String>),
    }

    match OneOrMany::deserialize(deserializer)? {
        OneOrMany::One(s) => Ok(vec![s]),
        OneOrMany::Many(v) => Ok(v),
    }
}

fn default_footer_max_lines() -> u16 {
    1
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct KeymapConfig {
    #[serde(default)]
    pub preset: KeymapPreset,
    #[serde(default)]
    pub tree_speed_search: bool,
    #[serde(default = "default_footer_max_lines")]
    pub footer_max_lines: u16,
    #[serde(default)]
    pub global: KeyBindingsList,
    #[serde(default)]
    pub log: KeyBindingsList,
    #[serde(default)]
    pub commit: KeyBindingsList,
    #[serde(default)]
    pub branch: KeyBindingsList,
    #[serde(default)]
    pub diff: KeyBindingsList,
    #[serde(default)]
    pub stash: KeyBindingsList,
    #[serde(default)]
    pub console: KeyBindingsList,
}

impl Default for KeymapConfig {
    fn default() -> Self {
        Self {
            preset: KeymapPreset::default(),
            tree_speed_search: false,
            footer_max_lines: default_footer_max_lines(),
            global: KeyBindingsList::default(),
            log: KeyBindingsList::default(),
            commit: KeyBindingsList::default(),
            branch: KeyBindingsList::default(),
            diff: KeyBindingsList::default(),
            stash: KeyBindingsList::default(),
            console: KeyBindingsList::default(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct KeyBindingsList {
    #[serde(default)]
    pub key_bindings: Vec<KeyBindingConfig>,
    #[serde(default)]
    pub prepend_keymap: Vec<KeyBindingConfig>,
    #[serde(default)]
    pub append_keymap: Vec<KeyBindingConfig>,
}

#[derive(Default)]
struct ParsedKeyCache(OnceLock<Option<(crate::keymap::KeySequence, String)>>);

impl fmt::Debug for ParsedKeyCache {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("ParsedKeyCache")
            .field(&self.0.get().is_some())
            .finish()
    }
}

impl Clone for ParsedKeyCache {
    fn clone(&self) -> Self {
        let lock = OnceLock::new();
        if let Some(val) = self.0.get() {
            let _ = lock.set(val.clone());
        }
        Self(lock)
    }
}

impl PartialEq for ParsedKeyCache {
    fn eq(&self, _other: &Self) -> bool {
        true
    }
}

impl Eq for ParsedKeyCache {}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct KeyBindingConfig {
    #[serde(deserialize_with = "deserialize_key_seq")]
    pub on: Vec<String>,
    pub run: KeyAction,
    #[serde(default)]
    pub desc: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub show_hint: Option<bool>,
    #[serde(skip)]
    parsed: ParsedKeyCache,
}

impl KeyBindingConfig {
    fn ensure_parsed(&self) -> Option<&(crate::keymap::KeySequence, String)> {
        self.parsed
            .0
            .get_or_init(|| {
                crate::keymap::KeySequence::parse(&self.on).map(|seq| {
                    let disp = seq.to_display_string();
                    (seq, disp)
                })
            })
            .as_ref()
    }

    pub fn parsed_seq(&self) -> Option<&crate::keymap::KeySequence> {
        self.ensure_parsed().map(|(seq, _)| seq)
    }

    pub fn display_key(&self) -> Option<&str> {
        self.ensure_parsed().map(|(_, disp)| disp.as_str())
    }

    pub fn precompile(&self) {
        let _ = self.ensure_parsed();
    }

    #[cfg(test)]
    pub fn is_precompiled(&self) -> bool {
        self.parsed.0.get().is_some()
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct KeymapConfigOverlay {
    pub preset: Option<KeymapPreset>,
    pub tree_speed_search: Option<bool>,
    pub footer_max_lines: Option<u16>,
    pub global: Option<KeyBindingsList>,
    pub log: Option<KeyBindingsList>,
    pub commit: Option<KeyBindingsList>,
    pub branch: Option<KeyBindingsList>,
    pub diff: Option<KeyBindingsList>,
    pub stash: Option<KeyBindingsList>,
    pub console: Option<KeyBindingsList>,
}

fn keys_equal(a: &[String], b: &[String]) -> bool {
    if a == b {
        return true;
    }
    match (
        crate::keymap::KeySequence::parse(a),
        crate::keymap::KeySequence::parse(b),
    ) {
        (Some(sa), Some(sb)) => sa == sb,
        _ => false,
    }
}

fn is_active_run(run: &KeyAction) -> bool {
    run.is_active()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MergeMode {
    Replace,
    Prepend,
    Append,
}

fn find_exact_binding<'a>(
    base: &'a [KeyBindingConfig],
    entry: &KeyBindingConfig,
) -> Option<&'a KeyBindingConfig> {
    base.iter()
        .find(|existing| keys_equal(&existing.on, &entry.on) && existing.run == entry.run)
}

fn push_unique_action(list: &mut Vec<KeyAction>, action: &KeyAction) {
    if !list.contains(action) {
        list.push(action.clone());
    }
}

fn demote_inherited_hints(base: &mut [KeyBindingConfig], demote_runs: &[KeyAction]) {
    if demote_runs.is_empty() {
        return;
    }
    for existing in base {
        if existing.show_hint == Some(true) && demote_runs.contains(&existing.run) {
            existing.show_hint = None;
        }
    }
}

fn apply_overlay_entries(
    base: &mut Vec<KeyBindingConfig>,
    entries: &[KeyBindingConfig],
    mode: MergeMode,
) {
    if entries.is_empty() {
        return;
    }

    let base_has_hint: Vec<KeyAction> = base
        .iter()
        .filter(|b| is_active_run(&b.run) && b.show_hint == Some(true))
        .map(|b| b.run.clone())
        .collect();

    let mut demote_runs: Vec<KeyAction> = Vec::new();
    let mut handled_runs: Vec<KeyAction> = Vec::new();
    for ov in entries {
        if !is_active_run(&ov.run) {
            continue;
        }
        if ov.show_hint.is_some() {
            push_unique_action(&mut demote_runs, &ov.run);
            push_unique_action(&mut handled_runs, &ov.run);
        } else if find_exact_binding(base, ov).is_some_and(|e| e.show_hint.is_some()) {
            push_unique_action(&mut handled_runs, &ov.run);
        }
    }

    let mut resolved = Vec::with_capacity(entries.len());
    for ov in entries {
        if !is_active_run(&ov.run) {
            continue;
        }
        let mut entry = ov.clone();
        if entry.show_hint.is_none() {
            if let Some(exact) = find_exact_binding(base, &entry)
                && exact.show_hint.is_some()
            {
                entry.show_hint = exact.show_hint;
            } else if mode != MergeMode::Append
                && !handled_runs.contains(&entry.run)
                && base_has_hint.contains(&entry.run)
                && !demote_runs.contains(&entry.run)
            {
                entry.show_hint = Some(true);
                push_unique_action(&mut demote_runs, &entry.run);
            }
        }
        resolved.push(entry);
    }

    let mut orphaned_hint_runs: Vec<KeyAction> = Vec::new();
    base.retain(|existing| {
        let colliding = entries.iter().find(|ov| keys_equal(&ov.on, &existing.on));
        let remove = colliding.is_some()
            || (mode == MergeMode::Replace
                && entries
                    .iter()
                    .any(|ov| is_active_run(&ov.run) && ov.run == existing.run));
        if remove
            && existing.show_hint == Some(true)
            && is_active_run(&existing.run)
            && !demote_runs.contains(&existing.run)
            && colliding.is_none_or(|ov| ov.show_hint != Some(false))
        {
            push_unique_action(&mut orphaned_hint_runs, &existing.run);
        }
        !remove
    });

    demote_inherited_hints(base, &demote_runs);
    match mode {
        MergeMode::Replace | MergeMode::Append => base.extend(resolved),
        MergeMode::Prepend => {
            resolved.append(base);
            *base = resolved;
        }
    }

    for run in orphaned_hint_runs {
        if !base
            .iter()
            .any(|b| b.run == run && b.show_hint == Some(true))
            && let Some(survivor) = base
                .iter_mut()
                .find(|b| b.run == run && b.show_hint.is_none())
        {
            survivor.show_hint = Some(true);
        }
    }
}

fn merge_section(base: &mut Vec<KeyBindingConfig>, overlay: &KeyBindingsList) {
    apply_overlay_entries(base, &overlay.key_bindings, MergeMode::Replace);
    apply_overlay_entries(base, &overlay.prepend_keymap, MergeMode::Prepend);
    apply_overlay_entries(base, &overlay.append_keymap, MergeMode::Append);
}

fn apply_preset_section(base: &mut Vec<KeyBindingConfig>, overlay: &KeyBindingsList) {
    if !overlay.key_bindings.is_empty() {
        *base = overlay.key_bindings.clone();
    }
    apply_overlay_entries(base, &overlay.prepend_keymap, MergeMode::Prepend);
    apply_overlay_entries(base, &overlay.append_keymap, MergeMode::Append);
}

impl KeymapConfig {
    fn normalize_sections(&mut self) {
        for list in [
            &mut self.global,
            &mut self.log,
            &mut self.commit,
            &mut self.branch,
            &mut self.diff,
            &mut self.stash,
            &mut self.console,
        ] {
            let extra = KeyBindingsList {
                key_bindings: Vec::new(),
                prepend_keymap: std::mem::take(&mut list.prepend_keymap),
                append_keymap: std::mem::take(&mut list.append_keymap),
            };
            merge_section(&mut list.key_bindings, &extra);
        }
    }

    /// Applies a preset definition (`keymaps/<preset>.toml`), where an explicit
    /// `key_bindings` array replaces that scope's base bindings cleanly before
    /// applying `prepend_keymap` / `append_keymap`.
    pub(crate) fn apply_preset_overlay(&mut self, overlay: &KeymapConfigOverlay) {
        if let Some(tss) = overlay.tree_speed_search {
            self.tree_speed_search = tss;
        }
        if let Some(lines) = overlay.footer_max_lines {
            self.footer_max_lines = lines;
        }
        for (dst, src) in [
            (&mut self.global.key_bindings, &overlay.global),
            (&mut self.log.key_bindings, &overlay.log),
            (&mut self.commit.key_bindings, &overlay.commit),
            (&mut self.branch.key_bindings, &overlay.branch),
            (&mut self.diff.key_bindings, &overlay.diff),
            (&mut self.stash.key_bindings, &overlay.stash),
            (&mut self.console.key_bindings, &overlay.console),
        ] {
            if let Some(sec) = src {
                apply_preset_section(dst, sec);
            }
        }
        self.precompile_all();
    }

    pub fn for_preset(preset: KeymapPreset) -> Self {
        let mut cfg: KeymapConfig =
            toml::from_str(crate::config::BUILTIN_KEYMAP_DEFAULT_TOML).unwrap_or_default();
        cfg.preset = KeymapPreset::Default;
        cfg.normalize_sections();

        match &preset {
            KeymapPreset::Default => {}
            KeymapPreset::Jetbrains => {
                if let Ok(ov) = toml::from_str::<KeymapConfigOverlay>(
                    crate::config::BUILTIN_KEYMAP_JETBRAINS_TOML,
                ) {
                    cfg.apply_preset_overlay(&ov);
                }
                cfg.preset = KeymapPreset::Jetbrains;
            }
            KeymapPreset::Vim => {
                if let Ok(ov) =
                    toml::from_str::<KeymapConfigOverlay>(crate::config::BUILTIN_KEYMAP_VIM_TOML)
                {
                    cfg.apply_preset_overlay(&ov);
                }
                cfg.preset = KeymapPreset::Vim;
            }
            KeymapPreset::Custom(_) => {
                cfg.preset = preset;
            }
        }
        cfg.precompile_all();
        cfg
    }

    pub fn precompile_all(&self) {
        for scope in [
            KeymapScope::Global,
            KeymapScope::Log,
            KeymapScope::Commit,
            KeymapScope::Branch,
            KeymapScope::Diff,
            KeymapScope::Stash,
            KeymapScope::Console,
        ] {
            for b in self.bindings_for(scope) {
                b.precompile();
            }
        }
    }

    pub fn merge_overlay(&mut self, overlay: &KeymapConfigOverlay) {
        if let Some(p) = &overlay.preset
            && p != &self.preset
        {
            let prev_footer_lines = self.footer_max_lines;
            *self = Self::for_preset(p.clone());
            self.footer_max_lines = prev_footer_lines;
        }
        self.merge_overlay_ignoring_preset(overlay);
    }

    pub(crate) fn merge_overlay_ignoring_preset(&mut self, overlay: &KeymapConfigOverlay) {
        if let Some(tss) = overlay.tree_speed_search {
            self.tree_speed_search = tss;
        }
        if let Some(lines) = overlay.footer_max_lines {
            self.footer_max_lines = lines;
        }
        if let Some(g) = &overlay.global {
            merge_section(&mut self.global.key_bindings, g);
        }
        if let Some(l) = &overlay.log {
            merge_section(&mut self.log.key_bindings, l);
        }
        if let Some(c) = &overlay.commit {
            merge_section(&mut self.commit.key_bindings, c);
        }
        if let Some(b) = &overlay.branch {
            merge_section(&mut self.branch.key_bindings, b);
        }
        if let Some(d) = &overlay.diff {
            merge_section(&mut self.diff.key_bindings, d);
        }
        if let Some(s) = &overlay.stash {
            merge_section(&mut self.stash.key_bindings, s);
        }
        if let Some(c) = &overlay.console {
            merge_section(&mut self.console.key_bindings, c);
        }
        self.precompile_all();
    }

    pub fn bindings_for(&self, scope: KeymapScope) -> &[KeyBindingConfig] {
        match scope {
            KeymapScope::Global => &self.global.key_bindings,
            KeymapScope::Log => &self.log.key_bindings,
            KeymapScope::Commit => &self.commit.key_bindings,
            KeymapScope::Branch => &self.branch.key_bindings,
            KeymapScope::Diff => &self.diff.key_bindings,
            KeymapScope::Stash => &self.stash.key_bindings,
            KeymapScope::Console => &self.console.key_bindings,
        }
    }

    pub fn primary_key_for(&self, scopes: &[KeymapScope], run: &KeyAction) -> Option<String> {
        for &scope in scopes {
            for b in self.bindings_for(scope) {
                if &b.run == run
                    && let Some(disp) = b.display_key()
                {
                    return Some(disp.to_string());
                }
            }
        }
        None
    }

    pub fn all_keys_for(&self, scopes: &[KeymapScope], run: &KeyAction) -> Vec<String> {
        let mut out = Vec::new();
        for &scope in scopes {
            for b in self.bindings_for(scope) {
                if &b.run == run
                    && let Some(disp) = b.display_key()
                    && !out.iter().any(|existing| existing == disp)
                {
                    out.push(disp.to_string());
                }
            }
        }
        out
    }

    pub fn editor_keys_for(&self, scopes: &[KeymapScope], run: &KeyAction) -> Vec<String> {
        let mut out = Vec::new();
        for &scope in scopes {
            for b in self.bindings_for(scope) {
                if &b.run == run
                    && let Some(seq) = b.parsed_seq()
                    && seq.starts_with_editor_chord()
                    && let Some(disp) = b.display_key()
                    && !out.iter().any(|existing| existing == disp)
                {
                    out.push(disp.to_string());
                }
            }
        }
        out
    }

    pub fn primary_editor_key_for(
        &self,
        scopes: &[KeymapScope],
        run: &KeyAction,
    ) -> Option<String> {
        self.editor_keys_for(scopes, run).into_iter().next()
    }

    /// Returns the primary non-shadowed single-stroke editor-chord sequence for `run` across `scopes`.
    pub fn primary_single_editor_seq_for(
        &self,
        scopes: &[KeymapScope],
        run: &KeyAction,
    ) -> Option<crate::keymap::KeySequence> {
        let mut shadowed = Vec::new();
        for &scope in scopes {
            for b in self.bindings_for(scope) {
                if !b.run.is_active() {
                    continue;
                }
                let Some(seq) = b.parsed_seq() else {
                    continue;
                };
                if !seq.starts_with_editor_chord() {
                    continue;
                }
                let first = &seq.strokes[0];
                if &b.run == run && seq.strokes.len() == 1 {
                    if !shadowed.contains(first) {
                        return Some(seq.clone());
                    }
                } else if !shadowed.contains(first) {
                    shadowed.push(first.clone());
                }
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn precompile_all_populates_parsed_key_cache() {
        let cfg = KeymapConfig::default();
        cfg.precompile_all();
        for scope in [
            KeymapScope::Global,
            KeymapScope::Log,
            KeymapScope::Commit,
            KeymapScope::Branch,
            KeymapScope::Diff,
            KeymapScope::Stash,
            KeymapScope::Console,
        ] {
            for b in cfg.bindings_for(scope) {
                assert!(
                    b.is_precompiled(),
                    "binding for {:?} with on={:?} must be precompiled",
                    b.run,
                    b.on
                );
                assert!(b.parsed_seq().is_some());
                assert!(b.display_key().is_some());
            }
        }
    }
}
