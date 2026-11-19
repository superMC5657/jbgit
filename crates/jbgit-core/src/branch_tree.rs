//! Branches Dashboard tree model (`BranchesDashboardTreeModel` +
//! `BranchTreeNodeComparator` in IDEA).
//!
//! Pure data: the TUI renders [`BranchesTreeModel::visible_rows`] and feeds
//! leaf names from [`BranchesTreeModel::collect_ref_names`] into the log
//! `BranchFilter`.

use std::cmp::Ordering;
use std::fmt::Write as _;

use rustc_hash::{FxHashMap, FxHashSet};

use crate::{Oid, RefInfo, RefKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BranchCategory {
    Local,
    Remote,
    Tags,
}

impl BranchCategory {
    pub const fn key(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Remote => "remote",
            Self::Tags => "tags",
        }
    }

    pub const fn title(self) -> &'static str {
        match self {
            Self::Local => "Local",
            Self::Remote => "Remote",
            Self::Tags => "Tags",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BranchNodeKind {
    Category(BranchCategory),
    /// Second-level remote grouping (`origin`, `upstream`).
    RemoteGroup,
    /// Virtual `/`-separated directory (`feature/auth`).
    Folder,
    /// A concrete ref. `RefKind::Head` marks the root `HEAD (main)` node.
    Leaf(RefKind),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchTreeNode {
    /// Stable identity used for expansion state (`local:feature/auth`, `refs/heads/main`).
    pub key: String,
    pub label: String,
    pub kind: BranchNodeKind,
    pub children: Vec<usize>,
    pub parent: Option<usize>,
    /// Leaves only: name understood by the log branch filter (`main`, `origin/main`, `HEAD`).
    pub ref_name: Option<String>,
    pub target_oid: Option<Oid>,
    pub is_favorite: bool,
    /// Leaf is the checked-out branch (or the `HEAD` node itself).
    pub is_current: bool,
    pub ahead_behind: Option<(usize, usize)>,
    /// Number of leaf refs at or under this node.
    pub leaf_count: usize,
}

impl BranchTreeNode {
    pub fn is_leaf(&self) -> bool {
        matches!(self.kind, BranchNodeKind::Leaf(_))
    }
}

/// Favorite branches: `main` / `master` are favorites by default; explicit
/// toggles are stored as additions / removals keyed by full ref name.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FavoriteBranches {
    added: FxHashSet<String>,
    removed: FxHashSet<String>,
}

impl FavoriteBranches {
    pub fn is_default_favorite(short_name: &str) -> bool {
        matches!(short_name, "main" | "master")
    }

    pub fn is_favorite(&self, full_name: &str, short_name: &str) -> bool {
        if self.added.contains(full_name) {
            return true;
        }
        Self::is_default_favorite(short_name) && !self.removed.contains(full_name)
    }

    /// Flips the favorite flag and returns the new state.
    pub fn toggle(&mut self, full_name: &str, short_name: &str) -> bool {
        let now = !self.is_favorite(full_name, short_name);
        self.added.remove(full_name);
        self.removed.remove(full_name);
        let default = Self::is_default_favorite(short_name);
        if now && !default {
            self.added.insert(full_name.to_string());
        } else if !now && default {
            self.removed.insert(full_name.to_string());
        }
        now
    }
}

/// Expansion state of folder / category nodes. Everything is expanded by
/// default except the `Tags` category.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BranchTreeExpansion {
    overrides: FxHashMap<String, bool>,
}

impl BranchTreeExpansion {
    fn default_expanded(key: &str) -> bool {
        key != BranchCategory::Tags.key()
    }

    pub fn is_expanded(&self, key: &str) -> bool {
        self.overrides
            .get(key)
            .copied()
            .unwrap_or_else(|| Self::default_expanded(key))
    }

    pub fn set(&mut self, key: &str, expanded: bool) {
        if expanded == Self::default_expanded(key) {
            self.overrides.remove(key);
        } else {
            self.overrides.insert(key.to_string(), expanded);
        }
    }

    pub fn toggle(&mut self, key: &str) -> bool {
        let next = !self.is_expanded(key);
        self.set(key, next);
        next
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BranchTreeOptions {
    /// Group `feature/auth/x` into folder nodes (default on).
    pub group_by_directory: bool,
    pub show_tags: bool,
}

impl Default for BranchTreeOptions {
    fn default() -> Self {
        Self {
            group_by_directory: true,
            show_tags: true,
        }
    }
}

/// Persistent Branches panel state (`git.branches.panel.persist.tree.state`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BranchesPanelSettings {
    pub favorites: FavoriteBranches,
    pub expansion: BranchTreeExpansion,
    pub options: BranchTreeOptions,
    /// `Navigate Log to Branch on Selection`.
    pub navigate_on_select: bool,
}

impl BranchesPanelSettings {
    pub fn serialize(&self) -> String {
        let mut out = String::from("# jbgit branches panel state\n");
        let _ = writeln!(
            out,
            "group_by_directory={}",
            self.options.group_by_directory
        );
        let _ = writeln!(out, "show_tags={}", self.options.show_tags);
        let _ = writeln!(out, "navigate_on_select={}", self.navigate_on_select);
        let mut added: Vec<_> = self.favorites.added.iter().collect();
        added.sort();
        for name in added {
            let _ = writeln!(out, "fav+={name}");
        }
        let mut removed: Vec<_> = self.favorites.removed.iter().collect();
        removed.sort();
        for name in removed {
            let _ = writeln!(out, "fav-={name}");
        }
        let mut exp: Vec<_> = self.expansion.overrides.iter().collect();
        exp.sort();
        for (key, on) in exp {
            let _ = writeln!(out, "expanded:{key}={on}");
        }
        out
    }

    pub fn parse(text: &str) -> Self {
        let mut s = Self::default();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((k, v)) = line.split_once('=') else {
                continue;
            };
            let flag = v == "true";
            match k {
                "group_by_directory" => s.options.group_by_directory = flag,
                "show_tags" => s.options.show_tags = flag,
                "navigate_on_select" => s.navigate_on_select = flag,
                "fav+" => {
                    s.favorites.added.insert(v.to_string());
                }
                "fav-" => {
                    s.favorites.removed.insert(v.to_string());
                }
                _ => {
                    if let Some(key) = k.strip_prefix("expanded:") {
                        s.expansion.overrides.insert(key.to_string(), flag);
                    }
                }
            }
        }
        s
    }
}

/// One flattened, renderable row of the tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BranchTreeRow {
    pub node: usize,
    pub depth: u16,
    /// `None` for leaves.
    pub expanded: Option<bool>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BranchesTreeModel {
    pub nodes: Vec<BranchTreeNode>,
    pub roots: Vec<usize>,
}

#[derive(Default)]
struct Dir<'a> {
    dirs: Vec<(String, Dir<'a>)>,
    leaves: Vec<(String, &'a RefInfo)>,
}

impl<'a> Dir<'a> {
    fn insert(&mut self, segments: &[&str], r: &'a RefInfo) {
        match segments {
            [] => {}
            [leaf] => self.leaves.push(((*leaf).to_string(), r)),
            [head, rest @ ..] => {
                let idx = match self.dirs.iter().position(|(n, _)| n == head) {
                    Some(i) => i,
                    None => {
                        self.dirs.push(((*head).to_string(), Dir::default()));
                        self.dirs.len() - 1
                    }
                };
                self.dirs[idx].1.insert(rest, r);
            }
        }
    }
}

impl BranchesTreeModel {
    pub fn build(
        refs: &[RefInfo],
        head_branch: Option<&str>,
        head_oid: Option<Oid>,
        favorites: &FavoriteBranches,
        options: BranchTreeOptions,
    ) -> Self {
        let mut model = Self::default();

        // Root: HEAD (main) / HEAD (detached abc1234)
        if let Some(oid) = head_oid {
            let current = head_branch.and_then(|b| {
                refs.iter()
                    .find(|r| r.kind == RefKind::LocalBranch && r.name == b)
            });
            let label = match head_branch {
                Some(b) => format!("HEAD ({b})"),
                None => format!("HEAD (detached {})", oid.short()),
            };
            let head = model.push(BranchTreeNode {
                key: "HEAD".to_string(),
                label,
                kind: BranchNodeKind::Leaf(RefKind::Head),
                children: Vec::new(),
                parent: None,
                ref_name: Some("HEAD".to_string()),
                target_oid: Some(oid),
                is_favorite: current
                    .is_some_and(|r| favorites.is_favorite(&r.full_name, leaf_short(r))),
                is_current: true,
                ahead_behind: current.and_then(|r| r.ahead_behind),
                leaf_count: 1,
            });
            model.roots.push(head);
        }

        let locals: Vec<&RefInfo> = refs
            .iter()
            .filter(|r| r.kind == RefKind::LocalBranch)
            .collect();
        let remotes: Vec<&RefInfo> = refs
            .iter()
            .filter(|r| r.kind == RefKind::RemoteBranch)
            .collect();
        let tags: Vec<&RefInfo> = refs.iter().filter(|r| r.kind == RefKind::Tag).collect();

        let local_root = model.push_category(BranchCategory::Local);
        let mut dir = Dir::default();
        for r in &locals {
            dir.insert(&split_name(&r.name, options.group_by_directory), r);
        }
        let children = model.emit_dir(dir, "local:", "", local_root, favorites, head_branch);
        model.finish_container(local_root, children);
        model.roots.push(local_root);

        if !remotes.is_empty() {
            let remote_root = model.push_category(BranchCategory::Remote);
            let mut groups: Vec<(String, Dir<'_>)> = Vec::new();
            for r in &remotes {
                let (remote, rest) = r.name.split_once('/').unwrap_or(("", r.name.as_str()));
                let idx = match groups.iter().position(|(n, _)| n == remote) {
                    Some(i) => i,
                    None => {
                        groups.push((remote.to_string(), Dir::default()));
                        groups.len() - 1
                    }
                };
                groups[idx]
                    .1
                    .insert(&split_name(rest, options.group_by_directory), r);
            }
            groups.sort_by(|a, b| remote_order(&a.0, &b.0));
            let mut group_ids = Vec::with_capacity(groups.len());
            for (remote, gdir) in groups {
                let key = format!("remote:{remote}");
                let gid = model.push(BranchTreeNode {
                    key: key.clone(),
                    label: remote.clone(),
                    kind: BranchNodeKind::RemoteGroup,
                    children: Vec::new(),
                    parent: Some(remote_root),
                    ref_name: None,
                    target_oid: None,
                    is_favorite: false,
                    is_current: false,
                    ahead_behind: None,
                    leaf_count: 0,
                });
                let prefix = format!("{key}:");
                let ch = model.emit_dir(gdir, &prefix, "", gid, favorites, head_branch);
                model.finish_container(gid, ch);
                group_ids.push(gid);
            }
            model.finish_container(remote_root, group_ids);
            model.roots.push(remote_root);
        }

        if options.show_tags && !tags.is_empty() {
            let tags_root = model.push_category(BranchCategory::Tags);
            let mut dir = Dir::default();
            for r in &tags {
                dir.insert(&split_name(&r.name, options.group_by_directory), r);
            }
            let children = model.emit_dir(dir, "tags:", "", tags_root, favorites, head_branch);
            model.finish_container(tags_root, children);
            model.roots.push(tags_root);
        }

        model
    }

    fn push(&mut self, node: BranchTreeNode) -> usize {
        self.nodes.push(node);
        self.nodes.len() - 1
    }

    fn push_category(&mut self, cat: BranchCategory) -> usize {
        self.push(BranchTreeNode {
            key: cat.key().to_string(),
            label: cat.title().to_string(),
            kind: BranchNodeKind::Category(cat),
            children: Vec::new(),
            parent: None,
            ref_name: None,
            target_oid: None,
            is_favorite: false,
            is_current: false,
            ahead_behind: None,
            leaf_count: 0,
        })
    }

    fn finish_container(&mut self, id: usize, children: Vec<usize>) {
        let count = children.iter().map(|&c| self.nodes[c].leaf_count).sum();
        for &c in &children {
            self.nodes[c].parent = Some(id);
        }
        self.nodes[id].children = children;
        self.nodes[id].leaf_count = count;
    }

    fn emit_dir(
        &mut self,
        dir: Dir<'_>,
        key_prefix: &str,
        path_prefix: &str,
        parent: usize,
        favorites: &FavoriteBranches,
        head_branch: Option<&str>,
    ) -> Vec<usize> {
        let mut out = Vec::with_capacity(dir.dirs.len() + dir.leaves.len());
        for (name, sub) in dir.dirs {
            // Compact single-child folder chains: `a/b/c` as one node.
            let (label, sub) = crate::compact_dir_chain(name, sub, |d| {
                (d.leaves.is_empty() && d.dirs.len() == 1)
                    .then(|| d.dirs.pop().expect("one dir"))
            });
            let path = format!("{path_prefix}{label}");
            let id = self.push(BranchTreeNode {
                key: format!("{key_prefix}{path}"),
                label,
                kind: BranchNodeKind::Folder,
                children: Vec::new(),
                parent: Some(parent),
                ref_name: None,
                target_oid: None,
                is_favorite: false,
                is_current: false,
                ahead_behind: None,
                leaf_count: 0,
            });
            let next_prefix = format!("{path}/");
            let ch = self.emit_dir(sub, key_prefix, &next_prefix, id, favorites, head_branch);
            self.finish_container(id, ch);
            out.push(id);
        }
        for (label, r) in dir.leaves {
            let is_current = r.kind == RefKind::LocalBranch
                && (r.is_head || head_branch == Some(r.name.as_str()));
            let id = self.push(BranchTreeNode {
                key: r.full_name.clone(),
                label,
                kind: BranchNodeKind::Leaf(r.kind),
                children: Vec::new(),
                parent: Some(parent),
                ref_name: Some(r.name.clone()),
                target_oid: Some(r.target_oid),
                is_favorite: r.kind != RefKind::Tag
                    && favorites.is_favorite(&r.full_name, leaf_short(r)),
                is_current,
                ahead_behind: r.ahead_behind,
                leaf_count: 1,
            });
            out.push(id);
        }
        out.sort_by(|&a, &b| self.compare_nodes(a, b));
        out
    }

    /// `BranchTreeNodeComparator`: ★ favorites → main/master → folders → others,
    /// natural order within each rank.
    fn compare_nodes(&self, a: usize, b: usize) -> Ordering {
        let rank = |n: &BranchTreeNode| -> u8 {
            match n.kind {
                BranchNodeKind::Leaf(_) if n.is_favorite => 0,
                BranchNodeKind::Leaf(_) if FavoriteBranches::is_default_favorite(&n.label) => 1,
                BranchNodeKind::Folder | BranchNodeKind::RemoteGroup => 2,
                _ => 3,
            }
        };
        let (na, nb) = (&self.nodes[a], &self.nodes[b]);
        rank(na)
            .cmp(&rank(nb))
            .then_with(|| natural_cmp(&na.label, &nb.label))
    }

    /// Flattens the tree honoring `expansion`. A non-empty `query` keeps only
    /// matching leaves plus their ancestors, all force-expanded.
    pub fn visible_rows(&self, expansion: &BranchTreeExpansion, query: &str) -> Vec<BranchTreeRow> {
        let query = query.trim();
        let keep = if query.is_empty() {
            None
        } else {
            let mut matcher = nucleo_matcher::Matcher::new(nucleo_matcher::Config::DEFAULT);
            let pattern = nucleo_matcher::pattern::Pattern::parse(
                query,
                nucleo_matcher::pattern::CaseMatching::Smart,
                nucleo_matcher::pattern::Normalization::Smart,
            );
            let mut buf = Vec::new();
            let mut keep = vec![false; self.nodes.len()];
            for (idx, node) in self.nodes.iter().enumerate() {
                if node.is_leaf()
                    && node.ref_name.as_deref().is_some_and(|n| {
                        pattern
                            .score(nucleo_matcher::Utf32Str::new(n, &mut buf), &mut matcher)
                            .is_some()
                            || pattern
                                .score(
                                    nucleo_matcher::Utf32Str::new(&node.label, &mut buf),
                                    &mut matcher,
                                )
                                .is_some()
                    })
                {
                    let mut cur = Some(idx);
                    while let Some(c) = cur {
                        if keep[c] {
                            break;
                        }
                        keep[c] = true;
                        cur = self.nodes[c].parent;
                    }
                }
            }
            Some(keep)
        };

        let mut rows = Vec::new();
        for &root in &self.roots {
            self.flatten(root, 0, expansion, keep.as_deref(), &mut rows);
        }
        rows
    }

    fn flatten(
        &self,
        id: usize,
        depth: u16,
        expansion: &BranchTreeExpansion,
        keep: Option<&[bool]>,
        out: &mut Vec<BranchTreeRow>,
    ) {
        if keep.is_some_and(|k| !k[id]) {
            return;
        }
        let node = &self.nodes[id];
        if node.is_leaf() {
            out.push(BranchTreeRow {
                node: id,
                depth,
                expanded: None,
            });
            return;
        }
        let expanded = keep.is_some() || expansion.is_expanded(&node.key);
        out.push(BranchTreeRow {
            node: id,
            depth,
            expanded: Some(expanded),
        });
        if expanded {
            for &c in &node.children {
                self.flatten(c, depth + 1, expansion, keep, out);
            }
        }
    }

    /// All leaf ref names at or under `id` (folders recurse into every child branch).
    pub fn collect_ref_names(&self, id: usize) -> Vec<String> {
        let mut out = Vec::new();
        let mut stack = vec![id];
        while let Some(cur) = stack.pop() {
            let Some(node) = self.nodes.get(cur) else {
                continue;
            };
            if let Some(name) = &node.ref_name
                && !out.contains(name)
            {
                out.push(name.clone());
            }
            stack.extend(node.children.iter().rev());
        }
        out
    }

    pub fn find_by_key(&self, key: &str) -> Option<usize> {
        self.nodes.iter().position(|n| n.key == key)
    }
}

fn leaf_short(r: &RefInfo) -> &str {
    match r.kind {
        RefKind::RemoteBranch => r.name.split_once('/').map_or(r.name.as_str(), |(_, s)| s),
        _ => r.name.as_str(),
    }
}

fn split_name(name: &str, group: bool) -> Vec<&str> {
    if group {
        name.split('/').filter(|s| !s.is_empty()).collect()
    } else {
        vec![name]
    }
}

fn remote_order(a: &str, b: &str) -> Ordering {
    let rank = |n: &str| u8::from(n != "origin");
    rank(a).cmp(&rank(b)).then_with(|| natural_cmp(a, b))
}

/// Fuzzy match backed by `nucleo-matcher` (`fda` matches `feature/dag-layout`).
pub fn fuzzy_match(haystack: &str, needle: &str) -> bool {
    let needle = needle.trim();
    if needle.is_empty() {
        return true;
    }
    let mut matcher = nucleo_matcher::Matcher::new(nucleo_matcher::Config::DEFAULT);
    let pattern = nucleo_matcher::pattern::Pattern::parse(
        needle,
        nucleo_matcher::pattern::CaseMatching::Smart,
        nucleo_matcher::pattern::Normalization::Smart,
    );
    let mut buf = Vec::new();
    pattern
        .score(
            nucleo_matcher::Utf32Str::new(haystack, &mut buf),
            &mut matcher,
        )
        .is_some()
}

/// Natural ordering: digit runs compare numerically (`v1.9` < `v1.10`),
/// letters compare case-insensitively.
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let (mut ai, mut bi) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (ai.peek().copied(), bi.peek().copied()) {
            (None, None) => return a.cmp(b),
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(ca), Some(cb)) if ca.is_ascii_digit() && cb.is_ascii_digit() => {
                let mut na = String::new();
                while let Some(c) = ai.peek().copied().filter(char::is_ascii_digit) {
                    na.push(c);
                    ai.next();
                }
                let mut nb = String::new();
                while let Some(c) = bi.peek().copied().filter(char::is_ascii_digit) {
                    nb.push(c);
                    bi.next();
                }
                let ta = na.trim_start_matches('0');
                let tb = nb.trim_start_matches('0');
                let ord = ta.len().cmp(&tb.len()).then_with(|| ta.cmp(tb));
                if ord != Ordering::Equal {
                    return ord;
                }
            }
            (Some(ca), Some(cb)) => {
                let ord = ca.to_lowercase().cmp(cb.to_lowercase());
                if ord != Ordering::Equal {
                    return ord;
                }
                ai.next();
                bi.next();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(name: &str, kind: RefKind, n: u8) -> RefInfo {
        let full_name = match kind {
            RefKind::LocalBranch => format!("refs/heads/{name}"),
            RefKind::RemoteBranch => format!("refs/remotes/{name}"),
            RefKind::Tag => format!("refs/tags/{name}"),
            RefKind::Head => "HEAD".to_string(),
        };
        RefInfo {
            name: name.to_string(),
            full_name,
            target_oid: Oid::from_bytes([n; 20]),
            kind,
            upstream: None,
            ahead_behind: None,
            is_head: false,
        }
    }

    fn labels(model: &BranchesTreeModel, rows: &[BranchTreeRow]) -> Vec<String> {
        rows.iter()
            .map(|row| {
                format!(
                    "{}{}",
                    "  ".repeat(row.depth as usize),
                    model.nodes[row.node].label
                )
            })
            .collect()
    }

    #[test]
    fn groups_directories_and_folder_enter_collects_all_children() {
        let refs = vec![
            r("feature/auth/login", RefKind::LocalBranch, 1),
            r("feature/auth/oauth", RefKind::LocalBranch, 2),
            r("bugfix/ui", RefKind::LocalBranch, 3),
            r("main", RefKind::LocalBranch, 4),
            r("origin/main", RefKind::RemoteBranch, 4),
        ];
        let model = BranchesTreeModel::build(
            &refs,
            Some("main"),
            Some(Oid::from_bytes([4; 20])),
            &FavoriteBranches::default(),
            BranchTreeOptions::default(),
        );
        let rows = model.visible_rows(&BranchTreeExpansion::default(), "");
        let got = labels(&model, &rows);
        assert_eq!(
            got,
            vec![
                "HEAD (main)",
                "Local",
                "  main",
                "  bugfix",
                "    ui",
                // `feature` has a single child folder `auth` → compacted.
                "  feature/auth",
                "    login",
                "    oauth",
                "Remote",
                "  origin",
                "    main",
            ]
        );

        let folder = model.find_by_key("local:feature/auth").unwrap();
        let mut names = model.collect_ref_names(folder);
        names.sort();
        assert_eq!(names, vec!["feature/auth/login", "feature/auth/oauth"]);

        // Collapsing hides children.
        let mut exp = BranchTreeExpansion::default();
        exp.toggle("local:feature/auth");
        let rows = model.visible_rows(&exp, "");
        assert!(!labels(&model, &rows).contains(&"    login".to_string()));
    }

    #[test]
    fn favorites_sort_first_and_toggle_round_trips() {
        let refs = vec![
            r("zeta", RefKind::LocalBranch, 1),
            r("alpha", RefKind::LocalBranch, 2),
            r("main", RefKind::LocalBranch, 3),
        ];
        let mut favs = FavoriteBranches::default();
        assert!(favs.toggle("refs/heads/zeta", "zeta"));
        let model =
            BranchesTreeModel::build(&refs, None, None, &favs, BranchTreeOptions::default());
        let rows = model.visible_rows(&BranchTreeExpansion::default(), "");
        assert_eq!(
            labels(&model, &rows),
            vec!["Local", "  main", "  zeta", "  alpha"]
        );

        // main is a default favorite; un-favoriting persists through serialization.
        assert!(!favs.toggle("refs/heads/main", "main"));
        let settings = BranchesPanelSettings {
            favorites: favs.clone(),
            ..Default::default()
        };
        let parsed = BranchesPanelSettings::parse(&settings.serialize());
        assert_eq!(parsed.favorites, favs);
        assert!(!parsed.favorites.is_favorite("refs/heads/main", "main"));
        assert!(parsed.favorites.is_favorite("refs/heads/zeta", "zeta"));
    }

    #[test]
    fn speed_search_keeps_matching_leaves_and_expands_ancestors() {
        let refs = vec![
            r("feature/dag-layout", RefKind::LocalBranch, 1),
            r("feature/partial-stage", RefKind::LocalBranch, 2),
            r("hotfix-crlf", RefKind::LocalBranch, 3),
            r("v1.0", RefKind::Tag, 4),
        ];
        let model = BranchesTreeModel::build(
            &refs,
            None,
            None,
            &FavoriteBranches::default(),
            BranchTreeOptions::default(),
        );
        let mut exp = BranchTreeExpansion::default();
        exp.set("local:feature", false);
        let rows = model.visible_rows(&exp, "dag");
        assert_eq!(
            labels(&model, &rows),
            vec!["Local", "  feature", "    dag-layout"]
        );
        // Tags category is collapsed by default.
        let rows = model.visible_rows(&BranchTreeExpansion::default(), "");
        let tags = rows
            .iter()
            .find(|row| model.nodes[row.node].key == "tags")
            .unwrap();
        assert_eq!(tags.expanded, Some(false));
    }

    #[test]
    fn natural_order_and_ahead_behind_on_head() {
        assert_eq!(natural_cmp("v1.9", "v1.10"), Ordering::Less);
        assert_eq!(natural_cmp("Alpha", "beta"), Ordering::Less);
        let mut main = r("main", RefKind::LocalBranch, 1);
        main.is_head = true;
        main.ahead_behind = Some((2, 1));
        let model = BranchesTreeModel::build(
            &[main],
            Some("main"),
            Some(Oid::from_bytes([1; 20])),
            &FavoriteBranches::default(),
            BranchTreeOptions::default(),
        );
        let head = &model.nodes[model.roots[0]];
        assert_eq!(head.ahead_behind, Some((2, 1)));
        assert!(head.is_favorite);
        assert_eq!(model.collect_ref_names(model.roots[0]), vec!["HEAD"]);
    }
}
