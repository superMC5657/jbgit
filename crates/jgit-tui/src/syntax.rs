//! Darcula syntax highlighting for `FrameDiffPreview` via `syntect`.

use std::io::Cursor;
use std::sync::OnceLock;

use jgit_core::{DiffLineKind, FileDiff};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;
use syntect::easy::HighlightLines;
use syntect::highlighting::{FontStyle, Theme, ThemeSet};
use syntect::parsing::SyntaxSet;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::theme::to_color;

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

/// One highlighted token within a diff line: byte range + optional fg + font style.
#[derive(Debug, Clone, Copy)]
pub struct SyntaxToken {
    pub start: usize,
    pub end: usize,
    pub fg: (u8, u8, u8),
    pub bold: bool,
    pub italic: bool,
}

/// Pre-highlighted tokens for every line of every hunk in a [`FileDiff`].
#[derive(Debug, Clone)]
pub struct HighlightedDiff {
    pub hunks: Vec<Vec<Vec<SyntaxToken>>>,
}

impl HighlightedDiff {
    pub fn build(diff: &FileDiff) -> Self {
        let assets = assets();
        let ext = diff.key.path.rsplit('.').next().unwrap_or("");
        let syntax = assets
            .ss
            .find_syntax_by_extension(ext)
            .or_else(|| {
                let name = diff.key.path.rsplit('/').next().unwrap_or(&diff.key.path);
                assets.ss.find_syntax_by_name(name)
            })
            .unwrap_or_else(|| assets.ss.find_syntax_plain_text());

        let mut hunks = Vec::with_capacity(diff.hunks.len());
        for hunk in &diff.hunks {
            // Highlight the "new" stream (Context + Added) and "old" stream (Context + Deleted)
            // separately so deletions don't corrupt post-change multiline syntax state.
            let mut hl_old = HighlightLines::new(syntax, &assets.theme);
            let mut hl_new = HighlightLines::new(syntax, &assets.theme);
            let mut line_tokens = Vec::with_capacity(hunk.lines.len());
            for line in &hunk.lines {
                let with_nl = format!("{}\n", line.text);
                let ranges = match line.kind {
                    DiffLineKind::Deleted => hl_old.highlight_line(&with_nl, &assets.ss),
                    DiffLineKind::Added => hl_new.highlight_line(&with_nl, &assets.ss),
                    DiffLineKind::Context => {
                        let _ = hl_old.highlight_line(&with_nl, &assets.ss);
                        hl_new.highlight_line(&with_nl, &assets.ss)
                    }
                }
                .unwrap_or_default();

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
        Self { hunks }
    }
}

/// Combines syntax foreground tokens with word-diff background ranges and truncates
/// cleanly to `max_width` display columns.
pub fn render_diff_line_spans(
    text: &str,
    syntax_tokens: &[SyntaxToken],
    word_ranges: &[(usize, usize)],
    max_width: usize,
    base: Style,
    word_bg: Color,
    true_color: bool,
) -> Vec<Span<'static>> {
    if max_width == 0 {
        return Vec::new();
    }

    // Truncate to fit `max_width` (reserving 1 column for '…' when needed).
    let (limit, ellipsis) = if text.width() <= max_width {
        (text.len(), false)
    } else if max_width == 1 {
        return vec![Span::styled("…", base)];
    } else {
        let target = max_width - 1;
        let mut used = 0usize;
        let mut end = 0usize;
        for (idx, ch) in text.char_indices() {
            let cw = ch.width().unwrap_or(0);
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

    // Collect boundary offsets inside `0..limit` from both syntax tokens and word ranges.
    let mut cuts = vec![0usize, limit];
    for t in syntax_tokens {
        let (s, e) = (t.start.min(limit), t.end.min(limit));
        if s < e && text.is_char_boundary(s) && text.is_char_boundary(e) {
            cuts.push(s);
            cuts.push(e);
        }
    }
    for &(ws, we) in word_ranges {
        let (s, e) = (ws.min(limit), we.min(limit));
        if s < e && text.is_char_boundary(s) && text.is_char_boundary(e) {
            cuts.push(s);
            cuts.push(e);
        }
    }
    cuts.sort_unstable();
    cuts.dedup();

    let mut spans = Vec::with_capacity(cuts.len());
    for pair in cuts.windows(2) {
        let (s, e) = (pair[0], pair[1]);
        if s >= e {
            continue;
        }
        let in_word = word_ranges.iter().any(|&(ws, we)| s >= ws && e <= we);
        let tok = syntax_tokens.iter().find(|t| s >= t.start && e <= t.end);
        let mut style = base;
        if let Some(t) = tok {
            style = style.fg(to_color(t.fg, true_color));
            if t.bold {
                style = style.add_modifier(Modifier::BOLD);
            }
            if t.italic {
                style = style.add_modifier(Modifier::ITALIC);
            }
        }
        if in_word {
            style = style.bg(word_bg).add_modifier(Modifier::BOLD);
        }
        spans.push(Span::styled(text[s..e].to_string(), style));
    }

    if ellipsis {
        spans.push(Span::styled("…", base));
    }
    spans
}

#[cfg(test)]
mod tests {
    use jgit_core::{DetailsKey, DiffContext, DiffHunk, DiffLine, FileDiffKey, Oid};

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
}
