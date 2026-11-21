//! Darcula syntax highlighting for `FrameDiffPreview` via `syntect`.

use std::io::Cursor;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

use jbgit_core::{DiffHunk, DiffLineKind, FileDiff};
use jbgit_engine::ThreeDocStageDiff;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;
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
        let ext = path.rsplit('.').next().unwrap_or("");
        let syntax = assets
            .ss
            .find_syntax_by_extension(ext)
            .or_else(|| {
                let name = path.rsplit('/').next().unwrap_or(path);
                assets.ss.find_syntax_by_name(name)
            })
            .unwrap_or_else(|| assets.ss.find_syntax_plain_text());

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
        let spans = render_diff_line_spans_with_tab(
            "\tlet x = 1;",
            &[],
            &[],
            40,
            2,
            Style::default(),
            Color::Reset,
            true,
        );
        let joined: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(joined, "  let x = 1;");
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
}
