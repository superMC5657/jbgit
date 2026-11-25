//! Regression & TUI session tests for Spec 13 Issue 11:
//! VCS Log highlighters & auto-suppression, LinearBek, branch-only linear collapsing,
//! multi-parent / multi-child GoTo chooser, No-Merges, Speed Search, filter history,
//! Align Labels, Author/Date column toggles, clickable commit hashes, Maximize Commit Message,
//! and Git Reflog recovery modal.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEventKind};
use jbgit_core::{
    CherryPickedKey, CommitChangesBundle, CommitDetails, CommitSignature, CommitSummary,
    DetailsKey, FileChange, FileStatusKind, GitOp, PersonStamp, RefInfo, RefKind, ReflogActionKind,
    ReflogBundle, ReflogEntry, ReflogKey, RepoSnapshot, RequestSlotId, WorkspaceTab,
};
use jbgit_engine::{EngineEvent, TaskRequest};

use super::{TestSession, oid, screen_text};
use crate::app::{Action, ModalLayer};
use crate::config::{AppConfig, KeymapConfig, KeymapPreset, KeymapScope};
use crate::hit_test::HitTarget;
use crate::log_view::{FilterField, PresentationToggle, filter_suggestions_with_context};

fn make_commit(
    id: u8,
    parents: &[u8],
    author: &str,
    email: &str,
    ts: i64,
    subject: &str,
    body: &str,
) -> CommitSummary {
    CommitSummary {
        oid: oid(id),
        parents: parents.iter().copied().map(oid).collect(),
        author_name: author.to_string(),
        author_email: email.to_string(),
        author_timestamp: ts,
        timestamp: ts,
        subject: subject.to_string(),
        body: body.to_string(),
    }
}

fn branch(name: &str, target: u8, is_head: bool) -> RefInfo {
    RefInfo {
        name: name.to_string(),
        full_name: format!("refs/heads/{name}"),
        target_oid: oid(target),
        kind: RefKind::LocalBranch,
        upstream: None,
        ahead_behind: None,
        is_head,
    }
}

/// Multi-author merge + fork DAG:
/// - `10` (main, merge [7, 8], Alice, ts 100)
/// - `8`  (feat tip, Bob, ts 95) -> `9` (feat base, Bob, ts 90) -> `1`
/// - `7`  (main parent, Alice, ts 80) -> `6` -> `5` -> `4` -> `1`
/// - `1`  (root base, Alice, ts 10)
fn merge_and_fork_snapshot() -> RepoSnapshot {
    let ref_hash = oid(1).short();
    RepoSnapshot {
        repo_name: "demo".into(),
        head_branch: Some("main".into()),
        head_oid: Some(oid(10)),
        user_name: Some("Alice".into()),
        user_email: Some("alice@example.com".into()),
        refs: vec![branch("main", 10, true), branch("feat", 8, false)],
        commits: vec![
            make_commit(
                10,
                &[7, 8],
                "Alice",
                "alice@example.com",
                100,
                "Merge branch 'feat'",
                &format!("Integrates feature work built on {ref_hash}\n\nMore details line."),
            ),
            make_commit(
                8,
                &[9],
                "Bob",
                "bob@example.com",
                95,
                "feat: add widget",
                "",
            ),
            make_commit(9, &[1], "Bob", "bob@example.com", 90, "feat: scaffold", ""),
            make_commit(
                7,
                &[6],
                "Alice",
                "alice@example.com",
                80,
                "main: polish",
                "",
            ),
            make_commit(
                6,
                &[5],
                "Alice",
                "alice@example.com",
                70,
                "main: step 3",
                "",
            ),
            make_commit(
                5,
                &[4],
                "Alice",
                "alice@example.com",
                60,
                "main: step 2",
                "",
            ),
            make_commit(
                4,
                &[1],
                "Alice",
                "alice@example.com",
                50,
                "main: step 1",
                "",
            ),
            make_commit(
                1,
                &[],
                "Alice",
                "alice@example.com",
                10,
                "initial commit",
                "",
            ),
        ],
        ..Default::default()
    }
}

fn ready_session(snap: RepoSnapshot) -> TestSession {
    let mut ts = TestSession::with_snapshot(snap);
    ts.dispatch(Action::SwitchTab(WorkspaceTab::Log));
    let _ = ts.render(140, 36);
    ts
}

#[test]
fn reflog_modal_opens_previews_diff_and_creates_recovery_branch_on_historical_entry() {
    let mut ts = ready_session(merge_and_fork_snapshot());
    ts.take_requests();

    // Press `R` in Log commit table -> opens Reflog modal & requests Reflog on its own slot.
    ts.send_key(KeyCode::Char('R'), KeyModifiers::NONE);
    assert!(matches!(
        ts.state().modal_stack.last(),
        Some(ModalLayer::Reflog(_))
    ));
    let reqs = ts.take_requests();
    assert!(
        reqs.iter().any(|(r, _)| matches!(
            r,
            TaskRequest::LoadReflog { key } if key.ref_name == "HEAD"
        )),
        "opening Reflog must dispatch LoadReflog: {reqs:?}"
    );

    // Deliver reflog with reset entry at row 0 and dropped historical commit at row 1.
    let dropped_oid = oid(99);
    let epoch = ts.state().request_slots.latest(RequestSlotId::Reflog);
    let key = ReflogKey::head();
    ts.deliver_engine_event(EngineEvent::ReflogLoaded {
        bundle: Arc::new(ReflogBundle {
            key,
            entries: vec![
                ReflogEntry {
                    oid: oid(10),
                    selector: "HEAD@{0}".into(),
                    action: ReflogActionKind::Reset,
                    message: "reset: moving to HEAD~1".into(),
                    timestamp: 1_700_000_200,
                    author: "Alice".into(),
                },
                ReflogEntry {
                    oid: dropped_oid,
                    selector: "HEAD@{1}".into(),
                    action: ReflogActionKind::Commit,
                    message: "commit: lost feature work".into(),
                    timestamp: 1_700_000_100,
                    author: "Alice".into(),
                },
            ],
            has_more: true,
        }),
        epoch,
    });
    let buf = ts.render(140, 36);
    let frame_txt = screen_text(&buf);
    assert!(frame_txt.contains("HEAD@{0}"), "{frame_txt}");
    assert!(frame_txt.contains("[reset]"), "{frame_txt}");
    assert!(frame_txt.contains("HEAD@{1}"), "{frame_txt}");
    assert!(frame_txt.contains("[commit]"), "{frame_txt}");

    // Move down to the dropped historical entry and verify synced commit-details fetch on ReflogCommitDetails slot.
    ts.take_requests();
    ts.send_key(KeyCode::Char('j'), KeyModifiers::NONE);
    assert_eq!(ts.state().modal_stack.reflog().unwrap().selected, 1);
    let detail_reqs = ts.take_requests();
    assert!(
        detail_reqs.iter().any(|(r, _)| matches!(
            r,
            TaskRequest::LoadCommitDetails {
                slot: RequestSlotId::ReflogCommitDetails,
                key,
            } if *key == DetailsKey::single(dropped_oid)
        )),
        "selecting reflog row must request commit details on ReflogCommitDetails slot: {detail_reqs:?}"
    );

    // Press `m` -> Load More pagination dispatches LoadReflog with limit + 200.
    ts.send_key(KeyCode::Char('m'), KeyModifiers::NONE);
    let more_reqs = ts.take_requests();
    assert!(
        more_reqs
            .iter()
            .any(|(r, _)| matches!(r, TaskRequest::LoadReflog { key } if key.limit > 200)),
        "pressing m must paginate reflog: {more_reqs:?}"
    );

    // Press `n` -> opens New Branch dialog targeting `dropped_oid`, type name + Enter -> dispatches CheckoutNewBranch / CreateBranch!
    ts.send_key(KeyCode::Char('n'), KeyModifiers::NONE);
    assert!(matches!(
        ts.state().modal_stack.last(),
        Some(ModalLayer::Dialog(_))
    ));
    ts.type_str("recover-lost");
    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);
    let git_ops = ts.take_git_ops();
    assert!(
        git_ops.iter().any(|op| matches!(
            op,
            GitOp::CheckoutNewBranch { name, start, .. } | GitOp::CreateBranch { name, start, .. }
                if name == "recover-lost" && start == &dropped_oid.to_string()
        )),
        "creating recovery branch from Reflog must target dropped OID: {git_ops:?}"
    );
}

#[test]
fn go_to_parent_and_go_to_child_open_numbered_picker_when_multiple_targets_exist() {
    let mut ts = ready_session(merge_and_fork_snapshot());
    // Row 0 is merge commit 10 with parents [7, 8].
    assert_eq!(
        ts.state()
            .log_commit_at(ts.state().log.graph.selected_idx)
            .unwrap()
            .oid,
        oid(10)
    );

    // Pressing Right (`GoToParentSmart`) on a 2-parent merge commit opens ChooseParent picker.
    ts.send_key(KeyCode::Right, KeyModifiers::NONE);
    assert!(
        matches!(
            ts.state().modal_stack.last(),
            Some(ModalLayer::ChooseParent { .. })
        ),
        "GoToParent on 2-parent merge must open ChooseParent modal"
    );
    let buf = ts.render(140, 36);
    let picker_txt = screen_text(&buf);
    assert!(picker_txt.contains(&oid(7).short()), "{picker_txt}");
    assert!(picker_txt.contains(&oid(8).short()), "{picker_txt}");
    assert!(picker_txt.contains("main: polish"), "{picker_txt}");
    assert!(picker_txt.contains("feat: add widget"), "{picker_txt}");

    // Press `2` -> jumps directly to 2nd parent (`oid(8)`).
    ts.send_key(KeyCode::Char('2'), KeyModifiers::NONE);
    assert!(ts.state().modal_stack.is_empty());
    assert_eq!(
        ts.state()
            .log_commit_at(ts.state().log.graph.selected_idx)
            .unwrap()
            .oid,
        oid(8)
    );

    // Now jump to base commit `oid(1)`, which has 2 visible children (`4` and `9`).
    ts.dispatch(Action::JumpToCommit(oid(1)));
    assert_eq!(
        ts.state()
            .log_commit_at(ts.state().log.graph.selected_idx)
            .unwrap()
            .oid,
        oid(1)
    );

    // Pressing Left (`GoToChild`) opens ChooseChild picker!
    ts.send_key(KeyCode::Left, KeyModifiers::NONE);
    assert!(
        matches!(
            ts.state().modal_stack.last(),
            Some(ModalLayer::ChooseChild { .. })
        ),
        "GoToChild on fork base with 2 visible children must open ChooseChild modal"
    );
    let buf = ts.render(140, 36);
    let child_txt = screen_text(&buf);
    assert!(child_txt.contains(&oid(4).short()), "{child_txt}");
    assert!(child_txt.contains(&oid(9).short()), "{child_txt}");

    // Pick candidate 2 -> closes modal and jumps to that child!
    let expected_second_child = ts
        .state()
        .log
        .graph
        .child_candidates(ts.state().repo_snapshot.as_ref())[1]
        .1;
    ts.send_key(KeyCode::Char('2'), KeyModifiers::NONE);
    assert!(ts.state().modal_stack.is_empty());
    assert_eq!(
        ts.state()
            .log_commit_at(ts.state().log.graph.selected_idx)
            .unwrap()
            .oid,
        expected_second_child
    );
}

#[test]
fn highlighters_auto_suppress_and_cherry_picked_loads_on_own_slot() {
    // 1. Multi-author repo: My Commits active, Current Branch active unfiltered.
    let mut ts = ready_session(merge_and_fork_snapshot());
    let snap = ts.state().repo_snapshot.clone().unwrap();
    assert!(ts.state().log.should_highlight_my_commits());
    assert!(ts.state().log.should_highlight_current_branch(&snap));

    // Filter strictly to current branch (`main`) -> Current Branch highlighter auto-suppresses!
    ts.dispatch(Action::ApplyFilterField {
        field: FilterField::Branch,
        expr: "main".into(),
    });
    assert!(
        !ts.state().log.should_highlight_current_branch(&snap),
        "Current Branch highlighter must auto-suppress when filtering solely to current branch"
    );

    // Filter strictly to `HEAD` -> also auto-suppressed!
    ts.dispatch(Action::ApplyFilterField {
        field: FilterField::Branch,
        expr: "HEAD".into(),
    });
    assert!(!ts.state().log.should_highlight_current_branch(&snap));

    // Filter to single non-current branch `feat` -> Current Branch re-enables AND triggers async LoadCherryPicked!
    ts.take_requests();
    ts.dispatch(Action::ApplyFilterField {
        field: FilterField::Branch,
        expr: "feat".into(),
    });
    assert!(ts.state().log.should_highlight_current_branch(&snap));
    let reqs = ts.take_requests();
    let expected_key = CherryPickedKey {
        head_oid: oid(10),
        branch: "feat".into(),
    };
    assert!(
        reqs.iter().any(
            |(r, _)| matches!(r, TaskRequest::LoadCherryPicked { key } if *key == expected_key)
        ),
        "filtering to single non-current branch must dispatch LoadCherryPicked: {reqs:?}"
    );

    // Deliver CherryPickedLoaded -> marks oid(8) as already cherry-picked.
    let cp_epoch = ts
        .state()
        .request_slots
        .latest(RequestSlotId::LogCherryPicked);
    let mut picked = HashSet::new();
    picked.insert(oid(8));
    ts.deliver_engine_event(EngineEvent::CherryPickedLoaded {
        key: expected_key,
        oids: Arc::new(picked),
        epoch: cp_epoch,
    });
    assert!(ts.state().log.is_commit_cherry_picked(&snap, &oid(8)));
    assert!(!ts.state().log.is_commit_cherry_picked(&snap, &oid(9)));

    // Enable UserFilter(@me) -> My Commits auto-suppresses!
    ts.dispatch(Action::ApplyFilterField {
        field: FilterField::User,
        expr: "@me".into(),
    });
    assert!(
        !ts.state().log.should_highlight_my_commits(),
        "My Commits must auto-suppress when UserFilter(@me) is active"
    );

    // Single-author repo -> My Commits auto-suppresses even with empty UserFilter!
    let mut single_author_snap = merge_and_fork_snapshot();
    for c in &mut single_author_snap.commits {
        c.author_name = "Alice".into();
        c.author_email = "alice@example.com".into();
    }
    let ts_single = ready_session(single_author_snap);
    assert!(
        !ts_single.state().log.should_highlight_my_commits(),
        "My Commits must auto-suppress in a single-author repository"
    );
}

#[test]
fn linear_bek_and_branch_only_collapse_and_no_merges_work_in_session() {
    let mut ts = ready_session(merge_and_fork_snapshot());

    // Press `L` -> toggles LinearBek on and folds merge commit `oid(10)`.
    ts.send_key(KeyCode::Char('L'), KeyModifiers::NONE);
    assert!(ts.state().log.graph.options.linear_bek);
    assert_eq!(
        ts.state()
            .log
            .graph
            .cached
            .as_ref()
            .unwrap()
            .visible
            .bek_merges
            .len(),
        1
    );

    // Press `Enter` on merge row 0 -> expands that folded LinearBek merge!
    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);
    assert!(
        ts.state()
            .log
            .graph
            .cached
            .as_ref()
            .unwrap()
            .visible
            .bek_merges
            .is_empty(),
        "Enter on folded LinearBek merge must expand it"
    );

    // Disable LinearBek and apply pure BranchFilter("main") -> linear collapsing MUST stay enabled!
    ts.send_key(KeyCode::Char('L'), KeyModifiers::NONE);
    ts.dispatch(Action::ApplyFilterField {
        field: FilterField::Branch,
        expr: "main".into(),
    });
    assert!(
        ts.state()
            .log
            .graph
            .cached
            .as_ref()
            .unwrap()
            .visible
            .can_collapse,
        "linear collapsing must remain permitted when only BranchFilter is active"
    );
    let before = ts.state().log.graph.visible_count();
    ts.dispatch(Action::GraphCollapseAll);
    assert!(
        ts.state().log.graph.visible_count() < before,
        "CollapseAll must fold linear segments under pure BranchFilter"
    );

    // Press `M` -> enables `--no-merges`, hides merge commit 10, and disables collapsing.
    ts.send_key(KeyCode::Char('M'), KeyModifiers::NONE);
    assert!(ts.state().log.filter.spec.no_merges);
    assert!(
        !ts.state()
            .log
            .graph
            .cached
            .as_ref()
            .unwrap()
            .visible
            .can_collapse
    );
    assert!(
        ts.state()
            .log
            .graph
            .cached
            .as_ref()
            .unwrap()
            .row_of_oid(&oid(10))
            .is_none()
    );
}

#[test]
fn speed_search_filter_history_columns_body_hash_links_and_maximize_modal() {
    let mut ts = ready_session(merge_and_fork_snapshot());
    let total_rows = ts.state().log.graph.visible_count();
    assert_eq!(total_rows, 8);

    // 1. In-table Speed Search (`s`) highlights matches WITHOUT hiding non-matching commits!
    ts.send_key(KeyCode::Char('s'), KeyModifiers::NONE);
    ts.type_str("main: step");
    assert_eq!(
        ts.state().log.graph.visible_count(),
        total_rows,
        "Speed search must NEVER hide non-matching commits"
    );
    assert_eq!(ts.state().log.speed_search().matches.len(), 3);
    let first_match = ts.state().log.graph.selected_idx;
    let matches_before = ts.state().log.speed_search().matches.clone();
    ts.send_key(KeyCode::Enter, KeyModifiers::NONE); // confirm -> browse mode
    ts.send_key(KeyCode::Char('n'), KeyModifiers::NONE); // next match
    assert_ne!(ts.state().log.graph.selected_idx, first_match);
    ts.send_key(KeyCode::Char('N'), KeyModifiers::NONE); // prev match
    assert_eq!(ts.state().log.graph.selected_idx, first_match);

    // Toggling No-Merges (`M`) shifts visible rows (merge row 0 disappears); speed-search
    // matches must be recomputed immediately so highlights and `n`/`N` never reference stale rows.
    ts.send_key(KeyCode::Char('M'), KeyModifiers::NONE);
    let matches_after_no_merges = ts.state().log.speed_search().matches.clone();
    assert_eq!(matches_after_no_merges.len(), 3);
    assert_ne!(
        matches_after_no_merges, matches_before,
        "speed search row indices must shift when No-Merges removes row 0"
    );
    for &row in &matches_after_no_merges {
        let c = ts
            .state()
            .log_commit_at(row)
            .expect("refreshed speed-search row must stay in bounds");
        assert!(
            c.subject.to_lowercase().contains("main: step"),
            "refreshed speed-search row {row} must still match query 'main: step', got {:?}",
            c.subject
        );
    }
    ts.send_key(KeyCode::Char('n'), KeyModifiers::NONE);
    assert!(
        ts.state()
            .log_commit_at(ts.state().log.graph.selected_idx)
            .unwrap()
            .subject
            .to_lowercase()
            .contains("main: step")
    );
    // Restore No-Merges off and also check Collapse All refreshes matches cleanly.
    ts.send_key(KeyCode::Char('M'), KeyModifiers::NONE);
    ts.dispatch(Action::GraphCollapseAll);
    for &row in &ts.state().log.speed_search().matches {
        let c = ts
            .state()
            .log_commit_at(row)
            .expect("speed-search row must stay valid after collapse");
        assert!(c.subject.to_lowercase().contains("main: step"));
    }
    ts.dispatch(Action::GraphExpandAll);
    assert_eq!(ts.state().log.speed_search().matches, matches_before);

    ts.send_key(KeyCode::Esc, KeyModifiers::NONE); // close speed search
    assert!(!ts.state().log.speed_search().is_active());

    // 2. Recent 10 filter history & Favorites suggestion in Branch filter.
    for i in 0..12 {
        ts.dispatch(Action::ApplyFilterField {
            field: FilterField::Branch,
            expr: format!("branch-{i}"),
        });
    }
    let hist = ts.state().log.filter.history_for(FilterField::Branch);
    assert_eq!(hist.len(), 10);
    assert_eq!(hist[0], "branch-11");
    let sugg = filter_suggestions_with_context(
        FilterField::Branch,
        ts.state().repo_snapshot.as_deref(),
        "",
        true,
        hist,
    );
    assert!(sugg.contains(&"Favorites".to_string()));
    assert!(sugg.contains(&"branch-11".to_string()));
    ts.dispatch(Action::ClearAllFilters);

    // 3. Author/Date columns & Align Labels toggles + [log] config overlay round-trip.
    ts.dispatch(Action::TogglePresentation(
        PresentationToggle::ShowAuthorColumn,
    ));
    ts.dispatch(Action::TogglePresentation(
        PresentationToggle::ShowDateColumn,
    ));
    ts.dispatch(Action::TogglePresentation(
        PresentationToggle::AlignLabelsRight,
    ));
    assert!(!ts.state().log.presentation.show_author_column);
    assert!(!ts.state().log.presentation.show_date_column);
    assert!(ts.state().log.presentation.align_labels_right);
    let _ = ts.render(140, 36);

    let mut cfg = AppConfig::load_defaults();
    let overlay: crate::config::JbgitConfigOverlay = toml::from_str(
        "[log]\nhighlight_my_commits = false\nhighlight_cherry_picked = false\nshow_author_column = false\nshow_date_column = false\nalign_labels_right = true\nlinear_bek = true\n",
    )
    .unwrap();
    cfg.jbgit.merge_overlay(&overlay);
    assert!(!cfg.jbgit.log.highlight_my_commits);
    assert!(!cfg.jbgit.log.highlight_cherry_picked);
    assert!(!cfg.jbgit.log.show_author_column);
    assert!(!cfg.jbgit.log.show_date_column);
    assert!(cfg.jbgit.log.align_labels_right);
    assert!(cfg.jbgit.log.linear_bek);

    // 4. Commit Details clickable 7-char hash in body + Maximize Commit Message (`E`).
    ts.dispatch(Action::JumpToCommit(oid(10)));
    let details_key = DetailsKey::single(oid(10));
    let details_epoch = ts
        .state()
        .request_slots
        .latest(RequestSlotId::LogCommitDetails);
    let body_msg = format!(
        "Merge branch 'feat'\n\nIntegrates feature work built on {}\n\nMore details line.",
        oid(1).short()
    );
    ts.deliver_engine_event(EngineEvent::CommitDetailsLoaded {
        slot: RequestSlotId::LogCommitDetails,
        bundle: Arc::new(CommitChangesBundle {
            key: details_key,
            details: vec![CommitDetails {
                oid: oid(10),
                parents: vec![oid(7), oid(8)],
                author: PersonStamp {
                    name: "Alice".into(),
                    email: "alice@example.com".into(),
                    time: 100,
                    offset_minutes: 0,
                },
                committer: PersonStamp {
                    name: "Alice".into(),
                    email: "alice@example.com".into(),
                    time: 100,
                    offset_minutes: 0,
                },
                message: body_msg,
                signature: CommitSignature::default(),
            }],
            changes: vec![FileChange {
                path: "src/lib.rs".into(),
                old_path: None,
                kind: FileStatusKind::Modified,
                additions: 1,
                deletions: 0,
                is_binary: false,
                exists_in_workdir: true,
                old_oid: None,
                new_oid: None,
            }],
        }),
        epoch: details_epoch,
    });
    let _ = ts.render(140, 36);

    // Clickable commit hash in Details body jumps directly to `oid(1)`.
    let hash_pos = ts
        .hit_center_of(|t| matches!(t, HitTarget::DetailsCommitHash { oid: o } if *o == oid(1)))
        .expect("embedded 7-char commit hash in details body must be clickable");
    ts.send_mouse(
        MouseEventKind::Down(MouseButton::Left),
        hash_pos.0,
        hash_pos.1,
    );
    assert_eq!(
        ts.state()
            .log_commit_at(ts.state().log.graph.selected_idx)
            .unwrap()
            .oid,
        oid(1),
        "clicking embedded commit hash in details body must jump to that commit"
    );

    // Press `E` on oid(10) -> opens maximized Commit Message modal; clicking the embedded
    // commit hash inside the modal must BOTH close the modal AND jump to `oid(1)`.
    ts.dispatch(Action::JumpToCommit(oid(10)));
    ts.send_key(KeyCode::Char('E'), KeyModifiers::NONE);
    assert!(matches!(
        ts.state().modal_stack.last(),
        Some(ModalLayer::CommitMessage { .. })
    ));
    let _ = ts.render(140, 36);
    let modal_hash_pos = ts
        .hit_center_of(|t| matches!(t, HitTarget::DetailsCommitHash { oid: o } if *o == oid(1)))
        .expect("embedded 7-char commit hash inside CommitMessage modal must be clickable");
    ts.send_mouse(
        MouseEventKind::Down(MouseButton::Left),
        modal_hash_pos.0,
        modal_hash_pos.1,
    );
    assert!(
        ts.state().modal_stack.is_empty(),
        "clicking embedded commit hash inside CommitMessage modal must close the modal"
    );
    assert_eq!(
        ts.state()
            .log_commit_at(ts.state().log.graph.selected_idx)
            .unwrap()
            .oid,
        oid(1),
        "clicking embedded commit hash inside CommitMessage modal must jump to target commit"
    );

    // Press `E` again and verify Esc also closes the modal cleanly.
    ts.send_key(KeyCode::Char('E'), KeyModifiers::NONE);
    assert!(matches!(
        ts.state().modal_stack.last(),
        Some(ModalLayer::CommitMessage { .. })
    ));
    let _ = ts.render(140, 36);
    ts.send_key(KeyCode::Esc, KeyModifiers::NONE);
    assert!(ts.state().modal_stack.is_empty());
}

#[test]
fn new_issue_11_keybindings_have_zero_conflicts_across_all_three_presets() {
    let scopes = [
        KeymapScope::Global,
        KeymapScope::Log,
        KeymapScope::Branch,
        KeymapScope::Commit,
        KeymapScope::Diff,
        KeymapScope::Stash,
        KeymapScope::Console,
    ];
    for preset in [
        KeymapPreset::Default,
        KeymapPreset::Jetbrains,
        KeymapPreset::Vim,
    ] {
        let km = KeymapConfig::for_preset(preset.clone());
        let global_keys: HashMap<Vec<String>, crate::keymap::KeyAction> = km
            .bindings_for(KeymapScope::Global)
            .iter()
            .map(|b| (b.on.clone(), b.run.clone()))
            .collect();
        for scope in scopes {
            let mut seen: HashMap<Vec<String>, crate::keymap::KeyAction> = HashMap::new();
            for b in km.bindings_for(scope) {
                if let Some(prev) = seen.insert(b.on.clone(), b.run.clone()) {
                    assert_eq!(
                        prev, b.run,
                        "duplicate key {:?} in {preset:?}::{scope:?}: {prev:?} vs {:?}",
                        b.on, b.run
                    );
                }
                if scope != KeymapScope::Global
                    && let Some(g_run) = global_keys.get(&b.on)
                {
                    assert!(
                        !matches!(
                            b.run.as_str(),
                            "log_speed_search"
                                | "open_reflog"
                                | "toggle_no_merges"
                                | "toggle_linear_bek"
                                | "maximize_commit_message"
                        ),
                        "new Issue 11 key {:?} ({:?}) in {preset:?}::{scope:?} conflicts with global {g_run:?}",
                        b.on,
                        b.run
                    );
                }
            }
        }
        // Assert all Issue 11 bindings actually resolve to their expected KeyAction in all 3 presets.
        let log_map: HashMap<Vec<String>, crate::keymap::KeyAction> = km
            .bindings_for(KeymapScope::Log)
            .iter()
            .map(|b| (b.on.clone(), b.run.clone()))
            .collect();
        for (key, expected) in [
            ("s", "log_speed_search"),
            ("R", "open_reflog"),
            ("M", "toggle_no_merges"),
            ("L", "toggle_linear_bek"),
            ("E", "maximize_commit_message"),
        ] {
            assert_eq!(
                log_map.get(&vec![key.to_string()]).map(|a| a.as_str()),
                Some(expected),
                "Log key {key} must resolve to {expected} in {preset:?}"
            );
        }
        let branch_map: HashMap<Vec<String>, crate::keymap::KeyAction> = km
            .bindings_for(KeymapScope::Branch)
            .iter()
            .map(|b| (b.on.clone(), b.run.clone()))
            .collect();
        assert_eq!(
            branch_map.get(&vec!["R".to_string()]).map(|a| a.as_str()),
            Some("open_reflog"),
            "Branch key R must resolve to open_reflog in {preset:?}"
        );
    }
}
