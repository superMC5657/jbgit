//! Terminal pixel-dimension and multiplexer passthrough probes.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::graph::renderer::CellPixelSize;

/// Timeout for background WezTerm CLI pane dimension probing.
pub(crate) const WEZTERM_PROBE_TIMEOUT: Duration = Duration::from_millis(150);

/// Terminal window dimensions in both character cells and pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowSizePx {
    pub columns: u16,
    pub rows: u16,
    pub width_px: u16,
    pub height_px: u16,
}

impl WindowSizePx {
    /// Derives a usable [`CellPixelSize`] from these window dimensions.
    pub fn cell_pixel_size(self) -> Option<CellPixelSize> {
        CellPixelSize::from_window(self.columns, self.rows, self.width_px, self.height_px)
    }
}

/// Runs `cmd` with null stdin/stderr and drains stdout concurrently on a
/// reader thread so large outputs cannot deadlock on OS pipe buffers.
fn run_command_with_timeout(cmd: &mut Command, timeout: Duration) -> Option<Vec<u8>> {
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout.read_to_end(&mut buf);
        buf
    });
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let out = reader.join().ok()?;
                return status.success().then_some(out);
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = reader.join();
                    return None;
                }
                std::thread::sleep(Duration::from_millis(2));
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = reader.join();
                return None;
            }
        }
    }
}

/// Queries `tmux` for `allow-passthrough` (`on` or `all`) within `timeout`.
pub(crate) fn query_tmux_passthrough(timeout: Duration) -> bool {
    let mut cmd = Command::new("tmux");
    cmd.args(["display-message", "-p", "#{allow-passthrough}"]);
    let Some(stdout) = run_command_with_timeout(&mut cmd, timeout) else {
        return false;
    };
    let val = String::from_utf8_lossy(&stdout);
    let trimmed = val.trim();
    trimmed == "on" || trimmed == "all"
}

fn wezterm_cli_binary() -> PathBuf {
    if let Some(exe) = std::env::var_os("WEZTERM_EXECUTABLE") {
        let cli_name = if cfg!(windows) {
            "wezterm.exe"
        } else {
            "wezterm"
        };
        let candidate = Path::new(&exe).with_file_name(cli_name);
        if candidate.exists() {
            return candidate;
        }
    }
    PathBuf::from("wezterm")
}

/// Returns `true` when the environment identifies a local WezTerm pane whose
/// pixel size can be queried via `wezterm cli list --format json`.
pub(crate) fn can_probe_wezterm_env() -> bool {
    can_probe_wezterm_env_with(|k| std::env::var(k).ok(), |p| p.exists())
}

fn can_probe_wezterm_env_with(
    env: impl Fn(&str) -> Option<String>,
    path_exists: impl Fn(&Path) -> bool,
) -> bool {
    if env("TERM_PROGRAM").as_deref() != Some("WezTerm") {
        return false;
    }
    let Some(pane_id) = env("WEZTERM_PANE").and_then(|s| s.trim().parse::<u64>().ok()) else {
        return false;
    };
    let _ = pane_id;
    if let Some(sock) = env("WEZTERM_UNIX_SOCKET") {
        return !sock.is_empty() && path_exists(Path::new(&sock));
    }
    true
}

/// Queries `wezterm cli list --format json` for the current pane's cell size
/// when the OS PTY (`crossterm::terminal::window_size`) reports zero pixels.
pub(crate) fn query_wezterm_cell_size(timeout: Duration) -> Option<CellPixelSize> {
    if !can_probe_wezterm_env() {
        return None;
    }
    let target_pane = std::env::var("WEZTERM_PANE")
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()?;
    let mut cmd = Command::new(wezterm_cli_binary());
    cmd.args(["cli", "list", "--format", "json"]);
    let stdout = run_command_with_timeout(&mut cmd, timeout)?;
    let text = String::from_utf8_lossy(&stdout);
    parse_wezterm_cli_list_window(&text, target_pane)?.cell_pixel_size()
}

#[derive(Default)]
struct WezTermPaneMetrics {
    pane_id: Option<u64>,
    cols: Option<u16>,
    rows: Option<u16>,
    pixel_width: Option<u16>,
    pixel_height: Option<u16>,
}

/// Extracts [`WindowSizePx`] from `wezterm cli list --format json` output for
/// the exact `target_pane_id`, validating via [`WindowSizePx::cell_pixel_size`].
pub(crate) fn parse_wezterm_cli_list_window(
    json: &str,
    target_pane_id: u64,
) -> Option<WindowSizePx> {
    for obj in json_array_objects(json) {
        let pane = parse_wezterm_pane_object(obj);
        if pane.pane_id == Some(target_pane_id) {
            let (Some(columns), Some(rows), Some(width_px), Some(height_px)) =
                (pane.cols, pane.rows, pane.pixel_width, pane.pixel_height)
            else {
                return None;
            };
            let win = WindowSizePx {
                columns,
                rows,
                width_px,
                height_px,
            };
            return win.cell_pixel_size().map(|_| win);
        }
    }
    None
}

fn scan_json_string(s: &str, quote_idx: usize) -> Option<(&str, usize)> {
    let bytes = s.as_bytes();
    let start = quote_idx + 1;
    let mut i = start;
    let mut escaped = false;
    while i < bytes.len() {
        let b = bytes[i];
        if escaped {
            escaped = false;
        } else if b == b'\\' {
            escaped = true;
        } else if b == b'"' {
            return Some((&s[start..i], i + 1));
        }
        i += 1;
    }
    None
}

fn skip_json_ws(bytes: &[u8], mut i: usize) -> usize {
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    i
}

fn json_array_objects(json: &str) -> Vec<&str> {
    let bytes = json.as_bytes();
    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut start: Option<usize> = None;
    let mut i = 0;

    while i < bytes.len() {
        match bytes[i] {
            b'"' => match scan_json_string(json, i) {
                Some((_, next)) => i = next,
                None => break,
            },
            b'{' => {
                if depth == 0 {
                    start = Some(i);
                }
                depth += 1;
                i += 1;
            }
            b'}' if depth > 0 => {
                depth -= 1;
                if depth == 0
                    && let Some(s) = start.take()
                {
                    out.push(&json[s..=i]);
                }
                i += 1;
            }
            _ => i += 1,
        }
    }
    out
}

fn parse_wezterm_pane_object(obj: &str) -> WezTermPaneMetrics {
    let bytes = obj.as_bytes();
    let mut metrics = WezTermPaneMetrics::default();
    let mut depth = 0usize;
    let mut in_size_obj = false;
    let mut has_size_obj = false;
    let mut i = 0;

    while i < bytes.len() {
        match bytes[i] {
            b'{' => {
                depth += 1;
                i += 1;
            }
            b'}' => {
                if depth > 0 {
                    depth -= 1;
                    if depth <= 1 {
                        in_size_obj = false;
                    }
                }
                i += 1;
            }
            b'"' => {
                let Some((key, after_str)) = scan_json_string(obj, i) else {
                    break;
                };
                let after_ws = skip_json_ws(bytes, after_str);
                if after_ws >= bytes.len() || bytes[after_ws] != b':' {
                    i = after_ws;
                    continue;
                }
                let val_idx = skip_json_ws(bytes, after_ws + 1);
                if depth == 1 && key == "size" && val_idx < bytes.len() && bytes[val_idx] == b'{' {
                    depth += 1;
                    in_size_obj = true;
                    has_size_obj = true;
                    i = val_idx + 1;
                    continue;
                }
                i = val_idx;
                let accept_pane_id = key == "pane_id" && depth == 1;
                let accept_dim = matches!(key, "cols" | "rows" | "pixel_width" | "pixel_height")
                    && ((in_size_obj && depth == 2) || (!has_size_obj && depth == 1));
                if !accept_pane_id && !accept_dim {
                    continue;
                }
                let val_start = i;
                while i < bytes.len() && bytes[i].is_ascii_digit() {
                    i += 1;
                }
                if i == val_start || (i < bytes.len() && matches!(bytes[i], b'.' | b'e' | b'E')) {
                    continue;
                }
                let Ok(val) = obj[val_start..i].parse::<u64>() else {
                    continue;
                };
                match key {
                    "pane_id" => metrics.pane_id = Some(val),
                    "cols" => metrics.cols = u16::try_from(val).ok(),
                    "rows" => metrics.rows = u16::try_from(val).ok(),
                    "pixel_width" => metrics.pixel_width = u16::try_from(val).ok(),
                    "pixel_height" => metrics.pixel_height = u16::try_from(val).ok(),
                    _ => {}
                }
            }
            _ => i += 1,
        }
    }
    metrics
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_of<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |k| {
            pairs
                .iter()
                .find(|(key, _)| *key == k)
                .map(|(_, v)| (*v).to_string())
        }
    }

    #[test]
    fn parses_wezterm_cli_list_window_exact_pane_and_rejects_unusable() {
        let json = r#"[
  {
    "window_id": 0,
    "tab_id": 0,
    "pane_id": 2,
    "title": "\"cols\": 99, \"pixel_width\": 999, {braces}",
    "extra": {
      "pane_id": 5,
      "cols": 999,
      "rows": 999,
      "pixel_width": 9999,
      "pixel_height": 9999
    },
    "size": {
      "rows": 42,
      "cols": 160,
      "pixel_width": 2400,
      "pixel_height": 1344,
      "dpi": 96
    }
  },
  {
    "window_id": 1,
    "tab_id": 0,
    "pane_id": 5,
    "size": {
      "rows": 30,
      "cols": 100,
      "pixel_width": 1800,
      "pixel_height": 1080
    }
  }
]"#;
        // Matches exact pane_id, ignores quoted strings and colliding keys in nested objects.
        assert_eq!(
            parse_wezterm_cli_list_window(json, 5),
            Some(WindowSizePx {
                columns: 100,
                rows: 30,
                width_px: 1800,
                height_px: 1080,
            })
        );
        assert_eq!(
            parse_wezterm_cli_list_window(json, 2),
            Some(WindowSizePx {
                columns: 160,
                rows: 42,
                width_px: 2400,
                height_px: 1344,
            })
        );
        // Never falls back to another pane when target_pane_id is absent.
        assert_eq!(parse_wezterm_cli_list_window(json, 999), None);

        // Rejects zero-pixel and unusable cell sizes (100x40 in 100x100px = 1x2px/cell).
        let zero_json = r#"[{"pane_id": 0, "size": {"rows": 40, "cols": 120, "pixel_width": 0, "pixel_height": 0}}]"#;
        assert_eq!(parse_wezterm_cli_list_window(zero_json, 0), None);
        let tiny_json = r#"[{"pane_id": 0, "size": {"rows": 40, "cols": 100, "pixel_width": 100, "pixel_height": 100}}]"#;
        assert_eq!(parse_wezterm_cli_list_window(tiny_json, 0), None);
    }

    #[test]
    fn wezterm_env_probe_requires_wezterm_program_numeric_pane_and_existing_socket() {
        assert!(can_probe_wezterm_env_with(
            env_of(&[("TERM_PROGRAM", "WezTerm"), ("WEZTERM_PANE", "3")]),
            |_| true,
        ));
        assert!(can_probe_wezterm_env_with(
            env_of(&[
                ("TERM_PROGRAM", "WezTerm"),
                ("WEZTERM_PANE", "3"),
                ("WEZTERM_UNIX_SOCKET", "/tmp/wez.sock"),
            ]),
            |p| p == Path::new("/tmp/wez.sock"),
        ));
        // Inherited WEZTERM_PANE inside Windows Terminal must not trigger WezTerm probing.
        assert!(!can_probe_wezterm_env_with(
            env_of(&[
                ("TERM_PROGRAM", "WindowsTerminal"),
                ("WT_SESSION", "abc"),
                ("WEZTERM_PANE", "3"),
            ]),
            |_| true,
        ));
        assert!(!can_probe_wezterm_env_with(
            env_of(&[("TERM_PROGRAM", "WezTerm")]),
            |_| true,
        ));
        assert!(!can_probe_wezterm_env_with(
            env_of(&[("TERM_PROGRAM", "WezTerm"), ("WEZTERM_PANE", "abc")]),
            |_| true,
        ));
        assert!(!can_probe_wezterm_env_with(
            env_of(&[
                ("TERM_PROGRAM", "WezTerm"),
                ("WEZTERM_PANE", "3"),
                ("WEZTERM_UNIX_SOCKET", "/tmp/missing.sock"),
            ]),
            |_| false,
        ));
    }
}
