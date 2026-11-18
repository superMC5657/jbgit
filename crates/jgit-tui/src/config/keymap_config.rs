use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct KeymapConfig {
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
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct KeyBindingsList {
    #[serde(default)]
    pub key_bindings: Vec<KeyBindingConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct KeyBindingConfig {
    pub on: Vec<String>,
    pub run: String,
    #[serde(default)]
    pub desc: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct KeymapConfigOverlay {
    pub global: Option<KeyBindingsList>,
    pub log: Option<KeyBindingsList>,
    pub commit: Option<KeyBindingsList>,
    pub branch: Option<KeyBindingsList>,
    pub diff: Option<KeyBindingsList>,
}

fn merge_section(base: &mut Vec<KeyBindingConfig>, overlay: &[KeyBindingConfig]) {
    if overlay.is_empty() {
        return;
    }
    base.retain(|existing| {
        !overlay.iter().any(|ov| {
            ov.on == existing.on || (!ov.run.is_empty() && ov.run == existing.run)
        })
    });
    base.extend(overlay.iter().cloned());
}

impl KeymapConfig {
    pub fn merge_overlay(&mut self, overlay: &KeymapConfigOverlay) {
        if let Some(g) = &overlay.global {
            merge_section(&mut self.global.key_bindings, &g.key_bindings);
        }
        if let Some(l) = &overlay.log {
            merge_section(&mut self.log.key_bindings, &l.key_bindings);
        }
        if let Some(c) = &overlay.commit {
            merge_section(&mut self.commit.key_bindings, &c.key_bindings);
        }
        if let Some(b) = &overlay.branch {
            merge_section(&mut self.branch.key_bindings, &b.key_bindings);
        }
        if let Some(d) = &overlay.diff {
            merge_section(&mut self.diff.key_bindings, &d.key_bindings);
        }
    }
}
