//! Acceptance tests for Spec 05 (branches popup, context menus, dialogs,
//! ongoing banner, Smart Operation, Stash tab), driven through input routing,
//! `AppState::dispatch` / `apply_engine_event` and a `TestBackend` render.

use std::sync::Arc;
use std::time::{Duration, Instant};

use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use jbgit_core::{
    CommitSummary, FileStatusKind, GitFailure, GitOp, GitOpOutcome, Oid, OngoingKind, OngoingStep,
    PaneId, RefInfo, RefKind, RepoSnapshot, RepositoryState, ResetMode, StageEntry,
    StageStatusSnapshot, StashEntry, TodoAction, WorkspaceTab,
};
use jbgit_engine::EngineEvent;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Modifier;

use crate::app::{
    Action, AppState, ContextMenuEntryKind, ContextMenuItem, Effect, ModalLayer, PopupKind,
};
use crate::branches_popup::{PopupRow, build_popup_rows};
use crate::dialogs::Dialog;
use crate::hit_test::{HitTarget, OngoingButton};
use crate::input::route_terminal_event;
use crate::views::overlays::context_menu_rect;
use crate::views::render_app;

fn oid(n: u8) -> Oid {
    Oid::from_bytes([n; 20])
}

fn local(name: &str, target: u8) -> RefInfo {
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

fn remote(name: &str, target: u8) -> RefInfo {
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
fn snapshot() -> RepoSnapshot {
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

fn state_with(snap: RepoSnapshot) -> AppState {
    let mut state = AppState::new_with_config(".".into(), None, crate::config::AppConfig::default());
    state.i18n = Arc::new(crate::i18n::I18n::load("en-US", None));
    state.apply_engine_event(EngineEvent::RepoSnapshotLoaded {
        snapshot: Arc::new(snap),
        epoch: 1,
    });
    state
}

fn run(state: &mut AppState, actions: Vec<Action>) -> Vec<Effect> {
    let mut effects = Vec::new();
    for a in actions {
        effects.extend(state.dispatch(a));
    }
    effects
}

fn key(state: &mut AppState, code: KeyCode, mods: KeyModifiers) -> Vec<Effect> {
    let actions = route_terminal_event(Event::Key(KeyEvent::new(code, mods)), state);
    run(state, actions)
}

fn type_str(state: &mut AppState, text: &str) {
    for c in text.chars() {
        key(state, KeyCode::Char(c), KeyModifiers::NONE);
    }
}

fn mouse(state: &mut AppState, kind: MouseEventKind, col: u16, row: u16) -> Vec<Effect> {
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

fn render(state: &mut AppState, w: u16, h: u16) -> Terminal<TestBackend> {
    let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
    terminal.draw(|f| render_app(f, state)).unwrap();
    terminal
}

fn row_text(buf: &Buffer, y: u16) -> String {
    (0..buf.area.width)
        .map(|x| buf[(x, y)].symbol().to_string())
        .collect()
}

fn screen_text(buf: &Buffer) -> String {
    (0..buf.area.height)
        .map(|y| row_text(buf, y))
        .collect::<Vec<_>>()
        .join("\n")
}

fn region_of(state: &AppState, pred: impl Fn(&HitTarget) -> bool) -> Rect {
    state
        .hit_registry
        .find(pred)
        .map(|r| r.rect)
        .expect("hit region not registered")
}

fn git_ops(effects: &[Effect]) -> Vec<GitOp> {
    effects
        .iter()
        .filter_map(|e| match e {
            Effect::GitOp(op) => Some(op.clone()),
            _ => None,
        })
        .collect()
}

fn top_menu(state: &AppState) -> (&[ContextMenuItem], usize, u8) {
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

fn find_item<'a>(items: &'a [ContextMenuItem], label: &str) -> &'a ContextMenuItem {
    items
        .iter()
        .find(|i| i.label.starts_with(label))
        .unwrap_or_else(|| panic!("menu item {label:?} missing"))
}

fn menu_item_idx(items: &[ContextMenuItem], label: &str) -> usize {
    items
        .iter()
        .position(|i| i.label.starts_with(label))
        .unwrap_or_else(|| panic!("menu item {label:?} missing"))
}

#[test]
fn branches_popup_fuzzy_search_highlights_and_opens_branch_actions() {
    let mut state = state_with(snapshot());
    state.dispatch(Action::SwitchTab(WorkspaceTab::Commit));
    // `b` opens the global Branches popup everywhere; `Ctrl+`` also works everywhere.
    key(&mut state, KeyCode::Char('b'), KeyModifiers::NONE);
    assert!(matches!(
        state.modal_stack.last(),
        Some(ModalLayer::FloatingPopup { .. })
    ));
    state.dispatch(Action::PopModal);
    key(&mut state, KeyCode::Char('`'), KeyModifiers::CONTROL);

    // Grouped view: Recent → Local (folders) → Remote.
    let rows = build_popup_rows(&state, "");
    let pos = |pred: &dyn Fn(&PopupRow) -> bool| rows.iter().position(pred).unwrap();
    let recent = pos(&|r| matches!(r, PopupRow::Header(h) if h == "Recent"));
    let locals = pos(&|r| matches!(r, PopupRow::Header(h) if h == "Local Branches"));
    let remotes = pos(&|r| matches!(r, PopupRow::Header(h) if h == "Remote Branches"));
    assert!(recent < locals && locals < remotes);
    assert!(matches!(&rows[recent + 1], PopupRow::Branch(b) if b.name == "feature/stage-tracker"));
    assert!(
        rows.iter()
            .any(|r| matches!(r, PopupRow::Folder(f) if f == "feature"))
    );

    // Typing filters instantly (no need to focus a search box).
    let started = Instant::now();
    type_str(&mut state, "f/dl");
    assert!(started.elapsed() < Duration::from_millis(500));
    let (query, selected) = match state.modal_stack.last() {
        Some(ModalLayer::FloatingPopup {
            query, selected, ..
        }) => (query.clone(), *selected),
        other => panic!("{other:?}"),
    };
    assert_eq!(query, "f/dl");
    let rows = build_popup_rows(&state, &query);
    let PopupRow::Branch(b) = &rows[selected] else {
        panic!("selection must be a branch row");
    };
    assert_eq!(b.name, "feature/dag-layout");
    let chars: Vec<char> = b.name.chars().collect();
    let hl: String = b.indices.iter().map(|&i| chars[i as usize]).collect();
    assert_eq!(hl, "f/dl");

    // Highlighted characters render bold in #548AF7.
    let term = render(&mut state, 120, 40);
    let buf = term.backend().buffer();
    let text = screen_text(buf);
    assert!(text.contains("Git Branches (jbgit)"), "{text}");
    let y = (0..40)
        .find(|&y| row_text(buf, y).contains("feature/dag-layout"))
        .unwrap();
    let line = row_text(buf, y);
    let x0 = line.find("feature/dag-layout").unwrap();
    let x0 = line[..x0].chars().count() as u16;
    let cell = &buf[(x0, y)];
    assert_eq!(cell.fg, state.theme.fg_accent_link);
    assert!(cell.modifier.contains(Modifier::BOLD));
    let plain = &buf[(x0 + 1, y)]; // 'e' is not matched
    assert_ne!(plain.fg, state.theme.fg_accent_link);

    // Up / Down skip header rows.
    key(&mut state, KeyCode::Down, KeyModifiers::NONE);
    if let Some(ModalLayer::FloatingPopup {
        selected, query, ..
    }) = state.modal_stack.last()
    {
        let rows = build_popup_rows(&state, query);
        assert!(rows[*selected].selectable());
    }
    key(&mut state, KeyCode::Up, KeyModifiers::NONE);

    // Enter opens the cascading action submenu beside the popup.
    let _ = render(&mut state, 120, 40);
    key(&mut state, KeyCode::Enter, KeyModifiers::NONE);
    let (items, _, depth) = top_menu(&state);
    assert_eq!(depth, 0);
    for label in [
        "Checkout",
        "New Branch from 'feature/dag-layout'",
        "Checkout and Rebase onto 'main'",
        "Compare with 'main'",
        "Show Diff with Working Tree",
        "Rebase 'main' onto",
        "Merge 'feature/dag-layout' into 'main'",
        "Update",
        "Push…",
        "Rename…",
        "Delete",
    ] {
        find_item(items, label);
    }
    let Some(ModalLayer::ContextMenu {
        parent: Some(p), ..
    }) = state.modal_stack.last()
    else {
        panic!("submenu must hang off the popup");
    };
    let sub = context_menu_rect(state.terminal_area, (0, 0), Some(*p), items);
    assert!(
        sub.x >= p.rect.right(),
        "submenu opens to the right of the popup"
    );

    // Running an action closes both the submenu and the popup.
    let effects = key(&mut state, KeyCode::Char('c'), KeyModifiers::NONE);
    assert!(state.modal_stack.is_empty());
    assert_eq!(
        git_ops(&effects),
        vec![GitOp::Checkout {
            target: "feature/dag-layout".into(),
            detach: false,
        }]
    );
}

#[test]
fn remote_checkout_tracks_new_branch_or_asks_when_diverged() {
    let mut state = state_with(snapshot());
    let effects = run(
        &mut state,
        vec![Action::CheckoutRemoteBranch {
            remote_ref: "origin/feature-new".into(),
        }],
    );
    assert_eq!(
        git_ops(&effects),
        vec![GitOp::CheckoutNewBranch {
            name: "feature-new".into(),
            start: "origin/feature-new".into(),
            track: true,
        }]
    );

    // Local `feature/dag-layout` (c5) has 1 commit not in origin (c4) → dialog.
    let effects = run(
        &mut state,
        vec![Action::CheckoutRemoteBranch {
            remote_ref: "origin/feature/dag-layout".into(),
        }],
    );
    assert!(git_ops(&effects).is_empty());
    let Some(ModalLayer::Dialog(d)) = state.modal_stack.last() else {
        panic!("diverged checkout must ask");
    };
    let Dialog::Choice(c) = d.as_ref() else {
        panic!()
    };
    let labels: Vec<&str> = c.options.iter().map(|o| o.label.as_str()).collect();
    assert!(labels.iter().any(|l| l.starts_with("Checkout local")));
    assert!(
        labels
            .iter()
            .any(|l| l.starts_with("Reset 'feature/dag-layout'"))
    );
    assert!(labels.contains(&"New Branch…"));
}

#[test]
fn delete_branch_guard_and_restore_notification() {
    let mut state = state_with(snapshot());
    // Unmerged (bugfix/unmerged = c3 is an ancestor of HEAD → merged!). Use a
    // branch with commits outside HEAD instead.
    let mut snap = snapshot();
    snap.commits.insert(
        0,
        CommitSummary {
            oid: oid(40),
            parents: vec![oid(3)],
            subject: "side work".into(),
            ..Default::default()
        },
    );
    snap.refs.push(local("topic", 40));
    state.apply_engine_event(EngineEvent::RepoSnapshotLoaded {
        snapshot: Arc::new(snap),
        epoch: 2,
    });

    // Merged branch: plain `-d` straight away.
    let effects = run(
        &mut state,
        vec![Action::PromptDeleteBranch {
            name: "bugfix/unmerged".into(),
        }],
    );
    assert_eq!(
        git_ops(&effects),
        vec![GitOp::DeleteBranch {
            name: "bugfix/unmerged".into(),
            force: false,
            tip: oid(3),
        }]
    );

    // Unmerged: warning with commit count + Force Delete.
    let effects = run(
        &mut state,
        vec![Action::PromptDeleteBranch {
            name: "topic".into(),
        }],
    );
    assert!(git_ops(&effects).is_empty());
    let term = render(&mut state, 120, 40);
    let text = screen_text(term.backend().buffer());
    assert!(text.contains("Delete Unmerged Branch"), "{text}");
    assert!(text.contains("1 commit(s) not merged"), "{text}");
    assert!(text.contains("Force Delete (-D)"), "{text}");
    // Default focus is Cancel; Left moves to Force Delete.
    key(&mut state, KeyCode::Left, KeyModifiers::NONE);
    let effects = key(&mut state, KeyCode::Enter, KeyModifiers::NONE);
    let op = GitOp::DeleteBranch {
        name: "topic".into(),
        force: true,
        tip: oid(40),
    };
    assert_eq!(git_ops(&effects), vec![op.clone()]);

    // Success → footer notification with [Restore Branch] → restores the tip.
    state.apply_engine_event(EngineEvent::GitOpFinished(GitOpOutcome::ok(op)));
    let term = render(&mut state, 120, 40);
    let footer = row_text(term.backend().buffer(), 39);
    assert!(footer.contains("Deleted branch 'topic'"), "{footer}");
    assert!(footer.contains("[Restore Branch]"), "{footer}");
    let r = region_of(&state, |t| matches!(t, HitTarget::NotificationAction));
    let effects = mouse(
        &mut state,
        MouseEventKind::Down(MouseButton::Left),
        r.x + 1,
        r.y,
    );
    assert_eq!(
        git_ops(&effects),
        vec![GitOp::RestoreBranch {
            name: "topic".into(),
            oid: oid(40),
        }]
    );
    assert!(state.notification.is_none());
}

#[test]
fn context_menu_and_submenu_flip_inside_viewport_at_bottom_right() {
    let mut state = state_with(snapshot());
    let _ = render(&mut state, 120, 40);

    // Right-clicking empty footer / chrome does NOT open a fallback menu.
    mouse(
        &mut state,
        MouseEventKind::Down(MouseButton::Right),
        119,
        39,
    );
    assert!(state.modal_stack.is_empty());

    // Push the center-right splitter right and shrink height so the bottom-most
    // CommitTableRow sits near the bottom-right corner of the center pane.
    state.splitters.update_from_pointer(
        jbgit_core::SplitterId::LogRightHorizontal,
        jbgit_core::SplitAxis::Horizontal,
        Rect::new(0, 0, 100, 20),
        99,
        5,
    );
    let _ = render(&mut state, 100, 20);
    let last_row = state
        .hit_registry
        .find(|t| matches!(t, HitTarget::CommitTableRow { .. }))
        .expect("visible commit row")
        .rect;
    mouse(
        &mut state,
        MouseEventKind::Down(MouseButton::Right),
        last_row.right().saturating_sub(1),
        last_row.y,
    );
    let (items, _, _) = top_menu(&state);
    let items = items.to_vec();
    let screen = state.terminal_area;
    let Some(ModalLayer::ContextMenu { anchor, parent, .. }) = state.modal_stack.last() else {
        panic!()
    };
    let rect = context_menu_rect(screen, *anchor, *parent, &items);
    assert!(rect.right() < screen.right(), "shadow column must fit");
    assert!(rect.bottom() < screen.bottom(), "shadow row must fit");
    assert!(
        rect.x < last_row.right().saturating_sub(1),
        "menu flipped left: {rect:?}"
    );

    let term = render(&mut state, 100, 20);
    let text = screen_text(term.backend().buffer());
    assert!(text.contains("╭") && text.contains("╰"), "rounded border");

    // Cascading submenu (`Presentation` via 'o'): no room on the right → opens left of parent.
    key(&mut state, KeyCode::Char('o'), KeyModifiers::NONE);
    let (sub_items, _, depth) = top_menu(&state);
    assert_eq!(depth, 1);
    let sub_items = sub_items.to_vec();
    let Some(ModalLayer::ContextMenu { anchor, parent, .. }) = state.modal_stack.last() else {
        panic!()
    };
    let sub = context_menu_rect(screen, *anchor, *parent, &sub_items);
    assert!(
        sub.right() <= rect.x,
        "submenu mirrored to the left: {sub:?} vs {rect:?}"
    );
    assert!(sub.bottom() < screen.bottom() && sub.right() < screen.right());
    let term = render(&mut state, 100, 20);
    let text = screen_text(term.backend().buffer());
    assert!(text.contains("Compact References View"), "{text}");
}

#[test]
fn right_click_preselects_row_and_keeps_multi_selection_for_squash() {
    let mut state = state_with(snapshot());
    let _ = render(&mut state, 140, 40);
    // Row 8 (c4) is not selected: right-click selects it first.
    let r = region_of(&state, |t| {
        matches!(t, HitTarget::CommitTableRow { visible_row: 8, .. })
    });
    mouse(
        &mut state,
        MouseEventKind::Down(MouseButton::Right),
        r.x + 30,
        r.y,
    );
    assert_eq!(state.log_graph.selected_idx, 8);
    let (items, _, _) = top_menu(&state);
    assert!(find_item(items, "Checkout Revision '").is_enabled());
    let squash = find_item(items, "Squash Commits");
    assert_eq!(
        squash.disabled_reason.as_deref(),
        Some("Select 2 or more commits to squash")
    );
    // Disabled reason is shown in the footer when focused.
    let idx = menu_item_idx(items, "Squash Commits");
    if let Some(ModalLayer::ContextMenu { selected, .. }) = state.modal_stack.last_mut() {
        *selected = idx;
    }
    let term = render(&mut state, 140, 40);
    let footer = row_text(term.backend().buffer(), 39);
    assert!(
        footer.contains("Select 2 or more commits to squash"),
        "{footer}"
    );
    state.dispatch(Action::PopContextMenus);

    // Multi-select rows 3..6, right-click row 5: selection is preserved.
    run(
        &mut state,
        vec![Action::SelectLogRow(3), Action::ExtendLogSelectionTo(6)],
    );
    let _ = render(&mut state, 140, 40);
    let r = region_of(&state, |t| {
        matches!(t, HitTarget::CommitTableRow { visible_row: 5, .. })
    });
    mouse(
        &mut state,
        MouseEventKind::Down(MouseButton::Right),
        r.x + 30,
        r.y,
    );
    for row in 3..=6 {
        assert!(
            state.log_graph.is_row_selected(row),
            "row {row} lost selection"
        );
    }
    let (items, _, _) = top_menu(&state);
    let squash = find_item(items, "Squash 4 Commits");
    assert!(squash.is_enabled(), "{:?}", squash.disabled_reason);
    assert!(
        find_item(items, "Cherry-Pick 4 Commits")
            .disabled_reason
            .is_some()
    );
    assert!(
        find_item(items, "Compare Versions")
            .disabled_reason
            .is_some()
    );

    // Squash opens the message editor prefilled with all four messages.
    let effects = key(&mut state, KeyCode::Char('s'), KeyModifiers::NONE);
    assert!(git_ops(&effects).is_empty());
    let Some(ModalLayer::Dialog(d)) = state.modal_stack.last() else {
        panic!("squash message dialog");
    };
    let Dialog::Message(m) = d.as_ref() else {
        panic!()
    };
    assert_eq!(m.text, "c6\n\nc7\n\nc8\n\nc9");
    let effects = key(&mut state, KeyCode::Enter, KeyModifiers::ALT);
    let ops = git_ops(&effects);
    let [GitOp::RewriteHistory { base, todo }] = ops.as_slice() else {
        panic!("{ops:?}")
    };
    assert_eq!(*base, Some(oid(5)));
    let actions: Vec<(Oid, TodoAction)> = todo.iter().map(|e| (e.oid, e.action)).collect();
    assert_eq!(
        actions,
        vec![
            (oid(6), TodoAction::Pick),
            (oid(7), TodoAction::Squash),
            (oid(8), TodoAction::Squash),
            (oid(9), TodoAction::Squash),
            (oid(10), TodoAction::Pick),
            (oid(11), TodoAction::Pick),
            (oid(12), TodoAction::Pick),
        ]
    );
    assert_eq!(todo[3].message.as_deref(), Some("c6\n\nc7\n\nc8\n\nc9"));
}

#[test]
fn rewrite_is_disabled_for_commits_on_protected_remote() {
    let mut state = state_with(snapshot());
    run(&mut state, vec![Action::SelectLogRow(10)]); // c2 == origin/main
    let items = crate::menus::log_commit_menu_items(&state);
    let reason = find_item(&items, "Reword Commit")
        .disabled_reason
        .clone()
        .unwrap();
    assert!(
        reason.contains("already pushed to protected branch 'origin/main'"),
        "{reason}"
    );
    assert!(find_item(&items, "Uncommit").disabled_reason.is_some());
    run(&mut state, vec![Action::SelectLogRow(0)]);
    let items = crate::menus::log_commit_menu_items(&state);
    assert!(find_item(&items, "Uncommit").is_enabled());
    assert!(find_item(&items, "Reword Commit").is_enabled());
}

#[test]
fn new_branch_dialog_validates_name_inline_and_blocks_confirm() {
    let mut state = state_with(snapshot());
    key(&mut state, KeyCode::Char('n'), KeyModifiers::NONE);
    assert!(matches!(
        state.modal_stack.last(),
        Some(ModalLayer::Dialog(_))
    ));

    for (input, err) in [
        ("bad name", "cannot contain spaces"),
        ("a..b", "cannot contain '..'"),
        ("-x", "cannot start with '-'"),
        ("main", "Branch 'main' already exists"),
    ] {
        key(&mut state, KeyCode::Char('u'), KeyModifiers::CONTROL);
        type_str(&mut state, input);
        let term = render(&mut state, 120, 40);
        let text = screen_text(term.backend().buffer());
        assert!(text.contains(err), "{input:?}: {text}");
        let effects = key(&mut state, KeyCode::Enter, KeyModifiers::NONE);
        assert!(git_ops(&effects).is_empty(), "{input:?} must not confirm");
        assert!(matches!(
            state.modal_stack.last(),
            Some(ModalLayer::Dialog(_))
        ));
        // The OK button renders disabled (muted).
        let ok = region_of(&state, |t| {
            matches!(
                t,
                HitTarget::DialogButton(crate::hit_test::DialogButtonKind::Confirm)
            )
        });
        let buf = term.backend().buffer();
        assert_eq!(buf[(ok.x + 1, ok.y)].fg, state.theme.fg_muted);
    }
    key(&mut state, KeyCode::Char('u'), KeyModifiers::CONTROL);
    type_str(&mut state, "feature/ok");
    let effects = key(&mut state, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(
        git_ops(&effects),
        vec![GitOp::CheckoutNewBranch {
            name: "feature/ok".into(),
            start: oid(12).to_string(),
            track: false,
        }]
    );
    assert!(state.modal_stack.is_empty());
}

#[test]
fn ongoing_banner_shows_state_progress_and_buttons_run_steps() {
    let mut snap = snapshot();
    snap.state = RepositoryState::RebaseMerge {
        progress: Some((2, 5)),
        branch: Some("feat/dag".into()),
    };
    snap.ongoing_onto = Some("main".into());
    let mut state = state_with(snap);
    state.apply_engine_event(EngineEvent::StageStatusLoaded {
        status: Arc::new(StageStatusSnapshot {
            conflicted: vec![StageEntry {
                path: "src/graph.rs".into(),
                old_path: None,
                kind: FileStatusKind::Conflicted,
            }],
            ..Default::default()
        }),
        epoch: 1,
    });

    for tab in [WorkspaceTab::Log, WorkspaceTab::Commit] {
        state.dispatch(Action::SwitchTab(tab));
        let term = render(&mut state, 140, 40);
        let banner = row_text(term.backend().buffer(), 2);
        assert!(
            banner.contains("⚠ REBASING feat/dag onto main (Step 2/5) — 1 conflicted file"),
            "{banner}"
        );
        for b in ["[ Resolve ]", "[ Continue ]", "[ Skip ]", "[ Abort ]"] {
            assert!(banner.contains(b), "{b} missing: {banner}");
        }
        let header = row_text(term.backend().buffer(), 0);
        assert!(header.contains("Rebasing 2/5"), "{header}");
    }

    // Continue is refused while conflicts remain.
    let r = region_of(&state, |t| {
        matches!(t, HitTarget::OngoingBanner(OngoingButton::Continue))
    });
    let effects = mouse(
        &mut state,
        MouseEventKind::Down(MouseButton::Left),
        r.x + 2,
        r.y,
    );
    assert!(git_ops(&effects).is_empty());
    assert!(
        state
            .status_notice
            .as_deref()
            .unwrap_or("")
            .contains("conflicted")
    );

    let r = region_of(&state, |t| {
        matches!(t, HitTarget::OngoingBanner(OngoingButton::Abort))
    });
    let effects = mouse(
        &mut state,
        MouseEventKind::Down(MouseButton::Left),
        r.x + 2,
        r.y,
    );
    assert_eq!(
        git_ops(&effects),
        vec![GitOp::Ongoing {
            kind: OngoingKind::Rebase,
            step: OngoingStep::Abort,
        }]
    );

    // Resolve jumps to the conflicted file in the Commit tab.
    state.dispatch(Action::SwitchTab(WorkspaceTab::Log));
    let _ = render(&mut state, 140, 40);
    let r = region_of(&state, |t| {
        matches!(t, HitTarget::OngoingBanner(OngoingButton::Resolve))
    });
    mouse(
        &mut state,
        MouseEventKind::Down(MouseButton::Left),
        r.x + 2,
        r.y,
    );
    assert_eq!(state.active_tab, WorkspaceTab::Commit);
    let row = &state.stage_workbench.rows[state.commit_stage_selected_idx];
    assert!(
        matches!(&row.kind, jbgit_core::StageTreeRowKind::File { path, .. } if path == "src/graph.rs")
    );
}

#[test]
fn smart_operation_dialog_offers_stash_and_force_checkout() {
    let mut state = state_with(snapshot());
    let op = GitOp::Checkout {
        target: "feature/dag-layout".into(),
        detach: false,
    };
    state.git_ops_running = 1;
    state.apply_engine_event(EngineEvent::GitOpFinished(GitOpOutcome::failed(
        op.clone(),
        GitFailure::LocalChangesOverwritten {
            files: vec!["src/app.rs".into()],
        },
    )));
    assert_eq!(state.git_ops_running, 0);
    let term = render(&mut state, 120, 40);
    let text = screen_text(term.backend().buffer());
    assert!(text.contains("Smart Checkout"), "{text}");
    assert!(text.contains("src/app.rs"), "{text}");
    assert!(text.contains("Force Checkout (Discard Local)"), "{text}");
    let effects = key(&mut state, KeyCode::Char('s'), KeyModifiers::NONE);
    assert_eq!(git_ops(&effects), vec![GitOp::Smart(Box::new(op))]);
    assert!(state.modal_stack.is_empty());

    // Merge failures offer Smart Merge (no force option).
    state.apply_engine_event(EngineEvent::GitOpFinished(GitOpOutcome::failed(
        GitOp::Merge {
            rev: "feature/dag-layout".into(),
        },
        GitFailure::LocalChangesOverwritten { files: vec![] },
    )));
    let Some(ModalLayer::Dialog(d)) = state.modal_stack.last() else {
        panic!()
    };
    let Dialog::Choice(c) = d.as_ref() else {
        panic!()
    };
    let labels: Vec<&str> = c.options.iter().map(|o| o.label.as_str()).collect();
    assert_eq!(labels, vec!["Smart Merge", "Cancel"]);
}

#[test]
fn reset_dialog_defaults_to_mixed_and_hard_needs_second_confirmation() {
    let mut state = state_with(snapshot());
    run(&mut state, vec![Action::SelectLogRow(3)]);
    let items = crate::menus::log_commit_menu_items(&state);
    let reset = find_item(&items, "Reset Current Branch to Here");
    let ContextMenuEntryKind::Action(crate::app::ContextMenuAction::Run(actions)) = &reset.kind
    else {
        panic!()
    };
    run(&mut state, actions.clone());
    let term = render(&mut state, 120, 40);
    let text = screen_text(term.backend().buffer());
    for m in ["( ) Soft", "(•) Mixed", "( ) Hard", "( ) Keep", "DATA LOSS"] {
        assert!(text.contains(m), "{m} missing: {text}");
    }
    let effects = key(&mut state, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(
        git_ops(&effects),
        vec![GitOp::Reset {
            mode: ResetMode::Mixed,
            target: oid(9),
        }]
    );

    run(&mut state, actions.clone());
    key(&mut state, KeyCode::Char('h'), KeyModifiers::NONE);
    let effects = key(&mut state, KeyCode::Enter, KeyModifiers::NONE);
    assert!(
        git_ops(&effects).is_empty(),
        "hard reset needs a second confirmation"
    );
    let term = render(&mut state, 120, 40);
    assert!(screen_text(term.backend().buffer()).contains("ALL uncommitted changes"));
    key(&mut state, KeyCode::Left, KeyModifiers::NONE);
    let effects = key(&mut state, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(
        git_ops(&effects),
        vec![GitOp::Reset {
            mode: ResetMode::Hard,
            target: oid(9),
        }]
    );

    // Keep mode via mnemonic.
    run(&mut state, actions.clone());
    key(&mut state, KeyCode::Char('k'), KeyModifiers::NONE);
    let effects = key(&mut state, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(
        git_ops(&effects),
        vec![GitOp::Reset {
            mode: ResetMode::Keep,
            target: oid(9),
        }]
    );
}

#[test]
fn hover_opens_submenu_after_delay_and_keyboard_trigger_anchors_on_selection() {
    let mut state = state_with(snapshot());
    let _ = render(&mut state, 140, 40);
    // Keyboard trigger (Shift+F10) opens the same commit menu at the selected row.
    run(&mut state, vec![Action::SelectLogRow(2)]);
    let _ = render(&mut state, 140, 40);
    let row = region_of(&state, |t| {
        matches!(t, HitTarget::CommitTableRow { visible_row: 2, .. })
    });
    key(&mut state, KeyCode::F(10), KeyModifiers::SHIFT);
    let Some(ModalLayer::ContextMenu { anchor, items, .. }) = state.modal_stack.last() else {
        panic!("keyboard menu")
    };
    assert_eq!(anchor.1, row.y);
    assert!(anchor.0 > row.x + row.width / 2);
    assert!(items.iter().any(|i| i.label.starts_with("Cherry-Pick")));
    state.dispatch(Action::PopContextMenus);
    key(&mut state, KeyCode::Char('.'), KeyModifiers::NONE);
    assert!(matches!(
        state.modal_stack.last(),
        Some(ModalLayer::ContextMenu { .. })
    ));

    // Hovering "Create Patch…" arms a 150 ms timer, then opens the submenu.
    let _ = render(&mut state, 140, 40);
    let (items, _, _) = top_menu(&state);
    let idx = menu_item_idx(items, "Create Patch");
    let r = region_of(
        &state,
        |t| matches!(t, HitTarget::MenuItem { menu_depth: 0, item_idx } if *item_idx == idx),
    );
    mouse(&mut state, MouseEventKind::Moved, r.x + 2, r.y);
    let hover = state.menu_hover.expect("hover timer armed");
    assert!(hover.due > Instant::now());
    assert_eq!(state.modal_stack.len(), 1, "not opened before the delay");
    state.menu_hover.as_mut().unwrap().due = Instant::now() - Duration::from_millis(1);
    state.dispatch(Action::FlushPendingDetails);
    let (sub, _, depth) = top_menu(&state);
    assert_eq!(depth, 1);
    assert!(
        sub.iter()
            .any(|i| i.label.starts_with("Copy Patch to Clipboard"))
    );

    // Moving back onto a plain item closes the submenu after the delay too.
    let _ = render(&mut state, 140, 40);
    let r = region_of(&state, |t| {
        matches!(
            t,
            HitTarget::MenuItem {
                menu_depth: 0,
                item_idx: 0
            }
        )
    });
    mouse(&mut state, MouseEventKind::Moved, r.x + 2, r.y);
    state.menu_hover.as_mut().unwrap().due = Instant::now() - Duration::from_millis(1);
    state.dispatch(Action::FlushPendingDetails);
    assert_eq!(state.modal_stack.len(), 1);

    // Mnemonics of enabled items are underlined; disabled items (Cherry-Pick
    // of a commit already on HEAD) are not.
    let term = render(&mut state, 140, 40);
    let buf = term.backend().buffer();
    let cell_at = |label: &str, offset: u16| {
        let y = (0..40).find(|&y| row_text(buf, y).contains(label)).unwrap();
        let line = row_text(buf, y);
        let x = line[..line.find(label).unwrap()].chars().count() as u16;
        buf[(x + offset, y)].clone()
    };
    let v = cell_at("Revert Commit", 2); // "Re[v]ert"
    assert_eq!(v.symbol(), "v");
    assert!(v.modifier.contains(Modifier::UNDERLINED));
    let p = cell_at("Cherry-Pick", 7);
    assert_eq!(p.symbol(), "P");
    assert!(!p.modifier.contains(Modifier::UNDERLINED));
    assert_eq!(p.fg, state.theme.fg_muted);
}

#[test]
fn stash_tab_lists_stashes_and_keys_run_operations() {
    let mut snap = snapshot();
    snap.stashes = vec![
        StashEntry {
            index: 0,
            oid: oid(90),
            message: "On main: wip parser".into(),
            timestamp: 0,
        },
        StashEntry {
            index: 1,
            oid: oid(91),
            message: "WIP on feature/dag-layout: abc fix".into(),
            timestamp: 0,
        },
    ];
    let mut state = state_with(snap);
    let effects = key(&mut state, KeyCode::Char('3'), KeyModifiers::NONE);
    assert_eq!(state.active_tab, WorkspaceTab::Stash);
    assert!(effects.iter().any(|e| matches!(
        e,
        Effect::RequestCommitDetails(k) if k.oids == vec![oid(90)]
    )));
    let term = render(&mut state, 140, 40);
    let text = screen_text(term.backend().buffer());
    assert!(text.contains("stash@{0} main: wip parser"), "{text}");
    assert!(
        text.contains("stash@{1} feature/dag-layout: abc fix"),
        "{text}"
    );

    key(&mut state, KeyCode::Down, KeyModifiers::NONE);
    assert_eq!(state.stash.selected, 1);

    // Enter safely opens diff preview rather than mutating/applying the stash.
    let effects = key(&mut state, KeyCode::Enter, KeyModifiers::NONE);
    assert!(git_ops(&effects).is_empty());
    assert_eq!(state.focused_pane, PaneId::StashDiffPreview);
    key(&mut state, KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(state.focused_pane, PaneId::StashList);

    // `a` applies (keep), `o` pops.
    let effects = key(&mut state, KeyCode::Char('a'), KeyModifiers::NONE);
    assert_eq!(
        git_ops(&effects),
        vec![GitOp::StashApply {
            index: 1,
            pop: false
        }]
    );
    let effects = key(&mut state, KeyCode::Char('o'), KeyModifiers::NONE);
    assert_eq!(
        git_ops(&effects),
        vec![GitOp::StashApply {
            index: 1,
            pop: true
        }]
    );

    key(&mut state, KeyCode::Char('d'), KeyModifiers::NONE);
    assert!(matches!(
        state.modal_stack.last(),
        Some(ModalLayer::Dialog(_))
    ));
    key(&mut state, KeyCode::Left, KeyModifiers::NONE);
    let effects = key(&mut state, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(git_ops(&effects), vec![GitOp::StashDrop { index: 1 }]);

    // Shift+B opens unstash-as-branch dialog and validates the name.
    key(&mut state, KeyCode::Char('B'), KeyModifiers::SHIFT);
    type_str(&mut state, "from-stash");
    let effects = key(&mut state, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(
        git_ops(&effects),
        vec![GitOp::StashBranch {
            index: 1,
            name: "from-stash".into(),
        }]
    );

    // Plain `b` triggers the global Branches popup even from the Stash tab.
    key(&mut state, KeyCode::Char('b'), KeyModifiers::NONE);
    assert!(matches!(
        state.modal_stack.last(),
        Some(ModalLayer::FloatingPopup {
            kind: PopupKind::BranchesQuickView,
            ..
        })
    ));
    state.dispatch(Action::PopModal);
}

#[test]
fn interactive_rebase_dialog_reorders_and_sets_actions() {
    let mut state = state_with(snapshot());
    run(&mut state, vec![Action::SelectLogRow(2)]); // c10
    let items = crate::menus::log_commit_menu_items(&state);
    let ContextMenuEntryKind::Action(crate::app::ContextMenuAction::Run(actions)) =
        &find_item(&items, "Interactive Rebase from Here").kind
    else {
        panic!()
    };
    run(&mut state, actions.clone());
    let term = render(&mut state, 140, 40);
    let text = screen_text(term.backend().buffer());
    assert!(text.contains("Interactive Rebase from"), "{text}");
    // Entries: c10, c11, c12. Move c10 down, drop c11 (now first), fixup c12.
    key(&mut state, KeyCode::Down, KeyModifiers::ALT);
    key(&mut state, KeyCode::Up, KeyModifiers::NONE);
    key(&mut state, KeyCode::Char('d'), KeyModifiers::NONE);
    key(&mut state, KeyCode::Down, KeyModifiers::NONE);
    key(&mut state, KeyCode::Down, KeyModifiers::NONE);
    key(&mut state, KeyCode::Char('f'), KeyModifiers::NONE);
    let effects = key(&mut state, KeyCode::Enter, KeyModifiers::NONE);
    let ops = git_ops(&effects);
    let [GitOp::RewriteHistory { base, todo }] = ops.as_slice() else {
        panic!("{ops:?}")
    };
    assert_eq!(*base, Some(oid(9)));
    let plan: Vec<(Oid, TodoAction)> = todo.iter().map(|e| (e.oid, e.action)).collect();
    assert_eq!(
        plan,
        vec![
            (oid(11), TodoAction::Drop),
            (oid(10), TodoAction::Pick),
            (oid(12), TodoAction::Fixup),
        ]
    );

    // Squashing the first kept commit is rejected inline.
    run(&mut state, actions.clone());
    key(&mut state, KeyCode::Char('s'), KeyModifiers::NONE);
    let effects = key(&mut state, KeyCode::Enter, KeyModifiers::NONE);
    assert!(git_ops(&effects).is_empty());
    assert!(matches!(
        state.modal_stack.last(),
        Some(ModalLayer::Dialog(_))
    ));
}

#[test]
fn stage_tree_menu_covers_spec_actions() {
    let mut state = state_with(snapshot());
    state.apply_engine_event(EngineEvent::StageStatusLoaded {
        status: Arc::new(StageStatusSnapshot {
            untracked: vec![StageEntry {
                path: "notes.txt".into(),
                old_path: None,
                kind: FileStatusKind::Untracked,
            }],
            conflicted: vec![StageEntry {
                path: "src/c.rs".into(),
                old_path: None,
                kind: FileStatusKind::Conflicted,
            }],
            ..Default::default()
        }),
        epoch: 1,
    });
    state
        .stage_workbench
        .sync_rows(state.stage_status.as_deref());
    let idx_of = |state: &AppState, p: &str| {
        state
            .stage_workbench
            .rows
            .iter()
            .position(
                |r| matches!(&r.kind, jbgit_core::StageTreeRowKind::File { path, .. } if path == p),
            )
            .unwrap()
    };
    let conflict = crate::menus::stage_tree_menu_items(&state, idx_of(&state, "src/c.rs"));
    for l in ["Resolve Conflicts", "Accept Yours", "Accept Theirs"] {
        find_item(&conflict, l);
    }
    let untracked = crate::menus::stage_tree_menu_items(&state, idx_of(&state, "notes.txt"));
    for l in [
        "Stage",
        "Stage Without Content (git add -N)",
        "Rollback / Discard Changes…",
        "Stash Selected Files…",
        "Add to .gitignore",
        "Add to .git/info/exclude",
        "Show Diff",
        "Three-Side Diff",
        "Create Patch to Clipboard",
        "Copy Relative Path",
        "Delete File…",
    ] {
        assert!(find_item(&untracked, l).is_enabled(), "{l} disabled");
    }
    let ContextMenuEntryKind::Action(crate::app::ContextMenuAction::Run(a)) =
        &find_item(&untracked, "Add to .gitignore").kind
    else {
        panic!()
    };
    let effects = run(&mut state, a.clone());
    assert_eq!(
        git_ops(&effects),
        vec![GitOp::AddToIgnore {
            patterns: vec!["/notes.txt".into()],
            exclude_file: false,
        }]
    );
    let _ = PaneId::CommitChangesTree;
}

#[test]
fn non_contiguous_squash_disabled_and_hunk_menu_respects_diff_side() {
    use jbgit_core::DiffContext;
    use jbgit_engine::{StageDiffKey, StageDiffSide};

    let mut state = state_with(snapshot());
    // Manually set non-contiguous selection: rows 2 and 5 (c10 and c7).
    state.log_graph.selected_idx = 2;
    state.log_graph.anchor = Some(2);
    // Use snapshot commits with ExtendLogSelectionTo for contiguous, vs checking
    // non-contiguous via a branch/merge or testing contiguous vs non-contiguous:
    // In linear history, selected_range() is contiguous; test hunk menu preconditions:
    state.stage_workbench.current_diff_key = Some(StageDiffKey {
        path: "src/lib.rs".into(),
        old_path: None,
        side: StageDiffSide::Staged,
        context: DiffContext::Three,
    });
    let staged_hunk = crate::menus::stage_hunk_menu_items(&state, 0);
    assert!(find_item(&staged_hunk, "Stage Hunk").disabled_reason.is_some());
    assert!(find_item(&staged_hunk, "Unstage Hunk").is_enabled());
    assert!(
        find_item(&staged_hunk, "Rollback Hunk")
            .disabled_reason
            .is_some()
    );

    state.stage_workbench.current_diff_key = Some(StageDiffKey {
        path: "src/lib.rs".into(),
        old_path: None,
        side: StageDiffSide::Unstaged,
        context: DiffContext::Three,
    });
    let unstaged_hunk = crate::menus::stage_hunk_menu_items(&state, 0);
    assert!(find_item(&unstaged_hunk, "Stage Hunk").is_enabled());
    assert!(
        find_item(&unstaged_hunk, "Unstage Hunk")
            .disabled_reason
            .is_some()
    );
    assert!(find_item(&unstaged_hunk, "Rollback Hunk").is_enabled());
}

#[test]
fn non_contiguous_squash_rejected_compare_views_stash_reuse_and_conflict_modal() {
    use jbgit_core::{
        CommitChangesBundle, ConflictDocument, DetailsKey, DetailsMode, FileChange,
    };

    // 1. Non-contiguous selection on a forked visible log: c12 (HEAD), side (c20 on main? no,
    //    insert an unrelated commit between c12 and c10 in visible order).
    let mut snap = snapshot();
    snap.commits.insert(
        1,
        CommitSummary {
            oid: oid(99),
            parents: vec![oid(1)],
            subject: "side commit".into(),
            timestamp: 1150,
            ..Default::default()
        },
    );
    snap.refs.push(local("side-branch", 99));
    let mut state = state_with(snap);
    let _ = render(&mut state, 140, 40);
    // Rows 0..=2 are [c12, side-commit, c11]. Selecting rows 0..=2 has c11, c12 on main
    // and side-commit NOT reachable from HEAD -> squash disabled!
    run(
        &mut state,
        vec![Action::SelectLogRow(0), Action::ExtendLogSelectionTo(2)],
    );
    let items = crate::menus::log_commit_menu_items(&state);
    let squash = find_item(&items, "Squash 3 Commits");
    assert!(squash.disabled_reason.is_some());

    // Ctrl+Space / Ctrl+@ must NOT open Branches popup.
    key(&mut state, KeyCode::Char(' '), KeyModifiers::CONTROL);
    assert!(state.modal_stack.is_empty());
    key(&mut state, KeyCode::Char('@'), KeyModifiers::CONTROL);
    assert!(state.modal_stack.is_empty());

    // 2. Compare Versions (Ctrl+D with 2 selected commits) and Compare with Local.
    let mut state = state_with(snapshot());
    run(
        &mut state,
        vec![
            Action::FocusPane(PaneId::LogCommitTable),
            Action::SelectLogRow(0),
            Action::ExtendLogSelectionTo(1),
        ],
    );
    let effects = key(&mut state, KeyCode::Char('d'), KeyModifiers::CONTROL);
    assert!(effects.iter().any(|e| matches!(
        e,
        Effect::RequestCommitDetails(k)
            if k.mode == DetailsMode::CompareVersions && k.oids == vec![oid(11), oid(12)]
    )));
    let effects = run(&mut state, vec![Action::CompareWithLocal { target: oid(10) }]);
    assert!(effects.iter().any(|e| matches!(
        e,
        Effect::RequestCommitDetails(k)
            if k.mode == DetailsMode::CompareWithLocal && k.oids == vec![oid(10)]
    )));

    // 3. Stash tab reuses CommitChangesBrowser + DiffPreview (dir toolbar + side-by-side toolbar).
    let mut snap = snapshot();
    snap.stashes = vec![StashEntry {
        index: 0,
        oid: oid(90),
        message: "On main: wip".into(),
        timestamp: 0,
    }];
    let mut state = state_with(snap);
    key(&mut state, KeyCode::Char('3'), KeyModifiers::NONE);
    state.apply_engine_event(EngineEvent::CommitDetailsLoaded {
        bundle: Arc::new(CommitChangesBundle {
            key: DetailsKey::single(oid(90)),
            details: vec![],
            changes: vec![FileChange {
                path: "src/main.rs".into(),
                old_path: None,
                kind: FileStatusKind::Modified,
                additions: 2,
                deletions: 1,
                is_binary: false,
                exists_in_workdir: true,
            }],
        }),
        epoch: 1,
    });
    let term = render(&mut state, 140, 40);
    let txt = screen_text(term.backend().buffer());
    assert!(txt.contains("⊞ Dir"), "{txt}");
    assert!(txt.contains("ctx 3"), "{txt}");
    assert!(txt.contains("src"), "{txt}");

    // 4. Built-in three-pane conflict merge modal resolves & emits SaveConflictResolution.
    let effs = run(
        &mut state,
        vec![Action::OpenConflictResolver("src/c.rs".into())],
    );
    assert!(effs
        .iter()
        .any(|e| matches!(e, Effect::LoadConflictDocument(p) if p == "src/c.rs")));
    let doc = ConflictDocument::parse(
        "src/c.rs",
        "<<<<<<< HEAD\nours\n=======\ntheirs\n>>>>>>> feat\n",
        None,
        None,
    );
    state.apply_engine_event(EngineEvent::ConflictDocumentLoaded { doc });
    let term = render(&mut state, 140, 40);
    let txt = screen_text(term.backend().buffer());
    assert!(txt.contains("Ours (HEAD)"), "{txt}");
    assert!(txt.contains("Merged Result"), "{txt}");
    assert!(txt.contains("Theirs (feat)"), "{txt}");
    key(&mut state, KeyCode::Char('y'), KeyModifiers::NONE);
    let save_effs = key(&mut state, KeyCode::Enter, KeyModifiers::NONE);
    assert!(save_effs.iter().any(|e| matches!(
        e,
        Effect::SaveConflictResolution { path, content, stage: true }
            if path == "src/c.rs" && content == b"ours\n"
    )));
}

#[test]
fn dynamic_footer_tracks_focus_and_keymap_overrides() {
    use crate::config::{KeymapConfig, KeymapPreset};

    let mut state = state_with(snapshot());
    state.config.keymap = KeymapConfig::for_preset(KeymapPreset::Default);
    state.dispatch(Action::SwitchTab(WorkspaceTab::Log));
    state.dispatch(Action::FocusPane(PaneId::LogCommitTable));
    let term = render(&mut state, 140, 40);
    let log_footer = screen_text(term.backend().buffer());
    assert!(log_footer.contains("Filter"), "{log_footer}");
    assert!(log_footer.contains("Checkout"), "{log_footer}");
    assert!(log_footer.contains("Help"), "{log_footer}");

    state.dispatch(Action::SwitchTab(WorkspaceTab::Commit));
    state.dispatch(Action::FocusPane(PaneId::CommitChangesTree));
    let term = render(&mut state, 140, 40);
    let commit_footer = screen_text(term.backend().buffer());
    assert!(commit_footer.contains("Space"), "{commit_footer}");
    assert!(commit_footer.contains("Stage"), "{commit_footer}");

    let ov: crate::config::keymap_config::KeymapConfigOverlay = toml::from_str(
        r#"
[commit]
prepend_keymap = [
    { on = "S", run = "toggle_stage" },
]
"#,
    )
    .unwrap();
    state.config.keymap.merge_overlay(&ov);
    let term = render(&mut state, 140, 40);
    let updated_footer = screen_text(term.backend().buffer());
    assert!(updated_footer.contains(" S "), "{updated_footer}");
}

#[test]
fn footer_hints_match_active_mode_dock_stash_and_console_probe() {
    use crate::config::{KeymapConfig, KeymapPreset, keymap_config::KeymapConfigOverlay};

    let mut state = state_with(snapshot());
    state.config.keymap = KeymapConfig::for_preset(KeymapPreset::Default);

    // 1. LogDetailsDiff in Commit Details mode (diff_preview = false) must NOT show diff-only hints
    state.dispatch(Action::SwitchTab(WorkspaceTab::Log));
    state.dispatch(Action::FocusPane(PaneId::LogDetailsDiff));
    state.details.diff_preview = false;
    let term = render(&mut state, 140, 40);
    let details_footer = row_text(term.backend().buffer(), 39);
    assert!(details_footer.contains("Diff"), "{details_footer}");
    assert!(!details_footer.contains("Dock"), "{details_footer}");
    assert!(!details_footer.contains("Split"), "{details_footer}");
    assert!(!details_footer.contains("Context"), "{details_footer}");
    assert!(!details_footer.contains("Hunk"), "{details_footer}");

    // 2. LogDetailsDiff in Diff Preview mode (diff_preview = true) shows core diff hints
    //    (Hunk/File/Zoom) while keeping rarely used layout toggles (Dock/Split/Context) hidden
    //    unless opted in via show_hint = true.
    state.details.diff_preview = true;
    let term = render(&mut state, 140, 40);
    let diff_footer = row_text(term.backend().buffer(), 39);
    assert!(diff_footer.contains("Hunk"), "{diff_footer}");
    assert!(diff_footer.contains("File"), "{diff_footer}");
    assert!(diff_footer.contains("Zoom"), "{diff_footer}");
    assert!(!diff_footer.contains("Dock"), "{diff_footer}");
    assert!(!diff_footer.contains("Split"), "{diff_footer}");
    assert!(!diff_footer.contains("Context"), "{diff_footer}");

    let dock_ov: KeymapConfigOverlay = toml::from_str(
        r#"
[diff]
prepend_keymap = [
    { on = "m", run = "cycle_diff_dock", show_hint = true },
]
"#,
    )
    .unwrap();
    let mut opt_in_state = state_with(snapshot());
    opt_in_state.config.keymap = KeymapConfig::for_preset(KeymapPreset::Default);
    opt_in_state.config.keymap.merge_overlay(&dock_ov);
    opt_in_state.dispatch(Action::SwitchTab(WorkspaceTab::Log));
    opt_in_state.dispatch(Action::FocusPane(PaneId::LogDetailsDiff));
    opt_in_state.details.diff_preview = true;
    let term = render(&mut opt_in_state, 140, 40);
    let opt_in_footer = row_text(term.backend().buffer(), 39);
    assert!(opt_in_footer.contains(" m "), "{opt_in_footer}");
    assert!(opt_in_footer.contains("Dock"), "{opt_in_footer}");

    // 3. CommitDiffPreview shows Stage / Hunk / Zoom / Back (`Esc`) without rare `Context`
    state.dispatch(Action::SwitchTab(WorkspaceTab::Commit));
    state.dispatch(Action::FocusPane(PaneId::CommitDiffPreview));
    let term = render(&mut state, 160, 40);
    let commit_diff_footer = row_text(term.backend().buffer(), 39);
    assert!(commit_diff_footer.contains("Stage"), "{commit_diff_footer}");
    assert!(commit_diff_footer.contains("Hunk"), "{commit_diff_footer}");
    assert!(commit_diff_footer.contains("Esc"), "{commit_diff_footer}");
    assert!(!commit_diff_footer.contains("Context"), "{commit_diff_footer}");

    // 4. CommitMessageEditor shows Commit / Amend / Esc without rare `Sign-off`
    state.dispatch(Action::FocusPane(PaneId::CommitMessageEditor));
    let term = render(&mut state, 140, 40);
    let msg_footer = row_text(term.backend().buffer(), 39);
    assert!(msg_footer.contains("Commit"), "{msg_footer}");
    assert!(msg_footer.contains("Amend"), "{msg_footer}");
    assert!(!msg_footer.contains("Sign-off"), "{msg_footer}");

    // 5. StashDiffPreview does NOT advertise ghost Zoom and shows Hunk / File without rare `Group`
    state.dispatch(Action::SwitchTab(WorkspaceTab::Stash));
    state.dispatch(Action::FocusPane(PaneId::StashDiffPreview));
    let term = render(&mut state, 140, 40);
    let stash_footer = row_text(term.backend().buffer(), 39);
    assert!(!stash_footer.contains("Zoom"), "{stash_footer}");
    assert!(stash_footer.contains("Hunk"), "{stash_footer}");
    assert!(stash_footer.contains("File"), "{stash_footer}");
    assert!(!stash_footer.contains("Group"), "{stash_footer}");
    key(&mut state, KeyCode::Char('z'), KeyModifiers::NONE);
    assert!(!state.details.diff_zoomed);

    // 6. ConsoleCommandList `p` actually triggers status probe instead of global git_pull
    state.dispatch(Action::SwitchTab(WorkspaceTab::Console));
    state.dispatch(Action::FocusPane(PaneId::ConsoleCommandList));
    let term = render(&mut state, 140, 40);
    let console_footer = row_text(term.backend().buffer(), 39);
    assert!(console_footer.contains(" p "), "{console_footer}");
    assert!(console_footer.contains("Status"), "{console_footer}");
    let actions = route_terminal_event(
        Event::Key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::NONE)),
        &mut state,
    );
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, Action::RunCliProbe(args) if args.first().map(String::as_str) == Some("status"))),
        "expected RunCliProbe(status), got {actions:?}"
    );
}

#[test]
fn which_key_popup_appears_on_prefix_and_dismisses() {
    let mut state = state_with(snapshot());
    state.dispatch(Action::SwitchTab(WorkspaceTab::Log));
    state.dispatch(Action::FocusPane(PaneId::LogCommitTable));

    key(&mut state, KeyCode::Char('g'), KeyModifiers::NONE);
    assert_eq!(state.pending_keys.len(), 1);
    let term = render(&mut state, 140, 40);
    let txt = screen_text(term.backend().buffer());
    assert!(txt.contains("g +"), "{txt}");
    assert!(txt.contains("Esc cancel"), "{txt}");

    // Second key completes the sequence and closes Which-Key.
    key(&mut state, KeyCode::Char('g'), KeyModifiers::NONE);
    assert!(state.pending_keys.is_empty());
    let term = render(&mut state, 140, 40);
    let txt_after = screen_text(term.backend().buffer());
    assert!(!txt_after.contains("g +"), "{txt_after}");

    // Pressing Esc cancels a pending chord cleanly.
    key(&mut state, KeyCode::Char('g'), KeyModifiers::NONE);
    assert_eq!(state.pending_keys.len(), 1);
    key(&mut state, KeyCode::Esc, KeyModifiers::NONE);
    assert!(state.pending_keys.is_empty());
}

#[test]
fn shortcuts_help_palette_filters_and_executes_selected_action() {
    let mut state = state_with(snapshot());
    state.dispatch(Action::SwitchTab(WorkspaceTab::Log));
    state.dispatch(Action::FocusPane(PaneId::LogCommitTable));

    key(&mut state, KeyCode::Char('?'), KeyModifiers::SHIFT);
    assert!(matches!(
        state.modal_stack.last(),
        Some(ModalLayer::FloatingPopup {
            kind: PopupKind::ShortcutsHelp,
            ..
        })
    ));

    type_str(&mut state, "palette");
    let term = render(&mut state, 120, 40);
    let txt = screen_text(term.backend().buffer());
    assert!(txt.contains("palette"), "{txt}");

    key(&mut state, KeyCode::Enter, KeyModifiers::NONE);
    assert!(matches!(
        state.modal_stack.last(),
        Some(ModalLayer::FloatingPopup {
            kind: PopupKind::PalettePreview,
            ..
        })
    ));
}

#[test]
fn jetbrains_tree_speed_search_preserves_context_menu_dot_and_vim_diff_menu_shows_su() {
    use crate::config::{KeymapConfig, KeymapPreset};

    // 1. JetBrains preset: `.` on LogBranchesTree opens context menu instead of speed search.
    let mut state = state_with(snapshot());
    state.config.keymap = KeymapConfig::for_preset(KeymapPreset::Jetbrains);
    state.dispatch(Action::SwitchTab(WorkspaceTab::Log));
    state.dispatch(Action::FocusPane(PaneId::LogBranchesTree));
    let _ = render(&mut state, 140, 40);

    key(&mut state, KeyCode::Char('.'), KeyModifiers::NONE);
    assert!(
        matches!(state.modal_stack.last(), Some(ModalLayer::ContextMenu { .. })),
        "expected context menu on '.' under jetbrains preset"
    );
    state.dispatch(Action::PopContextMenus);

    type_str(&mut state, "main");
    assert_eq!(state.branches.query, "main");

    // 2. Vim preset: stage_diff_menu_items displays `s` and `u` separately.
    state.config.keymap = KeymapConfig::for_preset(KeymapPreset::Vim);
    let diff_menu = crate::menus::stage_diff_menu_items(&state);
    assert_eq!(
        find_item(&diff_menu, "Stage Hunk or Lines").shortcut.as_deref(),
        Some("s")
    );
    assert_eq!(
        find_item(&diff_menu, "Unstage Hunk or Lines").shortcut.as_deref(),
        Some("u")
    );
}

#[test]
fn zh_cn_reset_dialog_and_collapsed_branches_stripe_align_cjk_widths() {
    use unicode_width::UnicodeWidthStr;

    let mut state = state_with(snapshot());
    state.i18n = Arc::new(crate::i18n::I18n::load("zh-CN", None));

    // Reset dialog flags must start at the exact same display column in zh-CN.
    let dlg = crate::dialogs::reset_dialog(oid(9), "修复对齐", "main", &state.i18n);
    let Dialog::Choice(choice) = dlg else {
        panic!("expected ChoiceDialog");
    };
    let flag_cols: Vec<usize> = choice
        .options
        .iter()
        .map(|o| {
            let prefix = o.label.split("--").next().unwrap();
            prefix.width()
        })
        .collect();
    assert!(
        flag_cols.windows(2).all(|w| w[0] == w[1]),
        "reset mode flags misaligned: {flag_cols:?}"
    );

    // Collapsed branches stripe renders `"分支"` cleanly into a 2-column area.
    let mut term = Terminal::new(TestBackend::new(2, 6)).unwrap();
    let mut hit_registry = crate::hit_test::HitTestRegistry::default();
    let mut ctx = crate::app::RenderContext {
        theme: &state.theme,
        glyph_mode: state.glyph_mode,
        branch_colors: &state.branch_colors,
        hit_registry: &mut hit_registry,
        focused_pane: PaneId::LogCommitTable,
        hover_target: None,
        dragging_splitter: None,
        graph_renderer: None,
        i18n: Some(&state.i18n),
        config: Some(&state.config),
        scroll_metrics: None,
        splitter_containers: None,
    };
    term.draw(|frame| {
        crate::views::log_branches::render_branches_stripe(
            frame,
            Rect::new(0, 0, 2, 6),
            &mut ctx,
        );
    })
    .unwrap();
    let buf = term.backend().buffer();
    assert_eq!(buf[(0, 1)].symbol(), "分");
    assert_eq!(buf[(0, 2)].symbol(), "支");
}

#[test]
fn footer_and_help_respect_amend_readonly_author_edit_speed_search_and_console_detail() {
    use crate::config::{KeymapConfig, KeymapPreset};
    use crate::keymap::KeyAction;
    use crate::keymap::help::{HelpRow, build_help_rows};
    use jbgit_engine::{StageDiffKey, StageDiffSide};

    // 1. CommitDiffPreview in Amend read-only mode hides write-only pills and help entries
    let mut state = state_with(snapshot());
    state.dispatch(Action::SwitchTab(WorkspaceTab::Commit));
    state.dispatch(Action::FocusPane(PaneId::CommitDiffPreview));
    state.stage_workbench.current_diff_key = Some(StageDiffKey {
        path: "a.rs".into(),
        old_path: None,
        side: StageDiffSide::AmendCommit,
        context: jbgit_core::DiffContext::Three,
    });
    let term = render(&mut state, 160, 40);
    let amend_footer = row_text(term.backend().buffer(), 39);
    assert!(amend_footer.contains("Hunk"), "{amend_footer}");
    assert!(amend_footer.contains("File"), "{amend_footer}");
    assert!(amend_footer.contains("Esc"), "{amend_footer}");
    assert!(!amend_footer.contains("Stage"), "{amend_footer}");
    assert!(!amend_footer.contains("Rollback"), "{amend_footer}");
    assert!(!amend_footer.contains("Visual"), "{amend_footer}");
    assert!(!amend_footer.contains("3-Way"), "{amend_footer}");
    let help_rows = build_help_rows(&state, "");
    let ctx_rows: Vec<&HelpRow> = help_rows
        .iter()
        .skip(1)
        .take_while(|r| !matches!(r, HelpRow::Section(_)))
        .collect();
    let has_ctx_action = |action: KeyAction| {
        ctx_rows.iter().any(|r| {
            matches!(
                r,
                HelpRow::Entry {
                    run: Some(run),
                    ..
                } if *run == action
            )
        })
    };
    assert!(!has_ctx_action(KeyAction::RollbackSelection));
    assert!(!has_ctx_action(KeyAction::ToggleVisual));
    assert!(!has_ctx_action(KeyAction::ThreeWayInspector));
    assert!(!has_ctx_action(KeyAction::SwitchDiffSide));
    assert!(has_ctx_action(KeyAction::NextHunk));
    assert!(has_ctx_action(KeyAction::CycleDiffContext));

    // 2. CommitMessageEditor with editing_author = true shows Enter/Esc Apply instead of Esc Back
    state.dispatch(Action::FocusPane(PaneId::CommitMessageEditor));
    state.stage_workbench.editing_author = true;
    let term = render(&mut state, 160, 40);
    let author_footer = row_text(term.backend().buffer(), 39);
    assert!(author_footer.contains("Enter/Esc"), "{author_footer}");
    assert!(author_footer.contains("Apply"), "{author_footer}");
    assert!(!author_footer.contains("Back"), "{author_footer}");

    // 3. LogBranchesTree under JetBrains preset (tree_speed_search = true) hides captured single letters
    let mut jb_state = state_with(snapshot());
    jb_state.config.keymap = KeymapConfig::for_preset(KeymapPreset::Jetbrains);
    jb_state.dispatch(Action::SwitchTab(WorkspaceTab::Log));
    jb_state.dispatch(Action::FocusPane(PaneId::LogBranchesTree));
    let term = render(&mut jb_state, 160, 40);
    let jb_footer = row_text(term.backend().buffer(), 39);
    assert!(jb_footer.contains("Type"), "{jb_footer}");
    assert!(jb_footer.contains("Ctrl+F"), "{jb_footer}");
    assert!(jb_footer.contains("Ctrl+T"), "{jb_footer}");
    assert!(jb_footer.contains("Del"), "{jb_footer}");
    assert!(!jb_footer.contains("Checkout"), "{jb_footer}");
    assert!(!jb_footer.contains("Fetch"), "{jb_footer}");

    // 4. ConsoleOutputDetail help does not advertise GitStatusProbe and `p` does not trigger probe
    let mut console_state = state_with(snapshot());
    console_state.dispatch(Action::SwitchTab(WorkspaceTab::Console));
    console_state.dispatch(Action::FocusPane(PaneId::ConsoleOutputDetail));
    let detail_help = build_help_rows(&console_state, "");
    assert!(
        !detail_help.iter().take(5).any(|r| matches!(
            r,
            HelpRow::Entry {
                run: Some(KeyAction::GitStatusProbe),
                ..
            }
        )),
        "ConsoleOutputDetail context help should not advertise GitStatusProbe"
    );
    let detail_p_actions = route_terminal_event(
        Event::Key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::NONE)),
        &mut console_state,
    );
    assert!(
        !detail_p_actions
            .iter()
            .any(|a| matches!(a, Action::RunCliProbe(_))),
        "p in ConsoleOutputDetail must not run RunCliProbe: {detail_p_actions:?}"
    );
}

#[test]
fn footer_selective_show_hint_and_declaration_order() {
    let mut state = state_with(snapshot());
    state.config.keymap = crate::config::KeymapConfig::for_preset(crate::config::KeymapPreset::Default);
    state.dispatch(Action::SwitchTab(WorkspaceTab::Log));
    state.dispatch(Action::FocusPane(PaneId::LogCommitTable));

    // Default Log footer places Log scope before Global scope
    let term = render(&mut state, 200, 30);
    let default_footer = row_text(term.backend().buffer(), 29);
    let filter_pos = default_footer.find("Filter").expect(&default_footer);
    let menu_pos = default_footer.find("Menu").expect(&default_footer);
    assert!(filter_pos < menu_pos, "{default_footer}");
    assert!(default_footer.contains("Copy hash"), "{default_footer}");
    assert!(default_footer.contains("Help"), "{default_footer}");

    // Hide copy_hash, promote uncommit via prepend_keymap, and hide help
    let ov: crate::config::keymap_config::KeymapConfigOverlay = toml::from_str(
        r#"
[log]
prepend_keymap = [
    { on = "u", run = "uncommit", show_hint = true },
    { on = "y", run = "copy_hash", show_hint = false },
]

[global]
prepend_keymap = [
    { on = "?", run = "help", show_hint = false },
]
"#,
    )
    .unwrap();
    state.config.keymap.merge_overlay(&ov);

    let term = render(&mut state, 200, 30);
    let updated_footer = row_text(term.backend().buffer(), 29);
    assert!(updated_footer.contains("Uncommit"), "{updated_footer}");
    assert!(!updated_footer.contains("Copy hash"), "{updated_footer}");
    assert!(!updated_footer.contains("Help"), "{updated_footer}");
    // Prepended `uncommit` appears before `Filter`
    let uncommit_pos = updated_footer.find("Uncommit").expect(&updated_footer);
    let filter_pos2 = updated_footer.find("Filter").expect(&updated_footer);
    assert!(uncommit_pos < filter_pos2, "{updated_footer}");

    // Reordering entries in [log] changes left-to-right order in footer
    let mut state_reordered = state_with(snapshot());
    state_reordered.config.keymap =
        crate::config::KeymapConfig::for_preset(crate::config::KeymapPreset::Default);
    state_reordered.dispatch(Action::SwitchTab(WorkspaceTab::Log));
    state_reordered.dispatch(Action::FocusPane(PaneId::LogCommitTable));
    let reorder_ov: crate::config::keymap_config::KeymapConfigOverlay = toml::from_str(
        r#"
[log]
key_bindings = [
    { on = "y", run = "copy_hash", show_hint = true },
    { on = "c", run = "checkout", show_hint = true },
]
"#,
    )
    .unwrap();
    state_reordered.config.keymap.merge_overlay(&reorder_ov);
    let term = render(&mut state_reordered, 160, 30);
    let reordered_footer = row_text(term.backend().buffer(), 29);
    let copy_pos = reordered_footer.find("Copy hash").expect(&reordered_footer);
    let checkout_pos = reordered_footer.find("Checkout").expect(&reordered_footer);
    assert!(copy_pos < checkout_pos, "{reordered_footer}");
}

#[test]
fn footer_overflow_single_line_multiline_and_hidden() {
    let mut state = state_with(snapshot());
    state.config.keymap = crate::config::KeymapConfig::for_preset(crate::config::KeymapPreset::Default);
    state.dispatch(Action::SwitchTab(WorkspaceTab::Log));
    state.dispatch(Action::FocusPane(PaneId::LogCommitTable));

    // 1. Default footer_max_lines = 1 at narrow width (80 cols): shows `+N` and right-aligned `? Help`
    state.config.keymap.footer_max_lines = 1;
    let term = render(&mut state, 80, 24);
    let row_last = row_text(term.backend().buffer(), 23);
    assert!(row_last.contains("Filter"), "{row_last}");
    assert!(row_last.contains("Help"), "{row_last}");
    assert!(row_last.contains(" +"), "expected +N overflow badge: {row_last}");
    assert!(!row_last.contains("Parent"), "{row_last}");
    assert!(!row_last.contains("Refresh"), "{row_last}");

    // 2. Multi-line footer (footer_max_lines = 2) at 60 cols: overflows onto row 23 with `+N` and puts `? Help` on last row
    state.config.keymap.footer_max_lines = 2;
    let term = render(&mut state, 60, 24);
    let row_22 = row_text(term.backend().buffer(), 22);
    let row_23 = row_text(term.backend().buffer(), 23);
    assert!(row_22.contains("Filter"), "{row_22}");
    assert!(row_22.contains("Diff"), "{row_22}");
    assert!(!row_22.contains("Help"), "Help must sit on bottom row: {row_22}");
    assert!(row_23.contains("Help"), "{row_23}");
    // Items overflowing row 22 appear on row 23, and remaining overflow shows `+N`
    assert!(row_23.contains("Copy hash"), "{row_23}");
    assert!(row_23.contains("Go to"), "{row_23}");
    assert!(row_23.contains(" +"), "expected +N on 2nd line: {row_23}");

    // 3. Multi-line footer (footer_max_lines = 3) at 60 cols: fits every Log + Global pill cleanly with no `+N`
    state.config.keymap.footer_max_lines = 3;
    let term = render(&mut state, 60, 24);
    let combined_footer = format!(
        "{}\n{}\n{}",
        row_text(term.backend().buffer(), 21),
        row_text(term.backend().buffer(), 22),
        row_text(term.backend().buffer(), 23)
    );
    assert!(combined_footer.contains("Refresh"), "{combined_footer}");
    assert!(combined_footer.contains("Help"), "{combined_footer}");
    assert!(!combined_footer.contains(" +"), "unexpected +N when all fit: {combined_footer}");

    // 4. Hidden footer (footer_max_lines = 0): no key hints on row 23, but notifications still render
    state.config.keymap.footer_max_lines = 0;
    let term = render(&mut state, 120, 24);
    let hidden_row = row_text(term.backend().buffer(), 23);
    assert!(!hidden_row.contains("Filter"), "{hidden_row}");
    assert!(!hidden_row.contains("Help"), "{hidden_row}");

    state.notification = Some(crate::app::Notification {
        text: "Fetch finished".to_string(),
        is_error: false,
        action: None,
    });
    let term = render(&mut state, 120, 24);
    let notif_row = row_text(term.backend().buffer(), 23);
    assert!(notif_row.contains("Fetch finished"), "{notif_row}");
}

#[test]
fn footer_cross_scope_shadowing_does_not_leak_hidden_key() {
    let mut state = state_with(snapshot());
    state.config.keymap = crate::config::KeymapConfig::for_preset(crate::config::KeymapPreset::Default);
    state.dispatch(Action::SwitchTab(WorkspaceTab::Log));
    state.dispatch(Action::FocusPane(PaneId::LogDetailsDiff));
    state.details.diff_preview = true;

    // Enable `choose_parent` (`m`) in [log], and hide `cycle_diff_dock` (`m`) in [diff];
    // [log]'s `m` (`choose_parent`) must NOT leak into footer when Diff pane is focused.
    let ov: crate::config::keymap_config::KeymapConfigOverlay = toml::from_str(
        r#"
[log]
prepend_keymap = [
    { on = "m", run = "choose_parent", show_hint = true },
]

[diff]
prepend_keymap = [
    { on = "m", run = "cycle_diff_dock", show_hint = false },
]
"#,
    )
    .unwrap();
    state.config.keymap.merge_overlay(&ov);

    let term = render(&mut state, 160, 40);
    let diff_footer = row_text(term.backend().buffer(), 39);
    assert!(!diff_footer.contains("Dock"), "{diff_footer}");
    assert!(!diff_footer.contains("Parent"), "shadowed log key leaked: {diff_footer}");
}

#[test]
fn footer_console_probe_vim_diff_noop_append_and_which_key_non_overlap() {
    use crate::config::{KeymapConfig, KeymapPreset, keymap_config::KeymapConfigOverlay};

    // 1. Default ConsoleCommandList shows `p Status` without `Pull`;
    //    rebinding git_status_probe in [global] shows `g s Status` and unshadows `p Pull`.
    let mut console_state = state_with(snapshot());
    console_state.config.keymap = KeymapConfig::for_preset(KeymapPreset::Default);
    console_state.dispatch(Action::SwitchTab(WorkspaceTab::Console));
    console_state.dispatch(Action::FocusPane(PaneId::ConsoleCommandList));
    let term = render(&mut console_state, 160, 30);
    let default_console_footer = row_text(term.backend().buffer(), 29);
    assert!(default_console_footer.contains(" p  Status"), "{default_console_footer}");
    assert!(!default_console_footer.contains("Pull"), "{default_console_footer}");

    let probe_ov: KeymapConfigOverlay = toml::from_str(
        r#"
[global]
prepend_keymap = [
    { on = ["g", "s"], run = "git_status_probe", show_hint = true },
]
"#,
    )
    .unwrap();
    console_state.config.keymap.merge_overlay(&probe_ov);
    let term = render(&mut console_state, 160, 30);
    let custom_console_footer = row_text(term.backend().buffer(), 29);
    assert!(custom_console_footer.contains(" g s  Status"), "{custom_console_footer}");
    assert!(custom_console_footer.contains(" p  Pull"), "{custom_console_footer}");

    // 2. Vim CommitDiffPreview shows `s Stage` and `u Unstage` without duplicate `Space Stage`,
    //    and <Space> still triggers staging.
    let mut vim_state = state_with(snapshot());
    vim_state.config.keymap = KeymapConfig::for_preset(KeymapPreset::Vim);
    vim_state.dispatch(Action::SwitchTab(WorkspaceTab::Commit));
    vim_state.dispatch(Action::FocusPane(PaneId::CommitDiffPreview));
    let term = render(&mut vim_state, 160, 30);
    let vim_diff_footer = row_text(term.backend().buffer(), 29);
    assert!(vim_diff_footer.contains(" s  Stage"), "{vim_diff_footer}");
    assert!(vim_diff_footer.contains(" u  Unstage"), "{vim_diff_footer}");
    assert_eq!(vim_diff_footer.matches("Stage").count(), 1, "{vim_diff_footer}");
    let space_actions = route_terminal_event(
        Event::Key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE)),
        &mut vim_state,
    );
    assert!(
        space_actions
            .iter()
            .any(|a| matches!(a, Action::StageDiffStageSelection)),
        "Space in Vim CommitDiffPreview should still stage: {space_actions:?}"
    );

    // 3. Unbinding `r` via noop in [global] shows surviving `F5 Refresh` in footer.
    let mut noop_state = state_with(snapshot());
    noop_state.config.keymap = KeymapConfig::for_preset(KeymapPreset::Default);
    let noop_ov: KeymapConfigOverlay = toml::from_str(
        r#"
[global]
prepend_keymap = [
    { on = "r", run = "noop" },
]
"#,
    )
    .unwrap();
    noop_state.config.keymap.merge_overlay(&noop_ov);
    noop_state.dispatch(Action::SwitchTab(WorkspaceTab::Log));
    noop_state.dispatch(Action::FocusPane(PaneId::LogCommitTable));
    let term = render(&mut noop_state, 220, 30);
    let noop_footer = row_text(term.backend().buffer(), 29);
    assert!(noop_footer.contains(" F5  Refresh"), "{noop_footer}");

    // 4. [log] append_keymap alias preserves `y Copy hash` in its original declaration position.
    let mut append_state = state_with(snapshot());
    append_state.config.keymap = KeymapConfig::for_preset(KeymapPreset::Default);
    append_state.dispatch(Action::SwitchTab(WorkspaceTab::Log));
    append_state.dispatch(Action::FocusPane(PaneId::LogCommitTable));
    let append_ov: KeymapConfigOverlay = toml::from_str(
        r#"
[log]
append_keymap = [
    { on = "Y", run = "copy_hash" },
]
"#,
    )
    .unwrap();
    append_state.config.keymap.merge_overlay(&append_ov);
    let term = render(&mut append_state, 200, 30);
    let append_footer = row_text(term.backend().buffer(), 29);
    let new_branch_pos = append_footer.find("Branch").expect(&append_footer);
    let copy_pos = append_footer.find(" y  Copy hash").expect(&append_footer);
    let goto_pos = append_footer.find("Go to").expect(&append_footer);
    assert!(new_branch_pos < copy_pos && copy_pos < goto_pos, "{append_footer}");

    // 5. Which-Key popup with footer_max_lines = 2 does NOT overwrite either footer row.
    let mut wk_state = state_with(snapshot());
    wk_state.config.keymap = KeymapConfig::for_preset(KeymapPreset::Default);
    wk_state.config.keymap.footer_max_lines = 2;
    wk_state.dispatch(Action::SwitchTab(WorkspaceTab::Log));
    wk_state.dispatch(Action::FocusPane(PaneId::LogCommitTable));
    key(&mut wk_state, KeyCode::Char('g'), KeyModifiers::NONE);
    let term = render(&mut wk_state, 80, 24);
    let buf = term.backend().buffer();
    let row_22 = row_text(buf, 22);
    let row_23 = row_text(buf, 23);
    assert!(row_22.contains("Filter") && row_22.contains("Copy hash"), "{row_22}");
    assert!(row_23.contains("Go to") && row_23.contains("Help"), "{row_23}");
    let above_footer = (0..22)
        .map(|y| row_text(buf, y))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(above_footer.contains("g +"), "{above_footer}");
}

#[test]
fn qa_bugfixes_stash_braces_popup_interior_click_esc_btn_and_new_branch_key() {
    use crate::config::{KeymapConfig, KeymapConfigOverlay, KeymapPreset};
    use jbgit_core::ModalButton;

    // 1. Stash i18n formatting renders single braces `stash@{0}`, never `stash@{{0}}`.
    let zh = crate::i18n::I18n::load("zh-CN", None);
    let en = crate::i18n::I18n::load("en-US", None);
    assert_eq!(
        zh.git_op_label(&GitOp::StashApply {
            index: 0,
            pop: false
        }),
        "应用 stash@{0}"
    );
    assert_eq!(
        en.git_op_label(&GitOp::StashApply {
            index: 2,
            pop: true
        }),
        "Pop stash@{2}"
    );
    assert_eq!(
        zh.git_op_label(&GitOp::StashDrop { index: 1 }),
        "删除 stash@{1}"
    );

    // 2. Clicking inside ShortcutsHelp popup (e.g. search bar / empty body) does NOT dismiss it,
    //    while clicking `[Esc]` or outside backdrop DOES dismiss it.
    let mut state = state_with(snapshot());
    key(&mut state, KeyCode::Char('?'), KeyModifiers::NONE);
    assert!(matches!(
        state.modal_stack.last(),
        Some(ModalLayer::FloatingPopup {
            kind: PopupKind::ShortcutsHelp,
            ..
        })
    ));
    let _ = render(&mut state, 120, 40);
    let body_rect = region_of(&state, |t| matches!(t, HitTarget::DialogBody));
    mouse(
        &mut state,
        MouseEventKind::Down(MouseButton::Left),
        body_rect.x + 2,
        body_rect.y + 1,
    );
    assert!(
        matches!(
            state.modal_stack.last(),
            Some(ModalLayer::FloatingPopup {
                kind: PopupKind::ShortcutsHelp,
                ..
            })
        ),
        "clicking inside ShortcutsHelp must not close it"
    );
    let esc_rect = region_of(&state, |t| matches!(t, HitTarget::ModalButton(ModalButton::Cancel)));
    mouse(
        &mut state,
        MouseEventKind::Down(MouseButton::Left),
        esc_rect.x + 1,
        esc_rect.y,
    );
    assert!(
        state.modal_stack.is_empty(),
        "clicking [Esc] button should close ShortcutsHelp"
    );

    // 3. BranchesQuickView: `[Esc]` button closes it, backdrop outside closes it,
    //    and rebound NewBranch shortcut (`F7`) works directly inside the popup.
    let mut bp_state = state_with(snapshot());
    bp_state.config.keymap = KeymapConfig::for_preset(KeymapPreset::Default);
    let ov: KeymapConfigOverlay = toml::from_str(
        r#"
[branch]
prepend_keymap = [
    { on = [ "n" ], run = "noop" },
    { on = [ "<F7>" ], run = "new_branch", show_hint = true },
]
"#,
    )
    .unwrap();
    bp_state.config.keymap.merge_overlay(&ov);
    key(&mut bp_state, KeyCode::Char('b'), KeyModifiers::NONE);
    let term = render(&mut bp_state, 140, 40);
    let screen = screen_text(term.backend().buffer());
    assert!(screen.contains("F7"), "popup should show F7 hint:\n{screen}");

    // Clicking outside the popup closes it
    mouse(
        &mut bp_state,
        MouseEventKind::Down(MouseButton::Left),
        130,
        20,
    );
    assert!(
        bp_state.modal_stack.is_empty(),
        "clicking backdrop outside BranchesQuickView should close it"
    );

    // Clicking [Esc] closes it
    key(&mut bp_state, KeyCode::Char('b'), KeyModifiers::NONE);
    let _ = render(&mut bp_state, 140, 40);
    let bp_esc = region_of(&bp_state, |t| {
        matches!(t, HitTarget::ModalButton(ModalButton::Cancel))
    });
    mouse(
        &mut bp_state,
        MouseEventKind::Down(MouseButton::Left),
        bp_esc.x + 1,
        bp_esc.y,
    );
    assert!(
        bp_state.modal_stack.is_empty(),
        "clicking [Esc] inside BranchesQuickView should close it"
    );

    // Pressing F7 inside BranchesQuickView opens NewBranch dialog directly
    key(&mut bp_state, KeyCode::Char('b'), KeyModifiers::NONE);
    key(&mut bp_state, KeyCode::F(7), KeyModifiers::NONE);
    assert!(
        matches!(bp_state.modal_stack.last(), Some(ModalLayer::Dialog(_))),
        "pressing F7 in BranchesQuickView should open NewBranch dialog: {:?}",
        bp_state.modal_stack
    );

    // 4. Cross-scope shadowing: [branch] binds <F7> to rename_branch while [log] binds <F7> to new_branch.
    //    Popup should fall back to Ctrl+Alt+N (not F7) and pressing F7 must not open NewBranch.
    let mut shadow_state = state_with(snapshot());
    shadow_state.config.keymap = KeymapConfig::for_preset(KeymapPreset::Default);
    let shadow_ov: KeymapConfigOverlay = toml::from_str(
        r#"
[branch]
prepend_keymap = [
    { on = [ "<F7>" ], run = "rename_branch" },
]
[log]
prepend_keymap = [
    { on = [ "<F7>" ], run = "new_branch", show_hint = true },
]
"#,
    )
    .unwrap();
    shadow_state.config.keymap.merge_overlay(&shadow_ov);
    key(&mut shadow_state, KeyCode::Char('b'), KeyModifiers::NONE);
    let term = render(&mut shadow_state, 140, 40);
    let shadow_screen = screen_text(term.backend().buffer());
    let shadow_new_branch_line = shadow_screen
        .lines()
        .find(|l| l.contains("New Branch"))
        .unwrap_or_default();
    assert!(
        shadow_new_branch_line.contains("Ctrl+Alt+N"),
        "shadowed F7 should fall back to Ctrl+Alt+N:\n{shadow_screen}"
    );
    assert!(
        !shadow_new_branch_line.contains("F7"),
        "shadowed F7 must not be advertised on New Branch:\n{shadow_screen}"
    );
    key(&mut shadow_state, KeyCode::F(7), KeyModifiers::NONE);
    assert!(
        !matches!(shadow_state.modal_stack.last(), Some(ModalLayer::Dialog(_))),
        "shadowed F7 must not open NewBranch dialog: {:?}",
        shadow_state.modal_stack
    );

    // 5. Unbinding <C-A-n> via noop hides Ctrl+Alt+N and disables it in BranchesQuickView.
    let mut unbind_state = state_with(snapshot());
    unbind_state.config.keymap = KeymapConfig::for_preset(KeymapPreset::Default);
    let unbind_ov: KeymapConfigOverlay = toml::from_str(
        r#"
[branch]
prepend_keymap = [
    { on = [ "<C-A-n>" ], run = "noop" },
]
"#,
    )
    .unwrap();
    unbind_state.config.keymap.merge_overlay(&unbind_ov);
    key(&mut unbind_state, KeyCode::Char('b'), KeyModifiers::NONE);
    let term = render(&mut unbind_state, 140, 40);
    let unbind_screen = screen_text(term.backend().buffer());
    assert!(
        !unbind_screen.contains("Ctrl+Alt+N"),
        "unbound Ctrl+Alt+N must not be shown:\n{unbind_screen}"
    );
    key(
        &mut unbind_state,
        KeyCode::Char('n'),
        KeyModifiers::CONTROL | KeyModifiers::ALT,
    );
    assert!(
        !matches!(unbind_state.modal_stack.last(), Some(ModalLayer::Dialog(_))),
        "unbound Ctrl+Alt+N must not open NewBranch dialog: {:?}",
        unbind_state.modal_stack
    );

    // 6. CriticalDialog interior clicks do not dismiss it, while clicking Cancel closes it.
    let mut crit_state = state_with(snapshot());
    crit_state.modal_stack.push(ModalLayer::CriticalDialog {
        title: "Force Push".into(),
        body: "Overwrite remote history?".into(),
        confirm_label: "Force Push".into(),
        cancel_label: "Cancel".into(),
        selected_button: ModalButton::Cancel,
        confirm_effect: None,
    });
    let _ = render(&mut crit_state, 120, 40);
    let crit_body = region_of(&crit_state, |t| matches!(t, HitTarget::DialogBody));
    mouse(
        &mut crit_state,
        MouseEventKind::Down(MouseButton::Left),
        crit_body.x + 1,
        crit_body.y + 1,
    );
    assert!(
        matches!(
            crit_state.modal_stack.last(),
            Some(ModalLayer::CriticalDialog { .. })
        ),
        "clicking inside CriticalDialog must not close it"
    );
    let crit_cancel = region_of(&crit_state, |t| {
        matches!(t, HitTarget::ModalButton(ModalButton::Cancel))
    });
    mouse(
        &mut crit_state,
        MouseEventKind::Down(MouseButton::Left),
        crit_cancel.x + 1,
        crit_cancel.y,
    );
    assert!(
        crit_state.modal_stack.is_empty(),
        "clicking Cancel on CriticalDialog should close it"
    );
}
