use std::sync::Arc;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEventKind};
use jbgit_core::{
    CommitChangesBundle, CommitSummary, ConflictDocument, DetailsKey, DetailsMode, DiffContext,
    FileChange, FileStatusKind, GitOp, Oid, PaneId, StageEntry, StageStatusSnapshot, StashEntry,
    TodoAction, WorkspaceTab,
};
use jbgit_engine::{EngineEvent, StageDiffKey, StageDiffSide};
use ratatui::layout::Rect;
use ratatui::style::Modifier;

use crate::app::{Action, AppState, ContextMenuAction, ContextMenuEntryKind, Effect, ModalLayer};
use crate::dialogs::Dialog;
use crate::hit_test::HitTarget;
use crate::views::overlays::context_menu_rect;

use super::{
    find_item, git_ops, key, local, menu_item_idx, mouse, oid, region_of, render, row_text, run,
    screen_text, snapshot, state_with, top_menu, type_str,
};

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
            kind: crate::app::PopupKind::BranchesQuickView,
            ..
        })
    ));
    state.dispatch(Action::PopModal);
}

#[test]
fn stage_tree_menu_covers_expected_actions() {
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
        .sync_rows(state.stage_status.as_ref());
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
    let ContextMenuEntryKind::Action(ContextMenuAction::Run(a)) =
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
    let mut state = state_with(snapshot());
    state.log_graph.selected_idx = 2;
    state.log_graph.anchor = Some(2);
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
    let mut state = state_with(snap);
    let _ = render(&mut state, 140, 40);
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
                old_oid: None,
                new_oid: None,
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
