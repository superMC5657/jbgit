//! Apply Patch dialog and Browse Repository at Revision (Spec 12 #13).

use std::sync::Arc;

use crossterm::event::{KeyCode, KeyModifiers};
use jbgit_core::{
    ApplyMode, GitFailure, GitOp, GitOpOutcome, Oid, PaneId, PatchCheck, PatchCheckKey,
    PatchFileStat, PatchInput, RepoSnapshot, TreeBlob, TreeBlobContent, TreeBlobKey, TreeEntry,
    TreeEntryKind, TreeKey, TreeListing, WorkspaceTab,
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

#[test]
fn clipboard_patch_preflight_error_three_way_retry_apply_and_conflicts() {
    let mut ts = TestSession::with_snapshot(snapshot());
    ts.dispatch(Action::SwitchTab(WorkspaceTab::Commit));
    let _ = ts.take_requests();

    // `<A-S-p>` = Apply Patch from Clipboard.
    ts.send_key(KeyCode::Char('p'), KeyModifiers::ALT | KeyModifiers::SHIFT);
    assert_eq!(top_apply_patch(&ts).source, PatchInput::Clipboard);
    let (key, epoch) = patch_check_request(&ts);
    assert_eq!(key.mode, ApplyMode::WorkingTree);
    let buf = ts.render(120, 30);
    assert!(screen_text(&buf).contains("Checking patch"));

    // Working-tree check fails: first error shown, 3-way suggested, Apply disabled.
    ts.deliver_engine_event(EngineEvent::PatchChecked {
        check: Arc::new(PatchCheck {
            key: key.clone(),
            files: Vec::new(),
            error: Some("error: f.txt: patch does not apply".into()),
        }),
        epoch,
    });
    let text = screen_text(&ts.render(120, 30));
    assert!(text.contains("patch does not apply"), "{text}");
    assert!(text.contains("3-way"), "{text}");
    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);
    assert!(matches!(
        ts.state().modal_stack.last(),
        Some(ModalLayer::ApplyPatch(_))
    ));
    assert!(ts.take_git_ops().is_empty());

    // `3` switches to 3-way and re-checks.
    ts.send_key(KeyCode::Char('3'), KeyModifiers::NONE);
    let (key3, epoch3) = patch_check_request(&ts);
    assert_eq!(key3.mode, ApplyMode::ThreeWay);
    ts.deliver_engine_event(EngineEvent::PatchChecked {
        check: Arc::new(PatchCheck {
            key: key3,
            files: vec![stat("f.txt", 1, 1, true), stat("sub/g.txt", 2, 0, false)],
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

    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);
    assert!(ts.state().modal_stack.is_empty());
    let op = GitOp::ApplyPatch {
        source: PatchInput::Clipboard,
        mode: ApplyMode::ThreeWay,
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

    // Get Version asks first, then checks the path out of that revision.
    ts.send_key(KeyCode::Char('g'), KeyModifiers::NONE);
    assert!(matches!(
        ts.state().modal_stack.last(),
        Some(ModalLayer::Dialog(_))
    ));
    ts.send_key(KeyCode::Char('g'), KeyModifiers::NONE);
    assert_eq!(
        ts.take_git_ops(),
        vec![GitOp::CheckoutPaths {
            rev: head.to_string(),
            paths: vec!["src/main.rs".into()],
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

    // Copy path, then Compare with Local closes the browser into the Log diff.
    ts.send_key(KeyCode::Char('y'), KeyModifiers::NONE);
    assert_eq!(ts.clipboard().last().map(String::as_str), Some("vendor"));
    select_row(&mut ts, "src/main.rs");
    ts.send_key(KeyCode::Char('c'), KeyModifiers::NONE);
    assert!(ts.state().modal_stack.is_empty());
    assert_eq!(ts.state().focused_pane, PaneId::LogChangesTree);
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
