//! Commit details, changed-file lists, change trees and diff models for the
//! right-hand Log panes (`CommitDetailsPanel`, `VcsLogChangesBrowser`,
//! `FrameDiffPreview`).

use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use rustc_hash::FxHashSet;

use crate::{CommitSummary, FileStatusKind, Oid};

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

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum SignatureStatus {
    /// `N` — no signature.
    #[default]
    Unsigned,
    /// Commit carries a signature header; async `git log` verification is pending.
    Pending,
    /// `G` — good (valid) signature.
    Good,
    /// `U` — good signature with unknown validity.
    GoodUnknownValidity,
    /// `X` — expired signature (`key_expired: false`), `Y` — expired key (`key_expired: true`).
    Expired { key_expired: bool },
    /// `R` — good signature made by a revoked key.
    Revoked,
    /// `E` — signature cannot be checked (missing key / verifier unavailable).
    CannotCheck,
    /// `B` — bad signature.
    Bad,
}

impl SignatureStatus {
    /// Maps a `git log --format=%G?` status code to [`SignatureStatus`].
    pub fn from_git_code(code: &str) -> Option<Self> {
        match code.trim() {
            "G" => Some(Self::Good),
            "U" => Some(Self::GoodUnknownValidity),
            "X" => Some(Self::Expired { key_expired: false }),
            "Y" => Some(Self::Expired { key_expired: true }),
            "R" => Some(Self::Revoked),
            "E" => Some(Self::CannotCheck),
            "B" => Some(Self::Bad),
            "N" => Some(Self::Unsigned),
            _ => None,
        }
    }

    pub const fn is_signed(self) -> bool {
        !matches!(self, Self::Unsigned)
    }

    /// 1-column terminal icon for Log column and Details badge.
    pub const fn icon(self) -> &'static str {
        match self {
            Self::Good => "✓",
            Self::GoodUnknownValidity | Self::Expired { .. } => "⚠",
            Self::Revoked | Self::Bad => "✕",
            Self::CannotCheck => "?",
            Self::Pending => "…",
            Self::Unsigned => " ",
        }
    }
}

/// Verified commit signature metadata parsed from `%G?`, `%GS`, and `%GK`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CommitSignature {
    pub status: SignatureStatus,
    pub signer: Option<String>,
    pub key: Option<String>,
}

impl CommitSignature {
    pub const fn unsigned() -> Self {
        Self {
            status: SignatureStatus::Unsigned,
            signer: None,
            key: None,
        }
    }

    pub const fn pending() -> Self {
        Self {
            status: SignatureStatus::Pending,
            signer: None,
            key: None,
        }
    }

    pub const fn cannot_check() -> Self {
        Self {
            status: SignatureStatus::CannotCheck,
            signer: None,
            key: None,
        }
    }

    /// Parses `%G?`, `%GS`, `%GK` field values into a [`CommitSignature`].
    pub fn parse_format_fields(code: &str, signer: &str, key: &str) -> Self {
        let code = code.trim();
        if code.is_empty() || code == "N" {
            return Self::unsigned();
        }
        let non_empty = |s: &str| {
            let trimmed = s.trim();
            (!trimmed.is_empty()).then(|| trimmed.to_string())
        };
        Self {
            status: SignatureStatus::from_git_code(code).unwrap_or(SignatureStatus::CannotCheck),
            signer: non_empty(signer),
            key: non_empty(key),
        }
    }

    /// Parses a single `%G?%x1f%GS%x1f%GK` line.
    pub fn parse_single_format_line(line: &str) -> Self {
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            return Self::cannot_check();
        }
        let mut parts = trimmed.splitn(3, '\x1f');
        let code = parts.next().unwrap_or("");
        let signer = parts.next().unwrap_or("");
        let key = parts.next().unwrap_or("");
        Self::parse_format_fields(code, signer, key)
    }

    /// Parses `%H%x1f%G?%x1f%GS%x1f%GK` lines from `git log --no-walk --stdin`.
    pub fn parse_batch_format_output(stdout: &str) -> Vec<(Oid, Self)> {
        let mut out = Vec::new();
        for raw in stdout.lines() {
            let line = raw.trim_end_matches('\r');
            if line.is_empty() {
                continue;
            }
            let mut parts = line.splitn(4, '\x1f');
            let Some(oid) = parts.next().and_then(|h| h.trim().parse::<Oid>().ok()) else {
                continue;
            };
            let code = parts.next().unwrap_or("");
            let signer = parts.next().unwrap_or("");
            let key = parts.next().unwrap_or("");
            out.push((oid, Self::parse_format_fields(code, signer, key)));
        }
        out
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CommitDetails {
    pub oid: Oid,
    pub parents: Vec<Oid>,
    pub author: PersonStamp,
    pub committer: PersonStamp,
    /// Full raw message (subject + body).
    pub message: String,
    pub signature: CommitSignature,
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

/// Maximum number of commits loaded into the Push outgoing preview list.
pub const MAX_OUTGOING_COMMITS: usize = 1_000;

/// Identity of an outgoing-commits query (`remote/remote_branch..branch`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct OutgoingKey {
    pub branch: String,
    pub remote: String,
    pub remote_branch: String,
    pub up_to: Option<Oid>,
}

/// Commits and net changed-file count pending push for [`OutgoingKey`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutgoingBundle {
    pub key: OutgoingKey,
    pub commits: Vec<CommitSummary>,
    pub files_changed: usize,
    pub remote_exists: bool,
    /// Last-known OID of `refs/remotes/<remote>/<remote_branch>` when it exists.
    pub remote_oid: Option<Oid>,
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
    build_change_rows_filtered(changes, group_by_dir, collapsed, |_| true)
}

/// Like [`build_change_rows`], retaining only entries satisfying `keep` while
/// preserving original `change_idx` indices into `changes`.
pub fn build_change_rows_filtered(
    changes: &[FileChange],
    group_by_dir: bool,
    collapsed: &FxHashSet<String>,
    mut keep: impl FnMut(&FileChange) -> bool,
) -> Vec<ChangeTreeRow> {
    let mut order: Vec<usize> = (0..changes.len()).filter(|&i| keep(&changes[i])).collect();
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

/// Whitespace comparison policy for diff computation (`git diff` / `-b` / `-w`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum WhitespaceMode {
    /// Compare lines byte-for-byte.
    #[default]
    Default,
    /// Ignore changes in amount of horizontal whitespace and trailing whitespace (`-b`).
    IgnoreChange,
    /// Ignore all horizontal whitespace (`-w`).
    IgnoreAll,
}

impl WhitespaceMode {
    pub const fn next(self) -> Self {
        match self {
            Self::Default => Self::IgnoreChange,
            Self::IgnoreChange => Self::IgnoreAll,
            Self::IgnoreAll => Self::Default,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::IgnoreChange => "ignore-change",
            Self::IgnoreAll => "ignore-all",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "default" | "none" | "exact" => Some(Self::Default),
            "ignore-change" | "ignore_change" | "change" | "-b" | "b" => Some(Self::IgnoreChange),
            "ignore-all" | "ignore_all" | "all" | "-w" | "w" => Some(Self::IgnoreAll),
            _ => None,
        }
    }

    pub const fn short_flag(self) -> &'static str {
        match self {
            Self::Default => "",
            Self::IgnoreChange => "-b",
            Self::IgnoreAll => "-w",
        }
    }

    /// Normalizes a single line (with or without trailing `\r`/`\n`) for diff comparison
    /// while leaving display text untouched.
    pub fn normalize_line<'a>(self, line: &'a [u8]) -> Cow<'a, [u8]> {
        let is_hws = |b: u8| matches!(b, b' ' | b'\t' | b'\r');
        match self {
            Self::Default => Cow::Borrowed(line),
            Self::IgnoreChange => {
                let mut end = line.len();
                while end > 0 && matches!(line[end - 1], b' ' | b'\t' | b'\r' | b'\n') {
                    end -= 1;
                }
                let trimmed = &line[..end];
                let mut out = Vec::with_capacity(trimmed.len());
                let mut in_ws = false;
                for &b in trimmed {
                    if is_hws(b) {
                        if !in_ws {
                            out.push(b' ');
                            in_ws = true;
                        }
                    } else {
                        out.push(b);
                        in_ws = false;
                    }
                }
                Cow::Owned(out)
            }
            Self::IgnoreAll => {
                let mut end = line.len();
                while end > 0 && matches!(line[end - 1], b'\r' | b'\n') {
                    end -= 1;
                }
                let mut out = Vec::with_capacity(end);
                for &b in &line[..end] {
                    if !is_hws(b) {
                        out.push(b);
                    }
                }
                Cow::Owned(out)
            }
        }
    }
}

/// Formats a byte count into a concise human-readable unit (`B` / `KiB` / `MiB` / `GiB`).
pub fn format_byte_size(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = 1024.0 * 1024.0;
    const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
    let b = bytes as f64;
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KiB", b / KIB)
    } else if bytes < 1024 * 1024 * 1024 {
        format!("{:.1} MiB", b / MIB)
    } else {
        format!("{:.2} GiB", b / GIB)
    }
}

/// Formats a signed byte size delta (`+2.4 KiB`, `-512 B`, `±0 B`).
pub fn format_signed_byte_delta(delta: i128) -> String {
    if delta == 0 {
        return "±0 B".to_string();
    }
    let sign = if delta > 0 { '+' } else { '-' };
    format!("{sign}{}", format_byte_size(delta.unsigned_abs() as u64))
}

/// Binary file metadata surfaced in Diff panels instead of a static placeholder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BinaryDiffInfo {
    pub old_size: Option<u64>,
    pub new_size: Option<u64>,
    pub old_oid: Option<Oid>,
    pub new_oid: Option<Oid>,
}

impl BinaryDiffInfo {
    /// Formats `12.4 KiB → 14.8 KiB (+2.4 KiB)` (using `missing_label` when a side is absent).
    pub fn size_summary(&self, missing_label: &str) -> String {
        match (self.old_size, self.new_size) {
            (Some(old), Some(new)) => {
                let delta = i128::from(new) - i128::from(old);
                format!(
                    "{} → {} ({})",
                    format_byte_size(old),
                    format_byte_size(new),
                    format_signed_byte_delta(delta)
                )
            }
            (None, Some(new)) => {
                format!(
                    "{missing_label} → {} (+{})",
                    format_byte_size(new),
                    format_byte_size(new)
                )
            }
            (Some(old), None) => {
                format!(
                    "{} → {missing_label} (-{})",
                    format_byte_size(old),
                    format_byte_size(old)
                )
            }
            (None, None) => format!("{missing_label} → {missing_label}"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffLineKind {
    Context,
    Added,
    Deleted,
    /// Trailing `\ No newline at end of file` indicator line (has no source line numbers).
    NoNewline,
}

/// Text payload stored in `DiffLine::text` for [`DiffLineKind::NoNewline`] lines.
/// Rendered immediately after the `\` 1-column marker so the visual output is
/// `\ No newline at end of file`.
pub const NO_NEWLINE_LINE_TEXT: &str = " No newline at end of file";

/// Canonical Git tree/index object modes.
pub const GIT_MODE_REGULAR: u32 = 0o100_644;
pub const GIT_MODE_EXECUTABLE: u32 = 0o100_755;
pub const GIT_MODE_SYMLINK: u32 = 0o120_000;
pub const GIT_MODE_GITLINK: u32 = 0o160_000;

/// File mode / object type metadata attached to a [`FileDiff`] or `ThreeDocStageDiff`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileModeChange {
    pub old_mode: Option<u32>,
    pub new_mode: Option<u32>,
    /// True when either side is a symbolic link (`0o120000`).
    pub is_symlink: bool,
}

impl FileModeChange {
    /// Builds mode/typechange metadata when the mode changed between sides or either
    /// side is a symbolic link (`0o120000`). Returns `None` for ordinary regular files
    /// whose mode did not change.
    pub fn from_modes(old_mode: Option<u32>, new_mode: Option<u32>) -> Option<Self> {
        let is_symlink = old_mode == Some(GIT_MODE_SYMLINK) || new_mode == Some(GIT_MODE_SYMLINK);
        let mode_changed = matches!((old_mode, new_mode), (Some(a), Some(b)) if a != b);
        if mode_changed || is_symlink {
            Some(Self {
                old_mode,
                new_mode,
                is_symlink,
            })
        } else {
            None
        }
    }

    pub const fn is_mode_changed(&self) -> bool {
        matches!((self.old_mode, self.new_mode), (Some(a), Some(b)) if a != b)
    }

    /// True when the Git object type bits (`0o170000`) changed (e.g. regular file ↔ symlink).
    pub const fn is_typechange(&self) -> bool {
        match (self.old_mode, self.new_mode) {
            (Some(a), Some(b)) => (a & 0o170_000) != (b & 0o170_000),
            _ => false,
        }
    }

    /// Formats a Git mode as 6-digit octal (e.g. `100644`, `100755`, `120000`).
    pub fn format_mode(mode: u32) -> String {
        format!("{mode:06o}")
    }
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
    pub whitespace: WhitespaceMode,
}

/// Hard ceiling (4 MiB) on single-file diff preview / three-doc tracker loading.
pub const MAX_DIFF_FILE_BYTES: usize = 4 * 1024 * 1024;
/// Maximum line pairs per diff for eager inline word-diff calculation.
pub const EAGER_WORD_DIFF_PAIR_BUDGET: usize = 256;
/// Maximum single-line byte length eligible for inline word-diff tokenization.
pub const MAX_WORD_DIFF_LINE_BYTES: usize = 2048;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LfsPointer {
    pub oid_sha256: String,
    pub size: u64,
}

impl LfsPointer {
    /// First 12 hexadecimal characters of the SHA-256 object id.
    pub fn short_oid(&self) -> &str {
        let end = self.oid_sha256.len().min(12);
        &self.oid_sha256[..end]
    }

    pub fn format_size(&self) -> String {
        const KIB: f64 = 1024.0;
        const MIB: f64 = 1024.0 * 1024.0;
        const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
        let b = self.size as f64;
        if self.size < 1024 {
            format!("{} B", self.size)
        } else if self.size < 1024 * 1024 {
            format!("{:.1} KiB ({} B)", b / KIB, self.size)
        } else if self.size < 1024 * 1024 * 1024 {
            format!("{:.1} MiB ({} B)", b / MIB, self.size)
        } else {
            format!("{:.2} GiB ({} B)", b / GIB, self.size)
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LfsDiff {
    pub old: Option<LfsPointer>,
    pub new: Option<LfsPointer>,
    pub filter_detected: bool,
}

/// Parses a Git LFS v1 pointer file (`version https://git-lfs.github.com/spec/v1`).
pub fn parse_lfs_pointer(bytes: &[u8]) -> Option<LfsPointer> {
    if bytes.is_empty() || bytes.len() > 1024 {
        return None;
    }
    let text = std::str::from_utf8(bytes).ok()?;
    let mut has_version = false;
    let mut oid_sha256 = None;
    let mut size = None;
    for raw in text.lines() {
        let line = raw.trim_end_matches('\r');
        if line.is_empty() {
            continue;
        }
        if line == "version https://git-lfs.github.com/spec/v1" {
            has_version = true;
        } else if let Some(hash) = line.strip_prefix("oid sha256:") {
            let hash = hash.trim();
            if hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
                return None;
            }
            oid_sha256 = Some(hash.to_ascii_lowercase());
        } else if let Some(sz) = line.strip_prefix("size ") {
            size = Some(sz.trim().parse::<u64>().ok()?);
        } else if line.starts_with("ext-") {
            continue;
        } else {
            return None;
        }
    }
    if !has_version {
        return None;
    }
    Some(LfsPointer {
        oid_sha256: oid_sha256?,
        size: size?,
    })
}

/// Returns `true` when `.gitattributes` contains any active `filter=lfs` rule.
pub fn has_lfs_filter_in_gitattributes(content: &str) -> bool {
    content.lines().any(|line| {
        let trimmed = line.trim();
        !trimmed.is_empty()
            && !trimmed.starts_with('#')
            && trimmed.split_whitespace().any(|tok| tok == "filter=lfs")
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDiff {
    pub key: FileDiffKey,
    pub hunks: Vec<DiffHunk>,
    pub is_binary: bool,
    /// Binary file byte sizes and blob OIDs when `is_binary == true`.
    pub binary: Option<BinaryDiffInfo>,
    /// Diff exceeded the line budget and was truncated.
    pub is_truncated: bool,
    /// Either side exceeded [`MAX_DIFF_FILE_BYTES`] and full diffing was skipped.
    pub is_too_large: bool,
    /// Git LFS pointer metadata when either side is an LFS pointer.
    pub lfs: Option<LfsDiff>,
    /// File mode or object-type change / symlink metadata (e.g. `100644 → 100755`, `120000`).
    pub mode_change: Option<FileModeChange>,
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

const SIMILARITY_PAIR_CELL_LIMIT: usize = 2048;
const SIMILARITY_MIN_SCORE: u32 = 300;
const SIMILARITY_MAX_CHARS: usize = 512;

fn sorted_char_bigrams(trimmed: &str) -> Vec<(char, char)> {
    let mut iter = trimmed.chars().take(SIMILARITY_MAX_CHARS);
    let Some(mut prev) = iter.next() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for ch in iter {
        out.push((prev, ch));
        prev = ch;
    }
    out.sort_unstable();
    out
}

/// Dice-coefficient bigram similarity in `0..=1000` on trimmed lines.
fn line_similarity_score(
    old_trimmed: &str,
    old_bigrams: &[(char, char)],
    new_trimmed: &str,
    new_bigrams: &[(char, char)],
) -> u32 {
    if old_trimmed.is_empty() || new_trimmed.is_empty() {
        return 0;
    }
    if old_trimmed == new_trimmed {
        return 1000;
    }
    if old_bigrams.is_empty() || new_bigrams.is_empty() {
        return 0;
    }
    let mut i = 0usize;
    let mut j = 0usize;
    let mut common = 0usize;
    while i < old_bigrams.len() && j < new_bigrams.len() {
        match old_bigrams[i].cmp(&new_bigrams[j]) {
            std::cmp::Ordering::Equal => {
                common += 1;
                i += 1;
                j += 1;
            }
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
        }
    }
    let denom = old_bigrams.len() + new_bigrams.len();
    ((common * 2000) / denom) as u32
}

/// Pairs a contiguous deletion run `del_start..del_end` with the following
/// addition run `add_start..add_end` using order-preserving greedy similarity matching.
fn pair_change_block(
    lines: &[DiffLine],
    del_start: usize,
    del_end: usize,
    add_start: usize,
    add_end: usize,
    out: &mut Vec<SideBySideRow>,
) {
    let dels = del_end - del_start;
    let adds = add_end - add_start;
    if dels == 0 || adds == 0 || (dels == 1 && adds == 1) || dels.saturating_mul(adds) > SIMILARITY_PAIR_CELL_LIMIT {
        for k in 0..dels.max(adds) {
            out.push(SideBySideRow {
                left: (k < dels).then_some(del_start + k),
                right: (k < adds).then_some(add_start + k),
            });
        }
        return;
    }

    let del_trimmed: Vec<&str> = (del_start..del_end)
        .map(|idx| lines[idx].text.trim())
        .collect();
    let add_trimmed: Vec<&str> = (add_start..add_end)
        .map(|idx| lines[idx].text.trim())
        .collect();
    let del_bigrams: Vec<Vec<(char, char)>> = del_trimmed
        .iter()
        .map(|s| sorted_char_bigrams(s))
        .collect();
    let add_bigrams: Vec<Vec<(char, char)>> = add_trimmed
        .iter()
        .map(|s| sorted_char_bigrams(s))
        .collect();

    let mut candidates: Vec<(u32, usize, usize)> = Vec::new();
    for di in 0..dels {
        for ai in 0..adds {
            let score = line_similarity_score(
                del_trimmed[di],
                &del_bigrams[di],
                add_trimmed[ai],
                &add_bigrams[ai],
            );
            if score >= SIMILARITY_MIN_SCORE {
                candidates.push((score, di, ai));
            }
        }
    }
    candidates.sort_unstable_by(|a, b| {
        b.0.cmp(&a.0)
            .then_with(|| a.1.abs_diff(a.2).cmp(&b.1.abs_diff(b.2)))
            .then_with(|| a.1.cmp(&b.1))
            .then_with(|| a.2.cmp(&b.2))
    });

    let mut used_d = vec![false; dels];
    let mut used_a = vec![false; adds];
    let mut anchors: Vec<(usize, usize)> = Vec::new();
    for (_, di, ai) in candidates {
        if used_d[di] || used_a[ai] {
            continue;
        }
        if anchors.iter().all(|&(pd, pa)| (di < pd) == (ai < pa)) {
            used_d[di] = true;
            used_a[ai] = true;
            anchors.push((di, ai));
        }
    }
    anchors.sort_unstable_by_key(|&(di, _)| di);

    let emit_gap = |out: &mut Vec<SideBySideRow>, d_lo: usize, d_hi: usize, a_lo: usize, a_hi: usize| {
        let dg = d_hi - d_lo;
        let ag = a_hi - a_lo;
        for k in 0..dg.max(ag) {
            out.push(SideBySideRow {
                left: (k < dg).then_some(del_start + d_lo + k),
                right: (k < ag).then_some(add_start + a_lo + k),
            });
        }
    };

    let mut prev_d = 0usize;
    let mut prev_a = 0usize;
    for (di, ai) in anchors {
        emit_gap(out, prev_d, di, prev_a, ai);
        out.push(SideBySideRow {
            left: Some(del_start + di),
            right: Some(add_start + ai),
        });
        prev_d = di + 1;
        prev_a = ai + 1;
    }
    emit_gap(out, prev_d, dels, prev_a, adds);
}

/// Aligns deletions with following additions using order-preserving greedy
/// similarity pairing; context lines appear on both sides.
pub fn side_by_side_rows(hunk: &DiffHunk) -> Vec<SideBySideRow> {
    let mut rows = Vec::with_capacity(hunk.lines.len());
    let lines = &hunk.lines;
    let mut i = 0;
    while i < lines.len() {
        match lines[i].kind {
            DiffLineKind::Context | DiffLineKind::NoNewline => {
                rows.push(SideBySideRow {
                    left: Some(i),
                    right: Some(i),
                });
                i += 1;
            }
            DiffLineKind::Deleted | DiffLineKind::Added => {
                let del_start = i;
                while i < lines.len() && lines[i].kind == DiffLineKind::Deleted {
                    i += 1;
                }
                let del_end = i;
                let left_no_nl = if del_end > del_start
                    && i < lines.len()
                    && lines[i].kind == DiffLineKind::NoNewline
                {
                    let idx = i;
                    i += 1;
                    Some(idx)
                } else {
                    None
                };
                let add_start = i;
                while i < lines.len() && lines[i].kind == DiffLineKind::Added {
                    i += 1;
                }
                let add_end = i;
                let right_no_nl = if add_end > add_start
                    && i < lines.len()
                    && lines[i].kind == DiffLineKind::NoNewline
                {
                    let idx = i;
                    i += 1;
                    Some(idx)
                } else {
                    None
                };
                pair_change_block(lines, del_start, del_end, add_start, add_end, &mut rows);
                if left_no_nl.is_some() || right_no_nl.is_some() {
                    rows.push(SideBySideRow {
                        left: left_no_nl,
                        right: right_no_nl,
                    });
                }
            }
        }
    }
    rows
}

/// Finds the paired `(deleted_idx, added_idx)` for `line_idx` within a hunk's
/// lines, using the exact same similarity pairing as [`side_by_side_rows`].
pub fn paired_line_in_hunk(lines: &[DiffLine], line_idx: usize) -> Option<(usize, usize)> {
    let line = lines.get(line_idx)?;
    let (del_start, del_end, add_start, add_end) = match line.kind {
        DiffLineKind::Deleted => {
            let mut ds = line_idx;
            while ds > 0 && lines[ds - 1].kind == DiffLineKind::Deleted {
                ds -= 1;
            }
            let mut de = line_idx + 1;
            while de < lines.len() && lines[de].kind == DiffLineKind::Deleted {
                de += 1;
            }
            let mut as_ = de;
            if as_ < lines.len() && lines[as_].kind == DiffLineKind::NoNewline {
                as_ += 1;
            }
            let mut ae = as_;
            while ae < lines.len() && lines[ae].kind == DiffLineKind::Added {
                ae += 1;
            }
            (ds, de, as_, ae)
        }
        DiffLineKind::Added => {
            let mut as_ = line_idx;
            while as_ > 0 && lines[as_ - 1].kind == DiffLineKind::Added {
                as_ -= 1;
            }
            let mut de = as_;
            if de > 0 && lines[de - 1].kind == DiffLineKind::NoNewline {
                de -= 1;
            }
            let mut ds = de;
            while ds > 0 && lines[ds - 1].kind == DiffLineKind::Deleted {
                ds -= 1;
            }
            let mut ae = line_idx + 1;
            while ae < lines.len() && lines[ae].kind == DiffLineKind::Added {
                ae += 1;
            }
            (ds, de, as_, ae)
        }
        DiffLineKind::Context | DiffLineKind::NoNewline => return None,
    };
    if del_start == del_end || add_start == add_end {
        return None;
    }
    let mut rows = Vec::with_capacity((del_end - del_start).max(add_end - add_start));
    pair_change_block(lines, del_start, del_end, add_start, add_end, &mut rows);
    rows.into_iter().find_map(|r| match (r.left, r.right) {
        (Some(l), Some(r)) if l == line_idx || r == line_idx => Some((l, r)),
        _ => None,
    })
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
            (Some(l), Some(r))
                if l != r
                    && hunk.lines[l].kind == DiffLineKind::Deleted
                    && hunk.lines[r].kind == DiffLineKind::Added =>
            {
                Some((l, r))
            }
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
            DiffLineKind::NoNewline => (None, None),
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

    #[test]
    fn signature_status_maps_all_git_codes_and_batch_lines() {
        let cases = [
            ("G", Some(SignatureStatus::Good), true, "✓"),
            ("U", Some(SignatureStatus::GoodUnknownValidity), true, "⚠"),
            ("X", Some(SignatureStatus::Expired { key_expired: false }), true, "⚠"),
            ("Y", Some(SignatureStatus::Expired { key_expired: true }), true, "⚠"),
            ("R", Some(SignatureStatus::Revoked), true, "✕"),
            ("E", Some(SignatureStatus::CannotCheck), true, "?"),
            ("B", Some(SignatureStatus::Bad), true, "✕"),
            ("N", Some(SignatureStatus::Unsigned), false, " "),
            ("?", None, false, " "),
        ];
        for (code, expected, signed, icon) in cases {
            assert_eq!(SignatureStatus::from_git_code(code), expected, "code={code}");
            if let Some(st) = expected {
                assert_eq!(st.is_signed(), signed);
                assert_eq!(st.icon(), icon);
            }
        }
        assert!(SignatureStatus::Pending.is_signed());
        assert_eq!(SignatureStatus::Pending.icon(), "…");

        let single = CommitSignature::parse_single_format_line("G\x1fAlice <a@x.io>\x1fKEY123\r\n");
        assert_eq!(
            single,
            CommitSignature {
                status: SignatureStatus::Good,
                signer: Some("Alice <a@x.io>".into()),
                key: Some("KEY123".into()),
            }
        );

        let no_signer = CommitSignature::parse_single_format_line("E\x1f\x1fDEADBEEF");
        assert_eq!(
            no_signer,
            CommitSignature {
                status: SignatureStatus::CannotCheck,
                signer: None,
                key: Some("DEADBEEF".into()),
            }
        );

        assert_eq!(
            CommitSignature::parse_single_format_line("N\x1f\x1f"),
            CommitSignature::unsigned()
        );
        assert_eq!(
            CommitSignature::parse_single_format_line(""),
            CommitSignature::cannot_check()
        );
        assert_eq!(
            CommitSignature::parse_format_fields("Z", "Alice", "K1"),
            CommitSignature {
                status: SignatureStatus::CannotCheck,
                signer: Some("Alice".into()),
                key: Some("K1".into()),
            }
        );

        let o1 = Oid::from_bytes([0x11; 20]);
        let o2 = Oid::from_bytes([0x22; 20]);
        let o3 = Oid::from_bytes([0x33; 20]);
        let batch = format!(
            "{o1}\x1fG\x1fAlice <a@x.io>\x1fAABBCC\r\n\
             {o2}\x1fN\x1f\x1f\n\
             bad_oid\x1fG\x1fX\x1fY\n\
             {o3}\x1fY\x1fBob\x1f112233\n"
        );
        let parsed = CommitSignature::parse_batch_format_output(&batch);
        assert_eq!(
            parsed,
            vec![
                (
                    o1,
                    CommitSignature {
                        status: SignatureStatus::Good,
                        signer: Some("Alice <a@x.io>".into()),
                        key: Some("AABBCC".into()),
                    }
                ),
                (o2, CommitSignature::unsigned()),
                (
                    o3,
                    CommitSignature {
                        status: SignatureStatus::Expired { key_expired: true },
                        signer: Some("Bob".into()),
                        key: Some("112233".into()),
                    }
                ),
            ]
        );
    }

    #[test]
    fn lfs_pointer_and_gitattributes_parsing() {
        let hash = "4d7a214614ab2935c943f9e0ff69d22eadbb8f32b1258daaa5e2ca24d17e2393";
        let ptr_text =
            format!("version https://git-lfs.github.com/spec/v1\noid sha256:{hash}\nsize 12345\n");
        let ptr = parse_lfs_pointer(ptr_text.as_bytes()).expect("valid LFS pointer");
        assert_eq!(ptr.oid_sha256, hash);
        assert_eq!(ptr.short_oid(), "4d7a214614ab");
        assert_eq!(ptr.size, 12345);
        assert!(ptr.format_size().contains("12.1 KiB"));

        assert_eq!(parse_lfs_pointer(b"just normal text\n"), None);
        assert_eq!(
            parse_lfs_pointer(
                b"version https://git-lfs.github.com/spec/v1\noid sha256:short\nsize 10\n"
            ),
            None
        );
        assert_eq!(parse_lfs_pointer(&vec![b'a'; 2048]), None);

        assert!(!has_lfs_filter_in_gitattributes(
            "# *.bin filter=lfs diff=lfs merge=lfs -text\n*.rs text\n"
        ));
        assert!(has_lfs_filter_in_gitattributes(
            "*.bin filter=lfs diff=lfs merge=lfs -text\n"
        ));
    }

    #[test]
    fn file_mode_change_and_no_newline_side_by_side_alignment() {
        assert_eq!(
            FileModeChange::from_modes(Some(GIT_MODE_REGULAR), Some(GIT_MODE_REGULAR)),
            None
        );
        let chmod = FileModeChange::from_modes(Some(GIT_MODE_REGULAR), Some(GIT_MODE_EXECUTABLE))
            .expect("mode change");
        assert!(chmod.is_mode_changed());
        assert!(!chmod.is_typechange());
        assert!(!chmod.is_symlink);
        assert_eq!(FileModeChange::format_mode(GIT_MODE_EXECUTABLE), "100755");

        let typechange = FileModeChange::from_modes(Some(GIT_MODE_REGULAR), Some(GIT_MODE_SYMLINK))
            .expect("typechange");
        assert!(typechange.is_mode_changed());
        assert!(typechange.is_typechange());
        assert!(typechange.is_symlink);

        let sym_only = FileModeChange::from_modes(Some(GIT_MODE_SYMLINK), Some(GIT_MODE_SYMLINK))
            .expect("symlink metadata");
        assert!(!sym_only.is_mode_changed());
        assert!(!sym_only.is_typechange());
        assert!(sym_only.is_symlink);

        let (hunks, _) =
            crate::build_diff_hunks(b"alpha\n", b"alpha", 3, crate::MAX_DIFF_LINES, || false)
                .unwrap();
        assert_eq!(hunks.len(), 1);
        assert_eq!(hunks[0].lines.len(), 3);
        assert_eq!(hunks[0].lines[0].kind, DiffLineKind::Deleted);
        assert_eq!(hunks[0].lines[1].kind, DiffLineKind::Added);
        assert_eq!(hunks[0].lines[2].kind, DiffLineKind::NoNewline);
        assert_eq!(hunks[0].lines[2].text, NO_NEWLINE_LINE_TEXT);

        let rows = side_by_side_rows(&hunks[0]);
        assert_eq!(
            rows,
            vec![
                SideBySideRow {
                    left: Some(0),
                    right: Some(1)
                },
                SideBySideRow {
                    left: None,
                    right: Some(2)
                },
            ]
        );
    }

    #[test]
    fn whitespace_mode_normalization_and_cycle() {
        assert_eq!(WhitespaceMode::Default.next(), WhitespaceMode::IgnoreChange);
        assert_eq!(WhitespaceMode::IgnoreChange.next(), WhitespaceMode::IgnoreAll);
        assert_eq!(WhitespaceMode::IgnoreAll.next(), WhitespaceMode::Default);

        assert_eq!(WhitespaceMode::parse("default"), Some(WhitespaceMode::Default));
        assert_eq!(WhitespaceMode::parse("-b"), Some(WhitespaceMode::IgnoreChange));
        assert_eq!(WhitespaceMode::parse("ignore_change"), Some(WhitespaceMode::IgnoreChange));
        assert_eq!(WhitespaceMode::parse("-w"), Some(WhitespaceMode::IgnoreAll));
        assert_eq!(WhitespaceMode::parse("ignore_all"), Some(WhitespaceMode::IgnoreAll));

        // IgnoreChange (-b): collapses horizontal runs & strips trailing whitespace,
        // while keeping word separation distinct from no-space concatenation.
        assert_eq!(
            WhitespaceMode::IgnoreChange.normalize_line(b"  let   x  =  1;   \r\n"),
            WhitespaceMode::IgnoreChange.normalize_line(b"\tlet x = 1;\n")
        );
        assert_ne!(
            WhitespaceMode::IgnoreChange.normalize_line(b"let x=1;\n"),
            WhitespaceMode::IgnoreChange.normalize_line(b"let x = 1;\n")
        );

        // IgnoreAll (-w): strips all horizontal whitespace.
        assert_eq!(
            WhitespaceMode::IgnoreAll.normalize_line(b"    let x = 1;  \r\n"),
            WhitespaceMode::IgnoreAll.normalize_line(b"letx=1;\n")
        );
    }

    #[test]
    fn greedy_similarity_pairing_aligns_modified_line_past_inserted_lines() {
        let mk = |kind, text: &str| DiffLine {
            kind,
            old_lineno: (kind != DiffLineKind::Added).then_some(1),
            new_lineno: (kind != DiffLineKind::Deleted).then_some(1),
            text: text.into(),
            word_ranges: Vec::new(),
        };
        // Deleted block has 1 line; Added block inserts a comment line BEFORE the modified line.
        let mut hunk = DiffHunk {
            header: "@@ -1,1 +1,2 @@".into(),
            lines: vec![
                mk(DiffLineKind::Deleted, "let result = compute_total(old_value, count);"),
                mk(DiffLineKind::Added, "// newly inserted explanatory comment"),
                mk(DiffLineKind::Added, "let result = compute_total(new_value, count);"),
            ],
        };

        let sbs = side_by_side_rows(&hunk);
        assert_eq!(
            sbs,
            vec![
                SideBySideRow {
                    left: None,
                    right: Some(1)
                },
                SideBySideRow {
                    left: Some(0),
                    right: Some(2)
                },
            ]
        );
        assert_eq!(paired_line_in_hunk(&hunk.lines, 0), Some((0, 2)));
        assert_eq!(paired_line_in_hunk(&hunk.lines, 1), None);
        assert_eq!(paired_line_in_hunk(&hunk.lines, 2), Some((0, 2)));

        apply_word_diff(&mut hunk);
        assert!(hunk.lines[1].word_ranges.is_empty());
        assert_eq!(
            &hunk.lines[0].text[hunk.lines[0].word_ranges[0].0..hunk.lines[0].word_ranges[0].1],
            "old_value"
        );
        assert_eq!(
            &hunk.lines[2].text[hunk.lines[2].word_ranges[0].0..hunk.lines[2].word_ranges[0].1],
            "new_value"
        );
    }

    #[test]
    fn binary_diff_info_formats_size_delta_and_sides() {
        let modified = BinaryDiffInfo {
            old_size: Some(12_698),
            new_size: Some(15_155),
            old_oid: Some(Oid::from_bytes([0x11; 20])),
            new_oid: Some(Oid::from_bytes([0x22; 20])),
        };
        assert_eq!(modified.size_summary("(none)"), "12.4 KiB → 14.8 KiB (+2.4 KiB)");

        let added = BinaryDiffInfo {
            old_size: None,
            new_size: Some(2048),
            old_oid: None,
            new_oid: Some(Oid::from_bytes([0x33; 20])),
        };
        assert_eq!(added.size_summary("(none)"), "(none) → 2.0 KiB (+2.0 KiB)");

        let deleted = BinaryDiffInfo {
            old_size: Some(512),
            new_size: None,
            old_oid: Some(Oid::from_bytes([0x44; 20])),
            new_oid: None,
        };
        assert_eq!(deleted.size_summary("(none)"), "512 B → (none) (-512 B)");
    }
}
