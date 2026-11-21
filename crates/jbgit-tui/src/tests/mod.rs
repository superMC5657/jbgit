mod branches_and_dialogs_tests;
mod context_menu_tests;
mod file_history_and_changes_tests;
mod footer_and_keymap_tests;
mod runtime_and_editor_tests;

use std::path::PathBuf;
use std::sync::Arc;

use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use jbgit_core::{
    CommitDetails, CommitSummary, GitOp, Oid, PersonStamp, RefInfo, RefKind, RepoSnapshot,
    RepositoryState, SignatureStatus,
};
use jbgit_engine::EngineEvent;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use crate::app::{
    Action, AppState, ContextMenuAction, ContextMenuEntryKind, ContextMenuItem, Effect, ModalLayer,
};
use crate::config::AppConfig;
use crate::hit_test::HitTarget;
use crate::i18n::I18n;
use crate::input::route_terminal_event;
use crate::views::render_app;

pub(super) fn oid(n: u8) -> Oid {
    Oid::from_bytes([n; 20])
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
        signature: SignatureStatus::default(),
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
    })
}

pub(super) fn test_state(workdir: PathBuf) -> AppState {
    let config = AppConfig::default();
    let mut state = AppState::new_with_config(workdir, None, config);
    state.i18n = Arc::new(I18n::load("en-US", None));
    state
}

pub(super) fn state_with(snap: RepoSnapshot) -> AppState {
    let mut state = AppState::new_with_config(".".into(), None, AppConfig::default());
    state.i18n = Arc::new(I18n::load("en-US", None));
    state.apply_engine_event(EngineEvent::RepoSnapshotLoaded {
        snapshot: Arc::new(snap),
        epoch: 1,
    });
    state
}

pub(super) fn run(state: &mut AppState, actions: Vec<Action>) -> Vec<Effect> {
    let mut effects = Vec::new();
    for a in actions {
        effects.extend(state.dispatch(a));
    }
    effects
}

pub(super) fn key(state: &mut AppState, code: KeyCode, mods: KeyModifiers) -> Vec<Effect> {
    let actions = route_terminal_event(Event::Key(KeyEvent::new(code, mods)), state);
    run(state, actions)
}

pub(super) fn type_str(state: &mut AppState, text: &str) {
    for c in text.chars() {
        key(state, KeyCode::Char(c), KeyModifiers::NONE);
    }
}

pub(super) fn mouse(
    state: &mut AppState,
    kind: MouseEventKind,
    col: u16,
    row: u16,
) -> Vec<Effect> {
    let actions = route_terminal_event(
        Event::Mouse(MouseEvent {
            kind,
            column: col,
            row,
            modifiers: KeyModifiers::NONE,
        }),
        state,
    );
    run(state, actions)
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

pub(super) fn render(state: &mut AppState, w: u16, h: u16) -> Terminal<TestBackend> {
    let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
    terminal.draw(|f| render_app(f, state)).unwrap();
    terminal
}

pub(super) fn render_to_buffer(state: &mut AppState, width: u16, height: u16) -> Buffer {
    render(state, width, height).backend().buffer().clone()
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

pub(super) fn region_of(state: &AppState, pred: impl Fn(&HitTarget) -> bool) -> Rect {
    state
        .hit_registry
        .find(pred)
        .map(|r| r.rect)
        .expect("hit region not registered")
}

pub(super) fn hit_center_of(
    state: &AppState,
    pred: impl Fn(&HitTarget) -> bool,
) -> Option<(u16, u16)> {
    state
        .hit_registry
        .find(pred)
        .map(|r| (r.rect.x + r.rect.width / 2, r.rect.y + r.rect.height / 2))
}

pub(super) fn git_ops(effects: &[Effect]) -> Vec<GitOp> {
    effects
        .iter()
        .filter_map(|e| match e {
            Effect::GitOp(op) => Some(op.clone()),
            _ => None,
        })
        .collect()
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
        ContextMenuEntryKind::Action(ContextMenuAction::Run(actions)) => actions.clone(),
        other => panic!("expected Run item, got {other:?}"),
    }
}
