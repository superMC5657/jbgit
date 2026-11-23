//! Read-only system clipboard access for `Apply Patch from Clipboard`.
//!
//! The TUI writes the clipboard with OSC 52, which terminals do not let
//! applications read back, so the engine shells out to the platform tool:
//! `Get-Clipboard` (Windows), `pbpaste` (macOS), `wl-paste` / `xclip` /
//! `xsel` (Wayland / X11). `JBGIT_CLIPBOARD_FILE` overrides the source with a
//! file (tests and the `jbgit-pilot` sandbox).

use std::process::{Command, Stdio};

/// Environment override: read the "clipboard" from this file instead.
pub const CLIPBOARD_FILE_ENV: &str = "JBGIT_CLIPBOARD_FILE";

/// Reads the clipboard as bytes for `git apply` stdin.
///
/// Uniform CRLF line endings (Windows clipboard conversion) are folded to LF;
/// mixed endings are kept as-is.
pub fn read_clipboard() -> Result<Vec<u8>, String> {
    let raw = read_raw()?;
    if raw.iter().all(|b| b.is_ascii_whitespace()) {
        return Err("the clipboard is empty".to_string());
    }
    Ok(normalize_crlf(raw))
}

fn read_raw() -> Result<Vec<u8>, String> {
    if let Some(path) = std::env::var_os(CLIPBOARD_FILE_ENV) {
        return std::fs::read(&path)
            .map_err(|e| format!("cannot read {}: {e}", std::path::Path::new(&path).display()));
    }
    let mut last_err = String::from("no clipboard tool found");
    for (program, args) in candidates() {
        let mut cmd = Command::new(program);
        cmd.args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        match cmd.output() {
            Ok(out) if out.status.success() => return Ok(out.stdout),
            Ok(out) => {
                last_err = format!("{program}: {}", String::from_utf8_lossy(&out.stderr).trim());
            }
            Err(e) => last_err = format!("{program}: {e}"),
        }
    }
    Err(last_err)
}

fn candidates() -> Vec<(&'static str, &'static [&'static str])> {
    if cfg!(windows) {
        vec![(
            "powershell.exe",
            &[
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "[Console]::OutputEncoding=[System.Text.Encoding]::UTF8; Get-Clipboard -Raw",
            ],
        )]
    } else if cfg!(target_os = "macos") {
        vec![("pbpaste", &[])]
    } else {
        vec![
            ("wl-paste", &["--no-newline"]),
            ("xclip", &["-selection", "clipboard", "-o"]),
            ("xsel", &["--clipboard", "--output"]),
        ]
    }
}

fn normalize_crlf(raw: Vec<u8>) -> Vec<u8> {
    let lf = raw.iter().filter(|&&b| b == b'\n').count();
    let crlf = raw.windows(2).filter(|w| w == b"\r\n").count();
    if lf == 0 || crlf != lf {
        return raw;
    }
    let mut out = Vec::with_capacity(raw.len());
    let mut iter = raw.into_iter().peekable();
    while let Some(b) = iter.next() {
        if b == b'\r' && iter.peek() == Some(&b'\n') {
            continue;
        }
        out.push(b);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uniform_crlf_folds_and_mixed_endings_stay() {
        assert_eq!(normalize_crlf(b"a\r\nb\r\n".to_vec()), b"a\nb\n");
        assert_eq!(normalize_crlf(b"a\r\nb\n".to_vec()), b"a\r\nb\n");
        assert_eq!(normalize_crlf(b"a\nb\n".to_vec()), b"a\nb\n");
    }
}
