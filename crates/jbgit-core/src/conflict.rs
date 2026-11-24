//! Pure domain model for built-in three-pane conflict resolution:
//! parses Git conflict markers (`<<<<<<<` / `|||||||` / `=======` / `>>>>>>>`)
//! or falls back to stage 2 (Ours) / stage 3 (Theirs) index blobs, tracks
//! per-hunk resolution choices (`Ours` / `Theirs` / `Both`), handles Rebase
//! Ours/Theirs semantic reversal, Delete/Modify resolution, lossless line-ending
//! preservation (CRLF / LF), and non-text (binary / submodule / directory)
//! conflict descriptors.

use crate::{LineEnding, Oid, RepositoryState};

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictChoice {
    Unresolved,
    Ours,
    Theirs,
    BothOursFirst,
    BothTheirsFirst,
}

impl ConflictChoice {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Unresolved => "Unresolved",
            Self::Ours => "Ours",
            Self::Theirs => "Theirs",
            Self::BothOursFirst => "Both (Ours + Theirs)",
            Self::BothTheirsFirst => "Both (Theirs + Ours)",
        }
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
    pub ours_label: String,
    pub theirs_label: String,
    pub ours_lines: Vec<String>,
    pub base_lines: Vec<String>,
    pub theirs_lines: Vec<String>,
    pub choice: ConflictChoice,
}

impl ConflictBlock {
    pub fn resolved_lines(&self) -> Option<Vec<String>> {
        match self.choice {
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConflictPaneRow {
    pub block_idx: Option<usize>,
    pub ours: String,
    pub merged: String,
    pub theirs: String,
    pub choice: Option<ConflictChoice>,
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
        let raw_lines: Vec<String> = worktree_text
            .lines()
            .map(|l| l.trim_end_matches('\r').to_string())
            .collect();

        let default_ours = options.default_ours_label.unwrap_or(if options.is_reversed {
            "replayed"
        } else {
            "HEAD"
        });
        let default_theirs = options.default_theirs_label.unwrap_or(if options.is_reversed {
            "onto"
        } else {
            "Incoming"
        });

        let mut segments = Vec::new();
        let mut common = Vec::new();
        let mut i = 0usize;
        let mut conflict_id = 0usize;

        while i < raw_lines.len() {
            if let Some(rest) = raw_lines[i].strip_prefix("<<<<<<<") {
                let top_label = rest.trim().to_string();
                let start = i;
                i += 1;
                let mut top_lines = Vec::new();
                let mut base_lines = Vec::new();
                let mut bottom_lines = Vec::new();
                let mut section = 0u8; // 0 = top, 1 = base, 2 = bottom
                let mut closed = None;
                while i < raw_lines.len() {
                    if raw_lines[i].starts_with("|||||||") && section == 0 {
                        section = 1;
                    } else if raw_lines[i].starts_with("=======") && section < 2 {
                        section = 2;
                    } else if let Some(trest) = raw_lines[i].strip_prefix(">>>>>>>")
                        && section == 2
                    {
                        closed = Some(trest.trim().to_string());
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
                    if !common.is_empty() {
                        segments.push(ConflictSegment::Common(std::mem::take(&mut common)));
                    }
                    let (ours_label, theirs_label, ours_lines, theirs_lines) =
                        if options.is_reversed {
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

        // If no inline markers were found but index stages differ (or both
        // sides deleted the file), synthesize one conflict block from logical
        // Yours (`index_ours`) and Theirs (`index_theirs`).
        let needs_synthetic = conflict_id == 0
            && (index_ours.is_some()
                || index_theirs.is_some()
                || options.side_presence.is_delete_conflict());
        if needs_synthetic {
            let split_lines = |s: Option<&str>| -> Vec<String> {
                s.map(|t| {
                    t.lines()
                        .map(|l| l.trim_end_matches('\r').to_string())
                        .collect()
                })
                .unwrap_or_default()
            };
            let fallback_ours_label = options
                .default_ours_label
                .unwrap_or(if options.is_reversed { "replayed" } else { "Ours" });
            let fallback_theirs_label = options
                .default_theirs_label
                .unwrap_or(if options.is_reversed { "onto" } else { "Theirs" });
            segments.clear();
            segments.push(ConflictSegment::Conflict(ConflictBlock {
                id: 0,
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
            ConflictSidePresence::BothDeleted => block.choice != ConflictChoice::Unresolved,
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

    pub fn accept_all(&mut self, choice: ConflictChoice) {
        for seg in &mut self.segments {
            if let ConflictSegment::Conflict(b) = seg {
                b.choice = choice;
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
                    None => {
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
                            ours: l.clone(),
                            merged: l.clone(),
                            theirs: l.clone(),
                            choice: None,
                        });
                    }
                }
                ConflictSegment::Conflict(b) => {
                    let choice_deletes = match self.side_presence {
                        ConflictSidePresence::BothPresent => false,
                        ConflictSidePresence::DeletedByTheirs => {
                            b.choice == ConflictChoice::Theirs
                        }
                        ConflictSidePresence::DeletedByOurs => b.choice == ConflictChoice::Ours,
                        ConflictSidePresence::BothDeleted => b.choice != ConflictChoice::Unresolved,
                    };
                    let merged_lines = if choice_deletes {
                        vec!["<File will be deleted>".to_string()]
                    } else {
                        b.resolved_lines().unwrap_or_else(|| {
                            let hint = match self.side_presence {
                                ConflictSidePresence::BothPresent => {
                                    "choose [y] Ours / [t] Theirs / [b] Both"
                                }
                                ConflictSidePresence::DeletedByTheirs => {
                                    "choose [y] Keep file / [t] Delete file"
                                }
                                ConflictSidePresence::DeletedByOurs => {
                                    "choose [y] Delete file / [t] Keep file"
                                }
                                ConflictSidePresence::BothDeleted => {
                                    "choose [y]/[t] Delete file"
                                }
                            };
                            vec![format!("⚠ Conflict #{}: {hint}", b.id + 1)]
                        })
                    };
                    let max_len = b.pane_row_count(self.side_presence);
                    for i in 0..max_len {
                        let ours = if i == 0
                            && b.ours_lines.is_empty()
                            && self.side_presence.ours_deleted()
                        {
                            "<Deleted>".to_string()
                        } else {
                            b.ours_lines.get(i).cloned().unwrap_or_default()
                        };
                        let theirs = if i == 0
                            && b.theirs_lines.is_empty()
                            && self.side_presence.theirs_deleted()
                        {
                            "<Deleted>".to_string()
                        } else {
                            b.theirs_lines.get(i).cloned().unwrap_or_default()
                        };
                        rows.push(ConflictPaneRow {
                            block_idx: Some(b.id),
                            ours,
                            merged: merged_lines.get(i).cloned().unwrap_or_default(),
                            theirs,
                            choice: Some(b.choice),
                        });
                    }
                }
            }
        }
        rows
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

        doc.set_choice(0, ConflictChoice::Ours);
        doc.set_choice(1, ConflictChoice::BothOursFirst);
        assert_eq!(doc.unresolved_count(), 0);
        assert_eq!(
            doc.render_merged(),
            "fn a() {}\nlet x = 1;\nfn mid() {}\nlet y = 10;\nlet y = 20;\n"
        );
        let rows = doc.three_pane_rows();
        assert!(rows.iter().any(|r| r.block_idx == Some(0) && r.ours == "let x = 1;"));
        assert!(rows.iter().any(|r| r.block_idx == Some(1) && r.theirs == "let y = 20;"));
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

        // Unresolved render keeps Git's native rebase orientation so reopening is idempotent.
        let unresolved = doc.render_resolved();
        assert!(unresolved.contains("<<<<<<< main\nupstream line\n=======\nmy replayed change\n>>>>>>> abc1234 (my commit)\n"));

        // Accepting Ours chooses the replayed local commit.
        doc.set_choice(0, ConflictChoice::Ours);
        assert_eq!(doc.render_resolved(), "my replayed change\n");
    }

    #[test]
    fn preserves_crlf_even_with_lf_git_markers_and_missing_trailing_newline() {
        let crlf_with_lf_markers = "header\r\n<<<<<<< HEAD\nours line\r\n=======\ntheirs line\r\n>>>>>>> feat\nfooter\r\n";
        let mut doc = ConflictDocument::parse("win.txt", crlf_with_lf_markers, None, None);
        assert_eq!(doc.line_ending, LineEnding::Crlf);
        assert!(doc.has_trailing_newline);
        doc.set_choice(0, ConflictChoice::Ours);
        assert_eq!(doc.render_resolved(), "header\r\nours line\r\nfooter\r\n");

        // No trailing newline must be preserved.
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
            assert_eq!(rows.iter().position(|r| r.block_idx == Some(bid)), Some(first));
        }
    }
}
