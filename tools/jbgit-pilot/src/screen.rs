use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use crate::protocol::{
    CellPos, ColorCount, ColorPair, ResolvedTarget, ResponseEnvelope, RowSpanInfo, TextMatch,
    TextTarget,
};
use crate::server::SharedPtyState;

pub(crate) fn build_screen_resp(
    id: u64,
    shared: &Arc<Mutex<SharedPtyState>>,
    exit_code: Option<i32>,
) -> ResponseEnvelope {
    let g = shared.lock().unwrap();
    let screen = g.parser.screen();
    let (cur_row, cur_col) = screen.cursor_position();
    let text = format_screen(screen, g.cols, g.rows);
    ResponseEnvelope {
        id,
        ok: true,
        error: None,
        alive: exit_code.is_none() && !g.eof,
        exit_code,
        cols: g.cols,
        rows: g.rows,
        cursor: CellPos {
            col: cur_col,
            row: cur_row,
        },
        screen: Some(text),
        matches: Vec::new(),
        row_spans: Vec::new(),
        palette: Vec::new(),
        clipboard: None,
        target: None,
    }
}

pub(crate) fn make_error_resp(
    id: u64,
    err: String,
    shared: &Arc<Mutex<SharedPtyState>>,
    exit_code: Option<i32>,
) -> ResponseEnvelope {
    let mut resp = build_screen_resp(id, shared, exit_code);
    resp.ok = false;
    resp.error = Some(err);
    resp
}

pub(crate) fn screen_has_content(screen: &vt100::Screen, cols: u16, rows: u16) -> bool {
    (0..rows).any(|r| !render_row_text(screen, r, cols).trim().is_empty())
}

fn format_screen(screen: &vt100::Screen, cols: u16, rows: u16) -> String {
    let mut out = Vec::with_capacity(rows as usize);
    for r in 0..rows {
        let row_str = render_row_text(screen, r, cols);
        out.push(format!("{r:02}|{}", row_str.trim_end()));
    }
    out.join("\n")
}

#[derive(Debug, Clone, Copy)]
struct RowCellSpan {
    start_byte: usize,
    start_col: u16,
    end_col: u16,
}

fn collect_row_text_and_cols(
    screen: &vt100::Screen,
    row: u16,
    cols: u16,
) -> (String, Vec<RowCellSpan>) {
    let mut row_str = String::new();
    let mut col_map: Vec<RowCellSpan> = Vec::with_capacity(cols as usize);
    for c in 0..cols {
        let Some(cell) = screen.cell(row, c) else {
            let start_byte = row_str.len();
            row_str.push(' ');
            col_map.push(RowCellSpan {
                start_byte,
                start_col: c,
                end_col: c,
            });
            continue;
        };
        if cell.is_wide_continuation() {
            if let Some(last) = col_map.last_mut() {
                last.end_col = c;
            }
            continue;
        }
        let start_byte = row_str.len();
        let contents = cell.contents();
        if contents.is_empty() {
            row_str.push(' ');
        } else {
            row_str.push_str(contents);
        }
        col_map.push(RowCellSpan {
            start_byte,
            start_col: c,
            end_col: c,
        });
    }
    (row_str, col_map)
}

fn render_row_text(screen: &vt100::Screen, row: u16, cols: u16) -> String {
    collect_row_text_and_cols(screen, row, cols).0
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MatchedSpan {
    pub(crate) start_col: u16,
    pub(crate) end_col: u16,
    pub(crate) row: u16,
    pub(crate) line: String,
}

impl MatchedSpan {
    pub(crate) fn center_pos(&self) -> CellPos {
        CellPos {
            col: self.start_col + (self.end_col.saturating_sub(self.start_col)) / 2,
            row: self.row,
        }
    }
}

pub(crate) fn find_text_spans_in_screen(
    screen: &vt100::Screen,
    cols: u16,
    rows: u16,
    needle: &str,
    row_filter: Option<u16>,
) -> Vec<MatchedSpan> {
    if needle.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    let row_range = match row_filter {
        Some(r) if r < rows => r..=r,
        Some(_) => return out,
        None => 0..=rows.saturating_sub(1),
    };
    if rows == 0 {
        return out;
    }
    for r in row_range {
        let (row_str, col_map) = collect_row_text_and_cols(screen, r, cols);
        let trimmed = row_str.trim_end().to_string();
        let mut search_from = 0usize;
        while let Some(rel) = row_str[search_from..].find(needle) {
            let byte_start = search_from + rel;
            let byte_end = byte_start + needle.len();
            let start_col = col_map
                .iter()
                .rev()
                .find(|s| s.start_byte <= byte_start)
                .map(|s| s.start_col)
                .unwrap_or(0);
            let end_col = col_map
                .iter()
                .rev()
                .find(|s| s.start_byte < byte_end)
                .map(|s| s.end_col)
                .unwrap_or(start_col);
            out.push(MatchedSpan {
                start_col,
                end_col,
                row: r,
                line: trimmed.clone(),
            });
            search_from = byte_end;
        }
    }
    out
}

pub(crate) fn resolve_text_target_in_screen(
    screen: &vt100::Screen,
    cols: u16,
    rows: u16,
    target: &TextTarget,
) -> anyhow::Result<ResolvedTarget> {
    let needle = target.needle.as_str();
    if needle.is_empty() {
        anyhow::bail!("text target cannot be empty");
    }
    if let Some(r) = target.row
        && r >= rows
    {
        anyhow::bail!("row {r} out of bounds (screen has {rows} rows)");
    }
    let matches = find_text_spans_in_screen(screen, cols, rows, needle, target.row);
    if matches.is_empty() {
        match target.row {
            Some(r) => anyhow::bail!("text {needle:?} not found on row {r}"),
            None => anyhow::bail!("text {needle:?} not found on screen"),
        }
    }
    let format_matches = || -> String {
        matches
            .iter()
            .enumerate()
            .map(|(i, m)| format!("[{i}] cols={}..={} row={}", m.start_col, m.end_col, m.row))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let selected_idx = match target.index {
        None => {
            if matches.len() > 1 {
                anyhow::bail!(
                    "ambiguous text target {needle:?}: {} matches ({}); specify --row <row> or --index <n>",
                    matches.len(),
                    format_matches()
                );
            }
            0
        }
        Some(idx) => {
            if idx >= matches.len() {
                anyhow::bail!(
                    "match index {idx} out of bounds for {needle:?} ({} match(es): {})",
                    matches.len(),
                    format_matches()
                );
            }
            idx
        }
    };
    let found = &matches[selected_idx];
    Ok(ResolvedTarget {
        pos: found.center_pos(),
        start_col: found.start_col,
        end_col: found.end_col,
        match_index: selected_idx,
        match_count: matches.len(),
    })
}

pub(crate) fn resolve_text_target(
    shared: &Arc<Mutex<SharedPtyState>>,
    target: &TextTarget,
) -> anyhow::Result<ResolvedTarget> {
    let g = shared.lock().unwrap();
    resolve_text_target_in_screen(g.parser.screen(), g.cols, g.rows, target)
}

pub(crate) fn find_text_matches(
    shared: &Arc<Mutex<SharedPtyState>>,
    needle: &str,
) -> Vec<TextMatch> {
    let g = shared.lock().unwrap();
    find_text_spans_in_screen(g.parser.screen(), g.cols, g.rows, needle, None)
        .into_iter()
        .map(|m| TextMatch {
            pos: CellPos {
                col: m.start_col,
                row: m.row,
            },
            end_col: Some(m.end_col),
            line: m.line,
        })
        .collect()
}

fn fmt_color(c: vt100::Color) -> String {
    match c {
        vt100::Color::Default => "default".into(),
        vt100::Color::Idx(i) => format!("idx({i})"),
        vt100::Color::Rgb(r, g, b) => format!("#{r:02X}{g:02X}{b:02X}"),
    }
}

pub(crate) fn inspect_row_spans(shared: &Arc<Mutex<SharedPtyState>>, row: u16) -> Vec<RowSpanInfo> {
    let g = shared.lock().unwrap();
    if row >= g.rows {
        return Vec::new();
    }
    let screen = g.parser.screen();
    let mut spans: Vec<RowSpanInfo> = Vec::new();
    let mut cur: Option<RowSpanInfo> = None;

    for c in 0..g.cols {
        let Some(cell) = screen.cell(row, c) else {
            continue;
        };
        if cell.is_wide_continuation() {
            if let Some(s) = cur.as_mut() {
                s.end_col = c;
            }
            continue;
        }
        let fg = fmt_color(cell.fgcolor());
        let bg = fmt_color(cell.bgcolor());
        let ch = if cell.contents().is_empty() {
            " "
        } else {
            cell.contents()
        };

        match cur.as_mut() {
            Some(s) if s.colors.fg == fg && s.colors.bg == bg => {
                s.end_col = c;
                s.text.push_str(ch);
            }
            _ => {
                if let Some(prev) = cur.take()
                    && !prev.text.trim().is_empty()
                {
                    spans.push(prev);
                }
                cur = Some(RowSpanInfo {
                    start_col: c,
                    end_col: c,
                    text: ch.to_string(),
                    colors: ColorPair { fg, bg },
                });
            }
        }
    }
    if let Some(prev) = cur.take()
        && !prev.text.trim().is_empty()
    {
        spans.push(prev);
    }
    spans
}

pub(crate) fn summarize_palette(shared: &Arc<Mutex<SharedPtyState>>) -> Vec<ColorCount> {
    let g = shared.lock().unwrap();
    let screen = g.parser.screen();
    let mut counts: BTreeMap<ColorPair, usize> = BTreeMap::new();
    for r in 0..g.rows {
        for c in 0..g.cols {
            if let Some(cell) = screen.cell(r, c) {
                if cell.is_wide_continuation() {
                    continue;
                }
                let key = ColorPair {
                    fg: fmt_color(cell.fgcolor()),
                    bg: fmt_color(cell.bgcolor()),
                };
                *counts.entry(key).or_insert(0) += 1;
            }
        }
    }
    let mut list: Vec<ColorCount> = counts
        .into_iter()
        .map(|(colors, cells)| ColorCount { colors, cells })
        .collect();
    list.sort_by_key(|a| std::cmp::Reverse(a.cells));
    list.truncate(20);
    list
}

/// Incremental streaming scanner for OSC-52 clipboard writes (`\x1b]52;<sel>;<b64>\x07|\x1b\\`).
///
/// Fed directly from the PTY reader loop so `PilotAction::Clipboard` returns the
/// latest clipboard payload in O(1) without re-reading `pty_raw.log`.
#[derive(Debug, Default)]
pub(crate) struct Osc52ClipboardScanner {
    pending: Vec<u8>,
    latest: Option<String>,
}

impl Osc52ClipboardScanner {
    const PREFIX: &'static [u8] = b"\x1b]52;";
    const MAX_PENDING_BYTES: usize = 256 * 1024;

    pub(crate) fn feed(&mut self, chunk: &[u8]) {
        if chunk.is_empty() {
            return;
        }
        self.pending.extend_from_slice(chunk);

        let mut i = 0;
        let mut keep_from = self.pending.len();
        while i < self.pending.len() {
            let rem = &self.pending[i..];
            if rem.starts_with(Self::PREFIX) {
                let after_prefix = &rem[Self::PREFIX.len()..];
                if let Some(semi) = after_prefix.iter().take(16).position(|&b| b == b';') {
                    let payload = &after_prefix[semi + 1..];
                    if let Some((term_idx, term_len)) = find_osc_terminator(payload) {
                        let b64 = &payload[..term_idx];
                        if b64 != b"?"
                            && let Some(decoded) = base64_decode(b64)
                            && let Ok(text) = String::from_utf8(decoded)
                        {
                            self.latest = Some(text);
                        }
                        i += Self::PREFIX.len() + semi + 1 + term_idx + term_len;
                        continue;
                    }
                    // Complete header, waiting for terminator in a later chunk.
                    if rem.len() <= Self::MAX_PENDING_BYTES {
                        keep_from = i;
                        break;
                    }
                } else if after_prefix.len() < 16 {
                    // Header selector not complete yet; wait for more bytes.
                    keep_from = i;
                    break;
                }
            } else if rem.len() < Self::PREFIX.len() && Self::PREFIX.starts_with(rem) {
                // Partial `\x1b]52;` prefix at the tail of the chunk.
                keep_from = i;
                break;
            }
            i += 1;
        }

        if keep_from == 0 {
            // Retain entire pending slice (bounded by MAX_PENDING_BYTES above).
        } else if keep_from >= self.pending.len() {
            self.pending.clear();
        } else {
            self.pending.drain(..keep_from);
        }
    }

    pub(crate) fn latest(&self) -> Option<&str> {
        self.latest.as_deref()
    }
}

#[cfg(test)]
pub(crate) fn extract_latest_osc52_clipboard(raw: &[u8]) -> Option<String> {
    let mut scanner = Osc52ClipboardScanner::default();
    scanner.feed(raw);
    scanner.latest().map(ToString::to_string)
}

fn find_osc_terminator(bytes: &[u8]) -> Option<(usize, usize)> {
    let mut j = 0;
    while j < bytes.len() {
        if bytes[j] == 0x07 {
            return Some((j, 1));
        }
        if bytes[j] == 0x1b && bytes.get(j + 1) == Some(&b'\\') {
            return Some((j, 2));
        }
        j += 1;
    }
    None
}

fn base64_decode(input: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity((input.len() * 3) / 4);
    let mut acc: u32 = 0;
    let mut bits: u8 = 0;
    for &b in input {
        if b.is_ascii_whitespace() {
            continue;
        }
        if b == b'=' {
            break;
        }
        let val = match b {
            b'A'..=b'Z' => b - b'A',
            b'a'..=b'z' => b - b'a' + 26,
            b'0'..=b'9' => b - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            _ => return None,
        };
        acc = (acc << 6) | u32::from(val);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(((acc >> bits) & 0xff) as u8);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU16;

    use crate::protocol::TextTarget;

    use super::{
        Osc52ClipboardScanner, extract_latest_osc52_clipboard, find_text_spans_in_screen,
        resolve_text_target_in_screen,
    };

    #[test]
    fn find_text_spans_tracks_exact_display_columns_with_cjk_wide_chars() {
        let mut parser = vt100::Parser::new(
            NonZeroU16::new(10).unwrap(),
            NonZeroU16::new(140).unwrap(),
            0,
        );
        // Simulate Log row with CJK before a menu at col 92 (`紧凑引用胶囊视图` inside `cols 92..=124`).
        let prefix = "  ★ main   QA Tester 刚刚 ";
        parser.process(b"\x1b[6;1H");
        parser.process(prefix.as_bytes());
        // Position cursor explicitly at 1-based row 6 (0-based row 5), col 93 (0-based col 92).
        parser.process(b"\x1b[6;93H\xe2\x94\x82   \xe7\xb4\xa7\xe5\x87\x91\xe5\xbc\x95\xe7\x94\xa8\xe8\x83\xb6\xe5\x9b\x8a\xe8\xa7\x86\xe5\x9b\xbe   \xe2\x94\x82");

        let spans = find_text_spans_in_screen(parser.screen(), 140, 10, "紧凑引用胶囊视图", None);
        assert_eq!(spans.len(), 1);
        let m = &spans[0];
        assert_eq!(m.row, 5);
        // col 92 = '│', cols 93..=95 = "   ", cols 96..=111 = 8 CJK chars (16 display cols)
        assert_eq!(m.start_col, 96);
        assert_eq!(m.end_col, 111);
        let center = m.center_pos();
        assert_eq!(center.row, 5);
        assert!((96..=111).contains(&center.col));
    }

    #[test]
    fn find_text_spans_finds_multiple_matches_on_same_row_and_respects_row_filter() {
        let mut parser =
            vt100::Parser::new(NonZeroU16::new(6).unwrap(), NonZeroU16::new(60).unwrap(), 0);
        parser.process(b"\x1b[2;1H[ Save ]   [ Save ]");
        parser.process(b"\x1b[4;1H[ Save ]");

        let all = find_text_spans_in_screen(parser.screen(), 60, 6, "Save", None);
        assert_eq!(all.len(), 3);
        assert_eq!(all[0].row, 1);
        assert_eq!(all[0].start_col, 2);
        assert_eq!(all[0].end_col, 5);
        assert_eq!(all[1].row, 1);
        assert_eq!(all[1].start_col, 13);
        assert_eq!(all[1].end_col, 16);
        assert_eq!(all[2].row, 3);
        assert_eq!(all[2].start_col, 2);

        let only_row3 = find_text_spans_in_screen(parser.screen(), 60, 6, "Save", Some(3));
        assert_eq!(only_row3.len(), 1);
        assert_eq!(only_row3[0].row, 3);
    }

    #[test]
    fn resolve_text_target_rejects_ambiguous_matches_unless_disambiguated() {
        let mut parser =
            vt100::Parser::new(NonZeroU16::new(6).unwrap(), NonZeroU16::new(60).unwrap(), 0);
        // Header row 0 and popup menu row 3 both contain "提交".
        parser.process(b"\x1b[1;1H[1] \xe6\x8f\x90\xe4\xba\xa4  [2] \xe6\x97\xa5\xe5\xbf\x97");
        parser.process(b"\x1b[4;21H\xe2\x94\x82 \xe6\x8f\x90\xe4\xba\xa4 \xe2\x94\x82");

        // Ambiguous without --row or --index must fail instead of silently clicking row 0.
        let err = resolve_text_target_in_screen(
            parser.screen(),
            60,
            6,
            &TextTarget {
                needle: "提交".to_string(),
                row: None,
                index: None,
            },
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("ambiguous"), "unexpected error: {err}");
        assert!(err.contains("row=0") && err.contains("row=3"));

        // Disambiguating by --row selects the popup item uniquely.
        let by_row = resolve_text_target_in_screen(
            parser.screen(),
            60,
            6,
            &TextTarget {
                needle: "提交".to_string(),
                row: Some(3),
                index: None,
            },
        )
        .unwrap();
        assert_eq!(by_row.pos.row, 3);
        assert_eq!(by_row.start_col, 22);
        assert_eq!(by_row.end_col, 25);
        assert!((by_row.start_col..=by_row.end_col).contains(&by_row.pos.col));
        assert_eq!(by_row.match_index, 0);
        assert_eq!(by_row.match_count, 1);

        // Disambiguating by --index selects the requested occurrence and reports 2/2.
        let by_idx = resolve_text_target_in_screen(
            parser.screen(),
            60,
            6,
            &TextTarget {
                needle: "提交".to_string(),
                row: None,
                index: Some(1),
            },
        )
        .unwrap();
        assert_eq!(by_idx.pos.row, 3);
        assert_eq!(by_idx.match_index, 1);
        assert_eq!(by_idx.match_count, 2);
    }

    #[test]
    fn resolve_text_target_validates_empty_oob_and_missing() {
        let mut parser =
            vt100::Parser::new(NonZeroU16::new(4).unwrap(), NonZeroU16::new(40).unwrap(), 0);
        parser.process(b"\x1b[1;1HHello World");

        let empty_err = resolve_text_target_in_screen(
            parser.screen(),
            40,
            4,
            &TextTarget {
                needle: String::new(),
                row: None,
                index: None,
            },
        )
        .unwrap_err()
        .to_string();
        assert!(empty_err.contains("cannot be empty"));

        let row_oob = resolve_text_target_in_screen(
            parser.screen(),
            40,
            4,
            &TextTarget {
                needle: "Hello".to_string(),
                row: Some(4),
                index: None,
            },
        )
        .unwrap_err()
        .to_string();
        assert!(row_oob.contains("out of bounds"));

        let missing = resolve_text_target_in_screen(
            parser.screen(),
            40,
            4,
            &TextTarget {
                needle: "Missing".to_string(),
                row: Some(0),
                index: None,
            },
        )
        .unwrap_err()
        .to_string();
        assert!(missing.contains("not found on row 0"));

        let idx_oob = resolve_text_target_in_screen(
            parser.screen(),
            40,
            4,
            &TextTarget {
                needle: "Hello".to_string(),
                row: None,
                index: Some(1),
            },
        )
        .unwrap_err()
        .to_string();
        assert!(idx_oob.contains("match index 1 out of bounds"));
        assert!(idx_oob.contains("[0] cols=0..=4 row=0"));
    }

    #[test]
    fn decodes_latest_osc52_clipboard_sequence() {
        assert_eq!(
            extract_latest_osc52_clipboard(b"\x1b]52;c;YWJj\x07"),
            Some("abc".to_string())
        );
        assert_eq!(
            extract_latest_osc52_clipboard(
                b"noise\x1b]52;c;Zmlyc3Q=\x07more\x1b]52;c;c2Vjb25k\x1b\\"
            ),
            Some("second".to_string())
        );
        assert_eq!(extract_latest_osc52_clipboard(b"\x1b]52;c;?\x07"), None);
    }

    #[test]
    fn incremental_osc52_scanner_handles_arbitrary_chunk_splits() {
        let stream = b"prefix\x1b]52;c;Zmlyc3Q=\x07mid\x1b]52;c;c2Vjb25k\x1b\\tail";
        for chunk_size in [1, 2, 3, 5, 7, 13] {
            let mut scanner = Osc52ClipboardScanner::default();
            for chunk in stream.chunks(chunk_size) {
                scanner.feed(chunk);
            }
            assert_eq!(
                scanner.latest(),
                Some("second"),
                "failed for chunk_size={chunk_size}"
            );
        }
    }
}
