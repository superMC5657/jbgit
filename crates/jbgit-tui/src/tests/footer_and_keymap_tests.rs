use std::sync::Arc;

use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEventKind};
use jbgit_core::{GitOp, ModalButton, PaneId, WorkspaceTab};
use jbgit_engine::{StageDiffKey, StageDiffSide, TaskRequest};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::layout::Rect;
use unicode_width::UnicodeWidthStr;

use crate::app::{Action, ModalLayer, PopupKind};
use crate::config::{KeymapConfig, KeymapPreset, keymap_config::KeymapConfigOverlay};
use crate::dialogs::Dialog;
use crate::hit_test::HitTarget;
use crate::input::route_terminal_event;
use crate::keymap::KeyAction;
use crate::keymap::help::{HelpRow, build_help_rows};

use super::{
    TestSession, find_item, oid, row_text, screen_text, snapshot,
};

#[test]
fn dynamic_footer_tracks_focus_and_keymap_overrides() {
    let mut ts = TestSession::with_snapshot(snapshot());
    ts.state_mut().config.keymap = KeymapConfig::for_preset(KeymapPreset::Default);
    ts.dispatch(Action::SwitchTab(WorkspaceTab::Log));
    ts.dispatch(Action::FocusPane(PaneId::LogCommitTable));
    let buf = ts.render(140, 40);
    let log_footer = screen_text(&buf);
    assert!(log_footer.contains("Filter"), "{log_footer}");
    assert!(log_footer.contains("Checkout"), "{log_footer}");
    assert!(log_footer.contains("Help"), "{log_footer}");

    ts.dispatch(Action::SwitchTab(WorkspaceTab::Commit));
    ts.dispatch(Action::FocusPane(PaneId::CommitChangesTree));
    let buf = ts.render(140, 40);
    let commit_footer = screen_text(&buf);
    assert!(commit_footer.contains("Space"), "{commit_footer}");
    assert!(commit_footer.contains("Stage"), "{commit_footer}");

    let ov: KeymapConfigOverlay = toml::from_str(
        r#"
[commit]
prepend_keymap = [
    { on = "S", run = "toggle_stage" },
]
"#,
    )
    .unwrap();
    ts.state_mut().config.keymap.merge_overlay(&ov);
    let buf = ts.render(140, 40);
    let updated_footer = screen_text(&buf);
    assert!(updated_footer.contains(" S "), "{updated_footer}");
}

#[test]
fn footer_hints_match_active_mode_dock_stash_and_console_probe() {
    let mut ts = TestSession::with_snapshot(snapshot());
    ts.state_mut().config.keymap = KeymapConfig::for_preset(KeymapPreset::Default);

    // 1. LogDetailsDiff in Commit Details mode (diff_preview = false) must NOT show diff-only hints
    ts.dispatch(Action::SwitchTab(WorkspaceTab::Log));
    ts.dispatch(Action::FocusPane(PaneId::LogDetailsDiff));
    ts.state_mut().log.details.diff_preview = false;
    let buf = ts.render(140, 40);
    let details_footer = row_text(&buf, 39);
    assert!(details_footer.contains("Diff"), "{details_footer}");
    assert!(!details_footer.contains("Dock"), "{details_footer}");
    assert!(!details_footer.contains("Split"), "{details_footer}");
    assert!(!details_footer.contains("Context"), "{details_footer}");
    assert!(!details_footer.contains("Hunk"), "{details_footer}");

    // 2. LogDetailsDiff in Diff Preview mode (diff_preview = true) shows core diff hints
    //    while keeping rarely used layout toggles hidden unless opted in via show_hint = true.
    ts.state_mut().log.details.diff_preview = true;
    let buf = ts.render(140, 40);
    let diff_footer = row_text(&buf, 39);
    assert!(diff_footer.contains("Hunk"), "{diff_footer}");
    assert!(diff_footer.contains("File"), "{diff_footer}");
    assert!(diff_footer.contains("Zoom"), "{diff_footer}");
    assert!(!diff_footer.contains("Dock"), "{diff_footer}");
    assert!(!diff_footer.contains("Split"), "{diff_footer}");
    assert!(!diff_footer.contains("Context"), "{diff_footer}");

    let dock_ov: KeymapConfigOverlay = toml::from_str(
        r#"
[diff]
prepend_keymap = [
    { on = "m", run = "cycle_diff_dock", show_hint = true },
]
"#,
    )
    .unwrap();
    let mut opt_ts = TestSession::with_snapshot(snapshot());
    opt_ts.state_mut().config.keymap = KeymapConfig::for_preset(KeymapPreset::Default);
    opt_ts.state_mut().config.keymap.merge_overlay(&dock_ov);
    opt_ts.dispatch(Action::SwitchTab(WorkspaceTab::Log));
    opt_ts.dispatch(Action::FocusPane(PaneId::LogDetailsDiff));
    opt_ts.state_mut().log.details.diff_preview = true;
    let buf = opt_ts.render(140, 40);
    let opt_in_footer = row_text(&buf, 39);
    assert!(opt_in_footer.contains(" m "), "{opt_in_footer}");
    assert!(opt_in_footer.contains("Dock"), "{opt_in_footer}");

    // 3. CommitDiffPreview shows Stage / Hunk / Zoom / Back (`Esc`) without rare `Context`
    ts.dispatch(Action::SwitchTab(WorkspaceTab::Commit));
    ts.dispatch(Action::FocusPane(PaneId::CommitDiffPreview));
    let buf = ts.render(160, 40);
    let commit_diff_footer = row_text(&buf, 39);
    assert!(commit_diff_footer.contains("Stage"), "{commit_diff_footer}");
    assert!(commit_diff_footer.contains("Hunk"), "{commit_diff_footer}");
    assert!(commit_diff_footer.contains("Esc"), "{commit_diff_footer}");
    assert!(!commit_diff_footer.contains("Context"), "{commit_diff_footer}");

    // 4. CommitMessageEditor shows Commit / Amend / Esc without rare `Sign-off`
    ts.dispatch(Action::FocusPane(PaneId::CommitMessageEditor));
    let buf = ts.render(140, 40);
    let msg_footer = row_text(&buf, 39);
    assert!(msg_footer.contains("Commit"), "{msg_footer}");
    assert!(msg_footer.contains("Amend"), "{msg_footer}");
    assert!(!msg_footer.contains("Sign-off"), "{msg_footer}");

    // 5. StashDiffPreview does NOT advertise ghost Zoom and shows Hunk / File without rare `Group`
    ts.dispatch(Action::SwitchTab(WorkspaceTab::Stash));
    ts.dispatch(Action::FocusPane(PaneId::StashDiffPreview));
    let buf = ts.render(140, 40);
    let stash_footer = row_text(&buf, 39);
    assert!(!stash_footer.contains("Zoom"), "{stash_footer}");
    assert!(stash_footer.contains("Hunk"), "{stash_footer}");
    assert!(stash_footer.contains("File"), "{stash_footer}");
    assert!(!stash_footer.contains("Group"), "{stash_footer}");
    ts.send_key(KeyCode::Char('z'), KeyModifiers::NONE);
    assert!(!ts.state().log.details.diff.zoomed);

    // 6. ConsoleCommandList `p` actually triggers status probe instead of global git_pull
    ts.dispatch(Action::SwitchTab(WorkspaceTab::Console));
    ts.dispatch(Action::FocusPane(PaneId::ConsoleCommandList));
    let buf = ts.render(140, 40);
    let console_footer = row_text(&buf, 39);
    assert!(console_footer.contains(" p "), "{console_footer}");
    assert!(console_footer.contains("Status"), "{console_footer}");
    let _ = ts.take_requests();
    ts.send_key(KeyCode::Char('p'), KeyModifiers::NONE);
    let reqs = ts.take_requests();
    assert!(
        reqs.iter().any(|(r, _)| matches!(
            r,
            TaskRequest::RunGitCli { args, .. } if args.first().map(String::as_str) == Some("status")
        )),
        "expected RunGitCli(status), got {reqs:?}"
    );
}

#[test]
fn which_key_popup_appears_on_prefix_and_dismisses() {
    let mut ts = TestSession::with_snapshot(snapshot());
    ts.dispatch(Action::SwitchTab(WorkspaceTab::Log));
    ts.dispatch(Action::FocusPane(PaneId::LogCommitTable));

    ts.send_key(KeyCode::Char('g'), KeyModifiers::NONE);
    assert_eq!(ts.state().pending_keys.len(), 1);
    let buf = ts.render(140, 40);
    let txt = screen_text(&buf);
    assert!(txt.contains("g +"), "{txt}");
    assert!(txt.contains("Esc cancel"), "{txt}");

    // Second key completes the sequence and closes Which-Key.
    ts.send_key(KeyCode::Char('g'), KeyModifiers::NONE);
    assert!(ts.state().pending_keys.is_empty());
    let buf = ts.render(140, 40);
    let txt_after = screen_text(&buf);
    assert!(!txt_after.contains("g +"), "{txt_after}");

    // Pressing Esc cancels a pending chord cleanly.
    ts.send_key(KeyCode::Char('g'), KeyModifiers::NONE);
    assert_eq!(ts.state().pending_keys.len(), 1);
    ts.send_key(KeyCode::Esc, KeyModifiers::NONE);
    assert!(ts.state().pending_keys.is_empty());
}

#[test]
fn shortcuts_help_palette_filters_and_executes_selected_action() {
    let mut ts = TestSession::with_snapshot(snapshot());
    ts.dispatch(Action::SwitchTab(WorkspaceTab::Log));
    ts.dispatch(Action::FocusPane(PaneId::LogCommitTable));

    ts.send_key(KeyCode::Char('?'), KeyModifiers::SHIFT);
    assert!(matches!(
        ts.state().modal_stack.last(),
        Some(ModalLayer::ShortcutsHelp { .. })
    ));

    ts.type_str("palette");
    let buf = ts.render(120, 40);
    let txt = screen_text(&buf);
    assert!(txt.contains("palette"), "{txt}");

    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);
    assert!(matches!(
        ts.state().modal_stack.last(),
        Some(ModalLayer::PalettePreview)
    ));
}

#[test]
fn jetbrains_tree_speed_search_preserves_context_menu_dot_and_vim_diff_menu_shows_su() {
    // 1. JetBrains preset: `.` on LogBranchesTree opens context menu instead of speed search.
    let mut ts = TestSession::with_snapshot(snapshot());
    ts.state_mut().config.keymap = KeymapConfig::for_preset(KeymapPreset::Jetbrains);
    ts.dispatch(Action::SwitchTab(WorkspaceTab::Log));
    ts.dispatch(Action::FocusPane(PaneId::LogBranchesTree));
    let _ = ts.render(140, 40);

    ts.send_key(KeyCode::Char('.'), KeyModifiers::NONE);
    assert!(
        matches!(ts.state().modal_stack.last(), Some(ModalLayer::ContextMenu { .. })),
        "expected context menu on '.' under jetbrains preset"
    );
    ts.dispatch(Action::PopContextMenus);

    ts.type_str("main");
    assert_eq!(ts.state().log.branches.query, "main");

    // 2. Vim preset: stage_diff_menu_items displays `s` and `u` separately.
    ts.state_mut().config.keymap = KeymapConfig::for_preset(KeymapPreset::Vim);
    let diff_menu = crate::menus::stage_diff_menu_items(ts.state());
    assert_eq!(
        find_item(&diff_menu, "Stage Hunk or Lines").shortcut.as_deref(),
        Some("s")
    );
    assert_eq!(
        find_item(&diff_menu, "Unstage Hunk or Lines").shortcut.as_deref(),
        Some("u")
    );
}

#[test]
fn zh_cn_reset_dialog_and_collapsed_branches_stripe_align_cjk_widths() {
    let mut ts = TestSession::with_snapshot(snapshot());
    ts.state_mut().i18n = Arc::new(crate::i18n::I18n::load("zh-CN", None));

    // Reset dialog flags must start at the exact same display column in zh-CN.
    let dlg = crate::dialogs::reset_dialog(oid(9), "修复对齐", "main", &ts.state().i18n);
    let Dialog::Choice(choice) = dlg else {
        panic!("expected ChoiceDialog");
    };
    let flag_cols: Vec<usize> = choice
        .options
        .iter()
        .map(|o| {
            let prefix = o.label.split("--").next().unwrap();
            prefix.width()
        })
        .collect();
    assert!(
        flag_cols.windows(2).all(|w| w[0] == w[1]),
        "reset mode flags misaligned: {flag_cols:?}"
    );

    // Collapsed branches stripe renders `"分支"` cleanly into a 2-column area.
    let mut term = Terminal::new(TestBackend::new(2, 6)).unwrap();
    let mut hit_registry = crate::hit_test::HitTestRegistry::default();
    let state = ts.state();
    let mut ctx = crate::app::RenderContext {
        theme: &state.theme,
        glyph_mode: state.glyph_mode,
        branch_colors: &state.branch_colors,
        hit_registry: &mut hit_registry,
        focused_pane: PaneId::LogCommitTable,
        hover_target: None,
        dragging_splitter: None,
        graph_renderer: None,
        i18n: Some(&state.i18n),
        config: Some(&state.config),
        scroll_bounds: None,
        commit_editor_viewport: None,
        splitter_containers: None,
    };
    term.draw(|frame| {
        crate::views::log_branches::render_branches_stripe(
            frame,
            Rect::new(0, 0, 2, 6),
            &mut ctx,
        );
    })
    .unwrap();
    let buf = term.backend().buffer();
    assert_eq!(buf[(0, 1)].symbol(), "分");
    assert_eq!(buf[(0, 2)].symbol(), "支");
}

#[test]
fn footer_and_help_respect_amend_readonly_author_edit_speed_search_and_console_detail() {
    // 1. CommitDiffPreview in Amend read-only mode hides write-only pills and help entries
    let mut ts = TestSession::with_snapshot(snapshot());
    ts.dispatch(Action::SwitchTab(WorkspaceTab::Commit));
    ts.dispatch(Action::FocusPane(PaneId::CommitDiffPreview));
    ts.state_mut().stage_workbench.set_diff_key_for_test(Some(StageDiffKey {
        path: "a.rs".into(),
        old_path: None,
        side: StageDiffSide::AmendCommit,
        context: jbgit_core::DiffContext::Three,
    }));
    let buf = ts.render(160, 40);
    let amend_footer = row_text(&buf, 39);
    assert!(amend_footer.contains("Hunk"), "{amend_footer}");
    assert!(amend_footer.contains("File"), "{amend_footer}");
    assert!(amend_footer.contains("Esc"), "{amend_footer}");
    assert!(!amend_footer.contains("Stage"), "{amend_footer}");
    assert!(!amend_footer.contains("Rollback"), "{amend_footer}");
    assert!(!amend_footer.contains("Visual"), "{amend_footer}");
    assert!(!amend_footer.contains("3-Way"), "{amend_footer}");
    let help_rows = build_help_rows(ts.state(), "");
    let ctx_rows: Vec<&HelpRow> = help_rows
        .iter()
        .skip(1)
        .take_while(|r| !matches!(r, HelpRow::Section(_)))
        .collect();
    let has_ctx_action = |action: KeyAction| {
        ctx_rows.iter().any(|r| {
            matches!(
                r,
                HelpRow::Entry {
                    run: Some(run),
                    ..
                } if *run == action
            )
        })
    };
    assert!(!has_ctx_action(KeyAction::RollbackSelection));
    assert!(!has_ctx_action(KeyAction::ToggleVisual));
    assert!(!has_ctx_action(KeyAction::ThreeWayInspector));
    assert!(!has_ctx_action(KeyAction::SwitchDiffSide));
    assert!(has_ctx_action(KeyAction::NextHunk));
    assert!(has_ctx_action(KeyAction::CycleDiffContext));

    // 2. CommitMessageEditor with editing_author = true shows Enter/Esc Apply instead of Esc Back
    ts.dispatch(Action::FocusPane(PaneId::CommitMessageEditor));
    ts.dispatch(Action::CommitFocusAuthorInput);
    let buf = ts.render(160, 40);
    let author_footer = row_text(&buf, 39);
    assert!(author_footer.contains("Enter/Esc"), "{author_footer}");
    assert!(author_footer.contains("Apply"), "{author_footer}");
    assert!(!author_footer.contains("Back"), "{author_footer}");

    // 3. LogBranchesTree under JetBrains preset (tree_speed_search = true) hides captured single letters
    let mut jb_ts = TestSession::with_snapshot(snapshot());
    jb_ts.state_mut().config.keymap = KeymapConfig::for_preset(KeymapPreset::Jetbrains);
    jb_ts.dispatch(Action::SwitchTab(WorkspaceTab::Log));
    jb_ts.dispatch(Action::FocusPane(PaneId::LogBranchesTree));
    let buf = jb_ts.render(160, 40);
    let jb_footer = row_text(&buf, 39);
    assert!(jb_footer.contains("Type"), "{jb_footer}");
    assert!(jb_footer.contains("Ctrl+F"), "{jb_footer}");
    assert!(jb_footer.contains("Ctrl+T"), "{jb_footer}");
    assert!(jb_footer.contains("Del"), "{jb_footer}");
    assert!(!jb_footer.contains("Checkout"), "{jb_footer}");
    assert!(!jb_footer.contains("Fetch"), "{jb_footer}");

    // 4. ConsoleOutputDetail help does not advertise GitStatusProbe and `p` does not trigger probe
    let mut console_ts = TestSession::with_snapshot(snapshot());
    console_ts.dispatch(Action::SwitchTab(WorkspaceTab::Console));
    console_ts.dispatch(Action::FocusPane(PaneId::ConsoleOutputDetail));
    let detail_help = build_help_rows(console_ts.state(), "");
    assert!(
        !detail_help.iter().take(5).any(|r| matches!(
            r,
            HelpRow::Entry {
                run: Some(KeyAction::GitStatusProbe),
                ..
            }
        )),
        "ConsoleOutputDetail context help should not advertise GitStatusProbe"
    );
    let _ = console_ts.take_requests();
    console_ts.send_key(KeyCode::Char('p'), KeyModifiers::NONE);
    let reqs = console_ts.take_requests();
    assert!(
        !reqs
            .iter()
            .any(|(r, _)| matches!(r, TaskRequest::RunGitCli { .. })),
        "p in ConsoleOutputDetail must not run RunGitCli: {reqs:?}"
    );
}

#[test]
fn footer_selective_show_hint_and_declaration_order() {
    let mut ts = TestSession::with_snapshot(snapshot());
    ts.state_mut().config.keymap = KeymapConfig::for_preset(KeymapPreset::Default);
    ts.dispatch(Action::SwitchTab(WorkspaceTab::Log));
    ts.dispatch(Action::FocusPane(PaneId::LogCommitTable));

    // Default Log footer places Log scope before Global scope
    let buf = ts.render(200, 30);
    let default_footer = row_text(&buf, 29);
    let filter_pos = default_footer.find("Filter").expect(&default_footer);
    let menu_pos = default_footer.find("Menu").expect(&default_footer);
    assert!(filter_pos < menu_pos, "{default_footer}");
    assert!(default_footer.contains("Copy hash"), "{default_footer}");
    assert!(default_footer.contains("Help"), "{default_footer}");

    // Hide copy_hash, promote uncommit via prepend_keymap, and hide help
    let ov: KeymapConfigOverlay = toml::from_str(
        r#"
[log]
prepend_keymap = [
    { on = "u", run = "uncommit", show_hint = true },
    { on = "y", run = "copy_hash", show_hint = false },
]

[global]
prepend_keymap = [
    { on = "?", run = "help", show_hint = false },
]
"#,
    )
    .unwrap();
    ts.state_mut().config.keymap.merge_overlay(&ov);

    let buf = ts.render(200, 30);
    let updated_footer = row_text(&buf, 29);
    assert!(updated_footer.contains("Uncommit"), "{updated_footer}");
    assert!(!updated_footer.contains("Copy hash"), "{updated_footer}");
    assert!(!updated_footer.contains("Help"), "{updated_footer}");
    let uncommit_pos = updated_footer.find("Uncommit").expect(&updated_footer);
    let filter_pos2 = updated_footer.find("Filter").expect(&updated_footer);
    assert!(uncommit_pos < filter_pos2, "{updated_footer}");

    // Reordering entries in [log] changes left-to-right order in footer
    let mut ts_reordered = TestSession::with_snapshot(snapshot());
    ts_reordered.state_mut().config.keymap = KeymapConfig::for_preset(KeymapPreset::Default);
    ts_reordered.dispatch(Action::SwitchTab(WorkspaceTab::Log));
    ts_reordered.dispatch(Action::FocusPane(PaneId::LogCommitTable));
    let reorder_ov: KeymapConfigOverlay = toml::from_str(
        r#"
[log]
key_bindings = [
    { on = "y", run = "copy_hash", show_hint = true },
    { on = "c", run = "checkout", show_hint = true },
]
"#,
    )
    .unwrap();
    ts_reordered.state_mut().config.keymap.merge_overlay(&reorder_ov);
    let buf = ts_reordered.render(160, 30);
    let reordered_footer = row_text(&buf, 29);
    let copy_pos = reordered_footer.find("Copy hash").expect(&reordered_footer);
    let checkout_pos = reordered_footer.find("Checkout").expect(&reordered_footer);
    assert!(copy_pos < checkout_pos, "{reordered_footer}");
}

#[test]
fn footer_overflow_single_line_multiline_and_hidden() {
    let mut ts = TestSession::with_snapshot(snapshot());
    ts.state_mut().config.keymap = KeymapConfig::for_preset(KeymapPreset::Default);
    ts.dispatch(Action::SwitchTab(WorkspaceTab::Log));
    ts.dispatch(Action::FocusPane(PaneId::LogCommitTable));

    // 1. Default footer_max_lines = 1 at narrow width (80 cols): shows `+N` and right-aligned `? Help`
    ts.state_mut().config.keymap.footer_max_lines = 1;
    let buf = ts.render(80, 24);
    let row_last = row_text(&buf, 23);
    assert!(row_last.contains("Filter"), "{row_last}");
    assert!(row_last.contains("Help"), "{row_last}");
    assert!(row_last.contains(" +"), "expected +N overflow badge: {row_last}");
    assert!(!row_last.contains("Parent"), "{row_last}");
    assert!(!row_last.contains("Refresh"), "{row_last}");

    // 2. Multi-line footer (footer_max_lines = 2) at 60 cols
    ts.state_mut().config.keymap.footer_max_lines = 2;
    let buf = ts.render(60, 24);
    let row_22 = row_text(&buf, 22);
    let row_23 = row_text(&buf, 23);
    assert!(row_22.contains("Filter"), "{row_22}");
    assert!(row_22.contains("Diff"), "{row_22}");
    assert!(!row_22.contains("Help"), "Help must sit on bottom row: {row_22}");
    assert!(row_23.contains("Help"), "{row_23}");
    assert!(row_23.contains("Copy hash"), "{row_23}");
    assert!(row_23.contains("Go to"), "{row_23}");
    assert!(row_23.contains(" +"), "expected +N on 2nd line: {row_23}");

    // 3. Multi-line footer (footer_max_lines = 3) at 60 cols
    ts.state_mut().config.keymap.footer_max_lines = 3;
    let buf = ts.render(60, 24);
    let combined_footer = format!(
        "{}\n{}\n{}",
        row_text(&buf, 21),
        row_text(&buf, 22),
        row_text(&buf, 23)
    );
    assert!(combined_footer.contains("Refresh"), "{combined_footer}");
    assert!(combined_footer.contains("Help"), "{combined_footer}");
    assert!(!combined_footer.contains(" +"), "unexpected +N when all fit: {combined_footer}");

    // 4. Hidden footer (footer_max_lines = 0): no key hints on row 23, but notifications still render
    ts.state_mut().config.keymap.footer_max_lines = 0;
    let buf = ts.render(120, 24);
    let hidden_row = row_text(&buf, 23);
    assert!(!hidden_row.contains("Filter"), "{hidden_row}");
    assert!(!hidden_row.contains("Help"), "{hidden_row}");

    ts.state_mut().notification = Some(crate::app::Notification {
        text: "Fetch finished".to_string(),
        is_error: false,
        action: None,
    });
    let buf = ts.render(120, 24);
    let notif_row = row_text(&buf, 23);
    assert!(notif_row.contains("Fetch finished"), "{notif_row}");
}

#[test]
fn footer_cross_scope_shadowing_does_not_leak_hidden_key() {
    let mut ts = TestSession::with_snapshot(snapshot());
    ts.state_mut().config.keymap = KeymapConfig::for_preset(KeymapPreset::Default);
    ts.dispatch(Action::SwitchTab(WorkspaceTab::Log));
    ts.dispatch(Action::FocusPane(PaneId::LogDetailsDiff));
    ts.state_mut().log.details.diff_preview = true;

    let ov: KeymapConfigOverlay = toml::from_str(
        r#"
[log]
prepend_keymap = [
    { on = "m", run = "choose_parent", show_hint = true },
]

[diff]
prepend_keymap = [
    { on = "m", run = "cycle_diff_dock", show_hint = false },
]
"#,
    )
    .unwrap();
    ts.state_mut().config.keymap.merge_overlay(&ov);

    let buf = ts.render(160, 40);
    let diff_footer = row_text(&buf, 39);
    assert!(!diff_footer.contains("Dock"), "{diff_footer}");
    assert!(!diff_footer.contains("Parent"), "shadowed log key leaked: {diff_footer}");
}

#[test]
fn footer_console_probe_vim_diff_noop_append_and_which_key_non_overlap() {
    // 1. Default ConsoleCommandList shows `p Status` without `Pull`;
    //    rebinding git_status_probe in [global] shows `g s Status` and unshadows `p Pull`.
    let mut console_ts = TestSession::with_snapshot(snapshot());
    console_ts.state_mut().config.keymap = KeymapConfig::for_preset(KeymapPreset::Default);
    console_ts.dispatch(Action::SwitchTab(WorkspaceTab::Console));
    console_ts.dispatch(Action::FocusPane(PaneId::ConsoleCommandList));
    let buf = console_ts.render(160, 30);
    let default_console_footer = row_text(&buf, 29);
    assert!(default_console_footer.contains(" p  Status"), "{default_console_footer}");
    assert!(!default_console_footer.contains("Pull"), "{default_console_footer}");

    let probe_ov: KeymapConfigOverlay = toml::from_str(
        r#"
[global]
prepend_keymap = [
    { on = ["g", "s"], run = "git_status_probe", show_hint = true },
]
"#,
    )
    .unwrap();
    console_ts.state_mut().config.keymap.merge_overlay(&probe_ov);
    let buf = console_ts.render(160, 30);
    let custom_console_footer = row_text(&buf, 29);
    assert!(custom_console_footer.contains(" g s  Status"), "{custom_console_footer}");
    assert!(custom_console_footer.contains(" p  Pull"), "{custom_console_footer}");

    // 2. Vim CommitDiffPreview shows `s Stage` and `u Unstage` without duplicate `Space Stage`
    let mut vim_ts = TestSession::with_snapshot(snapshot());
    vim_ts.state_mut().config.keymap = KeymapConfig::for_preset(KeymapPreset::Vim);
    vim_ts.dispatch(Action::SwitchTab(WorkspaceTab::Commit));
    vim_ts.dispatch(Action::FocusPane(PaneId::CommitDiffPreview));
    let buf = vim_ts.render(160, 30);
    let vim_diff_footer = row_text(&buf, 29);
    assert!(vim_diff_footer.contains(" s  Stage"), "{vim_diff_footer}");
    assert!(vim_diff_footer.contains(" u  Unstage"), "{vim_diff_footer}");
    assert_eq!(vim_diff_footer.matches("Stage").count(), 1, "{vim_diff_footer}");
    let space_actions = route_terminal_event(
        &Event::Key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE)),
        vim_ts.state(),
    );
    assert!(
        space_actions
            .iter()
            .any(|a| matches!(a, Action::StageDiffStageSelection)),
        "Space in Vim CommitDiffPreview should still stage: {space_actions:?}"
    );

    // 3. Unbinding `r` via noop in [global] shows surviving `F5 Refresh` in footer.
    let mut noop_ts = TestSession::with_snapshot(snapshot());
    noop_ts.state_mut().config.keymap = KeymapConfig::for_preset(KeymapPreset::Default);
    let noop_ov: KeymapConfigOverlay = toml::from_str(
        r#"
[global]
prepend_keymap = [
    { on = "r", run = "noop" },
]
"#,
    )
    .unwrap();
    noop_ts.state_mut().config.keymap.merge_overlay(&noop_ov);
    noop_ts.dispatch(Action::SwitchTab(WorkspaceTab::Log));
    noop_ts.dispatch(Action::FocusPane(PaneId::LogCommitTable));
    let buf = noop_ts.render(220, 30);
    let noop_footer = row_text(&buf, 29);
    assert!(noop_footer.contains(" F5  Refresh"), "{noop_footer}");

    // 4. [log] append_keymap alias preserves `y Copy hash` in its original declaration position.
    let mut append_ts = TestSession::with_snapshot(snapshot());
    append_ts.state_mut().config.keymap = KeymapConfig::for_preset(KeymapPreset::Default);
    append_ts.dispatch(Action::SwitchTab(WorkspaceTab::Log));
    append_ts.dispatch(Action::FocusPane(PaneId::LogCommitTable));
    let append_ov: KeymapConfigOverlay = toml::from_str(
        r#"
[log]
append_keymap = [
    { on = "Y", run = "copy_hash" },
]
"#,
    )
    .unwrap();
    append_ts.state_mut().config.keymap.merge_overlay(&append_ov);
    let buf = append_ts.render(200, 30);
    let append_footer = row_text(&buf, 29);
    let new_branch_pos = append_footer.find("Branch").expect(&append_footer);
    let copy_pos = append_footer.find(" y  Copy hash").expect(&append_footer);
    let goto_pos = append_footer.find("Go to").expect(&append_footer);
    assert!(new_branch_pos < copy_pos && copy_pos < goto_pos, "{append_footer}");

    // 5. Which-Key popup with footer_max_lines = 2 does NOT overwrite either footer row.
    let mut wk_ts = TestSession::with_snapshot(snapshot());
    wk_ts.state_mut().config.keymap = KeymapConfig::for_preset(KeymapPreset::Default);
    wk_ts.state_mut().config.keymap.footer_max_lines = 2;
    wk_ts.dispatch(Action::SwitchTab(WorkspaceTab::Log));
    wk_ts.dispatch(Action::FocusPane(PaneId::LogCommitTable));
    wk_ts.send_key(KeyCode::Char('g'), KeyModifiers::NONE);
    let buf = wk_ts.render(80, 24);
    let row_22 = row_text(&buf, 22);
    let row_23 = row_text(&buf, 23);
    assert!(row_22.contains("Filter") && row_22.contains("Copy hash"), "{row_22}");
    assert!(row_23.contains("Go to") && row_23.contains("Help"), "{row_23}");
    let above_footer = (0..22)
        .map(|y| row_text(&buf, y))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(above_footer.contains("g +"), "{above_footer}");
}

#[test]
fn stash_braces_popup_interior_click_esc_button_and_new_branch_shortcut() {
    // 1. Stash i18n formatting renders single braces `stash@{0}`, never `stash@{{0}}`.
    let zh = crate::i18n::I18n::load("zh-CN", None);
    assert_eq!(
        zh.git_op_label(&GitOp::StashApply {
            index: 0,
            pop: false
        }),
        "应用 stash@{0}"
    );
    let en = crate::i18n::I18n::load("en-US", None);
    assert_eq!(
        en.git_op_label(&GitOp::StashApply {
            index: 2,
            pop: true
        }),
        "Pop stash@{2}"
    );
    assert_eq!(
        zh.git_op_label(&GitOp::StashDrop { index: 1 }),
        "删除 stash@{1}"
    );

    // 2. Clicking inside ShortcutsHelp popup does NOT dismiss it; `[Esc]` or backdrop DOES.
    let mut ts = TestSession::with_snapshot(snapshot());
    ts.send_key(KeyCode::Char('?'), KeyModifiers::NONE);
    assert!(matches!(
        ts.state().modal_stack.last(),
        Some(ModalLayer::ShortcutsHelp { .. })
    ));
    let _ = ts.render(120, 40);
    let body_rect = ts.region_of(|t| matches!(t, HitTarget::DialogBody));
    ts.send_mouse(
        MouseEventKind::Down(MouseButton::Left),
        body_rect.x + 2,
        body_rect.y + 1,
    );
    assert!(
        matches!(ts.state().modal_stack.last(), Some(ModalLayer::ShortcutsHelp { .. })),
        "clicking inside ShortcutsHelp must not close it"
    );
    let esc_rect = ts.region_of(|t| matches!(t, HitTarget::ModalButton(ModalButton::Cancel)));
    ts.send_mouse(
        MouseEventKind::Down(MouseButton::Left),
        esc_rect.x + 1,
        esc_rect.y,
    );
    assert!(
        ts.state().modal_stack.is_empty(),
        "clicking [Esc] button should close ShortcutsHelp"
    );

    // 3. BranchesQuickView: `[Esc]` closes, backdrop closes, rebound NewBranch (`F7`) works inside popup.
    let mut bp_ts = TestSession::with_snapshot(snapshot());
    bp_ts.state_mut().config.keymap = KeymapConfig::for_preset(KeymapPreset::Default);
    let ov: KeymapConfigOverlay = toml::from_str(
        r#"
[branch]
prepend_keymap = [
    { on = [ "n" ], run = "noop" },
    { on = [ "<F7>" ], run = "new_branch", show_hint = true },
]
"#,
    )
    .unwrap();
    bp_ts.state_mut().config.keymap.merge_overlay(&ov);
    bp_ts.send_key(KeyCode::Char('b'), KeyModifiers::NONE);
    let buf = bp_ts.render(140, 40);
    let screen = screen_text(&buf);
    assert!(screen.contains("F7"), "popup should show F7 hint:\n{screen}");

    bp_ts.send_mouse(MouseEventKind::Down(MouseButton::Left), 130, 20);
    assert!(
        bp_ts.state().modal_stack.is_empty(),
        "clicking backdrop outside BranchesQuickView should close it"
    );

    bp_ts.send_key(KeyCode::Char('b'), KeyModifiers::NONE);
    bp_ts.send_key(KeyCode::F(7), KeyModifiers::NONE);
    assert!(
        matches!(bp_ts.state().modal_stack.last(), Some(ModalLayer::Dialog(_))),
        "pressing F7 in BranchesQuickView should open NewBranch dialog: {:?}",
        bp_ts.state().modal_stack
    );

    // 4. Cross-scope shadowing: [branch] binds <F7> to rename_branch while [log] binds <F7> to new_branch.
    let mut shadow_ts = TestSession::with_snapshot(snapshot());
    shadow_ts.state_mut().config.keymap = KeymapConfig::for_preset(KeymapPreset::Default);
    let shadow_ov: KeymapConfigOverlay = toml::from_str(
        r#"
[branch]
prepend_keymap = [
    { on = [ "<F7>" ], run = "rename_branch" },
]
[log]
prepend_keymap = [
    { on = [ "<F7>" ], run = "new_branch", show_hint = true },
]
"#,
    )
    .unwrap();
    shadow_ts.state_mut().config.keymap.merge_overlay(&shadow_ov);
    shadow_ts.send_key(KeyCode::Char('b'), KeyModifiers::NONE);
    let buf = shadow_ts.render(140, 40);
    let shadow_screen = screen_text(&buf);
    let shadow_new_branch_line = shadow_screen
        .lines()
        .find(|l| l.contains("New Branch"))
        .unwrap_or_default();
    assert!(
        shadow_new_branch_line.contains("Ctrl+Alt+N"),
        "shadowed F7 should fall back to Ctrl+Alt+N:\n{shadow_screen}"
    );
    assert!(
        !shadow_new_branch_line.contains("F7"),
        "shadowed F7 must not be advertised on New Branch:\n{shadow_screen}"
    );
    shadow_ts.send_key(KeyCode::F(7), KeyModifiers::NONE);
    assert!(
        !matches!(shadow_ts.state().modal_stack.last(), Some(ModalLayer::Dialog(_))),
        "shadowed F7 must not open NewBranch dialog: {:?}",
        shadow_ts.state().modal_stack
    );

    // 5. Unbinding <C-A-n> via noop hides Ctrl+Alt+N and disables it in BranchesQuickView.
    let mut unbind_ts = TestSession::with_snapshot(snapshot());
    unbind_ts.state_mut().config.keymap = KeymapConfig::for_preset(KeymapPreset::Default);
    let unbind_ov: KeymapConfigOverlay = toml::from_str(
        r#"
[branch]
prepend_keymap = [
    { on = [ "<C-A-n>" ], run = "noop" },
]
"#,
    )
    .unwrap();
    unbind_ts.state_mut().config.keymap.merge_overlay(&unbind_ov);
    unbind_ts.send_key(KeyCode::Char('b'), KeyModifiers::NONE);
    let buf = unbind_ts.render(140, 40);
    let unbind_screen = screen_text(&buf);
    assert!(
        !unbind_screen.contains("Ctrl+Alt+N"),
        "unbound Ctrl+Alt+N must not be shown:\n{unbind_screen}"
    );
    unbind_ts.send_key(
        KeyCode::Char('n'),
        KeyModifiers::CONTROL | KeyModifiers::ALT,
    );
    assert!(
        !matches!(unbind_ts.state().modal_stack.last(), Some(ModalLayer::Dialog(_))),
        "unbound Ctrl+Alt+N must not open NewBranch dialog: {:?}",
        unbind_ts.state().modal_stack
    );

    // 6. Dangerous ChoiceDialog interior clicks do not dismiss it, while clicking Cancel closes it.
    let mut crit_ts = TestSession::with_snapshot(snapshot());
    crit_ts.dispatch(Action::OpenDialog(Box::new(
        crate::dialogs::Dialog::Choice(crate::dialogs::ChoiceDialog::confirm(
            "Force Push",
            vec!["Overwrite remote history?".into()],
            "Force Push",
            "Cancel",
            Vec::new(),
        )),
    )));
    let _ = crit_ts.render(120, 40);
    let crit_body = crit_ts.region_of(|t| matches!(t, HitTarget::DialogBody));
    crit_ts.send_mouse(
        MouseEventKind::Down(MouseButton::Left),
        crit_body.x + 1,
        crit_body.y + 1,
    );
    assert!(
        matches!(crit_ts.state().modal_stack.last(), Some(ModalLayer::Dialog(_))),
        "clicking inside ChoiceDialog must not close it"
    );
    let crit_cancel = crit_ts.region_of(|t| matches!(t, HitTarget::DialogOption(1)));
    crit_ts.send_mouse(
        MouseEventKind::Down(MouseButton::Left),
        crit_cancel.x + 1,
        crit_cancel.y,
    );
    assert!(
        crit_ts.state().modal_stack.is_empty(),
        "clicking Cancel on ChoiceDialog should close it"
    );
}

#[test]
fn commit_editor_focus_bar_and_changes_tree_enter_diff_hint() {
    let mut ts = TestSession::with_snapshot(snapshot());
    ts.state_mut().config.keymap = KeymapConfig::for_preset(KeymapPreset::Default);
    ts.dispatch(Action::SwitchTab(WorkspaceTab::Commit));
    ts.deliver_engine_event(jbgit_engine::EngineEvent::StageStatusLoaded {
        status: Arc::new(jbgit_core::StageStatusSnapshot {
            unstaged: vec![jbgit_core::StageEntry {
                path: "src/lib.rs".into(),
                old_path: None,
                kind: jbgit_core::FileStatusKind::Modified,
            }],
            ..Default::default()
        }),
        epoch: 1,
    });

    // 1. On a container row (Unstaged group), footer does not show `Enter  Diff`.
    ts.dispatch(Action::FocusPane(PaneId::CommitChangesTree));
    ts.dispatch(Action::SelectCommitStageRow(0));
    let buf = ts.render(140, 40);
    let group_footer = row_text(&buf, 39);
    assert!(
        !group_footer.contains("Enter  Diff"),
        "group row must not advertise Enter Diff: {group_footer}"
    );

    // 2. On a file row with diff panel open, footer advertises `Enter  Diff`.
    let file_idx = ts
        .state()
        .stage_workbench
        .rows()
        .iter()
        .position(|r| !r.is_container())
        .expect("file row");
    ts.dispatch(Action::SelectCommitStageRow(file_idx));
    let buf = ts.render(140, 40);
    let file_footer = row_text(&buf, 39);
    assert!(
        file_footer.contains("Enter  Diff"),
        "file row should advertise Enter Diff: {file_footer}"
    );

    // 3. CommitMessageEditor displays focused accent bar `▏` only when focused.
    let ed_rect = ts.region_of(|t| matches!(t, HitTarget::CommitEditorArea));
    let unfocused_cell = &buf[(ed_rect.x, ed_rect.y + 1)];
    assert_ne!(unfocused_cell.symbol(), "▏");

    ts.dispatch(Action::FocusPane(PaneId::CommitMessageEditor));
    let buf = ts.render(140, 40);
    let focused_cell = &buf[(ed_rect.x, ed_rect.y + 1)];
    assert_eq!(focused_cell.symbol(), "▏");
    assert_eq!(focused_cell.fg, ts.state().theme.border_focused);
}

#[test]
fn command_catalog_keyboard_resolution_is_locale_and_mnemonic_independent_and_routing_is_pure() {
    use crate::keymap::{KeyAction, execute_action};

    let make = |lang: &str| {
        let mut ts = TestSession::with_snapshot(snapshot());
        ts.state_mut().i18n = Arc::new(crate::i18n::I18n::load(lang, None));
        ts
    };

    let mut en = make("en-US");
    let mut zh = make("zh-CN");

    // Log-commit commands resolve identical domain variants regardless of locale.
    let tag_of = |acts: Option<Vec<Action>>| -> Vec<String> {
        acts.unwrap_or_default()
            .into_iter()
            .map(|a| match a {
                Action::UncommitHead => "UncommitHead".into(),
                Action::RunGitOp(op) => format!("GitOp:{:?}", std::mem::discriminant(&op)),
                Action::OpenDialog(d) => format!("Dialog:{:?}", std::mem::discriminant(d.as_ref())),
                Action::PromptDeleteBranch { name } => format!("DeleteBranch:{name}"),
                other => format!("{other:?}"),
            })
            .collect()
    };

    for act in [
        KeyAction::Checkout,
        KeyAction::Uncommit,
        KeyAction::Reword,
        KeyAction::DropCommit,
    ] {
        assert_eq!(
            tag_of(execute_action(&act, en.state())),
            tag_of(execute_action(&act, zh.state())),
            "Log command {act:?} must be identical across en-US and zh-CN"
        );
    }

    // Branch-tree commands resolve identically across locales without scanning menu mnemonics.
    en.dispatch(Action::FocusPane(PaneId::LogBranchesTree));
    zh.dispatch(Action::FocusPane(PaneId::LogBranchesTree));
    if let Some(feature_row) = en
        .state()
        .log
        .branches
        .rows
        .iter()
        .position(|r| {
            en.state().log.branches.model.nodes[r.node]
                .ref_name
                .as_deref()
                == Some("feature")
        })
    {
        en.dispatch(Action::SelectBranchRow(feature_row));
        zh.dispatch(Action::SelectBranchRow(feature_row));
        for act in [
            KeyAction::Checkout,
            KeyAction::RenameBranch,
            KeyAction::DeleteBranch,
        ] {
            let en_tags = tag_of(execute_action(&act, en.state()));
            let zh_tags = tag_of(execute_action(&act, zh.state()));
            assert!(!en_tags.is_empty(), "Branch command {act:?} should resolve");
            assert_eq!(en_tags, zh_tags);
        }
    }

    // Pure routing: `route_terminal_event` borrows `&AppState` immutably and `ModalStack::pop`
    // automatically cleans up owned FileHistory / ConflictResolver / ThreeWayInspector / HookError.
    let immutable_ref: &crate::app::AppState = en.state();
    let routed = route_terminal_event(
        &Event::Key(KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE)),
        immutable_ref,
    );
    assert!(matches!(
        routed.as_slice(),
        [Action::OpenPopup {
            kind: PopupKind::ShortcutsHelp,
            ..
        }]
    ));

    en.dispatch(Action::OpenConflictResolver("a.rs".into()));
    assert!(en.state().modal_stack.conflict_merge().is_some());
    en.dispatch(Action::PopModal);
    assert!(en.state().modal_stack.conflict_merge().is_none());
}

#[test]
fn menu_shortcuts_omit_shadowed_and_noop_secondary_bindings() {
    let mut ts = TestSession::with_snapshot(snapshot());
    ts.state_mut().config.keymap = KeymapConfig::for_preset(KeymapPreset::Default);

    // Shadow `d` in [diff] with `next_hunk` and unbind `<Del>` in [diff] via `noop`
    // while keeping `<Del>` for DiscardChanges in [commit]:
    // `all_shortcuts_for_action` must NOT leak the shadowed `d`.
    let ov: KeymapConfigOverlay = toml::from_str(
        r#"
[diff]
prepend_keymap = [
    { on = "d", run = "next_hunk" },
    { on = "<Del>", run = "noop" },
]
"#,
    )
    .unwrap();
    ts.state_mut().config.keymap.merge_overlay(&ov);
    ts.dispatch(Action::SwitchTab(WorkspaceTab::Commit));
    ts.dispatch(Action::FocusPane(PaneId::CommitDiffPreview));
    let sc = crate::keymap::catalog::all_shortcuts_for_action(
        ts.state(),
        &[crate::config::KeymapScope::Commit],
        &KeyAction::DiscardChanges,
    );
    assert_eq!(
        sc.as_deref(),
        Some("Del"),
        "shadowed 'd' must not appear in multi-key menu hint"
    );

    // When `compare_or_diff` is shadowed in [log] by another action, `log_change_menu_items`
    // must NOT advertise the shadowed shortcut via raw `primary_key_for`.
    let log_ov: KeymapConfigOverlay = toml::from_str(
        r#"
[log]
prepend_keymap = [
    { on = "<C-d>", run = "quit" },
]
"#,
    )
    .unwrap();
    ts.state_mut().config.keymap.merge_overlay(&log_ov);
    ts.dispatch(Action::SwitchTab(WorkspaceTab::Log));
    ts.dispatch(Action::FocusPane(PaneId::LogChangesTree));
    let oid0 = ts.state().log_commit_at(0).unwrap().oid;
    ts.deliver_engine_event(jbgit_engine::EngineEvent::CommitDetailsLoaded {
        slot: jbgit_core::RequestSlotId::LogCommitDetails,
        bundle: Arc::new(jbgit_core::CommitChangesBundle {
            key: jbgit_core::DetailsKey::single(oid0),
            details: vec![super::make_details(oid0, vec![], "top")],
            changes: vec![jbgit_core::FileChange {
                path: "a.rs".into(),
                old_path: None,
                kind: jbgit_core::FileStatusKind::Modified,
                additions: 1,
                deletions: 0,
                is_binary: false,
                exists_in_workdir: true,
                old_oid: None,
                new_oid: None,
            }],
        }),
        epoch: 1,
    });
    let menu = crate::menus::log_change_menu_items(ts.state());
    let toggle_diff = find_item(&menu, "Toggle Diff Preview");
    assert_ne!(
        toggle_diff.shortcut.as_deref(),
        Some("Ctrl+D"),
        "shadowed Ctrl+D must not appear on log_change_menu_items"
    );
}

#[test]
fn commit_diff_preview_c_focuses_commit_message_editor() {
    let mut ts = TestSession::with_snapshot(snapshot());
    ts.dispatch(Action::SwitchTab(WorkspaceTab::Commit));
    ts.dispatch(Action::FocusPane(PaneId::CommitDiffPreview));
    assert_eq!(ts.state().focused_pane, PaneId::CommitDiffPreview);

    ts.send_key(KeyCode::Char('c'), KeyModifiers::NONE);
    assert_eq!(
        ts.state().focused_pane,
        PaneId::CommitMessageEditor,
        "pressing 'c' in CommitDiffPreview should focus CommitMessageEditor"
    );

    // Once inside CommitMessageEditor, 'c' must type text rather than re-trigger FocusCommitMsg.
    ts.send_key(KeyCode::Char('c'), KeyModifiers::NONE);
    assert_eq!(ts.state().stage_workbench.editor().text(), "c");
}

#[test]
fn annotate_alt_g_resolves_without_shadowing_across_all_presets() {
    use crate::config::KeymapPreset;
    use crate::keymap::{KeyAction, actions::check_scopes, catalog};

    for preset in [
        KeymapPreset::Default,
        KeymapPreset::Jetbrains,
        KeymapPreset::Vim,
    ] {
        let mut cfg = crate::config::AppConfig::load_defaults();
        cfg.keymap = crate::config::KeymapConfig::for_preset(preset.clone());
        let dir = tempfile::tempdir().unwrap();
        let mut ts = TestSession::with_config(dir.path().to_path_buf(), cfg);

        for (tab, pane) in [
            (WorkspaceTab::Log, PaneId::LogChangesTree),
            (WorkspaceTab::Log, PaneId::LogDetailsDiff),
            (WorkspaceTab::Commit, PaneId::CommitChangesTree),
            (WorkspaceTab::Commit, PaneId::CommitDiffPreview),
        ] {
            ts.dispatch(Action::SwitchTab(tab));
            ts.dispatch(Action::FocusPane(pane));
            let key = KeyEvent::new(KeyCode::Char('g'), KeyModifiers::ALT);
            let m = check_scopes(&[key], ts.state());
            assert!(
                matches!(m, crate::keymap::MatchResult::Exact(b) if b.run == KeyAction::Annotate),
                "expected <A-g> to match KeyAction::Annotate on {preset:?} / {tab:?} / {pane:?}"
            );
            let sc = catalog::shortcut_for_action(
                ts.state(),
                &crate::keymap::actions::scopes_for_context(ts.state()),
                &KeyAction::Annotate,
            );
            assert_eq!(
                sc.as_deref(),
                Some("Alt+G"),
                "expected non-shadowed Alt+G shortcut display on {preset:?} / {tab:?} / {pane:?}"
            );
        }
    }
}

#[test]
fn terminal_too_small_guard_matches_min_threshold_and_i18n_text() {
    use crate::views::{MIN_TERMINAL_HEIGHT, MIN_TERMINAL_WIDTH};

    let mut ts = TestSession::with_snapshot(snapshot());
    let expected_en = format!("(min {MIN_TERMINAL_WIDTH}x{MIN_TERMINAL_HEIGHT})");
    let expected_zh = format!("(最小要求{MIN_TERMINAL_WIDTH}x{MIN_TERMINAL_HEIGHT})");

    // Below MIN_TERMINAL_WIDTH x MIN_TERMINAL_HEIGHT renders the too-small guard in en-US and zh-CN.
    for (w, h) in [
        (35, 8),
        (MIN_TERMINAL_WIDTH - 1, MIN_TERMINAL_HEIGHT),
        (MIN_TERMINAL_WIDTH, MIN_TERMINAL_HEIGHT - 1),
    ] {
        ts.state_mut().i18n = Arc::new(crate::i18n::I18n::load("en-US", None));
        let en_buf = ts.render(w, h);
        let en_txt = screen_text(&en_buf);
        assert!(en_txt.contains("Terminal too small"), "{w}x{h}: {en_txt}");
        assert!(en_txt.contains(&expected_en), "{w}x{h}: {en_txt}");

        ts.state_mut().i18n = Arc::new(crate::i18n::I18n::load("zh-CN", None));
        let zh_buf = ts.render(w, h);
        let zh_txt = screen_text(&zh_buf);
        let zh_compact = zh_txt.replace(' ', "");
        assert!(zh_compact.contains("终端窗口过小"), "{w}x{h}: {zh_txt}");
        assert!(zh_compact.contains(&expected_zh), "{w}x{h}: {zh_txt}");
    }

    // At or above MIN_TERMINAL_WIDTH x MIN_TERMINAL_HEIGHT renders the normal UI without the guard.
    ts.state_mut().i18n = Arc::new(crate::i18n::I18n::load("en-US", None));
    for (w, h) in [(MIN_TERMINAL_WIDTH, MIN_TERMINAL_HEIGHT), (50, 16)] {
        let buf = ts.render(w, h);
        let txt = screen_text(&buf);
        assert!(!txt.contains("Terminal too small"), "{w}x{h}: {txt}");
    }
}

#[test]
fn commit_message_editor_shows_prominent_edit_mode_and_esc_exit_indicators() {
    let mut ts = TestSession::with_snapshot(snapshot());
    ts.dispatch(Action::SwitchTab(WorkspaceTab::Commit));
    ts.dispatch(Action::FocusPane(PaneId::CommitChangesTree));

    // Unfocused: ruler row must not display EDIT badge.
    let buf_unfocused = ts.render(140, 40);
    let ed_rect = ts.region_of(|t| matches!(t, HitTarget::CommitEditorArea));
    let ruler_unfocused = row_text(&buf_unfocused, ed_rect.y);
    assert!(!ruler_unfocused.contains("EDIT"), "{ruler_unfocused}");

    // Focused in en-US: ruler shows EDIT badge; footer leads with emphasized Esc Exit edit pill.
    ts.dispatch(Action::FocusPane(PaneId::CommitMessageEditor));
    let buf_en = ts.render(140, 40);
    let ruler_en = row_text(&buf_en, ed_rect.y);
    assert!(ruler_en.contains("EDIT"), "{ruler_en}");

    let footer_en = row_text(&buf_en, 39);
    assert!(footer_en.contains("Esc"), "{footer_en}");
    assert!(footer_en.contains("Exit edit"), "{footer_en}");

    // Verify Esc key badge cell uses border_focused background and fg_bright foreground.
    let esc_col = footer_en.find(" Esc ").expect(&footer_en) as u16;
    let esc_cell = &buf_en[(esc_col + 1, 39)];
    assert_eq!(esc_cell.bg, ts.state().theme.border_focused);
    assert_eq!(esc_cell.fg, ts.state().theme.fg_bright);

    // Focused in zh-CN: ruler shows 编辑中 and footer shows 退出编辑.
    ts.state_mut().i18n = Arc::new(crate::i18n::I18n::load("zh-CN", None));
    let buf_zh = ts.render(140, 40);
    let ruler_zh = row_text(&buf_zh, ed_rect.y);
    assert!(ruler_zh.replace(' ', "").contains("编辑中"), "{ruler_zh}");
    let footer_zh = row_text(&buf_zh, 39);
    assert!(footer_zh.contains("Esc"), "{footer_zh}");
    assert!(footer_zh.replace(' ', "").contains("退出编辑"), "{footer_zh}");
}
