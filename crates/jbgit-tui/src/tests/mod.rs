mod branches_and_dialogs_tests;
mod context_menu_tests;
mod annotate_and_selection_history_tests;
mod file_history_and_changes_tests;
mod footer_and_keymap_tests;
mod graph_collapse_tests;
mod log_selection_and_compare_tests;
mod log_tests;
mod merge_and_update_tests;
mod patch_and_tree_browser_tests;
mod remotes_and_credentials_tests;
mod repo_capabilities_tests;
mod repo_session_and_worktrees_tests;
mod runtime_and_editor_tests;
mod session_tests;
mod tag_and_push_tests;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use jbgit_core::{
    CommitDetails, CommitSignature, CommitSummary, GitOp, Oid, PersonStamp, RefInfo, RefKind,
    RepoSnapshot, RepositoryState,
};
use jbgit_engine::{EngineEvent, RecordingEngineBackend, TaskRequest};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use crate::app::{
    Action, AppState, ContextMenuEntryKind, ContextMenuItem, ModalLayer, UiAsyncEvent,
};
use crate::config::AppConfig;
use crate::hit_test::HitTarget;
use crate::i18n::I18n;
use crate::session::{SessionInput, TestHost, TuiSession, TurnReport};

pub(super) fn oid(n: u8) -> Oid {
    Oid::from_bytes([n; 20])
}

/// Configured remotes `names` with `https://example.com/<name>.git` URLs.
pub(super) fn remote_infos(names: &[&str]) -> Vec<jbgit_core::RemoteInfo> {
    names
        .iter()
        .map(|n| jbgit_core::RemoteInfo {
            name: (*n).to_string(),
            fetch_url: Some(format!("https://example.com/{n}.git")),
            push_url: None,
        })
        .collect()
}

pub(super) fn oid_n(n: u8) -> Oid {
    let mut bytes = [0u8; 20];
    bytes[0] = n;
    bytes[19] = n;
    Oid::from_bytes(bytes)
}

pub(super) fn local(name: &str, target: u8) -> RefInfo {
    RefInfo {
        name: name.into(),
        full_name: format!("refs/heads/{name}"),
        target_oid: oid(target),
        kind: RefKind::LocalBranch,
        upstream: None,
        ahead_behind: None,
        is_head: false,
    }
}

pub(super) fn remote(name: &str, target: u8) -> RefInfo {
    RefInfo {
        name: name.into(),
        full_name: format!("refs/remotes/{name}"),
        target_oid: oid(target),
        kind: RefKind::RemoteBranch,
        upstream: None,
        ahead_behind: None,
        is_head: false,
    }
}

/// Linear history c1 ← … ← c12 (HEAD, `main`); `origin/main` = c2 so c3..c12
/// are rewritable; `feature/dag-layout` = c5 tracking `origin/feature/dag-layout`.
pub(super) fn snapshot() -> RepoSnapshot {
    let mut main = local("main", 12);
    main.is_head = true;
    main.upstream = Some("origin/main".into());
    main.ahead_behind = Some((10, 0));
    let mut dag = local("feature/dag-layout", 5);
    dag.upstream = Some("origin/feature/dag-layout".into());
    dag.ahead_behind = Some((1, 0));
    let commits = (1..=12u8)
        .rev()
        .map(|n| CommitSummary {
            oid: oid(n),
            parents: if n > 1 { vec![oid(n - 1)] } else { vec![] },
            author_name: "Alice".into(),
            author_email: "alice@x.io".into(),
            timestamp: i64::from(n) * 100,
            author_timestamp: i64::from(n) * 100,
            subject: format!("c{n}"),
            body: String::new(),
        })
        .collect();
    RepoSnapshot {
        repo_name: "jbgit".into(),
        head_branch: Some("main".into()),
        head_oid: Some(oid(12)),
        refs: vec![
            main,
            dag,
            local("feature/stage-tracker", 7),
            local("bugfix/unmerged", 3),
            remote("origin/main", 2),
            remote("origin/feature/dag-layout", 4),
            remote("origin/feature-new", 9),
        ],
        commits,
        recent_branches: vec!["feature/stage-tracker".into()],
        ..Default::default()
    }
}

pub(super) fn make_commit(n: u8, parents: Vec<Oid>, subject: &str) -> CommitSummary {
    CommitSummary {
        oid: oid_n(n),
        parents,
        author_name: "Alice".to_string(),
        author_email: "alice@example.com".to_string(),
        timestamp: 1_700_000_000 + i64::from(n) * 60,
        author_timestamp: 1_700_000_000 + i64::from(n) * 60,
        subject: subject.to_string(),
        body: String::new(),
    }
}

pub(super) fn make_details(oid: Oid, parents: Vec<Oid>, subject: &str) -> CommitDetails {
    CommitDetails {
        oid,
        parents,
        author: PersonStamp {
            name: "Alice".into(),
            email: "alice@example.com".into(),
            time: 1_700_000_000,
            offset_minutes: 0,
        },
        committer: PersonStamp {
            name: "Alice".into(),
            email: "alice@example.com".into(),
            time: 1_700_000_000,
            offset_minutes: 0,
        },
        message: subject.into(),
        signature: CommitSignature::default(),
    }
}

pub(super) fn make_snapshot(workdir: PathBuf, commits: Vec<CommitSummary>) -> Arc<RepoSnapshot> {
    let head_oid = commits.first().map(|c| c.oid);
    let mut refs = Vec::new();
    if let Some(h) = head_oid {
        refs.push(RefInfo {
            name: "main".into(),
            full_name: "refs/heads/main".into(),
            target_oid: h,
            kind: RefKind::LocalBranch,
            upstream: None,
            ahead_behind: None,
            is_head: true,
        });
    }
    Arc::new(RepoSnapshot {
        repo_name: "test-repo".into(),
        workdir,
        git_dir: PathBuf::new(),
        common_dir: PathBuf::new(),
        state: RepositoryState::Clean,
        is_unborn_head: commits.is_empty(),
        head_branch: head_oid.map(|_| "main".into()),
        head_oid,
        refs,
        commits,
        user_name: Some("Alice".into()),
        user_email: Some("alice@example.com".into()),
        recent_branches: vec!["main".into()],
        stashes: Vec::new(),
        ongoing_onto: None,
        remotes: Vec::new(),
        worktrees: Vec::new(),
        submodules: Vec::new(),
        has_lfs: false,
        is_shallow: false,
    })
}

pub(super) fn test_state(workdir: PathBuf) -> AppState {
    let config = AppConfig::default();
    let mut state = AppState::new_with_config(workdir, None, config);
    state.i18n = Arc::new(I18n::load("en-US", None));
    state
}

/// Full end-to-end deterministic test harness pairing [`TuiSession`] with
/// [`RecordingEngineBackend`], [`TestHost`], a virtual monotonic clock, and
/// ratatui's [`TestBackend`].
pub(super) struct TestSession {
    pub(super) session: TuiSession,
    pub(super) engine: RecordingEngineBackend,
    pub(super) host: TestHost,
    clock: Instant,
}

impl TestSession {
    pub(super) fn new(state: AppState) -> Self {
        let clock = Instant::now();
        Self {
            session: TuiSession::new_at(state, clock),
            engine: RecordingEngineBackend::new(),
            host: TestHost::new(),
            clock,
        }
    }

    pub(super) fn with_snapshot(snap: RepoSnapshot) -> Self {
        let mut s = Self::new(test_state(snap.workdir.clone()));
        s.deliver_engine_event(EngineEvent::RepoSnapshotLoaded {
            snapshot: Arc::new(snap),
            epoch: 1,
        });
        s
    }

    pub(super) fn with_config(workdir: PathBuf, config: AppConfig) -> Self {
        let mut state = AppState::new_with_config(workdir, None, config);
        state.i18n = Arc::new(I18n::load("en-US", None));
        Self::new(state)
    }

    pub(super) fn state(&self) -> &AppState {
        self.session.state()
    }

    pub(super) fn state_mut(&mut self) -> &mut AppState {
        self.session.state_mut()
    }

    pub(super) fn now(&self) -> Instant {
        self.clock
    }

    pub(super) fn step(&mut self, inputs: impl IntoIterator<Item = SessionInput>) -> TurnReport {
        self.session
            .step_turn(inputs, self.clock, Some(&self.engine), &mut self.host)
    }

    pub(super) fn send_event(&mut self, ev: Event) -> TurnReport {
        self.step([SessionInput::Terminal(ev)])
    }

    pub(super) fn send_events(&mut self, events: impl IntoIterator<Item = Event>) -> TurnReport {
        self.step(events.into_iter().map(SessionInput::Terminal))
    }

    pub(super) fn send_key(&mut self, code: KeyCode, mods: KeyModifiers) -> TurnReport {
        self.send_event(Event::Key(KeyEvent::new(code, mods)))
    }

    pub(super) fn type_str(&mut self, text: &str) {
        let events: Vec<Event> = text
            .chars()
            .map(|c| Event::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)))
            .collect();
        self.send_events(events);
    }

    pub(super) fn send_mouse_event(&mut self, ev: MouseEvent) -> TurnReport {
        self.send_event(Event::Mouse(ev))
    }

    pub(super) fn send_mouse(
        &mut self,
        kind: MouseEventKind,
        col: u16,
        row: u16,
    ) -> TurnReport {
        self.send_event(Event::Mouse(MouseEvent {
            kind,
            column: col,
            row,
            modifiers: KeyModifiers::NONE,
        }))
    }

    pub(super) fn dispatch(&mut self, action: Action) -> TurnReport {
        self.step([SessionInput::Action(action)])
    }

    pub(super) fn dispatch_many(&mut self, actions: impl IntoIterator<Item = Action>) -> TurnReport {
        self.step(actions.into_iter().map(SessionInput::Action))
    }

    pub(super) fn deliver_engine_event(&mut self, ev: EngineEvent) -> TurnReport {
        self.step([SessionInput::Engine(ev)])
    }

    #[allow(dead_code)]
    pub(super) fn deliver_ui_async(&mut self, ev: UiAsyncEvent) -> TurnReport {
        self.step([SessionInput::UiAsync(ev)])
    }

    pub(super) fn advance_by(&mut self, delta: Duration) -> TurnReport {
        self.clock += delta;
        self.session
            .advance_to(self.clock, Some(&self.engine), &mut self.host)
    }

    pub(super) fn render(&mut self, w: u16, h: u16) -> Buffer {
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal
            .draw(|f| self.session.render(f))
            .unwrap();
        terminal.backend().buffer().clone()
    }

    pub(super) fn take_requests(&self) -> Vec<(TaskRequest, Option<u64>)> {
        self.engine.take_recorded()
    }

    pub(super) fn take_git_ops(&self) -> Vec<GitOp> {
        self.engine
            .take_recorded()
            .into_iter()
            .filter_map(|(req, _)| match req {
                TaskRequest::RunGitOp { op } => Some(op),
                _ => None,
            })
            .collect()
    }

    pub(super) fn region_of(&self, pred: impl Fn(&HitTarget) -> bool) -> Rect {
        self.state()
            .hit_registry
            .find(pred)
            .map(|r| r.rect)
            .expect("hit region not registered")
    }

    pub(super) fn hit_center_of(&self, pred: impl Fn(&HitTarget) -> bool) -> Option<(u16, u16)> {
        self.state()
            .hit_registry
            .find(pred)
            .map(|r| (r.rect.x + r.rect.width / 2, r.rect.y + r.rect.height / 2))
    }

    pub(super) fn should_quit(&self) -> bool {
        self.session.should_quit()
    }

    pub(super) fn next_deadline(&self) -> Option<Instant> {
        self.session.next_deadline()
    }

    pub(super) fn clipboard(&self) -> &[String] {
        &self.host.clipboard
    }

    pub(super) fn execute_effects(&mut self, effects: Vec<crate::app::Effect>) {
        crate::app::execute_effects_with_host(
            effects,
            self.session.state_mut(),
            Some(&self.engine),
            &mut self.host,
        );
    }
}

pub(super) fn left_click(col: u16, row: u16, mods: KeyModifiers) -> MouseEvent {
    MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: col,
        row,
        modifiers: mods,
    }
}

pub(super) fn right_click(col: u16, row: u16) -> MouseEvent {
    MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Right),
        column: col,
        row,
        modifiers: KeyModifiers::NONE,
    }
}

pub(super) fn row_text(buf: &Buffer, y: u16) -> String {
    (0..buf.area.width)
        .map(|x| buf[(x, y)].symbol().to_string())
        .collect()
}

pub(super) fn screen_text(buf: &Buffer) -> String {
    (0..buf.area.height)
        .map(|y| row_text(buf, y))
        .collect::<Vec<_>>()
        .join("\n")
}

pub(super) fn top_menu(state: &AppState) -> (&[ContextMenuItem], usize, u8) {
    match state.modal_stack.last() {
        Some(ModalLayer::ContextMenu {
            items,
            selected,
            depth,
            ..
        }) => (items, *selected, *depth),
        other => panic!("expected context menu, got {other:?}"),
    }
}

pub(super) fn find_item<'a>(items: &'a [ContextMenuItem], label: &str) -> &'a ContextMenuItem {
    items
        .iter()
        .find(|i| i.label.starts_with(label))
        .unwrap_or_else(|| panic!("menu item {label:?} missing"))
}

pub(super) fn menu_item_idx(items: &[ContextMenuItem], label: &str) -> usize {
    items
        .iter()
        .position(|i| i.label.starts_with(label))
        .unwrap_or_else(|| panic!("menu item {label:?} missing"))
}

pub(super) fn menu_item_by_label<'a>(
    items: &'a [ContextMenuItem],
    substr: &str,
) -> &'a ContextMenuItem {
    items
        .iter()
        .find(|i| !i.is_separator() && i.label.contains(substr))
        .unwrap_or_else(|| {
            let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
            panic!("menu item containing '{substr}' not found in {labels:?}");
        })
}

pub(super) fn item_run_actions(item: &ContextMenuItem) -> Vec<Action> {
    match &item.kind {
        ContextMenuEntryKind::Action(actions) => actions.clone(),
        other => panic!("expected Run item, got {other:?}"),
    }
}
