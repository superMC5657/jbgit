//! Spec 13 #05: Pull options dialog, standard Rebase dialog, Log Rebase onto
//! Selected Commit, Fixup!/Squash! split, non-contiguous Squash, Interactive
//! Rebase `edit`/`update-ref`/diff preview, and keymap conflict assertions.

use std::sync::Arc;

use crossterm::event::{KeyCode, KeyModifiers};
use jbgit_core::{
    CommitChangesBundle, CommitDetails, DiffContext, DiffHunk, DiffLine, DiffLineKind, FastForward,
    FileChange, FileDiff, FileDiffKey, FileStatusKind, FixupKind, GitOp, GitOpOutcome, GitVersion,
    OngoingKind, OngoingStep, PullOptions, RebaseOptions, RefInfo, RefKind, RepoSnapshot,
    RequestSlotId, StageEntry, StageStatusSnapshot, TodoAction, TodoEntry, WorkspaceTab,
};
use jbgit_engine::{EngineEvent, TaskRequest};

use crate::app::{Action, ContextMenuEntryKind, ModalLayer};
use crate::config::{KeymapConfig, KeymapPreset, KeymapScope};
use crate::dialogs::{Dialog, PullDialog, RebaseDialog, RebaseTodoDialog};
use crate::keymap::KeyAction;
use crate::menus::{branch_menu_items, log_commit_menu_items};

use super::{
    TestSession, find_item, item_run_actions, local, make_commit, oid, remote_infos, screen_text,
    snapshot,
};

fn pull_rebase_snapshot(common_dir: &std::path::Path) -> RepoSnapshot {
    let mut snap = snapshot();
    snap.common_dir = common_dir.to_path_buf();
    snap.remotes = remote_infos(&["origin", "upstream"]);
    snap.commits
        .insert(0, make_commit(20, vec![oid(5)], "topic work"));
    snap.refs.push(local("topic", 20));
    snap.refs.push(RefInfo {
        name: "origin/dev".into(),
        full_name: "refs/remotes/origin/dev".into(),
        kind: RefKind::RemoteBranch,
        target_oid: oid(2),
        is_head: false,
        upstream: None,
        ahead_behind: None,
    });
    snap.refs.push(RefInfo {
        name: "upstream/release".into(),
        full_name: "refs/remotes/upstream/release".into(),
        kind: RefKind::RemoteBranch,
        target_oid: oid(2),
        is_head: false,
        upstream: None,
        ahead_behind: None,
    });
    snap
}

fn pull_dialog(ts: &TestSession) -> &PullDialog {
    match ts.state().modal_stack.last() {
        Some(ModalLayer::Dialog(d)) => match d.as_ref() {
            Dialog::Pull(p) => p,
            other => panic!("expected Pull dialog, got {other:?}"),
        },
        other => panic!("expected dialog, got {other:?}"),
    }
}

fn rebase_dialog(ts: &TestSession) -> &RebaseDialog {
    match ts.state().modal_stack.last() {
        Some(ModalLayer::Dialog(d)) => match d.as_ref() {
            Dialog::Rebase(r) => r,
            other => panic!("expected Rebase dialog, got {other:?}"),
        },
        other => panic!("expected dialog, got {other:?}"),
    }
}

#[test]
fn pull_dialog_blocks_conflicts_fetches_completes_and_remembers_choices() {
    let dir = tempfile::tempdir().unwrap();
    let mut ts = TestSession::with_snapshot(pull_rebase_snapshot(dir.path()));

    // Quick Pull (`p`) still runs default `GitOp::Pull { options: PullOptions::default() }`.
    ts.send_key(KeyCode::Char('p'), KeyModifiers::NONE);
    assert_eq!(
        ts.take_git_ops(),
        vec![GitOp::Pull {
            options: PullOptions::default()
        }]
    );

    // `Pull…` opens via branch menu on current branch (HEAD / main) and via `<A-l>`.
    let items = branch_menu_items(ts.state(), "main", RefKind::LocalBranch);
    let pull_dots = find_item(&items, "Pull…");
    assert!(pull_dots.is_enabled());
    ts.dispatch_many(item_run_actions(pull_dots));
    assert_eq!(pull_dialog(&ts).options.remote.as_deref(), Some("origin"));
    assert_eq!(pull_dialog(&ts).options.branch.as_deref(), Some("main"));

    // Conflict matrix: --rebase + --squash blocks confirm and shows red error.
    ts.send_key(KeyCode::Char('e'), KeyModifiers::NONE); // --rebase
    ts.send_key(KeyCode::Char('s'), KeyModifiers::NONE); // --squash
    let text = screen_text(&ts.render(120, 40));
    assert!(
        text.contains("--rebase cannot be combined with --squash"),
        "{text}"
    );
    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);
    assert!(ts.take_git_ops().is_empty());
    assert!(matches!(
        ts.state().modal_stack.last(),
        Some(ModalLayer::Dialog(_))
    ));

    // Uncheck --squash, check --no-verify, switch remote to upstream -> branch adapts to release.
    ts.send_key(KeyCode::Char('s'), KeyModifiers::NONE);
    ts.send_key(KeyCode::Char('v'), KeyModifiers::NONE);
    ts.send_key(KeyCode::Char('r'), KeyModifiers::NONE);
    assert_eq!(pull_dialog(&ts).options.remote.as_deref(), Some("upstream"));
    assert_eq!(pull_dialog(&ts).options.branch.as_deref(), Some("release"));

    // Ctrl+R triggers FetchRemote without closing dialog.
    ts.send_key(KeyCode::Char('r'), KeyModifiers::CONTROL);
    assert_eq!(
        ts.take_git_ops(),
        vec![GitOp::FetchRemote {
            name: "upstream".into()
        }]
    );
    assert!(pull_dialog(&ts).fetching);

    // A failed FetchRemote must clear `fetching` so [Fetch] does not stay stuck.
    ts.deliver_engine_event(EngineEvent::GitOpFinished(GitOpOutcome {
        op: GitOp::FetchRemote {
            name: "upstream".into(),
        },
        success: false,
        stdout: Vec::new(),
        stderr: vec!["fatal: unable to access remote".into()],
        failure: Some(jbgit_core::GitFailure::Other("network unreachable".into())),
        stash_kept: false,
        update_info: None,
        prepared_message: None,
        paused_for_edit: false,
    }));
    assert!(!pull_dialog(&ts).fetching);

    // Command preview matches expected `git pull` invocation.
    let preview = screen_text(&ts.render(120, 40));
    assert!(
        preview.contains("git pull --rebase --no-verify upstream release"),
        "{preview}"
    );

    // Enter confirms and saves options to `<common_dir>/jbgit-pull-options`.
    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(
        ts.take_git_ops(),
        vec![GitOp::Pull {
            options: PullOptions {
                remote: Some("upstream".into()),
                branch: Some("release".into()),
                rebase: true,
                ff: FastForward::Default,
                squash: false,
                no_commit: false,
                no_verify: true,
            }
        }]
    );
    assert!(ts.state().modal_stack.is_empty());
    let saved = std::fs::read_to_string(dir.path().join("jbgit-pull-options")).unwrap();
    assert!(
        saved.contains("rebase=true") && saved.contains("no_verify=true"),
        "{saved}"
    );

    // Re-opening via global shortcut `<A-l>` restores remembered flags.
    ts.send_key(KeyCode::Char('l'), KeyModifiers::ALT);
    let reopened = pull_dialog(&ts);
    assert!(reopened.options.rebase);
    assert!(reopened.options.no_verify);
    assert_eq!(reopened.options.remote.as_deref(), Some("origin"));
}

#[test]
fn rebase_dialog_validates_version_root_onto_and_remembers_choices() {
    let dir = tempfile::tempdir().unwrap();
    let mut ts = TestSession::with_snapshot(pull_rebase_snapshot(dir.path()));
    ts.state_mut().git_version = Some(GitVersion::new(2, 37, 0));

    // Open `Rebase…` from non-current branch menu -> prefills `onto = "topic"`.
    let items = branch_menu_items(ts.state(), "topic", RefKind::LocalBranch);
    let rebase_dots = find_item(&items, "Rebase…");
    ts.dispatch_many(item_run_actions(rebase_dots));
    assert_eq!(rebase_dialog(&ts).options.onto, "topic");

    // Move off text rows (row 0..2 are Onto/Upstream/Branch), toggle --update-refs -> blocked on Git 2.37.
    ts.send_key(KeyCode::Down, KeyModifiers::NONE); // Upstream
    ts.send_key(KeyCode::Down, KeyModifiers::NONE); // Branch
    ts.send_key(KeyCode::Down, KeyModifiers::NONE); // Interactive
    ts.send_key(KeyCode::Char('r'), KeyModifiers::NONE); // UpdateRefs
    assert!(rebase_dialog(&ts).options.update_refs);
    let text = screen_text(&ts.render(120, 40));
    assert!(text.contains("Git ≥ 2.38"), "{text}");
    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);
    assert!(ts.take_git_ops().is_empty());

    // Upgrade Git version to 2.40 and re-open `Rebase…` -> --update-refs is now valid!
    ts.send_key(KeyCode::Esc, KeyModifiers::NONE);
    ts.state_mut().git_version = Some(GitVersion::new(2, 40, 0));
    ts.dispatch_many(item_run_actions(rebase_dots));
    ts.send_key(KeyCode::Down, KeyModifiers::NONE); // Upstream
    ts.type_str("origin/main");
    // Move to non-text row and toggle --update-refs and --autosquash.
    ts.send_key(KeyCode::Down, KeyModifiers::NONE); // Branch
    ts.send_key(KeyCode::Down, KeyModifiers::NONE); // Interactive
    ts.send_key(KeyCode::Char('r'), KeyModifiers::NONE); // UpdateRefs
    ts.send_key(KeyCode::Char('a'), KeyModifiers::NONE); // Autosquash

    let preview = screen_text(&ts.render(120, 40));
    assert!(
        preview.contains("git rebase -i --update-refs --autosquash --onto topic origin/main"),
        "{preview}"
    );

    // Toggling --root while upstream is set shows RootWithUpstream conflict error.
    ts.send_key(KeyCode::Char('t'), KeyModifiers::NONE); // Root
    let err_text = screen_text(&ts.render(120, 40));
    assert!(err_text.contains("--root cannot be combined"), "{err_text}");
    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);
    assert!(ts.take_git_ops().is_empty());

    // Turn off --root and confirm.
    ts.send_key(KeyCode::Char('t'), KeyModifiers::NONE);
    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(
        ts.take_git_ops(),
        vec![GitOp::Rebase {
            options: RebaseOptions {
                onto: "topic".into(),
                upstream: Some("origin/main".into()),
                branch: None,
                interactive: false,
                rebase_merges: false,
                update_refs: true,
                keep_empty: false,
                autosquash: true,
                root: false,
            }
        }]
    );
    let saved = std::fs::read_to_string(dir.path().join("jbgit-rebase-options")).unwrap();
    assert!(
        saved.contains("update_refs=true") && saved.contains("autosquash=true"),
        "{saved}"
    );

    // Re-open via `<A-e>` restores flags and leaves onto empty until entered.
    ts.send_key(KeyCode::Char('e'), KeyModifiers::ALT);
    let reopened = rebase_dialog(&ts);
    assert!(reopened.options.update_refs);
    assert!(reopened.options.autosquash);
    assert!(reopened.options.onto.is_empty());
}

#[test]
fn log_menu_rebase_onto_commit_fixup_squash_split_and_non_contiguous_squash() {
    let dir = tempfile::tempdir().unwrap();
    let mut ts = TestSession::with_snapshot(pull_rebase_snapshot(dir.path()));
    // Stage a file so Fixup! / Squash! / Fixup & Autosquash are enabled.
    let mut status = StageStatusSnapshot::default();
    status.staged.push(StageEntry {
        path: "src/lib.rs".into(),
        old_path: None,
        kind: FileStatusKind::Modified,
    });
    ts.deliver_engine_event(EngineEvent::StageStatusLoaded {
        status: Arc::new(status),
        epoch: 1,
    });

    // Row 0 is `topic` (oid_n(20), NOT an ancestor of HEAD `main` oid 12).
    let topic_oid = ts.state().log_commit_at(0).unwrap().oid;
    ts.dispatch(Action::SelectLogRow(0));
    let items_topic = log_commit_menu_items(ts.state());
    let rebase_onto = find_item(&items_topic, "Rebase 'main' onto Selected Commit");
    assert!(
        rebase_onto.is_enabled(),
        "{:?}",
        rebase_onto.disabled_reason
    );
    ts.dispatch_many(item_run_actions(rebase_onto));
    assert_eq!(
        ts.take_git_ops(),
        vec![GitOp::Rebase {
            options: RebaseOptions::onto(topic_oid.to_string(), None),
        }]
    );

    // Row 1 is `main` HEAD (oid 12): Rebase onto Selected Commit must be disabled (already in branch),
    // while Fixup! Commit, Squash! Commit, and Fixup & Autosquash are enabled!
    ts.dispatch(Action::SelectLogRow(1));
    let items_head = log_commit_menu_items(ts.state());
    assert!(
        find_item(&items_head, "Rebase 'main' onto Selected Commit")
            .disabled_reason
            .is_some()
    );
    let fixup_only = find_item(&items_head, "Fixup! Commit");
    let squash_only = find_item(&items_head, "Squash! Commit");
    let fixup_auto = find_item(&items_head, "Fixup & Autosquash…");
    assert!(fixup_only.is_enabled());
    assert!(squash_only.is_enabled());
    assert!(fixup_auto.is_enabled());

    assert!(matches!(
        item_run_actions(fixup_only).as_slice(),
        [Action::RunGitOp(GitOp::FixupCommit {
            target,
            kind: FixupKind::Fixup,
            base: Some(b),
            autosquash: false,
        })] if *target == oid(12) && *b == oid(11)
    ));
    assert!(matches!(
        item_run_actions(squash_only).as_slice(),
        [Action::RunGitOp(GitOp::FixupCommit {
            target,
            kind: FixupKind::Squash,
            base: Some(b),
            autosquash: false,
        })] if *target == oid(12) && *b == oid(11)
    ));
    assert!(matches!(
        item_run_actions(fixup_auto).as_slice(),
        [Action::RunGitOp(GitOp::FixupCommit {
            target,
            kind: FixupKind::Fixup,
            base: Some(b),
            autosquash: true,
        })] if *target == oid(12) && *b == oid(11)
    ));

    // Non-contiguous Squash: select row 1 (c12) and row 3 (c10), skipping row 2 (c11).
    ts.dispatch_many([Action::SelectLogRow(1), Action::ToggleLogRow(3)]);
    let items_non_contig = log_commit_menu_items(ts.state());
    let sq_item = find_item(&items_non_contig, "Squash 2 Commits…");
    assert!(sq_item.is_enabled(), "{:?}", sq_item.disabled_reason);
    let ContextMenuEntryKind::Action(acts) = &sq_item.kind else {
        panic!()
    };
    ts.dispatch_many(acts.clone());
    let Some(ModalLayer::Dialog(d)) = ts.state().modal_stack.last() else {
        panic!("expected squash message dialog");
    };
    let Dialog::Message(m) = d.as_ref() else {
        panic!("expected MessageDialog");
    };
    assert_eq!(m.text, "c10\n\nc12");
    ts.send_key(KeyCode::Enter, KeyModifiers::ALT);
    let ops = ts.take_git_ops();
    let [
        GitOp::RewriteHistory {
            base,
            todo,
            no_verify,
        },
    ] = ops.as_slice()
    else {
        panic!("{ops:?}");
    };
    assert_eq!(*base, Some(oid(9)));
    assert!(!no_verify);
    let plan: Vec<(jbgit_core::Oid, TodoAction)> = todo.iter().map(|e| (e.oid, e.action)).collect();
    assert_eq!(
        plan,
        vec![
            (oid(10), TodoAction::Pick),
            (oid(12), TodoAction::Squash),
            (oid(11), TodoAction::Pick),
        ]
    );
}

#[test]
fn interactive_rebase_dialog_edit_update_ref_file_preview_diff_and_edit_pause_guidance() {
    let mut ts = TestSession::with_snapshot(snapshot());
    let mut dlg = RebaseTodoDialog::new(
        Some(oid(9)),
        vec![
            TodoEntry::pick(oid(10), "c10"),
            TodoEntry::update_ref("refs/heads/stacked"),
            TodoEntry::pick(oid(11), "c11"),
        ],
        vec!["c10".into(), String::new(), "c11".into()],
    );
    dlg.selected = 0;
    let _ = ts.take_requests();
    ts.dispatch(Action::OpenDialog(Box::new(Dialog::RebaseTodo(dlg))));

    // Opening immediately requests commit details on RebaseCommitDetails slot.
    let reqs = ts.take_requests();
    let (details_key, details_epoch) = reqs
        .iter()
        .find_map(|(r, ep)| match r {
            TaskRequest::LoadCommitDetails {
                slot: RequestSlotId::RebaseCommitDetails,
                key,
                ..
            } => Some((key.clone(), *ep)),
            _ => None,
        })
        .expect("RebaseCommitDetails request");
    assert_eq!(details_key.oids, vec![oid(10)]);

    // Deliver CommitDetailsLoaded -> renders changed files summary in bottom preview!
    ts.deliver_engine_event(EngineEvent::CommitDetailsLoaded {
        slot: RequestSlotId::RebaseCommitDetails,
        epoch: details_epoch.unwrap_or(1),
        bundle: Arc::new(CommitChangesBundle {
            key: details_key.clone(),
            details: vec![CommitDetails::default()],
            changes: vec![FileChange {
                path: "src/engine.rs".into(),
                old_path: None,
                kind: FileStatusKind::Modified,
                additions: 4,
                deletions: 1,
                is_binary: false,
                exists_in_workdir: true,
                old_oid: None,
                new_oid: None,
            }],
        }),
    });
    let summary_screen = screen_text(&ts.render(120, 40));
    assert!(summary_screen.contains("src/engine.rs"), "{summary_screen}");
    assert!(summary_screen.contains("+4 -1"), "{summary_screen}");

    // Press `e` to mark c10 as Edit.
    ts.send_key(KeyCode::Char('e'), KeyModifiers::NONE);
    // Press `d` to open full Diff view -> requests RebaseFileDiff.
    let _ = ts.take_requests();
    ts.send_key(KeyCode::Char('d'), KeyModifiers::NONE);
    let diff_reqs = ts.take_requests();
    let (diff_key, diff_epoch) = diff_reqs
        .iter()
        .find_map(|(r, ep)| match r {
            TaskRequest::LoadFileDiff {
                slot: RequestSlotId::RebaseFileDiff,
                key,
                ..
            } => Some((key.clone(), *ep)),
            _ => None,
        })
        .expect("RebaseFileDiff request");
    assert_eq!(diff_key.path, "src/engine.rs");

    ts.deliver_engine_event(EngineEvent::FileDiffLoaded {
        slot: RequestSlotId::RebaseFileDiff,
        epoch: diff_epoch.unwrap_or(1),
        diff: Arc::new(FileDiff {
            key: FileDiffKey {
                details: details_key,
                path: "src/engine.rs".into(),
                old_path: None,
                context: DiffContext::Three,
                whitespace: jbgit_core::WhitespaceMode::Default,
            },
            is_binary: false,
            binary: None,
            is_too_large: false,
            is_truncated: false,
            mode_change: None,
            lfs: None,
            hunks: vec![DiffHunk {
                header: "@@ -1,2 +1,3 @@".into(),
                lines: vec![DiffLine {
                    kind: DiffLineKind::Added,
                    old_lineno: None,
                    new_lineno: Some(2),
                    text: "let edited = true;".into(),
                    word_ranges: Vec::new(),
                }],
            }],
        }),
    });
    let diff_screen = screen_text(&ts.render(120, 40));
    assert!(diff_screen.contains("+let edited = true;"), "{diff_screen}");

    // Esc exits full Diff mode back to Todo list (dialog stays open!).
    ts.send_key(KeyCode::Esc, KeyModifiers::NONE);
    assert!(matches!(
        ts.state().modal_stack.last(),
        Some(ModalLayer::Dialog(d)) if matches!(d.as_ref(), Dialog::RebaseTodo(r) if !r.show_diff)
    ));

    // Move to UpdateRef row (row 1): pressing `x` or Space must NOT alter UpdateRef!
    ts.send_key(KeyCode::Down, KeyModifiers::NONE);
    ts.send_key(KeyCode::Char('x'), KeyModifiers::NONE);
    ts.send_key(KeyCode::Char(' '), KeyModifiers::NONE);
    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);
    let ops = ts.take_git_ops();
    let [GitOp::RewriteHistory { todo, .. }] = ops.as_slice() else {
        panic!("{ops:?}");
    };
    assert_eq!(todo[0].action, TodoAction::Edit);
    assert_eq!(todo[1].action, TodoAction::UpdateRef);
    assert_eq!(todo[1].subject, "refs/heads/stacked");
    assert_eq!(todo[2].action, TodoAction::Pick);

    // When engine reports `paused_for_edit: true`, TUI switches to Commit tab, enables Amend,
    // and surfaces a `[Continue Rebase]` notification action!
    let mut out = GitOpOutcome::ok(GitOp::RewriteHistory {
        base: Some(oid(9)),
        todo: todo.clone(),
        no_verify: false,
    });
    out.paused_for_edit = true;
    ts.deliver_engine_event(EngineEvent::GitOpFinished(out));
    assert_eq!(ts.state().active_tab, WorkspaceTab::Commit);
    assert!(ts.state().stage_workbench.amend());
    let note = ts
        .state()
        .notification
        .as_ref()
        .expect("edit pause notification");
    let (_, action_list) = note.action.as_ref().expect("continue rebase button");
    assert!(matches!(
        action_list.as_slice(),
        [Action::RunGitOp(GitOp::Ongoing {
            kind: OngoingKind::Rebase,
            step: OngoingStep::Continue,
        })]
    ));
}

#[test]
fn pull_and_rebase_dialog_shortcuts_have_zero_conflicts_across_all_presets() {
    for preset in [
        KeymapPreset::Default,
        KeymapPreset::Jetbrains,
        KeymapPreset::Vim,
    ] {
        let cfg = KeymapConfig::for_preset(preset.clone());
        let pull_key = cfg.primary_key_for(&[KeymapScope::Global], &KeyAction::GitPullDialog);
        let rebase_key = cfg.primary_key_for(&[KeymapScope::Global], &KeyAction::GitRebaseDialog);
        assert_eq!(pull_key.as_deref(), Some("Alt+L"), "preset {preset:?}");
        assert_eq!(rebase_key.as_deref(), Some("Alt+E"), "preset {preset:?}");

        // Verify neither <A-l> nor <A-e> is shadowed in any scope.
        for scope in [
            KeymapScope::Global,
            KeymapScope::Log,
            KeymapScope::Commit,
            KeymapScope::Branch,
            KeymapScope::Diff,
            KeymapScope::Stash,
            KeymapScope::Console,
        ] {
            for b in cfg.bindings_for(scope) {
                if !b.run.is_active() {
                    continue;
                }
                let disp = b.display_key().unwrap_or("");
                if disp == "Alt+L" {
                    assert_eq!(
                        b.run,
                        KeyAction::GitPullDialog,
                        "Alt+L shadowed by {:?} in {:?}/{:?}",
                        b.run,
                        preset,
                        scope
                    );
                }
                if disp == "Alt+E" {
                    assert_eq!(
                        b.run,
                        KeyAction::GitRebaseDialog,
                        "Alt+E shadowed by {:?} in {:?}/{:?}",
                        b.run,
                        preset,
                        scope
                    );
                }
            }
        }
    }
}
