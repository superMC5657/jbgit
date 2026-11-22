use anyhow::{Result, bail};
use jbgit_core::{KeyToken, KeyTokenKind, parse_key_token};

use crate::protocol::MouseBtn;

/// Encodes a symbolic key token (e.g. `Enter`, `Ctrl+S`, `<C-s>`, `F8`, `Alt+r`,
/// `<Space>`, `j`) into ANSI / VT bytes suitable for writing into a PTY.
///
/// Parsing delegates directly to [`jbgit_core::parse_key_token`] so `keymap.toml`
/// syntax and `jbgit-pilot` key specs share a single canonical parser.
pub fn encode_key(spec: &str) -> Result<Vec<u8>> {
    let token = parse_key_token(spec)?;
    encode_token(token)
}

pub fn encode_token(token: KeyToken) -> Result<Vec<u8>> {
    let KeyToken {
        kind,
        ctrl,
        alt,
        shift,
    } = token;

    // Plain unmodified key (including standalone shifted uppercase letters like `G`
    // and normalized `BackTab`).
    let is_plain_char = !ctrl
        && !alt
        && matches!(kind, KeyTokenKind::Char(c) if !shift || c.is_ascii_uppercase());
    if is_plain_char && let KeyTokenKind::Char(ch) = kind {
        let mut buf = [0u8; 4];
        return Ok(ch.encode_utf8(&mut buf).as_bytes().to_vec());
    }

    if !ctrl && !alt && !shift {
        return encode_unmodified_named(kind);
    }

    encode_modified_token(kind, ctrl, alt, shift)
}

fn encode_unmodified_named(kind: KeyTokenKind) -> Result<Vec<u8>> {
    Ok(match kind {
        KeyTokenKind::Char(ch) => {
            let mut buf = [0u8; 4];
            ch.encode_utf8(&mut buf).as_bytes().to_vec()
        }
        KeyTokenKind::Enter => b"\r".to_vec(),
        KeyTokenKind::Esc => b"\x1b".to_vec(),
        KeyTokenKind::Tab => b"\t".to_vec(),
        KeyTokenKind::BackTab => b"\x1b[Z".to_vec(),
        KeyTokenKind::Backspace => vec![0x7f],
        KeyTokenKind::Up => b"\x1b[A".to_vec(),
        KeyTokenKind::Down => b"\x1b[B".to_vec(),
        KeyTokenKind::Right => b"\x1b[C".to_vec(),
        KeyTokenKind::Left => b"\x1b[D".to_vec(),
        KeyTokenKind::Home => b"\x1b[H".to_vec(),
        KeyTokenKind::End => b"\x1b[F".to_vec(),
        KeyTokenKind::Insert => b"\x1b[2~".to_vec(),
        KeyTokenKind::Delete => b"\x1b[3~".to_vec(),
        KeyTokenKind::PageUp => b"\x1b[5~".to_vec(),
        KeyTokenKind::PageDown => b"\x1b[6~".to_vec(),
        KeyTokenKind::F(1) => b"\x1bOP".to_vec(),
        KeyTokenKind::F(2) => b"\x1bOQ".to_vec(),
        KeyTokenKind::F(3) => b"\x1bOR".to_vec(),
        KeyTokenKind::F(4) => b"\x1bOS".to_vec(),
        KeyTokenKind::F(5) => b"\x1b[15~".to_vec(),
        KeyTokenKind::F(6) => b"\x1b[17~".to_vec(),
        KeyTokenKind::F(7) => b"\x1b[18~".to_vec(),
        KeyTokenKind::F(8) => b"\x1b[19~".to_vec(),
        KeyTokenKind::F(9) => b"\x1b[20~".to_vec(),
        KeyTokenKind::F(10) => b"\x1b[21~".to_vec(),
        KeyTokenKind::F(11) => b"\x1b[23~".to_vec(),
        KeyTokenKind::F(12) => b"\x1b[24~".to_vec(),
        KeyTokenKind::F(n) => bail!("unsupported function key F{n}"),
    })
}

fn encode_modified_token(
    kind: KeyTokenKind,
    ctrl: bool,
    alt: bool,
    shift: bool,
) -> Result<Vec<u8>> {
    let xterm_mod = 1 + u8::from(shift) + (u8::from(alt) * 2) + (u8::from(ctrl) * 4);
    match kind {
        KeyTokenKind::Tab if shift && !ctrl && !alt => Ok(b"\x1b[Z".to_vec()),
        KeyTokenKind::BackTab => Ok(b"\x1b[Z".to_vec()),
        KeyTokenKind::Up => Ok(format!("\x1b[1;{xterm_mod}A").into_bytes()),
        KeyTokenKind::Down => Ok(format!("\x1b[1;{xterm_mod}B").into_bytes()),
        KeyTokenKind::Right => Ok(format!("\x1b[1;{xterm_mod}C").into_bytes()),
        KeyTokenKind::Left => Ok(format!("\x1b[1;{xterm_mod}D").into_bytes()),
        KeyTokenKind::Home => Ok(format!("\x1b[1;{xterm_mod}H").into_bytes()),
        KeyTokenKind::End => Ok(format!("\x1b[1;{xterm_mod}F").into_bytes()),
        KeyTokenKind::F(n @ 1..=4) => {
            let suffix = match n {
                1 => 'P',
                2 => 'Q',
                3 => 'R',
                4 => 'S',
                _ => unreachable!(),
            };
            Ok(format!("\x1b[1;{xterm_mod}{suffix}").into_bytes())
        }
        KeyTokenKind::Insert
        | KeyTokenKind::Delete
        | KeyTokenKind::PageUp
        | KeyTokenKind::PageDown
        | KeyTokenKind::F(5..=12) => {
            let code = match kind {
                KeyTokenKind::Insert => 2,
                KeyTokenKind::Delete => 3,
                KeyTokenKind::PageUp => 5,
                KeyTokenKind::PageDown => 6,
                KeyTokenKind::F(5) => 15,
                KeyTokenKind::F(6) => 17,
                KeyTokenKind::F(7) => 18,
                KeyTokenKind::F(8) => 19,
                KeyTokenKind::F(9) => 20,
                KeyTokenKind::F(10) => 21,
                KeyTokenKind::F(11) => 23,
                KeyTokenKind::F(12) => 24,
                _ => unreachable!(),
            };
            Ok(format!("\x1b[{code};{xterm_mod}~").into_bytes())
        }
        KeyTokenKind::Backspace => {
            if alt {
                Ok(b"\x1b\x7f".to_vec())
            } else if ctrl {
                Ok(vec![0x08])
            } else {
                Ok(vec![0x7f])
            }
        }
        KeyTokenKind::Enter => {
            if ctrl {
                Ok(b"\x1b[13;5u".to_vec())
            } else if alt {
                Ok(b"\x1b\r".to_vec())
            } else {
                Ok(b"\r".to_vec())
            }
        }
        KeyTokenKind::Esc => Ok(b"\x1b".to_vec()),
        KeyTokenKind::Char(ch) => {
            let mut out = Vec::new();
            if alt {
                out.push(0x1b);
            }
            if ctrl {
                let code = match ch.to_ascii_lowercase() {
                    'a'..='z' => (ch.to_ascii_lowercase() as u8) - b'a' + 1,
                    '`' | '~' | '@' | ' ' => 0x00,
                    '[' => 0x1b,
                    '\\' => 0x1c,
                    ']' => 0x1d,
                    '^' => 0x1e,
                    '_' => 0x1f,
                    _ => bail!("unsupported Ctrl+{ch} combination"),
                };
                out.push(code);
            } else {
                let final_ch = if shift {
                    ch.to_ascii_uppercase()
                } else {
                    ch
                };
                let mut buf = [0u8; 4];
                out.extend_from_slice(final_ch.encode_utf8(&mut buf).as_bytes());
            }
            Ok(out)
        }
        other => bail!("unsupported modified key {other:?}"),
    }
}

/// Encodes an SGR mouse press + release at 0-based `(col, row)`.
pub fn encode_sgr_click(col: u16, row: u16, btn: MouseBtn) -> Vec<u8> {
    let code = match btn {
        MouseBtn::Left => 0,
        MouseBtn::Middle => 1,
        MouseBtn::Right => 2,
    };
    let c1 = col.saturating_add(1);
    let r1 = row.saturating_add(1);
    format!("\x1b[<{code};{c1};{r1}M\x1b[<{code};{c1};{r1}m").into_bytes()
}

/// Encodes an SGR mouse wheel scroll event at 0-based `(col, row)`.
pub fn encode_sgr_scroll(col: u16, row: u16, up: bool, steps: u16) -> Vec<u8> {
    let code = if up { 64 } else { 65 };
    let c1 = col.saturating_add(1);
    let r1 = row.saturating_add(1);
    let mut out = Vec::new();
    for _ in 0..steps.max(1) {
        out.extend_from_slice(format!("\x1b[<{code};{c1};{r1}M").as_bytes());
    }
    out
}

/// Encodes an SGR mouse hover / motion event (no button pressed) at 0-based `(col, row)`.
pub fn encode_sgr_hover(col: u16, row: u16) -> Vec<u8> {
    let c1 = col.saturating_add(1);
    let r1 = row.saturating_add(1);
    format!("\x1b[<35;{c1};{r1}M").into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_modified_function_and_nav_keys() {
        assert_eq!(encode_key("<S-F10>").unwrap(), b"\x1b[21;2~");
        assert_eq!(encode_key("Shift+F10").unwrap(), b"\x1b[21;2~");
        assert_eq!(encode_key("Ctrl+F1").unwrap(), b"\x1b[1;5P");
        assert_eq!(encode_key("Shift+Up").unwrap(), b"\x1b[1;2A");
        assert_eq!(encode_key("Shift+Down").unwrap(), b"\x1b[1;2B");
        assert_eq!(encode_key("Ctrl+PageDown").unwrap(), b"\x1b[6;5~");
        assert_eq!(encode_key("<C-A-n>").unwrap(), b"\x1b\x0e");
        assert_eq!(encode_key("Alt+Backspace").unwrap(), b"\x1b\x7f");
    }

    #[test]
    fn matches_keymap_toml_tokens_without_ambiguity() {
        assert_eq!(encode_key("<CR>").unwrap(), b"\r");
        assert_eq!(encode_key("<Enter>").unwrap(), b"\r");
        assert_eq!(encode_key("<Esc>").unwrap(), b"\x1b");
        assert_eq!(encode_key("<Space>").unwrap(), b" ");
        assert_eq!(encode_key("<BackTab>").unwrap(), b"\x1b[Z");
        assert_eq!(encode_key("<S-Tab>").unwrap(), b"\x1b[Z");
        assert_eq!(encode_key("<C-Enter>").unwrap(), b"\x1b[13;5u");
        assert_eq!(encode_key("<A-1>").unwrap(), b"\x1b1");
        assert_eq!(encode_key("<F8>").unwrap(), b"\x1b[19~");
        assert_eq!(encode_key("G").unwrap(), b"G");
        assert_eq!(encode_key("<S-g>").unwrap(), b"G");
        assert_eq!(encode_key("?").unwrap(), b"?");
    }

    #[test]
    fn encodes_sgr_hover_coordinates() {
        assert_eq!(encode_sgr_hover(10, 5), b"\x1b[<35;11;6M");
    }
}
