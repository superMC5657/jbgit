use std::sync::Arc;

use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEventKind};
use jbgit_core::{GitOp, ModalButton, PaneId, WorkspaceTab};
use jbgit_engine::{StageDiffKey, StageDiffSide};
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

use super::{find_item, key, mouse, oid, region_of, render, row_text, screen_text, snapshot, state_with, type_str};

#[test]
fn dynamic_footer_tracks_focus_and_keymap_overrides() {
    let mut state = state_with(snapshot());
    state.config.keymap = KeymapConfig::for_preset(KeymapPreset::Default);
    state.dispatch(Action::SwitchTab(WorkspaceTab::Log));
    state.dispatch(Action::FocusPane(PaneId::LogCommitTable));
    let term = render(&mut state, 140, 40);
    let log_footer = screen_text(term.backend().buffer());
    assert!(log_footer.contains("Filter"), "{log_footer}");
    assert!(log_footer.contains("Checkout"), "{log_footer}");
    assert!(log_footer.contains("Help"), "{log_footer}");

    state.dispatch(Action::SwitchTab(WorkspaceTab::Commit));
    state.dispatch(Action::FocusPane(PaneId::CommitChangesTree));
    let term = render(&mut state, 140, 40);
    let commit_footer = screen_text(term.backend().buffer());
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
    state.config.keymap.merge_overlay(&ov);
    let term = render(&mut state, 140, 40);
    let updated_footer = screen_text(term.backend().buffer());
    assert!(updated_footer.contains(" S "), "{updated_footer}");
}

#[test]
fn footer_hints_match_active_mode_dock_stash_and_console_probe() {
    let mut state = state_with(snapshot());
    state.config.keymap = KeymapConfig::for_preset(KeymapPreset::Default);

    // 1. LogDetailsDiff in Commit Details mode (diff_preview = false) must NOT show diff-only hints
    state.dispatch(Action::SwitchTab(WorkspaceTab::Log));
    state.dispatch(Action::FocusPane(PaneId::LogDetailsDiff));
    state.details.diff_preview = false;
    let term = render(&mut state, 140, 40);
    let details_footer = row_text(term.backend().buffer(), 39);
    assert!(details_footer.contains("Diff"), "{details_footer}");
    assert!(!details_footer.contains("Dock"), "{details_footer}");
    assert!(!details_footer.contains("Split"), "{details_footer}");
    assert!(!details_footer.contains("Context"), "{details_footer}");
    assert!(!details_footer.contains("Hunk"), "{details_footer}");

    // 2. LogDetailsDiff in Diff Preview mode (diff_preview = true) shows core diff hints
    //    while keeping rarely used layout toggles hidden unless opted in via show_hint = true.
    state.details.diff_preview = true;
    let term = render(&mut state, 140, 40);
    let diff_footer = row_text(term.backend().buffer(), 39);
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
    let mut opt_in_state = state_with(snapshot());
    opt_in_state.config.keymap = KeymapConfig::for_preset(KeymapPreset::Default);
    opt_in_state.config.keymap.merge_overlay(&dock_ov);
    opt_in_state.dispatch(Action::SwitchTab(WorkspaceTab::Log));
    opt_in_state.dispatch(Action::FocusPane(PaneId::LogDetailsDiff));
    opt_in_state.details.diff_preview = true;
    let term = render(&mut opt_in_state, 140, 40);
    let opt_in_footer = row_text(term.backend().buffer(), 39);
    assert!(opt_in_footer.contains(" m "), "{opt_in_footer}");
    assert!(opt_in_footer.contains("Dock"), "{opt_in_footer}");

    // 3. CommitDiffPreview shows Stage / Hunk / Zoom / Back (`Esc`) without rare `Context`
    state.dispatch(Action::SwitchTab(WorkspaceTab::Commit));
    state.dispatch(Action::FocusPane(PaneId::CommitDiffPreview));
    let term = render(&mut state, 160, 40);
    let commit_diff_footer = row_text(term.backend().buffer(), 39);
    assert!(commit_diff_footer.contains("Stage"), "{commit_diff_footer}");
    assert!(commit_diff_footer.contains("Hunk"), "{commit_diff_footer}");
    assert!(commit_diff_footer.contains("Esc"), "{commit_diff_footer}");
    assert!(!commit_diff_footer.contains("Context"), "{commit_diff_footer}");

    // 4. CommitMessageEditor shows Commit / Amend / Esc without rare `Sign-off`
    state.dispatch(Action::FocusPane(PaneId::CommitMessageEditor));
    let term = render(&mut state, 140, 40);
    let msg_footer = row_text(term.backend().buffer(), 39);
    assert!(msg_footer.contains("Commit"), "{msg_footer}");
    assert!(msg_footer.contains("Amend"), "{msg_footer}");
    assert!(!msg_footer.contains("Sign-off"), "{msg_footer}");

    // 5. StashDiffPreview does NOT advertise ghost Zoom and shows Hunk / File without rare `Group`
    state.dispatch(Action::SwitchTab(WorkspaceTab::Stash));
    state.dispatch(Action::FocusPane(PaneId::StashDiffPreview));
    let term = render(&mut state, 140, 40);
    let stash_footer = row_text(term.backend().buffer(), 39);
    assert!(!stash_footer.contains("Zoom"), "{stash_footer}");
    assert!(stash_footer.contains("Hunk"), "{stash_footer}");
    assert!(stash_footer.contains("File"), "{stash_footer}");
    assert!(!stash_footer.contains("Group"), "{stash_footer}");
    key(&mut state, KeyCode::Char('z'), KeyModifiers::NONE);
    assert!(!state.details.diff_zoomed);

    // 6. ConsoleCommandList `p` actually triggers status probe instead of global git_pull
    state.dispatch(Action::SwitchTab(WorkspaceTab::Console));
    state.dispatch(Action::FocusPane(PaneId::ConsoleCommandList));
    let term = render(&mut state, 140, 40);
    let console_footer = row_text(term.backend().buffer(), 39);
    assert!(console_footer.contains(" p "), "{console_footer}");
    assert!(console_footer.contains("Status"), "{console_footer}");
    let actions = route_terminal_event(
        Event::Key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::NONE)),
        &mut state,
    );
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, Action::RunCliProbe(args) if args.first().map(String::as_str) == Some("status"))),
        "expected RunCliProbe(status), got {actions:?}"
    );
}

#[test]
fn which_key_popup_appears_on_prefix_and_dismisses() {
    let mut state = state_with(snapshot());
    state.dispatch(Action::SwitchTab(WorkspaceTab::Log));
    state.dispatch(Action::FocusPane(PaneId::LogCommitTable));

    key(&mut state, KeyCode::Char('g'), KeyModifiers::NONE);
    assert_eq!(state.pending_keys.len(), 1);
    let term = render(&mut state, 140, 40);
    let txt = screen_text(term.backend().buffer());
    assert!(txt.contains("g +"), "{txt}");
    assert!(txt.contains("Esc cancel"), "{txt}");

    // Second key completes the sequence and closes Which-Key.
    key(&mut state, KeyCode::Char('g'), KeyModifiers::NONE);
    assert!(state.pending_keys.is_empty());
    let term = render(&mut state, 140, 40);
    let txt_after = screen_text(term.backend().buffer());
    assert!(!txt_after.contains("g +"), "{txt_after}");

    // Pressing Esc cancels a pending chord cleanly.
    key(&mut state, KeyCode::Char('g'), KeyModifiers::NONE);
    assert_eq!(state.pending_keys.len(), 1);
    key(&mut state, KeyCode::Esc, KeyModifiers::NONE);
    assert!(state.pending_keys.is_empty());
}

#[test]
fn shortcuts_help_palette_filters_and_executes_selected_action() {
    let mut state = state_with(snapshot());
    state.dispatch(Action::SwitchTab(WorkspaceTab::Log));
    state.dispatch(Action::FocusPane(PaneId::LogCommitTable));

    key(&mut state, KeyCode::Char('?'), KeyModifiers::SHIFT);
    assert!(matches!(
        state.modal_stack.last(),
        Some(ModalLayer::FloatingPopup {
            kind: PopupKind::ShortcutsHelp,
            ..
        })
    ));

    type_str(&mut state, "palette");
    let term = render(&mut state, 120, 40);
    let txt = screen_text(term.backend().buffer());
    assert!(txt.contains("palette"), "{txt}");

    key(&mut state, KeyCode::Enter, KeyModifiers::NONE);
    assert!(matches!(
        state.modal_stack.last(),
        Some(ModalLayer::FloatingPopup {
            kind: PopupKind::PalettePreview,
            ..
        })
    ));
}

#[test]
fn jetbrains_tree_speed_search_preserves_context_menu_dot_and_vim_diff_menu_shows_su() {
    // 1. JetBrains preset: `.` on LogBranchesTree opens context menu instead of speed search.
    let mut state = state_with(snapshot());
    state.config.keymap = KeymapConfig::for_preset(KeymapPreset::Jetbrains);
    state.dispatch(Action::SwitchTab(WorkspaceTab::Log));
    state.dispatch(Action::FocusPane(PaneId::LogBranchesTree));
    let _ = render(&mut state, 140, 40);

    key(&mut state, KeyCode::Char('.'), KeyModifiers::NONE);
    assert!(
        matches!(state.modal_stack.last(), Some(ModalLayer::ContextMenu { .. })),
        "expected context menu on '.' under jetbrains preset"
    );
    state.dispatch(Action::PopContextMenus);

    type_str(&mut state, "main");
    assert_eq!(state.branches.query, "main");

    // 2. Vim preset: stage_diff_menu_items displays `s` and `u` separately.
    state.config.keymap = KeymapConfig::for_preset(KeymapPreset::Vim);
    let diff_menu = crate::menus::stage_diff_menu_items(&state);
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
    let mut state = state_with(snapshot());
    state.i18n = Arc::new(crate::i18n::I18n::load("zh-CN", None));

    // Reset dialog flags must start at the exact same display column in zh-CN.
    let dlg = crate::dialogs::reset_dialog(oid(9), "修复对齐", "main", &state.i18n);
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
        scroll_metrics: None,
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
    let mut state = state_with(snapshot());
    state.dispatch(Action::SwitchTab(WorkspaceTab::Commit));
    state.dispatch(Action::FocusPane(PaneId::CommitDiffPreview));
    state.stage_workbench.current_diff_key = Some(StageDiffKey {
        path: "a.rs".into(),
        old_path: None,
        side: StageDiffSide::AmendCommit,
        context: jbgit_core::DiffContext::Three,
    });
    let term = render(&mut state, 160, 40);
    let amend_footer = row_text(term.backend().buffer(), 39);
    assert!(amend_footer.contains("Hunk"), "{amend_footer}");
    assert!(amend_footer.contains("File"), "{amend_footer}");
    assert!(amend_footer.contains("Esc"), "{amend_footer}");
    assert!(!amend_footer.contains("Stage"), "{amend_footer}");
    assert!(!amend_footer.contains("Rollback"), "{amend_footer}");
    assert!(!amend_footer.contains("Visual"), "{amend_footer}");
    assert!(!amend_footer.contains("3-Way"), "{amend_footer}");
    let help_rows = build_help_rows(&state, "");
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
    state.dispatch(Action::FocusPane(PaneId::CommitMessageEditor));
    state.stage_workbench.editing_author = true;
    let term = render(&mut state, 160, 40);
    let author_footer = row_text(term.backend().buffer(), 39);
    assert!(author_footer.contains("Enter/Esc"), "{author_footer}");
    assert!(author_footer.contains("Apply"), "{author_footer}");
    assert!(!author_footer.contains("Back"), "{author_footer}");

    // 3. LogBranchesTree under JetBrains preset (tree_speed_search = true) hides captured single letters
    let mut jb_state = state_with(snapshot());
    jb_state.config.keymap = KeymapConfig::for_preset(KeymapPreset::Jetbrains);
    jb_state.dispatch(Action::SwitchTab(WorkspaceTab::Log));
    jb_state.dispatch(Action::FocusPane(PaneId::LogBranchesTree));
    let term = render(&mut jb_state, 160, 40);
    let jb_footer = row_text(term.backend().buffer(), 39);
    assert!(jb_footer.contains("Type"), "{jb_footer}");
    assert!(jb_footer.contains("Ctrl+F"), "{jb_footer}");
    assert!(jb_footer.contains("Ctrl+T"), "{jb_footer}");
    assert!(jb_footer.contains("Del"), "{jb_footer}");
    assert!(!jb_footer.contains("Checkout"), "{jb_footer}");
    assert!(!jb_footer.contains("Fetch"), "{jb_footer}");

    // 4. ConsoleOutputDetail help does not advertise GitStatusProbe and `p` does not trigger probe
    let mut console_state = state_with(snapshot());
    console_state.dispatch(Action::SwitchTab(WorkspaceTab::Console));
    console_state.dispatch(Action::FocusPane(PaneId::ConsoleOutputDetail));
    let detail_help = build_help_rows(&console_state, "");
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
    let detail_p_actions = route_terminal_event(
        Event::Key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::NONE)),
        &mut console_state,
    );
    assert!(
        !detail_p_actions
            .iter()
            .any(|a| matches!(a, Action::RunCliProbe(_))),
        "p in ConsoleOutputDetail must not run RunCliProbe: {detail_p_actions:?}"
    );
}

#[test]
fn footer_selective_show_hint_and_declaration_order() {
    let mut state = state_with(snapshot());
    state.config.keymap = KeymapConfig::for_preset(KeymapPreset::Default);
    state.dispatch(Action::SwitchTab(WorkspaceTab::Log));
    state.dispatch(Action::FocusPane(PaneId::LogCommitTable));

    // Default Log footer places Log scope before Global scope
    let term = render(&mut state, 200, 30);
    let default_footer = row_text(term.backend().buffer(), 29);
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
    state.config.keymap.merge_overlay(&ov);

    let term = render(&mut state, 200, 30);
    let updated_footer = row_text(term.backend().buffer(), 29);
    assert!(updated_footer.contains("Uncommit"), "{updated_footer}");
    assert!(!updated_footer.contains("Copy hash"), "{updated_footer}");
    assert!(!updated_footer.contains("Help"), "{updated_footer}");
    let uncommit_pos = updated_footer.find("Uncommit").expect(&updated_footer);
    let filter_pos2 = updated_footer.find("Filter").expect(&updated_footer);
    assert!(uncommit_pos < filter_pos2, "{updated_footer}");

    // Reordering entries in [log] changes left-to-right order in footer
    let mut state_reordered = state_with(snapshot());
    state_reordered.config.keymap = KeymapConfig::for_preset(KeymapPreset::Default);
    state_reordered.dispatch(Action::SwitchTab(WorkspaceTab::Log));
    state_reordered.dispatch(Action::FocusPane(PaneId::LogCommitTable));
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
    state_reordered.config.keymap.merge_overlay(&reorder_ov);
    let term = render(&mut state_reordered, 160, 30);
    let reordered_footer = row_text(term.backend().buffer(), 29);
    let copy_pos = reordered_footer.find("Copy hash").expect(&reordered_footer);
    let checkout_pos = reordered_footer.find("Checkout").expect(&reordered_footer);
    assert!(copy_pos < checkout_pos, "{reordered_footer}");
}

#[test]
fn footer_overflow_single_line_multiline_and_hidden() {
    let mut state = state_with(snapshot());
    state.config.keymap = KeymapConfig::for_preset(KeymapPreset::Default);
    state.dispatch(Action::SwitchTab(WorkspaceTab::Log));
    state.dispatch(Action::FocusPane(PaneId::LogCommitTable));

    // 1. Default footer_max_lines = 1 at narrow width (80 cols): shows `+N` and right-aligned `? Help`
    state.config.keymap.footer_max_lines = 1;
    let term = render(&mut state, 80, 24);
    let row_last = row_text(term.backend().buffer(), 23);
    assert!(row_last.contains("Filter"), "{row_last}");
    assert!(row_last.contains("Help"), "{row_last}");
    assert!(row_last.contains(" +"), "expected +N overflow badge: {row_last}");
    assert!(!row_last.contains("Parent"), "{row_last}");
    assert!(!row_last.contains("Refresh"), "{row_last}");

    // 2. Multi-line footer (footer_max_lines = 2) at 60 cols
    state.config.keymap.footer_max_lines = 2;
    let term = render(&mut state, 60, 24);
    let row_22 = row_text(term.backend().buffer(), 22);
    let row_23 = row_text(term.backend().buffer(), 23);
    assert!(row_22.contains("Filter"), "{row_22}");
    assert!(row_22.contains("Diff"), "{row_22}");
    assert!(!row_22.contains("Help"), "Help must sit on bottom row: {row_22}");
    assert!(row_23.contains("Help"), "{row_23}");
    assert!(row_23.contains("Copy hash"), "{row_23}");
    assert!(row_23.contains("Go to"), "{row_23}");
    assert!(row_23.contains(" +"), "expected +N on 2nd line: {row_23}");

    // 3. Multi-line footer (footer_max_lines = 3) at 60 cols
    state.config.keymap.footer_max_lines = 3;
    let term = render(&mut state, 60, 24);
    let combined_footer = format!(
        "{}\n{}\n{}",
        row_text(term.backend().buffer(), 21),
        row_text(term.backend().buffer(), 22),
        row_text(term.backend().buffer(), 23)
    );
    assert!(combined_footer.contains("Refresh"), "{combined_footer}");
    assert!(combined_footer.contains("Help"), "{combined_footer}");
    assert!(!combined_footer.contains(" +"), "unexpected +N when all fit: {combined_footer}");

    // 4. Hidden footer (footer_max_lines = 0): no key hints on row 23, but notifications still render
    state.config.keymap.footer_max_lines = 0;
    let term = render(&mut state, 120, 24);
    let hidden_row = row_text(term.backend().buffer(), 23);
    assert!(!hidden_row.contains("Filter"), "{hidden_row}");
    assert!(!hidden_row.contains("Help"), "{hidden_row}");

    state.notification = Some(crate::app::Notification {
        text: "Fetch finished".to_string(),
        is_error: false,
        action: None,
    });
    let term = render(&mut state, 120, 24);
    let notif_row = row_text(term.backend().buffer(), 23);
    assert!(notif_row.contains("Fetch finished"), "{notif_row}");
}

#[test]
fn footer_cross_scope_shadowing_does_not_leak_hidden_key() {
    let mut state = state_with(snapshot());
    state.config.keymap = KeymapConfig::for_preset(KeymapPreset::Default);
    state.dispatch(Action::SwitchTab(WorkspaceTab::Log));
    state.dispatch(Action::FocusPane(PaneId::LogDetailsDiff));
    state.details.diff_preview = true;

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
    state.config.keymap.merge_overlay(&ov);

    let term = render(&mut state, 160, 40);
    let diff_footer = row_text(term.backend().buffer(), 39);
    assert!(!diff_footer.contains("Dock"), "{diff_footer}");
    assert!(!diff_footer.contains("Parent"), "shadowed log key leaked: {diff_footer}");
}

#[test]
fn footer_console_probe_vim_diff_noop_append_and_which_key_non_overlap() {
    // 1. Default ConsoleCommandList shows `p Status` without `Pull`;
    //    rebinding git_status_probe in [global] shows `g s Status` and unshadows `p Pull`.
    let mut console_state = state_with(snapshot());
    console_state.config.keymap = KeymapConfig::for_preset(KeymapPreset::Default);
    console_state.dispatch(Action::SwitchTab(WorkspaceTab::Console));
    console_state.dispatch(Action::FocusPane(PaneId::ConsoleCommandList));
    let term = render(&mut console_state, 160, 30);
    let default_console_footer = row_text(term.backend().buffer(), 29);
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
    console_state.config.keymap.merge_overlay(&probe_ov);
    let term = render(&mut console_state, 160, 30);
    let custom_console_footer = row_text(term.backend().buffer(), 29);
    assert!(custom_console_footer.contains(" g s  Status"), "{custom_console_footer}");
    assert!(custom_console_footer.contains(" p  Pull"), "{custom_console_footer}");

    // 2. Vim CommitDiffPreview shows `s Stage` and `u Unstage` without duplicate `Space Stage`
    let mut vim_state = state_with(snapshot());
    vim_state.config.keymap = KeymapConfig::for_preset(KeymapPreset::Vim);
    vim_state.dispatch(Action::SwitchTab(WorkspaceTab::Commit));
    vim_state.dispatch(Action::FocusPane(PaneId::CommitDiffPreview));
    let term = render(&mut vim_state, 160, 30);
    let vim_diff_footer = row_text(term.backend().buffer(), 29);
    assert!(vim_diff_footer.contains(" s  Stage"), "{vim_diff_footer}");
    assert!(vim_diff_footer.contains(" u  Unstage"), "{vim_diff_footer}");
    assert_eq!(vim_diff_footer.matches("Stage").count(), 1, "{vim_diff_footer}");
    let space_actions = route_terminal_event(
        Event::Key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE)),
        &mut vim_state,
    );
    assert!(
        space_actions
            .iter()
            .any(|a| matches!(a, Action::StageDiffStageSelection)),
        "Space in Vim CommitDiffPreview should still stage: {space_actions:?}"
    );

    // 3. Unbinding `r` via noop in [global] shows surviving `F5 Refresh` in footer.
    let mut noop_state = state_with(snapshot());
    noop_state.config.keymap = KeymapConfig::for_preset(KeymapPreset::Default);
    let noop_ov: KeymapConfigOverlay = toml::from_str(
        r#"
[global]
prepend_keymap = [
    { on = "r", run = "noop" },
]
"#,
    )
    .unwrap();
    noop_state.config.keymap.merge_overlay(&noop_ov);
    noop_state.dispatch(Action::SwitchTab(WorkspaceTab::Log));
    noop_state.dispatch(Action::FocusPane(PaneId::LogCommitTable));
    let term = render(&mut noop_state, 220, 30);
    let noop_footer = row_text(term.backend().buffer(), 29);
    assert!(noop_footer.contains(" F5  Refresh"), "{noop_footer}");

    // 4. [log] append_keymap alias preserves `y Copy hash` in its original declaration position.
    let mut append_state = state_with(snapshot());
    append_state.config.keymap = KeymapConfig::for_preset(KeymapPreset::Default);
    append_state.dispatch(Action::SwitchTab(WorkspaceTab::Log));
    append_state.dispatch(Action::FocusPane(PaneId::LogCommitTable));
    let append_ov: KeymapConfigOverlay = toml::from_str(
        r#"
[log]
append_keymap = [
    { on = "Y", run = "copy_hash" },
]
"#,
    )
    .unwrap();
    append_state.config.keymap.merge_overlay(&append_ov);
    let term = render(&mut append_state, 200, 30);
    let append_footer = row_text(term.backend().buffer(), 29);
    let new_branch_pos = append_footer.find("Branch").expect(&append_footer);
    let copy_pos = append_footer.find(" y  Copy hash").expect(&append_footer);
    let goto_pos = append_footer.find("Go to").expect(&append_footer);
    assert!(new_branch_pos < copy_pos && copy_pos < goto_pos, "{append_footer}");

    // 5. Which-Key popup with footer_max_lines = 2 does NOT overwrite either footer row.
    let mut wk_state = state_with(snapshot());
    wk_state.config.keymap = KeymapConfig::for_preset(KeymapPreset::Default);
    wk_state.config.keymap.footer_max_lines = 2;
    wk_state.dispatch(Action::SwitchTab(WorkspaceTab::Log));
    wk_state.dispatch(Action::FocusPane(PaneId::LogCommitTable));
    key(&mut wk_state, KeyCode::Char('g'), KeyModifiers::NONE);
    let term = render(&mut wk_state, 80, 24);
    let buf = term.backend().buffer();
    let row_22 = row_text(buf, 22);
    let row_23 = row_text(buf, 23);
    assert!(row_22.contains("Filter") && row_22.contains("Copy hash"), "{row_22}");
    assert!(row_23.contains("Go to") && row_23.contains("Help"), "{row_23}");
    let above_footer = (0..22)
        .map(|y| row_text(buf, y))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(above_footer.contains("g +"), "{above_footer}");
}

#[test]
fn stash_braces_popup_interior_click_esc_button_and_new_branch_shortcut() {
    // 1. Stash i18n formatting renders single braces `stash@{0}`, never `stash@{{0}}`.
    let zh = crate::i18n::I18n::load("zh-CN", None);
    let en = crate::i18n::I18n::load("en-US", None);
    assert_eq!(
        zh.git_op_label(&GitOp::StashApply {
            index: 0,
            pop: false
        }),
        "应用 stash@{0}"
    );
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
    let mut state = state_with(snapshot());
    key(&mut state, KeyCode::Char('?'), KeyModifiers::NONE);
    assert!(matches!(
        state.modal_stack.last(),
        Some(ModalLayer::FloatingPopup {
            kind: PopupKind::ShortcutsHelp,
            ..
        })
    ));
    let _ = render(&mut state, 120, 40);
    let body_rect = region_of(&state, |t| matches!(t, HitTarget::DialogBody));
    mouse(
        &mut state,
        MouseEventKind::Down(MouseButton::Left),
        body_rect.x + 2,
        body_rect.y + 1,
    );
    assert!(
        matches!(
            state.modal_stack.last(),
            Some(ModalLayer::FloatingPopup {
                kind: PopupKind::ShortcutsHelp,
                ..
            })
        ),
        "clicking inside ShortcutsHelp must not close it"
    );
    let esc_rect = region_of(&state, |t| matches!(t, HitTarget::ModalButton(ModalButton::Cancel)));
    mouse(
        &mut state,
        MouseEventKind::Down(MouseButton::Left),
        esc_rect.x + 1,
        esc_rect.y,
    );
    assert!(
        state.modal_stack.is_empty(),
        "clicking [Esc] button should close ShortcutsHelp"
    );

    // 3. BranchesQuickView: `[Esc]` closes, backdrop closes, rebound NewBranch (`F7`) works inside popup.
    let mut bp_state = state_with(snapshot());
    bp_state.config.keymap = KeymapConfig::for_preset(KeymapPreset::Default);
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
    bp_state.config.keymap.merge_overlay(&ov);
    key(&mut bp_state, KeyCode::Char('b'), KeyModifiers::NONE);
    let term = render(&mut bp_state, 140, 40);
    let screen = screen_text(term.backend().buffer());
    assert!(screen.contains("F7"), "popup should show F7 hint:\n{screen}");

    mouse(
        &mut bp_state,
        MouseEventKind::Down(MouseButton::Left),
        130,
        20,
    );
    assert!(
        bp_state.modal_stack.is_empty(),
        "clicking backdrop outside BranchesQuickView should close it"
    );

    key(&mut bp_state, KeyCode::Char('b'), KeyModifiers::NONE);
    let _ = render(&mut bp_state, 140, 40);
    let bp_esc = region_of(&bp_state, |t| {
        matches!(t, HitTarget::ModalButton(ModalButton::Cancel))
    });
    mouse(
        &mut bp_state,
        MouseEventKind::Down(MouseButton::Left),
        bp_esc.x + 1,
        bp_esc.y,
    );
    assert!(
        bp_state.modal_stack.is_empty(),
        "clicking [Esc] inside BranchesQuickView should close it"
    );

    key(&mut bp_state, KeyCode::Char('b'), KeyModifiers::NONE);
    key(&mut bp_state, KeyCode::F(7), KeyModifiers::NONE);
    assert!(
        matches!(bp_state.modal_stack.last(), Some(ModalLayer::Dialog(_))),
        "pressing F7 in BranchesQuickView should open NewBranch dialog: {:?}",
        bp_state.modal_stack
    );

    // 4. Cross-scope shadowing: [branch] binds <F7> to rename_branch while [log] binds <F7> to new_branch.
    let mut shadow_state = state_with(snapshot());
    shadow_state.config.keymap = KeymapConfig::for_preset(KeymapPreset::Default);
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
    shadow_state.config.keymap.merge_overlay(&shadow_ov);
    key(&mut shadow_state, KeyCode::Char('b'), KeyModifiers::NONE);
    let term = render(&mut shadow_state, 140, 40);
    let shadow_screen = screen_text(term.backend().buffer());
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
    key(&mut shadow_state, KeyCode::F(7), KeyModifiers::NONE);
    assert!(
        !matches!(shadow_state.modal_stack.last(), Some(ModalLayer::Dialog(_))),
        "shadowed F7 must not open NewBranch dialog: {:?}",
        shadow_state.modal_stack
    );

    // 5. Unbinding <C-A-n> via noop hides Ctrl+Alt+N and disables it in BranchesQuickView.
    let mut unbind_state = state_with(snapshot());
    unbind_state.config.keymap = KeymapConfig::for_preset(KeymapPreset::Default);
    let unbind_ov: KeymapConfigOverlay = toml::from_str(
        r#"
[branch]
prepend_keymap = [
    { on = [ "<C-A-n>" ], run = "noop" },
]
"#,
    )
    .unwrap();
    unbind_state.config.keymap.merge_overlay(&unbind_ov);
    key(&mut unbind_state, KeyCode::Char('b'), KeyModifiers::NONE);
    let term = render(&mut unbind_state, 140, 40);
    let unbind_screen = screen_text(term.backend().buffer());
    assert!(
        !unbind_screen.contains("Ctrl+Alt+N"),
        "unbound Ctrl+Alt+N must not be shown:\n{unbind_screen}"
    );
    key(
        &mut unbind_state,
        KeyCode::Char('n'),
        KeyModifiers::CONTROL | KeyModifiers::ALT,
    );
    assert!(
        !matches!(unbind_state.modal_stack.last(), Some(ModalLayer::Dialog(_))),
        "unbound Ctrl+Alt+N must not open NewBranch dialog: {:?}",
        unbind_state.modal_stack
    );

    // 6. CriticalDialog interior clicks do not dismiss it, while clicking Cancel closes it.
    let mut crit_state = state_with(snapshot());
    crit_state.modal_stack.push(ModalLayer::CriticalDialog {
        title: "Force Push".into(),
        body: "Overwrite remote history?".into(),
        confirm_label: "Force Push".into(),
        cancel_label: "Cancel".into(),
        selected_button: ModalButton::Cancel,
        confirm_effect: None,
    });
    let _ = render(&mut crit_state, 120, 40);
    let crit_body = region_of(&crit_state, |t| matches!(t, HitTarget::DialogBody));
    mouse(
        &mut crit_state,
        MouseEventKind::Down(MouseButton::Left),
        crit_body.x + 1,
        crit_body.y + 1,
    );
    assert!(
        matches!(
            crit_state.modal_stack.last(),
            Some(ModalLayer::CriticalDialog { .. })
        ),
        "clicking inside CriticalDialog must not close it"
    );
    let crit_cancel = region_of(&crit_state, |t| {
        matches!(t, HitTarget::ModalButton(ModalButton::Cancel))
    });
    mouse(
        &mut crit_state,
        MouseEventKind::Down(MouseButton::Left),
        crit_cancel.x + 1,
        crit_cancel.y,
    );
    assert!(
        crit_state.modal_stack.is_empty(),
        "clicking Cancel on CriticalDialog should close it"
    );
}

#[test]
fn commit_editor_focus_bar_and_changes_tree_enter_diff_hint() {
    let mut state = state_with(snapshot());
    state.config.keymap = KeymapConfig::for_preset(KeymapPreset::Default);
    state.dispatch(Action::SwitchTab(WorkspaceTab::Commit));
    state.stage_status = Some(Arc::new(jbgit_core::StageStatusSnapshot {
        unstaged: vec![jbgit_core::StageEntry {
            path: "src/lib.rs".into(),
            old_path: None,
            kind: jbgit_core::FileStatusKind::Modified,
        }],
        ..Default::default()
    }));
    state
        .stage_workbench
        .sync_rows(state.stage_status.as_ref());

    // 1. On a container row (Unstaged group), footer does not show `Enter  Diff`.
    state.dispatch(Action::FocusPane(PaneId::CommitChangesTree));
    state.dispatch(Action::SelectCommitStageRow(0));
    let term = render(&mut state, 140, 40);
    let group_footer = row_text(term.backend().buffer(), 39);
    assert!(
        !group_footer.contains("Enter  Diff"),
        "group row must not advertise Enter Diff: {group_footer}"
    );

    // 2. On a file row with diff panel open, footer advertises `Enter  Diff`.
    let file_idx = state
        .stage_workbench
        .rows
        .iter()
        .position(|r| !r.is_container())
        .expect("file row");
    state.dispatch(Action::SelectCommitStageRow(file_idx));
    let term = render(&mut state, 140, 40);
    let file_footer = row_text(term.backend().buffer(), 39);
    assert!(
        file_footer.contains("Enter  Diff"),
        "file row should advertise Enter Diff: {file_footer}"
    );

    // 3. CommitMessageEditor displays focused accent bar `▏` only when focused.
    let ed_rect = region_of(&state, |t| matches!(t, HitTarget::CommitEditorArea));
    let unfocused_cell = &term.backend().buffer()[(ed_rect.x, ed_rect.y + 1)];
    assert_ne!(unfocused_cell.symbol(), "▏");

    state.dispatch(Action::FocusPane(PaneId::CommitMessageEditor));
    let term = render(&mut state, 140, 40);
    let focused_cell = &term.backend().buffer()[(ed_rect.x, ed_rect.y + 1)];
    assert_eq!(focused_cell.symbol(), "▏");
    assert_eq!(focused_cell.fg, state.theme.border_focused);
}
