use anyhow::{Result, bail};

use crate::protocol::MouseBtn;

/// Encodes a symbolic key token (e.g. `Enter`, `Ctrl+S`, `F8`, `Alt+r`, `Space`, `j`)
/// into ANSI / VT bytes suitable for writing into a PTY.
pub fn encode_key(spec: &str) -> Result<Vec<u8>> {
    let trimmed = spec.trim();
    if trimmed.is_empty() {
        bail!("empty key specification");
    }

    // Strip optional `<...>` wrapper (e.g. `<C-s>`, `<F5>`, `<Space>`).
    let inner = trimmed
        .strip_prefix('<')
        .and_then(|s| s.strip_suffix('>'))
        .unwrap_or(trimmed);

    // Single literal character (including uppercase/symbols like `G`, `A`, `?`, `.`, `/`)
    if inner.chars().count() == 1 {
        let ch = inner.chars().next().unwrap();
        let mut buf = [0u8; 4];
        return Ok(ch.encode_utf8(&mut buf).as_bytes().to_vec());
    }

    let lower = inner.to_ascii_lowercase();
    if let Some(seq) = match_named_key(&lower) {
        return Ok(seq);
    }

    // Modifier combos: `Ctrl+S`, `C-s`, `Alt+r`, `A-r`, `Shift+Tab`, `S-Tab`, `Ctrl+Alt+N`
    let (ctrl, alt, shift, base) = parse_modifiers(inner)?;
    encode_modified_key(ctrl, alt, shift, &base)
}

fn match_named_key(lower: &str) -> Option<Vec<u8>> {
    Some(match lower {
        "enter" | "return" | "cr" => b"\r".to_vec(),
        "esc" | "escape" => b"\x1b".to_vec(),
        "tab" => b"\t".to_vec(),
        "backtab" | "shift+tab" | "s-tab" => b"\x1b[Z".to_vec(),
        "space" => b" ".to_vec(),
        "backspace" | "bs" => vec![0x7f],
        "up" => b"\x1b[A".to_vec(),
        "down" => b"\x1b[B".to_vec(),
        "right" => b"\x1b[C".to_vec(),
        "left" => b"\x1b[D".to_vec(),
        "home" => b"\x1b[H".to_vec(),
        "end" => b"\x1b[F".to_vec(),
        "insert" | "ins" => b"\x1b[2~".to_vec(),
        "del" | "delete" => b"\x1b[3~".to_vec(),
        "pgup" | "pageup" => b"\x1b[5~".to_vec(),
        "pgdn" | "pagedown" => b"\x1b[6~".to_vec(),
        "f1" => b"\x1bOP".to_vec(),
        "f2" => b"\x1bOQ".to_vec(),
        "f3" => b"\x1bOR".to_vec(),
        "f4" => b"\x1bOS".to_vec(),
        "f5" => b"\x1b[15~".to_vec(),
        "f6" => b"\x1b[17~".to_vec(),
        "f7" => b"\x1b[18~".to_vec(),
        "f8" => b"\x1b[19~".to_vec(),
        "f9" => b"\x1b[20~".to_vec(),
        "f10" => b"\x1b[21~".to_vec(),
        "f11" => b"\x1b[23~".to_vec(),
        "f12" => b"\x1b[24~".to_vec(),
        _ => return None,
    })
}

fn parse_modifiers(spec: &str) -> Result<(bool, bool, bool, String)> {
    let sep = if spec.contains('+') { '+' } else { '-' };
    let parts: Vec<&str> = spec.split(sep).collect();
    if parts.len() < 2 {
        bail!("unknown key '{spec}'");
    }

    let mut ctrl = false;
    let mut alt = false;
    let mut shift = false;
    for &p in &parts[..parts.len() - 1] {
        match p.to_ascii_lowercase().as_str() {
            "c" | "ctrl" | "control" => ctrl = true,
            "a" | "alt" | "m" | "meta" => alt = true,
            "s" | "shift" => shift = true,
            other => bail!("unknown modifier '{other}' in '{spec}'"),
        }
    }
    Ok((ctrl, alt, shift, parts[parts.len() - 1].to_string()))
}

fn encode_modified_key(ctrl: bool, alt: bool, shift: bool, base: &str) -> Result<Vec<u8>> {
    let lower = base.to_ascii_lowercase();

    // Special named base keys with modifiers
    let xterm_mod = 1 + u8::from(shift) + (u8::from(alt) * 2) + (u8::from(ctrl) * 4);
    match lower.as_str() {
        "tab" if shift && !ctrl && !alt => return Ok(b"\x1b[Z".to_vec()),
        "up" => return Ok(format!("\x1b[1;{xterm_mod}A").into_bytes()),
        "down" => return Ok(format!("\x1b[1;{xterm_mod}B").into_bytes()),
        "right" => return Ok(format!("\x1b[1;{xterm_mod}C").into_bytes()),
        "left" => return Ok(format!("\x1b[1;{xterm_mod}D").into_bytes()),
        "home" => return Ok(format!("\x1b[1;{xterm_mod}H").into_bytes()),
        "end" => return Ok(format!("\x1b[1;{xterm_mod}F").into_bytes()),
        "f1" | "f2" | "f3" | "f4" => {
            let suffix = match lower.as_str() {
                "f1" => 'P',
                "f2" => 'Q',
                "f3" => 'R',
                "f4" => 'S',
                _ => unreachable!(),
            };
            return Ok(format!("\x1b[1;{xterm_mod}{suffix}").into_bytes());
        }
        "insert" | "ins" | "del" | "delete" | "pgup" | "pageup" | "pgdn" | "pagedown"
        | "f5" | "f6" | "f7" | "f8" | "f9" | "f10" | "f11" | "f12" => {
            let code = match lower.as_str() {
                "insert" | "ins" => 2,
                "del" | "delete" => 3,
                "pgup" | "pageup" => 5,
                "pgdn" | "pagedown" => 6,
                "f5" => 15,
                "f6" => 17,
                "f7" => 18,
                "f8" => 19,
                "f9" => 20,
                "f10" => 21,
                "f11" => 23,
                "f12" => 24,
                _ => unreachable!(),
            };
            return Ok(format!("\x1b[{code};{xterm_mod}~").into_bytes());
        }
        "backspace" | "bs" => {
            if alt {
                return Ok(b"\x1b\x7f".to_vec());
            }
            if ctrl {
                return Ok(vec![0x08]);
            }
            return Ok(vec![0x7f]);
        }
        "enter" | "return" => {
            if ctrl {
                // CSI u encoding for Ctrl+Enter; fallback LF if raw
                return Ok(b"\x1b[13;5u".to_vec());
            }
            if alt {
                return Ok(b"\x1b\r".to_vec());
            }
            return Ok(b"\r".to_vec());
        }
        _ => {}
    }

    if base.chars().count() == 1 {
        let ch = base.chars().next().unwrap();
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
        return Ok(out);
    }

    bail!("unsupported modified key base '{base}'")
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
    fn encodes_sgr_hover_coordinates() {
        assert_eq!(encode_sgr_hover(10, 5), b"\x1b[<35;11;6M");
    }
}
