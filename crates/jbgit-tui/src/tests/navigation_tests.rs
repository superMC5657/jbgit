use std::sync::Arc;

use crossterm::event::{KeyCode, KeyModifiers};
use jbgit_core::{
    CommitChangesBundle, DetailsKey, DiffContext, DiffHunk, DiffLine, DiffLineKind, FileChange,
    FileDiff, FileDiffKey, FileStatusKind, Oid, OutgoingBundle, PaneId, ReflogActionKind,
    ReflogBundle, ReflogEntry, ReflogKey, RequestSlotId, StageEntry, StageStatusSnapshot,
    StashEntry, WhitespaceMode, WorkspaceTab,
};
use jbgit_engine::{EngineEvent, StageDiffKey, StageDiffSide, TaskRequest, ThreeDocStageDiff};

use crate::app::{Action, DiffScope, ModalLayer};
use crate::dialogs::{
    ChoiceDialog, ChoiceOption, CreatePatchDialog, CreatePatchRow, Dialog, IdentityDialog,
    IdentityField, MergeDialog, NewBranchDialog, NewBranchField, PullDialog, RebaseDialog,
    StashPushDialog, StashPushField,
};
use crate::keymap::{HelpRow, KeyAction, step_help_selection};
use crate::log_view::{LogSpeedSearchState, PushDialogFocus, ReflogFocus};
use crate::nav::{jump_sorted_anchor, step_index, step_selectable, step_sorted_position};
use crate::repo_lifecycle::CloneField;

use super::{TestSession, make_details, oid, remote_infos, row_text, snapshot};

fn mk_diff(details: DetailsKey, path: &str, lines: &[&str]) -> FileDiff {
    FileDiff {
        key: FileDiffKey {
            details,
            path: path.into(),
            old_path: None,
            context: DiffContext::Three,
            whitespace: WhitespaceMode::Default,
        },
        hunks: vec![DiffHunk {
            header: "@@ -1,1 +1,10 @@".into(),
            lines: lines
                .iter()
                .enumerate()
                .map(|(i, text)| DiffLine {
                    kind: DiffLineKind::Added,
                    old_lineno: None,
                    new_lineno: Some(i as u32 + 1),
                    text: (*text).into(),
                    word_ranges: Vec::new(),
                })
                .collect(),
        }],
        is_binary: false,
        binary: None,
        is_truncated: false,
        is_too_large: false,
        lfs: None,
        mode_change: None,
    }
}

fn mk_change(path: &str) -> FileChange {
    FileChange {
        path: path.into(),
        old_path: None,
        kind: FileStatusKind::Modified,
        additions: 1,
        deletions: 0,
        old_oid: None,
        new_oid: None,
        is_binary: false,
        exists_in_workdir: true,
    }
}

#[test]
fn nav_helpers_handle_magnitude_clamping_and_wrapping() {
    assert_eq!(step_index(0, 0, 1, false), 0);
    assert_eq!(step_index(0, 3, -1, false), 0);
    assert_eq!(step_index(2, 3, 1, false), 2);
    assert_eq!(step_index(0, 3, -1, true), 2);
    assert_eq!(step_index(2, 3, 1, true), 0);

    // Selectable indices: 1, 3, 4 (0 and 2 are headers/separators).
    let rows = [false, true, false, true, true];
    assert_eq!(step_selectable(&rows, 1, 2, false, |&b| b), 4);
    assert_eq!(step_selectable(&rows, 1, 10, false, |&b| b), 4);
    assert_eq!(step_selectable(&rows, 4, -2, false, |&b| b), 1);
    assert_eq!(step_selectable(&rows, 4, -10, false, |&b| b), 1);
    assert_eq!(step_selectable(&rows, 3, 2, true, |&b| b), 1);
    assert_eq!(step_selectable(&rows, 1, -1, true, |&b| b), 4);

    assert_eq!(step_sorted_position(&[2, 4, 7], 0, -1), Some(2));
    assert_eq!(step_sorted_position(&[2, 4, 7], 0, 1), Some(2));
    assert_eq!(step_sorted_position(&[2, 4, 7], 3, 1), Some(4));
    assert_eq!(step_sorted_position(&[2, 4, 7], 7, 1), Some(7));
    assert_eq!(step_sorted_position(&[], 0, 1), None);

    assert_eq!(jump_sorted_anchor(&[0, 5, 10], 0, -1), None);
    assert_eq!(jump_sorted_anchor(&[0, 5, 10], 0, 1), Some(5));
    assert_eq!(jump_sorted_anchor(&[0, 5, 10], 10, 1), None);
}

#[test]
fn non_wrapping_navigation_across_popups_dialogs_and_diff_jumps() {
    // 1. ShortcutsHelp selection clamps at first and last selectable rows.
    let help_rows = vec![
        HelpRow::Section("Header".into()),
        HelpRow::Entry {
            keys: "a".into(),
            desc: "First".into(),
            scope_tag: "Global".into(),
            run: Some(KeyAction::Refresh),
        },
        HelpRow::Section("Mid".into()),
        HelpRow::Entry {
            keys: "b".into(),
            desc: "Last".into(),
            scope_tag: "Global".into(),
            run: Some(KeyAction::Quit),
        },
    ];
    assert_eq!(step_help_selection(&help_rows, 1, -1), 1);
    assert_eq!(step_help_selection(&help_rows, 1, -5), 1);
    assert_eq!(step_help_selection(&help_rows, 1, 1), 3);
    assert_eq!(step_help_selection(&help_rows, 3, 1), 3);
    assert_eq!(step_help_selection(&help_rows, 3, 5), 3);

    // 2. Dialog helpers clamp when wrap == false and wrap when wrap == true.
    let mut merge = MergeDialog::new("feat", "main", Some(2), Default::default());
    merge.move_selection(-1, false);
    assert_eq!(merge.selected, 0);
    merge.move_selection(100, false);
    let merge_last = merge.selected;
    merge.move_selection(1, false);
    assert_eq!(merge.selected, merge_last);
    merge.move_selection(1, true);
    assert_eq!(merge.selected, 0);

    let mut pull = PullDialog::from_snapshot(&snapshot(), Default::default());
    pull.move_selection(-1, false);
    assert_eq!(pull.selected, 0);
    pull.move_selection(100, false);
    let pull_last = pull.selected;
    pull.move_selection(1, false);
    assert_eq!(pull.selected, pull_last);
    pull.move_selection(1, true);
    assert_eq!(pull.selected, 0);

    let mut rebase =
        RebaseDialog::from_snapshot(&snapshot(), Some("main".into()), None, Default::default());
    rebase.move_selection(-1, false);
    assert_eq!(rebase.selected, 0);
    rebase.move_selection(100, false);
    let rebase_last = rebase.selected;
    rebase.move_selection(1, false);
    assert_eq!(rebase.selected, rebase_last);
    rebase.move_selection(1, true);
    assert_eq!(rebase.selected, 0);

    let mut patch = CreatePatchDialog {
        source: jbgit_core::PatchSource::CommitFile {
            oid: oid(1),
            path: "a.rs".into(),
        },
        to_clipboard: true,
        path: String::new(),
        base: std::path::PathBuf::from("."),
        reverse: false,
        selected: 0,
        selected_all: false,
    };
    patch.move_selection(-1, false);
    assert_eq!(patch.selected, 0);
    patch.move_selection(1, false);
    assert_eq!(patch.row(), CreatePatchRow::Reverse);
    patch.move_selection(1, false);
    assert_eq!(patch.row(), CreatePatchRow::Reverse);
    patch.move_selection(1, true);
    assert_eq!(patch.row(), CreatePatchRow::Destination);

    let mut stash_dlg = StashPushDialog::new("", false, vec![]);
    stash_dlg.move_focus(-1, false);
    assert_eq!(stash_dlg.focused, StashPushField::Message);
    stash_dlg.move_focus(10, false);
    assert_eq!(stash_dlg.focused, StashPushField::KeepIndex);
    stash_dlg.move_focus(1, false);
    assert_eq!(stash_dlg.focused, StashPushField::KeepIndex);
    stash_dlg.move_focus(1, true);
    assert_eq!(stash_dlg.focused, StashPushField::Message);

    // 3. LogSpeedSearchState::step clamps when wrap == false and wraps when wrap == true.
    let mut ss = LogSpeedSearchState {
        matches: vec![2, 5, 9],
        current: Some(0),
        ..Default::default()
    };
    assert_eq!(ss.step(false, false), Some(2));
    assert_eq!(ss.step(false, true), Some(9));
    assert_eq!(ss.step(true, false), Some(9));
    assert_eq!(ss.step(true, true), Some(2));

    // 4. Interactive dialogs: Up/Down clamp while Tab/BackTab (and Choice Left/Right) still wrap.
    let mut snap = snapshot();
    snap.remotes = remote_infos(&["origin", "upstream"]);
    let mut ts = TestSession::with_snapshot(snap);

    // ChoiceDialog: Up/Down clamp, Left/Right/Tab/BackTab wrap.
    ts.dispatch(Action::OpenDialog(Box::new(Dialog::Choice(
        ChoiceDialog::radio(
            "Pick",
            vec![],
            vec![
                ChoiceOption::new("One", None, vec![]),
                ChoiceOption::new("Two", None, vec![]),
            ],
            0,
            "OK",
        ),
    ))));
    let choice_sel = |ts: &TestSession| match ts.state().modal_stack.last() {
        Some(ModalLayer::Dialog(d)) => match d.as_ref() {
            Dialog::Choice(c) => c.selected,
            _ => panic!(),
        },
        _ => panic!(),
    };
    ts.send_key(KeyCode::Up, KeyModifiers::NONE);
    assert_eq!(choice_sel(&ts), 0);
    ts.send_key(KeyCode::Down, KeyModifiers::NONE);
    ts.send_key(KeyCode::Down, KeyModifiers::NONE);
    assert_eq!(choice_sel(&ts), 1);
    ts.send_key(KeyCode::Tab, KeyModifiers::NONE);
    assert_eq!(choice_sel(&ts), 0);
    ts.send_key(KeyCode::Left, KeyModifiers::NONE);
    assert_eq!(choice_sel(&ts), 1);
    ts.dispatch(Action::PopModal);

    // IdentityDialog: Up/Down clamp, Tab/BackTab wrap.
    ts.dispatch(Action::OpenDialog(Box::new(Dialog::Identity(
        IdentityDialog::new(None, None, None),
    ))));
    let id_focus = |ts: &TestSession| match ts.state().modal_stack.last() {
        Some(ModalLayer::Dialog(d)) => match d.as_ref() {
            Dialog::Identity(id) => id.focused,
            _ => panic!(),
        },
        _ => panic!(),
    };
    ts.send_key(KeyCode::Up, KeyModifiers::NONE);
    assert_eq!(id_focus(&ts), IdentityField::Name);
    ts.send_key(KeyCode::BackTab, KeyModifiers::SHIFT);
    assert_eq!(id_focus(&ts), IdentityField::Global);
    ts.send_key(KeyCode::Down, KeyModifiers::NONE);
    assert_eq!(id_focus(&ts), IdentityField::Global);
    ts.send_key(KeyCode::Tab, KeyModifiers::NONE);
    assert_eq!(id_focus(&ts), IdentityField::Name);
    ts.dispatch(Action::PopModal);

    // NewBranchDialog: Up/Down clamp, Tab/BackTab wrap.
    ts.dispatch(Action::OpenDialog(Box::new(Dialog::NewBranch(
        NewBranchDialog::new("Title", "Prompt", "HEAD", "", vec![], false),
    ))));
    let nb_focus = |ts: &TestSession| match ts.state().modal_stack.last() {
        Some(ModalLayer::Dialog(d)) => match d.as_ref() {
            Dialog::NewBranch(nb) => nb.focused,
            _ => panic!(),
        },
        _ => panic!(),
    };
    ts.send_key(KeyCode::Up, KeyModifiers::NONE);
    assert_eq!(nb_focus(&ts), NewBranchField::Name);
    ts.send_key(KeyCode::BackTab, KeyModifiers::SHIFT);
    assert_eq!(nb_focus(&ts), NewBranchField::AutoSanitize);
    ts.send_key(KeyCode::Down, KeyModifiers::NONE);
    assert_eq!(nb_focus(&ts), NewBranchField::AutoSanitize);
    ts.send_key(KeyCode::Tab, KeyModifiers::NONE);
    assert_eq!(nb_focus(&ts), NewBranchField::Name);
    ts.dispatch(Action::PopModal);

    // CloneDialog: Up/Down clamp, Tab/BackTab wrap.
    ts.dispatch(Action::OpenCloneRepo { base_dir: None });
    let clone_focus = |ts: &TestSession| match ts.state().modal_stack.last() {
        Some(ModalLayer::CloneRepo(cd)) => cd.focus,
        _ => panic!(),
    };
    ts.send_key(KeyCode::Up, KeyModifiers::NONE);
    assert_eq!(clone_focus(&ts), CloneField::Url);
    ts.send_key(KeyCode::BackTab, KeyModifiers::SHIFT);
    assert_eq!(clone_focus(&ts), CloneField::RecurseSubmodules);
    ts.send_key(KeyCode::Down, KeyModifiers::NONE);
    assert_eq!(clone_focus(&ts), CloneField::RecurseSubmodules);
    ts.send_key(KeyCode::Tab, KeyModifiers::NONE);
    assert_eq!(clone_focus(&ts), CloneField::Url);
    ts.dispatch(Action::PopModal);

    // 5. BranchesPopup Up/Down clamping.
    ts.dispatch(Action::OpenBranchesPopup {
        query: String::new(),
    });
    ts.send_key(KeyCode::PageUp, KeyModifiers::NONE);
    ts.send_key(KeyCode::PageUp, KeyModifiers::NONE);
    let first_bp_sel = match ts.state().modal_stack.last() {
        Some(ModalLayer::BranchesPopup { selected, .. }) => *selected,
        _ => panic!(),
    };
    ts.send_key(KeyCode::Up, KeyModifiers::NONE);
    let after_up = match ts.state().modal_stack.last() {
        Some(ModalLayer::BranchesPopup { selected, .. }) => *selected,
        _ => panic!(),
    };
    assert_eq!(after_up, first_bp_sel);
    ts.send_key(KeyCode::PageDown, KeyModifiers::NONE);
    ts.send_key(KeyCode::PageDown, KeyModifiers::NONE);
    let last_bp_sel = match ts.state().modal_stack.last() {
        Some(ModalLayer::BranchesPopup { selected, .. }) => *selected,
        _ => panic!(),
    };
    ts.send_key(KeyCode::Down, KeyModifiers::NONE);
    let after_down = match ts.state().modal_stack.last() {
        Some(ModalLayer::BranchesPopup { selected, .. }) => *selected,
        _ => panic!(),
    };
    assert_eq!(after_down, last_bp_sel);
    ts.dispatch(Action::PopModal);

    // 6. PushDialog: Target Up does not wrap (while BackTab still wraps to Files); completion/remote picker clamp.
    ts.dispatch(Action::PromptPush {
        branch: "main".into(),
        up_to: None,
    });
    ts.dispatch(Action::PushSetFocus(PushDialogFocus::Target));
    ts.send_key(KeyCode::Up, KeyModifiers::NONE);
    assert_eq!(
        ts.state().modal_stack.push_dialog().unwrap().focus,
        PushDialogFocus::Target
    );
    ts.send_key(KeyCode::BackTab, KeyModifiers::SHIFT);
    assert_eq!(
        ts.state().modal_stack.push_dialog().unwrap().focus,
        PushDialogFocus::Files
    );
    ts.send_key(KeyCode::Tab, KeyModifiers::NONE);
    assert_eq!(
        ts.state().modal_stack.push_dialog().unwrap().focus,
        PushDialogFocus::Target
    );
    ts.dispatch(Action::PushToggleRemotePicker);
    ts.dispatch(Action::PushMoveRemotePicker(-1));
    assert_eq!(ts.state().modal_stack.push_dialog().unwrap().remote_idx, 0);
    ts.dispatch(Action::PushMoveRemotePicker(1));
    assert_eq!(ts.state().modal_stack.push_dialog().unwrap().remote_idx, 1);
    ts.dispatch(Action::PushMoveRemotePicker(1));
    assert_eq!(ts.state().modal_stack.push_dialog().unwrap().remote_idx, 1);
    ts.dispatch(Action::PushToggleRemotePicker);
    ts.dispatch(Action::PushStartEditTarget);
    ts.dispatch(Action::PushEditTargetClear);
    ts.dispatch(Action::PushEditTargetMoveCompletion(-1));
    assert_eq!(
        ts.state()
            .modal_stack
            .push_dialog()
            .unwrap()
            .completion_selected,
        0
    );
    ts.dispatch(Action::PushEditTargetMoveCompletion(100));
    let last_comp = ts
        .state()
        .modal_stack
        .push_dialog()
        .unwrap()
        .completion_selected;
    ts.dispatch(Action::PushEditTargetMoveCompletion(1));
    assert_eq!(
        ts.state()
            .modal_stack
            .push_dialog()
            .unwrap()
            .completion_selected,
        last_comp
    );
    ts.dispatch(Action::PopModal);

    // 7. LogSpeedSearch in-table editing Up/Down clamp while browse n/N wrap.
    ts.send_key(KeyCode::Char('s'), KeyModifiers::NONE);
    for ch in "main".chars() {
        ts.send_key(KeyCode::Char(ch), KeyModifiers::NONE);
    }
    let ss_len = ts.state().log.speed_search().matches.len();
    assert!(ss_len >= 2);
    assert_eq!(ts.state().log.speed_search().current, Some(0));
    ts.send_key(KeyCode::Up, KeyModifiers::NONE);
    assert_eq!(ts.state().log.speed_search().current, Some(0));
    for _ in 0..ss_len {
        ts.send_key(KeyCode::Down, KeyModifiers::NONE);
    }
    assert_eq!(ts.state().log.speed_search().current, Some(ss_len - 1));
    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);
    ts.send_key(KeyCode::Char('n'), KeyModifiers::NONE);
    assert_eq!(ts.state().log.speed_search().current, Some(0));
    ts.send_key(KeyCode::Char('N'), KeyModifiers::SHIFT);
    assert_eq!(ts.state().log.speed_search().current, Some(ss_len - 1));
    ts.send_key(KeyCode::Esc, KeyModifiers::NONE);

    // 8. Stage workbench: `{`/`}` file jump clamps and preserves multi_selected when clamped;
    //    `[`/`]` hunk jump and author completion clamp.
    ts.dispatch(Action::SwitchTab(WorkspaceTab::Commit));
    ts.deliver_engine_event(EngineEvent::StageStatusLoaded {
        status: Arc::new(StageStatusSnapshot {
            staged: vec![],
            unstaged: vec![
                StageEntry {
                    path: "a.rs".into(),
                    old_path: None,
                    kind: FileStatusKind::Modified,
                },
                StageEntry {
                    path: "b.rs".into(),
                    old_path: None,
                    kind: FileStatusKind::Modified,
                },
            ],
            untracked: vec![],
            conflicted: vec![],
            ignored: vec![],
            ..Default::default()
        }),
        epoch: 1,
    });
    // Starts on container row 0; `PrevFile` (`{`) clamps to first file (row 1).
    ts.dispatch(Action::DiffNextFile(DiffScope::Stage, -1));
    assert_eq!(ts.state().stage_workbench.selected_idx(), 1);
    ts.dispatch(Action::StageToggleMulti(1));
    assert!(!ts.state().stage_workbench.multi_selected().is_empty());
    // Clamped `PrevFile` at first file must NOT clear `multi_selected`.
    ts.dispatch(Action::DiffNextFile(DiffScope::Stage, -1));
    assert_eq!(ts.state().stage_workbench.selected_idx(), 1);
    assert!(!ts.state().stage_workbench.multi_selected().is_empty());
    // Actually moving to next file DOES clear `multi_selected`.
    ts.dispatch(Action::DiffNextFile(DiffScope::Stage, 1));
    assert_eq!(ts.state().stage_workbench.selected_idx(), 2);
    assert!(ts.state().stage_workbench.multi_selected().is_empty());
    ts.dispatch(Action::DiffNextFile(DiffScope::Stage, 1));
    assert_eq!(ts.state().stage_workbench.selected_idx(), 2);

    let diff_key = StageDiffKey {
        path: "b.rs".into(),
        old_path: None,
        side: StageDiffSide::Unstaged,
        context: DiffContext::Three,
        whitespace: WhitespaceMode::Default,
    };
    let vcs = b"1\n2\n3\n4\n5\n6\n7\n8\n9\n10\n";
    let local = b"1a\n2\n3\n4\n5\n6\n7\n8\n9\n10b\n";
    let tracker =
        jbgit_core::StageLineTracker::new_with_whitespace(vcs, vcs, local, WhitespaceMode::Default);
    let (hunks, is_truncated) = tracker.build_interactive_hunks(false, DiffContext::Three, 20_000);
    assert_eq!(hunks.len(), 2);
    ts.deliver_engine_event(EngineEvent::StageFileDiffLoaded {
        diff: Arc::new(ThreeDocStageDiff {
            key: diff_key,
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
        epoch: ts
            .state()
            .request_slots
            .latest(RequestSlotId::StageFileDiff)
            .max(1),
    });
    ts.dispatch(Action::DiffJumpHunk(DiffScope::Stage, -1));
    assert_eq!(ts.state().stage_workbench.diff_cursor_row(), 0);
    ts.dispatch(Action::DiffJumpHunk(DiffScope::Stage, 1));
    let second_hunk_row = ts.state().stage_workbench.diff_cursor_row();
    assert!(second_hunk_row > 0);
    ts.dispatch(Action::DiffJumpHunk(DiffScope::Stage, 1));
    assert_eq!(
        ts.state().stage_workbench.diff_cursor_row(),
        second_hunk_row
    );

    ts.dispatch(Action::CommitFocusAuthorInput);
    let cand_count = ts
        .state()
        .stage_workbench
        .author_candidates(ts.state().repo_snapshot.as_deref())
        .len();
    assert!(cand_count >= 1);
    ts.dispatch(Action::CommitAuthorCycleCompletion(-1));
    assert_eq!(ts.state().stage_workbench.author_completion_idx(), 0);
    ts.dispatch(Action::CommitAuthorCycleCompletion(100));
    assert_eq!(
        ts.state().stage_workbench.author_completion_idx(),
        cand_count - 1
    );
}

#[test]
fn clamped_file_jump_preserves_diff_viewport_and_search_in_log_and_stash() {
    let mut snap = snapshot();
    snap.stashes = vec![StashEntry {
        index: 0,
        oid: oid(90),
        message: "WIP on main: stash".into(),
        timestamp: 0,
    }];
    let mut ts = TestSession::with_snapshot(snap);

    // 1. Log Details `{`/`}` clamped no-op preserves scroll and inline search state.
    let head = oid(12);
    let log_key = DetailsKey::single(head);
    ts.dispatch(Action::ToggleDiffPreview);
    ts.deliver_engine_event(EngineEvent::CommitDetailsLoaded {
        slot: RequestSlotId::LogCommitDetails,
        bundle: Arc::new(CommitChangesBundle {
            key: log_key.clone(),
            details: vec![make_details(head, vec![], "commit")],
            changes: vec![mk_change("first.rs"), mk_change("second.rs")],
        }),
        epoch: ts
            .state()
            .request_slots
            .latest(RequestSlotId::LogCommitDetails)
            .max(1),
    });
    let lines: Vec<&str> = (0..100).map(|_| "match_line").collect();
    ts.deliver_engine_event(EngineEvent::FileDiffLoaded {
        slot: RequestSlotId::LogFileDiff,
        diff: Arc::new(mk_diff(log_key.clone(), "first.rs", &lines)),
        epoch: ts
            .state()
            .request_slots
            .latest(RequestSlotId::LogFileDiff)
            .max(1),
    });
    ts.dispatch(Action::FocusPane(PaneId::LogDetailsDiff));
    let _ = ts.render(140, 24);
    ts.dispatch(Action::DiffSearchOpen(DiffScope::Log));
    for ch in "match".chars() {
        ts.dispatch(Action::DiffSearchChar(DiffScope::Log, ch));
    }
    ts.dispatch(Action::DiffSearchConfirm(DiffScope::Log));
    ts.dispatch(Action::DiffScroll(DiffScope::Log, 4));
    let saved_offset = ts.state().log.details.diff.scroll.offset();
    assert!(saved_offset > 0);
    assert!(!ts.state().log.details.diff.search.matches.is_empty());

    // Clamped `PrevFile` at first file must NOT reset scroll or clear search matches.
    ts.dispatch(Action::DiffNextFile(DiffScope::Log, -1));
    assert_eq!(ts.state().log.details.diff.scroll.offset(), saved_offset);
    assert!(!ts.state().log.details.diff.search.matches.is_empty());

    // Moving to second file DOES reset view offsets.
    ts.dispatch(Action::DiffNextFile(DiffScope::Log, 1));
    assert_eq!(ts.state().log.details.diff.scroll.offset(), 0);
    assert!(ts.state().log.details.diff.search.matches.is_empty());

    // 2. Stash `{`/`}` clamped no-op preserves scroll offset.
    ts.dispatch(Action::SwitchTab(WorkspaceTab::Stash));
    let stash_key = DetailsKey::single(oid(90));
    ts.deliver_engine_event(EngineEvent::CommitDetailsLoaded {
        slot: RequestSlotId::StashCommitDetails,
        bundle: Arc::new(CommitChangesBundle {
            key: stash_key.clone(),
            details: vec![make_details(oid(90), vec![], "stash")],
            changes: vec![mk_change("s1.rs"), mk_change("s2.rs")],
        }),
        epoch: ts
            .state()
            .request_slots
            .latest(RequestSlotId::StashCommitDetails)
            .max(1),
    });
    ts.deliver_engine_event(EngineEvent::FileDiffLoaded {
        slot: RequestSlotId::StashFileDiff,
        diff: Arc::new(mk_diff(stash_key, "s1.rs", &lines)),
        epoch: ts
            .state()
            .request_slots
            .latest(RequestSlotId::StashFileDiff)
            .max(1),
    });
    let _ = ts.render(140, 24);
    ts.dispatch(Action::DiffScroll(DiffScope::Stash, 4));
    let stash_saved_offset = ts.state().stash.diff().scroll.offset();
    assert!(stash_saved_offset > 0);

    ts.dispatch(Action::DiffNextFile(DiffScope::Stash, -1));
    assert_eq!(ts.state().stash.diff().scroll.offset(), stash_saved_offset);

    ts.dispatch(Action::DiffNextFile(DiffScope::Stash, 1));
    assert_eq!(ts.state().stash.diff().scroll.offset(), 0);
}

#[test]
fn log_changes_tree_braces_switch_commits_with_clamp_and_scope_isolation() {
    use crate::config::{KeymapConfig, KeymapPreset};
    use crate::keymap::{MatchResult, actions::check_scopes};
    use crossterm::event::KeyEvent;

    let mut ts = TestSession::with_snapshot(snapshot());
    ts.dispatch(Action::SwitchTab(WorkspaceTab::Log));
    ts.dispatch(Action::FocusPane(PaneId::LogChangesTree));
    assert_eq!(ts.state().log.graph.selected_idx, 0);

    // 1. Pressing `}` (both NONE and SHIFT) in LogChangesTree moves down a commit,
    //    keeps focus on LogChangesTree, and updates `details.changes.current_key`.
    ts.send_key(KeyCode::Char('}'), KeyModifiers::NONE);
    assert_eq!(ts.state().log.graph.selected_idx, 1);
    assert_eq!(ts.state().focused_pane, PaneId::LogChangesTree);
    assert_eq!(
        ts.state()
            .log
            .details
            .changes
            .current_key
            .as_ref()
            .map(|k| k.oids.as_slice()),
        Some([oid(11)].as_slice())
    );

    ts.send_key(KeyCode::Char('}'), KeyModifiers::SHIFT);
    assert_eq!(ts.state().log.graph.selected_idx, 2);
    assert_eq!(ts.state().focused_pane, PaneId::LogChangesTree);
    assert_eq!(
        ts.state()
            .log
            .details
            .changes
            .current_key
            .as_ref()
            .map(|k| k.oids.as_slice()),
        Some([oid(10)].as_slice())
    );

    // Pressing `{` moves back up a commit while keeping focus on LogChangesTree.
    ts.send_key(KeyCode::Char('{'), KeyModifiers::SHIFT);
    assert_eq!(ts.state().log.graph.selected_idx, 1);
    assert_eq!(ts.state().focused_pane, PaneId::LogChangesTree);

    ts.send_key(KeyCode::Char('{'), KeyModifiers::NONE);
    assert_eq!(ts.state().log.graph.selected_idx, 0);
    assert_eq!(ts.state().focused_pane, PaneId::LogChangesTree);

    fn load_commit_changes_and_scroll_diff(
        ts: &mut TestSession,
        commit_oid: Oid,
        parents: Vec<Oid>,
        subject: &str,
        files: &[&str],
        diff_lines: &[&str],
    ) -> usize {
        let key = DetailsKey::single(commit_oid);
        ts.dispatch(Action::ToggleDiffPreview);
        ts.deliver_engine_event(EngineEvent::CommitDetailsLoaded {
            slot: RequestSlotId::LogCommitDetails,
            bundle: Arc::new(CommitChangesBundle {
                key: key.clone(),
                details: vec![make_details(commit_oid, parents, subject)],
                changes: files.iter().map(|p| mk_change(p)).collect(),
            }),
            epoch: ts
                .state()
                .request_slots
                .latest(RequestSlotId::LogCommitDetails)
                .max(1),
        });
        ts.deliver_engine_event(EngineEvent::FileDiffLoaded {
            slot: RequestSlotId::LogFileDiff,
            diff: Arc::new(mk_diff(key, files[1], diff_lines)),
            epoch: ts
                .state()
                .request_slots
                .latest(RequestSlotId::LogFileDiff)
                .max(1),
        });
        ts.dispatch(Action::ChangesSelect(crate::app::ChangesHost::Log, 1));
        let _ = ts.render(140, 24);
        ts.dispatch(Action::DiffScroll(DiffScope::Log, 4));
        ts.state().log.details.diff.scroll.offset()
    }

    // 2. Clamped `{` at row 0 must NOT reset `changes.selected` or `diff.scroll`.
    let lines: Vec<&str> = (0..100).map(|_| "diff_line").collect();
    let saved_scroll = load_commit_changes_and_scroll_diff(
        &mut ts,
        oid(12),
        vec![oid(11)],
        "c12",
        &["first.rs", "second.rs"],
        &lines,
    );
    assert_eq!(ts.state().log.details.changes.selected, 1);
    assert!(saved_scroll > 0);

    ts.send_key(KeyCode::Char('{'), KeyModifiers::NONE);
    assert_eq!(ts.state().log.graph.selected_idx, 0);
    assert_eq!(ts.state().log.details.changes.selected, 1);
    assert_eq!(ts.state().log.details.diff.scroll.offset(), saved_scroll);

    // 3. Clamped `}` at last visible commit never moves onto `[Load More]` tail row
    //    and preserves `changes.selected` / `diff.scroll`.
    let mut more_snap = snapshot();
    more_snap.has_more_commits = true;
    let mut more_ts = TestSession::with_snapshot(more_snap);
    more_ts.dispatch(Action::SwitchTab(WorkspaceTab::Log));
    let last_idx = more_ts.state().log.graph.visible_count() - 1;
    more_ts.dispatch(Action::SelectLogRow(last_idx));
    more_ts.dispatch(Action::FocusPane(PaneId::LogChangesTree));
    let tail_scroll = load_commit_changes_and_scroll_diff(
        &mut more_ts,
        oid(1),
        vec![],
        "c1",
        &["tail1.rs", "tail2.rs"],
        &lines,
    );
    assert!(more_ts.state().log.graph.has_load_more_row());
    assert!(tail_scroll > 0);

    more_ts.send_key(KeyCode::Char('}'), KeyModifiers::NONE);
    assert_eq!(more_ts.state().log.graph.selected_idx, last_idx);
    assert!(!more_ts.state().log.graph.is_cursor_on_load_more());
    assert_eq!(more_ts.state().log.details.changes.selected, 1);
    assert_eq!(
        more_ts.state().log.details.diff.scroll.offset(),
        tail_scroll
    );

    // When cursor is already on `[Load More]`, `}` is a no-op (does not step upward),
    // while `{` steps back up onto the last real commit.
    more_ts.dispatch(Action::SelectLogRow(last_idx + 1));
    assert!(more_ts.state().log.graph.is_cursor_on_load_more());
    more_ts.send_key(KeyCode::Char('}'), KeyModifiers::NONE);
    assert_eq!(more_ts.state().log.graph.selected_idx, last_idx + 1);
    assert!(more_ts.state().log.graph.is_cursor_on_load_more());
    more_ts.send_key(KeyCode::Char('{'), KeyModifiers::SHIFT);
    assert_eq!(more_ts.state().log.graph.selected_idx, last_idx);
    assert!(!more_ts.state().log.graph.is_cursor_on_load_more());

    // 4. Multi-selection in Log collapses cleanly to single adjacent commit.
    ts.dispatch(Action::SelectLogRow(0));
    ts.dispatch(Action::ExtendLogSelectionTo(2));
    assert!(ts.state().log.graph.has_multi_selection());
    ts.dispatch(Action::FocusPane(PaneId::LogChangesTree));
    ts.send_key(KeyCode::Char('}'), KeyModifiers::NONE);
    assert!(!ts.state().log.graph.has_multi_selection());
    assert_eq!(ts.state().log.graph.selected_idx, 3);

    // 5. Scope isolation:
    //    - In LogDetailsDiff with diff_preview=true, `{`/`}` switch files without moving commit.
    ts.dispatch(Action::SelectLogRow(0));
    ts.dispatch(Action::ChangesSelect(crate::app::ChangesHost::Log, 0));
    ts.dispatch(Action::FocusPane(PaneId::LogDetailsDiff));
    ts.send_key(KeyCode::Char('}'), KeyModifiers::NONE);
    assert_eq!(ts.state().log.graph.selected_idx, 0);
    assert_eq!(ts.state().log.details.changes.selected, 1);

    //    - In LogCommitTable, `{`/`}` do NOT move selected_idx.
    ts.dispatch(Action::FocusPane(PaneId::LogCommitTable));
    ts.send_key(KeyCode::Char('}'), KeyModifiers::NONE);
    assert_eq!(ts.state().log.graph.selected_idx, 0);
    ts.send_key(KeyCode::Char('{'), KeyModifiers::NONE);
    assert_eq!(ts.state().log.graph.selected_idx, 0);

    // 6. Footer & presets:
    //    - Rendered footer in LogChangesTree shows `{/}` paired pill (`Commit` in en-US, `提交` in zh-CN).
    ts.dispatch(Action::FocusPane(PaneId::LogChangesTree));
    ts.state_mut().i18n = Arc::new(crate::i18n::I18n::load("en-US", None));
    let buf_en = ts.render(160, 40);
    let footer_en = row_text(&buf_en, 39);
    assert!(footer_en.contains("{/}"), "{footer_en}");
    assert!(footer_en.contains("Commit"), "{footer_en}");

    ts.state_mut().i18n = Arc::new(crate::i18n::I18n::load("zh-CN", None));
    let buf_zh = ts.render(160, 40);
    let footer_zh = row_text(&buf_zh, 39);
    assert!(footer_zh.contains("{/}"), "{footer_zh}");
    assert!(footer_zh.replace(' ', "").contains("提交"), "{footer_zh}");

    //    - All three keymap presets resolve `{` / `}` in LogChangesTree to PrevCommit / NextCommit.
    for preset in [
        KeymapPreset::Default,
        KeymapPreset::Jetbrains,
        KeymapPreset::Vim,
    ] {
        ts.state_mut().config.keymap = KeymapConfig::for_preset(preset.clone());
        let lbrace = KeyEvent::new(KeyCode::Char('{'), KeyModifiers::NONE);
        let rbrace = KeyEvent::new(KeyCode::Char('}'), KeyModifiers::NONE);
        assert!(
            matches!(check_scopes(&[lbrace], ts.state()), MatchResult::Exact(b) if b.run == KeyAction::PrevCommit),
            "expected '{{' -> PrevCommit in {preset:?}"
        );
        assert!(
            matches!(check_scopes(&[rbrace], ts.state()), MatchResult::Exact(b) if b.run == KeyAction::NextCommit),
            "expected '}}' -> NextCommit in {preset:?}"
        );
    }
}

#[test]
fn reflog_and_push_dialog_braces_switch_commits_in_changes_pane() {
    // 1. Reflog modal (`ReflogFocus::Changes`)
    let mut ts = TestSession::with_snapshot(snapshot());
    ts.dispatch(Action::OpenReflog("HEAD".into()));
    let rf_epoch = ts.state().request_slots.latest(RequestSlotId::Reflog);
    ts.deliver_engine_event(EngineEvent::ReflogLoaded {
        bundle: Arc::new(ReflogBundle {
            key: ReflogKey::head(),
            entries: vec![
                ReflogEntry {
                    oid: oid(10),
                    selector: "HEAD@{0}".into(),
                    action: ReflogActionKind::Commit,
                    message: "commit: zero".into(),
                    timestamp: 1_700_000_200,
                    author: "Alice".into(),
                },
                ReflogEntry {
                    oid: oid(11),
                    selector: "HEAD@{1}".into(),
                    action: ReflogActionKind::Commit,
                    message: "commit: one".into(),
                    timestamp: 1_700_000_100,
                    author: "Alice".into(),
                },
            ],
            has_more: false,
        }),
        epoch: rf_epoch,
    });
    ts.dispatch(Action::ReflogFocus(ReflogFocus::Changes));
    ts.send_key(KeyCode::Char('}'), KeyModifiers::NONE);
    {
        let rf = ts.state().modal_stack.reflog().unwrap();
        assert_eq!(rf.selected, 1);
        assert_eq!(rf.focus, ReflogFocus::Changes);
        assert_eq!(
            rf.changes.current_key.as_ref().map(|k| k.oids.as_slice()),
            Some([oid(11)].as_slice())
        );
    }
    ts.send_key(KeyCode::Char('}'), KeyModifiers::NONE);
    assert_eq!(ts.state().modal_stack.reflog().unwrap().selected, 1);
    ts.send_key(KeyCode::Char('{'), KeyModifiers::SHIFT);
    assert_eq!(ts.state().modal_stack.reflog().unwrap().selected, 0);
    ts.send_key(KeyCode::Char('{'), KeyModifiers::NONE);
    assert_eq!(ts.state().modal_stack.reflog().unwrap().selected, 0);

    // Scope isolation: ReflogFocus::Diff and ReflogFocus::Entries ignore `{`/`}`.
    ts.dispatch(Action::ReflogFocus(ReflogFocus::Diff));
    ts.send_key(KeyCode::Char('}'), KeyModifiers::NONE);
    assert_eq!(ts.state().modal_stack.reflog().unwrap().selected, 0);
    ts.dispatch(Action::ReflogFocus(ReflogFocus::Entries));
    ts.send_key(KeyCode::Char('}'), KeyModifiers::NONE);
    assert_eq!(ts.state().modal_stack.reflog().unwrap().selected, 0);
    ts.dispatch(Action::CloseReflog);

    // 2. Push dialog (`PushDialogFocus::Files`)
    let mut push_snap = snapshot();
    push_snap.remotes = remote_infos(&["origin"]);
    let commits_list = push_snap.commits.clone();
    let mut pts = TestSession::with_snapshot(push_snap);
    let _ = pts.take_requests();
    pts.dispatch(Action::PromptPush {
        branch: "main".into(),
        up_to: None,
    });
    let (out_key, out_epoch) = pts
        .take_requests()
        .into_iter()
        .find_map(|(req, ep)| match req {
            TaskRequest::LoadOutgoing { key } => Some((key, ep.expect("epoch"))),
            _ => None,
        })
        .expect("LoadOutgoing request missing");
    pts.deliver_engine_event(EngineEvent::OutgoingLoaded {
        bundle: Arc::new(OutgoingBundle {
            key: out_key,
            commits: commits_list[..3].to_vec(),
            files_changed: 3,
            remote_exists: true,
            remote_oid: Some(oid(10)),
            truncated: false,
        }),
        epoch: out_epoch,
    });

    pts.dispatch(Action::PushSetFocus(PushDialogFocus::Files));
    pts.send_key(KeyCode::Char('}'), KeyModifiers::NONE);
    {
        let pd = pts.state().modal_stack.push_dialog().unwrap();
        assert_eq!(pd.selected_commit, 1);
        assert_eq!(pd.focus, PushDialogFocus::Files);
    }
    pts.send_key(KeyCode::Char('{'), KeyModifiers::SHIFT);
    assert_eq!(
        pts.state()
            .modal_stack
            .push_dialog()
            .unwrap()
            .selected_commit,
        0
    );
    pts.send_key(KeyCode::Char('{'), KeyModifiers::NONE);
    assert_eq!(
        pts.state()
            .modal_stack
            .push_dialog()
            .unwrap()
            .selected_commit,
        0
    );

    // When show_diff is active in Files focus, `{`/`}` must not switch commits or close diff.
    let head_oid = commits_list[0].oid;
    pts.deliver_engine_event(EngineEvent::CommitDetailsLoaded {
        slot: RequestSlotId::PushCommitDetails,
        bundle: Arc::new(CommitChangesBundle {
            key: DetailsKey::single(head_oid),
            details: vec![make_details(head_oid, vec![], "head")],
            changes: vec![mk_change("push.rs")],
        }),
        epoch: pts
            .state()
            .request_slots
            .latest(RequestSlotId::PushCommitDetails)
            .max(1),
    });
    pts.dispatch(Action::PushToggleDiff);
    assert!(pts.state().modal_stack.push_dialog().unwrap().show_diff);
    pts.send_key(KeyCode::Char('}'), KeyModifiers::NONE);
    {
        let pd = pts.state().modal_stack.push_dialog().unwrap();
        assert_eq!(pd.selected_commit, 0);
        assert!(pd.show_diff);
    }
    pts.dispatch(Action::PushToggleDiff);

    // Scope isolation: Commits and Target focus ignore `{`/`}`.
    pts.dispatch(Action::PushSetFocus(PushDialogFocus::Commits));
    pts.send_key(KeyCode::Char('}'), KeyModifiers::NONE);
    assert_eq!(
        pts.state()
            .modal_stack
            .push_dialog()
            .unwrap()
            .selected_commit,
        0
    );
    pts.dispatch(Action::PushSetFocus(PushDialogFocus::Target));
    pts.send_key(KeyCode::Char('}'), KeyModifiers::NONE);
    assert_eq!(
        pts.state()
            .modal_stack
            .push_dialog()
            .unwrap()
            .selected_commit,
        0
    );
}
