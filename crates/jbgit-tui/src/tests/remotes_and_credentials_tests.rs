//! Spec 12 #07 (Configure Remotes) and #08 (askpass credential dialog).

use crossterm::event::{KeyCode, KeyModifiers};
use jbgit_core::{GitFailure, GitOp, GitOpOutcome, RemoteInfo, RepoSnapshot};
use jbgit_engine::EngineEvent;
use jbgit_engine::askpass::AskpassRequest;

use crate::app::{Action, ModalLayer, UiAsyncEvent};
use crate::branches_popup::{PopupRow, build_popup_rows};
use crate::dialogs::Dialog;
use crate::menus::remote_group_menu_items;

use super::{TestSession, find_item, item_run_actions, remote, screen_text, snapshot};

fn remotes_snapshot() -> RepoSnapshot {
    let mut snap = snapshot();
    snap.remotes = vec![
        RemoteInfo {
            name: "origin".into(),
            fetch_url: Some("https://u:secret@example.com/o.git".into()),
            push_url: None,
        },
        RemoteInfo {
            name: "fork".into(),
            fetch_url: Some("https://example.com/f.git".into()),
            push_url: Some("ssh://git@example.com/f.git".into()),
        },
    ];
    snap.refs.push(remote("fork/main", 3));
    snap.refs.push(remote("fork/topic", 4));
    snap
}

fn key(ts: &mut TestSession, c: char) {
    ts.send_key(KeyCode::Char(c), KeyModifiers::NONE);
}

fn clear_input(ts: &mut TestSession) {
    ts.send_key(KeyCode::Char('u'), KeyModifiers::CONTROL);
}

fn top_dialog(ts: &TestSession) -> &Dialog {
    match ts.state().modal_stack.last() {
        Some(ModalLayer::Dialog(d)) => d.as_ref(),
        other => panic!("expected dialog, got {other:?}"),
    }
}

fn remotes_selected(ts: &TestSession) -> usize {
    ts.state()
        .modal_stack
        .iter()
        .find_map(|l| match l {
            ModalLayer::Remotes(rs) => Some(rs.selected),
            _ => None,
        })
        .expect("Remotes modal open")
}

#[test]
fn remotes_modal_lists_masked_urls_and_drives_every_remote_op() {
    let mut ts = TestSession::with_snapshot(remotes_snapshot());

    // <A-r> opens the modal on the current upstream's remote (`origin`).
    ts.send_key(KeyCode::Char('r'), KeyModifiers::ALT);
    assert_eq!(remotes_selected(&ts), 0);
    let text = screen_text(&ts.render(120, 30));
    assert!(text.contains("Configure Remotes"), "{text}");
    assert!(text.contains("https://u:***@example.com/o.git"), "{text}");
    assert!(text.contains("ssh://git@example.com/f.git"), "{text}");
    assert!(text.contains("(same as fetch)"), "{text}");
    assert!(!text.contains("secret"), "{text}");

    // Add: name step rejects duplicates, then asks for the URL.
    key(&mut ts, 'a');
    ts.type_str("origin");
    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);
    assert!(ts.take_git_ops().is_empty());
    let text = screen_text(&ts.render(120, 30));
    assert!(text.contains("Remote 'origin' already exists"), "{text}");
    clear_input(&mut ts);
    ts.type_str("upstream");
    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);
    assert!(matches!(top_dialog(&ts), Dialog::Input(_)));
    ts.type_str("https://h/u x.git");
    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);
    assert!(
        ts.take_git_ops().is_empty(),
        "whitespace in URL is rejected"
    );
    for _ in 0..6 {
        ts.send_key(KeyCode::Backspace, KeyModifiers::NONE);
    }
    ts.type_str(".git");
    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(
        ts.take_git_ops(),
        vec![GitOp::AddRemote {
            name: "upstream".into(),
            url: "https://h/u.git".into(),
        }]
    );
    assert!(matches!(
        ts.state().modal_stack.last(),
        Some(ModalLayer::Remotes(_))
    ));

    // Success offers "Fetch 'upstream'".
    ts.deliver_engine_event(EngineEvent::GitOpFinished(GitOpOutcome::ok(
        GitOp::AddRemote {
            name: "upstream".into(),
            url: "https://h/u.git".into(),
        },
    )));
    ts.dispatch(Action::NotificationActivate);
    assert_eq!(
        ts.take_git_ops(),
        vec![GitOp::FetchRemote {
            name: "upstream".into()
        }]
    );

    // Rename the selected remote (`fork`, second row).
    key(&mut ts, 'j');
    assert_eq!(remotes_selected(&ts), 1);
    key(&mut ts, 'r');
    clear_input(&mut ts);
    ts.type_str("mine");
    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(
        ts.take_git_ops(),
        vec![GitOp::RenameRemote {
            old: "fork".into(),
            new: "mine".into(),
        }]
    );

    // Edit the push URL (pre-filled with the stored value).
    key(&mut ts, 'p');
    match top_dialog(&ts) {
        Dialog::Input(i) => assert_eq!(i.value, "ssh://git@example.com/f.git"),
        other => panic!("{other:?}"),
    }
    clear_input(&mut ts);
    ts.type_str("git@example.com:me/f.git");
    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(
        ts.take_git_ops(),
        vec![GitOp::SetRemoteUrl {
            name: "fork".into(),
            url: "git@example.com:me/f.git".into(),
            push: true,
        }]
    );

    // Remove asks first and names the tracking branches that go away.
    ts.send_key(KeyCode::Delete, KeyModifiers::NONE);
    let text = screen_text(&ts.render(120, 30));
    assert!(text.contains("Remove Remote 'fork'"), "{text}");
    assert!(text.contains("Its 2 remote-tracking branch(es)"), "{text}");
    assert!(ts.take_git_ops().is_empty());
    // Dangerous confirm: focus starts on Cancel.
    ts.send_key(KeyCode::Left, KeyModifiers::NONE);
    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(
        ts.take_git_ops(),
        vec![GitOp::RemoveRemote {
            name: "fork".into()
        }]
    );

    // Fetch just this remote, then close.
    key(&mut ts, 'k');
    key(&mut ts, 'f');
    assert_eq!(
        ts.take_git_ops(),
        vec![GitOp::FetchRemote {
            name: "origin".into()
        }]
    );
    ts.send_key(KeyCode::Esc, KeyModifiers::NONE);
    assert!(ts.state().modal_stack.is_empty());
}

#[test]
fn remotes_entry_points_and_failures() {
    let mut ts = TestSession::with_snapshot(remotes_snapshot());
    let rows = build_popup_rows(ts.state(), "");
    let idx = rows
        .iter()
        .position(|r| *r == PopupRow::ConfigureRemotes)
        .expect("popup row");
    let acts = crate::branches_popup::activate_row(ts.state(), "", idx);
    assert!(matches!(
        acts.as_slice(),
        [Action::PopModal, Action::OpenRemotes]
    ));

    let items = remote_group_menu_items(ts.state(), Some("fork"));
    let fetch = find_item(&items, "Fetch 'fork'");
    assert!(matches!(
        item_run_actions(fetch).as_slice(),
        [Action::RunGitOp(GitOp::FetchRemote { name })] if name == "fork"
    ));
    let configure = find_item(&items, "Configure Remotes…");
    assert!(matches!(
        item_run_actions(configure).as_slice(),
        [Action::OpenRemotes]
    ));
    assert_eq!(remote_group_menu_items(ts.state(), None).len(), 1);

    // Duplicate names from git surface as a notice; auth failures get guidance.
    ts.deliver_engine_event(EngineEvent::GitOpFinished(GitOpOutcome::failed(
        GitOp::AddRemote {
            name: "origin".into(),
            url: "x".into(),
        },
        GitFailure::RemoteAlreadyExists,
    )));
    let notice = ts.state().status_notice.clone().unwrap_or_default();
    assert!(
        notice.contains("Remote 'origin' already exists"),
        "{notice}"
    );

    ts.deliver_engine_event(EngineEvent::GitOpFinished(GitOpOutcome::failed(
        GitOp::FetchRemote {
            name: "origin".into(),
        },
        GitFailure::AuthenticationFailed,
    )));
    let text = screen_text(&ts.render(120, 30));
    assert!(text.contains("Authentication Failed"), "{text}");
    assert!(text.contains("credential.helper"), "{text}");
    assert!(text.contains("ssh-add"), "{text}");
}

#[test]
fn credential_prompts_are_answered_in_fifo_order_and_never_leak() {
    let mut ts = TestSession::with_snapshot(remotes_snapshot());
    let (user_req, mut user_rx) = AskpassRequest::new("Username for 'https://h': ".into());
    let (pass_req, mut pass_rx) = AskpassRequest::new("Password for 'https://alice@h': ".into());
    ts.deliver_ui_async(UiAsyncEvent::Askpass(user_req));
    ts.deliver_ui_async(UiAsyncEvent::Askpass(pass_req));

    // Only the oldest prompt is on screen; usernames are typed in clear.
    let shown = |ts: &TestSession| {
        ts.state()
            .modal_stack
            .iter()
            .filter(|l| matches!(l, ModalLayer::Dialog(d) if matches!(d.as_ref(), Dialog::Credential(_))))
            .count()
    };
    assert_eq!(shown(&ts), 1);
    match top_dialog(&ts) {
        Dialog::Credential(c) => assert!(!c.masked && c.prompt.starts_with("Username")),
        other => panic!("{other:?}"),
    }
    ts.type_str("alice");
    let text = screen_text(&ts.render(100, 30));
    assert!(text.contains("alice"), "{text}");
    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(
        user_rx.try_recv().unwrap().as_deref().map(String::as_str),
        Some("alice")
    );

    // Password prompt: masked, pasteable, Tab reveals, Esc cancels.
    assert_eq!(shown(&ts), 1);
    ts.type_str("s3c");
    ts.dispatch(Action::ModalPaste("ret\n".into()));
    let text = screen_text(&ts.render(100, 30));
    assert!(text.contains("••••••"), "{text}");
    assert!(!text.contains("s3cret"), "{text}");
    assert!(!format!("{:?}", ts.state().modal_stack).contains("s3cret"));
    ts.send_key(KeyCode::Tab, KeyModifiers::NONE);
    let text = screen_text(&ts.render(100, 30));
    assert!(text.contains("s3cret"), "{text}");
    ts.send_key(KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(pass_rx.try_recv().unwrap(), None);
    assert!(ts.state().modal_stack.is_empty());

    // A prompt whose git process gave up disappears on the next tick.
    let (stale, stale_rx) = AskpassRequest::new("Password: ".into());
    ts.deliver_ui_async(UiAsyncEvent::Askpass(stale));
    assert_eq!(shown(&ts), 1);
    drop(stale_rx);
    ts.dispatch(Action::TickSpinner);
    assert_eq!(shown(&ts), 0);
}
