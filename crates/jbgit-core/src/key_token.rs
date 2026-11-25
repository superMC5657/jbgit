//! Shared symbolic key-token parser used by both `jbgit-tui` (`keymap.toml`)
//! and `jbgit-pilot` (PTY key-sequence encoding).

use thiserror::Error;

/// Base key identity independent of terminal/PTY representation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyTokenKind {
    Char(char),
    Enter,
    Esc,
    Tab,
    BackTab,
    Backspace,
    Insert,
    Delete,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    F(u8),
}

/// Normalized symbolic keystroke parsed from strings such as `"<C-d>"`,
/// `"Ctrl+Shift+K"`, `"<Space>"`, `"<CR>"`, `"<S-F10>"`, `"Shift+Tab"`, or `"G"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KeyToken {
    pub kind: KeyTokenKind,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum KeyTokenError {
    #[error("empty key specification")]
    Empty,
    #[error("unknown modifier '{modifier}' in '{spec}'")]
    UnknownModifier { modifier: String, spec: String },
    #[error("unknown key '{spec}'")]
    UnknownKey { spec: String },
}

fn parse_named_key(name: &str) -> Option<KeyTokenKind> {
    match name.to_ascii_lowercase().as_str() {
        "enter" | "return" | "cr" => Some(KeyTokenKind::Enter),
        "esc" | "escape" => Some(KeyTokenKind::Esc),
        "space" => Some(KeyTokenKind::Char(' ')),
        "tab" => Some(KeyTokenKind::Tab),
        "backtab" => Some(KeyTokenKind::BackTab),
        "backspace" | "bs" => Some(KeyTokenKind::Backspace),
        "ins" | "insert" => Some(KeyTokenKind::Insert),
        "del" | "delete" => Some(KeyTokenKind::Delete),
        "up" => Some(KeyTokenKind::Up),
        "down" => Some(KeyTokenKind::Down),
        "left" => Some(KeyTokenKind::Left),
        "right" => Some(KeyTokenKind::Right),
        "home" => Some(KeyTokenKind::Home),
        "end" => Some(KeyTokenKind::End),
        "pageup" | "pgup" => Some(KeyTokenKind::PageUp),
        "pagedown" | "pgdn" => Some(KeyTokenKind::PageDown),
        f if f.starts_with('f') && f.len() > 1 && f[1..].chars().all(|c| c.is_ascii_digit()) => {
            let n = f[1..].parse::<u8>().ok()?;
            (1..=24).contains(&n).then_some(KeyTokenKind::F(n))
        }
        _ => None,
    }
}

fn parse_base_key(base: &str) -> Option<KeyTokenKind> {
    if let Some(named) = parse_named_key(base) {
        return Some(named);
    }
    let mut chars = base.chars();
    let first = chars.next()?;
    chars.next().is_none().then_some(KeyTokenKind::Char(first))
}

fn normalize_token(kind: KeyTokenKind, ctrl: bool, alt: bool, shift: bool) -> KeyToken {
    if (kind == KeyTokenKind::Tab && shift && !ctrl && !alt) || kind == KeyTokenKind::BackTab {
        return KeyToken {
            kind: KeyTokenKind::BackTab,
            ctrl,
            alt,
            shift: false,
        };
    }
    if !ctrl
        && !alt
        && let KeyTokenKind::Char(c) = kind
    {
        if shift && c.is_ascii_lowercase() {
            return KeyToken {
                kind: KeyTokenKind::Char(c.to_ascii_uppercase()),
                ctrl: false,
                alt: false,
                shift: true,
            };
        }
        if c.is_ascii_uppercase() {
            return KeyToken {
                kind: KeyTokenKind::Char(c),
                ctrl: false,
                alt: false,
                shift: true,
            };
        }
    }
    KeyToken {
        kind,
        ctrl,
        alt,
        shift,
    }
}

/// Parses a single symbolic key specification into a normalized [`KeyToken`].
///
/// Accepts both angle-bracketed `keymap.toml` tokens (`<C-d>`, `<C-S-k>`, `<A-1>`,
/// `<Space>`, `<CR>`, `<BackTab>`, `<F8>`, `<S-F10>`) and bare pilot notation
/// (`Enter`, `Ctrl+S`, `Shift+Tab`, `Alt+Backspace`, `G`, `?`).
pub fn parse_key_token(spec: &str) -> Result<KeyToken, KeyTokenError> {
    let trimmed = spec.trim();
    if trimmed.is_empty() {
        return Err(KeyTokenError::Empty);
    }

    let inner = if trimmed.starts_with('<') && trimmed.ends_with('>') && trimmed.len() >= 3 {
        &trimmed[1..trimmed.len() - 1]
    } else {
        trimmed
    };

    if inner.is_empty() {
        return Err(KeyTokenError::Empty);
    }

    // Fast path: single literal character or unmodified named key.
    if let Some(kind) = parse_base_key(inner) {
        return Ok(normalize_token(kind, false, false, false));
    }

    let mut ctrl = false;
    let mut alt = false;
    let mut shift = false;
    let mut rest = inner;

    while let Some(sep_idx) = rest.find(['+', '-']) {
        if sep_idx == 0 {
            break;
        }
        let prefix = &rest[..sep_idx];
        let tail = &rest[sep_idx + 1..];
        if tail.is_empty() {
            break;
        }
        match prefix.to_ascii_lowercase().as_str() {
            "c" | "ctrl" | "control" => ctrl = true,
            "a" | "alt" | "m" | "meta" => alt = true,
            "s" | "shift" => shift = true,
            other => {
                return Err(KeyTokenError::UnknownModifier {
                    modifier: other.to_string(),
                    spec: trimmed.to_string(),
                });
            }
        }
        rest = tail;
        if parse_base_key(rest).is_some() {
            break;
        }
    }

    if !(ctrl || alt || shift) {
        return Err(KeyTokenError::UnknownKey {
            spec: trimmed.to_string(),
        });
    }

    let kind = parse_base_key(rest).ok_or_else(|| KeyTokenError::UnknownKey {
        spec: trimmed.to_string(),
    })?;
    Ok(normalize_token(kind, ctrl, alt, shift))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bracketed_and_bare_equivalents_identically() {
        assert_eq!(parse_key_token("<C-d>"), parse_key_token("Ctrl+d"));
        assert_eq!(parse_key_token("<C-S-k>"), parse_key_token("Ctrl+Shift+k"));
        assert_eq!(parse_key_token("<C-A-n>"), parse_key_token("Ctrl+Alt+n"));
        assert_eq!(parse_key_token("<S-Tab>"), parse_key_token("Shift+Tab"));
        assert_eq!(parse_key_token("<S-Tab>"), parse_key_token("<BackTab>"));
        assert_eq!(parse_key_token("<S-x>"), parse_key_token("X"));
        assert_eq!(parse_key_token("<CR>"), parse_key_token("Enter"));
        assert_eq!(parse_key_token("<Esc>"), parse_key_token("escape"));
        assert_eq!(parse_key_token("<Space>"), parse_key_token("Space"));
        assert_eq!(parse_key_token("<S-F10>"), parse_key_token("Shift+F10"));
        assert_eq!(
            parse_key_token("<C-PgDn>"),
            parse_key_token("Ctrl+PageDown")
        );
        assert_eq!(parse_key_token("<A-BS>"), parse_key_token("Alt+Backspace"));
    }

    #[test]
    fn handles_punctuation_and_rejects_invalid_tokens() {
        assert_eq!(
            parse_key_token("<").unwrap(),
            KeyToken {
                kind: KeyTokenKind::Char('<'),
                ctrl: false,
                alt: false,
                shift: false,
            }
        );
        assert_eq!(
            parse_key_token("-").unwrap(),
            KeyToken {
                kind: KeyTokenKind::Char('-'),
                ctrl: false,
                alt: false,
                shift: false,
            }
        );
        assert_eq!(
            parse_key_token("<C-->").unwrap(),
            KeyToken {
                kind: KeyTokenKind::Char('-'),
                ctrl: true,
                alt: false,
                shift: false,
            }
        );
        assert_eq!(
            parse_key_token("Ctrl++").unwrap(),
            KeyToken {
                kind: KeyTokenKind::Char('+'),
                ctrl: true,
                alt: false,
                shift: false,
            }
        );
        assert!(parse_key_token("").is_err());
        assert!(parse_key_token("<Bogus-a>").is_err());
        assert!(parse_key_token("NoSuchKey").is_err());
    }
}
