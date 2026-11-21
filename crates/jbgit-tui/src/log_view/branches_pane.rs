use std::path::PathBuf;
use std::sync::Arc;

use jbgit_core::{
    BranchTreeNode, BranchTreeRow, BranchesPanelSettings, BranchesTreeModel, RepoSnapshot,
};
use rustc_hash::{FxHashMap, FxHashSet};

/// Terminal width below which the Branches pane auto-collapses.
pub const NARROW_WIDTH: u16 = 110;

#[derive(Debug, Default)]
pub struct BranchesPaneState {
    pub settings: BranchesPanelSettings,
    pub model: BranchesTreeModel,
    pub rows: Vec<BranchTreeRow>,
    pub selected: usize,
    /// Node keys picked with Ctrl+Click for a union branch filter.
    pub multi: Vec<String>,
    /// Inline speed-search query.
    pub query: String,
    /// Explicit search input mode in the Branches pane (`/` or `Ctrl+F`).
    pub searching: bool,
    /// `Alt+B` / `[«]` collapse on a wide terminal.
    pub hidden_by_user: bool,
    /// `Alt+B` on a narrow terminal re-opens the auto-collapsed pane.
    pub forced_open: bool,
    pub settings_path: Option<PathBuf>,
    source: Option<Arc<RepoSnapshot>>,
    ahead_behind_overrides: FxHashMap<String, (usize, usize)>,
}

impl BranchesPaneState {
    pub fn is_collapsed(&self, total_width: u16) -> bool {
        if total_width < NARROW_WIDTH {
            !self.forced_open
        } else {
            self.hidden_by_user
        }
    }

    pub fn toggle_collapsed(&mut self, total_width: u16) {
        if total_width < NARROW_WIDTH {
            self.forced_open = !self.forced_open;
        } else {
            self.hidden_by_user = !self.hidden_by_user;
        }
    }

    pub fn load_settings(&mut self, path: PathBuf) {
        if let Ok(text) = std::fs::read_to_string(&path) {
            self.settings = BranchesPanelSettings::parse(&text);
        }
        self.settings_path = Some(path);
        self.source = None;
    }

    pub fn save_settings(&self) {
        if let Some(path) = &self.settings_path {
            let _ = std::fs::write(path, self.settings.serialize());
        }
    }

    /// Rebuilds the tree model when the snapshot changed (or `force`).
    pub fn sync_model(&mut self, snap: Option<&Arc<RepoSnapshot>>, force: bool) {
        let stale = match (snap, &self.source) {
            (Some(s), Some(src)) => !Arc::ptr_eq(s, src),
            (None, None) => false,
            _ => true,
        };
        if !stale && !force {
            return;
        }
        if stale {
            self.ahead_behind_overrides.clear();
        }
        let selected_key = self.selected_node().map(|n| n.key.clone());
        self.model = match snap {
            Some(s) => BranchesTreeModel::build(
                &s.refs,
                s.head_branch.as_deref(),
                s.head_oid,
                &self.settings.favorites,
                self.settings.options,
            ),
            None => BranchesTreeModel::default(),
        };
        if !self.ahead_behind_overrides.is_empty() {
            for node in &mut self.model.nodes {
                if let Some(&ab) = self.ahead_behind_overrides.get(&node.key) {
                    node.ahead_behind = Some(ab);
                }
            }
        }
        self.source = snap.cloned();
        let valid: FxHashSet<&str> = self.model.nodes.iter().map(|n| n.key.as_str()).collect();
        self.multi.retain(|k| valid.contains(k.as_str()));
        self.refresh_rows();
        if let Some(key) = selected_key {
            self.select_key(&key);
        }
    }

    pub fn apply_ahead_behind_backfill(&mut self, updates: &[(String, (usize, usize))]) {
        if updates.is_empty() {
            return;
        }
        for (full_name, ab) in updates {
            self.ahead_behind_overrides.insert(full_name.clone(), *ab);
        }
        for node in &mut self.model.nodes {
            if let Some(&ab) = self.ahead_behind_overrides.get(&node.key) {
                node.ahead_behind = Some(ab);
            }
        }
    }

    pub fn refresh_rows(&mut self) {
        self.rows = self
            .model
            .visible_rows(&self.settings.expansion, &self.query);
        if self.selected >= self.rows.len() {
            self.selected = self.rows.len().saturating_sub(1);
        }
    }

    pub fn select_key(&mut self, key: &str) {
        if let Some(pos) = self
            .rows
            .iter()
            .position(|r| self.model.nodes[r.node].key == key)
        {
            self.selected = pos;
        }
    }

    pub fn selected_row(&self) -> Option<BranchTreeRow> {
        self.rows.get(self.selected).copied()
    }

    pub fn selected_node(&self) -> Option<&BranchTreeNode> {
        self.selected_row().map(|r| &self.model.nodes[r.node])
    }

    pub fn move_selection(&mut self, delta: isize) {
        if self.rows.is_empty() {
            return;
        }
        self.selected =
            (self.selected as isize + delta).clamp(0, self.rows.len() as isize - 1) as usize;
    }

    /// Ref names for the branch filter: the Ctrl+Click set if any, otherwise
    /// the selected node (folders expand to every child branch).
    pub fn filter_ref_names(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        let push_node = |idx: usize, out: &mut Vec<String>| {
            for n in self.model.collect_ref_names(idx) {
                if !out.contains(&n) {
                    out.push(n);
                }
            }
        };
        if self.multi.is_empty() {
            if let Some(row) = self.selected_row() {
                push_node(row.node, &mut out);
            }
        } else {
            for key in &self.multi {
                if let Some(idx) = self.model.find_by_key(key) {
                    push_node(idx, &mut out);
                }
            }
        }
        out
    }

    pub fn toggle_multi(&mut self, key: &str) {
        if let Some(pos) = self.multi.iter().position(|k| k == key) {
            self.multi.remove(pos);
        } else {
            self.multi.push(key.to_string());
        }
    }
}
