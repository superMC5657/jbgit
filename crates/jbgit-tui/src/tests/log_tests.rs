//! Acceptance tests for the three-pane Log workbench, driven through
//! [`TestSession`] (`TuiSession` + `RecordingEngineBackend` + `TestHost`).

use std::sync::Arc;
use std::time::Duration;

use crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEventKind};
use jbgit_core::{
    CommitChangesBundle, CommitDetails, CommitSummary, DetailsKey, DiffContext, DiffHunk, DiffLine,
    DiffLineKind, FileChange, FileDiff, FileDiffKey, FileStatusKind, Oid, PaneId, PersonStamp,
    RefInfo, RefKind, RepoSnapshot, SplitterId, WorkspaceTab, apply_word_diff,
};
use jbgit_engine::{EngineEvent, TaskRequest};
use ratatui::buffer::Buffer;

use crate::app::{Action, ChangesHost, ModalLayer, PopupKind};
use crate::hit_test::{HitTarget, StageToolbarButton};
use crate::log_view::ChangesExpandState;

use super::TestSession;

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
fn fixture_session() -> TestSession {
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
    TestSession::with_snapshot(snap)
}

/// Lets the 120 ms text-filter debounce elapse via virtual clock.
fn settle_filter(ts: &mut TestSession) {
    let due = ts
        .state()
        .log
        .filter
        .text_apply_at
        .expect("debounced filter edit pending");
    let delta = due.saturating_duration_since(ts.now());
    ts.advance_by(delta);
}

fn screen_text(buf: &Buffer) -> Vec<String> {
    (0..buf.area.height)
        .map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect())
        .collect()
}

fn selected_oid(ts: &TestSession) -> Oid {
    ts.state()
        .log_commit_at(ts.state().log.graph.selected_idx)
        .map(|c| c.oid)
        .unwrap()
}

fn row_of(ts: &TestSession, n: u8) -> usize {
    ts.state()
        .log
        .graph
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
        exists_in_workdir: kind != FileStatusKind::Deleted,
        old_oid: None,
        new_oid: None,
    }
}

#[test]
fn folder_enter_filters_all_child_branches_and_esc_restores() {
    let mut ts = fixture_session();
    let _ = ts.render(140, 40);
    let folder_row = ts
        .state()
        .log
        .branches
        .rows
        .iter()
        .position(|r| ts.state().log.branches.model.nodes[r.node].key == "local:feature/auth")
        .expect("feature/auth folder row");
    ts.dispatch(Action::FocusPane(PaneId::LogBranchesTree));
    ts.dispatch(Action::SelectBranchRow(folder_row));
    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);

    // login + oauth tips and their shared ancestor only.
    assert_eq!(ts.state().log.graph.visible_count(), 3);
    let mut include = ts.state().log.filter.spec.branch.include.clone();
    include.sort();
    assert_eq!(include, vec!["feature/auth/login", "feature/auth/oauth"]);

    // Esc on the tree (no speed-search query) clears the branch filter.
    ts.send_key(KeyCode::Esc, KeyModifiers::NONE);
    assert!(ts.state().log.filter.spec.branch.is_empty());
    assert_eq!(ts.state().log.graph.visible_count(), 6);

    // Ctrl+Click union: bugfix/ui + main.
    let _ = ts.render(140, 40);
    let idx_of = |ts: &TestSession, key: &str| {
        ts.state()
            .log
            .branches
            .rows
            .iter()
            .position(|r| ts.state().log.branches.model.nodes[r.node].key == key)
            .unwrap()
    };
    let bugfix = idx_of(&ts, "refs/heads/bugfix/ui");
    let main = idx_of(&ts, "refs/heads/main");
    ts.dispatch(Action::SelectBranchRow(bugfix));
    ts.dispatch(Action::BranchesToggleMulti(main));
    ts.dispatch(Action::BranchesApplyFilter);
    // bugfix/ui (b1, a2, a1) ∪ main (m, a2, o1, a1) = 5 commits.
    assert_eq!(ts.state().log.graph.visible_count(), 5);
}

#[test]
fn ahead_behind_badges_and_narrow_terminal_collapse() {
    let mut ts = fixture_session();
    let buf = ts.render(140, 30);
    let text = screen_text(&buf).join("\n");
    assert!(text.contains("HEAD (main)"), "{text}");
    assert!(text.contains("↑2 ↓1"), "{text}");
    assert!(
        ts.state()
            .splitter_containers
            .contains_key(&SplitterId::LogLeftHorizontal)
    );

    // < 110 columns: Branches pane folds into the `»` stripe.
    let buf = ts.render(100, 30);
    assert!(
        !ts.state()
            .splitter_containers
            .contains_key(&SplitterId::LogLeftHorizontal)
    );
    let lines = screen_text(&buf);
    assert!(lines[2].starts_with('»'), "{}", lines[2]);
    assert!(ts.state().log.branches.is_collapsed(100));

    // Widening restores it; Alt+B collapses it explicitly.
    let _ = ts.render(140, 30);
    assert!(
        ts.state()
            .splitter_containers
            .contains_key(&SplitterId::LogLeftHorizontal)
    );
    ts.send_key(KeyCode::Char('b'), KeyModifiers::ALT);
    let _ = ts.render(140, 30);
    assert!(
        !ts.state()
            .splitter_containers
            .contains_key(&SplitterId::LogLeftHorizontal)
    );
}

#[test]
fn text_regex_filter_hides_rows_and_speed_search_finds_branches() {
    let mut ts = fixture_session();
    let _ = ts.render(140, 30);
    ts.send_key(KeyCode::Char('/'), KeyModifiers::NONE);
    assert!(ts.state().log.filter.editing_text);
    for c in "OAUTH".chars() {
        ts.send_key(KeyCode::Char(c), KeyModifiers::NONE);
    }
    // 'q' while typing must not quit.
    assert!(!ts.should_quit());
    // Typing is debounced: the graph is re-filtered once typing pauses.
    assert_eq!(ts.state().log.graph.visible_count(), 6);
    settle_filter(&mut ts);
    assert_eq!(ts.state().log.graph.visible_count(), 2);
    // Match case on → no hits.
    ts.send_key(KeyCode::Char('c'), KeyModifiers::ALT);
    assert_eq!(ts.state().log.graph.visible_count(), 0);
    ts.send_key(KeyCode::Char('c'), KeyModifiers::ALT);
    ts.send_key(KeyCode::Char('u'), KeyModifiers::CONTROL);
    ts.send_key(KeyCode::Char('r'), KeyModifiers::ALT);
    for c in "^(fix|login)".chars() {
        ts.send_key(KeyCode::Char(c), KeyModifiers::NONE);
    }
    // `Esc` ends editing and applies the pending edit immediately.
    ts.send_key(KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(ts.state().log.graph.visible_count(), 2);
    assert!(!ts.state().log.filter.editing_text);

    // Branch search (`tree_speed_search = false` by default): `j`/`k` navigate without
    // triggering search, while `/` starts explicit search filtering.
    ts.dispatch(Action::FocusPane(PaneId::LogBranchesTree));
    let before_row = ts.state().log.branches.selected;
    ts.send_key(KeyCode::Char('j'), KeyModifiers::NONE);
    assert!(!ts.state().log.branches.searching);
    assert!(ts.state().log.branches.query.is_empty());
    assert!(ts.state().log.branches.selected >= before_row);

    ts.send_key(KeyCode::Char('/'), KeyModifiers::NONE);
    assert!(ts.state().log.branches.searching);
    for c in "oau".chars() {
        ts.send_key(KeyCode::Char(c), KeyModifiers::NONE);
    }
    let leaves: Vec<_> = ts
        .state()
        .log
        .branches
        .rows
        .iter()
        .filter(|r| r.expanded.is_none())
        .map(|r| ts.state().log.branches.model.nodes[r.node].label.clone())
        .collect();
    assert_eq!(leaves, vec!["oauth"]);
    ts.send_key(KeyCode::Esc, KeyModifiers::NONE);
    assert!(ts.state().log.branches.query.is_empty());
    assert!(!ts.state().log.branches.searching);

    // Enabling `tree_speed_search = true` restores direct typing speed search.
    ts.state_mut().config.keymap.tree_speed_search = true;
    for c in "oau".chars() {
        ts.send_key(KeyCode::Char(c), KeyModifiers::NONE);
    }
    let leaves_ss: Vec<_> = ts
        .state()
        .log
        .branches
        .rows
        .iter()
        .filter(|r| r.expanded.is_none())
        .map(|r| ts.state().log.branches.model.nodes[r.node].label.clone())
        .collect();
    assert_eq!(leaves_ss, vec!["oauth"]);
    ts.send_key(KeyCode::Esc, KeyModifiers::NONE);
    assert!(ts.state().log.branches.query.is_empty());
}

#[test]
fn parent_child_navigation_history_and_goto() {
    let mut ts = fixture_session();
    let _ = ts.render(140, 30);
    assert_eq!(selected_oid(&ts), oid(M));

    // On a 2-parent merge commit, → opens ChooseParent; Enter picks parent[0] (A2).
    ts.send_key(KeyCode::Right, KeyModifiers::NONE);
    assert!(matches!(
        ts.state().modal_stack.last(),
        Some(ModalLayer::ChooseParent { .. })
    ));
    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(
        selected_oid(&ts),
        oid(A2),
        "→ + Enter jumps to parent[0] over unrelated rows"
    );
    // A2 has 2 visible children (M and B1), so ← opens ChooseChild; Enter picks the first child.
    ts.send_key(KeyCode::Left, KeyModifiers::NONE);
    if matches!(
        ts.state().modal_stack.last(),
        Some(ModalLayer::ChooseChild { .. })
    ) {
        ts.send_key(KeyCode::Enter, KeyModifiers::NONE);
    }
    let child = selected_oid(&ts);
    assert!(child == oid(M) || child == oid(B1));

    ts.dispatch(Action::SelectLogRow(row_of(&ts, M)));
    ts.send_key(KeyCode::Right, KeyModifiers::SHIFT);
    assert_eq!(
        selected_oid(&ts),
        oid(O1),
        "Shift+→ jumps to the second parent"
    );

    ts.send_key(KeyCode::Left, KeyModifiers::ALT);
    assert_eq!(selected_oid(&ts), oid(M));
    ts.send_key(KeyCode::Right, KeyModifiers::ALT);
    assert_eq!(selected_oid(&ts), oid(O1));

    // Ctrl+G opens Go-to; rendering the floating popup must not panic at any terminal size,
    // and Enter resolves refs and HEAD~N.
    ts.send_key(KeyCode::Char('g'), KeyModifiers::CONTROL);
    assert!(matches!(
        ts.state().modal_stack.last(),
        Some(ModalLayer::GoToRevision { .. })
    ));
    let _ = ts.render(140, 40);
    let _ = ts.render(20, 6);
    ts.type_str("HEAD~1");
    let _ = ts.render(140, 40);
    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);
    assert!(ts.state().modal_stack.is_empty());
    assert_eq!(selected_oid(&ts), oid(A2));
    ts.dispatch(Action::GoToRevision("feature/auth/login".into()));
    assert_eq!(selected_oid(&ts), oid(L1));
}

#[test]
fn debounced_details_lru_hit_and_multi_select_net_changes() {
    let mut ts = fixture_session();
    let _ = ts.render(140, 40);
    let _ = ts.take_requests();

    // Fast scrolling never issues requests directly; only the settled key is pending.
    for _ in 0..3 {
        ts.send_key(KeyCode::Down, KeyModifiers::NONE);
    }
    let reqs = ts.take_requests();
    assert!(
        !reqs
            .iter()
            .any(|(r, _)| matches!(r, TaskRequest::LoadCommitDetails { .. }))
    );
    let pending = ts
        .state()
        .log
        .details
        .pending
        .clone()
        .expect("pending details")
        .0;
    assert_eq!(pending.oids, vec![selected_oid(&ts)]);
    ts.advance_by(Duration::from_millis(35));
    let flushed = ts.take_requests();
    assert!(
        matches!(&flushed[..], [(TaskRequest::LoadCommitDetails { key: k, .. }, _)] if *k == pending)
    );

    // LRU hit: re-selecting a cached commit renders with 0 ms (no pending request).
    let top = DetailsKey::single(oid(M));
    ts.deliver_engine_event(EngineEvent::CommitDetailsLoaded {
        slot: jbgit_core::RequestSlotId::LogCommitDetails,
        bundle: bundle_for(
            top.clone(),
            vec![change("src/a.rs", FileStatusKind::Modified)],
        ),
        epoch: 7,
    });
    ts.dispatch(Action::SelectLogRow(0));
    assert!(ts.state().log.details.pending.is_none());
    assert_eq!(ts.state().log.details.changes.current_key, Some(top));

    // Shift+Down ×4 → 5 selected commits, one net-changes request.
    for _ in 0..4 {
        ts.send_key(KeyCode::Down, KeyModifiers::SHIFT);
    }
    let range_key = ts.state().log.details.changes.current_key.clone().unwrap();
    assert_eq!(range_key.oids.len(), 5);
    assert_eq!(range_key.oids[0], oid(M), "top-to-bottom visible order");
    let _ = ts.take_requests();
    ts.advance_by(Duration::from_millis(35));
    let flushed = ts.take_requests();
    assert!(
        matches!(&flushed[..], [(TaskRequest::LoadCommitDetails { key: k, .. }, _)] if *k == range_key)
    );

    ts.deliver_engine_event(EngineEvent::CommitDetailsLoaded {
        slot: jbgit_core::RequestSlotId::LogCommitDetails,
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
    let buf = ts.render(140, 40);
    let text = screen_text(&buf).join("\n");
    assert!(text.contains("5 commits · 3 files  +9 -3"), "{text}");
    assert!(
        text.contains("src/core/graph"),
        "compact middle packages: {text}"
    );
    assert!(text.contains("5 commits selected"), "{text}");
}

#[test]
fn details_parent_link_click_jumps_and_diff_preview_word_highlight() {
    let mut ts = fixture_session();
    let key0 = DetailsKey::single(oid(M));
    ts.deliver_engine_event(EngineEvent::CommitDetailsLoaded {
        slot: jbgit_core::RequestSlotId::LogCommitDetails,
        bundle: bundle_for(
            key0.clone(),
            vec![change("src/lib.rs", FileStatusKind::Modified)],
        ),
        epoch: 1,
    });
    let buf = ts.render(160, 40);
    let lines = screen_text(&buf);
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
    ts.send_mouse(MouseEventKind::Down(MouseButton::Left), x, y as u16);
    assert_eq!(selected_oid(&ts), oid(O1));
    assert_eq!(ts.state().focused_pane, PaneId::LogCommitTable);

    // Back to the merge; open the inline diff preview for its file.
    ts.dispatch(Action::JumpToCommit(oid(M)));
    ts.dispatch(Action::FocusPane(PaneId::LogChangesTree));
    let file_row = ts
        .state()
        .log
        .details
        .change_rows(&ts.state().diff_caches)
        .iter()
        .position(|r| r.label == "lib.rs")
        .unwrap();
    ts.dispatch(Action::ChangesSelect(ChangesHost::Log, file_row));
    let _ = ts.take_requests();
    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);
    let diff_key = FileDiffKey {
        details: key0,
        path: "src/lib.rs".into(),
        old_path: None,
        context: DiffContext::Three,
        whitespace: jbgit_core::WhitespaceMode::Default,
    };
    let reqs = ts.take_requests();
    assert!(
        reqs.iter()
            .any(|(r, _)| matches!(r, TaskRequest::LoadFileDiff { key: k, .. } if *k == diff_key)),
        "{reqs:?}"
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
    ts.deliver_engine_event(EngineEvent::FileDiffLoaded {
        slot: jbgit_core::RequestSlotId::LogFileDiff,
        diff: Arc::new(FileDiff {
            key: diff_key,
            hunks: vec![hunk],
            is_binary: false,
            binary: None,
            is_truncated: false,
            is_too_large: false,
            lfs: None,
            mode_change: None,
        }),
        epoch: 1,
    });
    let buf = ts.render(160, 40);
    let lines = screen_text(&buf);
    let (y, l) = lines
        .iter()
        .enumerate()
        .find(|(_, l)| l.contains("call(omega)"))
        .expect("added line rendered");
    let byte = l.find("omega").unwrap();
    let x = unicode_width::UnicodeWidthStr::width(&l[..byte]) as u16;
    assert_eq!(
        buf[(x, y as u16)].bg,
        ts.state().theme.diff_add_word_bg,
        "word-level highlight"
    );
    let paren = x - 1;
    assert_eq!(
        buf[(paren, y as u16)].bg,
        ts.state().theme.diff_add_bg,
        "line-level highlight"
    );

    // F4 zooms to full screen; Esc returns.
    ts.send_key(KeyCode::F(4), KeyModifiers::NONE);
    assert!(ts.state().log.details.diff.zoomed);
    ts.send_key(KeyCode::Esc, KeyModifiers::NONE);
    assert!(!ts.state().log.details.diff.zoomed);
}

#[test]
fn filter_editor_popup_applies_user_and_date_dimensions() {
    let mut ts = fixture_session();
    let _ = ts.render(140, 30);
    ts.dispatch(Action::OpenPopup {
        kind: PopupKind::FilterEditor(crate::log_view::FilterField::User),
        initial_query: String::new(),
    });
    ts.send_key(KeyCode::Char('@'), KeyModifiers::NONE);
    ts.send_key(KeyCode::Tab, KeyModifiers::NONE);
    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(ts.state().log.filter.spec.user.users, vec!["@me"]);
    // Alice authored the even-numbered commits: a2, o1, m.
    assert_eq!(ts.state().log.graph.visible_count(), 3);

    ts.dispatch(Action::ApplyFilterField {
        field: crate::log_view::FilterField::Date,
        expr: "garbage".into(),
    });
    assert!(
        ts.state()
            .status_notice
            .as_deref()
            .unwrap_or("")
            .contains("cannot parse")
    );
    ts.dispatch(Action::ClearAllFilters);
    assert_eq!(ts.state().log.graph.visible_count(), 6);
}

#[test]
fn clear_branch_filter_button_and_diff_dock_modes() {
    use crate::log_view::DiffPreviewDock;

    let mut ts = fixture_session();
    let _ = ts.render(140, 40);
    ts.dispatch(Action::FocusPane(PaneId::LogBranchesTree));
    let folder_row = ts
        .state()
        .log
        .branches
        .rows
        .iter()
        .position(|r| ts.state().log.branches.model.nodes[r.node].key == "local:feature/auth")
        .unwrap();
    ts.dispatch(Action::SelectBranchRow(folder_row));
    ts.dispatch(Action::BranchesApplyFilter);
    assert!(!ts.state().log.filter.spec.branch.is_empty());

    let buf = ts.render(140, 40);
    let lines = screen_text(&buf);
    let (y, line) = lines
        .iter()
        .enumerate()
        .find(|(_, l)| l.contains("[Clear Branch Filter]"))
        .expect("[Clear Branch Filter] button rendered");
    let byte = line.find("[Clear Branch Filter]").unwrap();
    let x = unicode_width::UnicodeWidthStr::width(&line[..byte]) as u16;
    ts.send_mouse(MouseEventKind::Down(MouseButton::Left), x, y as u16);
    assert!(ts.state().log.filter.spec.branch.is_empty());

    // Diff preview dock modes: Details -> Right (4th col) -> Bottom -> Details.
    ts.dispatch(Action::SelectLogRow(0));
    let key0 = DetailsKey::single(oid(M));
    ts.deliver_engine_event(EngineEvent::CommitDetailsLoaded {
        slot: jbgit_core::RequestSlotId::LogCommitDetails,
        bundle: bundle_for(key0, vec![change("src/lib.rs", FileStatusKind::Modified)]),
        epoch: 1,
    });
    ts.dispatch(Action::ChangesSelect(ChangesHost::Log, 1));
    ts.dispatch(Action::ToggleDiffPreview);
    assert_eq!(ts.state().log.details.diff_dock, DiffPreviewDock::Details);

    ts.dispatch(Action::DiffCycleDock);
    assert_eq!(ts.state().log.details.diff_dock, DiffPreviewDock::Right);
    let term_right = ts.render(180, 40);
    let txt_right = screen_text(&term_right).join("\n");
    assert!(txt_right.contains("Signed-off-by"), "{txt_right}");
    assert!(txt_right.contains("Diff · src/lib.rs"), "{txt_right}");

    ts.dispatch(Action::DiffCycleDock);
    assert_eq!(ts.state().log.details.diff_dock, DiffPreviewDock::Bottom);
    let term_bot = ts.render(140, 48);
    let txt_bot = screen_text(&term_bot).join("\n");
    assert!(txt_bot.contains("Closes #42"), "{txt_bot}");
    assert!(txt_bot.contains("Diff · src/lib.rs"), "{txt_bot}");
}

#[test]
fn mouse_drag_select_parent_chooser_and_lru_recency() {
    let mut ts = fixture_session();
    let buf = ts.render(140, 40);
    let lines = screen_text(&buf);
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

    ts.send_mouse(MouseEventKind::Down(MouseButton::Left), x0, y0);
    ts.send_mouse(MouseEventKind::Drag(MouseButton::Left), x2, y2);
    ts.send_mouse(MouseEventKind::Up(MouseButton::Left), x2, y2);
    assert_eq!(ts.state().log.graph.selected_range(), 0..=2);

    // Parent chooser popup: `m` on merge M, then `2` jumps to second parent O1.
    ts.dispatch(Action::SelectLogRow(row_of(&ts, M)));
    ts.send_key(KeyCode::Char('m'), KeyModifiers::NONE);
    assert!(matches!(
        ts.state().modal_stack.last(),
        Some(ModalLayer::ChooseParent { .. })
    ));
    ts.send_key(KeyCode::Char('2'), KeyModifiers::NONE);
    assert!(ts.state().modal_stack.is_empty());
    assert_eq!(selected_oid(&ts), oid(O1));

    // LRU recency: touching the oldest cached key prevents its eviction.
    let key_m = DetailsKey::single(oid(M));
    ts.deliver_engine_event(EngineEvent::CommitDetailsLoaded {
        slot: jbgit_core::RequestSlotId::LogCommitDetails,
        bundle: bundle_for(key_m.clone(), vec![]),
        epoch: 10,
    });
    for i in 1..crate::log_view::DETAILS_CACHE_CAPACITY {
        let mut bytes = [0u8; 20];
        bytes[..8].copy_from_slice(&(i as u64 + 1000).to_le_bytes());
        let k = DetailsKey::single(Oid::from_bytes(bytes));
        ts.deliver_engine_event(EngineEvent::CommitDetailsLoaded {
            slot: jbgit_core::RequestSlotId::LogCommitDetails,
            bundle: bundle_for(k, vec![]),
            epoch: 10 + i as u64,
        });
    }
    assert!(ts.state().diff_caches.details.contains(&key_m));
    // Re-selecting row 0 (oid M) must promote `key_m` to most-recently-used.
    ts.dispatch(Action::SelectLogRow(0));
    // Insert one more entry; `key_m` must survive because it was touched.
    let extra = DetailsKey::single(Oid::from_bytes([99; 20]));
    ts.deliver_engine_event(EngineEvent::CommitDetailsLoaded {
        slot: jbgit_core::RequestSlotId::LogCommitDetails,
        bundle: bundle_for(extra, vec![]),
        epoch: 999,
    });
    assert!(
        ts.state().diff_caches.details.contains(&key_m),
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
    let mut ts = TestSession::with_snapshot(snap);
    // Warm graph cache once.
    let _ = ts.render(160, 45);

    let frames = 60;
    let start = std::time::Instant::now();
    for i in 0..frames {
        ts.dispatch(Action::SelectLogRow((i * 100) % count));
        let _ = ts.render(160, 45);
    }
    let per_frame = start.elapsed() / frames as u32;
    eprintln!("50k-commit render_app per frame: {per_frame:?}");
}

#[test]
fn changes_toolbar_expand_collapse_buttons_reflect_tree_state() {
    let mut ts = fixture_session();
    let key0 = DetailsKey::single(oid(M));
    ts.deliver_engine_event(EngineEvent::CommitDetailsLoaded {
        slot: jbgit_core::RequestSlotId::LogCommitDetails,
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
    let buf = ts.render(140, 40);
    let (ex_x, ex_y) = find_glyph(&buf, "⊕");
    let (co_x, co_y) = find_glyph(&buf, "⊖");
    assert_eq!(buf[(ex_x, ex_y)].fg, ts.state().theme.fg_bright);
    assert_eq!(buf[(ex_x, ex_y)].bg, ts.state().theme.bg_selection_unfocused);
    assert_eq!(buf[(co_x, co_y)].fg, ts.state().theme.fg_muted);
    assert_eq!(buf[(co_x, co_y)].bg, ts.state().theme.bg_panel);

    // Click ⊖ -> all root dirs collapsed -> ⊕ inactive, ⊖ active.
    ts.send_mouse(MouseEventKind::Down(MouseButton::Left), co_x, co_y);
    let buf = ts.render(140, 40);
    assert_eq!(buf[(ex_x, ex_y)].fg, ts.state().theme.fg_muted);
    assert_eq!(buf[(ex_x, ex_y)].bg, ts.state().theme.bg_panel);
    assert_eq!(buf[(co_x, co_y)].fg, ts.state().theme.fg_bright);
    assert_eq!(buf[(co_x, co_y)].bg, ts.state().theme.bg_selection_unfocused);

    // Expand only `src` -> mixed state -> neither ⊕ nor ⊖ active.
    ts.state_mut().log.details.changes.collapsed.remove("src");
    let buf = ts.render(140, 40);
    assert_eq!(buf[(ex_x, ex_y)].fg, ts.state().theme.fg_muted);
    assert_eq!(buf[(ex_x, ex_y)].bg, ts.state().theme.bg_panel);
    assert_eq!(buf[(co_x, co_y)].fg, ts.state().theme.fg_muted);
    assert_eq!(buf[(co_x, co_y)].bg, ts.state().theme.bg_panel);

    // Click ⊕ -> all expanded again -> ⊕ active, ⊖ inactive.
    ts.send_mouse(MouseEventKind::Down(MouseButton::Left), ex_x, ex_y);
    let buf = ts.render(140, 40);
    assert_eq!(buf[(ex_x, ex_y)].fg, ts.state().theme.fg_bright);
    assert_eq!(buf[(ex_x, ex_y)].bg, ts.state().theme.bg_selection_unfocused);
    assert_eq!(buf[(co_x, co_y)].fg, ts.state().theme.fg_muted);
    assert_eq!(buf[(co_x, co_y)].bg, ts.state().theme.bg_panel);

    assert_eq!(
        ts.state()
            .log
            .details
            .changes
            .expand_state(&ts.state().diff_caches),
        ChangesExpandState::AllExpanded
    );

    // Collapse only a nested dir (`src/nested`) -> mixed state -> neither active.
    ts.state_mut()
        .log
        .details
        .changes
        .collapsed
        .insert("src/nested".into());
    assert_eq!(
        ts.state()
            .log
            .details
            .changes
            .expand_state(&ts.state().diff_caches),
        ChangesExpandState::Mixed
    );

    // Flat mode (`GroupByDir` off) -> neither active.
    ts.state_mut().log.details.changes.collapsed.clear();
    ts.dispatch(Action::ChangesToggleGroupByDir(ChangesHost::Log));
    assert_eq!(
        ts.state()
            .log
            .details
            .changes
            .expand_state(&ts.state().diff_caches),
        ChangesExpandState::Unavailable
    );
}

#[test]
fn changes_tree_enter_on_file_opens_and_focuses_diff_without_toggling_off() {
    let mut ts = fixture_session();
    let key0 = DetailsKey::single(oid(M));
    ts.deliver_engine_event(EngineEvent::CommitDetailsLoaded {
        slot: jbgit_core::RequestSlotId::LogCommitDetails,
        bundle: bundle_for(key0, vec![change("src/lib.rs", FileStatusKind::Modified)]),
        epoch: 1,
    });
    ts.dispatch(Action::FocusPane(PaneId::LogChangesTree));
    // Row 0 is `src`, Row 1 is `lib.rs`.
    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);
    assert!(
        !ts.state().log.details.diff_preview,
        "Enter on dir only collapses dir"
    );
    assert_eq!(ts.state().focused_pane, PaneId::LogChangesTree);
    ts.send_key(KeyCode::Enter, KeyModifiers::NONE); // re-expand `src`

    let file_row = ts
        .state()
        .log
        .details
        .change_rows(&ts.state().diff_caches)
        .iter()
        .position(|r| r.label == "lib.rs")
        .unwrap();
    ts.dispatch(Action::ChangesSelect(ChangesHost::Log, file_row));

    // Enter on file opens diff preview AND focuses LogDetailsDiff.
    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);
    assert!(ts.state().log.details.diff_preview);
    assert_eq!(ts.state().focused_pane, PaneId::LogDetailsDiff);

    // Go back to LogChangesTree while keeping diff_preview open,
    // pressing Enter on a file again keeps diff_preview open and focuses LogDetailsDiff.
    ts.dispatch(Action::FocusPane(PaneId::LogChangesTree));
    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);
    assert!(ts.state().log.details.diff_preview);
    assert_eq!(ts.state().focused_pane, PaneId::LogDetailsDiff);

    // Esc from LogDetailsDiff closes diff_preview and returns focus to LogChangesTree.
    ts.send_key(KeyCode::Esc, KeyModifiers::NONE);
    assert!(!ts.state().log.details.diff_preview);
    assert_eq!(ts.state().focused_pane, PaneId::LogChangesTree);
}

#[test]
fn commit_stage_toolbar_highlights_on_hover() {
    let mut ts = fixture_session();
    ts.dispatch(Action::SwitchTab(WorkspaceTab::Commit));
    let tracked = ts.state().i18n.t("view.commit.toolbar_tracked").to_string();
    let buf = ts.render(140, 40);
    let lines = screen_text(&buf);
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
    assert_eq!(buf[(rx, ry)].fg, ts.state().theme.fg_muted);
    assert_eq!(buf[(rx, ry)].bg, ts.state().theme.bg_panel);

    ts.state_mut().hover_target = Some(HitTarget::StageToolbar(StageToolbarButton::Refresh));
    let buf = ts.render(140, 40);
    assert_eq!(buf[(rx, ry)].fg, ts.state().theme.fg_bright);
    assert_eq!(buf[(rx, ry)].bg, ts.state().theme.bg_hover);
}

// ── Large-repo filtering & containing branches ──

#[test]
fn text_filter_typing_is_debounced_into_one_graph_rebuild() {
    let mut ts = fixture_session();
    let _ = ts.render(140, 30);
    let generation = ts.state().log.graph.filter_generation;
    let builds = ts.state().log.graph.cached.as_ref().unwrap().visible_builds;

    ts.send_key(KeyCode::Char('/'), KeyModifiers::NONE);
    ts.type_str("oauth");
    // Echo is immediate, the 50k-commit re-projection is not.
    assert_eq!(ts.state().log.filter.spec.text.query, "oauth");
    assert_eq!(ts.state().log.graph.filter_generation, generation);
    assert_eq!(
        ts.state().log.graph.cached.as_ref().unwrap().visible_builds,
        builds
    );
    let due = ts
        .state()
        .log
        .filter
        .text_apply_at
        .expect("debounce armed");
    assert!(ts.next_deadline().is_some_and(|d| d <= due));

    // Not yet due → still nothing.
    let almost_due = due - Duration::from_millis(1);
    let _ = ts.session.advance_to(almost_due, Some(&ts.engine), &mut ts.host);
    assert_eq!(ts.state().log.graph.filter_generation, generation);

    settle_filter(&mut ts);
    assert!(ts.state().log.filter.text_apply_at.is_none());
    assert_eq!(
        ts.state().log.graph.filter_generation,
        generation.wrapping_add(1)
    );
    assert_eq!(
        ts.state().log.graph.cached.as_ref().unwrap().visible_builds,
        builds + 1
    );
    assert_eq!(ts.state().log.graph.visible_count(), 2);
}

#[test]
fn snapshot_reload_with_active_filter_projects_visible_graph_once() {
    let mut ts = fixture_session();
    let _ = ts.render(140, 30);
    ts.send_key(KeyCode::Char('/'), KeyModifiers::NONE);
    ts.type_str("oauth");
    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(ts.state().log.graph.visible_count(), 2);

    // Same commits, new snapshot Arc (e.g. a watcher refresh).
    let reloaded = Arc::new((**ts.state().repo_snapshot.as_ref().unwrap()).clone());
    ts.deliver_engine_event(EngineEvent::RepoSnapshotLoaded {
        snapshot: reloaded,
        epoch: 2,
    });
    let _ = ts.render(140, 30);
    let cached = ts.state().log.graph.cached.as_ref().unwrap();
    assert_eq!(cached.visible_builds, 1, "no throw-away unfiltered projection");
    assert_eq!(ts.state().log.graph.visible_count(), 2);
}

#[test]
fn containing_branches_are_computed_off_thread_and_memoised() {
    let mut ts = fixture_session();
    ts.host.defer_async_jobs = true;
    let _ = ts.render(140, 40);
    let row = row_of(&ts, A2);
    ts.host.pending_containing.clear();
    ts.dispatch(Action::SelectLogRow(row));
    ts.advance_by(Duration::from_millis(35));
    let job = ts
        .host
        .pending_containing
        .pop()
        .expect("containing-branch walk is deferred to a background job");
    assert_eq!(job.oid, oid(A2));
    // The UI thread did not walk the graph.
    assert!(ts.state().log.details.containing.is_none());

    let names = job.run().expect("current token");
    assert!(names.iter().any(|n| n == "main"));
    assert!(names.iter().any(|n| n == "bugfix/ui"));
    ts.deliver_ui_async(crate::app::UiAsyncEvent::ContainingBranches {
        oid: job.oid,
        token: job.token,
        names: names.clone(),
    });
    assert_eq!(
        ts.state().log.details.containing,
        Some((oid(A2), names.clone()))
    );

    // A stale result (superseded epoch) is ignored.
    ts.host.pending_containing.clear();
    ts.dispatch(Action::SelectLogRow(row_of(&ts, L1)));
    ts.advance_by(Duration::from_millis(35));
    let l1_job = ts.host.pending_containing.pop().unwrap();
    let stale_token = l1_job.token;
    ts.state_mut().log.details.containing_slot.invalidate();
    ts.deliver_ui_async(crate::app::UiAsyncEvent::ContainingBranches {
        oid: l1_job.oid,
        token: stale_token,
        names: vec!["bogus".into()],
    });
    assert!(
        ts.state()
            .log
            .details
            .containing_cache
            .peek(&oid(L1))
            .is_none()
    );

    // Re-selecting a memoised commit needs no new walk.
    ts.host.pending_containing.clear();
    ts.dispatch(Action::SelectLogRow(row));
    ts.advance_by(Duration::from_millis(35));
    assert!(ts.host.pending_containing.is_empty());
    assert_eq!(ts.state().log.details.containing, Some((oid(A2), names)));
}

#[test]
fn large_graph_rebuild_offloads_off_thread_and_drops_stale_epochs() {
    use crate::graph::state::ASYNC_GRAPH_THRESHOLD;

    let count = ASYNC_GRAPH_THRESHOLD;
    let oid_u = |n: usize| {
        let mut bytes = [0u8; 20];
        bytes[..8].copy_from_slice(&(n as u64).to_le_bytes());
        Oid::from_bytes(bytes)
    };
    let mut commits = Vec::with_capacity(count);
    for i in 0..count {
        let parents = if i + 1 < count {
            vec![oid_u(i + 1)]
        } else {
            vec![]
        };
        commits.push(CommitSummary {
            oid: oid_u(i),
            parents,
            author_name: "Alice".into(),
            author_email: "alice@x.io".into(),
            timestamp: (count - i) as i64,
            author_timestamp: (count - i) as i64,
            subject: if i == 42 {
                "target needle".into()
            } else {
                format!("commit {i}")
            },
            body: String::new(),
        });
    }
    let snap = Arc::new(RepoSnapshot {
        repo_name: "large".into(),
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
    });

    let mut ts = TestSession::with_config(".".into(), crate::config::AppConfig::default());
    ts.state_mut().async_graph_enabled = true;
    ts.host.defer_async_jobs = true;

    ts.deliver_engine_event(EngineEvent::RepoSnapshotLoaded {
        snapshot: Arc::clone(&snap),
        epoch: 1,
    });
    // UI thread did NOT synchronously build the 2000+ commit graph.
    assert!(ts.state().log.graph.cached.is_none());
    assert!(ts.state().log.graph.rebuild_in_progress);
    let job = ts
        .host
        .pending_graph_rebuilds
        .pop()
        .expect("large snapshot offload emits RebuildLogGraph");
    let rebuilt = job.run().expect("valid token builds graph");
    ts.deliver_ui_async(crate::app::UiAsyncEvent::LogGraphRebuilt(Box::new(rebuilt)));
    assert!(!ts.state().log.graph.rebuild_in_progress);
    assert_eq!(ts.state().log.graph.visible_count(), count);

    // Trigger a text filter edit; a newer filter supersedes the first job.
    ts.dispatch(Action::FocusFilterText);
    ts.dispatch(Action::FilterTextInput('x'));
    ts.dispatch(Action::FilterTextDone);
    let job1 = ts
        .host
        .pending_graph_rebuilds
        .pop()
        .expect("first filter job");

    ts.dispatch(Action::FilterTextClear);
    ts.dispatch(Action::FocusFilterText);
    for c in "needle".chars() {
        ts.dispatch(Action::FilterTextInput(c));
    }
    ts.dispatch(Action::FilterTextDone);
    let job2 = ts
        .host
        .pending_graph_rebuilds
        .pop()
        .expect("second filter job");

    // Stale job1 cancels itself without wasting CPU or overwriting state.
    assert!(job1.run().is_none(), "superseded graph job must cancel");
    let rebuilt2 = job2.run().expect("latest graph job completes");
    ts.deliver_ui_async(crate::app::UiAsyncEvent::LogGraphRebuilt(Box::new(rebuilt2)));
    assert_eq!(ts.state().log.graph.visible_count(), 1);
}

#[test]
fn ahead_behind_backfill_updates_branches_without_rebuilding_graph() {
    let mut ts = fixture_session();
    let _ = ts.render(140, 40);
    let ptr_before = Arc::as_ptr(
        &ts.state()
            .log
            .graph
            .cached
            .as_ref()
            .unwrap()
            .permanent,
    );

    let repo_epoch = ts
        .state()
        .request_slots
        .latest(jbgit_core::RequestSlotId::RepoSnapshot);
    ts.deliver_engine_event(EngineEvent::RefAheadBehindBackfilled {
        epoch: repo_epoch,
        updates: vec![("refs/heads/bugfix/ui".to_string(), (3, 2))],
    });
    let ptr_after = Arc::as_ptr(
        &ts.state()
            .log
            .graph
            .cached
            .as_ref()
            .unwrap()
            .permanent,
    );
    assert_eq!(ptr_before, ptr_after, "backfill must not rebuild graph");

    let node = ts
        .state()
        .log
        .branches
        .model
        .nodes
        .iter()
        .find(|n| n.key == "refs/heads/bugfix/ui")
        .expect("bugfix/ui branch node");
    assert_eq!(node.ahead_behind, Some((3, 2)));

    // Stale epoch backfill is ignored.
    ts.deliver_engine_event(EngineEvent::RefAheadBehindBackfilled {
        epoch: repo_epoch.wrapping_add(99),
        updates: vec![("refs/heads/bugfix/ui".to_string(), (99, 99))],
    });
    let node = ts
        .state()
        .log
        .branches
        .model
        .nodes
        .iter()
        .find(|n| n.key == "refs/heads/bugfix/ui")
        .unwrap();
    assert_eq!(node.ahead_behind, Some((3, 2)));
}
