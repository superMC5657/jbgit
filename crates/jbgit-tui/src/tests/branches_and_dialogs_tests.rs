use std::sync::Arc;

use crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEventKind};
use jbgit_core::{
    CommitSummary, GitOp, GitOpOutcome, OngoingKind, OngoingStep, RepositoryState, ResetMode,
    StageEntry, StageStatusSnapshot, TodoAction, WorkspaceTab, FileStatusKind,
};
use jbgit_engine::EngineEvent;
use ratatui::style::Modifier;

use crate::app::{Action, ContextMenuEntryKind, ModalLayer};
use crate::branches_popup::{PopupRow, build_popup_rows};
use crate::dialogs::Dialog;
use crate::hit_test::{HitTarget, OngoingButton};
use crate::views::overlays::context_menu_rect;

use super::{
    find_item, git_ops, key, local, mouse, oid, region_of, render, row_text, run, screen_text,
    snapshot, state_with, top_menu, type_str,
};

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
    let started = std::time::Instant::now();
    type_str(&mut state, "f/dl");
    assert!(started.elapsed() < std::time::Duration::from_millis(500));
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
    // Use a branch with commits outside HEAD to exercise the unmerged delete guard.
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
        jbgit_core::GitFailure::LocalChangesOverwritten {
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
        jbgit_core::GitFailure::LocalChangesOverwritten { files: vec![] },
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
    let plan: Vec<(jbgit_core::Oid, TodoAction)> = todo.iter().map(|e| (e.oid, e.action)).collect();
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
