use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::keymap::KeyAction;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum KeymapPreset {
    #[default]
    Default,
    Jetbrains,
    Vim,
}

impl KeymapPreset {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "default" | "minimal" => Some(Self::Default),
            "jetbrains" | "idea" => Some(Self::Jetbrains),
            "vim" | "lazygit" => Some(Self::Vim),
            _ => None,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Jetbrains => "jetbrains",
            Self::Vim => "vim",
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
        Ok(Self::parse(&s).unwrap_or_default())
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

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct KeymapConfig {
    #[serde(default)]
    pub preset: KeymapPreset,
    #[serde(default)]
    pub tree_speed_search: bool,
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

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct KeyBindingsList {
    #[serde(default)]
    pub key_bindings: Vec<KeyBindingConfig>,
    #[serde(default)]
    pub prepend_keymap: Vec<KeyBindingConfig>,
    #[serde(default)]
    pub append_keymap: Vec<KeyBindingConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct KeyBindingConfig {
    #[serde(deserialize_with = "deserialize_key_seq")]
    pub on: Vec<String>,
    pub run: KeyAction,
    #[serde(default)]
    pub desc: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct KeymapConfigOverlay {
    pub preset: Option<KeymapPreset>,
    pub tree_speed_search: Option<bool>,
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

fn merge_section(base: &mut Vec<KeyBindingConfig>, overlay: &KeyBindingsList) {
    if !overlay.key_bindings.is_empty() {
        base.retain(|existing| {
            !overlay.key_bindings.iter().any(|ov| {
                keys_equal(&ov.on, &existing.on)
                    || (is_active_run(&ov.run) && ov.run == existing.run)
            })
        });
        base.extend(
            overlay
                .key_bindings
                .iter()
                .filter(|ov| is_active_run(&ov.run))
                .cloned(),
        );
    }

    if !overlay.prepend_keymap.is_empty() {
        base.retain(|existing| {
            !overlay
                .prepend_keymap
                .iter()
                .any(|ov| keys_equal(&ov.on, &existing.on))
        });
        let mut prepended: Vec<KeyBindingConfig> = overlay
            .prepend_keymap
            .iter()
            .filter(|ov| is_active_run(&ov.run))
            .cloned()
            .collect();
        prepended.append(base);
        *base = prepended;
    }

    if !overlay.append_keymap.is_empty() {
        base.retain(|existing| {
            !overlay
                .append_keymap
                .iter()
                .any(|ov| keys_equal(&ov.on, &existing.on))
        });
        base.extend(
            overlay
                .append_keymap
                .iter()
                .filter(|ov| is_active_run(&ov.run))
                .cloned(),
        );
    }
}

fn kb(on: &[&str], run: KeyAction, desc: Option<&str>) -> KeyBindingConfig {
    KeyBindingConfig {
        on: on.iter().map(|s| (*s).to_string()).collect(),
        run,
        desc: desc.map(ToString::to_string),
    }
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
        ] {
            let extra = KeyBindingsList {
                key_bindings: Vec::new(),
                prepend_keymap: std::mem::take(&mut list.prepend_keymap),
                append_keymap: std::mem::take(&mut list.append_keymap),
            };
            merge_section(&mut list.key_bindings, &extra);
        }
    }

    pub fn for_preset(preset: KeymapPreset) -> Self {
        let mut cfg: KeymapConfig =
            toml::from_str(crate::config::DEFAULT_KEYMAP_TOML).unwrap_or_default();
        cfg.preset = KeymapPreset::Default;
        cfg.tree_speed_search = false;
        cfg.normalize_sections();

        match preset {
            KeymapPreset::Default => cfg,
            KeymapPreset::Jetbrains => {
                cfg.preset = KeymapPreset::Jetbrains;
                cfg.tree_speed_search = true;
                let overlay = KeymapConfigOverlay {
                    preset: None,
                    tree_speed_search: Some(true),
                    global: Some(KeyBindingsList {
                        prepend_keymap: vec![
                            kb(&["<C-`>"], KeyAction::ToggleBranchesPopup, None),
                            kb(&["<C-~>"], KeyAction::ToggleBranchesPopup, None),
                            kb(&["<C-S-k>"], KeyAction::GitPush, None),
                            kb(&["<C-t>"], KeyAction::GitPull, None),
                            kb(&["<C-k>"], KeyAction::TabCommit, None),
                            kb(&["<C-A-n>"], KeyAction::NewBranch, None),
                        ],
                        ..Default::default()
                    }),
                    log: Some(KeyBindingsList {
                        prepend_keymap: vec![kb(&["<C-l>"], KeyAction::FocusFilter, None)],
                        ..Default::default()
                    }),
                    commit: Some(KeyBindingsList {
                        prepend_keymap: vec![
                            kb(&["<A-m>"], KeyAction::ToggleAmend, None),
                            kb(&["<C-Enter>"], KeyAction::Commit, None),
                        ],
                        ..Default::default()
                    }),
                    diff: Some(KeyBindingsList {
                        prepend_keymap: vec![
                            kb(&["<F4>"], KeyAction::ZoomDiff, None),
                            kb(&["c"], KeyAction::CycleDiffContext, None),
                        ],
                        ..Default::default()
                    }),
                    ..Default::default()
                };
                cfg.merge_overlay(&overlay);
                cfg
            }
            KeymapPreset::Vim => {
                cfg.preset = KeymapPreset::Vim;
                cfg.tree_speed_search = false;
                let overlay = KeymapConfigOverlay {
                    preset: None,
                    tree_speed_search: Some(false),
                    log: Some(KeyBindingsList {
                        prepend_keymap: vec![
                            kb(&["<C-d>"], KeyAction::PageDownHalf, None),
                            kb(&["<C-u>"], KeyAction::PageUpHalf, None),
                            kb(&["d"], KeyAction::CompareOrDiff, None),
                        ],
                        ..Default::default()
                    }),
                    diff: Some(KeyBindingsList {
                        prepend_keymap: vec![
                            kb(&["c"], KeyAction::CycleDiffContext, None),
                            kb(&["s"], KeyAction::StageSelection, None),
                            kb(&["u"], KeyAction::UnstageSelection, None),
                        ],
                        ..Default::default()
                    }),
                    ..Default::default()
                };
                cfg.merge_overlay(&overlay);
                cfg
            }
        }
    }

    pub fn merge_overlay(&mut self, overlay: &KeymapConfigOverlay) {
        if let Some(p) = overlay.preset
            && p != self.preset
        {
            *self = Self::for_preset(p);
        }
        if let Some(tss) = overlay.tree_speed_search {
            self.tree_speed_search = tss;
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
                    && let Some(seq) = crate::keymap::KeySequence::parse(&b.on)
                {
                    return Some(seq.to_display_string());
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
                    && let Some(seq) = crate::keymap::KeySequence::parse(&b.on)
                {
                    let disp = seq.to_display_string();
                    if !out.contains(&disp) {
                        out.push(disp);
                    }
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
                    && let Some(seq) = crate::keymap::KeySequence::parse(&b.on)
                    && seq.starts_with_editor_chord()
                {
                    let disp = seq.to_display_string();
                    if !out.contains(&disp) {
                        out.push(disp);
                    }
                }
            }
        }
        out
    }

    pub fn primary_editor_key_for(&self, scopes: &[KeymapScope], run: &KeyAction) -> Option<String> {
        self.editor_keys_for(scopes, run).into_iter().next()
    }
}
