use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEventKind};
use jbgit_core::{
    BisectInfo, FileStatusKind, GitFailure, GitOp, GitOpOutcome, RefInfo, RefKind, RepoSnapshot,
    RepositoryState, StageEntry, StageStatusSnapshot, StageTreeRowKind, SubmoduleState,
    WorkspaceTab, WorktreeBranch,
};
use jbgit_engine::{
    EngineEvent, StageDiffKey, StageDiffSide, SubmoduleDiffInfo, TaskRequest, ThreeDocStageDiff,
    askpass::AskpassRequest,
};

use super::{
    TestSession, item_run_actions, make_commit, make_snapshot, menu_item_by_label, oid, screen_text,
};
use crate::app::{Action, ContextMenuEntryKind, ModalLayer, UiAsyncEvent};
use crate::config::{KeymapConfig, KeymapPreset};
use crate::dialogs::Dialog;
use crate::hit_test::{BisectButton, HitTarget};
use crate::i18n::I18n;
use crate::keymap::{KeyAction, catalog};
use crate::menus;

fn snapshot_with_bisect() -> RepoSnapshot {
    let mut snap = (*make_snapshot(
        PathBuf::from("/tmp/bisect-repo"),
        vec![
            make_commit(3, vec![oid(2)], "bad head"),
            make_commit(2, vec![oid(1)], "mid commit"),
            make_commit(1, vec![], "good base"),
        ],
    ))
    .clone();
    snap.state = RepositoryState::Bisect;
    snap.bisect = Some(BisectInfo {
        remaining_revisions: Some(3),
        remaining_steps: Some(2),
    });
    snap
}

#[test]
fn bisect_banner_buttons_command_catalog_and_log_menu() {
    let mut ts = TestSession::with_snapshot(snapshot_with_bisect());
    let buf = ts.render(120, 30);
    let text = screen_text(&buf);
    assert!(
        text.contains("BISECTING"),
        "banner missing BISECTING:\n{text}"
    );
    assert!(
        text.contains("~2 step(s) left (3 rev(s))"),
        "banner missing bisect step estimate:\n{text}"
    );
    assert!(
        text.contains("Good")
            && text.contains("Bad")
            && text.contains("Skip")
            && text.contains("Reset"),
        "banner missing buttons:\n{text}"
    );

    // Click each Bisect banner button and verify declarative GitOp dispatch.
    for (btn, expected) in [
        (
            BisectButton::Good,
            GitOp::BisectMark {
                good: true,
                rev: None,
            },
        ),
        (
            BisectButton::Bad,
            GitOp::BisectMark {
                good: false,
                rev: None,
            },
        ),
        (BisectButton::Skip, GitOp::BisectSkip),
        (BisectButton::Reset, GitOp::BisectReset),
    ] {
        let (cx, cy) = ts
            .hit_center_of(|t| *t == HitTarget::BisectBanner(btn))
            .unwrap_or_else(|| panic!("missing HitTarget for {btn:?}"));
        let _ = ts.take_git_ops();
        ts.send_mouse(MouseEventKind::Down(MouseButton::Left), cx, cy);
        assert_eq!(ts.take_git_ops(), vec![expected]);
    }

    // CommandCatalog applicability & execution while bisecting.
    for (ka, expected) in [
        (
            KeyAction::BisectGood,
            GitOp::BisectMark {
                good: true,
                rev: None,
            },
        ),
        (
            KeyAction::BisectBad,
            GitOp::BisectMark {
                good: false,
                rev: None,
            },
        ),
        (KeyAction::BisectSkip, GitOp::BisectSkip),
        (KeyAction::BisectReset, GitOp::BisectReset),
    ] {
        assert!(catalog::is_action_applicable(&ka, ts.state()));
        let actions = catalog::execute_action(&ka, ts.state()).expect("bisect action");
        let _ = ts.take_git_ops();
        ts.dispatch_many(actions);
        assert_eq!(ts.take_git_ops(), vec![expected]);
    }

    // Log commit menu offers Mark Good (Bisect) while bisecting and Start Bisect Here (Bad) when clean.
    ts.state_mut().log.graph.selected_idx = 0;
    let bisect_items = menus::log_commit_menu_items(ts.state());
    let mark_good = menu_item_by_label(&bisect_items, "Mark Good");
    assert!(mark_good.is_enabled());
    let _ = ts.take_git_ops();
    ts.dispatch_many(item_run_actions(mark_good));
    let top_oid = ts.state().repo_snapshot.as_ref().unwrap().commits[0].oid;
    assert_eq!(
        ts.take_git_ops(),
        vec![GitOp::BisectMark {
            good: true,
            rev: Some(top_oid.to_string()),
        }]
    );

    let mut clean_snap = snapshot_with_bisect();
    clean_snap.state = RepositoryState::Clean;
    clean_snap.bisect = None;
    let mut ts_clean = TestSession::with_snapshot(clean_snap);
    assert!(!catalog::is_action_applicable(
        &KeyAction::BisectGood,
        ts_clean.state()
    ));
    ts_clean.state_mut().log.graph.selected_idx = 0;
    let clean_items = menus::log_commit_menu_items(ts_clean.state());
    let start_bad = menu_item_by_label(&clean_items, "Start Bisect Here (Bad)");
    assert!(start_bad.is_enabled());
    let _ = ts_clean.take_git_ops();
    ts_clean.dispatch_many(item_run_actions(start_bad));
    // Starting bisect pops a confirmation dialog first (never detaches HEAD silently).
    assert!(
        ts_clean.take_git_ops().is_empty(),
        "Start Bisect must require confirmation before detaching HEAD"
    );
    assert!(matches!(
        ts_clean.state().modal_stack.last(),
        Some(ModalLayer::Dialog(_))
    ));
    ts_clean.send_key(KeyCode::Left, KeyModifiers::NONE);
    ts_clean.send_key(KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(
        ts_clean.take_git_ops(),
        vec![GitOp::BisectStart {
            bad: Some(top_oid.to_string()),
            good: Vec::new(),
        }]
    );
}

#[test]
fn stage_flexible_ignore_submenu_offers_extension_dir_nested_and_editor() {
    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path();
    std::fs::create_dir_all(root.join("sub")).unwrap();
    std::fs::write(root.join("sub/.gitignore"), "# nested\n").unwrap();
    std::fs::write(root.join("sub/app.log"), "log line\n").unwrap();

    let mut snap =
        (*make_snapshot(root.to_path_buf(), vec![make_commit(1, vec![], "init")])).clone();
    snap.git_dir = root.join(".git");
    snap.common_dir = root.join(".git");
    let mut ts = TestSession::with_snapshot(snap);
    ts.dispatch(Action::SwitchTab(WorkspaceTab::Commit));
    ts.deliver_engine_event(EngineEvent::StageStatusLoaded {
        status: Arc::new(StageStatusSnapshot {
            untracked: vec![StageEntry {
                path: "sub/app.log".into(),
                old_path: None,
                kind: FileStatusKind::Untracked,
            }],
            ..Default::default()
        }),
        epoch: 1,
    });

    let row_idx = ts
        .state()
        .stage_workbench
        .rows()
        .iter()
        .position(
            |r| matches!(&r.kind, StageTreeRowKind::File { path, .. } if path == "sub/app.log"),
        )
        .expect("sub/app.log row");
    ts.state_mut().stage_workbench.select_row(row_idx);

    let items = menus::stage_tree_menu_items(ts.state(), row_idx);
    let ignore_sub = menu_item_by_label(&items, "Add to Ignore");
    let ContextMenuEntryKind::Submenu(sub_items) = &ignore_sub.kind else {
        panic!("Add to Ignore… must be a submenu");
    };

    // Must expose extension (*.log), parent dir (/sub/), nested sub/.gitignore,
    // .git/info/exclude, and $EDITOR shortcuts.
    let ext_root = menu_item_by_label(sub_items, "'*.log' → .gitignore");
    let dir_root = menu_item_by_label(sub_items, "'/sub/' → .gitignore");
    let nested_exact = menu_item_by_label(sub_items, "'/app.log' → sub/.gitignore");
    let _exclude_ext = menu_item_by_label(sub_items, "'*.log' → .git/info/exclude");
    let _open_gi = menu_item_by_label(sub_items, "Open .gitignore in $EDITOR");
    let _open_ex = menu_item_by_label(sub_items, "Open .git/info/exclude in $EDITOR");

    assert!(ext_root.is_enabled() && dir_root.is_enabled() && nested_exact.is_enabled());

    let _ = ts.take_git_ops();
    ts.dispatch_many(item_run_actions(ext_root));
    assert_eq!(
        ts.take_git_ops(),
        vec![GitOp::AddToIgnore {
            patterns: vec!["*.log".into()],
            exclude_file: false,
            target_file: None,
        }]
    );

    let _ = ts.take_git_ops();
    ts.dispatch_many(item_run_actions(nested_exact));
    assert_eq!(
        ts.take_git_ops(),
        vec![GitOp::AddToIgnore {
            patterns: vec!["/app.log".into()],
            exclude_file: false,
            target_file: Some(PathBuf::from("sub").join(".gitignore")),
        }]
    );
}

#[test]
fn submodule_gitlink_whole_entry_stage_enabled_while_line_staging_disabled() {
    let snap = (*make_snapshot(
        PathBuf::from("/tmp/sub-repo"),
        vec![make_commit(1, vec![], "init")],
    ))
    .clone();
    let mut ts = TestSession::with_snapshot(snap);
    ts.dispatch(Action::SwitchTab(WorkspaceTab::Commit));
    let mut submodules = BTreeMap::new();
    submodules.insert(
        "deps/sub".into(),
        SubmoduleState {
            new_commits: true,
            dirty_workdir: false,
            uninitialized: false,
        },
    );
    ts.deliver_engine_event(EngineEvent::StageStatusLoaded {
        status: Arc::new(StageStatusSnapshot {
            unstaged: vec![StageEntry {
                path: "deps/sub".into(),
                old_path: None,
                kind: FileStatusKind::Modified,
            }],
            submodules,
            ..Default::default()
        }),
        epoch: 1,
    });

    let row_idx = ts
        .state()
        .stage_workbench
        .rows()
        .iter()
        .position(|r| matches!(&r.kind, StageTreeRowKind::File { path, .. } if path == "deps/sub"))
        .expect("deps/sub row");
    ts.state_mut().stage_workbench.select_row(row_idx);

    let items = menus::stage_tree_menu_items(ts.state(), row_idx);
    let stage_item = menu_item_by_label(&items, "Stage");
    let discard_item = menu_item_by_label(&items, "Discard");
    assert!(
        stage_item.is_enabled(),
        "Submodule gitlink whole-entry Stage must be enabled"
    );
    assert!(
        discard_item.is_enabled(),
        "Submodule gitlink Discard must be enabled"
    );

    let _ = ts.take_requests();
    ts.dispatch(Action::StageActionSelected);
    let reqs = ts.take_requests();
    assert!(
        reqs.iter().any(|(r, _)| matches!(
            r,
            TaskRequest::StageTargets { targets } if targets.len() == 1 && targets[0].path == "deps/sub"
        )),
        "StageActionSelected must dispatch StageTargets for submodule gitlink, got: {reqs:?}"
    );

    // Line-level diff hunk staging must still be disabled for submodules.
    let sub_diff = ThreeDocStageDiff {
        key: StageDiffKey {
            path: "deps/sub".into(),
            old_path: None,
            side: StageDiffSide::Unstaged,
            context: jbgit_core::DiffContext::from_lines(3),
            whitespace: jbgit_core::WhitespaceMode::Default,
        },
        vcs_exists: true,
        staged_exists: true,
        local_exists: true,
        vcs_bytes: Vec::new(),
        staged_bytes: Vec::new(),
        local_bytes: Vec::new(),
        file_mode: 0o160000,
        mode_change: None,
        is_symlink: false,
        is_binary: false,
        binary: None,
        is_conflicted: false,
        submodule: Some(SubmoduleDiffInfo {
            head_oid: Some(oid(1)),
            index_oid: Some(oid(1)),
            workdir_oid: Some(oid(2)),
            state: SubmoduleState {
                new_commits: true,
                dirty_workdir: false,
                uninitialized: false,
            },
        }),
        lfs: None,
        tracker: jbgit_core::StageLineTracker::new(&[], &[], &[]),
        hunks: Vec::new(),
        is_truncated: false,
        is_too_large: false,
    };
    assert!(sub_diff.is_line_staging_disabled());
}

#[test]
fn commit_gpg_three_state_signed_tag_and_gpg_failure_dialog() {
    let snap = (*make_snapshot(
        PathBuf::from("/tmp/gpg-repo"),
        vec![make_commit(5, vec![], "head")],
    ))
    .clone();
    let mut ts = TestSession::with_snapshot(snap);
    ts.dispatch(Action::SwitchTab(WorkspaceTab::Commit));

    // 1. GPG three-state cycle: None (Auto) -> Some(true) (-S) -> Some(false) (--no-gpg-sign) -> None.
    assert_eq!(ts.state().stage_workbench.gpg_sign(), None);
    let t0 = screen_text(&ts.render(120, 32));
    assert!(t0.contains("GPG (auto)"), "{t0}");

    ts.dispatch(Action::CommitCycleGpgSign);
    assert_eq!(ts.state().stage_workbench.gpg_sign(), Some(true));
    let t1 = screen_text(&ts.render(120, 32));
    assert!(t1.contains("GPG -S"), "{t1}");

    // Submit commit with staged file -> CommitRequest carries gpg_sign = Some(true).
    ts.deliver_engine_event(EngineEvent::StageStatusLoaded {
        status: Arc::new(StageStatusSnapshot {
            staged: vec![StageEntry {
                path: "a.txt".into(),
                old_path: None,
                kind: FileStatusKind::Modified,
            }],
            ..Default::default()
        }),
        epoch: 1,
    });
    ts.dispatch(Action::CommitEditorPaste("feat: signed commit".into()));
    let _ = ts.take_requests();
    ts.dispatch(Action::SubmitCommit { and_push: false });
    let reqs = ts.take_requests();
    assert!(
        reqs.iter().any(|(r, _)| matches!(
            r,
            TaskRequest::ExecuteCommit { request, .. } if request.gpg_sign == Some(true)
        )),
        "expected CommitRequest with gpg_sign=Some(true), got: {reqs:?}"
    );

    ts.dispatch(Action::CommitCycleGpgSign);
    assert_eq!(ts.state().stage_workbench.gpg_sign(), Some(false));
    let t2 = screen_text(&ts.render(120, 32));
    assert!(t2.contains("No GPG"), "{t2}");

    ts.dispatch(Action::CommitCycleGpgSign);
    assert_eq!(ts.state().stage_workbench.gpg_sign(), None);

    // 2. Signed tag dialog (Ctrl+S toggles Sign tag (-s), which opens the tag message dialog).
    ts.state_mut().log.graph.selected_idx = 0;
    let log_items = menus::log_commit_menu_items(ts.state());
    let new_tag = menu_item_by_label(&log_items, "New Tag Here");
    ts.dispatch_many(item_run_actions(new_tag));
    ts.type_str("v1.2.3");
    ts.send_key(KeyCode::Char('s'), KeyModifiers::CONTROL);
    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);
    // Signed tags carry a tag message -> confirm the opened MessageDialog.
    ts.type_str("release v1.2.3");
    let _ = ts.take_git_ops();
    ts.send_key(KeyCode::Enter, KeyModifiers::CONTROL);
    let head_oid = ts.state().repo_snapshot.as_ref().unwrap().commits[0].oid;
    assert_eq!(
        ts.take_git_ops(),
        vec![GitOp::CreateTag {
            name: "v1.2.3".into(),
            target: head_oid,
            message: Some("release v1.2.3".into()),
            force: false,
            sign: true,
        }]
    );

    // 3. GpgSignFailed opens troubleshooting dialog mentioning gpg-agent & pinentry.
    ts.deliver_engine_event(EngineEvent::GitOpFinished(GitOpOutcome::failed(
        GitOp::CreateTag {
            name: "v1.2.3".into(),
            target: head_oid,
            message: None,
            force: false,
            sign: true,
        },
        GitFailure::GpgSignFailed {
            detail: "gpg: signing failed: No pinentry".into(),
        },
    )));
    let err_screen = screen_text(&ts.render(120, 32));
    assert!(err_screen.contains("GPG Signing Failed"), "{err_screen}");
    assert!(
        err_screen.contains("gpg-agent") && err_screen.contains("pinentry"),
        "{err_screen}"
    );
}

#[test]
fn ssh_host_key_askpass_opens_unmasked_confirmation_dialog() {
    let snap = (*make_snapshot(
        PathBuf::from("/tmp/ssh-repo"),
        vec![make_commit(1, vec![], "init")],
    ))
    .clone();
    let mut ts = TestSession::with_snapshot(snap);

    let prompt = "The authenticity of host 'example.com (192.0.2.1)' can't be established.\n\
ED25519 key fingerprint is SHA256:TestFingerprint1234567890.\n\
Are you sure you want to continue connecting (yes/no/[fingerprint])? ";
    let (req, mut rx) = AskpassRequest::new(prompt.to_string());
    assert!(req.host_key_confirm && !req.masked);

    ts.deliver_ui_async(UiAsyncEvent::Askpass(req));
    match ts.state().modal_stack.last() {
        Some(ModalLayer::Dialog(d)) => match d.as_ref() {
            Dialog::Credential(c) => {
                assert!(c.host_key_confirm);
                assert!(!c.masked);
            }
            other => panic!("expected Credential dialog, got {other:?}"),
        },
        other => panic!("expected Dialog modal, got {other:?}"),
    }

    ts.type_str("yes");
    let text = screen_text(&ts.render(120, 32));
    assert!(text.contains("SSH Host Key Verification"), "{text}");
    assert!(text.contains("SHA256:TestFingerprint1234567890"), "{text}");
    assert!(
        text.contains("yes"),
        "host-key input must be visible unmasked:\n{text}"
    );
    assert!(
        !text.contains("•••"),
        "host-key input must not be masked:\n{text}"
    );

    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);
    let reply = rx.try_recv().expect("askpass reply").expect("Some secret");
    assert_eq!(reply.as_str(), "yes");
}

#[test]
fn worktree_remote_branch_menu_force_checkbox_and_setup_command_confirmation() {
    let tmp = tempfile::TempDir::new().unwrap();
    let repo_dir = tmp.path().join("demo");
    std::fs::create_dir_all(&repo_dir).unwrap();
    let mut snap = (*make_snapshot(repo_dir.clone(), vec![make_commit(1, vec![], "init")])).clone();
    snap.refs.push(RefInfo {
        name: "origin/feat".into(),
        full_name: "refs/remotes/origin/feat".into(),
        target_oid: oid(1),
        kind: RefKind::RemoteBranch,
        upstream: None,
        ahead_behind: None,
        is_head: false,
    });
    let mut ts = TestSession::with_snapshot(snap);
    ts.state_mut().config.jbgit.worktree.setup_command = Some("npm ci".into());

    // Remote branch menu includes `New Worktree from 'origin/feat'…`.
    let remote_items = menus::branch_menu_items(ts.state(), "origin/feat", RefKind::RemoteBranch);
    let wt_item = menu_item_by_label(&remote_items, "New Worktree from 'origin/feat'");
    assert!(wt_item.is_enabled());
    ts.dispatch_many(item_run_actions(wt_item));

    // Worktree path dialog opens directly (prefilled for `feat`) -> press Tab to toggle `--force`, then Enter.
    ts.send_key(KeyCode::Tab, KeyModifiers::NONE);
    let _ = ts.take_git_ops();
    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);

    let expected_path = tmp.path().join("demo-feat");
    let expected_op = GitOp::AddWorktree {
        path: expected_path.clone(),
        branch: WorktreeBranch::New {
            name: "feat".into(),
            start: "origin/feat".into(),
        },
        force: true,
    };
    assert_eq!(ts.take_git_ops(), vec![expected_op.clone()]);

    // When AddWorktree succeeds and [worktree] setup_command is configured,
    // jbgit pops a confirmation dialog and never runs silently.
    ts.deliver_engine_event(EngineEvent::GitOpFinished(GitOpOutcome::ok(expected_op)));
    assert!(ts.state().pending_worktree_setup.is_none());
    let text = screen_text(&ts.render(120, 32));
    assert!(text.contains("Run Worktree Setup Command?"), "{text}");
    assert!(text.contains("npm ci"), "{text}");

    // Confirming sets pending_worktree_setup (default button is Cancel -> Left selects Run Setup).
    ts.send_key(KeyCode::Left, KeyModifiers::NONE);
    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(
        ts.state_mut().pending_worktree_setup.take(),
        Some((expected_path, "npm ci".into()))
    );
}

#[test]
fn issue_14_keymaps_and_i18n_have_no_conflicts_across_all_presets() {
    use std::collections::HashMap;

    for preset in [
        KeymapPreset::Default,
        KeymapPreset::Jetbrains,
        KeymapPreset::Vim,
    ] {
        let km = KeymapConfig::for_preset(preset.clone());
        assert!(
            !km.global.key_bindings.is_empty(),
            "preset {preset:?} must load cleanly"
        );
        // Verify no duplicate `on` chord maps to two distinct actions within any single scope.
        let scopes: [(&str, &[_]); 7] = [
            ("global", &km.global.key_bindings),
            ("log", &km.log.key_bindings),
            ("branch", &km.branch.key_bindings),
            ("commit", &km.commit.key_bindings),
            ("diff", &km.diff.key_bindings),
            ("stash", &km.stash.key_bindings),
            ("console", &km.console.key_bindings),
        ];
        for (scope_name, bindings) in scopes {
            let mut seen: HashMap<&[String], &KeyAction> = HashMap::new();
            for b in bindings {
                if !b.run.is_active() {
                    continue;
                }
                if let Some(prev) = seen.insert(b.on.as_slice(), &b.run) {
                    // Intentional prepend_keymap overrides in jetbrains/vim may override
                    // the same key with a new action, which takes precedence in matching,
                    // EXCEPT in `Default` where every binding in a scope must be unique!
                    if preset == KeymapPreset::Default {
                        assert_eq!(
                            prev, &b.run,
                            "Default preset scope [{scope_name}] has duplicate chord {:?}: {prev:?} vs {:?}",
                            b.on, b.run
                        );
                    }
                }
            }
        }
        // Verify [diff] does not shadow [global] `B` (toggle_branches_popup).
        assert!(
            !km.diff
                .key_bindings
                .iter()
                .any(|b| b.on.as_slice() == ["B"] && b.run != KeyAction::ToggleBranchesPopup),
            "preset {preset:?}: [diff] must not bind `B` and shadow [global] toggle_branches_popup"
        );
        // Verify Bisect global shortcuts are bound in every preset.
        for ka in [
            KeyAction::BisectGood,
            KeyAction::BisectBad,
            KeyAction::BisectSkip,
            KeyAction::BisectReset,
        ] {
            assert!(
                km.primary_key_for(&[crate::config::KeymapScope::Global], &ka)
                    .is_some(),
                "preset {preset:?}: missing global keybinding for {ka:?}"
            );
        }
    }

    let en = I18n::load("en-US", None);
    let zh = I18n::load("zh-CN", None);
    for ka in [
        KeyAction::BisectGood,
        KeyAction::BisectBad,
        KeyAction::BisectSkip,
        KeyAction::BisectReset,
        KeyAction::CycleGpgSign,
    ] {
        assert_eq!(KeyAction::parse(ka.as_str()), ka);
        let pill = ka.footer_pill_key().expect("footer_pill_key");
        assert!(
            !en.t(pill).starts_with("view.footer.pill."),
            "missing en-US pill for {ka:?}"
        );
        assert!(
            !zh.t(pill).starts_with("view.footer.pill."),
            "missing zh-CN pill for {ka:?}"
        );
        let help_key = format!("help.action.{}", ka.as_str());
        assert!(
            !en.t(&help_key).starts_with("help.action."),
            "missing en-US help for {ka:?}"
        );
        assert!(
            !zh.t(&help_key).starts_with("help.action."),
            "missing zh-CN help for {ka:?}"
        );
    }
}

#[test]
fn create_patch_inside_worktree_requires_confirmation_before_writing() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mut snap = snapshot_with_bisect();
    snap.state = RepositoryState::Clean;
    snap.bisect = None;
    snap.workdir = tmp.path().to_path_buf();
    let mut ts = TestSession::with_snapshot(snap);

    let inside_path = tmp.path().join("dirty.patch");
    let source = jbgit_core::PatchSource::Commits {
        oids: vec![ts.state().repo_snapshot.as_ref().unwrap().commits[0].oid],
        base: None,
    };
    ts.dispatch(Action::ConfirmCreatePatchFile {
        source: source.clone(),
        output: inside_path.clone(),
        reverse: false,
        remember_dir: tmp.path().to_path_buf(),
    });
    // Must pop confirmation dialog instead of writing untracked file silently.
    assert!(ts.take_git_ops().is_empty());
    assert!(matches!(
        ts.state().modal_stack.last(),
        Some(ModalLayer::Dialog(_))
    ));
    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(
        ts.take_git_ops(),
        vec![GitOp::CreatePatchFile {
            source,
            output: inside_path,
            reverse: false,
        }]
    );
}

#[test]
fn reflog_pending_diff_debounce_is_reported_in_app_next_deadline() {
    let mut ts = TestSession::with_snapshot(snapshot_with_bisect());
    let head_oid = ts.state().repo_snapshot.as_ref().unwrap().commits[0].oid;
    ts.dispatch(Action::OpenReflog("HEAD".into()));
    let now = ts.state().now();
    if let Some(rf) = ts.state_mut().modal_stack.reflog_mut() {
        rf.diff.pending = Some((
            jbgit_core::FileDiffKey {
                details: jbgit_core::DetailsKey::single(head_oid),
                path: "a.txt".into(),
                old_path: None,
                context: jbgit_core::DiffContext::Three,
                whitespace: jbgit_core::WhitespaceMode::Default,
            },
            now + std::time::Duration::from_millis(50),
        ));
    }
    assert!(
        ts.state().next_deadline().is_some(),
        "AppState::next_deadline() must include pending Reflog diff debounce"
    );
}
