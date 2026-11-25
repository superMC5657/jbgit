use std::sync::Arc;
use std::time::Duration;

use crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEventKind};
use jbgit_core::{
    CommitChangesBundle, CommitSummary, ConflictDocument, DetailsKey, DetailsMode, DiffContext,
    FileChange, FileStatusKind, GitOp, Oid, PaneId, StageEntry, StageStatusSnapshot, StashEntry,
    TodoAction, WorkspaceTab,
};
use jbgit_engine::{EngineEvent, StageDiffKey, StageDiffSide, TaskRequest};
use ratatui::layout::Rect;
use ratatui::style::Modifier;

use crate::app::{Action, AppState, ContextMenuEntryKind, ModalLayer};
use crate::dialogs::Dialog;
use crate::hit_test::HitTarget;
use crate::views::overlays::context_menu_rect;

use super::{
    TestSession, find_item, local, menu_item_idx, oid, row_text, screen_text, snapshot, top_menu,
};

#[test]
fn context_menu_and_submenu_flip_inside_viewport_at_bottom_right() {
    let mut ts = TestSession::with_snapshot(snapshot());
    let _ = ts.render(120, 40);

    // Right-clicking empty footer / chrome does NOT open a fallback menu.
    ts.send_mouse(MouseEventKind::Down(MouseButton::Right), 119, 39);
    assert!(ts.state().modal_stack.is_empty());

    // Push the center-right splitter right and shrink height so the bottom-most
    // CommitTableRow sits near the bottom-right corner of the center pane.
    ts.state_mut().splitters.update_from_pointer(
        jbgit_core::SplitterId::LogRightHorizontal,
        jbgit_core::SplitAxis::Horizontal,
        Rect::new(0, 0, 100, 20),
        99,
        5,
    );
    let _ = ts.render(100, 20);
    let last_row = ts.region_of(|t| matches!(t, HitTarget::CommitTableRow { .. }));
    ts.send_mouse(
        MouseEventKind::Down(MouseButton::Right),
        last_row.right().saturating_sub(1),
        last_row.y,
    );
    let (items, _, _) = top_menu(ts.state());
    let items = items.to_vec();
    let screen = ts.state().terminal_area;
    let Some(ModalLayer::ContextMenu { anchor, parent, .. }) = ts.state().modal_stack.last() else {
        panic!()
    };
    let rect = context_menu_rect(screen, *anchor, *parent, &items);
    assert!(rect.right() < screen.right(), "shadow column must fit");
    assert!(rect.bottom() < screen.bottom(), "shadow row must fit");
    assert!(
        rect.x < last_row.right().saturating_sub(1),
        "menu flipped left: {rect:?}"
    );

    let buf = ts.render(100, 20);
    let text = screen_text(&buf);
    assert!(text.contains("╭") && text.contains("╰"), "rounded border");

    // Cascading submenu (`Presentation` via 'o'): no room on the right → opens left of parent.
    ts.send_key(KeyCode::Char('o'), KeyModifiers::NONE);
    let (sub_items, _, depth) = top_menu(ts.state());
    assert_eq!(depth, 1);
    let sub_items = sub_items.to_vec();
    let Some(ModalLayer::ContextMenu { anchor, parent, .. }) = ts.state().modal_stack.last() else {
        panic!()
    };
    let sub = context_menu_rect(screen, *anchor, *parent, &sub_items);
    assert!(
        sub.right() <= rect.x,
        "submenu mirrored to the left: {sub:?} vs {rect:?}"
    );
    assert!(sub.bottom() < screen.bottom() && sub.right() < screen.right());
    let buf = ts.render(100, 20);
    let text = screen_text(&buf);
    assert!(text.contains("Compact References View"), "{text}");
}

#[test]
fn right_click_preselects_row_and_keeps_multi_selection_for_squash() {
    let mut ts = TestSession::with_snapshot(snapshot());
    let _ = ts.render(140, 40);
    // Row 8 (c4) is not selected: right-click selects it first.
    let r = ts.region_of(|t| matches!(t, HitTarget::CommitTableRow { visible_row: 8, .. }));
    ts.send_mouse(MouseEventKind::Down(MouseButton::Right), r.x + 30, r.y);
    assert_eq!(ts.state().log.graph.selected_idx, 8);
    let (items, _, _) = top_menu(ts.state());
    assert!(find_item(items, "Checkout Revision '").is_enabled());
    let squash = find_item(items, "Squash Commits");
    assert_eq!(
        squash.disabled_reason.as_deref(),
        Some("Select 2 or more commits to squash")
    );
    // Disabled reason is shown in the footer when focused.
    let idx = menu_item_idx(items, "Squash Commits");
    ts.dispatch(Action::ContextMenuSetSelected { depth: 0, idx });
    let buf = ts.render(140, 40);
    let footer = row_text(&buf, 39);
    assert!(
        footer.contains("Select 2 or more commits to squash"),
        "{footer}"
    );
    ts.dispatch(Action::PopContextMenus);

    // Multi-select rows 3..6, right-click row 5: selection is preserved.
    ts.dispatch_many([Action::SelectLogRow(3), Action::ExtendLogSelectionTo(6)]);
    let _ = ts.render(140, 40);
    let r = ts.region_of(|t| matches!(t, HitTarget::CommitTableRow { visible_row: 5, .. }));
    ts.send_mouse(MouseEventKind::Down(MouseButton::Right), r.x + 30, r.y);
    for row in 3..=6 {
        assert!(
            ts.state().log.graph.is_row_selected(row),
            "row {row} lost selection"
        );
    }
    let (items, _, _) = top_menu(ts.state());
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
    let _ = ts.take_requests();
    ts.send_key(KeyCode::Char('s'), KeyModifiers::NONE);
    assert!(ts.take_git_ops().is_empty());
    let Some(ModalLayer::Dialog(d)) = ts.state().modal_stack.last() else {
        panic!("squash message dialog");
    };
    let Dialog::Message(m) = d.as_ref() else {
        panic!()
    };
    assert_eq!(m.text, "c6\n\nc7\n\nc8\n\nc9");
    ts.send_key(KeyCode::Enter, KeyModifiers::ALT);
    let ops = ts.take_git_ops();
    let [GitOp::RewriteHistory { base, todo, .. }] = ops.as_slice() else {
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
    let mut ts = TestSession::with_snapshot(snapshot());
    ts.dispatch(Action::SelectLogRow(10)); // c2 == origin/main
    let items = crate::menus::log_commit_menu_items(ts.state());
    let reason = find_item(&items, "Reword Commit")
        .disabled_reason
        .clone()
        .unwrap();
    assert!(
        reason.contains("already pushed to protected branch 'origin/main'"),
        "{reason}"
    );
    assert!(find_item(&items, "Uncommit").disabled_reason.is_some());
    ts.dispatch(Action::SelectLogRow(0));
    let items = crate::menus::log_commit_menu_items(ts.state());
    assert!(find_item(&items, "Uncommit").is_enabled());
    assert!(find_item(&items, "Reword Commit").is_enabled());
}

#[test]
fn hover_opens_submenu_after_delay_and_keyboard_trigger_anchors_on_selection() {
    let mut ts = TestSession::with_snapshot(snapshot());
    let _ = ts.render(140, 40);
    // Keyboard trigger (Shift+F10) opens the same commit menu at the selected row.
    ts.dispatch(Action::SelectLogRow(2));
    let _ = ts.render(140, 40);
    let row = ts.region_of(|t| matches!(t, HitTarget::CommitTableRow { visible_row: 2, .. }));
    ts.send_key(KeyCode::F(10), KeyModifiers::SHIFT);
    let Some(ModalLayer::ContextMenu { anchor, items, .. }) = ts.state().modal_stack.last() else {
        panic!("keyboard menu")
    };
    assert_eq!(anchor.1, row.y);
    assert!(anchor.0 > row.x + row.width / 2);
    assert!(items.iter().any(|i| i.label.starts_with("Cherry-Pick")));
    ts.dispatch(Action::PopContextMenus);
    ts.send_key(KeyCode::Char('.'), KeyModifiers::NONE);
    assert!(matches!(
        ts.state().modal_stack.last(),
        Some(ModalLayer::ContextMenu { .. })
    ));

    // Hovering "Create Patch…" arms a 150 ms timer, then opens the submenu when virtual clock advances.
    let _ = ts.render(140, 40);
    let idx = {
        let (items, _, _) = top_menu(ts.state());
        menu_item_idx(items, "Create Patch")
    };
    let r = ts.region_of(
        |t| matches!(t, HitTarget::MenuItem { menu_depth: 0, item_idx } if *item_idx == idx),
    );
    ts.send_mouse(MouseEventKind::Moved, r.x + 2, r.y);
    let hover = ts.state().menu_hover.expect("hover timer armed");
    assert!(hover.due > ts.now());
    assert_eq!(
        ts.state().modal_stack.len(),
        1,
        "not opened before the delay"
    );
    ts.advance_by(Duration::from_millis(200));
    let (sub, _, depth) = top_menu(ts.state());
    assert_eq!(depth, 1);
    assert!(
        sub.iter()
            .any(|i| i.label.starts_with("Copy Patch to Clipboard"))
    );

    // Moving back onto a plain item closes the submenu after the delay too.
    let _ = ts.render(140, 40);
    let r = ts.region_of(|t| {
        matches!(
            t,
            HitTarget::MenuItem {
                menu_depth: 0,
                item_idx: 0
            }
        )
    });
    ts.send_mouse(MouseEventKind::Moved, r.x + 2, r.y);
    ts.advance_by(Duration::from_millis(200));
    assert_eq!(ts.state().modal_stack.len(), 1);

    // Mnemonics of enabled items are underlined; disabled items (Cherry-Pick
    // of a commit already on HEAD) are not.
    let buf = ts.render(140, 40);
    let cell_at = |label: &str, offset: u16| {
        let y = (0..40)
            .find(|&y| row_text(&buf, y).contains(label))
            .unwrap();
        let line = row_text(&buf, y);
        let x = line[..line.find(label).unwrap()].chars().count() as u16;
        buf[(x + offset, y)].clone()
    };
    let v = cell_at("Revert Commit", 2); // "Re[v]ert"
    assert_eq!(v.symbol(), "v");
    assert!(v.modifier.contains(Modifier::UNDERLINED));
    let p = cell_at("Cherry-Pick", 7);
    assert_eq!(p.symbol(), "P");
    assert!(!p.modifier.contains(Modifier::UNDERLINED));
    assert_eq!(p.fg, ts.state().theme.fg_muted);
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
    let mut ts = TestSession::with_snapshot(snap);
    let _ = ts.take_requests();
    ts.send_key(KeyCode::Char('3'), KeyModifiers::NONE);
    assert_eq!(ts.state().active_tab, WorkspaceTab::Stash);
    let reqs = ts.take_requests();
    assert!(reqs.iter().any(|(r, _)| matches!(
        r,
        TaskRequest::LoadCommitDetails { key: k, .. } if k.oids == vec![oid(90)]
    )));
    let buf = ts.render(140, 40);
    let text = screen_text(&buf);
    assert!(text.contains("stash@{0} main: wip parser"), "{text}");
    assert!(
        text.contains("stash@{1} feature/dag-layout: abc fix"),
        "{text}"
    );

    ts.send_key(KeyCode::Down, KeyModifiers::NONE);
    assert_eq!(ts.state().stash.selected(), 1);

    // Enter safely opens diff preview rather than mutating/applying the stash.
    let _ = ts.take_requests();
    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);
    assert!(ts.take_git_ops().is_empty());
    assert_eq!(ts.state().focused_pane, PaneId::StashDiffPreview);
    ts.send_key(KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(ts.state().focused_pane, PaneId::StashList);

    // `a` applies (keep), `o` pops.
    ts.send_key(KeyCode::Char('a'), KeyModifiers::NONE);
    assert_eq!(
        ts.take_git_ops(),
        vec![GitOp::StashApply {
            index: 1,
            pop: false,
            reinstate_index: false,
        }]
    );
    ts.send_key(KeyCode::Char('o'), KeyModifiers::NONE);
    assert_eq!(
        ts.take_git_ops(),
        vec![GitOp::StashApply {
            index: 1,
            pop: true,
            reinstate_index: false,
        }]
    );

    ts.send_key(KeyCode::Char('d'), KeyModifiers::NONE);
    assert!(matches!(
        ts.state().modal_stack.last(),
        Some(ModalLayer::Dialog(_))
    ));
    ts.send_key(KeyCode::Left, KeyModifiers::NONE);
    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(ts.take_git_ops(), vec![GitOp::StashDrop { index: 1 }]);

    // Shift+B opens unstash-as-branch dialog and validates the name.
    ts.send_key(KeyCode::Char('B'), KeyModifiers::SHIFT);
    ts.type_str("from-stash");
    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(
        ts.take_git_ops(),
        vec![GitOp::StashBranch {
            index: 1,
            name: "from-stash".into(),
        }]
    );

    // Plain `b` triggers the global Branches popup even from the Stash tab.
    ts.send_key(KeyCode::Char('b'), KeyModifiers::NONE);
    assert!(matches!(
        ts.state().modal_stack.last(),
        Some(ModalLayer::BranchesPopup { .. })
    ));
    ts.dispatch(Action::PopModal);
}

#[test]
fn stage_tree_menu_covers_expected_actions() {
    let mut ts = TestSession::with_snapshot(snapshot());
    ts.deliver_engine_event(EngineEvent::StageStatusLoaded {
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
    let idx_of = |state: &AppState, p: &str| {
        state
            .stage_workbench
            .rows()
            .iter()
            .position(
                |r| matches!(&r.kind, jbgit_core::StageTreeRowKind::File { path, .. } if path == p),
            )
            .unwrap()
    };
    let conflict = crate::menus::stage_tree_menu_items(ts.state(), idx_of(ts.state(), "src/c.rs"));
    for l in ["Resolve Conflicts", "Accept Yours", "Accept Theirs"] {
        find_item(&conflict, l);
    }
    let untracked =
        crate::menus::stage_tree_menu_items(ts.state(), idx_of(ts.state(), "notes.txt"));
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
    let ContextMenuEntryKind::Action(a) = &find_item(&untracked, "Add to .gitignore").kind else {
        panic!()
    };
    let _ = ts.take_requests();
    ts.dispatch_many(a.clone());
    assert_eq!(
        ts.take_git_ops(),
        vec![GitOp::AddToIgnore {
            patterns: vec!["/notes.txt".into()],
            exclude_file: false,
            target_file: None,
        }]
    );
    let _ = PaneId::CommitChangesTree;
}

#[test]
fn non_contiguous_squash_disabled_and_hunk_menu_respects_diff_side() {
    let mut ts = TestSession::with_snapshot(snapshot());
    ts.dispatch_many([Action::SelectLogRow(2), Action::ExtendLogSelectionTo(2)]);
    ts.state_mut()
        .stage_workbench
        .set_diff_key_for_test(Some(StageDiffKey {
            path: "src/lib.rs".into(),
            old_path: None,
            side: StageDiffSide::Staged,
            context: DiffContext::Three,
            whitespace: jbgit_core::WhitespaceMode::Default,
        }));
    let staged_hunk = crate::menus::stage_hunk_menu_items(ts.state(), 0);
    assert!(
        find_item(&staged_hunk, "Stage Hunk")
            .disabled_reason
            .is_some()
    );
    assert!(find_item(&staged_hunk, "Unstage Hunk").is_enabled());
    assert!(
        find_item(&staged_hunk, "Rollback Hunk")
            .disabled_reason
            .is_some()
    );

    ts.state_mut()
        .stage_workbench
        .set_diff_key_for_test(Some(StageDiffKey {
            path: "src/lib.rs".into(),
            old_path: None,
            side: StageDiffSide::Unstaged,
            context: DiffContext::Three,
            whitespace: jbgit_core::WhitespaceMode::Default,
        }));
    let unstaged_hunk = crate::menus::stage_hunk_menu_items(ts.state(), 0);
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
    // 1. Non-contiguous selection on a forked visible log: c12 (HEAD), side commit, c11.
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
    let mut ts = TestSession::with_snapshot(snap);
    let _ = ts.render(140, 40);
    ts.dispatch_many([Action::SelectLogRow(0), Action::ExtendLogSelectionTo(2)]);
    let items = crate::menus::log_commit_menu_items(ts.state());
    let squash = find_item(&items, "Squash 3 Commits");
    assert!(squash.disabled_reason.is_some());

    // Ctrl+Space / Ctrl+@ must NOT open Branches popup.
    ts.send_key(KeyCode::Char(' '), KeyModifiers::CONTROL);
    assert!(ts.state().modal_stack.is_empty());
    ts.send_key(KeyCode::Char('@'), KeyModifiers::CONTROL);
    assert!(ts.state().modal_stack.is_empty());

    // 2. Compare Versions (Ctrl+D with 2 selected commits) and Compare with Local.
    let mut ts = TestSession::with_snapshot(snapshot());
    ts.dispatch_many([
        Action::FocusPane(PaneId::LogCommitTable),
        Action::SelectLogRow(0),
        Action::ExtendLogSelectionTo(1),
    ]);
    let _ = ts.take_requests();
    ts.send_key(KeyCode::Char('d'), KeyModifiers::CONTROL);
    let reqs = ts.take_requests();
    assert!(reqs.iter().any(|(r, _)| matches!(
        r,
        TaskRequest::LoadCommitDetails { key: k, .. }
            if k.mode == DetailsMode::CompareVersions && k.oids == vec![oid(11), oid(12)]
    )));
    ts.dispatch(Action::CompareWithLocal { target: oid(10) });
    let reqs = ts.take_requests();
    assert!(reqs.iter().any(|(r, _)| matches!(
        r,
        TaskRequest::LoadCommitDetails { key: k, .. }
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
    let mut ts = TestSession::with_snapshot(snap);
    ts.send_key(KeyCode::Char('3'), KeyModifiers::NONE);
    ts.deliver_engine_event(EngineEvent::CommitDetailsLoaded {
        slot: jbgit_core::RequestSlotId::StashCommitDetails,
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
                old_oid: None,
                new_oid: None,
            }],
        }),
        epoch: 1,
    });
    let buf = ts.render(140, 40);
    let txt = screen_text(&buf);
    assert!(txt.contains("⊞ Dir"), "{txt}");
    assert!(txt.contains("ctx 3"), "{txt}");
    assert!(txt.contains("src"), "{txt}");

    // 4. Built-in three-pane conflict merge modal resolves & emits SaveConflictResolution.
    let _ = ts.take_requests();
    ts.dispatch(Action::OpenConflictResolver("src/c.rs".into()));
    let reqs = ts.take_requests();
    assert!(reqs.iter().any(|(r, _)| matches!(
        r,
        TaskRequest::LoadConflictDocument { path } if path == "src/c.rs"
    )));
    let doc = ConflictDocument::parse(
        "src/c.rs",
        "<<<<<<< HEAD\nours\n=======\ntheirs\n>>>>>>> feat\n",
        None,
        None,
    );
    ts.deliver_engine_event(EngineEvent::ConflictDocumentLoaded { doc, epoch: 1 });
    let buf = ts.render(140, 40);
    let txt = screen_text(&buf);
    assert!(txt.contains("Ours (HEAD)"), "{txt}");
    assert!(txt.contains("Merged Result"), "{txt}");
    assert!(txt.contains("Theirs (feat)"), "{txt}");
    let _ = ts.take_requests();
    ts.send_key(KeyCode::Char('y'), KeyModifiers::NONE);
    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);
    let reqs = ts.take_requests();
    assert!(reqs.iter().any(|(r, _)| matches!(
        r,
        TaskRequest::SaveConflictResolution { path, content, stage: true, delete: false }
            if path == "src/c.rs" && content == b"ours\n"
    )));
}

#[test]
fn p0_command_palette_checks_applicability_before_executing() {
    let mut snap = snapshot();
    snap.stashes = vec![StashEntry {
        index: 0,
        oid: oid(90),
        message: "On main: wip".into(),
        timestamp: 0,
    }];
    let mut ts = TestSession::with_snapshot(snap);
    ts.dispatch(Action::SwitchTab(WorkspaceTab::Log));
    ts.dispatch(Action::FocusPane(PaneId::LogCommitTable));
    let _ = ts.take_requests();

    // Open `?`, search `stash_pop` (Stash-only action) while in `[2 Log]`, press Enter.
    ts.send_key(KeyCode::Char('?'), KeyModifiers::SHIFT);
    ts.type_str("stash_pop");
    let _ = ts.take_requests();
    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);

    // Must close palette, emit a Notice, and NOT pop stash@{0}!
    assert!(ts.state().modal_stack.is_empty());
    assert!(
        ts.take_git_ops().is_empty(),
        "inapplicable stash_pop must not run from Log tab"
    );
    assert!(
        ts.state()
            .status_notice
            .as_deref()
            .is_some_and(|m| m.contains("not available")),
        "inapplicable command palette entry should show a notice, got {:?}",
        ts.state().status_notice
    );

    // Same for `bisect_reset` when not bisecting.
    ts.state_mut().status_notice = None;
    ts.send_key(KeyCode::Char('?'), KeyModifiers::SHIFT);
    ts.type_str("bisect_reset");
    let _ = ts.take_requests();
    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);
    assert!(ts.take_git_ops().is_empty());
    assert!(
        ts.state()
            .status_notice
            .as_deref()
            .is_some_and(|m| m.contains("not available")),
        "inapplicable bisect_reset should show a notice, got {:?}",
        ts.state().status_notice
    );
}

#[test]
fn p0_detached_head_branch_menu_disables_destructive_and_invalid_ops() {
    let mut snap = snapshot();
    snap.head_branch = None; // detached HEAD
    let ts = TestSession::with_snapshot(snap);

    let items = crate::menus::branch_menu_items(ts.state(), "", jbgit_core::RefKind::Head);
    for label in [
        "Checkout",
        "Rename",
        "Delete",
        "Set Upstream",
        "Compare with 'HEAD'",
        "Rebase 'HEAD' onto 'HEAD'",
        "Merge 'HEAD' into 'HEAD'",
    ] {
        let item = find_item(&items, label);
        assert!(
            !item.is_enabled(),
            "detached HEAD menu item '{label}' must be disabled"
        );
    }
    assert!(
        !items.iter().any(|i| i.label.contains("Worktree")),
        "detached HEAD must not offer New Worktree"
    );
    assert!(
        !items.iter().any(|i| i.label.contains("Favorite")),
        "detached HEAD must not offer Mark as Favorite"
    );
    assert!(
        crate::keymap::catalog::branch_checkout(ts.state(), "HEAD", jbgit_core::RefKind::Head)
            .is_err()
    );
    assert!(
        crate::keymap::catalog::branch_rename(ts.state(), "HEAD", jbgit_core::RefKind::Head)
            .is_err()
    );
    assert!(
        crate::keymap::catalog::branch_delete(ts.state(), "HEAD", jbgit_core::RefKind::Head)
            .is_err()
    );
}

#[test]
fn p0_right_click_stash_change_row_preselects_clicked_file() {
    let mut snap = snapshot();
    snap.stashes = vec![StashEntry {
        index: 0,
        oid: oid(90),
        message: "On main: wip".into(),
        timestamp: 0,
    }];
    let mut ts = TestSession::with_snapshot(snap);
    ts.send_key(KeyCode::Char('3'), KeyModifiers::NONE);
    ts.deliver_engine_event(EngineEvent::CommitDetailsLoaded {
        slot: jbgit_core::RequestSlotId::StashCommitDetails,
        bundle: Arc::new(CommitChangesBundle {
            key: DetailsKey::single(oid(90)),
            details: vec![],
            changes: vec![
                FileChange {
                    path: "a.rs".into(),
                    old_path: None,
                    kind: FileStatusKind::Modified,
                    additions: 1,
                    deletions: 0,
                    is_binary: false,
                    exists_in_workdir: true,
                    old_oid: None,
                    new_oid: None,
                },
                FileChange {
                    path: "b.rs".into(),
                    old_path: None,
                    kind: FileStatusKind::Modified,
                    additions: 1,
                    deletions: 0,
                    is_binary: false,
                    exists_in_workdir: true,
                    old_oid: None,
                    new_oid: None,
                },
            ],
        }),
        epoch: 1,
    });
    // Start with file 0 (a.rs) selected; right-click row 1 (b.rs).
    ts.dispatch(Action::ChangesSelect(crate::app::ChangesHost::Stash, 0));
    let _ = ts.render(140, 40);
    let r = ts.region_of(|t| matches!(t, HitTarget::StashChangeRow(1)));
    ts.send_mouse(MouseEventKind::Down(MouseButton::Right), r.x + 1, r.y);

    assert_eq!(ts.state().stash.changes().selected, 1);
    let (items, _, _) = top_menu(ts.state());
    let apply_file = find_item(items, "Apply Single File");
    let ContextMenuEntryKind::Action(acts) = &apply_file.kind else {
        panic!("expected Action on Apply Single File");
    };
    assert!(
        matches!(
            acts.as_slice(),
            [Action::RunGitOp(GitOp::CheckoutPaths { rev, paths })]
                if rev == "stash@{0}" && paths == &vec!["b.rs".to_string()]
        ),
        "expected CheckoutPaths for b.rs, got {acts:?}"
    );
}

#[test]
fn p0_right_click_stage_diff_line_preserves_inside_and_resets_outside_visual_bounds() {
    use jbgit_engine::ThreeDocStageDiff;

    let mut ts = TestSession::with_snapshot(snapshot());
    ts.dispatch(Action::SwitchTab(WorkspaceTab::Commit));
    ts.dispatch(Action::FocusPane(PaneId::CommitDiffPreview));
    let key = StageDiffKey {
        path: "src/lib.rs".into(),
        old_path: None,
        side: StageDiffSide::Unstaged,
        context: DiffContext::Three,
        whitespace: jbgit_core::WhitespaceMode::Default,
    };
    let vcs = b"a\nb\nc\nd\ne\nf\ng\nh\ni\nj\nk\nl\n";
    let local = b"a1\nb1\nc1\nd1\ne1\nf1\ng1\nh1\ni1\nj1\nk1\nl1\n";
    let tracker = jbgit_core::StageLineTracker::new(vcs, vcs, local);
    let (hunks, is_truncated) = tracker.build_interactive_hunks(false, DiffContext::Three, 20_000);
    ts.state_mut()
        .stage_workbench
        .set_diff_key_for_test(Some(key.clone()));
    let epoch = ts
        .state()
        .request_slots
        .latest(jbgit_core::RequestSlotId::StageFileDiff);
    ts.deliver_engine_event(EngineEvent::StageFileDiffLoaded {
        diff: Arc::new(ThreeDocStageDiff {
            key,
            vcs_exists: true,
            staged_exists: true,
            local_exists: true,
            vcs_bytes: vcs.to_vec(),
            staged_bytes: vcs.to_vec(),
            local_bytes: local.to_vec(),
            file_mode: 0o100644,
            mode_change: None,
            is_symlink: false,
            is_binary: false,
            binary: None,
            is_conflicted: false,
            submodule: None,
            lfs: None,
            tracker,
            hunks,
            is_truncated,
            is_too_large: false,
        }),
        epoch,
    });

    // Visually select rows 5..=10.
    ts.dispatch(Action::StageDiffSelectLine {
        row_idx: 5,
        extend: false,
    });
    ts.dispatch(Action::StageDiffToggleVisual);
    ts.dispatch(Action::StageDiffSelectLine {
        row_idx: 10,
        extend: true,
    });
    assert_eq!(
        ts.state().stage_workbench.selected_visual_bounds(),
        Some((5, 10))
    );

    // Right-click inside bounds (row 7): preserves visual selection without StageDiffSelectLine.
    let inside = crate::input::preselect_actions_for_menu_target(
        &HitTarget::StageDiffLine { row_idx: 7 },
        ts.state(),
    );
    assert!(
        matches!(
            inside.as_slice(),
            [Action::FocusPane(PaneId::CommitDiffPreview)]
        ),
        "inside visual bounds must not emit StageDiffSelectLine: {inside:?}"
    );

    // Right-click outside bounds (row 2): clears visual selection (`extend: false`).
    let outside = crate::input::preselect_actions_for_menu_target(
        &HitTarget::StageDiffLine { row_idx: 2 },
        ts.state(),
    );
    assert!(
        matches!(
            outside.as_slice(),
            [
                Action::FocusPane(PaneId::CommitDiffPreview),
                Action::StageDiffSelectLine {
                    row_idx: 2,
                    extend: false,
                },
            ]
        ),
        "outside visual bounds must reset with extend:false: {outside:?}"
    );
}

#[test]
fn p1_p2_ux_regressions_menus_mouse_paste_popups_and_ignore_filter() {
    use crossterm::event::{Event, KeyEvent};
    use std::collections::HashSet;

    // 1. P1-10: No duplicate Favorite in BranchTreeNode, and no dead ('j'/'k') or colliding mnemonics.
    let mut snap = snapshot();
    snap.has_more_commits = true;
    snap.is_shallow = true;
    snap.stashes = vec![StashEntry {
        index: 0,
        oid: oid(90),
        message: "On main: wip".into(),
        timestamp: 0,
    }];
    let mut ts = TestSession::with_snapshot(snap);
    let _ = ts.render(140, 40);

    let assert_clean_mnemonics = |name: &str, items: &[crate::app::ContextMenuItem]| {
        let mut seen: HashSet<char> = HashSet::new();
        for item in items {
            if let Some(m) = item.mnemonic {
                let lc = m.to_ascii_lowercase();
                assert!(
                    lc != 'j' && lc != 'k',
                    "menu '{name}' item '{}' uses dead navigation mnemonic '{m}'",
                    item.label
                );
                assert!(
                    seen.insert(lc),
                    "menu '{name}' has duplicate mnemonic '{m}' on '{}'",
                    item.label
                );
            }
        }
    };

    assert_clean_mnemonics(
        "log_commit",
        &crate::menus::log_commit_menu_items(ts.state()),
    );
    assert_clean_mnemonics(
        "presentation",
        &crate::input::presentation_menu_items(ts.state()),
    );
    assert_clean_mnemonics(
        "branch_local",
        &crate::menus::branch_menu_items(ts.state(), "main", jbgit_core::RefKind::LocalBranch),
    );
    assert_clean_mnemonics(
        "branch_remote",
        &crate::menus::branch_menu_items(
            ts.state(),
            "origin/main",
            jbgit_core::RefKind::RemoteBranch,
        ),
    );
    assert_clean_mnemonics(
        "branch_tag",
        &crate::menus::branch_menu_items(ts.state(), "v1.0", jbgit_core::RefKind::Tag),
    );
    assert_clean_mnemonics(
        "stage_diff",
        &crate::menus::stage_diff_menu_items(ts.state()),
    );
    assert_clean_mnemonics("stash", &crate::menus::stash_menu_items(ts.state()));
    assert_clean_mnemonics(
        "stash_file",
        &crate::menus::stash_file_menu_items(ts.state()),
    );

    // Check BranchTreeNode on a local branch leaf: only ONE Favorite item and zero collisions.
    let leaf_row = ts
        .state()
        .log
        .branches
        .rows
        .iter()
        .position(|r| {
            matches!(
                ts.state().log.branches.model.nodes[r.node].kind,
                jbgit_core::BranchNodeKind::Leaf(jbgit_core::RefKind::LocalBranch)
            )
        })
        .expect("local branch leaf");
    let branch_node_items = crate::input::build_menu_items_for_target(
        &HitTarget::BranchTreeNode {
            pane: PaneId::LogBranchesTree,
            node_id: jbgit_core::BranchTreeNodeId(leaf_row as u32),
            row: leaf_row,
        },
        ts.state(),
    )
    .expect("branch node menu");
    let fav_count = branch_node_items
        .iter()
        .filter(|i| i.label.contains("Favorite"))
        .count();
    assert_eq!(fav_count, 1, "BranchTreeNode must not duplicate Favorite");
    assert_clean_mnemonics("branch_tree_node", &branch_node_items);

    // Check CommitRefPill menu has zero collisions.
    let pill_items = crate::input::build_menu_items_for_target(
        &HitTarget::CommitRefPill {
            visible_row: 0,
            ref_name: "main".into(),
            is_detached_head: false,
        },
        ts.state(),
    )
    .expect("ref pill menu");
    assert_clean_mnemonics("commit_ref_pill", &pill_items);

    // 2. P1-9: Paste in LogBranchesTree is ignored when searching == false && tree_speed_search == false.
    ts.dispatch(Action::SwitchTab(WorkspaceTab::Log));
    ts.dispatch(Action::FocusPane(PaneId::LogBranchesTree));
    ts.state_mut().config.keymap.tree_speed_search = false;
    ts.state_mut().log.branches.searching = false;
    let paste_actions =
        crate::input::route_terminal_event(&Event::Paste("feat".into()), ts.state());
    assert!(
        paste_actions.is_empty(),
        "paste must be gated when branch search and tree_speed_search are off: {paste_actions:?}"
    );
    ts.state_mut().log.branches.searching = true;
    let paste_active = crate::input::route_terminal_event(&Event::Paste("feat".into()), ts.state());
    assert_eq!(paste_active.len(), 4);
    ts.state_mut().log.branches.searching = false;

    // 3. P1-7 & P1-2: StashChangeRow wheel moves changes list, and <Left>/<Right> on Stash dir expands/collapses.
    ts.dispatch(Action::SwitchTab(WorkspaceTab::Stash));
    ts.deliver_engine_event(EngineEvent::CommitDetailsLoaded {
        slot: jbgit_core::RequestSlotId::StashCommitDetails,
        bundle: Arc::new(CommitChangesBundle {
            key: DetailsKey::single(oid(90)),
            details: vec![],
            changes: vec![
                FileChange {
                    path: "dir/a.rs".into(),
                    old_path: None,
                    kind: FileStatusKind::Modified,
                    additions: 1,
                    deletions: 0,
                    is_binary: false,
                    exists_in_workdir: true,
                    old_oid: None,
                    new_oid: None,
                },
                FileChange {
                    path: "dir/b.rs".into(),
                    old_path: None,
                    kind: FileStatusKind::Modified,
                    additions: 1,
                    deletions: 0,
                    is_binary: false,
                    exists_in_workdir: true,
                    old_oid: None,
                    new_oid: None,
                },
            ],
        }),
        epoch: 1,
    });
    ts.dispatch(Action::FocusPane(PaneId::StashDiffPreview));
    let _ = ts.render(140, 40);
    let r = ts.region_of(|t| matches!(t, HitTarget::StashChangeRow(0)));
    let wheel_actions = crate::input::route_terminal_event(
        &Event::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: r.x + 1,
            row: r.y,
            modifiers: KeyModifiers::NONE,
        }),
        ts.state(),
    );
    assert!(
        matches!(
            wheel_actions.as_slice(),
            [Action::ChangesMove(crate::app::ChangesHost::Stash, 1)]
        ),
        "expected ChangesMove(Stash, 1), got {wheel_actions:?}"
    );

    // Row 0 is `dir` folder (group_by_dir defaults to true): pressing Left collapses it.
    ts.dispatch(Action::ChangesSelect(crate::app::ChangesHost::Stash, 0));
    let left_actions = crate::input::route_terminal_event(
        &Event::Key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE)),
        ts.state(),
    );
    assert!(
        matches!(
            left_actions.as_slice(),
            [Action::ChangesExpand(crate::app::ChangesHost::Stash, false)]
        ),
        "expected ChangesExpand(Stash, false), got {left_actions:?}"
    );

    // 4. P1-8: Mouse wheel scrolls ShortcutsHelp modal via DialogBody.
    ts.send_key(KeyCode::Char('?'), KeyModifiers::SHIFT);
    let _ = ts.render(140, 40);
    let body = ts.region_of(|t| matches!(t, HitTarget::DialogBody));
    let before_sel = match ts.state().modal_stack.last() {
        Some(ModalLayer::ShortcutsHelp { selected, .. }) => *selected,
        _ => panic!("expected ShortcutsHelp"),
    };
    ts.send_mouse(MouseEventKind::ScrollDown, body.x + 2, body.y + 2);
    let after_sel = match ts.state().modal_stack.last() {
        Some(ModalLayer::ShortcutsHelp { selected, .. }) => *selected,
        _ => panic!("expected ShortcutsHelp"),
    };
    assert!(
        after_sel > before_sel,
        "ShortcutsHelp wheel should advance selection"
    );
    ts.dispatch(Action::PopModal);

    // 5. P2-1: Floating popup `Down` is clamped so `Enter` still selects the last candidate.
    ts.dispatch(Action::SwitchTab(WorkspaceTab::Commit));
    let expected_last = ts
        .state()
        .stage_workbench
        .commit_history_candidates(ts.state().repo_snapshot.as_deref())
        .last()
        .cloned()
        .expect("history candidate from snapshot");
    ts.dispatch(Action::OpenPopup {
        kind: crate::app::PopupKind::CommitHistory,
        initial_query: String::new(),
    });
    for _ in 0..40 {
        ts.send_key(KeyCode::Down, KeyModifiers::NONE);
    }
    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(ts.state().stage_workbench.editor().text(), expected_last);

    // 6. P2-5 & P1-11: Mixed tracked + untracked ignores only untracked; untracked diff annotate & amend patch disabled.
    ts.deliver_engine_event(EngineEvent::StageStatusLoaded {
        status: Arc::new(StageStatusSnapshot {
            staged: vec![],
            unstaged: vec![StageEntry {
                path: "tracked.rs".into(),
                old_path: None,
                kind: FileStatusKind::Modified,
            }],
            untracked: vec![StageEntry {
                path: "untracked.tmp".into(),
                old_path: None,
                kind: FileStatusKind::Untracked,
            }],
            conflicted: vec![],
            ignored: vec![],
            ..Default::default()
        }),
        epoch: 1,
    });
    ts.dispatch(Action::StageSelectAll);
    let rows = ts.state().stage_workbench.rows().to_vec();
    let tracked_idx = rows
        .iter()
        .position(|r| matches!(&r.kind, jbgit_core::StageTreeRowKind::File { path, .. } if path == "tracked.rs"))
        .expect("tracked row");
    let mixed_menu = crate::menus::stage_tree_menu_items(ts.state(), tracked_idx);
    let ignore_item = find_item(&mixed_menu, "Add to .gitignore");
    assert!(ignore_item.is_enabled());
    let ContextMenuEntryKind::Action(ignore_acts) = &ignore_item.kind else {
        panic!("expected action on Add to .gitignore");
    };
    assert!(
        matches!(
            ignore_acts.as_slice(),
            [Action::RunGitOp(GitOp::AddToIgnore {
                patterns,
                exclude_file: false,
                target_file: None,
            })] if patterns == &vec!["/untracked.tmp".to_string()]
        ),
        "Add to .gitignore must include only untracked files, got {ignore_acts:?}"
    );

    // P1-11(a): annotate_stage_diff rejects untracked file.
    ts.state_mut()
        .stage_workbench
        .set_diff_key_for_test(Some(StageDiffKey {
            path: "untracked.tmp".into(),
            old_path: None,
            side: StageDiffSide::Unstaged,
            context: DiffContext::Three,
            whitespace: jbgit_core::WhitespaceMode::Default,
        }));
    assert!(
        crate::keymap::catalog::annotate_stage_diff(ts.state(), Some("untracked.tmp")).is_err(),
        "untracked file in CommitDiffPreview must not be annotatable"
    );

    // P1-11(b): AmendCommit diff disables Create Patch and Copy Patch.
    ts.state_mut()
        .stage_workbench
        .set_diff_key_for_test(Some(StageDiffKey {
            path: "tracked.rs".into(),
            old_path: None,
            side: StageDiffSide::AmendCommit,
            context: DiffContext::Three,
            whitespace: jbgit_core::WhitespaceMode::Default,
        }));
    let amend_diff_menu = crate::menus::stage_diff_menu_items(ts.state());
    assert!(!find_item(&amend_diff_menu, "Create Patch").is_enabled());
    assert!(!find_item(&amend_diff_menu, "Copy Patch").is_enabled());

    // P1-11(c): FileHistory Annotate disabled on Deleted commit.
    ts.dispatch(Action::OpenFileHistory {
        path: "gone.rs".into(),
        start_oid: None,
        follow_renames: true,
        line_range: None,
    });
    let fh_epoch = ts
        .state()
        .request_slots
        .latest(jbgit_core::RequestSlotId::FileHistoryList);
    ts.deliver_engine_event(EngineEvent::FileHistoryLoaded {
        bundle: Arc::new(jbgit_core::FileHistoryBundle {
            key: jbgit_core::FileHistoryKey::new("gone.rs", None, true, None),
            entries: vec![jbgit_core::FileHistoryEntry {
                oid: oid(5),
                author_name: "dev".into(),
                author_time: 0,
                summary: "delete gone.rs".into(),
                kind: FileStatusKind::Deleted,
                path_at_commit: "gone.rs".into(),
                old_path: None,
                additions: 0,
                deletions: 10,
                is_binary: false,
                range_patch: None,
            }],
            truncated: false,
        }),
        epoch: fh_epoch,
    });
    let fh_menu = crate::menus::file_history_menu_items(ts.state());
    assert!(!find_item(&fh_menu, "Annotate at Revision").is_enabled());
    assert_clean_mnemonics("file_history", &fh_menu);
    ts.dispatch(Action::PopModal);

    // P2-4(a): Push dialog `g` jumps to top of commits list.
    ts.dispatch(Action::PromptPush {
        branch: "main".into(),
        up_to: None,
    });
    if let Some(pd) = ts.state().modal_stack.push_dialog() {
        let _ = pd;
        let g_actions = crate::input::route_terminal_event(
            &Event::Key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE)),
            ts.state(),
        );
        assert!(
            matches!(g_actions.as_slice(), [Action::PushSelectCommit(0)]),
            "expected PushSelectCommit(0) on 'g' in Push dialog, got {g_actions:?}"
        );
    }
    ts.dispatch(Action::PopModal);

    // P2-4(b): Remote branch New Worktree hidden when git < 2.17.
    ts.state_mut().git_version = Some(jbgit_core::GitVersion::new(2, 16, 0));
    let remote_menu_old_git = crate::menus::branch_menu_items(
        ts.state(),
        "origin/main",
        jbgit_core::RefKind::RemoteBranch,
    );
    assert!(
        !remote_menu_old_git
            .iter()
            .any(|i| i.label.contains("Worktree")),
        "Remote branch New Worktree must be hidden on git < 2.17"
    );
}
