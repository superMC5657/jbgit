//! Deterministic single-turn TUI session driver shared by both the production
//! event loop (`src/main.rs`) and the test suite (`src/tests/*`).
//!
//! [`TuiSession`] encapsulates the entire turn pipeline:
//! event coalescing (up to [`MAX_COALESCE_EVENTS`]) → pure input routing →
//! state dispatch → [`coalesce_effects`] → terminal handoff & effect execution
//! through the injected [`EngineBackend`] and [`SessionHost`] adapters →
//! virtual/monotonic deadline scheduling and rendering.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crossterm::event::Event;
use jbgit_core::{RefreshScope, RequestSlotId};
use jbgit_engine::{EngineBackend, EngineEvent, EngineHandle, EngineSupervisor, RepoLocation};
use ratatui::Frame;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

use crate::app::{
    Action, AppState, ContainingBranchesJob, Effect, SyntaxJob, SyntaxJobKey, SyntaxTarget,
    UiAsyncEvent, execute_effects_with_host,
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
    let mut last_tree_syntax = None;
    let mut last_containing = None;
    let mut last_rebuild_graph = None;
    let mut last_commit_signatures = None;

    for (i, eff) in effects.iter().enumerate() {
        match eff {
            Effect::RequestCommitDetails { slot, .. } => last_details[slot.index()] = Some(i),
            Effect::RequestCommitSignatures(_) => last_commit_signatures = Some(i),
            Effect::RequestFileDiff { slot, .. } => last_file_diff[slot.index()] = Some(i),
            Effect::RequestStageFileDiff(_) => last_stage_diff = Some(i),
            Effect::RequestBlame(_) => last_blame = Some(i),
            Effect::RequestSyntaxHighlight(job) => match &job.target {
                SyntaxTarget::File {
                    key: SyntaxJobKey::RangePatch { .. },
                    ..
                } => last_range_syntax = Some(i),
                SyntaxTarget::File { .. } => last_file_syntax = Some(i),
                SyntaxTarget::Stage(_) => last_stage_syntax = Some(i),
                SyntaxTarget::Blame(_) => last_blame_syntax = Some(i),
                SyntaxTarget::TreeBlob(_) => last_tree_syntax = Some(i),
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
            Effect::RequestCommitSignatures(_) => last_commit_signatures == Some(i),
            Effect::RequestFileDiff { slot, .. } => last_file_diff[slot.index()] == Some(i),
            Effect::RequestStageFileDiff(_) => last_stage_diff == Some(i),
            Effect::RequestBlame(_) => last_blame == Some(i),
            Effect::RequestSyntaxHighlight(job) => match &job.target {
                SyntaxTarget::File {
                    key: SyntaxJobKey::RangePatch { .. },
                    ..
                } => last_range_syntax == Some(i),
                SyntaxTarget::File { .. } => last_file_syntax == Some(i),
                SyntaxTarget::Stage(_) => last_stage_syntax == Some(i),
                SyntaxTarget::Blame(_) => last_blame_syntax == Some(i),
                SyntaxTarget::TreeBlob(_) => last_tree_syntax == Some(i),
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
    /// Engine event of the currently bound repository.
    Engine(EngineEvent),
    /// Engine event stamped with the repository session generation it was
    /// produced for; dropped when the session has been rebound since.
    EngineFrom(u64, EngineEvent),
    UiAsync(UiAsyncEvent),
    /// Background UI job result stamped with its session generation.
    UiAsyncFrom(u64, UiAsyncEvent),
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
    /// Background job results, stamped with the session generation that
    /// spawned them ([`SessionInput::UiAsyncFrom`]).
    pub ui_async_tx: UnboundedSender<SessionInput>,
    pub cell_px_probe_deadline: Option<tokio::time::Instant>,
}

impl ProdHost {
    pub fn new(term_guard: Option<TerminalGuard>, ui_async_tx: UnboundedSender<SessionInput>) -> Self {
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

    fn spawn_containing_branches(&mut self, job: ContainingBranchesJob, state: &mut AppState) {
        let tx = self.ui_async_tx.clone();
        let generation = state.session_generation;
        tokio::task::spawn_blocking(move || {
            if let Some(names) = job.run() {
                let ev = UiAsyncEvent::ContainingBranches {
                    oid: job.oid,
                    token: job.token,
                    names,
                };
                let _ = tx.send(SessionInput::UiAsyncFrom(generation, ev));
            }
        });
    }

    fn spawn_rebuild_log_graph(&mut self, job: Box<LogGraphJob>, state: &mut AppState) {
        let tx = self.ui_async_tx.clone();
        let generation = state.session_generation;
        tokio::task::spawn_blocking(move || {
            if let Some(rebuilt) = job.run() {
                let ev = UiAsyncEvent::LogGraphRebuilt(Box::new(rebuilt));
                let _ = tx.send(SessionInput::UiAsyncFrom(generation, ev));
            }
        });
    }

    fn spawn_syntax_highlight(&mut self, job: SyntaxJob, state: &mut AppState) {
        let tx = self.ui_async_tx.clone();
        let generation = state.session_generation;
        tokio::task::spawn_blocking(move || {
            if let Some(ev) = job.run() {
                let _ = tx.send(SessionInput::UiAsyncFrom(generation, ev));
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
            SessionInput::EngineFrom(generation, ev) => {
                if generation != state.session_generation {
                    return false;
                }
                let effs = state.apply_engine_event(ev);
                push_effects(effs, state)
            }
            SessionInput::UiAsync(ui_ev) => {
                let effs = state.apply_ui_async_event(ui_ev);
                push_effects(effs, state)
            }
            SessionInput::UiAsyncFrom(generation, ui_ev) => {
                if generation != state.session_generation {
                    return false;
                }
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

    /// Repository session generation of the bound repository.
    pub fn generation(&self) -> u64 {
        self.state.session_generation
    }

    /// Path of a confirmed repository switch the host must now perform
    /// (`RepoSwitcher`, worktree Open, …).
    pub fn take_repo_switch_request(&mut self) -> Option<PathBuf> {
        self.state.pending_repo_switch.take()
    }

    /// Directory where the user requested `git init` from the Welcome modal.
    pub fn take_init_repo_request(&mut self) -> Option<PathBuf> {
        self.state.pending_init_repo.take()
    }

    /// Validated `git clone` request submitted from the `Clone Repository…` modal.
    pub fn take_clone_repo_request(&mut self) -> Option<jbgit_engine::CloneRequest> {
        self.state.pending_clone_repo.take()
    }

    /// Workdir of the bound repository.
    pub fn current_workdir(&self) -> PathBuf {
        self.state.current_workdir()
    }

    /// Reports a switch the host could not perform (the current repository
    /// stays bound).
    pub fn report_repo_switch_failure(&mut self, target: &Path, error: &str) {
        self.state.report_repo_switch_failure(target, error);
    }

    /// The switch target resolved to the bound repository (e.g. a subdirectory).
    pub fn report_repo_already_open(&mut self, target: &Path) {
        let shown = target.display().to_string();
        self.state.status_notice = Some(
            self.state
                .i18n
                .t_fmt("toast.repo_already_open", &[("path", &shown)]),
        );
        self.state.needs_redraw = true;
    }

    /// First half of a repository rebind: cancels local single-flight jobs,
    /// flushes the commit draft and returns the graphics shutdown sequence.
    /// The host then stops the engine ([`RepoSession::unbind`]).
    pub fn prepare_unbind(&mut self) -> Vec<u8> {
        self.state.file_syntax.invalidate();
        self.state.stage_syntax.invalidate();
        self.state.blame_syntax.invalidate();
        self.state.range_syntax.invalidate();
        self.shutdown()
    }

    /// Second half of a repository rebind: replaces the per-repository state
    /// with `next` (built for the new repository; its `.jbgit/` config layer
    /// is already loaded, branches panel settings and commit draft load with
    /// its first snapshot), carrying over terminal-level state, and bumps the
    /// session generation so late events of the previous repository are
    /// dropped. `writes_finished = false` reports that the previous
    /// repository's write operations outlived the unbind timeout.
    pub fn rebind(&mut self, mut next: AppState, writes_finished: bool) {
        let prev = &mut self.state;
        next.session_generation = prev.session_generation.wrapping_add(1);
        next.terminal_area = prev.terminal_area;
        next.log
            .graph_renderer
            .set_capabilities(prev.log.graph_renderer.capabilities());
        next.async_graph_enabled = prev.async_graph_enabled;
        next.credentials = std::mem::take(&mut prev.credentials);
        next.recent_repos = std::mem::take(&mut prev.recent_repos);
        if next.git_version.is_none() {
            next.git_version = prev.git_version;
        }
        if next.git_info.is_none() && next.git_info_error.is_none() {
            next.git_info = prev.git_info.clone();
            next.git_info_error = prev.git_info_error.clone();
        }
        next.clock = Some(self.clock);
        next.needs_redraw = true;
        next.on_repository_bound(Some(writes_finished));
        self.state = next;
    }
}

/// Engine binding of the active repository (Spec 12 Issue 09): the engine
/// handle, its event stream and the session generation its events belong to.
/// Rebinding = [`TuiSession::prepare_unbind`] → [`RepoSession::unbind`] →
/// [`RepoSession::bind_location`] → [`TuiSession::rebind`].
pub struct RepoSession {
    pub handle: Option<EngineHandle>,
    pub events: Option<UnboundedReceiver<EngineEvent>>,
    pub generation: u64,
    askpass: Option<jbgit_engine::askpass::AskpassServer>,
}

/// How long an unbind waits for in-flight write operations.
pub const UNBIND_WRITE_TIMEOUT: Duration = Duration::from_secs(5);

impl RepoSession {
    /// No engine (repository discovery or engine start failed).
    pub fn detached() -> Self {
        Self::detached_with_askpass(None)
    }

    /// No engine, keeping an active [`AskpassServer`] alive so standalone
    /// operations like `git clone` from the Welcome modal can answer prompts.
    pub fn detached_with_askpass(askpass: Option<jbgit_engine::askpass::AskpassServer>) -> Self {
        Self {
            handle: None,
            events: None,
            generation: 0,
            askpass,
        }
    }

    pub fn backend(&self) -> Option<&dyn EngineBackend> {
        self.handle.as_ref().map(|h| h as &dyn EngineBackend)
    }

    pub fn askpass_env(&self) -> Option<jbgit_engine::askpass::AskpassEnv> {
        self.askpass
            .as_ref()
            .and_then(|s| s.env_for_current_exe().ok())
    }

    /// Discovers the repository containing `target`.
    pub fn discover(target: &Path) -> Result<RepoLocation, String> {
        RepoLocation::discover(target).map_err(|e| e.to_string())
    }

    /// Starts the engine for `loc` and builds the matching [`AppState`]
    /// (per-repository config layer, detected git executable & version).
    pub fn bind_location(
        loc: RepoLocation,
        askpass: Option<jbgit_engine::askpass::AskpassServer>,
    ) -> (Self, AppState) {
        let workdir = loc.workdir.clone();
        let config = crate::config::AppConfig::load(Some(&workdir));
        let scan_config = config.jbgit.performance.to_engine_config();
        let git_override = config.jbgit.git.executable.clone();
        jbgit_engine::set_configured_git_executable(git_override.clone());
        let askpass_env = askpass
            .as_ref()
            .and_then(|s| s.env_for_current_exe().ok());
        match EngineSupervisor::start_with_options(
            loc,
            true,
            scan_config,
            askpass_env,
            git_override,
        ) {
            Ok((handle, rx)) => {
                let configured = config.jbgit.git.configured_executable().map(str::to_string);
                let mut state = AppState::new_with_config(workdir, None, config);
                populate_git_info(&mut state, configured.as_deref(), handle.git_info());
                if let Some(err) = state.git_info_error.clone() {
                    state.status_notice = Some(err);
                }
                let repo = Self {
                    handle: Some(handle),
                    events: Some(rx),
                    generation: 0,
                    askpass,
                };
                (repo, state)
            }
            Err(e) => {
                drop(askpass);
                let cli_i18n = crate::i18n::I18n::load_for_cli();
                let err = cli_i18n.t_fmt("cli.engine_err", &[("err", &e.to_string())]);
                let configured = config.jbgit.git.configured_executable().map(str::to_string);
                let mut state = AppState::new_with_config(workdir, Some(err), config);
                populate_git_info(&mut state, configured.as_deref(), None);
                (Self::detached(), state)
            }
        }
    }

    /// Stops the engine and its askpass bridge: advances request slots, stops
    /// the watcher and the supervisor, waits up to [`UNBIND_WRITE_TIMEOUT`] for
    /// in-flight write operations, then closes and removes the askpass channel.
    /// Returns `false` when writes were still running.
    pub async fn unbind(self) -> bool {
        drop(self.events);
        let finished = match self.handle {
            Some(handle) => handle.shutdown(UNBIND_WRITE_TIMEOUT).await,
            None => true,
        };
        drop(self.askpass);
        finished
    }
}

fn populate_git_info(
    state: &mut AppState,
    configured: Option<&str>,
    handle_info: Option<&jbgit_engine::GitExecutableInfo>,
) {
    if let Some(info) = handle_info {
        state.git_version = jbgit_core::GitVersion::parse(&info.version_string);
        state.git_info = Some(info.clone());
        state.git_info_error = None;
        return;
    }
    match jbgit_engine::detect_git_executable(configured) {
        Ok(info) => {
            state.git_version = jbgit_core::GitVersion::parse(&info.version_string);
            state.git_info = Some(info);
            state.git_info_error = None;
        }
        Err(err) => {
            state.git_info = None;
            state.git_info_error = Some(err.to_string());
        }
    }
}

/// Discovers the repository at `target_path`, starts the background
/// [`EngineSupervisor`], and builds a production [`TuiSession`] whose recent
/// repositories list is the user-level `recent-repos.toml`.
///
/// When `target_path` is an existing directory outside any Git repository,
/// keeps `askpass` alive and opens [`crate::app::ModalLayer::Welcome`] so the
/// user can initialize a repository, clone one, or open a recent repository.
pub fn init_engine_and_session(
    target_path: PathBuf,
    askpass: Option<jbgit_engine::askpass::AskpassServer>,
) -> (RepoSession, TuiSession) {
    let (repo, mut state) = match RepoSession::discover(&target_path) {
        Ok(loc) => RepoSession::bind_location(loc, askpass),
        Err(e) => {
            if target_path.is_dir() {
                let resolved_dir = target_path
                    .canonicalize()
                    .or_else(|_| std::env::current_dir().map(|cwd| cwd.join(&target_path)))
                    .unwrap_or(target_path);
                let config = crate::config::AppConfig::load(Some(&resolved_dir));
                let configured = config.jbgit.git.configured_executable().map(str::to_string);
                jbgit_engine::set_configured_git_executable(config.jbgit.git.executable.clone());
                let mut state = AppState::new_with_config(resolved_dir.clone(), None, config);
                populate_git_info(&mut state, configured.as_deref(), None);
                state.push_modal(crate::app::ModalLayer::Welcome(
                    crate::repo_lifecycle::WelcomeState::new(resolved_dir),
                ));
                (RepoSession::detached_with_askpass(askpass), state)
            } else {
                drop(askpass);
                let mut state = AppState::new(target_path, Some(e));
                let configured = state
                    .config
                    .jbgit
                    .git
                    .configured_executable()
                    .map(str::to_string);
                jbgit_engine::set_configured_git_executable(
                    state.config.jbgit.git.executable.clone(),
                );
                populate_git_info(&mut state, configured.as_deref(), None);
                (RepoSession::detached(), state)
            }
        }
    };
    state.async_graph_enabled = true;
    state.recent_repos =
        crate::repo_switcher::RecentRepos::load(crate::repo_switcher::RecentRepos::default_file());
    if repo.handle.is_some() {
        state.on_repository_bound(None);
    }
    (repo, TuiSession::new(state))
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
