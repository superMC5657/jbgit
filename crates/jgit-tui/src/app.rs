use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use jgit_core::{
    CommitSummary, GitConsoleEntry, ModalButton, Oid, PaneId, RefreshScope, RepoSnapshot,
    SplitAxis, SplitterId, StageStatusSnapshot, WorkspaceTab,
};
use jgit_engine::{EngineEvent, EngineHandle, TaskRequest};
use ratatui::layout::Rect;
use rustc_hash::FxHashMap;

use crate::hit_test::{HitTarget, HitTestRegistry};
use crate::theme::{BranchColorManager, GlyphMode, ThemePalette};
use crate::widgets::SplitterState;

pub struct RenderContext<'a> {
    pub theme: &'a ThemePalette,
    pub glyph_mode: GlyphMode,
    pub hit_registry: &'a mut HitTestRegistry,
    pub focused_pane: PaneId,
    pub hover_target: Option<&'a HitTarget>,
    pub dragging_splitter: Option<SplitterId>,
}

#[derive(Debug, Clone)]
pub enum ContextMenuAction {
    SwitchTab(WorkspaceTab),
    RefreshRepo,
    RunReadOnlyProbe(Vec<String>),
    ToggleGlyphMode,
    ToggleCompactRefs,
    ToggleTagNames,
    OpenHelp,
    OpenPalettePreview,
}

#[derive(Debug, Clone)]
pub enum ContextMenuEntryKind {
    Action(ContextMenuAction),
    Submenu(Vec<ContextMenuItem>),
}

#[derive(Debug, Clone)]
pub struct ContextMenuItem {
    pub label: String,
    pub mnemonic: Option<char>,
    pub shortcut: Option<String>,
    pub kind: ContextMenuEntryKind,
}

impl ContextMenuItem {
    pub fn action(
        label: impl Into<String>,
        mnemonic: Option<char>,
        shortcut: Option<&str>,
        action: ContextMenuAction,
    ) -> Self {
        Self {
            label: label.into(),
            mnemonic,
            shortcut: shortcut.map(ToString::to_string),
            kind: ContextMenuEntryKind::Action(action),
        }
    }

    pub fn submenu(
        label: impl Into<String>,
        mnemonic: Option<char>,
        items: Vec<ContextMenuItem>,
    ) -> Self {
        Self {
            label: label.into(),
            mnemonic,
            shortcut: None,
            kind: ContextMenuEntryKind::Submenu(items),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PopupKind {
    ShortcutsHelp,
    BranchesQuickView,
    /// Debug view of the Darcula palette, 14 track colors and glyph widths.
    PalettePreview,
}

#[derive(Debug, Clone)]
pub enum ModalLayer {
    /// Layer 2: Floating Popup (Branches Speed-Search, Shortcuts Help).
    FloatingPopup {
        kind: PopupKind,
        query: String,
        selected: usize,
    },
    /// Layer 3: Right-click Context Menu (supports cascading submenus via `depth`).
    ContextMenu {
        anchor: (u16, u16),
        items: Vec<ContextMenuItem>,
        selected: usize,
        depth: u8,
    },
    /// Layer 4: Critical Modal Dialog.
    CriticalDialog {
        title: String,
        body: String,
        confirm_label: String,
        cancel_label: String,
        selected_button: ModalButton,
        confirm_effect: Option<Effect>,
    },
}

impl ModalLayer {
    pub const fn z_level(&self) -> u8 {
        match self {
            Self::FloatingPopup { .. } => 20,
            Self::ContextMenu { depth, .. } => 30u8.saturating_add(depth.saturating_mul(2)),
            Self::CriticalDialog { .. } => 40,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct DragState {
    pub splitter_id: SplitterId,
    pub axis: SplitAxis,
    pub container: Rect,
}

#[derive(Debug, Clone, Copy)]
pub struct ClickRecord {
    pub at: Instant,
    pub col: u16,
    pub row: u16,
}

impl ClickRecord {
    pub fn is_double_click(&self, col: u16, row: u16) -> bool {
        self.at.elapsed() <= Duration::from_millis(300)
            && self.col.abs_diff(col) <= 1
            && self.row.abs_diff(row) <= 1
    }
}

#[derive(Debug, Clone)]
pub enum Effect {
    RefreshRepoAndStage,
    RefreshScope(RefreshScope),
    RequestCommitDetails(Vec<Oid>),
    RunCli {
        args: Vec<String>,
        stdin_payload: Option<Vec<u8>>,
        is_read_only: bool,
        refresh_after: RefreshScope,
    },
    Quit,
}

#[derive(Debug, Clone)]
pub enum Action {
    Quit,
    SwitchTab(WorkspaceTab),
    CycleFocus {
        reverse: bool,
    },
    FocusPane(PaneId),
    ToggleGlyphMode,
    ToggleCompactRefs,
    ToggleTagNames,
    RefreshAll,
    OpenPopup {
        kind: PopupKind,
        initial_query: String,
    },
    OpenContextMenu {
        anchor: (u16, u16),
        items: Vec<ContextMenuItem>,
        depth: u8,
    },
    PopModal,
    PopContextMenus,
    ConfirmCriticalDialog,
    ResetSplitter(SplitterId),
    SelectBranchRow(usize),
    ScrollBranches(isize),
    SelectLogRow(usize),
    ScrollLog(isize),
    SelectConsoleRow(usize),
    ScrollConsoleList(isize),
    ScrollConsoleOutput(isize),
    SelectCommitStageRow(usize),
    ScrollCommitStage(isize),
    RunCliProbe(Vec<String>),
    Resize,
    TickSpinner,
}

pub struct AppState {
    pub theme: ThemePalette,
    pub glyph_mode: GlyphMode,
    pub branch_colors: BranchColorManager,
    /// Compact References View: only the first ref keeps its text.
    pub compact_refs: bool,
    /// `Show Tag Names`: when off, tags collapse to icons.
    pub show_tag_names: bool,
    pub active_tab: WorkspaceTab,
    pub focused_pane: PaneId,
    pub splitters: SplitterState,
    pub splitter_containers: FxHashMap<SplitterId, (SplitAxis, Rect)>,
    pub terminal_area: Rect,
    pub modal_stack: Vec<ModalLayer>,
    pub hit_registry: HitTestRegistry,
    pub drag_state: Option<DragState>,
    pub last_click: Option<ClickRecord>,
    pub hover_pos: Option<(u16, u16)>,
    pub hover_target: Option<HitTarget>,

    pub repo_path_display: PathBuf,
    pub repo_error: Option<String>,
    pub repo_snapshot: Option<Arc<RepoSnapshot>>,
    pub stage_status: Option<Arc<StageStatusSnapshot>>,
    pub commit_details: FxHashMap<Oid, CommitSummary>,

    pub latest_repo_epoch: u64,
    pub latest_stage_epoch: u64,
    pub latest_commit_details_epoch: u64,

    pub branches_selected_idx: usize,
    pub log_selected_idx: usize,
    pub commit_stage_selected_idx: usize,

    pub console_entries: Vec<GitConsoleEntry>,
    pub console_selected_idx: usize,
    pub console_output_scroll: usize,

    pub next_cli_id: u64,
    pub loading_snapshot: bool,
    pub status_notice: Option<String>,
    pub spinner_frame: usize,
    pub needs_redraw: bool,
    pub should_quit: bool,
}

impl AppState {
    pub fn new(repo_path_display: PathBuf, repo_error: Option<String>) -> Self {
        let theme = ThemePalette::detect();
        let branch_colors = theme.branch_color_manager();
        Self {
            theme,
            glyph_mode: GlyphMode::Unicode,
            branch_colors,
            compact_refs: false,
            show_tag_names: true,
            active_tab: WorkspaceTab::Log,
            focused_pane: WorkspaceTab::Log.default_pane(),
            splitters: SplitterState::default(),
            splitter_containers: FxHashMap::default(),
            terminal_area: Rect::ZERO,
            modal_stack: Vec::new(),
            hit_registry: HitTestRegistry::new(),
            drag_state: None,
            last_click: None,
            hover_pos: None,
            hover_target: None,
            repo_path_display,
            repo_error,
            repo_snapshot: None,
            stage_status: None,
            commit_details: FxHashMap::default(),
            latest_repo_epoch: 0,
            latest_stage_epoch: 0,
            latest_commit_details_epoch: 0,
            branches_selected_idx: 0,
            log_selected_idx: 0,
            commit_stage_selected_idx: 0,
            console_entries: Vec::new(),
            console_selected_idx: 0,
            console_output_scroll: 0,
            next_cli_id: 1,
            loading_snapshot: false,
            status_notice: None,
            spinner_frame: 0,
            needs_redraw: true,
            should_quit: false,
        }
    }

    pub fn has_active_tasks(&self) -> bool {
        self.loading_snapshot || self.console_entries.iter().any(GitConsoleEntry::is_running)
    }

    pub fn spinner_char(&self) -> &'static str {
        const FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
        FRAMES[self.spinner_frame % FRAMES.len()]
    }

    pub fn branch_node_count(&self) -> usize {
        self.repo_snapshot.as_ref().map_or(0, |s| s.refs.len())
    }

    pub fn stage_item_count(&self) -> usize {
        self.stage_status.as_ref().map_or(0, |s| s.total_changes())
    }

    pub fn stage_entry_at(&self, idx: usize) -> Option<&jgit_core::StageEntry> {
        let status = self.stage_status.as_ref()?;
        status
            .conflicted
            .iter()
            .chain(status.staged.iter())
            .chain(status.unstaged.iter())
            .chain(status.untracked.iter())
            .nth(idx)
    }

    pub fn dispatch(&mut self, action: Action) -> Vec<Effect> {
        let mut effects = Vec::new();
        self.needs_redraw = true;

        match action {
            Action::Quit => {
                self.should_quit = true;
                effects.push(Effect::Quit);
            }
            Action::SwitchTab(tab) => {
                self.active_tab = tab;
                self.focused_pane = tab.default_pane();
            }
            Action::CycleFocus { reverse } => {
                let ring = self.active_tab.focus_ring();
                if let Some(pos) = ring.iter().position(|&p| p == self.focused_pane) {
                    let next = if reverse {
                        if pos == 0 { ring.len() - 1 } else { pos - 1 }
                    } else {
                        (pos + 1) % ring.len()
                    };
                    self.focused_pane = ring[next];
                } else if let Some(&first) = ring.first() {
                    self.focused_pane = first;
                }
            }
            Action::FocusPane(pane) => {
                self.focused_pane = pane;
            }
            Action::ToggleGlyphMode => {
                self.glyph_mode = self.glyph_mode.toggle();
                self.status_notice = Some(format!("GlyphMode: {}", self.glyph_mode.label()));
            }
            Action::ToggleCompactRefs => {
                self.compact_refs = !self.compact_refs;
                self.status_notice = Some(format!(
                    "Compact References View: {}",
                    if self.compact_refs { "on" } else { "off" }
                ));
            }
            Action::ToggleTagNames => {
                self.show_tag_names = !self.show_tag_names;
                self.status_notice = Some(format!(
                    "Show Tag Names: {}",
                    if self.show_tag_names { "on" } else { "off" }
                ));
            }
            Action::RefreshAll => {
                self.loading_snapshot = true;
                effects.push(Effect::RefreshRepoAndStage);
            }
            Action::OpenPopup {
                kind,
                initial_query,
            } => {
                self.modal_stack.push(ModalLayer::FloatingPopup {
                    kind,
                    query: initial_query,
                    selected: 0,
                });
            }
            Action::OpenContextMenu {
                anchor,
                items,
                depth,
            } => {
                if !items.is_empty() {
                    self.modal_stack.push(ModalLayer::ContextMenu {
                        anchor,
                        items,
                        selected: 0,
                        depth,
                    });
                }
            }
            Action::PopModal => {
                self.modal_stack.pop();
            }
            Action::PopContextMenus => {
                while matches!(
                    self.modal_stack.last(),
                    Some(ModalLayer::ContextMenu { .. })
                ) {
                    self.modal_stack.pop();
                }
            }
            Action::ConfirmCriticalDialog => {
                if let Some(ModalLayer::CriticalDialog { confirm_effect, .. }) =
                    self.modal_stack.pop()
                {
                    if let Some(eff) = confirm_effect {
                        effects.push(eff);
                    } else {
                        self.loading_snapshot = true;
                        effects.push(Effect::RefreshRepoAndStage);
                    }
                }
            }
            Action::ResetSplitter(id) => {
                self.splitters.reset(id);
            }
            Action::SelectBranchRow(idx) => {
                let len = self.branch_node_count();
                if len > 0 {
                    self.branches_selected_idx = idx.min(len - 1);
                }
            }
            Action::ScrollBranches(delta) => {
                let len = self.branch_node_count();
                if len > 0 {
                    self.branches_selected_idx = (self.branches_selected_idx as isize + delta)
                        .clamp(0, (len - 1) as isize)
                        as usize;
                }
            }
            Action::SelectLogRow(idx) => {
                if let Some(snap) = &self.repo_snapshot
                    && !snap.commits.is_empty()
                {
                    self.log_selected_idx = idx.min(snap.commits.len() - 1);
                    if let Some(commit) = snap.commits.get(self.log_selected_idx) {
                        effects.push(Effect::RequestCommitDetails(vec![commit.oid]));
                    }
                }
            }
            Action::ScrollLog(delta) => {
                let len = self.repo_snapshot.as_ref().map_or(0, |s| s.commits.len());
                if len > 0 {
                    let cur = self.log_selected_idx as isize;
                    let next = (cur + delta).clamp(0, (len - 1) as isize) as usize;
                    if next != self.log_selected_idx {
                        self.log_selected_idx = next;
                        if let Some(commit) = self
                            .repo_snapshot
                            .as_ref()
                            .and_then(|s| s.commits.get(next))
                        {
                            effects.push(Effect::RequestCommitDetails(vec![commit.oid]));
                        }
                    }
                }
            }
            Action::SelectConsoleRow(idx) => {
                if !self.console_entries.is_empty() {
                    self.console_selected_idx = idx.min(self.console_entries.len() - 1);
                    self.console_output_scroll = 0;
                }
            }
            Action::ScrollConsoleList(delta) => {
                let len = self.console_entries.len();
                if len > 0 {
                    let next = (self.console_selected_idx as isize + delta)
                        .clamp(0, (len - 1) as isize) as usize;
                    if next != self.console_selected_idx {
                        self.console_selected_idx = next;
                        self.console_output_scroll = 0;
                    }
                }
            }
            Action::ScrollConsoleOutput(delta) => {
                let next = (self.console_output_scroll as isize + delta).max(0) as usize;
                self.console_output_scroll = next;
            }
            Action::SelectCommitStageRow(idx) => {
                let total = self.stage_item_count();
                if total > 0 {
                    self.commit_stage_selected_idx = idx.min(total - 1);
                }
            }
            Action::ScrollCommitStage(delta) => {
                let total = self.stage_item_count();
                if total > 0 {
                    self.commit_stage_selected_idx =
                        (self.commit_stage_selected_idx as isize + delta)
                            .clamp(0, (total - 1) as isize) as usize;
                }
            }
            Action::RunCliProbe(args) => {
                effects.push(Effect::RunCli {
                    args,
                    stdin_payload: None,
                    is_read_only: true,
                    refresh_after: RefreshScope::None,
                });
            }
            Action::Resize | Action::TickSpinner => {
                if matches!(action, Action::TickSpinner) {
                    self.spinner_frame = self.spinner_frame.wrapping_add(1);
                }
            }
        }

        effects
    }

    pub fn apply_engine_event(&mut self, event: EngineEvent) -> Vec<Effect> {
        let mut effects = Vec::new();
        self.needs_redraw = true;

        match event {
            EngineEvent::RepoSnapshotLoaded { snapshot, epoch } => {
                if epoch < self.latest_repo_epoch {
                    return effects;
                }
                self.latest_repo_epoch = epoch;
                self.loading_snapshot = false;

                let commits_len = snapshot.commits.len();
                if self.log_selected_idx >= commits_len {
                    self.log_selected_idx = commits_len.saturating_sub(1);
                }
                let refs_len = snapshot.refs.len();
                if self.branches_selected_idx >= refs_len {
                    self.branches_selected_idx = refs_len.saturating_sub(1);
                }
                self.repo_snapshot = Some(snapshot);
            }
            EngineEvent::CommitDetailsLoaded { commits, epoch } => {
                if epoch < self.latest_commit_details_epoch {
                    return effects;
                }
                self.latest_commit_details_epoch = epoch;
                for c in commits {
                    self.commit_details.insert(c.oid, c);
                }
            }
            EngineEvent::StageStatusLoaded { status, epoch } => {
                if epoch < self.latest_stage_epoch {
                    return effects;
                }
                self.latest_stage_epoch = epoch;

                let total = status.total_changes();
                if self.commit_stage_selected_idx >= total {
                    self.commit_stage_selected_idx = total.saturating_sub(1);
                }
                self.stage_status = Some(status);
            }
            EngineEvent::ConsoleStarted(entry) => {
                self.console_entries.insert(0, entry);
                self.console_selected_idx = 0;
                self.console_output_scroll = 0;
            }
            EngineEvent::ConsoleOutput {
                id,
                is_stderr,
                line,
            } => {
                if let Some(entry) = self.console_entries.iter_mut().find(|e| e.id == id) {
                    if is_stderr {
                        entry.stderr_lines.push(line);
                    } else {
                        entry.stdout_lines.push(line);
                    }
                }
            }
            EngineEvent::ConsoleFinished {
                id,
                exit_code,
                duration_ms,
            } => {
                if let Some(entry) = self.console_entries.iter_mut().find(|e| e.id == id) {
                    entry.exit_code = Some(exit_code);
                    entry.duration_ms = Some(duration_ms);
                }
            }
            EngineEvent::IndexLockDetected {
                lock_path,
                message,
                retry_args,
                retry_stdin,
                retry_is_read_only,
                retry_refresh_after,
            } => {
                self.modal_stack.push(ModalLayer::CriticalDialog {
                    title: "Git Index Lock Conflict".to_string(),
                    body: format!(
                        "Another Git process appears to hold:\n{}\n\n{}",
                        lock_path.display(),
                        message
                    ),
                    confirm_label: "Retry".to_string(),
                    cancel_label: "Dismiss".to_string(),
                    selected_button: ModalButton::Confirm,
                    confirm_effect: Some(Effect::RunCli {
                        args: retry_args,
                        stdin_payload: retry_stdin,
                        is_read_only: retry_is_read_only,
                        refresh_after: retry_refresh_after,
                    }),
                });
            }
            EngineEvent::ExternalFsChanged(scope) => {
                effects.push(Effect::RefreshScope(scope));
            }
            EngineEvent::Error(msg) => {
                self.loading_snapshot = false;
                self.status_notice = Some(format!("Engine: {msg}"));
            }
        }

        effects
    }
}

pub fn execute_effects(effects: Vec<Effect>, state: &mut AppState, engine: Option<&EngineHandle>) {
    let Some(handle) = engine else {
        return;
    };

    for effect in effects {
        match effect {
            Effect::RefreshRepoAndStage => {
                state.latest_repo_epoch =
                    state.latest_repo_epoch.max(handle.request_repo_snapshot());
                state.latest_stage_epoch = state
                    .latest_stage_epoch
                    .max(handle.request_stage_status(false));
            }
            Effect::RefreshScope(scope) => {
                if scope.needs_graph_refresh() {
                    state.latest_repo_epoch =
                        state.latest_repo_epoch.max(handle.request_repo_snapshot());
                }
                if scope.needs_stage_refresh() {
                    state.latest_stage_epoch = state
                        .latest_stage_epoch
                        .max(handle.request_stage_status(false));
                }
            }
            Effect::RequestCommitDetails(oids) => {
                if !oids.is_empty() {
                    state.latest_commit_details_epoch = state
                        .latest_commit_details_epoch
                        .max(handle.request_commit_details(oids));
                }
            }
            Effect::RunCli {
                args,
                stdin_payload,
                is_read_only,
                refresh_after,
            } => {
                let id = state.next_cli_id;
                state.next_cli_id += 1;
                handle.send(TaskRequest::RunGitCli {
                    id,
                    args,
                    stdin_payload,
                    is_read_only,
                    refresh_after,
                });
            }
            Effect::Quit => {}
        }
    }
}
