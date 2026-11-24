//! Spec 12 #05 (Merge options dialog) and #06 (Update Project + Update Info).

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use jbgit_core::{
    FastForward, GitFailure, GitOp, GitOpOutcome, MergeOptions, OngoingKind, OngoingStep, PaneId,
    RefKind, RepoSnapshot, SkippedBranch, UpdateInfo, UpdateMode, UpdatedRange, WorkspaceTab,
};
use jbgit_engine::EngineEvent;

use crate::app::{Action, ModalLayer};
use crate::dialogs::{Dialog, MergeDialog};
use crate::menus::branch_menu_items;

use super::{
    TestSession, find_item, item_run_actions, local, make_commit, oid, screen_text, snapshot,
};

/// Default fixture + `topic` (one commit off `c5`) and a real `common_dir`.
fn merge_snapshot(common_dir: &std::path::Path) -> RepoSnapshot {
    let mut snap = snapshot();
    snap.commits
        .insert(0, make_commit(20, vec![oid(5)], "topic work"));
    snap.refs.push(local("topic", 20));
    snap.common_dir = common_dir.to_path_buf();
    snap
}

fn merge_dialog(ts: &TestSession) -> &MergeDialog {
    match ts.state().modal_stack.last() {
        Some(ModalLayer::Dialog(d)) => match d.as_ref() {
            Dialog::Merge(m) => m,
            other => panic!("expected Merge dialog, got {other:?}"),
        },
        other => panic!("expected dialog, got {other:?}"),
    }
}

fn key(ts: &mut TestSession, c: char) {
    ts.send_key(KeyCode::Char(c), KeyModifiers::NONE);
}

#[test]
fn merge_dialog_blocks_incompatible_options_and_remembers_choices() {
    let dir = tempfile::tempdir().unwrap();
    let mut ts = TestSession::with_snapshot(merge_snapshot(dir.path()));

    // Quick Merge keeps the legacy default options; `Merge…` opens the dialog.
    let items = branch_menu_items(ts.state(), "topic", RefKind::LocalBranch);
    let quick = find_item(&items, "Merge 'topic' into 'main'");
    assert!(matches!(
        item_run_actions(quick).as_slice(),
        [Action::RunGitOp(GitOp::Merge { options, .. })] if *options == MergeOptions::default()
    ));
    let dots = items
        .iter()
        .find(|i| i.label.ends_with('…') && i.label.starts_with("Merge 'topic'"))
        .expect("Merge… item");
    ts.dispatch_many(item_run_actions(dots));
    assert_eq!(merge_dialog(&ts).commit_count, Some(1));
    assert_eq!(merge_dialog(&ts).current, "main");

    // --squash + --no-ff clash: error shown, Enter does nothing.
    key(&mut ts, 's');
    key(&mut ts, 'f');
    assert_eq!(merge_dialog(&ts).options.ff, FastForward::NoFf);
    let text = screen_text(&ts.render(120, 40));
    assert!(
        text.contains("--squash cannot be combined with --no-ff"),
        "{text}"
    );
    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);
    assert!(ts.take_git_ops().is_empty());
    assert!(matches!(
        ts.state().modal_stack.last(),
        Some(ModalLayer::Dialog(_))
    ));

    // Cycle back to the default fast-forward policy, add a strategy.
    key(&mut ts, 'f');
    key(&mut ts, 'f');
    assert_eq!(merge_dialog(&ts).options.ff, FastForward::Default);
    key(&mut ts, 't');
    assert_eq!(merge_dialog(&ts).options.strategy.as_deref(), Some("ort"));
    let text = screen_text(&ts.render(120, 40));
    assert!(text.contains("git merge --squash -s ort topic"), "{text}");

    // Message row captures typing (incl. mnemonic letters) and blocks with --squash.
    key(&mut ts, 'm');
    ts.type_str("fs");
    assert_eq!(merge_dialog(&ts).message_text(), "fs");
    assert!(merge_dialog(&ts).options.validate().is_err());
    ts.send_key(KeyCode::Backspace, KeyModifiers::NONE);
    ts.send_key(KeyCode::Backspace, KeyModifiers::NONE);
    assert!(merge_dialog(&ts).options.message.is_none());

    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);
    let ops = ts.take_git_ops();
    assert_eq!(
        ops,
        vec![GitOp::Merge {
            rev: "topic".into(),
            options: MergeOptions {
                squash: true,
                strategy: Some("ort".into()),
                ..Default::default()
            },
        }]
    );
    assert!(ts.state().modal_stack.is_empty());
    let saved = std::fs::read_to_string(dir.path().join("jbgit-merge-options")).unwrap();
    assert!(
        saved.contains("squash=true") && saved.contains("strategy=ort"),
        "{saved}"
    );

    // Re-opening restores the remembered choices.
    ts.dispatch(Action::PromptMerge {
        rev: "topic".into(),
    });
    let reopened = merge_dialog(&ts);
    assert!(reopened.options.squash);
    assert_eq!(reopened.options.strategy.as_deref(), Some("ort"));
    assert!(reopened.options.message.is_none());

    // Esc closes without running anything.
    ts.send_key(KeyCode::Esc, KeyModifiers::NONE);
    assert!(ts.state().modal_stack.is_empty());
    assert!(ts.take_git_ops().is_empty());
}

#[test]
fn squash_result_prefills_commit_editor_and_ff_only_failure_offers_recovery() {
    let dir = tempfile::tempdir().unwrap();
    let mut ts = TestSession::with_snapshot(merge_snapshot(dir.path()));
    let squash = GitOp::Merge {
        rev: "topic".into(),
        options: MergeOptions {
            squash: true,
            ..Default::default()
        },
    };
    let mut out = GitOpOutcome::ok(squash);
    out.prepared_message = Some("Squashed commit of the following:\n\ncommit abc".into());
    ts.deliver_engine_event(EngineEvent::GitOpFinished(out));
    assert_eq!(ts.state().active_tab, WorkspaceTab::Commit);
    assert_eq!(
        ts.state().stage_workbench.editor().text(),
        "Squashed commit of the following:\n\ncommit abc"
    );
    let note = ts.state().notification.as_ref().expect("staged notice");
    assert!(note.text.contains("staged"), "{}", note.text);
    // No "delete merged branch?" cleanup prompt for an uncommitted squash.
    assert!(ts.state().modal_stack.is_empty());

    let ff_only = GitOp::Merge {
        rev: "topic".into(),
        options: MergeOptions {
            ff: FastForward::FfOnly,
            allow_unrelated: true,
            ..Default::default()
        },
    };
    ts.deliver_engine_event(EngineEvent::GitOpFinished(GitOpOutcome::failed(
        ff_only,
        GitFailure::NotFastForward,
    )));
    let Some(ModalLayer::Dialog(d)) = ts.state().modal_stack.last() else {
        panic!("expected recovery dialog");
    };
    let Dialog::Choice(c) = d.as_ref() else {
        panic!("expected choice dialog");
    };
    assert_eq!(c.title, "Cannot Fast-forward");
    assert_eq!(c.options.len(), 3);
    assert!(matches!(
        c.options[1].actions.as_slice(),
        [Action::RunGitOp(GitOp::Rebase { options })]
            if options.onto == "topic" && options.branch.is_none()
    ));
    key(&mut ts, 'm');
    assert_eq!(
        ts.take_git_ops(),
        vec![GitOp::Merge {
            rev: "topic".into(),
            options: MergeOptions {
                ff: FastForward::Default,
                allow_unrelated: true,
                ..Default::default()
            },
        }]
    );
}

#[test]
fn update_info_view_filters_log_to_updated_range_and_chip_clears_it() {
    let mut ts = TestSession::with_snapshot(snapshot());
    let info = UpdateInfo {
        ranges: vec![UpdatedRange {
            branch: "main".into(),
            before: oid(9),
            after: oid(12),
        }],
        skipped: vec![SkippedBranch {
            branch: "feature/dag-layout".into(),
            reason: "diverged from 'origin/feature/dag-layout'".into(),
        }],
        new_commits: 3,
        files_changed: 2,
        commits: vec![oid(12), oid(11), oid(10)],
    };
    let mut out = GitOpOutcome::ok(GitOp::UpdateProject {
        mode: Some(UpdateMode::Merge),
        save_changes: true,
        update_other_branches: true,
        update_submodules: false,
    });
    out.update_info = Some(info);
    ts.deliver_engine_event(EngineEvent::GitOpFinished(out));
    let note = ts
        .state()
        .notification
        .clone()
        .expect("update notification");
    assert!(
        note.text.starts_with("Updated 3 commit(s) in 2 file(s)"),
        "{}",
        note.text
    );
    assert!(
        note.text
            .contains("skipped 1 branch(es): feature/dag-layout"),
        "{}",
        note.text
    );
    assert_eq!(note.action.as_ref().map(|a| a.0.as_str()), Some("View"));

    ts.dispatch(Action::SwitchTab(WorkspaceTab::Commit));
    ts.dispatch(Action::NotificationActivate);
    assert_eq!(ts.state().active_tab, WorkspaceTab::Log);
    let range = ts
        .state()
        .log
        .filter
        .spec
        .range
        .clone()
        .expect("range filter");
    assert_eq!(range.len(), 3);
    let visible = ts
        .state()
        .log
        .graph
        .cached
        .as_ref()
        .map(|c| c.visible_count())
        .unwrap_or_default();
    assert_eq!(visible, 3);
    let text = screen_text(&ts.render(160, 40));
    assert!(text.contains("Range:"), "{text}");

    let (x, y) = ts
        .hit_center_of(|t| matches!(t, crate::hit_test::HitTarget::FilterRangeClear))
        .expect("range chip ×");
    ts.send_mouse_event(super::left_click(x, y, KeyModifiers::NONE));
    assert!(ts.state().log.filter.spec.range.is_none());
    let visible = ts
        .state()
        .log
        .graph
        .cached
        .as_ref()
        .map(|c| c.visible_count())
        .unwrap_or_default();
    assert_eq!(visible, 12);

    // Nothing new → "up to date" without a View button.
    let mut empty = GitOpOutcome::ok(GitOp::UpdateProject {
        mode: None,
        save_changes: true,
        update_other_branches: true,
        update_submodules: false,
    });
    empty.update_info = Some(UpdateInfo::default());
    ts.deliver_engine_event(EngineEvent::GitOpFinished(empty));
    let note = ts.state().notification.clone().unwrap();
    assert_eq!(note.text, "All files are up to date");
    assert!(note.action.is_none());
}

#[test]
fn update_project_entry_points_and_stash_restore_after_conflicts() {
    let mut ts = TestSession::with_snapshot(snapshot());

    // Branches popup: "Update Project…" row first, opening the mode chooser.
    ts.dispatch(Action::OpenBranchesPopup {
        query: String::new(),
    });
    let rows = crate::branches_popup::build_popup_rows(ts.state(), "");
    assert_eq!(rows[0], crate::branches_popup::PopupRow::UpdateProject);
    ts.dispatch_many(crate::branches_popup::activate_row(ts.state(), "", 0));
    let Some(ModalLayer::Dialog(d)) = ts.state().modal_stack.last() else {
        panic!("expected update dialog");
    };
    let Dialog::Choice(c) = d.as_ref() else {
        panic!("expected choice");
    };
    assert!(c.radio);
    assert_eq!(c.title, "Update Project");
    assert_eq!(c.selected, 0, "[update] mode = git-config by default");
    key(&mut ts, 'r');
    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(
        ts.take_git_ops(),
        vec![GitOp::UpdateProject {
            mode: Some(UpdateMode::Rebase),
            save_changes: true,
            update_other_branches: true,
            update_submodules: false,
        }]
    );

    // Header `↓N` badge opens the same chooser.
    let mut behind = snapshot();
    behind.refs[0].ahead_behind = Some((1, 3));
    ts.deliver_engine_event(EngineEvent::RepoSnapshotLoaded {
        snapshot: std::sync::Arc::new(behind),
        epoch: 2,
    });
    let text = screen_text(&ts.render(160, 40));
    assert!(text.contains("↓3"), "{text}");
    let (x, y) = ts
        .hit_center_of(|t| matches!(t, crate::hit_test::HitTarget::HeaderBehindBadge))
        .expect("behind badge");
    ts.send_mouse_event(super::left_click(x, y, KeyModifiers::NONE));
    assert!(matches!(
        ts.state().modal_stack.last(),
        Some(ModalLayer::Dialog(d)) if matches!(d.as_ref(), Dialog::Choice(c) if c.title == "Update Project")
    ));
    ts.send_key(KeyCode::Esc, KeyModifiers::NONE);

    // Conflict with kept stash → pop automatically after Continue succeeds.
    let op = GitOp::UpdateProject {
        mode: None,
        save_changes: true,
        update_other_branches: true,
        update_submodules: false,
    };
    let mut failed = GitOpOutcome::failed(op, GitFailure::Conflicts);
    failed.stash_kept = true;
    ts.deliver_engine_event(EngineEvent::GitOpFinished(failed));
    let note = ts.state().notification.clone().unwrap();
    assert!(note.text.contains("jbgit update project"), "{}", note.text);
    assert!(ts.state().pending_update_stash);
    ts.deliver_engine_event(EngineEvent::GitOpFinished(GitOpOutcome::ok(
        GitOp::Ongoing {
            kind: OngoingKind::Merge,
            step: OngoingStep::Continue,
        },
    )));
    assert!(!ts.state().pending_update_stash);
    assert_eq!(
        ts.take_git_ops(),
        vec![GitOp::StashApply {
            index: 0,
            pop: true,
            reinstate_index: false,
        }]
    );
}

#[test]
fn alt_u_runs_update_project_in_every_preset_and_jetbrains_ctrl_t_stays_pull() {
    use crate::config::KeymapPreset;
    use crate::keymap::{KeyAction, actions::check_scopes};

    for preset in [
        KeymapPreset::Default,
        KeymapPreset::Jetbrains,
        KeymapPreset::Vim,
    ] {
        let mut cfg = crate::config::AppConfig::load_defaults();
        cfg.keymap = crate::config::KeymapConfig::for_preset(preset.clone());
        let dir = tempfile::tempdir().unwrap();
        let mut ts = TestSession::with_config(dir.path().to_path_buf(), cfg);
        ts.deliver_engine_event(EngineEvent::RepoSnapshotLoaded {
            snapshot: std::sync::Arc::new(snapshot()),
            epoch: 1,
        });
        for (tab, pane) in [
            (WorkspaceTab::Log, PaneId::LogCommitTable),
            (WorkspaceTab::Commit, PaneId::CommitChangesTree),
        ] {
            ts.dispatch(Action::SwitchTab(tab));
            ts.dispatch(Action::FocusPane(pane));
            let k = KeyEvent::new(KeyCode::Char('u'), KeyModifiers::ALT);
            let m = check_scopes(&[k], ts.state());
            assert!(
                matches!(m, crate::keymap::MatchResult::Exact(b) if b.run == KeyAction::UpdateProject),
                "<A-u> on {preset:?} / {tab:?}"
            );
        }
        ts.send_key(KeyCode::Char('u'), KeyModifiers::ALT);
        assert_eq!(
            ts.take_git_ops(),
            vec![GitOp::UpdateProject {
                mode: None,
                save_changes: true,
                update_other_branches: true,
                update_submodules: false,
            }],
            "{preset:?}"
        );
        if preset == KeymapPreset::Jetbrains {
            let k = KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL);
            let m = check_scopes(&[k], ts.state());
            assert!(
                matches!(m, crate::keymap::MatchResult::Exact(b) if b.run == KeyAction::GitPull)
            );
        }
    }
}

#[test]
fn update_project_upstream_picker_and_submodules_checkbox() {
    // 1. Missing upstream opens Select Tracking Branch picker -> SetUpstream + UpdateProject.
    let mut no_up = snapshot();
    no_up.refs[0].upstream = None;
    let mut ts = TestSession::with_snapshot(no_up);
    ts.dispatch(Action::PromptUpdateProject);
    match ts.state().modal_stack.last() {
        Some(ModalLayer::Dialog(d)) => match d.as_ref() {
            Dialog::Choice(c) => {
                assert_eq!(c.title, "Select Tracking Branch");
                assert!(c.checkbox.is_none(), "no submodules -> no checkbox");
                assert_eq!(c.options[0].label, "origin/main");
            }
            other => panic!("expected ChoiceDialog, got {other:?}"),
        },
        other => panic!("expected Dialog, got {other:?}"),
    }
    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(
        ts.take_git_ops(),
        vec![GitOp::Sequence(vec![
            GitOp::SetUpstream {
                branch: "main".into(),
                upstream: "origin/main".into(),
            },
            GitOp::UpdateProject {
                mode: None,
                save_changes: true,
                update_other_branches: true,
                update_submodules: false,
            },
        ])]
    );

    // 2. Gone upstream (`origin/deleted` absent from refs) also triggers picker.
    let mut gone_up = snapshot();
    gone_up.refs[0].upstream = Some("origin/deleted".into());
    let mut ts_gone = TestSession::with_snapshot(gone_up);
    ts_gone.dispatch(Action::PromptUpdateProject);
    match ts_gone.state().modal_stack.last() {
        Some(ModalLayer::Dialog(d)) => match d.as_ref() {
            Dialog::Choice(c) => {
                assert_eq!(c.title, "Select Tracking Branch");
                assert!(c.body.iter().any(|l| l.contains("gone")), "{:?}", c.body);
            }
            other => panic!("expected ChoiceDialog, got {other:?}"),
        },
        other => panic!("expected Dialog, got {other:?}"),
    }

    // 3. When repo has submodules, Update Project shows Update Submodules checkbox
    //    and checking it + selecting Rebase emits `update_submodules: true`.
    let mut sub_snap = snapshot();
    sub_snap.submodules = vec!["libs/core".into()];
    let mut ts_sub = TestSession::with_snapshot(sub_snap);
    ts_sub.dispatch(Action::PromptUpdateProject);
    match ts_sub.state().modal_stack.last() {
        Some(ModalLayer::Dialog(d)) => match d.as_ref() {
            Dialog::Choice(c) => {
                assert_eq!(c.title, "Update Project");
                assert!(c.checkbox.is_some(), "submodules present -> checkbox visible");
            }
            other => panic!("expected ChoiceDialog, got {other:?}"),
        },
        other => panic!("expected Dialog, got {other:?}"),
    }
    ts_sub.dispatch(Action::DialogToggleCheckbox);
    key(&mut ts_sub, 'r');
    ts_sub.send_key(KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(
        ts_sub.take_git_ops(),
        vec![GitOp::UpdateProject {
            mode: Some(UpdateMode::Rebase),
            save_changes: true,
            update_other_branches: true,
            update_submodules: true,
        }]
    );
}
