use std::sync::Arc;
use std::time::Duration;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use jbgit_core::{
    ChangeRowKind, CommitChangesBundle, CommitSummary, DetailsKey, DetailsMode, FileChange,
    FileStatusKind, LogFilterSpec, Oid, PaneId, PathFilter, WorkspaceTab,
};
use jbgit_engine::{EngineEvent, TaskRequest};

use crate::app::{Action, ModalLayer};
use crate::config::{AppConfig, JbgitConfigOverlay, KeymapConfig, KeymapPreset};
use crate::graph::LogGraphState;
use crate::graph::state::FilterInputs;
use crate::hit_test::{ChangesToolbarButton, DiffToolbarButton, HitTarget};
use crate::keymap::{KeyAction, actions::check_scopes, catalog};
use crate::log_view::{DETAILS_DEBOUNCE, MAX_AGGREGATED_COMMITS};
use crate::theme::ThemePalette;

use super::{
    TestSession, find_item, left_click, make_commit, make_details, make_snapshot, oid, right_click,
    screen_text, snapshot,
};

fn build_graph(snap: &Arc<jbgit_core::RepoSnapshot>) -> LogGraphState {
    let colors = ThemePalette::darcula(true).branch_color_manager();
    let mut g = LogGraphState::default();
    g.ensure_cache(
        Some(snap),
        &colors,
        &FilterInputs {
            spec: &LogFilterSpec::default(),
            path_matches: None,
            favorites: &[],
        },
    );
    g
}

#[test]
fn log_graph_state_non_contiguous_selection_state_machine_and_oid_reload() {
    let base_snap = Arc::new(snapshot()); // 12 commits: row 0 = c12 .. row 11 = c1
    let mut g = build_graph(&base_snap);
    let colors = ThemePalette::darcula(true).branch_color_manager();

    // 1. Initial single selection at row 0.
    assert_eq!(g.selected_idx, 0);
    assert_eq!(g.selection_count(), 1);
    assert!(!g.has_multi_selection());
    assert_eq!(g.selected_rows_vec(), vec![0]);
    assert_eq!(g.selected_oids(Some(&base_snap)), vec![oid(12)]);

    // 2. Shift contiguous range selection (0..=2) then plain Down collapses it.
    g.extend_selection_to(2);
    assert_eq!(g.selected_rows_vec(), vec![0, 1, 2]);
    assert_eq!(g.selection_count(), 3);
    assert!(g.has_multi_selection());
    assert!(g.is_row_selected(1));
    g.scroll_by(1, false);
    assert_eq!(g.selected_idx, 3);
    assert_eq!(g.selected_rows_vec(), vec![3]);
    assert!(!g.has_multi_selection());

    // 3. Ctrl+Click non-contiguous toggle: from row 3, Ctrl+Click row 0 and row 6.
    g.toggle_row_selection(0, false);
    assert_eq!(g.selected_rows_vec(), vec![0, 3]);
    assert_eq!(g.selected_idx, 0);
    assert_eq!(g.selection_count(), 2);
    g.toggle_row_selection(6, false);
    assert_eq!(g.selected_rows_vec(), vec![0, 3, 6]);
    assert_eq!(g.selected_oids(Some(&base_snap)), vec![oid(12), oid(9), oid(6)]);

    // Unchecking row 3 leaves {0, 6}.
    g.toggle_row_selection(3, false);
    assert_eq!(g.selected_rows_vec(), vec![0, 6]);

    // Ctrl+Up/Down moves cursor only without clearing {0, 6}.
    g.move_cursor_only(2);
    assert_eq!(g.selected_idx, 5);
    assert_eq!(g.selected_rows_vec(), vec![0, 6]);

    // 4. Snapshot reload with a new commit prepended at row 0 shifts {0, 6} -> {1, 7} by OID.
    let mut shifted = (*base_snap).clone();
    shifted.commits.insert(
        0,
        CommitSummary {
            oid: oid(99),
            parents: vec![oid(12)],
            subject: "new top commit".into(),
            timestamp: 9999,
            ..Default::default()
        },
    );
    shifted.head_oid = Some(oid(99));
    let shifted_snap = Arc::new(shifted);
    g.on_snapshot_loaded();
    g.ensure_cache(
        Some(&shifted_snap),
        &colors,
        &FilterInputs {
            spec: &LogFilterSpec::default(),
            path_matches: None,
            favorites: &[],
        },
    );
    assert_eq!(g.selected_rows_vec(), vec![1, 7]);
    assert_eq!(g.selected_oids(Some(&shifted_snap)), vec![oid(12), oid(6)]);
    assert_eq!(g.selected_idx, 6, "cursor OID shifted from row 5 to row 6");

    // 5. Esc collapses multi-selection back to single cursor row.
    g.clear_multi_selection();
    assert!(!g.has_multi_selection());
    assert_eq!(g.selected_rows_vec(), vec![6]);
    assert_eq!(g.selection_count(), 1);

    // 6. Keyboard Space mark mode: mark row 1, navigate down with plain scroll_by to row 4,
    //    mark row 4 -> {1, 4}, then unmark both -> returns to normal single mode.
    g.select_row(1);
    g.toggle_row_selection(1, true);
    assert!(g.is_mark_mode());
    assert_eq!(g.selected_rows_vec(), vec![1]);
    g.scroll_by(3, false);
    assert_eq!(g.selected_idx, 4);
    assert_eq!(g.selected_rows_vec(), vec![1], "mark mode preserves row 1 while moving cursor");
    g.toggle_row_selection(4, true);
    assert_eq!(g.selected_rows_vec(), vec![1, 4]);
    // Uncheck both rows cleanly exits mark mode.
    g.toggle_row_selection(4, true);
    g.toggle_row_selection(1, true);
    assert!(!g.is_mark_mode());
    assert!(!g.has_multi_selection());
    assert_eq!(g.selected_rows_vec(), vec![1]);
}

#[test]
fn non_contiguous_two_commit_compare_versions_and_swap_direction() {
    use std::process::Command;

    use jbgit_core::RequestSlotId;
    use jbgit_engine::{EngineBackend, EngineSupervisor, RepoLocation};

    // Build a real temporary 5-commit Git repository so we verify both the TUI session
    // and the real EngineSupervisor net diff between row 3 (c1, older) and row 0 (c4, newer).
    let dir = tempfile::tempdir().unwrap();
    let git = |args: &[&str]| {
        let out = Command::new("git")
            .current_dir(dir.path())
            .args(args)
            .env("GIT_AUTHOR_NAME", "Alice")
            .env("GIT_AUTHOR_EMAIL", "alice@example.com")
            .env("GIT_COMMITTER_NAME", "Alice")
            .env("GIT_COMMITTER_EMAIL", "alice@example.com")
            .output()
            .expect("git command failed");
        assert!(
            out.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };

    git(&["init", "-b", "main"]);
    let mut oids = Vec::new();
    let commits_spec = [
        ("c0", "file.txt", "line0\n"),
        ("c1", "file.txt", "line0\nline1\n"),
        ("c2", "file.txt", "line0\nline1\nline2\n"),
        ("c3", "file.txt", "line0\nline1\nline2\nline3\n"),
        ("c4", "file.txt", "line0\nline1\nline2\nline3\nline4\n"),
    ];
    for (msg, fname, content) in commits_spec {
        std::fs::write(dir.path().join(fname), content).unwrap();
        git(&["add", fname]);
        git(&["commit", "-m", msg]);
        let sha = git(&["rev-parse", "HEAD"]);
        oids.push(sha.parse::<Oid>().unwrap());
    }

    let rt = tokio::runtime::Runtime::new().unwrap();
    let loc = RepoLocation::discover(dir.path()).unwrap();
    let (engine, mut rx) = rt.block_on(async { EngineSupervisor::start(loc, false).unwrap() });

    let recv_event = |rx: &mut tokio::sync::mpsc::UnboundedReceiver<EngineEvent>,
                      pred: &dyn Fn(&EngineEvent) -> bool|
     -> EngineEvent {
        rt.block_on(async {
            tokio::time::timeout(Duration::from_secs(10), async {
                loop {
                    let ev = rx.recv().await.expect("engine channel closed");
                    if pred(&ev) {
                        return ev;
                    }
                }
            })
            .await
            .expect("timed out waiting for engine event")
        })
    };

    engine.dispatch(TaskRequest::LoadRepoSnapshot);
    let snap = match recv_event(&mut rx, &|e| {
        matches!(e, EngineEvent::RepoSnapshotLoaded { .. })
    }) {
        EngineEvent::RepoSnapshotLoaded { snapshot, .. } => snapshot,
        _ => unreachable!(),
    };
    assert_eq!(snap.commits.len(), 5);
    assert_eq!(snap.commits[0].oid, oids[4]);
    assert_eq!(snap.commits[3].oid, oids[1]);

    let mut ts = TestSession::with_snapshot((*snap).clone());
    ts.dispatch(Action::FocusPane(PaneId::LogCommitTable));
    let _ = ts.render(140, 35);

    // Select row 0 (c4) and row 3 (c1), skipping rows 1 and 2, via keyboard Space + j + Space.
    ts.send_key(KeyCode::Char(' '), KeyModifiers::NONE);
    ts.send_key(KeyCode::Char('j'), KeyModifiers::NONE);
    ts.send_key(KeyCode::Char('j'), KeyModifiers::NONE);
    ts.send_key(KeyCode::Char('j'), KeyModifiers::NONE);
    ts.send_key(KeyCode::Char(' '), KeyModifiers::NONE);

    assert_eq!(ts.state().log.graph.selected_rows_vec(), vec![0, 3]);
    assert_eq!(
        ts.state().selected_commits_oldest_first(),
        vec![oids[1], oids[4]],
        "oldest-first must order row 3 (c1) before row 0 (c4)"
    );

    // Rendered table shows "2 selected" and `◆` markers on selected rows.
    let buf = ts.render(140, 35);
    let text = screen_text(&buf);
    assert!(text.contains("2 selected"), "{text}");
    assert!(text.contains('◆'), "{text}");

    // Right-clicking selected row 3 preserves the non-contiguous selection & enables Compare Versions.
    let (rx_pos, ry_pos) = ts
        .hit_center_of(|t| matches!(t, HitTarget::CommitTableRow { visible_row: 3, .. }))
        .expect("row 3 hit region");
    ts.send_mouse_event(right_click(rx_pos, ry_pos));
    assert_eq!(ts.state().log.graph.selected_rows_vec(), vec![0, 3]);
    let items = match ts.state().modal_stack.last() {
        Some(ModalLayer::ContextMenu { items, .. }) => items.clone(),
        other => panic!("expected context menu, got {other:?}"),
    };
    let cmp_item = find_item(&items, "Compare Versions");
    assert!(
        cmp_item.disabled_reason.is_none(),
        "Compare Versions must be enabled for 2 non-contiguous commits"
    );
    ts.send_key(KeyCode::Esc, KeyModifiers::NONE); // close menu

    // Trigger Compare Versions via Ctrl+D.
    let _ = ts.take_requests();
    ts.send_key(KeyCode::Char('d'), KeyModifiers::CONTROL);
    let reqs = ts.take_requests();
    let cmp_key = reqs
        .iter()
        .find_map(|(r, _)| match r {
            TaskRequest::LoadCommitDetails { key, .. }
                if key.mode == DetailsMode::CompareVersions =>
            {
                Some(key.clone())
            }
            _ => None,
        })
        .expect("CompareVersions request emitted");
    assert_eq!(cmp_key.oids, vec![oids[1], oids[4]]);

    // Execute the real EngineSupervisor diff on that exact DetailsKey and verify net diff c1 -> c4.
    engine.dispatch(TaskRequest::LoadCommitDetails {
        slot: RequestSlotId::LogCommitDetails,
        key: cmp_key.clone(),
    });
    let bundle = match recv_event(&mut rx, &|e| {
        matches!(e, EngineEvent::CommitDetailsLoaded { bundle, .. } if bundle.key == cmp_key)
    }) {
        EngineEvent::CommitDetailsLoaded { bundle, .. } => bundle,
        _ => unreachable!(),
    };
    assert_eq!(bundle.changes.len(), 1);
    assert_eq!(bundle.changes[0].path, "file.txt");
    assert_eq!(bundle.changes[0].additions, 3);
    assert_eq!(bundle.changes[0].deletions, 0);

    let file_diff_key = jbgit_core::FileDiffKey {
        details: cmp_key.clone(),
        path: "file.txt".into(),
        old_path: None,
        context: jbgit_core::DiffContext::Three,
        whitespace: jbgit_core::WhitespaceMode::Default,
    };
    engine.dispatch(TaskRequest::LoadFileDiff {
        slot: RequestSlotId::LogFileDiff,
        key: file_diff_key.clone(),
        known_oids: None,
    });
    let fdiff = match recv_event(&mut rx, &|e| {
        matches!(e, EngineEvent::FileDiffLoaded { diff, .. } if diff.key == file_diff_key)
    }) {
        EngineEvent::FileDiffLoaded { diff, .. } => diff,
        _ => unreachable!(),
    };
    let added: Vec<&str> = fdiff
        .hunks
        .iter()
        .flat_map(|h| h.lines.iter())
        .filter(|l| l.kind == jbgit_core::DiffLineKind::Added)
        .map(|l| l.text.as_str())
        .collect();
    assert_eq!(added, vec!["line2", "line3", "line4"]);

    // Deliver bundle to TUI and verify Swap Comparison Direction via `X` reverses base <-> target.
    ts.deliver_engine_event(EngineEvent::CommitDetailsLoaded {
        slot: RequestSlotId::LogCommitDetails,
        bundle,
        epoch: ts
            .state()
            .request_slots
            .latest(RequestSlotId::LogCommitDetails),
    });
    let _ = ts.render(140, 35);
    assert!(
        ts.hit_center_of(|t| matches!(t, HitTarget::ChangesToolbar(ChangesToolbarButton::SwapCompare)))
            .is_some(),
        "Changes toolbar should show Swap button in CompareVersions mode"
    );
    assert!(
        ts.hit_center_of(|t| matches!(t, HitTarget::DiffToolbar(DiffToolbarButton::SwapCompare)))
            .is_some(),
        "Diff toolbar should show Swap button in CompareVersions mode"
    );

    let _ = ts.take_requests();
    ts.send_key(KeyCode::Char('X'), KeyModifiers::SHIFT);
    let swap_reqs = ts.take_requests();
    assert!(
        swap_reqs.iter().any(|(r, _)| matches!(
            r,
            TaskRequest::LoadCommitDetails { key, .. }
                if key.mode == DetailsMode::CompareVersions && key.oids == vec![oids[4], oids[1]]
        )),
        "X must swap CompareVersions direction to [c4, c1]: {swap_reqs:?}"
    );

    // Verify swapped direction in real engine produces 3 deletions and 0 additions.
    let swapped_key = DetailsKey::compare_versions(oids[4], oids[1]);
    engine.dispatch(TaskRequest::LoadCommitDetails {
        slot: RequestSlotId::LogCommitDetails,
        key: swapped_key.clone(),
    });
    let swapped_bundle = match recv_event(&mut rx, &|e| {
        matches!(e, EngineEvent::CommitDetailsLoaded { bundle, .. } if bundle.key == swapped_key)
    }) {
        EngineEvent::CommitDetailsLoaded { bundle, .. } => bundle,
        _ => unreachable!(),
    };
    assert_eq!(swapped_bundle.changes[0].additions, 0);
    assert_eq!(swapped_bundle.changes[0].deletions, 3);

    // Ctrl+Click also toggles non-contiguous rows and Esc collapses back to cursor row.
    ts.dispatch(Action::FocusPane(PaneId::LogCommitTable));
    ts.dispatch(Action::SelectLogRow(0));
    let _ = ts.render(140, 35);
    let (cx, cy) = ts
        .hit_center_of(|t| matches!(t, HitTarget::CommitTableRow { visible_row: 3, .. }))
        .expect("row 3 hit region");
    ts.send_mouse_event(left_click(cx, cy, KeyModifiers::CONTROL));
    assert_eq!(ts.state().log.graph.selected_rows_vec(), vec![0, 3]);
    ts.send_key(KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(ts.state().log.graph.selected_rows_vec(), vec![3]);
    assert!(!ts.state().log.graph.has_multi_selection());
}

#[test]
fn many_changes_guard_blocks_over_limit_and_loads_on_enter() {
    let total = MAX_AGGREGATED_COMMITS + 15;
    let mut commits = Vec::with_capacity(total);
    for i in (0..total).rev() {
        let mut bytes = [0u8; 20];
        bytes[..8].copy_from_slice(&(i as u64 + 1).to_le_bytes());
        let id = Oid::from_bytes(bytes);
        let parent = if i > 0 {
            let mut pb = [0u8; 20];
            pb[..8].copy_from_slice(&(i as u64).to_le_bytes());
            vec![Oid::from_bytes(pb)]
        } else {
            vec![]
        };
        commits.push(CommitSummary {
            oid: id,
            parents: parent,
            author_name: "Alice".into(),
            author_email: "alice@example.com".into(),
            timestamp: 1_700_000_000 + (i as i64) * 60,
            author_timestamp: 1_700_000_000 + (i as i64) * 60,
            subject: format!("commit {i}"),
            body: String::new(),
        });
    }
    let snap = make_snapshot(std::path::PathBuf::new(), commits);
    let mut ts = TestSession::with_snapshot((*snap).clone());
    ts.dispatch(Action::FocusPane(PaneId::LogCommitTable));
    let _ = ts.render(140, 35);
    let _ = ts.take_requests();

    // Selecting exactly MAX_AGGREGATED_COMMITS (100) commits loads normally without blocking.
    ts.dispatch(Action::SelectLogRow(0));
    ts.dispatch(Action::ExtendLogSelectionTo(MAX_AGGREGATED_COMMITS - 1));
    assert!(ts.state().log.details.many_changes_blocked.is_none());
    ts.advance_by(DETAILS_DEBOUNCE + Duration::from_millis(5));
    assert!(ts.take_requests().iter().any(|(r, _)| matches!(
        r,
        TaskRequest::LoadCommitDetails { key, .. } if key.oids.len() == MAX_AGGREGATED_COMMITS
    )));

    // Selecting MAX_AGGREGATED_COMMITS + 5 (105) commits activates the ManyChanges guard.
    let over = MAX_AGGREGATED_COMMITS + 5;
    ts.dispatch(Action::SelectLogRow(0));
    ts.dispatch(Action::ExtendLogSelectionTo(over - 1));
    ts.advance_by(DETAILS_DEBOUNCE + Duration::from_millis(20));
    let blocked_reqs = ts.take_requests();
    assert!(
        !blocked_reqs
            .iter()
            .any(|(r, _)| matches!(r, TaskRequest::LoadCommitDetails { .. })),
        "selection above MAX_AGGREGATED_COMMITS must NOT auto-load details"
    );
    assert_eq!(
        ts.state()
            .log
            .details
            .many_changes_blocked
            .as_ref()
            .map(|k| k.oids.len()),
        Some(over)
    );

    let buf = ts.render(140, 35);
    let text = screen_text(&buf);
    assert!(
        text.contains(&format!("{over} commits (>{MAX_AGGREGATED_COMMITS})")),
        "{text}"
    );
    assert!(
        text.contains(&format!("{over} commits selected (limit {MAX_AGGREGATED_COMMITS})"))
            && text.contains("Enter or click to load aggregated changes"),
        "{text}"
    );
    assert!(
        ts.hit_center_of(|t| matches!(t, HitTarget::ChangesLoadMany))
            .is_some()
    );

    // Pressing Enter confirms and immediately requests aggregated changes.
    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);
    let confirmed_reqs = ts.take_requests();
    assert!(
        confirmed_reqs.iter().any(|(r, _)| matches!(
            r,
            TaskRequest::LoadCommitDetails { key, .. } if key.oids.len() == over
        )),
        "Enter on ManyChanges guard must trigger LoadCommitDetails: {confirmed_reqs:?}"
    );
    assert!(ts.state().log.details.many_changes_blocked.is_none());
}

#[test]
fn show_only_affected_changes_filters_tree_and_toggles_with_badge_key_and_config() {
    let c1 = make_commit(1, vec![], "touch a and b");
    let snap = make_snapshot(std::path::PathBuf::new(), vec![c1.clone()]);
    let mut ts = TestSession::with_snapshot((*snap).clone());
    assert!(ts.state().log.presentation.show_only_affected_changes);

    let key = DetailsKey::single(c1.oid);
    let bundle = Arc::new(CommitChangesBundle {
        key: key.clone(),
        details: vec![make_details(c1.oid, vec![], "touch a and b")],
        changes: vec![
            FileChange {
                path: "src/a.rs".into(),
                old_path: None,
                kind: FileStatusKind::Modified,
                additions: 4,
                deletions: 1,
                is_binary: false,
                exists_in_workdir: true,
                old_oid: None,
                new_oid: None,
            },
            FileChange {
                path: "src/b.rs".into(),
                old_path: None,
                kind: FileStatusKind::Added,
                additions: 10,
                deletions: 0,
                is_binary: false,
                exists_in_workdir: true,
                old_oid: None,
                new_oid: None,
            },
        ],
    });
    let _ = ts.render(140, 35);
    ts.deliver_engine_event(EngineEvent::CommitDetailsLoaded {
        slot: jbgit_core::RequestSlotId::LogCommitDetails,
        bundle,
        epoch: ts
            .state()
            .request_slots
            .latest(jbgit_core::RequestSlotId::LogCommitDetails),
    });

    // Without PathFilter: both files are visible.
    let rows_all = ts.state().log.details.change_rows(&ts.state().diff_caches);
    let file_paths = |rows: &[jbgit_core::ChangeTreeRow]| -> Vec<String> {
        rows.iter()
            .filter(|r| matches!(r.kind, ChangeRowKind::File { .. }))
            .map(|r| r.key.clone())
            .collect()
    };
    assert_eq!(file_paths(&rows_all), vec!["src/a.rs", "src/b.rs"]);

    // Activate PathFilter("src/a.rs") -> with Show Only Affected Changes ON, only src/a.rs is shown.
    ts.dispatch(Action::FilterLogByPath("src/a.rs".into()));
    let rows_filtered = ts.state().log.details.change_rows(&ts.state().diff_caches);
    assert_eq!(file_paths(&rows_filtered), vec!["src/a.rs"]);
    assert_eq!(
        ts.state()
            .log
            .details
            .changes
            .visible_change_counts(&ts.state().diff_caches),
        Some((1, 2))
    );

    let buf = ts.render(140, 35);
    let txt = screen_text(&buf);
    assert!(txt.contains("[Filtered: 1/2]"), "{txt}");
    assert!(txt.contains("a.rs"), "{txt}");
    assert!(!txt.contains("b.rs"), "b.rs must be filtered out: {txt}");

    // Clicking the [Filtered: 1/2] badge toggles Show Only Affected Changes OFF -> both files show.
    let (bx, by) = ts
        .hit_center_of(|t| matches!(t, HitTarget::ChangesFilteredBadge))
        .expect("ChangesFilteredBadge hit target");
    ts.send_mouse_event(left_click(bx, by, KeyModifiers::NONE));
    assert!(!ts.state().log.presentation.show_only_affected_changes);
    let rows_unfiltered = ts.state().log.details.change_rows(&ts.state().diff_caches);
    assert_eq!(file_paths(&rows_unfiltered), vec!["src/a.rs", "src/b.rs"]);
    let buf_off = ts.render(140, 35);
    let txt_off = screen_text(&buf_off);
    assert!(!txt_off.contains("[Filtered: 1/2]"), "{txt_off}");
    assert!(txt_off.contains("a.rs") && txt_off.contains("b.rs"), "{txt_off}");

    // Pressing `a` toggles Show Only Affected Changes back ON.
    ts.dispatch(Action::FocusPane(PaneId::LogChangesTree));
    ts.send_key(KeyCode::Char('a'), KeyModifiers::NONE);
    assert!(ts.state().log.presentation.show_only_affected_changes);
    assert_eq!(
        file_paths(&ts.state().log.details.change_rows(&ts.state().diff_caches)),
        vec!["src/a.rs"]
    );

    // PathFilter matching zero files in the commit shows clickable empty-filtered hint.
    ts.dispatch(Action::FilterLogByPath("docs/other.md".into()));
    let buf_empty = ts.render(140, 35);
    let txt_empty = screen_text(&buf_empty);
    assert!(txt_empty.contains("[Filtered: 0/2]"), "{txt_empty}");
    assert!(
        txt_empty.contains("No changes match the active path filter")
            && txt_empty.contains("hidden)"),
        "{txt_empty}"
    );

    // Verify jbgit.toml `[log]` persistence config overrides default to false.
    let mut cfg = AppConfig::load_defaults();
    assert!(cfg.jbgit.log.show_only_affected_changes);
    let ov: JbgitConfigOverlay =
        toml::from_str("[log]\nshow_only_affected_changes = false\n").unwrap();
    cfg.jbgit.merge_overlay(&ov);
    assert!(!cfg.jbgit.log.show_only_affected_changes);
    let ts_cfg = TestSession::with_config(std::path::PathBuf::new(), cfg);
    assert!(!ts_cfg.state().log.presentation.show_only_affected_changes);
    assert_eq!(PathFilter::single("src/a.rs").paths, vec!["src/a.rs"]);
}

#[test]
fn issue03_new_log_shortcuts_resolve_without_shadowing_across_all_presets() {
    for preset in [
        KeymapPreset::Default,
        KeymapPreset::Jetbrains,
        KeymapPreset::Vim,
    ] {
        let mut cfg = AppConfig::load_defaults();
        cfg.keymap = KeymapConfig::for_preset(preset.clone());
        let dir = tempfile::tempdir().unwrap();
        let mut ts = TestSession::with_config(dir.path().to_path_buf(), cfg);
        ts.deliver_engine_event(EngineEvent::RepoSnapshotLoaded {
            snapshot: Arc::new(snapshot()),
            epoch: 1,
        });

        // 1. Space in LogCommitTable -> ToggleLogRowSelection; in LogBranchesTree -> ToggleFavorite.
        ts.dispatch(Action::SwitchTab(WorkspaceTab::Log));
        ts.dispatch(Action::FocusPane(PaneId::LogCommitTable));
        let space = KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE);
        let m_table = check_scopes(&[space], ts.state());
        assert!(
            matches!(m_table, crate::keymap::MatchResult::Exact(b) if b.run == KeyAction::ToggleLogRowSelection),
            "Space must resolve to ToggleLogRowSelection on {preset:?}"
        );
        assert_eq!(
            catalog::shortcut_for_action(
                ts.state(),
                &crate::keymap::actions::scopes_for_context(ts.state()),
                &KeyAction::ToggleLogRowSelection,
            )
            .as_deref(),
            Some("Space"),
            "ToggleLogRowSelection must not be shadowed on {preset:?}"
        );

        ts.dispatch(Action::FocusPane(PaneId::LogBranchesTree));
        let m_branch = check_scopes(&[space], ts.state());
        assert!(
            matches!(m_branch, crate::keymap::MatchResult::Exact(b) if b.run == KeyAction::ToggleFavorite),
            "Space in LogBranchesTree must remain ToggleFavorite on {preset:?}"
        );

        // 2. `a` in LogCommitTable / LogChangesTree / LogDetailsDiff -> ToggleOnlyAffectedChanges.
        let key_a = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE);
        for pane in [
            PaneId::LogCommitTable,
            PaneId::LogChangesTree,
            PaneId::LogDetailsDiff,
        ] {
            ts.dispatch(Action::FocusPane(pane));
            let m = check_scopes(&[key_a], ts.state());
            assert!(
                matches!(m, crate::keymap::MatchResult::Exact(b) if b.run == KeyAction::ToggleOnlyAffectedChanges),
                "`a` must resolve to ToggleOnlyAffectedChanges on {preset:?} / {pane:?}"
            );
            assert_eq!(
                catalog::shortcut_for_action(
                    ts.state(),
                    &crate::keymap::actions::scopes_for_context(ts.state()),
                    &KeyAction::ToggleOnlyAffectedChanges,
                )
                .as_deref(),
                Some("a"),
                "ToggleOnlyAffectedChanges must not be shadowed on {preset:?} / {pane:?}"
            );
        }

        // 3. `X` when CompareVersions is active -> SwapCompareDirection across panes.
        ts.dispatch(Action::CompareVersions {
            base: oid(1),
            target: oid(5),
        });
        let key_x = KeyEvent::new(KeyCode::Char('X'), KeyModifiers::SHIFT);
        for pane in [
            PaneId::LogCommitTable,
            PaneId::LogChangesTree,
            PaneId::LogDetailsDiff,
        ] {
            ts.dispatch(Action::FocusPane(pane));
            let m = check_scopes(&[key_x], ts.state());
            assert!(
                matches!(m, crate::keymap::MatchResult::Exact(b) if b.run == KeyAction::SwapCompareDirection),
                "`X` must resolve to SwapCompareDirection on {preset:?} / {pane:?}"
            );
            assert_eq!(
                catalog::shortcut_for_action(
                    ts.state(),
                    &crate::keymap::actions::scopes_for_context(ts.state()),
                    &KeyAction::SwapCompareDirection,
                )
                .as_deref(),
                Some("X"),
                "SwapCompareDirection must not be shadowed on {preset:?} / {pane:?}"
            );
        }
    }
}
