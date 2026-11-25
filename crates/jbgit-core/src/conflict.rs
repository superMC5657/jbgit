//! Pure domain model for built-in three-pane conflict resolution:
//! - True 3-way merge (`:1` Base / `:2` Ours / `:3` Theirs) via `imara-diff`,
//!   classifying `Unchanged`, `LeftOnly`, `RightOnly`, `BothSame`, and `Conflict`
//!   hunks and supporting one-shot `apply_non_conflicting()`;
//! - Marker fallback parser (`<<<<<<<` / `|||||||` / `=======` / `>>>>>>>`);
//! - Per-hunk choices (`Ours` / `Theirs` / `Both*` / `Neither` / `Custom`),
//!   single-block reset, Rebase Ours/Theirs semantic reversal, Delete/Modify
//!   resolution, lossless line-ending preservation (CRLF / LF), and non-text
//!   (binary / submodule / directory) conflict descriptors.

use crate::{LineEnding, Oid, RepositoryState, compute_diff_blocks};

/// Returns `true` when Git's index stages (`:2` / `--ours` vs `:3` / `--theirs`)
/// have reversed semantics relative to the user's mental model of "Yours" vs
/// "Theirs".
///
/// During a rebase (`Rebase` / `RebaseInteractive` / `RebaseMerge`), Git checks
/// out the upstream/onto target as `HEAD` (stage 2 / `--ours`) and replays the
/// user's local commit on top of it (stage 3 / `--theirs`), so "Yours" is stage
/// 3 and "Theirs" is stage 2.
pub const fn is_reverse_conflict_state(state: &RepositoryState) -> bool {
    matches!(
        state,
        RepositoryState::Rebase { .. }
            | RepositoryState::RebaseInteractive { .. }
            | RepositoryState::RebaseMerge { .. }
    )
}

/// Classification of a 3-way merged hunk relative to the common ancestor (`:1`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ThreeWayBlockKind {
    /// Identical across base, ours, and theirs.
    Unchanged,
    /// Only ours (left) changed relative to base; theirs is unchanged.
    LeftOnly,
    /// Only theirs (right) changed relative to base; ours is unchanged.
    RightOnly,
    /// Both sides made the exact same change relative to base.
    BothSame,
    /// Conflicting changes on both sides (or add/add differing hunk without base).
    #[default]
    Conflict,
}

impl ThreeWayBlockKind {
    pub const fn is_non_conflicting_change(self) -> bool {
        matches!(self, Self::LeftOnly | Self::RightOnly | Self::BothSame)
    }
}

/// One classified hunk produced by [`three_way_merge_blocks`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreeWayHunk {
    pub kind: ThreeWayBlockKind,
    pub base_lines: Vec<String>,
    pub ours_lines: Vec<String>,
    pub theirs_lines: Vec<String>,
}

/// Per-side file change status shown in the multi-file Conflict Queue table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictSideStatus {
    Unchanged,
    Modified,
    Added,
    Deleted,
}

/// Maps a raw libgit2 index conflict label (from `(ancestor, our, their)`) into
/// logical `(Yours, Theirs)` statuses, flipping sides when `is_reversed` is true.
pub fn conflict_side_statuses(
    label: Option<&str>,
    is_reversed: bool,
) -> (ConflictSideStatus, ConflictSideStatus) {
    use ConflictSideStatus::*;
    let (raw_our, raw_their) = match label.unwrap_or("") {
        "Both Modified" => (Modified, Modified),
        "Added by Both" => (Added, Added),
        "Deleted by Them" => (Modified, Deleted),
        "Deleted by Us" => (Deleted, Modified),
        "Both Deleted" => (Deleted, Deleted),
        "Added by Us" => (Added, Unchanged),
        "Added by Them" => (Unchanged, Added),
        _ => (Modified, Modified),
    };
    if is_reversed {
        (raw_their, raw_our)
    } else {
        (raw_our, raw_their)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConflictChoice {
    Unresolved,
    Ours,
    Theirs,
    BothOursFirst,
    BothTheirsFirst,
    /// Discard changes from both sides (emits 0 lines).
    Neither,
    /// User-edited custom lines for the Result pane.
    Custom(Vec<String>),
}

impl ConflictChoice {
    pub const fn label(&self) -> &'static str {
        match self {
            Self::Unresolved => "Unresolved",
            Self::Ours => "Ours",
            Self::Theirs => "Theirs",
            Self::BothOursFirst => "Both (Ours + Theirs)",
            Self::BothTheirsFirst => "Both (Theirs + Ours)",
            Self::Neither => "Neither (Discard Both)",
            Self::Custom(_) => "Custom (Edited)",
        }
    }

    pub const fn is_resolved(&self) -> bool {
        !matches!(self, Self::Unresolved)
    }
}

/// Presence of each logical side (after applying [`is_reverse_conflict_state`])
/// in the Git index conflict entry.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ConflictSidePresence {
    /// Both logical sides exist (modify/modify or add/add).
    #[default]
    BothPresent,
    /// Yours kept/modified the file; Theirs deleted it.
    DeletedByTheirs,
    /// Yours deleted the file; Theirs kept/modified it.
    DeletedByOurs,
    /// Both sides deleted the file (`DD`).
    BothDeleted,
}

impl ConflictSidePresence {
    pub const fn is_delete_conflict(self) -> bool {
        !matches!(self, Self::BothPresent)
    }

    pub const fn ours_deleted(self) -> bool {
        matches!(self, Self::DeletedByOurs | Self::BothDeleted)
    }

    pub const fn theirs_deleted(self) -> bool {
        matches!(self, Self::DeletedByTheirs | Self::BothDeleted)
    }
}

/// Index metadata for one logical side of a non-text conflict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConflictSideInfo {
    pub oid: Oid,
    pub size: Option<u64>,
    pub mode: u32,
}

/// Category of conflict that must not be opened in the plain-text three-pane
/// merger.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpecialConflictKind {
    Binary,
    Submodule,
    Directory,
}

/// Descriptor for a binary, submodule gitlink (`0o160000`), or directory
/// conflict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpecialConflictInfo {
    pub path: String,
    pub kind: SpecialConflictKind,
    pub is_reversed: bool,
    pub ours: Option<ConflictSideInfo>,
    pub theirs: Option<ConflictSideInfo>,
    pub ours_label: String,
    pub theirs_label: String,
}

/// Outcome of loading a conflicted path for interactive resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadedConflict {
    Text(ConflictDocument),
    Special(SpecialConflictInfo),
}

/// Extra context supplied by the engine when parsing a [`ConflictDocument`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ConflictParseOptions<'a> {
    pub is_reversed: bool,
    pub side_presence: ConflictSidePresence,
    pub default_ours_label: Option<&'a str>,
    pub default_theirs_label: Option<&'a str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConflictSegment {
    Common(Vec<String>),
    Conflict(ConflictBlock),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConflictBlock {
    pub id: usize,
    pub kind: ThreeWayBlockKind,
    pub ours_label: String,
    pub theirs_label: String,
    pub ours_lines: Vec<String>,
    pub base_lines: Vec<String>,
    pub theirs_lines: Vec<String>,
    pub choice: ConflictChoice,
}

impl ConflictBlock {
    pub fn resolved_lines(&self) -> Option<Vec<String>> {
        match &self.choice {
            ConflictChoice::Unresolved => None,
            ConflictChoice::Ours => Some(self.ours_lines.clone()),
            ConflictChoice::Theirs => Some(self.theirs_lines.clone()),
            ConflictChoice::BothOursFirst => {
                let mut out = self.ours_lines.clone();
                out.extend(self.theirs_lines.iter().cloned());
                Some(out)
            }
            ConflictChoice::BothTheirsFirst => {
                let mut out = self.theirs_lines.clone();
                out.extend(self.ours_lines.iter().cloned());
                Some(out)
            }
            ConflictChoice::Neither => Some(Vec::new()),
            ConflictChoice::Custom(lines) => Some(lines.clone()),
        }
    }

    fn pane_row_count(&self, side_presence: ConflictSidePresence) -> usize {
        let ours_len = if self.ours_lines.is_empty() && side_presence.ours_deleted() {
            1
        } else {
            self.ours_lines.len()
        };
        let theirs_len = if self.theirs_lines.is_empty() && side_presence.theirs_deleted() {
            1
        } else {
            self.theirs_lines.len()
        };
        let merged_len = match self.resolved_lines() {
            Some(lines) if !lines.is_empty() => lines.len(),
            _ => 1,
        };
        ours_len.max(merged_len).max(theirs_len).max(1)
    }
}

/// Semantic placeholder kind for synthetic status/prompt rows in the merged result pane
/// so frontends can render localized text without parsing English strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictRowPlaceholder {
    FileWillBeDeleted,
    DiscardedNeither,
    Empty,
    UnresolvedLeftOnly { block_num: usize },
    UnresolvedRightOnly { block_num: usize },
    UnresolvedBothSame { block_num: usize },
    UnresolvedConflict { block_num: usize },
    UnresolvedDeletedByTheirs { block_num: usize },
    UnresolvedDeletedByOurs { block_num: usize },
    UnresolvedBothDeleted { block_num: usize },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConflictPaneRow {
    pub block_idx: Option<usize>,
    pub block_kind: Option<ThreeWayBlockKind>,
    pub ours: String,
    pub merged: String,
    pub theirs: String,
    pub choice: Option<ConflictChoice>,
    pub merged_placeholder: Option<ConflictRowPlaceholder>,
    pub ours_deleted_placeholder: bool,
    pub theirs_deleted_placeholder: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConflictDocument {
    pub path: String,
    pub segments: Vec<ConflictSegment>,
    pub line_ending: LineEnding,
    pub has_trailing_newline: bool,
    pub conflict_count: usize,
    pub is_reversed: bool,
    pub side_presence: ConflictSidePresence,
    /// `true` when stage `:1` (common ancestor Base) was available to build the
    /// 3-way model (or diff3 `|||||||` base markers were present).
    pub has_base: bool,
}

fn join_lines_for_diff(lines: &[String]) -> Vec<u8> {
    let cap: usize = lines.iter().map(|l| l.len() + 1).sum();
    let mut out = Vec::with_capacity(cap);
    for l in lines {
        out.extend_from_slice(l.as_bytes());
        out.push(b'\n');
    }
    out
}

fn split_normalized_lines(text: &str) -> Vec<String> {
    text.lines()
        .map(|l| l.trim_end_matches('\r').to_string())
        .collect()
}

/// Pure 3-way hunk merge using `imara-diff` (Histogram algorithm).
///
/// - When `base` is `Some(base_lines)`: diffs `base -> ours` and `base -> theirs`,
///   coalesces overlapping/adjacent base regions, and classifies each hunk as
///   `Unchanged`, `LeftOnly`, `RightOnly`, `BothSame`, or `Conflict`.
/// - When `base` is `None` (e.g. Add/Add conflict without common ancestor): diffs
///   `ours` directly against `theirs`, emitting `Unchanged` for shared regions
///   and `Conflict` for differing regions.
pub fn three_way_merge_blocks(
    base: Option<&[String]>,
    ours: &[String],
    theirs: &[String],
) -> Vec<ThreeWayHunk> {
    let Some(base_lines) = base else {
        return two_way_add_add_blocks(ours, theirs);
    };

    let base_bytes = join_lines_for_diff(base_lines);
    let ours_bytes = join_lines_for_diff(ours);
    let theirs_bytes = join_lines_for_diff(theirs);

    let left_diffs = compute_diff_blocks(&base_bytes, &ours_bytes);
    let right_diffs = compute_diff_blocks(&base_bytes, &theirs_bytes);

    let mut out = Vec::new();
    let mut base_pos = 0usize;
    let mut ours_pos = 0usize;
    let mut theirs_pos = 0usize;
    let mut li = 0usize;
    let mut ri = 0usize;

    while li < left_diffs.len() || ri < right_diffs.len() {
        let next_l = left_diffs.get(li).map_or(usize::MAX, |b| b.start1);
        let next_r = right_diffs.get(ri).map_or(usize::MAX, |b| b.start1);
        let next_start = next_l.min(next_r);

        if next_start > base_pos {
            let len = next_start - base_pos;
            let shared = base_lines[base_pos..next_start].to_vec();
            out.push(ThreeWayHunk {
                kind: ThreeWayBlockKind::Unchanged,
                base_lines: shared.clone(),
                ours_lines: shared.clone(),
                theirs_lines: shared,
            });
            base_pos = next_start;
            ours_pos += len;
            theirs_pos += len;
        }

        let mut l_end = li;
        let mut r_end = ri;
        let mut cluster_end = base_pos;

        if next_l == base_pos {
            cluster_end = cluster_end.max(left_diffs[l_end].end1);
            l_end += 1;
        }
        if next_r == base_pos {
            cluster_end = cluster_end.max(right_diffs[r_end].end1);
            r_end += 1;
        }

        loop {
            let mut grew = false;
            while l_end < left_diffs.len() && left_diffs[l_end].start1 <= cluster_end {
                cluster_end = cluster_end.max(left_diffs[l_end].end1);
                l_end += 1;
                grew = true;
            }
            while r_end < right_diffs.len() && right_diffs[r_end].start1 <= cluster_end {
                cluster_end = cluster_end.max(right_diffs[r_end].end1);
                r_end += 1;
                grew = true;
            }
            if !grew {
                break;
            }
        }

        let ours_end = if l_end == li {
            ours_pos + (cluster_end - base_pos)
        } else {
            let last_l = left_diffs[l_end - 1];
            last_l.end2 + (cluster_end - last_l.end1)
        };
        let theirs_end = if r_end == ri {
            theirs_pos + (cluster_end - base_pos)
        } else {
            let last_r = right_diffs[r_end - 1];
            last_r.end2 + (cluster_end - last_r.end1)
        };

        let base_slice = base_lines[base_pos..cluster_end].to_vec();
        let ours_slice = ours[ours_pos..ours_end].to_vec();
        let theirs_slice = theirs[theirs_pos..theirs_end].to_vec();

        let l_changed = l_end > li;
        let r_changed = r_end > ri;
        let kind = match (l_changed, r_changed) {
            (true, false) => ThreeWayBlockKind::LeftOnly,
            (false, true) => ThreeWayBlockKind::RightOnly,
            (true, true) => {
                if ours_slice == theirs_slice {
                    ThreeWayBlockKind::BothSame
                } else {
                    ThreeWayBlockKind::Conflict
                }
            }
            (false, false) => ThreeWayBlockKind::Unchanged,
        };

        out.push(ThreeWayHunk {
            kind,
            base_lines: base_slice,
            ours_lines: ours_slice,
            theirs_lines: theirs_slice,
        });

        base_pos = cluster_end;
        ours_pos = ours_end;
        theirs_pos = theirs_end;
        li = l_end;
        ri = r_end;
    }

    if base_pos < base_lines.len() {
        let shared = base_lines[base_pos..].to_vec();
        out.push(ThreeWayHunk {
            kind: ThreeWayBlockKind::Unchanged,
            base_lines: shared.clone(),
            ours_lines: shared.clone(),
            theirs_lines: shared,
        });
    }

    out
}

fn two_way_add_add_blocks(ours: &[String], theirs: &[String]) -> Vec<ThreeWayHunk> {
    let ours_bytes = join_lines_for_diff(ours);
    let theirs_bytes = join_lines_for_diff(theirs);
    let diffs = compute_diff_blocks(&ours_bytes, &theirs_bytes);

    let mut out = Vec::new();
    let mut o_pos = 0usize;
    let mut t_pos = 0usize;

    for d in diffs {
        if d.start1 > o_pos {
            let shared = ours[o_pos..d.start1].to_vec();
            out.push(ThreeWayHunk {
                kind: ThreeWayBlockKind::Unchanged,
                base_lines: Vec::new(),
                ours_lines: shared.clone(),
                theirs_lines: shared,
            });
        }
        out.push(ThreeWayHunk {
            kind: ThreeWayBlockKind::Conflict,
            base_lines: Vec::new(),
            ours_lines: ours[d.start1..d.end1].to_vec(),
            theirs_lines: theirs[d.start2..d.end2].to_vec(),
        });
        o_pos = d.end1;
        t_pos = d.end2;
    }
    if o_pos < ours.len() {
        let shared = ours[o_pos..].to_vec();
        out.push(ThreeWayHunk {
            kind: ThreeWayBlockKind::Unchanged,
            base_lines: Vec::new(),
            ours_lines: shared.clone(),
            theirs_lines: theirs[t_pos..].to_vec(),
        });
    }
    out
}

fn extract_marker_labels(
    worktree_text: &str,
    is_reversed: bool,
) -> (Option<String>, Option<String>) {
    let mut top = None;
    let mut bottom = None;
    for raw in worktree_text.lines() {
        let line = raw.trim_end_matches('\r');
        if top.is_none()
            && let Some(rest) = line.strip_prefix("<<<<<<<")
        {
            let lbl = rest.trim();
            if !lbl.is_empty() {
                top = Some(lbl.to_string());
            }
        } else if top.is_some()
            && let Some(rest) = line.strip_prefix(">>>>>>>")
        {
            let lbl = rest.trim();
            if !lbl.is_empty() {
                bottom = Some(lbl.to_string());
            }
            break;
        }
    }
    if is_reversed {
        (bottom, top)
    } else {
        (top, bottom)
    }
}

impl ConflictDocument {
    /// Parses conflict markers from `worktree_text`, or falls back to
    /// `index_ours` / `index_theirs` when no markers are present (e.g.
    /// add/add or delete/modify conflicts).
    pub fn parse(
        path: impl Into<String>,
        worktree_text: &str,
        index_ours: Option<&str>,
        index_theirs: Option<&str>,
    ) -> Self {
        Self::parse_with_options(
            path,
            worktree_text,
            index_ours,
            index_theirs,
            ConflictParseOptions::default(),
        )
    }

    /// Builds a true 3-way [`ConflictDocument`] from index stages `:1` (Base),
    /// logical Yours (`ours`), and logical Theirs (`theirs`) using
    /// [`three_way_merge_blocks`].
    ///
    /// For Delete/Modify and Both-Deleted conflicts (where one or both sides
    /// lack a blob), falls back to the synthetic delete/modify representation
    /// while preserving `base_lines` from `:1` for ancestor inspection.
    pub fn from_three_way(
        path: impl Into<String>,
        worktree_text: &str,
        base: Option<&str>,
        ours: Option<&str>,
        theirs: Option<&str>,
        options: ConflictParseOptions<'_>,
    ) -> Self {
        let path = path.into();
        if options.side_presence.is_delete_conflict() || ours.is_none() || theirs.is_none() {
            let mut doc = Self::parse_with_options(path, worktree_text, ours, theirs, options);
            if let Some(base_text) = base {
                let base_lines = split_normalized_lines(base_text);
                if let Some(ConflictSegment::Conflict(b)) = doc.segments.first_mut() {
                    b.base_lines = base_lines;
                }
                doc.has_base = true;
            }
            return doc;
        }

        let ours_text = ours.expect("checked is_some");
        let theirs_text = theirs.expect("checked is_some");
        let line_ending = detect_line_ending(worktree_text, Some(ours_text), Some(theirs_text));
        let has_trailing_newline = if !ours_text.is_empty() {
            ours_text.ends_with('\n')
        } else if !theirs_text.is_empty() {
            theirs_text.ends_with('\n')
        } else if !worktree_text.is_empty() {
            worktree_text.ends_with('\n')
        } else {
            false
        };

        let (marker_ours, marker_theirs) =
            extract_marker_labels(worktree_text, options.is_reversed);
        let default_ours = options
            .default_ours_label
            .unwrap_or(if options.is_reversed {
                "replayed"
            } else {
                "HEAD"
            });
        let default_theirs = options
            .default_theirs_label
            .unwrap_or(if options.is_reversed {
                "onto"
            } else {
                "Incoming"
            });
        let ours_label = marker_ours.unwrap_or_else(|| default_ours.to_string());
        let theirs_label = match marker_theirs {
            Some(ref t)
                if options.is_reversed && t == "HEAD" && options.default_theirs_label.is_some() =>
            {
                default_theirs.to_string()
            }
            Some(t) => t,
            None => default_theirs.to_string(),
        };

        let base_lines = base.map(split_normalized_lines);
        let ours_lines = split_normalized_lines(ours_text);
        let theirs_lines = split_normalized_lines(theirs_text);
        let hunks = three_way_merge_blocks(base_lines.as_deref(), &ours_lines, &theirs_lines);

        let mut segments = Vec::new();
        let mut conflict_id = 0usize;
        for hunk in hunks {
            match hunk.kind {
                ThreeWayBlockKind::Unchanged => {
                    if !hunk.ours_lines.is_empty() {
                        segments.push(ConflictSegment::Common(hunk.ours_lines));
                    }
                }
                kind => {
                    segments.push(ConflictSegment::Conflict(ConflictBlock {
                        id: conflict_id,
                        kind,
                        ours_label: ours_label.clone(),
                        theirs_label: theirs_label.clone(),
                        ours_lines: hunk.ours_lines,
                        base_lines: hunk.base_lines,
                        theirs_lines: hunk.theirs_lines,
                        choice: ConflictChoice::Unresolved,
                    }));
                    conflict_id += 1;
                }
            }
        }

        Self {
            path,
            segments,
            line_ending,
            has_trailing_newline,
            conflict_count: conflict_id,
            is_reversed: options.is_reversed,
            side_presence: options.side_presence,
            has_base: base.is_some(),
        }
    }

    /// Parses a conflict document with explicit reversal, Delete/Modify
    /// presence, and fallback side labels.
    ///
    /// `index_ours` and `index_theirs` MUST already be mapped to logical
    /// `(Yours, Theirs)` (i.e. stage 3 / stage 2 when `options.is_reversed` is
    /// `true`), while `worktree_text` is the raw working-tree file written by
    /// Git (whose `<<<<<<<` / `>>>>>>>` markers are swapped automatically when
    /// `options.is_reversed` is `true`).
    pub fn parse_with_options(
        path: impl Into<String>,
        worktree_text: &str,
        index_ours: Option<&str>,
        index_theirs: Option<&str>,
        options: ConflictParseOptions<'_>,
    ) -> Self {
        let path = path.into();
        let line_ending = detect_line_ending(worktree_text, index_ours, index_theirs);
        let has_trailing_newline = if !worktree_text.is_empty() {
            worktree_text.ends_with('\n')
        } else if let Some(s) = index_ours.filter(|s| !s.is_empty()) {
            s.ends_with('\n')
        } else if let Some(s) = index_theirs.filter(|s| !s.is_empty()) {
            s.ends_with('\n')
        } else {
            false
        };
        let raw_lines = split_normalized_lines(worktree_text);

        let default_ours = options
            .default_ours_label
            .unwrap_or(if options.is_reversed {
                "replayed"
            } else {
                "HEAD"
            });
        let default_theirs = options
            .default_theirs_label
            .unwrap_or(if options.is_reversed {
                "onto"
            } else {
                "Incoming"
            });

        let mut segments = Vec::new();
        let mut common = Vec::new();
        let mut i = 0usize;
        let mut conflict_id = 0usize;
        let mut has_base = false;

        if !options.side_presence.is_delete_conflict() {
            while i < raw_lines.len() {
                if let Some((marker_len, top_label_str)) = parse_open_marker(&raw_lines[i]) {
                    let top_label = top_label_str.to_string();
                    let start = i;
                    i += 1;
                    let mut top_lines = Vec::new();
                    let mut base_lines = Vec::new();
                    let mut bottom_lines = Vec::new();
                    let mut section = 0u8; // 0 = top, 1 = base, 2 = bottom
                    let mut closed = None;
                    let mut saw_base_marker = false;
                    while i < raw_lines.len() {
                        if is_base_marker(&raw_lines[i], marker_len) && section == 0 {
                            section = 1;
                            saw_base_marker = true;
                        } else if is_sep_marker(&raw_lines[i], marker_len) && section < 2 {
                            section = 2;
                        } else if let Some(trest) = parse_close_marker(&raw_lines[i], marker_len)
                            && section == 2
                        {
                            closed = Some(trest.to_string());
                            i += 1;
                            break;
                        } else {
                            match section {
                                0 => top_lines.push(raw_lines[i].clone()),
                                1 => base_lines.push(raw_lines[i].clone()),
                                _ => bottom_lines.push(raw_lines[i].clone()),
                            }
                        }
                        i += 1;
                    }
                    if let Some(bottom_label) = closed {
                        if saw_base_marker {
                            has_base = true;
                        }
                        if !common.is_empty() {
                            segments.push(ConflictSegment::Common(std::mem::take(&mut common)));
                        }
                        let (ours_label, theirs_label, ours_lines, theirs_lines) = if options
                            .is_reversed
                        {
                            let ours_lbl = if bottom_label.is_empty() {
                                default_ours.to_string()
                            } else {
                                bottom_label
                            };
                            let theirs_lbl = if top_label.is_empty()
                                || (top_label == "HEAD" && options.default_theirs_label.is_some())
                            {
                                default_theirs.to_string()
                            } else {
                                top_label
                            };
                            (ours_lbl, theirs_lbl, bottom_lines, top_lines)
                        } else {
                            let ours_lbl = if top_label.is_empty() {
                                default_ours.to_string()
                            } else {
                                top_label
                            };
                            let theirs_lbl = if bottom_label.is_empty() {
                                default_theirs.to_string()
                            } else {
                                bottom_label
                            };
                            (ours_lbl, theirs_lbl, top_lines, bottom_lines)
                        };
                        segments.push(ConflictSegment::Conflict(ConflictBlock {
                            id: conflict_id,
                            kind: ThreeWayBlockKind::Conflict,
                            ours_label,
                            theirs_label,
                            ours_lines,
                            base_lines,
                            theirs_lines,
                            choice: ConflictChoice::Unresolved,
                        }));
                        conflict_id += 1;
                        continue;
                    } else {
                        // Malformed / unclosed marker: treat as normal text.
                        common.extend(raw_lines[start..i].iter().cloned());
                        continue;
                    }
                }
                common.push(raw_lines[i].clone());
                i += 1;
            }
            if !common.is_empty() {
                segments.push(ConflictSegment::Common(common));
            }
        }

        // If no inline markers were found but index stages differ (or both
        // sides deleted the file), synthesize one conflict block from logical
        // Yours (`index_ours`) and Theirs (`index_theirs`).
        let needs_synthetic = conflict_id == 0
            && (index_ours.is_some()
                || index_theirs.is_some()
                || options.side_presence.is_delete_conflict());
        if needs_synthetic {
            let split_lines = |s: Option<&str>| -> Vec<String> {
                s.map(split_normalized_lines).unwrap_or_default()
            };
            let fallback_ours_label =
                options
                    .default_ours_label
                    .unwrap_or(if options.is_reversed {
                        "replayed"
                    } else {
                        "Ours"
                    });
            let fallback_theirs_label =
                options
                    .default_theirs_label
                    .unwrap_or(if options.is_reversed {
                        "onto"
                    } else {
                        "Theirs"
                    });
            segments.clear();
            segments.push(ConflictSegment::Conflict(ConflictBlock {
                id: 0,
                kind: ThreeWayBlockKind::Conflict,
                ours_label: fallback_ours_label.into(),
                theirs_label: fallback_theirs_label.into(),
                ours_lines: split_lines(index_ours),
                base_lines: Vec::new(),
                theirs_lines: split_lines(index_theirs),
                choice: ConflictChoice::Unresolved,
            }));
            conflict_id = 1;
        }

        Self {
            path,
            segments,
            line_ending,
            has_trailing_newline,
            conflict_count: conflict_id,
            is_reversed: options.is_reversed,
            side_presence: options.side_presence,
            has_base,
        }
    }

    pub const fn is_delete_modify(&self) -> bool {
        self.side_presence.is_delete_conflict()
    }

    /// Returns `true` when all conflicts are resolved and the selected choice
    /// deletes the file (Delete/Modify or Both-Deleted conflict).
    pub fn deletes_file(&self) -> bool {
        if !self.is_delete_modify() || self.unresolved_count() > 0 {
            return false;
        }
        let Some(block) = self.block(0) else {
            return false;
        };
        match self.side_presence {
            ConflictSidePresence::BothPresent => false,
            ConflictSidePresence::DeletedByTheirs => block.choice == ConflictChoice::Theirs,
            ConflictSidePresence::DeletedByOurs => block.choice == ConflictChoice::Ours,
            ConflictSidePresence::BothDeleted => !matches!(
                block.choice,
                ConflictChoice::Unresolved | ConflictChoice::Custom(_)
            ),
        }
    }

    pub fn unresolved_count(&self) -> usize {
        self.segments
            .iter()
            .filter(|s| {
                matches!(
                    s,
                    ConflictSegment::Conflict(b) if b.choice == ConflictChoice::Unresolved
                )
            })
            .count()
    }

    /// Number of still-unresolved blocks that are non-conflicting (`LeftOnly` /
    /// `RightOnly` / `BothSame`) and can be auto-applied in one click.
    pub fn non_conflicting_unresolved_count(&self) -> usize {
        self.segments
            .iter()
            .filter(|s| {
                matches!(
                    s,
                    ConflictSegment::Conflict(b)
                        if b.choice == ConflictChoice::Unresolved
                            && b.kind.is_non_conflicting_change()
                )
            })
            .count()
    }

    /// Auto-resolves every still-unresolved non-conflicting block (`LeftOnly` ->
    /// `Ours`, `RightOnly` -> `Theirs`, `BothSame` -> `Ours`), returning how
    /// many blocks were resolved.
    pub fn apply_non_conflicting(&mut self) -> usize {
        let mut applied = 0usize;
        for seg in &mut self.segments {
            if let ConflictSegment::Conflict(b) = seg
                && b.choice == ConflictChoice::Unresolved
            {
                match b.kind {
                    ThreeWayBlockKind::LeftOnly | ThreeWayBlockKind::BothSame => {
                        b.choice = ConflictChoice::Ours;
                        applied += 1;
                    }
                    ThreeWayBlockKind::RightOnly => {
                        b.choice = ConflictChoice::Theirs;
                        applied += 1;
                    }
                    ThreeWayBlockKind::Unchanged | ThreeWayBlockKind::Conflict => {}
                }
            }
        }
        applied
    }

    /// Returns the `id` of the first still-unresolved conflict block, if any.
    pub fn first_unresolved_block(&self) -> Option<usize> {
        self.segments.iter().find_map(|s| match s {
            ConflictSegment::Conflict(b) if b.choice == ConflictChoice::Unresolved => Some(b.id),
            _ => None,
        })
    }

    pub fn block(&self, idx: usize) -> Option<&ConflictBlock> {
        self.segments.iter().find_map(|s| match s {
            ConflictSegment::Conflict(b) if b.id == idx => Some(b),
            _ => None,
        })
    }

    pub fn set_choice(&mut self, block_idx: usize, choice: ConflictChoice) {
        for seg in &mut self.segments {
            if let ConflictSegment::Conflict(b) = seg
                && b.id == block_idx
            {
                b.choice = choice;
                break;
            }
        }
    }

    /// Resets a single conflict block back to [`ConflictChoice::Unresolved`].
    pub fn reset_block(&mut self, block_idx: usize) {
        self.set_choice(block_idx, ConflictChoice::Unresolved);
    }

    pub fn accept_all(&mut self, choice: ConflictChoice) {
        for seg in &mut self.segments {
            if let ConflictSegment::Conflict(b) = seg {
                b.choice = choice.clone();
            }
        }
    }

    /// Renders the resolved file contents using the original [`LineEnding`]
    /// (`\r\n` or `\n`) and [`Self::has_trailing_newline`]. Any still-unresolved
    /// block retains standard Git conflict markers (preserving Git's native
    /// orientation when [`Self::is_reversed`] is `true` so reopening is
    /// idempotent).
    pub fn render_resolved(&self) -> String {
        let mut lines = Vec::new();
        for seg in &self.segments {
            match seg {
                ConflictSegment::Common(ls) => lines.extend(ls.iter().cloned()),
                ConflictSegment::Conflict(b) => match b.resolved_lines() {
                    Some(resolved) => lines.extend(resolved),
                    None => match b.kind {
                        ThreeWayBlockKind::LeftOnly | ThreeWayBlockKind::BothSame => {
                            lines.extend(b.ours_lines.iter().cloned());
                        }
                        ThreeWayBlockKind::RightOnly => {
                            lines.extend(b.theirs_lines.iter().cloned());
                        }
                        _ if self.side_presence == ConflictSidePresence::BothDeleted => {}
                        _ => {
                            if self.is_reversed {
                                lines.push(format!("<<<<<<< {}", b.theirs_label));
                                lines.extend(b.theirs_lines.iter().cloned());
                                if !b.base_lines.is_empty() {
                                    lines.push("||||||| base".to_string());
                                    lines.extend(b.base_lines.iter().cloned());
                                }
                                lines.push("=======".to_string());
                                lines.extend(b.ours_lines.iter().cloned());
                                lines.push(format!(">>>>>>> {}", b.ours_label));
                            } else {
                                lines.push(format!("<<<<<<< {}", b.ours_label));
                                lines.extend(b.ours_lines.iter().cloned());
                                if !b.base_lines.is_empty() {
                                    lines.push("||||||| base".to_string());
                                    lines.extend(b.base_lines.iter().cloned());
                                }
                                lines.push("=======".to_string());
                                lines.extend(b.theirs_lines.iter().cloned());
                                lines.push(format!(">>>>>>> {}", b.theirs_label));
                            }
                        }
                    },
                },
            }
        }
        let sep = match self.line_ending {
            LineEnding::Crlf => "\r\n",
            LineEnding::Lf | LineEnding::None => "\n",
        };
        let mut out = lines.join(sep);
        if self.has_trailing_newline && !out.is_empty() {
            out.push_str(sep);
        }
        out
    }

    /// Backwards-compatible alias for [`Self::render_resolved`].
    pub fn render_merged(&self) -> String {
        self.render_resolved()
    }

    /// Returns `(first_row, total_rows)` for conflict block `block_idx` in the
    /// aligned three-pane row table produced by [`Self::three_pane_rows`].
    pub fn block_first_row(&self, block_idx: usize) -> Option<(usize, usize)> {
        let mut row = 0usize;
        let mut found = None;
        for seg in &self.segments {
            match seg {
                ConflictSegment::Common(ls) => row += ls.len(),
                ConflictSegment::Conflict(b) => {
                    if b.id == block_idx {
                        found = Some(row);
                    }
                    row += b.pane_row_count(self.side_presence);
                }
            }
        }
        found.map(|first| (first, row))
    }

    /// Builds aligned rows for the three-pane `(Ours | Result | Theirs)` view.
    pub fn three_pane_rows(&self) -> Vec<ConflictPaneRow> {
        let mut rows = Vec::new();
        for seg in &self.segments {
            match seg {
                ConflictSegment::Common(ls) => {
                    for l in ls {
                        rows.push(ConflictPaneRow {
                            block_idx: None,
                            block_kind: None,
                            ours: l.clone(),
                            merged: l.clone(),
                            theirs: l.clone(),
                            choice: None,
                            merged_placeholder: None,
                            ours_deleted_placeholder: false,
                            theirs_deleted_placeholder: false,
                        });
                    }
                }
                ConflictSegment::Conflict(b) => {
                    let choice_deletes = match self.side_presence {
                        ConflictSidePresence::BothPresent => false,
                        ConflictSidePresence::DeletedByTheirs => b.choice == ConflictChoice::Theirs,
                        ConflictSidePresence::DeletedByOurs => b.choice == ConflictChoice::Ours,
                        ConflictSidePresence::BothDeleted => !matches!(
                            b.choice,
                            ConflictChoice::Unresolved | ConflictChoice::Custom(_)
                        ),
                    };
                    let (merged_lines, placeholder) = if choice_deletes {
                        (
                            vec!["<File will be deleted>".to_string()],
                            Some(ConflictRowPlaceholder::FileWillBeDeleted),
                        )
                    } else if b.choice == ConflictChoice::Neither {
                        (
                            vec!["<Discarded (Neither)>".to_string()],
                            Some(ConflictRowPlaceholder::DiscardedNeither),
                        )
                    } else {
                        match b.resolved_lines() {
                            Some(lines) if lines.is_empty() => (
                                vec!["<Empty>".to_string()],
                                Some(ConflictRowPlaceholder::Empty),
                            ),
                            Some(lines) => (lines, None),
                            None => {
                                let block_num = b.id + 1;
                                let (hint, kind) = match self.side_presence {
                                    ConflictSidePresence::BothPresent => match b.kind {
                                        ThreeWayBlockKind::LeftOnly => (
                                            "Non-conflicting (Left): [a] Auto / [y] Ours",
                                            ConflictRowPlaceholder::UnresolvedLeftOnly {
                                                block_num,
                                            },
                                        ),
                                        ThreeWayBlockKind::RightOnly => (
                                            "Non-conflicting (Right): [a] Auto / [t] Theirs",
                                            ConflictRowPlaceholder::UnresolvedRightOnly {
                                                block_num,
                                            },
                                        ),
                                        ThreeWayBlockKind::BothSame => (
                                            "Non-conflicting (Same): [a] Auto / [y] Keep",
                                            ConflictRowPlaceholder::UnresolvedBothSame {
                                                block_num,
                                            },
                                        ),
                                        ThreeWayBlockKind::Unchanged
                                        | ThreeWayBlockKind::Conflict => (
                                            "choose [y] Ours / [t] Theirs / [d] Neither / [e] Edit",
                                            ConflictRowPlaceholder::UnresolvedConflict {
                                                block_num,
                                            },
                                        ),
                                    },
                                    ConflictSidePresence::DeletedByTheirs => (
                                        "choose [y] Keep file / [t] Delete file",
                                        ConflictRowPlaceholder::UnresolvedDeletedByTheirs {
                                            block_num,
                                        },
                                    ),
                                    ConflictSidePresence::DeletedByOurs => (
                                        "choose [y] Delete file / [t] Keep file",
                                        ConflictRowPlaceholder::UnresolvedDeletedByOurs {
                                            block_num,
                                        },
                                    ),
                                    ConflictSidePresence::BothDeleted => (
                                        "choose [y]/[t] Delete file",
                                        ConflictRowPlaceholder::UnresolvedBothDeleted { block_num },
                                    ),
                                };
                                (vec![format!("⚠ Block #{block_num}: {hint}")], Some(kind))
                            }
                        }
                    };
                    let max_len = b.pane_row_count(self.side_presence);
                    for i in 0..max_len {
                        let ours_deleted_placeholder =
                            i == 0 && b.ours_lines.is_empty() && self.side_presence.ours_deleted();
                        let ours = if ours_deleted_placeholder {
                            "<Deleted>".to_string()
                        } else {
                            b.ours_lines.get(i).cloned().unwrap_or_default()
                        };
                        let theirs_deleted_placeholder = i == 0
                            && b.theirs_lines.is_empty()
                            && self.side_presence.theirs_deleted();
                        let theirs = if theirs_deleted_placeholder {
                            "<Deleted>".to_string()
                        } else {
                            b.theirs_lines.get(i).cloned().unwrap_or_default()
                        };
                        rows.push(ConflictPaneRow {
                            block_idx: Some(b.id),
                            block_kind: Some(b.kind),
                            ours,
                            merged: merged_lines.get(i).cloned().unwrap_or_default(),
                            theirs,
                            choice: Some(b.choice.clone()),
                            merged_placeholder: if i == 0 { placeholder } else { None },
                            ours_deleted_placeholder,
                            theirs_deleted_placeholder,
                        });
                    }
                }
            }
        }
        rows
    }
}

fn parse_open_marker(line: &str) -> Option<(usize, &str)> {
    let n = line.bytes().take_while(|&b| b == b'<').count();
    if n < 7 {
        return None;
    }
    let rest = &line[n..];
    if rest.is_empty() || rest.starts_with(' ') || rest.starts_with('\t') {
        Some((n, rest.trim()))
    } else {
        None
    }
}

fn is_base_marker(line: &str, marker_len: usize) -> bool {
    let n = line.bytes().take_while(|&b| b == b'|').count();
    if n != marker_len {
        return false;
    }
    let rest = &line[n..];
    rest.is_empty() || rest.starts_with(' ') || rest.starts_with('\t')
}

fn is_sep_marker(line: &str, marker_len: usize) -> bool {
    line.len() == marker_len && line.bytes().all(|b| b == b'=')
}

fn parse_close_marker(line: &str, marker_len: usize) -> Option<&str> {
    let n = line.bytes().take_while(|&b| b == b'>').count();
    if n != marker_len {
        return None;
    }
    let rest = &line[n..];
    if rest.is_empty() || rest.starts_with(' ') || rest.starts_with('\t') {
        Some(rest.trim())
    } else {
        None
    }
}

fn count_line_endings(text: &str, skip_markers: bool) -> (usize, usize) {
    let mut crlf = 0usize;
    let mut lf = 0usize;
    for piece in text.split_inclusive('\n') {
        let Some(without_lf) = piece.strip_suffix('\n') else {
            continue;
        };
        let (content, is_crlf) = match without_lf.strip_suffix('\r') {
            Some(c) => (c, true),
            None => (without_lf, false),
        };
        if skip_markers
            && (content.starts_with("<<<<<<<")
                || content.starts_with("|||||||")
                || content.starts_with("=======")
                || content.starts_with(">>>>>>>"))
        {
            continue;
        }
        if is_crlf {
            crlf += 1;
        } else {
            lf += 1;
        }
    }
    (crlf, lf)
}

fn detect_line_ending(
    worktree_text: &str,
    index_ours: Option<&str>,
    index_theirs: Option<&str>,
) -> LineEnding {
    let (mut crlf, mut lf) = count_line_endings(worktree_text, true);
    if crlf == 0 && lf == 0 {
        if let Some(ours) = index_ours {
            let (c, l) = count_line_endings(ours, false);
            crlf += c;
            lf += l;
        }
        if let Some(theirs) = index_theirs {
            let (c, l) = count_line_endings(theirs, false);
            crlf += c;
            lf += l;
        }
    }
    if crlf > 0 && crlf >= lf {
        LineEnding::Crlf
    } else {
        LineEnding::Lf
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn parses_markers_resolves_per_block_and_renders_merged() {
        let src = "\
fn a() {}
<<<<<<< HEAD
let x = 1;
=======
let x = 2;
>>>>>>> feature
fn mid() {}
<<<<<<< HEAD
let y = 10;
||||||| base
let y = 0;
=======
let y = 20;
>>>>>>> feature
";
        let mut doc = ConflictDocument::parse("src/lib.rs", src, None, None);
        assert_eq!(doc.conflict_count, 2);
        assert_eq!(doc.unresolved_count(), 2);
        assert_eq!(doc.line_ending, LineEnding::Lf);
        assert!(doc.has_trailing_newline);
        assert!(doc.has_base);

        doc.set_choice(0, ConflictChoice::Ours);
        doc.set_choice(1, ConflictChoice::BothOursFirst);
        assert_eq!(doc.unresolved_count(), 0);
        assert_eq!(
            doc.render_merged(),
            "fn a() {}\nlet x = 1;\nfn mid() {}\nlet y = 10;\nlet y = 20;\n"
        );
        let rows = doc.three_pane_rows();
        assert!(
            rows.iter()
                .any(|r| r.block_idx == Some(0) && r.ours == "let x = 1;")
        );
        assert!(
            rows.iter()
                .any(|r| r.block_idx == Some(1) && r.theirs == "let y = 20;")
        );
    }

    #[test]
    fn falls_back_to_index_stages_when_no_markers_exist() {
        let doc = ConflictDocument::parse("a.txt", "", Some("ours\n"), Some("theirs\n"));
        assert_eq!(doc.conflict_count, 1);
        let b = doc.block(0).unwrap();
        assert_eq!(b.ours_lines, vec!["ours"]);
        assert_eq!(b.theirs_lines, vec!["theirs"]);
    }

    #[test]
    fn reverse_conflict_state_matches_rebase_variants_only() {
        assert!(is_reverse_conflict_state(&RepositoryState::Rebase {
            progress: None,
            branch: None,
        }));
        assert!(is_reverse_conflict_state(
            &RepositoryState::RebaseInteractive {
                progress: Some((1, 3)),
                branch: Some("feat".into()),
            }
        ));
        assert!(is_reverse_conflict_state(&RepositoryState::RebaseMerge {
            progress: None,
            branch: None,
        }));
        assert!(!is_reverse_conflict_state(&RepositoryState::Clean));
        assert!(!is_reverse_conflict_state(&RepositoryState::Merge));
        assert!(!is_reverse_conflict_state(&RepositoryState::CherryPick));
        assert!(!is_reverse_conflict_state(&RepositoryState::Revert));
        assert!(!is_reverse_conflict_state(&RepositoryState::Bisect));
        assert!(!is_reverse_conflict_state(&RepositoryState::Detached));
    }

    #[test]
    fn reversed_conflict_swaps_markers_and_renders_idempotently() {
        let raw = "\
<<<<<<< HEAD
upstream line
=======
my replayed change
>>>>>>> abc1234 (my commit)
";
        let mut doc = ConflictDocument::parse_with_options(
            "a.rs",
            raw,
            None,
            None,
            ConflictParseOptions {
                is_reversed: true,
                side_presence: ConflictSidePresence::BothPresent,
                default_ours_label: Some("feat"),
                default_theirs_label: Some("main"),
            },
        );
        assert!(doc.is_reversed);
        let b = doc.block(0).unwrap();
        assert_eq!(b.ours_lines, vec!["my replayed change"]);
        assert_eq!(b.ours_label, "abc1234 (my commit)");
        assert_eq!(b.theirs_lines, vec!["upstream line"]);
        assert_eq!(b.theirs_label, "main");

        let unresolved = doc.render_resolved();
        assert!(unresolved.contains(
            "<<<<<<< main\nupstream line\n=======\nmy replayed change\n>>>>>>> abc1234 (my commit)\n"
        ));

        doc.set_choice(0, ConflictChoice::Ours);
        assert_eq!(doc.render_resolved(), "my replayed change\n");
    }

    #[test]
    fn preserves_crlf_even_with_lf_git_markers_and_missing_trailing_newline() {
        let crlf_with_lf_markers =
            "header\r\n<<<<<<< HEAD\nours line\r\n=======\ntheirs line\r\n>>>>>>> feat\nfooter\r\n";
        let mut doc = ConflictDocument::parse("win.txt", crlf_with_lf_markers, None, None);
        assert_eq!(doc.line_ending, LineEnding::Crlf);
        assert!(doc.has_trailing_newline);
        doc.set_choice(0, ConflictChoice::Ours);
        assert_eq!(doc.render_resolved(), "header\r\nours line\r\nfooter\r\n");

        let no_eof_nl = "<<<<<<< HEAD\nours\r\n=======\ntheirs\r\n>>>>>>> feat\ntail";
        let mut doc2 = ConflictDocument::parse("win2.txt", no_eof_nl, None, None);
        assert_eq!(doc2.line_ending, LineEnding::Crlf);
        assert!(!doc2.has_trailing_newline);
        doc2.set_choice(0, ConflictChoice::Theirs);
        assert_eq!(doc2.render_resolved(), "theirs\r\ntail");
    }

    #[test]
    fn delete_modify_and_both_deleted_report_deletes_file_accurately() {
        let mut deleted_by_theirs = ConflictDocument::parse_with_options(
            "gone.txt",
            "local content\n",
            Some("local content\n"),
            None,
            ConflictParseOptions {
                side_presence: ConflictSidePresence::DeletedByTheirs,
                ..Default::default()
            },
        );
        assert!(deleted_by_theirs.is_delete_modify());
        assert!(!deleted_by_theirs.deletes_file());
        deleted_by_theirs.set_choice(0, ConflictChoice::Ours);
        assert!(!deleted_by_theirs.deletes_file());
        assert_eq!(deleted_by_theirs.render_resolved(), "local content\n");
        deleted_by_theirs.set_choice(0, ConflictChoice::Theirs);
        assert!(deleted_by_theirs.deletes_file());

        let mut both_deleted = ConflictDocument::parse_with_options(
            "dd.txt",
            "",
            None,
            None,
            ConflictParseOptions {
                side_presence: ConflictSidePresence::BothDeleted,
                ..Default::default()
            },
        );
        assert_eq!(both_deleted.conflict_count, 1);
        assert!(!both_deleted.deletes_file());
        both_deleted.set_choice(0, ConflictChoice::Ours);
        assert!(both_deleted.deletes_file());
    }

    #[test]
    fn block_first_row_matches_three_pane_rows_positions() {
        let mut raw = String::new();
        for i in 0..40 {
            raw.push_str(&format!("line {i}\n"));
        }
        raw.push_str("<<<<<<< HEAD\na1\na2\n=======\nb1\n>>>>>>> feat\n");
        for i in 0..50 {
            raw.push_str(&format!("mid {i}\n"));
        }
        raw.push_str("<<<<<<< HEAD\nc1\n=======\nd1\nd2\nd3\n>>>>>>> feat\n");
        let doc = ConflictDocument::parse("big.rs", &raw, None, None);
        let rows = doc.three_pane_rows();
        for bid in 0..doc.conflict_count {
            let (first, total) = doc.block_first_row(bid).unwrap();
            assert_eq!(total, rows.len());
            assert_eq!(
                rows.iter().position(|r| r.block_idx == Some(bid)),
                Some(first)
            );
        }
    }

    #[test]
    fn three_way_merge_classifies_left_right_both_same_and_conflict() {
        let base = lines(&["a", "b", "c", "d", "e", "f", "g"]);
        let ours = lines(&["a-left", "b", "c-same", "d", "e-ours", "f", "g"]);
        let theirs = lines(&["a", "b", "c-same", "d", "e-theirs", "f", "g-right"]);

        let hunks = three_way_merge_blocks(Some(&base), &ours, &theirs);
        let kinds: Vec<ThreeWayBlockKind> = hunks.iter().map(|h| h.kind).collect();
        assert_eq!(
            kinds,
            vec![
                ThreeWayBlockKind::LeftOnly,
                ThreeWayBlockKind::Unchanged,
                ThreeWayBlockKind::BothSame,
                ThreeWayBlockKind::Unchanged,
                ThreeWayBlockKind::Conflict,
                ThreeWayBlockKind::Unchanged,
                ThreeWayBlockKind::RightOnly,
            ]
        );
        assert_eq!(hunks[0].ours_lines, lines(&["a-left"]));
        assert_eq!(hunks[2].ours_lines, lines(&["c-same"]));
        assert_eq!(hunks[4].base_lines, lines(&["e"]));
        assert_eq!(hunks[4].ours_lines, lines(&["e-ours"]));
        assert_eq!(hunks[4].theirs_lines, lines(&["e-theirs"]));
        assert_eq!(hunks[6].theirs_lines, lines(&["g-right"]));
    }

    #[test]
    fn three_way_add_add_without_base_identifies_shared_and_conflicting_regions() {
        let ours = lines(&["common1", "ours-only", "common2"]);
        let theirs = lines(&["common1", "theirs-only", "common2"]);
        let hunks = three_way_merge_blocks(None, &ours, &theirs);
        let kinds: Vec<ThreeWayBlockKind> = hunks.iter().map(|h| h.kind).collect();
        assert_eq!(
            kinds,
            vec![
                ThreeWayBlockKind::Unchanged,
                ThreeWayBlockKind::Conflict,
                ThreeWayBlockKind::Unchanged,
            ]
        );
        assert!(hunks[1].base_lines.is_empty());
        assert_eq!(hunks[1].ours_lines, lines(&["ours-only"]));
        assert_eq!(hunks[1].theirs_lines, lines(&["theirs-only"]));
    }

    #[test]
    fn from_three_way_applies_non_conflicting_and_supports_neither_custom_and_reset() {
        let base = "a\nb\nc\nd\ne\n";
        let ours = "a-left\nb\nc-ours\nd\ne\n";
        let theirs = "a\nb\nc-theirs\nd\ne-right\n";

        let mut doc = ConflictDocument::from_three_way(
            "src/app.rs",
            "",
            Some(base),
            Some(ours),
            Some(theirs),
            ConflictParseOptions::default(),
        );
        assert!(doc.has_base);
        assert_eq!(doc.conflict_count, 3);
        assert_eq!(doc.non_conflicting_unresolved_count(), 2);

        // Apply Non-Conflicting resolves block 0 (LeftOnly) and block 2 (RightOnly), leaving block 1.
        assert_eq!(doc.apply_non_conflicting(), 2);
        assert_eq!(doc.non_conflicting_unresolved_count(), 0);
        assert_eq!(doc.unresolved_count(), 1);
        assert_eq!(doc.first_unresolved_block(), Some(1));

        // Custom inline edit on block 1.
        doc.set_choice(
            1,
            ConflictChoice::Custom(lines(&["c-custom-1", "c-custom-2"])),
        );
        assert_eq!(doc.unresolved_count(), 0);
        assert_eq!(
            doc.render_resolved(),
            "a-left\nb\nc-custom-1\nc-custom-2\nd\ne-right\n"
        );

        // Reset block 1 back to Unresolved, then choose Neither (discard both).
        doc.reset_block(1);
        assert_eq!(doc.unresolved_count(), 1);
        doc.set_choice(1, ConflictChoice::Neither);
        assert_eq!(doc.unresolved_count(), 0);
        assert_eq!(doc.render_resolved(), "a-left\nb\nd\ne-right\n");
    }

    #[test]
    fn conflict_side_statuses_flip_under_rebase() {
        use ConflictSideStatus::*;
        assert_eq!(
            conflict_side_statuses(Some("Deleted by Them"), false),
            (Modified, Deleted)
        );
        assert_eq!(
            conflict_side_statuses(Some("Deleted by Them"), true),
            (Deleted, Modified)
        );
        assert_eq!(
            conflict_side_statuses(Some("Added by Both"), true),
            (Added, Added)
        );
    }

    #[test]
    fn render_resolved_emits_clean_non_conflicting_blocks_and_both_deleted_custom_keeps_file() {
        let mut doc = ConflictDocument::from_three_way(
            "a.txt",
            "left-only\nkeep\n",
            Some("base\nkeep\n"),
            Some("left-only\nkeep\n"),
            Some("base\nkeep\n"),
            ConflictParseOptions::default(),
        );
        // Reset non-conflicting LeftOnly block back to Unresolved — must still render cleanly without markers.
        doc.reset_block(0);
        let rendered = doc.render_resolved();
        assert!(!rendered.contains("<<<<<<<"), "{rendered}");
        assert_eq!(rendered, "left-only\nkeep\n");

        // BothDeleted with Custom inline edit must NOT report deletes_file() == true.
        let mut del_doc = ConflictDocument::from_three_way(
            "gone.txt",
            "",
            Some("old\n"),
            None,
            None,
            ConflictParseOptions {
                side_presence: ConflictSidePresence::BothDeleted,
                ..Default::default()
            },
        );
        assert_eq!(del_doc.side_presence, ConflictSidePresence::BothDeleted);
        assert!(
            !del_doc.deletes_file(),
            "unresolved BothDeleted must not auto-delete"
        );
        del_doc.set_choice(0, ConflictChoice::Ours);
        assert!(del_doc.deletes_file());
        del_doc.set_choice(0, ConflictChoice::Custom(vec!["revived".into()]));
        assert!(!del_doc.deletes_file());
        assert_eq!(del_doc.render_resolved(), "revived");
    }

    #[test]
    fn delete_modify_with_literal_markers_synthesizes_whole_file_and_preserves_base() {
        let surviving = "\
intro
<<<<<<< HEAD
literal example
=======
other example
>>>>>>> branch
outro
";
        let base = "old base\n";
        let mut doc = ConflictDocument::from_three_way(
            "docs/conflict.md",
            surviving,
            Some(base),
            Some(surviving),
            None,
            ConflictParseOptions {
                side_presence: ConflictSidePresence::DeletedByTheirs,
                ..Default::default()
            },
        );
        assert_eq!(doc.conflict_count, 1);
        assert!(doc.has_base);
        let b = doc.block(0).unwrap();
        assert_eq!(b.base_lines, vec!["old base"]);
        assert_eq!(b.ours_lines.len(), 7);
        assert!(b.theirs_lines.is_empty());

        doc.set_choice(0, ConflictChoice::Ours);
        assert_eq!(doc.render_resolved(), surviving);
    }

    #[test]
    fn marker_fallback_does_not_split_ours_on_longer_equals_banner() {
        let src = "\
<<<<<<< HEAD
Title
===============
body from ours
=======
body from theirs
>>>>>>> feature
";
        let doc = ConflictDocument::parse("README.md", src, None, None);
        assert_eq!(doc.conflict_count, 1);
        let b = doc.block(0).unwrap();
        assert_eq!(
            b.ours_lines,
            vec!["Title", "===============", "body from ours"]
        );
        assert_eq!(b.theirs_lines, vec!["body from theirs"]);
    }
}
