mod app;
mod app_git;
mod app_stage;
mod branches_popup;
pub mod config;
mod dialogs;
mod external_editor;
mod graph;
mod hit_test;
pub mod i18n;
mod input;
mod input_menus;
mod input_stage;
pub mod keymap;
#[cfg(test)]
mod log_tests;
mod log_view;
mod menus;
mod stage_view;
mod stash_view;
mod syntax;
mod terminal;
#[cfg(test)]
mod tests;
mod theme;
mod views;
mod widgets;

use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{Event, EventStream};
use futures::StreamExt;
use jbgit_engine::{EngineEvent, EngineHandle, EngineSupervisor, RepoLocation};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

use crate::app::{Action, AppState, Effect, SyntaxJobKey, UiAsyncEvent, execute_effects};
use crate::external_editor::{effects_need_terminal_handoff, handle_external_editor_effects};
use crate::graph::GraphicsCapabilities;
use crate::input::route_terminal_event;
use crate::terminal::TerminalGuard;
use crate::views::render_app;

pub(crate) const MAX_COALESCE_EVENTS: usize = 64;

pub(crate) fn coalesce_effects(effects: Vec<Effect>) -> Vec<Effect> {
    let mut last_details = None;
    let mut last_file_diff = None;
    let mut last_stage_diff = None;
    let mut last_file_syntax = None;
    let mut last_stage_syntax = None;
    let mut last_containing = None;
    let mut last_rebuild_graph = None;

    for (i, eff) in effects.iter().enumerate() {
        match eff {
            Effect::RequestCommitDetails(_) => last_details = Some(i),
            Effect::RequestFileDiff(_) => last_file_diff = Some(i),
            Effect::RequestStageFileDiff(_) => last_stage_diff = Some(i),
            Effect::RequestSyntaxHighlight(job) => match job.key {
                SyntaxJobKey::File(_) => last_file_syntax = Some(i),
                SyntaxJobKey::Stage(_) => last_stage_syntax = Some(i),
            },
            Effect::ComputeContainingBranches(_) => last_containing = Some(i),
            Effect::RebuildLogGraph(_) => last_rebuild_graph = Some(i),
            _ => {}
        }
    }

    let mut out = Vec::with_capacity(effects.len());
    for (i, eff) in effects.into_iter().enumerate() {
        let keep = match &eff {
            Effect::RequestCommitDetails(_) => last_details == Some(i),
            Effect::RequestFileDiff(_) => last_file_diff == Some(i),
            Effect::RequestStageFileDiff(_) => last_stage_diff == Some(i),
            Effect::RequestSyntaxHighlight(job) => match job.key {
                SyntaxJobKey::File(_) => last_file_syntax == Some(i),
                SyntaxJobKey::Stage(_) => last_stage_syntax == Some(i),
            },
            Effect::ComputeContainingBranches(_) => last_containing == Some(i),
            Effect::RebuildLogGraph(_) => last_rebuild_graph == Some(i),
            _ => true,
        };
        if keep {
            out.push(eff);
        }
    }
    out
}

/// Polls `stream` once without suspending on `Poll::Pending`, while forwarding
/// the current async task's real `Context` waker rather than `noop_waker()`.
///
/// `crossterm::event::EventStream` only captures `cx.waker()` when its
/// `stream_wake_task_executed` flag transitions `false -> true` on `Poll::Pending`.
/// Polling via `FutureExt::now_or_never()` after consuming an event registers a
/// `noop_waker` with the background reader thread, permanently preventing
/// subsequent terminal input from waking `tokio::select!`.
pub(crate) async fn poll_stream_ready<S>(stream: &mut S) -> Option<Option<S::Item>>
where
    S: futures::Stream + Unpin,
{
    std::future::poll_fn(|cx| match std::pin::Pin::new(&mut *stream).poll_next(cx) {
        std::task::Poll::Ready(item) => std::task::Poll::Ready(Some(item)),
        std::task::Poll::Pending => std::task::Poll::Ready(None),
    })
    .await
}

fn handle_term_event(
    ev: Event,
    state: &mut AppState,
    batch_effects: &mut Vec<Effect>,
    cell_px_probe_deadline: &mut Option<tokio::time::Instant>,
) -> bool {
    let is_resize = matches!(ev, Event::Resize(..));
    if is_resize {
        // Font zoom (Ctrl +/-) arrives as a resize: re-calibrate cell pixels.
        let prev_cell_px = state.graph_renderer.capabilities().cell_px;
        let mut caps = GraphicsCapabilities::detect();
        if caps.needs_async_wezterm_probe() {
            caps.cell_px = prev_cell_px;
            *cell_px_probe_deadline =
                Some(tokio::time::Instant::now() + Duration::from_millis(100));
        }
        state.graph_renderer.set_capabilities(caps);
    }
    let actions = route_terminal_event(ev, state);
    for action in actions {
        let effs = state.dispatch(action);
        batch_effects.extend(effs);
    }
    is_resize || state.should_quit || effects_need_terminal_handoff(batch_effects)
}

fn parse_cli_args() -> Option<PathBuf> {
    let mut args = std::env::args().skip(1);
    let mut target_path = PathBuf::from(".");

    if let Some(arg) = args.next() {
        let cli_i18n = crate::i18n::I18n::load_for_cli();
        if arg == "-h" || arg == "--help" {
            println!("{}", cli_i18n.t("cli.tagline"));
            println!("{}", cli_i18n.t("cli.usage"));
            println!();
            println!("{}", cli_i18n.t("cli.options"));
            println!("{}", cli_i18n.t("cli.opt_help"));
            println!("{}", cli_i18n.t("cli.opt_print_default"));
            println!("{}", cli_i18n.t("cli.opt_init_config"));
            return None;
        }
        if arg == "--print-default-config" {
            let which = args.next();
            crate::config::AppConfig::print_default_config(which.as_deref());
            return None;
        }
        if arg == "--init-config" {
            match crate::config::AppConfig::init_config_detailed(None, false) {
                Ok((dir, files)) => {
                    for (name, created) in files {
                        let path_s = format!("{:?}", dir.join(name));
                        let msg_key = if created {
                            "cli.init_file_generated"
                        } else {
                            "cli.init_file_skipped"
                        };
                        println!("{}", cli_i18n.t_fmt(msg_key, &[("path", &path_s)]));
                    }
                    println!(
                        "{}",
                        cli_i18n.t_fmt("cli.init_ok", &[("dir", &format!("{dir:?}"))])
                    );
                }
                Err(e) => eprintln!(
                    "{}",
                    cli_i18n.t_fmt("cli.init_err", &[("err", &e.to_string())])
                ),
            }
            return None;
        }
        target_path = PathBuf::from(arg);
    }

    Some(target_path)
}

fn init_engine_and_state(
    target_path: PathBuf,
) -> (
    Option<EngineHandle>,
    Option<UnboundedReceiver<EngineEvent>>,
    AppState,
) {
    let cli_i18n = crate::i18n::I18n::load_for_cli();
    let (engine_handle, engine_rx, mut state) = match RepoLocation::discover(&target_path) {
        Ok(loc) => {
            let workdir = loc.workdir.clone();
            let config = crate::config::AppConfig::load(Some(&workdir));
            let scan_config = config.jbgit.performance.to_engine_config();
            match EngineSupervisor::start_with_config(loc, true, scan_config) {
                Ok((handle, rx)) => {
                    let s = AppState::new_with_config(workdir, None, config);
                    (Some(handle), Some(rx), s)
                }
                Err(e) => (
                    None,
                    None,
                    AppState::new_with_config(
                        target_path,
                        Some(cli_i18n.t_fmt("cli.engine_err", &[("err", &e.to_string())])),
                        config,
                    ),
                ),
            }
        }
        Err(e) => (None, None, AppState::new(target_path, Some(e.to_string()))),
    };
    state.async_graph_enabled = true;
    (engine_handle, engine_rx, state)
}

async fn drain_coalesced_events(
    event_stream: &mut EventStream,
    engine_rx: &mut Option<UnboundedReceiver<EngineEvent>>,
    ui_async_rx: &mut UnboundedReceiver<UiAsyncEvent>,
    state: &mut AppState,
    batch_effects: &mut Vec<Effect>,
    cell_px_probe_deadline: &mut Option<tokio::time::Instant>,
) {
    for _ in 1..MAX_COALESCE_EVENTS {
        if let Some(maybe_term) = poll_stream_ready(event_stream).await {
            match maybe_term {
                Some(Ok(ev)) => {
                    if handle_term_event(ev, state, batch_effects, cell_px_probe_deadline) {
                        break;
                    }
                    continue;
                }
                Some(Err(_)) | None => {
                    state.should_quit = true;
                    break;
                }
            }
        }
        if let Some(rx) = engine_rx.as_mut() {
            match rx.try_recv() {
                Ok(ev) => {
                    let effs = state.apply_engine_event(ev);
                    let stop = state.should_quit || effects_need_terminal_handoff(&effs);
                    batch_effects.extend(effs);
                    if stop {
                        break;
                    }
                    continue;
                }
                Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                    *engine_rx = None;
                }
                Err(tokio::sync::mpsc::error::TryRecvError::Empty) => {}
            }
        }
        match ui_async_rx.try_recv() {
            Ok(ui_ev) => {
                let effs = state.apply_ui_async_event(ui_ev);
                let stop = state.should_quit || effects_need_terminal_handoff(&effs);
                batch_effects.extend(effs);
                if stop {
                    break;
                }
            }
            Err(_) => break,
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let Some(target_path) = parse_cli_args() else {
        return Ok(());
    };

    let (engine_handle, mut engine_rx, mut state) = init_engine_and_state(target_path);
    let (ui_async_tx, mut ui_async_rx) =
        tokio::sync::mpsc::unbounded_channel::<UiAsyncEvent>();
    tokio::task::spawn_blocking(crate::syntax::warmup_syntax_assets);

    if let Some(handle) = engine_handle.as_ref() {
        let initial_effects = state.dispatch(Action::RefreshAll);
        execute_effects(initial_effects, &mut state, Some(handle), Some(&ui_async_tx));
    }

    let mut term_guard = Some(TerminalGuard::enter()?);
    let mut event_stream = EventStream::new();
    let initial_caps = GraphicsCapabilities::detect();
    state.graph_renderer.set_capabilities(initial_caps);
    let (cell_px_tx, mut cell_px_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut cell_px_probe_deadline: Option<tokio::time::Instant> = None;
    let mut cell_px_probe_in_flight = false;
    let mut cell_px_probe_pending = false;
    if initial_caps.needs_async_wezterm_probe() {
        cell_px_probe_in_flight = true;
        spawn_wezterm_cell_probe(cell_px_tx.clone());
    }

    while !state.should_quit {
        if state.needs_redraw {
            if let Some(guard) = term_guard.as_mut() {
                guard.terminal_mut().draw(|frame| {
                    render_app(frame, &mut state);
                })?;
                // Vector graph images (Kitty / Sixel / iTerm2) follow ratatui's flush.
                let _ = state.graph_renderer.write_emission(&mut std::io::stdout());
            }
            state.needs_redraw = false;
        }

        let details_deadline = state.next_deadline();
        let debounce_sleep = async {
            match details_deadline {
                Some(at) => tokio::time::sleep_until(tokio::time::Instant::from_std(at)).await,
                None => std::future::pending::<()>().await,
            }
        };

        let spinner_sleep = async {
            if state.has_active_tasks() {
                tokio::time::sleep(Duration::from_millis(100)).await;
            } else {
                std::future::pending::<()>().await;
            }
        };

        let cell_px_debounce_sleep = async {
            match cell_px_probe_deadline {
                Some(at) => tokio::time::sleep_until(at).await,
                None => std::future::pending::<()>().await,
            }
        };

        let mut batch_effects: Vec<Effect> = Vec::new();
        let mut allow_coalesce = false;

        tokio::select! {
            maybe_term = event_stream.next() => {
                match maybe_term {
                    Some(Ok(ev)) => {
                        let stop = handle_term_event(ev, &mut state, &mut batch_effects, &mut cell_px_probe_deadline);
                        allow_coalesce = !stop;
                    }
                    Some(Err(_)) | None => {
                        state.should_quit = true;
                    }
                }
            }
            maybe_engine = recv_engine(&mut engine_rx) => {
                if let Some(ev) = maybe_engine {
                    let effs = state.apply_engine_event(ev);
                    let stop = state.should_quit || effects_need_terminal_handoff(&effs);
                    batch_effects.extend(effs);
                    allow_coalesce = !stop;
                } else {
                    engine_rx = None;
                }
            }
            Some(ui_ev) = ui_async_rx.recv() => {
                let effs = state.apply_ui_async_event(ui_ev);
                let stop = state.should_quit || effects_need_terminal_handoff(&effs);
                batch_effects.extend(effs);
                allow_coalesce = !stop;
            }
            _ = debounce_sleep => {
                batch_effects.extend(state.dispatch(Action::FlushPendingDetails));
            }
            _ = spinner_sleep => {
                batch_effects.extend(state.dispatch(Action::TickSpinner));
            }
            _ = cell_px_debounce_sleep => {
                cell_px_probe_deadline = None;
                if cell_px_probe_in_flight {
                    cell_px_probe_pending = true;
                } else {
                    cell_px_probe_in_flight = true;
                    spawn_wezterm_cell_probe(cell_px_tx.clone());
                }
            }
            Some(maybe_cell_px) = cell_px_rx.recv() => {
                cell_px_probe_in_flight = false;
                if let Some(cell_px) = maybe_cell_px {
                    let mut caps = state.graph_renderer.capabilities();
                    if caps.protocol == Some(crate::graph::renderer::GraphicsProtocol::Iterm2)
                        && caps.cell_px != Some(cell_px)
                    {
                        caps.cell_px = Some(cell_px);
                        state.graph_renderer.set_capabilities(caps);
                        state.needs_redraw = true;
                    }
                }
                if std::mem::take(&mut cell_px_probe_pending) && cell_px_probe_deadline.is_none() {
                    cell_px_probe_in_flight = true;
                    spawn_wezterm_cell_probe(cell_px_tx.clone());
                }
            }
        }

        if allow_coalesce {
            drain_coalesced_events(
                &mut event_stream,
                &mut engine_rx,
                &mut ui_async_rx,
                &mut state,
                &mut batch_effects,
                &mut cell_px_probe_deadline,
            )
            .await;
        }

        if !batch_effects.is_empty() {
            let coalesced = coalesce_effects(batch_effects);
            let refresh_stage =
                handle_external_editor_effects(&coalesced, &mut state, &mut term_guard);
            execute_effects(coalesced, &mut state, engine_handle.as_ref(), Some(&ui_async_tx));
            if refresh_stage {
                execute_effects(
                    vec![Effect::RefreshScope(jbgit_core::RefreshScope::StageStatus)],
                    &mut state,
                    engine_handle.as_ref(),
                    Some(&ui_async_tx),
                );
            }
        }
    }

    state.stage_workbench.flush_draft_sync();
    let release = state.graph_renderer.shutdown_sequence();
    if !release.is_empty() {
        let _ = std::io::stdout().write_all(&release);
    }
    drop(term_guard);
    Ok(())
}

fn spawn_wezterm_cell_probe(tx: UnboundedSender<Option<crate::graph::renderer::CellPixelSize>>) {
    tokio::task::spawn_blocking(move || {
        let cell_px = crate::graph::probe::query_wezterm_cell_size(
            crate::graph::probe::WEZTERM_PROBE_TIMEOUT,
        );
        let _ = tx.send(cell_px);
    });
}

async fn recv_engine(rx: &mut Option<UnboundedReceiver<EngineEvent>>) -> Option<EngineEvent> {
    match rx {
        Some(channel) => channel.recv().await,
        None => std::future::pending().await,
    }
}
