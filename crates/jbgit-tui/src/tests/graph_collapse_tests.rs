//! Linear-branch collapsing in the Log graph (Spec 12 #12): collapse / expand
//! all, single segments via `zc` / `zo`, fold-marker clicks, disabled states
//! and selection preservation.

use std::sync::Arc;

use crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEventKind};
use jbgit_core::graph::CollapseMode;
use jbgit_core::{CommitSummary, Oid, PaneId, RefInfo, RefKind, RepoSnapshot, WorkspaceTab};
use jbgit_engine::EngineEvent;

use crate::app::Action;
use crate::config::{KeymapConfig, KeymapPreset};
use crate::hit_test::{GraphHitKind, HitTarget};
use crate::log_view::{FilterField, PresentationToggle};

use super::{TestSession, find_item, local, oid, row_text};

fn commit(n: u8, parents: &[u8]) -> CommitSummary {
    CommitSummary {
        oid: oid(n),
        parents: parents.iter().map(|&p| oid(p)).collect(),
        author_name: "Alice".into(),
        author_email: "alice@x.io".into(),
        timestamp: i64::from(n) * 100,
        author_timestamp: i64::from(n) * 100,
        subject: format!("c{n}"),
        body: String::new(),
    }
}

fn tag(name: &str, target: u8) -> RefInfo {
    RefInfo {
        name: name.into(),
        full_name: format!("refs/tags/{name}"),
        target_oid: oid(target),
        kind: RefKind::Tag,
        upstream: None,
        ahead_behind: None,
        is_head: false,
    }
}

/// `main` (HEAD) = 20, a merge of the mainline 19 → 18 → 17 and the side
/// chain 14 → 13 → 12, both forked from tag `v1` = 10; below it the linear
/// chain 9 → 8 → 7 → 6 down to the root 1.
///
/// Segments: {19,18,17} and {14,13,12} (20 → 10), {9,8,7,6} (10 → 1).
fn diamond(extra_head: bool) -> RepoSnapshot {
    let mut commits = vec![
        commit(20, &[19, 14]),
        commit(19, &[18]),
        commit(18, &[17]),
        commit(17, &[10]),
        commit(14, &[13]),
        commit(13, &[12]),
        commit(12, &[10]),
        commit(10, &[9]),
        commit(9, &[8]),
        commit(8, &[7]),
        commit(7, &[6]),
        commit(6, &[1]),
        commit(1, &[]),
    ];
    let head = if extra_head {
        commits.insert(0, commit(21, &[20]));
        21
    } else {
        20
    };
    let mut main = local("main", head);
    main.is_head = true;
    RepoSnapshot {
        repo_name: "demo".into(),
        head_branch: Some("main".into()),
        head_oid: Some(oid(head)),
        refs: vec![main, tag("v1", 10)],
        commits: commits.into(),
        ..Default::default()
    }
}

fn log_session(preset: KeymapPreset) -> TestSession {
    let mut ts = TestSession::with_snapshot(diamond(false));
    ts.state_mut().config.keymap = KeymapConfig::for_preset(preset);
    ts.dispatch(Action::SwitchTab(WorkspaceTab::Log));
    ts.dispatch(Action::FocusPane(PaneId::LogCommitTable));
    ts
}

fn visible_count(ts: &TestSession) -> usize {
    ts.state().log.graph.visible_count()
}

fn selected(ts: &TestSession) -> Option<Oid> {
    ts.state()
        .log
        .graph
        .selected_oid(ts.state().repo_snapshot.as_ref())
}

fn select(ts: &mut TestSession, n: u8) {
    let row = ts
        .state()
        .log
        .graph
        .cached
        .as_ref()
        .and_then(|c| c.row_of_oid(&oid(n)))
        .expect("commit visible");
    ts.dispatch(Action::SelectLogRow(row));
}

fn ctrl_shift(c: char) -> (KeyCode, KeyModifiers) {
    (
        KeyCode::Char(c),
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
    )
}

fn keys(ts: &mut TestSession, seq: &str) {
    for c in seq.chars() {
        let mods = if c.is_ascii_uppercase() {
            KeyModifiers::SHIFT
        } else {
            KeyModifiers::NONE
        };
        ts.send_key(KeyCode::Char(c), mods);
    }
}

#[test]
fn collapse_all_folds_linear_chains_and_keeps_refs_head_and_selection() {
    let mut ts = log_session(KeymapPreset::Jetbrains);
    assert_eq!(visible_count(&ts), 13);
    select(&mut ts, 20);

    let (code, mods) = ctrl_shift('-');
    ts.send_key(code, mods);
    assert_eq!(ts.state().log.graph.collapse.mode, CollapseMode::All);
    assert_eq!(
        visible_count(&ts),
        3,
        "only HEAD, tag v1 and the root remain"
    );
    assert_eq!(selected(&ts), Some(oid(20)));
    let notice = ts.state().status_notice.clone().unwrap_or_default();
    assert!(notice.contains("10 commit"), "{notice}");

    // Fold markers: hollow nodes on the up ends of collapsed segments.
    let buf = ts.render(120, 24);
    let markers = (0..buf.area.height)
        .filter(|&y| row_text(&buf, y).contains('○'))
        .count();
    assert_eq!(markers, 2, "HEAD-row merge and tag row carry fold markers");

    let (code, mods) = ctrl_shift('=');
    ts.send_key(code, mods);
    assert_eq!(ts.state().log.graph.collapse.mode, CollapseMode::None);
    assert_eq!(visible_count(&ts), 13);
    assert_eq!(selected(&ts), Some(oid(20)));
}

#[test]
fn collapse_all_keeps_the_selected_commits_segment_open() {
    let mut ts = log_session(KeymapPreset::Default);
    select(&mut ts, 13);
    ts.dispatch(Action::GraphCollapseAll);
    assert_eq!(selected(&ts), Some(oid(13)));
    // The side chain stays open; mainline and the bottom chain fold.
    assert_eq!(visible_count(&ts), 6);
}

#[test]
fn vim_z_chords_fold_and_unfold_segments() {
    let mut ts = log_session(KeymapPreset::Vim);
    select(&mut ts, 18);

    keys(&mut ts, "zc");
    assert_eq!(visible_count(&ts), 10, "mainline chain folded");
    assert_eq!(
        selected(&ts),
        Some(oid(20)),
        "cursor moves to the fold's up end"
    );
    assert_eq!(ts.state().log.graph.collapse.mode, CollapseMode::Custom);

    // `zc` on a commit that is the up end of an expanded chain folds it.
    select(&mut ts, 10);
    keys(&mut ts, "zc");
    assert_eq!(visible_count(&ts), 6);
    assert_eq!(selected(&ts), Some(oid(10)));

    keys(&mut ts, "zo");
    assert_eq!(visible_count(&ts), 10);
    select(&mut ts, 20);
    keys(&mut ts, "zo");
    assert_eq!(visible_count(&ts), 13);
    assert_eq!(ts.state().log.graph.collapse.mode, CollapseMode::None);

    keys(&mut ts, "zM");
    assert_eq!(visible_count(&ts), 3);
    keys(&mut ts, "zR");
    assert_eq!(visible_count(&ts), 13);
}

#[test]
fn clicking_fold_marker_or_collapsed_edge_expands() {
    let mut ts = log_session(KeymapPreset::Default);
    ts.dispatch(Action::GraphCollapseAll);
    ts.render(120, 24);

    let (x, y) = ts
        .hit_center_of(|t| {
            matches!(
                t,
                HitTarget::GraphPrintElement {
                    visible_row: 0,
                    kind: GraphHitKind::Node {
                        collapsed_below: true,
                        ..
                    },
                    ..
                }
            )
        })
        .expect("fold marker on the HEAD row");
    ts.send_mouse(MouseEventKind::Down(MouseButton::Left), x, y);
    assert_eq!(visible_count(&ts), 9, "both arms below HEAD expanded");
    assert_eq!(selected(&ts), Some(oid(20)));

    // The tag → root chain is still collapsed: an edge click expands it.
    let cached = ts.state().log.graph.cached.as_ref().unwrap();
    let up = cached.row_of_oid(&oid(10)).unwrap();
    let down = cached.row_of_oid(&oid(1)).unwrap();
    ts.dispatch(Action::GraphExpandEdge {
        up_row: up,
        down_row: down,
    });
    assert_eq!(visible_count(&ts), 13);

    // Clicking an expanded segment edge collapses that segment, and clicking
    // the resulting collapsed edge expands it back.
    ts.dispatch(Action::GraphExpandEdge {
        up_row: 0,
        down_row: 1,
    });
    assert_eq!(visible_count(&ts), 10);
    let collapsed_down = ts
        .state()
        .log
        .graph
        .cached
        .as_ref()
        .unwrap()
        .row_of_oid(&oid(10))
        .unwrap();
    ts.dispatch(Action::GraphExpandEdge {
        up_row: 0,
        down_row: collapsed_down,
    });
    assert_eq!(visible_count(&ts), 13);

    // An edge outside any collapsible linear segment is a no-op for folding.
    let mut ts_extra = TestSession::with_snapshot(diamond(true));
    ts_extra.dispatch(Action::SwitchTab(WorkspaceTab::Log));
    let before = visible_count(&ts_extra);
    ts_extra.dispatch(Action::GraphExpandEdge {
        up_row: 0,
        down_row: 1,
    });
    assert_eq!(visible_count(&ts_extra), before);
}

#[test]
fn collapsing_is_disabled_while_filtering_or_first_parent_only() {
    let mut ts = log_session(KeymapPreset::Default);

    ts.dispatch(Action::TogglePresentation(
        PresentationToggle::FirstParentOnly,
    ));
    let items = crate::input::presentation_menu_items(ts.state());
    let item = find_item(&items, "Collapse Linear Branches");
    assert!(
        item.disabled_reason
            .as_deref()
            .is_some_and(|r| r.contains("First Parent")),
        "{item:?}"
    );
    ts.dispatch(Action::GraphCollapseAll);
    ts.dispatch(Action::TogglePresentation(
        PresentationToggle::FirstParentOnly,
    ));

    ts.dispatch(Action::ApplyFilterField {
        field: FilterField::User,
        expr: "Alice".into(),
    });
    let items = crate::input::presentation_menu_items(ts.state());
    let item = find_item(&items, "Collapse Linear Branches");
    assert!(
        item.disabled_reason
            .as_deref()
            .is_some_and(|r| r.contains("filter")),
        "{item:?}"
    );
    let (code, mods) = ctrl_shift('-');
    ts.send_key(code, mods);
    assert!(
        ts.state()
            .status_notice
            .as_deref()
            .is_some_and(|n| n.contains("filter"))
    );

    // The filter projection itself is not collapsed even with a stale mode.
    ts.state_mut().log.graph.collapse.collapse_all();
    ts.state_mut().log.graph.options.collapse = CollapseMode::All;
    ts.state_mut().log.graph.filter_generation += 1;
    ts.dispatch(Action::FocusPane(PaneId::LogCommitTable));
    let cached = ts.state().log.graph.cached.as_ref().unwrap();
    assert!(!cached.visible.collapse_applied);
}

#[test]
fn jumping_to_a_hidden_commit_expands_its_segment() {
    let mut ts = log_session(KeymapPreset::Default);
    ts.dispatch(Action::GraphCollapseAll);
    assert_eq!(visible_count(&ts), 3);

    ts.dispatch(Action::GoToRevision(oid(13).to_string()));
    assert_eq!(selected(&ts), Some(oid(13)));
    assert_eq!(visible_count(&ts), 6, "only the side chain was expanded");

    // Back navigation also reveals.
    ts.dispatch(Action::GoToRevision(oid(8).to_string()));
    assert_eq!(selected(&ts), Some(oid(8)));
    ts.dispatch(Action::GraphCollapseAll);
    assert_eq!(selected(&ts), Some(oid(8)));
}

#[test]
fn collapse_all_survives_snapshot_reloads() {
    let mut ts = log_session(KeymapPreset::Default);
    ts.dispatch(Action::GraphCollapseAll);
    assert_eq!(visible_count(&ts), 3);

    ts.deliver_engine_event(EngineEvent::RepoSnapshotLoaded {
        snapshot: Arc::new(diamond(true)),
        epoch: 2,
    });
    ts.render(120, 24);
    assert_eq!(ts.state().log.graph.collapse.mode, CollapseMode::All);
    // 21 (HEAD) → 20 is now a plain edge; 20 stays the fork's up end.
    assert_eq!(visible_count(&ts), 4);
}
