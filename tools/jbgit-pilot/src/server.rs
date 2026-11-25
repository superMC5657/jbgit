use std::fs;
use std::io::{Read, Write};
use std::num::NonZeroU16;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use portable_pty::{CommandBuilder, PtySize, native_pty_system};

use crate::keys::{encode_key, encode_sgr_click, encode_sgr_hover, encode_sgr_scroll};
use crate::protocol::{CommandEnvelope, PilotAction, ResponseEnvelope};
use crate::sandbox::{
    PreparedSandbox, prepare_sandbox, write_custom_test_config, write_recent_repos,
};
use crate::screen::{
    Osc52ClipboardScanner, build_screen_resp, find_text_matches, inspect_row_spans,
    make_error_resp, screen_has_content, summarize_palette,
};

pub use crate::sandbox::SandboxScenario;

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

pub(crate) struct SharedPtyState {
    pub(crate) parser: vt100::Parser,
    pub(crate) osc52: Osc52ClipboardScanner,
    pub(crate) last_output: Instant,
    pub(crate) cols: u16,
    pub(crate) rows: u16,
    pub(crate) eof: bool,
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

    let mut prepared_sandbox: Option<PreparedSandbox> = None;

    let repo_path = if opts.sandbox {
        let prepared = prepare_sandbox(&opts.session_dir, opts.scenario)?;
        let repo_dir = prepared.repo_dir.clone();
        prepared_sandbox = Some(prepared);
        repo_dir
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

    if let Some(prepared) = &prepared_sandbox
        && !prepared.recent_repos.is_empty()
        && let Some(cfg) = &resolved_config_dir
    {
        write_recent_repos(cfg, &prepared.recent_repos)?;
    }

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
    cmd.env("GCM_INTERACTIVE", "never");
    if std::env::var_os("JBGIT_NO_EXTERNAL_EDITOR").is_none() {
        cmd.env("JBGIT_NO_EXTERNAL_EDITOR", "1");
    }
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
        osc52: Osc52ClipboardScanner::default(),
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
                        guard.osc52.feed(&buf[..n]);
                        guard.last_output = Instant::now();
                    }
                }
            }
        }
    });

    wait_for_initial_paint(&shared, 300, 8000);

    let mut ready_info = serde_json::json!({
        "pid": std::process::id(),
        "repo": repo_path,
        "bin": bin_path,
        "config_dir": resolved_config_dir,
        "scenario": format!("{:?}", opts.scenario).to_ascii_lowercase(),
        "cols": opts.cols,
        "rows": opts.rows,
    });
    if let Some(prepared) = &prepared_sandbox {
        if let Some(url) = &prepared.askpass_url {
            ready_info["askpass"] = serde_json::json!({
                "url": url,
                "auth_required": true,
            });
        }
        if let Some((sample, conflict)) = &prepared.patches {
            ready_info["sample_patch"] = serde_json::json!(sample);
            ready_info["conflict_patch"] = serde_json::json!(conflict);
        }
    }
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
                    let resp = make_error_resp(
                        0,
                        format!("invalid command json: {e}"),
                        &shared,
                        exit_code,
                    );
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
    drop(prepared_sandbox);
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
            let clip = shared
                .lock()
                .unwrap()
                .osc52
                .latest()
                .unwrap_or_default()
                .to_string();
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

fn wait_for_initial_paint(shared: &Arc<Mutex<SharedPtyState>>, settle_ms: u64, max_wait_ms: u64) {
    let start = Instant::now();
    let max_wait = Duration::from_millis(max_wait_ms);
    loop {
        let (painted, eof) = {
            let g = shared.lock().unwrap();
            (screen_has_content(g.parser.screen(), g.cols, g.rows), g.eof)
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
