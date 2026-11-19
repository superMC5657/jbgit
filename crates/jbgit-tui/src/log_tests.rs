//! Acceptance tests for Spec 03 (three-pane Log workbench), driven through
//! `AppState::dispatch` / `apply_engine_event` and a `TestBackend` render.

use std::sync::Arc;

use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use jbgit_core::{
    CommitChangesBundle, CommitDetails, CommitSummary, DetailsKey, DiffContext, DiffHunk, DiffLine,
    DiffLineKind, FileChange, FileDiff, FileDiffKey, FileStatusKind, Oid, PaneId, PersonStamp,
    RefInfo, RefKind, RepoSnapshot, SplitterId, WorkspaceTab, apply_word_diff,
};
use jbgit_engine::EngineEvent;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;

use crate::app::{Action, AppState, Effect, ModalLayer, PopupKind};
use crate::hit_test::{HitTarget, StageToolbarButton};
use crate::input::route_terminal_event;
use crate::log_view::ChangesExpandState;
use crate::views::render_app;

fn oid(n: u8) -> Oid {
    Oid::from_bytes([n; 20])
}

const A1: u8 = 1;
const A2: u8 = 2;
const L1: u8 = 3;
const O1: u8 = 4;
const B1: u8 = 5;
const M: u8 = 6;

fn commit(n: u8, parents: &[u8], ts: i64, subject: &str) -> CommitSummary {
    CommitSummary {
        oid: oid(n),
        parents: parents.iter().map(|&p| oid(p)).collect(),
        author_name: if n.is_multiple_of(2) {
            "Alice".into()
        } else {
            "Bob".into()
        },
        author_email: if n.is_multiple_of(2) {
            "alice@x.io".into()
        } else {
            "bob@x.io".into()
        },
        timestamp: ts,
        author_timestamp: ts,
        subject: subject.to_string(),
        body: String::new(),
    }
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

/// a1 ← a2 ← b1 (bugfix/ui); a1 ← l1 (feature/auth/login); a1 ← o1 (feature/auth/oauth);
/// m = merge(a2, o1) on main (HEAD, ↑2 ↓1 vs origin/main = a2).
fn fixture_state() -> AppState {
    let mut main = local("main", M);
    main.is_head = true;
    main.upstream = Some("origin/main".into());
    main.ahead_behind = Some((2, 1));
    let snap = RepoSnapshot {
        repo_name: "demo".into(),
        head_branch: Some("main".into()),
        head_oid: Some(oid(M)),
        refs: vec![
            main,
            local("feature/auth/login", L1),
            local("feature/auth/oauth", O1),
            local("bugfix/ui", B1),
            RefInfo {
                name: "origin/main".into(),
                full_name: "refs/remotes/origin/main".into(),
                target_oid: oid(A2),
                kind: RefKind::RemoteBranch,
                upstream: None,
                ahead_behind: None,
                is_head: false,
            },
        ],
        commits: vec![
            commit(M, &[A2, O1], 60, "Merge oauth flow"),
            commit(B1, &[A2], 50, "fix ui glitch"),
            commit(L1, &[A1], 40, "login form"),
            commit(O1, &[A1], 30, "oauth flow"),
            commit(A2, &[A1], 20, "main work"),
            commit(A1, &[], 10, "initial"),
        ],
        user_name: Some("Alice".into()),
        user_email: Some("alice@x.io".into()),
        ..Default::default()
    };
    let mut state = AppState::new(".".into(), None);
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

fn render(state: &mut AppState, w: u16, h: u16) -> Terminal<TestBackend> {
    let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
    terminal.draw(|f| render_app(f, state)).unwrap();
    terminal
}

fn screen_text(buf: &Buffer) -> Vec<String> {
    (0..buf.area.height)
        .map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect())
        .collect()
}

fn selected_oid(state: &AppState) -> Oid {
    state
        .log_commit_at(state.log_graph.selected_idx)
        .map(|c| c.oid)
        .unwrap()
}

fn row_of(state: &AppState, n: u8) -> usize {
    state
        .log_graph
        .cached
        .as_ref()
        .unwrap()
        .row_of_oid(&oid(n))
        .unwrap()
}

fn bundle_for(key: DetailsKey, changes: Vec<FileChange>) -> Arc<CommitChangesBundle> {
    let details = key
        .oids
        .iter()
        .map(|o| CommitDetails {
            oid: *o,
            parents: if *o == oid(M) {
                vec![oid(A2), oid(O1)]
            } else {
                vec![oid(A1)]
            },
            author: PersonStamp {
                name: "Alice".into(),
                email: "alice@x.io".into(),
                time: 60,
                offset_minutes: 0,
            },
            committer: PersonStamp {
                name: "Alice".into(),
                email: "alice@x.io".into(),
                time: 60,
                offset_minutes: 0,
            },
            message: "Merge oauth flow\n\nCloses #42\n\nSigned-off-by: Alice <alice@x.io>".into(),
            signature: Default::default(),
        })
        .collect();
    Arc::new(CommitChangesBundle {
        key,
        details,
        changes,
    })
}

fn change(path: &str, kind: FileStatusKind) -> FileChange {
    FileChange {
        path: path.into(),
        old_path: None,
        kind,
        additions: 3,
        deletions: 1,
        is_binary: false,
    }
}

#[test]
fn folder_enter_filters_all_child_branches_and_esc_restores() {
    let mut state = fixture_state();
    let _ = render(&mut state, 140, 40);
    let folder_row = state
        .branches
        .rows
        .iter()
        .position(|r| state.branches.model.nodes[r.node].key == "local:feature/auth")
        .expect("feature/auth folder row");
    state.dispatch(Action::FocusPane(PaneId::LogBranchesTree));
    state.dispatch(Action::SelectBranchRow(folder_row));
    key(&mut state, KeyCode::Enter, KeyModifiers::NONE);

    // login + oauth tips and their shared ancestor only.
    assert_eq!(state.log_graph.visible_count(), 3);
    let mut include = state.log_filter.spec.branch.include.clone();
    include.sort();
    assert_eq!(include, vec!["feature/auth/login", "feature/auth/oauth"]);

    // Esc on the tree (no speed-search query) clears the branch filter.
    key(&mut state, KeyCode::Esc, KeyModifiers::NONE);
    assert!(state.log_filter.spec.branch.is_empty());
    assert_eq!(state.log_graph.visible_count(), 6);

    // Ctrl+Click union: bugfix/ui + main.
    let _ = render(&mut state, 140, 40);
    let idx_of = |state: &AppState, key: &str| {
        state
            .branches
            .rows
            .iter()
            .position(|r| state.branches.model.nodes[r.node].key == key)
            .unwrap()
    };
    let bugfix = idx_of(&state, "refs/heads/bugfix/ui");
    let main = idx_of(&state, "refs/heads/main");
    state.dispatch(Action::SelectBranchRow(bugfix));
    state.dispatch(Action::BranchesToggleMulti(main));
    state.dispatch(Action::BranchesApplyFilter);
    // bugfix/ui (b1, a2, a1) ∪ main (m, a2, o1, a1) = 5 commits.
    assert_eq!(state.log_graph.visible_count(), 5);
}

#[test]
fn ahead_behind_badges_and_narrow_terminal_collapse() {
    let mut state = fixture_state();
    let terminal = render(&mut state, 140, 30);
    let text = screen_text(terminal.backend().buffer()).join("\n");
    assert!(text.contains("HEAD (main)"), "{text}");
    assert!(text.contains("↑2 ↓1"), "{text}");
    assert!(
        state
            .splitter_containers
            .contains_key(&SplitterId::LogLeftHorizontal)
    );

    // < 110 columns: Branches pane folds into the `»` stripe.
    let terminal = render(&mut state, 100, 30);
    assert!(
        !state
            .splitter_containers
            .contains_key(&SplitterId::LogLeftHorizontal)
    );
    let lines = screen_text(terminal.backend().buffer());
    assert!(lines[2].starts_with('»'), "{}", lines[2]);
    assert!(state.branches.is_collapsed(100));

    // Widening restores it; Alt+B collapses it explicitly.
    let _ = render(&mut state, 140, 30);
    assert!(
        state
            .splitter_containers
            .contains_key(&SplitterId::LogLeftHorizontal)
    );
    key(&mut state, KeyCode::Char('b'), KeyModifiers::ALT);
    let _ = render(&mut state, 140, 30);
    assert!(
        !state
            .splitter_containers
            .contains_key(&SplitterId::LogLeftHorizontal)
    );
}

#[test]
fn text_regex_filter_hides_rows_and_speed_search_finds_branches() {
    let mut state = fixture_state();
    let _ = render(&mut state, 140, 30);
    key(&mut state, KeyCode::Char('/'), KeyModifiers::NONE);
    assert!(state.log_filter.editing_text);
    for c in "OAUTH".chars() {
        key(&mut state, KeyCode::Char(c), KeyModifiers::NONE);
    }
    // 'q' while typing must not quit.
    assert!(!state.should_quit);
    assert_eq!(state.log_graph.visible_count(), 2);
    // Match case on → no hits.
    key(&mut state, KeyCode::Char('c'), KeyModifiers::ALT);
    assert_eq!(state.log_graph.visible_count(), 0);
    key(&mut state, KeyCode::Char('c'), KeyModifiers::ALT);
    key(&mut state, KeyCode::Char('u'), KeyModifiers::CONTROL);
    key(&mut state, KeyCode::Char('r'), KeyModifiers::ALT);
    for c in "^(fix|login)".chars() {
        key(&mut state, KeyCode::Char(c), KeyModifiers::NONE);
    }
    assert_eq!(state.log_graph.visible_count(), 2);
    key(&mut state, KeyCode::Esc, KeyModifiers::NONE);
    assert!(!state.log_filter.editing_text);

    // Branch search (`tree_speed_search = false` by default): `j`/`k` navigate without
    // triggering search, while `/` starts explicit search filtering.
    state.dispatch(Action::FocusPane(PaneId::LogBranchesTree));
    let before_row = state.branches.selected;
    key(&mut state, KeyCode::Char('j'), KeyModifiers::NONE);
    assert!(!state.branches.searching);
    assert!(state.branches.query.is_empty());
    assert!(state.branches.selected >= before_row);

    key(&mut state, KeyCode::Char('/'), KeyModifiers::NONE);
    assert!(state.branches.searching);
    for c in "oau".chars() {
        key(&mut state, KeyCode::Char(c), KeyModifiers::NONE);
    }
    let leaves: Vec<_> = state
        .branches
        .rows
        .iter()
        .filter(|r| r.expanded.is_none())
        .map(|r| state.branches.model.nodes[r.node].label.clone())
        .collect();
    assert_eq!(leaves, vec!["oauth"]);
    key(&mut state, KeyCode::Esc, KeyModifiers::NONE);
    assert!(state.branches.query.is_empty());
    assert!(!state.branches.searching);

    // Enabling `tree_speed_search = true` restores direct typing speed search.
    state.config.keymap.tree_speed_search = true;
    for c in "oau".chars() {
        key(&mut state, KeyCode::Char(c), KeyModifiers::NONE);
    }
    let leaves_ss: Vec<_> = state
        .branches
        .rows
        .iter()
        .filter(|r| r.expanded.is_none())
        .map(|r| state.branches.model.nodes[r.node].label.clone())
        .collect();
    assert_eq!(leaves_ss, vec!["oauth"]);
    key(&mut state, KeyCode::Esc, KeyModifiers::NONE);
    assert!(state.branches.query.is_empty());
}

#[test]
fn parent_child_navigation_history_and_goto() {
    let mut state = fixture_state();
    let _ = render(&mut state, 140, 30);
    assert_eq!(selected_oid(&state), oid(M));

    key(&mut state, KeyCode::Right, KeyModifiers::NONE);
    assert_eq!(
        selected_oid(&state),
        oid(A2),
        "→ jumps to parent[0] over unrelated rows"
    );
    key(&mut state, KeyCode::Left, KeyModifiers::NONE);
    let child = selected_oid(&state);
    assert!(child == oid(M) || child == oid(B1));

    state.dispatch(Action::SelectLogRow(row_of(&state, M)));
    key(&mut state, KeyCode::Right, KeyModifiers::SHIFT);
    assert_eq!(
        selected_oid(&state),
        oid(O1),
        "Shift+→ jumps to the second parent"
    );

    key(&mut state, KeyCode::Left, KeyModifiers::ALT);
    assert_eq!(selected_oid(&state), oid(M));
    key(&mut state, KeyCode::Right, KeyModifiers::ALT);
    assert_eq!(selected_oid(&state), oid(O1));

    // Ctrl+G opens Go-to; rendering the floating popup must not panic at any terminal size,
    // and Enter resolves refs and HEAD~N.
    key(&mut state, KeyCode::Char('g'), KeyModifiers::CONTROL);
    assert!(matches!(
        state.modal_stack.last(),
        Some(ModalLayer::FloatingPopup {
            kind: PopupKind::GoToRevision,
            ..
        })
    ));
    let _ = render(&mut state, 140, 40);
    let _ = render(&mut state, 20, 6);
    for c in "HEAD~1".chars() {
        key(&mut state, KeyCode::Char(c), KeyModifiers::NONE);
    }
    let _ = render(&mut state, 140, 40);
    key(&mut state, KeyCode::Enter, KeyModifiers::NONE);
    assert!(state.modal_stack.is_empty());
    assert_eq!(selected_oid(&state), oid(A2));
    state.dispatch(Action::GoToRevision("feature/auth/login".into()));
    assert_eq!(selected_oid(&state), oid(L1));
}

#[test]
fn debounced_details_lru_hit_and_multi_select_net_changes() {
    let mut state = fixture_state();
    let _ = render(&mut state, 140, 40);

    // Fast scrolling never issues requests directly; only the settled key is pending.
    let mut effects = Vec::new();
    for _ in 0..3 {
        effects.extend(key(&mut state, KeyCode::Down, KeyModifiers::NONE));
    }
    assert!(
        !effects
            .iter()
            .any(|e| matches!(e, Effect::RequestCommitDetails(_)))
    );
    let pending = state.details.pending.clone().expect("pending details").0;
    assert_eq!(pending.oids, vec![selected_oid(&state)]);
    let flushed = state.dispatch(Action::FlushPendingDetails);
    assert!(matches!(&flushed[..], [Effect::RequestCommitDetails(k)] if *k == pending));

    // LRU hit: re-selecting a cached commit renders with 0 ms (no pending request).
    let top = DetailsKey::single(oid(M));
    state.apply_engine_event(EngineEvent::CommitDetailsLoaded {
        bundle: bundle_for(
            top.clone(),
            vec![change("src/a.rs", FileStatusKind::Modified)],
        ),
        epoch: 7,
    });
    state.dispatch(Action::SelectLogRow(0));
    assert!(state.details.pending.is_none());
    assert_eq!(state.details.current_key, Some(top));

    // Shift+Down ×4 → 5 selected commits, one net-changes request.
    for _ in 0..4 {
        key(&mut state, KeyCode::Down, KeyModifiers::SHIFT);
    }
    let range_key = state.details.current_key.clone().unwrap();
    assert_eq!(range_key.oids.len(), 5);
    assert_eq!(range_key.oids[0], oid(M), "top-to-bottom visible order");
    let flushed = state.dispatch(Action::FlushPendingDetails);
    assert!(matches!(&flushed[..], [Effect::RequestCommitDetails(k)] if *k == range_key));

    state.apply_engine_event(EngineEvent::CommitDetailsLoaded {
        bundle: bundle_for(
            range_key,
            vec![
                change("src/core/graph/a.rs", FileStatusKind::Modified),
                change("src/core/graph/b.rs", FileStatusKind::Added),
                change("README.md", FileStatusKind::Deleted),
            ],
        ),
        epoch: 8,
    });
    let terminal = render(&mut state, 140, 40);
    let text = screen_text(terminal.backend().buffer()).join("\n");
    assert!(text.contains("5 commits · 3 files  +9 -3"), "{text}");
    assert!(
        text.contains("src/core/graph"),
        "compact middle packages: {text}"
    );
    assert!(text.contains("5 commits selected"), "{text}");
}

#[test]
fn details_parent_link_click_jumps_and_diff_preview_word_highlight() {
    let mut state = fixture_state();
    let key0 = DetailsKey::single(oid(M));
    state.apply_engine_event(EngineEvent::CommitDetailsLoaded {
        bundle: bundle_for(
            key0.clone(),
            vec![change("src/lib.rs", FileStatusKind::Modified)],
        ),
        epoch: 1,
    });
    let terminal = render(&mut state, 160, 40);
    let lines = screen_text(terminal.backend().buffer());
    let text = lines.join("\n");
    assert!(text.contains("Signed-off-by"), "{text}");
    assert!(text.contains("#42"), "{text}");

    // Click the second parent's short hash in the details panel.
    let short = oid(O1).short();
    let (y, line) = lines
        .iter()
        .enumerate()
        .find(|(_, l)| l.contains("Parents") && l.contains(&short))
        .expect("parents line");
    let byte = line.find(&short).unwrap();
    let x = unicode_width::UnicodeWidthStr::width(&line[..byte]) as u16;
    let actions = route_terminal_event(
        Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: x,
            row: y as u16,
            modifiers: KeyModifiers::NONE,
        }),
        &mut state,
    );
    run(&mut state, actions);
    assert_eq!(selected_oid(&state), oid(O1));
    assert_eq!(state.focused_pane, PaneId::LogCommitTable);

    // Back to the merge; open the inline diff preview for its file.
    state.dispatch(Action::JumpToCommit(oid(M)));
    state.dispatch(Action::FocusPane(PaneId::LogChangesTree));
    let file_row = state
        .details
        .change_rows()
        .iter()
        .position(|r| r.label == "lib.rs")
        .unwrap();
    state.dispatch(Action::ChangesSelectRow(file_row));
    let effects = key(&mut state, KeyCode::Enter, KeyModifiers::NONE);
    let diff_key = FileDiffKey {
        details: key0,
        path: "src/lib.rs".into(),
        old_path: None,
        context: DiffContext::Three,
    };
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::RequestFileDiff(k) if *k == diff_key)),
        "{effects:?}"
    );

    let line = |kind, old, new, text: &str| DiffLine {
        kind,
        old_lineno: old,
        new_lineno: new,
        text: text.into(),
        word_ranges: Vec::new(),
    };
    let mut hunk = DiffHunk {
        header: "@@ -1,3 +1,3 @@ fn main".into(),
        lines: vec![
            line(DiffLineKind::Context, Some(1), Some(1), "fn main() {"),
            line(DiffLineKind::Deleted, Some(2), None, "    call(alpha);"),
            line(DiffLineKind::Added, None, Some(2), "    call(omega);"),
            line(DiffLineKind::Context, Some(3), Some(3), "}"),
        ],
    };
    apply_word_diff(&mut hunk);
    state.apply_engine_event(EngineEvent::FileDiffLoaded {
        diff: Arc::new(FileDiff {
            key: diff_key,
            hunks: vec![hunk],
            is_binary: false,
            is_truncated: false,
        }),
        epoch: 1,
    });
    let terminal = render(&mut state, 160, 40);
    let buf = terminal.backend().buffer();
    let lines = screen_text(buf);
    let (y, l) = lines
        .iter()
        .enumerate()
        .find(|(_, l)| l.contains("call(omega)"))
        .expect("added line rendered");
    let byte = l.find("omega").unwrap();
    let x = unicode_width::UnicodeWidthStr::width(&l[..byte]) as u16;
    assert_eq!(
        buf[(x, y as u16)].bg,
        state.theme.diff_add_word_bg,
        "word-level highlight"
    );
    let paren = x - 1;
    assert_eq!(
        buf[(paren, y as u16)].bg,
        state.theme.diff_add_bg,
        "line-level highlight"
    );

    // F4 zooms to full screen; Esc returns.
    key(&mut state, KeyCode::F(4), KeyModifiers::NONE);
    assert!(state.details.diff_zoomed);
    key(&mut state, KeyCode::Esc, KeyModifiers::NONE);
    assert!(!state.details.diff_zoomed);
}

#[test]
fn filter_editor_popup_applies_user_and_date_dimensions() {
    let mut state = fixture_state();
    let _ = render(&mut state, 140, 30);
    state.dispatch(Action::OpenPopup {
        kind: PopupKind::FilterEditor(crate::log_view::FilterField::User),
        initial_query: String::new(),
    });
    key(&mut state, KeyCode::Char('@'), KeyModifiers::NONE);
    key(&mut state, KeyCode::Tab, KeyModifiers::NONE);
    key(&mut state, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(state.log_filter.spec.user.users, vec!["@me"]);
    // Alice authored the even-numbered commits: a2, o1, m.
    assert_eq!(state.log_graph.visible_count(), 3);

    state.dispatch(Action::ApplyFilterField {
        field: crate::log_view::FilterField::Date,
        expr: "garbage".into(),
    });
    assert!(
        state
            .status_notice
            .as_deref()
            .unwrap_or("")
            .contains("cannot parse")
    );
    state.dispatch(Action::ClearAllFilters);
    assert_eq!(state.log_graph.visible_count(), 6);
}

#[test]
fn clear_branch_filter_button_and_diff_dock_modes() {
    use crate::log_view::DiffPreviewDock;

    let mut state = fixture_state();
    let _ = render(&mut state, 140, 40);
    state.dispatch(Action::FocusPane(PaneId::LogBranchesTree));
    let folder_row = state
        .branches
        .rows
        .iter()
        .position(|r| state.branches.model.nodes[r.node].key == "local:feature/auth")
        .unwrap();
    state.dispatch(Action::SelectBranchRow(folder_row));
    state.dispatch(Action::BranchesApplyFilter);
    assert!(!state.log_filter.spec.branch.is_empty());

    let terminal = render(&mut state, 140, 40);
    let lines = screen_text(terminal.backend().buffer());
    let (y, line) = lines
        .iter()
        .enumerate()
        .find(|(_, l)| l.contains("[Clear Branch Filter]"))
        .expect("[Clear Branch Filter] button rendered");
    let byte = line.find("[Clear Branch Filter]").unwrap();
    let x = unicode_width::UnicodeWidthStr::width(&line[..byte]) as u16;
    let actions = route_terminal_event(
        Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: x,
            row: y as u16,
            modifiers: KeyModifiers::NONE,
        }),
        &mut state,
    );
    run(&mut state, actions);
    assert!(state.log_filter.spec.branch.is_empty());

    // Diff preview dock modes: Details -> Right (4th col) -> Bottom -> Details.
    state.dispatch(Action::SelectLogRow(0));
    let key0 = DetailsKey::single(oid(M));
    state.apply_engine_event(EngineEvent::CommitDetailsLoaded {
        bundle: bundle_for(key0, vec![change("src/lib.rs", FileStatusKind::Modified)]),
        epoch: 1,
    });
    state.dispatch(Action::ChangesSelectRow(1));
    state.dispatch(Action::ToggleDiffPreview);
    assert_eq!(state.details.diff_dock, DiffPreviewDock::Details);

    state.dispatch(Action::DiffCycleDock);
    assert_eq!(state.details.diff_dock, DiffPreviewDock::Right);
    let term_right = render(&mut state, 180, 40);
    let txt_right = screen_text(term_right.backend().buffer()).join("\n");
    assert!(txt_right.contains("Signed-off-by"), "{txt_right}");
    assert!(txt_right.contains("Diff · src/lib.rs"), "{txt_right}");

    state.dispatch(Action::DiffCycleDock);
    assert_eq!(state.details.diff_dock, DiffPreviewDock::Bottom);
    let term_bot = render(&mut state, 140, 48);
    let txt_bot = screen_text(term_bot.backend().buffer()).join("\n");
    assert!(txt_bot.contains("Closes #42"), "{txt_bot}");
    assert!(txt_bot.contains("Diff · src/lib.rs"), "{txt_bot}");
}

#[test]
fn mouse_drag_select_parent_chooser_and_lru_recency() {
    let mut state = fixture_state();
    let terminal = render(&mut state, 140, 40);
    let lines = screen_text(terminal.backend().buffer());
    let row_y = |subj: &str| {
        lines
            .iter()
            .enumerate()
            .find(|(_, l)| l.contains(subj))
            .map(|(y, l)| (y as u16, l.find(subj).unwrap()))
            .unwrap()
    };
    let (y0, b0) = row_y("Merge oauth flow");
    let (y2, b2) = row_y("login form");
    let x0 = unicode_width::UnicodeWidthStr::width(&lines[y0 as usize][..b0]) as u16;
    let x2 = unicode_width::UnicodeWidthStr::width(&lines[y2 as usize][..b2]) as u16;

    let a1 = route_terminal_event(
        Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: x0,
            row: y0,
            modifiers: KeyModifiers::NONE,
        }),
        &mut state,
    );
    run(&mut state, a1);
    let a2 = route_terminal_event(
        Event::Mouse(MouseEvent {
            kind: MouseEventKind::Drag(MouseButton::Left),
            column: x2,
            row: y2,
            modifiers: KeyModifiers::NONE,
        }),
        &mut state,
    );
    run(&mut state, a2);
    let a3 = route_terminal_event(
        Event::Mouse(MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            column: x2,
            row: y2,
            modifiers: KeyModifiers::NONE,
        }),
        &mut state,
    );
    run(&mut state, a3);
    assert_eq!(state.log_graph.selected_range(), 0..=2);

    // Parent chooser popup: `m` on merge M, then `2` jumps to second parent O1.
    state.dispatch(Action::SelectLogRow(row_of(&state, M)));
    key(&mut state, KeyCode::Char('m'), KeyModifiers::NONE);
    assert!(matches!(
        state.modal_stack.last(),
        Some(ModalLayer::FloatingPopup {
            kind: PopupKind::ChooseParent,
            ..
        })
    ));
    key(&mut state, KeyCode::Char('2'), KeyModifiers::NONE);
    assert!(state.modal_stack.is_empty());
    assert_eq!(selected_oid(&state), oid(O1));

    // LRU recency: touching the oldest cached key prevents its eviction.
    let key_m = DetailsKey::single(oid(M));
    state.apply_engine_event(EngineEvent::CommitDetailsLoaded {
        bundle: bundle_for(key_m.clone(), vec![]),
        epoch: 10,
    });
    for i in 1..crate::log_view::DETAILS_CACHE_CAPACITY {
        let mut bytes = [0u8; 20];
        bytes[..8].copy_from_slice(&(i as u64 + 1000).to_le_bytes());
        let k = DetailsKey::single(Oid::from_bytes(bytes));
        state.apply_engine_event(EngineEvent::CommitDetailsLoaded {
            bundle: bundle_for(k, vec![]),
            epoch: 10 + i as u64,
        });
    }
    assert!(state.details.cache.contains(&key_m));
    // Re-selecting row 0 (oid M) must promote `key_m` to most-recently-used.
    state.dispatch(Action::SelectLogRow(0));
    // Insert one more entry; `key_m` must survive because it was touched.
    let extra = DetailsKey::single(Oid::from_bytes([99; 20]));
    state.apply_engine_event(EngineEvent::CommitDetailsLoaded {
        bundle: bundle_for(extra, vec![]),
        epoch: 999,
    });
    assert!(
        state.details.cache.contains(&key_m),
        "LRU must keep recently accessed entry"
    );
}

#[test]
fn render_50k_commits_viewport_virtualized() {
    let count = 50_000usize;
    let oid_u = |n: usize| {
        let mut bytes = [0u8; 20];
        bytes[..8].copy_from_slice(&(n as u64).to_le_bytes());
        Oid::from_bytes(bytes)
    };
    let mut commits = Vec::with_capacity(count);
    for i in 0..count {
        let mut parents = Vec::with_capacity(2);
        if i + 1 < count {
            parents.push(oid_u(i + 1));
        }
        if i % 64 == 0 && i + 17 < count {
            parents.push(oid_u(i + 17));
        }
        commits.push(CommitSummary {
            oid: oid_u(i),
            parents,
            author_name: "Alice".into(),
            author_email: "alice@x.io".into(),
            timestamp: (count - i) as i64,
            author_timestamp: (count - i) as i64,
            subject: format!("commit {i}"),
            body: String::new(),
        });
    }
    let snap = RepoSnapshot {
        repo_name: "bench50k".into(),
        head_branch: Some("main".into()),
        head_oid: Some(oid_u(0)),
        refs: vec![RefInfo {
            name: "main".into(),
            full_name: "refs/heads/main".into(),
            target_oid: oid_u(0),
            kind: RefKind::LocalBranch,
            upstream: None,
            ahead_behind: None,
            is_head: true,
        }],
        commits,
        ..Default::default()
    };
    let mut state = AppState::new(".".into(), None);
    state.apply_engine_event(EngineEvent::RepoSnapshotLoaded {
        snapshot: Arc::new(snap),
        epoch: 1,
    });
    let mut terminal = Terminal::new(TestBackend::new(160, 45)).unwrap();
    // Warm graph cache once.
    terminal.draw(|f| render_app(f, &mut state)).unwrap();

    let frames = 60;
    let start = std::time::Instant::now();
    for i in 0..frames {
        state.dispatch(Action::SelectLogRow((i * 100) % count));
        terminal.draw(|f| render_app(f, &mut state)).unwrap();
    }
    let per_frame = start.elapsed() / frames as u32;
    eprintln!("50k-commit render_app per frame: {per_frame:?}");
}

#[test]
fn changes_toolbar_expand_collapse_buttons_reflect_tree_state() {
    let mut state = fixture_state();
    let key0 = DetailsKey::single(oid(M));
    state.apply_engine_event(EngineEvent::CommitDetailsLoaded {
        bundle: bundle_for(
            key0,
            vec![
                change("src/a.rs", FileStatusKind::Modified),
                change("src/nested/b.rs", FileStatusKind::Added),
                change("tests/c.rs", FileStatusKind::Modified),
            ],
        ),
        epoch: 1,
    });

    let find_glyph = |buf: &Buffer, glyph: &str| -> (u16, u16) {
        let lines = screen_text(buf);
        for (y, line) in lines.iter().enumerate() {
            if let Some(byte) = line.find(glyph) {
                let x = unicode_width::UnicodeWidthStr::width(&line[..byte]) as u16;
                return (x, y as u16);
            }
        }
        panic!("glyph {glyph} not found in:\n{}", lines.join("\n"));
    };

    // Initial state: all directories expanded -> ⊕ active, ⊖ inactive.
    let t = render(&mut state, 140, 40);
    let buf = t.backend().buffer();
    let (ex_x, ex_y) = find_glyph(buf, "⊕");
    let (co_x, co_y) = find_glyph(buf, "⊖");
    assert_eq!(buf[(ex_x, ex_y)].fg, state.theme.fg_bright);
    assert_eq!(buf[(ex_x, ex_y)].bg, state.theme.bg_selection_unfocused);
    assert_eq!(buf[(co_x, co_y)].fg, state.theme.fg_muted);
    assert_eq!(buf[(co_x, co_y)].bg, state.theme.bg_panel);

    // Click ⊖ -> all root dirs collapsed -> ⊕ inactive, ⊖ active.
    let acts = route_terminal_event(
        Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: co_x,
            row: co_y,
            modifiers: KeyModifiers::NONE,
        }),
        &mut state,
    );
    run(&mut state, acts);
    let t = render(&mut state, 140, 40);
    let buf = t.backend().buffer();
    assert_eq!(buf[(ex_x, ex_y)].fg, state.theme.fg_muted);
    assert_eq!(buf[(ex_x, ex_y)].bg, state.theme.bg_panel);
    assert_eq!(buf[(co_x, co_y)].fg, state.theme.fg_bright);
    assert_eq!(buf[(co_x, co_y)].bg, state.theme.bg_selection_unfocused);

    // Expand only `src` -> mixed state -> neither ⊕ nor ⊖ active.
    state.details.changes_collapsed.remove("src");
    let t = render(&mut state, 140, 40);
    let buf = t.backend().buffer();
    assert_eq!(buf[(ex_x, ex_y)].fg, state.theme.fg_muted);
    assert_eq!(buf[(ex_x, ex_y)].bg, state.theme.bg_panel);
    assert_eq!(buf[(co_x, co_y)].fg, state.theme.fg_muted);
    assert_eq!(buf[(co_x, co_y)].bg, state.theme.bg_panel);

    // Click ⊕ -> all expanded again -> ⊕ active, ⊖ inactive.
    let acts = route_terminal_event(
        Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: ex_x,
            row: ex_y,
            modifiers: KeyModifiers::NONE,
        }),
        &mut state,
    );
    run(&mut state, acts);
    let t = render(&mut state, 140, 40);
    let buf = t.backend().buffer();
    assert_eq!(buf[(ex_x, ex_y)].fg, state.theme.fg_bright);
    assert_eq!(buf[(ex_x, ex_y)].bg, state.theme.bg_selection_unfocused);
    assert_eq!(buf[(co_x, co_y)].fg, state.theme.fg_muted);
    assert_eq!(buf[(co_x, co_y)].bg, state.theme.bg_panel);

    assert_eq!(
        state.details.changes_expand_state(),
        ChangesExpandState::AllExpanded
    );

    // Collapse only a nested dir (`src/nested`) -> mixed state -> neither active.
    state.details.changes_collapsed.insert("src/nested".into());
    assert_eq!(
        state.details.changes_expand_state(),
        ChangesExpandState::Mixed
    );

    // Flat mode (`GroupByDir` off) -> neither active.
    state.details.changes_collapsed.clear();
    state.dispatch(Action::ChangesToggleGroupByDir);
    assert_eq!(
        state.details.changes_expand_state(),
        ChangesExpandState::Unavailable
    );
}

#[test]
fn changes_tree_enter_on_file_opens_and_focuses_diff_without_toggling_off() {
    let mut state = fixture_state();
    let key0 = DetailsKey::single(oid(M));
    state.apply_engine_event(EngineEvent::CommitDetailsLoaded {
        bundle: bundle_for(key0, vec![change("src/lib.rs", FileStatusKind::Modified)]),
        epoch: 1,
    });
    state.dispatch(Action::FocusPane(PaneId::LogChangesTree));
    // Row 0 is `src`, Row 1 is `lib.rs`.
    key(&mut state, KeyCode::Enter, KeyModifiers::NONE);
    assert!(!state.details.diff_preview, "Enter on dir only collapses dir");
    assert_eq!(state.focused_pane, PaneId::LogChangesTree);
    key(&mut state, KeyCode::Enter, KeyModifiers::NONE); // re-expand `src`

    let file_row = state
        .details
        .change_rows()
        .iter()
        .position(|r| r.label == "lib.rs")
        .unwrap();
    state.dispatch(Action::ChangesSelectRow(file_row));

    // Enter on file opens diff preview AND focuses LogDetailsDiff.
    key(&mut state, KeyCode::Enter, KeyModifiers::NONE);
    assert!(state.details.diff_preview);
    assert_eq!(state.focused_pane, PaneId::LogDetailsDiff);

    // Go back to LogChangesTree while keeping diff_preview open (e.g. via Tab/click),
    // pressing Enter on a file again keeps diff_preview open and focuses LogDetailsDiff.
    state.dispatch(Action::FocusPane(PaneId::LogChangesTree));
    key(&mut state, KeyCode::Enter, KeyModifiers::NONE);
    assert!(state.details.diff_preview);
    assert_eq!(state.focused_pane, PaneId::LogDetailsDiff);

    // Esc from LogDetailsDiff closes diff_preview and returns focus to LogChangesTree.
    key(&mut state, KeyCode::Esc, KeyModifiers::NONE);
    assert!(!state.details.diff_preview);
    assert_eq!(state.focused_pane, PaneId::LogChangesTree);
}

#[test]
fn commit_stage_toolbar_highlights_on_hover() {
    let mut state = fixture_state();
    state.dispatch(Action::SwitchTab(WorkspaceTab::Commit));
    let tracked = state.i18n.t("view.commit.toolbar_tracked").to_string();
    let t = render(&mut state, 140, 40);
    let buf = t.backend().buffer();
    let lines = screen_text(buf);
    let (rx, ry) = lines
        .iter()
        .enumerate()
        .find_map(|(y, line)| {
            if !line.contains(&tracked) {
                return None;
            }
            line.find("↻").map(|byte| {
                (
                    unicode_width::UnicodeWidthStr::width(&line[..byte]) as u16,
                    y as u16,
                )
            })
        })
        .expect("stage toolbar refresh button not found");
    assert_eq!(buf[(rx, ry)].fg, state.theme.fg_muted);
    assert_eq!(buf[(rx, ry)].bg, state.theme.bg_panel);

    state.hover_target = Some(HitTarget::StageToolbar(StageToolbarButton::Refresh));
    let t = render(&mut state, 140, 40);
    let buf = t.backend().buffer();
    assert_eq!(buf[(rx, ry)].fg, state.theme.fg_bright);
    assert_eq!(buf[(rx, ry)].bg, state.theme.bg_hover);
}
