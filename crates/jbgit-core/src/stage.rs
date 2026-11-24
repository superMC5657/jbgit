use std::collections::BTreeMap;

use rustc_hash::{FxHashMap, FxHashSet};

use crate::details::FileChange;

pub const UNTRACKED_COLLAPSE_THRESHOLD: usize = 50;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FileStatusKind {
    Modified,
    Added,
    Deleted,
    Renamed,
    Copied,
    Typechange,
    Untracked,
    Ignored,
    Conflicted,
}

impl FileStatusKind {
    pub const fn badge_char(self) -> char {
        match self {
            Self::Modified => 'M',
            Self::Added => 'A',
            Self::Deleted => 'D',
            Self::Renamed => 'R',
            Self::Copied => 'C',
            Self::Typechange => 'T',
            Self::Untracked => '?',
            Self::Ignored => 'I',
            Self::Conflicted => '!',
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StageEntry {
    pub path: String,
    pub old_path: Option<String>,
    pub kind: FileStatusKind,
}

/// Dirty/uninitialized status of a submodule gitlink relative to HEAD/index.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct SubmoduleState {
    pub new_commits: bool,
    pub dirty_workdir: bool,
    pub uninitialized: bool,
}

impl SubmoduleState {
    pub const fn is_dirty(self) -> bool {
        self.new_commits || self.dirty_workdir || self.uninitialized
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StageStatusSnapshot {
    pub staged: Vec<StageEntry>,
    pub unstaged: Vec<StageEntry>,
    pub untracked: Vec<StageEntry>,
    pub conflicted: Vec<StageEntry>,
    pub ignored: Vec<StageEntry>,
    pub conflict_labels: BTreeMap<String, String>,
    pub submodules: BTreeMap<String, SubmoduleState>,
}

impl StageStatusSnapshot {
    pub fn total_changes(&self) -> usize {
        self.staged.len() + self.unstaged.len() + self.untracked.len() + self.conflicted.len()
    }

    pub fn is_clean(&self) -> bool {
        self.staged.is_empty()
            && self.unstaged.is_empty()
            && self.untracked.is_empty()
            && self.conflicted.is_empty()
    }
}

/// Top-level grouping kind matching IDEA `GitStageTree.NodeKind` and `sortOrder`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum StageNodeKind {
    Conflicted = 0,
    Staged = 1,
    Unstaged = 2,
    Untracked = 3,
    Ignored = 4,
    AmendCommit = 5,
}

impl StageNodeKind {
    pub const fn title(self) -> &'static str {
        match self {
            Self::Conflicted => "Merge Conflicts",
            Self::Staged => "Staged",
            Self::Unstaged => "Unstaged",
            Self::Untracked => "Untracked",
            Self::Ignored => "Ignored",
            Self::AmendCommit => "Head Commit to Amend",
        }
    }

    pub const fn sort_order(self) -> u8 {
        self as u8
    }

    pub const fn can_stage(self) -> bool {
        matches!(self, Self::Unstaged | Self::Untracked)
    }

    pub const fn can_unstage(self) -> bool {
        matches!(self, Self::Staged)
    }
}

/// Read-only preview of `HEAD` shown inside `GitStageTree` when `Amend` mode is checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AmendCommitPreview {
    pub short_sha: String,
    pub message: String,
    pub files: Vec<FileChange>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StageTreeOptions {
    pub group_by_dir: bool,
    pub show_ignored: bool,
    pub untracked_collapse_threshold: usize,
    pub untracked_many_expanded: bool,
    pub amend_expanded: bool,
}

impl Default for StageTreeOptions {
    fn default() -> Self {
        Self {
            group_by_dir: true,
            show_ignored: false,
            untracked_collapse_threshold: UNTRACKED_COLLAPSE_THRESHOLD,
            untracked_many_expanded: false,
            amend_expanded: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StageTreeRowKind {
    Group {
        kind: StageNodeKind,
        file_count: usize,
    },
    Dir {
        group_kind: StageNodeKind,
        dir_prefix: String,
        file_count: usize,
    },
    File {
        group_kind: StageNodeKind,
        path: String,
        old_path: Option<String>,
        status_kind: FileStatusKind,
        conflict_label: Option<String>,
        submodule: Option<SubmoduleState>,
    },
    AmendFile {
        path: String,
        old_path: Option<String>,
        status_kind: FileStatusKind,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StageTreeRow {
    pub key: String,
    pub label: String,
    pub depth: u16,
    pub expanded: bool,
    pub kind: StageTreeRowKind,
}

impl StageTreeRow {
    pub fn group_kind(&self) -> StageNodeKind {
        match &self.kind {
            StageTreeRowKind::Group { kind, .. } => *kind,
            StageTreeRowKind::Dir { group_kind, .. } => *group_kind,
            StageTreeRowKind::File { group_kind, .. } => *group_kind,
            StageTreeRowKind::AmendFile { .. } => StageNodeKind::AmendCommit,
        }
    }

    pub fn is_file(&self) -> bool {
        matches!(self.kind, StageTreeRowKind::File { .. })
    }

    pub fn is_container(&self) -> bool {
        matches!(
            self.kind,
            StageTreeRowKind::Group { .. } | StageTreeRowKind::Dir { .. }
        )
    }

    pub fn submodule(&self) -> Option<SubmoduleState> {
        match &self.kind {
            StageTreeRowKind::File { submodule, .. } => *submodule,
            _ => None,
        }
    }

    pub fn is_submodule(&self) -> bool {
        self.submodule().is_some()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StageTarget {
    pub group_kind: StageNodeKind,
    pub path: String,
    pub old_path: Option<String>,
    pub status_kind: FileStatusKind,
    pub is_submodule: bool,
}

#[derive(Default)]
struct StageDir {
    dirs: BTreeMap<String, StageDir>,
    files: Vec<usize>,
}

impl StageDir {
    fn count(&self) -> usize {
        self.files.len() + self.dirs.values().map(StageDir::count).sum::<usize>()
    }
}

/// Builds the flattened `GitStageTree` rows following IDEA's `NodeKind` priority:
/// `Conflicted (0)` -> `Staged (1)` -> `Unstaged (2)` -> `Untracked (3, collapsed if >50)`
/// -> `Ignored (4, optional)` -> `Head Commit to Amend (5, when Amend active)`.
pub fn build_stage_tree_rows(
    status: &StageStatusSnapshot,
    amend: Option<&AmendCommitPreview>,
    opts: StageTreeOptions,
    collapsed: &FxHashSet<String>,
) -> Vec<StageTreeRow> {
    let mut rows = Vec::new();

    // 0. Merge Conflicts (only shown when non-empty)
    if !status.conflicted.is_empty() {
        let mut entries: Vec<StageEntry> = status.conflicted.clone();
        entries.sort_by(|a, b| a.path.cmp(&b.path));
        emit_group(
            StageNodeKind::Conflicted,
            &entries,
            Some(&status.conflict_labels),
            &status.submodules,
            opts.group_by_dir,
            collapsed,
            None,
            &mut rows,
        );
    }

    // 1. Staged
    if !status.staged.is_empty() {
        let mut entries = status.staged.clone();
        entries.sort_by(|a, b| a.path.cmp(&b.path));
        emit_group(
            StageNodeKind::Staged,
            &entries,
            None,
            &status.submodules,
            opts.group_by_dir,
            collapsed,
            None,
            &mut rows,
        );
    }

    // 2. Unstaged (+ Untracked when <= threshold)
    let many_untracked = status.untracked.len() > opts.untracked_collapse_threshold;
    let mut unstaged_entries = status.unstaged.clone();
    if !many_untracked {
        unstaged_entries.extend(status.untracked.iter().cloned());
    }
    if !unstaged_entries.is_empty() {
        unstaged_entries.sort_by(|a, b| a.path.cmp(&b.path));
        emit_group(
            StageNodeKind::Unstaged,
            &unstaged_entries,
            None,
            &status.submodules,
            opts.group_by_dir,
            collapsed,
            None,
            &mut rows,
        );
    }

    // 3. Untracked (dedicated collapsible group when > threshold)
    if many_untracked {
        let mut entries = status.untracked.clone();
        entries.sort_by(|a, b| a.path.cmp(&b.path));
        emit_group(
            StageNodeKind::Untracked,
            &entries,
            None,
            &status.submodules,
            opts.group_by_dir,
            collapsed,
            Some(opts.untracked_many_expanded),
            &mut rows,
        );
    }

    // 4. Ignored (optional)
    if opts.show_ignored && !status.ignored.is_empty() {
        let mut entries = status.ignored.clone();
        entries.sort_by(|a, b| a.path.cmp(&b.path));
        emit_group(
            StageNodeKind::Ignored,
            &entries,
            None,
            &status.submodules,
            opts.group_by_dir,
            collapsed,
            None,
            &mut rows,
        );
    }

    // 5. Head Commit to Amend (virtual read-only node)
    if let Some(amend_info) = amend {
        let group_key = format!("group:{:?}", StageNodeKind::AmendCommit);
        let expanded = opts.amend_expanded && !collapsed.contains(&group_key);
        rows.push(StageTreeRow {
            key: group_key,
            label: format!(
                "{} ({})",
                StageNodeKind::AmendCommit.title(),
                amend_info.short_sha
            ),
            depth: 0,
            expanded,
            kind: StageTreeRowKind::Group {
                kind: StageNodeKind::AmendCommit,
                file_count: amend_info.files.len(),
            },
        });
        if expanded {
            let mut files = amend_info.files.clone();
            files.sort_by(|a, b| a.path.cmp(&b.path));
            for f in files {
                rows.push(StageTreeRow {
                    key: format!("file:AmendCommit:{}", f.path),
                    label: f.path.clone(),
                    depth: 1,
                    expanded: false,
                    kind: StageTreeRowKind::AmendFile {
                        path: f.path,
                        old_path: f.old_path,
                        status_kind: f.kind,
                    },
                });
            }
        }
    }

    rows
}

#[allow(clippy::too_many_arguments)]
fn emit_group(
    group_kind: StageNodeKind,
    entries: &[StageEntry],
    conflict_labels: Option<&BTreeMap<String, String>>,
    submodules: &BTreeMap<String, SubmoduleState>,
    group_by_dir: bool,
    collapsed: &FxHashSet<String>,
    expanded_override: Option<bool>,
    out: &mut Vec<StageTreeRow>,
) {
    let group_key = format!("group:{group_kind:?}");
    let expanded = expanded_override.unwrap_or_else(|| !collapsed.contains(&group_key));
    out.push(StageTreeRow {
        key: group_key,
        label: group_kind.title().to_string(),
        depth: 0,
        expanded,
        kind: StageTreeRowKind::Group {
            kind: group_kind,
            file_count: entries.len(),
        },
    });
    if !expanded {
        return;
    }

    if !group_by_dir {
        for e in entries {
            let conflict_label = conflict_labels.and_then(|m| m.get(&e.path).cloned());
            let submodule = submodules.get(&e.path).copied();
            out.push(StageTreeRow {
                key: format!("file:{group_kind:?}:{}", e.path),
                label: e.path.clone(),
                depth: 1,
                expanded: false,
                kind: StageTreeRowKind::File {
                    group_kind,
                    path: e.path.clone(),
                    old_path: e.old_path.clone(),
                    status_kind: e.kind,
                    conflict_label,
                    submodule,
                },
            });
        }
        return;
    }

    let mut root = StageDir::default();
    for (idx, e) in entries.iter().enumerate() {
        let mut cur = &mut root;
        let segments: Vec<&str> = e.path.split('/').collect();
        for seg in &segments[..segments.len().saturating_sub(1)] {
            cur = cur.dirs.entry((*seg).to_string()).or_default();
        }
        cur.files.push(idx);
    }

    emit_stage_dir(
        &root,
        group_kind,
        "",
        1,
        entries,
        conflict_labels,
        submodules,
        collapsed,
        out,
    );
}

#[allow(clippy::too_many_arguments)]
fn emit_stage_dir(
    dir: &StageDir,
    group_kind: StageNodeKind,
    prefix: &str,
    depth: u16,
    entries: &[StageEntry],
    conflict_labels: Option<&BTreeMap<String, String>>,
    submodules: &BTreeMap<String, SubmoduleState>,
    collapsed: &FxHashSet<String>,
    out: &mut Vec<StageTreeRow>,
) {
    for (name, sub) in &dir.dirs {
        let (label, node) = crate::compact_dir_chain(name.clone(), sub, |d| {
            (d.files.is_empty() && d.dirs.len() == 1).then(|| {
                let (child_name, child) = d.dirs.iter().next().expect("one dir");
                (child_name.clone(), child)
            })
        });
        let dir_prefix = format!("{prefix}{label}");
        let key = format!("dir:{group_kind:?}:{dir_prefix}");
        let expanded = !collapsed.contains(&key);
        out.push(StageTreeRow {
            key,
            label,
            depth,
            expanded,
            kind: StageTreeRowKind::Dir {
                group_kind,
                dir_prefix: dir_prefix.clone(),
                file_count: node.count(),
            },
        });
        if expanded {
            emit_stage_dir(
                node,
                group_kind,
                &format!("{dir_prefix}/"),
                depth + 1,
                entries,
                conflict_labels,
                submodules,
                collapsed,
                out,
            );
        }
    }

    for &idx in &dir.files {
        let e = &entries[idx];
        let conflict_label = conflict_labels.and_then(|m| m.get(&e.path).cloned());
        let submodule = submodules.get(&e.path).copied();
        let file_name = e.path.rsplit('/').next().unwrap_or(&e.path).to_string();
        out.push(StageTreeRow {
            key: format!("file:{group_kind:?}:{}", e.path),
            label: file_name,
            depth,
            expanded: false,
            kind: StageTreeRowKind::File {
                group_kind,
                path: e.path.clone(),
                old_path: e.old_path.clone(),
                status_kind: e.kind,
                conflict_label,
                submodule,
            },
        });
    }
}

/// Collects all underlying file targets for a group, directory, or file row
/// (works even when the group/directory is collapsed).
pub fn collect_targets_under_row(
    rows: &[StageTreeRow],
    row_idx: usize,
    status: &StageStatusSnapshot,
    untracked_collapse_threshold: usize,
) -> Vec<StageTarget> {
    let Some(row) = rows.get(row_idx) else {
        return Vec::new();
    };
    let entries_for_group = |gk: StageNodeKind| -> Vec<StageEntry> {
        match gk {
            StageNodeKind::Conflicted => status.conflicted.clone(),
            StageNodeKind::Staged => status.staged.clone(),
            StageNodeKind::Unstaged => {
                let mut v = status.unstaged.clone();
                if status.untracked.len() <= untracked_collapse_threshold {
                    v.extend(status.untracked.iter().cloned());
                }
                v
            }
            StageNodeKind::Untracked => status.untracked.clone(),
            StageNodeKind::Ignored => status.ignored.clone(),
            StageNodeKind::AmendCommit => Vec::new(),
        }
    };

    match &row.kind {
        StageTreeRowKind::Group { kind, .. } => entries_for_group(*kind)
            .into_iter()
            .map(|e| StageTarget {
                is_submodule: status.submodules.contains_key(&e.path),
                group_kind: *kind,
                path: e.path,
                old_path: e.old_path,
                status_kind: e.kind,
            })
            .collect(),
        StageTreeRowKind::Dir {
            group_kind,
            dir_prefix,
            ..
        } => {
            let prefix_slash = format!("{dir_prefix}/");
            entries_for_group(*group_kind)
                .into_iter()
                .filter(|e| e.path.starts_with(&prefix_slash))
                .map(|e| StageTarget {
                    is_submodule: status.submodules.contains_key(&e.path),
                    group_kind: *group_kind,
                    path: e.path,
                    old_path: e.old_path,
                    status_kind: e.kind,
                })
                .collect()
        }
        StageTreeRowKind::File {
            group_kind,
            path,
            old_path,
            status_kind,
            submodule,
            ..
        } => vec![StageTarget {
            group_kind: *group_kind,
            path: path.clone(),
            old_path: old_path.clone(),
            status_kind: *status_kind,
            is_submodule: submodule.is_some(),
        }],
        StageTreeRowKind::AmendFile { .. } => Vec::new(),
    }
}

/// Computes the next cursor index after staging/unstaging/discarding `old_selected`
/// so repeated `Space` presses smoothly advance through remaining items (`GitStageTreeStateStrategy`).
pub fn next_anchor_after_mutation(
    old_rows: &[StageTreeRow],
    old_selected: usize,
    new_rows: &[StageTreeRow],
) -> usize {
    if new_rows.is_empty() {
        return 0;
    }
    let Some(old_row) = old_rows.get(old_selected) else {
        return old_selected.min(new_rows.len() - 1);
    };
    let group_kind = old_row.group_kind();
    let mut new_pos: FxHashMap<&str, usize> =
        FxHashMap::with_capacity_and_hasher(new_rows.len(), Default::default());
    for (idx, row) in new_rows.iter().enumerate() {
        new_pos.entry(row.key.as_str()).or_insert(idx);
    }

    // 1. Prefer next file in the same group
    for cand in old_rows.iter().skip(old_selected + 1) {
        if cand.is_file()
            && cand.group_kind() == group_kind
            && let Some(&pos) = new_pos.get(cand.key.as_str())
        {
            return pos;
        }
    }
    // 2. Fallback to previous file in the same group
    for cand in old_rows[..old_selected].iter().rev() {
        if cand.is_file()
            && cand.group_kind() == group_kind
            && let Some(&pos) = new_pos.get(cand.key.as_str())
        {
            return pos;
        }
    }
    // 3. Fallback to any remaining file in the same group
    if let Some(pos) = new_rows
        .iter()
        .position(|r| r.is_file() && r.group_kind() == group_kind)
    {
        return pos;
    }
    // 4. Fallback to next file in any group
    for cand in old_rows.iter().skip(old_selected + 1) {
        if cand.is_file()
            && let Some(&pos) = new_pos.get(cand.key.as_str())
        {
            return pos;
        }
    }
    old_selected.min(new_rows.len() - 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(path: &str, kind: FileStatusKind) -> StageEntry {
        StageEntry {
            path: path.to_string(),
            old_path: None,
            kind,
        }
    }

    #[test]
    fn dual_presence_and_priority_ordering() {
        let mut status = StageStatusSnapshot {
            conflicted: vec![entry("z_conflict.rs", FileStatusKind::Conflicted)],
            staged: vec![entry("foo.rs", FileStatusKind::Added)],
            unstaged: vec![entry("foo.rs", FileStatusKind::Modified)],
            untracked: vec![entry("draft.md", FileStatusKind::Untracked)],
            ignored: vec![entry("target/app", FileStatusKind::Ignored)],
            ..Default::default()
        };
        status
            .conflict_labels
            .insert("z_conflict.rs".into(), "Both Modified".into());

        let rows = build_stage_tree_rows(
            &status,
            None,
            StageTreeOptions::default(),
            &FxHashSet::default(),
        );
        let keys: Vec<&str> = rows.iter().map(|r| r.key.as_str()).collect();
        assert_eq!(
            keys,
            vec![
                "group:Conflicted",
                "file:Conflicted:z_conflict.rs",
                "group:Staged",
                "file:Staged:foo.rs",
                "group:Unstaged",
                "file:Unstaged:draft.md",
                "file:Unstaged:foo.rs",
            ]
        );
    }

    #[test]
    fn untracked_over_50_collapses_into_dedicated_group() {
        let untracked = (0..51)
            .map(|i| entry(&format!("gen/file_{i:03}.txt"), FileStatusKind::Untracked))
            .collect();
        let status = StageStatusSnapshot {
            unstaged: vec![entry("src/lib.rs", FileStatusKind::Modified)],
            untracked,
            ..Default::default()
        };

        let rows = build_stage_tree_rows(
            &status,
            None,
            StageTreeOptions::default(),
            &FxHashSet::default(),
        );
        assert!(rows.iter().any(|r| {
            matches!(
                r.kind,
                StageTreeRowKind::Group {
                    kind: StageNodeKind::Untracked,
                    file_count: 51
                }
            ) && !r.expanded
        }));
        assert!(!rows.iter().any(|r| r.key.starts_with("file:Untracked:")));
    }

    #[test]
    fn repeated_space_advances_cursor_smoothly_across_10_files() {
        let mut unstaged: Vec<StageEntry> = (0..10)
            .map(|i| entry(&format!("f{i}.rs"), FileStatusKind::Modified))
            .collect();
        let mut staged = Vec::new();
        let opts = StageTreeOptions {
            group_by_dir: false,
            ..Default::default()
        };
        let collapsed = FxHashSet::default();

        let snap0 = StageStatusSnapshot {
            staged: staged.clone(),
            unstaged: unstaged.clone(),
            ..Default::default()
        };
        let mut rows = build_stage_tree_rows(&snap0, None, opts, &collapsed);
        // Select f0.rs (index 1, after group:Unstaged at 0)
        let mut sel = rows
            .iter()
            .position(|r| r.key == "file:Unstaged:f0.rs")
            .unwrap();

        for step in 0..9 {
            let cur_path = match &rows[sel].kind {
                StageTreeRowKind::File {
                    group_kind: StageNodeKind::Unstaged,
                    path,
                    ..
                } => path.clone(),
                other => panic!("step {step}: expected Unstaged file, got {other:?}"),
            };
            assert_eq!(cur_path, format!("f{step}.rs"));

            let moved = unstaged.remove(0);
            staged.push(moved);
            let next_snap = StageStatusSnapshot {
                staged: staged.clone(),
                unstaged: unstaged.clone(),
                ..Default::default()
            };
            let new_rows = build_stage_tree_rows(&next_snap, None, opts, &collapsed);
            sel = next_anchor_after_mutation(&rows, sel, &new_rows);
            rows = new_rows;
        }
        assert_eq!(rows[sel].key, "file:Unstaged:f9.rs");
    }

    #[test]
    fn submodule_state_propagates_to_tree_rows_and_targets() {
        let sub_state = SubmoduleState {
            new_commits: true,
            dirty_workdir: true,
            uninitialized: false,
        };
        let mut snap = StageStatusSnapshot {
            unstaged: vec![
                StageEntry {
                    path: "libs/sub".into(),
                    old_path: None,
                    kind: FileStatusKind::Modified,
                },
                StageEntry {
                    path: "src/lib.rs".into(),
                    old_path: None,
                    kind: FileStatusKind::Modified,
                },
            ],
            ..Default::default()
        };
        snap.submodules.insert("libs/sub".into(), sub_state);

        let rows = build_stage_tree_rows(&snap, None, StageTreeOptions::default(), &FxHashSet::default());
        let sub_idx = rows
            .iter()
            .position(|r| r.key == "file:Unstaged:libs/sub")
            .unwrap();
        assert_eq!(rows[sub_idx].submodule(), Some(sub_state));
        assert!(rows[sub_idx].is_submodule());

        let targets = collect_targets_under_row(&rows, sub_idx, &snap, UNTRACKED_COLLAPSE_THRESHOLD);
        assert_eq!(targets.len(), 1);
        assert!(targets[0].is_submodule);

        let group_targets = collect_targets_under_row(&rows, 0, &snap, UNTRACKED_COLLAPSE_THRESHOLD);
        assert!(group_targets.iter().any(|t| t.path == "libs/sub" && t.is_submodule));
        assert!(group_targets.iter().any(|t| t.path == "src/lib.rs" && !t.is_submodule));
    }
}
