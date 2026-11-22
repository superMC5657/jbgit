use std::sync::Arc;

use crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEventKind};
use jbgit_core::{
    CommitSummary, FileStatusKind, GitOp, GitOpOutcome, OngoingKind, OngoingStep, RepositoryState,
    ResetMode, StageEntry, StageStatusSnapshot, TodoAction, WorkspaceTab,
};
use jbgit_engine::EngineEvent;
use ratatui::style::Modifier;

use crate::app::{Action, ContextMenuEntryKind, ModalLayer};
use crate::branches_popup::{PopupRow, build_popup_rows};
use crate::dialogs::Dialog;
use crate::hit_test::{HitTarget, OngoingButton};
use crate::views::overlays::context_menu_rect;

use super::{
    TestSession, find_item, local, oid, row_text, screen_text, snapshot, top_menu,
};

#[test]
fn branches_popup_fuzzy_search_highlights_and_opens_branch_actions() {
    let mut ts = TestSession::with_snapshot(snapshot());
    ts.dispatch(Action::SwitchTab(WorkspaceTab::Commit));
    // `b` opens the global Branches popup everywhere; `Ctrl+`` also works everywhere.
    ts.send_key(KeyCode::Char('b'), KeyModifiers::NONE);
    assert!(matches!(
        ts.state().modal_stack.last(),
        Some(ModalLayer::BranchesPopup { .. })
    ));
    ts.dispatch(Action::PopModal);
    ts.send_key(KeyCode::Char('`'), KeyModifiers::CONTROL);

    // Grouped view: Recent → Local (folders) → Remote.
    let rows = build_popup_rows(ts.state(), "");
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
    ts.type_str("f/dl");
    assert!(started.elapsed() < std::time::Duration::from_millis(500));
    let (query, selected) = match ts.state().modal_stack.last() {
        Some(ModalLayer::BranchesPopup { query, selected }) => (query.clone(), *selected),
        other => panic!("{other:?}"),
    };
    assert_eq!(query, "f/dl");
    let rows = build_popup_rows(ts.state(), &query);
    let PopupRow::Branch(b) = &rows[selected] else {
        panic!("selection must be a branch row");
    };
    assert_eq!(b.name, "feature/dag-layout");
    let chars: Vec<char> = b.name.chars().collect();
    let hl: String = b.indices.iter().map(|&i| chars[i as usize]).collect();
    assert_eq!(hl, "f/dl");

    // Highlighted characters render bold in #548AF7.
    let buf = ts.render(120, 40);
    let text = screen_text(&buf);
    assert!(text.contains("Git Branches (jbgit)"), "{text}");
    let y = (0..40)
        .find(|&y| row_text(&buf, y).contains("feature/dag-layout"))
        .unwrap();
    let line = row_text(&buf, y);
    let x0 = line.find("feature/dag-layout").unwrap();
    let x0 = line[..x0].chars().count() as u16;
    let cell = &buf[(x0, y)];
    assert_eq!(cell.fg, ts.state().theme.fg_accent_link);
    assert!(cell.modifier.contains(Modifier::BOLD));
    let plain = &buf[(x0 + 1, y)]; // 'e' is not matched
    assert_ne!(plain.fg, ts.state().theme.fg_accent_link);

    // Up / Down skip header rows.
    ts.send_key(KeyCode::Down, KeyModifiers::NONE);
    if let Some(ModalLayer::BranchesPopup { selected, query }) = ts.state().modal_stack.last() {
        let rows = build_popup_rows(ts.state(), query);
        assert!(rows[*selected].selectable());
    }
    ts.send_key(KeyCode::Up, KeyModifiers::NONE);

    // Enter opens the cascading action submenu beside the popup.
    let _ = ts.render(120, 40);
    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);
    let (items, _, depth) = top_menu(ts.state());
    assert_eq!(depth, 0);
    for label in [
        "Checkout",
        "New Branch from",
        "Checkout and Rebase onto",
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
    }) = ts.state().modal_stack.last()
    else {
        panic!("submenu must hang off the popup");
    };
    let sub = context_menu_rect(ts.state().terminal_area, (0, 0), Some(*p), items);
    assert!(
        sub.x >= p.rect.right(),
        "submenu opens to the right of the popup"
    );

    // Running an action closes both the submenu and the popup.
    let _ = ts.take_requests();
    ts.send_key(KeyCode::Char('c'), KeyModifiers::NONE);
    assert!(ts.state().modal_stack.is_empty());
    assert_eq!(
        ts.take_git_ops(),
        vec![GitOp::Checkout {
            target: "feature/dag-layout".into(),
            detach: false,
        }]
    );
}

#[test]
fn remote_checkout_tracks_new_branch_or_asks_when_diverged() {
    let mut ts = TestSession::with_snapshot(snapshot());
    let _ = ts.take_requests();
    ts.dispatch(Action::CheckoutRemoteBranch {
        remote_ref: "origin/feature-new".into(),
    });
    assert_eq!(
        ts.take_git_ops(),
        vec![GitOp::CheckoutNewBranch {
            name: "feature-new".into(),
            start: "origin/feature-new".into(),
            track: true,
        }]
    );

    // Local `feature/dag-layout` (c5) has 1 commit not in origin (c4) → dialog.
    ts.dispatch(Action::CheckoutRemoteBranch {
        remote_ref: "origin/feature/dag-layout".into(),
    });
    assert!(ts.take_git_ops().is_empty());
    let Some(ModalLayer::Dialog(d)) = ts.state().modal_stack.last() else {
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
    let mut ts = TestSession::with_snapshot(snapshot());
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
    ts.deliver_engine_event(EngineEvent::RepoSnapshotLoaded {
        snapshot: Arc::new(snap),
        epoch: 2,
    });
    let _ = ts.take_requests();

    // Merged branch: plain `-d` straight away.
    ts.dispatch(Action::PromptDeleteBranch {
        name: "bugfix/unmerged".into(),
    });
    assert_eq!(
        ts.take_git_ops(),
        vec![GitOp::DeleteBranch {
            name: "bugfix/unmerged".into(),
            force: false,
            tip: oid(3),
        }]
    );

    // Unmerged: warning with commit count + Force Delete.
    ts.dispatch(Action::PromptDeleteBranch {
        name: "topic".into(),
    });
    assert!(ts.take_git_ops().is_empty());
    let buf = ts.render(120, 40);
    let text = screen_text(&buf);
    assert!(text.contains("Delete Unmerged Branch"), "{text}");
    assert!(text.contains("1 commit(s) not merged"), "{text}");
    assert!(text.contains("Force Delete (-D)"), "{text}");
    // Default focus is Cancel; Left moves to Force Delete.
    ts.send_key(KeyCode::Left, KeyModifiers::NONE);
    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);
    let op = GitOp::DeleteBranch {
        name: "topic".into(),
        force: true,
        tip: oid(40),
    };
    assert_eq!(ts.take_git_ops(), vec![op.clone()]);

    // Success → footer notification with [Restore Branch] → restores the tip.
    ts.deliver_engine_event(EngineEvent::GitOpFinished(GitOpOutcome::ok(op)));
    let buf = ts.render(120, 40);
    let footer = row_text(&buf, 39);
    assert!(footer.contains("Deleted branch 'topic'"), "{footer}");
    assert!(footer.contains("[Restore Branch]"), "{footer}");
    let r = ts.region_of(|t| matches!(t, HitTarget::NotificationAction));
    ts.send_mouse(MouseEventKind::Down(MouseButton::Left), r.x + 1, r.y);
    assert_eq!(
        ts.take_git_ops(),
        vec![GitOp::RestoreBranch {
            name: "topic".into(),
            oid: oid(40),
        }]
    );
    assert!(ts.state().notification.is_none());
}

#[test]
fn new_branch_dialog_validates_name_inline_and_blocks_confirm() {
    let mut ts = TestSession::with_snapshot(snapshot());
    let _ = ts.take_requests();
    ts.send_key(KeyCode::Char('n'), KeyModifiers::NONE);
    assert!(matches!(
        ts.state().modal_stack.last(),
        Some(ModalLayer::Dialog(_))
    ));

    for (input, err) in [
        ("bad name", "cannot contain spaces"),
        ("a..b", "cannot contain '..'"),
        ("-x", "cannot start with '-'"),
        ("main", "Branch 'main' already exists"),
    ] {
        ts.send_key(KeyCode::Char('u'), KeyModifiers::CONTROL);
        ts.type_str(input);
        let buf = ts.render(120, 40);
        let text = screen_text(&buf);
        assert!(text.contains(err), "{input:?}: {text}");
        ts.send_key(KeyCode::Enter, KeyModifiers::NONE);
        assert!(ts.take_git_ops().is_empty(), "{input:?} must not confirm");
        assert!(matches!(
            ts.state().modal_stack.last(),
            Some(ModalLayer::Dialog(_))
        ));
        // The OK button renders disabled (muted).
        let ok = ts.region_of(|t| {
            matches!(
                t,
                HitTarget::DialogButton(crate::hit_test::DialogButtonKind::Confirm)
            )
        });
        assert_eq!(buf[(ok.x + 1, ok.y)].fg, ts.state().theme.fg_muted);
    }
    ts.send_key(KeyCode::Char('u'), KeyModifiers::CONTROL);
    ts.type_str("feature/ok");
    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(
        ts.take_git_ops(),
        vec![GitOp::CheckoutNewBranch {
            name: "feature/ok".into(),
            start: oid(12).to_string(),
            track: false,
        }]
    );
    assert!(ts.state().modal_stack.is_empty());
}

#[test]
fn ongoing_banner_shows_state_progress_and_buttons_run_steps() {
    let mut snap = snapshot();
    snap.state = RepositoryState::RebaseMerge {
        progress: Some((2, 5)),
        branch: Some("feat/dag".into()),
    };
    snap.ongoing_onto = Some("main".into());
    let mut ts = TestSession::with_snapshot(snap);
    ts.deliver_engine_event(EngineEvent::StageStatusLoaded {
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
    let _ = ts.take_requests();

    for tab in [WorkspaceTab::Log, WorkspaceTab::Commit] {
        ts.dispatch(Action::SwitchTab(tab));
        let buf = ts.render(140, 40);
        let banner = row_text(&buf, 2);
        assert!(
            banner.contains("⚠ REBASING feat/dag onto main (Step 2/5) — 1 conflicted file"),
            "{banner}"
        );
        for b in ["[ Resolve ]", "[ Continue ]", "[ Skip ]", "[ Abort ]"] {
            assert!(banner.contains(b), "{b} missing: {banner}");
        }
        let header = row_text(&buf, 0);
        assert!(header.contains("Rebasing 2/5"), "{header}");
    }

    // Continue is refused while conflicts remain.
    let r = ts.region_of(|t| matches!(t, HitTarget::OngoingBanner(OngoingButton::Continue)));
    ts.send_mouse(MouseEventKind::Down(MouseButton::Left), r.x + 2, r.y);
    assert!(ts.take_git_ops().is_empty());
    assert!(
        ts.state()
            .status_notice
            .as_deref()
            .unwrap_or("")
            .contains("conflicted")
    );

    let r = ts.region_of(|t| matches!(t, HitTarget::OngoingBanner(OngoingButton::Abort)));
    ts.send_mouse(MouseEventKind::Down(MouseButton::Left), r.x + 2, r.y);
    assert_eq!(
        ts.take_git_ops(),
        vec![GitOp::Ongoing {
            kind: OngoingKind::Rebase,
            step: OngoingStep::Abort,
        }]
    );

    // Resolve jumps to the conflicted file in the Commit tab.
    ts.dispatch(Action::SwitchTab(WorkspaceTab::Log));
    let _ = ts.render(140, 40);
    let r = ts.region_of(|t| matches!(t, HitTarget::OngoingBanner(OngoingButton::Resolve)));
    ts.send_mouse(MouseEventKind::Down(MouseButton::Left), r.x + 2, r.y);
    assert_eq!(ts.state().active_tab, WorkspaceTab::Commit);
    let row = ts.state().stage_workbench.selected_row().unwrap();
    assert!(
        matches!(&row.kind, jbgit_core::StageTreeRowKind::File { path, .. } if path == "src/graph.rs")
    );
}

#[test]
fn smart_operation_dialog_offers_stash_and_force_checkout() {
    let mut ts = TestSession::with_snapshot(snapshot());
    let op = GitOp::Checkout {
        target: "feature/dag-layout".into(),
        detach: false,
    };
    ts.state_mut().git_ops_running = 1;
    ts.deliver_engine_event(EngineEvent::GitOpFinished(GitOpOutcome::failed(
        op.clone(),
        jbgit_core::GitFailure::LocalChangesOverwritten {
            files: vec!["src/app.rs".into()],
        },
    )));
    assert_eq!(ts.state().git_ops_running, 0);
    let buf = ts.render(120, 40);
    let text = screen_text(&buf);
    assert!(text.contains("Smart Checkout"), "{text}");
    assert!(text.contains("src/app.rs"), "{text}");
    assert!(text.contains("Force Checkout (Discard Local)"), "{text}");
    let _ = ts.take_requests();
    ts.send_key(KeyCode::Char('s'), KeyModifiers::NONE);
    assert_eq!(ts.take_git_ops(), vec![GitOp::Smart(Box::new(op))]);
    assert!(ts.state().modal_stack.is_empty());

    // Merge failures offer Smart Merge (no force option).
    ts.deliver_engine_event(EngineEvent::GitOpFinished(GitOpOutcome::failed(
        GitOp::Merge {
            rev: "feature/dag-layout".into(),
        },
        jbgit_core::GitFailure::LocalChangesOverwritten { files: vec![] },
    )));
    let Some(ModalLayer::Dialog(d)) = ts.state().modal_stack.last() else {
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
    let mut ts = TestSession::with_snapshot(snapshot());
    ts.dispatch(Action::SelectLogRow(3));
    let items = crate::menus::log_commit_menu_items(ts.state());
    let reset = find_item(&items, "Reset Current Branch to Here");
    let ContextMenuEntryKind::Action(actions) = &reset.kind else {
        panic!()
    };
    let _ = ts.take_requests();
    ts.dispatch_many(actions.clone());
    let buf = ts.render(120, 40);
    let text = screen_text(&buf);
    for m in ["( ) Soft", "(•) Mixed", "( ) Hard", "( ) Keep", "DATA LOSS"] {
        assert!(text.contains(m), "{m} missing: {text}");
    }
    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(
        ts.take_git_ops(),
        vec![GitOp::Reset {
            mode: ResetMode::Mixed,
            target: oid(9),
        }]
    );

    ts.dispatch_many(actions.clone());
    ts.send_key(KeyCode::Char('h'), KeyModifiers::NONE);
    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);
    assert!(
        ts.take_git_ops().is_empty(),
        "hard reset needs a second confirmation"
    );
    let buf = ts.render(120, 40);
    assert!(screen_text(&buf).contains("ALL uncommitted changes"));
    ts.send_key(KeyCode::Left, KeyModifiers::NONE);
    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(
        ts.take_git_ops(),
        vec![GitOp::Reset {
            mode: ResetMode::Hard,
            target: oid(9),
        }]
    );

    // Keep mode via mnemonic.
    ts.dispatch_many(actions.clone());
    ts.send_key(KeyCode::Char('k'), KeyModifiers::NONE);
    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(
        ts.take_git_ops(),
        vec![GitOp::Reset {
            mode: ResetMode::Keep,
            target: oid(9),
        }]
    );
}

#[test]
fn interactive_rebase_dialog_reorders_and_sets_actions() {
    let mut ts = TestSession::with_snapshot(snapshot());
    ts.dispatch(Action::SelectLogRow(2)); // c10
    let items = crate::menus::log_commit_menu_items(ts.state());
    let ContextMenuEntryKind::Action(actions) =
        &find_item(&items, "Interactive Rebase from Here").kind
    else {
        panic!()
    };
    let _ = ts.take_requests();
    ts.dispatch_many(actions.clone());
    let buf = ts.render(140, 40);
    let text = screen_text(&buf);
    assert!(text.contains("Interactive Rebase from"), "{text}");
    // Entries: c10, c11, c12. Move c10 down, drop c11 (now first), fixup c12.
    ts.send_key(KeyCode::Down, KeyModifiers::ALT);
    ts.send_key(KeyCode::Up, KeyModifiers::NONE);
    ts.send_key(KeyCode::Char('d'), KeyModifiers::NONE);
    ts.send_key(KeyCode::Down, KeyModifiers::NONE);
    ts.send_key(KeyCode::Down, KeyModifiers::NONE);
    ts.send_key(KeyCode::Char('f'), KeyModifiers::NONE);
    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);
    let ops = ts.take_git_ops();
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
    ts.dispatch_many(actions.clone());
    ts.send_key(KeyCode::Char('s'), KeyModifiers::NONE);
    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);
    assert!(ts.take_git_ops().is_empty());
    assert!(matches!(
        ts.state().modal_stack.last(),
        Some(ModalLayer::Dialog(_))
    ));
}
