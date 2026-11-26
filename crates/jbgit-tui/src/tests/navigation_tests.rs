use std::sync::Arc;

use crossterm::event::{KeyCode, KeyModifiers};
use jbgit_core::{
    CommitChangesBundle, DetailsKey, DiffContext, DiffHunk, DiffLine, DiffLineKind, FileChange,
    FileDiff, FileDiffKey, FileStatusKind, PaneId, RequestSlotId, StageEntry, StageStatusSnapshot,
    StashEntry, WhitespaceMode, WorkspaceTab,
};
use jbgit_engine::{EngineEvent, StageDiffKey, StageDiffSide, ThreeDocStageDiff};

use crate::app::{Action, DiffScope, ModalLayer};
use crate::dialogs::{
    ChoiceDialog, ChoiceOption, CreatePatchDialog, CreatePatchRow, Dialog, IdentityDialog,
    IdentityField, MergeDialog, NewBranchDialog, NewBranchField, PullDialog, RebaseDialog,
    StashPushDialog, StashPushField,
};
use crate::keymap::{HelpRow, KeyAction, step_help_selection};
use crate::log_view::{LogSpeedSearchState, PushDialogFocus};
use crate::nav::{jump_sorted_anchor, step_index, step_selectable, step_sorted_position};
use crate::repo_lifecycle::CloneField;

use super::{TestSession, make_details, oid, remote_infos, snapshot};

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
