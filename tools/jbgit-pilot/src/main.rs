mod client;
mod keys;
mod protocol;
mod sandbox;
mod screen;
mod server;

use std::env;
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

use crate::client::{print_response, send_action};
use crate::protocol::{CellPos, MouseBtn, PilotAction};
use crate::server::{SandboxScenario, ServeOptions, run_serve};

fn default_session_dir() -> PathBuf {
    PathBuf::from(".pilot")
}

fn default_jbgit_bin() -> PathBuf {
    let exe_name = if cfg!(windows) { "jbgit.exe" } else { "jbgit" };
    let root_debug = PathBuf::from("target").join("debug").join(exe_name);
    let pilot_debug = PathBuf::from("tools")
        .join("jbgit-pilot")
        .join("target")
        .join("debug")
        .join(exe_name);
    match (root_debug.is_file(), pilot_debug.is_file()) {
        (true, true) => {
            let root_mtime = std::fs::metadata(&root_debug)
                .and_then(|m| m.modified())
                .ok();
            let pilot_mtime = std::fs::metadata(&pilot_debug)
                .and_then(|m| m.modified())
                .ok();
            if pilot_mtime > root_mtime {
                pilot_debug
            } else {
                root_debug
            }
        }
        (true, false) => root_debug,
        (false, true) => pilot_debug,
        (false, false) => {
            let release = PathBuf::from("target").join("release").join(exe_name);
            if release.is_file() {
                return release;
            }
            root_debug
        }
    }
}

fn print_help() {
    println!(
        "jbgit-pilot — Real ConPTY + vt100 interactive driver for AI exploratory QA

USAGE:
  jbgit-pilot [--session-dir <dir>] <COMMAND> [ARGS...]

SERVER (run with IsDaemon=true):
  serve [--repo <dir> | --sandbox] [--scenario full|conflict|basic]
        [--custom-config | --config-dir <dir> | --use-user-config]
        [--cols 140] [--rows 40] [--bin <path>] [--no-truecolor]

CLIENT COMMANDS:
  screen [--settle-ms 150]
  key <key1> [<key2> ...] [--settle-ms 200]
  type <text> [--settle-ms 200]
  click <col> <row> [--button left|right|middle] [--settle-ms 200]
  rclick <col> <row> [--settle-ms 200]
  hover <col> <row> [--settle-ms 200]
  scroll <up|down> <col> <row> [--steps 3] [--settle-ms 200]
  resize <cols> <rows> [--settle-ms 200]
  find <needle>
  inspect-row <row>
  colors
  clipboard
  wait-idle [--quiet-ms 250] [--timeout-ms 4000]
  stop"
    );
}

fn main() -> Result<()> {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.is_empty() || args[0] == "-h" || args[0] == "--help" {
        print_help();
        return Ok(());
    }

    let mut idx = 0usize;
    let mut session_dir = default_session_dir();
    if args.get(idx).map(String::as_str) == Some("--session-dir") {
        idx += 1;
        session_dir = PathBuf::from(req_arg(&args, &mut idx, "--session-dir")?);
    }

    let Some(cmd) = args.get(idx).map(String::as_str) else {
        print_help();
        return Ok(());
    };
    idx += 1;

    match cmd {
        "serve" => {
            let mut opts = ServeOptions {
                session_dir: session_dir.clone(),
                bin_path: default_jbgit_bin(),
                repo_path: None,
                sandbox: false,
                scenario: SandboxScenario::Full,
                custom_config: false,
                use_user_config: false,
                config_dir: None,
                cols: 140,
                rows: 40,
                truecolor: true,
            };
            while idx < args.len() {
                match args[idx].as_str() {
                    "--repo" => {
                        idx += 1;
                        opts.repo_path = Some(PathBuf::from(req_arg(&args, &mut idx, "--repo")?));
                    }
                    "--sandbox" => {
                        idx += 1;
                        opts.sandbox = true;
                    }
                    "--scenario" => {
                        idx += 1;
                        opts.sandbox = true;
                        opts.scenario = SandboxScenario::parse(&req_arg(&args, &mut idx, "--scenario")?)?;
                    }
                    "--custom-config" => {
                        idx += 1;
                        opts.custom_config = true;
                    }
                    "--use-user-config" => {
                        idx += 1;
                        opts.use_user_config = true;
                    }
                    "--config-dir" => {
                        idx += 1;
                        opts.config_dir = Some(PathBuf::from(req_arg(&args, &mut idx, "--config-dir")?));
                    }
                    "--cols" => {
                        idx += 1;
                        opts.cols = req_arg(&args, &mut idx, "--cols")?.parse()?;
                    }
                    "--rows" => {
                        idx += 1;
                        opts.rows = req_arg(&args, &mut idx, "--rows")?.parse()?;
                    }
                    "--bin" => {
                        idx += 1;
                        opts.bin_path = PathBuf::from(req_arg(&args, &mut idx, "--bin")?);
                    }
                    "--session-dir" => {
                        idx += 1;
                        opts.session_dir = PathBuf::from(req_arg(&args, &mut idx, "--session-dir")?);
                    }
                    "--no-truecolor" => {
                        idx += 1;
                        opts.truecolor = false;
                    }
                    other => bail!("unknown flag '{other}' for serve"),
                }
            }
            run_serve(opts)?;
        }
        "screen" => {
            let settle_ms = parse_opt_u64(&args, idx, "--settle-ms")?.unwrap_or(150);
            run_client(&session_dir, PilotAction::Screen { settle_ms }, 5000)?;
        }
        "key" => {
            let mut keys = Vec::new();
            let mut settle_ms = 200u64;
            while idx < args.len() {
                if args[idx] == "--settle-ms" {
                    idx += 1;
                    settle_ms = req_arg(&args, &mut idx, "--settle-ms")?.parse()?;
                } else {
                    keys.push(args[idx].clone());
                    idx += 1;
                }
            }
            if keys.is_empty() {
                bail!("usage: jbgit-pilot key <key1> [<key2> ...]");
            }
            run_client(&session_dir, PilotAction::Keys { keys, settle_ms }, 6000)?;
        }
        "type" => {
            let text = req_arg(&args, &mut idx, "text")?;
            let settle_ms = parse_opt_u64(&args, idx, "--settle-ms")?.unwrap_or(200);
            run_client(&session_dir, PilotAction::TypeText { text, settle_ms }, 6000)?;
        }
        "click" => {
            let col: u16 = req_arg(&args, &mut idx, "col")?.parse()?;
            let row: u16 = req_arg(&args, &mut idx, "row")?.parse()?;
            let mut button = MouseBtn::Left;
            let mut settle_ms = 200u64;
            while idx < args.len() {
                match args[idx].as_str() {
                    "--button" => {
                        idx += 1;
                        button = match req_arg(&args, &mut idx, "--button")?.as_str() {
                            "left" => MouseBtn::Left,
                            "right" => MouseBtn::Right,
                            "middle" => MouseBtn::Middle,
                            other => bail!("invalid button '{other}'"),
                        };
                    }
                    "--settle-ms" => {
                        idx += 1;
                        settle_ms = req_arg(&args, &mut idx, "--settle-ms")?.parse()?;
                    }
                    other => bail!("unknown flag '{other}' for click"),
                }
            }
            run_client(
                &session_dir,
                PilotAction::Click {
                    pos: CellPos { col, row },
                    button,
                    settle_ms,
                },
                6000,
            )?;
        }
        "rclick" => {
            let col: u16 = req_arg(&args, &mut idx, "col")?.parse()?;
            let row: u16 = req_arg(&args, &mut idx, "row")?.parse()?;
            let settle_ms = parse_opt_u64(&args, idx, "--settle-ms")?.unwrap_or(200);
            run_client(
                &session_dir,
                PilotAction::Click {
                    pos: CellPos { col, row },
                    button: MouseBtn::Right,
                    settle_ms,
                },
                6000,
            )?;
        }
        "hover" => {
            let col: u16 = req_arg(&args, &mut idx, "col")?.parse()?;
            let row: u16 = req_arg(&args, &mut idx, "row")?.parse()?;
            let settle_ms = parse_opt_u64(&args, idx, "--settle-ms")?.unwrap_or(200);
            run_client(
                &session_dir,
                PilotAction::Hover {
                    pos: CellPos { col, row },
                    settle_ms,
                },
                6000,
            )?;
        }
        "scroll" => {
            let dir = req_arg(&args, &mut idx, "up|down")?;
            let up = match dir.as_str() {
                "up" => true,
                "down" => false,
                _ => bail!("scroll direction must be 'up' or 'down'"),
            };
            let col: u16 = req_arg(&args, &mut idx, "col")?.parse()?;
            let row: u16 = req_arg(&args, &mut idx, "row")?.parse()?;
            let mut steps = 3u16;
            let mut settle_ms = 200u64;
            while idx < args.len() {
                match args[idx].as_str() {
                    "--steps" => {
                        idx += 1;
                        steps = req_arg(&args, &mut idx, "--steps")?.parse()?;
                    }
                    "--settle-ms" => {
                        idx += 1;
                        settle_ms = req_arg(&args, &mut idx, "--settle-ms")?.parse()?;
                    }
                    other => bail!("unknown flag '{other}' for scroll"),
                }
            }
            run_client(
                &session_dir,
                PilotAction::Scroll {
                    up,
                    pos: CellPos { col, row },
                    steps,
                    settle_ms,
                },
                6000,
            )?;
        }
        "resize" => {
            let cols: u16 = req_arg(&args, &mut idx, "cols")?.parse()?;
            let rows: u16 = req_arg(&args, &mut idx, "rows")?.parse()?;
            let settle_ms = parse_opt_u64(&args, idx, "--settle-ms")?.unwrap_or(200);
            run_client(
                &session_dir,
                PilotAction::Resize {
                    cols,
                    rows,
                    settle_ms,
                },
                6000,
            )?;
        }
        "find" => {
            let needle = req_arg(&args, &mut idx, "needle")?;
            run_client(&session_dir, PilotAction::FindText { needle }, 5000)?;
        }
        "inspect-row" => {
            let row: u16 = req_arg(&args, &mut idx, "row")?.parse()?;
            run_client(&session_dir, PilotAction::InspectRow { row }, 5000)?;
        }
        "colors" => {
            run_client(&session_dir, PilotAction::PaletteSummary, 5000)?;
        }
        "clipboard" => {
            run_client(&session_dir, PilotAction::Clipboard, 5000)?;
        }
        "wait-idle" => {
            let mut quiet_ms = 250u64;
            let mut timeout_ms = 4000u64;
            while idx < args.len() {
                match args[idx].as_str() {
                    "--quiet-ms" => {
                        idx += 1;
                        quiet_ms = req_arg(&args, &mut idx, "--quiet-ms")?.parse()?;
                    }
                    "--timeout-ms" => {
                        idx += 1;
                        timeout_ms = req_arg(&args, &mut idx, "--timeout-ms")?.parse()?;
                    }
                    other => bail!("unknown flag '{other}' for wait-idle"),
                }
            }
            run_client(
                &session_dir,
                PilotAction::WaitIdle {
                    quiet_ms,
                    timeout_ms,
                },
                timeout_ms + 2000,
            )?;
        }
        "stop" => {
            run_client(&session_dir, PilotAction::Stop, 5000)?;
        }
        other => bail!("unknown command '{other}' (run with --help)"),
    }
    Ok(())
}

fn run_client(session_dir: &Path, action: PilotAction, timeout_ms: u64) -> Result<()> {
    let resp = send_action(session_dir, action, timeout_ms)?;
    print_response(&resp);
    if !resp.ok {
        std::process::exit(1);
    }
    Ok(())
}

fn req_arg(args: &[String], idx: &mut usize, name: &str) -> Result<String> {
    let Some(v) = args.get(*idx) else {
        bail!("missing argument <{name}>");
    };
    *idx += 1;
    Ok(v.clone())
}

fn parse_opt_u64(args: &[String], mut idx: usize, flag: &str) -> Result<Option<u64>> {
    if args.get(idx).map(String::as_str) == Some(flag) {
        idx += 1;
        let val: u64 = req_arg(args, &mut idx, flag)?.parse()?;
        return Ok(Some(val));
    }
    Ok(None)
}
