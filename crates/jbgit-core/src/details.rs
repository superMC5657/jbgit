//! Commit details, changed-file lists, change trees and diff models for the
//! right-hand Log panes (`CommitDetailsPanel`, `VcsLogChangesBrowser`,
//! `FrameDiffPreview`).

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

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

/// Maximum single-selection line span before prompting for confirmation.
pub const MAX_SELECTION_HISTORY_LINES: u32 = 2_000;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct BlameOptions {
    pub ignore_whitespace: bool,
    pub detect_moves: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BlameKey {
    /// Repository-relative path (POSIX `/` separated).
    pub path: String,
    /// Target commit (`None` annotates the working tree file).
    pub rev: Option<Oid>,
    pub options: BlameOptions,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlameLine {
    pub final_line: u32,
    pub orig_line: u32,
    pub oid: Oid,
    pub orig_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlameCommit {
    pub oid: Oid,
    pub author: PersonStamp,
    pub summary: String,
    pub previous: Option<(Oid, String)>,
    pub is_boundary: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlameBundle {
    pub key: BlameKey,
    pub lines: Vec<BlameLine>,
    pub commits: HashMap<Oid, BlameCommit>,
    pub content: Arc<str>,
    pub uncommitted_oid_present: bool,
}

impl BlameBundle {
    /// Returns file lines aligned 1-to-1 with [`Self::lines`] (preserving any
    /// trailing empty line that `str::lines()` would strip).
    pub fn content_lines(&self) -> Vec<&str> {
        if self.lines.is_empty() {
            Vec::new()
        } else {
            self.content.split('\n').collect()
        }
    }

    pub fn commit_for(&self, line_idx: usize) -> Option<&BlameCommit> {
        let line = self.lines.get(line_idx)?;
        self.commits.get(&line.oid)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FileHistoryKey {
    /// Query relative path from repository root (POSIX `/` separated).
    pub path: String,
    /// Starting commit (`None` starts from `HEAD`).
    pub start_oid: Option<Oid>,
    /// Whether to track renames with `git log --follow`.
    pub follow_renames: bool,
    /// Optional 1-based inclusive line range (`git log -L <start>,<end>:<path>`).
    pub line_range: Option<(u32, u32)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileHistoryEntry {
    pub oid: Oid,
    /// Path of the file at this commit (may differ from `key.path` prior to rename).
    pub path_at_commit: String,
    /// Previous path if this commit renamed or copied the file.
    pub old_path: Option<String>,
    pub kind: FileStatusKind,
    pub additions: usize,
    pub deletions: usize,
    pub is_binary: bool,
    pub author_name: String,
    pub author_time: i64,
    pub summary: String,
    /// Cached range patch produced by `git log -L` for selection history.
    pub range_patch: Option<Arc<str>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileHistoryBundle {
    pub key: FileHistoryKey,
    pub entries: Vec<FileHistoryEntry>,
    /// True when the result hit the max entry limit (e.g. 500).
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileChange {
    pub path: String,
    pub old_path: Option<String>,
    pub kind: FileStatusKind,
    pub additions: usize,
    pub deletions: usize,
    pub is_binary: bool,
    pub exists_in_workdir: bool,
    pub old_oid: Option<Oid>,
    pub new_oid: Option<Oid>,
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
    pub const fn from_lines(lines: usize) -> Self {
        if lines >= 100 {
            Self::Full
        } else if lines >= 10 {
            Self::Ten
        } else {
            Self::Three
        }
    }

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

/// Hard ceiling (4 MiB) on single-file diff preview / three-doc tracker loading.
pub const MAX_DIFF_FILE_BYTES: usize = 4 * 1024 * 1024;
/// Maximum line pairs per diff for eager inline word-diff calculation.
pub const EAGER_WORD_DIFF_PAIR_BUDGET: usize = 256;
/// Maximum single-line byte length eligible for inline word-diff tokenization.
pub const MAX_WORD_DIFF_LINE_BYTES: usize = 2048;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDiff {
    pub key: FileDiffKey,
    pub hunks: Vec<DiffHunk>,
    pub is_binary: bool,
    /// Diff exceeded the line budget and was truncated.
    pub is_truncated: bool,
    /// Either side exceeded [`MAX_DIFF_FILE_BYTES`] and full diffing was skipped.
    pub is_too_large: bool,
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
    let mut unlimited = usize::MAX;
    apply_word_diff_with_budget(hunk, &mut unlimited);
}

/// Fills `word_ranges` on paired deleted/added lines up to `*pair_budget` pairs.
pub fn apply_word_diff_with_budget(hunk: &mut DiffHunk, pair_budget: &mut usize) {
    if *pair_budget == 0 {
        return;
    }
    let pairs: Vec<(usize, usize)> = side_by_side_rows(hunk)
        .into_iter()
        .filter_map(|r| match (r.left, r.right) {
            (Some(l), Some(r)) if l != r => Some((l, r)),
            _ => None,
        })
        .take(*pair_budget)
        .collect();
    *pair_budget = pair_budget.saturating_sub(pairs.len());
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
/// Lines with nothing in common or exceeding [`MAX_WORD_DIFF_LINE_BYTES`] get no inline highlight.
/// Byte ranges `(start, end)` within a line.
pub type ByteRanges = Vec<(usize, usize)>;

pub fn word_diff(old: &str, new: &str) -> (ByteRanges, ByteRanges) {
    if old.len() > MAX_WORD_DIFF_LINE_BYTES || new.len() > MAX_WORD_DIFF_LINE_BYTES {
        return (Vec::new(), Vec::new());
    }
    let a = tokenize(old);
    let b = tokenize(new);
    let tok_a = |i: usize| &old[a[i].0..a[i].1];
    let tok_b = |j: usize| &new[b[j].0..b[j].1];

    let max_common = a.len().min(b.len());
    let mut prefix = 0usize;
    let mut fixed_non_ws = 0usize;
    while prefix < max_common && tok_a(prefix) == tok_b(prefix) {
        if !tok_a(prefix).trim().is_empty() {
            fixed_non_ws += 1;
        }
        prefix += 1;
    }
    let mut suffix = 0usize;
    while suffix < max_common - prefix
        && tok_a(a.len() - 1 - suffix) == tok_b(b.len() - 1 - suffix)
    {
        if !tok_a(a.len() - 1 - suffix).trim().is_empty() {
            fixed_non_ws += 1;
        }
        suffix += 1;
    }

    let n = a.len() - prefix - suffix;
    let m = b.len() - prefix - suffix;
    if n == 0 && m == 0 {
        return (Vec::new(), Vec::new());
    }
    if n > WORD_DIFF_MAX_TOKENS || m > WORD_DIFF_MAX_TOKENS {
        return (Vec::new(), Vec::new());
    }

    let mut dp = vec![0u16; (n + 1) * (m + 1)];
    let idx = |i: usize, j: usize| i * (m + 1) + j;
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            dp[idx(i, j)] = if tok_a(prefix + i) == tok_b(prefix + j) {
                dp[idx(i + 1, j + 1)] + 1
            } else {
                dp[idx(i + 1, j)].max(dp[idx(i, j + 1)])
            };
        }
    }
    let (mid_count, keep_a, keep_b) = {
        let (mut i, mut j, mut count) = (0, 0, 0usize);
        let mut keep_a = vec![false; n];
        let mut keep_b = vec![false; m];
        while i < n && j < m {
            if tok_a(prefix + i) == tok_b(prefix + j) {
                keep_a[i] = true;
                keep_b[j] = true;
                if !tok_a(prefix + i).trim().is_empty() {
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
    if fixed_non_ws + mid_count == 0 {
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
    (
        ranges(&a[prefix..a.len() - suffix], &keep_a),
        ranges(&b[prefix..b.len() - suffix], &keep_b),
    )
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

struct ParsedHunkHeader {
    prefix_width: usize,
    is_combined: bool,
    old_start: u32,
    old_count: u32,
    new_start: u32,
    new_count: u32,
}

fn parse_range_spec(spec: &str) -> Option<(u32, u32)> {
    let mut parts = spec.split(',');
    let start = parts.next()?.parse::<u32>().ok()?;
    let count = match parts.next() {
        Some(c) => c.parse::<u32>().ok()?,
        None => 1,
    };
    Some((start, count))
}

fn parse_patch_hunk_header(line: &str) -> Option<ParsedHunkHeader> {
    let at_count = line.bytes().take_while(|&b| b == b'@').count();
    if at_count < 2 {
        return None;
    }
    let after_open = &line[at_count..];
    if !after_open.starts_with(' ') {
        return None;
    }
    let closing = "@".repeat(at_count);
    let end_rel = after_open.find(&closing)?;
    let inside = after_open[..end_rel].trim();
    let mut tokens = inside.split_ascii_whitespace();
    let first = tokens.next()?;
    let (old_start, old_count) = first
        .strip_prefix('-')
        .and_then(parse_range_spec)
        .unwrap_or((1, 0));

    let mut new_spec: Option<(u32, u32)> = None;
    for tok in std::iter::once(first).chain(tokens) {
        if let Some(rest) = tok.strip_prefix('+') {
            new_spec = parse_range_spec(rest);
        }
    }
    let (new_start, new_count) = new_spec?;

    Some(ParsedHunkHeader {
        prefix_width: at_count - 1,
        is_combined: at_count > 2,
        old_start,
        old_count,
        new_start,
        new_count,
    })
}

/// Parses unified (`@@ ... @@`) and combined merge (`@@@ ... @@@`) diff text
/// (such as `git log -L` range patches) into [`DiffHunk`]s with eager word diffs.
pub fn parse_unified_patch_hunks(patch: &str) -> Vec<DiffHunk> {
    let mut hunks = Vec::new();
    let mut current: Option<DiffHunk> = None;
    let mut prefix_width = 1usize;
    let mut is_combined = false;
    let mut old_no = 1u32;
    let mut new_no = 1u32;
    let mut old_rem = 0u32;
    let mut new_rem = 0u32;
    let mut word_budget = EAGER_WORD_DIFF_PAIR_BUDGET;

    let finish_hunk = |cur: &mut Option<DiffHunk>, out: &mut Vec<DiffHunk>, budget: &mut usize| {
        if let Some(mut h) = cur.take() {
            apply_word_diff_with_budget(&mut h, budget);
            out.push(h);
        }
    };

    for raw_line in patch.split('\n') {
        let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
        if let Some(hdr) = parse_patch_hunk_header(line) {
            finish_hunk(&mut current, &mut hunks, &mut word_budget);
            prefix_width = hdr.prefix_width;
            is_combined = hdr.is_combined;
            old_no = hdr.old_start;
            new_no = hdr.new_start;
            old_rem = hdr.old_count;
            new_rem = hdr.new_count;
            current = Some(DiffHunk {
                header: line.to_string(),
                lines: Vec::new(),
            });
            continue;
        }

        if current.is_none() {
            continue;
        }
        if line == "\\ No newline at end of file" {
            continue;
        }
        if line.starts_with("diff --git ")
            || line.starts_with("diff --cc ")
            || line.starts_with("diff --combined ")
        {
            finish_hunk(&mut current, &mut hunks, &mut word_budget);
            continue;
        }
        if !is_combined && old_rem == 0 && new_rem == 0 {
            finish_hunk(&mut current, &mut hunks, &mut word_budget);
            continue;
        }
        if line.len() < prefix_width || !line.is_char_boundary(prefix_width) {
            finish_hunk(&mut current, &mut hunks, &mut word_budget);
            continue;
        }
        let (prefix, text) = line.split_at(prefix_width);
        if !prefix.bytes().all(|b| matches!(b, b' ' | b'+' | b'-')) {
            finish_hunk(&mut current, &mut hunks, &mut word_budget);
            continue;
        }

        let kind = if prefix.contains('+') {
            DiffLineKind::Added
        } else if prefix.contains('-') {
            DiffLineKind::Deleted
        } else {
            DiffLineKind::Context
        };

        let (line_old, line_new) = match kind {
            DiffLineKind::Context => {
                let o = (!is_combined).then_some(old_no);
                let n = new_no;
                old_no = old_no.saturating_add(1);
                new_no = new_no.saturating_add(1);
                old_rem = old_rem.saturating_sub(1);
                new_rem = new_rem.saturating_sub(1);
                (o, Some(n))
            }
            DiffLineKind::Added => {
                let n = new_no;
                new_no = new_no.saturating_add(1);
                new_rem = new_rem.saturating_sub(1);
                (None, Some(n))
            }
            DiffLineKind::Deleted => {
                let o = (!is_combined).then_some(old_no);
                old_no = old_no.saturating_add(1);
                old_rem = old_rem.saturating_sub(1);
                (o, None)
            }
        };

        if let Some(h) = current.as_mut() {
            h.lines.push(DiffLine {
                kind,
                old_lineno: line_old,
                new_lineno: line_new,
                text: text.to_string(),
                word_ranges: Vec::new(),
            });
        }
    }

    finish_hunk(&mut current, &mut hunks, &mut word_budget);
    hunks
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
            exists_in_workdir: kind != FileStatusKind::Deleted,
            old_oid: None,
            new_oid: None,
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

        // Common prefix/suffix trimming keeps >256-token lines fast when few tokens differ,
        // while lines over MAX_WORD_DIFF_LINE_BYTES short-circuit cleanly.
        let long_prefix = "x ".repeat(200);
        let a_long = format!("{long_prefix}alpha tail");
        let b_long = format!("{long_prefix}omega tail");
        let (ol, nl) = word_diff(&a_long, &b_long);
        assert_eq!(&a_long[ol[0].0..ol[0].1], "alpha");
        assert_eq!(&b_long[nl[0].0..nl[0].1], "omega");

        let huge_a = format!("{}alpha", "a".repeat(MAX_WORD_DIFF_LINE_BYTES + 16));
        let huge_b = format!("{}omega", "a".repeat(MAX_WORD_DIFF_LINE_BYTES + 16));
        let (oh, nh) = word_diff(&huge_a, &huge_b);
        assert!(oh.is_empty() && nh.is_empty());
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

    #[test]
    fn parse_unified_patch_hunks_handles_unified_combined_and_empty() {
        assert!(parse_unified_patch_hunks("").is_empty());

        let patch = "\
diff --git a/src/lib.rs b/src/lib.rs
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -5 +5 @@ fn single() {
-    old_val(1);
+    new_val(1);
@@ -10,3 +10,4 @@ fn multi() {
 let a = 1;
-let b = 2;
+let b = 3;
+let c = 4;
\\ No newline at end of file
";
        let hunks = parse_unified_patch_hunks(patch);
        assert_eq!(hunks.len(), 2);
        assert_eq!(hunks[0].lines.len(), 2);
        assert_eq!(hunks[0].lines[0].kind, DiffLineKind::Deleted);
        assert_eq!(hunks[0].lines[0].old_lineno, Some(5));
        assert_eq!(hunks[0].lines[0].new_lineno, None);
        assert!(!hunks[0].lines[0].word_ranges.is_empty());
        assert_eq!(hunks[0].lines[1].kind, DiffLineKind::Added);
        assert_eq!(hunks[0].lines[1].new_lineno, Some(5));

        assert_eq!(hunks[1].lines.len(), 4);
        assert_eq!(hunks[1].lines[0].old_lineno, Some(10));
        assert_eq!(hunks[1].lines[0].new_lineno, Some(10));
        assert_eq!(hunks[1].lines[1].old_lineno, Some(11));
        assert_eq!(hunks[1].lines[2].new_lineno, Some(11));
        assert_eq!(hunks[1].lines[3].new_lineno, Some(12));

        let combined = "\
diff --cc src/lib.rs
@@@ -1,3 -1,3 +1,4 @@@
  fn keep() {
 +    added_branch();
 -    dropped_branch();
  }
";
        let ch = parse_unified_patch_hunks(combined);
        assert_eq!(ch.len(), 1);
        assert_eq!(ch[0].lines.len(), 4);
        assert_eq!(ch[0].lines[0].kind, DiffLineKind::Context);
        assert_eq!(ch[0].lines[0].new_lineno, Some(1));
        assert_eq!(ch[0].lines[1].kind, DiffLineKind::Added);
        assert_eq!(ch[0].lines[1].new_lineno, Some(2));
        assert_eq!(ch[0].lines[2].kind, DiffLineKind::Deleted);
        assert_eq!(ch[0].lines[2].new_lineno, None);
        assert_eq!(ch[0].lines[3].kind, DiffLineKind::Context);
        assert_eq!(ch[0].lines[3].new_lineno, Some(3));
    }
}
