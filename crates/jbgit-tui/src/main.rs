mod app;
mod app_git;
mod app_stage;
mod branches_popup;
pub mod config;
mod dialogs;
mod graph;
mod hit_test;
pub mod i18n;
mod input;
mod input_menus;
mod input_stage;
pub mod keymap;
#[cfg(test)]
mod log_tests;
#[cfg(test)]
mod spec05_tests;
#[cfg(test)]
mod spec09_tests;
mod log_view;
mod menus;
mod stage_view;
mod stash_view;
mod syntax;
mod terminal;
mod theme;
mod views;
mod widgets;

use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{Event, EventStream};
use futures::StreamExt;
use jbgit_engine::{EngineSupervisor, RepoLocation};

use crate::app::{Action, AppState, Effect, execute_effects};
use crate::graph::GraphicsCapabilities;
use crate::input::route_terminal_event;
use crate::terminal::TerminalGuard;
use crate::views::render_app;

#[tokio::main]
async fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let mut target_path = PathBuf::from(".");

    if let Some(arg) = args.next() {
        let cli_i18n = crate::i18n::I18n::load_for_cli();
        if arg == "-h" || arg == "--help" {
            println!("{}", cli_i18n.t("cli.tagline"));
            println!("{}", cli_i18n.t("cli.usage"));
            println!();
            println!("{}", cli_i18n.t("cli.options"));
            println!("{}", cli_i18n.t("cli.opt_help"));
            println!("{}", cli_i18n.t("cli.opt_print_default"));
            println!("{}", cli_i18n.t("cli.opt_init_config"));
            return Ok(());
        }
        if arg == "--print-default-config" {
            let which = args.next();
            crate::config::AppConfig::print_default_config(which.as_deref());
            return Ok(());
        }
        if arg == "--init-config" {
            match crate::config::AppConfig::init_config_detailed(None, false) {
                Ok((dir, files)) => {
                    for (name, created) in files {
                        let path_s = format!("{:?}", dir.join(name));
                        let msg_key = if created {
                            "cli.init_file_generated"
                        } else {
                            "cli.init_file_skipped"
                        };
                        println!("{}", cli_i18n.t_fmt(msg_key, &[("path", &path_s)]));
                    }
                    println!(
                        "{}",
                        cli_i18n.t_fmt("cli.init_ok", &[("dir", &format!("{dir:?}"))])
                    );
                }
                Err(e) => eprintln!(
                    "{}",
                    cli_i18n.t_fmt("cli.init_err", &[("err", &e.to_string())])
                ),
            }
            return Ok(());
        }
        target_path = PathBuf::from(arg);
    }

    let cli_i18n = crate::i18n::I18n::load_for_cli();
    let (engine_handle, mut engine_rx, mut state) = match RepoLocation::discover(&target_path) {
        Ok(loc) => {
            let workdir = loc.workdir.clone();
            match EngineSupervisor::start(loc, true) {
                Ok((handle, rx)) => {
                    let s = AppState::new(workdir, None);
                    (Some(handle), Some(rx), s)
                }
                Err(e) => (
                    None,
                    None,
                    AppState::new(
                        target_path,
                        Some(cli_i18n.t_fmt("cli.engine_err", &[("err", &e.to_string())])),
                    ),
                ),
            }
        }
        Err(e) => (None, None, AppState::new(target_path, Some(e.to_string()))),
    };

    if let Some(handle) = engine_handle.as_ref() {
        let initial_effects = state.dispatch(Action::RefreshAll);
        execute_effects(initial_effects, &mut state, Some(handle));
    }

    let mut term_guard = Some(TerminalGuard::enter()?);
    let mut event_stream = EventStream::new();
    state
        .graph_renderer
        .set_capabilities(GraphicsCapabilities::detect());

    while !state.should_quit {
        if state.needs_redraw {
            if let Some(guard) = term_guard.as_mut() {
                guard.terminal_mut().draw(|frame| {
                    render_app(frame, &mut state);
                })?;
                // Vector graph images (Kitty / Sixel / iTerm2) follow ratatui's flush.
                let _ = state.graph_renderer.write_emission(&mut std::io::stdout());
            }
            state.needs_redraw = false;
        }

        // 30 ms details debounce (CommitDetailsLoader): wake once the latest
        // selection has been stable long enough to fetch its details.
        let details_deadline = state.next_deadline();
        let debounce_sleep = async {
            match details_deadline {
                Some(at) => tokio::time::sleep_until(tokio::time::Instant::from_std(at)).await,
                None => std::future::pending::<()>().await,
            }
        };

        let spinner_sleep = async {
            if state.has_active_tasks() {
                tokio::time::sleep(Duration::from_millis(100)).await;
            } else {
                std::future::pending::<()>().await;
            }
        };

        tokio::select! {
            maybe_term = event_stream.next() => {
                match maybe_term {
                    Some(Ok(ev)) => {
                        if matches!(ev, Event::Resize(..)) {
                            // Font zoom (Ctrl +/-) arrives as a resize: re-calibrate cell pixels.
                            state
                                .graph_renderer
                                .set_capabilities(GraphicsCapabilities::detect());
                        }
                        let actions = route_terminal_event(ev, &mut state);
                        for action in actions {
                            let effects = state.dispatch(action);
                            let refresh_stage = handle_external_editor_effects(&effects, &mut state, &mut term_guard);
                            execute_effects(effects, &mut state, engine_handle.as_ref());
                            if refresh_stage {
                                execute_effects(
                                    vec![Effect::RefreshScope(jbgit_core::RefreshScope::StageStatus)],
                                    &mut state,
                                    engine_handle.as_ref(),
                                );
                            }
                        }
                    }
                    Some(Err(_)) | None => {
                        state.should_quit = true;
                    }
                }
            }
            maybe_engine = recv_engine(&mut engine_rx) => {
                if let Some(ev) = maybe_engine {
                    let effects = state.apply_engine_event(ev);
                    let refresh_stage = handle_external_editor_effects(&effects, &mut state, &mut term_guard);
                    execute_effects(effects, &mut state, engine_handle.as_ref());
                    if refresh_stage {
                        execute_effects(
                            vec![Effect::RefreshScope(jbgit_core::RefreshScope::StageStatus)],
                            &mut state,
                            engine_handle.as_ref(),
                        );
                    }
                } else {
                    engine_rx = None;
                }
            }
            _ = debounce_sleep => {
                let effects = state.dispatch(Action::FlushPendingDetails);
                execute_effects(effects, &mut state, engine_handle.as_ref());
            }
            _ = spinner_sleep => {
                let effects = state.dispatch(Action::TickSpinner);
                execute_effects(effects, &mut state, engine_handle.as_ref());
            }
        }
    }

    let release = state.graph_renderer.shutdown_sequence();
    if !release.is_empty() {
        let _ = std::io::stdout().write_all(&release);
    }
    drop(term_guard);
    Ok(())
}

fn resume_terminal_after_external_cmd(
    term_guard: &mut Option<TerminalGuard>,
    state: &mut AppState,
) {
    *term_guard = TerminalGuard::enter().ok();
    state.graph_renderer.invalidate();
}

fn handle_external_editor_effects(
    effects: &[Effect],
    state: &mut AppState,
    term_guard: &mut Option<TerminalGuard>,
) -> bool {
    let mut refresh_stage = false;
    let editor_bin = || {
        std::env::var("VISUAL")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| std::env::var("EDITOR").ok().filter(|s| !s.trim().is_empty()))
            .unwrap_or_else(|| {
                if cfg!(windows) {
                    "notepad".to_string()
                } else {
                    "vi".to_string()
                }
            })
    };

    for eff in effects {
        match eff {
            Effect::OpenExternalEditorMessage => {
                let tmp_path = std::env::temp_dir().join("jbgit-commit-editmsg.txt");
                if std::fs::write(&tmp_path, state.stage_workbench.editor.text()).is_err() {
                    continue;
                }
                drop(term_guard.take());
                let cmd = editor_bin();
                let status = std::process::Command::new(&cmd).arg(&tmp_path).status();
                resume_terminal_after_external_cmd(term_guard, state);
                if status.is_ok_and(|s| s.success())
                    && let Ok(updated) = std::fs::read_to_string(&tmp_path)
                {
                    let prev = state.stage_workbench.editor.text();
                    if !state.stage_workbench.amend
                        && !prev.trim().is_empty()
                        && prev.trim() != updated.trim()
                    {
                        state.stage_workbench.push_history(&prev);
                    }
                    state.stage_workbench.editor.set_text(&updated);
                    state.stage_workbench.empty_message_error = false;
                    state.stage_workbench.save_draft();
                }
                let _ = std::fs::remove_file(&tmp_path);
                state.needs_redraw = true;
            }
            Effect::OpenExternalEditorFile(rel) => {
                let full = state.repo_path_display.join(rel);
                drop(term_guard.take());
                let cmd = editor_bin();
                let _ = std::process::Command::new(&cmd).arg(&full).status();
                resume_terminal_after_external_cmd(term_guard, state);
                state.needs_redraw = true;
            }
            Effect::OpenExternalMergetool(rel) => {
                drop(term_guard.take());
                let _ = std::process::Command::new("git")
                    .args(["mergetool", "-y", "--", rel])
                    .current_dir(&state.repo_path_display)
                    .status();
                resume_terminal_after_external_cmd(term_guard, state);
                state.needs_redraw = true;
                refresh_stage = true;
            }
            _ => {}
        }
    }
    refresh_stage
}

async fn recv_engine(
    rx: &mut Option<tokio::sync::mpsc::UnboundedReceiver<jbgit_engine::EngineEvent>>,
) -> Option<jbgit_engine::EngineEvent> {
    match rx {
        Some(channel) => channel.recv().await,
        None => std::future::pending().await,
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::{
        Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    };
    use jbgit_core::{PaneId, SplitAxis, SplitterId, WorkspaceTab};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::layout::Rect;

    use crate::app::{Action, AppState};
    use crate::input::route_terminal_event;
    use crate::views::render_app;

    #[test]
    fn test_backend_render_tiny_and_normal_no_panic() {
        let mut state = AppState::new_with_config(
            std::path::PathBuf::from("."),
            None,
            crate::config::AppConfig::load_defaults(),
        );
        for (w, h) in [(10, 4), (45, 12), (59, 15), (120, 40)] {
            let backend = TestBackend::new(w, h);
            let mut terminal = Terminal::new(backend).unwrap();
            terminal.draw(|f| render_app(f, &mut state)).unwrap();
        }
    }

    #[test]
    fn tab_switch_and_focus_ring_and_splitter_drag() {
        let mut state = AppState::new_with_config(
            std::path::PathBuf::from("."),
            None,
            crate::config::AppConfig::load_defaults(),
        );
        let backend = TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| render_app(f, &mut state)).unwrap();

        // Switch tabs with 1..4
        for (ch, expected) in [
            ('1', WorkspaceTab::Commit),
            ('2', WorkspaceTab::Log),
            ('3', WorkspaceTab::Stash),
            ('4', WorkspaceTab::Console),
        ] {
            let actions = route_terminal_event(
                Event::Key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE)),
                &mut state,
            );
            for a in actions {
                state.dispatch(a);
            }
            assert_eq!(state.active_tab, expected);
        }

        // Cycle focus in Log tab
        state.dispatch(Action::SwitchTab(WorkspaceTab::Log));
        assert_eq!(state.focused_pane, PaneId::LogCommitTable);
        // Focus cycle: Branches → Commits → Changes → Details (wide terminal).
        state.dispatch(Action::CycleFocus { reverse: false });
        assert_eq!(state.focused_pane, PaneId::LogChangesTree);
        state.dispatch(Action::CycleFocus { reverse: false });
        assert_eq!(state.focused_pane, PaneId::LogDetailsDiff);
        state.dispatch(Action::CycleFocus { reverse: false });
        assert_eq!(state.focused_pane, PaneId::LogBranchesTree);

        // Render Log tab and simulate dragging LogLeftHorizontal splitter
        terminal.draw(|f| render_app(f, &mut state)).unwrap();
        let (_, container) = state
            .splitter_containers
            .get(&SplitterId::LogLeftHorizontal)
            .copied()
            .unwrap();
        let initial_ratio = state.splitters.ratio(SplitterId::LogLeftHorizontal);
        let split = state.splitters.split(
            container,
            SplitterId::LogLeftHorizontal,
            SplitAxis::Horizontal,
        );
        let div_x = split.divider.x;
        let div_y = split.divider.y + 2;

        // Left down on divider
        let actions = route_terminal_event(
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: div_x,
                row: div_y,
                modifiers: KeyModifiers::NONE,
            }),
            &mut state,
        );
        for a in actions {
            state.dispatch(a);
        }
        assert!(state.drag_state.is_some());

        // Drag right by +12 columns
        route_terminal_event(
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::Drag(MouseButton::Left),
                column: div_x + 12,
                row: div_y,
                modifiers: KeyModifiers::NONE,
            }),
            &mut state,
        );
        let dragged_ratio = state.splitters.ratio(SplitterId::LogLeftHorizontal);
        assert!(dragged_ratio > initial_ratio);

        // Release mouse
        route_terminal_event(
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::Up(MouseButton::Left),
                column: div_x + 12,
                row: div_y,
                modifiers: KeyModifiers::NONE,
            }),
            &mut state,
        );
        assert!(state.drag_state.is_none());

        // Double-click resets splitter ratio (re-render to update divider rect)
        terminal.draw(|f| render_app(f, &mut state)).unwrap();
        let split2 = state.splitters.split(
            container,
            SplitterId::LogLeftHorizontal,
            SplitAxis::Horizontal,
        );
        let dx2 = split2.divider.x;
        for _ in 0..2 {
            let acts = route_terminal_event(
                Event::Mouse(MouseEvent {
                    kind: MouseEventKind::Down(MouseButton::Left),
                    column: dx2,
                    row: div_y,
                    modifiers: KeyModifiers::NONE,
                }),
                &mut state,
            );
            for a in acts {
                state.dispatch(a);
            }
        }
        let reset_ratio = state.splitters.ratio(SplitterId::LogLeftHorizontal);
        assert!((reset_ratio - initial_ratio).abs() < 0.001);
        let _ = Rect::ZERO;
    }

    #[test]
    fn mouse_wheel_routes_to_hovered_pane_and_right_click_preselects() {
        use chrono::Local;
        use jbgit_core::{
            CommitSummary, ConsoleLine, ConsoleStream, GitConsoleEntry, Oid, RepoSnapshot,
            RepositoryState,
        };
        use std::sync::Arc;

        let mut state = AppState::new_with_config(
            std::path::PathBuf::from("."),
            None,
            crate::config::AppConfig::load_defaults(),
        );
        let oid1 = Oid::from_bytes([1u8; 20]);
        let oid2 = Oid::from_bytes([2u8; 20]);
        state.repo_snapshot = Some(Arc::new(RepoSnapshot {
            repo_name: "demo".into(),
            workdir: ".".into(),
            git_dir: ".git".into(),
            common_dir: ".git".into(),
            state: RepositoryState::Clean,
            is_unborn_head: false,
            user_name: None,
            user_email: None,
            recent_branches: vec![],
            stashes: vec![],
            ongoing_onto: None,
            head_branch: Some("main".into()),
            head_oid: Some(oid1),
            refs: vec![],
            commits: vec![
                CommitSummary {
                    oid: oid1,
                    parents: vec![],
                    author_name: "Alice".into(),
                    author_email: "a@b.c".into(),
                    timestamp: 0,
                    subject: "first".into(),
                    author_timestamp: 0,
                    body: String::new(),
                },
                CommitSummary {
                    oid: oid2,
                    parents: vec![oid1],
                    author_name: "Bob".into(),
                    author_email: "b@c.d".into(),
                    timestamp: 1,
                    subject: "second".into(),
                    author_timestamp: 0,
                    body: String::new(),
                },
            ],
        }));
        state.console_entries.push(GitConsoleEntry {
            id: 1,
            started_at: Local::now(),
            duration_ms: Some(5),
            command_line: "git status".into(),
            output_lines: (0..20)
                .map(|i| ConsoleLine {
                    stream: ConsoleStream::Stdout,
                    text: format!("short line {i}"),
                })
                .collect(),
            exit_code: Some(0),
        });

        let backend = TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).unwrap();

        // Non-overflowing output must stay at scroll 0 when wheeling down.
        state.dispatch(Action::SwitchTab(WorkspaceTab::Console));
        assert_eq!(state.focused_pane, PaneId::ConsoleCommandList);
        terminal.draw(|f| render_app(f, &mut state)).unwrap();
        for a in route_terminal_event(
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::ScrollDown,
                column: 90,
                row: 10,
                modifiers: KeyModifiers::NONE,
            }),
            &mut state,
        ) {
            state.dispatch(a);
        }
        assert_eq!(state.console_output_scroll, 0);

        // Expand output so it overflows viewport; wheel over ConsoleOutputDetail scrolls while focus stays on list.
        state.console_entries[0].output_lines = (0..60)
            .map(|i| ConsoleLine {
                stream: ConsoleStream::Stdout,
                text: format!("line {i}"),
            })
            .collect();
        terminal.draw(|f| render_app(f, &mut state)).unwrap();
        let actions = route_terminal_event(
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::ScrollDown,
                column: 90,
                row: 10,
                modifiers: KeyModifiers::NONE,
            }),
            &mut state,
        );
        for a in actions {
            state.dispatch(a);
        }
        assert!(state.console_output_scroll > 0);
        assert_eq!(state.focused_pane, PaneId::ConsoleCommandList);

        // In Log tab, right-clicking row 1 pre-selects row 1 AND opens ContextMenu
        state.dispatch(Action::SwitchTab(WorkspaceTab::Log));
        terminal.draw(|f| render_app(f, &mut state)).unwrap();
        assert_eq!(state.log_graph.selected_idx, 0);

        // Row 0 is at y=4 (header=0, tabs=1, pane_header=2, filter_bar=3, row0=4, row1=5)
        let actions = route_terminal_event(
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Right),
                column: 40,
                row: 5,
                modifiers: KeyModifiers::NONE,
            }),
            &mut state,
        );
        for a in actions {
            state.dispatch(a);
        }
        assert_eq!(state.log_graph.selected_idx, 1);
        assert_eq!(state.modal_stack.len(), 1);

        // Re-render with ContextMenu open and verify clicking outside closes menu without selecting row 0
        terminal.draw(|f| render_app(f, &mut state)).unwrap();
        let actions = route_terminal_event(
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 5,
                row: 3,
                modifiers: KeyModifiers::NONE,
            }),
            &mut state,
        );
        for a in actions {
            state.dispatch(a);
        }
        assert!(state.modal_stack.is_empty());
        assert_eq!(state.log_graph.selected_idx, 1);
    }

    #[test]
    fn stale_epoch_events_are_silently_dropped_on_ui_arrival() {
        use jbgit_core::{
            FileStatusKind, Oid, RepoSnapshot, RepositoryState, StageEntry, StageStatusSnapshot,
        };
        use jbgit_engine::EngineEvent;
        use std::sync::Arc;

        let mut state = AppState::new_with_config(
            std::path::PathBuf::from("."),
            None,
            crate::config::AppConfig::load_defaults(),
        );
        state.latest_repo_epoch = 5;
        state.latest_stage_epoch = 4;

        let stale_snap = Arc::new(RepoSnapshot {
            repo_name: "stale".into(),
            workdir: ".".into(),
            git_dir: ".git".into(),
            common_dir: ".git".into(),
            state: RepositoryState::Clean,
            is_unborn_head: true,
            user_name: None,
            user_email: None,
            recent_branches: vec![],
            stashes: vec![],
            ongoing_onto: None,
            head_branch: Some("old".into()),
            head_oid: None,
            refs: vec![],
            commits: vec![],
        });
        state.apply_engine_event(EngineEvent::RepoSnapshotLoaded {
            snapshot: stale_snap,
            epoch: 3,
        });
        assert!(state.repo_snapshot.is_none());

        let fresh_snap = Arc::new(RepoSnapshot {
            repo_name: "fresh".into(),
            workdir: ".".into(),
            git_dir: ".git".into(),
            common_dir: ".git".into(),
            state: RepositoryState::Clean,
            is_unborn_head: false,
            user_name: None,
            user_email: None,
            recent_branches: vec![],
            stashes: vec![],
            ongoing_onto: None,
            head_branch: Some("main".into()),
            head_oid: Some(Oid::ZERO),
            refs: vec![],
            commits: vec![],
        });
        state.apply_engine_event(EngineEvent::RepoSnapshotLoaded {
            snapshot: fresh_snap,
            epoch: 5,
        });
        assert_eq!(
            state.repo_snapshot.as_ref().map(|s| s.repo_name.as_str()),
            Some("fresh")
        );

        let stale_stage = Arc::new(StageStatusSnapshot {
            unstaged: vec![StageEntry {
                path: "stale.rs".into(),
                old_path: None,
                kind: FileStatusKind::Modified,
            }],
            ..Default::default()
        });
        state.apply_engine_event(EngineEvent::StageStatusLoaded {
            status: stale_stage,
            epoch: 2,
        });
        assert!(state.stage_status.is_none());
    }

    #[test]
    fn cjk_truncate_respects_display_width() {
        use crate::views::truncate_to_width;
        use unicode_width::UnicodeWidthStr;

        let s = "修复中文提交信息对齐问题";
        for max_w in 0..=12 {
            let out = truncate_to_width(s, max_w);
            assert!(
                out.width() <= max_w,
                "truncated '{out}' width {} exceeded {max_w}",
                out.width()
            );
        }
        assert_eq!(truncate_to_width(s, 5), "修复…");
    }

    #[test]
    fn context_menu_cascading_submenu_mnemonics_hover_and_no_stacking() {
        use crate::app::{ContextMenuAction, ContextMenuEntryKind, ContextMenuItem, ModalLayer};
        use jbgit_core::{FileStatusKind, StageEntry, StageStatusSnapshot};
        use std::sync::Arc;

        let mut state = AppState::new_with_config(
            std::path::PathBuf::from("."),
            None,
            crate::config::AppConfig::load_defaults(),
        );
        state.stage_status = Some(Arc::new(StageStatusSnapshot {
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
            ..Default::default()
        }));
        state.dispatch(Action::SwitchTab(WorkspaceTab::Commit));

        let backend = TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| render_app(f, &mut state)).unwrap();

        // Right-click b.rs in Commit changes tree (y=6: header=0, tabs=1, pane_header=2, toolbar=3, group=4, a.rs=5, b.rs=6)
        let actions = route_terminal_event(
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Right),
                column: 10,
                row: 6,
                modifiers: KeyModifiers::NONE,
            }),
            &mut state,
        );
        for a in actions {
            state.dispatch(a);
        }
        assert_eq!(state.commit_stage_selected_idx, 2);
        assert_eq!(state.modal_stack.len(), 1);

        // Render menu, then hover second item (menu anchored at row 6 -> border at 6, item 0 at 7, item 1 at 8)
        terminal.draw(|f| render_app(f, &mut state)).unwrap();
        route_terminal_event(
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::Moved,
                column: 14,
                row: 8,
                modifiers: KeyModifiers::NONE,
            }),
            &mut state,
        );
        assert!(matches!(
            state.modal_stack.last(),
            Some(ModalLayer::ContextMenu { selected: 1, .. })
        ));

        // Right-clicking on a MenuItem must not stack a duplicate ContextMenu
        let dup_actions = route_terminal_event(
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Right),
                column: 14,
                row: 8,
                modifiers: KeyModifiers::NONE,
            }),
            &mut state,
        );
        assert!(dup_actions.is_empty());

        // Mnemonic 'p' opens the cascading "Git Probes" submenu (depth 1)
        let sub_actions = route_terminal_event(
            Event::Key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::NONE)),
            &mut state,
        );
        for a in sub_actions {
            state.dispatch(a);
        }
        assert_eq!(state.modal_stack.len(), 2);
        assert!(matches!(
            state.modal_stack.last(),
            Some(ModalLayer::ContextMenu { depth: 1, .. })
        ));

        // Mnemonic 's' inside submenu closes both ContextMenu layers and emits RunCliProbe
        let run_actions = route_terminal_event(
            Event::Key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE)),
            &mut state,
        );
        let mut effects = Vec::new();
        for a in run_actions {
            effects.extend(state.dispatch(a));
        }
        assert!(state.modal_stack.is_empty());
        assert_eq!(effects.len(), 1);
        let _ = (
            ContextMenuAction::RefreshRepo,
            ContextMenuEntryKind::Submenu(vec![]),
            ContextMenuItem::action("x", None, None, ContextMenuAction::RefreshRepo),
        );
    }

    #[test]
    fn index_lock_retry_replays_blocked_cli_command() {
        use crate::app::Effect;
        use jbgit_core::RefreshScope;
        use jbgit_engine::EngineEvent;

        let mut state = AppState::new_with_config(
            std::path::PathBuf::from("."),
            None,
            crate::config::AppConfig::load_defaults(),
        );
        state.apply_engine_event(EngineEvent::IndexLockDetected {
            lock_path: ".git/index.lock".into(),
            message: "Unable to create '.git/index.lock': File exists.".into(),
            retry_args: vec!["add".into(), "--pathspec-from-file=-".into()],
            retry_stdin: Some(b"src/lib.rs\0".to_vec()),
            retry_is_read_only: false,
            retry_refresh_after: RefreshScope::StageStatus,
        });
        assert_eq!(state.modal_stack.len(), 1);

        let actions = route_terminal_event(
            Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            &mut state,
        );
        let mut effects = Vec::new();
        for a in actions {
            effects.extend(state.dispatch(a));
        }
        assert!(state.modal_stack.is_empty());
        assert_eq!(effects.len(), 1);
        match &effects[0] {
            Effect::RunCli {
                args,
                stdin_payload,
                is_read_only,
                refresh_after,
            } => {
                assert_eq!(
                    args,
                    &vec!["add".to_string(), "--pathspec-from-file=-".to_string()]
                );
                assert_eq!(stdin_payload.as_deref(), Some(b"src/lib.rs\0".as_slice()));
                assert!(!is_read_only);
                assert_eq!(*refresh_after, RefreshScope::StageStatus);
            }
            other => panic!("expected RunCli retry effect, got {other:?}"),
        }
    }

    #[test]
    fn detached_head_and_overflow_hover_tooltips_and_overflow_clicks() {
        use jbgit_core::{CommitSummary, Oid, RefInfo, RefKind, RepoSnapshot, RepositoryState};
        use std::sync::Arc;

        let mut state = AppState::new_with_config(
            std::path::PathBuf::from("."),
            None,
            crate::config::AppConfig::load_defaults(),
        );
        let oid = Oid::from_bytes([7u8; 20]);
        state.repo_snapshot = Some(Arc::new(RepoSnapshot {
            repo_name: "demo".into(),
            workdir: ".".into(),
            git_dir: ".git".into(),
            common_dir: ".git".into(),
            state: RepositoryState::Clean,
            is_unborn_head: false,
            user_name: None,
            user_email: None,
            recent_branches: vec![],
            stashes: vec![],
            ongoing_onto: None,
            head_branch: None,
            head_oid: Some(oid),
            refs: vec![
                RefInfo {
                    name: "alpha".into(),
                    full_name: "refs/heads/alpha".into(),
                    target_oid: oid,
                    kind: RefKind::LocalBranch,
                    upstream: None,
                    ahead_behind: None,
                    is_head: false,
                },
                RefInfo {
                    name: "beta".into(),
                    full_name: "refs/heads/beta".into(),
                    target_oid: oid,
                    kind: RefKind::LocalBranch,
                    upstream: None,
                    ahead_behind: None,
                    is_head: false,
                },
                RefInfo {
                    name: "gamma".into(),
                    full_name: "refs/heads/gamma".into(),
                    target_oid: oid,
                    kind: RefKind::LocalBranch,
                    upstream: None,
                    ahead_behind: None,
                    is_head: false,
                },
                RefInfo {
                    name: "v1.0.0".into(),
                    full_name: "refs/tags/v1.0.0".into(),
                    target_oid: oid,
                    kind: RefKind::Tag,
                    upstream: None,
                    ahead_behind: None,
                    is_head: false,
                },
                RefInfo {
                    name: "v1.0.1".into(),
                    full_name: "refs/tags/v1.0.1".into(),
                    target_oid: oid,
                    kind: RefKind::Tag,
                    upstream: None,
                    ahead_behind: None,
                    is_head: false,
                },
            ],
            commits: vec![CommitSummary {
                oid,
                parents: vec![],
                author_name: "Alice".into(),
                author_email: "a@b.c".into(),
                timestamp: 0,
                subject: "detached commit with many refs".into(),
                author_timestamp: 0,
                body: String::new(),
            }],
        }));

        state.i18n = Arc::new(crate::i18n::I18n::load("en-US", None));
        let backend = TestBackend::new(100, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| render_app(f, &mut state)).unwrap();

        let row0_text = |buf: &ratatui::buffer::Buffer, y: u16| -> String {
            (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect()
        };
        use unicode_width::UnicodeWidthStr;
        let row_line = row0_text(terminal.backend().buffer(), 4);
        let col_of = |needle: &str| {
            let byte_idx = row_line.find(needle).unwrap();
            row_line[..byte_idx].width() as u16
        };
        let head_x = col_of("HEAD");
        let plus_x = col_of("+");

        // Hover Detached HEAD pill -> shows Detached HEAD tooltip
        route_terminal_event(
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::Moved,
                column: head_x,
                row: 4,
                modifiers: KeyModifiers::NONE,
            }),
            &mut state,
        );
        terminal.draw(|f| render_app(f, &mut state)).unwrap();
        let full_text: String = (0..24)
            .map(|y| row0_text(terminal.backend().buffer(), y))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(full_text.contains("Detached HEAD"), "{full_text}");

        // Hover +N overflow pill -> lists all refs on commit
        route_terminal_event(
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::Moved,
                column: plus_x,
                row: 4,
                modifiers: KeyModifiers::NONE,
            }),
            &mut state,
        );
        terminal.draw(|f| render_app(f, &mut state)).unwrap();
        let full_text: String = (0..24)
            .map(|y| row0_text(terminal.backend().buffer(), y))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(full_text.contains("All Commit References"), "{full_text}");
        assert!(full_text.contains("v1.0.1"), "{full_text}");

        // Left-click +N does NOT open context menu; right-click DOES open it.
        let left_acts = route_terminal_event(
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: plus_x,
                row: 4,
                modifiers: KeyModifiers::NONE,
            }),
            &mut state,
        );
        for a in left_acts {
            state.dispatch(a);
        }
        assert!(state.modal_stack.is_empty());

        let right_acts = route_terminal_event(
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Right),
                column: plus_x,
                row: 4,
                modifiers: KeyModifiers::NONE,
            }),
            &mut state,
        );
        for a in right_acts {
            state.dispatch(a);
        }
        assert_eq!(state.modal_stack.len(), 1);
    }

    #[test]
    fn long_edge_arrow_hover_click_jump_center_and_alt_left_return() {
        use jbgit_core::{CommitSummary, Oid, RefInfo, RefKind, RepoSnapshot, RepositoryState};
        use std::sync::Arc;

        let oid_n = |n: u64| {
            let mut bytes = [0u8; 20];
            bytes[..8].copy_from_slice(&n.to_le_bytes());
            Oid::from_bytes(bytes)
        };

        // 42 commits: commit 0 merges [1, 41], forming a 41-row long edge (0 -> 41)
        let mut commits = Vec::new();
        for i in 0..=41u64 {
            let parents = if i == 0 {
                vec![oid_n(1), oid_n(41)]
            } else if i < 41 {
                vec![oid_n(i + 1)]
            } else {
                vec![]
            };
            commits.push(CommitSummary {
                oid: oid_n(i),
                parents,
                author_name: "Alice".into(),
                author_email: "a@b.c".into(),
                timestamp: (1000 - i) as i64,
                subject: format!("commit {i}"),
                author_timestamp: 0,
                body: String::new(),
            });
        }

        let mut state = AppState::new_with_config(
            std::path::PathBuf::from("."),
            None,
            crate::config::AppConfig::load_defaults(),
        );
        state.repo_snapshot = Some(Arc::new(RepoSnapshot {
            repo_name: "demo".into(),
            workdir: ".".into(),
            git_dir: ".git".into(),
            common_dir: ".git".into(),
            state: RepositoryState::Clean,
            is_unborn_head: false,
            user_name: None,
            user_email: None,
            recent_branches: vec![],
            stashes: vec![],
            ongoing_onto: None,
            head_branch: Some("main".into()),
            head_oid: Some(oid_n(0)),
            refs: vec![RefInfo {
                name: "main".into(),
                full_name: "refs/heads/main".into(),
                target_oid: oid_n(0),
                kind: RefKind::LocalBranch,
                upstream: None,
                ahead_behind: None,
                is_head: true,
            }],
            commits,
        }));

        let backend = TestBackend::new(120, 20);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| render_app(f, &mut state)).unwrap();

        let row_text = |buf: &ratatui::buffer::Buffer, y: u16| -> String {
            (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect()
        };
        use unicode_width::UnicodeWidthStr;
        // Row 1 of commit table is y=5 (header, tabs, pane_header, filter_bar, row0=4, row1=5)
        let row1 = row_text(terminal.backend().buffer(), 5);
        let arrow_byte = row1
            .find('▾')
            .expect("expected long-edge down arrow on row 1");
        let arrow_x = row1[..arrow_byte].width() as u16;

        // Hover over ▾ arrow -> shows target short hash & subject in footer status bar
        route_terminal_event(
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::Moved,
                column: arrow_x,
                row: 5,
                modifiers: KeyModifiers::NONE,
            }),
            &mut state,
        );
        terminal.draw(|f| render_app(f, &mut state)).unwrap();
        let status_line = row_text(terminal.backend().buffer(), 19);
        assert!(status_line.contains(&oid_n(41).short()), "{status_line}");
        assert!(status_line.contains("commit 41"), "{status_line}");

        // Left-click ▾ arrow -> jumps to row 41 (target parent commit)
        let acts = route_terminal_event(
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: arrow_x,
                row: 5,
                modifiers: KeyModifiers::NONE,
            }),
            &mut state,
        );
        for a in acts {
            state.dispatch(a);
        }
        assert_eq!(state.log_graph.selected_idx, 41);
        assert!(state.log_graph.scroll_offset > 0);

        // Alt+Left returns back to row 0
        let back_acts = route_terminal_event(
            Event::Key(KeyEvent::new(KeyCode::Left, KeyModifiers::ALT)),
            &mut state,
        );
        for a in back_acts {
            state.dispatch(a);
        }
        assert_eq!(state.log_graph.selected_idx, 0);
    }

    #[test]
    fn stage_workbench_dual_presence_visual_select_and_amend_draft_restore() {
        use jbgit_core::{
            AmendCommitPreview, DiffContext, FileChange, FileStatusKind, StageEntry,
            StageStatusSnapshot,
        };
        use jbgit_engine::{EngineEvent, StageDiffKey, StageDiffSide, ThreeDocStageDiff};
        use std::sync::Arc;

        let mut state = AppState::new_with_config(
            std::path::PathBuf::from("."),
            None,
            crate::config::AppConfig::load_defaults(),
        );
        state.stage_status = Some(Arc::new(StageStatusSnapshot {
            staged: vec![StageEntry {
                path: "src/foo.rs".into(),
                old_path: None,
                kind: FileStatusKind::Added,
            }],
            unstaged: vec![StageEntry {
                path: "src/foo.rs".into(),
                old_path: None,
                kind: FileStatusKind::Modified,
            }],
            ..Default::default()
        }));
        state.dispatch(Action::SwitchTab(WorkspaceTab::Commit));
        state.dispatch(Action::StageToggleGroupByDir); // flat mode for predictable row indices

        // Dual presence: Staged (0), src/foo.rs (1), Unstaged (2), src/foo.rs (3)
        assert_eq!(state.stage_workbench.rows.len(), 4);

        // Selecting Staged/src/foo.rs targets StageDiffSide::Staged
        state.dispatch(Action::SelectCommitStageRow(1));
        assert_eq!(
            state
                .stage_workbench
                .current_diff_key
                .as_ref()
                .map(|k| k.side),
            Some(StageDiffSide::Staged)
        );

        // Selecting Unstaged/src/foo.rs targets StageDiffSide::Unstaged
        state.dispatch(Action::SelectCommitStageRow(3));
        assert_eq!(
            state
                .stage_workbench
                .current_diff_key
                .as_ref()
                .map(|k| k.side),
            Some(StageDiffSide::Unstaged)
        );

        // Feed ThreeDocStageDiff and test visual selection partial staging effect
        let vcs = b"fn a() {}\n";
        let staged = b"fn a() {}\nfn staged_line() {}\n";
        let local = b"fn a() {}\nfn staged_line() {}\nfn local_1() {}\nfn local_2() {}\n";
        let tracker = jbgit_core::StageLineTracker::new(vcs, staged, local);
        let (hunks, is_truncated) = tracker.build_interactive_hunks(false, DiffContext::Three, 20_000);
        let diff = Arc::new(ThreeDocStageDiff {
            key: StageDiffKey {
                path: "src/foo.rs".into(),
                old_path: None,
                side: StageDiffSide::Unstaged,
                context: DiffContext::Three,
            },
            vcs_exists: true,
            staged_exists: true,
            local_exists: true,
            vcs_bytes: vcs.to_vec(),
            staged_bytes: staged.to_vec(),
            local_bytes: local.to_vec(),
            file_mode: 0o100644,
            is_binary: false,
            is_conflicted: false,
            tracker,
            hunks,
            is_truncated,
        });
        state.apply_engine_event(EngineEvent::StageFileDiffLoaded { diff, epoch: 1 });

        // Focus diff, move to first Added line (`fn local_1() {}`), toggle visual mode, stage only that line!
        state.dispatch(Action::FocusPane(PaneId::CommitDiffPreview));
        // Row 0 = Header, Row 1 = Context `fn a()`, Row 2 = Context `fn staged_line()`, Row 3 = Added `fn local_1()`
        state.dispatch(Action::StageDiffMoveCursor(3));
        state.dispatch(Action::StageDiffToggleVisual);
        let effs = state.dispatch(Action::StageDiffStageSelection);
        match effs.into_iter().find(|e| matches!(e, crate::app::Effect::ApplyStageBuffer { .. })) {
            Some(crate::app::Effect::ApplyStageBuffer { new_staged_bytes, .. }) => {
                assert_eq!(
                    new_staged_bytes,
                    b"fn a() {}\nfn staged_line() {}\nfn local_1() {}\n"
                );
            }
            other => panic!("expected ApplyStageBuffer, got {other:?}"),
        }

        // Test non-modal Amend draft preservation + virtual tree node
        state.dispatch(Action::FocusPane(PaneId::CommitMessageEditor));
        state.dispatch(Action::CommitEditorPaste("wip: my draft".into()));
        state.apply_engine_event(EngineEvent::AmendPreviewLoaded {
            preview: Some(AmendCommitPreview {
                short_sha: "a8f3d21".into(),
                message: "feat: previous commit".into(),
                files: vec![FileChange {
                    path: "prev.rs".into(),
                    old_path: None,
                    kind: FileStatusKind::Modified,
                    additions: 1,
                    deletions: 0,
                    is_binary: false,
                    exists_in_workdir: true,
                }],
            }),
        });

        state.dispatch(Action::CommitToggleAmend);
        assert!(state.stage_workbench.amend);
        assert_eq!(state.stage_workbench.editor.text(), "feat: previous commit");
        assert!(
            state
                .stage_workbench
                .rows
                .iter()
                .any(|r| r.label.contains("Head Commit to Amend (a8f3d21)"))
        );

        // Render full Commit tab to verify no panic and Amend node visible
        let backend = TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| render_app(f, &mut state)).unwrap();

        // Untoggling Amend restores original draft!
        state.dispatch(Action::CommitToggleAmend);
        assert!(!state.stage_workbench.amend);
        assert_eq!(state.stage_workbench.editor.text(), "wip: my draft");
    }

    #[test]
    fn stage_review_fixes_amend_diff_gutter_click_draft_persistence_and_push_confirm() {
        use jbgit_core::{
            AmendCommitPreview, DiffContext, FileChange, FileStatusKind, StageTreeRowKind,
        };
        use jbgit_engine::{
            CommitOutcome, EngineEvent, StageDiffKey, StageDiffSide, ThreeDocStageDiff,
        };
        use std::sync::Arc;

        let mut state = AppState::new_with_config(
            std::path::PathBuf::from("."),
            None,
            crate::config::AppConfig::load_defaults(),
        );
        state.stage_status = Some(Arc::new(jbgit_core::StageStatusSnapshot::default()));
        let tmp_dir = std::env::temp_dir().join(format!("jbgit-test-draft-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&tmp_dir);
        let draft_file = tmp_dir.join("jbgit-commit-draft");
        let _ = std::fs::remove_file(&draft_file);
        state.stage_workbench.load_draft(draft_file.clone());
        state.dispatch(Action::SwitchTab(WorkspaceTab::Commit));

        // 1. Editing commit message persists draft to disk and reloads into fresh state
        state.dispatch(Action::FocusPane(PaneId::CommitMessageEditor));
        state.dispatch(Action::CommitEditorPaste("draft: unsent note".into()));
        assert!(state.commit_history_candidates().iter().any(|c| c == "draft: unsent note"));
        let mut reloaded = AppState::new(std::path::PathBuf::from("."), None);
        reloaded.stage_workbench.load_draft(draft_file.clone());
        assert_eq!(reloaded.stage_workbench.editor.text(), "draft: unsent note");
        let _ = std::fs::remove_dir_all(&tmp_dir);

        // 2. Amend toggle always emits RequestAmendPreview & AmendFile selects StageDiffSide::AmendCommit
        state.apply_engine_event(EngineEvent::AmendPreviewLoaded {
            preview: Some(AmendCommitPreview {
                short_sha: "1122334".into(),
                message: "feat: head commit".into(),
                files: vec![FileChange {
                    path: "prev.rs".into(),
                    old_path: None,
                    kind: FileStatusKind::Modified,
                    additions: 1,
                    deletions: 1,
                    is_binary: false,
                    exists_in_workdir: true,
                }],
            }),
        });
        let amend_effs = state.dispatch(Action::CommitToggleAmend);
        assert!(
            amend_effs
                .iter()
                .any(|e| matches!(e, crate::app::Effect::RequestAmendPreview))
        );
        let amend_file_idx = state
            .stage_workbench
            .rows
            .iter()
            .position(|r| matches!(r.kind, StageTreeRowKind::AmendFile { .. }))
            .expect("AmendFile row");
        state.dispatch(Action::SelectCommitStageRow(amend_file_idx));
        assert_eq!(
            state
                .stage_workbench
                .current_diff_key
                .as_ref()
                .map(|k| k.side),
            Some(StageDiffSide::AmendCommit)
        );

        // Stale amend preview refreshes editor when a newer HEAD preview arrives
        state.apply_engine_event(EngineEvent::AmendPreviewLoaded {
            preview: Some(AmendCommitPreview {
                short_sha: "5566778".into(),
                message: "feat: newer head commit".into(),
                files: vec![],
            }),
        });
        assert_eq!(state.stage_workbench.editor.text(), "feat: newer head commit");
        state.dispatch(Action::CommitToggleAmend); // turn amend back off

        // 3. Mixed 3-doc diff: gutter click opens ThreeWayInspector with word-level diff & ]c doesn't cycle context
        state.stage_status = Some(Arc::new(jbgit_core::StageStatusSnapshot {
            staged: vec![jbgit_core::StageEntry {
                path: "src/lib.rs".into(),
                old_path: None,
                kind: FileStatusKind::Modified,
            }],
            unstaged: vec![jbgit_core::StageEntry {
                path: "src/lib.rs".into(),
                old_path: None,
                kind: FileStatusKind::Modified,
            }],
            ..Default::default()
        }));
        state.dispatch(Action::StageToggleGroupByDir);
        state.dispatch(Action::SelectCommitStageRow(3)); // Unstaged/src/lib.rs
        let vcs = b"let x = old_val;\n";
        let staged = b"let x = staged_val;\n";
        let local = b"let x = local_val;\n";
        let tracker = jbgit_core::StageLineTracker::new(vcs, staged, local);
        let (hunks, is_truncated) = tracker.build_interactive_hunks(false, DiffContext::Three, 20_000);
        state.stage_workbench.current_diff_key = Some(StageDiffKey {
            path: "src/lib.rs".into(),
            old_path: None,
            side: StageDiffSide::Unstaged,
            context: DiffContext::Three,
        });
        state.apply_engine_event(EngineEvent::StageFileDiffLoaded {
            diff: Arc::new(ThreeDocStageDiff {
                key: StageDiffKey {
                    path: "src/lib.rs".into(),
                    old_path: None,
                    side: StageDiffSide::Unstaged,
                    context: DiffContext::Three,
                },
                vcs_exists: true,
                staged_exists: true,
                local_exists: true,
                vcs_bytes: vcs.to_vec(),
                staged_bytes: staged.to_vec(),
                local_bytes: local.to_vec(),
                file_mode: 0o100644,
                is_binary: false,
                is_conflicted: false,
                tracker,
                hunks,
                is_truncated,
            }),
            epoch: 1,
        });

        let backend = TestBackend::new(120, 36);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| render_app(f, &mut state)).unwrap();

        // Find the `▚` mixed gutter cell in the rendered buffer and click it
        let buf = terminal.backend().buffer();
        let mut gutter_pos = None;
        for y in 0..36 {
            for x in 0..120 {
                if buf[(x, y)].symbol() == "▚" {
                    gutter_pos = Some((x, y));
                    break;
                }
            }
            if gutter_pos.is_some() {
                break;
            }
        }
        let (gx, gy) = gutter_pos.expect("mixed 3-way gutter bar rendered");
        let click_acts = route_terminal_event(
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: gx,
                row: gy,
                modifiers: KeyModifiers::NONE,
            }),
            &mut state,
        );
        for a in click_acts {
            state.dispatch(a);
        }
        assert!(matches!(
            state.modal_stack.last(),
            Some(crate::app::ModalLayer::FloatingPopup {
                kind: crate::app::PopupKind::ThreeWayInspector,
                ..
            })
        ));
        terminal.draw(|f| render_app(f, &mut state)).unwrap();
        state.dispatch(Action::PopModal);
        assert!(state.stage_workbench.three_way_range_idx.is_none());

        // `]c` jumps hunk without changing diff_context
        state.dispatch(Action::FocusPane(PaneId::CommitDiffPreview));
        for ch in [']', 'c'] {
            for a in route_terminal_event(
                Event::Key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE)),
                &mut state,
            ) {
                state.dispatch(a);
            }
        }
        assert_eq!(state.stage_workbench.diff_context, DiffContext::Three);

        // 4. CommitCompleted with and_push opens Push confirmation dialog instead of blind push
        let commit_effs = state.apply_engine_event(EngineEvent::CommitCompleted(CommitOutcome {
            success: true,
            commit_oid: None,
            short_sha: Some("abc1234".into()),
            summary: "Committed abc1234".into(),
            error_output: None,
            and_push: true,
            preserved_message: None,
        }));
        assert!(!commit_effs.iter().any(|e| matches!(
            e,
            crate::app::Effect::RunCli { args, .. } if args.first().map(String::as_str) == Some("push")
        )));
        assert!(matches!(
            state.modal_stack.last(),
            Some(crate::app::ModalLayer::CriticalDialog { title, .. })
                if title == state.i18n.t("dialog.push.confirm_title")
        ));
    }

    #[test]
    fn vertical_layout_ratios_render_drag_and_reset() {
        use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
        use jbgit_core::{
            Oid, RepoSnapshot, RepositoryState, SplitAxis, SplitterId, StashEntry, WorkspaceTab,
        };
        use std::sync::Arc;

        let temp = tempfile::tempdir().unwrap();
        let cfg_dir = temp.path().join(".jbgit");
        std::fs::create_dir_all(&cfg_dir).unwrap();
        std::fs::write(
            cfg_dir.join("jbgit.toml"),
            r#"
[layout]
log_details_ratio = [1, 3]
log_diff_right_ratio = [3, 1]
log_diff_bottom_ratio = [2, 3]
commit_left_ratio = [3, 1]
stash_preview_ratio = [1, 1]
"#,
        )
        .unwrap();

        let mut state = AppState::new(temp.path().to_path_buf(), None);
        state.repo_snapshot = Some(Arc::new(RepoSnapshot {
            repo_name: "demo".into(),
            workdir: temp.path().to_path_buf(),
            git_dir: temp.path().join(".git"),
            common_dir: temp.path().join(".git"),
            state: RepositoryState::Clean,
            is_unborn_head: false,
            user_name: None,
            user_email: None,
            recent_branches: vec![],
            stashes: vec![StashEntry {
                index: 0,
                oid: Oid::from_bytes([2u8; 20]),
                message: "WIP on main: test".into(),
                timestamp: 1_700_000_000,
            }],
            ongoing_onto: None,
            head_branch: Some("main".into()),
            head_oid: Some(Oid::from_bytes([1u8; 20])),
            refs: vec![],
            commits: vec![],
        }));

        assert!((state.splitters.ratio(SplitterId::LogDetailsVertical) - 0.25).abs() < 1e-4);
        assert!((state.splitters.ratio(SplitterId::LogDiffRightHorizontal) - 0.75).abs() < 1e-4);
        assert!((state.splitters.ratio(SplitterId::LogDiffBottomVertical) - 0.40).abs() < 1e-4);
        assert!((state.splitters.ratio(SplitterId::CommitLeftVertical) - 0.75).abs() < 1e-4);
        assert!((state.splitters.ratio(SplitterId::StashPreviewVertical) - 0.50).abs() < 1e-4);

        let backend = TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).unwrap();

        let cases = [
            (
                WorkspaceTab::Log,
                None,
                SplitterId::LogDetailsVertical,
                SplitAxis::Vertical,
                0.25f32,
                6i16,
            ),
            (
                WorkspaceTab::Log,
                Some(crate::log_view::DiffPreviewDock::Right),
                SplitterId::LogDiffRightHorizontal,
                SplitAxis::Horizontal,
                0.75f32,
                -15i16,
            ),
            (
                WorkspaceTab::Log,
                Some(crate::log_view::DiffPreviewDock::Bottom),
                SplitterId::LogDiffBottomVertical,
                SplitAxis::Vertical,
                0.40f32,
                6i16,
            ),
            (
                WorkspaceTab::Commit,
                None,
                SplitterId::CommitLeftVertical,
                SplitAxis::Vertical,
                0.75f32,
                -6i16,
            ),
            (
                WorkspaceTab::Stash,
                None,
                SplitterId::StashPreviewVertical,
                SplitAxis::Vertical,
                0.50f32,
                5i16,
            ),
        ];

        for (tab, dock, splitter_id, expected_axis, expected_default, delta) in cases {
            state.dispatch(Action::SwitchTab(tab));
            if let Some(d) = dock {
                state.details.diff_preview = true;
                state.details.diff_dock = d;
            } else {
                state.details.diff_preview = false;
            }
            terminal.draw(|f| render_app(f, &mut state)).unwrap();

            let (axis, container) = state
                .splitter_containers
                .get(&splitter_id)
                .copied()
                .unwrap_or_else(|| panic!("splitter {splitter_id:?} not registered on {tab:?}"));
            assert_eq!(axis, expected_axis);

            let split = state.splitters.split(container, splitter_id, expected_axis);
            assert!(!split.collapsed);
            let (div_x, div_y, target_x, target_y) = match expected_axis {
                SplitAxis::Vertical => (
                    split.divider.x + 2,
                    split.divider.y,
                    split.divider.x + 2,
                    (split.divider.y as i16 + delta) as u16,
                ),
                SplitAxis::Horizontal => (
                    split.divider.x,
                    split.divider.y + 2,
                    (split.divider.x as i16 + delta) as u16,
                    split.divider.y + 2,
                ),
            };

            for a in route_terminal_event(
                Event::Mouse(MouseEvent {
                    kind: MouseEventKind::Down(MouseButton::Left),
                    column: div_x,
                    row: div_y,
                    modifiers: KeyModifiers::NONE,
                }),
                &mut state,
            ) {
                state.dispatch(a);
            }
            assert!(state.drag_state.is_some());

            route_terminal_event(
                Event::Mouse(MouseEvent {
                    kind: MouseEventKind::Drag(MouseButton::Left),
                    column: target_x,
                    row: target_y,
                    modifiers: KeyModifiers::NONE,
                }),
                &mut state,
            );
            let dragged = state.splitters.ratio(splitter_id);
            if delta > 0 {
                assert!(dragged > expected_default);
            } else {
                assert!(dragged < expected_default);
            }

            route_terminal_event(
                Event::Mouse(MouseEvent {
                    kind: MouseEventKind::Up(MouseButton::Left),
                    column: target_x,
                    row: target_y,
                    modifiers: KeyModifiers::NONE,
                }),
                &mut state,
            );

            // Re-render and double-click divider to reset back to configured default ratio
            terminal.draw(|f| render_app(f, &mut state)).unwrap();
            let split2 = state.splitters.split(container, splitter_id, expected_axis);
            let (reset_x, reset_y) = match expected_axis {
                SplitAxis::Vertical => (split2.divider.x + 2, split2.divider.y),
                SplitAxis::Horizontal => (split2.divider.x, split2.divider.y + 2),
            };
            for _ in 0..2 {
                for a in route_terminal_event(
                    Event::Mouse(MouseEvent {
                        kind: MouseEventKind::Down(MouseButton::Left),
                        column: reset_x,
                        row: reset_y,
                        modifiers: KeyModifiers::NONE,
                    }),
                    &mut state,
                ) {
                    state.dispatch(a);
                }
            }
            assert!(
                (state.splitters.ratio(splitter_id) - expected_default).abs() < 1e-4,
                "reset for {splitter_id:?} expected {expected_default}, got {}",
                state.splitters.ratio(splitter_id)
            );
        }
    }

    #[test]
    fn console_overscroll_clamps_reverses_and_resizes_cleanly() {
        use std::path::PathBuf;
        use chrono::Local;
        use jbgit_core::{ConsoleLine, ConsoleStream, GitConsoleEntry};

        let mut state = AppState::new_with_config(
            PathBuf::from("."),
            None,
            crate::config::AppConfig::load_defaults(),
        );
        state.console_entries.push(GitConsoleEntry {
            id: 1,
            started_at: Local::now(),
            duration_ms: Some(12),
            command_line: "git pull".into(),
            output_lines: (0..120)
                .map(|i| ConsoleLine {
                    stream: ConsoleStream::Stdout,
                    text: format!("create mode 100644 file_{i}.rs"),
                })
                .collect(),
            exit_code: Some(0),
        });
        state.dispatch(Action::SwitchTab(WorkspaceTab::Console));

        let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
        terminal.draw(|f| render_app(f, &mut state)).unwrap();
        let bound = state.scroll_metrics.console_output.expect("console bound recorded");
        assert!(bound.max_scroll > 0);

        // Scroll down 100 times past bottom: must stay clamped at max_scroll.
        for _ in 0..100 {
            state.dispatch(Action::ScrollConsoleOutput(3));
        }
        assert_eq!(state.console_output_scroll, bound.max_scroll);

        // First upward scroll must respond immediately with zero dead-zone.
        state.dispatch(Action::ScrollConsoleOutput(-3));
        assert_eq!(state.console_output_scroll, bound.max_scroll.saturating_sub(3));

        // Scroll back to bottom, then resize taller and re-render: scroll must clamp down.
        state.dispatch(Action::ScrollConsoleOutput(isize::MAX / 2));
        assert_eq!(state.console_output_scroll, bound.max_scroll);
        terminal.backend_mut().resize(120, 50);
        terminal.draw(|f| render_app(f, &mut state)).unwrap();
        let taller_bound = state.scroll_metrics.console_output.expect("taller bound recorded");
        assert!(taller_bound.max_scroll < bound.max_scroll);
        assert_eq!(state.console_output_scroll, taller_bound.max_scroll);
    }

    #[test]
    fn log_diff_double_goto_bottom_no_overflow_and_reverses_immediately() {
        use std::path::PathBuf;
        use std::sync::Arc;
        use jbgit_core::Oid;

        let mut state = AppState::new_with_config(
            PathBuf::from("."),
            None,
            crate::config::AppConfig::load_defaults(),
        );
        let oid = Oid::from_bytes([1u8; 20]);
        let changes = vec![jbgit_core::FileChange {
            path: "lib.rs".into(),
            old_path: None,
            kind: jbgit_core::FileStatusKind::Modified,
            additions: 60,
            deletions: 0,
            is_binary: false,
            exists_in_workdir: true,
        }];
        let details = jbgit_core::CommitDetails {
            oid,
            parents: vec![],
            author: jbgit_core::PersonStamp {
                name: "Dev".into(),
                email: "dev@example.com".into(),
                time: 1_700_000_000,
                offset_minutes: 0,
            },
            committer: jbgit_core::PersonStamp {
                name: "Dev".into(),
                email: "dev@example.com".into(),
                time: 1_700_000_000,
                offset_minutes: 0,
            },
            message: "feat: sample".into(),
            signature: jbgit_core::SignatureStatus::Unsigned,
        };
        let key = jbgit_core::DetailsKey::single(oid);
        state.details.cache.insert(
            key.clone(),
            Arc::new(jbgit_core::CommitChangesBundle {
                key: key.clone(),
                details: vec![details],
                changes,
            }),
        );
        let diff_key = jbgit_core::FileDiffKey {
            details: key.clone(),
            path: "lib.rs".into(),
            old_path: None,
            context: state.details.diff_context,
        };
        let lines: Vec<jbgit_core::DiffLine> = (0..80)
            .map(|i| jbgit_core::DiffLine {
                kind: jbgit_core::DiffLineKind::Added,
                old_lineno: None,
                new_lineno: Some(i + 1),
                text: format!("let v{i} = {i};"),
                word_ranges: vec![],
            })
            .collect();
        state.details.diff_cache.insert(
            diff_key.clone(),
            Arc::new(jbgit_core::FileDiff {
                key: diff_key,
                is_binary: false,
                is_truncated: false,
                hunks: vec![jbgit_core::DiffHunk {
                    header: "@@ -0,0 +1,80 @@".into(),
                    lines,
                }],
            }),
        );
        state.details.current_key = Some(key);
        state.details.diff_preview = true;
        state.active_tab = WorkspaceTab::Log;
        state.focused_pane = PaneId::LogDetailsDiff;

        let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
        terminal.draw(|f| render_app(f, &mut state)).unwrap();
        let bound = state.scroll_metrics.log_diff.expect("log_diff bound recorded");
        assert!(bound.max_scroll > 0);

        // Trigger GotoBottom (`isize::MAX / 2`) twice: must not overflow and must clamp to max_scroll.
        state.dispatch(Action::ScrollDiff(isize::MAX / 2));
        state.dispatch(Action::ScrollDiff(isize::MAX / 2));
        assert_eq!(state.details.diff_scroll, bound.max_scroll);

        // Immediately scrolling up 1 line must take effect on the very first action.
        state.dispatch(Action::ScrollDiff(-1));
        assert_eq!(state.details.diff_scroll, bound.max_scroll - 1);
    }

    #[test]
    fn console_output_detail_navigation_keys_work() {
        use std::path::PathBuf;
        use chrono::Local;
        use jbgit_core::{ConsoleLine, ConsoleStream, GitConsoleEntry};

        let mut state = AppState::new_with_config(
            PathBuf::from("."),
            None,
            crate::config::AppConfig::load_defaults(),
        );
        state.console_entries.push(GitConsoleEntry {
            id: 1,
            started_at: Local::now(),
            duration_ms: Some(10),
            command_line: "git log".into(),
            output_lines: (0..100)
                .map(|i| ConsoleLine {
                    stream: ConsoleStream::Stdout,
                    text: format!("commit {i}"),
                })
                .collect(),
            exit_code: Some(0),
        });
        state.dispatch(Action::SwitchTab(WorkspaceTab::Console));
        state.dispatch(Action::FocusPane(PaneId::ConsoleOutputDetail));

        let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
        terminal.draw(|f| render_app(f, &mut state)).unwrap();
        let bound = state.scroll_metrics.console_output.expect("console bound recorded");
        assert!(bound.viewport_h > 10);
        assert!(bound.max_scroll > bound.viewport_h);

        let send_key = |state: &mut AppState, code: KeyCode, modifiers: KeyModifiers| {
            let actions = route_terminal_event(
                Event::Key(KeyEvent::new(code, modifiers)),
                state,
            );
            for a in actions {
                state.dispatch(a);
            }
        };

        // PageDown uses viewport_h - 1
        send_key(&mut state, KeyCode::PageDown, KeyModifiers::NONE);
        let expected_page = bound.viewport_h.saturating_sub(1);
        assert_eq!(state.console_output_scroll, expected_page);

        // PageUp brings back to 0
        send_key(&mut state, KeyCode::PageUp, KeyModifiers::NONE);
        assert_eq!(state.console_output_scroll, 0);

        // Ctrl+d / Ctrl+u move half a page
        send_key(&mut state, KeyCode::Char('d'), KeyModifiers::CONTROL);
        let expected_half = (bound.viewport_h / 2).max(1);
        assert_eq!(state.console_output_scroll, expected_half);
        send_key(&mut state, KeyCode::Char('u'), KeyModifiers::CONTROL);
        assert_eq!(state.console_output_scroll, 0);

        // `G` jumps to bottom, `gg` jumps to top
        send_key(&mut state, KeyCode::Char('G'), KeyModifiers::SHIFT);
        assert_eq!(state.console_output_scroll, bound.max_scroll);
        send_key(&mut state, KeyCode::Char('g'), KeyModifiers::NONE);
        send_key(&mut state, KeyCode::Char('g'), KeyModifiers::NONE);
        assert_eq!(state.console_output_scroll, 0);

        // `End` jumps to bottom, `Home` jumps to top
        send_key(&mut state, KeyCode::End, KeyModifiers::NONE);
        assert_eq!(state.console_output_scroll, bound.max_scroll);
        send_key(&mut state, KeyCode::Home, KeyModifiers::NONE);
        assert_eq!(state.console_output_scroll, 0);

        // j / k scroll line by line in ConsoleOutputDetail
        send_key(&mut state, KeyCode::Char('j'), KeyModifiers::NONE);
        assert_eq!(state.console_output_scroll, 1);
        send_key(&mut state, KeyCode::Char('k'), KeyModifiers::NONE);
        assert_eq!(state.console_output_scroll, 0);

        // `gg` routes via `[global]` dynamic keymap (works even when `g b` is unbound, and respects `noop` override).
        let unbind_gb: crate::config::KeymapConfigOverlay =
            toml::from_str("[global]\nprepend_keymap = [{ on = [\"g\", \"b\"], run = \"noop\" }]\n")
                .unwrap();
        state.config.keymap.merge_overlay(&unbind_gb);
        send_key(&mut state, KeyCode::End, KeyModifiers::NONE);
        assert_eq!(state.console_output_scroll, bound.max_scroll);
        send_key(&mut state, KeyCode::Char('g'), KeyModifiers::NONE);
        send_key(&mut state, KeyCode::Char('g'), KeyModifiers::NONE);
        assert_eq!(state.console_output_scroll, 0);

        let unbind_gg: crate::config::KeymapConfigOverlay =
            toml::from_str("[global]\nprepend_keymap = [{ on = [\"g\", \"g\"], run = \"noop\" }]\n")
                .unwrap();
        state.config.keymap.merge_overlay(&unbind_gg);
        send_key(&mut state, KeyCode::End, KeyModifiers::NONE);
        assert_eq!(state.console_output_scroll, bound.max_scroll);
        send_key(&mut state, KeyCode::Char('g'), KeyModifiers::NONE);
        send_key(&mut state, KeyCode::Char('g'), KeyModifiers::NONE);
        assert_eq!(state.console_output_scroll, bound.max_scroll);
    }
}
