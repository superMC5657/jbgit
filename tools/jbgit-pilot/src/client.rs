use std::fs;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};

use crate::protocol::{CommandEnvelope, PilotAction, ResponseEnvelope};

pub fn send_action(
    session_dir: &Path,
    action: PilotAction,
    timeout_ms: u64,
) -> Result<ResponseEnvelope> {
    let ready_path = session_dir.join("ready.json");
    let starting_path = session_dir.join("starting.json");
    let cmd_path = session_dir.join("cmd.json");
    let resp_path = session_dir.join("resp.json");

    if !ready_path.is_file() && starting_path.is_file() {
        let start_deadline = Instant::now() + Duration::from_millis(15_000);
        while !ready_path.is_file() && starting_path.is_file() && Instant::now() < start_deadline {
            thread::sleep(Duration::from_millis(40));
        }
    }

    if !ready_path.is_file() {
        bail!(
            "no active jbgit-pilot session found at {:?}. Start one first with `jbgit-pilot serve ...` (with IsDaemon=true).",
            session_dir
        );
    }

    let id = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64;

    let _ = fs::remove_file(&resp_path);

    let env = CommandEnvelope { id, action };
    let tmp_cmd = session_dir.join("cmd.json.tmp");
    fs::write(&tmp_cmd, serde_json::to_vec_pretty(&env)?)?;
    fs::rename(&tmp_cmd, &cmd_path).context("publish cmd.json")?;

    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    loop {
        if resp_path.is_file()
            && let Ok(raw) = fs::read_to_string(&resp_path)
            && let Ok(resp) = serde_json::from_str::<ResponseEnvelope>(&raw)
            && resp.id == id
        {
            let _ = fs::remove_file(&resp_path);
            return Ok(resp);
        }
        if Instant::now() >= deadline {
            bail!("timed out waiting for jbgit-pilot daemon response ({timeout_ms}ms)");
        }
        thread::sleep(Duration::from_millis(12));
    }
}

pub fn print_response(resp: &ResponseEnvelope) {
    println!(
        "[alive={} exit={:?} size={}x{} cursor=({}, {})]",
        resp.alive, resp.exit_code, resp.cols, resp.rows, resp.cursor.col, resp.cursor.row
    );
    if let Some(err) = &resp.error {
        eprintln!("ERROR: {err}");
    }
    if let Some(target) = resp.target {
        println!(
            "TARGET col={} row={} span={}..={} match={}/{}",
            target.pos.col,
            target.pos.row,
            target.start_col,
            target.end_col,
            target.match_index + 1,
            target.match_count,
        );
    }
    if let Some(screen) = &resp.screen {
        println!("{screen}");
    }
    if !resp.matches.is_empty() {
        for m in &resp.matches {
            if let Some(end_col) = m.end_col {
                println!(
                    "MATCH col={} end_col={} row={} | {}",
                    m.pos.col, end_col, m.pos.row, m.line
                );
            } else {
                println!("MATCH col={} row={} | {}", m.pos.col, m.pos.row, m.line);
            }
        }
    }
    if !resp.row_spans.is_empty() {
        for s in &resp.row_spans {
            println!(
                "SPAN cols={}..={} fg={} bg={} | {:?}",
                s.start_col, s.end_col, s.colors.fg, s.colors.bg, s.text
            );
        }
    }
    if !resp.palette.is_empty() {
        for c in &resp.palette {
            println!(
                "COLOR cells={:<5} fg={:<9} bg={}",
                c.cells, c.colors.fg, c.colors.bg
            );
        }
    }
    if let Some(clip) = &resp.clipboard {
        println!("CLIPBOARD: {clip}");
    }
}
