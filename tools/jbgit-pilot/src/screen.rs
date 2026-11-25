use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use crate::protocol::{CellPos, ColorCount, ColorPair, ResponseEnvelope, RowSpanInfo, TextMatch};
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

fn collect_row_text_and_cols(
    screen: &vt100::Screen,
    row: u16,
    cols: u16,
) -> (String, Vec<(usize, u16)>) {
    let mut row_str = String::new();
    let mut col_map = Vec::with_capacity(cols as usize);
    for c in 0..cols {
        let Some(cell) = screen.cell(row, c) else {
            let start_byte = row_str.len();
            row_str.push(' ');
            col_map.push((start_byte, c));
            continue;
        };
        if cell.is_wide_continuation() {
            continue;
        }
        let start_byte = row_str.len();
        let contents = cell.contents();
        if contents.is_empty() {
            row_str.push(' ');
        } else {
            row_str.push_str(contents);
        }
        col_map.push((start_byte, c));
    }
    (row_str, col_map)
}

fn render_row_text(screen: &vt100::Screen, row: u16, cols: u16) -> String {
    collect_row_text_and_cols(screen, row, cols).0
}

pub(crate) fn find_text_matches(
    shared: &Arc<Mutex<SharedPtyState>>,
    needle: &str,
) -> Vec<TextMatch> {
    let g = shared.lock().unwrap();
    let screen = g.parser.screen();
    let mut out = Vec::new();
    for r in 0..g.rows {
        let (row_str, col_map) = collect_row_text_and_cols(screen, r, g.cols);
        if let Some(byte_idx) = row_str.find(needle) {
            let col = col_map
                .iter()
                .rev()
                .find(|(b, _)| *b <= byte_idx)
                .map(|(_, c)| *c)
                .unwrap_or(0);
            out.push(TextMatch {
                pos: CellPos { col, row: r },
                line: row_str.trim_end().to_string(),
            });
        }
    }
    out
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
    use super::{Osc52ClipboardScanner, extract_latest_osc52_clipboard};

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
