//! Three-document lossless line tracker (`GitStageLineStatusTracker` + `BlockMerger`)
//! and pure-memory partial staging / unstaging / rollback buffer synthesis.

use std::ops::Range;

use imara_diff::{Algorithm, Diff, InternedInput};
use rustc_hash::FxHashSet;

use crate::details::{DiffContext, DiffHunk, DiffLine, DiffLineKind, apply_word_diff};

/// Line ending metadata for lossless byte-for-byte document reconstruction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineEnding {
    Lf,
    Crlf,
    None,
}

impl LineEnding {
    pub const fn as_bytes(self) -> &'static [u8] {
        match self {
            Self::Lf => b"\n",
            Self::Crlf => b"\r\n",
            Self::None => b"",
        }
    }
}

/// One lossless line slice (content bytes without trailing `\n` or `\r\n`, plus ending tag).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlicedLine {
    pub content: Vec<u8>,
    pub ending: LineEnding,
}

impl SlicedLine {
    pub fn to_bytes_into(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.content);
        out.extend_from_slice(self.ending.as_bytes());
    }

    pub fn display_text(&self) -> String {
        String::from_utf8_lossy(&self.content).replace('\t', "    ")
    }
}

/// Lossless line-sliced representation of an arbitrary byte buffer.
/// `SlicedDocument::from_bytes(b).to_bytes() == b` for every byte sequence `b`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SlicedDocument {
    pub lines: Vec<SlicedLine>,
}

impl SlicedDocument {
    pub fn from_bytes(bytes: &[u8]) -> Self {
        if bytes.is_empty() {
            return Self { lines: Vec::new() };
        }
        let mut lines = Vec::new();
        let mut start = 0usize;
        for (i, &b) in bytes.iter().enumerate() {
            if b == b'\n' {
                if i > start && bytes[i - 1] == b'\r' {
                    lines.push(SlicedLine {
                        content: bytes[start..i - 1].to_vec(),
                        ending: LineEnding::Crlf,
                    });
                } else {
                    lines.push(SlicedLine {
                        content: bytes[start..i].to_vec(),
                        ending: LineEnding::Lf,
                    });
                }
                start = i + 1;
            }
        }
        if start < bytes.len() {
            lines.push(SlicedLine {
                content: bytes[start..].to_vec(),
                ending: LineEnding::None,
            });
        }
        Self { lines }
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let cap: usize = self
            .lines
            .iter()
            .map(|l| l.content.len() + l.ending.as_bytes().len())
            .sum();
        let mut out = Vec::with_capacity(cap);
        for line in &self.lines {
            line.to_bytes_into(&mut out);
        }
        out
    }

    pub fn len(&self) -> usize {
        self.lines.len()
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    pub fn slice_display(&self, range: Range<usize>) -> Vec<String> {
        let start = range.start.min(self.lines.len());
        let end = range.end.min(self.lines.len());
        self.lines[start..end]
            .iter()
            .map(SlicedLine::display_text)
            .collect()
    }
}

/// Half-open line-range hunk between two documents (`before[start1..end1]` -> `after[start2..end2]`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiffBlock {
    pub start1: usize,
    pub end1: usize,
    pub start2: usize,
    pub end2: usize,
}

impl DiffBlock {
    pub fn delta(&self) -> isize {
        let deleted = self.end1 as isize - self.start1 as isize;
        let inserted = self.end2 as isize - self.start2 as isize;
        inserted - deleted
    }
}

/// Computes exact line diff blocks between two byte slices using `imara-diff` Histogram.
/// Line indices match `SlicedDocument::from_bytes` 1-to-1.
pub fn compute_diff_blocks(before: &[u8], after: &[u8]) -> Vec<DiffBlock> {
    if before == after {
        return Vec::new();
    }
    let input = InternedInput::new(before, after);
    let mut diff = Diff::compute(Algorithm::Histogram, &input);
    diff.postprocess_lines(&input);
    diff.hunks()
        .map(|h| DiffBlock {
            start1: h.before.start as usize,
            end1: h.before.end as usize,
            start2: h.after.start as usize,
            end2: h.after.end as usize,
        })
        .collect()
}

/// Unified three-document change block output by [`merge_stage_blocks`] (`BlockMerger`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagedRange {
    /// Line range in worktree (`local_doc`) `[local_start, local_end)`.
    pub local_range: Range<usize>,
    /// Line range in index (`staged_doc`) `[staged_start, staged_end)`.
    pub staged_range: Range<usize>,
    /// Line range in HEAD (`vcs_doc`) `[vcs_start, vcs_end)`.
    pub vcs_range: Range<usize>,
    /// Whether this range has staged differences (`HEAD != Index`).
    pub has_staged: bool,
    /// Whether this range has unstaged differences (`Index != Worktree`).
    pub has_unstaged: bool,
}

/// Faithful Rust port of IDEA `GitStageLineStatusTracker.BlockMerger` (lines 922-1018).
///
/// Merges `staged_blocks = diff(vcs_doc, staged_doc)` and
/// `unstaged_blocks = diff(staged_doc, local_doc)` using `staged_doc` as the common pivot.
pub fn merge_stage_blocks(
    staged_blocks: &[DiffBlock],
    unstaged_blocks: &[DiffBlock],
) -> Vec<StagedRange> {
    struct Merger {
        ranges: Vec<StagedRange>,
        dirty_start: Option<usize>,
        dirty_end: usize,
        has_staged: bool,
        has_unstaged: bool,
        staged_shift: isize,
        unstaged_shift: isize,
        dirty_staged_shift: isize,
        dirty_unstaged_shift: isize,
    }

    impl Merger {
        fn new() -> Self {
            Self {
                ranges: Vec::new(),
                dirty_start: None,
                dirty_end: 0,
                has_staged: false,
                has_unstaged: false,
                staged_shift: 0,
                unstaged_shift: 0,
                dirty_staged_shift: 0,
                dirty_unstaged_shift: 0,
            }
        }

        fn mark_dirty(&mut self, start: usize, end: usize) {
            match self.dirty_start {
                None => {
                    self.dirty_start = Some(start);
                    self.dirty_end = end;
                }
                Some(_) => {
                    self.dirty_end = self.dirty_end.max(end);
                }
            }
        }

        fn flush(&mut self, next_line: usize) {
            if let Some(dirty_start) = self.dirty_start
                && self.dirty_end < next_line
            {
                let local_start = (dirty_start as isize + self.unstaged_shift).max(0) as usize;
                let local_end =
                    (self.dirty_end as isize + self.unstaged_shift + self.dirty_unstaged_shift)
                        .max(0) as usize;
                let vcs_start = (dirty_start as isize + self.staged_shift).max(0) as usize;
                let vcs_end =
                    (self.dirty_end as isize + self.staged_shift + self.dirty_staged_shift).max(0)
                        as usize;
                self.ranges.push(StagedRange {
                    local_range: local_start..local_end,
                    staged_range: dirty_start..self.dirty_end,
                    vcs_range: vcs_start..vcs_end,
                    has_staged: self.has_staged,
                    has_unstaged: self.has_unstaged,
                });

                self.dirty_start = None;
                self.dirty_end = 0;
                self.has_staged = false;
                self.has_unstaged = false;

                self.staged_shift += self.dirty_staged_shift;
                self.unstaged_shift += self.dirty_unstaged_shift;
                self.dirty_staged_shift = 0;
                self.dirty_unstaged_shift = 0;
            }
        }

        fn handle_staged(&mut self, block: &DiffBlock) {
            self.flush(block.start2);
            self.dirty_staged_shift -= block.delta();
            self.mark_dirty(block.start2, block.end2);
            self.has_staged = true;
        }

        fn handle_unstaged(&mut self, block: &DiffBlock) {
            self.flush(block.start1);
            self.dirty_unstaged_shift += block.delta();
            self.mark_dirty(block.start1, block.end1);
            self.has_unstaged = true;
        }
    }

    let mut merger = Merger::new();
    let mut i = 0usize;
    let mut j = 0usize;
    while i < staged_blocks.len() || j < unstaged_blocks.len() {
        if j >= unstaged_blocks.len() {
            merger.handle_staged(&staged_blocks[i]);
            i += 1;
        } else if i >= staged_blocks.len() {
            merger.handle_unstaged(&unstaged_blocks[j]);
            j += 1;
        } else if staged_blocks[i].start2 <= unstaged_blocks[j].start1 {
            merger.handle_staged(&staged_blocks[i]);
            i += 1;
        } else {
            merger.handle_unstaged(&unstaged_blocks[j]);
            j += 1;
        }
    }
    merger.flush(usize::MAX);
    merger.ranges
}

/// Three-state gutter indicator for a diff row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineGutterMark {
    None,
    Unstaged(DiffLineKind),
    Staged(DiffLineKind),
    Both,
}

/// Typed line selection across a diff side (`before` vs `after` 0-based line indices).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LineSelection {
    pub staged_side: bool,
    pub before_lines: FxHashSet<usize>,
    pub after_lines: FxHashSet<usize>,
}

impl LineSelection {
    pub fn new(
        staged_side: bool,
        before_lines: FxHashSet<usize>,
        after_lines: FxHashSet<usize>,
    ) -> Self {
        Self {
            staged_side,
            before_lines,
            after_lines,
        }
    }

    pub fn from_iters(
        staged_side: bool,
        before: impl IntoIterator<Item = usize>,
        after: impl IntoIterator<Item = usize>,
    ) -> Self {
        Self {
            staged_side,
            before_lines: before.into_iter().collect(),
            after_lines: after.into_iter().collect(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.before_lines.is_empty() && self.after_lines.is_empty()
    }
}

/// Complete three-document tracker state for one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StageLineTracker {
    pub vcs_doc: SlicedDocument,
    pub staged_doc: SlicedDocument,
    pub local_doc: SlicedDocument,
    pub staged_blocks: Vec<DiffBlock>,
    pub unstaged_blocks: Vec<DiffBlock>,
    pub ranges: Vec<StagedRange>,
}

impl StageLineTracker {
    pub fn new(vcs_bytes: &[u8], staged_bytes: &[u8], local_bytes: &[u8]) -> Self {
        let vcs_doc = SlicedDocument::from_bytes(vcs_bytes);
        let staged_doc = SlicedDocument::from_bytes(staged_bytes);
        let local_doc = SlicedDocument::from_bytes(local_bytes);
        let staged_blocks = compute_diff_blocks(vcs_bytes, staged_bytes);
        let unstaged_blocks = compute_diff_blocks(staged_bytes, local_bytes);
        let ranges = merge_stage_blocks(&staged_blocks, &unstaged_blocks);
        Self {
            vcs_doc,
            staged_doc,
            local_doc,
            staged_blocks,
            unstaged_blocks,
            ranges,
        }
    }

    /// Finds the `StagedRange` index touching a `staged_doc` line index.
    pub fn range_for_staged_line(&self, staged_line: usize) -> Option<usize> {
        self.ranges.iter().position(|r| {
            if r.staged_range.is_empty() {
                r.staged_range.start == staged_line
            } else {
                r.staged_range.contains(&staged_line)
            }
        })
    }

    /// Finds the `StagedRange` index touching a `local_doc` line index.
    pub fn range_for_local_line(&self, local_line: usize) -> Option<usize> {
        self.ranges.iter().position(|r| {
            if r.local_range.is_empty() {
                r.local_range.start == local_line
            } else {
                r.local_range.contains(&local_line)
            }
        })
    }

    fn extend_selected(
        out: &mut Vec<SlicedLine>,
        lines: &[SlicedLine],
        range: Range<usize>,
        selected: &FxHashSet<usize>,
        keep_selected: bool,
    ) {
        if range.is_empty() {
            return;
        }
        out.extend(
            lines[range.clone()]
                .iter()
                .enumerate()
                .filter(|(offset, _)| selected.contains(&(range.start + offset)) == keep_selected)
                .map(|(_, line)| line.clone()),
        );
    }

    fn splice_blocks(
        base_doc: &SlicedDocument,
        other_doc: &SlicedDocument,
        blocks: &[DiffBlock],
        base_is_before: bool,
        sel_before: &FxHashSet<usize>,
        sel_after: &FxHashSet<usize>,
    ) -> Vec<u8> {
        let mut out = SlicedDocument::default();
        let mut cursor = 0usize;
        for block in blocks {
            if base_is_before {
                if block.start1 > cursor {
                    out.lines
                        .extend(base_doc.lines[cursor..block.start1].iter().cloned());
                }
                Self::extend_selected(
                    &mut out.lines,
                    &base_doc.lines,
                    block.start1..block.end1,
                    sel_before,
                    false,
                );
                Self::extend_selected(
                    &mut out.lines,
                    &other_doc.lines,
                    block.start2..block.end2,
                    sel_after,
                    true,
                );
                cursor = block.end1;
            } else {
                if block.start2 > cursor {
                    out.lines
                        .extend(base_doc.lines[cursor..block.start2].iter().cloned());
                }
                Self::extend_selected(
                    &mut out.lines,
                    &other_doc.lines,
                    block.start1..block.end1,
                    sel_before,
                    true,
                );
                Self::extend_selected(
                    &mut out.lines,
                    &base_doc.lines,
                    block.start2..block.end2,
                    sel_after,
                    false,
                );
                cursor = block.end2;
            }
        }
        if cursor < base_doc.lines.len() {
            out.lines.extend(base_doc.lines[cursor..].iter().cloned());
        }
        out.to_bytes()
    }

    /// Synthesizes a new Index buffer (`new_staged_bytes`) by staging only the
    /// selected `unstaged_blocks` lines (`before_lines` = `staged_doc` deletions,
    /// `after_lines` = `local_doc` additions).
    pub fn stage_lines(&self, sel: &LineSelection) -> Vec<u8> {
        Self::splice_blocks(
            &self.staged_doc,
            &self.local_doc,
            &self.unstaged_blocks,
            true,
            &sel.before_lines,
            &sel.after_lines,
        )
    }

    /// Synthesizes a new Index buffer (`new_staged_bytes`) by unstaging only the
    /// selected `staged_blocks` lines (`before_lines` = `vcs_doc` lines to restore,
    /// `after_lines` = `staged_doc` additions to remove).
    pub fn unstage_lines(&self, sel: &LineSelection) -> Vec<u8> {
        Self::splice_blocks(
            &self.staged_doc,
            &self.vcs_doc,
            &self.staged_blocks,
            false,
            &sel.before_lines,
            &sel.after_lines,
        )
    }

    /// Synthesizes a new Worktree buffer (`new_local_bytes`) by rolling back only the
    /// selected `unstaged_blocks` lines back to `staged_doc`
    /// (`before_lines` = `staged_doc` lines to restore, `after_lines` = `local_doc` lines to remove).
    pub fn rollback_lines(&self, sel: &LineSelection) -> Vec<u8> {
        Self::splice_blocks(
            &self.local_doc,
            &self.staged_doc,
            &self.unstaged_blocks,
            false,
            &sel.before_lines,
            &sel.after_lines,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn push_context_line(
        &self,
        staged_side: bool,
        doc_before: &SlicedDocument,
        cur_before: usize,
        cur_after: usize,
        lines: &mut Vec<DiffLine>,
        line_meta: &mut Vec<StageDiffLineMeta>,
        has_mixed: &mut bool,
    ) {
        lines.push(DiffLine {
            kind: DiffLineKind::Context,
            old_lineno: Some(cur_before as u32 + 1),
            new_lineno: Some(cur_after as u32 + 1),
            text: doc_before.lines[cur_before].display_text(),
            word_ranges: Vec::new(),
        });
        let pivot_line = if staged_side { cur_after } else { cur_before };
        let sr_idx = self.range_for_staged_line(pivot_line);
        let mark = sr_idx
            .and_then(|idx| self.ranges.get(idx))
            .map_or(LineGutterMark::None, |sr| {
                if sr.has_staged && sr.has_unstaged {
                    *has_mixed = true;
                    LineGutterMark::Both
                } else if sr.has_staged {
                    LineGutterMark::Staged(DiffLineKind::Context)
                } else {
                    LineGutterMark::None
                }
            });
        line_meta.push(StageDiffLineMeta {
            before_idx: Some(cur_before),
            after_idx: Some(cur_after),
            gutter_mark: mark,
            staged_range_idx: sr_idx,
        });
    }

    /// Builds interactive diff hunks with per-line gutter marks & `StagedRange` links.
    /// When `staged_side` is true, diffs `vcs_doc ↔ staged_doc`; otherwise `staged_doc ↔ local_doc`.
    pub fn build_interactive_hunks(
        &self,
        staged_side: bool,
        context: DiffContext,
        max_lines: usize,
    ) -> (Vec<StageDiffHunk>, bool) {
        let (doc_before, doc_after, blocks) = if staged_side {
            (&self.vcs_doc, &self.staged_doc, &self.staged_blocks)
        } else {
            (&self.staged_doc, &self.local_doc, &self.unstaged_blocks)
        };
        if blocks.is_empty() {
            return (Vec::new(), false);
        }

        let context_len = context.lines() as usize;
        let before_len = doc_before.len();
        let mut out = Vec::new();
        let mut budget = max_lines;
        let mut truncated = false;

        let mut i = 0usize;
        while i < blocks.len() {
            let first = &blocks[i];
            let mut group_end = i + 1;
            while group_end < blocks.len() {
                let prev_end = blocks[group_end - 1].end1;
                let next_start = blocks[group_end].start1;
                if next_start.saturating_sub(prev_end) <= context_len.saturating_mul(2) {
                    group_end += 1;
                } else {
                    break;
                }
            }

            let start_before = first.start1.saturating_sub(context_len);
            let leading = first.start1 - start_before;
            let start_after = first.start2.saturating_sub(leading);

            let mut lines = Vec::new();
            let mut line_meta = Vec::new();
            let mut cur_before = start_before;
            let mut cur_after = start_after;
            let mut has_mixed = false;

            for b in &blocks[i..group_end] {
                while cur_before < b.start1 {
                    if budget == 0 {
                        truncated = true;
                        break;
                    }
                    budget -= 1;
                    self.push_context_line(
                        staged_side,
                        doc_before,
                        cur_before,
                        cur_after,
                        &mut lines,
                        &mut line_meta,
                        &mut has_mixed,
                    );
                    cur_before += 1;
                    cur_after += 1;
                }
                if truncated {
                    break;
                }
                for (b_idx, line) in doc_before
                    .lines
                    .iter()
                    .enumerate()
                    .take(b.end1)
                    .skip(b.start1)
                {
                    if budget == 0 {
                        truncated = true;
                        break;
                    }
                    budget -= 1;
                    lines.push(DiffLine {
                        kind: DiffLineKind::Deleted,
                        old_lineno: Some(b_idx as u32 + 1),
                        new_lineno: None,
                        text: line.display_text(),
                        word_ranges: Vec::new(),
                    });
                    let sr_idx = if staged_side {
                        self.range_for_staged_line(b.start2)
                    } else {
                        self.range_for_staged_line(b_idx)
                    };
                    let mark = sr_idx
                        .and_then(|idx| self.ranges.get(idx))
                        .map_or(
                            if staged_side {
                                LineGutterMark::Staged(DiffLineKind::Deleted)
                            } else {
                                LineGutterMark::Unstaged(DiffLineKind::Deleted)
                            },
                            |sr| {
                                if sr.has_staged && sr.has_unstaged {
                                    has_mixed = true;
                                    LineGutterMark::Both
                                } else if staged_side {
                                    LineGutterMark::Staged(DiffLineKind::Deleted)
                                } else {
                                    LineGutterMark::Unstaged(DiffLineKind::Deleted)
                                }
                            },
                        );
                    line_meta.push(StageDiffLineMeta {
                        before_idx: Some(b_idx),
                        after_idx: None,
                        gutter_mark: mark,
                        staged_range_idx: sr_idx,
                    });
                }
                for (a_idx, line) in doc_after
                    .lines
                    .iter()
                    .enumerate()
                    .take(b.end2)
                    .skip(b.start2)
                {
                    if budget == 0 {
                        truncated = true;
                        break;
                    }
                    budget -= 1;
                    lines.push(DiffLine {
                        kind: DiffLineKind::Added,
                        old_lineno: None,
                        new_lineno: Some(a_idx as u32 + 1),
                        text: line.display_text(),
                        word_ranges: Vec::new(),
                    });
                    let sr_idx = if staged_side {
                        self.range_for_staged_line(a_idx)
                    } else {
                        self.range_for_local_line(a_idx)
                    };
                    let is_pair_mod = b.end1 > b.start1;
                    let mark_kind = if is_pair_mod {
                        DiffLineKind::Context // rendered as modified blue in gutter
                    } else {
                        DiffLineKind::Added
                    };
                    let mark = sr_idx
                        .and_then(|idx| self.ranges.get(idx))
                        .map_or(
                            if staged_side {
                                LineGutterMark::Staged(mark_kind)
                            } else {
                                LineGutterMark::Unstaged(mark_kind)
                            },
                            |sr| {
                                if sr.has_staged && sr.has_unstaged {
                                    has_mixed = true;
                                    LineGutterMark::Both
                                } else if staged_side {
                                    LineGutterMark::Staged(mark_kind)
                                } else {
                                    LineGutterMark::Unstaged(mark_kind)
                                }
                            },
                        );
                    line_meta.push(StageDiffLineMeta {
                        before_idx: None,
                        after_idx: Some(a_idx),
                        gutter_mark: mark,
                        staged_range_idx: sr_idx,
                    });
                }
                if truncated {
                    break;
                }
                cur_before = b.end1;
                cur_after = b.end2;
            }

            if !truncated {
                let tail_end = cur_before.saturating_add(context_len).min(before_len);
                while cur_before < tail_end {
                    if budget == 0 {
                        truncated = true;
                        break;
                    }
                    budget -= 1;
                    self.push_context_line(
                        staged_side,
                        doc_before,
                        cur_before,
                        cur_after,
                        &mut lines,
                        &mut line_meta,
                        &mut has_mixed,
                    );
                    cur_before += 1;
                    cur_after += 1;
                }
            }

            let len_before = cur_before - start_before;
            let len_after = cur_after - start_after;
            let mut hunk = DiffHunk {
                header: format!(
                    "@@ -{},{} +{},{} @@",
                    start_before + 1,
                    len_before,
                    start_after + 1,
                    len_after
                ),
                lines,
            };
            apply_word_diff(&mut hunk);
            out.push(StageDiffHunk {
                hunk,
                block_indices: (i..group_end).collect(),
                line_meta,
                has_mixed_range: has_mixed,
            });
            if truncated {
                break;
            }
            i = group_end;
        }

        (out, truncated)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StageDiffLineMeta {
    pub before_idx: Option<usize>,
    pub after_idx: Option<usize>,
    pub gutter_mark: LineGutterMark,
    pub staged_range_idx: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StageDiffHunk {
    pub hunk: DiffHunk,
    pub block_indices: Vec<usize>,
    pub line_meta: Vec<StageDiffLineMeta>,
    pub has_mixed_range: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn sliced_document_roundtrips_all_edge_cases() {
        let cases: &[&[u8]] = &[
            b"",
            b"\n",
            b"\r\n",
            b"no_newline",
            b"a\nb\nc",
            b"a\r\nb\nc\r\nd",
            b"mixed\r\nlines\nwithout_eof",
            b"\x00\xff\n\r\n",
        ];
        for &raw in cases {
            let doc = SlicedDocument::from_bytes(raw);
            assert_eq!(doc.to_bytes(), raw, "failed roundtrip for {raw:?}");
        }
    }

    #[test]
    fn block_merger_fuses_adjacent_staged_and_unstaged_ranges() {
        // HEAD: line1, line2, line3, line4
        // Staged: line1, line2_staged, line3, line4
        // Local: line1, line2_local, line3, line4_local
        let vcs = b"line1\nline2\nline3\nline4\n";
        let staged = b"line1\nline2_staged\nline3\nline4\n";
        let local = b"line1\nline2_local\nline3\nline4_local\n";

        let tracker = StageLineTracker::new(vcs, staged, local);
        assert_eq!(tracker.ranges.len(), 2);

        // Range 0 touches line2 (both staged and unstaged)
        assert!(tracker.ranges[0].has_staged);
        assert!(tracker.ranges[0].has_unstaged);
        assert_eq!(tracker.ranges[0].vcs_range, 1..2);
        assert_eq!(tracker.ranges[0].staged_range, 1..2);
        assert_eq!(tracker.ranges[0].local_range, 1..2);

        // Range 1 touches line4 (unstaged only)
        assert!(!tracker.ranges[1].has_staged);
        assert!(tracker.ranges[1].has_unstaged);
        assert_eq!(tracker.ranges[1].vcs_range, 3..4);
        assert_eq!(tracker.ranges[1].staged_range, 3..4);
        assert_eq!(tracker.ranges[1].local_range, 3..4);
    }

    #[test]
    fn partial_line_staging_and_no_eof_newline_preservation() {
        let vcs = b"a\nb\nc";
        let staged = b"a\nb\nc";
        let local = b"a\nb1\nb2\nb3\nc";

        let tracker = StageLineTracker::new(vcs, staged, local);
        assert_eq!(tracker.unstaged_blocks.len(), 1);

        // Stage ONLY b1 (local line index 1) and b3 (local line index 3),
        // without staging the deletion of b (staged line index 1) or b2!
        let mut add = FxHashSet::default();
        add.insert(1usize);
        add.insert(3usize);
        let new_staged = tracker.stage_lines(&LineSelection::new(false, FxHashSet::default(), add));
        assert_eq!(new_staged, b"a\nb\nb1\nb3\nc");
        // Notice: trailing 'c' still has NO newline!
        assert!(!new_staged.ends_with(b"\n"));
    }

    #[test]
    fn partial_unstage_and_rollback_preserve_other_lines() {
        let vcs = b"keep1\norig\nkeep2\n";
        let staged = b"keep1\nstaged_a\nstaged_b\nkeep2\n";
        let local = b"keep1\nstaged_a\nstaged_b\nlocal_extra\nkeep2\n";

        let tracker = StageLineTracker::new(vcs, staged, local);

        // Unstage only staged_b (staged_doc idx 2) and don't restore orig
        let mut add_staged = FxHashSet::default();
        add_staged.insert(2usize);
        let after_unstage =
            tracker.unstage_lines(&LineSelection::new(true, FxHashSet::default(), add_staged));
        assert_eq!(after_unstage, b"keep1\nstaged_a\nkeep2\n");

        // Rollback local_extra (local_doc idx 3) -> local equals staged
        let mut add_local = FxHashSet::default();
        add_local.insert(3usize);
        let after_rollback =
            tracker.rollback_lines(&LineSelection::new(false, FxHashSet::default(), add_local));
        assert_eq!(after_rollback, staged);
    }

    fn select_all_blocks(blocks: &[DiffBlock], staged_side: bool) -> LineSelection {
        let mut del = FxHashSet::default();
        let mut add = FxHashSet::default();
        for b in blocks {
            del.extend(b.start1..b.end1);
            add.extend(b.start2..b.end2);
        }
        LineSelection::new(staged_side, del, add)
    }

    fn doc_strategy() -> impl Strategy<Value = Vec<u8>> {
        let line_strat = prop::sample::select(vec![
            "alpha", "beta", "gamma", "delta", "epsilon", "zeta", "eta", "theta", "iota",
            "kappa", "lambda", "mu",
        ]);
        let ending_strat = prop::sample::select(vec!["\n", "\r\n"]);
        (
            prop::collection::vec((line_strat.clone(), ending_strat), 0..24),
            prop::option::of(line_strat),
        )
            .prop_map(|(lines, tail)| {
                let mut out = Vec::new();
                for (l, e) in lines {
                    out.extend_from_slice(l.as_bytes());
                    out.extend_from_slice(e.as_bytes());
                }
                if let Some(t) = tail {
                    out.extend_from_slice(t.as_bytes());
                }
                out
            })
    }

    proptest! {
        #[test]
        fn proptest_sliced_doc_lossless_and_partial_stage_unstage_invariants(
            vcs in doc_strategy(),
            staged in doc_strategy(),
            local in doc_strategy(),
            pick_seed in any::<u64>(),
        ) {
            // 1. Lossless byte roundtrip
            prop_assert_eq!(SlicedDocument::from_bytes(&vcs).to_bytes(), vcs.clone());
            prop_assert_eq!(SlicedDocument::from_bytes(&staged).to_bytes(), staged.clone());
            prop_assert_eq!(SlicedDocument::from_bytes(&local).to_bytes(), local.clone());

            let tracker = StageLineTracker::new(&vcs, &staged, &local);

            // 2. Partial stage a random subset of unstaged lines, then stage the remaining lines -> must equal local!
            let mut del_sub = FxHashSet::default();
            let mut add_sub = FxHashSet::default();
            let mut bit = 0u32;
            for b in &tracker.unstaged_blocks {
                for s_idx in b.start1..b.end1 {
                    if ((pick_seed >> (bit % 64)) & 1) == 1 {
                        del_sub.insert(s_idx);
                    }
                    bit += 1;
                }
                for l_idx in b.start2..b.end2 {
                    if ((pick_seed >> (bit % 64)) & 1) == 1 {
                        add_sub.insert(l_idx);
                    }
                    bit += 1;
                }
            }
            let mid_staged = tracker.stage_lines(&LineSelection::new(false, del_sub, add_sub));
            let tracker2 = StageLineTracker::new(&vcs, &mid_staged, &local);
            let final_staged = tracker2.stage_lines(&select_all_blocks(&tracker2.unstaged_blocks, false));
            prop_assert_eq!(final_staged, local.clone());

            // 3. Partial unstage a subset, then unstage all remaining -> must equal vcs!
            let mut vcs_del_sub = FxHashSet::default();
            let mut st_add_sub = FxHashSet::default();
            for b in &tracker2.staged_blocks {
                for v_idx in b.start1..b.end1 {
                    if ((pick_seed >> (bit % 64)) & 1) == 1 {
                        vcs_del_sub.insert(v_idx);
                    }
                    bit += 1;
                }
                for s_idx in b.start2..b.end2 {
                    if ((pick_seed >> (bit % 64)) & 1) == 1 {
                        st_add_sub.insert(s_idx);
                    }
                    bit += 1;
                }
            }
            let mid_unstaged = tracker2.unstage_lines(&LineSelection::new(true, vcs_del_sub, st_add_sub));
            let tracker3 = StageLineTracker::new(&vcs, &mid_unstaged, &local);
            let final_unstaged = tracker3.unstage_lines(&select_all_blocks(&tracker3.staged_blocks, true));
            prop_assert_eq!(final_unstaged, vcs.clone());

            // 4. Full rollback restores local to staged
            let rolled_back = tracker.rollback_lines(&select_all_blocks(&tracker.unstaged_blocks, false));
            prop_assert_eq!(rolled_back, staged);
        }
    }
}
