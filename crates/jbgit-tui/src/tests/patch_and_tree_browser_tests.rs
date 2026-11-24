//! Apply Patch dialog, Create Patch config dialog, and Browse Repository at Revision
//! (Spec 12 #13 + Spec 13 #10).

use std::sync::Arc;
use std::time::Duration;

use crossterm::event::{KeyCode, KeyModifiers};
use jbgit_core::{
    ApplyMode, ApplyOptions, DetailsKey, FileStatusKind, GitFailure, GitOp, GitOpOutcome, Oid,
    PaneId, PatchCheck, PatchCheckKey, PatchFileSection, PatchFileStat, PatchInput, PatchSource,
    RepoSnapshot, StageEntry, StageStatusSnapshot, TreeBlob, TreeBlobContent, TreeBlobKey,
    TreeEntry, TreeEntryKind, TreeKey, TreeListing, WorkspaceTab,
};
use jbgit_engine::{EngineEvent, TaskRequest};

use crate::app::{Action, ModalLayer, PopupKind};
use crate::dialogs::Dialog;

use super::{TestSession, find_item, item_run_actions, oid, screen_text, snapshot};

fn requests(ts: &TestSession) -> Vec<(TaskRequest, Option<u64>)> {
    ts.take_requests()
}

fn patch_check_request(ts: &TestSession) -> (PatchCheckKey, u64) {
    requests(ts)
        .into_iter()
        .find_map(|(req, epoch)| match req {
            TaskRequest::CheckPatch { key } => Some((key, epoch.unwrap())),
            _ => None,
        })
        .expect("CheckPatch request")
}

fn top_apply_patch(ts: &TestSession) -> &crate::patch_view::ApplyPatchState {
    match ts.state().modal_stack.last() {
        Some(ModalLayer::ApplyPatch(s)) => s,
        other => panic!("ApplyPatch not on top: {other:?}"),
    }
}

fn stat(path: &str, added: u32, deleted: u32, conflicts: bool) -> PatchFileStat {
    PatchFileStat {
        path: path.into(),
        added: Some(added),
        deleted: Some(deleted),
        conflicts,
    }
}

fn section(path: &str, diff_text: &str) -> PatchFileSection {
    PatchFileSection {
        path: path.into(),
        diff_text: diff_text.into(),
    }
}

#[test]
fn clipboard_patch_preflight_error_three_way_retry_apply_and_conflicts() {
    let mut ts = TestSession::with_snapshot(snapshot());
    ts.dispatch(Action::SwitchTab(WorkspaceTab::Commit));
    let _ = ts.take_requests();

    // `<A-S-p>` = Apply Patch from Clipboard.
    ts.send_key(KeyCode::Char('p'), KeyModifiers::ALT | KeyModifiers::SHIFT);
    assert_eq!(top_apply_patch(&ts).source, PatchInput::Clipboard);
    let (key, epoch) = patch_check_request(&ts);
    assert_eq!(key.options.mode, ApplyMode::WorkingTree);
    let buf = ts.render(120, 30);
    assert!(screen_text(&buf).contains("Checking patch"));

    // Working-tree check fails: first error shown, 3-way suggested, Apply disabled.
    ts.deliver_engine_event(EngineEvent::PatchChecked {
        check: Arc::new(PatchCheck {
            key: key.clone(),
            files: Vec::new(),
            sections: Vec::new(),
            has_index_headers: true,
            error: Some("error: f.txt: patch does not apply".into()),
        }),
        epoch,
    });
    let text = screen_text(&ts.render(120, 30));
    assert!(text.contains("patch does not apply"), "{text}");
    assert!(text.contains("3-way"), "{text}");
    ts.send_key(KeyCode::Char('A'), KeyModifiers::SHIFT);
    assert!(matches!(
        ts.state().modal_stack.last(),
        Some(ModalLayer::ApplyPatch(_))
    ));
    assert!(ts.take_git_ops().is_empty());

    // `3` switches to 3-way and re-checks.
    ts.send_key(KeyCode::Char('3'), KeyModifiers::NONE);
    let (key3, epoch3) = patch_check_request(&ts);
    assert_eq!(key3.options.mode, ApplyMode::ThreeWay);
    ts.deliver_engine_event(EngineEvent::PatchChecked {
        check: Arc::new(PatchCheck {
            key: key3,
            files: vec![stat("f.txt", 1, 1, true), stat("sub/g.txt", 2, 0, false)],
            sections: vec![
                section("f.txt", "diff --git a/f.txt b/f.txt\n@@ -1 +1 @@\n-old\n+new"),
                section("sub/g.txt", "diff --git a/sub/g.txt b/sub/g.txt\n@@ -1 +1,3 @@\n+x\n+y"),
            ],
            has_index_headers: true,
            error: None,
        }),
        epoch: epoch3,
    });
    let text = screen_text(&ts.render(120, 30));
    assert!(
        text.contains("sub/g.txt") && text.contains("+2") && text.contains("-0"),
        "{text}"
    );
    assert!(text.contains("1 with conflicts"), "{text}");

    // `A` (Shift+A) confirms Apply.
    ts.send_key(KeyCode::Char('A'), KeyModifiers::SHIFT);
    assert!(ts.state().modal_stack.is_empty());
    let op = GitOp::ApplyPatch {
        source: PatchInput::Clipboard,
        options: ApplyOptions::with_mode(ApplyMode::ThreeWay),
    };
    assert_eq!(ts.take_git_ops(), vec![op.clone()]);

    // Conflicts lead to the existing three-pane merge flow.
    ts.deliver_engine_event(EngineEvent::GitOpFinished(GitOpOutcome::failed(
        op,
        GitFailure::PatchConflicts {
            paths: vec!["f.txt".into()],
        },
    )));
    let n = ts.state().notification.as_ref().expect("notification");
    assert!(n.text.contains("1 file"), "{}", n.text);
    let (_, actions) = n.action.as_ref().expect("resolve action");
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, Action::OpenConflictResolver(p) if p == "f.txt"))
    );
}

#[test]
fn apply_patch_partial_checkbox_diff_preview_depth_options_and_plain_three_way_hint() {
    let mut ts = TestSession::with_snapshot(snapshot());
    ts.dispatch(Action::SwitchTab(WorkspaceTab::Commit));
    let _ = ts.take_requests();

    ts.dispatch(Action::OpenApplyPatch(PatchInput::Clipboard));
    let (key, epoch) = patch_check_request(&ts);
    ts.deliver_engine_event(EngineEvent::PatchChecked {
        check: Arc::new(PatchCheck {
            key,
            files: vec![stat("a.rs", 2, 1, false), stat("b.rs", 3, 0, false)],
            sections: vec![
                section("a.rs", "diff --git a/a.rs b/a.rs\n@@ -1 +1 @@\n-fn old() {}\n+fn new_a() {}"),
                section("b.rs", "diff --git a/b.rs b/b.rs\n@@ -1 +1 @@\n+fn added_b() {}"),
            ],
            has_index_headers: false,
            error: None,
        }),
        epoch,
    });

    // Uncheck first file (`a.rs`) with Space -> schedules debounced re-check with `include_paths: ["b.rs"]`.
    ts.send_key(KeyCode::Char(' '), KeyModifiers::NONE);
    assert!(!top_apply_patch(&ts).is_checked("a.rs"));
    assert!(top_apply_patch(&ts).is_checked("b.rs"));
    assert!(requests(&ts).is_empty(), "should debounce before 120ms");
    ts.advance_by(Duration::from_millis(120));
    let (k_partial, ep_partial) = patch_check_request(&ts);
    assert_eq!(k_partial.options.include_paths, vec!["b.rs".to_string()]);
    ts.deliver_engine_event(EngineEvent::PatchChecked {
        check: Arc::new(PatchCheck {
            key: k_partial,
            files: vec![stat("b.rs", 3, 0, false)],
            sections: vec![
                section("a.rs", "diff --git a/a.rs b/a.rs\n@@ -1 +1 @@\n-fn old() {}\n+fn new_a() {}"),
                section("b.rs", "diff --git a/b.rs b/b.rs\n@@ -1 +1 @@\n+fn added_b() {}"),
            ],
            has_index_headers: false,
            error: None,
        }),
        epoch: ep_partial,
    });
    // Both files remain visible in list (a.rs unchecked, b.rs checked).
    assert_eq!(top_apply_patch(&ts).known_files.len(), 2);

    // Move to `b.rs` and press Enter to open right-hand syntax-highlighted diff preview.
    ts.send_key(KeyCode::Char('j'), KeyModifiers::NONE);
    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);
    assert!(top_apply_patch(&ts).show_preview);
    let preview_txt = screen_text(&ts.render(130, 30));
    assert!(preview_txt.contains("fn added_b()"), "{preview_txt}");

    // Toggle Reverse (`r`), increment strip (`+` -> -p2), set directory (`D` -> "sub" + Enter).
    ts.send_key(KeyCode::Char('r'), KeyModifiers::NONE);
    ts.send_key(KeyCode::Char('+'), KeyModifiers::NONE);
    ts.send_key(KeyCode::Char('D'), KeyModifiers::SHIFT);
    ts.type_str("sub");
    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);
    ts.advance_by(Duration::from_millis(120));
    let (k_opts, _) = patch_check_request(&ts);
    assert!(k_opts.options.reverse);
    assert_eq!(k_opts.options.strip, Some(2));
    assert_eq!(k_opts.options.directory.as_deref(), Some("sub"));

    // Plain unified patch failing under `--3way` renders explicit index-header fallback guidance.
    ts.send_key(KeyCode::Char('3'), KeyModifiers::NONE);
    let (k3, ep3) = patch_check_request(&ts);
    ts.deliver_engine_event(EngineEvent::PatchChecked {
        check: Arc::new(PatchCheck {
            key: k3,
            files: Vec::new(),
            sections: Vec::new(),
            has_index_headers: false,
            error: Some("error: patch does not apply".into()),
        }),
        epoch: ep3,
    });
    let txt = screen_text(&ts.render(130, 30));
    assert!(txt.contains("lacks Git index headers"), "{txt}");
}

#[test]
fn create_patch_dialog_defaults_outside_repo_supports_reverse_and_mixed_stage_paths() {
    let parent = tempfile::TempDir::new().unwrap();
    let repo_dir = parent.path().join("my-repo");
    std::fs::create_dir(&repo_dir).unwrap();
    let snap = RepoSnapshot {
        workdir: repo_dir.clone(),
        ..snapshot()
    };
    let mut ts = TestSession::with_snapshot(snap);

    // 1. From Log menu: Save Patch Files opens CreatePatchDialog with default path outside repo_dir.
    ts.dispatch(Action::SwitchTab(WorkspaceTab::Log));
    ts.dispatch(Action::FocusPane(PaneId::LogCommitTable));
    let items = crate::menus::log_commit_menu_items(ts.state());
    let create_sub = find_item(&items, "Create Patch…");
    let crate::app::ContextMenuEntryKind::Submenu(sub_items) = &create_sub.kind else {
        panic!("expected submenu");
    };
    let save_item = find_item(sub_items, "Create Patch File…");
    ts.dispatch_many(item_run_actions(save_item));
    let Some(ModalLayer::Dialog(d)) = ts.state().modal_stack.last() else {
        panic!("CreatePatchDialog expected");
    };
    let Dialog::CreatePatch(cp) = d.as_ref() else {
        panic!("expected Dialog::CreatePatch");
    };
    assert!(!cp.to_clipboard);
    assert!(
        !std::path::Path::new(&cp.path).starts_with(&repo_dir),
        "default patch path must be outside worktree to avoid polluting git status: {}",
        cp.path
    );
    // Move to Reverse row and toggle `-R`, then confirm.
    ts.send_key(KeyCode::Down, KeyModifiers::NONE);
    ts.send_key(KeyCode::Char(' '), KeyModifiers::NONE);
    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);
    let ops = ts.take_git_ops();
    assert!(matches!(
        ops.as_slice(),
        [GitOp::CreatePatchFile { reverse: true, output, .. }] if !output.starts_with(&repo_dir)
    ), "unexpected ops: {ops:?}");

    // 2. From Stage menu with Staged + Unstaged + Untracked files selected:
    ts.dispatch(Action::SwitchTab(WorkspaceTab::Commit));
    ts.deliver_engine_event(EngineEvent::StageStatusLoaded {
        status: Arc::new(StageStatusSnapshot {
            staged: vec![StageEntry {
                path: "s.rs".into(),
                old_path: None,
                kind: FileStatusKind::Modified,
            }],
            unstaged: vec![
                StageEntry {
                    path: "u.rs".into(),
                    old_path: None,
                    kind: FileStatusKind::Modified,
                },
                StageEntry {
                    path: "new.rs".into(),
                    old_path: None,
                    kind: FileStatusKind::Untracked,
                },
            ],
            ..StageStatusSnapshot::default()
        }),
        epoch: 1,
    });
    // Select all rows in stage tree (selects s.rs, u.rs, new.rs across Staged + Unstaged/Untracked).
    ts.state_mut().stage_workbench.select_all_files();
    let stage_items = crate::menus::stage_tree_menu_items(ts.state(), 1);
    let create_stage = find_item(&stage_items, "Create Patch…");
    ts.dispatch_many(item_run_actions(create_stage));
    let Some(ModalLayer::Dialog(d)) = ts.state().modal_stack.last() else {
        panic!("Stage CreatePatchDialog expected");
    };
    let Dialog::CreatePatch(cp) = d.as_ref() else {
        panic!("expected Dialog::CreatePatch");
    };
    assert_eq!(
        cp.source,
        PatchSource::StagePaths {
            staged: vec!["s.rs".into()],
            unstaged: vec!["u.rs".into()],
            untracked: vec!["new.rs".into()],
        }
    );
}

#[test]
fn patch_file_prompt_completes_paths_and_opens_the_dialog() {
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::create_dir(dir.path().join("patches")).unwrap();
    std::fs::write(dir.path().join("patches/fix.patch"), "diff").unwrap();
    let snap = RepoSnapshot {
        workdir: dir.path().to_path_buf(),
        ..snapshot()
    };
    let mut ts = TestSession::with_snapshot(snap);
    ts.dispatch(Action::SwitchTab(WorkspaceTab::Commit));

    let items = crate::menus::stage_tree_menu_items(ts.state(), 0);
    let item = find_item(&items, "Apply Patch…");
    let actions = item_run_actions(item);
    ts.dispatch_many(actions);
    let Some(ModalLayer::Dialog(d)) = ts.state().modal_stack.last() else {
        panic!("path prompt expected");
    };
    assert!(matches!(d.as_ref(), Dialog::Input(_)));

    ts.type_str("pat");
    ts.send_key(KeyCode::Tab, KeyModifiers::NONE);
    ts.type_str("f");
    ts.send_key(KeyCode::Tab, KeyModifiers::NONE);
    let Some(ModalLayer::Dialog(d)) = ts.state().modal_stack.last() else {
        panic!("path prompt expected");
    };
    let Dialog::Input(input) = d.as_ref() else {
        panic!()
    };
    assert_eq!(input.value, "patches/fix.patch");

    let _ = ts.take_requests();
    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(
        top_apply_patch(&ts).source,
        PatchInput::File(dir.path().join("patches/fix.patch"))
    );
    let (key, _) = patch_check_request(&ts);
    assert_eq!(
        key.source,
        PatchInput::File(dir.path().join("patches/fix.patch"))
    );

    // A missing file keeps the prompt open with an inline error.
    ts.dispatch(Action::PopModal);
    ts.send_key(KeyCode::Char('p'), KeyModifiers::ALT);
    ts.type_str("nope.patch");
    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);
    assert!(matches!(
        ts.state().modal_stack.last(),
        Some(ModalLayer::Dialog(_))
    ));
}

fn entry(path: &str, kind: TreeEntryKind) -> TreeEntry {
    TreeEntry {
        name: path.rsplit('/').next().unwrap().into(),
        path: path.into(),
        kind,
        mode: if kind == TreeEntryKind::Submodule {
            0o160_000
        } else {
            0o100_644
        },
        size: Some(12),
        oid: Oid::from_bytes([0xab; 20]),
        symlink_target: None,
    }
}

fn tree_request(ts: &TestSession) -> Option<(TreeKey, u64)> {
    requests(ts).into_iter().find_map(|(req, epoch)| match req {
        TaskRequest::LoadTree { key } => Some((key, epoch.unwrap())),
        _ => None,
    })
}

fn deliver_listing(ts: &mut TestSession, key: TreeKey, epoch: u64, entries: Vec<TreeEntry>) {
    ts.deliver_engine_event(EngineEvent::TreeLoaded {
        listing: Arc::new(TreeListing { key, entries }),
        epoch,
    });
}

fn blob_request(reqs: &[(TaskRequest, Option<u64>)]) -> Option<(TreeBlobKey, u64)> {
    reqs.iter().find_map(|(req, epoch)| match req {
        TaskRequest::LoadTreeBlob { key } => Some((key.clone(), epoch.unwrap())),
        _ => None,
    })
}

fn select_row(ts: &mut TestSession, path: &str) {
    let idx = match ts.state().modal_stack.last() {
        Some(ModalLayer::TreeBrowser(s)) => s
            .rows
            .iter()
            .position(|r| r.entry.path == path)
            .expect("row"),
        _ => panic!("tree browser not on top"),
    };
    ts.dispatch(Action::TreeBrowser(
        crate::tree_browser::TreeBrowserAction::Select(idx),
    ));
}

#[test]
fn tree_browser_lazy_loads_previews_files_and_runs_commands() {
    let mut ts = TestSession::with_snapshot(snapshot());
    ts.dispatch(Action::SwitchTab(WorkspaceTab::Log));
    ts.dispatch(Action::FocusPane(PaneId::LogCommitTable));
    let _ = ts.take_requests();
    let head = oid(12);

    let items = crate::menus::log_commit_menu_items(ts.state());
    let item = find_item(&items, "Browse Repository at Revision");
    ts.dispatch_many(item_run_actions(item));
    let (root_key, epoch) = tree_request(&ts).expect("root listing requested");
    assert_eq!(
        root_key,
        TreeKey {
            oid: head,
            dir: String::new()
        }
    );
    deliver_listing(
        &mut ts,
        root_key,
        epoch,
        vec![
            entry("src", TreeEntryKind::Tree),
            entry("vendor", TreeEntryKind::Submodule),
            entry("logo.png", TreeEntryKind::Blob),
        ],
    );
    let text = screen_text(&ts.render(140, 30));
    assert!(
        text.contains("Repository at") && text.contains("c12"),
        "{text}"
    );
    assert!(
        text.contains("vendor") && text.contains("read-only"),
        "{text}"
    );

    // Expanding a directory requests just that level.
    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);
    let (src_key, epoch) = tree_request(&ts).expect("src listing requested");
    assert_eq!(src_key.dir, "src");
    deliver_listing(
        &mut ts,
        src_key,
        epoch,
        vec![entry("src/main.rs", TreeEntryKind::Blob)],
    );

    // Selecting a file loads its preview (syntax-highlighted text).
    ts.send_key(KeyCode::Char('j'), KeyModifiers::NONE);
    let reqs = requests(&ts);
    let (blob_key, epoch) = blob_request(&reqs).expect("preview requested");
    assert_eq!(blob_key.path, "src/main.rs");
    ts.deliver_engine_event(EngineEvent::TreeBlobLoaded {
        blob: Arc::new(TreeBlob {
            key: blob_key,
            size: 13,
            content: TreeBlobContent::Text("fn main() {}\n".into()),
        }),
        epoch,
    });
    let text = screen_text(&ts.render(140, 30));
    assert!(text.contains("fn main() {}"), "{text}");

    // `Y` copies file content to clipboard.
    ts.send_key(KeyCode::Char('Y'), KeyModifiers::SHIFT);
    assert_eq!(ts.clipboard().last().map(String::as_str), Some("fn main() {}\n"));

    // Annotate opens above the browser; Esc returns to it.
    ts.send_key(KeyCode::Char('a'), KeyModifiers::NONE);
    assert!(matches!(
        ts.state().modal_stack.last(),
        Some(ModalLayer::Annotate(_))
    ));
    let blame_rev = requests(&ts).into_iter().find_map(|(r, _)| match r {
        TaskRequest::LoadBlame { key } => Some(key.rev),
        _ => None,
    });
    assert_eq!(blame_rev, Some(Some(head)));
    ts.send_key(KeyCode::Esc, KeyModifiers::NONE);
    assert!(matches!(
        ts.state().modal_stack.last(),
        Some(ModalLayer::TreeBrowser(_))
    ));

    // Get Version asks with radio options (default: --worktree only), Enter confirms.
    ts.send_key(KeyCode::Char('g'), KeyModifiers::NONE);
    assert!(matches!(
        ts.state().modal_stack.last(),
        Some(ModalLayer::Dialog(_))
    ));
    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(
        ts.take_git_ops(),
        vec![GitOp::RestorePaths {
            rev: head.to_string(),
            paths: vec!["src/main.rs".into()],
            staged: false,
            worktree: true,
        }]
    );

    // Binary files and submodules get placeholders; submodules are read-only.
    select_row(&mut ts, "logo.png");
    let reqs = requests(&ts);
    let (bin_key, epoch) = blob_request(&reqs).expect("preview requested");
    ts.deliver_engine_event(EngineEvent::TreeBlobLoaded {
        blob: Arc::new(TreeBlob {
            key: bin_key,
            size: 2048,
            content: TreeBlobContent::Binary,
        }),
        epoch,
    });
    let text = screen_text(&ts.render(140, 30));
    assert!(text.contains("Binary file (2048 bytes)"), "{text}");
    select_row(&mut ts, "vendor");
    assert!(blob_request(&requests(&ts)).is_none());
    ts.send_key(KeyCode::Char('g'), KeyModifiers::NONE);
    assert!(
        ts.state()
            .status_notice
            .as_deref()
            .is_some_and(|n| n.contains("submodule"))
    );

    // Copy path, then Compare with Local on a directory scopes to that directory!
    ts.send_key(KeyCode::Char('y'), KeyModifiers::NONE);
    assert_eq!(ts.clipboard().last().map(String::as_str), Some("vendor"));
    select_row(&mut ts, "src");
    ts.send_key(KeyCode::Char('c'), KeyModifiers::NONE);
    assert!(ts.state().modal_stack.is_empty());
    assert_eq!(ts.state().focused_pane, PaneId::LogChangesTree);
    assert_eq!(
        ts.state().log.details.compare_override,
        Some(DetailsKey::compare_with_local_path(head, Some("src".into())))
    );
}

#[test]
fn tree_browser_fuzzy_search_reveals_nested_file_symlink_export_and_batch_restore() {
    let mut ts = TestSession::with_snapshot(snapshot());
    ts.dispatch(Action::SwitchTab(WorkspaceTab::Log));
    let head = oid(12);
    let _ = ts.take_requests();

    ts.dispatch(Action::OpenTreeBrowser { oid: head });
    let (root_key, root_ep) = tree_request(&ts).expect("root listing");
    let mut sym = entry("link.txt", TreeEntryKind::Blob);
    sym.mode = 0o120_000;
    sym.symlink_target = Some("target.txt".into());
    deliver_listing(
        &mut ts,
        root_key,
        root_ep,
        vec![
            entry("a", TreeEntryKind::Tree),
            sym,
            entry("README.md", TreeEntryKind::Blob),
        ],
    );

    // Press `/` to open fuzzy search -> triggers LoadTreeIndex.
    ts.send_key(KeyCode::Char('/'), KeyModifiers::NONE);
    let idx_ep = requests(&ts)
        .into_iter()
        .find_map(|(r, ep)| match r {
            TaskRequest::LoadTreeIndex { oid } if oid == head => Some(ep.unwrap()),
            _ => None,
        })
        .expect("LoadTreeIndex requested");
    ts.deliver_engine_event(EngineEvent::TreeIndexLoaded {
        oid: head,
        paths: Arc::new(vec![
            "a/b/deep_target.rs".into(),
            "README.md".into(),
            "link.txt".into(),
        ]),
        epoch: idx_ep,
    });

    // Type `deep` and hit Enter -> closes search, queues ancestor listings `a` then `a/b`, and reveals file!
    ts.type_str("deep");
    let search_txt = screen_text(&ts.render(140, 30));
    assert!(search_txt.contains("a/b/deep_target.rs"), "{search_txt}");
    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);

    let (a_key, a_ep) = tree_request(&ts).expect("dir a requested");
    assert_eq!(a_key.dir, "a");
    deliver_listing(
        &mut ts,
        a_key,
        a_ep,
        vec![entry("a/b", TreeEntryKind::Tree)],
    );
    let (ab_key, ab_ep) = tree_request(&ts).expect("dir a/b requested");
    assert_eq!(ab_key.dir, "a/b");
    deliver_listing(
        &mut ts,
        ab_key,
        ab_ep,
        vec![entry("a/b/deep_target.rs", TreeEntryKind::Blob)],
    );

    // Cursor landed on `a/b/deep_target.rs` and requested its preview!
    let Some(ModalLayer::TreeBrowser(tb)) = ts.state().modal_stack.last() else {
        panic!("TreeBrowser expected");
    };
    assert_eq!(
        tb.selected_entry().map(|e| e.path.as_str()),
        Some("a/b/deep_target.rs")
    );
    let reqs = requests(&ts);
    let (preview_key, _) = blob_request(&reqs).expect("preview for revealed file");
    assert_eq!(preview_key.path, "a/b/deep_target.rs");

    // Symlink preview explicitly shows `symlink → target.txt`.
    select_row(&mut ts, "link.txt");
    let reqs = requests(&ts);
    let (sym_key, sym_ep) = blob_request(&reqs).expect("symlink preview");
    ts.deliver_engine_event(EngineEvent::TreeBlobLoaded {
        blob: Arc::new(TreeBlob {
            key: sym_key,
            size: 10,
            content: TreeBlobContent::Symlink("target.txt".into()),
        }),
        epoch: sym_ep,
    });
    let txt = screen_text(&ts.render(140, 30));
    assert!(txt.contains("symlink → target.txt"), "{txt}");

    // Export to File (`e`) opens input dialog and dispatches ExportTreeBlob.
    select_row(&mut ts, "README.md");
    ts.send_key(KeyCode::Char('e'), KeyModifiers::NONE);
    assert!(matches!(
        ts.state().modal_stack.last(),
        Some(ModalLayer::Dialog(_))
    ));
    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);
    let exported = requests(&ts).into_iter().any(|(r, _)| {
        matches!(r, TaskRequest::ExportTreeBlob { oid, path, .. } if oid == head && path == "README.md")
    });
    assert!(exported, "ExportTreeBlob should be dispatched");

    // Multi-select 2 files with Space and run Get Version -> --staged --worktree option (`s` + Enter).
    select_row(&mut ts, "a/b/deep_target.rs");
    ts.send_key(KeyCode::Char(' '), KeyModifiers::NONE);
    select_row(&mut ts, "README.md");
    ts.send_key(KeyCode::Char(' '), KeyModifiers::NONE);
    ts.send_key(KeyCode::Char('g'), KeyModifiers::NONE);
    ts.send_key(KeyCode::Char('s'), KeyModifiers::NONE);
    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(
        ts.take_git_ops(),
        vec![GitOp::RestorePaths {
            rev: head.to_string(),
            paths: vec!["README.md".into(), "a/b/deep_target.rs".into()],
            staged: true,
            worktree: true,
        }]
    );
}

#[test]
fn go_to_revision_alt_enter_browses_the_resolved_commit() {
    let mut ts = TestSession::with_snapshot(snapshot());
    ts.dispatch(Action::SwitchTab(WorkspaceTab::Log));
    ts.dispatch(Action::OpenPopup {
        kind: PopupKind::GoToRevision,
        initial_query: String::new(),
    });
    ts.type_str("main~2");
    ts.send_key(KeyCode::Enter, KeyModifiers::ALT);
    match ts.state().modal_stack.last() {
        Some(ModalLayer::TreeBrowser(s)) => assert_eq!(s.oid, oid(10)),
        other => panic!("tree browser expected, got {other:?}"),
    }
}
