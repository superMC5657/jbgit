mod askpass_http;
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
use crate::protocol::{CellPos, MouseBtn, MouseModifiers, PilotAction, TextTarget};
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
  serve [--repo <dir> | --sandbox]
        [--scenario full|conflict|basic|multi-repo|capabilities|askpass|rebase-conflict|bisect|welcome|workflow]
        [--custom-config | --config-dir <dir> | --use-user-config]
        [--cols 140] [--rows 40] [--bin <path>] [--no-truecolor] [--env KEY=VALUE]

CLIENT COMMANDS:
  screen [--settle-ms 150]
  key <key1> [<key2> ...] [--settle-ms 200]
  type <text> [--settle-ms 200]
  click <col> <row> [--button left|right|middle] [--ctrl] [--shift] [--alt] [--settle-ms 200]
  rclick <col> <row> [--ctrl] [--shift] [--alt] [--settle-ms 200]
  hover <col> <row> [--settle-ms 200]
  click-text <needle> [--row <row>] [--index <n>] [--button left|right|middle] [--ctrl] [--shift] [--alt] [--settle-ms 200]
  rclick-text <needle> [--row <row>] [--index <n>] [--ctrl] [--shift] [--alt] [--settle-ms 200]
  hover-text <needle> [--row <row>] [--index <n>] [--settle-ms 200]
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
                extra_env: Vec::new(),
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
                        opts.scenario =
                            SandboxScenario::parse(&req_arg(&args, &mut idx, "--scenario")?)?;
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
                        opts.config_dir =
                            Some(PathBuf::from(req_arg(&args, &mut idx, "--config-dir")?));
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
                        opts.session_dir =
                            PathBuf::from(req_arg(&args, &mut idx, "--session-dir")?);
                    }
                    "--no-truecolor" => {
                        idx += 1;
                        opts.truecolor = false;
                    }
                    "--env" => {
                        idx += 1;
                        let pair = req_arg(&args, &mut idx, "--env")?;
                        let Some((k, v)) = pair.split_once('=') else {
                            bail!("invalid --env '{pair}' (expected KEY=VALUE)");
                        };
                        if k.is_empty() {
                            bail!("invalid --env '{pair}' (empty key)");
                        }
                        opts.extra_env.push((k.to_string(), v.to_string()));
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
            run_client(
                &session_dir,
                PilotAction::TypeText { text, settle_ms },
                6000,
            )?;
        }
        "click" => {
            let action = parse_click_action(&args, &mut idx, MouseBtn::Left, true, "click")?;
            run_client(&session_dir, action, 6000)?;
        }
        "rclick" => {
            let action = parse_click_action(&args, &mut idx, MouseBtn::Right, false, "rclick")?;
            run_client(&session_dir, action, 6000)?;
        }
        "click-text" => {
            let action =
                parse_click_text_action(&args, &mut idx, MouseBtn::Left, true, "click-text")?;
            run_client(&session_dir, action, 6000)?;
        }
        "rclick-text" => {
            let action =
                parse_click_text_action(&args, &mut idx, MouseBtn::Right, false, "rclick-text")?;
            run_client(&session_dir, action, 6000)?;
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
        "hover-text" => {
            let action = parse_hover_text_action(&args, &mut idx)?;
            run_client(&session_dir, action, 6000)?;
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

fn try_parse_text_target_flag(
    args: &[String],
    idx: &mut usize,
    row: &mut Option<u16>,
    index: &mut Option<usize>,
) -> Result<bool> {
    match args[*idx].as_str() {
        "--row" => {
            *idx += 1;
            *row = Some(req_arg(args, idx, "--row")?.parse()?);
            Ok(true)
        }
        "--index" => {
            *idx += 1;
            *index = Some(req_arg(args, idx, "--index")?.parse()?);
            Ok(true)
        }
        _ => Ok(false),
    }
}

fn try_parse_click_flag(
    args: &[String],
    idx: &mut usize,
    allow_button: bool,
    button: &mut MouseBtn,
    modifiers: &mut MouseModifiers,
) -> Result<bool> {
    match args[*idx].as_str() {
        "--button" if allow_button => {
            *idx += 1;
            *button = match req_arg(args, idx, "--button")?.as_str() {
                "left" => MouseBtn::Left,
                "right" => MouseBtn::Right,
                "middle" => MouseBtn::Middle,
                other => bail!("invalid button '{other}'"),
            };
            Ok(true)
        }
        "--ctrl" => {
            *idx += 1;
            modifiers.ctrl = true;
            Ok(true)
        }
        "--shift" => {
            *idx += 1;
            modifiers.shift = true;
            Ok(true)
        }
        "--alt" => {
            *idx += 1;
            modifiers.alt = true;
            Ok(true)
        }
        _ => Ok(false),
    }
}

fn parse_click_action(
    args: &[String],
    idx: &mut usize,
    default_button: MouseBtn,
    allow_button: bool,
    cmd_name: &str,
) -> Result<PilotAction> {
    let col: u16 = req_arg(args, idx, "col")?.parse()?;
    let row: u16 = req_arg(args, idx, "row")?.parse()?;
    let mut button = default_button;
    let mut modifiers = MouseModifiers::default();
    let mut settle_ms = 200u64;
    while *idx < args.len() {
        if try_parse_click_flag(args, idx, allow_button, &mut button, &mut modifiers)? {
            continue;
        }
        if args[*idx] == "--settle-ms" {
            *idx += 1;
            settle_ms = req_arg(args, idx, "--settle-ms")?.parse()?;
            continue;
        }
        bail!("unknown flag '{}' for {cmd_name}", args[*idx]);
    }
    Ok(PilotAction::Click {
        pos: CellPos { col, row },
        button,
        modifiers,
        settle_ms,
    })
}

fn parse_click_text_action(
    args: &[String],
    idx: &mut usize,
    default_button: MouseBtn,
    allow_button: bool,
    cmd_name: &str,
) -> Result<PilotAction> {
    let needle = req_arg(args, idx, "needle")?;
    let mut row = None;
    let mut index = None;
    let mut button = default_button;
    let mut modifiers = MouseModifiers::default();
    let mut settle_ms = 200u64;
    while *idx < args.len() {
        if try_parse_text_target_flag(args, idx, &mut row, &mut index)?
            || try_parse_click_flag(args, idx, allow_button, &mut button, &mut modifiers)?
        {
            continue;
        }
        if args[*idx] == "--settle-ms" {
            *idx += 1;
            settle_ms = req_arg(args, idx, "--settle-ms")?.parse()?;
            continue;
        }
        bail!("unknown flag '{}' for {cmd_name}", args[*idx]);
    }
    Ok(PilotAction::ClickText {
        target: TextTarget { needle, row, index },
        button,
        modifiers,
        settle_ms,
    })
}

fn parse_hover_text_action(args: &[String], idx: &mut usize) -> Result<PilotAction> {
    let needle = req_arg(args, idx, "needle")?;
    let mut row = None;
    let mut index = None;
    let mut settle_ms = 200u64;
    while *idx < args.len() {
        if try_parse_text_target_flag(args, idx, &mut row, &mut index)? {
            continue;
        }
        if args[*idx] == "--settle-ms" {
            *idx += 1;
            settle_ms = req_arg(args, idx, "--settle-ms")?.parse()?;
            continue;
        }
        bail!("unknown flag '{}' for hover-text", args[*idx]);
    }
    Ok(PilotAction::HoverText {
        target: TextTarget { needle, row, index },
        settle_ms,
    })
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

#[cfg(test)]
mod tests {
    use super::{parse_click_text_action, parse_hover_text_action};
    use crate::protocol::{MouseBtn, MouseModifiers, PilotAction, TextTarget};

    fn to_args(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn parse_click_text_defaults_and_full_flags() {
        let args = to_args(&["紧凑引用胶囊视图"]);
        let mut idx = 0;
        let action =
            parse_click_text_action(&args, &mut idx, MouseBtn::Left, true, "click-text").unwrap();
        assert_eq!(
            action,
            PilotAction::ClickText {
                target: TextTarget {
                    needle: "紧凑引用胶囊视图".to_string(),
                    row: None,
                    index: None,
                },
                button: MouseBtn::Left,
                modifiers: MouseModifiers::default(),
                settle_ms: 200,
            }
        );

        let args = to_args(&[
            "Save",
            "--ctrl",
            "--row",
            "5",
            "--button",
            "middle",
            "--index",
            "2",
            "--shift",
            "--alt",
            "--settle-ms",
            "350",
        ]);
        let mut idx = 0;
        let action =
            parse_click_text_action(&args, &mut idx, MouseBtn::Left, true, "click-text").unwrap();
        assert_eq!(
            action,
            PilotAction::ClickText {
                target: TextTarget {
                    needle: "Save".to_string(),
                    row: Some(5),
                    index: Some(2),
                },
                button: MouseBtn::Middle,
                modifiers: MouseModifiers {
                    ctrl: true,
                    shift: true,
                    alt: true,
                },
                settle_ms: 350,
            }
        );
    }

    #[test]
    fn parse_rclick_text_uses_right_button_and_rejects_button_override() {
        let args = to_args(&["src/auth.rs", "--row", "7"]);
        let mut idx = 0;
        let action =
            parse_click_text_action(&args, &mut idx, MouseBtn::Right, false, "rclick-text")
                .unwrap();
        assert_eq!(
            action,
            PilotAction::ClickText {
                target: TextTarget {
                    needle: "src/auth.rs".to_string(),
                    row: Some(7),
                    index: None,
                },
                button: MouseBtn::Right,
                modifiers: MouseModifiers::default(),
                settle_ms: 200,
            }
        );

        let bad_args = to_args(&["src/auth.rs", "--button", "left"]);
        let mut idx = 0;
        let err =
            parse_click_text_action(&bad_args, &mut idx, MouseBtn::Right, false, "rclick-text")
                .unwrap_err();
        assert!(err.to_string().contains("unknown flag '--button'"));
    }

    #[test]
    fn parse_hover_text_parses_flags_and_rejects_unknown() {
        let args = to_args(&["分支▾", "--row", "3", "--index", "1", "--settle-ms", "120"]);
        let mut idx = 0;
        let action = parse_hover_text_action(&args, &mut idx).unwrap();
        assert_eq!(
            action,
            PilotAction::HoverText {
                target: TextTarget {
                    needle: "分支▾".to_string(),
                    row: Some(3),
                    index: Some(1),
                },
                settle_ms: 120,
            }
        );

        let bad_args = to_args(&["分支▾", "--ctrl"]);
        let mut idx = 0;
        let err = parse_hover_text_action(&bad_args, &mut idx).unwrap_err();
        assert!(err.to_string().contains("unknown flag '--ctrl'"));
    }
}
