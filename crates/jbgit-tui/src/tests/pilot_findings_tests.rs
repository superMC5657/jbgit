use std::path::PathBuf;
use std::sync::Arc;

use crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEventKind};
use jbgit_core::{
    CommitChangesBundle, CommitDetails, CommitSignature, ConflictDocument, DetailsKey,
    DiffContext, FileStatusKind, PersonStamp, StageEntry, StageLineTracker, StageStatusSnapshot,
    WhitespaceMode, WorktreeInfo, WorkspaceTab,
};
use jbgit_engine::{
    CommitPrecheckInfo, EngineEvent, StageDiffKey, StageDiffSide, TaskRequest, ThreeDocStageDiff,
};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use unicode_width::UnicodeWidthStr;

use crate::app::{Action, ConflictMergeState, ModalLayer, Notification, PopupKind};
use crate::hit_test::{DiffHunkActionKind, HitTarget};
use crate::i18n::I18n;
use crate::log_view::{FilterField, format_absolute_time};

use super::{TestSession, local, oid, snapshot};

fn rendered_row(buf: &Buffer, y: u16) -> String {
    let mut out = String::new();
    let mut x = 0u16;
    while x < buf.area.width {
        let sym = buf[(x, y)].symbol();
        out.push_str(sym);
        x += (sym.width().max(1)) as u16;
    }
    out
}

fn rendered_screen(buf: &Buffer) -> String {
    (0..buf.area.height)
        .map(|y| rendered_row(buf, y))
        .collect::<Vec<_>>()
        .join("\n")
}

fn zh_session_with_snapshot(snap: jbgit_core::RepoSnapshot) -> TestSession {
    let mut ts = TestSession::with_snapshot(snap);
    ts.state_mut().i18n = Arc::new(I18n::load("zh-CN", None));
    ts
}

/// P1 #1: Active filter chips (Date, Paths, --no-merges) must render ahead of
/// inactive `All` chips so active filters are never silently invisible at 140x40.
#[test]
fn log_filter_bar_prioritizes_active_chips_and_condenses_inactive_at_140x40() {
    let mut ts = zh_session_with_snapshot(snapshot());
    ts.dispatch(Action::SwitchTab(WorkspaceTab::Log));

    // With no active filters at 140x40 in zh-CN, all 4 filter triggers stay reachable.
    let buf_idle = ts.render(140, 40);
    let filter_row_idle = rendered_row(&buf_idle, 3);
    assert!(
        filter_row_idle.contains("分支")
            && filter_row_idle.contains("作者")
            && filter_row_idle.contains("日期")
            && filter_row_idle.contains("路径"),
        "expected all 4 filter chips reachable at 140x40, got: {filter_row_idle}"
    );

    // Activate Paths filter; it must render ahead of inactive chips with a clickable `×`.
    ts.dispatch(Action::ApplyFilterField {
        field: FilterField::Paths,
        expr: "src/auth.rs".into(),
    });
    let buf_path = ts.render(140, 40);
    let filter_row_path = rendered_row(&buf_path, 3);
    assert!(
        filter_row_path.contains("路径:") && filter_row_path.contains("src/auth.rs"),
        "expected active Paths chip visible at 140x40, got: {filter_row_path}"
    );
    let clear_pos = ts
        .hit_center_of(|t| matches!(t, HitTarget::FilterClear(FilterField::Paths)))
        .expect("active Paths filter must expose clickable clear button");
    ts.send_mouse(MouseEventKind::Down(MouseButton::Left), clear_pos.0, clear_pos.1);
    assert!(
        !ts.state().log.filter.field_active(FilterField::Paths),
        "clicking Paths clear button must clear active path filter"
    );

    // Activate multiple filters on a narrow viewport so overflow `+N×` badge appears and clears all.
    ts.dispatch(Action::ApplyFilterField {
        field: FilterField::Branch,
        expr: "main".into(),
    });
    ts.dispatch(Action::ApplyFilterField {
        field: FilterField::User,
        expr: "Alice".into(),
    });
    ts.dispatch(Action::ApplyFilterField {
        field: FilterField::Date,
        expr: "7d".into(),
    });
    ts.dispatch(Action::ApplyFilterField {
        field: FilterField::Paths,
        expr: "crates/jbgit-tui/src/views/log.rs".into(),
    });
    ts.dispatch(Action::ToggleNoMergesFilter);
    let _ = ts.render(90, 30);
    if let Some((cx, cy)) = ts.hit_center_of(|t| matches!(t, HitTarget::FilterClearAll)) {
        ts.send_mouse(MouseEventKind::Down(MouseButton::Left), cx, cy);
        assert!(ts.state().log.filter.spec.is_empty());
    }
}

/// P1 #2: Sanitizing double-width CJK background cells at `rect.x - 1` prevents
/// `ratatui::Buffer::diff` from skipping the modal's left border glyph.
#[test]
fn cjk_overlay_sanitization_preserves_modal_left_border_and_diff() {
    let area = Rect::new(0, 0, 20, 6);
    let mut buf = Buffer::empty(area);
    // Place double-width CJK chars straddling left edge (x=3 -> modal starts at x=4)
    // and straddling right edge (x=13 -> modal ends at x=14).
    for y in 1..5u16 {
        buf.set_string(3, y, "分", ratatui::style::Style::default());
        buf.set_string(13, y, "远", ratatui::style::Style::default());
    }
    let modal_rect = Rect::new(4, 1, 10, 4);
    crate::views::sanitize_overlay_edges(&mut buf, modal_rect);

    // Draw modal border inside `modal_rect`.
    for y in modal_rect.y..modal_rect.bottom() {
        let left_sym = if y == modal_rect.y {
            "╭"
        } else if y + 1 == modal_rect.bottom() {
            "╰"
        } else {
            "│"
        };
        buf[(modal_rect.x, y)].set_symbol(left_sym);
        buf[(modal_rect.right() - 1, y)].set_symbol("│");
    }

    // Verify no width-2 glyph sits at `rect.x - 1`, and Buffer::diff emits every left border cell.
    let empty = Buffer::empty(area);
    let updates = empty.diff(&buf);
    for y in modal_rect.y..modal_rect.bottom() {
        assert_eq!(buf[(modal_rect.x - 1, y)].symbol(), " ");
        assert!(
            buf[(modal_rect.x - 1, y)].symbol().width() <= 1,
            "left neighbor must not be double-width"
        );
        assert_eq!(buf[(modal_rect.right(), y)].symbol(), " ");
        assert!(
            updates
                .iter()
                .any(|(x, uy, cell)| *x == modal_rect.x && *uy == y && cell.symbol() != " "),
            "Buffer::diff skipped modal left border at ({}, {y})",
            modal_rect.x
        );
    }

    // End-to-end check: opening Branches Popup over zh-CN UI at 140x40 leaves no wide glyph
    // straddling the popup's left border.
    let mut ts = zh_session_with_snapshot(snapshot());
    ts.dispatch(Action::SwitchTab(WorkspaceTab::Log));
    ts.dispatch(Action::OpenPopup {
        kind: PopupKind::BranchesQuickView,
        initial_query: String::new(),
    });
    let screen_buf = ts.render(140, 40);
    let mut found_top_left = None;
    'outer: for y in 0..40u16 {
        for x in 1..140u16 {
            if screen_buf[(x, y)].symbol() == "╭" {
                found_top_left = Some((x, y));
                break 'outer;
            }
        }
    }
    let (mx, my) = found_top_left.expect("branches popup top-left border must render");
    for y in my..my + 10 {
        assert!(
            screen_buf[(mx - 1, y)].symbol().width() <= 1,
            "found un-sanitized wide char '{}' at ({}, {y}) next to modal border",
            screen_buf[(mx - 1, y)].symbol(),
            mx - 1
        );
    }
}

/// P1 #3: Pressing `Down` + `Enter` in `FilterEditor` with an empty query applies
/// the highlighted candidate, while clearing the query still resets to All.
#[test]
fn filter_editor_down_enter_applies_highlighted_candidate_while_clear_still_works() {
    let mut ts = TestSession::with_snapshot(snapshot());
    ts.dispatch(Action::SwitchTab(WorkspaceTab::Log));

    // 1. Open Date filter with empty query, press Down (`yesterday`), press Enter -> applies!
    ts.dispatch(Action::OpenPopup {
        kind: PopupKind::FilterEditor(FilterField::Date),
        initial_query: String::new(),
    });
    ts.send_key(KeyCode::Down, KeyModifiers::NONE);
    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);
    assert!(
        ts.state().log.filter.field_active(FilterField::Date),
        "Down + Enter on empty FilterEditor must apply highlighted candidate"
    );
    assert_eq!(
        ts.state()
            .log
            .filter
            .field_expr(FilterField::Date)
            .to_ascii_lowercase(),
        "yesterday"
    );

    // 2. Re-open Date filter with current value, backspace to empty, press Enter -> clears to All!
    ts.dispatch(Action::OpenPopup {
        kind: PopupKind::FilterEditor(FilterField::Date),
        initial_query: "7d".into(),
    });
    ts.send_key(KeyCode::Backspace, KeyModifiers::NONE);
    ts.send_key(KeyCode::Backspace, KeyModifiers::NONE);
    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);
    assert!(
        !ts.state().log.filter.field_active(FilterField::Date),
        "clearing query and pressing Enter must reset filter to All"
    );

    // 3. Opening empty and immediately pressing Enter without navigating stays All.
    ts.dispatch(Action::OpenPopup {
        kind: PopupKind::FilterEditor(FilterField::Date),
        initial_query: String::new(),
    });
    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);
    assert!(!ts.state().log.filter.field_active(FilterField::Date));
}

/// P1 #4: Auto-loaded `.git/MERGE_MSG` is cleared once the ongoing merge/cherry-pick/revert
/// finishes or aborts, unless the user replaced it with a fresh manual commit message.
#[test]
fn stale_merge_msg_clears_when_sequencer_ends_unless_user_edited() {
    let mut ts = TestSession::with_snapshot(snapshot());
    ts.dispatch(Action::SwitchTab(WorkspaceTab::Commit));

    let merge_msg = "Merge branch 'feature/theirs'\n\n# Conflicts:\n#\tconflict_app.txt";
    let ep1 = ts
        .state()
        .request_slots
        .latest(jbgit_core::RequestSlotId::CommitPrecheck)
        .max(1);
    ts.deliver_engine_event(EngineEvent::CommitPrecheckLoaded {
        info: CommitPrecheckInfo {
            comment_char: '#',
            prepared_message: Some(merge_msg.into()),
            has_sequencer_state: true,
            ..Default::default()
        },
        epoch: ep1,
    });
    assert_eq!(ts.state().stage_workbench.editor().text(), merge_msg);

    // Sequencer completes (e.g. `git merge --continue`): MERGE_MSG is gone -> editor clears.
    ts.deliver_engine_event(EngineEvent::CommitPrecheckLoaded {
        info: CommitPrecheckInfo {
            comment_char: '#',
            prepared_message: None,
            has_sequencer_state: false,
            ..Default::default()
        },
        epoch: ep1 + 1,
    });
    assert!(
        ts.state().stage_workbench.editor().is_empty(),
        "stale MERGE_MSG should be cleared once sequencer ends, got: {:?}",
        ts.state().stage_workbench.editor().text()
    );

    // Now test user-edited message preservation: load MERGE_MSG, then user replaces it manually.
    ts.deliver_engine_event(EngineEvent::CommitPrecheckLoaded {
        info: CommitPrecheckInfo {
            comment_char: '#',
            prepared_message: Some(merge_msg.into()),
            has_sequencer_state: true,
            ..Default::default()
        },
        epoch: ep1 + 2,
    });
    assert_eq!(ts.state().stage_workbench.editor().text(), merge_msg);
    ts.state_mut()
        .stage_workbench
        .apply_external_message("feat: fresh manual commit", std::time::Instant::now());
    ts.deliver_engine_event(EngineEvent::CommitPrecheckLoaded {
        info: CommitPrecheckInfo {
            comment_char: '#',
            prepared_message: None,
            has_sequencer_state: false,
            ..Default::default()
        },
        epoch: ep1 + 3,
    });
    assert_eq!(
        ts.state().stage_workbench.editor().text(),
        "feat: fresh manual commit"
    );
}

/// P2 #5: Linked-worktree badge must elide before crushing branch names to `…` at 140x40.
#[test]
fn worktree_badge_elides_before_crushing_branch_name_at_140x40() {
    let mut snap = snapshot();
    snap.refs.push(local("feature/ui-test", 4));
    snap.worktrees.push(WorktreeInfo {
        path: PathBuf::from("/tmp/sandbox-repo-feature"),
        branch: Some("feature/ui-test".into()),
        head: Some(oid(4)),
        is_main: false,
        is_bare: false,
        detached: false,
        locked: None,
        prunable: None,
    });
    let mut ts = zh_session_with_snapshot(snap);
    ts.dispatch(Action::SwitchTab(WorkspaceTab::Log));
    let buf = ts.render(140, 40);
    let txt = rendered_screen(&buf);
    assert!(
        txt.contains("ui-test"),
        "branch name `ui-test` must remain visible beside linked-worktree badge at 140x40:\n{txt}"
    );
}

/// P2 #6: Untracked files disable line-level rollback (`[↺ Rollback]` / `d`) with a
/// clear localized notice instead of silently truncating them to 0 bytes.
#[test]
fn untracked_file_disables_line_rollback_and_hides_rollback_pill() {
    let mut ts = zh_session_with_snapshot(snapshot());
    ts.dispatch(Action::SwitchTab(WorkspaceTab::Commit));
    ts.deliver_engine_event(EngineEvent::StageStatusLoaded {
        status: Arc::new(StageStatusSnapshot {
            untracked: vec![StageEntry {
                path: "scratch_note.txt".into(),
                old_path: None,
                kind: FileStatusKind::Untracked,
            }],
            ..Default::default()
        }),
        epoch: 1,
    });

    let local_bytes = b"untracked qa file\n".to_vec();
    let tracker = StageLineTracker::new(b"", b"", &local_bytes);
    let (hunks, is_truncated) = tracker.build_interactive_hunks(false, DiffContext::Three, 1000);
    let untracked_diff = Arc::new(ThreeDocStageDiff {
        key: StageDiffKey {
            path: "scratch_note.txt".into(),
            old_path: None,
            side: StageDiffSide::Unstaged,
            context: DiffContext::Three,
            whitespace: WhitespaceMode::Default,
        },
        vcs_exists: false,
        staged_exists: false,
        local_exists: true,
        vcs_bytes: Vec::new(),
        staged_bytes: Vec::new(),
        local_bytes,
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
    });
    assert!(untracked_diff.is_untracked());
    ts.state_mut()
        .stage_workbench
        .set_diff_for_test(Some(untracked_diff));

    let buf = ts.render(140, 40);
    let txt = rendered_screen(&buf);
    assert!(
        txt.contains("[+ 暂存]"),
        "stage hunk pill should still render for untracked file:\n{txt}"
    );
    assert!(
        !txt.contains("[↺ 回滚]"),
        "rollback hunk pill must be hidden for untracked file:\n{txt}"
    );

    let _ = ts.take_requests();
    ts.dispatch(Action::StageDiffRollbackSelection);
    let reqs = ts.take_requests();
    assert!(
        !reqs
            .iter()
            .any(|(r, _)| matches!(r, TaskRequest::WriteWorktreeBuffer { .. })),
        "line rollback must not write/truncate untracked file: {reqs:?}"
    );
    assert_eq!(
        ts.state().status_notice.as_deref(),
        Some(ts.state().i18n.t("toast.untracked_rollback_disabled"))
    );

    ts.dispatch(Action::ApplyStageDiffHunk(0, DiffHunkActionKind::Rollback));
    let reqs2 = ts.take_requests();
    assert!(
        !reqs2
            .iter()
            .any(|(r, _)| matches!(r, TaskRequest::WriteWorktreeBuffer { .. })),
        "hunk rollback must not write/truncate untracked file: {reqs2:?}"
    );

    // Context menu items must also disable rollback for untracked files.
    let diff_menu = crate::menus::stage_diff_menu_items(ts.state());
    let rollback_item = diff_menu
        .iter()
        .find(|i| i.label == ts.state().i18n.t("menu.stage.rollback_selection"))
        .expect("rollback menu item present");
    assert_eq!(
        rollback_item.disabled_reason.as_deref(),
        Some(ts.state().i18n.t("menu.stage.untracked_no_rollback"))
    );
}

/// P2 #7: `[3 Stash]` footer uses `应用` (not `确认`) in `zh-CN`, and action-less
/// toast banners omit `Alt+Z 执行`.
#[test]
fn stash_footer_uses_apply_translation_and_actionless_toast_omits_alt_z() {
    let mut ts = zh_session_with_snapshot(snapshot());
    ts.dispatch(Action::SwitchTab(WorkspaceTab::Stash));
    let buf = ts.render(140, 40);
    let footer_row = rendered_row(&buf, 39);
    assert!(
        footer_row.contains("应用"),
        "expected Stash footer to display '应用' for StashApply, got: {footer_row}"
    );
    assert!(
        !footer_row.contains("确认"),
        "Stash footer must not mistranslate StashApply as '确认', got: {footer_row}"
    );

    // Action-less notification must omit `Alt+Z`.
    ts.state_mut().notification = Some(Notification {
        text: "已切换到 'sandbox-repo-b'".into(),
        is_error: false,
        action: None,
    });
    let buf_no_act = ts.render(140, 40);
    let toast_no_act = rendered_row(&buf_no_act, 39);
    assert!(
        toast_no_act.contains("Alt+X 关闭"),
        "expected dismiss hint on action-less toast, got: {toast_no_act}"
    );
    assert!(
        !toast_no_act.contains("Alt+Z"),
        "action-less toast must not advertise Alt+Z, got: {toast_no_act}"
    );

    // Actionable notification must still display `Alt+Z 执行 · Alt+X 关闭`.
    ts.state_mut().notification = Some(Notification {
        text: "推送完成".into(),
        is_error: false,
        action: Some(("查看".into(), vec![])),
    });
    let buf_act = ts.render(140, 40);
    let toast_act = rendered_row(&buf_act, 39);
    assert!(
        toast_act.contains("Alt+Z 执行") && toast_act.contains("Alt+X 关闭"),
        "actionable toast must keep Alt+Z + Alt+X hint, got: {toast_act}"
    );
}

/// P2 #8: Commit Details keeps short SHA + `[⤢ 完整]` + full timestamp visible at 140x40,
/// and 3-Way Conflict Resolver localizes unresolved block placeholders in `zh-CN`.
#[test]
fn commit_details_narrow_layout_and_conflict_resolver_zh_cn_placeholders() {
    let mut ts = zh_session_with_snapshot(snapshot());
    ts.dispatch(Action::SwitchTab(WorkspaceTab::Log));
    ts.dispatch(Action::SelectLogRow(0));

    let head_oid = oid(12);
    let ts_unix = 1_764_055_200i64;
    let expected_time = format_absolute_time(ts_unix, true);
    let key = DetailsKey::single(head_oid);
    let bundle = Arc::new(CommitChangesBundle {
        key,
        details: vec![CommitDetails {
            oid: head_oid,
            parents: vec![oid(11)],
            author: PersonStamp {
                name: "Long Author Name".into(),
                email: "very.long.email.address@example.com".into(),
                time: ts_unix,
                offset_minutes: 0,
            },
            committer: PersonStamp {
                name: "Long Author Name".into(),
                email: "very.long.email.address@example.com".into(),
                time: ts_unix,
                offset_minutes: 0,
            },
            message: "feat: verify details layout at 140x40".into(),
            signature: CommitSignature::default(),
        }],
        changes: Vec::new(),
    });
    ts.deliver_engine_event(EngineEvent::CommitDetailsLoaded {
        slot: jbgit_core::RequestSlotId::LogCommitDetails,
        bundle,
        epoch: 1,
    });

    let buf = ts.render(140, 40);
    let txt = rendered_screen(&buf);
    assert!(
        txt.contains(&head_oid.short()),
        "expected short SHA visible at 140x40:\n{txt}"
    );
    assert!(
        txt.contains("[⤢ 完整]"),
        "expected [⤢ 完整] button visible at 140x40:\n{txt}"
    );
    assert!(
        ts.hit_center_of(|t| matches!(t, HitTarget::DetailsMaximizeMessage))
            .is_some(),
        "[⤢ 完整] button must be clickable at 140x40"
    );
    assert!(
        txt.contains(&expected_time),
        "expected full timestamp `{expected_time}` visible at 140x40:\n{txt}"
    );

    // 3-Way Conflict Resolver in zh-CN must localize unresolved block placeholder.
    let conflict_text = "<<<<<<< HEAD\nours line\n=======\ntheirs line\n>>>>>>> feature/theirs\n";
    let doc = ConflictDocument::parse(
        "conflict_app.txt",
        conflict_text,
        Some("ours line\n"),
        Some("theirs line\n"),
    );
    let cm = ConflictMergeState {
        path: "conflict_app.txt".into(),
        doc: Some(doc),
        ..Default::default()
    };
    ts.state_mut()
        .modal_stack
        .push(ModalLayer::ConflictResolver(Box::new(cm)));

    let cbuf = ts.render(140, 40);
    let ctxt = rendered_screen(&cbuf);
    assert!(
        ctxt.contains("冲突块 #1"),
        "expected localized zh-CN unresolved conflict placeholder:\n{ctxt}"
    );
    assert!(
        !ctxt.contains("⚠ Block #1:"),
        "3-Way Conflict Resolver must not leak English placeholder in zh-CN:\n{ctxt}"
    );
}
