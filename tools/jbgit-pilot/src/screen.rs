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

pub(crate) fn find_text_matches(shared: &Arc<Mutex<SharedPtyState>>, needle: &str) -> Vec<TextMatch> {
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

pub(crate) fn inspect_row_spans(
    shared: &Arc<Mutex<SharedPtyState>>,
    row: u16,
) -> Vec<RowSpanInfo> {
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

pub(crate) fn extract_latest_osc52_clipboard(raw: &[u8]) -> Option<String> {
    const PREFIX: &[u8] = b"\x1b]52;";
    let mut latest = None;
    let mut i = 0;
    while i + PREFIX.len() <= raw.len() {
        if raw[i..].starts_with(PREFIX) {
            let after_prefix = &raw[i + PREFIX.len()..];
            if let Some(semi) = after_prefix.iter().take(16).position(|&b| b == b';') {
                let payload = &after_prefix[semi + 1..];
                let end = find_osc_terminator(payload);
                if let Some((term_idx, term_len)) = end {
                    let b64 = &payload[..term_idx];
                    if b64 != b"?"
                        && let Some(decoded) = base64_decode(b64)
                        && let Ok(text) = String::from_utf8(decoded)
                    {
                        latest = Some(text);
                    }
                    i += PREFIX.len() + semi + 1 + term_idx + term_len;
                    continue;
                }
            }
        }
        i += 1;
    }
    latest
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
    use super::extract_latest_osc52_clipboard;

    #[test]
    fn decodes_latest_osc52_clipboard_sequence() {
        assert_eq!(
            extract_latest_osc52_clipboard(b"\x1b]52;c;YWJj\x07"),
            Some("abc".to_string())
        );
        assert_eq!(
            extract_latest_osc52_clipboard(b"noise\x1b]52;c;Zmlyc3Q=\x07more\x1b]52;c;c2Vjb25k\x1b\\"),
            Some("second".to_string())
        );
        assert_eq!(
            extract_latest_osc52_clipboard(b"\x1b]52;c;?\x07"),
            None
        );
    }
}
