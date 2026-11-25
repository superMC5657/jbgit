use std::path::PathBuf;
use std::sync::Arc;

use crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEventKind};
use jbgit_core::{
    CommitChangesBundle, CommitDetails, CommitSignature, ConflictDocument, DetailsKey, DiffContext,
    FileStatusKind, PersonStamp, StageEntry, StageLineTracker, StageStatusSnapshot, WhitespaceMode,
    WorkspaceTab, WorktreeInfo,
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
    ts.send_mouse(
        MouseEventKind::Down(MouseButton::Left),
        clear_pos.0,
        clear_pos.1,
    );
    assert!(
        !ts.state().log.filter.field_active(FilterField::Paths),
        "clicking Paths clear button must clear active path filter"
    );

    // P2-06a: Activate a long Branch chip at 140x40; `路径▾` and `[排除合并]` must stay
    // visible AND register clickable HitTargets.
    ts.dispatch(Action::ApplyFilterField {
        field: FilterField::Branch,
        expr: "feature/linear-chain".into(),
    });
    let buf_branch = ts.render(140, 40);
    let filter_row_branch = rendered_row(&buf_branch, 3);
    assert!(
        filter_row_branch.contains("分支:")
            && filter_row_branch.contains("路径")
            && filter_row_branch.contains("排除合并"),
        "long active Branch chip at 140x40 must not evict `路径▾` or `[排除合并]`, got: {filter_row_branch}"
    );
    assert!(
        ts.hit_center_of(|t| matches!(t, HitTarget::FilterChip(FilterField::Paths)))
            .is_some(),
        "`路径▾` chip must have a registered HitTarget at 140x40"
    );
    assert!(
        ts.hit_center_of(|t| matches!(t, HitTarget::FilterNoMergesToggle))
            .is_some(),
        "`[排除合并]` chip must have a registered HitTarget at 140x40"
    );
    ts.dispatch(Action::ClearFilterField(FilterField::Branch));

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

/// P2-06b: `FilterEditor` (User/Branch/Date/Paths), `GoToRevision`, and `CommitHistory`
/// must clear their query on `Ctrl+u`.
#[test]
fn filter_editor_and_revision_popups_clear_query_on_ctrl_u() {
    let mut ts = zh_session_with_snapshot(snapshot());
    ts.dispatch(Action::SwitchTab(WorkspaceTab::Log));

    for kind in [
        PopupKind::FilterEditor(FilterField::User),
        PopupKind::FilterEditor(FilterField::Branch),
        PopupKind::FilterEditor(FilterField::Date),
        PopupKind::FilterEditor(FilterField::Paths),
        PopupKind::GoToRevision,
        PopupKind::CommitHistory,
    ] {
        ts.dispatch(Action::OpenPopup {
            kind,
            initial_query: String::new(),
        });
        ts.type_str("@me");
        assert_eq!(
            ts.state()
                .modal_stack
                .last()
                .and_then(ModalLayer::popup_query_and_selected)
                .map(|(q, _)| q),
            Some("@me"),
            "query should contain typed text for {kind:?}"
        );
        ts.send_key(KeyCode::Char('u'), KeyModifiers::CONTROL);
        assert_eq!(
            ts.state()
                .modal_stack
                .last()
                .and_then(ModalLayer::popup_query_and_selected)
                .map(|(q, _)| q),
            Some(""),
            "Ctrl+u must clear query for {kind:?}"
        );
        ts.send_key(KeyCode::Esc, KeyModifiers::NONE);
    }
}

/// P2-13: `render_input` sizes dynamically so Worktree `--force` checkbox hint
/// renders intact at 140x40 in both `zh-CN` and `en-US`, and safely truncates
/// only the checkbox label while preserving `({hint})` on a narrow 50x16 terminal.
#[test]
fn render_input_worktree_path_dialog_preserves_checkbox_hint_wide_and_narrow() {
    let ctx = crate::dialogs::WorktreeDialogCtx {
        main: PathBuf::from("/tmp/repo"),
        parent: Some(PathBuf::from("/tmp")),
    };

    for (locale, expected_hint) in [("zh-CN", "(Tab 切换)"), ("en-US", "(Tab)")] {
        let mut ts = TestSession::with_snapshot(snapshot());
        ts.state_mut().i18n = Arc::new(I18n::load(locale, None));
        let dlg = ctx.path_dialog(
            jbgit_core::WorktreeBranch::Existing("feature/worktree-test".into()),
            &ts.state().i18n,
        );
        ts.dispatch(Action::OpenDialog(Box::new(dlg)));

        let wide_buf = ts.render(140, 40);
        let wide_txt = rendered_screen(&wide_buf);
        assert!(
            wide_txt.contains(expected_hint),
            "[{locale}] 140x40 must show complete `{expected_hint}` without truncation:\n{wide_txt}"
        );
        assert!(
            wide_txt.contains("--force"),
            "[{locale}] 140x40 must show complete `--force` checkbox text:\n{wide_txt}"
        );

        let narrow_buf = ts.render(50, 16);
        let narrow_txt = rendered_screen(&narrow_buf);
        assert!(
            narrow_txt.contains(expected_hint),
            "[{locale}] 50x16 narrow screen must preserve `{expected_hint}`:\n{narrow_txt}"
        );
    }
}

/// P3-01: `IdentityDialog` defers red `✕` validation error until the user attempts to confirm.
#[test]
fn identity_dialog_defers_validation_error_until_confirm() {
    let mut ts = zh_session_with_snapshot(snapshot());
    let dlg =
        crate::dialogs::Dialog::Identity(crate::dialogs::IdentityDialog::new(None, None, None));
    ts.dispatch(Action::OpenDialog(Box::new(dlg)));

    // Initial render must not flash red `✕` validation error.
    let buf_initial = ts.render(140, 40);
    let txt_initial = rendered_screen(&buf_initial);
    assert!(
        !txt_initial.contains('✕'),
        "IdentityDialog must not display `✕` error before submit:\n{txt_initial}"
    );

    // Pressing Enter on empty form reveals the validation error.
    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);
    let buf_after = ts.render(140, 40);
    let txt_after = rendered_screen(&buf_after);
    assert!(
        txt_after.contains('✕') && txt_after.contains("用户名不能为空"),
        "IdentityDialog must display `✕` validation error after submitting empty form:\n{txt_after}"
    );
}

/// P3-02: Non-Git Welcome screen suppresses background `加载中…` / `正在加载提交图谱…`
/// placeholders, and `CloneFinished(Err)` deduplicates error lines already streamed
/// into `progress_lines`.
#[test]
fn welcome_suppresses_loading_placeholders_and_clone_err_dedups_progress() {
    let tmp = tempfile::TempDir::new().unwrap();
    let empty_dir = tmp.path().join("no_git_here");
    std::fs::create_dir_all(&empty_dir).unwrap();

    let (_repo, mut session) = crate::session::init_engine_and_session(empty_dir, None);
    session.state_mut().i18n = Arc::new(I18n::load("zh-CN", None));
    assert!(session.state().is_unattached_workspace());

    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(140, 40)).unwrap();
    terminal.draw(|f| session.render(f)).unwrap();
    let screen = rendered_screen(terminal.backend().buffer());
    assert!(
        !screen.contains("加载中…"),
        "Welcome background header must not show `加载中…`:\n{screen}"
    );
    assert!(
        !screen.contains("正在加载提交图谱…"),
        "Welcome background log pane must not show `正在加载提交图谱…`:\n{screen}"
    );
    assert!(
        screen.contains("无仓库"),
        "Welcome background header should show `无仓库`:\n{screen}"
    );

    // CloneFinished(Err) deduplicates matching error lines from progress_lines.
    let mut ts = zh_session_with_snapshot(snapshot());
    ts.dispatch(Action::OpenCloneRepo { base_dir: None });
    let err_line = "fatal: could not create work tree dir 'dest': Invalid argument";
    ts.deliver_ui_async(crate::app::UiAsyncEvent::CloneProgress(
        "Cloning into 'dest'...".into(),
    ));
    ts.deliver_ui_async(crate::app::UiAsyncEvent::CloneProgress(err_line.into()));
    ts.deliver_ui_async(crate::app::UiAsyncEvent::CloneFinished(
        Err(err_line.into()),
    ));

    match ts.state().modal_stack.last() {
        Some(ModalLayer::CloneRepo(cd)) => {
            assert_eq!(cd.progress_lines, vec!["Cloning into 'dest'..."]);
            assert_eq!(cd.error.as_deref(), Some(err_line));
        }
        other => panic!("expected CloneRepo modal, got {other:?}"),
    }

    let clone_buf = ts.render(120, 32);
    // Exclude row 0 (header status_notice toast) so we count occurrences inside the Clone modal.
    let modal_body = (1..32u16)
        .map(|y| rendered_row(&clone_buf, y))
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(
        modal_body.matches("could not create work tree dir").count(),
        1,
        "Clone modal must render the fatal error line only once:\n{modal_body}"
    );
    assert!(
        modal_body.contains("Cloning into 'dest'..."),
        "Non-duplicate progress lines should remain visible:\n{modal_body}"
    );
}

/// P3-03: Bisect banner buttons have symmetric spaces inside `[ ... ]`, and Console
/// failed command rows separate exit code from command and never clip trailing `ms`.
#[test]
fn bisect_banner_symmetric_spacing_and_console_failed_row_layout() {
    let mut snap = snapshot();
    snap.state = jbgit_core::RepositoryState::Bisect;
    snap.bisect = Some(jbgit_core::BisectInfo {
        remaining_steps: Some(2),
        remaining_revisions: Some(4),
    });
    let mut ts = zh_session_with_snapshot(snap);
    ts.dispatch(Action::SwitchTab(WorkspaceTab::Log));

    let buf = ts.render(140, 40);
    let screen = rendered_screen(&buf);
    assert!(
        screen.contains("[ Good Alt+Shift+G ]"),
        "Bisect Good button must have symmetric inner spaces:\n{screen}"
    );
    assert!(
        screen.contains("[ 跳过 Alt+Shift+S ]"),
        "Bisect Skip button must have symmetric inner spaces:\n{screen}"
    );

    // Console failed command row: space between `✗2` and `git ...` and intact `53ms` suffix.
    ts.dispatch(Action::SwitchTab(WorkspaceTab::Console));
    ts.state_mut()
        .console
        .entries_mut_for_test()
        .push(jbgit_core::GitConsoleEntry {
            id: 42,
            started_at: chrono::Local::now(),
            duration_ms: Some(53),
            command_line:
                "git -c credential.helper= clone --progress https://example.com/very/long/repository/path/that/must/truncate.git target_dir"
                    .into(),
            output_lines: Vec::new(),
            exit_code: Some(2),
        });
    let cbuf = ts.render(100, 30);
    let ctxt = rendered_screen(&cbuf);
    assert!(
        ctxt.contains("✗2 git "),
        "Console failed row must have a space between exit code and command:\n{ctxt}"
    );
    assert!(
        ctxt.contains(" 53ms"),
        "Console failed row must preserve full `53ms` duration without clipping `s`:\n{ctxt}"
    );
}

/// P3-04a: `InputDialog` (and sibling single-line dialogs) support `Ctrl+a` select-all:
/// highlight on render, clear on `Backspace`/`Delete`, and replace on typing/paste.
#[test]
fn input_dialog_ctrl_a_select_all_clears_and_replaces() {
    let mut ts = zh_session_with_snapshot(snapshot());
    let make_input = || {
        crate::dialogs::Dialog::Input(crate::dialogs::InputDialog::new(
            "Rename",
            "Name:",
            "initial-value",
            crate::dialogs::InputValidation::NonEmpty,
            crate::dialogs::InputIntent::CheckoutRevision,
        ))
    };

    // 1. Ctrl+a + Backspace clears all content, and rendering highlights selection beforehand.
    ts.dispatch(Action::OpenDialog(Box::new(make_input())));
    ts.send_key(KeyCode::Char('a'), KeyModifiers::CONTROL);
    let sel_buf = ts.render(100, 30);
    let mut found_highlight = false;
    let sel_bg = ts.state().theme.bg_selection_focused;
    for y in 0..30u16 {
        for x in 0..100u16 {
            if sel_buf[(x, y)].symbol() == "i" && sel_buf[(x, y)].bg == sel_bg {
                found_highlight = true;
            }
        }
    }
    assert!(
        found_highlight,
        "Ctrl+a in InputDialog must highlight selected text with bg_selection_focused"
    );
    ts.send_key(KeyCode::Backspace, KeyModifiers::NONE);
    match ts.state().modal_stack.last() {
        Some(ModalLayer::Dialog(d)) => match d.as_ref() {
            crate::dialogs::Dialog::Input(inp) => {
                assert_eq!(inp.value, "");
                assert!(!inp.selected_all);
            }
            other => panic!("expected Input dialog, got {other:?}"),
        },
        other => panic!("expected Dialog layer, got {other:?}"),
    }
    ts.send_key(KeyCode::Esc, KeyModifiers::NONE);

    // 2. Ctrl+a + typing replaces full content.
    ts.dispatch(Action::OpenDialog(Box::new(make_input())));
    ts.send_key(KeyCode::Char('a'), KeyModifiers::CONTROL);
    ts.type_str("fresh");
    match ts.state().modal_stack.last() {
        Some(ModalLayer::Dialog(d)) => match d.as_ref() {
            crate::dialogs::Dialog::Input(inp) => {
                assert_eq!(inp.value, "fresh");
                assert!(!inp.selected_all);
            }
            other => panic!("expected Input dialog, got {other:?}"),
        },
        other => panic!("expected Dialog layer, got {other:?}"),
    }
    ts.send_key(KeyCode::Esc, KeyModifiers::NONE);

    // 3. Ctrl+a + Paste replaces full content.
    ts.dispatch(Action::OpenDialog(Box::new(make_input())));
    ts.send_key(KeyCode::Char('a'), KeyModifiers::CONTROL);
    ts.send_event(crossterm::event::Event::Paste("pasted-name".into()));
    match ts.state().modal_stack.last() {
        Some(ModalLayer::Dialog(d)) => match d.as_ref() {
            crate::dialogs::Dialog::Input(inp) => {
                assert_eq!(inp.value, "pasted-name");
                assert!(!inp.selected_all);
            }
            other => panic!("expected Input dialog, got {other:?}"),
        },
        other => panic!("expected Dialog layer, got {other:?}"),
    }
}

/// P3-04c: Rolling back a whole hunk (non-visual `d` or hunk button) shows
/// `toast.rolled_back_hunk` (`选中代码块`), while visual line selection rollback shows
/// `toast.rolled_back_lines` (`选中行`).
#[test]
fn stage_diff_rollback_toast_distinguishes_hunk_from_visual_line_selection() {
    let build_diff = || {
        let base = b"line 1\nline 2\nline 3\n";
        let local = b"line 1\nline 2 edited\nline 3 edited\n";
        let tracker = StageLineTracker::new(base, base, local);
        let (hunks, is_truncated) =
            tracker.build_interactive_hunks(false, DiffContext::Three, 1000);
        Arc::new(ThreeDocStageDiff {
            key: StageDiffKey {
                path: "src/app.rs".into(),
                old_path: None,
                side: StageDiffSide::Unstaged,
                context: DiffContext::Three,
                whitespace: WhitespaceMode::Default,
            },
            vcs_exists: true,
            staged_exists: true,
            local_exists: true,
            vcs_bytes: base.to_vec(),
            staged_bytes: base.to_vec(),
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
        })
    };

    // 1. Non-visual mode rollback (`d` -> StageDiffRollbackSelection) -> whole hunk toast.
    let mut ts = zh_session_with_snapshot(snapshot());
    ts.dispatch(Action::SwitchTab(WorkspaceTab::Commit));
    ts.state_mut()
        .stage_workbench
        .set_diff_for_test(Some(build_diff()));
    ts.dispatch(Action::StageDiffRollbackSelection);
    let hunk_notice = ts.state().status_notice.clone().unwrap_or_default();
    assert!(
        hunk_notice.contains("选中代码块"),
        "non-visual rollback must use hunk wording `选中代码块`, got: {hunk_notice:?}"
    );

    // 2. Visual mode line selection rollback -> selected lines toast.
    ts.state_mut()
        .stage_workbench
        .set_diff_for_test(Some(build_diff()));
    // Move cursor onto a changed line inside the hunk, toggle visual mode, and rollback.
    ts.dispatch(Action::StageDiffMoveCursor(2));
    ts.dispatch(Action::StageDiffToggleVisual);
    ts.dispatch(Action::StageDiffRollbackSelection);
    let lines_notice = ts.state().status_notice.clone().unwrap_or_default();
    assert!(
        lines_notice.contains("选中行"),
        "visual selection rollback must use line wording `选中行`, got: {lines_notice:?}"
    );
}

/// Code-review regression: active filter chips with 1-2 col labels (`v1`, `a`, `王`)
/// must NOT vanish into `+1×` on normal/wide screens; idle `en-US` at 140x40 must keep
/// `[No Merges]` visible; and `⚙` Presentation menu must expose `ToggleNoMergesFilter`.
#[test]
fn log_filter_bar_renders_short_active_chips_en_us_idle_no_merges_and_gear_menu_entry() {
    for short_branch in ["a", "v1", "王"] {
        let mut ts = zh_session_with_snapshot(snapshot());
        ts.dispatch(Action::SwitchTab(WorkspaceTab::Log));
        ts.dispatch(Action::ApplyFilterField {
            field: FilterField::Branch,
            expr: short_branch.into(),
        });
        let buf = ts.render(140, 40);
        let row3 = rendered_row(&buf, 3);
        assert!(
            row3.contains(short_branch) && !row3.contains("+1×"),
            "1-2 col active branch `{short_branch}` must render directly (not `+1×`), row3={row3:?}"
        );
    }

    // Idle en-US at 140x40 must keep `[No Merges]` visible and clickable.
    let mut ts_en = TestSession::with_snapshot(snapshot());
    ts_en.state_mut().i18n = Arc::new(I18n::load("en-US", None));
    ts_en.dispatch(Action::SwitchTab(WorkspaceTab::Log));
    let buf_en = ts_en.render(140, 40);
    let row3_en = rendered_row(&buf_en, 3);
    assert!(
        row3_en.contains("No Merges"),
        "idle en-US 140x40 filter bar must keep `No Merges` visible, row3={row3_en:?}"
    );
    assert!(
        ts_en
            .hit_center_of(|t| matches!(t, HitTarget::FilterNoMergesToggle))
            .is_some(),
        "idle en-US 140x40 must register FilterNoMergesToggle hit target"
    );

    // Narrow 80x24 fallback: `⚙` Presentation menu must contain `ToggleNoMergesFilter`.
    let pres_items = crate::input::presentation_menu_items(ts_en.state());
    assert!(
        pres_items.iter().any(|item| matches!(
            &item.kind,
            crate::app::ContextMenuEntryKind::Action(actions)
                if actions.iter().any(|a| matches!(a, Action::ToggleNoMergesFilter))
        )),
        "`⚙` presentation menu must include Action::ToggleNoMergesFilter for narrow viewports"
    );
}

/// Code-review regression (P1-02): viewing a shallow boundary commit BEFORE `Unshallow`
/// caches `parents: []` in `diff_caches.details`; when `RepoSnapshotLoaded` flips
/// `is_shallow: true -> false`, cached details must be evicted and immediately re-requested.
#[test]
fn unshallow_snapshot_evicts_cached_commit_details_and_rerequests() {
    let mut shallow_snap = snapshot();
    shallow_snap.is_shallow = true;
    let boundary_oid = shallow_snap.commits[0].oid;
    let details_key = DetailsKey::commits(vec![boundary_oid], 0);
    let stamp = PersonStamp {
        name: "Dev".into(),
        email: "dev@example.com".into(),
        time: 1_700_000_000,
        offset_minutes: 0,
    };

    let mut ts = zh_session_with_snapshot(shallow_snap.clone());
    ts.state_mut()
        .diff_caches
        .insert_details(Arc::new(CommitChangesBundle {
            key: details_key.clone(),
            details: vec![CommitDetails {
                oid: boundary_oid,
                parents: Vec::new(),
                author: stamp.clone(),
                committer: stamp,
                message: "boundary".into(),
                signature: CommitSignature::default(),
            }],
            changes: Vec::new(),
        }));
    ts.state_mut().log.details.changes.current_key = Some(details_key.clone());
    assert!(ts.state().diff_caches.peek_details(&details_key).is_some());

    let mut full_snap = shallow_snap;
    full_snap.is_shallow = false;
    let effects = ts
        .state_mut()
        .apply_engine_event(EngineEvent::RepoSnapshotLoaded {
            snapshot: Arc::new(full_snap),
            epoch: 2,
        });

    assert!(
        ts.state().diff_caches.peek_details(&details_key).is_none(),
        "shallow->unshallow transition must evict stale CommitChangesBundle from diff_caches"
    );
    assert!(
        effects.iter().any(|e| matches!(
            e,
            crate::app::Effect::RequestCommitDetails {
                slot: jbgit_core::RequestSlotId::LogCommitDetails,
                key,
            } if *key == details_key
        )),
        "expected immediate RequestCommitDetails for currently viewed boundary commit, got: {effects:?}"
    );
}

/// Code-review regression (P2-03 & CS-6.6): `c -> CopyFilePath` must not leak when
/// Log diff is zoomed, must appear in `?` Help, and must respect explicit `c -> noop`.
#[test]
fn copy_file_path_respects_zoomed_diff_noop_and_appears_in_help() {
    use crate::keymap::help::{HelpRow, build_help_rows};
    use crate::keymap::{KeyAction, actions::route_dynamic_key};

    let mut ts = zh_session_with_snapshot(snapshot());
    ts.dispatch(Action::SwitchTab(WorkspaceTab::Log));
    ts.dispatch(Action::FocusPane(jbgit_core::PaneId::LogChangesTree));

    // Appears in `?` Help under default keymap.
    let rows = build_help_rows(ts.state(), "");
    assert!(
        rows.iter().any(|r| matches!(
            r,
            HelpRow::Entry {
                run: Some(KeyAction::CopyFilePath),
                ..
            }
        )),
        "CopyFilePath must be discoverable in `?` Help / Command Palette"
    );

    // Must not fire when Log diff is zoomed fullscreen.
    ts.state_mut().log.details.diff.zoomed = true;
    let key_c = crossterm::event::KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE);
    assert!(
        route_dynamic_key(&key_c, ts.state()).is_none(),
        "plain `c` must not trigger CopyFilePath while Log diff is zoomed"
    );
    assert!(
        !crate::keymap::catalog::is_action_applicable(&KeyAction::CopyFilePath, ts.state()),
        "CopyFilePath must not be applicable while Log diff is zoomed"
    );

    // Explicit `c -> noop` in `[log]` must disable both routing and context menu hint.
    ts.state_mut().log.details.diff.zoomed = false;
    let noop_binding: crate::config::keymap_config::KeyBindingConfig = toml::from_str(
        r#"on = ["c"]
run = "noop""#,
    )
    .unwrap();
    ts.state_mut()
        .config
        .keymap
        .log
        .key_bindings
        .push(noop_binding);
    assert!(
        !crate::keymap::actions::is_plain_c_copy_file_path_fallback_active(ts.state()),
        "explicit `c -> noop` in [log] must suppress plain `c` CopyFilePath fallback"
    );
    assert!(route_dynamic_key(&key_c, ts.state()).is_none());
}

/// Code-review regression (P2-05): FileHistory scope dropdown must not show `✓` on
/// `Current HEAD` while pinned to an older `start_oid`.
#[test]
fn file_history_scope_menu_unchecks_current_head_when_pinned_to_old_commit() {
    let snap = snapshot();
    let head_oid = snap.commits[0].oid;
    let old_oid = oid(0x99);
    assert_ne!(head_oid, old_oid);

    let mut ts = zh_session_with_snapshot(snap);
    ts.dispatch(Action::OpenFileHistory {
        path: "src/app.rs".into(),
        start_oid: Some(old_oid),
        follow_renames: true,
        line_range: None,
    });
    let items_pinned = crate::menus::file_history_scope_menu_items(ts.state());
    assert!(
        !items_pinned[0].label.starts_with('✓'),
        "pinned old start_oid must NOT check Current HEAD in dropdown, got: {:?}",
        items_pinned[0].label
    );

    ts.dispatch(Action::FileHistorySetBranchScope(
        jbgit_core::HistoryBranchScope::CurrentHead,
    ));
    let items_head = crate::menus::file_history_scope_menu_items(ts.state());
    assert!(
        items_head[0].label.starts_with('✓'),
        "after switching to Current HEAD, dropdown must check Current HEAD, got: {:?}",
        items_head[0].label
    );
}

/// Code-review regression (P3-02 & P3-04a): CloneDialogState supports `Ctrl+a` select-all
/// replacement/clear and renders multi-line `CloneFinished(Err)` without losing `fatal:`.
#[test]
fn clone_dialog_supports_ctrl_a_select_all_and_multiline_error_rendering() {
    let mut ts = zh_session_with_snapshot(snapshot());
    let mut cd = crate::repo_lifecycle::CloneDialogState::new(PathBuf::from("C:/ws"));
    cd.url = "https://example.com/old.git".into();
    ts.state_mut()
        .modal_stack
        .push(ModalLayer::CloneRepo(Box::new(cd)));

    // Ctrl+a then Backspace clears the entire URL field.
    ts.send_key(KeyCode::Char('a'), KeyModifiers::CONTROL);
    match ts.state().modal_stack.last() {
        Some(ModalLayer::CloneRepo(cd)) => assert!(cd.selected_all),
        other => panic!("expected CloneRepo modal, got {other:?}"),
    }
    ts.send_key(KeyCode::Backspace, KeyModifiers::NONE);
    match ts.state().modal_stack.last() {
        Some(ModalLayer::CloneRepo(cd)) => {
            assert_eq!(cd.url, "");
            assert!(!cd.selected_all);
        }
        other => panic!("expected CloneRepo modal, got {other:?}"),
    }

    // Multi-line git clone stderr must render the `fatal:` line cleanly without duplication.
    if let Some(ModalLayer::CloneRepo(cd)) = ts.state_mut().modal_stack.last_mut() {
        cd.cloning = true;
        cd.push_progress("Cloning into 'dest'...".into());
        cd.push_progress("fatal: repository 'https://example.com/missing.git' not found".into());
    }
    ts.state_mut()
        .apply_ui_async_event(crate::app::UiAsyncEvent::CloneFinished(Err(
            "Cloning into 'dest'...\nfatal: repository 'https://example.com/missing.git' not found"
                .into(),
        )));
    let buf = ts.render(100, 30);
    let screen = rendered_screen(&buf);
    let modal_body = screen.lines().skip(1).collect::<Vec<_>>().join("\n");
    assert_eq!(
        modal_body.matches("fatal: repository").count(),
        1,
        "multi-line clone error must display `fatal:` line once inside modal:\n{screen}"
    );
    assert_eq!(
        modal_body.matches("Cloning into 'dest'...").count(),
        1,
        "multi-line clone error must display context line once without duplicating progress:\n{screen}"
    );
}

#[test]
fn stage_view_initial_apply_status_selects_first_file_row_for_context_menu() {
    use jbgit_core::{FileStatusKind, StageEntry, StageStatusSnapshot, StageTreeRowKind};

    let mut ts = TestSession::with_snapshot(snapshot());
    let status = Arc::new(StageStatusSnapshot {
        staged: vec![StageEntry {
            path: "staged_partial.txt".into(),
            old_path: None,
            kind: FileStatusKind::Modified,
        }],
        unstaged: vec![StageEntry {
            path: "staged_partial.txt".into(),
            old_path: None,
            kind: FileStatusKind::Modified,
        }],
        ..StageStatusSnapshot::default()
    });
    ts.deliver_engine_event(EngineEvent::StageStatusLoaded { status, epoch: 1 });

    let sel = ts.state().stage_workbench.selected_idx();
    assert_ne!(sel, 0, "initial selection should skip group header row 0");
    let row = ts
        .state()
        .stage_workbench
        .rows()
        .get(sel)
        .expect("selected row exists");
    assert!(
        matches!(
            &row.kind,
            StageTreeRowKind::File { path, .. } if path == "staged_partial.txt"
        ),
        "expected selected row to be staged_partial.txt, got {:?}",
        row.kind
    );

    let items = crate::menus::stage_tree_menu_items(ts.state(), sel);
    for label in ["Show Diff", "Three-Side Diff", "Show File History"] {
        let item = items
            .iter()
            .find(|i| i.label.contains(label))
            .unwrap_or_else(|| panic!("menu item containing {label:?} missing: {items:?}"));
        assert!(
            item.is_enabled(),
            "menu item {:?} should be enabled on initial file selection",
            item.label
        );
    }
}

#[test]
fn push_dialog_target_edit_supports_ctrl_a_select_all_and_replace() {
    let mut ts = TestSession::with_snapshot(snapshot());
    let pd = crate::log_view::PushDialogState::new(
        "main".into(),
        vec!["origin".into()],
        0,
        "main".into(),
        false,
        None,
        jbgit_core::DiffContext::default(),
    );
    ts.state_mut()
        .modal_stack
        .push(ModalLayer::PushDialog(Box::new(pd)));

    // Press `e` to edit target (`main`), then Ctrl+a + Backspace -> empty.
    ts.send_key(KeyCode::Char('e'), KeyModifiers::NONE);
    assert!(ts.state().modal_stack.push_dialog().unwrap().editing_target);
    assert_eq!(
        ts.state().modal_stack.push_dialog().unwrap().target_draft,
        "main"
    );
    ts.send_key(KeyCode::Char('a'), KeyModifiers::CONTROL);
    assert!(
        ts.state()
            .modal_stack
            .push_dialog()
            .unwrap()
            .target_selected_all
    );
    ts.send_key(KeyCode::Backspace, KeyModifiers::NONE);
    let pd = ts.state().modal_stack.push_dialog().unwrap();
    assert_eq!(pd.target_draft, "");
    assert!(!pd.target_selected_all);

    // Cancel and re-enter edit, Ctrl+a + type "release" -> replaces entire draft.
    ts.send_key(KeyCode::Esc, KeyModifiers::NONE);
    ts.send_key(KeyCode::Char('e'), KeyModifiers::NONE);
    ts.send_key(KeyCode::Char('a'), KeyModifiers::CONTROL);
    for ch in "release".chars() {
        ts.send_key(KeyCode::Char(ch), KeyModifiers::NONE);
    }
    let pd = ts.state().modal_stack.push_dialog().unwrap();
    assert_eq!(pd.target_draft, "release");
    assert!(!pd.target_selected_all);

    // Ctrl+a followed by Esc must cancel edit AND reset `target_selected_all` immediately.
    ts.send_key(KeyCode::Char('a'), KeyModifiers::CONTROL);
    assert!(
        ts.state()
            .modal_stack
            .push_dialog()
            .unwrap()
            .target_selected_all
    );
    ts.send_key(KeyCode::Esc, KeyModifiers::NONE);
    let pd = ts.state().modal_stack.push_dialog().unwrap();
    assert!(!pd.editing_target);
    assert!(
        !pd.target_selected_all,
        "Esc cancel in PushDialog must reset target_selected_all"
    );
}

#[test]
fn inline_option_dialogs_support_ctrl_a_select_all() {
    use crate::dialogs::{
        CreatePatchDialog, CreatePatchRow, Dialog, MergeDialog, MergeDialogRow, PullDialog,
        PullDialogRow, RebaseDialog, RebaseDialogRow,
    };

    let snap = snapshot();
    let mut ts = TestSession::with_snapshot(snap.clone());

    // 1. MergeDialog (Message row)
    let mut md = MergeDialog::new("feature", "main", Some(2), Default::default());
    md.selected = MergeDialogRow::ALL
        .iter()
        .position(|r| *r == MergeDialogRow::Message)
        .unwrap();
    md.push_message("old merge msg");
    ts.state_mut()
        .modal_stack
        .push(ModalLayer::Dialog(Box::new(Dialog::Merge(md))));
    ts.send_key(KeyCode::Char('a'), KeyModifiers::CONTROL);
    ts.send_key(KeyCode::Backspace, KeyModifiers::NONE);
    match ts.state().modal_stack.last() {
        Some(ModalLayer::Dialog(d)) => match d.as_ref() {
            Dialog::Merge(m) => assert_eq!(m.message_text(), ""),
            other => panic!("expected MergeDialog, got {other:?}"),
        },
        other => panic!("expected Dialog, got {other:?}"),
    }
    for ch in "hello".chars() {
        ts.send_key(KeyCode::Char(ch), KeyModifiers::NONE);
    }
    ts.send_key(KeyCode::Char('a'), KeyModifiers::CONTROL);
    ts.send_key(KeyCode::Char('x'), KeyModifiers::NONE);
    match ts.state().modal_stack.last() {
        Some(ModalLayer::Dialog(d)) => match d.as_ref() {
            Dialog::Merge(m) => assert_eq!(m.message_text(), "x"),
            other => panic!("expected MergeDialog, got {other:?}"),
        },
        other => panic!("expected Dialog, got {other:?}"),
    }
    ts.send_key(KeyCode::Esc, KeyModifiers::NONE);

    // 2. PullDialog (Branch row)
    let mut pd = PullDialog::from_snapshot(&snap, Default::default());
    pd.selected = PullDialogRow::ALL
        .iter()
        .position(|r| *r == PullDialogRow::Branch)
        .unwrap();
    pd.options.branch = Some("feature/old".into());
    ts.state_mut()
        .modal_stack
        .push(ModalLayer::Dialog(Box::new(Dialog::Pull(pd))));
    ts.send_key(KeyCode::Char('a'), KeyModifiers::CONTROL);
    ts.send_key(KeyCode::Backspace, KeyModifiers::NONE);
    match ts.state().modal_stack.last() {
        Some(ModalLayer::Dialog(d)) => match d.as_ref() {
            Dialog::Pull(p) => assert_eq!(p.branch_text(), ""),
            other => panic!("expected PullDialog, got {other:?}"),
        },
        other => panic!("expected Dialog, got {other:?}"),
    }
    for ch in "main".chars() {
        ts.send_key(KeyCode::Char(ch), KeyModifiers::NONE);
    }
    ts.send_key(KeyCode::Char('a'), KeyModifiers::CONTROL);
    for ch in "dev".chars() {
        ts.send_key(KeyCode::Char(ch), KeyModifiers::NONE);
    }
    match ts.state().modal_stack.last() {
        Some(ModalLayer::Dialog(d)) => match d.as_ref() {
            Dialog::Pull(p) => assert_eq!(p.branch_text(), "dev"),
            other => panic!("expected PullDialog, got {other:?}"),
        },
        other => panic!("expected Dialog, got {other:?}"),
    }
    ts.send_key(KeyCode::Esc, KeyModifiers::NONE);

    // 3. RebaseDialog (Onto row)
    let mut rd =
        RebaseDialog::from_snapshot(&snap, Some("origin/main".into()), None, Default::default());
    rd.selected = RebaseDialogRow::ALL
        .iter()
        .position(|r| *r == RebaseDialogRow::Onto)
        .unwrap();
    ts.state_mut()
        .modal_stack
        .push(ModalLayer::Dialog(Box::new(Dialog::Rebase(rd))));
    ts.send_key(KeyCode::Char('a'), KeyModifiers::CONTROL);
    ts.send_key(KeyCode::Backspace, KeyModifiers::NONE);
    match ts.state().modal_stack.last() {
        Some(ModalLayer::Dialog(d)) => match d.as_ref() {
            Dialog::Rebase(r) => assert_eq!(r.options.onto, ""),
            other => panic!("expected RebaseDialog, got {other:?}"),
        },
        other => panic!("expected Dialog, got {other:?}"),
    }
    for ch in "v1".chars() {
        ts.send_key(KeyCode::Char(ch), KeyModifiers::NONE);
    }
    ts.send_key(KeyCode::Char('a'), KeyModifiers::CONTROL);
    for ch in "v2".chars() {
        ts.send_key(KeyCode::Char(ch), KeyModifiers::NONE);
    }
    match ts.state().modal_stack.last() {
        Some(ModalLayer::Dialog(d)) => match d.as_ref() {
            Dialog::Rebase(r) => assert_eq!(r.options.onto, "v2"),
            other => panic!("expected RebaseDialog, got {other:?}"),
        },
        other => panic!("expected Dialog, got {other:?}"),
    }
    ts.send_key(KeyCode::Esc, KeyModifiers::NONE);

    // 4. CreatePatchDialog (Path row)
    let cp = CreatePatchDialog {
        source: jbgit_core::PatchSource::StagePaths {
            staged: vec!["a.rs".into()],
            unstaged: Vec::new(),
            untracked: Vec::new(),
        },
        to_clipboard: false,
        path: "/tmp/old.patch".into(),
        base: PathBuf::from("/tmp"),
        reverse: false,
        selected: CreatePatchRow::ALL
            .iter()
            .position(|r| *r == CreatePatchRow::Path)
            .unwrap(),
        selected_all: false,
    };
    ts.state_mut()
        .modal_stack
        .push(ModalLayer::Dialog(Box::new(Dialog::CreatePatch(cp))));
    ts.send_key(KeyCode::Char('a'), KeyModifiers::CONTROL);
    ts.send_key(KeyCode::Backspace, KeyModifiers::NONE);
    match ts.state().modal_stack.last() {
        Some(ModalLayer::Dialog(d)) => match d.as_ref() {
            Dialog::CreatePatch(c) => assert_eq!(c.path, ""),
            other => panic!("expected CreatePatchDialog, got {other:?}"),
        },
        other => panic!("expected Dialog, got {other:?}"),
    }
    for ch in "a.patch".chars() {
        ts.send_key(KeyCode::Char(ch), KeyModifiers::NONE);
    }
    ts.send_key(KeyCode::Char('a'), KeyModifiers::CONTROL);
    for ch in "b.patch".chars() {
        ts.send_key(KeyCode::Char(ch), KeyModifiers::NONE);
    }
    match ts.state().modal_stack.last() {
        Some(ModalLayer::Dialog(d)) => match d.as_ref() {
            Dialog::CreatePatch(c) => assert_eq!(c.path, "b.patch"),
            other => panic!("expected CreatePatchDialog, got {other:?}"),
        },
        other => panic!("expected Dialog, got {other:?}"),
    }
}

#[test]
fn branch_compare_bar_preserves_counts_and_close_button_with_tree_diff() {
    let mut ts = zh_session_with_snapshot(snapshot());
    ts.state_mut().log.branch_compare = Some(crate::log_view::BranchCompareState {
        left: "main".into(),
        right: "feature/pull-me".into(),
        left_oid: oid(1),
        right_oid: oid(2),
        direction: jbgit_core::BranchCompareDirection::Symmetric,
        only_left: 1,
        only_right: 1,
        tree_diff: true,
    });
    let buf = ts.render(140, 40);
    let screen = rendered_screen(&buf);
    assert!(
        screen.contains("(← 1 · → 1)"),
        "compare bar must preserve complete commit counts:\n{screen}"
    );
    assert!(
        screen.contains("✕"),
        "compare bar must preserve close button:\n{screen}"
    );
    let close_rect = ts
        .state()
        .hit_registry
        .find(|t| *t == HitTarget::BranchCompareClose)
        .map(|r| r.rect);
    assert!(
        close_rect.is_some(),
        "BranchCompareClose hit target must be registered"
    );
}

fn sample_conflict_merge_state() -> ConflictMergeState {
    let doc = ConflictDocument::from_three_way(
        "conflict_app.txt",
        "line1\n<<<<<<< HEAD\nline2_ours\n=======\nline2_theirs\n>>>>>>> feature/theirs\nline3\nline4_theirs\n",
        Some("line1\nline2\nline3\nline4\n"),
        Some("line1\nline2_ours\nline3\nline4\n"),
        Some("line1\nline2_theirs\nline3\nline4_theirs\n"),
        Default::default(),
    );
    ConflictMergeState {
        path: "conflict_app.txt".into(),
        doc: Some(doc),
        ..Default::default()
    }
}

#[test]
fn conflict_merge_modal_narrow_viewports_keep_save_close_and_no_edge_bleed() {
    use crate::hit_test::ConflictMergeButtonKind;

    for (cols, rows) in [(80u16, 24u16), (50u16, 16u16)] {
        let mut ts = zh_session_with_snapshot(snapshot());
        ts.state_mut()
            .modal_stack
            .push(ModalLayer::ConflictResolver(Box::new(
                sample_conflict_merge_state(),
            )));
        let buf = ts.render(cols, rows);
        let screen = rendered_screen(&buf);
        assert!(
            screen.contains("保存"),
            "Save button must be visible at {cols}x{rows}:\n{screen}"
        );
        assert!(
            screen.contains("关闭"),
            "Close button must be visible at {cols}x{rows}:\n{screen}"
        );
        let has_save = ts
            .state()
            .hit_registry
            .find(|t| *t == HitTarget::ConflictMergeButton(ConflictMergeButtonKind::SaveAndStage))
            .is_some();
        let has_close = ts
            .state()
            .hit_registry
            .find(|t| *t == HitTarget::ConflictMergeButton(ConflictMergeButtonKind::Cancel))
            .is_some();
        assert!(has_save, "SaveAndStage hit target missing at {cols}x{rows}");
        assert!(has_close, "Cancel hit target missing at {cols}x{rows}");
        // Modal border spans full width (0..cols-1) so no underlying characters leak outside.
        assert_eq!(
            buf[(0, 2)].symbol(),
            "╭",
            "top-left modal border should start at col 0 on {cols}x{rows}"
        );
        assert_eq!(
            buf[(cols - 1, 2)].symbol(),
            "╮",
            "top-right modal border should reach col {} on {cols}x{rows}",
            cols - 1
        );
    }
}

#[test]
fn tree_browser_modal_has_symmetric_margins_without_one_col_sliver() {
    let mut ts = zh_session_with_snapshot(snapshot());
    ts.dispatch(Action::OpenTreeBrowser { oid: oid(1) });
    let buf = ts.render(140, 40);
    // 92% of 140 = 128 => x = 6..=133, 90% of 40 = 36 => y = 2..=37.
    assert_eq!(buf[(6, 2)].symbol(), "╭");
    assert_eq!(buf[(133, 2)].symbol(), "╮");
    assert_ne!(buf[(1, 1)].symbol(), "╭", "must not leave 1-col sliver gap");
}

#[test]
fn annotate_with_stacked_line_history_has_no_double_bottom_border() {
    let mut ts = zh_session_with_snapshot(snapshot());
    ts.dispatch(Action::OpenAnnotate {
        path: "src/main.rs".into(),
        rev: None,
    });
    ts.dispatch(Action::OpenFileHistory {
        path: "src/main.rs".into(),
        start_oid: None,
        follow_renames: false,
        line_range: Some((1, 5)),
    });
    let buf = ts.render(140, 40);
    let bottom_border_rows: Vec<u16> = (0..40)
        .filter(|&y| {
            let line: String = (0..140).map(|x| buf[(x, y)].symbol()).collect();
            line.contains('╰') && line.contains('╯')
        })
        .collect();
    assert_eq!(
        bottom_border_rows.len(),
        1,
        "stacked Annotate + Line History must not show double bottom borders, found rows: {bottom_border_rows:?}"
    );
}

#[test]
fn file_history_narrow_80x24_toolbar_renders_compact_buttons_without_clipping() {
    use crate::hit_test::FileHistoryToolbarButton;

    let mut ts = zh_session_with_snapshot(snapshot());
    ts.dispatch(Action::OpenFileHistory {
        path: "src/auth.rs".into(),
        start_oid: None,
        follow_renames: true,
        line_range: None,
    });
    let buf = ts.render(80, 24);
    let screen = rendered_screen(&buf);
    assert!(
        screen.contains("⤢全屏"),
        "compact Zoom button should be visible at 80x24:\n{screen}"
    );
    for btn in [
        FileHistoryToolbarButton::BranchScope,
        FileHistoryToolbarButton::FollowRenames,
        FileHistoryToolbarButton::CompareLocal,
        FileHistoryToolbarButton::SideBySide,
        FileHistoryToolbarButton::Zoom,
    ] {
        assert!(
            ts.state()
                .hit_registry
                .find(|t| *t == HitTarget::FileHistoryToolbar(btn))
                .is_some(),
            "expected FileHistoryToolbar({btn:?}) to be registered at 80x24"
        );
    }
}

#[test]
fn context_menu_presentation_my_commits_renders_without_ellipsis() {
    let mut ts = zh_session_with_snapshot(snapshot());
    let items = crate::input::presentation_menu_items(ts.state());
    ts.dispatch(Action::OpenContextMenu {
        anchor: (100, 4),
        items,
        depth: 0,
    });
    let buf = ts.render(140, 40);
    let screen = rendered_screen(&buf);
    assert!(
        screen.contains("高亮：我的提交 (My Commits)"),
        "presentation menu item must render completely without ellipsis:\n{screen}"
    );
}

#[test]
fn conflict_merge_bottom_buttons_change_style_on_hover() {
    use crate::hit_test::ConflictMergeButtonKind;

    let mut ts = zh_session_with_snapshot(snapshot());
    ts.state_mut()
        .modal_stack
        .push(ModalLayer::ConflictResolver(Box::new(
            sample_conflict_merge_state(),
        )));
    let base_buf = ts.render(140, 40);
    let hover_bg = ts.state().theme.bg_hover;
    let bright_fg = ts.state().theme.fg_bright;

    for kind in [
        ConflictMergeButtonKind::ApplyNonConflicting,
        ConflictMergeButtonKind::AcceptOurs,
    ] {
        let rect = ts
            .state()
            .hit_registry
            .find(|t| *t == HitTarget::ConflictMergeButton(kind))
            .map(|r| r.rect)
            .unwrap_or_else(|| panic!("button {kind:?} not registered"));
        let cx = rect.x + rect.width / 2;
        let cy = rect.y;
        let before = (base_buf[(cx, cy)].fg, base_buf[(cx, cy)].bg);

        ts.dispatch(Action::MouseMove { col: cx, row: cy });
        let after_buf = ts.render(140, 40);
        let after = (after_buf[(cx, cy)].fg, after_buf[(cx, cy)].bg);
        assert_ne!(
            before, after,
            "ConflictMergeButton({kind:?}) style must change on hover"
        );
        assert_eq!(after, (bright_fg, hover_bg));
    }
}

#[test]
fn log_filter_chips_gear_and_header_branch_pill_change_style_on_hover() {
    let mut ts = zh_session_with_snapshot(snapshot());
    let base_buf = ts.render(140, 40);

    for target in [
        HitTarget::FilterChip(FilterField::Branch),
        HitTarget::FilterPresentation,
        HitTarget::BranchWidgetPill,
    ] {
        let rect = ts
            .state()
            .hit_registry
            .find(|t| *t == target)
            .map(|r| r.rect)
            .unwrap_or_else(|| panic!("target {target:?} not registered"));
        let cx = rect.x + rect.width / 2;
        let cy = rect.y;
        let before = (
            base_buf[(cx, cy)].fg,
            base_buf[(cx, cy)].bg,
            base_buf[(cx, cy)].modifier,
        );

        ts.dispatch(Action::MouseMove { col: cx, row: cy });
        let after_buf = ts.render(140, 40);
        let after = (
            after_buf[(cx, cy)].fg,
            after_buf[(cx, cy)].bg,
            after_buf[(cx, cy)].modifier,
        );
        assert_ne!(before, after, "{target:?} cell style must change on hover");
    }
}

#[test]
fn log_filter_bar_hover_preserves_active_toggle_and_input_error_styles() {
    use ratatui::style::Modifier;

    let mut ts = zh_session_with_snapshot(snapshot());
    ts.state_mut().log.filter.spec.text.regex = true;
    ts.state_mut().log.filter.spec.text.match_case = true;
    ts.state_mut().log.filter.spec.text.query = "[invalid".into();
    ts.state_mut().log.filter.editing_text = true;
    ts.state_mut().log.graph.filter_error = Some(jbgit_core::FilterError::InvalidRegex(
        "invalid regex".into(),
    ));

    let _ = ts.render(140, 40);
    let active_bg = ts.state().theme.border_focused;
    let edit_bg = ts.state().theme.bg_selection_focused;
    let err_fg = ts.state().theme.status_conflicted;

    for target in [HitTarget::FilterRegexToggle, HitTarget::FilterCaseToggle] {
        let rect = ts
            .state()
            .hit_registry
            .find(|t| *t == target)
            .map(|r| r.rect)
            .unwrap_or_else(|| panic!("{target:?} not registered"));
        let cx = rect.x + rect.width / 2;
        let cy = rect.y;
        ts.dispatch(Action::MouseMove { col: cx, row: cy });
        let buf = ts.render(140, 40);
        assert_eq!(
            buf[(cx, cy)].bg,
            active_bg,
            "hovering active {target:?} must preserve border_focused background"
        );
        assert!(
            buf[(cx, cy)].modifier.contains(Modifier::UNDERLINED),
            "hovering active {target:?} should add UNDERLINED feedback"
        );
    }

    let input_rect = ts
        .state()
        .hit_registry
        .find(|t| *t == HitTarget::FilterTextInput)
        .map(|r| r.rect)
        .expect("FilterTextInput not registered");
    let cx = input_rect.x + 2;
    let cy = input_rect.y;
    ts.dispatch(Action::MouseMove { col: cx, row: cy });
    let buf = ts.render(140, 40);
    assert_eq!(
        buf[(cx, cy)].bg,
        edit_bg,
        "hovering FilterTextInput while editing must preserve bg_selection_focused"
    );
    assert_eq!(
        buf[(cx, cy)].fg,
        err_fg,
        "hovering FilterTextInput on regex error must preserve status_conflicted fg"
    );
}

// =========================================================================
// Round 3 QA regression tests (P2-1 through P2-5 + P3-1)
// =========================================================================

#[test]
fn round3_p2_1_regex_filter_error_toast_clears_when_filter_recovers() {
    let mut ts = zh_session_with_snapshot(snapshot());

    // Enter invalid regex filter -> status_notice reports filter error.
    ts.dispatch(Action::ToggleFilterRegex);
    for ch in "[invalid".chars() {
        ts.dispatch(Action::FilterTextInput(ch));
    }
    ts.dispatch(Action::FilterTextDone);
    assert!(
        ts.state()
            .status_notice
            .as_deref()
            .is_some_and(|n| n.contains("筛选错误")),
        "invalid regex must surface filter error toast, got: {:?}",
        ts.state().status_notice
    );

    // Clearing the text input recovers filter_error -> stale filter error toast clears.
    ts.dispatch(Action::FilterTextClear);
    ts.dispatch(Action::FilterTextDone);
    assert!(
        ts.state().log.graph.filter_error.is_none(),
        "clearing query must clear filter_error"
    );
    assert!(
        ts.state().status_notice.is_none(),
        "stale filter error toast must clear once regex recovers, got: {:?}",
        ts.state().status_notice
    );

    // Unrelated status_notice must NOT be clobbered when filter recovers.
    for ch in "[bad".chars() {
        ts.dispatch(Action::FilterTextInput(ch));
    }
    ts.dispatch(Action::FilterTextDone);
    assert!(ts.state().log.graph.filter_error.is_some());
    ts.state_mut().status_notice = Some("自定义提示".into());
    ts.dispatch(Action::FilterTextClear);
    ts.dispatch(Action::FilterTextDone);
    assert_eq!(
        ts.state().status_notice.as_deref(),
        Some("自定义提示"),
        "clearing filter error must not erase unrelated status_notice"
    );
}

#[test]
fn round3_p2_2_multi_active_filters_keep_no_merges_visible_at_140x40() {
    use jbgit_core::{DateFilter, PathFilter, UserFilter};

    // Two active filters (Date + Paths) in zh-CN at 140x40.
    let mut ts = zh_session_with_snapshot(snapshot());
    ts.state_mut().log.filter.spec.date = DateFilter::parse("yesterday", 1_700_000_000).unwrap();
    ts.state_mut().log.filter.spec.paths = PathFilter::parse("src/auth.rs");
    let buf = ts.render(140, 40);
    let row3 = rendered_row(&buf, 3);
    assert!(
        row3.contains("[排除合并]"),
        "[排除合并] must remain visible with 2 active filters at 140x40, got row 3: {row3:?}"
    );
    assert!(
        ts.state()
            .hit_registry
            .find(|t| *t == HitTarget::FilterNoMergesToggle)
            .is_some(),
        "FilterNoMergesToggle hit target must be registered with 2 active filters"
    );

    // Three active filters (User + Date + Paths) in zh-CN at 140x40.
    ts.state_mut().log.filter.spec.user = UserFilter::parse("QA");
    let buf3 = ts.render(140, 40);
    let row3_three = rendered_row(&buf3, 3);
    assert!(
        row3_three.contains("[排除合并]"),
        "[排除合并] must remain visible with 3 active filters at 140x40, got row 3: {row3_three:?}"
    );
    assert!(
        ts.state()
            .hit_registry
            .find(|t| *t == HitTarget::FilterNoMergesToggle)
            .is_some(),
        "FilterNoMergesToggle hit target must be registered with 3 active filters"
    );

    // Same check in en-US at 140x40.
    let mut ts_en = TestSession::with_snapshot(snapshot());
    ts_en.state_mut().log.filter.spec.date = DateFilter::parse("yesterday", 1_700_000_000).unwrap();
    ts_en.state_mut().log.filter.spec.paths = PathFilter::parse("src/auth.rs");
    let buf_en = ts_en.render(140, 40);
    let row3_en = rendered_row(&buf_en, 3);
    assert!(
        row3_en.contains("[No Merges]"),
        "[No Merges] must remain visible with 2 active filters in en-US at 140x40, got: {row3_en:?}"
    );
}

#[test]
fn round3_p2_3_lfs_binary_submodule_whitespace_block_visual_mode() {
    use jbgit_core::{LfsDiff, LfsPointer, SubmoduleState};
    use jbgit_engine::SubmoduleDiffInfo;

    let mut ts = zh_session_with_snapshot(snapshot());
    ts.dispatch(Action::SwitchTab(WorkspaceTab::Commit));

    let make_text_diff = |path: &str| {
        let base = b"line 1\nline 2\n";
        let local = b"line 1\nline 2 edited\n";
        let tracker = StageLineTracker::new(base, base, local);
        let (hunks, is_truncated) =
            tracker.build_interactive_hunks(false, DiffContext::Three, 1000);
        Arc::new(ThreeDocStageDiff {
            key: StageDiffKey {
                path: path.into(),
                old_path: None,
                side: StageDiffSide::Unstaged,
                context: DiffContext::Three,
                whitespace: WhitespaceMode::Default,
            },
            vcs_exists: true,
            staged_exists: true,
            local_exists: true,
            vcs_bytes: base.to_vec(),
            staged_bytes: base.to_vec(),
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
        })
    };

    // 1. LFS pointer blocks visual mode and emits LFS notice.
    let mut lfs_diff = (*make_text_diff("assets/model.bin")).clone();
    lfs_diff.lfs = Some(LfsDiff {
        old: None,
        new: Some(LfsPointer {
            oid_sha256: "0123456789abcdef".into(),
            size: 1024,
        }),
        filter_detected: true,
    });
    ts.state_mut()
        .stage_workbench
        .set_diff_for_test(Some(Arc::new(lfs_diff)));
    ts.dispatch(Action::StageDiffToggleVisual);
    assert!(
        ts.state().stage_workbench.visual_anchor_row().is_none(),
        "LFS pointer diff must not enter visual line-selection mode"
    );
    assert!(
        ts.state()
            .status_notice
            .as_deref()
            .is_some_and(|n| n.contains("LFS")),
        "LFS pointer visual toggle should show LFS notice, got: {:?}",
        ts.state().status_notice
    );

    // 2. Binary diff blocks visual mode (both key toggle and Shift+Click extend).
    let mut bin_diff = (*make_text_diff("assets/logo.bin")).clone();
    bin_diff.is_binary = true;
    bin_diff.hunks.clear();
    ts.state_mut()
        .stage_workbench
        .set_diff_for_test(Some(Arc::new(bin_diff)));
    ts.dispatch(Action::StageDiffToggleVisual);
    assert!(ts.state().stage_workbench.visual_anchor_row().is_none());
    ts.dispatch(Action::StageDiffSelectLine {
        row_idx: 0,
        extend: true,
    });
    assert!(ts.state().stage_workbench.visual_anchor_row().is_none());

    // 3. Submodule diff blocks visual mode.
    let mut sub_diff = (*make_text_diff("libs/submod")).clone();
    sub_diff.submodule = Some(SubmoduleDiffInfo {
        head_oid: Some(oid(1)),
        index_oid: Some(oid(1)),
        workdir_oid: Some(oid(2)),
        state: SubmoduleState {
            new_commits: true,
            dirty_workdir: true,
            uninitialized: false,
        },
    });
    ts.state_mut()
        .stage_workbench
        .set_diff_for_test(Some(Arc::new(sub_diff)));
    ts.dispatch(Action::StageDiffToggleVisual);
    assert!(ts.state().stage_workbench.visual_anchor_row().is_none());

    // 4. Ignore-whitespace mode blocks visual mode.
    ts.state_mut()
        .stage_workbench
        .set_diff_for_test(Some(make_text_diff("src/wide_lines.rs")));
    ts.dispatch(Action::DiffCycleWhitespace(crate::app::DiffScope::Stage));
    ts.dispatch(Action::StageDiffToggleVisual);
    assert!(
        ts.state().stage_workbench.visual_anchor_row().is_none(),
        "ignore-whitespace mode must not enter visual line-selection mode"
    );
}

#[test]
fn round3_p2_4_file_history_toolbar_auto_compacts_at_140x40_with_side_by_side_and_context_10() {
    use crate::app::DiffScope;
    use crate::hit_test::FileHistoryToolbarButton;

    let mut ts = zh_session_with_snapshot(snapshot());
    ts.dispatch(Action::OpenFileHistory {
        path: "src/auth.rs".into(),
        start_oid: None,
        follow_renames: true,
        line_range: None,
    });
    ts.dispatch(Action::DiffToggleSideBySide(DiffScope::FileHistory));
    ts.dispatch(Action::DiffCycleContext(DiffScope::FileHistory));
    let buf = ts.render(140, 40);
    let screen = rendered_screen(&buf);
    assert!(
        ts.state()
            .hit_registry
            .find(|t| *t == HitTarget::FileHistoryToolbar(FileHistoryToolbarButton::LocateInLog))
            .is_some(),
        "LocateInLog button must remain visible and clickable at 140x40 when SideBySide + Lines10 are active:\n{screen}"
    );
}

#[test]
fn round3_p2_5_log_zoomed_diff_stays_zoomed_when_switching_merge_parent_to_empty_changes() {
    let p1 = super::oid_n(1);
    let p2 = super::oid_n(2);
    let merge_oid = super::oid_n(3);
    let mut snap = snapshot();
    snap.commits = vec![
        super::make_commit(3, vec![p1, p2], "merge feature/ui-cards"),
        super::make_commit(1, vec![], "parent 1"),
        super::make_commit(2, vec![], "parent 2"),
    ];
    snap.head_oid = Some(merge_oid);
    snap.refs = vec![local("main", 3)];

    let mut ts = zh_session_with_snapshot(snap);
    let key_p1 = DetailsKey::commits(vec![merge_oid], 0);
    let bundle_p1 = Arc::new(CommitChangesBundle {
        key: key_p1.clone(),
        details: vec![super::make_details(
            merge_oid,
            vec![p1, p2],
            "merge feature/ui-cards",
        )],
        changes: vec![jbgit_core::FileChange {
            path: "src/card.rs".into(),
            old_path: None,
            kind: FileStatusKind::Modified,
            is_binary: false,
            old_oid: Some(oid(10)),
            new_oid: Some(oid(11)),
            additions: 2,
            deletions: 1,
            exists_in_workdir: true,
        }],
    });
    ts.state_mut().diff_caches.insert_details(bundle_p1);
    ts.state_mut().log.details.changes.current_key = Some(key_p1);
    let file_row = ts
        .state()
        .log
        .details
        .change_rows(&ts.state().diff_caches)
        .iter()
        .position(|r| r.label == "card.rs")
        .expect("card.rs row in changes tree");
    ts.dispatch(Action::ChangesSelect(
        crate::app::ChangesHost::Log,
        file_row,
    ));

    // Enter fullscreen diff on parent 1/2.
    ts.dispatch(Action::ZoomDiff(true));
    assert!(ts.state().log.details.diff.zoomed);

    // Switch to parent 2/2 (initially uncached -> loading). Must remain zoomed!
    ts.dispatch(Action::SwitchMergeParent);
    assert_eq!(ts.state().log.details.merge_parent, 1);
    assert!(
        ts.state().log.details.diff.zoomed,
        "switching merge parent while zoomed must NOT exit fullscreen during loading"
    );

    // Deliver empty bundle for parent 2/2 (no file changes). Must remain zoomed and render cleanly!
    let key_p2 = DetailsKey::commits(vec![merge_oid], 1);
    let empty_bundle = Arc::new(CommitChangesBundle {
        key: key_p2.clone(),
        details: vec![super::make_details(
            merge_oid,
            vec![p1, p2],
            "merge feature/ui-cards",
        )],
        changes: vec![],
    });
    let ep = ts
        .state()
        .request_slots
        .latest(jbgit_core::RequestSlotId::LogCommitDetails);
    ts.deliver_engine_event(EngineEvent::CommitDetailsLoaded {
        slot: jbgit_core::RequestSlotId::LogCommitDetails,
        bundle: empty_bundle,
        epoch: ep,
    });
    assert!(
        ts.state().log.details.diff.zoomed,
        "empty parent 2/2 bundle must keep fullscreen diff active"
    );
    let buf = ts.render(140, 40);
    let screen = rendered_screen(&buf);
    assert!(
        screen.contains("无差异内容") || screen.contains("无变更"),
        "zoomed diff with empty parent changes should render clean empty state:\n{screen}"
    );

    // Pressing SwitchMergeParent again returns to parent 1/2 while staying zoomed
    // and automatically restores selection onto the first changed file.
    ts.dispatch(Action::SwitchMergeParent);
    assert_eq!(ts.state().log.details.merge_parent, 0);
    assert!(ts.state().log.details.diff.zoomed);
    assert_eq!(
        ts.state()
            .log
            .details
            .selected_change(&ts.state().diff_caches)
            .map(|c| c.path.as_str()),
        Some("src/card.rs")
    );
}

#[test]
fn round3_p3_1_commit_editor_and_message_dialog_support_ctrl_a_and_multiline_editing() {
    use crate::dialogs::{Dialog, MessageDialog, MessageIntent};

    // 1. CommitMessageEditor: Ctrl+a select-all, replace, undo, backspace, Ctrl+u.
    let mut ts = zh_session_with_snapshot(snapshot());
    ts.dispatch(Action::SwitchTab(WorkspaceTab::Commit));
    ts.dispatch(Action::FocusPane(jbgit_core::PaneId::CommitMessageEditor));
    ts.dispatch(Action::CommitEditorPaste(
        "feat: old title\nbody line".into(),
    ));
    assert_eq!(
        ts.state().stage_workbench.editor().text(),
        "feat: old title\nbody line"
    );

    ts.send_key(KeyCode::Char('a'), KeyModifiers::CONTROL);
    assert!(ts.state().stage_workbench.editor().selected_all);
    // Rendering while selected_all applies selection background highlight cleanly.
    let _ = ts.render(140, 40);

    // Typing replaces the entire selected text in a single undoable step.
    ts.send_key(KeyCode::Char('X'), KeyModifiers::NONE);
    assert_eq!(ts.state().stage_workbench.editor().text(), "X");
    assert!(!ts.state().stage_workbench.editor().selected_all);
    ts.send_key(KeyCode::Char('z'), KeyModifiers::CONTROL);
    assert_eq!(
        ts.state().stage_workbench.editor().text(),
        "feat: old title\nbody line"
    );

    // Ctrl+a + Backspace clears the whole editor.
    ts.send_key(KeyCode::Char('a'), KeyModifiers::CONTROL);
    ts.send_key(KeyCode::Backspace, KeyModifiers::NONE);
    assert_eq!(ts.state().stage_workbench.editor().text(), "");

    // Ctrl+u deletes to line start.
    ts.dispatch(Action::CommitEditorPaste("prefix_keep".into()));
    ts.send_key(KeyCode::Left, KeyModifiers::NONE);
    ts.send_key(KeyCode::Left, KeyModifiers::NONE);
    ts.send_key(KeyCode::Left, KeyModifiers::NONE);
    ts.send_key(KeyCode::Left, KeyModifiers::NONE);
    ts.send_key(KeyCode::Char('u'), KeyModifiers::CONTROL);
    assert_eq!(ts.state().stage_workbench.editor().text(), "keep");

    // 2. Dialog::Message (Reword): Ctrl+a, Ctrl+u, cursor nav, Delete, paste.
    ts.dispatch(Action::OpenDialog(Box::new(Dialog::Message(
        MessageDialog::new(
            "Reword Commit",
            "line one\nline two".into(),
            MessageIntent::RewordHead,
        ),
    ))));

    // Move Home, Delete first char of line two ('l' -> "ine two"), Up, End, insert '!'.
    ts.send_key(KeyCode::Home, KeyModifiers::NONE);
    ts.send_key(KeyCode::Delete, KeyModifiers::NONE);
    ts.send_key(KeyCode::Up, KeyModifiers::NONE);
    ts.send_key(KeyCode::End, KeyModifiers::NONE);
    ts.send_key(KeyCode::Char('!'), KeyModifiers::NONE);
    if let Some(ModalLayer::Dialog(d)) = ts.state().modal_stack.last()
        && let Dialog::Message(m) = d.as_ref()
    {
        assert_eq!(m.text, "line one!\nine two");
    } else {
        panic!("expected MessageDialog");
    }

    // Ctrl+a + paste replaces all text.
    ts.send_key(KeyCode::Char('a'), KeyModifiers::CONTROL);
    let _ = ts.render(140, 40);
    ts.dispatch(Action::ModalPaste("reworded message".into()));
    if let Some(ModalLayer::Dialog(d)) = ts.state().modal_stack.last()
        && let Dialog::Message(m) = d.as_ref()
    {
        assert_eq!(m.text, "reworded message");
        assert!(!m.selected_all);
    } else {
        panic!("expected MessageDialog");
    }

    // Ctrl+u clears the message.
    ts.send_key(KeyCode::Char('u'), KeyModifiers::CONTROL);
    if let Some(ModalLayer::Dialog(d)) = ts.state().modal_stack.last()
        && let Dialog::Message(m) = d.as_ref()
    {
        assert_eq!(m.text, "");
    } else {
        panic!("expected MessageDialog");
    }
}
