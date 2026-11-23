//! Deterministic single-turn TUI session driver shared by both the production
//! event loop (`src/main.rs`) and the test suite (`src/tests/*`).
//!
//! [`TuiSession`] encapsulates the entire turn pipeline:
//! event coalescing (up to [`MAX_COALESCE_EVENTS`]) → pure input routing →
//! state dispatch → [`coalesce_effects`] → terminal handoff & effect execution
//! through the injected [`EngineBackend`] and [`SessionHost`] adapters →
//! virtual/monotonic deadline scheduling and rendering.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use crossterm::event::Event;
use jbgit_core::{RefreshScope, RequestSlotId};
use jbgit_engine::{EngineBackend, EngineEvent, EngineHandle, EngineSupervisor, RepoLocation};
use ratatui::Frame;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

use crate::app::{
    Action, AppState, ContainingBranchesJob, Effect, SyntaxJob, SyntaxJobKey, UiAsyncEvent,
    execute_effects_with_host,
};
use crate::external_editor::{effects_need_terminal_handoff, handle_external_editor_effects};
use crate::graph::GraphicsCapabilities;
use crate::graph::renderer::CellPixelSize;
use crate::graph::state::LogGraphJob;
use crate::input::route_terminal_event;
use crate::terminal::TerminalGuard;
use crate::views::render_app;

/// Maximum number of ready terminal/engine/async events drained into a single
/// turn before coalescing effects and rendering.
pub const MAX_COALESCE_EVENTS: usize = 64;

/// Coalesces redundant single-flight requests within a batch while preserving
/// ordering and keeping every non-coalescable side effect intact.
pub fn coalesce_effects(effects: Vec<Effect>) -> Vec<Effect> {
    let mut last_details = [None; RequestSlotId::COUNT];
    let mut last_file_diff = [None; RequestSlotId::COUNT];
    let mut last_stage_diff = None;
    let mut last_blame = None;
    let mut last_file_syntax = None;
    let mut last_stage_syntax = None;
    let mut last_blame_syntax = None;
    let mut last_range_syntax = None;
    let mut last_containing = None;
    let mut last_rebuild_graph = None;

    for (i, eff) in effects.iter().enumerate() {
        match eff {
            Effect::RequestCommitDetails { slot, .. } => last_details[slot.index()] = Some(i),
            Effect::RequestFileDiff { slot, .. } => last_file_diff[slot.index()] = Some(i),
            Effect::RequestStageFileDiff(_) => last_stage_diff = Some(i),
            Effect::RequestBlame(_) => last_blame = Some(i),
            Effect::RequestSyntaxHighlight(job) => match job.key {
                SyntaxJobKey::File(_) => last_file_syntax = Some(i),
                SyntaxJobKey::Stage(_) => last_stage_syntax = Some(i),
                SyntaxJobKey::Blame(_) => last_blame_syntax = Some(i),
                SyntaxJobKey::RangePatch { .. } => last_range_syntax = Some(i),
            },
            Effect::ComputeContainingBranches(_) => last_containing = Some(i),
            Effect::RebuildLogGraph(_) => last_rebuild_graph = Some(i),
            _ => {}
        }
    }

    let mut out = Vec::with_capacity(effects.len());
    for (i, eff) in effects.into_iter().enumerate() {
        let keep = match &eff {
            Effect::RequestCommitDetails { slot, .. } => last_details[slot.index()] == Some(i),
            Effect::RequestFileDiff { slot, .. } => last_file_diff[slot.index()] == Some(i),
            Effect::RequestStageFileDiff(_) => last_stage_diff == Some(i),
            Effect::RequestBlame(_) => last_blame == Some(i),
            Effect::RequestSyntaxHighlight(job) => match job.key {
                SyntaxJobKey::File(_) => last_file_syntax == Some(i),
                SyntaxJobKey::Stage(_) => last_stage_syntax == Some(i),
                SyntaxJobKey::Blame(_) => last_blame_syntax == Some(i),
                SyntaxJobKey::RangePatch { .. } => last_range_syntax == Some(i),
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
pub async fn poll_stream_ready<S>(stream: &mut S) -> Option<Option<S::Item>>
where
    S: futures::Stream + Unpin,
{
    std::future::poll_fn(|cx| match std::pin::Pin::new(&mut *stream).poll_next(cx) {
        std::task::Poll::Ready(item) => std::task::Poll::Ready(Some(item)),
        std::task::Poll::Pending => std::task::Poll::Ready(None),
    })
    .await
}

/// One discrete input delivered to [`TuiSession::step_turn`].
#[derive(Debug)]
pub enum SessionInput {
    Terminal(Event),
    Engine(EngineEvent),
    UiAsync(UiAsyncEvent),
    Action(Action),
    FlushPendingDetails,
    TickSpinner,
}

/// Summary of work performed during a single [`TuiSession`] turn.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct TurnReport {
    pub processed_inputs: usize,
    pub raw_effects_count: usize,
    pub coalesced_effects_count: usize,
    pub stopped_for_handoff: bool,
    pub stopped_for_resize: bool,
}

/// Host-environment seam separating production OS/terminal I/O from
/// deterministic in-memory test adapters.
pub trait SessionHost {
    fn copy_to_clipboard(&mut self, text: &str);
    fn on_resize(&mut self, state: &mut AppState);
    fn handle_terminal_handoff(&mut self, effects: &[Effect], state: &mut AppState) -> bool;
    fn spawn_containing_branches(&mut self, job: ContainingBranchesJob, state: &mut AppState);
    fn spawn_rebuild_log_graph(&mut self, job: Box<LogGraphJob>, state: &mut AppState);
    fn spawn_syntax_highlight(&mut self, job: SyntaxJob, state: &mut AppState);
}

/// Production host adapter bridging [`TuiSession`] to Crossterm/TerminalGuard,
/// system/OSC-52 clipboard, `$EDITOR` subprocesses, and Tokio blocking workers.
pub struct ProdHost {
    pub term_guard: Option<TerminalGuard>,
    pub ui_async_tx: UnboundedSender<UiAsyncEvent>,
    pub cell_px_probe_deadline: Option<tokio::time::Instant>,
}

impl ProdHost {
    pub fn new(term_guard: Option<TerminalGuard>, ui_async_tx: UnboundedSender<UiAsyncEvent>) -> Self {
        Self {
            term_guard,
            ui_async_tx,
            cell_px_probe_deadline: None,
        }
    }
}

impl SessionHost for ProdHost {
    fn copy_to_clipboard(&mut self, text: &str) {
        crate::terminal::copy_to_clipboard(text);
    }

    fn on_resize(&mut self, state: &mut AppState) {
        // Font zoom (Ctrl +/-) arrives as a resize: re-calibrate cell pixels.
        let prev_cell_px = state.log.graph_renderer.capabilities().cell_px;
        let mut caps = GraphicsCapabilities::detect();
        if caps.needs_async_wezterm_probe() {
            caps.cell_px = prev_cell_px;
            self.cell_px_probe_deadline =
                Some(tokio::time::Instant::now() + Duration::from_millis(100));
        }
        state.log.graph_renderer.set_capabilities(caps);
    }

    fn handle_terminal_handoff(&mut self, effects: &[Effect], state: &mut AppState) -> bool {
        handle_external_editor_effects(effects, state, &mut self.term_guard)
    }

    fn spawn_containing_branches(&mut self, job: ContainingBranchesJob, _state: &mut AppState) {
        let tx = self.ui_async_tx.clone();
        tokio::task::spawn_blocking(move || {
            if let Some(names) = job.run() {
                let _ = tx.send(UiAsyncEvent::ContainingBranches {
                    oid: job.oid,
                    token: job.token,
                    names,
                });
            }
        });
    }

    fn spawn_rebuild_log_graph(&mut self, job: Box<LogGraphJob>, _state: &mut AppState) {
        let tx = self.ui_async_tx.clone();
        tokio::task::spawn_blocking(move || {
            if let Some(rebuilt) = job.run() {
                let _ = tx.send(UiAsyncEvent::LogGraphRebuilt(Box::new(rebuilt)));
            }
        });
    }

    fn spawn_syntax_highlight(&mut self, job: SyntaxJob, _state: &mut AppState) {
        let tx = self.ui_async_tx.clone();
        tokio::task::spawn_blocking(move || {
            if let Some(ev) = job.run() {
                let _ = tx.send(ev);
            }
        });
    }
}

/// Recorded external editor / mergetool invocation captured by [`TestHost`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExternalEditorCall {
    CommitMessage,
    File(String),
    Mergetool(String),
}

/// Deterministic in-memory host adapter for tests: zero disk/clipboard/process
/// side effects, full observability of clipboard writes, editor calls, and
/// optional inline/deferred async worker execution.
#[derive(Debug, Default)]
pub struct TestHost {
    pub clipboard: Vec<String>,
    pub external_editor_calls: Vec<ExternalEditorCall>,
    pub resize_count: usize,
    pub run_syntax_inline: bool,
    pub defer_async_jobs: bool,
    pub pending_containing: Vec<ContainingBranchesJob>,
    pub pending_graph_rebuilds: Vec<Box<LogGraphJob>>,
    pub pending_syntax: Vec<SyntaxJob>,
}

impl TestHost {
    pub fn new() -> Self {
        Self::default()
    }
}

impl SessionHost for TestHost {
    fn copy_to_clipboard(&mut self, text: &str) {
        self.clipboard.push(text.to_string());
    }

    fn on_resize(&mut self, _state: &mut AppState) {
        self.resize_count += 1;
    }

    fn handle_terminal_handoff(&mut self, effects: &[Effect], state: &mut AppState) -> bool {
        let mut refresh_stage = false;
        for eff in effects {
            match eff {
                Effect::OpenExternalEditorMessage => {
                    self.external_editor_calls
                        .push(ExternalEditorCall::CommitMessage);
                    state.needs_redraw = true;
                }
                Effect::OpenExternalEditorFile(rel) => {
                    self.external_editor_calls
                        .push(ExternalEditorCall::File(rel.clone()));
                    state.needs_redraw = true;
                    refresh_stage = true;
                }
                Effect::OpenExternalMergetool(rel) => {
                    self.external_editor_calls
                        .push(ExternalEditorCall::Mergetool(rel.clone()));
                    state.needs_redraw = true;
                    refresh_stage = true;
                }
                _ => {}
            }
        }
        refresh_stage
    }

    fn spawn_containing_branches(&mut self, job: ContainingBranchesJob, state: &mut AppState) {
        if self.defer_async_jobs {
            self.pending_containing.push(job);
        } else if let Some(names) = job.run() {
            let _ = state.apply_ui_async_event(UiAsyncEvent::ContainingBranches {
                oid: job.oid,
                token: job.token,
                names,
            });
        }
    }

    fn spawn_rebuild_log_graph(&mut self, job: Box<LogGraphJob>, state: &mut AppState) {
        if self.defer_async_jobs {
            self.pending_graph_rebuilds.push(job);
        } else if let Some(rebuilt) = job.run() {
            let _ = state.apply_ui_async_event(UiAsyncEvent::LogGraphRebuilt(Box::new(rebuilt)));
        }
    }

    fn spawn_syntax_highlight(&mut self, job: SyntaxJob, state: &mut AppState) {
        if self.defer_async_jobs {
            self.pending_syntax.push(job);
        } else if self.run_syntax_inline
            && let Some(ev) = job.run()
        {
            let _ = state.apply_ui_async_event(ev);
        }
    }
}

/// Deep module driving deterministic single-turn TUI execution.
pub struct TuiSession {
    state: AppState,
    clock: Instant,
}

impl TuiSession {
    pub fn new(state: AppState) -> Self {
        Self::new_at(state, Instant::now())
    }

    pub fn new_at(mut state: AppState, clock: Instant) -> Self {
        state.clock = Some(clock);
        Self { state, clock }
    }

    pub fn state(&self) -> &AppState {
        &self.state
    }

    pub fn state_mut(&mut self) -> &mut AppState {
        &mut self.state
    }

    pub fn now(&self) -> Instant {
        self.clock
    }

    pub fn should_quit(&self) -> bool {
        self.state.should_quit
    }

    pub fn set_should_quit(&mut self, quit: bool) {
        self.state.should_quit = quit;
    }

    pub fn needs_redraw(&self) -> bool {
        self.state.needs_redraw
    }

    pub fn next_deadline(&self) -> Option<Instant> {
        self.state.next_deadline()
    }

    pub fn has_active_tasks(&self) -> bool {
        self.state.has_active_tasks()
    }

    pub fn set_graphics_capabilities(&mut self, caps: GraphicsCapabilities) {
        self.state.log.graph_renderer.set_capabilities(caps);
    }

    pub fn apply_wezterm_cell_px(&mut self, cell_px: CellPixelSize) {
        let mut caps = self.state.log.graph_renderer.capabilities();
        if caps.protocol == Some(crate::graph::renderer::GraphicsProtocol::Iterm2)
            && caps.cell_px != Some(cell_px)
        {
            caps.cell_px = Some(cell_px);
            self.state.log.graph_renderer.set_capabilities(caps);
            self.state.needs_redraw = true;
        }
    }

    /// Processes a batch of ready inputs (capped at [`MAX_COALESCE_EVENTS`]) in
    /// one deterministic turn, coalesces all produced effects, and executes them
    /// through the injected `engine` and `host` adapters.
    pub fn step_turn(
        &mut self,
        inputs: impl IntoIterator<Item = SessionInput>,
        now: Instant,
        engine: Option<&dyn EngineBackend>,
        host: &mut dyn SessionHost,
    ) -> TurnReport {
        self.clock = now;
        self.state.clock = Some(now);

        let mut report = TurnReport::default();
        let mut batch_effects: Vec<Effect> = Vec::new();

        for input in inputs.into_iter().take(MAX_COALESCE_EVENTS) {
            report.processed_inputs += 1;
            let stop = self.ingest_single(input, &mut batch_effects, host, &mut report);
            if stop {
                break;
            }
        }

        self.finish_batch(batch_effects, engine, host, &mut report);
        report
    }

    /// Processes a single input into `batch_effects` (used by async streaming
    /// drain loops that stop pulling when a resize/quit/handoff occurs).
    pub fn ingest_into_batch(
        &mut self,
        input: SessionInput,
        now: Instant,
        batch_effects: &mut Vec<Effect>,
        host: &mut dyn SessionHost,
        report: &mut TurnReport,
    ) -> bool {
        self.clock = now;
        self.state.clock = Some(now);
        report.processed_inputs += 1;
        self.ingest_single(input, batch_effects, host, report)
    }

    /// Coalesces and executes a previously accumulated batch of effects.
    pub fn finish_batch(
        &mut self,
        batch_effects: Vec<Effect>,
        engine: Option<&dyn EngineBackend>,
        host: &mut dyn SessionHost,
        report: &mut TurnReport,
    ) {
        report.raw_effects_count = batch_effects.len();
        if batch_effects.is_empty() {
            return;
        }
        let coalesced = coalesce_effects(batch_effects);
        report.coalesced_effects_count = coalesced.len();
        let refresh_stage = host.handle_terminal_handoff(&coalesced, &mut self.state);
        execute_effects_with_host(coalesced, &mut self.state, engine, host);
        if refresh_stage {
            execute_effects_with_host(
                vec![Effect::RefreshScope(RefreshScope::StageStatus)],
                &mut self.state,
                engine,
                host,
            );
        }
    }

    fn ingest_single(
        &mut self,
        input: SessionInput,
        batch_effects: &mut Vec<Effect>,
        host: &mut dyn SessionHost,
        report: &mut TurnReport,
    ) -> bool {
        Self::ingest_into_state(&mut self.state, input, batch_effects, host, report)
    }

    fn ingest_into_state(
        state: &mut AppState,
        input: SessionInput,
        batch_effects: &mut Vec<Effect>,
        host: &mut dyn SessionHost,
        report: &mut TurnReport,
    ) -> bool {
        let mut push_effects = |effs: Vec<Effect>, state: &AppState| -> bool {
            let handoff = effects_need_terminal_handoff(&effs);
            if handoff {
                report.stopped_for_handoff = true;
            }
            batch_effects.extend(effs);
            state.should_quit || handoff
        };
        match input {
            SessionInput::Terminal(ev) => {
                let is_resize = matches!(ev, Event::Resize(..));
                if is_resize {
                    host.on_resize(state);
                    report.stopped_for_resize = true;
                }
                let actions = route_terminal_event(&ev, state);
                let mut effs = Vec::new();
                for action in actions {
                    effs.extend(state.dispatch(action));
                }
                push_effects(effs, state) || is_resize
            }
            SessionInput::Engine(ev) => {
                let effs = state.apply_engine_event(ev);
                push_effects(effs, state)
            }
            SessionInput::UiAsync(ui_ev) => {
                let effs = state.apply_ui_async_event(ui_ev);
                push_effects(effs, state)
            }
            SessionInput::Action(action) => {
                let effs = state.dispatch(action);
                push_effects(effs, state)
            }
            SessionInput::FlushPendingDetails => {
                batch_effects.extend(state.dispatch(Action::FlushPendingDetails));
                false
            }
            SessionInput::TickSpinner => {
                batch_effects.extend(state.dispatch(Action::TickSpinner));
                false
            }
        }
    }

    /// Advances the session's monotonic clock to `at` and flushes any due
    /// debounced requests, draft saves, hover timers, and key timeouts.
    pub fn advance_to(
        &mut self,
        at: Instant,
        engine: Option<&dyn EngineBackend>,
        host: &mut dyn SessionHost,
    ) -> TurnReport {
        self.step_turn([SessionInput::FlushPendingDetails], at, engine, host)
    }

    /// Renders the current UI frame and clears `needs_redraw`.
    pub fn render(&mut self, frame: &mut Frame<'_>) {
        render_app(frame, &mut self.state);
        self.state.needs_redraw = false;
    }

    /// Emits any pending vector graphics bytes (Kitty / Sixel / iTerm2) after a frame flush.
    pub fn write_graphics_emission(&mut self, w: &mut impl std::io::Write) -> std::io::Result<()> {
        self.state.log.graph_renderer.write_emission(w)
    }

    /// Flushes any pending commit-message draft synchronously and returns any
    /// graphics protocol shutdown escape sequence.
    pub fn shutdown(&mut self) -> Vec<u8> {
        self.state.stage_workbench.flush_draft_sync();
        self.state.log.graph_renderer.shutdown_sequence()
    }
}

/// Discovers the repository at `target_path`, starts the background
/// [`EngineSupervisor`], and builds a production [`TuiSession`].
pub fn init_engine_and_session(
    target_path: PathBuf,
) -> (
    Option<EngineHandle>,
    Option<UnboundedReceiver<EngineEvent>>,
    TuiSession,
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
    (engine_handle, engine_rx, TuiSession::new(state))
}

pub fn spawn_wezterm_cell_probe(tx: UnboundedSender<Option<CellPixelSize>>) {
    tokio::task::spawn_blocking(move || {
        let cell_px = crate::graph::probe::query_wezterm_cell_size(
            crate::graph::probe::WEZTERM_PROBE_TIMEOUT,
        );
        let _ = tx.send(cell_px);
    });
}

pub async fn recv_engine(rx: &mut Option<UnboundedReceiver<EngineEvent>>) -> Option<EngineEvent> {
    match rx {
        Some(channel) => channel.recv().await,
        None => std::future::pending().await,
    }
}
