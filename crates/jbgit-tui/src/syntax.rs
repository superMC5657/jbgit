//! Darcula syntax highlighting for `FrameDiffPreview` via `syntect`.

use std::cell::RefCell;
use std::io::Cursor;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use jbgit_core::{DiffHunk, DiffLine, DiffLineKind, FileDiff, MAX_DIFF_FILE_BYTES};
use jbgit_engine::ThreeDocStageDiff;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;
use rustc_hash::FxHashMap;
use syntect::easy::HighlightLines;
use syntect::highlighting::{FontStyle, Theme, ThemeSet};
use syntect::parsing::SyntaxSet;
use unicode_width::UnicodeWidthChar;

use crate::theme::to_color;

/// Lines longer than 2 KiB skip `syntect` tokenization to avoid `fancy-regex` backtracking stalls
/// on minified JS/JSON/SVG bundles.
pub const MAX_SYNTAX_LINE_BYTES: usize = 2048;

const CHECK_CANCEL_EVERY_LINES: usize = 64;

const DARCULA_TM_THEME: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>name</key><string>Darcula</string>
  <key>settings</key>
  <array>
    <dict><key>settings</key><dict>
      <key>background</key><string>#1E1F22</string>
      <key>foreground</key><string>#BCBEC4</string>
    </dict></dict>
    <dict><key>scope</key><string>comment, punctuation.definition.comment</string>
      <key>settings</key><dict><key>foreground</key><string>#7A7E85</string><key>fontStyle</key><string>italic</string></dict></dict>
    <dict><key>scope</key><string>string, constant.other.symbol</string>
      <key>settings</key><dict><key>foreground</key><string>#6AAB73</string></dict></dict>
    <dict><key>scope</key><string>constant.numeric, constant.language, constant.character</string>
      <key>settings</key><dict><key>foreground</key><string>#2AACB8</string></dict></dict>
    <dict><key>scope</key><string>keyword, storage.type, storage.modifier</string>
      <key>settings</key><dict><key>foreground</key><string>#CF8E6D</string></dict></dict>
    <dict><key>scope</key><string>entity.name.function, support.function, meta.function-call</string>
      <key>settings</key><dict><key>foreground</key><string>#56A8F5</string></dict></dict>
    <dict><key>scope</key><string>entity.name.type, entity.name.struct, entity.name.enum, entity.name.trait, support.type</string>
      <key>settings</key><dict><key>foreground</key><string>#B3AE60</string></dict></dict>
    <dict><key>scope</key><string>variable.parameter, variable.other.member, support.constant</string>
      <key>settings</key><dict><key>foreground</key><string>#C77DBB</string></dict></dict>
  </array>
</dict>
</plist>"#;

struct SyntaxAssets {
    ss: SyntaxSet,
    theme: Theme,
}

static ASSETS: OnceLock<SyntaxAssets> = OnceLock::new();

fn assets() -> &'static SyntaxAssets {
    ASSETS.get_or_init(|| {
        let ss = SyntaxSet::load_defaults_newlines();
        let theme = ThemeSet::load_from_reader(&mut Cursor::new(DARCULA_TM_THEME))
            .unwrap_or_else(|_| {
                ThemeSet::load_defaults()
                    .themes
                    .into_values()
                    .next()
                    .unwrap_or_default()
            });
        SyntaxAssets { ss, theme }
    })
}

/// Pre-warms bundled syntax grammars & Darcula theme off the UI thread at startup.
pub fn warmup_syntax_assets() {
    let _ = assets();
}

fn find_syntax<'a>(assets: &'a SyntaxAssets, path: &str) -> &'a syntect::parsing::SyntaxReference {
    let ext = path.rsplit('.').next().unwrap_or("");
    assets
        .ss
        .find_syntax_by_extension(ext)
        .or_else(|| {
            let name = path.rsplit('/').next().unwrap_or(path);
            assets.ss.find_syntax_by_name(name)
        })
        .unwrap_or_else(|| assets.ss.find_syntax_plain_text())
}

/// One highlighted token within a diff line: byte range + optional fg + font style.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyntaxToken {
    pub start: usize,
    pub end: usize,
    pub fg: (u8, u8, u8),
    pub bold: bool,
    pub italic: bool,
}

/// Pre-highlighted tokens for every line of every hunk in a [`FileDiff`].
#[derive(Debug, Clone, Default)]
pub struct HighlightedDiff {
    pub hunks: Vec<Vec<Vec<SyntaxToken>>>,
}

impl HighlightedDiff {
    #[cfg(test)]
    pub fn build(diff: &FileDiff) -> Self {
        Self::build_cancellable_file(diff, &AtomicBool::new(false)).unwrap_or_default()
    }

    pub fn build_cancellable_file(diff: &FileDiff, cancel: &AtomicBool) -> Option<Self> {
        Self::build_cancellable_hunks(&diff.key.path, diff.hunks.iter(), diff.hunks.len(), cancel)
    }

    pub fn build_cancellable_stage(
        diff: &ThreeDocStageDiff,
        cancel: &AtomicBool,
    ) -> Option<Self> {
        Self::build_cancellable_hunks(
            &diff.key.path,
            diff.hunks.iter().map(|h| &h.hunk),
            diff.hunks.len(),
            cancel,
        )
    }

    pub fn build_cancellable_hunks<'a>(
        path: &str,
        hunks_iter: impl IntoIterator<Item = &'a DiffHunk>,
        hunk_count: usize,
        cancel: &AtomicBool,
    ) -> Option<Self> {
        if cancel.load(Ordering::Relaxed) {
            return None;
        }
        let assets = assets();
        let syntax = find_syntax(assets, path);

        let mut hunks = Vec::with_capacity(hunk_count);
        let mut line_buf = String::with_capacity(256);

        for hunk in hunks_iter {
            if cancel.load(Ordering::Relaxed) {
                return None;
            }
            // Only allocate/advance `hl_old` when the hunk contains deletions, and stop
            // advancing it on Context lines past the last deletion in that hunk.
            let last_deleted_idx = hunk
                .lines
                .iter()
                .rposition(|l| l.kind == DiffLineKind::Deleted);
            let mut hl_old = last_deleted_idx.map(|_| HighlightLines::new(syntax, &assets.theme));
            let mut hl_new = HighlightLines::new(syntax, &assets.theme);
            let mut line_tokens = Vec::with_capacity(hunk.lines.len());

            for (line_idx, line) in hunk.lines.iter().enumerate() {
                if line_idx % CHECK_CANCEL_EVERY_LINES == 0 && cancel.load(Ordering::Relaxed) {
                    return None;
                }
                if line.text.len() > MAX_SYNTAX_LINE_BYTES {
                    line_tokens.push(Vec::new());
                    continue;
                }

                line_buf.clear();
                line_buf.push_str(&line.text);
                line_buf.push('\n');

                let ranges = match line.kind {
                    DiffLineKind::Deleted => hl_old
                        .as_mut()
                        .and_then(|hl| hl.highlight_line(&line_buf, &assets.ss).ok())
                        .unwrap_or_default(),
                    DiffLineKind::Added => hl_new
                        .highlight_line(&line_buf, &assets.ss)
                        .unwrap_or_default(),
                    DiffLineKind::Context => {
                        if let Some(last_del) = last_deleted_idx
                            && line_idx < last_del
                            && let Some(hl) = hl_old.as_mut()
                        {
                            let _ = hl.highlight_line(&line_buf, &assets.ss);
                        }
                        hl_new
                            .highlight_line(&line_buf, &assets.ss)
                            .unwrap_or_default()
                    }
                };

                let max_len = line.text.len();
                let mut pos = 0usize;
                let mut toks = Vec::with_capacity(ranges.len());
                for (style, text) in ranges {
                    let end = (pos + text.len()).min(max_len);
                    if end > pos {
                        toks.push(SyntaxToken {
                            start: pos,
                            end,
                            fg: (style.foreground.r, style.foreground.g, style.foreground.b),
                            bold: style.font_style.contains(FontStyle::BOLD),
                            italic: style.font_style.contains(FontStyle::ITALIC),
                        });
                    }
                    pos = end;
                    if pos >= max_len {
                        break;
                    }
                }
                line_tokens.push(toks);
            }
            hunks.push(line_tokens);
        }
        Some(Self { hunks })
    }
}

/// Pre-highlighted tokens for every line of an annotated source file.
#[derive(Debug, Clone, Default)]
pub struct HighlightedFile {
    pub lines: Vec<Vec<SyntaxToken>>,
}

impl HighlightedFile {
    pub fn build_cancellable(
        path: &str,
        content: &str,
        line_count: usize,
        cancel: &AtomicBool,
    ) -> Option<Self> {
        if cancel.load(Ordering::Relaxed) {
            return None;
        }
        if line_count == 0 {
            return Some(Self { lines: Vec::new() });
        }
        if content.len() > MAX_DIFF_FILE_BYTES {
            return Some(Self {
                lines: vec![Vec::new(); line_count],
            });
        }
        let assets = assets();
        let syntax = find_syntax(assets, path);
        let mut hl = HighlightLines::new(syntax, &assets.theme);
        let mut lines = Vec::with_capacity(line_count);
        let mut line_buf = String::with_capacity(256);

        for (idx, line) in content.split('\n').enumerate() {
            if idx % CHECK_CANCEL_EVERY_LINES == 0 && cancel.load(Ordering::Relaxed) {
                return None;
            }
            if line.len() > MAX_SYNTAX_LINE_BYTES {
                lines.push(Vec::new());
                continue;
            }
            line_buf.clear();
            line_buf.push_str(line);
            line_buf.push('\n');

            let ranges = hl.highlight_line(&line_buf, &assets.ss).unwrap_or_default();
            let max_len = line.len();
            let mut pos = 0usize;
            let mut toks = Vec::with_capacity(ranges.len());
            for (style, text) in ranges {
                let end = (pos + text.len()).min(max_len);
                if end > pos {
                    toks.push(SyntaxToken {
                        start: pos,
                        end,
                        fg: (style.foreground.r, style.foreground.g, style.foreground.b),
                        bold: style.font_style.contains(FontStyle::BOLD),
                        italic: style.font_style.contains(FontStyle::ITALIC),
                    });
                }
                pos = end;
                if pos >= max_len {
                    break;
                }
            }
            lines.push(toks);
        }
        Some(Self { lines })
    }
}

/// Combines syntax foreground tokens with word-diff background ranges and truncates
/// cleanly to `max_width` display columns.
#[cfg(test)]
pub fn render_diff_line_spans(
    text: &str,
    syntax_tokens: &[SyntaxToken],
    word_ranges: &[(usize, usize)],
    max_width: usize,
    base: Style,
    word_bg: Color,
    true_color: bool,
) -> Vec<Span<'static>> {
    render_diff_line_spans_with_tab(
        text,
        syntax_tokens,
        word_ranges,
        max_width,
        4,
        base,
        word_bg,
        true_color,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn render_diff_line_spans_with_tab(
    text: &str,
    syntax_tokens: &[SyntaxToken],
    word_ranges: &[(usize, usize)],
    max_width: usize,
    tab_size: usize,
    base: Style,
    word_bg: Color,
    true_color: bool,
) -> Vec<Span<'static>> {
    if max_width == 0 {
        return Vec::new();
    }
    let tab_w = tab_size.max(1);
    let char_display_w = |ch: char| -> usize {
        if ch == '\t' {
            tab_w
        } else {
            ch.width().unwrap_or(0)
        }
    };
    let total_w: usize = text.chars().map(char_display_w).sum();

    // Truncate to fit `max_width` (reserving 1 column for '…' when needed).
    let (limit, ellipsis) = if total_w <= max_width {
        (text.len(), false)
    } else if max_width == 1 {
        return vec![Span::styled("…", base)];
    } else {
        let target = max_width - 1;
        let mut used = 0usize;
        let mut end = 0usize;
        for (idx, ch) in text.char_indices() {
            let cw = char_display_w(ch);
            if used + cw > target {
                break;
            }
            used += cw;
            end = idx + ch.len_utf8();
        }
        (end, true)
    };

    if limit == 0 {
        return if ellipsis {
            vec![Span::styled("…", base)]
        } else {
            Vec::new()
        };
    }

    // Two-cursor sweep over sorted `syntax_tokens` and `word_ranges` in O(tokens + word_ranges).
    let tab_spaces = " ".repeat(tab_w);
    let mut spans = Vec::with_capacity(syntax_tokens.len() + word_ranges.len() + 2);
    let mut pos = 0usize;
    let mut ti = 0usize;
    let mut wi = 0usize;

    while pos < limit {
        while let Some(t) = syntax_tokens.get(ti) {
            if t.end <= pos || t.start >= t.end || t.start >= limit {
                ti += 1;
            } else {
                break;
            }
        }
        while let Some(&(ws, we)) = word_ranges.get(wi) {
            if we <= pos || ws >= we || ws >= limit {
                wi += 1;
            } else {
                break;
            }
        }

        let active_tok = syntax_tokens
            .get(ti)
            .filter(|t| t.start <= pos && pos < t.end);
        let active_wr = word_ranges
            .get(wi)
            .filter(|&&(ws, we)| ws <= pos && pos < we);

        let mut next = limit;
        if let Some(t) = active_tok {
            next = next.min(t.end.min(limit));
        } else if let Some(t) = syntax_tokens.get(ti)
            && t.start > pos
        {
            next = next.min(t.start);
        }
        if let Some(&(_, we)) = active_wr {
            next = next.min(we.min(limit));
        } else if let Some(&(ws, _)) = word_ranges.get(wi)
            && ws > pos
        {
            next = next.min(ws);
        }

        while next < limit && !text.is_char_boundary(next) {
            next += 1;
        }
        if next <= pos {
            break;
        }

        let mut style = base;
        if let Some(t) = active_tok {
            style = style.fg(to_color(t.fg, true_color));
            if t.bold {
                style = style.add_modifier(Modifier::BOLD);
            }
            if t.italic {
                style = style.add_modifier(Modifier::ITALIC);
            }
        }
        if active_wr.is_some() {
            style = style.bg(word_bg).add_modifier(Modifier::BOLD);
        }

        let raw_slice = &text[pos..next];
        let rendered = if raw_slice.contains('\t') {
            raw_slice.replace('\t', &tab_spaces)
        } else {
            raw_slice.to_string()
        };
        spans.push(Span::styled(rendered, style));
        pos = next;
    }

    if ellipsis {
        spans.push(Span::styled("…", base));
    }
    spans
}

const MAX_LAZY_WORD_DIFF_ENTRIES: usize = 8_192;

pub type SharedWordRanges = Arc<[(usize, usize)]>;

/// Finds the paired `(deleted_idx, added_idx)` for `line_idx` within a single hunk,
/// matching `jbgit_core::side_by_side_rows` pairing semantics.
pub fn paired_line_in_hunk(lines: &[DiffLine], line_idx: usize) -> Option<(usize, usize)> {
    let line = lines.get(line_idx)?;
    match line.kind {
        DiffLineKind::Deleted => {
            let mut del_start = line_idx;
            while del_start > 0 && lines[del_start - 1].kind == DiffLineKind::Deleted {
                del_start -= 1;
            }
            let offset = line_idx - del_start;
            let mut add_start = line_idx + 1;
            while add_start < lines.len() && lines[add_start].kind == DiffLineKind::Deleted {
                add_start += 1;
            }
            let right_idx = add_start + offset;
            if lines.get(right_idx).is_some_and(|r| r.kind == DiffLineKind::Added) {
                Some((line_idx, right_idx))
            } else {
                None
            }
        }
        DiffLineKind::Added => {
            let mut add_start = line_idx;
            while add_start > 0 && lines[add_start - 1].kind == DiffLineKind::Added {
                add_start -= 1;
            }
            let offset = line_idx - add_start;
            let mut del_start = add_start;
            while del_start > 0 && lines[del_start - 1].kind == DiffLineKind::Deleted {
                del_start -= 1;
            }
            let left_idx = del_start + offset;
            if left_idx < add_start {
                Some((left_idx, line_idx))
            } else {
                None
            }
        }
        DiffLineKind::Context => None,
    }
}

#[derive(Debug)]
struct CachedLazyWordDiffs<D> {
    diff: Arc<D>,
    ranges: FxHashMap<(usize, usize), SharedWordRanges>,
}

/// Per-diff lazy inline word-diff cache shared across Unified and Split views.
#[derive(Debug)]
pub struct LazyWordDiffCache<D> {
    inner: RefCell<Option<CachedLazyWordDiffs<D>>>,
}

impl<D> Default for LazyWordDiffCache<D> {
    fn default() -> Self {
        Self {
            inner: RefCell::new(None),
        }
    }
}

impl<D> Clone for LazyWordDiffCache<D> {
    fn clone(&self) -> Self {
        Self::default()
    }
}

impl<D> LazyWordDiffCache<D> {
    fn resolve_pair(
        &self,
        diff: &Arc<D>,
        hunk_idx: usize,
        left_idx: usize,
        right_idx: usize,
        lines: &[DiffLine],
    ) -> (SharedWordRanges, SharedWordRanges) {
        let mut guard = self.inner.borrow_mut();
        let needs_reset = guard
            .as_ref()
            .is_none_or(|cached| !Arc::ptr_eq(&cached.diff, diff));
        if needs_reset {
            *guard = Some(CachedLazyWordDiffs {
                diff: Arc::clone(diff),
                ranges: FxHashMap::default(),
            });
        }
        let cached = guard.as_mut().expect("initialized above");
        if let (Some(lw), Some(rw)) = (
            cached.ranges.get(&(hunk_idx, left_idx)),
            cached.ranges.get(&(hunk_idx, right_idx)),
        ) {
            return (Arc::clone(lw), Arc::clone(rw));
        }

        let l = &lines[left_idx];
        let r = &lines[right_idx];
        let (lw, rw): (SharedWordRanges, SharedWordRanges) = if l.word_ranges.is_empty()
            && r.word_ranges.is_empty()
            && l.text != r.text
            && l.text.len() <= jbgit_core::MAX_WORD_DIFF_LINE_BYTES
            && r.text.len() <= jbgit_core::MAX_WORD_DIFF_LINE_BYTES
        {
            let (a, b) = jbgit_core::word_diff(&l.text, &r.text);
            (a.into(), b.into())
        } else {
            (Arc::from([]), Arc::from([]))
        };

        if cached.ranges.len() >= MAX_LAZY_WORD_DIFF_ENTRIES {
            cached.ranges.clear();
        }
        cached
            .ranges
            .insert((hunk_idx, left_idx), Arc::clone(&lw));
        cached
            .ranges
            .insert((hunk_idx, right_idx), Arc::clone(&rw));
        (lw, rw)
    }

    /// Returns cached lazy word ranges for a Unified row when eager `word_ranges` is empty.
    pub fn unified_ranges(
        &self,
        diff: &Arc<D>,
        hunk_idx: usize,
        line_idx: usize,
        lines: &[DiffLine],
    ) -> Option<SharedWordRanges> {
        let line = lines.get(line_idx)?;
        if !line.word_ranges.is_empty() {
            return None;
        }
        let (li, ri) = paired_line_in_hunk(lines, line_idx)?;
        if !lines[li].word_ranges.is_empty() || !lines[ri].word_ranges.is_empty() {
            return None;
        }
        let (lw, rw) = self.resolve_pair(diff, hunk_idx, li, ri, lines);
        Some(if line_idx == li { lw } else { rw })
    }

    /// Returns cached lazy word ranges for a Split row pair when eager `word_ranges` are empty.
    pub fn split_ranges(
        &self,
        diff: &Arc<D>,
        hunk_idx: usize,
        left: Option<usize>,
        right: Option<usize>,
        lines: &[DiffLine],
    ) -> (Option<SharedWordRanges>, Option<SharedWordRanges>) {
        let (Some(li), Some(ri)) = (left, right) else {
            return (None, None);
        };
        if li == ri {
            return (None, None);
        }
        let (Some(l), Some(r)) = (lines.get(li), lines.get(ri)) else {
            return (None, None);
        };
        if !l.word_ranges.is_empty() || !r.word_ranges.is_empty() {
            return (None, None);
        }
        let (lw, rw) = self.resolve_pair(diff, hunk_idx, li, ri, lines);
        (Some(lw), Some(rw))
    }

    #[cfg(test)]
    pub(crate) fn cached_entry_count(&self) -> usize {
        self.inner.borrow().as_ref().map_or(0, |c| c.ranges.len())
    }
}

#[cfg(test)]
mod tests {
    use jbgit_core::{DetailsKey, DiffContext, DiffHunk, DiffLine, FileDiffKey, Oid};

    use super::*;

    #[test]
    fn darcula_syntect_highlights_rust_keywords_and_preserves_word_bg() {
        let diff = FileDiff {
            key: FileDiffKey {
                details: DetailsKey::single(Oid::ZERO),
                path: "src/lib.rs".into(),
                old_path: None,
                context: DiffContext::Three,
            },
            hunks: vec![DiffHunk {
                header: "@@ -1,1 +1,1 @@".into(),
                lines: vec![DiffLine {
                    kind: DiffLineKind::Added,
                    old_lineno: None,
                    new_lineno: Some(1),
                    text: "fn call(omega: &str) {}".into(),
                    word_ranges: vec![(8, 13)],
                }],
            }],
            is_binary: false,
            is_truncated: false,
            is_too_large: false,
        };
        let hl = HighlightedDiff::build(&diff);
        let toks = &hl.hunks[0][0];
        let fn_tok = toks.iter().find(|t| t.start == 0).unwrap();
        // `fn` gets the Darcula keyword color (#CF8E6D).
        assert_eq!(fn_tok.fg, (0xCF, 0x8E, 0x6D));

        let spans = render_diff_line_spans(
            &diff.hunks[0].lines[0].text,
            toks,
            &diff.hunks[0].lines[0].word_ranges,
            80,
            Style::default().bg(Color::Rgb(41, 68, 54)),
            Color::Rgb(55, 102, 75),
            true,
        );
        let omega = spans.iter().find(|s| s.content == "omega").unwrap();
        assert_eq!(omega.style.bg, Some(Color::Rgb(55, 102, 75)));
    }

    #[test]
    fn tab_size_expands_tabs_in_diff_spans() {
        let before = b"fn main() {\n\tlet val = alpha;\n}\n";
        let after = b"fn main() {\n\tlet val = omega;\n}\n";
        let (hunks, _) =
            jbgit_core::build_diff_hunks(before, after, 3, jbgit_core::MAX_DIFF_LINES, || false)
                .expect("diff hunks");
        let diff = FileDiff {
            key: FileDiffKey {
                details: DetailsKey::single(Oid::ZERO),
                path: "src/main.rs".into(),
                old_path: None,
                context: DiffContext::Three,
            },
            hunks,
            is_binary: false,
            is_truncated: false,
            is_too_large: false,
        };
        let hl = HighlightedDiff::build(&diff);
        let (line_idx, added) = diff.hunks[0]
            .lines
            .iter()
            .enumerate()
            .find(|(_, l)| l.kind == DiffLineKind::Added)
            .expect("added line");
        assert!(added.text.starts_with('\t'), "raw tab must be preserved");
        let word_bg = Color::Rgb(55, 102, 75);

        for (tab_size, expected_prefix) in [(2, "  let val = omega;"), (8, "        let val = omega;")]
        {
            for tokens in [&[][..], hl.hunks[0][line_idx].as_slice()] {
                let spans = render_diff_line_spans_with_tab(
                    &added.text,
                    tokens,
                    &added.word_ranges,
                    80,
                    tab_size,
                    Style::default(),
                    word_bg,
                    true,
                );
                let joined: String = spans.iter().map(|s| s.content.as_ref()).collect();
                assert_eq!(joined, expected_prefix);
                let omega = spans
                    .iter()
                    .find(|s| s.content == "omega")
                    .expect("omega word span");
                assert_eq!(
                    omega.style.bg,
                    Some(word_bg),
                    "word-diff bg must stay aligned at tab_size={tab_size}"
                );
            }
        }
    }

    #[test]
    fn syntax_guard_skips_long_minified_lines_and_cancels_mid_flight() {
        let make_line = |kind, text: String| DiffLine {
            kind,
            old_lineno: None,
            new_lineno: None,
            text,
            word_ranges: Vec::new(),
        };
        let minified = "const a=1;".repeat(2500); // 25 KB single line
        assert!(minified.len() > MAX_SYNTAX_LINE_BYTES);
        let diff = FileDiff {
            key: FileDiffKey {
                details: DetailsKey::single(Oid::ZERO),
                path: "bundle.min.js".into(),
                old_path: None,
                context: DiffContext::Three,
            },
            hunks: vec![DiffHunk {
                header: "@@ -1,2 +1,2 @@".into(),
                lines: vec![
                    make_line(DiffLineKind::Added, minified),
                    make_line(DiffLineKind::Added, "let normal = 42;".into()),
                ],
            }],
            is_binary: false,
            is_truncated: false,
            is_too_large: false,
        };
        let hl = HighlightedDiff::build_cancellable_file(&diff, &AtomicBool::new(false)).unwrap();
        assert!(hl.hunks[0][0].is_empty(), "25KB line must skip syntect");
        assert!(!hl.hunks[0][1].is_empty(), "following normal line must still highlight");

        let cancelled = AtomicBool::new(true);
        assert!(HighlightedDiff::build_cancellable_file(&diff, &cancelled).is_none());
    }

    #[test]
    fn single_stream_and_mixed_deletion_stream_produce_accurate_tokens() {
        let make_line = |kind, text: &str| DiffLine {
            kind,
            old_lineno: None,
            new_lineno: None,
            text: text.into(),
            word_ranges: Vec::new(),
        };
        let diff = FileDiff {
            key: FileDiffKey {
                details: DetailsKey::single(Oid::ZERO),
                path: "lib.rs".into(),
                old_path: None,
                context: DiffContext::Three,
            },
            hunks: vec![
                // Hunk 0: pure Context + Added (uses single hl_new stream)
                DiffHunk {
                    header: "@@ -1,2 +1,3 @@".into(),
                    lines: vec![
                        make_line(DiffLineKind::Context, "/* comment start"),
                        make_line(DiffLineKind::Added, "still comment */"),
                        make_line(DiffLineKind::Context, "fn after() {}"),
                    ],
                },
                // Hunk 1: mixed Deleted + Context trailing
                DiffHunk {
                    header: "@@ -10,3 +11,2 @@".into(),
                    lines: vec![
                        make_line(DiffLineKind::Context, "let x = 1;"),
                        make_line(DiffLineKind::Deleted, "let old_fn = true;"),
                        make_line(DiffLineKind::Added, "let new_fn = false;"),
                        make_line(DiffLineKind::Context, "return x;"),
                    ],
                },
            ],
            is_binary: false,
            is_truncated: false,
            is_too_large: false,
        };
        let hl = HighlightedDiff::build(&diff);
        // `fn` in hunk 0 line 2 is highlighted as keyword, NOT comment
        let fn_tok = hl.hunks[0][2].iter().find(|t| t.start == 0).unwrap();
        assert_eq!(fn_tok.fg, (0xCF, 0x8E, 0x6D));
        // `let` in hunk 1 Deleted & Added both get keyword color
        assert_eq!(hl.hunks[1][1][0].fg, (0xCF, 0x8E, 0x6D));
        assert_eq!(hl.hunks[1][2][0].fg, (0xCF, 0x8E, 0x6D));
    }

    #[test]
    fn lazy_word_diff_cache_covers_unified_and_split_and_invalidates_on_diff_change() {
        let make_line = |kind, text: &str| DiffLine {
            kind,
            old_lineno: None,
            new_lineno: None,
            text: text.into(),
            word_ranges: Vec::new(),
        };
        let diff1 = Arc::new(FileDiff {
            key: FileDiffKey {
                details: DetailsKey::single(Oid::ZERO),
                path: "src/lib.rs".into(),
                old_path: None,
                context: DiffContext::Three,
            },
            hunks: vec![DiffHunk {
                header: "@@ -1,3 +1,3 @@".into(),
                lines: vec![
                    make_line(DiffLineKind::Context, "fn demo() {"),
                    make_line(DiffLineKind::Deleted, "    let old_val = 1;"),
                    make_line(DiffLineKind::Deleted, "    let untouched_del = 9;"),
                    make_line(DiffLineKind::Added, "    let new_val = 1;"),
                    make_line(DiffLineKind::Context, "}"),
                ],
            }],
            is_binary: false,
            is_truncated: false,
            is_too_large: false,
        });

        let cache = LazyWordDiffCache::default();
        let lines = &diff1.hunks[0].lines;

        // Unified row for Deleted line 1 pairs with Added line 3.
        let u_del = cache.unified_ranges(&diff1, 0, 1, lines).unwrap();
        assert!(!u_del.is_empty());
        assert_eq!(cache.cached_entry_count(), 2);

        // Unpaired Deleted line 2 returns None.
        assert!(cache.unified_ranges(&diff1, 0, 2, lines).is_none());

        // Subsequent Split query for the same pair hits cache without adding entries.
        let (sl, sr) = cache.split_ranges(&diff1, 0, Some(1), Some(3), lines);
        assert_eq!(sl.as_deref(), Some(&u_del[..]));
        assert!(sr.is_some_and(|r| !r.is_empty()));
        assert_eq!(cache.cached_entry_count(), 2);

        // Different Arc<FileDiff> resets the cache.
        let diff2 = Arc::new((*diff1).clone());
        let _ = cache.unified_ranges(&diff2, 0, 3, &diff2.hunks[0].lines);
        assert_eq!(cache.cached_entry_count(), 2);
    }

    #[test]
    fn highlighted_file_highlights_rust_skips_long_lines_and_cancels() {
        let long = "let a = 1;".repeat(400); // > 2048 bytes
        let content = format!("fn demo() {{\n{long}\n    let x = 42;\n");
        let hl = HighlightedFile::build_cancellable(
            "src/demo.rs",
            &content,
            4,
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(hl.lines.len(), 4);
        assert_eq!(hl.lines[0][0].fg, (0xCF, 0x8E, 0x6D)); // `fn`
        assert!(hl.lines[1].is_empty(), "long line must skip syntect");
        assert!(!hl.lines[2].is_empty(), "normal line after long line highlights");
        assert!(hl.lines[3].is_empty(), "trailing empty line preserved");

        let cancelled = AtomicBool::new(true);
        assert!(
            HighlightedFile::build_cancellable("src/demo.rs", &content, 4, &cancelled).is_none()
        );
    }
}
