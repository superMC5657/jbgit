use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::event::EventStream;
use futures::StreamExt;
use jbgit_engine::EngineBackend;
use jbgit_engine::askpass::{
    AskpassRequest, AskpassResponder, AskpassServer, ChannelResponder, FixedResponder,
};
use jbgit_tui::graph::GraphicsCapabilities;
use jbgit_tui::terminal::TerminalGuard;
use jbgit_tui::{
    Action, MAX_COALESCE_EVENTS, ProdHost, RepoSession, SessionInput, TuiSession, TurnReport,
    UiAsyncEvent, init_engine_and_session, poll_stream_ready, recv_engine,
    spawn_wezterm_cell_probe,
};

fn parse_cli_args() -> Option<PathBuf> {
    let mut args = std::env::args().skip(1);
    let Some(arg) = args.next() else {
        return Some(PathBuf::from("."));
    };
    let cli_i18n = jbgit_tui::i18n::I18n::load_for_cli();
    match arg.as_str() {
        "-h" | "--help" => {
            for k in ["cli.tagline", "cli.usage", "", "cli.options", "cli.opt_help", "cli.opt_print_default", "cli.opt_init_config"] {
                if k.is_empty() { println!(); } else { println!("{}", cli_i18n.t(k)); }
            }
            None
        }
        "--print-default-config" => {
            jbgit_tui::config::AppConfig::print_default_config(args.next().as_deref());
            None
        }
        "--init-config" => {
            match jbgit_tui::config::AppConfig::init_config_detailed(None, false) {
                Ok((dir, files)) => {
                    for (name, created) in files {
                        let path_s = format!("{:?}", dir.join(name));
                        let key = if created { "cli.init_file_generated" } else { "cli.init_file_skipped" };
                        println!("{}", cli_i18n.t_fmt(key, &[("path", &path_s)]));
                    }
                    println!("{}", cli_i18n.t_fmt("cli.init_ok", &[("dir", &format!("{dir:?}"))]));
                }
                Err(e) => eprintln!("{}", cli_i18n.t_fmt("cli.init_err", &[("err", &e.to_string())])),
            }
            None
        }
        other => Some(PathBuf::from(other)),
    }
}

/// `GIT_ASKPASS` / `SSH_ASKPASS` helper mode: `jbgit <prompt>` when launched
/// by git / ssh with the session variables set, or the explicit (hidden)
/// `jbgit askpass <prompt>`. Never listed in `--help`.
fn askpass_client_mode() -> Option<i32> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("askpass") {
        return Some(jbgit_engine::askpass::run_client(&args[1..].join(" ")));
    }
    jbgit_engine::askpass::client_invocation(&args).map(jbgit_engine::askpass::run_client)
}

/// Starts this session's credential bridge. `JBGIT_ASKPASS_RESPONDER=cancel`
/// or `=value:<text>` swaps the dialog for a deterministic responder
/// (`jbgit-pilot` sandboxes / scripted QA).
fn start_askpass(
    ui_tx: tokio::sync::mpsc::UnboundedSender<SessionInput>,
) -> Option<AskpassServer> {
    let responder: Arc<dyn AskpassResponder> =
        match std::env::var("JBGIT_ASKPASS_RESPONDER").ok().as_deref() {
            Some("cancel") => Arc::new(FixedResponder::cancelling()),
            Some(v) if v.starts_with("value:") => {
                Arc::new(FixedResponder::answering(&v["value:".len()..]))
            }
            _ => {
                let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<AskpassRequest>();
                tokio::spawn(async move {
                    while let Some(req) = rx.recv().await {
                        // Not generation-stamped: a prompt belongs to a live
                        // git process and is answered whatever repository is bound.
                        if let Err(e) = ui_tx.send(SessionInput::UiAsync(UiAsyncEvent::Askpass(req)))
                            && let SessionInput::UiAsync(UiAsyncEvent::Askpass(req)) = e.0
                        {
                            req.respond(None);
                        }
                    }
                });
                Arc::new(ChannelResponder::new(tx))
            }
        };
    AskpassServer::start(responder).ok()
}

/// In-process repository switch (Spec 12 Issue 09): validate the target,
/// unbind the current repository (cancel slots, flush the draft, stop the
/// engine waiting ≤ 5 s for writes, release graphics), bind the new one and
/// load it. A target that is not a repository leaves the session untouched.
async fn switch_repository(
    session: &mut TuiSession,
    repo: &mut RepoSession,
    host: &mut ProdHost,
    target: &std::path::Path,
    askpass: Option<jbgit_engine::askpass::AskpassEnv>,
) {
    let loc = match RepoSession::discover(target) {
        Ok(loc) => loc,
        Err(e) => {
            session.report_repo_switch_failure(target, &e);
            return;
        }
    };
    if jbgit_core::same_path(&loc.workdir, &session.current_workdir()) {
        session.report_repo_already_open(target);
        return;
    }
    let release = session.prepare_unbind();
    if !release.is_empty() {
        let _ = std::io::stdout().write_all(&release);
    }
    let old = std::mem::replace(repo, RepoSession::detached());
    let writes_finished = old.unbind().await;
    let (next_repo, next_state) = RepoSession::bind_location(loc, askpass);
    *repo = next_repo;
    session.rebind(next_state, writes_finished);
    repo.generation = session.generation();
    if repo.handle.is_some() {
        session.step_turn([SessionInput::Action(Action::RefreshAll)], Instant::now(), repo.backend(), host);
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    if let Some(code) = askpass_client_mode() {
        std::process::exit(code);
    }
    let Some(target_path) = parse_cli_args() else {
        return Ok(());
    };

    let (ui_async_tx, mut ui_async_rx) = tokio::sync::mpsc::unbounded_channel::<SessionInput>();
    let askpass_server = start_askpass(ui_async_tx.clone());
    let askpass_env = askpass_server
        .as_ref()
        .and_then(|s| s.env_for_current_exe().ok());
    let (mut repo, mut session) = init_engine_and_session(target_path, askpass_env.clone());
    repo.generation = session.generation();
    tokio::task::spawn_blocking(jbgit_tui::syntax::warmup_syntax_assets);

    let mut host = ProdHost::new(Some(TerminalGuard::enter()?), ui_async_tx);
    if repo.handle.is_some() {
        session.step_turn([SessionInput::Action(Action::RefreshAll)], Instant::now(), repo.backend(), &mut host);
    }

    let mut event_stream = EventStream::new();
    let initial_caps = GraphicsCapabilities::detect();
    session.set_graphics_capabilities(initial_caps);
    let (cell_px_tx, mut cell_px_rx) = tokio::sync::mpsc::unbounded_channel();
    let (mut probe_in_flight, mut probe_pending) = (false, false);
    if initial_caps.needs_async_wezterm_probe() {
        probe_in_flight = true;
        spawn_wezterm_cell_probe(cell_px_tx.clone());
    }

    while !session.should_quit() {
        let engine_ref = repo.handle.as_ref().map(|h| h as &dyn EngineBackend);
        let generation = repo.generation;
        if session.needs_redraw()
            && let Some(guard) = host.term_guard.as_mut()
        {
            guard.terminal_mut().draw(|frame| session.render(frame))?;
            let _ = session.write_graphics_emission(&mut std::io::stdout());
        }

        let debounce_sleep = async {
            match session.next_deadline() {
                Some(at) => tokio::time::sleep_until(tokio::time::Instant::from_std(at)).await,
                None => std::future::pending::<()>().await,
            }
        };
        let spinner_sleep = async {
            if session.has_active_tasks() {
                tokio::time::sleep(Duration::from_millis(100)).await;
            } else {
                std::future::pending::<()>().await;
            }
        };
        let probe_sleep = async {
            match host.cell_px_probe_deadline {
                Some(at) => tokio::time::sleep_until(at).await,
                None => std::future::pending::<()>().await,
            }
        };

        let mut batch_effects = Vec::new();
        let mut report = TurnReport::default();
        let mut allow_coalesce = false;

        tokio::select! {
            maybe_term = event_stream.next() => match maybe_term {
                Some(Ok(ev)) => allow_coalesce = !session.ingest_into_batch(SessionInput::Terminal(ev), Instant::now(), &mut batch_effects, &mut host, &mut report),
                Some(Err(_)) | None => session.set_should_quit(true),
            },
            maybe_engine = recv_engine(&mut repo.events) => match maybe_engine {
                Some(ev) => allow_coalesce = !session.ingest_into_batch(SessionInput::EngineFrom(generation, ev), Instant::now(), &mut batch_effects, &mut host, &mut report),
                None => repo.events = None,
            },
            Some(ui_input) = ui_async_rx.recv() => {
                allow_coalesce = !session.ingest_into_batch(ui_input, Instant::now(), &mut batch_effects, &mut host, &mut report);
            }
            _ = debounce_sleep => {
                session.ingest_into_batch(SessionInput::FlushPendingDetails, Instant::now(), &mut batch_effects, &mut host, &mut report);
            }
            _ = spinner_sleep => {
                session.ingest_into_batch(SessionInput::TickSpinner, Instant::now(), &mut batch_effects, &mut host, &mut report);
            }
            _ = probe_sleep => {
                host.cell_px_probe_deadline = None;
                if probe_in_flight { probe_pending = true; } else { probe_in_flight = true; spawn_wezterm_cell_probe(cell_px_tx.clone()); }
            }
            Some(maybe_cell_px) = cell_px_rx.recv() => {
                probe_in_flight = false;
                if let Some(cell_px) = maybe_cell_px { session.apply_wezterm_cell_px(cell_px); }
                if std::mem::take(&mut probe_pending) && host.cell_px_probe_deadline.is_none() {
                    probe_in_flight = true;
                    spawn_wezterm_cell_probe(cell_px_tx.clone());
                }
            }
        }

        if allow_coalesce {
            for _ in 1..MAX_COALESCE_EVENTS {
                let now = Instant::now();
                if let Some(maybe_term) = poll_stream_ready(&mut event_stream).await {
                    match maybe_term {
                        Some(Ok(ev)) => if session.ingest_into_batch(SessionInput::Terminal(ev), now, &mut batch_effects, &mut host, &mut report) { break; } else { continue; },
                        Some(Err(_)) | None => { session.set_should_quit(true); break; }
                    }
                }
                if let Some(rx) = repo.events.as_mut() {
                    match rx.try_recv() {
                        Ok(ev) => if session.ingest_into_batch(SessionInput::EngineFrom(generation, ev), now, &mut batch_effects, &mut host, &mut report) { break; } else { continue; },
                        Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => repo.events = None,
                        Err(tokio::sync::mpsc::error::TryRecvError::Empty) => {}
                    }
                }
                match ui_async_rx.try_recv() {
                    Ok(ui_input) => if session.ingest_into_batch(ui_input, now, &mut batch_effects, &mut host, &mut report) { break; },
                    Err(_) => break,
                }
            }
        }

        session.finish_batch(batch_effects, engine_ref, &mut host, &mut report);

        if let Some(target) = session.take_repo_switch_request() {
            switch_repository(&mut session, &mut repo, &mut host, &target, askpass_env.clone()).await;
        }
    }

    let release = session.shutdown();
    if !release.is_empty() {
        let _ = std::io::stdout().write_all(&release);
    }
    drop(host.term_guard);
    drop(askpass_server);
    Ok(())
}
