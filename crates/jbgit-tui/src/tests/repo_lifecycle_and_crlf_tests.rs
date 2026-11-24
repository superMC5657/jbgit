use std::path::PathBuf;
use std::sync::Arc;

use crossterm::event::{Event, KeyCode, KeyModifiers};
use jbgit_core::WorkspaceTab;
use jbgit_engine::{CommitRequest, EngineEvent, GitExecutableInfo, TaskRequest};
use ratatui::Terminal;
use ratatui::backend::TestBackend;

use crate::app::{Action, ModalLayer, PopupKind, UiAsyncEvent};
use crate::config::{AppConfig, JbgitConfigOverlay, KeymapConfig, KeymapPreset, KeymapScope};
use crate::dialogs::Dialog;
use crate::i18n::I18n;
use crate::keymap::{KeyAction, execute_action};
use crate::repo_lifecycle::{CloneDialogAction, CloneField};
use crate::session::{RepoSession, SessionInput, TestHost, init_engine_and_session};

use super::{TestSession, screen_text, snapshot};

#[tokio::test]
async fn non_git_dir_opens_welcome_init_and_rebinds_to_workbench() {
    let tmp = tempfile::TempDir::new().unwrap();
    let empty_dir = tmp.path().join("fresh_workspace");
    std::fs::create_dir_all(&empty_dir).unwrap();

    let (repo, mut session) = init_engine_and_session(empty_dir.clone(), None);
    session.state_mut().i18n = Arc::new(I18n::load("en-US", None));
    assert!(repo.handle.is_none(), "non-Git dir starts detached");
    assert!(
        matches!(session.state().modal_stack.last(), Some(ModalLayer::Welcome(_))),
        "expected Welcome modal in non-Git directory, got {:?}",
        session.state().modal_stack.last()
    );

    // Render Welcome modal and verify all 3 entries are visible.
    let mut terminal = Terminal::new(TestBackend::new(110, 30)).unwrap();
    terminal.draw(|f| session.render(f)).unwrap();
    let welcome_screen = screen_text(terminal.backend().buffer());
    assert!(welcome_screen.contains("Welcome to jbgit"), "{welcome_screen}");
    assert!(welcome_screen.contains("Initialize Git Repository"), "{welcome_screen}");
    assert!(welcome_screen.contains("Clone Repository"), "{welcome_screen}");
    assert!(welcome_screen.contains("Open Recent"), "{welcome_screen}");

    // Press Enter on the default selection (Initialize Git Repository).
    let mut host = TestHost::new();
    session.step_turn(
        [SessionInput::Terminal(Event::Key(
            crossterm::event::KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        ))],
        session.now(),
        None,
        &mut host,
    );
    let init_dir = session
        .take_init_repo_request()
        .expect("Enter on Welcome item 0 should queue init_repo request");
    assert!(jbgit_core::same_path(&init_dir, &empty_dir));

    // Execute real git init and rebind the session into the new repository.
    let loc = jbgit_engine::init_repository_sync(&init_dir, None).expect("git init");
    let (next_repo, mut next_state) = RepoSession::bind_location(loc, None);
    next_state.i18n = Arc::new(I18n::load("en-US", None));
    assert!(next_repo.handle.is_some(), "bound repo must start engine");
    assert!(next_state.git_info.is_some(), "git_info must be populated");

    session.rebind(next_state, true);
    assert!(
        session.state().modal_stack.is_empty(),
        "Welcome modal must be gone after rebind"
    );
    terminal.draw(|f| session.render(f)).unwrap();
    let workbench_screen = screen_text(terminal.backend().buffer());
    assert!(
        !workbench_screen.contains("Welcome to jbgit"),
        "expected main workbench after rebind, got:\n{workbench_screen}"
    );

    let _ = next_repo.unbind().await;
}

#[test]
fn clone_dialog_accessible_from_switcher_and_catalog_infers_dir_and_streams_progress() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mut snap = snapshot();
    snap.workdir = tmp.path().join("existing-repo");
    std::fs::create_dir_all(&snap.workdir).unwrap();
    let mut ts = TestSession::with_snapshot(snap);

    // 1. Open via CommandCatalog (KeyAction::CloneRepository)
    let catalog_actions = execute_action(&KeyAction::CloneRepository, ts.state())
        .expect("CloneRepository must be executable from CommandCatalog");
    ts.dispatch_many(catalog_actions);
    assert!(matches!(
        ts.state().modal_stack.last(),
        Some(ModalLayer::CloneRepo(_))
    ));
    ts.send_key(KeyCode::Esc, KeyModifiers::NONE);
    assert!(ts.state().modal_stack.is_empty());

    // 2. Open via RepoSwitcher -> Ctrl+L
    ts.dispatch(Action::OpenRepoSwitcher);
    assert!(matches!(
        ts.state().modal_stack.last(),
        Some(ModalLayer::RepoSwitcher(_))
    ));
    ts.send_key(KeyCode::Char('l'), KeyModifiers::CONTROL);
    assert!(matches!(
        ts.state().modal_stack.last(),
        Some(ModalLayer::CloneRepo(_))
    ));

    // 3. Paste remote URL -> target_dir auto-infers directory name
    ts.send_event(Event::Paste(
        "https://github.com/acme/awesome-project.git".into(),
    ));
    let expected_dest = tmp.path().join("awesome-project");
    match ts.state().modal_stack.last() {
        Some(ModalLayer::CloneRepo(cd)) => {
            assert_eq!(cd.url, "https://github.com/acme/awesome-project.git");
            assert_eq!(
                PathBuf::from(&cd.target_dir),
                expected_dest,
                "expected auto-derived target_dir under parent"
            );
        }
        other => panic!("expected CloneRepo modal, got {other:?}"),
    }

    // 4. Set depth = 1 and toggle --recurse-submodules, then submit
    ts.dispatch(Action::CloneRepoUpdate(CloneDialogAction::SetFocus(
        CloneField::Depth,
    )));
    ts.type_str("1");
    ts.dispatch(Action::CloneRepoUpdate(CloneDialogAction::SetFocus(
        CloneField::RecurseSubmodules,
    )));
    ts.send_key(KeyCode::Char(' '), KeyModifiers::NONE);
    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);

    let clone_req = ts
        .session
        .take_clone_repo_request()
        .expect("Enter on valid CloneRepo form should produce CloneRequest");
    assert_eq!(clone_req.url, "https://github.com/acme/awesome-project.git");
    assert_eq!(clone_req.target_dir, expected_dest);
    assert_eq!(clone_req.depth, Some(1));
    assert!(clone_req.recurse_submodules);

    // 5. Stream progress lines and verify they render live in the modal
    ts.deliver_ui_async(UiAsyncEvent::CloneProgress(
        "Receiving objects: 50% (10/20)".into(),
    ));
    let buf = ts.render(100, 30);
    let txt = screen_text(&buf);
    assert!(txt.contains("Cloning repository"), "{txt}");
    assert!(txt.contains("Receiving objects: 50%"), "{txt}");

    // Verify zh-CN CJK button widths render completely without clipping
    // (ratatui TestBackend stores a trailing space cell after each 2-column CJK glyph)
    ts.state_mut().i18n = Arc::new(I18n::load("zh-CN", None));
    let zh_buf = ts.render(100, 30);
    let zh_txt = screen_text(&zh_buf);
    let compact: String = zh_txt.chars().filter(|c| !c.is_whitespace()).collect();
    assert!(compact.contains("[克隆]"), "CJK submit button clipped:\n{zh_txt}");
    assert!(compact.contains("[取消]"), "CJK cancel button clipped:\n{zh_txt}");
    ts.state_mut().i18n = Arc::new(I18n::load("en-US", None));

    // 6. Deliver CloneFinished(Ok(dest)) -> closes modal and triggers pending_repo_switch
    ts.deliver_ui_async(UiAsyncEvent::CloneFinished(Ok(expected_dest.clone())));
    assert!(ts.state().modal_stack.is_empty());
    assert_eq!(
        ts.session.take_repo_switch_request().as_ref(),
        Some(&expected_dest)
    );
}

#[test]
fn crlf_warning_dialog_supports_fix_as_is_and_dont_warn_again_persistence() {
    let tmp = tempfile::TempDir::new().unwrap();
    let workdir = tmp.path().to_path_buf();
    let mut snap = snapshot();
    snap.workdir = workdir.clone();
    let mut ts = TestSession::with_snapshot(snap);

    let req = CommitRequest {
        message: "feat: windows file".into(),
        check_crlf: true,
        fix_autocrlf: false,
        ..Default::default()
    };

    // Choice 1: Fix & Commit (`f`) -> dispatches ExecuteCommit with fix_autocrlf=true, check_crlf=false
    ts.deliver_engine_event(EngineEvent::CommitCrlfWarning {
        request: req.clone(),
        files: vec!["win.txt".into(), "sub/cfg.ini".into()],
    });
    assert!(matches!(
        ts.state().modal_stack.last(),
        Some(ModalLayer::Dialog(d)) if matches!(d.as_ref(), Dialog::Choice(_))
    ));
    let buf = ts.render(110, 30);
    let txt = screen_text(&buf);
    assert!(txt.contains("CRLF Line Separators Detected"), "{txt}");
    assert!(txt.contains("win.txt"), "{txt}");
    assert!(txt.contains("Fix & Commit"), "{txt}");
    assert!(txt.contains("Commit As Is"), "{txt}");
    assert!(txt.contains("Don't warn again"), "{txt}");

    let _ = ts.take_requests();
    ts.send_key(KeyCode::Char('f'), KeyModifiers::NONE);
    let reqs = ts.take_requests();
    let exec = reqs
        .iter()
        .find_map(|(r, _)| match r {
            TaskRequest::ExecuteCommit { request, .. } => Some(request),
            _ => None,
        })
        .expect("expected ExecuteCommit after Fix & Commit");
    assert!(exec.fix_autocrlf);
    assert!(!exec.check_crlf);

    // Choice 2: Commit As Is (`c`) -> fix_autocrlf=false, check_crlf=false, warn_crlf stays true
    ts.deliver_engine_event(EngineEvent::CommitCrlfWarning {
        request: req.clone(),
        files: vec!["win.txt".into()],
    });
    let _ = ts.take_requests();
    ts.send_key(KeyCode::Char('c'), KeyModifiers::NONE);
    let reqs = ts.take_requests();
    let exec = reqs
        .iter()
        .find_map(|(r, _)| match r {
            TaskRequest::ExecuteCommit { request, .. } => Some(request),
            _ => None,
        })
        .expect("expected ExecuteCommit after Commit As Is");
    assert!(!exec.fix_autocrlf);
    assert!(!exec.check_crlf);
    assert!(ts.state().config.jbgit.commit.warn_crlf);

    // Choice 3: Don't warn again (`d`) -> persists [commit] warn_crlf = false to .jbgit/jbgit.toml
    ts.deliver_engine_event(EngineEvent::CommitCrlfWarning {
        request: req,
        files: vec!["win.txt".into()],
    });
    let _ = ts.take_requests();
    ts.send_key(KeyCode::Char('d'), KeyModifiers::NONE);
    let reqs = ts.take_requests();
    let exec = reqs
        .iter()
        .find_map(|(r, _)| match r {
            TaskRequest::ExecuteCommit { request, .. } => Some(request),
            _ => None,
        })
        .expect("expected ExecuteCommit after Don't warn again");
    assert!(!exec.fix_autocrlf);
    assert!(!exec.check_crlf);
    assert!(!ts.state().config.jbgit.commit.warn_crlf);

    let persisted_file = workdir.join(".jbgit").join("jbgit.toml");
    assert!(persisted_file.is_file(), "expected .jbgit/jbgit.toml to be written");
    let reloaded = AppConfig::load(Some(&workdir));
    assert!(
        !reloaded.jbgit.commit.warn_crlf,
        "reloaded repo config must have commit.warn_crlf == false"
    );
}

#[test]
fn git_executable_config_and_display_in_console_and_help_popup() {
    let mut cfg = AppConfig::load_defaults();
    assert_eq!(cfg.jbgit.git.configured_executable(), None);
    assert!(cfg.jbgit.commit.warn_crlf);

    let ov: JbgitConfigOverlay = toml::from_str(
        r#"
[git]
executable = "/custom/bin/git"

[commit]
warn_crlf = false
"#,
    )
    .unwrap();
    cfg.jbgit.merge_overlay(&ov);
    assert_eq!(cfg.jbgit.git.configured_executable(), Some("/custom/bin/git"));
    assert!(!cfg.jbgit.commit.warn_crlf);

    let mut ts = TestSession::with_snapshot(snapshot());
    ts.state_mut().git_info = Some(GitExecutableInfo {
        path: PathBuf::from("/opt/git/bin/git"),
        version_string: "git version 2.47.0".into(),
    });

    // Console tab startup summary shows resolved git version & path when no command selected
    ts.dispatch(Action::SwitchTab(WorkspaceTab::Console));
    let console_buf = ts.render(120, 24);
    let console_txt = screen_text(&console_buf);
    assert!(
        console_txt.contains("git version 2.47.0") && console_txt.contains("/opt/git/bin/git"),
        "expected git info in Console startup summary:\n{console_txt}"
    );

    // ShortcutsHelp (`?`) popup also displays resolved git version & path
    ts.dispatch(Action::OpenPopup {
        kind: PopupKind::ShortcutsHelp,
        initial_query: String::new(),
    });
    let help_buf = ts.render(120, 32);
    let help_txt = screen_text(&help_buf);
    assert!(
        help_txt.contains("git version 2.47.0") && help_txt.contains("/opt/git/bin/git"),
        "expected git info in ShortcutsHelp popup:\n{help_txt}"
    );
}

#[test]
fn keymap_presets_have_no_conflicts_with_clone_repository() {
    for preset in [
        KeymapPreset::Default,
        KeymapPreset::Jetbrains,
        KeymapPreset::Vim,
    ] {
        let km = KeymapConfig::for_preset(preset.clone());
        for scope in [
            KeymapScope::Global,
            KeymapScope::Log,
            KeymapScope::Branch,
            KeymapScope::Commit,
            KeymapScope::Diff,
            KeymapScope::Stash,
            KeymapScope::Console,
        ] {
            let bindings = km.bindings_for(scope);
            let mut seen = std::collections::HashSet::new();
            for b in bindings {
                assert!(
                    seen.insert(b.on.clone()),
                    "duplicate key sequence {:?} in preset {:?} scope {:?}",
                    b.on,
                    preset,
                    scope
                );
            }
        }
    }
    assert_eq!(KeyAction::parse("clone_repository"), KeyAction::CloneRepository);
    assert_eq!(KeyAction::CloneRepository.as_str(), "clone_repository");
}
