//! Commit details, changed-file lists, change trees and diff models for the
//! right-hand Log panes (`CommitDetailsPanel`, `VcsLogChangesBrowser`,
//! `FrameDiffPreview`).

use std::collections::BTreeMap;

use rustc_hash::FxHashSet;

use crate::{FileStatusKind, Oid};

/// What a [`DetailsKey`] represents.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum DetailsMode {
    /// Normal commit inspection (1 commit vs `parent_index`, or net changes of N commits).
    #[default]
    Commits,
    /// Snapshot net diff between two commits (`oids[0]` → `oids[1]`).
    CompareVersions,
    /// Snapshot diff of `oids[0]` against the current working tree.
    CompareWithLocal,
}

/// Identity of a details request: the selected commits (top-to-bottom visible
/// order), which parent a single merge commit is compared against, and mode.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DetailsKey {
    pub oids: Vec<Oid>,
    pub parent_index: usize,
    pub mode: DetailsMode,
}

impl DetailsKey {
    pub fn single(oid: Oid) -> Self {
        Self {
            oids: vec![oid],
            parent_index: 0,
            mode: DetailsMode::Commits,
        }
    }

    pub fn commits(oids: Vec<Oid>, parent_index: usize) -> Self {
        Self {
            oids,
            parent_index,
            mode: DetailsMode::Commits,
        }
    }

    pub fn compare_versions(base: Oid, target: Oid) -> Self {
        Self {
            oids: vec![base, target],
            parent_index: 0,
            mode: DetailsMode::CompareVersions,
        }
    }

    pub fn compare_with_local(oid: Oid) -> Self {
        Self {
            oids: vec![oid],
            parent_index: 0,
            mode: DetailsMode::CompareWithLocal,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PersonStamp {
    pub name: String,
    pub email: String,
    pub time: i64,
    pub offset_minutes: i32,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SignatureStatus {
    #[default]
    Unsigned,
    /// Carries a `gpgsig` header that has not been verified against a keyring.
    SignedUnverified,
    Verified,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CommitDetails {
    pub oid: Oid,
    pub parents: Vec<Oid>,
    pub author: PersonStamp,
    pub committer: PersonStamp,
    /// Full raw message (subject + body).
    pub message: String,
    pub signature: SignatureStatus,
}

impl CommitDetails {
    /// Author and committer are the same person at the same instant.
    pub fn same_author_and_committer(&self) -> bool {
        self.author.name == self.committer.name
            && self.author.email == self.committer.email
            && self.author.time == self.committer.time
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileChange {
    pub path: String,
    pub old_path: Option<String>,
    pub kind: FileStatusKind,
    pub additions: usize,
    pub deletions: usize,
    pub is_binary: bool,
}

/// Result of a details load: metadata for every selected commit plus the
/// (net, for multi-selection) list of changed files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitChangesBundle {
    pub key: DetailsKey,
    pub details: Vec<CommitDetails>,
    pub changes: Vec<FileChange>,
}

impl CommitChangesBundle {
    pub fn totals(&self) -> (usize, usize) {
        self.changes
            .iter()
            .fold((0, 0), |(a, d), c| (a + c.additions, d + c.deletions))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeRowKind {
    Dir { file_count: usize },
    File { change_idx: usize },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangeTreeRow {
    /// Directory path for dirs, file path for files (stable across reloads).
    pub key: String,
    pub label: String,
    pub depth: u16,
    pub kind: ChangeRowKind,
    /// Directories only.
    pub expanded: bool,
}

#[derive(Default)]
struct ChangeDir {
    dirs: BTreeMap<String, ChangeDir>,
    files: Vec<usize>,
}

impl ChangeDir {
    fn count(&self) -> usize {
        self.files.len() + self.dirs.values().map(ChangeDir::count).sum::<usize>()
    }
}

/// Flattens `changes` into rows: `group_by_dir` builds a directory tree with
/// single-child chains compacted (`src/main/rust/graph`); otherwise a flat,
/// path-sorted list. Dirs whose key is in `collapsed` hide their children.
pub fn build_change_rows(
    changes: &[FileChange],
    group_by_dir: bool,
    collapsed: &FxHashSet<String>,
) -> Vec<ChangeTreeRow> {
    let mut order: Vec<usize> = (0..changes.len()).collect();
    order.sort_by(|&a, &b| changes[a].path.cmp(&changes[b].path));

    if !group_by_dir {
        return order
            .into_iter()
            .map(|i| ChangeTreeRow {
                key: changes[i].path.clone(),
                label: changes[i].path.clone(),
                depth: 0,
                kind: ChangeRowKind::File { change_idx: i },
                expanded: false,
            })
            .collect();
    }

    let mut root = ChangeDir::default();
    for i in order {
        let mut cur = &mut root;
        let segments: Vec<&str> = changes[i].path.split('/').collect();
        for seg in &segments[..segments.len().saturating_sub(1)] {
            cur = cur.dirs.entry((*seg).to_string()).or_default();
        }
        cur.files.push(i);
    }

    let mut rows = Vec::new();
    emit_change_dir(&root, "", 0, changes, collapsed, &mut rows);
    rows
}

fn emit_change_dir(
    dir: &ChangeDir,
    prefix: &str,
    depth: u16,
    changes: &[FileChange],
    collapsed: &FxHashSet<String>,
    out: &mut Vec<ChangeTreeRow>,
) {
    for (name, sub) in &dir.dirs {
        let (label, node) = crate::compact_dir_chain(name.clone(), sub, |d| {
            (d.files.is_empty() && d.dirs.len() == 1).then(|| {
                let (child_name, child) = d.dirs.iter().next().expect("one dir");
                (child_name.clone(), child)
            })
        });
        let key = format!("{prefix}{label}");
        let expanded = !collapsed.contains(&key);
        out.push(ChangeTreeRow {
            key: key.clone(),
            label,
            depth,
            kind: ChangeRowKind::Dir {
                file_count: node.count(),
            },
            expanded,
        });
        if expanded {
            emit_change_dir(node, &format!("{key}/"), depth + 1, changes, collapsed, out);
        }
    }
    for &i in &dir.files {
        let path = &changes[i].path;
        out.push(ChangeTreeRow {
            key: path.clone(),
            label: path.rsplit('/').next().unwrap_or(path).to_string(),
            depth,
            kind: ChangeRowKind::File { change_idx: i },
            expanded: false,
        });
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum DiffContext {
    #[default]
    Three,
    Ten,
    Full,
}

impl DiffContext {
    pub const fn lines(self) -> u32 {
        match self {
            Self::Three => 3,
            Self::Ten => 10,
            Self::Full => 1_000_000,
        }
    }

    pub const fn next(self) -> Self {
        match self {
            Self::Three => Self::Ten,
            Self::Ten => Self::Full,
            Self::Full => Self::Three,
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Three => "3",
            Self::Ten => "10",
            Self::Full => "Full",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffLineKind {
    Context,
    Added,
    Deleted,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffLine {
    pub kind: DiffLineKind,
    pub old_lineno: Option<u32>,
    pub new_lineno: Option<u32>,
    pub text: String,
    /// Byte ranges of changed words (`ByWord` inline highlighting).
    pub word_ranges: Vec<(usize, usize)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffHunk {
    pub header: String,
    pub lines: Vec<DiffLine>,
}

/// Identity of a file diff request.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FileDiffKey {
    pub details: DetailsKey,
    pub path: String,
    pub old_path: Option<String>,
    pub context: DiffContext,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDiff {
    pub key: FileDiffKey,
    pub hunks: Vec<DiffHunk>,
    pub is_binary: bool,
    /// Diff exceeded the line budget and was truncated.
    pub is_truncated: bool,
}

impl FileDiff {
    pub fn line_count(&self) -> usize {
        self.hunks.iter().map(|h| h.lines.len() + 1).sum()
    }
}

/// One aligned row of a side-by-side diff: indices into `hunk.lines`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SideBySideRow {
    pub left: Option<usize>,
    pub right: Option<usize>,
}

/// Aligns deletions with following additions; context lines appear on both sides.
pub fn side_by_side_rows(hunk: &DiffHunk) -> Vec<SideBySideRow> {
    let mut rows = Vec::with_capacity(hunk.lines.len());
    let lines = &hunk.lines;
    let mut i = 0;
    while i < lines.len() {
        match lines[i].kind {
            DiffLineKind::Context => {
                rows.push(SideBySideRow {
                    left: Some(i),
                    right: Some(i),
                });
                i += 1;
            }
            _ => {
                let del_start = i;
                while i < lines.len() && lines[i].kind == DiffLineKind::Deleted {
                    i += 1;
                }
                let add_start = i;
                while i < lines.len() && lines[i].kind == DiffLineKind::Added {
                    i += 1;
                }
                let dels = add_start - del_start;
                let adds = i - add_start;
                for k in 0..dels.max(adds) {
                    rows.push(SideBySideRow {
                        left: (k < dels).then_some(del_start + k),
                        right: (k < adds).then_some(add_start + k),
                    });
                }
            }
        }
    }
    rows
}

/// Fills `word_ranges` on paired deleted/added lines (`ByWord` highlighting).
pub fn apply_word_diff(hunk: &mut DiffHunk) {
    let pairs: Vec<(usize, usize)> = side_by_side_rows(hunk)
        .into_iter()
        .filter_map(|r| match (r.left, r.right) {
            (Some(l), Some(r)) if l != r => Some((l, r)),
            _ => None,
        })
        .collect();
    for (l, r) in pairs {
        let (old_ranges, new_ranges) = word_diff(&hunk.lines[l].text, &hunk.lines[r].text);
        hunk.lines[l].word_ranges = old_ranges;
        hunk.lines[r].word_ranges = new_ranges;
    }
}

fn tokenize(s: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut iter = s.char_indices().peekable();
    while let Some((start, c)) = iter.next() {
        let class = |c: char| -> u8 {
            if c.is_alphanumeric() || c == '_' {
                0
            } else if c.is_whitespace() {
                1
            } else {
                2
            }
        };
        let cls = class(c);
        let mut end = start + c.len_utf8();
        if cls != 2 {
            while let Some(&(i, n)) = iter.peek() {
                if class(n) != cls {
                    break;
                }
                end = i + n.len_utf8();
                iter.next();
            }
        }
        out.push((start, end));
    }
    out
}

const WORD_DIFF_MAX_TOKENS: usize = 256;

/// Token-level LCS diff; returns changed byte ranges for `old` and `new`.
/// Lines with nothing in common get no inline highlight (the line color says it all).
/// Byte ranges `(start, end)` within a line.
pub type ByteRanges = Vec<(usize, usize)>;

pub fn word_diff(old: &str, new: &str) -> (ByteRanges, ByteRanges) {
    let a = tokenize(old);
    let b = tokenize(new);
    if a.len() > WORD_DIFF_MAX_TOKENS || b.len() > WORD_DIFF_MAX_TOKENS {
        return (Vec::new(), Vec::new());
    }
    let tok_a = |i: usize| &old[a[i].0..a[i].1];
    let tok_b = |j: usize| &new[b[j].0..b[j].1];
    let (n, m) = (a.len(), b.len());
    let mut dp = vec![0u16; (n + 1) * (m + 1)];
    let idx = |i: usize, j: usize| i * (m + 1) + j;
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            dp[idx(i, j)] = if tok_a(i) == tok_b(j) {
                dp[idx(i + 1, j + 1)] + 1
            } else {
                dp[idx(i + 1, j)].max(dp[idx(i, j + 1)])
            };
        }
    }
    let common_non_ws = {
        let (mut i, mut j, mut count) = (0, 0, 0usize);
        let mut keep_a = vec![false; n];
        let mut keep_b = vec![false; m];
        while i < n && j < m {
            if tok_a(i) == tok_b(j) {
                keep_a[i] = true;
                keep_b[j] = true;
                if !tok_a(i).trim().is_empty() {
                    count += 1;
                }
                i += 1;
                j += 1;
            } else if dp[idx(i + 1, j)] >= dp[idx(i, j + 1)] {
                i += 1;
            } else {
                j += 1;
            }
        }
        (count, keep_a, keep_b)
    };
    let (count, keep_a, keep_b) = common_non_ws;
    if count == 0 {
        return (Vec::new(), Vec::new());
    }
    let ranges = |toks: &[(usize, usize)], keep: &[bool]| {
        let mut out: Vec<(usize, usize)> = Vec::new();
        for (k, &(s, e)) in toks.iter().enumerate() {
            if keep[k] {
                continue;
            }
            match out.last_mut() {
                Some(last) if last.1 == s => last.1 = e,
                _ => out.push((s, e)),
            }
        }
        out
    };
    (ranges(&a, &keep_a), ranges(&b, &keep_b))
}

/// Classification of a commit message line for rich rendering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageLineKind {
    Subject,
    Body,
    /// `Co-authored-by:` / `Signed-off-by:` / `Reviewed-by:` style trailer.
    Trailer,
}

pub fn classify_message_line(idx: usize, line: &str) -> MessageLineKind {
    if idx == 0 {
        return MessageLineKind::Subject;
    }
    let t = line.trim_start();
    if let Some((key, rest)) = t.split_once(':')
        && !key.is_empty()
        && !rest.is_empty()
        && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        && key.contains('-')
        && key.ends_with("-by")
    {
        return MessageLineKind::Trailer;
    }
    MessageLineKind::Body
}

/// Byte ranges of `#123` issue references.
pub fn issue_ref_ranges(line: &str) -> Vec<(usize, usize)> {
    let bytes = line.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'#'
            && (i == 0 || !bytes[i - 1].is_ascii_alphanumeric())
            && i + 1 < bytes.len()
            && bytes[i + 1].is_ascii_digit()
        {
            let start = i;
            i += 1;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            out.push((start, i));
        } else {
            i += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fc(path: &str, kind: FileStatusKind) -> FileChange {
        FileChange {
            path: path.to_string(),
            old_path: None,
            kind,
            additions: 1,
            deletions: 0,
            is_binary: false,
        }
    }

    #[test]
    fn change_tree_compacts_middle_packages_and_flat_mode_sorts() {
        let changes = vec![
            fc("src/main/rust/graph/a.rs", FileStatusKind::Modified),
            fc("src/main/rust/graph/b.rs", FileStatusKind::Added),
            fc("README.md", FileStatusKind::Modified),
            fc("docs/x.md", FileStatusKind::Deleted),
        ];
        let rows = build_change_rows(&changes, true, &FxHashSet::default());
        let got: Vec<String> = rows
            .iter()
            .map(|r| format!("{}{}", "  ".repeat(r.depth as usize), r.label))
            .collect();
        assert_eq!(
            got,
            vec![
                "docs",
                "  x.md",
                "src/main/rust/graph",
                "  a.rs",
                "  b.rs",
                "README.md"
            ]
        );
        assert_eq!(rows[2].kind, ChangeRowKind::Dir { file_count: 2 });

        let mut collapsed = FxHashSet::default();
        collapsed.insert("src/main/rust/graph".to_string());
        assert_eq!(build_change_rows(&changes, true, &collapsed).len(), 4);

        let flat = build_change_rows(&changes, false, &FxHashSet::default());
        assert_eq!(flat[0].label, "README.md");
        assert_eq!(flat.len(), 4);
    }

    #[test]
    fn word_diff_highlights_only_changed_tokens() {
        let (old, new) = word_diff("let x = compute(a, b);", "let x = compute(a, c);");
        assert_eq!(&"let x = compute(a, b);"[old[0].0..old[0].1], "b");
        assert_eq!(&"let x = compute(a, c);"[new[0].0..new[0].1], "c");
        assert_eq!(old.len(), 1);
        let (o, n) = word_diff("abc", "xyz");
        assert!(o.is_empty() && n.is_empty());
    }

    #[test]
    fn side_by_side_pairs_runs_and_word_diff_applies() {
        let line = |kind, text: &str| DiffLine {
            kind,
            old_lineno: None,
            new_lineno: None,
            text: text.to_string(),
            word_ranges: Vec::new(),
        };
        let mut hunk = DiffHunk {
            header: "@@".into(),
            lines: vec![
                line(DiffLineKind::Context, "fn main() {"),
                line(DiffLineKind::Deleted, "    old_call(1);"),
                line(DiffLineKind::Added, "    new_call(1);"),
                line(DiffLineKind::Added, "    extra();"),
                line(DiffLineKind::Context, "}"),
            ],
        };
        let rows = side_by_side_rows(&hunk);
        assert_eq!(rows.len(), 4);
        assert_eq!(
            rows[1],
            SideBySideRow {
                left: Some(1),
                right: Some(2)
            }
        );
        assert_eq!(
            rows[2],
            SideBySideRow {
                left: None,
                right: Some(3)
            }
        );
        apply_word_diff(&mut hunk);
        assert_eq!(hunk.lines[1].word_ranges, vec![(4, 12)]);
        assert!(hunk.lines[3].word_ranges.is_empty());
    }

    #[test]
    fn message_trailers_and_issue_refs() {
        assert_eq!(
            classify_message_line(0, "Fix #12"),
            MessageLineKind::Subject
        );
        assert_eq!(
            classify_message_line(3, "Co-authored-by: A <a@b>"),
            MessageLineKind::Trailer
        );
        assert_eq!(classify_message_line(2, "Note: x"), MessageLineKind::Body);
        assert_eq!(
            issue_ref_ranges("fixes #123 and a#4, #5"),
            vec![(6, 10), (20, 22)]
        );
    }
}
