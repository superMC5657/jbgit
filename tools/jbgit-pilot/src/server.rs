use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Write};
use std::num::NonZeroU16;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use portable_pty::{CommandBuilder, PtySize, native_pty_system};

use crate::keys::{encode_key, encode_sgr_click, encode_sgr_hover, encode_sgr_scroll};
use crate::protocol::{
    CellPos, ColorCount, ColorPair, CommandEnvelope, PilotAction, ResponseEnvelope, RowSpanInfo,
    TextMatch,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SandboxScenario {
    Basic,
    Full,
    Conflict,
}

impl SandboxScenario {
    pub fn parse(s: &str) -> Result<Self> {
        match s.to_ascii_lowercase().as_str() {
            "basic" => Ok(Self::Basic),
            "full" => Ok(Self::Full),
            "conflict" => Ok(Self::Conflict),
            other => bail!("unknown scenario '{other}' (expected: full, conflict, basic)"),
        }
    }
}

pub struct ServeOptions {
    pub session_dir: PathBuf,
    pub bin_path: PathBuf,
    pub repo_path: Option<PathBuf>,
    pub sandbox: bool,
    pub scenario: SandboxScenario,
    pub custom_config: bool,
    pub use_user_config: bool,
    pub config_dir: Option<PathBuf>,
    pub cols: u16,
    pub rows: u16,
    pub truecolor: bool,
}

struct SharedPtyState {
    parser: vt100::Parser,
    last_output: Instant,
    cols: u16,
    rows: u16,
    eof: bool,
}

pub fn run_serve(opts: ServeOptions) -> Result<()> {
    fs::create_dir_all(&opts.session_dir)
        .with_context(|| format!("create session dir {:?}", opts.session_dir))?;

    let cmd_path = opts.session_dir.join("cmd.json");
    let resp_path = opts.session_dir.join("resp.json");
    let ready_path = opts.session_dir.join("ready.json");
    let starting_path = opts.session_dir.join("starting.json");
    let _ = fs::remove_file(&cmd_path);
    let _ = fs::remove_file(&resp_path);
    let _ = fs::remove_file(&ready_path);
    fs::write(
        &starting_path,
        serde_json::to_string(&serde_json::json!({ "pid": std::process::id() }))?,
    )?;

    let repo_path = if opts.sandbox {
        let sandbox_dir = std::path::absolute(opts.session_dir.join("sandbox-repo"))?;
        let remote_dir = std::path::absolute(opts.session_dir.join("sandbox-remote.git"))?;
        init_sandbox_repo(&sandbox_dir, &remote_dir, opts.scenario)?;
        sandbox_dir
    } else if let Some(p) = opts.repo_path.clone() {
        std::path::absolute(p)?
    } else {
        std::env::current_dir()?
    };

    let resolved_config_dir: Option<PathBuf> = if let Some(cfg) = &opts.config_dir {
        Some(std::path::absolute(cfg)?)
    } else if opts.custom_config {
        let cfg_dir = std::path::absolute(opts.session_dir.join("custom-config"))?;
        write_custom_test_config(&cfg_dir)?;
        Some(cfg_dir)
    } else if opts.sandbox && !opts.use_user_config {
        let empty_dir = std::path::absolute(opts.session_dir.join("empty-config"))?;
        if empty_dir.exists() {
            fs::remove_dir_all(&empty_dir)?;
        }
        fs::create_dir_all(&empty_dir)?;
        Some(empty_dir)
    } else {
        None
    };

    let bin_path = std::path::absolute(&opts.bin_path)?;
    if !bin_path.is_file() {
        bail!(
            "jbgit binary not found at {:?} (run `cargo build -p jbgit-tui` first or pass --bin)",
            bin_path
        );
    }

    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows: opts.rows,
            cols: opts.cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .context("open PTY")?;

    let mut cmd = CommandBuilder::new(&bin_path);
    cmd.arg(&repo_path);
    cmd.cwd(&repo_path);
    if opts.truecolor {
        cmd.env("COLORTERM", "truecolor");
    }
    if let Some(cfg) = &resolved_config_dir {
        cmd.env("JBGIT_CONFIG_DIR", cfg);
        if !opts.use_user_config {
            cmd.env("APPDATA", cfg);
            cmd.env("XDG_CONFIG_HOME", cfg);
        }
    }

    let mut child = pair
        .slave
        .spawn_command(cmd)
        .with_context(|| format!("spawn {:?}", bin_path))?;
    drop(pair.slave);

    let mut reader = pair.master.try_clone_reader().context("clone PTY reader")?;
    let mut writer = pair.master.take_writer().context("take PTY writer")?;

    let nz_rows = NonZeroU16::new(opts.rows.max(1)).unwrap();
    let nz_cols = NonZeroU16::new(opts.cols.max(1)).unwrap();

    let shared = Arc::new(Mutex::new(SharedPtyState {
        parser: vt100::Parser::new(nz_rows, nz_cols, 0),
        last_output: Instant::now(),
        cols: opts.cols.max(1),
        rows: opts.rows.max(1),
        eof: false,
    }));

    let log_path = opts.session_dir.join("pty_raw.log");
    let reader_log_path = log_path.clone();
    let shared_reader = Arc::clone(&shared);
    thread::spawn(move || {
        let mut log_file = fs::File::create(&reader_log_path).ok();
        let mut buf = [0u8; 8192];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => {
                    if let Ok(mut guard) = shared_reader.lock() {
                        guard.eof = true;
                    }
                    break;
                }
                Ok(n) => {
                    if let Some(f) = log_file.as_mut() {
                        let _ = f.write_all(&buf[..n]);
                        let _ = f.flush();
                    }
                    if let Ok(mut guard) = shared_reader.lock() {
                        guard.parser.process(&buf[..n]);
                        guard.last_output = Instant::now();
                    }
                }
            }
        }
    });

    // Wait for initial TUI paint
    wait_for_initial_paint(&shared, 300, 8000);

    let ready_info = serde_json::json!({
        "pid": std::process::id(),
        "repo": repo_path,
        "bin": bin_path,
        "config_dir": resolved_config_dir,
        "scenario": format!("{:?}", opts.scenario).to_ascii_lowercase(),
        "cols": opts.cols,
        "rows": opts.rows,
    });
    fs::write(&ready_path, serde_json::to_string_pretty(&ready_info)?)?;
    let _ = fs::remove_file(&starting_path);
    eprintln!(
        "jbgit-pilot serving (repo={}, config={}, size={}x{}, session={})",
        repo_path.display(),
        resolved_config_dir
            .as_ref()
            .map_or_else(|| "<default>".to_string(), |p| p.display().to_string()),
        opts.cols,
        opts.rows,
        opts.session_dir.display()
    );

    let mut exit_code: Option<i32> = None;

    loop {
        if exit_code.is_none()
            && let Ok(Some(status)) = child.try_wait()
        {
            exit_code = Some(status.exit_code() as i32);
        }

        if cmd_path.is_file() {
            let raw = match fs::read_to_string(&cmd_path) {
                Ok(s) => s,
                Err(_) => {
                    thread::sleep(Duration::from_millis(10));
                    continue;
                }
            };
            let _ = fs::remove_file(&cmd_path);

            let env: CommandEnvelope = match serde_json::from_str(&raw) {
                Ok(v) => v,
                Err(e) => {
                    let resp =
                        make_error_resp(0, format!("invalid command json: {e}"), &shared, exit_code);
                    write_resp_atomic(&opts.session_dir, &resp_path, &resp)?;
                    continue;
                }
            };

            let is_stop = matches!(env.action, PilotAction::Stop);
            let resp = handle_action(
                env,
                &mut writer,
                &*pair.master,
                &shared,
                &mut child,
                &mut exit_code,
                &log_path,
            );
            write_resp_atomic(&opts.session_dir, &resp_path, &resp)?;

            if is_stop {
                break;
            }
        }

        thread::sleep(Duration::from_millis(10));
    }

    let _ = fs::remove_file(&ready_path);
    let _ = fs::remove_file(&starting_path);
    Ok(())
}

fn write_resp_atomic(session_dir: &Path, resp_path: &Path, resp: &ResponseEnvelope) -> Result<()> {
    let tmp = session_dir.join("resp.json.tmp");
    fs::write(&tmp, serde_json::to_vec_pretty(resp)?)?;
    if resp_path.exists() {
        let _ = fs::remove_file(resp_path);
    }
    fs::rename(&tmp, resp_path)?;
    Ok(())
}

fn handle_action(
    env: CommandEnvelope,
    writer: &mut Box<dyn Write + Send>,
    master: &dyn portable_pty::MasterPty,
    shared: &Arc<Mutex<SharedPtyState>>,
    child: &mut Box<dyn portable_pty::Child + Send + Sync>,
    exit_code: &mut Option<i32>,
    log_path: &Path,
) -> ResponseEnvelope {
    let id = env.id;
    let res: Result<ResponseEnvelope> = (|| match env.action {
        PilotAction::Screen { settle_ms } => {
            wait_quiet(shared, settle_ms, 2000);
            Ok(build_screen_resp(id, shared, *exit_code))
        }
        PilotAction::Keys { keys, settle_ms } => {
            let sent_at = Instant::now();
            for k in &keys {
                let bytes = encode_key(k)?;
                writer.write_all(&bytes)?;
            }
            writer.flush()?;
            wait_after_input(shared, sent_at, settle_ms, 2500);
            refresh_exit(child, exit_code);
            Ok(build_screen_resp(id, shared, *exit_code))
        }
        PilotAction::TypeText { text, settle_ms } => {
            let sent_at = Instant::now();
            writer.write_all(text.as_bytes())?;
            writer.flush()?;
            wait_after_input(shared, sent_at, settle_ms, 2500);
            refresh_exit(child, exit_code);
            Ok(build_screen_resp(id, shared, *exit_code))
        }
        PilotAction::Click {
            pos,
            button,
            settle_ms,
        } => {
            let sent_at = Instant::now();
            let bytes = encode_sgr_click(pos.col, pos.row, button);
            writer.write_all(&bytes)?;
            writer.flush()?;
            wait_after_input(shared, sent_at, settle_ms, 2500);
            refresh_exit(child, exit_code);
            Ok(build_screen_resp(id, shared, *exit_code))
        }
        PilotAction::Scroll {
            up,
            pos,
            steps,
            settle_ms,
        } => {
            let sent_at = Instant::now();
            let bytes = encode_sgr_scroll(pos.col, pos.row, up, steps);
            writer.write_all(&bytes)?;
            writer.flush()?;
            wait_after_input(shared, sent_at, settle_ms, 2500);
            refresh_exit(child, exit_code);
            Ok(build_screen_resp(id, shared, *exit_code))
        }
        PilotAction::Hover { pos, settle_ms } => {
            let sent_at = Instant::now();
            let bytes = encode_sgr_hover(pos.col, pos.row);
            writer.write_all(&bytes)?;
            writer.flush()?;
            wait_after_input(shared, sent_at, settle_ms, 2500);
            refresh_exit(child, exit_code);
            Ok(build_screen_resp(id, shared, *exit_code))
        }
        PilotAction::Clipboard => {
            let raw = fs::read(log_path).unwrap_or_default();
            let clip = extract_latest_osc52_clipboard(&raw).unwrap_or_default();
            let mut resp = build_screen_resp(id, shared, *exit_code);
            resp.screen = None;
            resp.clipboard = Some(clip);
            Ok(resp)
        }
        PilotAction::Resize {
            cols,
            rows,
            settle_ms,
        } => {
            let sent_at = Instant::now();
            let cols = cols.max(1);
            let rows = rows.max(1);
            master.resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })?;
            {
                let mut g = shared.lock().unwrap();
                g.cols = cols;
                g.rows = rows;
                g.parser.set_size(
                    NonZeroU16::new(rows).unwrap(),
                    NonZeroU16::new(cols).unwrap(),
                );
            }
            wait_after_input(shared, sent_at, settle_ms, 2500);
            refresh_exit(child, exit_code);
            Ok(build_screen_resp(id, shared, *exit_code))
        }
        PilotAction::FindText { needle } => {
            let matches = find_text_matches(shared, &needle);
            let mut resp = build_screen_resp(id, shared, *exit_code);
            resp.screen = None;
            resp.matches = matches;
            Ok(resp)
        }
        PilotAction::InspectRow { row } => {
            let spans = inspect_row_spans(shared, row);
            let mut resp = build_screen_resp(id, shared, *exit_code);
            resp.screen = None;
            resp.row_spans = spans;
            Ok(resp)
        }
        PilotAction::PaletteSummary => {
            let pal = summarize_palette(shared);
            let mut resp = build_screen_resp(id, shared, *exit_code);
            resp.screen = None;
            resp.palette = pal;
            Ok(resp)
        }
        PilotAction::WaitIdle {
            quiet_ms,
            timeout_ms,
        } => {
            wait_quiet(shared, quiet_ms, timeout_ms);
            refresh_exit(child, exit_code);
            Ok(build_screen_resp(id, shared, *exit_code))
        }
        PilotAction::Stop => {
            if exit_code.is_none() {
                for seq in [b"\x1b".as_slice(), b"\x1b".as_slice(), b"q".as_slice()] {
                    let _ = writer.write_all(seq);
                    let _ = writer.flush();
                    thread::sleep(Duration::from_millis(80));
                }
                let deadline = Instant::now() + Duration::from_millis(800);
                while Instant::now() < deadline {
                    refresh_exit(child, exit_code);
                    if exit_code.is_some() {
                        break;
                    }
                    thread::sleep(Duration::from_millis(30));
                }
                if exit_code.is_none() {
                    let _ = child.kill();
                    if let Ok(status) = child.wait() {
                        *exit_code = Some(status.exit_code() as i32);
                    }
                }
            }
            let mut resp = build_screen_resp(id, shared, *exit_code);
            resp.alive = false;
            Ok(resp)
        }
    })();

    match res {
        Ok(r) => r,
        Err(e) => make_error_resp(id, e.to_string(), shared, *exit_code),
    }
}

fn refresh_exit(
    child: &mut Box<dyn portable_pty::Child + Send + Sync>,
    exit_code: &mut Option<i32>,
) {
    if exit_code.is_none()
        && let Ok(Some(status)) = child.try_wait()
    {
        *exit_code = Some(status.exit_code() as i32);
    }
}

fn wait_after_input(
    shared: &Arc<Mutex<SharedPtyState>>,
    sent_at: Instant,
    settle_ms: u64,
    max_wait_ms: u64,
) {
    let first_byte_budget = Duration::from_millis(220.min(max_wait_ms));
    loop {
        let (has_new, eof) = {
            let g = shared.lock().unwrap();
            (g.last_output >= sent_at, g.eof)
        };
        if has_new || eof || sent_at.elapsed() >= first_byte_budget {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    wait_quiet(shared, settle_ms, max_wait_ms);
}

fn screen_has_content(screen: &vt100::Screen, cols: u16, rows: u16) -> bool {
    (0..rows).any(|r| !render_row_text(screen, r, cols).trim().is_empty())
}

fn wait_for_initial_paint(
    shared: &Arc<Mutex<SharedPtyState>>,
    settle_ms: u64,
    max_wait_ms: u64,
) {
    let start = Instant::now();
    let max_wait = Duration::from_millis(max_wait_ms);
    loop {
        let (painted, eof) = {
            let g = shared.lock().unwrap();
            (
                screen_has_content(g.parser.screen(), g.cols, g.rows),
                g.eof,
            )
        };
        if painted || eof || start.elapsed() >= max_wait {
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }
    wait_quiet(shared, settle_ms, 2500);
}

fn wait_quiet(shared: &Arc<Mutex<SharedPtyState>>, settle_ms: u64, max_wait_ms: u64) {
    if settle_ms == 0 {
        return;
    }
    let start = Instant::now();
    let settle = Duration::from_millis(settle_ms);
    let max_wait = Duration::from_millis(max_wait_ms);
    loop {
        let (elapsed, eof) = {
            let g = shared.lock().unwrap();
            (g.last_output.elapsed(), g.eof)
        };
        if eof || elapsed >= settle || start.elapsed() >= max_wait {
            break;
        }
        thread::sleep(Duration::from_millis(15));
    }
}

fn build_screen_resp(
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

fn make_error_resp(
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

fn find_text_matches(shared: &Arc<Mutex<SharedPtyState>>, needle: &str) -> Vec<TextMatch> {
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

fn inspect_row_spans(shared: &Arc<Mutex<SharedPtyState>>, row: u16) -> Vec<RowSpanInfo> {
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

fn summarize_palette(shared: &Arc<Mutex<SharedPtyState>>) -> Vec<ColorCount> {
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

fn write_custom_test_config(dir: &Path) -> Result<()> {
    if dir.exists() {
        fs::remove_dir_all(dir)?;
    }
    fs::create_dir_all(dir)?;

    fs::write(
        dir.join("jbgit.toml"),
        r#"
[general]
language = "en-US"
default_tab = "log"

[layout]
log_ratio = [ 1, 3, 2 ]
commit_ratio = [ 1, 2 ]
file_history_ratio = [ 1, 2 ]
file_history_vertical_ratio = [ 1, 2 ]
"#,
    )?;

    fs::write(
        dir.join("theme.toml"),
        r##"
flavor = "catppuccin-mocha"

[glyphs]
mode = "unicode"

[palette]
bg_canvas = "#112233"
bg_panel = "#223344"
border_focused = "#FF6600"
"##,
    )?;

    fs::write(
        dir.join("keymap.toml"),
        r#"
footer_max_lines = 2

[global]
prepend_keymap = [
    { on = [ "r" ], run = "noop" },
    { on = [ "b" ], run = "noop" },
    { on = [ "<F8>" ], run = "refresh", show_hint = true },
    { on = [ "<F6>" ], run = "toggle_branches_popup", show_hint = true },
    { on = [ "g", "x" ], run = "refresh" },
]

[log]
prepend_keymap = [
    { on = [ "n" ], run = "noop" },
    { on = [ "<F7>" ], run = "new_branch", show_hint = true },
]

[branch]
prepend_keymap = [
    { on = [ "n" ], run = "noop" },
    { on = [ "<F7>" ], run = "new_branch", show_hint = true },
]

[commit]
prepend_keymap = [
    { on = [ "<C-s>" ], run = "noop" },
    { on = [ "<F9>" ], run = "commit", show_hint = true },
]
"#,
    )?;
    Ok(())
}

pub fn init_sandbox_repo(dir: &Path, remote_dir: &Path, scenario: SandboxScenario) -> Result<()> {
    if dir.exists() {
        fs::remove_dir_all(dir)?;
    }
    if remote_dir.exists() {
        fs::remove_dir_all(remote_dir)?;
    }
    fs::create_dir_all(dir)?;

    run_git(dir, &["init", "-b", "main"])?;
    run_git(dir, &["config", "user.name", "QA Tester"])?;
    run_git(dir, &["config", "user.email", "qa@example.com"])?;

    match scenario {
        SandboxScenario::Basic => {
            fs::write(dir.join("README.md"), "# jbgit QA Sandbox\n\nline 1\nline 2\n")?;
            run_git(dir, &["add", "README.md"])?;
            run_git(dir, &["commit", "-m", "initial commit"])?;

            fs::create_dir_all(dir.join("src"))?;
            fs::write(
                dir.join("src/main.rs"),
                "fn main() {\n    println!(\"hello jbgit\");\n}\n",
            )?;
            run_git(dir, &["add", "src/main.rs"])?;
            run_git(dir, &["commit", "-m", "add main.rs"])?;

            run_git(dir, &["branch", "feature/ui-test"])?;
            run_git(dir, &["branch", "bugfix/scroll"])?;
            run_git(dir, &["tag", "v0.1.0"])?;

            fs::write(
                dir.join("src/main.rs"),
                "fn main() {\n    println!(\"hello jbgit modified\");\n}\n",
            )?;
            fs::write(dir.join("scratch_note.txt"), "untracked qa file\n")?;
        }
        SandboxScenario::Conflict => {
            fs::write(dir.join("conflict_app.txt"), "line 1: base\nline 2: common\n")?;
            fs::write(dir.join("README.md"), "# Conflict QA Repo\n")?;
            run_git(dir, &["add", "."])?;
            run_git(dir, &["commit", "-m", "base commit"])?;

            run_git(dir, &["checkout", "-b", "feature/theirs"])?;
            fs::write(
                dir.join("conflict_app.txt"),
                "line 1: theirs change\nline 2: common\n",
            )?;
            run_git(dir, &["commit", "-am", "theirs change"])?;

            run_git(dir, &["checkout", "main"])?;
            fs::write(
                dir.join("conflict_app.txt"),
                "line 1: ours change\nline 2: common\n",
            )?;
            run_git(dir, &["commit", "-am", "ours change"])?;

            // Expected to exit non-zero with merge conflict markers on disk
            let out = Command::new("git")
                .args(["merge", "feature/theirs"])
                .current_dir(dir)
                .env("GIT_AUTHOR_NAME", "QA Tester")
                .env("GIT_AUTHOR_EMAIL", "qa@example.com")
                .env("GIT_COMMITTER_NAME", "QA Tester")
                .env("GIT_COMMITTER_EMAIL", "qa@example.com")
                .output()?;
            if out.status.success() {
                bail!("expected merge conflict in Conflict scenario, but merge succeeded");
            }
        }
        SandboxScenario::Full => {
            // 1. Create bare remote + base history
            fs::create_dir_all(remote_dir)?;
            run_git(remote_dir, &["init", "--bare", "-b", "main"])?;

            fs::write(dir.join("README.md"), "# jbgit Full QA Sandbox\n\nline 1\n")?;
            fs::create_dir_all(dir.join("src/components"))?;
            let mut src_lines = Vec::new();
            for i in 1..=30 {
                src_lines.push(format!("fn step_{i:02}() {{ println!(\"step {i}\"); }}"));
            }
            fs::write(dir.join("src/main.rs"), format!("{}\n", src_lines.join("\n")))?;
            fs::write(
                dir.join("src/legacy_auth.rs"),
                "pub fn verify_user(user: &str) -> bool {\n    !user.is_empty()\n}\n",
            )?;
            fs::write(
                dir.join("src/components/header.rs"),
                "pub fn title() -> &'static str { \"jbgit\" }\n",
            )?;
            fs::write(dir.join(".gitignore"), "*.log\n")?;
            fs::write(dir.join("staged_partial.txt"), "alpha\nbeta\ngamma\ndelta\n")?;
            run_git(dir, &["add", "."])?;
            run_git(dir, &["commit", "-m", "initial project scaffold"])?;
            run_git(dir, &["tag", "v0.1.0"])?;

            // Evolve and rename src/legacy_auth.rs -> src/auth.rs for --follow File History testing
            fs::write(
                dir.join("src/legacy_auth.rs"),
                "pub fn verify_user(user: &str) -> bool {\n    !user.is_empty()\n}\n\npub fn token_ttl() -> u64 {\n    3600\n}\n",
            )?;
            run_git(dir, &["commit", "-am", "feat: add token_ttl to legacy_auth"])?;
            run_git(dir, &["mv", "src/legacy_auth.rs", "src/auth.rs"])?;
            run_git(dir, &["commit", "-m", "refactor: rename legacy_auth.rs to auth.rs"])?;

            // Push initial main + create a remote-only branch
            run_git(dir, &["remote", "add", "origin", &remote_dir.to_string_lossy()])?;
            run_git(dir, &["push", "-u", "origin", "main"])?;

            run_git(dir, &["checkout", "-b", "feature/remote-only"])?;
            fs::write(dir.join("remote_note.md"), "remote-only commit\n")?;
            run_git(dir, &["add", "remote_note.md"])?;
            run_git(dir, &["commit", "-m", "feat: remote-only branch work"])?;
            run_git(dir, &["push", "-u", "origin", "feature/remote-only"])?;

            // 2. Merged side branch (creates real merge commit + DAG fork/join)
            run_git(dir, &["checkout", "main"])?;
            run_git(dir, &["branch", "-D", "feature/remote-only"])?;
            run_git(dir, &["checkout", "-b", "feature/ui-cards"])?;
            fs::write(dir.join("src/cards.rs"), "pub fn card() -> &'static str { \"card\" }\n")?;
            run_git(dir, &["add", "src/cards.rs"])?;
            run_git(dir, &["commit", "-m", "feat: add ui cards module"])?;

            run_git(dir, &["checkout", "main"])?;
            run_git(dir, &["merge", "--no-ff", "feature/ui-cards", "-m", "merge feature/ui-cards"])?;

            // 3. Unmerged feature + bugfix branches
            run_git(dir, &["checkout", "-b", "feature/login"])?;
            fs::write(
                dir.join("src/auth.rs"),
                "pub fn verify_user(user: &str) -> bool {\n    !user.is_empty()\n}\n\npub fn token_ttl() -> u64 {\n    3600\n}\n\npub fn login() -> bool {\n    true\n}\n",
            )?;
            run_git(dir, &["add", "src/auth.rs"])?;
            run_git(dir, &["commit", "-m", "feat: implement oauth login"])?;

            run_git(dir, &["checkout", "main"])?;
            run_git(dir, &["branch", "bugfix/scroll-edge"])?;

            // Create `feature/pull-me` & `feature/diverged` tracking origin,
            // then push new commits from a peer clone so Fetch / Pull / Push-Rejected can be tested!
            run_git(dir, &["checkout", "-b", "feature/pull-me"])?;
            fs::write(dir.join("pull_me.txt"), "base\n")?;
            run_git(dir, &["add", "pull_me.txt"])?;
            run_git(dir, &["commit", "-m", "chore: pull-me base"])?;
            run_git(dir, &["push", "-u", "origin", "feature/pull-me"])?;

            run_git(dir, &["checkout", "-b", "feature/diverged", "main"])?;
            fs::write(dir.join("diverged.txt"), "base\n")?;
            run_git(dir, &["add", "diverged.txt"])?;
            run_git(dir, &["commit", "-m", "chore: diverged base"])?;
            run_git(dir, &["push", "-u", "origin", "feature/diverged"])?;
            fs::write(dir.join("diverged.txt"), "local side change\n")?;
            run_git(dir, &["commit", "-am", "feat: local diverged commit"])?;

            let peer_dir = dir
                .parent()
                .map(|p| p.join("sandbox-peer"))
                .unwrap_or_else(|| PathBuf::from("sandbox-peer"));
            if peer_dir.exists() {
                fs::remove_dir_all(&peer_dir)?;
            }
            run_git(
                dir.parent().unwrap_or(dir),
                &[
                    "clone",
                    &remote_dir.to_string_lossy(),
                    &peer_dir.to_string_lossy(),
                ],
            )?;
            run_git(&peer_dir, &["config", "user.name", "Remote Peer"])?;
            run_git(&peer_dir, &["config", "user.email", "peer@example.com"])?;
            run_git(&peer_dir, &["checkout", "feature/pull-me"])?;
            fs::write(peer_dir.join("pull_me.txt"), "base\nremote 1\n")?;
            run_git(&peer_dir, &["commit", "-am", "remote: pull commit 1"])?;
            fs::write(peer_dir.join("pull_me.txt"), "base\nremote 1\nremote 2\n")?;
            run_git(&peer_dir, &["commit", "-am", "remote: pull commit 2"])?;
            run_git(&peer_dir, &["push", "origin", "feature/pull-me"])?;

            run_git(&peer_dir, &["checkout", "feature/diverged"])?;
            fs::write(peer_dir.join("diverged.txt"), "remote side change\n")?;
            run_git(&peer_dir, &["commit", "-am", "remote: diverged commit"])?;
            run_git(&peer_dir, &["push", "origin", "feature/diverged"])?;
            let _ = fs::remove_dir_all(&peer_dir);

            run_git(dir, &["checkout", "main"])?;

            // Local main is ahead of origin/main
            fs::write(dir.join("README.md"), "# jbgit Full QA Sandbox\n\nline 1\nupdated on main\n")?;
            fs::write(
                dir.join("src/auth.rs"),
                "pub fn verify_user(user: &str) -> bool {\n    !user.trim().is_empty()\n}\n\npub fn token_ttl() -> u64 {\n    3600\n}\n",
            )?;
            fs::write(
                dir.join("src/components/header.rs"),
                "pub fn title() -> &'static str { \"jbgit TUI\" }\n",
            )?;
            run_git(dir, &["commit", "-am", "feat: refine auth, header, and README on main"])?;

            // 4. Create two real Stashes for Stash tab testing
            fs::write(dir.join("README.md"), "# jbgit Full QA Sandbox\n\nstash one edit\n")?;
            run_git(dir, &["stash", "push", "-m", "wip: older stash"])?;
            fs::write(dir.join("README.md"), "# jbgit Full QA Sandbox\n\nstash two edit\n")?;
            run_git(dir, &["stash", "push", "-m", "wip: refactor stash test"])?;

            // 5. Partial staged file (staged + unstaged dual presence)
            fs::write(dir.join("staged_partial.txt"), "alpha-staged\nbeta\ngamma\ndelta\n")?;
            run_git(dir, &["add", "staged_partial.txt"])?;
            fs::write(dir.join("staged_partial.txt"), "alpha-staged\nbeta\ngamma\ndelta-unstaged\n")?;

            // 6. Multi-hunk modified file (far apart so 2 distinct hunks exist)
            src_lines[1] = "fn step_02() { println!(\"step 2 modified hunk A\"); }".to_string();
            src_lines[26] = "fn step_27() { println!(\"step 27 modified hunk B\"); }".to_string();
            fs::write(dir.join("src/main.rs"), format!("{}\n", src_lines.join("\n")))?;

            // 7. Untracked and ignored files
            fs::write(dir.join("scratch_note.txt"), "untracked qa file\n")?;
            fs::write(dir.join("debug_trace.log"), "ignored debug log\n")?;
        }
    }
    Ok(())
}

fn extract_latest_osc52_clipboard(raw: &[u8]) -> Option<String> {
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

fn run_git(dir: &Path, args: &[&str]) -> Result<()> {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "QA Tester")
        .env("GIT_AUTHOR_EMAIL", "qa@example.com")
        .env("GIT_COMMITTER_NAME", "QA Tester")
        .env("GIT_COMMITTER_EMAIL", "qa@example.com")
        .output()
        .with_context(|| format!("run git {args:?}"))?;
    if !out.status.success() {
        bail!(
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
